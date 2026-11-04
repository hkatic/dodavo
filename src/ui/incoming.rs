//! The window asking whether to accept what someone sends. It has the dialog
//! role, so when it comes to the front screen readers read who sends what and
//! whether it fits on the disk before the focused button; the notification
//! says who sends what when it stays in the background.
//!
//! All the offers of one sender that wait for an answer share a window, with
//! one answer. Several selected items arrive as that many offers, and windows
//! of their own would open exactly on top of each other: answering the one in
//! front would leave the others behind it, waiting unseen.

use std::path::{Path, PathBuf};

use windows_sys::Win32::{
    Foundation::{HWND, LRESULT, WPARAM},
    UI::WindowsAndMessaging::*,
};

use super::{
    accessibility, confirm, crlf, error, format_size, get_text, set_text, shell, short_id, tray,
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
const ID_ITEMS: u16 = 15;
const EN_KILLFOCUS: u16 = 0x0200;

const WIDTH: i32 = 480;
const CONTENT_W: i32 = 452;
/// The room the list of items takes when there are several.
const ITEMS_H: i32 = 96;
const TIMER_ANNOUNCE: usize = 1;
/// Offers of a selection arrive one by one; the number is read out once they
/// have stopped coming.
const ANNOUNCE_AFTER_MS: u32 = 300;

struct Incoming {
    from: String,
    name: String,
    known: bool,
    /// What waits for the answer, with the id of each transfer.
    offers: Vec<(u64, Offer)>,
    /// Set once an answer was sent (or is no longer needed).
    answered: bool,
    headline: HWND,
    description: HWND,
    items_caption: HWND,
    items: HWND,
    warning: HWND,
    add_contact: HWND,
    save_caption: HWND,
    dir: HWND,
    browse: HWND,
    space: HWND,
    accept_button: HWND,
    decline_button: HWND,
    /// The width of each of the two buttons, in 96-DPI units.
    button_w: i32,
}

/// Whether `offer` fits into `dir`, with `folder` deciding what is kept of a
/// folder.
pub fn check_space(offer: &Offer, dir: &Path, folder: ReceivedFolder) -> Space {
    space::check_space(offer, dir, &std::env::temp_dir(), folder)
}

/// Shows an offer: in the window of the same sender's waiting offers, or in a
/// new one. Returns the window. If that sender's window is busy (a dialog of
/// its own is open), the offer gets a window of its own rather than wait.
pub fn open_or_add(id: u64, from: &str, name: &str, known: bool, offer: Offer) -> HWND {
    let open_windows: Vec<HWND> = app::with(|a| a.offers.values().copied().collect());
    let mut offer = Some(offer);
    for hwnd in open_windows {
        let added = window::with_handler::<Incoming, _>(hwnd, |s, w| {
            if s.from != from || s.answered {
                return false;
            }
            if let Some(offer) = offer.take() {
                s.add(w, id, offer);
            }
            true
        });
        if added == Some(true) {
            return hwnd;
        }
    }
    let Some(offer) = offer else {
        // Taken only by a window that then returned above.
        return std::ptr::null_mut();
    };
    open(id, from, name, known, offer)
}

fn open(id: u64, from: &str, name: &str, known: bool, offer: Offer) -> HWND {
    let null = std::ptr::null_mut();
    let handler = Incoming {
        from: from.to_string(),
        name: name.to_string(),
        known,
        offers: vec![(id, offer)],
        answered: false,
        headline: null,
        description: null,
        items_caption: null,
        items: null,
        warning: null,
        add_contact: null,
        save_caption: null,
        dir: null,
        browse: null,
        space: null,
        accept_button: null,
        decline_button: null,
        button_w: 0,
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(incoming_title),
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            // Sized to the content in `on_create`.
            client: (WIDTH, 236),
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

/// The sender cancelled the offer `id` before we answered: the window closes
/// when that was the last one. Returns `false` if the window is busy and this
/// has to be retried.
pub fn close_by_peer(hwnd: HWND, id: u64) -> bool {
    window::with_handler::<Incoming, _>(hwnd, |s, w| {
        s.offers.retain(|(offer_id, _)| *offer_id != id);
        if s.offers.is_empty() {
            s.answered = true;
            w.close();
        } else {
            s.show_offers(w);
        }
        tray::balloon(
            app::main_window(),
            tr!(cancelled_balloon_title),
            &tr!(cancelled_by_peer, name = s.name),
            None,
        );
    })
    .is_some()
}

/// How an item is described, with its size.
fn describe(offer: &Offer) -> String {
    match offer.kind {
        Kind::File => tr!(
            file_description,
            name = offer.name,
            size = format_size(offer.size)
        ),
        Kind::Folder => trn!(
            folder_description,
            offer.file_count,
            name = offer.name,
            size = format_size(offer.content_size)
        ),
    }
}

impl Incoming {
    fn update_space(&self) -> Space {
        let offers: Vec<&Offer> = self.offers.iter().map(|(_, offer)| offer).collect();
        let space = space::check_space_of(
            &offers,
            Path::new(get_text(self.dir).trim()),
            &std::env::temp_dir(),
            app::cfg().received_folder,
        );
        set_text(self.space, &space.text);
        space
    }

    fn add(&mut self, w: &Window, id: u64, offer: Offer) {
        self.offers.push((id, offer));
        self.show_offers(w);
        unsafe { SetTimer(w.hwnd, TIMER_ANNOUNCE, ANNOUNCE_AFTER_MS, None) };
    }

    /// Says what waits for an answer, and arranges the window for it.
    fn show_offers(&self, w: &Window) {
        let (headline, description) = match self.offers.as_slice() {
            [(_, offer)] => (
                match offer.kind {
                    Kind::File => tr!(wants_file, name = self.name),
                    Kind::Folder => tr!(wants_folder, name = self.name),
                },
                describe(offer),
            ),
            offers => {
                let total = offers
                    .iter()
                    .map(|(_, offer)| match offer.kind {
                        Kind::File => offer.size,
                        Kind::Folder => offer.content_size,
                    })
                    .fold(0u64, u64::saturating_add);
                (
                    trn!(wants_items, offers.len(), name = self.name),
                    tr!(items_total, size = format_size(total)),
                )
            }
        };
        set_text(self.headline, &headline);
        set_text(self.description, &description);
        let lines: Vec<String> = self.offers.iter().map(|(_, o)| describe(o)).collect();
        set_text(self.items, &crlf(&lines.join("\n")));
        self.update_space();
        self.layout(w);
    }

    /// Places everything below the description: the list only takes room
    /// when there are several items.
    fn layout(&self, w: &Window) {
        let several = self.offers.len() > 1;
        window::show(self.items_caption, several);
        window::show(self.items, several);
        let mut y = 66;
        if several {
            w.place(self.items_caption, (14, y - 4, CONTENT_W, 18));
            w.place(self.items, (14, y + 16, CONTENT_W, ITEMS_H - 26));
            y += ITEMS_H;
        }
        if !self.known {
            w.place(self.warning, (14, y, CONTENT_W, 50));
            w.place(self.add_contact, (14, y + 56, CONTENT_W, 20));
            y += 82;
        }
        w.place(self.save_caption, (14, y, CONTENT_W, 18));
        w.place(self.dir, (14, y + 20, 360, 24));
        w.place(self.browse, (380, y + 19, 86, 26));
        w.place(self.space, (14, y + 52, CONTENT_W, 36));
        let right = 14 + CONTENT_W;
        w.place(
            self.accept_button,
            (right - 2 * self.button_w - 6, y + 102, self.button_w, 30),
        );
        w.place(
            self.decline_button,
            (right - self.button_w, y + 102, self.button_w, 30),
        );
        w.set_client_size(WIDTH, y + 170);
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
            let name_usable = self
                .offers
                .first()
                .is_some_and(|(_, offer)| !offer.sender_name.trim().is_empty())
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
        for (i, (id, offer)) in self.offers.iter().enumerate() {
            // Only the first window takes the focus, which then stays there
            // while the others open.
            app::accept_offer(*id, &self.from, &sender, offer, dir.clone(), i == 0);
        }
        w.close();
    }
}

impl Handler for Incoming {
    fn on_create(&mut self, w: &Window) {
        w.set_dialog_role();
        // The rectangles are set by `layout`.
        let later = (14, 0, CONTENT_W, 18);
        self.headline = w.label("", (14, 12, CONTENT_W, 20));
        w.make_bold(self.headline);
        w.live_region(self.headline);
        self.description = w.label("", (14, 36, CONTENT_W, 18));
        // A label and a list that only appear when there are several items.
        self.items_caption = w.caption(tr!(incoming_items_label), later);
        self.items = w.read_only_text("", ID_ITEMS, later);

        if !self.known {
            self.warning = w.label(
                &tr!(unknown_sender_warning, id = short_id(&self.from)),
                later,
            );
            self.add_contact = w.checkbox(
                &tr!(add_to_recipients, name = self.name.replace('&', "&&")),
                ID_ADD_CONTACT,
                false,
                later,
            );
        }

        self.save_caption = w.caption(tr!(save_to), later);
        let suggested = app::cfg().suggested_save_dir();
        self.dir = w.edit(&suggested.display().to_string(), ID_DIR, later);
        self.browse = w.button(tr!(browse), ID_BROWSE, later);
        self.space = w.label("", later);

        self.button_w = w
            .button_width(tr!(accept), 96)
            .max(w.button_width(tr!(decline), 96));
        self.accept_button = w.button(tr!(accept), ID_ACCEPT, later);
        self.decline_button = w.button(tr!(decline), ID_DECLINE, later);
        self.show_offers(w);
        w.default_id.set(ID_ACCEPT);
        window::focus(self.accept_button);
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

    fn on_message(&mut self, w: &Window, msg: u32, wp: WPARAM, _lp: isize) -> Option<LRESULT> {
        if msg == WM_TIMER && wp == TIMER_ANNOUNCE {
            unsafe { KillTimer(w.hwnd, TIMER_ANNOUNCE) };
            accessibility::announce(self.headline);
            return Some(0);
        }
        None
    }

    fn on_destroy(&mut self, _w: &Window) {
        for (id, _) in &self.offers {
            if !self.answered {
                app::net_send(Command::Respond {
                    id: *id,
                    accept: None,
                });
            }
            app::offer_window_closed(*id);
        }
    }
}
