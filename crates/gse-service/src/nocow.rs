//! Copy-on-write for the files VMs write. On btrfs a VM disk that copies on
//! write fragments with every guest write and makes the host flush slowly,
//! and the page cache it fills stalls everything else that waits on the
//! disk. A folder marked no-copy-on-write passes the mark to the files made
//! in it from then on; files made before keep copying on write until they
//! are made anew (a rebuild, a reset). Other file systems have no such mark,
//! and marking there does nothing.

use std::path::Path;
use std::process::{Command, Stdio};

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
