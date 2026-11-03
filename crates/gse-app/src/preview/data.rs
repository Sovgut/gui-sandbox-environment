//! The made-up machine every preview starts from, close to what the
//! design's boards show: sandboxes `demo`, `steam-games` and `wow`, the
//! sets `launchers` (as the service ships it), `wow` and `desktop`, and
//! `demo`'s evening.

use gse_protocol::{
    AppSource, BaseImage, Build, Environment, FailedBuild, Gpu, GuestApp, GuestFile, GuestFiles,
    GuestProcess, JournalEntry, Preset, PresetApp, Running, ScreenAccess, Settings, Stage,
    SwitchMode,
};

/// The sandbox most states show, as on the boards.
pub const DEMO: &str = "demo";

use super::{ago, now_seconds};

/// The address of the GPU that drives the monitor, and of the one that
/// does not.
pub const MONITOR_GPU: &str = "0000:07:00.0";
pub const SPARE_GPU: &str = "0000:0e:00.0";

/// Seconds since the epoch at `hour:minute` of the preview's day.
pub fn at(hour: u64, minute: u64) -> u64 {
    let minutes = (19 * 60 + 30_u64).saturating_sub(hour * 60 + minute);
    now_seconds().saturating_sub(minutes * 60)
}

pub fn settings(gpu: &str) -> Settings {
    Settings {
        gpu: gpu.into(),
        screen_mode: "2560x1440@60".into(),
        internet: true,
        gpu_driver: None,
    }
}

pub fn gpus() -> Vec<Gpu> {
    vec![
        Gpu {
            address: MONITOR_GPU.into(),
            vendor_id: 0x8086,
            device_id: 0x56a5,
            vendor: Some("Intel Corporation".into()),
            device: Some("DG2 [Arc A380]".into()),
            driver: Some("i915".into()),
            monitor: true,
            monitor_mode: Some("2560x1440@59.95".into()),
            mode: SwitchMode::Single,
            family: Some("Intel Arc Alchemist".into()),
            integrated: false,
            support: gse_protocol::Support {
                level: gse_protocol::SupportLevel::Expected,
                reason: Some(
                    "gse has not run on an Intel card yet; it needs Resizable BAR turned on in \
                     the firmware to run at full speed"
                        .into(),
                ),
            },
            guest_drivers: vec!["i915".into(), "xe".into()],
        },
        Gpu {
            address: SPARE_GPU.into(),
            vendor_id: 0x10de,
            device_id: 0x2484,
            vendor: Some("NVIDIA Corporation".into()),
            device: Some("RTX 3070".into()),
            driver: Some("nouveau".into()),
            monitor: false,
            monitor_mode: None,
            mode: SwitchMode::Secondary,
            family: Some("NVIDIA Ampere".into()),
            integrated: false,
            support: gse_protocol::Support {
                level: gse_protocol::SupportLevel::Tested,
                reason: None,
            },
            guest_drivers: vec!["nvidia".into(), "nouveau".into()],
        },
    ]
}

/// A machine with two cards of one model, one of them driving the monitor.
pub fn same_model_gpus() -> Vec<Gpu> {
    vec![
        Gpu {
            monitor_mode: Some("2560x1440@60".into()),
            ..one_gpu().remove(0)
        },
        gpus().remove(1),
    ]
}

/// A machine with one graphics card, which drives the monitor.
pub fn one_gpu() -> Vec<Gpu> {
    vec![Gpu {
        address: MONITOR_GPU.into(),
        vendor_id: 0x10de,
        device_id: 0x2484,
        vendor: Some("NVIDIA Corporation".into()),
        device: Some("RTX 3070".into()),
        driver: Some("nvidia".into()),
        monitor: true,
        monitor_mode: Some("1920x1080@60".into()),
        mode: SwitchMode::Single,
        family: Some("NVIDIA Ampere".into()),
        integrated: false,
        support: gse_protocol::Support {
            level: gse_protocol::SupportLevel::Tested,
            reason: None,
        },
        guest_drivers: vec!["nvidia".into(), "nouveau".into()],
    }]
}

pub fn environment(name: &str, image: &str, played: Option<u64>) -> Environment {
    Environment {
        name: name.into(),
        image: image.into(),
        image_ready: true,
        created: ago(60 * 24 * 20),
        last_started: played,
        settings: settings(SPARE_GPU),
        running: None,
    }
}

