//! The window that asks who should receive what File Explorer handed over:
//! "Send with Dodavo" in its context menu, or Dodavo in its Send to menu.
//!
//! The context menu starts one process per selected item, and Send to one
//! process with all of them. A process that finds Dodavo running passes its
//! paths on through `WM_COPYDATA` (see `forward_to_running_instance` in
//! `main.rs`) and ends; the paths then collect in one window here.
//!
//! The paths and the controls live outside the handler, because more paths
//! can arrive while the handler is busy with an Add Recipient window. Nothing
//! is sent until the user chooses Send, so another program that sends this
//! message can only open this window.

use std::{
    cell::{Cell, RefCell},
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::{Controls::*, Input::KeyboardAndMouse::IsWindowEnabled, WindowsAndMessaging::*},
};

use super::{
    accessibility, crlf, info, main_window, send, set_text,
    tray::{self, BalloonAction},
    wide,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    config::Contact,
    i18n::{tr, trn},
    net::Kind,
};

/// Marks the `WM_COPYDATA` messages that carry paths: "DODV".
pub const COPY_DATA_ID: usize = 0x444F_4456;
/// Posted to the main window when paths have arrived.
pub const WM_APP_SEND_TO: u32 = WM_APP + 3;
/// More than a command line can hold, which is where the paths come from.
const MAX_PAYLOAD: usize = 1 << 20;

const ID_ITEMS: u16 = 10;
const ID_LIST: u16 = 11;
const ID_ADD: u16 = 12;
const ID_SEND: u16 = 13;

const CONTENT_X: i32 = 14;
const CONTENT_W: i32 = 452;
const LIST_Y: i32 = 180;
const LIST_H: i32 = 110;
const BUTTONS_Y: i32 = 302;

/// After the last path of a selection has arrived, the count is read out.
const ANNOUNCE_AFTER_MS: u32 = 300;

#[derive(Clone, Copy)]
struct Controls {
    window: HWND,
    count: HWND,
    items: HWND,
    list: HWND,
    /// Takes the place of the list while there are no recipients.
    note: HWND,
    send: HWND,
    add: HWND,
}

thread_local! {
    /// Paths that arrived and that the window does not show yet.
    static PENDING: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    /// What the open window offers to send.
    static PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    /// The recipients in the order of the list.
    static SHOWN: RefCell<Vec<Contact>> = const { RefCell::new(Vec::new()) };
    /// The open window; there is one at most.
    static CONTROLS: Cell<Option<Controls>> = const { Cell::new(None) };
    static ANNOUNCE_TIMER: Cell<usize> = const { Cell::new(0) };
}

/// The paths as one `WM_COPYDATA` payload: UTF-16, each ending in a NUL.
pub fn encode(paths: &[PathBuf]) -> Vec<u8> {
    paths
        .iter()
        .flat_map(|path| path.as_os_str().encode_wide().chain(std::iter::once(0)))
        .flat_map(u16::to_ne_bytes)
        .collect()
}

/// The absolute paths in a payload; `None` if there are none, or if this is
/// not such a payload.
pub fn decode(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    if bytes.len() > MAX_PAYLOAD || !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&unit| u16::from_ne_bytes(unit))
        .collect();
    let paths: Vec<PathBuf> = units
        .split(|&unit| unit == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(OsString::from_wide(path)))
        .filter(|path| path.is_absolute())
        .collect();
    (!paths.is_empty()).then_some(paths)
}

/// Takes the paths of a `WM_COPYDATA` message and has the main window show
/// them; returns whether it could use the message. This can run in the middle
/// of any call that waits for another thread, so it only notes them.
pub fn receive(main: HWND, payload: &[u8]) -> bool {
    let Some(paths) = decode(payload) else {
        return false;
    };
    PENDING.with(|p| p.borrow_mut().extend(paths));
    unsafe { PostMessageW(main, WM_APP_SEND_TO, 0, 0) };
    true
}

/// Shows the paths that arrived (`WM_APP_SEND_TO`).
pub fn show_pending() {
    let paths = PENDING.take();
    if !paths.is_empty() {
        add(paths);
    }
}

/// Offers `paths` for sending: in the open window, or in a new one.
pub fn add(paths: Vec<PathBuf>) {
    let added = PATHS.with(|known| {
        let mut known = known.borrow_mut();
        let before = known.len();
        for path in paths {
            if !known.contains(&path) {
                known.push(path);
            }
        }
        known.len() != before
    });
    match CONTROLS.get() {
        Some(c) => {
            if added {
                show_paths(&c);
                announce_later();
            }
            // Behind an Add Recipient window it stays where it is.
            if unsafe { IsWindowEnabled(c.window) } != 0 {
                window::bring_to_front(c.window);
            }
        }
        None => open(),
    }
}

