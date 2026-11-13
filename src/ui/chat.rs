//! The chat window: the messages exchanged with one person, and a field to
//! write the next one. One window per person, which stays out of the way
//! when a message arrives (it does not take the focus). The messages are
//! kept on the disk (`crate::chat`), and travel in `net::chat`.
//!
//! For screen readers: the history is a list, one message per item, which
//! the arrow keys read; a message that arrives while the window is in front
//! is put into the status line, a live region, and announced there, and one
//! that arrives while it is not is told by a notification, which screen
//! readers read. A message of the user's that has arrived at the other
//! side is put there too, while the window is in front, with the user's
//! name as the other person's messages have theirs, so that both are read
//! the same way. The status line also says that the other person is typing
//! and why a message could not be sent.
//!
//! Several messages can be selected in the history (Shift and Ctrl with the
//! arrow keys or the mouse, Ctrl+A for all), and copied (Ctrl+C) or deleted
//! from this computer (Delete), also from its context menu.

use std::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use windows_sys::Win32::{
    Foundation::{FILETIME, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM},
    Globalization::{GetDateFormatW, GetTimeFormatW},
    Graphics::Gdi::{
        COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, CreateSolidBrush, DT_EXPANDTABS, DT_NOPREFIX,
        DT_WORDBREAK, DeleteObject, DrawFocusRect, DrawTextW, FillRect, GetSysColor,
        MapWindowPoints, ScreenToClient, SetBkMode, SetTextColor, TRANSPARENT,
    },
    System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
    UI::{
        Controls::{DRAWITEMSTRUCT, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_LISTBOX},
        Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_DELETE, VK_SHIFT},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            CreatePopupMenu, FlashWindow, GetClientRect, GetForegroundWindow, GetParent, IDCANCEL,
            KillTimer, LB_ADDSTRING, LB_DELETESTRING, LB_ERR, LB_GETCARETINDEX, LB_GETCOUNT,
            LB_GETITEMRECT, LB_GETSEL, LB_GETSELCOUNT, LB_GETSELITEMS, LB_GETTEXT, LB_GETTEXTLEN,
            LB_GETTOPINDEX, LB_INSERTSTRING, LB_ITEMFROMPOINT, LB_RESETCONTENT, LB_SETANCHORINDEX,
            LB_SETCARETINDEX, LB_SETITEMHEIGHT, LB_SETSEL, LB_SETTOPINDEX, LBS_EXTENDEDSEL,
            LBN_SELCHANGE, LBS_HASSTRINGS, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY,
            LBS_OWNERDRAWVARIABLE, PostMessageW,
            SW_SHOWNOACTIVATE, SetTimer, ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_CHAR, WM_CLOSE,
            WM_COMMAND, WM_CONTEXTMENU, WM_DRAWITEM, WM_KEYDOWN, WM_NCDESTROY, WM_TIMER,
            WS_CAPTION, WS_CLIPCHILDREN, WS_EX_APPWINDOW, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT,
            WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VSCROLL,
        },
    },
};

use super::{
    accessibility, confirm, confirm_no, crlf, get_text, menu, send, set_text, shell, short_id,
    sound,
    theme,
    tray::{self, BalloonAction},
    wide,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    chat::{Entry, History, MAX_CHARS, Sending},
    config::Contact,
    i18n::{tr, trn},
    identity,
    net::Command,
    sound::SoundEvent,
};

const ID_HISTORY: u16 = 10;
const ID_INPUT: u16 = 11;
const ID_SEND: u16 = 12;
const ID_ADD: u16 = 13;
const ID_BLOCK: u16 = 14;
const ID_DELETE: u16 = 15;
const ID_SELECTED: u16 = 16;
/// The commands of the history (its keys and its context menu).
const ID_COPY: u16 = 17;
const ID_SELECT_ALL: u16 = 18;
const ID_DELETE_SELECTED: u16 = 19;

const EN_CHANGE: u16 = 0x0300;
const EM_LIMITTEXT: u32 = 0x00C5;
const SS_NOPREFIX: u32 = 0x80;
const SS_ENDELLIPSIS: u32 = 0x4000;

/// The window (96-DPI units).
const WIDTH: i32 = 500;
const MARGIN: i32 = 14;
const CONTENT_W: i32 = WIDTH - 2 * MARGIN;
const HISTORY_H: i32 = 230;
/// Room around the text of a message in the history.
const ITEM_PAD: i32 = 4;
/// The height of the box with the whole text of the selected message.
const SELECTED_H: i32 = 64;
/// Windows allows no higher item in a list: a message that wraps over more
/// lines than fit shows its beginning there, and all of it in the box below
/// the history once it is selected.
const MAX_ITEM_PX: i32 = 255;

/// The messages that arrived are announced once no more has come for this
/// long, all at once: a text that is replaced right away is not read.
const TIMER_SPEAK: usize = 1;
const SPEAK_AFTER_MS: u32 = 300;
/// "… is typing" goes when the other side has not said so again for a while.
const TIMER_PEER_TYPING: usize = 2;
const PEER_TYPING_MS: u32 = 8000;
/// The other side is told that the user stopped typing once no key came for
/// a while.
const TIMER_MY_TYPING: usize = 3;
const MY_TYPING_MS: u32 = 5000;
/// While the user types, the other side is told so again this often.
const TYPING_AGAIN: Duration = Duration::from_secs(3);
/// How much of a message a notification shows, in characters.
const BALLOON_CHARS: usize = 200;

static NEXT_MESSAGE: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The messages on their way, by their numbers, noted on the disk too,
    /// so that one that the program ends on is not kept as sent (see
    /// `Sending`). Not in the windows: a window may be closed before its
    /// message has arrived or failed. `None` until [`start`].
    static UNCONFIRMED: RefCell<Option<Sending>> = const { RefCell::new(None) };
}

/// At the start of the program, before any chat is shown: the messages
/// that the last run left on their way are marked as not sent in their
/// chats, since nobody will say any more whether they arrived.
pub fn start(data_dir: &std::path::Path) {
    UNCONFIRMED.with(|u| *u.borrow_mut() = Some(Sending::start(data_dir)));
}

