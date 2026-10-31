//! Settings dialog.

use std::path::PathBuf;

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{BN_CLICKED, IDCANCEL},
};

use super::{
    error, get_text, info, set_text, shell,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    config::{CloseAction, ConnectionLostAction},
    i18n::{Lang, tr},
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

struct SettingsDialog {
    my_name: HWND,
    always: HWND,
    dir_label: HWND,
    dir: HWND,
    browse: HWND,
    auto: HWND,
    ask_after: HWND,
    autostart: HWND,
    minimized: HWND,
    tray: HWND,
    check_updates: HWND,
    close_running: HWND,
    connection_lost: HWND,
    language: HWND,
}

pub fn show(owner: HWND) {
    let handler = SettingsDialog {
        my_name: std::ptr::null_mut(),
        always: std::ptr::null_mut(),
        dir_label: std::ptr::null_mut(),
        dir: std::ptr::null_mut(),
        browse: std::ptr::null_mut(),
        auto: std::ptr::null_mut(),
        ask_after: std::ptr::null_mut(),
        autostart: std::ptr::null_mut(),
        minimized: std::ptr::null_mut(),
        tray: std::ptr::null_mut(),
        check_updates: std::ptr::null_mut(),
        close_running: std::ptr::null_mut(),
        connection_lost: std::ptr::null_mut(),
        language: std::ptr::null_mut(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(settings_title),
            client: (500, 674),
            center_on: owner,
            ..Default::default()
        },
        Box::new(handler),
    );
    window::run_modal(owner, hwnd);
}

impl SettingsDialog {
    fn update_enabled(&self) {
        let on = window::is_checked(self.always);
        window::enable(self.dir_label, on);
        window::enable(self.dir, on);
        window::enable(self.browse, on);
    }

    fn save(&self, w: &Window) -> bool {
        let my_name = get_text(self.my_name).trim().to_string();
        if my_name.is_empty() {
            error(w.hwnd, tr!(enter_my_name));
            return false;
        }
        let always = window::is_checked(self.always);
        let dir_text = get_text(self.dir).trim().to_string();
        let dir = (!dir_text.is_empty()).then(|| PathBuf::from(&dir_text));
        if always {
            match &dir {
                Some(d) if d.is_absolute() && std::fs::create_dir_all(d).is_ok() => {}
                _ => {
                    error(w.hwnd, tr!(invalid_save_folder));
                    return false;
                }
            }
        }
        let auto = window::is_checked(self.auto);
        let ask_after = window::is_checked(self.ask_after);
        let autostart = window::is_checked(self.autostart);
        let minimized = window::is_checked(self.minimized);
        let tray = window::is_checked(self.tray);
        let check_updates = window::is_checked(self.check_updates);
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
        app::update_cfg(|c| {
            c.my_name = my_name;
            c.always_save = always;
            c.always_save_dir = dir;
            c.auto_accept = auto;
            c.ask_after_receive = ask_after;
            c.start_with_windows = autostart;
            c.start_minimized = minimized;
            c.minimize_to_tray = tray;
            c.check_updates = check_updates;
            c.close_running_transfer = close_running;
            c.connection_lost = connection_lost;
            c.language = language;
        });
        shell::set_start_with_windows(autostart);
        if language_changed {
            info(w.hwnd, tr!(language_restart));
        }
        true
    }
}

impl Handler for SettingsDialog {
    fn on_create(&mut self, w: &Window) {
        let cfg = app::cfg();
        w.caption(tr!(my_name_label), (14, 12, 472, 18));
        self.my_name = w.edit(&cfg.my_name, ID_MY_NAME, (14, 32, 472, 24));

        let heading = w.label(tr!(receiving_heading), (14, 70, 472, 18));
        w.make_bold(heading);
        self.always = w.checkbox(
            tr!(always_save),
            ID_ALWAYS,
            cfg.always_save,
            (24, 94, 462, 20),
        );
        let dir = cfg
            .always_save_dir
            .clone()
            .unwrap_or_else(|| cfg.suggested_save_dir());
        // The label right before the field gives it its accessible name (the
        // check box above would otherwise not be used as the name).
        self.dir_label = w.caption(tr!(always_save_folder_label), (42, 118, 444, 18));
        self.dir = w.edit(&dir.display().to_string(), ID_DIR, (42, 140, 352, 24));
        self.browse = w.button(tr!(browse), ID_BROWSE, (400, 139, 86, 26));
        // Independent of the folder above: without it, files go to Downloads.
        self.auto = w.checkbox(
            tr!(auto_accept),
            ID_AUTO,
            cfg.auto_accept,
            (24, 174, 462, 20),
        );
        w.label(tr!(auto_accept_note), (42, 196, 444, 34));
        self.ask_after = w.checkbox(
            tr!(ask_after_receive),
            ID_ASK_AFTER,
            cfg.ask_after_receive,
            (24, 234, 462, 20),
        );

        let heading = w.label(tr!(general_heading), (14, 268, 472, 18));
        w.make_bold(heading);
        self.autostart = w.checkbox(
            tr!(start_with_windows),
            ID_AUTOSTART,
            cfg.start_with_windows,
            (24, 292, 462, 20),
        );
        self.minimized = w.checkbox(
            tr!(start_minimized),
            ID_MINIMIZED,
            cfg.start_minimized,
            (24, 316, 462, 20),
        );
        self.tray = w.checkbox(
            tr!(keep_in_tray),
            ID_TRAY,
            cfg.minimize_to_tray,
            (24, 340, 462, 20),
        );
        self.check_updates = w.checkbox(
            tr!(check_updates),
            ID_CHECK_UPDATES,
            cfg.check_updates,
            (24, 364, 462, 20),
        );
        w.caption(tr!(close_running_label), (24, 396, 462, 18));
        let actions = [tr!(close_ask), tr!(close_background), tr!(close_cancel)];
        let selected = CloseAction::ALL
            .iter()
            .position(|&a| a == cfg.close_running_transfer)
            .unwrap_or(0);
        self.close_running = w.combobox(&actions, selected, ID_CLOSE_RUNNING, (24, 416, 300, 24));
        w.caption(tr!(connection_lost_label), (24, 452, 462, 18));
        let actions = [tr!(lost_ask), tr!(lost_wait), tr!(lost_cancel)];
        let selected = ConnectionLostAction::ALL
            .iter()
            .position(|&a| a == cfg.connection_lost)
            .unwrap_or(0);
        self.connection_lost =
            w.combobox(&actions, selected, ID_CONNECTION_LOST, (24, 472, 300, 24));
        w.caption(tr!(language_label), (24, 508, 462, 18));
        let mut languages = vec![tr!(language_default)];
        languages.extend(Lang::ALL.iter().map(|l| l.native_name()));
        let selected = cfg
            .language
            .as_deref()
            .and_then(Lang::from_code)
            .and_then(|l| Lang::ALL.iter().position(|&x| x == l))
            .map_or(0, |i| i + 1);
        self.language = w.combobox(&languages, selected, ID_LANGUAGE, (24, 528, 300, 24));

        let my_id = app::with(|a| a.my_id.clone());
        w.label(tr!(my_id_label), (14, 568, 472, 18));
        let id_edit = w.edit(&my_id, 0, (14, 588, 380, 24));
        super::send(
            id_edit,
            windows_sys::Win32::UI::Controls::EM_SETREADONLY,
            1,
            0,
        );
        w.button(tr!(copy), ID_COPY, (400, 587, 86, 26));

        w.button(tr!(ok), ID_OK, (300, 630, 90, 30));
        w.button(tr!(cancel), IDCANCEL as u16, (396, 630, 90, 30));
        w.default_id.set(ID_OK);
        self.update_enabled();
        window::focus(self.my_name);
    }

    fn on_command(&mut self, w: &Window, id: u16, code: u16) -> bool {
        match id {
            ID_ALWAYS if code == BN_CLICKED as u16 => self.update_enabled(),
            ID_BROWSE => {
                let current = PathBuf::from(get_text(self.dir).trim());
                if let Some(dir) = shell::pick_folder(w.hwnd, tr!(pick_save_folder), Some(&current))
                {
                    set_text(self.dir, &dir.display().to_string());
                }
            }
            ID_COPY => {
                let my_id = app::with(|a| a.my_id.clone());
                shell::copy_to_clipboard(w.hwnd, &my_id);
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
}
