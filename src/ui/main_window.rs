//! The main window: the recipient list, menus, buttons and the tray icon.

use std::{cell::Cell, path::PathBuf};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{MapWindowPoints, ScreenToClient, UpdateWindow},
    System::DataExchange::COPYDATASTRUCT,
    UI::{
        Controls::*,
        Input::KeyboardAndMouse::{
            GetFocus, IsWindowEnabled, VK_DELETE, VK_F1, VK_F2, VK_OEM_COMMA,
        },
        Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP},
        WindowsAndMessaging::*,
    },
};

use super::{
    APP_NAME, DONATE_URL, ask, confirm, info, progress, send, send_to, set_text, shell, short_id,
    tray, wide,
    window::{self, CreateOptions, Handler, Window},
    without_shortcut,
};
use crate::{
    app,
    config::Contact,
    i18n::{tr, trn},
    net::Kind,
};

const ID_SEND_FILE: u16 = 101;
const ID_SEND_FOLDER: u16 = 102;
const ID_ADD: u16 = 103;
const ID_EDIT: u16 = 104;
const ID_REMOVE: u16 = 105;
const ID_COPY_ID: u16 = 106;
const ID_SETTINGS: u16 = 107;
const ID_EXIT: u16 = 108;
const ID_HELP: u16 = 109;
const ID_ABOUT: u16 = 110;
const ID_TRAY_OPEN: u16 = 111;
const ID_VIEW_HELP: u16 = 112;
const ID_SHOW_TRANSFERS: u16 = 113;
/// Shows the running transfers with the selected recipient.
const ID_SHOW_CONTACT_TRANSFERS: u16 = 114;
const ID_CHECK_UPDATES: u16 = 115;
const ID_LICENSE: u16 = 116;
const ID_DONATE: u16 = 117;
const ID_LIST: u16 = 200;
/// The first automatic update check, shortly after the start.
const TIMER_UPDATE_START: usize = 1;
/// Looks regularly whether the daily update check is due.
const TIMER_UPDATE_TICK: usize = 2;
/// The running transfers listed in the Transfers menu, in menu order.
const ID_TRANSFER_FIRST: u16 = 300;
const ID_TRANSFER_LAST: u16 = 349;
/// The list's Transfer column.
const COL_TRANSFER: usize = 2;

const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
/// Command links of the exit question.
const CHOICE_EXIT_KEEP: i32 = 100;
const CHOICE_EXIT_CANCEL: i32 = 101;
const CHOICE_EXIT_STAY: i32 = 102;

thread_local! {
    static MAIN: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static ACCEL: Cell<HACCEL> = const { Cell::new(std::ptr::null_mut()) };
    static MSG_TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static MSG_SHOW: Cell<u32> = const { Cell::new(0) };
    static MSG_EXIT_FOR_UPDATE: Cell<u32> = const { Cell::new(0) };
    static LIST: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static STATUS: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
}

/// The window message another instance posts to bring this one to the front.
pub fn show_message_id() -> u32 {
    unsafe { RegisterWindowMessageW(wide("Dodavo_Show").as_ptr()) }
}

/// The window message the instance that installs an update posts to the
/// others (other profiles): exit now, with the transfers suspended.
pub fn exit_for_update_message_id() -> u32 {
    unsafe { RegisterWindowMessageW(wide("Dodavo_ExitForUpdate").as_ptr()) }
}

pub fn window_title(profile: Option<&str>) -> String {
    match profile {
        Some(p) => format!("{APP_NAME} [{p}]"),
        None => APP_NAME.to_string(),
    }
}

struct MainWindow {
    list: HWND,
    label: HWND,
    status: HWND,
    send_file: HWND,
    send_folder: HWND,
    add: HWND,
    copy: HWND,
    /// Widths of the four buttons above, in 96-DPI units.
    widths: [i32; 4],
    /// The Transfers menu of the menu bar; filled when it opens.
    transfers_menu: HMENU,
    /// The transfer ids behind `ID_TRANSFER_FIRST + index`.
    menu_transfers: Vec<u64>,
}

/// Appends a command to a menu.
fn menu_item(menu: HMENU, id: u16, text: &str) {
    let text = wide(text);
    unsafe { AppendMenuW(menu, MF_STRING, id as usize, text.as_ptr()) };
}

/// Appends a text that cannot be chosen (an explanation).
fn menu_note(menu: HMENU, text: &str) {
    let text = wide(text);
    unsafe { AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, text.as_ptr()) };
}

fn clear_menu(menu: HMENU) {
    unsafe {
        while GetMenuItemCount(menu) > 0 {
            DeleteMenu(menu, 0, MF_BYPOSITION);
        }
    }
}

fn menu_separator(menu: HMENU) {
    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
}

/// Appends a drop-down menu to a menu bar.
fn menu_popup(bar: HMENU, popup: HMENU, text: &str) {
    let text = wide(text);
    unsafe { AppendMenuW(bar, MF_POPUP, popup as usize, text.as_ptr()) };
}

