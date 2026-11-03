//! Everything worth sending when something goes wrong, in one zip archive:
//! the logs of the app, its install, the service, image builds, the
//! antivirus's last run and every sandbox, the settings, what the
//! computer is, and what the service makes of its CPU and GPUs. It reads only files a
//! user may read and runs only commands a user may run, so it works without
//! the service and without the window: `gse --diagnostics` makes the same
//! archive from a terminal.

use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result};
use zip::write::SimpleFileOptions;

/// The most of each log the archive keeps, from its end.
const LOG_TAIL: u64 = 16 << 20;
/// The service's folders of logs and sandboxes.
const SERVICE_LOGS: &str = "/var/log/gse";
const ENVIRONMENTS: &str = "/var/lib/gse/environments";
/// The files of a sandbox's folder that say what happened to it; its disk
/// stays out.
const SANDBOX_FILES: [&str; 8] = [
    "journal",
    "settings.toml",
    "image",
    "base",
    "console.log",
    "console.previous.log",
    "guest.log",
    "guest.previous.log",
];

/// What the computer is and how gse stands on it, by the commands that say
/// so; each runs as the user and may be missing.
const HOST_COMMANDS: [(&str, &[&str]); 16] = [
    ("uname", &["-a"]),
    ("cat", &["/etc/os-release"]),
    ("cat", &["/proc/cmdline"]),
    ("rpm-ostree", &["status"]),
    (
        "rpm",
        &[
            "-q",
            "qemu-system-x86-core",
            "qemu-img",
            "qemu-audio-pa",
            "qemu-device-usb-host",
            "edk2-ovmf",
            "passt",
            "passt-selinux",
            "nftables",
            "kernel",
        ],
    ),
    ("lspci", &["-nnk"]),
    ("sh", &["-c", "ls /sys/kernel/iommu_groups | wc -l"]),
    (
        "sh",
        &[
            "-c",
            "for d in /sys/kernel/iommu_groups/*/devices/*; do echo \"${d#/sys/kernel/iommu_groups/}\"; done",
        ],
    ),
    ("ls", &["-l", "/dev/kvm", "/dev/vfio"]),
    (
        "sh",
        &[
            "-c",
            "lsmod | grep -E 'vfio|nvidia|nouveau|amdgpu|i915|xe|kvm'",
        ],
    ),
    ("nvidia-smi", &["-L"]),
    ("free", &["-m"]),
    ("df", &["-h", "/var/lib/gse", "/home"]),
    ("systemctl", &["status", "gse", "--no-pager"]),
    (
        "journalctl",
        &["-u", "gse", "-b", "--no-pager", "-n", "5000"],
    ),
    (
        "journalctl",
        &["-u", "gse", "-b", "-1", "--no-pager", "-n", "5000"],
    ),
];

/// Makes the archive in the received files folder and says where it is.
pub fn save() -> Result<PathBuf> {
    let folder = crate::config::AppConfig::load().received;
    fs::create_dir_all(&folder).with_context(|| format!("{}", folder.display()))?;
    let at = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let archive = folder.join(format!("gse-diagnostics-{at}.zip"));
    tracing::info!("saving diagnostics to {}", archive.display());
    write(&archive).with_context(|| format!("{}", archive.display()))?;
    Ok(archive)
}

fn write(archive: &Path) -> Result<()> {
    let mut zip = zip::ZipWriter::new(File::create_new(archive)?);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let mut missing = Vec::new();
    let mut add = |zip: &mut zip::ZipWriter<File>, name: String, path: &Path| -> Result<()> {
        match tail(path) {
            Ok(bytes) => {
                zip.start_file(name, options)?;
                zip.write_all(&bytes)?;
            }
            Err(e) if path.exists() => missing.push(format!("{}: {e}", path.display())),
            Err(_) => {}
        }
        Ok(())
    };

    let app = crate::applog::dir();
    for file in ["app.log", "app.previous.log", "install.log"] {
        add(&mut zip, format!("app/{file}"), &app.join(file))?;
    }
    let config = crate::config::path();
    add(&mut zip, "settings/app.toml".into(), &config)?;
    add(
        &mut zip,
        "settings/gse.toml".into(),
        Path::new("/etc/gse/gse.toml"),
    )?;
    for path in files_in(Path::new(SERVICE_LOGS)) {
        let name = path.strip_prefix(SERVICE_LOGS).unwrap_or(&path);
        add(&mut zip, format!("service/{}", name.display()), &path)?;
    }
    for file in ["stdout", "stderr"] {
        add(
            &mut zip,
            format!("service/antivirus/{file}"),
            &Path::new("/var/lib/gse/clamav/runs").join(file),
        )?;
    }
    for sandbox in folders_in(Path::new(ENVIRONMENTS)) {
        let name = sandbox.file_name().unwrap_or_default().to_string_lossy();
        for file in SANDBOX_FILES {
            add(
                &mut zip,
                format!("sandboxes/{name}/{file}"),
                &sandbox.join(file),
            )?;
        }
    }

    zip.start_file("host.txt", options)?;
    zip.write_all(host().as_bytes())?;
    zip.start_file("hardware.txt", options)?;
    match crate::client::hardware() {
        Ok(text) => zip.write_all(text.as_bytes())?,
        Err(e) => zip.write_all(format!("the service did not answer: {e:#}\n").as_bytes())?,
    }
    if !missing.is_empty() {
        zip.start_file("unreadable.txt", options)?;
        zip.write_all(missing.join("\n").as_bytes())?;
    }
    zip.finish()?;
    Ok(())
}

/// The last [`LOG_TAIL`] bytes of `path`.
fn tail(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Every file under `dir`, at any depth.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_in(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// The folders right under `dir`.
fn folders_in(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found
}

/// What [`HOST_COMMANDS`] say, each under its own heading.
fn host() -> String {
    use std::fmt::Write as _;
    let mut text = format!(
        "gse {}\napp log folder: {}\n",
        env!("CARGO_PKG_VERSION"),
        crate::applog::dir().display()
    );
    for (program, args) in HOST_COMMANDS {
        writeln!(text, "\n=== {program} {}", args.join(" ")).ok();
        match Command::new(program).args(args).output() {
            Ok(out) => {
                text.push_str(&String::from_utf8_lossy(&out.stdout));
                text.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            Err(e) => {
                writeln!(text, "({e})").ok();
            }
        }
    }
    text
}

impl crate::Home {
    /// Saves the archive in the background and says where it went.
    pub(crate) fn save_diagnostics(&mut self, cx: &mut gpui::Context<Self>) {
        if self.preview_skips("saving diagnostics", cx) {
            return;
        }
        self.say("Saving diagnostics...", false);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = cx.background_executor().spawn(async { save() }).await;
            this.update(cx, |home, cx| {
                match saved {
                    Ok(path) => home.tell(
                        crate::Toast::result("Diagnostics saved; send this file").wrote(path),
                    ),
                    Err(e) => home.say(format!("Diagnostics were not saved: {e:#}"), true),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
