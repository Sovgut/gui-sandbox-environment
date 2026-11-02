//! Updates of the app, from the project's GitHub releases. The app looks
//! for a newer release when it starts and then once a day, unless the user
//! turns that off, and the user decides whether to install it; only a copy
//! run from the AppImage can install one.
//!
//! A release carries the AppImage and its ed25519 signature, made by the
//! release workflow with a key whose public half is built in here. The
//! AppImage holds the service that runs as root, so one whose signature
//! does not check out is never installed. The new AppImage takes the place
//! of the running file and starts instead of it.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::Deserialize;

const LATEST: &str = "https://api.github.com/repos/Sovgut/gui-sandbox-environment/releases/latest";
/// Every release, for people to read.
const RELEASES: &str = "https://github.com/Sovgut/gui-sandbox-environment/releases";
/// The release assets: the AppImage and its raw 64-byte signature.
const APPIMAGE: &str = "gse-x86_64.AppImage";
const SIGNATURE: &str = "gse-x86_64.AppImage.sig";
/// The public half of the release signing key.
const PUBLIC_KEY: [u8; 32] = [
    0xd2, 0xb8, 0x3b, 0x8d, 0xb1, 0xc1, 0x2a, 0x34, 0x49, 0x58, 0xde, 0x71, 0x16, 0x36, 0x43, 0x6e,
    0xbe, 0xf2, 0x42, 0xb7, 0x84, 0x0a, 0x52, 0xec, 0x92, 0x64, 0xc5, 0xec, 0x37, 0x87, 0xf1, 0x2d,
];
/// The largest AppImage the app downloads.
const MAX_APPIMAGE: u64 = 512 << 20;
/// How often the app looks for a newer release.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The app's version; a preview's is the one the design shows.
pub fn version() -> &'static str {
    if crate::preview::active() {
        "0.1.0"
    } else {
        env!("CARGO_PKG_VERSION")
    }
}

/// The service's version as it last said it; a preview's service is the
/// app's version.
fn service_version() -> Option<String> {
    if crate::preview::active() {
        Some(version().to_owned())
    } else {
        crate::client::service_build()
    }
}

/// How the service's version compares with the app's, as About says it:
/// `service`, the version it said, leads when it is another release than
/// the app's; one release on an older or newer protocol says only how it
/// compares.
fn service_hint(matched: crate::client::Match, service: Option<&str>) -> String {
    use crate::client::Match;
    let app = version();
    let named = service.filter(|service| *service != app);
    match (matched, named) {
        (Match::Same, _) => format!("{}, matches the app", service.unwrap_or(app)),
        (Match::ServiceOlder, Some(service)) => format!("{service}, older than the app's {app}"),
        (Match::ServiceOlder, None) => format!("Older than the app's {app}"),
        (Match::ServiceNewer, Some(service)) => format!("{service}, newer than the app's {app}"),
        (Match::ServiceNewer, None) => format!("Newer than the app's {app}"),
    }
}

/// A release newer than the running app.
#[derive(Debug, Clone)]
pub struct Release {
    pub version: Version,
    /// What the release says it changes, as written on GitHub.
    pub notes: String,
    appimage: String,
    signature: String,
}

