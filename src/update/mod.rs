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

pub mod http;
pub mod keys;
pub mod manifest;
pub mod statement;
pub mod version;

use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use ed25519_dalek::VerifyingKey;
use tokio_util::sync::CancellationToken;
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE, WAIT_OBJECT_0},
    System::{
        Registry::{
            HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY,
            KEY_WOW64_64KEY, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
        },
        Threading::{GetExitCodeProcess, WaitForSingleObject},
    },
    UI::{
        Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            ShellExecuteExW,
        },
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

use crate::ui::wide;
use manifest::Manifest;
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
    /// Release notes in the user's language, with `\r\n` line ends.
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
    let notes = manifest
        .notes_for(lang)
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    Some(UpdateInfo {
        version: manifest.version.clone(),
        notes,
        url: manifest.installer.url.clone(),
        size: manifest.installer.size,
        sha256,
        file_name: manifest.installer_file_name(),
    })
}

/// How the running program was installed, which decides how Setup must run
/// to replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// `%LOCALAPPDATA%\Programs\Dodavo`, registered under HKEY_CURRENT_USER.
    PerUser,
    /// Program Files, registered under HKEY_LOCAL_MACHINE; Setup needs
    /// administrator rights and asks for them (elevation prompt).
    AllUsers,
    /// Neither registration matches this program (a portable copy, a
    /// development build): Setup decides by itself.
    Unknown,
}

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{34D1793E-8FBB-46FC-BC27-09F31F071943}_is1";

/// Decides the install mode from where the program runs and where the two
/// possible registrations say Dodavo is installed.
pub fn mode_for(
    exe_dir: Option<&Path>,
    per_user: Option<&Path>,
    all_users: Option<&Path>,
) -> InstallMode {
    fn normal(p: &Path) -> String {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    }
    let Some(exe_dir) = exe_dir.map(normal) else {
        return InstallMode::Unknown;
    };
    if all_users.is_some_and(|d| normal(d) == exe_dir) {
        InstallMode::AllUsers
    } else if per_user.is_some_and(|d| normal(d) == exe_dir) {
        InstallMode::PerUser
    } else {
        InstallMode::Unknown
    }
}

