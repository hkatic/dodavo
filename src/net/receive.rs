//! Receiving side of a transfer. The receiver never dials for data: after a
//! lost connection it waits for the sender to come back with `Continue`,
//! and only asks now and then (`Query`) whether the sender still has the
//! transfer at all.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
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
    Accept, CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_OK, CLOSE_SUSPENDED, Event, Outcome, Phase,
    ReceivedFolder, Shared, Throttle,
    protocol::{self, CHUNK, Kind, Msg, Offer, Token},
    send::AbortOnDrop,
    session::{
        self, Control, Ending, Lost, Owned, PeerClose, ReceiveFiles, Role, Session, Side, Streams,
        TransferRecord, lost, off_runtime, peer_close,
    },
    spawn_path_watch,
};
use crate::{
    archive::{self, Cancelled, FileId, ItemTimes},
    i18n::tr,
};

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
/// Tests put the names of items here whose receive is to stop once all of
/// their data has arrived, before it is checked, to simulate a shutdown
/// while the sender waits for the verdict. `VERIFY_HELD` lists those that
/// have stopped there.
#[cfg(test)]
pub(super) static HOLD_VERIFY: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
pub(super) static VERIFY_HELD: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Whether the receive of `name` is to wait before it checks its data (see
/// `HOLD_VERIFY`); notes that it does.
#[cfg(test)]
fn held_before_verifying(name: &str) -> bool {
    let hold = HOLD_VERIFY.lock().unwrap().iter().any(|n| n == name);
    let mut held = VERIFY_HELD.lock().unwrap();
    if hold && !held.iter().any(|n| n == name) {
        held.push(name.to_string());
    }
    hold
}
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

/// The outcome of a receive that ended with `error` in its last step:
/// cancelled if the step stopped because the user cancelled, which it says
/// with [`Cancelled`]. Any other error is a failure, also one after a cancel
/// that came too late.
fn failed_or_cancelled(error: anyhow::Error) -> Outcome {
    if archive::is_cancelled(&error) {
        Outcome::Cancelled
    } else {
        Outcome::Failed(format!("{error:#}"))
    }
}

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

/// The dates the sender gave the item.
fn offer_times(offer: &Offer) -> ItemTimes {
    ItemTimes {
        modified: offer.modified,
        accessed: None,
        created: offer.created,
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
    let Some(Accept {
        dir: dest_dir,
        folder,
    }) = decision
    else {
        let _ = protocol::write_msg(&mut send, &Msg::Decline).await;
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        shared.finish(id, Outcome::Declined);
        return;
    };

    let outcome = accept(
        &shared, &endpoint, id, &ctl, conn, send, recv, offer, from, dest_dir, folder,
    )
    .await;
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
    let data_dir = shared.data_dir.clone();
    let checked = off_runtime(move || check_restored(record, &data_dir)).await;
    let restored = checked.and_then(|(record, peer, placed)| {
        Receiver::restore(shared, endpoint, id, record, peer, &ctl).map(|r| (r, placed))
    });
    let (mut receiver, placed) = match restored {
        Ok(restored) => restored,
        Err(outcome) => return outcome,
    };
    if placed.is_some() {
        // The item is in place already: a cancel, also one that is waiting
        // already, comes too late and must not delete it.
        receiver.session.hand_over();
    }
    tokio::select! {
        biased;
        ending = cancelled(shared, id, &ctl, None) => {
            ending.forget(&shared.data_dir).await;
            Outcome::Cancelled
        }
        o = receiver.continue_restored(placed) => o,
    }
}

/// Checks a receive from an earlier run before it goes on: the sender's
/// Contact ID, and the part file, which is cut back to what is known to be
/// on disk. What fails ends the transfer. Also returns the received item if
/// it is in place already (see `TransferRecord::placed_item`). Blocking.
fn check_restored(
    record: TransferRecord,
    data_dir: &Path,
) -> Result<(TransferRecord, EndpointId, Option<PathBuf>), Outcome> {
    let Some(peer) = record.peer_id() else {
        record.discard(data_dir);
        return Err(Outcome::Failed(tr!(error_bad_resume).into()));
    };
    // Data beyond the last flush may never have reached the disk.
    if let Err(e) = record.truncate_to_synced() {
        record.discard(data_dir);
        return Err(Outcome::Failed(format!("{e:#}")));
    }
    let placed = record.placed_item().map(Path::to_path_buf);
    Ok((record, peer, placed))
}

/// Finishes once the user has cancelled and the transfer has ended for it
/// (see `net::cancelled`): the sender is told if connected (on the offer's
/// connection `conn` until the transfer has one of its own), otherwise a
/// tombstone waits for its next `Continue`; the result deletes the partial
/// data. Never finishes while the last step runs: a cancel during unpacking
/// is left to that step, which stops and ends the transfer as cancelled,
/// and one that comes when the item is already being put in place is too
/// late, so the transfer ends by itself, completed (see `Session::cancel`).
async fn cancelled(shared: &Shared, id: u64, ctl: &Control, conn: Option<&Connection>) -> Ending {
    let ending = super::cancelled(shared, id, ctl).await;
    // Also when the transfer had not got to its own connection yet.
    if let Some(conn) = conn {
        conn.close(CLOSE_CANCELLED.into(), b"cancelled");
    }
    ending
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
    /// For folders: what is kept of the archive.
    folder: ReceivedFolder,
    /// The dates the sender gave the item.
    times: ItemTimes,
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

/// Decides once who has the part of a new receive: the thread that makes
/// it, which then makes the record and hands both to the transfer, or a
/// cancel that comes meanwhile, which ends the receive at once, without
/// waiting for the thread, which then deletes the part again and makes no
/// record (one that a later start would take for an unfinished receive).
/// Whichever comes first.
#[derive(Clone, Default)]
struct First(Arc<AtomicBool>);

impl First {
    /// Whether this caller came first.
    fn take(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    ctl: &Control,
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    offer: Offer,
    from: EndpointId,
    dest_dir: PathBuf,
    folder: ReceivedFolder,
) -> Outcome {
    let name = archive::sanitize_file_name(&offer.name);
    // On a thread of its own, which a cancel does not wait for: the
    // destination may not answer for a long time (a network folder that is
    // gone, say).
    let first = First::default();
    let mut making = {
        let (dir, temp_dir, data_dir) = (
            dest_dir.clone(),
            shared.temp_dir.clone(),
            shared.data_dir.clone(),
        );
        let (name, offer, paused, first) = (
            name.clone(),
            offer.clone(),
            ctl.paused.subscribe(),
            first.clone(),
        );
        tokio::task::spawn_blocking(move || {
            new_receive(
                &dir, &temp_dir, &data_dir, &name, offer, from, folder, &paused, &first,
            )
        })
    };
    let made = tokio::select! {
        biased;
        made = session::joined(&mut making) => made,
        () = ctl.cancel.cancelled() => {
            if first.take() {
                conn.close(CLOSE_CANCELLED.into(), b"cancelled");
                return Outcome::Cancelled;
            }
            // Made already: the transfer takes them, and the cancel ends it
            // right away (below).
            session::joined(&mut making).await
        }
    };
    let record = match made {
        Ok(record) => record,
        Err(_) if ctl.cancel.flag().load(Ordering::Relaxed) => {
            conn.close(CLOSE_CANCELLED.into(), b"cancelled");
            return Outcome::Cancelled;
        }
        Err(e) => {
            conn.close(CLOSE_FAILED.into(), b"failed");
            return Outcome::Failed(format!("{e:#}"));
        }
    };
    let (session, session_rx) = Session::new(id, record.token, from, Role::Receive);
    let part_path = record
        .receive_files()
        .map(|files| files.part.clone())
        .unwrap_or_default();
    let size = record.offer.size;
    let kind = record.offer.kind;
    let times = offer_times(&record.offer);
    session.set_record(record);
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
        folder,
        times,
        size,
        have: 0,
        hasher: Some(blake3::Hasher::new()),
        done_hash: None,
        peer_paused: Arc::new(AtomicBool::new(false)),
        peer_suspended: false,
    };
    let streams = Streams {
        conn: conn.clone(),
        send,
        recv,
    };
    tokio::select! {
        biased;
        ending = cancelled(shared, id, ctl, Some(&conn)) => {
            ending.forget(&shared.data_dir).await;
            Outcome::Cancelled
        }
        o = receiver.start(streams) => o,
    }
}

/// Makes the part file of a new receive and its record, and saves the
/// record, unless a cancel came first (see [`First`]): then it deletes the
/// part again and fails with [`Cancelled`]. `paused` tells whether the user
/// has paused the receive by now. Blocking.
#[allow(clippy::too_many_arguments)]
fn new_receive(
    dest_dir: &Path,
    temp_dir: &Path,
    data_dir: &Path,
    name: &str,
    offer: Offer,
    from: EndpointId,
    folder: ReceivedFolder,
    paused: &watch::Receiver<bool>,
    first: &First,
) -> anyhow::Result<TransferRecord> {
    let (part, part_id, part_created) =
        prepare_part(dest_dir, temp_dir, name, offer.kind, folder, &offer.token)?;
    if !first.take() {
        // Cancelled meanwhile: nothing of it may stay.
        let _ = std::fs::remove_file(&part);
        bail!(Cancelled);
    }
    let record = TransferRecord {
        token: offer.token,
        peer: from.to_string(),
        peer_name: offer.sender_name.clone(),
        paused: *paused.borrow(),
        created: std::time::SystemTime::now(),
        side: Side::Receive {
            dest_dir: dest_dir.to_path_buf(),
            files: ReceiveFiles::new(part.clone()),
            part_id: Some(part_id),
            part_created,
            synced: Some(0),
            folder,
        },
        offer,
        done_hash: None,
        done_sent: false,
        cancelled: false,
        ended: false,
        ended_at: None,
    };
    if let Err(e) = record.save(data_dir) {
        let _ = std::fs::remove_file(&part);
        return Err(e.into());
    }
    Ok(record)
}

/// Creates the part file of a new receive, without replacing anything, and
/// returns it with its identity and its creation time. Blocking.
fn prepare_part(
    dest_dir: &Path,
    temp_dir: &Path,
    name: &str,
    kind: Kind,
    folder: ReceivedFolder,
    token: &Token,
) -> anyhow::Result<(PathBuf, FileId, Option<SystemTime>)> {
    std::fs::create_dir_all(dest_dir)?;
    // A zip that is only unpacked arrives in the temp folder; files go
    // straight to the destination folder, and so do the zips of folders that
    // are kept as such.
    let part = if session::temporary_part(kind, folder) {
        std::fs::create_dir_all(temp_dir)?;
        let zip = session::temp_archive(temp_dir, Role::Receive, token);
        std::fs::File::create(&zip)?;
        zip
    } else {
        let part_name = match kind {
            Kind::File => format!("{name}.part"),
            Kind::Folder => format!("{name}.zip.part"),
        };
        // Created here and not over anything, also when another transfer of
        // the same name starts at the same moment.
        let part = archive::create_unique_file(dest_dir, &part_name)?;
        // Marked from the start: the mark stays with the file when it is
        // renamed into place, so it is never there unmarked.
        let _ = archive::mark_of_the_web(&part);
        part
    };
    // Its own creation time, not that of a file of the same name deleted
    // moments before, which Windows may give it: on FAT and exFAT the time
    // tells the part from such a file (see `Side::Receive::part_created`).
    let _ = archive::set_times(&part, None, Some(SystemTime::now()));
    let created = std::fs::symlink_metadata(&part)
        .and_then(|m| m.created())
        .ok();
    match archive::file_id(&part) {
        Ok(id) => Ok((part, id, created)),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e.into())
        }
    }
}

/// Whether the part file of a running receive is still its own (see
/// `TransferRecord::part_is_there`): it is opened by its name for each
/// connection. Also when there is nothing to tell, the transfer having no
/// record any more: then it is ending anyway.
async fn part_still_there(session: &Session) -> bool {
    let Some(record) = session.with_record(TransferRecord::clone) else {
        return true;
    };
    off_runtime(move || record.part_is_there()).await
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
/// about pauses and resumes by the user, and sends what the receive has to
/// say (`Verified`, then the `Result`, which is the last message).
async fn control_writer(
    mut send: SendStream,
    mut paused_rx: watch::Receiver<bool>,
    mut msgs: mpsc::UnboundedReceiver<Msg>,
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
                session.update(&shared.data_dir, move |s, d| s.save_paused(d, paused)).await;
                shared.emit(Event::Phase {
                    id,
                    phase: pause_phase(paused, peer_paused.load(Ordering::Relaxed)),
                });
                let msg = if paused { Msg::Paused } else { Msg::Resumed };
                if protocol::write_msg(&mut send, &msg).await.is_err() {
                    return;
                }
            }
            msg = msgs.recv() => {
                let Some(msg) = msg else { return };
                let last = matches!(msg, Msg::Result { .. });
                if protocol::write_msg(&mut send, &msg).await.is_err() {
                    return;
                }
                if last {
                    let _ = send.finish();
                    return;
                }
            }
        }
    }
}

