//! The environment's agent: answers the host over the virtio-serial port
//! [`agent::PORT`], as root, and never speaks first. It lists the processes
//! of `guest` by the program each belongs to, with the program that
//! started each program, ends them, and moves files into `guest`'s
//! `~/Inbox` and out of `~/Outbox`.
//!
//! The port's device node belongs to root, so no program of `guest` can
//! stand in for the agent. `guest` owns the two folders, though, and can
//! swap anything in them at any moment: every path below them is opened in
//! one `openat2` call that refuses symbolic links and leaving the folder,
//! new files and folders are given to `guest`, and a file only leaves when
//! it is a regular file with a single link that `guest` owns.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use gse_protocol::agent::{self, Answer, Call, Message};
use gse_protocol::{GuestApp, GuestFile, GuestFiles, GuestProcess, receive, send};
use rustix::fs::{FileType, Gid, Mode, OFlags, ResolveFlags, Uid};
use rustix::process::{Pid, PidfdFlags, Signal};
use sha2::{Digest as _, Sha256};

pub(crate) const GUEST: &str = "guest";
/// The units of `guest`'s own systemd that run no program a user started.
/// Units of `guest`'s systemd that the desktop's own services run in, by
/// name or by the start of it. A listing leaves them out; malware can name
/// a unit of its own like one of them, so stopping the sandbox is what
/// ends everything for certain.
const INFRASTRUCTURE: [&str; 13] = [
    "init.scope",
    "gse-launch.service",
    "gse-xwayland.service",
    "dbus-broker.service",
    "dbus.service",
    "dbus-:",
    "at-spi-dbus-bus.service",
    "gvfs-",
    "xdg-",
    "flatpak-portal.service",
    "flatpak-session-helper.service",
    "dconf.service",
    "p11-kit-server.service",
];
/// How much of a command line a process listing carries.
const MAX_COMMAND: usize = 300;
/// How deep a path below the Inbox or the Outbox may go.
const MAX_DEPTH: usize = 16;
/// How many parents up from a program's first process the program that
/// started it is looked for.
const MAX_ANCESTRY: usize = 64;

/// The `guest` account.
#[derive(Debug, Clone, Copy)]
struct Account {
    uid: u32,
    gid: u32,
}

fn account() -> Result<Account> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    passwd
        .lines()
        .find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            (fields.first() == Some(&GUEST) && fields.len() > 3)
                .then(|| {
                    Some(Account {
                        uid: fields[2].parse().ok()?,
                        gid: fields[3].parse().ok()?,
                    })
                })
                .flatten()
        })
        .context("no guest account")
}

/// Answers the host for as long as the guest runs. The port stays open the
/// whole time: what the host sends while the guest has it closed is thrown
/// away, so a host that connects just then would wait for an answer to a
/// call the agent never saw.
pub fn serve() -> Result<()> {
    let guest = account()?;
    let path = format!("/dev/virtio-ports/{}", agent::PORT);
    let port = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| path.clone())?;
    let mut agent = Agent {
        guest,
        reader: BufReader::new(port.try_clone()?),
        writer: port,
        upload: None,
        incoming: None,
        outgoing: None,
    };
    crate::incoming::install_outbox_wait();
    loop {
        if let Err(e) = agent.serve_connection() {
            eprintln!("agent: {e:#}");
        }
        // The port reads as ended while the host is not connected.
        agent.abandon_upload();
        sleep(Duration::from_millis(100));
    }
}

/// A file arriving in the Inbox.
struct Upload {
    id: u64,
    file: File,
    /// Where it is, for removing it when it does not arrive whole.
    dir: OwnedFd,
    name: String,
    remaining: u64,
    hash: Sha256,
}

struct Agent {
    guest: Account,
    reader: BufReader<File>,
    writer: File,
    upload: Option<Upload>,
    /// The files the host kept for this start, while they arrive.
    incoming: Option<crate::incoming::Batch>,
    /// The files the host takes out of the Outbox now, while they leave.
    outgoing: Option<crate::incoming::Batch>,
}

impl Agent {
    fn answer(&mut self, id: u64, body: Answer) -> Result<()> {
        send(&mut self.writer, &Message { id, body })?;
        Ok(())
    }

