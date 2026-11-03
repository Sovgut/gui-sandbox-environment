//! Files on their way through the Inbox and the Outbox, shown inside the
//! guest. With one GPU the host's app is gone while the environment runs, so
//! this is the only word of them. The agent (root) writes what it does to
//! files in /run/gse; the desktop of `sandbox` shows them:
//!
//! - files the host kept for this start arriving in the Inbox, and files
//!   the host takes out of the Outbox while the environment runs: a window
//!   with a bar while they move ([`Kind`]);
//! - after each round of taking the Outbox, a notification of what left
//!   and what stays ([`COLLECTED`]);
//! - when the user asked to leave, a window saying the screen goes dark
//!   ([`LEAVING`]).
//!
//! A power-off from the guest's own menu waits until the host took every
//! file of the Outbox, at most [`OUTBOX_WAIT`] ([`wait_for_outbox`]).

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Which way a batch of files moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Into the Inbox: what the host kept for this start.
    In,
    /// Out of the Outbox, for the host after the run.
    Out,
}

impl Kind {
    /// Where the agent says how far the files got; only root writes it.
    fn status(self) -> &'static str {
        match self {
            Self::In => "/run/gse/incoming",
            Self::Out => "/run/gse/outgoing",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::In => "Moving files into the Inbox",
            Self::Out => "Taking files out for the host",
        }
    }
}

/// What the host took of the Outbox or left in it while the environment
/// runs, one `size path` line each; it exists once the host takes the
/// Outbox by itself.
const HANDLED: &str = "/run/gse/outbox-handled";
/// The notification of the last round of taking the Outbox: a round
/// number, then its title and its text, a line each.
pub const COLLECTED: &str = "/run/gse/collected";
/// Present once the user asked to leave.
pub const LEAVING: &str = "/run/gse/leaving";
/// How long a power-off waits for the host to take the Outbox.
const OUTBOX_WAIT: Duration = Duration::from_secs(60);

/// How often the agent rewrites the status, and the desktop reads it.
const EVERY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub files: u32,
    pub arrived: u32,
    pub done: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub finished: bool,
    /// The file arriving now.
    pub current: String,
}

