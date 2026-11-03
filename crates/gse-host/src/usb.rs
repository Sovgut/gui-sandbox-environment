//! USB devices a sandbox takes on a single-GPU machine.
//!
//! There the host session is gone while the sandbox runs, so the devices it
//! would have used are free: a USB microphone or headset, a webcam, a
//! gamepad. QEMU takes each whole (`usb-host`), detaching the host's drivers
//! from it. Keyboards and mice stay with the host and reach the guest
//! through `input-linux` instead, since the way out of the sandbox is read
//! from them (see `VmSpec::host_input`); so do storage devices, which the
//! host may be copying files from. A gamepad whose touchpad also shows up as
//! a mouse goes as a gamepad, and that mouse with it.
//!
//! Detaching the drivers lasts until something binds them again, which QEMU
//! does not do when it is killed, so the service gives each device back by
//! enumerating it anew ([`give_back`]).

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

/// A USB device for a sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    /// Its name in sysfs, such as `1-2` or `3-1.4`.
    pub name: String,
    pub bus: u16,
    pub address: u16,
    pub vendor: u16,
    pub product: u16,
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

/// The USB devices a sandbox takes on a single-GPU machine: every one with
/// a sound or video interface, or that is a gamepad, unless it is also a
/// keyboard or holds storage.
#[must_use]
pub fn for_guest(root: &Root) -> Vec<UsbDevice> {
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
        let own: Vec<&Input> = inputs.iter().filter(|i| inside(&i.sysfs, &name)).collect();
        let media = interfaces.iter().any(|c| matches!(*c, AUDIO | VIDEO));
        let gamepad = own.iter().any(|i| i.joystick);
        let keyboard = own.iter().any(|i| i.keyboard);
        let storage = interfaces.contains(&MASS_STORAGE);
        let mouse_only = own.iter().any(|i| i.mouse) && !gamepad;
        if !(media || gamepad) || keyboard || storage || mouse_only {
            continue;
        }
        let number = |file: &str| root.read_opt(format!("{dir}/{file}"))?.trim().parse().ok();
        let (Some(bus), Some(address)) = (number("busnum"), number("devnum")) else {
            continue;
        };
        devices.push(UsbDevice {
            label: root
                .read_opt(format!("{dir}/product"))
                .map_or_else(|| name.clone(), |p| p.trim().to_owned()),
            vendor: hex(root, &format!("{dir}/idVendor")).unwrap_or_default(),
            product: hex(root, &format!("{dir}/idProduct")).unwrap_or_default(),
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
        let devices = for_guest(&root);
        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["1-1.5", "1-2", "3-1"]);
        let gamepad = &devices[0];
        assert_eq!(gamepad.label, "DualSense Wireless Controller");
        // Its touchpad, a mouse, leaves the host with it.
        assert_eq!(gamepad.events, ["/dev/input/event28", "/dev/input/event30"]);
        let microphone = &devices[1];
        assert_eq!((microphone.bus, microphone.vendor), (1, 0x0c76));
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
