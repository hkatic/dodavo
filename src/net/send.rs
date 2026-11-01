//! Sending side of a transfer. The sender dials: for the offer, and again
//! whenever the connection was lost, until the transfer is complete or
//! cancelled.

use std::{
    io::SeekFrom,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anyhow::{Context, bail};
use iroh::{Endpoint, EndpointId, endpoint::Connection};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::{mpsc, watch},
};

use super::{
    CLOSE_FAILED, CLOSE_OK, CLOSE_SUSPENDED, Event, Outcome, Phase, Shared, Throttle,
    protocol::{self, CHUNK, Kind, Msg, Offer, Token},
    session::{
        self, Control, Lost, PeerClose, Role, Session, Side, Snapshots, Streams, TransferRecord,
        lost, off_runtime, peer_close,
    },
    spawn_path_watch,
};
use crate::{archive, i18n::tr};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(40);
/// How long the receiver may take to answer `Continue`.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(15);

/// Deletes the temporary zip of a folder transfer when dropped (until the
/// transfer has a record, which then owns the zip).
struct TempFile(Option<PathBuf>);

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub(crate) struct AbortOnDrop(pub tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How a connection ended for the transfer.
enum End {
    /// The connection was lost; dial again.
    Lost,
    /// The transfer is over.
    Final(Outcome),
}

/// A new transfer: prepares the data, makes the offer and streams it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    to: EndpointId,
    path: PathBuf,
    kind: Kind,
    my_name: String,
    peer_name: String,
    ctl: Arc<Control>,
) -> Outcome {
    tokio::select! {
        outcome = start(shared, endpoint, id, to, path, kind, my_name, peer_name, &ctl) => outcome,
        ending = super::cancelled(shared, id, &ctl) => {
            ending.forget(&shared.data_dir).await;
            Outcome::Cancelled
        }
    }
}

/// A transfer from an earlier run: dials until the receiver is back.
pub(super) async fn resume(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    record: TransferRecord,
    ctl: Arc<Control>,
) -> Outcome {
    // The record first: it registers the session (and sets the stage a
    // cancel finds) before it waits for anything, so that a cancel always
    // finds it, also one from the last run (see `TransferRecord::cancelled`).
    //
    // A cancel ends the transfer when it comes (the receiver is told if
    // connected, otherwise a tombstone waits for its next `Query`), unless
    // it waits for the receiver's verdict on all of the data, which the
    // transfer's task then acts on (`Session::defer_cancel`), or comes once
    // the receiver has confirmed all of the item: then it is too late, and
    // the transfer ends by itself (`Session::hand_over`).
    tokio::select! {
        biased;
        outcome = continue_record(shared, endpoint, id, record, &ctl) => outcome,
        ending = super::cancelled(shared, id, &ctl) => {
            ending.forget(&shared.data_dir).await;
            Outcome::Cancelled
        }
    }
}

async fn dial(endpoint: &Endpoint, to: EndpointId) -> Result<Connection, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(to, protocol::ALPN)).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(tr!(error_timeout).into()),
    }
}

struct Sender<'a> {
    shared: &'a Arc<Shared>,
    endpoint: &'a Endpoint,
    id: u64,
    to: EndpointId,
    ctl: &'a Control,
    session: Arc<Session>,
    token: Token,
    data_path: PathBuf,
    size: u64,
    snapshots: Snapshots,
    paused_rx: watch::Receiver<bool>,
    peer_paused: bool,
    /// The last connection was closed by the receiver on purpose (it is
    /// exiting): the wait for the next one is not a network problem.
    peer_suspended: bool,
}

