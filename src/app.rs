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
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{IsWindowVisible, KillTimer, PostMessageW, SetTimer, WM_APP},
};

use crate::{
    config::{Config, Contact},
    i18n::{tr, trn},
    identity,
    net::{self, Command, Event, Kind, NetHandle, Outcome, Phase, Role},
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
    /// Running and recently finished transfers by transfer id.
    pub transfers: HashMap<u64, Transfer>,
    /// Open "incoming transfer" windows by transfer id.
    pub offers: HashMap<u64, HWND>,
    /// What selecting the last notification does.
    pub balloon_action: Option<BalloonAction>,
}

/// A transfer the UI knows about: its progress window and the state that the
/// recipient list and the Transfers menu show.
pub struct Transfer {
    pub hwnd: HWND,
    /// The other side's Contact ID (empty when unknown).
    pub peer_id: String,
    pub peer_name: String,
    pub dir: progress::Direction,
    pub item: String,
    pub phase: Option<Phase>,
    pub done: u64,
    pub total: u64,
    /// False once the transfer has ended; its window may still be open.
    pub active: bool,
    /// When the recipient list last showed this transfer's state.
    list_shown: Option<Instant>,
}

impl Transfer {
    fn new(
        hwnd: HWND,
        peer_id: &str,
        peer_name: &str,
        dir: progress::Direction,
        item: &str,
        total: u64,
    ) -> Self {
        Transfer {
            hwnd,
            peer_id: peer_id.to_string(),
            peer_name: peer_name.to_string(),
            dir,
            item: item.to_string(),
            phase: None,
            done: 0,
            total,
            active: true,
            list_shown: None,
        }
    }

    /// One line of state for the recipient list and the Transfers menu, such
    /// as “Sending – “report.pdf” (45%)”.
    pub fn describe(&self) -> String {
        let state = match self.phase {
            None => tr!(starting),
            Some(Phase::Preparing) => tr!(tx_preparing),
            Some(Phase::Connecting) => tr!(tx_connecting),
            Some(Phase::WaitingForAccept) => tr!(tx_waiting_accept),
            Some(Phase::Transferring) => match self.dir {
                progress::Direction::Send => tr!(tx_sending),
                progress::Direction::Receive => tr!(tx_receiving),
            },
            Some(Phase::Paused) => tr!(tx_paused),
            Some(Phase::PausedByPeer) => tr!(tx_paused_by_peer),
            Some(Phase::WaitingForConnection) => tr!(tx_waiting_connection),
            Some(Phase::Verifying) => tr!(tx_verifying),
            Some(Phase::Unpacking) => tr!(tx_unpacking),
        };
        let with_percent = self.total > 0
            && matches!(
                self.phase,
                Some(
                    Phase::Transferring
                        | Phase::Paused
                        | Phase::PausedByPeer
                        | Phase::WaitingForConnection
                )
            );
        if with_percent {
            let percent = (self.done as u128 * 100 / self.total as u128).min(100);
            tr!(
                transfer_status_percent,
                state = state,
                item = self.item,
                percent = percent
            )
        } else {
            tr!(transfer_status, state = state, item = self.item)
        }
    }

    /// Whether this is a running transfer with the given peer.
    fn is_with(&self, peer_id: &str) -> bool {
        self.active && !self.peer_id.is_empty() && self.peer_id.eq_ignore_ascii_case(peer_id)
    }
}

