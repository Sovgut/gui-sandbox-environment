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
            self.page == crate::Page::Sandbox && self.screen.is_none() && !self.processes_loading
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
            .max_h(px(220.))
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

    /// The programs of the selected sandbox, when it runs.
    pub(crate) fn processes_card(&self, cx: &mut Context<Self>) -> Option<Div> {
        let name = self.selected_up()?;
        let search = Search::new(self.processes_filter.read(cx).text());
        let (list, summary) = self.process_list(&name, &search, cx);
        let columns = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .pb_1()
            .border_b_1()
            .border_color(Theme::border())
            .text_xs()
            .text_color(Theme::subtle())
            .child(cell(PID_WIDTH).child("PID"))
            .child(div().flex_1().child("Program"))
            .child(cell(MEMORY_WIDTH).child("Memory"))
            .child(cell(CPU_WIDTH).child("CPU"))
            .child(cell(ACTIONS_WIDTH));
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
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
                                .child("Programs"),
                        )
                        .child(div().flex_1().child(self.processes_filter.clone()))
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(Theme::subtle())
                                .child(summary),
                        ),
                )
                .when(search.broken, |card| {
                    card.child(
                        div()
                            .text_xs()
                            .text_color(Theme::warning())
                            .child("Not a regular expression; matched as plain text"),
                    )
                })
                .child(columns)
                .child(
                    div()
                        .relative()
                        .child(list)
                        .children(scrollbar(&self.processes_scroll)),
                ),
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
