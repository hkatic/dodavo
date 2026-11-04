//! The window asking whether to accept an incoming transfer. It has the
//! dialog role, so when it comes to the front screen readers read who sends
//! what and whether it fits on the disk before the focused button; the
//! notification says who sends what when it stays in the background.

use std::path::{Path, PathBuf};

use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};

use super::{
    confirm, error, format_size, get_text, set_text, shell, short_id, tray,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    config::Contact,
    i18n::{tr, trn},
    net::{Command, Kind, Offer, ReceivedFolder},
    space::{self, Space},
};

const ID_BROWSE: u16 = 10;
const ID_ACCEPT: u16 = 11;
const ID_DECLINE: u16 = 12;
const ID_ADD_CONTACT: u16 = 13;
const ID_DIR: u16 = 14;
const EN_KILLFOCUS: u16 = 0x0200;

struct Incoming {
    id: u64,
    from: String,
    name: String,
    known: bool,
    offer: Offer,
    /// Set once an answer was sent (or is no longer needed).
    answered: bool,
    dir: HWND,
    space: HWND,
    add_contact: HWND,
}

/// Whether `offer` fits into `dir`, with `folder` deciding what is kept of a
/// folder.
pub fn check_space(offer: &Offer, dir: &Path, folder: ReceivedFolder) -> Space {
    space::check_space(offer, dir, &std::env::temp_dir(), folder)
}

pub fn open(id: u64, from: &str, name: &str, known: bool, offer: Offer) -> HWND {
    let height = if known { 236 } else { 318 };
    let handler = Incoming {
        id,
        from: from.to_string(),
        name: name.to_string(),
        known,
        offer,
        answered: false,
        dir: std::ptr::null_mut(),
        space: std::ptr::null_mut(),
        add_contact: std::ptr::null_mut(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(incoming_title),
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            client: (480, height),
            center_on: app::main_window(),
            ..Default::default()
        },
        Box::new(handler),
    );
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        FlashWindow(hwnd, 1);
    }
    tray::balloon(
        app::main_window(),
        tr!(incoming_balloon_title),
        &tr!(incoming_balloon, name = name),
        None,
    );
    hwnd
}

/// The sender cancelled before we answered. Returns `false` if the window is
/// busy and this has to be retried.
pub fn close_by_peer(hwnd: HWND) -> bool {
    window::with_handler::<Incoming, _>(hwnd, |s, w| {
        s.answered = true;
        let name = s.name.clone();
        w.close();
        tray::balloon(
            app::main_window(),
            tr!(cancelled_balloon_title),
            &tr!(cancelled_by_peer, name = name),
            None,
        );
    })
    .is_some()
}

impl Incoming {
    fn update_space(&self) -> Space {
        let space = check_space(
            &self.offer,
            Path::new(get_text(self.dir).trim()),
            app::cfg().received_folder,
        );
        set_text(self.space, &space.text);
        space
    }

    fn accept(&mut self, w: &Window) {
        let text = get_text(self.dir);
        let dir = PathBuf::from(text.trim());
        if text.trim().is_empty() || !dir.is_absolute() {
            error(w.hwnd, tr!(choose_save_folder));
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            error(
                w.hwnd,
                &tr!(folder_unusable, path = dir.display(), error = e),
            );
            return;
        }
        let space = self.update_space();
        if !space.fits {
            error(w.hwnd, &space.text);
            return;
        }
        if space.enough == Some(false)
            && !confirm(w.hwnd, &tr!(space_confirm, details = space.text))
        {
            return;
        }
        let mut sender = self.name.clone();
        if !self.known && window::is_checked(self.add_contact) {
            let contact = Contact {
                name: self.name.clone(),
                id: self.from.clone(),
            };
            // The name is what the sender said, so it is only stored as is when
            // it is a usable, free name; otherwise the user chooses one, with
            // the same checks as "Add Recipient".
            let name_usable = !self.offer.sender_name.trim().is_empty()
                && !app::cfg()
                    .contacts
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(&contact.name));
            let contact = if name_usable {
                Some(contact)
            } else {
                super::recipient::show_new(w.hwnd, contact)
            };
            let Some(contact) = contact else {
                return;
            };
            sender = contact.name.clone();
            app::update_cfg(|c| {
                c.contacts.push(contact);
                c.contacts.sort_by_key(|c| c.name.to_lowercase());
            });
            super::main_window::refresh_list();
        }
        self.answered = true;
        app::accept_offer(self.id, &self.from, &sender, &self.offer, dir, true);
        w.close();
    }
}

impl Handler for Incoming {
    fn on_create(&mut self, w: &Window) {
        w.set_dialog_role();
        let o = &self.offer;
        let (headline, description) = match o.kind {
            Kind::File => (
                tr!(wants_file, name = self.name),
                tr!(file_description, name = o.name, size = format_size(o.size)),
            ),
            Kind::Folder => (
                tr!(wants_folder, name = self.name),
                trn!(
                    folder_description,
                    o.file_count,
                    name = o.name,
                    size = format_size(o.content_size)
                ),
            ),
        };
        let title = w.label(&headline, (14, 12, 452, 20));
        w.make_bold(title);
        w.label(&description, (14, 36, 452, 18));

        let mut y = 66;
        if !self.known {
            w.label(
                &tr!(unknown_sender_warning, id = short_id(&self.from)),
                (14, y, 452, 50),
            );
            self.add_contact = w.checkbox(
                &tr!(add_to_recipients, name = self.name.replace('&', "&&")),
                ID_ADD_CONTACT,
                false,
                (14, y + 56, 452, 20),
            );
            y += 82;
        }

        w.caption(tr!(save_to), (14, y, 452, 18));
        let suggested = app::cfg().suggested_save_dir();
        self.dir = w.edit(
            &suggested.display().to_string(),
            ID_DIR,
            (14, y + 20, 360, 24),
        );
        w.button(tr!(browse), ID_BROWSE, (380, y + 19, 86, 26));
        self.space = w.label("", (14, y + 52, 452, 36));
        self.update_space();

        let button_w = w
            .button_width(tr!(accept), 96)
            .max(w.button_width(tr!(decline), 96));
        w.button(
            tr!(accept),
            ID_ACCEPT,
            (466 - 2 * button_w - 6, y + 102, button_w, 30),
        );
        w.button(
            tr!(decline),
            ID_DECLINE,
            (466 - button_w, y + 102, button_w, 30),
        );
        w.default_id.set(ID_ACCEPT);
        window::focus(w.item(ID_ACCEPT));
    }

    fn on_command(&mut self, w: &Window, id: u16, code: u16) -> bool {
        match id {
            ID_BROWSE => {
                let current = PathBuf::from(get_text(self.dir).trim());
                if let Some(dir) = shell::pick_folder(w.hwnd, tr!(pick_save_folder), Some(&current))
                {
                    set_text(self.dir, &dir.display().to_string());
                    self.update_space();
                }
            }
            // Checked when leaving the field rather than on every keystroke,
            // since looking up a half-typed network path can take a while.
            ID_DIR if code == EN_KILLFOCUS => {
                self.update_space();
            }
            ID_ACCEPT => self.accept(w),
            ID_DECLINE => w.close(),
            _ => return false,
        }
        true
    }

    fn on_destroy(&mut self, _w: &Window) {
        if !self.answered {
            app::net_send(Command::Respond {
                id: self.id,
                accept: None,
            });
        }
        app::offer_window_closed(self.id);
    }
}
