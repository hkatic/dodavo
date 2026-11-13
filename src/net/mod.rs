//! Networking: an iroh endpoint running on a tokio runtime in a background
//! thread. The UI talks to it with [`Command`]s and receives [`Event`]s.
//!
//! A transfer is identified by a token on both sides. Once accepted, it has
//! a record on disk ([`session::TransferRecord`]) and survives lost
//! connections and restarts: the sender dials again, the receiver waits.

pub mod chat;
pub mod protocol;
mod receive;
pub(crate) mod send;
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

pub use crate::config::{OverLimit, ReceivedFolder};
pub use chat::ChatPolicy;
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
    /// A send of a document that the user chose in Android's pickers, which
    /// is read where it is (see [`crate::source`]).
    SendDocument {
        id: u64,
        to: EndpointId,
        document: crate::source::Document,
        my_name: String,
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
    /// Sends a chat message to `to` (see [`chat`]). `id` is the sender's
    /// number for it, which `Event::ChatSent` or `Event::ChatFailed`
    /// repeats.
    Chat {
        to: EndpointId,
        id: u64,
        text: String,
    },
    /// Tells `to` that the user is writing a chat message, or has stopped.
    /// Nothing reports whether that got there.
    Typing {
        to: EndpointId,
        on: bool,
    },
    /// The system says that the network changed (another Wi-Fi, mobile
    /// data, or back from sleep). Android tells only the app, so iroh hears
    /// it from there; elsewhere iroh watches the network itself.
    NetworkChange,
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
    /// Gives this installation a new identity, and so a new Contact ID (see
    /// [`NetHandle::change_identity`]).
    NewIdentity {
        secret: SecretKey,
        /// Set once nobody waits for `done` any more: the identity must
        /// not change behind the back of who asked.
        abandoned: Arc<AtomicBool>,
        done: std::sync::mpsc::Sender<Result<(), IdentityError>>,
    },
}

/// Why the identity was not changed (see [`NetHandle::change_identity`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// Transfers are still under way that a cancel does not end: a send
    /// whose data has all gone out, until the receiver says whether it has
    /// the item. Nothing has changed, except that the other transfers were
    /// cancelled.
    Busy,
    /// The new key could not be saved, with the reason. The identity is the
    /// old one.
    Failed(String),
    /// The network did not answer in time (it has not come up yet, say).
    /// The identity is the old one, and stays it.
    TimedOut,
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
    /// Another program keeps the partly received file out, on this side
    /// when receiving, on the other side when sending; the transfer
    /// continues as soon as it lets go. `finishing` once all of the data
    /// has been sent (a pause has no say any more), and on the receiving
    /// side when the last step waits for it.
    PartInUse {
        finishing: bool,
    },
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

impl Outcome {
    /// How a transfer that ends here with this outcome ended, for its
    /// tombstone (see `Msg::Gone`).
    fn ended(&self) -> protocol::Ended {
        use protocol::Ended;
        match self {
            Outcome::Completed(_) => Ended::Completed,
            Outcome::Cancelled | Outcome::CancelledByPeer | Outcome::Declined => Ended::Cancelled,
            Outcome::Failed(_) | Outcome::Unreachable(_) => Ended::Failed,
        }
    }
}

/// The text of a failure for the user, cause after cause as `{error:#}`
/// shows them, but with Windows' own text of an I/O error without the
/// "(os error 32)" that Rust adds, a text of our own where only the kind of
/// the error is known (tokio's files keep only that of a failed write), and
/// a general one for a panic, whose details the panic hook has shown.
pub(crate) fn failure_text(error: &anyhow::Error) -> String {
    let mut causes = Vec::new();
    for cause in error.chain() {
        if cause
            .downcast_ref::<tokio::task::JoinError>()
            .is_some_and(|e| e.is_panic())
        {
            causes.push(crate::i18n::tr!(error_unexpected).to_string());
            continue;
        }
        // The connection ended (the other computer went away): what follows
        // are the library's details, in English.
        if ends_connection(cause) {
            causes.push(crate::i18n::tr!(error_connection_lost).to_string());
            break;
        }
        causes.push(match cause.downcast_ref::<std::io::Error>() {
            Some(e) => io_error_text(e),
            None => cause.to_string(),
        });
    }
    causes.join(": ")
}

