//! Chat: text messages between two people, over connections of their own
//! (`protocol::CHAT_ALPN`), which the transfers know nothing of.
//!
//! Two people share one connection with one stream, whoever dialed, and
//! either writes on it (see [`ChatMsg`]). It is dialed when a message has to
//! go out and none is there, and closed once nothing has been said for a
//! while. What is sent to a person waits in one [`Queue`], whichever
//! connection takes it from there, so it arrives in the order it was sent
//! in, also when both people dial at the same moment. A message that cannot
//! be delivered now is reported as failed, and the user sends it again: the
//! dial for it is tried again only for a short while ([`tries_again`]). A
//! side takes chat connections only where its user interface shows chats
//! (see `net::start_with_chat`): one that does not refuses the connection,
//! and the sender is told that the other side cannot chat.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointId,
    endpoint::{Connection, RecvStream, SendStream},
};
use tokio::sync::{Notify, mpsc};

use super::{
    CLOSE_OK, Event, Shared,
    protocol::{self, ChatMsg},
};
use crate::{
    config::ChatFrom,
    i18n::{os_tr, tr},
};

/// Who may send chat messages, and the name this side gives of itself (see
/// `config::Config::chat_policy`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatPolicy {
    pub from: ChatFrom,
    /// The recipients, for `ChatFrom::Recipients`.
    pub recipients: HashSet<EndpointId>,
    /// The display name the user chose for themselves.
    pub my_name: String,
}

/// How long a dial may take. Shorter than a transfer's: someone waits for it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a message waits for a connection while a transfer with that
/// person has one (see [`tries_again`]).
const PATIENCE: Duration = Duration::from_secs(45);
/// The pause between a dial that failed and the next one.
const RETRY_AFTER: Duration = Duration::from_secs(1);
/// How long the other side may take to answer `Hello`.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the other side may take to confirm a `Text`. The connection
/// delivers it whatever else it carries, and one whose other side is gone
/// ends by itself well before this.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// A connection on which nothing was said for this long is closed.
const IDLE: Duration = Duration::from_secs(300);
/// After a dial that failed, the user's typing does not dial that person
/// again for this long (a message does).
const QUIET: Duration = Duration::from_secs(60);
/// How long the answers of a person whom a message of the user's reached are
/// taken where only recipients may write (see [`takes`]): long enough for an
/// answer that comes the next morning. Kept in memory, so that it ends with
/// the program at the latest.
const ANSWERS: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest name that a peer may give of itself, in characters.
const MAX_NAME: usize = 100;

/// What goes out on a chat connection.
pub(crate) enum Out {
    Text { id: u64, text: String },
    Typing(bool),
}

/// What waits to go out, and since when.
struct Queued {
    out: Out,
    since: Instant,
}

/// Which connection sends what waits for a person.
struct Turn {
    /// The connection that sends, or the dial for it.
    sender: u64,
    /// A connection that came in from that person meanwhile. It sends once
    /// `sender` has the answers to all it sent, so that nothing it carries
    /// overtakes what `sender` did.
    heir: Option<u64>,
    waiting: VecDeque<Queued>,
}

/// What is to go out to a person, in the order it was sent in, and which of
/// the connections with that person takes it from here. There may be two
/// connections, when both people dialed, and each side sends on the newest
/// that came in: the other side dialed it because it had none, so an older
/// one may be gone without this side knowing yet. A connection that neither
/// sends nor is next goes on taking what arrives on it.
struct Queue {
    turn: Mutex<Turn>,
    changed: Notify,
}

impl Queue {
    fn new(sender: u64) -> Queue {
        Queue {
            turn: Mutex::new(Turn {
                sender,
                heir: None,
                waiting: VecDeque::new(),
            }),
            changed: Notify::new(),
        }
    }

    fn push(&self, out: Out) {
        self.turn.lock().unwrap().waiting.push_back(Queued {
            out,
            since: Instant::now(),
        });
        self.changed.notify_waiters();
    }

