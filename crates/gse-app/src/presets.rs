//! The presets view: the presets images are built from, whether each has
//! an image, and an editor for them. The service keeps the presets and
//! builds the images; a build's steps reach the app as service events while
//! it runs, so every open app shows them. The service also keeps when a
//! build started and the last failed build of each set, which stays marked
//! on its set with the build's console a click away.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use gpui::{
    Context, Div, Entity, FontWeight, PathPromptOptions, SharedString, div, prelude::*, px,
};
use gse_protocol::{
    AppSource, BASE_IMAGE, Build, Event, MAX_TITLE, Preset, PresetApp, Request, valid_name,
};

use crate::Home;
use crate::client::{self, Unanswered};
use crate::icons::Icon;
use crate::settings::{Choice, Picker, page_title};
use crate::text_input::{Look, TextInput};
use crate::ui::{self, Kind, LineLook, Size, Theme, Tone, button, segment, step_line};

/// The columns of the editor's list of apps after the name: where each
/// comes from, the account it runs as, and the way to take it out.
const EDITOR_FROM: f32 = 90.;
const EDITOR_RUNS_AS: f32 = 150.;
const EDITOR_REMOVE: f32 = 28.;

/// The steps a build reports for a preset of `apps`, in order: their names
/// as the build VM's console reports them and their labels. Steps for a
/// kind of app the preset lacks do not happen.
fn build_plan(apps: &[PresetApp]) -> Vec<(&'static str, &'static str)> {
    let any = |kind: fn(&AppSource) -> bool| apps.iter().any(|a| kind(&a.source));
    let mut plan = vec![("prepare", "Prepare")];
    if any(|s| matches!(s, AppSource::Dnf { .. })) {
        plan.push(("packages", "Packages"));
    }
    if any(|s| matches!(s, AppSource::Flatpak { .. })) {
        plan.push(("flatpaks", "Flatpaks"));
    }
    if any(|s| matches!(s, AppSource::Appimage { .. })) {
        plan.push(("appimages", "AppImages"));
    }
    plan.push(("menu", "Menu"));
    plan.push(("cleanup", "Finish"));
    plan
}

/// The steps a build of the base image reports, in order, with their
/// labels: the download of Fedora, then the steps of the provisioning
/// script, then flattening the image.
const BASE_PLAN: [(&str, &str); 13] = [
    ("prepare", "Prepare"),
    ("download", "Download"),
    ("update", "Update"),
    ("RPM Fusion", "RPM Fusion"),
    ("NVIDIA driver", "Driver"),
    ("desktop", "Desktop"),
    ("accounts", "Accounts"),
    ("login", "Login"),
    ("guest runtime", "Runtime"),
    ("flatpak", "Flatpak"),
    ("sandboxes", "Sandboxes"),
    ("cleanup", "Clean up"),
    ("flatten", "Finish"),
];

/// What a build is called to the user: a set by its title, the base image
/// as the sandbox system.
pub(crate) fn build_name(preset: &str) -> String {
    if preset == BASE_IMAGE {
        "The sandbox system".to_owned()
    } else {
        format!("The set \"{}\"", set_title(preset))
    }
}

/// The titles of the sets the service last listed, by their names. Sets
/// are named from places that hold only a set's name, such as the set a
/// sandbox is made from, so the titles are kept apart from the window.
static TITLES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// The titles of `presets` that have one, by their names.
fn titles(presets: &[Preset]) -> BTreeMap<String, String> {
    presets
        .iter()
        .filter_map(|p| Some((p.name.clone(), p.title.clone()?)))
        .collect()
}

/// What the set `name` is called to the user: its title, or its name when
/// it has none.
fn title_in(titles: &BTreeMap<String, String>, name: &str) -> String {
    titles.get(name).cloned().unwrap_or_else(|| name.to_owned())
}

/// What the set `name` is called to the user: the title the service last
/// listed for it, or its name when it has none.
pub(crate) fn set_title(name: &str) -> String {
    title_in(
        &TITLES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        name,
    )
}

/// A failed build: of which set, or of the sandbox system, the step it
/// stopped at, and its console.
pub(crate) struct Failure {
    pub(crate) preset: String,
    pub(crate) step: Option<&'static str>,
    pub(crate) log: Option<PathBuf>,
}

/// A name no set or image in `taken` has for a new set titled `title`: its
/// letters and digits in lowercase with hyphens between its words,
/// numbered when taken, as `wow-addons` and `wow-addons-2`.
fn free_name(title: &str, taken: &[String]) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    // Room for a number after it.
    let mut stem = words.join("-");
    stem.truncate(28);
    let stem = match stem.trim_end_matches('-') {
        "" => "set",
        stem => stem,
    };
    let free = |name: &String| valid_name(name) && name != BASE_IMAGE && !taken.contains(name);
    // One more than are taken finds a free one.
    std::iter::once(stem.to_owned())
        .chain((2..=taken.len() + 2).map(|n| format!("{stem}-{n}")))
        .find(free)
        .unwrap_or_default()
}

/// When the build `build` started, as the service says it.
fn started_at(build: &Build) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_opt(i64::try_from(build.started?).ok()?, 0)
        .single()
}

/// A time in whole minutes as a build's row says it: none under a minute.
fn minutes_text(minutes: i64) -> Option<String> {
    match minutes {
        ..=0 => None,
        1..60 => Some(format!("{minutes} min")),
        _ => Some(format!("{} h {} min", minutes / 60, minutes % 60)),
    }
}

/// The state of the presets view.
pub struct Presets {
    /// The presets, or why they are unavailable.
    pub(crate) list: Result<Vec<Preset>, SharedString>,
    /// The image build in progress, as the service last reported it.
    pub(crate) build: Option<Build>,
    /// The preset whose build the user asked to stop.
    pub(crate) stopping: Option<String>,
    pub(crate) editor: Option<Editor>,
    /// A preset whose delete or rebuild waits for a second click.
    pub(crate) confirm: Option<(String, Confirm)>,
    /// The base image every set is built on, as the service reports it.
    pub(crate) base: Option<gse_protocol::BaseImage>,
    /// The sets built on an older base image.
    pub(crate) stale: Vec<String>,
    /// The sets the service ships with, offered when a new set is made.
    pub(crate) templates: Vec<Preset>,
    /// A new set is being started: the recommended sets are shown.
    pub(crate) choosing: bool,
    /// When the build in progress started, as the service says it or the
    /// app saw it.
    pub(crate) since: Option<chrono::DateTime<chrono::Local>>,
    /// The last failed build of each set and of the sandbox system, each
    /// until the next build of it.
    pub(crate) failed: Vec<Failure>,
    /// The service does not know sets of apps: it is older than this app.
    pub(crate) old_service: bool,
    /// The time a build has taken is drawn again now and then.
    ticking: bool,
}