    fn serve_connection(&mut self) -> Result<()> {
        loop {
            let message = match receive::<Message<Call>>(&mut self.reader) {
                Ok(Some(message)) => message,
                Ok(None) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    self.answer(
                        0,
                        Answer::Error {
                            message: format!("unreadable call: {e}"),
                        },
                    )?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let id = message.id;
            match self.handle(message) {
                Ok(Some(body)) => self.answer(id, body)?,
                Ok(None) => {}
                Err(e) => self.answer(
                    id,
                    Answer::Error {
                        message: format!("{e:#}"),
                    },
                )?,
            }
        }
    }

    /// Answers one call; an upload's chunks are answered only at its end.
    fn handle(&mut self, message: Message<Call>) -> Result<Option<Answer>> {
        let Message { id, body } = message;
        if !matches!(body, Call::Chunk { .. } | Call::PutEnd { .. })
            || self.upload.as_ref().is_some_and(|u| u.id != id)
        {
            self.abandon_upload();
        }
        // A host that moved on to something else sends no more of the
        // batch: it ends short, so the desktop stops waiting for it.
        if !matches!(
            body,
            Call::Put { .. } | Call::Chunk { .. } | Call::PutEnd { .. }
        ) && let Some(mut batch) = self.incoming.take()
        {
            batch.end();
        }
        Ok(Some(match body {
            Call::Hello => Answer::Hello {
                version: agent::VERSION,
            },
            Call::Processes => Answer::Processes {
                apps: processes(self.guest),
            },
            Call::EndProcess { pid, force } => {
                end_process(self.guest, pid, force)?;
                Answer::Done
            }
            Call::EndApp { unit } => {
                end_app(self.guest, &unit)?;
                Answer::Done
            }
            Call::Files => Answer::Files {
                files: GuestFiles {
                    inbox: list(&format!("/home/{GUEST}/Inbox")),
                    outbox: list(&format!("/home/{GUEST}/Outbox")),
                },
            },
            Call::Put {
                path,
                size,
                executable,
            } => {
                self.start_upload(id, &path, size, executable)?;
                return Ok(None);
            }
            Call::Chunk { data } => {
                self.chunk(&data)?;
                return Ok(None);
            }
            Call::PutEnd { sha256 } => {
                self.finish_upload(&sha256)?;
                Answer::Done
            }
            Call::Get { path } => {
                self.get(id, &path)?;
                return Ok(None);
            }
            // An empty batch only ends the one before, which the check
            // above did.
            Call::Incoming { files, bytes } => {
                if files > 0 {
                    self.incoming = Some(crate::incoming::Batch::new(
                        crate::incoming::Kind::In,
                        files,
                        bytes,
                    ));
                }
                Answer::Done
            }
            Call::Collecting => {
                crate::incoming::collecting();
                Answer::Done
            }
            Call::Outgoing { files, bytes } => {
                if files > 0 {
                    self.outgoing = Some(crate::incoming::Batch::new(
                        crate::incoming::Kind::Out,
                        files,
                        bytes,
                    ));
                }
                Answer::Done
            }
            Call::Collected { taken, refused } => {
                if let Some(mut batch) = self.outgoing.take() {
                    batch.end();
                }
                crate::incoming::collected(&taken, &refused);
                Answer::Done
            }
            Call::Leaving => {
                crate::incoming::leaving();
                Answer::Done
            }
        }))
    }

    /// The folder `name` of `guest`'s home, made if it is missing.
    fn folder(&self, name: &str) -> Result<OwnedFd> {
        let home = rustix::fs::open(
            format!("/home/{GUEST}"),
            OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        self.subfolder(&home, name)
    }

    /// The folder `name` inside `dir`, made for `guest` if it is missing.
    fn subfolder(&self, dir: &OwnedFd, name: &str) -> Result<OwnedFd> {
        match rustix::fs::mkdirat(dir, name, Mode::from_raw_mode(0o755)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        let folder = rustix::fs::openat2(
            dir,
            name,
            OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            confined(),
        )?;
        let stat = rustix::fs::fstat(&folder)?;
        if stat.st_uid == 0 {
            rustix::fs::fchown(
                &folder,
                Some(Uid::from_raw(self.guest.uid)),
                Some(Gid::from_raw(self.guest.gid)),
            )?;
        } else if stat.st_uid != self.guest.uid {
            bail!("{name} does not belong to {GUEST}");
        }
        Ok(folder)
    }

    fn start_upload(&mut self, id: u64, path: &str, size: u64, executable: bool) -> Result<()> {
        if size > agent::MAX_SENT_FILE {
            bail!("{path} is larger than {} bytes", agent::MAX_SENT_FILE);
        }
        if let Some(batch) = &mut self.incoming {
            batch.file(path);
        }
        let parts = components(path)?;
        let (name, parents) = parts.split_last().context("empty path")?;
        let mut dir = self.folder("Inbox")?;
        for part in parents {
            dir = self.subfolder(&dir, part)?;
        }
        let (file, name) = (0..100)
            .find_map(|n| {
                let candidate = numbered(name, n);
                match rustix::fs::openat2(
                    &dir,
                    &candidate,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(if executable { 0o755 } else { 0o644 }),
                    confined(),
                ) {
                    Ok(fd) => Some(Ok((fd, candidate))),
                    Err(rustix::io::Errno::EXIST) => None,
                    Err(e) => Some(Err(e)),
                }
            })
            .context("too many files of that name")??;
        rustix::fs::fchown(
            &file,
            Some(Uid::from_raw(self.guest.uid)),
            Some(Gid::from_raw(self.guest.gid)),
        )?;
        self.upload = Some(Upload {
            id,
            file: File::from(file),
            dir,
            name,
            remaining: size,
            hash: Sha256::new(),
        });
        Ok(())
    }

    fn chunk(&mut self, data: &str) -> Result<()> {
        let upload = self.upload.as_mut().context("no file is arriving")?;
        let bytes = BASE64.decode(data).context("a chunk is not base64")?;
        let len = bytes.len() as u64;
        if len > upload.remaining {
            self.abandon_upload();
            bail!("the file is longer than announced");
        }
        upload.remaining -= len;
        upload.hash.update(&bytes);
        if let Err(e) = upload.file.write_all(&bytes) {
            self.abandon_upload();
            return Err(e.into());
        }
        if let Some(batch) = &mut self.incoming {
            batch.bytes(len);
        }
        Ok(())
    }

    fn finish_upload(&mut self, sha256: &str) -> Result<()> {
        let upload = self.upload.take().context("no file is arriving")?;
        let hash = hex(&upload.hash.clone().finalize());
        if upload.remaining != 0 || hash != sha256 {
            rustix::fs::unlinkat(
                &upload.dir,
                upload.name.as_str(),
                rustix::fs::AtFlags::empty(),
            )
            .ok();
            bail!("the file arrived damaged");
        }
        upload.file.sync_all()?;
        if self
            .incoming
            .as_mut()
            .is_some_and(crate::incoming::Batch::arrived)
        {
            self.incoming = None;
        }
        Ok(())
    }

    /// Drops a file that stopped arriving halfway.
    fn abandon_upload(&mut self) {
        if let Some(upload) = self.upload.take() {
            rustix::fs::unlinkat(
                &upload.dir,
                upload.name.as_str(),
                rustix::fs::AtFlags::empty(),
            )
            .ok();
        }
    }

    fn get(&mut self, id: u64, path: &str) -> Result<()> {
        components(path)?;
        let outbox = self.folder("Outbox")?;
        let fd = rustix::fs::openat2(
            &outbox,
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            confined(),
        )
        .with_context(|| format!("opening {path}"))?;
        let stat = rustix::fs::fstat(&fd)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            bail!("{path} is not a regular file");
        }
        if stat.st_uid != self.guest.uid || stat.st_nlink != 1 {
            bail!("{path} is not a file of {GUEST}'s own");
        }
        let size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
        if size > agent::MAX_TAKEN_FILE {
            bail!("{path} is larger than {} bytes", agent::MAX_TAKEN_FILE);
        }
        let mut file = File::from(fd);
        let mut hash = Sha256::new();
        let mut buffer = vec![0; agent::CHUNK];
        let mut sent = 0_u64;
        if let Some(batch) = &mut self.outgoing {
            batch.file(path);
        }
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            sent += read as u64;
            if let Some(batch) = &mut self.outgoing {
                batch.bytes(read as u64);
            }
            if sent > agent::MAX_TAKEN_FILE {
                bail!("{path} grew past {} bytes", agent::MAX_TAKEN_FILE);
            }
            hash.update(&buffer[..read]);
            self.answer(
                id,
                Answer::Chunk {
                    data: BASE64.encode(&buffer[..read]),
                },
            )?;
        }
        if self
            .outgoing
            .as_mut()
            .is_some_and(crate::incoming::Batch::arrived)
        {
            self.outgoing = None;
        }
        self.answer(
            id,
            Answer::GetEnd {
                size: sent,
                sha256: hex(&hash.finalize()),
            },
        )
    }
}

/// Paths below the Inbox and the Outbox stay there and follow no link.
fn confined() -> ResolveFlags {
    ResolveFlags::BENEATH
        | ResolveFlags::NO_SYMLINKS
        | ResolveFlags::NO_MAGICLINKS
        | ResolveFlags::NO_XDEV
}

/// The parts of a relative path, refused when it could leave its folder.
fn components(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = path.split('/').collect();
    if path.len() > 4096
        || parts.len() > MAX_DEPTH
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == ".." || p.len() > 255 || p.contains('\0'))
    {
        bail!("'{path}' is not a plain relative path");
    }
    Ok(parts)
}

