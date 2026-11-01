//! The Fedora Cloud image the base image is built from.
//!
//! Fedora publishes the cloud images of a release in
//! `releases/<N>/Cloud/x86_64/images/`, beside a CHECKSUM file that names
//! each image with its size and SHA-256. The listing of that directory and
//! the CHECKSUM file are read from Fedora's own server over HTTPS. The image
//! comes through Fedora's redirector from whichever mirror it picks and
//! counts only once it matches the CHECKSUM, so a mirror cannot slip in
//! another image. The signature Fedora puts on the CHECKSUM file is not
//! checked: the file comes straight from Fedora, which HTTPS vouches for.
//!
//! A verified image stays in the cache, and the next build uses it again as
//! long as it still matches; the cache keeps nothing else.

use std::fs;
use std::io::{BufReader, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha2::{Digest as _, Sha256};
use tracing::{info, warn};

/// The Fedora release `guest/base/provision.sh` is written and tested for;
/// `fedora_release` in `/etc/gse/gse.toml` picks another.
pub const RELEASE: u32 = 45;

/// Fedora's own server, which the listings and CHECKSUM files come from.
const FEDORA: &str = "https://dl.fedoraproject.org/pub/fedora/linux/releases";
/// Fedora's redirector, which hands each download to a mirror; the images
/// come from there.
const MIRRORS: &str = "https://download.fedoraproject.org/pub/fedora/linux/releases";
/// The largest listing or CHECKSUM file read.
const MAX_TEXT: u64 = 1 << 20;
/// The largest image downloaded.
const MAX_IMAGE: u64 = 4 << 30;
/// Ends the name of an image while it downloads.
const PARTIAL: &str = ".part";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a download may go without a byte arriving.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// The least time between two reports of a download's progress.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// How a download goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Download {
    /// The image is not in the cache, or no longer matches; it downloads
    /// now.
    Started,
    /// This much of it has arrived, in percent.
    Percent(u8),
}

/// An image a CHECKSUM file names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Published {
    name: String,
    /// In lowercase hex.
    sha256: String,
    size: Option<u64>,
}

/// Where the listings and CHECKSUM files (`checksums`) and the images
/// (`images`) come from: each the `releases` directory of a Fedora tree.
struct Sources<'a> {
    checksums: &'a str,
    images: &'a str,
}

/// The Fedora Cloud Base Generic image of `release`, verified, in `cache`:
/// the cached copy when it still matches Fedora's CHECKSUM, otherwise a
/// fresh download, which `progress` follows. Setting `stop` ends a
/// download, and nothing of it stays.
pub fn fetch(
    cache: &Path,
    release: u32,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Download),
) -> Result<PathBuf> {
    fetch_from(&agent(), &FEDORA_SOURCES, cache, release, stop, progress)
}

/// Fedora's own tree for the checksums, its mirrors for the images.
const FEDORA_SOURCES: Sources<'static> = Sources {
    checksums: FEDORA,
    images: MIRRORS,
};

