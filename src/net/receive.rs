//! Receiving side of a transfer. The receiver never dials for data: after a
//! lost connection it waits for the sender to come back with `Continue`,
//! and only asks now and then (`Query`) whether the sender still has the
//! transfer at all.

use std::{
    path::PathBuf,
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
};

use super::{
    CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_OK, CLOSE_SUSPENDED, Event, Outcome, Phase, Shared,
    Throttle,
    protocol::{self, CHUNK, Kind, Msg, Offer},
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
/// How long to wait for a replacement connection after losing one before
/// telling the user (the router replaces connections in one go).
const REPLACEMENT_GRACE: Duration = Duration::from_secs(1);

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
    peer_paused: Arc<AtomicBool>,
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
            Kind::Folder => archive::temp_dir()?
                .join(format!("recv-{}.zip", protocol::token_hex(&offer.token))),
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
        },
        offer,
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
        peer_paused: Arc::new(AtomicBool::new(false)),
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
    } = &record.side
    else {
        return Outcome::Failed(tr!(error_bad_resume).into());
    };
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
        peer_paused: Arc::new(AtomicBool::new(false)),
    };
    *session.record.lock().unwrap() = Some(record);
    shared.add_session(session);
    receiver.emit_waiting();
    receiver.drive(None).await
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
    if *paused_rx.borrow_and_update() && protocol::write_msg(&mut send, &Msg::Paused).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            changed = paused_rx.changed() => {
                if changed.is_err() {
                    return;
                }
                let paused = *paused_rx.borrow_and_update();
                session.save_paused(&shared.data_dir, paused);
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
            Phase::WaitingForConnection
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
        loop {
            tokio::select! {
                s = self.session_rx.recv() => {
                    let Some(s) = s else {
                        return Err(Outcome::Failed(tr!(error_bad_resume).into()));
                    };
                    if let Some(s) = self.take_new(s).await {
                        return Ok(s);
                    }
                }
                _ = tokio::time::sleep(QUERY_INTERVAL) => {
                    if !self.ctl.is_paused() && self.query_gone().await {
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

    /// Asks the sender whether it still has the transfer. `true` if it says no.
    async fn query_gone(&self) -> bool {
        let dial = self.endpoint.connect(self.peer, protocol::ALPN);
        let Ok(Ok(conn)) = tokio::time::timeout(Duration::from_secs(20), dial).await else {
            return false;
        };
        let token = self.session.token;
        let answer: anyhow::Result<Msg> = async {
            let (mut send, mut recv) = conn.open_bi().await?;
            protocol::write_msg(&mut send, &Msg::Query { token }).await?;
            send.finish()?;
            tokio::time::timeout(Duration::from_secs(15), protocol::read_msg(&mut recv)).await?
        }
        .await;
        conn.close(CLOSE_OK.into(), b"query");
        matches!(answer, Ok(Msg::Gone { .. }))
    }

    /// Receives on one connection.
    async fn stream(&mut self, s: Streams) -> Result<PathBuf, End> {
        let Streams { conn, send, recv } = s;
        self.session.set_current(Some(conn.clone()));
        let _watch = AbortOnDrop(spawn_path_watch(self.shared.clone(), self.id, conn.clone()));
        self.peer_paused.store(false, Ordering::Relaxed);
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
            session::hash_range(&path, 0, have, &mut hasher, cancel.flag(), &mut |done, total| {
                if throttle.ready() || done == total {
                    shared.emit(Event::Progress { id, done, total });
                }
            })?;
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
        let mut out = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.part_path)
            .await?;
        let received = self.read_frames(&mut recv, &mut out).await;
        // Whatever happened, what was written has to be on disk before `have` is trusted.
        out.flush().await?;
        drop(out);
        let hash = received?;

        if hash != *self.hasher.as_ref().unwrap().finalize().as_bytes() {
            let _ = result_tx.send(Msg::Result {
                ok: false,
                message: tr!(error_checksum).into(),
            });
            return Err(Reported(tr!(error_damaged).into()).into());
        }
        let _ = result_tx.send(Msg::Result {
            ok: true,
            message: String::new(),
        });
        self.finalize().await
    }

    /// Reads frames until `Done`; returns the sender's hash.
    async fn read_frames(
        &mut self,
        recv: &mut RecvStream,
        out: &mut tokio::fs::File,
    ) -> anyhow::Result<[u8; 32]> {
        let mut buf = vec![0u8; CHUNK];
        let mut throttle = Throttle::new();
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