/// `name`, or with ` (n)` before its extension for the n-th other file.
fn numbered(name: &str, n: usize) -> String {
    if n == 0 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The regular files below `root`, by their path below it. Links and
/// anything else are left out; this is a listing only, and reading checks
/// each file again.
fn list(root: &str) -> Vec<GuestFile> {
    let mut found = Vec::new();
    let mut pending = vec![(Path::new(root).to_path_buf(), String::new(), 0)];
    while let Some((dir, prefix, depth)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if found.len() >= agent::MAX_FILES {
                return found;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() && depth + 1 < MAX_DEPTH {
                pending.push((entry.path(), path, depth + 1));
            } else if meta.is_file() {
                found.push(GuestFile {
                    path,
                    size: meta.len(),
                });
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// A field of `/proc/<pid>/status`.
fn status_field<'a>(status: &'a str, field: &str) -> Option<&'a str> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))
        .map(str::trim)
}

/// The unit of `guest`'s systemd a process runs in.
fn unit_of(pid: &str) -> Option<String> {
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    path.rsplit('/').next().map(str::to_owned)
}

/// Whether `unit` runs a program a user started: a menu entry's unit, or
/// the scope Flatpak moves an app into once it runs.
fn is_app_unit(unit: &str) -> bool {
    let launched = unit
        .strip_prefix("guest-app-")
        .and_then(|rest| rest.strip_suffix(".service"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let flatpak = unit
        .strip_prefix("app-flatpak-")
        .and_then(|rest| rest.strip_suffix(".scope"))
        .is_some_and(|rest| {
            !rest.is_empty()
                // systemd writes a `-` inside the app's ID as `\x2d`.
                && rest.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_' || b == b'\\'
                })
        });
    launched || flatpak
}

fn is_infrastructure(unit: &str) -> bool {
    INFRASTRUCTURE
        .iter()
        .any(|name| unit == *name || (name.ends_with(['-', ':']) && unit.starts_with(name)))
}

/// The title a launched program's unit carries; a Flatpak scope is titled
/// by its app's ID.
fn title_of(guest: Account, unit: &str) -> String {
    if let Some(rest) = unit
        .strip_prefix("app-flatpak-")
        .and_then(|r| r.strip_suffix(".scope"))
    {
        // `app-flatpak-<id>-<number>.scope`.
        return rest
            .rsplit_once('-')
            .map_or(rest, |(id, _)| id)
            .replace("\\x2d", "-");
    }
    fs::read_to_string(format!("/run/user/{}/systemd/transient/{unit}", guest.uid))
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("Description="))
                .map(|d| d.trim_start_matches("[systemd-run] ").to_owned())
        })
        .unwrap_or_else(|| unit.to_owned())
}

