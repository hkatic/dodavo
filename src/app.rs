//! Application state shared by the UI (UI thread only) and the bridge that
//! carries network events onto the UI thread.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicIsize, Ordering},
    },
};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{IsWindowVisible, KillTimer, PostMessageW, SetTimer, WM_APP},
};

use crate::{
    config::{Config, Contact},
    i18n::tr,
    identity,
    net::{self, Command, Event, Kind, NetHandle, Outcome, Role},
    ui::{
        self, incoming, progress,
        tray::{self, BalloonAction},
    },
};

pub const WM_APP_NET: u32 = WM_APP + 1;

pub struct App {
    pub cfg: Config,
    pub data_dir: PathBuf,
    pub my_id: String,
    pub net: NetHandle,
    pub main: HWND,
    /// Progress windows by transfer id.
    pub transfers: HashMap<u64, HWND>,
    /// Open "incoming transfer" windows by transfer id.
    pub offers: HashMap<u64, HWND>,
    /// What selecting the last notification does.
    pub balloon_action: Option<BalloonAction>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

static EVENTS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn install(app: App) {
    APP.with(|a| *a.borrow_mut() = Some(app));
}

pub fn set_main_window(hwnd: HWND) {
    MAIN_HWND.store(hwnd as isize, Ordering::SeqCst);
    with(|a| a.main = hwnd);
    // The network thread starts before the window exists; events it emitted
    // meanwhile (such as the first "online" status) are waiting in the queue.
    unsafe { PostMessageW(hwnd, WM_APP_NET, 0, 0) };
}

/// Accesses the application state. Do not create windows or show message
/// boxes inside the closure (they run nested message loops).
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.with(|a| f(a.borrow_mut().as_mut().expect("app not initialised")))
}

pub fn cfg() -> Config {
    with(|a| a.cfg.clone())
}

pub fn main_window() -> HWND {
    with(|a| a.main)
}

/// Changes and saves the configuration.
pub fn update_cfg(f: impl FnOnce(&mut Config)) {
    let result = with(|a| {
        f(&mut a.cfg);
        a.cfg.save(&a.data_dir)
    });
    if let Err(e) = result {
        ui::error(main_window(), &tr!(settings_not_saved, error = e));
    }
}

pub fn net_send(cmd: Command) {
    with(|a| a.net.send(cmd));
}

/// Called from network threads.
pub fn event_sink(event: Event) {
    EVENTS.lock().unwrap().push_back(event);
    let hwnd = MAIN_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe { PostMessageW(hwnd as HWND, WM_APP_NET, 0, 0) };
    }
}

/// Display name for a peer: our own name for them if they are a recipient.
pub fn peer_name(id: &str, fallback: &str) -> (String, bool) {
    with(|a| match a.cfg.contact_by_id(id) {
        Some(c) => (c.name.clone(), true),
        None => (
            if fallback.trim().is_empty() {
                tr!(unknown_sender).into()
            } else {
                fallback.trim().to_string()
            },
            false,
        ),
    })
}

pub fn start_send(contact: &Contact, path: PathBuf, kind: Kind) {
    let Some(to) = identity::parse_contact_id(&contact.id) else {
        ui::error(main_window(), &tr!(contact_invalid, name = contact.name));
        return;
    };
    let id = net::next_transfer_id();
    let item = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let hwnd = progress::open(
        id,
        progress::Direction::Send,
        &contact.name,
        &item,
        kind,
        None,
        true,
    );
    with(|a| a.transfers.insert(id, hwnd));
    let my_name = with(|a| a.cfg.my_name.clone());
    net_send(Command::Send {
        id,
        to,
        path,
        kind,
        my_name,
        peer_name: contact.name.clone(),
    });
}

/// Accepts an incoming offer: remembers the folder and opens a progress
/// window, which gets the focus when the user accepted (`activate`).
pub fn accept_offer(id: u64, sender: &str, offer: &net::Offer, dir: PathBuf, activate: bool) {
    update_cfg(|c| c.last_save_dir = Some(dir.clone()));
    let hwnd = progress::open(
        id,
        progress::Direction::Receive,
        sender,
        &offer.name,
        offer.kind,
        Some(offer.size),
        activate,
    );
    with(|a| a.transfers.insert(id, hwnd));
    net_send(Command::Respond {
        id,
        accept: Some(dir),
    });
}

fn active_transfers() -> Vec<(u64, HWND)> {
    let windows: Vec<(u64, HWND)> = with(|a| a.transfers.iter().map(|(i, h)| (*i, *h)).collect());
    windows
        .into_iter()
        .filter(|&(_, h)| progress::is_active(h))
        .collect()
}

pub fn active_transfer_count() -> usize {
    active_transfers().len()
}

