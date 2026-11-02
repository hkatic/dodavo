//! macOS: known folders, free disk space and the user's preferences.

use std::{
    ffi::{CStr, CString, c_char, c_void},
    io,
    mem::MaybeUninit,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    boolean::CFBoolean,
    number::CFNumber,
    string::CFString,
    url::CFURL,
};
use core_foundation_sys::{
    base::{Boolean, CFTypeRef},
    error::CFErrorRef,
    locale::CFLocaleCopyPreferredLanguages,
    string::CFStringRef,
    url::{CFURLRef, CFURLSetResourcePropertyForKey, kCFURLVolumeLocalizedNameKey},
};

// Not in the core-foundation-sys crate. Both are older than macOS 11.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFURLIsExcludedFromBackupKey: CFStringRef;
    static kCFURLVolumeAvailableCapacityForImportantUsageKey: CFStringRef;
    fn CFURLCopyResourcePropertyForKey(
        url: CFURLRef,
        key: CFStringRef,
        value: *mut CFTypeRef,
        error: *mut CFErrorRef,
    ) -> Boolean;
}

/// `~/Library/Application Support/Dodavo`. Never a temporary folder as a last
/// resort, as the Contact ID lives there: without a home folder nothing can
/// be kept, and `home_dir` looks in the user database when `HOME` is not set.
pub fn data_base() -> PathBuf {
    std::env::home_dir()
        .expect("every user has a home folder")
        .join("Library/Application Support/Dodavo")
}

pub fn downloads_dir() -> PathBuf {
    std::env::home_dir()
        .expect("every user has a home folder")
        .join("Downloads")
}

/// The temporary archives of the transfers. Not in `$TMPDIR`: macOS deletes
/// what stays there for a few days, and a transfer may wait for much longer.
pub fn temp_base() -> PathBuf {
    data_base().join("tmp")
}

/// What Apple calls its settings app on this macOS.
pub fn settings_app_name() -> &'static str {
    let mut version = [0 as c_char; 32];
    let mut length = version.len();
    // SAFETY: a C string for the name, and a buffer with its length to fill.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            version.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    let major: u32 = if status == 0 {
        let text = c_text(&version);
        text.split('.')
            .next()
            .and_then(|major| major.parse().ok())
            .unwrap_or(13)
    } else {
        13
    };
    if major >= 13 {
        crate::i18n::mac_tr!(system_settings)
    } else {
        crate::i18n::mac_tr!(system_preferences)
    }
}

/// The text of a system error, in the user's language.
pub fn error_text(code: i32) -> Option<String> {
    use crate::i18n::{mac_tr, tr};
    Some(match code {
        libc::ENOSPC | libc::EDQUOT => tr!(error_disk_full).to_string(),
        libc::EACCES | libc::EPERM => mac_tr!(error_no_access, settings = settings_app_name()),
        libc::ENAMETOOLONG => mac_tr!(error_name_too_long).to_string(),
        libc::EROFS => mac_tr!(error_read_only).to_string(),
        libc::ENOENT | libc::ENOTDIR => crate::i18n::tr!(error_part_missing).to_string(),
        code => mac_tr!(error_system, code = code),
    })
}

/// The name to send under until the user chooses one: the full name of the
/// account, or else its short name.
pub fn user_name() -> Option<String> {
    let mut account = MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0 as c_char; 16 * 1024];
    // SAFETY: the call fills `account`, with its strings in `buffer`, of the
    // given length; all outlive it. `found` is `account` if there is one.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            account.as_mut_ptr(),
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if status == 0 && !found.is_null() {
        // SAFETY: filled by the call above, and `pw_gecos` is a C string in
        // `buffer` or null.
        let gecos = unsafe { account.assume_init_ref().pw_gecos };
        if !gecos.is_null() {
            // SAFETY: as above.
            let name = unsafe { CStr::from_ptr(gecos) }.to_string_lossy();
            let name = name.split(',').next().unwrap_or_default().trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    std::env::var("USER").ok()
}

/// The languages the user prefers, most preferred first (`hr-HR`, ...),
/// which also reflects the language chosen for this app.
pub fn preferred_languages() -> Vec<String> {
    // SAFETY: the call has no arguments, and returns an array that the caller
    // owns, of strings.
    let languages: CFArray<CFString> =
        unsafe { CFArray::wrap_under_create_rule(CFLocaleCopyPreferredLanguages()) };
    languages
        .iter()
        .map(|language| language.to_string())
        .collect()
}

/// A resource property of the file or folder at `path`.
fn url_property(path: &Path, key: CFStringRef) -> Option<CFType> {
    let url = CFURL::from_path(path, path.is_dir())?;
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: `url` and `key` are valid for the call, which stores an owned
    // object in `value`, if there is one, and no error.
    let ok = unsafe {
        CFURLCopyResourcePropertyForKey(
            url.as_concrete_TypeRef(),
            key,
            &mut value,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: an object that the call gave to the caller.
    (ok != 0 && !value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
}

/// What `statfs` says of the volume that holds `path`.
pub(crate) fn statfs_of(path: &Path) -> io::Result<libc::statfs> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut info = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: a C string and a structure to fill, which outlive the call.
    if unsafe { libc::statfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: filled by the call.
    Ok(unsafe { info.assume_init() })
}

/// A string in a fixed buffer of a C structure.
pub(crate) fn c_text(buffer: &[c_char]) -> String {
    // SAFETY: `c_char` and `u8` have the same size and alignment.
    let bytes = unsafe { &*(std::ptr::from_ref(buffer) as *const [u8]) };
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

/// The root of the volume holding `path` and the space available on it for
/// what the user asks for: this includes what macOS can free, such as caches,
/// which `statfs` leaves out. `path` may not exist yet; its nearest existing
/// parent is used.
pub fn disk_free(path: &Path) -> Option<(PathBuf, u64)> {
    let existing = path.ancestors().find(|p| p.is_dir())?;
    let volume = statfs_of(existing).ok()?;
    // SAFETY: a constant string of the framework, which macOS 11 has.
    let key = unsafe { kCFURLVolumeAvailableCapacityForImportantUsageKey };
    let free = url_property(existing, key)
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|number| number.to_i64())
        .and_then(|free| u64::try_from(free).ok())
        .filter(|&free| free > 0)
        .unwrap_or_else(|| volume.f_bavail.saturating_mul(u64::from(volume.f_bsize)));
    Some((PathBuf::from(c_text(&volume.f_mntonname)), free))
}

/// The name of the volume at `root` as the Finder shows it.
pub fn volume_name(root: &Path) -> Option<String> {
    // SAFETY: a constant string of the framework.
    let key = unsafe { kCFURLVolumeLocalizedNameKey };
    let name = url_property(root, key)?.downcast::<CFString>()?;
    Some(name.to_string())
}

/// Keeps a folder out of Time Machine backups. Best effort.
pub fn exclude_from_backups(path: &Path) {
    let Some(url) = CFURL::from_path(path, true) else {
        return;
    };
    // SAFETY: the arguments are valid for the call, and no error is wanted.
    unsafe {
        CFURLSetResourcePropertyForKey(
            url.as_concrete_TypeRef(),
            kCFURLIsExcludedFromBackupKey,
            CFBoolean::true_value().as_CFTypeRef() as *const c_void,
            std::ptr::null_mut(),
        );
    }
}
