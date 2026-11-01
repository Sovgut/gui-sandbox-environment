//! The settings forms: which GPU an environment gets, the mode of its
//! screen and whether it reaches the internet. App settings edit what new
//! sandboxes start with; a sandbox's settings dialog edits that sandbox's
//! own. Each form edits a draft and saves it in one request.

use gpui::{Context, Div, FontWeight, IntoElement, SharedString, div, prelude::*};
use gse_protocol::{Gpu, Request, ScreenMode, Settings, SwitchMode};

use crate::client::{self, Unanswered};
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, badge, button, segment, segments, tag};
use crate::{Home, Modal};

const RESOLUTIONS: [(u32, u32); 4] = [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)];
const RATES: [u32; 5] = [30, 60, 90, 120, 144];
const FALLBACK: ScreenMode = ScreenMode {
    width: 2560,
    height: 1440,
    rate: 60,
};

/// Which settings a form edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// What new sandboxes start with.
    Defaults,
    /// The sandbox whose settings dialog is open.
    Sandbox,
}

/// Turns a failed settings request into words for the user.
pub fn explain(error: &anyhow::Error) -> SharedString {
    if error.downcast_ref::<Unanswered>().is_some() {
        "The running gse service does not know these settings yet. \
         Restart the service to change them here."
            .into()
    } else {
        format!("{error:#}").into()
    }
}

fn gpu_title(gpu: &Gpu) -> String {
    gpu.device
        .clone()
        .unwrap_or_else(|| format!("GPU {:04x}:{:04x}", gpu.vendor_id, gpu.device_id))
}

fn field(label: &'static str, control: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(Theme::muted())
                .child(label),
        )
        .child(div().flex().child(control))
}

impl Home {
    /// Takes freshly loaded settings, keeping a draft the user is editing.
    pub(crate) fn settings_loaded(&mut self, loaded: Result<(Settings, Vec<Gpu>), SharedString>) {
        let saved = self.settings.as_ref().ok().map(|(s, _)| s.clone());
        if let Ok((settings, _)) = &loaded
            && (self.draft.is_none()
                || self.draft == saved
                || self.draft.as_ref() == Some(settings))
        {
            self.draft = Some(settings.clone());
        }
        self.settings = loaded;
    }

    fn draft_of(&self, target: Target) -> Option<&Settings> {
        match target {
            Target::Defaults => self.draft.as_ref(),
            Target::Sandbox => self.sandbox_draft.as_ref(),
        }
    }

    fn draft_mut(&mut self, target: Target) -> Option<&mut Settings> {
        match target {
            Target::Defaults => self.draft.as_mut(),
            Target::Sandbox => self.sandbox_draft.as_mut(),
        }
    }

    fn draft_mode(&self, target: Target) -> ScreenMode {
        self.draft_of(target)
            .and_then(|d| d.screen_mode.parse().ok())
            .unwrap_or(FALLBACK)
    }

    fn set_draft_mode(&mut self, target: Target, mode: ScreenMode, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft_mut(target) {
            draft.screen_mode = mode.to_string();
            cx.notify();
        }
    }

