//! `daemon`: the service clients talk to over [`gse_protocol::SOCKET`].
//!
//! Every connection is served on a thread of its own. Who may use the
//! service is polkit's decision: the action `io.github.sovgut.gse.manage`
//! (`dist/polkit-1/actions`) is checked once per connection against the
//! process on the other end, which the kernel names (`SO_PEERCRED`). The
//! user behind that process owns any environment it starts: QEMU runs as
//! them, and the screen memory, input socket and sound go to them.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gse_host::Root;
use gse_host::pci::PciAddress;
use gse_host::preset::Preset as HostPreset;
use gse_protocol::{
    BASE_IMAGE, BaseImage, Build, Downloaded, Environment, Event, FailedBuild, JournalEntry,
    MAX_LINE, Reply, Request, Running, SOCKET, ScreenAccess, ScreenMode, Settings, Stage, receive,
    send, valid_name,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::{antivirus, checks, fedora, gpu, image, mailbox, net, nocow, presets, storage, vm};

/// The polkit action every client needs.
pub const ACTION: &str = "io.github.sovgut.gse.manage";
/// Whether this service has the guest program every base image build needs.
fn has_guest_program() -> bool {
    static HAS_GUEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS_GUEST.get_or_init(|| image::guest_program(None).is_ok())
}

/// The guest driver packs the base image needs for the GPUs the settings
/// name, new environments' and each environment's own, with the drivers
/// chosen for them (see `gse_host::hardware::packs`), and `bluetooth` when
/// an environment takes a Bluetooth adapter plugged in now.
fn base_packs(root: &Root, config: &Config, store: &Store) -> Result<Vec<&'static str>> {
    let defaults = config.settings();
    let mut named = vec![(defaults.gpu.clone(), defaults.gpu_driver.clone())];
    let mut bluetooth = false;
    let adapters: Vec<gse_protocol::UsbId> = gse_host::usb::listed(root)
        .into_iter()
        .filter(|d| d.kind == gse_protocol::UsbKind::Bluetooth)
        .map(|d| d.id)
        .collect();
    named.extend(store.environments().into_iter().map(|(name, _)| {
        let settings = store.settings(&name, &defaults);
        bluetooth |= settings
            .devices
            .usb
            .iter()
            .any(|c| c.take && adapters.contains(&c.id));
        (settings.gpu, settings.gpu_driver)
    }));
    let gpus: Vec<(gse_host::families::Family, Option<String>)> = named
        .into_iter()
        .filter_map(|(gpu, driver)| {
            let gpu = gse_host::pci::Gpu::read(root, gpu.parse().ok()?).ok()?;
            let display = gpu.display();
            Some((
                gse_host::families::family(display.vendor, display.device),
                driver,
            ))
        })
        .collect();
    let chosen: Vec<(gse_host::families::Family, Option<&str>)> =
        gpus.iter().map(|(f, d)| (*f, d.as_deref())).collect();
    let mut packs = gse_host::hardware::packs(&chosen).map_err(anyhow::Error::msg)?;
    if bluetooth {
        packs.push("bluetooth");
    }
    Ok(packs)
}

/// How often the service looks for a sandbox disk to make anew (see
/// [`Service::tend_disks`]), and how long it waits after being stopped.
const DISK_WORK_EVERY: Duration = Duration::from_secs(120);
/// How a start's journal line begins; the time a sandbox last played is
/// read back from it.
const STARTING: &str = "Starting for";
pub const CONFIG: &str = "/etc/gse/gse.toml";
const DATA: &str = "/var/lib/gse";
/// The size past which an environment's journal drops its older lines,
/// keeping at most [`JOURNAL_KEEP`] of the latest within half that size.
const JOURNAL_MAX: usize = 256 * 1024;
const JOURNAL_KEEP: usize = 1000;
/// How many of its latest entries a journal request answers.
const JOURNAL_SHOWN: usize = 500;

/// `/etc/gse/gse.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The GPU environments get.
    #[serde(deserialize_with = "pci_address")]
    pub gpu: PciAddress,
    #[serde(default = "default_screen_mode")]
    pub screen_mode: String,
    #[serde(default = "default_internet")]
    pub internet: bool,
    #[serde(default = "default_memory")]
    pub memory_mib: u32,
    #[serde(default = "default_cpus")]
    pub cpus: u16,
    /// The driver a new environment runs the GPU with; none for its default.
    #[serde(default)]
    pub gpu_driver: Option<String>,
    /// The Fedora release base images are built from; without it, the one
    /// the base provisioning script is made for ([`fedora::RELEASE`]).
    #[serde(default)]
    pub fedora_release: Option<u32>,
    /// Development only: forward 127.0.0.1:PORT to each environment's SSH
    /// port. Only development images run an SSH server.
    #[serde(default)]
    pub dev_ssh_port: Option<u16>,
}

/// The screen a sandbox on `gpu` gets, as its journal says it: `mode` in
/// the app's window, or the monitor's preferred mode when that GPU drives
/// the monitor.
fn screen_words(root: &Root, gpu: &str, mode: &str) -> String {
    let gpu = gpu
        .parse()
        .ok()
        .and_then(|address| gse_host::pci::Gpu::read(root, address).ok());
    match gpu {
        Some(gpu) if gse_host::plan::Mode::for_gpu(&gpu) == gse_host::plan::Mode::Single => gpu
            .monitor_mode()
            .map_or_else(|| "the monitor's own".to_owned(), str::to_owned),
        _ => mode.to_owned(),
    }
}

/// Whether the GPU at `gpu` drives the monitor, so an environment on it
/// takes the monitor and the host session goes.
fn takes_monitor(root: &Root, gpu: PciAddress) -> bool {
    gse_host::pci::Gpu::read(root, gpu)
        .is_ok_and(|gpu| gse_host::plan::Mode::for_gpu(&gpu) == gse_host::plan::Mode::Single)
}

/// How large an environment's /home may grow.
const HOME_SIZE: &str = "512G";

/// Seconds since the Unix epoch.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Runs `qemu-img` with `args`, then `path`, then `after`.
fn qemu_img(args: &[&str], path: &Path, after: &[&str]) -> Result<()> {
    let status = Command::new("qemu-img")
        .args(args)
        .arg(path)
        .args(after)
        .status()
        .context("starting qemu-img")?;
    if !status.success() {
        bail!("qemu-img failed ({status})");
    }
    Ok(())
}

fn pci_address<'de, D: serde::Deserializer<'de>>(from: D) -> Result<PciAddress, D::Error> {
    let text = String::deserialize(from)?;
    text.parse().map_err(serde::de::Error::custom)
}

fn default_screen_mode() -> String {
    "2560x1440@60".into()
}
const fn default_internet() -> bool {
    true
}
const fn default_memory() -> u32 {
    12288
}
const fn default_cpus() -> u16 {
    8
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// The Fedora release base images are built from.
    pub fn fedora_release(&self) -> u32 {
        self.fedora_release.unwrap_or(fedora::RELEASE)
    }

    /// The file [`Config::load`] reads back as `self`.
    fn to_toml(&self) -> String {
        let fedora = self
            .fedora_release
            .map(|release| {
                format!("\n# The Fedora release base images are built from.\nfedora_release = {release}\n")
            })
            .unwrap_or_default();
        let driver = self
            .gpu_driver
            .as_ref()
            .map(|driver| format!("gpu_driver = \"{}\"\n", driver.escape_default()))
            .unwrap_or_default();
        let dev = self
            .dev_ssh_port
            .map(|port| format!("\n# Development only.\ndev_ssh_port = {port}\n"))
            .unwrap_or_default();
        format!(
            "# /etc/gse/gse.toml: the machine's settings for the gse service.\n\
             \n\
             # What a new environment starts with; each environment keeps its\n\
             # own copy. The GPU is named as `gse-service gpu list` shows it,\n\
             # and the screen mode applies when the screen is shown in a window.\n\
             gpu = \"{}\"\n\
             {driver}\
             screen_mode = \"{}\"\n\
             internet = {}\n\
             \n\
             memory_mib = {}\n\
             cpus = {}\n{fedora}{dev}",
            self.gpu,
            self.screen_mode.escape_default(),
            self.internet,
            self.memory_mib,
            self.cpus
        )
    }

    /// Replaces the file at `path` in one step: the new file is written next
    /// to it and renamed over it, so a reader sees the old or the new file,
    /// never a part.
    fn save(&self, path: &Path) -> Result<()> {
        replace_file(path, &self.to_toml())
    }

    fn settings(&self) -> Settings {
        Settings {
            gpu: self.gpu.to_string(),
            screen_mode: self.screen_mode.clone(),
            internet: self.internet,
            gpu_driver: self.gpu_driver.clone(),
            devices: gse_protocol::Devices::default(),
        }
    }
}

/// Replaces the file at `path` with `text` in one step: the new file is
/// written next to it and renamed over it, so a reader sees the old or the
/// new file, never a part. The file is readable by everyone: the app reads
/// the GPUs named in settings to stay off them.
fn replace_file(path: &Path, text: &str) -> Result<()> {
    let mut name = path.file_name().context("file path")?.to_owned();
    name.push(".new");
    let temp = path.with_file_name(name);
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        fs::rename(&temp, path)
    };
    write().map_err(|e| {
        fs::remove_file(&temp).ok();
        anyhow::Error::new(e).context(format!("writing {}", path.display()))
    })
}

/// `settings` as checked against the machine: the GPU must be one of
/// `gpus`, its driver one it can run on (none for its default), and a frame
/// of the screen mode must fit a slot of the screen memory.
fn validate(settings: &Settings, gpus: &[gse_protocol::Gpu]) -> Result<Settings> {
    let address: PciAddress = settings
        .gpu
        .parse()
        .with_context(|| format!("'{}' is not a PCI address", settings.gpu))?;
    let Some(gpu) = gpus.iter().find(|g| g.address == address.to_string()) else {
        bail!("{address} is not a GPU of this machine");
    };
    let gpu_driver = match settings.gpu_driver.as_deref() {
        None => None,
        Some(driver) if gpu.guest_drivers.first().map(String::as_str) == Some(driver) => None,
        Some(driver) if gpu.guest_drivers.iter().any(|d| d == driver) => Some(driver.to_owned()),
        Some(driver) => bail!("a sandbox cannot run {address} with {driver}"),
    };
    let mode: ScreenMode = settings.screen_mode.parse().map_err(anyhow::Error::msg)?;
    let frame = u64::from(mode.width) * u64::from(mode.height) * 4;
    if frame > gse_screen::SLOT_SIZE as u64 {
        bail!(
            "a {}x{} frame does not fit the screen memory",
            mode.width,
            mode.height
        );
    }
    Ok(Settings {
        gpu: address.to_string(),
        screen_mode: mode.to_string(),
        internet: settings.internet,
        gpu_driver,
        devices: settings.devices.clone(),
    })
}

