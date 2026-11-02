//! `install` and `uninstall`: putting gse on the machine, and taking all of
//! it off again. The app runs both through polkit.
//!
//! `install` takes the service and guest programs from the directory its
//! own program is in: the app gives it a copy of the AppImage's
//! `usr/libexec/gse` that root can read, since root cannot read the
//! AppImage's FUSE mount. They go to [`PROGRAM_DIR`]. The systemd unit, the
//! polkit action, the sysusers entry and the default settings are built
//! into this program from `dist/`. Installing over an install updates it:
//! the programs and gse's own files are replaced, the settings and any
//! drop-ins of the unit stay, and the service restarts on the new program.
//!
//! `uninstall` removes everything gse put on the machine, so nothing of it
//! stays; the files in the user's home are the app's to remove. The service
//! is stopped first, which stops any environment and gives its GPU back.
//! Then go the unit and its drop-ins, the network rules, the settings, the
//! images and environments, the state and sockets, the polkit action, the
//! network account and, last, the service's own programs.

use std::fs;
use std::io::{BufReader, ErrorKind, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gse_host::net::{NET_USER, PASST};
use gse_host::vm::{OVMF_CODE, OVMF_VARS_TEMPLATE, QEMU};
use gse_protocol::{Reply, Request, VERSION, receive, send};
use tracing::{info, warn};

use crate::{daemon, net};

/// Where `install` puts the programs. On Fedora Atomic systems such as
/// Bazzite `/usr` is read-only, and `/usr/local` is the writable
/// `/var/usrlocal`.
const PROGRAM_DIR: &str = "/usr/local/libexec/gse";
/// The programs `install` puts there: the service, and the guest-side
/// program the service puts into base images.
const PROGRAMS: [&str; 2] = ["gse-service", "gse-guest"];

const UNIT_NAME: &str = "gse.service";
const UNIT: &str = "/etc/systemd/system/gse.service";
const UNIT_TEXT: &str = include_str!("../../../dist/systemd/gse.service");
const POLICY: &str = "io.github.sovgut.gse.policy";
const POLICY_TEXT: &str =
    include_str!("../../../dist/polkit-1/actions/io.github.sovgut.gse.policy");
/// Where polkit reads actions, in the order `install` tries them: `/usr` is
/// read-only on Fedora Atomic systems, and polkit reads `/etc` too.
const POLICY_DIRS: [&str; 2] = ["/usr/share/polkit-1/actions", "/etc/polkit-1/actions"];
const SYSUSERS: &str = "/etc/sysusers.d/gse.conf";
const SYSUSERS_TEXT: &str = include_str!("../../../dist/sysusers.d/gse.conf");
/// The settings a machine starts with, at [`daemon::CONFIG`].
const SETTINGS_TEXT: &str = include_str!("../../../dist/gse/gse.toml");

/// The Fedora packages that hold what the service runs: QEMU with the
/// PulseAudio sound backend it plays through, the UEFI firmware, passt with
/// its SELinux policy, and nftables.
const PACKAGES: [&str; 7] = [
    "qemu-system-x86-core",
    "qemu-img",
    "qemu-audio-pa",
    "edk2-ovmf",
    "passt",
    "passt-selinux",
    "nftables",
];
/// Present on an rpm-ostree system (Fedora Atomic, Bazzite), where packages
/// are layered onto the image rather than installed with dnf.
const OSTREE_BOOTED: &str = "/run/ostree-booted";

/// Programs the service runs that a machine may lack.
const TOOLS: [&str; 4] = [QEMU, "qemu-img", PASST, "nft"];
/// Where systemd finds the programs a service runs.
const SERVICE_PATH: [&str; 4] = ["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin"];
/// How long the service may take to answer once it restarted: it gives
/// back a GPU an environment held before it listens.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long polkit may take to know the action once its file is in place.
const POLKIT_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the action's file is put in place again while polkit does not
/// know the action: longer than the 4 s polkit's file monitor takes to find
/// a directory that was missing.
const POLKIT_NUDGE: Duration = Duration::from_secs(5);

/// Directories gse owns entirely.
const DIRECTORIES: [&str; 6] = [
    "/etc/gse",
    "/var/lib/gse",
    "/run/gse",
    net::SOCKET_DIR,
    PROGRAM_DIR,
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

/// A step of `install`, named by the id its report lines carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The packages the service runs, installed when missing.
    Packages,
    /// What environments need from the machine; what is missing is only
    /// reported.
    Check,
    /// The service and guest programs.
    Programs,
    /// The unit, the polkit action, the sysusers entry and the settings.
    Files,
    /// The network account.
    Account,
    /// The service, enabled and started on the new program.
    Service,
}

impl Step {
    const fn id(self) -> &'static str {
        match self {
            Self::Packages => "packages",
            Self::Check => "check",
            Self::Programs => "programs",
            Self::Files => "files",
            Self::Account => "account",
            Self::Service => "service",
        }
    }
}