/// `demo` played two hours ago, `steam-games` yesterday, `wow` from a set
/// not built yet, newest first as the boards list them.
pub fn environments() -> Vec<Environment> {
    vec![
        Environment {
            created: ago(60 * 24 * 5),
            ..environment(DEMO, "launchers", Some(ago(120)))
        },
        Environment {
            created: ago(60 * 24 * 20),
            ..environment("steam-games", "launchers", Some(ago(60 * 26)))
        },
        Environment {
            image_ready: false,
            created: ago(60 * 24 * 30),
            ..environment("wow", "wow", None)
        },
    ]
}

pub fn running(stage: Stage) -> Running {
    Running {
        stage,
        internet: true,
        screen: Some(ScreenAccess {
            memory: "/dev/shm/gse-preview.screen".into(),
            input: "/run/gse/preview.input".into(),
        }),
    }
}

pub fn app(name: &str, source: AppSource, trusted: bool) -> PresetApp {
    PresetApp {
        name: name.into(),
        source,
        desktop: None,
        command: Vec::new(),
        trusted,
    }
}

pub fn flatpak(id: &str) -> AppSource {
    AppSource::Flatpak { id: id.into() }
}

pub fn dnf(package: &str) -> AppSource {
    AppSource::Dnf {
        packages: vec![package.into()],
        repos: Vec::new(),
    }
}

/// The game launchers as the service ships them.
pub fn launchers() -> Preset {
    Preset {
        name: "launchers".into(),
        title: Some("Game launchers".into()),
        apps: vec![
            app("Steam", dnf("steam"), false),
            app("Lutris", dnf("lutris"), false),
            app("Heroic", flatpak("com.heroicgameslauncher.hgl"), false),
            app("ProtonPlus", flatpak("com.vysp3r.ProtonPlus"), false),
            app("Bottles", flatpak("com.usebottles.bottles"), false),
            app(
                "Faugus Launcher",
                flatpak("io.github.Faugus.faugus-launcher"),
                false,
            ),
            app("Chrome", dnf("google-chrome-stable"), false),
            app("Discord", flatpak("com.discordapp.Discord"), true),
        ],
    }
}

/// A set of a package and a Flatpak, so its build has five steps.
pub fn wow() -> Preset {
    Preset {
        name: "wow".into(),
        title: Some("WoW addons".into()),
        apps: vec![
            app("Wine", dnf("wine"), false),
            app("WowUp", flatpak("io.wowup.WowUp"), false),
        ],
    }
}

pub fn desktop() -> Preset {
    Preset {
        name: "desktop".into(),
        title: Some("Empty desktop".into()),
        apps: vec![app("Chrome", dnf("google-chrome-stable"), false)],
    }
}

/// The sets the sandboxes are made from: the game launchers with the
/// seven apps the boards of a sandbox's page count, `wow` and the empty
/// desktop.
pub fn sets() -> Vec<Preset> {
    vec![tour_sets().swap_remove(0), wow(), desktop()]
}

/// The sets the welcome tour offers, as its boards show them: the game
/// launchers with seven apps, and the empty desktop with its file manager
/// and browser.
pub fn tour_sets() -> Vec<Preset> {
    vec![
        Preset {
            name: "launchers".into(),
            title: Some("Game launchers".into()),
            apps: vec![
                app("Steam", dnf("steam"), false),
                app("Lutris", dnf("lutris"), false),
                app("Heroic", flatpak("com.heroicgameslauncher.hgl"), false),
                app("Bottles", flatpak("com.usebottles.bottles"), false),
                app("ProtonPlus", flatpak("com.vysp3r.ProtonPlus"), false),
                app(
                    "Faugus Launcher",
                    flatpak("io.github.Faugus.faugus-launcher"),
                    false,
                ),
                app("Discord", flatpak("com.discordapp.Discord"), true),
            ],
        },
        Preset {
            name: "desktop".into(),
            title: Some("Empty desktop".into()),
            apps: vec![
                app("Files", dnf("nemo"), true),
                app("Browser", dnf("google-chrome-stable"), false),
            ],
        },
    ]
}

