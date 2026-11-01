//! `uninstall`: removes everything gse put on the machine, so nothing of it
//! stays. The app runs it through polkit from "Remove everything" in its
//! settings; the files in the user's home are the app's to remove.
//!
//! The service is stopped first, which stops any environment and gives its
//! GPU back. Then go the unit and its drop-ins, the network rules, the
//! settings, the images and environments, the state and sockets, the polkit
//! action, the network account and, last, the service's own program.

use std::fs;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use gse_protocol::{Reply, Request, receive, send};

use tracing::{info, warn};

use crate::net;

/// Directories gse owns entirely.
const DIRECTORIES: [&str; 5] = [
    "/etc/gse",
    "/var/lib/gse",
    "/run/gse",
    net::SOCKET_DIR,
    "/usr/libexec/gse",
];

/// Files gse put in shared directories, wherever an install may have put
/// them.
const FILES: [&str; 5] = [
    "/usr/share/polkit-1/actions/io.github.sovgut.gse.policy",
    "/etc/polkit-1/actions/io.github.sovgut.gse.policy",
    "/etc/sysusers.d/gse.conf",
    "/usr/lib/sysusers.d/gse.conf",
    "/usr/local/sbin/gse-service",
];

/// Runs `program` with `args`; a failure is logged, since removing goes on.
fn run(program: &str, args: &[&str], dry_run: bool) {
    info!("{program} {}", args.join(" "));
    if dry_run {
        return;
    }
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => warn!("{program} {}: {status}", args.join(" ")),
        Err(e) => warn!("{program}: {e}"),
    }
}

fn remove(path: &Path, dry_run: bool) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    info!("removing {}", path.display());
    if dry_run {
        return;
    }
    let outcome = if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    if let Err(e) = outcome {
        warn!("removing {}: {e}", path.display());
    }
}

/// Asks the running service to stop its environment, and waits until QEMU
/// is gone and the GPU is back with the host.
fn stop_environment(dry_run: bool) {
    info!("stopping any running environment");
    if dry_run {
        return;
    }
    let Ok(mut stream) = UnixStream::connect(gse_protocol::SOCKET) else {
        return;
    };
    if send(&mut stream, &Request::Stop).is_err() {
        return;
    }
    let mut reader = BufReader::new(stream);
    receive::<Reply>(&mut reader).ok();
    for _ in 0..600 {
        let running = fs::read_dir("/run/gse")
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".qemu-pid"))
            });
        if !running {
            return;
        }
        sleep(Duration::from_millis(100));
    }
    warn!("the environment did not stop in time");
}

pub fn uninstall(dry_run: bool) {
    stop_environment(dry_run);
    run("systemctl", &["disable", "--now", "gse.service"], dry_run);
    remove(Path::new("/etc/systemd/system/gse.service.d"), dry_run);
    remove(Path::new("/etc/systemd/system/gse.service"), dry_run);
    run("systemctl", &["daemon-reload"], dry_run);
    run("nft", &["delete", "table", "inet", "gse"], dry_run);
    for directory in DIRECTORIES {
        remove(Path::new(directory), dry_run);
    }
    for entry in fs::read_dir("/dev/shm").into_iter().flatten().flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("gse-") && n.ends_with(".screen"))
        {
            remove(&entry.path(), dry_run);
        }
    }
    if Command::new("getent")
        .args(["passwd", "gse-net"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        run("userdel", &["gse-net"], dry_run);
    }
    for file in FILES {
        remove(Path::new(file), dry_run);
    }
    info!("gse is removed");
}