/// Where images and environments live: `base.qcow2`, the base image presets
/// build on; `images/<image>.qcow2`, with `<image>.base`, the base image it
/// was built on, and `<image>.failed`, its last build when that failed
/// (`base.failed` for the base image); `environments/<name>/` with the name
/// of the environment's image, its `settings.toml` and `journal`, and once
/// it has started, its disk, firmware variables and serial log;
/// `presets/<preset>.toml`; `builds/`, where an image or the base image is
/// built before it replaces the one in use; and `cache/`, with the Fedora
/// Cloud image the base image is built from.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }

    /// Where the files of a take of `name` wait while the antivirus looks
    /// at them.
    fn scratch(&self, name: &str) -> PathBuf {
        self.root.join("scan").join(name)
    }

    fn environments_dir(&self) -> PathBuf {
        self.root.join("environments")
    }

    fn presets_dir(&self) -> PathBuf {
        self.root.join("presets")
    }

    fn builds_dir(&self) -> PathBuf {
        self.root.join("builds")
    }

    fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    fn base(&self) -> PathBuf {
        self.root.join("base.qcow2")
    }

    fn image(&self, image: &str) -> PathBuf {
        self.images_dir().join(format!("{image}.qcow2"))
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.environments_dir().join(name)
    }

    #[must_use]
    pub fn disk(&self, name: &str) -> PathBuf {
        self.dir(name).join("disk.qcow2")
    }

    /// The environment's own /home, apart from its system so that a new
    /// image of its set leaves it alone: only a reset empties it.
    pub fn home_disk(&self, name: &str) -> PathBuf {
        self.dir(name).join("home.qcow2")
    }

    fn sorted_names(dir: &Path, keep: impl Fn(&Path) -> Option<String>) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| keep(&e.path()))
            .collect();
        names.sort();
        names
    }

    /// The images that exist; a link to an image that is gone is none.
    pub fn images(&self) -> Vec<String> {
        Self::sorted_names(&self.images_dir(), |p| {
            (p.extension()? == "qcow2" && p.exists())
                .then(|| p.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
    }

    /// Every environment, with the image it was created from.
    pub fn environments(&self) -> Vec<(String, String)> {
        Self::sorted_names(&self.environments_dir(), |p| {
            p.is_dir()
                .then(|| p.file_name()?.to_str().map(str::to_owned))
                .flatten()
        })
        .into_iter()
        .map(|name| {
            let image = fs::read_to_string(self.dir(&name).join("image"))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            (name, image)
        })
        .collect()
    }

    /// A new environment on `image`, which exists or has a preset it will
    /// be built from. Its disk comes with its first start.
    pub fn create(&self, name: &str, image: &str, settings: &Settings) -> Result<()> {
        if !valid_name(name) || !valid_name(image) {
            bail!("names are 1-32 lowercase letters, digits and hyphens");
        }
        if !self.image(image).exists() && !presets::exists(&self.presets_dir(), image) {
            bail!("no image or preset '{image}'");
        }
        let dir = self.dir(name);
        if dir.exists() {
            bail!("'{name}' exists");
        }
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("image"), format!("{image}\n"))?;
        self.set_settings(name, settings)
    }

    /// When `name` was created: its image file is written then, and never
    /// again.
    fn created(&self, name: &str) -> u64 {
        fs::metadata(self.dir(name).join("image"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    }

    /// When `name` last started, by its journal.
    fn last_started(&self, name: &str) -> Option<u64> {
        let journal = fs::read_to_string(self.dir(name).join("journal")).ok()?;
        journal.lines().rev().find_map(|line| {
            serde_json::from_str::<JournalEntry>(line)
                .ok()
                .filter(|e| e.text.starts_with(STARTING))
                .map(|e| e.time)
        })
    }

    /// Makes `name` an environment of the image `image`, renamed: its record
    /// keeps the time it was made, and its disk is pointed at the image
    /// under the new name.
    fn rename_image_of(&self, name: &str, image: &str) -> Result<()> {
        let record = self.dir(name).join("image");
        let made = fs::metadata(&record).and_then(|m| m.modified()).ok();
        fs::write(&record, format!("{image}\n"))?;
        if let Some(made) = made {
            fs::File::options()
                .write(true)
                .open(&record)?
                .set_modified(made)?;
        }
        let disk = self.disk(name);
        if disk.exists() {
            let status = Command::new("qemu-img")
                .args(["rebase", "-u", "-F", "qcow2", "-b"])
                .arg(self.image(image))
                .arg(&disk)
                .status()
                .context("starting qemu-img")?;
            if !status.success() {
                bail!("qemu-img rebase of '{name}' failed ({status})");
            }
        }
        Ok(())
    }

    fn image_of(&self, name: &str) -> Result<String> {
        fs::read_to_string(self.dir(name).join("image"))
            .map(|s| s.trim().to_owned())
            .with_context(|| format!("no environment '{name}'"))
    }

    /// Makes the disk of `name` once its image exists: a fresh overlay on
    /// the image, so every start after a reset begins from the image.
    fn ensure_disk(&self, name: &str) -> Result<()> {
        let home = self.home_disk(name);
        if !home.exists() {
            // Sparse: it takes on the host what the guest writes into it.
            qemu_img(&["create", "-q", "-f", "qcow2"], &home, &[HOME_SIZE])?;
        }
        let disk = self.disk(name);
        if disk.exists() {
            return Ok(());
        }
        let image = self.image_of(name)?;
        let base = self.image(&image);
        if !base.exists() {
            bail!("the image '{image}' is not built yet");
        }
        let status = Command::new("qemu-img")
            .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
            .arg(base)
            .arg(&disk)
            .status()
            .context("starting qemu-img")?;
        if !status.success() {
            bail!("qemu-img failed ({status})");
        }
        Ok(())
    }

    /// Throws away everything written into `name`, its /home included; its
    /// next start begins from its image again.
    pub fn reset(&self, name: &str) -> Result<()> {
        self.renew_system(name)?;
        fs::remove_file(self.home_disk(name)).ok();
        Ok(())
    }

    /// Throws away the system of `name`, which a new image of its set
    /// replaces; its /home stays.
    fn renew_system(&self, name: &str) -> Result<()> {
        self.image_of(name)?;
        let dir = self.dir(name);
        fs::remove_file(self.disk(name)).ok();
        fs::remove_file(dir.join("OVMF_VARS.qcow2")).ok();
        fs::remove_file(dir.join("console.log")).ok();
        Ok(())
    }

    /// The settings of `name`; one created without them gets `defaults`.
    fn settings(&self, name: &str, defaults: &Settings) -> Settings {
        fs::read_to_string(self.dir(name).join("settings.toml"))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_else(|| defaults.clone())
    }

    fn set_settings(&self, name: &str, settings: &Settings) -> Result<()> {
        let text = toml::to_string(settings).context("writing the settings")?;
        replace_file(&self.dir(name).join("settings.toml"), &text)
    }

    /// Adds `text` to the journal of `name`, dropping older lines once the
    /// journal grows past [`JOURNAL_MAX`] bytes.
    fn log(&self, name: &str, text: &str) -> Result<JournalEntry> {
        let entry = JournalEntry {
            time: now(),
            text: text.to_owned(),
        };
        let path = self.dir(name).join("journal");
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(line.as_bytes())?;
        if file.metadata()?.len() > JOURNAL_MAX as u64 {
            let text = fs::read_to_string(&path)?;
            let mut size = 0;
            let mut kept: Vec<&str> = text
                .lines()
                .rev()
                .take(JOURNAL_KEEP)
                .take_while(|line| {
                    size += line.len() + 1;
                    size <= JOURNAL_MAX / 2
                })
                .collect();
            kept.reverse();
            replace_file(&path, &(kept.join("\n") + "\n"))?;
        }
        Ok(entry)
    }

    /// The latest [`JOURNAL_SHOWN`] entries of the journal of `name`.
    fn journal(&self, name: &str) -> Result<Vec<JournalEntry>> {
        self.image_of(name)?;
        let text = fs::read_to_string(self.dir(name).join("journal")).unwrap_or_default();
        let entries: Vec<JournalEntry> = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Ok(entries[entries.len().saturating_sub(JOURNAL_SHOWN)..].to_vec())
    }

    pub fn rename(&self, name: &str, to: &str) -> Result<()> {
        self.image_of(name)?;
        if !valid_name(to) {
            bail!("names are 1-32 lowercase letters, digits and hyphens");
        }
        let target = self.dir(to);
        if target.exists() {
            bail!("'{to}' exists");
        }
        fs::rename(self.dir(name), target)?;
        Ok(())
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let dir = self.dir(name);
        if !valid_name(name) || !dir.is_dir() {
            bail!("no environment '{name}'");
        }
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// Puts the freshly built `built` in place of the image `image`. An
    /// overlay cannot outlive a change of the image under it, so every
    /// environment created from `image` gets its system anew on the new one,
    /// its /home kept; their names come back.
    fn replace_image(&self, image: &str, built: &Path) -> Result<Vec<String>> {
        fs::rename(built, self.image(image))
            .with_context(|| format!("putting the image '{image}' in place"))?;
        // Which base image it stands on, so a newer base names it stale.
        let base = fs::read_to_string(self.base_fingerprint_path()).unwrap_or_default();
        fs::write(self.on_base_path(image), base)
            .with_context(|| format!("recording the base of the image '{image}'"))?;
        let mut reset = Vec::new();
        for (name, _) in self
            .environments()
            .into_iter()
            .filter(|(_, from)| from == image)
        {
            match self.renew_system(&name) {
                Ok(()) => reset.push(name),
                Err(e) => error!("resetting '{name}' onto the new image '{image}': {e:#}"),
            }
        }
        Ok(reset)
    }

    /// Puts the built base image in place, with the fingerprint of what it
    /// was built from and the driver `packs` it holds beside it. Images
    /// built on the old one stand on their own and keep working;
    /// [`Store::stale_images`] names them.
    fn replace_base(&self, built: &Path, packs: &[&str]) -> Result<()> {
        fs::rename(built, self.base()).context("putting the base image in place")?;
        fs::write(self.base_drivers_path(), format!("{}\n", packs.join(" ")))
            .context("writing the base image's drivers")?;
        fs::write(
            self.base_fingerprint_path(),
            format!("{}\n", image::base_fingerprint(packs)),
        )
        .context("writing the base image's fingerprint")
    }

    fn base_fingerprint_path(&self) -> PathBuf {
        self.root.join("base.fingerprint")
    }

    fn base_drivers_path(&self) -> PathBuf {
        self.root.join("base.drivers")
    }

    /// The record of the base image the image `image` was built on.
    fn on_base_path(&self, image: &str) -> PathBuf {
        self.images_dir().join(format!("{image}.base"))
    }

    /// The record of the last build of the image `image`, or of the base
    /// image, when it failed; it stays until the image builds again.
    fn failure_path(&self, image: &str) -> PathBuf {
        self.images_dir().join(format!("{image}.failed"))
    }

    /// Records that the build of `image` failed at `step`, its console kept
    /// at `log`.
    fn record_failure(
        &self,
        image: &str,
        step: Option<String>,
        log: Option<PathBuf>,
    ) -> Result<()> {
        let record = serde_json::to_string(&FailureRecord { step, log })?;
        replace_file(&self.failure_path(image), &record)
    }

    /// Forgets the failed build of `image`, which builds again or is gone.
    fn forget_failure(&self, image: &str) {
        fs::remove_file(self.failure_path(image)).ok();
    }

    /// Removes the image `image` with the records beside it.
    fn forget_image(&self, image: &str) {
        for path in [
            self.image(image),
            self.on_base_path(image),
            self.failure_path(image),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => info!("removed {}", path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!("removing {}: {e}", path.display()),
            }
        }
    }

    /// Whether a preset file names `image`.
    fn has_preset(&self, image: &str) -> bool {
        self.presets_dir().join(format!("{image}.toml")).exists()
    }

    /// Removes the images no preset names and no environment stands on,
    /// left by a set deleted while its image stayed.
    fn prune_images(&self) {
        let used: Vec<String> = self
            .environments()
            .into_iter()
            .map(|(_, image)| image)
            .collect();
        for image in self.images() {
            if !self.has_preset(&image) && !used.contains(&image) {
                self.forget_image(&image);
            }
        }
    }

    /// The last failed build of each of `images` that has one.
    fn failures<'a>(&self, images: impl IntoIterator<Item = &'a str>) -> Vec<FailedBuild> {
        images
            .into_iter()
            .filter_map(|image| {
                let record = fs::read_to_string(self.failure_path(image)).ok()?;
                let FailureRecord { step, log } = serde_json::from_str(&record).ok()?;
                Some(FailedBuild {
                    preset: image.to_owned(),
                    step,
                    log,
                })
            })
            .collect()
    }

    /// The images of presets built on another base image than the one in
    /// place, by the fingerprints recorded at their builds; none while the
    /// base image has no fingerprint.
    fn stale_images(&self) -> Vec<String> {
        let Ok(base) = fs::read_to_string(self.base_fingerprint_path()) else {
            return Vec::new();
        };
        self.images()
            .into_iter()
            .filter(|image| {
                self.has_preset(image)
                    && fs::read_to_string(self.on_base_path(image)).ok().as_deref()
                        != Some(base.as_str())
            })
            .collect()
    }

    /// The base image: when it was built, and whether it is current: built
    /// by this service's provisioning script, with every driver pack of
    /// `needed`. A pack it holds beyond those does no harm, so choosing a
    /// driver the image already has needs no build. Without `needed` (no
    /// guest program to build with) it is taken as it is.
    fn base_image(&self, needed: Option<&[&str]>) -> Option<BaseImage> {
        let built = fs::metadata(self.base())
            .and_then(|m| m.modified())
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let recorded = fs::read_to_string(self.base_fingerprint_path()).ok();
        let drivers = fs::read_to_string(self.base_drivers_path()).unwrap_or_default();
        let held: Vec<&str> = drivers.split_whitespace().collect();
        Some(BaseImage {
            built,
            current: needed.is_none_or(|needed| {
                recorded.is_some_and(|r| r.trim() == image::base_fingerprint(&held))
                    && needed.iter().all(|pack| held.contains(pack))
            }),
        })
    }
}

/// The environment that runs, if any.
struct Current {
    name: String,
    stage: Stage,
    internet: bool,
    owner: u32,
    stop: Arc<AtomicBool>,
    /// Raised when the user asks to leave by the keyboard (one GPU).
    leave: Arc<AtomicBool>,
    /// Whether the service takes the Outbox by itself now, so a leave
    /// waits for its last round.
    collecting: Arc<AtomicBool>,
    screen: ScreenAccess,
    /// The carriers it started with, each with its disk's serial.
    inbox_disks: Vec<(PathBuf, String)>,
}

/// The image build in progress, if any.
struct Building {
    /// The preset, or [`BASE_IMAGE`] for the base image.
    preset: String,
    steps: Vec<String>,
    stop: Arc<AtomicBool>,
    /// When it started, in seconds since the Unix epoch.
    started: u64,
}

/// What [`Store::failure_path`] holds: the step a failed build reached and
/// where its console is kept. The image is the file's name.
#[derive(Serialize, Deserialize)]
struct FailureRecord {
    step: Option<String>,
    log: Option<PathBuf>,
}

/// What a build tells the connection that asked for it.
enum BuildUpdate {
    Step {
        step: String,
        download: Option<Downloaded>,
    },
    Finished(Result<(), String>),
}

struct Shared {
    root: Root,
    config: Config,
    store: Store,
    current: Option<Current>,
    build: Option<Building>,
    watchers: Vec<Sender<Event>>,
    /// The connection to the running environment's agent, made on first use.
    agent: Arc<Mutex<Option<Agent>>>,
    /// The sends in under way, by environment, each with the flag that
    /// stops it.
    sends: HashMap<String, Arc<AtomicBool>>,
    /// A move of the data to another disk under way, with the flag that
    /// stops it; nothing starts or builds meanwhile.
    moving: Option<Arc<AtomicBool>>,
    /// A sandbox's disk being made anew without copy-on-write (see
    /// [`Service::tend_disks`]), with the flag that stops it.
    disk_work: Option<Arc<AtomicBool>>,
}

/// A send in under way; it leaves the service's list when it ends.
struct Send {
    service: Service,
    name: String,
    flag: Arc<AtomicBool>,
}

impl Drop for Send {
    fn drop(&mut self) {
        self.service.lock().sends.remove(&self.name);
    }
}

#[derive(Clone)]
struct Service(Arc<Mutex<Shared>>);

impl Service {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn broadcast(&self, event: &Event) {
        self.lock()
            .watchers
            .retain(|w| w.send(event.clone()).is_ok());
    }

    /// Where the data is kept and moving it.
    fn handle_storage(&self, request: Request, out: &mut UnixStream) -> Result<()> {
        match request {
            Request::Storage => {
                let usage = storage::usage()?;
                send(
                    out,
                    &Reply::Storage {
                        path: usage.path,
                        used: usage.used,
                        free: usage.free,
                    },
                )?;
            }
            Request::MoveStorage { to } => self.move_storage(&to, out)?,
            Request::CancelMove => {
                let shared = self.lock();
                let stop = shared.moving.as_ref().context("nothing is being moved")?;
                stop.store(true, Ordering::SeqCst);
                info!("stopping the move of the sandboxes");
            }
            _ => unreachable!("not a storage request"),
        }
        Ok(())
    }

    /// Moves the data to the disk of `to` while nothing runs or builds,
    /// telling the client on `out` how far it got.
    fn move_storage(&self, to: &Path, out: &mut UnixStream) -> Result<()> {
        let destination = storage::check(to)?;
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut shared = self.lock();
            if let Some(current) = &shared.current {
                bail!("'{}' is running; stop it first", current.name);
            }
            if let Some(build) = &shared.build {
                bail!("the image '{}' is building; wait for it", build.preset);
            }
            if shared.moving.is_some() {
                bail!("the sandboxes are being moved already");
            }
            shared.moving = Some(Arc::clone(&stop));
        }
        mailbox::yield_to_others();
        let mut last: Option<Instant> = None;
        let moved = storage::move_to(
            &destination,
            &mut |done, total| {
                if done >= total || last.is_none_or(|at| at.elapsed() >= SENDING_EVERY) {
                    last = Some(Instant::now());
                    send(out, &Reply::Moving { done, total }).ok();
                }
            },
            &stop,
        );
        self.lock().moving = None;
        moved
    }

    /// Notes a send in to `name`, which [`Request::CancelTransfer`] can
    /// stop; one at a time per environment.
    fn start_send(&self, name: &str) -> Result<Send> {
        let flag = Arc::new(AtomicBool::new(false));
        let mut shared = self.lock();
        if shared.sends.contains_key(name) {
            bail!("files are being sent into '{name}' already");
        }
        shared.sends.insert(name.to_owned(), flag.clone());
        Ok(Send {
            service: self.clone(),
            name: name.to_owned(),
            flag,
        })
    }

    /// Adds `text` to the journal of `name` and tells the watchers.
    fn log(&self, name: &str, text: impl AsRef<str>) {
        let store = self.lock().store.clone();
        match store.log(name, text.as_ref()) {
            Ok(entry) => self.broadcast(&Event::Journal {
                name: name.to_owned(),
                entry,
            }),
            Err(e) => warn!("journal of '{name}': {e:#}"),
        }
    }

    fn list(&self) -> Vec<Environment> {
        let shared = self.lock();
        let defaults = shared.config.settings();
        shared
            .store
            .environments()
            .into_iter()
            .map(|(name, image)| {
                let running = shared
                    .current
                    .as_ref()
                    .filter(|c| c.name == name)
                    .map(|c| Running {
                        stage: c.stage,
                        internet: c.internet,
                        screen: Some(c.screen.clone()),
                    });
                Environment {
                    image_ready: shared.store.image(&image).exists(),
                    created: shared.store.created(&name),
                    last_started: shared.store.last_started(&name),
                    settings: shared.store.settings(&name, &defaults),
                    name,
                    image,
                    running,
                }
            })
            .collect()
    }

    /// Gives the preset `name` the name `to`: its file, its image, and the
    /// environments made from it, whose disks are pointed at the image under
    /// its new name.
    fn rename_preset(&self, name: &str, to: &str) -> Result<()> {
        let shared = self.lock();
        if let Some(current) = &shared.current {
            bail!(
                "'{}' is running; sets are renamed once it stops",
                current.name
            );
        }
        if let Some(build) = &shared.build {
            bail!(
                "'{}' is building; sets are renamed once it ends",
                build.preset
            );
        }
        let store = shared.store.clone();
        if store.image(to).exists() {
            bail!("an image '{to}' exists");
        }
        presets::rename(&store.presets_dir(), name, to)?;
        if store.image(name).exists() {
            fs::rename(store.image(name), store.image(to))
                .with_context(|| format!("renaming the image of '{name}'"))?;
            fs::rename(store.on_base_path(name), store.on_base_path(to)).ok();
        }
        fs::rename(store.failure_path(name), store.failure_path(to)).ok();
        let users: Vec<String> = store
            .environments()
            .into_iter()
            .filter(|(_, image)| image == name)
            .map(|(environment, _)| environment)
            .collect();
        drop(shared);
        for environment in &users {
            store.rename_image_of(environment, to)?;
            self.log(
                environment,
                format!("Its set of apps {name} is now called {to}"),
            );
        }
        self.broadcast(&Event::PresetsChanged);
        self.broadcast(&Event::Changed);
        Ok(())
    }

    /// Makes the disks of sandboxes that still copy on write anew without
    /// it, one at a time, while nothing runs, builds or moves: such a disk,
    /// made before its folder was marked, writes several times slower on
    /// btrfs. A thread looks for one every [`DISK_WORK_EVERY`]; any request
    /// that changes something stops the copy under way
    /// ([`Service::pause_disk_work`]), which then begins again later.
    fn tend_disks(&self) {
        let service = self.clone();
        thread::spawn(move || {
            let mut skipped = std::collections::HashSet::new();
            loop {
                thread::sleep(DISK_WORK_EVERY);
                let claimed = {
                    let mut shared = service.lock();
                    let idle = shared.current.is_none()
                        && shared.build.is_none()
                        && shared.moving.is_none()
                        && shared.sends.is_empty()
                        && shared.disk_work.is_none();
                    let next = shared
                        .store
                        .environments()
                        .into_iter()
                        .flat_map(|(name, _)| {
                            let dir = shared.store.dir(&name);
                            [shared.store.disk(&name), dir.join("home.qcow2")]
                                .map(|disk| (name.clone(), disk))
                        })
                        .find(|(_, disk)| {
                            disk.is_file() && !skipped.contains(disk) && !nocow::is_set(disk)
                        });
                    match next.filter(|_| idle) {
                        Some((name, disk)) => {
                            let stop = Arc::new(AtomicBool::new(false));
                            shared.disk_work = Some(Arc::clone(&stop));
                            Some((name, disk, stop))
                        }
                        None => None,
                    }
                };
                let Some((name, disk, stop)) = claimed else {
                    continue;
                };
                info!(
                    "{name}: making {} anew without copy-on-write",
                    disk.display()
                );
                mailbox::yield_to_others();
                let outcome = nocow::rewrite(&disk, &stop);
                service.lock().disk_work = None;
                match outcome {
                    Ok(true) => service.log(
                        &name,
                        "Its disk was rewritten for faster writes (no copy-on-write on the host)",
                    ),
                    // No mark to give, or no room: the same next time, so
                    // this disk is left alone until the service starts again.
                    Ok(false) => {
                        info!("{name}: {} stays as it is", disk.display());
                        skipped.insert(disk);
                    }
                    Err(e) => info!("{name}: {} was not made anew: {e:#}", disk.display()),
                }
            }
        });
    }

    /// Stops the disk work under way and waits for it to let go of its
    /// file, before anything that may touch a sandbox's disks.
    fn pause_disk_work(&self) {
        let Some(stop) = self.lock().disk_work.clone() else {
            return;
        };
        stop.store(true, Ordering::SeqCst);
        for _ in 0..600 {
            if self.lock().disk_work.is_none() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        warn!("the disk work did not stop in time");
    }

    /// Starts `name` for `owner`; progress goes to `progress` and to every
    /// watcher, and a start that fails ends `progress` with the reason.
    #[allow(clippy::too_many_lines)] // one start, from the checks to the VM's end
    fn start(
        &self,
        name: &str,
        keyboard: Option<gse_protocol::Keyboard>,
        owner: u32,
        progress: Sender<Result<Stage, String>>,
    ) -> Result<()> {
        // Files a service before carriers left waiting as plain files go in
        // by disk as well; on btrfs the copy only shares their blocks.
        if self.lock().current.is_none()
            && let Ok(dir) = self.environment_dir(name)
        {
            match mailbox::pack_loose(&dir) {
                Ok(0) => {}
                Ok(count) => info!("{name}: {count} waiting file(s) packed to go in by disk"),
                Err(e) => warn!("{name}: packing the waiting files: {e:#}"),
            }
        }
        let (launch, stop, root) = {
            let mut shared = self.lock();
            if shared.moving.is_some() {
                bail!("the sandboxes are being moved to another disk");
            }
            if let Some(current) = &shared.current {
                bail!("'{}' is running", current.name);
            }
            if let Some(build) = &shared.build {
                bail!("the image '{}' is building", build.preset);
            }
            shared.store.ensure_disk(name)?;
            let settings = shared.store.settings(name, &shared.config.settings());
            let gpu: PciAddress = settings
                .gpu
                .parse()
                .with_context(|| format!("'{}' is not a PCI address", settings.gpu))?;
            let leave = Arc::new(AtomicBool::new(false));
            let inbox_disks: Vec<PathBuf> = mailbox::carriers(&shared.store.dir(name))
                .into_iter()
                .take(gse_protocol::carrier::MAX_DISKS)
                .collect();
            let launch = vm::Launch {
                name: name.to_owned(),
                disk: shared.store.disk(name),
                read_only_disks: Vec::new(),
                inbox_disks: inbox_disks.clone(),
                devices: settings.devices.clone(),
                gpu,
                memory_mib: shared.config.memory_mib,
                cpus: shared.config.cpus,
                internet: settings.internet,
                screen_mode: settings.screen_mode.clone(),
                gpu_driver: gpu::guest_driver_item(
                    &shared.root,
                    gpu,
                    settings.gpu_driver.as_deref(),
                ),
                owner: vm::Owner::from_uid(owner)?,
                dev_ssh_port: shared.config.dev_ssh_port,
                dev_vnc: None,
                keyboard,
                leave: Some(Arc::clone(&leave)),
            };
            let stop = Arc::new(AtomicBool::new(false));
            shared.current = Some(Current {
                name: name.to_owned(),
                stage: Stage::Preparing,
                internet: settings.internet,
                owner,
                stop: Arc::clone(&stop),
                leave: Arc::clone(&leave),
                collecting: Arc::new(AtomicBool::new(false)),
                screen: ScreenAccess {
                    memory: launch.screen_memory(),
                    input: launch.input_socket(),
                },
                inbox_disks: inbox_disks
                    .into_iter()
                    .enumerate()
                    .map(|(i, path)| (path, format!("{}{i}", gse_protocol::carrier::SERIAL)))
                    .collect(),
            });
            (launch, stop, shared.root.clone())
        };
        self.stop_on_early_leave(name);

        self.log(
            name,
            format!(
                "{STARTING} {} on GPU {}, screen {}, internet {}",
                launch.owner.name,
                launch.gpu,
                screen_words(&root, &launch.gpu.to_string(), &launch.screen_mode),
                on_off(launch.internet)
            ),
        );
        let single = takes_monitor(&root, launch.gpu);
        let wanted = gse_host::usb::for_guest(&root, &launch.devices, single);
        if !wanted.is_empty() && !vm::qemu_has_device("usb-host") {
            self.log(
                name,
                format!(
                    "{} stay with the host: QEMU lacks its USB module \
                     (qemu-device-usb-host); the app's Repair puts it back",
                    wanted
                        .iter()
                        .map(|d| d.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        let service = self.clone();
        thread::spawn(move || {
            let name = launch.name.clone();
            let outcome = vm::launch_environment(
                &root,
                &launch,
                &stop,
                &mut |stage| {
                    if let Some(current) = service.lock().current.as_mut() {
                        current.stage = stage;
                    }
                    progress.send(Ok(stage)).ok();
                    service.broadcast(&Event::Progress {
                        name: name.clone(),
                        stage,
                    });
                    if stage != Stage::Preparing {
                        service.log(&name, stage_entry(stage));
                    }
                    if stage == Stage::Ready {
                        service.tend_mailbox(&name, single);
                    }
                },
                false,
            );
            let error = outcome.err().map(|e| format!("{e:#}"));
            if let Some(e) = &error {
                error!("environment {name}: {e}");
                progress.send(Err(e.clone())).ok();
            }
            let agent = {
                let mut shared = service.lock();
                shared.current = None;
                Arc::clone(&shared.agent)
            };
            *agent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            service.log(
                &name,
                error
                    .as_ref()
                    .map_or_else(|| "Stopped".to_owned(), |e| format!("Stopped: {e}")),
            );
            service.broadcast(&Event::Stopped { name, error });
        });
        Ok(())
    }

    fn not_running(&self, name: &str) -> Result<()> {
        if self.lock().current.as_ref().is_some_and(|c| c.name == name) {
            bail!("'{name}' is running; stop it first");
        }
        Ok(())
    }

    /// Starts `name` and reports its progress to `out` until the desktop is
    /// up; a start that fails ends with the reason.
    fn start_and_report(
        &self,
        name: &str,
        keyboard: Option<gse_protocol::Keyboard>,
        peer: Peer,
        out: &mut UnixStream,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.start(name, keyboard, peer.uid, tx)?;
        for update in rx {
            match update {
                Ok(stage) => {
                    send(out, &Reply::Progress { stage })?;
                    if stage == Stage::Ready {
                        return Ok(());
                    }
                }
                Err(reason) => bail!(reason),
            }
        }
        bail!("'{name}' stopped before it was ready")
    }

    /// Stops whatever runs, an environment or an image build, and waits
    /// until it is over and the GPU is back with the host.
    fn stop_everything(&self) {
        {
            let shared = self.lock();
            if let Some(current) = shared.current.as_ref() {
                current.stop.store(true, Ordering::SeqCst);
            }
            if let Some(build) = shared.build.as_ref() {
                build.stop.store(true, Ordering::SeqCst);
            }
        }
        for _ in 0..300 {
            let shared = self.lock();
            if shared.current.is_none() && shared.build.is_none() {
                return;
            }
            drop(shared);
            thread::sleep(Duration::from_millis(100));
        }
        warn!("the environment or image build did not stop in time");
    }

    /// Records that the build of `preset` reached `step` and tells the
    /// watchers and `report`.
    fn build_step(&self, preset: &str, step: String, report: &Sender<BuildUpdate>) {
        if let Some(build) = self.lock().build.as_mut() {
            build.steps.push(step.clone());
        }
        self.build_progress(preset, step, None, report);
    }

    /// Tells the watchers and `report` how far the build of `preset` is,
    /// such as `download 40%` with the bytes of that `download`, without
    /// recording it as a step.
    fn build_progress(
        &self,
        preset: &str,
        step: String,
        download: Option<Downloaded>,
        report: &Sender<BuildUpdate>,
    ) {
        self.broadcast(&Event::Build {
            preset: preset.to_owned(),
            step: step.clone(),
            download,
        });
        report.send(BuildUpdate::Step { step, download }).ok();
    }

    /// Makes `name` the image build in progress and returns the flag that
    /// stops it. Images build only while no environment runs, one at a
    /// time. The image's last failed build is forgotten: this one replaces
    /// it.
    fn claim_build(&self, name: &str) -> Result<Arc<AtomicBool>> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut shared = self.lock();
        if shared.moving.is_some() {
            bail!("the sandboxes are being moved to another disk");
        }
        if let Some(current) = &shared.current {
            bail!("'{}' is running; images build once it stops", current.name);
        }
        if let Some(build) = &shared.build {
            bail!("the image '{}' is building", build.preset);
        }
        shared.store.forget_failure(name);
        shared.build = Some(Building {
            preset: name.to_owned(),
            steps: Vec::new(),
            stop: Arc::clone(&stop),
            started: now(),
        });
        Ok(stop)
    }

    /// A sender for the steps the build VM of `name` reports, which pass
    /// them on as steps of the build, and the thread that does it; the
    /// thread ends once every sender is gone.
    fn forward_steps(
        &self,
        name: &str,
        report: &Sender<BuildUpdate>,
    ) -> (Sender<String>, thread::JoinHandle<()>) {
        let (steps, reached) = mpsc::channel();
        let (service, name, report) = (self.clone(), name.to_owned(), report.clone());
        let forwarder = thread::spawn(move || {
            for step in reached {
                service.build_step(&name, step, &report);
            }
        });
        (steps, forwarder)
    }

    /// Ends the build of `name` with `outcome`, which the watchers and
    /// `report` learn; another build may start then. A build that failed,
    /// rather than being stopped, is recorded with the step it reached and
    /// its `console`, until the image builds again.
    fn finish_build(
        &self,
        name: &str,
        outcome: Result<()>,
        console: Option<PathBuf>,
        report: &Sender<BuildUpdate>,
    ) {
        let error = outcome.err().map(|e| format!("{e:#}"));
        if let Some(e) = &error {
            error!("building the image '{name}': {e}");
        } else {
            info!("the image '{name}' is ready");
        }
        {
            let mut shared = self.lock();
            let build = shared.build.take();
            let stopped = build
                .as_ref()
                .is_some_and(|b| b.stop.load(Ordering::SeqCst));
            if error.is_some() && !stopped {
                let step = build.and_then(|b| b.steps.last().cloned());
                if let Err(e) = shared.store.record_failure(name, step, console) {
                    warn!("recording the failed build of '{name}': {e:#}");
                }
            }
        }
        self.broadcast(&Event::Changed);
        self.broadcast(&Event::BuildFinished {
            preset: name.to_owned(),
            error: error.clone(),
        });
        report
            .send(BuildUpdate::Finished(error.map_or(Ok(()), Err)))
            .ok();
    }

    /// Starts building the image of preset `name` for `peer` on a thread of
    /// its own; its steps and its end go to `report` and to every watcher.
    fn start_build(&self, name: &str, peer: Peer, report: Sender<BuildUpdate>) -> Result<()> {
        let store = self.lock().store.clone();
        let preset = presets::load(&store.presets_dir(), name)?;
        let base = store.base();
        if !base.exists() {
            bail!("no base image; build or install one first");
        }
        let appimages = open_appimages(&preset, peer.uid)?;
        let owner = vm::Owner::from_uid(peer.uid)?;
        let stop = self.claim_build(name)?;
        info!("building the image of preset '{name}' for {}", owner.name);

        let service = self.clone();
        let name = name.to_owned();
        thread::spawn(move || {
            service.build_step(&name, "prepare".into(), &report);
            let (steps, forwarder) = service.forward_steps(&name, &report);
            let job = image::Job::new(&stop, Some(owner.name), Some(steps));
            let out = store.builds_dir().join(format!("{name}.qcow2"));
            fs::remove_file(&out).ok();
            let outcome = image::build_preset_image(&preset, appimages, &base, &out, &job, false)
                .and_then(|()| store.replace_image(&name, &out));
            let console = job.console.get().cloned();
            drop(job);
            forwarder.join().ok();
            for reset in outcome.as_deref().unwrap_or_default() {
                service.log(
                    reset,
                    format!("The image {name} is new; the next start begins from it"),
                );
            }
            service.finish_build(&name, outcome.map(drop), console, &report);
        });
        Ok(())
    }

    /// Starts building the base image for `peer` on a thread of its own:
    /// the Fedora Cloud image of the configured release is fetched,
    /// provisioned in a build VM and put in place of the old base image.
    /// Its steps and its end go to `report` and to every watcher, under the
    /// name [`BASE_IMAGE`].
    fn start_base_build(&self, peer: Peer, report: Sender<BuildUpdate>) -> Result<()> {
        let guest = image::guest_program(None)?;
        let owner = vm::Owner::from_uid(peer.uid)?;
        let (store, release, dev, packs) = {
            let shared = self.lock();
            let dev = shared.config.dev_ssh_port.is_some();
            let packs = base_packs(&shared.root, &shared.config, &shared.store)?;
            (
                shared.store.clone(),
                shared.config.fedora_release(),
                dev,
                packs,
            )
        };
        let dev_key = dev.then(image::working_tree_dev_key).flatten();
        let stop = self.claim_build(BASE_IMAGE)?;
        info!(
            "building the base image from Fedora {release} with the drivers {} for {}",
            packs.join(", "),
            owner.name
        );

        let service = self.clone();
        thread::spawn(move || {
            service.build_step(BASE_IMAGE, "prepare".into(), &report);
            let mut on_download = |download: fedora::Download| match download {
                fedora::Download::Started => {
                    service.build_step(BASE_IMAGE, "download".into(), &report);
                }
                fedora::Download::Progress { received, total } => {
                    let percent = fedora::Download::percent(received, total);
                    service.build_progress(
                        BASE_IMAGE,
                        format!("download {percent}%"),
                        Some(Downloaded { received, total }),
                        &report,
                    );
                }
            };
            let fetched = fedora::fetch(&store.cache_dir(), release, &stop, &mut on_download);
            let mut console = None;
            let outcome = fetched.and_then(|from| {
                let (steps, forwarder) = service.forward_steps(BASE_IMAGE, &report);
                let job = image::Job::new(&stop, Some(owner.name), Some(steps));
                let out = store.builds_dir().join(format!("{BASE_IMAGE}.qcow2"));
                fs::remove_file(&out).ok();
                let built = image::build_base_image(
                    &from,
                    &out,
                    guest,
                    &packs,
                    dev_key.as_deref(),
                    &job,
                    false,
                );
                console = job.console.get().cloned();
                drop(job);
                forwarder.join().ok();
                built.and_then(|()| store.replace_base(&out, &packs))
            });
            service.finish_build(BASE_IMAGE, outcome, console, &report);
        });
        Ok(())
    }

    /// Builds the image of preset `name` and reports each step to `out`
    /// until the build ends; a failed build ends with the reason.
    fn build_and_report(&self, name: &str, peer: Peer, out: &mut UnixStream) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.start_build(name, peer, tx)?;
        report_build(name, &rx, out)
    }

    /// Builds the base image and reports each step to `out` until the
    /// build ends; a failed build ends with the reason.
    fn build_base_and_report(&self, peer: Peer, out: &mut UnixStream) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.start_base_build(peer, tx)?;
        report_build(BASE_IMAGE, &rx, out)
    }

    /// Stops the running environment at once, if `peer` owns it.
    fn stop(&self, peer: Peer) -> Result<()> {
        let mut shared = self.lock();
        let current = shared.current.as_mut().context("nothing is running")?;
        if current.owner != peer.uid && peer.uid != 0 {
            bail!("'{}' belongs to another user", current.name);
        }
        current.stop.store(true, Ordering::SeqCst);
        current.stage = Stage::Stopping;
        let name = current.name.clone();
        drop(shared);
        self.log(&name, stage_entry(Stage::Stopping));
        self.broadcast(&Event::Progress {
            name,
            stage: Stage::Stopping,
        });
        Ok(())
    }

    /// Runs `work` with the agent of `name`, which must be running and up;
    /// a failed call drops the connection, so the next one starts afresh.
    fn with_agent<T>(&self, name: &str, work: impl FnOnce(&mut Agent) -> Result<T>) -> Result<T> {
        let slot = {
            let shared = self.lock();
            let current = shared
                .current
                .as_ref()
                .filter(|c| c.name == name)
                .with_context(|| format!("'{name}' is not running"))?;
            if current.stage != Stage::Ready {
                bail!("'{name}' is not up yet");
            }
            Arc::clone(&shared.agent)
        };
        let mut slot = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(Agent::connect(&vm::agent_socket(name))?);
        }
        let outcome = work(slot.as_mut().expect("connected above"));
        if outcome.is_err() {
            *slot = None;
        }
        outcome
    }

    /// Whether `name` runs and its agent answers.
    fn is_up(&self, name: &str) -> bool {
        self.lock()
            .current
            .as_ref()
            .is_some_and(|c| c.name == name && c.stage == Stage::Ready)
    }

    /// The folder of the environment `name`, which must exist.
    fn environment_dir(&self, name: &str) -> Result<PathBuf> {
        let shared = self.lock();
        shared.store.image_of(name)?;
        Ok(shared.store.dir(name))
    }

    /// Stops `name` at once when the user asks to leave before the service
    /// takes its Outbox by itself (before its desktop is up, or with two
    /// GPUs), where no last round waits.
    fn stop_on_early_leave(&self, name: &str) {
        let service = self.clone();
        let name = name.to_owned();
        thread::spawn(move || {
            loop {
                let flags = service
                    .lock()
                    .current
                    .as_ref()
                    .filter(|c| c.name == name)
                    .map(|c| {
                        (
                            Arc::clone(&c.stop),
                            c.leave.load(Ordering::SeqCst),
                            c.collecting.load(Ordering::SeqCst),
                        )
                    });
                match flags {
                    Some((stop, true, false)) => {
                        stop.store(true, Ordering::SeqCst);
                        return;
                    }
                    Some((_, false, _)) => thread::sleep(Duration::from_millis(200)),
                    None | Some((_, true, true)) => return,
                }
            }
        });
    }

    /// Once `name` is up: what waits for its Inbox goes in, and on a single
    /// GPU, where the app is gone while it runs, its Outbox is taken into
    /// the spool every [`mailbox::COLLECT_EVERY`] until it stops.
    fn tend_mailbox(&self, name: &str, single: bool) {
        let service = self.clone();
        let name = name.to_owned();
        thread::spawn(move || {
            let Ok(dir) = service.environment_dir(&name) else {
                return;
            };
            if !mailbox::waiting(&dir).is_empty() {
                let attached = service
                    .lock()
                    .current
                    .as_ref()
                    .filter(|c| c.name == name)
                    .map(|c| c.inbox_disks.clone())
                    .unwrap_or_default();
                match service.with_agent(&name, |agent| mailbox::deliver(&dir, agent, &attached)) {
                    Ok(delivered) => {
                        service.log(
                            &name,
                            format!("{} file(s) that waited put into the Inbox", delivered.files),
                        );
                        for (path, why) in delivered.stayed {
                            service.log(&name, format!("{path} still waits for the Inbox: {why}"));
                        }
                        for (path, why) in delivered.failed {
                            service.log(&name, format!("{path} did not go into the Inbox: {why}"));
                        }
                    }
                    Err(e) => warn!("{name}: delivering the waiting files: {e:#}"),
                }
            }
            if !single {
                return;
            }
            let flags = service
                .lock()
                .current
                .as_ref()
                .filter(|c| c.name == name)
                .map(|c| {
                    c.collecting.store(true, Ordering::SeqCst);
                    (Arc::clone(&c.stop), Arc::clone(&c.leave))
                });
            let Some((stop, leave)) = flags else {
                return;
            };
            service.with_agent(&name, Agent::collecting).ok();
            let mut seen = std::collections::HashSet::new();
            while service.is_up(&name) {
                let leaving = leave.load(Ordering::SeqCst);
                if leaving {
                    service.with_agent(&name, Agent::leaving).ok();
                }
                match service.with_agent(&name, |agent| mailbox::collect(&dir, agent, &mut seen)) {
                    Ok(0) => {}
                    Ok(count) => service.log(
                        &name,
                        format!("{count} file(s) of the Outbox taken, for the host after the run"),
                    ),
                    Err(e) => warn!("{name}: taking the Outbox: {e:#}"),
                }
                if leaving {
                    // The last round is in; the window that says so has
                    // a moment to show before the screen goes.
                    thread::sleep(Duration::from_secs(2));
                    stop.store(true, Ordering::SeqCst);
                    return;
                }
                let next = std::time::Instant::now() + mailbox::COLLECT_EVERY;
                while std::time::Instant::now() < next && !leave.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(200));
                }
            }
        });
    }

    /// The requests about a running environment's programs and files.
    /// Copies `paths` of the host into the Inbox of `name`, or keeps them
    /// for its next start, telling the client how far it got; a
    /// [`Request::CancelTransfer`] stops it.
    fn send_files(
        &self,
        name: &str,
        paths: &[PathBuf],
        peer: Peer,
        out: &mut UnixStream,
    ) -> Result<()> {
        let files = host_files(paths, peer.uid)?;
        let count = files.len();
        let stop = self.start_send(name)?;
        mailbox::yield_to_others();
        let up = self.is_up(name);
        let sent = {
            let mut copied = sending(out, total_size(&files));
            if up {
                self.with_agent(name, |agent| {
                    for (done, (path, mut file, size)) in files.into_iter().enumerate() {
                        // An AppImage runs whether or not its
                        // download kept the flag.
                        let executable = file.metadata()?.mode() & 0o100 != 0
                            || path.to_ascii_lowercase().ends_with(".appimage");
                        let mut counted =
                            mailbox::Counted::new(&mut file, &mut copied).stopping(&stop.flag);
                        if let Err(e) = agent.put(&path, &mut counted, size, executable) {
                            if e.downcast_ref::<std::io::Error>()
                                .is_some_and(mailbox::is_stopped)
                            {
                                return Err(mailbox::Stopped { copied: done }.into());
                            }
                            return Err(e);
                        }
                    }
                    Ok(count)
                })
            } else {
                let dir = self.environment_dir(name)?;
                mailbox::queue(&dir, files, &mut copied, &stop.flag)
            }
        };
        match sent {
            Ok(count) if up => {
                self.log(
                    name,
                    format!("{count} file(s) from the host put into the Inbox"),
                );
            }
            Ok(count) => self.log(
                name,
                format!(
                    "{count} file(s) from the host wait for the next start to go into the Inbox"
                ),
            ),
            Err(e) => {
                let Some(stopped) = e.downcast_ref::<mailbox::Stopped>() else {
                    return Err(e);
                };
                let copied = stopped.copied;
                self.log(
                    name,
                    format!("sending files in stopped: {copied} of {count} file(s) went in"),
                );
                send(
                    out,
                    &Reply::Stopped {
                        copied,
                        files: count,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn handle_guest(&self, request: Request, peer: Peer, out: &mut UnixStream) -> Result<()> {
        match request {
            Request::Processes { name } => {
                let apps = self.with_agent(&name, Agent::processes)?;
                send(out, &Reply::Processes { apps })?;
            }
            Request::EndProcess { name, pid, force } => {
                self.with_agent(&name, |a| a.end_process(pid, force))?;
                let how = if force { "Killed" } else { "Asked to quit:" };
                self.log(&name, format!("{how} process {pid}"));
            }
            Request::EndApp { name, unit } => {
                // The journal names the program as the list showed it.
                let title = self.with_agent(&name, |agent| {
                    let title = agent
                        .processes()?
                        .into_iter()
                        .find(|app| app.unit.as_deref() == Some(unit.as_str()))
                        .map_or_else(|| unit.clone(), |app| app.title);
                    agent.end_app(&unit)?;
                    Ok(title)
                })?;
                self.log(&name, format!("Killed {title} with everything it started"));
            }
            Request::RemoveBuffered {
                name,
                waiting,
                taken,
            } => {
                if self.lock().current.as_ref().is_some_and(|c| c.name == name) {
                    bail!("'{name}' is running");
                }
                if self.lock().sends.contains_key(&name) {
                    bail!("files are being sent to '{name}'");
                }
                let dir = self.environment_dir(&name)?;
                let removed =
                    mailbox::remove_waiting(&dir, &waiting)? + mailbox::remove_taken(&dir, &taken)?;
                self.log(
                    &name,
                    format!("{removed} file(s) removed from the host's buffer"),
                );
            }
            Request::Files { name } if !self.is_up(&name) => {
                let dir = self.environment_dir(&name)?;
                let files = gse_protocol::GuestFiles {
                    inbox: mailbox::waiting(&dir),
                    outbox: mailbox::taken(&dir),
                };
                send(out, &Reply::Files { files })?;
            }
            Request::Files { name } => {
                let files = self.with_agent(&name, Agent::files)?;
                send(out, &Reply::Files { files })?;
            }
            Request::SendFiles { name, paths } => self.send_files(&name, &paths, peer, out)?,
            Request::CancelTransfer { name } => {
                let shared = self.lock();
                let flag = shared
                    .sends
                    .get(&name)
                    .context("no files are being sent in")?;
                flag.store(true, Ordering::SeqCst);
                info!("stopping the send of files into '{name}'");
            }
            Request::TakeFiles { name } => {
                let dir = self.environment_dir(&name)?;
                if self.is_up(&name) {
                    self.with_agent(&name, |agent| mailbox::spool_outbox(&dir, agent))?;
                }
                mailbox::yield_to_others();
                let scratch = self.lock().store.scratch(&name);
                let (passed, refused) = mailbox::hand_over(&dir, out, &scratch, &scan)?;
                self.log(
                    &name,
                    format!("{passed} file(s) of the Outbox taken to the host, {refused} refused"),
                );
            }
            _ => unreachable!("not a request about a running environment"),
        }
        Ok(())
    }

    /// Replaces the settings of `name`; the internet of a running
    /// environment follows at once.
    fn set_environment_settings(&self, name: &str, settings: &Settings) -> Result<()> {
        let root = self.lock().root.clone();
        let settings = validate(settings, &gpu::describe(&root)?)?;
        let mut shared = self.lock();
        let before = shared.store.settings(name, &shared.config.settings());
        shared.store.image_of(name)?;
        shared.store.set_settings(name, &settings)?;
        let internet_changed = match shared.current.as_mut() {
            Some(current) if current.name == name && current.internet != settings.internet => {
                net::apply(settings.internet, false)?;
                current.internet = settings.internet;
                true
            }
            _ => false,
        };
        drop(shared);
        if before.gpu != settings.gpu
            || before.screen_mode != settings.screen_mode
            || before.gpu_driver != settings.gpu_driver
        {
            self.log(
                name,
                format!(
                    "Settings: GPU {}, screen {}; they apply from the next start",
                    settings.gpu,
                    screen_words(&self.lock().root, &settings.gpu, &settings.screen_mode)
                ),
            );
        }
        if before.internet != settings.internet {
            self.log(name, format!("Internet {}", on_off(settings.internet)));
        }
        if internet_changed {
            self.broadcast(&Event::Internet {
                on: settings.internet,
            });
        }
        self.broadcast(&Event::Changed);
        Ok(())
    }

    /// The requests about presets and image builds, the base image's
    /// included.
    fn handle_preset(&self, request: Request, peer: Peer, out: &mut UnixStream) -> Result<()> {
        match request {
            Request::Presets => {
                let shared = self.lock();
                let presets = presets::list(&shared.store.presets_dir());
                let build = shared.build.as_ref().map(|b| Build {
                    preset: b.preset.clone(),
                    steps: b.steps.clone(),
                    started: Some(b.started),
                });
                let packs = base_packs(&shared.root, &shared.config, &shared.store)
                    .unwrap_or_else(|_| vec!["mesa"]);
                let base = shared
                    .store
                    .base_image(has_guest_program().then_some(packs.as_slice()));
                let stale = shared.store.stale_images();
                let failed = shared
                    .store
                    .failures(presets.iter().map(|p| p.name.as_str()).chain([BASE_IMAGE]));
                drop(shared);
                send(
                    out,
                    &Reply::Presets {
                        presets,
                        build,
                        base,
                        stale,
                        failed,
                    },
                )?;
            }
            Request::SavePreset { preset } => {
                let dir = self.lock().store.presets_dir();
                presets::save(&dir, &preset)?;
                self.broadcast(&Event::PresetsChanged);
            }
            Request::DeletePreset { name } => {
                let shared = self.lock();
                if shared.build.as_ref().is_some_and(|b| b.preset == name) {
                    bail!("'{name}' is building; stop the build first");
                }
                if let Some((environment, _)) = shared
                    .store
                    .environments()
                    .into_iter()
                    .find(|(_, from)| *from == name)
                {
                    bail!("the sandbox '{environment}' is made from '{name}'; delete it first");
                }
                presets::delete(&shared.store.presets_dir(), &name)?;
                shared.store.forget_image(&name);
                drop(shared);
                self.broadcast(&Event::PresetsChanged);
            }
            Request::RenamePreset { name, to } => self.rename_preset(&name, &to)?,
            Request::PresetTemplates => send(
                out,
                &Reply::PresetTemplates {
                    templates: presets::templates(),
                },
            )?,
            Request::BuildPreset { name } => self.build_and_report(&name, peer, out)?,
            Request::BuildBase => self.build_base_and_report(peer, out)?,
            Request::StopBuild => {
                let shared = self.lock();
                let build = shared.build.as_ref().context("no image is building")?;
                build.stop.store(true, Ordering::SeqCst);
                info!("stopping the build of '{}'", build.preset);
            }
            _ => unreachable!("not a preset request"),
        }
        Ok(())
    }

    /// The requests that create, change and remove environments.
    fn handle_lifecycle(&self, request: Request) -> Result<()> {
        match request {
            Request::Create { name, image } => {
                let shared = self.lock();
                shared
                    .store
                    .create(&name, &image, &shared.config.settings())?;
                drop(shared);
                self.log(&name, format!("Created from the set of apps {image}"));
                self.broadcast(&Event::Changed);
            }
            Request::Reset { name } => {
                self.not_running(&name)?;
                self.lock().store.reset(&name)?;
                self.log(&name, "Reset: the next start begins from the image");
                self.broadcast(&Event::Changed);
            }
            Request::Rename { name, to } => {
                self.not_running(&name)?;
                self.lock().store.rename(&name, &to)?;
                self.log(&to, format!("Renamed from {name}"));
                self.broadcast(&Event::Changed);
            }
            Request::SetEnvironmentSettings { name, settings } => {
                self.set_environment_settings(&name, &settings)?;
            }
            Request::Delete { name } => {
                self.not_running(&name)?;
                self.lock().store.delete(&name)?;
                self.broadcast(&Event::Changed);
            }
            _ => unreachable!("not a lifecycle request"),
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // one arm per request, most a line
    fn handle(&self, request: Request, peer: Peer, out: &mut UnixStream) -> Result<()> {
        if !matches!(
            request,
            Request::Hello
                | Request::List
                | Request::Images
                | Request::Watch
                | Request::Settings
                | Request::Journal { .. }
                | Request::Processes { .. }
                | Request::Files { .. }
                | Request::Antivirus
                | Request::Hardware
                | Request::HostDevices
                | Request::Packages
                | Request::Storage
                | Request::Presets
                | Request::PresetTemplates
        ) {
            self.pause_disk_work();
        }
        match request {
            Request::Hello => send(
                out,
                &Reply::Hello {
                    version: gse_protocol::VERSION,
                    build: env!("CARGO_PKG_VERSION").into(),
                },
            )?,
            Request::Restart => {
                self.stop_everything();
                send(out, &Reply::Done)?;
                info!("restarting at a client's request");
                std::process::exit(0);
            }
            Request::List => send(
                out,
                &Reply::Environments {
                    environments: self.list(),
                },
            )?,
            Request::Images => send(
                out,
                &Reply::Images {
                    images: self.lock().store.images(),
                },
            )?,
            request @ (Request::Create { .. }
            | Request::Reset { .. }
            | Request::Rename { .. }
            | Request::Delete { .. }
            | Request::SetEnvironmentSettings { .. }) => self.handle_lifecycle(request)?,
            request @ (Request::Processes { .. }
            | Request::EndProcess { .. }
            | Request::EndApp { .. }
            | Request::Files { .. }
            | Request::RemoveBuffered { .. }
            | Request::SendFiles { .. }
            | Request::TakeFiles { .. }
            | Request::CancelTransfer { .. }) => self.handle_guest(request, peer, out)?,
            Request::Journal { name } => {
                let entries = self.lock().store.journal(&name)?;
                send(out, &Reply::Journal { entries })?;
            }
            Request::Start { name, keyboard } => {
                self.start_and_report(&name, keyboard, peer, out)?;
            }
            Request::Stop => self.stop(peer)?,
            Request::Internet { on } => {
                let name = self
                    .lock()
                    .current
                    .as_ref()
                    .context("nothing is running")?
                    .name
                    .clone();
                let mut settings = self
                    .list()
                    .into_iter()
                    .find(|e| e.name == name)
                    .map(|e| e.settings)
                    .context("the running environment is gone")?;
                settings.internet = on;
                self.set_environment_settings(&name, &settings)?;
            }
            request @ (Request::Presets
            | Request::SavePreset { .. }
            | Request::DeletePreset { .. }
            | Request::RenamePreset { .. }
            | Request::PresetTemplates
            | Request::BuildPreset { .. }
            | Request::BuildBase
            | Request::StopBuild) => self.handle_preset(request, peer, out)?,
            Request::Watch => {
                let (tx, rx) = mpsc::channel();
                self.lock().watchers.push(tx);
                return watch(&rx, out);
            }
            Request::Settings => {
                let root = self.lock().root.clone();
                let gpus = gpu::describe(&root)?;
                let settings = self.lock().config.settings();
                send(out, &Reply::Settings { settings, gpus })?;
            }
            Request::SetSettings { settings } => self.set_defaults(&settings)?,
            Request::Storage | Request::MoveStorage { .. } | Request::CancelMove => {
                self.handle_storage(request, out)?;
            }
            Request::Antivirus => {
                let state = antivirus::Antivirus::new(antivirus::ROOT).state();
                send(
                    out,
                    &Reply::Antivirus {
                        engine: state.engine,
                        database: state.database,
                        checked: state.checked,
                        error: state.error,
                    },
                )?;
            }
            Request::Packages => {
                let (missing, after_restart) = crate::setup::missing_packages();
                send(
                    out,
                    &Reply::Packages {
                        missing,
                        after_restart,
                    },
                )?;
            }
            Request::HostDevices => {
                let (root, gpu) = {
                    let shared = self.lock();
                    (shared.root.clone(), shared.config.gpu)
                };
                let board_sound = gse_host::pci::Gpu::read(&root, gpu)
                    .is_ok_and(|gpu| !gse_host::pci::movable_sound_cards(&root, &gpu).is_empty());
                send(
                    out,
                    &Reply::HostDevices {
                        usb: gse_host::usb::listed(&root),
                        board_sound,
                    },
                )?;
            }
            Request::Hardware => {
                let root = self.lock().root.clone();
                let cpu = gse_host::hardware::Cpu::read(&root)
                    .map_or_else(|_| gse_protocol::Support::default(), |cpu| cpu.support());
                let mut text = gse_host::hardware::describe(&root);
                let shared = self.lock();
                let drivers = match base_packs(&shared.root, &shared.config, &shared.store) {
                    Ok(packs) => packs.join(" "),
                    Err(e) => format!("unknown: {e:#}"),
                };
                text.push_str("base image needs the drivers ");
                text.push_str(&drivers);
                text.push('\n');
                drop(shared);
                send(out, &Reply::Hardware { text, cpu })?;
            }
        }
        send(out, &Reply::Done)?;
        Ok(())
    }

    /// Replaces the settings new environments start with.
    fn set_defaults(&self, settings: &Settings) -> Result<()> {
        let root = self.lock().root.clone();
        let settings = validate(settings, &gpu::describe(&root)?)?;
        let mut shared = self.lock();
        let config = Config {
            gpu: settings.gpu.parse()?,
            screen_mode: settings.screen_mode.clone(),
            internet: settings.internet,
            gpu_driver: settings.gpu_driver.clone(),
            ..shared.config.clone()
        };
        if config != shared.config {
            config.save(Path::new(CONFIG))?;
            info!(
                "settings of new environments: GPU {} ({}), screen {}, internet {}",
                settings.gpu,
                settings.gpu_driver.as_deref().unwrap_or("default driver"),
                settings.screen_mode,
                on_off(settings.internet)
            );
            shared.config = config;
            drop(shared);
            self.broadcast(&Event::SettingsChanged);
        }
        Ok(())
    }
}

/// Opens the AppImages of `preset` for a build that `uid` asked for. The
/// service reads them as root, so each must be a file that user could read
/// anyway: their own, or readable by everyone. The open files go to the
/// build, so a path changed afterwards cannot swap in another file.
fn open_appimages(preset: &HostPreset, uid: u32) -> Result<Vec<(String, fs::File)>> {
    preset
        .seed_files()
        .into_iter()
        .map(|(seed, path)| {
            let file =
                open_as(&path, uid).with_context(|| format!("AppImage {}", path.display()))?;
            Ok((seed, file))
        })
        .collect()
}

/// Tells the client on `out` that the Outbox file `path` stays inside.
fn refuse(out: &mut dyn std::io::Write, path: &str, reason: String) -> std::io::Result<()> {
    send(
        out,
        &Reply::Refused {
            path: path.to_owned(),
            reason,
        },
    )
}

/// Has the antivirus look at the files below `staging`, which it may read
/// then.
fn scan(staging: &Path) -> Result<Vec<antivirus::Found>> {
    if let Some(scratch) = staging.parent() {
        antivirus::readable_by_scanner(scratch)?;
    }
    antivirus::Antivirus::new(antivirus::ROOT)
        .scan(staging)
        .context("the files cannot pass the antivirus")
}

/// Files kept in the Outbox, each with why.
pub type Refusals = Vec<(String, String)>;

/// Copies `files` of the Outbox through `agent` onto `out`, file by file,
/// with the outbound checks; returns the files that passed and the ones
/// refused.
pub fn take_files(
    agent: &mut Agent,
    out: &mut dyn std::io::Write,
    files: &[gse_protocol::GuestFile],
) -> Result<(Vec<gse_protocol::GuestFile>, Refusals)> {
    let (mut passed, mut refused, mut total) = (Vec::new(), Vec::new(), 0_u64);
    for file in files {
        if passed.len() + refused.len() >= checks::MAX_TAKE_FILES {
            let reason = format!("a take holds at most {} files", checks::MAX_TAKE_FILES);
            refuse(out, &file.path, reason.clone())?;
            refused.push((file.path.clone(), reason));
            continue;
        }
        if let Some(reason) = checks::by_name(&file.path) {
            refuse(out, &file.path, reason.clone())?;
            refused.push((file.path.clone(), reason));
            continue;
        }
        let room = checks::MAX_TAKE.saturating_sub(total);
        let mut forward = checks::Forward::new(out, &file.path, file.size);
        let outcome = agent
            .get(&file.path, &mut forward, room)
            .and_then(|size| Ok((size, forward.finish()?)));
        let (started, verdict) = (forward.started(), forward.refused.take());
        match (outcome, verdict) {
            (Ok((size, ())), None) => {
                passed.push(gse_protocol::GuestFile {
                    path: file.path.clone(),
                    size,
                });
                total += size;
            }
            (Ok(_), Some(reason)) => {
                refuse(out, &file.path, reason.clone())?;
                refused.push((file.path.clone(), reason));
            }
            // The client drops a file refused after it began.
            (Err(e), _) => {
                let reason = if started {
                    format!("it broke off: {e:#}")
                } else {
                    format!("{e:#}")
                };
                refuse(out, &file.path, reason.clone())?;
                refused.push((file.path.clone(), reason));
            }
        }
    }
    Ok((passed, refused))
}

/// The files `paths` name for the Inbox, opened for `uid`: a file under its
/// own name, a folder with every regular file below it under the folder's
/// name. Links and anything else are left out.
/// How often a send in says how far it got.
const SENDING_EVERY: Duration = Duration::from_millis(250);

/// The bytes `files` hold.
fn total_size(files: &[(String, fs::File, u64)]) -> u64 {
    files.iter().map(|(_, _, size)| size).sum()
}

/// What a send in of `total` bytes calls with each part it copies: it tells
/// the client how far it got every [`SENDING_EVERY`] and once at the end. A
/// client gone meanwhile misses the news, not the files.
fn sending(out: &mut UnixStream, total: u64) -> impl FnMut(u64) + '_ {
    let mut done = 0;
    let mut last: Option<Instant> = None;
    move |part| {
        done += part;
        if done >= total || last.is_none_or(|at| at.elapsed() >= SENDING_EVERY) {
            last = Some(Instant::now());
            send(out, &Reply::Sending { done, total }).ok();
        }
    }
}

fn host_files(paths: &[PathBuf], uid: u32) -> Result<Vec<(String, fs::File, u64)>> {
    let mut found = Vec::new();
    let mut pending: Vec<(PathBuf, String, usize)> = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("{} has no plain name", path.display()))?;
        pending.push((path.clone(), name.to_owned(), 0));
    }
    while let Some((path, inbox_path, depth)) = pending.pop() {
        let metadata =
            fs::symlink_metadata(&path).with_context(|| format!("{}", path.display()))?;
        if metadata.is_dir() {
            if depth >= 16 {
                bail!("{} goes deeper than 16 folders", path.display());
            }
            for entry in fs::read_dir(&path)?.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                pending.push((entry.path(), format!("{inbox_path}/{name}"), depth + 1));
            }
        } else if metadata.is_file() {
            let file = open_as(&path, uid)?;
            let size = file.metadata()?.len();
            if size > gse_protocol::agent::MAX_SENT_FILE {
                bail!("{} is larger than 64 GiB", path.display());
            }
            found.push((inbox_path, file, size));
            if found.len() > checks::MAX_TAKE_FILES {
                bail!("at most {} files at once", checks::MAX_TAKE_FILES);
            }
        }
    }
    Ok(found)
}

/// Opens `path` for the user `uid` asks it for. The service reads as root,
/// so the file must be one that user could read anyway: their own, or
/// readable by everyone. The open file is checked, so a path changed in
/// between cannot swap in another.
fn open_as(path: &Path, uid: u32) -> Result<fs::File> {
    let shown = path.display();
    let metadata = fs::metadata(path).with_context(|| format!("{shown}"))?;
    if !metadata.is_file() {
        bail!("{shown} is not a file");
    }
    let file = fs::File::open(path).with_context(|| format!("{shown}"))?;
    let metadata = file.metadata()?;
    let mode = metadata.mode();
    let readable = uid == 0 || (metadata.uid() == uid && mode & 0o400 != 0) || mode & 0o004 != 0;
    if !metadata.is_file() || !readable {
        bail!("{shown} is not a file you can read");
    }
    Ok(file)
}

/// The journal line of an environment start reaching `stage`.
const fn stage_entry(stage: Stage) -> &'static str {
    match stage {
        Stage::Preparing => "Preparing",
        Stage::SwitchingGpu => "Switching the GPU",
        Stage::Booting => "Booting",
        Stage::Ready => "The desktop is up",
        Stage::Stopping => "Stopping: QEMU is killed and the GPU goes back to the host",
    }
}

const fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn watch(events: &Receiver<Event>, out: &mut UnixStream) -> Result<()> {
    for event in events {
        send(out, &Reply::Event { event })?;
    }
    Ok(())
}

/// Sends each step of the build of `name` that `updates` brings to `out`
/// until the build ends; a failed build ends with the reason.
fn report_build(name: &str, updates: &Receiver<BuildUpdate>, out: &mut UnixStream) -> Result<()> {
    for update in updates {
        match update {
            BuildUpdate::Step { step, download } => {
                send(out, &Reply::BuildStep { step, download })?;
            }
            BuildUpdate::Finished(Ok(())) => return Ok(()),
            BuildUpdate::Finished(Err(reason)) => bail!(reason),
        }
    }
    bail!("the build of '{name}' ended without a verdict")
}

/// The process on the other end of a connection.
#[derive(Debug, Clone, Copy)]
struct Peer {
    pid: i32,
    uid: u32,
}

fn peer(stream: &UnixStream) -> Result<Peer> {
    let cred = rustix::net::sockopt::socket_peercred(stream).context("reading peer credentials")?;
    Ok(Peer {
        pid: cred.pid.as_raw_nonzero().get(),
        uid: cred.uid.as_raw(),
    })
}

/// Whether polkit lets `peer` use the service. The process is named with its
/// start time so a recycled pid cannot stand in for it.
fn authorized(peer: Peer) -> bool {
    if peer.uid == 0 {
        return true;
    }
    let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", peer.pid)) else {
        return false;
    };
    // Field 22, counted after the parenthesised command name, which may hold
    // spaces.
    let Some(start_time) = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
    else {
        return false;
    };
    Command::new("pkcheck")
        .args(["--action-id", ACTION, "--process"])
        .arg(format!("{},{start_time},{}", peer.pid, peer.uid))
        .output()
        .is_ok_and(|o| o.status.success())
}

fn serve_connection(service: &Service, mut stream: UnixStream) -> Result<()> {
    let peer = peer(&stream)?;
    if !authorized(peer) {
        send(
            &mut stream,
            &Reply::Error {
                message: "not authorized".into(),
            },
        )?;
        bail!("uid {} pid {} is not authorized", peer.uid, peer.pid);
    }
    let mut reader = BufReader::new(stream.try_clone()?);
    while let Some(request) = receive::<Request>(&mut reader)? {
        info!("uid {}: {request:?}", peer.uid);
        if let Err(e) = service.handle(request, peer, &mut stream) {
            send(
                &mut stream,
                &Reply::Error {
                    message: format!("{e:#}"),
                },
            )?;
        }
    }
    Ok(())
}

/// Serves clients until the process is killed.
pub fn serve(root: Root) -> Result<()> {
    let config = Config::load(Path::new(CONFIG))?;
    vm::recover(&root);
    let store = Store::new(DATA);
    for dir in [
        store.images_dir(),
        store.environments_dir(),
        store.builds_dir(),
    ] {
        fs::create_dir_all(&dir)?;
        nocow::mark(&dir);
    }
    // A build the service did not live to finish leaves its partial image,
    // and maybe a partial download.
    for leftover in fs::read_dir(store.builds_dir())?.flatten() {
        fs::remove_file(leftover.path()).ok();
    }
    fedora::remove_partial(&store.cache_dir());
    presets::seed(&store.presets_dir())?;
    store.prune_images();
    antivirus::keep_updated();
    let socket = Path::new(SOCKET);
    fs::create_dir_all(socket.parent().context("socket path")?)?;
    fs::remove_file(socket).ok();
    let listener =
        UnixListener::bind(socket).with_context(|| format!("listening on {}", socket.display()))?;
    // Anyone may connect; polkit decides who is served.
    fs::set_permissions(socket, fs::Permissions::from_mode(0o666))?;
    info!(
        "serving {} for GPU {} ({} images, {} environments)",
        socket.display(),
        config.gpu,
        store.images().len(),
        store.environments().len()
    );
    let service = Service(Arc::new(Mutex::new(Shared {
        root,
        config,
        store,
        current: None,
        build: None,
        watchers: Vec::new(),
        agent: Arc::default(),
        sends: HashMap::new(),
        moving: None,
        disk_work: None,
    })));
    service.tend_disks();
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                warn!("accepting: {e}");
                continue;
            }
        };
        let service = service.clone();
        thread::spawn(move || {
            if let Err(e) = serve_connection(&service, stream) {
                warn!("connection: {e:#}");
            }
        });
    }
    Ok(())
}

