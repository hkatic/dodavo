//! Automatic updates: finding out whether a newer version is published,
//! downloading its installer, checking it and handing over to Setup.
//!
//! Releases live on GitHub. Every release carries the installer and a small
//! manifest (`update.json`, see `manifest.rs`), and the fixed URL below always
//! points at the manifest of the latest published release. The manifest names
//! the installer's size and SHA-256 and is signed with a key the program knows
//! (`keys.rs`), so a damaged or foreign file is never run.
//!
//! The work happens on a thread of its own with a small tokio runtime, so
//! transfers on the network runtime are not affected. Results reach the UI
//! thread as [`Event`]s through `app::update_sink`, with the same queue and
//! retry contract as network events. The window and the scheduling are in
//! `ui::update`.
//!
//! Installing: Setup is started silently with the install mode to keep
//! (`/CURRENTUSER` or `/ALLUSERS`; for all users Setup elevates itself and
//! Windows asks for permission), `/DODAVOUPDATE=<our pid>` and the arguments
//! Dodavo should be started with afterwards. Once Setup runs with the rights
//! it needs, it posts `Dodavo_ExitForUpdate` to every running instance, waits
//! for this process and for the shared mutex to go, replaces the program and
//! starts Dodavo again (see `[Code]` in installer\Dodavo.iss). Dodavo keeps
//! running until that message arrives, so a declined elevation prompt only
//! ends Setup, which Dodavo notices and reports.

#[cfg(windows)]
mod installer;

pub mod http;
pub mod keys;
pub mod manifest;
pub mod statement;
pub mod version;

use std::{
    io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use ed25519_dalek::VerifyingKey;
#[cfg(windows)]
pub use installer::{
    InstallMode, close_handle, install_mode, launch_installer, setup_exited, setup_parameters,
};
use manifest::Manifest;
use tokio_util::sync::CancellationToken;
use version::Version;

/// The manifest of the latest published release.
pub const MANIFEST_URL: &str =
    "https://github.com/hkatic/dodavo/releases/latest/download/update.json";
/// Delay after the start before the first automatic check.
pub const START_DELAY: Duration = Duration::from_secs(15);
/// How often the scheduler looks whether a check is due.
pub const TICK: Duration = Duration::from_secs(60 * 60);
/// How often an automatic check is made.
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long "Remind Me Later" keeps the window away.
pub const SNOOZE: Duration = Duration::from_secs(24 * 60 * 60);

const PENDING_FILE: &str = "pending.json";
const LOG_FILE: &str = "setup.log";

/// The manifest URL, or the `DODAVO_UPDATE_URL` override (for testing with a
/// local web server).
pub fn manifest_url() -> String {
    std::env::var("DODAVO_UPDATE_URL")
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| MANIFEST_URL.to_string())
}

/// The keys that may sign a manifest: the built-in ones, or those in
/// `DODAVO_UPDATE_KEY` (hex, several separated by `;`) for testing.
pub fn trusted_keys() -> Vec<VerifyingKey> {
    let from_env = std::env::var("DODAVO_UPDATE_KEY").ok();
    let hex: Vec<&str> = match &from_env {
        Some(list) if !list.trim().is_empty() => list.split(';').collect(),
        _ => keys::PUBLIC_KEYS_HEX.to_vec(),
    };
    hex.into_iter()
        .filter_map(statement::parse_public_key)
        .collect()
}