    /// The next thing for the connection `serial` to send, or `None` once
    /// it does not send any more. `idle` says that it waits for no answer,
    /// which is when a connection that came in takes over from it. Nothing
    /// is lost when this is given up while it waits.
    async fn next(&self, serial: u64, idle: bool) -> Option<Queued> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            // Before the look, so that a change right after it is not missed.
            changed.as_mut().enable();
            {
                let mut turn = self.turn.lock().unwrap();
                if turn.sender == serial {
                    match turn.heir {
                        None => {
                            if let Some(queued) = turn.waiting.pop_front() {
                                return Some(queued);
                            }
                        }
                        Some(heir) if idle => {
                            turn.sender = heir;
                            turn.heir = None;
                            drop(turn);
                            self.changed.notify_waiters();
                            return None;
                        }
                        Some(_) => {}
                    }
                } else if turn.heir != Some(serial) {
                    return None;
                }
            }
            changed.await;
        }
    }

    /// Waits until a connection that came in has taken the place of the
    /// dial `serial`, which has sent nothing yet.
    async fn taken_over(&self, serial: u64) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut turn = self.turn.lock().unwrap();
                if turn.sender != serial {
                    return;
                }
                if let Some(heir) = turn.heir.take() {
                    turn.sender = heir;
                    drop(turn);
                    self.changed.notify_waiters();
                    return;
                }
            }
            changed.await;
        }
    }
}

/// What became of a queue whose dial failed (see [`Chats::dial_failed`]).
#[derive(Debug, PartialEq, Eq)]
enum AfterDial {
    /// A connection that came in sends what waits.
    TakenOver,
    /// These messages failed, and the rest is dialed for again.
    Again(Vec<u64>),
    /// These messages failed, and nothing waits any more.
    Over(Vec<u64>),
}

/// The chat connections of an endpoint that takes them.
pub(crate) struct Chats {
    pub(crate) policy: Arc<Mutex<ChatPolicy>>,
    /// What waits for each person who has a connection or a dial. Locked
    /// before a queue's own lock.
    queues: Mutex<HashMap<EndpointId, Arc<Queue>>>,
    quiet: Mutex<HashMap<EndpointId, Instant>>,
    /// The people whom a message of the user's reached, each with the time
    /// until which their answers are taken (see [`ANSWERS`]).
    answers: Mutex<HashMap<EndpointId, Instant>>,
    serial: AtomicU64,
}

impl Chats {
    pub(crate) fn new(policy: Arc<Mutex<ChatPolicy>>) -> Chats {
        Chats {
            policy,
            queues: Mutex::default(),
            quiet: Mutex::default(),
            answers: Mutex::default(),
            serial: AtomicU64::new(1),
        }
    }

    /// A message of the user's reached `peer` just now: who is written to
    /// may answer for a while, whoever may write otherwise.
    fn wrote_to(&self, peer: EndpointId) {
        let now = Instant::now();
        let mut answers = self.answers.lock().unwrap();
        answers.retain(|_, until| now < *until);
        answers.insert(peer, now + ANSWERS);
    }

    /// Whether `peer` may answer a message of the user's (see `wrote_to`).
    fn may_answer(&self, peer: &EndpointId) -> bool {
        let answers = self.answers.lock().unwrap();
        answers.get(peer).is_some_and(|until| Instant::now() < *until)
    }

    /// A connection came in from `peer`: it sends what goes to that person
    /// from now on, after a connection that sends now.
    fn join(&self, peer: EndpointId) -> (u64, Arc<Queue>) {
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        let mut queues = self.queues.lock().unwrap();
        if let Some(queue) = queues.get(&peer) {
            queue.turn.lock().unwrap().heir = Some(serial);
            queue.changed.notify_waiters();
            return (serial, queue.clone());
        }
        let queue = Arc::new(Queue::new(serial));
        queues.insert(peer, queue.clone());
        (serial, queue)
    }