/// Shows a context menu at a screen position and returns the chosen command.
fn track_popup(menu: HMENU, owner: HWND, pt: POINT) -> Option<u16> {
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            owner,
            std::ptr::null(),
        )
    };
    unsafe { DestroyMenu(menu) };
    (cmd != 0).then_some(cmd as u16)
}

/// Builds the menu bar; also returns its Transfers menu, which is filled
/// each time it opens.
fn build_menu() -> (HMENU, HMENU) {
    let bar = unsafe { CreateMenu() };
    let file = unsafe { CreatePopupMenu() };
    menu_item(file, ID_SEND_FILE, tr!(menu_send_file));
    menu_item(file, ID_SEND_FOLDER, tr!(menu_send_folder));
    menu_separator(file);
    menu_item(file, ID_ADD, tr!(menu_add));
    menu_item(file, ID_EDIT, tr!(menu_edit));
    menu_item(file, ID_REMOVE, tr!(menu_remove));
    menu_separator(file);
    menu_item(file, ID_COPY_ID, tr!(menu_copy_id));
    menu_item(file, ID_SETTINGS, tr!(menu_settings));
    menu_separator(file);
    menu_item(file, ID_EXIT, tr!(menu_exit));
    menu_popup(bar, file, tr!(menu_file));

    let transfers = unsafe { CreatePopupMenu() };
    menu_popup(bar, transfers, tr!(menu_transfers));

    let help = unsafe { CreatePopupMenu() };
    menu_item(help, ID_VIEW_HELP, tr!(menu_view_help));
    menu_item(help, ID_HELP, tr!(menu_how));
    menu_separator(help);
    menu_item(help, ID_LICENSE, tr!(menu_license));
    menu_item(help, ID_DONATE, tr!(menu_donate));
    menu_separator(help);
    menu_item(help, ID_CHECK_UPDATES, tr!(menu_check_updates));
    menu_item(help, ID_ABOUT, tr!(menu_about));
    menu_popup(bar, help, tr!(menu_help));
    (bar, transfers)
}

/// The shortcuts of the main window. Menus show them after a tab in their
/// texts (`i18n`), and the help lists them.
fn accelerators() -> [ACCEL; 8] {
    let virt = FVIRTKEY | FCONTROL;
    [
        ACCEL {
            fVirt: virt,
            key: b'O' as u16,
            cmd: ID_SEND_FILE,
        },
        ACCEL {
            fVirt: virt | FSHIFT,
            key: b'O' as u16,
            cmd: ID_SEND_FOLDER,
        },
        ACCEL {
            fVirt: virt,
            key: b'N' as u16,
            cmd: ID_ADD,
        },
        ACCEL {
            fVirt: virt | FSHIFT,
            key: b'C' as u16,
            cmd: ID_COPY_ID,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: VK_F2,
            cmd: ID_EDIT,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: VK_DELETE,
            cmd: ID_REMOVE,
        },
        ACCEL {
            fVirt: virt,
            key: b'T' as u16,
            cmd: ID_SHOW_TRANSFERS,
        },
        // The key the Mac has for its settings, with Command.
        ACCEL {
            fVirt: virt,
            key: VK_OEM_COMMA,
            cmd: ID_SETTINGS,
        },
    ]
}

fn build_accelerators() -> HACCEL {
    let table = accelerators();
    unsafe { CreateAcceleratorTableW(table.as_ptr(), table.len() as i32) }
}

pub fn create(profile: Option<&str>, start_hidden: bool) -> HWND {
    MSG_TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) });
    MSG_SHOW.set(show_message_id());
    MSG_EXIT_FOR_UPDATE.set(exit_for_update_message_id());
    ACCEL.set(build_accelerators());
    let (menu, transfers_menu) = build_menu();
    let handler = MainWindow {
        list: std::ptr::null_mut(),
        label: std::ptr::null_mut(),
        status: std::ptr::null_mut(),
        send_file: std::ptr::null_mut(),
        send_folder: std::ptr::null_mut(),
        add: std::ptr::null_mut(),
        copy: std::ptr::null_mut(),
        widths: [110, 110, 120, 144],
        transfers_menu,
        menu_transfers: Vec::new(),
    };
    let title = window_title(profile);
    let hwnd = window::create(
        CreateOptions {
            title: &title,
            main_class: true,
            style: WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_ACCEPTFILES,
            menu,
            client: (600, 400),
            urgent: Some(urgent_message),
            ..Default::default()
        },
        Box::new(handler),
    );
    MAIN.set(hwnd);
    app::set_main_window(hwnd);
    unsafe {
        SetTimer(
            hwnd,
            TIMER_UPDATE_START,
            crate::update::START_DELAY.as_millis() as u32,
            None,
        );
        SetTimer(
            hwnd,
            TIMER_UPDATE_TICK,
            crate::update::TICK.as_millis() as u32,
            None,
        );
    }
    // The list view is created outside of the handler so that the notifications
    // it sends while being created (e.g. for the empty-list text) reach the handler.
    let list = window::with_window(hwnd, create_list).unwrap();
    LIST.set(list);
    window::with_handler::<MainWindow, _>(hwnd, |h, w| {
        h.list = list;
        let (cx, cy) = w.client_size_dip();
        if cx < h.min_client_width() {
            w.set_client_size(h.min_client_width(), cy);
        }
        let (cx, cy) = w.client_size_dip();
        h.on_size(w, cx, cy);
        unsafe {
            // Tab order: right after the "Recipients:" label.
            SetWindowPos(
                list,
                h.label,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    });
    super::theme::apply_to_window(hwnd);
    unsafe { DragAcceptFiles(hwnd, 1) };
    tray::add(hwnd);
    // The first recipient is selected; the list gets the focus when the window is shown.
    refresh_list();
    window::focus(list);
    if !start_hidden {
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            // Started by the user (desktop, Explorer, Start menu): take the
            // foreground even when the launcher did not hand it over.
            SetForegroundWindow(hwnd);
        }
    }
    hwnd
}

