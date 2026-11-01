//! Networking: an iroh endpoint running on a tokio runtime in a background
//! thread. The UI talks to it with [`Command`]s and receives [`Event`]s.
//!
//! A transfer is identified by a token on both sides. Once accepted, it has
//! a record on disk ([`session::TransferRecord`]) and survives lost
//! connections and restarts: the sender dials again, the receiver waits.

pub mod protocol;
mod receive;
mod send;
pub mod session;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iroh::{Endpoint, EndpointId, SecretKey, Watcher, endpoint::Connection, endpoint::presets};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use tokio::sync::{mpsc, oneshot, watch};

pub use crate::config::ReceivedFolder;
pub use protocol::{Kind, Offer};
use session::{Control, Fate, Session, Streams};
pub use session::{Role, TransferRecord};

pub enum Command {
    Send {
        id: u64,
        to: EndpointId,
        path: PathBuf,
        kind: Kind,
        my_name: String,
        /// The recipient's name as shown in the window, kept for a restart.
        peer_name: String,
    },
    /// Answer to an incoming offer: `Some` accepts it, `None` declines.
    Respond {
        id: u64,
        accept: Option<Accept>,
    },
    Cancel {
        id: u64,
    },
    Pause {
        id: u64,
    },
    Resume {
        id: u64,
    },
    /// Closes the transfer's connection as if the network had dropped it.
    #[cfg(test)]
    DropConnection {
        id: u64,
    },
    /// Suspends every transfer (it continues at the next start), closes the
    /// endpoint and then signals `done`.
    Shutdown {
        done: std::sync::mpsc::Sender<()>,
    },
}

/// How an accepted offer is saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accept {
    /// The folder the item is saved into.
    pub dir: PathBuf,
    /// For folders: what is kept of the archive. It is stored with the
    /// transfer, so a later change of the setting does not affect it.
    pub folder: ReceivedFolder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Connecting,
    WaitingForAccept,
    Transferring,
    /// Paused by the user on this side.
    Paused,
    /// Paused by the user on the other side.
    PausedByPeer,
    /// Not connected; the transfer continues when the connection is back.
    /// `lost` when the network dropped it, which the user may want to know
    /// about; not when the other side closed it on purpose (it is exiting),
    /// or when the transfer was just picked up after a restart.
    WaitingForConnection {
        lost: bool,
    },
    /// Receiving side: connected, but another program keeps the partly
    /// received file out; the transfer continues as soon as it lets go.
    PartInUse,
    /// Hashing data that was already transferred before continuing.
    Verifying,
    Unpacking,
    /// Sending side: all of the folder arrived, and the receiver is
    /// unpacking it. The transfer is complete once it has.
    PeerUnpacking,
}

#[derive(Debug)]
pub enum Outcome {
    /// Finished successfully; for received items, the final path.
    Completed(Option<PathBuf>),
    Declined,
    Cancelled,
    /// The other side cancelled.
    CancelledByPeer,
    Unreachable(String),
    Failed(String),
}

#[derive(Debug)]
pub enum Event {
    BindFailed(String),
    Online(bool),
    Offer {
        id: u64,
        from: EndpointId,
        offer: Offer,
    },
    /// An unfinished transfer from an earlier run; its phases follow.
    Restored {
        id: u64,
        /// Boxed: much larger than the other events.
        record: Box<TransferRecord>,
        /// Bytes already transferred.
        have: u64,
        /// Whether the transfer goes on. If not, `Finished` follows at once
        /// with what ended it.
        goes_on: bool,
    },
    Phase {
        id: u64,
        phase: Phase,
    },
    /// Sending side: the receiver has confirmed that all of the item arrived
    /// intact (also before a restart). The item is the receiver's now, and
    /// the transfer can no longer be cancelled here.
    HandedOver {
        id: u64,
    },
    /// Sending side: the user's cancel waits for the receiver's verdict on
    /// the data, which it may have all of (see `Session::defer_cancel`), and
    /// the transfer goes on until that comes: it ends as cancelled if the
    /// receiver asks for more data, and is handed over (`HandedOver`) if it
    /// has the item. Sent as soon as the cancel is known to wait, before it
    /// is on disk. Not sent for a cancel from the last run, whose record
    /// says so already (see `Restored`), nor when the cancel ends the
    /// transfer in a moment, nor when it came too late (`HandedOver`
    /// precedes such a cancel).
    CancelWaits {
        id: u64,
    },
    Progress {
        id: u64,
        done: u64,
        total: u64,
    },
    Path {
        id: u64,
        direct: bool,
    },
    Finished {
        id: u64,
        outcome: Outcome,
    },
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Allocates an id for a new transfer (used for both directions).
pub fn next_transfer_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

type Sink = Box<dyn Fn(Event) + Send + Sync>;

/// Cancellation signal usable from async code and from blocking threads.
#[derive(Clone)]
pub(crate) struct CancelToken {
    flag: Arc<AtomicBool>,
    tx: Arc<watch::Sender<bool>>,
}

impl CancelToken {
    fn new() -> Self {
        CancelToken {
            flag: Arc::new(AtomicBool::new(false)),
            tx: Arc::new(watch::channel(false).0),
        }
    }
    fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.tx.send_replace(true);
    }
    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.flag
    }
    pub(crate) async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}

pub(crate) struct Shared {
    sink: Sink,
    /// Where transfer records are kept.
    pub(crate) data_dir: PathBuf,
    /// This profile's folder for temporary archives (see
    /// `archive::temp_dir_for`); created on use.
    pub(crate) temp_dir: PathBuf,
    pub(crate) pending: Mutex<HashMap<u64, oneshot::Sender<Option<Accept>>>>,
    controls: Mutex<HashMap<u64, Arc<Control>>>,
    sessions: Mutex<Vec<Arc<Session>>>,
    /// Transfers from an earlier run whose tasks have not registered their
    /// sessions yet: a question about them must wait, not get "gone".
    restoring: Mutex<HashSet<protocol::Token>>,
    /// Set once shutdown has begun: transfers stop dialing, so that the
    /// other side sees one clean suspension and not a fresh connection that
    /// dies a moment later without explanation.
    pub(crate) shutting_down: AtomicBool,
}

impl Shared {
    pub(crate) fn emit(&self, event: Event) {
        (self.sink)(event)
    }

    /// The restored transfer's session is registered.
    pub(crate) fn restored(&self, token: &protocol::Token) {
        self.restoring.lock().unwrap().remove(token);
    }

    /// The session for `token`, waiting a little for one that is still being
    /// restored at startup.
    async fn session_soon(&self, token: &protocol::Token) -> Option<Arc<Session>> {
        for _ in 0..100 {
            if let Some(session) = self.session(token) {
                return Some(session);
            }
            if !self.restoring.lock().unwrap().contains(token) {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.session(token)
    }

    pub(crate) fn register(&self, id: u64, paused: bool) -> Arc<Control> {
        let ctl = Arc::new(Control::new(paused));
        self.controls.lock().unwrap().insert(id, ctl.clone());
        ctl
    }

    fn control(&self, id: u64) -> Option<Arc<Control>> {
        self.controls.lock().unwrap().get(&id).cloned()
    }

    pub(crate) fn add_session(&self, session: Arc<Session>) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|s| s.id != session.id);
        sessions.push(session);
    }

    pub(crate) fn session(&self, token: &protocol::Token) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.token == *token)
            .cloned()
    }

    pub(crate) fn session_by_id(&self, id: u64) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    pub(crate) fn finish(&self, id: u64, outcome: Outcome) {
        self.controls.lock().unwrap().remove(&id);
        self.pending.lock().unwrap().remove(&id);
        self.sessions.lock().unwrap().retain(|s| s.id != id);
        self.emit(Event::Finished { id, outcome });
    }
}

