//! Running a jail VM: hand the GPU over, run QEMU until the guest powers off,
//! and give the GPU back whatever happened.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};
use std::{env, fs};

use anyhow::{Context, Result, bail};
use gamejail_host::Root;
use gamejail_host::pci::PciAddress;
use gamejail_host::vm::{Network, OVMF_VARS_TEMPLATE, VmSpec};
use tracing::{error, info, warn};

use crate::{STATE_DIR, gpu, state_path};

/// How long a guest gets to shut down after a power-down request before QEMU
/// is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

#[derive(clap::Args)]
pub struct RunArgs {
    /// The GPU to hand to the VM.
    #[arg(long)]
    gpu: PciAddress,
    /// The VM's disk. Its directory names the VM and holds the firmware
    /// variables and the serial log.
    disk: PathBuf,
    /// An extra raw disk attached read-only, such as a cloud-init seed.
    #[arg(long)]
    read_only_disk: Vec<PathBuf>,
    #[arg(long, default_value_t = 12288)]
    memory_mib: u32,
    #[arg(long, default_value_t = 8)]
    cpus: u16,
    /// Development only: QEMU user networking with SSH on 127.0.0.1:PORT.
    /// The guest reaches the host and the LAN through it.
    #[arg(long, value_name = "PORT")]
    dev_ssh_port: Option<u16>,
    /// Development only: an emulated display on VNC 127.0.0.1:5900+N.
    #[arg(long, value_name = "N")]
    dev_vnc: Option<u16>,
    /// The account QEMU drops to once its devices are open; by default the
    /// user who ran sudo or pkexec.
    #[arg(long)]
    run_as: Option<String>,
    /// In secondary mode, take the GPU while user processes still hold it.
    #[arg(long)]
    force: bool,
}

pub fn run(root: &Root, args: &RunArgs, dry_run: bool) -> Result<()> {
    let disk =
        fs::canonicalize(&args.disk).with_context(|| format!("disk {}", args.disk.display()))?;
    let dir = disk.parent().context("disk has no directory")?;
    let name = dir
        .file_name()
        .map_or_else(|| "jail".into(), |n| n.to_string_lossy().into_owned());
    let read_only_disks = args
        .read_only_disk
        .iter()
        .map(|p| fs::canonicalize(p).with_context(|| format!("disk {}", p.display())))
        .collect::<Result<Vec<_>>>()?;
    let spec_for = |passthrough: Vec<PciAddress>| VmSpec {
        name: name.clone(),
        disk: disk.clone(),
        read_only_disks: read_only_disks.clone(),
        firmware_vars: dir.join("OVMF_VARS.qcow2"),
        memory_mib: args.memory_mib,
        cpus: args.cpus,
        passthrough,
        network: args
            .dev_ssh_port
            .map_or(Network::None, |ssh_port| Network::DevUserNet { ssh_port }),
        vnc_display: args.dev_vnc,
        monitor_socket: state_path(&format!("{name}.monitor")),
        serial_log: dir.join("console.log"),
        run_as: args.run_as.clone().or_else(invoking_user),
    };

    let attached = gpu::attach(root, args.gpu, args.force, dry_run)?;
    let spec = spec_for(attached.functions.iter().map(|f| f.address).collect());
    let outcome = if dry_run {
        info!("dry run: {}", spec.command_line().join(" "));
        Ok(())
    } else {
        run_qemu(&spec)
    };
    let returned = gpu::detach(root, attached.display().address, dry_run);
    if let Err(e) = &returned {
        error!("returning the GPU failed: {e:#}");
    }
    outcome.and(returned)
}

fn run_qemu(spec: &VmSpec) -> Result<()> {
    fs::create_dir_all(STATE_DIR)?;
    if !spec.firmware_vars.exists() {
        fs::copy(OVMF_VARS_TEMPLATE, &spec.firmware_vars)
            .with_context(|| format!("creating {}", spec.firmware_vars.display()))?;
    }
    fs::remove_file(&spec.monitor_socket).ok();

    let signals = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&signals);
    ctrlc::set_handler(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
    .context("installing the signal handler")?;

    let command_line = spec.command_line();
    info!("starting {}", command_line.join(" "));
    // VFIO pins all guest memory. QEMU runs in its own process group so a
    // Ctrl+C in the terminal reaches only this service, which then asks the
    // guest to power down instead of losing the VM mid-write.
    let mut child = Command::new("prlimit")
        .arg("--memlock=unlimited:unlimited")
        .arg("--")
        .args(&command_line)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .context("starting QEMU")?;

    let mut powerdown_at: Option<Instant> = None;
    let status: ExitStatus = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        match signals.load(Ordering::SeqCst) {
            0 => {}
            1 if powerdown_at.is_none() => {
                info!("asking the guest to power down");
                request_powerdown(&spec.monitor_socket);
                powerdown_at = Some(Instant::now());
            }
            1 if powerdown_at.is_some_and(|t| t.elapsed() < SHUTDOWN_GRACE) => {}
            _ => {
                warn!("stopping QEMU");
                child.kill().ok();
            }
        }
        sleep(Duration::from_millis(200));
    };
    fs::remove_file(&spec.monitor_socket).ok();
    info!("QEMU exited: {status}");
    if !status.success() && powerdown_at.is_none() {
        bail!("QEMU exited with {status}");
    }
    Ok(())
}

fn request_powerdown(monitor: &Path) {
    let sent =
        UnixStream::connect(monitor).and_then(|mut stream| stream.write_all(b"system_powerdown\n"));
    if let Err(e) = sent {
        warn!("monitor {}: {e}", monitor.display());
    }
}

/// The user behind sudo or pkexec, by name, for QEMU's `-run-with user=`.
fn invoking_user() -> Option<String> {
    let uid = env::var("PKEXEC_UID")
        .or_else(|_| env::var("SUDO_UID"))
        .ok()?;
    let output = Command::new("id").args(["-nu", &uid]).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