/// A report line: `kind` and the id of `step`, then `text` on the same
/// line.
fn line(kind: &str, step: Step, text: Option<&str>) -> String {
    let mut line = format!("{kind} {}", step.id());
    if let Some(text) = text {
        line.push(' ');
        line.extend(
            text.trim()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c }),
        );
    }
    line
}

/// Prints a report line for the app. The app may have gone away; the
/// install goes on without it.
fn report(kind: &str, step: Step, text: Option<&str>) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", line(kind, step, text)).ok();
    out.flush().ok();
}

/// Runs `work` as `step`, reporting its start, every warning it gives and
/// how it ends.
fn run_step(step: Step, work: impl FnOnce(&mut dyn FnMut(String)) -> Result<()>) -> Result<()> {
    report("step", step, None);
    let mut warning = |text: String| {
        warn!("{}: {text}", step.id());
        report("warning", step, Some(&text));
    };
    match work(&mut warning) {
        Ok(()) => {
            report("done", step, None);
            Ok(())
        }
        Err(e) => {
            report("failed", step, Some(&format!("{e:#}")));
            Err(e.context(format!("the {} step of the install failed", step.id())))
        }
    }
}

/// `install`. On stdout each step prints `step <id>` as it begins,
/// `warning <id> <text>` for each thing it found that does not stop it, and
/// `done <id>` or `failed <id> <reason>` as it ends. The steps are, in
/// order, `check`, `programs`, `files`, `account` and `service`; a failed
/// step ends the install. Logs go to stderr.
pub fn install(dry_run: bool) -> Result<()> {
    run_step(Step::Packages, |warning| install_packages(warning, dry_run))?;
    run_step(Step::Check, |warning| {
        check(warning);
        Ok(())
    })?;
    run_step(Step::Programs, |warning| install_programs(warning, dry_run))?;
    run_step(Step::Files, |warning| install_files(warning, dry_run))?;
    run_step(Step::Account, |_| create_account(dry_run))?;
    run_step(Step::Service, |_| start_service(dry_run))?;
    info!("gse is installed");
    Ok(())
}

/// Installs the [`PACKAGES`] the machine lacks: with dnf, or layered live
/// onto an rpm-ostree image. Elsewhere it only says what to install.
fn install_packages(warning: &mut dyn FnMut(String), dry_run: bool) -> Result<()> {
    if !Path::new("/usr/bin/rpm").exists() {
        warning(
            "this is not a Fedora-family system: install QEMU for x86_64 with qemu-img and \
             PulseAudio sound, the OVMF UEFI firmware, passt and nftables yourself"
                .into(),
        );
        return Ok(());
    }
    let missing: Vec<&str> = PACKAGES
        .into_iter()
        .filter(|package| {
            !Command::new("rpm")
                .args(["-q", "--quiet", package])
                .status()
                .is_ok_and(|s| s.success())
        })
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    info!("installing {}", missing.join(" "));
    if Path::new(OSTREE_BOOTED).exists() {
        let mut args = vec!["install", "--idempotent", "--apply-live"];
        args.extend(&missing);
        command("rpm-ostree", &args, dry_run).context(
            "the packages could not be added to the running system; restart the computer \
             and try again",
        )?;
        // rpm-ostree may layer the packages for the next boot only, and
        // still succeed: then the programs are not there yet.
        if !dry_run && tools_missing() {
            bail!(
                "QEMU and the rest are installed for the next start of the computer: restart \
                 it, then press Try again"
            );
        }
        return Ok(());
    }
    let mut args = vec!["install", "-y"];
    args.extend(&missing);
    command("dnf", &args, dry_run)
}

