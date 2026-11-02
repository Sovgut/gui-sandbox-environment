//! Every state a preview can show. Each starts from the made-up machine
//! of [`super::data`] with `demo` selected and stopped, and changes what
//! the state is about.

use gpui::{Context, Entity};
use gse_protocol::{BaseImage, Build, Environment, GuestFiles, Preset, Stage};

use super::data;
use crate::devservice::Install;
use crate::journal::Period;
use crate::onboarding::Tour;
use crate::presets::{Confirm as SetConfirm, SourceKind};
use crate::setup::{Setup, Step};
use crate::shell::PanelTab;
use crate::text_input::TextInput;
use crate::transfer::Moved;
use crate::update::{Release, State as Update};
use crate::{AppTab, Confirm, Home, Modal, Page, SortBy, Toast, client};

/// A state: its name for `GSE_PREVIEW`, what it shows, the switches of the
/// made-up machine it sets (see [`super::flag`]) and how it gets there.
pub struct State {
    pub name: &'static str,
    pub what: &'static str,
    pub flags: &'static [&'static str],
    pub show: fn(&mut Home, &mut Context<Home>),
}

type Show = fn(&mut Home, &mut Context<Home>);

const fn state(name: &'static str, what: &'static str, show: Show) -> State {
    State {
        name,
        what,
        flags: &[],
        show,
    }
}

const fn flagged(
    name: &'static str,
    what: &'static str,
    flags: &'static [&'static str],
    show: Show,
) -> State {
    State {
        name,
        what,
        flags,
        show,
    }
}