#[allow(clippy::too_many_arguments)]
async fn start(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    to: EndpointId,
    path: PathBuf,
    kind: Kind,
    my_name: String,
    peer_name: String,
    ctl: &Control,
) -> Outcome {
    let token = protocol::new_token();
    let (session, _unused) = Session::new(id, token, to, Role::Send);
    shared.add_session(session.clone());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| tr!(untitled).into());
    // The item's own dates, for the receiver to give to what it saves.
    let times = std::fs::metadata(&path)
        .map(|m| archive::ItemTimes::of(&m))
        .unwrap_or_default();

    // 1. Prepare the data (zip folders). `size` is what gets streamed.
    let mut temp = TempFile(None);
    let prepared = prepare(shared, id, &path, kind, &token, ctl, &mut temp).await;
    let (data_path, file_count, content_size, size) = match prepared {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(format!("{e:#}")),
    };
    // What a later run compares to notice a changed source: the file, or the
    // zip the folder was packed into.
    let (source_len, source_mtime) = match std::fs::metadata(&data_path) {
        Ok(m) => (m.len(), m.modified().ok()),
        Err(_) => (0, None),
    };

    // 2. Connect.
    shared.emit(Event::Phase {
        id,
        phase: Phase::Connecting,
    });
    let conn = match dial(endpoint, to).await {
        Ok(conn) => conn,
        Err(e) => return Outcome::Unreachable(e),
    };
    session.set_current(Some(conn.clone()));
    let _watch = AbortOnDrop(spawn_path_watch(shared.clone(), id, conn.clone()));

    // 3. Offer and wait for the answer.
    let offer = Offer {
        kind,
        name,
        size,
        file_count,
        content_size,
        sender_name: my_name,
        token,
        modified: times.modified,
        created: times.created,
    };
    let answer: anyhow::Result<Option<Streams>> = async {
        let (mut send, mut recv) = conn.open_bi().await?;
        protocol::write_msg(&mut send, &Msg::Offer(offer.clone())).await?;
        shared.emit(Event::Phase {
            id,
            phase: Phase::WaitingForAccept,
        });
        match protocol::read_msg(&mut recv).await? {
            Msg::Accept => Ok(Some(Streams {
                conn: conn.clone(),
                send,
                recv,
            })),
            Msg::Decline => Ok(None),
            other => bail!(tr!(error_unexpected_answer, answer = format!("{other:?}"))),
        }
    }
    .await;
    let streams = match answer {
        Ok(Some(streams)) => streams,
        Ok(None) => {
            session.close_current(CLOSE_OK, b"declined");
            return Outcome::Declined;
        }
        // Not accepted yet, so there is nothing to continue later.
        Err(e) => {
            return match peer_close(&conn) {
                PeerClose::Cancelled => Outcome::CancelledByPeer,
                PeerClose::Failed => Outcome::Failed(tr!(error_recipient_failed).into()),
                PeerClose::Suspended | PeerClose::Lost => {
                    session.close_current(CLOSE_FAILED, b"failed");
                    Outcome::Failed(format!("{e:#}"))
                }
            };
        }
    };

    // 4. Accepted: from now on the transfer survives lost connections and restarts.
    let record = TransferRecord {
        token,
        peer: to.to_string(),
        offer,
        peer_name,
        paused: ctl.is_paused(),
        created: std::time::SystemTime::now(),
        side: Side::Send {
            source: path,
            source_len,
            source_mtime,
            data_path: data_path.clone(),
        },
        done_hash: None,
        done_sent: false,
        cancelled: false,
        ended: false,
    };
    // The session's from now on, and a cancel waits until it is on disk: one
    // that came meanwhile then ends the transfer with it, so that no record
    // is left behind.
    session.set_record(record);
    session.defer_cancel();
    let saved = session
        .update(&shared.data_dir, |s, d| s.save_record(d))
        .await;
    if let Some(Err(e)) = saved {
        return match session.stop_deferring() {
            Some(ending) => {
                ending.forget(&shared.data_dir).await;
                Outcome::Cancelled
            }
            None => {
                session
                    .end(Some(CLOSE_FAILED), false)
                    .forget(&shared.data_dir)
                    .await;
                Outcome::Failed(format!("{e:#}"))
            }
        };
    }
    temp.0 = None;
    if let Some(ending) = session.stop_deferring() {
        ending.forget(&shared.data_dir).await;
        return Outcome::Cancelled;
    }
    let mut sender = Sender {
        shared,
        endpoint,
        id,
        to,
        ctl,
        session,
        token,
        data_path,
        size,
        snapshots: Snapshots::new(),
        paused_rx: ctl.paused.subscribe(),
        peer_paused: false,
        peer_suspended: false,
    };
    sender.drive(Some(streams), 0).await
}

