//! Settings dialog.

use std::path::{Path, PathBuf};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT},
    UI::{
        Controls::{NMHDR, TCN_SELCHANGE},
        WindowsAndMessaging::{BN_CLICKED, IDCANCEL},
    },
};

use super::{
    error, get_text, info, set_text, shell,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    config::{CloseAction, ConnectionLostAction, OverLimit, ReceivedFolder},
    i18n::{Lang, tr},
    limit::{self, Unit},
};

const ID_OK: u16 = 1;
const ID_MY_NAME: u16 = 10;
const ID_ALWAYS: u16 = 11;
const ID_DIR: u16 = 12;
const ID_BROWSE: u16 = 13;
const ID_AUTO: u16 = 14;
const ID_AUTOSTART: u16 = 15;
const ID_TRAY: u16 = 16;
const ID_COPY: u16 = 17;
const ID_LANGUAGE: u16 = 18;
const ID_ASK_AFTER: u16 = 19;
const ID_MINIMIZED: u16 = 20;
const ID_CLOSE_RUNNING: u16 = 21;
const ID_CONNECTION_LOST: u16 = 22;
const ID_CHECK_UPDATES: u16 = 23;
const ID_SHOW_AFTER_SEND: u16 = 24;
const ID_RECEIVED_FOLDER: u16 = 25;
const ID_EXPLORER_MENU: u16 = 26;
const ID_BLOCKED: u16 = 27;
const ID_CHANGE_ID: u16 = 28;
const ID_LIMIT: u16 = 29;
const ID_LIMIT_SIZE: u16 = 30;
const ID_LIMIT_UNIT: u16 = 31;
const ID_OVER_LIMIT: u16 = 32;
const ID_TEMP_DIR: u16 = 33;
const ID_TEMP_BROWSE: u16 = 34;
const ID_TABS: u16 = 35;
const ID_CLOSE_WHEN_ENDED: u16 = 36;
const ID_WARN_FAT_TEMP: u16 = 37;

/// The window (96-DPI units): a margin around a page of settings.
const MARGIN: i32 = 14;
const PAGE_W: i32 = 466;
const WIDTH: i32 = MARGIN + PAGE_W + MARGIN;

/// The pages, in the order of their tabs.
const PAGE_GENERAL: usize = 0;
const PAGE_TRANSFERS: usize = 1;
const PAGE_RECEIVING: usize = 2;

struct SettingsDialog {
    /// The row of tabs, and the controls of each tab's page.
    tabs: HWND,
    pages: Vec<Vec<HWND>>,
    my_name: HWND,
    always: HWND,
    dir_label: HWND,
    dir: HWND,
    browse: HWND,
    auto: HWND,
    received_folder: HWND,
    ask_after: HWND,
    /// The general limit on the size of an item: whether there is one, its
    /// size with the label and the unit, and what happens to a larger item.
    limit: HWND,
    limit_label: HWND,
    limit_size: HWND,
    limit_unit: HWND,
    over_limit: HWND,
    show_after_send: HWND,
    /// Whether a progress window closes as soon as its transfer ends.
    close_when_ended: HWND,
    autostart: HWND,
    minimized: HWND,
    tray: HWND,
    check_updates: HWND,
    explorer_menu: HWND,
    /// Whether this copy of Dodavo can have the entries that box is about.
    explorer_menu_available: bool,
    close_running: HWND,
    connection_lost: HWND,
    language: HWND,
    /// The folder for temporary files, as the user chose it or Windows
    /// gives it.
    temp_dir: HWND,
    /// Whether to tell of a temporary archive that is too large for the
    /// drive of that folder.
    warn_fat_temp: HWND,
    /// The read-only field with this installation's Contact ID.
    my_id: HWND,
}

