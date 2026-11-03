//! The selected sandbox's journal, and the dialogs over the window: adding
//! a sandbox, a sandbox's settings and files as panels at the right, and
//! the questions before playing or closing.

use chrono::{Datelike as _, Local, TimeZone as _};
use gpui::{
    AnyElement, ClipboardItem, Context, Div, FontWeight, HighlightStyle, Hsla, SharedString,
    StyledText, Window, div, prelude::*, px, relative,
};
use gpui_kit::component::ActiveTheme as _;
use gse_protocol::{JournalEntry, Request, valid_name};

use crate::client;
use crate::icons::Icon;
use crate::journal::{Filter, Period};
use crate::presets::set_title;
use crate::settings::{Target, group};
use crate::text_input::{Look, TextInput};
use crate::ui::{self, Kind, Size, Theme, button, icon_button};
use crate::{AppTab, Confirm, Home, Modal, Page};

/// The width of the journal's column of times.
const TIME_WIDTH: f32 = 110.;

/// `time`, in seconds since the epoch, on the local clock.
fn local(time: u64) -> Option<chrono::DateTime<Local>> {
    i64::try_from(time)
        .ok()
        .and_then(|t| Local.timestamp_opt(t, 0).single())
}

/// A journal entry's time as its line shows it: "Today 18:12",
/// "Yesterday 18:12", "30 Oct 18:12", and the date alone for another
/// year's line.
fn journal_time(time: u64) -> String {
    let Some(when) = local(time) else {
        return String::new();
    };
    let today = crate::clock::now().date_naive();
    let day = when.date_naive();
    let clock = when.format("%H:%M");
    if day == today {
        format!("Today {clock}")
    } else if today.pred_opt() == Some(day) {
        format!("Yesterday {clock}")
    } else if day.year() == today.year() {
        format!("{} {clock}", when.format("%-d %b"))
    } else {
        when.format("%-d %b %Y").to_string()
    }
}

/// A journal line as it is copied: its full date and time, then its text.
fn copied_line(entry: &JournalEntry) -> String {
    local(entry.time).map_or_else(
        || entry.text.clone(),
        |when| format!("{} {}", when.format("%Y-%m-%d %H:%M:%S"), entry.text),
    )
}

/// `before`, `part` and `after` as one line of text, `part` in `strong`,
/// and in bold with `bold`: a word to type, or a folder named in a
/// sentence.
pub(crate) fn marked(
    before: &str,
    part: &str,
    after: &str,
    strong: Hsla,
    bold: bool,
) -> StyledText {
    let at = before.len();
    StyledText::new(format!("{before}{part}{after}")).with_highlights(vec![(
        at..at + part.len(),
        HighlightStyle {
            color: Some(strong),
            font_weight: bold.then_some(FontWeight::BOLD),
            ..HighlightStyle::default()
        },
    )])
}

/// `reason`, such as an error from the service, as the start of a
/// sentence.
pub(crate) fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How to come back from a sandbox that took the monitor: its menu, or
/// both Ctrl keys at once.
fn way_back() -> Div {
    let keys = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_size(px(13.))
        .text_color(Theme::muted())
        .child(ui::key("Left Ctrl"))
        .child("+")
        .child(ui::key("Right Ctrl"));
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3p5()
        .rounded(px(12.))
        .bg(Theme::well())
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().child("To come back to your desktop"))
                // Small text on the title's 14 px lines, as the design sets it.
                .child(
                    div()
                        .text_xs()
                        .line_height(px(17.))
                        .text_color(Theme::muted())
                        .child(
                            "Shut the sandbox down from its menu, or press both Ctrl keys \
                             together to stop it at once.",
                        ),
                ),
        )
        .child(keys)
}

/// A form field that cannot be typed into now, showing `text` in the
/// quiet colour: a running sandbox's name, the word of a removal under way.
pub(crate) fn still_field(text: String) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(36.))
        .px(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(Theme::line())
        .bg(Theme::background())
        .text_color(Theme::muted())
        .child(text)
}

/// The ground of a small square that stands for `name`, a set or a
/// program: one of a few dark tints, the same for a name wherever it shows.
pub(crate) fn tile_color(name: &str) -> Hsla {
    const TINTS: [u32; 8] = [
        0x2a_35_50, 0x2a_25_50, 0x3a_2d_2a, 0x2a_35_50, 0x2b_2b_33, 0x23_3a_33, 0x3d_2e_22,
        0x23_3a_33,
    ];
    let hash = name.bytes().fold(0_usize, |h, b| {
        h.wrapping_mul(31).wrapping_add(usize::from(b))
    });
    gpui::rgb(TINTS[hash % TINTS.len()]).into()
}

