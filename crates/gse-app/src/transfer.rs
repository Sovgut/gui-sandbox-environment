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
use gpui::{Context, Div, Entity, FontWeight, PathPromptOptions, div, prelude::*, px};
use gse_protocol::{GuestFile, GuestFiles, Request};
use zip::write::SimpleFileOptions;

use crate::client::{self, Taking};
use crate::config::Look;
use crate::icons::Icon;
use crate::programs::size_text;
use crate::settings::{group, row};
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, button};
use crate::{Home, Modal};

/// What the last move of files did, for the dialog to say.
pub enum Moved {
    Taken {
        archive: Option<PathBuf>,
        count: usize,
        refused: Vec<(String, String)>,
    },
    Sent(usize),
    Failed(String),
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
fn take_into(name: &str, folder: &Path) -> Result<Moved> {
    fs::create_dir_all(folder).with_context(|| format!("{}", folder.display()))?;
    let staging = folder.join(format!(".gse-taking-{name}-{}", std::process::id()));
    fs::remove_dir_all(&staging).ok();
    fs::create_dir(&staging)?;
    let mut current: Option<(String, File)> = None;
    let mut taken: Vec<String> = Vec::new();
    let mut refused = Vec::new();
    let outcome = client::take(name, |part| {
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
            Taking::Chunk(bytes) => current
                .as_mut()
                .context("the service sent bytes of no file")?
                .1
                .write_all(&bytes)?,
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

fn section(title: &'static str, text: String) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().font_weight(FontWeight::BOLD).child(title))
        .child(
            div()
                .text_sm()
                .line_height(px(19.))
                .text_color(Theme::secondary_text())
                .child(text),
        )
}

fn file_list(id: &'static str, files: &[GuestFile], empty: &'static str) -> impl IntoElement {
    let mut list = div()
        .id(id)
        .flex()
        .flex_col()
        .max_h(px(180.))
        .overflow_y_scroll()
        .rounded(px(10.))
        .bg(Theme::well())
        .border_1()
        .border_color(Theme::border())
        .text_sm();
    if files.is_empty() {
        return list.child(div().px_3().py_2().text_color(Theme::subtle()).child(empty));
    }
    for (i, file) in files.iter().enumerate() {
        list = list.child(
            div()
                .flex()
                .gap_3()
                .px_3()
                .py_2()
                .when(i > 0, |row| row.border_t_1().border_color(Theme::divider()))
                .child(div().flex_1().min_w_0().truncate().child(file.path.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_color(Theme::muted())
                        .child(size_text(file.size)),
                ),
        );
    }
    list
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

    /// Runs `work`, a move of files for `name`, then lists its folders again.
    fn move_files(
        &mut self,
        name: &str,
        work: impl FnOnce() -> Moved + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let key = format!("{name}/files");
        self.busy.insert(key.clone(), "Moving files");
        self.moved = None;
        cx.notify();
        let name = name.to_owned();
        cx.spawn(async move |this, cx| {
            let moved = cx.background_executor().spawn(async move { work() }).await;
            this.update(cx, |home, cx| {
                home.busy.remove(&key);
                home.moved = Some(moved);
                Self::load_files(&name, cx);
            })
            .ok();
        })
        .detach();
    }

    fn take_files(&mut self, name: &str, cx: &mut Context<Self>) {
        let (owner, folder) = (name.to_owned(), self.app_config.received.clone());
        self.move_files(
            name,
            move || take_into(&owner, &folder).unwrap_or_else(|e| Moved::Failed(format!("{e:#}"))),
            cx,
        );
    }

    /// Puts `paths` of this computer into the Inbox of `name`.
    fn send_paths(&mut self, name: &str, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let count = paths.len();
        let request = Request::SendFiles {
            name: name.to_owned(),
            paths,
        };
        self.move_files(
            name,
            move || match client::command(&request) {
                Ok(()) => Moved::Sent(count),
                Err(e) => Moved::Failed(format!("{e:#}")),
            },
            cx,
        );
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

    fn moved_note(&self) -> Option<Div> {
        let note = |color: gpui::Hsla, text: String| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(color.opacity(0.1))
                .border_1()
                .border_color(color.opacity(0.35))
                .text_sm()
                .child(div().text_color(color).child(text))
        };
        Some(match self.moved.as_ref()? {
            Moved::Sent(count) => note(Theme::good(), format!("{count} item(s) are in the Inbox.")),
            Moved::Failed(e) => note(Theme::danger(), e.clone()),
            Moved::Taken {
                archive,
                count,
                refused,
            } => {
                let head = match archive {
                    Some(archive) => format!("{count} file(s) saved to {}", archive.display()),
                    None => "Nothing passed the checks; no archive was written.".to_owned(),
                };
                let mut box_ = note(
                    if refused.is_empty() {
                        Theme::good()
                    } else {
                        Theme::warning()
                    },
                    head,
                );
                for (path, reason) in refused {
                    box_ = box_.child(
                        div()
                            .text_xs()
                            .text_color(Theme::muted())
                            .child(format!("Refused {path}: {reason}")),
                    );
                }
                box_
            }
        })
    }

    fn take_section(
        &self,
        name: &str,
        outbox: &[GuestFile],
        (busy, up): (bool, bool),
        cx: &mut Context<Self>,
    ) -> Div {
        let take = name.to_owned();
        let folder = self.app_config.received.display().to_string();
        let open = self.app_config.received.clone();
        let last = match &self.moved {
            Some(Moved::Taken {
                archive: Some(archive),
                ..
            }) => archive
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section(
                "Take out",
                if up {
                    format!(
                        "What is in the sandbox's Outbox comes to {folder} as one zip. \
                         Programs, scripts and archives stay inside."
                    )
                } else {
                    format!(
                        "What its Outbox held while it ran on the whole screen was taken out \
                         then, and comes to {folder} as one zip. Programs, scripts and archives \
                         stayed inside."
                    )
                },
            ))
            .child(file_list(
                "outbox-files",
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
                    match outbox.len() {
                        0 => "Take files out".to_owned(),
                        1 => "Take 1 file out".to_owned(),
                        n => format!("Take {n} files out"),
                    },
                    Kind::Primary,
                    !busy && !outbox.is_empty(),
                    cx.listener(move |home, _, _, cx| home.take_files(&take, cx)),
                )
                .icon(ui::kit_icon(Icon::Download))
                .w_full(),
            )
            .children(last.map(|archive| {
                div()
                    .flex()
                    .gap_1()
                    .text_xs()
                    .text_color(Theme::good())
                    .child(format!("Last time: {archive} \u{b7}"))
                    .child(
                        div()
                            .id("show-taken")
                            .text_color(Theme::accent())
                            .cursor_pointer()
                            .on_click(move |_, _, _| {
                                fs::create_dir_all(&open).ok();
                                std::process::Command::new("xdg-open")
                                    .arg(&open)
                                    .spawn()
                                    .ok();
                            })
                            .child("Show in folder"),
                    )
            }))
    }

    fn send_section(
        name: &str,
        inbox: &[GuestFile],
        (busy, up): (bool, bool),
        cx: &mut Context<Self>,
    ) -> Div {
        let (send_files, send_folders) = (name.to_owned(), name.to_owned());
        let place = if up {
            "In the Inbox now"
        } else {
            "Waiting for the next start"
        };
        let now = match inbox {
            [] if up => "The Inbox is empty.".to_owned(),
            [] => "Nothing waits to go in.".to_owned(),
            [one] => format!("{place}: {} \u{b7} {}", one.path, size_text(one.size)),
            many => format!("{place}: {} items", many.len()),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section(
                "Send in",
                if up {
                    "Files and folders land in the sandbox's Inbox, where its programs open and \
                     run them."
                } else {
                    "Files and folders wait here and land in the sandbox's Inbox when it next \
                     starts, where its programs open and run them."
                }
                .to_owned(),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2p5()
                    .h(px(150.))
                    .rounded(px(12.))
                    .border_2()
                    .border_dashed()
                    .border_color(Theme::line())
                    .text_sm()
                    .text_color(Theme::secondary_text())
                    .child(ui::icon(Icon::Upload, Theme::secondary_text()).size(px(26.)))
                    .child("Drop files or folders here")
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(button(
                                "send-files",
                                "Choose files",
                                Kind::Secondary,
                                !busy,
                                cx.listener(move |_, _, _, cx| {
                                    Self::choose_and_send(&send_files, false, cx);
                                }),
                            ))
                            .child(button(
                                "send-folders",
                                "Choose a folder",
                                Kind::Secondary,
                                !busy,
                                cx.listener(move |_, _, _, cx| {
                                    Self::choose_and_send(&send_folders, true, cx);
                                }),
                            )),
                    ),
            )
            .child(div().text_sm().text_color(Theme::muted()).child(now))
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

    fn files_list(&self, name: &str, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let busy = self.busy.contains_key(&format!("{name}/files"));
        let files = self
            .files
            .as_ref()
            .filter(|(n, _)| n == name)
            .map(|(_, f)| f);
        let (inbox, outbox, problem) = match files {
            Some(Ok(GuestFiles { inbox, outbox })) => (inbox.as_slice(), outbox.as_slice(), None),
            Some(Err(e)) => (&[][..], &[][..], Some(e.clone())),
            None => (&[][..], &[][..], None),
        };
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
            .gap_5()
            .px_6()
            .py_5()
            .overflow_y_scroll()
            .track_scroll(&self.files_scroll)
            .on_drop(
                cx.listener(move |home, paths: &gpui::ExternalPaths, _, cx| {
                    home.send_paths(&drop_to, paths.paths().to_vec(), cx);
                }),
            )
            .children(problem.map(|p| div().text_sm().text_color(Theme::warning()).child(p)))
            .child(self.take_section(name, outbox, (busy, up), cx))
            .child(Self::send_section(name, inbox, (busy, up), cx))
            .when(busy, |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(Theme::accent())
                        .child("Moving files..."),
                )
            })
            .children(self.moved_note())
    }

    /// App settings, General: the look, where taken files go, the tour,
    /// and removing GSE.
    pub(crate) fn general_view(&self, cx: &mut Context<Self>) -> Div {
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
        let looks = ui::segments()
            .child(choice("look-system", "System", Look::System, cx))
            .child(choice("look-light", "Light", Look::Light, cx))
            .child(choice("look-dark", "Dark", Look::Dark, cx));
        let folder = self.app_config.received.display().to_string();
        let rows = group(
            None,
            vec![
                row(
                    "Look",
                    Some("Follows the system unless you pick one.".to_owned()),
                    looks,
                ),
                row(
                    "Files taken out go to",
                    Some(folder),
                    button(
                        "received-choose",
                        "Change",
                        Kind::Secondary,
                        true,
                        cx.listener(|_, _, _, cx| Self::choose_received(cx)),
                    ),
                ),
                row(
                    "Show the welcome tour again",
                    Some("From the first step, any time.".to_owned()),
                    button(
                        "start-tour",
                        "Start tour",
                        Kind::Secondary,
                        true,
                        cx.listener(|home, _, _, cx| home.start_tour(cx)),
                    ),
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
                        .font_weight(FontWeight::BOLD)
                        .text_color(Theme::danger_text())
                        .child("Remove GSE"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_5()
                        .px_4()
                        .py_4()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(Theme::danger().opacity(0.25))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
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
                                Kind::Danger,
                                true,
                                cx.listener(|home, _, _, cx| home.open_remove(cx)),
                            )
                            .flex_none(),
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

    /// Opens the question before removing everything, and measures what
    /// would go.
    pub(crate) fn open_remove(&mut self, cx: &mut Context<Self>) {
        let confirm = cx.new(|cx| TextInput::new("remove", cx));
        cx.observe(&confirm, |_, _, cx| cx.notify()).detach();
        self.modal = Some(Modal::RemoveAll {
            keep: true,
            confirm,
            sizes: None,
        });
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

    /// The choice to keep the taken files when removing everything.
    fn keep_box(keep: bool, detail: String, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_start()
            .gap_2p5()
            .px_3p5()
            .py_3()
            .rounded(px(10.))
            .bg(Theme::well())
            .child(
                gpui_kit::component::checkbox::Checkbox::new("remove-keep")
                    .checked(keep)
                    .on_click(cx.listener(|home, on: &bool, _, cx| {
                        if let Some(Modal::RemoveAll { keep, .. }) = home.modal.as_mut() {
                            *keep = *on;
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_sm().child("Keep files taken from sandboxes"))
                    .child(div().text_xs().text_color(Theme::muted()).child(detail)),
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
        let item = |text: String| {
            div()
                .flex()
                .gap_2()
                .text_sm()
                .text_color(Theme::secondary_text())
                .child("\u{2022}")
                .child(text)
        };
        let folder = self.app_config.received.display().to_string();
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(Theme::secondary_text())
                    .child("This stops every sandbox and deletes:"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(item(sandboxes))
                    .child(item(sets))
                    .child(item(
                        "The background service and its system settings".to_owned(),
                    )),
            )
            .child(Self::keep_box(keep, format!("{folder}{received}"), cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_sm()
                            .text_color(Theme::secondary_text())
                            .child("Type remove to confirm"),
                    )
                    .child(confirm.clone()),
            )
            .child(div().text_xs().text_color(Theme::muted()).child(
                "Your system asks for the administrator password. The app quits when it is done.",
            ));
        let buttons = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2p5()
            .child(button(
                "remove-cancel",
                "Cancel",
                Kind::Secondary,
                !removing,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    cx.notify();
                }),
            ))
            .child(
                button(
                    "remove-go",
                    if removing {
                        "Removing..."
                    } else {
                        "Remove everything"
                    },
                    Kind::Danger,
                    typed && !removing,
                    cx.listener(move |home, _, _, cx| home.remove_everything(!keep, cx)),
                )
                .bg(Theme::danger())
                .text_color(Theme::on_danger()),
            );
        crate::sandbox::dialog("Remove GSE from this computer?", body, buttons)
    }

    /// Runs the service's uninstall through polkit, removes the app's own
    /// files, and the received files folder with `received`, and closes the
    /// app.
    fn remove_everything(&mut self, received: bool, cx: &mut Context<Self>) {
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
    /// What goes and what stays, in words, measured or not yet.
    fn lines(sizes: Option<&Self>) -> (String, String, String) {
        let count = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        sizes.map_or_else(
            || {
                (
                    "Sandboxes with everything inside them".to_owned(),
                    "Sets of apps and the sandbox system".to_owned(),
                    String::new(),
                )
            },
            |s| {
                (
                    format!(
                        "{} with everything inside them \u{b7} {}",
                        count(s.sandboxes, "sandbox", "sandboxes"),
                        size_text(s.sandbox_bytes)
                    ),
                    format!(
                        "{} and the sandbox system \u{b7} {}",
                        count(s.sets, "set of apps", "sets of apps"),
                        size_text(s.set_bytes)
                    ),
                    format!(" \u{b7} {} stays where it is", size_text(s.received_bytes)),
                )
            },
        )
    }

    fn measure(received: &Path) -> Self {
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