/// Whether `error`, one cause of a failure (see [`failure_text`]), is the
/// end of a connection, or the error of a read or a write on a connection
/// that ended, which an `io::Error` carries (its own source skips it). Only
/// an end that says nothing more: one that the network timed out, that the
/// other side reset or closed, or that this side closed. A protocol error,
/// for one, keeps its own text.
fn ends_connection(error: &(dyn std::error::Error + 'static)) -> bool {
    use iroh::endpoint::ConnectionError;
    let ended = |e: &(dyn std::error::Error + 'static)| {
        e.downcast_ref::<ConnectionError>().map(|e| {
            matches!(
                e,
                ConnectionError::TimedOut
                    | ConnectionError::Reset
                    | ConnectionError::ApplicationClosed(_)
                    | ConnectionError::LocallyClosed
            )
        })
    };
    if let Some(ended) = ended(error) {
        return ended;
    }
    let mut next = error
        .downcast_ref::<std::io::Error>()
        .and_then(|e| e.get_ref())
        .map(|inner| inner as &(dyn std::error::Error + 'static));
    while let Some(e) = next {
        if let Some(ended) = ended(e) {
            return ended;
        }
        next = e.source();
    }
    false
}

/// Whether a dial failed because the other side does not take the protocol
/// that was asked for: its TLS answers with the alert "no application
/// protocol" (120), which QUIC carries as a crypto error. For a transfer
/// that is a Dodavo of another version (see `protocol::ALPN`), and for a
/// chat one that has none (see `chat::dial`).
pub(crate) fn no_such_protocol(error: &iroh::endpoint::ConnectError) -> bool {
    use iroh::endpoint::{ConnectError, ConnectingError, ConnectionError, TransportErrorCode};
    let source = match error {
        ConnectError::Connection { source, .. } => source,
        ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => source,
        _ => return false,
    };
    let code = TransportErrorCode::crypto(120);
    match source {
        ConnectionError::ConnectionClosed(close) => close.error_code == code,
        ConnectionError::TransportError(error) => error.code == code,
        _ => false,
    }
}

/// The text of a dial that failed (see `send::dial`), in the user's
/// language whatever failed: that the other computer cannot be found or does
/// not answer, which is what users meet (Dodavo is not running there, or
/// one of them is offline); that its Dodavo does not take this version's
/// transfers (see [`no_such_protocol`]); that the connection ended while it
/// was being made; and for anything else only that no connection was made,
/// the library's own words for it being in English.
pub(crate) fn connect_error_text(error: &iroh::endpoint::ConnectError) -> String {
    use crate::i18n::tr;
    use iroh::endpoint::{ConnectError, ConnectWithOptsError, ConnectingError, ConnectionError};
    if no_such_protocol(error) {
        return tr!(error_other_version).into();
    }
    let ended = match error {
        ConnectError::Connect {
            source: ConnectWithOptsError::NoAddress { .. },
            ..
        } => return tr!(error_peer_not_found).into(),
        ConnectError::Connection { source, .. } => source,
        ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => source,
        _ => return tr!(error_connect_failed).into(),
    };
    match ended {
        ConnectionError::TimedOut => tr!(error_timeout),
        ConnectionError::Reset
        | ConnectionError::ConnectionClosed(_)
        | ConnectionError::ApplicationClosed(_)
        | ConnectionError::LocallyClosed => tr!(error_connection_lost),
        _ => tr!(error_connect_failed),
    }
    .into()
}

/// Why no endpoint could be bound, for the status line: the system's own
/// words where it refused something (no network, a port that may not be
/// used), and otherwise only that the network could not be started, the
/// library's own words for it being in English.
fn bind_error_text(error: &anyhow::Error) -> String {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .map(io_error_text)
        .unwrap_or_else(|| crate::i18n::tr!(error_network_start).into())
}

/// The text of an I/O error for the user (see [`failure_text`]).
pub(crate) fn io_error_text(error: &std::io::Error) -> String {
    // The system's own texts are in English here, whatever the user's language.
    #[cfg(unix)]
    if let Some(text) = error.raw_os_error().and_then(crate::platform::error_text) {
        return text;
    }
    if let Some(code) = error.raw_os_error() {
        let text = error.to_string();
        return text
            .strip_suffix(&format!(" (os error {code})"))
            .map_or(text.clone(), str::to_string);
    }
    match error.kind() {
        std::io::ErrorKind::StorageFull if error.get_ref().is_none() => {
            crate::i18n::tr!(error_disk_full).into()
        }
        _ => error.to_string(),
    }
}

#[derive(Debug)]
pub enum Event {
    /// No endpoint could be bound, with the reason for the user; it is
    /// tried again.
    BindFailed(String),
    /// An endpoint is bound after `BindFailed`: that problem is over, and
    /// whether the endpoint is online is not known yet, as at a start.
    Connecting,
    Online(bool),
    Offer {
        id: u64,
        from: EndpointId,
        offer: Offer,
    },
    /// An offer was declined because its item is larger than the sender's
    /// limit, and the user wants to be told (`OverLimit::DeclineAndTell`).
    /// It is no transfer: it has no id, and nothing follows.
    OverLimit {
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
    /// How the transfer's connection goes: `Some(true)` directly,
    /// `Some(false)` through a relay server; `None` once it has none (it
    /// waits for the next one).
    Path {
        id: u64,
        direct: Option<bool>,
    },
    Finished {
        id: u64,
        outcome: Outcome,
    },
    /// A chat message arrived. `name` is what the sender calls themselves.
    Chat {
        from: EndpointId,
        name: String,
        text: String,
    },
    /// The chat message `id` (see `Command::Chat`) arrived at the other side.
    ChatSent {
        to: EndpointId,
        id: u64,
    },
    /// The chat message `id` was not delivered, with the reason for the
    /// user. Nothing tries it again.
    ChatFailed {
        to: EndpointId,
        id: u64,
        error: String,
    },
    /// The other person of a chat is writing a message, or has stopped
    /// (which is not always said: who shows it lets it lapse).
    Typing {
        from: EndpointId,
        on: bool,
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
    /// This profile's folder for temporary archives in the system's place
    /// (see `archive::temp_dir_for`); created on use. What is always there
    /// to fall back on, and where the copies of shared items are.
    pub(crate) temp_dir: PathBuf,
    /// The folder that the user chose for them instead (see
    /// `NetHandle::set_temp_dir`, `Shared::temp_dir_now`).
    temp_chosen: Arc<Mutex<Option<PathBuf>>>,
    /// Folders that were chosen before, which a start clears of leftovers.
    temp_before: Vec<PathBuf>,
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
    /// Senders whose offers are declined without asking (see
    /// `NetHandle::set_blocked`).
    blocked: Blocked,
    /// How large an item each sender may offer (see
    /// `NetHandle::set_limits`).
    limits: Arc<Mutex<Limits>>,
    /// Whether an endpoint is bound and commands are taken (see
    /// `NetHandle::is_up`).
    up: Arc<AtomicBool>,
    /// The chat connections, where this side takes any (see
    /// [`start_with_chat`]).
    chat: Option<Arc<chat::Chats>>,
}

/// The blocked senders, shared by the network and its handle.
type Blocked = Arc<Mutex<HashSet<EndpointId>>>;

/// How large an item each sender may offer, and what happens to a larger
/// one (see `config::Config::limits`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Limits {
    pub action: OverLimit,
    /// For everyone who is not in `per_peer`; `None` for any size.
    pub general: Option<u64>,
    /// Senders with a limit of their own, `None` being any size.
    pub per_peer: HashMap<EndpointId, Option<u64>>,
}

impl Limits {
    /// The largest item, in bytes, that `peer` may offer; `None` for any size.
    pub fn limit_for(&self, peer: &EndpointId) -> Option<u64> {
        self.per_peer.get(peer).copied().unwrap_or(self.general)
    }
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
            if selected.is_some() && selected != last {
                last = selected;
                shared.emit(Event::Path {
                    id,
                    direct: selected,
                });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
}

#[derive(Clone)]
pub struct NetHandle {
    tx: mpsc::UnboundedSender<Command>,
    blocked: Blocked,
    limits: Arc<Mutex<Limits>>,
    temp_chosen: Arc<Mutex<Option<PathBuf>>>,
    up: Arc<AtomicBool>,
    chat: Arc<Mutex<ChatPolicy>>,
}

/// Where the temporary archives of folder transfers go (see [`start`]).
pub struct TempDirs {
    /// This profile's folder in the system's place for them
    /// (`archive::temp_dir_for`).
    pub default: PathBuf,
    /// This profile's folder in the one that the user chose instead
    /// (`Config::temp_dir_for`).
    pub chosen: Option<PathBuf>,
    /// The same in the folders chosen before
    /// (`Config::temp_dirs_before_for`).
    pub before: Vec<PathBuf>,
}

impl From<PathBuf> for TempDirs {
    fn from(default: PathBuf) -> Self {
        TempDirs {
            default,
            chosen: None,
            before: Vec::new(),
        }
    }
}

impl Shared {
    /// The folder for a new temporary archive: the one that the user chose,
    /// if it is there or can be made, and otherwise the system's (the
    /// chosen drive may be unplugged). Blocking.
    pub(crate) fn temp_dir_now(&self) -> PathBuf {
        let chosen = self.temp_chosen.lock().unwrap().clone();
        match chosen {
            Some(dir) if std::fs::create_dir_all(&dir).is_ok() => dir,
            _ => self.temp_dir.clone(),
        }
    }
}

impl NetHandle {
    /// A handle without a network behind it, for a program that will not get
    /// to use it (another instance runs already, say).
    pub fn idle() -> NetHandle {
        NetHandle {
            tx: mpsc::unbounded_channel().0,
            blocked: Blocked::default(),
            limits: Arc::default(),
            temp_chosen: Arc::default(),
            up: Arc::default(),
            chat: Arc::default(),
        }
    }

    /// Chooses the folder for the temporary archives of the transfers that
    /// start from now on (`None`: the system's place). Those under way keep
    /// theirs where they are.
    pub fn set_temp_dir(&self, chosen: Option<PathBuf>) {
        *self.temp_chosen.lock().unwrap() = chosen;
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Whether the network has an endpoint and takes commands: not before
    /// the first one is bound and the transfers of the last run are
    /// restored, nor while another endpoint takes its place at a change of
    /// the identity. A command sent meanwhile waits for that, which never
    /// comes while no endpoint can be bound (`Event::BindFailed`). Whether
    /// the endpoint is online has nothing to do with it.
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::SeqCst)
    }

    /// From now on, offers from these senders are declined without asking:
    /// the sender learns only that the offer was declined, and nothing here
    /// sees it. Transfers with them that are under way go on.
    pub fn set_blocked(&self, ids: impl IntoIterator<Item = EndpointId>) {
        *self.blocked.lock().unwrap() = ids.into_iter().collect();
    }

    /// From now on, an offer of an item larger than its sender's limit is
    /// declined where `limits.action` says so: the sender learns only that
    /// the offer was declined, and here it is either not seen at all or
    /// reported with `Event::OverLimit`. With `OverLimit::Ask` every offer
    /// is emitted as without limits, and the user interface keeps a larger
    /// one from starting by itself (`Config::auto_save_dir_for`). Offers
    /// that wait for an answer and transfers under way go on.
    pub fn set_limits(&self, limits: Limits) {
        *self.limits.lock().unwrap() = limits;
    }

    /// From now on, chat messages are taken as `policy` says, also on the
    /// connections that are open. Does nothing where the network was
    /// started without chat (see [`start_with_chat`]).
    pub fn set_chat(&self, policy: ChatPolicy) {
        *self.chat.lock().unwrap() = policy;
    }

    /// Suspends all transfers and closes connections so that peers learn
    /// about it, waiting at most `timeout`.
    pub fn shutdown(&self, timeout: Duration) {
        let (done, wait) = std::sync::mpsc::channel();
        self.send(Command::Shutdown { done });
        let _ = wait.recv_timeout(timeout);
    }

    /// Makes `secret` the identity of this installation, in the key file
    /// and on the network, and waits for that at most `timeout`. No transfer
    /// can go on under another identity (the other side takes a transfer
    /// only from the one it began with), so every transfer is cancelled
    /// first and every waiting offer declined, which the other sides learn
    /// while the old identity still answers. The transfers report their end
    /// as after any cancel. If one of them cannot be ended that way, the
    /// identity stays (see [`IdentityError`]). Contacts and blocked senders
    /// are other people's IDs and stay as they are.
    pub fn change_identity(
        &self,
        secret: SecretKey,
        timeout: Duration,
    ) -> Result<(), IdentityError> {
        let (done, wait) = std::sync::mpsc::channel();
        let abandoned = Arc::new(AtomicBool::new(false));
        self.send(Command::NewIdentity {
            secret,
            abandoned: abandoned.clone(),
            done,
        });
        match wait.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                abandoned.store(true, Ordering::SeqCst);
                // An answer that came just before that stands.
                wait.try_recv().unwrap_or(Err(IdentityError::TimedOut))
            }
        }
    }
}

/// How long a change of the identity waits for the transfers it cancelled to
/// end (see [`NetHandle::change_identity`]).
const IDENTITY_WAIT: Duration = Duration::from_secs(5);

/// Starts the network thread. `sink` is called from network threads for
/// every event. `data_dir` holds the records of unfinished transfers and
/// `temp_dir` the temporary archives of folder transfers (a path is the
/// system's place alone, see [`TempDirs`]). Offers from the
/// `blocked` senders are declined from the start (see
/// `NetHandle::set_blocked`), and `limits` hold from the start (see
/// `NetHandle::set_limits`). Such a network takes no chat connections: who
/// dials one is told that this side cannot chat.
pub fn start(
    secret: SecretKey,
    data_dir: PathBuf,
    temp_dir: impl Into<TempDirs>,
    blocked: impl IntoIterator<Item = EndpointId>,
    limits: Limits,
    sink: impl Fn(Event) + Send + Sync + 'static,
) -> NetHandle {
    start_with_chat(secret, data_dir, temp_dir, blocked, limits, None, sink)
}

/// [`start`], for a user interface that shows chats when `chat` says who
/// may send messages (see `NetHandle::set_chat`): only then does the
/// endpoint take chat connections, so that no message is confirmed to its
/// sender and then shown to nobody.
pub fn start_with_chat(
    secret: SecretKey,
    data_dir: PathBuf,
    temp_dir: impl Into<TempDirs>,
    blocked: impl IntoIterator<Item = EndpointId>,
    limits: Limits,
    chat: Option<ChatPolicy>,
    sink: impl Fn(Event) + Send + Sync + 'static,
) -> NetHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let chats = chat.is_some();
    let chat = Arc::new(Mutex::new(chat.unwrap_or_default()));
    let blocked = Blocked::new(Mutex::new(blocked.into_iter().collect()));
    let limits = Arc::new(Mutex::new(limits));
    let up = Arc::new(AtomicBool::new(false));
    let temp = temp_dir.into();
    let temp_chosen = Arc::new(Mutex::new(temp.chosen));
    let shared = Arc::new(Shared {
        sink: Box::new(sink),
        data_dir,
        temp_dir: temp.default,
        temp_chosen: temp_chosen.clone(),
        temp_before: temp.before,
        pending: Mutex::new(HashMap::new()),
        controls: Mutex::new(HashMap::new()),
        sessions: Mutex::new(Vec::new()),
        restoring: Mutex::new(HashSet::new()),
        shutting_down: AtomicBool::new(false),
        blocked: blocked.clone(),
        limits: limits.clone(),
        up: up.clone(),
        chat: chats.then(|| Arc::new(chat::Chats::new(chat.clone()))),
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
    NetHandle {
        tx,
        blocked,
        limits,
        temp_chosen,
        up,
        chat,
    }
}

/// Runs a send of `what` to its outcome, as a task of its own.
fn start_send(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    to: EndpointId,
    what: send::What,
    my_name: String,
    peer_name: String,
) {
    let ctl = shared.register(id, false);
    let shared = shared.clone();
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
        let task = {
            let shared = shared.clone();
            async move { send::run(&shared, &endpoint, id, to, what, my_name, peer_name, ctl).await }
        };
        let outcome = guarded(&shared, id, task).await;
        shared.finish(id, outcome);
    });
}

/// Binds an endpoint that takes transfers and, with `chat`, chat
/// connections.
async fn bind(secret: SecretKey, chat: bool) -> anyhow::Result<Endpoint> {
    let mut alpns = vec![protocol::ALPN.to_vec()];
    if chat {
        alpns.push(protocol::CHAT_ALPN.to_vec());
    }
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(alpns)
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;
    Ok(endpoint)
}

/// Runs the task of the transfer `id` to its outcome. A panic in it (a
/// bug, which the panic hook has told the user about) ends the transfer as
/// failed, as an error would, with what it leaves deleted and the peer
/// told, rather than leaving its window waiting for an end that never comes
/// and its record to fail the same way at every start. A transfer that had
/// ended before the panic stays as it ended, for the peer too, and one that
/// had completed (a received item is in place, say) is reported as
/// completed: the panic only cut short what came after. A cancel or a
/// failure is reported as a failure, since its cleanup may have been cut
/// short.
async fn guarded(
    shared: &Arc<Shared>,
    id: u64,
    task: impl Future<Output = Outcome> + Send + 'static,
) -> Outcome {
    fn failed() -> Outcome {
        Outcome::Failed(crate::i18n::tr!(error_unexpected).into())
    }
    match tokio::spawn(task).await {
        Ok(outcome) => outcome,
        Err(error) if error.is_panic() => {
            let Some(session) = shared.session_by_id(id) else {
                return failed();
            };
            let data_dir = shared.data_dir.clone();
            // A task of its own too: the panic may have left the session's
            // state poisoned (then the transfer fails, with nothing more
            // done). Unless the transfer had ended already, the peer learns
            // at once that it failed, not only at its next query after a
            // lost connection.
            tokio::spawn(async move {
                let earlier = session.ended();
                let code = earlier.is_none().then_some(CLOSE_FAILED);
                session
                    .end(code, protocol::Ended::Failed)
                    .forget(&data_dir)
                    .await;
                match earlier {
                    Some((protocol::Ended::Completed, placed)) => Outcome::Completed(placed),
                    _ => failed(),
                }
            })
            .await
            .unwrap_or_else(|_| failed())
        }
        // The runtime shuts down, and nothing is reported any more.
        Err(_) => std::future::pending().await,
    }
}

/// How a transfer that this side no longer has ended here, for `Msg::Gone`:
/// as its tombstone says, or failed, since without one nothing is known of it
/// (it never got as far as its record, say, or a start had to end it).
fn gone(data_dir: &Path, token: &protocol::Token) -> protocol::Ended {
    session::tombstone(data_dir, token).unwrap_or(protocol::Ended::Failed)
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
        Msg::Offer(_) if shared.blocked.lock().unwrap().contains(&peer) => {
            // Declined as the user would, so the sender cannot tell; the
            // connection stays until the sender has read it.
            let _ = protocol::write_msg(&mut send, &Msg::Decline).await;
            let _ = send.finish();
            let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
            return;
        }
        Msg::Offer(offer) => {
            let declined = {
                let limits = shared.limits.lock().unwrap();
                crate::limit::over(&offer, limits.limit_for(&peer))
                    .then_some(limits.action)
                    .filter(|action| *action != OverLimit::Ask)
            };
            let Some(action) = declined else {
                receive::handle(shared, endpoint, conn, send, recv, offer).await;
                return;
            };
            // Declined as a blocked sender's is. The user is told once the
            // sender has the answer, or has gone.
            let _ = protocol::write_msg(&mut send, &Msg::Decline).await;
            let _ = send.finish();
            if action == OverLimit::DeclineAndTell {
                shared.emit(Event::OverLimit { from: peer, offer });
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
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
                    ended: gone(&shared.data_dir, &token),
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
                    ended: gone(&shared.data_dir, &token),
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
            let task = {
                let shared = shared.clone();
                async move {
                    match record.role() {
                        Role::Send => send::resume(&shared, &endpoint, id, record, ctl).await,
                        Role::Receive => receive::resume(&shared, &endpoint, id, record, ctl).await,
                    }
                }
            };
            let outcome = guarded(&shared, id, task).await;
            // However it ended (also before the session was registered):
            // questions about it must not wait any longer.
            shared.restored(&token);
            shared.finish(id, outcome);
        });
    }
}

