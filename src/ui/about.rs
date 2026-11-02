//! Help → About: the version, the author, the website and the license notice
//! (the "Appropriate Legal Notices" the GPL asks an interactive program to
//! show), in a Windows task dialog with a link to the website and a button
//! that opens the license.

use std::io;

use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IDCLOSE};

use super::{
    APP_AUTHOR, APP_NAME, APP_WEBSITE, COPYRIGHT_YEARS, TaskDialog, TaskIcon, error, shell,
    task_dialog,
};
use crate::i18n::tr;

/// The button that opens the license.
const ID_LICENSE: i32 = 100;

/// Opens a page of ours in the web browser: the website when its link here is
/// chosen (with the mouse, or with Tab and Enter), the page of Help → Donate.
/// Says so when that fails, for example when no web browser is set up.
pub fn open_link(owner: HWND, url: &str) {
    // The href of the link, which this dialog wrote itself, or a constant.
    if url.starts_with("https://") && !shell::open_url(owner, url) {
        error(
            owner,
            &tr!(website_failed, error = io::Error::last_os_error()),
        );
    }
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
    let chosen = task_dialog(&TaskDialog {
        owner,
        instruction: &tr!(
            about_instruction,
            app = APP_NAME,
            version = env!("CARGO_PKG_VERSION")
        ),
        content: &content(),
        buttons: &[(ID_LICENSE, tr!(menu_license))],
        command_links: false,
        close_button: true,
        default_button: IDCLOSE,
        icon: TaskIcon::Program,
        verification: None,
        on_link: Some(open_link),
    });
    // Opened after the task dialog has closed, as a dialog of the main window.
    if let Some((ID_LICENSE, _)) = chosen {
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
        // README.md repeats the notice; the version resource and Setup take
        // it from Cargo.toml (build.rs, installer\Dodavo.iss).
        let readme = include_str!("../../README.md");
        assert!(
            readme.contains(&format!("Copyright © {COPYRIGHT_YEARS} {APP_AUTHOR}")),
            "README.md: the copyright notice must match Cargo.toml"
        );
    }
}
