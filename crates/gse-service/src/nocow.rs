//! Copy-on-write for the files VMs write. On btrfs a VM disk that copies on
//! write fragments with every guest write and makes the host flush slowly,
//! and the page cache it fills stalls everything else that waits on the
//! disk. A folder marked no-copy-on-write passes the mark to the files made
//! in it from then on; files made before keep copying on write until they
//! are made anew: by a rebuild, a reset, or [`rewrite`], which the service
//! runs on the disks of sandboxes while nothing else goes on. Other file
//! systems have no such mark, and marking there does nothing.

use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, bail};

/// What a rewrite leaves free on the disk beyond the copy it makes.
const MARGIN: u64 = 10 << 30;
/// How much a rewrite writes before it waits for that part to be on disk.
const PACE: u64 = 32 << 20;

/// Makes the file `path` anew without copy-on-write: a copy written beside
/// it, in its folder marked so, takes its place in one rename. Holes stay
/// holes. Returns false, with nothing changed, when the file has the mark
/// already, its file system has none to give, or the disk lacks room for
/// the copy; a raised `stop` ends it with an error and removes the copy.
/// Nothing else may have the file open.
pub fn rewrite(path: &Path, stop: &AtomicBool) -> Result<bool> {
    if is_set(path) {
        return Ok(false);
    }
    let folder = path.parent().context("the file's folder")?;
    mark(folder);
    if !is_set(folder) {
        return Ok(false);
    }
    let meta = fs::metadata(path)?;
    let free = rustix::fs::statvfs(folder)?;
    if free.f_bavail.saturating_mul(free.f_frsize) < (meta.blocks() * 512).saturating_add(MARGIN) {
        return Ok(false);
    }
    let mut name = path.file_name().context("the file's name")?.to_owned();
    name.push(".rewrite");
    let copy = path.with_file_name(name);
    fs::remove_file(&copy).ok();
    let made = (|| -> Result<()> {
        let mut from = File::open(path)?;
        let mut to = File::create_new(&copy)?;
        if !is_set(&copy) {
            bail!("the copy would copy on write as well");
        }
        copy_data(&mut from, &mut to, meta.len(), stop)?;
        std::os::unix::fs::chown(&copy, Some(meta.uid()), Some(meta.gid()))?;
        to.set_permissions(fs::Permissions::from_mode(meta.mode() & 0o7777))?;
        to.sync_all()?;
        fs::rename(&copy, path)?;
        File::open(folder)?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = made {
        fs::remove_file(&copy).ok();
        return Err(e);
    }
    Ok(true)
}

/// Copies the `len` bytes of `from` into `to`, leaving its holes out, and
/// waits every [`PACE`] for what it wrote to be on disk.
fn copy_data(from: &mut File, to: &mut File, len: u64, stop: &AtomicBool) -> Result<()> {
    let mut buffer = vec![0; 1 << 20];
    let mut unsynced = 0_u64;
    let mut at = 0_u64;
    while at < len {
        // The next stretch of data; none left means a hole to the end.
        let Ok(data) = rustix::fs::seek(&*from, rustix::fs::SeekFrom::Data(at)) else {
            break;
        };
        let hole = rustix::fs::seek(&*from, rustix::fs::SeekFrom::Hole(data)).unwrap_or(len);
        from.seek(SeekFrom::Start(data))?;
        to.seek(SeekFrom::Start(data))?;
        at = data;
        while at < hole {
            if stop.load(Ordering::SeqCst) {
                bail!("stopped");
            }
            let part = usize::try_from((hole - at).min(buffer.len() as u64))?;
            from.read_exact(&mut buffer[..part])?;
            to.write_all(&buffer[..part])?;
            at += part as u64;
            unsynced += part as u64;
            if unsynced >= PACE {
                unsynced = 0;
                to.sync_data()?;
            }
        }
    }
    to.set_len(len)?;
    Ok(())
}

/// Marks the folder `dir` so the files made in it do not copy on write.
pub fn mark(dir: &Path) {
    let marked = Command::new("chattr")
        .arg("+C")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if marked {
        tracing::debug!("{} makes files without copy-on-write", dir.display());
    }
}

/// Whether `path` is a file that does not copy on write.
pub fn is_set(path: &Path) -> bool {
    Command::new("lsattr")
        .arg("-d")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| flags_say_nocow(&String::from_utf8_lossy(&out.stdout)))
}

/// Whether a line of `lsattr` carries the no-copy-on-write flag `C`.
fn flags_say_nocow(line: &str) -> bool {
    line.split_whitespace()
        .next()
        .is_some_and(|flags| flags.contains('C'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a real btrfs folder, named by `GSE_NOCOW_DIR`: a file that copies
    /// on write is made anew without, with its bytes, owner and mode.
    #[test]
    #[ignore = "needs a folder on btrfs in GSE_NOCOW_DIR"]
    fn a_file_on_btrfs_loses_copy_on_write() {
        let dir = std::path::PathBuf::from(std::env::var("GSE_NOCOW_DIR").unwrap()).join("rewrite");
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let disk = dir.join("disk.qcow2");
        let data: Vec<u8> = (0..(40_usize << 20))
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        fs::write(&disk, &data).unwrap();
        fs::set_permissions(&disk, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(!is_set(&disk));
        assert!(rewrite(&disk, &AtomicBool::new(false)).unwrap());
        assert!(is_set(&disk));
        assert_eq!(fs::read(&disk).unwrap(), data);
        assert_eq!(fs::metadata(&disk).unwrap().mode() & 0o777, 0o640);
        assert!(!rewrite(&disk, &AtomicBool::new(false)).unwrap());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_keeps_data_and_holes() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let mut from = File::create_new(&a).unwrap();
        from.write_all(&[7; 5000]).unwrap();
        from.seek(SeekFrom::Start(8 << 20)).unwrap();
        from.write_all(&[9; 3000]).unwrap();
        from.set_len(16 << 20).unwrap();
        let mut from = File::open(&a).unwrap();
        let mut to = File::create_new(&b).unwrap();
        copy_data(&mut from, &mut to, 16 << 20, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
        assert!(fs::metadata(&b).unwrap().blocks() * 512 < 2 << 20);
        let mut again = File::create(&b).unwrap();
        assert!(copy_data(&mut from, &mut again, 16 << 20, &AtomicBool::new(true)).is_err());
    }

    #[test]
    fn the_c_flag_is_read_from_lsattr() {
        assert!(flags_say_nocow(
            "---------------C------ /var/lib/gse/images\n"
        ));
        assert!(!flags_say_nocow(
            "---------------------- /var/lib/gse/images\n"
        ));
        assert!(!flags_say_nocow(""));
    }
}
