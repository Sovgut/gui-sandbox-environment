//! Talking to a running environment's agent over its virtio-serial port
//! (`gse_protocol::agent`). The agent lives inside the environment, where
//! malware may have replaced it, so whatever it says is hostile: its lines
//! are capped by the protocol, its answers must carry the id of the call,
//! listings are cut to size and files must arrive with the size and the
//! hash the agent announced.

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use gse_protocol::agent::{self, Answer, Call, Message};
use gse_protocol::{GuestApp, GuestFiles, receive, send};
use sha2::{Digest as _, Sha256};

/// How long one answer may take.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// Lines with another call's id skipped before giving up on an answer.
const MAX_STRAY: usize = 64;
/// The most processes a listing may hold.
const MAX_PROCESSES: usize = 5000;

pub struct Agent {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
    /// The version the agent greeted with.
    version: u32,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

impl Agent {
    /// Connects to the agent behind `socket` and checks it speaks this
    /// service's version.
    pub fn connect(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .with_context(|| format!("the environment's agent ({})", socket.display()))?;
        stream.set_read_timeout(Some(ANSWER_TIMEOUT))?;
        stream.set_write_timeout(Some(ANSWER_TIMEOUT))?;
        let mut agent = Self {
            reader: BufReader::new(stream.try_clone()?),
            stream,
            next: 1,
            version: agent::MIN_VERSION,
        };
        match agent.ask(Call::Hello)? {
            Answer::Hello { version }
                if (agent::MIN_VERSION..=agent::VERSION).contains(&version) =>
            {
                agent.version = version;
                Ok(agent)
            }
            Answer::Hello { version } => {
                bail!(
                    "the environment's agent speaks version {version}, not {} to {}",
                    agent::MIN_VERSION,
                    agent::VERSION
                )
            }
            _ => bail!("the environment's agent did not greet"),
        }
    }

    fn call(&mut self, body: Call) -> Result<u64> {
        let id = self.next;
        self.next += 1;
        send(&mut self.stream, &Message { id, body })?;
        Ok(id)
    }

    /// The next answer to the call `id`; an error answer fails.
    fn answer(&mut self, id: u64) -> Result<Answer> {
        for _ in 0..MAX_STRAY {
            let message = receive::<Message<Answer>>(&mut self.reader)
                .context("reading the agent's answer")?
                .context("the agent hung up")?;
            if message.id != id {
                continue;
            }
            if let Answer::Error { message } = message.body {
                bail!("{}", message.chars().take(500).collect::<String>());
            }
            return Ok(message.body);
        }
        bail!("the agent answered other calls")
    }

    fn ask(&mut self, body: Call) -> Result<Answer> {
        let id = self.call(body)?;
        self.answer(id)
    }

    /// Tells the agent that `files` of `bytes` in all follow, kept for this
    /// start, so the guest shows them arriving. An agent before version 2
    /// takes them without a word.
    pub fn announce(&mut self, files: usize, bytes: u64) -> Result<()> {
        if self.version < 2 {
            return Ok(());
        }
        self.done(Call::Incoming {
            files: u32::try_from(files).unwrap_or(u32::MAX),
            bytes,
        })
    }

    fn done(&mut self, body: Call) -> Result<()> {
        match self.ask(body)? {
            Answer::Done => Ok(()),
            _ => bail!("the agent answered something else"),
        }
    }

    pub fn processes(&mut self) -> Result<Vec<GuestApp>> {
        let Answer::Processes { mut apps } = self.ask(Call::Processes)? else {
            bail!("the agent answered something else");
        };
        let mut left = MAX_PROCESSES;
        for app in &mut apps {
            app.processes.truncate(left);
            left -= app.processes.len();
        }
        apps.retain(|a| !a.processes.is_empty());
        Ok(apps)
    }

    pub fn end_process(&mut self, pid: u32, force: bool) -> Result<()> {
        self.done(Call::EndProcess { pid, force })
    }

    pub fn end_app(&mut self, unit: &str) -> Result<()> {
        self.done(Call::EndApp {
            unit: unit.to_owned(),
        })
    }

    pub fn files(&mut self) -> Result<GuestFiles> {
        let Answer::Files { mut files } = self.ask(Call::Files)? else {
            bail!("the agent answered something else");
        };
        files.inbox.truncate(agent::MAX_FILES);
        files.outbox.truncate(agent::MAX_FILES);
        Ok(files)
    }

    /// Puts `size` bytes from `from` into the Inbox at `path`, runnable when
    /// `executable`.
    pub fn put(
        &mut self,
        path: &str,
        from: &mut impl Read,
        size: u64,
        executable: bool,
    ) -> Result<()> {
        let limit = if self.version >= 2 {
            agent::MAX_SENT_FILE
        } else {
            agent::MAX_TAKEN_FILE
        };
        if size > limit {
            if limit < agent::MAX_SENT_FILE && size <= agent::MAX_SENT_FILE {
                bail!(
                    "{path} is larger than 4 GiB, which this sandbox's system takes at most; \
                     after Update everything it takes up to 64 GiB"
                );
            }
            bail!("{path} is larger than {limit} bytes");
        }
        let id = self.call(Call::Put {
            path: path.to_owned(),
            size,
            executable,
        })?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; agent::CHUNK];
        let mut sent = 0_u64;
        while sent < size {
            let read = match from.read(&mut buffer) {
                Ok(read) => read,
                Err(e) => {
                    // Any other call makes the agent drop the upload and
                    // the part of the file it wrote.
                    self.ask(Call::Hello).ok();
                    return Err(e.into());
                }
            };
            if read == 0 {
                break;
            }
            let read = read.min(usize::try_from(size - sent).unwrap_or(usize::MAX));
            hash.update(&buffer[..read]);
            sent += read as u64;
            send(
                &mut self.stream,
                &Message {
                    id,
                    body: Call::Chunk {
                        data: BASE64.encode(&buffer[..read]),
                    },
                },
            )?;
        }
        send(
            &mut self.stream,
            &Message {
                id,
                body: Call::PutEnd {
                    sha256: hex(&hash.finalize()),
                },
            },
        )?;
        match self.answer(id)? {
            Answer::Done if sent == size => Ok(()),
            Answer::Done => bail!("{path} was shorter than its size"),
            _ => bail!("the agent answered something else"),
        }
    }

    /// Copies the Outbox file at `path` into `to`, refusing more than
    /// `limit` bytes; returns its size.
    pub fn get(&mut self, path: &str, to: &mut impl Write, limit: u64) -> Result<u64> {
        let id = self.call(Call::Get {
            path: path.to_owned(),
        })?;
        let mut hash = Sha256::new();
        let mut received = 0_u64;
        loop {
            match self.answer(id)? {
                Answer::Chunk { data } => {
                    let bytes = BASE64.decode(data).context("a chunk is not base64")?;
                    received += bytes.len() as u64;
                    if received > limit.min(agent::MAX_TAKEN_FILE) {
                        bail!("{path} is larger than allowed");
                    }
                    hash.update(&bytes);
                    to.write_all(&bytes)?;
                }
                Answer::GetEnd { size, sha256 } => {
                    if size != received || sha256 != hex(&hash.finalize()) {
                        bail!("{path} arrived damaged");
                    }
                    return Ok(received);
                }
                _ => bail!("the agent answered something else"),
            }
        }
    }
}
