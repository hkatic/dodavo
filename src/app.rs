//! Application state shared by the UI (UI thread only) and the bridge that
//! carries network events onto the UI thread.

use std::{
    cell::{Cell, RefCell},
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
    System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState},
    UI::WindowsAndMessaging::{IsWindowVisible, KillTimer, PostMessageW, SetTimer, WM_APP},
};

use crate::{
    config::{Config, Contact},
    i18n::{tr, trn},
    identity,
    net::{self, Command, Event, Kind, NetHandle, Outcome, Phase, Role},
    ui::{
        self, APP_NAME, format_size, incoming, progress,
        tray::{self, BalloonAction},
    },
    update,
};

/// Speed is averaged over the progress reports of the last few seconds.
const RATE_WINDOW: Duration = Duration::from_secs(4);

pub const WM_APP_NET: u32 = WM_APP + 1;

pub struct App {
    pub cfg: Config,
    pub data_dir: PathBuf,
    pub my_id: String,
    pub net: NetHandle,
    /// The `--profile` this instance runs with.
    pub profile: Option<String>,
    pub main: HWND,
    /// Whether the network reports us online (`None` until it says).
    pub online: Option<bool>,
    /// Running and recently finished transfers by transfer id.
    pub transfers: HashMap<u64, Transfer>,
    /// Open "incoming transfer" windows by transfer id.
    pub offers: HashMap<u64, HWND>,
    /// What selecting the last notification does.
    pub balloon_action: Option<BalloonAction>,
    /// The open "Update Available" window, if any.
    pub update_window: HWND,
    /// The running update check or download, if any.
    pub update_job: Option<update::Job>,
    /// What the user chose for running transfers when installing an update:
    /// cancel them (`true`) or suspend them when Setup asks us to exit.
    pub update_cancel_transfers: bool,
}

