//! USB devices of the host and the ones a sandbox takes.
//!
//! Each device gets a [`UsbKind`] from its interfaces and input devices,
//! and a sandbox takes the ones its settings choose
//! (`gse_protocol::Devices`): by default, on a single-GPU machine, where the
//! host session is gone while the sandbox runs, a USB microphone or headset,
//! a webcam, a gamepad; with another GPU, none. QEMU takes each whole
//! (`usb-host`), detaching the host's drivers from it. Keyboards and mice
//! are never taken: on a single GPU they reach the guest through
//! `input-linux`, since the way out of the sandbox is read from them (see
//! `VmSpec::host_input`). A gamepad whose touchpad also shows up as a mouse
//! is a gamepad, and that mouse goes with it.
//!
//! Detaching the drivers lasts until something binds them again, which QEMU
//! does not do when it is killed, so the service gives each device back by
//! enumerating it anew ([`give_back`]).

use gse_protocol::{Devices, HostUsb, UsbId, UsbKind};

use crate::{Result, Root};

const USB_DEVICES: &str = "/sys/bus/usb/devices";
/// What the kernel lists of every input device.
const INPUT_DEVICES: &str = "/proc/bus/input/devices";
/// `KEY_A`: an input device that has it is a keyboard, not a set of media
/// or power buttons.
const KEY_A: usize = 30;

/// USB interface classes.
const AUDIO: u8 = 0x01;
const MASS_STORAGE: u8 = 0x08;
const HUB: u8 = 0x09;
const VIDEO: u8 = 0x0e;
const WIRELESS: u8 = 0xe0;

/// A USB device of the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    /// Its name in sysfs, such as `1-2` or `3-1.4`.
    pub name: String,
    pub bus: u16,
    pub address: u16,
    pub id: UsbId,
    pub kind: UsbKind,
    /// What it calls itself, for people.
    pub label: String,
    /// Its input event nodes (`/dev/input/eventN`), which leave the host
    /// with it.
    pub events: Vec<String>,
}

/// An input device as [`INPUT_DEVICES`] lists it.
struct Input {
    sysfs: String,
    event: Option<String>,
    keyboard: bool,
    mouse: bool,
    joystick: bool,
}

fn inputs(text: &str) -> Vec<Input> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut sysfs = None;
            let mut handlers: Vec<&str> = Vec::new();
            let mut keys = "";
            for line in block.lines() {
                if let Some(path) = line.strip_prefix("S: Sysfs=") {
                    sysfs = Some(path.trim().to_owned());
                } else if let Some(list) = line.strip_prefix("H: Handlers=") {
                    handlers = list.split_whitespace().collect();
                } else if let Some(bits) = line.strip_prefix("B: KEY=") {
                    keys = bits;
                }
            }
            Some(Input {
                sysfs: sysfs?,
                event: handlers
                    .iter()
                    .find(|h| h.starts_with("event"))
                    .map(|e| format!("/dev/input/{e}")),
                keyboard: handlers.contains(&"kbd") && has_bit(keys, KEY_A),
                mouse: handlers.iter().any(|h| h.starts_with("mouse")),
                joystick: handlers.iter().any(|h| h.starts_with("js")),
            })
        })
        .collect()
}

/// Whether bit `bit` is set in a bitmap as [`INPUT_DEVICES`] prints it: hex
/// words of the machine's width, the most significant first.
fn has_bit(bitmap: &str, bit: usize) -> bool {
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let Some(word) = words.len().checked_sub(1 + bit / 64).map(|i| words[i]) else {
        return false;
    };
    u64::from_str_radix(word, 16).is_ok_and(|w| w & (1 << (bit % 64)) != 0)
}

/// Whether the sysfs path `path` lies inside the USB device `name`: it
/// passes through `name` and on into one of its interfaces (`name:1.0`).
fn inside(path: &str, name: &str) -> bool {
    let interface = format!("{name}:");
    path.split('/')
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] == name && pair[1].starts_with(&interface))
}

fn hex(root: &Root, path: &str) -> Option<u16> {
    u16::from_str_radix(root.read_opt(path)?.trim(), 16).ok()
}

/// What a device with these interface classes and input devices is. A
/// keyboard is a keyboard whatever else it has, a gamepad a gamepad even
/// with a touchpad mouse; a webcam has sound too.
fn kind(interfaces: &[u8], own: &[&Input]) -> UsbKind {
    let gamepad = own.iter().any(|i| i.joystick);
    if own.iter().any(|i| i.keyboard) || (own.iter().any(|i| i.mouse) && !gamepad) {
        UsbKind::Input
    } else if gamepad {
        UsbKind::Gamepad
    } else if interfaces.contains(&MASS_STORAGE) {
        UsbKind::Storage
    } else if interfaces.contains(&VIDEO) {
        UsbKind::Camera
    } else if interfaces.contains(&AUDIO) {
        UsbKind::Sound
    } else if interfaces.contains(&WIRELESS) {
        UsbKind::Bluetooth
    } else {
        UsbKind::Other
    }
}

