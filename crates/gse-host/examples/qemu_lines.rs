//! Prints the command lines gse runs, one per line as `<kind> <args>`, for
//! `scripts/dev/qemu-compat.sh` to try on each Fedora release gse supports:
//! passt, then QEMU for an image build, a sandbox in the app's window (with
//! either QMP monitor option) and a sandbox on a single GPU, then the
//! nftables ruleset with its newlines as `\n`. The paths are placeholders the
//! script fills in: `@DIR@` for a scratch folder, `@PASST@` for passt's
//! socket.
//!
//! `cargo run -p gse-host --example qemu_lines`

use std::path::PathBuf;

use gse_host::net::{Passt, ruleset};
use gse_host::vm::{HostInput, Network, Screen, VmSpec};

/// `nobody`, which every system has.
const NET_UID: u32 = 65534;

fn base(name: &str) -> VmSpec {
    VmSpec {
        name: name.into(),
        disk: "@DIR@/disk.qcow2".into(),
        home_disk: Some("@DIR@/home.qcow2".into()),
        read_only_disks: Vec::new(),
        firmware_vars: "@DIR@/OVMF_VARS.qcow2".into(),
        memory_mib: 2048,
        cpus: 2,
        cpu: gse_host::vm::GuestCpu::amd_smt(),
        passthrough: Vec::new(),
        network: Network::Passt {
            socket: "@PASST@".into(),
        },
        vnc_display: None,
        screen: None,
        host_input: Vec::new(),
        qmp_object: false,
        keyboard: None,
        input_socket: None,
        monitor_socket: "@DIR@/monitor".into(),
        agent_socket: None,
        serial_log: "@DIR@/console.log".into(),
        journal_log: None,
        run_as: Some("nobody".into()),
        guest_program: None,
        direct_io: false,
        image_build: false,
        gpu_driver: Some("nvidia".into()),
    }
}

fn main() {
    let mut build = base("build-base");
    build.read_only_disks = vec!["@DIR@/seed.img".into()];
    build.image_build = true;
    build.home_disk = None;

    let mut window = base("window");
    window.screen = Some(Screen {
        memory: "@DIR@/screen".into(),
        mode: "2560x1440@60".into(),
        sound_server: "@DIR@/pulse".into(),
    });
    window.input_socket = Some("@DIR@/input".into());
    window.agent_socket = Some("@DIR@/agent".into());
    window.journal_log = Some(PathBuf::from("@DIR@/guest.log"));

    let mut single = base("single");
    single.host_input = vec![HostInput {
        device: "@DIR@/keyboard".into(),
        keyboard: true,
    }];
    single.keyboard = Some("@DIR@/keyboard".into());
    single.input_socket = Some("@DIR@/input".into());
    single.agent_socket = Some("@DIR@/agent".into());
    single.journal_log = Some(PathBuf::from("@DIR@/guest.log"));

    let mut window_qmp_object = window.clone();
    window_qmp_object.qmp_object = true;

    let passt = Passt {
        socket: "@PASST@".into(),
        net_uid: NET_UID,
        dev_ssh_port: Some(2222),
    };
    println!("passt {}", passt.command_line().join(" "));
    for (kind, spec) in [
        ("build", build),
        ("window", window),
        ("window-qmp-object", window_qmp_object),
        ("single", single),
    ] {
        println!("{kind} {}", spec.command_line().join(" "));
    }
    println!(
        "nft {}",
        ruleset(NET_UID, true)
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
    );
}
