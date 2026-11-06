//! Linux, and Android on top of it: known folders, free disk space and the
//! user's preferences. On Android only the app can ask the system for these,
//! so it gives them at start ([`set_dirs`]); `DODAVO_DATA_DIR`,
//! `DODAVO_TEMP_DIR` and `DODAVO_DOWNLOADS_DIR` stand in for it elsewhere (the
//! test peer on a phone, say).

use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub(crate) use super::unix::statfs_of;

/// What the app tells at start (see [`set_dirs`]).
#[derive(Debug, Clone, Default)]
pub struct Dirs {
    /// The settings, the identity and the records of transfers.
    pub data: PathBuf,
    /// The temporary archives of transfers, kept for as long as a transfer
    /// waits: not a cache that the system may empty meanwhile.
    pub temp: PathBuf,
    /// Where received items go.
    pub downloads: PathBuf,
    /// The languages the user prefers, most preferred first (`hr-HR`, ...).
    pub languages: Vec<String>,
    /// The name of the device, to send under until the user chooses one.
    pub device_name: Option<String>,
}

static DIRS: OnceLock<Dirs> = OnceLock::new();

/// Gives the folders and preferences for the rest of the process. Only the
/// first call counts: the folders cannot move while transfers use them.
pub fn set_dirs(dirs: Dirs) {
    let _ = DIRS.set(dirs);
}

fn from_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Where the settings and the Contact ID are. Never a temporary folder as a
/// last resort, as the Contact ID lives there.
pub fn data_base() -> PathBuf {
    if let Some(dirs) = DIRS.get() {
        return dirs.data.clone();
    }
    if let Some(dir) = from_env("DODAVO_DATA_DIR") {
        return dir;
    }
    #[cfg(target_os = "android")]
    panic!("the app gives the folder of its data at start (platform::set_dirs)");
    #[cfg(not(target_os = "android"))]
    {
        let data = from_env("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share"));
        data.join("Dodavo")
    }
}

pub fn downloads_dir() -> PathBuf {
    if let Some(dirs) = DIRS.get() {
        return dirs.downloads.clone();
    }
    if let Some(dir) = from_env("DODAVO_DOWNLOADS_DIR") {
        return dir;
    }
    #[cfg(target_os = "android")]
    panic!("the app gives the folder of received items at start (platform::set_dirs)");
    #[cfg(not(target_os = "android"))]
    home().join("Downloads")
}

/// The temporary archives of the transfers, which may wait for a long time.
pub fn temp_base() -> PathBuf {
    if let Some(dirs) = DIRS.get() {
        return dirs.temp.clone();
    }
    from_env("DODAVO_TEMP_DIR").unwrap_or_else(|| data_base().join("tmp"))
}

#[cfg(not(target_os = "android"))]
fn home() -> PathBuf {
    std::env::home_dir().expect("every user has a home folder")
}

/// The name to send under until the user chooses one: the device's name on
/// Android, the account's elsewhere.
pub fn user_name() -> Option<String> {
    if let Some(name) = DIRS.get().and_then(|dirs| dirs.device_name.clone()) {
        return Some(name);
    }
    #[cfg(target_os = "android")]
    return None;
    #[cfg(not(target_os = "android"))]
    super::unix::account_name()
}

/// The languages the user prefers, most preferred first (`hr-HR`, ...): as
/// the app says, or from the locale of the environment.
pub fn preferred_languages() -> Vec<String> {
    if let Some(dirs) = DIRS.get() {
        return dirs.languages.clone();
    }
    let mut languages = Vec::new();
    if let Ok(list) = std::env::var("LANGUAGE") {
        languages.extend(list.split(':').map(locale_tag));
    }
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(name) {
            languages.push(locale_tag(&value));
        }
    }
    languages.retain(|tag| !tag.is_empty() && tag != "C" && tag != "POSIX");
    languages
}

