//! The settings forms: which GPU an environment gets, the mode of its
//! screen and whether it reaches the internet, as rows of a group. App
//! settings edit what new sandboxes start with; a sandbox's settings panel
//! edits that sandbox's own. A change is saved as soon as it is made.

use gpui::{Context, Div, FontWeight, Hsla, IntoElement, SharedString, div, prelude::*, px};
use gse_protocol::{Gpu, Request, ScreenMode, Settings, SupportLevel, SwitchMode, UsbKind};

use crate::client::{self, Unanswered};
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Size, SwitchSize, Theme, Tone, button};
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

/// The line under the chosen graphics card: how it changes hands, and how
/// well gse knows it to work when it has not been tested, in its color.
fn gpu_line(gpu: &Gpu) -> (String, Hsla) {
    let mode = match gpu.mode {
        SwitchMode::Single => "Takes the monitor while it runs",
        SwitchMode::Secondary => "Your desktop stays up while it runs",
    };
    match (gpu.support.level, &gpu.support.reason) {
        (SupportLevel::Unsupported, reason) => (
            format!(
                "Not supported: {}",
                reason.as_deref().unwrap_or("gse cannot run it")
            ),
            Theme::danger(),
        ),
        (SupportLevel::Expected, Some(reason)) => (
            format!("{mode}. Not tested yet: {reason}"),
            Theme::warning_text(),
        ),
        _ => (mode.to_owned(), Theme::muted()),
    }
}

/// What a USB device of `kind` is, for people.
const fn usb_kind_text(kind: UsbKind) -> &'static str {
    match kind {
        UsbKind::Sound => "Sound: a microphone or headset",
        UsbKind::Camera => "A camera, usually with a microphone",
        UsbKind::Gamepad => "A gamepad",
        UsbKind::Bluetooth => "A Bluetooth adapter",
        UsbKind::Storage => "A drive",
        UsbKind::Input => "A keyboard or mouse",
        UsbKind::Other => "Another USB device",
    }
}

/// What people call the guest driver `driver`.
pub(crate) fn driver_title(driver: &str) -> String {
    match driver {
        "nvidia" => "NVIDIA (proprietary)".to_owned(),
        "nouveau" => "Open (nouveau, NVK)".to_owned(),
        "amdgpu" => "AMDGPU (open)".to_owned(),
        "i915" => "i915 (open)".to_owned(),
        "xe" => "Xe (open)".to_owned(),
        other => other.to_owned(),
    }
}

/// What `gpu` is called among `gpus`: its model, and its PCI slot when
/// another card there has the same model.
pub(crate) fn gpu_title(gpu: &Gpu, gpus: &[Gpu]) -> String {
    let model = |gpu: &Gpu| {
        gpu.device
            .clone()
            .unwrap_or_else(|| format!("GPU {:04x}:{:04x}", gpu.vendor_id, gpu.device_id))
    };
    let name = model(gpu);
    if gpus
        .iter()
        .any(|other| other.address != gpu.address && model(other) == name)
    {
        let slot = gpu.address.strip_prefix("0000:").unwrap_or(&gpu.address);
        format!("{name} ({slot})")
    } else {
        name
    }
}

/// One entry of a [`Home::menu`]: its label, a quieter note after it,
/// whether it is the current one, and the value picking it hands on.
pub(crate) struct Choice<T> {
    label: String,
    note: Option<&'static str>,
    current: bool,
    value: T,
}

impl<T> Choice<T> {
    pub(crate) fn new(label: impl Into<String>, value: T) -> Self {
        Self {
            label: label.into(),
            note: None,
            current: false,
            value,
        }
    }

    /// Marks the entry as the one chosen now.
    pub(crate) const fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }

    pub(crate) const fn note(mut self, note: Option<&'static str>) -> Self {
        self.note = note;
        self
    }
}

/// How a [`Home::menu`] looks while it is closed: the value it shows, its
/// size, whether it fills the width it is given, and whether it opens.
pub(crate) struct Picker {
    id: SharedString,
    current: String,
    size: Size,
    wide: bool,
    enabled: bool,
}

impl Picker {
    /// A menu 32 tall that shows `current`.
    pub(crate) fn new(id: impl Into<SharedString>, current: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            current: current.into(),
            size: Size::Medium,
            wide: false,
            enabled: true,
        }
    }

    pub(crate) const fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// As wide as the space it is given, its value at the start and the
    /// chevron at the end: a menu in a column of a table.
    pub(crate) const fn wide(mut self) -> Self {
        self.wide = true;
        self
    }

    pub(crate) const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// How a group of settings rows looks, by where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rows {
    /// In a sandbox's settings sheet: rows 12 by 14 on the group ground.
    Sheet,
    /// On a page of app settings that lists them: rows 14 by 16 on the
    /// surface.
    Page,
    /// On a page of a few settings, each named in bold over a line that
    /// says more: rows 16 by 18 on the panel ground.
    Headlines,
}