/// The open window, for a notification that leads to it.
pub fn open_window() -> Option<HWND> {
    CONTROLS.get().map(|c| c.window)
}

fn open() {
    let hwnd = window::create(
        CreateOptions {
            title: tr!(send_to_title),
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            client: (CONTENT_W + 2 * CONTENT_X, BUTTONS_Y + 30 + 14),
            center_on: app::main_window(),
            ..Default::default()
        },
        Box::new(SendTo),
    );
    unsafe { ShowWindow(hwnd, SW_SHOW) };
    if unsafe { SetForegroundWindow(hwnd) } == 0 {
        // Windows withholds the foreground when the user has gone on to
        // something else, so they have to be told that it waits for them.
        unsafe { FlashWindow(hwnd, 1) };
        tray::balloon(
            app::main_window(),
            tr!(send_to_title),
            tr!(send_to_waiting),
            Some(BalloonAction::SendTo),
        );
    }
}

fn show_paths(c: &Controls) {
    let (count, text) = PATHS.with(|paths| {
        let paths = paths.borrow();
        let lines: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        (paths.len(), lines.join("\n"))
    });
    set_text(c.count, &trn!(send_to_count, count));
    set_text(c.items, &crlf(&text));
}

/// A selection in the context menu arrives path by path, so the new count is
/// read out once they have stopped coming.
fn announce_later() {
    stop_announcement();
    let timer = unsafe {
        SetTimer(
            std::ptr::null_mut(),
            0,
            ANNOUNCE_AFTER_MS,
            Some(announce_timer),
        )
    };
    ANNOUNCE_TIMER.set(timer);
}

fn stop_announcement() {
    let timer = ANNOUNCE_TIMER.replace(0);
    if timer != 0 {
        unsafe { KillTimer(std::ptr::null_mut(), timer) };
    }
}

unsafe extern "system" fn announce_timer(_: HWND, _: u32, _: usize, _: u32) {
    stop_announcement();
    if let Some(c) = CONTROLS.get() {
        accessibility::announce(c.count);
    }
}

/// Shows the current recipients. The one selected stays selected; if it is
/// gone, none is, so that nothing goes to someone else by mistake.
pub fn refresh_recipients() {
    let Some(c) = CONTROLS.get() else {
        return;
    };
    let selected = main_window::list_selection(c.list);
    let keep = selected.and_then(|i| SHOWN.with(|s| s.borrow().get(i).map(|c| c.id.clone())));
    let contacts = app::cfg().contacts;
    send(c.list, LVM_DELETEALLITEMS, 0, 0);
    for (i, contact) in contacts.iter().enumerate() {
        let mut name = wide(&contact.name);
        let mut item: LVITEMW = unsafe { std::mem::zeroed() };
        item.mask = LVIF_TEXT;
        item.iItem = i as i32;
        item.pszText = name.as_mut_ptr();
        send(c.list, LVM_INSERTITEMW, 0, &item as *const _ as isize);
    }
    let index = match &keep {
        Some(id) => contacts
            .iter()
            .position(|contact| contact.id.eq_ignore_ascii_case(id)),
        None => (!contacts.is_empty()).then_some(0),
    };
    if let Some(index) = index {
        main_window::list_select(c.list, index);
    }
    let any = !contacts.is_empty();
    SHOWN.set(contacts);
    window::show(c.list, any);
    window::show(c.note, !any);
    window::enable(c.send, any);
    window::with_window(c.window, |w| {
        w.default_id.set(if any { ID_SEND } else { ID_ADD });
    });
    if !any {
        window::focus(c.add);
    }
}

struct SendTo;

impl SendTo {
    fn send(&self, w: &Window) {
        let Some(c) = CONTROLS.get() else {
            return;
        };
        let contact = main_window::list_selection(c.list)
            .and_then(|i| SHOWN.with(|s| s.borrow().get(i).cloned()));
        let Some(contact) = contact else {
            info(w.hwnd, tr!(select_recipient_first));
            return;
        };
        let paths = PATHS.take();
        for (i, path) in paths.into_iter().enumerate() {
            let kind = if path.is_dir() {
                Kind::Folder
            } else {
                Kind::File
            };
            // Only the first window takes the focus, which then stays there
            // while the others open.
            app::start_send(&contact, path, kind, i == 0);
        }
        w.close();
    }

    fn add_recipient(&self, w: &Window) {
        let Some(contact) = super::recipient::show(w.hwnd, None) else {
            return;
        };
        let id = contact.id.clone();
        app::update_cfg(|c| {
            c.contacts.push(contact);
            c.contacts.sort_by_key(|c| c.name.to_lowercase());
        });
        // Also shows the new recipient here (see `refresh_recipients`).
        main_window::refresh_list();
        let Some(c) = CONTROLS.get() else {
            return;
        };
        let index = SHOWN.with(|s| {
            s.borrow()
                .iter()
                .position(|contact| contact.id.eq_ignore_ascii_case(&id))
        });
        if let Some(index) = index {
            main_window::list_select(c.list, index);
        }
        window::focus(c.list);
    }
}

