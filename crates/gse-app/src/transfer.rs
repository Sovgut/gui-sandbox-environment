//! Moving files in and out of a running sandbox from one dialog. Files and
//! folders of this computer go into `guest`'s `~/Inbox` as they are.
//! Everything in `guest`'s `~/Outbox` that passes the service's checks comes
//! out as one zip archive in the received files folder of app settings. The
//! app writes the archive itself, as the user, and never opens what it
//! holds.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use chrono::Local;
use gpui::{Context, Div, FontWeight, PathPromptOptions, div, prelude::*, px};
use gse_protocol::{GuestFile, GuestFiles, Request};
use zip::write::SimpleFileOptions;

use crate::client::{self, Taking};
use crate::icons::Icon;
use crate::programs::size_text;
use crate::sandbox::dialog;
use crate::ui::{self, Kind, Theme, button, icon};
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
        .gap_0p5()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .child(div().text_xs().text_color(Theme::muted()).child(text))
}

fn file_list(id: &'static str, files: &[GuestFile], empty: &'static str) -> impl IntoElement {
    let mut list = div()
        .id(id)
        .flex()
        .flex_col()
        .max_h(px(150.))
        .overflow_y_scroll()
        .rounded_md()
        .bg(Theme::background())
        .border_1()
        .border_color(Theme::border())
        .text_sm();
    if files.is_empty() {
        return list.child(div().p_2().text_color(Theme::subtle()).child(empty));
    }
    for file in files {
        list = list.child(
            div()
                .flex()
                .gap_3()
                .px_2()
                .py_0p5()
                .child(div().flex_1().min_w_0().truncate().child(file.path.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
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
            this.update(cx, |home, cx| {
                let count = paths.len();
                let request = Request::SendFiles {
                    name: name.clone(),
                    paths,
                };
                home.move_files(
                    &name,
                    move || match client::command(&request) {
                        Ok(()) => Moved::Sent(count),
                        Err(e) => Moved::Failed(format!("{e:#}")),
                    },
                    cx,
                );
            })
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
        busy: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let take = name.to_owned();
        let folder = self.app_config.received.display().to_string();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section(
                "Take files out",
                format!(
                    "What guest put into ~/Outbox (Outbox on the sandbox's desktop) and \
                         what passes the checks: no programs, scripts, shortcuts or \
                         archives. It goes into one zip archive in {folder}."
                ),
            ))
            .child(file_list("outbox-files", outbox, "The Outbox is empty."))
            .child(
                div().flex().child(
                    button(
                        "take-files",
                        match outbox.len() {
                            0 => "Take files".to_owned(),
                            n => format!("Take {n} file(s)"),
                        },
                        Kind::Primary,
                        !busy && !outbox.is_empty(),
                        cx.listener(move |home, _, _, cx| home.take_files(&take, cx)),
                    )
                    .gap_2()
                    .child(icon(Icon::Download, Theme::on_accent())),
                ),
            )
    }

    fn send_section(name: &str, inbox: &[GuestFile], busy: bool, cx: &mut Context<Self>) -> Div {
        let (send_files, send_folders) = (name.to_owned(), name.to_owned());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section(
                "Send files in",
                "Files and folders of this computer go into guest's ~/Inbox \
                     (Inbox on the sandbox's desktop) as they are. They open and run \
                     from there, as guest; the sandbox's own desktop stays out of \
                     guest's reach."
                    .to_owned(),
            ))
            .child(file_list("inbox-files", inbox, "The Inbox is empty."))
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
            )
    }

    pub(crate) fn transfer_dialog(&self, name: &str, cx: &mut Context<Self>) -> Div {
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
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .children(problem.map(|p| div().text_sm().text_color(Theme::warning()).child(p)))
            .child(self.take_section(name, outbox, busy, cx))
            .child(Self::send_section(name, inbox, busy, cx))
            .when(busy, |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(Theme::accent())
                        .child("Moving files..."),
                )
            })
            .children(self.moved_note());
        let open = self.app_config.received.clone();
        let buttons = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(button(
                "open-received",
                "Open the folder",
                Kind::Ghost,
                true,
                move |_, _, _| {
                    fs::create_dir_all(&open).ok();
                    std::process::Command::new("xdg-open")
                        .arg(&open)
                        .spawn()
                        .ok();
                },
            ))
            .child(button(
                "transfer-close",
                "Close",
                Kind::Secondary,
                true,
                cx.listener(|home, _, _, cx| {
                    home.modal = None;
                    cx.notify();
                }),
            ));
        dialog(format!("{name}: files"), body, buttons)
    }

    /// App settings: where taken files go, and removing everything.
    pub(crate) fn general_view(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.updates_card(cx))
            .child(self.received_card(cx))
            .child(self.remove_card(cx))
    }

    fn received_card(&self, cx: &mut Context<Self>) -> Div {
        let saved = self.app_config.received.display().to_string();
        let typed = self.received_input.read(cx).text().trim().to_owned();
        let dirty = !typed.is_empty() && typed != saved;
        ui::card(
            "Received files",
            Some("The folder the zip archives of files taken out of a sandbox go to."),
        )
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(self.received_input.clone()))
                .child(button(
                    "received-choose",
                    "Choose",
                    Kind::Secondary,
                    true,
                    cx.listener(|_, _, _, cx| Self::choose_received(cx)),
                ))
                .child(button(
                    "received-save",
                    "Save",
                    Kind::Primary,
                    dirty,
                    cx.listener(move |home, _, _, cx| {
                        home.app_config.received = PathBuf::from(&typed);
                        match home.app_config.save() {
                            Ok(()) => home.say("Taken files go to the new folder", false),
                            Err(e) => home.say(format!("{e:#}"), true),
                        }
                        cx.notify();
                    }),
                )),
        )
    }

    fn remove_card(&self, cx: &mut Context<Self>) -> Div {
        let received = self.app_config.received.display().to_string();
        let also = self.remove_received;
        let mut card = ui::card(
            "Remove everything",
            Some(
                "Stops any sandbox and removes everything gse put on this computer: the \
                 service, its settings, every image and sandbox with all that is inside, \
                 the network rules and account, and this app's own settings. Your \
                 password is asked for once. The app closes afterwards; delete the \
                 AppImage file yourself.",
            ),
        )
        .child(
            div().flex().child(
                ui::segments()
                    .child(ui::segment(
                        "remove-keep-received",
                        format!("Keep {received}"),
                        !also,
                        true,
                        cx.listener(|home, _, _, cx| {
                            home.remove_received = false;
                            cx.notify();
                        }),
                    ))
                    .child(ui::segment(
                        "remove-received",
                        format!("Delete {received} too"),
                        also,
                        true,
                        cx.listener(|home, _, _, cx| {
                            home.remove_received = true;
                            cx.notify();
                        }),
                    )),
            ),
        );
        let removing = self.busy.contains_key("remove-everything");
        let actions = div().flex().items_center().gap_2();
        card = card.child(if self.remove_confirm {
            actions
                .child(
                    div()
                        .text_sm()
                        .text_color(Theme::danger())
                        .child("This cannot be undone. Remove everything?"),
                )
                .child(button(
                    "remove-cancel",
                    "Cancel",
                    Kind::Ghost,
                    !removing,
                    cx.listener(|home, _, _, cx| {
                        home.remove_confirm = false;
                        cx.notify();
                    }),
                ))
                .child(button(
                    "remove-go",
                    if removing {
                        "Removing..."
                    } else {
                        "Remove everything"
                    },
                    Kind::Danger,
                    !removing,
                    cx.listener(|home, _, _, cx| home.remove_everything(cx)),
                ))
        } else {
            actions.child(button(
                "remove-ask",
                "Remove everything...",
                Kind::Danger,
                true,
                cx.listener(|home, _, _, cx| {
                    home.remove_confirm = true;
                    cx.notify();
                }),
            ))
        });
        card
    }

    /// Runs the service's uninstall through polkit, removes the app's own
    /// files and closes the app.
    fn remove_everything(&mut self, cx: &mut Context<Self>) {
        self.busy.insert("remove-everything".into(), "Removing");
        cx.notify();
        let received = self
            .remove_received
            .then(|| self.app_config.received.clone());
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
                        home.remove_confirm = false;
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
                    home.received_input.update(cx, |input, cx| {
                        input.set_text(path.display().to_string(), cx);
                    });
                }
            })
            .ok();
        })
        .detach();
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
