//! The window asking whether to accept an incoming transfer.

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
    net::{Command, Kind, Offer},
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

/// Whether an offer fits on the disk when saved into a folder.
pub struct Space {
    /// `None` if the free space could not be determined.
    pub enough: Option<bool>,
    /// A sentence describing the result.
    pub text: String,
}

/// Bytes needed on the destination drive and on the drive of the temporary
/// folder. A folder arrives as a zip in the temporary folder and is then
/// unpacked into the destination, so both need room at the same time.
fn space_needed(offer: &Offer, same_volume: bool) -> (u64, u64) {
    match offer.kind {
        Kind::File => (offer.size, 0),
        Kind::Folder if same_volume => (offer.size.saturating_add(offer.content_size), 0),
        Kind::Folder => (offer.content_size, offer.size),
    }
}

/// `C:\` is shown as `C:`; other volume paths (e.g. network shares) as they are.
fn drive_name(root: &Path) -> String {
    let s = root.display().to_string();
    match s.strip_suffix('\\') {
        Some(d) if d.len() == 2 && d.ends_with(':') => d.to_string(),
        _ => s,
    }
}

pub fn check_space(offer: &Offer, dir: &Path) -> Space {
    let unknown = || Space {
        enough: None,
        text: tr!(space_unknown).to_string(),
    };
    let Some((dest_root, dest_free)) = shell::disk_free(dir) else {
        return unknown();
    };
    let mut volumes = vec![(dest_root.clone(), dest_free)];
    if offer.kind == Kind::Folder {
        let Some(temp) = shell::disk_free(&std::env::temp_dir()) else {
            return unknown();
        };
        volumes.push(temp);
    }
    let same_volume = volumes
        .get(1)
        .is_none_or(|(root, _)| root.as_os_str().eq_ignore_ascii_case(dest_root.as_os_str()));
    let (dest_need, temp_need) = space_needed(offer, same_volume);
    let mut checks = vec![(&volumes[0], dest_need)];
    if !same_volume {
        checks.push((&volumes[1], temp_need));
    }
    let short = checks.iter().find(|((_, free), need)| need > free);
    let ((root, free), needed) = short.copied().unwrap_or(checks[0]);
    let args = (
        format_size(needed),
        format_size(*free),
        drive_name(root),
    );
    let mut text = if short.is_some() {
        tr!(space_short, needed = args.0, free = args.1, drive = args.2)
    } else {
        tr!(space_ok, needed = args.0, free = args.1, drive = args.2)
    };
    if offer.kind == Kind::Folder {
        text = format!("{text} {}", tr!(space_folder_note));
    }
    Space {
        enough: Some(short.is_none()),
        text,
    }
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
        let space = check_space(&self.offer, Path::new(get_text(self.dir).trim()));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(kind: Kind) -> Offer {
        Offer {
            kind,
            name: "x".into(),
            size: 100,
            file_count: 3,
            content_size: 150,
            sender_name: String::new(),
            token: [0; 16],
        }
    }

    #[test]
    fn needed_space() {
        assert_eq!(space_needed(&offer(Kind::File), true), (100, 0));
        assert_eq!(space_needed(&offer(Kind::File), false), (100, 0));
        // The zip and the unpacked folder exist at the same time.
        assert_eq!(space_needed(&offer(Kind::Folder), true), (250, 0));
        assert_eq!(space_needed(&offer(Kind::Folder), false), (150, 100));
    }

    #[test]
    fn drive_names() {
        assert_eq!(drive_name(Path::new(r"C:\")), "C:");
        assert_eq!(
            drive_name(Path::new(r"\\server\share\")),
            r"\\server\share\"
        );
    }
}
