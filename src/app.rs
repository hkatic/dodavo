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
    /// Why the network has no endpoint, while it has none (see
    /// `Event::BindFailed`).
    pub net_problem: Option<String>,
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
    /// The speed, from the progress reports, as the progress window has it.
    speed: progress::Speed,
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
            speed: progress::Speed::default(),
            list_shown: None,
        }
    }

    /// Records a progress report at `now` and updates the speed. Returns
    /// whether the total changed (the first report tells the size).
    fn note_progress(&mut self, now: Instant, done: u64, total: u64) -> bool {
        let changed = self.total != total;
        self.done = done;
        self.total = total;
        self.speed.note(now, done);
        changed
    }

    /// A phase change (pause, lost connection, …) makes the old speed stale.
    fn clear_rate(&mut self) {
        self.speed.reset();
    }

    /// Bytes per second at `now`, when known, and only while some data is
    /// still to come (a peer may take a while to check what arrived); zero
    /// once the data of a transfer has stopped coming. As in the progress
    /// window (see `progress::Progress::rate`, `progress::Speed::stalled`).
    fn rate(&self, now: Instant) -> Option<u64> {
        if self.done >= self.total {
            None
        } else if self.phase == Some(Phase::Transferring) && self.speed.stalled(now) {
            Some(0)
        } else {
            self.speed.rate()
        }
    }

    /// One line of state for the recipient list and the Transfers menu, such
    /// as “Sending – “report.pdf” (45%)”.
    pub fn describe(&self) -> String {
        crate::view::describe(self.phase, self.dir, self.done, self.total, &self.item)
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
fn tray_tip(active: &[&Transfer], now: Instant) -> String {
    let speed = |rate: u64| tr!(speed_value, rate = format_size(rate));
    match active {
        [] => APP_NAME.to_string(),
        [t] => match t.rate(now) {
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
            let rates: Vec<u64> = many.iter().filter_map(|t| t.rate(now)).collect();
            let rate: u64 = rates.iter().sum();
            if !rates.is_empty() {
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
        // Whoever is blocked now is, also if the file cannot be saved.
        a.net.set_blocked(a.cfg.blocked_ids());
        a.net.set_limits(a.cfg.limits());
        a.net
            .set_temp_dir(a.cfg.temp_dir_for(a.profile.as_deref()));
        a.cfg.save(&a.data_dir)
    });
    if let Err(e) = result {
        ui::error(main_window(), &tr!(settings_not_saved, error = e));
    }
}

/// The folder for the temporary archives of new transfers (see
/// `Config::temp_dir_in_use`).
pub fn temp_dir() -> std::path::PathBuf {
    with(|a| a.cfg.temp_dir_in_use(a.profile.as_deref()))
}

/// Blocks the sender `id` under `name` (see `Config::block`) and declines
/// what they offer that waits for an answer, except in the window `except`,
/// which its caller closes.
pub fn block_sender(name: &str, id: &str, except: HWND) {
    update_cfg(|c| c.block(name, id));
    incoming::decline_from(id, except);
    ui::main_window::refresh_list();
}

/// Gives this installation a new Contact ID, if the user confirms it at
/// `owner`: every transfer is cancelled and every waiting offer declined
/// first, since none can go on under another ID (see
/// `NetHandle::change_identity`). Says how it went, and returns whether the
/// ID changed. Not to be called inside `with`.
pub fn change_contact_id(owner: HWND) -> bool {
    // Until the network is up, it takes no commands.
    if !with(|a| a.net.is_up()) {
        ui::error(owner, tr!(change_id_not_ready));
        return false;
    }
    let ending = with(|a| a.offers.len()) + active_transfer_count();
    let mut question = tr!(change_id_confirm).to_string();
    if ending > 0 {
        question = format!("{question}\n\n{}", trn!(change_id_transfers, ending));
    }
    if !ui::confirm_no(owner, &question) {
        return false;
    }
    incoming::decline_all();
    let net = with(|a| a.net.clone());
    let secret = identity::new_key();
    let new_id = secret.public().to_string();
    // The UI waits: nothing may begin under the old ID meanwhile.
    let result = ui::busy(|| net.change_identity(secret, Duration::from_secs(20)));
    match result {
        Ok(()) => {
            with(|a| a.my_id = new_id.clone());
            ui::info(owner, &tr!(change_id_done, id = new_id));
            true
        }
        Err(e) => {
            let text = match e {
                net::IdentityError::Busy => tr!(change_id_busy).to_string(),
                net::IdentityError::Failed(error) => tr!(change_id_failed, error = error),
                net::IdentityError::TimedOut => tr!(change_id_not_ready).to_string(),
            };
            ui::error(owner, &text);
            false
        }
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

/// Starts sending each of `paths` to each of `contacts`, every one as a
/// transfer of its own, and says whether it did: nothing starts when one of
/// the Contact IDs is not valid, which `owner` then shows.
pub fn start_sends(owner: HWND, contacts: &[Contact], paths: &[PathBuf]) -> bool {
    let invalid = contacts
        .iter()
        .find(|c| identity::parse_contact_id(&c.id).is_none());
    if let Some(contact) = invalid {
        ui::error(owner, &tr!(contact_invalid, name = contact.name));
        return false;
    }
    // Only the first window takes the focus, which then stays there while the
    // others open.
    let mut first = true;
    for contact in contacts {
        for path in paths {
            let kind = if path.is_dir() {
                Kind::Folder
            } else {
                Kind::File
            };
            start_send(contact, path.clone(), kind, first);
            first = false;
        }
    }
    true
}

/// Starts sending `path`. Its progress window takes the focus if `activate`.
fn start_send(contact: &Contact, path: PathBuf, kind: Kind, activate: bool) {
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
        activate,
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
    // Several offers answered together go to one folder: one save is enough.
    if with(|a| a.cfg.last_save_dir.as_ref() != Some(&dir)) {
        update_cfg(|c| c.last_save_dir = Some(dir.clone()));
    }
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

/// The data of transfer `id` has stopped coming (see `progress::Speed`): the
/// notification area icon's speed says so at once, since no report comes
/// that would update it.
pub fn transfer_stalled(id: u64) {
    refresh_transfer_views(id, true);
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
        (active.len(), tray_tip(&active, Instant::now()), busy)
    });
    tray::set_status(main_window(), count, &tip);
    keep_awake(busy);
}

/// Whether a transfer in this phase is getting somewhere by itself. A paused
/// one and one waiting for a connection may stay that way for days.
fn under_way(phase: Option<Phase>) -> bool {
    !matches!(
        phase,
        Some(Phase::Paused | Phase::PausedByPeer | Phase::WaitingForConnection { .. })
    )
}

/// Whether another transfer with the same person as `id` is under way: the
/// items of one selection, which should not pile up a dialog each.
pub fn others_under_way(id: u64) -> bool {
    with(|a| {
        let Some(peer) = a.transfers.get(&id).map(|t| t.peer_id.clone()) else {
            return false;
        };
        a.transfers.iter().any(|(other, t)| {
            *other != id && t.active && t.peer_id.eq_ignore_ascii_case(&peer) && under_way(t.phase)
        })
    })
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

/// Whose turn an event has to wait for: the events of one transfer, and
/// those of the update, are delivered in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Transfer(u64),
    Update,
}

/// The lane of an event; `None` for one that never waits for a window.
/// Progress reports are among those: one that finds its window busy is
/// dropped, and holding them back would only pile them up.
fn lane_of(event: &Queued) -> Option<Lane> {
    match event {
        Queued::Update(_) => Some(Lane::Update),
        Queued::Net(event) => match event {
            Event::Offer { id, .. }
            | Event::Restored { id, .. }
            | Event::Phase { id, .. }
            | Event::HandedOver { id }
            | Event::CancelWaits { id }
            | Event::Path { id, .. }
            | Event::Finished { id, .. } => Some(Lane::Transfer(*id)),
            Event::BindFailed(_)
            | Event::Connecting
            | Event::Online(_)
            | Event::OverLimit { .. }
            | Event::Progress { .. } => None,
        },
    }
}

/// The events that could not be delivered yet, because their window was busy
/// (showing a message box, say), in their order. Only the events of the same
/// lane wait behind one: the other transfers go on.
///
/// Delivering an event can run a message loop (a window that asks
/// something), in which more events are delivered, so everything here is
/// behind cells, and no borrow is held while an event is delivered.
struct Backlog<E> {
    held: RefCell<Vec<(Lane, E)>>,
    /// The lanes of the events that are being delivered right now.
    in_hand: RefCell<Vec<Lane>>,
    /// Whether a call is trying the held events again: one inside it leaves
    /// them alone.
    retrying: Cell<bool>,
}

impl<E> Backlog<E> {
    const fn new() -> Self {
        Backlog {
            held: RefCell::new(Vec::new()),
            in_hand: RefCell::new(Vec::new()),
            retrying: Cell::new(false),
        }
    }

    /// Delivers the held events whose windows are free by now, and then
    /// those that `next` gives, through `deliver`, which returns an event
    /// that it could not deliver. Returns whether some are held now.
    fn run(
        &self,
        mut next: impl FnMut() -> Option<E>,
        lane_of: impl Fn(&E) -> Option<Lane>,
        mut deliver: impl FnMut(E) -> Option<E>,
    ) -> bool {
        if !self.retrying.replace(true) {
            // Calls inside this one only add at the end, so what is before
            // `at` stays where it is.
            let mut blocked = Vec::new();
            let mut at = 0;
            loop {
                let taken = {
                    let mut held = self.held.borrow_mut();
                    match held.get(at) {
                        None => break,
                        Some((lane, _)) if blocked.contains(lane) => {
                            at += 1;
                            continue;
                        }
                        Some(_) => held.remove(at),
                    }
                };
                let (lane, event) = taken;
                if let Some(event) = self.deliver_in(lane, event, &mut deliver) {
                    self.held.borrow_mut().insert(at, (lane, event));
                    blocked.push(lane);
                    at += 1;
                }
            }
            self.retrying.set(false);
        }
        while let Some(event) = next() {
            let Some(lane) = lane_of(&event) else {
                deliver(event);
                continue;
            };
            let waits = self.in_hand.borrow().contains(&lane)
                || self.held.borrow().iter().any(|(held, _)| *held == lane);
            if waits {
                self.held.borrow_mut().push((lane, event));
            } else if let Some(event) = self.deliver_in(lane, event, &mut deliver) {
                // Before what arrived for its lane while it was in hand.
                let mut held = self.held.borrow_mut();
                let at = held
                    .iter()
                    .position(|(other, _)| *other == lane)
                    .unwrap_or(held.len());
                held.insert(at, (lane, event));
            }
        }
        !self.held.borrow().is_empty()
    }

    /// Delivers one event, with its lane noted as in hand meanwhile.
    fn deliver_in(
        &self,
        lane: Lane,
        event: E,
        deliver: &mut impl FnMut(E) -> Option<E>,
    ) -> Option<E> {
        self.in_hand.borrow_mut().push(lane);
        let back = deliver(event);
        let mut in_hand = self.in_hand.borrow_mut();
        if let Some(at) = in_hand.iter().rposition(|other| *other == lane) {
            in_hand.remove(at);
        }
        back
    }
}

thread_local! {
    static BACKLOG: Backlog<Queued> = const { Backlog::new() };
    /// Whether the timer that tries the held events again is set.
    static RETRY_SET: Cell<bool> = const { Cell::new(false) };
}

/// Drains the queue of network and update events (on the UI thread). An
/// event whose window is busy waits, together with the later ones of its
/// transfer, and is tried again shortly (see `Backlog`).
pub fn process_events() {
    let held = BACKLOG.with(|backlog| {
        backlog.run(
            || EVENTS.lock().unwrap().pop_front(),
            lane_of,
            handle_event,
        )
    });
    if held && !RETRY_SET.replace(true) {
        unsafe { SetTimer(std::ptr::null_mut(), 0, 250, Some(retry_timer)) };
    }
}

unsafe extern "system" fn retry_timer(_: HWND, _: u32, id: usize, _: u32) {
    unsafe { KillTimer(std::ptr::null_mut(), id) };
    RETRY_SET.set(false);
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
        // What the network says of itself, each in place of what it said before.
        Event::BindFailed(error) => {
            with(|a| (a.online, a.net_problem) = (None, Some(error)));
            ui::main_window::show_status();
        }
        Event::Connecting => {
            with(|a| (a.online, a.net_problem) = (None, None));
            ui::main_window::show_status();
        }
        Event::Online(online) => {
            with(|a| (a.online, a.net_problem) = (Some(online), None));
            ui::main_window::show_status();
            if online {
                // A check that was waiting for the connection.
                ui::update::tick();
            }
        }
        Event::Offer { id, from, offer } => {
            let from = from.to_string();
            let (name, known) = peer_name(&from, &offer.sender_name);
            let cfg = cfg();
            // Without enough disk space, on a drive that cannot hold a file
            // this large, or for an item over the sender's limit, ask
            // instead (the window explains why).
            let auto_dir = cfg.auto_save_dir_for(&from, &offer).filter(|d| {
                std::fs::create_dir_all(d).is_ok() && {
                    let space = incoming::check_space(&offer, d, cfg.received_folder);
                    space.fits && space.enough != Some(false)
                }
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
                let hwnd = incoming::open_or_add(id, &from, &name, known, offer);
                with(|a| a.offers.insert(id, hwnd));
            }
        }
        Event::OverLimit { from, offer } => {
            let (name, _) = peer_name(&from.to_string(), &offer.sender_name);
            let (title, text) = crate::limit::declined_notice(&name, &offer);
            tray::balloon(main_window(), &title, &text, None);
        }
        Event::Restored {
            id,
            record,
            have,
            goes_on,
        } => {
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
            // Cancelled in the last run: the network side cancels it again.
            // The record tells whether the cancel waits on for the
            // receiver's verdict (the data had all gone out) or ends the
            // transfer as soon as it is issued.
            let cancelling = record.cancel_pending();
            if cancelling {
                progress::set_cancelling(hwnd, record.done_sent);
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
            // One that does not go on is reported as it ends, right after this.
            if goes_on {
                let text = if cancelling {
                    tr!(restored_cancelling_balloon, item = record.offer.name)
                } else {
                    tr!(restored_balloon, item = record.offer.name)
                };
                tray::balloon(
                    main_window(),
                    tr!(restored_balloon_title),
                    &text,
                    Some(BalloonAction::Transfer(id)),
                );
            }
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
                give_phase_notice(id, notice);
            }
        }
        Event::HandedOver { id } => {
            if let Some(h) = transfer_window(id)
                && !progress::set_handed_over(h)
            {
                return Some(Event::HandedOver { id });
            }
        }
        Event::CancelWaits { id } => {
            if let Some(h) = transfer_window(id) {
                let Some(notice) = progress::set_cancel_waits(h) else {
                    return Some(Event::CancelWaits { id });
                };
                give_phase_notice(id, notice);
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
                if !incoming::close_by_peer(h, id) {
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

/// Gives the notification that a change in the progress window of transfer
/// `id` asks for (see `progress::PhaseNotice`).
fn give_phase_notice(id: u64, notice: progress::PhaseNotice) {
    if let Some((title, text)) = notice.balloon {
        tray::balloon(
            main_window(),
            &title,
            &text,
            Some(BalloonAction::Transfer(id)),
        );
    }
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

    /// A transfer that may wait for days must not keep dialogs away.
    #[test]
    fn waiting_transfers_are_not_under_way() {
        assert!(under_way(None));
        assert!(under_way(Some(Phase::Connecting)));
        assert!(under_way(Some(Phase::WaitingForAccept)));
        assert!(under_way(Some(Phase::Transferring)));
        assert!(!under_way(Some(Phase::Paused)));
        assert!(!under_way(Some(Phase::PausedByPeer)));
        assert!(!under_way(Some(Phase::WaitingForConnection { lost: true })));
    }

    /// Delivers `events` through `backlog`, with the transfers in `busy`
    /// busy, and returns what was delivered. An event is its transfer and a
    /// number; transfer 0 stands for an event that never waits.
    fn deliver(
        backlog: &Backlog<(u64, u32)>,
        events: &[(u64, u32)],
        busy: &[u64],
    ) -> Vec<(u64, u32)> {
        let mut queue: VecDeque<(u64, u32)> = events.iter().copied().collect();
        let mut delivered = Vec::new();
        backlog.run(
            || queue.pop_front(),
            |(id, _)| (*id != 0).then_some(Lane::Transfer(*id)),
            |event| {
                if busy.contains(&event.0) {
                    Some(event)
                } else {
                    delivered.push(event);
                    None
                }
            },
        );
        delivered
    }

    /// A busy window holds up its own transfer's events, in their order,
    /// and nobody else's.
    #[test]
    fn a_busy_window_holds_up_only_its_own_events() {
        let backlog = Backlog::new();
        let events = [(1, 1), (2, 1), (1, 2), (0, 1), (2, 2), (3, 1)];
        assert_eq!(
            deliver(&backlog, &events, &[1]),
            [(2, 1), (0, 1), (2, 2), (3, 1)]
        );
        // Still busy: a later event of that transfer waits behind them, and
        // another transfer's window is busy now as well.
        assert_eq!(
            deliver(&backlog, &[(1, 3), (3, 2), (2, 3)], &[1, 3]),
            [(2, 3)]
        );
        // Free again: each transfer's events in their order.
        assert_eq!(
            deliver(&backlog, &[(1, 4)], &[3]),
            [(1, 1), (1, 2), (1, 3), (1, 4)]
        );
        assert_eq!(deliver(&backlog, &[], &[]), [(3, 2)]);
        assert!(backlog.held.borrow().is_empty());
        assert!(backlog.in_hand.borrow().is_empty());
    }

    /// Events that arrive while one of their transfer is being delivered (in
    /// a window's own message loop) wait for it, and stay behind it when it
    /// could not be delivered.
    #[test]
    fn events_wait_for_the_one_in_hand() {
        let backlog: Backlog<(u64, u32)> = Backlog::new();
        let lane = |event: &(u64, u32)| Some(Lane::Transfer(event.0));
        let mut outer = VecDeque::from([(1u64, 1u32)]);
        let delivered = RefCell::new(Vec::new());
        let held = backlog.run(
            || outer.pop_front(),
            lane,
            |event| {
                // Inside: its own transfer's next event, and another's.
                let mut inner = VecDeque::from([(1u64, 2u32), (2, 1)]);
                backlog.run(
                    || inner.pop_front(),
                    lane,
                    |event| {
                        delivered.borrow_mut().push(event);
                        None
                    },
                );
                Some(event)
            },
        );
        assert!(held);
        assert_eq!(*delivered.borrow(), [(2, 1)]);
        let mut none = VecDeque::new();
        backlog.run(
            || none.pop_front(),
            lane,
            |event| {
                delivered.borrow_mut().push(event);
                None
            },
        );
        assert_eq!(*delivered.borrow(), [(2, 1), (1, 1), (1, 2)]);
    }

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

    /// The speed is the progress window's (see `progress::Speed`), also
    /// zero once the data of a transfer has stopped coming.
    #[test]
    fn speed_comes_from_the_recent_reports() {
        let mut t = transfer(Some(Phase::Transferring), 0, 1000);
        let start = Instant::now();
        let later = start + Duration::from_secs(2);
        assert!(!t.note_progress(start, 0, 1000));
        t.note_progress(later, 200, 1000);
        assert_eq!(t.rate(later), Some(100));
        let stalled = later + crate::view::STALLED_AFTER;
        assert_eq!(t.rate(stalled), Some(0));
        t.phase = Some(Phase::Unpacking);
        assert_eq!(t.rate(stalled), Some(100), "this side's own work");
        // The first report sets the size.
        assert!(t.note_progress(later, 300, 2000));
        t.clear_rate();
        assert_eq!(t.rate(stalled), None);
    }

    /// Once all of it is done there is no speed, and none of zero either:
    /// the other side may be checking what arrived.
    #[test]
    fn a_transfer_with_nothing_to_come_has_no_speed() {
        let mut t = transfer(Some(Phase::Transferring), 0, 1000);
        let start = Instant::now();
        let later = start + Duration::from_secs(2);
        t.note_progress(start, 0, 1000);
        t.note_progress(later, 1000, 1000);
        assert_eq!(t.rate(later), None);
        assert_eq!(t.rate(later + crate::view::STALLED_AFTER), None);
    }

    /// Gives `t` a speed of `rate` bytes per second, as of a second after
    /// `start`.
    fn moving(t: &mut Transfer, start: Instant, rate: u64) {
        t.speed.note(start, 0);
        t.speed.note(start + Duration::from_secs(1), rate);
    }

    #[test]
    fn tray_tip_names_one_transfer_and_counts_several() {
        let start = Instant::now();
        let now = start + Duration::from_secs(1);
        assert_eq!(tray_tip(&[], now), APP_NAME);
        let mut one = transfer(Some(Phase::Transferring), 45, 100);
        let tip = tray_tip(&[&one], now);
        assert!(tip.starts_with(APP_NAME), "{tip}");
        assert!(tip.contains("report.pdf") && tip.contains("45"), "{tip}");
        assert!(!tip.contains("/s"), "{tip}");
        moving(&mut one, start, 2048);
        let tip = tray_tip(&[&one], now);
        assert!(tip.contains("/s") && tip.contains("2"), "{tip}");

        let mut two = transfer(Some(Phase::Transferring), 0, 0);
        let tip = tray_tip(&[&one, &two], now);
        assert!(tip.contains('2') && !tip.contains("report.pdf"), "{tip}");
        assert!(tip.contains("45%") || tip.contains("45 %"), "{tip}");
        two.total = 100;
        two.done = 99;
        moving(&mut two, start, 1024);
        let tip = tray_tip(&[&one, &two], now);
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
            (Some(Phase::PartInUse { finishing: false }), false),
            (Some(Phase::PartInUse { finishing: true }), false),
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
