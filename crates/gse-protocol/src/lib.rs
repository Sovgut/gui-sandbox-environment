//! Messages between the gse service and its clients.
//!
//! A client connects to [`SOCKET`] and writes one [`Request`] per line as
//! JSON. The service answers each request with [`Reply`] lines and ends the
//! answer with [`Reply::Done`] or [`Reply::Error`]. [`Request::Start`] sends
//! [`Reply::Progress`] lines as the environment comes up before its last
//! line; [`Request::Watch`] never ends and sends every [`Event`] of the
//! service until the client goes away. [`Request::Settings`] answers the
//! settings new environments start with, and the machine's GPUs;
//! [`Request::SetSettings`] changes them, and
//! [`Request::SetEnvironmentSettings`] changes one environment's own.
//! [`Request::Journal`] answers what happened to an environment, which
//! [`Event::Journal`] continues. [`Request::Presets`] lists
//! the presets images are built from, with the build in progress and the
//! builds that failed; [`Request::SavePreset`] and
//! [`Request::DeletePreset`] change them, and [`Request::BuildPreset`]
//! builds a preset's image, sending [`Reply::BuildStep`] lines as the build
//! goes. [`Request::BuildBase`] builds the base image those images are
//! built on the same way, under the name [`BASE_IMAGE`].

pub mod qmp;

use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The service's socket.
pub const SOCKET: &str = "/run/gse/service.sock";

/// The protocol's version: a client and a service speak to each other only
/// when theirs match ([`Request::Hello`]). It changes with every change to
/// the messages.
pub const VERSION: u32 = 8;

/// The name a build of the base image goes by wherever a preset's name
/// would stand: in [`Event::Build`], [`Event::BuildFinished`] and
/// [`Build`]. No preset may take it.
pub const BASE_IMAGE: &str = "base";

/// The longest line either side accepts; anything longer ends the
/// connection.
pub const MAX_LINE: usize = 64 * 1024;

