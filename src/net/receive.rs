//! Receiving side of a transfer. The receiver never dials for data: after a
//! lost connection it waits for the sender to come back with `Continue`,
//! and only asks now and then (`Query`) whether the sender still has the
//! transfer at all.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::bail;
use iroh::{
    Endpoint, EndpointId,
    endpoint::{Connection, RecvStream, SendStream},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

use super::{
    CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_OK, CLOSE_SUSPENDED, Event, Outcome, Phase, Shared,
    Throttle,
    protocol::{self, CHUNK, Kind, Msg, Offer, Token},
    send::AbortOnDrop,
    session::{
        self, Control, Lost, PeerClose, Role, Session, Side, Streams, TransferRecord, lost,
        peer_close,
    },
    spawn_path_watch,
};
use crate::{archive, i18n::tr};

/// How often a waiting receiver asks the sender whether the transfer still exists.
const QUERY_INTERVAL: Duration = Duration::from_secs(30);
/// How long one such question may take, dialing included.
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
/// The part file is flushed to disk, and the amount noted in the record,
/// after this much new data.
const SYNC_EVERY: u64 = 64 * 1024 * 1024;
/// Tests set this to stop a folder transfer right before unpacking, to
/// simulate a shutdown during the last step.
#[cfg(test)]
pub(super) static HOLD_FINALIZE: AtomicBool = AtomicBool::new(false);
/// How long to wait for a replacement connection after losing one before
/// telling the user (the router replaces connections in one go).
const REPLACEMENT_GRACE: Duration = Duration::from_secs(1);

/// The answer of a running query, or never, when none is running (the
/// expression of a disabled `select!` branch is still evaluated).
async fn query_answer(query: &mut Option<JoinHandle<Option<bool>>>) -> Option<bool> {
    match query {
        Some(handle) => handle.await.ok().flatten(),
        None => std::future::pending().await,
    }
}

/// A failure that was already reported to the sender over the stream. The
/// connection must stay open until the sender has read the report, so it is
/// not closed with an error code like other failures.
#[derive(Debug)]
struct Reported(String);

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Reported {}

/// Removes a partially unpacked folder unless disarmed.
struct Cleanup(Option<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

enum End {
    Lost,
    Final(Outcome),
}

/// Marks the session as syncing while it exists (see `Session::syncing`).
struct Syncing(Arc<Session>);

impl Syncing {
    fn new(session: Arc<Session>) -> Self {
        session.syncing.store(true, Ordering::Relaxed);
        Syncing(session)
    }
}

impl Drop for Syncing {
    fn drop(&mut self) {
        self.0.syncing.store(false, Ordering::Relaxed);
    }
}

/// Runs `update` on the session's record off the runtime's worker threads:
/// saving the record syncs it to disk, which can take a while on a slow one.
async fn update_record(
    session: &Arc<Session>,
    shared: &Shared,
    update: impl FnOnce(&Session, &Path) + Send + 'static,
) {
    let session = session.clone();
    let data_dir = shared.data_dir.clone();
    let _ = tokio::task::spawn_blocking(move || update(&session, &data_dir)).await;
}

/// The status while connected.
fn pause_phase(local: bool, peer: bool) -> Phase {
    if local {
        Phase::Paused
    } else if peer {
        Phase::PausedByPeer
    } else {
        Phase::Transferring
    }
}

/// A new offer on a fresh connection: asks the user, then receives.
pub(super) async fn handle(
    shared: Arc<Shared>,
    endpoint: Endpoint,
    conn: Connection,
    mut send: SendStream,
    recv: RecvStream,
    offer: Offer,
) {
    let from = conn.remote_id();
    let id = super::next_transfer_id();
    let ctl = shared.register(id, false);
    let (tx, rx) = oneshot::channel();
    shared.pending.lock().unwrap().insert(id, tx);
    shared.emit(Event::Offer {
        id,
        from,
        offer: offer.clone(),
    });

    // Wait for the user's decision (or for the sender to give up).
    let decision = tokio::select! {
        answer = rx => answer.ok().flatten(),
        _ = conn.closed() => {
            shared.finish(id, Outcome::CancelledByPeer);
            return;
        }
        _ = ctl.cancel.cancelled() => None,
    };
    let Some(dest_dir) = decision else {
        let _ = protocol::write_msg(&mut send, &Msg::Decline).await;
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        shared.finish(id, Outcome::Declined);
        return;
    };

    let cancel = ctl.cancel.clone();
    let outcome = tokio::select! {
        o = accept(&shared, &endpoint, id, &ctl, conn.clone(), send, recv, offer, from, dest_dir) => o,
        _ = cancel.cancelled() => cancelled(&shared, id, Some(&conn)),
    };
    shared.finish(id, outcome);
}

/// A transfer from an earlier run: waits for the sender to come back.
pub(super) async fn resume(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    record: TransferRecord,
    ctl: Arc<Control>,
) -> Outcome {
    let cancel = ctl.cancel.clone();
    tokio::select! {
        o = continue_record(shared, endpoint, id, record, &ctl) => o,
        _ = cancel.cancelled() => cancelled(shared, id, None),
    }
}

/// The user cancelled: tell the sender if connected, otherwise leave a
/// tombstone for its next `Continue`, and delete the partial data.
fn cancelled(shared: &Arc<Shared>, id: u64, conn: Option<&Connection>) -> Outcome {
    match shared.session_by_id(id) {
        Some(session) => session.end(&shared.data_dir, Some(CLOSE_CANCELLED), false),
        None => {
            if let Some(conn) = conn {
                conn.close(CLOSE_CANCELLED.into(), b"cancelled");
            }
        }
    }
    Outcome::Cancelled
}

struct Receiver<'a> {
    shared: &'a Arc<Shared>,
    endpoint: &'a Endpoint,
    id: u64,
    peer: EndpointId,
    ctl: &'a Control,
    session: Arc<Session>,
    session_rx: mpsc::UnboundedReceiver<Streams>,
    kind: Kind,
    name: String,
    dest_dir: PathBuf,
    part_path: PathBuf,
    size: u64,
    /// Bytes written to the part file.
    have: u64,
    /// The hash of those bytes; `None` until the part file was hashed again
    /// after a restart.
    hasher: Option<blake3::Hasher>,
    /// The sender's hash of the whole item, once `Done` arrived.
    done_hash: Option<[u8; 32]>,
    peer_paused: Arc<AtomicBool>,
    /// The last connection was closed by the sender on purpose (it is
    /// exiting): the wait for the next one is not a network problem.
    peer_suspended: bool,
}

#[allow(clippy::too_many_arguments)]
async fn accept(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    ctl: &Control,
    conn: Connection,
    mut send: SendStream,
    recv: RecvStream,
    offer: Offer,
    from: EndpointId,
    dest_dir: PathBuf,
) -> Outcome {
    let name = archive::sanitize_file_name(&offer.name);
    let prepared: anyhow::Result<PathBuf> = async {
        tokio::fs::create_dir_all(&dest_dir).await?;
        // Folders arrive as a zip in the temp folder; files go straight to the destination folder.
        let part_path = match offer.kind {
            Kind::File => archive::unique_path(&dest_dir, &format!("{name}.part"), false),
            Kind::Folder => {
                tokio::fs::create_dir_all(&shared.temp_dir).await?;
                shared
                    .temp_dir
                    .join(format!("recv-{}.zip", protocol::token_hex(&offer.token)))
            }
        };
        tokio::fs::File::create(&part_path).await?;
        Ok(part_path)
    }
    .await;
    let part_path = match prepared {
        Ok(p) => p,
        Err(e) => {
            conn.close(CLOSE_FAILED.into(), b"failed");
            return Outcome::Failed(format!("{e:#}"));
        }
    };
    let record = TransferRecord {
        token: offer.token,
        peer: from.to_string(),
        peer_name: offer.sender_name.clone(),
        paused: ctl.is_paused(),
        created: std::time::SystemTime::now(),
        side: Side::Receive {
            dest_dir: dest_dir.clone(),
            part_path: part_path.clone(),
            synced: Some(0),
        },
        offer,
        done_hash: None,
    };
    if let Err(e) = record.save(&shared.data_dir) {
        let _ = std::fs::remove_file(&part_path);
        conn.close(CLOSE_FAILED.into(), b"failed");
        return Outcome::Failed(format!("{e:#}"));
    }
    let (session, session_rx) = Session::new(id, record.token, from, Role::Receive);
    let size = record.offer.size;
    let kind = record.offer.kind;
    *session.record.lock().unwrap() = Some(record);
    shared.add_session(session.clone());
    let mut receiver = Receiver {
        shared,
        endpoint,
        id,
        peer: from,
        ctl,
        session: session.clone(),
        session_rx,
        kind,
        name,
        dest_dir,
        part_path,
        size,
        have: 0,
        hasher: Some(blake3::Hasher::new()),
        done_hash: None,
        peer_paused: Arc::new(AtomicBool::new(false)),
        peer_suspended: false,
    };
    if protocol::write_msg(&mut send, &Msg::Accept).await.is_err() {
        // The sender never learnt that we accepted, so it will not come back.
        session.end(&shared.data_dir, None, false);
        return Outcome::Failed(tr!(error_connection_lost).into());
    }
    receiver.drive(Some(Streams { conn, send, recv })).await
}

async fn continue_record(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    record: TransferRecord,
    ctl: &Control,
) -> Outcome {
    let Some(peer) = record.peer_id() else {
        record.discard(&shared.data_dir);
        return Outcome::Failed(tr!(error_bad_resume).into());
    };
    let Side::Receive {
        dest_dir,
        part_path,
        ..
    } = &record.side
    else {
        return Outcome::Failed(tr!(error_bad_resume).into());
    };
    // Data beyond the last flush may never have reached the disk.
    if let Err(e) = record.truncate_to_synced() {
        record.discard(&shared.data_dir);
        return Outcome::Failed(format!("{e:#}"));
    }
    let (session, session_rx) = Session::new(id, record.token, peer, Role::Receive);
    let mut receiver = Receiver {
        shared,
        endpoint,
        id,
        peer,
        ctl,
        session: session.clone(),
        session_rx,
        kind: record.offer.kind,
        name: archive::sanitize_file_name(&record.offer.name),
        dest_dir: dest_dir.clone(),
        part_path: part_path.clone(),
        size: record.offer.size,
        have: record.have().min(record.offer.size),
        hasher: None,
        done_hash: record.done_hash,
        peer_paused: Arc::new(AtomicBool::new(false)),
        peer_suspended: false,
    };
    let token = record.token;
    *session.record.lock().unwrap() = Some(record);
    shared.add_session(session);
    shared.restored(&token);
    // Everything had arrived and the sender was told so: only the last step
    // (renaming, or unpacking the folder) was interrupted.
    if let Some(hash) = receiver.done_hash
        && receiver.have == receiver.size
    {
        return receiver.complete_alone(hash).await;
    }
    receiver.emit_waiting();
    receiver.drive(None).await
}

/// Asks the sender whether it still has the transfer. `Some(completed)` if it
/// says it is gone, `None` if it is still there or could not be reached.
async fn query_gone(endpoint: Endpoint, peer: EndpointId, token: Token) -> Option<bool> {
    let ask = async {
        let conn = endpoint.connect(peer, protocol::ALPN).await.ok()?;
        let answer: anyhow::Result<Msg> = async {
            let (mut send, mut recv) = conn.open_bi().await?;
            protocol::write_msg(&mut send, &Msg::Query { token }).await?;
            send.finish()?;
            protocol::read_msg(&mut recv).await
        }
        .await;
        conn.close(CLOSE_OK.into(), b"query");
        match answer {
            Ok(Msg::Gone { completed }) => Some(completed),
            _ => None,
        }
    };
    tokio::time::timeout(QUERY_TIMEOUT, ask)
        .await
        .unwrap_or(None)
}

/// Owns the sending half of the stream while receiving: tells the sender
/// about pauses and resumes by the user, and finally sends the `Result`.
async fn control_writer(
    mut send: SendStream,
    mut paused_rx: watch::Receiver<bool>,
    mut result_rx: oneshot::Receiver<Msg>,
    shared: Arc<Shared>,
    id: u64,
    session: Arc<Session>,
    peer_paused: Arc<AtomicBool>,
) {
    if *paused_rx.borrow_and_update() && protocol::write_msg(&mut send, &Msg::Paused).await.is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            changed = paused_rx.changed() => {
                if changed.is_err() {
                    return;
                }
                let paused = *paused_rx.borrow_and_update();
                update_record(&session, &shared, move |s, d| s.save_paused(d, paused)).await;
                shared.emit(Event::Phase {
                    id,
                    phase: pause_phase(paused, peer_paused.load(Ordering::Relaxed)),
                });
                let msg = if paused { Msg::Paused } else { Msg::Resumed };
                if protocol::write_msg(&mut send, &msg).await.is_err() {
                    return;
                }
            }
            result = &mut result_rx => {
                if let Ok(msg) = result {
                    let _ = protocol::write_msg(&mut send, &msg).await;
                    let _ = send.finish();
                }
                return;
            }
        }
    }
}