/// The sets as the New sandbox board offers them: the game launchers as
/// the tour shows them, `wow` with its addon manager and Battle.net on
/// Wine (a Flatpak and a package, so its build has five steps), and the
/// empty desktop with a file manager and a browser.
pub fn new_sandbox_sets() -> Vec<Preset> {
    let launchers = tour_sets().swap_remove(0);
    vec![
        launchers,
        Preset {
            name: "wow".into(),
            title: Some("WoW addons".into()),
            apps: vec![
                app("WowUp", flatpak("io.wowup.WowUp"), false),
                app("Battle.net", dnf("wine"), false),
            ],
        },
        Preset {
            name: "desktop".into(),
            title: Some("Empty desktop".into()),
            apps: vec![
                app("Files", dnf("nemo"), true),
                app("browser", dnf("google-chrome-stable"), false),
            ],
        },
    ]
}

pub fn base(current: bool) -> BaseImage {
    BaseImage {
        built: ago(60 * 24 * 3),
        current,
    }
}

/// `wow` building for six minutes, at its Flatpaks step (40%).
pub fn wow_build() -> Build {
    Build {
        preset: "wow".into(),
        steps: vec!["prepare".into(), "packages".into(), "flatpaks".into()],
        started: Some(ago(6)),
    }
}

/// The sandbox system's build at its driver step, twelve minutes in.
pub fn system_build() -> Build {
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
        started: Some(ago(12)),
    }
}

/// `demo`'s evening as the service writes it.
pub fn journal() -> Vec<JournalEntry> {
    let line = |hour: u64, minute: u64, text: &str| JournalEntry {
        time: at(hour, minute),
        text: text.into(),
    };
    vec![
        line(
            18,
            12,
            "Refused setup.exe from the Outbox: a Windows program",
        ),
        line(
            18,
            28,
            "Starting for me on GPU 0000:0e:00.0, screen 2560x1440@60, internet on",
        ),
        line(18, 28, "Switching the GPU"),
        line(18, 28, "Booting"),
        line(18, 29, "The desktop is up"),
        line(18, 35, "Internet blocked for this sandbox"),
        line(
            18,
            40,
            "refused install.sh: a shell script with the executable bit",
        ),
        line(
            18,
            45,
            "1 file(s) of the Outbox taken to the host, 1 refused",
        ),
        line(18, 50, "Internet allowed for this sandbox"),
        line(18, 55, "Stopping"),
        line(18, 55, "Stopped"),
    ]
}

/// The journal of `demo` started again at 19:28, up to `stage`.
pub fn journal_since_start(stage: Stage) -> Vec<JournalEntry> {
    let reached = match stage {
        Stage::Preparing => 1,
        Stage::SwitchingGpu => 2,
        Stage::Booting => 3,
        Stage::Ready => 4,
        Stage::Stopping => 5,
    };
    let mut lines = journal();
    lines.extend(
        [
            (
                28,
                "Starting for me on GPU 0000:0e:00.0, screen 2560x1440@60, internet on",
            ),
            (28, "Switching the GPU"),
            (28, "Booting"),
            (29, "The desktop is up"),
            (30, "Stopping"),
        ]
        .into_iter()
        .take(reached)
        .map(|(minute, text)| JournalEntry {
            time: at(19, minute),
            text: text.into(),
        }),
    );
    lines
}

/// `demo`'s journal as the Activity board filters it: 214 lines, three of
/// them this evening about what was refused or blocked.
pub fn activity_journal() -> Vec<JournalEntry> {
    let evenings = [
        "Started on RTX 3070, internet allowed",
        "Ready",
        "Internet blocked for this sandbox",
        "Internet allowed for this sandbox",
        "Stopped",
    ];
    // Forty-two evenings before this one, from 20:05 every 20 minutes,
    // less their first line: 209 lines, and today's five make 214.
    let mut lines: Vec<JournalEntry> = (1..=42_u64)
        .rev()
        .flat_map(|days| {
            evenings
                .iter()
                .zip(0_u64..)
                .map(move |(text, i)| JournalEntry {
                    time: (now_seconds() + (35 + 20 * i) * 60).saturating_sub(days * 86_400),
                    text: (*text).to_owned(),
                })
        })
        .skip(1)
        .collect();
    lines.extend(
        [
            (18, 12, "Refused setup.exe from Outbox: Windows program"),
            (18, 28, "Started on RTX 3070, internet allowed"),
            (18, 29, "Ready"),
            (
                18,
                40,
                "refused install.sh: shell script with the executable bit",
            ),
            (19, 21, "Internet blocked for this sandbox"),
        ]
        .map(|(hour, minute, text)| JournalEntry {
            time: at(hour, minute),
            text: text.into(),
        }),
    );
    lines
}