pub fn show(owner: HWND) {
    let handler = SettingsDialog {
        tabs: std::ptr::null_mut(),
        pages: Vec::new(),
        my_name: std::ptr::null_mut(),
        always: std::ptr::null_mut(),
        dir_label: std::ptr::null_mut(),
        dir: std::ptr::null_mut(),
        browse: std::ptr::null_mut(),
        auto: std::ptr::null_mut(),
        received_folder: std::ptr::null_mut(),
        ask_after: std::ptr::null_mut(),
        limit: std::ptr::null_mut(),
        limit_label: std::ptr::null_mut(),
        limit_size: std::ptr::null_mut(),
        limit_unit: std::ptr::null_mut(),
        over_limit: std::ptr::null_mut(),
        show_after_send: std::ptr::null_mut(),
        close_when_ended: std::ptr::null_mut(),
        autostart: std::ptr::null_mut(),
        minimized: std::ptr::null_mut(),
        tray: std::ptr::null_mut(),
        check_updates: std::ptr::null_mut(),
        explorer_menu: std::ptr::null_mut(),
        explorer_menu_available: false,
        close_running: std::ptr::null_mut(),
        connection_lost: std::ptr::null_mut(),
        language: std::ptr::null_mut(),
        temp_dir: std::ptr::null_mut(),
        warn_fat_temp: std::ptr::null_mut(),
        my_id: std::ptr::null_mut(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(settings_title),
            // Sized to the content in `on_create`.
            client: (WIDTH, 436),
            center_on: owner,
            ..Default::default()
        },
        Box::new(handler),
    );
    window::run_modal(owner, hwnd);
}

/// A path without what only its writing adds (a separator at its end).
fn tidy(path: &Path) -> PathBuf {
    path.components().collect()
}

/// The folder that Windows gives for temporary files, in which Dodavo keeps
/// its own (see `platform::temp_base`).
pub fn system_temp() -> PathBuf {
    tidy(&std::env::temp_dir())
}

/// The user's choice of a folder for temporary files, as written in `text`:
/// none for an empty text or the folder that Windows gives, and an error for
/// a folder that cannot be used.
pub fn temp_dir_choice(text: &str) -> Result<Option<PathBuf>, ()> {
    let text = text.trim();
    let dir = (!text.is_empty())
        .then(|| tidy(Path::new(text)))
        .filter(|dir| !same_folder(dir, &system_temp()));
    match &dir {
        Some(dir) if !(dir.is_absolute() && std::fs::create_dir_all(dir).is_ok()) => Err(()),
        _ => Ok(dir),
    }
}

/// Whether two paths name the same folder, as far as their writing tells.
fn same_folder(a: &Path, b: &Path) -> bool {
    tidy(a)
        .as_os_str()
        .eq_ignore_ascii_case(tidy(b).as_os_str())
}

impl SettingsDialog {
    /// Shows the controls of one page and hides those of the others. The
    /// focus goes to the tabs if it was on a control that is hidden now, and
    /// a screen reader then says which tab is selected.
    fn show_page(&self, index: usize) {
        window::select_tab(self.tabs, index);
        let focus = window::focused();
        for (i, page) in self.pages.iter().enumerate() {
            for &ctl in page {
                window::show(ctl, i == index);
            }
            if i != index && page.contains(&focus) {
                window::focus(self.tabs);
            }
        }
    }

    /// Reports what cannot be saved, on the page of the field it is about,
    /// and leaves the focus in that field.
    fn refuse(&self, w: &Window, page: usize, field: HWND, text: &str) {
        self.show_page(page);
        error(w.hwnd, text);
        window::focus(field);
    }

    fn update_enabled(&self) {
        let on = window::is_checked(self.always);
        window::enable(self.dir_label, on);
        window::enable(self.dir, on);
        window::enable(self.browse, on);
        // Without that folder Dodavo asks where to save, so nothing can start
        // by itself. The setting keeps its value for when the folder is back.
        window::enable(self.auto, on);
        // What happens to a larger item stays: it also holds for the limits
        // that recipients have of their own.
        let limited = window::is_checked(self.limit);
        window::enable(self.limit_label, limited);
        window::enable(self.limit_size, limited);
        window::enable(self.limit_unit, limited);
        // A window that closes at the end shows no message and asks nothing.
        // Both settings keep their values for when it stays open again.
        let stays = !window::is_checked(self.close_when_ended);
        window::enable(self.show_after_send, stays);
        window::enable(self.ask_after, stays);
    }