fn create_list(w: &Window) -> HWND {
    let list = w.add_ex(
        "SysListView32",
        "",
        WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
        WS_EX_CLIENTEDGE,
        ID_LIST,
        (12, 30, 576, 280),
    );
    send(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize,
    );
    for (i, title) in [tr!(col_name), tr!(col_contact_id), tr!(col_transfer)]
        .iter()
        .enumerate()
    {
        let mut text = wide(title);
        let mut col: LVCOLUMNW = unsafe { std::mem::zeroed() };
        col.mask = LVCF_TEXT | LVCF_WIDTH;
        col.cx = w.px(200);
        col.pszText = text.as_mut_ptr();
        send(list, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
    }
    list
}

/// Keyboard handling for the message loops: accelerators and Tab navigation.
pub fn translate(msg: &MSG) -> bool {
    unsafe {
        let root = GetAncestor(msg.hwnd, GA_ROOT);
        if root.is_null() {
            return false;
        }
        // F1 opens the help from every window of the program.
        if msg.message == WM_KEYDOWN
            && msg.wParam == VK_F1 as usize
            && window::all_windows().contains(&root)
        {
            super::help::show(root);
            return true;
        }
        let main = MAIN.get();
        if root == main
            && IsWindowEnabled(main) != 0
            && TranslateAcceleratorW(main, ACCEL.get(), msg) != 0
        {
            return true;
        }
        if window::all_windows().contains(&root) {
            return IsDialogMessageW(root, msg) != 0;
        }
        false
    }
}

pub fn set_status(text: &str) {
    set_text(STATUS.get(), text);
}

pub fn set_online(online: bool) {
    set_text(STATUS.get(), status_text(Some(online)));
}

/// Shows the connection state again after a temporary status text.
pub fn restore_status() {
    set_text(STATUS.get(), status_text(app::with(|a| a.online)));
}

fn status_text(online: Option<bool>) -> &'static str {
    match online {
        None => tr!(status_connecting),
        Some(true) => tr!(status_online),
        Some(false) => tr!(status_offline),
    }
}

fn selected_index() -> Option<usize> {
    list_selection(LIST.get())
}

fn select_index(index: usize) {
    list_select(LIST.get(), index);
}

