//! What happened, kept as plain files a user can read without the service
//! or the app, so `gse --diagnostics` can gather them even when neither
//! works: the service's own log and each image build's guest console in
//! [`DIR`], and in each environment's folder its guest console and the
//! guest's journal.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::Result;

/// gse's log folder.
pub const DIR: &str = "/var/log/gse";
/// The service's log; the previous start's is beside it.
pub const SERVICE: &str = "/var/log/gse/service.log";
/// Image build consoles, one file per build.
const BUILDS: &str = "/var/log/gse/builds";
/// How many build consoles are kept.
const KEEP_BUILDS: usize = 12;
/// The size past which a log is moved aside when the service starts.
const ROTATE_AT: u64 = 16 << 20;
/// The size past which a running environment's log starts over: the guest
/// writes them, and a hostile one could fill the disk.
pub const RUNNING_LIMIT: u64 = 64 << 20;

/// The file beside `path` that holds what it held before: `x.log` becomes
/// `x.previous.log`.
pub fn previous(path: &Path) -> PathBuf {
    let stem = path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    path.with_file_name(format!("{stem}.previous.log"))
}

/// Moves `path` to its [`previous`] file when it holds more than `limit`
/// bytes; with a `limit` of 0, whenever it holds anything.
pub fn rotate(path: &Path, limit: u64) {
    if fs::metadata(path).is_ok_and(|m| m.len() > limit) {
        fs::rename(path, previous(path)).ok();
    }
}

/// Opens the service's log for appending, readable by everyone, after
/// moving a large one aside.
pub fn open_service_log() -> Result<fs::File> {
    fs::create_dir_all(DIR)?;
    fs::set_permissions(DIR, fs::Permissions::from_mode(0o755))?;
    rotate(Path::new(SERVICE), ROTATE_AT);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(SERVICE)?;
    Ok(file)
}

/// Keeps the guest console of a build of `image` that just ended, `ok` or
/// not, and drops the oldest past [`KEEP_BUILDS`].
pub fn keep_build_console(image: &str, console: &Path, ok: bool) {
    let keep = || -> Result<()> {
        fs::create_dir_all(BUILDS)?;
        fs::set_permissions(BUILDS, fs::Permissions::from_mode(0o755))?;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let verdict = if ok { "ok" } else { "failed" };
        let to = Path::new(BUILDS).join(format!("{at}-{image}-{verdict}.log"));
        fs::copy(console, &to)?;
        fs::set_permissions(&to, fs::Permissions::from_mode(0o644))?;
        let mut kept: Vec<PathBuf> = fs::read_dir(BUILDS)?
            .flatten()
            .map(|e| e.path())
            .collect();
        kept.sort();
        for old in kept.iter().rev().skip(KEEP_BUILDS) {
            fs::remove_file(old).ok();
        }
        Ok(())
    };
    if let Err(e) = keep() {
        tracing::warn!("keeping the build console of {image}: {e:#}");
    }
}

/// Starts the logs of an environment run in `dir`: the last run's move
/// aside, and the new ones are empty files of `uid`, the environment's
/// owner, who alone may read them. QEMU appends to them.
pub fn start_environment_logs(dir: &Path, uid: u32) -> Result<(PathBuf, PathBuf)> {
    let console = dir.join("console.log");
    let journal = dir.join("guest.log");
    for log in [&console, &journal] {
        rotate(log, 0);
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(log)?;
        std::os::unix::fs::fchown(&file, Some(uid), Some(uid))?;
        let previous = previous(log);
        if previous.exists() {
            std::os::unix::fs::chown(&previous, Some(uid), Some(uid)).ok();
            fs::set_permissions(&previous, fs::Permissions::from_mode(0o600)).ok();
        }
    }
    Ok((console, journal))
}

/// Starts `path` over once it holds more than `limit` bytes, saying so in
/// it. QEMU opens its logs to append, so it goes on writing from the start.
pub fn cap(path: &Path, limit: u64) {
    if fs::metadata(path).is_ok_and(|m| m.len() > limit)
        && let Ok(mut file) = fs::OpenOptions::new().write(true).open(path)
    {
        file.set_len(0).ok();
        writeln!(file, "--- gse: this log passed {limit} bytes and started over ---").ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_sits_beside() {
        assert_eq!(
            previous(Path::new("/x/console.log")),
            Path::new("/x/console.previous.log")
        );
    }

    #[test]
    fn a_large_log_moves_aside_and_a_capped_one_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("a.log");
        fs::write(&log, "0123456789").unwrap();
        rotate(&log, 20);
        assert!(log.exists());
        rotate(&log, 5);
        assert!(!log.exists());
        assert_eq!(fs::read_to_string(previous(&log)).unwrap(), "0123456789");

        fs::write(&log, "0123456789").unwrap();
        cap(&log, 5);
        assert!(fs::read_to_string(&log).unwrap().starts_with("--- gse:"));
    }
}