    /// The connection `serial` with `peer` is over. What waited for it and
    /// no other connection sends.
    fn leave(&self, peer: &EndpointId, serial: u64, queue: &Arc<Queue>) -> Vec<Queued> {
        let mut queues = self.queues.lock().unwrap();
        let mut turn = queue.turn.lock().unwrap();
        if turn.heir == Some(serial) {
            turn.heir = None;
            queue.changed.notify_waiters();
            return Vec::new();
        }
        if turn.sender != serial {
            return Vec::new();
        }
        if let Some(heir) = turn.heir.take() {
            turn.sender = heir;
            queue.changed.notify_waiters();
            return Vec::new();
        }
        // Under the lock that a send takes, so nothing more is queued here.
        if queues.get(peer).is_some_and(|q| Arc::ptr_eq(q, queue)) {
            queues.remove(peer);
        }
        turn.waiting.drain(..).collect()
    }

    /// The dial `serial` for `peer` failed. A message that `keeps` says to
    /// keep, by how long it has waited, is dialed for again; the others
    /// failed. Word of the user's typing is not kept: it is old by then.
    fn dial_failed(
        &self,
        peer: &EndpointId,
        serial: u64,
        queue: &Arc<Queue>,
        keeps: impl Fn(Duration) -> bool,
    ) -> AfterDial {
        let mut queues = self.queues.lock().unwrap();
        let mut turn = queue.turn.lock().unwrap();
        if turn.sender != serial {
            return AfterDial::TakenOver;
        }
        if let Some(heir) = turn.heir.take() {
            turn.sender = heir;
            queue.changed.notify_waiters();
            return AfterDial::TakenOver;
        }
        let mut failed = Vec::new();
        turn.waiting.retain(|queued| match &queued.out {
            Out::Text { id, .. } => {
                let keep = keeps(queued.since.elapsed());
                if !keep {
                    failed.push(*id);
                }
                keep
            }
            Out::Typing(_) => false,
        });
        if !turn.waiting.is_empty() {
            return AfterDial::Again(failed);
        }
        if queues.get(peer).is_some_and(|q| Arc::ptr_eq(q, queue)) {
            queues.remove(peer);
        }
        AfterDial::Over(failed)
    }
}

/// Whether a message that has waited for `age` is dialed for once more
/// after a dial that failed. `busy` says that a transfer with that person
/// has a connection: the person is there then, and it is the transfers'
/// data that holds the dial up, so the message waits longer. Otherwise the
/// user hears that the person cannot be reached as soon as one dial would
/// have told.
fn tries_again(age: Duration, busy: bool) -> bool {
    age < if busy { PATIENCE } else { CONNECT_TIMEOUT }
}

/// Whether messages from `peer` are taken now: never from a blocked sender,
/// and where only recipients may write, from them and from a person whom
/// the user wrote to not long ago (see `Chats::wrote_to`), whose answer
/// would otherwise be refused like any message of theirs.
fn takes(shared: &Shared, chats: &Chats, peer: &EndpointId) -> bool {
    if shared.blocked.lock().unwrap().contains(peer) {
        return false;
    }
    let by_policy = {
        let policy = chats.policy.lock().unwrap();
        policy.from == ChatFrom::Anyone || policy.recipients.contains(peer)
    };
    by_policy || chats.may_answer(peer)
}

/// The name a peer gave of itself, as far as it is shown.
fn shown_name(name: &str) -> String {
    name.trim().chars().take(MAX_NAME).collect()
}

/// Sends `out` to `to`: on the connection there is, or on one that is
/// dialed for it. A message is answered with `Event::ChatSent` or
/// `Event::ChatFailed`; whether the other person learns of the user's
/// typing is never reported.
pub(crate) fn send(shared: &Arc<Shared>, endpoint: &Endpoint, to: EndpointId, out: Out) {
    let fail = |out: Out, error: &str| {
        if let Out::Text { id, .. } = out {
            shared.emit(Event::ChatFailed {
                to,
                id,
                error: error.into(),
            });
        }
    };
    let Some(chats) = shared.chat.clone() else {
        return fail(out, os_tr!(chat_error_cannot_chat));
    };
    let (serial, queue) = {
        let mut queues = chats.queues.lock().unwrap();
        if let Some(queue) = queues.get(&to) {
            return queue.push(out);
        }
        match &out {
            // Nobody is told that the user stopped, and nobody who cannot
            // be reached is dialed for every key.
            Out::Typing(false) => return,
            Out::Typing(true) => {
                let quiet = chats.quiet.lock().unwrap().get(&to).copied();
                if quiet.is_some_and(|until| Instant::now() < until) {
                    return;
                }
            }
            Out::Text { .. } => {}
        }
        if shared.shutting_down.load(Ordering::SeqCst) {
            drop(queues);
            return fail(out, tr!(error_connection_lost));
        }
        let serial = chats.serial.fetch_add(1, Ordering::Relaxed);
        let queue = Arc::new(Queue::new(serial));
        queue.push(out);
        queues.insert(to, queue.clone());
        (serial, queue)
    };
    tokio::spawn(dial_for(
        shared.clone(),
        chats,
        endpoint.clone(),
        to,
        serial,
        queue,
    ));
}

