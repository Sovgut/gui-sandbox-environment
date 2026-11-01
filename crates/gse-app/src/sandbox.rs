//! The selected sandbox's journal, and the dialogs over the window: adding
//! a sandbox, a sandbox's settings and files as panels at the right, and
//! the questions before playing or closing.

use chrono::{Local, TimeZone as _};
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_kit::component::Sizable as _;
use gse_protocol::{JournalEntry, Request, valid_name};

use crate::client;
use crate::icons::Icon;
use crate::journal::{Filter, Period};
use crate::settings::{Target, group};
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, button, icon_button};
use crate::{AppTab, Confirm, Home, Modal, Page};

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

impl Home {
    /// Shows the sandbox `name` on the right, leaving the screen of another.
    pub(crate) fn select(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((shown, _)) = &self.screen {
            if shown == name {
                return;
            }
            self.close_screen(window, cx);
        }
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
        let Some(first) = self
            .app_config
            .first_set
            .as_ref()
            .filter(|s| presets.contains(s))
            .or_else(|| presets.iter().find(|p| self.images.contains(p)))
            .or_else(|| presets.first())
            .cloned()
        else {
            self.page = Page::AppSettings;
            self.app_tab = AppTab::Presets;
            if !self.editing_preset() {
                self.new_set(cx);
            }
            self.say(
                "Make a set of apps first: sandboxes are made from sets",
                false,
            );
            cx.notify();
            return;
        };
        let suggested = self.suggested_name(&first);
        let name = cx.new(|cx| {
            let mut input = TextInput::new(suggested.clone(), cx);
            input.set_text(suggested, cx);
            input
        });
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        self.modal = Some(Modal::Add {
            preset: Some(first),
            name,
            details: false,
        });
        cx.notify();
    }

    /// Picks `pick` in the add dialog; a suggested name follows the set, a
    /// typed one stays.
    fn pick_set(&mut self, pick: &str, cx: &mut Context<Self>) {
        let suggested = self.suggested_name(pick);
        if let Some(Modal::Add { preset, name, .. }) = self.modal.as_mut() {
            let typed = name.read(cx).text().trim().to_owned();
            let was_suggested = preset
                .as_deref()
                .is_none_or(|p| typed.starts_with(&format!("{p}-")));
            if typed.is_empty() || was_suggested {
                name.update(cx, |input, cx| input.set_text(suggested, cx));
            }
            *preset = Some(pick.to_owned());
            cx.notify();
        }
    }

    /// The name a new sandbox from `preset` gets unless the user gives one.
    fn suggested_name(&self, preset: &str) -> String {
        (1..=self.environments.len() + 1)
            .map(|n| format!("{preset}-{n}"))
            .find(|n| !self.environments.iter().any(|e| e.name == *n))
            .unwrap_or_default()
    }

