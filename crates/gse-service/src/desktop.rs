//! Keeping the host desktop off the GPUs sandboxes get, on a machine where
//! another GPU drives the monitor.
//!
//! KWin opens every GPU, one without a monitor included, and so does
//! kmscon; a GPU they hold cannot go to `vfio-pci` while the desktop runs.
//! The install therefore limits them to the GPU that drives the monitor:
//!
//! - a udev rule names that GPU's card node `/dev/dri/gse-desktop-card` by
//!   its PCI address (`cardN` numbers follow probe order, and by-path names
//!   hold `:`, KWin's list separator);
//! - a drop-in for every user's `plasma-kwin_wayland.service` sets
//!   `KWIN_DRM_DEVICES` to it;
//! - a Vulkan loader settings file gives `/usr/bin/kwin_wayland` only the
//!   desktop GPU's Vulkan driver: `kwin_wayland` has `cap_sys_nice`, so the
//!   loader ignores `VK_*` variables in it. A driver both GPUs share cannot
//!   be told apart this way, and the file is then left out;
//! - kmscon's `gpus=primary`, when the desktop GPU is the boot GPU.
//!
//! The loader reads one settings file by a fixed name, and kmscon one
//! config; one that gse did not write stays as it is. Everything applies
//! from the next login.

use std::fs;
use std::path::Path;

use anyhow::Result;
use gse_host::Root;
use gse_host::pci::Gpu;
use tracing::info;

const UDEV_RULE: &str = "/etc/udev/rules.d/61-gse-desktop-card.rules";
const KWIN_DROP_IN: &str = "/etc/systemd/user/plasma-kwin_wayland.service.d/gse-desktop-gpu.conf";
const LOADER_SETTINGS: &str = "/etc/vulkan/loader_settings.d/vk_loader_settings.json";
const KMSCON: &str = "/etc/kmscon/kmscon.conf";
/// The lines gse adds to kmscon's config; uninstall removes exactly these.
const KMSCON_LINES: &str =
    "# Added by gse-service: only the boot GPU, so sandboxes can take the others.\ngpus=primary\n";

/// The files that keep the desktop on its GPU, as they would be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Files {
    pub udev_rule: String,
    pub kwin_drop_in: String,
    /// None when the desktop GPU's Vulkan driver is one a sandbox GPU needs
    /// too, or the machine has none for it.
    pub loader_settings: Option<String>,
    /// Whether kmscon is limited to the boot GPU.
    pub kmscon: bool,
}

/// The Vulkan driver manifests a GPU on `driver` uses on the host.
fn icd_files(driver: &str) -> &'static [&'static str] {
    match driver {
        "i915" | "xe" => &["/usr/share/vulkan/icd.d/intel_icd.x86_64.json"],
        "amdgpu" => &["/usr/share/vulkan/icd.d/radeon_icd.x86_64.json"],
        "nouveau" => &["/usr/share/vulkan/icd.d/nouveau_icd.x86_64.json"],
        "nvidia" => &[
            "/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json",
            "/usr/share/vulkan/icd.d/nvidia_icd.json",
        ],
        _ => &[],
    }
}

/// What keeps the desktop on its GPU: needed when exactly one GPU drives
/// the monitor and another, which sandboxes can get, drives none.
#[must_use]
pub fn files(root: &Root) -> Option<Files> {
    let gpus = Gpu::discover(root).ok()?;
    let mut desktop = gpus.iter().filter(|g| g.drives_display());
    let (Some(desktop), None) = (desktop.next(), desktop.next()) else {
        return None;
    };
    let others: Vec<&Gpu> = gpus.iter().filter(|g| !g.drives_display()).collect();
    if others.is_empty() {
        return None;
    }
    let display = desktop.display();
    let udev_rule = format!(
        "# Written by gse-service: the GPU that drives the desktop, by its PCI address,\n\
         # for KWIN_DRM_DEVICES.\n\
         SUBSYSTEM==\"drm\", KERNEL==\"card[0-9]*\", KERNELS==\"{}\", SYMLINK+=\"dri/gse-desktop-card\"\n",
        display.address
    );
    let kwin_drop_in =
        "# Written by gse-service: KWin uses only the GPU that drives the desktop,\n\
         # so sandboxes can take the others while it runs.\n\
         [Service]\n\
         Environment=KWIN_DRM_DEVICES=/dev/dri/gse-desktop-card\n"
            .to_owned();
    let driver = display.driver.as_deref().unwrap_or_default();
    let shared = others.iter().any(|g| {
        let other = g.display().driver.as_deref().unwrap_or_default();
        icd_files(other)
            .iter()
            .any(|icd| icd_files(driver).contains(icd))
            || g.display().vendor == display.vendor
    });
    let icds: Vec<&str> = icd_files(driver)
        .iter()
        .copied()
        .filter(|icd| root.exists(icd))
        .collect();
    let loader_settings = (!shared && !icds.is_empty()).then(|| loader_settings(&icds));
    Some(Files {
        udev_rule,
        kwin_drop_in,
        loader_settings,
        kmscon: display.boot_vga,
    })
}

