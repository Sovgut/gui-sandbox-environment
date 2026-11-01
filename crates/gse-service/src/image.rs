//! Building environment images.
//!
//! An image is built inside a VM without a GPU, from an existing image (the
//! Fedora Cloud image for the base, the base image for a preset) and a
//! cloud-init seed carrying the provisioning script. The build VM uses the environment
//! network, so it reaches package mirrors and nothing local. Its verdict comes
//! back on the serial console; a passing build is flattened into one
//! compressed qcow2 that no longer depends on the image it was built from.

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use gse_host::preset::Preset;
use gse_host::seed;
use gse_host::vm::{Network, VmSpec};
use tracing::info;

use crate::{state_path, vm};

const VERDICT: &str = "GSE-BUILD: ";
/// The step reported once the guest passed, while its disk is flattened
/// into the image.
const FLATTEN: &str = "flatten";
/// The guest-side program, installed by the provisioning script from the seed.
const GUEST_BIN: &str = "gse-guest";
/// Room for the driver build and the desktop; the environment's own overlay grows on
/// top of this.
const BUILD_DISK: &str = "40G";

/// How a build runs and where it reports.
pub struct Job<'a> {
    /// Set to stop the build; the image is not written then.
    pub stop: &'a AtomicBool,
    /// The account the build VM's QEMU drops to.
    pub run_as: Option<String>,
    /// Receives each provisioning step (an `=== <step>` line of the guest)
    /// as it reaches the console.
    pub steps: Option<Sender<String>>,
}

impl<'a> Job<'a> {
    /// A build from the terminal, run for the user who ran sudo or pkexec.
    fn invoked(stop: &'a AtomicBool) -> Self {
        Self {
            stop,
            run_as: vm::Owner::invoking().map(|o| o.name),
            steps: None,
        }
    }
}

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
    /// The `gse-guest` binary to install in the image; by default the one
    /// next to this program.
    #[arg(long)]
    guest_bin: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct BuildPresetArgs {
    /// The base image to build on.
    #[arg(long)]
    base: PathBuf,
    /// The preset file (TOML).
    #[arg(long)]
    preset: PathBuf,
    /// Where to write the preset image.
    out: PathBuf,
}

pub fn build_base(args: &BuildBaseArgs, stop: &AtomicBool, dry_run: bool) -> Result<()> {
    let key = args
        .dev_ssh_key
        .as_ref()
        .map(|p| fs::read_to_string(p).with_context(|| format!("key {}", p.display())))
        .transpose()?;
    let guest = guest_program(args.guest_bin.as_deref())?;
    build_base_image(
        &args.from,
        &args.out,
        guest,
        key.as_deref(),
        &Job::invoked(stop),
        dry_run,
    )
}

/// The `gse-guest` program a base image gets: the file at `path`, by
/// default the one next to this program (see [`working_tree_guest`] for a
/// development build).
pub fn guest_program(path: Option<&Path>) -> Result<Vec<u8>> {
    let path = if let Some(path) = path {
        path.to_owned()
    } else {
        let beside = std::env::current_exe()?.with_file_name(GUEST_BIN);
        if beside.exists() {
            beside
        } else {
            working_tree_guest().unwrap_or(beside)
        }
    };
    fs::read(&path).with_context(|| format!("guest binary {}", path.display()))
}

/// A development build of the service, run from a copy outside the working
/// tree, takes the guest program from the tree it was built in: the
/// release build, else the debug one. A release build of the service never
/// looks there.
fn working_tree_guest() -> Option<std::path::PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    ["release", "debug"]
        .iter()
        .map(|profile| target.join(profile).join(GUEST_BIN))
        .find(|path| path.exists())
}

/// The public key a development build of the service puts into the base
/// images it builds when the machine forwards a dev SSH port: the one the
/// working tree keeps for its test VM. A release build never looks there.
pub fn working_tree_dev_key() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let key = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.working/vm/proto/id_ed25519.pub");
    fs::read_to_string(key).ok()
}

