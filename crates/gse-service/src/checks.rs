//! The checks for outbound files: what an environment's Outbox may hand to
//! the host. Programs, scripts, shortcuts, documents that carry macros and
//! archives that could hide any of them stay inside, by their name and by
//! how their content starts, and a take has size limits. A file's start is
//! held back until it has been checked, so nothing of a refused file
//! reaches the client.

use std::io::{self, Write};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use gse_protocol::{Reply, send};

/// The most bytes one take hands over.
pub const MAX_TAKE: u64 = 8 << 30;
/// The most files one take hands over.
pub const MAX_TAKE_FILES: usize = 1000;
/// How much of a file's start the content check looks at.
const HEAD: usize = 64;

/// Starts of files that run, or hold what runs.
const MAGIC: [(&[u8], &str); 12] = [
    (b"\x7fELF", "a Linux program"),
    (b"MZ", "a Windows program"),
    (b"#!", "a script"),
    (b"L\0\0\0\x01\x14\x02\0", "a Windows shortcut"),
    (b"[Desktop Entry]", "a desktop shortcut"),
    (
        b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1",
        "an Office document or installer",
    ),
    (b"PK\x03\x04", "an archive"),
    (b"7z\xbc\xaf\x27\x1c", "an archive"),
    (b"Rar!\x1a\x07", "an archive"),
    (b"\x1f\x8b", "an archive"),
    (b"\xfd7zXZ\0", "an archive"),
    (b"MSCF", "an archive"),
];

/// Why a file named `name` may not leave, if its name already says so:
/// [`gse_protocol::kept_by_name`], which the app marks its Outbox list by.
pub fn by_name(name: &str) -> Option<String> {
    gse_protocol::kept_by_name(name)?;
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(format!("a .{extension} file can run or start a program"))
}

/// Why a file starting with `head` may not leave, if it may not.
pub fn by_content(head: &[u8]) -> Option<String> {
    MAGIC
        .iter()
        .find(|(magic, _)| head.starts_with(magic))
        .map(|(_, what)| format!("it is {what}"))
}

/// Hands a file on as it arrives, once its start passed [`by_content`]:
/// to the client, or into an environment's spool of files taken while it
/// ran. A refused file is read to its end and thrown away, so the agent's
/// answers stay in step.
pub struct Forward<'a> {
    out: &'a mut dyn io::Write,
    path: String,
    size: u64,
    head: Vec<u8>,
    started: bool,
    pub refused: Option<String>,
}

impl<'a> Forward<'a> {
    pub fn new(out: &'a mut dyn io::Write, path: &str, size: u64) -> Self {
        Self {
            out,
            path: path.to_owned(),
            size,
            head: Vec::new(),
            started: false,
            refused: None,
        }
    }

    /// Whether any of the file reached the client.
    pub const fn started(&self) -> bool {
        self.started
    }

    fn chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        send(
            self.out,
            &Reply::FileChunk {
                data: BASE64.encode(bytes),
            },
        )
    }

    /// Checks the start once enough of it is there, or `force`d at the end.
    fn check(&mut self, force: bool) -> io::Result<()> {
        if self.started || self.refused.is_some() || (self.head.len() < HEAD && !force) {
            return Ok(());
        }
        if let Some(reason) = by_content(&self.head) {
            self.refused = Some(reason);
            self.head.clear();
            return Ok(());
        }
        send(
            self.out,
            &Reply::FileStart {
                path: self.path.clone(),
                size: self.size,
            },
        )?;
        self.started = true;
        let head = std::mem::take(&mut self.head);
        if !head.is_empty() {
            self.chunk(&head)?;
        }
        Ok(())
    }

    /// Ends the file: a short one is checked now.
    pub fn finish(&mut self) -> io::Result<()> {
        self.check(true)
    }
}

impl Write for Forward<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.refused.is_some() {
            return Ok(bytes.len());
        }
        if self.started {
            self.chunk(bytes)?;
        } else {
            self.head.extend_from_slice(bytes);
            self.check(false)?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_scripts_and_archives_are_refused() {
        assert!(by_name("game.exe").is_some());
        assert!(by_name("Save.LNK").is_some());
        assert!(by_name("slot.sav").is_none());
        assert!(by_name("README").is_none());
        assert!(by_content(b"\x7fELF\x02").is_some());
        assert!(by_content(b"MZ\x90\x00").is_some());
        assert!(by_content(b"#!/bin/sh\n").is_some());
        assert!(by_content(b"PK\x03\x04").is_some());
        assert!(by_content(b"SAVEGAME\x01\x02").is_none());
        assert!(by_content(b"\x89PNG\r\n").is_none());
    }

    fn take(content: &[u8]) -> (Vec<Reply>, Option<String>) {
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut forward = Forward::new(&mut ours, "slot.sav", content.len() as u64);
        for piece in content.chunks(10) {
            forward.write_all(piece).unwrap();
        }
        forward.finish().unwrap();
        let refused = forward.refused.clone();
        drop(ours);
        let mut reader = std::io::BufReader::new(theirs);
        let mut replies = Vec::new();
        while let Some(reply) = gse_protocol::receive::<Reply>(&mut reader).unwrap() {
            replies.push(reply);
        }
        (replies, refused)
    }

    #[test]
    fn a_file_reaches_the_client_only_once_its_start_passed() {
        let (replies, refused) = take(&[7; 200]);
        assert!(refused.is_none());
        assert!(matches!(replies[0], Reply::FileStart { size: 200, .. }));
        let bytes: Vec<u8> = replies[1..]
            .iter()
            .flat_map(|r| match r {
                Reply::FileChunk { data } => BASE64.decode(data).unwrap(),
                _ => panic!("not a chunk"),
            })
            .collect();
        assert_eq!(bytes, vec![7; 200]);

        let mut program = b"\x7fELF".to_vec();
        program.extend([0; 300]);
        let (replies, refused) = take(&program);
        assert!(refused.is_some());
        assert_eq!(replies, []);

        let (replies, refused) = take(b"tiny");
        assert!(refused.is_none());
        assert_eq!(replies.len(), 2);
    }
}
