//! Dodavo – send files and folders directly between
//! computers, identified by a permanent Contact ID instead of an IP address.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
// The Win32 user interface reaches the library through `crate::net` and so on.
pub(crate) use dodavo_core::{
    archive, config, format, i18n, identity, net, platform, space, update, view,
};

#[cfg(windows)]
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicIsize, Ordering},
    time::{Duration, Instant},
};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
        DataExchange::COPYDATASTRUCT,
        Threading::CreateMutexW,
    },
    UI::{
        Controls::{
            ICC_LISTVIEW_CLASSES, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
            InitCommonControlsEx,
        },
        WindowsAndMessaging::{
            AllowSetForegroundWindow, DispatchMessageW, FindWindowW, GetMessageW,
            GetWindowThreadProcessId, MSG, PostMessageW, SMTO_NORMAL, SendMessageTimeoutW,
            TranslateMessage, WM_COPYDATA,
        },
    },
};

// The `windows` crates bind CoTaskMemFree to combase.dll, which Windows 7 does
// not have, and ole32.dll exports it on every Windows version. Whichever comes
// first on the linker's command line wins for the whole program, also for the
// calls in those crates, and only the libraries of the program itself come
// before them. `installer\check-imports.ps1 -Profile win7` fails without this.
#[cfg(windows)]
#[link(name = "ole32")]
unsafe extern "system" {}

#[cfg(windows)]
use i18n::tr;
#[cfg(windows)]
use ui::wide;

/// How long a start with items looks for the instance that takes them, which
/// may be starting itself: its window comes after the settings, the identity
/// and the network, and a message about those can hold it up.
#[cfg(windows)]
const FORWARD_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(windows)]
#[derive(Debug, Default, PartialEq)]
struct Args {
    minimized: bool,
    /// Separate settings and identity, e.g. to run two instances for testing.
    profile: Option<String>,
    /// Started by Setup right after an automatic update.
    updated: bool,
    /// What to send, from File Explorer's menus (everything after `--send`).
    send: Vec<PathBuf>,
}

#[cfg(windows)]
/// The mutexes that mark a running instance (see `main`). They are closed
/// right before an update's Setup takes over, so that Setup never sees them.
static INSTANCE_MUTEX: AtomicIsize = AtomicIsize::new(0);
#[cfg(windows)]
static RUNNING_MUTEX: AtomicIsize = AtomicIsize::new(0);

#[cfg(windows)]
/// Releases the "running" mutexes before this instance exits for an update.
pub fn release_instance_mutexes() {
    for mutex in [&INSTANCE_MUTEX, &RUNNING_MUTEX] {
        let handle = mutex.swap(0, Ordering::SeqCst) as HANDLE;
        if !handle.is_null() {
            unsafe { CloseHandle(handle) };
        }
    }
}

/// `OsString`, because File Explorer passes names on as they are, and Windows
/// allows names that are not valid Unicode.
#[cfg(windows)]
fn parse_args(mut it: impl Iterator<Item = OsString>) -> Args {
    let mut args = Args::default();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--minimized") => args.minimized = true,
            Some("--updated") => args.updated = true,
            Some("--profile") => {
                args.profile = it
                    .next()
                    .map(|p| {
                        p.to_string_lossy()
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                            .collect()
                    })
                    .filter(|p: &String| !p.is_empty())
            }
            Some("--send") => args.send = it.by_ref().map(PathBuf::from).collect(),
            _ => {}
        }
    }
    args
}

