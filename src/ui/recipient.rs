//! "Add Recipient" / "Edit Recipient" dialog.

use std::{cell::RefCell, rc::Rc};

use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IDCANCEL};

use super::{
    error, get_text,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{app, config::Contact, i18n::tr, identity};

const ID_NAME: u16 = 10;
const ID_CONTACT_ID: u16 = 11;
const ID_OK: u16 = 1;

struct RecipientDialog {
    /// The contact being edited (`None` when adding one).
    existing: Option<Contact>,
    /// The texts the fields start with.
    initial: Contact,
    result: Rc<RefCell<Option<Contact>>>,
    name: HWND,
    id: HWND,
}

/// Shows the dialog modally; returns the new or edited contact.
pub fn show(owner: HWND, existing: Option<&Contact>) -> Option<Contact> {
    let initial = existing.cloned().unwrap_or(Contact {
        name: String::new(),
        id: String::new(),
    });
    show_with(owner, existing.cloned(), initial)
}

/// Shows the "Add Recipient" dialog with the fields filled in, for example
/// with what an unknown sender told about themselves.
pub fn show_new(owner: HWND, initial: Contact) -> Option<Contact> {
    show_with(owner, None, initial)
}

fn show_with(owner: HWND, existing: Option<Contact>, initial: Contact) -> Option<Contact> {
    let result = Rc::new(RefCell::new(None));
    let handler = RecipientDialog {
        existing,
        initial,
        result: result.clone(),
        name: std::ptr::null_mut(),
        id: std::ptr::null_mut(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: if handler.existing.is_some() {
                tr!(edit_recipient_title)
            } else {
                tr!(add_recipient_title)
            },
            client: (460, 230),
            center_on: owner,
            ..Default::default()
        },
        Box::new(handler),
    );
    window::run_modal(owner, hwnd);
    result.take()
}

impl RecipientDialog {
    fn validate(&self, w: &Window) -> Option<Contact> {
        let name = get_text(self.name).trim().to_string();
        if name.is_empty() {
            error(w.hwnd, tr!(enter_recipient_name));
            window::focus(self.name);
            return None;
        }
        let Some(id) = identity::parse_contact_id(&get_text(self.id)) else {
            error(w.hwnd, tr!(invalid_contact_id));
            window::focus(self.id);
            return None;
        };
        let id = id.to_string();
        if app::with(|a| a.my_id == id) {
            error(w.hwnd, tr!(own_contact_id));
            return None;
        }
        let cfg = app::cfg();
        let others = cfg
            .contacts
            .iter()
            .filter(|c| Some(*c) != self.existing.as_ref());
        for c in others {
            if c.id.eq_ignore_ascii_case(&id) {
                error(w.hwnd, &tr!(contact_id_taken, name = c.name));
                return None;
            }
            if c.name.eq_ignore_ascii_case(&name) {
                error(w.hwnd, &tr!(name_taken, name = c.name));
                return None;
            }
        }
        Some(Contact { name, id })
    }
}

impl Handler for RecipientDialog {
    fn on_create(&mut self, w: &Window) {
        w.caption(tr!(recipient_name_label), (14, 12, 432, 18));
        self.name = w.edit(&self.initial.name, ID_NAME, (14, 32, 432, 24));
        w.caption(tr!(contact_id_label), (14, 66, 432, 18));
        self.id = w.edit(&self.initial.id, ID_CONTACT_ID, (14, 86, 432, 24));
        w.label(tr!(recipient_hint), (14, 118, 432, 52));
        w.button(tr!(ok), ID_OK, (260, 188, 90, 30));
        w.button(tr!(cancel), IDCANCEL as u16, (356, 188, 90, 30));
        w.default_id.set(ID_OK);
        window::focus(self.name);
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        if id == ID_OK {
            if let Some(contact) = self.validate(w) {
                *self.result.borrow_mut() = Some(contact);
                w.close();
            }
            return true;
        }
        false
    }
}
