//! The antivirus every file taken out of a sandbox passes: ClamAV, kept by
//! the service in [`ROOT`] and installed nowhere else. The service fetches
//! Cisco Talos's own Linux build from its GitHub releases, takes it only
//! with a valid signature of the Talos key pinned here, unpacks it into a
//! folder of its own, and updates it and its signature database itself,
//! once a day, apart from gse's own updates and the sandboxes' images.
//!
//! The scanner reads files that may be built to exploit it, so it never
//! runs as root: each run is a transient unit as [`ACCOUNT`], without
//! network, with the system read-only and only the files it scans and its
//! database in reach.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use tracing::{info, warn};

/// Where the engine, its database and its state live.
pub const ROOT: &str = "/var/lib/gse/clamav";
/// The account the scanner and the database updates run as.
pub const ACCOUNT: &str = "gse-scan";
/// How often the engine and the database are brought up to date.
pub const UPDATE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// A database older than this no longer counts as protection: a take
/// waits for an update instead.
const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const RELEASES: &str = "https://api.github.com/repos/Cisco-Talos/clamav/releases/latest";
/// The key Cisco Talos signs ClamAV releases with, from the ClamAV
/// documentation (`src/manual/cisco-talos.gpg` in Cisco-Talos/
/// clamav-documentation); fingerprint 5BAD CA26 65EF 59DC F8A2 3D8B
/// 707F 0DB4 8083 6771. Talos rotates it every couple of years; a release
/// signed with a newer key waits for a gse that pins it, and the installed
/// engine and its database updates go on meanwhile.
const TALOS_KEY: &str = include_str!("../../../dist/clamav/cisco-talos.asc");
/// The largest package taken from a release.
const MAX_PACKAGE: u64 = 300 << 20;

/// What the service knows of the antivirus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// The engine's version, such as `1.5.4`.
    pub engine: Option<String>,
    /// When the newest part of the database was written, in seconds since
    /// the epoch.
    pub database: Option<u64>,
    /// When the last update ran, and why it failed if it did.
    pub checked: Option<u64>,
    pub error: Option<String>,
}

pub struct Antivirus {
    root: PathBuf,
}

/// A file a scan found infected, and what with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub signature: String,
}

impl Antivirus {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn engine_dir(&self, version: &str) -> PathBuf {
        self.root.join(format!("engine-{version}"))
    }

    fn database(&self) -> PathBuf {
        self.root.join("db")
    }

    fn engine_record(&self) -> PathBuf {
        self.root.join("engine")
    }

    fn checked_record(&self) -> PathBuf {
        self.root.join("checked")
    }

    /// The installed engine's version.
    fn engine(&self) -> Option<String> {
        let version = fs::read_to_string(self.engine_record()).ok()?;
        let version = version.trim().to_owned();
        self.engine_dir(&version).is_dir().then_some(version)
    }