impl Release {
    /// A release that downloads nothing, for a preview of the updates page.
    pub fn preview(version: &str, notes: &str) -> Self {
        Self {
            version: Version::parse(version).expect("a preview version"),
            notes: notes.to_owned(),
            appimage: String::new(),
            signature: String::new(),
        }
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// The AppImage file the app runs from, if it runs from one.
pub fn running_appimage() -> Option<PathBuf> {
    if crate::preview::active() {
        return (!crate::preview::flag("no-appimage"))
            .then(|| PathBuf::from("/home/me/AppImages/gui_sandbox_environment.appimage"));
    }
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("gse/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// The latest release, when it is newer than this app.
pub fn newer() -> Result<Option<Release>> {
    let release: GithubRelease = agent()
        .get(LATEST)
        .set("Accept", "application/vnd.github+json")
        .call()
        .context("asking GitHub for the latest release")?
        .into_json()?;
    let version = Version::parse(release.tag_name.trim_start_matches('v'))
        .with_context(|| format!("the release '{}' has no version", release.tag_name))?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    if version <= current {
        return Ok(None);
    }
    let url = |name: &str| {
        release
            .assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
            .with_context(|| format!("the release {version} has no {name}"))
    };
    Ok(Some(Release {
        appimage: url(APPIMAGE)?,
        signature: url(SIGNATURE)?,
        notes: release.body.unwrap_or_default(),
        version,
    }))
}

fn download(url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    agent()
        .get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("{url} is larger than {limit} bytes");
    }
    Ok(bytes)
}

/// Whether `signature` is the release key's signature of `appimage`.
fn verify(appimage: &[u8], signature: &[u8]) -> Result<()> {
    let key = VerifyingKey::from_bytes(&PUBLIC_KEY)?;
    let signature = Signature::from_slice(signature).context("the signature is malformed")?;
    key.verify_strict(appimage, &signature)
        .context("the update's signature does not match; it was not installed")
}

/// Downloads `release`, checks its signature and puts it in place of the
/// AppImage at `appimage`.
pub fn install(release: &Release, appimage: &Path) -> Result<()> {
    let bytes = download(&release.appimage, MAX_APPIMAGE)?;
    let signature = download(&release.signature, 1024)?;
    verify(&bytes, &signature)?;
    let mut name = appimage.file_name().context("AppImage path")?.to_owned();
    name.push(".new");
    let new = appimage.with_file_name(name);
    let put = || -> Result<()> {
        fs::write(&new, &bytes)?;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
        fs::rename(&new, appimage)?;
        Ok(())
    };
    put()
        .inspect_err(|_| {
            fs::remove_file(&new).ok();
        })
        .with_context(|| format!("replacing {}", appimage.display()))
}

/// Starts the AppImage at `appimage`; the running app closes after.
pub fn start(appimage: &Path) -> Result<()> {
    std::process::Command::new(appimage)
        .spawn()
        .with_context(|| format!("starting {}", appimage.display()))?;
    Ok(())
}

/// A release's notes as a list: a line that starts with `-` or `*` as an
/// item with a disc before it, a heading as a plain line.
fn release_notes(notes: &str) -> gpui::Div {
    use crate::ui::Theme;
    use gpui::{div, prelude::*, px, relative};

    // A line of 13 px text 1.7 apart, the disc in the middle of its first
    // line, the text after the list's indent.
    let line = 22.;
    let mut list = div()
        .flex()
        .flex_col()
        .text_size(px(13.))
        .line_height(relative(1.7))
        .text_color(Theme::soft_text());
    for note in notes.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (item, text) = match note.strip_prefix("- ").or_else(|| note.strip_prefix("* ")) {
            Some(text) => (true, text),
            None => (false, note.trim_start_matches('#').trim()),
        };
        list = list.child(
            div()
                .flex()
                .items_start()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .w(px(18.))
                        .h(px(line))
                        .pl(px(3.))
                        .when(item, |marker| {
                            marker.child(div().size(px(4.)).rounded_full().bg(Theme::soft_text()))
                        }),
                )
                .child(div().flex_1().min_w_0().child(text.to_owned())),
        );
    }
    list
}

/// Where the app stands with updates.
pub enum State {
    /// Not looked yet, or not an AppImage.
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed(String),
}

impl crate::Home {
    /// Looks for a newer release: when asked, or on its own when the user
    /// keeps automatic checks on. Only the AppImage looks, since only it can
    /// install what it finds.
    pub(crate) fn check_updates(&mut self, asked: bool, cx: &mut gpui::Context<Self>) {
        if crate::preview::active() {
            if asked {
                self.preview_skips("looking for updates", cx);
            }
            return;
        }
        if running_appimage().is_none()
            || (!asked && !self.app_config.check_updates)
            || matches!(self.update, State::Checking | State::Installing(_))
        {
            return;
        }
        self.update = State::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async { newer() }).await;
            match &found {
                Ok(Some(release)) => tracing::info!("update available: {}", release.version),
                Ok(None) => tracing::info!("no newer release"),
                Err(e) => tracing::warn!("looking for updates: {e:#}"),
            }
            this.update(cx, |home, cx| {
                home.update_checked = Some(crate::clock::now());
                home.update = match found {
                    Ok(Some(release)) => State::Available(release),
                    Ok(None) => State::UpToDate,
                    Err(e) => State::Failed(format!("{e:#}")),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn install_update(&mut self, cx: &mut gpui::Context<Self>) {
        if self.preview_skips("installing the update", cx) {
            return;
        }
        let (State::Available(release), Some(appimage)) = (&self.update, running_appimage()) else {
            return;
        };
        let release = release.clone();
        tracing::info!("installing {} over {}", release.version, appimage.display());
        self.update = State::Installing(release.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { install(&release, &appimage).and_then(|()| start(&appimage)) })
                .await;
            this.update(cx, |home, cx| match outcome {
                Ok(()) => cx.quit(),
                Err(e) => {
                    tracing::warn!("installing the update: {e:#}");
                    home.update = State::Failed(format!("{e:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The banner of a newer release, above the right side.
    pub(crate) fn update_notice(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::Div> {
        use crate::ui::{Kind, Size, Tone, banner, button};
        use gpui::prelude::*;
        let (text, installing) = match &self.update {
            State::Available(release) => (
                format!(
                    "GSE {} is ready. The app restarts to install it; running sandboxes keep \
                     going.",
                    release.version
                ),
                false,
            ),
            State::Installing(release) => (
                format!(
                    "Installing GSE {}. The app restarts when it is in place; running sandboxes \
                     keep going.",
                    release.version
                ),
                true,
            ),
            _ => return None,
        };
        Some(
            banner(Tone::Accent, crate::icons::Icon::Download, text).child(
                button(
                    "update-install",
                    if installing {
                        "Installing..."
                    } else {
                        "Install"
                    },
                    Kind::Primary,
                    !installing,
                    cx.listener(|home, _, _, cx| home.install_update(cx)),
                )
                .size(Size::Banner),
            ),
        )
    }

    /// A newer release: the way to install it, and what it brings.
    fn release_news(
        &self,
        release: &Release,
        appimage: bool,
        cx: &mut gpui::Context<Self>,
    ) -> (gpui::Div, Option<gpui::Div>) {
        use crate::ui::{Kind, Theme, Tone, button};
        use gpui::{div, prelude::*, px};

        let installing = matches!(self.update, State::Installing(_));
        let card = div()
            .flex()
            .items_center()
            .gap_4()
            .p(px(18.))
            .rounded(px(12.))
            .bg(Tone::Accent.ground())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_base()
                            // A browser's line of 16 px Inter, so the line
                            // under it sits where the design has it.
                            .line_height(px(20.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if installing {
                                format!("GSE {} is being installed", release.version)
                            } else {
                                format!("GSE {} is available", release.version)
                            }),
                    )
                    .child(
                        div()
                            .mt(px(2.))
                            .text_size(px(13.))
                            .text_color(Theme::secondary_text())
                            .child(format!(
                                "You have {}. Signed by the GSE release key, checked before \
                                 installing.",
                                version()
                            )),
                    ),
            )
            .child(
                button(
                    "update-install",
                    if installing {
                        "Installing..."
                    } else {
                        "Install and restart"
                    },
                    Kind::Primary,
                    appimage && !installing,
                    cx.listener(|home, _, _, cx| home.install_update(cx)),
                )
                .size(crate::ui::Size::Large)
                .px_4()
                .when(!appimage, |b| {
                    b.tooltip("Only a copy run from the AppImage updates itself")
                }),
            );
        let notes = (!release.notes.trim().is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .px(px(18.))
                .py_4()
                .rounded(px(12.))
                .bg(Theme::surface())
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(format!("What's new in {}", release.version)),
                )
                .child(release_notes(&release.notes))
                .child(
                    div().flex().child(
                        crate::ui::link("all-releases", "All releases")
                            .on_click(|_, _, cx| cx.open_url(RELEASES)),
                    ),
                )
        });
        (card, notes)
    }

    /// When the app last looked for a newer version, or what came of it.
    fn checked_text(&self, appimage: bool) -> String {
        let last = |at: chrono::DateTime<chrono::Local>| {
            if crate::clock::now().date_naive() == at.date_naive() {
                format!("today at {}", at.format("%H:%M"))
            } else {
                at.format("%-d %b at %H:%M").to_string()
            }
        };
        match (&self.update, self.update_checked) {
            _ if !appimage => "Only the AppImage looks for updates".to_owned(),
            (State::Checking, _) => "Looking for a newer version...".to_owned(),
            (State::Failed(e), _) => format!("Looking failed: {e}"),
            (State::UpToDate, Some(at)) => format!("Up to date \u{b7} last checked {}", last(at)),
            (_, Some(at)) => format!("Last checked {}", last(at)),
            (_, None) => "Not checked yet".to_owned(),
        }
    }

    /// The row of the background service: its version and how it compares
    /// with the app's. A service that matches speaks the app's protocol and
    /// is the app's release.
    fn service_row(&self) -> gpui::Div {
        use crate::client::Match;
        use crate::settings::Rows;
        use crate::ui::Theme;
        use gpui::{div, prelude::*, px};

        let (state, color) = match self.service {
            Match::Same => ("Up to date", Theme::good()),
            Match::ServiceOlder => (
                if crate::setup::carries_service() {
                    "Older; update it"
                } else {
                    "Older; restart it"
                },
                Theme::warning(),
            ),
            Match::ServiceNewer => ("Update the app", Theme::warning()),
        };
        Rows::Page.row(
            "Background service",
            Some(service_hint(self.service, service_version().as_deref())),
            div().text_size(px(13.)).text_color(color).child(state),
        )
    }

    /// App settings, About and updates: a newer release when there is one,
    /// what it brings, and how the app and its service stand.
    pub(crate) fn updates_card(&self, cx: &mut gpui::Context<Self>) -> gpui::Div {
        use crate::settings::{Rows, page_title};
        use crate::ui::{Kind, Size, SwitchSize, Theme, button, switch};
        use gpui::{div, prelude::*, px};

        let appimage = running_appimage().is_some();
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(page_title("About and updates", None));
        let release = match &self.update {
            State::Available(release) | State::Installing(release) => Some(release),
            _ => None,
        };
        if let Some(release) = release {
            let (card, notes) = self.release_news(release, appimage, cx);
            page = page.child(card).children(notes);
        }
        if let Some(dev) = self.dev_service_card(cx) {
            page = page.child(dev);
        }
        let checked = self.checked_text(appimage);
        let auto = self.app_config.check_updates;
        let rows = Rows::Page;
        page.child(rows.group(
            None,
            vec![
                rows.row(
                    "Version",
                    Some(if appimage {
                        "Runs from the AppImage and updates itself".to_owned()
                    } else {
                        "Not run from the AppImage, so it does not update itself".to_owned()
                    }),
                    div().text_size(px(13.)).child(version()),
                ),
                rows.row(
                    "Check automatically",
                    Some("Once a day; installing always asks first".to_owned()),
                    switch("check-updates", auto && appimage, SwitchSize::Row)
                        .disabled(!appimage)
                        .on_change(cx.listener(|home, on: &bool, _, cx| {
                            home.app_config.check_updates = *on;
                            if let Err(e) = home.app_config.save() {
                                home.say(format!("The setting was not saved: {e:#}"), true);
                            }
                            cx.notify();
                        })),
                ),
                self.service_row(),
                rows.row(
                    "Diagnostics",
                    Some(
                        "The logs of the app, the service, image builds and sandboxes, with what \
                         this computer is, in one archive to send when something goes wrong"
                            .to_owned(),
                    ),
                    button(
                        "save-diagnostics",
                        "Save diagnostics",
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, _, cx| home.save_diagnostics(cx)),
                    )
                    .size(Size::Small),
                ),
                rows.note_row(
                    checked,
                    if matches!(self.update, State::Failed(_)) {
                        Theme::danger_text()
                    } else {
                        Theme::muted()
                    },
                    button(
                        "update-check",
                        "Check now",
                        Kind::Secondary,
                        appimage && !matches!(self.update, State::Checking | State::Installing(_)),
                        cx.listener(|home, _, _, cx| home.check_updates(true, cx)),
                    )
                    .size(Size::Small),
                ),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes<const N: usize>(hex: &str) -> [u8; N] {
        let mut out = [0; N];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn signatures_made_by_openssl_check_out() {
        // `openssl pkeyutl -sign -rawin`, as the release workflow signs, with
        // a key made for this test only.
        let key = VerifyingKey::from_bytes(&bytes(
            "a7a48e43dae7bdd79e95650ad71c18ff8a0d1fd9f4193d72da41accbf36efb9c",
        ))
        .unwrap();
        let signature = Signature::from_bytes(&bytes(
            "38a061255841d47b0bdef18dc3a99224dbecbc0416bac6f43625ff6ab0b33104\
             56eedd2e0f77fdc0884d0c66e309db0d563477bb85fafc839f67307b7dacb508",
        ));
        assert!(key.verify_strict(b"gse release", &signature).is_ok());
        assert!(key.verify_strict(b"gse releasf", &signature).is_err());
    }

    #[test]
    fn about_names_the_service_version_beside_the_apps() {
        use crate::client::Match;
        let app = version();
        assert_eq!(
            service_hint(Match::Same, Some(app)),
            format!("{app}, matches the app")
        );
        assert_eq!(
            service_hint(Match::Same, None),
            format!("{app}, matches the app")
        );
        assert_eq!(
            service_hint(Match::ServiceOlder, Some("0.0.9")),
            format!("0.0.9, older than the app's {app}")
        );
        // One release on an older protocol, as a development build is.
        assert_eq!(
            service_hint(Match::ServiceOlder, Some(app)),
            format!("Older than the app's {app}")
        );
        assert_eq!(
            service_hint(Match::ServiceNewer, Some("99.0.0")),
            format!("99.0.0, newer than the app's {app}")
        );
        assert_eq!(
            service_hint(Match::ServiceNewer, None),
            format!("Newer than the app's {app}")
        );
    }

    #[test]
    fn only_the_release_key_passes() {
        let other = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signature = ed25519_dalek::Signer::sign(&other, b"appimage").to_bytes();
        assert!(verify(b"appimage", &signature).is_err());
        assert!(verify(b"appimage", &[0; 10]).is_err());
        assert!(VerifyingKey::from_bytes(&PUBLIC_KEY).is_ok());
    }
}
