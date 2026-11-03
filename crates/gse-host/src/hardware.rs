//! What the machine is: its CPU and the families of its GPUs, read from
//! procfs and sysfs through a [`Root`], how well gse knows each to work,
//! and the guest driver packs a base image needs for them. Hardware
//! decisions (the guest CPU, how a GPU changes hands, what an image carries)
//! are made from what this module reads.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use gse_protocol::{Support, SupportLevel};

use crate::families::{self, Family, Kind};
use crate::pci::{self, Gpu};
use crate::{Result, Root};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVendor {
    Amd,
    Intel,
    Other,
}

/// Which kind of core a logical CPU belongs to on a hybrid CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreKind {
    Uniform,
    Performance,
    Efficient,
}

/// A logical CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub cpu: u32,
    pub core: u32,
    pub package: u32,
    pub kind: CoreKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpu {
    pub vendor: CpuVendor,
    pub model_name: String,
    pub flags: BTreeSet<String>,
    /// The online logical CPUs.
    pub threads: Vec<Thread>,
}

/// CPUs of a list such as `0-11,16,18-19`.
fn cpu_list(text: &str) -> Vec<u32> {
    let mut cpus = Vec::new();
    for part in text.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((from, to)) => {
                if let (Ok(from), Ok(to)) = (from.parse::<u32>(), to.parse::<u32>()) {
                    cpus.extend(from..=to);
                }
            }
            None => cpus.extend(part.parse::<u32>().ok()),
        }
    }
    cpus
}

impl Cpu {
    pub fn read(root: &Root) -> Result<Self> {
        let info = root.read("/proc/cpuinfo")?;
        let field = |name: &str| {
            info.lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    (key.trim() == name).then(|| value.trim().to_owned())
                })
                .unwrap_or_default()
        };
        let vendor = match field("vendor_id").as_str() {
            "AuthenticAMD" => CpuVendor::Amd,
            "GenuineIntel" => CpuVendor::Intel,
            _ => CpuVendor::Other,
        };
        let performance = root
            .read_opt("/sys/devices/cpu_core/cpus")
            .map(|t| cpu_list(&t));
        let efficient = root
            .read_opt("/sys/devices/cpu_atom/cpus")
            .map(|t| cpu_list(&t));
        let mut threads: Vec<Thread> = root
            .list("/sys/devices/system/cpu")
            .into_iter()
            .filter_map(|name| name.strip_prefix("cpu")?.parse::<u32>().ok())
            .filter_map(|cpu| {
                let topology = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
                let number = |name: &str| root.read_opt(format!("{topology}/{name}"))?.parse().ok();
                let kind = match (&performance, &efficient) {
                    (Some(p), Some(e)) if !p.is_empty() && !e.is_empty() => {
                        if e.contains(&cpu) {
                            CoreKind::Efficient
                        } else {
                            CoreKind::Performance
                        }
                    }
                    _ => CoreKind::Uniform,
                };
                Some(Thread {
                    cpu,
                    core: number("core_id")?,
                    package: number("physical_package_id")?,
                    kind,
                })
            })
            .collect();
        threads.sort_by_key(|t| t.cpu);
        Ok(Self {
            vendor,
            model_name: field("model name"),
            flags: field("flags")
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            threads,
        })
    }

    #[must_use]
    pub fn is_hybrid(&self) -> bool {
        self.threads.iter().any(|t| t.kind == CoreKind::Efficient)
    }

    /// The logical CPUs a sandbox runs on: the performance cores' on a
    /// hybrid CPU, where an efficiency core would stall a game's main
    /// thread at random, and every one otherwise.
    #[must_use]
    pub fn guest_cpus(&self) -> Vec<u32> {
        self.threads
            .iter()
            .filter(|t| t.kind != CoreKind::Efficient)
            .map(|t| t.cpu)
            .collect()
    }

    /// Threads per core among [`Cpu::guest_cpus`]: two when every core
    /// there has two, else one.
    #[must_use]
    pub fn threads_per_core(&self) -> u16 {
        let guest = self.guest_cpus();
        let mut cores: std::collections::BTreeMap<(u32, u32), u16> =
            std::collections::BTreeMap::new();
        for thread in self.threads.iter().filter(|t| guest.contains(&t.cpu)) {
            *cores.entry((thread.package, thread.core)).or_default() += 1;
        }
        if !cores.is_empty() && cores.values().all(|n| *n == 2) {
            2
        } else {
            1
        }
    }

    /// QEMU's `-cpu`: the host's own CPU, with AMD's `topoext`, without which
    /// an AMD guest sees no threads per core.
    #[must_use]
    pub fn qemu_model(&self) -> &'static str {
        if self.vendor == CpuVendor::Amd && self.flags.contains("topoext") {
            "host,topoext=on"
        } else {
            "host"
        }
    }

    /// The guest CPU a VM of this host gets.
    #[must_use]
    pub fn guest(&self) -> crate::vm::GuestCpu {
        crate::vm::GuestCpu {
            model: self.qemu_model().to_owned(),
            threads_per_core: self.threads_per_core(),
            affinity: self.is_hybrid().then(|| self.guest_cpus()),
        }
    }

    #[must_use]
    pub fn support(&self) -> Support {
        match self.vendor {
            CpuVendor::Amd => Support {
                level: SupportLevel::Tested,
                reason: None,
            },
            CpuVendor::Intel => Support {
                level: SupportLevel::Expected,
                reason: Some(if self.is_hybrid() {
                    "gse has not run on an Intel CPU yet; sandboxes run on its performance \
                     cores only"
                        .to_owned()
                } else {
                    "gse has not run on an Intel CPU yet".to_owned()
                }),
            },
            CpuVendor::Other => Support {
                level: SupportLevel::Unsupported,
                reason: Some("gse knows only AMD and Intel CPUs".to_owned()),
            },
        }
    }

    /// The firmware switches virtualization and the IOMMU go by on this
    /// CPU, for people.
    #[must_use]
    pub const fn firmware_terms(&self) -> (&'static str, &'static str) {
        match self.vendor {
            CpuVendor::Amd => ("SVM", "AMD-Vi (IOMMU)"),
            CpuVendor::Intel => ("VT-x", "VT-d"),
            CpuVendor::Other => ("virtualization", "the IOMMU"),
        }
    }
}

