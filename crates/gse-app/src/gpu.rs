//! Keeping the app off the GPUs environments get.
//!
//! The app draws with Vulkan, and the Vulkan loader opens every GPU whose
//! driver it loads, the passed-through one included. A process holding that
//! GPU blocks its hand-over to an environment, and unbinding a driver under
//! a live user corrupts kernel memory, so the service refuses. The app
//! therefore restarts itself once with `VK_LOADER_DRIVERS_DISABLE` set to the
//! Vulkan drivers of the vendors of every GPU an environment's settings name,
//! as long as a GPU of another vendor is left to draw on. When the desktop's
//! GPU shares the vendor (an Intel iGPU beside an Arc card, an AMD APU
//! beside a Radeon), leaving the driver out would leave the app nothing to
//! draw with, so it restarts inside bubblewrap instead, with a `/dev/dri`
//! that holds only the other GPUs' nodes. `GSE_APP_OFF_GPU` names the GPUs
//! that restart was made for; an app started again after they changed (the
//! settings views restart it) inherits both variables and restarts once
//! more for the GPUs now set.

use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

const CONFIG: &str = "/etc/gse/gse.toml";
const ENVIRONMENTS: &str = "/var/lib/gse/environments";
const VARIABLE: &str = "VK_LOADER_DRIVERS_DISABLE";
const MARKER: &str = "GSE_APP_OFF_GPU";
const BUBBLEWRAP: &str = "/usr/bin/bwrap";

/// The Vulkan driver manifests of a PCI vendor, as loader globs.
fn drivers(vendor: u32) -> Option<&'static str> {
    match vendor {
        0x10de => Some("*nouveau*,*nvidia*"),
        0x1002 => Some("*radeon*,*amd*"),
        0x8086 => Some("*intel*"),
        _ => None,
    }
}

fn read_hex(path: &str) -> Option<u32> {
    let text = fs::read_to_string(path).ok()?;
    u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// PCI vendors of every display controller but those in `except`.
fn other_gpu_vendors(except: &[String]) -> Vec<u32> {
    fs::read_dir("/sys/bus/pci/devices")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !except.contains(&e.file_name().to_string_lossy().into_owned()))
        .filter_map(|e| {
            let dir = e.path().to_string_lossy().into_owned();
            let class = read_hex(&format!("{dir}/class"))?;
            (class >> 16 == 0x03).then(|| read_hex(&format!("{dir}/vendor")))?
        })
        .collect()
}

/// The GPU a settings file names, in the long form `0000:0e:00.0`.
fn gpu_of(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let table: toml::Table = text.parse().ok()?;
    let address = table.get("gpu")?.as_str()?.to_owned();
    // `0e:00.0` and `0000:0e:00.0` both name it; sysfs uses the long form.
    Some(
        if address.matches(':').count() == 1 {
            format!("0000:{address}")
        } else {
            address
        }
        .to_ascii_lowercase(),
    )
}

/// Every GPU the service's settings name: the one new environments get and
/// each environment's own, sorted.
pub fn reserved_gpus() -> Vec<String> {
    let mut gpus: Vec<String> = fs::read_dir(ENVIRONMENTS)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| gpu_of(&e.path().join("settings.toml")))
        .chain(gpu_of(Path::new(CONFIG)))
        .collect();
    gpus.sort();
    gpus.dedup();
    gpus
}

/// How the app stays off the GPUs environments get.
#[derive(Debug, PartialEq, Eq)]
enum StayOff {
    /// Leave these Vulkan drivers (loader globs) out.
    Drivers(String),
    /// Run with only these DRM nodes in `/dev/dri`, since the desktop's
    /// GPU needs the same driver.
    Nodes(Vec<String>),
    /// Nothing to stay off.
    Nothing,
}