/// Hands `paths` to the instance that is running or still starting, and has
/// it offer them for sending. Returns `false` if it never took them.
#[cfg(windows)]
fn forward_to_running_instance(title: &str, paths: &[PathBuf]) -> bool {
    let class = wide(ui::window::MAIN_CLASS);
    let title = wide(title);
    let payload = ui::send_to::encode(paths);
    let data = COPYDATASTRUCT {
        dwData: ui::send_to::COPY_DATA_ID,
        cbData: payload.len() as u32,
        lpData: payload.as_ptr() as *mut _,
    };
    let deadline = Instant::now() + FORWARD_TIMEOUT;
    loop {
        let window = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
        if !window.is_null() {
            let mut answer = 0;
            let sent = unsafe {
                // See `show_running_instance`.
                let mut pid = 0;
                GetWindowThreadProcessId(window, &mut pid);
                AllowSetForegroundWindow(pid);
                // A window that does not take messages yet is starting, so
                // this waits instead of giving up on one that seems hung.
                // Should a message arrive late, the window drops the repeat.
                SendMessageTimeoutW(
                    window,
                    WM_COPYDATA,
                    0,
                    &data as *const _ as isize,
                    SMTO_NORMAL,
                    2000,
                    &mut answer,
                )
            };
            if sent != 0 && answer != 0 {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Says that the items could not be handed over. The context menu starts a
/// process per item, and one message is enough for all of them, so the first
/// keeps this mutex until its message is closed.
#[cfg(windows)]
fn report_forward_failure(profile: Option<&str>) {
    let name = wide("Local\\DodavoForwardFailed");
    let first = unsafe {
        CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        GetLastError() != ERROR_ALREADY_EXISTS
    };
    if first {
        i18n::init(config::read_language(&config::data_dir(profile)).as_deref());
        ui::error(std::ptr::null_mut(), tr!(send_to_failed));
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
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

    let args = parse_args(std::env::args_os().skip(1));
    let profile = args.profile.as_deref();
    let title = ui::main_window::window_title(profile);
    // Relative paths would mean something else to the instance that takes them.
    let send: Vec<PathBuf> = args
        .send
        .iter()
        .map(|path| std::path::absolute(path).unwrap_or_else(|_| path.clone()))
        .collect();

    // Only one instance per profile: a second start brings the first one to the front.
    // The mutex handle is deliberately kept open for the life of the process.
    let instance_mutex = wide(&format!("Local\\Dodavo-{}", profile.unwrap_or("default")));
    let already_running = unsafe {
        let handle = CreateMutexW(std::ptr::null(), 1, instance_mutex.as_ptr());
        INSTANCE_MUTEX.store(handle as isize, Ordering::SeqCst);
        GetLastError() == ERROR_ALREADY_EXISTS
    };
    if already_running {
        // While a handle is open the mutex stays, even after the instance has
        // gone, and a new start would take itself for a second one. The wait
        // below is long enough for that, for example during an update.
        release_instance_mutexes();
        if !send.is_empty() {
            if !forward_to_running_instance(&title, &send) {
                report_forward_failure(profile);
            }
        } else if !args.minimized {
            // A start in the background (autostart) leaves the running instance alone.
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
    // The same for File Explorer's menus, whose text also follows the language.
    if ui::shell::explorer_menu_available(profile) {
        ui::shell::set_explorer_menu(cfg.explorer_menu);
    }
    // Started only to send something, Dodavo stays out of the way, unless
    // nothing but its main window would then show that it is running.
    let start_hidden =
        args.minimized || cfg.start_minimized || (!send.is_empty() && cfg.minimize_to_tray);
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
    if !send.is_empty() {
        ui::send_to::add(send);
    }

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

/// The Mac user interface is a program of its own, in `macos/`.
#[cfg(not(windows))]
fn main() {
    eprintln!("This program is the Windows user interface. See macos/ for the Mac one.");
    std::process::exit(1);
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::ffi::OsStringExt;

    use super::*;

    fn parse(args: &[&str]) -> Args {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn options_are_read_as_before() {
        assert_eq!(parse(&[]), Args::default());
        let args = parse(&["--minimized", "--updated", "--profile", "al-ice_2!", "other"]);
        assert!(args.minimized);
        assert!(args.updated);
        assert_eq!(args.profile.as_deref(), Some("al-ice_2"));
        assert!(args.send.is_empty());
        assert_eq!(parse(&["--profile", "!!!"]).profile, None);
        assert_eq!(parse(&["--profile"]).profile, None);
    }

    /// What File Explorer starts for its two menus. The arguments end with
    /// the paths, so a file named like an option is a file.
    #[test]
    fn everything_after_send_is_a_path() {
        let args = parse(&["--profile", "a", "--send", r"C:\one.txt", "--minimized", r"D:\two"]);
        assert_eq!(args.profile.as_deref(), Some("a"));
        assert!(!args.minimized);
        assert_eq!(
            args.send,
            [
                PathBuf::from(r"C:\one.txt"),
                PathBuf::from("--minimized"),
                PathBuf::from(r"D:\two"),
            ]
        );
    }

    /// The shortcut in the Send to folder, started by itself.
    #[test]
    fn send_without_paths_is_a_plain_start() {
        assert_eq!(parse(&["--send"]), Args::default());
    }

    #[test]
    fn a_name_that_is_not_unicode_is_kept() {
        let mut units: Vec<u16> = r"C:\Temp\".encode_utf16().collect();
        units.push(0xD800);
        let name = OsString::from_wide(&units);
        let args = parse_args([OsString::from("--send"), name.clone()].into_iter());
        assert_eq!(args.send, [PathBuf::from(name)]);
    }
}
