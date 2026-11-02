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
mod update;

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
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
            AllowSetForegroundWindow, DispatchMessageW, FindWindowW, GetMessageW,
            GetWindowThreadProcessId, MSG, PostMessageW, TranslateMessage,
        },
    },
};

use i18n::tr;
use ui::wide;

struct Args {
    minimized: bool,
    /// Separate settings and identity, e.g. to run two instances for testing.
    profile: Option<String>,
    /// Started by Setup right after an automatic update.
    updated: bool,
}

/// The mutexes that mark a running instance (see `main`). They are closed
/// right before an update's Setup takes over, so that Setup never sees them.
static INSTANCE_MUTEX: AtomicIsize = AtomicIsize::new(0);
static RUNNING_MUTEX: AtomicIsize = AtomicIsize::new(0);

/// Releases the "running" mutexes before this instance exits for an update.
pub fn release_instance_mutexes() {
    for mutex in [&INSTANCE_MUTEX, &RUNNING_MUTEX] {
        let handle = mutex.swap(0, Ordering::SeqCst) as HANDLE;
        if !handle.is_null() {
            unsafe { CloseHandle(handle) };
        }
    }
}

fn parse_args() -> Args {
    let mut args = Args {
        minimized: false,
        profile: None,
        updated: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minimized" => args.minimized = true,
            "--updated" => args.updated = true,
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

/// Brings the window of the instance that is already running to the front.
fn show_running_instance(title: &str) {
    let class = wide(ui::window::MAIN_CLASS);
    let title = wide(title);
    // The other instance may still be starting: its window is created after
    // the settings, the identity and the network, so look for it for a while.
    let mut existing = std::ptr::null_mut();
    for _ in 0..50 {
        existing = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
        if !existing.is_null() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if existing.is_null() {
        return;
    }
    unsafe {
        // Only the process the user just started may take the foreground;
        // hand that right to the running instance before asking it to show
        // its window, otherwise Windows merely flashes its taskbar button.
        let mut pid = 0;
        GetWindowThreadProcessId(existing, &mut pid);
        AllowSetForegroundWindow(pid);
        PostMessageW(existing, ui::main_window::show_message_id(), 0, 0);
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        // One message at a time: several tasks may fail the same way at once.
        static SHOWING: AtomicBool = AtomicBool::new(false);
        match std::thread::current().name() {
            // The UI thread: the program ends once it has unwound.
            Some("main") => ui::error(std::ptr::null_mut(), &tr!(panic_message, info = info)),
            // The network thread's own loop: nothing can be sent or
            // received any more, so the program ends after the message
            // (unfinished transfers go on at the next start).
            Some("network") => {
                ui::error(std::ptr::null_mut(), &tr!(panic_message, info = info));
                std::process::exit(1);
            }
            // Another thread ends (a transfer's task, which then fails, see
            // `net::guarded`), and the program goes on. The message gets a
            // thread of its own, so that this one unwinds at once instead of
            // staying stuck in the message box, holding up what waits for it.
            _ if !SHOWING.swap(true, Ordering::SeqCst) => {
                let text = tr!(panic_message_goes_on, info = info);
                std::thread::spawn(move || {
                    ui::error(std::ptr::null_mut(), &text);
                    SHOWING.store(false, Ordering::SeqCst);
                });
            }
            _ => {}
        }
    }));

    let args = parse_args();
    let profile = args.profile.as_deref();
    let title = ui::main_window::window_title(profile);

    // Only one instance per profile: a second start brings the first one to the front.
    // The mutex handle is deliberately kept open for the life of the process.
    let instance_mutex = wide(&format!("Local\\Dodavo-{}", profile.unwrap_or("default")));
    let already_running = unsafe {
        let handle = CreateMutexW(std::ptr::null(), 1, instance_mutex.as_ptr());
        INSTANCE_MUTEX.store(handle as isize, Ordering::SeqCst);
        GetLastError() == ERROR_ALREADY_EXISTS
    };
    if already_running {
        // A start in the background (autostart) leaves the running instance alone.
        if !args.minimized {
            show_running_instance(&title);
        }
        return;
    }
    // Held by every instance, whatever its profile, so the installer and
    // uninstaller can tell that Dodavo is running (AppMutex in installer\Dodavo.iss).
    let running_mutex = wide("Local\\DodavoRunning");
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, running_mutex.as_ptr()) };
    RUNNING_MUTEX.store(handle as isize, Ordering::SeqCst);
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
    let net = net::start(
        secret,
        data_dir.clone(),
        archive::temp_dir_for(profile),
        app::event_sink,
    );
    app::install(app::App {
        cfg,
        data_dir,
        my_id,
        net,
        profile: profile.map(str::to_string),
        main: std::ptr::null_mut(),
        online: None,
        transfers: Default::default(),
        offers: Default::default(),
        balloon_action: None,
        update_window: std::ptr::null_mut(),
        update_job: None,
        update_cancel_transfers: false,
    });

    ui::main_window::create(profile, start_hidden);
    ui::update::on_start(args.updated);

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