/// What the event queue carries onto the UI thread.
pub enum Queued {
    Net(Event),
    Update(update::Event),
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
    /// Bytes per second over the last few seconds, when known.
    pub rate: Option<u64>,
    /// Recent progress reports (time, bytes done) for the speed.
    samples: VecDeque<(Instant, u64)>,
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
            rate: None,
            samples: VecDeque::new(),
            list_shown: None,
        }
    }

    /// Records a progress report at `now` and updates the speed. Returns
    /// whether the total changed (the first report tells the size).
    fn note_progress(&mut self, now: Instant, done: u64, total: u64) -> bool {
        let changed = self.total != total;
        self.done = done;
        self.total = total;
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > RATE_WINDOW {
            self.samples.pop_front();
        }
        self.rate = match (self.samples.front(), self.samples.back()) {
            (Some(&(t0, d0)), Some(&(t1, d1))) => {
                let secs = t1.duration_since(t0).as_secs_f64();
                (secs > 0.5 && d1 > d0).then(|| ((d1 - d0) as f64 / secs) as u64)
            }
            _ => None,
        };
        changed
    }

    /// A phase change (pause, lost connection, …) makes the old speed stale.
    fn clear_rate(&mut self) {
        self.samples.clear();
        self.rate = None;
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
            Some(Phase::WaitingForConnection { .. }) => tr!(tx_waiting_connection),
            Some(Phase::Verifying) => tr!(tx_verifying),
            Some(Phase::Unpacking) => tr!(tx_unpacking),
            Some(Phase::PeerUnpacking) => tr!(tx_peer_unpacking),
        };
        let with_percent = self.total > 0
            && matches!(
                self.phase,
                Some(
                    Phase::Transferring
                        | Phase::Paused
                        | Phase::PausedByPeer
                        | Phase::WaitingForConnection { .. }
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

    /// Whether data is being worked on right now (not paused, not waiting):
    /// then the computer must not go to sleep by itself.
    fn moving_data(&self) -> bool {
        self.active
            && matches!(
                self.phase,
                Some(Phase::Preparing | Phase::Transferring | Phase::Verifying | Phase::Unpacking)
            )
    }
}

/// Keeps the computer from sleeping automatically while `busy` (like a
/// download does); the display may still turn off. The flag belongs to the
/// calling thread, so this runs on the UI thread only.
fn keep_awake(busy: bool) {
    if AWAKE.get() == busy {
        return;
    }
    AWAKE.set(busy);
    let state = if busy {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    unsafe { SetThreadExecutionState(state) };
}

/// The text of the recipient list's Transfer column for one peer's transfers.
fn status_of(mine: &[&Transfer]) -> String {
    match mine {
        [] => String::new(),
        [t] => t.describe(),
        _ => trn!(transfers_in_progress, mine.len()),
    }
}

/// The tooltip of the notification area icon (what a screen reader reads on
/// it): the running transfer, or how many there are, with progress and speed.
fn tray_tip(active: &[&Transfer]) -> String {
    let speed = |rate: u64| tr!(speed_value, rate = format_size(rate));
    match active {
        [] => APP_NAME.to_string(),
        [t] => match t.rate {
            Some(rate) => tr!(
                tray_tip_one_speed,
                app = APP_NAME,
                status = t.describe(),
                speed = speed(rate)
            ),
            None => tr!(tray_tip_one, app = APP_NAME, status = t.describe()),
        },
        many => {
            let transfers = trn!(transfers_in_progress, many.len());
            // Progress over the transfers whose size is known.
            let (done, total) = many
                .iter()
                .filter(|t| t.total > 0)
                .fold((0u128, 0u128), |(d, s), t| {
                    (d + t.done.min(t.total) as u128, s + t.total as u128)
                });
            let percent = (done * 100).checked_div(total).unwrap_or(0);
            let rate: u64 = many.iter().filter_map(|t| t.rate).sum();
            if many.iter().any(|t| t.rate.is_some()) {
                tr!(
                    tray_tip_many_speed,
                    app = APP_NAME,
                    transfers = transfers,
                    percent = percent,
                    speed = speed(rate)
                )
            } else {
                tr!(
                    tray_tip_many,
                    app = APP_NAME,
                    transfers = transfers,
                    percent = percent
                )
            }
        }
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// When the notification area icon last showed the transfers.
    static TRAY_SHOWN: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Whether the computer is currently kept from sleeping.
    static AWAKE: Cell<bool> = const { Cell::new(false) };
}

static EVENTS: Mutex<VecDeque<Queued>> = Mutex::new(VecDeque::new());
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
    enqueue(Queued::Net(event));
}

/// Called from the update thread.
pub fn update_sink(event: update::Event) {
    enqueue(Queued::Update(event));
}

fn enqueue(event: Queued) {
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
/// What is kept of a folder is decided now, from the settings.
pub fn accept_offer(
    id: u64,
    peer_id: &str,
    sender: &str,
    offer: &net::Offer,
    dir: PathBuf,
    activate: bool,
) {
    update_cfg(|c| c.last_save_dir = Some(dir.clone()));
    let folder = cfg().received_folder;
    let hwnd = progress::open(
        id,
        progress::Direction::Receive,
        sender,
        &offer.name,
        offer.kind,
        Some(offer.size),
        activate,
    );
    progress::set_received_folder(hwnd, folder);
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
        accept: Some(net::Accept { dir, folder }),
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

/// Updates the recipient list's Transfer column for a transfer's peer and the
/// notification area icon. Progress changes are frequent, so unless `force`d,
/// each is updated about once a second.
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
    refresh_tray(force);
}

/// Shows the running transfers on the notification area icon: a count badge
/// and a tooltip with progress and speed.
fn refresh_tray(force: bool) {
    let due = force
        || TRAY_SHOWN
            .get()
            .is_none_or(|shown| shown.elapsed() >= Duration::from_secs(1));
    if !due {
        return;
    }
    TRAY_SHOWN.set(Some(Instant::now()));
    let (count, tip, busy) = with(|a| {
        let active: Vec<&Transfer> = a.transfers.values().filter(|t| t.active).collect();
        let busy = active.iter().any(|t| t.moving_data());
        (active.len(), tray_tip(&active), busy)
    });
    tray::set_status(main_window(), count, &tip);
    keep_awake(busy);
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

/// Drains the queue of network and update events (on the UI thread).
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
fn handle_event(event: Queued) -> Option<Queued> {
    match event {
        Queued::Net(event) => handle_net_event(event).map(Queued::Net),
        Queued::Update(event) => ui::update::handle_event(event).map(Queued::Update),
    }
}

fn handle_net_event(event: Event) -> Option<Event> {
    match event {
        Event::BindFailed(msg) => ui::main_window::set_status(&tr!(network_problem, error = msg)),
        Event::Online(online) => {
            with(|a| a.online = Some(online));
            ui::main_window::set_online(online);
            if online {
                // A check that was waiting for the connection.
                ui::update::tick();
            }
        }
        Event::Offer { id, from, offer } => {
            let from = from.to_string();
            let (name, known) = peer_name(&from, &offer.sender_name);
            let cfg = cfg();
            // Without enough disk space, ask instead (the window explains why).
            let auto_dir = Some(cfg.auto_save_dir()).filter(|d| {
                cfg.auto_accept
                    && known
                    && std::fs::create_dir_all(d).is_ok()
                    && incoming::check_space(&offer, d, cfg.received_folder).enough != Some(false)
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
            if let Some(folder) = record.received_folder() {
                progress::set_received_folder(hwnd, folder);
            }
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
                Some(BalloonAction::Transfer(id)),
            );
        }
        Event::Phase { id, phase } => {
            with(|a| {
                if let Some(t) = a.transfers.get_mut(&id) {
                    t.phase = Some(phase);
                    t.clear_rate();
                }
            });
            refresh_transfer_views(id, true);
            if let Some(h) = transfer_window(id) {
                let Some(notice) = progress::set_phase(h, phase) else {
                    return Some(Event::Phase { id, phase });
                };
                if let Some((title, text)) = notice.balloon {
                    tray::balloon(
                        main_window(),
                        &title,
                        &text,
                        Some(BalloonAction::Transfer(id)),
                    );
                }
            }
        }
        Event::HandedOver { id } => {
            if let Some(h) = transfer_window(id)
                && !progress::set_handed_over(h)
            {
                return Some(Event::HandedOver { id });
            }
        }
        Event::Progress { id, done, total } => {
            // The first report tells the size; show it right away (a paused
            // transfer may not report again for a long time).
            let now = Instant::now();
            let size_known = with(|a| {
                a.transfers
                    .get_mut(&id)
                    .is_some_and(|t| t.note_progress(now, done, total))
            });
            refresh_transfer_views(id, size_known);
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
                    // A completed transfer's window closes by itself; by
                    // then a click brings up the main window.
                    let action = match done.path {
                        Some(path) => BalloonAction::Reveal(path),
                        None => BalloonAction::Transfer(id),
                    };
                    tray::balloon(main_window(), &done.title, &done.text, Some(action));
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
    refresh_tray(true);
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
    fn speed_comes_from_the_recent_reports() {
        let mut t = transfer(Some(Phase::Transferring), 0, 1000);
        let start = Instant::now();
        assert!(!t.note_progress(start, 0, 1000));
        // Too early for a rate.
        t.note_progress(start + Duration::from_millis(200), 100, 1000);
        assert_eq!(t.rate, None);
        t.note_progress(start + Duration::from_secs(2), 1000 - 800, 1000);
        assert_eq!(t.rate, Some(100));
        // Old samples fall out of the window; the first report sets the size.
        t.note_progress(start + Duration::from_secs(7), 700, 1000);
        assert_eq!(t.samples.len(), 2);
        assert!(t.note_progress(start + Duration::from_secs(8), 800, 2000));
        t.clear_rate();
        assert_eq!(t.rate, None);
    }

    #[test]
    fn tray_tip_names_one_transfer_and_counts_several() {
        assert_eq!(tray_tip(&[]), APP_NAME);
        let mut one = transfer(Some(Phase::Transferring), 45, 100);
        let tip = tray_tip(&[&one]);
        assert!(tip.starts_with(APP_NAME), "{tip}");
        assert!(tip.contains("report.pdf") && tip.contains("45"), "{tip}");
        assert!(!tip.contains("/s"), "{tip}");
        one.rate = Some(2048);
        let tip = tray_tip(&[&one]);
        assert!(tip.contains("/s") && tip.contains("2"), "{tip}");

        let mut two = transfer(Some(Phase::Transferring), 0, 0);
        let tip = tray_tip(&[&one, &two]);
        assert!(tip.contains('2') && !tip.contains("report.pdf"), "{tip}");
        assert!(tip.contains("45%") || tip.contains("45 %"), "{tip}");
        two.total = 100;
        two.done = 100;
        two.rate = Some(1024);
        let tip = tray_tip(&[&one, &two]);
        assert!(tip.contains("72%") || tip.contains("72 %"), "{tip}");
        assert!(tip.contains("3.0 KB") || tip.contains("3,0 KB"), "{tip}");
        assert!(!tip.contains('{'), "{tip}");
    }

    #[test]
    fn only_flowing_data_keeps_the_computer_awake() {
        for (phase, busy) in [
            (None, false),
            (Some(Phase::Connecting), false),
            (Some(Phase::WaitingForAccept), false),
            (Some(Phase::Preparing), true),
            (Some(Phase::Transferring), true),
            (Some(Phase::Verifying), true),
            (Some(Phase::Unpacking), true),
            (Some(Phase::PeerUnpacking), false),
            (Some(Phase::Paused), false),
            (Some(Phase::PausedByPeer), false),
            (Some(Phase::WaitingForConnection { lost: true }), false),
        ] {
            let mut t = transfer(phase, 1, 2);
            assert_eq!(t.moving_data(), busy, "{phase:?}");
            t.active = false;
            assert!(!t.moving_data(), "{phase:?} once ended");
        }
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