    fn save_defaults(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = self.draft.clone() else {
            return;
        };
        self.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { client::command(&Request::SetSettings { settings }) })
                .await;
            this.update(cx, |home, cx| {
                home.saving = false;
                match outcome {
                    Ok(()) => home.say("New sandboxes start with these settings", false),
                    Err(e) => home.say(explain(&e), true),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Opens the settings dialog of `name`.
    pub(crate) fn open_sandbox_settings(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.environments.iter().find(|e| e.name == name).cloned() else {
            return;
        };
        self.sandbox_draft = Some(environment.settings.clone());
        let rename = cx.new(|cx| {
            let mut input = TextInput::new(name.to_owned(), cx);
            input.set_text(name.to_owned(), cx);
            input
        });
        cx.observe(&rename, |_, _, cx| cx.notify()).detach();
        self.modal = Some(Modal::SandboxSettings {
            name: name.to_owned(),
            rename,
        });
        cx.notify();
    }

    /// Saves the settings of `name`, renaming it to `to` first when that
    /// differs.
    pub(crate) fn save_sandbox_settings(&mut self, name: &str, to: &str, cx: &mut Context<Self>) {
        let Some(settings) = self.sandbox_draft.clone() else {
            return;
        };
        let rename = (to != name).then(|| Request::Rename {
            name: name.to_owned(),
            to: to.to_owned(),
        });
        let request = Request::SetEnvironmentSettings {
            name: to.to_owned(),
            settings,
        };
        let to = to.to_owned();
        self.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    if let Some(rename) = rename {
                        client::command(&rename)?;
                    }
                    client::command(&request)
                })
                .await;
            this.update(cx, |home, cx| {
                home.saving = false;
                match outcome {
                    Ok(()) => {
                        home.modal = None;
                        home.sandbox_draft = None;
                        home.selected = Some(to);
                        home.journal = None;
                    }
                    Err(e) => home.say(explain(&e), true),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    fn gpu_row(
        &self,
        target: Target,
        gpu: &Gpu,
        saved: &Settings,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.draft_of(target).is_some_and(|d| d.gpu == gpu.address);
        let address = gpu.address.clone();
        let (mode, mode_color) = match gpu.mode {
            SwitchMode::Single => ("single", Theme::warning()),
            SwitchMode::Secondary => ("secondary", Theme::good()),
        };
        let mut facts = vec![gpu.address.clone()];
        facts.push(match &gpu.driver {
            Some(driver) => format!("driver {driver}"),
            None => "no driver".into(),
        });
        facts.push(if gpu.monitor {
            "monitor connected".into()
        } else {
            "no monitor".into()
        });
        let radio = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size_4()
            .rounded_full()
            .border_2()
            .border_color(if selected {
                Theme::accent()
            } else {
                Theme::subtle()
            })
            .when(selected, |r| {
                r.child(div().size_2().rounded_full().bg(Theme::accent()))
            });
        div()
            .id(SharedString::from(format!(
                "gpu-{target:?}-{}",
                gpu.address
            )))
            .flex()
            .items_center()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .map(|row| {
                if selected {
                    row.bg(Theme::accent().opacity(0.08))
                        .border_color(Theme::accent().opacity(0.6))
                } else {
                    row.border_color(Theme::border())
                }
            })
            .when(!self.saving && !selected, |row| {
                row.cursor_pointer()
                    .hover(|s| s.bg(Theme::raised()))
                    .on_click(cx.listener(move |home, _, _, cx| {
                        if let Some(draft) = home.draft_mut(target) {
                            draft.gpu.clone_from(&address);
                            cx.notify();
                        }
                    }))
            })
            .child(radio)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(gpu_title(gpu)),
                            )
                            .when(saved.gpu == gpu.address, |line| line.child(tag("saved"))),
                    )
                    .child(
                        div().text_sm().text_color(Theme::muted()).truncate().child(
                            gpu.vendor
                                .clone()
                                .unwrap_or_else(|| format!("PCI vendor {:04x}", gpu.vendor_id)),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::subtle())
                            .child(facts.join("  |  ")),
                    ),
            )
            .child(badge(mode, mode_color))
    }

    fn gpu_card(
        &self,
        target: Target,
        saved: &Settings,
        gpus: &[Gpu],
        cx: &mut Context<Self>,
    ) -> Div {
        let mut gpu_card = ui::card(
            "Target GPU",
            Some("The GPU the sandbox gets. It leaves the host while the sandbox runs."),
        );
        if gpus.is_empty() {
            gpu_card = gpu_card.child(
                div()
                    .text_sm()
                    .text_color(Theme::subtle())
                    .child("The service found no GPU."),
            );
        }
        for gpu in gpus {
            gpu_card = gpu_card.child(self.gpu_row(target, gpu, saved, cx));
        }
        gpu_card.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_xs()
                .text_color(Theme::subtle())
                .child(
                    "single: the GPU drives the host desktop, which stops while the sandbox runs.",
                )
                .child("secondary: another GPU drives the desktop, which stays up."),
        )
    }

    fn screen_card(&self, target: Target, gpus: &[Gpu], cx: &mut Context<Self>) -> Div {
        let enabled = !self.saving;
        let mode = self.draft_mode(target);
        let single = self.draft_of(target).is_some_and(|d| {
            gpus.iter()
                .any(|g| g.address == d.gpu && g.mode == SwitchMode::Single)
        });
        let mut sizes: Vec<(u32, u32)> = RESOLUTIONS.to_vec();
        if !sizes.contains(&(mode.width, mode.height)) {
            sizes.push((mode.width, mode.height));
        }
        let mut rates = RATES.to_vec();
        if !rates.contains(&mode.rate) {
            rates.push(mode.rate);
            rates.sort_unstable();
        }

        let mut size_choice = segments();
        for (width, height) in sizes {
            size_choice = size_choice.child(segment(
                format!("size-{target:?}-{width}x{height}"),
                format!("{width} x {height}"),
                (mode.width, mode.height) == (width, height),
                enabled,
                cx.listener(move |home, _, _, cx| {
                    let mode = ScreenMode {
                        width,
                        height,
                        ..home.draft_mode(target)
                    };
                    home.set_draft_mode(target, mode, cx);
                }),
            ));
        }
        let mut rate_choice = segments();
        for rate in rates {
            rate_choice = rate_choice.child(segment(
                format!("rate-{target:?}-{rate}"),
                format!("{rate} FPS"),
                mode.rate == rate,
                enabled,
                cx.listener(move |home, _, _, cx| {
                    let mode = ScreenMode {
                        rate,
                        ..home.draft_mode(target)
                    };
                    home.set_draft_mode(target, mode, cx);
                }),
            ));
        }
        let card = ui::card(
            "Screen",
            Some(
                "The sandbox's screen in the app's window. \
                 Its refresh rate is the most frames per second the sandbox shows.",
            ),
        );
        if single {
            return card.child(div().text_sm().text_color(Theme::subtle()).child(
                "With this GPU the sandbox drives the monitor itself, \
                 at the monitor's own resolution and refresh rate.",
            ));
        }
        card.child(field("Resolution", size_choice))
            .child(field("Frame rate", rate_choice))
    }

    fn internet_card(&self, target: Target, cx: &mut Context<Self>) -> Div {
        let on = self.draft_of(target).is_none_or(|d| d.internet);
        let choice = |label: &'static str, value: bool, cx: &mut Context<Self>| {
            segment(
                format!("internet-{target:?}-{value}"),
                label,
                on == value,
                !self.saving,
                cx.listener(move |home, _, _, cx| {
                    if let Some(draft) = home.draft_mut(target) {
                        draft.internet = value;
                        cx.notify();
                    }
                }),
            )
        };
        ui::card(
            "Internet",
            Some(
                "Without it the sandbox reaches nothing outside itself. \
                 A running sandbox follows a change at once.",
            ),
        )
        .child(
            div().flex().child(
                segments()
                    .child(choice("Allowed", true, cx))
                    .child(choice("Cut off", false, cx)),
            ),
        )
    }

