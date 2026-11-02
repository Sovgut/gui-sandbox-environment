//! Preparing the computer on a first start: the bundled service is
//! installed through one polkit prompt, then the sandbox system and the
//! game launchers are built through it. The window shows only this until
//! everything is in place.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use futures::StreamExt as _;
use gpui::{Context, Div, FontWeight, div, prelude::*, px, relative};
use gpui_kit::component::ActiveTheme as _;
use gse_protocol::{BASE_IMAGE, Build, Downloaded};

use crate::client;
use crate::icons::Icon;
use crate::ui::{self, Kind, Size, Theme, Tone, button};
use crate::{Home, onboarding::Tour};

/// The set of apps a prepared computer starts with.
const FIRST_SET: &str = "launchers";

/// Where the service keeps the base image every set is built on.
const BASE_PATH: &str = "/var/lib/gse/base.qcow2";

/// The most lines of a step's log the details show.
const DETAIL_LINES: usize = 8;

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
        "Download and build the sandbox system",
        "Downloads Fedora and builds the desktop every sandbox runs.",
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
    /// What the step in progress is doing now, as the install reports it.
    pub activity: Option<&'static str>,
    /// When the step in progress began: a download's time left goes by its
    /// pace since then.
    pub since: Option<chrono::DateTime<chrono::Local>>,
    /// How far the download of Fedora got, in bytes done and in all, while
    /// the service reports them.
    pub download: Option<(u64, u64)>,
    /// The lines the install reported, each with the step it belongs to.
    pub log: Vec<(Step, String)>,
    /// Whether the latest lines of the step's log are open under the steps.
    pub details: bool,
}

impl Setup {
    /// Preparing that starts from `step` once asked to.
    pub fn new(step: Step) -> Self {
        Self {
            step,
            running: false,
            warnings: Vec::new(),
            failed: None,
            activity: None,
            since: None,
            download: None,
            log: Vec::new(),
            details: false,
        }
    }
}

/// What the service install reports, line by line.
enum Report {
    Begun(String),
    Warning(String),
    Failed(String),
    /// A line of the install's output as it came, for the details.
    Line(String),
    Ended(Result<()>),
}

/// What the step in progress does now and how far it got.
struct Progress {
    /// Shown in place of the step's description.
    detail: String,
    /// The share done, from 0 to 1, when it is known.
    done: Option<f32>,
}

/// Programs the service runs, which its install puts on the machine.
const TOOLS: [&str; 4] = ["qemu-system-x86_64", "qemu-img", "passt", "nft"];
/// Where systemd finds them.
const TOOL_DIRS: [&str; 4] = ["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin"];
/// Files of the installed packages that no program shows: the UEFI firmware
/// and QEMU's PulseAudio backend.
const TOOL_FILES: [&str; 2] = [
    "/usr/share/edk2/ovmf/OVMF_CODE_4M.qcow2",
    "/usr/lib64/qemu/audio-pa.so",
];

/// Whether something the service runs is missing, which the install puts
/// in place.
fn tools_missing() -> bool {
    TOOLS.iter().any(|tool| {
        !TOOL_DIRS
            .iter()
            .any(|dir| Path::new(dir).join(tool).exists())
    }) || TOOL_FILES.iter().any(|file| !Path::new(file).exists())
}

/// Whether the computer still needs preparing, and from which step. A
/// machine that lacks what the service runs goes through the install again,
/// which installs it.
pub fn needed() -> Option<Step> {
    if !Path::new(gse_protocol::SOCKET).exists() || tools_missing() {
        return Some(Step::Check);
    }
    if !Path::new(BASE_PATH).exists() {
        return Some(Step::System);
    }
    None
}