/// Finishes once the user has cancelled the transfer `id` and the transfer
/// has ended for it; the result deletes what is no longer needed. Never
/// finishes when the cancel is left to the transfer's task, which acts on it
/// at its next step, or comes too late, so that the transfer ends by itself
/// (see `Session::cancel`). Raced against a transfer only once its session
/// is registered, so that it always finds the session.
pub(crate) async fn cancelled(shared: &Shared, id: u64, ctl: &Control) -> session::Ending {
    ctl.cancel.cancelled().await;
    let Some(session) = shared.session_by_id(id) else {
        return session::Ending::nothing();
    };
    match session.cancel() {
        session::Cancel::Ended(ending) => return ending,
        session::Cancel::LeftToStep { to_save, waits } => {
            // The last step of a receive stops and ends the transfer soon,
            // and so does a send whose record was being saved for the first
            // time (see `send::start`). A send whose data has all gone out
            // waits for the receiver's verdict. The UI learns that at once:
            // what holds the transfer up meanwhile holds up the cancel, and
            // a phase that arrives before the save is done must not pass
            // for one the cancel ends in a moment. Not a cancel from the
            // last run, which the record told the UI already
            // (`Event::Restored`).
            if waits && to_save {
                shared.emit(Event::CancelWaits { id });
            }
            // A later start does not lose the cancel: it deletes the files of
            // a receive instead of finishing them, and cancels a send again.
            // Until the record is on disk, the save is tried again and
            // again (unless the transfer ends meanwhile), and shutdown waits
            // its moment for it. A cancel from the last run is on disk.
            if to_save {
                while let Some(Err(_)) = session
                    .update(&shared.data_dir, |s, d| s.save_record(d))
                    .await
                {
                    tokio::time::sleep(SAVE_RETRY).await;
                }
            }
            // Shutdown does not wait for the receiver's verdict once the
            // cancel is on disk.
            if waits {
                ctl.cancel_waits.store(true, Ordering::Relaxed);
            }
        }
        session::Cancel::TooLate => ctl.cancel_waits.store(true, Ordering::Relaxed),
    }
    std::future::pending().await
}

/// How long `cancelled` waits before it tries again to save a cancel that
/// could not be saved.
const SAVE_RETRY: Duration = Duration::from_millis(500);

/// Limits progress events to a few per second.
pub(crate) struct Throttle(Option<Instant>);

impl Throttle {
    pub(crate) fn new() -> Self {
        Throttle(None)
    }
    pub(crate) fn ready(&mut self) -> bool {
        let now = Instant::now();
        match self.0 {
            Some(last) if now.duration_since(last) < Duration::from_millis(100) => false,
            _ => {
                self.0 = Some(now);
                true
            }
        }
    }
}

/// Reports whether the connection currently uses a direct path or a relay.
pub(crate) fn spawn_path_watch(
    shared: Arc<Shared>,
    id: u64,
    conn: Connection,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = None;
        loop {
            let paths = conn.paths();
            let selected = paths.iter().find(|p| p.is_selected()).map(|p| p.is_ip());
            if let Some(direct) = selected
                && selected != last
            {
                last = selected;
                shared.emit(Event::Path { id, direct });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
}

pub struct NetHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl NetHandle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Suspends all transfers and closes connections so that peers learn
    /// about it, waiting at most `timeout`.
    pub fn shutdown(&self, timeout: Duration) {
        let (done, wait) = std::sync::mpsc::channel();
        self.send(Command::Shutdown { done });
        let _ = wait.recv_timeout(timeout);
    }
}

/// Starts the network thread. `sink` is called from network threads for
/// every event. `data_dir` holds the records of unfinished transfers and
/// `temp_dir` the temporary archives of folder transfers.
pub fn start(
    secret: SecretKey,
    data_dir: PathBuf,
    temp_dir: PathBuf,
    sink: impl Fn(Event) + Send + Sync + 'static,
) -> NetHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        sink: Box::new(sink),
        data_dir,
        temp_dir,
        pending: Mutex::new(HashMap::new()),
        controls: Mutex::new(HashMap::new()),
        sessions: Mutex::new(Vec::new()),
        restoring: Mutex::new(HashSet::new()),
        shutting_down: AtomicBool::new(false),
    });
    std::thread::Builder::new()
        .name("network".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(run(secret, shared, rx));
        })
        .expect("spawn network thread");
    NetHandle { tx }
}

async fn bind(secret: SecretKey) -> anyhow::Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![protocol::ALPN.to_vec()])
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;
    Ok(endpoint)
}

/// Handles a new connection: an offer, the continuation of an interrupted
/// transfer, or a question about one.
async fn accept(shared: Arc<Shared>, endpoint: Endpoint, conn: Connection) {
    use protocol::Msg;
    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
        return;
    };
    let first = tokio::time::timeout(Duration::from_secs(30), protocol::read_msg(&mut recv)).await;
    let Ok(Ok(first)) = first else { return };
    let peer = conn.remote_id();
    // Short answers, after which the connection is done.
    let answer = match first {
        Msg::Offer(offer) => {
            receive::handle(shared, endpoint, conn, send, recv, offer).await;
            return;
        }
        Msg::Continue { token } => {
            if shared.shutting_down.load(Ordering::SeqCst) {
                // Exiting: nothing continues now; the sender tries again later.
                conn.close(CLOSE_SUSPENDED.into(), b"exiting");
                return;
            }
            let session = shared
                .session_soon(&token)
                .await
                .filter(|s| s.role == Role::Receive && s.peer == peer);
            match session {
                Some(session) => {
                    // The transfer's task picks the new connection up; an old
                    // one it may still hold is dead from the sender's point of view.
                    session.close_current(CLOSE_SUSPENDED, b"replaced");
                    let _ = session.new_conn.send(Streams { conn, send, recv });
                    return;
                }
                None => Msg::Gone {
                    completed: session::tombstone(&shared.data_dir, &token) == Some(true),
                },
            }
        }
        Msg::Query { token } => {
            let alive = shared
                .session_soon(&token)
                .await
                .is_some_and(|s| s.role == Role::Send && s.peer == peer);
            if alive {
                Msg::Alive
            } else {
                Msg::Gone {
                    completed: session::tombstone(&shared.data_dir, &token) == Some(true),
                }
            }
        }
        _ => return,
    };
    let _ = protocol::write_msg(&mut send, &answer).await;
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    conn.close(CLOSE_OK.into(), b"done");
}

/// Picks up the transfers that were interrupted in an earlier run.
async fn restore(shared: &Arc<Shared>, endpoint: &Endpoint) {
    for (id, record, ctl) in restore_records(shared).await {
        let shared = shared.clone();
        let endpoint = endpoint.clone();
        let token = record.token;
        tokio::spawn(async move {
            let outcome = match record.role() {
                Role::Send => send::resume(&shared, &endpoint, id, record, ctl).await,
                Role::Receive => receive::resume(&shared, &endpoint, id, record, ctl).await,
            };
            // However it ended (also before the session was registered):
            // questions about it must not wait any longer.
            shared.restored(&token);
            shared.finish(id, outcome);
        });
    }
}