/// Whether a program in [`TOOLS`] is missing from where systemd finds it.
fn tools_missing() -> bool {
    TOOLS.iter().any(|tool| {
        !SERVICE_PATH
            .iter()
            .any(|dir| is_program(&Path::new(dir).join(tool)))
    })
}

/// Looks at what environments need from the machine. The service installs
/// and runs without it, so what is missing is a warning for the user to act
/// on.
fn check(warning: &mut dyn FnMut(String)) {
    if !Path::new("/dev/kvm").exists() {
        warning(
            "/dev/kvm is missing: turn on virtualization (AMD SVM or Intel VT-x) \
             in the firmware settings"
                .into(),
        );
    }
    if fs::read_dir("/sys/kernel/iommu_groups").map_or(0, Iterator::count) == 0 {
        warning(
            "there are no IOMMU groups, so no GPU can be handed to an environment: \
             turn on the IOMMU (AMD-Vi or Intel VT-d) in the firmware settings"
                .into(),
        );
    }
    if !vfio_pci_available() {
        warning(
            "the kernel has no vfio-pci module, so no GPU can be handed to an environment".into(),
        );
    }
    for tool in TOOLS {
        if !SERVICE_PATH
            .iter()
            .any(|dir| is_program(&Path::new(dir).join(tool)))
        {
            warning(format!("{tool} is not installed"));
        }
    }
    for firmware in [OVMF_CODE, OVMF_VARS_TEMPLATE] {
        if !Path::new(firmware).is_file() {
            warning(format!("the UEFI firmware {firmware} is missing"));
        }
    }
}

