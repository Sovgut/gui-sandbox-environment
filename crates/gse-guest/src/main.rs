//! The part of gse that runs inside an environment.
//!
//! Untrusted programs run as the `guest` account but show their windows on the
//! desktop of `sandbox`. Three pieces make that possible without giving `guest`
//! the desktop:
//!
//! - `desktop` runs as `sandbox` for the lifetime of the desktop. It opens a
//!   Wayland socket for `guest` through the compositor's security-context
//!   protocol: labwc marks every client of that socket as sandboxed and
//!   withholds the privileged protocols from it, so no screen capture, no
//!   clipboard monitoring, no virtual input, no list of other windows. It also
//!   relays launch requests to the environment launcher.
//! - `launcher` runs as `guest`. It connects to the desktop and starts the
//!   programs it is asked for as transient units of the `guest` account's own
//!   systemd instance.
//! - `run` is how `sandbox` asks: it hands a command line to `desktop`.
//! - `screen` runs as `sandbox` and copies the desktop into the memory the host
//!   shows as the environment screen.
//!
//! Every connection between the two accounts is opened by `guest`. `sandbox` is
//! `unconfined_u` with the full MCS range and `guest` is `user_u` at `s0`;
//! SELinux cannot label a connection from the former to a socket the latter
//! serves, and the connect fails with `EINVAL`.

mod agent;
mod broker;
mod display;
mod incoming;
mod launcher;
mod screen;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

/// Directory of the sockets `guest` connects to: owned by `sandbox`, setgid
/// `guest`, so only those two accounts reach them.
pub const SHARED_DIR: &str = "/run/gse/display";
/// The restricted Wayland socket.
pub const DISPLAY_SOCKET: &str = "/run/gse/display/wayland-0";
/// Where the environment launcher connects to receive launch requests.
pub const LAUNCHER_SOCKET: &str = "/run/gse/display/launcher.sock";
/// Where `run` reaches `desktop`, in `sandbox`'s runtime directory.
const RUN_SOCKET_NAME: &str = "gse-run.sock";

fn run_socket() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join(RUN_SOCKET_NAME))
}

#[derive(Parser)]
#[command(version, about)]
enum Cli {
    /// As `sandbox`: open the restricted display for `guest` and relay launch
    /// requests, for the lifetime of the desktop.
    Desktop,
    /// As `guest`: connect to the desktop and start the programs it asks for.
    Launcher,
    /// As `sandbox`: start a program as `guest`.
    Run {
        /// The program's name: a notification says it is starting, since a
        /// launcher can take a while to show a window.
        #[arg(long)]
        title: Option<String>,
        /// The notification's icon, as a menu entry names it.
        #[arg(long, requires = "title")]
        icon: Option<String>,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// As `sandbox`: copy the desktop into the environment screen memory the host
    /// shows, for the lifetime of the desktop.
    Screen,
    /// As root: answer the host on the agent's port.
    Agent,
}

fn main() -> Result<()> {
    match Cli::parse() {
        Cli::Desktop => {
            std::thread::spawn(incoming::watch);
            broker::spawn(LAUNCHER_SOCKET.as_ref(), &run_socket()?)?;
            display::serve(DISPLAY_SOCKET.as_ref())
        }
        Cli::Launcher => launcher::serve(LAUNCHER_SOCKET.as_ref()),
        Cli::Run {
            title,
            icon,
            command,
        } => {
            broker::request(&run_socket()?, title.as_deref(), &command)?;
            if let Some(title) = title {
                broker::notify_starting(&title, icon.as_deref());
            }
            Ok(())
        }
        Cli::Screen => screen::serve(),
        Cli::Agent => agent::serve(),
    }
}