/// The text of the recipient list's Transfer column for one peer's transfers.
fn status_of(mine: &[&Transfer]) -> String {
    match mine {
        [] => String::new(),
        [t] => t.describe(),
        _ => trn!(transfers_in_progress, mine.len()),
    }
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
    let transfer = Transfer::new(
        hwnd,
        &contact.id,
        &contact.name,
        progress::Direction::Send,
        &item,
        0,
    );
    with(|a| a.transfers.insert(id, transfer));
    refresh_transfer_views(id, true);
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
/// `peer_id` is the sender's Contact ID and `sender` the name shown for them.
pub fn accept_offer(
    id: u64,
    peer_id: &str,
    sender: &str,
    offer: &net::Offer,
    dir: PathBuf,
    activate: bool,
) {
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
    let transfer = Transfer::new(
        hwnd,
        peer_id,
        sender,
        progress::Direction::Receive,
        &offer.name,
        offer.size,
    );
    with(|a| a.transfers.insert(id, transfer));
    refresh_transfer_views(id, true);
    net_send(Command::Respond {
        id,
        accept: Some(dir),
    });
}

fn active_transfers() -> Vec<(u64, HWND)> {
    with(|a| {
        a.transfers
            .iter()
            .filter(|(_, t)| t.active)
            .map(|(id, t)| (*id, t.hwnd))
            .collect()
    })
}

/// The running transfers for the Transfers menu (id, window and menu text),
/// sorted by peer name and item.
pub fn active_transfer_list() -> Vec<(u64, HWND, String)> {
    with(|a| {
        let mut list: Vec<(&u64, &Transfer)> =
            a.transfers.iter().filter(|(_, t)| t.active).collect();
        list.sort_by_cached_key(|(_, t)| (t.peer_name.to_lowercase(), t.item.clone()));
        list.into_iter()
            .map(|(id, t)| {
                let text = tr!(
                    transfer_menu_entry,
                    name = t.peer_name,
                    status = t.describe()
                );
                (*id, t.hwnd, text)
            })
            .collect()
    })
}

/// The windows of the running transfers with one peer.
pub fn transfer_windows_of(peer_id: &str) -> Vec<HWND> {
    with(|a| {
        a.transfers
            .values()
            .filter(|t| t.is_with(peer_id))
            .map(|t| t.hwnd)
            .collect()
    })
}

/// What the recipient list shows next to one peer.
pub fn transfer_status(peer_id: &str) -> String {
    with(|a| {
        let mine: Vec<&Transfer> = a
            .transfers
            .values()
            .filter(|t| t.is_with(peer_id))
            .collect();
        status_of(&mine)
    })
}

/// Updates the recipient list's Transfer column for a transfer's peer.
/// Progress changes are frequent, so unless `force`d, the list is updated
/// about once a second per transfer.
fn refresh_transfer_views(id: u64, force: bool) {
    let peer = with(|a| {
        let t = a.transfers.get_mut(&id)?;
        let due = force
            || t.list_shown
                .is_none_or(|shown| shown.elapsed() >= Duration::from_secs(1));
        if !due {
            return None;
        }
        t.list_shown = Some(Instant::now());
        Some(t.peer_id.clone())
    });
    if let Some(peer) = peer.filter(|p| !p.is_empty()) {
        ui::main_window::update_transfer_status(&peer);
    }
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
                accept_offer(id, &from, &name, &offer, dir, false);
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
            let mut transfer = Transfer::new(
                hwnd,
                &record.peer,
                &name,
                dir,
                &record.offer.name,
                record.offer.size,
            );
            transfer.done = have;
            with(|a| a.transfers.insert(id, transfer));
            refresh_transfer_views(id, true);
            tray::balloon(
                main_window(),
                tr!(restored_balloon_title),
                &tr!(restored_balloon, item = record.offer.name),
                Some(BalloonAction::Window(hwnd)),
            );
        }
        Event::Phase { id, phase } => {
            with(|a| {
                if let Some(t) = a.transfers.get_mut(&id) {
                    t.phase = Some(phase);
                }
            });
            refresh_transfer_views(id, true);
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
            with(|a| {
                if let Some(t) = a.transfers.get_mut(&id) {
                    t.done = done;
                    t.total = total;
                }
            });
            refresh_transfer_views(id, false);
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
            with(|a| {
                if let Some(t) = a.transfers.get_mut(&id) {
                    t.active = false;
                }
            });
            refresh_transfer_views(id, true);
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

pub fn transfer_window(id: u64) -> Option<HWND> {
    with(|a| a.transfers.get(&id).map(|t| t.hwnd))
}

/// Forgets a closed progress window.
pub fn transfer_window_closed(id: u64) {
    let peer = with(|a| a.transfers.remove(&id).map(|t| t.peer_id));
    if let Some(peer) = peer.filter(|p| !p.is_empty()) {
        ui::main_window::update_transfer_status(&peer);
    }
}


pub fn offer_window_closed(id: u64) {
    with(|a| a.offers.remove(&id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(phase: Option<Phase>, done: u64, total: u64) -> Transfer {
        let mut t = Transfer::new(
            std::ptr::null_mut(),
            "abc",
            "Alice",
            progress::Direction::Send,
            "report.pdf",
            total,
        );
        t.phase = phase;
        t.done = done;
        t
    }

    #[test]
    fn describe_shows_the_item_and_the_percentage_while_transferring() {
        let t = transfer(Some(Phase::Transferring), 45, 100);
        let text = t.describe();
        assert!(text.contains("report.pdf"), "{text}");
        assert!(text.contains("45"), "{text}");
        assert!(text.contains(tr!(tx_sending)), "{text}");

        // Before the size is known there is no percentage.
        let text = transfer(Some(Phase::Transferring), 0, 0).describe();
        assert!(!text.contains('%'), "{text}");
        // Preparing has no meaningful percentage either.
        let text = transfer(Some(Phase::Preparing), 50, 100).describe();
        assert!(!text.contains('%'), "{text}");
        assert!(text.contains(tr!(tx_preparing)), "{text}");
        // More than 100% is never shown.
        let text = transfer(Some(Phase::Paused), 200, 100).describe();
        assert!(text.contains("100"), "{text}");
    }

    #[test]
    fn status_counts_several_transfers() {
        assert_eq!(status_of(&[]), "");
        let one = transfer(Some(Phase::Transferring), 1, 2);
        assert_eq!(status_of(&[&one]), one.describe());
        let two = transfer(Some(Phase::Connecting), 0, 0);
        let text = status_of(&[&one, &two]);
        assert!(text.contains('2'), "{text}");
        assert!(!text.contains("report.pdf"), "{text}");
    }

    #[test]
    fn peer_match_ignores_case_and_needs_a_running_transfer() {
        let mut t = transfer(None, 0, 0);
        assert!(t.is_with("ABC"));
        assert!(!t.is_with("abd"));
        t.active = false;
        assert!(!t.is_with("abc"));
        t.active = true;
        t.peer_id.clear();
        assert!(!t.is_with(""));
    }
}
