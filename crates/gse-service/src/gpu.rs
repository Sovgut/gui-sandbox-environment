//! GPU commands: listing, holders, and hand-over to vfio-pci and back.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use gse_host::exec::{self, Executor, HostSystem};
use gse_host::pci::{Gpu, PciAddress};
use gse_host::plan::{self, Handoff, Mode, VFIO_PCI};
use gse_host::{Root, holders};
use tracing::{error, info};

use crate::{STATE_DIR, is_root, state_path};

pub fn list(root: &Root) -> Result<()> {
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

pub fn holders(root: &Root, gpu: PciAddress) -> Result<()> {
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
    state_path(&format!("gpu-{gpu}.handoff"))
}

/// Hands the GPU to vfio-pci and returns it as it was before, so callers know
/// every function that moved. A failed attach returns the GPU to the host.
pub fn attach(root: &Root, gpu: PciAddress, dry_run: bool) -> Result<Gpu> {
    let system = HostSystem;
    let snapshot = exec::snapshot(root, &system, gpu)?;
    let gpu = snapshot.gpu.display().address;
    if snapshot.gpu.display().driver.as_deref() == Some(VFIO_PCI) {
        bail!("{gpu} is already on {VFIO_PCI}");
    }
    let (attach_plan, handoff) = plan::attach(&snapshot);
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
    Ok(snapshot.gpu)
}

pub fn detach(root: &Root, gpu: PciAddress, dry_run: bool) -> Result<()> {
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
