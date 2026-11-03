//! PCI functions and GPUs as sysfs describes them.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::{Error, Result, Root};

const PCI_DEVICES: &str = "/sys/bus/pci/devices";

/// `domain:bus:device.function`, written as `0000:0e:00.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PciAddress {
    pub domain: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    #[must_use]
    pub fn same_slot(&self, other: &Self) -> bool {
        (self.domain, self.bus, self.device) == (other.domain, other.bus, other.device)
    }

    #[must_use]
    pub fn sysfs(&self) -> PathBuf {
        PathBuf::from(PCI_DEVICES).join(self.to_string())
    }
}

impl fmt::Display for PciAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

impl FromStr for PciAddress {
    type Err = Error;

    /// Accepts `0000:0e:00.0`, `0e:00.0`, and slots without a function
    /// (`0000:0e:00`, `0e:00`), which mean function 0.
    fn from_str(s: &str) -> Result<Self> {
        let bad = || Error::PciAddress(s.to_owned());
        let (slot, function) = match s.rsplit_once('.') {
            Some((slot, function)) => (slot, u8::from_str_radix(function, 16).map_err(|_| bad())?),
            None => (s, 0),
        };
        let parts: Vec<&str> = slot.split(':').collect();
        let (domain, bus, device) = match parts.as_slice() {
            [d, b, v] => (*d, *b, *v),
            [b, v] => ("0", *b, *v),
            _ => return Err(bad()),
        };
        let address = Self {
            domain: u16::from_str_radix(domain, 16).map_err(|_| bad())?,
            bus: u8::from_str_radix(bus, 16).map_err(|_| bad())?,
            device: u8::from_str_radix(device, 16).map_err(|_| bad())?,
            function,
        };
        if address.device > 0x1f || address.function > 7 {
            return Err(bad());
        }
        Ok(address)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PciFunction {
    pub address: PciAddress,
    pub vendor: u16,
    pub device: u16,
    /// 24-bit class code, such as `0x030000` for a VGA controller.
    pub class: u32,
    pub driver: Option<String>,
    pub driver_override: Option<String>,
    pub iommu_group: Option<u32>,
    pub boot_vga: bool,
}

impl PciFunction {
    pub fn read(root: &Root, address: PciAddress) -> Result<Self> {
        let dir = address.sysfs();
        let hex = |name: &str| -> Result<u32> {
            let raw = root.read(dir.join(name))?;
            u32::from_str_radix(raw.trim_start_matches("0x"), 16)
                .map_err(|_| Error::Step(format!("{address}: unreadable {name} '{raw}'")))
        };
        let override_ = root
            .read_opt(dir.join("driver_override"))
            .filter(|v| !v.is_empty() && v != "(null)");
        Ok(Self {
            address,
            vendor: u16::try_from(hex("vendor")?).unwrap_or_default(),
            device: u16::try_from(hex("device")?).unwrap_or_default(),
            class: hex("class")?,
            driver: root.link_name(dir.join("driver")),
            driver_override: override_,
            iommu_group: root
                .link_name(dir.join("iommu_group"))
                .and_then(|g| g.parse().ok()),
            boot_vga: root.read_opt(dir.join("boot_vga")).as_deref() == Some("1"),
        })
    }

    #[must_use]
    pub fn is_display(&self) -> bool {
        matches!(self.class >> 8, 0x0300 | 0x0302)
    }

    #[must_use]
    pub fn is_bridge(&self) -> bool {
        self.class >> 16 == 0x06
    }
}

/// The directory of `address` at its place in the PCI tree, below the
/// bridges it sits behind.
fn device_dir(root: &Root, address: PciAddress) -> Option<PathBuf> {
    std::fs::canonicalize(root.path(address.sysfs())).ok()
}

fn address_of(dir: &Path) -> Option<PciAddress> {
    dir.file_name()?.to_str()?.parse().ok()
}

/// The upstream port of a PCIe switch on the card itself, which cards such
/// as Intel's Arc carry: their HDMI audio sits behind it as a device of its
/// own, in another slot and IOMMU group. It is the bridge two levels above
/// the GPU when that bridge has the GPU's vendor and is no root port (a root
/// port's parent is the host bridge, `pci0000:00`, which has no address).
fn card_switch(root: &Root, display: &PciFunction) -> Option<PathBuf> {
    let dir = device_dir(root, display.address)?;
    let downstream = dir.parent()?;
    let upstream = downstream.parent()?;
    address_of(downstream)?;
    let port = PciFunction::read(root, address_of(upstream)?).ok()?;
    address_of(upstream.parent()?)?;
    (port.vendor == display.vendor && port.is_bridge()).then(|| upstream.to_owned())
}

/// Functions that share an IOMMU group with one of `gpu`'s and are neither
/// part of it nor bridges. VFIO hands over whole groups, so the GPU cannot
/// go without them, and they would leave the host with it.
#[must_use]
pub fn foreign_group_members(root: &Root, gpu: &Gpu) -> Vec<PciFunction> {
    let groups: Vec<u32> = gpu.functions.iter().filter_map(|f| f.iommu_group).collect();
    root.list(PCI_DEVICES)
        .into_iter()
        .filter_map(|name| name.parse::<PciAddress>().ok())
        .filter(|a| !gpu.functions.iter().any(|f| f.address == *a))
        .filter_map(|address| PciFunction::read(root, address).ok())
        .filter(|f| !f.is_bridge() && f.iommu_group.is_some_and(|g| groups.contains(&g)))
        .collect()
}

/// A display connector of a GPU, such as `card1-HDMI-A-1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    pub name: String,
    pub connected: bool,
    /// The connected monitor's preferred mode from its EDID, as
    /// `1920x1080@60`: the mode a guest that takes the GPU shows on it.
    pub preferred_mode: Option<String>,
}

/// The preferred mode in an EDID: its first detailed timing descriptor, its
/// refresh rate exact to two decimals (`2560x1440@59.98`).
#[must_use]
pub fn edid_preferred_mode(edid: &[u8]) -> Option<String> {
    let d = edid.get(54..72)?;
    let clock = u64::from(u16::from_le_bytes([d[0], d[1]])) * 10_000;
    let width = u64::from(d[2]) | (u64::from(d[4] & 0xf0) << 4);
    let h_blank = u64::from(d[3]) | (u64::from(d[4] & 0x0f) << 8);
    let height = u64::from(d[5]) | (u64::from(d[7] & 0xf0) << 4);
    let v_blank = u64::from(d[6]) | (u64::from(d[7] & 0x0f) << 8);
    let total = (width + h_blank) * (height + v_blank);
    if clock == 0 || total == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let rate = clock as f64 / total as f64;
    let rate = format!("{rate:.2}");
    let rate = rate.trim_end_matches('0').trim_end_matches('.');
    Some(format!("{width}x{height}@{rate}"))
}

/// A GPU and its companions: every function in its slot (usually its HDMI
/// audio) and every device behind the card's own PCIe switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    /// The display function first, then the others in address order.
    pub functions: Vec<PciFunction>,
    /// DRM nodes of the display function: `card*` and `renderD*`.
    pub drm_nodes: Vec<String>,
    pub connectors: Vec<Connector>,
}