    pub fn state(&self) -> State {
        let newest = fs::read_dir(self.database())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|e| e == "cvd" || e == "cld")
            })
            .filter_map(|entry| entry.metadata().ok()?.modified().ok())
            .max();
        let (checked, error) = fs::read_to_string(self.checked_record())
            .ok()
            .map(|text| {
                let (time, error) = text.split_once('\n').unwrap_or((text.as_str(), ""));
                (
                    time.trim().parse().ok(),
                    Some(error.trim().to_owned()).filter(|e| !e.is_empty()),
                )
            })
            .unwrap_or_default();
        State {
            engine: self.engine(),
            database: newest.map(seconds),
            checked,
            error,
        }
    }

    /// Brings the engine and the database up to date, and notes when and
    /// how that went.
    pub fn update(&self) -> Result<()> {
        let outcome = self.update_engine().and_then(|()| self.update_database());
        let error = outcome
            .as_ref()
            .err()
            .map(|e| format!("{e:#}").replace('\n', " "))
            .unwrap_or_default();
        fs::write(
            self.checked_record(),
            format!("{}\n{error}\n", seconds(SystemTime::now())),
        )
        .ok();
        outcome
    }

    /// Installs the latest ClamAV release when it is not the installed one.
    fn update_engine(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o755))?;
        let http = crate::fedora::agent();
        let release: serde_json::Value = serde_json::from_str(
            &crate::fedora::text(&http, RELEASES).context("asking GitHub for the latest ClamAV")?,
        )?;
        let version = release["tag_name"]
            .as_str()
            .and_then(|tag| tag.strip_prefix("clamav-"))
            .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .context("the latest ClamAV release names no version")?
            .to_owned();
        if self.engine().as_deref() == Some(version.as_str()) {
            return Ok(());
        }
        let package = format!("clamav-{version}.linux.x86_64.rpm");
        let url_of = |name: &str| {
            release["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|asset| asset["name"] == name)
                .and_then(|asset| asset["browser_download_url"].as_str())
                .map(str::to_owned)
                .with_context(|| format!("ClamAV {version} has no {name}"))
        };
        let data = fetch(&http, &url_of(&package)?, MAX_PACKAGE)?;
        let signature = fetch(&http, &url_of(&format!("{package}.sig"))?, 64 << 10)?;
        verify(&data, &signature)
            .with_context(|| format!("checking the signature of {package}"))?;
        self.unpack(&version, &data)?;
        info!("ClamAV {version} is installed in {}", self.root.display());
        Ok(())
    }

    /// Unpacks the package `data` of `version` into its own folder, makes
    /// it the engine in use and removes the others.
    fn unpack(&self, version: &str, data: &[u8]) -> Result<()> {
        let work = self.root.join(".unpack");
        fs::remove_dir_all(&work).ok();
        fs::create_dir_all(&work)?;
        let rpm = work.join("package.rpm");
        fs::write(&rpm, data)?;
        let mut rpm2cpio = Command::new("rpm2cpio")
            .arg(&rpm)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting rpm2cpio")?;
        let cpio = Command::new("cpio")
            .args(["-idm", "--quiet", "--no-absolute-filenames"])
            .current_dir(&work)
            .stdin(rpm2cpio.stdout.take().context("rpm2cpio's output")?)
            .stderr(Stdio::null())
            .status()
            .context("starting cpio")?;
        if !rpm2cpio.wait()?.success() || !cpio.success() {
            bail!("the ClamAV package did not unpack");
        }
        let tree = work.join("usr/local");
        if !tree.join("bin/clamscan").is_file() || !tree.join("bin/freshclam").is_file() {
            bail!("the ClamAV package holds no clamscan and freshclam");
        }
        let target = self.engine_dir(version);
        fs::remove_dir_all(&target).ok();
        fs::rename(&tree, &target)?;
        // Under SELinux files in /var/lib carry a type nothing may run;
        // the engine's programs and libraries get the types of their kind.
        // Without SELinux these do nothing.
        for (part, kind) in [("bin", "bin_t"), ("sbin", "bin_t"), ("lib64", "lib_t")] {
            Command::new("chcon")
                .args(["-R", "-t", kind])
                .arg(target.join(part))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok();
        }
        fs::remove_dir_all(&work).ok();
        let record = self.root.join(".engine");
        fs::write(&record, format!("{version}\n"))?;
        fs::rename(&record, self.engine_record())?;
        for entry in fs::read_dir(&self.root)?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("engine-") && entry.path() != target {
                fs::remove_dir_all(entry.path()).ok();
            }
        }
        Ok(())
    }

    /// Updates the signature database with the engine's own freshclam, as
    /// [`ACCOUNT`].
    fn update_database(&self) -> Result<()> {
        let version = self.engine().context("no ClamAV engine is installed")?;
        let engine = self.engine_dir(&version);
        let database = self.database();
        fs::create_dir_all(&database)?;
        own(&database)?;
        let config = self.root.join("freshclam.conf");
        fs::write(
            &config,
            format!(
                "DatabaseDirectory {}\nDatabaseOwner {ACCOUNT}\nCVDCertsDirectory {}\n\
                 DatabaseMirror database.clamav.net\nForeground yes\nLogTime no\n",
                database.display(),
                engine.join("etc/certs").display()
            ),
        )?;
        let ran = self.run_confined(
            &engine,
            &[&database],
            true,
            &[
                engine.join("bin/freshclam").into_os_string(),
                format!("--config-file={}", config.display()).into(),
                "--stdout".into(),
            ],
        )?;
        if ran.code != Some(0) {
            bail!("freshclam failed: {}", problems(&ran.stdout, &ran.stderr));
        }
        Ok(())
    }

    /// Scans every file below `dir`, which [`ACCOUNT`] must be able to read;
    /// returns those it found infected. Without an engine or with a stale
    /// database it fails instead of passing anything.
    pub fn scan(&self, dir: &Path) -> Result<Vec<Found>> {
        let version = self.engine().context(
            "the antivirus is not installed yet; it downloads within a day of the service's start",
        )?;
        let fresh = self.state().database.is_some_and(|written| {
            seconds(SystemTime::now()).saturating_sub(written) < STALE_AFTER.as_secs()
        });
        if !fresh {
            bail!("the antivirus's signatures are missing or older than a week");
        }
        let engine = self.engine_dir(&version);
        let mut args = vec![
            engine.join("bin/clamscan").into_os_string(),
            format!("--database={}", self.database().display()).into(),
        ];
        args.extend(
            [
                "--infected",
                "--no-summary",
                "--stdout",
                "--recursive",
                "--max-filesize=4000M",
                "--max-scansize=4000M",
                "--alert-exceeds-max=yes",
            ]
            .map(std::ffi::OsString::from),
        );
        args.push(dir.as_os_str().to_owned());
        let ran = self.run_confined(&engine, &[], false, &args)?;
        match ran.code {
            Some(0) => Ok(Vec::new()),
            Some(1) => Ok(found(&String::from_utf8_lossy(&ran.stdout))),
            _ => bail!("clamscan failed: {}", last_line(&ran.stdout, &ran.stderr)),
        }
    }

    /// Runs `args` (a program of `engine` first) as [`ACCOUNT`] and waits
    /// for it. It runs as a transient unit: no privileges, the system
    /// read-only but for `writable`, no network unless `network`, the
    /// engine's own libraries first. Its output goes to files the manager
    /// opens, since handing pipes over D-Bus fails on some systems
    /// (Bazzite's manager drops the request). Where no transient unit can
    /// start at all, it runs as [`ACCOUNT`] directly, with fewer walls.
    fn run_confined(
        &self,
        engine: &Path,
        writable: &[&Path],
        network: bool,
        args: &[std::ffi::OsString],
    ) -> Result<Ran> {
        // Readable by all: the app's diagnostics carry the last run's
        // output, which holds nothing but the antivirus's own messages.
        let runs = self.root.join("runs");
        fs::create_dir_all(&runs)?;
        fs::set_permissions(&runs, fs::Permissions::from_mode(0o755))?;
        let (out, err) = (runs.join("stdout"), runs.join("stderr"));
        fs::remove_file(&out).ok();
        fs::remove_file(&err).ok();
        let libraries = engine.join("lib64");
        let mut unit = Command::new("systemd-run");
        unit.args([
            "--wait",
            "--quiet",
            "--collect",
            "--service-type=exec",
            &format!("--property=User={ACCOUNT}"),
            &format!("--property=Group={ACCOUNT}"),
            "--property=NoNewPrivileges=yes",
            "--property=ProtectSystem=strict",
            "--property=ProtectHome=yes",
            "--property=PrivateTmp=yes",
            "--property=PrivateDevices=yes",
            "--property=ProtectKernelTunables=yes",
            "--property=ProtectKernelModules=yes",
            "--property=ProtectControlGroups=yes",
            "--property=RestrictSUIDSGID=yes",
            "--property=LockPersonality=yes",
            "--property=MemoryMax=4G",
            &format!("--property=StandardOutput=truncate:{}", out.display()),
            &format!("--property=StandardError=truncate:{}", err.display()),
            &format!("--setenv=LD_LIBRARY_PATH={}", libraries.display()),
        ]);
        if let Some(bundle) = ca_bundle() {
            unit.arg(format!("--setenv=CURL_CA_BUNDLE={}", bundle.display()));
        }
        if !network {
            unit.arg("--property=PrivateNetwork=yes");
        }
        for path in writable {
            unit.arg(format!("--property=ReadWritePaths={}", path.display()));
        }
        let started = unit
            .arg("--")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("starting systemd-run")?;
        let manager = String::from_utf8_lossy(&started.stderr);
        for file in [&out, &err] {
            fs::set_permissions(file, fs::Permissions::from_mode(0o644)).ok();
        }
        if !manager.contains("Failed to start transient") {
            return Ok(Ran {
                code: started.status.code(),
                stdout: fs::read(&out).unwrap_or_default(),
                stderr: fs::read(&err).unwrap_or_else(|_| started.stderr.clone()),
            });
        }
        warn!(
            "no transient unit for the antivirus ({}); running it as {ACCOUNT} directly",
            manager.trim()
        );
        let (uid, gid) = account_ids()?;
        let (program, rest) = args.split_first().context("nothing to run")?;
        let output = {
            use std::os::unix::process::CommandExt as _;
            Command::new(program)
                .args(rest)
                .env_clear()
                .env("LD_LIBRARY_PATH", &libraries)
                .envs(ca_bundle().map(|bundle| ("CURL_CA_BUNDLE", bundle)))
                .env("PATH", "/usr/bin:/bin")
                .uid(uid)
                .gid(gid)
                .current_dir("/")
                .stdin(Stdio::null())
                .output()
                .context("starting the antivirus")?
        };
        Ok(Ran {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// How a run of the antivirus ended, and what it printed.
struct Ran {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// The system's bundle of trusted certificates. The engine's freshclam
/// looks for one where the system it was built on keeps it, which Fedora
/// does not, and then fails every download with "Connection failed".
fn ca_bundle() -> Option<PathBuf> {
    [
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/ssl/cert.pem",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

/// The user and group ids of [`ACCOUNT`].
fn account_ids() -> Result<(u32, u32)> {
    let id = |flag: &str| -> Result<u32> {
        let output = Command::new("id")
            .args([flag, ACCOUNT])
            .stderr(Stdio::null())
            .output()
            .context("starting id")?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .with_context(|| format!("there is no account {ACCOUNT}"))
    };
    Ok((id("-u")?, id("-g")?))
}

/// Gives `path` to [`ACCOUNT`].
fn own(path: &Path) -> Result<()> {
    let status = Command::new("chown")
        .arg(format!("{ACCOUNT}:{ACCOUNT}"))
        .arg(path)
        .status()
        .context("starting chown")?;
    if !status.success() {
        bail!("{} cannot be given to {ACCOUNT}", path.display());
    }
    Ok(())
}

/// Lets [`ACCOUNT`] read `dir` and everything below it, and nobody else
/// but root.
pub fn readable_by_scanner(dir: &Path) -> Result<()> {
    let status = Command::new("chgrp")
        .args(["-R", ACCOUNT])
        .arg(dir)
        .status()
        .context("starting chgrp")?;
    if !status.success() {
        bail!("{} cannot be handed to {ACCOUNT}", dir.display());
    }
    let status = Command::new("chmod")
        .args(["-R", "g+rX,o-rwx"])
        .arg(dir)
        .status()
        .context("starting chmod")?;
    if !status.success() {
        bail!("{} cannot be made readable by {ACCOUNT}", dir.display());
    }
    Ok(())
}

/// The files clamscan's `--infected` output names: `path: Signature FOUND`.
fn found(output: &str) -> Vec<Found> {
    output
        .lines()
        .filter_map(|line| line.strip_suffix(" FOUND"))
        .filter_map(|line| line.rsplit_once(": "))
        .map(|(path, signature)| Found {
            path: PathBuf::from(path),
            signature: signature.to_owned(),
        })
        .collect()
}

/// What freshclam said went wrong: its error and warning lines, which name
/// the cause before its closing "Update failed"; the last line without them.
fn problems(stdout: &[u8], stderr: &[u8]) -> String {
    let text = [stdout, stderr]
        .into_iter()
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join("\n");
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim) {
        if (line.starts_with("ERROR") || line.starts_with("WARNING")) && !lines.contains(&line) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        return last_line(stdout, stderr);
    }
    lines
        .iter()
        .rev()
        .take(4)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" / ")
}

/// The last line a program printed, for an error.
fn last_line(stdout: &[u8], stderr: &[u8]) -> String {
    let text = [stderr, stdout]
        .into_iter()
        .map(String::from_utf8_lossy)
        .find(|text| !text.trim().is_empty())
        .unwrap_or_default();
    text.lines().last().unwrap_or("no output").trim().to_owned()
}

/// The body at `url`, at most `limit` bytes.
fn fetch(http: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    http.get(url)
        .call()?
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut data)
        .with_context(|| format!("downloading {url}"))?;
    if data.len() as u64 > limit {
        bail!("{url} is larger than {limit} bytes");
    }
    Ok(data)
}

/// Checks that `signature` is the Talos key's signature of `data`.
fn verify(data: &[u8], signature: &[u8]) -> Result<()> {
    use pgp::composed::{Deserializable as _, DetachedSignature, SignedPublicKey};
    let (key, _) = SignedPublicKey::from_string(TALOS_KEY).context("reading the Talos key")?;
    // Talos publishes the signature armored; a binary one reads too.
    let signature = if signature.starts_with(b"-----BEGIN") {
        let text = std::str::from_utf8(signature).context("the signature is not text")?;
        DetachedSignature::from_string(text).map(|(signature, _)| signature)
    } else {
        DetachedSignature::from_bytes(signature)
    }
    .context("reading the signature")?;
    signature
        .verify(&key, data)
        .context("the signature is not the Talos key's")?;
    Ok(())
}

fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Updates the antivirus now and then every [`UPDATE_EVERY`], for as long
/// as the service runs.
pub fn keep_updated() {
    std::thread::spawn(|| {
        // The service starts with the machine; the network may not be up.
        std::thread::sleep(Duration::from_secs(60));
        let antivirus = Antivirus::new(ROOT);
        loop {
            match antivirus.update() {
                Ok(()) => info!("the antivirus is up to date"),
                Err(e) => warn!("updating the antivirus: {e:#}"),
            }
            std::thread::sleep(UPDATE_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn found_files_are_read_from_clamscan() {
        let output = "/var/lib/gse/scan/1/files/a b.exe: Win.Test.EICAR_HDB-1 FOUND\n\
                      /var/lib/gse/scan/1/files/x: Heuristics.Limits.Exceeded.MaxFileSize FOUND\n";
        assert_eq!(
            found(output),
            [
                Found {
                    path: "/var/lib/gse/scan/1/files/a b.exe".into(),
                    signature: "Win.Test.EICAR_HDB-1".into()
                },
                Found {
                    path: "/var/lib/gse/scan/1/files/x".into(),
                    signature: "Heuristics.Limits.Exceeded.MaxFileSize".into()
                },
            ]
        );
    }

    #[test]
    fn freshclam_says_why_it_failed() {
        let out = b"ClamAV update process started\nWARNING: Can't download daily.cvd from \
                    https://database.clamav.net/daily.cvd\nERROR: Received error code 403 or \
                    429\nERROR: Update failed.\n";
        assert_eq!(
            problems(out, b""),
            "WARNING: Can't download daily.cvd from https://database.clamav.net/daily.cvd / \
             ERROR: Received error code 403 or 429 / ERROR: Update failed."
        );
        assert_eq!(problems(b"something odd\n", b""), "something odd");
    }

    #[test]
    fn the_pinned_key_reads_and_rejects_what_it_did_not_sign() {
        use pgp::composed::{Deserializable as _, SignedPublicKey};
        use pgp::types::KeyDetails as _;
        let (key, _) = SignedPublicKey::from_string(TALOS_KEY).unwrap();
        let fingerprint = key
            .fingerprint()
            .as_bytes()
            .iter()
            .fold(String::new(), |mut hex, b| {
                use std::fmt::Write as _;
                write!(hex, "{b:02X}").ok();
                hex
            });
        assert_eq!(fingerprint, "5BADCA2665EF59DCF8A23D8B707F0DB480836771");
        assert!(verify(b"not signed", b"not a signature").is_err());
    }

    /// Checks a downloaded release against its signature:
    /// `GSE_CLAMAV_RPM=/path/clamav-X.linux.x86_64.rpm cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a downloaded ClamAV release"]
    fn a_real_release_verifies() {
        let rpm = std::env::var("GSE_CLAMAV_RPM").unwrap();
        let data = fs::read(&rpm).unwrap();
        let signature = fs::read(format!("{rpm}.sig")).unwrap();
        verify(&data, &signature).unwrap();
        let mut tampered = data.clone();
        tampered[1000] ^= 1;
        assert!(verify(&tampered, &signature).is_err());
    }
}
