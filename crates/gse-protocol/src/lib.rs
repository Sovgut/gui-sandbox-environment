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
pub const VERSION: u32 = 16;

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
    /// Remove files that wait for the Inbox of `name` (`waiting`) and files
    /// taken out of its Outbox that the app has not taken yet (`taken`), by
    /// the paths [`Request::Files`] lists them under. `name` must not be
    /// running.
    RemoveBuffered {
        name: String,
        waiting: Vec<String>,
        taken: Vec<String>,
    },
    /// Stop any running environment or image build and restart the
    /// service, which comes back as whatever build is installed. Answered
    /// before the service exits.
    Restart,
    /// What the antivirus is: [`Reply::Antivirus`].
    Antivirus,
    /// What the service reads about the machine: [`Reply::Hardware`].
    Hardware,
    /// The devices of the host a sandbox can take: [`Reply::HostDevices`].
    HostDevices,
    /// The packages sandboxes run on that the host lacks:
    /// [`Reply::Packages`].
    Packages,
    /// Where the sandboxes are kept: [`Reply::Storage`].
    Storage,
    /// Move everything the service keeps to the folder `gse` in `to`, or
    /// back to the system disk for `/var/lib`, while nothing runs or
    /// builds; [`Reply::Moving`] says how far it got.
    MoveStorage {
        to: PathBuf,
    },
    /// Stop a [`Request::MoveStorage`] under way; what it copied goes.
    CancelMove,
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
    /// Where the sandboxes are kept, how many bytes they take and how many
    /// are free beside them.
    Storage {
        path: PathBuf,
        used: u64,
        free: u64,
    },
    /// How far a [`Request::MoveStorage`] got: `done` of `total` bytes.
    Moving {
        done: u64,
        total: u64,
    },
    /// The antivirus every file taken out passes: its engine's version,
    /// when its signatures were written and when it last updated, in
    /// seconds since the epoch, and why that update failed if it did.
    Antivirus {
        engine: Option<String>,
        database: Option<u64>,
        checked: Option<u64>,
        error: Option<String>,
    },
    /// The machine as the service reads it, for people and diagnostics,
    /// and how well gse knows its CPU to work.
    Hardware {
        text: String,
        cpu: Support,
    },
    /// The packages sandboxes run on that the running system lacks, and of
    /// those the ones installed for its next start, which a restart brings.
    Packages {
        missing: Vec<String>,
        after_restart: Vec<String>,
    },
    /// The USB devices plugged into the host, and whether it has a sound
    /// card a sandbox can take whole (the board's, behind its 3.5 mm jacks).
    HostDevices {
        usb: Vec<HostUsb>,
        board_sound: bool,
        /// Whether that card can be reset: one that cannot may come back
        /// from a sandbox with its codec silent until the computer
        /// restarts, so it goes only when the settings ask for it.
        #[serde(default)]
        board_sound_resets: bool,
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
    /// The agent's version: 2 knows [`Call::Incoming`], 3 takes files up
    /// to [`MAX_SENT_FILE`], 4 knows [`Call::Collecting`],
    /// [`Call::Outgoing`], [`Call::Collected`] and [`Call::Leaving`], 5
    /// knows [`Call::Import`]. A host speaks to any agent from
    /// [`MIN_VERSION`] on, so environments made from an older image keep
    /// working.
    pub const VERSION: u32 = 5;
    pub const MIN_VERSION: u32 = 1;
    /// Raw bytes per chunk of a file; in base64 a chunk stays well under
    /// [`crate::MAX_LINE`].
    pub const CHUNK: usize = 32 * 1024;
    /// The largest file sent into the Inbox. Agents before version 3 take
    /// at most [`MAX_TAKEN_FILE`].
    pub const MAX_SENT_FILE: u64 = 64 << 30;
    /// The largest file taken out of the Outbox.
    pub const MAX_TAKEN_FILE: u64 = 4 << 30;
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
        /// The host takes the Outbox by itself while this environment runs
        /// (one GPU: the app is gone), and goes on until it stops. Version 4
        /// on.
        Collecting,
        /// `files` of `bytes` in all leave the Outbox now as [`Call::Get`]s;
        /// the guest shows them going. Version 4 on.
        Outgoing {
            files: u32,
            bytes: u64,
        },
        /// What a round of taking the Outbox did: the files that left for
        /// the host and the ones that stay, with why. Version 4 on.
        Collected {
            taken: Vec<crate::GuestFile>,
            refused: Vec<(String, String)>,
        },
        /// The user asked to leave: the host takes the Outbox once more and
        /// then stops the environment. Version 4 on.
        Leaving,
        /// The Outbox file at `path`, answered with [`Answer::Chunk`]s and
        /// an [`Answer::GetEnd`].
        Get {
            path: String,
        },
        /// Put the files of the carrier disk with the serial `serial` (see
        /// [`crate::carrier`]) into the Inbox. Answered with
        /// [`Answer::Importing`] now and then while it copies, and an
        /// [`Answer::Imported`] at its end. Version 5 on.
        Import {
            serial: String,
        },
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "answer", rename_all = "kebab-case")]
    pub enum Answer {
        Hello {
            version: u32,
        },
        Processes {
            apps: Vec<GuestApp>,
        },
        Files {
            files: GuestFiles,
        },
        Chunk {
            data: String,
        },
        GetEnd {
            size: u64,
            sha256: String,
        },
        /// How many bytes of a [`Call::Import`] are in so far.
        Importing {
            done: u64,
        },
        /// How many files of a [`Call::Import`] went into the Inbox, and the
        /// ones that did not, with why.
        Imported {
            put: usize,
            failed: Vec<(String, String)>,
        },
        Done,
        Error {
            message: String,
        },
    }
}