fn process(pid: &str, status: &str) -> GuestProcess {
    let command: String = fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .replace('\0', " ")
                .trim()
                .chars()
                .take(MAX_COMMAND)
                .collect()
        })
        .unwrap_or_default();
    let ticks = fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let fields: Vec<u64> = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .skip(11)
                .take(2)
                .filter_map(|f| f.parse().ok())
                .collect();
            Some(fields.iter().sum::<u64>())
        })
        .unwrap_or(0);
    GuestProcess {
        pid: pid.parse().unwrap_or(0),
        name: status_field(status, "Name").unwrap_or_default().to_owned(),
        command,
        memory_kib: status_field(status, "VmRSS")
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0),
        // The kernel counts 100 ticks a second on every architecture gse
        // runs on.
        cpu_seconds: ticks / 100,
    }
}

/// Where a process of `guest` belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Belongs {
    /// A program a user started, by its unit (see [`is_app_unit`]).
    Program(String),
    /// Outside every program.
    Other,
    /// The desktop's own services, `guest`'s systemd among them, which a
    /// listing leaves out.
    Desktop,
}

/// A process of `guest` as a listing found it.
#[derive(Debug, Clone)]
struct Found {
    parent: u32,
    belongs: Belongs,
}

/// The unit of the program that started the program `unit`, whose
/// processes are `pids`: the nearest program up the line of parents of its
/// first processes, past processes outside every program. The line ends at
/// the desktop's services, `guest`'s systemd among them, which starts every
/// menu entry, and at a process that is not `guest`'s.
fn starter(found: &HashMap<u32, Found>, unit: &str, pids: &[u32]) -> Option<String> {
    let own = |pid: u32| {
        found
            .get(&pid)
            .is_some_and(|f| matches!(&f.belongs, Belongs::Program(u) if u == unit))
    };
    pids.iter()
        .filter_map(|pid| found.get(pid))
        // The first processes: those no other process of the program started.
        .filter(|process| !own(process.parent))
        .find_map(|process| {
            let mut at = process.parent;
            for _ in 0..MAX_ANCESTRY {
                let parent = found.get(&at)?;
                match &parent.belongs {
                    Belongs::Program(other) if other != unit => return Some(other.clone()),
                    Belongs::Desktop => return None,
                    Belongs::Program(_) | Belongs::Other => at = parent.parent,
                }
            }
            None
        })
}

