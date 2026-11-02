//! Previews: the app shows one of its pages in a given state, filled with
//! made-up sandboxes, sets and files, and talks to no service. It is how
//! the pages are compared with the design without a machine to match.
//!
//! `GSE_PREVIEW=<state>` picks the state; `GSE_PREVIEW=list` prints them.
//! Nothing in a preview reaches the service, so its buttons mostly do
//! nothing.

use gpui::{AppContext as _, Context};
use gse_protocol::{
    AppSource, BaseImage, Build, Environment, Gpu, GuestApp, GuestFile, GuestFiles, GuestProcess,
    JournalEntry, Preset, PresetApp, Running, ScreenAccess, Settings, Stage, SwitchMode,
};

use crate::onboarding::Tour;
use crate::setup::{Setup, Step};
use crate::shell::PanelTab;
use crate::text_input::TextInput;
use crate::{AppTab, Home, Modal, Page, client};

/// Every state a preview can show, with what it is.
pub const STATES: [(&str, &str); 29] = [
    ("main", "a stopped sandbox, Activity tab"),
    ("setup", "first start: preparing the computer"),
    ("setup-failed", "first start: a step failed"),
    (
        "setup-packages",
        "first start: installing what sandboxes run on",
    ),
    ("tour-welcome", "welcome tour, step 1"),
    ("tour-first", "welcome tour, step 2: the first sandbox"),
    ("tour-play", "welcome tour, step 3: press Play"),
    ("starting", "a sandbox starting"),
    ("waiting", "a sandbox waiting for its set to build"),
    ("running", "a running sandbox's page"),
    ("screen", "a running sandbox's screen in the window"),
    ("files", "the Files panel over a screen"),
    ("programs", "the Programs tab of a running sandbox"),
    ("activity", "the Activity tab with a filter"),
    ("sandbox-settings", "a sandbox's settings panel"),
    ("new-sandbox", "the New sandbox dialog"),
    ("single-gpu", "playing on the only graphics card"),
    ("close", "closing the window while a sandbox runs"),
    ("switch", "playing while another sandbox runs"),
    ("notices", "banners and a message"),
    ("settings", "settings: General"),
    ("sets", "settings: Sets of apps, one building"),
    (
        "new-set",
        "settings: the recommended sets to start a new set from",
    ),
    ("set-editor", "settings: editing a set"),
    ("defaults", "settings: New sandboxes"),
    ("updates", "settings: About and updates, a newer release"),
    ("remove-all", "settings: the Remove everything dialog"),
    (
        "building",
        "the sandbox system building, seen from a sandbox's page",
    ),
    (
        "system-build",
        "settings: Sets of apps while the sandbox system builds",
    ),
];

/// The state to show, when the app runs as a preview.
pub fn state() -> Option<String> {
    std::env::var("GSE_PREVIEW").ok().filter(|s| !s.is_empty())
}

/// Seconds since the epoch, `minutes` ago.
fn ago(minutes: u64) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .saturating_sub(minutes * 60)
}

fn settings(mode: &str) -> Settings {
    Settings {
        gpu: "0000:0e:00.0".into(),
        screen_mode: mode.into(),
        internet: true,
    }
}

fn gpus() -> Vec<Gpu> {
    vec![
        Gpu {
            address: "0000:07:00.0".into(),
            vendor_id: 0x8086,
            device_id: 0x56a5,
            vendor: Some("Intel Corporation".into()),
            device: Some("DG2 [Arc A380]".into()),
            driver: Some("i915".into()),
            monitor: true,
            monitor_mode: Some("2560x1440@59.95".into()),
            mode: SwitchMode::Single,
        },
        Gpu {
            address: "0000:0e:00.0".into(),
            vendor_id: 0x10de,
            device_id: 0x2484,
            vendor: Some("NVIDIA Corporation".into()),
            device: Some("RTX 3070".into()),
            driver: Some("nouveau".into()),
            monitor: false,
            monitor_mode: None,
            mode: SwitchMode::Secondary,
        },
    ]
}