impl Status {
    fn to_text(&self) -> String {
        format!(
            "{} {} {} {} {} {}\n{}\n",
            self.files,
            self.arrived,
            self.done,
            self.bytes,
            self.elapsed_ms,
            u8::from(self.finished),
            self.current.replace('\n', " ")
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let numbers: Vec<u64> = lines
            .next()?
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let [files, arrived, done, bytes, elapsed_ms, finished] = numbers[..] else {
            return None;
        };
        Some(Self {
            files: u32::try_from(files).ok()?,
            arrived: u32::try_from(arrived).ok()?,
            done,
            bytes,
            elapsed_ms,
            finished: finished == 1,
            current: lines.next().unwrap_or_default().to_owned(),
        })
    }

    /// `412 MB of 1.2 GB · about 2 minutes left`, the time once there is a
    /// pace to tell it by.
    fn progress_text(&self) -> String {
        let mut text = format!("{} of {}", size_text(self.done), size_text(self.bytes));
        if self.done > 0 && self.elapsed_ms >= 3000 && self.done < self.bytes {
            #[allow(clippy::cast_precision_loss)] // a pace
            let left = (self.bytes - self.done) as f64 * (self.elapsed_ms as f64 / 1000.)
                / self.done as f64;
            text.push_str(" \u{b7} ");
            text.push_str(&time_left(left));
        }
        text
    }

    fn percent(&self) -> u64 {
        (self.done.saturating_mul(100) / self.bytes.max(1)).min(99)
    }
}

/// Sizes in decimal units, as the host's app writes them.
fn size_text(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let bytes = bytes as f64;
    let (value, unit) = [(1e9, "GB"), (1e6, "MB"), (1e3, "KB")]
        .into_iter()
        .find(|(size, _)| bytes >= *size)
        .map_or((bytes, "B"), |(size, unit)| (bytes / size, unit));
    if value < 10. && unit != "B" {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

fn time_left(seconds: f64) -> String {
    let minutes = (seconds / 60.).round();
    if minutes < 1. {
        "less than a minute left".to_owned()
    } else if minutes < 2. {
        "about a minute left".to_owned()
    } else if minutes < 90. {
        format!("about {minutes:.0} minutes left")
    } else {
        format!("about {:.0} hours left", (minutes / 60.).round())
    }
}

/// A batch of files arriving, as the agent follows it.
pub struct Batch {
    kind: Kind,
    status: Status,
    since: Instant,
    written: Option<Instant>,
}

impl Batch {
    pub fn new(kind: Kind, files: u32, bytes: u64) -> Self {
        let mut batch = Self {
            kind,
            status: Status {
                files,
                arrived: 0,
                done: 0,
                bytes,
                elapsed_ms: 0,
                finished: false,
                current: String::new(),
            },
            since: Instant::now(),
            written: None,
        };
        batch.write(true);
        batch
    }

    /// The file `name` begins to arrive.
    pub fn file(&mut self, name: &str) {
        name.clone_into(&mut self.status.current);
        self.write(true);
    }

    pub fn bytes(&mut self, count: u64) {
        self.status.done += count;
        self.write(false);
    }

    /// The batch ends short of its files: the host moved on without the
    /// rest.
    pub fn end(&mut self) {
        self.status.finished = true;
        self.write(true);
    }

    /// A file arrived whole; returns whether it was the last.
    pub fn arrived(&mut self) -> bool {
        self.status.arrived += 1;
        self.status.finished = self.status.arrived >= self.status.files;
        self.write(true);
        self.status.finished
    }

    /// Writes the status, at most every [`EVERY`] unless `now`. A status
    /// that cannot be written only costs the window.
    fn write(&mut self, now: bool) {
        if !now && self.written.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        self.written = Some(Instant::now());
        self.status.elapsed_ms =
            u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX);
        write_atomically(Path::new(self.kind.status()), &self.status.to_text());
    }
}

/// Puts `text` at `path` in one step, its folder made if missing. A file
/// that cannot be written only costs what the desktop shows.
fn write_atomically(path: &Path, text: &str) {
    let Some(dir) = path.parent() else { return };
    fs::create_dir_all(dir).ok();
    let partial = dir.join(format!(
        ".{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    if fs::write(&partial, text).is_ok() {
        fs::rename(&partial, path).ok();
    }
}

/// The host now takes the Outbox by itself while the environment runs.
pub fn collecting() {
    if !Path::new(HANDLED).exists() {
        write_atomically(Path::new(HANDLED), "");
    }
}

/// Notes a round of taking the Outbox: what left for the host and what
/// stays, with why; the desktop says so.
pub fn collected(taken: &[gse_protocol::GuestFile], refused: &[(String, String)]) {
    use std::io::Write as _;
    if let Ok(mut handled) = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(HANDLED)
    {
        for file in taken {
            writeln!(handled, "{} {}", file.size, file.path).ok();
        }
        for (path, _) in refused {
            let size = fs::metadata(format!("/home/{}/Outbox/{path}", crate::agent::GUEST))
                .map_or(0, |m| m.len());
            writeln!(handled, "{size} {path}").ok();
        }
    }
    let round = fs::read_to_string(COLLECTED)
        .ok()
        .and_then(|text| text.lines().next()?.parse::<u64>().ok())
        .unwrap_or(0)
        + 1;
    let files = |count: usize| {
        if count == 1 {
            "1 file".to_owned()
        } else {
            format!("{count} files")
        }
    };
    let stays: Vec<String> = refused
        .iter()
        .map(|(path, why)| format!("{path} stays: {why}"))
        .collect();
    let (title, mut body) = if taken.is_empty() {
        (
            format!("{} stay in the Outbox", files(refused.len())),
            String::new(),
        )
    } else {
        (
            format!("{} left for the host", files(taken.len())),
            "They reach your computer once you are back on its desktop and the antivirus \
             checked them."
                .to_owned(),
        )
    };
    for line in stays {
        if !body.is_empty() {
            body.push(' ');
        }
        body.push_str(&line);
        body.push('.');
    }
    write_atomically(
        Path::new(COLLECTED),
        &format!("{round}\n{title}\n{}\n", body.replace('\n', " ")),
    );
}

/// The user asked to leave; the desktop says the screen goes dark.
pub fn leaving() {
    write_atomically(Path::new(LEAVING), "");
}

/// Waits until the host took or turned down every file of the Outbox, at
/// most [`OUTBOX_WAIT`]; at once when the host does not take the Outbox by
/// itself. Runs as root when the guest powers off.
pub fn wait_for_outbox() {
    if !Path::new(HANDLED).exists() {
        return;
    }
    let started = Instant::now();
    while started.elapsed() < OUTBOX_WAIT {
        let handled = fs::read_to_string(HANDLED).unwrap_or_default();
        let waiting = outbox_files().into_iter().any(|(size, path)| {
            !handled
                .lines()
                .any(|line| line.split_once(' ') == Some((&size.to_string(), path.as_str())))
        });
        if !waiting {
            return;
        }
        println!("waiting for the host to take the Outbox");
        sleep(Duration::from_secs(1));
    }
}

/// Every regular file of the Outbox, with its size and its path in it.
fn outbox_files() -> Vec<(u64, String)> {
    let root = std::path::PathBuf::from(format!("/home/{}/Outbox", crate::agent::GUEST));
    let mut found = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else if meta.is_file()
                && let Ok(path) = entry.path().strip_prefix(&root)
            {
                found.push((meta.len(), path.to_string_lossy().into_owned()));
            }
        }
    }
    found
}

/// Has a power-off wait for the Outbox: a unit in /run, so it needs no
/// rebuilt image, that stops before the agent and waits in its stop. Runs
/// as root when the agent starts.
pub fn install_outbox_wait() {
    let unit = "[Unit]\n\
                Description=Wait for the host to take the Outbox\n\
                After=gse-agent.service\n\
                \n\
                [Service]\n\
                Type=oneshot\n\
                RemainAfterExit=yes\n\
                ExecStart=/bin/true\n\
                ExecStop=/usr/libexec/gse/gse-guest outbox-wait\n\
                TimeoutStopSec=90\n";
    let path = Path::new("/run/systemd/system/gse-outbox-wait.service");
    if fs::read_to_string(path).is_ok_and(|text| text == unit) {
        return;
    }
    write_atomically(path, unit);
    for args in [
        &["daemon-reload"][..],
        &["start", "gse-outbox-wait.service"],
    ] {
        Command::new("systemctl")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
    }
}

/// What the desktop shows of one direction of files.
struct Shown {
    kind: Kind,
    window: Option<Child>,
    dismissed: bool,
    told: bool,
}

impl Shown {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            window: None,
            dismissed: false,
            told: false,
        }
    }

    /// Follows the batch's status: a window while it moves, a notification
    /// when files arrived.
    fn follow(&mut self) {
        let Some(status) = fs::read_to_string(self.kind.status())
            .ok()
            .as_deref()
            .and_then(Status::parse)
        else {
            return;
        };
        if status.finished {
            if let Some(mut child) = self.window.take() {
                if let Some(input) = child.stdin.as_mut() {
                    input.write_all(b"100\n").ok();
                }
                drop(child.stdin.take());
                child.wait().ok();
            }
            if !self.told && self.kind == Kind::In {
                notify(
                    "folder",
                    &arrived_title(status.arrived, status.files),
                    arrived_body(status.arrived, status.files),
                );
            }
            self.told = true;
            return;
        }
        // A new batch after a finished one shows again.
        self.told = false;
        if self.window.is_none() && !self.dismissed {
            self.window = open_window(self.kind.title());
        }
        let line = format!(
            "{}\n#{} \u{b7} {}\n",
            status.percent(),
            status.progress_text(),
            status.current
        );
        let shown = self
            .window
            .as_mut()
            .and_then(|child| child.stdin.as_mut())
            .map(|input| input.write_all(line.as_bytes()));
        if shown.is_some_and(|written| written.is_err()) {
            // The window was closed; the notification still comes.
            self.window = None;
            self.dismissed = true;
        }
    }
}

