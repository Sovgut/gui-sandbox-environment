//! The environment network on the host: the nftables table and the passt process.

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gse_host::net::{self, NET_USER, Passt};
use tracing::info;

/// UID of the account passt runs as. It comes from
/// `dist/sysusers.d/gse.conf`.
pub fn net_uid() -> Result<u32> {
    let output = Command::new("id").args(["-u", NET_USER]).output()?;
    if !output.status.success() {
        bail!(
            "no '{NET_USER}' account; install dist/sysusers.d/gse.conf into \
             /etc/sysusers.d and run systemd-sysusers"
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("unreadable uid")
}

fn nft(script: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        info!("dry run: nft -f -\n{script}");
        return Ok(());
    }
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .context("starting nft")?;
    child
        .stdin
        .take()
        .context("nft stdin")?
        .write_all(script.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("nft rejected the ruleset ({status})");
    }
    Ok(())
}

/// Installs or replaces the environment network rules.
pub fn apply(internet: bool, dry_run: bool) -> Result<()> {
    nft(&net::ruleset(net_uid()?, internet), dry_run)?;
    info!(
        "environment network: internet {}",
        if internet { "on" } else { "off" }
    );
    Ok(())
}

pub fn remove(dry_run: bool) -> Result<()> {
    nft(&net::remove_ruleset(), dry_run)
}

pub fn print_rules() -> Result<()> {
    print!("{}", net::ruleset(net_uid()?, true));
    Ok(())
}

/// Where passt's sockets live. SELinux lets `passt_t` create sockets in
/// `tmp_t` directories but not in `/run` (`var_run_t`). passt binds the socket
/// as root, before dropping to the network account, and is denied
/// `dac_override`, so the directory is root's; mode 0700 keeps other users
/// from planting a socket QEMU would connect to.
pub const SOCKET_DIR: &str = "/tmp/gse-net";

fn socket_dir() -> Result<PathBuf> {
    let dir = PathBuf::from(SOCKET_DIR);
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("creating {SOCKET_DIR}")),
    }
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o077 != 0 {
        bail!("{SOCKET_DIR} is not a private root directory; remove it and retry");
    }
    Ok(dir)
}

/// Starts passt for the environment `name` and waits until its socket exists.
pub fn start_passt(name: &str, dev_ssh_port: Option<u16>) -> Result<(Passt, Child)> {
    let uid = net_uid()?;
    let dir = socket_dir()?;
    let passt = Passt {
        socket: dir.join(format!("{name}.passt")),
        net_uid: uid,
        dev_ssh_port,
    };
    fs::remove_file(&passt.socket).ok();
    let command_line = passt.command_line();
    info!("starting {}", command_line.join(" "));
    let mut child = Command::new(&command_line[0])
        .args(&command_line[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .process_group(0)
        .spawn()
        .context("starting passt")?;
    wait_for_socket(&passt.socket, &mut child)?;
    Ok((passt, child))
}

fn wait_for_socket(socket: &Path, child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        if let Some(status) = child.try_wait()? {
            bail!("passt exited before listening ({status})");
        }
        if Instant::now() > deadline {
            child.kill().ok();
            bail!("passt did not create {} in time", socket.display());
        }
        sleep(Duration::from_millis(50));
    }
    Ok(())
}
