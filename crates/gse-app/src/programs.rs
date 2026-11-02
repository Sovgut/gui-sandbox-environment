//! The programs `guest` runs in the selected sandbox, as its agent reports
//! them: a table of each program with its processes, naming the program
//! that started it when no menu entry did, where a process can be asked to
//! quit or killed, and a program killed with everything it started, without
//! a restart. Everything here comes from inside the sandbox and is shown as
//! text only.

use std::time::{Duration, Instant};

use gpui::{
    App, Context, Div, FontWeight, HighlightStyle, MouseButton, SharedString, StyledText,
    WeakEntity, Window, div, prelude::*, px,
};
use gse_protocol::{GuestApp, GuestProcess, Request, Stage};

use crate::Home;
use crate::client;
use crate::icons::Icon;
use crate::sandbox::{marked, sentence, tile_color};
use crate::ui::{self, Kind, Size, Theme, Tone, button, notice, scrollbar, small_icon_button};

/// How often the list of programs follows the sandbox.
pub const PROCESS_POLL: Duration = Duration::from_secs(3);

/// How long × on a program's card is held before it kills the program.
const HOLD: Duration = Duration::from_millis(700);

/// The columns of the table: a process's PID, memory, CPU time and
/// buttons; its name and command line take the rest.
const PID_WIDTH: f32 = 70.;
const MEMORY_WIDTH: f32 = 90.;
const CPU_WIDTH: f32 = 70.;
const ACTIONS_WIDTH: f32 = 80.;
/// The room between the cells of a program's row, which line up with the
/// columns a little to their left.
const PROGRAM_GAP: f32 = 10.;

/// `bytes` in decimal units, as a file manager says it: one decimal under
/// 10 where it says something (`2.1 MB`, `3 MB`), none above (`812 MB`).
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    // The value as shown: a tenth under 10, whole from there.
    let shown = |value: f64| {
        let tenths = (value * 10.0).round() / 10.0;
        if tenths < 10.0 { tenths } else { value.round() }
    };
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    // What rounds to 1000 reads as 1 of the next unit.
    while unit + 1 < UNITS.len() && shown(value) >= 1000.0 {
        value /= 1000.0;
        unit += 1;
    }
    let value = shown(value);
    if unit > 0 && value < 10.0 && value.fract() != 0.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// CPU time in the largest unit that says it: seconds under a minute,
/// whole minutes under an hour (`9 min`), then hours and minutes.
fn cpu_text(seconds: u64) -> String {
    match seconds {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s => match (s / 3600, s % 3600 / 60) {
            (hours, 0) => format!("{hours} h"),
            (hours, minutes) => format!("{hours} h {minutes} min"),
        },
    }
}

/// The memory `apps` take, in bytes.
fn memory(apps: &[GuestApp]) -> u64 {
    apps.iter()
        .flat_map(|a| &a.processes)
        .map(|p| p.memory_kib * 1024)
        .sum()
}

/// Where `app` comes from, after its title: the program that started it,
/// as the sandbox's agent names it, or no menu at all for the processes
/// outside every program.
fn origin(app: &GuestApp) -> Option<String> {
    if app.unit.is_none() {
        return Some("not started from a menu".to_owned());
    }
    app.started_by
        .as_ref()
        .map(|parent| format!("started by {parent}"))
}

/// `apps` with the processes outside every program last, as one row after
/// the programs started from a menu.
fn menu_order(apps: &[GuestApp]) -> Vec<GuestApp> {
    let mut apps = apps.to_vec();
    apps.sort_by_key(|a| a.unit.is_none());
    apps
}

/// What the search keeps: a regular expression, ignoring case, over a
/// program's title and its processes' names, command lines and PIDs. An
/// expression that does not compile is matched as plain text.
struct Search {
    pattern: Option<regex::Regex>,
    broken: bool,
}

impl Search {
    fn new(text: &str) -> Self {
        let text = text.trim();
        if text.is_empty() {
            return Self {
                pattern: None,
                broken: false,
            };
        }
        let build = |p: &str| regex::RegexBuilder::new(p).case_insensitive(true).build();
        match build(text) {
            Ok(pattern) => Self {
                pattern: Some(pattern),
                broken: false,
            },
            Err(_) => Self {
                pattern: build(&regex::escape(text)).ok(),
                broken: true,
            },
        }
    }