/// How the app stays off `addresses`.
fn stay_off(addresses: &[String]) -> StayOff {
    let mut vendors: Vec<u32> = addresses
        .iter()
        .filter_map(|a| read_hex(&format!("/sys/bus/pci/devices/{a}/vendor")))
        .collect();
    vendors.sort_unstable();
    vendors.dedup();
    let others = other_gpu_vendors(addresses);
    if vendors.iter().any(|v| others.contains(v)) {
        return StayOff::Nodes(other_gpu_nodes(addresses));
    }
    let globs: Vec<&str> = vendors.into_iter().filter_map(drivers).collect();
    if globs.is_empty() {
        StayOff::Nothing
    } else {
        StayOff::Drivers(globs.join(","))
    }
}

/// The DRM nodes (`card*`, `renderD*`) of every display controller but
/// those in `except`.
fn other_gpu_nodes(except: &[String]) -> Vec<String> {
    let mut nodes: Vec<String> = fs::read_dir("/sys/bus/pci/devices")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !except.contains(&e.file_name().to_string_lossy().into_owned()))
        .filter(|e| {
            read_hex(&format!("{}/class", e.path().display())).is_some_and(|c| c >> 16 == 0x03)
        })
        .flat_map(|e| {
            fs::read_dir(e.path().join("drm"))
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|n| n.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("card") || n.starts_with("renderD"))
        .collect();
    nodes.sort();
    nodes
}

/// bubblewrap's arguments for a system as it is but with only `nodes` in
/// `/dev/dri`.
fn bubblewrap_args(nodes: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["--dev-bind", "/", "/", "--tmpfs", "/dev/dri"]
        .map(str::to_owned)
        .to_vec();
    for node in nodes {
        let path = format!("/dev/dri/{node}");
        args.extend(["--dev-bind".to_owned(), path.clone(), path]);
    }
    args.push("--".to_owned());
    args
}

/// Restarts the app without the environments' GPU drivers when that is
/// needed and possible; returns when the app may go on as it is.
pub fn stay_off_reserved_gpu() {
    let marker = std::env::var(MARKER).ok();
    if marker.is_none() && std::env::var_os(VARIABLE).is_some() {
        // Set by whoever started the app; theirs to choose.
        return;
    }
    let addresses = reserved_gpus();
    let joined = addresses.join(",");
    if marker.as_deref() == Some(joined.as_str()) {
        return;
    }
    let how = stay_off(&addresses);
    if marker.is_none() && how == StayOff::Nothing {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = match &how {
        StayOff::Nodes(nodes) if Path::new(BUBBLEWRAP).exists() => {
            let mut command = Command::new(BUBBLEWRAP);
            command.args(bubblewrap_args(nodes)).arg(&exe);
            command
        }
        StayOff::Nodes(_) => {
            eprintln!(
                "the desktop GPU needs the same driver as a GPU environments get, and \
                 {BUBBLEWRAP} is missing to hide that GPU from the app"
            );
            Command::new(&exe)
        }
        _ => Command::new(&exe),
    };
    command
        .args(std::env::args_os().skip(1))
        .env(MARKER, &joined);
    match how {
        StayOff::Drivers(globs) => command.env(VARIABLE, globs),
        _ => command.env_remove(VARIABLE),
    };
    let error = command.exec();
    eprintln!("restarting for the environments' GPUs {joined}: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bubblewrap_keeps_only_the_given_nodes() {
        assert_eq!(
            bubblewrap_args(&["card1".into(), "renderD128".into()]),
            [
                "--dev-bind",
                "/",
                "/",
                "--tmpfs",
                "/dev/dri",
                "--dev-bind",
                "/dev/dri/card1",
                "/dev/dri/card1",
                "--dev-bind",
                "/dev/dri/renderD128",
                "/dev/dri/renderD128",
                "--"
            ]
        );
    }

    #[test]
    fn each_vendor_maps_to_its_driver_manifests() {
        assert_eq!(drivers(0x10de), Some("*nouveau*,*nvidia*"));
        assert_eq!(drivers(0x8086), Some("*intel*"));
        assert_eq!(drivers(0x1234), None);
    }
}