/// A carrier: files the host hands an environment on a read-only disk of
/// their own instead of through the agent's port, which is far too slow for
/// games of tens of gigabytes. A carrier is a plain file on the host and a
/// virtio disk with the serial [`carrier::SERIAL`] and a number in the guest.
///
/// It holds the files back to back, each from a [`carrier::BLOCK`]-aligned
/// offset, then the [`carrier::Index`] as JSON from an aligned offset, and
/// ends with one block, the trailer: [`carrier::MAGIC`], a newline, the
/// index's offset and length in decimal and a newline, padded with zeros.
pub mod carrier {
    use serde::{Deserialize, Serialize};

    pub const BLOCK: u64 = 4096;
    pub const MAGIC: &str = "GSE-CARRIER 1";
    /// The serial of a carrier disk is this and its number.
    pub const SERIAL: &str = "gse-inbox-";
    /// The most carrier disks an environment starts with.
    pub const MAX_DISKS: usize = 8;
    /// The longest index a guest reads.
    pub const MAX_INDEX: u64 = 16 << 20;

    /// A file in a carrier.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Entry {
        /// Where it goes in the Inbox, as a relative path.
        pub path: String,
        pub size: u64,
        pub executable: bool,
        /// Where its bytes start.
        pub offset: u64,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Index {
        pub files: Vec<Entry>,
    }

    /// `n` rounded up to a whole [`BLOCK`].
    #[must_use]
    pub const fn align(n: u64) -> u64 {
        n.div_ceil(BLOCK) * BLOCK
    }

    /// The trailer for an index at `offset`, `len` bytes long.
    #[must_use]
    pub fn trailer(offset: u64, len: u64) -> Vec<u8> {
        let mut block = format!("{MAGIC}\n{offset} {len}\n").into_bytes();
        block.resize(usize::try_from(BLOCK).unwrap_or(4096), 0);
        block
    }

    /// The index's offset and length from a trailer.
    #[must_use]
    pub fn parse_trailer(block: &[u8]) -> Option<(u64, u64)> {
        let text = std::str::from_utf8(block.split(|b| *b == 0).next()?).ok()?;
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let (offset, len) = lines.next()?.split_once(' ')?;
        Some((offset.parse().ok()?, len.parse().ok()?))
    }

