//! Sends the window's keyboard and mouse to the environment through QEMU's QMP
//! socket, as events of the VM's virtual keyboard, tablet (absolute) and
//! mouse (relative). QEMU routes each event to the device of its kind.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;

use anyhow::{Context, Result, bail};
use winit::keyboard::KeyCode;

pub struct Input {
    stream: UnixStream,
}

impl Input {
    pub fn connect(socket: &Path) -> Result<Self> {
        let stream =
            UnixStream::connect(socket).with_context(|| format!("{}", socket.display()))?;
        let mut replies = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        replies.read_line(&mut line)?;
        if !line.contains("\"QMP\"") {
            bail!("{} is not a QMP socket", socket.display());
        }
        let mut input = Self { stream };
        input.send(r#"{"execute":"qmp_capabilities"}"#)?;
        // Replies and QEMU's events are of no interest; drain them so QEMU
        // never blocks on a full socket.
        thread::spawn(move || {
            let mut line = String::new();
            while replies.read_line(&mut line).is_ok_and(|n| n > 0) {
                line.clear();
            }
        });
        Ok(input)
    }

    fn send(&mut self, command: &str) -> Result<()> {
        self.stream.write_all(command.as_bytes())?;
        self.stream.write_all(b"\n")?;
        Ok(())
    }

    fn events(&mut self, events: &[String]) -> Result<()> {
        self.send(&format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{}]}}}}"#,
            events.join(",")
        ))
    }

    /// Moves the pointer to `(x, y)` in `0.0..=1.0` of the environment screen.
    pub fn pointer(&mut self, x: f64, y: f64) -> Result<()> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let axis = |name, v: f64| {
            format!(
                r#"{{"type":"abs","data":{{"axis":"{name}","value":{}}}}}"#,
                (v.clamp(0.0, 1.0) * 32767.0) as u32
            )
        };
        self.events(&[axis("x", x), axis("y", y)])
    }

    /// Moves the pointer by `(dx, dy)` on the relative mouse.
    pub fn motion(&mut self, dx: i32, dy: i32) -> Result<()> {
        let axis =
            |name, v: i32| format!(r#"{{"type":"rel","data":{{"axis":"{name}","value":{v}}}}}"#);
        self.events(&[axis("x", dx), axis("y", dy)])
    }

    /// `button` is QEMU's name: left, middle, right, side, extra, wheel-up,
    /// wheel-down.
    pub fn button(&mut self, button: &str, down: bool) -> Result<()> {
        self.events(&[format!(
            r#"{{"type":"btn","data":{{"down":{down},"button":"{button}"}}}}"#
        )])
    }

    pub fn key(&mut self, key: KeyCode, down: bool) -> Result<()> {
        let Some(code) = qcode(key) else {
            return Ok(());
        };
        self.events(&[format!(
            r#"{{"type":"key","data":{{"down":{down},"key":{{"type":"qcode","data":"{code}"}}}}}}"#
        )])
    }
}

/// QEMU's name for a key: one table, so its length is the key count.
#[allow(clippy::too_many_lines)]
fn qcode(key: KeyCode) -> Option<&'static str> {
    use KeyCode as K;
    Some(match key {
        K::KeyA => "a",
        K::KeyB => "b",
        K::KeyC => "c",
        K::KeyD => "d",
        K::KeyE => "e",
        K::KeyF => "f",
        K::KeyG => "g",
        K::KeyH => "h",
        K::KeyI => "i",
        K::KeyJ => "j",
        K::KeyK => "k",
        K::KeyL => "l",
        K::KeyM => "m",
        K::KeyN => "n",
        K::KeyO => "o",
        K::KeyP => "p",
        K::KeyQ => "q",
        K::KeyR => "r",
        K::KeyS => "s",
        K::KeyT => "t",
        K::KeyU => "u",
        K::KeyV => "v",
        K::KeyW => "w",
        K::KeyX => "x",
        K::KeyY => "y",
        K::KeyZ => "z",
        K::Digit0 => "0",
        K::Digit1 => "1",
        K::Digit2 => "2",
        K::Digit3 => "3",
        K::Digit4 => "4",
        K::Digit5 => "5",
        K::Digit6 => "6",
        K::Digit7 => "7",
        K::Digit8 => "8",
        K::Digit9 => "9",
        K::F1 => "f1",
        K::F2 => "f2",
        K::F3 => "f3",
        K::F4 => "f4",
        K::F5 => "f5",
        K::F6 => "f6",
        K::F7 => "f7",
        K::F8 => "f8",
        K::F9 => "f9",
        K::F10 => "f10",
        K::F11 => "f11",
        K::F12 => "f12",
        K::Escape => "esc",
        K::Tab => "tab",
        K::CapsLock => "caps_lock",
        K::Space => "spc",
        K::Enter => "ret",
        K::Backspace => "backspace",
        K::Insert => "insert",
        K::Delete => "delete",
        K::Home => "home",
        K::End => "end",
        K::PageUp => "pgup",
        K::PageDown => "pgdn",
        K::ArrowLeft => "left",
        K::ArrowRight => "right",
        K::ArrowUp => "up",
        K::ArrowDown => "down",
        K::ShiftLeft => "shift",
        K::ShiftRight => "shift_r",
        K::ControlLeft => "ctrl",
        K::ControlRight => "ctrl_r",
        K::AltLeft => "alt",
        K::AltRight => "alt_r",
        K::SuperLeft => "meta_l",
        K::SuperRight => "meta_r",
        K::ContextMenu => "menu",
        K::PrintScreen => "print",
        K::Pause => "pause",
        K::NumLock => "num_lock",
        K::Minus => "minus",
        K::Equal => "equal",
        K::BracketLeft => "bracket_left",
        K::BracketRight => "bracket_right",
        K::Backslash => "backslash",
        K::IntlBackslash => "less",
        K::Semicolon => "semicolon",
        K::Quote => "apostrophe",
        K::Backquote => "grave_accent",
        K::Comma => "comma",
        K::Period => "dot",
        K::Slash => "slash",
        K::Numpad0 => "kp_0",
        K::Numpad1 => "kp_1",
        K::Numpad2 => "kp_2",
        K::Numpad3 => "kp_3",
        K::Numpad4 => "kp_4",
        K::Numpad5 => "kp_5",
        K::Numpad6 => "kp_6",
        K::Numpad7 => "kp_7",
        K::Numpad8 => "kp_8",
        K::Numpad9 => "kp_9",
        K::NumpadDecimal => "kp_decimal",
        K::NumpadDivide => "kp_divide",
        K::NumpadMultiply => "kp_multiply",
        K::NumpadSubtract => "kp_subtract",
        K::NumpadAdd => "kp_add",
        K::NumpadEnter => "kp_enter",
        _ => return None,
    })
}