impl<'a> Receiver<'a> {
    /// A receive from an earlier run (checked by `check_restored`), with its
    /// session registered right away (before anything waits), so that a
    /// cancel, or a `Continue` from the sender, finds it.
    fn restore(
        shared: &'a Arc<Shared>,
        endpoint: &'a Endpoint,
        id: u64,
        record: TransferRecord,
        peer: EndpointId,
        ctl: &'a Control,
    ) -> Result<Self, Outcome> {
        let Side::Receive {
            dest_dir,
            files,
            folder,
            ..
        } = &record.side
        else {
            return Err(Outcome::Failed(tr!(error_bad_resume).into()));
        };
        let (session, session_rx) = Session::new(id, record.token, peer, Role::Receive);
        let receiver = Receiver {
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
            part_path: files.part.clone(),
            folder: *folder,
            times: offer_times(&record.offer),
            size: record.offer.size,
            have: record.have().min(record.offer.size),
            hasher: None,
            done_hash: record.done_hash,
            peer_paused: Arc::new(AtomicBool::new(false)),
            peer_suspended: false,
        };
        let token = record.token;
        session.set_record(record);
        shared.add_session(session);
        shared.restored(&token);
        Ok(receiver)
    }

    /// Goes on with a receive from an earlier run. `placed` is the item if it
    /// was in place already: only the end of the transfer was interrupted
    /// (see `check_restored`).
    async fn continue_restored(&mut self, placed: Option<PathBuf>) -> Outcome {
        if let Some(path) = placed {
            let times = (self.kind == Kind::File).then_some(self.times);
            let ending = self.session.end(None, true);
            let (data_dir, item) = (self.shared.data_dir.clone(), path.clone());
            off_runtime(move || {
                if let Some(times) = times {
                    times.apply(&item);
                }
                ending.forget_blocking(&data_dir);
            })
            .await;
            return Outcome::Completed(Some(path));
        }
        // Everything had arrived and the sender was told so: only the last
        // step (renaming, or unpacking the folder) was interrupted.
        if let Some(hash) = self.done_hash
            && self.have == self.size
        {
            return self.complete_alone(hash).await;
        }
        self.emit_waiting();
        self.drive(None).await
    }