fn environment(name: &str, image: &str, played: Option<u64>) -> Environment {
    Environment {
        name: name.into(),
        image: image.into(),
        image_ready: true,
        created: ago(60 * 24 * 20),
        last_started: played,
        settings: settings("2560x1440@60"),
        running: None,
    }
}

fn running(stage: Stage) -> Running {
    Running {
        stage,
        internet: true,
        screen: Some(ScreenAccess {
            memory: "/dev/shm/gse-preview.screen".into(),
            input: "/run/gse/preview.input".into(),
        }),
    }
}

fn app(name: &str, source: AppSource, trusted: bool) -> PresetApp {
    PresetApp {
        name: name.into(),
        source,
        desktop: None,
        command: Vec::new(),
        trusted,
    }
}

fn flatpak(id: &str) -> AppSource {
    AppSource::Flatpak { id: id.into() }
}

fn sets() -> Vec<Preset> {
    let dnf = |package: &str| AppSource::Dnf {
        packages: vec![package.into()],
        repos: Vec::new(),
    };
    vec![
        Preset {
            name: "launchers".into(),
            apps: vec![
                app("Steam", dnf("steam"), false),
                app("Lutris", dnf("lutris"), false),
                app("Heroic", flatpak("com.heroicgameslauncher.hgl"), false),
                app("Bottles", flatpak("com.usebottles.bottles"), false),
                app("ProtonPlus", flatpak("com.vysp3r.ProtonPlus"), false),
                app("Chrome", flatpak("com.google.Chrome"), false),
                app("Discord", flatpak("com.discordapp.Discord"), true),
            ],
        },
        Preset {
            name: "wow".into(),
            apps: vec![
                app(
                    "WowUp",
                    AppSource::Appimage {
                        path: "/home/me/AppImages/wowup-cf.AppImage".into(),
                    },
                    false,
                ),
                app("Battle.net", flatpak("com.usebottles.bottles"), false),
            ],
        },
        Preset {
            name: "desktop".into(),
            apps: vec![app("Firefox", flatpak("org.mozilla.firefox"), false)],
        },
    ]
}

fn journal() -> Vec<JournalEntry> {
    let line = |minutes: u64, text: &str| JournalEntry {
        time: ago(minutes),
        text: text.into(),
    };
    vec![
        line(180, "Created from the set of apps launchers"),
        line(
            95,
            "Starting for me on GPU 0000:0e:00.0, screen 2560x1440@60, internet on",
        ),
        line(95, "Switching the GPU"),
        line(94, "Booting"),
        line(93, "The desktop is up"),
        line(70, "Refused setup.exe from the Outbox: a Windows program"),
        line(52, "1 file(s) of the Outbox taken to the host, 1 refused"),
        line(
            40,
            "refused install.sh: a shell script with the executable bit",
        ),
        line(21, "Internet blocked for this sandbox"),
        line(
            12,
            "Stopping: QEMU is killed and the GPU goes back to the host",
        ),
        line(12, "Stopped"),
    ]
}

fn programs() -> Vec<GuestApp> {
    let process = |pid: u32, name: &str, command: &str, mib: u64, cpu: u64| GuestProcess {
        pid,
        name: name.into(),
        command: command.into(),
        memory_kib: mib * 1024,
        cpu_seconds: cpu,
    };
    vec![
        GuestApp {
            unit: Some("app-steam.service".into()),
            title: "Steam".into(),
            processes: vec![
                process(
                    1438,
                    "steam",
                    "/home/guest/.local/share/Steam/steam.sh",
                    312,
                    41,
                ),
                process(1502, "steamwebhelper", "--type=gpu-process", 201, 12),
            ],
        },
        GuestApp {
            unit: Some("app-eldenring.scope".into()),
            title: "Elden Ring".into(),
            processes: vec![process(
                3310,
                "eldenring.exe",
                "wine Z:\\games\\ELDEN RING\\eldenring.exe",
                5939,
                540,
            )],
        },
        GuestApp {
            unit: None,
            title: "Other programs".into(),
            processes: vec![process(980, "nemo", "nemo /home/guest/Inbox", 48, 3)],
        },
    ]
}