    fn save(&self, w: &Window) -> bool {
        let my_name = get_text(self.my_name).trim().to_string();
        if my_name.is_empty() {
            self.refuse(w, PAGE_GENERAL, self.my_name, tr!(enter_my_name));
            return false;
        }
        let always = window::is_checked(self.always);
        let dir_text = get_text(self.dir).trim().to_string();
        let dir = (!dir_text.is_empty()).then(|| PathBuf::from(&dir_text));
        if always {
            match &dir {
                Some(d) if d.is_absolute() && std::fs::create_dir_all(d).is_ok() => {}
                _ => {
                    self.refuse(w, PAGE_RECEIVING, self.dir, tr!(invalid_save_folder));
                    return false;
                }
            }
        }
        let auto = window::is_checked(self.auto);
        let received_folder = window::combo_selection(self.received_folder)
            .and_then(|i| ReceivedFolder::ALL.get(i).copied())
            .unwrap_or_default();
        let ask_after = window::is_checked(self.ask_after);
        let limited = window::is_checked(self.limit);
        let unit = window::combo_selection(self.limit_unit)
            .and_then(|i| Unit::ALL.get(i).copied())
            .unwrap_or(Unit::Mb);
        // Without the box, a size that cannot be used is left as it was.
        let size_limit = match limit::from_input(&get_text(self.limit_size), unit) {
            Ok(bytes) => Some(bytes),
            Err(text) if limited => {
                self.refuse(w, PAGE_RECEIVING, self.limit_size, &text);
                return false;
            }
            Err(_) => None,
        };
        let over_limit = window::combo_selection(self.over_limit)
            .and_then(|i| OverLimit::ALL.get(i).copied())
            .unwrap_or_default();
        let show_after_send = window::is_checked(self.show_after_send);
        let close_when_ended = window::is_checked(self.close_when_ended);
        let autostart = window::is_checked(self.autostart);
        let minimized = window::is_checked(self.minimized);
        let tray = window::is_checked(self.tray);
        let check_updates = window::is_checked(self.check_updates);
        // A box that cannot be used says nothing about the setting.
        let explorer_menu = self
            .explorer_menu_available
            .then(|| window::is_checked(self.explorer_menu));
        let close_running = window::combo_selection(self.close_running)
            .and_then(|i| CloseAction::ALL.get(i).copied())
            .unwrap_or_default();
        let connection_lost = window::combo_selection(self.connection_lost)
            .and_then(|i| ConnectionLostAction::ALL.get(i).copied())
            .unwrap_or_default();
        // Entry 0 is "Windows default", followed by `Lang::ALL`.
        let language = window::combo_selection(self.language)
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| Lang::ALL.get(i))
            .map(|l| l.code().to_string());
        let language_changed = language != app::cfg().language;
        // Empty, or the folder that Windows gives: no choice of the user's.
        let Ok(temp_dir) = temp_dir_choice(&get_text(self.temp_dir)) else {
            self.refuse(w, PAGE_TRANSFERS, self.temp_dir, tr!(invalid_temp_folder));
            return false;
        };
        let warn_fat_temp = window::is_checked(self.warn_fat_temp);
        // Unset, it follows Downloads to wherever that is moved.
        let dir = dir.filter(|d| *d != crate::platform::downloads_dir());
        app::update_cfg(|c| {
            c.my_name = my_name;
            c.always_save = always;
            c.always_save_dir = dir;
            c.auto_accept = auto;
            c.received_folder = received_folder;
            c.ask_after_receive = ask_after;
            c.limit_size = limited;
            if let Some(bytes) = size_limit {
                c.size_limit = bytes;
            }
            c.over_limit = over_limit;
            c.show_after_send = show_after_send;
            c.close_when_ended = close_when_ended;
            c.start_with_windows = autostart;
            c.start_minimized = minimized;
            c.minimize_to_tray = tray;
            c.check_updates = check_updates;
            if let Some(on) = explorer_menu {
                c.explorer_menu = on;
            }
            c.close_running_transfer = close_running;
            c.connection_lost = connection_lost;
            c.language = language;
            c.set_temp_dir(temp_dir);
            c.warn_fat_temp = warn_fat_temp;
        });
        shell::set_start_with_windows(autostart);
        if let Some(on) = explorer_menu {
            shell::set_explorer_menu(on);
        }
        if language_changed {
            info(w.hwnd, tr!(language_restart));
        }
        true
    }
}

