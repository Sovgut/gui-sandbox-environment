//! Moving files in and out of a sandbox from one dialog. Files and folders
//! of this computer go into `guest`'s `~/Inbox` as they are; sent while the
//! sandbox is stopped, they wait with the service for its next start.
//! Everything in `guest`'s `~/Outbox` that passes the service's checks comes
//! out as one zip archive in the received files folder of app settings;
//! what the service took while a sandbox owned the screen comes out the
//! same way after the run. The app writes the archive itself, as the user,
//! and never opens what it holds.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use chrono::Local;
use gpui::{
    Context, Div, Entity, FontWeight, Hsla, PathPromptOptions, div, prelude::*, px, relative,
};
use gse_protocol::{GuestFile, GuestFiles, Request};
use zip::write::SimpleFileOptions;

use crate::client::{self, Taking};
use crate::config::{Look, shown_path};
use crate::icons::Icon;
use crate::programs::size_text;
use crate::sandbox::{DialogSize, dialog, dialog_text, marked, sentence, still_field};
use crate::settings::Rows;
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Size, Theme, Tone, button};
use crate::{Home, Modal};

/// What the last move of files did, for the panel to say.
pub enum Moved {
    /// `count` files went into `archive`, none when nothing passed the
    /// checks; `refused` are the files the take kept inside, with why.
    Taken {
        archive: Option<PathBuf>,
        count: usize,
        refused: Vec<(String, String)>,
    },
    /// That many files and folders went into the Inbox, or wait for the
    /// next start.
    Sent(usize),
    /// Taking files out failed, for this reason.
    TakeFailed(String),
    /// Sending files in failed, for this reason.
    SendFailed(String),
    /// Taking files out was stopped; nothing was written.
    TakeStopped,
    /// Sending files in was stopped: `copied` of `files` went in whole.
    SendStopped { copied: usize, files: usize },
}

/// A move of files under way: how far it got, and the flag that stops it.
pub(crate) struct Flow {
    /// What it is, [`TAKING`] or [`SENDING`].
    pub(crate) doing: &'static str,
    pub(crate) done: u64,
    pub(crate) total: u64,
    since: std::time::Instant,
    pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Flow {
    pub(crate) fn new(doing: &'static str, total: u64) -> Self {
        Self {
            doing,
            done: 0,
            total,
            since: std::time::Instant::now(),
            stop: std::sync::Arc::default(),
        }
    }

    /// A flow `elapsed` seconds old, for previews.
    pub(crate) fn at(doing: &'static str, done: u64, total: u64, elapsed: u64) -> Self {
        let mut flow = Self::new(doing, total);
        flow.done = done;
        flow.since = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(elapsed))
            .unwrap_or(flow.since);
        flow
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `412 MB of 1.2 GB · about 2 minutes left`; the time once there is
    /// a pace to tell it by.
    pub(crate) fn text(&self) -> String {
        let mut text = format!(
            "{}: {} of {}",
            self.doing,
            size_text(self.done.min(self.total)),
            size_text(self.total)
        );
        let elapsed = self.since.elapsed().as_secs_f64();
        if self.done > 0 && elapsed >= 3. && self.done < self.total {
            #[allow(clippy::cast_precision_loss)] // a pace
            let left = (self.total - self.done) as f64 / (self.done as f64 / elapsed);
            text.push_str(" \u{b7} ");
            text.push_str(&crate::setup::time_left(left));
        }
        text
    }

    /// How much of it is done, from 0 to 1.
    pub(crate) fn share(&self) -> f32 {
        if self.total == 0 {
            return 0.;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a share
        let share = (self.done as f64 / self.total as f64).clamp(0., 1.) as f32;
        share
    }
}

/// The stop of a take its Cancel asked for.
#[derive(Debug)]
struct TakeCancelled;

impl std::fmt::Display for TakeCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the take was stopped")
    }
}

impl std::error::Error for TakeCancelled {}

/// What the busy entry of a sandbox's files says while a move runs.
pub(crate) const TAKING: &str = "Taking files out";
pub(crate) const SENDING: &str = "Sending files in";

/// `count` of `one`, in its plural from 2 on: `1 file`, `2 files`.
fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// Whether `path`, from the sandbox, stays below the folder it is joined
/// to.
fn plain(path: &str) -> bool {
    path.len() <= 4096
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains('\0'))
}

