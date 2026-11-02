//! Sends keyboard and mouse input to an environment through QEMU's QMP
//! socket, as events of the VM's virtual keyboard, tablet (absolute pointer)
//! and mouse (relative motion). QEMU routes each event to the device of its
//! kind. The app's window sends through it, and on a single-GPU machine the
//! service, which reads the host's keyboards and mice itself.
//!
//! Keys travel by their physical position, not by what the host's layout
//! makes of them: they arrive as evdev codes, which go to QEMU as XT
//! scancodes ("qnum"), so the environment's own layout applies inside it.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// QEMU serves its QMP socket to one client at a time; a second connection
/// waits, without a greeting, until the first one is gone.
const GREETING_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Input {
    stream: UnixStream,
}

impl Drop for Input {
    /// Ends the connection, which also ends the thread draining it, so QEMU
    /// is free for the next client.
    fn drop(&mut self) {
        self.stream.shutdown(Shutdown::Both).ok();
    }
}

impl Input {
    pub fn connect(socket: &Path) -> Result<Self> {
        let stream =
            UnixStream::connect(socket).with_context(|| format!("{}", socket.display()))?;
        stream.set_read_timeout(Some(GREETING_TIMEOUT))?;
        let mut replies = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        replies
            .read_line(&mut line)
            .context("the environment's input is busy with another client")?;
        if !line.contains("\"QMP\"") {
            bail!("{} is not a QMP socket", socket.display());
        }
        stream.set_read_timeout(None)?;
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

    /// Presses or releases the key with evdev code `code`; keys QEMU has no
    /// scancode for are dropped.
    pub fn key(&mut self, code: u32, down: bool) -> Result<()> {
        let Some(qnum) = qnum(code) else {
            return Ok(());
        };
        self.events(&[format!(
            r#"{{"type":"key","data":{{"down":{down},"key":{{"type":"number","data":{qnum}}}}}}}"#
        )])
    }
}

/// QEMU's scancode for an evdev key code: the XT set 1 code, with 0x80 added
/// for the keys an XT keyboard prefixes with 0xe0.
#[must_use]
pub fn qnum(code: u32) -> Option<u32> {
    Some(match code {
        // Escape through keypad dot share their numbers with set 1.
        1..=83 => code,
        86 => 0x56,  // the key between left shift and Z
        87 => 0x57,  // F11
        88 => 0x58,  // F12
        96 => 0x9c,  // keypad enter
        97 => 0x9d,  // right ctrl
        98 => 0xb5,  // keypad slash
        99 => 0xb7,  // print screen
        100 => 0xb8, // right alt
        102 => 0xc7, // home
        103 => 0xc8, // up
        104 => 0xc9, // page up
        105 => 0xcb, // left
        106 => 0xcd, // right
        107 => 0xcf, // end
        108 => 0xd0, // down
        109 => 0xd1, // page down
        110 => 0xd2, // insert
        111 => 0xd3, // delete
        113 => 0xa0, // mute
        114 => 0xae, // volume down
        115 => 0xb0, // volume up
        119 => 0xc6, // pause
        125 => 0xdb, // left meta
        126 => 0xdc, // right meta
        127 => 0xdd, // menu
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_keys_keep_their_number_and_extended_keys_gain_0x80() {
        assert_eq!(qnum(30), Some(30), "A");
        assert_eq!(qnum(1), Some(1), "Escape");
        assert_eq!(qnum(103), Some(0xc8), "Up");
        assert_eq!(qnum(97), Some(0x9d), "Right Ctrl");
        assert_eq!(qnum(240), None);
    }
}