impl Handler for SendTo {
    fn on_create(&mut self, w: &Window) {
        w.set_dialog_role();
        // Its text is not the name of the next control, so that screen
        // readers read it when the window comes up.
        let count = w.label("", (CONTENT_X, 12, CONTENT_W, 20));
        w.make_bold(count);
        w.live_region(count);
        w.caption(tr!(send_to_items_label), (CONTENT_X, 38, CONTENT_W, 18));
        let items = w.read_only_text("", ID_ITEMS, (CONTENT_X, 58, CONTENT_W, 92));
        w.caption(
            tr!(send_to_recipient_label),
            (CONTENT_X, LIST_Y - 20, CONTENT_W, 18),
        );
        let list = w.add_ex(
            "SysListView32",
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER,
            WS_EX_CLIENTEDGE,
            ID_LIST,
            (CONTENT_X, LIST_Y, CONTENT_W, LIST_H),
        );
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize,
        );
        let mut title = wide("");
        let mut column: LVCOLUMNW = unsafe { std::mem::zeroed() };
        column.mask = LVCF_TEXT | LVCF_WIDTH;
        column.cx = w.px(CONTENT_W) - unsafe { GetSystemMetrics(SM_CXVSCROLL) } - 4;
        column.pszText = title.as_mut_ptr();
        send(list, LVM_INSERTCOLUMNW, 0, &column as *const _ as isize);
        let note = w.label(
            tr!(no_recipients_yet),
            (CONTENT_X, LIST_Y, CONTENT_W, LIST_H),
        );

        let add_w = w.button_width(tr!(ctx_add), 120);
        let add = w.button(tr!(ctx_add), ID_ADD, (CONTENT_X, BUTTONS_Y, add_w, 30));
        let right = CONTENT_X + CONTENT_W;
        let send_button = w.button(
            tr!(send_to_send),
            ID_SEND,
            (right - 2 * 96 - 6, BUTTONS_Y, 96, 30),
        );
        w.button(
            tr!(cancel),
            IDCANCEL as u16,
            (right - 96, BUTTONS_Y, 96, 30),
        );

        let controls = Controls {
            window: w.hwnd,
            count,
            items,
            list,
            note,
            send: send_button,
            add,
        };
        CONTROLS.set(Some(controls));
        show_paths(&controls);
        refresh_recipients();
        if !app::cfg().contacts.is_empty() {
            window::focus(list);
        }
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        match id {
            ID_SEND => self.send(w),
            ID_ADD => self.add_recipient(w),
            _ => return false,
        }
        true
    }

    fn on_notify(&mut self, w: &Window, hdr: &NMHDR, _lparam: isize) -> Option<isize> {
        if hdr.idFrom == ID_LIST as usize && hdr.code == NM_DBLCLK {
            self.send(w);
            return Some(0);
        }
        None
    }

    fn on_destroy(&mut self, _w: &Window) {
        stop_announcement();
        CONTROLS.set(None);
        PATHS.take();
        SHOWN.take();
        main_window::tray_hint_when_hidden();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_survive_the_message() {
        let paths = vec![
            PathBuf::from(r"C:\Temp\Račun 2026.pdf"),
            PathBuf::from(r"D:\mapa s razmakom"),
            PathBuf::from(r"\\server\share\file.txt"),
        ];
        assert_eq!(decode(&encode(&paths)), Some(paths));
    }

    /// Windows allows names that are not valid Unicode.
    #[test]
    fn names_that_are_not_unicode_survive_the_message() {
        let mut units: Vec<u16> = r"C:\Temp\".encode_utf16().collect();
        units.push(0xD800);
        let path = PathBuf::from(OsString::from_wide(&units));
        assert_eq!(
            decode(&encode(std::slice::from_ref(&path))),
            Some(vec![path])
        );
    }

    #[test]
    fn anything_else_is_refused() {
        // Nothing at all, half a character, only ends of paths.
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0x43]), None);
        assert_eq!(decode(&[0, 0, 0, 0]), None);
        assert_eq!(decode(&vec![0x41; MAX_PAYLOAD + 2]), None);
        // A relative path means nothing to another process.
        assert_eq!(decode(&encode(&[PathBuf::from("file.txt")])), None);
        // Good paths are kept.
        let good = PathBuf::from(r"C:\good.txt");
        assert_eq!(
            decode(&encode(&[PathBuf::from("bad.txt"), good.clone()])),
            Some(vec![good])
        );
    }
}
