//! Chat: text messages between two people, over connections of their own
//! (`protocol::CHAT_ALPN`), which the transfers know nothing of.
//!
//! Two people share one connection with one stream, whoever dialed, and
//! either writes on it (see [`ChatMsg`]). It is dialed when a message has to
//! go out and none is there, and closed once nothing has been said for a
//! while. Nothing is tried again: a message that cannot be delivered now is
//! reported as failed, and the user sends it again. A side takes chat
//! connections only where its user interface shows chats (see
//! `net::start_with_chat`): one that does not refuses the connection, and
//! the sender is told that the other side cannot chat.

use std::{
    collections::{HashMap, HashSet},
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
use tokio::sync::mpsc;

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
/// How long the other side may take to answer `Hello`, and a `Text`.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
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

/// The connection to a person, or the dial for one, as who sends sees it.
struct Link {
    /// Which of the connections with that person this is.
    serial: u64,
    tx: mpsc::UnboundedSender<Out>,
}

/// The chat connections of an endpoint that takes them.
pub(crate) struct Chats {
    pub(crate) policy: Arc<Mutex<ChatPolicy>>,
    links: Mutex<HashMap<EndpointId, Link>>,
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
            links: Mutex::default(),
            quiet: Mutex::default(),
            answers: Mutex::default(),
            serial: AtomicU64::new(1),
        }
    }

    /// Makes a link to `peer` the one that sends go to.
    fn link(&self, peer: EndpointId) -> (u64, mpsc::UnboundedReceiver<Out>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        self.links.lock().unwrap().insert(peer, Link { serial, tx });
        (serial, rx)
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

    /// The link `serial` to `peer` is over. Another one that took its place
    /// meanwhile stays.
    fn unlink(&self, peer: &EndpointId, serial: u64) {
        let mut links = self.links.lock().unwrap();
        if links.get(peer).is_some_and(|l| l.serial == serial) {
            links.remove(peer);
        }
    }
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

/// Reports every `Text` that still waits in `rx` as failed.
fn fail_queued(shared: &Shared, to: EndpointId, rx: &mut mpsc::UnboundedReceiver<Out>, error: &str) {
    while let Ok(out) = rx.try_recv() {
        if let Out::Text { id, .. } = out {
            shared.emit(Event::ChatFailed {
                to,
                id,
                error: error.into(),
            });
        }
    }
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
    let (serial, rx) = {
        let mut links = chats.links.lock().unwrap();
        let out = match links.get(&to) {
            Some(link) => match link.tx.send(out) {
                Ok(()) => return,
                // Its connection has just ended.
                Err(unsent) => unsent.0,
            },
            None => out,
        };
        match out {
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
            drop(links);
            return fail(out, tr!(error_connection_lost));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(out);
        let serial = chats.serial.fetch_add(1, Ordering::Relaxed);
        links.insert(to, Link { serial, tx });
        (serial, rx)
    };
    let shared = shared.clone();
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
        let mut rx = rx;
        match dial(&chats, &endpoint, to).await {
            Ok((conn, send, recv, name)) => {
                chats.quiet.lock().unwrap().remove(&to);
                converse(shared, chats, conn, send, recv, name, serial, rx).await;
            }
            Err(error) => {
                chats
                    .quiet
                    .lock()
                    .unwrap()
                    .insert(to, Instant::now() + QUIET);
                // First, so that nothing more is queued here.
                chats.unlink(&to, serial);
                // The other side may have connected meanwhile: what waited
                // for this dial goes out there.
                while let Ok(out) = rx.try_recv() {
                    let unsent = match chats.links.lock().unwrap().get(&to) {
                        Some(link) => link.tx.send(out).err().map(|unsent| unsent.0),
                        None => Some(out),
                    };
                    if let Some(Out::Text { id, .. }) = unsent {
                        shared.emit(Event::ChatFailed {
                            to,
                            id,
                            error: error.clone(),
                        });
                    }
                }
            }
        }
    });
}

/// Connects to `to` and greets it. The streams, and the name the other side
/// gave of itself; or why not, for the user.
async fn dial(
    chats: &Chats,
    endpoint: &Endpoint,
    to: EndpointId,
) -> Result<(Connection, SendStream, RecvStream, String), String> {
    let lost = || tr!(error_connection_lost).to_string();
    let connecting = endpoint.connect(to, protocol::CHAT_ALPN);
    let conn = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(Ok(conn)) => conn,
        // Before anything else: for a transfer the same answer means
        // another version (see `net::connect_error_text`).
        Ok(Err(e)) if super::no_such_protocol(&e) => {
            return Err(os_tr!(chat_error_cannot_chat).into());
        }
        Ok(Err(e)) => return Err(super::connect_error_text(&e)),
        Err(_) => return Err(tr!(error_timeout).into()),
    };
    let greeted = async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|_| lost())?;
        let name = chats.policy.lock().unwrap().my_name.clone();
        protocol::write_as(&mut send, &ChatMsg::Hello { name })
            .await
            .map_err(|_| lost())?;
        match protocol::read_as::<_, ChatMsg>(&mut recv).await {
            Ok(ChatMsg::Welcome { name }) => Ok((send, recv, shown_name(&name))),
            Ok(ChatMsg::Refused) => Err(tr!(chat_error_refused).to_string()),
            _ => Err(lost()),
        }
    };
    match tokio::time::timeout(ANSWER_TIMEOUT, greeted).await {
        Ok(Ok((send, recv, name))) => Ok((conn, send, recv, name)),
        Ok(Err(error)) => {
            conn.close(CLOSE_OK.into(), b"done");
            Err(error)
        }
        Err(_) => {
            conn.close(CLOSE_OK.into(), b"done");
            Err(tr!(error_timeout).into())
        }
    }
}

