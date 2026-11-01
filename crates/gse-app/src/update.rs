//! Updates of the app, from the project's GitHub releases. While the app
//! runs as an AppImage it looks for a newer release when it starts and
//! every few hours, and the user decides whether to install it.
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
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// A release newer than the running app.
#[derive(Debug, Clone)]
pub struct Release {
    pub version: Version,
    appimage: String,
    signature: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// The AppImage file the app runs from, if it runs from one.
pub fn running_appimage() -> Option<PathBuf> {
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

/// Where the app stands with updates.
pub enum State {
    /// Not looked yet, or not an AppImage.
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Version),
    Failed(String),
}

impl crate::Home {
    /// Looks for a newer release, when the app runs as an AppImage.
    pub(crate) fn check_updates(&mut self, cx: &mut gpui::Context<Self>) {
        if running_appimage().is_none()
            || matches!(self.update, State::Checking | State::Installing(_))
        {
            return;
        }
        self.update = State::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async { newer() }).await;
            this.update(cx, |home, cx| {
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
        let (State::Available(release), Some(appimage)) = (&self.update, running_appimage()) else {
            return;
        };
        let release = release.clone();
        self.update = State::Installing(release.version.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { install(&release, &appimage).and_then(|()| start(&appimage)) })
                .await;
            this.update(cx, |home, cx| match outcome {
                Ok(()) => cx.quit(),
                Err(e) => {
                    home.update = State::Failed(format!("{e:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The notice of a newer release, above the right side.
    pub(crate) fn update_notice(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::Div> {
        use gpui::prelude::*;
        let installing = matches!(self.update, State::Installing(_));
        let version = match &self.update {
            State::Available(release) => release.version.to_string(),
            State::Installing(version) => version.to_string(),
            _ => return None,
        };
        Some(
            crate::ui::notice(
                crate::ui::Theme::accent(),
                format!("Version {version} is available"),
                "The update is checked against the project's signing key before it \
                 replaces this app. A running sandbox keeps running.",
            )
            .child(crate::ui::button(
                "update-install",
                if installing {
                    "Installing..."
                } else {
                    "Install and restart"
                },
                crate::ui::Kind::Primary,
                !installing,
                cx.listener(|home, _, _, cx| home.install_update(cx)),
            )),
        )
    }

    /// App settings: the version and looking for updates.
    pub(crate) fn updates_card(&self, cx: &mut gpui::Context<Self>) -> gpui::Div {
        use gpui::{div, prelude::*};
        let status: String = match &self.update {
            _ if running_appimage().is_none() => {
                "This copy does not run from an AppImage, so it does not update itself.".into()
            }
            State::Idle => String::new(),
            State::Checking => "Looking for a newer version...".into(),
            State::UpToDate => "This is the latest version.".into(),
            State::Available(release) => format!("Version {} is available.", release.version),
            State::Installing(version) => format!("Installing version {version}..."),
            State::Failed(e) => format!("Looking for updates failed: {e}"),
        };
        crate::ui::card(
            "Updates",
            Some("The app looks for a newer version when it starts and every six hours."),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(crate::ui::Theme::muted())
                        .child(format!("Version {}. {status}", env!("CARGO_PKG_VERSION"))),
                )
                .child(crate::ui::button(
                    "update-check",
                    "Check now",
                    crate::ui::Kind::Secondary,
                    running_appimage().is_some()
                        && !matches!(self.update, State::Checking | State::Installing(_)),
                    cx.listener(|home, _, _, cx| home.check_updates(cx)),
                )),
        )
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
    fn only_the_release_key_passes() {
        let other = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signature = ed25519_dalek::Signer::sign(&other, b"appimage").to_bytes();
        assert!(verify(b"appimage", &signature).is_err());
        assert!(verify(b"appimage", &[0; 10]).is_err());
        assert!(VerifyingKey::from_bytes(&PUBLIC_KEY).is_ok());
    }
}