/// A set's apps on one line: the first five, and how many more there are.
fn apps_line(apps: &[&str]) -> String {
    const SHOWN: usize = 5;
    if apps.len() <= SHOWN {
        apps.join(", ")
    } else {
        format!(
            "{} and {} more",
            apps[..SHOWN].join(", "),
            apps.len() - SHOWN
        )
    }
}

/// What the journal says when `filter` and the period keep none of its
/// lines.
fn nothing_kept(filter: &Filter) -> String {
    if !filter.text.is_empty() && !filter.expression {
        format!(
            "Nothing matches: \"{}\" is not a valid regular expression, so it is matched as \
             plain text.",
            filter.text
        )
    } else if filter.narrows() {
        "No line matches the filter.".to_owned()
    } else {
        "Nothing happened in this period.".to_owned()
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
        // The field stays empty and offers the name it would take.
        let suggested = self.suggested_name(&first);
        let name = cx.new(|cx| TextInput::new(suggested, cx).look(Look::DialogField));
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        self.modal = Some(Modal::Add {
            preset: Some(first),
            name,
            details: false,
        });
        cx.notify();
    }

    /// Picks `pick` in the add dialog; the name offered follows the set.
    fn pick_set(&mut self, pick: &str, cx: &mut Context<Self>) {
        let suggested = self.suggested_name(pick);
        if let Some(Modal::Add { preset, name, .. }) = self.modal.as_mut() {
            name.update(cx, |input, cx| input.set_placeholder(suggested, cx));
            *preset = Some(pick.to_owned());
            cx.notify();
        }
    }

    /// The name a new sandbox from `preset` gets unless the user types one:
    /// the set's own word for the first of its sandboxes, then that word
    /// numbered from 2 (`desktop`, then `desktop-2`). The game launchers'
    /// sandboxes are `games`, as the tour names the first one.
    fn suggested_name(&self, preset: &str) -> String {
        let word = if preset == "launchers" {
            "games"
        } else {
            preset
        };
        let taken = |name: &str| self.environments.iter().any(|e| e.name == name);
        if !taken(word) && !self.environments.iter().any(|e| e.image == preset) {
            return word.to_owned();
        }
        (2..=self.environments.len() + 2)
            .map(|n| format!("{word}-{n}"))
            .find(|n| !taken(n))
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
    /// click; and how many there are, with their text as the copy button
    /// copies it.
    fn journal_lines(&self, filter: &Filter) -> (gpui::Stateful<Div>, usize, String) {
        let mut lines = div()
            .id("journal")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.journal_scroll)
            .text_size(px(13.));
        // What the list says in place of lines, where their text would start.
        let quiet = |text: String| {
            div()
                .px_3p5()
                .py(px(7.))
                .text_color(Theme::muted())
                .child(text)
        };
        let mut shown = 0;
        let mut shown_text = String::new();
        match self.journal.as_ref().map(|(_, e)| e.as_slice()) {
            None => lines = lines.child(quiet("Loading...".to_owned())),
            Some([]) => {
                lines = lines.child(quiet(
                    "Nothing has happened to this sandbox yet.".to_owned(),
                ));
            }
            Some(entries) => {
                // The newest first, so the latest event is always in sight.
                for entry in entries
                    .iter()
                    .rev()
                    .filter(|e| self.journal_period.keeps(e.time) && filter.keeps(e.time, &e.text))
                {
                    let line = copied_line(entry);
                    shown_text.push_str(&line);
                    shown_text.push('\n');
                    let marks: Vec<(std::ops::Range<usize>, HighlightStyle)> = filter
                        .matches(&entry.text)
                        .into_iter()
                        .map(|range| {
                            (
                                range,
                                HighlightStyle {
                                    background_color: Some(Theme::warning().opacity(0.25)),
                                    ..HighlightStyle::default()
                                },
                            )
                        })
                        .collect();
                    // A line under the pointer lights up across the whole
                    // panel; the rule under it keeps to the list's inset.
                    lines = lines.child(
                        div()
                            .id(("journal-line", shown))
                            .group("journal-line")
                            .flex_none()
                            .px_3p5()
                            .cursor_pointer()
                            .hover(|row| row.bg(Theme::hover()))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(line.clone()));
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .py(px(7.))
                                    .border_b_1()
                                    .border_color(Theme::divider())
                                    .child(
                                        div()
                                            .flex_none()
                                            .w(px(TIME_WIDTH))
                                            .text_color(Theme::subtle())
                                            .child(journal_time(entry.time)),
                                    )
                                    .child(div().flex_1().min_w_0().child(
                                        StyledText::new(entry.text.clone()).with_highlights(marks),
                                    ))
                                    .child(
                                        div()
                                            .flex_none()
                                            .pl_3()
                                            .text_xs()
                                            .text_color(Theme::muted())
                                            .opacity(0.)
                                            .group_hover("journal-line", |s| s.opacity(1.))
                                            .child("Copy line"),
                                    ),
                            ),
                    );
                    shown += 1;
                }
                if shown == 0 {
                    lines = lines.child(quiet(nothing_kept(filter)));
                }
            }
        }
        (lines, shown, shown_text)
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
            .size(Size::Toolbar)
            .dropdown()
            // As wide as the longest period, so it keeps its place when
            // another is picked.
            .w(px(106.))
            .pl(px(16.))
            .pr(px(2.))
            .gap_1(),
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
                    ui::menu_item(
                        period.text(),
                        period.text(),
                        None,
                        period == self.journal_period,
                    )
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.journal_period = period;
                        home.period_open = false;
                        cx.notify();
                    })),
                );
            }
            picker = picker.child(gpui::deferred(menu).with_priority(1));
        }
        picker
    }

    /// The line under the journal's filter: how many lines it shows of how
    /// many, and what the filter does or what is wrong with it; nothing
    /// while the journal is read, nor for an empty journal and no filter,
    /// which the list itself speaks of.
    fn filter_line(&self, filter: &Filter, shown: usize, mono: SharedString) -> Option<Div> {
        let total = self.journal.as_ref()?.1.len();
        let lines = |n: usize| if n == 1 { "line" } else { "lines" };
        let counted = if filter.narrows() || self.journal_period != Period::Everything {
            format!("{shown} of {total} {}", lines(total))
        } else {
            format!("{total} {}", lines(total))
        };
        let line = div()
            .flex()
            .gap_1()
            .px_3p5()
            .pb_2()
            .text_xs()
            .text_color(Theme::muted())
            .child(div().flex_none().child(counted));
        let does = match (&filter.problem, filter.summary()) {
            (Some(problem), _) => div().text_color(Theme::warning()).child(problem.clone()),
            // The `@` words as typed, in the field's monospace.
            (None, Some((times, does))) => div()
                .flex()
                .gap_1()
                .when(!times.is_empty(), |d| {
                    d.child(
                        div()
                            .font_family(mono)
                            .text_color(Theme::secondary_text())
                            .child(times),
                    )
                })
                .child(does),
            (None, None) => return (total > 0).then_some(line),
        };
        Some(
            line.child("\u{b7}")
                .child(div().flex_1().min_w_0().child(does)),
        )
    }

    /// The Activity tab: the journal with its filter, and what a click on
    /// it copies.
    pub(crate) fn activity_body(&self, cx: &mut Context<Self>) -> Div {
        let filter = Filter::new(self.journal_filter.read(cx).text());
        let (lines, shown, shown_text) = self.journal_lines(&filter);
        let summary = self.filter_line(&filter, shown, cx.theme().mono_font_family.clone());
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
                    // A pixel more than the other bars: the design's filter
                    // is that much taller than its buttons.
                    .py(px(13.))
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
                                        .top_0()
                                        .bottom_0()
                                        .right(px(11.))
                                        .flex()
                                        .items_center()
                                        .child(
                                            div()
                                                .px(px(6.))
                                                .py(px(2.))
                                                .rounded(px(5.))
                                                .bg(Theme::accent().opacity(0.15))
                                                .text_size(px(11.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(Theme::accent_text())
                                                .child("regex"),
                                        ),
                                )
                            }),
                    )
                    .child(self.period_picker(cx))
                    .child(
                        icon_button(
                            "journal-copy",
                            Icon::Copy,
                            "Copy every line shown",
                            Kind::Quiet,
                            shown > 0,
                            move |_, _, cx| crate::shell::copy(shown_text.clone(), cx),
                        )
                        .size(Size::Toolbar),
                    ),
            )
            .children(summary)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(lines)
                    .children(ui::scrollbar(&self.journal_scroll)),
            )
            .child(
                div()
                    .flex_none()
                    .px_3p5()
                    .py_2p5()
                    .border_t_1()
                    .border_color(Theme::divider())
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(
                        "Click a line to copy it. The copy button takes every line the filter \
                         shows.",
                    ),
            )
    }

    /// What a set's card says of it: "Preparing" with how far it got, as
    /// the sidebar says it, while it is built for the first time; else its
    /// state as the sets page says it.
    fn card_state(&self, preset: &str) -> (String, Hsla) {
        if self.building(preset) && !self.images.iter().any(|i| i == preset) {
            let text = self.build_percent(preset).map_or_else(
                || "Preparing".to_owned(),
                |percent| format!("Preparing, {percent}%"),
            );
            return (text, Theme::warning());
        }
        let (text, color) = self.set_state(preset);
        (text.to_owned(), color)
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
            .max_h(px(300.))
            .overflow_y_scroll();
        match &self.presets_list() {
            Ok(presets) if presets.is_empty() => {
                list = list.child(
                    div()
                        .text_size(px(13.))
                        .text_color(Theme::muted())
                        .child("No sets of apps yet. Make one first."),
                );
            }
            Ok(presets) => {
                for preset in presets {
                    let selected = chosen == Some(preset.as_str());
                    let (state, color) = self.card_state(preset);
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
                                        .border_color(Theme::selected())
                                        .hover(|s| s.bg(Theme::raised()))
                                }
                            })
                            .on_click(cx.listener(move |home, _, _, cx| home.pick_set(&pick, cx)))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(40.))
                                    .rounded(px(10.))
                                    .bg(tile_color(preset)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_size(px(15.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(set_title(preset)),
                                    )
                                    .child(
                                        div()
                                            .mt(px(2.))
                                            .truncate()
                                            .text_size(px(13.))
                                            .text_color(Theme::secondary_text())
                                            .child(apps_line(&apps)),
                                    ),
                            )
                            .child(div().flex_none().text_xs().text_color(color).child(state)),
                    );
                }
            }
            Err(reason) => {
                list = list.child(
                    div()
                        .text_size(px(13.))
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
        let unbuilt = chosen
            .filter(|p| !self.images.iter().any(|i| i == p))
            .map(|p| {
                if self.building(p) {
                    "Its set of apps is being built: the sandbox can play once it is done."
                } else {
                    "Its set of apps is not built yet: it starts building now, and the sandbox \
                     can play once it is done."
                }
            });
        let create = chosen
            .zip(final_name.clone())
            .filter(|_| problem.is_none())
            .map(|(p, n)| (p.to_owned(), n));
        let make_set = div()
            .id("add-new-preset")
            .text_size(px(13.))
            .text_color(Theme::link())
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
                .text_size(px(13.))
                .text_color(Theme::secondary_text())
                .child(text)
        };
        let body = div()
            .flex()
            .flex_col()
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
                    .mt_5()
                    .child(label("Name"))
                    .child(name.clone())
                    .children(
                        problem.map(|p| div().text_xs().text_color(Theme::warning()).child(p)),
                    ),
            )
            .child(self.add_details(details, cx).mt_3())
            .children(unbuilt.map(|note| {
                div()
                    .mt_3()
                    .text_size(px(13.))
                    .text_color(Theme::muted())
                    .child(note)
            }));
        dialog(
            DialogSize::Wide,
            "New sandbox",
            body,
            Self::add_buttons(create, cx),
        )
    }

    /// What every new sandbox starts with, folded under one line.
    fn add_details(&self, open: bool, cx: &mut Context<Self>) -> Div {
        // A filled triangle, turned down while open, as a browser marks a
        // part that folds; the drawing's own margin goes into the row's.
        let marker = ui::kit_icon(Icon::Play)
            .size(px(14.))
            .ml(px(-4.))
            .text_color(Theme::secondary_text())
            .when(open, |m| {
                m.rotate(gpui::radians(std::f32::consts::FRAC_PI_2))
            });
        let row = div()
            .id("add-details")
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(13.))
            .text_color(Theme::secondary_text())
            .cursor_pointer()
            .on_click(cx.listener(|home, _, _, cx| {
                if let Some(Modal::Add { details, .. }) = home.modal.as_mut() {
                    *details = !*details;
                    cx.notify();
                }
            }))
            .child(marker)
            .child("Graphics, screen and internet (as for every new sandbox)");
        let mut box_ = div().flex().flex_col().gap_2().child(row);
        if open && let Ok((settings, gpus)) = &self.settings {
            let gpu = gpus.iter().find(|g| g.address == settings.gpu).map_or_else(
                || "None".to_owned(),
                |g| crate::settings::gpu_title(g, gpus),
            );
            let line = |label: &'static str, value: String| {
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(13.))
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
                        self.screen_text(&settings.gpu, &settings.screen_mode),
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
            .child(
                button(
                    "add-cancel",
                    "Cancel",
                    Kind::Quiet,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.modal = None;
                        cx.notify();
                    }),
                )
                .size(Size::Dialog),
            )
            .child(
                button(
                    "add-create",
                    "Create",
                    Kind::Primary,
                    create.is_some(),
                    cx.listener(move |home, _, _, cx| {
                        if let Some((preset, name)) = &create {
                            home.create(preset, name, cx);
                        }
                    }),
                )
                .size(Size::Dialog)
                .px_5(),
            )
    }

    /// The control of a reset or delete row: its button, or the question
    /// and buttons of the second click, whose last button is the strongest.
    /// The service does neither while the sandbox runs.
    fn discard_control(
        &self,
        name: &str,
        what: Confirm,
        running: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let busy = self.busy.contains_key(name);
        let target = name.to_owned();
        let (label, doing, first) = match what {
            Confirm::Reset => ("Reset", "Resetting", Kind::Danger),
            Confirm::Delete => ("Delete", "Deleting", Kind::DangerFilled),
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
                    Kind::DangerFilled,
                    !busy && !running,
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
        div().child(button(
            match what {
                Confirm::Reset => "sandbox-reset",
                Confirm::Delete => "sandbox-delete",
            },
            label,
            first,
            !busy && !running,
            cx.listener(move |home, _, _, cx| {
                home.confirm = Some((target.clone(), what));
                cx.notify();
            }),
        ))
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
                "Back to a clean copy of its set; everything inside, /home included, is lost",
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
        let created = local(environment.created)
            .map(|t| format!(" \u{b7} created {}", t.format("%-d %b")))
            .unwrap_or_default();
        let save = name.to_owned();
        let can_rename = to != name && problem.is_none() && !self.saving;
        // A running sandbox keeps its name: the field only shows it.
        let field = if running {
            still_field(name.to_owned())
        } else {
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
                })
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().text_color(Theme::muted()).child("Name"))
            .child(field)
            .children(
                problem
                    .filter(|_| !running)
                    .map(|p| div().text_xs().text_color(Theme::warning()).child(p)),
            )
            .when(running, |s| {
                s.child(
                    div()
                        .text_xs()
                        .text_color(Theme::muted())
                        .child("It can be renamed once it stops."),
                )
            })
            .child(div().text_xs().text_color(Theme::muted()).child(marked(
                "Set of apps: ",
                &set_title(&environment.image),
                &created,
                Theme::soft_text(),
                false,
            )))
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
                SheetHead::Plain,
                "Sandbox settings",
                div()
                    .p_6()
                    .text_size(px(13.))
                    .text_color(Theme::muted())
                    .child("The sandbox is gone."),
                None,
            );
        };
        // While it runs, only the internet switch applies at once; the
        // rest is greyed out until it stops.
        let running = environment.running.is_some();
        let name_section = self.rename_section(environment, rename, running, cx);
        let body = div()
            .flex()
            .flex_col()
            .gap(px(22.))
            .child(name_section)
            .child(self.graphics_rows(Target::Sandbox, gpus, cx))
            .children(self.devices_rows(gpus, cx))
            .child(group(
                None,
                vec![self.internet_row(
                    Target::Sandbox,
                    "Off cuts the network at once, even while running",
                    cx,
                )],
            ))
            .child(self.danger_zone(name, running, cx));
        sheet(
            SheetHead::Plain,
            "Sandbox settings",
            div().flex().flex_col().flex_1().min_h_0().child(
                div()
                    .id("sandbox-settings-body")
                    .size_full()
                    .px_6()
                    .pt_2()
                    .pb_6()
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
        let body = dialog_text(format!(
            "You are closing the app window. {running} can go on without it, out of sight, \
             holding the graphics card, sound and network; open GSE again to see its screen."
        ));
        let buttons = div()
            .child(
                button(
                    "close-cancel",
                    "Don't close",
                    Kind::Ghost,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.modal = None;
                        cx.notify();
                    }),
                )
                .size(Size::Dialog)
                .px_4(),
            )
            .child(
                button(
                    "close-keep",
                    format!("Close, leave {running} running"),
                    Kind::Secondary,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.closing = true;
                        cx.quit();
                    }),
                )
                .size(Size::Dialog)
                .px_4(),
            )
            .child(
                button(
                    "close-stop",
                    "Stop it and close",
                    Kind::DangerFilled,
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
                )
                .size(Size::Dialog),
            );
        dialog(
            DialogSize::Small,
            format!("Close GSE while {running} is running?"),
            body,
            buttons,
        )
    }

    /// Playing `to` while `from` runs.
    fn switch_dialog(from: &str, to: &str, cx: &mut Context<Self>) -> Div {
        let body = dialog_text(format!(
            "Only one sandbox can hold the graphics card at a time, and {from} has it now. \
             Stopping shuts {from} down like a computer; anything unsaved in its programs is lost."
        ));
        let target = to.to_owned();
        let buttons = div()
            .child(
                button(
                    "switch-cancel",
                    "Don't start",
                    Kind::Quiet,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.modal = None;
                        cx.notify();
                    }),
                )
                .size(Size::Dialog)
                .px_4(),
            )
            .child(
                button(
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
                )
                .size(Size::Dialog),
            );
        dialog(
            DialogSize::Small,
            format!("Stop {from} to start {to}?"),
            body,
            buttons,
        )
    }

    /// What playing on the card that drives the monitor does: the only
    /// card of the computer, or the one its settings name among several,
    /// where another one would keep the desktop.
    fn monitor_handover(&self) -> String {
        let only_card = self
            .settings
            .as_ref()
            .is_ok_and(|(_, gpus)| gpus.len() <= 1);
        let (takes, keep) = if only_card {
            ("This computer has one graphics card", "")
        } else {
            (
                "The graphics card it plays on drives your monitor",
                " To keep your desktop, give it another graphics card in its settings.",
            )
        };
        format!(
            "{takes}, so the sandbox takes it whole, with your keyboard, mouse and sound. Your \
             desktop session closes as if you logged out: save your work and close your apps \
             first. When the sandbox stops, the login screen comes back. Files you put in its \
             Outbox meanwhile wait in its Files tab; files sent in before it starts are in its \
             Inbox.{keep}"
        )
    }

    /// Playing `name` on the graphics card that drives the monitor.
    /// `explained` is whether "Don't explain this again" is ticked; the app
    /// keeps it only when the user switches over.
    fn single_gpu_dialog(&self, name: &str, explained: bool, cx: &mut Context<Self>) -> Div {
        let tick = ui::checkbox(
            "single-explained",
            explained,
            true,
            div()
                .text_size(px(13.))
                .text_color(Theme::secondary_text())
                .child("Don't explain this again"),
            cx.listener(|home, on: &bool, _, cx| {
                if let Some(Modal::SingleGpu { explained, .. }) = home.modal.as_mut() {
                    *explained = *on;
                    cx.notify();
                }
            }),
        );
        let body = div()
            .flex()
            .flex_col()
            .gap(px(DialogSize::Large.gap()))
            .child(dialog_text(self.monitor_handover()))
            .child(way_back())
            .child(div().flex().child(tick));
        let go = name.to_owned();
        let buttons = div()
            .child(
                button(
                    "single-cancel",
                    "Not now",
                    Kind::Quiet,
                    true,
                    cx.listener(|home, _, _, cx| {
                        home.modal = None;
                        cx.notify();
                    }),
                )
                .size(Size::Dialog),
            )
            .child(
                button(
                    "single-go",
                    "Switch over",
                    Kind::Primary,
                    true,
                    cx.listener(move |home, _, _, cx| {
                        if explained {
                            home.app_config.single_gpu_explained = true;
                            if let Err(e) = home.app_config.save() {
                                home.say(format!("The setting was not saved: {e:#}"), true);
                            }
                        }
                        home.modal = None;
                        home.start(&go, cx);
                    }),
                )
                .size(Size::Dialog)
                .px_5(),
            );
        let icon = div()
            .flex()
            .items_center()
            .justify_center()
            .size(px(48.))
            .rounded(px(14.))
            .bg(Theme::chosen())
            .child(ui::icon(Icon::Monitor, Theme::accent()).size(px(24.)));
        dialog_with(
            DialogSize::Large,
            Some(icon),
            format!("Your monitor goes over to {name}"),
            body,
            buttons,
        )
    }

    /// Closes the open dialog or panel, and what it was editing.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        self.sandbox_draft = None;
        self.confirm = None;
        cx.notify();
    }

    /// The × of a panel or of the New sandbox dialog, `size` square, its
    /// cross as large as the design's.
    fn closer(size: Size, cx: &mut Context<Self>) -> ui::Button {
        icon_button(
            "sheet-close",
            Icon::Close,
            "Close",
            Kind::Ghost,
            true,
            cx.listener(|home, _, _, cx| home.dismiss(cx)),
        )
        .size(size)
        .icon_size(13.)
    }

    /// The open dialog or side panel over the window, if any.
    pub(crate) fn modal_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // What shows, and where: a panel at the right with its head, or a
        // dialog that far from the window's centre.
        let (layer, place) = match self.modal.as_ref()? {
            Modal::Add {
                preset,
                name,
                details,
            } => {
                // The close button stands in the title's row, at its end;
                // the design's × sits a pixel below the row's middle.
                let (top, inset, _) = DialogSize::Wide.padding();
                let dialog = self.add_dialog(preset.as_deref(), &name.clone(), *details, cx);
                (
                    dialog.relative().child(
                        div()
                            .absolute()
                            .top(px(top + 1.))
                            .right(px(inset))
                            .child(Self::closer(Size::Medium, cx)),
                    ),
                    Place::Centre(2., -31.),
                )
            }
            Modal::SandboxSettings { name, rename } => (
                self.sandbox_settings_sheet(&name.clone(), &rename.clone(), cx),
                Place::Side(SheetHead::Plain),
            ),
            Modal::Transfer { name } => (
                sheet(
                    SheetHead::Ruled,
                    format!("Files \u{b7} {name}"),
                    self.files_body(&name.clone(), cx),
                    None,
                ),
                Place::Side(SheetHead::Ruled),
            ),
            Modal::Close => (self.close_dialog(cx), Place::Centre(28., -69.5)),
            Modal::RemoveAll {
                keep,
                confirm,
                sizes,
            } => (
                self.remove_dialog(*keep, &confirm.clone(), sizes.as_ref(), cx),
                Place::Centre(30., -35.5),
            ),
            Modal::Switch { from, to } => (
                Self::switch_dialog(&from.clone(), &to.clone(), cx),
                Place::Centre(28., -79.5),
            ),
            Modal::SingleGpu { name, explained } => (
                self.single_gpu_dialog(&name.clone(), *explained, cx),
                Place::Centre(30., -22.5),
            ),
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
            .occlude();
        Some(match place {
            // A panel at the side leaves more of the page in sight than a
            // dialog does, the files panel most of all.
            Place::Side(head) => {
                let (top, inset, size) = head.close_place();
                backdrop
                    .bg(gpui::rgb(0x05_06_0a).opacity(match head {
                        SheetHead::Plain => 0.6,
                        SheetHead::Ruled => 0.5,
                    }))
                    .justify_end()
                    .child(
                        div()
                            .id("modal-outside")
                            .flex_1()
                            .h_full()
                            .on_click(cx.listener(|home, _, _, cx| home.dismiss(cx))),
                    )
                    .child(
                        layer.child(
                            div()
                                .absolute()
                                .top(px(top))
                                .right(px(inset))
                                .child(Self::closer(size, cx)),
                        ),
                    )
                    .into_any_element()
            }
            Place::Centre(right, down) => centred(backdrop, layer, right, down),
        })
    }
}