/// What [`restore`] does before the transfers that go on can run: reads
/// the records of the last run, deletes in the background the files of the
/// transfers that are over, reports those that expired or cannot go on,
/// and returns the transfers that go on, each with its id and its
/// registered `Control`.
async fn restore_records(shared: &Arc<Shared>) -> Vec<(u64, TransferRecord, Arc<Control>)> {
    // Reading the records and looking at their files is file work: each
    // transfer that goes on with how many bytes it has and whether it can
    // go on.
    let (going_on, over) = {
        let (data_dir, temp_dir) = (shared.data_dir.clone(), shared.temp_dir.clone());
        session::off_runtime(move || {
            let found = session::load_all(&data_dir);
            let all = found
                .going_on
                .iter()
                .chain(found.over.iter().map(|(r, _)| r));
            session::cleanup_temp(&temp_dir, all);
            let going_on = found
                .going_on
                .into_iter()
                .map(|record| (record.have(), record.can_go_on(), record))
                .collect::<Vec<_>>();
            (going_on, found.over)
        })
        .await
    };
    // Known before any task runs, so that an early `Continue` or `Query`
    // from the other side waits instead of being answered with "gone".
    shared
        .restoring
        .lock()
        .unwrap()
        .extend(going_on.iter().map(|(.., record)| record.token));
    // Deleting the files of a transfer that is over can take a while, and
    // nothing waits for it.
    fn in_background(shared: &Shared, work: impl FnOnce(&Path) + Send + 'static) {
        let data_dir = shared.data_dir.clone();
        tokio::task::spawn_blocking(move || work(&data_dir));
    }
    // A transfer of the last run that does not go on is reported like one
    // that does, and fails at once.
    let report_failed = |record: &TransferRecord, have: u64, error: &str| {
        let id = next_transfer_id();
        shared.emit(Event::Restored {
            id,
            record: Box::new(record.clone()),
            have,
            goes_on: false,
        });
        shared.emit(Event::Finished {
            id,
            outcome: Outcome::Failed(error.into()),
        });
    };
    for (record, fate) in over {
        match fate {
            Fate::Ended => in_background(shared, move |dir| record.discard(dir)),
            Fate::GivenUp => in_background(shared, move |dir| record.give_up(dir)),
            // `load_all` has ended it, with its files.
            Fate::Expired => {
                let error = if record.receiver_may_have_item() {
                    crate::i18n::tr!(error_expired_receiver_may_have)
                } else {
                    crate::i18n::tr!(error_expired)
                };
                report_failed(&record, 0, error);
            }
        }
    }
    let mut resumed = Vec::new();
    for (have, present, record) in going_on {
        if !present {
            report_failed(&record, have, crate::i18n::tr!(error_part_missing));
            shared.restored(&record.token);
            in_background(shared, move |dir| record.discard(dir));
            continue;
        }
        let id = next_transfer_id();
        shared.emit(Event::Restored {
            id,
            record: Box::new(record.clone()),
            have,
            goes_on: true,
        });
        let ctl = shared.register(id, record.paused);
        // The user cancelled a send in the last run, and the cancel was
        // still waiting for the receiver's verdict: it waits on. Such a
        // record is not paused (see `TransferRecord::cancelled`).
        if record.cancel_pending() {
            ctl.cancel();
        }
        resumed.push((id, record, ctl));
    }
    resumed
}

