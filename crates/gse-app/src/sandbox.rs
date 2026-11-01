//! The sandbox pane: the selected sandbox's state, the steps of its start
//! and its journal; and the dialogs over the window: adding a sandbox, a
//! sandbox's settings, and closing the app while a sandbox runs.

use chrono::{Local, TimeZone as _};
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px};
use gse_protocol::{Environment, JournalEntry, Request, Stage, valid_name};

use crate::client;
use crate::icons::Icon;
use crate::journal::Filter;
use crate::settings::Target;
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, badge, button, icon_button};
use crate::{AppTab, Confirm, Home, Modal, Page};

/// The steps of a start, in order, with the names the status line shows.
const STEPS: [(Stage, &str); 4] = [
    (Stage::Preparing, "Prepare"),
    (Stage::SwitchingGpu, "Switch GPU"),
    (Stage::Booting, "Boot"),
    (Stage::Ready, "Desktop"),
];

/// A journal entry's time: the time of day for today, with the date before.
fn journal_time(time: u64) -> String {
    let Some(when) = i64::try_from(time)
        .ok()
        .and_then(|t| Local.timestamp_opt(t, 0).single())
    else {
        return String::new();
    };
    if when.date_naive() == Local::now().date_naive() {
        when.format("%H:%M:%S").to_string()
    } else {
        when.format("%b %d %H:%M").to_string()
    }
}

/// The steps of a start as a [`ui::timeline`]. `stage` is `None` for a
/// sandbox that is not running, whose steps are all ahead.
fn stage_line(name: &str, stage: Option<Stage>) -> Div {
    let reached = stage.map(|s| match s {
        Stage::Stopping => STEPS.len() - 1,
        s => STEPS.iter().position(|(step, _)| *step == s).unwrap_or(0),
    });
    let labels: Vec<&str> = STEPS.iter().map(|(_, label)| *label).collect();
    ui::timeline(
        &format!("stage-{name}"),
        &labels,
        reached,
        stage == Some(Stage::Ready),
    )
}

impl Home {
    /// Shows the sandbox `name` on the right.
    pub(crate) fn select(&mut self, name: &str, cx: &mut Context<Self>) {
        self.page = Page::Sandbox;
        self.confirm = None;
        if self.selected.as_deref() != Some(name) {
            self.selected = Some(name.to_owned());
            self.journal_filter
                .update(cx, |input, cx| input.set_text("", cx));
            Self::load_journal(name.to_owned(), cx);
        }
        cx.notify();
    }

    /// Keeps the selection on a sandbox that exists, and its journal loaded.
    pub(crate) fn keep_selection(&mut self, cx: &mut Context<Self>) {
        let exists = |name: &str| self.environments.iter().any(|e| e.name == name);
        match self.selected.clone() {
            Some(name) if exists(&name) => {
                if self.journal.as_ref().is_none_or(|(n, _)| *n != name) {
                    Self::load_journal(name, cx);
                }
            }
            _ => {
                self.selected = self.environments.first().map(|e| e.name.clone());
                self.journal = None;
                if let Some(name) = self.selected.clone() {
                    Self::load_journal(name, cx);
                }
            }
        }
    }