    /// Starts a new receive on the connection of its offer.
    async fn start(&mut self, mut streams: Streams) -> Outcome {
        if protocol::write_msg(&mut streams.send, &Msg::Accept)
            .await
            .is_err()
        {
            // The sender never learnt that we accepted, so it will not come back.
            self.end(None, false).await;
            return Outcome::Failed(tr!(error_connection_lost).into());
        }
        self.drive(Some(streams)).await
    }

    fn emit(&self, phase: Phase) {
        self.shared.emit(Event::Phase { id: self.id, phase });
    }

    /// Ends the transfer on this side (see `Session::end`) and deletes what
    /// goes, off the runtime's worker threads.
    async fn end(&self, code: Option<u32>, completed: bool) {
        let ending = self.session.end(code, completed);
        ending.forget(&self.shared.data_dir).await;
    }

    /// Whether the last step unpacks the folder.
    fn unpacks(&self) -> bool {
        self.kind == Kind::Folder && self.folder != ReceivedFolder::KeepZip
    }

    /// Answers senders that come back while the last step runs (which
    /// nothing reads new connections in) with "try again later" while the
    /// result exists. Once the step is over, the tombstone answers them;
    /// without this, their connections would pile up unanswered until they
    /// give up on each.
    fn turn_away_new_connections(&mut self) -> AbortOnDrop {
        let (_unused, placeholder) = mpsc::unbounded_channel();
        let mut new_conns = std::mem::replace(&mut self.session_rx, placeholder);
        AbortOnDrop(tokio::spawn(async move {
            while let Some(s) = new_conns.recv().await {
                s.conn.close(CLOSE_SUSPENDED.into(), b"finishing");
            }
        }))
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
                        self.end(None, false).await;
                        return Err(Outcome::CancelledByPeer);
                    }
                }
                _ = paused_rx.changed() => {
                    let paused = *paused_rx.borrow_and_update();
                    self.session
                        .update(&self.shared.data_dir, move |s, d| s.save_paused(d, paused))
                        .await;
                    self.emit_waiting();
                }
            }
        }
    }

    /// The sender's hash is known: check what is on disk and finish without
    /// the sender (after a restart during the last step, or when the sender
    /// already counts the transfer as complete).
    async fn complete_alone(&mut self, expected: [u8; 32]) -> Outcome {
        let _turn_away = self.turn_away_new_connections();
        self.hasher = None;
        if let Err(e) = self.rehash().await {
            self.end(None, false).await;
            return Outcome::Failed(format!("{e:#}"));
        }
        if *self.hasher.as_ref().unwrap().finalize().as_bytes() != expected {
            self.end(None, false).await;
            return Outcome::Failed(tr!(error_damaged).into());
        }
        match self.finalize().await {
            Ok(path) => Outcome::Completed(Some(path)),
            Err(e) => {
                self.end(None, false).await;
                failed_or_cancelled(e)
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
        let (msgs_tx, msgs_rx) = mpsc::unbounded_channel();
        let writer = tokio::spawn(control_writer(
            send,
            self.ctl.paused.subscribe(),
            msgs_rx,
            self.shared.clone(),
            self.id,
            self.session.clone(),
            self.peer_paused.clone(),
        ));

        let result = self.pump(recv, msgs_tx).await;
        match result {
            Ok(path) => {
                // The item is in place and the transfer has ended (see
                // `finalize`). Let the Result go out, and give the sender a
                // moment to read it and close.
                let _ = tokio::time::timeout(Duration::from_secs(10), writer).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                self.session.set_current(None);
                Ok(path)
            }
            Err(e) if e.is::<Lost>() => {
                writer.abort();
                match peer_close(&conn) {
                    PeerClose::Cancelled => {
                        self.end(None, false).await;
                        Err(End::Final(Outcome::CancelledByPeer))
                    }
                    PeerClose::Failed => {
                        self.end(None, false).await;
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
                self.end(None, false).await;
                Err(End::Final(Outcome::Failed(format!("{e:#}"))))
            }
            Err(e) => {
                writer.abort();
                let outcome = failed_or_cancelled(e);
                let code = match outcome {
                    Outcome::Cancelled => CLOSE_CANCELLED,
                    _ => CLOSE_FAILED,
                };
                self.end(Some(code), false).await;
                Err(End::Final(outcome))
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

    /// Receives the rest of the data, checks it and puts the item in place,
    /// telling the sender through `msgs` (see `control_writer`).
    async fn pump(
        &mut self,
        mut recv: RecvStream,
        msgs: mpsc::UnboundedSender<Msg>,
    ) -> anyhow::Result<PathBuf> {
        // While the transfer waited, the part may have been deleted and its
        // name taken by another transfer's part, which is not written to.
        if !part_still_there(&self.session).await {
            bail!(tr!(error_part_missing));
        }
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
        #[cfg(test)]
        while held_before_verifying(&self.name) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        if hash != *self.hasher.as_ref().unwrap().finalize().as_bytes() {
            let _ = msgs.send(Msg::Result {
                ok: false,
                message: tr!(error_checksum).into(),
            });
            return Err(Reported(tr!(error_damaged).into()).into());
        }
        // From here on this side can finish on its own, even after a restart.
        self.done_hash = Some(hash);
        self.session
            .update(&self.shared.data_dir, move |s, d| s.save_done_hash(d, hash))
            .await;
        // The item is this side's now: the sender waits for the result, and
        // its cancel comes too late (see `Msg::Verified`).
        let _ = msgs.send(Msg::Verified {
            unpacking: self.unpacks(),
        });
        let _turn_away = self.turn_away_new_connections();
        let placed = self.finalize().await?;
        let _ = msgs.send(Msg::Result {
            ok: true,
            message: String::new(),
        });
        Ok(placed)
    }

    /// Notes in the record how much of the part file is on disk.
    async fn save_synced(&self) {
        let have = self.have;
        self.session
            .update(&self.shared.data_dir, move |s, d| s.save_synced(d, have))
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

    /// Moves the file into place, or unpacks the folder and keeps its zip
    /// as chosen. Once it succeeds, the transfer has ended as completed.
    async fn finalize(&mut self) -> anyhow::Result<PathBuf> {
        if self.unpacks() {
            self.emit(Phase::Unpacking);
            #[cfg(test)]
            while HOLD_FINALIZE.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let shared = self.shared.clone();
        let id = self.id;
        let cancel = self.ctl.cancel.clone();
        let (kind, folder, times) = (self.kind, self.folder, self.times);
        let (dest, name) = (self.dest_dir.clone(), self.name.clone());
        let transfer = Transfer {
            session: self.session.clone(),
            data_dir: shared.data_dir.clone(),
        };
        tokio::task::spawn_blocking(move || {
            // Until this is over, a cancel is left to it.
            let _finishing = transfer.session.finishing()?;
            match kind {
                Kind::File => finish_file(&transfer, &dest, &name, times),
                Kind::Folder => {
                    let mut throttle = Throttle::new();
                    let mut progress = |done, total| {
                        if throttle.ready() || done == total {
                            shared.emit(Event::Progress { id, done, total });
                        }
                    };
                    finish_folder(
                        &transfer,
                        &dest,
                        &name,
                        folder,
                        times,
                        cancel.flag(),
                        &mut progress,
                    )
                }
            }
        })
        .await?
    }
}

/// A running receive as its last step sees it: the session, and the data
/// folder for its record.
struct Transfer {
    session: Arc<Session>,
    data_dir: PathBuf,
}

impl Transfer {
    fn files(&self) -> anyhow::Result<ReceiveFiles> {
        self.session.receive_files().ok_or_else(|| Cancelled.into())
    }

    /// Notes `files` in the record (see `Session::save_receive_files`).
    fn save(&self, files: &ReceiveFiles) -> io::Result<()> {
        self.session.save_receive_files(&self.data_dir, files)
    }

    /// Runs the step that puts the item in place, and ends the transfer as
    /// completed (`Session::place`).
    fn place(&self, step: &mut dyn FnMut() -> anyhow::Result<PathBuf>) -> anyhow::Result<PathBuf> {
        self.session.place(&self.data_dir, step)
    }

    /// Renames the part file to `dir\name`, or to the next free name, and
    /// returns where it went. Each name is noted in the record before it is
    /// tried (`Side::Receive::moving`), so that a crash in between never
    /// loses the part; once the transfer is to end, nothing is tried.
    fn move_part(&self, files: &mut ReceiveFiles, dir: &Path, name: &str) -> io::Result<PathBuf> {
        let id = archive::file_id(&files.part)?;
        let part = files.part.clone();
        let moved = archive::move_to_unique_noted(&part, dir, name, false, &mut |to| {
            files.moving = Some(Owned {
                path: to.to_path_buf(),
                id,
            });
            self.save(files)
        });
        match moved {
            Ok(to) => {
                files.part.clone_from(&to);
                // Best effort: the note above finds the part after a crash.
                let _ = self.save(files);
                Ok(to)
            }
            Err(e) => {
                self.unnote_move(files);
                Err(e)
            }
        }
    }

    /// Forgets the note of a rename of the part that did not happen, so that
    /// a later start never takes what has that name for the part (see
    /// `TransferRecord::follow_moved_part`). Best effort.
    fn unnote_move(&self, files: &mut ReceiveFiles) {
        if files.moving.take().is_some() {
            let _ = self.save(files);
        }
    }

    /// Notes the new folder `folder` that is about to be unpacked into (and
    /// whatever else `files` says by now). A folder with anything in it is
    /// always noted, so that a later start or the end of the transfer can
    /// delete it: the record keeps the note also when saving it fails (see
    /// `Session::save_receive_files`), and only a folder that could not be
    /// noted at all is removed here.
    fn note_folder(&self, files: &mut ReceiveFiles, folder: &Path) -> io::Result<()> {
        let owned = match Owned::at(folder) {
            Ok(owned) => owned,
            Err(e) => {
                let _ = std::fs::remove_dir(folder);
                return Err(e);
            }
        };
        files.unpacking = Some(owned);
        self.save(files)
    }
}

/// Puts a received file in place under a free name, with the sender's
/// dates. Returns where it went.
fn finish_file(
    transfer: &Transfer,
    dest: &Path,
    name: &str,
    times: ItemTimes,
) -> anyhow::Result<PathBuf> {
    let mut files = transfer.files()?;
    transfer.place(&mut || {
        let path = transfer.move_part(&mut files, dest, name)?;
        times.apply(&path);
        Ok(path)
    })
}

fn zip_name(stem: &str) -> String {
    format!("{stem}.zip")
}

/// Unpacks a received folder from its zip, keeps the zip as `<name>.zip`,
/// or both, as `folder` says. The unpacked folder gets the sender's dates.
/// Returns the folder, or the zip when only that is kept. Either all of it
/// is in place or, after a failure, none of it: the folder is removed here,
/// and a zip that already has its name goes with the part when the transfer
/// ends.
///
/// The folder is always a new one, so that everything in it is this
/// transfer's. After a restart, the folder that an earlier run began to
/// unpack into (`Side::Receive::unpacking`) is deleted first, since what is
/// in it may have changed since or be in use, and the archive is unpacked
/// again into a new folder, which gets the same name when the old one could
/// be deleted. One that cannot be deleted yet is noted as a leftover and
/// goes later.
fn finish_folder(
    transfer: &Transfer,
    dest: &Path,
    name: &str,
    folder: ReceivedFolder,
    times: ItemTimes,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<PathBuf> {
    let mut files = transfer.files()?;
    if folder == ReceivedFolder::KeepZip {
        return transfer
            .place(&mut || Ok(transfer.move_part(&mut files, dest, &zip_name(name))?));
    }
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!(Cancelled);
    }
    // Noted with the new folder (until then the record still names it).
    if let Some(earlier) = files.unpacking.take()
        && !earlier.remove_folder()
    {
        files.leftovers.push(earlier);
    }
    let both = folder == ReceivedFolder::Both;
    let target = if both && !files.part_in_place() {
        new_pair(transfer, &mut files, dest, name)?
    } else {
        // With its zip in place already (after a restart), the folder gets
        // the zip's name.
        let stem = match files.part.file_stem() {
            Some(stem) if both => stem.to_string_lossy().into_owned(),
            _ => name.to_string(),
        };
        let target = archive::create_unique_dir(dest, &stem, |_| true)?;
        transfer.note_folder(&mut files, &target)?;
        target
    };
    let mut partial = Cleanup(Some(target.clone()));
    archive::unzip(&files.part, &target, true, cancel, progress)?;
    times.apply(&target);
    let placed = transfer.place(&mut || Ok(target.clone()))?;
    partial.0 = None;
    Ok(placed)
}

/// For a folder that is kept with its zip: a new folder, and the part moved
/// next to it under the zip's name, both with the same number ("Photos (2)"
/// and "Photos (2).zip"), before anything is unpacked. Both are noted in the
/// record first. Returns the folder.
fn new_pair(
    transfer: &Transfer,
    files: &mut ReceiveFiles,
    dest: &Path,
    name: &str,
) -> anyhow::Result<PathBuf> {
    let id = archive::file_id(&files.part)?;
    let zip_of = |stem: &str| dest.join(zip_name(stem));
    loop {
        let target =
            archive::create_unique_dir(dest, name, |stem| !archive::name_taken(&zip_of(stem)))?;
        let zip = zip_of(&target.file_name().unwrap_or_default().to_string_lossy());
        files.moving = Some(Owned {
            path: zip.clone(),
            id,
        });
        // A failure from here on leaves the folder noted: the end of the
        // transfer deletes it.
        if let Err(e) = transfer.note_folder(files, &target) {
            transfer.unnote_move(files);
            return Err(e.into());
        }
        match archive::rename_new(&files.part, &zip) {
            Ok(()) => {
                files.part = zip;
                // Best effort: `moving` finds the part after a crash.
                let _ = transfer.save(files);
                return Ok(target);
            }
            // The zip's name was taken after all: the next pair. The empty
            // folder goes now, or later as a leftover.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if let Some(empty) = files.unpacking.take()
                    && std::fs::remove_dir(&empty.path).is_err()
                {
                    files.leftovers.push(empty);
                }
                transfer.unnote_move(files);
            }
            Err(e) => {
                transfer.unnote_move(files);
                return Err(e.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dodavo-receive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn times(secs: u64) -> (ItemTimes, SystemTime) {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let filetime = secs * 10_000_000 + archive::FILETIME_UNIX_EPOCH;
        let times = ItemTimes {
            modified: Some(filetime),
            accessed: None,
            created: Some(filetime),
        };
        (times, at)
    }

    /// A zip of a folder with one file, as the sender makes it.
    fn received_zip(base: &Path, part_name: &str) -> PathBuf {
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"alpha").unwrap();
        let part = base.join("dest").join(part_name);
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        let cancel = AtomicBool::new(false);
        archive::zip_folder(&src, &part, &cancel, &mut |_, _| {}).unwrap();
        part
    }

    /// A new part is given the time of its creation, not that of a file of
    /// the same name deleted a moment before, which Windows would give it:
    /// on FAT and exFAT the time tells the part from such a file.
    #[test]
    fn a_new_part_is_created_now() {
        let base = scratch("created-now");
        let old = base.join("a.txt.part");
        std::fs::write(&old, b"old").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(400 * 24 * 60 * 60);
        archive::set_times(&old, None, Some(long_ago)).unwrap();
        std::fs::remove_file(&old).unwrap();
        let temp = base.join("temp");
        let (part, id, created) = prepare_part(
            &base,
            &temp,
            "a.txt",
            Kind::File,
            ReceivedFolder::Extract,
            &[1; 16],
        )
        .unwrap();
        assert_eq!(part, old);
        assert_eq!(id, archive::file_id(&part).unwrap());
        let created = created.unwrap();
        assert_eq!(
            std::fs::metadata(&part).unwrap().created().unwrap(),
            created
        );
        let age = SystemTime::now().duration_since(created).unwrap();
        assert!(age < Duration::from_secs(10 * 60), "{age:?}");
        std::fs::remove_dir_all(&base).ok();
    }

    /// The part is opened by its name for each connection, and written to
    /// only while it is still the transfer's: not once the user has deleted
    /// it and another file has taken its name.
    #[tokio::test(flavor = "multi_thread")]
    async fn only_its_own_part_is_written_to() {
        let base = scratch("still-there");
        let part = base.join("dest").join("a.txt.part");
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        assert!(part_still_there(&transfer.session).await);
        std::fs::remove_file(&part).unwrap();
        std::fs::write(&part, b"another").unwrap();
        assert!(!part_still_there(&transfer.session).await);
        transfer
            .session
            .end(None, false)
            .forget(&transfer.data_dir)
            .await;
        assert_eq!(std::fs::read(&part).unwrap(), b"another");
        assert!(part_still_there(&transfer.session).await, "nothing to tell");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A receive of "Photos" (or of a file "a.txt") with its part at `part`,
    /// as a running transfer has it, with its record in `base\data`.
    fn receiving(base: &Path, part: &Path, kind: Kind, folder: ReceivedFolder) -> Transfer {
        let data_dir = base.join("data");
        let token = [7; 16];
        let name = match kind {
            Kind::File => "a.txt",
            Kind::Folder => "Photos",
        };
        let record = TransferRecord {
            token,
            peer: "ab".repeat(32),
            offer: Offer {
                kind,
                name: name.into(),
                size: 5,
                file_count: 1,
                content_size: 5,
                sender_name: "Ana".into(),
                token,
                modified: None,
                created: None,
            },
            peer_name: "Ana".into(),
            paused: false,
            created: SystemTime::now(),
            side: Side::Receive {
                dest_dir: part.parent().unwrap().to_path_buf(),
                files: ReceiveFiles::new(part.to_path_buf()),
                part_id: archive::file_id(part).ok(),
                part_created: None,
                synced: None,
                folder,
            },
            done_hash: None,
            done_sent: false,
            cancelled: false,
            ended: false,
            ended_at: None,
        };
        record.save(&data_dir).unwrap();
        let peer = iroh::SecretKey::generate().public();
        let (session, _) = Session::new(1, token, peer, Role::Receive);
        session.set_record(record);
        Transfer { session, data_dir }
    }

    /// Runs the last step of a received folder, as `finalize` does.
    fn finish_with(
        transfer: &Transfer,
        dest: &Path,
        folder: ReceivedFolder,
        t: ItemTimes,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> anyhow::Result<PathBuf> {
        let _finishing = transfer.session.finishing()?;
        finish_folder(transfer, dest, "Photos", folder, t, cancel, progress)
    }

    fn finish(transfer: &Transfer, dest: &Path, folder: ReceivedFolder, t: ItemTimes) -> PathBuf {
        let cancel = AtomicBool::new(false);
        let path = finish_with(transfer, dest, folder, t, &cancel, &mut |_, _| {}).unwrap();
        // The transfer has ended as completed.
        assert!(transfer.session.with_record(|_| ()).is_none());
        assert_eq!(session::tombstone(&transfer.data_dir, &[7; 16]), Some(true));
        assert!(session::records(&transfer.data_dir).is_empty());
        path
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A cancel that comes while the part of a new receive is being made
    /// wins: the part goes again, and no record is made, which a later start
    /// would take for an unfinished receive.
    #[test]
    fn a_cancel_while_the_part_is_made_leaves_nothing() {
        let base = scratch("first");
        let (dest, temp, data_dir) = (base.join("dest"), base.join("temp"), base.join("data"));
        let offer = Offer {
            kind: Kind::File,
            name: "a.txt".into(),
            size: 5,
            file_count: 1,
            content_size: 5,
            sender_name: "Ana".into(),
            token: [8; 16],
            modified: None,
            created: None,
        };
        let from = iroh::SecretKey::generate().public();
        let (_paused_tx, paused) = watch::channel(false);
        let make = |first: &First| {
            new_receive(
                &dest,
                &temp,
                &data_dir,
                "a.txt",
                offer.clone(),
                from,
                ReceivedFolder::Extract,
                &paused,
                first,
            )
        };

        let cancel_first = First::default();
        assert!(cancel_first.take(), "the cancel comes first");
        assert!(archive::is_cancelled(&make(&cancel_first).unwrap_err()));
        assert!(names_in(&dest).is_empty());
        assert!(session::records(&data_dir).is_empty());

        // Made first: the transfer has the part and the record.
        let first = First::default();
        let record = make(&first).unwrap();
        assert!(!first.take(), "a cancel now ends the transfer instead");
        assert_eq!(names_in(&dest), ["a.txt.part"]);
        assert_eq!(session::records(&data_dir), [record]);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_file_gets_a_free_name_and_its_dates() {
        let base = scratch("file");
        std::fs::write(base.join("a.txt"), b"old").unwrap();
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"new").unwrap();
        let (t, at) = times(1_500_000_000);
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let path = finish_file(&transfer, &base, "a.txt", t).unwrap();
        assert_eq!(path, base.join("a (2).txt"));
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!part.exists());
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (meta.modified().unwrap(), meta.created().unwrap()),
            (at, at)
        );
        assert!(transfer.session.with_record(|_| ()).is_none());
        std::fs::remove_dir_all(&base).ok();
    }

    /// A rename of the part that fails leaves no note of it behind, so that
    /// a later start never takes what gets that name for the part.
    #[test]
    fn a_failed_rename_leaves_no_note() {
        let base = scratch("unnoted");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"new").unwrap();
        let (t, _) = times(1_500_000_000);
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        assert!(finish_file(&transfer, &base.join("gone"), "a.txt", t).is_err());
        assert_eq!(transfer.files().unwrap().moving, None);
        let records = session::records(&transfer.data_dir);
        assert_eq!(records[0].receive_files().unwrap().moving, None);
        assert!(part.exists());
        // The transfer still runs: it can end as failed or be cancelled.
        assert!(transfer.session.cancel_blocking(&transfer.data_dir));
        assert!(!part.exists());
        std::fs::remove_dir_all(&base).ok();
    }

    /// An item that has all arrived intact is put in place even if its
    /// record cannot be saved any more: failing would delete it.
    #[test]
    fn the_last_step_goes_on_when_the_record_cannot_be_saved() {
        let base = scratch("unsaved");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"new").unwrap();
        let (t, _) = times(1_500_000_000);
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        // The record's temporary file cannot be made from now on.
        let tmp = session::records_dir(&transfer.data_dir)
            .join(format!("{}.json.tmp", protocol::token_hex(&[7; 16])));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = finish_file(&transfer, &base, "a.txt", t).unwrap();
        assert_eq!(path, base.join("a.txt"));
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(transfer.session.with_record(|_| ()).is_none(), "completed");
        assert!(session::records(&transfer.data_dir).is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_folder_is_unpacked_kept_as_zip_or_both() {
        let base = scratch("folder");
        let dest = base.join("dest");
        let (t, at) = times(1_400_000_000);

        // Only unpacked: the zip stays where it is (a temp file that the
        // end of the transfer deletes).
        let part = received_zip(&base, "recv.zip");
        let transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Extract);
        let path = finish(&transfer, &dest, ReceivedFolder::Extract, t);
        assert_eq!(path, dest.join("Photos"));
        assert_eq!(std::fs::read(path.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), at);
        assert!(path.join("a.txt:Zone.Identifier").exists());
        assert!(!part.exists(), "the temporary zip goes when it ends");

        // Only the zip.
        let part = received_zip(&base, "Photos.zip.part");
        let transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::KeepZip);
        let path = finish(&transfer, &dest, ReceivedFolder::KeepZip, t);
        assert_eq!(path, dest.join("Photos.zip"));
        assert!(path.is_file());
        assert!(!part.exists());
        assert!(!dest.join("Photos (2)").exists());

        // Both, with matching names: "Photos" and "Photos.zip" are taken,
        // and so is "Photos (2).zip".
        std::fs::write(dest.join("Photos (2).zip"), b"theirs").unwrap();
        let part = received_zip(&base, "Photos.zip.part");
        let transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Both);
        let path = finish(&transfer, &dest, ReceivedFolder::Both, t);
        assert_eq!(path, dest.join("Photos (3)"));
        assert_eq!(std::fs::read(path.join("a.txt")).unwrap(), b"alpha");
        assert!(dest.join("Photos (3).zip").is_file());
        assert_eq!(
            std::fs::read(dest.join("Photos (2).zip")).unwrap(),
            b"theirs"
        );
        assert!(!part.exists());
        std::fs::remove_dir_all(&base).ok();
    }

    /// A cancel during unpacking stops it; the folder goes at once, and the
    /// rest (the part, or the zip that already had its name) goes when the
    /// transfer ends: nothing of the item stays. A cancel before the last
    /// step lets it make nothing at all.
    #[test]
    fn a_cancelled_folder_leaves_nothing() {
        let base = scratch("cancelled");
        let dest = base.join("dest");
        let (t, _) = times(1_400_000_000);
        for folder in [ReceivedFolder::Extract, ReceivedFolder::Both] {
            let part = received_zip(&base, "Photos.zip.part");
            let transfer = receiving(&base, &part, Kind::Folder, folder);
            let cancel = AtomicBool::new(false);
            let unpacked = finish_with(&transfer, &dest, folder, t, &cancel, &mut |_, _| {
                cancel.store(true, Ordering::Relaxed);
                // As the network side does: the cancel is left to the step.
                assert!(!transfer.session.cancel_blocking(&transfer.data_dir));
            });
            assert!(unpacked.is_err());
            assert!(
                session::records(&transfer.data_dir)[0].ended,
                "a crash now deletes the files at the next start"
            );
            let left = names_in(&dest);
            assert_eq!(left.len(), 1, "only the part or the zip: {left:?}");
            transfer
                .session
                .end(None, false)
                .forget_blocking(&transfer.data_dir);
            assert!(names_in(&dest).is_empty());
            assert!(session::records(&transfer.data_dir).is_empty());

            let part = received_zip(&base, "Photos.zip.part");
            let transfer = receiving(&base, &part, Kind::Folder, folder);
            let cancel = AtomicBool::new(true);
            assert!(finish_with(&transfer, &dest, folder, t, &cancel, &mut |_, _| {}).is_err());
            assert_eq!(names_in(&dest), ["Photos.zip.part"]);
            transfer
                .session
                .end(None, false)
                .forget_blocking(&transfer.data_dir);
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// Notes in the record that the receive was unpacking into `folder`, as
    /// an earlier run that ended halfway left it.
    fn was_unpacking_into(transfer: &Transfer, folder: &Path) {
        let mut files = transfer.files().unwrap();
        files.unpacking = Some(Owned::at(folder).unwrap());
        transfer.save(&files).unwrap();
    }

    /// After a restart, the folder that was being unpacked into is replaced
    /// by a new one under the same name, not left next to a new copy; one
    /// that is no longer this transfer's is left alone.
    #[test]
    fn an_interrupted_unpacking_starts_over() {
        let base = scratch("again");
        let dest = base.join("dest");
        let (t, _) = times(1_400_000_000);
        for folder in [ReceivedFolder::Extract, ReceivedFolder::Both] {
            let part = received_zip(&base, "Photos.zip.part");
            let partial = dest.join("Photos");
            std::fs::create_dir_all(&partial).unwrap();
            std::fs::write(partial.join("a.txt"), b"al").unwrap();
            std::fs::write(partial.join("stale.txt"), b"not in the archive").unwrap();
            let transfer = receiving(&base, &part, Kind::Folder, folder);
            was_unpacking_into(&transfer, &partial);
            let path = finish(&transfer, &dest, folder, t);
            assert_eq!(path, partial);
            assert_eq!(names_in(&partial), ["a.txt"]);
            assert_eq!(std::fs::read(partial.join("a.txt")).unwrap(), b"alpha");
            let expected: &[&str] = match folder {
                ReceivedFolder::Both => &["Photos", "Photos.zip"],
                _ => &["Photos"],
            };
            assert_eq!(names_in(&dest), expected);
            std::fs::remove_dir_all(&dest).unwrap();
        }

        // The user deleted the folder and made one of the same name.
        let part = received_zip(&base, "recv.zip");
        let partial = dest.join("Photos");
        std::fs::create_dir_all(&partial).unwrap();
        let transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Extract);
        was_unpacking_into(&transfer, &partial);
        std::fs::remove_dir(&partial).unwrap();
        std::fs::create_dir(&partial).unwrap();
        std::fs::write(partial.join("mine.txt"), b"mine").unwrap();
        let path = finish(&transfer, &dest, ReceivedFolder::Extract, t);
        assert_eq!(path, dest.join("Photos (2)"));
        assert_eq!(names_in(&partial), ["mine.txt"]);
        std::fs::remove_dir_all(&dest).unwrap();

        // After a restart with the zip in place already ("Both"): the
        // folder gets the zip's name, and the zip stays.
        let zip = received_zip(&base, "Photos (5).zip");
        let transfer = receiving(&base, &zip, Kind::Folder, ReceivedFolder::Both);
        let mut files = transfer.files().unwrap();
        files.moving = Some(Owned::at(&zip).unwrap());
        transfer.save(&files).unwrap();
        let path = finish(&transfer, &dest, ReceivedFolder::Both, t);
        assert_eq!(path, dest.join("Photos (5)"));
        assert!(zip.is_file());
        std::fs::remove_dir_all(&base).ok();
    }

    /// A file of the earlier folder that another program holds open does
    /// not stop the receive: the archive is unpacked into a new folder, and
    /// what is left of the old one is the transfer's to delete later, not
    /// forgotten.
    #[test]
    fn a_held_file_in_the_earlier_folder_does_not_stop_it() {
        use std::os::windows::fs::OpenOptionsExt;
        let base = scratch("held");
        let dest = base.join("dest");
        let (t, _) = times(1_400_000_000);
        let part = received_zip(&base, "recv.zip");
        let partial = dest.join("Photos");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("a.txt"), b"al").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(partial.join("a.txt"))
            .unwrap();
        let transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Extract);
        was_unpacking_into(&transfer, &partial);
        let cancel = AtomicBool::new(false);
        let path = finish_with(
            &transfer,
            &dest,
            ReceivedFolder::Extract,
            t,
            &cancel,
            &mut |_, _| {
                // The leftover is in the record while the folder is unpacked.
                let files = transfer.files().unwrap();
                assert_eq!(files.leftovers.len(), 1);
                assert_eq!(files.leftovers[0].path, partial);
            },
        )
        .unwrap();
        assert_eq!(path, dest.join("Photos (2)"));
        assert_eq!(std::fs::read(path.join("a.txt")).unwrap(), b"alpha");
        assert!(partial.exists(), "still held when the transfer ended");
        drop(held);
        std::fs::remove_dir_all(&base).ok();
    }

    /// A link that was put into the earlier folder between the runs goes
    /// with that folder, and nothing is written, or deleted, where it
    /// pointed.
    #[test]
    fn nothing_goes_through_a_link_in_the_earlier_folder() {
        let base = scratch("junction");
        let dest = base.join("dest");
        let (t, _) = times(1_400_000_000);
        // An archive with a file in a sub folder.
        let src = base.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("x.dll"), b"from the peer").unwrap();
        let part = base.join("recv.zip");
        archive::zip_folder(&src, &part, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let partial = dest.join("Photos");
        std::fs::create_dir_all(&partial).unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(partial.join("sub"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Extract);
        was_unpacking_into(&transfer, &partial);
        let path = finish(&transfer, &dest, ReceivedFolder::Extract, t);
        assert_eq!(path, partial);
        assert!(!path.join("sub").is_symlink());
        assert_eq!(
            std::fs::read(path.join("sub").join("x.dll")).unwrap(),
            b"from the peer"
        );
        assert_eq!(names_in(&outside), ["keep.txt"]);
        std::fs::remove_dir_all(&base).ok();
    }
}