async fn continue_record(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    record: TransferRecord,
    ctl: &Control,
) -> Outcome {
    let Some(to) = record.peer_id() else {
        let data_dir = shared.data_dir.clone();
        off_runtime(move || record.discard(&data_dir)).await;
        return Outcome::Failed(tr!(error_bad_resume).into());
    };
    let Side::Send { data_path, .. } = &record.side else {
        return Outcome::Failed(tr!(error_bad_resume).into());
    };
    let (session, _unused) = Session::new(id, record.token, to, Role::Send);
    shared.add_session(session.clone());
    shared.restored(&record.token);
    let mut sender = Sender {
        shared,
        endpoint,
        id,
        to,
        ctl,
        session: session.clone(),
        token: record.token,
        data_path: data_path.clone(),
        size: record.offer.size,
        snapshots: Snapshots::new(),
        paused_rx: ctl.paused.subscribe(),
        peer_paused: false,
        peer_suspended: false,
    };
    let handed_over = record.done_hash.is_some();
    if handed_over {
        // The receiver had confirmed all of it before the restart.
        session.hand_over();
    } else if record.done_sent {
        // It may have all of it: a cancel waits for its verdict.
        session.defer_cancel();
    }
    session.set_record(record);
    if handed_over {
        shared.emit(Event::HandedOver { id });
    }
    sender.emit_waiting();
    sender.drive(None, 0).await
}

/// Zips a folder (reporting progress) or measures a file. Returns what is
/// streamed: its path, the number of files, their size, and the stream size.
async fn prepare(
    shared: &Arc<Shared>,
    id: u64,
    path: &PathBuf,
    kind: Kind,
    token: &Token,
    ctl: &Control,
    temp: &mut TempFile,
) -> anyhow::Result<(PathBuf, u64, u64, u64)> {
    match kind {
        Kind::File => {
            let size = tokio::fs::metadata(path)
                .await
                .with_context(|| tr!(error_cannot_read, path = path.display()))?
                .len();
            Ok((path.clone(), 1, size, size))
        }
        Kind::Folder => {
            shared.emit(Event::Phase {
                id,
                phase: Phase::Preparing,
            });
            tokio::fs::create_dir_all(&shared.temp_dir).await?;
            let zip_path = session::temp_archive(&shared.temp_dir, Role::Send, token);
            temp.0 = Some(zip_path.clone());
            let shared2 = shared.clone();
            let cancel = ctl.cancel.clone();
            let src = path.clone();
            let zip_dst = zip_path.clone();
            let (file_count, content_size) = tokio::task::spawn_blocking(move || {
                let mut throttle = Throttle::new();
                archive::zip_folder(&src, &zip_dst, cancel.flag(), &mut |done, total| {
                    if throttle.ready() || done == total {
                        shared2.emit(Event::Progress { id, done, total });
                    }
                })
            })
            .await??;
            let size = tokio::fs::metadata(&zip_path).await?.len();
            Ok((zip_path, file_count, content_size, size))
        }
    }
}