/// `InstallLocation` of Dodavo's uninstall entry under `root`, if there is one.
fn uninstall_location(root: HKEY, wow_flag: u32) -> Option<PathBuf> {
    let key = wide(UNINSTALL_KEY);
    let value = wide("InstallLocation");
    let mut handle: HKEY = std::ptr::null_mut();
    let opened = unsafe { RegOpenKeyExW(root, key.as_ptr(), 0, KEY_READ | wow_flag, &mut handle) };
    if opened != ERROR_SUCCESS {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let mut kind = 0u32;
    let read = unsafe {
        RegQueryValueExW(
            handle,
            value.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    unsafe { RegCloseKey(handle) };
    if read != ERROR_SUCCESS || kind != REG_SZ {
        return None;
    }
    let text = crate::ui::from_wide_ptr(buf.as_ptr());
    (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()))
}

/// The install mode of the running program.
pub fn install_mode() -> InstallMode {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let per_user = uninstall_location(HKEY_CURRENT_USER, 0);
    let all_users = uninstall_location(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY)
        .or_else(|| uninstall_location(HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY));
    mode_for(
        exe_dir.as_deref(),
        per_user.as_deref(),
        all_users.as_deref(),
    )
}

/// Setup's command line: silent, the install mode to keep, our process id
/// (Setup waits for it) and the arguments to start Dodavo with afterwards.
pub fn setup_parameters(
    pid: u32,
    mode: InstallMode,
    relaunch_args: &str,
    log: Option<&Path>,
) -> String {
    let mut params = String::from("/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL");
    match mode {
        InstallMode::PerUser => params.push_str(" /CURRENTUSER"),
        InstallMode::AllUsers => params.push_str(" /ALLUSERS"),
        InstallMode::Unknown => {}
    }
    params.push_str(&format!(
        " /DODAVOUPDATE={pid} /DODAVOARGS=\"{}\"",
        relaunch_args.replace('"', "")
    ));
    // A log with a fixed name only when Setup runs as this user. An
    // elevated Setup may run as another account (someone else's
    // administrator credentials), which may not be able to create a file in
    // this user's temp folder, and Setup aborts when it cannot create the
    // named log. Plain /LOG writes into the installing account's own temp
    // folder instead and never fails.
    match log {
        Some(log) if mode == InstallMode::PerUser => {
            params.push_str(&format!(" /LOG=\"{}\"", log.display()));
        }
        _ => params.push_str(" /LOG"),
    }
    params
}

/// Starts Setup silently (see [`setup_parameters`]) and returns a handle of
/// its process. Setup is started with the plain "open" verb, never elevated
/// by Dodavo: for an all-users installation Setup asks for elevation itself,
/// and only then can it start Dodavo again as the ordinary user. The elevated
/// Setup tells every running instance to exit (`Dodavo_ExitForUpdate`) and
/// waits for them before it replaces the program.
pub fn launch_installer(
    setup: &Path,
    mode: InstallMode,
    relaunch_args: &str,
) -> Result<HANDLE, Error> {
    let log = log_path();
    if let Some(log) = &log {
        let _ = std::fs::remove_file(log);
    }
    let params = setup_parameters(std::process::id(), mode, relaunch_args, log.as_deref());
    let verb = wide("open");
    let file = wide(&setup.display().to_string());
    let params = wide(&params);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
    if !ok || info.hProcess.is_null() {
        let code = unsafe { GetLastError() };
        return Err(Error::Launch(format!(
            "{} (Windows error {code})",
            setup.display()
        )));
    }
    Ok(info.hProcess)
}

/// Whether the Setup process has ended already, and with which exit code.
pub fn setup_exited(process: HANDLE) -> Option<u32> {
    unsafe {
        if WaitForSingleObject(process, 0) != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        GetExitCodeProcess(process, &mut code);
        Some(code)
    }
}

pub fn close_handle(process: HANDLE) {
    unsafe { CloseHandle(process) };
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
    fn install_mode_follows_the_registration_of_the_running_copy() {
        let pf = Path::new(r"C:\Program Files\Dodavo\");
        let local = Path::new(r"C:\Users\ana\AppData\Local\Programs\Dodavo\");
        let m = |exe: &str, per_user: Option<&Path>, all_users: Option<&Path>| {
            mode_for(Some(Path::new(exe)), per_user, all_users)
        };
        assert_eq!(
            m(r"C:\Program Files\Dodavo", None, Some(pf)),
            InstallMode::AllUsers
        );
        assert_eq!(
            m(r"c:\program files\dodavo", Some(local), Some(pf)),
            InstallMode::AllUsers
        );
        assert_eq!(
            m(
                r"C:\Users\ana\AppData\Local\Programs\Dodavo",
                Some(local),
                Some(pf)
            ),
            InstallMode::PerUser
        );
        assert_eq!(
            m(
                r"C:\Users\ana\AppData\Local\Programs\Dodavo",
                Some(local),
                None
            ),
            InstallMode::PerUser
        );
        // A portable copy next to an installation, or no installation at all.
        assert_eq!(
            m(r"D:\Tools\Dodavo", Some(local), Some(pf)),
            InstallMode::Unknown
        );
        assert_eq!(m(r"D:\Tools\Dodavo", None, None), InstallMode::Unknown);
        assert_eq!(mode_for(None, Some(local), Some(pf)), InstallMode::Unknown);
    }

    #[test]
    fn setup_parameters_name_the_mode_the_pid_and_the_relaunch() {
        let log = Path::new(r"C:\Temp\Dodavo\update\setup.log");
        let p = setup_parameters(
            4711,
            InstallMode::AllUsers,
            "--updated --profile ana",
            Some(log),
        );
        // Elevated Setup may run as another account: no fixed log path.
        assert_eq!(
            p,
            "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL /ALLUSERS \
             /DODAVOUPDATE=4711 /DODAVOARGS=\"--updated --profile ana\" /LOG"
        );
        let p = setup_parameters(1, InstallMode::PerUser, "--updated", Some(log));
        assert!(p.contains(" /CURRENTUSER "), "{p}");
        assert!(
            p.ends_with("/DODAVOARGS=\"--updated\" /LOG=\"C:\\Temp\\Dodavo\\update\\setup.log\""),
            "{p}"
        );
        let p = setup_parameters(1, InstallMode::PerUser, "--updated", None);
        assert!(p.ends_with("/DODAVOARGS=\"--updated\" /LOG"), "{p}");
        let p = setup_parameters(
            1,
            InstallMode::Unknown,
            "--updated \"--minimized\"",
            Some(log),
        );
        assert!(!p.contains("USER"), "{p}");
        assert!(p.ends_with(" /LOG"), "{p}");
        // Quotes inside the arguments would break Setup's parsing; they are dropped.
        assert!(p.contains("/DODAVOARGS=\"--updated --minimized\""), "{p}");
    }

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
        assert_eq!(info.notes, "Što je novo\r\n\r\n- Automatska ažuriranja");
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