    fn load_journal(name: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let request = name.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move { client::journal(&request) })
                .await;
            this.update(cx, |home, cx| {
                if home.selected.as_deref() == Some(name.as_str()) {
                    // A service without journals leaves the journal empty.
                    home.journal = Some((name, loaded.unwrap_or_default()));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn journal_event(&mut self, name: &str, entry: &JournalEntry) {
        if let Some((shown, entries)) = self.journal.as_mut()
            && shown == name
        {
            entries.push(entry.clone());
        }
    }

    /// Opens the add dialog with a preset already picked, one with a
    /// ready image when there is one; with no preset at all, opens the
    /// presets editor instead.
    pub(crate) fn open_add(&mut self, cx: &mut Context<Self>) {
        let presets = self.presets_list().unwrap_or_default();
        let Some(first) = presets
            .iter()
            .find(|p| self.images.contains(p))
            .or_else(|| presets.first())
            .cloned()
        else {
            self.page = Page::AppSettings;
            self.app_tab = AppTab::Presets;
            if !self.editing_preset() {
                self.open_editor(None, cx);
            }
            self.say(
                "Create a preset first: sandboxes are made from presets",
                false,
            );
            cx.notify();
            return;
        };
        let name = cx.new(|cx| TextInput::new("leave empty for the suggested name", cx));
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        self.modal = Some(Modal::Add {
            preset: Some(first),
            name,
        });
        cx.notify();
    }

    /// The name a new sandbox from `preset` gets unless the user gives one.
    fn suggested_name(&self, preset: &str) -> String {
        (1..=self.environments.len() + 1)
            .map(|n| format!("{preset}-{n}"))
            .find(|n| !self.environments.iter().any(|e| e.name == *n))
            .unwrap_or_default()
    }

    fn create(&mut self, preset: &str, name: &str, cx: &mut Context<Self>) {
        let request = Request::Create {
            name: name.to_owned(),
            image: preset.to_owned(),
        };
        self.modal = None;
        self.selected = Some(name.to_owned());
        self.page = Page::Sandbox;
        self.journal = None;
        self.request(format!("{name}/create"), "Creating", request, cx);
    }

    /// The question and buttons of a reset or delete of `name` waiting for
    /// its second click.
    fn confirm_actions(
        name: &str,
        confirm: Confirm,
        busy: bool,
        actions: Div,
        cx: &mut Context<Self>,
    ) -> Div {
        let (question, label, doing) = match confirm {
            Confirm::Reset => ("Wipe everything inside?", "Reset", "Resetting"),
            Confirm::Delete => ("Delete it and its disk?", "Delete", "Deleting"),
        };
        let target = name.to_owned();
        actions
            .child(div().text_sm().text_color(Theme::muted()).child(question))
            .child(button(
                "confirm-cancel",
                "Cancel",
                Kind::Ghost,
                true,
                cx.listener(|home, _, _, cx| {
                    home.confirm = None;
                    cx.notify();
                }),
            ))
            .child(button(
                "confirm-go",
                label,
                Kind::Danger,
                !busy,
                cx.listener(move |home, _, _, cx| {
                    let request = match confirm {
                        Confirm::Reset => Request::Reset {
                            name: target.clone(),
                        },
                        Confirm::Delete => Request::Delete {
                            name: target.clone(),
                        },
                    };
                    home.modal = None;
                    home.sandbox_draft = None;
                    home.request(target.clone(), doing, request, cx);
                }),
            ))
    }

    /// Reset and delete, in the settings dialog of `name`.
    fn discard_card(&self, name: &str, running: bool, cx: &mut Context<Self>) -> Div {
        let busy = self.busy.contains_key(name);
        let card = ui::card(
            "Reset or delete",
            Some(
                "Reset wipes everything inside and the next start begins from the \
                 preset's image. Delete removes the sandbox with its disk and journal.",
            ),
        );
        let actions = div().flex().items_center().gap_2();
        if running {
            return card.child(
                div()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("Both wait until it stops."),
            );
        }
        if let Some((_, confirm)) = self.confirm.as_ref().filter(|(n, _)| n == name) {
            return card.child(Self::confirm_actions(name, *confirm, busy, actions, cx));
        }
        let (reset, delete) = (name.to_owned(), name.to_owned());
        card.child(
            actions
                .child(icon_button(
                    "sandbox-reset",
                    Icon::Reset,
                    "Reset: wipe everything inside",
                    Kind::Danger,
                    !busy,
                    cx.listener(move |home, _, _, cx| {
                        home.confirm = Some((reset.clone(), Confirm::Reset));
                        cx.notify();
                    }),
                ))
                .child(icon_button(
                    "sandbox-delete",
                    Icon::Trash,
                    "Delete the sandbox with its disk and journal",
                    Kind::Danger,
                    !busy,
                    cx.listener(move |home, _, _, cx| {
                        home.confirm = Some((delete.clone(), Confirm::Delete));
                        cx.notify();
                    }),
                )),
        )
    }

    /// The buttons of the selected sandbox.
    fn sandbox_actions(&self, environment: &Environment, cx: &mut Context<Self>) -> Div {
        let name = environment.name.clone();
        let busy = self.busy.contains_key(&name);
        let mut actions = div().flex().flex_none().items_center().gap_2();

        let up = environment
            .running
            .as_ref()
            .is_some_and(|r| r.stage == Stage::Ready);
        let files = name.clone();
        actions = actions.child(icon_button(
            "sandbox-files",
            Icon::Transfer,
            if up {
                "Files: take them out of the Outbox, send them into the Inbox"
            } else {
                "Files move while the sandbox runs"
            },
            Kind::Secondary,
            up,
            cx.listener(move |home, _, _, cx| home.open_transfer(&files, cx)),
        ));
        let settings = name.clone();
        let settings_button = icon_button(
            "sandbox-settings",
            Icon::Sliders,
            "Sandbox settings: name, GPU, screen, internet",
            Kind::Secondary,
            self.settings.is_ok(),
            cx.listener(move |home, _, _, cx| home.open_sandbox_settings(&settings, cx)),
        );
        if let Some(running) = &environment.running {
            let stop_key = format!("{name}/stop");
            let stopping = self.busy.contains_key(&stop_key) || running.stage == Stage::Stopping;
            actions = actions.child(settings_button).child(icon_button(
                "sandbox-stop",
                Icon::Stop,
                if stopping {
                    "Stopping..."
                } else {
                    "Stop: QEMU is killed and the GPU goes back to the host"
                },
                Kind::Danger,
                !stopping,
                cx.listener(move |home, _, _, cx| {
                    home.request(stop_key.clone(), "Stopping", Request::Stop, cx);
                }),
            ));
            if running.stage == Stage::Ready && running.screen.is_some() {
                let show = name.clone();
                actions = actions.child(icon_button(
                    "sandbox-enter",
                    Icon::Play,
                    "Enter: show its screen",
                    Kind::Primary,
                    !stopping,
                    cx.listener(move |home, _, window, cx| home.open_screen(&show, window, cx)),
                ));
            }
            return actions;
        }
        let start = name.clone();
        let can_start =
            !busy && environment.image_ready && !self.any_running() && self.presets.build.is_none();
        actions.child(settings_button).child(icon_button(
            "sandbox-enter",
            Icon::Play,
            if busy {
                "Starting..."
            } else {
                "Enter: start it and show its screen"
            },
            Kind::Primary,
            can_start,
            cx.listener(move |home, _, _, cx| home.start(&start, cx)),
        ))
    }

    /// Why the selected sandbox cannot start, when that is not obvious.
    fn sandbox_notice(&self, environment: &Environment, cx: &mut Context<Self>) -> Option<Div> {
        if environment.running.is_some() {
            return None;
        }
        let preset = environment.image.clone();
        if !environment.image_ready {
            if let Some(line) = self.build_line(&preset) {
                return Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_3()
                        .rounded_lg()
                        .bg(Theme::warning().opacity(0.08))
                        .border_1()
                        .border_color(Theme::warning().opacity(0.35))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(Theme::warning())
                                .child(format!(
                                    "The image of {preset} is building; the sandbox can start once it is ready"
                                )),
                        )
                        .child(line),
                );
            }
            let can_build = !self.any_running() && self.presets.build.is_none();
            return Some(
                ui::notice(
                    Theme::warning(),
                    format!("The image of {preset} is not built"),
                    "The sandbox can start once its preset's image is built.",
                )
                .child(button(
                    "sandbox-build",
                    "Build image",
                    Kind::Primary,
                    can_build,
                    cx.listener(move |home, _, _, cx| home.build_image(&preset, cx)),
                )),
            );
        }
        if let Some(build) = &self.presets.build {
            return Some(ui::notice(
                Theme::accent(),
                format!("The image of {} is building", build.preset),
                "Sandboxes start once the build ends.",
            ));
        }
        if let Some(running) = self.environments.iter().find(|e| e.running.is_some()) {
            return Some(ui::notice(
                Theme::accent(),
                format!("{} is running", running.name),
                "One sandbox runs at a time.",
            ));
        }
        None
    }

    /// The journal lines `filter` keeps, newest first, each copied by a
    /// click, and their text.
    fn journal_lines(&self, filter: &Filter) -> (gpui::Stateful<Div>, String) {
        let mut lines = div()
            .id("journal")
            .flex()
            .flex_col()
            .size_full()
            .gap_0p5()
            .overflow_y_scroll()
            .track_scroll(&self.journal_scroll)
            .text_sm();
        let mut shown_text = String::new();
        let entries = self.journal.as_ref().map(|(_, e)| e.as_slice());
        match entries {
            None => {
                lines = lines.child(div().text_color(Theme::muted()).child("Loading..."));
            }
            Some([]) => {
                lines = lines.child(
                    div()
                        .text_color(Theme::muted())
                        .child("Nothing has happened to this sandbox yet."),
                );
            }
            Some(entries) => {
                // Times line up in a column as wide as the longest of them:
                // a time of day, or a date with it for older lines.
                let today = Local::now().date_naive();
                let dated = entries.iter().any(|e| {
                    i64::try_from(e.time)
                        .ok()
                        .and_then(|t| Local.timestamp_opt(t, 0).single())
                        .is_some_and(|t| t.date_naive() != today)
                });
                let column = px(if dated { 104. } else { 64. });
                // The newest first, so the latest event is always in sight.
                for (i, entry) in entries
                    .iter()
                    .rev()
                    .filter(|e| filter.keeps(e.time, &e.text))
                    .enumerate()
                {
                    let line = format!("{} {}", journal_time(entry.time), entry.text);
                    shown_text.push_str(&line);
                    shown_text.push('\n');
                    lines = lines.child(
                        div()
                            .id(("journal-line", i))
                            .flex()
                            .gap_3()
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|row| row.bg(Theme::raised()))
                            .tooltip(ui::tooltip("Click to copy this line".into()))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    line.clone(),
                                ));
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .w(column)
                                    .text_color(Theme::subtle())
                                    .child(journal_time(entry.time)),
                            )
                            .child(div().min_w_0().child(entry.text.clone())),
                    );
                }
            }
        }
        (lines, shown_text)
    }

    fn journal_card(&self, cx: &mut Context<Self>) -> Div {
        let filter = Filter::new(self.journal_filter.read(cx).text());
        let (lines, shown_text) = self.journal_lines(&filter);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_3()
            .rounded_lg()
            .bg(Theme::surface())
            .border_1()
            .border_color(Theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Journal"),
                    )
                    .child(div().flex_1().child(self.journal_filter.clone()))
                    .child(icon_button(
                        "journal-copy",
                        Icon::Copy,
                        "Copy every line shown",
                        Kind::Ghost,
                        !shown_text.is_empty(),
                        move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                shown_text.clone(),
                            ));
                        },
                    )),
            )
            .children(
                filter
                    .problem
                    .map(|problem| div().text_xs().text_color(Theme::warning()).child(problem)),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(lines)
                    .children(ui::scrollbar(&self.journal_scroll)),
            )
    }

    /// The right side for the selected sandbox.
    pub(crate) fn sandbox_pane(&self, cx: &mut Context<Self>) -> Div {
        let environment = self
            .selected
            .as_ref()
            .and_then(|name| self.environments.iter().find(|e| e.name == *name));
        let Some(environment) = environment else {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                    if self.connected.is_none() {
                        "Loading..."
                    } else {
                        "No sandbox selected"
                    },
                ))
                .when(self.connected == Some(true), |empty| {
                    empty.child(
                        div()
                            .text_sm()
                            .text_color(Theme::muted())
                            .child("Add a sandbox from a preset to start."),
                    )
                });
        };
        let stage = environment.running.as_ref().map(|r| r.stage);
        let detail = match &environment.running {
            Some(running) => format!(
                "Preset {}, internet {}",
                environment.image,
                if running.internet {
                    "allowed"
                } else {
                    "cut off"
                }
            ),
            None => format!("Preset {}", environment.image),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
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
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(environment.name.clone()),
                                    )
                                    .child(self.state_badge(environment)),
                            )
                            .child(div().text_sm().text_color(Theme::muted()).child(detail)),
                    )
                    .child(self.sandbox_actions(environment, cx)),
            )
            .when(environment.image_ready || stage.is_some(), |pane| {
                pane.child(stage_line(&environment.name, stage))
            })
            .children(self.sandbox_notice(environment, cx))
            .children(self.processes_card(cx))
            .child(self.journal_card(cx))
    }

    /// The presets to add a sandbox from, `chosen` marked.
    fn preset_choices(&self, chosen: Option<&str>, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        // A long list scrolls on its own, so the name and the buttons stay
        // in sight.
        let mut list = div()
            .id("preset-choices")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(260.))
            .overflow_y_scroll();
        match &self.presets_list() {
            Ok(presets) if presets.is_empty() => {
                list = list.child(
                    div()
                        .text_sm()
                        .text_color(Theme::muted())
                        .child("No presets yet. Create one first."),
                );
            }
            Ok(presets) => {
                for preset in presets {
                    let selected = chosen == Some(preset.as_str());
                    let state = if self.images.contains(preset) {
                        badge("ready", Theme::good())
                    } else if self.building(preset) {
                        badge("building", Theme::warning())
                    } else {
                        badge("not built", Theme::subtle())
                    };
                    let pick = preset.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("pick-{preset}")))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .cursor_pointer()
                            .map(|row| {
                                if selected {
                                    row.bg(Theme::accent().opacity(0.08))
                                        .border_color(Theme::accent().opacity(0.6))
                                } else {
                                    row.border_color(Theme::border())
                                        .hover(|s| s.bg(Theme::raised()))
                                }
                            })
                            .on_click(cx.listener(move |home, _, _, cx| {
                                if let Some(Modal::Add { preset, .. }) = home.modal.as_mut() {
                                    *preset = Some(pick.clone());
                                    cx.notify();
                                }
                            }))
                            .child(div().font_weight(FontWeight::MEDIUM).child(preset.clone()))
                            .child(state),
                    );
                }
            }
            Err(reason) => {
                list = list.child(
                    div()
                        .text_sm()
                        .text_color(Theme::warning())
                        .child(reason.clone()),
                );
            }
        }
        list
    }

    fn add_dialog(
        &self,
        chosen: Option<&str>,
        name: &gpui::Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> Div {
        let list = self.preset_choices(chosen, cx);
        let typed = name.read(cx).text().trim().to_owned();
        let final_name = match (typed.is_empty(), chosen) {
            (false, _) => Some(typed.clone()),
            (true, Some(preset)) => Some(self.suggested_name(preset)),
            (true, None) => None,
        };
        let problem = match &final_name {
            Some(n) if !valid_name(n) => {
                Some("Names are lowercase letters, digits and hyphens, up to 32.")
            }
            Some(n) if self.environments.iter().any(|e| e.name == *n) => {
                Some("A sandbox of that name exists.")
            }
            _ => None,
        };
        let not_built = chosen.is_some_and(|p| !self.images.contains(&p.to_owned()));
        let create = chosen
            .zip(final_name.clone())
            .filter(|_| problem.is_none())
            .map(|(p, n)| (p.to_owned(), n));
        let name_hint = chosen.map_or_else(
            || "Pick a preset first.".to_owned(),
            |p| format!("Left empty, it is {}.", self.suggested_name(p)),
        );
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(Theme::muted())
                            .child("Preset"),
                    )
                    .child(list),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(Theme::muted())
                            .child("Name"),
                    )
                    .child(name.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(problem.map_or(Theme::subtle(), |_| Theme::warning()))
                            .child(problem.map_or(name_hint, str::to_owned)),
                    ),
            )
            .when(not_built, |body| {
                body.child(div().text_sm().text_color(Theme::muted()).child(
                    "Its image is not ready yet. The sandbox appears at once and can start \
                     once the image is built.",
                ))
            });
        dialog("Add sandbox", body, self.add_buttons(create, cx))
    }

    /// The buttons of the add dialog; `create` is the preset and name the
    /// Create button uses, when the form is complete.
    fn add_buttons(&self, create: Option<(String, String)>, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(button(
                "add-new-preset",
                "Create preset",
                Kind::Ghost,
                !self.editing_preset() && self.presets_list().is_ok(),
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    home.page = Page::AppSettings;
                    home.app_tab = AppTab::Presets;
                    home.open_editor(None, cx);
                }),
            ))
            .child(div().flex_1())
            .child(button(
                "add-cancel",
                "Cancel",
                Kind::Ghost,
                true,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    cx.notify();
                }),
            ))
            .child(button(
                "add-create",
                "Create",
                Kind::Primary,
                create.is_some(),
                cx.listener(move |home, _, _, cx| {
                    if let Some((preset, name)) = &create {
                        home.create(preset, name, cx);
                    }
                }),
            ))
    }

    fn sandbox_settings_dialog(
        &self,
        name: &str,
        rename: &gpui::Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> Div {
        let environment = self.environments.iter().find(|e| e.name == name);
        let (Some(environment), Ok((_, gpus))) = (environment, &self.settings) else {
            return dialog(
                "Sandbox settings",
                div().child("The sandbox is gone."),
                div().flex().justify_end().child(button(
                    "settings-close",
                    "Close",
                    Kind::Ghost,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.modal = None;
                        cx.notify();
                    }),
                )),
            );
        };
        let running = environment.running.is_some();
        let typed = rename.read(cx).text().trim().to_owned();
        let to = if typed.is_empty() || running {
            name.to_owned()
        } else {
            typed
        };
        let problem = if to == name {
            None
        } else if !valid_name(&to) {
            Some("Names are lowercase letters, digits and hyphens, up to 32.")
        } else if self.environments.iter().any(|e| e.name == to) {
            Some("A sandbox of that name exists.")
        } else {
            None
        };
        let dirty = to != name || self.sandbox_draft.as_ref() != Some(&environment.settings);
        let name_card = ui::card("Name", None)
            .child(if running {
                div()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("It can be renamed once it stops.")
            } else {
                div().child(rename.clone())
            })
            .children(problem.map(|p| div().text_xs().text_color(Theme::warning()).child(p)));
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(name_card)
            .child(self.settings_form(Target::Sandbox, &environment.settings, gpus, cx));
        body = body.child(self.discard_card(name, running, cx));
        if running {
            body = body.child(div().text_sm().text_color(Theme::muted()).child(
                "It runs now: the GPU and the screen apply from its next start, \
                 the internet at once.",
            ));
        }
        let save = name.to_owned();
        let buttons = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(button(
                "sandbox-settings-cancel",
                "Cancel",
                Kind::Ghost,
                !self.saving,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    home.sandbox_draft = None;
                    home.confirm = None;
                    cx.notify();
                }),
            ))
            .child(button(
                "sandbox-settings-save",
                if self.saving { "Saving..." } else { "Save" },
                Kind::Primary,
                dirty && problem.is_none() && !self.saving,
                cx.listener(move |home, _, _, cx| home.save_sandbox_settings(&save, &to, cx)),
            ));
        dialog(format!("{name}: settings"), body, buttons)
    }

    fn close_dialog(&self, cx: &mut Context<Self>) -> Div {
        let running = self
            .environments
            .iter()
            .find(|e| e.running.is_some())
            .map_or_else(String::new, |e| e.name.clone());
        let body = div().text_sm().text_color(Theme::muted()).child(format!(
            "{running} keeps the GPU, its sound and its network while it runs. \
                 Left running, it stays out of sight until the app opens again."
        ));
        let buttons = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(button(
                "close-cancel",
                "Cancel",
                Kind::Ghost,
                true,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    cx.notify();
                }),
            ))
            .child(button(
                "close-keep",
                "Keep it running",
                Kind::Secondary,
                true,
                cx.listener(|home, _, _, cx| {
                    home.closing = true;
                    cx.quit();
                }),
            ))
            .child(button(
                "close-stop",
                "Stop it and close",
                Kind::Danger,
                true,
                cx.listener(|home, _, _, cx| {
                    home.closing = true;
                    cx.spawn(async move |_, cx| {
                        cx.background_executor()
                            .spawn(async { client::command(&Request::Stop) })
                            .await
                            .ok();
                        cx.update(|cx| cx.quit()).ok();
                    })
                    .detach();
                }),
            ));
        dialog(format!("{running} is running"), body, buttons)
    }

    /// The open dialog over the window, if any.
    pub(crate) fn modal_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = match self.modal.as_ref()? {
            Modal::Add { preset, name } => self.add_dialog(preset.as_deref(), &name.clone(), cx),
            Modal::SandboxSettings { name, rename } => {
                self.sandbox_settings_dialog(&name.clone(), &rename.clone(), cx)
            }
            Modal::Transfer { name } => self.transfer_dialog(&name.clone(), cx),
            Modal::Close => self.close_dialog(cx),
        };
        Some(
            div()
                .id("modal")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::black().opacity(0.6))
                .occlude()
                .child(dialog)
                .into_any_element(),
        )
    }
}

/// A dialog box: a title, its body and a row of buttons.
pub(crate) fn dialog(title: impl Into<SharedString>, body: Div, buttons: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .w(px(620.))
        .max_h(px(680.))
        .p_5()
        .rounded_xl()
        .bg(Theme::background())
        .border_1()
        .border_color(Theme::border())
        .shadow_lg()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(
            div()
                .id("dialog-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        )
        .child(buttons)
}