/// An HTTPS client that refuses plain HTTP, redirects included.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .https_only(true)
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .user_agent(concat!("gse/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// The directory of the cloud images of `release` below a `releases`
/// directory.
fn images_dir(release: u32) -> String {
    format!("{release}/Cloud/x86_64/images")
}

/// The Generic image of `release` as its CHECKSUM file describes it.
fn resolve(agent: &ureq::Agent, sources: &Sources<'_>, release: u32) -> Result<Published> {
    let directory = images_dir(release);
    let listing_url = format!("{}/{directory}/", sources.checksums);
    let listing = text(agent, &listing_url)
        .with_context(|| format!("listing the cloud images of Fedora {release}"))?;
    let checksum = checksum_file(&listing, release)
        .with_context(|| format!("{listing_url} lists no CHECKSUM file of Fedora {release}"))?;
    let checksum_url = format!("{}/{directory}/{checksum}", sources.checksums);
    let checksums = text(agent, &checksum_url)
        .with_context(|| format!("reading the checksums of Fedora {release}"))?;
    generic_image(&checksums, release)
        .with_context(|| format!("{checksum_url} names no Fedora Cloud Base Generic image"))
}

fn fetch_from(
    agent: &ureq::Agent,
    sources: &Sources<'_>,
    cache: &Path,
    release: u32,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Download),
) -> Result<PathBuf> {
    // A release not out yet has no published images; the one before it
    // serves until it is.
    let (image, release) = match resolve(agent, sources, release) {
        Ok(image) => (image, release),
        Err(e) => match resolve(agent, sources, release - 1) {
            Ok(image) => {
                warn!(
                    "Fedora {release} has no published cloud image ({e:#}); using {}",
                    release - 1
                );
                (image, release - 1)
            }
            Err(_) => return Err(e),
        },
    };
    fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
    let path = cache.join(&image.name);
    if path.is_file() {
        if sha256_of(&path)? == image.sha256 {
            info!("{} is in the cache and matches its checksum", image.name);
            keep_only(cache, &image.name);
            return Ok(path);
        }
        warn!(
            "{} no longer matches its checksum; downloading it again",
            path.display()
        );
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    progress(Download::Started);
    let url = format!("{}/{}/{}", sources.images, images_dir(release), image.name);
    download(agent, &url, &image, &path, stop, progress)
        .with_context(|| format!("downloading {}", image.name))?;
    keep_only(cache, &image.name);
    Ok(path)
}

/// The text at `url`, which may be at most [`MAX_TEXT`] bytes.
fn text(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut text = String::new();
    agent
        .get(url)
        .call()?
        .into_reader()
        .take(MAX_TEXT + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("reading {url}"))?;
    if text.len() as u64 > MAX_TEXT {
        bail!("{url} is larger than {MAX_TEXT} bytes");
    }
    Ok(text)
}

/// Whether `c` may stand in the name of a file Fedora publishes.
fn file_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// The CHECKSUM file of the cloud images of `release` that a listing of
/// their directory names, such as `Fedora-Cloud-45-1.4-x86_64-CHECKSUM`;
/// the newest compose when it names several.
fn checksum_file(listing: &str, release: u32) -> Option<String> {
    let prefix = format!("Fedora-Cloud-{release}-");
    listing
        .match_indices(&prefix)
        .filter(|(at, _)| {
            listing[..*at]
                .chars()
                .next_back()
                .is_none_or(|c| !file_name_char(c))
        })
        .filter_map(|(at, _)| {
            let name: String = listing[at..]
                .chars()
                .take_while(|&c| file_name_char(c))
                .collect();
            let compose = name
                .strip_prefix(&prefix)?
                .strip_suffix("-x86_64-CHECKSUM")?;
            let version: Vec<u32> = compose
                .split('.')
                .map(|part| part.parse().ok())
                .collect::<Option<_>>()?;
            Some((version, name))
        })
        .max()
        .map(|(_, name)| name)
}

/// The Generic qcow2 image of `release` that a CHECKSUM file names, with its
/// SHA-256 and, when the file gives it, its size. A signed CHECKSUM file
/// reads the same: the signature wraps the lines without changing them.
fn generic_image(checksums: &str, release: u32) -> Option<Published> {
    let prefix = format!("Fedora-Cloud-Base-Generic-{release}-");
    let (name, sha256) = checksums.lines().find_map(|line| {
        let (name, hash) = line.trim().strip_prefix("SHA256 (")?.split_once(") = ")?;
        let plain = name.starts_with(&prefix)
            && name.ends_with(".x86_64.qcow2")
            && name.chars().all(file_name_char);
        let hex = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        (plain && hex).then(|| (name.to_owned(), hash.to_ascii_lowercase()))
    })?;
    let size = checksums.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")?
            .strip_prefix(name.as_str())?
            .strip_prefix(": ")?
            .strip_suffix(" bytes")?
            .parse()
            .ok()
    });
    Some(Published { name, sha256, size })
}

fn sha256_of(path: &Path) -> Result<String> {
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut BufReader::with_capacity(1 << 20, file), &mut hash)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(format!("{:x}", hash.finalize()))
}

/// Downloads `image` from `url` into `path`. It arrives under a name of its
/// own and takes `path` only once all of it is there and matches.
fn download(
    agent: &ureq::Agent,
    url: &str,
    image: &Published,
    path: &Path,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Download),
) -> Result<()> {
    let partial = path.with_file_name(format!("{}{PARTIAL}", image.name));
    let outcome = receive(agent, url, image, &partial, stop, progress).and_then(|()| {
        fs::rename(&partial, path).with_context(|| format!("putting {} in place", path.display()))
    });
    if outcome.is_err() {
        fs::remove_file(&partial).ok();
    }
    outcome
}

