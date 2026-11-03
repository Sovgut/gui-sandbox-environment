//! Machines gse has not run on, built from public data on their parts:
//! what the hardware profile, the support levels, the guest driver packs
//! and the hand-over plans make of each. A capture from a real machine
//! (`scripts/target/capture-machine.sh`) replaces a synthetic one here once
//! someone runs it.

use std::fs;

use gse_protocol::SupportLevel;

use crate::families::{self, AMD, INTEL, NVIDIA};
use crate::hardware::{self, Cpu};
use crate::pci::Gpu;
use crate::pci::fixture::{self, Function, add, add_card, add_under, bridge};
use crate::plan::{self, Mode, Snapshot, Step};
use crate::{Root, session::Session};

fn function<'a>(
    address: &'a str,
    vendor: u16,
    device: u16,
    class: u32,
    driver: &'a str,
    group: u32,
) -> Function<'a> {
    Function {
        address,
        vendor,
        device,
        class,
        driver: Some(driver),
        group,
        boot_vga: false,
    }
}

fn vga<'a>(
    address: &'a str,
    vendor: u16,
    device: u16,
    driver: &'a str,
    group: u32,
) -> Function<'a> {
    function(address, vendor, device, 0x03_0000, driver, group)
}

fn audio(address: &str, vendor: u16, device: u16, group: u32) -> Function<'_> {
    function(address, vendor, device, 0x04_0300, "snd_hda_intel", group)
}

fn set_reset(root: &Root, address: &str, methods: &str) {
    fs::write(
        root.path(format!("/sys/bus/pci/devices/{address}/reset_method")),
        format!("{methods}\n"),
    )
    .unwrap();
}

/// An Intel CPU with `performance` two-thread cores and `efficient`
/// one-thread cores.
fn intel_hybrid(root: &Root, performance: u32, efficient: u32) {
    fs::create_dir_all(root.path("/proc")).unwrap();
    fs::write(
        root.path("/proc/cpuinfo"),
        "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: Intel Core i5-13600K\nflags\t\t: fpu vmx ht\n",
    )
    .unwrap();
    let mut cpu = 0;
    for core in 0..performance {
        for _ in 0..2 {
            add_thread(root, cpu, core * 4);
            cpu += 1;
        }
    }
    let p_cpus = cpu;
    for core in 0..efficient {
        add_thread(root, cpu, 100 + core);
        cpu += 1;
    }
    for (kind, list) in [
        ("cpu_core", format!("0-{}", p_cpus - 1)),
        ("cpu_atom", format!("{p_cpus}-{}", cpu - 1)),
    ] {
        fs::create_dir_all(root.path(format!("/sys/devices/{kind}"))).unwrap();
        fs::write(root.path(format!("/sys/devices/{kind}/cpus")), list).unwrap();
    }
}