pub const STATES: &[State] = &[
    // The whole window.
    state(
        "main",
        "a stopped sandbox, Activity tab, while another one's set builds",
        |home, _| {
            presets(home, Some(data::wow_build()), true, Vec::new());
        },
    ),
    state("rail", "the sidebar folded to covers", |home, _| {
        home.sidebar_collapsed = true;
    }),
    flagged(
        "fullscreen",
        "the window in full screen",
        &["fullscreen"],
        |_, _| {},
    ),
    state("loading", "before the service first answers", loading),
    state("empty", "no sandboxes yet", |home, _| {
        home.environments.clear();
        home.selected = None;
        home.journal = None;
    }),
    state("search", "the sandbox list searched", |home, cx| {
        type_into(&home.sandbox_search.clone(), "st", cx);
    }),
    state("sorted", "the sandbox list by name", |home, _| {
        home.sort = SortBy::Name;
    }),
    state(
        "stages",
        "a sandbox running while another waits for its set",
        |home, _| {
            run(home, data::DEMO, Stage::Ready);
            presets(home, Some(data::wow_build()), true, Vec::new());
        },
    ),
    // A sandbox's cover and facts.
    state(
        "not-built",
        "a sandbox whose set is not built",
        |home, _| {
            select(home, "wow");
        },
    ),
    state(
        "waiting",
        "a sandbox waiting for its set to build",
        |home, _| {
            select(home, "wow");
            presets(home, Some(data::wow_build()), true, Vec::new());
        },
    ),
    state("starting", "a sandbox booting", |home, _| {
        run(home, data::DEMO, Stage::Booting);
    }),
    state(
        "starting-gpu",
        "a sandbox handing over the graphics card",
        |home, _| run(home, data::DEMO, Stage::SwitchingGpu),
    ),
    state(
        "starting-busy",
        "Play pressed, before the service answers",
        |home, _| {
            home.busy.insert(data::DEMO.into(), "Starting");
        },
    ),
    state("running", "a running sandbox's page", running_demo),
    state(
        "own-monitor",
        "a sandbox running on a monitor of its own",
        |home, cx| {
            running_demo(home, cx);
            if let Some(running) = environment(home, data::DEMO).running.as_mut() {
                running.screen = None;
            }
        },
    ),
    state("stopping", "a sandbox stopping", |home, _| {
        run(home, data::DEMO, Stage::Stopping);
    }),
    state(
        "stopping-busy",
        "Stop pressed, before the service answers",
        |home, cx| {
            running_demo(home, cx);
            home.busy.insert("demo/stop".into(), "Stopping");
        },
    ),
    state(
        "single-gpu-page",
        "a sandbox that plays on the only graphics card",
        |home, _| one_gpu(home),
    ),
    // The tabs under the cover.
    flagged(
        "activity",
        "the Activity tab of a running sandbox, filtered to today",
        &["strip"],
        activity,
    ),
    state("activity-bad", "a filter that is no regex", |home, cx| {
        type_into(&home.journal_filter.clone(), "refus(", cx);
    }),
    flagged(
        "activity-period",
        "the period menu open",
        &["strip"],
        |home, cx| {
            activity(home, cx);
            home.period_open = true;
        },
    ),
    state(
        "journal-empty",
        "a sandbox with nothing in its journal",
        |home, _| {
            home.journal = Some((data::DEMO.into(), Vec::new()));
        },
    ),
    state("journal-loading", "the journal not read yet", |home, _| {
        home.journal = None;
    }),
    flagged(
        "programs",
        "the Programs tab of a running sandbox",
        &["strip"],
        programs,
    ),
    state(
        "programs-stopped",
        "the Programs tab of a stopped sandbox",
        |home, _| {
            home.tab = PanelTab::Programs;
        },
    ),
    flagged(
        "programs-empty",
        "a sandbox running no programs",
        &["strip"],
        |home, cx| {
            programs(home, cx);
            home.processes = Some((data::DEMO.into(), Ok(Vec::new())));
        },
    ),
    flagged(
        "programs-error",
        "the programs not read",
        &["strip"],
        |home, cx| {
            programs(home, cx);
            home.processes = Some((
                data::DEMO.into(),
                Err("the sandbox did not answer within 5 seconds".into()),
            ));
        },
    ),
    flagged(
        "programs-loading",
        "the programs being read",
        &["strip"],
        |home, cx| {
            programs(home, cx);
            home.processes = None;
            home.processes_loading = true;
        },
    ),
    flagged(
        "programs-search",
        "the programs searched",
        &["strip"],
        |home, cx| {
            programs(home, cx);
            type_into(&home.processes_filter.clone(), "steam", cx);
        },
    ),
    // A sandbox's screen in the window.
    state("screen", "a running sandbox's screen in the window", screen),
    flagged(
        "screen-captured",
        "the screen holding the mouse and keyboard",
        &["captured"],
        screen,
    ),
    flagged(
        "screen-fullscreen",
        "the screen with the window in full screen",
        &["fullscreen"],
        screen,
    ),
    state(
        "screen-sidebar",
        "the screen beside the whole sidebar",
        |home, cx| {
            screen(home, cx);
            home.sidebar_collapsed = false;
        },
    ),
    state(
        "screen-stopping",
        "the screen after Stop was pressed",
        |home, cx| {
            screen(home, cx);
            home.busy.insert("demo/stop".into(), "Stopping");
        },
    ),
    state(
        "close",
        "closing the window while a sandbox runs",
        |home, cx| {
            screen(home, cx);
            home.modal = Some(Modal::Close);
        },
    ),
    // Files in and out.
    state(
        "files",
        "the Files panel over a screen, files taken out an hour ago",
        |home, cx| {
            files(home, cx);
            home.last_taken.insert(
                data::DEMO.into(),
                "/home/me/GSE/demo 2026-11-01 18.45.50.zip".into(),
            );
        },
    ),
    state(
        "files-idle",
        "the Files panel over a screen, nothing taken out yet",
        files,
    ),
    state(
        "files-taken",
        "the Files panel over a screen, the Outbox just taken",
        |home, cx| {
            files(home, cx);
            let archive = "/home/me/GSE/demo 2026-11-01 19.29.12.zip";
            home.last_taken.insert(data::DEMO.into(), archive.into());
            home.moved = Some(Moved::Taken {
                archive: Some(archive.into()),
                count: 2,
                refused: vec![("trainer.exe".into(), "a Windows program".into())],
            });
            if let Some((_, Ok(files))) = home.files.as_mut() {
                files.outbox.retain(|f| f.path == "trainer.exe");
            }
        },
    ),
    state(
        "files-stopped",
        "the Files panel of a stopped sandbox",
        |home, _| {
            home.files = Some((data::DEMO.into(), Ok(data::files_between_runs())));
            home.modal = Some(Modal::Transfer {
                name: data::DEMO.into(),
            });
        },
    ),
    state("files-empty", "an empty Inbox and Outbox", |home, cx| {
        files(home, cx);
        home.files = Some((
            data::DEMO.into(),
            Ok(GuestFiles {
                inbox: Vec::new(),
                outbox: Vec::new(),
            }),
        ));
    }),
    state("files-loading", "the folders being listed", |home, cx| {
        files(home, cx);
        home.files = None;
    }),
    state("files-error", "the folders not listed", |home, cx| {
        files(home, cx);
        home.files = Some((
            data::DEMO.into(),
            Err("the sandbox did not answer within 5 seconds".into()),
        ));
    }),
    state("files-busy", "files being taken out", |home, cx| {
        files(home, cx);
        home.busy
            .insert("demo/files".into(), crate::transfer::TAKING);
    }),
    state("files-sent", "files just sent in", |home, cx| {
        files(home, cx);
        home.moved = Some(Moved::Sent(2));
        home.files = Some((data::DEMO.into(), Ok(data::files_between_runs())));
    }),
    state("files-failed", "taking files out failed", |home, cx| {
        files(home, cx);
        home.moved = Some(Moved::TakeFailed("No space left on device".into()));
    }),
    // New sandbox.
    state(
        "new-sandbox",
        "the New sandbox dialog, no name yet",
        |home, cx| new_sandbox(home, true, cx),
    ),
    state(
        "new-sandbox-name",
        "the New sandbox dialog, named",
        |home, cx| {
            new_sandbox(home, true, cx);
            add_name(home, "games", cx);
        },
    ),
    state(
        "new-sandbox-exists",
        "the New sandbox dialog, a name that is taken",
        |home, cx| {
            new_sandbox(home, true, cx);
            add_name(home, data::DEMO, cx);
        },
    ),
    state(
        "new-sandbox-unbuilt",
        "the New sandbox dialog, a set not built",
        |home, cx| {
            new_sandbox(home, false, cx);
            if let Some(Modal::Add { preset, .. }) = home.modal.as_mut() {
                *preset = Some("wow".into());
            }
            add_name(home, "wow-2", cx);
        },
    ),
    state(
        "new-sandbox-details",
        "the New sandbox dialog with its settings unfolded",
        |home, cx| {
            new_sandbox(home, true, cx);
            if let Some(Modal::Add { details, .. }) = home.modal.as_mut() {
                *details = true;
            }
        },
    ),
    state(
        "new-sandbox-nosets",
        "New sandbox pressed with no sets",
        |home, cx| {
            home.load_presets(client::PresetsState {
                presets: Vec::new(),
                build: None,
                base: Some(data::base(true)),
                stale: Vec::new(),
                failed: Vec::new(),
            });
            home.images.clear();
            home.open_add(cx);
        },
    ),
    // A sandbox's settings.
    state(
        "sandbox-settings",
        "a running sandbox's settings",
        |home, cx| {
            running_demo(home, cx);
            home.open_sandbox_settings(data::DEMO, cx);
        },
    ),
    state(
        "sandbox-settings-stopped",
        "a stopped sandbox's settings",
        |home, cx| home.open_sandbox_settings(data::DEMO, cx),
    ),
    state(
        "sandbox-settings-rename",
        "a sandbox renamed",
        |home, cx| {
            home.open_sandbox_settings(data::DEMO, cx);
            if let Some(Modal::SandboxSettings { rename, .. }) = &home.modal {
                type_into(&rename.clone(), "demo-old", cx);
            }
        },
    ),
    state(
        "sandbox-settings-single",
        "a sandbox's settings set to the only graphics card",
        |home, cx| {
            one_gpu(home);
            home.open_sandbox_settings(data::DEMO, cx);
        },
    ),
    state(
        "sandbox-settings-menu",
        "a sandbox's settings with the GPU menu open",
        |home, cx| {
            home.open_sandbox_settings(data::DEMO, cx);
            home.open_menu = Some("sandbox-gpu".into());
        },
    ),
    state(
        "sandbox-settings-menu-same",
        "the GPU menu with two cards of one model",
        |home, cx| {
            home.settings = Ok((data::settings(data::SPARE_GPU), data::same_model_gpus()));
            home.open_sandbox_settings(data::DEMO, cx);
            home.open_menu = Some("sandbox-gpu".into());
        },
    ),
    state(
        "confirm-reset",
        "a reset waiting for the second click",
        |home, cx| {
            home.open_sandbox_settings(data::DEMO, cx);
            home.confirm = Some((data::DEMO.into(), Confirm::Reset));
        },
    ),
    state(
        "confirm-delete",
        "a delete waiting for the second click",
        |home, cx| {
            home.open_sandbox_settings(data::DEMO, cx);
            home.confirm = Some((data::DEMO.into(), Confirm::Delete));
        },
    ),
    // Dialogs about playing.
    state(
        "single-gpu",
        "playing on the only graphics card",
        |home, _| {
            one_gpu(home);
            home.modal = Some(Modal::SingleGpu {
                name: data::DEMO.into(),
                explained: false,
            });
        },
    ),
    state(
        "monitor-gpu",
        "playing on the card that drives the monitor, another one free, ticked not to explain \
         again",
        |home, _| {
            environment(home, data::DEMO).settings.gpu = data::MONITOR_GPU.into();
            home.modal = Some(Modal::SingleGpu {
                name: data::DEMO.into(),
                explained: true,
            });
        },
    ),
    state("switch", "playing while another sandbox runs", |home, _| {
        run(home, data::DEMO, Stage::Ready);
        environment(home, "wow").image_ready = true;
        home.images.push("wow".into());
        home.modal = Some(Modal::Switch {
            from: data::DEMO.into(),
            to: "wow".into(),
        });
    }),
    // Banners and messages.
    state(
        "notices",
        "a newer release, the service to update, files taken out and files kept in",
        |home, _| {
            home.update = Update::Available(about_release());
            home.service = client::Match::ServiceOlder;
            home.tell(taken());
            home.tell(
                Toast::error(
                    "2 files stayed in the sandbox",
                    "setup.exe is a Windows program; run.sh is an executable script. \
                     Programs can't leave a sandbox.",
                )
                .about(data::DEMO),
            );
        },
    ),
    state("toast", "a result, which goes by itself", |home, _| {
        home.tell(taken());
    }),
    state(
        "error",
        "an error from the last request, which waits for a click",
        |home, _| {
            home.tell(
                Toast::error(
                    "Starting demo failed",
                    "The GPU 0000:0e:00.0 is in use by another program",
                )
                .about(data::DEMO),
            );
        },
    ),
    state("outdated", "the sandbox system out of date", |home, _| {
        presets(home, None, false, vec!["launchers".into()]);
    }),
    state("outdated-sets", "a set out of date", |home, _| {
        presets(home, None, true, vec!["launchers".into()]);
    }),
    state(
        "building",
        "the sandbox system building, seen from a sandbox's page",
        |home, _| {
            // The board's one sandbox, made from the launchers.
            home.environments = vec![data::environment(
                "games",
                "launchers",
                Some(super::ago(60 * 26)),
            )];
            select(home, "games");
            home.images = vec!["launchers".into()];
            home.load_presets(client::PresetsState {
                presets: vec![data::system_board_launchers()],
                build: Some(data::system_build()),
                base: Some(data::base(false)),
                stale: vec!["launchers".into()],
                failed: Vec::new(),
            });
        },
    ),
    state(
        "service-newer",
        "a service newer than the app",
        |home, _| {
            home.service = client::Match::ServiceNewer;
        },
    ),
    state(
        "service-newer-release",
        "a service newer than the app, and the app's newer release found",
        |home, _| {
            home.service = client::Match::ServiceNewer;
            home.update = Update::Available(about_release());
        },
    ),
    state(
        "service-update",
        "a service older than the app, which carries a newer one",
        |home, _| home.service = client::Match::ServiceOlder,
    ),
    flagged(
        "service-restart",
        "a development service older than the app",
        &["service-kept"],
        |home, _| home.service = client::Match::ServiceOlder,
    ),
    state(
        "service-updating",
        "the service being updated",
        |home, _| {
            home.service = client::Match::ServiceOlder;
            home.restarting = true;
        },
    ),
    flagged(
        "gpu-restart",
        "GPUs changed since the app started",
        &["gpu-moved"],
        |_, _| {},
    ),
    state("update-banner", "a newer release of the app", |home, _| {
        home.update = Update::Available(about_release());
    }),
    state(
        "update-installing",
        "a newer release installing",
        |home, _| {
            home.update = Update::Installing(about_release());
        },
    ),
    // Preparing the computer.
    state(
        "setup",
        "first start: downloading the sandbox system",
        setup_download,
    ),
    state(
        "setup-details",
        "first start: the download with its details open",
        |home, cx| {
            setup_download(home, cx);
            if let Some(setup) = home.setup.as_mut() {
                setup.details = true;
            }
        },
    ),
    state("setup-idle", "first start, before Start", |home, _| {
        setup(home, Step::Check, false, None, None);
    }),
    state(
        "setup-packages",
        "first start: installing what sandboxes run on",
        |home, _| {
            setup(
                home,
                Step::Check,
                true,
                None,
                Some("Installing what sandboxes run on: QEMU, the UEFI firmware, passt"),
            );
            if let Some(setup) = home.setup.as_mut() {
                setup.log = vec![(Step::Check, "step packages".into())];
            }
        },
    ),
    state(
        "setup-service",
        "first start: installing the service",
        |home, _| {
            setup(home, Step::Service, true, None, Some("Copying the service"));
        },
    ),
    state(
        "setup-launchers",
        "first start: building the first set",
        |home, _| {
            setup(home, Step::Launchers, true, None, None);
            home.presets.build = Some(Build {
                preset: "launchers".into(),
                steps: vec!["prepare".into(), "packages".into()],
                started: None,
            });
        },
    ),
    state("setup-failed", "first start: a build failed", |home, _| {
        setup(
            home,
            Step::System,
            false,
            Some("listing the cloud images of Fedora 44: status code 403"),
            None,
        );
    }),
    state(
        "setup-failed-install",
        "first start: installing packages failed",
        |home, _| {
            setup(
                home,
                Step::Check,
                false,
                Some("dnf exited with status 1: No match for argument: edk2-ovmf"),
                None,
            );
        },
    ),
    state("setup-warnings", "first start with findings", |home, _| {
        setup(home, Step::Service, false, None, None);
        if let Some(setup) = home.setup.as_mut() {
            setup.warnings = vec![
                "The GPU shares its IOMMU group with its audio function; both go to the \
                 sandbox"
                    .into(),
            ];
        }
    }),
    // The welcome tour.
    state("tour-welcome", "welcome tour, step 1", |home, _| {
        // Next leads to the second step as its board shows it.
        tour_sets(home);
        home.images = vec!["launchers".into()];
        home.tour = Some(Tour::Welcome);
    }),
    state(
        "tour-first",
        "welcome tour, step 2: the first sandbox",
        |home, cx| tour_first(home, Some("launchers"), "games", cx),
    ),
    state(
        "tour-first-empty",
        "welcome tour, step 2, nothing picked",
        |home, cx| tour_first(home, None, "", cx),
    ),
    state(
        "tour-first-taken",
        "welcome tour, step 2, a name that is taken",
        |home, cx| tour_first(home, Some("launchers"), data::DEMO, cx),
    ),
    state(
        "tour-first-building",
        "welcome tour, step 2, the picked set building",
        |home, cx| {
            tour_first(home, Some("desktop"), "work", cx);
            home.presets.build = Some(Build {
                preset: "desktop".into(),
                steps: vec!["prepare".into(), "packages".into()],
                started: None,
            });
        },
    ),
    state(
        "tour-play",
        "welcome tour, step 3: press Play",
        |home, _| {
            let mut games = data::environment("games", "launchers", None);
            games.created = data::at(19, 29);
            tour_sets(home);
            home.environments = vec![games];
            home.selected = Some("games".into());
            home.journal = Some((
                "games".into(),
                vec![gse_protocol::JournalEntry {
                    time: data::at(19, 29),
                    text: "Created from the set of apps launchers".into(),
                }],
            ));
            home.tour = Some(Tour::PressPlay);
        },
    ),
    // Settings: General.
    state("settings", "settings: General", |home, _| {
        settings(home, AppTab::General);
        home.app_config.look = crate::config::Look::System;
    }),
    // Settings: Sets of apps.
    state(
        "sets",
        "settings: Sets of apps, one building, one failed",
        |home, _| {
            settings(home, AppTab::Presets);
            board_sets(
                home,
                Some(data::wow_build()),
                true,
                vec!["launchers".into()],
            );
        },
    ),
    state(
        "sets-idle",
        "settings: Sets of apps, nothing building",
        |home, _| {
            settings(home, AppTab::Presets);
            board_sets(home, None, true, Vec::new());
        },
    ),
    state(
        "sets-rebuilding",
        "a built set building again",
        |home, _| {
            settings(home, AppTab::Presets);
            board_sets(
                home,
                Some(Build {
                    preset: "launchers".into(),
                    steps: vec!["prepare".into(), "packages".into()],
                    started: None,
                }),
                true,
                Vec::new(),
            );
        },
    ),
    state(
        "sets-rebuild",
        "a rebuild waiting for the second click",
        |home, _| {
            settings(home, AppTab::Presets);
            board_sets(home, None, true, Vec::new());
            home.presets.confirm = Some(("launchers".into(), SetConfirm::Build));
        },
    ),
    state("sets-stopping", "a build being stopped", |home, _| {
        settings(home, AppTab::Presets);
        board_sets(
            home,
            Some(data::wow_build()),
            true,
            vec!["launchers".into()],
        );
        home.presets.stopping = Some("wow".into());
    }),
    state(
        "sets-running",
        "the sets while a sandbox runs",
        |home, _| {
            settings(home, AppTab::Presets);
            board_sets(home, None, true, Vec::new());
            run(home, data::DEMO, Stage::Ready);
        },
    ),
    state(
        "sets-outdated",
        "the sets on an outdated system",
        |home, _| {
            settings(home, AppTab::Presets);
            board_sets(home, None, false, vec!["launchers".into()]);
        },
    ),
    state(
        "system-build",
        "settings: Sets of apps while the sandbox system builds",
        |home, _| {
            settings(home, AppTab::Presets);
            home.environments.retain(|e| e.name == data::DEMO);
            home.images = vec!["launchers".into()];
            home.load_presets(client::PresetsState {
                presets: vec![data::system_board_launchers()],
                build: Some(data::system_build()),
                base: Some(data::base(false)),
                stale: vec!["launchers".into()],
                failed: Vec::new(),
            });
        },
    ),
    state("sets-empty", "no sets of apps", |home, _| {
        settings(home, AppTab::Presets);
        board_sets(home, None, true, Vec::new());
        home.presets.list = Ok(Vec::new());
    }),
    state("sets-error", "the sets not read", |home, _| {
        settings(home, AppTab::Presets);
        home.presets.list = Err("connecting to the gse service: No such file or directory".into());
    }),
    flagged(
        "sets-old-service",
        "a development service that knows no sets",
        &["service-kept"],
        |home, _| {
            settings(home, AppTab::Presets);
            home.presets.list = Err("The running gse service does not know sets of apps yet. \
             Restart the service to see and edit them here."
                .into());
            home.presets.old_service = true;
        },
    ),
    state(
        "new-set",
        "settings: the recommended sets to start a new set from",
        |home, cx| {
            settings(home, AppTab::Presets);
            home.presets.templates = vec![data::launchers(), data::desktop_with_firefox()];
            home.new_set(cx);
        },
    ),
    state("set-editor", "settings: editing a set", |home, cx| {
        edit(home, Some(&data::wow_addons()), cx);
        if let Some(editor) = home.presets.editor.as_mut() {
            editor.searched = "lutr".into();
            editor.found = Ok(data::flathub("lutr"));
            let search = editor.flathub_id.clone();
            type_into(&search, "lutr", cx);
        }
    }),
    state("set-new", "a new set, empty", |home, cx| {
        edit(home, None, cx);
    }),
    state("set-flathub", "adding an app from Flathub", |home, cx| {
        edit(home, None, cx);
        if let Some(editor) = home.presets.editor.as_mut() {
            editor.searched = "steam".into();
            editor.found = Ok(data::flathub("steam"));
            let search = editor.flathub_id.clone();
            type_into(&search, "steam", cx);
        }
    }),
    state("set-fedora", "adding an app from Fedora", |home, cx| {
        edit(home, None, cx);
        if let Some(editor) = home.presets.editor.as_mut() {
            editor.source = SourceKind::Fedora;
            let (name, packages) = (editor.app_name.clone(), editor.packages.clone());
            type_into(&name, "Steam", cx);
            type_into(&packages, "steam", cx);
        }
    }),
    state("set-appimage", "adding an AppImage", |home, cx| {
        edit(home, None, cx);
        if let Some(editor) = home.presets.editor.as_mut() {
            editor.source = SourceKind::AppImage;
            // A folder too long for the form, which gives up its start.
            editor.appimage = Some(
                "/home/me/Downloads/Games/World of Warcraft/Addon managers/WowUp-CF-2.20.0.AppImage"
                    .into(),
            );
            let name = editor.app_name.clone();
            type_into(&name, "WowUp", cx);
        }
    }),
    state("set-problem", "a set refused", |home, cx| {
        edit(home, None, cx);
        if let Some(editor) = home.presets.editor.as_mut() {
            editor.problem = Some("A set needs a name".into());
        }
    }),
    state("set-saving", "a set being saved", |home, cx| {
        edit(home, Some(&data::wow_addons()), cx);
        if let Some(editor) = home.presets.editor.as_mut() {
            editor.saving = true;
        }
    }),
    state("set-title", "a set's title being edited", |home, cx| {
        edit(home, Some(&data::wow_addons()), cx);
        if let Some(editor) = &home.presets.editor {
            type_into(&editor.name.clone(), "WoW Classic addons", cx);
        }
    }),
    state(
        "set-delete",
        "a delete waiting for the second click",
        |home, cx| {
            edit(home, Some(&data::desktop_with_firefox()), cx);
            home.presets.confirm = Some(("desktop".into(), SetConfirm::Delete));
        },
    ),
    state("set-runs-as", "an app's Runs as menu open", |home, cx| {
        edit(home, Some(&data::wow_addons()), cx);
        home.open_menu = Some("runs-as-0".into());
    }),
    // Settings: New sandboxes.
    state("defaults", "settings: New sandboxes", |home, _| {
        settings(home, AppTab::Defaults);
    }),
    state("defaults-menu", "the GPU menu open", |home, _| {
        settings(home, AppTab::Defaults);
        home.open_menu = Some("defaults-gpu".into());
    }),
    state(
        "defaults-single",
        "new sandboxes on the only graphics card",
        |home, _| {
            settings(home, AppTab::Defaults);
            one_gpu(home);
        },
    ),
    state("defaults-saving", "the settings being saved", |home, _| {
        settings(home, AppTab::Defaults);
        home.saving = true;
    }),
    state("defaults-error", "the settings not read", |home, _| {
        settings(home, AppTab::Defaults);
        home.settings = Err("connecting to the gse service: No such file or directory".into());
        home.draft = None;
    }),
    // Settings: About and updates.
    state(
        "updates",
        "settings: About and updates, a newer release",
        |home, _| {
            settings(home, AppTab::Updates);
            home.update = Update::Available(about_release());
            home.update_checked = Some(checked_at());
        },
    ),
    state(
        "updates-unchecked",
        "updates not looked for yet",
        |home, _| {
            settings(home, AppTab::Updates);
        },
    ),
    state("updates-checking", "looking for updates", |home, _| {
        settings(home, AppTab::Updates);
        home.update = Update::Checking;
        home.update_checked = Some(checked_at());
    }),
    state("updates-none", "no newer release", |home, _| {
        settings(home, AppTab::Updates);
        home.update = Update::UpToDate;
        home.update_checked = Some(checked_at());
    }),
    state("updates-failed", "looking for updates failed", |home, _| {
        settings(home, AppTab::Updates);
        home.update = Update::Failed("api.github.com: connection timed out".into());
        home.update_checked = Some(checked_at());
    }),
    state("updates-installing", "an update installing", |home, _| {
        settings(home, AppTab::Updates);
        home.update = Update::Installing(about_release());
        home.update_checked = Some(checked_at());
    }),
    flagged(
        "updates-build",
        "the app run from a build, not an AppImage",
        &["no-appimage"],
        |home, _| settings(home, AppTab::Updates),
    ),
    state(
        "updates-service-older",
        "About with a service older than the app",
        |home, _| {
            settings(home, AppTab::Updates);
            home.service = client::Match::ServiceOlder;
        },
    ),
    flagged(
        "updates-dev",
        "About on a machine with the development helper",
        &["dev"],
        |home, _| settings(home, AppTab::Updates),
    ),
    flagged(
        "updates-dev-running",
        "the service build waiting for the password",
        &["dev"],
        |home, _| {
            settings(home, AppTab::Updates);
            home.dev_install = Install::Running;
        },
    ),
    flagged(
        "updates-dev-done",
        "the service build installed",
        &["dev"],
        |home, _| {
            settings(home, AppTab::Updates);
            home.dev_install = Install::Done;
        },
    ),
    flagged(
        "updates-dev-failed",
        "the service build not installed",
        &["dev"],
        |home, _| {
            settings(home, AppTab::Updates);
            home.dev_install = Install::Failed("not authorized".into());
        },
    ),
    // Remove everything.
    state(
        "remove-all",
        "settings: the Remove everything dialog",
        |home, cx| {
            settings(home, AppTab::General);
            home.open_remove(cx);
        },
    ),
    state(
        "remove-all-measuring",
        "Remove everything while it measures",
        |home, cx| {
            settings(home, AppTab::General);
            home.ask_remove(cx);
        },
    ),
    state(
        "remove-all-ready",
        "Remove everything confirmed, taken files too",
        |home, cx| {
            settings(home, AppTab::General);
            home.open_remove(cx);
            remove_confirmed(home, cx);
        },
    ),
    state("remove-all-busy", "everything being removed", |home, cx| {
        settings(home, AppTab::General);
        home.open_remove(cx);
        remove_confirmed(home, cx);
        home.busy.insert("remove-everything".into(), "Removing");
    }),
];