/// Whether the kernel has vfio-pci: loaded, built in, or a module it can
/// load.
fn vfio_pci_available() -> bool {
    Path::new("/sys/module/vfio_pci").exists()
        || Command::new("modprobe")
            .args(["--dry-run", "--quiet", "vfio-pci"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

fn is_program(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Copies the service and guest programs from the directory this program
/// runs from into [`PROGRAM_DIR`]. Each takes the place of the one there in
/// one step, so the running service keeps its program until it restarts.
fn install_programs(warning: &mut dyn FnMut(String), dry_run: bool) -> Result<()> {
    let exe = std::env::current_exe().context("finding this program")?;
    let from = exe.parent().context("this program has no directory")?;
    for name in PROGRAMS {
        let source = from.join(name);
        if !source.is_file() {
            bail!(
                "{} is missing: install runs from a directory that holds {}",
                source.display(),
                PROGRAMS.join(" and ")
            );
        }
    }
    let dir = Path::new(PROGRAM_DIR);
    if !dry_run {
        make_dir(dir)?;
        // The app looks for the installed service here as its user.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("opening {} to everyone", dir.display()))?;
    }
    for name in PROGRAMS {
        let (source, target) = (from.join(name), dir.join(name));
        info!("{} -> {}", source.display(), target.display());
        if !dry_run {
            replace(&target, 0o755, |temp| fs::copy(&source, temp).map(drop))?;
        }
    }
    relabel(&[dir], warning, dry_run);
    Ok(())
}

/// Writes the unit, the polkit action, the sysusers entry and, unless
/// there are settings already, the default settings; then waits until
/// polkit knows the action.
fn install_files(warning: &mut dyn FnMut(String), dry_run: bool) -> Result<()> {
    let unit = unit_running(&Path::new(PROGRAM_DIR).join(PROGRAMS[0]))?;
    put(Path::new(UNIT), &unit, dry_run)?;
    let policy = install_policy(dry_run)?;
    put(Path::new(SYSUSERS), SYSUSERS_TEXT, dry_run)?;
    let mut written = vec![PathBuf::from(UNIT), policy.clone(), PathBuf::from(SYSUSERS)];
    let settings = Path::new(daemon::CONFIG);
    if settings.exists() {
        info!("keeping the settings in {}", settings.display());
    } else {
        put(settings, &default_settings(), dry_run)?;
        written.push(settings.to_owned());
    }
    let written: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
    relabel(&written, warning, dry_run);
    await_policy(&policy, warning, dry_run)
}

/// The settings a new machine starts with: [`SETTINGS_TEXT`] with the GPU
/// this machine has in place of the example one.
fn default_settings() -> String {
    let Some(gpu) = crate::gpu::default_gpu(&gse_host::Root::system()) else {
        return SETTINGS_TEXT.to_owned();
    };
    info!("the new settings name the GPU {gpu}");
    SETTINGS_TEXT
        .lines()
        .map(|line| {
            if line.starts_with("gpu = ") {
                format!("gpu = \"{gpu}\"")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Waits until polkit knows the gse action, without which the service
/// serves nobody but root. polkit reads its actions again when a policy
/// file appears in a directory it watches, and it watches a directory that
/// was missing when it started only once it finds it there, some seconds
/// later; until it knows the action, the file is put in place again.
fn await_policy(policy: &Path, warning: &mut dyn FnMut(String), dry_run: bool) -> Result<()> {
    info!("waiting for polkit to know {}", daemon::ACTION);
    if dry_run {
        return Ok(());
    }
    let deadline = Instant::now() + POLKIT_TIMEOUT;
    let mut put_at = Instant::now();
    loop {
        match polkit_knows(daemon::ACTION) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => {
                warning(format!(
                    "pkaction could not tell whether polkit knows the action: {e}"
                ));
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "polkit does not know the action {} in {} (polkit reads \
                 /etc/polkit-1/actions from version 126 on)",
                daemon::ACTION,
                policy.display()
            );
        }
        sleep(Duration::from_millis(500));
        if put_at.elapsed() >= POLKIT_NUDGE {
            replace(policy, 0o644, |temp| fs::write(temp, POLICY_TEXT))?;
            put_at = Instant::now();
        }
    }
}

/// Whether polkit knows `action`.
fn polkit_knows(action: &str) -> std::io::Result<bool> {
    Command::new("pkaction")
        .args(["--action-id", action])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
}

/// The unit `dist/systemd/gse.service` with its `ExecStart` running
/// `program`. The dist unit stays as packagers install it.
fn unit_running(program: &Path) -> Result<String> {
    let mut unit = String::new();
    let mut starts = 0;
    for line in UNIT_TEXT.lines() {
        let line = match line.strip_prefix("ExecStart=") {
            Some(command) => {
                starts += 1;
                match command.split_once(' ') {
                    Some((_, arguments)) => format!("ExecStart={} {arguments}", program.display()),
                    None => format!("ExecStart={}", program.display()),
                }
            }
            None => line.to_owned(),
        };
        unit.push_str(&line);
        unit.push('\n');
    }
    if starts != 1 {
        bail!("the built-in unit has {starts} ExecStart lines, not one");
    }
    Ok(unit)
}

/// Puts the polkit action into the first of [`POLICY_DIRS`] that takes it,
/// and removes a copy an earlier install left in the other.
fn install_policy(dry_run: bool) -> Result<PathBuf> {
    let [usr, etc] = POLICY_DIRS.map(|dir| Path::new(dir).join(POLICY));
    if dry_run {
        info!(
            "writing {}, or {} where /usr is read-only",
            usr.display(),
            etc.display()
        );
        return Ok(usr);
    }
    let (placed, other) = match put(&usr, POLICY_TEXT, false) {
        Ok(()) => (usr, etc),
        Err(e) => {
            info!("{e:#}; the action goes to {} instead", etc.display());
            put(&etc, POLICY_TEXT, false)?;
            (etc, usr)
        }
    };
    if other.exists() {
        fs::remove_file(&other).ok();
    }
    Ok(placed)
}

/// Writes `text` to `path` as a file everyone may read.
fn put(path: &Path, text: &str, dry_run: bool) -> Result<()> {
    info!("writing {}", path.display());
    if dry_run {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        make_dir(parent)?;
    }
    replace(path, 0o644, |temp| fs::write(temp, text))
}

/// Puts the file `fill` writes at `path` with `mode`, in one step: it is
/// written next to `path` and renamed over it, so nothing ever sees a part
/// of it, and a running program keeps the file it started from.
fn replace(path: &Path, mode: u32, fill: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<()> {
    let mut name = path.file_name().context("file path")?.to_owned();
    name.push(".new");
    let temp = path.with_file_name(name);
    let write = || -> std::io::Result<()> {
        fill(&temp)?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        fs::File::open(&temp)?.sync_all()?;
        fs::rename(&temp, path)
    };
    write().map_err(|e| {
        fs::remove_file(&temp).ok();
        anyhow::Error::new(e).context(format!("writing {}", path.display()))
    })
}

/// Creates `dir` and whatever is missing above it with mode 0755: the
/// daemons that read gse's files run as other users, whatever umask the
/// install runs with.
fn make_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        make_dir(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("creating {}", dir.display())),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("creating {}", dir.display())),
    }
}

/// Gives `paths`, and what they hold, the SELinux labels the policy names
/// for them; systemd starts the service only from a program labelled as
/// one it may run. A machine without `restorecon` has no SELinux to
/// satisfy.
fn relabel(paths: &[&Path], warning: &mut dyn FnMut(String), dry_run: bool) {
    let shown: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let shown = shown.join(" ");
    info!("restorecon -R {shown}");
    if dry_run {
        return;
    }
    let outcome = Command::new("restorecon")
        .arg("-R")
        .args(paths)
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .status();
    match outcome {
        Ok(status) if status.success() => {}
        Ok(status) => warning(format!("restorecon could not label {shown} ({status})")),
        Err(e) if e.kind() == ErrorKind::NotFound => info!("no restorecon, so nothing to label"),
        Err(e) => warning(format!("restorecon: {e}")),
    }
}

/// Creates the account passt runs as, from the sysusers entry.
fn create_account(dry_run: bool) -> Result<()> {
    command("systemd-sysusers", &[SYSUSERS], dry_run)?;
    if !dry_run && !account_exists(NET_USER) {
        bail!("systemd-sysusers did not create the account {NET_USER}");
    }
    Ok(())
}

fn account_exists(name: &str) -> bool {
    Command::new("getent")
        .args(["passwd", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Enables the service and starts it on the new program, then waits until
/// it answers. A service that runs restarts, once any environment it runs
/// has stopped and given its GPU back.
fn start_service(dry_run: bool) -> Result<()> {
    command("systemctl", &["daemon-reload"], dry_run)?;
    command("systemctl", &["enable", UNIT_NAME], dry_run)?;
    stop_environment(dry_run);
    command("systemctl", &["restart", UNIT_NAME], dry_run)?;
    if dry_run {
        info!("waiting for the service to answer");
        return Ok(());
    }
    wait_for_service()
}

/// Runs `program` with `args`, failing when it fails. Its output goes to
/// stderr with the logs, since stdout carries the report.
fn command(program: &str, args: &[&str], dry_run: bool) -> Result<()> {
    info!("{program} {}", args.join(" "));
    if dry_run {
        return Ok(());
    }
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .status()
        .with_context(|| format!("starting {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

/// Waits until the service answers on its socket with this program's
/// protocol version, for up to [`START_TIMEOUT`].
fn wait_for_service() -> Result<()> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let problem = match hello() {
            Ok(version) if version == VERSION => return Ok(()),
            Ok(version) => bail!(
                "the service speaks protocol version {version}, not {VERSION}: a drop-in in \
                 {UNIT}.d may start another program"
            ),
            Err(e) => format!("{e:#}"),
        };
        let failed = Command::new("systemctl")
            .args(["is-failed", "--quiet", UNIT_NAME])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if failed || Instant::now() >= deadline {
            bail!("the service does not answer ({problem}); `journalctl -u {UNIT_NAME}` says why");
        }
        sleep(Duration::from_millis(500));
    }
}

/// The protocol version the service on its socket answers with.
fn hello() -> Result<u32> {
    let mut stream = UnixStream::connect(gse_protocol::SOCKET)
        .with_context(|| format!("connecting to {}", gse_protocol::SOCKET))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    send(&mut stream, &Request::Hello)?;
    match receive::<Reply>(&mut BufReader::new(stream))? {
        Some(Reply::Hello { version, .. }) => Ok(version),
        other => bail!("it answered {other:?}"),
    }
}

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
    // A service that hangs must not hold up what comes after.
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
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
    run("systemctl", &["disable", "--now", UNIT_NAME], dry_run);
    remove(Path::new(&format!("{UNIT}.d")), dry_run);
    remove(Path::new(UNIT), dry_run);
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
    if account_exists(NET_USER) {
        run("userdel", &[NET_USER], dry_run);
    }
    for file in FILES {
        remove(Path::new(file), dry_run);
    }
    info!("gse is removed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_lines_stay_on_one_line() {
        assert_eq!(line("step", Step::Check, None), "step check");
        assert_eq!(line("done", Step::Programs, None), "done programs");
        assert_eq!(
            line(
                "failed",
                Step::Files,
                Some("writing /etc/gse/gse.toml:\nRead-only file system\r\n")
            ),
            "failed files writing /etc/gse/gse.toml: Read-only file system"
        );
        assert_eq!(
            line("warning", Step::Check, Some("passt\tis not installed")),
            "warning check passt is not installed"
        );
        let ids: Vec<&str> = [
            Step::Check,
            Step::Programs,
            Step::Files,
            Step::Account,
            Step::Service,
        ]
        .map(Step::id)
        .to_vec();
        assert_eq!(ids, ["check", "programs", "files", "account", "service"]);
    }

    #[test]
    fn the_unit_runs_the_installed_program() {
        let unit = unit_running(&Path::new(PROGRAM_DIR).join("gse-service")).unwrap();
        assert!(unit.contains("\nExecStart=/usr/local/libexec/gse/gse-service daemon\n"));
        assert_eq!(unit.matches("ExecStart=").count(), 1);
        assert_eq!(unit.lines().count(), UNIT_TEXT.lines().count());
        assert!(unit.contains("\nRestart=always\n"));
        assert!(unit.ends_with("WantedBy=multi-user.target\n"));
        // Every other line is the dist unit's.
        for (installed, dist) in unit.lines().zip(UNIT_TEXT.lines()) {
            assert!(installed == dist || dist.starts_with("ExecStart="));
        }
    }

    #[test]
    fn the_built_in_files_fit_the_service() {
        assert!(POLICY_TEXT.contains(&format!("<action id=\"{}\">", daemon::ACTION)));
        assert!(SYSUSERS_TEXT.contains(&format!("u {NET_USER} ")));
        let settings: daemon::Config = toml::from_str(SETTINGS_TEXT).unwrap();
        assert_eq!(settings.fedora_release, None);
        assert_eq!(settings.dev_ssh_port, None);
        assert!(PROGRAMS.contains(&"gse-service"));
        assert!(DIRECTORIES.contains(&PROGRAM_DIR));
    }

    #[test]
    fn files_are_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gse.conf");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        replace(&path, 0o644, |temp| fs::write(temp, "new")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let program = dir.path().join("gse-guest");
        replace(&program, 0o755, |temp| fs::copy(&path, temp).map(drop)).unwrap();
        assert_eq!(
            fs::metadata(&program).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let failed = replace(&path, 0o644, |_| Err(ErrorKind::Other.into()));
        assert!(failed.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["gse-guest", "gse.conf"]);
    }

    #[test]
    fn directories_are_made_open_to_everyone() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("polkit-1/actions");
        make_dir(&deep).unwrap();
        for made in [dir.path().join("polkit-1"), deep.clone()] {
            assert_eq!(
                fs::metadata(&made).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        make_dir(&deep).unwrap();
        put(&deep.join(POLICY), POLICY_TEXT, false).unwrap();
        assert_eq!(fs::read_to_string(deep.join(POLICY)).unwrap(), POLICY_TEXT);
    }
}
