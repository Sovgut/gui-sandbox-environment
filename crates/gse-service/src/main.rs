//! gse's root service.
//!
//! `daemon` serves the app over a socket. `install` and `uninstall` put gse
//! on the machine and take it off again; the app runs them through polkit.
//! The other commands are the same operations run directly from a terminal,
//! for development and repair.

mod agent;
mod checks;
mod daemon;
mod fedora;
mod gpu;
mod image;
mod net;
mod presets;
mod setup;
mod vm;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gse_host::Root;
use gse_host::pci::PciAddress;
use tracing::error;

/// Runtime state; cleared on reboot, which also returns every GPU to the host.
const STATE_DIR: &str = "/run/gse";

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Log every step and change nothing.
    #[arg(long, global = true)]
    dry_run: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve clients on the service socket.
    Daemon,
    /// Put gse on this machine, or update it: the service and guest
    /// programs from the directory this program runs from, the systemd unit,
    /// the polkit action, the network account and, unless there are
    /// settings already, the default settings; then (re)start the service.
    /// Each step is reported on stdout as `step <id>`, `warning <id> <text>`
    /// and `done <id>` or `failed <id> <reason>` lines.
    Install,
    /// Remove everything gse put on this machine: the service, its settings,
    /// images and environments, the network rules and account.
    Uninstall,
    /// GPUs and their hand-over to environments.
    Gpu {
        #[command(subcommand)]
        action: GpuAction,
    },
    /// Environment virtual machines.
    Vm {
        #[command(subcommand)]
        action: VmAction,
    },
    /// The environment network rules.
    Net {
        #[command(subcommand)]
        action: NetAction,
    },
    /// Environment images.
    Image {
        #[command(subcommand)]
        action: ImageAction,
    },
}

#[derive(Subcommand)]
enum ImageAction {
    /// Build the base image every environment shares: NVIDIA driver, desktop and the
    /// `sandbox` and `guest` accounts, in a VM without a GPU.
    BuildBase(image::BuildBaseArgs),
    /// Build a preset image: the base image with a preset's applications.
    BuildPreset(image::BuildPresetArgs),
}

#[derive(Subcommand)]
enum NetAction {
    /// Turn the running environment's internet on or off.
    Internet {
        #[arg(value_enum)]
        state: Switch,
    },
    /// Remove the environment network rules.
    Remove,
    /// Print the rules installed while an environment runs with internet on.
    Rules,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Subcommand)]
enum GpuAction {
    /// Every GPU, its driver, IOMMU group and switch mode.
    List,
    /// Processes holding a GPU's device nodes.
    Holders { gpu: PciAddress },
    /// Hand a GPU to vfio-pci. In single mode this stops the desktop; in
    /// secondary mode applications using the GPU have to be closed first.
    Attach { gpu: PciAddress },
    /// Return a GPU from vfio-pci to the host.
    Detach { gpu: PciAddress },
}

#[derive(Subcommand)]
enum VmAction {
    /// Hand the GPU to a VM, run it until it powers off, then return the GPU.
    Run(vm::RunArgs),
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Err(e) = run(&Cli::parse()) {
        error!("{e:#}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<()> {
    let root = Root::system();
    match &cli.command {
        Command::Daemon => {
            require_root(false)?;
            daemon::serve(root)
        }
        Command::Install => {
            require_root(cli.dry_run)?;
            setup::install(cli.dry_run)
        }
        Command::Uninstall => {
            require_root(cli.dry_run)?;
            setup::uninstall(cli.dry_run);
            Ok(())
        }
        Command::Gpu { action } => match action {
            GpuAction::List => gpu::list(&root),
            GpuAction::Holders { gpu } => gpu::holders(&root, *gpu),
            GpuAction::Attach { gpu } => {
                require_root(cli.dry_run)?;
                gpu::attach(&root, *gpu, cli.dry_run).map(drop)
            }
            GpuAction::Detach { gpu } => {
                require_root(cli.dry_run)?;
                gpu::detach(&root, *gpu, cli.dry_run)
            }
        },
        Command::Vm { action } => match action {
            VmAction::Run(args) => {
                require_root(cli.dry_run)?;
                vm::run(&root, args, &*stop_on_ctrl_c()?, cli.dry_run)
            }
        },
        Command::Image { action } => match action {
            ImageAction::BuildBase(args) => {
                require_root(cli.dry_run)?;
                image::build_base(args, &*stop_on_ctrl_c()?, cli.dry_run)
            }
            ImageAction::BuildPreset(args) => {
                require_root(cli.dry_run)?;
                image::build_preset(args, &*stop_on_ctrl_c()?, cli.dry_run)
            }
        },
        Command::Net { action } => match action {
            NetAction::Internet { state } => {
                require_root(cli.dry_run)?;
                net::apply(matches!(state, Switch::On), cli.dry_run)
            }
            NetAction::Remove => {
                require_root(cli.dry_run)?;
                net::remove(cli.dry_run)
            }
            NetAction::Rules => net::print_rules(),
        },
    }
}

/// A flag Ctrl+C sets, for commands that run a VM until it is stopped.
fn stop_on_ctrl_c() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let requested = Arc::clone(&stop);
    ctrlc::set_handler(move || requested.store(true, Ordering::SeqCst))
        .context("installing the signal handler")?;
    Ok(stop)
}

fn state_path(name: &str) -> PathBuf {
    PathBuf::from(STATE_DIR).join(name)
}

fn is_root() -> Result<bool> {
    let status = fs::read_to_string("/proc/self/status")?;
    let euid = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().nth(1))
        .context("no Uid line in /proc/self/status")?;
    Ok(euid == "0")
}

fn require_root(dry_run: bool) -> Result<()> {
    if !dry_run && !is_root()? {
        bail!("this needs root; --dry-run shows the plan without it");
    }
    Ok(())
}