fn add_thread(root: &Root, cpu: u32, core: u32) {
    let dir = root.path(format!("/sys/devices/system/cpu/cpu{cpu}/topology"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("core_id"), format!("{core}\n")).unwrap();
    fs::write(dir.join("physical_package_id"), "0\n").unwrap();
}

fn single_snapshot(gpu: Gpu) -> Snapshot {
    Snapshot {
        gpu,
        display_manager_active: true,
        graphical_sessions: vec![Session {
            id: "2".into(),
            user: "friend".into(),
        }],
        gpu_services: Vec::new(),
        sound_cards: Vec::new(),
    }
}

fn unloads(snapshot: &Snapshot) -> bool {
    plan::attach(snapshot)
        .0
        .steps
        .iter()
        .any(|s| matches!(s, Step::UnloadModules(_)))
}

fn family_of(gpu: &Gpu) -> families::Family {
    families::family(gpu.display().vendor, gpu.display().device)
}

#[test]
fn an_rx_6800_alone_on_a_hybrid_intel_cpu() {
    let dir = tempfile::tempdir().unwrap();
    let root = Root::at(dir.path());
    intel_hybrid(&root, 6, 8);
    // Navi 21 carries its own switch; GPU and audio share a slot behind it.
    add(&root, &bridge("0000:00:01.0", INTEL, 0xa70d, 1));
    add_under(
        &root,
        &bridge("0000:03:00.0", AMD, 0x1478, 14),
        "0000:00:01.0",
    );
    add_under(
        &root,
        &bridge("0000:04:00.0", AMD, 0x1479, 14),
        "0000:03:00.0",
    );
    let mut gpu = vga("0000:05:00.0", AMD, 0x73bf, "amdgpu", 15);
    gpu.boot_vga = true;
    add_under(&root, &gpu, "0000:04:00.0");
    add_under(
        &root,
        &audio("0000:05:00.1", AMD, 0xab28, 15),
        "0000:04:00.0",
    );
    add_card(
        &root,
        "0000:05:00.0",
        "card0",
        "renderD128",
        &[("DP-1", true)],
    );

    let cpu = Cpu::read(&root).unwrap();
    assert!(cpu.is_hybrid());
    let guest = cpu.guest();
    assert_eq!(guest.model, "host");
    assert_eq!(guest.threads_per_core, 2);
    assert_eq!(guest.affinity, Some((0..12).collect()));
    assert_eq!(cpu.support().level, SupportLevel::Expected);

    let rx = Gpu::read(&root, "05:00".parse().unwrap()).unwrap();
    assert_eq!(rx.functions.len(), 2);
    let support = hardware::gpu_support(&root, &rx);
    assert_eq!(support.level, SupportLevel::Expected);
    assert!(support.reason.unwrap().contains("AMD card"));
    assert_eq!(family_of(&rx).guest_drivers(), ["amdgpu"]);
    assert_eq!(
        hardware::packs(&[(family_of(&rx), None)]).unwrap(),
        ["mesa"]
    );
    let snapshot = single_snapshot(rx);
    assert_eq!(Mode::for_gpu(&snapshot.gpu), Mode::Single);
    assert!(!unloads(&snapshot));
}

#[test]
fn an_rx_580_warns_of_its_reset_unless_vendor_reset_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let root = Root::at(dir.path());
    add(&root, &vga("0000:0a:00.0", AMD, 0x67df, "amdgpu", 20));
    add(&root, &audio("0000:0a:00.1", AMD, 0xaaf0, 20));
    set_reset(&root, "0000:0a:00.0", "bus");
    let rx = Gpu::read(&root, "0a:00".parse().unwrap()).unwrap();
    let support = hardware::gpu_support(&root, &rx);
    assert_eq!(support.level, SupportLevel::Expected);
    assert!(support.reason.unwrap().contains("vendor-reset"));
    set_reset(&root, "0000:0a:00.0", "device_specific bus");
    let support = hardware::gpu_support(&root, &rx);
    assert!(!support.reason.unwrap().contains("vendor-reset"));
}

#[test]
fn an_arc_a770_beside_an_intel_igpu() {
    let dir = tempfile::tempdir().unwrap();
    let root = Root::at(dir.path());
    let mut igpu = vga("0000:00:02.0", INTEL, 0x4680, "i915", 0);
    igpu.boot_vga = true;
    add(&root, &igpu);
    add_card(
        &root,
        "0000:00:02.0",
        "card0",
        "renderD128",
        &[("HDMI-A-1", true)],
    );
    add(&root, &bridge("0000:00:01.0", INTEL, 0xa70d, 1));
    add_under(
        &root,
        &bridge("0000:03:00.0", INTEL, 0x4fa0, 12),
        "0000:00:01.0",
    );
    add_under(
        &root,
        &bridge("0000:04:01.0", INTEL, 0x4fa4, 13),
        "0000:03:00.0",
    );
    add_under(
        &root,
        &bridge("0000:04:04.0", INTEL, 0x4fa4, 14),
        "0000:03:00.0",
    );
    add_under(
        &root,
        &vga("0000:05:00.0", INTEL, 0x56a0, "i915", 15),
        "0000:04:01.0",
    );
    add_under(
        &root,
        &audio("0000:06:00.0", INTEL, 0x4f90, 16),
        "0000:04:04.0",
    );
    add_card(
        &root,
        "0000:05:00.0",
        "card1",
        "renderD129",
        &[("DP-1", false)],
    );

    let igpu = Gpu::read(&root, "00:02".parse().unwrap()).unwrap();
    assert_eq!(igpu.functions.len(), 1);
    assert_eq!(
        hardware::gpu_support(&root, &igpu).level,
        SupportLevel::Unsupported
    );
    let arc = Gpu::read(&root, "05:00".parse().unwrap()).unwrap();
    let functions: Vec<String> = arc
        .functions
        .iter()
        .map(|f| f.address.to_string())
        .collect();
    assert_eq!(functions, ["0000:05:00.0", "0000:06:00.0"]);
    assert_eq!(
        hardware::gpu_support(&root, &arc).level,
        SupportLevel::Expected
    );
    let family = family_of(&arc);
    assert_eq!(hardware::guest_driver(family, Some("xe")), Some("xe"));
    assert_eq!(
        hardware::packs(&[(family, Some("xe"))]).unwrap(),
        ["mesa", "intel-media"]
    );
    let snapshot = single_snapshot(arc);
    assert_eq!(Mode::for_gpu(&snapshot.gpu), Mode::Secondary);
    assert!(!unloads(&snapshot));
}

