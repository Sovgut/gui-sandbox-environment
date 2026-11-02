//! The gse app: the sandboxes in a list on the left, the selected one's
//! cover, facts and tabs on the right, its screen in place of everything
//! else while it plays, and app settings with the sets of apps and what new
//! sandboxes start with. Everything goes through the gse service; the app
//! itself needs no privileges.

mod applog;
mod client;
mod config;
mod devservice;
mod diagnostics;
mod flathub;
mod gpu;
mod icons;
mod journal;
mod keyboard;
mod onboarding;
mod presets;
mod preview;
mod programs;
mod sandbox;
mod screen;
mod settings;
mod setup;
mod shell;
mod text_input;
mod transfer;
mod ui;
mod update;
mod windowbar;

use std::collections::BTreeMap;

use futures::StreamExt as _;
use gpui::{
    App, Bounds, Context, Div, Entity, FontWeight, IntoElement, ParentElement, Render,
    SharedString, Styled, TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions,
    div, prelude::*, px, size,
};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::ButtonVariants as _;
use gse_protocol::{
    Environment, Event, Gpu, GuestApp, GuestFiles, JournalEntry, Request, Settings, Stage,
};

use crate::icons::Icon;
use crate::text_input::TextInput;
use crate::ui::{Kind, Theme, badge, button, icon_button};

fn stage_text(stage: Stage) -> &'static str {
    match stage {
        Stage::Preparing => "Preparing",
        Stage::SwitchingGpu => "Switching the GPU",
        Stage::Booting => "Booting",
        Stage::Ready => "Running",
        Stage::Stopping => "Stopping",
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
            Self::Preset => "by set of apps",
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
    Updates,
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
        /// Whether the settings it starts with are unfolded.
        details: bool,
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
    /// Removing everything, asked first: keep the taken files or not, the
    /// word that confirms it, and what would go once measured.
    RemoveAll {
        keep: bool,
        confirm: Entity<TextInput>,
        sizes: Option<transfer::Sizes>,
    },
    /// Playing `to` while `from` runs: one has to stop first.
    Switch { from: String, to: String },
    /// Playing `name` hands it the GPU that drives the monitor.
    SingleGpu { name: String },
}

/// A message for the user: an error stays until dismissed.
#[derive(Debug, Clone)]
struct Status {
    text: SharedString,
    error: bool,
    /// When it was said; a message that is no error goes after
    /// [`TOAST_FOR`].
    at: std::time::Instant,
}

/// How long a message that is no error stays in sight.
const TOAST_FOR: std::time::Duration = std::time::Duration::from_secs(5);

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
    modal: Option<Modal>,
    /// The user chose to close the window although a sandbox runs.
    closing: bool,
    /// The environment screen shown in place of everything else, and whose.
    screen: Option<(String, Entity<screen::Screen>)>,
    /// An environment whose screen opens once it is ready.
    open_when_ready: Option<String>,
    /// Failed attempts at opening the screen in a row.
    screen_attempts: u32,
    /// The sidebar shows only covers; it does so by itself while a screen
    /// is open.
    sidebar_collapsed: bool,
    /// The tab under the selected sandbox's cover.
    tab: shell::PanelTab,
    files_scroll: gpui::ScrollHandle,
    /// The welcome tour, while it runs.
    tour: Option<onboarding::Tour>,
    /// Preparing the computer, while it is not ready for sandboxes.
    setup: Option<setup::Setup>,
    /// How far back the journal shows, and whether its menu is open.
    journal_period: journal::Period,
    period_open: bool,
    /// The sandbox whose screen a preview shows, drawn without a screen.
    preview_screen: Option<String>,
    /// How round each corner of the window is now; what the app paints in
    /// a corner follows it.
    corners: gpui::Corners<gpui::Pixels>,
    /// When the app last looked for updates.
    update_checked: Option<chrono::DateTime<chrono::Local>>,
    /// The settings menu that is open, by its id.
    open_menu: Option<SharedString>,
    /// Putting a development build of the service in place.
    dev_install: devservice::Install,
}

