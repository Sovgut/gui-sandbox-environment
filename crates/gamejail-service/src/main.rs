//! gamejail's root service.
//!
//! The commands here are the service's operations, callable directly while
//! the service grows its socket interface.

mod gpu;
mod vm;

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gamejail_host::Root;
use gamejail_host::pci::PciAddress;
use tracing::error;

/// Runtime state; cleared on reboot, which also returns every GPU to the host.
const STATE_DIR: &str = "/run/gamejail";

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
    /// GPUs and their hand-over to jails.
    Gpu {
        #[command(subcommand)]
        action: GpuAction,
    },
    /// Jail virtual machines.
    Vm {
        #[command(subcommand)]
        action: VmAction,
    },
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
                vm::run(&root, args, cli.dry_run)
            }
        },
    }
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
