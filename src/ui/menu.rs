//! Menus made in code: the menu bar and the context menus.

use windows_sys::Win32::{
    Foundation::{HWND, POINT},
    UI::WindowsAndMessaging::*,
};

use super::wide;

/// Appends a command to a menu.
pub fn item(menu: HMENU, id: u16, text: &str) {
    let text = wide(text);
    unsafe { AppendMenuW(menu, MF_STRING, id as usize, text.as_ptr()) };
}

/// Appends a command that is unavailable unless `enabled`: it is still
/// listed, greyed, so that the menu reads the same every time.
pub fn item_enabled(menu: HMENU, id: u16, text: &str, enabled: bool) {
    let text = wide(text);
    let flags = if enabled {
        MF_STRING
    } else {
        MF_STRING | MF_GRAYED
    };
    unsafe { AppendMenuW(menu, flags, id as usize, text.as_ptr()) };
}

/// Appends a text that cannot be chosen (an explanation).
pub fn note(menu: HMENU, text: &str) {
    let text = wide(text);
    unsafe { AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, text.as_ptr()) };
}

pub fn clear(menu: HMENU) {
    unsafe {
        while GetMenuItemCount(menu) > 0 {
            DeleteMenu(menu, 0, MF_BYPOSITION);
        }
    }
}

pub fn separator(menu: HMENU) {
    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
}

/// Appends a drop-down menu to a menu bar.
pub fn popup(bar: HMENU, popup: HMENU, text: &str) {
    let text = wide(text);
    unsafe { AppendMenuW(bar, MF_POPUP, popup as usize, text.as_ptr()) };
}

/// Shows a context menu at a screen position, destroys it, and returns the
/// chosen command.
pub fn track(menu: HMENU, owner: HWND, pt: POINT) -> Option<u16> {
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            owner,
            std::ptr::null(),
        )
    };
    unsafe { DestroyMenu(menu) };
    (cmd != 0).then_some(cmd as u16)
}