/// The USB devices a sandbox whose settings name `devices` takes, on a
/// single-GPU machine when `single`.
#[must_use]
pub fn for_guest(root: &Root, devices: &Devices, single: bool) -> Vec<UsbDevice> {
    host_devices(root)
        .into_iter()
        .filter(|d| devices.takes_usb(&d.id, d.kind, single))
        .collect()
}

/// Every USB device plugged into the host but its hubs, as the settings
/// list them.
#[must_use]
pub fn listed(root: &Root) -> Vec<HostUsb> {
    host_devices(root)
        .into_iter()
        .map(|d| HostUsb {
            id: d.id,
            label: d.label,
            kind: d.kind,
        })
        .collect()
}

/// Every USB device plugged into the host but its hubs.
#[must_use]
pub fn host_devices(root: &Root) -> Vec<UsbDevice> {
    let inputs = inputs(&root.read_opt(INPUT_DEVICES).unwrap_or_default());
    let mut devices = Vec::new();
    for name in root.list(USB_DEVICES) {
        // Root hubs are `usbN`, interfaces `N-P:C.I`.
        if name.starts_with("usb") || name.contains(':') {
            continue;
        }
        let dir = format!("{USB_DEVICES}/{name}");
        let class = |file: &str| {
            root.read_opt(file)
                .and_then(|c| u8::from_str_radix(c.trim(), 16).ok())
        };
        if class(&format!("{dir}/bDeviceClass")) == Some(HUB) {
            continue;
        }
        let interfaces: Vec<u8> = root
            .list(USB_DEVICES)
            .into_iter()
            .filter(|i| i.starts_with(&format!("{name}:")))
            .filter_map(|i| class(&format!("{USB_DEVICES}/{i}/bInterfaceClass")))
            .collect();
        // A hub may say so only in its interface.
        if interfaces.contains(&HUB) {
            continue;
        }
        let own: Vec<&Input> = inputs.iter().filter(|i| inside(&i.sysfs, &name)).collect();
        let number = |file: &str| root.read_opt(format!("{dir}/{file}"))?.trim().parse().ok();
        let (Some(bus), Some(address)) = (number("busnum"), number("devnum")) else {
            continue;
        };
        devices.push(UsbDevice {
            label: root
                .read_opt(format!("{dir}/product"))
                .map_or_else(|| name.clone(), |p| p.trim().to_owned()),
            id: UsbId {
                vendor: hex(root, &format!("{dir}/idVendor")).unwrap_or_default(),
                product: hex(root, &format!("{dir}/idProduct")).unwrap_or_default(),
                serial: root
                    .read_opt(format!("{dir}/serial"))
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty()),
            },
            kind: kind(&interfaces, &own),
            events: own.iter().filter_map(|i| i.event.clone()).collect(),
            name,
            bus,
            address,
        });
    }
    devices
}

