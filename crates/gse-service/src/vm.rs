//! Running an environment VM: hand the GPU over, run QEMU until the guest
//! powers off or the host stops it, and give the GPU back whatever happened.
//!
//! The host stops an environment by killing QEMU, never by asking the guest:
//! whatever runs inside could ignore or delay a power-down request. The guest
//! loses what it had not written yet, as on a power cut; a clean shutdown is
//! started from inside the environment.

use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gse_host::Root;
use gse_host::pci::PciAddress;
use gse_host::vm::{Network, OVMF_VARS_TEMPLATE, Screen, VmSpec};
use gse_protocol::Stage;
use tracing::{error, info, warn};

use crate::{STATE_DIR, gpu, net, state_path};

/// Held for as long as a VM runs. Every VM, an environment or an image
/// build, shares the one environment network ruleset and the host's memory,
/// so a second one is refused rather than started beside it.
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

/// The user an environment runs for: QEMU drops to this account, and the
/// screen memory, the input socket and the sound go to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub name: String,
}

impl Owner {
    pub fn from_uid(uid: u32) -> Result<Self> {
        let output = Command::new("id")
            .args(["-nu", &uid.to_string()])
            .output()?;
        if !output.status.success() {
            bail!("no account with uid {uid}");
        }
        Ok(Self {
            uid,
            name: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        })
    }