/// Where a modal stands.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    /// A panel at the right edge, headed so.
    Side(SheetHead),
    /// A dialog whose centre is this far right of and below the window's.
    Centre(f32, f32),
}

/// `dialog` on `backdrop`, its centre `right` and `down` from the
/// window's, as the design places it in a 1280 x 800 window. Spacers above
/// and below centre it; in a window too short for it, the one below gives
/// up its room first and the dialog keeps to the top.
fn centred(backdrop: gpui::Stateful<Div>, dialog: Div, right: f32, down: f32) -> AnyElement {
    let spacer = |basis: f32| {
        div()
            .flex_grow(1.)
            .flex_shrink(1.)
            .flex_basis(px(basis.max(0.)))
    };
    backdrop
        .bg(gpui::rgb(0x05_06_0a).opacity(0.7))
        .flex_col()
        .items_center()
        .when(right > 0., |b| b.pl(px(right * 2.)))
        .when(right < 0., |b| b.pr(px(-right * 2.)))
        .child(spacer(down * 2.))
        .child(dialog.flex_shrink(0.01).min_h_0())
        .child(spacer(-down * 2.))
        .into_any_element()
}

/// How large a dialog is: its width, and the padding, gaps and title that
/// go with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogSize {
    /// 520 wide inside its padding: a question with a few answers.
    Small,
    /// 580 wide inside its padding: a question with a form under it.
    Medium,
    /// 600 wide inside its padding: an explanation to read before going on.
    Large,
    /// 700 wide with its padding: making something, picked from a list.
    Wide,
}