impl Rows {
    /// The padding of a row: across, then down.
    const fn padding(self) -> (f32, f32) {
        match self {
            Self::Sheet => (14., 12.),
            Self::Page => (16., 14.),
            Self::Headlines => (18., 16.),
        }
    }

    fn ground(self) -> Hsla {
        match self {
            Self::Sheet => Theme::group(),
            Self::Page => Theme::surface(),
            Self::Headlines => Theme::panel(),
        }
    }

    fn divider(self) -> Hsla {
        match self {
            Self::Headlines => Theme::divider(),
            Self::Sheet | Self::Page => Theme::border(),
        }
    }

    /// A group of `rows` with lines between them, and a small heading over
    /// them when given.
    pub(crate) fn group(self, heading: Option<&'static str>, rows: Vec<Div>) -> Div {
        let (across, _) = self.padding();
        let mut group = div()
            .flex()
            .flex_col()
            .rounded(px(if self == Self::Headlines { 14. } else { 12. }))
            .bg(self.ground())
            .children(heading.map(|h| {
                div()
                    .px(px(across))
                    .py_3()
                    .border_b_1()
                    .border_color(self.divider())
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(h)
            }));
        for (i, row) in rows.into_iter().enumerate() {
            group = group.child(row.when(i > 0, |r| r.border_t_1().border_color(self.divider())));
        }
        group
    }

    /// One row: what it is, a line under it, and its control.
    pub(crate) fn row(
        self,
        title: impl Into<SharedString>,
        hint: Option<String>,
        control: impl IntoElement,
    ) -> Div {
        self.tinted_row(title, hint, Theme::muted(), control)
    }

    /// A [`Rows::row`] whose line under the title is in `color`, such as
    /// [`Theme::danger`] for a failure.
    pub(crate) fn tinted_row(
        self,
        title: impl Into<SharedString>,
        hint: Option<String>,
        color: Hsla,
        control: impl IntoElement,
    ) -> Div {
        let (across, down) = self.padding();
        let headline = self == Self::Headlines;
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(if headline { 20. } else { 16. }))
            .px(px(across))
            .py(px(down))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .when(headline, |t| t.font_weight(FontWeight::SEMIBOLD))
                            .child(title.into()),
                    )
                    .children(hint.map(|h| {
                        div()
                            .when(headline, |t| t.mt(px(2.)).text_size(px(13.)))
                            .when(!headline, Styled::text_xs)
                            .text_color(color)
                            .child(h)
                    })),
            )
            .child(div().flex_none().child(control))
    }

    /// A row with no title: a line of 13 px text in `color`, and its
    /// control.
    pub(crate) fn note_row(self, text: String, color: Hsla, control: impl IntoElement) -> Div {
        let (across, down) = self.padding();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px(px(across))
            .py(px(down))
            .child(
                div()
                    .min_w_0()
                    .text_size(px(13.))
                    .text_color(color)
                    .child(text),
            )
            .child(div().flex_none().child(control))
    }
}

/// A group of rows of a sandbox's settings sheet, with a small heading
/// when given.
pub(crate) fn group(heading: Option<&'static str>, rows: Vec<Div>) -> Div {
    Rows::Sheet.group(heading, rows)
}

/// The heading of a page of app settings: its title, 24 px bold, and what
/// the page is for under it.
pub(crate) fn page_title(title: &'static str, about: Option<&'static str>) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .child(
            div()
                .text_size(px(24.))
                .font_weight(FontWeight::BOLD)
                .child(title),
        )
        .children(about.map(|about| {
            div()
                .mt_1()
                .text_size(px(13.))
                .text_color(Theme::muted())
                .child(about)
        }))
}

