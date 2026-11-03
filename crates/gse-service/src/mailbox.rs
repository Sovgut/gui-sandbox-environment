//! Files on their way in or out of an environment that is not there to
//! take them: on a single-GPU machine the app is gone while it runs, and
//! nothing can be sent in or taken out then.
//!
//! - Sent in while it is stopped, files wait in its folder (`inbox-waiting/`)
//!   as carriers (`gse_protocol::carrier`), one for each send. The next start
//!   attaches up to [`carrier::MAX_DISKS`] of them as read-only disks, and
//!   once it is up the agent copies their files into the Inbox at the speed
//!   of the disks; the agent's port carries only the call and its progress.
//!   A carrier that did not fit waits for the start after.
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
use gse_protocol::carrier::{self, Entry, Index};

use tracing::warn;

use crate::agent::Agent;

/// How often a single-GPU environment's Outbox is looked at.
pub const COLLECT_EVERY: Duration = Duration::from_secs(5);

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
/// environment in `dir`, in a carrier of their own; returns how many.
/// `copied` hears of each part copied. When `stop` turns true the copy ends
/// at the next part, the file under way is left out, and the error is
/// [`Stopped`] with the files kept whole.
pub fn queue(
    dir: &Path,
    files: Vec<(String, File, u64)>,
    copied: &mut dyn FnMut(u64),
    stop: &AtomicBool,
) -> Result<usize> {
    if let Some((path, _, _)) = files.iter().find(|(path, _, _)| !plain(path)) {
        bail!("{path} is no plain path");
    }
    if let Some((path, _, _)) = files
        .iter()
        .find(|(_, _, size)| *size > gse_protocol::agent::MAX_SENT_FILE)
    {
        bail!("{path} is larger than a sandbox takes (64 GiB)");
    }
    let waiting = waiting_dir(dir);
    fs::create_dir_all(&waiting)?;
    fs::set_permissions(&waiting, fs::Permissions::from_mode(0o700)).ok();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut writer = CarrierWriter::create(&waiting.join(format!("{stamp}.{CARRIER}")))?;
    let count = files.len();
    for (done, (path, mut from, _)) in files.into_iter().enumerate() {
        let executable = from.metadata()?.mode() & 0o100 != 0
            || path.to_ascii_lowercase().ends_with(".appimage");
        let mut reader = Counted::new(&mut from, &mut *copied).stopping(stop);
        if let Err(e) = writer.add(&path, executable, &mut reader) {
            writer.finish()?;
            if is_stopped(&e) {
                return Err(Stopped { copied: done }.into());
            }
            return Err(e).with_context(|| format!("copying {path}"));
        }
    }
    writer.finish()?;
    Ok(count)
}

/// The extension of a carrier's file.
const CARRIER: &str = "carrier";

/// Writes a carrier: each file from the next aligned offset, then, at
/// [`CarrierWriter::finish`], the index and the trailer.
struct CarrierWriter {
    path: PathBuf,
    file: File,
    files: Vec<Entry>,
    end: u64,
}

impl CarrierWriter {
    fn create(path: &Path) -> Result<Self> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("{}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            file,
            files: Vec::new(),
            end: 0,
        })
    }

    /// Copies `from` in as the Inbox file `path`.
    fn add(&mut self, path: &str, executable: bool, from: &mut impl io::Read) -> io::Result<()> {
        use std::io::Seek as _;
        self.file.seek(io::SeekFrom::Start(self.end))?;
        let size = paced_copy(from, &mut self.file)?;
        self.files.push(Entry {
            path: path.to_owned(),
            size,
            executable,
            offset: self.end,
        });
        self.end = carrier::align(self.end + size);
        Ok(())
    }

    /// Ends the carrier with the files added whole; one without any goes.
    fn finish(mut self) -> Result<()> {
        use std::io::Seek as _;
        if self.files.is_empty() {
            drop(self.file);
            fs::remove_file(&self.path).ok();
            return Ok(());
        }
        let index = serde_json::to_vec(&Index { files: self.files })?;
        let len = index.len() as u64;
        self.file.seek(io::SeekFrom::Start(self.end))?;
        self.file.write_all(&index)?;
        let trailer = carrier::align(self.end + len);
        self.file.seek(io::SeekFrom::Start(trailer))?;
        self.file.write_all(&carrier::trailer(self.end, len))?;
        self.file.set_len(trailer + carrier::BLOCK)?;
        self.file.sync_all()?;
        Ok(())
    }
}

/// The carriers waiting in `dir`, oldest first.
pub fn carriers(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(waiting_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == CARRIER) && p.is_file())
        .collect();
    found.sort();
    found
}