/// The message `id` is on its way no more: what it was, if it was.
fn confirmed(id: u64) -> Option<crate::chat::OnItsWay> {
    UNCONFIRMED.with(|u| u.borrow_mut().as_mut().and_then(|sending| sending.remove(id)))
}

struct Chat {
    /// The other person's Contact ID, as `key_of` writes it.
    peer: String,
    name: String,
    /// Whether the other person is a recipient.
    known: bool,
    history: History,
    list: HWND,
    /// The whole text of the message that is selected in the history, to
    /// read at leisure: a list item cannot show all of a very long one.
    selected: HWND,
    status: HWND,
    input: HWND,
    warning: HWND,
    add: HWND,
    block: HWND,
    delete: HWND,
    close: HWND,
    /// What arrived, and the user's messages that arrived at the other
    /// side, in that order, which have not been announced yet.
    unspoken: Vec<String>,
    /// Whether the status line says that the other person is typing.
    shows_typing: bool,
    /// When the other side was last told that the user is typing, while it
    /// believes so.
    told_typing: Option<Instant>,
}

/// How a Contact ID names its chat: without case.
fn key_of(peer: &str) -> String {
    peer.to_ascii_lowercase()
}

/// The open chat window with `peer`.
fn window_of(peer: &str) -> Option<HWND> {
    let key = key_of(peer);
    app::with(|a| a.chats.get(&key).copied())
}

/// Opens the chat with the person whose Contact ID is `peer`, or brings it
/// to the front. `name` is what they call themselves, for someone who is not
/// a recipient. With `activate` the window takes the focus, which goes to
/// the message field; otherwise it only shows. Not to be called inside
/// `app::with`.
pub fn open(peer: &str, name: &str, activate: bool) -> HWND {
    let hwnd = match window_of(peer) {
        Some(hwnd) => hwnd,
        None => create(peer, name),
    };
    if activate {
        window::bring_to_front(hwnd);
    } else {
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    }
    hwnd
}

fn create(peer: &str, name: &str) -> HWND {
    let peer = key_of(peer);
    let (name, known) = app::peer_name(&peer, name);
    let data_dir = app::with(|a| a.data_dir.clone());
    let null = std::ptr::null_mut();
    let handler = Chat {
        history: History::load(&data_dir, &peer),
        peer: peer.clone(),
        name: name.clone(),
        known,
        list: null,
        selected: null,
        status: null,
        input: null,
        warning: null,
        add: null,
        block: null,
        delete: null,
        close: null,
        unspoken: Vec::new(),
        shows_typing: false,
        told_typing: None,
    };
    let hwnd = window::create(
        CreateOptions {
            title: &tr!(chat_title, name = name),
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            // Sized to the content in `on_create`.
            client: (WIDTH, 420),
            center_on: app::main_window(),
            urgent: Some(draw_history),
            ..Default::default()
        },
        Box::new(handler),
    );
    app::with(|a| a.chats.insert(peer, hwnd));
    hwnd
}

/// A chat message arrived. Returns `false` if its window is busy (it shows
/// a question), and the message has to be given again later.
pub fn received(from: &str, name: &str, text: &str) -> bool {
    let hwnd = match window_of(from) {
        Some(hwnd) => hwnd,
        None => open(from, name, false),
    };
    let front = unsafe { GetForegroundWindow() } == hwnd;
    let shown = window::with_handler::<Chat, _>(hwnd, |c, w| {
        c.received(w, text, front);
        c.name.clone()
    });
    let Some(shown_name) = shown else {
        return false;
    };
    sound::play(SoundEvent::MessageReceived);
    if !front {
        // Never in front of what the user is doing, and a window that the
        // user minimized stays so.
        unsafe { FlashWindow(hwnd, 1) };
        let short: String = text.chars().take(BALLOON_CHARS).collect();
        tray::balloon(
            app::main_window(),
            &tr!(chat_balloon_title, name = shown_name),
            &short,
            Some(BalloonAction::Chat {
                peer: key_of(from),
                name: shown_name,
            }),
        );
    }
    true
}

/// The message `id` arrived at the other side. The sound says so, and the
/// chat window in front reads it out, as it does the other person's: the
/// user is there, and nothing else of the window changes. A window that is
/// not in front, or that asks something, says nothing.
pub fn sent(id: u64) {
    let Some(message) = confirmed(id) else {
        return;
    };
    sound::play(SoundEvent::MessageSent);
    if let Some(hwnd) = window_of(&message.peer)
        && unsafe { GetForegroundWindow() } == hwnd
    {
        window::with_handler::<Chat, _>(hwnd, |c, w| c.delivered(w, &message.text));
    }
}

/// The message `id` was not delivered. Returns `false` if its window is
/// busy, and this has to be said again later.
pub fn failed(id: u64, error: &str) -> bool {
    let Some((peer, time, text)) = UNCONFIRMED.with(|u| {
        let u = u.borrow();
        let message = u.as_ref().and_then(|sending| sending.get(id));
        message.map(|m| (m.peer.clone(), m.time, m.text.clone()))
    }) else {
        return true;
    };
    let said = tr!(chat_not_sent, error = error);
    match window_of(&peer) {
        Some(hwnd) => {
            let told =
                window::with_handler::<Chat, _>(hwnd, |c, w| c.failed(w, time, &text, &said));
            if told.is_none() {
                return false;
            }
            if unsafe { GetForegroundWindow() } != hwnd {
                unsafe { FlashWindow(hwnd, 1) };
            }
        }
        // The window was closed meanwhile: the chat on the disk says that
        // the message was not sent, and a notification tells the user.
        None => {
            let data_dir = app::with(|a| a.data_dir.clone());
            let mut history = History::load(&data_dir, &peer);
            if let Some(index) = find(&history, time, &text) {
                let _ = history.set_failed(index, true);
            }
            let (name, _) = app::peer_name(&peer, "");
            tray::balloon(
                app::main_window(),
                &tr!(chat_title, name = name),
                &said,
                Some(BalloonAction::Chat {
                    peer: peer.clone(),
                    name,
                }),
            );
        }
    }
    confirmed(id);
    sound::play(SoundEvent::MessageFailed);
    true
}

