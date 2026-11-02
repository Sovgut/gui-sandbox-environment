//! Files on their way in or out of an environment that is not there to
//! take them: on a single-GPU machine the app is gone while it runs, and
//! nothing can be sent in or taken out then.
//!
//! - Sent in while it is stopped, files wait in its folder (`inbox-waiting/`)
//!   and go into its Inbox once it is up again.
//! - While a single-GPU environment runs, the service takes what appears in
//!   its Outbox every [`COLLECT_EVERY`], past the same checks as a take, into
//!   a spool (`outbox-taken`): the lines a take sends the client, kept until
//!   the app takes them after the run.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gse_protocol::GuestFile;

use crate::agent::Agent;

/// How often a single-GPU environment's Outbox is looked at.
pub const COLLECT_EVERY: Duration = Duration::from_secs(20);

fn waiting_dir(dir: &Path) -> PathBuf {
    dir.join("inbox-waiting")
}

fn spool(dir: &Path) -> PathBuf {
    dir.join("outbox-taken")
}

/// What the spool holds, one `size path` line per file that passed.
fn spool_index(dir: &Path) -> PathBuf {
    dir.join("outbox-taken.index")
}

/// Whether `path` is a plain relative path, as Inbox paths are.
fn plain(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains('\0'))
}

/// Keeps `files` (Inbox path, open file, size) for the next start of the
/// environment in `dir`; returns how many. `copied` hears of each part
/// copied. When `stop` turns true the copy ends at the next part, the file
/// under way is removed, and the error is [`Stopped`] with the files kept
/// whole.
pub fn queue(
    dir: &Path,
    files: Vec<(String, File, u64)>,
    copied: &mut dyn FnMut(u64),
    stop: &AtomicBool,
) -> Result<usize> {
    let waiting = waiting_dir(dir);
    let count = files.len();
    for (done, (path, mut from, _)) in files.into_iter().enumerate() {
        if !plain(&path) {
            bail!("{path} is no plain path");
        }
        let to = waiting.join(&path);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        let executable = from.metadata()?.mode() & 0o100 != 0
            || path.to_ascii_lowercase().ends_with(".appimage");
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(if executable { 0o700 } else { 0o600 })
            .open(&to)
            .with_context(|| format!("{}", to.display()))?;
        let mut reader = Counted::new(&mut from, &mut *copied).stopping(stop);
        if let Err(e) = paced_copy(&mut reader, &mut out) {
            drop(out);
            fs::remove_file(&to).ok();
            if is_stopped(&e) {
                return Err(Stopped { copied: done }.into());
            }
            return Err(e).with_context(|| format!("copying {path}"));
        }
    }
    fs::set_permissions(&waiting, fs::Permissions::from_mode(0o700)).ok();
    Ok(count)
}

/// A send that [`Request::CancelTransfer`](gse_protocol::Request) stopped,
/// with how many of its files went in whole.
#[derive(Debug)]
pub struct Stopped {
    pub copied: usize,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped after {} file(s)", self.copied)
    }
}

impl std::error::Error for Stopped {}

/// What a stopping [`Counted`] reads fail with.
#[derive(Debug)]
struct StopRead;

impl std::fmt::Display for StopRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped")
    }
}

impl std::error::Error for StopRead {}

/// Whether `error` is a read that a stop ended.
pub fn is_stopped(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.downcast_ref::<StopRead>().is_some())
}

/// How much a copy writes before it waits for that part to be on disk.
const PACE: u64 = 32 << 20;

/// Copies `from` into `to` without piling up unwritten data: every
/// [`PACE`] it waits until what it wrote is on disk. A bulk copy into the
/// page cache otherwise leaves gigabytes for the kernel to flush at once,
/// and on btrfs every program that touches the disk waits for that flush.
fn paced_copy(from: &mut impl io::Read, to: &mut File) -> io::Result<u64> {
    let mut buffer = vec![0; 1 << 20];
    let mut written = 0_u64;
    let mut paced = 0_u64;
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buffer[..read])?;
        written += read as u64;
        if written - paced >= PACE {
            to.sync_data()?;
            paced = written;
        }
    }
    Ok(written)
}