impl DialogSize {
    /// The width inside the padding.
    const fn width(self) -> f32 {
        match self {
            Self::Small => 520.,
            Self::Medium => 580.,
            Self::Large => 600.,
            Self::Wide => 700. - 26. * 2.,
        }
    }

    /// The padding at the top, at the sides and at the bottom.
    pub(crate) const fn padding(self) -> (f32, f32, f32) {
        match self {
            Self::Small => (26., 26., 26.),
            Self::Medium | Self::Large => (28., 28., 28.),
            Self::Wide => (24., 26., 22.),
        }
    }

    /// The space between the title, the body and the buttons.
    const fn gap(self) -> f32 {
        match self {
            Self::Small => 14.,
            Self::Medium => 16.,
            Self::Large => 18.,
            Self::Wide => 20.,
        }
    }

    const fn title(self) -> f32 {
        match self {
            Self::Small | Self::Medium => 20.,
            Self::Large | Self::Wide => 22.,
        }
    }
}

/// A dialog box `size` wide: a title, its body, and the row of `buttons`
/// at its end.
pub(crate) fn dialog(
    size: DialogSize,
    title: impl Into<SharedString>,
    body: Div,
    buttons: Div,
) -> Div {
    dialog_with(size, None, title, body, buttons)
}

/// A [`dialog`] with an `icon` over its title.
fn dialog_with(
    size: DialogSize,
    icon: Option<Div>,
    title: impl Into<SharedString>,
    body: Div,
    buttons: Div,
) -> Div {
    let (top, inset, bottom) = size.padding();
    div()
        .flex()
        .flex_col()
        .gap(px(size.gap()))
        // The border is outside the width, as the boards draw it.
        .w(px(size.width() + inset * 2. + 2.))
        .max_h(px(680.))
        .pt(px(top))
        .px(px(inset))
        .pb(px(bottom))
        .rounded(px(18.))
        .bg(Theme::dialog())
        .border_1()
        .border_color(Theme::line())
        .shadow_lg()
        .children(icon)
        .child(
            div()
                .text_size(px(size.title()))
                .font_weight(FontWeight::BOLD)
                // A wide dialog has a close button in its title's row.
                .when(size == DialogSize::Wide, |t| {
                    t.flex().items_center().min_h(px(32.))
                })
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
        .child(
            buttons
                .flex()
                .items_center()
                .justify_end()
                .gap_2p5()
                // A question's answers stand a little apart from it.
                .when(size == DialogSize::Small, |b| b.mt(px(6.))),
        )
}

/// A paragraph of a dialog's running text: 14 px in the dialog's text
/// colour, its lines spaced for reading.
pub(crate) fn dialog_text(text: impl Into<SharedString>) -> Div {
    div()
        .line_height(relative(1.6))
        .text_color(Theme::body_text())
        .child(text.into())
}

/// The top of a panel's title row, and the room at its sides.
const SHEET_TOP: f32 = 22.;
const SHEET_SIDE: f32 = 24.;
/// The height of a ruled panel's head.
const RULED_HEAD: f32 = 60.;

/// How a panel at the right edge heads its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetHead {
    /// The title over a form, nothing between them; on the panel's ground.
    Plain,
    /// The title in a bar ruled off from sections that are ruled off from
    /// each other; on a ground a shade lighter.
    Ruled,
}