/// The other person of a chat is typing, or has stopped. Only an open chat
/// shows it; one that is busy misses it.
pub fn typing(from: &str, on: bool) {
    let Some(hwnd) = window_of(from) else {
        return;
    };
    let front = unsafe { GetForegroundWindow() } == hwnd;
    let began = window::with_handler::<Chat, _>(hwnd, |c, w| c.peer_typing(w, on, front));
    if began == Some(true) {
        sound::play(SoundEvent::Typing);
    }
}

/// Deletes every chat on the disk, and the open windows show none either.
/// A chat window that is busy (it asks something) would go on with the
/// messages it has: then nothing is deleted, and that window is returned,
/// for the user to answer it first.
pub fn delete_all() -> std::io::Result<Option<HWND>> {
    let windows: Vec<HWND> = app::with(|a| a.chats.values().copied().collect());
    let busy = windows
        .iter()
        .find(|&&h| window::with_handler::<Chat, _>(h, |_, _| ()).is_none());
    if let Some(&busy) = busy {
        return Ok(Some(busy));
    }
    let data_dir = app::with(|a| a.data_dir.clone());
    crate::chat::delete_all(&data_dir)?;
    for hwnd in windows {
        window::with_handler::<Chat, _>(hwnd, |c, _| c.clear());
    }
    Ok(None)
}

/// Notes a message of the user's to `peer` as on its way, and gives it its
/// number.
fn on_its_way(peer: &str, entry: &Entry) -> u64 {
    let id = NEXT_MESSAGE.fetch_add(1, Ordering::Relaxed);
    UNCONFIRMED.with(|u| {
        if let Some(sending) = u.borrow_mut().as_mut() {
            sending.add(id, peer, entry);
        }
    });
    id
}

/// Sends `text` to each of `contacts` as a message of its own, which is
/// kept in the chat with that person, where the answer arrives too. Whether
/// a message reached its person is said as for any message: one that did
/// not is marked in that chat, and a notification tells of it. Returns the
/// chat window that is busy (it asks something), if one is: then nothing
/// was sent. Every Contact ID has to be valid.
fn send_to_each(contacts: &[Contact], text: &str) -> Option<HWND> {
    let open: Vec<Option<HWND>> = contacts.iter().map(|c| window_of(&c.id)).collect();
    // First, so that the message goes to all or to nobody.
    let busy = open
        .iter()
        .flatten()
        .find(|&&h| window::with_handler::<Chat, _>(h, |_, _| ()).is_none());
    if let Some(&busy) = busy {
        return Some(busy);
    }
    let data_dir = app::with(|a| a.data_dir.clone());
    for (contact, window) in contacts.iter().zip(open) {
        let Some(to) = identity::parse_contact_id(&contact.id) else {
            continue;
        };
        match window {
            // The chat that is open shows it.
            Some(hwnd) => {
                window::with_handler::<Chat, _>(hwnd, |c, w| c.send_text(w, to, text.to_string()));
            }
            None => {
                let peer = key_of(&contact.id);
                let entry = Entry::now(true, text);
                let id = on_its_way(&peer, &entry);
                // A chat that cannot be saved is no reason not to send.
                let _ = History::load(&data_dir, &peer).push(entry);
                app::net_send(Command::Chat {
                    to,
                    id,
                    text: text.to_string(),
                });
            }
        }
    }
    None
}

const ID_SEVERAL_INPUT: u16 = 10;
const ID_SEVERAL_SEND: u16 = 11;

/// The window in which one message is written for several people.
struct Several {
    contacts: Vec<Contact>,
    input: HWND,
    sent: std::rc::Rc<std::cell::Cell<bool>>,
}

/// Asks at `owner` for a message to `contacts`, of whom there are several,
/// and sends it to each of them (see `send_to_each`). Whether it was sent.
/// Not to be called inside `app::with`.
pub fn message_several(owner: HWND, contacts: &[Contact]) -> bool {
    let invalid = contacts
        .iter()
        .find(|c| identity::parse_contact_id(&c.id).is_none());
    if let Some(contact) = invalid {
        super::error(owner, &tr!(contact_invalid, name = contact.name));
        return false;
    }
    let sent = std::rc::Rc::new(std::cell::Cell::new(false));
    let handler = Several {
        contacts: contacts.to_vec(),
        input: std::ptr::null_mut(),
        sent: sent.clone(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(several_title),
            // Sized to the content in `on_create`.
            client: (WIDTH, 200),
            center_on: owner,
            ..Default::default()
        },
        Box::new(handler),
    );
    window::run_modal(owner, hwnd);
    sent.get()
}

impl Handler for Several {
    fn on_create(&mut self, w: &Window) {
        // Screen readers read who gets the message when the window opens.
        w.set_dialog_role();
        let x = MARGIN;
        let mut y = 12;
        // Every name: this is who the message goes to.
        let names: Vec<&str> = self.contacts.iter().map(|c| c.name.as_str()).collect();
        let to = tr!(several_to, names = names.join(", "));
        let to_h = w.text_height(&to, CONTENT_W).max(18);
        w.label(&to, (x, y, CONTENT_W, to_h));
        y += to_h + 6;
        let note = tr!(several_note);
        let note_h = w.text_height(note, CONTENT_W).max(18);
        w.label(note, (x, y, CONTENT_W, note_h));
        y += note_h + 12;
        w.caption(tr!(chat_message_label), (x, y, CONTENT_W, 18));
        y += 20;
        self.input = w.edit("", ID_SEVERAL_INPUT, (x, y, CONTENT_W, 24));
        send(self.input, EM_LIMITTEXT, MAX_CHARS, 0);
        y += 40;
        let send_w = w.button_width(tr!(chat_send), 90);
        w.button(
            tr!(chat_send),
            ID_SEVERAL_SEND,
            (WIDTH - MARGIN - 96 - send_w, y, send_w, 30),
        );
        w.button(tr!(cancel), IDCANCEL as u16, (WIDTH - MARGIN - 90, y, 90, 30));
        w.set_client_size(WIDTH, y + 30 + MARGIN);
        w.default_id.set(ID_SEVERAL_SEND);
        window::focus(self.input);
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        if id != ID_SEVERAL_SEND {
            return false;
        }
        let text = get_text(self.input).trim().to_string();
        if text.is_empty() {
            window::focus(self.input);
            return true;
        }
        match send_to_each(&self.contacts, &text) {
            // A chat with one of them asks something: that comes first.
            Some(busy) => window::bring_to_front(busy),
            None => {
                self.sent.set(true);
                w.close();
            }
        }
        true
    }
}

