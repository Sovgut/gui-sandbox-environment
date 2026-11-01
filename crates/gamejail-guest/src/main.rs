//! The part of gamejail that runs inside a jail.
//!
//! Games run as the `jail` account but show their windows on the desktop of
//! `friend`. Three pieces make that possible without giving `jail` the desktop:
//!
//! - `desktop` runs as `friend` for the lifetime of the desktop. It opens a
//!   Wayland socket for `jail` through the compositor's security-context
//!   protocol: labwc marks every client of that socket as sandboxed and
//!   withholds the privileged protocols from it, so no screen capture, no
//!   clipboard monitoring, no virtual input, no list of other windows. It also
//!   relays launch requests to the jail launcher.
//! - `launcher` runs as `jail`. It connects to the desktop and starts the
//!   programs it is asked for as transient units of the jail account's own
//!   systemd instance.
//! - `run` is how `friend` asks: it hands a command line to `desktop`.
//!
//! Every connection between the two accounts is opened by `jail`. `friend` is
//! `unconfined_u` with the full MCS range and `jail` is `user_u` at `s0`;
//! SELinux cannot label a connection from the former to a socket the latter
//! serves, and the connect fails with `EINVAL`.

mod broker;
mod display;
mod launcher;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

/// Directory of the sockets `jail` connects to: owned by `friend`, setgid
/// `jail`, so only those two accounts reach them.
pub const SHARED_DIR: &str = "/run/gamejail/display";
/// The restricted Wayland socket.
pub const DISPLAY_SOCKET: &str = "/run/gamejail/display/wayland-0";
/// Where the jail launcher connects to receive launch requests.
pub const LAUNCHER_SOCKET: &str = "/run/gamejail/display/launcher.sock";
/// Where `run` reaches `desktop`, in `friend`'s runtime directory.
const RUN_SOCKET_NAME: &str = "gamejail-run.sock";

fn run_socket() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join(RUN_SOCKET_NAME))
}

#[derive(Parser)]
#[command(version, about)]
enum Cli {
    /// As `friend`: open the restricted display for `jail` and relay launch
    /// requests, for the lifetime of the desktop.
    Desktop,
    /// As `jail`: connect to the desktop and start the programs it asks for.
    Launcher,
    /// As `friend`: start a program as `jail`.
    Run {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse() {
        Cli::Desktop => {
            broker::spawn(LAUNCHER_SOCKET.as_ref(), &run_socket()?)?;
            display::serve(DISPLAY_SOCKET.as_ref())
        }
        Cli::Launcher => launcher::serve(LAUNCHER_SOCKET.as_ref()),
        Cli::Run { command } => broker::request(&run_socket()?, &command),
    }
}
