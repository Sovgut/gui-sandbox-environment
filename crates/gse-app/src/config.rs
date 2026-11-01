//! The app's own settings, kept per user in `~/.config/gse/app.toml`: where
//! files taken out of a sandbox go. The service knows nothing of them.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct AppConfig {
    /// The folder archives of taken files are written to.
    pub received: PathBuf,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map_or_else(|| home().join(".config"), PathBuf::from)
        .join("gse/app.toml")
}

impl AppConfig {
    /// The settings saved, or the defaults: archives go to `~/GSE`.
    pub fn load() -> Self {
        let received = fs::read_to_string(path())
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
            .and_then(|table| table.get("received")?.as_str().map(PathBuf::from))
            .unwrap_or_else(|| home().join("GSE"));
        Self { received }
    }

    pub fn save(&self) -> Result<()> {
        let path = path();
        fs::create_dir_all(path.parent().context("config path")?)?;
        let mut table = toml::Table::new();
        table.insert(
            "received".into(),
            self.received.to_string_lossy().into_owned().into(),
        );
        fs::write(&path, toml::to_string(&table)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

/// Where an install put the service's program.
const SERVICE_PROGRAMS: [&str; 2] = [
    "/usr/local/sbin/gse-service",
    "/usr/libexec/gse/gse-service",
];

/// Removes everything gse put on the machine through the service's
/// `uninstall`, run as root through polkit, then the app's own settings
/// and, when given, the received files folder.
pub fn remove_everything(received: Option<&std::path::Path>) -> Result<()> {
    let program = SERVICE_PROGRAMS
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .context("the gse service is not installed")?;
    let status = std::process::Command::new("pkexec")
        .args([program, "uninstall"])
        .status()
        .context("starting pkexec")?;
    if !status.success() {
        anyhow::bail!("the uninstall was refused or failed ({status})");
    }
    if let Some(dir) = path().parent() {
        fs::remove_dir_all(dir).ok();
    }
    if let Some(received) = received {
        fs::remove_dir_all(received).ok();
    }
    Ok(())
}
