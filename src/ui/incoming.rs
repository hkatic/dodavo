//! The window asking whether to accept what someone sends. It has the dialog
//! role, so when it comes to the front screen readers read who sends what and
//! whether it fits on the disk before the focused button; the notification
//! says who sends what when it stays in the background.
//!
//! All the offers of one sender that wait for an answer share a window, with
//! one answer. Several selected items arrive as that many offers, and windows
//! of their own would open exactly on top of each other: answering the one in
//! front would leave the others behind it, waiting unseen.

use std::{
    cell::Cell,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{HWND, LRESULT, WPARAM},
    UI::{Input::KeyboardAndMouse::GetFocus, WindowsAndMessaging::*},
};

use super::{
    accessibility, confirm, crlf, error, format_size, get_text, set_text, shell, short_id, tray,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app, archive,
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
/// Offers of a selection arrive one by one; the number is read out, and the
/// free space looked up, once they have stopped coming.
const ANNOUNCE_AFTER_MS: u32 = 300;
/// An item that arrived more recently than this is not covered by Accept: nobody
/// can have seen it, and Enter may be on its way already. It stays in the window.
const SEEN_AFTER: Duration = Duration::from_secs(1);

/// An offer that waits for the answer.
struct Waiting {
    id: u64,
    offer: Offer,
    since: Instant,
}

/// What an Accept at `now` covers, and what stays. The first item is what the
/// window opened with, so it goes as it always did.
fn split_seen(waiting: Vec<Waiting>, now: Instant) -> (Vec<Waiting>, Vec<Waiting>) {
    let mut seen = Vec::new();
    let mut just_added = Vec::new();
    for (i, item) in waiting.into_iter().enumerate() {
        if i == 0 || now.saturating_duration_since(item.since) >= SEEN_AFTER {
            seen.push(item);
        } else {
            just_added.push(item);
        }
    }
    (seen, just_added)
}

struct Incoming {
    from: String,
    name: String,
    known: bool,
    offers: Vec<Waiting>,
    /// Set once an answer was sent (or is no longer needed).
    answered: bool,
    /// An offer came since the last look, which the user has to be told of.
    arrived: bool,
    /// The height of the client area to take when the window is restored: it
    /// cannot change while the window is minimized.
    pending_height: Cell<Option<i32>>,
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
        offers: vec![Waiting {
            id,
            offer,
            since: Instant::now(),
        }],
        answered: false,
        arrived: false,
        pending_height: Cell::new(None),
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
        s.offers.retain(|item| item.id != id);
        if s.offers.is_empty() {
            s.answered = true;
            w.close();
        } else {
            s.offers_changed(w);
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

/// How an item is described, with its size, under the name it will be saved
/// as: a peer's name may hold line ends, which would fake lines of the list.
fn describe(offer: &Offer) -> String {
    let name = archive::sanitize_file_name(&offer.name);
    match offer.kind {
        Kind::File => tr!(
            file_description,
            name = name,
            size = format_size(offer.size)
        ),
        Kind::Folder => trn!(
            folder_description,
            offer.file_count,
            name = name,
            size = format_size(offer.content_size)
        ),
    }
}

impl Incoming {
    fn update_space(&self) -> Space {
        let offers: Vec<&Offer> = self.offers.iter().map(|item| &item.offer).collect();
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
        self.offers.push(Waiting {
            id,
            offer,
            since: Instant::now(),
        });
        self.arrived = true;
        self.offers_changed(w);
    }

    /// Shows what waits now. The look at the disk, which can take a while on
    /// a network path, and the announcement wait until the changes stop.
    fn offers_changed(&self, w: &Window) {
        self.show_offers(w);
        unsafe { SetTimer(w.hwnd, TIMER_ANNOUNCE, ANNOUNCE_AFTER_MS, None) };
    }

    /// The changes have stopped.
    fn offers_settled(&mut self, w: &Window) {
        self.update_space();
        accessibility::announce(self.headline);
        if std::mem::take(&mut self.arrived) && unsafe { GetForegroundWindow() } != w.hwnd {
            unsafe { FlashWindow(w.hwnd, 1) };
            tray::balloon(
                app::main_window(),
                tr!(incoming_balloon_title),
                &tr!(incoming_balloon, name = self.name),
                None,
            );
        }
    }

    /// Says what waits for an answer, and arranges the window for it.
    fn show_offers(&self, w: &Window) {
        let (headline, description) = match self.offers.as_slice() {
            [item] => (
                match item.offer.kind {
                    Kind::File => tr!(wants_file, name = self.name),
                    Kind::Folder => tr!(wants_folder, name = self.name),
                },
                describe(&item.offer),
            ),
            offers => {
                let total = offers
                    .iter()
                    .map(|item| match item.offer.kind {
                        Kind::File => item.offer.size,
                        Kind::Folder => item.offer.content_size,
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
        let lines: Vec<String> = self.offers.iter().map(|i| describe(&i.offer)).collect();
        set_text(self.items, &crlf(&lines.join("\n")));
        self.layout(w);
    }

    /// Places everything below the description: the list only takes room
    /// when there are several items.
    fn layout(&self, w: &Window) {
        let several = self.offers.len() > 1;
        // The focus must not stay on a control that goes away.
        if !several && unsafe { GetFocus() } == self.items {
            window::focus(self.accept_button);
        }
        window::show(self.items_caption, several);
        window::show(self.items, several);
        if !self.warning.is_null() {
            window::show(self.warning, !self.known);
            window::show(self.add_contact, !self.known);
        }
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
        let height = y + 170;
        if window::is_minimized(w.hwnd) {
            self.pending_height.set(Some(height));
        } else {
            w.set_client_size(WIDTH, height);
        }
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
                .is_some_and(|item| !item.offer.sender_name.trim().is_empty())
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
            // Should something stay in this window, this is settled now.
            self.known = true;
            self.name = sender.clone();
        }
        let (seen, just_added) = split_seen(std::mem::take(&mut self.offers), Instant::now());
        let stays = !just_added.is_empty();
        for (i, item) in seen.iter().enumerate() {
            // Only the first window takes the focus, which then stays there
            // while the others open, and none if this window has more to ask.
            app::accept_offer(
                item.id,
                &self.from,
                &sender,
                &item.offer,
                dir.clone(),
                i == 0 && !stays,
            );
            app::offer_window_closed(item.id);
        }
        if stays {
            self.offers = just_added;
            self.offers_changed(w);
        } else {
            self.answered = true;
            w.close();
        }
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
        self.update_space();
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
            self.offers_settled(w);
            return Some(0);
        }
        None
    }

    fn on_size(&mut self, w: &Window, _cx: i32, _cy: i32) {
        if !window::is_minimized(w.hwnd)
            && let Some(height) = self.pending_height.take()
        {
            w.set_client_size(WIDTH, height);
        }
    }

    fn on_destroy(&mut self, _w: &Window) {
        for item in &self.offers {
            if !self.answered {
                app::net_send(Command::Respond {
                    id: item.id,
                    accept: None,
                });
            }
            app::offer_window_closed(item.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(name: &str) -> Offer {
        Offer {
            kind: Kind::File,
            name: name.into(),
            size: 1,
            file_count: 1,
            content_size: 1,
            largest_file: 1,
            sender_name: "Ana".into(),
            token: [0; 16],
            modified: None,
            created: None,
        }
    }

    fn waiting(id: u64, age: Duration, now: Instant) -> Waiting {
        Waiting {
            id,
            offer: offer("x"),
            since: now - age,
        }
    }

    /// An answer covers only what can have been seen when it was given.
    #[test]
    fn an_answer_leaves_what_just_arrived() {
        let now = Instant::now();
        let long_ago = Duration::from_secs(5);
        let (seen, stays) = split_seen(
            vec![
                waiting(1, long_ago, now),
                waiting(2, SEEN_AFTER, now),
                waiting(3, Duration::from_millis(100), now),
            ],
            now,
        );
        let ids = |list: &[Waiting]| list.iter().map(|i| i.id).collect::<Vec<_>>();
        assert_eq!(ids(&seen), [1, 2]);
        assert_eq!(ids(&stays), [3]);
    }

    /// What the window opened with goes at once, as a single offer always did.
    #[test]
    fn the_first_item_counts_as_seen() {
        let now = Instant::now();
        let (seen, stays) = split_seen(vec![waiting(1, Duration::ZERO, now)], now);
        assert_eq!(seen.len(), 1);
        assert!(stays.is_empty());
    }

    /// A peer's name cannot fake lines of the list.
    #[test]
    fn a_name_cannot_fake_lines() {
        let line = describe(&offer("photo.jpg\r\n\n\n.exe"));
        assert!(!line.contains(['\r', '\n']), "{line:?}");
    }
}