fn loader_settings(icds: &[&str]) -> String {
    let drivers: Vec<String> = icds
        .iter()
        .map(|icd| format!("        {{ \"path\": \"{icd}\" }}"))
        .collect();
    format!(
        "{{\n  \"file_format_version\": \"1.0.1\",\n  \"settings_array\": [\n    {{\n      \
         \"app_keys\": [\"/usr/bin/kwin_wayland\"],\n      \
         \"additional_drivers_use_exclusively\": true,\n      \
         \"additional_drivers\": [\n{}\n      ]\n    }}\n  ]\n}}\n",
        drivers.join(",\n")
    )
}

/// Writes the [`Files`] this machine needs, if any. A loader settings file
/// or kmscon config that is not gse's stays, with a warning.
pub fn install(warning: &mut dyn FnMut(String), dry_run: bool) -> Result<()> {
    let Some(files) = files(&Root::system()) else {
        info!("one GPU drives the monitor and no other can go to a sandbox beside it");
        return Ok(());
    };
    let write = |path: &str, text: &str| -> Result<()> {
        info!("writing {path}");
        if dry_run {
            return Ok(());
        }
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
        Ok(())
    };
    write(UDEV_RULE, &files.udev_rule)?;
    write(KWIN_DROP_IN, &files.kwin_drop_in)?;
    if let Some(settings) = &files.loader_settings {
        match fs::read_to_string(LOADER_SETTINGS) {
            Ok(present) if present != *settings && !written_by_gse(&present) => warning(format!(
                "{LOADER_SETTINGS} is not gse's and stays: KWin may open the GPU sandboxes \
                 get, and then that GPU waits for KWin to let go"
            )),
            _ => write(LOADER_SETTINGS, settings)?,
        }
    }
    if files.kmscon
        && let Ok(config) = fs::read_to_string(KMSCON)
        && !config.lines().any(|l| l.trim_start().starts_with("gpus="))
    {
        let mut config = config;
        if !config.is_empty() && !config.ends_with('\n') {
            config.push('\n');
        }
        config.push_str(KMSCON_LINES);
        write(KMSCON, &config)?;
    }
    crate::setup::run("udevadm", &["control", "--reload"], dry_run);
    crate::setup::run("udevadm", &["trigger", "--subsystem-match=drm"], dry_run);
    warning(
        "log out and in once: the desktop then keeps off the graphics card sandboxes get"
            .to_owned(),
    );
    Ok(())
}

/// Whether the loader settings `text` are the ones gse writes: they name
/// only `kwin_wayland`, exclusively.
fn written_by_gse(text: &str) -> bool {
    text.contains("\"app_keys\": [\"/usr/bin/kwin_wayland\"]")
        && text.matches("\"app_keys\"").count() == 1
}

/// Removes what [`install`] wrote.
pub fn uninstall(dry_run: bool) {
    for path in [UDEV_RULE, KWIN_DROP_IN] {
        if Path::new(path).exists() {
            info!("removing {path}");
            if !dry_run {
                fs::remove_file(path).ok();
            }
        }
    }
    if fs::read_to_string(LOADER_SETTINGS).is_ok_and(|t| written_by_gse(&t)) {
        info!("removing {LOADER_SETTINGS}");
        if !dry_run {
            fs::remove_file(LOADER_SETTINGS).ok();
        }
    }
    if let Ok(config) = fs::read_to_string(KMSCON)
        && config.contains(KMSCON_LINES)
    {
        info!("removing gse's lines from {KMSCON}");
        if !dry_run {
            fs::write(KMSCON, config.replace(KMSCON_LINES, "")).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dev_pc_keeps_its_desktop_on_the_arc() {
        let (_dir, root) = gse_host::pci::fixture::dev_pc();
        fs::create_dir_all(root.path("/usr/share/vulkan/icd.d")).unwrap();
        fs::write(
            root.path("/usr/share/vulkan/icd.d/intel_icd.x86_64.json"),
            "{}",
        )
        .unwrap();
        let files = files(&root).unwrap();
        assert!(files.udev_rule.contains("KERNELS==\"0000:07:00.0\""));
        assert!(
            files
                .kwin_drop_in
                .contains("KWIN_DRM_DEVICES=/dev/dri/gse-desktop-card")
        );
        let settings = files.loader_settings.unwrap();
        assert!(settings.contains("intel_icd.x86_64.json"));
        assert!(written_by_gse(&settings));
        assert!(files.kmscon);
    }

    #[test]
    fn a_machine_without_a_second_gpu_needs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(files(&Root::at(dir.path())), None);
    }
}
