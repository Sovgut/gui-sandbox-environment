//! The desktop side of launching: holds the jail launcher's connection and
//! relays `run` requests over it.
//!
//! Requests travel as a 4-byte big-endian length followed by the command line,
//! NUL-separated. The launcher answers with one line, `ok <unit>` or
//! `error <text>`, which is all that ever comes back from the jail. A newer
//! launcher connection replaces an older one; a jail process posing as the
//! launcher can only drop or misreport requests.

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const MAX_REQUEST: u64 = 64 * 1024;
const MAX_REPLY: u64 = 4096;
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

fn listen(path: &Path, mode: u32) -> Result<UnixListener> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(listener)
}

/// Starts relaying in the background: the launcher connects on
/// `launcher_socket` (group `jail`), `run` on `run_socket` (only `friend`).
pub fn spawn(launcher_socket: &Path, run_socket: &Path) -> Result<()> {
    let launchers = listen(launcher_socket, 0o660)?;
    let requests = listen(run_socket, 0o600)?;
    let launcher: Arc<Mutex<Option<UnixStream>>> = Arc::default();

    let slot = Arc::clone(&launcher);
    thread::spawn(move || {
        for stream in launchers.incoming().flatten() {
            stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok();
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stream);
        }
    });
    thread::spawn(move || {
        for mut client in requests.incoming().flatten() {
            let reply = relay(&launcher, &mut client).unwrap_or_else(|e| format!("error {e:#}"));
            client.write_all(format!("{reply}\n").as_bytes()).ok();
        }
    });
    Ok(())
}

fn relay(launcher: &Mutex<Option<UnixStream>>, client: &mut UnixStream) -> Result<String> {
    let mut request = Vec::new();
    (&mut *client).take(MAX_REQUEST).read_to_end(&mut request)?;
    let mut slot = launcher
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(stream) = slot.as_mut() else {
        bail!("the jail launcher is not connected");
    };
    let exchange = |stream: &mut UnixStream| -> Result<String> {
        let len = u32::try_from(request.len())?;
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(&request)?;
        let mut reply = String::new();
        BufReader::new(stream.take(MAX_REPLY)).read_line(&mut reply)?;
        if reply.is_empty() {
            bail!("the jail launcher hung up");
        }
        Ok(reply.trim_end().to_owned())
    };
    match exchange(stream) {
        Ok(reply) => Ok(reply),
        Err(e) => {
            *slot = None;
            Err(e)
        }
    }
}

/// Hands `command` to the desktop and reports what the jail answered.
pub fn request(run_socket: &Path, command: &[String]) -> Result<()> {
    let mut stream = UnixStream::connect(run_socket)
        .with_context(|| format!("connecting to the desktop at {}", run_socket.display()))?;
    stream.write_all(command.join("\0").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    stream.take(MAX_REPLY).read_to_string(&mut reply)?;
    let reply = reply.trim();
    match reply.strip_prefix("ok ") {
        Some(unit) => {
            println!("started as jail: {unit}");
            Ok(())
        }
        None => bail!("{}", reply.strip_prefix("error ").unwrap_or(reply)),
    }
}