/// Takes the Outbox of `name` into a zip archive in `folder`. Files arrive
/// one by one into a staging folder beside the archive, since one can still
/// be refused after it began; the archive holds those that came whole.
fn take_into(
    name: &str,
    folder: &Path,
    progress: &mut dyn FnMut(u64),
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Moved> {
    fs::create_dir_all(folder).with_context(|| format!("{}", folder.display()))?;
    let staging = folder.join(format!(".gse-taking-{name}-{}", std::process::id()));
    fs::remove_dir_all(&staging).ok();
    fs::create_dir(&staging)?;
    let mut current: Option<(String, File)> = None;
    let mut taken: Vec<String> = Vec::new();
    let mut refused = Vec::new();
    let outcome = client::take(name, |part| {
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(TakeCancelled.into());
        }
        match part {
            Taking::Start { path } => {
                if !plain(&path) {
                    bail!("the service sent the path '{path}'");
                }
                let target = staging.join(&path);
                fs::create_dir_all(target.parent().context("file path")?)?;
                current = Some((path.clone(), File::create_new(&target)?));
                taken.push(path);
            }
            Taking::Chunk(bytes) => {
                current
                    .as_mut()
                    .context("the service sent bytes of no file")?
                    .1
                    .write_all(&bytes)?;
                progress(bytes.len() as u64);
            }
            Taking::Refused { path, reason } => {
                if current.as_ref().is_some_and(|(p, _)| *p == path) {
                    current = None;
                    fs::remove_file(staging.join(&path)).ok();
                    taken.retain(|p| *p != path);
                }
                refused.push((path, reason));
            }
        }
        Ok(())
    });
    drop(current);
    if outcome
        .as_ref()
        .is_err_and(|e| e.downcast_ref::<TakeCancelled>().is_some())
    {
        fs::remove_dir_all(&staging).ok();
        return Ok(Moved::TakeStopped);
    }
    let archive = outcome.and_then(|()| {
        if taken.is_empty() {
            return Ok(None);
        }
        let stamp = Local::now().format("%Y-%m-%d %H.%M.%S");
        let archive = folder.join(format!("{name} {stamp}.zip"));
        write_zip(&staging, &taken, &archive)?;
        Ok(Some(archive))
    });
    fs::remove_dir_all(&staging).ok();
    Ok(Moved::Taken {
        archive: archive?,
        count: taken.len(),
        refused,
    })
}

