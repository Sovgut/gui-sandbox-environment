//! The host's keyboards and mice on a single-GPU machine. The environment
//! owns the monitor there and the host session is gone, so QEMU grabs these
//! devices and gives them to the guest (see `VmSpec::host_input`).
//!
//! The way back is both Ctrl keys pressed together: QEMU then lets every
//! device go. The service reads the keyboards too, and a grabbed device
//! delivers nothing to any other reader, so the first key event the service
//! sees means the user asked to leave, and it stops the environment. The
//! guest never sees the devices, so it can neither fake this nor block it.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gse_host::vm::HostInput;
use tracing::{info, warn};

/// What the kernel lists of every input device.
const DEVICES: &str = "/proc/bus/input/devices";
/// `KEY_A`: a device that has it is a keyboard, not a power or media button.
const KEY_A: usize = 30;
/// `EV_KEY`, the type of a key or button event.
const EV_KEY: u16 = 1;
/// The size of a `struct input_event` on 64-bit Linux.
const EVENT_SIZE: usize = 24;
/// How long QEMU may take to grab the devices once it started; what the
/// keyboards deliver until then is the user still typing on the host.
const GRAB_SETTLE: Duration = Duration::from_secs(3);

/// Every keyboard and mouse of the host, by its event node.
pub fn discover() -> Vec<HostInput> {
    fs::read_to_string(DEVICES)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// The keyboards and mice in the text of [`DEVICES`].
fn parse(text: &str) -> Vec<HostInput> {
    let mut found = Vec::new();
    for block in text.split("\n\n") {
        let mut handlers: Vec<&str> = Vec::new();
        let mut keys = "";
        for line in block.lines() {
            if let Some(list) = line.strip_prefix("H: Handlers=") {
                handlers = list.split_whitespace().collect();
            } else if let Some(bits) = line.strip_prefix("B: KEY=") {
                keys = bits;
            }
        }
        let Some(event) = handlers.iter().find(|h| h.starts_with("event")) else {
            continue;
        };
        let keyboard = handlers.contains(&"kbd") && has_bit(keys, KEY_A);
        let mouse = handlers.iter().any(|h| h.starts_with("mouse"));
        if keyboard || mouse {
            found.push(HostInput {
                device: PathBuf::from("/dev/input").join(event),
                keyboard,
            });
        }
    }
    found
}

/// Whether bit `bit` is set in a bitmap as [`DEVICES`] prints it: hex words
/// of the machine's width, the most significant first.
fn has_bit(bitmap: &str, bit: usize) -> bool {
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let Some(word) = words.len().checked_sub(1 + bit / 64).map(|i| words[i]) else {
        return false;
    };
    u64::from_str_radix(word, 16).is_ok_and(|w| w & (1 << (bit % 64)) != 0)
}

/// Watches `keyboards` until `done`, and sets `stop` once one of them
/// delivers a key: QEMU let go of them, so the user pressed both Ctrl keys.
pub fn watch_for_exit(keyboards: &[PathBuf], stop: &AtomicBool, done: &AtomicBool) {
    let mut files: Vec<File> = keyboards
        .iter()
        .filter_map(|path| match File::open(path) {
            Ok(file) => Some(file),
            Err(e) => {
                warn!("watching {}: {e}", path.display());
                None
            }
        })
        .collect();
    if files.is_empty() {
        warn!("no keyboard to watch: only the guest's own shutdown ends this environment");
        return;
    }
    let started = Instant::now();
    let mut buffer = [0u8; EVENT_SIZE * 64];
    while !done.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst) {
        let mut fds: Vec<rustix::event::PollFd<'_>> = files
            .iter()
            .map(|file| rustix::event::PollFd::new(file, rustix::event::PollFlags::IN))
            .collect();
        let timeout = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 250_000_000,
        };
        if rustix::event::poll(&mut fds, Some(&timeout)).is_err() {
            continue;
        }
        let ready: Vec<bool> = fds
            .iter()
            .map(|fd| !fd.revents().is_empty())
            .collect();
        drop(fds);
        for (file, ready) in files.iter_mut().zip(ready) {
            if !ready {
                continue;
            }
            let Ok(read) = file.read(&mut buffer) else {
                continue;
            };
            let settled = started.elapsed() >= GRAB_SETTLE;
            let key = buffer[..read]
                .as_chunks::<EVENT_SIZE>()
                .0
                .iter()
                .any(|event| u16::from_ne_bytes([event[16], event[17]]) == EV_KEY);
            if key && settled {
                info!("the keyboard came back to the host: stopping the environment");
                stop.store(true, Ordering::SeqCst);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
I: Bus=0019 Vendor=0000 Product=0001 Version=0000
N: Name=\"Power Button\"
H: Handlers=kbd event0
B: EV=3
B: KEY=8000 10000000000000 0

I: Bus=0003 Vendor=3434 Product=0860 Version=0111
N: Name=\"Keychron Keychron Q6 Max\"
H: Handlers=sysrq kbd leds event2
B: EV=120013
B: KEY=1000000000007 ff9f207ac14057ff febeffdfffefffff fffffffffffffffe

I: Bus=0003 Vendor=046d Product=c547 Version=0111
N: Name=\"Logitech G502 X\"
H: Handlers=mouse0 event8
B: EV=17
B: KEY=1f0000 0 0 0 0

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"HDA NVidia HDMI/DP,pcm=3\"
H: Handlers=event9
B: EV=21
";

    #[test]
    fn finds_keyboards_and_mice_but_not_buttons() {
        assert_eq!(
            parse(SAMPLE),
            vec![
                HostInput {
                    device: "/dev/input/event2".into(),
                    keyboard: true,
                },
                HostInput {
                    device: "/dev/input/event8".into(),
                    keyboard: false,
                },
            ]
        );
    }

    #[test]
    fn bits_count_from_the_last_word() {
        assert!(has_bit("1 0", 64));
        assert!(!has_bit("1 0", 0));
        assert!(has_bit("4", 2));
        assert!(!has_bit("", 0));
    }
}