/// Dials `to` for what waits in `queue`, as its connection `serial`, and
/// runs the connection.
async fn dial_for(
    shared: Arc<Shared>,
    chats: Arc<Chats>,
    endpoint: Endpoint,
    to: EndpointId,
    serial: u64,
    queue: Arc<Queue>,
) {
    // Whether what waits is still this dial's to send.
    let mut mine = true;
    loop {
        let dialed = {
            let dialing = dial(&chats, &endpoint, to);
            tokio::pin!(dialing);
            loop {
                tokio::select! {
                    dialed = &mut dialing => break dialed,
                    // That person's own connection came in first, and what
                    // waits goes out on it. This dial goes on: the other
                    // side may have left its messages to it the same way.
                    () = queue.taken_over(serial), if mine => mine = false,
                }
            }
        };
        let failed = match dialed {
            Ok((conn, send, recv, name)) => {
                chats.quiet.lock().unwrap().remove(&to);
                return converse(shared, chats, conn, send, recv, name, serial, queue).await;
            }
            Err(failed) => failed,
        };
        if !mine {
            return;
        }
        chats
            .quiet
            .lock()
            .unwrap()
            .insert(to, Instant::now() + QUIET);
        let again = failed.passing && !shared.shutting_down.load(Ordering::SeqCst);
        let busy = shared.connected_to(&to);
        let after = chats.dial_failed(&to, serial, &queue, |age| again && tries_again(age, busy));
        let (failed_now, again) = match after {
            AfterDial::TakenOver => return,
            AfterDial::Again(ids) => (ids, true),
            AfterDial::Over(ids) => (ids, false),
        };
        for id in failed_now {
            shared.emit(Event::ChatFailed {
                to,
                id,
                error: failed.text.clone(),
            });
        }
        if !again {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(RETRY_AFTER) => {}
            () = queue.taken_over(serial) => return,
        }
    }
}

/// Why a dial failed.
struct DialError {
    /// For the user.
    text: String,
    /// Whether another try may go better: not where the other side refuses
    /// messages, has no chat or cannot be found.
    passing: bool,
}

impl DialError {
    fn passing(text: impl Into<String>) -> DialError {
        DialError {
            text: text.into(),
            passing: true,
        }
    }

    fn lasting(text: impl Into<String>) -> DialError {
        DialError {
            text: text.into(),
            passing: false,
        }
    }
}

