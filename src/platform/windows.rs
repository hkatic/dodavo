//! Windows: known folders, free disk space and UTF-16 strings.

use std::path::{Path, PathBuf};

use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW},
    System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_DWORD, REG_SZ, RegCloseKey,
        RegOpenKeyExW, RegQueryValueExW,
    },
    UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath},
};

// The `windows` crates bind CoTaskMemFree to combase.dll, which Windows 7
// doesn't have. ole32.dll exports it on every Windows version.
#[link(name = "ole32")]
unsafe extern "system" {
    pub fn CoTaskMemFree(pv: *mut std::ffi::c_void);
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a NUL terminated UTF-16 string.
// Public for the program's own interface; the library has no other users.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// `%APPDATA%\Dodavo`.
pub fn data_base() -> PathBuf {
    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let base = appdata.join("Dodavo");
    // Keep the identity and settings of versions released under the old name.
    let legacy = appdata.join("QuickAndEasyFileTransfer");
    if !base.exists() && legacy.is_dir() {
        let _ = std::fs::rename(&legacy, &base);
    }
    base
}

/// The temporary archives of the transfers.
pub fn temp_base() -> PathBuf {
    std::env::temp_dir().join("Dodavo")
}

/// The name to send under until the user chooses one.
pub fn user_name() -> Option<String> {
    std::env::var("USERNAME").ok()
}

pub fn downloads_dir() -> PathBuf {
    unsafe {
        let mut p = std::ptr::null_mut();
        if SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut p) == 0 {
            let s = from_wide_ptr(p);
            CoTaskMemFree(p as _);
            return PathBuf::from(s);
        }
    }
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Downloads"))
        .unwrap_or_else(std::env::temp_dir)
}

/// What the log says of this Windows (see `logging`): its name, version,
/// build, release and edition, and the processor's architecture. Nothing of
/// the user or of the computer's name.
pub fn os_description() -> Vec<String> {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let text = |name: &str| match registry_value(HKEY_LOCAL_MACHINE, KEY, name)? {
        RegistryValue::Text(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    };
    let number = |name: &str| match registry_value(HKEY_LOCAL_MACHINE, KEY, name)? {
        RegistryValue::Number(n) => Some(n),
        _ => None,
    };
    let build: u32 = text("CurrentBuild")
        .or_else(|| text("CurrentBuildNumber"))
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);
    // Windows 10 and later keep "6.3" in CurrentVersion for old programs.
    let version = match (
        number("CurrentMajorVersionNumber"),
        number("CurrentMinorVersionNumber"),
    ) {
        (Some(major), Some(minor)) => format!("{major}.{minor}"),
        _ => text("CurrentVersion").unwrap_or_else(|| "?".into()),
    };
    let name = match version.as_str() {
        "10.0" if build >= 22000 => "Windows 11",
        "10.0" => "Windows 10",
        "6.3" => "Windows 8.1",
        "6.2" => "Windows 8",
        "6.1" => "Windows 7",
        _ => "Windows",
    };
    let mut line = format!("{name}, version {version}, build {build}");
    if let Some(ubr) = number("UBR") {
        line.push_str(&format!(" update {ubr}"));
    }
    if let Some(release) = text("DisplayVersion").or_else(|| text("ReleaseId")) {
        line.push_str(&format!(", release {release}"));
    }
    if let Some(pack) = text("CSDVersion") {
        line.push_str(&format!(", {pack}"));
    }
    if let Some(edition) = text("EditionID") {
        line.push_str(&format!(", edition {edition}"));
    }
    // A 32-bit program on 64-bit Windows sees the native one in the first.
    let native = std::env::var("PROCESSOR_ARCHITEW6432")
        .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
        .unwrap_or_else(|_| "?".into());
    vec![line, format!("processor architecture {native}")]
}

enum RegistryValue {
    Text(String),
    Number(u32),
}

/// A text or number value of the registry, from its 64-bit view.
fn registry_value(root: HKEY, key: &str, name: &str) -> Option<RegistryValue> {
    let key = wide(key);
    let name = wide(name);
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: NUL-terminated strings and a handle to fill, closed below.
    let opened = unsafe {
        RegOpenKeyExW(
            root,
            key.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut handle,
        )
    };
    if opened != ERROR_SUCCESS {
        return None;
    }
    let mut buf = [0u16; 256];
    // One unit less than the buffer, so that a text always ends in a NUL.
    let mut size = ((buf.len() - 1) * 2) as u32;
    let mut kind = 0u32;
    // SAFETY: the buffer is as large as `size` says.
    let read = unsafe {
        RegQueryValueExW(
            handle,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    // SAFETY: the handle was opened above.
    unsafe { RegCloseKey(handle) };
    if read != ERROR_SUCCESS {
        return None;
    }
    match kind {
        REG_SZ => Some(RegistryValue::Text(from_wide_ptr(buf.as_ptr()))),
        REG_DWORD if size == 4 => {
            Some(RegistryValue::Number(buf[0] as u32 | (buf[1] as u32) << 16))
        }
        _ => None,
    }
}

/// The root of the volume holding `path` (e.g. `C:\`) and the free space on
/// it available to the user. `path` may not exist yet; its nearest existing
/// parent is used.
pub fn disk_free(path: &Path) -> Option<(PathBuf, u64)> {
    let existing = path.ancestors().find(|p| p.is_dir())?;
    let name = wide(&existing.display().to_string());
    let mut root = [0u16; 512];
    let mut free = 0u64;
    let ok = unsafe {
        GetVolumePathNameW(name.as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
            && GetDiskFreeSpaceExW(
                name.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) != 0
    };
    ok.then(|| (PathBuf::from(from_wide_ptr(root.as_ptr())), free))
}
