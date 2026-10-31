//! A jail VM and the QEMU command line that runs it.

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
    /// The jail's writable disk, usually an overlay on a preset image.
    pub disk: PathBuf,
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
    /// the passed-through GPU is the jail's real screen.
    pub vnc_display: Option<u16>,
    /// QEMU's human monitor, used to ask the guest to power down.
    pub monitor_socket: PathBuf,
    pub serial_log: PathBuf,
    /// The account QEMU drops to once its devices are open.
    pub run_as: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    None,
    /// QEMU user networking with SSH forwarded to `127.0.0.1:<ssh_port>`.
    /// Development only: the guest reaches the host and the LAN through it.
    DevUserNet {
        ssh_port: u16,
    },
}

impl VmSpec {
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
        push(&[
            "-drive",
            &format!("if=pflash,format=qcow2,readonly=on,file={OVMF_CODE}"),
        ]);
        push(&[
            "-drive",
            &format!(
                "if=pflash,format=qcow2,file={}",
                self.firmware_vars.display()
            ),
        ]);
        push(&[
            "-drive",
            &format!("file={},if=virtio,format=qcow2", self.disk.display()),
        ]);
        for disk in &self.read_only_disks {
            push(&[
                "-drive",
                &format!("file={},if=virtio,format=raw,readonly=on", disk.display()),
            ]);
        }
        for function in &self.passthrough {
            push(&["-device", &format!("vfio-pci,host={function}")]);
        }
        match self.network {
            Network::None => push(&["-nic", "none"]),
            Network::DevUserNet { ssh_port } => push(&[
                "-nic",
                &format!("user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:{ssh_port}-:22"),
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
        push(&["-serial", &format!("file:{}", self.serial_log.display())]);
        push(&[
            "-monitor",
            &format!("unix:{},server,nowait", self.monitor_socket.display()),
        ]);
        if let Some(user) = &self.run_as {
            push(&["-run-with", &format!("user={user}")]);
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
            monitor_socket: "/run/gamejail/proto.monitor".into(),
            serial_log: "/vm/console.log".into(),
            run_as: Some("sovhut".into()),
        }
    }

    fn after<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
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
    fn jail_has_no_network_and_no_emulated_display_by_default() {
        let args = spec().command_line();
        assert_eq!(after(&args, "-nic"), ["none"]);
        assert_eq!(after(&args, "-vga"), ["none"]);
        assert_eq!(after(&args, "-display"), ["none"]);
    }

    #[test]
    fn development_options_add_vnc_and_ssh_forwarding() {
        let mut dev = spec();
        dev.network = Network::DevUserNet { ssh_port: 2222 };
        dev.vnc_display = Some(1);
        let args = dev.command_line();
        assert_eq!(
            after(&args, "-nic"),
            ["user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2222-:22"]
        );
        assert_eq!(after(&args, "-display"), ["vnc=127.0.0.1:1"]);
        assert!(after(&args, "-device").contains(&"virtio-vga"));
    }

    #[test]
    fn odd_cpu_counts_are_plain_cpus() {
        let mut odd = spec();
        odd.cpus = 3;
        assert_eq!(after(&odd.command_line(), "-smp"), ["3"]);
    }
}
