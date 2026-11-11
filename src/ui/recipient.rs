//! "Add Recipient" / "Edit Recipient" dialog.

use std::{cell::RefCell, rc::Rc};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{CBN_SELCHANGE, IDCANCEL},
};

use super::{
    error, get_text,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    config::{Contact, ContactLimit},
    i18n::tr,
    identity,
    limit::{self, Unit},
};

const ID_NAME: u16 = 10;
const ID_CONTACT_ID: u16 = 11;
const ID_LIMIT: u16 = 12;
const ID_LIMIT_SIZE: u16 = 13;
const ID_LIMIT_UNIT: u16 = 14;
const ID_OK: u16 = 1;

/// The places of the limit list's choices.
const LIMIT_GENERAL: usize = 0;
const LIMIT_NONE: usize = 1;
const LIMIT_OWN: usize = 2;

struct RecipientDialog {
    /// The contact being edited (`None` when adding one).
    existing: Option<Contact>,
    /// The texts the fields start with.
    initial: Contact,
    result: Rc<RefCell<Option<Contact>>>,
    name: HWND,
    id: HWND,
    /// Which limit holds for the recipient, and the size and unit of one of
    /// their own.
    limit: HWND,
    limit_size: HWND,
    limit_unit: HWND,
}

/// Shows the dialog modally; returns the new or edited contact.
pub fn show(owner: HWND, existing: Option<&Contact>) -> Option<Contact> {
    let initial = existing.cloned().unwrap_or(Contact::new("", ""));
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
        limit: std::ptr::null_mut(),
        limit_size: std::ptr::null_mut(),
        limit_unit: std::ptr::null_mut(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: if handler.existing.is_some() {
                tr!(edit_recipient_title)
            } else {
                tr!(add_recipient_title)
            },
            client: (460, 290),
            center_on: owner,
            ..Default::default()
        },
        Box::new(handler),
    );
    window::run_modal(owner, hwnd);
    result.take()
}

impl RecipientDialog {
    /// The size and its unit are for a limit of the recipient's own.
    fn update_enabled(&self) {
        let own = window::combo_selection(self.limit) == Some(LIMIT_OWN);
        window::enable(self.limit_size, own);
        window::enable(self.limit_unit, own);
    }

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
        let limit = match window::combo_selection(self.limit) {
            Some(LIMIT_NONE) => ContactLimit::Unlimited,
            Some(LIMIT_OWN) => {
                let unit = window::combo_selection(self.limit_unit)
                    .and_then(|i| Unit::ALL.get(i).copied())
                    .unwrap_or(Unit::Mb);
                match limit::from_input(&get_text(self.limit_size), unit) {
                    Ok(bytes) => ContactLimit::Bytes(bytes),
                    Err(text) => {
                        error(w.hwnd, &text);
                        window::focus(self.limit_size);
                        return None;
                    }
                }
            }
            _ => ContactLimit::General,
        };
        Some(Contact { name, id, limit })
    }
}

impl Handler for RecipientDialog {
    fn on_create(&mut self, w: &Window) {
        w.caption(tr!(recipient_name_label), (14, 12, 432, 18));
        self.name = w.edit(&self.initial.name, ID_NAME, (14, 32, 432, 24));
        w.caption(tr!(contact_id_label), (14, 66, 432, 18));
        self.id = w.edit(&self.initial.id, ID_CONTACT_ID, (14, 86, 432, 24));
        w.caption(tr!(recipient_limit_label), (14, 122, 432, 18));
        let choices = [
            tr!(recipient_limit_general),
            tr!(recipient_limit_none),
            tr!(recipient_limit_own),
        ];
        // A limit of their own starts from the one in the settings.
        let (selected, bytes) = match self.initial.limit {
            ContactLimit::General => (LIMIT_GENERAL, app::with(|a| a.cfg.size_limit)),
            ContactLimit::Unlimited => (LIMIT_NONE, app::with(|a| a.cfg.size_limit)),
            ContactLimit::Bytes(bytes) => (LIMIT_OWN, bytes),
        };
        let (number, unit) = limit::to_input(bytes);
        self.limit = w.combobox(&choices, selected, ID_LIMIT, (14, 142, 250, 24));
        // The list before them is no label, so these two get their names.
        self.limit_size = w.number_edit(&number.to_string(), 7, ID_LIMIT_SIZE, (272, 142, 84, 24));
        w.set_accessible_name(self.limit_size, tr!(limit_number_name));
        let units = Unit::ALL.map(Unit::name);
        let unit = Unit::ALL.iter().position(|&u| u == unit).unwrap_or(0);
        self.limit_unit = w.combobox(&units, unit, ID_LIMIT_UNIT, (362, 142, 84, 24));
        w.set_accessible_name(self.limit_unit, tr!(limit_unit_name));
        w.label(tr!(recipient_hint), (14, 178, 432, 52));
        w.button(tr!(ok), ID_OK, (260, 248, 90, 30));
        w.button(tr!(cancel), IDCANCEL as u16, (356, 248, 90, 30));
        w.default_id.set(ID_OK);
        self.update_enabled();
        window::focus(self.name);
    }

    fn on_command(&mut self, w: &Window, id: u16, code: u16) -> bool {
        if id == ID_LIMIT && code as u32 == CBN_SELCHANGE {
            self.update_enabled();
            return true;
        }
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