/// Handles a chat connection that came in.
pub(crate) async fn accept(shared: Arc<Shared>, conn: Connection) {
    let Some(chats) = shared.chat.clone() else {
        return;
    };
    let peer = conn.remote_id();
    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
        return;
    };
    let first = protocol::read_as::<_, ChatMsg>(&mut recv);
    let Ok(Ok(ChatMsg::Hello { name })) = tokio::time::timeout(ANSWER_TIMEOUT, first).await else {
        return;
    };
    if shared.shutting_down.load(Ordering::SeqCst) || !takes(&shared, &chats, &peer) {
        // Nothing here sees it; the connection stays until the other side
        // has read the answer.
        let _ = protocol::write_as(&mut send, &ChatMsg::Refused).await;
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        return;
    }
    let my_name = chats.policy.lock().unwrap().my_name.clone();
    if protocol::write_as(&mut send, &ChatMsg::Welcome { name: my_name })
        .await
        .is_err()
    {
        return;
    }
    // What is sent from now on goes here. A connection that was there
    // before goes on taking what arrives on it.
    let (serial, rx) = chats.link(peer);
    converse(shared, chats, conn, send, recv, shown_name(&name), serial, rx).await;
}

/// Runs a chat connection with the person at its other end, whose name for
/// themselves is `name`, until it ends.
#[allow(clippy::too_many_arguments)]
async fn converse(
    shared: Arc<Shared>,
    chats: Arc<Chats>,
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    name: String,
    serial: u64,
    mut rx: mpsc::UnboundedReceiver<Out>,
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
    // Why it ended, for the messages that wait; `None` when nothing waits.
    let error: Option<String> = loop {
        tokio::select! {
            out = rx.recv(), if sending => {
                let msg = match out {
                    Some(Out::Text { id, text }) => {
                        waiting.insert(id, Instant::now());
                        ChatMsg::Text { id, text }
                    }
                    Some(Out::Typing(on)) => ChatMsg::Typing { on },
                    // Another connection with this person took over.
                    None => {
                        sending = false;
                        continue;
                    }
                };
                last = Instant::now();
                if protocol::write_as(&mut send, &msg).await.is_err() {
                    break Some(tr!(error_connection_lost).into());
                }
            }
            msg = in_rx.recv() => {
                let Some(msg) = msg else {
                    break Some(tr!(error_connection_lost).into());
                };
                match msg {
                    ChatMsg::Text { .. } | ChatMsg::Typing { .. }
                        if !takes(&shared, &chats, &peer) =>
                    {
                        // The user blocked this person meanwhile, or takes
                        // messages from recipients only now, and has not
                        // written to this one.
                        let _ = protocol::write_as(&mut send, &ChatMsg::Refused).await;
                        let _ = send.finish();
                        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                        break Some(tr!(error_connection_lost).into());
                    }
                    ChatMsg::Text { id, text } => {
                        // Exiting: nobody would see it, so it is not
                        // confirmed, and its sender is told that it failed.
                        if shared.shutting_down.load(Ordering::SeqCst) {
                            break None;
                        }
                        last = Instant::now();
                        if text.len() > protocol::MAX_CHAT_TEXT || text.trim().is_empty() {
                            continue;
                        }
                        peer_typing = false;
                        shared.emit(Event::Chat {
                            from: peer,
                            name: name.clone(),
                            text,
                        });
                        if protocol::write_as(&mut send, &ChatMsg::Received { id }).await.is_err() {
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
                    ChatMsg::Refused => break Some(tr!(chat_error_refused).into()),
                    ChatMsg::Hello { .. } | ChatMsg::Welcome { .. } => {}
                }
            }
            _ = tick.tick() => {
                let late: Vec<u64> = waiting
                    .iter()
                    .filter(|(_, since)| since.elapsed() >= ANSWER_TIMEOUT)
                    .map(|(id, _)| *id)
                    .collect();
                for id in late {
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
    // First, so that nothing more is queued here.
    chats.unlink(&peer, serial);
    let error = error.unwrap_or_else(|| tr!(error_connection_lost).into());
    for id in waiting.into_keys() {
        shared.emit(Event::ChatFailed {
            to: peer,
            id,
            error: error.clone(),
        });
    }
    fail_queued(&shared, peer, &mut rx, &error);
    if peer_typing {
        shared.emit(Event::Typing {
            from: peer,
            on: false,
        });
    }
    conn.close(CLOSE_OK.into(), b"done");
}