/// A process of `guest`, its memory in decimal megabytes as the app shows
/// it.
fn process(pid: u32, name: &str, command: &str, mb: u64, cpu: u64) -> GuestProcess {
    GuestProcess {
        pid,
        name: name.into(),
        command: command.into(),
        memory_kib: mb * 1_000_000 / 1024,
        cpu_seconds: cpu,
    }
}

/// `demo`'s programs as the board of its screen lists them: Steam with its
/// helpers, the game it started, and the file manager of the Outbox.
pub fn programs() -> Vec<GuestApp> {
    let mut steam = vec![
        process(
            1438,
            "steam",
            "/home/guest/.local/share/Steam/steam.sh",
            312,
            41,
        ),
        process(1502, "steamwebhelper", "--type=gpu-process", 201, 12),
    ];
    // Twelve renderers that take 299 MB between them.
    steam.extend((0..12).map(|i| {
        process(
            1510 + i,
            "steamwebhelper",
            "--type=renderer",
            if i == 0 { 24 } else { 25 },
            1,
        )
    }));
    vec![
        GuestApp {
            unit: Some("app-steam.service".into()),
            title: "Steam".into(),
            started_by: None,
            processes: steam,
        },
        GuestApp {
            unit: Some("app-eldenring.scope".into()),
            title: "Elden Ring".into(),
            started_by: Some("Steam".into()),
            processes: vec![
                process(
                    3310,
                    "eldenring.exe",
                    "wine Z:\\games\\ELDEN RING\\eldenring.exe",
                    5800,
                    540,
                ),
                process(3290, "steam.exe", "wine C:\\Steam\\steam.exe", 180, 20),
                process(3281, "wineserver", "/usr/bin/wineserver", 60, 9),
                process(3296, "explorer.exe", "C:\\windows\\explorer.exe", 55, 2),
                process(
                    3287,
                    "services.exe",
                    "C:\\windows\\system32\\services.exe",
                    45,
                    1,
                ),
            ],
        },
        GuestApp {
            unit: Some("app-outbox.service".into()),
            title: "Outbox".into(),
            started_by: None,
            processes: vec![process(980, "nemo", "nemo /home/guest/Outbox", 51, 3)],
        },
    ]
}

/// `demo`'s programs as the Programs board lists them, each process in
/// sight.
pub fn programs_listed() -> Vec<GuestApp> {
    vec![
        GuestApp {
            unit: Some("app-steam.service".into()),
            title: "Steam".into(),
            started_by: None,
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
            started_by: Some("Steam".into()),
            processes: vec![process(
                3310,
                "eldenring.exe",
                "wine Z:\\games\\ELDEN RING\\eldenring.exe",
                5800,
                540,
            )],
        },
        GuestApp {
            unit: None,
            title: "Other programs".into(),
            started_by: None,
            processes: vec![process(980, "nemo", "nemo /home/guest/Inbox", 48, 3)],
        },
    ]
}

fn file(path: &str, size: u64) -> GuestFile {
    GuestFile {
        path: path.into(),
        size,
    }
}

pub fn files() -> GuestFiles {
    GuestFiles {
        inbox: vec![file("wowupcf.appimage", 137_000_000)],
        outbox: vec![
            file("saves/slot1.sav", 2_100_000),
            file("screenshots/boss.png", 3_400_000),
            file("trainer.exe", 900_000),
        ],
    }
}

/// A stopped sandbox's: files waiting for its next start, and what was
/// taken out of it while it ran on the whole screen.
pub fn files_between_runs() -> GuestFiles {
    GuestFiles {
        inbox: vec![
            file("mods/questie.zip", 4_100_000),
            file("wowupcf.appimage", 137_000_000),
        ],
        outbox: vec![file("screenshots/raid.png", 2_800_000)],
    }
}