/// Closes the chat window with `peer`, if one is open (the person was
/// blocked). One that asks something closes once it has its answer.
pub fn close(peer: &str) {
    if let Some(hwnd) = window_of(peer) {
        unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    }
}

/// The message of `time` with `text` that the user wrote and that is not
/// marked as failed, from the end: of two that are the same, each failure
/// marks one.
fn find(history: &History, time: u64, text: &str) -> Option<usize> {
    history.find_sent(time, text)
}

/// `time` (Unix seconds) as the user's Windows writes it: the time of day
/// for today, and the date with it for another day.
fn time_text(time: u64) -> String {
    fn local(time: u64) -> Option<SYSTEMTIME> {
        // 100 ns since 1601.
        let ticks = time.checked_add(11_644_473_600)?.checked_mul(10_000_000)?;
        let file_time = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        unsafe {
            let mut utc: SYSTEMTIME = std::mem::zeroed();
            let mut local: SYSTEMTIME = std::mem::zeroed();
            (FileTimeToSystemTime(&file_time, &mut utc) != 0
                && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0)
                .then_some(local)
        }
    }
    const LOCALE_USER_DEFAULT: u32 = 0x0400;
    const TIME_NOSECONDS: u32 = 2;
    const DATE_SHORTDATE: u32 = 1;
    let Some(at) = local(time) else {
        return String::new();
    };
    let mut buf = [0u16; 80];
    let text = |n: i32, buf: &[u16]| String::from_utf16_lossy(&buf[..(n - 1).max(0) as usize]);
    let n = unsafe {
        GetTimeFormatW(
            LOCALE_USER_DEFAULT,
            TIME_NOSECONDS,
            &at,
            std::ptr::null(),
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    let time_of_day = text(n, &buf);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let today = local(now).is_some_and(|now| {
        (now.wYear, now.wMonth, now.wDay) == (at.wYear, at.wMonth, at.wDay)
    });
    if today {
        return time_of_day;
    }
    let n = unsafe {
        GetDateFormatW(
            LOCALE_USER_DEFAULT,
            DATE_SHORTDATE,
            &at,
            std::ptr::null(),
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    format!("{} {time_of_day}", text(n, &buf))
}

/// The text of the list item `index`.
fn item_text(list: HWND, index: usize) -> String {
    let len = send(list, LB_GETTEXTLEN, index, 0);
    if len < 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = send(list, LB_GETTEXT, index, buf.as_mut_ptr() as isize);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Draws a message of the history, wrapped at words. It needs nothing of the
/// handler, so the history also shows while the window asks something.
fn draw_history(w: &Window, msg: u32, _wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if msg != WM_DRAWITEM {
        return None;
    }
    // SAFETY: Windows sends a `DRAWITEMSTRUCT` with `WM_DRAWITEM`.
    let item = unsafe { &*(lp as *const DRAWITEMSTRUCT) };
    if item.CtlType != ODT_LISTBOX || item.CtlID != ID_HISTORY as u32 {
        return None;
    }
    // An empty list asks for its focus rectangle alone.
    if item.itemID == u32::MAX {
        return Some(1);
    }
    let text: Vec<u16> = item_text(item.hwndItem, item.itemID as usize)
        .encode_utf16()
        .collect();
    let (color, background) = if item.itemState & ODS_SELECTED != 0 {
        unsafe { (GetSysColor(COLOR_HIGHLIGHTTEXT), GetSysColor(COLOR_HIGHLIGHT)) }
    } else {
        theme::input_colors()
    };
    let pad = w.px(ITEM_PAD);
    let mut rect = item.rcItem;
    rect.left += pad;
    rect.right -= pad;
    rect.top += pad / 2;
    unsafe {
        let brush = CreateSolidBrush(background);
        FillRect(item.hDC, &item.rcItem, brush);
        DeleteObject(brush);
        SetTextColor(item.hDC, color);
        SetBkMode(item.hDC, TRANSPARENT as i32);
        DrawTextW(
            item.hDC,
            text.as_ptr(),
            text.len() as i32,
            &mut rect,
            DT_WORDBREAK | DT_EXPANDTABS | DT_NOPREFIX,
        );
        if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
            DrawFocusRect(item.hDC, &item.rcItem);
        }
    }
    Some(1)
}

/// The indices of the selected messages, in their order.
fn selected(list: HWND) -> Vec<usize> {
    let count = send(list, LB_GETSELCOUNT, 0, 0);
    if count <= 0 {
        return Vec::new();
    }
    let mut items = vec![0i32; count as usize];
    let n = send(
        list,
        LB_GETSELITEMS,
        items.len(),
        items.as_mut_ptr() as isize,
    );
    items.truncate(n.max(0) as usize);
    items
        .into_iter()
        .filter_map(|i| usize::try_from(i).ok())
        .collect()
}

/// Selects the message at `index` alone, with the focus rectangle on it,
/// and Shift with an arrow key goes on from it.
fn select_only(list: HWND, index: usize) {
    send(list, LB_SETSEL, 0, -1);
    send(list, LB_SETSEL, 1, index as isize);
    send(list, LB_SETANCHORINDEX, index, 0);
    send(list, LB_SETCARETINDEX, index, 0);
}

/// The history list's own keys: Ctrl+C copies the selected messages, Ctrl+A
/// selects all, and Delete deletes the selected ones. Each becomes a command
/// of the window, which is carried out after this message, outside of the
/// list's own handling, as the question before a deletion needs.
unsafe extern "system" fn history_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(history_proc), 0);
        }
        let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
        let shift = GetKeyState(VK_SHIFT as i32) < 0;
        let command = match msg {
            WM_KEYDOWN if ctrl && wp == 'C' as usize => Some(ID_COPY),
            WM_KEYDOWN if ctrl && wp == 'A' as usize => Some(ID_SELECT_ALL),
            WM_KEYDOWN if !ctrl && !shift && wp == VK_DELETE as usize => Some(ID_DELETE_SELECTED),
            // The characters of Ctrl+A and Ctrl+C, which the list would look
            // for in its items.
            WM_CHAR if ctrl && (wp == 1 || wp == 3) => return 0,
            _ => None,
        };
        if let Some(id) = command {
            PostMessageW(GetParent(hwnd), WM_COMMAND, id as usize, 0);
            return 0;
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

impl Chat {
    /// A message as the history shows it and a screen reader reads it.
    fn line(&self, entry: &Entry) -> String {
        let time = time_text(entry.time);
        match (entry.mine, entry.failed) {
            (true, true) => tr!(chat_entry_failed, time = time, text = entry.text),
            (true, false) => tr!(chat_entry_mine, time = time, text = entry.text),
            (false, _) => tr!(chat_entry, name = self.name, time = time, text = entry.text),
        }
    }

    /// Gives the list item `index` the height that its wrapped text needs.
    fn size_item(&self, w: &Window, index: usize, line: &str) {
        // Less the frame, the scroll bar and the room around the text.
        let width = CONTENT_W - 4 - 18 - 2 * ITEM_PAD;
        let height = (w.px(w.text_height(line, width) + ITEM_PAD)).clamp(1, MAX_ITEM_PX);
        send(self.list, LB_SETITEMHEIGHT, index, height as isize);
    }

    /// Shows every message of the chat, the last one selected and in view.
    fn fill(&self, w: &Window) {
        send(self.list, LB_RESETCONTENT, 0, 0);
        for (index, entry) in self.history.entries().iter().enumerate() {
            let line = self.line(entry);
            send(self.list, LB_ADDSTRING, 0, wide(&line).as_ptr() as isize);
            self.size_item(w, index, &line);
        }
        self.to_last();
        window::enable(self.delete, !self.history.entries().is_empty());
    }

    /// Selects the last message alone and brings it into view.
    fn to_last(&self) {
        let count = send(self.list, LB_GETCOUNT, 0, 0);
        if count > 0 {
            select_only(self.list, count as usize - 1);
            send(self.list, LB_SETTOPINDEX, count as usize - 1, 0);
        }
        self.show_selected();
    }

    /// Shows the whole text of the message that the user is at in the
    /// history (the one with the focus rectangle, also among several that
    /// are selected) in the box below it; nothing in an empty history.
    fn show_selected(&self) {
        let index = send(self.list, LB_GETCARETINDEX, 0, 0);
        let text = usize::try_from(index)
            .ok()
            .and_then(|index| self.history.entries().get(index))
            .map_or("", |entry| entry.text.as_str());
        // Only a change: setting the text puts a reader back at its start.
        let text = crlf(text);
        if get_text(self.selected) != text {
            set_text(self.selected, &text);
        }
    }

    /// Whether the user is at the history or at the text of its selected
    /// message, and stays there when something arrives.
    fn reading(&self) -> bool {
        let focused = window::focused();
        focused == self.list || focused == self.selected
    }

    /// Adds a message to the chat and to the list. A user who is reading
    /// the history stays where they are; otherwise the new message is the
    /// selected one, so that the history is entered at its end.
    fn add(&mut self, w: &Window, entry: Entry) {
        let line = self.line(&entry);
        let (gone, saved) = self.history.push(entry);
        for _ in 0..gone {
            send(self.list, LB_DELETESTRING, 0, 0);
        }
        let index = send(self.list, LB_ADDSTRING, 0, wide(&line).as_ptr() as isize);
        if index >= 0 {
            self.size_item(w, index as usize, &line);
        }
        if !self.reading() {
            self.to_last();
        } else if gone > 0 {
            // Another message has the selected place now.
            self.show_selected();
        }
        window::enable(self.delete, true);
        if let Err(e) = saved {
            self.say(&tr!(chat_not_saved, error = e), true);
        }
    }

    /// Puts `text` into the status line, and has screen readers read it
    /// when `announce`.
    fn say(&mut self, text: &str, announce: bool) {
        self.shows_typing = false;
        set_text(self.status, text);
        if announce {
            accessibility::announce(self.status);
        }
    }

    fn received(&mut self, w: &Window, text: &str, front: bool) {
        self.add(w, Entry::now(false, text));
        self.unspoken
            .push(tr!(chat_said, name = self.name, text = text));
        // Whoever wrote is not typing any more.
        unsafe { KillTimer(w.hwnd, TIMER_PEER_TYPING) };
        if front {
            unsafe { SetTimer(w.hwnd, TIMER_SPEAK, SPEAK_AFTER_MS, None) };
        } else {
            // The notification tells of it. The status line has it for when
            // the window comes to the front, where screen readers read the
            // texts of the window.
            self.speak(false);
        }
    }

    /// The user's message `text` arrived at the other side, while the
    /// window is in front: it is announced as the other person's are, with
    /// the user's own name, together with what else arrives by then.
    fn delivered(&mut self, w: &Window, text: &str) {
        let me = app::with(|a| a.cfg.my_name.trim().to_string());
        let me = if me.is_empty() {
            tr!(unknown_user).to_string()
        } else {
            me
        };
        self.unspoken.push(tr!(chat_said, name = me, text = text));
        unsafe { SetTimer(w.hwnd, TIMER_SPEAK, SPEAK_AFTER_MS, None) };
    }

    /// Shows what arrived since the last time in the status line.
    fn speak(&mut self, announce: bool) {
        if self.unspoken.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.unspoken).join("  ");
        self.say(&text, announce);
    }

    fn failed(&mut self, w: &Window, time: u64, text: &str, said: &str) {
        if let Some(index) = find(&self.history, time, text) {
            let _ = self.history.set_failed(index, true);
            let line = self.line(&self.history.entries()[index]);
            // The row is replaced: the user who reads the history stays at
            // the same message, with the same ones selected, and sees the
            // same part of it.
            let was_selected = send(self.list, LB_GETSEL, index, 0) > 0;
            let caret = send(self.list, LB_GETCARETINDEX, 0, 0);
            let top = send(self.list, LB_GETTOPINDEX, 0, 0);
            send(self.list, LB_DELETESTRING, index, 0);
            send(self.list, LB_INSERTSTRING, index, wide(&line).as_ptr() as isize);
            self.size_item(w, index, &line);
            if was_selected {
                send(self.list, LB_SETSEL, 1, index as isize);
            }
            if caret >= 0 {
                send(self.list, LB_SETCARETINDEX, caret as usize, 0);
            }
            if top >= 0 {
                send(self.list, LB_SETTOPINDEX, top as usize, 0);
            }
        }
        // Back into the field, to send it again, unless the user is writing
        // the next one.
        if get_text(self.input).is_empty() {
            set_text(self.input, text);
        }
        self.say(said, true);
    }

    /// The other person is typing or has stopped. Returns whether to tell
    /// the user so with a sound: when it began, in the window in front. The
    /// sound has a setting of its own (Settings, Sounds), so "Announce when
    /// the other person is typing" decides only what is said.
    fn peer_typing(&mut self, w: &Window, on: bool, front: bool) -> bool {
        if !on {
            unsafe { KillTimer(w.hwnd, TIMER_PEER_TYPING) };
            if self.shows_typing {
                self.say("", false);
            }
            return false;
        }
        unsafe { SetTimer(w.hwnd, TIMER_PEER_TYPING, PEER_TYPING_MS, None) };
        // Said once, and not over what is about to be announced.
        if self.shows_typing || !self.unspoken.is_empty() {
            return false;
        }
        let announce = front && app::with(|a| a.cfg.announce_typing);
        self.say(&tr!(chat_typing, name = self.name), announce);
        self.shows_typing = true;
        front
    }

    fn to(&self) -> Option<iroh::EndpointId> {
        identity::parse_contact_id(&self.peer)
    }

    /// Tells the other side that the user is typing (again, every now and
    /// then, while they are) or has stopped, if the settings say to.
    fn tell_typing(&mut self, w: &Window, on: bool) {
        let Some(to) = self.to() else { return };
        if on {
            unsafe { SetTimer(w.hwnd, TIMER_MY_TYPING, MY_TYPING_MS, None) };
            let due = self
                .told_typing
                .is_none_or(|at| at.elapsed() >= TYPING_AGAIN);
            if due && app::with(|a| a.cfg.send_typing) {
                self.told_typing = Some(Instant::now());
                app::net_send(Command::Typing { to, on: true });
            }
        } else {
            unsafe { KillTimer(w.hwnd, TIMER_MY_TYPING) };
            if self.told_typing.take().is_some() {
                app::net_send(Command::Typing { to, on: false });
            }
        }
    }

    fn send_message(&mut self, w: &Window) {
        let text = get_text(self.input).trim().to_string();
        if text.is_empty() {
            window::focus(self.input);
            return;
        }
        let Some(to) = self.to() else {
            super::error(w.hwnd, &tr!(contact_invalid, name = self.name));
            return;
        };
        // The message itself says that the user has stopped typing.
        unsafe { KillTimer(w.hwnd, TIMER_MY_TYPING) };
        self.told_typing = None;
        set_text(self.input, "");
        self.say("", false);
        self.send_text(w, to, text);
        window::focus(self.input);
    }

    /// Adds a message of the user's to the chat and sends it.
    fn send_text(&mut self, w: &Window, to: iroh::EndpointId, text: String) {
        let entry = Entry::now(true, text.clone());
        let id = on_its_way(&self.peer, &entry);
        self.add(w, entry);
        self.to_last();
        app::net_send(Command::Chat { to, id, text });
    }

    fn add_recipient(&mut self, w: &Window) {
        let initial = Contact::new(self.name.clone(), self.peer.clone());
        let Some(contact) = super::recipient::show_new(w.hwnd, initial) else {
            return;
        };
        // With another Contact ID written into the window, it is another
        // person who became a recipient, and this chat stays as it is.
        let this_person = contact.id.eq_ignore_ascii_case(&self.peer);
        if this_person {
            self.name = contact.name.clone();
        }
        app::update_cfg(|c| c.add_contact(contact));
        super::main_window::refresh_list();
        if !this_person {
            return;
        }
        self.known = true;
        set_text(w.hwnd, &tr!(chat_title, name = self.name));
        // The focus was on the button that goes now.
        window::focus(self.input);
        window::show(self.add, false);
        window::show(self.warning, false);
        // The messages carry the name.
        self.fill(w);
    }

    /// Blocks the other person, if the user confirms it, and closes the chat.
    fn block_peer(&mut self, w: &Window) {
        let text = if self.known {
            tr!(block_recipient_confirm, name = self.name)
        } else {
            tr!(block_confirm, name = self.name)
        };
        if !confirm(w.hwnd, &text) {
            return;
        }
        app::block_sender(&self.name, &self.peer, std::ptr::null_mut());
        w.close();
    }

    fn delete_conversation(&mut self, w: &Window) {
        if !confirm_no(w.hwnd, &tr!(chat_delete_confirm, name = self.name)) {
            return;
        }
        if let Err(e) = self.history.clear() {
            super::error(w.hwnd, &tr!(chat_delete_failed, error = e));
            return;
        }
        self.clear();
        window::focus(self.input);
        self.say(tr!(chat_deleted), true);
    }

    /// Copies the selected messages, one per line, as the history shows them.
    fn copy_selected(&mut self, w: &Window) {
        let indices = selected(self.list);
        if indices.is_empty() {
            return;
        }
        let lines: Vec<String> = indices.iter().map(|&i| item_text(self.list, i)).collect();
        let said = if shell::copy_to_clipboard(w.hwnd, &lines.join("\r\n")) {
            trn!(chat_copied, indices.len())
        } else {
            tr!(clipboard_busy).to_string()
        };
        self.say(&said, true);
    }

    fn select_all(&mut self) {
        let count = send(self.list, LB_GETCOUNT, 0, 0);
        if count <= 0 {
            return;
        }
        send(self.list, LB_SETSEL, 1, -1);
        // Nothing else tells a screen reader of it.
        self.say(&trn!(chat_all_selected, count as usize), true);
    }

    /// Deletes the selected messages from the chat on this computer, after
    /// the user confirms it. The other person keeps them. The message that
    /// followed the first one deleted is selected then; with none left, the
    /// focus goes to the message field.
    fn delete_selected(&mut self, w: &Window) {
        let indices = selected(self.list);
        if indices.is_empty() {
            return;
        }
        if !confirm_no(w.hwnd, &trn!(chat_delete_messages_confirm, indices.len())) {
            return;
        }
        // The rows are the chat's messages, one for one; nothing changes
        // them while the question is open (the window is busy then).
        let saved = self.history.remove(&indices);
        for &index in indices.iter().rev() {
            send(self.list, LB_DELETESTRING, index, 0);
        }
        let count = send(self.list, LB_GETCOUNT, 0, 0).max(0) as usize;
        if count == 0 {
            window::focus(self.input);
            window::enable(self.delete, false);
        } else {
            select_only(self.list, indices[0].min(count - 1));
        }
        self.show_selected();
        let said = match saved {
            Ok(()) => trn!(chat_messages_deleted, indices.len()),
            Err(e) => tr!(chat_not_saved, error = e),
        };
        self.say(&said, true);
    }

    /// The context menu of the history, at `at` (screen coordinates) for
    /// the mouse, or at the message with the focus rectangle for the
    /// keyboard. A click on a message that is not selected selects it alone,
    /// as File Explorer does.
    fn history_menu(&mut self, w: &Window, at: Option<POINT>) {
        let pt = match at {
            Some(pt) => {
                let mut client = pt;
                unsafe { ScreenToClient(self.list, &mut client) };
                let xy = ((client.y as u16 as isize) << 16) | client.x as u16 as isize;
                let hit = send(self.list, LB_ITEMFROMPOINT, 0, xy);
                let (index, outside) = ((hit & 0xFFFF) as usize, (hit >> 16) & 0xFFFF != 0);
                if !outside && send(self.list, LB_GETSEL, index, 0) <= 0 {
                    select_only(self.list, index);
                    self.show_selected();
                }
                window::focus(self.list);
                pt
            }
            None => {
                let mut list_rect: RECT = unsafe { std::mem::zeroed() };
                unsafe { GetClientRect(self.list, &mut list_rect) };
                let mut item: RECT = unsafe { std::mem::zeroed() };
                let caret = send(self.list, LB_GETCARETINDEX, 0, 0);
                let found = caret >= 0
                    && send(
                        self.list,
                        LB_GETITEMRECT,
                        caret as usize,
                        &mut item as *mut _ as isize,
                    ) != LB_ERR as isize;
                // Where the message is in view, or else the top of the list.
                let y = if found && item.top >= 0 && item.top < list_rect.bottom {
                    item.top
                } else {
                    0
                };
                let mut p = POINT {
                    x: w.px(ITEM_PAD * 4),
                    y: y + w.px(ITEM_PAD * 4).min(list_rect.bottom - y).max(0),
                };
                unsafe { MapWindowPoints(self.list, std::ptr::null_mut(), &mut p, 1) };
                p
            }
        };
        let count = send(self.list, LB_GETCOUNT, 0, 0);
        let any = send(self.list, LB_GETSELCOUNT, 0, 0) > 0;
        let popup = unsafe { CreatePopupMenu() };
        menu::item_enabled(popup, ID_COPY, tr!(chat_copy), any);
        menu::item_enabled(popup, ID_SELECT_ALL, tr!(chat_select_all), count > 0);
        menu::separator(popup);
        menu::item_enabled(popup, ID_DELETE_SELECTED, tr!(chat_delete_message), any);
        match menu::track(popup, w.hwnd, pt) {
            Some(ID_COPY) => self.copy_selected(w),
            Some(ID_SELECT_ALL) => self.select_all(),
            Some(ID_DELETE_SELECTED) => self.delete_selected(w),
            _ => {}
        }
    }

    /// Shows a chat without messages (they were deleted).
    fn clear(&mut self) {
        // The file is gone already when all chats were deleted at once.
        let _ = self.history.clear();
        send(self.list, LB_RESETCONTENT, 0, 0);
        self.show_selected();
        self.unspoken.clear();
        if window::focused() == self.delete {
            window::focus(self.input);
        }
        window::enable(self.delete, false);
    }
}

impl Handler for Chat {
    fn on_create(&mut self, w: &Window) {
        // Screen readers read the warning and the status line when the
        // window comes to the front.
        w.set_dialog_role();
        let x = MARGIN;
        let mut y = 12;
        let warning = tr!(chat_unknown_warning, id = short_id(&self.peer));
        let warning_h = w.text_height(&warning, CONTENT_W).max(18);
        self.warning = w.label(&warning, (x, y, CONTENT_W, warning_h));
        if self.known {
            window::show(self.warning, false);
        } else {
            y += warning_h + 10;
        }
        // The caption right before the list gives it its accessible name.
        w.caption(tr!(chat_history_label), (x, y, CONTENT_W, 18));
        y += 20;
        self.list = w.add_ex(
            "LISTBOX",
            "",
            WS_TABSTOP
                | WS_VSCROLL
                | (LBS_NOTIFY
                    | LBS_NOINTEGRALHEIGHT
                    | LBS_OWNERDRAWVARIABLE
                    | LBS_HASSTRINGS
                    | LBS_EXTENDEDSEL) as u32,
            WS_EX_CLIENTEDGE,
            ID_HISTORY,
            (x, y, CONTENT_W, HISTORY_H),
        );
        unsafe { SetWindowSubclass(self.list, Some(history_proc), 0, 0) };
        y += HISTORY_H + 8;
        // The caption gives the box its accessible name.
        w.caption(tr!(chat_selected_label), (x, y, CONTENT_W, 18));
        y += 20;
        self.selected = w.read_only_text("", ID_SELECTED, (x, y, CONTENT_W, SELECTED_H));
        y += SELECTED_H + 8;
        // One line: the message that arrived last (either way), that the
        // other person is typing, or why a message was not sent.
        self.status = w.add(
            "STATIC",
            "",
            SS_NOPREFIX | SS_ENDELLIPSIS,
            0,
            (x, y, CONTENT_W, 18),
        );
        w.live_region(self.status);
        y += 26;
        w.caption(tr!(chat_message_label), (x, y, CONTENT_W, 18));
        y += 20;
        let send_w = w.button_width(tr!(chat_send), 86);
        self.input = w.edit("", ID_INPUT, (x, y, CONTENT_W - send_w - 6, 24));
        send(self.input, EM_LIMITTEXT, MAX_CHARS, 0);
        w.button(
            tr!(chat_send),
            ID_SEND,
            (x + CONTENT_W - send_w, y - 1, send_w, 26),
        );
        y += 40;
        let mut left = x;
        let add_w = w.button_width(tr!(chat_add), 96);
        self.add = w.button(tr!(chat_add), ID_ADD, (left, y, add_w, 30));
        if self.known {
            window::show(self.add, false);
        } else {
            left += add_w + 6;
        }
        let block_w = w.button_width(tr!(chat_block), 86);
        self.block = w.button(tr!(chat_block), ID_BLOCK, (left, y, block_w, 30));
        left += block_w + 6;
        let delete_w = w.button_width(tr!(chat_delete), 96);
        self.delete = w.button(tr!(chat_delete), ID_DELETE, (left, y, delete_w, 30));
        left += delete_w + 6;
        // Escape closes it too; it needs no access key.
        let close_w = 90;
        let width = (left + close_w + MARGIN).max(WIDTH);
        self.close = w.button(
            tr!(close),
            IDCANCEL as u16,
            (width - MARGIN - close_w, y, close_w, 30),
        );
        w.set_client_size(width, y + 30 + MARGIN);
        // Enter sends, wherever the focus is but on another button.
        w.default_id.set(ID_SEND);
        self.fill(w);
        window::focus(self.input);
    }

    fn on_command(&mut self, w: &Window, id: u16, code: u16) -> bool {
        match id {
            ID_SEND => self.send_message(w),
            ID_HISTORY if code == LBN_SELCHANGE as u16 => self.show_selected(),
            ID_INPUT if code == EN_CHANGE => {
                let writing = !get_text(self.input).is_empty();
                self.tell_typing(w, writing);
            }
            ID_ADD => self.add_recipient(w),
            ID_BLOCK => self.block_peer(w),
            ID_DELETE => self.delete_conversation(w),
            ID_COPY => self.copy_selected(w),
            ID_SELECT_ALL => self.select_all(),
            ID_DELETE_SELECTED => self.delete_selected(w),
            _ => return false,
        }
        true
    }

    /// The size of the text may have changed (another monitor's scaling).
    fn on_size(&mut self, w: &Window, _cx: i32, _cy: i32) {
        if self.list.is_null() {
            return;
        }
        for (index, entry) in self.history.entries().iter().enumerate() {
            self.size_item(w, index, &self.line(entry));
        }
    }

    fn on_message(&mut self, w: &Window, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            // From the keyboard (Shift+F10, the Menu key), the position is
            // -1, -1.
            WM_CONTEXTMENU if wp as HWND == self.list => {
                let at = (lp as u32 != u32::MAX).then(|| POINT {
                    x: (lp & 0xFFFF) as i16 as i32,
                    y: ((lp >> 16) & 0xFFFF) as i16 as i32,
                });
                self.history_menu(w, at);
                Some(0)
            }
            WM_TIMER => match wp {
                TIMER_SPEAK => {
                    unsafe { KillTimer(w.hwnd, TIMER_SPEAK) };
                    let front = unsafe { GetForegroundWindow() } == w.hwnd;
                    self.speak(front);
                    Some(0)
                }
                TIMER_PEER_TYPING => {
                    self.peer_typing(w, false, false);
                    Some(0)
                }
                TIMER_MY_TYPING => {
                    self.tell_typing(w, false);
                    Some(0)
                }
                _ => None,
            },
            // The user went elsewhere: they are not typing here now. The
            // window does the rest itself.
            WM_ACTIVATE if (wp & 0xFFFF) as u32 == WA_INACTIVE => {
                self.tell_typing(w, false);
                None
            }
            _ => None,
        }
    }

    fn on_destroy(&mut self, w: &Window) {
        self.tell_typing(w, false);
        app::chat_window_closed(&self.peer, w.hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_is_named_without_case() {
        assert_eq!(key_of("AbC123"), "abc123");
    }

    #[test]
    fn times_are_written_for_the_user() {
        // A time of another day has its date; one of now only the time.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let today = time_text(now);
        let earlier = time_text(now - 40 * 86_400);
        assert!(!today.is_empty());
        assert!(earlier.len() > today.len(), "{earlier:?} {today:?}");
        assert!(earlier.contains(' '));
    }
}