/// What the carrier at `path` holds.
pub fn carrier_index(path: &Path) -> Result<Index> {
    use std::os::unix::fs::FileExt as _;
    let file = File::open(path).with_context(|| format!("{}", path.display()))?;
    let size = file.metadata()?.len();
    let mut block = vec![0; usize::try_from(carrier::BLOCK)?];
    file.read_exact_at(
        &mut block,
        size.checked_sub(carrier::BLOCK).context("no trailer")?,
    )?;
    let (offset, len) = carrier::parse_trailer(&block).context("no carrier trailer")?;
    if len > carrier::MAX_INDEX || offset.saturating_add(len) > size {
        bail!("the carrier's index lies outside it");
    }
    let mut index = vec![0; usize::try_from(len)?];
    file.read_exact_at(&mut index, offset)?;
    let index: Index = serde_json::from_slice(&index).context("the carrier's index")?;
    index.check(offset).map_err(anyhow::Error::msg)?;
    Ok(index)
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
pub(crate) fn paced_copy(from: &mut impl io::Read, to: &mut File) -> io::Result<u64> {
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

/// Files waiting as plain files below `inbox-waiting/`, which services
/// before carriers kept there.
fn loose_files(dir: &Path) -> Vec<(String, PathBuf, u64)> {
    let carriers = carriers(dir);
    files_below(&waiting_dir(dir))
        .into_iter()
        .filter(|(_, full, _)| !carriers.contains(full))
        .collect()
}

/// The files waiting to go into the Inbox.
pub fn waiting(dir: &Path) -> Vec<GuestFile> {
    let mut files: Vec<GuestFile> = loose_files(dir)
        .into_iter()
        .map(|(path, _, size)| GuestFile { path, size })
        .collect();
    for carrier in carriers(dir) {
        if let Ok(index) = carrier_index(&carrier) {
            files.extend(index.files.into_iter().map(|f| GuestFile {
                path: f.path,
                size: f.size,
            }));
        }
    }
    files
}

/// Puts the waiting files into the Inbox through `agent`: the carriers in
/// `attached` (each with the serial of its disk) through their disks, and
/// loose files through the agent's port. A carrier the environment did not
/// start with waits for the next start.
pub fn deliver(dir: &Path, agent: &mut Agent, attached: &[(PathBuf, String)]) -> Result<Delivered> {
    let files = loose_files(dir);
    let mut delivered = Delivered::default();
    let mut indexed = Vec::new();
    for carrier in carriers(dir) {
        match carrier_index(&carrier) {
            Ok(index) => indexed.push((carrier, index)),
            Err(e) => {
                warn!("{}: {e:#}", carrier.display());
                fs::remove_file(&carrier).ok();
                delivered.failed.push((
                    "a batch of files sent in".to_owned(),
                    format!("its copy on the host is damaged: {e:#}"),
                ));
            }
        }
    }
    let mut ready: Vec<(PathBuf, Index, String)> = Vec::new();
    for (carrier, index) in indexed {
        if let Some((_, serial)) = attached.iter().find(|(path, _)| *path == carrier) {
            ready.push((carrier, index, serial.clone()));
        } else {
            for file in &index.files {
                delivered
                    .stayed
                    .push((file.path.clone(), "it waits for the next start".to_owned()));
            }
        }
    }
    if files.is_empty() && ready.is_empty() {
        return Ok(delivered);
    }
    let count = files.len() + ready.iter().map(|(_, i, _)| i.files.len()).sum::<usize>();
    let bytes = files.iter().map(|(_, _, size)| size).sum::<u64>()
        + ready.iter().map(|(_, i, _)| i.bytes()).sum::<u64>();
    agent.announce(count, bytes)?;
    for (carrier, index, serial) in &ready {
        match agent.import(serial) {
            Ok((put, failed)) => {
                fs::remove_file(carrier).ok();
                delivered.files += put;
                delivered.failed.extend(failed);
            }
            Err(e) => {
                for file in &index.files {
                    delivered.stayed.push((file.path.clone(), format!("{e:#}")));
                }
                // The agent's connection is gone with the error.
                return Ok(delivered);
            }
        }
    }
    for (path, full, size) in &files {
        let put = File::open(full)
            .map_err(anyhow::Error::from)
            .and_then(|mut file| {
                let executable = file.metadata()?.mode() & 0o100 != 0;
                agent.put(path, &mut file, *size, executable)
            });
        match put {
            Ok(()) => {
                fs::remove_file(full).ok();
                delivered.files += 1;
            }
            // The others still go in; this one waits for the next start.
            Err(e) => delivered.stayed.push((path.clone(), format!("{e:#}"))),
        }
    }
    if delivered.stayed.is_empty() && delivered.failed.is_empty() {
        if carriers(dir).is_empty() {
            fs::remove_dir_all(waiting_dir(dir)).ok();
        }
    } else {
        // An empty batch ends the one the guest shows, short of its files.
        agent.announce(0, 0).ok();
    }
    Ok(delivered)
}

/// What [`deliver`] did: how many files went into the Inbox, the ones that
/// stayed waiting and the ones that could not go in, with why.
#[derive(Debug, Default)]
pub struct Delivered {
    pub files: usize,
    pub stayed: Vec<(String, String)>,
    pub failed: Vec<(String, String)>,
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
    agent.outgoing(new.len(), new.iter().map(|f| f.size).sum())?;
    let (passed, refused) = crate::daemon::take_files(agent, &mut spool, &new)?;
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
    let count = passed.len();
    agent.collected(passed, refused)?;
    Ok(count)
}

/// Takes the whole Outbox through `agent` into the spool, past the checks
/// by type; returns how many files passed and how many were refused.
pub fn spool_outbox(dir: &Path, agent: &mut Agent) -> Result<(usize, usize)> {
    let outbox = agent.files()?.outbox;
    let mut spool = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(spool(dir))?;
    let (passed, refused) = crate::daemon::take_files(agent, &mut spool, &outbox)?;
    let mut index = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(spool_index(dir))?;
    for file in &passed {
        writeln!(index, "{} {}", file.size, file.path)?;
    }
    Ok((passed.len(), refused.len()))
}

/// What a spool holds, in order.
enum Spooled {
    File { path: String, staged: PathBuf },
    Refused { path: String, reason: String },
}

/// Sends the spool to the client on `out` as a take would, every file past
/// `scan` first, and empties it; returns how many files went and how many
/// were refused. The files wait in `scratch` (a new folder) while `scan`
/// looks at them; one it finds infected is refused with what it found. A
/// scan that cannot run fails the take and keeps the spool for the next.
pub fn hand_over(
    dir: &Path,
    out: &mut impl io::Write,
    scratch: &Path,
    scan: &dyn Fn(&Path) -> Result<Vec<crate::antivirus::Found>>,
) -> Result<(usize, usize)> {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use gse_protocol::{Reply, send};
    use std::io::{BufRead as _, Read as _};
    let spooled = match File::open(spool(dir)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(e) => return Err(e.into()),
    };
    let staging = scratch.join("files");
    fs::remove_dir_all(scratch).ok();
    fs::create_dir_all(&staging)?;
    fs::set_permissions(scratch, fs::Permissions::from_mode(0o750))?;
    let outcome = (|| {
        let mut entries = Vec::new();
        let mut current: Option<File> = None;
        for line in io::BufReader::new(spooled).lines() {
            match serde_json::from_str::<Reply>(&line?)? {
                Reply::FileStart { path, .. } => {
                    if !plain(&path) {
                        bail!("the spool names the path '{path}'");
                    }
                    let staged = staging.join(&path);
                    if let Some(parent) = staged.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    current = Some(File::create(&staged)?);
                    entries.push(Spooled::File { path, staged });
                }
                Reply::FileChunk { data } => {
                    let bytes = BASE64
                        .decode(data)
                        .context("a spooled chunk is not base64")?;
                    current
                        .as_mut()
                        .context("the spool holds bytes of no file")?
                        .write_all(&bytes)?;
                }
                Reply::Refused { path, reason } => {
                    // A file refused after it began never reaches the client.
                    if let Some(Spooled::File { path: last, staged }) = entries.last()
                        && *last == path
                    {
                        fs::remove_file(staged).ok();
                        entries.pop();
                        current = None;
                    }
                    entries.push(Spooled::Refused { path, reason });
                }
                other => bail!("the spool holds {other:?}"),
            }
        }
        drop(current);
        let found = scan(&staging)?;
        let (mut passed, mut refused) = (0, 0);
        for entry in entries {
            match entry {
                Spooled::File { path, staged } => {
                    if let Some(hit) = found.iter().find(|hit| hit.path == staged) {
                        refused += 1;
                        send(
                            out,
                            &Reply::Refused {
                                path,
                                reason: format!("the antivirus found {}", hit.signature),
                            },
                        )?;
                        continue;
                    }
                    let mut file = File::open(&staged)?;
                    let size = file.metadata()?.len();
                    send(out, &Reply::FileStart { path, size })?;
                    let mut buffer = vec![0; gse_protocol::agent::CHUNK];
                    loop {
                        let read = file.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        send(
                            out,
                            &Reply::FileChunk {
                                data: BASE64.encode(&buffer[..read]),
                            },
                        )?;
                    }
                    passed += 1;
                }
                Spooled::Refused { path, reason } => {
                    refused += 1;
                    send(out, &Reply::Refused { path, reason })?;
                }
            }
        }
        Ok((passed, refused))
    })();
    fs::remove_dir_all(scratch).ok();
    let counts = outcome?;
    fs::remove_file(spool(dir)).ok();
    fs::remove_file(spool_index(dir)).ok();
    Ok(counts)
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
        // The file under way is cut off with the index after the whole one.
        let carrier = &carriers(dir.path())[0];
        assert_eq!(
            fs::metadata(carrier).unwrap().len(),
            (3 << 20) + 2 * carrier::BLOCK
        );
    }

    #[test]
    fn a_carrier_holds_each_file_from_an_aligned_offset() {
        use std::os::unix::fs::FileExt as _;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, "first").unwrap();
        fs::write(&b, vec![9_u8; 5000]).unwrap();
        let files = vec![
            ("Game/a.txt".to_owned(), File::open(&a).unwrap(), 5),
            ("Game/b.bin".to_owned(), File::open(&b).unwrap(), 5000),
        ];
        queue(dir.path(), files, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let carrier = &carriers(dir.path())[0];
        let index = carrier_index(carrier).unwrap();
        assert_eq!(
            index
                .files
                .iter()
                .map(|f| (f.path.as_str(), f.size, f.offset))
                .collect::<Vec<_>>(),
            [("Game/a.txt", 5, 0), ("Game/b.bin", 5000, 4096)]
        );
        let file = File::open(carrier).unwrap();
        let mut bytes = [0_u8; 5];
        file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"first");
        assert_eq!(fs::metadata(carrier).unwrap().len() % carrier::BLOCK, 0);
        // Files left as plain files by an older service still count.
        fs::write(waiting_dir(dir.path()).join("old.sav"), "x").unwrap();
        assert_eq!(waiting(dir.path()).len(), 3);
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
        fs::write(
            spool(dir.path()),
            "{\"reply\":\"file-start\",\"path\":\"Screenshots/a.png\",\"size\":8}\n\
             {\"reply\":\"file-chunk\",\"data\":\"cHJvZ3Jlc3M=\"}\n",
        )
        .unwrap();
        fs::write(spool_index(dir.path()), "8 Screenshots/a.png\n").unwrap();
        assert_eq!(
            taken(dir.path()),
            [GuestFile {
                path: "Screenshots/a.png".into(),
                size: 8,
            }]
        );
        let scratch = dir.path().join("scan");
        let mut out = Vec::new();
        let clean = |_: &Path| Ok(Vec::new());
        assert_eq!(
            hand_over(dir.path(), &mut out, &scratch, &clean).unwrap(),
            (1, 0)
        );
        let replies: Vec<gse_protocol::Reply> = out
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(
            replies,
            [
                gse_protocol::Reply::FileStart {
                    path: "Screenshots/a.png".into(),
                    size: 8
                },
                gse_protocol::Reply::FileChunk {
                    data: "cHJvZ3Jlc3M=".into()
                }
            ]
        );
        assert_eq!(
            hand_over(dir.path(), &mut out, &scratch, &clean).unwrap(),
            (0, 0)
        );

        fs::write(
            spool(dir.path()),
            "{\"reply\":\"file-start\",\"path\":\"bad.dll\",\"size\":3}\n\
             {\"reply\":\"file-chunk\",\"data\":\"YmFk\"}\n\
             {\"reply\":\"refused\",\"path\":\"run.sh\",\"reason\":\"a script\"}\n",
        )
        .unwrap();
        let infected = |staging: &Path| {
            Ok(vec![crate::antivirus::Found {
                path: staging.join("bad.dll"),
                signature: "Win.Trojan.Test".into(),
            }])
        };
        let mut out = Vec::new();
        assert_eq!(
            hand_over(dir.path(), &mut out, &scratch, &infected).unwrap(),
            (0, 2)
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("the antivirus found Win.Trojan.Test"));
        assert!(!text.contains("file-start"));
        assert!(!scratch.exists());

        fs::write(
            spool(dir.path()),
            "{\"reply\":\"refused\",\"path\":\"x\",\"reason\":\"y\"}\n",
        )
        .unwrap();
        let broken = |_: &Path| -> Result<Vec<crate::antivirus::Found>> { bail!("no engine") };
        assert!(hand_over(dir.path(), &mut Vec::new(), &scratch, &broken).is_err());
        assert!(spool(dir.path()).exists());
        assert_eq!(taken(dir.path()), Vec::new());
    }
}