/// Sound cards a VM can take whole along with `gpu` on a single-GPU machine:
/// HD Audio controllers (class `0x0403`) other than the GPU's own, each
/// alone in its IOMMU group but for bridges, so that moving it moves
/// nothing else. The host's speakers and microphone on the board's jacks
/// are on one of them.
#[must_use]
pub fn movable_sound_cards(root: &Root, gpu: &Gpu) -> Vec<PciFunction> {
    let all: Vec<PciFunction> = root
        .list(PCI_DEVICES)
        .into_iter()
        .filter_map(|name| name.parse::<PciAddress>().ok())
        .filter_map(|address| PciFunction::read(root, address).ok())
        .collect();
    let is_bridge = PciFunction::is_bridge;
    all.iter()
        .filter(|f| f.class >> 8 == 0x0403)
        .filter(|f| !gpu.functions.iter().any(|g| g.address == f.address))
        .filter(|f| {
            f.iommu_group.is_some_and(|group| {
                all.iter()
                    .filter(|o| o.iommu_group == Some(group) && o.address != f.address)
                    .all(is_bridge)
            })
        })
        .cloned()
        .collect()
}

impl Gpu {
    /// Reads the GPU whose display function sits in `address`'s slot.
    pub fn read(root: &Root, address: PciAddress) -> Result<Self> {
        let addresses: Vec<PciAddress> = root
            .list(PCI_DEVICES)
            .into_iter()
            .filter_map(|name| name.parse().ok())
            .collect();
        let mut functions = Vec::new();
        for candidate in addresses.iter().filter(|a| a.same_slot(&address)) {
            functions.push(PciFunction::read(root, *candidate)?);
        }
        functions.sort_by_key(|f| (!f.is_display(), f.address));
        let Some(display) = functions.first().filter(|f| f.is_display()) else {
            return Err(Error::NoGpu(address.to_string()));
        };
        if let Some(switch) = card_switch(root, display) {
            for candidate in &addresses {
                if functions.iter().any(|f| f.address == *candidate) {
                    continue;
                }
                let inside = device_dir(root, *candidate)
                    .is_some_and(|dir| dir != switch && dir.starts_with(&switch));
                if inside {
                    let function = PciFunction::read(root, *candidate)?;
                    if !function.is_bridge() {
                        functions.push(function);
                    }
                }
            }
        }
        let display = &functions[0];
        let drm = display.address.sysfs().join("drm");
        let drm_nodes: Vec<String> = root
            .list(&drm)
            .into_iter()
            .filter(|n| n.starts_with("card") || n.starts_with("renderD"))
            .collect();
        let mut connectors = Vec::new();
        for card in drm_nodes.iter().filter(|n| n.starts_with("card")) {
            for name in root.list(drm.join(card)) {
                if !name.starts_with(&format!("{card}-")) {
                    continue;
                }
                let dir = drm.join(card).join(&name);
                let connected = root.read_opt(dir.join("status")).as_deref() == Some("connected");
                let preferred_mode = connected
                    .then(|| std::fs::read(root.path(dir.join("edid"))).ok())
                    .flatten()
                    .and_then(|edid| edid_preferred_mode(&edid));
                connectors.push(Connector {
                    name,
                    connected,
                    preferred_mode,
                });
            }
        }
        Ok(Self {
            functions,
            drm_nodes,
            connectors,
        })
    }

