//! The presets the service keeps: `<name>.toml` files in the presets
//! directory, in the format of `gse_host::preset`. Clients see and send them
//! as [`gse_protocol::Preset`]; everything they send is checked here before
//! it reaches a file, since the generated provisioning script runs as root
//! in the build VM. No preset is called [`BASE_IMAGE`]: a build of the base
//! image goes by that name.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gse_host::preset::{App, Preset as HostPreset, Source};
use gse_protocol::{AppSource, BASE_IMAGE, Preset, PresetApp, valid_name};
use tracing::warn;

/// The presets a fresh service starts with.
const BUILT_IN: [(&str, &str); 2] = [
    ("launchers", include_str!("../../../presets/launchers.toml")),
    ("desktop", include_str!("../../../presets/desktop.toml")),
];

/// The most applications one preset holds.
const MAX_APPS: usize = 64;
/// The longest application name and command argument.
const MAX_TEXT: usize = 256;

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

/// Whether `name` can name a preset.
fn preset_name(name: &str) -> bool {
    valid_name(name) && name != BASE_IMAGE
}

/// The names of the preset files in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "toml")
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .filter(|n| preset_name(n))
        .collect();
    names.sort();
    names
}

/// Whether `dir` holds the preset `name`.
pub fn exists(dir: &Path, name: &str) -> bool {
    preset_name(name) && path(dir, name).is_file()
}

/// The built-in presets, to start new ones from.
pub fn templates() -> Vec<Preset> {
    BUILT_IN
        .iter()
        .filter_map(|(name, text)| match HostPreset::parse(text) {
            Ok(preset) => Some(to_protocol(&preset)),
            Err(e) => {
                warn!("the built-in preset '{name}': {e:#}");
                None
            }
        })
        .collect()
}

/// Puts the built-in presets into `dir` when it holds no preset at all, so
/// a preset the user deleted stays deleted.
pub fn seed(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    if names(dir).is_empty() {
        for (name, text) in BUILT_IN {
            write_atomically(&path(dir, name), text.as_bytes())?;
        }
    }
    Ok(())
}

/// Reads the preset `name`.
pub fn load(dir: &Path, name: &str) -> Result<HostPreset> {
    if !preset_name(name) {
        bail!("no preset '{name}'");
    }
    let text = fs::read_to_string(path(dir, name)).map_err(|e| missing(name, e))?;
    let preset = HostPreset::parse(&text).with_context(|| format!("preset '{name}'"))?;
    if preset.name != name {
        bail!("the file of preset '{name}' names it '{}'", preset.name);
    }
    Ok(preset)
}

/// Every preset that reads back; a broken file is logged and left out.
pub fn list(dir: &Path) -> Vec<Preset> {
    names(dir)
        .into_iter()
        .filter_map(|name| match load(dir, &name) {
            Ok(preset) => Some(to_protocol(&preset)),
            Err(e) => {
                warn!("{e:#}");
                None
            }
        })
        .collect()
}

/// Checks `preset` and writes it, replacing any preset of the same name.
pub fn save(dir: &Path, preset: &Preset) -> Result<()> {
    validate(preset)?;
    let text = from_protocol(preset).to_toml()?;
    // What the build reads must be what was checked.
    let back = HostPreset::parse(&text)?;
    if to_protocol(&back) != *preset {
        bail!("preset '{}' does not survive being written", preset.name);
    }
    let header = format!(
        "# The preset '{}', kept by gse-service; the app edits it.\n\n",
        preset.name
    );
    write_atomically(&path(dir, &preset.name), (header + &text).as_bytes())
}

/// Gives the preset `name` the name `to`: the new file first, then the old
/// one goes.
pub fn rename(dir: &Path, name: &str, to: &str) -> Result<()> {
    if !preset_name(to) {
        bail!("'{to}' cannot name a preset");
    }
    if exists(dir, to) {
        bail!("a preset '{to}' exists");
    }
    let mut preset = to_protocol(&load(dir, name)?);
    to.clone_into(&mut preset.name);
    save(dir, &preset)?;
    delete(dir, name)
}

pub fn delete(dir: &Path, name: &str) -> Result<()> {
    if !preset_name(name) {
        bail!("no preset '{name}'");
    }
    fs::remove_file(path(dir, name)).map_err(|e| missing(name, e))
}

/// Says that `name` does not exist, or why its file could not be used.
fn missing(name: &str, error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!("no preset '{name}'")
    } else {
        anyhow::Error::new(error).context(format!("preset '{name}'"))
    }
}

/// Replaces the file at `path` in one step: the new file is written next to
/// it and renamed over it, so a reader sees the old or the new file, never a
/// part.
fn write_atomically(path: &Path, data: &[u8]) -> Result<()> {
    let mut name = path.file_name().context("no file name")?.to_owned();
    name.push(".new");
    let temp = path.with_file_name(name);
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(data)?;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        fs::rename(&temp, path)
    };
    write().map_err(|e| {
        fs::remove_file(&temp).ok();
        anyhow::Error::new(e).context(format!("writing {}", path.display()))
    })
}