/// The selected row of a list view that allows one.
pub fn list_selection(list: HWND) -> Option<usize> {
    let i = send(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
    (i >= 0).then_some(i as usize)
}

/// Selects a row of a list view and gives it the list's focus.
pub fn list_select(list: HWND, index: usize) {
    let mut item: LVITEMW = unsafe { std::mem::zeroed() };
    item.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
    item.state = LVIS_SELECTED | LVIS_FOCUSED;
    send(list, LVM_SETITEMSTATE, index, &item as *const _ as isize);
    send(list, LVM_ENSUREVISIBLE, index, 0);
}

fn set_cell(list: HWND, index: usize, column: usize, text: &str) {
    let mut text = wide(text);
    let mut item: LVITEMW = unsafe { std::mem::zeroed() };
    item.mask = LVIF_TEXT;
    item.iItem = index as i32;
    item.iSubItem = column as i32;
    item.pszText = text.as_mut_ptr();
    send(list, LVM_SETITEMTEXTW, index, &item as *const _ as isize);
}

/// Rebuilds the recipient list from the configuration.
pub fn refresh_list() {
    let list = LIST.get();
    let contacts = app::cfg().contacts;
    let selected = selected_index();
    send(list, LVM_DELETEALLITEMS, 0, 0);
    for (i, c) in contacts.iter().enumerate() {
        let mut name = wide(&c.name);
        let mut item: LVITEMW = unsafe { std::mem::zeroed() };
        item.mask = LVIF_TEXT;
        item.iItem = i as i32;
        item.pszText = name.as_mut_ptr();
        send(list, LVM_INSERTITEMW, 0, &item as *const _ as isize);
        set_cell(list, i, 1, &short_id(&c.id));
        set_cell(list, i, COL_TRANSFER, &app::transfer_status(&c.id));
    }
    if let Some(sel) = selected.filter(|&s| s < contacts.len()) {
        select_index(sel);
    } else if !contacts.is_empty() {
        select_index(0);
    }
    send_to::refresh_recipients();
}

/// Shows the state of a peer's transfers in the list. Only that cell changes,
/// so the selection, the focus and the other cells stay as they are.
pub fn update_transfer_status(peer_id: &str) {
    let list = LIST.get();
    if list.is_null() {
        return;
    }
    let index = app::with(|a| {
        a.cfg
            .contacts
            .iter()
            .position(|c| c.id.eq_ignore_ascii_case(peer_id))
    });
    if let Some(i) = index {
        set_cell(list, i, COL_TRANSFER, &app::transfer_status(peer_id));
    }
}

impl MainWindow {
    /// Room for all buttons, with a gap between the left and the right pair.
    fn min_client_width(&self) -> i32 {
        self.widths.iter().sum::<i32>() + 2 * 6 + 2 * 12 + 24
    }

    fn selected(&self) -> Option<usize> {
        selected_index()
    }

    fn select(&self, index: usize) {
        select_index(index);
    }

    fn selected_contact(&self, w: &Window) -> Option<Contact> {
        let contacts = app::cfg().contacts;
        match self.selected().and_then(|i| contacts.get(i).cloned()) {
            Some(c) => Some(c),
            None => {
                let text = if contacts.is_empty() {
                    tr!(no_recipients_yet)
                } else {
                    tr!(select_recipient_first)
                };
                info(w.hwnd, text);
                None
            }
        }
    }

    fn send_item(&self, w: &Window, kind: Kind) {
        let Some(contact) = self.selected_contact(w) else {
            return;
        };
        let title = match kind {
            Kind::File => tr!(pick_file_title, name = contact.name),
            Kind::Folder => tr!(pick_folder_title, name = contact.name),
        };
        let paths = shell::pick_many(w.hwnd, &title, kind == Kind::Folder);
        // Only the first window takes the focus, which then stays there while
        // the others open.
        for (i, path) in paths.into_iter().enumerate() {
            app::start_send(&contact, path, kind, i == 0);
        }
    }

    fn add_recipient(&self, w: &Window) {
        if let Some(contact) = super::recipient::show(w.hwnd, None) {
            let name = contact.name.clone();
            app::update_cfg(|c| {
                c.contacts.push(contact);
                c.contacts.sort_by_key(|c| c.name.to_lowercase());
            });
            self.refresh_and_select(&name);
        }
    }

    fn edit_recipient(&self, w: &Window) {
        let Some(index) = self.selected() else { return };
        let Some(existing) = app::cfg().contacts.get(index).cloned() else {
            return;
        };
        if let Some(contact) = super::recipient::show(w.hwnd, Some(&existing)) {
            let name = contact.name.clone();
            app::update_cfg(|c| {
                if let Some(slot) = c.contacts.iter_mut().find(|c| **c == existing) {
                    *slot = contact;
                }
                c.contacts.sort_by_key(|c| c.name.to_lowercase());
            });
            self.refresh_and_select(&name);
        }
    }

    fn refresh_and_select(&self, name: &str) {
        refresh_list();
        if let Some(i) = app::cfg().contacts.iter().position(|c| c.name == name) {
            self.select(i);
        }
    }

    fn remove_recipient(&self, w: &Window) {
        let Some(index) = self.selected() else { return };
        let Some(contact) = app::cfg().contacts.get(index).cloned() else {
            return;
        };
        if confirm(w.hwnd, &tr!(remove_confirm, name = contact.name)) {
            app::update_cfg(|c| c.contacts.retain(|x| *x != contact));
            refresh_list();
        }
    }

    fn copy_my_id(&self, w: &Window) {
        let id = app::with(|a| a.my_id.clone());
        if shell::copy_to_clipboard(w.hwnd, &id) {
            info(w.hwnd, &tr!(id_copied, id = id));
        } else {
            super::error(w.hwnd, tr!(clipboard_busy));
        }
    }

    /// Brings the windows of all running transfers back.
    fn show_transfers(&self, w: &Window) {
        let transfers = app::active_transfer_list();
        if transfers.is_empty() {
            info(w.hwnd, tr!(menu_no_transfers));
        }
        for (_, hwnd, _) in transfers {
            progress::show(hwnd);
        }
    }

    /// Brings the windows of the running transfers with the selected recipient back.
    fn show_contact_transfers(&self, w: &Window) {
        let Some(contact) = self.selected_contact(w) else {
            return;
        };
        for hwnd in app::transfer_windows_of(&contact.id) {
            progress::show(hwnd);
        }
    }

    /// Brings one transfer chosen from the Transfers menu back.
    fn show_transfer(&self, id: u16) {
        let Some(&transfer) = self.menu_transfers.get((id - ID_TRANSFER_FIRST) as usize) else {
            return;
        };
        if let Some(hwnd) = app::transfer_window(transfer) {
            progress::show(hwnd);
        }
    }

    /// Fills a menu with the running transfers and the "Show All" command,
    /// with its shortcut if `shortcuts` (they work in the menu bar only).
    fn fill_transfers_menu(&mut self, menu: HMENU, shortcuts: bool) {
        clear_menu(menu);
        self.menu_transfers.clear();
        let transfers = app::active_transfer_list();
        if transfers.is_empty() {
            menu_note(menu, tr!(menu_no_transfers));
        }
        let room = (ID_TRANSFER_LAST - ID_TRANSFER_FIRST + 1) as usize;
        for (i, (id, _, text)) in transfers.into_iter().take(room).enumerate() {
            self.menu_transfers.push(id);
            // Names and file names are not access keys.
            let text = text.replace('&', "&&");
            menu_item(menu, ID_TRANSFER_FIRST + i as u16, &text);
        }
        menu_separator(menu);
        let show_all = tr!(menu_show_transfers);
        let show_all = if shortcuts {
            show_all
        } else {
            without_shortcut(show_all)
        };
        menu_item(menu, ID_SHOW_TRANSFERS, show_all);
    }

    /// Enter in the list: `IsDialogMessageW` turns it into `IDOK` because the
    /// window has no default button. It opens the recipient's actions menu.
    fn enter_pressed(&mut self, w: &Window) -> bool {
        if unsafe { GetFocus() } != self.list || self.selected().is_none() {
            return false;
        }
        self.context_menu(w, None);
        true
    }

    fn command(&mut self, w: &Window, id: u16) -> bool {
        match id {
            ID_SEND_FILE => self.send_item(w, Kind::File),
            ID_SEND_FOLDER => self.send_item(w, Kind::Folder),
            ID_ADD => self.add_recipient(w),
            ID_EDIT => self.edit_recipient(w),
            ID_REMOVE => self.remove_recipient(w),
            ID_COPY_ID => self.copy_my_id(w),
            ID_SHOW_TRANSFERS => self.show_transfers(w),
            ID_SHOW_CONTACT_TRANSFERS => self.show_contact_transfers(w),
            ID_TRANSFER_FIRST..=ID_TRANSFER_LAST => self.show_transfer(id),
            id if id == IDOK as u16 => return self.enter_pressed(w),
            ID_SETTINGS => {
                super::settings::show(w.hwnd);
            }
            ID_EXIT => quit(w),
            ID_TRAY_OPEN => window::bring_to_front(w.hwnd),
            ID_VIEW_HELP => super::help::show(w.hwnd),
            ID_HELP => info(w.hwnd, tr!(help_text)),
            ID_CHECK_UPDATES => super::update::check_now(w.hwnd),
            ID_LICENSE => super::license::show(w.hwnd),
            ID_DONATE => super::about::open_link(w.hwnd, DONATE_URL),
            ID_ABOUT => super::about::show(w.hwnd),
            _ => return false,
        }
        true
    }

    /// The actions for the selected recipient (or "Add Recipient" when there
    /// is none), at `screen` or below the selected row.
    fn context_menu(&mut self, w: &Window, screen: Option<POINT>) {
        let contact = self
            .selected()
            .and_then(|i| app::cfg().contacts.get(i).cloned());
        let pt = screen.unwrap_or_else(|| {
            let mut rc: RECT = unsafe { std::mem::zeroed() };
            if let Some(i) = self.selected() {
                rc.left = LVIR_BOUNDS as i32;
                send(self.list, LVM_GETITEMRECT, i, &mut rc as *mut _ as isize);
            }
            let mut p = POINT {
                x: rc.left + 20,
                y: rc.bottom,
            };
            unsafe { MapWindowPoints(self.list, std::ptr::null_mut(), &mut p, 1) };
            p
        });
        let menu = unsafe { CreatePopupMenu() };
        if let Some(contact) = contact {
            menu_item(menu, ID_SEND_FILE, tr!(ctx_send_file));
            menu_item(menu, ID_SEND_FOLDER, tr!(ctx_send_folder));
            match app::transfer_windows_of(&contact.id).len() {
                0 => {}
                1 => menu_item(menu, ID_SHOW_CONTACT_TRANSFERS, tr!(ctx_show_transfer)),
                _ => menu_item(menu, ID_SHOW_CONTACT_TRANSFERS, tr!(ctx_show_transfers)),
            }
            menu_separator(menu);
            menu_item(menu, ID_EDIT, tr!(ctx_edit));
            menu_item(menu, ID_REMOVE, tr!(ctx_remove));
        } else {
            menu_item(menu, ID_ADD, tr!(ctx_add));
        }
        if let Some(cmd) = track_popup(menu, w.hwnd, pt) {
            self.command(w, cmd);
        }
    }

    /// The menu of the notification-area icon. The accelerators only work in
    /// the main window, not from here, so no item shows a shortcut.
    fn tray_menu(&mut self, w: &Window) {
        let item = |menu, id, text| menu_item(menu, id, without_shortcut(text));
        let menu = unsafe { CreatePopupMenu() };
        item(menu, ID_TRAY_OPEN, tr!(tray_open));
        // Destroyed together with its parent by `track_popup`.
        let transfers = unsafe { CreatePopupMenu() };
        self.fill_transfers_menu(transfers, false);
        menu_popup(menu, transfers, tr!(menu_transfers));
        item(menu, ID_COPY_ID, tr!(menu_copy_id));
        menu_separator(menu);
        item(menu, ID_EXIT, tr!(menu_exit));
        unsafe { SetMenuDefaultItem(menu, ID_TRAY_OPEN as u32, 0) };
        let mut pt = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        // Required so the menu closes when clicking elsewhere.
        unsafe { SetForegroundWindow(w.hwnd) };
        let cmd = track_popup(menu, w.hwnd, pt);
        unsafe { PostMessageW(w.hwnd, WM_NULL, 0, 0) };
        if let Some(cmd) = cmd {
            self.command(w, cmd);
        }
    }

    fn drop_files(&mut self, w: &Window, hdrop: HDROP) {
        let mut paths = Vec::new();
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
                paths.push(PathBuf::from(String::from_utf16_lossy(
                    &buf[..len as usize],
                )));
            }
            DragQueryPoint(hdrop, &mut pt);
            DragFinish(hdrop);
            MapWindowPoints(w.hwnd, self.list, &mut pt, 1);
        }
        let mut hit: LVHITTESTINFO = unsafe { std::mem::zeroed() };
        hit.pt = pt;
        let index = send(self.list, LVM_HITTEST, 0, &mut hit as *mut _ as isize);
        if index >= 0 {
            self.select(index as usize);
        }
        let Some(contact) = self.selected_contact(w) else {
            return;
        };
        if paths.len() > 1
            && !confirm(
                w.hwnd,
                &trn!(drop_confirm, paths.len(), name = contact.name),
            )
        {
            return;
        }
        for (i, path) in paths.into_iter().enumerate() {
            let kind = if path.is_dir() {
                Kind::Folder
            } else {
                Kind::File
            };
            app::start_send(&contact, path, kind, i == 0);
        }
    }
}

