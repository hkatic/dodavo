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
    CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_OK, CLOSE_SUSPENDED, Event, Outcome, Phase, Shared,
    Throttle,
    protocol::{self, CHUNK, Kind, Msg, Offer, Token},
    session::{
        self, Control, Lost, PeerClose, Role, Session, Side, Snapshots, Streams, TransferRecord,
        lost, peer_close,
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
    let cancel = ctl.cancel.clone();
    tokio::select! {
        outcome = start(shared, endpoint, id, to, path, kind, my_name, peer_name, &ctl) => outcome,
        _ = cancel.cancelled() => cancelled(shared, id),
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
    let cancel = ctl.cancel.clone();
    tokio::select! {
        outcome = continue_record(shared, endpoint, id, record, &ctl) => outcome,
        _ = cancel.cancelled() => cancelled(shared, id),
    }
}

/// The user cancelled: tell the receiver if connected, otherwise leave a
/// tombstone for its next `Query`, and delete what is no longer needed.
fn cancelled(shared: &Arc<Shared>, id: u64) -> Outcome {
    if let Some(session) = shared.session_by_id(id) {
        session.end(&shared.data_dir, Some(CLOSE_CANCELLED), false);
    }
    Outcome::Cancelled
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
    };
    if let Err(e) = record.save(&shared.data_dir) {
        session.close_current(CLOSE_FAILED, b"failed");
        return Outcome::Failed(format!("{e:#}"));
    }
    temp.0 = None;
    *session.record.lock().unwrap() = Some(record);
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
        record.discard(&shared.data_dir);
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
    *session.record.lock().unwrap() = Some(record);
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
            let zip_path = shared
                .temp_dir
                .join(format!("send-{}.zip", protocol::token_hex(token)));
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

    /// The user paused or resumed: remember it and show it.
    fn pause_changed(&mut self, connected: bool) -> bool {
        let paused = *self.paused_rx.borrow_and_update();
        self.session.save_paused(&self.shared.data_dir, paused);
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
                // Paused: nothing to do until the user resumes.
                let _ = self.paused_rx.changed().await;
                self.pause_changed(false);
                continue;
            }
            let unchanged = self
                .session
                .record
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|r| r.source_unchanged());
            if !unchanged {
                self.session.end(&self.shared.data_dir, None, false);
                return Err(Outcome::Failed(tr!(error_source_changed).into()));
            }
            let dialed = tokio::select! {
                r = dial(self.endpoint, self.to) => r,
                _ = self.paused_rx.changed() => {
                    self.pause_changed(false);
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
                match self.handshake(conn).await {
                    Ok(streams) => return Ok(streams),
                    Err(Some(completed)) => {
                        self.session.end(&self.shared.data_dir, None, completed);
                        return Err(if completed {
                            Outcome::Completed(None)
                        } else {
                            Outcome::CancelledByPeer
                        });
                    }
                    Err(None) => {}
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.paused_rx.changed() => {
                    self.pause_changed(false);
                }
            }
            delay = (delay * 2).min(RETRY_MAX);
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
        let data_dir = &self.shared.data_dir;
        match result {
            Ok(outcome) => {
                self.session.end(data_dir, Some(CLOSE_OK), true);
                Ok(outcome)
            }
            Err(e) if e.is::<Lost>() => match peer_close(&conn) {
                PeerClose::Cancelled => {
                    self.session.end(data_dir, None, false);
                    Err(End::Final(Outcome::CancelledByPeer))
                }
                PeerClose::Failed => {
                    self.session.end(data_dir, None, false);
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
                self.session.end(data_dir, Some(CLOSE_FAILED), false);
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
                    let paused = self.pause_changed(true);
                    if pending.is_empty() {
                        let msg = if paused { Msg::Paused } else { Msg::Resumed };
                        protocol::write_msg(send, &msg).await.map_err(lost)?;
                    } else {
                        announce = Some(paused);
                    }
                }
            }
        }

        protocol::write_msg(
            send,
            &Msg::Done {
                hash: *hasher.finalize().as_bytes(),
            },
        )
        .await
        .map_err(lost)?;
        send.finish().map_err(lost)?;

        // Wait for the receiver's verdict.
        loop {
            match ctl_rx.recv().await {
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
