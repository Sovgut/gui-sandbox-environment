//! The presets view: the presets images are built from, whether each has
//! an image, and an editor for them. The service keeps the presets and
//! builds the images; a build's steps reach the app as service events while
//! it runs, so every open app shows them.

use std::path::PathBuf;

use gpui::{
    Context, Div, Entity, FontWeight, PathPromptOptions, SharedString, div, prelude::*, px,
};
use gse_protocol::{AppSource, Build, Event, Preset, PresetApp, Request, valid_name};

use crate::Home;
use crate::client::{self, Unanswered};
use crate::icons::Icon;
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, button, segment};
use gpui_kit::component::Sizable as _;

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

/// What a build is called to the user: a set by its name, the base image
/// as the sandbox system.
pub(crate) fn build_name(preset: &str) -> String {
    if preset == gse_protocol::BASE_IMAGE {
        "The sandbox system".to_owned()
    } else {
        format!("The set {preset}")
    }
}

/// The state of the presets view.
pub struct Presets {
    /// The presets, or why they are unavailable.
    list: Result<Vec<Preset>, SharedString>,
    /// The image build in progress, as the service last reported it.
    pub(crate) build: Option<Build>,
    /// The preset whose build the user asked to stop.
    stopping: Option<String>,
    editor: Option<Editor>,
    /// A preset whose delete or rebuild waits for a second click.
    confirm: Option<(String, Confirm)>,
    /// The base image every set is built on, as the service reports it.
    pub(crate) base: Option<gse_protocol::BaseImage>,
    /// The sets built on an older base image.
    pub(crate) stale: Vec<String>,
    /// The sets the service ships with, offered when a new set is made.
    pub(crate) templates: Vec<Preset>,
    /// A new set is being started: the recommended sets are shown.
    choosing: bool,
}