/// Says once that Dodavo goes on running in the notification area.
fn tray_hint() {
    let cfg = app::cfg();
    if cfg.minimize_to_tray && !cfg.tray_hint_shown {
        tray::balloon(app::main_window(), APP_NAME, tr!(tray_hint), None);
        app::update_cfg(|c| c.tray_hint_shown = true);
    }
}

/// The same for a window that closes while the main window does not show.
pub fn tray_hint_when_hidden() {
    if unsafe { IsWindowVisible(MAIN.get()) } == 0 {
        tray_hint();
    }
}

/// Asks whether to exit while transfers run; the default is to stay.
/// Returns `None` to stay, otherwise whether the transfers are to be
/// cancelled (suspended transfers continue at the next start). Without
/// running transfers there is nothing to ask.
pub fn confirm_exit(owner: HWND) -> Option<bool> {
    let active = app::active_transfer_count();
    if active == 0 {
        return Some(false);
    }
    let (choice, _) = ask(
        owner,
        &trn!(exit_instruction, active),
        tr!(exit_text),
        &[
            (CHOICE_EXIT_STAY, tr!(exit_stay)),
            (CHOICE_EXIT_KEEP, tr!(exit_keep)),
            (CHOICE_EXIT_CANCEL, tr!(exit_cancel_transfers)),
        ],
        false,
        None,
    );
    match choice {
        CHOICE_EXIT_KEEP => Some(false),
        CHOICE_EXIT_CANCEL => Some(true),
        // "Don't exit", Escape or the dialog's close button.
        _ => None,
    }
}

