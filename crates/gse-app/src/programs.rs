//! The programs `guest` runs in the selected sandbox, as its agent reports
//! them: a table grouped by the menu entry that started each program, where
//! a process can be asked to quit or killed, and a program killed with
//! everything it started, without a restart. Everything here comes from
//! inside the sandbox and is shown as text only.

use std::time::Duration;

use gpui::{Context, Div, FontWeight, SharedString, div, prelude::*, px};
use gse_protocol::{GuestApp, GuestProcess, Request, Stage};

use crate::Home;
use crate::client;
use crate::icons::Icon;
use crate::ui::{Kind, Theme, scrollbar, small_icon_button};

/// How often the list of programs follows the sandbox.
pub const PROCESS_POLL: Duration = Duration::from_secs(3);

const PID_WIDTH: f32 = 64.;
const MEMORY_WIDTH: f32 = 76.;
const CPU_WIDTH: f32 = 52.;
/// Room for a row's two buttons.
const ACTIONS_WIDTH: f32 = 52.;

pub fn size_text(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64;
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.0} KiB", value / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MiB", value / 1_048_576.0),
        _ => format!("{:.1} GiB", value / 1_073_741_824.0),
    }
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

    fn end(&mut self, name: &str, request: Request, cx: &mut Context<Self>) {
        let key = format!("{name}/end");
        self.request(key, "Ending", request, cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            this.update(cx, Self::poll_processes).ok();
        })
        .detach();
    }

    fn process_row(name: &str, process: &GuestProcess, cx: &mut Context<Self>) -> Div {
        let (quit, kill) = (name.to_owned(), name.to_owned());
        let pid = process.pid;
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(26.))
            .text_sm()
            .hover(|row| row.bg(Theme::raised().opacity(0.5)))
            .child(
                cell(PID_WIDTH)
                    .pl_4()
                    .text_xs()
                    .text_color(Theme::subtle())
                    .child(pid.to_string()),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_baseline()
                    .gap_2()
                    .child(div().flex_none().child(process.name.clone()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(Theme::subtle())
                            .child(process.command.clone()),
                    ),
            )
            .child(
                cell(MEMORY_WIDTH)
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(size_text(process.memory_kib * 1024)),
            )
            .child(
                cell(CPU_WIDTH)
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(format!("{} s", process.cpu_seconds)),
            )
            .child(
                cell(ACTIONS_WIDTH)
                    .flex()
                    .justify_end()
                    .gap_1()
                    .child(small_icon_button(
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
                    ))
                    .child(small_icon_button(
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
                    )),
            )
    }

    fn app_rows(name: &str, app: &GuestApp, cx: &mut Context<Self>) -> Div {
        let memory: u64 = app.processes.iter().map(|p| p.memory_kib).sum();
        let mut rows =
            div().flex().flex_col().child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(28.))
                    .bg(Theme::raised().opacity(0.6))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(app.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(Theme::subtle())
                            .child(match app.processes.len() {
                                1 => "1 process".to_owned(),
                                n => format!("{n} processes"),
                            }),
                    )
                    .child(
                        cell(MEMORY_WIDTH)
                            .text_xs()
                            .text_color(Theme::muted())
                            .child(size_text(memory * 1024)),
                    )
                    .child(cell(CPU_WIDTH))
                    .child(cell(ACTIONS_WIDTH).flex().justify_end().children(
                        app.unit.clone().map(|unit| {
                            let owner = name.to_owned();
                            small_icon_button(
                                SharedString::from(format!("end-{unit}")),
                                Icon::Stop,
                                "Kill it with everything it started",
                                Kind::Danger,
                                cx.listener(move |home, _, _, cx| {
                                    let request = Request::EndApp {
                                        name: owner.clone(),
                                        unit: unit.clone(),
                                    };
                                    home.end(&owner, request, cx);
                                }),
                            )
                        }),
                    )),
            );
        for process in &app.processes {
            rows = rows.child(Self::process_row(name, process, cx));
        }
        rows
    }

    /// The programs of `name` the search keeps, as table rows, and how many
    /// there are in all.
    fn process_list(
        &self,
        name: &str,
        search: &Search,
        cx: &mut Context<Self>,
    ) -> (gpui::Stateful<Div>, String) {
        let mut list = div()
            .id("processes")
            .flex()
            .flex_col()
            .gap_1()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.processes_scroll);
        let mut summary = String::new();
        match self.processes.as_ref().filter(|(n, _)| n == name) {
            None => {
                list = list.child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(Theme::muted())
                        .child("Loading..."),
                );
            }
            Some((_, Err(e))) => {
                list = list.child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(Theme::warning())
                        .child(e.clone()),
                );
            }
            Some((_, Ok(apps))) if apps.is_empty() => {
                list = list.child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(Theme::muted())
                        .child("Nothing runs as guest."),
                );
            }
            Some((_, Ok(apps))) => {
                let processes: usize = apps.iter().map(|a| a.processes.len()).sum();
                summary = format!("{} programs, {processes} processes", apps.len());
                let kept: Vec<GuestApp> = apps
                    .iter()
                    .map(|app| search.keep(app))
                    .filter(|app| !app.processes.is_empty())
                    .collect();
                if kept.is_empty() {
                    list = list.child(
                        div()
                            .p_2()
                            .text_sm()
                            .text_color(Theme::muted())
                            .child("Nothing matches the search."),
                    );
                }
                for app in &kept {
                    list = list.child(Self::app_rows(name, app, cx));
                }
            }
        }
        (list, summary)
    }

    /// The Programs tab of `name`: every program with its processes.
    pub(crate) fn programs_body(&self, name: &str, cx: &mut Context<Self>) -> Div {
        let search = Search::new(self.processes_filter.read(cx).text());
        let (list, summary) = self.process_list(name, &search, cx);
        let columns = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .text_xs()
            .text_color(Theme::subtle())
            .child(cell(PID_WIDTH).child("PID"))
            .child(div().flex_1().child("Program"))
            .child(cell(MEMORY_WIDTH).child("Memory"))
            .child(cell(CPU_WIDTH).child("CPU"))
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
            .child(div().px_1p5().child(columns))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .px_1p5()
                    .child(list)
                    .children(scrollbar(&self.processes_scroll)),
            )
            .child(
                div()
                    .flex_none()
                    .px_3p5()
                    .py_2p5()
                    .border_t_1()
                    .border_color(Theme::border())
                    .text_xs()
                    .text_color(Theme::muted())
                    .child(
                        "\u{d7} asks a program to quit and lets it save; \u{25a0} ends it at once. \
                         On a program's row it ends the program with everything it started.",
                    ),
            )
    }

    /// A program of the sandbox at play, as a card beside its screen.
    fn aside_card(name: &str, app: &GuestApp, cx: &mut Context<Self>) -> Div {
        let memory: u64 = app.processes.iter().map(|p| p.memory_kib).sum();
        let detail = match app.processes.len() {
            1 => format!("1 process \u{b7} {}", size_text(memory * 1024)),
            n => format!("{n} processes \u{b7} {}", size_text(memory * 1024)),
        };
        let end = app.unit.clone().map(|unit| {
            let owner = name.to_owned();
            small_icon_button(
                SharedString::from(format!("aside-end-{unit}")),
                Icon::Close,
                "Close it with everything it started",
                Kind::Ghost,
                cx.listener(move |home, _, _, cx| {
                    let request = Request::EndApp {
                        name: owner.clone(),
                        unit: unit.clone(),
                    };
                    home.end(&owner, request, cx);
                }),
            )
        });
        div()
            .flex()
            .items_center()
            .gap_2p5()
            .p_2p5()
            .rounded(px(10.))
            .bg(Theme::surface())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
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
            .children(end)
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
                for app in apps {
                    cards = cards.child(Self::aside_card(name, app, cx));
                }
            }
            Some((_, Err(e))) => {
                cards = cards.child(
                    div()
                        .text_sm()
                        .text_color(Theme::warning())
                        .child(e.clone()),
                );
            }
            None => {
                cards = cards.child(
                    div()
                        .text_sm()
                        .text_color(Theme::muted())
                        .child("Loading..."),
                );
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(300.))
            .h_full()
            .bg(Theme::aside())
            .rounded_br(self.corners.bottom_right)
            .border_l_1()
            .border_color(Theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .pt_3p5()
                    .pb_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .child("Programs"),
                    )
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
                    .px_4()
                    .py_3p5()
                    .border_t_1()
                    .border_color(Theme::border())
                    .text_xs()
                    .text_color(Theme::muted())
                    .child("Every process, and ending one at once, is in the Programs tab."),
            )
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
}