fn environment<'a>(home: &'a mut Home, name: &str) -> &'a mut Environment {
    home.environments
        .iter_mut()
        .find(|e| e.name == name)
        .expect("a sandbox of the made-up machine")
}

fn select(home: &mut Home, name: &str) {
    home.selected = Some(name.into());
    home.journal = Some((name.into(), Vec::new()));
}

/// Puts `name` in the running state at `stage`; `demo`'s journal shows
/// the start.
fn run(home: &mut Home, name: &str, stage: Stage) {
    environment(home, name).running = Some(data::running(stage));
    if name == data::DEMO {
        home.journal = Some((data::DEMO.into(), data::journal_since_start(stage)));
    }
}

/// The machine with one graphics card, every sandbox set to it.
fn one_gpu(home: &mut Home) {
    home.settings = Ok((data::settings(data::MONITOR_GPU), data::one_gpu()));
    home.draft = Some(data::settings(data::MONITOR_GPU));
    for environment in &mut home.environments {
        environment.settings.gpu = data::MONITOR_GPU.into();
    }
}

fn running_demo(home: &mut Home, _: &mut Context<Home>) {
    run(home, data::DEMO, Stage::Ready);
    home.processes = Some((data::DEMO.into(), Ok(data::programs())));
}

/// `demo`'s screen in the window, its desktop up for 24 minutes as the
/// Running board has it.
fn screen(home: &mut Home, cx: &mut Context<Home>) {
    running_demo(home, cx);
    home.ready_at.insert(
        data::DEMO.into(),
        super::now() - chrono::TimeDelta::minutes(24),
    );
    home.preview_screen = Some(data::DEMO.into());
    home.sidebar_collapsed = true;
}