impl SheetHead {
    /// Where the close button stands from the panel's top and right edge,
    /// and its size. In a ruled head the design's × sits a pixel below the
    /// head's middle.
    const fn close_place(self) -> (f32, f32, Size) {
        match self {
            Self::Plain => (SHEET_TOP, SHEET_SIDE, Size::Small),
            Self::Ruled => ((RULED_HEAD - 32.) / 2. + 1., 20., Size::Medium),
        }
    }
}

/// A panel at the right edge: a title headed as `head` says, its body and
/// an optional row of buttons at the bottom. The close button goes over
/// the title's row, at its end.
pub(crate) fn sheet(
    head: SheetHead,
    title: impl Into<SharedString>,
    body: Div,
    buttons: Option<Div>,
) -> Div {
    let title = div()
        .text_lg()
        .font_weight(FontWeight::BOLD)
        .child(title.into());
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(460.))
        .h_full()
        .bg(match head {
            SheetHead::Plain => Theme::sidebar(),
            SheetHead::Ruled => Theme::surface(),
        })
        .border_l_1()
        .border_color(Theme::line())
        .child(match head {
            SheetHead::Plain => div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(30.))
                .mt(px(SHEET_TOP))
                .mb_2()
                .px(px(SHEET_SIDE))
                .child(title),
            // The rule is under the head's 60 px, as the design draws it.
            SheetHead::Ruled => div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(RULED_HEAD + 1.))
                .px(px(SHEET_SIDE))
                .border_b_1()
                .border_color(Theme::border())
                .child(title),
        })
        .child(body)
        .children(buttons.map(|b| {
            b.flex_none()
                .px_6()
                .py_4()
                .border_t_1()
                .border_color(Theme::border())
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_times_name_the_day() {
        let now = chrono::Local::now();
        let seconds = |when: chrono::DateTime<Local>| u64::try_from(when.timestamp()).unwrap();
        assert_eq!(
            journal_time(seconds(now)),
            format!("Today {}", now.format("%H:%M"))
        );
        let noon = chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        let yesterday = Local
            .from_local_datetime(&now.date_naive().pred_opt().unwrap().and_time(noon))
            .unwrap();
        assert_eq!(journal_time(seconds(yesterday)), "Yesterday 12:00");
        let last_year = now - chrono::Duration::days(400);
        assert_eq!(
            journal_time(seconds(last_year)),
            last_year.format("%-d %b %Y").to_string()
        );
    }

    #[test]
    fn a_copied_line_carries_its_date_and_time() {
        let entry = JournalEntry {
            time: 0,
            text: "Stopped".into(),
        };
        let line = copied_line(&entry);
        assert!(line.ends_with(" Stopped"), "{line}");
        assert_eq!(line.len(), "1970-01-01 00:00:00 Stopped".len());
    }

    #[test]
    fn a_set_names_five_apps_and_counts_the_rest() {
        assert_eq!(apps_line(&["Steam", "Lutris"]), "Steam, Lutris");
        assert_eq!(
            apps_line(&["A", "B", "C", "D", "E", "F", "G"]),
            "A, B, C, D, E and 2 more"
        );
        assert_eq!(apps_line(&["A", "B", "C", "D", "E"]), "A, B, C, D, E");
    }

    #[test]
    fn reasons_start_a_sentence() {
        assert_eq!(
            sentence("the sandbox did not answer"),
            "The sandbox did not answer"
        );
        assert_eq!(sentence(""), "");
    }
}