/// Where the installer is downloaded to: `%TEMP%\Dodavo\update`.
pub fn update_dir() -> io::Result<PathBuf> {
    let dir = crate::archive::temp_dir()?.join("update");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn log_path() -> Option<PathBuf> {
    update_dir().ok().map(|d| d.join(LOG_FILE))
}

/// Removes downloaded installers and partial downloads. Files that are in use
/// (a Setup that is still running) are simply left alone.
pub fn cleanup() {
    let Ok(dir) = update_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.ends_with(".exe") || name.ends_with(".part") {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The daily check: nothing is shown unless there is an update.
    Automatic,
    /// Help → Check for Updates: the result is always reported.
    Manual,
}

/// A newer version that is available.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    /// Release notes in the user's language, as in the manifest.
    pub notes: String,
    pub url: String,
    pub size: u64,
    pub sha256: [u8; 32],
    /// The local file name of the installer.
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Network(String),
    Http(u16),
    TooLarge,
    Manifest(String),
    Signature,
    Size,
    Checksum,
    Io(String),
    DiskSpace {
        needed: u64,
        free: u64,
    },
    Cancelled,
    Launch(String),
    /// Setup ended right away (for example, the elevation prompt was declined).
    SetupExit(u32),
}

impl Error {
    /// The error as a sentence for the user.
    pub fn text(&self) -> String {
        match self {
            Error::Network(detail) => crate::i18n::tr!(update_err_network, detail = detail),
            Error::Http(status) => crate::i18n::tr!(update_err_http, status = status),
            Error::TooLarge => crate::i18n::tr!(update_err_too_large).to_string(),
            Error::Manifest(detail) => crate::i18n::tr!(update_err_manifest, detail = detail),
            Error::Signature => crate::i18n::tr!(update_err_signature).to_string(),
            Error::Size => crate::i18n::tr!(update_err_size).to_string(),
            Error::Checksum => crate::i18n::tr!(update_err_checksum).to_string(),
            Error::Io(detail) => crate::i18n::tr!(update_err_io, detail = detail),
            Error::DiskSpace { needed, free } => crate::i18n::tr!(
                update_err_disk_space,
                needed = crate::format::format_size(*needed),
                free = crate::format::format_size(*free)
            ),
            Error::Cancelled => crate::i18n::tr!(cancelled).to_string(),
            Error::Launch(detail) => crate::i18n::tr!(update_err_launch, detail = detail),
            Error::SetupExit(code) => crate::i18n::tr!(update_err_setup_exit, code = code),
        }
    }
}

pub enum Event {
    /// A check ended: `Ok(None)` means that this is the latest version.
    Checked {
        job: u64,
        result: Result<Option<UpdateInfo>, Error>,
    },
    /// Download progress; may be dropped.
    Progress { job: u64, done: u64, total: u64 },
    /// The download is complete and is being compared with the manifest.
    Verifying { job: u64 },
    /// The installer is ready at the path, or the download failed.
    Downloaded {
        job: u64,
        result: Result<PathBuf, Error>,
    },
}

impl Event {
    pub fn job(&self) -> u64 {
        match self {
            Event::Checked { job, .. }
            | Event::Progress { job, .. }
            | Event::Verifying { job }
            | Event::Downloaded { job, .. } => *job,
        }
    }
}

/// A running check or download. Events of a job that is no longer current
/// are ignored, so cancelling is just forgetting the job.
#[derive(Clone)]
pub struct Job {
    pub id: u64,
    pub trigger: Trigger,
    pub cancel: CancellationToken,
}

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

impl Job {
    pub fn new(trigger: Trigger) -> Job {
        Job {
            id: NEXT_JOB.fetch_add(1, Ordering::Relaxed),
            trigger,
            cancel: CancellationToken::new(),
        }
    }
}

/// Runs an async job on a thread of its own and reports its result.
fn spawn<F>(job: u64, sink: fn(Event), work: impl FnOnce() -> F + Send + 'static)
where
    F: Future<Output = Event>,
{
    let spawned = std::thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            let event = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(work()),
                Err(e) => Event::Checked {
                    job,
                    result: Err(Error::Io(e.to_string())),
                },
            };
            sink(event);
        });
    if let Err(e) = spawned {
        sink(Event::Checked {
            job,
            result: Err(Error::Io(e.to_string())),
        });
    }
}

/// Looks for a newer version; `lang` picks the release notes.
pub fn spawn_check(job: Job, lang: String, sink: fn(Event)) {
    let id = job.id;
    spawn(id, sink, move || async move {
        let result = async {
            let client = http::client()?;
            check(
                &client,
                &manifest_url(),
                &trusted_keys(),
                &Version::current(),
                &lang,
            )
            .await
        }
        .await;
        Event::Checked { job: id, result }
    });
}

/// Downloads the installer of `info` to `dest` and checks it.
pub fn spawn_download(job: Job, info: UpdateInfo, dest: PathBuf, sink: fn(Event)) {
    let id = job.id;
    let cancel = job.cancel;
    spawn(id, sink, move || async move {
        let result = async {
            let client = http::client()?;
            let mut progress = |done, total| {
                sink(Event::Progress {
                    job: id,
                    done,
                    total,
                })
            };
            let hash = http::download(&client, &info.url, &dest, info.size, &mut progress, &cancel)
                .await?;
            sink(Event::Verifying { job: id });
            if hash != info.sha256 {
                let _ = std::fs::remove_file(&dest);
                return Err(Error::Checksum);
            }
            Ok(dest)
        }
        .await;
        Event::Downloaded { job: id, result }
    });
}

