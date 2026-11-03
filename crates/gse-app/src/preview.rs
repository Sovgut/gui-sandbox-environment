//! Previews: the app shows one of its pages in a given state, filled with
//! made-up sandboxes, sets and files. It is how the pages are compared with
//! the design, and every state looks the same on every machine and in every
//! run:
//!
//! - nothing reaches the service: requests fail at once, and buttons that
//!   would send one say so instead (`Home::run`);
//! - nothing touches the machine: no polkit prompt, network, file written,
//!   disk measured or config read; what the app learns from the machine
//!   (an AppImage, the installed service, the dev helper, kwinrc, GPUs the
//!   app started for) is made up too, and [`flag`]s switch it per state;
//! - the clock stands at [`now`], and animations stand still.
//!
//! `GSE_PREVIEW=<state>` picks the state, `GSE_PREVIEW=list` prints them,
//! and `GSE_PREVIEW_LOOK=light` shows it in the light look (dark otherwise,
//! as the design is).

mod data;
mod states;

use gpui::Context;

use crate::{Home, client};

pub use data::flathub;

/// The state to show, when the app runs as a preview.
pub fn state() -> Option<String> {
    std::env::var("GSE_PREVIEW").ok().filter(|s| !s.is_empty())
}

/// Whether the app runs as a preview.
pub fn active() -> bool {
    state().is_some()
}

/// Whether the preview is in the light look.
pub fn light() -> bool {
    std::env::var("GSE_PREVIEW_LOOK").is_ok_and(|look| look == "light")
}

/// Switches of the made-up machine a state sets: `dev` (the dev helper is
/// installed), `no-appimage` (the app runs from a build), `service-kept`
/// (the service is a development one the app does not carry), `gpu-moved`
/// (the settings name GPUs the app did not start for).
static FLAGS: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

/// Whether the state set `name`.
pub fn flag(name: &str) -> bool {
    FLAGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&name)
}

fn set_flag(name: &'static str) {
    FLAGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(name);
}

/// Seconds since the epoch at which every preview happens: 1 November 2026,
/// 19:30 at the machine's offset.
fn now_seconds() -> u64 {
    use chrono::TimeZone as _;
    chrono::Local
        .with_ymd_and_hms(2026, 11, 1, 19, 30, 0)
        .single()
        .map_or(0, |t| u64::try_from(t.timestamp()).unwrap_or(0))
}

/// The moment every preview happens at.
pub fn now() -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_opt(i64::try_from(now_seconds()).unwrap_or(0), 0)
        .single()
        .unwrap_or_else(chrono::Local::now)
}

/// Seconds since the epoch, `minutes` before [`now`].
fn ago(minutes: u64) -> u64 {
    now_seconds().saturating_sub(minutes * 60)
}

impl Home {
    /// In a preview, says that `what` is not done and returns true; the
    /// caller then skips it.
    pub(crate) fn preview_skips(&mut self, what: &str, cx: &mut Context<Self>) -> bool {
        if !active() {
            return false;
        }
        self.say(format!("Preview: {what} is not done"), false);
        cx.notify();
        true
    }

    /// Fills the app with the made-up machine and shows `state` on it.
    pub(crate) fn preview(&mut self, state: &str, cx: &mut Context<Self>) {
        self.connected = Some(true);
        self.service = client::Match::Same;
        self.settings = Ok((data::settings(data::SPARE_GPU), data::gpus()));
        self.draft = Some(data::settings(data::SPARE_GPU));
        self.images = vec!["launchers".into(), "desktop".into()];
        self.environments = data::environments();
        self.load_presets(client::PresetsState {
            presets: data::sets(),
            build: None,
            base: Some(data::base(true)),
            stale: Vec::new(),
            failed: Vec::new(),
        });
        self.presets.templates = vec![data::launchers(), data::desktop()];
        self.selected = Some("demo".into());
        self.journal = Some(("demo".into(), data::journal()));
        self.tour = None;
        self.setup = None;
        self.toasts.clear();
        self.storage = Some(client::Storage {
            path: "/var/lib/gse".into(),
            used: 61_000_000_000,
            free: 412_000_000_000,
        });
        self.antivirus = Some(client::Antivirus {
            engine: Some("1.5.4".into()),
            database: Some(data::at(6, 12)),
            checked: Some(data::at(6, 12)),
            error: None,
        });
        match states::STATES.iter().find(|s| s.name == state) {
            Some(found) => {
                for name in found.flags {
                    set_flag(name);
                }
                (found.show)(self, cx);
            }
            None => self.say(
                format!("No preview state {state}; GSE_PREVIEW=list lists them"),
                true,
            ),
        }
        // The app started for the GPUs the settings name, unless the state
        // shows the notice that they moved.
        self.started_for = if flag("gpu-moved") {
            Vec::new()
        } else {
            self.gpus_in_settings()
        };
        cx.notify();
    }
}

/// The states, one per line with what each shows, for `GSE_PREVIEW=list`.
pub fn list() -> String {
    states::STATES
        .iter()
        .fold(String::new(), |mut text, state| {
            use std::fmt::Write as _;
            let _ = writeln!(text, "{}\t{}", state.name, state.what);
            text
        })
}