/// Makes sure nothing a client sends can be larger than the protocol allows
/// before it reaches the parser.
const _: () = assert!(MAX_LINE <= 1 << 20);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_needs_only_the_gpu() {
        let config: Config = toml::from_str("gpu = \"0000:0e:00.0\"\n").unwrap();
        assert_eq!(config.gpu.to_string(), "0000:0e:00.0");
        assert_eq!(config.screen_mode, "2560x1440@60");
        assert_eq!(config.fedora_release(), fedora::RELEASE);
        assert!(toml::from_str::<Config>("gpu = \"0000:0e:00.0\"\nmystery = 1\n").is_err());
        let older: Config =
            toml::from_str("gpu = \"0000:0e:00.0\"\nfedora_release = 44\n").unwrap();
        assert_eq!(older.fedora_release(), 44);
    }

    #[test]
    fn config_saves_what_it_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gse.toml");
        let config = Config {
            gpu: "0000:0e:00.0".parse().unwrap(),
            screen_mode: "1920x1080@144".into(),
            internet: false,
            memory_mib: 4096,
            cpus: 2,
            gpu_driver: Some("nouveau".into()),
            fedora_release: Some(44),
            dev_ssh_port: Some(2222),
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        // A release left to the default stays out of the file, so it moves
        // with the provisioning script.
        let plain = Config {
            fedora_release: None,
            dev_ssh_port: None,
            ..config.clone()
        };
        plain.save(&path).unwrap();
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("fedora_release")
        );
        assert_eq!(Config::load(&path).unwrap(), plain);
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(config.save(&dir.path().join("missing/gse.toml")).is_err());
    }

    #[test]
    fn settings_must_name_a_gpu_and_a_mode_that_fits() {
        let gpus = [gse_protocol::Gpu {
            address: "0000:0e:00.0".into(),
            vendor_id: 0x10de,
            device_id: 0x2484,
            vendor: None,
            device: None,
            driver: Some("nouveau".into()),
            monitor: false,
            monitor_mode: None,
            mode: gse_protocol::SwitchMode::Secondary,
            family: None,
            integrated: false,
            support: gse_protocol::Support::default(),
            guest_drivers: vec!["nvidia".into(), "nouveau".into()],
        }];
        let settings = |gpu: &str, screen_mode: &str| Settings {
            gpu: gpu.into(),
            screen_mode: screen_mode.into(),
            internet: true,
            gpu_driver: None,
            devices: gse_protocol::Devices::default(),
        };
        let checked = validate(&settings("0e:00.0", "3840x2160@120"), &gpus).unwrap();
        assert_eq!(checked.gpu, "0000:0e:00.0");
        assert_eq!(checked.screen_mode, "3840x2160@120");
        let with = |driver: &str| Settings {
            gpu_driver: Some(driver.into()),
            devices: gse_protocol::Devices::default(),
            ..settings("0e:00.0", "1920x1080@60")
        };
        assert_eq!(
            validate(&with("nouveau"), &gpus)
                .unwrap()
                .gpu_driver
                .as_deref(),
            Some("nouveau")
        );
        // The default driver is stored as none, so it follows the default.
        assert_eq!(validate(&with("nvidia"), &gpus).unwrap().gpu_driver, None);
        assert!(validate(&with("amdgpu"), &gpus).is_err());
        assert!(validate(&settings("0000:07:00.0", "1920x1080@60"), &gpus).is_err());
        assert!(validate(&settings("nonsense", "1920x1080@60"), &gpus).is_err());
        assert!(validate(&settings("0000:0e:00.0", "1920x1080@300"), &gpus).is_err());
        assert!(validate(&settings("0000:0e:00.0", "7680x4320@60"), &gpus).is_err());
    }

    #[test]
    fn store_lists_images_and_environments() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::create_dir_all(store.images_dir()).unwrap();
        fs::write(store.image("launchers"), b"").unwrap();
        fs::write(store.images_dir().join("notes.txt"), b"").unwrap();
        assert_eq!(store.images(), ["launchers"]);

        fs::create_dir_all(store.dir("test")).unwrap();
        fs::write(store.dir("test").join("image"), "launchers\n").unwrap();
        assert_eq!(
            store.environments(),
            [("test".to_owned(), "launchers".to_owned())]
        );
    }

    #[test]
    fn images_without_a_set_are_neither_stale_nor_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::create_dir_all(store.images_dir()).unwrap();
        fs::create_dir_all(store.presets_dir()).unwrap();
        fs::write(store.base_fingerprint_path(), "new\n").unwrap();
        for image in ["games", "orphan", "used"] {
            fs::write(store.image(image), b"").unwrap();
            fs::write(store.on_base_path(image), "old\n").unwrap();
        }
        fs::write(store.presets_dir().join("games.toml"), b"").unwrap();
        fs::create_dir_all(store.dir("test")).unwrap();
        fs::write(store.dir("test").join("image"), "used\n").unwrap();
        assert_eq!(store.stale_images(), ["games"]);

        store.prune_images();
        assert_eq!(store.images(), ["games", "used"]);
        assert!(!store.on_base_path("orphan").exists());
    }

    #[test]
    fn a_new_image_resets_the_environments_created_from_it() {
        if Command::new("qemu-img").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::create_dir_all(store.images_dir()).unwrap();
        fs::create_dir_all(store.builds_dir()).unwrap();
        let qcow2 = |path: &Path| {
            assert!(
                Command::new("qemu-img")
                    .args(["create", "-q", "-f", "qcow2"])
                    .arg(path)
                    .arg("1M")
                    .status()
                    .unwrap()
                    .success()
            );
        };
        qcow2(&store.image("launchers"));
        qcow2(&store.image("other"));
        store.create("a", "launchers", &defaults()).unwrap();
        store.create("b", "other", &defaults()).unwrap();
        store.ensure_disk("a").unwrap();
        store.ensure_disk("b").unwrap();
        fs::write(store.dir("a").join("console.log"), "old").unwrap();
        fs::write(store.dir("b").join("console.log"), "old").unwrap();

        let built = store.builds_dir().join("launchers.qcow2");
        qcow2(&built);
        assert_eq!(store.replace_image("launchers", &built).unwrap(), ["a"]);
        assert!(!built.exists());
        assert!(!store.disk("a").exists());
        assert!(!store.dir("a").join("console.log").exists());
        assert!(store.disk("b").exists());
        assert!(store.dir("b").join("console.log").exists());
        store.ensure_disk("a").unwrap();
        assert!(store.disk("a").exists());
        assert!(store.replace_image("launchers", &built).is_err());
    }

    fn defaults() -> Settings {
        Settings {
            gpu: "0000:0e:00.0".into(),
            screen_mode: "2560x1440@60".into(),
            internet: true,
            gpu_driver: None,
            devices: gse_protocol::Devices::default(),
        }
    }

    #[test]
    fn an_environment_waits_for_its_preset_image() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::create_dir_all(store.presets_dir()).unwrap();
        fs::write(store.presets_dir().join("games.toml"), "name = \"games\"\n").unwrap();
        store.create("later", "games", &defaults()).unwrap();
        assert!(store.ensure_disk("later").is_err());
        assert!(!store.disk("later").exists());
        assert!(store.create("other", "missing", &defaults()).is_err());
    }

    #[test]
    fn settings_belong_to_each_environment() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::create_dir_all(store.images_dir()).unwrap();
        fs::write(store.image("launchers"), b"").unwrap();
        store.create("a", "launchers", &defaults()).unwrap();
        let own = Settings {
            gpu: "0000:07:00.0".into(),
            screen_mode: "1920x1080@144".into(),
            internet: false,
            gpu_driver: None,
            devices: gse_protocol::Devices::default(),
        };
        store.set_settings("a", &own).unwrap();
        assert_eq!(store.settings("a", &defaults()), own);
        store.create("b", "launchers", &defaults()).unwrap();
        assert!(store.rename("a", "b").is_err());
        assert!(store.rename("a", "Bad").is_err());
        store.rename("a", "c").unwrap();
        assert_eq!(store.settings("c", &defaults()), own);
        assert!(!store.dir("a").exists());

        fs::create_dir_all(store.dir("old")).unwrap();
        fs::write(store.dir("old").join("image"), "launchers\n").unwrap();
        assert_eq!(store.settings("old", &defaults()), defaults());
    }

    #[test]
    fn the_journal_keeps_its_latest_lines() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::create_dir_all(store.images_dir()).unwrap();
        fs::write(store.image("launchers"), b"").unwrap();
        store.create("a", "launchers", &defaults()).unwrap();
        assert_eq!(store.journal("a").unwrap(), []);
        let long = "x".repeat(400);
        for i in 0..1200 {
            store.log("a", &format!("{i} {long}")).unwrap();
        }
        assert!(
            fs::metadata(store.dir("a").join("journal")).unwrap().len() <= JOURNAL_MAX as u64 + 512
        );
        let entries = store.journal("a").unwrap();
        assert_eq!(entries.len(), JOURNAL_SHOWN);
        assert!(entries.last().unwrap().text.starts_with("1199 "));
        assert!(store.journal("missing").is_err());
    }

    #[test]
    fn appimages_must_be_readable_by_whoever_builds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.appimage");
        fs::write(&file, b"ELF").unwrap();
        let preset = HostPreset::parse(&format!(
            "name = \"x\"\n[[app]]\nname = \"App\"\nappimage = \"{}\"\n",
            file.display()
        ))
        .unwrap();
        let me = fs::metadata(&file).unwrap().uid();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(open_appimages(&preset, me).unwrap().len(), 1);
        assert!(open_appimages(&preset, me + 1).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open_appimages(&preset, me + 1).is_ok());

        let missing = HostPreset::parse(
            "name = \"x\"\n[[app]]\nname = \"App\"\nappimage = \"/nonexistent/a.appimage\"\n",
        )
        .unwrap();
        assert!(open_appimages(&missing, 0).is_err());
        let directory = HostPreset::parse(&format!(
            "name = \"x\"\n[[app]]\nname = \"App\"\nappimage = \"{}\"\n",
            dir.path().display()
        ))
        .unwrap();
        assert!(open_appimages(&directory, 0).is_err());
    }

    #[test]
    fn a_new_base_image_takes_the_place_of_the_old() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::create_dir_all(store.builds_dir()).unwrap();
        fs::write(store.base(), "old").unwrap();
        let built = store.builds_dir().join("base.qcow2");
        fs::write(&built, "new").unwrap();
        store.replace_base(&built, &["mesa", "nvidia"]).unwrap();
        assert_eq!(fs::read_to_string(store.base()).unwrap(), "new");
        assert!(!built.exists());
        assert!(store.replace_base(&built, &["mesa"]).is_err());
        assert_eq!(fs::read_to_string(store.base()).unwrap(), "new");
        // It holds what the settings need, or more.
        assert!(store.base_image(Some(&["mesa", "nvidia"])).unwrap().current);
        assert!(store.base_image(Some(&["mesa"])).unwrap().current);
        assert!(
            !store
                .base_image(Some(&["mesa", "intel-media"]))
                .unwrap()
                .current
        );
        // One built by another provisioning script is out of date.
        fs::write(store.base_fingerprint_path(), "other\n").unwrap();
        assert!(!store.base_image(Some(&["mesa"])).unwrap().current);
        assert!(store.base_image(None).unwrap().current);
    }

    /// A service over a store in `dir` that runs nothing.
    fn idle_service(dir: &Path) -> Service {
        let store = Store::new(dir);
        fs::create_dir_all(store.images_dir()).unwrap();
        Service(Arc::new(Mutex::new(Shared {
            root: Root::at(dir),
            config: toml::from_str("gpu = \"0000:0e:00.0\"\n").unwrap(),
            store,
            current: None,
            build: None,
            watchers: Vec::new(),
            agent: Arc::default(),
            sends: HashMap::new(),
            moving: None,
            disk_work: None,
        })))
    }

    #[test]
    fn a_failed_build_is_kept_until_its_image_builds_again() {
        let dir = tempfile::tempdir().unwrap();
        let service = idle_service(dir.path());
        let store = service.lock().store.clone();
        let (report, _updates) = mpsc::channel();

        service.claim_build("wow").unwrap();
        let started = service.lock().build.as_ref().map(|b| b.started).unwrap();
        assert!(now().abs_diff(started) < 60);
        service.build_step("wow", "prepare".into(), &report);
        service.build_step("wow", "packages".into(), &report);
        let log = PathBuf::from("/var/log/gse/builds/1793545000-wow-failed.log");
        let failed = || Err(anyhow::anyhow!("the build failed (FAILED 12)"));
        service.finish_build("wow", failed(), Some(log.clone()), &report);
        service.claim_build(BASE_IMAGE).unwrap();
        service.build_step(BASE_IMAGE, "download".into(), &report);
        service.finish_build(BASE_IMAGE, failed(), None, &report);
        assert_eq!(
            store.failures(["launchers", "wow", BASE_IMAGE]),
            [
                FailedBuild {
                    preset: "wow".into(),
                    step: Some("packages".into()),
                    log: Some(log),
                },
                FailedBuild {
                    preset: BASE_IMAGE.into(),
                    step: Some("download".into()),
                    log: None,
                },
            ]
        );
        // The records are no images.
        assert_eq!(store.images(), Vec::<String>::new());

        // The next build clears it, and a stopped build is no failure.
        let stop = service.claim_build("wow").unwrap();
        assert_eq!(store.failures(["wow"]), []);
        stop.store(true, Ordering::SeqCst);
        service.finish_build("wow", failed(), None, &report);
        assert_eq!(store.failures(["wow"]), []);
        service.claim_build(BASE_IMAGE).unwrap();
        service.finish_build(BASE_IMAGE, Ok(()), None, &report);
        assert_eq!(store.failures([BASE_IMAGE]), []);
        assert!(service.lock().build.is_none());
    }

    #[test]
    fn store_refuses_bad_names_and_missing_images() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert!(store.create("../x", "launchers", &defaults()).is_err());
        assert!(store.create("test", "missing", &defaults()).is_err());
        assert!(store.rename("test", "other").is_err());
        assert!(store.delete("test").is_err());
    }
}
