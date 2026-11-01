//! Building jail images.
//!
//! An image is built inside a VM without a GPU, from a Fedora Cloud image and a
//! cloud-init seed carrying the provisioning script. The build VM uses the jail
//! network, so it reaches package mirrors and nothing local. Its verdict comes
//! back on the serial console; a passing build is flattened into one
//! compressed qcow2 that no longer depends on the cloud image.

use std::fs;
use std::io::{BufRead as _, BufReader, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use gamejail_host::seed;
use gamejail_host::vm::{Network, VmSpec};
use tracing::info;

use crate::{state_path, vm};

const VERDICT: &str = "GAMEJAIL-BUILD: ";
/// Room for the driver build and the desktop; the jail's own overlay grows on
/// top of this.
const BUILD_DISK: &str = "40G";

#[derive(clap::Args)]
pub struct BuildBaseArgs {
    /// The Fedora Cloud Base Generic qcow2 to start from.
    #[arg(long)]
    from: PathBuf,
    /// Where to write the base image.
    out: PathBuf,
    /// Development only: keep SSH and a `dev` account with this public key.
    #[arg(long, value_name = "PUBKEY_FILE")]
    dev_ssh_key: Option<PathBuf>,
}

pub fn build_base(args: &BuildBaseArgs, dry_run: bool) -> Result<()> {
    let from = fs::canonicalize(&args.from)
        .with_context(|| format!("base image {}", args.from.display()))?;
    let out = absolute(&args.out)?;
    if out.exists() {
        bail!("{} already exists", out.display());
    }
    let stem = out
        .file_stem()
        .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
    let work = out.with_file_name(format!(".build-{stem}"));
    let key = args
        .dev_ssh_key
        .as_ref()
        .map(|p| fs::read_to_string(p).with_context(|| format!("key {}", p.display())))
        .transpose()?;

    let overlay = work.join("disk.qcow2");
    let seed_img = work.join("seed.img");
    let console = work.join("console.log");
    let spec_for = |socket: PathBuf| VmSpec {
        name: format!("build-{stem}"),
        disk: overlay.clone(),
        read_only_disks: vec![seed_img.clone()],
        firmware_vars: work.join("OVMF_VARS.qcow2"),
        memory_mib: 6144,
        cpus: 8,
        passthrough: Vec::new(),
        network: Network::Passt { socket },
        vnc_display: None,
        monitor_socket: state_path("build.monitor"),
        serial_log: console.clone(),
        run_as: vm::invoking_user(),
    };
    if dry_run {
        info!("dry run: build in {}", work.display());
        info!(
            "dry run: {}",
            spec_for(PathBuf::from("passt.socket"))
                .command_line()
                .join(" ")
        );
        return Ok(());
    }

    if work.exists() {
        fs::remove_dir_all(&work)?;
    }
    fs::create_dir_all(&work)?;
    run(
        "qemu-img",
        &["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"],
        &[&from, &overlay],
        &[BUILD_DISK],
    )?;
    let user_data = seed::cloud_config(seed::BASE_PROVISION, key.as_deref());
    seed::write_nocloud(&seed_img, &format!("gamejail-{stem}-{}", now()), &user_data)?;
    info!(
        "building {}; guest console: {}",
        out.display(),
        console.display()
    );

    let done = Arc::new(AtomicBool::new(false));
    let follower = follow_console(console.clone(), Arc::clone(&done));
    let outcome = vm::run_networked("build", true, None, spec_for);
    done.store(true, Ordering::SeqCst);
    follower.join().ok();
    outcome?;

    let verdict = last_verdict(&console)?;
    if verdict != "OK" {
        bail!(
            "the build failed ({verdict}); its files are kept in {}",
            work.display()
        );
    }
    info!("flattening into {}", out.display());
    run(
        "qemu-img",
        &["convert", "-c", "-O", "qcow2"],
        &[&overlay, &out],
        &[],
    )?;
    give_to_invoking_user(&out);
    fs::remove_dir_all(&work)?;
    info!("{} is ready", out.display());
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = fs::canonicalize(parent.unwrap_or(Path::new(".")))
        .with_context(|| format!("directory of {}", path.display()))?;
    Ok(dir.join(path.file_name().context("no file name")?))
}

fn run(program: &str, before: &[&str], paths: &[&Path], after: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(before)
        .args(paths)
        .args(after)
        .status()
        .with_context(|| format!("starting {program}"))?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Logs the provisioning steps and the verdict as they reach the console.
fn follow_console(console: PathBuf, done: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut position = 0;
        while !done.load(Ordering::SeqCst) {
            if let Ok(mut file) = fs::File::open(&console)
                && file.seek(SeekFrom::Start(position)).is_ok()
            {
                let mut reader = BufReader::new(file);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if !line.ends_with('\n') {
                        break;
                    }
                    position += line.len() as u64;
                    let text = line.trim();
                    if text.starts_with("=== ") || text.starts_with(VERDICT) {
                        info!("guest: {text}");
                    }
                    line.clear();
                }
            }
            thread::sleep(Duration::from_secs(1));
        }
    })
}

fn last_verdict(console: &Path) -> Result<String> {
    let log = fs::read(console).with_context(|| format!("reading {}", console.display()))?;
    String::from_utf8_lossy(&log)
        .lines()
        .rev()
        .find_map(|l| {
            l.find(VERDICT)
                .map(|at| l[at + VERDICT.len()..].trim().to_owned())
        })
        .context("the guest powered off without a verdict")
}

fn give_to_invoking_user(path: &Path) {
    let owner = std::env::var("PKEXEC_UID")
        .or_else(|_| std::env::var("SUDO_UID"))
        .ok()
        .and_then(|uid| uid.parse().ok());
    if let Some(uid) = owner {
        std::os::unix::fs::chown(path, Some(uid), Some(uid)).ok();
    }
}