impl Sender<'_> {
    fn emit(&self, phase: Phase) {
        self.shared.emit(Event::Phase { id: self.id, phase });
    }

    /// The status while connected: paused here, paused there, or running.
    fn emit_pause_phase(&self) {
        self.emit(if self.ctl.is_paused() {
            Phase::Paused
        } else if self.peer_paused {
            Phase::PausedByPeer
        } else {
            Phase::Transferring
        });
    }

    /// The status while disconnected.
    fn emit_waiting(&self) {
        self.emit(if self.ctl.is_paused() {
            Phase::Paused
        } else {
            Phase::WaitingForConnection {
                lost: !self.peer_suspended,
            }
        });
    }

    /// Ends the transfer on this side (see `Session::end`) and deletes what
    /// goes, off the runtime's worker threads.
    async fn end(&self, code: Option<u32>, completed: bool) {
        let ending = self.session.end(code, completed);
        ending.forget(&self.shared.data_dir).await;
    }

    /// The user paused or resumed: remember it and show it.
    async fn pause_changed(&mut self, connected: bool) -> bool {
        let paused = *self.paused_rx.borrow_and_update();
        self.session
            .update(&self.shared.data_dir, move |s, d| s.save_paused(d, paused))
            .await;
        if connected {
            self.emit_pause_phase();
        } else {
            self.emit_waiting();
        }
        paused
    }

    /// Streams on one connection after another until the transfer is over.
    async fn drive(&mut self, mut streams: Option<Streams>, mut have: u64) -> Outcome {
        loop {
            let s = match streams.take() {
                Some(s) => s,
                None => match self.reconnect().await {
                    Ok((s, h)) => {
                        have = h;
                        s
                    }
                    Err(outcome) => return outcome,
                },
            };
            match self.stream(s, have).await {
                Ok(outcome) => return outcome,
                Err(End::Final(outcome)) => return outcome,
                Err(End::Lost) => {
                    self.emit_waiting();
                    streams = None;
                }
            }
        }
    }

    /// Dials the receiver until it answers `Continue`.
    async fn reconnect(&mut self) -> Result<(Streams, u64), Outcome> {
        let mut delay = RETRY_MIN;
        loop {
            if self.shared.shutting_down.load(Ordering::SeqCst) {
                // Exiting: the transfer continues at the next start, and the
                // receiver was told so by the suspended connection.
                std::future::pending::<()>().await;
            }
            if *self.paused_rx.borrow() {
                // Paused: nothing to do until the user resumes (a cancel
                // resumes the transfer too, see `Command::Cancel`).
                if self.paused_rx.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
                self.pause_changed(false).await;
                continue;
            }
            // Once the receiver may have all of it, the source no longer
            // matters (and failing would not end the transfer there); it is
            // checked if the receiver turns out not to have it.
            if self.session.running() && !self.source_unchanged().await {
                self.end(None, false).await;
                return Err(Outcome::Failed(tr!(error_source_changed).into()));
            }
            let dialed = tokio::select! {
                r = dial(self.endpoint, self.to) => r,
                _ = self.paused_rx.changed() => {
                    self.pause_changed(false).await;
                    continue;
                }
            };
            if let Ok(conn) = dialed {
                if self.shared.shutting_down.load(Ordering::SeqCst) {
                    // Exiting since the dial began: this connection must not
                    // carry data that the endpoint then cuts off without a word.
                    conn.close(CLOSE_SUSPENDED.into(), b"exiting");
                    continue;
                }
                // Whether the receiver may have all of the item (the cancel
                // waits for what it answers now), or has confirmed that (a
                // cancel came too late).
                let deferred = self.session.deferring();
                let handed_over = self.session.handed_over();
                match self.handshake(conn).await {
                    Ok(streams) => {
                        // The receiver does not have the item: a cancel that
                        // waited for that ends the transfer now.
                        if let Some(ending) = self.session.stop_deferring() {
                            ending.forget(&self.shared.data_dir).await;
                            return Err(Outcome::Cancelled);
                        }
                        if deferred {
                            // `Done` goes out again after the rest of the
                            // data; until then a cancel ends the transfer,
                            // also after a restart. The data is needed again,
                            // as it was.
                            self.session
                                .update(&self.shared.data_dir, |s, d| s.save_done_sent(d, false))
                                .await;
                            if !self.source_unchanged().await {
                                self.end(Some(CLOSE_FAILED), false).await;
                                return Err(Outcome::Failed(tr!(error_source_changed).into()));
                            }
                        }
                        return Ok(streams);
                    }
                    Err(Some(completed)) => {
                        self.end(None, completed).await;
                        return Err(if completed {
                            Outcome::Completed(None)
                        } else if self.ctl.cancelled() && !handed_over {
                            // Ended there too, without the item, as the user
                            // wanted here.
                            Outcome::Cancelled
                        } else {
                            // Ended there, by the receiver: also when the
                            // user's cancel here came too late, since the
                            // item was the receiver's by then.
                            Outcome::CancelledByPeer
                        });
                    }
                    Err(None) => {}
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.paused_rx.changed() => {
                    self.pause_changed(false).await;
                }
            }
            delay = (delay * 2).min(RETRY_MAX);
        }
    }

    /// Whether the data to send still looks like it did at the start (see
    /// `TransferRecord::source_unchanged`).
    async fn source_unchanged(&self) -> bool {
        match self.session.with_record(TransferRecord::clone) {
            Some(record) => off_runtime(move || record.source_unchanged()).await,
            None => true,
        }
    }

    /// `Continue` on a new connection. `Err(Some(completed))` if the receiver
    /// no longer has the transfer, `Err(None)` to try again later.
    async fn handshake(&self, conn: Connection) -> Result<(Streams, u64), Option<bool>> {
        // Known to shutdown from the first byte, so that it is suspended like
        // any other and never cut off by the endpoint closing.
        self.session.set_current(Some(conn.clone()));
        let token = self.token;
        let answer: anyhow::Result<_> = async {
            let (mut send, mut recv) = conn.open_bi().await?;
            protocol::write_msg(&mut send, &Msg::Continue { token }).await?;
            let msg = tokio::time::timeout(ANSWER_TIMEOUT, protocol::read_msg(&mut recv)).await??;
            Ok((msg, send, recv))
        }
        .await;
        match answer {
            Ok((Msg::ContinueFrom { have }, send, recv)) if have <= self.size => {
                Ok((Streams { conn, send, recv }, have))
            }
            Ok((Msg::Gone { completed }, _, _)) => {
                conn.close(CLOSE_OK.into(), b"gone");
                Err(Some(completed))
            }
            _ => {
                conn.close(CLOSE_SUSPENDED.into(), b"retry");
                Err(None)
            }
        }
    }

    /// Streams from byte `have` on one connection.
    async fn stream(&mut self, s: Streams, have: u64) -> Result<Outcome, End> {
        let Streams {
            conn,
            mut send,
            recv,
        } = s;
        self.session.set_current(Some(conn.clone()));
        let _watch = AbortOnDrop(spawn_path_watch(self.shared.clone(), self.id, conn.clone()));
        // Everything the receiver says arrives through this channel, so the
        // main loop can wait for it and for the next write at the same time.
        let (ctl_tx, mut ctl_rx) = mpsc::unbounded_channel();
        let _reader = AbortOnDrop(tokio::spawn(async move {
            let mut recv = recv;
            while let Ok(msg) = protocol::read_msg(&mut recv).await {
                if ctl_tx.send(msg).is_err() {
                    break;
                }
            }
        }));
        self.peer_paused = false;
        self.peer_suspended = false;

        let result = self.pump(&mut send, &mut ctl_rx, have).await;
        match result {
            Ok(outcome) => {
                self.end(Some(CLOSE_OK), true).await;
                Ok(outcome)
            }
            Err(e) if e.is::<Lost>() => match peer_close(&conn) {
                PeerClose::Cancelled => {
                    self.end(None, false).await;
                    Err(End::Final(Outcome::CancelledByPeer))
                }
                PeerClose::Failed => {
                    self.end(None, false).await;
                    Err(End::Final(Outcome::Failed(
                        tr!(error_recipient_failed).into(),
                    )))
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
            },
            Err(e) => {
                self.end(Some(CLOSE_FAILED), false).await;
                Err(End::Final(Outcome::Failed(format!("{e:#}"))))
            }
        }
    }

    /// The hash of the first `have` bytes, from the nearest snapshot.
    async fn hasher_at(&mut self, have: u64) -> anyhow::Result<blake3::Hasher> {
        self.snapshots.rewind(have);
        let (from, mut hasher) = self.snapshots.best(have);
        if from == have {
            return Ok(hasher);
        }
        self.emit(Phase::Verifying);
        let path = self.data_path.clone();
        let cancel = self.ctl.cancel.clone();
        let shared = self.shared.clone();
        let id = self.id;
        tokio::task::spawn_blocking(move || {
            let mut throttle = Throttle::new();
            session::hash_range(
                &path,
                from,
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
        .await?
        .map_err(Into::into)
    }

    async fn pump(
        &mut self,
        send: &mut iroh::endpoint::SendStream,
        ctl_rx: &mut mpsc::UnboundedReceiver<Msg>,
        have: u64,
    ) -> anyhow::Result<Outcome> {
        let mut hasher = self.hasher_at(have).await?;
        self.emit_pause_phase();
        self.shared.emit(Event::Progress {
            id: self.id,
            done: have,
            total: self.size,
        });
        let mut file = tokio::fs::File::open(&self.data_path).await?;
        file.seek(SeekFrom::Start(have)).await?;
        if self.ctl.is_paused() {
            protocol::write_msg(send, &Msg::Paused)
                .await
                .map_err(lost)?;
        }

        let mut done = have;
        let mut buf = vec![0u8; CHUNK];
        // The frame being written: header and payload, and how much of it went out.
        let mut pending: Vec<u8> = Vec::new();
        let mut off = 0usize;
        let mut payload = 0usize;
        // A pause or resume by the user that has to wait until the frame is complete.
        let mut announce: Option<bool> = None;
        let mut throttle = Throttle::new();
        loop {
            let idle = pending.is_empty() && (*self.paused_rx.borrow() || self.peer_paused);
            if pending.is_empty() && !idle {
                if done == self.size {
                    break;
                }
                let want = CHUNK.min((self.size - done) as usize);
                let n = file.read(&mut buf[..want]).await?;
                if n == 0 {
                    bail!(tr!(error_file_shorter));
                }
                pending = protocol::encode(&Msg::Data { len: n as u32 })?;
                pending.extend_from_slice(&buf[..n]);
                off = 0;
                payload = n;
            }
            // The write branch is disabled while there is nothing to write; its
            // expression is still evaluated, so the slice must stay in range.
            let rest = if pending.is_empty() { 0 } else { off };
            tokio::select! {
                // Cancel safe: either some bytes are written or none.
                r = send.write(&pending[rest..]), if !pending.is_empty() => {
                    off += r.map_err(lost)?;
                    if off == pending.len() {
                        hasher.update(&pending[pending.len() - payload..]);
                        done += payload as u64;
                        self.snapshots.record(done, &hasher);
                        pending.clear();
                        off = 0;
                        if throttle.ready() || done == self.size {
                            self.shared.emit(Event::Progress { id: self.id, done, total: self.size });
                        }
                        if let Some(paused) = announce.take() {
                            let msg = if paused { Msg::Paused } else { Msg::Resumed };
                            protocol::write_msg(send, &msg).await.map_err(lost)?;
                        }
                    }
                }
                msg = ctl_rx.recv() => match msg {
                    Some(Msg::Paused) => {
                        self.peer_paused = true;
                        self.emit_pause_phase();
                    }
                    Some(Msg::Resumed) => {
                        self.peer_paused = false;
                        self.emit_pause_phase();
                    }
                    Some(Msg::Result { ok: false, message }) => {
                        bail!(tr!(error_peer_reported, message = message))
                    }
                    Some(other) => {
                        bail!(tr!(error_unexpected_answer, answer = format!("{other:?}")))
                    }
                    None => return Err(lost(tr!(error_connection_lost))),
                },
                _ = self.paused_rx.changed() => {
                    let paused = self.pause_changed(true).await;
                    if pending.is_empty() {
                        let msg = if paused { Msg::Paused } else { Msg::Resumed };
                        protocol::write_msg(send, &msg).await.map_err(lost)?;
                    } else {
                        announce = Some(paused);
                    }
                }
            }
        }

        let hash = *hasher.finalize().as_bytes();
        // From `Done` on, the receiver may have all of the item, which is the
        // receiver's once it has checked it: a cancel waits for its verdict
        // (also after a restart), and so the two sides never disagree on
        // whether the item arrived. Before `Done` goes out, so that no cancel
        // can close the connection behind it.
        self.session.defer_cancel();
        self.session
            .update(&self.shared.data_dir, |s, d| s.save_done_sent(d, true))
            .await;
        protocol::write_msg(send, &Msg::Done { hash })
            .await
            .map_err(lost)?;
        send.finish().map_err(lost)?;

        // Wait for the receiver's verdict: first that all of it arrived
        // intact, then, once it is saved there, the result.
        loop {
            match ctl_rx.recv().await {
                Some(Msg::Verified { unpacking }) => {
                    // The item is the receiver's now (also after a restart).
                    self.session.hand_over();
                    // The phase first: it says what is going on there, also
                    // why the transfer can no longer be cancelled.
                    if unpacking {
                        self.emit(Phase::PeerUnpacking);
                    }
                    self.shared.emit(Event::HandedOver { id: self.id });
                    self.session
                        .update(&self.shared.data_dir, move |s, d| s.save_done_hash(d, hash))
                        .await;
                }
                Some(Msg::Result { ok: true, .. }) => return Ok(Outcome::Completed(None)),
                Some(Msg::Result { ok: false, message }) => {
                    bail!(tr!(error_peer_reported, message = message))
                }
                Some(Msg::Paused | Msg::Resumed) => {}
                Some(other) => bail!(tr!(error_unexpected_answer, answer = format!("{other:?}"))),
                None => return Err(lost(tr!(error_connection_lost))),
            }
        }
    }
}
