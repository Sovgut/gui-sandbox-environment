//! PCI functions and GPUs as sysfs describes them.

use std::fmt;
use std::path::PathBuf;
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
}

/// A display connector of a GPU, such as `card1-HDMI-A-1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    pub name: String,
    pub connected: bool,
}

/// A GPU and every function in its slot (usually its HDMI audio).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    /// The display function first, then the others in address order.
    pub functions: Vec<PciFunction>,
    /// DRM nodes of the display function: `card*` and `renderD*`.
    pub drm_nodes: Vec<String>,
    pub connectors: Vec<Connector>,
}

impl Gpu {
    /// Reads the GPU whose display function sits in `address`'s slot.
    pub fn read(root: &Root, address: PciAddress) -> Result<Self> {
        let mut functions = Vec::new();
        for name in root.list(PCI_DEVICES) {
            let Ok(candidate) = name.parse::<PciAddress>() else {
                continue;
            };
            if candidate.same_slot(&address) {
                functions.push(PciFunction::read(root, candidate)?);
            }
        }
        functions.sort_by_key(|f| (!f.is_display(), f.address));
        let Some(display) = functions.first().filter(|f| f.is_display()) else {
            return Err(Error::NoGpu(address.to_string()));
        };
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
                let status = root.read_opt(drm.join(card).join(&name).join("status"));
                connectors.push(Connector {
                    connected: status.as_deref() == Some("connected"),
                    name,
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
pub(crate) mod fixture {
    //! A fake sysfs tree shaped like the development PC: an Arc A380 that
    //! drives the monitor and an RTX 3070 with its HDMI audio and no monitor.

    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    pub struct Function<'a> {
        pub address: &'a str,
        pub vendor: u16,
        pub device: u16,
        pub class: u32,
        pub driver: Option<&'a str>,
        pub group: u32,
        pub boot_vga: bool,
    }

    pub fn add(root: &Root, f: &Function<'_>) {
        let dir = root.path(PathBuf::from(PCI_DEVICES).join(f.address));
        fs::create_dir_all(&dir).unwrap();
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

    pub fn dev_pc() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        add(
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
        );
        add(
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
        );
        add(
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
        );
        add(
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