/// The longest title of a preset, in characters.
pub const MAX_TITLE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case")]
pub enum Request {
    /// The service's protocol version and build, answered by every service
    /// whatever its version, so a client can tell it is out of date.
    Hello,
    /// The environments and which one runs.
    List,
    /// The images environments can be created from.
    Images,
    /// A new environment on `image`, with the settings new environments
    /// start with. The image may still be building, or not built yet: the
    /// environment then waits for it, and starts once it exists.
    Create {
        name: String,
        image: String,
    },
    /// Start `name`; progress follows until it is ready. `keyboard` is the
    /// host user's, which the sandbox takes for this run.
    Start {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keyboard: Option<Keyboard>,
    },
    /// Stop the running environment at once.
    Stop,
    /// Wipe `name` back to its image. It must not be running.
    Reset {
        name: String,
    },
    /// Delete `name` and its disk. It must not be running.
    Delete {
        name: String,
    },
    /// Give `name` the name `to`. It must not be running.
    Rename {
        name: String,
        to: String,
    },
    /// Turn the running environment's internet on or off.
    Internet {
        on: bool,
    },
    /// Every event of the service, for as long as the connection lasts.
    Watch,
    /// The settings new environments start with, and the GPUs settings can
    /// name.
    Settings,
    /// Replace the settings new environments start with.
    SetSettings {
        settings: Settings,
    },
    /// Replace the settings of `name`; they apply from its next start, but
    /// the internet setting of a running environment applies at once.
    SetEnvironmentSettings {
        name: String,
        settings: Settings,
    },
    /// The latest [`JournalEntry`]s of `name`, oldest first.
    Journal {
        name: String,
    },
    /// The programs `guest` runs in the running environment `name`.
    Processes {
        name: String,
    },
    /// End one process of `guest`: asked to quit, or killed with `force`.
    EndProcess {
        name: String,
        pid: u32,
        force: bool,
    },
    /// Kill everything a [`GuestApp`] started.
    EndApp {
        name: String,
        unit: String,
    },
    /// What `guest`'s Inbox and Outbox hold.
    Files {
        name: String,
    },
    /// Copy files and folders of the host into `guest`'s Inbox, each under
    /// its own name, a folder with what it holds. The service reads them as
    /// the user who asks, so only what they could read themselves.
    SendFiles {
        name: String,
        paths: Vec<PathBuf>,
    },
    /// Copy everything in `guest`'s Outbox that passes the checks for
    /// outbound files to the client: [`Reply::FileStart`] and its
    /// [`Reply::FileChunk`]s for each file that passes, [`Reply::Refused`]
    /// for each that does not.
    TakeFiles {
        name: String,
    },
    /// Stop the [`Request::SendFiles`] running for `name`: the files copied
    /// whole stay, the one under way is removed, and the send answers
    /// [`Reply::Stopped`] before it is done. A take stops when its client
    /// closes the connection.
    CancelTransfer {
        name: String,
    },
    /// Stop any running environment or image build and restart the
    /// service, which comes back as whatever build is installed. Answered
    /// before the service exits.
    Restart,
    /// The presets and the image build in progress, if any.
    Presets,
    /// Create or replace the preset of the same name.
    SavePreset {
        preset: Preset,
    },
    DeletePreset {
        name: String,
    },
    /// The presets the service ships with, to start new presets from.
    PresetTemplates,
    /// Give the preset `name` the name `to`, with its image and every
    /// environment made from it. Nothing may be running or building.
    RenamePreset {
        name: String,
        to: String,
    },
    /// Build the image `images/<name>` from the base image and the preset
    /// `name`, replacing the old image once the build succeeds; the build's
    /// steps follow until it ends. Environments created from the old image
    /// are reset onto the new one. Nothing may be running.
    BuildPreset {
        name: String,
    },
    /// Build the base image every preset image is built on, replacing the
    /// old one once the build succeeds; the build's steps follow until it
    /// ends. The service fetches the Fedora Cloud image of the release its
    /// settings name (`fedora_release`, by default the one the base image's
    /// provisioning script is made for) and checks it against Fedora's
    /// checksum, reporting `download` and then `download <percent>%` with
    /// the bytes received and in all while it downloads; a verified copy
    /// from an earlier build is used without a download. The build goes by the name [`BASE_IMAGE`]. Images built
    /// on the old base image keep working. Nothing may be running.
    BuildBase,
    /// Stop the image build in progress; the old image stays.
    StopBuild,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case")]
pub enum Reply {
    Hello {
        version: u32,
        /// The service's package version, for people.
        build: String,
    },
    Environments {
        environments: Vec<Environment>,
    },
    Images {
        images: Vec<String>,
    },
    Settings {
        settings: Settings,
        gpus: Vec<Gpu>,
    },
    Journal {
        entries: Vec<JournalEntry>,
    },
    Processes {
        apps: Vec<GuestApp>,
    },
    Files {
        files: GuestFiles,
    },
    /// An Outbox file that passed the checks begins; its chunks follow.
    FileStart {
        path: String,
        size: u64,
    },
    /// How far a [`Request::SendFiles`] got: `done` of its `total` bytes
    /// copied, now and then while it runs and once at its end.
    Sending {
        done: u64,
        total: u64,
    },
    /// A [`Request::SendFiles`] stopped by [`Request::CancelTransfer`]:
    /// `copied` of its `files` went in whole.
    Stopped {
        copied: usize,
        files: usize,
    },
    /// The next bytes of the file begun last, in base64.
    FileChunk {
        data: String,
    },
    /// An Outbox file the checks refused.
    Refused {
        path: String,
        reason: String,
    },
    Progress {
        stage: Stage,
    },
    Presets {
        presets: Vec<Preset>,
        build: Option<Build>,
        /// The base image every preset image is built on; absent while
        /// there is none.
        #[serde(default)]
        base: Option<BaseImage>,
        /// The presets whose images were built on an older base image.
        #[serde(default)]
        stale: Vec<String>,
        /// The last build of each preset, and of the base image, that
        /// failed; each stays until its image builds again.
        #[serde(default)]
        failed: Vec<FailedBuild>,
    },
    PresetTemplates {
        templates: Vec<Preset>,
    },
    /// The image build reached `step`: `prepare` as it begins, then the
    /// names its provisioning script reports, and `flatten` once the build
    /// VM passed. A base image build reports its download before those:
    /// `download`, then `download <percent>%` with the bytes in `download`.
    BuildStep {
        step: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        download: Option<Downloaded>,
    },
    Event {
        event: Event,
    },
    /// The request is complete.
    Done,
    /// The request failed; nothing more follows for it.
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    pub image: String,
    /// Whether its image exists; until then the environment cannot start.
    pub image_ready: bool,
    /// When it was created, in seconds since the Unix epoch.
    #[serde(default)]
    pub created: u64,
    /// When it last started, in seconds since the Unix epoch; never when
    /// absent.
    #[serde(default)]
    pub last_started: Option<u64>,
    /// What it gets at its next start.
    pub settings: Settings,
    /// Present while this environment runs.
    pub running: Option<Running>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    pub stage: Stage,
    pub internet: bool,
    /// The screen memory and input socket, given to the user who started
    /// the environment; absent when the GPU drives a monitor of its own.
    pub screen: Option<ScreenAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenAccess {
    pub memory: PathBuf,
    pub input: PathBuf,
}

/// Where an environment start is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Checking the GPU and preparing the screen and the network.
    Preparing,
    /// Moving the GPU to vfio-pci; on a single-GPU machine the host
    /// desktop goes away here.
    SwitchingGpu,
    /// QEMU is up and the guest boots.
    Booting,
    /// The guest's desktop is up.
    Ready,
    /// The host was asked to stop it: QEMU is killed and the GPU goes back
    /// to the host.
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    Progress {
        name: String,
        stage: Stage,
    },
    /// The environment stopped; `error` says why when it was not asked to.
    Stopped {
        name: String,
        error: Option<String>,
    },
    Internet {
        on: bool,
    },
    /// The set of environments or images changed.
    Changed,
    /// The settings changed.
    SettingsChanged,
    /// The set of presets or one of them changed.
    PresetsChanged,
    /// The image build of `preset`, or of the base image when that is
    /// [`BASE_IMAGE`], reached `step` (see [`Reply::BuildStep`]).
    Build {
        preset: String,
        step: String,
        /// How far the download is, while `step` is `download <percent>%`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        download: Option<Downloaded>,
    },
    /// The image build of `preset`, or of the base image when that is
    /// [`BASE_IMAGE`], ended; `error` says why when it did not produce an
    /// image.
    BuildFinished {
        preset: String,
        error: Option<String>,
    },
    /// Something happened to the environment `name`.
    Journal {
        name: String,
        entry: JournalEntry,
    },
}

/// A process of `guest`, as the environment's agent reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestProcess {
    pub pid: u32,
    pub name: String,
    /// Its command line, cut short.
    pub command: String,
    pub memory_kib: u64,
    pub cpu_seconds: u64,
}

