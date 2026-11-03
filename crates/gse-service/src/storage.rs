//! Where the service keeps its data: the base image, the sets' images, the
//! sandboxes with their /home, the download cache and the antivirus. It is
//! always reached as [`DATA`]; on another disk, [`DATA`] is a link to a
//! folder `gse` there, and the service's unit waits for that disk to be
//! mounted. A move copies everything there, swaps the link and removes the
//! old copy, so nothing else in the service notices.

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, bail};
use tracing::info;

/// The path the service reaches its data by.
pub const DATA: &str = "/var/lib/gse";
/// The drop-in that has the service wait for the disk its data is on.
const DROP_IN: &str = "/etc/systemd/system/gse.service.d/storage.conf";
/// What a move must leave free on the target beyond the data itself: room
/// for a build and the sandboxes' growth.
const MARGIN: u64 = 20 << 30;

/// File systems the data may live on: they keep Unix owners and modes and
/// SELinux labels, and run disk images at full speed. NTFS, exFAT and FAT
/// do none of that.
const SUITABLE: [(i64, &str); 4] = [
    (0xEF53, "ext4"),
    (0x9123_683E, "btrfs"),
    (0x5846_5342, "xfs"),
    (0xF2F5_2010, "f2fs"),
];

/// Where the data is and how much room it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// The folder the data is in, the link followed.
    pub path: PathBuf,
    pub used: u64,
    pub free: u64,
}

/// Where the data is now, how much it takes and what is free beside it.
pub fn usage() -> Result<Usage> {
    let path = fs::canonicalize(DATA).with_context(|| format!("{DATA} is not there"))?;
    Ok(Usage {
        used: size_of(&path),
        free: free_on(&path)?,
        path,
    })
}

/// The folder the data goes to for a move to `to`: [`DATA`] itself for
/// `/var/lib` (back to the system disk), `to/gse` otherwise.
pub fn destination(to: &Path) -> PathBuf {
    if to == Path::new(DATA).parent().unwrap_or(Path::new("/")) {
        PathBuf::from(DATA)
    } else {
        to.join("gse")
    }
}

/// Checks that the data may move to `to`: an existing folder on a
/// [`SUITABLE`] file system, every folder above it in root's hands, and
/// room for the data and [`MARGIN`]. Returns the folder the data goes to.
pub fn check(to: &Path) -> Result<PathBuf> {
    let to = fs::canonicalize(to).with_context(|| format!("{} is not there", to.display()))?;
    if !to.is_dir() {
        bail!("{} is not a folder", to.display());
    }
    let destination = destination(&to);
    let current = fs::canonicalize(DATA)?;
    if destination == current {
        bail!("the sandboxes are kept in {} already", current.display());
    }
    if destination.starts_with(&current) {
        bail!("{} is inside the sandboxes' own folder", to.display());
    }
    if destination != Path::new(DATA) && destination.exists() {
        bail!(
            "{} exists already; choose a folder without a gse folder in it",
            destination.display()
        );
    }
    let kind = rustix::fs::statfs(&to)
        .with_context(|| format!("reading the file system of {}", to.display()))?
        .f_type;
    if !SUITABLE.iter().any(|(magic, _)| kind == *magic) {
        bail!(
            "{} is on a file system that cannot hold sandboxes (it keeps no Linux owners, \
             modes or SELinux labels, or runs disk images slowly): choose a disk with ext4, \
             btrfs, xfs or f2fs",
            to.display()
        );
    }
    for folder in to.ancestors() {
        let meta = fs::metadata(folder)?;
        let mode = meta.mode();
        let others_write = mode & 0o022 != 0 && mode & 0o1000 == 0;
        if meta.uid() != 0 || others_write {
            bail!(
                "{} is not root's alone: any program of a user could swap the images there. \
                 Choose a folder that only root can change, such as the top of a disk",
                folder.display()
            );
        }
    }
    let used = size_of(&current);
    let free = free_on(&to)?;
    if free < used.saturating_add(MARGIN) {
        bail!(
            "{} has {} free; the sandboxes need {} and room to grow",
            to.display(),
            gigabytes(free),
            gigabytes(used.saturating_add(MARGIN))
        );
    }
    Ok(destination)
}

