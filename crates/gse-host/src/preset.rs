//! Presets: what an environment image contains on top of the base image.
//!
//! A preset is a TOML file listing applications, each installed from Fedora or
//! RPM Fusion, from Flathub, or from a local AppImage. An application runs as
//! `guest` unless it is marked `trusted`, which makes it run as `sandbox`. Every
//! `guest` application gets a menu entry on the desktop that starts it through
//! `gse-guest run`, overriding any entry its package installs, so clicking
//! it never starts it as `sandbox`; the same entries go onto the desktop. The
//! preset's `name` names its image; its `title`, if any, is what people call
//! it.
//!
//! ```toml
//! name = "launchers"
//! title = "Game launchers"
//!
//! [[app]]
//! name = "Steam"
//! dnf = ["steam"]
//! repos = ["rpmfusion-nonfree-steam"]
//! desktop = "steam.desktop"
//! command = ["steam"]
//!
//! [[app]]
//! name = "Heroic"
//! flatpak = "com.heroicgameslauncher.hgl"
//!
//! [[app]]
//! name = "Epic Games Launcher"
//! appimage = "/home/me/AppImages/epic_games_launcher.appimage"
//!
//! [[app]]
//! name = "Discord"
//! flatpak = "com.discordapp.Discord"
//! trusted = true
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Where AppImages are unpacked inside the image.
pub const APPS_DIR: &str = "/opt/gse/apps";
const GUEST_RUN: &str = "/usr/libexec/gse/gse-guest run";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, rename = "app")]
    pub apps: Vec<App>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dnf: Vec<String>,
    /// Repositories to enable while installing `dnf` packages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flatpak: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appimage: Option<PathBuf>,
    /// The desktop file this application's menu entry replaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop: Option<String>,
    /// What to run; derived from a Flatpak ID or AppImage when absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Runs as `sandbox` instead of `guest`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source<'a> {
    Dnf {
        packages: &'a [String],
        repos: &'a [String],
    },
    Flatpak(&'a str),
    AppImage(&'a PathBuf),
}

impl App {
    pub fn source(&self) -> Result<Source<'_>> {
        let bad = |why: &str| Error::Step(format!("app '{}': {why}", self.name));
        match (!self.dnf.is_empty(), &self.flatpak, &self.appimage) {
            (true, None, None) => Ok(Source::Dnf {
                packages: &self.dnf,
                repos: &self.repos,
            }),
            (false, Some(id), None) => Ok(Source::Flatpak(id)),
            (false, None, Some(path)) => Ok(Source::AppImage(path)),
            _ => Err(bad("needs exactly one of dnf, flatpak or appimage")),
        }
    }

    /// A file-name-safe form of the name.
    #[must_use]
    pub fn slug(&self) -> String {
        let slug: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        slug.split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// The name the AppImage travels under on the seed volume.
    #[must_use]
    pub fn seed_file(&self) -> String {
        format!("{}.appimage", self.slug())
    }

    fn command_line(&self) -> Result<Vec<String>> {
        if !self.command.is_empty() {
            return Ok(self.command.clone());
        }
        match self.source()? {
            Source::Flatpak(id) => Ok(vec!["flatpak".into(), "run".into(), id.into()]),
            Source::AppImage(_) => Ok(vec![format!("{APPS_DIR}/{}/AppRun", self.slug())]),
            Source::Dnf { .. } => Err(Error::Step(format!(
                "app '{}': a dnf application needs a command",
                self.name
            ))),
        }
    }

    fn desktop_id(&self) -> String {
        match (&self.desktop, &self.flatpak) {
            (Some(id), _) => id.clone(),
            (None, Some(flatpak)) => format!("{flatpak}.desktop"),
            (None, None) => format!("gse-{}.desktop", self.slug()),
        }
    }

    /// Writes the shell lines that create this application's menu entry in
    /// `$entries`.
    fn write_menu_entry(&self, s: &mut String) -> Result<()> {
        let command = self.command_line()?;
        let icon = match self.source()? {
            Source::Flatpak(id) => id.to_owned(),
            Source::AppImage(_) => format!("{APPS_DIR}/{}/.DirIcon", self.slug()),
            Source::Dnf { .. } => self
                .desktop
                .as_deref()
                .unwrap_or(&self.name)
                .trim_end_matches(".desktop")
                .to_owned(),
        };
        let exec = if self.trusted {
            join_desktop(&command)
        } else {
            let title = join_desktop(&["--title".to_owned(), self.name.clone()]);
            let icon = join_desktop(&["--icon".to_owned(), icon.clone()]);
            format!("{GUEST_RUN} {title} {icon} {}", join_desktop(&command))
        };
        let suffix = if self.trusted { "" } else { " (guest)" };
        let _ = writeln!(
            s,
            "cat > \"$entries\"/{} <<'GSE_ENTRY'",
            quote(&self.desktop_id())
        );
        let _ = writeln!(
            s,
            "[Desktop Entry]\nType=Application\nName={}{suffix}\nExec={exec}\nIcon={icon}\nCategories=Game;",
            self.name
        );
        let _ = writeln!(s, "GSE_ENTRY");
        Ok(())
    }
}

impl Preset {
    /// The file [`Preset::parse`] reads back as `self`.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).map_err(|e| Error::Step(format!("preset: {e}")))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let preset: Self = toml::from_str(text).map_err(|e| Error::Step(format!("preset: {e}")))?;
        for app in &preset.apps {
            app.source()?;
            app.command_line()?;
        }
        Ok(preset)
    }

    /// The AppImages to put on the seed volume, as (seed file name, host path).
    #[must_use]
    pub fn seed_files(&self) -> Vec<(String, PathBuf)> {
        self.apps
            .iter()
            .filter_map(|a| a.appimage.as_ref().map(|p| (a.seed_file(), p.clone())))
            .collect()
    }

    /// The provisioning script that installs this preset on the base image,
    /// with the same serial-console verdict as the base build.
    pub fn script(&self) -> Result<String> {
        let mut s = String::from(PRELUDE);
        let flatpaks: Vec<&str> = self
            .apps
            .iter()
            .filter_map(|a| a.flatpak.as_deref())
            .collect();
        let mut repos: Vec<&str> = self
            .apps
            .iter()
            .flat_map(|a| a.repos.iter().map(String::as_str))
            .collect();
        repos.sort_unstable();
        repos.dedup();
        let packages: Vec<&str> = self
            .apps
            .iter()
            .flat_map(|a| a.dnf.iter().map(String::as_str))
            .collect();

        if !packages.is_empty() {
            let _ = writeln!(s, "step \"packages\"");
            let _ = write!(s, "dnf -y install");
            for repo in &repos {
                let _ = write!(s, " --enablerepo={}", quote(repo));
            }
            let _ = writeln!(s, " {}", join_quoted(&packages));
        }
        if !flatpaks.is_empty() {
            let _ = writeln!(s, "step \"flatpaks\"");
            let _ = writeln!(
                s,
                "flatpak install --system -y --noninteractive flathub {}",
                join_quoted(&flatpaks)
            );
            s.push_str(NVIDIA_FLATPAK_GL);
        }
        if self.apps.iter().any(|a| a.appimage.is_some()) {
            let _ = writeln!(s, "step \"appimages\"");
            let _ = writeln!(
                s,
                "mkdir -p /run/gse-seed && mount -o ro LABEL=CIDATA /run/gse-seed"
            );
            for app in self.apps.iter().filter(|a| a.appimage.is_some()) {
                let dir = format!("{APPS_DIR}/{}", app.slug());
                let _ = writeln!(s, "rm -rf {0} && mkdir -p {0}", quote(&dir));
                let _ = writeln!(
                    s,
                    "cp /run/gse-seed/{} /tmp/app.appimage && chmod +x /tmp/app.appimage",
                    quote(&app.seed_file())
                );
                let _ = writeln!(
                    s,
                    "(cd /tmp && ./app.appimage --appimage-extract > /dev/null)"
                );
                let _ = writeln!(
                    s,
                    "cp -a /tmp/squashfs-root/. {}/ && rm -rf /tmp/squashfs-root /tmp/app.appimage",
                    quote(&dir)
                );
            }
            let _ = writeln!(s, "umount /run/gse-seed");
            let _ = writeln!(s, "chmod -R a+rX {APPS_DIR} && restorecon -R /opt/gse");
        }

        let _ = writeln!(s, "step \"menu\"");
        let _ = writeln!(s, "entries=/home/sandbox/.local/share/applications");
        let _ = writeln!(
            s,
            "install -d -o sandbox -g sandbox /home/sandbox/.local /home/sandbox/.local/share \"$entries\""
        );
        for app in &self.apps {
            app.write_menu_entry(&mut s)?;
        }
        let _ = writeln!(s, "chown -R sandbox:sandbox /home/sandbox/.local");
        // The same entries as desktop shortcuts, executable so the file
        // manager starts them without asking.
        let _ = writeln!(s, "install -d -o sandbox -g sandbox /home/sandbox/Desktop");
        for app in &self.apps {
            let _ = writeln!(
                s,
                "install -o sandbox -g sandbox -m 0755 \"$entries\"/{0} /home/sandbox/Desktop/{0}",
                quote(&app.desktop_id())
            );
        }
        s.push_str(TRUST_SHORTCUTS);
        s.push_str(EPILOGUE);
        Ok(s)
    }
}