fn exit(main: HWND, cancel_transfers: bool) {
    if cancel_transfers {
        app::cancel_all_transfers();
    }
    // While the window still exists, so the icon is reliably removed.
    tray::remove(main);
    unsafe { DestroyWindow(main) };
    // `on_destroy` posts WM_QUIT only once the handler is free. If a dialog
    // is open (the exit came through `urgent_message`), its message loop
    // runs inside the handler; WM_QUIT now ends it and every loop below it,
    // as each passes WM_QUIT on.
    unsafe { PostQuitMessage(0) };
}

/// Exits the application. While transfers run, the user is asked first.
fn quit(w: &Window) {
    if let Some(cancel_transfers) = confirm_exit(w.hwnd) {
        exit(w.hwnd, cancel_transfers);
    }
}

impl Handler for MainWindow {
    fn on_create(&mut self, w: &Window) {
        // Button widths follow their (translated) text. The buttons have no
        // access keys, so Alt+letter opens the menus; their shortcuts are
        // the accelerators.
        self.widths = [
            w.button_width(tr!(send_file), 110),
            w.button_width(tr!(send_folder), 110),
            w.button_width(tr!(add_recipient), 120),
            w.button_width(tr!(copy_my_id), 144),
        ];
        self.label = w.caption(tr!(recipients_label), (12, 10, 300, 18));
        self.send_file = w.button(tr!(send_file), ID_SEND_FILE, (12, 320, 110, 30));
        self.send_folder = w.button(tr!(send_folder), ID_SEND_FOLDER, (128, 320, 110, 30));
        self.add = w.button(tr!(add_recipient), ID_ADD, (318, 320, 120, 30));
        self.copy = w.button(tr!(copy_my_id), ID_COPY_ID, (444, 320, 144, 30));
        self.status = w.label(status_text(None), (12, 360, 576, 18));
        STATUS.set(self.status);
    }