    /// The user who ran sudo or pkexec.
    pub fn invoking() -> Option<Self> {
        let uid = std::env::var("PKEXEC_UID")
            .or_else(|_| std::env::var("SUDO_UID"))
            .ok()?
            .parse()
            .ok()?;
        Self::from_uid(uid).ok()
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct Launch {
    pub name: String,
    pub disk: PathBuf,
    pub read_only_disks: Vec<PathBuf>,
    pub gpu: PciAddress,
    pub memory_mib: u32,
    pub cpus: u16,
    pub internet: bool,
    /// The mode of the environment screen shown on the host.
    pub screen_mode: String,
    pub owner: Owner,
    /// Development only: forward 127.0.0.1:PORT to the guest's SSH port.
    pub dev_ssh_port: Option<u16>,
    /// Development only: an emulated display on VNC 127.0.0.1:5900+N.
    pub dev_vnc: Option<u16>,
}

impl Launch {
    /// The environment screen memory, readable by the owner only.
    #[must_use]
    pub fn screen_memory(&self) -> PathBuf {
        PathBuf::from(format!("/dev/shm/gse-{}.screen", self.name))
    }

    /// QEMU's input socket, handed to the owner.
    #[must_use]
    pub fn input_socket(&self) -> PathBuf {
        state_path(&format!("{}.input", self.name))
    }

    /// The guest agent's port, which only the service talks to.
    #[must_use]
    pub fn agent_socket(&self) -> PathBuf {
        agent_socket(&self.name)
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
}

/// `vm run`: launches the environment whose disk is given, for the user who
/// ran sudo or pkexec, until it powers off or `stop` is set.
pub fn run(root: &Root, args: &RunArgs, stop: &AtomicBool, dry_run: bool) -> Result<()> {
    let disk =
        fs::canonicalize(&args.disk).with_context(|| format!("disk {}", args.disk.display()))?;
    let dir = disk.parent().context("disk has no directory")?;
    let launch = Launch {
        name: dir.file_name().map_or_else(
            || "environment".into(),
            |n| n.to_string_lossy().into_owned(),
        ),
        disk: disk.clone(),
        read_only_disks: args
            .read_only_disk
            .iter()
            .map(|p| fs::canonicalize(p).with_context(|| format!("disk {}", p.display())))
            .collect::<Result<Vec<_>>>()?,
        gpu: args.gpu,
        memory_mib: args.memory_mib,
        cpus: args.cpus,
        internet: !args.offline,
        screen_mode: args.screen_mode.clone(),
        owner: Owner::invoking().context("run this through sudo or pkexec")?,
        dev_ssh_port: args.dev_ssh_port,
        dev_vnc: args.dev_vnc,
    };
    launch_environment(root, &launch, stop, &mut |_| {}, dry_run)
}

/// Runs `launch` until the guest powers off or `stop` is set, reporting each
/// stage to `progress` as it is reached.
pub fn launch_environment(
    root: &Root,
    launch: &Launch,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Stage),
    dry_run: bool,
) -> Result<()> {
    progress(Stage::Preparing);
    info!("environment {}: preparing", launch.name);
    let dir = launch.disk.parent().context("disk has no directory")?;
    let _lock = if dry_run { None } else { Some(lock()?) };
    let screen = launch.screen_memory();
    if !dry_run {
        create_screen(&screen, launch.owner.uid)?;
    }
    progress(Stage::SwitchingGpu);
    let attached = match gpu::attach(root, launch.gpu, dry_run) {
        Ok(attached) => attached,
        Err(e) => {
            fs::remove_file(&screen).ok();
            return Err(e);
        }
    };
    let spec_for = |socket: PathBuf| VmSpec {
        name: launch.name.clone(),
        disk: launch.disk.clone(),
        read_only_disks: launch.read_only_disks.clone(),
        firmware_vars: dir.join("OVMF_VARS.qcow2"),
        memory_mib: launch.memory_mib,
        cpus: launch.cpus,
        passthrough: attached.functions.iter().map(|f| f.address).collect(),
        network: Network::Passt { socket },
        vnc_display: launch.dev_vnc,
        screen: Some(Screen {
            memory: screen.clone(),
            mode: launch.screen_mode.clone(),
            sound_server: PathBuf::from(format!("/run/user/{}/pulse/native", launch.owner.uid)),
        }),
        input_socket: Some(launch.input_socket()),
        agent_socket: Some(launch.agent_socket()),
        monitor_socket: state_path(&format!("{}.monitor", launch.name)),
        serial_log: dir.join("console.log"),
        run_as: Some(launch.owner.name.clone()),
        image_build: false,
    };

    let outcome = if dry_run {
        net::apply(launch.internet, true)?;
        let spec = spec_for(PathBuf::from(net::SOCKET_DIR).join(format!("{}.passt", launch.name)));
        info!("dry run: {}", spec.command_line().join(" "));
        Ok(())
    } else {
        let mut watch = Watch {
            owner: Some(launch.owner.uid),
            progress,
        };
        run_networked(
            &launch.name,
            launch.internet,
            launch.dev_ssh_port,
            stop,
            &mut watch,
            spec_for,
        )
    };
    let returned = gpu::detach(root, attached.display().address, dry_run);
    if let Err(e) = &returned {
        error!("returning the GPU failed: {e:#}");
    }
    fs::remove_file(&screen).ok();
    outcome.and(returned)
}

/// Where the pid of the VM `name`'s QEMU is kept while it runs.
/// The guest agent's port of the environment `name`.
pub fn agent_socket(name: &str) -> PathBuf {
    state_path(&format!("{name}.agent"))
}

fn qemu_pid_file(name: &str) -> PathBuf {
    state_path(&format!("{name}.qemu-pid"))
}

/// Cleans up after a service that died while a VM ran: kills the QEMU it
/// left behind, returns every GPU it had handed over, and removes the
/// network rules, screen memory and sockets. Run before serving anything.
pub fn recover(root: &Root) {
    let Ok(entries) = fs::read_dir(STATE_DIR) else {
        return;
    };
    let mut found = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(name) = file.strip_suffix(".qemu-pid") {
            found = true;
            if let Some(pid) = fs::read_to_string(&path)
                .ok()
                .and_then(|p| p.trim().parse::<i32>().ok())
            {
                kill_qemu(name, pid);
            }
            fs::remove_file(&path).ok();
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("input" | "monitor")
        ) {
            fs::remove_file(&path).ok();
        }
    }
    for gpu in gpu::handed_over() {
        found = true;
        warn!("returning {gpu}, which a previous service left on vfio-pci");
        if let Err(e) = gpu::detach(root, gpu, false) {
            error!("returning {gpu} failed: {e:#}");
        }
    }
    if found {
        net::remove(false).ok();
    }
    for entry in fs::read_dir("/dev/shm").into_iter().flatten().flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.starts_with("gse-") && file.ends_with(".screen") {
            fs::remove_file(entry.path()).ok();
        }
    }
}

/// Kills the QEMU of the VM `name` if `pid` is still that process, and waits
/// for it to go.
fn kill_qemu(name: &str, pid: i32) {
    let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let is_it = cmdline.split(|&b| b == 0).any(|arg| arg == name.as_bytes())
        && cmdline.windows(11).any(|w| w == b"qemu-system");
    if !is_it {
        return;
    }
    warn!("stopping the QEMU of {name} (pid {pid}), left by a previous service");
    let Some(process) = rustix::process::Pid::from_raw(pid) else {
        return;
    };
    rustix::process::kill_process(process, rustix::process::Signal::KILL).ok();
    for _ in 0..50 {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return;
        }
        sleep(Duration::from_millis(100));
    }
}