/// Device IDs gse has run a sandbox on.
const TESTED: [(u16, u16); 2] = [
    (families::NVIDIA, 0x2484), // RTX 3070
    (families::NVIDIA, 0x2488), // RTX 3070 (LHR)
];

fn unsupported(reason: impl Into<String>) -> Support {
    Support {
        level: SupportLevel::Unsupported,
        reason: Some(reason.into()),
    }
}

fn expected(reason: impl Into<String>) -> Support {
    Support {
        level: SupportLevel::Expected,
        reason: Some(reason.into()),
    }
}

/// The reset methods the kernel has for `function`, if it says.
fn reset_methods(root: &Root, function: &pci::PciFunction) -> Option<Vec<String>> {
    root.read_opt(function.address.sysfs().join("reset_method"))
        .map(|text| text.split_whitespace().map(str::to_owned).collect())
}

/// How well gse knows `gpu` to work in a sandbox.
#[must_use]
pub fn gpu_support(root: &Root, gpu: &Gpu) -> Support {
    let display = gpu.display();
    let family = families::family(display.vendor, display.device);
    let foreign = pci::foreign_group_members(root, gpu);
    if let Some(member) = foreign.first() {
        return unsupported(format!(
            "it shares its IOMMU group with {} (class {:06x}), which would leave the host \
             with it; another PCIe slot may put it in a group of its own",
            member.address, member.class
        ));
    }
    let resets = reset_methods(root, display);
    let support = match family.kind {
        Kind::Unknown => unsupported("gse knows no driver for this vendor"),
        Kind::AmdIntegrated | Kind::IntelIntegrated => {
            unsupported("an integrated GPU cannot be handed to a sandbox whole")
        }
        Kind::NvidiaOld => unsupported(
            "no current NVIDIA driver supports it, and nouveau runs it at its lowest clocks",
        ),
        Kind::AmdOld => {
            unsupported("it runs on the old radeon driver and cannot be reset between sandboxes")
        }
        Kind::IntelDg1 => unsupported("Intel's drivers take this card only behind a test switch"),
        Kind::NvidiaOpen if TESTED.contains(&(display.vendor, display.device)) => Support {
            level: SupportLevel::Tested,
            reason: None,
        },
        Kind::NvidiaOpen => expected("it goes the way of the tested RTX 3070"),
        Kind::NvidiaLegacy => {
            expected("it needs NVIDIA's legacy driver in the sandbox; gse has not run on one yet")
        }
        Kind::AmdResetBug
            if !resets
                .as_ref()
                .is_some_and(|r| r.iter().any(|m| m == "device_specific")) =>
        {
            expected(
                "this family often fails to reset: after a sandbox the card may need a reboot \
                 to work on the host again, unless the vendor-reset module is installed",
            )
        }
        Kind::AmdModern | Kind::AmdResetBug => expected("gse has not run on an AMD card yet"),
        Kind::AmdUnknown => expected("gse does not know this AMD card yet"),
        Kind::IntelAlchemist | Kind::IntelBattlemage => expected(
            "gse has not run on an Intel card yet; it needs Resizable BAR turned on in the \
             firmware to run at full speed",
        ),
    };
    if support.level != SupportLevel::Unsupported && resets.as_ref().is_some_and(Vec::is_empty) {
        return expected(
            "it has no reset method, so a sandbox that crashes may leave it stuck until a reboot",
        );
    }
    support
}