impl Default for Presets {
    fn default() -> Self {
        Self {
            list: Err("Loading the presets...".into()),
            build: None,
            stopping: None,
            editor: None,
            confirm: None,
            base: None,
            stale: Vec::new(),
            templates: Vec::new(),
            choosing: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Delete,
    Build,
}

/// Where a new app comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Flathub,
    Fedora,
    AppImage,
}

/// A preset being created or changed, with the form for a new app.
struct Editor {
    /// The preset's name when it exists; a new one is named in `name`.
    existing: Option<String>,
    name: Entity<TextInput>,
    apps: Vec<PresetApp>,
    source: SourceKind,
    app_name: Entity<TextInput>,
    flathub_id: Entity<TextInput>,
    packages: Entity<TextInput>,
    command: Entity<TextInput>,
    desktop: Entity<TextInput>,
    appimage: Option<PathBuf>,
    trusted: bool,
    /// Why the last add or save was refused.
    problem: Option<SharedString>,
    saving: bool,
    /// What the Flathub search last asked for, and what it found.
    searched: String,
    found: Result<Vec<crate::flathub::Found>, String>,
}

/// Turns a failed presets request into words for the user.
fn explain(error: &anyhow::Error) -> SharedString {
    if error.downcast_ref::<Unanswered>().is_some() {
        "The running gse service does not know presets yet. \
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
                    Ok(state) => home.load_presets(state),
                    Err(e) => home.presets.list = Err(explain(&e)),
                }
                // A service without templates leaves only "make your own".
                home.presets.templates = templates.unwrap_or_default();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Takes the sets, the build in progress and the base image as the
    /// service reported them.
    pub(crate) fn load_presets(&mut self, state: client::PresetsState) {
        self.presets.list = Ok(state.presets);
        self.presets.build = state.build;
        self.presets.base = state.base;
        self.presets.stale = state.stale;
    }

    /// Follows the image build through the service's events.
    pub(crate) fn preset_event(&mut self, event: &Event) {
        match event {
            Event::Build { preset, step } => {
                let build = self.presets.build.get_or_insert_with(|| Build {
                    preset: preset.clone(),
                    steps: Vec::new(),
                });
                if build.preset != *preset {
                    *build = Build {
                        preset: preset.clone(),
                        steps: Vec::new(),
                    };
                }
                // The build's banner and line show each step; only its end is
                // worth a message.
                build.steps.push(step.clone());
            }
            Event::BuildFinished { preset, error } => {
                self.presets.build = None;
                let stopped = self.presets.stopping.as_ref() == Some(preset);
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

    /// The steps of the build of `preset`'s image and the one it is at,
    /// while it builds.
    pub(crate) fn build_steps(&self, preset: &str) -> Option<(Vec<&'static str>, usize)> {
        let build = self.presets.build.as_ref().filter(|b| b.preset == preset)?;
        let plan = if preset == gse_protocol::BASE_IMAGE {
            BASE_PLAN.to_vec()
        } else {
            self.preset_apps(preset)
                .map_or_else(|| build_plan(&[]), build_plan)
        };
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
        self.presets.build = Some(Build {
            preset: name.to_owned(),
            steps: Vec::new(),
        });
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
                match outcome {
                    Err(_) if stopped => home.say(format!("{name}: the build was stopped"), false),
                    Err(e) => home.say(format!("{name}: {}", explain(&e)), true),
                    Ok(()) => home.say(format!("The set {name} is built"), false),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_editor(&mut self, preset: Option<&Preset>, cx: &mut Context<Self>) {
        let mut input = |placeholder: &'static str| {
            let input = cx.new(|cx| TextInput::new(placeholder, cx));
            cx.observe(&input, |_, _, cx| cx.notify()).detach();
            input
        };
        let search = input("Search Flathub");
        self.presets.confirm = None;
        self.presets.editor = Some(Editor {
            existing: preset.map(|p| p.name.clone()),
            name: input("Name of the set, such as games"),
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
            if let Some(name) = &editor.existing {
                let name = name.clone();
                editor.name.update(cx, |input, cx| input.set_text(name, cx));
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

    /// Saves the set being edited, and builds it after with `build`.
    fn save_preset(&mut self, build: bool, cx: &mut Context<Self>) {
        let taken: Vec<String> = self
            .presets
            .list
            .as_ref()
            .map(|l| l.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        let Some(editor) = self.presets.editor.as_mut() else {
            return;
        };
        let name = editor
            .existing
            .clone()
            .unwrap_or_else(|| editor.name.read(cx).text().trim().to_owned());
        let problem = if !valid_name(&name) {
            Some("Name the set with 1-32 lowercase letters, digits and hyphens.")
        } else if editor.existing.is_none() && taken.contains(&name) {
            Some("A set of that name exists.")
        } else if editor.apps.is_empty() {
            Some("Add at least one app.")
        } else {
            None
        };
        if let Some(problem) = problem {
            editor.problem = Some(problem.into());
            cx.notify();
            return;
        }
        editor.saving = true;
        editor.problem = None;
        let preset = Preset {
            name: name.clone(),
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
                        home.say(format!("Set {name} saved"), false);
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

    /// The build of `preset` as a line of its steps, while it builds.
    fn build_line(&self, preset: &str) -> Option<Div> {
        let (steps, reached) = self.build_steps(preset)?;
        Some(ui::timeline(
            &format!("build-{preset}"),
            &steps,
            Some(reached),
            false,
            Theme::warning(),
        ))
    }

    /// The square that stands for a set: its cover and its initial.
    fn set_mark(name: &str) -> Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(40.))
            .rounded(px(10.))
            .bg(crate::shell::cover(name))
            .font_weight(FontWeight::BOLD)
            .child(
                name.chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string(),
            )
    }

    /// The rebuild of `name` asking first, since it resets its sandboxes.
    fn rebuild_confirm(name: &str, users: usize, cx: &mut Context<Self>) -> Div {
        let go = name.to_owned();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child(match users {
                        1 => "Rebuilding resets its sandbox.".to_owned(),
                        n => format!("Rebuilding resets its {n} sandboxes."),
                    }),
            )
            .child(button(
                format!("preset-cancel-{name}"),
                "Cancel",
                Kind::Ghost,
                true,
                cx.listener(|home, _, _, cx| {
                    home.presets.confirm = None;
                    cx.notify();
                }),
            ))
            .child(button(
                format!("preset-confirm-{name}"),
                "Rebuild",
                Kind::Danger,
                true,
                cx.listener(move |home, _, _, cx| home.build_image(&go, cx)),
            ))
    }

    /// What can be done with `preset` now, at the right of its row.
    fn set_actions(&self, preset: &Preset, users: usize, cx: &mut Context<Self>) -> Div {
        let name = preset.name.clone();
        let actions = div().flex().flex_none().items_center().gap_3();
        if self
            .presets
            .confirm
            .as_ref()
            .is_some_and(|(n, c)| *n == name && *c == Confirm::Build)
        {
            return actions.child(Self::rebuild_confirm(&name, users, cx));
        }
        if self.building(&name) {
            let stopping = self.presets.stopping.as_ref() == Some(&name);
            return actions.child(button(
                format!("preset-stop-{name}"),
                if stopping { "Stopping..." } else { "Stop" },
                Kind::Secondary,
                !stopping,
                cx.listener(move |home, _, _, cx| {
                    home.presets.stopping = Some(name.clone());
                    home.request(format!("{name}/stop-build"), "", Request::StopBuild, cx);
                }),
            ));
        }
        let free = !self.any_building() && !self.any_running();
        let built = self.images.contains(&name);
        let (edit, to_build) = (preset.clone(), name.clone());
        actions
            .child(
                div()
                    .id(SharedString::from(format!("preset-edit-{name}")))
                    .text_sm()
                    .text_color(Theme::accent())
                    .cursor_pointer()
                    .on_click(cx.listener(move |home, _, _, cx| home.open_editor(Some(&edit), cx)))
                    .child("Edit"),
            )
            .child(
                button(
                    format!("preset-build-{name}"),
                    if built { "Rebuild" } else { "Build" },
                    if built {
                        Kind::Secondary
                    } else {
                        Kind::Primary
                    },
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
                .small()
                .when(!free, |b| {
                    b.tooltip(if self.any_running() {
                        "Sets build while no sandbox runs"
                    } else {
                        "Another set is building"
                    })
                }),
            )
    }

    /// One set in the list: its mark, apps and users, state and actions,
    /// and its build's steps while it builds.
    fn set_row(&self, preset: &Preset, cx: &mut Context<Self>) -> Div {
        let name = &preset.name;
        let users = self
            .environments
            .iter()
            .filter(|e| e.image == *name)
            .count();
        let apps: Vec<&str> = preset.apps.iter().map(|a| a.name.as_str()).collect();
        let used = match users {
            0 => "no sandboxes".to_owned(),
            1 => "1 sandbox".to_owned(),
            n => format!("{n} sandboxes"),
        };
        let (state, color) = self.set_state(name);
        let state = match self.build_steps(name) {
            Some((steps, reached)) => format!(
                "Building \u{b7} {}",
                steps.get(reached).copied().unwrap_or_default()
            ),
            None => state.to_owned(),
        };
        let building = self.building(name);
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
                    .child(Self::set_mark(name))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(name.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(Theme::muted())
                                    .child(format!("{} \u{b7} {used}", apps.join(", "))),
                            ),
                    )
                    .child(div().flex_none().text_xs().text_color(color).child(state))
                    .child(self.set_actions(preset, users, cx)),
            )
            .children(self.build_line(name))
    }

    /// The sandbox system every set is built on, with a way to rebuild it.
    fn base_row(&self, cx: &mut Context<Self>) -> Div {
        let building = self.building(gse_protocol::BASE_IMAGE);
        let built = std::fs::metadata("/var/lib/gse/base.qcow2")
            .and_then(|m| m.modified())
            .ok()
            .map(|t| {
                chrono::DateTime::<chrono::Local>::from(t)
                    .format("built %-d %b")
                    .to_string()
            });
        let free = !self.any_building() && !self.any_running();
        let outdated = self.anything_outdated();
        let state = if building {
            match self.build_percent(gse_protocol::BASE_IMAGE) {
                Some(p) => format!("Building, {p}%"),
                None => "Building".to_owned(),
            }
        } else if self.base_outdated() {
            "Out of date".to_owned()
        } else {
            built.unwrap_or_else(|| "Not built".to_owned())
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
                    .child(div().text_xs().text_color(Theme::muted()).child(
                        "The desktop and drivers every set is built on. Rebuild it after a \
                         GSE update, then rebuild the sets.",
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(if building {
                        Theme::warning()
                    } else {
                        Theme::muted()
                    })
                    .child(state),
            )
            .when(!building, |r| {
                r.child(if outdated {
                    button(
                        "base-update",
                        "Update everything",
                        Kind::Primary,
                        free,
                        cx.listener(|home, _, _, cx| home.update_everything(cx)),
                    )
                    .small()
                } else {
                    button(
                        "base-rebuild",
                        "Rebuild",
                        Kind::Secondary,
                        free,
                        cx.listener(|home, _, _, cx| home.build_base(cx)),
                    )
                    .small()
                })
            });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .py_3()
            .rounded(px(12.))
            .border_1()
            .border_color(if building || outdated {
                Theme::warning().opacity(0.3)
            } else {
                Theme::border()
            })
            .child(row)
            .children(self.build_line(gse_protocol::BASE_IMAGE))
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
    /// set are reset onto its new build. What is left to do is asked of the
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
        let base = gse_protocol::BASE_IMAGE;
        self.busy.insert(format!("{base}/build"), "Building");
        self.presets.build = Some(Build {
            preset: base.to_owned(),
            steps: Vec::new(),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { client::build_base() })
                .await;
            this.update(cx, |home, cx| {
                home.busy.remove(&format!("{base}/build"));
                match outcome {
                    Ok(()) => home.say(
                        "The sandbox system is rebuilt; rebuild the sets to use it",
                        false,
                    ),
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
            format!("runs-as-{index}"),
            if app.trusted {
                "sandbox (trusted)"
            } else {
                "guest (untrusted)"
            },
            vec![
                ("guest (untrusted)".to_owned(), false),
                ("sandbox (trusted)".to_owned(), true),
            ],
            !locked,
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
            .gap_3()
            .px_3p5()
            .py_2p5()
            .border_t_1()
            .border_color(Theme::divider())
            .text_sm()
            .child(div().flex_1().min_w_0().truncate().child(app.name.clone()))
            .child(
                div()
                    .w(px(90.))
                    .flex_none()
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(from),
            )
            .child(div().w(px(170.)).flex_none().child(runs))
            .child(ui::small_icon_button(
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
            ))
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
                    .text_sm()
                    .text_color(Theme::muted())
                    .border_b_1()
                    .border_color(Theme::border())
                    .child(format!("In this set \u{b7} {}", editor.apps.len())),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .px_3p5()
                    .py_2()
                    .child(head("App", None))
                    .child(head("From", Some(90.)))
                    .child(head("Runs as", Some(170.)))
                    .child(div().w(px(22.)).flex_none()),
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
                                    Kind::Secondary,
                                    !added,
                                    cx.listener(move |home, _, _, cx| home.add_found(&pick, cx)),
                                )
                                .small(),
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
            let chosen: SharedString = editor.appimage.as_ref().map_or_else(
                || "No file chosen".into(),
                |p| p.display().to_string().into(),
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
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(Theme::muted())
                            .child(chosen),
                    ),
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

    /// The page of the set being made or changed.
    fn editor_page(&self, editor: &Editor, cx: &mut Context<Self>) -> Div {
        let locked = editor.saving;
        let name_field = self.editor_name(editor, cx);
        let delete = editor.existing.clone();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .id("editor-back")
                    .text_sm()
                    .text_color(Theme::muted())
                    .cursor_pointer()
                    .hover(|b| b.text_color(Theme::text()))
                    .on_click(cx.listener(|home, _, _, cx| {
                        home.presets.editor = None;
                        cx.notify();
                    }))
                    .child("\u{2190} Sets of apps"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(name_field)
                    .child(button(
                        "preset-editor-save",
                        if locked { "Saving..." } else { "Save" },
                        Kind::Secondary,
                        !locked,
                        cx.listener(|home, _, _, cx| home.save_preset(false, cx)),
                    ))
                    .child(button(
                        "preset-editor-build",
                        "Save and build",
                        Kind::Primary,
                        !locked && !self.any_building() && !self.any_running(),
                        cx.listener(|home, _, _, cx| home.save_preset(true, cx)),
                    )),
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
                    .gap_4()
                    .h(px(460.))
                    .child(self.editor_apps(editor, cx))
                    .child(Self::editor_sources(editor, cx)),
            )
            .children(delete.map(|name| self.delete_zone(&name, locked, cx)))
    }

    /// The name of the set being edited: typed for a new set, renamed in
    /// place for one that exists.
    fn editor_name(&self, editor: &Editor, cx: &mut Context<Self>) -> Div {
        let field = div().flex_1().min_w_0().child(editor.name.clone());
        let Some(existing) = editor.existing.clone() else {
            return field;
        };
        let typed = editor.name.read(cx).text().trim().to_owned();
        if typed.is_empty() || typed == existing {
            return field;
        }
        let taken = self.presets_list().unwrap_or_default().contains(&typed);
        let problem = if !valid_name(&typed) || typed == gse_protocol::BASE_IMAGE {
            Some("Names are lowercase letters, digits and hyphens, up to 32.")
        } else if taken {
            Some("A set of that name exists.")
        } else if self.any_running() || self.any_building() {
            Some("Sets are renamed while nothing runs or builds.")
        } else {
            None
        };
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(field)
            .child(button(
                "preset-rename",
                "Rename",
                Kind::Primary,
                problem.is_none(),
                cx.listener(move |_, _, _, cx| Self::rename_set(&existing, &typed, cx)),
            ))
            .children(problem.map(|p| div().text_xs().text_color(Theme::warning()).child(p)))
    }

    /// Renames the set `name` to `to`, its sandboxes following.
    fn rename_set(name: &str, to: &str, cx: &mut Context<Self>) {
        let request = Request::RenamePreset {
            name: name.to_owned(),
            to: to.to_owned(),
        };
        let to = to.to_owned();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { client::command(&request) })
                .await;
            this.update(cx, |home, cx| {
                match outcome {
                    Ok(()) => {
                        if let Some(editor) = home.presets.editor.as_mut() {
                            editor.existing = Some(to.clone());
                        }
                        home.say(format!("The set is now called {to}"), false);
                    }
                    Err(e) => home.say(explain(&e), true),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
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
            .gap_4()
            .px_4()
            .py_3()
            .rounded(px(12.))
            .border_1()
            .border_color(Theme::danger().opacity(0.3))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
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
                Kind::Danger,
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

    /// Opens the editor on a new set with the apps of `template`, named
    /// after it unless that name is taken.
    fn start_from(&mut self, template: &Preset, cx: &mut Context<Self>) {
        let taken = self.presets_list().unwrap_or_default();
        let name = std::iter::once(template.name.clone())
            .chain((2..=taken.len() + 2).map(|n| format!("{}-{n}", template.name)))
            .find(|n| !taken.contains(n))
            .unwrap_or_default();
        self.presets.choosing = false;
        self.open_editor(None, cx);
        if let Some(editor) = self.presets.editor.as_mut() {
            editor.apps.clone_from(&template.apps);
            editor.name.update(cx, |input, cx| input.set_text(name, cx));
        }
        cx.notify();
    }

    /// What a recommended set is called and what it is for.
    fn template_text(name: &str) -> (&'static str, &'static str) {
        match name {
            "launchers" => (
                "Game launchers",
                "Everything to install and run Windows games: stores and the tools to run them.",
            ),
            "desktop" => (
                "Empty desktop",
                "Just the desktop and a browser, for one program you bring in through the Inbox.",
            ),
            _ => ("", ""),
        }
    }

    /// A recommended set to start a new one from.
    fn template_card(index: usize, template: &Preset, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, about) = Self::template_text(&template.name);
        let title = if title.is_empty() {
            template.name.clone()
        } else {
            title.to_owned()
        };
        let mut chips = div().flex().flex_wrap().gap_1p5();
        for app in template.apps.iter().take(6) {
            chips = chips.child(ui::tag(app.name.clone()));
        }
        if template.apps.len() > 6 {
            chips = chips.child(ui::tag(format!("+{}", template.apps.len() - 6)));
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
            .border_color(Theme::border())
            .bg(Theme::surface())
            .cursor_pointer()
            .hover(|c| c.border_color(Theme::accent()))
            .on_click(cx.listener(move |home, _, _, cx| home.start_from(&pick, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child(title))
                    .child(ui::badge("Recommended", Theme::accent())),
            )
            .child(
                div()
                    .text_sm()
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
                div()
                    .id("templates-back")
                    .text_sm()
                    .text_color(Theme::muted())
                    .cursor_pointer()
                    .hover(|b| b.text_color(Theme::text()))
                    .on_click(cx.listener(|home, _, _, cx| {
                        home.presets.choosing = false;
                        cx.notify();
                    }))
                    .child("\u{2190} Sets of apps"),
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
        let can_create = self.presets.list.is_ok();
        let title = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(div().text_sm().text_color(Theme::muted()).child(
                "Each sandbox starts as a copy of one set. A set is built once, then any \
                 number of sandboxes use it.",
            ))
            .when(can_create, |title| {
                title.child(
                    button(
                        "preset-new",
                        "New set",
                        Kind::Primary,
                        true,
                        cx.listener(|home, _, _, cx| home.new_set(cx)),
                    )
                    .icon(gpui_kit::component::IconName::Plus),
                )
            });
        let mut view = div().flex().flex_col().gap_4().child(title);
        if self.service == client::Match::ServiceOlder {
            return view.child(ui::notice(
                Theme::warning(),
                "Sets of apps need the current service",
                "The running gse service is older than this app. Restart it to see, edit \
                 and build sets here.",
            ));
        }
        let presets = match &self.presets.list {
            Ok(presets) => presets,
            Err(reason) => {
                return view.child(
                    ui::notice(
                        Theme::warning(),
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
        if presets.is_empty() {
            view = view.child(
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
                    .child("No sets of apps. Make one with New set."),
            );
        }
        let mut list = div().flex().flex_col().gap_2p5();
        for preset in presets {
            list = list.child(self.set_row(preset, cx));
        }
        view.child(list).child(self.base_row(cx)).child(
            div().text_xs().text_color(Theme::subtle()).child(
                "Rebuilding a set resets the sandboxes made from it. A set in use by a \
                 sandbox can't be deleted.",
            ),
        )
    }
}