/// What a running VM reports back: its input socket goes to `owner`, and
/// [`Stage::Booting`] and [`Stage::Ready`] go to `progress`.
pub struct Watch<'a> {
    pub owner: Option<u32>,
    pub progress: &'a mut dyn FnMut(Stage),
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

    /// Whether the line has arrived.
    fn check_seen(&mut self) -> bool {
        self.seen || self.check()
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

/// Whether the VM's screen memory holds a frame, or it has none.
fn screen_shows(spec: &VmSpec) -> bool {
    let Some(screen) = &spec.screen else {
        return true;
    };
    fs::File::open(&screen.memory)
        .ok()
        .and_then(|file| gse_screen::Reader::new(&file).ok())
        .is_some_and(|reader| reader.published() > 0)
}

/// Gives `uid` the socket QEMU created, once it exists; their viewer sends
/// input through it.
fn hand_over(socket: &Path, uid: u32) -> bool {
    if !socket.exists() {
        return false;
    }
    std::os::unix::fs::chown(socket, Some(uid), Some(uid)).ok();
    info!("environment input: {}", socket.display());
    true
}

/// Creates the environment screen memory, readable only by `uid`: their
/// viewer shows it, and nobody else on the host may.
fn create_screen(path: &Path, uid: u32) -> Result<()> {
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
    std::os::unix::fs::fchown(&file, Some(uid), Some(uid))?;
    Ok(())
}

/// Brings up the environment network, runs the VM `spec_for` describes once
/// passt's socket exists, and takes the network down again. The caller holds
/// the [`lock`].
pub fn run_networked(
    name: &str,
    internet: bool,
    dev_ssh_port: Option<u16>,
    stop: &AtomicBool,
    watch: &mut Watch<'_>,
    spec_for: impl Fn(PathBuf) -> VmSpec,
) -> Result<()> {
    net::apply(internet, false)?;
    let outcome = net::start_passt(name, dev_ssh_port).and_then(|(passt, mut child)| {
        let outcome = run_qemu(&spec_for(passt.socket), stop, watch);
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

fn run_qemu(spec: &VmSpec, stop: &AtomicBool, watch: &mut Watch<'_>) -> Result<()> {
    fs::create_dir_all(STATE_DIR)?;
    if !spec.firmware_vars.exists() {
        fs::copy(OVMF_VARS_TEMPLATE, &spec.firmware_vars)
            .with_context(|| format!("creating {}", spec.firmware_vars.display()))?;
    }
    fs::remove_file(&spec.monitor_socket).ok();
    for socket in spec.input_socket.iter().chain(&spec.agent_socket) {
        fs::remove_file(socket).ok();
    }

    let command_line = spec.command_line();
    info!("starting {}", command_line.join(" "));
    // VFIO pins all guest memory. QEMU runs in its own process group so a
    // Ctrl+C in the terminal reaches only this service, which then kills QEMU
    // and still gives the GPU back.
    let mut child = Command::new("prlimit")
        .arg("--memlock=unlimited:unlimited")
        .arg("--")
        .args(&command_line)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .context("starting QEMU")?;
    // A service that dies leaves QEMU running in its own process group; the
    // next service finds it through this file (see `recover`).
    let pid_file = qemu_pid_file(&spec.name);
    fs::write(&pid_file, child.id().to_string()).ok();
    (watch.progress)(Stage::Booting);

    let mut stopped = false;
    let mut ready = ReadyWatch::new(&spec.serial_log);
    let mut ready_reported = false;
    let mut input_handed_over = spec.input_socket.is_none() || watch.owner.is_none();
    let status: ExitStatus = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if !input_handed_over && let (Some(socket), Some(uid)) = (&spec.input_socket, watch.owner) {
            input_handed_over = hand_over(socket, uid);
        }
        // Image builds boot the same guest, whose desktop also reports
        // ready; only an environment's readiness means anything. With its
        // screen on the host, the environment is ready once the first frame
        // is there too, so the app never shows a ready but empty screen.
        if !spec.image_build && !ready_reported && ready.check_seen() && screen_shows(spec) {
            ready_reported = true;
            info!("environment is ready");
            (watch.progress)(Stage::Ready);
        }
        if !stopped && stop.load(Ordering::SeqCst) {
            warn!("stopping QEMU");
            child.kill().ok();
            stopped = true;
        }
        // Fast until the owner has the input socket, so a client never
        // finds it still root's.
        sleep(Duration::from_millis(if input_handed_over {
            200
        } else {
            20
        }));
    };
    fs::remove_file(&pid_file).ok();
    fs::remove_file(&spec.monitor_socket).ok();
    for socket in spec.input_socket.iter().chain(&spec.agent_socket) {
        fs::remove_file(socket).ok();
    }
    info!("QEMU exited: {status}");
    if !status.success() && !stopped {
        bail!("QEMU exited with {status}");
    }
    Ok(())
}