    impl Index {
        /// Checks that every file lies whole, aligned and apart from the
        /// others, before `end` (the index's offset), within the sizes the
        /// Inbox takes.
        pub fn check(&self, end: u64) -> Result<(), String> {
            if self.files.len() > crate::agent::MAX_FILES {
                return Err(format!("{} files are too many", self.files.len()));
            }
            let mut free = 0;
            for entry in &self.files {
                if entry.offset % BLOCK != 0 || entry.offset < free {
                    return Err(format!("{} lies where it cannot", entry.path));
                }
                if entry.size > crate::agent::MAX_SENT_FILE {
                    return Err(format!("{} is too large", entry.path));
                }
                free = entry
                    .offset
                    .checked_add(entry.size)
                    .filter(|e| *e <= end)
                    .ok_or_else(|| format!("{} runs past the files", entry.path))?;
            }
            Ok(())
        }

        /// The bytes of all its files.
        #[must_use]
        pub fn bytes(&self) -> u64 {
            self.files.iter().map(|f| f.size).sum()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_trailer_reads_back_and_rejects_anything_else() {
            let block = trailer(8192, 77);
            assert_eq!(block.len(), 4096);
            assert_eq!(parse_trailer(&block), Some((8192, 77)));
            assert_eq!(parse_trailer(&[0; 4096]), None);
            assert_eq!(parse_trailer(b"GSE-CARRIER 2\n1 2\n"), None);
        }

        #[test]
        fn files_must_lie_whole_and_apart() {
            let entry = |offset, size| Entry {
                path: "a".into(),
                size,
                executable: false,
                offset,
            };
            let good = Index {
                files: vec![entry(0, 5000), entry(8192, 10)],
            };
            assert!(good.check(12288).is_ok());
            assert_eq!(good.bytes(), 5010);
            assert!(good.check(8195).is_err());
            assert!(
                Index {
                    files: vec![entry(0, 5000), entry(4096, 10)]
                }
                .check(1 << 20)
                .is_err()
            );
            assert!(
                Index {
                    files: vec![entry(100, 1)]
                }
                .check(1 << 20)
                .is_err()
            );
            assert!(
                Index {
                    files: vec![entry(u64::MAX - 4095, 4096)]
                }
                .check(u64::MAX)
                .is_err()
            );
        }
    }
}

#[cfg(test)]
mod devices_tests {
    use super::*;

    #[test]
    fn choices_override_the_defaults_of_each_mode() {
        let mic = UsbId {
            vendor: 0x0c76,
            product: 0x161e,
            serial: None,
        };
        let mut devices = Devices::default();
        assert!(devices.takes_usb(&mic, UsbKind::Sound, true));
        assert!(!devices.takes_usb(&mic, UsbKind::Sound, false));
        assert!(!devices.takes_usb(&mic, UsbKind::Storage, true));
        assert!(!devices.takes_usb(&mic, UsbKind::Input, true));
        devices.choose_usb(&mic, UsbKind::Sound, true, false);
        assert!(devices.takes_usb(&mic, UsbKind::Sound, false));
        devices.choose_usb(&mic, UsbKind::Sound, false, true);
        assert!(!devices.takes_usb(&mic, UsbKind::Sound, true));
        devices.choose_usb(&mic, UsbKind::Sound, true, true);
        assert!(devices.is_empty());
        assert!(devices.takes_board_sound(true, true));
        assert!(!devices.takes_board_sound(true, false));
        assert!(!devices.takes_board_sound(false, true));
    }

