//! Windows: known folders, free disk space and UTF-16 strings.

use std::path::{Path, PathBuf};

use windows_sys::Win32::{
    Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW},
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
pub fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
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