impl Default for Presets {
    fn default() -> Self {
        Self {
            list: Err("Loading the sets of apps...".into()),
            build: None,
            stopping: None,
            editor: None,
            confirm: None,
            base: None,
            stale: Vec::new(),
            templates: Vec::new(),
            choosing: false,
            since: None,
            failed: Vec::new(),
            old_service: false,
            ticking: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirm {
    Delete,
    Build,
}

/// Where a new app comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Flathub,
    Fedora,
    AppImage,
}

/// A preset being created or changed, with the form for a new app.
pub(crate) struct Editor {
    /// The preset's name when it exists; a new one is named after its title
    /// when it is saved.
    pub(crate) existing: Option<String>,
    /// The recommended set a new one starts from, whose name it takes
    /// while that is free.
    pub(crate) template: Option<String>,
    /// The preset's title.
    pub(crate) name: Entity<TextInput>,
    pub(crate) apps: Vec<PresetApp>,
    pub(crate) source: SourceKind,
    pub(crate) app_name: Entity<TextInput>,
    pub(crate) flathub_id: Entity<TextInput>,
    pub(crate) packages: Entity<TextInput>,
    pub(crate) command: Entity<TextInput>,
    pub(crate) desktop: Entity<TextInput>,
    pub(crate) appimage: Option<PathBuf>,
    pub(crate) trusted: bool,
    /// Why the last add or save was refused.
    pub(crate) problem: Option<SharedString>,
    pub(crate) saving: bool,
    /// What the Flathub search last asked for, and what it found.
    pub(crate) searched: String,
    pub(crate) found: Result<Vec<crate::flathub::Found>, String>,
}

/// Turns a failed presets request into words for the user.
fn explain(error: &anyhow::Error) -> SharedString {
    if error.downcast_ref::<Unanswered>().is_some() {
        "The running gse service does not know sets of apps yet. \
         Restart the service to see and edit them here."
            .into()
    } else {
        format!("{error:#}").into()
    }
}

/// What a source is called and what it names.
fn source_text(source: &AppSource) -> (&'static str, String) {
    match source {
        AppSource::Flatpak { id } => ("Flathub", id.clone()),
        AppSource::Dnf { packages, .. } => ("Fedora", packages.join(" ")),
        AppSource::Appimage { path } => ("AppImage", path.display().to_string()),
    }
}

/// The way back from a page of a set to the list of sets, its text `size`
/// pixels.
fn back_link(id: &'static str, size: f32) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .text_size(px(size))
        .text_color(Theme::muted())
        .cursor_pointer()
        .hover(|b| b.text_color(Theme::text()))
        .child("\u{2190} Sets of apps")
}

fn field(label: &'static str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(Theme::muted())
                .child(label),
        )
        .child(control)
}

impl Editor {
    /// The app the form describes, or what is missing from it.
    fn new_app(&self, cx: &Context<Home>) -> Result<PresetApp, &'static str> {
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_owned();
        let name = text(&self.app_name);
        let mut command = Vec::new();
        let mut desktop = None;
        let source = match self.source {
            SourceKind::Flathub => {
                let id = text(&self.flathub_id);
                if id.is_empty() {
                    return Err("Enter the app's Flathub ID, such as com.discordapp.Discord.");
                }
                AppSource::Flatpak { id }
            }
            SourceKind::Fedora => {
                let packages: Vec<String> = text(&self.packages)
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .collect();
                if packages.is_empty() {
                    return Err("Enter the packages to install, such as steam.");
                }
                command = text(&self.command)
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                let Some(program) = command.first() else {
                    return Err("Enter the command that starts the app.");
                };
                // The package's own menu entry would start the app as
                // `sandbox`; the preset's entry has to take its name.
                let given = text(&self.desktop);
                desktop = Some(if given.is_empty() {
                    let program = program.rsplit('/').next().unwrap_or(program);
                    format!("{program}.desktop")
                } else if given.ends_with(".desktop") {
                    given
                } else {
                    format!("{given}.desktop")
                });
                AppSource::Dnf {
                    packages,
                    repos: Vec::new(),
                }
            }
            SourceKind::AppImage => {
                let Some(path) = self.appimage.clone() else {
                    return Err("Choose the AppImage file.");
                };
                AppSource::Appimage { path }
            }
        };
        if name.is_empty() {
            return Err("Name the app.");
        }
        if self.apps.iter().any(|a| a.name.eq_ignore_ascii_case(&name)) {
            return Err("The set has an app of that name.");
        }
        Ok(PresetApp {
            name,
            source,
            desktop,
            command,
            trusted: self.trusted,
        })
    }
}