/// Connects to `to` and greets it. The streams, and the name the other side
/// gave of itself; or why not, for the user.
async fn dial(
    chats: &Chats,
    endpoint: &Endpoint,
    to: EndpointId,
) -> Result<(Connection, SendStream, RecvStream, String), DialError> {
    use iroh::endpoint::{ConnectError, ConnectWithOptsError};
    let alias = crate::logging::peer(&to);
    let lost = |what: &str| {
        crate::log_info!("chat with {alias}: the greeting failed ({what})");
        DialError::passing(tr!(error_connection_lost))
    };
    let connecting = endpoint.connect(to, protocol::CHAT_ALPN);
    let conn = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(Ok(conn)) => conn,
        // Before anything else: for a transfer the same answer means
        // another version (see `net::connect_error_text`).
        Ok(Err(e)) if super::no_such_protocol(&e) => {
            crate::log_info!("chat with {alias}: Dodavo there has no chat");
            return Err(DialError::lasting(os_tr!(chat_error_cannot_chat)));
        }
        Ok(Err(e)) => {
            crate::log_info!(
                "chat with {alias}: dial failed: {}",
                crate::logging::connect_error(&e)
            );
            let text = super::connect_error_text(&e);
            let unknown = matches!(
                e,
                ConnectError::Connect {
                    source: ConnectWithOptsError::NoAddress { .. },
                    ..
                }
            );
            return Err(if unknown {
                DialError::lasting(text)
            } else {
                DialError::passing(text)
            });
        }
        Err(_) => {
            crate::log_info!(
                "chat with {alias}: no connection in {} s",
                CONNECT_TIMEOUT.as_secs()
            );
            return Err(DialError::passing(tr!(error_timeout)));
        }
    };
    let greeted = async {
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| lost(&crate::logging::connection_error(&e)))?;
        let name = chats.policy.lock().unwrap().my_name.clone();
        protocol::write_as(&mut send, &ChatMsg::Hello { name })
            .await
            .map_err(|e| lost(&crate::logging::cause(&e)))?;
        match protocol::read_as::<_, ChatMsg>(&mut recv).await {
            Ok(ChatMsg::Welcome { name }) => Ok((send, recv, shown_name(&name))),
            Ok(ChatMsg::Refused) => {
                crate::log_info!("chat with {alias}: refused there");
                Err(DialError::lasting(tr!(chat_error_refused)))
            }
            Ok(other) => Err(lost(other.kind_name())),
            Err(e) => Err(lost(&crate::logging::cause(&e))),
        }
    };
    match tokio::time::timeout(ANSWER_TIMEOUT, greeted).await {
        Ok(Ok((send, recv, name))) => {
            crate::log_info!("chat with {alias}: connected");
            Ok((conn, send, recv, name))
        }
        Ok(Err(error)) => {
            conn.close(CLOSE_OK.into(), b"done");
            Err(error)
        }
        Err(_) => {
            crate::log_info!(
                "chat with {alias}: no answer to the greeting in {} s",
                ANSWER_TIMEOUT.as_secs()
            );
            conn.close(CLOSE_OK.into(), b"done");
            Err(DialError::passing(tr!(error_timeout)))
        }
    }
}

/// Handles a chat connection that came in.
pub(crate) async fn accept(shared: Arc<Shared>, conn: Connection) {
    let Some(chats) = shared.chat.clone() else {
        return;
    };
    let peer = conn.remote_id();
    let alias = crate::logging::peer(&peer);
    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
        crate::log_info!("chat from {alias}: the connection ended before its stream");
        return;
    };
    let first = protocol::read_as::<_, ChatMsg>(&mut recv);
    let name = match tokio::time::timeout(ANSWER_TIMEOUT, first).await {
        Ok(Ok(ChatMsg::Hello { name })) => name,
        Ok(Ok(other)) => {
            crate::log_info!(
                "chat from {alias}: began with {}; ignored",
                other.kind_name()
            );
            return;
        }
        Ok(Err(e)) => {
            crate::log_info!(
                "chat from {alias}: no greeting: {}",
                crate::logging::cause(&e)
            );
            return;
        }
        Err(_) => {
            crate::log_info!(
                "chat from {alias}: no greeting in {} s",
                ANSWER_TIMEOUT.as_secs()
            );
            return;
        }
    };
    let exiting = shared.shutting_down.load(Ordering::SeqCst);
    if exiting || !takes(&shared, &chats, &peer) {
        crate::log_info!(
            "chat from {alias}: refused ({})",
            if exiting {
                "exiting"
            } else if shared.blocked.lock().unwrap().contains(&peer) {
                "a blocked sender"
            } else {
                "only recipients may write"
            }
        );
        // Nothing here sees it; the connection stays until the other side
        // has read the answer.
        let _ = protocol::write_as(&mut send, &ChatMsg::Refused).await;
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        return;
    }
    let my_name = chats.policy.lock().unwrap().my_name.clone();
    if let Err(e) = protocol::write_as(&mut send, &ChatMsg::Welcome { name: my_name }).await {
        crate::log_info!(
            "chat from {alias}: the welcome failed: {}",
            crate::logging::cause(&e)
        );
        return;
    }
    crate::log_info!("chat from {alias}: connected");
    let (serial, queue) = chats.join(peer);
    converse(
        shared,
        chats,
        conn,
        send,
        recv,
        shown_name(&name),
        serial,
        queue,
    )
    .await;
}

