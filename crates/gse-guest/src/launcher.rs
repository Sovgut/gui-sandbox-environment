//! The environment side of launching: connects to the desktop and starts what it is
//! asked for as transient units of the `guest` account's systemd instance, so a
//! program gets that account's environment (the restricted display, its own X
//! server) and can be tracked and stopped like any unit.

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::broker::MAX_REQUEST;

/// Connects whenever the desktop is there, and again after it restarts.
pub fn serve(socket: &Path) -> Result<()> {
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => {
                eprintln!("connected to the desktop");
                if let Err(e) = serve_connection(stream) {
                    eprintln!("desktop connection ended: {e:#}");
                }
            }
            Err(_) => sleep(Duration::from_secs(2)),
        }
    }
}

fn serve_connection(mut stream: UnixStream) -> Result<()> {
    loop {
        let mut len = [0_u8; 4];
        match stream.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let len = u64::from(u32::from_be_bytes(len));
        if len > MAX_REQUEST {
            bail!("request of {len} bytes");
        }
        let mut request = Vec::new();
        (&mut stream).take(len).read_to_end(&mut request)?;
        let reply = match start(&request) {
            Ok(unit) => format!("ok {unit}\n"),
            Err(e) => format!("error {e:#}\n"),
        };
        stream.write_all(reply.as_bytes())?;
    }
}

fn start(request: &[u8]) -> Result<String> {
    let argv: Vec<String> = request
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8(a.to_vec()))
        .collect::<Result<_, _>>()
        .context("the command line is not UTF-8")?;
    if argv.is_empty() {
        bail!("empty command line");
    }
    let unit = format!(
        "guest-app-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let status = Command::new("systemd-run")
        .args(["--user", "--collect", "--quiet", "--unit", &unit, "--"])
        .args(&argv)
        .status()
        .context("starting systemd-run")?;
    if !status.success() {
        bail!("systemd-run failed ({status})");
    }
    Ok(unit)
}