/// Whether the processor's virtualization is on: the kernel then offers
/// `/dev/kvm`. A preview's machine has it.
fn virtualization_on() -> bool {
    crate::preview::active() || Path::new("/dev/kvm").exists()
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

/// Where the install's log goes: what it ran and what dnf or rpm-ostree
/// said, for when it stops or stalls. Each install replaces the last one's.
pub fn install_log_path() -> PathBuf {
    if crate::preview::active() {
        return PathBuf::from("/home/me/.cache/gse/install.log");
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("gse").join("install.log")
}

fn install_log() -> Stdio {
    let path = install_log_path();
    path.parent().map(std::fs::create_dir_all);
    std::fs::File::create(&path).map_or_else(|_| Stdio::null(), Stdio::from)
}

/// Runs the install through polkit and hands each line it reports to
/// `report`.
fn install(report: &futures::channel::mpsc::UnboundedSender<Report>) -> Result<()> {
    let staged = stage()?;
    let mut child = Command::new("pkexec")
        .arg(staged.join("gse-service"))
        .arg("install")
        .stdout(Stdio::piped())
        .stderr(install_log())
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
        // After the step it begins, so the line goes with that step.
        report.unbounded_send(Report::Line(line)).ok();
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
    if crate::preview::active() {
        return !crate::preview::flag("service-kept");
    }
    std::env::var_os("APPDIR").is_some()
}

/// How far a download got, as preparing shows it, and the share done: the
/// gigabytes done and in all once the service reports `bytes`, otherwise
/// its `percent`; then the time left at the pace it kept over the
/// `elapsed` seconds since its step began.
fn download_detail(
    bytes: Option<(u64, u64)>,
    percent: Option<u8>,
    elapsed: Option<u64>,
) -> (String, f64) {
    #[allow(clippy::cast_precision_loss)]
    let gigabytes = |count: u64| count as f64 / 1e9;
    let (mut text, done) = match (bytes, percent) {
        (Some((done, total)), _) if total > 0 => (
            format!("{:.1} of {:.1} GB", gigabytes(done), gigabytes(total)),
            (gigabytes(done) / gigabytes(total)).min(1.),
        ),
        (_, Some(percent)) => (
            format!("{percent}% downloaded"),
            f64::from(percent.min(100)) / 100.,
        ),
        _ => ("Starting the download".to_owned(), 0.),
    };
    // A pace needs a little of the download behind it.
    #[allow(clippy::cast_precision_loss)]
    let elapsed = elapsed.filter(|&s| s >= 5).map(|s| s as f64);
    if let Some(elapsed) = elapsed
        && done > 0.
        && done < 1.
    {
        text.push_str(" \u{b7} ");
        text.push_str(&time_left(elapsed * (1. - done) / done));
    }
    (text, done)
}

/// `seconds` still to go, as roughly as a person says it.
fn time_left(seconds: f64) -> String {
    let minutes = (seconds / 60.).round();
    if minutes < 1. {
        "less than a minute left".to_owned()
    } else if minutes < 2. {
        "about a minute left".to_owned()
    } else if minutes < 90. {
        format!("about {minutes:.0} minutes left")
    } else {
        format!("about {:.0} hours left", (minutes / 60.).round())
    }
}

/// The lines of a build's steps as the details show them: each `download
/// <percent>%` stands for the ones before it, so only the latest stays.
fn build_lines(build: &Build) -> Vec<String> {
    let downloading = |step: &String| step.starts_with("download ");
    build
        .steps
        .iter()
        .enumerate()
        .filter(|(i, step)| !(downloading(step) && build.steps.get(i + 1).is_some_and(downloading)))
        .map(|(_, step)| step.clone())
        .collect()
}

impl Home {
    /// Installs the service this app carries over an older one; the install
    /// restarts it.
    pub(crate) fn update_service(&mut self, cx: &mut Context<Self>) {
        if self.preview_skips("updating the service", cx) {
            return;
        }
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
        if self.preview_skips("preparing the computer", cx) {
            return;
        }
        let Some(setup) = self.setup.as_mut() else {
            return;
        };
        setup.running = true;
        setup.failed = None;
        setup.since = Some(crate::clock::now());
        setup.download = None;
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
        match &report {
            Report::Begun(step) => tracing::info!("preparing: {step}"),
            Report::Warning(text) => tracing::warn!("preparing: {text}"),
            Report::Failed(reason) => tracing::warn!("preparing failed: {reason}"),
            Report::Line(_) => {}
            Report::Ended(Ok(())) => tracing::info!("preparing done"),
            Report::Ended(Err(e)) => tracing::warn!("preparing stopped: {e:#}"),
        }
        let Some(setup) = self.setup.as_mut() else {
            return;
        };
        match report {
            Report::Begun(step) => {
                let (next, activity) = match step.as_str() {
                    "packages" => (
                        Step::Check,
                        Some("Installing what sandboxes run on: QEMU, the UEFI firmware, passt"),
                    ),
                    "check" => (
                        Step::Check,
                        Some("Checking virtualization and the graphics card"),
                    ),
                    "system" => (Step::System, None),
                    "launchers" => (Step::Launchers, None),
                    "files" => (Step::Service, Some("Writing the service's settings")),
                    "account" => (
                        Step::Service,
                        Some("Creating the service's network account"),
                    ),
                    "service" => (Step::Service, Some("Starting the service")),
                    _ => (Step::Service, Some("Copying the service")),
                };
                if next != setup.step {
                    setup.since = Some(crate::clock::now());
                    setup.download = None;
                }
                (setup.step, setup.activity) = (next, activity);
            }
            Report::Warning(text) => setup.warnings.push(text),
            Report::Failed(reason) => setup.failed = Some(reason),
            Report::Line(line) => setup.log.push((setup.step, line)),
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
                // Try again starts where the computer stands now: past the
                // install once the service is in place, back at it while
                // something it installs is missing.
                if let Some(next) = needed() {
                    setup.step = next;
                }
                Self::reload(cx);
            }
        }
    }

    /// Takes how far the download of Fedora got from the event of the
    /// sandbox system's build: the bytes of `download <percent>%`, none at
    /// any other step or from a service that reports only the percentage.
    pub(crate) fn follow_download(&mut self, download: Option<Downloaded>) {
        if let Some(setup) = self.setup.as_mut() {
            setup.download = download.map(|d| (d.received, d.total));
        }
    }

    /// The build `step` runs, while the service reports it.
    fn step_build(&self, step: Step) -> Option<&Build> {
        let name = match step {
            Step::System => BASE_IMAGE,
            Step::Launchers => FIRST_SET,
            Step::Check | Step::Service => return None,
        };
        self.presets.build.as_ref().filter(|b| b.preset == name)
    }

    /// What `step`, in progress, does now and how far it got: a download's
    /// size and time left, a build's step and share done, or what the
    /// install is at. `bytes` are the download's, done and in all, when the
    /// service reports them.
    fn step_progress(
        &self,
        setup: &Setup,
        step: Step,
        bytes: Option<(u64, u64)>,
    ) -> Option<Progress> {
        let Some(build) = self.step_build(step) else {
            return setup.activity.map(|activity| Progress {
                detail: activity.to_owned(),
                done: None,
            });
        };
        let last = build.steps.last().map_or("", String::as_str);
        if last == "download" || last.starts_with("download ") {
            let percent = last
                .strip_prefix("download ")
                .and_then(|p| p.trim_end_matches('%').parse().ok());
            let elapsed = setup
                .since
                .and_then(|since| u64::try_from((crate::clock::now() - since).num_seconds()).ok());
            let (detail, done) = download_detail(bytes, percent, elapsed);
            #[allow(clippy::cast_possible_truncation)]
            let done = done as f32;
            return Some(Progress {
                detail,
                done: Some(done),
            });
        }
        let (labels, reached) = self.build_steps(&build.preset)?;
        let percent = self.build_percent(&build.preset).unwrap_or(0);
        Some(Progress {
            detail: format!(
                "{} \u{b7} {percent}%",
                labels.get(reached).copied().unwrap_or_default()
            ),
            done: Some(f32::from(percent) / 100.),
        })
    }

    /// What checking the computer found: the graphics card a sandbox gets,
    /// once the service names it, and whether virtualization is on.
    fn check_findings(&self) -> Option<String> {
        let mut found = Vec::new();
        if let Ok((settings, gpus)) = &self.settings
            && let Some(gpu) = gpus.iter().find(|g| g.address == settings.gpu)
        {
            found.push(format!(
                "{} can be handed to the sandbox.",
                crate::settings::gpu_title(gpu, gpus)
            ));
        }
        if virtualization_on() {
            found.push("Virtualization is on.".to_owned());
        }
        (!found.is_empty()).then(|| found.join(" "))
    }

    /// The latest lines of the log of the step in progress: a build's
    /// steps, or what the install reported for the step.
    fn step_log(&self, setup: &Setup) -> Vec<String> {
        let lines = self.step_build(setup.step).map_or_else(
            || {
                setup
                    .log
                    .iter()
                    .filter(|(step, _)| *step == setup.step)
                    .map(|(_, line)| line.clone())
                    .collect()
            },
            build_lines,
        );
        let skip = lines.len().saturating_sub(DETAIL_LINES);
        lines.into_iter().skip(skip).collect()
    }

    /// One step of preparing: its mark, its title, and what it says now.
    /// The step in progress tells what it does in place of its description,
    /// over a bar of how far it got when that is known; `bytes` are the
    /// download's, done and in all, when the service reports them.
    fn setup_row(&self, setup: &Setup, index: usize, bytes: Option<(u64, u64)>) -> Div {
        let (step, title, text) = STEPS[index];
        let done = step < setup.step;
        let current = step == setup.step;
        let ahead = !done && !current;
        let progress = if current && setup.running {
            self.step_progress(setup, step, bytes)
        } else {
            None
        };
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
                        ui::kit_icon(Icon::Check)
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
                    m.border_2().border_color(Theme::ahead())
                }
            });
        let detail = match &progress {
            Some(progress) => progress.detail.clone(),
            None if done && step == Step::Check => {
                self.check_findings().unwrap_or_else(|| text.to_owned())
            }
            None => text.to_owned(),
        };
        // GPUI clips to rectangles, so the fill rounds its own ends: the
        // track's at the left, and at the right once it is full.
        let bar = progress.and_then(|p| p.done).map(|share| {
            let share = share.clamp(0., 1.);
            div().h(px(6.)).rounded(px(3.)).bg(Theme::track()).child(
                div()
                    .h_full()
                    .w(relative(share))
                    .rounded_l(px(3.))
                    .when(share >= 1., |fill| fill.rounded_r(px(3.)))
                    .bg(Theme::accent()),
            )
        });
        div()
            .flex()
            .gap_3p5()
            .px_3p5()
            .py(px(if current { 14. } else { 12. }))
            .rounded(px(12.))
            .when(current, |row| row.bg(Theme::surface()))
            .child(marker)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_2p5()
                    .child(
                        div()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .line_height(ui::normal_line(15.))
                                    .font_weight(if ahead {
                                        FontWeight::MEDIUM
                                    } else {
                                        FontWeight::SEMIBOLD
                                    })
                                    .text_color(if ahead { Theme::muted() } else { Theme::text() })
                                    .child(title),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(13.))
                                    .line_height(ui::normal_line(13.))
                                    .text_color(Theme::muted())
                                    .child(detail),
                            ),
                    )
                    .children(bar),
            )
    }

    /// The latest lines of the step's log, open under the steps.
    fn setup_details(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let lines = self.step_log(setup);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3p5()
            .py_3()
            .rounded(px(12.))
            .bg(Theme::panel())
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(12.))
            .text_color(Theme::muted())
            .map(|log| {
                if lines.is_empty() {
                    log.child("Nothing reported yet.")
                } else {
                    log.children(lines.into_iter().map(|line| div().truncate().child(line)))
                }
            })
    }

    /// The foot of the setup page: what to know at the left, what can be
    /// done now at the right. The window starts each build once the step
    /// before it is done; a build, once started, goes on in the service
    /// without the window.
    fn setup_footer(setup: &Setup, cx: &mut Context<Self>) -> Div {
        let installs = setup.step <= Step::Service;
        let note = match (setup.running, installs) {
            (true, true) => {
                "Keep the window open: it starts the builds once the service is installed."
            }
            (true, false) => "You can leave the window; this goes on in the background.",
            (false, true) => "Keep the window open while the service installs.",
            (false, false) => "The service is in place, so no password is needed.",
        };
        // The quiet buttons, as tall as the main one, on the raised fill.
        let quiet = |button: ui::Button| {
            button
                .size(Size::Large)
                .fill(Theme::raised())
                .text_size(px(13.))
                .px(px(16.))
        };
        let mut buttons = div().flex().flex_none().items_center().gap_2p5();
        if setup.running {
            buttons = buttons.child(quiet(button(
                "setup-details",
                if setup.details {
                    "Hide details"
                } else {
                    "Show details"
                },
                Kind::Quiet,
                true,
                cx.listener(|home, _, _, cx| {
                    if let Some(setup) = home.setup.as_mut() {
                        setup.details = !setup.details;
                        cx.notify();
                    }
                }),
            )));
        } else {
            if setup.failed.is_some() {
                buttons = buttons.child(quiet(button(
                    "setup-diagnostics",
                    "Save diagnostics",
                    Kind::Quiet,
                    true,
                    cx.listener(|home, _, _, cx| home.save_diagnostics(cx)),
                )));
            }
            buttons = buttons.child(
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
                .size(Size::Large)
                .px(px(16.)),
            );
        }
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .text_size(px(13.))
            .text_color(Theme::muted())
            .child(div().flex_1().min_w_0().child(note))
            .child(buttons)
    }

    /// The title of the setup page and what preparing is about.
    fn setup_heading(setup: &Setup) -> Div {
        // The password is asked as the install begins.
        let asked = if setup.running || setup.step > Step::Service {
            "was"
        } else {
            "is"
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(30.))
                    .line_height(ui::normal_line(30.))
                    .font_weight(FontWeight::BOLD)
                    .child("Getting this computer ready"),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(relative(1.5))
                    .text_color(Theme::secondary_text())
                    .child(format!(
                        "GSE runs untrusted games inside a sealed virtual computer. It needs a \
                         few system parts first. Your password {asked} asked once; the rest \
                         happens on its own and takes about 15 minutes."
                    )),
            )
    }

    /// The whole window while the computer is being prepared.
    pub(crate) fn setup_page(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Option<Div> {
        let setup = self.setup.as_ref()?;
        let mut steps = div().flex().flex_col().gap_1();
        for index in 0..STEPS.len() {
            steps = steps.child(self.setup_row(setup, index, setup.download));
        }
        let page = div()
            .flex()
            .flex_col()
            .gap_7()
            .w(px(600.))
            .child(Self::setup_heading(setup))
            .children(self.update_notice(cx))
            .child(steps)
            .children((setup.running && setup.details).then(|| self.setup_details(setup, cx)))
            .children(setup.failed.clone().map(|reason| {
                // The log's path starts a line of its own, so it does not
                // break where it has a slash.
                let reason = if setup.step <= Step::Service {
                    format!(
                        "{reason}.\nThe details are in {}.",
                        crate::config::shown_path(&install_log_path())
                    )
                } else {
                    reason
                };
                ui::notice(Tone::Danger, "Preparing stopped", reason)
            }))
            .children((!setup.warnings.is_empty()).then(|| {
                ui::notice(
                    Tone::Warning,
                    "Worth knowing",
                    setup.warnings.join(" \u{b7} "),
                )
            }))
            .child(Self::setup_footer(setup, cx))
            .children(self.toasts.last().map(|status| {
                div()
                    .text_size(px(13.))
                    .text_color(if status.error {
                        Theme::danger()
                    } else {
                        Theme::muted()
                    })
                    .child(status.text())
            }));
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
                            .pl_4()
                            .child(crate::shell::mark(26.))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("GSE"),
                            )
                            .into_any_element(),
                    ),
                    window,
                    cx,
                ))
                .child(
                    // The column stands in the middle of what is left, a
                    // little above it, and scrolls once it is taller.
                    div()
                        .id("setup")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .min_h_full()
                                .pb_10()
                                .child(page),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_says_its_size_and_the_time_left() {
        let (text, done) = download_detail(Some((1_200_000_000, 2_800_000_000)), None, Some(180));
        assert_eq!(text, "1.2 of 2.8 GB \u{b7} about 4 minutes left");
        assert!((done - 0.428_6).abs() < 1e-3);
        // Without the bytes, its percentage stands in.
        let (text, done) = download_detail(None, Some(43), Some(180));
        assert_eq!(text, "43% downloaded \u{b7} about 4 minutes left");
        assert!((done - 0.43).abs() < 1e-9);
        // Too early for a pace.
        let (text, _) = download_detail(Some((100_000_000, 2_800_000_000)), None, Some(2));
        assert_eq!(text, "0.1 of 2.8 GB");
        assert_eq!(download_detail(None, None, None).0, "Starting the download");
    }

    #[test]
    fn time_left_reads_roughly() {
        assert_eq!(time_left(20.), "less than a minute left");
        assert_eq!(time_left(70.), "about a minute left");
        assert_eq!(time_left(240.), "about 4 minutes left");
        assert_eq!(time_left(3.0 * 3600.), "about 3 hours left");
    }

    #[test]
    fn the_details_keep_only_the_latest_download_percentage() {
        let build = Build {
            preset: BASE_IMAGE.into(),
            steps: [
                "prepare",
                "download",
                "download 1%",
                "download 2%",
                "update",
            ]
            .map(str::to_owned)
            .to_vec(),
            started: None,
        };
        assert_eq!(
            build_lines(&build),
            ["prepare", "download", "download 2%", "update"]
        );
    }
}
