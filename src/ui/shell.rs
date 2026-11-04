//! Shell integration: known folders, file/folder pickers, clipboard,
//! Explorer, the "Start with Windows" registry entry, and the entries in File
//! Explorer's menus.

use std::path::{Path, PathBuf};

use windows::{
    Win32::{
        Foundation::HWND as WHWND,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile},
        UI::Shell::{
            FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog,
            IFileOpenDialog, IShellItem, IShellLinkW, SHCreateItemFromParsingName,
            SIGDN_FILESYSPATH, ShellLink,
        },
    },
    core::{HSTRING, Interface},
};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Registry::{
            HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegSetKeyValueW,
        },
    },
    UI::{
        Shell::{FOLDERID_SendTo, SHGetKnownFolderPath, ShellExecuteW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

use super::{from_wide_ptr, wide};
pub use crate::platform::disk_free;
use crate::{i18n::tr, platform::CoTaskMemFree, update};

const CF_UNICODETEXT: u32 = 13;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Dodavo";
/// Autostart entry written by versions released under the old name.
const LEGACY_RUN_VALUE: &str = "QuickAndEasyFileTransfer";
/// "Send with Dodavo" in File Explorer's context menu, for files and for
/// folders. A selection of both shows it too, and starts one process per item.
/// Setup deletes these two keys and the shortcut below when it uninstalls.
const VERB_KEYS: [&str; 2] = [
    r"Software\Classes\*\shell\Dodavo.Send",
    r"Software\Classes\Directory\shell\Dodavo.Send",
];
/// The shortcut in the Send to folder, which is what File Explorer's Send to
/// menu lists, under its file name.
const SEND_TO_LINK: &str = "Dodavo.lnk";

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
        CoTaskMemFree(name.0 as _);
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
    shell_open(std::ptr::null_mut(), &path.display().to_string())
}

/// Opens a web address in the default browser. Anything Windows shows on
/// the way, such as the choice of an app, belongs to `owner`. Returns `false`
/// (with the reason in the thread's last error) if Windows could not open it.
pub fn open_url(owner: HWND, url: &str) -> bool {
    shell_open(owner, url)
}

fn shell_open(owner: HWND, target: &str) -> bool {
    let target = wide(target);
    let result = unsafe {
        ShellExecuteW(
            owner,
            wide("open").as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 mean success.
    result as usize > 32
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

/// Whether this copy of Dodavo has entries in File Explorer's menus. Others
/// would take them over from the installed copy, and nothing would remove
/// them again: a development build, a copy on a USB stick, or a test profile.
pub fn explorer_menu_available(profile: Option<&str>) -> bool {
    profile.is_none() && update::install_mode() != update::InstallMode::Unknown
}

/// Adds or removes the entries in File Explorer's menus, for this user. They
/// are written anew at every start, so that they point at this program and
/// speak its language. Nothing depends on them, so a failure is not reported.
pub fn set_explorer_menu(enable: bool) {
    if !enable {
        for key in VERB_KEYS {
            delete_registry_tree(key);
        }
        if let Some(folder) = send_to_folder() {
            let _ = std::fs::remove_file(folder.join(SEND_TO_LINK));
        }
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    for key in VERB_KEYS {
        write_verb(key, &exe, tr!(explorer_verb));
    }
    if let Some(folder) = send_to_folder() {
        let _ = write_send_to_link(&folder.join(SEND_TO_LINK), &exe);
    }
}

/// A verb of File Explorer's context menu that passes on the selected item.
/// "Player" shows it for up to 100 selected items, where the default would
/// stop at 15.
fn write_verb(key: &str, exe: &Path, label: &str) {
    let exe = exe.display();
    set_registry_text(key, None, label);
    set_registry_text(key, Some("Icon"), &format!("\"{exe}\",0"));
    set_registry_text(key, Some("MultiSelectModel"), "Player");
    set_registry_text(
        &format!(r"{key}\command"),
        None,
        &format!("\"{exe}\" --send \"%1\""),
    );
}

fn set_registry_text(key: &str, value: Option<&str>, text: &str) {
    let key = wide(key);
    let value = value.map(wide);
    let value_ptr = value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    let text = wide(text);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value_ptr,
            REG_SZ,
            text.as_ptr() as _,
            (text.len() * 2) as u32,
        );
    }
}

fn delete_registry_tree(key: &str) {
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(key).as_ptr()) };
}

fn send_to_folder() -> Option<PathBuf> {
    unsafe {
        let mut path = std::ptr::null_mut();
        let result = SHGetKnownFolderPath(&FOLDERID_SendTo, 0, std::ptr::null_mut(), &mut path);
        let folder = (result == 0).then(|| PathBuf::from(from_wide_ptr(path)));
        CoTaskMemFree(path as _);
        folder
    }
}

/// File Explorer adds the paths of the selected items to the arguments.
fn write_send_to_link(link: &Path, exe: &Path) -> windows::core::Result<()> {
    let exe = HSTRING::from(exe.as_os_str());
    unsafe {
        let shortcut: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        shortcut.SetPath(&exe)?;
        shortcut.SetArguments(&HSTRING::from("--send"))?;
        shortcut.SetIconLocation(&exe, 0)?;
        shortcut
            .cast::<IPersistFile>()?
            .Save(&HSTRING::from(link.as_os_str()), true)
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

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Registry::{RRF_RT_REG_SZ, RegGetValueW};

    use super::*;

    fn read(key: &str, value: Option<&str>) -> Option<String> {
        let key = wide(key);
        let value = value.map(wide);
        let value_ptr = value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let read = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value_ptr,
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as _,
                &mut size,
            )
        };
        (read == 0).then(|| from_wide_ptr(buf.as_ptr()))
    }

    /// The other keys of the program are not touched: this one is its own.
    #[test]
    fn a_verb_says_how_the_program_gets_the_item_and_can_be_removed() {
        let key = format!(r"Software\Dodavo-test-{}\shell\verb", std::process::id());
        let exe = Path::new(r"C:\Program Files\Dodavo\Dodavo.exe");
        write_verb(&key, exe, "Send &with Dodavo");
        assert_eq!(read(&key, None).as_deref(), Some("Send &with Dodavo"));
        assert_eq!(
            read(&key, Some("Icon")).as_deref(),
            Some(r#""C:\Program Files\Dodavo\Dodavo.exe",0"#)
        );
        assert_eq!(
            read(&key, Some("MultiSelectModel")).as_deref(),
            Some("Player")
        );
        assert_eq!(
            read(&format!(r"{key}\command"), None).as_deref(),
            Some(r#""C:\Program Files\Dodavo\Dodavo.exe" --send "%1""#)
        );
        let root = key.split(r"\shell").next().unwrap().to_string();
        delete_registry_tree(&root);
        assert_eq!(read(&key, None), None);
    }

    /// Setup removes what the program writes, by name (`installer\Dodavo.iss`).
    #[test]
    fn setup_knows_the_entries() {
        let setup = include_str!("../../installer/Dodavo.iss");
        for key in VERB_KEYS {
            assert!(setup.contains(key), "{key}");
        }
        assert!(setup.contains(&format!(r"{{usersendto}}\{SEND_TO_LINK}")));
    }
}
