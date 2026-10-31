//! Help → License: the license of Dodavo (GNU GPL, version 3 or later) and
//! the licenses of the open-source components built into it, which their
//! licenses require to be passed on with the program. The texts are the
//! files `LICENSE` and `res/third-party-licenses.txt` (made by
//! `installer\licenses.ps1`), in English.

use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Controls::{EM_SCROLLCARET, EM_SETSEL},
        WindowsAndMessaging::{CBN_SELCHANGE, IDCANCEL},
    },
};

use super::{
    APP_NAME, APP_WEBSITE, crlf, send, set_text,
    window::{self, CreateOptions, Handler, Window},
};
use crate::i18n::tr;

const ID_SHOW: u16 = 10;
const ID_TEXT: u16 = 11;
const WIDTH: i32 = 600;
/// The height of the text field; the window's height follows from it and
/// from the height of the introduction.
const TEXT_HEIGHT: i32 = 214;

const DODAVO_LICENSE: &str = include_str!("../../LICENSE");
const THIRD_PARTY_LICENSES: &str = include_str!("../../res/third-party-licenses.txt");

/// The texts, in the order of the Show list.
const TEXTS: [&str; 2] = [DODAVO_LICENSE, THIRD_PARTY_LICENSES];

struct LicenseWindow {
    show: HWND,
    text: HWND,
}

/// Shows the window modally.
pub fn show(owner: HWND) {
    let handler = LicenseWindow {
        show: std::ptr::null_mut(),
        text: std::ptr::null_mut(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(license_title),
            // Set in `on_create`, once the introduction is measured.
            client: (WIDTH, 400),
            center_on: owner,
            ..Default::default()
        },
        Box::new(handler),
    );
    window::run_modal(owner, hwnd);
}

impl LicenseWindow {
    /// Shows text `index`, to be read from the top. Only the text shown is
    /// converted for the field (each is some hundred kilobytes).
    fn show_text(&self, index: usize) {
        let Some(text) = TEXTS.get(index) else {
            return;
        };
        set_text(self.text, &crlf(text));
        send(self.text, EM_SETSEL, 0, 0);
        send(self.text, EM_SCROLLCARET, 0, 0);
    }
}

impl Handler for LicenseWindow {
    fn on_create(&mut self, w: &Window) {
        // Screen readers read the introduction when the window opens. It
        // ends with the address of the source code, so it is as high as its
        // text needs at the current text size.
        w.set_dialog_role();
        let intro = tr!(license_intro, app = APP_NAME, website = APP_WEBSITE);
        let intro_h = w.text_height(&intro, WIDTH - 28);
        w.label(&intro, (14, 12, WIDTH - 28, intro_h));
        let y = 12 + intro_h + 8;
        w.caption(tr!(license_show_label), (14, y, WIDTH - 28, 18));
        self.show = w.combobox(
            &[tr!(license_dodavo), tr!(license_components)],
            0,
            ID_SHOW,
            (14, y + 20, WIDTH - 28, 24),
        );
        // The caption names the text field for screen readers.
        w.caption(tr!(license_text_label), (14, y + 54, WIDTH - 28, 18));
        self.text = w.read_only_text("", ID_TEXT, (14, y + 74, WIDTH - 28, TEXT_HEIGHT));
        self.show_text(0);
        let close = tr!(close);
        let close_w = w.button_width(close, 90);
        let button_y = y + 74 + TEXT_HEIGHT + 12;
        w.button(
            close,
            IDCANCEL as u16,
            (WIDTH - 14 - close_w, button_y, close_w, 30),
        );
        w.set_client_size(WIDTH, button_y + 30 + 14);
        // Enter closes the window too (the text is read-only).
        w.default_id.set(IDCANCEL as u16);
        window::focus(self.text);
    }

    fn on_command(&mut self, _w: &Window, id: u16, code: u16) -> bool {
        if id == ID_SHOW && code as u32 == CBN_SELCHANGE {
            if let Some(index) = window::combo_selection(self.show) {
                self.show_text(index);
            }
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_built_in() {
        assert!(DODAVO_LICENSE.contains("GNU GENERAL PUBLIC LICENSE"));
        assert!(DODAVO_LICENSE.contains("Version 3, 29 June 2007"));
        // The components that ship with Dodavo, with their licenses.
        for name in ["iroh", "tokio", "ring", "webpki-roots", "attohttpc", "zip"] {
            assert!(
                THIRD_PARTY_LICENSES.contains(&format!("{name} ")),
                "{name} is missing from res/third-party-licenses.txt"
            );
        }
        // The Rust standard library, which is part of every build too.
        assert!(THIRD_PARTY_LICENSES.contains("THE RUST STANDARD LIBRARY"));
        assert!(THIRD_PARTY_LICENSES.contains("- hashbrown "));
        assert_eq!(crlf("a\nb\r\nc"), "a\r\nb\r\nc");
    }
}
