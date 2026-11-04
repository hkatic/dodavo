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

pub use crate::format::{format_duration, format_size};
pub use crate::platform::{from_wide_ptr, wide};

pub const APP_NAME: &str = "Dodavo";
/// The author, the website (the repository on GitHub) and the years of the
/// copyright notice, from Cargo.toml (the years from
/// `[package.metadata.dodavo]`, passed on by build.rs, which also writes all
/// three into the program's version resource).
pub const APP_AUTHOR: &str = dodavo_core::AUTHOR;
pub const APP_WEBSITE: &str = dodavo_core::WEBSITE;
pub const COPYRIGHT_YEARS: &str = dodavo_core::COPYRIGHT_YEARS;
pub use dodavo_core::DONATE_URL;

/// A text with the `\r\n` line ends a multiline edit control needs.
pub fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// A menu text without the shortcut shown after the tab ("&Open\tCtrl+O").
pub fn without_shortcut(text: &str) -> &str {
    text.split_once('\t').map_or(text, |(shown, _)| shown)
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

/// Shortens a Contact ID for display, e.g. `3f8a9c…e41b07`.
pub fn short_id(id: &str) -> String {
    if id.len() <= 16 {
        id.to_string()
    } else {
        format!("{}…{}", &id[..8], &id[id.len() - 6..])
    }
}