fn files() -> GuestFiles {
    let file = |path: &str, size: u64| GuestFile {
        path: path.into(),
        size,
    };
    GuestFiles {
        inbox: vec![file("wowupcf.appimage", 137 << 20)],
        outbox: vec![
            file("saves/slot1.sav", 2_200_000),
            file("screenshots/boss.png", 3_500_000),
            file("trainer.exe", 900_000),
        ],
    }
}

/// The sandbox system's build at its driver step.
fn system_build() -> Build {
    Build {
        preset: gse_protocol::BASE_IMAGE.into(),
        steps: [
            "prepare",
            "download",
            "update",
            "RPM Fusion",
            "NVIDIA driver",
        ]
        .map(str::to_owned)
        .to_vec(),
    }
}

impl Home {
    /// Fills the app with made-up sandboxes and sets and shows `state`.
    pub(crate) fn preview(&mut self, state: &str, cx: &mut Context<Self>) {
        self.connected = Some(true);
        self.service = client::Match::Same;
        self.settings = Ok((settings("2560x1440@60"), gpus()));
        self.draft = Some(settings("2560x1440@60"));
        self.images = vec!["launchers".into(), "desktop".into()];
        self.environments = vec![
            environment("demo", "launchers", Some(ago(120))),
            environment("steam-games", "launchers", Some(ago(60 * 26))),
            Environment {
                image_ready: false,
                ..environment("wow", "wow", None)
            },
        ];
        self.load_presets(client::PresetsState {
            presets: sets(),
            build: None,
            base: Some(BaseImage {
                built: ago(60 * 24 * 3),
                current: true,
            }),
            stale: Vec::new(),
        });
        self.selected = Some("demo".into());
        self.journal = Some(("demo".into(), journal()));
        self.tour = None;
        self.setup = None;
        self.status = None;
        self.show(state, cx);
        cx.notify();
    }

    /// Puts `name` in the running state at `stage`.
    fn preview_running(&mut self, name: &str, stage: Stage) {
        if let Some(environment) = self.environments.iter_mut().find(|e| e.name == name) {
            environment.running = Some(running(stage));
        }
    }

    /// The first-start page, preparing or stopped.
    fn preview_setup(&mut self, state: &str) {
        if state == "setup-packages" {
            self.setup = Some(Setup {
                step: Step::Check,
                running: true,
                warnings: Vec::new(),
                failed: None,
                activity: Some("installing what sandboxes run on: QEMU, the UEFI firmware, passt"),
            });
            return;
        }
        self.setup = Some(Setup {
            step: Step::System,
            running: state == "setup",
            warnings: Vec::new(),
            failed: (state == "setup-failed")
                .then(|| "listing the cloud images of Fedora 45: status code 403".to_owned()),
            activity: None,
        });
    }

