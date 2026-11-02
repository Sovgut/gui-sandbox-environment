//! Preparing the computer on a first start: the bundled service is
//! installed through one polkit prompt, then the sandbox system and the
//! game launchers are built through it. The window shows only this until
//! everything is in place.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use futures::StreamExt as _;
use gpui::{Context, Div, FontWeight, div, prelude::*, px};
use gse_protocol::BASE_IMAGE;

use crate::client;
use crate::icons::Icon;
use crate::ui::{self, Kind, Theme, button};
use crate::{Home, onboarding::Tour};

/// The set of apps a prepared computer starts with.
const FIRST_SET: &str = "launchers";

/// Where the service keeps the base image every set is built on.
const BASE_PATH: &str = "/var/lib/gse/base.qcow2";

/// The steps of preparing, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Check,
    Service,
    System,
    Launchers,
}

const STEPS: [(Step, &str, &str); 4] = [
    (
        Step::Check,
        "Check this computer",
        "Virtualization, the graphics card hand-over, and the tools sandboxes run on, \
         installed if missing.",
    ),
    (
        Step::Service,
        "Install the background service",
        "Starts sandboxes and keeps them apart from your files.",
    ),
    (
        Step::System,
        "Build the sandbox system",
        "Downloads Fedora and builds the desktop every sandbox runs. About 15 minutes.",
    ),
    (
        Step::Launchers,
        "Build the game launchers",
        "Steam, Lutris, Heroic, Bottles and more, ready inside.",
    ),
];

/// How far preparing got.
pub struct Setup {
    /// The step in progress, or the one to start from.
    pub step: Step,
    /// Whether the steps are running now.
    pub running: bool,
    /// Findings that do not stop preparing.
    pub warnings: Vec<String>,
    /// Why the last attempt stopped.
    pub failed: Option<String>,
}

/// What the service install reports, line by line.
enum Report {
    Begun(String),
    Warning(String),
    Failed(String),
    Ended(Result<()>),
}

/// Whether the computer still needs preparing, and from which step.
pub fn needed() -> Option<Step> {
    if !Path::new(gse_protocol::SOCKET).exists() {
        return Some(Step::Check);
    }
    if !Path::new(BASE_PATH).exists() {
        return Some(Step::System);
    }
    None
}

/// The folder the service program and the guest program come from: the
/// AppImage's, or the one this program runs from.
fn payload() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("APPDIR") {
        return Ok(PathBuf::from(dir).join("usr/libexec/gse"));
    }
    let exe = std::env::current_exe()?;
    Ok(exe.parent().context("the program's folder")?.to_path_buf())
}

/// Copies the programs out of the AppImage, whose mount root cannot read,
/// into a folder of their own.
fn stage() -> Result<PathBuf> {
    let from = payload()?;
    let to = std::env::temp_dir().join(format!("gse-setup-{}", std::process::id()));
    std::fs::remove_dir_all(&to).ok();
    std::fs::create_dir_all(&to)?;
    for program in ["gse-service", "gse-guest"] {
        std::fs::copy(from.join(program), to.join(program))
            .with_context(|| format!("copying {program} from {}", from.display()))?;
    }
    Ok(to)
}

/// Runs the install through polkit and hands each line it reports to
/// `report`.
fn install(report: &futures::channel::mpsc::UnboundedSender<Report>) -> Result<()> {
    let staged = stage()?;
    let mut child = Command::new("pkexec")
        .arg(staged.join("gse-service"))
        .arg("install")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting pkexec")?;
    let stdout = child.stdout.take().context("the install's output")?;
    let mut failure = None;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        let (kind, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match kind {
            "step" => {
                report.unbounded_send(Report::Begun(rest.to_owned())).ok();
            }
            "warning" => {
                let text = rest.split_once(' ').map_or(rest, |(_, text)| text);
                report.unbounded_send(Report::Warning(text.to_owned())).ok();
            }
            "failed" => {
                let reason = rest.split_once(' ').map_or(rest, |(_, reason)| reason);
                failure = Some(reason.to_owned());
                report
                    .unbounded_send(Report::Failed(reason.to_owned()))
                    .ok();
            }
            _ => {}
        }
    }
    let status = child.wait()?;
    std::fs::remove_dir_all(&staged).ok();
    match (status.code(), failure) {
        (Some(0), _) => Ok(()),
        (_, Some(reason)) => bail!(reason),
        (Some(126 | 127), None) => bail!("the password prompt was dismissed"),
        _ => bail!("the install failed ({status})"),
    }
}

/// Whether the app carries a service of its own to install: an AppImage
/// does; a development build runs the service the machine set up.
pub fn carries_service() -> bool {
    std::env::var_os("APPDIR").is_some()
}