fn to_protocol(preset: &HostPreset) -> Preset {
    Preset {
        name: preset.name.clone(),
        apps: preset
            .apps
            .iter()
            .filter_map(|app| {
                let source = match app.source().ok()? {
                    Source::Dnf { packages, repos } => AppSource::Dnf {
                        packages: packages.to_vec(),
                        repos: repos.to_vec(),
                    },
                    Source::Flatpak(id) => AppSource::Flatpak { id: id.to_owned() },
                    Source::AppImage(path) => AppSource::Appimage { path: path.clone() },
                };
                Some(PresetApp {
                    name: app.name.clone(),
                    source,
                    desktop: app.desktop.clone(),
                    command: app.command.clone(),
                    trusted: app.trusted,
                })
            })
            .collect(),
    }
}

fn from_protocol(preset: &Preset) -> HostPreset {
    HostPreset {
        name: preset.name.clone(),
        apps: preset
            .apps
            .iter()
            .map(|app| {
                let mut out = App {
                    name: app.name.clone(),
                    dnf: Vec::new(),
                    repos: Vec::new(),
                    flatpak: None,
                    appimage: None,
                    desktop: app.desktop.clone(),
                    command: app.command.clone(),
                    trusted: app.trusted,
                };
                match &app.source {
                    AppSource::Dnf { packages, repos } => {
                        out.dnf.clone_from(packages);
                        out.repos.clone_from(repos);
                    }
                    AppSource::Flatpak { id } => out.flatpak = Some(id.clone()),
                    AppSource::Appimage { path } => out.appimage = Some(path.clone()),
                }
                out
            })
            .collect(),
    }
}