/// A program started from a menu entry, with every process it started.
/// The processes of `guest` outside any such program come as one with no
/// `unit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestApp {
    pub unit: Option<String>,
    pub title: String,
    /// The title of the program whose process started this one, such as
    /// the launcher of a game; none for a program started from a menu
    /// entry, which `guest`'s systemd starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<String>,
    pub processes: Vec<GuestProcess>,
}

/// A file in `guest`'s Inbox or Outbox, by its path below that folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestFile {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestFiles {
    pub inbox: Vec<GuestFile>,
    pub outbox: Vec<GuestFile>,
}

/// File name endings an Outbox never hands out, with what such a file is:
/// they run or start something when opened.
const KEPT_BY_NAME: [(&str, &str); 22] = [
    ("exe", "a program"),
    ("dll", "a program"),
    ("com", "a program"),
    ("scr", "a program"),
    ("jar", "a program"),
    ("msi", "an installer"),
    ("bat", "a script"),
    ("cmd", "a script"),
    ("ps1", "a script"),
    ("vbs", "a script"),
    ("vbe", "a script"),
    ("js", "a script"),
    ("jse", "a script"),
    ("wsf", "a script"),
    ("hta", "a script"),
    ("sh", "a script"),
    ("py", "a script"),
    ("lnk", "a shortcut"),
    ("desktop", "a shortcut"),
    ("docm", "a document with macros"),
    ("xlsm", "a document with macros"),
    ("pptm", "a document with macros"),
];

/// What the Outbox file at `path` is when its name alone keeps it inside,
/// such as "a program". The service also refuses files by how they start,
/// which only a take finds out.
#[must_use]
pub fn kept_by_name(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    KEPT_BY_NAME
        .iter()
        .find(|(kept, _)| *kept == extension)
        .map(|(_, what)| *what)
}

/// What the host and an environment's agent say to each other over the
/// virtio-serial port [`agent::PORT`]. The host always asks; the agent only
/// answers, with the id of the call. Every line is a [`agent::Message`].
pub mod agent {
    use serde::{Deserialize, Serialize};

    use crate::{GuestApp, GuestFiles};

    /// The port's name; the guest sees it as `/dev/virtio-ports/<PORT>`.
    pub const PORT: &str = "io.github.sovgut.gse.agent";
    /// The port the guest streams its journal to, one way; the host only
    /// keeps what arrives, capped, and never parses it.
    pub const JOURNAL_PORT: &str = "io.github.sovgut.gse.journal";
    /// The agent's version: 2 knows [`Call::Incoming`]. A host speaks to
    /// any agent from [`MIN_VERSION`] on, so environments made from an older
    /// image keep working.
    pub const VERSION: u32 = 2;
    pub const MIN_VERSION: u32 = 1;
    /// Raw bytes per chunk of a file; in base64 a chunk stays well under
    /// [`crate::MAX_LINE`].
    pub const CHUNK: usize = 32 * 1024;
    /// The largest file either way.
    pub const MAX_FILE: u64 = 4 << 30;
    /// The most files a listing of the Inbox or the Outbox holds.
    pub const MAX_FILES: usize = 10_000;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Message<T> {
        pub id: u64,
        #[serde(flatten)]
        pub body: T,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "call", rename_all = "kebab-case")]
    pub enum Call {
        Hello,
        Processes,
        EndProcess {
            pid: u32,
            force: bool,
        },
        /// Kill the unit `unit` and everything in it.
        EndApp {
            unit: String,
        },
        Files,
        /// A file of `size` bytes for the Inbox at `path`; its
        /// [`Call::Chunk`]s and a [`Call::PutEnd`] follow, all under the
        /// same id, and only the end is answered.
        Put {
            path: String,
            size: u64,
            /// Whether the file runs, as it did on the host.
            #[serde(default)]
            executable: bool,
        },
        Chunk {
            data: String,
        },
        PutEnd {
            sha256: String,
        },
        /// `files` of `bytes` in all follow as [`Call::Put`]s: the files
        /// the host kept for this start. The guest shows them arriving on
        /// its desktop. Version 2 on.
        Incoming {
            files: u32,
            bytes: u64,
        },
        /// The Outbox file at `path`, answered with [`Answer::Chunk`]s and
        /// an [`Answer::GetEnd`].
        Get {
            path: String,
        },
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "answer", rename_all = "kebab-case")]
    pub enum Answer {
        Hello { version: u32 },
        Processes { apps: Vec<GuestApp> },
        Files { files: GuestFiles },
        Chunk { data: String },
        GetEnd { size: u64, sha256: String },
        Done,
        Error { message: String },
    }
}

