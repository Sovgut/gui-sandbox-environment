//! The gse app: the sandboxes in a list on the left, the selected one's
//! state, start and journal on the right, and app settings with the presets
//! and what new sandboxes start with. Everything goes through the gse
//! service; the app itself needs no privileges.

mod client;
mod config;
mod gpu;
mod icons;
mod input;
mod journal;
mod presets;
mod programs;
mod sandbox;
mod screen;
mod settings;
mod text_input;
mod transfer;
mod ui;
mod update;

use std::collections::BTreeMap;

use futures::StreamExt as _;
use gpui::{
    App, Application, Bounds, Context, Div, Entity, FontWeight, Hsla, IntoElement, ParentElement,
    Render, SharedString, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use gse_protocol::{
    Environment, Event, Gpu, GuestApp, GuestFiles, JournalEntry, Request, Settings, Stage,
};

use crate::icons::Icon;
use crate::text_input::TextInput;
use crate::ui::{Kind, Theme, badge, button, icon_button, segment, segments};

fn stage_text(stage: Stage) -> &'static str {
    match stage {
        Stage::Preparing => "Preparing",
        Stage::SwitchingGpu => "Switching the GPU",
        Stage::Booting => "Booting",
        Stage::Ready => "Running",
        Stage::Stopping => "Stopping",
    }
}

fn stage_color(stage: Stage) -> Hsla {
    match stage {
        Stage::Ready => Theme::good(),
        _ => Theme::warning(),
    }
}

/// What the right side shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// The selected sandbox.
    Sandbox,
    AppSettings,
}

/// The order of the sandbox list; the sort button steps through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Created,
    Name,
    Preset,
}

impl SortBy {
    const fn next(self) -> Self {
        match self {
            Self::Created => Self::Name,
            Self::Name => Self::Preset,
            Self::Preset => Self::Created,
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Created => Icon::SortNewest,
            Self::Name => Icon::SortName,
            Self::Preset => Icon::SortPreset,
        }
    }

    const fn text(self) -> &'static str {
        match self {
            Self::Created => "newest first",
            Self::Name => "by name",
            Self::Preset => "by preset",
        }
    }
}

/// Orders names with numbers in them by the numbers' values, so `x-2`
/// comes before `x-10`.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(s: &str) -> Vec<(bool, String)> {
        let mut parts: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match parts.last_mut() {
                Some((d, part)) if *d == digit => part.push(c),
                _ => parts.push((digit, c.to_string())),
            }
        }
        parts
    }
    let (a, b) = (parts(a), parts(b));
    for (x, y) in a.iter().zip(&b) {
        let order = match (x, y) {
            ((true, x), (true, y)) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
            ((_, x), (_, y)) => x.cmp(y),
        };
        if order.is_ne() {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// The parts of app settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppTab {
    General,
    Presets,
    Defaults,
}

/// A destructive action waiting for a second click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Reset,
    Delete,
}

/// A dialog over the window.
enum Modal {
    /// Adding a sandbox: the chosen preset and the new sandbox's name.
    Add {
        preset: Option<String>,
        name: Entity<TextInput>,
    },
    /// A sandbox's settings, with its name as edited.
    SandboxSettings {
        name: String,
        rename: Entity<TextInput>,
    },
    /// Moving files in and out of the running sandbox `name`.
    Transfer { name: String },
    /// The window is closing while a sandbox runs.
    Close,
}

/// A message for the user: an error stays until dismissed.
#[derive(Debug, Clone)]
struct Status {
    text: SharedString,
    error: bool,
}