/// The processes of `guest`, by the program each belongs to, with the
/// program that started each.
fn processes(guest: Account) -> Vec<GuestApp> {
    let mut apps: BTreeMap<Option<String>, Vec<GuestProcess>> = BTreeMap::new();
    let mut found = HashMap::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let pid = entry.file_name().to_string_lossy().into_owned();
        let Ok(number) = pid.parse::<u32>() else {
            continue;
        };
        let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
            continue;
        };
        let uid = status_field(&status, "Uid")
            .and_then(|u| u.split_whitespace().next()?.parse::<u32>().ok());
        if uid != Some(guest.uid) {
            continue;
        }
        let belongs = match unit_of(&pid) {
            Some(unit) if is_infrastructure(&unit) => Belongs::Desktop,
            Some(unit) if is_app_unit(&unit) => Belongs::Program(unit),
            _ => Belongs::Other,
        };
        let parent = status_field(&status, "PPid")
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        found.insert(
            number,
            Found {
                parent,
                belongs: belongs.clone(),
            },
        );
        let key = match belongs {
            Belongs::Desktop => continue,
            Belongs::Program(unit) => Some(unit),
            Belongs::Other => None,
        };
        apps.entry(key).or_default().push(process(&pid, &status));
    }
    apps.into_iter()
        .map(|(unit, mut processes)| {
            processes.sort_by_key(|p| p.pid);
            let pids: Vec<u32> = processes.iter().map(|p| p.pid).collect();
            GuestApp {
                title: unit
                    .as_deref()
                    .map_or_else(|| "Other programs".to_owned(), |u| title_of(guest, u)),
                started_by: unit
                    .as_deref()
                    .and_then(|u| starter(&found, u, &pids))
                    .map(|u| title_of(guest, &u)),
                unit,
                processes,
            }
        })
        .collect()
}

fn end_process(guest: Account, pid: u32, force: bool) -> Result<()> {
    let raw = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .context("no such process")?;
    // The pidfd names this process for good; the owner is checked after it
    // is open, so a pid used again in between cannot be the one signalled.
    let fd = rustix::process::pidfd_open(raw, PidfdFlags::empty()).context("no such process")?;
    let status = fs::read_to_string(format!("/proc/{pid}/status")).context("no such process")?;
    let uid =
        status_field(&status, "Uid").and_then(|u| u.split_whitespace().next()?.parse::<u32>().ok());
    if uid != Some(guest.uid) {
        bail!("process {pid} is not {GUEST}'s");
    }
    rustix::process::pidfd_send_signal(&fd, if force { Signal::KILL } else { Signal::TERM })?;
    Ok(())
}