    /// Every GPU on the machine.
    pub fn discover(root: &Root) -> Result<Vec<Self>> {
        let mut gpus = Vec::new();
        for name in root.list(PCI_DEVICES) {
            let Ok(address) = name.parse::<PciAddress>() else {
                continue;
            };
            if PciFunction::read(root, address)?.is_display() {
                gpus.push(Self::read(root, address)?);
            }
        }
        Ok(gpus)
    }

    #[must_use]
    pub fn display(&self) -> &PciFunction {
        &self.functions[0]
    }

    #[must_use]
    pub fn iommu_group(&self) -> Option<u32> {
        self.display().iommu_group
    }

    /// The preferred mode of the first monitor attached, if its EDID says.
    #[must_use]
    pub fn monitor_mode(&self) -> Option<&str> {
        self.connectors
            .iter()
            .filter(|c| c.connected)
            .find_map(|c| c.preferred_mode.as_deref())
    }

    /// Whether a monitor is attached, which means the host desktop runs on it.
    #[must_use]
    pub fn drives_display(&self) -> bool {
        self.connectors.iter().any(|c| c.connected)
    }

    #[must_use]
    pub fn is_nvidia(&self) -> bool {
        self.display().vendor == 0x10de
    }
}

#[cfg(test)]
mod edid_tests {
    use super::edid_preferred_mode;

    /// An EDID whose first detailed timing is the given one.
    fn edid(clock_10khz: u16, h: (u16, u16), v: (u16, u16)) -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        let d = &mut edid[54..72];
        d[0..2].copy_from_slice(&clock_10khz.to_le_bytes());
        d[2] = (h.0 & 0xff) as u8;
        d[3] = (h.1 & 0xff) as u8;
        d[4] = (((h.0 >> 8) as u8) << 4) | (h.1 >> 8) as u8;
        d[5] = (v.0 & 0xff) as u8;
        d[6] = (v.1 & 0xff) as u8;
        d[7] = (((v.0 >> 8) as u8) << 4) | (v.1 >> 8) as u8;
        edid
    }

    #[test]
    fn the_preferred_mode_keeps_its_exact_rate() {
        // CVT reduced blanking 2560x1440: 241.50 MHz over 2720 x 1481.
        assert_eq!(
            edid_preferred_mode(&edid(24150, (2560, 160), (1440, 41))).as_deref(),
            Some("2560x1440@59.95")
        );
        // CEA 1920x1080: 148.50 MHz over 2200 x 1125 is exactly 60.
        assert_eq!(
            edid_preferred_mode(&edid(14850, (1920, 280), (1080, 45))).as_deref(),
            Some("1920x1080@60")
        );
        assert_eq!(edid_preferred_mode(&[0; 20]), None);
    }
}