/// What Flathub finds for `query`: the apps below whose name or id holds
/// it.
pub fn flathub(query: &str) -> Vec<crate::flathub::Found> {
    let query = query.to_lowercase();
    [
        (
            "Steam",
            "com.valvesoftware.Steam",
            "Launcher for the Steam software distribution service",
        ),
        (
            "Steam Link",
            "com.valvesoftware.SteamLink",
            "Play your Steam games from another computer",
        ),
        (
            "SteamGridDB Manager",
            "com.steamgriddb.SGDBoop",
            "Apply custom artwork to Steam games",
        ),
        (
            "Heroic Games Launcher",
            "com.heroicgameslauncher.hgl",
            "An Epic Games, GOG and Amazon Prime Games launcher",
        ),
        (
            "Lutris",
            "net.lutris.Lutris",
            "Video game preservation platform",
        ),
        (
            "Lutris Wine runners",
            "org.winehq.Wine",
            "Wine builds to run Windows games with Lutris",
        ),
        (
            "Discord",
            "com.discordapp.Discord",
            "Messaging, voice and video client",
        ),
    ]
    .into_iter()
    .filter(|(name, id, _)| {
        name.to_lowercase().contains(&query) || id.to_lowercase().contains(&query)
    })
    .map(|(name, id, summary)| crate::flathub::Found {
        name: name.into(),
        app_id: id.into(),
        summary: summary.into(),
    })
    .collect()
}

/// The sets of apps as their page's board shows them: the game launchers
/// trimmed to four, out of date; `wow` building; an office set whose build
/// failed; and the desktop with a browser, never built, which this board
/// calls "Browser only".
pub fn board_sets() -> Vec<Preset> {
    vec![
        Preset {
            name: "launchers".into(),
            title: Some("Game launchers".into()),
            apps: vec![
                app("Steam", dnf("steam"), false),
                app("Heroic", flatpak("com.heroicgameslauncher.hgl"), false),
                app("Lutris", dnf("lutris"), false),
                app("Bottles", flatpak("com.usebottles.bottles"), false),
            ],
        },
        // A package and a Flatpak, so its build has five steps.
        Preset {
            name: "wow".into(),
            title: Some("WoW addons".into()),
            apps: vec![
                app("WowUp", flatpak("io.wowup.WowUp"), false),
                app("CurseForge", dnf("curseforge"), false),
            ],
        },
        Preset {
            name: "office".into(),
            title: Some("Office".into()),
            apps: vec![
                app("LibreOffice", dnf("libreoffice"), false),
                app("Okular", flatpak("org.kde.okular"), false),
            ],
        },
        Preset {
            title: Some("Browser only".into()),
            ..desktop_with_firefox()
        },
    ]
}

/// The empty desktop with Firefox, as the boards of the sets show it.
pub fn desktop_with_firefox() -> Preset {
    Preset {
        name: "desktop".into(),
        title: Some("Empty desktop".into()),
        apps: vec![app("Firefox", dnf("firefox"), false)],
    }
}

/// The failed build of the office set on the sets' board, stopped at its
/// packages.
pub fn office_failure() -> FailedBuild {
    FailedBuild {
        preset: "office".into(),
        step: Some("packages".into()),
        log: Some("/var/log/gse/builds/1793546400-office-failed.log".into()),
    }
}

/// The sandboxes of the sets' board: three made from the game launchers
/// and `wow`, which waits for its set.
pub fn board_environments() -> Vec<Environment> {
    let mut environments = environments();
    environments.push(Environment {
        created: ago(60 * 24 * 10),
        ..environment("lan-party", "launchers", Some(ago(60 * 24 * 4)))
    });
    environments
}

/// The sandbox system built this morning, current.
pub fn board_base() -> BaseImage {
    BaseImage {
        built: at(9, 40),
        current: true,
    }
}

/// The game launchers as the sandbox system's board lists them.
pub fn system_board_launchers() -> Preset {
    Preset {
        name: "launchers".into(),
        title: Some("Game launchers".into()),
        apps: vec![
            app("Steam", dnf("steam"), false),
            app("Lutris", dnf("lutris"), false),
            app("Heroic", flatpak("com.heroicgameslauncher.hgl"), false),
            app("Bottles", flatpak("com.usebottles.bottles"), false),
            app("Firefox", dnf("firefox"), false),
            app("Discord", flatpak("com.discordapp.Discord"), true),
        ],
    }
}

/// `wow` as its editor's board shows it: an AppImage, two Flatpaks and a
/// package that runs as `sandbox`.
pub fn wow_addons() -> Preset {
    Preset {
        name: "wow".into(),
        title: Some("WoW addons".into()),
        apps: vec![
            app(
                "WowUp-CF",
                AppSource::Appimage {
                    path: "/home/me/Downloads/WowUp-CF-2.20.0.AppImage".into(),
                },
                false,
            ),
            app("Battle.net", flatpak("com.usebottles.bottles"), false),
            app("Firefox", dnf("firefox"), true),
            app("Discord", flatpak("com.discordapp.Discord"), false),
        ],
    }
}