/// Shows on the desktop the files moving in and out, the rounds of taking
/// the Outbox and the way out, for as long as the session lasts. Runs as
/// `sandbox` beside the desktop's other parts.
pub fn watch() {
    let mut shown = [Shown::new(Kind::In), Shown::new(Kind::Out)];
    let mut round = fs::read_to_string(COLLECTED)
        .ok()
        .and_then(|text| text.lines().next()?.parse::<u64>().ok())
        .unwrap_or(0);
    let mut leaving: Option<Child> = None;
    loop {
        sleep(EVERY);
        for direction in &mut shown {
            direction.follow();
        }
        if let Ok(text) = fs::read_to_string(COLLECTED) {
            let mut lines = text.lines();
            let number = lines
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
            if number > round {
                round = number;
                let title = lines.next().unwrap_or_default();
                let body = lines.next().unwrap_or_default();
                notify("folder-download", title, body);
            }
        }
        if leaving.is_none() && Path::new(LEAVING).exists() {
            leaving = Command::new("zenity")
                .args([
                    "--progress",
                    "--pulsate",
                    "--no-cancel",
                    "--title=Returning to your desktop",
                    "--text=The host takes the Outbox once more, then the screen goes dark for \
                     about 15 seconds while the graphics card goes back.",
                    "--width=460",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok();
        }
    }
}

/// The progress window: zenity reads a percentage per line and `#` lines
/// as its text.
fn open_window(title: &str) -> Option<Child> {
    Command::new("zenity")
        .args([
            "--progress",
            &format!("--title={title}"),
            "--text=Getting ready",
            "--percentage=0",
            "--auto-close",
            "--no-cancel",
            "--width=460",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn arrived_title(arrived: u32, files: u32) -> String {
    if arrived < files {
        format!("{arrived} of {files} files arrived in the Inbox")
    } else if files == 1 {
        "1 file arrived in the Inbox".to_owned()
    } else {
        format!("{files} files arrived in the Inbox")
    }
}

fn arrived_body(arrived: u32, files: u32) -> &'static str {
    if arrived < files {
        "The rest still wait on the host for the next start; the sandbox's Activity on the host \
         says why."
    } else if files == 1 {
        "Open the Inbox on the desktop to use it."
    } else {
        "Open the Inbox on the desktop to use them."
    }
}

/// A desktop notification of gse's that stays until dismissed.
fn notify(icon: &str, title: &str, body: &str) {
    Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.Notifications",
            "--object-path",
            "/org/freedesktop/Notifications",
            "--method",
            "org.freedesktop.Notifications.Notify",
            "GUI Sandbox Environment",
            "0",
            icon,
            title,
            body,
            "[]",
            "{}",
            "0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_reads_back_as_written() {
        let status = Status {
            files: 3,
            arrived: 1,
            done: 412_000_000,
            bytes: 1_200_000_000,
            elapsed_ms: 120_000,
            finished: false,
            current: "Mods/big pack.zip".into(),
        };
        assert_eq!(Status::parse(&status.to_text()), Some(status.clone()));
        assert_eq!(status.percent(), 34);
        assert_eq!(
            status.progress_text(),
            "412 MB of 1.2 GB \u{b7} about 4 minutes left"
        );
        assert_eq!(Status::parse("nonsense"), None);
    }
}