fn files(home: &mut Home, cx: &mut Context<Home>) {
    screen(home, cx);
    home.files = Some((data::DEMO.into(), Ok(data::files())));
    home.modal = Some(Modal::Transfer {
        name: data::DEMO.into(),
    });
}

/// The Activity tab of the running `demo` as its board shows it: today's
/// lines of a long journal, filtered to what was refused or blocked.
fn activity(home: &mut Home, cx: &mut Context<Home>) {
    running_demo(home, cx);
    home.journal = Some((data::DEMO.into(), data::activity_journal()));
    home.journal_period = Period::Today;
    type_into(&home.journal_filter.clone(), "refus|block @18:00-19:30", cx);
}

/// The Programs tab of the running `demo` with the programs its board
/// lists.
fn programs(home: &mut Home, cx: &mut Context<Home>) {
    running_demo(home, cx);
    home.tab = PanelTab::Programs;
    home.processes = Some((data::DEMO.into(), Ok(data::programs_listed())));
}

/// The New sandbox dialog over the sets its board offers, `wow` building
/// when `building`.
fn new_sandbox(home: &mut Home, building: bool, cx: &mut Context<Home>) {
    home.load_presets(client::PresetsState {
        presets: data::new_sandbox_sets(),
        build: building.then(data::wow_build),
        base: Some(data::base(true)),
        stale: Vec::new(),
        failed: Vec::new(),
    });
    home.open_add(cx);
}