/// One line of an environment's journal, written by the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub text: String,
}

/// What an image holds on top of the base image; see
/// `gse_host::preset` for the file the service keeps it in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// Also the name of its file, of the image built from it and of the
    /// image its environments name, so [`valid_name`] applies; a new title
    /// leaves it as it is.
    pub name: String,
    /// What people call it, such as "Game launchers": up to
    /// [`MAX_TITLE`] characters on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub apps: Vec<PresetApp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetApp {
    /// The name on its menu entry.
    pub name: String,
    pub source: AppSource,
    /// The desktop file of the package that this app's menu entry replaces.
    pub desktop: Option<String>,
    /// What to run; empty for a Flatpak or an AppImage, whose command
    /// follows from the source.
    pub command: Vec<String>,
    /// Runs as `sandbox` instead of `guest`.
    pub trusted: bool,
}

/// Where an application comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AppSource {
    /// Fedora or RPM Fusion packages; such an app needs a command.
    Dnf {
        packages: Vec<String>,
        /// Repositories enabled while installing them.
        repos: Vec<String>,
    },
    /// A Flathub application ID.
    Flatpak { id: String },
    /// An AppImage file on the host, read by the service when it builds.
    Appimage { path: PathBuf },
}

/// The base image, as the service sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseImage {
    /// When it was built, in seconds since the Unix epoch.
    pub built: u64,
    /// Whether it holds the guest program and provisioning of this service;
    /// an older one lacks what the service now puts into images, and the
    /// preset images built on it lack it too.
    pub current: bool,
}

/// An image build in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    /// The preset, or [`BASE_IMAGE`] for the base image.
    pub preset: String,
    /// The steps reported so far, oldest first; how far a download has
    /// come (`download <percent>%`) is not kept.
    pub steps: Vec<String>,
    /// When it started, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<u64>,
}

/// How far a download has come, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Downloaded {
    pub received: u64,
    /// What it holds in all.
    pub total: u64,
}

/// The last build of an image that failed, kept until the image builds
/// again. A build that was stopped did not fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedBuild {
    /// The preset, or [`BASE_IMAGE`] for the base image.
    pub preset: String,
    /// The last step it reported (see [`Reply::BuildStep`]).
    pub step: Option<String>,
    /// The build VM's console as the service keeps it; none when the build
    /// failed before its VM ran.
    pub log: Option<PathBuf>,
}

/// Keyboard layouts as XKB names them, the first one active, and the XKB
/// options that go with them, such as the key that switches layouts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyboard {
    /// Layouts with their variant, if any: `us`, `ru`, `us(intl)`.
    pub layouts: Vec<String>,
    /// Such as `grp:win_space_toggle`.
    pub options: Vec<String>,
}

impl Keyboard {
    /// The lines the guest reads from the `fw_cfg` item `opt/gse/keyboard`:
    /// `layouts=us,ru` and `options=grp:win_space_toggle`.
    #[must_use]
    pub fn to_guest_text(&self) -> String {
        format!(
            "layouts={}\noptions={}\n",
            self.layouts.join(","),
            self.options.join(",")
        )
    }
}

/// What an environment gets when it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// PCI address of the GPU environments get, as `0000:0e:00.0`.
    pub gpu: String,
    /// The environment screen's mode, as [`ScreenMode`] writes it.
    pub screen_mode: String,
    /// Whether it reaches the internet.
    #[serde(default = "internet_by_default")]
    pub internet: bool,
}

const fn internet_by_default() -> bool {
    true
}

/// A GPU of the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    /// PCI address of its display function, as `0000:0e:00.0`.
    pub address: String,
    pub vendor_id: u16,
    pub device_id: u16,
    /// Names from the PCI ID database, when it knows them.
    pub vendor: Option<String>,
    pub device: Option<String>,
    /// The kernel driver bound to it, if any.
    pub driver: Option<String>,
    /// Whether a monitor is connected to it.
    pub monitor: bool,
    /// That monitor's preferred mode, as `1920x1080@60`, when its EDID
    /// says: what a sandbox that takes this GPU shows.
    #[serde(default)]
    pub monitor_mode: Option<String>,
    /// How it changes hands when an environment gets it.
    pub mode: SwitchMode,
}