impl Home {
    fn new(cx: &mut Context<Self>) -> Self {
        let preview = preview::state().is_some();
        if !preview {
            Self::follow_service(cx);
        }
        let processes_filter =
            cx.new(|cx| TextInput::new("Regex: program, process, command, PID", cx));
        cx.observe(&processes_filter, |_, _, cx| cx.notify())
            .detach();
        let journal_filter =
            cx.new(|cx| TextInput::new("Regex, @14:05, @14:00..14:30, @2026-11-01", cx));
        cx.observe(&journal_filter, |_, _, cx| cx.notify()).detach();
        let app_config = config::AppConfig::load();
        let setup = setup::needed().map(|step| setup::Setup {
            step,
            running: false,
            warnings: Vec::new(),
            failed: None,
            activity: None,
        });
        // The tour follows preparing, which starts it when it ends.
        let tour = (!app_config.tour_done && setup.is_none()).then_some(onboarding::Tour::Welcome);
        let sandbox_search = cx.new(|cx| TextInput::new("Search sandboxes", cx));
        cx.observe(&sandbox_search, |_, _, cx| cx.notify()).detach();
        if !preview {
            Self::keep_checking(cx);
        }
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
            modal: None,
            closing: false,
            screen: None,
            open_when_ready: None,
            screen_attempts: 0,
            sidebar_collapsed: false,
            tab: shell::PanelTab::Activity,
            files_scroll: gpui::ScrollHandle::new(),
            tour,
            setup,
            journal_period: journal::Period::Everything,
            period_open: false,
            open_menu: None,
            update_checked: None,
            dev_install: devservice::Install::Idle,
            corners: gpui::Corners::default(),
            preview_screen: None,
        };
        if !preview {
            Self::reload(cx);
        }
        home
    }

    /// Checks for updates now and then, and reloads the programs of the
    /// sandbox in sight.
    fn keep_checking(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                if this
                    .update(cx, |home, cx| home.check_updates(false, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(update::CHECK_EVERY).await;
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let alive = this.update(cx, |home, cx| {
                    if home
                        .status
                        .as_ref()
                        .is_some_and(|s| !s.error && s.at.elapsed() >= TOAST_FOR)
                    {
                        home.status = None;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
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
    }

    /// Follows the service's events, reloading what they change.
    fn follow_service(cx: &mut Context<Self>) {
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
    }

    fn say(&mut self, text: impl Into<SharedString>, error: bool) {
        let text = text.into();
        if error {
            tracing::warn!("shown: {text}");
        } else {
            tracing::info!("shown: {text}");
        }
        self.status = Some(Status {
            text,
            error,
            at: std::time::Instant::now(),
        });
    }

    /// Puts an event worth telling into the status line.
    fn note(&mut self, event: &Event) {
        if !matches!(event, Event::Journal { .. }) {
            tracing::info!("event {event:?}");
        }
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
                match &service {
                    Ok(found) if *found != home.service => {
                        tracing::info!("service: {found:?}");
                    }
                    Err(e) => tracing::warn!("service: {e:#}"),
                    Ok(_) => {}
                }
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
        tracing::info!("{key}: {doing}");
        self.busy.insert(key.clone(), doing);
        self.confirm = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { work() }).await;
            if let Err(e) = &outcome {
                tracing::warn!("{key}: {e:#}");
            }
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
        tracing::info!("request {request:?}");
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
        tracing::info!("opening the screen of {name}");
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
                entity.update(cx, |screen, cx| screen.focus(window, cx));
                self.screen = Some((name.to_owned(), entity));
                self.sidebar_collapsed = true;
                self.page = Page::Sandbox;
                self.selected = Some(name.to_owned());
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
        if let Some((name, _)) = self.screen.take() {
            tracing::info!("leaving the screen of {name}");
            self.sidebar_collapsed = false;
            window
                .set_raw_key_handler(None::<fn(&gpui::RawKeyEvent, &mut Window, &mut App) -> bool>);
            window.set_pointer_lock(false);
            if window.is_fullscreen() {
                window.toggle_fullscreen();
            }
        }
        cx.notify();
    }

    /// A sandbox at play: a bar with its controls over its screen, and its
    /// What the mouse does now over the screen, and the keys that change it.
    fn screen_hint(captured: bool) -> Div {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap_1p5()
            .text_sm()
            .text_color(Theme::muted())
            .child(if captured {
                "Mouse held by the sandbox \u{b7}"
            } else {
                "Click into the screen to play \u{b7}"
            })
            .child(ui::key("Ctrl Alt G"))
            .child(if captured {
                "frees it"
            } else {
                "grabs or frees the mouse"
            })
    }

    /// programs beside it.
    fn screen_view(
        &self,
        name: &str,
        screen: gpui::AnyElement,
        captured: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let full = window.is_fullscreen();
        let stop_key = format!("{name}/stop");
        let stopping = self.busy.contains_key(&stop_key);
        let internet = self
            .environments
            .iter()
            .find(|e| e.name == name)
            .is_some_and(|e| e.settings.internet);
        let (files, toggle) = (name.to_owned(), name.to_owned());
        let hint = Self::screen_hint(captured);
        let bar = div()
            .flex()
            .flex_1()
            .items_center()
            .gap_3()
            .pl_2()
            .child(
                windowbar::not_dragging().child(
                    gpui_kit::component::button::Button::new("screen-back")
                        .ghost()
                        .small()
                        .icon(gpui_kit::component::IconName::ChevronLeft)
                        .tooltip("Back to the sandbox's page")
                        .on_click(cx.listener(|home, _, window, cx| home.close_screen(window, cx))),
                ),
            )
            .child(div().font_weight(FontWeight::BOLD).child(name.to_owned()))
            .child(badge("Running", Theme::good()))
            .child(hint)
            .child(
                windowbar::not_dragging()
                    .gap_3()
                    .child(
                        gpui_kit::component::switch::Switch::new("screen-internet")
                            .checked(internet)
                            .label("Internet")
                            .on_change(cx.listener(move |home, on: &bool, _, cx| {
                                home.set_internet(&toggle, *on, cx);
                            })),
                    )
                    .child(icon_button(
                        "screen-files",
                        Icon::Transfer,
                        "Files: take them out, send them in",
                        Kind::Secondary,
                        true,
                        cx.listener(move |home, _, _, cx| home.open_transfer(&files, cx)),
                    ))
                    .child(icon_button(
                        "screen-full",
                        Icon::Fullscreen,
                        if full {
                            "Leave full screen (Ctrl Alt F)"
                        } else {
                            "Full screen (Ctrl Alt F)"
                        },
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, window, cx| {
                            if !window.is_fullscreen() {
                                home.say("Ctrl Alt F leaves full screen", false);
                                cx.notify();
                            }
                            window.toggle_fullscreen();
                        }),
                    ))
                    .child(
                        button(
                            "screen-stop",
                            if stopping { "Stopping..." } else { "Stop" },
                            Kind::Danger,
                            !stopping,
                            cx.listener(move |home, _, _, cx| {
                                home.request(stop_key.clone(), "Stopping", Request::Stop, cx);
                            }),
                        )
                        .icon(ui::kit_icon(Icon::Stop)),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(!full, |view| {
                view.child(
                    Self::window_bar(Some(bar.into_any_element()), window, cx)
                        .border_b_1()
                        .border_color(Theme::border()),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_1().min_w_0().child(screen))
                    .when(!full, |row| row.child(self.programs_aside(name, cx))),
            )
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

    /// Opens app settings, leaving a sandbox's screen.
    fn open_app_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_screen(window, cx);
        self.page = Page::AppSettings;
        self.confirm = None;
        cx.notify();
    }

    /// The menu of app settings, in place of the sandbox list.
    fn settings_nav(&self, cx: &mut Context<Self>) -> Div {
        let item = |id: &'static str, label: &'static str, tab: AppTab, cx: &mut Context<Self>| {
            let chosen = self.app_tab == tab;
            div()
                .id(id)
                .px_3()
                .py_2()
                .rounded(px(9.))
                .text_sm()
                .cursor_pointer()
                .map(|i| {
                    if chosen {
                        i.bg(Theme::selected()).font_weight(FontWeight::SEMIBOLD)
                    } else {
                        i.text_color(Theme::secondary_text())
                            .hover(|i| i.bg(Theme::raised()))
                    }
                })
                .on_click(cx.listener(move |home, _, _, cx| {
                    home.app_tab = tab;
                    cx.notify();
                }))
                .child(label)
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap_0p5()
            .w(px(shell::SIDEBAR_WIDTH))
            .h_full()
            .px_2()
            .bg(Theme::sidebar())
            .rounded_tl(self.corners.top_left)
            .rounded_bl(self.corners.bottom_left)
            .border_r_1()
            .border_color(Theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(shell::BAR_HEIGHT))
                    .px_1()
                    .child(
                        gpui_kit::component::button::Button::new("settings-back")
                            .ghost()
                            .small()
                            .icon(gpui_kit::component::IconName::ChevronLeft)
                            .tooltip("Back to sandboxes")
                            .on_click(cx.listener(|home, _, _, cx| {
                                home.page = Page::Sandbox;
                                cx.notify();
                            })),
                    )
                    .child(div().font_weight(FontWeight::BOLD).child("Settings")),
            )
            .child(item("nav-general", "General", AppTab::General, cx))
            .child(item("nav-presets", "Sets of apps", AppTab::Presets, cx))
            .child(item("nav-defaults", "New sandboxes", AppTab::Defaults, cx))
            .child(item(
                "nav-updates",
                "About and updates",
                AppTab::Updates,
                cx,
            ))
    }

    fn app_settings_view(&self, cx: &mut Context<Self>) -> Div {
        let title = match self.app_tab {
            AppTab::General => "General",
            AppTab::Presets => "Sets of apps",
            AppTab::Defaults => "New sandboxes",
            AppTab::Updates => "About and updates",
        };
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .text_size(px(26.))
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            .child(match self.app_tab {
                AppTab::Presets => self.presets_view(cx),
                AppTab::Defaults => self.defaults_view(cx),
                AppTab::General => self.general_view(cx),
                AppTab::Updates => self.updates_card(cx),
            })
    }

    /// A message that is no error, at the bottom right until dismissed.
    fn toast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let status = self.status.as_ref().filter(|s| !s.error)?;
        let dismiss = cx.listener(|home, _, _, cx| {
            home.status = None;
            cx.notify();
        });
        Some(
            div()
                .id("toast")
                .absolute()
                .right(px(24.))
                .bottom(px(24.))
                .flex()
                .items_start()
                .gap_3()
                .w(px(380.))
                .p_3p5()
                .rounded(px(12.))
                .bg(Theme::popover())
                .border_1()
                .border_color(Theme::line())
                .shadow_lg()
                .occlude()
                .cursor_pointer()
                .on_click(dismiss)
                .child(
                    gpui_kit::component::Icon::new(gpui_kit::component::IconName::CircleCheck)
                        .size(px(18.))
                        .text_color(Theme::good()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(status.text.clone()),
                )
                .child(
                    gpui_kit::component::Icon::new(gpui_kit::component::IconName::Close)
                        .size(px(14.))
                        .text_color(Theme::muted()),
                ),
        )
    }

    /// The build in progress, wherever the user is but on the page of the
    /// sets, which shows it in full.
    fn build_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let build = self.presets.build.as_ref()?;
        if self.page == Page::AppSettings && self.app_tab == AppTab::Presets {
            return None;
        }
        let step = self
            .build_steps(&build.preset)
            .and_then(|(steps, reached)| steps.get(reached).copied());
        let percent = self.build_percent(&build.preset).unwrap_or(0);
        let detail = match step {
            Some(step) => format!(
                "{step} \u{b7} {percent}%. No sandbox starts until it is done; closing the app does not stop it."
            ),
            None => {
                "No sandbox starts until it is done; closing the app does not stop it.".to_owned()
            }
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2p5()
                .px_4()
                .py_3()
                .rounded(px(12.))
                .bg(Theme::warning().opacity(0.1))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                                    format!(
                                        "{} is being built",
                                        presets::build_name(&build.preset)
                                    ),
                                ))
                                .child(div().text_sm().text_color(Theme::muted()).child(detail)),
                        )
                        .child(button(
                            "build-show",
                            "Show",
                            Kind::Secondary,
                            true,
                            cx.listener(|home, _, window, cx| {
                                home.open_app_settings(window, cx);
                                home.app_tab = AppTab::Presets;
                            }),
                        )),
                )
                .child(
                    div().h(px(4.)).rounded_full().bg(Theme::selected()).child(
                        div()
                            .h_full()
                            .rounded_full()
                            .bg(Theme::warning())
                            .w(gpui::relative(f32::from(percent) / 100.)),
                    ),
                ),
        )
    }

    /// The notice that the sandbox system or sets want rebuilding; not
    /// while a build runs, which is the update or has to end first.
    fn outdated_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.anything_outdated()
            || self.presets.build.is_some()
            || self.busy.contains_key("everything")
        {
            return None;
        }
        Some(
            ui::notice(
                Theme::warning(),
                if self.base_outdated() {
                    "The sandbox system is out of date"
                } else {
                    "Sets of apps are out of date"
                },
                "They lack what this version of GSE puts into sandboxes. Updating \
                 rebuilds what is out of date and resets the sandboxes made from it; it \
                 takes a while and needs no sandbox running.",
            )
            .child(button(
                "update-everything",
                "Update everything",
                Kind::Primary,
                !self.any_running() && self.presets.build.is_none(),
                cx.listener(|home, _, _, cx| home.update_everything(cx)),
            )),
        )
    }

    /// The notice of a service that does not match this app.
    fn service_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        match self.service {
            client::Match::Same => None,
            client::Match::ServiceNewer => Some(ui::notice(
                Theme::warning(),
                "This app is out of date",
                "The gse service is newer than this app. Install the current version of the app.",
            )),
            client::Match::ServiceOlder if setup::carries_service() => Some(
                ui::notice(
                    Theme::warning(),
                    "The service needs an update",
                    "The gse service is older than this app. Updating installs the one this \
                         app carries and asks for your password; a running sandbox and an image \
                         build are stopped first.",
                )
                .child(button(
                    "update-service",
                    if self.restarting {
                        "Updating..."
                    } else {
                        "Update service"
                    },
                    Kind::Primary,
                    !self.restarting,
                    cx.listener(|home, _, _, cx| home.update_service(cx)),
                )),
            ),
            client::Match::ServiceOlder => Some(
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
            ),
        }
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
        if let Some(building) = self.build_notice(cx) {
            any = true;
            notices = notices.child(building);
        }
        if let Some(outdated) = self.outdated_notice(cx) {
            any = true;
            notices = notices.child(outdated);
        }
        if let Some(service) = self.service_notice(cx) {
            any = true;
            notices = notices.child(service);
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
        self.corners = gpui_kit::component::window_corner_radius(window);
        let root = div()
            .relative()
            .flex()
            .size_full()
            .bg(Theme::background())
            .text_color(Theme::text())
            .rounded_tl(self.corners.top_left)
            .rounded_tr(self.corners.top_right)
            .rounded_bl(self.corners.bottom_left)
            .rounded_br(self.corners.bottom_right);
        if let Some(page) = self.setup_page(window, cx) {
            return root.child(page);
        }
        if let Some(page) = self.tour_page(window, cx) {
            return root.child(page).children(self.modal_view(cx));
        }
        if let Some((name, screen)) = self.screen.clone() {
            let captured = screen.read(cx).captured();
            return root
                .child(self.sidebar(window.is_fullscreen(), cx))
                .child(self.screen_view(&name, screen.into_any_element(), captured, window, cx))
                .children(self.toast(cx))
                .children(self.modal_view(cx));
        }
        if let Some(name) = self.preview_screen.clone() {
            let screen = preview_desktop(&name);
            return root
                .child(self.sidebar(window.is_fullscreen(), cx))
                .child(self.screen_view(&name, screen, false, window, cx))
                .children(self.toast(cx))
                .children(self.modal_view(cx));
        }
        let (side, content) = match self.page {
            Page::AppSettings => (
                self.settings_nav(cx),
                div()
                    .id("app-settings-page")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .w_full()
                            .px_12()
                            .pb_8()
                            .children(self.notices(cx))
                            .child(self.app_settings_view(cx)),
                    )
                    .into_any_element(),
            ),
            Page::Sandbox => (
                self.sidebar(window.is_fullscreen(), cx),
                self.sandbox_page(window, cx).into_any_element(),
            ),
        };
        root.child(side)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(Self::window_bar(None, window, cx))
                    .child(content),
            )
            .children(self.toast(cx))
            .children(self.modal_view(cx))
    }
}

