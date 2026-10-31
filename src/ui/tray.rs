//! Notification area (system tray) icon.

use std::path::PathBuf;

use windows_sys::Win32::{
    Foundation::HWND,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NOTIFYICONDATAW, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            GetSystemMetrics, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXSMICON, SM_CYSMICON,
            WM_APP,
        },
    },
};

use super::APP_NAME;

pub const WM_APP_TRAY: u32 = WM_APP + 2;
const TRAY_ID: u32 = 1;

/// What selecting a notification does.
pub enum BalloonAction {
    /// Shows a file or folder in File Explorer.
    Reveal(PathBuf),
    /// Brings a window of the program to the front.
    Window(HWND),
}

fn copy_into(dst: &mut [u16], text: &str) {
    let mut i = 0;
    for c in text.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ID;
    nid
}

pub fn add(hwnd: HWND) {
    let mut nid = base(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    unsafe {
        nid.hIcon = LoadImageW(
            GetModuleHandleW(std::ptr::null()),
            1 as _,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        ) as _;
        // The struct is packed on 32-bit Windows: fill the arrays through copies.
        let mut tip = nid.szTip;
        copy_into(&mut tip, APP_NAME);
        nid.szTip = tip;
        Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

pub fn remove(hwnd: HWND) {
    let nid = base(hwnd);
    unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

/// Shows a notification. Selecting it performs `action` (or shows the main
/// window). Not to be called inside `app::with`.
pub fn balloon(hwnd: HWND, title: &str, text: &str, action: Option<BalloonAction>) {
    crate::app::with(|a| a.balloon_action = action);
    let mut nid = base(hwnd);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    let mut info_title = nid.szInfoTitle;
    copy_into(&mut info_title, title);
    nid.szInfoTitle = info_title;
    let mut info = nid.szInfo;
    copy_into(&mut info, text);
    nid.szInfo = info;
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}