fn loading(home: &mut Home, _: &mut Context<Home>) {
    home.connected = None;
    home.environments.clear();
    home.selected = None;
    home.journal = None;
    home.settings = Err("Loading the settings...".into());
    home.presets.list = Err("Loading the sets of apps...".into());
    home.presets.base = None;
}

/// Replaces the build, the base image's state and the outdated sets.
fn presets(home: &mut Home, build: Option<Build>, current: bool, stale: Vec<String>) {
    home.load_presets(client::PresetsState {
        presets: data::sets(),
        build,
        base: Some(data::base(current)),
        stale,
        failed: Vec::new(),
    });
}

fn settings(home: &mut Home, tab: AppTab) {
    home.page = Page::AppSettings;
    home.app_tab = tab;
}

/// The page of `set`, or of a new set, in the editor.
fn edit(home: &mut Home, set: Option<&Preset>, cx: &mut Context<Home>) {
    settings(home, AppTab::Presets);
    home.open_editor(set, cx);
}

/// The sets of their page's board: three sandboxes from the game
/// launchers and `wow` waiting for its set; only the launchers built, on
/// a sandbox system `current` or not; the office set failed at its
/// packages; and `build` running for six minutes when given.
fn board_sets(home: &mut Home, build: Option<Build>, current: bool, stale: Vec<String>) {
    home.environments = data::board_environments();
    home.images = vec!["launchers".into()];
    home.load_presets(client::PresetsState {
        presets: data::board_sets(),
        build,
        base: Some(BaseImage {
            current,
            ..data::board_base()
        }),
        stale,
        failed: vec![data::office_failure()],
    });
    home.presets.since = home
        .presets
        .build
        .is_some()
        .then(|| super::now() - chrono::TimeDelta::minutes(6));
}