/// Text on one line: it ends up in a desktop entry and a here-document of
/// the provisioning script, where a line break would start something new.
fn one_line(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

/// A package, repository, Flatpak ID or desktop file name: it reaches a
/// command line, so it cannot start with `-` and holds no shell or path
/// characters.
fn word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_TEXT
        && !text.starts_with('-')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

fn validate(preset: &Preset) -> Result<()> {
    if !valid_name(&preset.name) {
        bail!("preset names are 1-32 lowercase letters, digits and hyphens");
    }
    if preset.name == BASE_IMAGE {
        bail!("'{BASE_IMAGE}' is the base image's name; give the preset another");
    }
    if preset.apps.len() > MAX_APPS {
        bail!("a preset holds at most {MAX_APPS} apps");
    }
    let mut slugs = BTreeSet::new();
    for (app, host) in preset.apps.iter().zip(&from_protocol(preset).apps) {
        let name = &app.name;
        if name.trim().is_empty() || name.len() > MAX_TEXT || !one_line(name) {
            bail!("app names are 1-{MAX_TEXT} characters on one line");
        }
        match &app.source {
            AppSource::Dnf { packages, repos } => {
                if packages.is_empty() {
                    bail!("app '{name}': name at least one package");
                }
                if let Some(bad) = packages.iter().chain(repos).find(|w| !word(w)) {
                    bail!("app '{name}': '{bad}' is not a package or repository name");
                }
                if app.command.is_empty() {
                    bail!("app '{name}': a package app needs a command");
                }
            }
            AppSource::Flatpak { id } => {
                if !word(id) || id.contains('+') || id.split('.').count() < 3 {
                    bail!("app '{name}': '{id}' is not a Flathub ID such as com.example.App");
                }
            }
            AppSource::Appimage { path } => {
                if !path.is_absolute() || !path.to_str().is_some_and(one_line) {
                    bail!("app '{name}': the AppImage needs an absolute path");
                }
            }
        }
        if let Some(desktop) = &app.desktop
            && (!word(desktop) || !desktop.ends_with(".desktop"))
        {
            bail!("app '{name}': '{desktop}' is not a desktop file name");
        }
        if app
            .command
            .iter()
            .any(|a| a.is_empty() || a.len() > MAX_TEXT || !one_line(a))
        {
            bail!("app '{name}': every word of the command is 1-{MAX_TEXT} characters on one line");
        }
        let slug = host.slug();
        if slug.is_empty() {
            bail!("app '{name}': the name needs a letter or a digit");
        }
        if !slugs.insert(slug) {
            bail!("app '{name}': another app has nearly the same name");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, source: AppSource) -> PresetApp {
        PresetApp {
            name: name.into(),
            source,
            desktop: None,
            command: Vec::new(),
            trusted: false,
        }
    }

    fn flatpak(name: &str, id: &str) -> PresetApp {
        app(name, AppSource::Flatpak { id: id.into() })
    }

    #[test]
    fn the_built_in_presets_seed_an_empty_directory_only() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path()).unwrap();
        let presets = list(dir.path());
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["desktop", "launchers"]);
        assert!(presets[1].apps.iter().any(|a| a.trusted));

        delete(dir.path(), "launchers").unwrap();
        delete(dir.path(), "desktop").unwrap();
        let mine = Preset {
            name: "mine".into(),
            apps: vec![flatpak("Heroic", "com.heroicgameslauncher.hgl")],
        };
        save(dir.path(), &mine).unwrap();
        seed(dir.path()).unwrap();
        assert_eq!(list(dir.path()), [mine]);
    }

    #[test]
    fn the_built_in_presets_pass_every_check() {
        for preset in templates() {
            validate(&preset).unwrap_or_else(|e| panic!("{}: {e:#}", preset.name));
        }
        assert_eq!(templates().len(), BUILT_IN.len());
    }

    #[test]
    fn every_built_in_preset_is_a_template() {
        let names: Vec<String> = templates().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["launchers", "desktop"]);
    }

    #[test]
    fn a_renamed_preset_keeps_its_apps_under_the_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let mine = Preset {
            name: "mine".into(),
            apps: vec![flatpak("Heroic", "com.heroicgameslauncher.hgl")],
        };
        save(dir.path(), &mine).unwrap();
        rename(dir.path(), "mine", "games").unwrap();
        assert_eq!(
            list(dir.path()),
            [Preset {
                name: "games".into(),
                ..mine.clone()
            }]
        );
        save(dir.path(), &mine).unwrap();
        assert!(rename(dir.path(), "mine", "games").is_err());
        assert!(rename(dir.path(), "mine", "base").is_err());
    }

    #[test]
    fn saved_presets_read_back_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let mut steam = app(
            "Steam",
            AppSource::Dnf {
                packages: vec!["steam".into()],
                repos: vec!["rpmfusion-nonfree-steam".into()],
            },
        );
        steam.command = vec!["steam".into(), "-silent".into()];
        steam.desktop = Some("steam.desktop".into());
        let mut discord = flatpak("Discord", "com.discordapp.Discord");
        discord.trusted = true;
        let preset = Preset {
            name: "games".into(),
            apps: vec![
                steam,
                discord,
                app(
                    "Epic Games Launcher",
                    AppSource::Appimage {
                        path: "/home/me/epic.appimage".into(),
                    },
                ),
            ],
        };
        save(dir.path(), &preset).unwrap();
        assert_eq!(list(dir.path()), std::slice::from_ref(&preset));
        assert_eq!(load(dir.path(), "games").unwrap().apps.len(), 3);
        // Saving again replaces the file and leaves nothing beside it.
        save(dir.path(), &preset).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        delete(dir.path(), "games").unwrap();
        assert_eq!(list(dir.path()), []);
        assert!(delete(dir.path(), "games").is_err());
        assert!(load(dir.path(), "../games").is_err());
    }

    #[test]
    fn no_preset_takes_the_base_image_name() {
        let dir = tempfile::tempdir().unwrap();
        let base = Preset {
            name: BASE_IMAGE.into(),
            apps: vec![flatpak("Heroic", "com.heroicgameslauncher.hgl")],
        };
        assert!(save(dir.path(), &base).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);

        // A file put there by hand is no preset either.
        fs::write(path(dir.path(), BASE_IMAGE), "name = \"base\"\n").unwrap();
        assert_eq!(list(dir.path()), []);
        assert!(!exists(dir.path(), BASE_IMAGE));
        assert!(load(dir.path(), BASE_IMAGE).is_err());
        assert!(delete(dir.path(), BASE_IMAGE).is_err());
    }

    #[test]
    fn unsafe_presets_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let refused = |apps: Vec<PresetApp>| {
            save(
                dir.path(),
                &Preset {
                    name: "x".into(),
                    apps,
                },
            )
            .is_err()
        };
        assert!(
            save(
                dir.path(),
                &Preset {
                    name: "../x".into(),
                    apps: Vec::new()
                }
            )
            .is_err()
        );
        let dnf = |packages: &[&str], command: &[&str]| {
            let mut a = app(
                "A",
                AppSource::Dnf {
                    packages: packages.iter().map(|&p| p.into()).collect(),
                    repos: Vec::new(),
                },
            );
            a.command = command.iter().map(|&c| c.into()).collect();
            a
        };
        assert!(!refused(vec![dnf(&["steam"], &["steam"])]));
        assert!(refused(vec![dnf(&["steam"], &[])]), "dnf needs a command");
        assert!(refused(vec![dnf(&[], &["steam"])]));
        assert!(refused(vec![dnf(&["--setopt=x"], &["steam"])]));
        assert!(refused(vec![dnf(&["a b"], &["steam"])]));
        assert!(refused(vec![dnf(&["steam"], &["a\nGSE_ENTRY"])]));
        assert!(refused(vec![flatpak("A", "../../etc")]));
        assert!(refused(vec![flatpak("A", "-x.y.z")]));
        assert!(refused(vec![flatpak("A", "nodots")]));
        assert!(refused(vec![flatpak("Line\nbreak", "com.example.App")]));
        assert!(refused(vec![flatpak("  ", "com.example.App")]));
        assert!(refused(vec![flatpak("!!", "com.example.App")]));
        assert!(refused(vec![
            flatpak("My App", "com.example.App"),
            flatpak("my-app", "com.example.Other"),
        ]));
        assert!(refused(vec![app(
            "A",
            AppSource::Appimage {
                path: "relative.appimage".into()
            }
        )]));
        let mut bad_desktop = flatpak("A", "com.example.App");
        bad_desktop.desktop = Some("../x.desktop".into());
        assert!(refused(vec![bad_desktop]));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