/// Moves the data to `destination` (from [`check`]), telling `progress`
/// the bytes copied and in all. A raised `stop` ends it at the next file
/// and removes what it copied. The caller makes sure nothing runs.
pub fn move_to(
    destination: &Path,
    progress: &mut dyn FnMut(u64, u64),
    stop: &AtomicBool,
) -> Result<()> {
    let current = fs::canonicalize(DATA)?;
    let data = Path::new(DATA);
    let back = destination == data;
    // Back on the system disk the copy waits beside the link.
    let copy = if back {
        data.with_extension("moving")
    } else {
        destination.to_owned()
    };
    fs::remove_dir_all(data.with_extension("moving")).ok();
    let total = size_of(&current);
    let mut done = 0;
    progress(0, total);
    let copied = copy_tree(
        &current,
        &copy,
        &mut |bytes| {
            done += bytes;
            progress(done, total);
        },
        stop,
    );
    if let Err(e) = copied {
        fs::remove_dir_all(&copy).ok();
        return Err(e);
    }
    for dir in ["images", "environments", "builds"] {
        crate::nocow::mark(&copy.join(dir));
    }
    if back {
        // The link goes and the copy takes its place.
        fs::remove_file(data)?;
        fs::rename(&copy, data)?;
        fs::remove_file(DROP_IN).ok();
    } else {
        let link = data.with_extension("link");
        fs::remove_file(&link).ok();
        std::os::unix::fs::symlink(destination, &link)?;
        if current == data {
            // The data was on the system disk: it steps aside for the link.
            let aside = data.with_extension("old");
            fs::rename(data, &aside)?;
            fs::rename(&link, data)?;
        } else {
            fs::rename(&link, data)?;
        }
        write_drop_in(destination)?;
    }
    reload_units();
    // The old copy goes last, once the data is reachable at its new place.
    let old = if current == data && !back {
        data.with_extension("old")
    } else {
        current
    };
    fs::remove_dir_all(&old).with_context(|| format!("removing {}", old.display()))?;
    info!("the sandboxes are kept in {}", destination.display());
    Ok(())
}

/// The folder the data really is in, for removing it.
pub fn real_data() -> Option<PathBuf> {
    fs::canonicalize(DATA).ok()
}

/// The drop-in that has the service wait for `destination`'s disk.
fn write_drop_in(destination: &Path) -> Result<()> {
    let path = Path::new(DROP_IN);
    fs::create_dir_all(path.parent().context("drop-in folder")?)?;
    fs::write(
        path,
        format!(
            "# Written by gse-service: its data is kept on another disk.\n\
             [Unit]\nRequiresMountsFor={}\n",
            destination.display()
        ),
    )?;
    Ok(())
}

fn reload_units() {
    Command::new("systemctl")
        .arg("daemon-reload")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
}

/// Copies the folder `from` to `to` with owners, modes and times, files
/// through [`crate::mailbox::paced_copy`]; `copied` hears of the bytes.
fn copy_tree(from: &Path, to: &Path, copied: &mut dyn FnMut(u64), stop: &AtomicBool) -> Result<()> {
    let meta = fs::symlink_metadata(from)?;
    fs::create_dir(to).with_context(|| format!("creating {}", to.display()))?;
    keep_metadata(to, &meta)?;
    for entry in fs::read_dir(from)? {
        if stop.load(Ordering::SeqCst) {
            bail!("the move was stopped");
        }
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let meta = fs::symlink_metadata(&source)?;
        let kind = meta.file_type();
        if kind.is_dir() {
            copy_tree(&source, &target, copied, stop)?;
        } else if kind.is_file() {
            let mut input = fs::File::open(&source)?;
            let mut output = fs::File::create_new(&target)
                .with_context(|| format!("creating {}", target.display()))?;
            let mut counted = crate::mailbox::Counted::new(&mut input, copied).stopping(stop);
            crate::mailbox::paced_copy(&mut counted, &mut output).map_err(|e| {
                if crate::mailbox::is_stopped(&e) {
                    anyhow::anyhow!("the move was stopped")
                } else {
                    anyhow::Error::new(e).context(format!("copying {}", source.display()))
                }
            })?;
            keep_metadata(&target, &meta)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
        }
        // Sockets and pipes belong to a running service and stay behind.
    }
    Ok(())
}

/// Gives `path` the owner, mode and modification time in `meta`.
fn keep_metadata(path: &Path, meta: &fs::Metadata) -> Result<()> {
    std::os::unix::fs::chown(path, Some(meta.uid()), Some(meta.gid()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
    if meta.is_file() {
        fs::File::options()
            .write(true)
            .open(path)?
            .set_modified(meta.modified()?)?;
    }
    Ok(())
}

/// The bytes the regular files below `path` take.
fn size_of(path: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![path.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// The bytes free for root on the file system of `path`.
fn free_on(path: &Path) -> Result<u64> {
    let stats = rustix::fs::statvfs(path)
        .with_context(|| format!("reading the free space of {}", path.display()))?;
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

fn gigabytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // for people
    let gigabytes = bytes as f64 / 1e9;
    format!("{gigabytes:.0} GB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_lands_in_a_gse_folder_or_back_in_var_lib() {
        assert_eq!(
            destination(Path::new("/mnt/games")),
            Path::new("/mnt/games/gse")
        );
        assert_eq!(destination(Path::new("/var/lib")), Path::new(DATA));
    }

    #[test]
    fn a_tree_copies_with_its_files_and_stops_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir_all(from.join("images")).unwrap();
        fs::write(from.join("images/base.qcow2"), vec![1_u8; 70_000]).unwrap();
        fs::write(from.join("base.fingerprint"), "abc\n").unwrap();
        let mut seen = 0;
        copy_tree(
            &from,
            &dir.path().join("to"),
            &mut |n| seen += n,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(seen, 70_004);
        assert_eq!(
            fs::read(dir.path().join("to/images/base.qcow2"))
                .unwrap()
                .len(),
            70_000
        );
        assert_eq!(size_of(&dir.path().join("to")), 70_004);
        let stopped = copy_tree(
            &from,
            &dir.path().join("again"),
            &mut |_| {},
            &AtomicBool::new(true),
        );
        assert!(stopped.is_err());
    }
}
