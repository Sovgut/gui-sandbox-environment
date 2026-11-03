//! An environment VM and the QEMU command line that runs it.

use std::path::PathBuf;

use crate::pci::PciAddress;

pub const QEMU: &str = "qemu-system-x86_64";

/// UEFI firmware as Fedora's `edk2-ovmf` ships it. Secure Boot stays off: the
/// guest's NVIDIA module is built locally and not signed.
pub const OVMF_CODE: &str = "/usr/share/edk2/ovmf/OVMF_CODE_4M.qcow2";
pub const OVMF_VARS_TEMPLATE: &str = "/usr/share/edk2/ovmf/OVMF_VARS_4M.qcow2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSpec {
    pub name: String,
    /// The environment's writable disk, usually an overlay on a preset image.
    pub disk: PathBuf,
    /// The environment's own /home, which the guest finds by its serial
    /// `gse-home` and mounts over the system's; images are built without.
    pub home_disk: Option<PathBuf>,
    /// Extra raw disks attached read-only, such as a cloud-init seed.
    pub read_only_disks: Vec<PathBuf>,
    /// This VM's copy of the UEFI variable store.
    pub firmware_vars: PathBuf,
    pub memory_mib: u32,
    /// Logical CPUs, given to the guest as one socket of two-thread cores
    /// when the count is even.
    pub cpus: u16,
    /// PCI functions handed over whole, all already on vfio-pci.
    pub passthrough: Vec<PciAddress>,
    pub network: Network,
    /// An emulated display reachable over VNC on localhost, for development:
    /// the passed-through GPU is the environment's real screen.
    pub vnc_display: Option<u16>,
    /// The environment screen shown on the host: the guest copies its desktop
    /// into shared memory instead of driving a monitor of its own, and plays
    /// its sound on a playback-only sound card through the host user's sound
    /// server. QEMU reaches it over the PulseAudio protocol: its native
    /// PipeWire backend barely advances a stream it opened as root before
    /// dropping privileges, and the guest's sound then crawls.
    pub screen: Option<Screen>,
    /// The host's own keyboards and mice, which QEMU grabs and gives to the
    /// guest: on a single-GPU machine the environment owns the monitor and
    /// nothing on the host is left to send input. Both Ctrl keys pressed
    /// together hand them back, which the service takes as the way out.
    /// Only keyboards carry `grab_all`: QEMU's toggle on a device with it
    /// lets go of every device without it and skips the others, so the
    /// mice come back with the keyboard and the next move of the mouse
    /// tells the service at once.
    pub host_input: Vec<HostInput>,
    /// A file with the host user's keyboard layouts
    /// (`gse_protocol::Keyboard::to_guest_text`), handed over as the `fw_cfg`
    /// item `opt/gse/keyboard`.
    pub keyboard: Option<PathBuf>,
    /// Whether this QEMU has the `monitor-qmp` object (QEMU 11 on), which
    /// takes the place of the deprecated `-mon`; older ones only know `-mon`.
    pub qmp_object: bool,
    /// A QMP socket through which the viewer of the environment screen sends input to
    /// the VM's virtual keyboard, its tablet (absolute pointer for the
    /// desktop) and its mouse (relative motion for games that lock the
    /// pointer).
    pub input_socket: Option<PathBuf>,
    /// QEMU's human monitor, on a socket only root reaches, for inspecting a
    /// running VM.
    pub monitor_socket: PathBuf,
    /// The guest agent's virtio-serial port, on a socket only root reaches.
    pub agent_socket: Option<PathBuf>,
    /// The guest's serial console, appended to.
    pub serial_log: PathBuf,
    /// The guest's journal, which it streams over a virtio-serial port
    /// beside the agent's, appended to. Needs `agent_socket`.
    pub journal_log: Option<PathBuf>,
    /// The account QEMU drops to once its devices are open.
    pub run_as: Option<String>,
    /// Whether the disk and the home disk go past the host's page cache
    /// (`cache=none`), so a guest writing gigabytes does not fill it. Only
    /// for files that do not copy on write: direct I/O into a btrfs file
    /// with checksums can store a block whose checksum no longer matches
    /// when the guest changes the buffer while it is written.
    pub direct_io: bool,
    /// The `gse-guest` program the environment runs, put in place at boot
    /// so the guest's side of gse follows the host's without a rebuilt
    /// image. It travels on a read-only disk (serial `gse-guest`); its size
    /// and SHA-256 as the `fw_cfg` items `opt/gse/guest-size` and
    /// `opt/gse/guest-sha256`. `fw_cfg` itself suits a few bytes only: the
    /// guest reads it from the start for every page, so megabytes take
    /// minutes.
    pub guest_program: Option<GuestProgram>,
    /// An image build: the guest learns it through the `fw_cfg` item
    /// `opt/gse/build` and starts no desktop, which a VM without a GPU could
    /// only start and lose in a loop.
    pub image_build: bool,
}