/// Lowers the calling thread's I/O and CPU priority, so a bulk copy waits
/// for everything else on the machine: the best-effort I/O class at its
/// lowest level (through `ionice`, since `ioprio_set` has no safe binding),
/// and nice 10. The thread of a connection ends with it. Failing to only
/// costs the courtesy.
pub fn yield_to_others() {
    let thread = rustix::thread::gettid();
    rustix::process::setpriority_process(Some(thread), 10).ok();
    std::process::Command::new("ionice")
        .args([
            "-c",
            "2",
            "-n",
            "7",
            "-p",
            &thread.as_raw_nonzero().to_string(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok();
}

/// A reader that tells `copied` how many bytes each read gave, and fails
/// with a stop once its `stop` flag is up.
pub struct Counted<'a, R> {
    inner: R,
    copied: &'a mut dyn FnMut(u64),
    stop: Option<&'a AtomicBool>,
}

impl<'a, R> Counted<'a, R> {
    pub fn new(inner: R, copied: &'a mut dyn FnMut(u64)) -> Self {
        Self {
            inner,
            copied,
            stop: None,
        }
    }

    /// Fails the next read once `stop` is up.
    pub fn stopping(mut self, stop: &'a AtomicBool) -> Self {
        self.stop = Some(stop);
        self
    }
}

impl<R: io::Read> io::Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
            return Err(io::Error::other(StopRead));
        }
        let read = self.inner.read(buffer)?;
        (self.copied)(read as u64);
        Ok(read)
    }
}

/// Every regular file below `root`, as a path relative to it.
fn files_below(root: &Path) -> Vec<(String, PathBuf, u64)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file()
                && let Some(relative) = path.strip_prefix(root).ok().and_then(Path::to_str)
            {
                found.push((relative.to_owned(), path.clone(), metadata.len()));
            }
        }
    }
    found.sort();
    found
}

/// The files waiting to go into the Inbox.
pub fn waiting(dir: &Path) -> Vec<GuestFile> {
    files_below(&waiting_dir(dir))
        .into_iter()
        .map(|(path, _, size)| GuestFile { path, size })
        .collect()
}

/// Puts the waiting files into the Inbox through `agent`; returns how many.
pub fn deliver(dir: &Path, agent: &mut Agent) -> Result<usize> {
    let files = files_below(&waiting_dir(dir));
    if !files.is_empty() {
        agent.announce(files.len(), files.iter().map(|(_, _, size)| size).sum())?;
    }
    for (path, full, size) in &files {
        let mut file = File::open(full)?;
        let executable = file.metadata()?.mode() & 0o100 != 0;
        agent.put(path, &mut file, *size, executable)?;
        fs::remove_file(full).ok();
    }
    fs::remove_dir_all(waiting_dir(dir)).ok();
    Ok(files.len())
}

/// The files taken out while the environment ran, which the app has not
/// taken yet.
pub fn taken(dir: &Path) -> Vec<GuestFile> {
    fs::read_to_string(spool_index(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (size, path) = line.split_once(' ')?;
            Some(GuestFile {
                path: path.to_owned(),
                size: size.parse().ok()?,
            })
        })
        .collect()
}

