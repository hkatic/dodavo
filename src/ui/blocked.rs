//! The Blocked Senders dialog: the senders whose offers are declined without
//! asking (see `Config::blocked`), and Unblock. A change takes effect at once,
//! whatever happens to the Settings dialog it is opened from.

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        IDCANCEL, LB_ADDSTRING, LB_ERR, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL,
        LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, WS_EX_CLIENTEDGE, WS_TABSTOP, WS_VSCROLL,
    },
};

use super::{
    accessibility, send, short_id,
    window::{self, CreateOptions, Handler, Window},
    wide,
};
use crate::{app, config::Contact, i18n::tr};

const ID_LIST: u16 = 10;
const ID_UNBLOCK: u16 = 11;

const WIDTH: i32 = 460;

struct BlockedDialog {
    list: HWND,
    /// "No sender is blocked", shown when the list is empty.
    none: HWND,
    unblock: HWND,
    close: HWND,
    /// What the list shows, in its order.
    shown: Vec<Contact>,
}

pub fn show(owner: HWND) {
    let null = std::ptr::null_mut();
    let handler = BlockedDialog {
        list: null,
        none: null,
        unblock: null,
        close: null,
        shown: Vec::new(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(blocked_title),
            client: (WIDTH, 300),
            center_on: owner,
            ..Default::default()
        },
        Box::new(handler),
    );
    window::run_modal(owner, hwnd);
}

/// The blocked senders in the order of their names.
fn sorted() -> Vec<Contact> {
    let mut blocked = app::cfg().blocked;
    blocked.sort_by_cached_key(|c| c.name.to_lowercase());
    blocked
}

impl BlockedDialog {
    /// Shows the blocked senders, with the one at `select` (or the last one)
    /// selected.
    fn fill(&mut self, select: usize) {
        self.shown = sorted();
        send(self.list, LB_RESETCONTENT, 0, 0);
        for c in &self.shown {
            let entry = tr!(blocked_entry, name = c.name, id = short_id(&c.id));
            send(self.list, LB_ADDSTRING, 0, wide(&entry).as_ptr() as isize);
        }
        let empty = self.shown.is_empty();
        window::show(self.none, empty);
        window::enable(self.unblock, !empty);
        if !empty {
            send(self.list, LB_SETCURSEL, select.min(self.shown.len() - 1), 0);
        }
    }

    fn unblock(&mut self) {
        let index = send(self.list, LB_GETCURSEL, 0, 0);
        if index == LB_ERR as isize {
            return;
        }
        let Some(contact) = self.shown.get(index as usize).cloned() else {
            return;
        };
        app::update_cfg(|c| {
            c.unblock(&contact.id);
        });
        self.fill(index as usize);
        if self.shown.is_empty() {
            // The button is disabled now, and the focus must not stay on it.
            window::focus(self.close);
            accessibility::announce(self.none);
        } else {
            window::focus(self.list);
        }
    }
}

impl Handler for BlockedDialog {
    fn on_create(&mut self, w: &Window) {
        // Screen readers read "No sender is blocked" when the dialog opens.
        w.set_dialog_role();
        // The caption right before the list gives it its accessible name.
        w.caption(tr!(blocked_list_label), (14, 12, WIDTH - 28, 18));
        self.list = w.add_ex(
            "LISTBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32,
            WS_EX_CLIENTEDGE,
            ID_LIST,
            (14, 32, WIDTH - 28, 180),
        );
        self.none = w.label(tr!(blocked_none), (14, 220, WIDTH - 28, 18));
        w.live_region(self.none);
        self.unblock = w.button(tr!(unblock), ID_UNBLOCK, (14, 252, 110, 30));
        // Escape and Enter close it too; it needs no access key.
        self.close = w.button(tr!(close), IDCANCEL as u16, (WIDTH - 14 - 90, 252, 90, 30));
        w.default_id.set(IDCANCEL as u16);
        self.fill(0);
        window::focus(if self.shown.is_empty() {
            self.close
        } else {
            self.list
        });
    }

    fn on_command(&mut self, _w: &Window, id: u16, _code: u16) -> bool {
        if id == ID_UNBLOCK {
            self.unblock();
            return true;
        }
        false
    }
}