fn type_into(input: &Entity<TextInput>, text: &str, cx: &mut Context<Home>) {
    let text = text.to_owned();
    input.update(cx, |input, cx| input.set_text(text, cx));
}

fn add_name(home: &Home, text: &str, cx: &mut Context<Home>) {
    if let Some(Modal::Add { name, .. }) = &home.modal {
        type_into(&name.clone(), text, cx);
    }
}

fn remove_confirmed(home: &mut Home, cx: &mut Context<Home>) {
    if let Some(Modal::RemoveAll { keep, confirm, .. }) = home.modal.as_mut() {
        *keep = false;
        let confirm = confirm.clone();
        type_into(&confirm, "remove", cx);
    }
}

fn setup(
    home: &mut Home,
    step: Step,
    running: bool,
    failed: Option<&str>,
    activity: Option<&'static str>,
) {
    home.setup = Some(Setup {
        running,
        failed: failed.map(str::to_owned),
        activity,
        ..Setup::new(step)
    });
}

/// The sandbox system's download as the Setup board shows it: 1.2 of
/// 2.8 GB three minutes after the step began, so four are left.
fn setup_download(home: &mut Home, _: &mut Context<Home>) {
    setup(home, Step::System, true, None, None);
    if let Some(setup) = home.setup.as_mut() {
        setup.since = Some(super::now() - chrono::Duration::minutes(3));
        setup.download = Some((1_200_000_000, 2_800_000_000));
    }
    home.presets.build = Some(Build {
        preset: gse_protocol::BASE_IMAGE.into(),
        steps: vec!["prepare".into(), "download".into(), "download 43%".into()],
        started: None,
    });
}