/// The guest driver a sandbox runs `family` with: `chosen` when it is one
/// of the family's, else its default.
#[must_use]
pub fn guest_driver(family: Family, chosen: Option<&str>) -> Option<&'static str> {
    let drivers = family.guest_drivers();
    chosen
        .and_then(|c| drivers.iter().find(|d| **d == c).copied())
        .or_else(|| drivers.first().copied())
}

/// The driver packs a base image needs for GPUs of `families` run with the
/// guest drivers chosen beside them (see `guest/base/provision.sh`): `mesa`
/// always, `nvidia` or `nvidia-legacy` for NVIDIA cards on the proprietary
/// driver, `intel-media` for Intel cards. The two NVIDIA packs ship one
/// kernel module under one name, so they cannot share an image.
pub fn packs(gpus: &[(Family, Option<&str>)]) -> std::result::Result<Vec<&'static str>, String> {
    let mut packs = vec!["mesa"];
    let mut add = |pack: &'static str| {
        if !packs.contains(&pack) {
            packs.push(pack);
        }
    };
    for (family, chosen) in gpus {
        match (family.kind, guest_driver(*family, *chosen)) {
            (Kind::NvidiaOpen, Some("nvidia")) => add("nvidia"),
            (Kind::NvidiaLegacy, Some("nvidia")) => add("nvidia-legacy"),
            (
                Kind::IntelAlchemist
                | Kind::IntelBattlemage
                | Kind::IntelDg1
                | Kind::IntelIntegrated,
                _,
            ) => add("intel-media"),
            _ => {}
        }
    }
    if packs.contains(&"nvidia") && packs.contains(&"nvidia-legacy") {
        return Err(
            "an NVIDIA card of Turing or newer and an older one cannot both run on the \
             proprietary driver: choose the open driver for one of them"
                .to_owned(),
        );
    }
    packs.sort_by_key(|p| {
        ["mesa", "nvidia", "nvidia-legacy", "intel-media"]
            .iter()
            .position(|q| q == p)
    });
    Ok(packs)
}