/// How the rows of `target` look: a page's for new sandboxes, the sheet's
/// for a sandbox.
const fn rows_of(target: Target) -> Rows {
    match target {
        Target::Defaults => Rows::Page,
        Target::Sandbox => Rows::Sheet,
    }
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

    /// Whether the graphics of `target` can change now: not while a change
    /// is saved, nor while the sandbox runs, since they apply at a start.
    fn graphics_open(&self, target: Target) -> bool {
        let running = target == Target::Sandbox
            && matches!(&self.modal, Some(Modal::SandboxSettings { name, .. })
                if self.environments.iter().any(|e| e.name == *name && e.running.is_some()));
        !self.saving && !running
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

    /// A control showing what `picker` holds with a chevron after it, that
    /// opens a list of `options`, the current one checked; picking one
    /// calls `pick` with its value. One menu is open at a time, by its id.
    pub(crate) fn menu<T: Clone + 'static>(
        &self,
        picker: Picker,
        options: Vec<Choice<T>>,
        pick: impl Fn(&mut Self, T, &mut Context<Self>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let Picker {
            id,
            current,
            size,
            wide,
            enabled,
        } = picker;
        let open = self.open_menu.as_ref() == Some(&id);
        let mut picker = div().relative().when(wide, Styled::w_full).child(
            button(
                id.clone(),
                current,
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
            .size(size)
            .fill(Theme::selected())
            .dropdown()
            // The chevron sits near the end, as a browser's select has it.
            .map(|b| {
                if wide {
                    b.w_full().px(px(4.))
                } else {
                    b.pr(px(4.))
                }
            }),
        );
        if open {
            let mut list = div()
                .id(SharedString::from(format!("{id}-menu")))
                .absolute()
                .top(px(size.height() + 4.))
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
            for (i, choice) in options.into_iter().enumerate() {
                let pick = pick.clone();
                let value = choice.value;
                list = list.child(
                    ui::menu_item(
                        SharedString::from(format!("{id}-{i}")),
                        choice.label,
                        choice.note,
                        choice.current,
                    )
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.open_menu = None;
                        pick(home, value.clone(), cx);
                        cx.notify();
                    })),
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

    /// The menu of the graphics card `target` gets, `chosen` checked and
    /// the card that drives the monitor marked. A sandbox always has a
    /// card, so there is nothing to pick for none.
    fn gpu_menu(
        &self,
        target: Target,
        gpus: &[Gpu],
        chosen: Option<&Gpu>,
        cx: &mut Context<Self>,
    ) -> Div {
        let shown = match chosen {
            Some(gpu) => gpu_title(gpu, gpus),
            None if gpus.is_empty() => "No card found".to_owned(),
            None => "Choose a card".to_owned(),
        };
        self.menu(
            Picker::new(
                match target {
                    Target::Defaults => "defaults-gpu",
                    Target::Sandbox => "sandbox-gpu",
                },
                shown,
            )
            .enabled(self.graphics_open(target) && !gpus.is_empty()),
            gpus.iter()
                .map(|g| {
                    let note = if g.support.level == SupportLevel::Unsupported {
                        Some("not supported")
                    } else {
                        g.monitor.then_some("drives the monitor")
                    };
                    Choice::new(gpu_title(g, gpus), g.address.clone())
                        .current(chosen.is_some_and(|c| c.address == g.address))
                        .note(note)
                })
                .collect(),
            move |home, address: String, cx| {
                home.change(
                    target,
                    |d| {
                        d.gpu = address;
                        d.gpu_driver = None;
                    },
                    cx,
                );
            },
            cx,
        )
    }

    /// The graphics rows of `target`: its GPU, screen size and frame rate,
    /// under their heading.
    pub(crate) fn graphics_rows(
        &self,
        target: Target,
        gpus: &[Gpu],
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = self.graphics_open(target);
        let draft = self.draft_of(target);
        let chosen = draft.and_then(|d| gpus.iter().find(|g| g.address == d.gpu));
        let (gpu_line, line_color) = chosen.map_or((None, Theme::muted()), |g| {
            let (line, color) = gpu_line(g);
            (Some(line), color)
        });
        let driver_row = chosen
            .filter(|g| g.guest_drivers.len() > 1)
            .map(|g| self.driver_row(target, g, cx));
        let gpu_menu = self.gpu_menu(target, gpus, chosen, cx);
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
            div().text_size(px(13.)).text_color(Theme::muted()).child(
                crate::shell::monitor_mode(gpu)
                    .map_or_else(|| "The monitor's own".to_owned(), |(size, _)| size),
            )
        } else {
            self.menu(
                Picker::new(
                    match target {
                        Target::Defaults => "defaults-size",
                        Target::Sandbox => "sandbox-size",
                    },
                    format!("{} x {}", mode.width, mode.height),
                )
                .enabled(enabled),
                sizes
                    .into_iter()
                    .map(|(w, h)| {
                        Choice::new(format!("{w} x {h}"), (w, h))
                            .current((w, h) == (mode.width, mode.height))
                    })
                    .collect(),
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
            div().text_size(px(13.)).text_color(Theme::muted()).child(
                chosen
                    .and_then(crate::shell::monitor_mode)
                    .map_or_else(|| "The monitor's own".to_owned(), |(_, rate)| rate),
            )
        } else {
            self.menu(
                Picker::new(
                    match target {
                        Target::Defaults => "defaults-rate",
                        Target::Sandbox => "sandbox-rate",
                    },
                    format!("{} FPS", mode.rate),
                )
                .enabled(enabled),
                rates
                    .into_iter()
                    .map(|r| Choice::new(format!("{r} FPS"), r).current(r == mode.rate))
                    .collect(),
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
        let rows = rows_of(target);
        let mut group = vec![rows.tinted_row("Graphics card", gpu_line, line_color, gpu_menu)];
        group.extend(driver_row);
        group.push(rows.row("Screen size", None, screen));
        group.push(rows.row("Frame rate", None, rate));
        rows.group(Some("Graphics"), group)
    }

    /// The row choosing the driver the sandbox runs `gpu` with, among the
    /// ones it can run on; none stored means its default, the first.
    fn driver_row(&self, target: Target, gpu: &Gpu, cx: &mut Context<Self>) -> Div {
        let default = gpu.guest_drivers.first().cloned().unwrap_or_default();
        let current = self
            .draft_of(target)
            .and_then(|d| d.gpu_driver.clone())
            .filter(|d| gpu.guest_drivers.contains(d))
            .unwrap_or_else(|| default.clone());
        let menu = self.menu(
            Picker::new(
                match target {
                    Target::Defaults => "defaults-gpu-driver",
                    Target::Sandbox => "sandbox-gpu-driver",
                },
                driver_title(&current),
            )
            .enabled(self.graphics_open(target)),
            gpu.guest_drivers
                .iter()
                .map(|d| {
                    Choice::new(driver_title(d), d.clone())
                        .current(*d == current)
                        .note((*d == default).then_some("default"))
                })
                .collect(),
            move |home, driver: String, cx| {
                let stored = (driver != default).then_some(driver);
                home.change(target, |d| d.gpu_driver = stored, cx);
            },
            cx,
        );
        rows_of(target).row(
            "Driver in the sandbox",
            Some("Loaded at each start; one the base image lacks has it rebuilt first".to_owned()),
            menu,
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
        rows_of(target).row(
            "Internet",
            Some(hint.to_owned()),
            ui::switch(
                match target {
                    Target::Defaults => "defaults-internet",
                    Target::Sandbox => "sandbox-internet",
                },
                on,
                SwitchSize::Row,
            )
            .disabled(self.saving)
            .on_change(cx.listener(move |home, on: &bool, _, cx| {
                let on = *on;
                home.change(target, |d| d.internet = on, cx);
            })),
        )
    }

    /// The sandbox's devices: the board's sound card and each USB device
    /// plugged into the host, with a switch that says whether the sandbox
    /// takes it while it runs. The defaults follow its GPU's mode: on a
    /// single GPU the host session is gone meanwhile, so the sound card and
    /// USB sound devices, cameras and gamepads go; with another GPU nothing
    /// does. None when the host has nothing to take.
    pub(crate) fn devices_rows(&self, gpus: &[Gpu], cx: &mut Context<Self>) -> Option<Div> {
        let target = Target::Sandbox;
        let draft = self.draft_of(target)?;
        let single = gpus
            .iter()
            .find(|g| g.address == draft.gpu)
            .is_some_and(|g| g.mode == SwitchMode::Single);
        let open = self.graphics_open(target);
        let rows = rows_of(target);
        let mut list = Vec::new();
        if self.host_devices.board_sound {
            let on = draft.devices.takes_board_sound(single);
            list.push(
                rows.row(
                    "Sound card on the board",
                    Some(
                        "Its 3.5 mm jacks: speakers, headphones and a microphone; the host has no \
                     sound from them meanwhile"
                            .to_owned(),
                    ),
                    ui::switch("sandbox-board-sound", on, SwitchSize::Row)
                        .disabled(!open)
                        .on_change(cx.listener(move |home, on: &bool, _, cx| {
                            let on = *on;
                            home.change(
                                target,
                                |d| d.devices.board_sound = (on != single).then_some(on),
                                cx,
                            );
                        })),
                ),
            );
        }
        for (i, device) in self
            .host_devices
            .usb
            .iter()
            .filter(|d| d.kind.offered())
            .enumerate()
        {
            let on = draft.devices.takes_usb(&device.id, device.kind, single);
            let (id, kind) = (device.id.clone(), device.kind);
            let what = usb_kind_text(device.kind);
            let hint = match device.kind {
                UsbKind::Storage => {
                    format!("{what}. Programs in the sandbox can read and change everything on it")
                }
                UsbKind::Bluetooth => {
                    format!("{what}. The base image gets Bluetooth at its next build")
                }
                _ => what.to_owned(),
            };
            list.push(
                rows.row(
                    device.label.clone(),
                    Some(hint),
                    ui::switch(
                        SharedString::from(format!("sandbox-usb-{i}")),
                        on,
                        SwitchSize::Row,
                    )
                    .disabled(!open)
                    .on_change(cx.listener(move |home, on: &bool, _, cx| {
                        let on = *on;
                        home.change(target, |d| d.devices.choose_usb(&id, kind, on, single), cx);
                    })),
                ),
            );
        }
        list.extend(self.other_devices_rows(rows, cx));
        if list.is_empty() {
            return None;
        }
        list.push(rows.note_row(
            if single {
                "Keyboards and mice reach the sandbox anyway and stay the way out.".to_owned()
            } else {
                "Keyboards and mice stay with the host; the sandbox gets them in its window."
                    .to_owned()
            },
            Theme::muted(),
            div(),
        ));
        Some(rows.group(Some("Devices"), list))
    }

    /// The USB devices of no known kind: a line that shows or hides them,
    /// and, shown, each with its IDs and a switch that stays off.
    fn other_devices_rows(&self, rows: Rows, cx: &mut Context<Self>) -> Vec<Div> {
        let mut found = Vec::new();
        let others: Vec<&gse_protocol::HostUsb> = self
            .host_devices
            .usb
            .iter()
            .filter(|d| d.kind == UsbKind::Other)
            .collect();
        if !others.is_empty() {
            let shown = self.show_other_devices;
            found.push(
                rows.note_row(
                    format!(
                        "{} more USB device(s) of a kind gse does not know yet; a sandbox cannot \
                     take them",
                        others.len()
                    ),
                    Theme::muted(),
                    button(
                        "sandbox-usb-others",
                        if shown { "Hide" } else { "Show other devices" },
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, _, cx| {
                            home.show_other_devices = !home.show_other_devices;
                            cx.notify();
                        }),
                    )
                    .size(Size::Toolbar),
                ),
            );
            if shown {
                for (i, device) in others.iter().enumerate() {
                    found.push(
                        rows.row(
                            device.label.clone(),
                            Some(format!(
                                "USB ID {:04x}:{:04x}: name it when asking for its kind",
                                device.id.vendor, device.id.product
                            )),
                            ui::switch(
                                SharedString::from(format!("sandbox-usb-other-{i}")),
                                false,
                                SwitchSize::Row,
                            )
                            .disabled(true),
                        ),
                    );
                }
            }
        }
        found
    }

    /// App settings: what new sandboxes start with.
    pub(crate) fn defaults_view(&self, cx: &mut Context<Self>) -> Div {
        let header = div()
            .flex()
            .items_end()
            .justify_between()
            .gap_4()
            .child(page_title(
                "New sandboxes",
                Some(
                    "What a sandbox gets when it is made. Each one can change these later in \
                     its own settings.",
                ),
            ))
            // The rows wait while a change is saved; this says why.
            .when(self.saving, |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(px(13.))
                        .text_color(Theme::accent())
                        .child("Saving..."),
                )
            });
        let view = div().flex().flex_col().gap(px(18.)).child(header);
        let (_, gpus) = match &self.settings {
            Ok(loaded) => loaded,
            Err(reason) => {
                return view.child(
                    ui::notice(Tone::Warning, "Settings are unavailable", reason.clone()).child(
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
            .or_else(|| sets.first().cloned());
        let set_menu = self.menu(
            Picker::new(
                "defaults-set",
                first
                    .as_deref()
                    .map_or_else(|| "No sets yet".to_owned(), crate::presets::set_title),
            )
            .enabled(!sets.is_empty()),
            sets.iter()
                .map(|s| {
                    Choice::new(crate::presets::set_title(s), s.clone())
                        .current(first.as_ref() == Some(s))
                })
                .collect(),
            |home, set: String, cx| {
                home.app_config.first_set = Some(set);
                if let Err(e) = home.app_config.save() {
                    home.say(format!("The setting was not saved: {e:#}"), true);
                }
                cx.notify();
            },
            cx,
        );
        view.child(Rows::Page.group(
            None,
            vec![Rows::Page.row(
                "Set of apps",
                Some("Picked first in the new sandbox dialog".to_owned()),
                set_menu,
            )],
        ))
        .child(self.graphics_rows(Target::Defaults, gpus, cx))
        .child(Rows::Page.group(
            None,
            vec![self.internet_row(
                Target::Defaults,
                "Off is safer for programs you don't know",
                cx,
            )],
        ))
    }
}