/// How a GPU changes hands; see `gse_host::plan::Mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SwitchMode {
    /// The GPU drives the host desktop, which stops while an environment runs.
    Single,
    /// Another GPU drives the desktop.
    Secondary,
}

/// `WIDTHxHEIGHT@RATE`, such as `2560x1440@60`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenMode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in frames per second.
    pub rate: u32,
}

impl ScreenMode {
    pub const MAX_WIDTH: u32 = 7680;
    pub const MAX_HEIGHT: u32 = 4320;
    pub const MIN_SIDE: u32 = 320;
    pub const RATES: std::ops::RangeInclusive<u32> = 24..=240;
}

impl fmt::Display for ScreenMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.rate)
    }
}

impl FromStr for ScreenMode {
    type Err = String;

    /// Accepts modes within [`Self::MAX_WIDTH`] by [`Self::MAX_HEIGHT`],
    /// sides of at least [`Self::MIN_SIDE`] and rates in [`Self::RATES`].
    fn from_str(s: &str) -> Result<Self, String> {
        let number = |text: &str| -> Result<u32, String> {
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("'{s}' is not WIDTHxHEIGHT@RATE"));
            }
            text.parse().map_err(|_| format!("'{s}' is out of range"))
        };
        let (size, rate) = s
            .split_once('@')
            .ok_or_else(|| format!("'{s}' is not WIDTHxHEIGHT@RATE"))?;
        let (width, height) = size
            .split_once('x')
            .ok_or_else(|| format!("'{s}' is not WIDTHxHEIGHT@RATE"))?;
        let mode = Self {
            width: number(width)?,
            height: number(height)?,
            rate: number(rate)?,
        };
        if !(Self::MIN_SIDE..=Self::MAX_WIDTH).contains(&mode.width)
            || !(Self::MIN_SIDE..=Self::MAX_HEIGHT).contains(&mode.height)
        {
            return Err(format!(
                "{}x{} is outside {min}x{min} to {}x{}",
                mode.width,
                mode.height,
                Self::MAX_WIDTH,
                Self::MAX_HEIGHT,
                min = Self::MIN_SIDE,
            ));
        }
        if !Self::RATES.contains(&mode.rate) {
            return Err(format!(
                "{} frames per second is outside {}-{}",
                mode.rate,
                Self::RATES.start(),
                Self::RATES.end()
            ));
        }
        Ok(mode)
    }
}

/// Writes `message` as one line.
pub fn send(to: &mut (impl Write + ?Sized), message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    to.write_all(&line)?;
    to.flush()
}

