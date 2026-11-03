//! The gse app: the sandboxes in a list on the left, the selected one's
//! cover, facts and tabs on the right, its screen in place of everything
//! else while it plays, and app settings with the sets of apps and what new
//! sandboxes start with. Everything goes through the gse service; the app
//! itself needs no privileges.

mod applog;
mod client;
mod clock;
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
mod storage;
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
use gpui_kit::component::TitleBar;
use gse_protocol::{
    Environment, Event, Gpu, GuestApp, GuestFiles, JournalEntry, Request, Settings, Stage,
};

use crate::icons::Icon;
use crate::text_input::{Look, TextInput};
use crate::ui::{Kind, Size, SwitchSize, Theme, Tone, badge, button, icon_button};

fn stage_text(stage: Stage) -> &'static str {
    match stage {
        Stage::Preparing => "Preparing",
        Stage::SwitchingGpu => "Handing over the graphics card",
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
    /// Playing `name` hands it the GPU that drives the monitor; `explained`
    /// is whether "Don't explain this again" is ticked.
    SingleGpu { name: String, explained: bool },
}

/// A message at the bottom right of the window: a result, which goes by
/// itself after [`TOAST_FOR`], or an error, which waits for a click.
#[derive(Debug, Clone)]
pub(crate) struct Toast {
    /// Tells the toasts apart while they stack; [`Home::tell`] gives it.
    id: u64,
    /// What happened, in bold.
    title: SharedString,
    /// Why, or what it means, under the title.
    detail: Option<SharedString>,
    error: bool,
    /// The file a result made, named under the title with a way to its
    /// folder.
    file: Option<std::path::PathBuf>,
    /// The sandbox an error is about; "Open activity" shows its journal.
    sandbox: Option<String>,
    /// When it was said.
    at: std::time::Instant,
}

impl Toast {
    fn new(title: impl Into<SharedString>, error: bool) -> Self {
        Self {
            id: 0,
            title: title.into(),
            detail: None,
            error,
            file: None,
            sandbox: None,
            at: std::time::Instant::now(),
        }
    }

    /// A result in a line.
    pub(crate) fn result(title: impl Into<SharedString>) -> Self {
        Self::new(title, false)
    }