/// The `gse-guest` program a start hands over: a disk image that holds it
/// from its first byte, its length and its SHA-256 in hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestProgram {
    pub image: PathBuf,
    pub size: u64,
    pub sha256: String,
}

/// An evdev device of the host that the VM takes over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInput {
    pub device: PathBuf,
    /// Keys repeat when held; a mouse has none to repeat.
    pub keyboard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The file behind the memory the guest shares with the host
    /// (`ivshmem-plain`); see `gse-screen` for its layout.
    pub memory: PathBuf,
    /// The guest's output mode, such as `2560x1440@60`, handed over as the
    /// `fw_cfg` item `opt/gse/screen`.
    pub mode: String,
    /// The PulseAudio socket of the host user's sound server. QEMU opens it
    /// while still root, so it is named rather than looked up in a runtime
    /// directory, which libpulse refuses to use for another user.
    pub sound_server: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    None,
    /// A virtio card backed by the passt instance listening on `socket`;
    /// see [`crate::net`] for what the environment can reach through it.
    Passt {
        socket: PathBuf,
    },
}

impl VmSpec {
    /// The cache option of the writable disks.
    fn cache(&self) -> &'static str {
        if self.direct_io { ",cache=none" } else { "" }
    }

    /// The firmware and the disks.
    fn disks(&self) -> Vec<String> {
        let mut drives = vec![
            format!("if=pflash,format=qcow2,readonly=on,file={OVMF_CODE}"),
            format!(
                "if=pflash,format=qcow2,file={}",
                self.firmware_vars.display()
            ),
            format!(
                "file={},if=virtio,format=qcow2{}",
                self.disk.display(),
                self.cache()
            ),
        ];
        for disk in &self.read_only_disks {
            drives.push(format!(
                "file={},if=virtio,format=raw,readonly=on",
                disk.display()
            ));
        }
        let mut args: Vec<String> = drives
            .into_iter()
            .flat_map(|drive| ["-drive".to_owned(), drive])
            .collect();
        // The serial belongs to the device: `-drive` no longer takes one.
        if let Some(home) = &self.home_disk {
            args.extend([
                "-drive".to_owned(),
                format!(
                    "if=none,id=home,file={},format=qcow2{}",
                    home.display(),
                    self.cache()
                ),
                "-device".to_owned(),
                "virtio-blk-pci,drive=home,serial=gse-home".to_owned(),
            ]);
        }
        args
    }

    /// The command line, program first.
    #[must_use]
    pub fn command_line(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        let mut push = |parts: &[&str]| args.extend(parts.iter().map(|s| (*s).to_owned()));

        push(&[
            QEMU,
            "-name",
            &self.name,
            "-machine",
            "q35,accel=kvm",
            "-cpu",
            "host,topoext=on",
        ]);
        let smp = if self.cpus >= 2 && self.cpus.is_multiple_of(2) {
            format!("{},sockets=1,cores={},threads=2", self.cpus, self.cpus / 2)
        } else {
            self.cpus.to_string()
        };
        push(&["-smp", &smp, "-m", &format!("{}M", self.memory_mib)]);
        let disks = self.disks();
        push(&disks.iter().map(String::as_str).collect::<Vec<_>>());
        for function in &self.passthrough {
            push(&["-device", &format!("vfio-pci,host={function}")]);
        }
        match &self.network {
            Network::None => push(&["-nic", "none"]),
            Network::Passt { socket } => push(&[
                "-netdev",
                &format!(
                    "stream,id=net0,server=off,addr.type=unix,addr.path={}",
                    socket.display()
                ),
                "-device",
                "virtio-net-pci,netdev=net0",
            ]),
        }
        match self.vnc_display {
            Some(display) => push(&[
                "-device",
                "virtio-vga",
                "-display",
                &format!("vnc=127.0.0.1:{display}"),
            ]),
            None => push(&["-vga", "none", "-display", "none"]),
        }
        let screen_and_input = self.screen_and_input();
        push(
            &screen_and_input
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        if self.image_build {
            push(&["-fw_cfg", "name=opt/gse/build,string=1"]);
        }
        if let Some(keyboard) = &self.keyboard {
            push(&[
                "-fw_cfg",
                &format!("name=opt/gse/keyboard,file={}", keyboard.display()),
            ]);
        }
        if let Some(guest) = &self.guest_program {
            push(&[
                "-drive",
                &format!(
                    "if=none,id=gseguest,file={},format=raw,readonly=on",
                    guest.image.display()
                ),
                "-device",
                "virtio-blk-pci,drive=gseguest,serial=gse-guest",
                "-fw_cfg",
                &format!("name=opt/gse/guest-size,string={}", guest.size),
                "-fw_cfg",
                &format!("name=opt/gse/guest-sha256,string={}", guest.sha256),
            ]);
        }
        // Appended to, so the service can start a log over that grew too
        // large while QEMU goes on writing.
        push(&[
            "-chardev",
            &format!(
                "file,id=console,path={},append=on",
                self.serial_log.display()
            ),
            "-serial",
            "chardev:console",
        ]);
        push(&[
            "-monitor",
            &format!("unix:{},server,nowait", self.monitor_socket.display()),
        ]);
        if let Some(user) = &self.run_as {
            push(&["-run-with", &format!("user={user}")]);
        }
        args
    }

    /// The shared screen memory and the input devices and socket of the
    /// environment screen shown on the host.
    fn screen_and_input(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        let mut push = |parts: &[&str]| args.extend(parts.iter().map(|s| (*s).to_owned()));
        if let Some(screen) = &self.screen {
            push(&[
                "-object",
                &format!(
                    "memory-backend-file,id=screen,mem-path={},size={},share=on",
                    screen.memory.display(),
                    gse_screen::SIZE
                ),
                "-device",
                "ivshmem-plain,memdev=screen",
                "-fw_cfg",
                &format!("name=opt/gse/screen,string={}", screen.mode),
                // A playback and a capture stream: the host's microphone
                // reaches the guest, for voice chat. virtio-sound rather
                // than an emulated Intel HDA, whose DMA timing the guest's
                // PipeWire follows unevenly and plays with dropouts.
                "-audiodev",
                &format!(
                    "pa,id=audio,server=unix:{},out.stream-name=gse-{},in.stream-name=gse-{}",
                    screen.sound_server.display(),
                    self.name,
                    self.name
                ),
                "-device",
                "virtio-sound-pci,audiodev=audio,streams=2",
            ]);
        }
        if let Some(socket) = &self.agent_socket {
            push(&[
                "-device",
                "virtio-serial-pci,id=serial",
                "-chardev",
                &format!(
                    "socket,id=agent,path={},server=on,wait=off",
                    socket.display()
                ),
                "-device",
                &format!(
                    "virtserialport,bus=serial.0,chardev=agent,name={}",
                    gse_protocol::agent::PORT
                ),
            ]);
            if let Some(journal) = &self.journal_log {
                push(&[
                    "-chardev",
                    &format!("file,id=journal,path={},append=on", journal.display()),
                    "-device",
                    &format!(
                        "virtserialport,bus=serial.0,chardev=journal,name={}",
                        gse_protocol::agent::JOURNAL_PORT
                    ),
                ]);
            }
        }
        // Grabbing any of them grabs all, and ctrl-ctrl lets all go.
        for (index, input) in self.host_input.iter().enumerate() {
            push(&[
                "-object",
                &format!(
                    "input-linux,id=host-input{index},evdev={},grab_all={on},repeat={on},grab-toggle=ctrl-ctrl",
                    input.device.display(),
                    on = if input.keyboard { "on" } else { "off" }
                ),
            ]);
        }
        if let Some(socket) = &self.input_socket {
            push(&[
                "-device",
                "virtio-keyboard-pci",
                "-device",
                "virtio-tablet-pci",
                "-device",
                "virtio-mouse-pci",
                "-chardev",
                &format!(
                    "socket,id=input,path={},server=on,wait=off",
                    socket.display()
                ),
            ]);
            if self.qmp_object {
                push(&["-object", "monitor-qmp,id=input-monitor,chardev=input"]);
            } else {
                push(&["-mon", "chardev=input,mode=control"]);
            }
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> VmSpec {
        VmSpec {
            name: "proto".into(),
            disk: "/vm/disk.qcow2".into(),
            home_disk: None,
            read_only_disks: vec!["/vm/seed.img".into()],
            firmware_vars: "/vm/OVMF_VARS.qcow2".into(),
            memory_mib: 12288,
            cpus: 8,
            passthrough: vec![
                "0000:0e:00.0".parse().unwrap(),
                "0000:0e:00.1".parse().unwrap(),
            ],
            network: Network::None,
            vnc_display: None,
            screen: None,
            input_socket: None,
            agent_socket: None,
            monitor_socket: "/run/gse/proto.monitor".into(),
            serial_log: "/vm/console.log".into(),
            journal_log: None,
            host_input: Vec::new(),
            qmp_object: false,
            keyboard: None,
            run_as: Some("sovhut".into()),
            guest_program: None,
            direct_io: false,
            image_build: false,
        }
    }

    #[test]
    fn host_input_is_grabbed_together_and_only_keyboards_repeat() {
        let mut spec = spec();
        spec.host_input = vec![
            HostInput {
                device: "/dev/input/event2".into(),
                keyboard: true,
            },
            HostInput {
                device: "/dev/input/event5".into(),
                keyboard: false,
            },
        ];
        let args = spec.command_line();
        let objects = after(&args, "-object");
        assert!(objects.contains(
            &"input-linux,id=host-input0,evdev=/dev/input/event2,grab_all=on,repeat=on,grab-toggle=ctrl-ctrl"
        ));
        assert!(objects.contains(
            &"input-linux,id=host-input1,evdev=/dev/input/event5,grab_all=off,repeat=off,grab-toggle=ctrl-ctrl"
        ));
    }

    #[test]
    fn the_input_monitor_follows_what_qemu_has() {
        let mut spec = spec();
        spec.input_socket = Some("/run/gse/proto.input".into());
        let args = spec.command_line();
        assert_eq!(after(&args, "-mon"), ["chardev=input,mode=control"]);
        assert!(!args.iter().any(|a| a.contains("monitor-qmp")));

        spec.qmp_object = true;
        let args = spec.command_line();
        assert_eq!(after(&args, "-mon"), Vec::<&str>::new());
        assert!(after(&args, "-object").contains(&"monitor-qmp,id=input-monitor,chardev=input"));
    }

    fn after<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn the_guest_program_goes_on_a_disk_of_its_own() {
        let mut spec = spec();
        assert!(!spec.command_line().iter().any(|a| a.contains("gse-guest")));
        spec.guest_program = Some(GuestProgram {
            image: "/var/lib/gse/environments/x/guest-program.img".into(),
            size: 2_700_000,
            sha256: "ab12".into(),
        });
        let args = spec.command_line();
        assert!(after(&args, "-drive").contains(
            &"if=none,id=gseguest,file=/var/lib/gse/environments/x/guest-program.img,format=raw,readonly=on"
        ));
        assert!(
            after(&args, "-device").contains(&"virtio-blk-pci,drive=gseguest,serial=gse-guest")
        );
        let items = after(&args, "-fw_cfg");
        assert!(items.contains(&"name=opt/gse/guest-size,string=2700000"));
        assert!(items.contains(&"name=opt/gse/guest-sha256,string=ab12"));
    }

    #[test]
    fn only_disks_without_copy_on_write_skip_the_page_cache() {
        let mut spec = spec();
        spec.home_disk = Some("/vm/home.qcow2".into());
        let drives = |spec: &VmSpec| {
            after(&spec.command_line(), "-drive")
                .into_iter()
                .filter(|d| d.contains("disk.qcow2") || d.contains("home.qcow2"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert!(drives(&spec).iter().all(|d| !d.contains("cache=")));
        spec.direct_io = true;
        let direct = drives(&spec);
        assert_eq!(direct.len(), 2);
        assert!(direct.iter().all(|d| d.ends_with(",cache=none")));
    }

    #[test]
    fn passes_every_function_and_drops_privileges() {
        let args = spec().command_line();
        assert_eq!(args[0], QEMU);
        assert_eq!(
            after(&args, "-device"),
            ["vfio-pci,host=0000:0e:00.0", "vfio-pci,host=0000:0e:00.1"]
        );
        assert_eq!(after(&args, "-smp"), ["8,sockets=1,cores=4,threads=2"]);
        assert_eq!(after(&args, "-m"), ["12288M"]);
        assert_eq!(after(&args, "-run-with"), ["user=sovhut"]);
        assert!(
            after(&args, "-drive").contains(&"file=/vm/seed.img,if=virtio,format=raw,readonly=on")
        );
    }

    #[test]
    fn environment_has_no_network_and_no_emulated_display_by_default() {
        let args = spec().command_line();
        assert_eq!(after(&args, "-nic"), ["none"]);
        assert_eq!(after(&args, "-vga"), ["none"]);
        assert_eq!(after(&args, "-display"), ["none"]);
    }

    #[test]
    fn network_card_is_backed_by_passt() {
        let mut environment = spec();
        environment.network = Network::Passt {
            socket: "/run/gse/proto.passt".into(),
        };
        let args = environment.command_line();
        assert_eq!(after(&args, "-nic"), Vec::<&str>::new());
        assert_eq!(
            after(&args, "-netdev"),
            ["stream,id=net0,server=off,addr.type=unix,addr.path=/run/gse/proto.passt"]
        );
        assert!(after(&args, "-device").contains(&"virtio-net-pci,netdev=net0"));
    }

    #[test]
    fn development_display_is_vnc_on_loopback() {
        let mut dev = spec();
        dev.vnc_display = Some(1);
        let args = dev.command_line();
        assert_eq!(after(&args, "-display"), ["vnc=127.0.0.1:1"]);
        assert!(after(&args, "-device").contains(&"virtio-vga"));
    }

    #[test]
    fn environment_screen_is_shared_memory() {
        let mut environment = spec();
        environment.screen = Some(Screen {
            memory: "/dev/shm/gse-proto.screen".into(),
            mode: "1920x1080@60".into(),
            sound_server: "/run/user/1000/pulse/native".into(),
        });
        let args = environment.command_line();
        assert_eq!(
            after(&args, "-object"),
            [
                "memory-backend-file,id=screen,mem-path=/dev/shm/gse-proto.screen,size=134217728,share=on"
            ]
        );
        assert!(after(&args, "-device").contains(&"ivshmem-plain,memdev=screen"));
        assert_eq!(
            after(&args, "-fw_cfg"),
            ["name=opt/gse/screen,string=1920x1080@60"]
        );
        assert_eq!(
            after(&args, "-audiodev"),
            [
                "pa,id=audio,server=unix:/run/user/1000/pulse/native,out.stream-name=gse-proto,in.stream-name=gse-proto"
            ]
        );
        assert!(after(&args, "-device").contains(&"virtio-sound-pci,audiodev=audio,streams=2"));
    }

    #[test]
    fn image_builds_tell_the_guest() {
        let mut build = spec();
        assert_eq!(after(&build.command_line(), "-fw_cfg"), Vec::<&str>::new());
        build.image_build = true;
        assert_eq!(
            after(&build.command_line(), "-fw_cfg"),
            ["name=opt/gse/build,string=1"]
        );
    }

    #[test]
    fn odd_cpu_counts_are_plain_cpus() {
        let mut odd = spec();
        odd.cpus = 3;
        assert_eq!(after(&odd.command_line(), "-smp"), ["3"]);
    }
}
