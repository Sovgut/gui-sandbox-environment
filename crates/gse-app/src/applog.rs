//! The app's log: everything it does, to `~/.cache/gse/app.log` (the
//! previous start's beside it) and to stderr. A panic is logged with its
//! backtrace before the app goes down, so a crash leaves a record.

use std::fs;
use std::path::PathBuf;

/// The size past which the log is moved aside when the app starts.
const ROTATE_AT: u64 = 8 << 20;

/// The folder the app keeps its logs in.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("gse")
}

/// The app's log.
pub fn path() -> PathBuf {
    dir().join("app.log")
}

/// Starts logging, and logs what this app is and where it runs.
pub fn init() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let path = path();
    let file = (|| {
        fs::create_dir_all(dir()).ok()?;
        if fs::metadata(&path).is_ok_and(|m| m.len() > ROTATE_AT) {
            fs::rename(&path, path.with_file_name("app.previous.log")).ok();
        }
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
    })();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,naga=warn,wgpu=warn"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(file.map(|file| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
        }))
        .init();

    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("the app crashed: {info}\n{backtrace}");
    }));

    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| "-".into());
    tracing::info!(
        "gse {} starting; AppImage {}, session {} on {}, desktop {}",
        env!("CARGO_PKG_VERSION"),
        var("APPIMAGE"),
        var("XDG_SESSION_TYPE"),
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "Wayland"
        } else {
            "X11"
        },
        var("XDG_CURRENT_DESKTOP"),
    );
}