/// Writes the image at `url` into `partial`, hashing it on the way, and
/// checks it against `image`.
fn receive(
    agent: &ureq::Agent,
    url: &str,
    image: &Published,
    partial: &Path,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Download),
) -> Result<()> {
    let response = agent.get(url).call()?;
    info!("downloading {}", response.get_url());
    let total = image
        .size
        .or_else(|| response.header("Content-Length")?.trim().parse().ok())
        .filter(|&total| total > 0);
    let limit = image.size.unwrap_or(MAX_IMAGE).min(MAX_IMAGE);
    let mut body = response.into_reader().take(limit + 1);
    let mut file =
        fs::File::create(partial).with_context(|| format!("creating {}", partial.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let (mut received, mut shown, mut shown_at) = (0_u64, 0_u8, Instant::now());
    loop {
        if stop.load(Ordering::SeqCst) {
            bail!("the download was stopped");
        }
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("receiving the image"),
        };
        received += read as u64;
        if received > limit {
            bail!("the image is larger than {limit} bytes");
        }
        hash.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .with_context(|| format!("writing {}", partial.display()))?;
        if let Some(total) = total {
            let percent = u8::try_from(received.saturating_mul(100) / total)
                .unwrap_or(100)
                .min(100);
            if percent > shown && shown_at.elapsed() >= REPORT_EVERY {
                progress(Download::Percent(percent));
                (shown, shown_at) = (percent, Instant::now());
            }
        }
    }
    if let Some(size) = image.size
        && size != received
    {
        bail!("the image ended after {received} of {size} bytes");
    }
    if format!("{:x}", hash.finalize()) != image.sha256 {
        bail!("the image does not match Fedora's checksum");
    }
    file.sync_all()
        .with_context(|| format!("writing {}", partial.display()))?;
    Ok(())
}

/// Removes every file in `cache` but `name`: images of another release or
/// compose, and downloads that never finished.
fn keep_only(cache: &Path, name: &str) {
    for entry in fs::read_dir(cache).into_iter().flatten().flatten() {
        if entry.file_name() != name && entry.file_type().is_ok_and(|t| t.is_file()) {
            info!("removing {}", entry.path().display());
            fs::remove_file(entry.path()).ok();
        }
    }
}

/// Removes the downloads in `cache` that never finished, which a service
/// that stopped in the middle of one leaves.
pub fn remove_partial(cache: &Path) {
    for entry in fs::read_dir(cache).into_iter().flatten().flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(PARTIAL))
        {
            fs::remove_file(entry.path()).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::BufRead as _;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    const GENERIC: &str = "Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2";
    const DIRECTORY: &str = "/releases/45/Cloud/x86_64/images";

    const LISTING: &str = r#"<html><body><h1>Index of /pub/fedora/linux/releases/45/Cloud/x86_64/images</h1>
<pre><a href="?C=N;O=D">Name</a> <a href="/pub/fedora/linux/releases/45/Cloud/x86_64/">Parent Directory</a>
<a href="Fedora-Cloud-45-1.4-x86_64-CHECKSUM">Fedora-Cloud-45-1.4-x86_64-CHECKSUM</a>  2026-10-28 15:34  2.2K
<a href="Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2">Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2</a>  2026-10-28 14:01  557M
<a href="Fedora-Cloud-Base-UEFI-UKI-45-1.4.x86_64.qcow2">Fedora-Cloud-Base-UEFI-UKI-45-1.4.x86_64.qcow2</a>  2026-10-28 14:00  602M
</pre></body></html>
"#;

    /// A CHECKSUM file as Fedora signs it, giving the Generic image
    /// `sha256` and `size`.
    fn signed_checksums(sha256: &str, size: usize) -> String {
        format!(
            "-----BEGIN PGP SIGNED MESSAGE-----\n\
             Hash: SHA256\n\
             \n\
             # Fedora-Cloud-Base-AmazonEC2-45-1.4.x86_64.raw.xz: 597552904 bytes\n\
             SHA256 (Fedora-Cloud-Base-AmazonEC2-45-1.4.x86_64.raw.xz) = \
             adca6974831a34271e778329adfb8ab3c279e25e0c6ffbda4844406592f8605e\n\
             # {GENERIC}: {size} bytes\n\
             SHA256 ({GENERIC}) = {sha256}\n\
             # Fedora-Cloud-Base-UEFI-UKI-45-1.4.x86_64.qcow2: 663486464 bytes\n\
             SHA256 (Fedora-Cloud-Base-UEFI-UKI-45-1.4.x86_64.qcow2) = \
             a67eaa189c4c00e47ff8b0886d3c7ae28986c8fdd3065f043188ea9517a5bcd3\n\
             -----BEGIN PGP SIGNATURE-----\n\
             \n\
             iQIzBAEBCAAdFiEENvYS3PJ/fRpIqDXk2/z3HG2fkKYFAmnrjXQACgkQ2/z3HG2f\n\
             =Kz1t\n\
             -----END PGP SIGNATURE-----\n"
        )
    }

    #[test]
    fn the_listing_names_the_checksum_file() {
        assert_eq!(
            checksum_file(LISTING, 45).as_deref(),
            Some("Fedora-Cloud-45-1.4-x86_64-CHECKSUM")
        );
        assert_eq!(checksum_file(LISTING, 44), None);
        assert_eq!(checksum_file(LISTING, 4), None);
        let two = format!("{LISTING}<a href=\"Fedora-Cloud-45-1.10-x86_64-CHECKSUM\">");
        assert_eq!(
            checksum_file(&two, 45).as_deref(),
            Some("Fedora-Cloud-45-1.10-x86_64-CHECKSUM")
        );
        assert_eq!(
            checksum_file(
                "Fedora-Cloud-45-x-x86_64-CHECKSUM MyFedora-Cloud-45-1.4-x86_64-CHECKSUM \
                 Fedora-Cloud-45--x86_64-CHECKSUM",
                45
            ),
            None
        );
    }

    #[test]
    fn the_checksum_file_names_the_generic_image() {
        let sha256 = "C1700B9E060544D6B2BF9629463D05B0F96EE6A2B31E44A1EE0DA95DC66B6A73";
        let checksums = signed_checksums(sha256, 603_717_632);
        assert_eq!(
            generic_image(&checksums, 45),
            Some(Published {
                name: GENERIC.into(),
                sha256: sha256.to_ascii_lowercase(),
                size: Some(603_717_632),
            })
        );
        assert_eq!(generic_image(&checksums, 44), None);
        assert_eq!(
            generic_image(&checksums.replace("# Fedora-Cloud-Base-Generic", "# x"), 45)
                .and_then(|image| image.size),
            None
        );
        for bad in [
            format!("SHA256 (../{GENERIC}) = {sha256}"),
            format!("SHA256 (Fedora-Cloud-Base-Generic-45-1.4/x.x86_64.qcow2) = {sha256}"),
            format!("SHA256 ({GENERIC}) = {}", &sha256[1..]),
            format!("SHA256 ({GENERIC}) = {}", sha256.replace('C', "G")),
            format!("SHA512 ({GENERIC}) = {sha256}"),
        ] {
            assert_eq!(generic_image(&bad, 45), None, "{bad}");
        }
    }

    /// Serves `files` by path over plain HTTP on a local port; the counts
    /// say how often each path was asked for.
    fn serve(files: HashMap<String, Vec<u8>>) -> (String, Arc<Mutex<HashMap<String, usize>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/releases", listener.local_addr().unwrap());
        let counts = Arc::new(Mutex::new(HashMap::new()));
        let counted = Arc::clone(&counts);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let mut header = String::new();
                while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
                    header.clear();
                }
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                *counted.lock().unwrap().entry(path.clone()).or_insert(0) += 1;
                let (status, body) = files.get(&path).map_or(("404 Not Found", &[][..]), |body| {
                    ("200 OK", body.as_slice())
                });
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).ok();
                stream.write_all(body).ok();
            }
        });
        (base, counts)
    }

    /// A Fedora tree on a local port with `image` as the Generic image of
    /// release 45, whose CHECKSUM file gives `sha256` and `size`.
    fn fedora(
        image: &[u8],
        sha256: &str,
        size: usize,
    ) -> (String, Arc<Mutex<HashMap<String, usize>>>) {
        serve(HashMap::from([
            (format!("{DIRECTORY}/"), LISTING.as_bytes().to_vec()),
            (
                format!("{DIRECTORY}/Fedora-Cloud-45-1.4-x86_64-CHECKSUM"),
                signed_checksums(sha256, size).into_bytes(),
            ),
            (format!("{DIRECTORY}/{GENERIC}"), image.to_vec()),
        ]))
    }

    fn image() -> Vec<u8> {
        (0..3 * 1024 * 1024_u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect()
    }

    fn fetch_local(
        base: &str,
        cache: &Path,
        stop: &AtomicBool,
        heard: &mut Vec<Download>,
    ) -> Result<PathBuf> {
        let sources = Sources {
            checksums: base,
            images: base,
        };
        let agent = ureq::AgentBuilder::new().build();
        fetch_from(&agent, &sources, cache, 45, stop, &mut |d| heard.push(d))
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_image_downloads_once_and_stays_while_it_matches() {
        let image = image();
        let sha256 = format!("{:x}", Sha256::digest(&image));
        let (base, counts) = fedora(&image, &sha256, image.len());
        let downloads = || {
            counts
                .lock()
                .unwrap()
                .get(&format!("{DIRECTORY}/{GENERIC}"))
                .copied()
                .unwrap_or(0)
        };
        let cache = tempfile::tempdir().unwrap();
        fs::write(
            cache
                .path()
                .join("Fedora-Cloud-Base-Generic-44-1.7.x86_64.qcow2"),
            b"an older release",
        )
        .unwrap();
        let stop = AtomicBool::new(false);
        let mut heard = Vec::new();

        let path = fetch_local(&base, cache.path(), &stop, &mut heard).unwrap();
        assert_eq!(path, cache.path().join(GENERIC));
        assert_eq!(fs::read(&path).unwrap(), image);
        assert_eq!(heard.first(), Some(&Download::Started));
        assert_eq!(files_in(cache.path()), [GENERIC]);
        assert_eq!(downloads(), 1);

        heard.clear();
        assert_eq!(
            fetch_local(&base, cache.path(), &stop, &mut heard).unwrap(),
            path
        );
        assert_eq!(heard, []);
        assert_eq!(downloads(), 1);

        fs::write(&path, b"damaged").unwrap();
        fetch_local(&base, cache.path(), &stop, &mut heard).unwrap();
        assert_eq!(fs::read(&path).unwrap(), image);
        assert_eq!(downloads(), 2);
    }

    #[test]
    fn an_image_that_does_not_check_out_is_not_kept() {
        let image = image();
        let sha256 = format!("{:x}", Sha256::digest(&image));
        let other = format!("{:x}", Sha256::digest(b"another image"));
        let stop = AtomicBool::new(false);
        for (base, _) in [
            fedora(&image, &other, image.len()),
            fedora(&image, &sha256, image.len() + 1),
            fedora(&image, &sha256, image.len() - 1),
        ] {
            let cache = tempfile::tempdir().unwrap();
            let outcome = fetch_local(&base, cache.path(), &stop, &mut Vec::new());
            assert!(outcome.is_err());
            assert_eq!(files_in(cache.path()), Vec::<String>::new());
        }

        let (base, _) = fedora(&image, &sha256, image.len());
        let cache = tempfile::tempdir().unwrap();
        let stopped = AtomicBool::new(true);
        let error = fetch_local(&base, cache.path(), &stopped, &mut Vec::new()).unwrap_err();
        assert!(format!("{error:#}").contains("stopped"), "{error:#}");
        assert_eq!(files_in(cache.path()), Vec::<String>::new());

        let (missing, _) = serve(HashMap::new());
        let error = fetch_local(&missing, cache.path(), &stop, &mut Vec::new()).unwrap_err();
        assert!(format!("{error:#}").contains("404"), "{error:#}");
    }

    /// Fedora publishes the release base images are built from where the
    /// service looks for it; `GSE_FEDORA_RELEASE` checks another release.
    /// It needs the internet, and the image itself is not downloaded.
    #[test]
    #[ignore = "reaches Fedora's servers"]
    fn fedora_publishes_the_release_where_it_is_looked_for() {
        let release = std::env::var("GSE_FEDORA_RELEASE")
            .ok()
            .and_then(|release| release.parse().ok())
            .unwrap_or(RELEASE);
        let agent = agent();
        let image = resolve(&agent, &FEDORA_SOURCES, release).unwrap();
        assert!(image.size.is_some(), "{image:?}");
        let url = format!("{MIRRORS}/{}/{}", images_dir(release), image.name);
        let response = agent.head(&url).call().unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.get_url().starts_with("https://"));
    }

    #[test]
    fn unfinished_downloads_are_cleared() {
        let cache = tempfile::tempdir().unwrap();
        fs::write(cache.path().join(format!("{GENERIC}{PARTIAL}")), b"half").unwrap();
        fs::write(cache.path().join(GENERIC), b"whole").unwrap();
        remove_partial(cache.path());
        assert_eq!(files_in(cache.path()), [GENERIC]);
        remove_partial(&cache.path().join("missing"));
    }
}
