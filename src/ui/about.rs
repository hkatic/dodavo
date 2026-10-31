//! Help → About: the version, the author, the website and the license notice
//! (the "Appropriate Legal Notices" the GPL asks an interactive program to
//! show), in a Windows task dialog with a link to the website and a button
//! that opens the license.

use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, S_OK, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TDCBF_CLOSE_BUTTON,
                TDF_ALLOW_DIALOG_CANCELLATION, TDF_ENABLE_HYPERLINKS,
                TDF_POSITION_RELATIVE_TO_WINDOW, TDF_USE_HICON_MAIN, TDN_HYPERLINK_CLICKED,
                TaskDialogIndirect,
            },
            WindowsAndMessaging::{IDCLOSE, LoadIconW},
        },
    },
    core::HRESULT,
};

use super::{APP_AUTHOR, APP_NAME, APP_WEBSITE, COPYRIGHT_YEARS, from_wide_ptr, shell, wide};
use crate::i18n::tr;

/// The button that opens the license.
const ID_LICENSE: i32 = 100;

/// Opens the website when its link is chosen (with the mouse, or with Tab
/// and Enter).
unsafe extern "system" fn callback(
    _hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
    _data: isize,
) -> HRESULT {
    if msg == TDN_HYPERLINK_CLICKED as u32 {
        // The href of the link, which this dialog wrote itself.
        let url = from_wide_ptr(lparam as *const u16);
        if url.starts_with("https://") {
            shell::open_url(&url);
        }
    }
    S_OK
}

/// The text below the version: what Dodavo does, the copyright, the website
/// (as a link) and the license notice. The link markup stays out of the
/// translations.
fn content() -> String {
    format!(
        "{}\n\n© {COPYRIGHT_YEARS} {APP_AUTHOR}\n<a href=\"{APP_WEBSITE}\">{APP_WEBSITE}</a>\n\n{}",
        tr!(about_text),
        tr!(about_license),
    )
}

pub fn show(owner: HWND) {
    let title = wide(APP_NAME);
    let instruction = wide(&tr!(
        about_instruction,
        app = APP_NAME,
        version = env!("CARGO_PKG_VERSION")
    ));
    let content = wide(&content());
    let license = wide(tr!(menu_license));
    let buttons = [TASKDIALOG_BUTTON {
        nButtonID: ID_LICENSE,
        pszButtonText: license.as_ptr(),
    }];
    // The program icon (resource 1, see res/app.rc); shared, never destroyed.
    let icon = unsafe { LoadIconW(GetModuleHandleW(std::ptr::null()), 1 as _) };
    let icon_flag = if icon.is_null() {
        0
    } else {
        TDF_USE_HICON_MAIN
    };
    let mut config: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    config.cbSize = size_of::<TASKDIALOGCONFIG>() as u32;
    config.hwndParent = owner;
    config.dwFlags = TDF_ENABLE_HYPERLINKS
        | TDF_ALLOW_DIALOG_CANCELLATION
        | TDF_POSITION_RELATIVE_TO_WINDOW
        | icon_flag;
    config.dwCommonButtons = TDCBF_CLOSE_BUTTON;
    config.pszWindowTitle = title.as_ptr();
    config.Anonymous1 = TASKDIALOGCONFIG_0 { hMainIcon: icon };
    config.pszMainInstruction = instruction.as_ptr();
    config.pszContent = content.as_ptr();
    config.cButtons = buttons.len() as u32;
    config.pButtons = buttons.as_ptr();
    config.nDefaultButton = IDCLOSE;
    config.pfCallback = Some(callback);
    let mut button = 0;
    let hr = unsafe {
        TaskDialogIndirect(
            &config,
            &mut button,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    // Opened after the task dialog has closed, as a dialog of the main window.
    if hr >= 0 && button == ID_LICENSE {
        super::license::show(owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    /// The task dialog reads `<a …>` as a link, so the translations must not
    /// contain markup of their own.
    #[test]
    fn translations_have_no_markup() {
        for lang in Lang::ALL {
            let s = lang.strings();
            for text in [s.about_text, s.about_license] {
                assert!(!text.contains('<'), "{lang:?}: {text:?}");
            }
        }
    }

    #[test]
    fn notice_names_author_and_website() {
        assert_eq!(APP_AUTHOR, "Hrvoje Katić");
        assert_eq!(APP_WEBSITE, "https://github.com/hkatic/dodavo");
        let text = content();
        assert!(text.contains("© 2026 Hrvoje Katić"));
        assert!(text.contains(
            "<a href=\"https://github.com/hkatic/dodavo\">https://github.com/hkatic/dodavo</a>"
        ));
    }
}