/// Kills a launched program with everything it started, through its
/// cgroup.
fn end_app(guest: Account, unit: &str) -> Result<()> {
    if !is_app_unit(unit) {
        bail!("'{unit}' is not a launched program");
    }
    let path = format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{unit}/cgroup.kill",
        uid = guest.uid
    );
    fs::write(&path, "1").with_context(|| format!("{unit} is not running"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_below_their_folder() {
        assert_eq!(
            components("saves/slot1.sav").unwrap(),
            ["saves", "slot1.sav"]
        );
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "a/./b", "a/"] {
            assert!(components(bad).is_err(), "{bad}");
        }
        assert!(components("a/".repeat(MAX_DEPTH).trim_end_matches('/')).is_ok());
        assert!(components("a/".repeat(MAX_DEPTH + 1).trim_end_matches('/')).is_err());
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        assert_eq!(numbered("save.dat", 0), "save.dat");
        assert_eq!(numbered("save.dat", 2), "save (2).dat");
        assert_eq!(numbered("README", 1), "README (1)");
        assert_eq!(numbered(".hidden", 1), ".hidden (1)");
    }

    #[test]
    fn desktop_services_are_infrastructure() {
        assert!(is_infrastructure("gvfs-daemon.service"));
        assert!(is_infrastructure(
            "dbus-:1.6-org.a11y.atspi.Registry@0.service"
        ));
        assert!(is_infrastructure("at-spi-dbus-bus.service"));
        assert!(!is_infrastructure("guest-app-1.service"));
        assert!(!is_infrastructure("dbus-broker.service.d"));
    }

    #[test]
    fn a_program_names_the_program_that_started_it() {
        const STEAM: &str = "guest-app-1.service";
        const GAME: &str = "app-flatpak-com.example.Game-7.scope";
        const HEROIC: &str = "app-flatpak-com.heroicgameslauncher.hgl-8.scope";
        const OTHER: &str = "app-flatpak-com.example.Other-9.scope";
        let program = |unit: &str| Belongs::Program(unit.to_owned());
        let found: HashMap<u32, Found> = [
            // `guest`'s systemd, started by root's.
            (100, 1, Belongs::Desktop),
            (200, 100, program(STEAM)),
            (210, 200, program(STEAM)),
            // A game Steam's helper started into a unit of its own.
            (300, 210, program(GAME)),
            (310, 300, program(GAME)),
            // Heroic from the menu: systemd started it.
            (400, 100, program(HEROIC)),
            // Started through a shell Steam ran outside every program.
            (500, 200, Belongs::Other),
            (600, 500, program(OTHER)),
            // Parents that loop, and one that is not `guest`'s.
            (700, 701, Belongs::Other),
            (701, 700, Belongs::Other),
            (800, 700, program("guest-app-2.service")),
            (900, 42, program("guest-app-3.service")),
        ]
        .into_iter()
        .map(|(pid, parent, belongs)| (pid, Found { parent, belongs }))
        .collect();
        assert_eq!(starter(&found, GAME, &[300, 310]).as_deref(), Some(STEAM));
        assert_eq!(starter(&found, STEAM, &[200, 210]), None);
        assert_eq!(starter(&found, HEROIC, &[400]), None);
        assert_eq!(starter(&found, OTHER, &[600]).as_deref(), Some(STEAM));
        assert_eq!(starter(&found, "guest-app-2.service", &[800]), None);
        assert_eq!(starter(&found, "guest-app-3.service", &[900]), None);
    }

    #[test]
    fn only_launched_programs_have_app_units() {
        assert!(is_app_unit("guest-app-1793542552787721096.service"));
        assert!(is_app_unit(
            "app-flatpak-com.heroicgameslauncher.hgl-3620867229.scope"
        ));
        assert!(!is_app_unit("app-flatpak-../x.scope"));
        let faugus = r"app-flatpak-io.github.Faugus.faugus\x2dlauncher-359534095.scope";
        assert!(is_app_unit(faugus));
        assert_eq!(
            title_of(Account { uid: 0, gid: 0 }, faugus),
            "io.github.Faugus.faugus-launcher"
        );
        assert!(!is_app_unit("guest-app-.service"));
        assert!(!is_app_unit("gse-launch.service"));
        assert!(!is_app_unit("guest-app-1/../x.service"));
    }
}
