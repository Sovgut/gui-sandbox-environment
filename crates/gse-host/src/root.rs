//! Filesystem root for `/sys`, `/proc` and `/dev`, and sysfs attribute access.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Result, io_error};

#[derive(Debug, Clone)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// The running system.
    #[must_use]
    pub fn system() -> Self {
        Self::at("/")
    }

    /// A tree that mirrors the system layout, such as a test fixture.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Maps an absolute system path into this root.
    #[must_use]
    pub fn path(&self, system_path: impl AsRef<Path>) -> PathBuf {
        let system_path = system_path.as_ref();
        self.path
            .join(system_path.strip_prefix("/").unwrap_or(system_path))
    }

    /// Reads an attribute without its trailing newline.
    pub fn read(&self, system_path: impl AsRef<Path>) -> Result<String> {
        let path = self.path(system_path);
        fs::read_to_string(&path)
            .map(|s| s.trim_end().to_owned())
            .map_err(io_error(path))
    }

    /// Reads an attribute, or `None` when it is absent or unreadable.
    pub fn read_opt(&self, system_path: impl AsRef<Path>) -> Option<String> {
        self.read(system_path).ok()
    }

    /// The file name a symlink points to, such as the driver bound to a device.
    pub fn link_name(&self, system_path: impl AsRef<Path>) -> Option<String> {
        fs::read_link(self.path(system_path))
            .ok()
            .and_then(|target| target.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    /// Writes one value to a sysfs attribute as a single newline-terminated
    /// write. An empty value written as zero bytes never reaches the kernel,
    /// so clearing `driver_override` that way silently leaves it set.
    pub fn write(&self, system_path: impl AsRef<Path>, value: &str) -> Result<()> {
        let path = self.path(system_path);
        fs::write(&path, format!("{value}\n")).map_err(io_error(path))
    }

    /// Names of the entries in a directory, sorted; empty when it is missing.
    #[must_use]
    pub fn list(&self, system_path: impl AsRef<Path>) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path(system_path))
            .map(|entries| {
                entries
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[must_use]
    pub fn exists(&self, system_path: impl AsRef<Path>) -> bool {
        self.path(system_path).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_value_is_written_as_a_newline() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        fs::create_dir_all(root.path("/sys/x")).unwrap();
        root.write("/sys/x/driver_override", "").unwrap();
        assert_eq!(
            fs::read(root.path("/sys/x/driver_override")).unwrap(),
            b"\n"
        );
    }

    #[test]
    fn paths_are_mapped_under_the_root() {
        let root = Root::at("/tmp/fake");
        assert_eq!(root.path("/sys/bus"), PathBuf::from("/tmp/fake/sys/bus"));
    }
}