fn write_zip(staging: &Path, files: &[String], archive: &Path) -> Result<()> {
    let mut zip = zip::ZipWriter::new(
        File::create_new(archive).with_context(|| format!("{}", archive.display()))?,
    );
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    for path in files {
        zip.start_file(path.as_str(), options)?;
        io::copy(&mut File::open(staging.join(path))?, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}

/// The heading of a part of the files panel, and what it does under it:
/// `before`, `strong` in the text's full colour, such as a folder, and
/// `after`.
fn section(title: &'static str, (before, strong, after): (&str, &str, &str)) -> Div {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::BOLD)
                .child(title),
        )
        .child(
            div()
                .mt(px(3.))
                .text_size(px(13.))
                .line_height(relative(1.45))
                .text_color(Theme::secondary_text())
                .child(marked(before, strong, after, Theme::text(), false)),
        )
}

/// A line under a part of the files panel that says how a move went.
fn outcome(color: Hsla, text: String) -> Div {
    div().text_xs().text_color(color).child(text)
}

/// A bar `share` full, from 0 to 1.
pub(crate) fn bar(share: f32) -> Div {
    div().h(px(6.)).rounded(px(3.)).bg(Theme::track()).child(
        div()
            .h_full()
            .w(relative(share))
            .rounded_l(px(3.))
            .when(share >= 1., |fill| fill.rounded_r(px(3.)))
            .bg(Theme::accent()),
    )
}

/// An item of a list: a disc in the gutter, level with its first line.
fn listed(text: String) -> Div {
    div()
        .flex()
        .items_start()
        .line_height(relative(1.8))
        .text_color(Theme::soft_text())
        .child(
            div().flex_none().w(px(18.)).child(
                div()
                    .ml(px(2.))
                    .mt(px(10.))
                    .size(px(5.))
                    .rounded_full()
                    .bg(Theme::soft_text()),
            ),
        )
        .child(text)
}

/// What the panel knows of the Inbox or the Outbox.
#[derive(Clone, Copy)]
enum Listed<'a> {
    Loading,
    /// The listing failed; the panel says why above.
    Failed,
    Files(&'a [GuestFile]),
}

/// How many files of the Outbox a take hands out as far as their names
/// tell; the service refuses others by how they start, which only the take
/// finds out.
fn passing(outbox: &[GuestFile]) -> usize {
    outbox
        .iter()
        .filter(|f| gse_protocol::kept_by_name(&f.path).is_none())
        .count()
}

/// The Outbox as a list, files whose name keeps them inside marked; none
/// when it could not be listed.
fn outbox_list(listed: Listed, empty: &'static str) -> Option<gpui::Stateful<Div>> {
    let mut list = div()
        .id("outbox-files")
        .flex()
        .flex_col()
        .max_h(px(180.))
        .overflow_y_scroll()
        .rounded(px(10.))
        .bg(Theme::well())
        .border_1()
        .border_color(Theme::border())
        .text_size(px(13.));
    let quiet = |text: &'static str| div().px_3().py_2().text_color(Theme::subtle()).child(text);
    let files = match listed {
        Listed::Failed => return None,
        Listed::Loading => return Some(list.child(quiet("Loading..."))),
        Listed::Files([]) => return Some(list.child(quiet(empty))),
        Listed::Files(files) => files,
    };
    for (i, file) in files.iter().enumerate() {
        let kept = gse_protocol::kept_by_name(&file.path);
        list = list.child(
            div()
                .flex()
                .gap_3()
                .px_3()
                .py(px(9.))
                .when(i > 0, |row| row.border_t_1().border_color(Theme::divider()))
                .when(kept.is_some(), |row| row.text_color(Theme::warning()))
                .child(div().flex_1().min_w_0().truncate().child(file.path.clone()))
                .child(match kept {
                    Some(what) => div().flex_none().child(format!("stays: {what}")),
                    None => div()
                        .flex_none()
                        .text_color(Theme::muted())
                        .child(size_text(file.size)),
                }),
        );
    }
    Some(list)
}

impl Home {
    pub(crate) fn open_transfer(&mut self, name: &str, cx: &mut Context<Self>) {
        self.moved = None;
        self.modal = Some(Modal::Transfer {
            name: name.to_owned(),
        });
        Self::load_files(name, cx);
        cx.notify();
    }

    /// Reloads the Inbox and Outbox of `name`.
    pub(crate) fn load_files(name: &str, cx: &mut Context<Self>) {
        let name = name.to_owned();
        cx.spawn(async move |this, cx| {
            let request = name.clone();
            let files = cx
                .background_executor()
                .spawn(async move { client::files(&request) })
                .await;
            this.update(cx, |home, cx| {
                home.files = Some((name, files.map_err(|e| format!("{e:#}").into())));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Lists the Inbox and Outbox of `name` again after a listing failed,
    /// showing them as loading meanwhile.
    fn list_files_again(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.preview_skips("listing the files again", cx) {
            return;
        }
        self.files = None;
        Self::load_files(name, cx);
        cx.notify();
    }

    /// Runs `work`, a move of files for `name` that `doing` names, then
    /// lists its folders again. The panel says how it went while it shows
    /// that sandbox; an archive it wrote is the sandbox's last one.
    fn move_files(
        &mut self,
        name: &str,
        doing: &'static str,
        work: impl FnOnce() -> Moved + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let key = format!("{name}/files");
        self.busy.insert(key.clone(), doing);
        self.moved = None;
        cx.notify();
        let name = name.to_owned();
        cx.spawn(async move |this, cx| {
            let moved = cx.background_executor().spawn(async move { work() }).await;
            this.update(cx, |home, cx| {
                home.busy.remove(&key);
                if let Moved::Taken {
                    archive: Some(archive),
                    ..
                } = &moved
                {
                    home.last_taken.insert(name.clone(), archive.clone());
                }
                if matches!(&home.modal, Some(Modal::Transfer { name: shown }) if *shown == name) {
                    home.moved = Some(moved);
                } else {
                    home.tell_moved(&name, moved);
                }
                Self::load_files(&name, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Says how a move for `name` went in toasts, for when its panel is no
    /// longer open: what was taken, what stayed inside and why, or the error.
    fn tell_moved(&mut self, name: &str, moved: Moved) {
        match moved {
            Moved::Taken {
                archive,
                count,
                refused,
            } => {
                if let Some(archive) = archive {
                    self.tell(crate::Toast::taken(name, count, archive));
                }
                if !refused.is_empty() {
                    let why = refused
                        .iter()
                        .map(|(file, reason)| format!("{file} is {reason}"))
                        .collect::<Vec<_>>()
                        .join("; ");
                    self.tell(
                        crate::Toast::error(
                            format!(
                                "{} stayed in the sandbox",
                                counted(refused.len(), "file", "files")
                            ),
                            format!("{why}. Programs can't leave a sandbox."),
                        )
                        .about(name),
                    );
                }
            }
            Moved::Sent(_) | Moved::TakeStopped | Moved::SendStopped { .. } => {}
            Moved::TakeFailed(why) => self
                .tell(crate::Toast::error("Taking files out failed", sentence(&why)).about(name)),
            Moved::SendFailed(why) => self
                .tell(crate::Toast::error("Sending files in failed", sentence(&why)).about(name)),
        }
    }

    fn take_files(&mut self, name: &str, cx: &mut Context<Self>) {
        let total = match &self.files {
            Some((shown, Ok(files))) if shown == name => files
                .outbox
                .iter()
                .filter(|f| gse_protocol::kept_by_name(&f.path).is_none())
                .map(|f| f.size)
                .sum(),
            _ => 0,
        };
        let flow = Flow::new(TAKING, total);
        let stop = flow.stop.clone();
        let report = self.follow_flow(name, flow, cx);
        let (owner, folder) = (name.to_owned(), self.app_config.received.clone());
        self.move_files(
            name,
            TAKING,
            move || {
                let mut taken = 0;
                let mut progress = |part| {
                    taken += part;
                    report.unbounded_send((taken, total)).ok();
                };
                take_into(&owner, &folder, &mut progress, &stop)
                    .unwrap_or_else(|e| Moved::TakeFailed(format!("{e:#}")))
            },
            cx,
        );
    }

    /// Notes `flow` as the move under way for `name`; the sender it gives
    /// takes the bytes moved and in all as they go. The flow ends when the
    /// sender is dropped.
    fn follow_flow(
        &mut self,
        name: &str,
        flow: Flow,
        cx: &mut Context<Self>,
    ) -> futures::channel::mpsc::UnboundedSender<(u64, u64)> {
        self.flows.insert(name.to_owned(), flow);
        let (report, mut reports) = futures::channel::mpsc::unbounded::<(u64, u64)>();
        let owner = name.to_owned();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some((done, total)) = reports.next().await {
                this.update(cx, |home, cx| {
                    if let Some(flow) = home.flows.get_mut(&owner) {
                        flow.done = done;
                        flow.total = total;
                    }
                    cx.notify();
                })
                .ok();
            }
            this.update(cx, |home, cx| {
                home.flows.remove(&owner);
                cx.notify();
            })
            .ok();
        })
        .detach();
        report
    }

    /// Puts `paths` of this computer into the Inbox of `name`.
    fn send_paths(&mut self, name: &str, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let count = paths.len();
        let request = Request::SendFiles {
            name: name.to_owned(),
            paths,
        };
        let report = self.follow_flow(name, Flow::new(SENDING, 0), cx);
        self.move_files(
            name,
            SENDING,
            move || {
                let sent = client::send_files(&request, |done, total| {
                    report.unbounded_send((done, total)).ok();
                });
                match sent {
                    Ok(None) => Moved::Sent(count),
                    Ok(Some((copied, files))) => Moved::SendStopped { copied, files },
                    Err(e) => Moved::SendFailed(format!("{e:#}")),
                }
            },
            cx,
        );
    }

    /// Stops the move of files under way for `name`. A take stops at its
    /// next part and writes nothing; a send asks the service to stop, which
    /// keeps the files that went in whole.
    pub(crate) fn cancel_move(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some((stop, doing)) = self.flows.get(name).map(|f| (f.stop.clone(), f.doing)) else {
            return;
        };
        if stop.load(std::sync::atomic::Ordering::SeqCst)
            || self.preview_skips("stopping the move of files", cx)
        {
            return;
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if doing == SENDING {
            let request = Request::CancelTransfer {
                name: name.to_owned(),
            };
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = client::command(&request) {
                        tracing::warn!("stopping a send: {e:#}");
                    }
                })
                .detach();
        }
        cx.notify();
    }

    /// The move of files under way for `name` as a line, a bar and Cancel;
    /// `id` keeps its button apart from another view of the same flow.
    pub(crate) fn flow_view(
        &self,
        name: &str,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let flow = self.flows.get(name)?;
        let text = if flow.stopped() {
            "Stopping...".to_owned()
        } else if flow.total == 0 {
            format!("{}...", flow.doing)
        } else {
            flow.text()
        };
        let owner = name.to_owned();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(Theme::accent())
                                .child(text),
                        )
                        .child(
                            button(
                                id,
                                "Cancel",
                                Kind::Secondary,
                                !flow.stopped(),
                                cx.listener(move |home, _, _, cx| home.cancel_move(&owner, cx)),
                            )
                            .size(Size::Row),
                        ),
                )
                .child(bar(flow.share())),
        )
    }

    /// Asks for files, or for folders, and puts them into the Inbox.
    fn choose_and_send(name: &str, folders: bool, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: !folders,
            directories: folders,
            multiple: true,
            prompt: Some("Put into the Inbox".into()),
        });
        let name = name.to_owned();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            this.update(cx, |home, cx| home.send_paths(&name, paths, cx))
                .ok();
        })
        .detach();
    }

    /// A line naming `archive` after `text`, with a link to the folder it
    /// is in.
    fn archive_line(text: String, archive: &Path) -> Div {
        let folder = archive.parent().map(Path::to_path_buf).unwrap_or_default();
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .text_xs()
            .text_color(Theme::good())
            .child(text)
            .child("\u{b7}")
            .child(
                ui::link("show-taken", "Show in folder")
                    .text_xs()
                    .on_click(move |_, _, _| {
                        if crate::preview::active() {
                            return;
                        }
                        std::process::Command::new("xdg-open")
                            .arg(&folder)
                            .spawn()
                            .ok();
                    }),
            )
    }

    /// How the take of `name` going on or just done goes, or else the
    /// archive its Outbox last went into. Files a take kept inside are
    /// named here unless the list marks them already.
    fn take_outcome(&self, name: &str, busy: Option<&str>, cx: &mut Context<Self>) -> Vec<Div> {
        if busy == Some(TAKING) {
            return vec![
                self.flow_view(name, "cancel-take", cx)
                    .unwrap_or_else(|| outcome(Theme::accent(), format!("{TAKING}..."))),
            ];
        }
        let file_name = |archive: &Path| {
            archive
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        match &self.moved {
            Some(Moved::Taken {
                archive,
                count,
                refused,
            }) => {
                let head = match archive {
                    Some(archive) => Self::archive_line(
                        format!(
                            "{} taken out: {}",
                            counted(*count, "file", "files"),
                            file_name(archive)
                        ),
                        archive,
                    ),
                    None => outcome(
                        Theme::warning(),
                        "Nothing passed the checks; no archive was written.".to_owned(),
                    ),
                };
                std::iter::once(head)
                    .chain(
                        refused
                            .iter()
                            .filter(|(path, _)| gse_protocol::kept_by_name(path).is_none())
                            .map(|(path, reason)| {
                                outcome(Theme::warning(), format!("{path} stayed inside: {reason}"))
                            }),
                    )
                    .collect()
            }
            Some(Moved::TakeFailed(e)) => vec![outcome(
                Theme::danger(),
                format!("Taking files out failed: {e}"),
            )],
            Some(Moved::TakeStopped) => vec![outcome(
                Theme::warning(),
                "Stopped: nothing was taken out.".to_owned(),
            )],
            _ => self
                .last_taken
                .get(name)
                .map(|archive| {
                    Self::archive_line(format!("Last time: {}", file_name(archive)), archive)
                })
                .into_iter()
                .collect(),
        }
    }

    /// How the sending going on or just done goes.
    fn send_outcome(
        &self,
        name: &str,
        up: bool,
        busy: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if busy == Some(SENDING) {
            return Some(
                self.flow_view(name, "cancel-send", cx)
                    .unwrap_or_else(|| outcome(Theme::accent(), format!("{SENDING}..."))),
            );
        }
        match &self.moved {
            Some(Moved::Sent(count)) => {
                let items = counted(*count, "item", "items");
                Some(outcome(
                    Theme::good(),
                    match (up, *count) {
                        (true, _) => format!("{items} went into the Inbox."),
                        (false, 1) => format!("{items} waits for the next start."),
                        (false, _) => format!("{items} wait for the next start."),
                    },
                ))
            }
            Some(Moved::SendFailed(e)) => Some(outcome(
                Theme::danger(),
                format!("Sending files in failed: {e}"),
            )),
            Some(Moved::SendStopped { copied, files }) => Some(outcome(
                Theme::warning(),
                format!(
                    "Stopped: {copied} of {} went in.",
                    counted(*files, "item", "items")
                ),
            )),
            _ => None,
        }
    }

    fn take_section(
        &self,
        name: &str,
        outbox: Listed,
        (busy, up): (Option<&'static str>, bool),
        cx: &mut Context<Self>,
    ) -> Div {
        let taken = match outbox {
            Listed::Files(files) => passing(files),
            Listed::Loading | Listed::Failed => 0,
        };
        let take = name.to_owned();
        let folder = shown_path(&self.app_config.received);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section(
                "Take out",
                if up {
                    (
                        "What is in the sandbox's Outbox comes to ",
                        &folder,
                        " as one zip. Programs, scripts and archives stay inside.",
                    )
                } else {
                    (
                        "What its Outbox held while it ran on the whole screen was taken out \
                         then, and comes to ",
                        &folder,
                        " as one zip. Programs, scripts and archives stayed inside.",
                    )
                },
            ))
            .children(outbox_list(
                outbox,
                if up {
                    "The Outbox is empty."
                } else {
                    "Nothing was taken out of it."
                },
            ))
            .child(
                button(
                    "take-files",
                    match taken {
                        0 => "Take files out".to_owned(),
                        1 => "Take 1 file out".to_owned(),
                        n => format!("Take {n} files out"),
                    },
                    Kind::Primary,
                    busy.is_none() && taken > 0,
                    cx.listener(move |home, _, _, cx| home.take_files(&take, cx)),
                )
                .size(Size::Dialog)
                .icon(Icon::Download)
                .h(px(42.))
                .w_full(),
            )
            .children(self.take_outcome(name, busy, cx))
    }

    fn send_section(
        &self,
        name: &str,
        inbox: Listed,
        (busy, up): (Option<&'static str>, bool),
        cx: &mut Context<Self>,
    ) -> Div {
        let (send_files, send_folders) = (name.to_owned(), name.to_owned());
        let place = if up {
            "In the Inbox now"
        } else {
            "Waiting for the next start"
        };
        let now = match inbox {
            Listed::Loading => Some("Loading...".to_owned()),
            Listed::Failed => None,
            Listed::Files([]) if up => Some("The Inbox is empty.".to_owned()),
            Listed::Files([]) => Some("Nothing waits to go in.".to_owned()),
            Listed::Files([one]) => Some(format!(
                "{place}: {} \u{b7} {}",
                one.path,
                size_text(one.size)
            )),
            Listed::Files(many) => Some(format!("{place}: {} items", many.len())),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section(
                "Send in",
                (
                    if up {
                        "Files and folders land in the sandbox's Inbox, where its programs open \
                         and run them."
                    } else {
                        "Files and folders wait here and land in the sandbox's Inbox when it \
                         next starts, where its programs open and run them."
                    },
                    "",
                    "",
                ),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2p5()
                    // 150 inside its border, as the design draws it.
                    .h(px(154.))
                    .rounded(px(12.))
                    .border_2()
                    .border_dashed()
                    .border_color(Theme::pending())
                    .text_sm()
                    .text_color(Theme::secondary_text())
                    .child(ui::icon(Icon::Upload, Theme::secondary_text()).size(px(26.)))
                    .child("Drop files or folders here")
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button(
                                    "send-files",
                                    "Choose files",
                                    Kind::Secondary,
                                    busy.is_none(),
                                    cx.listener(move |_, _, _, cx| {
                                        Self::choose_and_send(&send_files, false, cx);
                                    }),
                                )
                                .fill(Theme::selected()),
                            )
                            .child(
                                button(
                                    "send-folders",
                                    "Choose a folder",
                                    Kind::Secondary,
                                    busy.is_none(),
                                    cx.listener(move |_, _, _, cx| {
                                        Self::choose_and_send(&send_folders, true, cx);
                                    }),
                                )
                                .fill(Theme::selected()),
                            ),
                    ),
            )
            .children(now.map(|now| {
                div()
                    .text_size(px(13.))
                    .text_color(Theme::muted())
                    .child(now)
            }))
            .children(self.send_outcome(name, up, busy, cx))
    }

    /// Taking files out of `name` and sending files in: its Files tab, and
    /// the panel beside its screen.
    pub(crate) fn files_body(&self, name: &str, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.files_list(name, cx))
    }

    /// The take and the send as parts ruled off from each other, under the
    /// reason the folders could not be listed.
    fn files_list(&self, name: &str, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let busy = self.busy.get(&format!("{name}/files")).copied();
        let files = self
            .files
            .as_ref()
            .filter(|(n, _)| n == name)
            .map(|(_, f)| f);
        let (inbox, outbox, problem) = match files {
            Some(Ok(GuestFiles { inbox, outbox })) => {
                (Listed::Files(inbox), Listed::Files(outbox), None)
            }
            Some(Err(e)) => (Listed::Failed, Listed::Failed, Some(e.clone())),
            None => (Listed::Loading, Listed::Loading, None),
        };
        let again = name.to_owned();
        let drop_to = name.to_owned();
        let up = self.environments.iter().any(|e| {
            e.name == name
                && e.running
                    .as_ref()
                    .is_some_and(|r| r.stage == gse_protocol::Stage::Ready)
        });
        div()
            .id("files-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.files_scroll)
            .on_drop(
                cx.listener(move |home, paths: &gpui::ExternalPaths, _, cx| {
                    home.send_paths(&drop_to, paths.paths().to_vec(), cx);
                }),
            )
            .children(problem.map(|reason| {
                div().px_6().pt_5().child(
                    ui::notice(Tone::Warning, "Files are unavailable", sentence(&reason)).child(
                        button(
                            "files-retry",
                            "Try again",
                            Kind::Secondary,
                            true,
                            cx.listener(move |home, _, _, cx| home.list_files_again(&again, cx)),
                        ),
                    ),
                )
            }))
            .child(
                self.take_section(name, outbox, (busy, up), cx)
                    .px_6()
                    .py_5()
                    .border_b_1()
                    .border_color(Theme::border()),
            )
            .child(self.send_section(name, inbox, (busy, up), cx).px_6().py_5())
    }

    /// The look's choice: system, light or dark.
    fn look_choice(&self, cx: &mut Context<Self>) -> Div {
        let look = self.app_config.look;
        let choice =
            |id: &'static str, label: &'static str, value: Look, cx: &mut Context<Self>| {
                ui::segment(
                    id,
                    label,
                    look == value,
                    true,
                    cx.listener(move |home, _, window, cx| home.set_look(value, window, cx)),
                )
            };
        ui::segments()
            .child(choice("look-system", "System", Look::System, cx))
            .child(choice("look-light", "Light", Look::Light, cx))
            .child(choice("look-dark", "Dark", Look::Dark, cx))
    }

    /// App settings, General: the look, where taken files go, where the
    /// sandboxes are kept, the tour, and removing GSE.
    pub(crate) fn general_view(&self, cx: &mut Context<Self>) -> Div {
        let looks = self.look_choice(cx);
        let folder = shown_path(&self.app_config.received);
        let rows = Rows::Headlines.group(
            None,
            vec![
                Rows::Headlines.row(
                    "Look",
                    Some("Follows the system unless you pick one.".to_owned()),
                    looks,
                ),
                Rows::Headlines.row(
                    "Files taken out go to",
                    Some(folder),
                    button(
                        "received-choose",
                        "Change",
                        Kind::Secondary,
                        true,
                        cx.listener(|_, _, _, cx| Self::choose_received(cx)),
                    )
                    .size(Size::Toolbar)
                    .fill(Theme::selected()),
                ),
                self.storage_row(cx),
                Rows::Headlines.row(
                    "Show the welcome tour again",
                    Some("From the first step, any time.".to_owned()),
                    button(
                        "start-tour",
                        "Start tour",
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, _, cx| home.start_tour(cx)),
                    )
                    .size(Size::Toolbar)
                    .fill(Theme::selected()),
                ),
            ],
        );
        div().flex().flex_col().gap_7().child(rows).child(
            div()
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(Theme::danger_text())
                        .child("Remove GSE"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_5()
                        .px(px(18.))
                        .py_4()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(Theme::danger().opacity(0.25))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(13.))
                                .line_height(px(20.))
                                .text_color(Theme::secondary_text())
                                .child(
                                    "Stops every sandbox and removes the service, every \
                                     sandbox with all inside it, and these settings. Nothing \
                                     of GSE stays but the app file.",
                                ),
                        )
                        .child(
                            button(
                                "remove-ask",
                                "Remove everything",
                                Kind::DangerOutline,
                                true,
                                cx.listener(|home, _, _, cx| home.open_remove(cx)),
                            )
                            .size(Size::Toolbar)
                            .h(px(36.)),
                        ),
                ),
        )
    }

    /// Switches the look and keeps the choice.
    fn set_look(&mut self, look: Look, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.app_config.look = look;
        if let Err(e) = self.app_config.save() {
            self.say(format!("The setting was not saved: {e:#}"), true);
        }
        Theme::apply(look.light(window.appearance()), cx);
        cx.refresh_windows();
    }

    /// The question before removing everything as it opens: the taken files
    /// kept, nothing typed, nothing measured yet.
    pub(crate) fn ask_remove(&mut self, cx: &mut Context<Self>) {
        let confirm = cx.new(|cx| TextInput::new("", cx).fill(Theme::background));
        cx.observe(&confirm, |_, _, cx| cx.notify()).detach();
        self.modal = Some(Modal::RemoveAll {
            keep: true,
            confirm,
            sizes: None,
        });
        cx.notify();
    }

    /// Opens the question before removing everything, and measures what
    /// would go.
    pub(crate) fn open_remove(&mut self, cx: &mut Context<Self>) {
        self.ask_remove(cx);
        let received = self.app_config.received.clone();
        cx.spawn(async move |this, cx| {
            let sizes = cx
                .background_executor()
                .spawn(async move { Sizes::measure(&received) })
                .await;
            this.update(cx, |home, cx| {
                if let Some(Modal::RemoveAll { sizes: shown, .. }) = home.modal.as_mut() {
                    *shown = Some(sizes);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The choice to keep the taken files when removing everything. It holds
    /// still once removing began.
    fn keep_box(keep: bool, detail: String, removing: bool, cx: &mut Context<Self>) -> Div {
        div()
            .px_3p5()
            .py_3()
            .rounded(px(10.))
            .bg(Theme::well())
            .child(
                ui::checkbox(
                    "remove-keep",
                    keep,
                    !removing,
                    // The small line keeps the title's 14 px lines, as the
                    // design sets it.
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_sm().child("Keep files taken from sandboxes"))
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(17.))
                                .text_color(Theme::muted())
                                .child(detail),
                        ),
                    cx.listener(|home, on: &bool, _, cx| {
                        if let Some(Modal::RemoveAll { keep, .. }) = home.modal.as_mut() {
                            *keep = *on;
                            cx.notify();
                        }
                    }),
                )
                .items_start(),
            )
    }

    /// The question before removing everything.
    pub(crate) fn remove_dialog(
        &self,
        keep: bool,
        confirm: &Entity<TextInput>,
        sizes: Option<&Sizes>,
        cx: &mut Context<Self>,
    ) -> Div {
        let removing = self.busy.contains_key("remove-everything");
        let typed = confirm.read(cx).text().trim() == "remove";
        let (sandboxes, sets, received) = Sizes::lines(sizes);
        let folder = shown_path(&self.app_config.received);
        let fate = if keep {
            "stays where it is"
        } else {
            "is deleted too"
        };
        let kept = match received {
            Some(size) => format!("{folder} \u{b7} {size} {fate}"),
            None => format!("{folder} {fate}"),
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(dialog_text("This stops every sandbox and deletes:"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(listed(sandboxes))
                    .child(listed(sets))
                    .child(listed(
                        "The background service and its system settings".to_owned(),
                    )),
            )
            .child(Self::keep_box(keep, kept, removing, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(Theme::secondary_text())
                            .child(marked(
                                "Type ",
                                "remove",
                                " to confirm",
                                Theme::text(),
                                true,
                            )),
                    )
                    .child(if removing {
                        still_field(confirm.read(cx).text().to_owned()).into_any_element()
                    } else {
                        confirm.clone().into_any_element()
                    }),
            )
            .child(div().text_xs().text_color(Theme::muted()).child(
                "Your system asks for the administrator password. The app quits when it is done.",
            ));
        dialog(
            DialogSize::Medium,
            "Remove GSE from this computer?",
            body,
            Self::remove_buttons(keep, typed, removing, cx),
        )
        .border_color(Theme::danger().opacity(0.3))
    }

    /// Cancel, and the button that removes everything once `typed`, the
    /// taken files too unless `keep`.
    fn remove_buttons(keep: bool, typed: bool, removing: bool, cx: &mut Context<Self>) -> Div {
        div()
            .child(
                button(
                    "remove-cancel",
                    "Cancel",
                    Kind::Quiet,
                    !removing,
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
                    "remove-go",
                    if removing {
                        "Removing..."
                    } else {
                        "Remove everything"
                    },
                    Kind::DangerFilled,
                    typed && !removing,
                    cx.listener(move |home, _, _, cx| home.remove_everything(!keep, cx)),
                )
                .size(Size::Dialog),
            )
    }

    /// Runs the service's uninstall through polkit, removes the app's own
    /// files, and the received files folder with `received`, and closes the
    /// app.
    fn remove_everything(&mut self, received: bool, cx: &mut Context<Self>) {
        if self.preview_skips("removing everything", cx) {
            return;
        }
        self.busy.insert("remove-everything".into(), "Removing");
        cx.notify();
        let received = received.then(|| self.app_config.received.clone());
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { crate::config::remove_everything(received.as_deref()) })
                .await;
            this.update(cx, |home, cx| {
                home.busy.remove("remove-everything");
                match outcome {
                    Ok(()) => cx.quit(),
                    Err(e) => {
                        home.modal = None;
                        home.say(format!("Removing failed: {e:#}"), true);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_received(cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            this.update(cx, |home, cx| {
                if let Some(path) = paths.into_iter().next() {
                    home.app_config.received = path;
                    match home.app_config.save() {
                        Ok(()) => home.say("Taken files go to the new folder", false),
                        Err(e) => home.say(format!("{e:#}"), true),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

/// What "Remove everything" would delete, measured on the disk.
pub struct Sizes {
    sandboxes: usize,
    sandbox_bytes: u64,
    sets: usize,
    set_bytes: u64,
    received_bytes: u64,
}

/// The space the files under `path` take on the disk, sparse disks by what
/// they hold.
fn disk_usage(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| disk_usage(&e.path()))
            .sum()
    } else {
        meta.blocks() * 512
    }
}

impl Sizes {
    /// What goes in words, measured or being measured, and how much the
    /// received files folder holds once measured. The sets' size takes in
    /// the sandbox system they are built on.
    fn lines(sizes: Option<&Self>) -> (String, String, Option<String>) {
        sizes.map_or_else(
            || {
                (
                    "Sandboxes with everything inside them \u{b7} measuring...".to_owned(),
                    "Sets of apps \u{b7} measuring...".to_owned(),
                    None,
                )
            },
            |s| {
                (
                    format!(
                        "{} with everything inside them \u{b7} {}",
                        counted(s.sandboxes, "sandbox", "sandboxes"),
                        size_text(s.sandbox_bytes)
                    ),
                    format!(
                        "{} \u{b7} {}",
                        counted(s.sets, "set of apps", "sets of apps"),
                        size_text(s.set_bytes)
                    ),
                    Some(size_text(s.received_bytes)),
                )
            },
        )
    }

    fn measure(received: &Path) -> Self {
        if crate::preview::active() {
            return Self {
                sandboxes: 4,
                sandbox_bytes: 61_000_000_000,
                sets: 3,
                set_bytes: 18_000_000_000,
                received_bytes: 2_300_000_000,
            };
        }
        let root = Path::new("/var/lib/gse");
        let count = |dir: &str, toml: bool| {
            fs::read_dir(root.join(dir))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| !toml || e.path().extension().is_some_and(|x| x == "toml"))
                .count()
        };
        Self {
            sandboxes: count("environments", false),
            sandbox_bytes: disk_usage(&root.join("environments")),
            sets: count("presets", true),
            set_bytes: disk_usage(&root.join("images")) + disk_usage(&root.join("base.qcow2")),
            received_bytes: disk_usage(received),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_from_the_sandbox_stay_in_the_staging_folder() {
        assert!(plain("saves/slot1.sav"));
        for bad in ["", "/etc/x", "../x", "a/../../b", "a//b", "."] {
            assert!(!plain(bad), "{bad}");
        }
    }

    #[test]
    fn taken_files_land_in_a_zip_archive() {
        let dir = tempfile_dir();
        let staging = dir.join("staging");
        fs::create_dir_all(staging.join("saves")).unwrap();
        fs::write(staging.join("saves/slot.sav"), b"SAVE").unwrap();
        let archive = dir.join("out.zip");
        write_zip(&staging, &["saves/slot.sav".to_owned()], &archive).unwrap();
        let mut zip = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
        let mut entry = zip.by_name("saves/slot.sav").unwrap();
        let mut text = String::new();
        io::Read::read_to_string(&mut entry, &mut text).unwrap();
        assert_eq!(text, "SAVE");
        assert!(
            write_zip(&staging, &[], &archive).is_err(),
            "never overwrites"
        );
        fs::remove_dir_all(dir).ok();
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gse-app-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