    pub(crate) fn create(&mut self, preset: &str, name: &str, cx: &mut Context<Self>) {
        let request = Request::Create {
            name: name.to_owned(),
            image: preset.to_owned(),
        };
        self.modal = None;
        self.selected = Some(name.to_owned());
        self.page = Page::Sandbox;
        self.journal = None;
        self.request(format!("{name}/create"), "Creating", request, cx);
        // A set nothing has built yet starts building, so the sandbox can
        // play once it is done; the build waits while a sandbox runs.
        let unbuilt = !self.images.iter().any(|i| i == preset) && !self.building(preset);
        if unbuilt && !self.any_running() && self.presets.build.is_none() {
            self.build_image(preset, cx);
        }
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
                    .filter(|e| self.journal_period.keeps(e.time) && filter.keeps(e.time, &e.text))
                    .enumerate()
                {
                    let line = format!("{} {}", journal_time(entry.time), entry.text);
                    shown_text.push_str(&line);
                    shown_text.push('\n');
                    let marks: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = filter
                        .matches(&entry.text)
                        .into_iter()
                        .map(|range| {
                            (
                                range,
                                gpui::HighlightStyle {
                                    background_color: Some(Theme::warning().opacity(0.25)),
                                    ..gpui::HighlightStyle::default()
                                },
                            )
                        })
                        .collect();
                    lines = lines.child(
                        div()
                            .id(("journal-line", i))
                            .group("journal-line")
                            .flex()
                            .gap_4()
                            .px_1p5()
                            .py_1()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|row| row.bg(Theme::hover()))
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
                            .child(div().flex_1().min_w_0().child(
                                gpui::StyledText::new(entry.text.clone()).with_highlights(marks),
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(Theme::muted())
                                    .opacity(0.)
                                    .group_hover("journal-line", |s| s.opacity(1.))
                                    .child("Copy line"),
                            ),
                    );
                }
            }
        }
        (lines, shown_text)
    }

    /// The time range the journal shows, as a button with a menu.
    fn period_picker(&self, cx: &mut Context<Self>) -> Div {
        let mut picker = div().relative().flex_none().child(
            button(
                "journal-period",
                self.journal_period.text(),
                Kind::Secondary,
                true,
                cx.listener(|home, _, _, cx| {
                    home.period_open = !home.period_open;
                    cx.notify();
                }),
            )
            .icon(gpui_kit::component::IconName::ChevronDown),
        );
        if self.period_open {
            let mut menu = div()
                .id("journal-period-menu")
                .absolute()
                .top(px(38.))
                .right_0()
                .w(px(150.))
                .p_1()
                .rounded(px(10.))
                .bg(Theme::popover())
                .border_1()
                .border_color(Theme::line())
                .shadow_lg()
                .occlude();
            for period in Period::ALL {
                menu = menu.child(
                    div()
                        .id(period.text())
                        .px_2p5()
                        .py_1p5()
                        .rounded(px(6.))
                        .text_sm()
                        .cursor_pointer()
                        .when(period == self.journal_period, |i| {
                            i.font_weight(FontWeight::SEMIBOLD).bg(Theme::selected())
                        })
                        .hover(|i| i.bg(Theme::raised()))
                        .on_click(cx.listener(move |home, _, _, cx| {
                            home.journal_period = period;
                            home.period_open = false;
                            cx.notify();
                        }))
                        .child(period.text()),
                );
            }
            picker = picker.child(gpui::deferred(menu).with_priority(1));
        }
        picker
    }

    /// The Activity tab: the journal with its filter.
    pub(crate) fn activity_body(&self, cx: &mut Context<Self>) -> Div {
        let filter = Filter::new(self.journal_filter.read(cx).text());
        let (lines, shown_text) = self.journal_lines(&filter);
        let count = shown_text.lines().count();
        let total = self.journal.as_ref().map_or(0, |(_, e)| e.len());
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3p5()
                    .py_3()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .child(self.journal_filter.clone())
                            .when(filter.expression, |field| {
                                field.child(
                                    div()
                                        .absolute()
                                        .right(px(8.))
                                        .top(px(7.))
                                        .px_1p5()
                                        .rounded(px(5.))
                                        .bg(Theme::accent().opacity(0.15))
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(Theme::accent_text())
                                        .child("regex"),
                                )
                            }),
                    )
                    .child(self.period_picker(cx))
                    .child(icon_button(
                        "journal-copy",
                        Icon::Copy,
                        "Copy every line shown",
                        Kind::Secondary,
                        !shown_text.is_empty(),
                        move |_, _, cx| crate::shell::copy(shown_text.clone(), cx),
                    )),
            )
            .child(
                div()
                    .px_3p5()
                    .pb_2()
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(match filter.problem {
                        Some(problem) => div().text_color(Theme::warning()).child(problem),
                        None => div().child(format!(
                            "{count} of {total} lines \u{b7} @18:00-19:30 limits the time, \
                             the rest is a regular expression"
                        )),
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_2()
                    .child(lines)
                    .children(ui::scrollbar(&self.journal_scroll)),
            )
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
                        .child("No sets of apps yet. Make one first."),
                );
            }
            Ok(presets) => {
                for preset in presets {
                    let selected = chosen == Some(preset.as_str());
                    let (state, color) = self.set_state(preset);
                    let pick = preset.clone();
                    let apps: Vec<&str> = self
                        .preset_apps(preset)
                        .unwrap_or_default()
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect();
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("pick-{preset}")))
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_3p5()
                            .px_4()
                            .py_3p5()
                            .rounded(px(12.))
                            .border_2()
                            .cursor_pointer()
                            .map(|row| {
                                if selected {
                                    row.bg(Theme::chosen()).border_color(Theme::accent())
                                } else {
                                    row.bg(Theme::panel())
                                        .border_color(Theme::line())
                                        .hover(|s| s.bg(Theme::raised()))
                                }
                            })
                            .on_click(cx.listener(move |home, _, _, cx| home.pick_set(&pick, cx)))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(40.))
                                    .rounded(px(10.))
                                    .bg(crate::shell::cover(preset)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(preset.clone()),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .text_color(Theme::muted())
                                            .child(apps.join(", ")),
                                    ),
                            )
                            .child(div().flex_none().text_xs().text_color(color).child(state)),
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
        details: bool,
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
        let make_set = div()
            .id("add-new-preset")
            .text_sm()
            .text_color(Theme::accent())
            .cursor_pointer()
            .hover(|l| l.text_color(Theme::accent_text()))
            .on_click(cx.listener(|home, _, _, cx| {
                home.modal = None;
                home.page = Page::AppSettings;
                home.app_tab = AppTab::Presets;
                if !home.editing_preset() {
                    home.new_set(cx);
                }
            }))
            .child("+ Make your own set of apps");
        let label = |text: &'static str| {
            div()
                .text_sm()
                .text_color(Theme::secondary_text())
                .child(text)
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(label("Start from"))
                    .child(list)
                    .child(make_set),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(label("Name"))
                    .child(name.clone())
                    .children(
                        problem.map(|p| div().text_xs().text_color(Theme::warning()).child(p)),
                    ),
            )
            .child(self.add_details(details, cx))
            .when(not_built, |body| {
                body.child(div().text_sm().text_color(Theme::muted()).child(
                    "Its set of apps is not built yet: it starts building now, and the \
                     sandbox can play once it is done.",
                ))
            });
        dialog("New sandbox", body, Self::add_buttons(create, cx))
    }

    /// What every new sandbox starts with, folded under one line.
    fn add_details(&self, open: bool, cx: &mut Context<Self>) -> Div {
        let row = div()
            .id("add-details")
            .flex()
            .items_center()
            .gap_1p5()
            .text_sm()
            .text_color(Theme::secondary_text())
            .cursor_pointer()
            .on_click(cx.listener(|home, _, _, cx| {
                if let Some(Modal::Add { details, .. }) = home.modal.as_mut() {
                    *details = !*details;
                    cx.notify();
                }
            }))
            .child(
                gpui_kit::component::Icon::new(if open {
                    gpui_kit::component::IconName::ChevronDown
                } else {
                    gpui_kit::component::IconName::ChevronRight
                })
                .size(px(14.)),
            )
            .child("Graphics, screen and internet (as for every new sandbox)");
        let mut box_ = div().flex().flex_col().gap_2().child(row);
        if open && let Ok((settings, gpus)) = &self.settings {
            let gpu = gpus
                .iter()
                .find(|g| g.address == settings.gpu)
                .map_or_else(|| "None".to_owned(), crate::settings::gpu_title);
            let line = |label: &'static str, value: String| {
                div()
                    .flex()
                    .justify_between()
                    .text_sm()
                    .child(div().text_color(Theme::muted()).child(label))
                    .child(value)
            };
            box_ = box_.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .px_3()
                    .py_2p5()
                    .rounded(px(10.))
                    .bg(Theme::panel())
                    .child(line("Graphics card", gpu))
                    .child(line(
                        "Screen",
                        crate::shell::mode_text(&settings.screen_mode),
                    ))
                    .child(line(
                        "Internet",
                        if settings.internet {
                            "Allowed"
                        } else {
                            "Cut off"
                        }
                        .into(),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::subtle())
                            .child("Change them for this sandbox in its settings once it exists."),
                    ),
            );
        }
        box_
    }

    /// The buttons of the add dialog; `create` is the preset and name the
    /// Create button uses, when the form is complete.
    fn add_buttons(create: Option<(String, String)>, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2p5()
            .child(button(
                "add-cancel",
                "Cancel",
                Kind::Secondary,
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

    /// The control of a reset or delete row: its button, or the question
    /// and buttons of the second click.
    fn discard_control(
        &self,
        name: &str,
        what: Confirm,
        running: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let busy = self.busy.contains_key(name);
        let target = name.to_owned();
        let (label, doing, filled) = match what {
            Confirm::Reset => ("Reset", "Resetting", false),
            Confirm::Delete => ("Delete", "Deleting", true),
        };
        let waiting = self
            .confirm
            .as_ref()
            .is_some_and(|(n, c)| n == name && *c == what);
        if waiting {
            return div()
                .flex()
                .items_center()
                .gap_2()
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
                    format!("{label} now"),
                    Kind::Danger,
                    !busy,
                    cx.listener(move |home, _, _, cx| {
                        let request = match what {
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
                ));
        }
        let ask = button(
            match what {
                Confirm::Reset => "sandbox-reset",
                Confirm::Delete => "sandbox-delete",
            },
            label,
            Kind::Danger,
            !busy && !running,
            cx.listener(move |home, _, _, cx| {
                home.confirm = Some((target.clone(), what));
                cx.notify();
            }),
        )
        .small();
        div().child(if filled {
            ask.bg(Theme::danger()).text_color(Theme::on_danger())
        } else {
            ask
        })
    }

    /// Reset and delete, at the bottom of the settings of `name`.
    fn danger_zone(&self, name: &str, running: bool, cx: &mut Context<Self>) -> Div {
        let line = |title: &'static str, hint: &'static str, control: Div| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_3p5()
                .py_3()
                .border_t_1()
                .border_color(Theme::danger().opacity(0.15))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().text_sm().child(title))
                        .child(
                            div()
                                .text_xs()
                                .text_color(Theme::muted())
                                .child(if running {
                                    "Waits until it stops"
                                } else {
                                    hint
                                }),
                        ),
                )
                .child(control)
        };
        div()
            .flex()
            .flex_col()
            .rounded(px(12.))
            .border_1()
            .border_color(Theme::danger().opacity(0.3))
            .child(
                div()
                    .px_3p5()
                    .py_3()
                    .text_xs()
                    .text_color(Theme::danger_text())
                    .child("Danger zone"),
            )
            .child(line(
                "Reset",
                "Back to a clean copy of its set; everything inside is lost",
                self.discard_control(name, Confirm::Reset, running, cx),
            ))
            .child(line(
                "Delete",
                "Removes the sandbox and its disk",
                self.discard_control(name, Confirm::Delete, running, cx),
            ))
    }

    /// The name of the sandbox in its settings, renamed in place.
    fn rename_section(
        &self,
        environment: &gse_protocol::Environment,
        rename: &gpui::Entity<TextInput>,
        running: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = environment.name.as_str();
        let typed = rename.read(cx).text().trim().to_owned();
        let to = if typed.is_empty() {
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
        let created = i64::try_from(environment.created)
            .ok()
            .and_then(|t| Local.timestamp_opt(t, 0).single())
            .map(|t| format!(" \u{b7} created {}", t.format("%-d %b")))
            .unwrap_or_default();
        let save = name.to_owned();
        let can_rename = to != name && problem.is_none() && !running && !self.saving;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().text_color(Theme::muted()).child("Name"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(rename.clone()))
                    .when(to != name, |r| {
                        r.child(button(
                            "sandbox-rename",
                            "Rename",
                            Kind::Primary,
                            can_rename,
                            cx.listener(move |home, _, _, cx| {
                                home.save_sandbox_settings(&save, &to, cx);
                            }),
                        ))
                    }),
            )
            .children(problem.map(|p| div().text_xs().text_color(Theme::warning()).child(p)))
            .when(running, |s| {
                s.child(
                    div()
                        .text_xs()
                        .text_color(Theme::muted())
                        .child("It can be renamed once it stops."),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(format!("Set of apps: {}{created}", environment.image)),
            )
    }

    fn sandbox_settings_sheet(
        &self,
        name: &str,
        rename: &gpui::Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> Div {
        let environment = self.environments.iter().find(|e| e.name == name);
        let (Some(environment), Ok((_, gpus))) = (environment, &self.settings) else {
            return sheet(
                "Sandbox settings",
                div()
                    .p_6()
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("The sandbox is gone."),
                None,
            );
        };
        let running = environment.running.is_some();
        let name_section = self.rename_section(environment, rename, running, cx);
        let body =
            div()
                .flex()
                .flex_col()
                .gap_5()
                .child(name_section)
                .child(self.graphics_rows(Target::Sandbox, gpus, cx))
                .child(group(
                    None,
                    vec![self.internet_row(
                        Target::Sandbox,
                        "Off cuts the network at once, even while running",
                        cx,
                    )],
                ))
                .when(running, |b| {
                    b.child(div().text_xs().text_color(Theme::warning_text()).child(format!(
                    "Graphics and screen apply at the next start; {name} is running now."
                )))
                })
                .child(self.danger_zone(name, running, cx));
        sheet(
            "Sandbox settings",
            div().flex().flex_col().flex_1().min_h_0().child(
                div()
                    .id("sandbox-settings-body")
                    .size_full()
                    .px_6()
                    .py_5()
                    .overflow_y_scroll()
                    .child(body),
            ),
            None,
        )
    }

    fn close_dialog(&self, cx: &mut Context<Self>) -> Div {
        let running = self
            .environments
            .iter()
            .find(|e| e.running.is_some())
            .map_or_else(String::new, |e| e.name.clone());
        let body = div().text_sm().text_color(Theme::muted()).child(format!(
            "You are closing the app window. {running} can go on without it, out of sight, \
             holding the graphics card, sound and network; open GSE again to see its screen."
        ));
        let buttons = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(button(
                "close-cancel",
                "Don't close",
                Kind::Ghost,
                true,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    cx.notify();
                }),
            ))
            .child(button(
                "close-keep",
                format!("Close, leave {running} running"),
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
                        cx.update(|cx| cx.quit());
                    })
                    .detach();
                }),
            ));
        dialog(
            format!("Close GSE while {running} is running?"),
            body,
            buttons,
        )
    }

    /// Playing `to` while `from` runs.
    fn switch_dialog(from: &str, to: &str, cx: &mut Context<Self>) -> Div {
        let body = div().text_sm().text_color(Theme::muted()).child(format!(
            "Only one sandbox can hold the graphics card at a time, and {from} has it now. \
             Stopping shuts {from} down like a computer; anything unsaved in its programs is lost."
        ));
        let target = to.to_owned();
        let buttons = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(button(
                "switch-cancel",
                "Don't start",
                Kind::Ghost,
                true,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    cx.notify();
                }),
            ))
            .child(button(
                "switch-go",
                format!("Stop {from}, start {to}"),
                Kind::Primary,
                true,
                cx.listener(move |home, _, _, cx| {
                    home.modal = None;
                    home.selected = Some(target.clone());
                    home.open_when_ready = Some(target.clone());
                    let start = target.clone();
                    home.run(
                        start.clone(),
                        "Starting",
                        move || {
                            client::command(&Request::Stop)?;
                            client::start(&start, |_| {})
                        },
                        cx,
                    );
                }),
            ));
        dialog(format!("Stop {from} to start {to}?"), body, buttons)
    }

    /// Playing `name` on the graphics card that drives the monitor.
    fn single_gpu_dialog(name: &str, cx: &mut Context<Self>) -> Div {
        let keys = div()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .text_color(Theme::muted())
            .child(ui::key("Left Ctrl"))
            .child("+")
            .child(ui::key("Right Ctrl"));
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_sm().text_color(Theme::muted()).child(
                "This computer has one graphics card, so the sandbox takes it whole: your \
                 desktop disappears until you come back. Apps you left open keep running; \
                 save your work in them first.",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .px_4()
                    .py_3p5()
                    .rounded(px(12.))
                    .bg(Theme::panel())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_sm().child("To come back to your desktop"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::muted())
                                    .child("Press both Ctrl keys together."),
                            ),
                    )
                    .child(keys),
            );
        let (go, once) = (name.to_owned(), name.to_owned());
        let buttons = div()
            .flex()
            .items_center()
            .gap_2()
            .child(button(
                "single-once",
                "Don't explain this again",
                Kind::Ghost,
                true,
                cx.listener(move |home, _, _, cx| {
                    home.app_config.single_gpu_explained = true;
                    if let Err(e) = home.app_config.save() {
                        home.say(format!("The setting was not saved: {e:#}"), true);
                    }
                    home.modal = None;
                    home.start(&once, cx);
                }),
            ))
            .child(div().flex_1())
            .child(button(
                "single-cancel",
                "Not now",
                Kind::Secondary,
                true,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    cx.notify();
                }),
            ))
            .child(button(
                "single-go",
                "Switch over",
                Kind::Primary,
                true,
                cx.listener(move |home, _, _, cx| {
                    home.modal = None;
                    home.start(&go, cx);
                }),
            ));
        dialog(format!("Your monitor goes over to {name}"), body, buttons)
    }

    /// The open dialog or side panel over the window, if any.
    pub(crate) fn modal_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let close = cx.listener(|home, _, _, cx| {
            home.modal = None;
            home.sandbox_draft = None;
            home.confirm = None;
            cx.notify();
        });
        let closer = |cx: &mut Context<Self>| {
            ui::small_icon_button(
                "sheet-close",
                Icon::Close,
                "Close",
                Kind::Ghost,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    home.sandbox_draft = None;
                    home.confirm = None;
                    cx.notify();
                }),
            )
        };
        let (layer, sheet) = match self.modal.as_ref()? {
            Modal::Add {
                preset,
                name,
                details,
            } => {
                let dialog = self.add_dialog(preset.as_deref(), &name.clone(), *details, cx);
                (
                    dialog.relative().child(
                        div()
                            .absolute()
                            .top(px(22.))
                            .right(px(22.))
                            .child(closer(cx)),
                    ),
                    false,
                )
            }
            Modal::SandboxSettings { name, rename } => (
                self.sandbox_settings_sheet(&name.clone(), &rename.clone(), cx),
                true,
            ),
            Modal::Transfer { name } => (
                sheet(
                    format!("Files \u{b7} {name}"),
                    self.files_body(&name.clone(), cx),
                    None,
                ),
                true,
            ),
            Modal::Close => (self.close_dialog(cx), false),
            Modal::RemoveAll {
                keep,
                confirm,
                sizes,
            } => (
                self.remove_dialog(*keep, &confirm.clone(), sizes.as_ref(), cx),
                false,
            ),
            Modal::Switch { from, to } => {
                (Self::switch_dialog(&from.clone(), &to.clone(), cx), false)
            }
            Modal::SingleGpu { name } => (Self::single_gpu_dialog(&name.clone(), cx), false),
        };
        let backdrop = div()
            .id("modal")
            .absolute()
            .inset_0()
            .rounded_tl(self.corners.top_left)
            .rounded_tr(self.corners.top_right)
            .rounded_bl(self.corners.bottom_left)
            .rounded_br(self.corners.bottom_right)
            .flex()
            .bg(gpui::rgb(0x05_06_0a).opacity(0.7))
            .occlude();
        Some(if sheet {
            backdrop
                .justify_end()
                .child(div().id("modal-outside").flex_1().h_full().on_click(close))
                .child(
                    layer.child(
                        div()
                            .absolute()
                            .top(px(16.))
                            .right(px(16.))
                            .child(closer(cx)),
                    ),
                )
                .into_any_element()
        } else {
            backdrop
                .items_center()
                .justify_center()
                .child(layer)
                .into_any_element()
        })
    }
}

/// A dialog box: a title, its body and a row of buttons.
pub(crate) fn dialog(title: impl Into<SharedString>, body: Div, buttons: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .w(px(580.))
        .max_h(px(680.))
        .p_7()
        .rounded(px(18.))
        .bg(Theme::dialog())
        .border_1()
        .border_color(Theme::line())
        .shadow_lg()
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::BOLD)
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

/// A panel at the right edge: a title, its body and an optional row of
/// buttons at the bottom.
pub(crate) fn sheet(title: impl Into<SharedString>, body: Div, buttons: Option<Div>) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(460.))
        .h_full()
        .bg(Theme::sidebar())
        .border_l_1()
        .border_color(Theme::line())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(60.))
                .px_6()
                .border_b_1()
                .border_color(Theme::border())
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .child(title.into()),
                ),
        )
        .child(body)
        .children(buttons.map(|b| {
            b.flex_none()
                .px_6()
                .py_4()
                .border_t_1()
                .border_color(Theme::border())
        }))
}