/// Gives the device `name` back to the host's drivers by enumerating it
/// anew: deauthorizing it unbinds whatever holds it, and authorizing it
/// probes its drivers again.
pub fn give_back(root: &Root, name: &str) -> Result<()> {
    let authorized = format!("{USB_DEVICES}/{name}/authorized");
    root.write(&authorized, "0")?;
    root.write(&authorized, "1")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn add_device(root: &Root, name: &str, class: &str, interfaces: &[&str], product: &str) {
        let dir = root.path(format!("{USB_DEVICES}/{name}"));
        fs::create_dir_all(&dir).unwrap();
        let (bus, address) = name.split_once('-').unwrap();
        fs::write(dir.join("busnum"), format!("{bus}\n")).unwrap();
        fs::write(dir.join("devnum"), format!("{}\n", address.len() + 3)).unwrap();
        fs::write(dir.join("bDeviceClass"), format!("{class}\n")).unwrap();
        fs::write(dir.join("idVendor"), "0c76\n").unwrap();
        fs::write(dir.join("idProduct"), "161e\n").unwrap();
        fs::write(dir.join("product"), format!("{product}\n")).unwrap();
        fs::write(dir.join("authorized"), "1\n").unwrap();
        for (i, class) in interfaces.iter().enumerate() {
            let dir = root.path(format!("{USB_DEVICES}/{name}:1.{i}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("bInterfaceClass"), format!("{class}\n")).unwrap();
        }
    }

    /// A machine like the development PC plus a gamepad and a webcam: a
    /// keyboard, a mouse with macro keys, a USB microphone with media keys,
    /// a hub, a flash drive.
    fn machine() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        add_device(&root, "5-3", "00", &["03", "03", "03"], "Keychron Q6 Max");
        add_device(&root, "5-4", "00", &["03", "03"], "G502 X");
        add_device(
            &root,
            "1-2",
            "00",
            &["01", "01", "03"],
            "USB PnP Audio Device",
        );
        add_device(&root, "1-1", "09", &["09"], "USB2734");
        add_device(&root, "1-1.4", "00", &["09"], "Hub Controller");
        add_device(
            &root,
            "1-1.5",
            "00",
            &["03"],
            "DualSense Wireless Controller",
        );
        add_device(&root, "3-1", "ef", &["0e", "0e", "01", "01"], "HD Webcam");
        add_device(&root, "3-6", "00", &["08"], "Flash Disk");
        let usb = "/devices/pci0000:00/0000:00:08.1/0000:10:00.3";
        let text = format!(
            "N: Name=\"Keychron Q6 Max Keyboard\"\n\
             S: Sysfs={usb}/usb5/5-3/5-3:1.2/0003:3434:0860.0005/input/input6\n\
             H: Handlers=sysrq kbd leds event6\n\
             B: KEY=1000000000007 ff800000000007ff febeffdfffefffff fffffffffffffffe\n\
             \n\
             N: Name=\"Logitech G502 X\"\n\
             S: Sysfs={usb}/usb5/5-4/5-4:1.0/0003:046D:C099.0008/input/input8\n\
             H: Handlers=mouse1 event8\n\
             B: KEY=1f0000 0 0 0 0\n\
             \n\
             N: Name=\"USB PnP Audio Device\"\n\
             S: Sysfs={usb}/usb1/1-2/1-2:1.2/0003:0C76:161E.0007/input/input7\n\
             H: Handlers=kbd event7\n\
             B: KEY=7000000000 0 0 0\n\
             \n\
             N: Name=\"DualSense Wireless Controller\"\n\
             S: Sysfs={usb}/usb1/1-1/1-1.5/1-1.5:1.3/0003:054C:0CE6.000A/input/input30\n\
             H: Handlers=event28 js0\n\
             B: KEY=7fdb000000000000 0 0 0 0\n\
             \n\
             N: Name=\"DualSense Wireless Controller Touchpad\"\n\
             S: Sysfs={usb}/usb1/1-1/1-1.5/1-1.5:1.3/0003:054C:0CE6.000A/input/input32\n\
             H: Handlers=mouse2 event30\n\
             B: KEY=2420 10000 0 0 0 0\n"
        );
        fs::create_dir_all(root.path("/proc/bus/input")).unwrap();
        fs::write(root.path(INPUT_DEVICES), text).unwrap();
        (dir, root)
    }

    #[test]
    fn microphones_webcams_and_gamepads_go_and_the_rest_stays() {
        let (_dir, root) = machine();
        let devices = for_guest(&root, &Devices::default(), true);
        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["1-1.5", "1-2", "3-1"]);
        // With another GPU the host keeps them unless the settings say.
        assert_eq!(for_guest(&root, &Devices::default(), false), Vec::new());
        let kinds: Vec<(String, UsbKind)> = listed(&root)
            .into_iter()
            .map(|d| (d.label, d.kind))
            .collect();
        assert!(kinds.contains(&("Flash Disk".into(), UsbKind::Storage)));
        assert!(kinds.contains(&("Keychron Q6 Max".into(), UsbKind::Input)));
        assert!(kinds.contains(&("HD Webcam".into(), UsbKind::Camera)));
        let mut chosen = Devices::default();
        let disk = listed(&root)
            .into_iter()
            .find(|d| d.kind == UsbKind::Storage)
            .unwrap();
        chosen.choose_usb(&disk.id, disk.kind, true, true);
        assert_eq!(for_guest(&root, &chosen, true).len(), 4);
        let gamepad = &devices[0];
        assert_eq!(gamepad.label, "DualSense Wireless Controller");
        // Its touchpad, a mouse, leaves the host with it.
        assert_eq!(gamepad.events, ["/dev/input/event28", "/dev/input/event30"]);
        let microphone = &devices[1];
        assert_eq!((microphone.bus, microphone.id.vendor), (1, 0x0c76));
    }

    #[test]
    fn a_device_lies_inside_only_its_own_path() {
        let path = "/devices/x/usb1/1-1/1-1.5/1-1.5:1.3/input/input30";
        assert!(inside(path, "1-1.5"));
        assert!(!inside(path, "1-1"));
    }

    #[test]
    fn giving_back_enumerates_the_device_anew() {
        let (_dir, root) = machine();
        give_back(&root, "1-2").unwrap();
        assert_eq!(
            root.read(format!("{USB_DEVICES}/1-2/authorized")).unwrap(),
            "1"
        );
    }
}