/// `hr_HR.UTF-8` as `hr-HR`.
fn locale_tag(locale: &str) -> String {
    let locale = locale.split(['.', '@']).next().unwrap_or_default();
    locale.replace('_', "-")
}

/// The text of a system error, in the user's language.
pub fn error_text(code: i32) -> Option<String> {
    use crate::i18n::{android_tr, tr};
    Some(match code {
        libc::ENOSPC | libc::EDQUOT => tr!(error_disk_full).to_string(),
        libc::EACCES | libc::EPERM => android_tr!(error_no_access).to_string(),
        libc::ENAMETOOLONG => android_tr!(error_name_too_long).to_string(),
        libc::EROFS => android_tr!(error_read_only).to_string(),
        libc::ENOENT | libc::ENOTDIR => tr!(error_part_missing).to_string(),
        code => android_tr!(error_system, code = code),
    })
}

/// The root of the volume holding `path` and the space available on it.
/// `path` may not exist yet; its nearest existing parent is used. On Android
/// the shared storage keeps its files on the volume of the app's data, so a
/// path there counts as that volume: what a receive puts in both places
/// shares one free space (see `space::check_space_of`).
pub fn disk_free(path: &Path) -> Option<(PathBuf, u64)> {
    let existing = path.ancestors().find(|p| p.is_dir())?;
    let volume = statvfs_of(existing)?;
    let free = volume.f_bavail.saturating_mul(volume.f_frsize);
    #[cfg(target_os = "android")]
    {
        let data = data_base();
        if let Some(data) = data.ancestors().find(|p| p.is_dir())
            && let Some(data_volume) = statvfs_of(data)
            && (data_volume.f_blocks, data_volume.f_frsize, data_volume.f_files)
                == (volume.f_blocks, volume.f_frsize, volume.f_files)
        {
            return Some((volume_root(data), free));
        }
    }
    Some((volume_root(existing), free))
}

/// The highest folder above `path` on the same device: where the volume is.
fn volume_root(path: &Path) -> PathBuf {
    let device = |p: &Path| p.metadata().ok().map(|m| m.dev());
    let Some(own) = device(path) else {
        return path.to_path_buf();
    };
    path.ancestors()
        .take_while(|p| device(p) == Some(own))
        .last()
        .unwrap_or(path)
        .to_path_buf()
}

fn statvfs_of(path: &Path) -> Option<libc::statvfs> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: a C string and a structure to fill, which outlive the call.
    if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: filled by the call.
    Some(unsafe { info.assume_init() })
}

/// Nothing to do: what Android backs up is up to the app's rules.
pub fn exclude_from_backups(_path: &Path) {}

/// The version of Android's API (26 for Android 8.0, ...), read once.
#[cfg(target_os = "android")]
pub fn api_level() -> u32 {
    static LEVEL: OnceLock<u32> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        let mut value = [0 as std::ffi::c_char; libc::PROP_VALUE_MAX as usize];
        // SAFETY: a C string for the name, and a buffer of the length that
        // the call writes at most.
        let length =
            unsafe { libc::__system_property_get(c"ro.build.version.sdk".as_ptr(), value.as_mut_ptr()) };
        if length <= 0 {
            return 0;
        }
        // SAFETY: the call wrote a NUL-terminated string.
        let text = unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) };
        text.to_str().ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
    })
}

/// Gives iroh, and the crates below it, Java's VM and the app's context,
/// which they need for the system's DNS servers and the network's
/// interfaces. Once, before the network starts: without it, those crates
/// find out by catching a panic, which the panic hook would see first.
///
/// # Safety
///
/// Both pointers stay valid until the process ends.
#[cfg(target_os = "android")]
pub unsafe fn install_android_jni_context(
    java_vm: *mut std::ffi::c_void,
    context: *mut std::ffi::c_void,
) {
    // SAFETY: as the caller says.
    unsafe { iroh::dns::install_android_jni_context(java_vm, context) }
}
