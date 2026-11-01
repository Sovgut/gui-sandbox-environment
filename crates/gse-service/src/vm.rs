//! Running an environment VM: hand the GPU over, run QEMU until the guest
//! powers off or the host stops it, and give the GPU back whatever happened.
//!
//! The host stops an environment by killing QEMU, never by asking the guest:
//! whatever runs inside could ignore or delay a power-down request. The guest
//! loses what it had not written yet, as on a power cut; a clean shutdown is
//! started from inside the environment.

use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;
use std::{env, fs};

use anyhow::{Context, Result, bail};
use gse_host::Root;
use gse_host::pci::PciAddress;
use gse_host::vm::{Network, OVMF_VARS_TEMPLATE, Screen, VmSpec};
use tracing::{error, info, warn};

use crate::{STATE_DIR, gpu, net, state_path};

/// Held for as long as a VM runs. Every VM, an environment or an image build, shares
/// the one environment network ruleset and the host's memory, so a second one is
/// refused rather than started beside it.
pub struct RunLock(#[allow(dead_code)] fs::File);

/// Takes the run lock, or fails if another VM is running.
pub fn lock() -> Result<RunLock> {
    fs::create_dir_all(STATE_DIR)?;
    let path = state_path("vm.lock");
    let file = fs::File::create(&path).with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(RunLock(file)),
        Err(fs::TryLockError::WouldBlock) => bail!("another environment or image build is running"),
        Err(fs::TryLockError::Error(e)) => Err(e).context("locking"),
    }
}

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
    /// Development only: forward 127.0.0.1:PORT on the host to the guest's
    /// SSH port.
    #[arg(long, value_name = "PORT")]
    dev_ssh_port: Option<u16>,
    /// Start with the internet off; `net internet on` turns it on while the
    /// environment runs.
    #[arg(long)]
    offline: bool,
    /// Development only: an emulated display on VNC 127.0.0.1:5900+N.
    #[arg(long, value_name = "N")]
    dev_vnc: Option<u16>,
    /// The mode of the environment screen shown on the host.
    #[arg(long, default_value = "2560x1440@60")]
    screen_mode: String,
    /// The account QEMU drops to once its devices are open; by default the
    /// user who ran sudo or pkexec.
    #[arg(long)]
    run_as: Option<String>,
}

pub fn run(root: &Root, args: &RunArgs, dry_run: bool) -> Result<()> {
    let disk =
        fs::canonicalize(&args.disk).with_context(|| format!("disk {}", args.disk.display()))?;
    let dir = disk.parent().context("disk has no directory")?;
    let name = dir.file_name().map_or_else(
        || "environment".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    let read_only_disks = args
        .read_only_disk
        .iter()
        .map(|p| fs::canonicalize(p).with_context(|| format!("disk {}", p.display())))
        .collect::<Result<Vec<_>>>()?;
    let _lock = if dry_run { None } else { Some(lock()?) };
    let screen = PathBuf::from(format!("/dev/shm/gse-{name}.screen"));
    if !dry_run {
        create_screen(&screen)?;
    }
    let attached = gpu::attach(root, args.gpu, dry_run)?;
    let spec_for = |socket: PathBuf| VmSpec {
        name: name.clone(),
        disk: disk.clone(),
        read_only_disks: read_only_disks.clone(),
        firmware_vars: dir.join("OVMF_VARS.qcow2"),
        memory_mib: args.memory_mib,
        cpus: args.cpus,
        passthrough: attached.functions.iter().map(|f| f.address).collect(),
        network: Network::Passt { socket },
        vnc_display: args.dev_vnc,
        screen: Some(Screen {
            memory: screen.clone(),
            mode: args.screen_mode.clone(),
            sound_server: PathBuf::from(format!(
                "/run/user/{}/pulse/native",
                invoking_uid().unwrap_or(0)
            )),
        }),
        input_socket: Some(input_socket(&name)),
        monitor_socket: state_path(&format!("{name}.monitor")),
        serial_log: dir.join("console.log"),
        run_as: args.run_as.clone().or_else(invoking_user),
        image_build: false,
    };

    let outcome = if dry_run {
        net::apply(!args.offline, true)?;
        let spec = spec_for(PathBuf::from(net::SOCKET_DIR).join(format!("{name}.passt")));
        info!("dry run: {}", spec.command_line().join(" "));
        Ok(())
    } else {
        run_networked(&name, !args.offline, args.dev_ssh_port, spec_for)
    };
    let returned = gpu::detach(root, attached.display().address, dry_run);
    if let Err(e) = &returned {
        error!("returning the GPU failed: {e:#}");
    }
    fs::remove_file(&screen).ok();
    outcome.and(returned)
}

/// The line the guest writes to its serial console once its desktop is up.
const READY: &[u8] = b"GSE-READY";

/// Watches a VM's serial log for [`READY`].
struct ReadyWatch {
    log: PathBuf,
    seen: bool,
    read: u64,
}

impl ReadyWatch {
    fn new(log: &Path) -> Self {
        Self {
            log: log.to_owned(),
            seen: false,
            read: 0,
        }
    }

    /// Whether the line arrived since the last call; true once.
    fn check(&mut self) -> bool {
        use std::io::{Read as _, Seek as _, SeekFrom};
        if self.seen {
            return false;
        }
        let Ok(mut file) = fs::File::open(&self.log) else {
            return false;
        };
        // Re-read a little before the last end, so a marker split across two
        // reads is still found.
        let start = self.read.saturating_sub(READY.len() as u64);
        let mut tail = Vec::new();
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut tail).is_err() {
            return false;
        }
        self.read = start + tail.len() as u64;
        self.seen = tail.windows(READY.len()).any(|w| w == READY);
        self.seen
    }
}

