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
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iroh::{Endpoint, EndpointId, SecretKey, Watcher, endpoint::Connection, endpoint::presets};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use tokio::sync::{mpsc, oneshot, watch};

pub use protocol::{Kind, Offer};
use session::{Control, Session, Streams};
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
    /// Answer to an incoming offer: `Some(folder)` accepts and saves into that folder.
    Respond {
        id: u64,
        accept: Option<PathBuf>,
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
    /// The connection was lost; the transfer continues when it is back.
    WaitingForConnection,
    /// Hashing data that was already transferred before continuing.
    Verifying,
    Unpacking,
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
        record: TransferRecord,
        /// Bytes already transferred.
        have: u64,
    },
    Phase {
        id: u64,
        phase: Phase,
    },
    /// The other side closed the connection on purpose (exiting, for now);
    /// the `WaitingForConnection` that follows is not a network problem.
    PeerSuspended {
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
    pub(crate) pending: Mutex<HashMap<u64, oneshot::Sender<Option<PathBuf>>>>,
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
/// every event. `data_dir` holds the records of unfinished transfers.
pub fn start(
    secret: SecretKey,
    data_dir: PathBuf,
    sink: impl Fn(Event) + Send + Sync + 'static,
) -> NetHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        sink: Box::new(sink),
        data_dir,
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
fn restore(shared: &Arc<Shared>, endpoint: &Endpoint) {
    let records = session::load_all(&shared.data_dir);
    session::cleanup_temp(&records);
    // Known before any task runs, so that an early `Continue` or `Query`
    // from the other side waits instead of being answered with "gone".
    shared
        .restoring
        .lock()
        .unwrap()
        .extend(records.iter().map(|r| r.token));
    for record in records {
        let id = next_transfer_id();
        let have = record.have();
        shared.emit(Event::Restored {
            id,
            record: record.clone(),
            have,
        });
        if !record.files_present() {
            shared.restored(&record.token);
            session::write_tombstone(&shared.data_dir, &record.token, false);
            record.discard(&shared.data_dir);
            shared.emit(Event::Finished {
                id,
                outcome: Outcome::Failed(crate::i18n::tr!(error_part_missing).into()),
            });
            continue;
        }
        let ctl = shared.register(id, record.paused);
        let shared = shared.clone();
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            let outcome = match record.role() {
                Role::Send => send::resume(&shared, &endpoint, id, record, ctl).await,
                Role::Receive => receive::resume(&shared, &endpoint, id, record, ctl).await,
            };
            shared.finish(id, outcome);
        });
    }
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
    restore(&shared, &endpoint);

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
                    ctl.cancel.cancel();
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
                // their peer and to clean up; the rest is suspended.
                let deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < deadline {
                    let cancelling = shared
                        .controls
                        .lock()
                        .unwrap()
                        .values()
                        .any(|c| c.cancel.flag().load(Ordering::Relaxed));
                    if !cancelling {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let sessions: Vec<_> = shared.sessions.lock().unwrap().clone();
                for session in sessions {
                    session.close_current(CLOSE_SUSPENDED, b"exiting");
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
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
    use std::sync::mpsc;

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
        let a = start(SecretKey::generate(), base.join("a"), sink(tx_a));
        let b = Arc::new(start(key_b, base.join("b"), sink(tx_b)));

        // Receiver: accept everything except offers named "decline.txt", and
        // pause the first transfer once for a moment.
        let recv_dir = base.join("received");
        let b_thread = {
            let recv_dir = recv_dir.clone();
            let b = b.clone();
            std::thread::spawn(move || {
                let mut done = Vec::new();
                let mut phases = Vec::new();
                let mut paused_once = false;
                while done.len() < 3 {
                    match rx_b
                        .recv_timeout(Duration::from_secs(120))
                        .expect("receiver timed out")
                    {
                        Event::Offer { id, offer, .. } => {
                            let accept = (offer.name != "decline.txt").then(|| recv_dir.clone());
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

        // A file, paused first by the receiver and then by the sender.
        let file = base.join("hello.bin");
        let data = pattern(30_000_000);
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

        // A folder.
        let folder = base.join("My Folder");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::write(folder.join("a.txt"), b"alpha").unwrap();
        std::fs::write(folder.join("sub").join("b.txt"), b"beta").unwrap();
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
            2
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
        assert!(!recv_dir.join("decline.txt").exists());
        let left = leftovers(&recv_dir);
        assert_eq!(left.len(), 2, "unexpected files: {left:?}");
        assert!(session::load_all(&base.join("a")).is_empty());
        assert!(session::load_all(&base.join("b")).is_empty());
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
        let a = start(SecretKey::generate(), base.join("a"), sink(tx_a));
        let b = Arc::new(start(key_b, base.join("b"), sink(tx_b)));
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
                            accept: Some(recv_dir.clone()),
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
        assert!(phases.contains(&Phase::WaitingForConnection), "{phases:?}");
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
        let a = start(SecretKey::generate(), base.join("a"), sink(tx_a));
        let b = start(key_b.clone(), dir_b.clone(), sink(tx_b));
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
            accept: Some(recv_dir.clone()),
        });
        wait_for(
            &rx_b,
            |e| matches!(e, Event::Progress { done, .. } if *done > 4_000_000),
        );
        b.shutdown(Duration::from_secs(5));
        drop(b);
        let records = session::load_all(&dir_b);
        assert_eq!(records.len(), 1, "the record survives the shutdown");
        let written = records[0].have();
        assert!(written > 0);

        wait_for(&rx_a, |e| {
            matches!(
                e,
                Event::Phase {
                    phase: Phase::WaitingForConnection,
                    ..
                }
            )
        });
        let (tx_b2, rx_b2) = mpsc::channel();
        let b2 = start(key_b, dir_b.clone(), sink(tx_b2));
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
        assert!(session::load_all(&dir_b).is_empty());
        drop(b2);
        std::fs::remove_dir_all(&base).ok();
    }

    /// The receiver is stopped after all data has arrived, while it is still
    /// unpacking the folder, and started again: it finishes on its own from
    /// the hash the sender gave it, without the sender, which is already done.
    #[test]
    fn finish_alone_after_restart_during_unpacking() {
        let base = scratch("finish-alone");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let dir_b = base.join("b");
        let a = start(SecretKey::generate(), base.join("a"), sink(tx_a));
        let b = start(key_b.clone(), dir_b.clone(), sink(tx_b));
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
            accept: Some(recv_dir.clone()),
        });
        // The sender is done as soon as the receiver has confirmed the data…
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Completed(_)));
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
        let records = session::load_all(&dir_b);
        assert_eq!(records.len(), 1, "the record survives");
        assert!(
            records[0].done_hash.is_some(),
            "the sender's hash was saved"
        );
        assert!(!recv_dir.join("Photos").exists());

        let (tx_b2, rx_b2) = mpsc::channel();
        let b2 = start(key_b, dir_b.clone(), sink(tx_b2));
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
        assert!(session::load_all(&dir_b).is_empty());
        drop(b2);
        std::fs::remove_dir_all(&base).ok();
    }
}
