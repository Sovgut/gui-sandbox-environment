//! cloud-init NoCloud seeds: the one-time instructions a build VM boots with.
//!
//! The seed is a small FAT volume labelled `CIDATA`, written here in Rust so
//! building an image needs no mkfs or mtools on the host.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use crate::{Error, Result, io_error};

/// The base-layer provisioning script, run once inside the build VM.
pub const BASE_PROVISION: &str = include_str!("../../../guest/base/provision.sh");

const SEED_BYTES: u64 = 2 * 1024 * 1024;

/// The `#cloud-config` that writes `script` into the guest and runs it as
/// root. With `dev_ssh_key`, a `dev` account with that key and passwordless
/// sudo is created and the script is told to keep it.
#[must_use]
pub fn cloud_config(script: &str, dev_ssh_key: Option<&str>) -> String {
    let mut out = String::from("#cloud-config\n");
    match dev_ssh_key {
        Some(key) => {
            let _ = writeln!(out, "users:");
            let _ = writeln!(out, "  - name: dev");
            let _ = writeln!(out, "    sudo: \"ALL=(ALL) NOPASSWD:ALL\"");
            let _ = writeln!(out, "    shell: /bin/bash");
            let _ = writeln!(out, "    ssh_authorized_keys:");
            let _ = writeln!(out, "      - {}", key.trim());
        }
        None => {
            let _ = writeln!(out, "users: []");
        }
    }
    let _ = writeln!(out, "ssh_pwauth: false");
    let _ = writeln!(out, "growpart: {{mode: auto, devices: [\"/\"]}}");
    let _ = writeln!(out, "write_files:");
    let _ = writeln!(out, "  - path: /root/provision.sh");
    let _ = writeln!(out, "    permissions: \"0700\"");
    let _ = writeln!(out, "    content: |");
    for line in script.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "      {line}");
        }
    }
    let dev = u8::from(dev_ssh_key.is_some());
    let _ = writeln!(out, "runcmd:");
    let _ = writeln!(
        out,
        "  - [env, \"GAMEJAIL_DEV={dev}\", bash, /root/provision.sh]"
    );
    out
}

/// Writes a NoCloud seed volume with `user_data` and an instance id.
pub fn write_nocloud(path: &Path, instance_id: &str, user_data: &str) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(io_error(path))?;
    file.set_len(SEED_BYTES).map_err(io_error(path))?;
    format_and_fill(file, instance_id, user_data)
        .map_err(|e| Error::Step(format!("{}: writing the seed: {e}", path.display())))
}

fn format_and_fill(mut file: File, instance_id: &str, user_data: &str) -> std::io::Result<()> {
    fatfs::format_volume(
        &mut file,
        fatfs::FormatVolumeOptions::new().volume_label(*b"CIDATA     "),
    )?;
    let fs = fatfs::FileSystem::new(file, fatfs::FsOptions::new())?;
    let root = fs.root_dir();
    root.create_file("meta-data")?
        .write_all(format!("instance-id: {instance_id}\nlocal-hostname: gamejail\n").as_bytes())?;
    root.create_file("user-data")?
        .write_all(user_data.as_bytes())?;
    drop(root);
    fs.unmount()
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    #[test]
    fn script_is_embedded_as_an_indented_block() {
        let config = cloud_config("#!/bin/bash\necho one\n\necho two\n", None);
        assert!(config.starts_with("#cloud-config\n"));
        assert!(config.contains("users: []"));
        assert!(
            config
                .contains("    content: |\n      #!/bin/bash\n      echo one\n\n      echo two\n")
        );
        assert!(config.contains("GAMEJAIL_DEV=0"));
    }

    #[test]
    fn development_builds_get_an_ssh_account() {
        let config = cloud_config("true\n", Some("ssh-ed25519 AAAA test\n"));
        assert!(config.contains("  - name: dev"));
        assert!(config.contains("      - ssh-ed25519 AAAA test\n"));
        assert!(config.contains("GAMEJAIL_DEV=1"));
    }

    #[test]
    fn seed_is_a_cidata_volume_with_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed.img");
        write_nocloud(&path, "build-1", "#cloud-config\n").unwrap();

        let fs =
            fatfs::FileSystem::new(File::open(&path).unwrap(), fatfs::FsOptions::new()).unwrap();
        assert_eq!(fs.volume_label().trim_end(), "CIDATA");
        let mut user_data = String::new();
        fs.root_dir()
            .open_file("user-data")
            .unwrap()
            .read_to_string(&mut user_data)
            .unwrap();
        assert_eq!(user_data, "#cloud-config\n");
        let mut meta = String::new();
        fs.root_dir()
            .open_file("meta-data")
            .unwrap()
            .read_to_string(&mut meta)
            .unwrap();
        assert!(meta.starts_with("instance-id: build-1\n"));
    }

    #[test]
    fn the_base_script_reports_a_verdict() {
        assert!(BASE_PROVISION.contains("GAMEJAIL-BUILD: $*"));
        assert!(BASE_PROVISION.contains("verdict \"OK\""));
    }
}
