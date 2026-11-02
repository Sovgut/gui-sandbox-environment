//! Files the host kept for this start, on their way into the Inbox. The
//! agent (root) writes how far they got to [`STATUS`]; the desktop of
//! `sandbox` shows that as a window with a bar while they arrive and a
//! notification once they are in. With one GPU the host's app is gone while
//! the environment runs, so this is the only word that they arrived.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Where the agent says how far the files got; only root writes it.
pub const STATUS: &str = "/run/gse/incoming";

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
    status: Status,
    since: Instant,
    written: Option<Instant>,
}

impl Batch {
    pub fn new(files: u32, bytes: u64) -> Self {
        let mut batch = Self {
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
        let path = Path::new(STATUS);
        let Some(dir) = path.parent() else { return };
        fs::create_dir_all(dir).ok();
        let partial = dir.join(".incoming");
        if fs::write(&partial, self.status.to_text()).is_ok() {
            fs::rename(&partial, path).ok();
        }
    }
}

/// Shows on the desktop the files arriving, for as long as the session
/// lasts: a window with a bar while they come, a notification once they
/// are in. Runs as `sandbox` beside the desktop's other parts.
pub fn watch() {
    let mut window: Option<Child> = None;
    let mut dismissed = false;
    let mut told = false;
    loop {
        sleep(EVERY);
        let Some(status) = fs::read_to_string(STATUS)
            .ok()
            .as_deref()
            .and_then(Status::parse)
        else {
            continue;
        };
        if status.finished {
            if let Some(mut child) = window.take() {
                if let Some(input) = child.stdin.as_mut() {
                    input.write_all(b"100\n").ok();
                }
                drop(child.stdin.take());
                child.wait().ok();
            }
            if !told {
                told = true;
                notify_arrived(status.files);
            }
            continue;
        }
        if window.is_none() && !dismissed {
            window = open_window();
        }
        let line = format!(
            "{}\n#{} \u{b7} {}\n",
            status.percent(),
            status.progress_text(),
            status.current
        );
        let shown = window
            .as_mut()
            .and_then(|child| child.stdin.as_mut())
            .map(|input| input.write_all(line.as_bytes()));
        if shown.is_some_and(|written| written.is_err()) {
            // The window was closed; the notification still comes.
            window = None;
            dismissed = true;
        }
    }
}

/// The progress window: zenity reads a percentage per line and `#` lines
/// as its text.
fn open_window() -> Option<Child> {
    Command::new("zenity")
        .args([
            "--progress",
            "--title=Moving files into the Inbox",
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

fn notify_arrived(files: u32) {
    let title = if files == 1 {
        "1 file arrived in the Inbox".to_owned()
    } else {
        format!("{files} files arrived in the Inbox")
    };
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
            "folder",
            &title,
            "Open the Inbox on the desktop to use them.",
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