    fn on_size(&mut self, w: &Window, cx: i32, cy: i32) {
        let margin = 12;
        let button_y = cy - 30 - 34;
        let [file_w, folder_w, add_w, copy_w] = self.widths;
        w.place(self.label, (margin, 10, cx - 2 * margin, 18));
        w.place(
            self.list,
            (margin, 30, cx - 2 * margin, (button_y - 10 - 30).max(40)),
        );
        w.place(self.send_file, (margin, button_y, file_w, 30));
        w.place(
            self.send_folder,
            (margin + file_w + 6, button_y, folder_w, 30),
        );
        w.place(self.copy, (cx - margin - copy_w, button_y, copy_w, 30));
        w.place(
            self.add,
            (cx - margin - copy_w - 6 - add_w, button_y, add_w, 30),
        );
        w.place(self.status, (margin, cy - 26, cx - 2 * margin, 18));
        let list_width = w.px(cx - 2 * margin) - unsafe { GetSystemMetrics(SM_CXVSCROLL) } - 4;
        // Name and Contact ID (shortened) take 30% each; the transfer state the rest.
        let name_width = list_width * 3 / 10;
        for (column, width) in [
            (0, name_width),
            (1, name_width),
            (COL_TRANSFER, list_width - 2 * name_width),
        ] {
            send(self.list, LVM_SETCOLUMNWIDTH, column, width as isize);
        }
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        self.command(w, id)
    }

    fn on_notify(&mut self, w: &Window, hdr: &NMHDR, lparam: LPARAM) -> Option<LRESULT> {
        if hdr.idFrom != ID_LIST as usize {
            return None;
        }
        match hdr.code {
            // Like Enter: the recipient's actions menu, below the row.
            NM_DBLCLK => {
                if self.selected().is_some() {
                    self.context_menu(w, None);
                }
                Some(0)
            }
            LVN_GETEMPTYMARKUP => {
                let markup = unsafe { &mut *(lparam as *mut NMLVEMPTYMARKUP) };
                markup.dwFlags = EMF_CENTERED;
                let text = tr!(empty_list);
                for (i, c) in text
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .enumerate()
                    .take(markup.szMarkup.len())
                {
                    markup.szMarkup[i] = c;
                }
                Some(1)
            }
            _ => None,
        }
    }

    // Not WM_DESTROY in `on_message`: when Exit is chosen from a menu, the
    // window is destroyed while this handler is busy, so that message would
    // never reach it and the process would keep running without a window.
    fn on_destroy(&mut self, w: &Window) {
        tray::remove(w.hwnd);
        unsafe { PostQuitMessage(0) };
    }

    fn on_close(&mut self, w: &Window) {
        if app::cfg().minimize_to_tray {
            unsafe { ShowWindow(w.hwnd, SW_HIDE) };
            tray_hint();
        } else {
            quit(w);
        }
    }

    fn on_message(&mut self, w: &Window, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            tray::WM_APP_TRAY => {
                match lp as u32 {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => window::bring_to_front(w.hwnd),
                    WM_RBUTTONUP | WM_CONTEXTMENU => self.tray_menu(w),
                    NIN_BALLOONUSERCLICK => match app::with(|a| a.balloon_action.take()) {
                        Some(tray::BalloonAction::Reveal(path)) => shell::reveal_in_explorer(&path),
                        Some(tray::BalloonAction::Transfer(id)) => match app::transfer_window(id) {
                            Some(h) => progress::show(h),
                            None => window::bring_to_front(w.hwnd),
                        },
                        Some(tray::BalloonAction::Update) => match app::with(|a| a.update_window) {
                            h if h.is_null() => window::bring_to_front(w.hwnd),
                            h => window::bring_to_front(h),
                        },
                        Some(tray::BalloonAction::SendTo) => match send_to::open_window() {
                            Some(h) => window::bring_to_front(h),
                            None => window::bring_to_front(w.hwnd),
                        },
                        _ => window::bring_to_front(w.hwnd),
                    },
                    _ => {}
                }
                Some(0)
            }
            WM_INITMENUPOPUP if wp as HMENU == self.transfers_menu => {
                self.fill_transfers_menu(self.transfers_menu, true);
                Some(0)
            }
            WM_DROPFILES => {
                self.drop_files(w, wp as HDROP);
                Some(0)
            }
            WM_CONTEXTMENU if wp as HWND == self.list => {
                let pt = POINT {
                    x: (lp & 0xFFFF) as i16 as i32,
                    y: ((lp >> 16) & 0xFFFF) as i16 as i32,
                };
                if lp == -1 {
                    self.context_menu(w, None);
                } else {
                    // Make sure the item under the cursor is selected.
                    let mut client = pt;
                    unsafe { ScreenToClient(self.list, &mut client) };
                    let mut hit: LVHITTESTINFO = unsafe { std::mem::zeroed() };
                    hit.pt = client;
                    let index = send(self.list, LVM_HITTEST, 0, &mut hit as *mut _ as isize);
                    if index >= 0 {
                        self.select(index as usize);
                    }
                    self.context_menu(w, Some(pt));
                }
                Some(0)
            }
            WM_GETMINMAXINFO => {
                let info = unsafe { &mut *(lp as *mut MINMAXINFO) };
                // The client width plus some room for the window frame.
                info.ptMinTrackSize = POINT {
                    x: w.px((self.min_client_width() + 16).max(520)),
                    y: w.px(320),
                };
                Some(0)
            }
            m if m == MSG_SHOW.get() => {
                window::bring_to_front(w.hwnd);
                Some(0)
            }
            WM_TIMER if wp == TIMER_UPDATE_START => {
                unsafe { KillTimer(w.hwnd, TIMER_UPDATE_START) };
                // Leftovers of an earlier update; a Setup that is still
                // running from such a file keeps it, which is fine.
                crate::update::cleanup();
                super::update::tick();
                Some(0)
            }
            WM_TIMER if wp == TIMER_UPDATE_TICK => {
                super::update::tick();
                Some(0)
            }
            _ => None,
        }
    }
}