/// Reads one message line; `None` at the end of the stream.
pub fn receive<T: for<'de> Deserialize<'de>>(from: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    let read = io::Read::take(&mut *from, MAX_LINE as u64 + 1).read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Whether `name` can name an environment, an image or a preset: it
/// becomes a directory name and part of file names.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_stays_readable_across_versions() {
        // Every service answers this exact line, whatever else changed.
        let mut wire = Vec::new();
        send(&mut wire, &Request::Hello).unwrap();
        assert_eq!(wire, b"{\"request\":\"hello\"}\n");
        let reply: Reply =
            receive(&mut &b"{\"reply\":\"hello\",\"version\":7,\"build\":\"0.1.0\"}\n"[..])
                .unwrap()
                .unwrap();
        assert_eq!(
            reply,
            Reply::Hello {
                version: 7,
                build: "0.1.0".into()
            }
        );
    }

    #[test]
    fn requests_round_trip_as_tagged_lines() {
        let mut wire = Vec::new();
        send(
            &mut wire,
            &Request::Start {
                name: "test".into(),
                keyboard: None,
            },
        )
        .unwrap();
        assert_eq!(wire, b"{\"request\":\"start\",\"name\":\"test\"}\n");
        let back: Request = receive(&mut wire.as_slice()).unwrap().unwrap();
        assert_eq!(
            back,
            Request::Start {
                name: "test".into(),
                keyboard: None,
            }
        );
        assert!(receive::<Request>(&mut &b""[..]).unwrap().is_none());
    }

    #[test]
    fn replies_carry_events() {
        let mut wire = Vec::new();
        let reply = Reply::Event {
            event: Event::Progress {
                name: "test".into(),
                stage: Stage::SwitchingGpu,
            },
        };
        send(&mut wire, &reply).unwrap();
        assert!(String::from_utf8_lossy(&wire).contains("\"stage\":\"switching-gpu\""));
        assert_eq!(receive::<Reply>(&mut wire.as_slice()).unwrap(), Some(reply));
    }

    #[test]
    fn settings_round_trip() {
        let mut wire = Vec::new();
        let request = Request::SetSettings {
            settings: Settings {
                gpu: "0000:0e:00.0".into(),
                screen_mode: "1920x1080@144".into(),
                internet: true,
            },
        };
        send(&mut wire, &request).unwrap();
        assert!(wire.starts_with(b"{\"request\":\"set-settings\",\"settings\":{"));
        assert_eq!(
            receive::<Request>(&mut wire.as_slice()).unwrap(),
            Some(request)
        );

        let mut wire = Vec::new();
        let reply = Reply::Settings {
            settings: Settings {
                gpu: "0000:0e:00.0".into(),
                screen_mode: "2560x1440@60".into(),
                internet: true,
            },
            gpus: vec![Gpu {
                address: "0000:0e:00.0".into(),
                vendor_id: 0x10de,
                device_id: 0x2484,
                vendor: Some("NVIDIA Corporation".into()),
                device: None,
                driver: Some("nouveau".into()),
                monitor: false,
                monitor_mode: Some("1920x1080@60".into()),
                mode: SwitchMode::Secondary,
            }],
        };
        send(&mut wire, &reply).unwrap();
        assert!(String::from_utf8_lossy(&wire).contains("\"mode\":\"secondary\""));
        assert_eq!(receive::<Reply>(&mut wire.as_slice()).unwrap(), Some(reply));

        let mut wire = Vec::new();
        let event = Reply::Event {
            event: Event::SettingsChanged,
        };
        send(&mut wire, &event).unwrap();
        assert_eq!(
            wire,
            b"{\"reply\":\"event\",\"event\":{\"event\":\"settings-changed\"}}\n"
        );
        assert_eq!(receive::<Reply>(&mut wire.as_slice()).unwrap(), Some(event));
    }

    #[test]
    fn the_base_image_state_round_trips() {
        let reply = Reply::Presets {
            presets: Vec::new(),
            build: None,
            base: Some(BaseImage {
                built: 1,
                current: false,
            }),
            stale: vec!["launchers".into()],
            failed: Vec::new(),
        };
        let mut wire = Vec::new();
        send(&mut wire, &reply).unwrap();
        assert_eq!(receive::<Reply>(&mut wire.as_slice()).unwrap(), Some(reply));
        // A service without the field answers without it.
        let old = b"{\"reply\":\"presets\",\"presets\":[],\"build\":null}\n";
        assert!(matches!(
            receive::<Reply>(&mut old.as_slice()).unwrap(),
            Some(Reply::Presets { base: None, .. })
        ));
    }

    #[test]
    fn what_an_older_peer_leaves_out_reads_as_nothing() {
        let presets = b"{\"reply\":\"presets\",\"presets\":[{\"name\":\"wow\",\"apps\":[]}],\
                        \"build\":{\"preset\":\"wow\",\"steps\":[\"prepare\"]}}\n";
        let Some(Reply::Presets {
            presets,
            build: Some(build),
            failed,
            ..
        }) = receive::<Reply>(&mut presets.as_slice()).unwrap()
        else {
            panic!("not a presets reply");
        };
        assert_eq!(presets[0].title, None);
        assert_eq!(build.started, None);
        assert_eq!(failed, []);

        let event = b"{\"reply\":\"event\",\"event\":{\"event\":\"build\",\"preset\":\"base\",\
                      \"step\":\"download 43%\"}}\n";
        assert!(matches!(
            receive::<Reply>(&mut event.as_slice()).unwrap(),
            Some(Reply::Event {
                event: Event::Build { download: None, .. }
            })
        ));

        let answer =
            b"{\"id\":2,\"answer\":\"processes\",\"apps\":[{\"unit\":\"guest-app-1.service\",\
                       \"title\":\"Steam\",\"processes\":[]}]}\n";
        let Some(agent::Message {
            body: agent::Answer::Processes { apps },
            ..
        }) = receive::<agent::Message<agent::Answer>>(&mut answer.as_slice()).unwrap()
        else {
            panic!("not a list of programs");
        };
        assert_eq!(apps[0].started_by, None);
    }

    #[test]
    fn a_download_reports_its_bytes_beside_its_percentage() {
        let event = Reply::Event {
            event: Event::Build {
                preset: BASE_IMAGE.into(),
                step: "download 43%".into(),
                download: Some(Downloaded {
                    received: 1_200_000_000,
                    total: 2_800_000_000,
                }),
            },
        };
        let mut wire = Vec::new();
        send(&mut wire, &event).unwrap();
        assert!(
            String::from_utf8_lossy(&wire)
                .contains("\"download\":{\"received\":1200000000,\"total\":2800000000}")
        );
        assert_eq!(receive::<Reply>(&mut wire.as_slice()).unwrap(), Some(event));
    }

    #[test]
    fn every_request_round_trips() {
        for request in [
            Request::Processes { name: "x".into() },
            Request::EndProcess {
                name: "x".into(),
                pid: 7,
                force: true,
            },
            Request::SendFiles {
                name: "x".into(),
                paths: vec!["/home/me/save.dat".into()],
            },
            Request::TakeFiles { name: "x".into() },
            Request::CancelTransfer { name: "x".into() },
            Request::Rename {
                name: "x".into(),
                to: "y".into(),
            },
            Request::Journal { name: "x".into() },
            Request::SetEnvironmentSettings {
                name: "x".into(),
                settings: Settings {
                    gpu: "0000:0e:00.0".into(),
                    screen_mode: "1920x1080@60".into(),
                    internet: true,
                },
            },
            Request::Presets,
            Request::PresetTemplates,
            Request::DeletePreset { name: "x".into() },
            Request::RenamePreset {
                name: "x".into(),
                to: "y".into(),
            },
            Request::BuildPreset { name: "x".into() },
            Request::BuildBase,
            Request::StopBuild,
        ] {
            let mut wire = Vec::new();
            send(&mut wire, &request).unwrap();
            assert_eq!(
                receive::<Request>(&mut wire.as_slice()).unwrap(),
                Some(request)
            );
        }
    }

    #[test]
    fn presets_round_trip() {
        let preset = Preset {
            name: "launchers".into(),
            title: Some("Game launchers".into()),
            apps: vec![
                PresetApp {
                    name: "Steam".into(),
                    source: AppSource::Dnf {
                        packages: vec!["steam".into()],
                        repos: Vec::new(),
                    },
                    desktop: Some("steam.desktop".into()),
                    command: vec!["steam".into()],
                    trusted: false,
                },
                PresetApp {
                    name: "Discord".into(),
                    source: AppSource::Flatpak {
                        id: "com.discordapp.Discord".into(),
                    },
                    desktop: None,
                    command: Vec::new(),
                    trusted: true,
                },
                PresetApp {
                    name: "Epic".into(),
                    source: AppSource::Appimage {
                        path: "/home/me/epic.appimage".into(),
                    },
                    desktop: None,
                    command: Vec::new(),
                    trusted: false,
                },
            ],
        };
        let mut wire = Vec::new();
        let request = Request::SavePreset {
            preset: preset.clone(),
        };
        send(&mut wire, &request).unwrap();
        let text = String::from_utf8_lossy(&wire).into_owned();
        assert!(text.starts_with("{\"request\":\"save-preset\",\"preset\":{"));
        assert!(text.contains("\"name\":\"launchers\",\"title\":\"Game launchers\","));
        assert!(
            text.contains("\"source\":{\"kind\":\"flatpak\",\"id\":\"com.discordapp.Discord\"}")
        );
        assert!(text.contains("\"kind\":\"appimage\",\"path\":\"/home/me/epic.appimage\""));
        assert_eq!(
            receive::<Request>(&mut wire.as_slice()).unwrap(),
            Some(request)
        );

        let mut wire = Vec::new();
        let reply = Reply::Presets {
            presets: vec![preset],
            build: Some(Build {
                preset: "launchers".into(),
                steps: vec!["packages".into()],
                started: Some(1_793_545_989),
            }),
            base: None,
            stale: Vec::new(),
            failed: vec![FailedBuild {
                preset: BASE_IMAGE.into(),
                step: Some("NVIDIA driver".into()),
                log: Some("/var/log/gse/builds/1793545000-base-failed.log".into()),
            }],
        };
        send(&mut wire, &reply).unwrap();
        assert!(wire.starts_with(b"{\"reply\":\"presets\","));
        assert_eq!(receive::<Reply>(&mut wire.as_slice()).unwrap(), Some(reply));
        let mut wire = Vec::new();
        send(&mut wire, &Request::BuildPreset { name: "x".into() }).unwrap();
        assert_eq!(wire, b"{\"request\":\"build-preset\",\"name\":\"x\"}\n");
    }

    #[test]
    fn the_base_image_build_reports_under_its_own_name() {
        let mut wire = Vec::new();
        send(&mut wire, &Request::BuildBase).unwrap();
        assert_eq!(wire, b"{\"request\":\"build-base\"}\n");
        // The name would pass for a preset's; the service refuses presets
        // of that name.
        assert!(valid_name(BASE_IMAGE));
        let event = Event::BuildFinished {
            preset: BASE_IMAGE.into(),
            error: None,
        };
        let mut wire = Vec::new();
        send(&mut wire, &Reply::Event { event }).unwrap();
        assert_eq!(
            wire,
            b"{\"reply\":\"event\",\"event\":{\"event\":\"build-finished\",\"preset\":\"base\",\"error\":null}}\n"
        );
    }

    #[test]
    fn builds_report_steps_and_their_end() {
        for reply in [
            Reply::BuildStep {
                step: "flatpaks".into(),
                download: None,
            },
            Reply::BuildStep {
                step: "download 2%".into(),
                download: Some(Downloaded {
                    received: 12_000_000,
                    total: 600_000_000,
                }),
            },
            Reply::Event {
                event: Event::Build {
                    preset: "launchers".into(),
                    step: "menu".into(),
                    download: None,
                },
            },
            Reply::Event {
                event: Event::BuildFinished {
                    preset: "launchers".into(),
                    error: Some("the build failed (FAILED 12)".into()),
                },
            },
            Reply::Event {
                event: Event::PresetsChanged,
            },
            Reply::Journal {
                entries: vec![JournalEntry {
                    time: 1_793_545_989,
                    text: "Started".into(),
                }],
            },
            Reply::Event {
                event: Event::Journal {
                    name: "demo".into(),
                    entry: JournalEntry {
                        time: 1_793_545_990,
                        text: "Internet cut off".into(),
                    },
                },
            },
        ] {
            let mut wire = Vec::new();
            send(&mut wire, &reply).unwrap();
            assert_eq!(receive::<Reply>(&mut wire.as_slice()).unwrap(), Some(reply));
        }
        let mut wire = Vec::new();
        send(
            &mut wire,
            &Reply::BuildStep {
                step: "menu".into(),
                download: None,
            },
        )
        .unwrap();
        assert_eq!(wire, b"{\"reply\":\"build-step\",\"step\":\"menu\"}\n");
    }

    #[test]
    fn agent_messages_carry_their_id_beside_the_call() {
        use agent::{Answer, Call, Message};
        let call = Message {
            id: 3,
            body: Call::EndProcess {
                pid: 42,
                force: false,
            },
        };
        let mut wire = Vec::new();
        send(&mut wire, &call).unwrap();
        assert_eq!(
            wire,
            b"{\"id\":3,\"call\":\"end-process\",\"pid\":42,\"force\":false}\n"
        );
        assert_eq!(
            receive::<Message<Call>>(&mut wire.as_slice()).unwrap(),
            Some(call)
        );
        let answer = Message {
            id: 3,
            body: Answer::Processes {
                apps: vec![GuestApp {
                    unit: Some("guest-app-1.service".into()),
                    title: "Steam".into(),
                    started_by: None,
                    processes: vec![GuestProcess {
                        pid: 42,
                        name: "steam".into(),
                        command: "steam -silent".into(),
                        memory_kib: 1024,
                        cpu_seconds: 3,
                    }],
                }],
            },
        };
        let mut wire = Vec::new();
        send(&mut wire, &answer).unwrap();
        assert!(!String::from_utf8_lossy(&wire).contains("started_by"));
        assert_eq!(
            receive::<Message<Answer>>(&mut wire.as_slice()).unwrap(),
            Some(answer)
        );
        let game = GuestApp {
            unit: Some("app-flatpak-com.example.Game-7.scope".into()),
            title: "com.example.Game".into(),
            started_by: Some("Steam".into()),
            processes: Vec::new(),
        };
        let mut wire = Vec::new();
        send(&mut wire, &game).unwrap();
        assert!(String::from_utf8_lossy(&wire).contains("\"started_by\":\"Steam\""));
        assert_eq!(
            receive::<GuestApp>(&mut wire.as_slice()).unwrap(),
            Some(game)
        );
    }

    #[test]
    fn screen_modes_parse_within_bounds() {
        let mode: ScreenMode = "2560x1440@60".parse().unwrap();
        assert_eq!(
            mode,
            ScreenMode {
                width: 2560,
                height: 1440,
                rate: 60
            }
        );
        assert_eq!(mode.to_string(), "2560x1440@60");
        assert!("7680x4320@240".parse::<ScreenMode>().is_ok());
        assert!("1920x1080@24".parse::<ScreenMode>().is_ok());
        for bad in [
            "",
            "1920x1080",
            "1920@60",
            "1920x1080@",
            "x1080@60",
            "1920x1080@60.0",
            "+1920x1080@60",
            "1920x1080@23",
            "1920x1080@241",
            "7681x4320@60",
            "7680x4321@60",
            "100x100@60",
            "99999999999x1080@60",
        ] {
            assert!(bad.parse::<ScreenMode>().is_err(), "{bad}");
        }
    }

    #[test]
    fn oversized_and_garbled_lines_are_rejected() {
        let long = vec![b'x'; MAX_LINE + 10];
        assert!(receive::<Request>(&mut long.as_slice()).is_err());
        assert!(receive::<Request>(&mut &b"{\"request\":\"nope\"}\n"[..]).is_err());
    }

    #[test]
    fn names_are_safe_file_names() {
        assert!(valid_name("test"));
        assert!(valid_name("games-2"));
        assert!(!valid_name(""));
        assert!(!valid_name("../x"));
        assert!(!valid_name("Test"));
        assert!(!valid_name("-x"));
        assert!(!valid_name(&"a".repeat(33)));
    }

    #[test]
    fn outbox_files_that_run_stay_by_their_name() {
        assert_eq!(kept_by_name("trainer.exe"), Some("a program"));
        assert_eq!(kept_by_name("mods/Setup.MSI"), Some("an installer"));
        assert_eq!(kept_by_name("run.sh"), Some("a script"));
        assert_eq!(kept_by_name("saves/slot1.sav"), None);
        assert_eq!(kept_by_name("README"), None);
        assert_eq!(kept_by_name("archive.exe.png"), None);
    }
}
