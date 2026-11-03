//! Talking to the gse service. Every call opens its own connection and
//! blocks, so the app runs them off the UI thread.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::channel::mpsc::UnboundedSender;
use gse_protocol::{
    BaseImage, Build, Environment, Event, FailedBuild, Gpu, GuestApp, GuestFiles, JournalEntry,
    Preset, Reply, Request, SOCKET, Settings, Stage, receive, send,
};

/// The service closed the connection without answering. A service older
/// than the app does that with a request it does not know.
#[derive(Debug)]
pub struct Unanswered;

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the service closed the connection")
    }
}

impl std::error::Error for Unanswered {}

fn connect() -> Result<(UnixStream, BufReader<UnixStream>)> {
    if crate::preview::active() {
        anyhow::bail!("a preview talks to no service");
    }
    let stream = UnixStream::connect(SOCKET)
        .with_context(|| format!("the gse service is not running ({SOCKET})"))?;
    let reader = BufReader::new(stream.try_clone()?);
    Ok((stream, reader))
}

/// Sends `request` and calls `each` with every reply until the service is
/// done with it.
fn call(request: &Request, mut each: impl FnMut(Reply)) -> Result<()> {
    let (mut stream, mut reader) = connect()?;
    send(&mut stream, request)?;
    loop {
        match receive::<Reply>(&mut reader)?.ok_or(Unanswered)? {
            Reply::Done => return Ok(()),
            Reply::Error { message } => return Err(anyhow!(message)),
            reply => each(reply),
        }
    }
}

/// Sends a request that answers with nothing but done or an error.
pub fn command(request: &Request) -> Result<()> {
    call(request, |reply| eprintln!("unexpected reply: {reply:?}"))
}

/// Sends files in with `request`, telling `progress` the bytes done and in
/// all as the service reports them. A send that was stopped gives how many
/// of its files went in whole, and how many it had.
pub fn send_files(
    request: &Request,
    mut progress: impl FnMut(u64, u64),
) -> Result<Option<(usize, usize)>> {
    let mut stopped = None;
    call(request, |reply| match reply {
        Reply::Sending { done, total } => progress(done, total),
        Reply::Stopped { copied, files } => stopped = Some((copied, files)),
        reply => eprintln!("unexpected reply: {reply:?}"),
    })?;
    Ok(stopped)
}

/// What the service says of its antivirus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Antivirus {
    /// The engine's version.
    pub engine: Option<String>,
    /// When its signatures were written and when it last updated, in
    /// seconds since the epoch.
    pub database: Option<u64>,
    pub checked: Option<u64>,
    /// Why the last update failed.
    pub error: Option<String>,
}

/// Where the sandboxes are kept, how many bytes they take and how many are
/// free beside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage {
    pub path: std::path::PathBuf,
    pub used: u64,
    pub free: u64,
}

pub fn storage() -> Result<Storage> {
    let mut found = None;
    call(&Request::Storage, |reply| {
        if let Reply::Storage { path, used, free } = reply {
            found = Some(Storage { path, used, free });
        }
    })?;
    found.context("no storage in the reply")
}

/// Moves the sandboxes to the disk of `to`, telling `progress` the bytes
/// moved and in all.
pub fn move_storage(to: &std::path::Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    call(
        &Request::MoveStorage { to: to.to_owned() },
        |reply| match reply {
            Reply::Moving { done, total } => progress(done, total),
            reply => eprintln!("unexpected reply: {reply:?}"),
        },
    )
}

pub fn antivirus() -> Result<Antivirus> {
    let mut found = None;
    call(&Request::Antivirus, |reply| {
        if let Reply::Antivirus {
            engine,
            database,
            checked,
            error,
        } = reply
        {
            found = Some(Antivirus {
                engine,
                database,
                checked,
                error,
            });
        }
    })?;
    found.context("no antivirus in the reply")
}

pub fn environments() -> Result<Vec<Environment>> {
    let mut found = None;
    call(&Request::List, |reply| {
        if let Reply::Environments { environments } = reply {
            found = Some(environments);
        }
    })?;
    found.context("no list in the reply")
}