impl Handler for SettingsDialog {
    fn on_create(&mut self, w: &Window) {
        let cfg = app::cfg();
        // Tab goes from the tabs down the page that shows, then to OK and
        // Cancel. Every page is made here, one after the other.
        let (x, width) = (MARGIN, PAGE_W);
        // Fields that belong to a check box are indented.
        let (sub_x, sub_w) = (x + 18, width - 18);
        self.tabs = w.tabs(
            &[
                tr!(settings_tab_general),
                tr!(settings_tab_transfers),
                tr!(settings_tab_receiving),
            ],
            ID_TABS,
            (x, 10, width, 26),
        );
        let tabs_h = w.tabs_height(self.tabs);
        w.place(self.tabs, (x, 10, width, tabs_h));
        let top = 10 + tabs_h + 14;

        // General: who the user is, and how Dodavo starts and looks.
        let start = w.control_count();
        let mut y = top;
        w.caption(tr!(my_name_label), (x, y, width, 18));
        self.my_name = w.edit(&cfg.my_name, ID_MY_NAME, (x, y + 20, width, 24));
        y += 56;
        let my_id = app::with(|a| a.my_id.clone());
        w.label(tr!(my_id_label), (x, y, width, 18));
        self.my_id = w.edit(&my_id, 0, (x, y + 20, width - 92, 24));
        super::send(
            self.my_id,
            windows_sys::Win32::UI::Controls::EM_SETREADONLY,
            1,
            0,
        );
        w.button(tr!(copy), ID_COPY, (x + width - 86, y + 19, 86, 26));
        // Applies at once, like blocking: it is not one of the settings
        // that OK saves.
        let change_w = w.button_width(tr!(change_id_button), 86);
        w.button(
            tr!(change_id_button),
            ID_CHANGE_ID,
            (x, y + 52, change_w, 26),
        );
        y += 90;
        w.caption(tr!(language_label), (x, y, width, 18));
        let mut languages = vec![tr!(language_default)];
        languages.extend(Lang::ALL.iter().map(|l| l.native_name()));
        let selected = cfg
            .language
            .as_deref()
            .and_then(Lang::from_code)
            .and_then(|l| Lang::ALL.iter().position(|&x| x == l))
            .map_or(0, |i| i + 1);
        self.language = w.combobox(&languages, selected, ID_LANGUAGE, (x, y + 20, 300, 24));
        y += 58;
        self.autostart = w.checkbox(
            tr!(start_with_windows),
            ID_AUTOSTART,
            cfg.start_with_windows,
            (x, y, width, 20),
        );
        y += 24;
        self.minimized = w.checkbox(
            tr!(start_minimized),
            ID_MINIMIZED,
            cfg.start_minimized,
            (x, y, width, 20),
        );
        y += 24;
        self.tray = w.checkbox(
            tr!(keep_in_tray),
            ID_TRAY,
            cfg.minimize_to_tray,
            (x, y, width, 20),
        );
        y += 24;
        self.check_updates = w.checkbox(
            tr!(check_updates),
            ID_CHECK_UPDATES,
            cfg.check_updates,
            (x, y, width, 20),
        );
        y += 24;
        self.explorer_menu_available =
            shell::explorer_menu_available(app::with(|a| a.profile.clone()).as_deref());
        self.explorer_menu = w.checkbox(
            tr!(explorer_menu),
            ID_EXPLORER_MENU,
            self.explorer_menu_available && cfg.explorer_menu,
            (x, y, width, 20),
        );
        window::enable(self.explorer_menu, self.explorer_menu_available);
        let mut bottom = y + 20;
        self.pages.push(w.controls_from(start));

        // Transfers: what holds for sending and for receiving.
        let start = w.control_count();
        let mut y = top;
        w.caption(tr!(close_running_label), (x, y, width, 18));
        let actions = [tr!(close_ask), tr!(close_background), tr!(close_cancel)];
        let selected = CloseAction::ALL
            .iter()
            .position(|&a| a == cfg.close_running_transfer)
            .unwrap_or(0);
        self.close_running =
            w.combobox(&actions, selected, ID_CLOSE_RUNNING, (x, y + 20, 300, 24));
        y += 56;
        w.caption(tr!(connection_lost_label), (x, y, width, 18));
        let actions = [tr!(lost_ask), tr!(lost_wait), tr!(lost_cancel)];
        let selected = ConnectionLostAction::ALL
            .iter()
            .position(|&a| a == cfg.connection_lost)
            .unwrap_or(0);
        self.connection_lost = w.combobox(
            &actions,
            selected,
            ID_CONNECTION_LOST,
            (x, y + 20, 300, 24),
        );
        y += 58;
        self.close_when_ended = w.checkbox(
            tr!(close_when_ended),
            ID_CLOSE_WHEN_ENDED,
            cfg.close_when_ended,
            (x, y, width, 20),
        );
        y += 24;
        self.show_after_send = w.checkbox(
            tr!(show_after_send),
            ID_SHOW_AFTER_SEND,
            cfg.show_after_send,
            (x, y, width, 20),
        );
        y += 32;
        // The folder in which Dodavo keeps its own folder of temporary
        // files: the user's choice, or what Windows gives.
        w.caption(tr!(temp_folder_label), (x, y, width, 18));
        let temp = cfg.temp_dir.clone().unwrap_or_else(system_temp);
        self.temp_dir = w.edit(
            &temp.display().to_string(),
            ID_TEMP_DIR,
            (x, y + 20, width - 92, 24),
        );
        w.button(
            tr!(browse),
            ID_TEMP_BROWSE,
            (x + width - 86, y + 19, 86, 26),
        );
        y += 54;
        self.warn_fat_temp = w.checkbox(
            tr!(warn_fat_temp),
            ID_WARN_FAT_TEMP,
            cfg.warn_fat_temp,
            (x, y, width, 20),
        );
        y += 20;
        bottom = bottom.max(y);
        self.pages.push(w.controls_from(start));

        // Receiving: where items go, what starts by itself and who may send.
        let start = w.control_count();
        let mut y = top;
        self.always = w.checkbox(
            tr!(always_save),
            ID_ALWAYS,
            cfg.always_save,
            (x, y, width, 20),
        );
        y += 24;
        let dir = cfg.always_save_folder();
        // The label right before the field gives it its accessible name (the
        // check box above would otherwise not be used as the name).
        self.dir_label = w.caption(tr!(always_save_folder_label), (sub_x, y, sub_w, 18));
        self.dir = w.edit(
            &dir.display().to_string(),
            ID_DIR,
            (sub_x, y + 22, sub_w - 92, 24),
        );
        self.browse = w.button(tr!(browse), ID_BROWSE, (x + width - 86, y + 21, 86, 26));
        y += 56;
        self.auto = w.checkbox(
            tr!(auto_accept),
            ID_AUTO,
            cfg.auto_accept,
            (x, y, width, 20),
        );
        y += 22;
        let note = tr!(auto_accept_note);
        let note_h = w.text_height(note, sub_w).max(18);
        w.label(note, (sub_x, y, sub_w, note_h));
        y += note_h + 8;
        w.caption(tr!(received_folder_label), (x, y, width, 18));
        let choices = [
            tr!(received_extract),
            tr!(received_keep_zip),
            tr!(received_both),
        ];
        let selected = ReceivedFolder::ALL
            .iter()
            .position(|&f| f == cfg.received_folder)
            .unwrap_or(0);
        // As wide as the longest choice needs, with the arrow.
        let combo_w = choices
            .iter()
            .map(|c| w.text_width(c) + 40)
            .max()
            .unwrap_or(0)
            .clamp(300, width);
        self.received_folder = w.combobox(
            &choices,
            selected,
            ID_RECEIVED_FOLDER,
            (x, y + 20, combo_w, 24),
        );
        y += 56;
        self.ask_after = w.checkbox(
            tr!(ask_after_receive),
            ID_ASK_AFTER,
            cfg.ask_after_receive,
            (x, y, width, 20),
        );
        y += 30;
        self.limit = w.checkbox(tr!(limit_size), ID_LIMIT, cfg.limit_size, (x, y, width, 20));
        y += 26;
        // The size and its unit follow their label on one line.
        let label = tr!(limit_size_label);
        let label_w = (w.text_width(label) + 8).min(sub_w - 190);
        self.limit_label = w.caption(label, (sub_x, y + 3, label_w, 18));
        let (number, unit) = limit::to_input(cfg.size_limit);
        let size_x = sub_x + label_w + 6;
        self.limit_size =
            w.number_edit(&number.to_string(), 7, ID_LIMIT_SIZE, (size_x, y, 84, 24));
        let units = Unit::ALL.map(Unit::name);
        let unit = Unit::ALL.iter().position(|&u| u == unit).unwrap_or(0);
        self.limit_unit = w.combobox(&units, unit, ID_LIMIT_UNIT, (size_x + 90, y, 84, 24));
        // It follows a field, not a label, so it gets its name.
        w.set_accessible_name(self.limit_unit, tr!(limit_unit_name));
        y += 34;
        w.caption(tr!(over_limit_label), (x, y, width, 18));
        let actions = [tr!(over_ask), tr!(over_decline), tr!(over_decline_tell)];
        let selected = OverLimit::ALL
            .iter()
            .position(|&a| a == cfg.over_limit)
            .unwrap_or(0);
        let combo_w = actions
            .iter()
            .map(|a| w.text_width(a) + 40)
            .max()
            .unwrap_or(0)
            .clamp(300, width);
        self.over_limit = w.combobox(
            &actions,
            selected,
            ID_OVER_LIMIT,
            (x, y + 20, combo_w, 24),
        );
        y += 56;
        let blocked_w = w.button_width(tr!(blocked_senders_button), 160);
        w.button(
            tr!(blocked_senders_button),
            ID_BLOCKED,
            (x, y, blocked_w, 26),
        );
        bottom = bottom.max(y + 26);
        self.pages.push(w.controls_from(start));

        let buttons_y = bottom + 22;
        w.button(tr!(ok), ID_OK, (WIDTH - MARGIN - 186, buttons_y, 90, 30));
        w.button(
            tr!(cancel),
            IDCANCEL as u16,
            (WIDTH - MARGIN - 90, buttons_y, 90, 30),
        );
        w.set_client_size(WIDTH, buttons_y + 30 + MARGIN);
        w.default_id.set(ID_OK);
        self.update_enabled();
        self.show_page(PAGE_GENERAL);
        window::focus(self.my_name);
    }