/// The main window's messages that must be handled even while one of its
/// dialogs is open (`CreateOptions::urgent`): Setup and Windows send each of
/// them once, and they need nothing of the handler.
fn urgent_message(w: &Window, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        // Items for sending, from another start of Dodavo.
        WM_COPYDATA => {
            // SAFETY: for this message Windows passes a valid struct, and
            // `cbData` bytes at `lpData`, which stay valid until it returns.
            let data = unsafe { &*(lp as *const COPYDATASTRUCT) };
            if data.dwData != send_to::COPY_DATA_ID || data.lpData.is_null() {
                return Some(0);
            }
            let payload = unsafe {
                std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize)
            };
            Some(send_to::receive(w.hwnd, payload) as LRESULT)
        }
        send_to::WM_APP_SEND_TO => {
            send_to::show_pending();
            Some(0)
        }
        // The Setup of an update is ready and asks every instance to leave
        // (see `update`). The instance that installs exits as the user chose
        // in the Update Available window; the other profiles leave with their
        // transfers suspended, as at a logoff, and are not started again by
        // Setup. `exit` also ends a dialog that is open.
        m if m == MSG_EXIT_FOR_UPDATE.get() && m != 0 => {
            // The mutexes go first, so that Setup never sees them.
            crate::release_instance_mutexes();
            exit(w.hwnd, app::with(|a| a.update_cancel_transfers));
            Some(0)
        }
        // Logoff or shutdown is never blocked with a question: transfers
        // continue at the next start anyway.
        WM_QUERYENDSESSION => Some(1),
        WM_ENDSESSION => {
            if wp != 0 {
                // Tell the peers the transfers are suspended, not lost, while
                // the process is still allowed to run.
                tray::remove(w.hwnd);
                app::with(|a| a.net.shutdown(std::time::Duration::from_secs(2)));
            }
            Some(0)
        }
        // Explorer was restarted: the icon has to be added again.
        m if m == MSG_TASKBAR_CREATED.get() && m != 0 => {
            tray::add(w.hwnd);
            Some(0)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    /// Settings has its shortcut, which its menu item shows in every language.
    #[test]
    fn settings_have_a_shortcut() {
        assert!(accelerators().iter().any(|a| {
            a.cmd == ID_SETTINGS && a.key == VK_OEM_COMMA && a.fVirt == FVIRTKEY | FCONTROL
        }));
        for lang in Lang::ALL {
            let text = lang.strings().menu_settings;
            assert!(text.contains("\tCtrl+"), "{lang:?}: {text}");
        }
    }

    #[test]
    fn no_shortcut_repeats() {
        let table = accelerators();
        for (i, a) in table.iter().enumerate() {
            assert!(
                !table[..i]
                    .iter()
                    .any(|b| b.key == a.key && b.fVirt == a.fVirt),
                "the shortcut of command {} repeats",
                a.cmd
            );
        }
    }

    /// Donate comes right after License, Check for Updates right before About.
    #[test]
    fn help_menu_order() {
        let (bar, _) = build_menu();
        let help = unsafe { GetSubMenu(bar, 2) };
        let ids: Vec<u32> = (0..unsafe { GetMenuItemCount(help) })
            .map(|i| unsafe { GetMenuItemID(help, i) })
            .collect();
        unsafe { DestroyMenu(bar) };
        let separator = 0;
        assert_eq!(
            ids,
            [
                ID_VIEW_HELP,
                ID_HELP,
                separator,
                ID_LICENSE,
                ID_DONATE,
                separator,
                ID_CHECK_UPDATES,
                ID_ABOUT,
            ]
            .map(u32::from)
        );
    }
}
