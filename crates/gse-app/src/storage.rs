//! App settings, General: where the sandboxes are kept, and moving them to
//! another disk (see `gse-service`'s `storage`).

use std::path::PathBuf;

use gpui::{Context, Div, PathPromptOptions, div, prelude::*, px};
use gse_protocol::Request;

use crate::Home;
use crate::client;
use crate::programs::size_text;
use crate::settings::Rows;
use crate::transfer::{Flow, bar};
use crate::ui::{Kind, Size, Theme, button};

/// What a move of the sandboxes says while it runs.
const MOVING: &str = "Moving the sandboxes";

impl Home {
    /// Asks where to move the sandboxes; the row then asks to confirm.
    fn choose_storage(cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Keep the sandboxes here".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            this.update(cx, |home, cx| {
                home.storage_target = paths.into_iter().next();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Moves the sandboxes to the disk of `to`.
    fn move_storage(&mut self, to: PathBuf, cx: &mut Context<Self>) {
        self.storage_target = None;
        if self.preview_skips("moving the sandboxes", cx) {
            return;
        }
        let flow = Flow::new(MOVING, self.storage.as_ref().map_or(0, |s| s.used));
        let (report, mut reports) = futures::channel::mpsc::unbounded::<(u64, u64)>();
        self.moving_storage = Some(flow);
        cx.notify();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let moved = cx.background_executor().spawn(async move {
                client::move_storage(&to, |done, total| {
                    report.unbounded_send((done, total)).ok();
                })
                .map(|()| to)
            });
            let follow = async {
                while let Some((done, total)) = reports.next().await {
                    this.update(cx, |home, cx| {
                        if let Some(flow) = home.moving_storage.as_mut() {
                            flow.done = done;
                            flow.total = total;
                        }
                        cx.notify();
                    })
                    .ok();
                }
            };
            let (moved, ()) = futures::join!(moved, follow);
            this.update(cx, |home, cx| {
                home.moving_storage = None;
                match moved {
                    Ok(to) => home.say(
                        format!("The sandboxes are kept on the disk of {}", to.display()),
                        false,
                    ),
                    Err(e) => home.say(format!("The sandboxes were not moved: {e:#}"), true),
                }
                Self::reload(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Stops the move under way; what it copied goes.
    fn cancel_storage_move(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = &self.moving_storage else {
            return;
        };
        flow.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        cx.background_executor()
            .spawn(async {
                if let Err(e) = client::command(&Request::CancelMove) {
                    tracing::warn!("stopping the move: {e:#}");
                }
            })
            .detach();
        cx.notify();
    }

    /// The row of where the sandboxes are kept: the place and its room,
    /// Change and, away from the system disk, Move back; a confirmation once
    /// a folder is chosen; the move's progress while it runs.
    /// The row while a move runs: how far it got, a bar and Cancel.
    fn storage_moving_row(flow: &Flow, cx: &mut Context<Self>) -> Div {
        let text = if flow.stopped() {
            "Stopping...".to_owned()
        } else if flow.total == 0 {
            format!("{MOVING}...")
        } else {
            flow.text()
        };
        Rows::Headlines.row(
            "Sandboxes are kept in",
            None,
            div()
                .flex()
                .flex_col()
                .gap_2()
                .w(px(380.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(Theme::accent())
                                .child(text),
                        )
                        .child(
                            button(
                                "storage-cancel",
                                "Cancel",
                                Kind::Secondary,
                                !flow.stopped(),
                                cx.listener(|home, _, _, cx| home.cancel_storage_move(cx)),
                            )
                            .size(Size::Row),
                        ),
                )
                .child(bar(flow.share())),
        )
    }

    /// The row asking to move the sandboxes to the disk of `target`.
    fn storage_confirm_row(&self, target: PathBuf, busy: bool, cx: &mut Context<Self>) -> Div {
        let place = if target == std::path::Path::new("/var/lib") {
            "back to the system disk".to_owned()
        } else {
            format!("to {}", crate::config::shown_path(&target.join("gse")))
        };
        Rows::Headlines.row(
            "Move the sandboxes?",
            Some(format!(
                "Everything, {}, is copied {place} and then removed here; nothing can start \
                 meanwhile.",
                self.storage
                    .as_ref()
                    .map_or_else(|| "all of it".to_owned(), |s| size_text(s.used))
            )),
            div()
                .flex()
                .gap_2()
                .child(
                    button(
                        "storage-keep",
                        "Cancel",
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, _, cx| {
                            home.storage_target = None;
                            cx.notify();
                        }),
                    )
                    .size(Size::Toolbar),
                )
                .child(
                    button(
                        "storage-move",
                        "Move",
                        Kind::Primary,
                        !busy,
                        cx.listener(move |home, _, _, cx| {
                            home.move_storage(target.clone(), cx);
                        }),
                    )
                    .size(Size::Toolbar),
                ),
        )
    }

    /// The row of where the sandboxes are kept: the place and its room,
    /// Change and, away from the system disk, Move back; a confirmation once
    /// a folder is chosen; the move's progress while it runs.
    pub(crate) fn storage_row(&self, cx: &mut Context<Self>) -> Div {
        if let Some(flow) = &self.moving_storage {
            return Self::storage_moving_row(flow, cx);
        }
        let busy = self.any_running() || self.presets.build.is_some();
        if let Some(target) = self.storage_target.clone() {
            return self.storage_confirm_row(target, busy, cx);
        }
        let (place, room) = match &self.storage {
            Some(storage) => (
                crate::config::shown_path(&storage.path),
                format!(
                    "{} taken, {} free there",
                    size_text(storage.used),
                    size_text(storage.free)
                ),
            ),
            None => (
                "/var/lib/gse".to_owned(),
                "the service does not say how much".to_owned(),
            ),
        };
        let away = self
            .storage
            .as_ref()
            .is_some_and(|s| s.path != std::path::Path::new("/var/lib/gse"));
        Rows::Headlines.row(
            "Sandboxes are kept in",
            Some(if busy {
                format!(
                    "{place} \u{b7} {room}. Stop the running sandbox and any build to move them."
                )
            } else {
                format!("{place} \u{b7} {room}. Another disk needs ext4, btrfs, xfs or f2fs.")
            }),
            div()
                .flex()
                .gap_2()
                .when(away, |row| {
                    row.child(
                        button(
                            "storage-back",
                            "Move back",
                            Kind::Secondary,
                            !busy,
                            cx.listener(|home, _, _, cx| {
                                home.storage_target = Some(PathBuf::from("/var/lib"));
                                cx.notify();
                            }),
                        )
                        .size(Size::Toolbar)
                        .fill(Theme::selected()),
                    )
                })
                .child(
                    button(
                        "storage-choose",
                        "Change",
                        Kind::Secondary,
                        !busy,
                        cx.listener(|_, _, _, cx| Self::choose_storage(cx)),
                    )
                    .size(Size::Toolbar)
                    .fill(Theme::selected()),
                ),
        )
    }
}