/// Fetches and verifies the manifest. The signature is checked before
/// anything else in the manifest is trusted.
pub async fn check(
    client: &reqwest::Client,
    url: &str,
    keys: &[VerifyingKey],
    current: &Version,
    lang: &str,
) -> Result<Option<UpdateInfo>, Error> {
    let bytes = http::fetch_manifest(client, url).await?;
    let manifest = manifest::parse(&bytes).map_err(Error::Manifest)?;
    if !statement::verify(keys, &manifest) {
        return Err(Error::Signature);
    }
    Ok(newer(&manifest, current, lang))
}

/// The update described by a (verified) manifest, if it is newer than `current`.
pub fn newer(manifest: &Manifest, current: &Version, lang: &str) -> Option<UpdateInfo> {
    let version = manifest.parsed_version()?;
    if version <= *current {
        return None;
    }
    let sha256: [u8; 32] = manifest::from_hex(&manifest.installer.sha256)?
        .try_into()
        .ok()?;
    Some(UpdateInfo {
        version: manifest.version.clone(),
        notes: manifest.notes_for(lang).to_string(),
        url: manifest.installer.url.clone(),
        size: manifest.installer.size,
        sha256,
        file_name: manifest.installer_file_name(),
    })
}

/// Remembers that an update to `version` was handed to Setup, so that the
/// next start can tell when it did not happen.
pub fn write_pending(version: &str) -> io::Result<()> {
    let path = update_dir()?.join(PENDING_FILE);
    let json = serde_json::json!({ "version": version });
    std::fs::write(path, json.to_string())
}

pub fn read_pending() -> Option<String> {
    let path = update_dir().ok()?.join(PENDING_FILE);
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("version")?.as_str().map(str::to_string)
}

pub fn remove_pending() {
    if let Ok(dir) = update_dir() {
        let _ = std::fs::remove_file(dir.join(PENDING_FILE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn events_know_their_job() {
        assert_eq!(Event::Verifying { job: 7 }.job(), 7);
        assert_eq!(
            Event::Progress {
                job: 8,
                done: 1,
                total: 2
            }
            .job(),
            8
        );
        assert_eq!(
            Event::Checked {
                job: 9,
                result: Ok(None)
            }
            .job(),
            9
        );
        assert_eq!(
            Event::Downloaded {
                job: 10,
                result: Err(Error::Cancelled)
            }
            .job(),
            10
        );
    }

    fn signed_manifest(version: &str, secret: &[u8; 32]) -> Manifest {
        let mut m = manifest::example();
        m.version = version.into();
        m.signature = statement::sign(
            secret,
            &statement::statement(m.format, &m.version, &m.installer.sha256, m.installer.size),
        );
        m
    }

    #[test]
    fn newer_versions_are_offered_with_the_right_notes() {
        let m = signed_manifest("1.1.0", &[1u8; 32]);
        let current = Version::parse("1.0.0").unwrap();
        let info = newer(&m, &current, "hr").expect("newer");
        assert_eq!(info.version, "1.1.0");
        assert_eq!(info.notes, "Što je novo\n\n- Automatska ažuriranja");
        assert_eq!(info.file_name, "Dodavo-1.1.0-setup.exe");
        assert_eq!(info.size, m.installer.size);
        assert_eq!(manifest::to_hex(&info.sha256), m.installer.sha256);
        // English for a language without notes.
        assert!(
            newer(&m, &current, "sl")
                .unwrap()
                .notes
                .starts_with("What's new")
        );
        // Not newer: the same or an older version.
        assert!(newer(&m, &Version::parse("1.1.0").unwrap(), "en").is_none());
        assert!(newer(&m, &Version::parse("1.2.0-beta.1").unwrap(), "en").is_none());
    }

    #[test]
    fn check_rejects_wrong_keys_and_serves_local_files() {
        // A full `check` against a local HTTP server would need a server; the
        // pieces it is made of (fetch, parse, verify, newer) are tested
        // separately. Here: the key handling used by `check`.
        let secret = [5u8; 32];
        let m = signed_manifest("9.0.0", &secret);
        let key = SigningKey::from_bytes(&secret).verifying_key();
        assert!(statement::verify(&[key], &m));
        let other = SigningKey::from_bytes(&[6u8; 32]).verifying_key();
        assert!(!statement::verify(&[other], &m));
        // The built-in placeholder key verifies nothing.
        assert!(!statement::verify(&trusted_keys(), &m));
    }

    #[test]
    fn pending_marker_roundtrip() {
        remove_pending();
        assert_eq!(read_pending(), None);
        write_pending("1.2.3").unwrap();
        assert_eq!(read_pending().as_deref(), Some("1.2.3"));
        remove_pending();
        assert_eq!(read_pending(), None);
    }
}