/// The desktop's file manager runs a shortcut only once it is marked
/// trusted, a GIO metadata attribute of `sandbox`'s; a throwaway session bus
/// starts the metadata service, which needs no desktop.
const TRUST_SHORTCUTS: &str = r#"runuser -u sandbox -- dbus-run-session -- sh -c \
    'for f in /home/sandbox/Desktop/*.desktop; do gio set -t string "$f" metadata::trust true; done'
"#;

const PRELUDE: &str = r#"#!/bin/bash
# Installs a preset on the base image; generated by gse-service.
set -euo pipefail
verdict() {
    echo "GSE-BUILD: $*" > /dev/ttyS0
    systemctl --no-block poweroff
}
trap 'verdict "FAILED $LINENO"' ERR
step() { echo "=== $*" > /dev/ttyS0; }
"#;

/// Flatpak programs reach the NVIDIA driver only through a runtime extension
/// matching its exact version. Flatpak picks it by the loaded module, which
/// the GPU-less build VM does not have, so it is named from the package,
/// the current branch's or a legacy one's (`xorg-x11-drv-nvidia-580xx`); an
/// image without NVIDIA's driver needs none. Installed files keep the label
/// of the repository objects they link to until relabelled, and `guest`
/// cannot run them before that.
const NVIDIA_FLATPAK_GL: &str = r#"nvidia="$(rpm -qa --qf '%{NAME} %{VERSION}\n' 'xorg-x11-drv-nvidia*' \
    | awk '$1 ~ /^xorg-x11-drv-nvidia(-[0-9]+xx)?$/ { print $2; exit }' | tr . -)"