    fn finds(&self, text: &str) -> bool {
        self.pattern.as_ref().is_none_or(|p| p.is_match(text))
    }

    /// `app` with the processes the search keeps; all of them when its
    /// title matches, none when nothing does.
    fn keep(&self, app: &GuestApp) -> GuestApp {
        let mut kept = app.clone();
        if !self.finds(&app.title) {
            kept.processes.retain(|p| {
                self.finds(&p.name) || self.finds(&p.command) || self.finds(&p.pid.to_string())
            });
        }
        kept
    }
}

fn cell(width: f32) -> Div {
    div().flex_none().w(px(width))
}

/// How many programs and processes there are and the memory they take,
/// or how many of them a search keeps and what those take:
/// `1 of 3 programs · 2 of 4 processes · 513 MB`.
fn counts(all: &[GuestApp], kept: &[GuestApp], searched: bool) -> String {
    let processes = |apps: &[GuestApp]| apps.iter().map(|a| a.processes.len()).sum::<usize>();
    let count = |kept: usize, all: usize, one: &str, many: &str| {
        let noun = if all == 1 { one } else { many };
        if searched {
            format!("{kept} of {all} {noun}")
        } else {
            format!("{all} {noun}")
        }
    };
    format!(
        "{} \u{b7} {} \u{b7} {}",
        count(kept.len(), all.len(), "program", "programs"),
        count(processes(kept), processes(all), "process", "processes"),
        size_text(memory(if searched { kept } else { all }))
    )
}

/// `title` in bold with `origin` after it in the quiet colour, as one line.
fn titled(title: &str, origin: Option<String>) -> StyledText {
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        ..HighlightStyle::default()
    };
    match origin {
        None => StyledText::new(title.to_owned()).with_highlights(vec![(0..title.len(), bold)]),
        Some(origin) => {
            let quiet = HighlightStyle {
                color: Some(Theme::muted()),
                ..HighlightStyle::default()
            };
            let start = title.len() + 1;
            let end = start + origin.len();
            StyledText::new(format!("{title} {origin}"))
                .with_highlights(vec![(0..title.len(), bold), (start..end, quiet)])
        }
    }
}

impl Home {
    /// The name of the selected sandbox when it runs and is up.
    pub(crate) fn selected_up(&self) -> Option<String> {
        let name = self.selected.clone()?;
        self.environments
            .iter()
            .find(|e| e.name == name)?
            .running
            .as_ref()
            .filter(|r| r.stage == Stage::Ready)
            .map(|_| name)
    }

