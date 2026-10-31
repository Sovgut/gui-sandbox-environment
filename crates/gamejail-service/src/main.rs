//! gamejail's root service.
//!
//! The commands here are the service's GPU operations, callable directly while
//! the service grows its socket interface.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gamejail_host::exec::{self, Executor, HostSystem};
use gamejail_host::pci::{Gpu, PciAddress};
use gamejail_host::plan::{self, Handoff, Mode, VFIO_PCI};
use gamejail_host::{Root, holders};
use tracing::{error, info};

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
}

#[derive(Subcommand)]
enum GpuAction {
    /// Every GPU, its driver, IOMMU group and switch mode.
    List,
    /// Processes holding a GPU's device nodes.
    Holders { gpu: PciAddress },
    /// Hand a GPU to vfio-pci. In single mode this stops the desktop.
    Attach {
        gpu: PciAddress,
        /// In secondary mode, proceed while user processes still hold the GPU.
        #[arg(long)]
        force: bool,
    },
    /// Return a GPU from vfio-pci to the host.
    Detach { gpu: PciAddress },
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
            GpuAction::List => list(&root),
            GpuAction::Holders { gpu } => holders(&root, *gpu),
            GpuAction::Attach { gpu, force } => attach(&root, *gpu, *force, cli.dry_run),
            GpuAction::Detach { gpu } => detach(&root, *gpu, cli.dry_run),
        },
    }
}

fn list(root: &Root) -> Result<()> {
    for gpu in Gpu::discover(root)? {
        let display = gpu.display();
        println!(
            "{}  {:04x}:{:04x}  driver {}  group {}  mode {}{}",
            display.address,
            display.vendor,
            display.device,
            display.driver.as_deref().unwrap_or("-"),
            gpu.iommu_group()
                .map_or_else(|| "-".into(), |g| g.to_string()),
            Mode::for_gpu(&gpu),
            if display.boot_vga { "  boot-vga" } else { "" },
        );
        for function in &gpu.functions[1..] {
            println!(
                "  {}  driver {}",
                function.address,
                function.driver.as_deref().unwrap_or("-")
            );
        }
        for connector in gpu.connectors.iter().filter(|c| c.connected) {
            println!("  monitor on {}", connector.name);
        }
    }
    Ok(())
}

fn holders(root: &Root, gpu: PciAddress) -> Result<()> {
    if !is_root()? {
        info!(
            "not root: processes of other users and ones with file capabilities, \
             such as kwin_wayland, are not visible"
        );
    }
    let gpu = Gpu::read(root, gpu)?;
    for holder in holders::scan(root, &holders::device_nodes(root, &gpu)) {
        println!(
            "{}  uid {}  {}  {}",
            holder.pid, holder.uid, holder.comm, holder.cgroup
        );
    }
    Ok(())
}

fn handoff_path(gpu: PciAddress) -> PathBuf {
    PathBuf::from(STATE_DIR).join(format!("gpu-{gpu}.handoff"))
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

fn attach(root: &Root, gpu: PciAddress, force: bool, dry_run: bool) -> Result<()> {
    require_root(dry_run)?;
    let system = HostSystem;
    let snapshot = exec::snapshot(root, &system, gpu)?;
    let gpu = snapshot.gpu.display().address;
    if snapshot.gpu.display().driver.as_deref() == Some(VFIO_PCI) {
        bail!("{gpu} is already on {VFIO_PCI}");
    }
    let (attach_plan, handoff) = plan::attach(&snapshot, force);
    info!("attaching {gpu} in {} mode", handoff.mode);

    if !dry_run {
        fs::create_dir_all(STATE_DIR)?;
        fs::write(handoff_path(gpu), handoff.to_text())?;
    }
    let executor = Executor {
        root,
        system: &system,
        dry_run,
    };
    if let Err(e) = executor.run(&attach_plan) {
        error!("attach failed: {e}; returning the GPU to the host");
        executor
            .run(&plan::detach(&handoff))
            .context("returning the GPU after a failed attach")?;
        if !dry_run {
            fs::remove_file(handoff_path(gpu)).ok();
        }
        return Err(e.into());
    }
    if let (false, Some(group)) = (dry_run, snapshot.gpu.iommu_group()) {
        info!("{gpu} is on {VFIO_PCI}: /dev/vfio/{group}");
    }
    Ok(())
}

fn detach(root: &Root, gpu: PciAddress, dry_run: bool) -> Result<()> {
    require_root(dry_run)?;
    let current = Gpu::read(root, gpu)?;
    let gpu = current.display().address;
    let handoff = if let Ok(text) = fs::read_to_string(handoff_path(gpu)) {
        Handoff::from_text(&text)?
    } else {
        // Not attached by this service, or the record was lost: hand the
        // functions back and let the kernel choose their drivers.
        info!("no handoff for {gpu}; letting the kernel pick drivers");
        Handoff {
            mode: Mode::Secondary,
            display_manager_was_active: false,
            gpu_services: Vec::new(),
            drivers: current
                .functions
                .iter()
                .map(|f| (f.address, None))
                .collect(),
        }
    };
    Executor {
        root,
        system: &HostSystem,
        dry_run,
    }
    .run(&plan::detach(&handoff))?;
    if !dry_run {
        fs::remove_file(handoff_path(gpu)).ok();
        let driver = Gpu::read(root, gpu)?.display().driver.clone();
        info!(
            "{gpu} is back on {}",
            driver.as_deref().unwrap_or("no driver")
        );
    }
    Ok(())
}