impl Receiver<'_> {
    fn emit(&self, phase: Phase) {
        self.shared.emit(Event::Phase { id: self.id, phase });
    }

    fn emit_pause_phase(&self) {
        self.emit(pause_phase(
            self.ctl.is_paused(),
            self.peer_paused.load(Ordering::Relaxed),
        ));
    }

    fn emit_waiting(&self) {
        self.emit(if self.ctl.is_paused() {
            Phase::Paused
        } else {
            Phase::WaitingForConnection {
                lost: !self.peer_suspended,
            }
        });
    }

    async fn drive(&mut self, mut streams: Option<Streams>) -> Outcome {
        loop {
            let s = match streams.take() {
                Some(s) => s,
                None => match self.wait_for_connection().await {
                    Ok(s) => s,
                    Err(outcome) => return outcome,
                },
            };
            match self.stream(s).await {
                Ok(path) => return Outcome::Completed(Some(path)),
                Err(End::Final(outcome)) => return outcome,
                Err(End::Lost) => {
                    // The router may have replaced the connection already.
                    let replacement =
                        tokio::time::timeout(REPLACEMENT_GRACE, self.session_rx.recv()).await;
                    if let Ok(Some(s)) = replacement {
                        streams = self.take_new(s).await;
                    }
                    if streams.is_none() {
                        self.emit_waiting();
                    }
                }
            }
        }
    }

    /// Answers `Continue` on a new connection with what we already have.
    async fn take_new(&mut self, mut s: Streams) -> Option<Streams> {
        let answer = Msg::ContinueFrom { have: self.have };
        match protocol::write_msg(&mut s.send, &answer).await {
            Ok(()) => Some(s),
            Err(_) => {
                s.conn.close(CLOSE_SUSPENDED.into(), b"retry");
                None
            }
        }
    }

    /// Waits for the sender to dial again. Meanwhile asks now and then whether
    /// it still has the transfer, and follows the user's pauses.
    async fn wait_for_connection(&mut self) -> Result<Streams, Outcome> {
        let mut paused_rx = self.ctl.paused.subscribe();
        // The question runs as its own task, so that a `Continue` arriving
        // meanwhile is answered at once.
        let mut query: Option<JoinHandle<Option<bool>>> = None;
        loop {
            let asking = query.is_some();
            tokio::select! {
                s = self.session_rx.recv() => {
                    let Some(s) = s else {
                        return Err(Outcome::Failed(tr!(error_bad_resume).into()));
                    };
                    if let Some(s) = self.take_new(s).await {
                        if let Some(q) = query.take() {
                            q.abort();
                        }
                        return Ok(s);
                    }
                }
                _ = tokio::time::sleep(QUERY_INTERVAL), if !asking => {
                    if !self.ctl.is_paused() {
                        let (endpoint, peer, token) =
                            (self.endpoint.clone(), self.peer, self.session.token);
                        query = Some(tokio::spawn(query_gone(endpoint, peer, token)));
                    }
                }
                answer = query_answer(&mut query), if asking => {
                    query = None;
                    if let Some(completed) = answer {
                        if completed
                            && let Some(hash) = self.done_hash
                            && self.have == self.size
                        {
                            return Err(self.complete_alone(hash).await);
                        }
                        self.session.end(&self.shared.data_dir, None, false);
                        return Err(Outcome::CancelledByPeer);
                    }
                }
                _ = paused_rx.changed() => {
                    let paused = *paused_rx.borrow_and_update();
                    self.session.save_paused(&self.shared.data_dir, paused);
                    self.emit_waiting();
                }
            }
        }
    }

    /// The sender's hash is known: check what is on disk and finish without
    /// the sender (after a restart during the last step, or when the sender
    /// already counts the transfer as complete).
    async fn complete_alone(&mut self, expected: [u8; 32]) -> Outcome {
        // A sender that comes back meanwhile is told to try again later; once
        // this is over, the tombstone answers it. Without this, its
        // connections would pile up unanswered until it gives up on each.
        let (_unused, placeholder) = mpsc::unbounded_channel();
        let mut new_conns = std::mem::replace(&mut self.session_rx, placeholder);
        let _drain = AbortOnDrop(tokio::spawn(async move {
            while let Some(s) = new_conns.recv().await {
                s.conn.close(CLOSE_SUSPENDED.into(), b"finishing");
            }
        }));
        let data_dir = &self.shared.data_dir;
        self.hasher = None;
        if let Err(e) = self.rehash().await {
            self.session.end(data_dir, None, false);
            return Outcome::Failed(format!("{e:#}"));
        }
        if *self.hasher.as_ref().unwrap().finalize().as_bytes() != expected {
            self.session.end(data_dir, None, false);
            return Outcome::Failed(tr!(error_damaged).into());
        }
        match self.finalize().await {
            Ok(path) => {
                self.session.end(data_dir, None, true);
                Outcome::Completed(Some(path))
            }
            Err(e) => {
                self.session.end(data_dir, None, false);
                Outcome::Failed(format!("{e:#}"))
            }
        }
    }

    /// Receives on one connection.
    async fn stream(&mut self, s: Streams) -> Result<PathBuf, End> {
        let Streams { conn, send, recv } = s;
        self.session.set_current(Some(conn.clone()));
        let _watch = AbortOnDrop(spawn_path_watch(self.shared.clone(), self.id, conn.clone()));
        self.peer_paused.store(false, Ordering::Relaxed);
        self.peer_suspended = false;
        let (result_tx, result_rx) = oneshot::channel();
        let writer = tokio::spawn(control_writer(
            send,
            self.ctl.paused.subscribe(),
            result_rx,
            self.shared.clone(),
            self.id,
            self.session.clone(),
            self.peer_paused.clone(),
        ));

        let result = self.pump(recv, result_tx).await;
        let data_dir = &self.shared.data_dir;
        match result {
            Ok(path) => {
                // Let the Result go out, and give the sender a moment to read it and close.
                let _ = tokio::time::timeout(Duration::from_secs(10), writer).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                self.session.end(data_dir, None, true);
                Ok(path)
            }
            Err(e) if e.is::<Lost>() => {
                writer.abort();
                match peer_close(&conn) {
                    PeerClose::Cancelled => {
                        self.session.end(data_dir, None, false);
                        Err(End::Final(Outcome::CancelledByPeer))
                    }
                    PeerClose::Failed => {
                        self.session.end(data_dir, None, false);
                        Err(End::Final(Outcome::Failed(tr!(error_sender_failed).into())))
                    }
                    PeerClose::Suspended => {
                        self.peer_suspended = true;
                        self.session.set_current(None);
                        Err(End::Lost)
                    }
                    PeerClose::Lost => {
                        self.session.set_current(None);
                        Err(End::Lost)
                    }
                }
            }
            Err(e) if e.is::<Reported>() => {
                // The sender is told over the stream; closing now would discard that.
                let _ = tokio::time::timeout(Duration::from_secs(10), writer).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                self.session.end(data_dir, None, false);
                Err(End::Final(Outcome::Failed(format!("{e:#}"))))
            }
            Err(e) => {
                writer.abort();
                self.session.end(data_dir, Some(CLOSE_FAILED), false);
                Err(End::Final(Outcome::Failed(format!("{e:#}"))))
            }
        }
    }

    /// After a restart: the hash of the part file as it is on disk.
    async fn rehash(&mut self) -> anyhow::Result<()> {
        if self.hasher.is_some() {
            return Ok(());
        }
        self.emit(Phase::Verifying);
        let path = self.part_path.clone();
        let have = self.have;
        let cancel = self.ctl.cancel.clone();
        let shared = self.shared.clone();
        let id = self.id;
        let hasher = tokio::task::spawn_blocking(move || {
            let mut hasher = blake3::Hasher::new();
            let mut throttle = Throttle::new();
            session::hash_range(
                &path,
                0,
                have,
                &mut hasher,
                cancel.flag(),
                &mut |done, total| {
                    if throttle.ready() || done == total {
                        shared.emit(Event::Progress { id, done, total });
                    }
                },
            )?;
            Ok::<_, std::io::Error>(hasher)
        })
        .await??;
        self.hasher = Some(hasher);
        Ok(())
    }

    async fn pump(
        &mut self,
        mut recv: RecvStream,
        result_tx: oneshot::Sender<Msg>,
    ) -> anyhow::Result<PathBuf> {
        self.rehash().await?;
        self.emit_pause_phase();
        self.shared.emit(Event::Progress {
            id: self.id,
            done: self.have,
            total: self.size,
        });
        // Shutdown waits until what arrived is on disk and in the record.
        let syncing = Syncing::new(self.session.clone());
        let mut out = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.part_path)
            .await?;
        let received = self.read_frames(&mut recv, &mut out).await;
        // Whatever happened, what was written has to be on disk before `have`
        // is trusted, also after a power cut.
        out.flush().await?;
        out.sync_data().await?;
        drop(out);
        self.save_synced().await;
        drop(syncing);
        let hash = received?;

        if hash != *self.hasher.as_ref().unwrap().finalize().as_bytes() {
            let _ = result_tx.send(Msg::Result {
                ok: false,
                message: tr!(error_checksum).into(),
            });
            return Err(Reported(tr!(error_damaged).into()).into());
        }
        // From here on this side can finish on its own, even after a restart.
        self.done_hash = Some(hash);
        update_record(&self.session, self.shared, move |s, d| {
            s.save_done_hash(d, hash)
        })
        .await;
        let _ = result_tx.send(Msg::Result {
            ok: true,
            message: String::new(),
        });
        self.finalize().await
    }

    /// Notes in the record how much of the part file is on disk.
    async fn save_synced(&self) {
        let have = self.have;
        update_record(&self.session, self.shared, move |s, d| {
            s.save_synced(d, have)
        })
        .await;
    }

    /// Reads frames until `Done`; returns the sender's hash.
    async fn read_frames(
        &mut self,
        recv: &mut RecvStream,
        out: &mut tokio::fs::File,
    ) -> anyhow::Result<[u8; 32]> {
        let mut buf = vec![0u8; CHUNK];
        let mut throttle = Throttle::new();
        let mut unsynced = 0u64;
        loop {
            match protocol::read_msg(recv).await.map_err(lost)? {
                Msg::Data { len } => {
                    let len = len as usize;
                    anyhow::ensure!(
                        len <= CHUNK && self.have + len as u64 <= self.size,
                        tr!(error_bad_resume)
                    );
                    recv.read_exact(&mut buf[..len]).await.map_err(lost)?;
                    out.write_all(&buf[..len]).await?;
                    self.hasher.as_mut().unwrap().update(&buf[..len]);
                    self.have += len as u64;
                    unsynced += len as u64;
                    if unsynced >= SYNC_EVERY {
                        out.sync_data().await?;
                        self.save_synced().await;
                        unsynced = 0;
                    }
                    if throttle.ready() || self.have == self.size {
                        self.shared.emit(Event::Progress {
                            id: self.id,
                            done: self.have,
                            total: self.size,
                        });
                    }
                }
                Msg::Paused => {
                    self.peer_paused.store(true, Ordering::Relaxed);
                    self.emit_pause_phase();
                }
                Msg::Resumed => {
                    self.peer_paused.store(false, Ordering::Relaxed);
                    self.emit_pause_phase();
                }
                Msg::Done { hash } => {
                    anyhow::ensure!(self.have == self.size, tr!(error_protocol_end));
                    return Ok(hash);
                }
                other => bail!(tr!(error_unexpected_answer, answer = format!("{other:?}"))),
            }
        }
    }

    /// Moves the file into place, or unpacks the folder.
    async fn finalize(&mut self) -> anyhow::Result<PathBuf> {
        match self.kind {
            Kind::File => {
                let final_path = archive::unique_path(&self.dest_dir, &self.name, false);
                tokio::fs::rename(&self.part_path, &final_path).await?;
                Ok(final_path)
            }
            Kind::Folder => {
                self.emit(Phase::Unpacking);
                #[cfg(test)]
                while HOLD_FINALIZE.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let final_path = archive::unique_path(&self.dest_dir, &self.name, true);
                let shared = self.shared.clone();
                let id = self.id;
                let cancel = self.ctl.cancel.clone();
                let target = final_path.clone();
                let zip = self.part_path.clone();
                tokio::task::spawn_blocking(move || {
                    let mut partial = Cleanup(Some(target.clone()));
                    let mut throttle = Throttle::new();
                    archive::unzip(&zip, &target, cancel.flag(), &mut |done, total| {
                        if throttle.ready() || done == total {
                            shared.emit(Event::Progress { id, done, total });
                        }
                    })?;
                    partial.0 = None;
                    anyhow::Ok(())
                })
                .await??;
                Ok(final_path)
            }
        }
    }
}