/// The machine as text, for diagnostics and `gse-service hardware`.
#[must_use]
pub fn describe(root: &Root) -> String {
    let mut out = String::new();
    match Cpu::read(root) {
        Ok(cpu) => {
            let support = cpu.support();
            let _ = writeln!(out, "cpu {} ({:?})", cpu.model_name, cpu.vendor);
            let interesting: Vec<&str> = ["svm", "vmx", "topoext", "ht", "hypervisor"]
                .into_iter()
                .filter(|f| cpu.flags.contains(*f))
                .collect();
            let _ = writeln!(out, "  flags {}", interesting.join(" "));
            let _ = writeln!(
                out,
                "  threads {} hybrid {} guest cpus {} threads per core {}",
                cpu.threads.len(),
                cpu.is_hybrid(),
                cpu.guest_cpus().len(),
                cpu.threads_per_core()
            );
            let _ = writeln!(out, "  qemu -cpu {}", cpu.qemu_model());
            let _ = writeln!(
                out,
                "  support {:?} {}",
                support.level,
                support.reason.unwrap_or_default()
            );
        }
        Err(e) => {
            let _ = writeln!(out, "cpu unreadable: {e}");
        }
    }
    let _ = writeln!(
        out,
        "kernel cmdline {}",
        root.read_opt("/proc/cmdline").unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "iommu groups {}",
        root.list("/sys/kernel/iommu_groups").len()
    );
    for gpu in Gpu::discover(root).unwrap_or_default() {
        let display = gpu.display();
        let family = families::family(display.vendor, display.device);
        let support = gpu_support(root, &gpu);
        let _ = writeln!(
            out,
            "gpu {} {:04x}:{:04x} {} driver {} group {} monitor {} boot-vga {}",
            display.address,
            display.vendor,
            display.device,
            family.name,
            display.driver.as_deref().unwrap_or("-"),
            display
                .iommu_group
                .map_or_else(|| "-".to_owned(), |g| g.to_string()),
            gpu.drives_display(),
            display.boot_vga
        );
        for function in &gpu.functions {
            let _ = writeln!(
                out,
                "  function {} class {:06x} driver {} group {} reset {}",
                function.address,
                function.class,
                function.driver.as_deref().unwrap_or("-"),
                function
                    .iommu_group
                    .map_or_else(|| "-".to_owned(), |g| g.to_string()),
                reset_methods(root, function).map_or_else(|| "-".to_owned(), |r| r.join(","))
            );
        }
        for member in pci::foreign_group_members(root, &gpu) {
            let _ = writeln!(
                out,
                "  shares its group with {} class {:06x}",
                member.address, member.class
            );
        }
        let _ = writeln!(out, "  guest drivers {}", family.guest_drivers().join(" "));
        let _ = writeln!(
            out,
            "  support {:?} {}",
            support.level,
            support.reason.unwrap_or_default()
        );
    }
    for device in crate::usb::for_guest(root) {
        let _ = writeln!(
            out,
            "usb {} {:04x}:{:04x} {} goes to a single-GPU sandbox",
            device.name, device.vendor, device.product, device.label
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::pci::fixture;

    fn add_cpus(root: &Root, vendor: &str, flags: &str, cores: &[(u32, u32)]) {
        fs::create_dir_all(root.path("/proc")).unwrap();
        fs::write(
            root.path("/proc/cpuinfo"),
            format!("processor\t: 0\nvendor_id\t: {vendor}\nmodel name\t: Test CPU\nflags\t\t: {flags}\n\n"),
        )
        .unwrap();
        for (cpu, core) in cores {
            let dir = root.path(format!("/sys/devices/system/cpu/cpu{cpu}/topology"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("core_id"), format!("{core}\n")).unwrap();
            fs::write(dir.join("physical_package_id"), "0\n").unwrap();
        }
    }

    #[test]
    fn an_amd_cpu_with_smt_keeps_topoext_and_two_threads() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        add_cpus(
            &root,
            "AuthenticAMD",
            "fpu svm topoext ht",
            &[(0, 0), (1, 1), (2, 0), (3, 1)],
        );
        let cpu = Cpu::read(&root).unwrap();
        assert_eq!(cpu.qemu_model(), "host,topoext=on");
        assert_eq!(cpu.threads_per_core(), 2);
        assert_eq!(cpu.guest().affinity, None);
        assert_eq!(cpu.support().level, SupportLevel::Tested);
    }

    #[test]
    fn a_hybrid_intel_cpu_runs_sandboxes_on_its_performance_cores() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        // Two P-cores with two threads each, four E-cores with one.
        add_cpus(
            &root,
            "GenuineIntel",
            "fpu vmx ht",
            &[
                (0, 0),
                (1, 0),
                (2, 4),
                (3, 4),
                (4, 8),
                (5, 9),
                (6, 10),
                (7, 11),
            ],
        );
        fs::create_dir_all(root.path("/sys/devices/cpu_core")).unwrap();
        fs::create_dir_all(root.path("/sys/devices/cpu_atom")).unwrap();
        fs::write(root.path("/sys/devices/cpu_core/cpus"), "0-3\n").unwrap();
        fs::write(root.path("/sys/devices/cpu_atom/cpus"), "4-7\n").unwrap();
        let cpu = Cpu::read(&root).unwrap();
        assert!(cpu.is_hybrid());
        assert_eq!(cpu.qemu_model(), "host");
        assert_eq!(cpu.guest_cpus(), [0, 1, 2, 3]);
        assert_eq!(cpu.threads_per_core(), 2);
        assert_eq!(cpu.guest().affinity, Some(vec![0, 1, 2, 3]));
        assert_eq!(cpu.firmware_terms(), ("VT-x", "VT-d"));
    }

    #[test]
    fn an_intel_cpu_without_hyper_threading_gets_one_thread_per_core() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        add_cpus(
            &root,
            "GenuineIntel",
            "fpu vmx",
            &[(0, 0), (1, 1), (2, 2), (3, 3)],
        );
        let cpu = Cpu::read(&root).unwrap();
        assert_eq!(cpu.threads_per_core(), 1);
        assert_eq!(cpu.support().level, SupportLevel::Expected);
    }

    #[test]
    fn cpu_lists_expand() {
        assert_eq!(cpu_list("0-2,5,7-8\n"), [0, 1, 2, 5, 7, 8]);
        assert_eq!(cpu_list(""), [] as [u32; 0]);
    }

    #[test]
    fn the_tested_card_is_tested_and_the_desktop_arc_is_expected() {
        let (_dir, root) = fixture::dev_pc();
        let rtx = Gpu::read(&root, "0e:00".parse().unwrap()).unwrap();
        assert_eq!(gpu_support(&root, &rtx).level, SupportLevel::Tested);
        let arc = Gpu::read(&root, "07:00".parse().unwrap()).unwrap();
        assert_eq!(gpu_support(&root, &arc).level, SupportLevel::Expected);
    }

    #[test]
    fn packs_follow_the_cards_and_their_drivers() {
        let rtx = families::family(families::NVIDIA, 0x2484);
        let gtx1080 = families::family(families::NVIDIA, 0x1b80);
        let arc = families::family(families::INTEL, 0x56a5);
        let rx = families::family(families::AMD, 0x73bf);
        assert_eq!(packs(&[(rtx, None)]).unwrap(), ["mesa", "nvidia"]);
        assert_eq!(packs(&[(rtx, Some("nouveau"))]).unwrap(), ["mesa"]);
        assert_eq!(
            packs(&[(gtx1080, None)]).unwrap(),
            ["mesa", "nvidia-legacy"]
        );
        assert_eq!(
            packs(&[(arc, Some("xe")), (rx, None)]).unwrap(),
            ["mesa", "intel-media"]
        );
        assert_eq!(packs(&[]).unwrap(), ["mesa"]);
        assert!(packs(&[(rtx, None), (gtx1080, None)]).is_err());
        assert_eq!(
            packs(&[(rtx, None), (gtx1080, Some("nouveau"))]).unwrap(),
            ["mesa", "nvidia"]
        );
    }

    #[test]
    fn an_unknown_choice_falls_back_to_the_default() {
        let arc = families::family(families::INTEL, 0x56a5);
        assert_eq!(guest_driver(arc, Some("xe")), Some("xe"));
        assert_eq!(guest_driver(arc, Some("nvidia")), Some("i915"));
        assert_eq!(guest_driver(arc, None), Some("i915"));
    }
}
