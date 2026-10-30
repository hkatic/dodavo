//! Shell integration: known folders, file/folder pickers, clipboard,
//! Explorer, and the "Start with Windows" registry entry.

use std::path::{Path, PathBuf};

use windows::{
    Win32::{
        Foundation::HWND as WHWND,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
        UI::Shell::{
            FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog,
            IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
        },
    },
    core::HSTRING,
};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW},
    },
    Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW},
    UI::{
        Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, ShellExecuteW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

use super::{from_wide_ptr, wide};

const CF_UNICODETEXT: u32 = 13;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Dodavo";
/// Autostart entry written by versions released under the old name.
const LEGACY_RUN_VALUE: &str = "QuickAndEasyFileTransfer";

pub fn downloads_dir() -> PathBuf {
    unsafe {
        let mut p = std::ptr::null_mut();
        if SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut p) == 0 {
            let s = from_wide_ptr(p);
            windows_sys::Win32::System::Com::CoTaskMemFree(p as _);
            return PathBuf::from(s);
        }
    }
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Downloads"))
        .unwrap_or_else(std::env::temp_dir)
}

fn pick(owner: HWND, title: &str, folder: bool, initial: Option<&Path>) -> Option<PathBuf> {
    unsafe {
        let dlg: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut opts = dlg.GetOptions().ok()? | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST;
        if folder {
            opts |= FOS_PICKFOLDERS;
        }
        dlg.SetOptions(opts).ok()?;
        dlg.SetTitle(&HSTRING::from(title)).ok()?;
        if let Some(dir) = initial.filter(|d| d.is_dir())
            && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from(dir.as_os_str()),
                None,
            )
        {
            let _ = dlg.SetFolder(&item);
        }
        dlg.Show(Some(WHWND(owner as _))).ok()?;
        let item = dlg.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as _));
        path.map(PathBuf::from)
    }
}

pub fn pick_file(owner: HWND, title: &str) -> Option<PathBuf> {
    pick(owner, title, false, None)
}

pub fn pick_folder(owner: HWND, title: &str, initial: Option<&Path>) -> Option<PathBuf> {
    pick(owner, title, true, initial)
}

pub fn copy_to_clipboard(owner: HWND, text: &str) -> bool {
    let data = wide(text);
    unsafe {
        if OpenClipboard(owner) == 0 {
            return false;
        }
        EmptyClipboard();
        let bytes = data.len() * 2;
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        let mut ok = false;
        if !mem.is_null() {
            let dst = GlobalLock(mem) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
                GlobalUnlock(mem);
                ok = !SetClipboardData(CF_UNICODETEXT, mem).is_null();
            }
            if !ok {
                GlobalFree(mem);
            }
        }
        CloseClipboard();
        ok
    }
}

/// Opens a file with its default program, or a folder in Explorer.
/// Returns `false` if Windows could not open it.
pub fn open_path(path: &Path) -> bool {
    let file = wide(&path.display().to_string());
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 mean success.
    result as usize > 32
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

/// Opens Explorer with the item selected.
pub fn reveal_in_explorer(path: &Path) {
    let params = wide(&format!("/select,\"{}\"", path.display()));
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide("explorer.exe").as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

pub fn set_start_with_windows(enable: bool) {
    unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(LEGACY_RUN_VALUE).as_ptr(),
        );
        if enable {
            let exe = std::env::current_exe().unwrap_or_default();
            let cmd = wide(&format!("\"{}\" --minimized", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(RUN_VALUE).as_ptr(),
                REG_SZ,
                cmd.as_ptr() as _,
                (cmd.len() * 2) as u32,
            );
        } else {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(RUN_VALUE).as_ptr(),
            );
        }
    }
}