    #[test]
    fn settings_with_devices_survive_toml() {
        let settings = Settings {
            gpu: "0000:09:00.0".into(),
            screen_mode: "1920x1080@60".into(),
            internet: true,
            gpu_driver: None,
            devices: Devices {
                board_sound: Some(false),
                usb: vec![UsbChoice {
                    id: UsbId {
                        vendor: 1,
                        product: 2,
                        serial: Some("A1".into()),
                    },
                    take: true,
                }],
            },
        };
        let text = toml::to_string(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
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
    /// The driver the sandbox runs the GPU with, one of its
    /// [`Gpu::guest_drivers`]; none for the GPU's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_driver: Option<String>,
    /// The host's devices it takes while it runs, beyond the defaults.
    /// Last, since in TOML a table follows the plain values.
    #[serde(default, skip_serializing_if = "Devices::is_empty")]
    pub devices: Devices,
}

/// A USB device by what it is rather than where it is plugged in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UsbId {
    pub vendor: u16,
    pub product: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

/// What a USB device is, as far as a sandbox cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsbKind {
    /// A microphone, a headset, a sound card.
    Sound,
    /// A webcam, usually with a microphone.
    Camera,
    Gamepad,
    Bluetooth,
    /// A flash drive or a disk.
    Storage,
    /// A keyboard or a mouse: it reaches a single-GPU sandbox through QEMU's
    /// `input-linux`, since the way out is read from it, and is never taken.
    Input,
    /// A device gse cannot tell the kind of yet. It is never taken: the
    /// settings list it apart, with its IDs, so one that should go can be
    /// given its kind.
    Other,
}

impl UsbKind {
    /// Whether a single-GPU sandbox takes it unless its settings say no.
    #[must_use]
    pub const fn by_default(self) -> bool {
        matches!(self, Self::Sound | Self::Camera | Self::Gamepad)
    }

    /// Whether a sandbox may take it at all.
    #[must_use]
    pub const fn offered(self) -> bool {
        !matches!(self, Self::Input | Self::Other)
    }
}

/// A USB device of the host, as the settings list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostUsb {
    pub id: UsbId,
    /// What it calls itself.
    pub label: String,
    pub kind: UsbKind,
}

/// Whether a sandbox takes one USB device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbChoice {
    pub id: UsbId,
    pub take: bool,
}

/// The host devices a sandbox takes while it runs, where its settings
/// differ from the defaults: on a single-GPU machine, where the host
/// session is gone meanwhile, the board's sound card and USB sound devices,
/// cameras and gamepads; with another GPU, none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Devices {
    /// The board's sound card, behind its 3.5 mm jacks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_sound: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usb: Vec<UsbChoice>,
}

impl Devices {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.board_sound.is_none() && self.usb.is_empty()
    }

    /// Whether the sandbox takes the board's sound card: by default on a
    /// single-GPU machine (`single`) when the card can be reset (`resets`).
    #[must_use]
    pub fn takes_board_sound(&self, single: bool, resets: bool) -> bool {
        self.board_sound.unwrap_or(single && resets)
    }

    /// Whether the sandbox takes the USB device `id` of `kind`.
    #[must_use]
    pub fn takes_usb(&self, id: &UsbId, kind: UsbKind, single: bool) -> bool {
        kind.offered()
            && self
                .usb
                .iter()
                .find(|c| c.id == *id)
                .map_or(single && kind.by_default(), |c| c.take)
    }

    /// Records whether the sandbox takes `id`; a choice that matches what
    /// it does anyway, on a single GPU when `single`, is dropped.
    pub fn choose_usb(&mut self, id: &UsbId, kind: UsbKind, take: bool, single: bool) {
        self.usb.retain(|c| c.id != *id);
        if take != (single && kind.by_default()) {
            self.usb.push(UsbChoice {
                id: id.clone(),
                take,
            });
        }
    }
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
    /// The family gse knows it as, such as `NVIDIA Ampere`.
    #[serde(default)]
    pub family: Option<String>,
    /// Whether it is part of the CPU or the board rather than a card.
    #[serde(default)]
    pub integrated: bool,
    /// How well gse knows it to work in a sandbox.
    #[serde(default)]
    pub support: Support,
    /// The drivers a sandbox can run it with, its default first; empty
    /// when gse knows none.
    #[serde(default)]
    pub guest_drivers: Vec<String>,
}

/// How well gse knows a piece of hardware to work, worst first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupportLevel {
    /// Known not to work, or known to take the host something it needs.
    Unsupported,
    /// Not run yet, with nothing known against it.
    #[default]
    Expected,
    /// gse has run on it.
    Tested,
}

/// A [`SupportLevel`] and what people should know about it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Support {
    pub level: SupportLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
                gpu_driver: None,
                devices: crate::Devices::default(),
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
                gpu_driver: None,
                devices: crate::Devices::default(),
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
                family: None,
                integrated: false,
                support: Support::default(),
                guest_drivers: Vec::new(),
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
                    gpu_driver: None,
                    devices: crate::Devices::default(),
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