pub fn images() -> Result<Vec<String>> {
    let mut found = None;
    call(&Request::Images, |reply| {
        if let Reply::Images { images } = reply {
            found = Some(images);
        }
    })?;
    found.context("no images in the reply")
}

/// How the running service compares with this app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    Same,
    /// The service is older: it answers with an older version, or does not
    /// know the handshake at all.
    ServiceOlder,
    ServiceNewer,
}

/// The release the service said it is when it last answered [`hello`].
static SERVICE_BUILD: Mutex<Option<String>> = Mutex::new(None);

/// The service's release as it last said it, such as `0.5.0`; none before
/// it answered, or from a service that does not know the handshake.
pub fn service_build() -> Option<String> {
    SERVICE_BUILD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Keeps the release the service said it is, for [`service_build`].
fn heard_build(build: Option<String>) {
    *SERVICE_BUILD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = build;
}

/// Asks the service for its protocol version, and keeps the release it
/// names for [`service_build`].
pub fn hello() -> Result<Match> {
    let mut version = None;
    let mut build = None;
    let outcome = call(&Request::Hello, |reply| {
        if let Reply::Hello {
            version: v,
            build: b,
        } = reply
        {
            version = Some(v);
            build = Some(b);
        }
    });
    match outcome {
        Err(e) if e.is::<Unanswered>() => {
            heard_build(None);
            return Ok(Match::ServiceOlder);
        }
        other => other?,
    }
    heard_build(build.clone());
    Ok(
        match version
            .context("no version in the reply")?
            .cmp(&gse_protocol::VERSION)
        {
            // One protocol, but the service may still be an older release
            // with fixes this app's copy has.
            std::cmp::Ordering::Equal => build_match(build.as_deref()),
            std::cmp::Ordering::Less => Match::ServiceOlder,
            std::cmp::Ordering::Greater => Match::ServiceNewer,
        },
    )
}

/// How the service's release compares with this app's, when both speak
/// one protocol; a build that is not a version counts as the same.
fn build_match(build: Option<&str>) -> Match {
    let parse = |v: &str| semver::Version::parse(v).ok();
    match (build.and_then(parse), parse(env!("CARGO_PKG_VERSION"))) {
        (Some(service), Some(app)) => match service.cmp(&app) {
            std::cmp::Ordering::Less => Match::ServiceOlder,
            std::cmp::Ordering::Greater => Match::ServiceNewer,
            std::cmp::Ordering::Equal => Match::Same,
        },
        _ => Match::Same,
    }
}

/// The service's settings and the GPUs they can name.
pub fn settings() -> Result<(Settings, Vec<Gpu>)> {
    let mut found = None;
    call(&Request::Settings, |reply| {
        if let Reply::Settings { settings, gpus } = reply {
            found = Some((settings, gpus));
        }
    })?;
    found.context("no settings in the reply")
}

/// The programs `guest` runs in the running sandbox `name`.
pub fn processes(name: &str) -> Result<Vec<GuestApp>> {
    let mut found = None;
    call(
        &Request::Processes {
            name: name.to_owned(),
        },
        |reply| {
            if let Reply::Processes { apps } = reply {
                found = Some(apps);
            }
        },
    )?;
    found.context("no programs in the reply")
}

/// What `guest`'s Inbox and Outbox in the running sandbox `name` hold.
pub fn files(name: &str) -> Result<GuestFiles> {
    let mut found = None;
    call(
        &Request::Files {
            name: name.to_owned(),
        },
        |reply| {
            if let Reply::Files { files } = reply {
                found = Some(files);
            }
        },
    )?;
    found.context("no files in the reply")
}

/// A piece of a take, as [`take`] hands it on.
pub enum Taking {
    Start { path: String },
    Chunk(Vec<u8>),
    Refused { path: String, reason: String },
}

/// Takes the Outbox of `name` past the service's checks, handing every
/// piece to `each`; the first error of `each` ends the take.
pub fn take(name: &str, mut each: impl FnMut(Taking) -> Result<()>) -> Result<()> {
    use base64::Engine as _;
    let mut failure = None;
    call(
        &Request::TakeFiles {
            name: name.to_owned(),
        },
        |reply| {
            if failure.is_some() {
                return;
            }
            let part = match reply {
                Reply::FileStart { path, .. } => Taking::Start { path },
                Reply::FileChunk { data } => {
                    match base64::engine::general_purpose::STANDARD.decode(data) {
                        Ok(bytes) => Taking::Chunk(bytes),
                        Err(e) => {
                            failure = Some(anyhow!("a chunk is not base64: {e}"));
                            return;
                        }
                    }
                }
                Reply::Refused { path, reason } => Taking::Refused { path, reason },
                _ => return,
            };
            if let Err(e) = each(part) {
                failure = Some(e);
            }
        },
    )?;
    failure.map_or(Ok(()), Err)
}

/// The latest journal entries of `name`, oldest first.
pub fn journal(name: &str) -> Result<Vec<JournalEntry>> {
    let mut found = None;
    call(
        &Request::Journal {
            name: name.to_owned(),
        },
        |reply| {
            if let Reply::Journal { entries } = reply {
                found = Some(entries);
            }
        },
    )?;
    found.context("no journal in the reply")
}

/// What the service says of the presets and their images.
pub struct PresetsState {
    pub presets: Vec<Preset>,
    /// The image build in progress, if any.
    pub build: Option<Build>,
    pub base: Option<BaseImage>,
    /// The presets whose images stand on an older base image.
    pub stale: Vec<String>,
    /// The last failed build of each image, until it builds again.
    pub failed: Vec<FailedBuild>,
}

/// The presets, the image build in progress, the base image, the images
/// built on an older one and the builds that failed.
pub fn presets() -> Result<PresetsState> {
    let mut found = None;
    call(&Request::Presets, |reply| {
        if let Reply::Presets {
            presets,
            build,
            base,
            stale,
            failed,
        } = reply
        {
            found = Some(PresetsState {
                presets,
                build,
                base,
                stale,
                failed,
            });
        }
    })?;
    found.context("no presets in the reply")
}

/// The presets the service ships with, to start new ones from.
pub fn preset_templates() -> Result<Vec<Preset>> {
    let mut found = None;
    call(&Request::PresetTemplates, |reply| {
        if let Reply::PresetTemplates { templates } = reply {
            found = Some(templates);
        }
    })?;
    found.context("no templates in the reply")
}

/// Builds the image of preset `name`, returning once the build ends. Its
/// steps also reach every watcher, which is where the app shows them.
pub fn build_preset(name: &str) -> Result<()> {
    call(
        &Request::BuildPreset {
            name: name.to_owned(),
        },
        |_| {},
    )
}

/// Builds the base image every set of apps is built on, downloading the
/// Fedora image it starts from when needed; returns when the build ends.
pub fn build_base() -> Result<()> {
    call(&Request::BuildBase, |_| {})
}

/// Starts `name` and calls `progress` with each stage until it is ready.
pub fn start(name: &str, mut progress: impl FnMut(Stage)) -> Result<()> {
    call(
        &Request::Start {
            name: name.to_owned(),
            keyboard: crate::keyboard::host(),
        },
        |reply| {
            if let Reply::Progress { stage } = reply {
                progress(stage);
            }
        },
    )
}

/// Forwards the service's events to `events` for as long as the app runs,
/// connecting again whenever the service goes away. `None` stands for "the
/// connection was lost or made", after which the app reloads its state.
pub fn watch(events: UnboundedSender<Option<Event>>) {
    thread::spawn(move || {
        loop {
            let outcome = (|| -> Result<()> {
                let (mut stream, mut reader) = connect()?;
                send(&mut stream, &Request::Watch)?;
                events.unbounded_send(None).ok();
                while let Some(reply) = receive::<Reply>(&mut reader)? {
                    match reply {
                        Reply::Event { event } => {
                            if events.unbounded_send(Some(event)).is_err() {
                                return Ok(());
                            }
                        }
                        Reply::Error { message } => bail!(message),
                        _ => {}
                    }
                }
                Ok(())
            })();
            if events.is_closed() {
                return;
            }
            if let Err(e) = outcome {
                eprintln!("service events: {e:#}");
            }
            events.unbounded_send(None).ok();
            thread::sleep(Duration::from_secs(2));
        }
    });
}
