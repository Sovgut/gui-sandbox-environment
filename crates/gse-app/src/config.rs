//! The app's own settings, kept per user in `~/.config/gse/app.toml`: where
//! files taken out of a sandbox go, and what the user has been shown once.
//! The service knows nothing of them.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Which palette the app uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// The system's choice, light or dark.
    System,
    Light,
    Dark,
}

impl Look {
    /// Whether the light palette goes with this look in a window of
    /// `appearance`.
    pub fn light(self, appearance: gpui::WindowAppearance) -> bool {
        match self {
            Self::Light => true,
            Self::Dark => false,
            Self::System => matches!(
                appearance,
                gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

pub struct AppConfig {
    /// The folder archives of taken files are written to.
    pub received: PathBuf,
    /// The welcome tour ran to its end or was skipped.
    pub tour_done: bool,
    /// The set of apps the new sandbox dialog picks first.
    pub first_set: Option<String>,
    pub look: Look,
    /// The app looks for updates by itself.
    pub check_updates: bool,
    /// The user asked not to be told again that a sandbox takes the only
    /// graphics card.
    pub single_gpu_explained: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

pub(crate) fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map_or_else(|| home().join(".config"), PathBuf::from)
        .join("gse/app.toml")
}

impl AppConfig {
    /// The settings saved, or the defaults: archives go to `~/GSE`.
    pub fn load() -> Self {
        let table = fs::read_to_string(path())
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
            .unwrap_or_default();
        let flag = |key: &str| {
            table
                .get(key)
                .and_then(toml::Value::as_bool)
                .unwrap_or(false)
        };
        Self {
            received: table
                .get("received")
                .and_then(toml::Value::as_str)
                .map_or_else(|| home().join("GSE"), PathBuf::from),
            tour_done: flag("tour_done"),
            check_updates: table
                .get("check_updates")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true),
            look: match table.get("look").and_then(toml::Value::as_str) {
                Some("light") => Look::Light,
                Some("dark") => Look::Dark,
                _ => Look::System,
            },
            first_set: table
                .get("first_set")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            single_gpu_explained: flag("single_gpu_explained"),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = path();
        fs::create_dir_all(path.parent().context("config path")?)?;
        let mut table = toml::Table::new();
        table.insert(
            "received".into(),
            self.received.to_string_lossy().into_owned().into(),
        );
        table.insert("tour_done".into(), self.tour_done.into());
        table.insert("look".into(), self.look.name().into());
        table.insert("check_updates".into(), self.check_updates.into());
        if let Some(set) = &self.first_set {
            table.insert("first_set".into(), set.clone().into());
        }
        table.insert(
            "single_gpu_explained".into(),
            self.single_gpu_explained.into(),
        );
        fs::write(&path, toml::to_string(&table)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

/// Where an install put the service's program.
const SERVICE_PROGRAMS: [&str; 3] = [
    "/usr/local/libexec/gse/gse-service",
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