/// A sandbox's screen as a preview draws it: its cover where the desktop
/// would be.
fn preview_desktop(name: &str) -> gpui::AnyElement {
    div()
        .flex()
        .size_full()
        .items_center()
        .justify_center()
        .bg(gpui::black())
        .child(
            div()
                .w(px(820.))
                .h(px(461.))
                .rounded(px(6.))
                .bg(shell::cover(name))
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui::white().opacity(0.45))
                .child("[the sandbox's desktop, live]"),
        )
        .into_any_element()
}

fn main() {
    if preview::state().as_deref() == Some("list") {
        print!("{}", preview::list());
        return;
    }
    applog::init();
    if std::env::args().any(|a| a == "--diagnostics") {
        match diagnostics::save() {
            Ok(path) => println!("{}", path.display()),
            Err(e) => {
                eprintln!("diagnostics were not saved: {e:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    gpu::stay_off_reserved_gpu();
    gpui_kit::application()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            // Previews match the design, which is dark.
            let look = config::AppConfig::load().look;
            let light = preview::state().is_none() && look.light(cx.window_appearance());
            Theme::apply(light, cx);
            text_input::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("GSE".into()),
                        ..TitleBar::title_bar_options()
                    }),
                    window_decorations: Some(WindowDecorations::Client),
                    app_owns_titlebar_drag: true,
                    app_id: Some("io.github.sovgut.gse".into()),
                    window_min_size: Some(size(px(960.), px(600.))),
                    ..WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let home = cx.new(Home::new);
                    if let Some(state) = preview::state() {
                        home.update(cx, |home, cx| home.preview(&state, cx));
                    }
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
