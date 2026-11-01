//! The presets view: the presets images are built from, whether each has
//! an image, and an editor for them. The service keeps the presets and
//! builds the images; a build's steps reach the app as service events while
//! it runs, so every open app shows them.

use std::fmt::Write as _;
use std::path::PathBuf;

use gpui::{
    App, ClickEvent, Context, Div, Entity, FontWeight, PathPromptOptions, SharedString, Window,
    div, prelude::*, px,
};
use gse_protocol::{AppSource, Build, Event, Preset, PresetApp, Request, valid_name};

use crate::Home;
use crate::client::{self, Unanswered};
use crate::icons::Icon;
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, badge, button, icon_button, segment, segments, tag, tooltip};

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
}

impl Default for Presets {
    fn default() -> Self {
        Self {
            list: Err("Loading the presets...".into()),
            build: None,
            stopping: None,
            editor: None,
            confirm: None,
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
            return Err("The preset has an app of that name.");
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
            let loaded = cx
                .background_executor()
                .spawn(async { client::presets() })
                .await;
            this.update(cx, |home, cx| {
                match loaded {
                    Ok((presets, build)) => {
                        home.presets.list = Ok(presets);
                        home.presets.build = build;
                    }
                    Err(e) => home.presets.list = Err(explain(&e)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                build.steps.push(step.clone());
                self.say(format!("{preset}: building, {step}"), false);
            }
            Event::BuildFinished { preset, error } => {
                self.presets.build = None;
                let stopped = self.presets.stopping.as_ref() == Some(preset);
                match error {
                    _ if stopped => self.say(format!("{preset}: the build was stopped"), false),
                    Some(e) => self.say(format!("{preset}: {e}"), true),
                    None => self.say(format!("The image {preset} is ready"), false),
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
                    Ok(()) => home.say(format!("The image {name} is ready"), false),
                }
                Self::reload(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_editor(&mut self, preset: Option<&Preset>, cx: &mut Context<Self>) {
        let mut input = |placeholder: &'static str| cx.new(|cx| TextInput::new(placeholder, cx));
        self.presets.confirm = None;
        self.presets.editor = Some(Editor {
            existing: preset.map(|p| p.name.clone()),
            name: input("games"),
            apps: preset.map_or_else(Vec::new, |p| p.apps.clone()),
            source: SourceKind::Flathub,
            app_name: input("Discord"),
            flathub_id: input("com.discordapp.Discord"),
            packages: input("steam"),
            command: input("steam"),
            desktop: input("steam.desktop"),
            appimage: None,
            trusted: false,
            problem: None,
            saving: false,
        });
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

    fn save_preset(&mut self, cx: &mut Context<Self>) {
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
            Some("Name the preset with 1-32 lowercase letters, digits and hyphens.")
        } else if editor.existing.is_none() && taken.contains(&name) {
            Some("A preset of that name exists.")
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
                        home.say(format!("Preset {name} saved"), false);
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

    /// One app of a preset as a table row: its name, where it comes from,
    /// what names it there, and which account runs it.
    fn app_row(app: &PresetApp, last: bool) -> Div {
        let runs_as = if app.trusted {
            badge("sandbox", Theme::warning())
        } else {
            badge("guest", Theme::subtle())
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .when(!last, |row| row.border_b_1().border_color(Theme::border()))
            .child(Self::app_identity(app))
            .child(
                div()
                    .flex_none()
                    .w(px(84.))
                    .flex()
                    .justify_end()
                    .child(runs_as),
            )
    }

    /// An app's name, where it comes from and what names it there.
    fn app_identity(app: &PresetApp) -> Div {
        let (kind, detail) = source_text(&app.source);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(170.))
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(app.name.clone()),
            )
            .child(div().flex_none().w(px(84.)).child(tag(kind)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(Theme::subtle())
                    .child(detail),
            )
    }

    /// The progress of the build of `preset`'s image as a timeline, while
    /// it builds.
    pub(crate) fn build_line(&self, preset: &str) -> Option<Div> {
        let build = self.presets.build.as_ref().filter(|b| b.preset == preset)?;
        let plan = self
            .presets
            .list
            .as_ref()
            .ok()
            .and_then(|list| list.iter().find(|p| p.name == preset))
            .map_or_else(|| build_plan(&[]), |p| build_plan(&p.apps));
        // A step outside the plan keeps the line where it was.
        let reached = build
            .steps
            .iter()
            .rev()
            .find_map(|step| plan.iter().position(|(name, _)| name == step))
            .unwrap_or(0);
        let labels: Vec<&str> = plan.iter().map(|(_, label)| *label).collect();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(ui::timeline(
                    &format!("build-{preset}"),
                    &labels,
                    Some(reached),
                    false,
                ))
                .child(div().text_xs().text_color(Theme::subtle()).child(
                    "The build runs in a VM without a GPU and takes several minutes; \
                     the old image stays until the new one is ready, and closing the app \
                     does not stop it.",
                )),
        )
    }

    /// A delete or rebuild of `name` waiting for its second click.
    fn preset_confirm(
        name: &str,
        confirm: Confirm,
        users: &[&str],
        actions: Div,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = name.to_owned();
        let question: SharedString = match confirm {
            Confirm::Delete => "Delete the preset? Its image stays.".into(),
            Confirm::Build => match users.len() {
                1 => "Rebuilding resets its sandbox.".into(),
                n => format!("Rebuilding resets its {n} sandboxes.").into(),
            },
        };
        actions
            .child(div().text_sm().text_color(Theme::muted()).child(question))
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
                match confirm {
                    Confirm::Delete => "Delete",
                    Confirm::Build => "Rebuild",
                },
                Kind::Danger,
                true,
                cx.listener(move |home, _, _, cx| match confirm {
                    Confirm::Delete => {
                        home.presets.confirm = None;
                        home.request(
                            format!("{name}/delete-preset"),
                            "Deleting",
                            Request::DeletePreset { name: name.clone() },
                            cx,
                        );
                    }
                    Confirm::Build => home.build_image(&name, cx),
                }),
            ))
    }

    fn preset_actions(&self, preset: &Preset, users: &[&str], cx: &mut Context<Self>) -> Div {
        let name = preset.name.clone();
        let actions = div().flex().flex_none().items_center().gap_2();
        if let Some((_, confirm)) = self.presets.confirm.as_ref().filter(|(n, _)| *n == name) {
            return Self::preset_confirm(&name, *confirm, users, actions, cx);
        }
        if self.building(&name) {
            let stopping = self.presets.stopping.as_ref() == Some(&name);
            return actions.child(button(
                format!("preset-stop-{name}"),
                if stopping {
                    "Stopping..."
                } else {
                    "Stop the build"
                },
                Kind::Danger,
                !stopping,
                cx.listener(move |home, _, _, cx| {
                    home.presets.stopping = Some(name.clone());
                    home.request(format!("{name}/stop-build"), "", Request::StopBuild, cx);
                }),
            ));
        }
        let free = !self.any_building()
            && !self.any_running()
            && !self.busy.contains_key(&format!("{name}/delete-preset"));
        let has_image = self.images.contains(&name);
        let reset_needed = !users.is_empty();
        let (delete, edit, build) = (name.clone(), preset.clone(), name.clone());
        let why_not = if self.any_running() {
            "Images build while no sandbox runs"
        } else {
            "Another image is building"
        };
        actions
            .child(icon_button(
                format!("preset-edit-{name}"),
                Icon::Edit,
                "Edit the apps",
                Kind::Ghost,
                true,
                cx.listener(move |home, _, _, cx| home.open_editor(Some(&edit), cx)),
            ))
            .child(icon_button(
                format!("preset-delete-{name}"),
                Icon::Trash,
                if free {
                    "Delete the preset; its image stays"
                } else {
                    why_not
                },
                Kind::Ghost,
                free,
                cx.listener(move |home, _, _, cx| {
                    home.presets.confirm = Some((delete.clone(), Confirm::Delete));
                    cx.notify();
                }),
            ))
            .child(
                button(
                    format!("preset-build-{name}"),
                    if has_image {
                        "Rebuild image"
                    } else {
                        "Build image"
                    },
                    if has_image {
                        Kind::Secondary
                    } else {
                        Kind::Primary
                    },
                    free,
                    cx.listener(move |home, _, _, cx| {
                        if reset_needed {
                            home.presets.confirm = Some((build.clone(), Confirm::Build));
                            cx.notify();
                        } else {
                            home.build_image(&build, cx);
                        }
                    }),
                )
                .when(!free, |b| b.tooltip(tooltip(why_not.into()))),
            )
    }

    /// How many apps a preset holds and how many sandboxes use its image,
    /// the sandboxes named in a tooltip.
    fn preset_facts(preset: &Preset, users: &[&str]) -> Div {
        let name = &preset.name;
        let apps_count = match preset.apps.len() {
            1 => "1 app".to_owned(),
            n => format!("{n} apps"),
        };
        let mut facts = div().flex().items_center().gap_2().child(tag(apps_count));
        if !users.is_empty() {
            let shown: Vec<&str> = users.iter().copied().take(12).collect();
            let mut list = shown.join(", ");
            if users.len() > shown.len() {
                let _ = write!(list, " and {} more", users.len() - shown.len());
            }
            facts = facts.child(
                div()
                    .id(SharedString::from(format!("preset-users-{name}")))
                    .child(tag(match users.len() {
                        1 => "1 sandbox".to_owned(),
                        n => format!("{n} sandboxes"),
                    }))
                    .tooltip(tooltip(list.into())),
            );
        }
        facts
    }

    fn preset_card(&self, preset: &Preset, cx: &mut Context<Self>) -> Div {
        let name = &preset.name;
        let building = self.building(name);
        let users: Vec<&str> = self
            .environments
            .iter()
            .filter(|e| e.image == *name)
            .map(|e| e.name.as_str())
            .collect();
        let state = if building {
            badge("Building", Theme::accent())
        } else if self.images.contains(name) {
            badge("Image ready", Theme::good())
        } else {
            badge("Not built", Theme::subtle())
        };
        let facts = Self::preset_facts(preset, &users);
        let mut apps = div()
            .flex()
            .flex_col()
            .rounded_md()
            .bg(Theme::background())
            .border_1()
            .border_color(Theme::border());
        if preset.apps.is_empty() {
            apps = apps.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(Theme::subtle())
                    .child("No apps: the image is the base desktop alone."),
            );
        }
        for (i, app) in preset.apps.iter().enumerate() {
            apps = apps.child(Self::app_row(app, i + 1 == preset.apps.len()));
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .rounded_xl()
            .bg(Theme::surface())
            .border_1()
            .border_color(if building {
                Theme::accent().opacity(0.4)
            } else {
                Theme::border()
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(name.clone()),
                                    )
                                    .child(state),
                            )
                            .child(facts),
                    )
                    .child(self.preset_actions(preset, &users, cx)),
            )
            .child(apps)
            .children(
                self.build_line(name)
                    .map(|line| line.pt_4().border_t_1().border_color(Theme::border())),
            )
    }

    fn set_app_trusted(
        index: usize,
        value: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |home, _, _, cx| {
            if let Some(app) = home
                .presets
                .editor
                .as_mut()
                .and_then(|e| e.apps.get_mut(index))
            {
                app.trusted = value;
                cx.notify();
            }
        })
    }

    fn set_new_app_trusted(
        value: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |home, _, _, cx| {
            if let Some(editor) = home.presets.editor.as_mut() {
                editor.trusted = value;
                cx.notify();
            }
        })
    }

    fn editor_app_row(index: usize, app: &PresetApp, locked: bool, cx: &mut Context<Self>) -> Div {
        let trusted = app.trusted;
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(Theme::border())
            .child(Self::app_identity(app))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(
                        segments()
                            .child(segment(
                                format!("app-guest-{index}"),
                                "guest",
                                !trusted,
                                !locked,
                                Self::set_app_trusted(index, false, cx),
                            ))
                            .child(segment(
                                format!("app-trusted-{index}"),
                                "trusted",
                                trusted,
                                !locked,
                                Self::set_app_trusted(index, true, cx),
                            )),
                    )
                    .child(button(
                        format!("app-remove-{index}"),
                        "Remove",
                        Kind::Ghost,
                        !locked,
                        cx.listener(move |home, _, _, cx| {
                            if let Some(editor) = home.presets.editor.as_mut()
                                && index < editor.apps.len()
                            {
                                editor.apps.remove(index);
                                cx.notify();
                            }
                        }),
                    )),
            )
    }

    /// The fields that name a new app and where it comes from.
    fn new_app_identity(editor: &Editor, cx: &mut Context<Self>) -> Div {
        match editor.source {
            SourceKind::Flathub => div()
                .flex()
                .gap_3()
                .child(field("Name", editor.app_name.clone()))
                .child(field("Flathub ID", editor.flathub_id.clone())),
            SourceKind::Fedora => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(field("Name", editor.app_name.clone()))
                        .child(field("Packages", editor.packages.clone())),
                )
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(field("Command", editor.command.clone()))
                        .child(field(
                            "Menu entry it replaces (optional)",
                            editor.desktop.clone(),
                        )),
                )
                .child(div().text_xs().text_color(Theme::subtle()).child(
                    "Packages come from Fedora and RPM Fusion. The package's own menu entry \
                     is replaced so the app starts as the account chosen below; without a \
                     name here it is taken from the command.",
                )),
            SourceKind::AppImage => {
                let chosen: SharedString = editor.appimage.as_ref().map_or_else(
                    || "No file chosen".into(),
                    |p| p.display().to_string().into(),
                );
                div()
                    .flex()
                    .gap_3()
                    .items_end()
                    .child(field("Name", editor.app_name.clone()))
                    .child(field(
                        "AppImage file",
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
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
                                    .text_sm()
                                    .text_color(Theme::muted())
                                    .truncate()
                                    .child(chosen),
                            ),
                    ))
            }
        }
    }

    fn new_app_form(editor: &Editor, cx: &mut Context<Self>) -> Div {
        let source = editor.source;
        let choose =
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
            };
        let trusted = editor.trusted;
        let identity = Self::new_app_identity(editor, cx);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded_lg()
            .bg(Theme::background())
            .border_1()
            .border_color(Theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Add an app"),
                    )
                    .child(
                        segments()
                            .child(choose("source-flathub", "Flathub", SourceKind::Flathub, cx))
                            .child(choose(
                                "source-fedora",
                                "Fedora package",
                                SourceKind::Fedora,
                                cx,
                            ))
                            .child(choose(
                                "source-appimage",
                                "AppImage",
                                SourceKind::AppImage,
                                cx,
                            )),
                    ),
            )
            .child(identity)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div().flex().items_center().gap_2().child(
                            segments()
                                .child(segment(
                                    "new-app-guest",
                                    "Runs as guest",
                                    !trusted,
                                    true,
                                    Self::set_new_app_trusted(false, cx),
                                ))
                                .child(segment(
                                    "new-app-trusted",
                                    "Trusted: runs as sandbox",
                                    trusted,
                                    true,
                                    Self::set_new_app_trusted(true, cx),
                                )),
                        ),
                    )
                    .child(button(
                        "app-add",
                        "Add app",
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, _, cx| home.add_app(cx)),
                    )),
            )
    }

    fn editor_card(editor: &Editor, cx: &mut Context<Self>) -> Div {
        let locked = editor.saving;
        let title: SharedString = match &editor.existing {
            Some(name) => format!("Edit {name}").into(),
            None => "New preset".into(),
        };
        let mut card = ui::card(
            title,
            Some(
                "The apps an image built from this preset holds on top of the base image. \
                 Changes apply to the next build.",
            ),
        );
        if editor.existing.is_none() {
            card = card.child(div().w_full().max_w(px(320.)).child(field(
                "Name (lowercase letters, digits and hyphens)",
                editor.name.clone(),
            )));
        }
        let mut apps = div().flex().flex_col().gap_2();
        if editor.apps.is_empty() {
            apps = apps.child(
                div()
                    .text_sm()
                    .text_color(Theme::subtle())
                    .child("No apps yet."),
            );
        }
        for (index, app) in editor.apps.iter().enumerate() {
            apps = apps.child(Self::editor_app_row(index, app, locked, cx));
        }
        card.child(apps)
            .child(Self::new_app_form(editor, cx))
            .children(
                editor
                    .problem
                    .clone()
                    .map(|problem| div().text_sm().text_color(Theme::danger()).child(problem)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(button(
                        "preset-editor-cancel",
                        "Cancel",
                        Kind::Ghost,
                        !locked,
                        cx.listener(|home, _, _, cx| {
                            home.presets.editor = None;
                            cx.notify();
                        }),
                    ))
                    .child(button(
                        "preset-editor-save",
                        if locked { "Saving..." } else { "Save preset" },
                        Kind::Primary,
                        !locked,
                        cx.listener(|home, _, _, cx| home.save_preset(cx)),
                    )),
            )
    }

    pub(crate) fn presets_view(&self, cx: &mut Context<Self>) -> Div {
        let can_create = self.presets.editor.is_none() && self.presets.list.is_ok();
        let title = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Presets"),
                    )
                    .child(div().text_sm().text_color(Theme::muted()).child(
                        "The apps an image holds. Environments are created from the images.",
                    )),
            )
            .when(can_create, |title| {
                title.child(button(
                    "preset-new",
                    "New preset",
                    Kind::Secondary,
                    true,
                    cx.listener(|home, _, _, cx| home.open_editor(None, cx)),
                ))
            });
        let view = div().flex().flex_col().gap_4().child(title);

        if self.service == client::Match::ServiceOlder {
            return view.child(ui::notice(
                Theme::warning(),
                "Presets need the current service",
                "The running gse service is older than this app and does not know presets. \
                 Restart it to see, edit and build them here.",
            ));
        }
        let presets = match &self.presets.list {
            Ok(presets) => presets,
            Err(reason) => {
                return view.child(
                    ui::notice(Theme::warning(), "Presets are unavailable", reason.clone()).child(
                        button(
                            "presets-retry",
                            "Try again",
                            Kind::Secondary,
                            true,
                            cx.listener(|_, _, _, cx| Self::reload_presets(cx)),
                        ),
                    ),
                );
            }
        };
        if let Some(editor) = &self.presets.editor {
            return view.child(Self::editor_card(editor, cx));
        }
        let mut view = view;
        if let Some(running) = self.environments.iter().find(|e| e.running.is_some()) {
            view = view.child(ui::notice(
                Theme::warning(),
                format!("{} is running", running.name),
                "Images build once it stops.",
            ));
        }
        if presets.is_empty() {
            view = view.child(
                div()
                    .py_10()
                    .rounded_lg()
                    .border_1()
                    .border_dashed()
                    .border_color(Theme::border())
                    .flex()
                    .justify_center()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("No presets. Create one with New preset."),
            );
        }
        for preset in presets {
            view = view.child(self.preset_card(preset, cx));
        }
        view.child(div().text_xs().text_color(Theme::subtle()).child(
            "Apps run as guest, an account that cannot reach the desktop's files. \
                     Trusted apps run as sandbox, the desktop's own account.",
        ))
    }
}
