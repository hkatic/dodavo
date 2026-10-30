//! Dodavo – send files and folders directly between
//! computers, identified by a permanent Contact ID instead of an IP address.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod archive;
mod config;
mod i18n;
mod identity;
mod net;
mod ui;

use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
        Threading::CreateMutexW,
    },
    UI::{
        Controls::{
            ICC_LISTVIEW_CLASSES, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
            InitCommonControlsEx,
        },
        WindowsAndMessaging::{
            DispatchMessageW, FindWindowW, GetMessageW, MSG, PostMessageW, TranslateMessage,
        },
    },
};

use i18n::tr;
use ui::wide;

struct Args {
    minimized: bool,
    /// Separate settings and identity, e.g. to run two instances for testing.
    profile: Option<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        minimized: false,
        profile: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minimized" => args.minimized = true,
            "--profile" => {
                args.profile = it
                    .next()
                    .map(|p| {
                        p.chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                            .collect()
                    })
                    .filter(|p: &String| !p.is_empty())
            }
            _ => {}
        }
    }
    args
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        ui::error(std::ptr::null_mut(), &tr!(panic_message, info = info));
    }));

    let args = parse_args();
    let profile = args.profile.as_deref();
    let title = ui::main_window::window_title(profile);

    // Only one instance per profile: a second start brings the first one to the front.
    // The mutex handle is deliberately kept open for the life of the process.
    let instance_mutex = wide(&format!("Local\\Dodavo-{}", profile.unwrap_or("default")));
    let already_running = unsafe {
        CreateMutexW(std::ptr::null(), 1, instance_mutex.as_ptr());
        GetLastError() == ERROR_ALREADY_EXISTS
    };
    if already_running {
        let class = wide(ui::window::MAIN_CLASS);
        let title = wide(&title);
        let existing = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
        if !existing.is_null() {
            unsafe { PostMessageW(existing, ui::main_window::show_message_id(), 0, 0) };
        }
        return;
    }
    // Held by every instance, whatever its profile, so the installer and
    // uninstaller can tell that Dodavo is running (AppMutex in installer\Dodavo.iss).
    let running_mutex = wide("Local\\DodavoRunning");
    unsafe { CreateMutexW(std::ptr::null(), 0, running_mutex.as_ptr()) };
    unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    let icc = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES,
    };
    unsafe { InitCommonControlsEx(&icc) };
    ui::theme::init();

    let data_dir = config::data_dir(profile);
    // The language comes from the settings, so the message about an unusable
    // settings file has to wait until the language is chosen.
    let (cfg, cfg_error) = match config::Config::load(&data_dir) {
        Ok(cfg) => (cfg, None),
        Err(e) => (config::Config::default(), Some(e)),
    };
    i18n::init(cfg.language.as_deref());
    if let Some(e) = cfg_error {
        let mut text = tr!(settings_unreadable, path = e.path.display(), error = e.error);
        if let Some(backup) = &e.backup {
            text.push_str("\n\n");
            text.push_str(&tr!(settings_backup, backup = backup.display()));
        }
        ui::error(std::ptr::null_mut(), &text);
    }
    let secret = match identity::load_or_create(&data_dir) {
        Ok(key) => key,
        Err(e) => {
            ui::error(
                std::ptr::null_mut(),
                &tr!(identity_failed, path = data_dir.display(), error = e),
            );
            return;
        }
    };
    let my_id = secret.public().to_string();
    // Keep the autostart entry pointing at the current location of the program.
    if cfg.start_with_windows {
        ui::shell::set_start_with_windows(true);
    }
    let start_hidden = args.minimized || cfg.start_minimized;
    let net = net::start(secret, app::event_sink);
    app::install(app::App {
        cfg,
        data_dir,
        my_id,
        net,
        main: std::ptr::null_mut(),
        transfers: Default::default(),
        offers: Default::default(),
        balloon_path: None,
    });

    ui::main_window::create(profile, start_hidden);

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        if !ui::main_window::translate(&msg) {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    // Tell peers that running transfers end, then leave: exiting the process
    // also stops the network thread.
    app::with(|a| a.net.shutdown(std::time::Duration::from_secs(3)));
    std::process::exit(0);
}