/// Runs a chat connection with the person at its other end, whose name for
/// themselves is `name`, until it ends. It is the connection `serial` of
/// `queue`, and sends what waits there while it is its turn.
#[allow(clippy::too_many_arguments)]
async fn converse(
    shared: Arc<Shared>,
    chats: Arc<Chats>,
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    name: String,
    serial: u64,
    queue: Arc<Queue>,
) {
    let peer = conn.remote_id();
    // Reading a message cannot be given up half way, so it has a task of
    // its own.
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        while let Ok(msg) = protocol::read_as::<_, ChatMsg>(&mut recv).await {
            if in_tx.send(msg).is_err() {
                break;
            }
        }
    });
    // The user's messages that the other side has not confirmed, since when.
    let mut waiting: HashMap<u64, Instant> = HashMap::new();
    let mut peer_typing = false;
    let mut last = Instant::now();
    let mut sending = true;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // Why it ended, for the log.
    let mut why = "idle".to_string();
    // Why it ended, for the messages that wait; `None` when nothing waits.
    let error: Option<String> = loop {
        tokio::select! {
            queued = queue.next(serial, waiting.is_empty()), if sending => {
                let Some(queued) = queued else {
                    // Another connection with this person sends from now on.
                    sending = false;
                    continue;
                };
                let msg = match queued.out {
                    Out::Text { id, text } => {
                        waiting.insert(id, Instant::now());
                        ChatMsg::Text { id, text }
                    }
                    Out::Typing(on) => ChatMsg::Typing { on },
                };
                last = Instant::now();
                if let Err(e) = protocol::write_as(&mut send, &msg).await {
                    why = format!("a write failed: {}", crate::logging::cause(&e));
                    break Some(tr!(error_connection_lost).into());
                }
            }
            msg = in_rx.recv() => {
                let Some(msg) = msg else {
                    why = format!(
                        "the other side's stream ended; the connection says {}",
                        conn.close_reason()
                            .map_or_else(|| "nothing".into(), |r| crate::logging::connection_error(&r))
                    );
                    break Some(tr!(error_connection_lost).into());
                };
                match msg {
                    ChatMsg::Text { .. } | ChatMsg::Typing { .. }
                        if !takes(&shared, &chats, &peer) =>
                    {
                        // The user blocked this person meanwhile, or takes
                        // messages from recipients only now, and has not
                        // written to this one.
                        why = "refused: blocked, or no longer allowed to write".into();
                        let _ = protocol::write_as(&mut send, &ChatMsg::Refused).await;
                        let _ = send.finish();
                        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                        break Some(tr!(error_connection_lost).into());
                    }
                    ChatMsg::Text { id, text } => {
                        // Exiting: nobody would see it, so it is not
                        // confirmed, and its sender is told that it failed.
                        if shared.shutting_down.load(Ordering::SeqCst) {
                            why = "a message came while exiting".into();
                            break None;
                        }
                        last = Instant::now();
                        if text.len() > protocol::MAX_CHAT_TEXT || text.trim().is_empty() {
                            crate::log_info!(
                                "chat with {}: a message too long or empty was left out",
                                crate::logging::peer(&peer)
                            );
                            continue;
                        }
                        peer_typing = false;
                        shared.emit(Event::Chat {
                            from: peer,
                            name: name.clone(),
                            text,
                        });
                        if let Err(e) = protocol::write_as(&mut send, &ChatMsg::Received { id }).await {
                            why = format!("a write failed: {}", crate::logging::cause(&e));
                            break Some(tr!(error_connection_lost).into());
                        }
                    }
                    ChatMsg::Received { id } => {
                        if waiting.remove(&id).is_some() {
                            chats.wrote_to(peer);
                            shared.emit(Event::ChatSent { to: peer, id });
                        }
                    }
                    ChatMsg::Typing { on } => {
                        last = Instant::now();
                        peer_typing = on;
                        shared.emit(Event::Typing { from: peer, on });
                    }
                    ChatMsg::Refused => {
                        why = "refused there".into();
                        break Some(tr!(chat_error_refused).into());
                    }
                    ChatMsg::Hello { .. } | ChatMsg::Welcome { .. } => {}
                }
            }
            _ = tick.tick() => {
                let late: Vec<u64> = waiting
                    .iter()
                    .filter(|(_, since)| since.elapsed() >= CONFIRM_TIMEOUT)
                    .map(|(id, _)| *id)
                    .collect();
                for id in late {
                    crate::log_info!(
                        "chat with {}: message {id} not confirmed in {} s",
                        crate::logging::peer(&peer),
                        CONFIRM_TIMEOUT.as_secs()
                    );
                    waiting.remove(&id);
                    shared.emit(Event::ChatFailed {
                        to: peer,
                        id,
                        error: tr!(chat_error_no_answer).into(),
                    });
                }
                if waiting.is_empty() && last.elapsed() >= IDLE {
                    break None;
                }
            }
        }
    };
    reader.abort();
    crate::log_info!(
        "chat with {}: the connection ended ({why})",
        crate::logging::peer(&peer)
    );
    // First, so that nothing more waits for this connection.
    let unsent = chats.leave(&peer, serial, &queue);
    let error = error.unwrap_or_else(|| tr!(error_connection_lost).into());
    let unsent = unsent.into_iter().filter_map(|queued| match queued.out {
        Out::Text { id, .. } => Some(id),
        Out::Typing(_) => None,
    });
    for id in waiting.into_keys().chain(unsent) {
        shared.emit(Event::ChatFailed {
            to: peer,
            id,
            error: error.clone(),
        });
    }
    if peer_typing {
        shared.emit(Event::Typing {
            from: peer,
            on: false,
        });
    }
    conn.close(CLOSE_OK.into(), b"done");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(id: u64) -> Out {
        Out::Text {
            id,
            text: id.to_string(),
        }
    }

    /// The message that the connection `serial` gets to send now, if any.
    async fn now(queue: &Queue, serial: u64, idle: bool) -> Option<Option<u64>> {
        let next = queue.next(serial, idle);
        match tokio::time::timeout(Duration::from_millis(20), next).await {
            Ok(Some(Queued {
                out: Out::Text { id, .. },
                ..
            })) => Some(Some(id)),
            Ok(Some(_)) => panic!("not a message"),
            Ok(None) => Some(None),
            Err(_) => None,
        }
    }

    fn person() -> EndpointId {
        iroh::SecretKey::generate().public()
    }

    /// A message waits for a connection only as long as one dial takes,
    /// unless a transfer shows that the person is there.
    #[test]
    fn a_message_waits_longer_while_a_transfer_is_connected() {
        let second = Duration::from_secs(1);
        assert!(tries_again(Duration::ZERO, false));
        assert!(tries_again(CONNECT_TIMEOUT - second, false));
        assert!(!tries_again(CONNECT_TIMEOUT, false));
        assert!(tries_again(CONNECT_TIMEOUT, true));
        assert!(tries_again(PATIENCE - second, true));
        assert!(!tries_again(PATIENCE, true));
    }

    /// Both people dialed: what waited for this side's dial goes out on the
    /// connection that came in, in the order it was sent in, and the dial
    /// sends nothing.
    #[tokio::test]
    async fn a_connection_that_came_in_takes_what_waited_for_a_dial() {
        let chats = Chats::new(Arc::default());
        let peer = person();
        let dial = 100;
        let queue = Arc::new(Queue::new(dial));
        chats.queues.lock().unwrap().insert(peer, queue.clone());
        queue.push(text(1));
        queue.push(text(2));
        let pending = tokio::time::timeout(Duration::from_millis(20), queue.taken_over(dial));
        assert!(pending.await.is_err(), "nothing came in yet");

        let (came_in, same) = chats.join(peer);
        assert!(Arc::ptr_eq(&queue, &same));
        assert_eq!(
            now(&queue, came_in, true).await,
            None,
            "the dial has not let go"
        );
        queue.taken_over(dial).await;
        queue.push(text(3));
        for id in 1..=3 {
            assert_eq!(now(&queue, came_in, false).await, Some(Some(id)));
        }
        assert_eq!(now(&queue, dial, true).await, Some(None));
        // The dial ends, and the queue stays with the connection.
        assert!(chats.leave(&peer, dial, &queue).is_empty());
        assert!(chats.queues.lock().unwrap().contains_key(&peer));
        queue.push(text(4));
        let left = chats.leave(&peer, came_in, &queue);
        assert_eq!(left.len(), 1);
        assert!(!chats.queues.lock().unwrap().contains_key(&peer));
    }

    /// A connection that came in sends only once the one before it has the
    /// answers to what it sent, and goes on from where that one stopped.
    #[tokio::test]
    async fn a_new_connection_waits_for_the_answers_of_the_one_before() {
        let chats = Chats::new(Arc::default());
        let peer = person();
        let (first, queue) = chats.join(peer);
        queue.push(text(1));
        queue.push(text(2));
        assert_eq!(now(&queue, first, true).await, Some(Some(1)));

        let (second, _) = chats.join(peer);
        queue.push(text(3));
        // The first waits for the answer to 1, and takes nothing more.
        assert_eq!(now(&queue, first, false).await, None);
        assert_eq!(now(&queue, second, true).await, None);
        // The answer came.
        assert_eq!(now(&queue, first, true).await, Some(None));
        assert_eq!(now(&queue, second, false).await, Some(Some(2)));
        assert_eq!(now(&queue, second, false).await, Some(Some(3)));
        assert_eq!(now(&queue, first, true).await, Some(None));
    }

    /// When the connection that was to take over ends first, the one that
    /// sends goes on; when the one that sends ends, the other takes over at
    /// once; and a third one takes the place of the second.
    #[tokio::test]
    async fn connections_that_end_leave_the_queue_to_the_others() {
        let chats = Chats::new(Arc::default());
        let peer = person();
        let (first, queue) = chats.join(peer);
        let (second, _) = chats.join(peer);
        queue.push(text(1));
        assert_eq!(now(&queue, first, false).await, None);
        assert!(chats.leave(&peer, second, &queue).is_empty());
        assert_eq!(now(&queue, first, false).await, Some(Some(1)));

        let (third, _) = chats.join(peer);
        let (fourth, _) = chats.join(peer);
        assert_eq!(now(&queue, third, true).await, Some(None), "replaced");
        queue.push(text(2));
        assert!(chats.leave(&peer, first, &queue).is_empty());
        assert_eq!(now(&queue, fourth, false).await, Some(Some(2)));
        queue.push(text(3));
        assert_eq!(chats.leave(&peer, fourth, &queue).len(), 1);
        assert!(chats.queues.lock().unwrap().is_empty());
    }

    /// After a dial that failed, the messages that have waited long enough
    /// fail, and the others are dialed for again; a connection that came in
    /// meanwhile takes all of them.
    #[tokio::test]
    async fn a_failed_dial_keeps_the_messages_that_can_still_wait() {
        let chats = Chats::new(Arc::default());
        let peer = person();
        let dial = 7;
        let queue = Arc::new(Queue::new(dial));
        chats.queues.lock().unwrap().insert(peer, queue.clone());
        queue.push(Out::Typing(true));
        queue.push(text(1));
        std::thread::sleep(Duration::from_millis(60));
        queue.push(text(2));
        let young = |age: Duration| age < Duration::from_millis(50);
        assert_eq!(
            chats.dial_failed(&peer, dial, &queue, young),
            AfterDial::Again(vec![1])
        );
        assert!(chats.queues.lock().unwrap().contains_key(&peer));
        assert_eq!(
            chats.dial_failed(&peer, dial, &queue, |_| false),
            AfterDial::Over(vec![2])
        );
        assert!(chats.queues.lock().unwrap().is_empty());

        let queue = Arc::new(Queue::new(dial));
        chats.queues.lock().unwrap().insert(peer, queue.clone());
        queue.push(text(3));
        let (came_in, _) = chats.join(peer);
        assert_eq!(
            chats.dial_failed(&peer, dial, &queue, |_| false),
            AfterDial::TakenOver
        );
        assert_eq!(now(&queue, came_in, false).await, Some(Some(3)));
    }
}