/// The app's one window. Its flags are separate facts of the view, not
/// one state spread over several.
#[allow(clippy::struct_excessive_bools)]
struct Home {
    page: Page,
    app_tab: AppTab,
    /// The sandbox the right side shows.
    selected: Option<String>,
    environments: Vec<Environment>,
    images: Vec<String>,
    /// Whether the last request reached the service; `None` before the first.
    connected: Option<bool>,
    /// Requests in flight by key (an environment's name, or the name and
    /// what is done to it), with what the request does; their buttons wait.
    busy: BTreeMap<String, &'static str>,
    confirm: Option<(String, Confirm)>,
    status: Option<Status>,
    /// What new sandboxes start with and the machine's GPUs, or why they are
    /// missing.
    settings: Result<(Settings, Vec<Gpu>), SharedString>,
    /// What new sandboxes start with, as edited in app settings.
    draft: Option<Settings>,
    /// The settings of the sandbox whose settings dialog is open, as edited.
    sandbox_draft: Option<Settings>,
    saving: bool,
    /// The GPUs the app stayed off when it started.
    started_for: Vec<String>,
    /// How the service's protocol compares with the app's.
    service: client::Match,
    restarting: bool,
    presets: presets::Presets,
    /// The journal of the selected sandbox, and whose.
    journal: Option<(String, Vec<JournalEntry>)>,
    journal_filter: Entity<TextInput>,
    processes_filter: Entity<TextInput>,
    sandbox_search: Entity<TextInput>,
    sort: SortBy,
    /// The programs of the selected sandbox while it runs, and whose.
    processes: Option<(String, Result<Vec<GuestApp>, SharedString>)>,
    processes_loading: bool,
    processes_scroll: gpui::ScrollHandle,
    journal_scroll: gpui::ScrollHandle,
    /// The Inbox and Outbox of the sandbox whose settings are open.
    files: Option<(String, Result<GuestFiles, SharedString>)>,
    /// What the last move of files did.
    moved: Option<transfer::Moved>,
    app_config: config::AppConfig,
    update: update::State,
    /// "Remove everything" waits for its second click.
    remove_confirm: bool,
    /// Whether removing everything takes the received files folder too.
    remove_received: bool,
    /// The received files folder as edited in app settings.
    received_input: Entity<TextInput>,
    modal: Option<Modal>,
    /// The user chose to close the window although a sandbox runs.
    closing: bool,
    /// The environment screen shown in place of everything else, and whose.
    screen: Option<(String, Entity<screen::Screen>)>,
    /// An environment whose screen opens once it is ready.
    open_when_ready: Option<String>,
    /// Failed attempts at opening the screen in a row.
    screen_attempts: u32,
}