impl Home {
    pub(crate) fn reload_presets(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (loaded, templates) = cx
                .background_executor()
                .spawn(async { (client::presets(), client::preset_templates()) })
                .await;
            this.update(cx, |home, cx| {
                match loaded {
                    Ok(state) => {
                        home.presets.old_service = false;
                        home.load_presets(state);
                    }
                    Err(e) => {
                        home.presets.old_service = e.downcast_ref::<Unanswered>().is_some();
                        home.presets.list = Err(explain(&e));
                    }
                }
                // A service without templates leaves only "make your own".
                home.presets.templates = templates.unwrap_or_default();
                home.time_build(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Takes the sets, the build in progress, the base image and the failed
    /// builds as the service reported them.
    pub(crate) fn load_presets(&mut self, state: client::PresetsState) {
        // A build whose start neither the service says nor the app saw has
        // taken no time it knows of.
        let preset = |build: &Option<Build>| build.as_ref().map(|b| b.preset.clone());
        match state.build.as_ref().and_then(started_at) {
            Some(started) => self.presets.since = Some(started),
            None if preset(&state.build) != preset(&self.presets.build) => {
                self.presets.since = None;
            }
            None => {}
        }
        *TITLES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = titles(&state.presets);
        self.presets.list = Ok(state.presets);
        self.presets.build = state.build;
        self.presets.base = state.base;
        self.presets.stale = state.stale;
        self.presets.failed = state
            .failed
            .into_iter()
            .map(|failed| Failure {
                step: failed
                    .step
                    .as_deref()
                    .and_then(|step| self.build_step_label(&failed.preset, step)),
                preset: failed.preset,
                log: failed.log,
            })
            .collect();
    }

    /// Follows the image build through the service's events.
    pub(crate) fn preset_event(&mut self, event: &Event) {
        match event {
            Event::Build {
                preset,
                step,
                download,
            } => {
                if step == "prepare" {
                    self.presets.since = Some(crate::clock::now());
                }
                self.presets.failed.retain(|f| f.preset != *preset);
                let build = self.presets.build.get_or_insert_with(|| Build {
                    preset: preset.clone(),
                    steps: Vec::new(),
                    started: None,
                });
                if build.preset != *preset {
                    *build = Build {
                        preset: preset.clone(),
                        steps: Vec::new(),
                        started: None,
                    };
                }
                // The build's banner and line show each step; only its end is
                // worth a message.
                build.steps.push(step.clone());
                if preset == BASE_IMAGE {
                    self.follow_download(*download);
                }
            }
            Event::BuildFinished { preset, error } => {
                let stopped = self.presets.stopping.as_ref() == Some(preset);
                if error.is_some() && !stopped {
                    self.fail(preset);
                }
                self.presets.build = None;
                self.presets.since = None;
                let what = build_name(preset);
                match error {
                    _ if stopped => self.say(format!("{what}: the build was stopped"), false),
                    Some(e) => self.say(format!("{what}: {e}"), true),
                    None => self.say(format!("{what} is built"), false),
                }
            }
            _ => {}
        }
    }

    /// Marks the build of `preset` failed at the step it reached, until the
    /// service's record of it, with its console, comes.
    fn fail(&mut self, preset: &str) {
        let step = self
            .build_steps(preset)
            .and_then(|(steps, reached)| steps.get(reached).copied());
        self.presets.failed.retain(|f| f.preset != preset);
        self.presets.failed.push(Failure {
            preset: preset.to_owned(),
            step,
            log: None,
        });
    }

    /// The failed build of `preset`, unless it builds again.
    fn failure(&self, preset: &str) -> Option<&Failure> {
        if self.building(preset) {
            return None;
        }
        self.presets.failed.iter().find(|f| f.preset == preset)
    }

    /// Opens the console of the last failed build of `preset`.
    fn show_build_log(&mut self, preset: &str, cx: &mut Context<Self>) {
        if self.preview_skips("opening the build's log", cx) {
            return;
        }
        let log = self
            .failure(preset)
            .and_then(|f| f.log.clone())
            .filter(|log| log.exists());
        if let Some(log) = log {
            cx.open_with_system(&log);
        } else {
            self.say(
                format!(
                    "{}: the service kept no log of the build",
                    build_name(preset)
                ),
                true,
            );
            cx.notify();
        }
    }

    /// How long the build in progress has taken, once it has taken a
    /// minute and the app saw it start.
    fn build_took(&self) -> Option<String> {
        minutes_text((crate::clock::now() - self.presets.since?).num_minutes())
    }

    /// Keeps the time a build has taken current: while a build runs whose
    /// start the app saw, the window is drawn again every half a minute.
    fn time_build(&mut self, cx: &mut Context<Self>) {
        if crate::preview::active() || self.presets.ticking || self.presets.since.is_none() {
            return;
        }
        self.presets.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(30))
                    .await;
                let going = this
                    .update(cx, |home, cx| {
                        let going = home.presets.since.is_some();
                        home.presets.ticking = going;
                        if going {
                            cx.notify();
                        }
                        going
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        })
        .detach();
    }

    /// The names of the presets, or why they are unavailable.
    pub(crate) fn presets_list(&self) -> Result<Vec<String>, SharedString> {
        self.presets
            .list
            .as_ref()
            .map(|presets| presets.iter().map(|p| p.name.clone()).collect())
            .map_err(Clone::clone)
    }

    /// The apps of `preset`, when it is known.
    pub(crate) fn preset_apps(&self, preset: &str) -> Option<&[PresetApp]> {
        self.presets
            .list
            .as_ref()
            .ok()?
            .iter()
            .find(|p| p.name == preset)
            .map(|p| p.apps.as_slice())
    }

    /// The steps a build of `preset`'s image reports, with their labels.
    fn plan(&self, preset: &str) -> Vec<(&'static str, &'static str)> {
        if preset == BASE_IMAGE {
            BASE_PLAN.to_vec()
        } else {
            self.preset_apps(preset)
                .map_or_else(|| build_plan(&[]), build_plan)
        }
    }

    /// The label of `step`, as a build of `preset` reports it, when its
    /// line has one.
    fn build_step_label(&self, preset: &str, step: &str) -> Option<&'static str> {
        self.plan(preset)
            .into_iter()
            .find(|(name, _)| *name == step)
            .map(|(_, label)| label)
    }

    /// The steps of the build of `preset`'s image and the one it is at,
    /// while it builds.
    pub(crate) fn build_steps(&self, preset: &str) -> Option<(Vec<&'static str>, usize)> {
        let build = self.presets.build.as_ref().filter(|b| b.preset == preset)?;
        let plan = self.plan(preset);
        // A step outside the plan keeps the line where it was.
        let reached = build
            .steps
            .iter()
            .rev()
            .find_map(|step| plan.iter().position(|(name, _)| name == step))
            .unwrap_or(0);
        Some((plan.iter().map(|(_, label)| *label).collect(), reached))
    }

    /// How far the build of `preset` got, in percent, while it builds: a
    /// download's own percentage, or the share of steps passed.
    pub(crate) fn build_percent(&self, preset: &str) -> Option<u8> {
        let build = self.presets.build.as_ref().filter(|b| b.preset == preset)?;
        // The download moves the whole build's share of its step along.
        let (steps, reached) = self.build_steps(preset)?;
        let share = 100 / steps.len().max(1);
        if let Some(percent) = build
            .steps
            .last()
            .and_then(|s| s.strip_prefix("download "))
            .and_then(|p| p.trim_end_matches('%').parse::<usize>().ok())
        {
            return u8::try_from(reached * share + percent * share / 100).ok();
        }
        u8::try_from(reached * 100 / steps.len().max(1)).ok()
    }

    /// What a set is now, in a few words and a colour: being built (while
    /// an older build may still serve), built, or not built.
    pub(crate) fn set_state(&self, preset: &str) -> (&'static str, gpui::Hsla) {
        let built = self.images.iter().any(|i| i == preset);
        if built && !self.building(preset) && self.presets.stale.iter().any(|s| s == preset) {
            return ("Out of date", Theme::warning());
        }
        match (self.building(preset), built) {
            (true, true) => ("Ready, being rebuilt", Theme::warning()),
            (true, false) => ("Being built", Theme::warning()),
            (false, true) => ("Ready", Theme::good()),
            (false, false) => ("Not built yet", Theme::subtle()),
        }
    }

    /// Whether a preset is open in the editor.
    pub(crate) fn editing_preset(&self) -> bool {
        self.presets.editor.is_some()
    }

    pub(crate) fn building(&self, preset: &str) -> bool {
        self.presets
            .build
            .as_ref()
            .is_some_and(|b| b.preset == preset)
            || self.busy.contains_key(&format!("{preset}/build"))
    }

    fn any_building(&self) -> bool {
        self.presets.build.is_some() || self.busy.keys().any(|k| k.ends_with("/build"))
    }

    pub(crate) fn build_image(&mut self, name: &str, cx: &mut Context<Self>) {
        let key = format!("{name}/build");
        self.busy.insert(key.clone(), "Building");
        self.presets.confirm = None;
        self.presets.stopping = None;
        self.presets.failed.retain(|f| f.preset != name);
        self.presets.build = Some(Build {
            preset: name.to_owned(),
            steps: Vec::new(),
            started: None,
        });
        self.presets.since = Some(crate::clock::now());
        self.time_build(cx);
        cx.notify();
        let name = name.to_owned();
        cx.spawn(async move |this, cx| {
            let request = name.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move { client::build_preset(&request) })
                .await;
            this.update(cx, |home, cx| {
                home.busy.remove(&key);
                let stopped = home.presets.stopping.take().as_ref() == Some(&name);
                let what = build_name(&name);
                // A build that failed is marked from the service's record,
                // which the reload brings; a refused one never ran.
                match outcome {
                    Err(_) if stopped => home.say(format!("{what}: the build was stopped"), false),
                    Err(e) => home.say(format!("{what}: {}", explain(&e)), true),
                    Ok(()) => home.say(format!("{what} is built"), false),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_editor(&mut self, preset: Option<&Preset>, cx: &mut Context<Self>) {
        let search = cx.new(|cx| TextInput::new("Search Flathub", cx).look(Look::Search));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        let name = cx.new(|cx| {
            TextInput::new("Name of the set, such as WoW addons", cx).look(Look::TitleField)
        });
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        let mut input = |placeholder: &'static str| {
            let input = cx.new(|cx| TextInput::new(placeholder, cx));
            cx.observe(&input, |_, _, cx| cx.notify()).detach();
            input
        };
        self.presets.confirm = None;
        self.presets.editor = Some(Editor {
            existing: preset.map(|p| p.name.clone()),
            template: None,
            name,
            apps: preset.map_or_else(Vec::new, |p| p.apps.clone()),
            source: SourceKind::Flathub,
            app_name: input("Name, such as Steam"),
            flathub_id: search.clone(),
            packages: input("steam"),
            command: input("steam"),
            desktop: input("steam.desktop"),
            appimage: None,
            trusted: false,
            problem: None,
            saving: false,
            searched: String::new(),
            found: Ok(Vec::new()),
        });
        if let Some(editor) = &self.presets.editor {
            cx.observe(&editor.flathub_id, |home, _, cx| home.search_flathub(cx))
                .detach();
            if let Some(preset) = preset {
                let title = preset.title.clone().unwrap_or_else(|| preset.name.clone());
                editor
                    .name
                    .update(cx, |input, cx| input.set_text(title, cx));
            }
        }
        cx.notify();
    }

    fn add_app(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.presets.editor.as_mut() else {
            return;
        };
        match editor.new_app(cx) {
            Ok(app) => {
                editor.apps.push(app);
                for input in [
                    &editor.app_name,
                    &editor.flathub_id,
                    &editor.packages,
                    &editor.command,
                    &editor.desktop,
                ] {
                    input.update(cx, |input, cx| input.set_text("", cx));
                }
                editor.appimage = None;
                editor.trusted = false;
                editor.problem = None;
            }
            Err(problem) => editor.problem = Some(problem.into()),
        }
        cx.notify();
    }

    fn pick_appimage(cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |home, cx| {
                match picked {
                    Ok(Ok(Some(paths))) => {
                        if let (Some(editor), Some(path)) =
                            (home.presets.editor.as_mut(), paths.into_iter().next())
                        {
                            if editor.app_name.read(cx).text().is_empty()
                                && let Some(stem) = path.file_stem()
                            {
                                let stem = stem.to_string_lossy().replace(['_', '-'], " ");
                                editor
                                    .app_name
                                    .update(cx, |input, cx| input.set_text(stem, cx));
                            }
                            editor.appimage = Some(path);
                        }
                    }
                    Ok(Err(e)) => home.say(format!("The file chooser failed: {e:#}"), true),
                    Ok(Ok(None)) | Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Saves the set being edited, and builds it after with `build`. A new
    /// set takes the name of the recommended set it starts from while that
    /// is free, or one made from its title; a set keeps its name whatever
    /// its title becomes.
    fn save_preset(&mut self, build: bool, cx: &mut Context<Self>) {
        // Every set by its name and what it is called.
        let sets: Vec<(String, String)> = self
            .presets
            .list
            .as_ref()
            .map(|list| {
                list.iter()
                    .map(|p| {
                        let title = p.title.clone().unwrap_or_else(|| p.name.clone());
                        (p.name.clone(), title)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let taken: Vec<String> = sets
            .iter()
            .map(|(name, _)| name.clone())
            .chain(self.images.iter().cloned())
            .collect();
        let Some(editor) = self.presets.editor.as_mut() else {
            return;
        };
        let title = editor.name.read(cx).text().trim().to_owned();
        let called = sets.iter().any(|(name, other)| {
            Some(name) != editor.existing.as_ref() && other.to_lowercase() == title.to_lowercase()
        });
        let problem = if title.is_empty() {
            Some("Name the set.".to_owned())
        } else if title.chars().count() > MAX_TITLE {
            Some(format!("Name the set in up to {MAX_TITLE} characters."))
        } else if called {
            Some("A set of that name exists.".to_owned())
        } else if editor.apps.is_empty() {
            Some("Add at least one app.".to_owned())
        } else {
            None
        };
        if let Some(problem) = problem {
            editor.problem = Some(problem.into());
            cx.notify();
            return;
        }
        let name = editor
            .existing
            .clone()
            .or_else(|| editor.template.clone().filter(|t| !taken.contains(t)))
            .unwrap_or_else(|| free_name(&title, &taken));
        editor.saving = true;
        editor.problem = None;
        let preset = Preset {
            name: name.clone(),
            title: Some(title.clone()),
            apps: editor.apps.clone(),
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { client::command(&Request::SavePreset { preset }) })
                .await;
            this.update(cx, |home, cx| {
                match outcome {
                    Ok(()) => {
                        home.presets.editor = None;
                        home.say(format!("The set \"{title}\" is saved"), false);
                        if build {
                            home.build_image(&name, cx);
                        }
                    }
                    Err(e) => {
                        if let Some(editor) = home.presets.editor.as_mut() {
                            editor.saving = false;
                            editor.problem = Some(explain(&e));
                        }
                    }
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The build of `preset` as a line of its steps, while it builds: a
    /// set's in small dots beside the words that name its step, the
    /// sandbox system's long one naming the step it is at.
    fn build_line(&self, preset: &str) -> Option<Div> {
        let (steps, reached) = self.build_steps(preset)?;
        let look = if preset == BASE_IMAGE {
            LineLook {
                dot: 12.,
                halo: 5.,
                pending: Theme::selected(),
                named: true,
            }
        } else {
            LineLook {
                dot: 10.,
                halo: 4.,
                pending: Theme::pending(),
                named: false,
            }
        };
        Some(step_line(
            &format!("build-{preset}"),
            &steps,
            Some(reached),
            false,
            Tone::Warning,
            look,
        ))
    }

    /// The square that stands for a set: its initial in the colour of
    /// `tone` on its ground, grey with no tone.
    fn set_mark(title: &str, tone: Option<Tone>) -> Div {
        let (ground, letter) = tone.map_or_else(
            || (Theme::control(), Theme::secondary_text()),
            |tone| (tone.ground(), tone.color()),
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(40.))
            .rounded(px(10.))
            .bg(ground)
            .text_color(letter)
            .text_base()
            .font_weight(FontWeight::BOLD)
            .child(
                title
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string(),
            )
    }

    /// The rebuild of `name` asking first, since it gives its sandboxes a
    /// new system: a line of its own under the set's row, lined up with its
    /// name, so the row keeps its apps in sight.
    fn rebuild_confirm(name: &str, users: usize, cx: &mut Context<Self>) -> Div {
        let go = name.to_owned();
        div()
            .flex()
            .items_center()
            .gap_2()
            .pl(px(56.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child(match users {
                        1 => "Its sandbox gets a new system; its files in /home stay.".to_owned(),
                        n => format!(
                            "Its {n} sandboxes get a new system; their files in /home stay."
                        ),
                    }),
            )
            .child(
                button(
                    format!("preset-cancel-{name}"),
                    "Cancel",
                    Kind::Ghost,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.presets.confirm = None;
                        cx.notify();
                    }),
                )
                .size(Size::Small),
            )
            .child(
                button(
                    format!("preset-confirm-{name}"),
                    "Rebuild",
                    Kind::Danger,
                    true,
                    cx.listener(move |home, _, _, cx| home.build_image(&go, cx)),
                )
                .size(Size::Small),
            )
    }

    /// Whether the rebuild of `name` waits for its second click.
    fn rebuild_asked(&self, name: &str) -> bool {
        self.presets
            .confirm
            .as_ref()
            .is_some_and(|(n, c)| n == name && *c == Confirm::Build)
    }

    /// The button that builds `name` again, or for the first time: it asks
    /// first when sandboxes are made from it, and waits while another
    /// build or a sandbox runs.
    fn build_button(
        &self,
        name: &str,
        label: &'static str,
        users: usize,
        cx: &mut Context<Self>,
    ) -> ui::Button {
        let free = !self.any_building() && !self.any_running();
        let built = self.images.iter().any(|i| i == name);
        let to_build = name.to_owned();
        button(
            format!("preset-build-{name}"),
            label,
            if built { Kind::Secondary } else { Kind::Accent },
            free,
            cx.listener(move |home, _, _, cx| {
                if users > 0 {
                    home.presets.confirm = Some((to_build.clone(), Confirm::Build));
                    cx.notify();
                } else {
                    home.build_image(&to_build, cx);
                }
            }),
        )
        .size(Size::Small)
        .when(!free, |b| {
            b.tooltip(if self.any_running() {
                "Sets build while no sandbox runs"
            } else {
                "Another build runs"
            })
        })
    }

    /// What can be done with `preset` now, at the right of its row: Stop
    /// while it builds; its build's log and Try again after a failure; Edit
    /// and Build or Rebuild otherwise. None while its rebuild asks first,
    /// under the row.
    fn set_actions(&self, preset: &Preset, users: usize, cx: &mut Context<Self>) -> Div {
        let name = preset.name.clone();
        let actions = div().flex().flex_none().items_center().gap_4();
        if self.rebuild_asked(&name) {
            return actions;
        }
        if self.building(&name) {
            let stopping = self.presets.stopping.as_ref() == Some(&name);
            return actions.child(
                button(
                    format!("preset-stop-{name}"),
                    if stopping { "Stopping..." } else { "Stop" },
                    Kind::Secondary,
                    !stopping,
                    cx.listener(move |home, _, _, cx| {
                        home.presets.stopping = Some(name.clone());
                        home.request(format!("{name}/stop-build"), "", Request::StopBuild, cx);
                    }),
                )
                .size(Size::Small),
            );
        }
        let failure = self.failure(&name);
        let (failed, logged) = (failure.is_some(), failure.is_some_and(|f| f.log.is_some()));
        let built = self.images.contains(&name);
        let (edit, log) = (preset.clone(), name.clone());
        actions
            .when(logged, |actions| {
                actions.child(
                    ui::link(format!("preset-log-{name}"), "Show log")
                        .on_click(cx.listener(move |home, _, _, cx| home.show_build_log(&log, cx))),
                )
            })
            // A failed set is mended on its page, so Edit stays.
            .child(
                ui::link(format!("preset-edit-{name}"), "Edit")
                    .on_click(cx.listener(move |home, _, _, cx| home.open_editor(Some(&edit), cx))),
            )
            .child(self.build_button(
                &name,
                match (failed, built) {
                    (true, _) => "Try again",
                    (false, true) => "Rebuild",
                    (false, false) => "Build",
                },
                users,
                cx,
            ))
    }

    /// What a set is now in a few words and their colour, and the tone of
    /// its mark: being built with the step it is at and the time it has
    /// taken, failed at a step, out of date, ready; nothing when it was
    /// never built, which its line of apps says.
    fn set_status(&self, name: &str) -> (Option<String>, gpui::Hsla, Option<Tone>) {
        let built = self.images.iter().any(|i| i == name);
        if self.building(name) {
            let step = self
                .build_steps(name)
                .and_then(|(steps, reached)| steps.get(reached).copied());
            let words: Vec<String> = [
                Some(if built { "Rebuilding" } else { "Building" }.to_owned()),
                step.map(str::to_owned),
                self.build_took(),
            ]
            .into_iter()
            .flatten()
            .collect();
            return (
                Some(words.join(" \u{b7} ")),
                Theme::warning_text(),
                Some(Tone::Warning),
            );
        }
        if let Some(failure) = self.failure(name) {
            let text = failure.step.map_or_else(
                || "Build failed".to_owned(),
                |step| format!("Build failed at {step}"),
            );
            return (Some(text), Theme::danger_text(), Some(Tone::Danger));
        }
        if !built {
            (None, Theme::muted(), None)
        } else if self.presets.stale.iter().any(|s| s == name) {
            (
                Some("Out of date".to_owned()),
                Theme::warning(),
                Some(Tone::Accent),
            )
        } else {
            (Some("Ready".to_owned()), Theme::good(), Some(Tone::Accent))
        }
    }

    /// One set in the list: its mark, apps and users, state and actions,
    /// and its build's steps while it builds.
    fn set_row(&self, preset: &Preset, cx: &mut Context<Self>) -> Div {
        let name = &preset.name;
        let title = set_title(name);
        let users = self
            .environments
            .iter()
            .filter(|e| e.image == *name)
            .count();
        let built = self.images.contains(name);
        let building = self.building(name);
        let (state, color, tone) = self.set_status(name);
        let apps: Vec<&str> = preset.apps.iter().map(|a| a.name.as_str()).collect();
        // Sandboxes made from a set that is not built wait for it.
        let used = match users {
            0 if state.is_none() => "not built yet".to_owned(),
            0 => "no sandboxes".to_owned(),
            1 if !built => "1 sandbox waiting".to_owned(),
            1 => "1 sandbox".to_owned(),
            n if !built => format!("{n} sandboxes waiting"),
            n => format!("{n} sandboxes"),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .bg(Theme::surface())
            .when(building, |r| {
                r.border_1().border_color(Theme::warning().opacity(0.25))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(Self::set_mark(&title, tone))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_base()
                                    // A browser's line of 16 px Inter, so the
                                    // apps sit under it where the design has them.
                                    .line_height(px(20.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(Theme::muted())
                                    .child(format!("{} \u{b7} {used}", apps.join(", "))),
                            ),
                    )
                    .children(
                        state.map(|state| {
                            div().flex_none().text_xs().text_color(color).child(state)
                        }),
                    )
                    .child(self.set_actions(preset, users, cx)),
            )
            .when(self.rebuild_asked(name), |row| {
                row.child(Self::rebuild_confirm(name, users, cx))
            })
            .children(self.build_line(name))
    }

    /// What can be done with the sandbox system now: Stop while it builds;
    /// its build's log and Try again after a failure; Update everything
    /// while it is out of date; Rebuild otherwise.
    fn base_actions(&self, cx: &mut Context<Self>) -> Div {
        let base = BASE_IMAGE;
        let actions = div().flex().flex_none().items_center().gap_4();
        if self.building(base) {
            let stopping = self.presets.stopping.as_deref() == Some(base);
            return actions.child(
                button(
                    "base-stop",
                    if stopping { "Stopping..." } else { "Stop" },
                    Kind::Secondary,
                    !stopping,
                    cx.listener(move |home, _, _, cx| {
                        home.presets.stopping = Some(base.to_owned());
                        home.request(format!("{base}/stop-build"), "", Request::StopBuild, cx);
                    }),
                )
                .size(Size::Small),
            );
        }
        let free = !self.any_building() && !self.any_running();
        let failure = self.failure(base);
        let (failed, logged) = (failure.is_some(), failure.is_some_and(|f| f.log.is_some()));
        let action = if self.base_outdated() && !failed {
            button(
                "base-update",
                "Update everything",
                Kind::Primary,
                free,
                cx.listener(|home, _, _, cx| home.update_everything(cx)),
            )
        } else {
            button(
                "base-rebuild",
                if failed { "Try again" } else { "Rebuild" },
                Kind::Secondary,
                free,
                cx.listener(|home, _, _, cx| home.build_base(cx)),
            )
        };
        actions
            .when(logged, |actions| {
                actions.child(
                    ui::link("base-log", "Show log")
                        .on_click(cx.listener(move |home, _, _, cx| home.show_build_log(base, cx))),
                )
            })
            .child(action.size(Size::Small).when(!free, |b| {
                b.tooltip(if self.any_running() {
                    "The system builds while no sandbox runs"
                } else {
                    "Another build runs"
                })
            }))
    }

    /// The sandbox system every set is built on, with a way to rebuild it.
    fn base_row(&self, cx: &mut Context<Self>) -> Div {
        let base = BASE_IMAGE;
        let building = self.building(base);
        let built = self
            .presets
            .base
            .as_ref()
            .and_then(|base| {
                use chrono::TimeZone as _;
                chrono::Local
                    .timestamp_opt(i64::try_from(base.built).ok()?, 0)
                    .single()
            })
            .map(|t| t.format("built %-d %b").to_string());
        let outdated = self.base_outdated();
        let (state, color) = if building {
            match self.build_percent(base) {
                Some(p) => (format!("Building, {p}%"), Theme::warning()),
                None => ("Building".to_owned(), Theme::warning()),
            }
        } else if let Some(failure) = self.failure(base) {
            (
                failure.step.map_or_else(
                    || "Build failed".to_owned(),
                    |step| format!("Build failed at {step}"),
                ),
                Theme::danger_text(),
            )
        } else if outdated {
            ("Out of date".to_owned(), Theme::warning())
        } else {
            (
                built.unwrap_or_else(|| "Not built".to_owned()),
                Theme::muted(),
            )
        };
        let row = div()
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Sandbox system"),
                    )
                    .child(div().mt(px(2.)).text_xs().text_color(Theme::muted()).child(
                        "The desktop and drivers every set is built on. Rebuild it after a \
                         GSE update, then rebuild the sets.",
                    )),
            )
            .child(div().flex_none().text_xs().text_color(color).child(state))
            .child(self.base_actions(cx));
        div()
            .flex()
            .flex_col()
            .gap_3p5()
            .px_4()
            .py(px(if building { 14. } else { 12. }))
            .rounded(px(12.))
            .border_1()
            .border_color(if building || outdated {
                Theme::warning().opacity(0.3)
            } else {
                Theme::border()
            })
            .child(row)
            .children(self.build_line(base))
    }

    /// Whether the sandbox system lacks what this service puts into it,
    /// so it and every set built on it want rebuilding.
    pub(crate) fn base_outdated(&self) -> bool {
        self.presets.base.as_ref().is_some_and(|b| !b.current)
    }

    /// Whether anything wants rebuilding: the sandbox system, or a set built
    /// on an older one.
    pub(crate) fn anything_outdated(&self) -> bool {
        self.base_outdated() || !self.presets.stale.is_empty()
    }

    /// Rebuilds the sandbox system when it is out of date, then every set
    /// built on an older one, one after another; the sandboxes made from a
    /// set get its new build as their system, their /home kept. What is left to do is asked of the
    /// service each time, so the work picks up where it stopped.
    pub(crate) fn update_everything(&mut self, cx: &mut Context<Self>) {
        let base = self.base_outdated();
        let key = "everything".to_owned();
        self.busy.insert(key.clone(), "Updating");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    if base {
                        client::build_base()?;
                    }
                    for set in client::presets()?.stale {
                        client::build_preset(&set)?;
                    }
                    anyhow::Ok(())
                })
                .await;
            this.update(cx, |home, cx| {
                home.busy.remove(&key);
                match outcome {
                    Ok(()) => home.say("The sandbox system and every set are up to date", false),
                    Err(e) => home.say(format!("Updating stopped: {}", explain(&e)), true),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Rebuilds the sandbox system through the service.
    fn build_base(&mut self, cx: &mut Context<Self>) {
        let base = BASE_IMAGE;
        self.busy.insert(format!("{base}/build"), "Building");
        self.presets.stopping = None;
        self.presets.failed.retain(|f| f.preset != base);
        self.presets.build = Some(Build {
            preset: base.to_owned(),
            steps: Vec::new(),
            started: None,
        });
        self.presets.since = Some(crate::clock::now());
        self.time_build(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { client::build_base() })
                .await;
            this.update(cx, |home, cx| {
                home.busy.remove(&format!("{base}/build"));
                let stopped = home.presets.stopping.take().as_deref() == Some(base);
                match outcome {
                    Ok(()) => home.say(
                        "The sandbox system is rebuilt; rebuild the sets to use it",
                        false,
                    ),
                    Err(_) if stopped => {
                        home.say("The sandbox system: the build was stopped", false);
                    }
                    Err(e) => home.say(format!("The sandbox system: {}", explain(&e)), true),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Searches Flathub for what the editor's search field holds.
    fn search_flathub(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.presets.editor.as_mut() else {
            return;
        };
        let query = editor.flathub_id.read(cx).text().trim().to_owned();
        if query == editor.searched {
            return;
        }
        editor.searched.clone_from(&query);
        if query.chars().count() < 2 {
            editor.found = Ok(Vec::new());
            cx.notify();
            return;
        }
        // A preview reaches no network; it finds what it makes up.
        if crate::preview::active() {
            editor.found = Ok(crate::preview::flathub(&query));
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            // Typing goes on; only the search for what stays in the field
            // is worth asking for.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(350))
                .await;
            let still = this
                .update(cx, |home, cx| {
                    home.presets
                        .editor
                        .as_ref()
                        .is_some_and(|e| e.flathub_id.read(cx).text().trim() == query)
                })
                .unwrap_or(false);
            if !still {
                return;
            }
            let asked = query.clone();
            let found = cx
                .background_executor()
                .spawn(async move { crate::flathub::search(&asked) })
                .await;
            this.update(cx, |home, cx| {
                if let Some(editor) = home.presets.editor.as_mut()
                    && editor.searched == query
                {
                    editor.found = found.map_err(|e| format!("{e:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Puts a Flathub app into the set being edited.
    fn add_found(&mut self, found: &crate::flathub::Found, cx: &mut Context<Self>) {
        if let Some(editor) = self.presets.editor.as_mut()
            && !editor
                .apps
                .iter()
                .any(|a| matches!(&a.source, AppSource::Flatpak { id } if *id == found.app_id))
        {
            editor.apps.push(PresetApp {
                name: found.name.clone(),
                source: AppSource::Flatpak {
                    id: found.app_id.clone(),
                },
                desktop: None,
                command: Vec::new(),
                trusted: false,
            });
            cx.notify();
        }
    }

    /// One app of the set being edited: its name, source, account and a
    /// way to take it out.
    fn editor_app_row(
        &self,
        index: usize,
        app: &PresetApp,
        locked: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let (from, _) = source_text(&app.source);
        let runs = self.menu(
            Picker::new(
                format!("runs-as-{index}"),
                if app.trusted {
                    "sandbox (trusted)"
                } else {
                    "guest (untrusted)"
                },
            )
            .size(Size::Row)
            .wide()
            .enabled(!locked),
            vec![
                Choice::new("guest (untrusted)", false).current(!app.trusted),
                Choice::new("sandbox (trusted)", true).current(app.trusted),
            ],
            move |home, trusted: bool, cx| {
                if let Some(app) = home
                    .presets
                    .editor
                    .as_mut()
                    .and_then(|e| e.apps.get_mut(index))
                {
                    app.trusted = trusted;
                    cx.notify();
                }
            },
            cx,
        );
        div()
            .flex()
            .items_center()
            .px_3p5()
            .py_2p5()
            .border_t_1()
            .border_color(Theme::divider())
            .text_sm()
            .child(div().flex_1().min_w_0().truncate().child(app.name.clone()))
            .child(
                div()
                    .w(px(EDITOR_FROM))
                    .flex_none()
                    .text_xs()
                    .text_color(Theme::secondary_text())
                    .child(from),
            )
            .child(div().w(px(EDITOR_RUNS_AS)).flex_none().child(runs))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(px(EDITOR_REMOVE))
                    .child(
                        ui::small_icon_button(
                            SharedString::from(format!("app-remove-{index}")),
                            Icon::Close,
                            "Take it out of the set",
                            Kind::Ghost,
                            cx.listener(move |home, _, _, cx| {
                                if let Some(editor) = home.presets.editor.as_mut()
                                    && index < editor.apps.len()
                                {
                                    editor.apps.remove(index);
                                    cx.notify();
                                }
                            }),
                        )
                        .disabled(locked),
                    ),
            )
    }

    /// The left column of the editor: the apps in the set.
    fn editor_apps(&self, editor: &Editor, cx: &mut Context<Self>) -> Div {
        let locked = editor.saving;
        let mut list = div()
            .id("editor-apps")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        if editor.apps.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_sm()
                    .text_color(Theme::subtle())
                    .child("No apps yet. Add them from the right."),
            );
        }
        for (index, app) in editor.apps.iter().enumerate() {
            list = list.child(self.editor_app_row(index, app, locked, cx));
        }
        let head = |text: &'static str, w: Option<f32>| {
            let cell = div().text_xs().text_color(Theme::subtle()).child(text);
            match w {
                Some(w) => cell.w(px(w)).flex_none(),
                None => cell.flex_1(),
            }
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .rounded(px(12.))
            .bg(Theme::surface())
            .child(
                div()
                    .px_3p5()
                    .py_3()
                    .text_size(px(13.))
                    .text_color(Theme::muted())
                    .border_b_1()
                    .border_color(Theme::border())
                    .child(format!("In this set \u{b7} {}", editor.apps.len())),
            )
            .child(
                div()
                    .flex()
                    .px_3p5()
                    .py_2()
                    .child(head("App", None))
                    .child(head("From", Some(EDITOR_FROM)))
                    .child(head("Runs as", Some(EDITOR_RUNS_AS)))
                    .child(div().w(px(EDITOR_REMOVE)).flex_none()),
            )
            .child(list)
            .child(
                div()
                    .px_3p5()
                    .py_3()
                    .border_t_1()
                    .border_color(Theme::border())
                    .text_xs()
                    .text_color(Theme::muted())
                    .child("guest can't see the files of sandbox. Put anything you don't trust under guest."),
            )
    }

    /// The Flathub tab of the editor: a search and what it found.
    fn flathub_results(editor: &Editor, cx: &mut Context<Self>) -> Div {
        let mut results = div().flex().flex_col();
        match &editor.found {
            Err(e) => {
                results = results.child(
                    div()
                        .px_3p5()
                        .py_2()
                        .text_sm()
                        .text_color(Theme::warning())
                        .child(e.clone()),
                );
            }
            Ok(found) if found.is_empty() => {
                results = results.child(
                    div()
                        .px_3p5()
                        .py_2()
                        .text_sm()
                        .text_color(Theme::subtle())
                        .child(if editor.searched.chars().count() < 2 {
                            "Type the name of an app."
                        } else {
                            "Nothing found."
                        }),
                );
            }
            Ok(found) => {
                for (i, app) in found.iter().enumerate() {
                    let added = editor.apps.iter().any(
                        |a| matches!(&a.source, AppSource::Flatpak { id } if *id == app.app_id),
                    );
                    let pick = app.clone();
                    results = results.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .px_3p5()
                            .py_2p5()
                            .border_t_1()
                            .border_color(Theme::divider())
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().truncate().child(app.name.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(Theme::muted())
                                            .truncate()
                                            .child(app.app_id.clone()),
                                    ),
                            )
                            .child(
                                button(
                                    SharedString::from(format!("found-add-{i}")),
                                    if added { "Added" } else { "Add" },
                                    Kind::Accent,
                                    !added,
                                    cx.listener(move |home, _, _, cx| home.add_found(&pick, cx)),
                                )
                                .size(Size::Row),
                            ),
                    );
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(div().px_2p5().pb_2p5().child(editor.flathub_id.clone()))
            .child(
                div()
                    .id("flathub-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(results),
            )
    }

    /// The Fedora and AppImage tabs of the editor: a short form.
    fn manual_form(editor: &Editor, cx: &mut Context<Self>) -> Div {
        let form = div().flex().flex_col().gap_3().px_3p5().pb_3p5();
        let form = if editor.source == SourceKind::Fedora {
            form.child(field("Name", editor.app_name.clone()))
                .child(field("Packages", editor.packages.clone()))
                .child(field("Command", editor.command.clone()))
                .child(field(
                    "Menu entry it replaces (optional)",
                    editor.desktop.clone(),
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(Theme::subtle())
                        .child("Packages come from Fedora and RPM Fusion."),
                )
        } else {
            // The file's name in full where it fits, with its folder under
            // it; a long folder gives up its start, which matters least.
            let chosen = editor.appimage.as_ref().map_or_else(
                || {
                    div()
                        .text_sm()
                        .text_color(Theme::muted())
                        .child("No file chosen")
                },
                |path| {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    let folder = path
                        .parent()
                        .map(crate::config::shown_path)
                        .unwrap_or_default();
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_middle()
                                .text_sm()
                                .child(name),
                        )
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_start()
                                .text_xs()
                                .text_color(Theme::muted())
                                .child(folder),
                        )
                },
            );
            form.child(field("Name", editor.app_name.clone())).child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(button(
                        "app-choose-file",
                        "Choose file...",
                        Kind::Secondary,
                        true,
                        cx.listener(|_, _, _, cx| Self::pick_appimage(cx)),
                    ))
                    .child(div().flex_1().min_w_0().child(chosen)),
            )
        };
        form.child(div().flex().justify_end().child(button(
            "app-add",
            "Add",
            Kind::Primary,
            true,
            cx.listener(|home, _, _, cx| home.add_app(cx)),
        )))
    }

    /// The right column of the editor: where apps come from.
    fn editor_sources(editor: &Editor, cx: &mut Context<Self>) -> Div {
        let source = editor.source;
        let tab =
            |id: &'static str, label: &'static str, kind: SourceKind, cx: &mut Context<Self>| {
                segment(
                    id,
                    label,
                    source == kind,
                    true,
                    cx.listener(move |home, _, _, cx| {
                        if let Some(editor) = home.presets.editor.as_mut() {
                            editor.source = kind;
                            editor.problem = None;
                            cx.notify();
                        }
                    }),
                )
                .flex_1()
                .when(source == kind, |t| t.font_weight(FontWeight::SEMIBOLD))
            };
        let body = match source {
            SourceKind::Flathub => Self::flathub_results(editor, cx),
            _ => Self::manual_form(editor, cx),
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(360.))
            .rounded(px(12.))
            .bg(Theme::surface())
            .child(
                div()
                    .flex()
                    .gap_1()
                    .p_2p5()
                    .child(tab("source-flathub", "Flathub", SourceKind::Flathub, cx))
                    .child(tab("source-fedora", "Fedora", SourceKind::Fedora, cx))
                    .child(tab("source-appimage", "AppImage", SourceKind::AppImage, cx)),
            )
            .child(body)
            .when(source == SourceKind::Flathub, |c| {
                c.child(
                    div()
                        .px_3p5()
                        .py_3()
                        .border_t_1()
                        .border_color(Theme::border())
                        .text_xs()
                        .text_color(Theme::muted())
                        .child("AppImage takes a file from this computer."),
                )
            })
    }

    /// The page of the set being made or changed. Its two panels take the
    /// height the window leaves them, down to a floor under which the page
    /// scrolls instead.
    fn editor_page(&self, editor: &Editor, cx: &mut Context<Self>) -> Div {
        let locked = editor.saving;
        // The set's title; its name stays what it was made with.
        let name_field = div().flex_1().min_w_0().child(editor.name.clone());
        let delete = editor.existing.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .gap_4()
            .child(
                back_link("editor-back", 13.).on_click(cx.listener(|home, _, _, cx| {
                    home.presets.editor = None;
                    cx.notify();
                })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(name_field)
                    .child(
                        button(
                            "preset-editor-save",
                            if locked { "Saving..." } else { "Save" },
                            Kind::Secondary,
                            !locked,
                            cx.listener(|home, _, _, cx| home.save_preset(false, cx)),
                        )
                        .size(Size::Dialog)
                        .px_4()
                        .rounded(px(9.)),
                    )
                    .child(
                        button(
                            "preset-editor-build",
                            "Save and build",
                            Kind::Primary,
                            !locked && !self.any_building() && !self.any_running(),
                            cx.listener(|home, _, _, cx| home.save_preset(true, cx)),
                        )
                        .size(Size::Dialog)
                        .px_4()
                        .rounded(px(9.)),
                    ),
            )
            .children(
                editor
                    .problem
                    .clone()
                    .map(|problem| div().text_sm().text_color(Theme::danger()).child(problem)),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(280.))
                    .gap_4()
                    .child(self.editor_apps(editor, cx))
                    .child(Self::editor_sources(editor, cx)),
            )
            .children(delete.map(|name| self.delete_zone(&name, locked, cx)))
    }

    /// Deleting the set being edited, asking a second time; a set some
    /// sandbox uses stays.
    fn delete_zone(&self, name: &str, locked: bool, cx: &mut Context<Self>) -> Div {
        let users = self.environments.iter().filter(|e| e.image == name).count();
        let waiting = self
            .presets
            .confirm
            .as_ref()
            .is_some_and(|(n, c)| *n == name && *c == Confirm::Delete);
        let gone = name.to_owned();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_3p5()
            .py_3()
            .rounded(px(12.))
            .border_1()
            .border_color(Theme::danger().opacity(0.3))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .text_color(Theme::muted())
                    .child(match users {
                        0 => "Delete the set and what it built.".to_owned(),
                        1 => "1 sandbox is made from it; delete that first.".to_owned(),
                        n => format!("{n} sandboxes are made from it; delete them first."),
                    }),
            )
            .child(button(
                "preset-delete",
                if waiting { "Delete now" } else { "Delete" },
                Kind::DangerFilled,
                !locked && users == 0,
                cx.listener(move |home, _, _, cx| {
                    if waiting {
                        home.presets.confirm = None;
                        home.presets.editor = None;
                        home.request(
                            format!("{gone}/delete-preset"),
                            "Deleting",
                            Request::DeletePreset { name: gone.clone() },
                            cx,
                        );
                    } else {
                        home.presets.confirm = Some((gone.clone(), Confirm::Delete));
                        cx.notify();
                    }
                }),
            ))
    }

    /// Starts a new set: the recommended sets first, when the service has
    /// any, otherwise an empty editor.
    pub(crate) fn new_set(&mut self, cx: &mut Context<Self>) {
        if self.presets.templates.is_empty() {
            self.open_editor(None, cx);
        } else {
            self.presets.choosing = true;
            cx.notify();
        }
    }

    /// Opens the editor on a new set with the apps of `template`, titled
    /// after it, with a number while a set has that title.
    fn start_from(&mut self, template: &Preset, cx: &mut Context<Self>) {
        let called: Vec<String> = self
            .presets
            .list
            .as_ref()
            .map(|list| {
                list.iter()
                    .map(|p| p.title.as_ref().unwrap_or(&p.name).to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let first = template.title.as_ref().unwrap_or(&template.name);
        let title = std::iter::once(first.clone())
            .chain((2..=called.len() + 2).map(|n| format!("{first} {n}")))
            .find(|title| !called.contains(&title.to_lowercase()))
            .unwrap_or_default();
        self.presets.choosing = false;
        self.open_editor(None, cx);
        if let Some(editor) = self.presets.editor.as_mut() {
            editor.template = Some(template.name.clone());
            editor.apps.clone_from(&template.apps);
            editor
                .name
                .update(cx, |input, cx| input.set_text(title, cx));
        }
        cx.notify();
    }

    /// What a recommended set is for.
    fn template_pitch(name: &str) -> &'static str {
        match name {
            "launchers" => {
                "Everything to install and run Windows games: stores and the tools to run them."
            }
            "desktop" => {
                "Just the desktop and a browser, for one program you bring in through the Inbox."
            }
            _ => "",
        }
    }

    /// A recommended set to start a new one from.
    fn template_card(index: usize, template: &Preset, cx: &mut Context<Self>) -> impl IntoElement {
        let about = Self::template_pitch(&template.name);
        let title = template
            .title
            .clone()
            .unwrap_or_else(|| template.name.clone());
        let chip = |label: String| ui::tag(label).rounded(px(7.));
        let mut chips = div().flex().flex_wrap().gap_1p5();
        for app in template.apps.iter().take(6) {
            chips = chips.child(chip(app.name.clone()));
        }
        if template.apps.len() > 6 {
            chips = chips.child(chip(format!("+{}", template.apps.len() - 6)));
        }
        let pick = template.clone();
        div()
            .id(("template", index))
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .rounded(px(16.))
            .border_2()
            .border_color(Theme::line())
            // A shade under the page's cards, as the panel of choices.
            .bg(Theme::sidebar())
            .cursor_pointer()
            .hover(|c| c.border_color(Theme::accent()))
            .on_click(cx.listener(move |home, _, _, cx| home.start_from(&pick, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child(title))
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .py(px(2.))
                            .rounded(px(6.))
                            .bg(Theme::accent().opacity(0.15))
                            .text_color(Theme::accent())
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Recommended"),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .line_height(gpui::relative(1.5))
                    .text_color(Theme::secondary_text())
                    .child(about),
            )
            .child(chips)
    }

    /// The first step of a new set: a recommended one, or one of your own.
    fn template_page(&self, cx: &mut Context<Self>) -> Div {
        let mut cards = div().flex().flex_wrap().gap_4();
        for (index, template) in self.presets.templates.iter().enumerate() {
            cards = cards.child(
                div()
                    .w(px(400.))
                    .child(Self::template_card(index, template, cx)),
            );
        }
        cards = cards.child(
            div().w(px(400.)).child(
                div()
                    .id("template-own")
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .h_full()
                    .min_h(px(150.))
                    .p_5()
                    .rounded(px(16.))
                    .border_2()
                    .border_dashed()
                    .border_color(Theme::line())
                    .cursor_pointer()
                    .hover(|c| c.border_color(Theme::accent()))
                    .on_click(cx.listener(|home, _, _, cx| {
                        home.presets.choosing = false;
                        home.open_editor(None, cx);
                    }))
                    .child(
                        gpui_kit::component::Icon::new(gpui_kit::component::IconName::Plus)
                            .size(px(22.))
                            .text_color(Theme::accent()),
                    )
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Make your own"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(Theme::muted())
                            .child("Pick every app yourself"),
                    ),
            ),
        );
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                back_link("templates-back", 14.).on_click(cx.listener(|home, _, _, cx| {
                    home.presets.choosing = false;
                    cx.notify();
                })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("New set of apps"),
                    )
                    .child(div().text_sm().text_color(Theme::muted()).child(
                        "Start from a recommended set and change it as you like, or make one of \
                         your own.",
                    )),
            )
            .child(cards)
    }

    pub(crate) fn presets_view(&self, cx: &mut Context<Self>) -> Div {
        if let Some(editor) = &self.presets.editor {
            return self.editor_page(editor, cx);
        }
        if self.presets.choosing {
            return self.template_page(cx);
        }
        let can_create = self.presets.list.is_ok() && !self.presets.old_service;
        let header = div()
            .flex()
            .items_end()
            .justify_between()
            .gap_4()
            .child(page_title(
                "Sets of apps",
                Some(
                    "Each sandbox starts as a copy of one set. A set is built once, then any \
                     number of sandboxes use it.",
                ),
            ))
            .when(can_create, |header| {
                header.child(
                    button(
                        "preset-new",
                        "New set",
                        Kind::Primary,
                        true,
                        cx.listener(|home, _, _, cx| home.new_set(cx)),
                    )
                    .size(Size::Large),
                )
            });
        let view = div().flex().flex_col().gap(px(18.)).child(header);
        if self.service == client::Match::ServiceOlder || self.presets.old_service {
            return view.child(self.old_service_notice(cx));
        }
        let presets = match &self.presets.list {
            Ok(presets) => presets,
            Err(reason) => {
                return view.child(
                    ui::notice(
                        Tone::Warning,
                        "Sets of apps are unavailable",
                        reason.clone(),
                    )
                    .child(button(
                        "presets-retry",
                        "Try again",
                        Kind::Secondary,
                        true,
                        cx.listener(|_, _, _, cx| Self::reload_presets(cx)),
                    )),
                );
            }
        };
        let list = if presets.is_empty() {
            div()
                .py_10()
                .rounded(px(12.))
                .border_1()
                .border_dashed()
                .border_color(Theme::border())
                .flex()
                .justify_center()
                .text_sm()
                .text_color(Theme::muted())
                .child("No sets of apps. Make one with New set.")
        } else {
            let mut list = div().flex().flex_col().gap_2p5();
            for preset in presets {
                list = list.child(self.set_row(preset, cx));
            }
            list
        };
        view.child(list).child(self.base_row(cx)).child(
            div().text_xs().text_color(Theme::muted()).child(
                "Rebuilding a set gives the sandboxes made from it a new system; their \
                 games, saves and files in /home stay. A set in use by a sandbox can't be \
                 deleted.",
            ),
        )
    }

    /// The sets wait for a service that knows them: the way to it, as the
    /// banner of an older service offers it. An AppImage installs the
    /// service it carries; a development build restarts the one installed.
    fn old_service_notice(&self, cx: &mut Context<Self>) -> Div {
        let carried = crate::setup::carries_service();
        ui::notice(
            Tone::Warning,
            "Sets of apps need the current service",
            if carried {
                "The running gse service is older than this app. Update it to see, edit and \
                 build sets here."
            } else {
                "The running gse service is older than this app. Restart it to see, edit and \
                 build sets here."
            },
        )
        .child(button(
            "sets-service",
            match (carried, self.restarting) {
                (true, false) => "Update service",
                (true, true) => "Updating...",
                (false, false) => "Restart service",
                (false, true) => "Restarting...",
            },
            Kind::Warning,
            !self.restarting,
            cx.listener(move |home, _, _, cx| {
                if carried {
                    home.update_service(cx);
                } else {
                    home.restarting = true;
                    home.request("service".into(), "", Request::Restart, cx);
                }
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_counts_its_time_in_minutes_and_hours() {
        assert_eq!(minutes_text(0), None);
        assert_eq!(minutes_text(6).as_deref(), Some("6 min"));
        assert_eq!(minutes_text(75).as_deref(), Some("1 h 15 min"));
    }

    #[test]
    fn sets_go_by_their_titles_and_untitled_ones_by_their_names() {
        let set = |name: &str, title: Option<&str>| Preset {
            name: name.into(),
            title: title.map(str::to_owned),
            apps: Vec::new(),
        };
        let titles = titles(&[
            set("launchers", Some("Game launchers")),
            set("wow-addons", Some("WoW addons")),
            set("old", None),
        ]);
        assert_eq!(title_in(&titles, "launchers"), "Game launchers");
        assert_eq!(title_in(&titles, "wow-addons"), "WoW addons");
        assert_eq!(title_in(&titles, "old"), "old");
        assert_eq!(title_in(&titles, "gone"), "gone");
        assert_eq!(build_name(BASE_IMAGE), "The sandbox system");
    }

    #[test]
    fn a_new_set_is_named_after_its_title() {
        let taken = |names: &[&str]| names.iter().map(|&n| n.to_owned()).collect::<Vec<_>>();
        assert_eq!(free_name("WoW addons", &[]), "wow-addons");
        assert_eq!(
            free_name("WoW addons", &taken(&["wow-addons"])),
            "wow-addons-2"
        );
        assert_eq!(
            free_name("WoW addons!", &taken(&["wow-addons", "wow-addons-2"])),
            "wow-addons-3"
        );
        assert_eq!(free_name("  Office -- 2026 ", &[]), "office-2026");
        assert_eq!(free_name("Игры", &[]), "set");
        assert_eq!(free_name("Base", &[]), "base-2");
        let long = free_name(&"Very long title ".repeat(4), &[]);
        assert_eq!(long, "very-long-title-very-long-ti");
        assert!(valid_name(&long));
        let numbered = free_name(&"Very long title ".repeat(4), &taken(&[&long]));
        assert_eq!(numbered, "very-long-title-very-long-ti-2");
        assert!(valid_name(&numbered));
    }

    #[test]
    fn a_build_started_when_the_service_says() {
        let build = |started| Build {
            preset: "wow".into(),
            steps: Vec::new(),
            started,
        };
        assert_eq!(started_at(&build(None)), None);
        assert_eq!(
            started_at(&build(Some(1_793_545_989))).map(|t| t.timestamp()),
            Some(1_793_545_989)
        );
    }
}