if [ -n "$nvidia" ]; then
    flatpak install --system -y --noninteractive flathub \
        "org.freedesktop.Platform.GL.nvidia-$nvidia" "org.freedesktop.Platform.GL32.nvidia-$nvidia"
fi
restorecon -R /var/lib/flatpak
"#;

const EPILOGUE: &str = r#"step "cleanup"
# What the system now puts in /home, for environments that keep their own.
/usr/libexec/gse/home snapshot
dnf clean all
truncate -s 0 /etc/machine-id
trap - ERR
verdict "OK"
"#;

/// Single-quotes a word for bash.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

fn join_quoted(words: &[&str]) -> String {
    words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

/// An argument list for a desktop entry's `Exec` key, quoting what needs it.
fn join_desktop(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-+:=".contains(c))
            {
                a.clone()
            } else {
                format!(
                    "\"{}\"",
                    a.replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('`', "\\`")
                        .replace('$', "\\$")
                )
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAUNCHERS: &str = r#"
name = "launchers"
title = "Game launchers"

[[app]]
name = "Steam"
dnf = ["steam"]
repos = ["rpmfusion-nonfree-steam"]
desktop = "steam.desktop"
command = ["steam"]

[[app]]
name = "Heroic"
flatpak = "com.heroicgameslauncher.hgl"

[[app]]
name = "Epic Games Launcher"
appimage = "/home/me/epic.appimage"

[[app]]
name = "Discord"
flatpak = "com.discordapp.Discord"
trusted = true
"#;

    #[test]
    fn parses_and_derives_commands() {
        let preset = Preset::parse(LAUNCHERS).unwrap();
        assert_eq!(preset.apps.len(), 4);
        assert_eq!(
            preset.apps[1].command_line().unwrap(),
            ["flatpak", "run", "com.heroicgameslauncher.hgl"]
        );
        assert_eq!(preset.apps[2].slug(), "epic-games-launcher");
        assert_eq!(
            preset.seed_files(),
            [(
                "epic-games-launcher.appimage".to_owned(),
                PathBuf::from("/home/me/epic.appimage")
            )]
        );
    }

    #[test]
    fn writes_what_it_parses() {
        let preset = Preset::parse(LAUNCHERS).unwrap();
        assert_eq!(preset.title.as_deref(), Some("Game launchers"));
        let text = preset.to_toml().unwrap();
        assert!(text.starts_with("name = \"launchers\"\ntitle = \"Game launchers\"\n"));
        assert!(
            text.contains(
                "[[app]]\nname = \"Heroic\"\nflatpak = \"com.heroicgameslauncher.hgl\"\n"
            )
        );
        assert!(!text.contains("trusted = false"));
        assert_eq!(Preset::parse(&text).unwrap(), preset);
        // A preset without a title is written without one.
        let untitled = Preset {
            title: None,
            ..preset
        };
        let text = untitled.to_toml().unwrap();
        assert!(!text.contains("title"));
        assert_eq!(Preset::parse(&text).unwrap(), untitled);
    }

    #[test]
    fn rejects_apps_with_no_or_several_sources() {
        assert!(Preset::parse("name = \"x\"\n[[app]]\nname = \"a\"\n").is_err());
        assert!(
            Preset::parse("name = \"x\"\n[[app]]\nname = \"a\"\ndnf = [\"a\"]\nflatpak = \"b\"\n")
                .is_err()
        );
        assert!(
            Preset::parse("name = \"x\"\n[[app]]\nname = \"a\"\ndnf = [\"a\"]\n").is_err(),
            "dnf needs a command"
        );
        assert!(Preset::parse("name = \"x\"\nmystery = 1\n").is_err());
    }

    #[test]
    fn guest_apps_start_through_the_launcher_and_trusted_ones_do_not() {
        let script = Preset::parse(LAUNCHERS).unwrap().script().unwrap();
        assert!(
            script
                .contains("Exec=/usr/libexec/gse/gse-guest run --title Steam --icon steam steam\n")
        );
        assert!(script.contains(
            "Exec=/usr/libexec/gse/gse-guest run --title Heroic --icon com.heroicgameslauncher.hgl \
             flatpak run com.heroicgameslauncher.hgl\n"
        ));
        assert!(script.contains(
            "Exec=/usr/libexec/gse/gse-guest run --title \"Epic Games Launcher\" \
             --icon /opt/gse/apps/epic-games-launcher/.DirIcon \
             /opt/gse/apps/epic-games-launcher/AppRun\n"
        ));
        assert!(script.contains("Name=Discord\nExec=flatpak run com.discordapp.Discord\n"));
        assert!(script.contains("\"$entries\"/'steam.desktop'"));
        assert!(script.contains(
            "install -o sandbox -g sandbox -m 0755 \"$entries\"/'steam.desktop' /home/sandbox/Desktop/'steam.desktop'\n"
        ));
        assert!(script.contains("\"$entries\"/'com.heroicgameslauncher.hgl.desktop'"));
    }

    #[test]
    fn script_installs_every_source_and_reports_a_verdict() {
        let script = Preset::parse(LAUNCHERS).unwrap().script().unwrap();
        assert!(script.contains("dnf -y install --enablerepo='rpmfusion-nonfree-steam' 'steam'\n"));
        assert!(script.contains("flatpak install --system -y --noninteractive flathub 'com.heroicgameslauncher.hgl' 'com.discordapp.Discord'\n"));
        assert!(script.contains("org.freedesktop.Platform.GL.nvidia-$nvidia"));
        assert!(
            script.contains("cp /run/gse-seed/'epic-games-launcher.appimage' /tmp/app.appimage")
        );
        assert!(script.ends_with("verdict \"OK\"\n"));
    }

    #[test]
    fn words_are_quoted_for_bash_and_desktop_entries() {
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(
            join_desktop(&["a b".into(), "$HOME".into(), "plain".into()]),
            "\"a b\" \"\\$HOME\" plain"
        );
    }
}