/// Takes what is new in the Outbox through `agent` into the spool; `seen`
/// holds what earlier rounds took, by path and size, so a file taken once
/// is taken again only when it changed. Returns how many passed.
pub fn collect(dir: &Path, agent: &mut Agent, seen: &mut HashSet<(String, u64)>) -> Result<usize> {
    let outbox = agent.files()?.outbox;
    let new: Vec<GuestFile> = outbox
        .into_iter()
        .filter(|f| !seen.contains(&(f.path.clone(), f.size)))
        .collect();
    if new.is_empty() {
        return Ok(0);
    }
    let mut spool = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(spool(dir))?;
    let (passed, _) = crate::daemon::take_files(agent, &mut spool, &new)?;
    let mut index = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(spool_index(dir))?;
    for file in &passed {
        writeln!(index, "{} {}", file.size, file.path)?;
    }
    for file in new {
        seen.insert((file.path, file.size));
    }
    Ok(passed.len())
}

/// Sends the spool to the client on `out` as a take would, and empties it;
/// returns how many files it held.
pub fn hand_over(dir: &Path, out: &mut impl io::Write) -> Result<usize> {
    let count = taken(dir).len();
    match File::open(spool(dir)) {
        Ok(mut spooled) => {
            io::copy(&mut spooled, out)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    }
    fs::remove_file(spool(dir)).ok();
    fs::remove_file(spool_index(dir)).ok();
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_files_wait_under_their_inbox_paths() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("save.dat");
        fs::write(&source, "progress").unwrap();
        let files = vec![("Saves/save.dat".to_owned(), File::open(&source).unwrap(), 8)];
        let mut copied = 0;
        assert_eq!(
            queue(
                dir.path(),
                files,
                &mut |n| copied += n,
                &AtomicBool::new(false)
            )
            .unwrap(),
            1
        );
        assert_eq!(copied, 8);
        assert_eq!(
            waiting(dir.path()),
            [GuestFile {
                path: "Saves/save.dat".into(),
                size: 8,
            }]
        );
        let escaping = vec![("../x".to_owned(), File::open(&source).unwrap(), 8)];
        assert!(queue(dir.path(), escaping, &mut |_| {}, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn a_stopped_queue_keeps_whole_files_and_drops_the_one_under_way() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("big.bin");
        fs::write(&source, vec![7_u8; 3 << 20]).unwrap();
        let open = || File::open(&source).unwrap();
        let files = vec![
            ("one.bin".to_owned(), open(), 3 << 20),
            ("two.bin".to_owned(), open(), 3 << 20),
        ];
        let stop = AtomicBool::new(false);
        let mut seen = 0;
        let error = queue(
            dir.path(),
            files,
            &mut |n| {
                seen += n;
                if seen > 4 << 20 {
                    stop.store(true, Ordering::SeqCst);
                }
            },
            &stop,
        )
        .unwrap_err();
        assert_eq!(error.downcast_ref::<Stopped>().unwrap().copied, 1);
        let waiting = waiting(dir.path());
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].path, "one.bin");
        assert_eq!(waiting[0].size, 3 << 20);
    }

    #[test]
    fn a_paced_copy_copies_everything() {
        let dir = tempfile::tempdir().unwrap();
        let size = usize::try_from(PACE * 2).unwrap() + 12345;
        let data: Vec<u8> = (0..size).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let to = dir.path().join("copy");
        let mut out = File::create(&to).unwrap();
        assert_eq!(
            paced_copy(&mut data.as_slice(), &mut out).unwrap(),
            data.len() as u64
        );
        assert_eq!(fs::read(&to).unwrap(), data);
    }

    #[test]
    fn the_spool_is_handed_over_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(spool(dir.path()), "{\"reply\":\"file-start\"}\n").unwrap();
        fs::write(spool_index(dir.path()), "8 Screenshots/a.png\n").unwrap();
        assert_eq!(
            taken(dir.path()),
            [GuestFile {
                path: "Screenshots/a.png".into(),
                size: 8,
            }]
        );
        let mut out = Vec::new();
        assert_eq!(hand_over(dir.path(), &mut out).unwrap(), 1);
        assert_eq!(out, b"{\"reply\":\"file-start\"}\n");
        assert_eq!(hand_over(dir.path(), &mut out).unwrap(), 0);
        assert_eq!(taken(dir.path()), Vec::new());
    }
}