#[cfg(any(test, feature = "fixtures"))]
// Test fixtures: they panic on any failure to build the fake tree.
#[allow(clippy::missing_panics_doc, clippy::must_use_candidate)]
pub mod fixture {
    //! Fake sysfs trees: PCI functions placed behind their bridges as the
    //! kernel lays them out, and the development PC built from them.

    use std::fs;
    use std::os::unix::fs::symlink;

    use super::{PCI_DEVICES, PathBuf, Root};

    pub struct Function<'a> {
        pub address: &'a str,
        pub vendor: u16,
        pub device: u16,
        pub class: u32,
        pub driver: Option<&'a str>,
        pub group: u32,
        pub boot_vga: bool,
    }

    /// Adds `f` on the root bus.
    pub fn add(root: &Root, f: &Function<'_>) {
        add_at(root, f, &root.path("/sys/devices/pci0000:00"));
    }

    /// Adds `f` behind the bridge `parent`, added before it.
    pub fn add_under(root: &Root, f: &Function<'_>, parent: &str) {
        let parent = fs::canonicalize(root.path(PathBuf::from(PCI_DEVICES).join(parent))).unwrap();
        add_at(root, f, &parent);
    }

    fn add_at(root: &Root, f: &Function<'_>, parent: &std::path::Path) {
        let dir = parent.join(f.address);
        fs::create_dir_all(&dir).unwrap();
        let link = root.path(PathBuf::from(PCI_DEVICES).join(f.address));
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&dir, &link).unwrap();
        fs::write(dir.join("vendor"), format!("0x{:04x}\n", f.vendor)).unwrap();
        fs::write(dir.join("device"), format!("0x{:04x}\n", f.device)).unwrap();
        fs::write(dir.join("class"), format!("0x{:06x}\n", f.class)).unwrap();
        fs::write(dir.join("driver_override"), "(null)\n").unwrap();
        if f.class >> 8 == 0x0300 {
            fs::write(dir.join("boot_vga"), if f.boot_vga { "1\n" } else { "0\n" }).unwrap();
        }
        symlink(
            format!("../../../kernel/iommu_groups/{}", f.group),
            dir.join("iommu_group"),
        )
        .unwrap();
        if let Some(driver) = f.driver {
            symlink(
                format!("../../../bus/pci/drivers/{driver}"),
                dir.join("driver"),
            )
            .unwrap();
        }
    }

    pub fn add_card(
        root: &Root,
        address: &str,
        card: &str,
        render: &str,
        connectors: &[(&str, bool)],
    ) {
        let drm = root.path(PathBuf::from(PCI_DEVICES).join(address).join("drm"));
        fs::create_dir_all(drm.join(render)).unwrap();
        for (name, connected) in connectors {
            let dir = drm.join(card).join(format!("{card}-{name}"));
            fs::create_dir_all(&dir).unwrap();
            let status = if *connected {
                "connected\n"
            } else {
                "disconnected\n"
            };
            fs::write(dir.join("status"), status).unwrap();
        }
    }

    /// A PCIe bridge (a root port or a switch port).
    pub fn bridge(address: &str, vendor: u16, device: u16, group: u32) -> Function<'_> {
        Function {
            address,
            vendor,
            device,
            class: 0x06_0400,
            driver: Some("pcieport"),
            group,
            boot_vga: false,
        }
    }

    /// The development PC: the Arc A380 behind its own switch, with its
    /// HDMI audio behind another port of it, and the RTX 3070 with its
    /// audio in one slot on a root port.
    pub fn dev_pc() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        add(&root, &bridge("0000:00:01.2", 0x1022, 0x1483, 2));
        add_under(
            &root,
            &bridge("0000:05:00.0", 0x8086, 0x4fa1, 24),
            "0000:00:01.2",
        );
        add_under(
            &root,
            &bridge("0000:06:01.0", 0x8086, 0x4fa4, 25),
            "0000:05:00.0",
        );
        add_under(
            &root,
            &bridge("0000:06:04.0", 0x8086, 0x4fa4, 26),
            "0000:05:00.0",
        );
        add_under(
            &root,
            &Function {
                address: "0000:07:00.0",
                vendor: 0x8086,
                device: 0x56a5,
                class: 0x03_0000,
                driver: Some("i915"),
                group: 27,
                boot_vga: true,
            },
            "0000:06:01.0",
        );
        add_under(
            &root,
            &Function {
                address: "0000:08:00.0",
                vendor: 0x8086,
                device: 0x4f92,
                class: 0x04_0300,
                driver: Some("snd_hda_intel"),
                group: 28,
                boot_vga: false,
            },
            "0000:06:04.0",
        );
        add(&root, &bridge("0000:00:03.1", 0x1022, 0x1483, 5));
        add_under(
            &root,
            &Function {
                address: "0000:0e:00.0",
                vendor: 0x10de,
                device: 0x2484,
                class: 0x03_0000,
                driver: Some("nouveau"),
                group: 31,
                boot_vga: false,
            },
            "0000:00:03.1",
        );
        add_under(
            &root,
            &Function {
                address: "0000:0e:00.1",
                vendor: 0x10de,
                device: 0x228b,
                class: 0x04_0300,
                driver: Some("snd_hda_intel"),
                group: 31,
                boot_vga: false,
            },
            "0000:00:03.1",
        );
        add_card(
            &root,
            "0000:07:00.0",
            "card1",
            "renderD128",
            &[("DP-3", true), ("HDMI-A-1", false)],
        );
        add_card(
            &root,
            "0000:0e:00.0",
            "card0",
            "renderD129",
            &[("DP-1", false), ("HDMI-A-5", false)],
        );
        (dir, root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_parse_in_all_accepted_forms() {
        let full: PciAddress = "0000:0e:00.1".parse().unwrap();
        assert_eq!(full.to_string(), "0000:0e:00.1");
        assert_eq!(
            "0e:00".parse::<PciAddress>().unwrap().to_string(),
            "0000:0e:00.0"
        );
        assert_eq!(
            "0000:09:00".parse::<PciAddress>().unwrap().to_string(),
            "0000:09:00.0"
        );
        assert!("0e".parse::<PciAddress>().is_err());
        assert!("0000:0e:20.0".parse::<PciAddress>().is_err());
        assert!("0000:0e:00.8".parse::<PciAddress>().is_err());
    }

    #[test]
    fn discovers_both_gpus_with_their_slots() {
        let (_dir, root) = fixture::dev_pc();
        let gpus = Gpu::discover(&root).unwrap();
        assert_eq!(gpus.len(), 2);

        let rtx = &gpus[1];
        assert_eq!(rtx.display().address.to_string(), "0000:0e:00.0");
        assert_eq!(rtx.functions.len(), 2);
        assert_eq!(rtx.functions[1].driver.as_deref(), Some("snd_hda_intel"));
        assert_eq!(rtx.iommu_group(), Some(31));
        assert!(rtx.is_nvidia());
        assert!(!rtx.drives_display());
        assert_eq!(rtx.drm_nodes, ["card0", "renderD129"]);
        assert_eq!(rtx.display().driver_override, None);

        let arc = &gpus[0];
        assert!(arc.display().boot_vga);
        assert!(arc.drives_display());
        // Its audio is behind the card's own switch, in a group of its own.
        let arc_functions: Vec<String> = arc
            .functions
            .iter()
            .map(|f| f.address.to_string())
            .collect();
        assert_eq!(arc_functions, ["0000:07:00.0", "0000:08:00.0"]);
    }

    #[test]
    fn a_device_sharing_the_gpus_group_is_foreign() {
        let (_dir, root) = fixture::dev_pc();
        let rtx = Gpu::read(&root, "0e:00".parse().unwrap()).unwrap();
        assert_eq!(foreign_group_members(&root, &rtx), []);
        fixture::add(
            &root,
            &fixture::Function {
                address: "0000:0f:00.0",
                vendor: 0x1022,
                device: 0x149c,
                class: 0x0c_0330,
                driver: Some("xhci_hcd"),
                group: 31,
                boot_vga: false,
            },
        );
        let foreign = foreign_group_members(&root, &rtx);
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].address.to_string(), "0000:0f:00.0");
    }

    #[test]
    fn reading_a_slot_without_a_gpu_fails() {
        let (_dir, root) = fixture::dev_pc();
        assert!(matches!(
            Gpu::read(&root, "0000:08:00".parse().unwrap()),
            Err(Error::NoGpu(_))
        ));
    }
}