impl Home {
    /// Installs the service this app carries over an older one; the install
    /// restarts it.
    pub(crate) fn update_service(&mut self, cx: &mut Context<Self>) {
        self.restarting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async {
                    let (report, _) = futures::channel::mpsc::unbounded();
                    install(&report)
                })
                .await;
            this.update(cx, |home, cx| {
                if let Err(e) = outcome {
                    home.restarting = false;
                    home.say(format!("The service was not updated: {e:#}"), true);
                }
                Self::reload(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs the steps from the current one to the end.
    pub(crate) fn prepare(&mut self, cx: &mut Context<Self>) {
        let Some(setup) = self.setup.as_mut() else {
            return;
        };
        setup.running = true;
        setup.failed = None;
        let from = setup.step;
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let worker = tx.clone();
        std::thread::spawn(move || {
            let outcome = (|| -> Result<()> {
                if from <= Step::Service {
                    install(&worker)?;
                }
                if from <= Step::System {
                    worker.unbounded_send(Report::Begun("system".into())).ok();
                    client::build_base()?;
                }
                worker
                    .unbounded_send(Report::Begun("launchers".into()))
                    .ok();
                client::build_preset(FIRST_SET)
            })();
            worker.unbounded_send(Report::Ended(outcome)).ok();
        });
        drop(tx);
        cx.spawn(async move |this, cx| {
            while let Some(report) = rx.next().await {
                let alive = this.update(cx, |home, cx| {
                    home.setup_report(report, cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn setup_report(&mut self, report: Report, cx: &mut Context<Self>) {
        let Some(setup) = self.setup.as_mut() else {
            return;
        };
        match report {
            Report::Begun(step) => {
                setup.step = match step.as_str() {
                    "packages" | "check" => Step::Check,
                    "system" => Step::System,
                    "launchers" => Step::Launchers,
                    _ => Step::Service,
                };
            }
            Report::Warning(text) => setup.warnings.push(text),
            Report::Failed(reason) => setup.failed = Some(reason),
            Report::Ended(Ok(())) => {
                self.setup = None;
                if !self.app_config.tour_done {
                    self.tour = Some(Tour::Welcome);
                }
                Self::reload(cx);
            }
            Report::Ended(Err(e)) => {
                setup.running = false;
                setup.failed = Some(format!("{e:#}"));
                // The service may be in place now even if a later step failed.
                if let Some(next) = needed() {
                    setup.step = setup.step.max(next);
                }
                Self::reload(cx);
            }
        }
    }

    /// The last thing the build of the base image or the first set said.
    fn build_detail(&self, step: Step) -> Option<String> {
        let build = self.presets.build.as_ref()?;
        let name = match step {
            Step::System => BASE_IMAGE,
            Step::Launchers => FIRST_SET,
            _ => return None,
        };
        (build.preset == name)
            .then(|| build.steps.last().cloned())
            .flatten()
    }

    fn setup_row(&self, setup: &Setup, index: usize) -> Div {
        let (step, title, text) = STEPS[index];
        let done = step < setup.step;
        let current = step == setup.step;
        let marker = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded_full()
            .map(|m| {
                if done {
                    m.bg(Theme::good()).child(
                        gpui_kit::component::Icon::new(gpui_kit::component::IconName::Check)
                            .size(px(14.))
                            .text_color(Theme::on_accent()),
                    )
                } else if current && setup.failed.is_some() {
                    m.bg(Theme::danger())
                } else if current {
                    m.border_2()
                        .border_color(Theme::accent())
                        .child(div().size(px(8.)).rounded_full().bg(Theme::accent()))
                } else {
                    m.border_2().border_color(Theme::selected())
                }
            });
        let detail = if current && setup.running {
            self.build_detail(step)
                .map_or_else(|| text.to_owned(), |d| format!("{text} Now: {d}."))
        } else {
            text.to_owned()
        };
        div()
            .flex()
            .gap_3p5()
            .p_3p5()
            .rounded(px(12.))
            .when(current, |row| row.bg(Theme::surface()))
            .child(marker)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .font_weight(if done || current {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .text_color(if done || current {
                                Theme::text()
                            } else {
                                Theme::muted()
                            })
                            .child(title),
                    )
                    .child(div().text_sm().text_color(Theme::muted()).child(detail)),
            )
    }

    /// What the user can do on the setup page now.
    fn setup_action(setup: &Setup, cx: &mut Context<Self>) -> Div {
        if setup.running {
            div()
                .text_sm()
                .text_color(Theme::muted())
                .child("You can leave the window; the builds go on in the background.")
        } else {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    button(
                        "prepare",
                        if setup.failed.is_some() {
                            "Try again"
                        } else {
                            "Prepare this computer"
                        },
                        Kind::Primary,
                        true,
                        cx.listener(|home, _, _, cx| home.prepare(cx)),
                    )
                    .icon(ui::kit_icon(Icon::Download)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(Theme::muted())
                        .child("Your password is asked once, to install the service."),
                )
        }
    }

    /// The whole window while the computer is being prepared.
    pub(crate) fn setup_page(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Option<Div> {
        let setup = self.setup.as_ref()?;
        let mut steps = div().flex().flex_col().gap_1();
        for index in 0..STEPS.len() {
            steps = steps.child(self.setup_row(setup, index));
        }
        let action = Self::setup_action(setup, cx);
        let page = div()
            .flex()
            .flex_col()
            .gap_7()
            .w(px(600.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(30.))
                            .font_weight(FontWeight::BOLD)
                            .child("Getting this computer ready"),
                    )
                    .child(div().text_color(Theme::muted()).child(
                        "GSE runs untrusted games inside a sealed virtual computer. It needs a \
                         few system parts first; after that everything happens on its own.",
                    )),
            )
            .children(self.update_notice(cx))
            .child(steps)
            .children(
                setup
                    .failed
                    .clone()
                    .map(|reason| ui::notice(Theme::danger(), "Preparing stopped", reason)),
            )
            .children((!setup.warnings.is_empty()).then(|| {
                ui::notice(
                    Theme::warning(),
                    "Worth knowing",
                    setup.warnings.join(" \u{b7} "),
                )
            }))
            .child(action);
        Some(
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(Self::window_bar(
                    Some(
                        div()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .child(crate::shell::mark(26.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("GSE"))
                            .pl_4()
                            .into_any_element(),
                    ),
                    window,
                    cx,
                ))
                .child(
                    div()
                        .id("setup")
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .justify_center()
                        .pt_12()
                        .overflow_y_scroll()
                        .child(page),
                ),
        )
    }
}