/// The sets the tour offers, as its boards show them.
fn tour_sets(home: &mut Home) {
    home.load_presets(client::PresetsState {
        presets: data::tour_sets(),
        build: None,
        base: Some(data::base(true)),
        stale: Vec::new(),
        failed: Vec::new(),
    });
}

/// The second tour step with `preset` picked and `name` typed; only
/// `launchers` is built.
fn tour_first(home: &mut Home, preset: Option<&str>, name: &str, cx: &mut Context<Home>) {
    tour_sets(home);
    home.images = vec!["launchers".into()];
    let name = Home::first_name_field(name.to_owned(), cx);
    home.tour = Some(Tour::FirstSandbox {
        preset: preset.map(str::to_owned),
        name,
    });
}

/// The newer release the banners and About and updates show, as their
/// boards do: the one after a preview's version, 0.1.0.
fn about_release() -> Release {
    Release::preview(
        "0.2.0",
        "- Setup and a guided first sandbox\n\
         - Programs grouped by what started them\n\
         - Activity filter by time range",
    )
}

/// Three files of `demo` taken out into the archive the Notices board
/// names.
fn taken() -> Toast {
    Toast::taken(data::DEMO, 3, "/home/me/GSE/demo-2026-11-01.zip".into())
}

/// When About and updates last looked: today at 18:02.
fn checked_at() -> chrono::DateTime<chrono::Local> {
    super::now() - chrono::TimeDelta::minutes(88)
}