/// QEMU's input socket for the environment `name`.
fn input_socket(name: &str) -> PathBuf {
    state_path(&format!("{name}.input"))
}

/// Gives the user who started the environment the socket QEMU created, once it
/// exists; their viewer sends input through it.
fn hand_to_invoking_user(socket: &Path) -> bool {
    if !socket.exists() {
        return false;
    }
    if let Some(uid) = invoking_uid() {
        std::os::unix::fs::chown(socket, Some(uid), Some(uid)).ok();
    }
    info!("environment input: {}", socket.display());
    true
}

/// Creates the environment screen memory, readable only by the user who started the
/// environment: their viewer shows it, and nobody else on the host may.
fn create_screen(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::remove_file(path).ok();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.set_len(gse_screen::SIZE as u64)?;
    if let Some(uid) = invoking_uid() {
        std::os::unix::fs::fchown(&file, Some(uid), Some(uid))?;
    }
    Ok(())
}

/// Brings up the environment network, runs the VM `spec_for` describes once passt's
/// socket exists, and takes the network down again. The caller holds the
/// [`lock`].
pub fn run_networked(
    name: &str,
    internet: bool,
    dev_ssh_port: Option<u16>,
    spec_for: impl Fn(PathBuf) -> VmSpec,
) -> Result<()> {
    net::apply(internet, false)?;
    let outcome = net::start_passt(name, dev_ssh_port).and_then(|(passt, mut child)| {
        let outcome = run_qemu(&spec_for(passt.socket));
        // passt exits with its one guest; this only covers a QEMU that never
        // connected.
        child.kill().ok();
        child.wait().ok();
        outcome
    });
    if let Err(e) = net::remove(false) {
        error!("removing the environment network rules failed: {e:#}");
    }
    outcome
}

fn run_qemu(spec: &VmSpec) -> Result<()> {
    fs::create_dir_all(STATE_DIR)?;
    if !spec.firmware_vars.exists() {
        fs::copy(OVMF_VARS_TEMPLATE, &spec.firmware_vars)
            .with_context(|| format!("creating {}", spec.firmware_vars.display()))?;
    }
    fs::remove_file(&spec.monitor_socket).ok();
    if let Some(socket) = &spec.input_socket {
        fs::remove_file(socket).ok();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let requested = Arc::clone(&stop);
    ctrlc::set_handler(move || requested.store(true, Ordering::SeqCst))
        .context("installing the signal handler")?;

    let command_line = spec.command_line();
    info!("starting {}", command_line.join(" "));
    // VFIO pins all guest memory. QEMU runs in its own process group so a
    // Ctrl+C in the terminal reaches only this service, which then kills QEMU
    // and still gives the GPU back.
    let mut qemu = Command::new("prlimit");
    qemu.arg("--memlock=unlimited:unlimited")
        .arg("--")
        .args(&command_line)
        .stdin(Stdio::null())
        .process_group(0);
    let mut child = qemu.spawn().context("starting QEMU")?;

    let mut stopped = false;
    let mut ready = ReadyWatch::new(&spec.serial_log);
    let mut input_handed_over = spec.input_socket.is_none();
    let status: ExitStatus = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if !input_handed_over && let Some(socket) = &spec.input_socket {
            input_handed_over = hand_to_invoking_user(socket);
        }
        // Image builds boot the same guest, whose desktop also reports
        // ready; only an environment's readiness means anything.
        if spec.input_socket.is_some() && ready.check() {
            info!("environment is ready");
        }
        if !stopped && stop.load(Ordering::SeqCst) {
            warn!("stopping QEMU");
            child.kill().ok();
            stopped = true;
        }
        sleep(Duration::from_millis(200));
    };
    fs::remove_file(&spec.monitor_socket).ok();
    if let Some(socket) = &spec.input_socket {
        fs::remove_file(socket).ok();
    }
    info!("QEMU exited: {status}");
    if !status.success() && !stopped {
        bail!("QEMU exited with {status}");
    }
    Ok(())
}

/// The user behind sudo or pkexec, by name, for QEMU's `-run-with user=`.
/// The user who ran sudo or pkexec.
fn invoking_uid() -> Option<u32> {
    env::var("PKEXEC_UID")
        .or_else(|_| env::var("SUDO_UID"))
        .ok()?
        .parse()
        .ok()
}

pub fn invoking_user() -> Option<String> {
    let uid = invoking_uid()?.to_string();
    let output = Command::new("id").args(["-nu", &uid]).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
