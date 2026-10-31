//! Classic Win32 user interface.

pub mod about;
pub mod accessibility;
pub mod help;
pub mod incoming;
pub mod license;
pub mod main_window;
pub mod progress;
pub mod recipient;
pub mod settings;
pub mod shell;
pub mod theme;
pub mod tray;
pub mod update;
pub mod window;

use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Controls::{
            TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_INFORMATION_ICON,
            TDCBF_CLOSE_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_POSITION_RELATIVE_TO_WINDOW,
            TDF_USE_COMMAND_LINKS, TaskDialogIndirect,
        },
        WindowsAndMessaging::{
            GetWindowTextLengthW, GetWindowTextW, IDCANCEL, IDYES, MB_ICONERROR,
            MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MessageBoxW, SendMessageW,
            SetWindowTextW,
        },
    },
};

use crate::i18n::{self, Strings, fill, tr, trn};

pub const APP_NAME: &str = "Dodavo";
/// The author and the website (the repository on GitHub), from Cargo.toml.
pub const APP_AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
pub const APP_WEBSITE: &str = env!("CARGO_PKG_REPOSITORY");
/// The years of the copyright notice (also in res/app.rc and installer\Dodavo.iss).
pub const COPYRIGHT_YEARS: &str = "2026";

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

pub fn get_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

pub fn set_text(hwnd: HWND, text: &str) {
    unsafe {
        SetWindowTextW(hwnd, wide(text).as_ptr());
    }
}

pub fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { SendMessageW(hwnd, msg, wparam, lparam) }
}

fn message_box(owner: HWND, text: &str, style: u32) -> i32 {
    unsafe { MessageBoxW(owner, wide(text).as_ptr(), wide(APP_NAME).as_ptr(), style) }
}

pub fn info(owner: HWND, text: &str) {
    message_box(owner, text, MB_OK | MB_ICONINFORMATION);
}

pub fn error(owner: HWND, text: &str) {
    message_box(owner, text, MB_OK | MB_ICONERROR);
}

pub fn confirm(owner: HWND, text: &str) -> bool {
    message_box(owner, text, MB_YESNO | MB_ICONQUESTION) == IDYES
}

/// Asks a question with a Windows task dialog: `choices` become command links
/// (the first is the default), `close` adds a Close button, and `verification`
/// adds a check box such as "Don't ask me again". Returns the id of the chosen
/// link (`IDCANCEL` or `IDCLOSE` otherwise) and the state of the check box.
pub fn ask(
    owner: HWND,
    instruction: &str,
    content: &str,
    choices: &[(i32, &str)],
    close: bool,
    verification: Option<&str>,
) -> (i32, bool) {
    let title = wide(APP_NAME);
    let instruction = wide(instruction);
    let content = wide(content);
    let verification = verification.map(wide);
    let texts: Vec<Vec<u16>> = choices.iter().map(|(_, t)| wide(t)).collect();
    let buttons: Vec<TASKDIALOG_BUTTON> = choices
        .iter()
        .zip(&texts)
        .map(|((id, _), text)| TASKDIALOG_BUTTON {
            nButtonID: *id,
            pszButtonText: text.as_ptr(),
        })
        .collect();
    let mut config: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    config.cbSize = size_of::<TASKDIALOGCONFIG>() as u32;
    config.hwndParent = owner;
    config.dwFlags =
        TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW;
    config.dwCommonButtons = if close { TDCBF_CLOSE_BUTTON } else { 0 };
    config.pszWindowTitle = title.as_ptr();
    config.Anonymous1 = TASKDIALOGCONFIG_0 {
        pszMainIcon: TD_INFORMATION_ICON,
    };
    config.pszMainInstruction = instruction.as_ptr();
    config.pszContent = content.as_ptr();
    config.cButtons = buttons.len() as u32;
    config.pButtons = buttons.as_ptr();
    config.nDefaultButton = choices.first().map_or(0, |c| c.0);
    config.pszVerificationText = verification
        .as_ref()
        .map_or(std::ptr::null(), |v| v.as_ptr());
    let mut button = 0;
    let mut checked = 0;
    let hr = unsafe { TaskDialogIndirect(&config, &mut button, std::ptr::null_mut(), &mut checked) };
    if hr < 0 {
        return (IDCANCEL, false);
    }
    (button, checked != 0)
}

pub fn format_size(bytes: u64) -> String {
    format_size_in(i18n::s(), bytes)
}

fn format_size_in(s: &Strings, bytes: u64) -> String {
    if bytes < 1024 {
        return fill(s.pick(&s.bytes, bytes), &[("n", &bytes)]);
    }
    let units = [s.kilobytes, s.megabytes, s.gigabytes, s.terabytes];
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < units.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    let number = if v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}").replace('.', s.decimal_separator)
    };
    format!("{number} {}", units[unit])
}

pub fn format_duration(secs: u64) -> String {
    match secs {
        0..=4 => tr!(few_seconds).into(),
        5..=59 => trn!(seconds, secs),
        60..=3599 => tr!(minutes_seconds, m = secs / 60, s = secs % 60),
        _ => tr!(hours_minutes, h = secs / 3600, m = (secs % 3600) / 60),
    }
}

/// Shortens a Contact ID for display, e.g. `3f8a9c…e41b07`.
pub fn short_id(id: &str) -> String {
    if id.len() <= 16 {
        id.to_string()
    } else {
        format!("{}…{}", &id[..8], &id[id.len() - 6..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn sizes() {
        let en = Lang::En.strings();
        assert_eq!(format_size_in(en, 1), "1 byte");
        assert_eq!(format_size_in(en, 512), "512 bytes");
        assert_eq!(format_size_in(en, 1536), "1.5 KB");
        assert_eq!(format_size_in(en, 250 * 1024 * 1024), "250 MB");
        let hr = Lang::Hr.strings();
        assert_eq!(format_size_in(hr, 1536), "1,5 kB");
        assert_eq!(format_size_in(hr, 3), "3 bajta");
        assert_eq!(format_size_in(Lang::Sl.strings(), 2), "2 bajta");
    }
}