async fn run(secret: SecretKey, shared: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<Command>) {
    let endpoint = loop {
        match bind(secret.clone()).await {
            Ok(ep) => break ep,
            Err(e) => {
                shared.emit(Event::BindFailed(e.to_string()));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    // Online indicator: connected to the home relay server.
    {
        let endpoint = endpoint.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut status = endpoint.home_relay_status();
            let mut last = None;
            loop {
                let online = status.get().iter().any(|s| s.is_connected());
                if last != Some(online) {
                    last = Some(online);
                    shared.emit(Event::Online(online));
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    // The transfers of the last run, before anyone can ask about them.
    restore(&shared, &endpoint).await;

    // Incoming connections.
    {
        let endpoint = endpoint.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let shared = shared.clone();
                let endpoint = endpoint.clone();
                tokio::spawn(async move {
                    let Ok(accepting) = incoming.accept() else {
                        return;
                    };
                    let Ok(conn) = accepting.await else { return };
                    accept(shared, endpoint, conn).await;
                });
            }
        });
    }

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Send {
                id,
                to,
                path,
                kind,
                my_name,
                peer_name,
            } => {
                let ctl = shared.register(id, false);
                let shared = shared.clone();
                let endpoint = endpoint.clone();
                tokio::spawn(async move {
                    let outcome = send::run(
                        &shared, &endpoint, id, to, path, kind, my_name, peer_name, ctl,
                    )
                    .await;
                    shared.finish(id, outcome);
                });
            }
            Command::Respond { id, accept } => {
                if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(accept);
                }
            }
            Command::Cancel { id } => {
                if let Some(ctl) = shared.control(id) {
                    ctl.cancel();
                }
            }
            Command::Pause { id } => {
                if let Some(ctl) = shared.control(id) {
                    ctl.paused.send_replace(true);
                }
            }
            Command::Resume { id } => {
                if let Some(ctl) = shared.control(id) {
                    ctl.paused.send_replace(false);
                }
            }
            #[cfg(test)]
            Command::DropConnection { id } => {
                if let Some(session) = shared.session_by_id(id) {
                    session.close_current(CLOSE_SUSPENDED, b"dropped");
                }
            }
            Command::Shutdown { done } => {
                // No more dialing from now on (see `shutting_down`).
                shared.shutting_down.store(true, Ordering::SeqCst);
                // Transfers that were just cancelled need a moment to tell
                // their peer and to clean up; the rest is suspended, also
                // those whose cancel came too late or waits for the
                // receiver's verdict (once it is in their record).
                let deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < deadline {
                    let cancelling = shared
                        .controls
                        .lock()
                        .unwrap()
                        .values()
                        .any(|c| c.cancelling());
                    if !cancelling {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                // Let the close reach the peers, and let receivers get what
                // they have onto the disk and into their records: the next
                // start continues from there.
                let settle = Instant::now() + Duration::from_millis(200);
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    // Every pass, so that a connection made meanwhile (a dial
                    // that was already under way) ends the same way and not
                    // with the endpoint, which would look like a loss.
                    let sessions: Vec<_> = shared.sessions.lock().unwrap().clone();
                    for session in &sessions {
                        session.close_current(CLOSE_SUSPENDED, b"exiting");
                    }
                    let syncing = sessions.iter().any(|s| s.syncing.load(Ordering::Relaxed));
                    let now = Instant::now();
                    if (!syncing && now >= settle) || now >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                endpoint.close().await;
                let _ = done.send(());
                return;
            }
        }
    }
}

/// Application error codes used when closing connections.
pub(crate) const CLOSE_OK: u32 = 0;
/// The user on that side cancelled the transfer.
pub(crate) const CLOSE_CANCELLED: u32 = 1;
/// The transfer failed on that side (for example, the file could not be
/// written), so the peer reports a failure rather than a cancellation.
pub(crate) const CLOSE_FAILED: u32 = 2;
/// That side is going away (exiting, or replacing the connection); the
/// transfer continues later, like after a lost connection.
pub(crate) const CLOSE_SUSPENDED: u32 = 3;

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::SystemTime};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dodavo-net-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn wait_finished(rx: &mpsc::Receiver<Event>, id: u64) -> Outcome {
        loop {
            match rx
                .recv_timeout(Duration::from_secs(120))
                .expect("timed out")
            {
                Event::Finished { id: i, outcome } if i == id => return outcome,
                _ => {}
            }
        }
    }

    /// Waits until `pred` accepts an event (events before it are dropped).
    fn wait_for(rx: &mpsc::Receiver<Event>, mut pred: impl FnMut(&Event) -> bool) -> Event {
        loop {
            let e = rx
                .recv_timeout(Duration::from_secs(120))
                .expect("timed out");
            if pred(&e) {
                return e;
            }
        }
    }

    fn sink(tx: mpsc::Sender<Event>) -> impl Fn(Event) + Send + Sync + 'static {
        let tx = Mutex::new(tx);
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len as u32).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn leftovers(dir: &PathBuf) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect()
    }

    /// Two endpoints in one process: sends a file (pausing on both sides on
    /// the way) and a folder from A to B, then checks that a declined offer
    /// is reported to the sender.
    #[test]
    fn end_to_end() {
        let base = scratch("e2e");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let a = start(
            SecretKey::generate(),
            base.join("a"),
            base.join("temp"),
            sink(tx_a),
        );
        let b = Arc::new(start(key_b, base.join("b"), base.join("temp"), sink(tx_b)));

        // Receiver: accept everything except offers named "decline.txt" (the
        // folder "Kept" also keeps its zip), and pause the first transfer
        // once for a moment.
        let recv_dir = base.join("received");
        let b_thread = {
            let recv_dir = recv_dir.clone();
            let b = b.clone();
            std::thread::spawn(move || {
                let mut done = Vec::new();
                let mut phases = Vec::new();
                let mut paused_once = false;
                while done.len() < 4 {
                    match rx_b
                        .recv_timeout(Duration::from_secs(120))
                        .expect("receiver timed out")
                    {
                        Event::Offer { id, offer, .. } => {
                            let folder = if offer.name == "Kept" {
                                ReceivedFolder::Both
                            } else {
                                ReceivedFolder::Extract
                            };
                            let accept = (offer.name != "decline.txt").then(|| Accept {
                                dir: recv_dir.clone(),
                                folder,
                            });
                            b.send(Command::Respond { id, accept });
                        }
                        Event::Progress { id, done, .. } if done > 0 && !paused_once => {
                            paused_once = true;
                            b.send(Command::Pause { id });
                            std::thread::sleep(Duration::from_millis(300));
                            b.send(Command::Resume { id });
                        }
                        Event::Phase { phase, .. } => phases.push(phase),
                        Event::Finished { outcome, .. } => done.push(outcome),
                        _ => {}
                    }
                }
                (done, phases)
            })
        };

        // A file, paused first by the receiver and then by the sender. Its
        // dates are kept.
        let file = base.join("hello.bin");
        let data = pattern(30_000_000);
        std::fs::write(&file, &data).unwrap();
        let file_modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        let file_created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_400_000_000);
        crate::archive::set_times(&file, Some(file_modified), Some(file_created)).unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: file,
            kind: Kind::File,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        wait_for(&rx_a, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::PausedByPeer,
                    ..
                }
            )
        });
        wait_for(&rx_a, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::Transferring,
                    ..
                }
            )
        });
        a.send(Command::Pause { id });
        wait_for(&rx_a, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::Paused,
                    ..
                }
            )
        });
        std::thread::sleep(Duration::from_millis(300));
        a.send(Command::Resume { id });
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Completed(_)));
        assert_eq!(std::fs::read(recv_dir.join("hello.bin")).unwrap(), data);

        // A folder, unpacked, with the dates of its files and of itself.
        let folder = base.join("My Folder");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::write(folder.join("a.txt"), b"alpha").unwrap();
        std::fs::write(folder.join("sub").join("b.txt"), b"beta").unwrap();
        let inner_modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_300_000_000);
        crate::archive::set_times(&folder.join("a.txt"), Some(inner_modified), None).unwrap();
        crate::archive::set_times(&folder, Some(inner_modified), None).unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: folder,
            kind: Kind::Folder,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Completed(_)));

        // A folder that is unpacked and kept as a zip too.
        let kept = base.join("Kept");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("c.txt"), b"gamma").unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: kept,
            kind: Kind::Folder,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Completed(_)));

        // A declined offer.
        let declined = base.join("decline.txt");
        std::fs::write(&declined, b"no").unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: declined,
            kind: Kind::File,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Declined));

        let (results, phases) = b_thread.join().unwrap();
        assert_eq!(
            results
                .iter()
                .filter(|o| matches!(o, Outcome::Completed(_)))
                .count(),
            3
        );
        assert!(phases.contains(&Phase::PausedByPeer), "{phases:?}");
        assert!(phases.contains(&Phase::Paused), "{phases:?}");
        assert_eq!(
            std::fs::read(recv_dir.join("My Folder").join("sub").join("b.txt")).unwrap(),
            b"beta"
        );
        assert_eq!(
            std::fs::read(recv_dir.join("My Folder").join("a.txt")).unwrap(),
            b"alpha"
        );
        // The dates arrived, and received files carry the mark of the web.
        let meta = std::fs::metadata(recv_dir.join("hello.bin")).unwrap();
        assert_eq!(meta.modified().unwrap(), file_modified);
        assert_eq!(meta.created().unwrap(), file_created);
        let mark = std::fs::read_to_string(recv_dir.join("hello.bin:Zone.Identifier")).unwrap();
        assert!(mark.contains("ZoneId=3"), "{mark}");
        let modified = |p: PathBuf| std::fs::metadata(p).unwrap().modified().unwrap();
        let folder = recv_dir.join("My Folder");
        assert_eq!(modified(folder.join("a.txt")), inner_modified);
        assert_eq!(modified(folder.clone()), inner_modified);
        assert!(folder.join("a.txt:Zone.Identifier").exists());
        // "Kept" is there twice: unpacked, and as its zip.
        assert_eq!(
            std::fs::read(recv_dir.join("Kept").join("c.txt")).unwrap(),
            b"gamma"
        );
        assert!(recv_dir.join("Kept.zip").is_file());
        assert!(recv_dir.join("Kept.zip:Zone.Identifier").exists());
        assert!(!recv_dir.join("decline.txt").exists());
        let left = leftovers(&recv_dir);
        assert_eq!(left.len(), 4, "unexpected files: {left:?}");
        assert!(session::records(&base.join("a")).is_empty());
        assert!(session::records(&base.join("b")).is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    /// The sender's connection is dropped in the middle; the transfer
    /// continues on a new one and the file is still intact.
    #[test]
    fn resume_after_disconnect() {
        let base = scratch("drop");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let a = start(
            SecretKey::generate(),
            base.join("a"),
            base.join("temp"),
            sink(tx_a),
        );
        let b = Arc::new(start(key_b, base.join("b"), base.join("temp"), sink(tx_b)));
        let recv_dir = base.join("received");
        let b_thread = {
            let recv_dir = recv_dir.clone();
            let b = b.clone();
            std::thread::spawn(move || {
                let mut phases = Vec::new();
                loop {
                    match rx_b
                        .recv_timeout(Duration::from_secs(120))
                        .expect("timed out")
                    {
                        Event::Offer { id, .. } => b.send(Command::Respond {
                            id,
                            accept: Some(Accept {
                                dir: recv_dir.clone(),
                                folder: ReceivedFolder::Extract,
                            }),
                        }),
                        Event::Phase { phase, .. } => phases.push(phase),
                        Event::Finished { outcome, .. } => return (outcome, phases),
                        _ => {}
                    }
                }
            })
        };
        let file = base.join("big.bin");
        let data = pattern(40_000_000);
        std::fs::write(&file, &data).unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: file,
            kind: Kind::File,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        wait_for(
            &rx_a,
            |e| matches!(e, Event::Progress { done, .. } if *done > 4_000_000),
        );
        a.send(Command::DropConnection { id });
        let mut phases = Vec::new();
        let outcome = loop {
            match rx_a
                .recv_timeout(Duration::from_secs(120))
                .expect("timed out")
            {
                Event::Phase { phase, .. } => phases.push(phase),
                Event::Finished { id: i, outcome } if i == id => break outcome,
                _ => {}
            }
        };
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        assert!(
            phases
                .iter()
                .any(|p| matches!(p, Phase::WaitingForConnection { .. })),
            "{phases:?}"
        );
        // The receiver usually gets the replacement connection within its
        // grace period and never shows the interruption; it must not fail.
        let (outcome_b, phases_b) = b_thread.join().unwrap();
        assert!(matches!(outcome_b, Outcome::Completed(_)), "{outcome_b:?}");
        assert!(
            phases_b
                .iter()
                .filter(|p| **p == Phase::Transferring)
                .count()
                >= 2,
            "{phases_b:?}"
        );
        assert_eq!(std::fs::read(recv_dir.join("big.bin")).unwrap(), data);
        assert_eq!(leftovers(&recv_dir).len(), 1);
        std::fs::remove_dir_all(&base).ok();
    }

    /// The receiver is shut down in the middle and started again with the
    /// same identity and data folder: the transfer is restored and continues
    /// from what was already written.
    #[test]
    fn resume_after_restart() {
        let base = scratch("restart");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let dir_b = base.join("b");
        let a = start(
            SecretKey::generate(),
            base.join("a"),
            base.join("temp"),
            sink(tx_a),
        );
        let b = start(key_b.clone(), dir_b.clone(), base.join("temp"), sink(tx_b));
        let recv_dir = base.join("received");

        let file = base.join("big.bin");
        let data = pattern(40_000_000);
        std::fs::write(&file, &data).unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: file,
            kind: Kind::File,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        let Event::Offer { id: id_in, .. } = wait_for(&rx_b, |e| matches!(e, Event::Offer { .. }))
        else {
            unreachable!()
        };
        b.send(Command::Respond {
            id: id_in,
            accept: Some(Accept {
                dir: recv_dir.clone(),
                folder: ReceivedFolder::Extract,
            }),
        });
        wait_for(
            &rx_b,
            |e| matches!(e, Event::Progress { done, .. } if *done > 4_000_000),
        );
        b.shutdown(Duration::from_secs(5));
        drop(b);
        let records = session::records(&dir_b);
        assert_eq!(records.len(), 1, "the record survives the shutdown");
        let written = records[0].have();
        assert!(written > 0);

        wait_for(&rx_a, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::WaitingForConnection { .. },
                    ..
                }
            )
        });
        let (tx_b2, rx_b2) = mpsc::channel();
        let b2 = start(key_b, dir_b.clone(), base.join("temp"), sink(tx_b2));
        let Event::Restored {
            id: id_b2, have, ..
        } = wait_for(&rx_b2, |e| matches!(e, Event::Restored { .. }))
        else {
            unreachable!()
        };
        assert_eq!(have, written);
        // The part file is hashed again, then the rest arrives.
        wait_for(&rx_b2, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::Verifying,
                    ..
                }
            )
        });
        let Event::Progress { done: first, .. } = wait_for(
            &rx_b2,
            |e| matches!(e, Event::Progress { id, total, .. } if *id == id_b2 && *total == data.len() as u64),
        ) else {
            unreachable!()
        };
        assert!(
            first >= written,
            "continues from {written}, not from {first}"
        );
        assert!(matches!(
            wait_finished(&rx_b2, id_b2),
            Outcome::Completed(_)
        ));
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Completed(_)));
        assert_eq!(std::fs::read(recv_dir.join("big.bin")).unwrap(), data);
        assert_eq!(leftovers(&recv_dir).len(), 1);
        assert!(session::records(&dir_b).is_empty());
        drop(b2);
        std::fs::remove_dir_all(&base).ok();
    }

    /// The receiver is stopped after all data has arrived, while it is still
    /// unpacking the folder, and started again: it finishes on its own from
    /// the hash the sender gave it. The sender waits for that (a cancel there
    /// comes too late once the receiver has confirmed the data), and learns
    /// that the item was saved when it dials again.
    #[test]
    fn finish_alone_after_restart_during_unpacking() {
        let base = scratch("finish-alone");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let dir_b = base.join("b");
        let a = start(
            SecretKey::generate(),
            base.join("a"),
            base.join("temp"),
            sink(tx_a),
        );
        let b = start(key_b.clone(), dir_b.clone(), base.join("temp"), sink(tx_b));
        let recv_dir = base.join("received");

        let folder = base.join("Photos");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        let data = pattern(3_000_000);
        std::fs::write(folder.join("a.bin"), &data).unwrap();
        std::fs::write(folder.join("sub").join("b.txt"), b"beta").unwrap();
        receive::HOLD_FINALIZE.store(true, Ordering::SeqCst);
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: folder,
            kind: Kind::Folder,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        let Event::Offer { id: id_in, .. } = wait_for(&rx_b, |e| matches!(e, Event::Offer { .. }))
        else {
            unreachable!()
        };
        b.send(Command::Respond {
            id: id_in,
            accept: Some(Accept {
                dir: recv_dir.clone(),
                folder: ReceivedFolder::Extract,
            }),
        });
        // The sender waits while the receiver unpacks; cancelling there is
        // too late…
        wait_for(&rx_a, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::PeerUnpacking,
                    ..
                }
            )
        });
        a.send(Command::Cancel { id });
        // …while the receiver is held right before unpacking: stop it there.
        wait_for(&rx_b, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::Unpacking,
                    ..
                }
            )
        });
        b.shutdown(Duration::from_secs(5));
        drop(b);
        receive::HOLD_FINALIZE.store(false, Ordering::SeqCst);
        let records = session::records(&dir_b);
        assert_eq!(records.len(), 1, "the record survives");
        assert!(
            records[0].done_hash.is_some(),
            "the sender's hash was saved"
        );
        assert!(!recv_dir.join("Photos").exists());
        // As if the program had ended halfway through unpacking: a folder
        // with part of the contents, which the record names.
        let partial = recv_dir.join("Photos");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("a.bin"), &data[..1000]).unwrap();
        let mut record = records[0].clone();
        if let session::Side::Receive { files, .. } = &mut record.side {
            files.unpacking = Some(session::Owned::at(&partial).unwrap());
        }
        record.save(&dir_b).unwrap();

        let (tx_b2, rx_b2) = mpsc::channel();
        let b2 = start(key_b, dir_b.clone(), base.join("temp"), sink(tx_b2));
        let Event::Restored { id: id_b2, .. } =
            wait_for(&rx_b2, |e| matches!(e, Event::Restored { .. }))
        else {
            unreachable!()
        };
        let outcome = wait_finished(&rx_b2, id_b2);
        assert!(
            matches!(outcome, Outcome::Completed(Some(_))),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read(recv_dir.join("Photos").join("sub").join("b.txt")).unwrap(),
            b"beta"
        );
        assert_eq!(
            std::fs::read(recv_dir.join("Photos").join("a.bin")).unwrap(),
            data
        );
        // The incomplete folder was replaced, not left next to a copy.
        assert_eq!(leftovers(&recv_dir), ["Photos"]);
        assert!(session::records(&dir_b).is_empty());
        // The sender learns on its next dial that the item was saved.
        let outcome = wait_finished(&rx_a, id);
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        assert!(session::records(&base.join("a")).is_empty());
        drop(b2);
        std::fs::remove_dir_all(&base).ok();
    }

    /// A record whose transfer had ended right after its part was renamed
    /// follows the part, so that ending it deletes the renamed part and not
    /// another transfer's part that has the old name by now.
    #[test]
    fn an_ended_record_follows_its_moved_part() {
        let dir = scratch("ended-moved");
        let part = dir.join("Photos.zip.part");
        std::fs::write(&part, b"zip").unwrap();
        let record = TransferRecord {
            token: [5; 16],
            peer: "ab".repeat(32),
            offer: Offer {
                kind: Kind::Folder,
                name: "Photos".into(),
                size: 3,
                file_count: 1,
                content_size: 3,
                sender_name: "Ana".into(),
                token: [5; 16],
                modified: None,
                created: None,
            },
            peer_name: "Ana".into(),
            paused: false,
            created: SystemTime::now(),
            side: session::Side::Receive {
                dest_dir: dir.clone(),
                files: session::ReceiveFiles {
                    moving: Some(session::Owned {
                        path: dir.join("Photos (2).zip"),
                        id: crate::archive::file_id(&part).unwrap(),
                    }),
                    ..session::ReceiveFiles::new(part.clone())
                },
                part_id: Some(crate::archive::file_id(&part).unwrap()),
                synced: Some(3),
                folder: ReceivedFolder::Both,
            },
            done_hash: Some([0; 32]),
            done_sent: false,
            cancelled: false,
            ended: true,
            ended_at: None,
        };
        record.save(&dir).unwrap();
        std::fs::rename(&part, dir.join("Photos (2).zip")).unwrap();
        std::fs::write(&part, b"another transfer").unwrap();
        let mut loaded = session::records(&dir);
        assert_eq!(loaded.len(), 1);
        loaded.remove(0).discard(&dir);
        assert_eq!(std::fs::read(&part).unwrap(), b"another transfer");
        assert!(!dir.join("Photos (2).zip").exists());
        assert!(session::records(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What a start does with the records of the last run before the
    /// transfers that go on run (`restore_records`): one that goes on is
    /// registered and returned; one whose files are gone and one that
    /// expired are reported, and fail at once (the expired one is ended
    /// with its files before anything goes on); the files of one that
    /// ended are deleted in the background, and its record stays while one
    /// of them cannot be; those of one that ended long ago are tried once
    /// more, and its record goes however that went. The UI hears nothing of
    /// the ended ones.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_start_sorts_out_the_records_of_the_last_run() {
        let dir = scratch("restore-records");
        let (shared, events) = recording_shared(&dir);
        let peer = SecretKey::generate().public().to_string();
        let record_file = |token: u8| {
            session::records_dir(&dir).join(format!("{}.json", protocol::token_hex(&[token; 16])))
        };
        let receive = |token: u8, name: &str| {
            let part = dir.join(name);
            std::fs::write(&part, b"abc").unwrap();
            let side = session::Side::Receive {
                dest_dir: dir.clone(),
                files: session::ReceiveFiles::new(part.clone()),
                part_id: Some(crate::archive::file_id(&part).unwrap()),
                synced: Some(3),
                folder: ReceivedFolder::Extract,
            };
            (file_record(token, &peer, 3, side), part)
        };
        let hold = |path: &std::path::Path| {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
                .unwrap()
        };
        let month_ago = SystemTime::now() - Duration::from_secs(31 * 24 * 60 * 60);
        let make_old = |token: u8| {
            std::fs::File::options()
                .write(true)
                .open(record_file(token))
                .unwrap()
                .set_modified(month_ago)
                .unwrap();
        };

        let (on, on_part) = receive(1, "on.part");
        on.save(&dir).unwrap();
        let (missing, missing_part) = receive(2, "missing.part");
        missing.save(&dir).unwrap();
        std::fs::remove_file(&missing_part).unwrap();
        let (expired, expired_part) = receive(3, "expired.part");
        expired.save(&dir).unwrap();
        make_old(3);
        // It was being unpacked; its part cannot be deleted now.
        let (mut ended, ended_part) = receive(4, "ended.part");
        let ended_folder = dir.join("ended");
        std::fs::create_dir_all(&ended_folder).unwrap();
        std::fs::write(ended_folder.join("one.txt"), b"1").unwrap();
        ended.ended = true;
        ended.ended_at = Some(SystemTime::now());
        if let session::Side::Receive { files, .. } = &mut ended.side {
            files.unpacking = Some(session::Owned::at(&ended_folder).unwrap());
        }
        ended.save(&dir).unwrap();
        let held_ended = hold(&ended_part);
        let (mut given_up, given_up_part) = receive(5, "given-up.part");
        given_up.ended = true;
        given_up.ended_at = Some(month_ago);
        given_up.save(&dir).unwrap();
        let held = hold(&given_up_part);
        // A send whose data had all gone out: the receiver may have it.
        let source = dir.join("sent.bin");
        std::fs::write(&source, b"abc").unwrap();
        let mut sent = file_record(
            6,
            &peer,
            3,
            session::Side::Send {
                source: source.clone(),
                source_len: 3,
                source_mtime: None,
                data_path: source.clone(),
            },
        );
        sent.done_sent = true;
        sent.save(&dir).unwrap();
        make_old(6);
        session::started_before(&dir);

        let resumed = restore_records(&shared).await;
        assert_eq!(resumed.len(), 1);
        let (id, record, _) = &resumed[0];
        assert_eq!(record.token, on.token);
        assert!(shared.control(*id).is_some(), "registered");
        assert!(on_part.exists() && record_file(1).exists());
        assert!(
            !expired_part.exists() && !record_file(3).exists(),
            "ended before anything goes on"
        );
        assert!(source.exists(), "what the user chose to send stays");
        assert!(!record_file(6).exists());

        let events = std::mem::take(&mut *events.lock().unwrap());
        let restored = events
            .iter()
            .filter_map(|event| match event {
                Event::Restored {
                    id,
                    record,
                    goes_on,
                    ..
                } => Some((record.token[0], (*id, *goes_on))),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut tokens = restored.keys().copied().collect::<Vec<_>>();
        tokens.sort();
        assert_eq!(tokens, [1, 2, 3, 6], "nothing of the ended ones");
        assert_eq!(restored[&1], (*id, true));
        let failed = |token: u8| {
            let (id, goes_on) = restored[&token];
            assert!(!goes_on);
            events
                .iter()
                .find_map(|event| match event {
                    Event::Finished {
                        id: finished,
                        outcome: Outcome::Failed(error),
                    } if *finished == id => Some(error.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(failed(2), crate::i18n::tr!(error_part_missing));
        assert_eq!(failed(3), crate::i18n::tr!(error_expired));
        assert_eq!(failed(6), crate::i18n::tr!(error_expired_receiver_may_have));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Finished { .. }))
                .count(),
            3
        );

        // In the background.
        let deadline = Instant::now() + Duration::from_secs(10);
        // The ended one names only its part then, which the next start tries.
        let ended_left = || {
            std::fs::read_to_string(record_file(4))
                .ok()
                .and_then(|text| serde_json::from_str::<TransferRecord>(&text).ok())
                .filter(|left| left.receive_files().unwrap().unpacking.is_none())
        };
        let left = loop {
            if let Some(left) = ended_left()
                && ![2, 5].iter().any(|&token| record_file(token).exists())
            {
                break left;
            }
            assert!(Instant::now() < deadline, "the background work never ended");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(left.ended && !ended_folder.exists());
        assert!(ended_part.exists() && left.part_is_there());
        assert!(given_up_part.exists(), "still held, and given up");
        drop(held);
        drop(held_ended);
        assert!(on_part.exists() && record_file(1).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A record of a file transfer with `peer` (a Contact ID) and `side`.
    fn file_record(token: u8, peer: &str, size: u64, side: session::Side) -> TransferRecord {
        TransferRecord {
            token: [token; 16],
            peer: peer.into(),
            offer: Offer {
                kind: Kind::File,
                name: "a.bin".into(),
                size,
                file_count: 1,
                content_size: size,
                sender_name: "Ana".into(),
                token: [token; 16],
                modified: None,
                created: None,
            },
            peer_name: "Ana".into(),
            paused: false,
            created: SystemTime::now(),
            side,
            done_hash: None,
            done_sent: false,
            cancelled: false,
            ended: false,
            ended_at: None,
        }
    }

    /// A `Shared` that keeps the events it emits for the test to look at.
    fn recording_shared(dir: &std::path::Path) -> (Arc<Shared>, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let shared = Arc::new(Shared {
            sink: Box::new(move |event| sink.lock().unwrap().push(event)),
            data_dir: dir.to_path_buf(),
            temp_dir: dir.join("temp"),
            pending: Mutex::new(HashMap::new()),
            controls: Mutex::new(HashMap::new()),
            sessions: Mutex::new(Vec::new()),
            restoring: Mutex::new(HashSet::new()),
            shutting_down: AtomicBool::new(false),
        });
        (shared, events)
    }

    /// Whether `Event::CancelWaits` was emitted for the transfer `id`.
    fn cancel_waits_told(events: &Mutex<Vec<Event>>, id: u64) -> bool {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, Event::CancelWaits { id: told } if *told == id))
    }

    /// A cancel of a send that waits for the receiver's verdict is on disk
    /// before shutdown stops waiting for it, so that the next start cancels
    /// the transfer again, and the UI is told that it waits; a cancel left
    /// to the last step of a receive, which ends the transfer soon, is
    /// waited for, and the UI is not told that it waits.
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_waits_for_a_cancel_until_it_is_on_disk() {
        let dir = scratch("cancel-waits");
        let (shared, events) = recording_shared(&dir);
        let peer = SecretKey::generate().public();
        let zip = dir.join("send.zip");
        std::fs::write(&zip, b"zip").unwrap();
        let mut record = file_record(
            1,
            &peer.to_string(),
            3,
            session::Side::Send {
                source: dir.join("Photos"),
                source_len: 3,
                source_mtime: None,
                data_path: zip,
            },
        );
        record.done_sent = true;
        record.save(&dir).unwrap();
        let (send, _rx) = Session::new(1, record.token, peer, Role::Send);
        send.set_record(record);
        send.defer_cancel();
        shared.add_session(send.clone());
        let ctl = shared.register(1, false);
        ctl.cancel();
        assert!(ctl.cancelling(), "shutdown waits");
        let waited =
            tokio::time::timeout(Duration::from_secs(1), cancelled(&shared, 1, &ctl)).await;
        assert!(waited.is_err(), "the transfer goes on");
        assert!(!ctl.cancelling(), "shutdown no longer waits");
        assert!(session::records(&dir)[0].cancel_pending());
        assert!(cancel_waits_told(&events, 1), "the UI is told");

        let part = dir.join("b.bin.part");
        std::fs::write(&part, b"abc").unwrap();
        let record = file_record(
            2,
            &peer.to_string(),
            3,
            session::Side::Receive {
                dest_dir: dir.clone(),
                files: session::ReceiveFiles::new(part),
                part_id: None,
                synced: Some(3),
                folder: ReceivedFolder::Extract,
            },
        );
        let (receive, _rx) = Session::new(2, record.token, peer, Role::Receive);
        receive.set_record(record);
        let _finishing = receive.finishing().unwrap();
        shared.add_session(receive.clone());
        let ctl = shared.register(2, false);
        ctl.cancel();
        let waited =
            tokio::time::timeout(Duration::from_secs(1), cancelled(&shared, 2, &ctl)).await;
        assert!(waited.is_err(), "left to the last step");
        assert!(ctl.cancelling(), "which ends the transfer soon");
        assert!(!cancel_waits_told(&events, 2), "nothing to tell the UI");
        let stored = session::records(&dir);
        assert!(stored.iter().find(|r| r.token == [2; 16]).unwrap().ended);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A send's cancel from the last run, issued again at this start, is in
    /// the record already: it is not saved again, and the UI is not told
    /// again that it waits (the record told it, see `Event::Restored`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_from_the_last_run_is_not_saved_again() {
        let dir = scratch("cancel-restored");
        let (shared, events) = recording_shared(&dir);
        let peer = SecretKey::generate().public();
        let mut record = file_record(
            1,
            &peer.to_string(),
            3,
            session::Side::Send {
                source: dir.join("a.bin"),
                source_len: 3,
                source_mtime: None,
                data_path: dir.join("a.bin"),
            },
        );
        record.done_sent = true;
        record.cancelled = true;
        record.save(&dir).unwrap();
        let path = std::fs::read_dir(session::records_dir(&dir))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|e| e == "json"))
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(600))
            .unwrap();
        // As the file system keeps it: FAT and exFAT round it.
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        let record = session::records(&dir).remove(0);
        assert!(record.cancel_pending());
        let (send, _rx) = Session::new(1, record.token, peer, Role::Send);
        send.defer_cancel();
        send.set_record(record);
        shared.add_session(send.clone());
        let ctl = shared.register(1, false);
        ctl.cancel();
        let waited =
            tokio::time::timeout(Duration::from_secs(1), cancelled(&shared, 1, &ctl)).await;
        assert!(waited.is_err(), "the transfer goes on");
        assert!(!ctl.cancelling(), "shutdown does not wait");
        assert!(!cancel_waits_told(&events, 1), "the record told the UI");
        assert!(ctl.cancel_waits.load(Ordering::Relaxed));
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            written,
            "not saved again"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A cancel while a send's record is being saved for the first time
    /// ends the transfer right after the save (see `send::start`), so
    /// shutdown waits for it like for any other cancel, and the UI is not
    /// told that it waits.
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_waits_for_a_cancel_during_the_first_save() {
        let dir = scratch("cancel-first-save");
        let (shared, events) = recording_shared(&dir);
        let peer = SecretKey::generate().public();
        let record = file_record(
            1,
            &peer.to_string(),
            3,
            session::Side::Send {
                source: dir.join("a.bin"),
                source_len: 3,
                source_mtime: None,
                data_path: dir.join("a.bin"),
            },
        );
        let (send, _rx) = Session::new(1, record.token, peer, Role::Send);
        send.set_record(record);
        send.defer_cancel();
        shared.add_session(send.clone());
        let ctl = shared.register(1, false);
        ctl.cancel();
        let waited =
            tokio::time::timeout(Duration::from_secs(1), cancelled(&shared, 1, &ctl)).await;
        assert!(waited.is_err(), "left to the task");
        assert!(ctl.cancelling(), "which ends the transfer soon");
        assert!(!cancel_waits_told(&events, 1), "nothing to tell the UI");
        let stored = session::records(&dir);
        assert!(stored[0].cancelled && !stored[0].done_sent, "{stored:?}");
        let ending = send.stop_deferring().expect("the cancel ends it");
        ending.forget_blocking(&dir);
        assert!(session::records(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A cancel that cannot be saved is saved again until it is on disk:
    /// shutdown waits meanwhile, and the next start does not lose it. The
    /// UI is told that the cancel waits right away, not only once it is on
    /// disk.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_is_saved_until_it_is_on_disk() {
        let dir = scratch("cancel-save-again");
        // Nothing can be saved while a file takes the records folder's name.
        std::fs::write(session::records_dir(&dir), b"in the way").unwrap();
        let (shared, events) = recording_shared(&dir);
        let peer = SecretKey::generate().public();
        let mut record = file_record(
            1,
            &peer.to_string(),
            3,
            session::Side::Send {
                source: dir.join("a.bin"),
                source_len: 3,
                source_mtime: None,
                data_path: dir.join("a.bin"),
            },
        );
        record.done_sent = true;
        let (send, _rx) = Session::new(1, record.token, peer, Role::Send);
        send.set_record(record);
        send.defer_cancel();
        shared.add_session(send.clone());
        let ctl = shared.register(1, false);
        ctl.cancel();
        let shared2 = shared.clone();
        let ctl2 = ctl.clone();
        let waiting = tokio::spawn(async move { cancelled(&shared2, 1, &ctl2).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(ctl.cancelling(), "not on disk yet: shutdown waits");
        assert!(cancel_waits_told(&events, 1), "the UI is told already");
        assert!(session::records(&dir).is_empty());
        std::fs::remove_file(session::records_dir(&dir)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while ctl.cancelling() {
            assert!(Instant::now() < deadline, "the save was not tried again");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let stored = session::records(&dir);
        assert!(stored[0].cancel_pending(), "{stored:?}");
        waiting.abort();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A send whose data the receiver may have goes on after a restart
    /// even when its data is gone: the receiver may not need it any more.
    #[test]
    fn a_send_the_receiver_may_have_goes_on_without_its_data() {
        let dir = scratch("gone-data");
        let peer = SecretKey::generate().public().to_string();
        let side = session::Side::Send {
            source: dir.join("gone.bin"),
            source_len: 3,
            source_mtime: None,
            data_path: dir.join("gone.bin"),
        };
        let mut record = file_record(3, &peer, 3, side);
        assert!(!record.can_go_on());
        record.done_sent = true;
        assert!(record.can_go_on());
        record.done_sent = false;
        record.done_hash = Some([1; 32]);
        assert!(record.can_go_on());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A receive whose item was in place already when the program ended
    /// (only the end of the transfer was interrupted) completes at the next
    /// start, also when it is cancelled right away: the cancel comes too
    /// late and must not delete the item.
    #[test]
    fn a_placed_item_survives_a_cancel_after_a_restart() {
        let base = scratch("placed-cancel");
        let (data_dir, recv_dir) = (base.join("data"), base.join("received"));
        std::fs::create_dir_all(&recv_dir).unwrap();
        let item = recv_dir.join("a.bin");
        std::fs::write(&item, b"0123456789").unwrap();
        let id = crate::archive::file_id(&item).unwrap();
        let mut record = file_record(
            4,
            &SecretKey::generate().public().to_string(),
            10,
            session::Side::Receive {
                dest_dir: recv_dir.clone(),
                files: session::ReceiveFiles {
                    moving: Some(session::Owned {
                        path: item.clone(),
                        id,
                    }),
                    ..session::ReceiveFiles::new(item.clone())
                },
                part_id: Some(id),
                synced: Some(10),
                folder: ReceivedFolder::Extract,
            },
        );
        record.done_hash = Some([1; 32]);
        record.save(&data_dir).unwrap();

        let (tx, rx) = mpsc::channel();
        let net = start(
            SecretKey::generate(),
            data_dir.clone(),
            base.join("temp"),
            sink(tx),
        );
        let Event::Restored { id, .. } = wait_for(&rx, |e| matches!(e, Event::Restored { .. }))
        else {
            unreachable!()
        };
        net.send(Command::Cancel { id });
        let outcome = wait_finished(&rx, id);
        assert!(
            matches!(outcome, Outcome::Completed(Some(_))),
            "{outcome:?}"
        );
        assert_eq!(std::fs::read(&item).unwrap(), b"0123456789");
        assert!(session::records(&data_dir).is_empty());
        assert_eq!(session::tombstone(&data_dir, &[4; 16]), Some(true));
        net.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }

    /// Two endpoints, A sending to B, as `send_until_verdict` leaves them.
    struct Stopped {
        key_a: SecretKey,
        key_b: SecretKey,
        dir_a: PathBuf,
        dir_b: PathBuf,
        recv_dir: PathBuf,
        file: PathBuf,
        data: Vec<u8>,
    }

    /// Sends the file `name` from A to B until all of its data and `Done`
    /// have arrived, holds B before it checks them, cancels the transfer on
    /// A if `cancel`, and then shuts both down: the sender waits for the
    /// receiver's verdict, which it gets only after a restart.
    fn send_until_verdict(base: &std::path::Path, name: &str, cancel: bool) -> Stopped {
        let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
        let (dir_a, dir_b) = (base.join("a"), base.join("b"));
        let recv_dir = base.join("received");
        let file = base.join(name);
        let data = pattern(3_000_000);
        std::fs::write(&file, &data).unwrap();
        receive::HOLD_VERIFY.lock().unwrap().push(name.into());
        let (tx_a, _rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let a = start(key_a.clone(), dir_a.clone(), base.join("temp"), sink(tx_a));
        let b = start(key_b.clone(), dir_b.clone(), base.join("temp"), sink(tx_b));
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: key_b.public(),
            path: file.clone(),
            kind: Kind::File,
            my_name: "A".into(),
            peer_name: "B".into(),
        });
        let Event::Offer { id: id_in, .. } = wait_for(&rx_b, |e| matches!(e, Event::Offer { .. }))
        else {
            unreachable!()
        };
        b.send(Command::Respond {
            id: id_in,
            accept: Some(Accept {
                dir: recv_dir.clone(),
                folder: ReceivedFolder::Extract,
            }),
        });
        let deadline = Instant::now() + Duration::from_secs(120);
        let held = || receive::VERIFY_HELD.lock().unwrap().contains(&name.into());
        while !held() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        if cancel {
            a.send(Command::Cancel { id });
        }
        // Shutdown waits until the cancel is in the record.
        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
        receive::HOLD_VERIFY.lock().unwrap().retain(|n| n != name);
        let records = session::records(&dir_a);
        assert_eq!(records.len(), 1);
        assert!(records[0].done_sent, "{:?}", records[0]);
        assert_eq!(records[0].cancel_pending(), cancel, "{:?}", records[0]);
        Stopped {
            key_a,
            key_b,
            dir_a,
            dir_b,
            recv_dir,
            file,
            data,
        }
    }

    /// A running endpoint with its events and the id of its restored transfer.
    type Restarted = (NetHandle, mpsc::Receiver<Event>, u64);

    /// Starts B and then A again; returns A and B.
    fn restart(base: &std::path::Path, s: &Stopped) -> (Restarted, Restarted) {
        let restored = |key: &SecretKey, dir: &PathBuf| {
            let (tx, rx) = mpsc::channel();
            let net = start(key.clone(), dir.clone(), base.join("temp"), sink(tx));
            let Event::Restored { id, .. } = wait_for(&rx, |e| matches!(e, Event::Restored { .. }))
            else {
                unreachable!()
            };
            (net, rx, id)
        };
        let b = restored(&s.key_b, &s.dir_b);
        let a = restored(&s.key_a, &s.dir_a);
        (a, b)
    }

    /// The sender cancels while it waits for the receiver's verdict, and
    /// both exit before the verdict comes. The cancel is not lost: after
    /// the restart it waits on, also while the transfer is paused, and ends
    /// the transfer once the receiver turns out not to have the item.
    #[test]
    fn a_cancel_waits_for_the_verdict_across_a_restart() {
        let base = scratch("cancel-restart");
        let stopped = send_until_verdict(&base, "cancel-me.bin", true);
        // Paused too (while it waited for the connection, say): it is not
        // paused for the verdict, since the window offers no Resume any more.
        let mut record = session::records(&stopped.dir_a).remove(0);
        record.paused = true;
        record.save(&stopped.dir_a).unwrap();

        let ((a, rx_a, id_a), (b, rx_b, id_b)) = restart(&base, &stopped);
        let outcome = wait_finished(&rx_a, id_a);
        assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
        let outcome = wait_finished(&rx_b, id_b);
        assert!(matches!(outcome, Outcome::CancelledByPeer), "{outcome:?}");
        assert!(session::records(&stopped.dir_a).is_empty());
        assert!(session::records(&stopped.dir_b).is_empty());
        assert!(leftovers(&stopped.recv_dir).is_empty());
        assert_eq!(std::fs::read(&stopped.file).unwrap(), stopped.data);
        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }

    /// The receiver turns out not to have the item after a restart: the
    /// sender needs its data again, and fails because it is gone (it went
    /// on without it until then).
    #[test]
    fn a_send_needs_its_data_again_when_the_receiver_lacks_it() {
        let base = scratch("data-again");
        let stopped = send_until_verdict(&base, "delete-me.bin", false);
        std::fs::remove_file(&stopped.file).unwrap();

        let ((a, rx_a, id_a), (b, rx_b, id_b)) = restart(&base, &stopped);
        let outcome = wait_finished(&rx_a, id_a);
        assert!(
            matches!(&outcome, Outcome::Failed(e) if e.as_str() == crate::i18n::tr!(error_source_changed)),
            "{outcome:?}"
        );
        let outcome = wait_finished(&rx_b, id_b);
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
        assert!(session::records(&stopped.dir_a).is_empty());
        assert!(session::records(&stopped.dir_b).is_empty());
        assert!(leftovers(&stopped.recv_dir).is_empty());
        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }
}