    /// What went wrong, and why.
    pub(crate) fn error(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Self {
        Self {
            detail: Some(detail.into()),
            ..Self::new(title, true)
        }
    }

    /// An error said in one string: its first clause heads it and the rest
    /// says why, as in `the gse service is not running: Connection refused`.
    fn error_text(text: &str) -> Self {
        let (title, detail) = text
            .split_once(": ")
            .or_else(|| text.split_once(". "))
            .map_or((text, None), |(title, detail)| (title, Some(detail)));
        Self {
            detail: detail.map(|d| sentence(d).into()),
            ..Self::new(sentence(title), true)
        }
    }

    /// Files taken out of `sandbox` into the zip archive `archive`.
    pub(crate) fn taken(sandbox: &str, count: usize, archive: std::path::PathBuf) -> Self {
        let files = if count == 1 {
            "1 file".to_owned()
        } else {
            format!("{count} files")
        };
        Self::result(format!("Took {files} from {sandbox}")).wrote(archive)
    }

    /// The result made the file `path`.
    pub(crate) fn wrote(mut self, path: std::path::PathBuf) -> Self {
        self.file = Some(path);
        self
    }

    /// The error is about the sandbox `name`.
    pub(crate) fn about(mut self, name: &str) -> Self {
        self.sandbox = Some(name.to_owned());
        self
    }

    /// The whole message on one line, the file it names included, as the
    /// log and the first start's page say it.
    pub(crate) fn text(&self) -> SharedString {
        let mut text = self.title.to_string();
        for more in self
            .detail
            .iter()
            .map(ToString::to_string)
            .chain(self.file.iter().map(|file| config::shown_path(file)))
        {
            text = format!("{text}: {more}");
        }
        text.into()
    }
}

/// `text` with a capital first letter, as a line of its own starts: errors
/// chained by `anyhow` start small.
fn sentence(text: &str) -> String {
    let mut letters = text.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

/// A running sandbox's state with how long it has been up, `minutes`;
/// nothing under a minute.
fn running_for(minutes: i64) -> String {
    match minutes {
        ..=0 => "Running".to_owned(),
        1..60 => format!("Running \u{b7} {minutes} min"),
        _ => format!("Running \u{b7} {} h {} min", minutes / 60, minutes % 60),
    }
}

/// How long a toast that is no error stays in sight.
const TOAST_FOR: std::time::Duration = std::time::Duration::from_secs(5);
/// The most toasts stacked at once; the oldest give way.
const MOST_TOASTS: usize = 3;

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
    /// The toasts at the bottom right, oldest first.
    toasts: Vec<Toast>,
    /// The id the next toast gets.
    next_toast: u64,
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
    /// The archive each sandbox's Outbox last went into, by its name.
    last_taken: BTreeMap<String, std::path::PathBuf>,
    /// The move of files under way for each sandbox: how far it got and
    /// the flag that stops it.
    flows: BTreeMap<String, transfer::Flow>,
    /// The service's antivirus as it last said, and whether an update of it
    /// runs now.
    antivirus: Option<client::Antivirus>,
    /// The app an update started: it updates the service as soon as it
    /// finds it older.
    finish_update: bool,
    /// Where the sandboxes are kept, as the service last said; the folder
    /// chosen to move them to, waiting for a yes; the move under way.
    storage: Option<client::Storage>,
    /// The host's devices a sandbox can take, as last listed.
    host_devices: client::HostDevices,
    /// Whether the sandbox settings list the USB devices gse cannot tell
    /// the kind of.
    show_other_devices: bool,
    storage_target: Option<std::path::PathBuf>,
    moving_storage: Option<transfer::Flow>,
    app_config: config::AppConfig,
    update: update::State,
    /// The update banner was put off with Later until the app starts again.
    update_later: bool,
    /// When each running sandbox's desktop came up, as the app saw it.
    ready_at: BTreeMap<String, chrono::DateTime<chrono::Local>>,
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
        let processes_filter = cx.new(|cx| {
            TextInput::new("Find a program, process or PID", cx)
                .look(Look::Search)
                .icon(Icon::Search)
        });
        cx.observe(&processes_filter, |_, _, cx| cx.notify())
            .detach();
        let journal_filter = cx.new(|cx| {
            TextInput::new("Regex, @14:05, @14:00..14:30, @2026-11-01", cx)
                .look(Look::Search)
                .icon(Icon::Search)
                .monospace()
        });
        cx.observe(&journal_filter, |_, _, cx| cx.notify()).detach();
        let app_config = config::AppConfig::load();
        let setup = setup::needed().map(setup::Setup::new);
        // The tour follows preparing, which starts it when it ends.
        let tour = (!app_config.tour_done && setup.is_none()).then_some(onboarding::Tour::Welcome);
        let sandbox_search = cx.new(|cx| {
            TextInput::new("Search", cx)
                .look(Look::Search)
                .icon(Icon::Search)
                .fill(Theme::raised)
        });
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
            toasts: Vec::new(),
            next_toast: 0,
            settings: Err("Loading the settings...".into()),
            draft: None,
            sandbox_draft: None,
            saving: false,
            // A preview's GPUs are its own; `gpu-moved` shows the notice.
            started_for: if preview::active() {
                Vec::new()
            } else {
                gpu::reserved_gpus()
            },
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
            last_taken: BTreeMap::new(),
            flows: BTreeMap::new(),
            antivirus: None,
            finish_update: std::env::var_os(update::FINISH_UPDATE).is_some(),
            storage: None,
            host_devices: client::HostDevices::default(),
            show_other_devices: false,
            storage_target: None,
            moving_storage: None,
            app_config,
            update: update::State::Idle,
            update_later: false,
            ready_at: BTreeMap::new(),
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
                    let shown = home.toasts.len();
                    home.toasts
                        .retain(|toast| toast.error || toast.at.elapsed() < TOAST_FOR);
                    if home.toasts.len() != shown {
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

    /// Says `text` in a toast: a result, or an error whose first clause
    /// heads it.
    fn say(&mut self, text: impl Into<SharedString>, error: bool) {
        let text = text.into();
        self.tell(if error {
            Toast::error_text(&text)
        } else {
            Toast::result(text)
        });
    }

    /// Shows `toast` at the bottom right, in place of one that says the
    /// same.
    pub(crate) fn tell(&mut self, mut toast: Toast) {
        if toast.error {
            tracing::warn!("shown: {}", toast.text());
        } else {
            tracing::info!("shown: {}", toast.text());
        }
        self.toasts
            .retain(|shown| (&shown.title, &shown.detail) != (&toast.title, &toast.detail));
        toast.id = self.next_toast;
        self.next_toast += 1;
        self.toasts.push(toast);
        let over = self.toasts.len().saturating_sub(MOST_TOASTS);
        self.toasts.drain(..over);
    }

    /// Puts an event worth telling into a toast.
    fn note(&mut self, event: &Event) {
        if !matches!(event, Event::Journal { .. }) {
            tracing::info!("event {event:?}");
        }
        match event {
            Event::Stopped {
                name,
                error: Some(error),
            } => {
                self.ready_at.remove(name);
                self.tell(Toast::error(format!("{name} stopped"), sentence(error)).about(name));
            }
            Event::Stopped { name, error: None } => {
                self.ready_at.remove(name);
                self.say(format!("{name} stopped"), false);
            }
            Event::Progress { name, stage } => {
                if *stage == Stage::Ready {
                    self.ready_at.insert(name.clone(), clock::now());
                }
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
            let (loaded, settings, service, antivirus, storage, devices) = cx
                .background_executor()
                .spawn(async {
                    let loaded = client::environments().and_then(|e| Ok((e, client::images()?)));
                    (
                        loaded,
                        client::settings(),
                        client::hello(),
                        client::antivirus(),
                        client::storage(),
                        client::host_devices(),
                    )
                })
                .await;
            this.update(cx, |home, cx| {
                if let Ok(devices) = devices {
                    home.host_devices = devices;
                }
                if let Ok(antivirus) = antivirus {
                    home.antivirus = Some(antivirus);
                }
                if let Ok(storage) = storage {
                    home.storage = Some(storage);
                }
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
                    if home.finish_update
                        && service == client::Match::ServiceOlder
                        && setup::carries_service()
                        && !home.restarting
                    {
                        home.finish_update = false;
                        home.update_service(cx);
                    } else if service == client::Match::Same {
                        home.finish_update = false;
                    }
                }
                match loaded {
                    Ok((environments, images)) => {
                        // The errors of the time the service was gone are
                        // stale.
                        if home.connected == Some(false) {
                            home.toasts.retain(|toast| !toast.error);
                        }
                        home.environments = environments;
                        home.images = images;
                        home.connected = Some(true);
                        home.keep_selection(cx);
                    }
                    Err(e) => {
                        home.connected = Some(false);
                        // A service that restarts is briefly gone; that is
                        // no error.
                        if !home.restarting {
                            home.say(format!("{e:#}"), true);
                        }
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
        if preview::active() {
            self.say(format!("Preview: {doing} is not sent"), false);
            cx.notify();
            return;
        }
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
                    let error = format!("{e:#}");
                    let toast = if doing.is_empty() {
                        Toast::error_text(&error)
                    } else {
                        Toast::error(format!("{doing} {name} failed"), sentence(&error))
                    };
                    home.tell(toast.about(name));
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
                self.tell(
                    Toast::error(
                        format!("The screen of {name} did not open"),
                        sentence(&format!("{e:#}")),
                    )
                    .about(name),
                );
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

    /// What the mouse does now over the screen, and the keys that change it.
    /// The spaces around the key are the words' own, as in a line of text;
    /// in a narrow window the words give way to an ellipsis.
    fn screen_hint(captured: bool) -> Div {
        let words = |text: &'static str| div().min_w_0().truncate().child(text);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .text_size(px(13.))
            .text_color(Theme::muted())
            .child(words(if captured {
                "Mouse held by the sandbox \u{b7} "
            } else {
                "Click into the screen to play \u{b7} "
            }))
            .child(ui::inline_key("Ctrl Alt G"))
            .child(words(if captured {
                " frees it"
            } else {
                " grabs or frees the mouse"
            }))
    }

    /// The controls at the end of a playing sandbox's bar: its internet,
    /// its files, full screen and Stop.
    fn screen_tools(&self, name: &str, full: bool, cx: &mut Context<Self>) -> Div {
        let stop_key = format!("{name}/stop");
        let stopping = self.busy.contains_key(&stop_key);
        let internet = self
            .environments
            .iter()
            .find(|e| e.name == name)
            .is_some_and(|e| e.settings.internet);
        let (files, toggle) = (name.to_owned(), name.to_owned());
        windowbar::not_dragging()
            .gap_3()
            .child(
                // The switch sits in a button of the bar, its label
                // toggling it too.
                ui::switch("screen-internet", internet, SwitchSize::Bar)
                    .label("Internet")
                    .h(px(32.))
                    .px_3()
                    .rounded(px(8.))
                    .bg(Theme::raised())
                    .text_size(px(13.))
                    .text_color(Theme::soft_text())
                    .on_change(cx.listener(move |home, on: &bool, _, cx| {
                        home.set_internet(&toggle, *on, cx);
                    })),
            )
            .child(
                icon_button(
                    "screen-files",
                    Icon::Transfer,
                    "Files: take them out, send them in",
                    Kind::Quiet,
                    true,
                    cx.listener(move |home, _, _, cx| home.open_transfer(&files, cx)),
                )
                .fill(Theme::raised()),
            )
            .child(
                icon_button(
                    "screen-full",
                    Icon::Fullscreen,
                    if full {
                        "Leave full screen (Ctrl Alt F)"
                    } else {
                        "Full screen (Ctrl Alt F)"
                    },
                    Kind::Quiet,
                    true,
                    cx.listener(|home, _, window, cx| {
                        if !window.is_fullscreen() {
                            home.say("Ctrl Alt F leaves full screen", false);
                            cx.notify();
                        }
                        window.toggle_fullscreen();
                    }),
                )
                .fill(Theme::raised()),
            )
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
                .icon(Icon::Stop)
                .icon_size(12.),
            )
    }

    /// How long `name` has been up: since its desktop came up when the app
    /// saw that, since it started otherwise.
    fn running_text(&self, name: &str) -> String {
        let since = self.ready_at.get(name).copied().or_else(|| {
            use chrono::TimeZone as _;
            let started = self
                .environments
                .iter()
                .find(|e| e.name == name)?
                .last_started?;
            chrono::Local
                .timestamp_opt(i64::try_from(started).ok()?, 0)
                .single()
        });
        running_for(since.map_or(0, |since| (clock::now() - since).num_minutes()))
    }

    /// A sandbox at play: a bar with its controls over its screen, and its
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
        // A narrow window wraps the mouse hint, not the controls after it.
        let bar = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_3()
            .pl_5()
            .pr(px(14.))
            .child(
                windowbar::not_dragging().child(
                    icon_button(
                        "screen-back",
                        gpui_kit::component::IconName::ChevronLeft,
                        "Back to the sandbox's page",
                        Kind::Ghost,
                        true,
                        cx.listener(|home, _, window, cx| home.close_screen(window, cx)),
                    )
                    .size(Size::Small)
                    .icon_size(16.)
                    .text_color(Theme::nav_text()),
                ),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .child(name.to_owned()),
            )
            .child(badge(self.running_text(name), Theme::good()))
            .child(Self::screen_hint(captured))
            .child(self.screen_tools(name, full, cx));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            // The bar is as tall as the title bar, with its line below that.
            .when(!full, |view| {
                view.child(
                    Self::window_bar(Some(bar.into_any_element()), window, cx)
                        .h(px(shell::BAR_HEIGHT + 1.))
                        .border_b_1()
                        .border_color(Theme::divider()),
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

    /// The sandboxes the search finds by their names and their sets' names
    /// and titles, in the chosen order.
    fn listed(&self, cx: &Context<Self>) -> Vec<&Environment> {
        let search = self.sandbox_search.read(cx).text().trim().to_lowercase();
        let mut listed: Vec<&Environment> = self
            .environments
            .iter()
            .filter(|e| {
                search.is_empty()
                    || e.name.to_lowercase().contains(&search)
                    || e.image.to_lowercase().contains(&search)
                    || presets::set_title(&e.image)
                        .to_lowercase()
                        .contains(&search)
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
                .py(px(9.))
                .rounded(px(9.))
                .text_sm()
                .cursor_pointer()
                .map(|i| {
                    if chosen {
                        i.bg(Theme::selected()).font_weight(FontWeight::SEMIBOLD)
                    } else {
                        i.text_color(Theme::nav_text())
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
                    .px_2()
                    .child(
                        icon_button(
                            "settings-back",
                            gpui_kit::component::IconName::ChevronLeft,
                            "Back to sandboxes",
                            Kind::Ghost,
                            true,
                            cx.listener(|home, _, _, cx| {
                                home.page = Page::Sandbox;
                                cx.notify();
                            }),
                        )
                        .size(Size::Small)
                        .icon_size(16.)
                        .text_color(Theme::nav_text()),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
                            .child("Settings"),
                    ),
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

    /// The chosen part of app settings under the notices, in a column as
    /// wide as the design draws it: General narrow, New sandboxes and About
    /// and updates up to 760, the sets as wide as the window. Each page
    /// heads itself, but for General. The column is at least as tall as
    /// the page, so the set editor's panels reach down to its foot.
    fn app_settings_view(&self, cx: &mut Context<Self>) -> Div {
        let page = match self.app_tab {
            AppTab::General => div()
                .flex()
                .flex_col()
                .gap_7()
                .child(
                    div()
                        .text_size(px(26.))
                        .font_weight(FontWeight::BOLD)
                        .child("General"),
                )
                .child(self.general_view(cx)),
            AppTab::Presets => self.presets_view(cx),
            AppTab::Defaults => self.defaults_view(cx),
            AppTab::Updates => self.updates_card(cx),
        };
        let general = self.app_tab == AppTab::General;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .min_h_full()
            .px(px(if general { 48. } else { 40. }))
            .pb(px(if general { 32. } else { 24. }))
            .map(|column| match self.app_tab {
                AppTab::General => column.max_w(px(720. + 2. * 48.)),
                AppTab::Defaults | AppTab::Updates => column.max_w(px(760. + 2. * 40.)),
                AppTab::Presets => column,
            })
            .children(self.notices(cx))
            .child(page)
    }

    /// Takes the toast `id` away.
    fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        self.toasts.retain(|toast| toast.id != id);
        cx.notify();
    }

    /// The Activity tab of the sandbox `name`, on its page.
    fn open_activity(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_screen(window, cx);
        self.select(name, window, cx);
        self.tab = shell::PanelTab::Activity;
        cx.notify();
    }

    /// What a toast leads to: the folder of the file a result made, the
    /// activity of a sandbox an error is about.
    fn toast_links(&self, toast: &Toast, cx: &mut Context<Self>) -> Vec<gpui::Stateful<Div>> {
        let id = toast.id;
        let show_folder = toast.file.clone().map(|file| {
            ui::link(
                SharedString::from(format!("toast-{id}-folder")),
                "Show in folder",
            )
            .on_click(cx.listener(move |home, _, _, cx| {
                if !home.preview_skips("showing the folder", cx) {
                    cx.reveal_path(&file);
                }
            }))
        });
        let activity = toast
            .sandbox
            .clone()
            .filter(|name| self.environments.iter().any(|e| e.name == *name))
            .map(|name| {
                ui::link(
                    SharedString::from(format!("toast-{id}-activity")),
                    "Open activity",
                )
                .on_click(cx.listener(move |home, _, window, cx| {
                    home.dismiss_toast(id, cx);
                    home.open_activity(&name, window, cx);
                }))
            });
        show_folder.into_iter().chain(activity).collect()
    }

    /// One toast: a result with a check, which a click takes away, or an
    /// error with a red edge, which waits for its ×. A result that made a
    /// file names it with a way to its folder; an error about a sandbox
    /// leads to its activity.
    fn toast_view(&self, toast: &Toast, cx: &mut Context<Self>) -> impl IntoElement {
        let id = toast.id;
        let line = |text: SharedString| {
            div()
                .mt(px(2.))
                .text_color(Theme::secondary_text())
                .child(text)
        };
        let links = self.toast_links(toast, cx);
        div()
            .id(SharedString::from(format!("toast-{id}")))
            .flex()
            .items_start()
            .gap_3()
            .p_3p5()
            .rounded(px(12.))
            .bg(Theme::popover())
            .border_1()
            .border_color(if toast.error {
                Theme::danger().opacity(0.3)
            } else {
                Theme::line()
            })
            .shadow(vec![
                gpui::BoxShadow::new(px(0.), px(12.), gpui::black().opacity(0.53))
                    .blur_radius(px(32.)),
            ])
            .occlude()
            .when(!toast.error, |t| {
                t.cursor_pointer()
                    .on_click(cx.listener(move |home, _, _, cx| home.dismiss_toast(id, cx)))
            })
            .child(if toast.error {
                ui::kit_icon(Icon::Failed)
                    .size(px(18.))
                    .text_color(Theme::danger())
            } else {
                ui::kit_icon(Icon::Done)
                    .size(px(18.))
                    .text_color(Theme::good())
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(toast.title.clone()),
                    )
                    .children(toast.detail.clone().map(line))
                    .children(
                        toast
                            .file
                            .as_deref()
                            .map(|file| line(config::shown_path(file).into())),
                    )
                    .when(!links.is_empty(), |text| {
                        text.child(div().flex().gap_3().mt_2().children(links))
                    }),
            )
            .when(toast.error, |t| {
                t.child(
                    div()
                        .id(SharedString::from(format!("toast-{id}-dismiss")))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(22.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_color(Theme::muted())
                        .hover(|b| b.bg(Theme::raised()).text_color(Theme::text()))
                        .tooltip(ui::tooltip("Dismiss".into()))
                        .on_click(cx.listener(move |home, _, _, cx| home.dismiss_toast(id, cx)))
                        .child(ui::kit_icon(Icon::Close).size(px(10.))),
                )
            })
    }

    /// The toasts, stacked at the bottom right with the newest at the
    /// bottom.
    fn toasts_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.toasts.is_empty() {
            return None;
        }
        let mut stack = div()
            .absolute()
            .right(px(24.))
            .bottom(px(24.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .w(px(380.));
        for toast in &self.toasts {
            stack = stack.child(self.toast_view(toast, cx));
        }
        Some(stack)
    }

    /// The build in progress, wherever the user is but where it shows
    /// already: on the page of the sets, which shows it in full, and on the
    /// page of a sandbox made from the set that builds, whose cover follows
    /// the build.
    fn build_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let build = self.presets.build.as_ref()?;
        let shown = match self.page {
            Page::AppSettings => self.app_tab == AppTab::Presets,
            Page::Sandbox => self.chosen().is_some_and(|e| e.image == build.preset),
        };
        if shown {
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
                                .child(
                                    div()
                                        .text_sm()
                                        .line_height(ui::normal_line(14.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!(
                                            "{} is being built",
                                            presets::build_name(&build.preset)
                                        )),
                                )
                                .child(
                                    div()
                                        .mt(px(2.))
                                        .text_size(px(13.))
                                        .line_height(ui::normal_line(13.))
                                        .text_color(Theme::muted())
                                        .child(detail),
                                ),
                        )
                        .child(
                            button(
                                "build-show",
                                "Show",
                                Kind::Secondary,
                                true,
                                cx.listener(|home, _, window, cx| {
                                    home.open_app_settings(window, cx);
                                    home.app_tab = AppTab::Presets;
                                }),
                            )
                            .px(px(14.)),
                        ),
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
        let stay = "games, saves and files in /home stay.";
        let text = match self.presets.stale.as_slice() {
            _ if self.base_outdated() => format!(
                "The sandbox system is out of date. Updating rebuilds it and the sets on it; \
                 {stay}"
            ),
            [set] => format!(
                "{} is out of date. Updating rebuilds it; {stay}",
                presets::build_name(set)
            ),
            _ => format!("Sets of apps are out of date. Updating rebuilds them; {stay}"),
        };
        Some(
            ui::banner(Tone::Warning, Icon::Warning, text).child(
                button(
                    "update-everything",
                    "Update everything",
                    Kind::Warning,
                    !self.any_running() && self.presets.build.is_none(),
                    cx.listener(|home, _, _, cx| home.update_everything(cx)),
                )
                .size(Size::Banner)
                .tooltip("It takes a while and needs no sandbox running"),
            ),
        )
    }

    /// The banner of a service that does not match this app.
    fn service_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let banner = |text: &'static str| ui::banner(Tone::Warning, Icon::Warning, text);
        match self.service {
            client::Match::Same => None,
            client::Match::ServiceNewer => {
                // Only the AppImage updates itself. A release it found has
                // its own install button, in the update banner or on About
                // and updates; until then this banner looks for one.
                Some(match &self.update {
                    _ if update::running_appimage().is_none() => banner(
                        "The background service is newer than the app. Install the current \
                         version of the app.",
                    ),
                    update::State::Available(_) | update::State::Installing(_) => banner(
                        "The background service is newer than the app. Installing the update \
                         brings the app up to it.",
                    ),
                    state => banner(
                        "The background service is newer than the app. Install the current \
                         version of the app.",
                    )
                    .child(
                        button(
                            "update-check",
                            if matches!(state, update::State::Checking) {
                                "Checking..."
                            } else {
                                "Check for updates"
                            },
                            Kind::Warning,
                            !matches!(state, update::State::Checking),
                            cx.listener(|home, _, _, cx| home.check_updates(true, cx)),
                        )
                        .size(Size::Banner),
                    ),
                })
            }
            client::Match::ServiceOlder if setup::carries_service() => Some(
                banner(if self.any_running() {
                    "The background service is older than the app. Stop the running sandbox to \
                     update it."
                } else {
                    "The background service is older than the app. Updating it asks for your \
                     password."
                })
                .child(
                    button(
                        "update-service",
                        if self.restarting {
                            "Updating..."
                        } else {
                            "Update service"
                        },
                        Kind::Warning,
                        !self.restarting && !self.any_running(),
                        cx.listener(|home, _, _, cx| home.update_service(cx)),
                    )
                    .size(Size::Banner),
                ),
            ),
            client::Match::ServiceOlder => Some(
                banner(
                    "The background service still runs an older version. Restarting it stops a \
                     running sandbox.",
                )
                .child(
                    button(
                        "restart-service",
                        if self.restarting {
                            "Restarting..."
                        } else {
                            "Restart service"
                        },
                        Kind::Warning,
                        !self.restarting,
                        cx.listener(|home, _, _, cx| {
                            home.restarting = true;
                            home.request("service".into(), "", Request::Restart, cx);
                        }),
                    )
                    .size(Size::Banner),
                ),
            ),
        }
    }

    /// The banner of a newer release, unless Later put it off; Later sits
    /// beside Install until the install begins.
    fn release_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.update_later {
            return None;
        }
        let banner = self.update_notice(cx)?;
        let later = matches!(self.update, update::State::Available(_)).then(|| {
            button(
                "update-later",
                "Later",
                Kind::Ghost,
                true,
                cx.listener(|home, _, _, cx| {
                    home.update_later = true;
                    cx.notify();
                }),
            )
            .size(Size::Banner)
            .px(px(10.))
            .text_color(Theme::secondary_text())
        });
        Some(banner.children(later))
    }

    /// Banners above the right side, each for something to do or wait for:
    /// a newer release, a build, what is out of date, the service's
    /// version, a GPU change the app has to restart for. Results and errors
    /// are toasts instead.
    fn notices(&self, cx: &mut Context<Self>) -> Option<Div> {
        let mut notices = div().flex().flex_col().gap_4();
        let mut any = false;
        // About and updates shows a newer release in full.
        let about = self.page == Page::AppSettings && self.app_tab == AppTab::Updates;
        if !about && let Some(update) = self.release_notice(cx) {
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
        if self.connected == Some(true)
            && self.settings.is_ok()
            && self.gpus_in_settings() != self.started_for
        {
            any = true;
            notices = notices.child(
                ui::banner(
                    Tone::Accent,
                    Icon::Refresh,
                    "Restart the app to apply the GPU change: it keeps its own drawing off the \
                     GPUs sandboxes get.",
                )
                .child(
                    button(
                        "restart-app",
                        "Restart now",
                        Kind::Primary,
                        !self.any_running(),
                        cx.listener(|_, _, _, cx| cx.restart()),
                    )
                    .size(Size::Banner),
                ),
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
        let root = ui::window_text(div())
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
                .children(self.toasts_view(cx))
                .children(self.modal_view(cx));
        }
        if let Some(name) = self.preview_screen.clone() {
            let screen = preview_desktop(&name);
            return root
                .child(self.sidebar(window.is_fullscreen(), cx))
                .child(self.screen_view(&name, screen, preview::flag("captured"), window, cx))
                .children(self.toasts_view(cx))
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
                    .child(self.app_settings_view(cx))
                    .into_any_element(),
            ),
            // The title bar over the page leaves full screen.
            Page::Sandbox => (
                self.sidebar(false, cx),
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
            .children(self.toasts_view(cx))
            .children(self.modal_view(cx))
    }
}

/// A sandbox's screen as a preview draws it: its cover where the desktop
/// would be, a 16:9 screen nine tenths as wide as the room it has.
fn preview_desktop(name: &str) -> gpui::AnyElement {
    div()
        .flex()
        .size_full()
        .items_center()
        .justify_center()
        .bg(screen::surround())
        .child(
            div()
                .relative()
                .w(gpui::relative(0.9))
                .max_h_full()
                .aspect_ratio(16. / 9.)
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui::white().opacity(0.45))
                .child(shell::cover(name, 6.))
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
            // Previews are dark like the design unless asked for light.
            let look = config::AppConfig::load().look;
            let light = if preview::active() {
                preview::light()
            } else {
                look.light(cx.window_appearance())
            };
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
                        if preview::flag("fullscreen") {
                            window.toggle_fullscreen();
                        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_heads_its_toast_with_its_first_clause() {
        let toast = Toast::error_text(
            "the gse service is not running (/run/gse/service.sock): Connection refused",
        );
        assert_eq!(
            toast.title,
            "The gse service is not running (/run/gse/service.sock)"
        );
        assert_eq!(toast.detail.as_deref(), Some("Connection refused"));
        let toast = Toast::error_text("The service does not know this yet. Restart it.");
        assert_eq!(toast.title, "The service does not know this yet");
        assert_eq!(toast.detail.as_deref(), Some("Restart it."));
        let toast = Toast::error_text("nothing more to say");
        assert_eq!(
            (toast.title.as_ref(), toast.detail),
            ("Nothing more to say", None)
        );
    }

    #[test]
    fn a_running_sandbox_says_how_long_it_has_been_up() {
        assert_eq!(running_for(0), "Running");
        assert_eq!(running_for(24), "Running \u{b7} 24 min");
        assert_eq!(running_for(125), "Running \u{b7} 2 h 5 min");
    }

    #[test]
    fn a_take_says_how_many_files_it_took() {
        let toast = Toast::taken("demo", 1, "/srv/GSE/demo.zip".into());
        assert_eq!(toast.title, "Took 1 file from demo");
        assert!(!toast.error);
        assert_eq!(toast.text(), "Took 1 file from demo: /srv/GSE/demo.zip");
        assert_eq!(
            Toast::taken("demo", 3, "a.zip".into()).title,
            "Took 3 files from demo"
        );
    }
}