#[test]
fn a_radeon_beside_an_amd_apu() {
    let dir = tempfile::tempdir().unwrap();
    let root = Root::at(dir.path());
    add(&root, &bridge("0000:00:08.1", 0x1022, 0x1635, 9));
    let mut apu = vga("0000:0e:00.0", AMD, 0x1638, "amdgpu", 10);
    apu.boot_vga = true;
    add_under(&root, &apu, "0000:00:08.1");
    add_card(
        &root,
        "0000:0e:00.0",
        "card0",
        "renderD128",
        &[("HDMI-A-1", true)],
    );
    add(&root, &bridge("0000:00:01.1", 0x1022, 0x1633, 2));
    add_under(
        &root,
        &vga("0000:03:00.0", AMD, 0x73df, "amdgpu", 3),
        "0000:00:01.1",
    );
    add_under(
        &root,
        &audio("0000:03:00.1", AMD, 0xab28, 3),
        "0000:00:01.1",
    );

    let apu = Gpu::read(&root, "0e:00".parse().unwrap()).unwrap();
    assert!(family_of(&apu).integrated());
    assert_eq!(
        hardware::gpu_support(&root, &apu).level,
        SupportLevel::Unsupported
    );
    let radeon = Gpu::read(&root, "03:00".parse().unwrap()).unwrap();
    assert_eq!(radeon.functions.len(), 2);
    assert_eq!(
        hardware::gpu_support(&root, &radeon).level,
        SupportLevel::Expected
    );
}

#[test]
fn a_gtx_1080_needs_the_legacy_driver() {
    let dir = tempfile::tempdir().unwrap();
    let root = Root::at(dir.path());
    let mut gtx = vga("0000:09:00.0", NVIDIA, 0x1b80, "nvidia", 16);
    gtx.boot_vga = true;
    add(&root, &gtx);
    add(&root, &audio("0000:09:00.1", NVIDIA, 0x10f0, 16));
    add_card(
        &root,
        "0000:09:00.0",
        "card0",
        "renderD128",
        &[("DP-1", true)],
    );

    let gtx = Gpu::read(&root, "09:00".parse().unwrap()).unwrap();
    let support = hardware::gpu_support(&root, &gtx);
    assert_eq!(support.level, SupportLevel::Expected);
    assert!(support.reason.unwrap().contains("legacy"));
    assert_eq!(
        hardware::packs(&[(family_of(&gtx), None)]).unwrap(),
        ["mesa", "nvidia-legacy"]
    );
    assert_eq!(
        hardware::packs(&[(family_of(&gtx), Some("nouveau"))]).unwrap(),
        ["mesa"]
    );
    assert!(unloads(&single_snapshot(gtx)));
}

#[test]
fn a_gpu_sharing_its_group_with_a_usb_controller_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let root = Root::at(dir.path());
    add(&root, &vga("0000:01:00.0", NVIDIA, 0x2504, "nvidia", 2));
    add(
        &root,
        &function("0000:02:00.0", 0x1022, 0x43ee, 0x0c_0330, "xhci_hcd", 2),
    );
    let gpu = Gpu::read(&root, "01:00".parse().unwrap()).unwrap();
    let support = hardware::gpu_support(&root, &gpu);
    assert_eq!(support.level, SupportLevel::Unsupported);
    assert!(support.reason.unwrap().contains("0000:02:00.0"));
}

#[test]
fn the_development_pc_keeps_its_tested_path() {
    let (_dir, root) = fixture::dev_pc();
    let rtx = Gpu::read(&root, "0e:00".parse().unwrap()).unwrap();
    assert_eq!(
        hardware::gpu_support(&root, &rtx).level,
        SupportLevel::Tested
    );
    assert_eq!(
        hardware::packs(&[(family_of(&rtx), None)]).unwrap(),
        ["mesa", "nvidia"]
    );
    // nouveau on the host unbinds; nothing is unloaded.
    assert!(!unloads(&single_snapshot(rtx)));
}

/// Prints what gse makes of a machine captured by
/// `scripts/target/capture-machine.sh`, unpacked at `GSE_MACHINE` (the
/// folder holding `proc` and `sys`).
#[test]
#[ignore = "needs GSE_MACHINE"]
fn a_captured_machine() {
    let root = Root::at(std::env::var("GSE_MACHINE").expect("GSE_MACHINE"));
    let text = hardware::describe(&root);
    println!("{text}");
    assert!(text.contains("gpu "));
}