    fn show(&mut self, state: &str, cx: &mut Context<Self>) {
        match state {
            "setup" | "setup-failed" | "setup-packages" => self.preview_setup(state),
            "tour-welcome" => self.tour = Some(Tour::Welcome),
            "tour-first" => {
                self.start_tour(cx);
                self.preview_tour_first(cx);
            }
            "tour-play" => {
                self.environments
                    .insert(0, environment("games", "launchers", None));
                self.selected = Some("games".into());
                self.journal = Some(("games".into(), journal()[..1].to_vec()));
                self.tour = Some(Tour::PressPlay);
            }
            "starting" => self.preview_running("demo", Stage::Booting),
            "waiting" => {
                self.selected = Some("wow".into());
                self.load_presets(client::PresetsState {
                    presets: sets(),
                    build: Some(Build {
                        preset: "wow".into(),
                        steps: vec!["prepare".into(), "flatpaks".into()],
                    }),
                    base: None,
                    stale: Vec::new(),
                });
            }
            "running" => {
                self.preview_running("demo", Stage::Ready);
                self.processes = Some(("demo".into(), Ok(programs())));
            }
            "screen" | "files" => {
                self.preview_running("demo", Stage::Ready);
                self.processes = Some(("demo".into(), Ok(programs())));
                self.preview_screen = Some("demo".into());
                self.sidebar_collapsed = true;
                if state == "files" {
                    self.files = Some(("demo".into(), Ok(files())));
                    self.modal = Some(Modal::Transfer {
                        name: "demo".into(),
                    });
                }
            }
            "programs" => {
                self.preview_running("demo", Stage::Ready);
                self.processes = Some(("demo".into(), Ok(programs())));
                self.tab = PanelTab::Programs;
            }
            "activity" => {
                self.journal_filter
                    .update(cx, |input, cx| input.set_text("refus|block", cx));
            }
            "sandbox-settings" => self.open_sandbox_settings("demo", cx),
            "new-sandbox" => self.open_add(cx),
            "single-gpu" => {
                self.modal = Some(Modal::SingleGpu {
                    name: "demo".into(),
                });
            }
            "close" => {
                self.preview_running("demo", Stage::Ready);
                self.modal = Some(Modal::Close);
            }
            "switch" => {
                self.preview_running("steam-games", Stage::Ready);
                self.modal = Some(Modal::Switch {
                    from: "steam-games".into(),
                    to: "demo".into(),
                });
            }
            "notices" => {
                self.service = client::Match::ServiceOlder;
                self.say("Took 3 files from demo", false);
            }
            "building" | "system-build" => self.show_build(state),
            "settings" | "sets" | "new-set" | "set-editor" | "defaults" | "updates"
            | "remove-all" => {
                self.show_settings(state, cx);
            }
            _ => {}
        }
    }

    /// The sandbox system building, from a sandbox's page or the sets.
    fn show_build(&mut self, state: &str) {
        if state == "system-build" {
            self.page = Page::AppSettings;
            self.app_tab = AppTab::Presets;
        }
        self.load_presets(client::PresetsState {
            presets: sets(),
            build: Some(system_build()),
            base: Some(BaseImage {
                built: ago(60 * 24 * 3),
                current: false,
            }),
            stale: vec!["launchers".into()],
        });
    }

    /// A settings page in `state`.
    fn show_settings(&mut self, state: &str, cx: &mut Context<Self>) {
        self.page = Page::AppSettings;
        self.app_tab = match state {
            "sets" | "new-set" | "set-editor" => AppTab::Presets,
            "defaults" => AppTab::Defaults,
            "updates" => AppTab::Updates,
            _ => AppTab::General,
        };
        match state {
            "sets" => self.load_presets(client::PresetsState {
                presets: sets(),
                build: Some(Build {
                    preset: "wow".into(),
                    steps: vec!["prepare".into(), "packages".into(), "flatpaks".into()],
                }),
                base: Some(BaseImage {
                    built: ago(60 * 24 * 3),
                    current: true,
                }),
                stale: Vec::new(),
            }),
            "new-set" => {
                self.presets.templates = sets().into_iter().filter(|s| s.name != "wow").collect();
                self.new_set(cx);
            }
            "set-editor" => {
                let set = sets().swap_remove(1);
                self.open_editor(Some(&set), cx);
            }
            "updates" => {
                self.update = crate::update::State::Available(crate::update::Release::preview(
                    "0.2.0",
                    "- Setup and a guided first sandbox\n- Programs grouped by what \
                         started them\n- Activity filter by time range",
                ));
                self.update_checked = Some(chrono::Local::now());
            }
            "remove-all" => self.open_remove(cx),
            _ => {}
        }
    }

    /// The second tour step with a set picked and a name typed.
    fn preview_tour_first(&mut self, cx: &mut Context<Self>) {
        let name = cx.new(|cx| {
            let mut input = TextInput::new("games", cx);
            input.set_text("games", cx);
            input
        });
        self.tour = Some(Tour::FirstSandbox {
            preset: Some("launchers".into()),
            name,
        });
    }
}

/// The states, one per line with what each shows, for `GSE_PREVIEW=list`.
pub fn list() -> String {
    STATES
        .iter()
        .fold(String::new(), |mut text, (state, what)| {
            use std::fmt::Write as _;
            let _ = writeln!(text, "{state}\t{what}");
            text
        })
}