/// What makes a base image what it is: the guest program `guest` and the
/// provisioning script, as a short hex digest. A base image built from
/// another pair lacks what this service puts into images.
pub fn base_fingerprint(guest: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(guest);
    hasher.update(seed::BASE_PROVISION.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(16)
        .fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// Builds the base image `out` from the Fedora Cloud image `from`, with
/// `guest` as the guest-side program. With `dev_ssh_key`, the image keeps
/// SSH and a `dev` account with that key.
pub fn build_base_image(
    from: &Path,
    out: &Path,
    guest: Vec<u8>,
    dev_ssh_key: Option<&str>,
    job: &Job<'_>,
    dry_run: bool,
) -> Result<()> {
    let user_data = seed::cloud_config(seed::BASE_PROVISION, dev_ssh_key);
    build(
        from,
        out,
        &user_data,
        &[(GUEST_BIN.to_owned(), guest)],
        job,
        dry_run,
    )
}

/// Installs a preset on the base image. The preset's AppImages travel on the
/// seed volume.
pub fn build_preset(args: &BuildPresetArgs, stop: &AtomicBool, dry_run: bool) -> Result<()> {
    let text = fs::read_to_string(&args.preset)
        .with_context(|| format!("preset {}", args.preset.display()))?;
    let preset = Preset::parse(&text)?;
    let mut appimages = Vec::new();
    for (name, path) in preset.seed_files() {
        let file = fs::File::open(&path).with_context(|| format!("AppImage {}", path.display()))?;
        appimages.push((name, file));
    }
    build_preset_image(
        &preset,
        appimages,
        &args.base,
        &args.out,
        &Job::invoked(stop),
        dry_run,
    )
}

/// Installs `preset` on the image `base` into `out`. `appimages` are the
/// preset's AppImages, opened, under their seed file names.
pub fn build_preset_image(
    preset: &Preset,
    appimages: Vec<(String, fs::File)>,
    base: &Path,
    out: &Path,
    job: &Job<'_>,
    dry_run: bool,
) -> Result<()> {
    let mut files = Vec::new();
    for (name, mut file) in appimages {
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .with_context(|| format!("reading the AppImage for {name}"))?;
        files.push((name, data));
    }
    info!(
        "preset '{}': {} applications",
        preset.name,
        preset.apps.len()
    );
    // No accounts: the base image already has every account it needs.
    let user_data = seed::cloud_config(&preset.script()?, None);
    build(base, out, &user_data, &files, job, dry_run)
}

/// Builds `out` from `from` by booting it once with a seed carrying
/// `user_data` and `files`, then flattening the result.
fn build(
    from: &Path,
    out: &Path,
    user_data: &str,
    files: &[(String, Vec<u8>)],
    job: &Job<'_>,
    dry_run: bool,
) -> Result<()> {
    let from = fs::canonicalize(from).with_context(|| format!("base image {}", from.display()))?;
    let out = absolute(out)?;
    if out.exists() {
        bail!("{} already exists", out.display());
    }
    let stem = out
        .file_stem()
        .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
    let work = out.with_file_name(format!(".build-{stem}"));

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
        screen: None,
        input_socket: None,
        agent_socket: None,
        monitor_socket: state_path("build.monitor"),
        serial_log: console.clone(),
        run_as: job.run_as.clone(),
        image_build: true,
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

    let _lock = vm::lock()?;
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
    let seed_files: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();
    seed::write_nocloud(
        &seed_img,
        &format!("gse-{stem}-{}", now()),
        user_data,
        &seed_files,
    )?;
    info!(
        "building {}; guest console: {}",
        out.display(),
        console.display()
    );

    let done = Arc::new(AtomicBool::new(false));
    let follower = follow_console(console.clone(), Arc::clone(&done), job.steps.clone());
    let mut watch = vm::Watch {
        owner: None,
        progress: &mut |_| {},
    };
    let outcome = vm::run_networked("build", true, None, job.stop, &mut watch, spec_for);
    done.store(true, Ordering::SeqCst);
    follower.join().ok();
    outcome?;
    if job.stop.load(Ordering::SeqCst) {
        bail!(
            "the build was stopped; its files are kept in {}",
            work.display()
        );
    }

    let verdict = last_verdict(&console)?;
    if verdict != "OK" {
        bail!(
            "the build failed ({verdict}); its files are kept in {}",
            work.display()
        );
    }
    flatten(&overlay, &out, &work, job.stop)?;
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

/// Flattens the built `overlay` into the image `out`; a stop ends it and
/// leaves no image, the build's files kept in `work`.
fn flatten(overlay: &Path, out: &Path, work: &Path, stop: &AtomicBool) -> Result<()> {
    info!("flattening into {}", out.display());
    let flattened = run_stoppable(
        "qemu-img",
        &["convert", "-c", "-O", "qcow2"],
        &[overlay, out],
        stop,
    );
    if stop.load(Ordering::SeqCst) {
        fs::remove_file(out).ok();
        bail!(
            "the build was stopped; its files are kept in {}",
            work.display()
        );
    }
    flattened
}

/// Runs `program` like [`run`], killing it when `stop` is set.
fn run_stoppable(program: &str, before: &[&str], paths: &[&Path], stop: &AtomicBool) -> Result<()> {
    let mut child = Command::new(program)
        .args(before)
        .args(paths)
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("{program} failed ({status})");
            }
            return Ok(());
        }
        if stop.load(Ordering::SeqCst) {
            child.kill().ok();
            child.wait().ok();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Logs the provisioning steps and the verdict as they reach the console,
/// and hands each step's name to `steps`.
fn follow_console(
    console: PathBuf,
    done: Arc<AtomicBool>,
    steps: Option<Sender<String>>,
) -> thread::JoinHandle<()> {
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
                    if let Some(steps) = &steps {
                        if let Some(step) = step_name(text) {
                            steps.send(step.to_owned()).ok();
                        } else if text.ends_with(&format!("{VERDICT}OK")) {
                            steps.send(FLATTEN.to_owned()).ok();
                        }
                    }
                    line.clear();
                }
            }
            thread::sleep(Duration::from_secs(1));
        }
    })
}

/// The step an `=== <step>` console line names. The console also carries
/// the guest's own output, so a step name is kept short and printable.
fn step_name(line: &str) -> Option<&str> {
    let step = line.strip_prefix("=== ")?.trim();
    (!step.is_empty() && step.len() <= 80 && !step.chars().any(char::is_control)).then_some(step)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_come_from_marked_console_lines() {
        assert_eq!(step_name("=== packages"), Some("packages"));
        assert_eq!(step_name("=== nvidia driver "), Some("nvidia driver"));
        assert_eq!(step_name("==="), None);
        assert_eq!(step_name("=== "), None);
        assert_eq!(step_name("[  OK  ] Started cloud-init"), None);
        assert_eq!(step_name(&format!("=== {}", "x".repeat(81))), None);
    }

    #[test]
    fn the_last_verdict_counts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("console.log");
        fs::write(&log, "=== packages\nGSE-BUILD: FAILED 12\nnoise\n").unwrap();
        assert_eq!(last_verdict(&log).unwrap(), "FAILED 12");
        fs::write(&log, "=== cleanup\n\x1b[0mGSE-BUILD: OK\n").unwrap();
        assert_eq!(last_verdict(&log).unwrap(), "OK");
        fs::write(&log, "=== packages\n").unwrap();
        assert!(last_verdict(&log).is_err());
    }
}
