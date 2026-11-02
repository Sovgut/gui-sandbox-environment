//! The settings forms: which GPU an environment gets, the mode of its
//! screen and whether it reaches the internet, as rows of a group. App
//! settings edit what new sandboxes start with; a sandbox's settings panel
//! edits that sandbox's own. A change is saved as soon as it is made.

use gpui::{Context, Div, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gse_protocol::{Gpu, Request, ScreenMode, Settings, SwitchMode};

use crate::client::{self, Unanswered};
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, button};
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

pub(crate) fn gpu_title(gpu: &Gpu) -> String {
    gpu.device
        .clone()
        .unwrap_or_else(|| format!("GPU {:04x}:{:04x}", gpu.vendor_id, gpu.device_id))
}

/// A group of settings rows, with a small heading when given.
pub(crate) fn group(heading: Option<&'static str>, rows: Vec<Div>) -> Div {
    let mut group = div()
        .flex()
        .flex_col()
        .rounded(px(12.))
        .bg(Theme::group())
        .children(heading.map(|h| {
            div()
                .px_3p5()
                .pt_3()
                .pb_1()
                .text_xs()
                .text_color(Theme::muted())
                .child(h)
        }));
    for (i, row) in rows.into_iter().enumerate() {
        group = group.child(row.when(i > 0, |r| r.border_t_1().border_color(Theme::border())));
    }
    group
}