/// Cancels every running transfer (before exiting).
pub fn cancel_all_transfers() {
    for (id, _) in active_transfers() {
        net_send(Command::Cancel { id });
    }
}

/// Drains the queue of network events (on the UI thread).
pub fn process_events() {
    loop {
        let Some(event) = EVENTS.lock().unwrap().pop_front() else {
            break;
        };
        if let Some(event) = handle_event(event) {
            // The target window is busy (e.g. showing a message box); retry shortly.
            EVENTS.lock().unwrap().push_front(event);
            unsafe { SetTimer(std::ptr::null_mut(), 0, 250, Some(retry_timer)) };
            break;
        }
    }
}

unsafe extern "system" fn retry_timer(_: HWND, _: u32, id: usize, _: u32) {
    unsafe { KillTimer(std::ptr::null_mut(), id) };
    process_events();
}

/// Handles one event; returns it back if it could not be delivered yet.
fn handle_event(event: Event) -> Option<Event> {
    match event {
        Event::BindFailed(msg) => ui::main_window::set_status(&tr!(network_problem, error = msg)),
        Event::Online(online) => ui::main_window::set_online(online),
        Event::Offer { id, from, offer } => {
            let from = from.to_string();
            let (name, known) = peer_name(&from, &offer.sender_name);
            let cfg = cfg();
            // Without enough disk space, ask instead (the window explains why).
            let auto_dir = Some(cfg.auto_save_dir()).filter(|d| {
                cfg.auto_accept
                    && known
                    && std::fs::create_dir_all(d).is_ok()
                    && incoming::check_space(&offer, d).enough != Some(false)
            });
            if let Some(dir) = auto_dir {
                accept_offer(id, &name, &offer, dir, false);
                tray::balloon(
                    main_window(),
                    tr!(receiving_balloon_title),
                    &tr!(receiving_balloon, name = name, item = offer.name),
                    None,
                );
            } else {
                let hwnd = incoming::open(id, &from, &name, known, offer);
                with(|a| a.offers.insert(id, hwnd));
            }
        }
        Event::Restored { id, record, have } => {
            let (name, _) = peer_name(&record.peer, &record.peer_name);
            let dir = match record.role() {
                Role::Send => progress::Direction::Send,
                Role::Receive => progress::Direction::Receive,
            };
            let hwnd = progress::open(
                id,
                dir,
                &name,
                &record.offer.name,
                record.offer.kind,
                Some(record.offer.size),
                false,
            );
            progress::set_progress(hwnd, have, record.offer.size);
            with(|a| a.transfers.insert(id, hwnd));
            tray::balloon(
                main_window(),
                tr!(restored_balloon_title),
                &tr!(restored_balloon, item = record.offer.name),
                Some(BalloonAction::Window(hwnd)),
            );
        }
        Event::Phase { id, phase } => {
            if let Some(h) = transfer_window(id) {
                let Some(notice) = progress::set_phase(h, phase) else {
                    return Some(Event::Phase { id, phase });
                };
                if let Some((title, text)) = notice.balloon {
                    tray::balloon(main_window(), &title, &text, Some(BalloonAction::Window(h)));
                }
            }
        }
        Event::Progress { id, done, total } => {
            if let Some(h) = transfer_window(id) {
                // Progress updates are frequent; dropping one is harmless.
                progress::set_progress(h, done, total);
            }
        }
        Event::Path { id, direct } => {
            if let Some(h) = transfer_window(id)
                && !progress::set_path(h, direct)
            {
                return Some(Event::Path { id, direct });
            }
        }
        Event::Finished { id, outcome } => {
            // The sender gave up while we were still deciding.
            if let Some(h) = with(|a| a.offers.get(&id).copied()) {
                if !incoming::close_by_peer(h) {
                    return Some(Event::Finished { id, outcome });
                }
                with(|a| a.offers.remove(&id));
            }
            if let Some(h) = transfer_window(id) {
                let Some(done) = progress::finish(h, &outcome) else {
                    return Some(Event::Finished { id, outcome });
                };
                let main_visible = unsafe { IsWindowVisible(main_window()) } != 0;
                if done.notify || (!main_visible && matches!(outcome, Outcome::Completed(_))) {
                    let action = match done.path {
                        Some(path) => Some(BalloonAction::Reveal(path)),
                        None => Some(BalloonAction::Window(h)),
                    };
                    tray::balloon(main_window(), &done.title, &done.text, action);
                }
            }
        }
    }
    None
}

fn transfer_window(id: u64) -> Option<HWND> {
    with(|a| a.transfers.get(&id).copied())
}

/// Forgets a closed progress window.
pub fn transfer_window_closed(id: u64) {
    with(|a| a.transfers.remove(&id));
}

pub fn offer_window_closed(id: u64) {
    with(|a| a.offers.remove(&id));
}