impl Home {
    fn new(cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        client::watch(tx);
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let alive = this.update(cx, |home, cx| {
                    // The journal grows line by line; nothing else changed.
                    if let Some(Event::Journal { name, entry }) = &event {
                        home.journal_event(name, entry);
                        cx.notify();
                        return;
                    }
                    if let Some(event) = &event {
                        home.note(event);
                    }
                    Self::reload(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        let processes_filter =
            cx.new(|cx| TextInput::new("Regex: program, process, command, PID", cx));
        cx.observe(&processes_filter, |_, _, cx| cx.notify())
            .detach();
        let journal_filter =
            cx.new(|cx| TextInput::new("Regex, @14:05, @14:00..14:30, @2026-11-01", cx));
        cx.observe(&journal_filter, |_, _, cx| cx.notify()).detach();
        let app_config = config::AppConfig::load();
        let received_input = cx.new(|cx| {
            let mut input = TextInput::new("~/GSE", cx);
            input.set_text(app_config.received.display().to_string(), cx);
            input
        });
        cx.observe(&received_input, |_, _, cx| cx.notify()).detach();
        let sandbox_search = cx.new(|cx| TextInput::new("Search sandboxes", cx));
        cx.observe(&sandbox_search, |_, _, cx| cx.notify()).detach();
        cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, Self::check_updates).is_err() {
                    break;
                }
                cx.background_executor().timer(update::CHECK_EVERY).await;
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(programs::PROCESS_POLL).await;
                if this.update(cx, Home::poll_processes).is_err() {
                    break;
                }
            }
        })
        .detach();
        let home = Self {
            page: Page::Sandbox,
            app_tab: AppTab::Presets,
            selected: None,
            environments: Vec::new(),
            images: Vec::new(),
            connected: None,
            busy: BTreeMap::new(),
            confirm: None,
            status: None,
            settings: Err("Loading the settings...".into()),
            draft: None,
            sandbox_draft: None,
            saving: false,
            started_for: gpu::reserved_gpus(),
            service: client::Match::Same,
            restarting: false,
            presets: presets::Presets::default(),
            journal: None,
            journal_filter,
            processes_filter,
            sandbox_search,
            sort: SortBy::Created,
            processes: None,
            processes_loading: false,
            processes_scroll: gpui::ScrollHandle::new(),
            journal_scroll: gpui::ScrollHandle::new(),
            files: None,
            moved: None,
            app_config,
            update: update::State::Idle,
            remove_confirm: false,
            remove_received: false,
            received_input,
            modal: None,
            closing: false,
            screen: None,
            open_when_ready: None,
            screen_attempts: 0,
        };
        Self::reload(cx);
        home
    }

    fn say(&mut self, text: impl Into<SharedString>, error: bool) {
        self.status = Some(Status {
            text: text.into(),
            error,
        });
    }

    /// Puts an event worth telling into the status line.
    fn note(&mut self, event: &Event) {
        match event {
            Event::Stopped {
                name,
                error: Some(error),
            } => self.say(format!("{name} stopped: {error}"), true),
            Event::Stopped { name, error: None } => self.say(format!("{name} stopped"), false),
            Event::Progress { name, stage } => {
                self.say(format!("{name}: {}", stage_text(*stage)), false);
            }
            Event::Internet { on } => self.say(
                if *on {
                    "Internet allowed"
                } else {
                    "Internet cut off"
                },
                false,
            ),
            Event::Build { .. } | Event::BuildFinished { .. } => self.preset_event(event),
            Event::Changed
            | Event::SettingsChanged
            | Event::PresetsChanged
            | Event::Journal { .. } => {}
        }
    }

    fn reload(cx: &mut Context<Self>) {
        Self::reload_presets(cx);
        cx.spawn(async move |this, cx| {
            let (loaded, settings, service) = cx
                .background_executor()
                .spawn(async {
                    let loaded = client::environments().and_then(|e| Ok((e, client::images()?)));
                    (loaded, client::settings(), client::hello())
                })
                .await;
            this.update(cx, |home, cx| {
                if let Ok(service) = service {
                    home.service = service;
                    home.restarting &= service != client::Match::Same;
                }
                match loaded {
                    Ok((environments, images)) => {
                        // The error that said the service was gone is stale.
                        if home.connected == Some(false)
                            && home.status.as_ref().is_some_and(|s| s.error)
                        {
                            home.status = None;
                        }
                        home.environments = environments;
                        home.images = images;
                        home.connected = Some(true);
                        home.keep_selection(cx);
                    }
                    Err(e) => {
                        home.connected = Some(false);
                        home.say(format!("{e:#}"), true);
                    }
                }
                home.settings_loaded(settings.map_err(|e| settings::explain(&e)));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs `work` off the UI thread under `key`, then reloads; a failure
    /// goes into the status line.
    fn run(
        &mut self,
        key: String,
        doing: &'static str,
        work: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.busy.insert(key.clone(), doing);
        self.confirm = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { work() }).await;
            this.update(cx, |home, cx| {
                home.busy.remove(&key);
                if let Err(e) = outcome {
                    let name = key.split('/').next().unwrap_or_default();
                    home.say(format!("{name}: {e:#}"), true);
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    fn request(
        &mut self,
        key: String,
        doing: &'static str,
        request: Request,
        cx: &mut Context<Self>,
    ) {
        self.run(key, doing, move || client::command(&request), cx);
    }

    fn start(&mut self, name: &str, cx: &mut Context<Self>) {
        self.open_when_ready = Some(name.to_owned());
        let owned = name.to_owned();
        self.run(
            name.to_owned(),
            "Starting",
            move || client::start(&owned, |_| {}),
            cx,
        );
    }

    fn any_running(&self) -> bool {
        self.environments.iter().any(|e| e.running.is_some())
    }

    /// The GPUs the settings name now, as [`gpu::reserved_gpus`] lists them.
    fn gpus_in_settings(&self) -> Vec<String> {
        let mut gpus: Vec<String> = self
            .environments
            .iter()
            .map(|e| e.settings.gpu.clone())
            .chain(self.settings.as_ref().ok().map(|(s, _)| s.gpu.clone()))
            .collect();
        gpus.sort();
        gpus.dedup();
        gpus
    }

    /// Shows the screen of `name`, which must be running with its screen
    /// on the host.
    fn open_screen(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let access = self
            .environments
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| e.running.as_ref())
            .and_then(|r| r.screen.clone());
        let Some(access) = access else {
            return;
        };
        match screen::Connection::open(&access) {
            Ok(connection) => {
                let entity = cx.new(|cx| screen::Screen::new(connection, cx));
                entity.update(cx, |screen, _| screen.focus(window));
                self.screen = Some((name.to_owned(), entity));
                self.screen_attempts = 0;
            }
            // Right after a start the service may not have handed the input
            // socket over yet; try again for a few seconds before telling.
            Err(_) if self.screen_attempts < 6 => {
                self.screen_attempts += 1;
                let name = name.to_owned();
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    this.update_in(cx, |home, window, cx| home.open_screen(&name, window, cx))
                        .ok();
                })
                .detach();
            }
            Err(e) => {
                self.screen_attempts = 0;
                self.say(format!("{name}: the screen did not open: {e:#}"), true);
            }
        }
        cx.notify();
    }

    /// Leaves the screen: keys go back to the app and the pointer is free.
    fn close_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen.take().is_some() {
            window
                .set_raw_key_handler(None::<fn(&gpui::RawKeyEvent, &mut Window, &mut App) -> bool>);
            window.set_pointer_lock(false);
            if window.is_fullscreen() {
                window.toggle_fullscreen();
            }
        }
        cx.notify();
    }

    /// The screen view: a slim bar over the environment's screen.
    fn screen_view(
        &self,
        name: &str,
        screen: &Entity<screen::Screen>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let captured = screen.read(cx).captured();
        let full = window.is_fullscreen();
        let stop_key = format!("{name}/stop");
        let stopping = self.busy.contains_key(&stop_key);
        let bar = div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(Theme::surface())
            .border_b_1()
            .border_color(Theme::border())
            .child(button(
                "screen-back",
                "Back",
                Kind::Ghost,
                true,
                cx.listener(|home, _, window, cx| home.close_screen(window, cx)),
            ))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name.to_owned()),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child(if captured {
                        "Mouse captured. Ctrl+Alt+G releases it."
                    } else {
                        "Click into the screen to type. Ctrl+Alt+G captures the mouse for games."
                    }),
            )
            .child(button(
                "screen-full",
                if full {
                    "Leave full screen"
                } else {
                    "Full screen"
                },
                Kind::Secondary,
                true,
                cx.listener(|_, _, window, _| window.toggle_fullscreen()),
            ))
            .child(button(
                "screen-stop",
                if stopping { "Stopping..." } else { "Stop" },
                Kind::Danger,
                !stopping,
                cx.listener(move |home, _, _, cx| {
                    home.request(stop_key.clone(), "Stopping", Request::Stop, cx);
                }),
            ));
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(bar)
            .child(div().flex_1().min_h_0().child(screen.clone()))
    }

    /// Whether the window may close now; with a sandbox running, the close
    /// dialog asks first.
    fn should_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.closing || !self.any_running() {
            return true;
        }
        self.modal = Some(Modal::Close);
        cx.notify();
        false
    }

    /// The state badge of a sandbox in the list and its pane.
    /// What a sandbox is doing, in words and as a colour.
    fn state(&self, environment: &Environment) -> (&'static str, Hsla) {
        let doing = self.busy.get(&environment.name).copied();
        match (&environment.running, doing) {
            (Some(running), _) => (stage_text(running.stage), stage_color(running.stage)),
            (None, Some(doing)) => (doing, Theme::accent()),
            (None, None) if !environment.image_ready => {
                if self.building(&environment.image) {
                    ("Image building", Theme::warning())
                } else {
                    ("No image", Theme::subtle())
                }
            }
            (None, None) => ("Stopped", Theme::subtle()),
        }
    }

    /// The state of a sandbox as a badge.
    fn state_badge(&self, environment: &Environment) -> Div {
        let (text, color) = self.state(environment);
        badge(text, color)
    }

    fn sidebar_row(&self, environment: &Environment, cx: &mut Context<Self>) -> impl IntoElement {
        let name = environment.name.clone();
        let selected =
            self.page == Page::Sandbox && self.selected.as_deref() == Some(name.as_str());
        div()
            .id(SharedString::from(format!("row-{name}")))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .map(|row| {
                if selected {
                    row.bg(Theme::accent().opacity(0.14))
                } else {
                    row.hover(|s| s.bg(Theme::raised()))
                }
            })
            .on_click(cx.listener(move |home, _, _, cx| home.select(&name, cx)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(environment.name.clone()),
            )
            .children({
                // A sandbox doing nothing shows no dot.
                let (text, color) = self.state(environment);
                let idle = environment.running.is_none()
                    && !self.busy.contains_key(&environment.name)
                    && !self.building(&environment.image);
                (!idle).then(|| {
                    div()
                        .id(SharedString::from(format!("state-{}", environment.name)))
                        .flex_none()
                        .p_1()
                        .child(div().size_2().rounded_full().bg(color))
                        .tooltip(ui::tooltip(text.into()))
                })
            })
    }

    /// The sandboxes the search finds, in the chosen order.
    fn listed(&self, cx: &Context<Self>) -> Vec<&Environment> {
        let search = self.sandbox_search.read(cx).text().trim().to_lowercase();
        let mut listed: Vec<&Environment> = self
            .environments
            .iter()
            .filter(|e| {
                search.is_empty()
                    || e.name.to_lowercase().contains(&search)
                    || e.image.to_lowercase().contains(&search)
            })
            .collect();
        match self.sort {
            SortBy::Created => listed.sort_by(|a, b| {
                b.created
                    .cmp(&a.created)
                    .then_with(|| natural(&a.name, &b.name))
            }),
            SortBy::Name => listed.sort_by(|a, b| natural(&a.name, &b.name)),
            SortBy::Preset => listed.sort_by(|a, b| {
                natural(&a.image, &b.image).then_with(|| natural(&a.name, &b.name))
            }),
        }
        listed
    }

    /// The search field and the sort button above the list.
    fn search_bar(&self, cx: &mut Context<Self>) -> Div {
        let sort = self.sort;
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(Theme::border())
            .child(div().flex_1().min_w_0().child(self.sandbox_search.clone()))
            .child(icon_button(
                "sort",
                sort.icon(),
                format!("Sorted {}; click for {}", sort.text(), sort.next().text()),
                Kind::Ghost,
                true,
                cx.listener(move |home, _, _, cx| {
                    home.sort = sort.next();
                    cx.notify();
                }),
            ))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let search = self.search_bar(cx);
        let mut list = div()
            .id("sandboxes")
            .flex()
            .flex_col()
            .flex_1()
            .gap_0p5()
            .p_1()
            .overflow_y_scroll();
        if self.connected.is_none() {
            list = list.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("Loading..."),
            );
        } else if self.environments.is_empty() && self.connected == Some(true) {
            list = list.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("No sandboxes yet."),
            );
        }
        let listed = self.listed(cx);
        if listed.is_empty() && !self.environments.is_empty() {
            list = list.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("No sandbox matches."),
            );
        }
        for environment in listed {
            list = list.child(self.sidebar_row(environment, cx));
        }
        let settings_open = self.page == Page::AppSettings;
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(260.))
            .h_full()
            .bg(Theme::surface())
            .border_r_1()
            .border_color(Theme::border())
            .child(search)
            .child(list)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(Theme::border())
                    .child(
                        button(
                            "add-sandbox",
                            "Add sandbox",
                            Kind::Primary,
                            self.connected == Some(true),
                            cx.listener(|home, _, _, cx| home.open_add(cx)),
                        )
                        .flex_1(),
                    )
                    .child(icon_button(
                        "app-settings",
                        Icon::Settings,
                        "App settings: presets and what new sandboxes start with",
                        if settings_open {
                            Kind::Primary
                        } else {
                            Kind::Secondary
                        },
                        true,
                        cx.listener(|home, _, _, cx| {
                            home.page = Page::AppSettings;
                            home.confirm = None;
                            cx.notify();
                        }),
                    )),
            )
    }

    fn app_settings_view(&self, cx: &mut Context<Self>) -> Div {
        let tab = |id: &'static str, label: &'static str, tab: AppTab, cx: &mut Context<Self>| {
            segment(
                id,
                label,
                self.app_tab == tab,
                true,
                cx.listener(move |home, _, _, cx| {
                    home.app_tab = tab;
                    cx.notify();
                }),
            )
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("App settings"),
                    )
                    .child(
                        segments()
                            .child(tab("tab-presets", "Presets", AppTab::Presets, cx))
                            .child(tab("tab-defaults", "New sandboxes", AppTab::Defaults, cx))
                            .child(tab("tab-general", "General", AppTab::General, cx)),
                    ),
            )
            .child(match self.app_tab {
                AppTab::Presets => self.presets_view(cx),
                AppTab::Defaults => self.defaults_view(cx),
                AppTab::General => self.general_view(cx),
            })
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let (dot, connection) = match self.connected {
            None => (Theme::subtle(), "Connecting to the service..."),
            Some(true) => (Theme::good(), "Service connected"),
            Some(false) => (Theme::danger(), "Service unavailable"),
        };
        let message = self.status.as_ref().filter(|s| !s.error).map(|status| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .text_color(Theme::muted())
                        .truncate()
                        .child(status.text.clone()),
                )
                .child(button(
                    "dismiss-message",
                    "Clear",
                    Kind::Ghost,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.status = None;
                        cx.notify();
                    }),
                ))
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .h(px(36.))
            .px_4()
            .bg(Theme::surface())
            .border_t_1()
            .border_color(Theme::border())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child(div().size_2().rounded_full().bg(dot))
                    .child(connection),
            )
            .children(message)
    }

    /// Notices above the right side: the service's version, an error from
    /// the last request, a GPU change the app has to restart for.
    fn notices(&self, cx: &mut Context<Self>) -> Option<Div> {
        let mut notices = div().flex().flex_col().gap_3();
        let mut any = false;
        if let Some(update) = self.update_notice(cx) {
            any = true;
            notices = notices.child(update);
        }
        match self.service {
            client::Match::Same => {}
            client::Match::ServiceNewer => {
                any = true;
                notices = notices.child(ui::notice(
                    Theme::warning(),
                    "This app is out of date",
                    "The gse service is newer than this app. Install the current version of the app.",
                ));
            }
            client::Match::ServiceOlder => {
                any = true;
                notices = notices.child(
                    ui::notice(
                        Theme::warning(),
                        "The service needs a restart",
                        "The gse service still runs an older version. Restarting it loads the \
                         current one; a running sandbox and an image build are stopped first.",
                    )
                    .child(button(
                        "restart-service",
                        if self.restarting {
                            "Restarting..."
                        } else {
                            "Restart service"
                        },
                        Kind::Primary,
                        !self.restarting,
                        cx.listener(|home, _, _, cx| {
                            home.restarting = true;
                            home.request("service".into(), "", Request::Restart, cx);
                        }),
                    )),
                );
            }
        }
        if let Some(status) = self.status.as_ref().filter(|s| s.error) {
            any = true;
            notices = notices.child(
                ui::notice(Theme::danger(), "Something went wrong", status.text.clone()).child(
                    button(
                        "dismiss-error",
                        "Dismiss",
                        Kind::Ghost,
                        true,
                        cx.listener(|home, _, _, cx| {
                            home.status = None;
                            cx.notify();
                        }),
                    ),
                ),
            );
        }
        if self.connected == Some(true)
            && self.settings.is_ok()
            && self.gpus_in_settings() != self.started_for
        {
            any = true;
            notices = notices.child(
                ui::notice(
                    Theme::accent(),
                    "Restart the app to apply the GPU change",
                    "The app keeps its own drawing off the GPUs sandboxes get \
                     and picks them when it starts.",
                )
                .child(button(
                    "restart-app",
                    "Restart now",
                    Kind::Primary,
                    !self.any_running(),
                    cx.listener(|_, _, _, cx| cx.restart()),
                )),
            );
        }
        any.then_some(notices)
    }
}

impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = |home: &Self, name: &str| {
            home.environments.iter().any(|e| {
                e.name == name
                    && e.running
                        .as_ref()
                        .is_some_and(|r| r.stage == Stage::Ready && r.screen.is_some())
            })
        };
        if let Some(name) = self.open_when_ready.clone()
            && ready(self, &name)
        {
            self.open_when_ready = None;
            self.open_screen(&name, window, cx);
        }
        if let Some((name, _)) = &self.screen
            && !ready(self, &name.clone())
        {
            self.close_screen(window, cx);
        }
        let root = div()
            .relative()
            .size_full()
            .bg(Theme::background())
            .text_color(Theme::text());
        if let Some((name, screen)) = self.screen.clone() {
            return root
                .child(self.screen_view(&name, &screen, window, cx))
                .children(self.modal_view(cx));
        }
        let content = match self.page {
            Page::AppSettings => div()
                .id("app-settings-page")
                .flex_1()
                .min_w_0()
                .overflow_y_scroll()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .w_full()
                        .max_w(px(920.))
                        .mx_auto()
                        .px_4()
                        .py_4()
                        .children(self.notices(cx))
                        .child(self.app_settings_view(cx)),
                )
                .into_any_element(),
            Page::Sandbox => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_3()
                .px_4()
                .pt_3()
                .pb_3()
                .children(self.notices(cx))
                .child(self.sandbox_pane(cx))
                .into_any_element(),
        };
        root.flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(cx))
                    .child(content),
            )
            .child(self.footer(cx))
            .children(self.modal_view(cx))
    }
}

fn main() {
    gpu::stay_off_reserved_gpu();
    Application::new()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
            text_input::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("GUI Sandbox Environment".into()),
                        ..TitlebarOptions::default()
                    }),
                    app_id: Some("io.github.sovgut.gse".into()),
                    window_min_size: Some(size(px(820.), px(520.))),
                    ..WindowOptions::default()
                },
                |window, cx| {
                    let home = cx.new(Home::new);
                    let weak = home.downgrade();
                    window.on_window_should_close(cx, move |_, cx| {
                        weak.update(cx, Home::should_close).unwrap_or(true)
                    });
                    home
                },
            )
            .expect("opening the window");
            cx.activate(true);
        });
}