/// Why a transfer from an earlier run cannot go on, its files being gone
/// (see `TransferRecord::files_present`): what the user sent, or the data
/// that was partly transferred (a received part, or a folder's packed data).
fn missing_text(record: &TransferRecord) -> String {
    use crate::i18n::tr;
    match &record.side {
        session::Side::Send {
            source,
            staging: None,
            ..
        } if !(record.offer.kind == Kind::Folder && crate::source::is_document(source))
            && !crate::source::exists(source) =>
        {
            // A document by its name, as the app shows it.
            let path = if crate::source::is_document(source) {
                record.offer.name.clone()
            } else {
                source.display().to_string()
            };
            match record.offer.kind {
                Kind::File => tr!(error_file_missing, path = path),
                Kind::Folder => tr!(error_folder_missing, path = path),
            }
        }
        _ => tr!(error_part_missing).into(),
    }
}

/// What [`restore`] does before the transfers that go on can run: reads
/// the records of the last run, deletes in the background the files of the
/// transfers that are over, reports those that expired or cannot go on,
/// and returns the transfers that go on, each with its id and its
/// registered `Control`.
async fn restore_records(shared: &Arc<Shared>) -> Vec<(u64, TransferRecord, Arc<Control>)> {
    // Reading the records and looking at their files is file work: each
    // transfer that goes on with how many bytes it has and, if it cannot go
    // on, why.
    let (going_on, over) = {
        let data_dir = shared.data_dir.clone();
        // Every folder that may hold a temporary archive of this profile.
        let mut temp_dirs = vec![shared.temp_dir.clone()];
        temp_dirs.extend(shared.temp_chosen.lock().unwrap().clone());
        temp_dirs.extend(shared.temp_before.iter().cloned());
        session::off_runtime(move || {
            // Before the records are read: an old note would go with them.
            session::cleanup_packing(&data_dir);
            let found = session::load_all(&data_dir);
            let all = found
                .going_on
                .iter()
                .chain(found.over.iter().map(|(r, _)| r));
            let all: Vec<&TransferRecord> = all.collect();
            for temp_dir in &temp_dirs {
                session::cleanup_temp(temp_dir, all.iter().copied());
            }
            let going_on = found
                .going_on
                .into_iter()
                .map(|record| {
                    let missing = (!record.can_go_on()).then(|| missing_text(&record));
                    (record.have(), missing, record)
                })
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
    for (have, missing, record) in going_on {
        if let Some(error) = missing {
            report_failed(&record, have, &error);
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

/// Binds an endpoint for `secret`, trying again for as long as it fails.
async fn bind_patiently(secret: &SecretKey, shared: &Shared) -> Endpoint {
    let mut failed = false;
    loop {
        match bind(secret.clone(), shared.chat.is_some()).await {
            Ok(ep) => {
                // What the status says of the failure holds no more, and
                // the endpoint says nothing of itself for a while (see
                // `online_report`).
                if failed {
                    shared.emit(Event::Connecting);
                }
                break ep;
            }
            Err(e) => {
                failed = true;
                shared.emit(Event::BindFailed(bind_error_text(&e)));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// How long a new endpoint may take to reach its home relay server before
/// it is reported as offline.
const ONLINE_GRACE: Duration = Duration::from_secs(5);

/// What to report of an endpoint that is connected to its home relay server
/// or not (`online`), `age` after it was bound, when `last` is what was
/// reported of it before; `None` for nothing. A new endpoint is not connected
/// yet, which says nothing about the network: "offline" waits until it has
/// had `ONLINE_GRACE`. Until then the status is what it was: that Dodavo is
/// connecting at a start, and what the endpoint before it was at a change of
/// the identity.
fn online_report(last: Option<bool>, online: bool, age: Duration) -> Option<bool> {
    let known = online || age >= ONLINE_GRACE;
    (known && last != Some(online)).then_some(online)
}

/// Reports whether `endpoint`, bound just now, is online: connected to the
/// home relay server (see `online_report`).
fn watch_online(shared: &Arc<Shared>, endpoint: &Endpoint) -> tokio::task::JoinHandle<()> {
    let endpoint = endpoint.clone();
    let shared = shared.clone();
    tokio::spawn(async move {
        let mut status = endpoint.home_relay_status();
        let bound = Instant::now();
        let mut last = None;
        loop {
            let online = status.get().iter().any(|s| s.is_connected());
            if let Some(report) = online_report(last, online, bound.elapsed()) {
                last = Some(report);
                shared.emit(Event::Online(report));
            }
            // Often until the first report, so that it is not late.
            let pause = if last.is_none() { 250 } else { 2000 };
            tokio::time::sleep(Duration::from_millis(pause)).await;
        }
    })
}

/// Takes the connections that come in to `endpoint`.
fn serve(shared: &Arc<Shared>, endpoint: &Endpoint) -> tokio::task::JoinHandle<()> {
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
                if conn.alpn() == protocol::CHAT_ALPN {
                    chat::accept(shared, conn).await;
                } else {
                    accept(shared, endpoint, conn).await;
                }
            });
        }
    })
}

/// Ends every transfer for a change of the identity: declines the offers
/// that wait for an answer and cancels the rest, then waits a while for
/// them to end. Whether all of them have.
async fn end_all_transfers(shared: &Shared) -> bool {
    let deadline = Instant::now() + IDENTITY_WAIT;
    loop {
        // Every pass: an offer may have been on its way in.
        let waiting: Vec<_> = shared.pending.lock().unwrap().drain().collect();
        for (_, answer) in waiting {
            let _ = answer.send(None);
        }
        let controls: Vec<_> = shared.controls.lock().unwrap().values().cloned().collect();
        if controls.is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        for ctl in controls.iter().filter(|c| !c.cancelled()) {
            ctl.cancel();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run(secret: SecretKey, shared: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<Command>) {
    let mut endpoint = bind_patiently(&secret, &shared).await;
    let mut online = watch_online(&shared, &endpoint);

    // The transfers of the last run, before anyone can ask about them.
    restore(&shared, &endpoint).await;

    // Incoming connections.
    let mut serving = serve(&shared, &endpoint);
    shared.up.store(true, Ordering::SeqCst);

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
                let what = send::What {
                    path,
                    kind,
                    document: None,
                };
                start_send(&shared, &endpoint, id, to, what, my_name, peer_name);
            }
            Command::SendDocument {
                id,
                to,
                document,
                my_name,
                peer_name,
            } => {
                let what = send::What::document(document);
                start_send(&shared, &endpoint, id, to, what, my_name, peer_name);
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
            Command::Chat { to, id, text } => {
                chat::send(&shared, &endpoint, to, chat::Out::Text { id, text });
            }
            Command::Typing { to, on } => {
                chat::send(&shared, &endpoint, to, chat::Out::Typing(on));
            }
            Command::NetworkChange => {
                let endpoint = endpoint.clone();
                tokio::spawn(async move { endpoint.network_change().await });
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
            Command::NewIdentity {
                secret,
                abandoned,
                done,
            } => {
                if abandoned.load(Ordering::SeqCst) {
                    continue;
                }
                // No new offers while the transfers end, and none that
                // would begin under an identity about to go.
                serving.abort();
                if !end_all_transfers(&shared).await || abandoned.load(Ordering::SeqCst) {
                    serving = serve(&shared, &endpoint);
                    let _ = done.send(Err(IdentityError::Busy));
                    continue;
                }
                let saved = {
                    let (dir, secret) = (shared.data_dir.clone(), secret.clone());
                    session::off_runtime(move || crate::identity::replace(&dir, &secret)).await
                };
                if let Err(e) = saved {
                    serving = serve(&shared, &endpoint);
                    let text = failure_text(&anyhow::Error::new(e));
                    let _ = done.send(Err(IdentityError::Failed(text)));
                    continue;
                }
                // The key is the new one from here on, whatever the network
                // does: the old endpoint goes, and the new one comes up as
                // the first one did at the start. Nothing says that the
                // network is gone meanwhile, which it is not: the new
                // endpoint reports what it finds (see `online_report`).
                // No command is taken until it is there, which who asked
                // can tell from here on.
                shared.up.store(false, Ordering::SeqCst);
                let _ = done.send(Ok(()));
                online.abort();
                // Let the closes of the cancelled transfers reach the peers
                // (see `Command::Shutdown`).
                tokio::time::sleep(Duration::from_millis(200)).await;
                endpoint.close().await;
                endpoint = bind_patiently(&secret, &shared).await;
                online = watch_online(&shared, &endpoint);
                serving = serve(&shared, &endpoint);
                shared.up.store(true, Ordering::SeqCst);
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

    /// Waits for the end of the transfer `id`. A timeout names the line that
    /// waited.
    #[track_caller]
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

    /// Waits until `pred` accepts an event (events before it are dropped). A
    /// timeout names the line that waited.
    #[track_caller]
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

    /// Makes sure that `a` reaches `b`, whose Contact ID is `to`, before a
    /// test sends what it is about: a small file goes out until `b` answers,
    /// and is declined there. An endpoint that has just started is not found
    /// at once where the local network is closed to the test (GitHub's Mac
    /// runner): its address is published a moment after it is online, and a
    /// send that does not find its receiver ends as unreachable, with nothing
    /// to try it again. A `b` that blocks `a` declines by itself, which is an
    /// answer too. For two endpoints that are online; what they report
    /// meanwhile is dropped, and nothing of the small file is left for the
    /// test to come across.
    #[track_caller]
    fn reach(
        a: &NetHandle,
        rx_a: &mpsc::Receiver<Event>,
        b: &NetHandle,
        rx_b: &mpsc::Receiver<Event>,
        to: EndpointId,
        base: &std::path::Path,
    ) {
        const NAME: &str = "reach.probe";
        let probe = base.join(NAME);
        std::fs::write(&probe, b"?").unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        // The offers of it that `b` was shown and that have not ended there.
        let mut shown = Vec::new();
        let note = |shown: &mut Vec<u64>, event| match event {
            Event::Offer { id, offer, .. } if offer.name == NAME => {
                shown.push(id);
                b.send(Command::Respond { id, accept: None });
            }
            Event::Finished { id, .. } => shown.retain(|s| *s != id),
            _ => {}
        };
        let mut tries = 0;
        let mut last = None;
        'tries: loop {
            tries += 1;
            let id = next_transfer_id();
            a.send(Command::Send {
                id,
                to,
                path: probe.clone(),
                kind: Kind::File,
                my_name: "A".into(),
                peer_name: "B".into(),
            });
            loop {
                assert!(
                    Instant::now() < deadline,
                    "the receiver was not reached in {tries} tries, of which the last ended as {last:?}"
                );
                if let Ok(event) = rx_b.recv_timeout(Duration::from_millis(20)) {
                    note(&mut shown, event);
                }
                let ended = rx_a.try_iter().find_map(|e| match e {
                    Event::Finished { id: i, outcome } if i == id => Some(outcome),
                    _ => None,
                });
                match ended {
                    None => {}
                    Some(Outcome::Declined) => break 'tries,
                    Some(other) => {
                        last = Some(other);
                        std::thread::sleep(Duration::from_millis(250));
                        continue 'tries;
                    }
                }
            }
        }
        while !shown.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = rx_b.recv_timeout(left).expect("timed out");
            note(&mut shown, event);
        }
        if tries > 1 {
            eprintln!("reached in {tries} tries; before that: {last:?}");
        }
        std::fs::remove_file(&probe).unwrap();
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
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = Arc::new(start(
            key_b,
            base.join("b"),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_b),
        ));
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);

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
        if crate::archive::testing::CREATION_TIMES {
            assert_eq!(meta.created().unwrap(), file_created);
        }
        let marked = crate::archive::testing::has_mark;
        assert!(marked(&recv_dir.join("hello.bin")));
        let modified = |p: PathBuf| std::fs::metadata(p).unwrap().modified().unwrap();
        let folder = recv_dir.join("My Folder");
        assert_eq!(modified(folder.join("a.txt")), inner_modified);
        assert_eq!(modified(folder.clone()), inner_modified);
        assert!(marked(&folder.join("a.txt")));
        // "Kept" is there twice: unpacked, and as its zip.
        assert_eq!(
            std::fs::read(recv_dir.join("Kept").join("c.txt")).unwrap(),
            b"gamma"
        );
        assert!(recv_dir.join("Kept.zip").is_file());
        assert!(marked(&recv_dir.join("Kept.zip")));
        assert!(!recv_dir.join("decline.txt").exists());
        let left = leftovers(&recv_dir);
        assert_eq!(left.len(), 4, "unexpected files: {left:?}");
        assert!(session::records(&base.join("a")).is_empty());
        assert!(session::records(&base.join("b")).is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    /// An offer from a blocked sender is declined without the receiver
    /// seeing it, and one sent after the block is lifted is shown again.
    #[test]
    fn a_blocked_sender_is_declined_unseen() {
        let base = scratch("blocked");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_a = SecretKey::generate();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let a = start(
            key_a.clone(),
            base.join("a"),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = start(
            key_b,
            base.join("b"),
            base.join("temp"),
            [key_a.public()],
            Limits::default(),
            sink(tx_b),
        );
        // Both can be found a moment after they are online, also where the
        // local network is closed to the test (GitHub's Mac runner).
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let file = base.join("note.txt");
        std::fs::write(&file, b"hello").unwrap();
        let send = |id| {
            a.send(Command::Send {
                id,
                to: id_b,
                path: file.clone(),
                kind: Kind::File,
                my_name: "A".into(),
                peer_name: "B".into(),
            })
        };

        let first = next_transfer_id();
        send(first);
        let outcome = wait_finished(&rx_a, first);
        assert!(matches!(outcome, Outcome::Declined), "{outcome:?}");
        let seen: Vec<Event> = rx_b.try_iter().collect();
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, Event::Offer { .. } | Event::Finished { .. })),
            "{seen:?}"
        );

        b.set_blocked([]);
        let second = next_transfer_id();
        send(second);
        let Event::Offer { id, from, .. } = wait_for(&rx_b, |e| matches!(e, Event::Offer { .. }))
        else {
            unreachable!()
        };
        assert_eq!(from, key_a.public());
        b.send(Command::Respond { id, accept: None });
        let outcome = wait_finished(&rx_a, second);
        assert!(matches!(outcome, Outcome::Declined), "{outcome:?}");
        std::fs::remove_dir_all(&base).ok();
    }

    /// An item over its sender's limit is declined where the limits say so,
    /// unseen or with a word to the user; where they say to ask, and for a
    /// sender whose own limit allows it, the offer is shown.
    #[test]
    fn an_item_over_the_limit_is_declined() {
        let base = scratch("limit");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_a = SecretKey::generate();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let limits = |action, own: Option<Option<u64>>| Limits {
            action,
            general: Some(4),
            per_peer: own.map(|limit| (key_a.public(), limit)).into_iter().collect(),
        };
        let a = start(
            key_a.clone(),
            base.join("a"),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = start(
            key_b,
            base.join("b"),
            base.join("temp"),
            [],
            limits(OverLimit::Decline, None),
            sink(tx_b),
        );
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let file = base.join("note.txt");
        std::fs::write(&file, b"hello").unwrap();
        let send = || {
            let id = next_transfer_id();
            a.send(Command::Send {
                id,
                to: id_b,
                path: file.clone(),
                kind: Kind::File,
                my_name: "A".into(),
                peer_name: "B".into(),
            });
            id
        };
        let declined = |id| {
            let outcome = wait_finished(&rx_a, id);
            assert!(matches!(outcome, Outcome::Declined), "{outcome:?}");
        };
        let seen = |e: &Event| {
            matches!(
                e,
                Event::Offer { .. } | Event::Finished { .. } | Event::OverLimit { .. }
            )
        };

        // Five bytes against a limit of four: declined, and nothing is seen.
        declined(send());
        let events: Vec<Event> = rx_b.try_iter().collect();
        assert!(!events.iter().any(seen), "{events:?}");

        // Declined, and the user is told what it was.
        b.set_limits(limits(OverLimit::DeclineAndTell, None));
        declined(send());
        let Event::OverLimit { from, offer } = wait_for(&rx_b, seen) else {
            panic!("the offer was not declined for its size");
        };
        assert_eq!(from, key_a.public());
        assert_eq!((offer.name.as_str(), offer.size), ("note.txt", 5));

        // The user is asked, as about any offer.
        b.set_limits(limits(OverLimit::Ask, None));
        let third = send();
        let Event::Offer { id, .. } = wait_for(&rx_b, seen) else {
            panic!("the offer was not shown");
        };
        b.send(Command::Respond { id, accept: None });
        declined(third);
        wait_for(&rx_b, |e| matches!(e, Event::Finished { .. }));

        // The sender's own limit comes first: any size, and five bytes.
        for own in [None, Some(5)] {
            b.set_limits(limits(OverLimit::Decline, Some(own)));
            let shown = send();
            let Event::Offer { id, .. } = wait_for(&rx_b, seen) else {
                panic!("the offer was not shown with the sender's limit {own:?}");
            };
            b.send(Command::Respond { id, accept: None });
            declined(shown);
            wait_for(&rx_b, |e| matches!(e, Event::Finished { .. }));
        }
        // And is the one that declines, whatever the general one allows.
        let mut tight = limits(OverLimit::Decline, Some(Some(4)));
        tight.general = None;
        b.set_limits(tight);
        declined(send());
        let events: Vec<Event> = rx_b.try_iter().collect();
        assert!(!events.iter().any(seen), "{events:?}");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A new identity ends what was under way, which the other side learns,
    /// is the key of the next start, and is where the peer reaches this side
    /// from then on.
    #[test]
    fn a_new_identity_ends_the_transfers_and_takes_over() {
        let base = scratch("identity");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_a = SecretKey::generate();
        let dir_b = base.join("b");
        let key_b = crate::identity::load_or_create(&dir_b).unwrap();
        let id_b = key_b.public();
        let a = start(key_a, base.join("a"), base.join("temp"), [], Limits::default(), sink(tx_a));
        let b = start(key_b, dir_b.clone(), base.join("temp"), [], Limits::default(), sink(tx_b));
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let recv_dir = base.join("received");
        std::fs::create_dir_all(&recv_dir).unwrap();
        let send = |to, name: &str, len| {
            let file = base.join(name);
            std::fs::write(&file, pattern(len)).unwrap();
            let id = next_transfer_id();
            a.send(Command::Send {
                id,
                to,
                path: file,
                kind: Kind::File,
                my_name: "A".into(),
                peer_name: "B".into(),
            });
            id
        };
        let offer = |name: &str| {
            let Event::Offer { id, .. } = wait_for(
                &rx_b,
                |e| matches!(e, Event::Offer { offer, .. } if offer.name == name),
            ) else {
                unreachable!()
            };
            id
        };
        let accept = |id| {
            b.send(Command::Respond {
                id,
                accept: Some(Accept {
                    dir: recv_dir.clone(),
                    folder: ReceivedFolder::Extract,
                }),
            })
        };

        // One transfer under way, paused so that it stays that way, and one
        // offer that waits for its answer.
        let running = send(id_b, "running.bin", 8 << 20);
        let running_b = offer("running.bin");
        b.send(Command::Pause { id: running_b });
        accept(running_b);
        wait_for(
            &rx_a,
            |e| matches!(e, Event::Phase { id, phase: Phase::PausedByPeer } if *id == running),
        );
        let waiting = send(id_b, "waiting.bin", 100);
        offer("waiting.bin");

        let new_key = SecretKey::generate();
        let new_id = new_key.public();
        assert!(b.is_up());
        assert_eq!(b.change_identity(new_key, Duration::from_secs(60)), Ok(()));
        // In whichever order the two end.
        let mut ended = HashMap::new();
        while ended.len() < 2 {
            if let Event::Finished { id, outcome } =
                wait_for(&rx_a, |e| matches!(e, Event::Finished { .. }))
            {
                ended.insert(id, outcome);
            }
        }
        let outcome = &ended[&running];
        assert!(matches!(outcome, Outcome::CancelledByPeer), "{outcome:?}");
        let outcome = &ended[&waiting];
        assert!(
            matches!(outcome, Outcome::Declined | Outcome::CancelledByPeer),
            "{outcome:?}"
        );
        assert!(
            leftovers(&recv_dir).is_empty(),
            "{:?}",
            leftovers(&recv_dir)
        );
        assert_eq!(
            crate::identity::load_or_create(&dir_b).unwrap().public(),
            new_id
        );

        // The new Contact ID answers.
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        assert!(b.is_up());
        reach(&a, &rx_a, &b, &rx_b, new_id, &base);
        let again = send(new_id, "again.bin", 100);
        accept(offer("again.bin"));
        let outcome = wait_finished(&rx_a, again);
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        assert_eq!(
            std::fs::read(recv_dir.join("again.bin")).unwrap(),
            pattern(100)
        );

        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }

    /// A copy of a shared item in the sender's staging folder goes with its
    /// transfer: when it completed, when it was declined, and a folder's as
    /// soon as it is packed.
    #[test]
    fn a_shared_copy_goes_with_its_transfer() {
        let base = scratch("staged");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let temp_a = base.join("temp-a");
        let a = start(SecretKey::generate(), base.join("a"), temp_a.clone(), [], Limits::default(), sink(tx_a));
        let b = start(key_b, base.join("b"), base.join("temp-b"), [], Limits::default(), sink(tx_b));
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let staging = session::staging_root(&temp_a);
        let copy = |folder: &str, name: &str| {
            let path = staging.join(folder).join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            path
        };
        let received = base.join("received");
        let send_and_answer = |path: PathBuf, kind: Kind, accept: bool| {
            let id = next_transfer_id();
            a.send(Command::Send {
                id,
                to: id_b,
                path,
                kind,
                my_name: "A".into(),
                peer_name: "B".into(),
            });
            let Event::Offer { id: offered, .. } = wait_for(&rx_b, |e| matches!(e, Event::Offer { .. }))
            else {
                unreachable!()
            };
            let accept = accept.then(|| Accept {
                dir: received.clone(),
                folder: ReceivedFolder::Extract,
            });
            b.send(Command::Respond { id: offered, accept });
            wait_finished(&rx_a, id)
        };

        let file = copy("one", "note.txt");
        std::fs::write(&file, b"hello").unwrap();
        let outcome = send_and_answer(file.clone(), Kind::File, true);
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        assert_eq!(std::fs::read(received.join("note.txt")).unwrap(), b"hello");
        assert!(!staging.join("one").exists(), "completed");

        let file = copy("two", "declined.txt");
        std::fs::write(&file, b"no").unwrap();
        let outcome = send_and_answer(file, Kind::File, false);
        assert!(matches!(outcome, Outcome::Declined), "{outcome:?}");
        assert!(!staging.join("two").exists(), "declined");

        let folder = copy("three", "Photos").join("a.txt");
        std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
        std::fs::write(&folder, b"alpha").unwrap();
        let outcome = send_and_answer(staging.join("three").join("Photos"), Kind::Folder, true);
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        assert_eq!(std::fs::read(received.join("Photos").join("a.txt")).unwrap(), b"alpha");
        assert!(!staging.join("three").exists(), "packed and completed");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A document from Android's pickers goes as the app describes it: a
    /// file under the name and date the app gives, read from where it is,
    /// and a folder packed from what the app lists of it.
    #[test]
    fn a_document_goes_as_the_app_describes_it() {
        let base = scratch("document");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let a = start(SecretKey::generate(), base.join("a"), base.join("temp-a"), [], Limits::default(), sink(tx_a));
        let b = start(key_b, base.join("b"), base.join("temp-b"), [], Limits::default(), sink(tx_b));
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let stored = base.join("stored");
        std::fs::create_dir_all(&stored).unwrap();
        let data = |name: &str, bytes: &[u8]| {
            let path = stored.join(name);
            std::fs::write(&path, bytes).unwrap();
            path.display().to_string()
        };
        let received = base.join("received");
        let send_and_accept = |document: crate::source::Document| {
            let id = next_transfer_id();
            a.send(Command::SendDocument {
                id,
                to: id_b,
                document,
                my_name: "A".into(),
                peer_name: "B".into(),
            });
            let Event::Offer { id: offered, offer, .. } =
                wait_for(&rx_b, |e| matches!(e, Event::Offer { .. }))
            else {
                unreachable!()
            };
            b.send(Command::Respond {
                id: offered,
                accept: Some(Accept {
                    dir: received.clone(),
                    folder: ReceivedFolder::Extract,
                }),
            });
            (offer, wait_finished(&rx_a, id))
        };

        let modified = 1_600_000_000_000;
        let (offer, outcome) = send_and_accept(crate::source::Document {
            uri: data("1", b"hello"),
            name: "Report.txt".into(),
            modified: Some(modified),
            items: None,
        });
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        assert_eq!(offer.name, "Report.txt");
        let report = received.join("Report.txt");
        assert_eq!(std::fs::read(&report).unwrap(), b"hello");
        assert_eq!(
            report.metadata().unwrap().modified().ok(),
            crate::source::millis_to_system(modified)
        );
        assert!(stored.join("1").exists(), "a document is never deleted");

        let item = |path: &[&str], uri: Option<String>| crate::source::Item {
            path: path.iter().map(|p| p.to_string()).collect(),
            folder: uri.is_none(),
            size: uri.as_ref().map_or(0, |u| std::fs::metadata(u).unwrap().len()),
            modified: None,
            uri: uri.unwrap_or_default(),
        };
        let (offer, outcome) = send_and_accept(crate::source::Document {
            uri: base.join("not-a-folder").display().to_string(),
            name: "Trip".into(),
            modified: None,
            items: Some(vec![
                item(&["sub"], None),
                item(&["sub", "b.txt"], Some(data("2", b"beta"))),
                item(&["a.txt"], Some(data("3", b"alpha"))),
            ]),
        });
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        assert_eq!((offer.kind, offer.file_count, offer.content_size), (Kind::Folder, 2, 9));
        assert_eq!(std::fs::read(received.join("Trip/a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(received.join("Trip/sub/b.txt")).unwrap(), b"beta");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Another program changes the part while no connection has it open:
    /// the transfer goes on, and fails at the end, since what is on the disk
    /// is not what the sender sent. Nothing of the item is kept.
    #[test]
    fn a_part_changed_between_connections_fails_the_transfer() {
        let base = scratch("part-changed");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let a = start(
            SecretKey::generate(),
            base.join("a"),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = Arc::new(start(
            key_b,
            base.join("b"),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_b),
        ));
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let recv_dir = base.join("received");
        // Set once the receiver waits for the next connection.
        let waiting = Arc::new(AtomicBool::new(false));
        let b_thread = {
            let (recv_dir, b, waiting) = (recv_dir.clone(), b.clone(), waiting.clone());
            std::thread::spawn(move || {
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
                        Event::Phase {
                            phase: Phase::WaitingForConnection { .. },
                            ..
                        } => waiting.store(true, Ordering::SeqCst),
                        Event::Finished { outcome, .. } => return outcome,
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
        // A paused sender does not dial again by itself, so the part stays
        // closed for as long as the test needs.
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
        a.send(Command::DropConnection { id });
        let deadline = Instant::now() + Duration::from_secs(60);
        while !waiting.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the receiver never waited");
            std::thread::sleep(Duration::from_millis(50));
        }
        // What reached the disk is in the record, with how the part begins
        // (see `session::Head`).
        let records = session::records(&base.join("b"));
        let begins = session::Head::of(&data[..session::HEAD_LEN]);
        assert!(
            matches!(
                &records[..],
                [r] if matches!(
                    &r.side,
                    session::Side::Receive { head: Some(head), synced: Some(synced), .. }
                        if *head == begins && *synced > 0
                )
            ),
            "{records:?}"
        );
        // One byte of what arrived becomes another, as a program that
        // writes to the file would leave it. Its length stays.
        let part = recv_dir.join("big.bin.part");
        {
            use std::io::{Read, Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&part)
                .expect("the part, which no connection has open");
            let mut byte = [0u8];
            file.seek(SeekFrom::Start(1000)).unwrap();
            file.read_exact(&mut byte).unwrap();
            byte[0] ^= 0xff;
            file.seek(SeekFrom::Start(1000)).unwrap();
            file.write_all(&byte).unwrap();
            file.sync_all().unwrap();
        }
        a.send(Command::Resume { id });
        let outcome_a = wait_finished(&rx_a, id);
        assert!(matches!(outcome_a, Outcome::Failed(_)), "{outcome_a:?}");
        let outcome_b = b_thread.join().unwrap();
        let damaged = crate::i18n::tr!(error_damaged);
        assert!(
            matches!(&outcome_b, Outcome::Failed(text) if text == damaged),
            "{outcome_b:?}"
        );
        assert!(!part.exists());
        assert!(!recv_dir.join("big.bin").exists());
        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
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
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = Arc::new(start(
            key_b,
            base.join("b"),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_b),
        ));
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
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
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = start(
            key_b.clone(),
            dir_b.clone(),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_b),
        );
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
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
        let b2 = start(key_b, dir_b.clone(), base.join("temp"), [], Limits::default(), sink(tx_b2));
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
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = start(
            key_b.clone(),
            dir_b.clone(),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_b),
        );
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
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
        let b2 = start(key_b, dir_b.clone(), base.join("temp"), [], Limits::default(), sink(tx_b2));
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
                largest_file: 3,
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
                head: None,
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
            std::fs::create_dir_all(part.parent().unwrap()).unwrap();
            std::fs::write(&part, b"abc").unwrap();
            let side = session::Side::Receive {
                dest_dir: dir.clone(),
                files: session::ReceiveFiles::new(part.clone()),
                part_id: Some(crate::archive::file_id(&part).unwrap()),
                synced: Some(3),
                folder: ReceivedFolder::Extract,
                head: None,
            };
            (file_record(token, &peer, 3, side), part)
        };
        let hold = crate::archive::testing::hold;
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
        // The parts that are held are in folders of their own: on Linux a
        // hold closes the folder (see `archive::testing::hold`).
        let (mut ended, ended_part) = receive(4, "held/ended.part");
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
        let (mut given_up, given_up_part) = receive(5, "held-too/given-up.part");
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
                sent: 0,
                staging: None,
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
                largest_file: size,
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

    /// A new temporary archive goes to the folder that the user chose, and
    /// to the system's place while that one cannot be made (its drive is
    /// not there). A start clears the leftovers of every folder that was
    /// ever used: the system's place, the chosen one and those before.
    #[tokio::test(flavor = "multi_thread")]
    async fn temporary_archives_go_where_the_user_chose() {
        let dir = scratch("temp-chosen");
        let (shared, _events) = recording_shared(&dir);
        assert_eq!(shared.temp_dir_now(), dir.join("temp"));
        let chosen = dir.join("chosen").join("Dodavo");
        *shared.temp_chosen.lock().unwrap() = Some(chosen.clone());
        assert_eq!(shared.temp_dir_now(), chosen);
        assert!(chosen.is_dir());
        // Under a file no folder can be made.
        let file = dir.join("a file");
        std::fs::write(&file, b"x").unwrap();
        *shared.temp_chosen.lock().unwrap() = Some(file.join("Dodavo"));
        assert_eq!(shared.temp_dir_now(), dir.join("temp"));

        let before = dir.join("before").join("Dodavo");
        let month_ago = SystemTime::now() - Duration::from_secs(31 * 24 * 60 * 60);
        let leftover = |folder: &std::path::Path| {
            std::fs::create_dir_all(folder).unwrap();
            let zip = session::temp_archive(folder, session::Role::Send, &[3; 16]);
            std::fs::write(&zip, b"zip").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&zip)
                .unwrap()
                .set_modified(month_ago)
                .unwrap();
            zip
        };
        let shared = {
            let (shared, _) = recording_shared(&dir);
            let mut shared = Arc::try_unwrap(shared).ok().expect("the only one");
            shared.temp_chosen = Arc::new(Mutex::new(Some(chosen.clone())));
            shared.temp_before = vec![before.clone()];
            Arc::new(shared)
        };
        let leftovers = [
            leftover(&dir.join("temp")),
            leftover(&chosen),
            leftover(&before),
        ];
        let elsewhere = leftover(&dir.join("elsewhere"));
        assert!(restore_records(&shared).await.is_empty());
        assert!(leftovers.iter().all(|zip| !zip.exists()));
        assert!(elsewhere.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `Shared` that keeps the events it emits for the test to look at.
    pub(super) fn recording_shared(dir: &std::path::Path) -> (Arc<Shared>, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let shared = Arc::new(Shared {
            sink: Box::new(move |event| sink.lock().unwrap().push(event)),
            data_dir: dir.to_path_buf(),
            temp_dir: dir.join("temp"),
            temp_chosen: Arc::default(),
            temp_before: Vec::new(),
            pending: Mutex::new(HashMap::new()),
            controls: Mutex::new(HashMap::new()),
            sessions: Mutex::new(Vec::new()),
            restoring: Mutex::new(HashSet::new()),
            shutting_down: AtomicBool::new(false),
            blocked: Blocked::default(),
            limits: Arc::default(),
            up: Arc::default(),
            chat: None,
        });
        (shared, events)
    }

    /// The text of a failure: Windows' own, without Rust's error code; one
    /// of ours where only the kind of the error is known; a general one for
    /// a panic, whose details the panic hook shows.
    #[tokio::test]
    async fn failure_texts() {
        let held =
            anyhow::Error::from(std::io::Error::from_raw_os_error(32)).context("Cannot open x");
        let text = failure_text(&held);
        assert!(text.starts_with("Cannot open x: "), "{text}");
        assert!(!text.contains("os error"), "{text}");
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        assert_eq!(
            failure_text(&full.into()),
            crate::i18n::tr!(error_disk_full)
        );
        let panicked = tokio::task::spawn_blocking(|| -> u8 { panic!("boom") })
            .await
            .unwrap_err();
        let text = failure_text(&anyhow::Error::from(panicked).context("x"));
        assert_eq!(text, format!("x: {}", crate::i18n::tr!(error_unexpected)));
        // A connection that ended, also as the error of a read on it: ours,
        // not the library's "connection lost: timed out".
        let lost = crate::i18n::tr!(error_connection_lost);
        let ended = iroh::endpoint::ConnectionError::TimedOut;
        assert_eq!(failure_text(&ended.clone().into()), lost);
        let read = std::io::Error::from(iroh::endpoint::ReadError::ConnectionLost(ended));
        assert_eq!(failure_text(&anyhow::Error::from(read).context("x")), format!("x: {lost}"));
        // Not an error of the protocol, which keeps the library's text.
        let broken = iroh::endpoint::ConnectionError::VersionMismatch;
        assert_eq!(failure_text(&broken.clone().into()), broken.to_string());
    }

    /// A panic in a transfer's task ends the transfer as failed, rather than
    /// leaving it without an end (see `guarded`).
    #[tokio::test]
    async fn a_transfer_whose_task_panics_fails() {
        async fn panics() -> Outcome {
            panic!("boom")
        }
        let dir = scratch("guarded");
        let (shared, _) = recording_shared(&dir);
        let outcome = guarded(&shared, 7, panics()).await;
        assert!(
            matches!(&outcome, Outcome::Failed(text) if text == crate::i18n::tr!(error_unexpected)),
            "{outcome:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A panic once the transfer has completed here (the received item is
    /// in place) leaves it completed, with the item where it was put; one
    /// after a cancel is still a failure, since the cancel's cleanup may
    /// have been cut short (see `guarded`).
    #[tokio::test]
    async fn a_transfer_that_completed_before_a_panic_stays_completed() {
        async fn panics() -> Outcome {
            panic!("boom")
        }
        let dir = scratch("guarded-completed");
        let (shared, _) = recording_shared(&dir);
        let peer = SecretKey::generate().public();
        let (session, _rx) = Session::new(7, [7; 16], peer, Role::Receive);
        let item = dir.join("a.bin");
        session.end_placed(&item).forget(&dir).await;
        shared.add_session(session);
        let outcome = guarded(&shared, 7, panics()).await;
        assert!(
            matches!(&outcome, Outcome::Completed(Some(path)) if *path == item),
            "{outcome:?}"
        );
        let (session, _rx) = Session::new(8, [8; 16], peer, Role::Receive);
        assert!(session.cancel_blocking(&dir));
        shared.add_session(session);
        let outcome = guarded(&shared, 8, panics()).await;
        assert!(
            matches!(&outcome, Outcome::Failed(text) if text == crate::i18n::tr!(error_unexpected)),
            "{outcome:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A new endpoint is reported as online as soon as it is, and as offline
    /// only once it has had the time to connect: not at the start, and not
    /// when it takes the place of another at a change of the identity.
    #[test]
    fn a_new_endpoint_is_not_offline_before_it_had_time_to_connect() {
        let soon = Duration::from_millis(250);
        assert_eq!(online_report(None, false, Duration::ZERO), None);
        assert_eq!(online_report(None, false, soon), None);
        assert_eq!(online_report(None, true, soon), Some(true));
        assert_eq!(online_report(None, false, ONLINE_GRACE), Some(false));
        // Later on, every change is reported, once.
        let later = ONLINE_GRACE * 10;
        assert_eq!(online_report(Some(true), true, later), None);
        assert_eq!(online_report(Some(true), false, later), Some(false));
        assert_eq!(online_report(Some(false), false, later), None);
        assert_eq!(online_report(Some(false), true, later), Some(true));
        // Online at once and then not any more: said when that time is up.
        assert_eq!(online_report(Some(true), false, soon), None);
        assert_eq!(online_report(Some(true), false, ONLINE_GRACE), Some(false));
    }

    /// Without a tombstone, a transfer that this side does not know (any
    /// more) cannot go on: for the other side, it failed here.
    #[test]
    fn gone_without_a_tombstone_failed() {
        let dir = scratch("gone");
        assert_eq!(gone(&dir, &[1; 16]), protocol::Ended::Failed);
        session::write_tombstone(&dir, &[1; 16], protocol::Ended::Cancelled);
        assert_eq!(gone(&dir, &[1; 16]), protocol::Ended::Cancelled);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A send that cannot go on because what it sends is gone says so, not
    /// that the partly transferred data is gone.
    #[test]
    fn a_send_without_its_source_names_it() {
        let dir = scratch("source-gone");
        let peer = iroh::SecretKey::generate().public().to_string();
        let source = dir.join("gone.bin");
        let side = session::Side::Send {
            source: source.clone(),
            source_len: 3,
            source_mtime: None,
            data_path: source.clone(),
            sent: 0,
            staging: None,
        };
        let record = file_record(1, &peer, 3, side);
        assert_eq!(
            missing_text(&record),
            crate::i18n::tr!(error_file_missing, path = source.display())
        );
        std::fs::remove_dir_all(&dir).ok();
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
                sent: 0,
                staging: None,
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
        // The save is synced to the disk, which can take seconds where the
        // disk is slow (GitHub's Mac runner): it gets the time it needs.
        let deadline = Instant::now() + Duration::from_secs(30);
        {
            let waiting = cancelled(&shared, 1, &ctl);
            tokio::pin!(waiting);
            while ctl.cancelling() && Instant::now() < deadline {
                let waited = tokio::time::timeout(Duration::from_millis(100), &mut waiting).await;
                assert!(waited.is_err(), "the transfer goes on");
            }
            let waited = tokio::time::timeout(Duration::from_millis(300), &mut waiting).await;
            assert!(waited.is_err(), "the transfer goes on");
        }
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
                head: None,
            },
        );
        let (receive, _rx) = Session::new(2, record.token, peer, Role::Receive);
        receive.set_record(record);
        let _finishing = receive.finishing().unwrap();
        shared.add_session(receive.clone());
        let ctl = shared.register(2, false);
        ctl.cancel();
        // Until the record on the disk says that the transfer has ended.
        let ended = || {
            let stored = session::records(&dir);
            stored.iter().any(|r| r.token == [2; 16] && r.ended)
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        {
            let waiting = cancelled(&shared, 2, &ctl);
            tokio::pin!(waiting);
            while !ended() && Instant::now() < deadline {
                let waited = tokio::time::timeout(Duration::from_millis(100), &mut waiting).await;
                assert!(waited.is_err(), "left to the last step");
            }
            let waited = tokio::time::timeout(Duration::from_millis(300), &mut waiting).await;
            assert!(waited.is_err(), "left to the last step");
        }
        assert!(ctl.cancelling(), "which ends the transfer soon");
        assert!(!cancel_waits_told(&events, 2), "nothing to tell the UI");
        assert!(ended());
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
                sent: 0,
                staging: None,
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
                sent: 0,
                staging: None,
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
                sent: 0,
                staging: None,
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
            sent: 0,
            staging: None,
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
                head: None,
            },
        );
        record.done_hash = Some([1; 32]);
        record.save(&data_dir).unwrap();

        let (tx, rx) = mpsc::channel();
        let net = start(
            SecretKey::generate(),
            data_dir.clone(),
            base.join("temp"),
            [],
            Limits::default(),
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
        assert_eq!(
            session::tombstone(&data_dir, &[4; 16]),
            Some(protocol::Ended::Completed)
        );
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
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let a = start(
            key_a.clone(),
            dir_a.clone(),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_a),
        );
        let b = start(
            key_b.clone(),
            dir_b.clone(),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx_b),
        );
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, key_b.public(), &base);
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
            let net = start(key.clone(), dir.clone(), base.join("temp"), [], Limits::default(), sink(tx));
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

    /// A send whose data has all gone out cannot be ended by a cancel while
    /// the receiver is away, and under another identity nobody could ask the
    /// receiver any more: the identity stays, and the cancel waits on.
    #[test]
    fn a_new_identity_waits_for_a_send_the_receiver_may_have() {
        let base = scratch("identity-busy");
        let stopped = send_until_verdict(&base, "maybe-there.bin", false);
        let (tx, rx) = mpsc::channel();
        let a = start(
            stopped.key_a.clone(),
            stopped.dir_a.clone(),
            base.join("temp"),
            [],
            Limits::default(),
            sink(tx),
        );
        wait_for(&rx, |e| matches!(e, Event::Restored { .. }));
        assert_eq!(
            a.change_identity(SecretKey::generate(), Duration::from_secs(60)),
            Err(IdentityError::Busy)
        );
        assert!(!stopped.dir_a.join("secret.key").exists());
        a.shutdown(Duration::from_secs(5));
        let records = session::records(&stopped.dir_a);
        assert_eq!(records.len(), 1);
        assert!(records[0].cancel_pending(), "{:?}", records[0]);
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

    /// A network that takes chat messages as `policy` says.
    fn start_chatting(
        key: SecretKey,
        dir: PathBuf,
        blocked: impl IntoIterator<Item = EndpointId>,
        policy: ChatPolicy,
        tx: mpsc::Sender<Event>,
    ) -> NetHandle {
        let temp = dir.join("temp");
        start_with_chat(key, dir, temp, blocked, Limits::default(), Some(policy), sink(tx))
    }

    fn named(name: &str) -> ChatPolicy {
        ChatPolicy {
            my_name: name.into(),
            ..Default::default()
        }
    }

    /// How the chat message `id` ended for its sender: `Ok` once it has
    /// arrived, or why it has not.
    #[track_caller]
    fn chat_result(rx: &mpsc::Receiver<Event>, id: u64) -> Result<(), String> {
        let event = wait_for(rx, |e| {
            matches!(e, Event::ChatSent { id: i, .. } | Event::ChatFailed { id: i, .. } if *i == id)
        });
        match event {
            Event::ChatFailed { error, .. } => Err(error),
            _ => Ok(()),
        }
    }

    /// Messages go both ways, each confirmed to its sender and shown with
    /// the name the sender chose, and so does word that someone is typing.
    #[test]
    fn a_chat_goes_both_ways() {
        let base = scratch("chat");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
        let (id_a, id_b) = (key_a.public(), key_b.public());
        let a = start_chatting(key_a, base.join("a"), [], named("Ana"), tx_a);
        let b = start_chatting(key_b, base.join("b"), [], named("  Bo  "), tx_b);
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);

        a.send(Command::Typing { to: id_b, on: true });
        let event = wait_for(&rx_b, |e| matches!(e, Event::Typing { .. }));
        assert!(matches!(event, Event::Typing { from, on: true } if from == id_a));
        a.send(Command::Chat {
            to: id_b,
            id: 1,
            text: "Šaljem ti slike.".into(),
        });
        let event = wait_for(&rx_b, |e| matches!(e, Event::Chat { .. }));
        let Event::Chat { from, name, text } = event else {
            unreachable!()
        };
        assert_eq!(
            (from, name.as_str(), text.as_str()),
            (id_a, "Ana", "Šaljem ti slike.")
        );
        assert_eq!(chat_result(&rx_a, 1), Ok(()));

        // The answer, with the name as it is shown.
        b.send(Command::Chat {
            to: id_a,
            id: 7,
            text: "Hvala!".into(),
        });
        let event = wait_for(&rx_a, |e| matches!(e, Event::Chat { .. }));
        let Event::Chat { from, name, text } = event else {
            unreachable!()
        };
        assert_eq!((from, name.as_str(), text.as_str()), (id_b, "Bo", "Hvala!"));
        assert_eq!(chat_result(&rx_b, 7), Ok(()));

        // Several at once arrive in their order.
        for id in 10..15 {
            a.send(Command::Chat {
                to: id_b,
                id,
                text: id.to_string(),
            });
        }
        let mut texts = Vec::new();
        while texts.len() < 5 {
            if let Event::Chat { text, .. } = wait_for(&rx_b, |e| matches!(e, Event::Chat { .. })) {
                texts.push(text);
            }
        }
        assert_eq!(texts, ["10", "11", "12", "13", "14"]);

        // B goes away: A is told that B stopped typing, and a message to
        // nobody fails.
        b.send(Command::Typing { to: id_a, on: true });
        wait_for(&rx_a, |e| matches!(e, Event::Typing { on: true, .. }));
        b.shutdown(Duration::from_secs(5));
        wait_for(&rx_a, |e| matches!(e, Event::Typing { on: false, .. }));
        a.send(Command::Chat {
            to: id_b,
            id: 20,
            text: "Jesi li tu?".into(),
        });
        assert!(chat_result(&rx_a, 20).is_err());
        a.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }

    /// A message from a blocked sender, and one from someone who is not a
    /// recipient where only recipients may write, is refused: its sender is
    /// told so, and nothing is seen here.
    #[test]
    fn a_chat_is_refused_unseen() {
        let base = scratch("chat-refused");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
        let (id_a, id_b) = (key_a.public(), key_b.public());
        let a = start_chatting(key_a, base.join("a"), [], named("Ana"), tx_a);
        let b = start_chatting(key_b, base.join("b"), [id_a], named("Bo"), tx_b);
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let say = |id| {
            a.send(Command::Chat {
                to: id_b,
                id,
                text: "Halo?".into(),
            });
            chat_result(&rx_a, id)
        };
        let refused = Err(crate::i18n::tr!(chat_error_refused).to_string());

        assert_eq!(say(1), refused);
        b.set_blocked([]);
        b.set_chat(ChatPolicy {
            from: crate::config::ChatFrom::Recipients,
            ..named("Bo")
        });
        assert_eq!(say(2), refused);
        let seen: Vec<Event> = rx_b.try_iter().collect();
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, Event::Chat { .. } | Event::Typing { .. })),
            "{seen:?}"
        );

        b.set_chat(ChatPolicy {
            from: crate::config::ChatFrom::Recipients,
            recipients: [id_a].into_iter().collect(),
            ..named("Bo")
        });
        assert_eq!(say(3), Ok(()));
        wait_for(&rx_b, |e| matches!(e, Event::Chat { .. }));
        // Blocked in the middle of the chat: the next message is refused on
        // the connection that is open.
        b.set_blocked([id_a]);
        assert!(say(4).is_err());
        let seen: Vec<Event> = rx_b.try_iter().collect();
        assert!(
            !seen.iter().any(|e| matches!(e, Event::Chat { .. })),
            "{seen:?}"
        );
        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }

    /// Where only recipients may write, a person whom the user wrote to may
    /// answer, on the connection of that message and on one of their own;
    /// not once the user has blocked them.
    #[test]
    fn an_answer_is_taken_from_someone_the_user_wrote_to() {
        let base = scratch("chat-answer");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
        let (id_a, id_b) = (key_a.public(), key_b.public());
        let a = start_chatting(key_a.clone(), base.join("a"), [], named("Ana"), tx_a);
        let only_recipients = ChatPolicy {
            from: crate::config::ChatFrom::Recipients,
            ..named("Bo")
        };
        let b = start_chatting(key_b, base.join("b"), [], only_recipients, tx_b);
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let say = |from: &NetHandle, rx: &mpsc::Receiver<Event>, to, id| {
            from.send(Command::Chat {
                to,
                id,
                text: "Halo?".into(),
            });
            chat_result(rx, id)
        };
        let refused = Err(crate::i18n::tr!(chat_error_refused).to_string());

        // Ana is no recipient of Bo's, who has not written to her.
        assert_eq!(say(&a, &rx_a, id_b, 1), refused);
        assert_eq!(say(&b, &rx_b, id_a, 2), Ok(()));
        wait_for(&rx_a, |e| matches!(e, Event::Chat { .. }));
        // Her answer, on the connection that his message came on.
        assert_eq!(say(&a, &rx_a, id_b, 3), Ok(()));
        wait_for(&rx_b, |e| matches!(e, Event::Chat { .. }));
        // And on a connection of her own, as after a restart of hers.
        a.shutdown(Duration::from_secs(5));
        let (tx_a, rx_a) = mpsc::channel();
        let a = start_chatting(key_a, base.join("a"), [], named("Ana"), tx_a);
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        assert_eq!(say(&a, &rx_a, id_b, 4), Ok(()));
        wait_for(&rx_b, |e| matches!(e, Event::Chat { .. }));
        // Blocking her ends that.
        b.set_blocked([id_a]);
        assert_eq!(say(&a, &rx_a, id_b, 5), refused);
        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }

    /// A send to a Dodavo that does not take this version's transfers ends
    /// with word of that, in the user's language, and not as a recipient
    /// that cannot be reached.
    #[test]
    fn a_send_to_another_version_says_so() {
        let base = scratch("other-version");
        let (tx_a, rx_a) = mpsc::channel();
        let a = start(
            SecretKey::generate(),
            base.join("a"),
            base.join("a").join("temp"),
            [],
            Limits::default(),
            sink(tx_a),
        );
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        // What a version with another protocol is to this one: an endpoint
        // that takes connections, but none of `protocol::ALPN`.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let other = rt.block_on(async {
            Endpoint::builder(presets::N0)
                .alpns(vec![b"dodavo/other-version".to_vec()])
                .address_lookup(MdnsAddressLookup::builder())
                .bind()
                .await
                .unwrap()
        });
        let to = other.id();
        let serving = {
            let other = other.clone();
            rt.spawn(async move {
                while let Some(incoming) = other.accept().await {
                    if let Ok(accepting) = incoming.accept() {
                        let _ = accepting.await;
                    }
                }
            })
        };
        let file = base.join("x.txt");
        std::fs::write(&file, b"x").unwrap();
        // Until the new endpoint is found (see `reach`).
        let deadline = Instant::now() + Duration::from_secs(120);
        let outcome = loop {
            let id = next_transfer_id();
            a.send(Command::Send {
                id,
                to,
                path: file.clone(),
                kind: Kind::File,
                my_name: "A".into(),
                peer_name: "B".into(),
            });
            match wait_finished(&rx_a, id) {
                Outcome::Unreachable(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(250));
                }
                outcome => break outcome,
            }
        };
        let expected = crate::i18n::tr!(error_other_version);
        assert!(
            matches!(&outcome, Outcome::Failed(text) if text == expected),
            "{outcome:?}"
        );
        serving.abort();
        rt.block_on(other.close());
        a.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }

    /// A side whose user interface shows no chats takes no chat connection,
    /// and who writes to it is told that it cannot chat, as is one that
    /// shows no chats itself.
    #[test]
    fn a_side_without_chat_says_so() {
        let base = scratch("chat-none");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let (key_a, key_b) = (SecretKey::generate(), SecretKey::generate());
        let (id_a, id_b) = (key_a.public(), key_b.public());
        let a = start_chatting(key_a, base.join("a"), [], named("Ana"), tx_a);
        let b = start(
            key_b,
            base.join("b"),
            base.join("b").join("temp"),
            [],
            Limits::default(),
            sink(tx_b),
        );
        wait_for(&rx_a, |e| matches!(e, Event::Online(true)));
        wait_for(&rx_b, |e| matches!(e, Event::Online(true)));
        reach(&a, &rx_a, &b, &rx_b, id_b, &base);
        let cannot = Err(crate::i18n::os_tr!(chat_error_cannot_chat).to_string());

        a.send(Command::Chat {
            to: id_b,
            id: 1,
            text: "Halo?".into(),
        });
        assert_eq!(chat_result(&rx_a, 1), cannot);
        b.send(Command::Chat {
            to: id_a,
            id: 2,
            text: "Halo?".into(),
        });
        assert_eq!(chat_result(&rx_b, 2), cannot);
        let seen: Vec<Event> = rx_a.try_iter().chain(rx_b.try_iter()).collect();
        assert!(
            !seen.iter().any(|e| matches!(e, Event::Chat { .. })),
            "{seen:?}"
        );
        a.shutdown(Duration::from_secs(5));
        b.shutdown(Duration::from_secs(5));
        std::fs::remove_dir_all(&base).ok();
    }
}
