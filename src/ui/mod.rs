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

use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, S_OK, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_INFORMATION_ICON,
                TDCBF_CLOSE_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_ENABLE_HYPERLINKS,
                TDF_POSITION_RELATIVE_TO_WINDOW, TDF_USE_COMMAND_LINKS, TDF_USE_HICON_MAIN,
                TDN_HYPERLINK_CLICKED, TaskDialogIndirect,
            },
            WindowsAndMessaging::{
                GetWindowTextLengthW, GetWindowTextW, IDCANCEL, IDYES, LoadIconW, MB_ICONERROR,
                MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MessageBoxW, SendMessageW,
                SetWindowTextW,
            },
        },
    },
    core::HRESULT,
};

use crate::i18n::{self, Strings, fill, tr, trn};

pub const APP_NAME: &str = "Dodavo";
/// The author, the website (the repository on GitHub) and the years of the
/// copyright notice, from Cargo.toml (the years from
/// `[package.metadata.dodavo]`, passed on by build.rs, which also writes all
/// three into the program's version resource).
pub const APP_AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
pub const APP_WEBSITE: &str = env!("CARGO_PKG_REPOSITORY");
pub const COPYRIGHT_YEARS: &str = env!("DODAVO_COPYRIGHT_YEARS");
/// The page that Help → Donate opens. The help pages name it too (a test
/// checks them).
pub const DONATE_URL: &str = "https://paypal.me/hrvix";

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A text with the `\r\n` line ends a multiline edit control needs.
pub fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// A menu text without the shortcut shown after the tab ("&Open\tCtrl+O").
pub fn without_shortcut(text: &str) -> &str {
    text.split_once('\t').map_or(text, |(shown, _)| shown)
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

/// The main icon of a task dialog.
pub enum TaskIcon {
    Information,
    /// Dodavo's own icon (resource 1, see res/app.rc).
    Program,
}

/// A Windows task dialog titled "Dodavo", shown with [`task_dialog`].
pub struct TaskDialog<'a> {
    pub owner: HWND,
    pub instruction: &'a str,
    pub content: &'a str,
    /// Buttons of its own, with their ids.
    pub buttons: &'a [(i32, &'a str)],
    /// Shows `buttons` as command links instead of push buttons.
    pub command_links: bool,
    /// Adds a Close button (`IDCLOSE`).
    pub close_button: bool,
    /// The button Enter chooses (0: the first one).
    pub default_button: i32,
    pub icon: TaskIcon,
    /// A check box such as "Don't ask me again".
    pub verification: Option<&'a str>,
    /// Makes the `<a href="…">` in `content` links. Called with the task
    /// dialog (as the owner of anything it shows) and the href.
    pub on_link: Option<fn(HWND, &str)>,
}

/// Passes a link chosen in a task dialog to its `on_link`.
unsafe extern "system" fn task_dialog_callback(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
    data: isize,
) -> HRESULT {
    if msg == TDN_HYPERLINK_CLICKED as u32 && data != 0 {
        // SAFETY: `data` points to the `on_link` of the `TaskDialog` that
        // `task_dialog` is showing, which outlives the dialog.
        let on_link = unsafe { &*(data as *const fn(HWND, &str)) };
        on_link(hwnd, &from_wide_ptr(lparam as *const u16));
    }
    S_OK
}

/// Shows a task dialog. Returns the id of the button chosen (`IDCANCEL` for
/// Escape or the title bar's close button) and the state of the check box, or
/// `None` if the dialog could not be shown.
pub fn task_dialog(d: &TaskDialog) -> Option<(i32, bool)> {
    let title = wide(APP_NAME);
    let instruction = wide(d.instruction);
    let content = wide(d.content);
    let verification = d.verification.map(wide);
    let texts: Vec<Vec<u16>> = d.buttons.iter().map(|(_, t)| wide(t)).collect();
    let buttons: Vec<TASKDIALOG_BUTTON> = d
        .buttons
        .iter()
        .zip(&texts)
        .map(|((id, _), text)| TASKDIALOG_BUTTON {
            nButtonID: *id,
            pszButtonText: text.as_ptr(),
        })
        .collect();
    let mut config: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    config.cbSize = size_of::<TASKDIALOGCONFIG>() as u32;
    config.hwndParent = d.owner;
    config.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW;
    if d.command_links {
        config.dwFlags |= TDF_USE_COMMAND_LINKS;
    }
    config.Anonymous1 = TASKDIALOGCONFIG_0 {
        pszMainIcon: TD_INFORMATION_ICON,
    };
    if let TaskIcon::Program = d.icon {
        // Shared, never destroyed; without it the dialog has no icon.
        let icon = unsafe { LoadIconW(GetModuleHandleW(std::ptr::null()), 1 as _) };
        config.Anonymous1 = TASKDIALOGCONFIG_0 { hMainIcon: icon };
        config.dwFlags |= TDF_USE_HICON_MAIN;
    }
    if d.on_link.is_some() {
        config.dwFlags |= TDF_ENABLE_HYPERLINKS;
        config.pfCallback = Some(task_dialog_callback);
        config.lpCallbackData = d.on_link.as_ref().map_or(0, |f| f as *const _ as isize);
    }
    config.dwCommonButtons = if d.close_button {
        TDCBF_CLOSE_BUTTON
    } else {
        0
    };
    config.pszWindowTitle = title.as_ptr();
    config.pszMainInstruction = instruction.as_ptr();
    config.pszContent = content.as_ptr();
    config.cButtons = buttons.len() as u32;
    config.pButtons = buttons.as_ptr();
    config.nDefaultButton = d.default_button;
    config.pszVerificationText = verification
        .as_ref()
        .map_or(std::ptr::null(), |v| v.as_ptr());
    let mut button = 0;
    let mut checked = 0;
    let hr =
        unsafe { TaskDialogIndirect(&config, &mut button, std::ptr::null_mut(), &mut checked) };
    (hr >= 0).then_some((button, checked != 0))
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
    task_dialog(&TaskDialog {
        owner,
        instruction,
        content,
        buttons: choices,
        command_links: true,
        close_button: close,
        default_button: choices.first().map_or(0, |c| c.0),
        icon: TaskIcon::Information,
        verification,
        on_link: None,
    })
    .unwrap_or((IDCANCEL, false))
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