    /// Reloads the programs of the selected sandbox, when it is up and its
    /// pane is in sight.
    pub(crate) fn poll_processes(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.selected_up().filter(|_| {
            (self.page == crate::Page::Sandbox || self.screen.is_some()) && !self.processes_loading
        }) else {
            if self.selected_up().is_none() {
                self.processes = None;
            }
            return;
        };
        self.processes_loading = true;
        cx.spawn(async move |this, cx| {
            let request = name.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move { client::processes(&request) })
                .await;
            this.update(cx, |home, cx| {
                home.processes_loading = false;
                home.processes = Some((name, loaded.map_err(|e| format!("{e:#}").into())));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs `work`, which ends programs of `name`, then reads the programs
    /// again once they had a moment to go.
    fn ending(
        &mut self,
        name: &str,
        work: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.run(format!("{name}/end"), "Ending", work, cx);
        // A preview sends nothing, and its made-up programs stay.
        if crate::preview::active() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            this.update(cx, Self::poll_processes).ok();
        })
        .detach();
    }

    fn end(&mut self, name: &str, request: Request, cx: &mut Context<Self>) {
        tracing::info!("request {request:?}");
        self.ending(name, move || client::command(&request), cx);
    }

    /// Asks each of `pids`, the processes of one program of `name`, to
    /// quit, as closing the program's windows would. A process that went
    /// with another one before its turn is no failure.
    fn quit_program(&mut self, name: &str, pids: Vec<u32>, cx: &mut Context<Self>) {
        tracing::info!("asking processes {pids:?} of {name} to quit");
        let owner = name.to_owned();
        self.ending(
            name,
            move || {
                let mut failure = None;
                let mut asked = false;
                for pid in pids {
                    let request = Request::EndProcess {
                        name: owner.clone(),
                        pid,
                        force: false,
                    };
                    match client::command(&request) {
                        Ok(()) => asked = true,
                        Err(e) => {
                            failure.get_or_insert(e);
                        }
                    }
                }
                match failure {
                    Some(e) if !asked => Err(e),
                    _ => Ok(()),
                }
            },
            cx,
        );
    }

    fn process_row(name: &str, process: &GuestProcess, cx: &mut Context<Self>) -> Div {
        let (quit, kill) = (name.to_owned(), name.to_owned());
        let pid = process.pid;
        let command = HighlightStyle {
            color: Some(Theme::subtle()),
            ..HighlightStyle::default()
        };
        let start = process.name.len() + 1;
        let line = format!("{} {}", process.name, process.command);
        let end = line.len();
        div()
            .flex()
            .items_center()
            .px_3p5()
            .py(px(5.))
            .hover(|row| row.bg(Theme::hover()))
            .child(
                cell(PID_WIDTH)
                    .text_color(Theme::subtle())
                    .child(pid.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr_3()
                    .truncate()
                    .child(StyledText::new(line).with_highlights(vec![(start..end, command)])),
            )
            .child(
                cell(MEMORY_WIDTH)
                    .text_color(Theme::secondary_text())
                    .child(size_text(process.memory_kib * 1024)),
            )
            .child(
                cell(CPU_WIDTH)
                    .text_color(Theme::secondary_text())
                    .child(cpu_text(process.cpu_seconds)),
            )
            .child(
                cell(ACTIONS_WIDTH)
                    .flex()
                    .justify_end()
                    .gap_1()
                    // The marks are as small as the footer's × and ■.
                    .child(
                        small_icon_button(
                            SharedString::from(format!("quit-{pid}")),
                            Icon::Close,
                            "Ask it to quit",
                            Kind::Ghost,
                            cx.listener(move |home, _, _, cx| {
                                let request = Request::EndProcess {
                                    name: quit.clone(),
                                    pid,
                                    force: false,
                                };
                                home.end(&quit, request, cx);
                            }),
                        )
                        .icon_size(12.),
                    )
                    .child(
                        small_icon_button(
                            SharedString::from(format!("kill-{pid}")),
                            Icon::Stop,
                            "Kill it now",
                            Kind::Danger,
                            cx.listener(move |home, _, _, cx| {
                                let request = Request::EndProcess {
                                    name: kill.clone(),
                                    pid,
                                    force: true,
                                };
                                home.end(&kill, request, cx);
                            }),
                        )
                        .icon_size(12.),
                    ),
            )
    }

    /// A program's row, `first` of the table or a little apart from the
    /// one before, and the rows of its processes.
    fn app_rows(name: &str, app: &GuestApp, first: bool, cx: &mut Context<Self>) -> Div {
        let close = app.unit.clone().map(|unit| {
            let owner = name.to_owned();
            button(
                SharedString::from(format!("end-{unit}")),
                "Close all",
                Kind::Danger,
                true,
                cx.listener(move |home, _, _, cx| {
                    let request = Request::EndApp {
                        name: owner.clone(),
                        unit: unit.clone(),
                    };
                    home.end(&owner, request, cx);
                }),
            )
            .size(Size::Row)
            .w(px(ACTIONS_WIDTH))
            .h(px(26.))
            .tooltip("Ends it with everything it started")
        });
        let mut rows = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap(px(PROGRAM_GAP))
                .px_3p5()
                .py_2()
                .when(!first, |row| row.mt(px(6.)))
                .bg(Theme::group())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(titled(&app.title, origin(app))),
                )
                .child(
                    cell(MEMORY_WIDTH)
                        .text_color(Theme::secondary_text())
                        .child(size_text(memory(std::slice::from_ref(app)))),
                )
                .map(|row| match close {
                    Some(close) => row.child(cell(CPU_WIDTH)).child(close),
                    None => row.child(cell(CPU_WIDTH + PROGRAM_GAP + ACTIONS_WIDTH)),
                }),
        );
        for process in &app.processes {
            rows = rows.child(Self::process_row(name, process, cx));
        }
        rows
    }

    /// Reads the programs again after reading them failed.
    fn read_programs_again(&mut self, cx: &mut Context<Self>) {
        if self.preview_skips("reading the programs again", cx) {
            return;
        }
        self.processes = None;
        self.poll_processes(cx);
        cx.notify();
    }

    /// The programs of `name` the search keeps, as table rows; how many of
    /// them and of their processes it keeps, and whether there are rows at
    /// all, not a message in their place.
    fn process_list(
        &self,
        name: &str,
        search: &Search,
        cx: &mut Context<Self>,
    ) -> (gpui::Stateful<Div>, String, bool) {
        let mut list = div()
            .id("processes")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.processes_scroll)
            .text_size(px(13.));
        let quiet =
            |text: &'static str| div().px_3p5().py_2().text_color(Theme::muted()).child(text);
        let mut summary = String::new();
        let mut rows = false;
        match self.processes.as_ref().filter(|(n, _)| n == name) {
            None => list = list.child(quiet("Loading...")),
            Some((_, Err(e))) => {
                list = list.child(div().px_3p5().py_2().child(
                    notice(Tone::Warning, "Programs are unavailable", sentence(e)).child(button(
                        "programs-retry",
                        "Try again",
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, _, cx| home.read_programs_again(cx)),
                    )),
                ));
            }
            Some((_, Ok(apps))) if apps.is_empty() => {
                list = list.child(quiet("Nothing runs as guest."));
            }
            Some((_, Ok(apps))) => {
                let kept: Vec<GuestApp> = menu_order(apps)
                    .iter()
                    .map(|app| search.keep(app))
                    .filter(|app| !app.processes.is_empty())
                    .collect();
                summary = counts(apps, &kept, search.pattern.is_some());
                if kept.is_empty() {
                    list = list.child(quiet("Nothing matches the search."));
                }
                for (i, app) in kept.iter().enumerate() {
                    list = list.child(Self::app_rows(name, app, i == 0, cx));
                }
                rows = !kept.is_empty();
            }
        }
        (list, summary, rows)
    }

    /// The Programs tab of `name`: every program with its processes.
    pub(crate) fn programs_body(&self, name: &str, cx: &mut Context<Self>) -> Div {
        let search = Search::new(self.processes_filter.read(cx).text());
        let (list, summary, rows) = self.process_list(name, &search, cx);
        let heading = |width: Option<f32>, text: &'static str| {
            width
                .map_or_else(|| div().flex_1(), cell)
                .py(px(6.))
                .child(text)
        };
        let columns = div()
            .flex()
            .px_3p5()
            .text_xs()
            .text_color(Theme::subtle())
            .child(heading(Some(PID_WIDTH), "PID"))
            .child(heading(None, "Program"))
            .child(heading(Some(MEMORY_WIDTH), "Memory"))
            .child(heading(Some(CPU_WIDTH), "CPU"))
            .child(cell(ACTIONS_WIDTH));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3p5()
                    .py_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.processes_filter.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(Theme::muted())
                            .child(summary),
                    ),
            )
            .when(search.broken, |body| {
                body.child(
                    div()
                        .px_3p5()
                        .pb_2()
                        .text_xs()
                        .text_color(Theme::warning())
                        .child("Not a regular expression; matched as plain text"),
                )
            })
            // The columns name rows; a message in their place needs none.
            .when(rows, |body| body.child(columns))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .children(scrollbar(&self.processes_scroll)),
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
                        "\u{d7} asks a program to quit and lets it save; \u{25a0} ends it at once. \
                         Close all ends a program with everything it started.",
                    ),
            )
    }

    /// A program of the sandbox at play, as a card beside its screen.
    fn aside_card(name: &str, app: &GuestApp, cx: &mut Context<Self>) -> Div {
        let size = size_text(memory(std::slice::from_ref(app)));
        let detail = match (origin(app), app.processes.len()) {
            (Some(origin), _) => format!("{origin} \u{b7} {size}"),
            (None, 1) => format!("1 process \u{b7} {size}"),
            (None, n) => format!("{n} processes \u{b7} {size}"),
        };
        let close = app.unit.clone().map(|unit| CloseProgram {
            id: SharedString::from(format!("aside-end-{unit}")),
            title: app.title.clone(),
            home: cx.weak_entity(),
            sandbox: name.to_owned(),
            unit,
            pids: app.processes.iter().map(|p| p.pid).collect(),
        });
        div()
            .flex()
            .items_center()
            .gap_2p5()
            .p_2p5()
            .rounded(px(10.))
            .bg(Theme::group())
            .child(
                div()
                    .flex_none()
                    .size(px(30.))
                    .rounded(px(8.))
                    .bg(tile_color(&app.title)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(app.title.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(Theme::muted())
                            .child(detail),
                    ),
            )
            .children(close)
    }

    /// The programs of the sandbox at play, as cards beside its screen.
    pub(crate) fn programs_aside(&self, name: &str, cx: &mut Context<Self>) -> Div {
        let mut cards = div()
            .id("aside-programs")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_1()
            .px_2p5()
            .overflow_y_scroll();
        let mut count = 0;
        match self.processes.as_ref().filter(|(n, _)| n == name) {
            Some((_, Ok(apps))) => {
                count = apps.len();
                for app in &menu_order(apps) {
                    cards = cards.child(Self::aside_card(name, app, cx));
                }
            }
            Some((_, Err(e))) => {
                cards = cards.child(
                    div()
                        .px_1p5()
                        .text_size(px(13.))
                        .text_color(Theme::warning())
                        .child(sentence(e)),
                );
            }
            None => {
                cards = cards.child(
                    div()
                        .px_1p5()
                        .text_size(px(13.))
                        .text_color(Theme::muted())
                        .child("Loading..."),
                );
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            // 300 inside its border, as the design draws it.
            .w(px(301.))
            .h_full()
            .bg(Theme::aside())
            .rounded_br(self.corners.bottom_right)
            .border_l_1()
            .border_color(Theme::divider())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .pt_3p5()
                    .pb_2()
                    .child(div().font_weight(FontWeight::BOLD).child("Programs"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::muted())
                            .child(format!("{count} running")),
                    ),
            )
            .child(cards)
            .child(
                div()
                    .pl_4()
                    // The design's first line ends at the edge; a little
                    // more room keeps it whole where a font is set wider.
                    .pr(px(12.))
                    .py_3p5()
                    .border_t_1()
                    .border_color(Theme::divider())
                    .text_xs()
                    .line_height(gpui::relative(1.5))
                    .text_color(Theme::muted())
                    .child(marked(
                        "A program that hangs: hold ",
                        "\u{d7}",
                        " to force it closed. Details per process are one click away.",
                        Theme::soft_text(),
                        false,
                    )),
            )
    }
}

/// Where a press on a card's × stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Press {
    #[default]
    Up,
    /// Down since that moment; let go before [`HOLD`], it asks the
    /// program to quit.
    Down(Instant),
    /// Held for [`HOLD`]: the program was killed, and letting go does
    /// nothing more.
    Fired,
}