/// One row of a [`group`]: what it is, a line under it, and its control.
pub(crate) fn row(title: &'static str, hint: Option<String>, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_3p5()
        .py_3()
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(div().text_sm().child(title))
                .children(hint.map(|h| div().text_xs().text_color(Theme::muted()).child(h))),
        )
        .child(div().flex_none().child(control))
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
    /// differs; the settings panel stays open on the new name.
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
                        if let Some(Modal::SandboxSettings { name, .. }) = home.modal.as_mut()
                            && *name != to
                        {
                            name.clone_from(&to);
                            home.journal = None;
                        }
                        home.selected = Some(to);
                    }
                    Err(e) => home.say(explain(&e), true),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    /// A button showing `current` that opens a list of `options`; picking
    /// one calls `pick` with its value. One menu is open at a time, by `id`.
    pub(crate) fn menu<T: Clone + 'static>(
        &self,
        id: impl Into<SharedString>,
        current: &str,
        options: Vec<(String, T)>,
        enabled: bool,
        pick: impl Fn(&mut Self, T, &mut Context<Self>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let id: SharedString = id.into();
        let open = self.open_menu.as_ref() == Some(&id);
        let mut picker = div().relative().child(
            button(
                id.clone(),
                current.to_owned(),
                Kind::Secondary,
                enabled,
                cx.listener({
                    let id = id.clone();
                    move |home, _, _, cx| {
                        home.open_menu = if home.open_menu.as_ref() == Some(&id) {
                            None
                        } else {
                            Some(id.clone())
                        };
                        cx.notify();
                    }
                }),
            )
            .small()
            .icon(gpui_kit::component::IconName::ChevronDown),
        );
        if open {
            let mut list = div()
                .id(SharedString::from(format!("{id}-menu")))
                .absolute()
                .top(px(34.))
                .right_0()
                .min_w(px(180.))
                .max_h(px(260.))
                .overflow_y_scroll()
                .p_1()
                .rounded(px(10.))
                .bg(Theme::popover())
                .border_1()
                .border_color(Theme::line())
                .shadow_lg()
                .occlude();
            for (i, (label, value)) in options.into_iter().enumerate() {
                let pick = pick.clone();
                let chosen = label == current;
                list = list.child(
                    div()
                        .id(SharedString::from(format!("{id}-{i}")))
                        .px_2p5()
                        .py_1p5()
                        .rounded(px(6.))
                        .text_sm()
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .when(chosen, |o| {
                            o.font_weight(FontWeight::SEMIBOLD).bg(Theme::selected())
                        })
                        .hover(|o| o.bg(Theme::raised()))
                        .on_click(cx.listener(move |home, _, _, cx| {
                            home.open_menu = None;
                            pick(home, value.clone(), cx);
                            cx.notify();
                        }))
                        .child(label),
                );
            }
            picker = picker.child(gpui::deferred(list).with_priority(1));
        }
        picker
    }

    /// Changes the draft of `target` with `change` and saves it at once.
    fn change(
        &mut self,
        target: Target,
        change: impl FnOnce(&mut Settings),
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.draft_mut(target) else {
            return;
        };
        change(draft);
        match target {
            Target::Defaults => self.save_defaults(cx),
            Target::Sandbox => {
                if let Some(Modal::SandboxSettings { name, .. }) = &self.modal {
                    let name = name.clone();
                    self.save_sandbox_settings(&name, &name, cx);
                }
            }
        }
        cx.notify();
    }

    /// The graphics rows of `target`: its GPU, screen size and frame rate.
    pub(crate) fn graphics_rows(
        &self,
        target: Target,
        gpus: &[Gpu],
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = !self.saving;
        let draft = self.draft_of(target);
        let chosen = draft.and_then(|d| gpus.iter().find(|g| g.address == d.gpu));
        let gpu_hint = chosen.map(|g| match g.mode {
            SwitchMode::Single => "Takes the monitor while it runs".to_owned(),
            SwitchMode::Secondary => "Your desktop stays up while it runs".to_owned(),
        });
        let gpu_menu = self.menu(
            match target {
                Target::Defaults => "defaults-gpu",
                Target::Sandbox => "sandbox-gpu",
            },
            &chosen.map_or_else(|| "None".to_owned(), gpu_title),
            gpus.iter()
                .map(|g| (gpu_title(g), g.address.clone()))
                .collect(),
            enabled && !gpus.is_empty(),
            move |home, address: String, cx| {
                home.change(target, |d| d.gpu = address, cx);
            },
            cx,
        );
        let single = chosen.is_some_and(|g| g.mode == SwitchMode::Single);
        let mode = self.draft_mode(target);
        let mut sizes: Vec<(u32, u32)> = RESOLUTIONS.to_vec();
        if !sizes.contains(&(mode.width, mode.height)) {
            sizes.push((mode.width, mode.height));
        }
        let mut rates = RATES.to_vec();
        if !rates.contains(&mode.rate) {
            rates.push(mode.rate);
            rates.sort_unstable();
        }
        let screen: Div = if let (true, Some(gpu)) = (single, chosen) {
            div()
                .text_sm()
                .text_color(Theme::muted())
                .child(crate::shell::monitor_text(gpu))
        } else {
            self.menu(
                match target {
                    Target::Defaults => "defaults-size",
                    Target::Sandbox => "sandbox-size",
                },
                &format!("{} x {}", mode.width, mode.height),
                sizes
                    .into_iter()
                    .map(|(w, h)| (format!("{w} x {h}"), (w, h)))
                    .collect(),
                enabled,
                move |home, (width, height): (u32, u32), cx| {
                    let mode = ScreenMode {
                        width,
                        height,
                        ..home.draft_mode(target)
                    };
                    home.change(target, |d| d.screen_mode = mode.to_string(), cx);
                },
                cx,
            )
        };
        let rate: Div = if single {
            div()
                .text_sm()
                .text_color(Theme::muted())
                .child("The monitor's own")
        } else {
            self.menu(
                match target {
                    Target::Defaults => "defaults-rate",
                    Target::Sandbox => "sandbox-rate",
                },
                &format!("{} FPS", mode.rate),
                rates.into_iter().map(|r| (format!("{r} FPS"), r)).collect(),
                enabled,
                move |home, rate: u32, cx| {
                    let mode = ScreenMode {
                        rate,
                        ..home.draft_mode(target)
                    };
                    home.change(target, |d| d.screen_mode = mode.to_string(), cx);
                },
                cx,
            )
        };
        group(
            Some("Graphics"),
            vec![
                row("Graphics card", gpu_hint, gpu_menu),
                row("Screen size", None, screen),
                row("Frame rate", None, rate),
            ],
        )
    }

    /// The internet row of `target`, as a switch.
    pub(crate) fn internet_row(
        &self,
        target: Target,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let on = self.draft_of(target).is_none_or(|d| d.internet);
        row(
            "Internet",
            Some(hint.to_owned()),
            gpui_kit::component::switch::Switch::new(match target {
                Target::Defaults => "defaults-internet",
                Target::Sandbox => "sandbox-internet",
            })
            .checked(on)
            .disabled(self.saving)
            .on_change(cx.listener(move |home, on: &bool, _, cx| {
                let on = *on;
                home.change(target, |d| d.internet = on, cx);
            })),
        )
    }

    /// App settings: what new sandboxes start with.
    pub(crate) fn defaults_view(&self, cx: &mut Context<Self>) -> Div {
        let view = div().flex().flex_col().gap_4().child(
            div().text_sm().text_color(Theme::muted()).child(
                "What a sandbox gets when it is made. Each one can change these later in its \
                 own settings.",
            ),
        );
        let (_, gpus) = match &self.settings {
            Ok(loaded) => loaded,
            Err(reason) => {
                return view.child(
                    ui::notice(Theme::warning(), "Settings are unavailable", reason.clone()).child(
                        button(
                            "settings-retry",
                            "Try again",
                            Kind::Secondary,
                            true,
                            cx.listener(|_, _, _, cx| Self::reload(cx)),
                        ),
                    ),
                );
            }
        };
        let sets = self.presets_list().unwrap_or_default();
        let first = self
            .app_config
            .first_set
            .clone()
            .filter(|s| sets.contains(s))
            .or_else(|| sets.first().cloned())
            .unwrap_or_else(|| "None".to_owned());
        let set_menu = self.menu(
            "defaults-set",
            &first,
            sets.iter().map(|s| (s.clone(), s.clone())).collect(),
            !sets.is_empty(),
            |home, set: String, cx| {
                home.app_config.first_set = Some(set);
                if let Err(e) = home.app_config.save() {
                    home.say(format!("The setting was not saved: {e:#}"), true);
                }
                cx.notify();
            },
            cx,
        );
        view.child(group(
            None,
            vec![row(
                "Set of apps",
                Some("Picked first in the new sandbox dialog".to_owned()),
                set_menu,
            )],
        ))
        .child(self.graphics_rows(Target::Defaults, gpus, cx))
        .child(group(
            None,
            vec![self.internet_row(
                Target::Defaults,
                "Off is safer for programs you don't know",
                cx,
            )],
        ))
    }
}