    /// The GPU, screen and internet cards of `target`.
    pub(crate) fn settings_form(
        &self,
        target: Target,
        saved: &Settings,
        gpus: &[Gpu],
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.gpu_card(target, saved, gpus, cx))
            .child(self.screen_card(target, gpus, cx))
            .child(self.internet_card(target, cx))
    }

    fn save_bar(&self, saved: &Settings, cx: &mut Context<Self>) -> Div {
        let dirty = self.draft.as_ref().is_some_and(|d| d != saved);
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(Theme::subtle())
                    .child(if dirty {
                        "Unsaved changes"
                    } else {
                        "No changes"
                    }),
            )
            .child(button(
                "settings-revert",
                "Revert",
                Kind::Ghost,
                dirty && !self.saving,
                cx.listener(|home, _, _, cx| {
                    home.draft = home.settings.as_ref().ok().map(|(s, _)| s.clone());
                    cx.notify();
                }),
            ))
            .child(button(
                "settings-save",
                if self.saving { "Saving..." } else { "Save" },
                Kind::Primary,
                dirty && !self.saving,
                cx.listener(|home, _, _, cx| home.save_defaults(cx)),
            ))
    }

    /// App settings: what new sandboxes start with.
    pub(crate) fn defaults_view(&self, cx: &mut Context<Self>) -> Div {
        let view = div().flex().flex_col().gap_4().child(
            div().text_sm().text_color(Theme::muted()).child(
                "A new sandbox starts with these settings; each sandbox then keeps its own.",
            ),
        );
        match &self.settings {
            Ok((saved, gpus)) => view
                .child(self.settings_form(Target::Defaults, saved, gpus, cx))
                .child(self.save_bar(saved, cx)),
            Err(reason) => view.child(
                ui::notice(Theme::warning(), "Settings are unavailable", reason.clone()).child(
                    button(
                        "settings-retry",
                        "Try again",
                        Kind::Secondary,
                        true,
                        cx.listener(|_, _, _, cx| Self::reload(cx)),
                    ),
                ),
            ),
        }
    }
}