/// The × of a program's card beside the screen. A click asks each process
/// of the program to quit; held for [`HOLD`], it kills the program with
/// everything it started, and turns red meanwhile.
#[derive(IntoElement)]
struct CloseProgram {
    id: SharedString,
    title: String,
    home: WeakEntity<Home>,
    /// The sandbox the program runs in.
    sandbox: String,
    unit: String,
    pids: Vec<u32>,
}

impl RenderOnce for CloseProgram {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let press = window.use_keyed_state(self.id.clone(), cx, |_, _| Press::Up);
        let held = matches!(*press.read(cx), Press::Down(_));
        let Self {
            id,
            title,
            home,
            sandbox,
            unit,
            pids,
        } = self;
        let (pressed, released, left) = (press.downgrade(), press.downgrade(), press.downgrade());
        let (kill, quit) = (home.clone(), home);
        let (kill_in, quit_in) = (sandbox.clone(), sandbox);
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(26.))
            // The design's × is a letter, which sits under the middle.
            .pt(px(4.))
            .rounded(px(7.))
            .cursor_pointer()
            .text_color(if held {
                Theme::danger()
            } else {
                Theme::muted()
            })
            .hover(|b| b.bg(Theme::raised()))
            .child(ui::kit_icon(Icon::Close).size(px(11.)))
            .tooltip(ui::tooltip(
                format!("Ask {title} to quit; hold to force it closed").into(),
            ))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let at = Instant::now();
                pressed
                    .update(cx, |press, cx| {
                        *press = Press::Down(at);
                        cx.notify();
                    })
                    .ok();
                let (press, home, sandbox, unit) =
                    (pressed.clone(), kill.clone(), kill_in.clone(), unit.clone());
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(HOLD).await;
                    let fired = press
                        .update(cx, |press, cx| {
                            let fired = *press == Press::Down(at);
                            if fired {
                                *press = Press::Fired;
                                cx.notify();
                            }
                            fired
                        })
                        .unwrap_or(false);
                    if fired {
                        home.update(cx, |home, cx| {
                            home.end(
                                &sandbox,
                                Request::EndApp {
                                    name: sandbox.clone(),
                                    unit,
                                },
                                cx,
                            );
                        })
                        .ok();
                    }
                })
                .detach();
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                let was = released
                    .update(cx, |press, cx| {
                        cx.notify();
                        std::mem::take(press)
                    })
                    .unwrap_or_default();
                if matches!(was, Press::Down(_)) {
                    let pids = pids.clone();
                    quit.update(cx, |home, cx| home.quit_program(&quit_in, pids, cx))
                        .ok();
                }
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                left.update(cx, |press, cx| {
                    *press = Press::Up;
                    cx.notify();
                })
                .ok();
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> GuestApp {
        let process = |pid, name: &str, command: &str| GuestProcess {
            pid,
            name: name.into(),
            command: command.into(),
            memory_kib: 1,
            cpu_seconds: 0,
        };
        GuestApp {
            unit: Some("guest-app-1.service".into()),
            title: "Lutris".into(),
            started_by: None,
            processes: vec![
                process(10, "lutris", "/usr/bin/python3 /usr/bin/lutris"),
                process(11, "wine", "wine game.exe"),
            ],
        }
    }

    #[test]
    fn the_search_keeps_matching_processes_or_the_whole_program() {
        assert_eq!(Search::new("").keep(&app()).processes.len(), 2);
        assert_eq!(Search::new("LUTRIS").keep(&app()).processes.len(), 2);
        assert_eq!(Search::new("game\\.exe").keep(&app()).processes.len(), 1);
        assert_eq!(Search::new("^11$").keep(&app()).processes.len(), 1);
        assert_eq!(Search::new("steam").keep(&app()).processes, []);
        let broken = Search::new("(wine");
        assert!(broken.broken);
        assert_eq!(broken.keep(&app()).processes, []);
    }

    #[test]
    fn sizes_read_in_decimal_units() {
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(999), "999 B");
        assert_eq!(size_text(1000), "1 KB");
        assert_eq!(size_text(2_100_000), "2.1 MB");
        assert_eq!(size_text(3_000_000), "3 MB");
        assert_eq!(size_text(9_960_000), "10 MB");
        assert_eq!(size_text(137_400_000), "137 MB");
        assert_eq!(size_text(812_000_000), "812 MB");
        assert_eq!(size_text(999_700_000), "1 GB");
        assert_eq!(size_text(5_800_000_000), "5.8 GB");
        assert_eq!(size_text(61_000_000_000), "61 GB");
    }

    #[test]
    fn cpu_time_reads_in_its_largest_unit() {
        assert_eq!(cpu_text(0), "0 s");
        assert_eq!(cpu_text(41), "41 s");
        assert_eq!(cpu_text(60), "1 min");
        assert_eq!(cpu_text(540), "9 min");
        assert_eq!(cpu_text(3599), "59 min");
        assert_eq!(cpu_text(3600), "1 h");
        assert_eq!(cpu_text(5 * 3600 + 7 * 60 + 30), "5 h 7 min");
    }

    #[test]
    fn the_counts_follow_the_search() {
        let all = [app(), app()];
        assert_eq!(
            counts(&all, &all, false),
            "2 programs \u{b7} 4 processes \u{b7} 4.1 KB"
        );
        let kept = [Search::new("wine").keep(&app())];
        assert_eq!(
            counts(&all, &kept, true),
            "1 of 2 programs \u{b7} 1 of 4 processes \u{b7} 1 KB"
        );
    }

    #[test]
    fn stray_processes_come_after_the_programs() {
        let stray = GuestApp {
            unit: None,
            title: "Other programs".into(),
            ..app()
        };
        let order = menu_order(&[stray, app()]);
        assert_eq!(order[0].title, "Lutris");
        assert_eq!(
            origin(&order[1]).as_deref(),
            Some("not started from a menu")
        );
        assert_eq!(origin(&order[0]), None);
    }

    #[test]
    fn a_program_another_started_names_it() {
        let game = GuestApp {
            unit: Some("app-flatpak-com.example.Game-7.scope".into()),
            title: "com.example.Game".into(),
            started_by: Some("Steam".into()),
            ..app()
        };
        assert_eq!(origin(&game).as_deref(), Some("started by Steam"));
        // The processes outside every program have no menu, whatever the
        // agent says.
        let stray = GuestApp { unit: None, ..game };
        assert_eq!(origin(&stray).as_deref(), Some("not started from a menu"));
    }
}