    fn on_command(&mut self, w: &Window, id: u16, code: u16) -> bool {
        match id {
            ID_ALWAYS | ID_LIMIT | ID_CLOSE_WHEN_ENDED if code == BN_CLICKED as u16 => self.update_enabled(),
            ID_BROWSE => {
                let current = PathBuf::from(get_text(self.dir).trim());
                if let Some(dir) = shell::pick_folder(w.hwnd, tr!(pick_save_folder), Some(&current))
                {
                    set_text(self.dir, &dir.display().to_string());
                }
            }
            ID_TEMP_BROWSE => {
                let current = PathBuf::from(get_text(self.temp_dir).trim());
                if let Some(dir) = shell::pick_folder(w.hwnd, tr!(pick_temp_folder), Some(&current))
                {
                    set_text(self.temp_dir, &dir.display().to_string());
                }
            }
            ID_BLOCKED => super::blocked::show(w.hwnd),
            ID_COPY => {
                let my_id = app::with(|a| a.my_id.clone());
                shell::copy_to_clipboard(w.hwnd, &my_id);
            }
            ID_CHANGE_ID => {
                if app::change_contact_id(w.hwnd) {
                    set_text(self.my_id, &app::with(|a| a.my_id.clone()));
                    window::focus(self.my_id);
                }
            }
            ID_OK => {
                if self.save(w) {
                    w.close();
                }
            }
            _ => return false,
        }
        true
    }

    fn on_notify(&mut self, _w: &Window, hdr: &NMHDR, _lparam: LPARAM) -> Option<LRESULT> {
        if hdr.hwndFrom == self.tabs && hdr.code == TCN_SELCHANGE {
            self.show_page(window::tab_selection(self.tabs));
        }
        None
    }
}
