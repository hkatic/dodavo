//! Receiving side of a transfer. The receiver never dials for data: after a
//! lost connection it waits for the sender to come back with `Continue`,
//! and only asks now and then (`Query`) whether the sender still has the
//! transfer at all.

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
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
    Accept, CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_OK, CLOSE_SUSPENDED, Event, Outcome, Phase,
    ReceivedFolder, Shared, Throttle,
    protocol::{self, CHUNK, Ended, Kind, Msg, Offer, Token},
    send::AbortOnDrop,
    session::{
        self, Control, Ending, HEAD_LEN, Head, KeptPart, Lost, Owned, PeerClose, ReceiveFiles,
        Role, Session, Side, Streams, TransferRecord, lost, off_runtime, peer_close,
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
/// How often a connection, or the last step, tries again to open the part
/// file while another program keeps it out (see `open_for_connection` and
/// `Transfer::open_part`), once it has tried for a moment without saying so
/// (`archive::HELD_RETRY_MS`): a virus scanner that reads the part after a
/// connection closed it lets go well before that.
const PART_BUSY_PAUSE: Duration = Duration::from_secs(1);
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
async fn query_answer(query: &mut Option<JoinHandle<Option<Ended>>>) -> Option<Ended> {
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
        Outcome::Failed(super::failure_text(&error))
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

    /// What arrived is on disk and in the record already: shutdown need not
    /// wait for what the connection still does.
    #[cfg(windows)]
    fn over(&self) {
        self.0.syncing.store(false, Ordering::Relaxed);
    }
}

impl Drop for Syncing {
    fn drop(&mut self) {
        self.0.syncing.store(false, Ordering::Relaxed);
    }
}

/// What holds a connected receive up besides the user's pause.
#[derive(Default)]
struct HeldUp {
    /// The sender paused the transfer.
    peer_paused: bool,
    /// Another program keeps the part file out (see `open_for_connection`).
    part_in_use: bool,
}

impl HeldUp {
    /// The status while connected, where `paused` is the user's pause.
    fn phase(&self, paused: bool) -> Phase {
        if paused {
            Phase::Paused
        } else if self.part_in_use {
            Phase::PartInUse { finishing: false }
        } else if self.peer_paused {
            Phase::PausedByPeer
        } else {
            Phase::Transferring
        }
    }
}

/// The status of a receive, which two tasks give while it is connected:
/// the transfer's own, and the one that tells the sender about the user's
/// pauses (`control_writer`). Each change of what the status depends on is
/// followed by working it out and giving it under one lock, so that the
/// status given last is the one that holds, whichever task changed what
/// last.
struct Status {
    shared: Arc<Shared>,
    id: u64,
    /// The user's pause.
    paused: watch::Receiver<bool>,
    state: Mutex<StatusState>,
}

#[derive(Default)]
struct StatusState {
    held: HeldUp,
    /// Receiving on a connection: the user's pause and `held` give the
    /// status. Otherwise the transfer's task gives it as it goes (waiting
    /// for a connection, checking the data, the last step), and a pause
    /// changes nothing in it.
    pauses_decide: bool,
}

impl Status {
    fn new(shared: &Arc<Shared>, id: u64, paused: watch::Receiver<bool>) -> Arc<Self> {
        Arc::new(Status {
            shared: shared.clone(),
            id,
            paused,
            state: Mutex::default(),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, StatusState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, phase: Phase) {
        self.shared.emit(Event::Phase { id: self.id, phase });
    }

    /// Gives the status that the pauses decide, under the lock `state`.
    fn emit_connected(&self, state: &StatusState) {
        self.emit(state.held.phase(*self.paused.borrow()));
    }

    /// Changes what holds the transfer up with `change`, and gives the
    /// status if the pauses decide it; with a `change` that changes nothing
    /// after a pause or a resume by the user.
    fn change(&self, change: impl FnOnce(&mut HeldUp)) {
        let mut state = self.state();
        change(&mut state.held);
        if state.pauses_decide {
            self.emit_connected(&state);
        }
    }

    /// From now on the pauses decide the status; gives it.
    fn connected(&self) {
        let mut state = self.state();
        state.pauses_decide = true;
        self.emit_connected(&state);
    }

    /// A new connection: the sender has not paused on it, and the transfer's
    /// task gives the status until the part file is open (see `connected`).
    fn new_connection(&self) {
        let mut state = self.state();
        state.held = HeldUp::default();
        state.pauses_decide = false;
    }

    /// Another program keeps the part file out while a connection opens it
    /// (`in_use`), and the status says so, or follows the user's pause; or
    /// the part is open now, and the transfer's task gives the status (see
    /// `connected`).
    fn part_in_use(&self, in_use: bool) {
        let mut state = self.state();
        state.held.part_in_use = in_use;
        state.pauses_decide = in_use;
        if in_use {
            self.emit_connected(&state);
        }
    }

    /// Gives `phase`, which the pauses do not decide (until `connected`).
    fn set(&self, phase: Phase) {
        let mut state = self.state();
        state.pauses_decide = false;
        self.emit(phase);
    }

    /// The pauses no longer decide the status, which stays as it is: all of
    /// the data has arrived.
    fn hold(&self) {
        self.state().pauses_decide = false;
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

    let task = {
        let (shared, endpoint) = (shared.clone(), endpoint.clone());
        async move {
            accept(
                &shared, &endpoint, id, &ctl, conn, send, recv, offer, from, dest_dir, folder,
            )
            .await
        }
    };
    let outcome = super::guarded(&shared, id, task).await;
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
    let restored = checked.and_then(|mut checked| {
        let placed = checked.placed.take();
        Receiver::restore(shared, endpoint, id, checked, &ctl).map(|r| (r, placed))
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

/// A receive from an earlier run, checked (see `check_restored`).
struct Checked {
    record: TransferRecord,
    peer: EndpointId,
    /// The received item, if it is in place already (see
    /// `TransferRecord::placed_item`).
    placed: Option<PathBuf>,
    /// The part file could not be cut back yet (see `Receiver::cut`).
    cut: bool,
}

/// Checks a receive from an earlier run before it goes on: the sender's
/// Contact ID, and the part file, which is cut back to what is known to be
/// on disk, or later, when another program has it open now (a virus
/// scanner or a backup program after a crash, say): then the first
/// connection cuts it, waiting for that program as it does for its own open
/// (see `Receiver::cut`). What fails ends the transfer. Blocking.
fn check_restored(record: TransferRecord, data_dir: &Path) -> Result<Checked, Outcome> {
    let Some(peer) = record.peer_id() else {
        record.discard(data_dir);
        return Err(Outcome::Failed(tr!(error_bad_resume).into()));
    };
    // Data beyond the last flush may never have reached the disk.
    let cut = match record.truncate_to_synced() {
        Ok(()) => false,
        Err(e) if archive::is_busy(&e) => true,
        Err(e) => {
            record.discard(data_dir);
            return Err(Outcome::Failed(super::failure_text(&e.into())));
        }
    };
    let placed = record.placed_item().map(Path::to_path_buf);
    Ok(Checked {
        record,
        peer,
        placed,
        cut,
    })
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
    /// `hasher` is of the part on disk, and no data was written since.
    #[cfg(unix)]
    hashed_from_disk: bool,
    /// The sender's hash of the whole item, once `Done` arrived.
    done_hash: Option<[u8; 32]>,
    /// The first bytes of the part, at most `HEAD_LEN` of them, while they
    /// arrive in this run: what the record notes of how the part begins
    /// (see `session::Head`). `None` for a receive that was picked up with
    /// data from an earlier run, and once all of them are noted.
    head: Option<Vec<u8>>,
    status: Arc<Status>,
    /// The last connection was closed by the sender on purpose (it is
    /// exiting): the wait for the next one is not a network problem.
    peer_suspended: bool,
    /// After a restart: the part file is still to be cut back to what is
    /// known to be on disk (see `TransferRecord::truncate_to_synced`), since
    /// another program had it open then. The next connection does it before
    /// it opens the part (see `open_own_part`), and so it waits for that
    /// program as it does for its own open; only then is new data appended.
    cut: bool,
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
        let (dir, shared, data_dir) = (dest_dir.clone(), shared.clone(), shared.data_dir.clone());
        let (name, offer, paused, first) = (
            name.clone(),
            offer.clone(),
            ctl.paused.subscribe(),
            first.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let temp_dir = shared.temp_dir_now();
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
            return Outcome::Failed(super::failure_text(&e));
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
        #[cfg(unix)]
        hashed_from_disk: false,
        done_hash: None,
        head: Some(Vec::new()),
        status: Status::new(shared, id, ctl.paused.subscribe()),
        peer_suspended: false,
        cut: false,
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
    let (part, part_id) = prepare_part(
        dest_dir,
        temp_dir,
        data_dir,
        name,
        &offer,
        folder,
    )?;
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
            synced: Some(0),
            folder,
            head: None,
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

/// What a new file or folder of a receive in `dir` must not be named, even
/// when nothing has the name now: where identities are weak (see
/// `FileId::weak`), what the records other than `except`'s name (see
/// `session::held_names`); elsewhere nothing.
fn held_in(dir: &Path, data_dir: &Path, except: Option<&Token>) -> HashSet<PathBuf> {
    if archive::file_id(dir).is_ok_and(|id| id.weak()) {
        session::held_names(data_dir, except)
    } else {
        HashSet::new()
    }
}

/// Creates the part file of a new receive, without replacing anything, and
/// returns it with its identity. Blocking.
#[allow(clippy::too_many_arguments)]
fn prepare_part(
    dest_dir: &Path,
    temp_dir: &Path,
    data_dir: &Path,
    name: &str,
    offer: &Offer,
    folder: ReceivedFolder,
) -> anyhow::Result<(PathBuf, FileId)> {
    std::fs::create_dir_all(dest_dir)?;
    // Everything arrives in the destination folder, on the drive that the
    // user chose for it: also the zip of a folder that is only unpacked,
    // which is deleted at the end. Only where that drive cannot hold the
    // zip does it arrive in the temp folder.
    let part = if session::part_in_temp(offer.kind, folder, offer.size, dest_dir) {
        std::fs::create_dir_all(temp_dir)?;
        let zip = session::temp_archive(temp_dir, Role::Receive, &offer.token);
        std::fs::File::create(&zip)?;
        zip
    } else {
        let part_name = match offer.kind {
            Kind::File => format!("{name}.part"),
            Kind::Folder => format!("{name}.zip.part"),
        };
        // Created here and not over anything, also when another transfer of
        // the same name starts at the same moment.
        let held = held_in(dest_dir, data_dir, None);
        let part = archive::create_unique_file_where(dest_dir, &part_name, |candidate| {
            !held.contains(&dest_dir.join(candidate))
        })?;
        // Marked from the start: the mark stays with the file when it is
        // renamed into place, so it is never there unmarked.
        let _ = archive::mark_of_the_web(&part);
        part
    };
    // With its own creation time, not that of a file of the same name
    // deleted moments before, which Windows may give it: on FAT and exFAT
    // the time tells the part from such a file (see `FileId`).
    match archive::created_now(&part) {
        Ok(id) => Ok((part, id)),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e.into())
        }
    }
}

/// Opens the part file of a running receive to append to it, which happens
/// by its name for each connection: only while it is still the transfer's,
/// as told by the handle that is written to (see
/// `TransferRecord::open_part`), not once it has been deleted and another
/// file has taken the name while the transfer waited. It is open for the
/// connection while the result's `KeptPart` exists, and meanwhile no other
/// program can write to it, delete it or rename it, so it stays the part
/// until the connection is over; the user who wants it gone cancels the
/// transfer, which deletes it through the handle (see `Session::keep_part`).
/// While another program has it open in a way that keeps it out (see
/// `archive::is_busy`), this tries again as `pauses` says, and then fails,
/// which the connection waits for (see `open_for_connection`). Without a
/// record the transfer is ending, and nothing is written. Blocking.
fn open_own_part(
    session: &Arc<Session>,
    pauses: &[u64],
) -> anyhow::Result<(std::fs::File, KeptPart)> {
    let (part, _) = open_checked(session, &archive::connection_options(), pauses)?;
    let kept = session.keep_part(&part)?;
    Ok((part, kept))
}

/// After a restart: cuts the part file back to what is known to be on disk
/// (see `Receiver::cut`), trying again while another program keeps it out,
/// pausing as `pauses` says (see `archive::while_busy`). Blocking.
fn cut_part(session: &Session, pauses: &[u64]) -> io::Result<()> {
    let record = session
        .with_record(TransferRecord::clone)
        .ok_or_else(|| io::Error::other(Cancelled))?;
    archive::while_busy(pauses, || record.truncate_to_synced())
}

/// Opens the part file to hash it (see `Receiver::rehash`). The handle that
/// the connection writes to keeps it the part on Windows.
#[cfg(windows)]
fn open_to_hash(_session: &Session, path: &Path, pauses: &[u64]) -> io::Result<std::fs::File> {
    archive::while_busy(pauses, || std::fs::File::open(path))
}

/// Opens the part file to hash it (see `Receiver::rehash`): only while it is
/// still the part, as told by the handle that is read, since no connection
/// keeps others from replacing it on POSIX.
#[cfg(unix)]
fn open_to_hash(session: &Session, _path: &Path, pauses: &[u64]) -> io::Result<std::fs::File> {
    open_checked(session, &archive::last_step_options(false), pauses).map(|(part, _)| part)
}

/// Writes what arrived to the disk.
#[cfg(not(target_os = "macos"))]
async fn sync_part(out: &tokio::fs::File) -> io::Result<()> {
    out.sync_data().await
}

/// Writes what arrived to the disk. Rust asks for all of it to reach the
/// medium, which some file systems (SMB) refuse: then as far as they can do.
#[cfg(target_os = "macos")]
async fn sync_part(out: &tokio::fs::File) -> io::Result<()> {
    match out.sync_data().await {
        Err(e) if archive::full_sync_refused(&e) => {
            let file = out.try_clone().await?.into_std().await;
            off_runtime(move || archive::sync_as_far_as_possible(&file)).await
        }
        done => done,
    }
}

/// Opens the part file of the receive with `options` if it is still the
/// part (see `TransferRecord::open_part`), trying again while another
/// program keeps it out, pausing as `pauses` says (see
/// `archive::while_busy`). Returns it with the record it was checked
/// against. Fails with `Cancelled` once the transfer has ended. Blocking.
fn open_checked(
    session: &Session,
    options: &std::fs::OpenOptions,
    pauses: &[u64],
) -> io::Result<(std::fs::File, TransferRecord)> {
    let record = session
        .with_record(TransferRecord::clone)
        .ok_or_else(|| io::Error::other(Cancelled))?;
    let part = archive::while_busy(pauses, || record.open_part(options))?;
    Ok((part, record))
}

/// Asks the sender whether it still has the transfer. How it ended there if
/// it says it is gone, `None` if it is still there or could not be reached.
async fn query_gone(endpoint: Endpoint, peer: EndpointId, token: Token) -> Option<Ended> {
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
            Ok(Msg::Gone { ended }) => Some(ended),
            _ => None,
        }
    };
    tokio::time::timeout(QUERY_TIMEOUT, ask)
        .await
        .unwrap_or(None)
}

/// Owns the sending half of the stream while receiving: tells the sender
/// about pauses and resumes by the user, and sends what the receive has to
/// say (that another program keeps the part out, `Verified`, then the
/// `Result`, which is the last message).
async fn control_writer(
    mut send: SendStream,
    mut paused_rx: watch::Receiver<bool>,
    mut msgs: mpsc::UnboundedReceiver<Msg>,
    shared: Arc<Shared>,
    session: Arc<Session>,
    status: Arc<Status>,
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
                status.change(|_| {});
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
        checked: Checked,
        ctl: &'a Control,
    ) -> Result<Self, Outcome> {
        let Checked {
            record, peer, cut, ..
        } = checked;
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
            #[cfg(unix)]
            hashed_from_disk: false,
            done_hash: record.done_hash,
            // What is on the disk from an earlier run is not known here.
            head: (record.have() == 0).then(Vec::new),
            status: Status::new(shared, id, ctl.paused.subscribe()),
            peer_suspended: false,
            cut,
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
            let id = self
                .session
                .with_record(TransferRecord::part_identity)
                .flatten();
            let ending = self.session.end_placed(&path);
            let (data_dir, item) = (self.shared.data_dir.clone(), path.clone());
            off_runtime(move || {
                // As in `finish_file`.
                let (now, later) = times.unwrap_or_default().split(id.as_ref());
                now.apply(&item);
                ending.forget_blocking(&data_dir);
                later.apply(&item);
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
            self.end(None, Ended::Failed).await;
            return Outcome::Failed(tr!(error_connection_lost).into());
        }
        self.drive(Some(streams)).await
    }

    /// Ends the transfer on this side (see `Session::end`) and deletes what
    /// goes, off the runtime's worker threads.
    async fn end(&self, code: Option<u32>, ended: Ended) {
        let ending = self.session.end(code, ended);
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

    /// The status while disconnected, after saying that there is no
    /// connection now (a paused transfer can be without one too).
    fn emit_waiting(&self) {
        self.shared.emit(Event::Path {
            id: self.id,
            direct: None,
        });
        self.status.set(if self.ctl.is_paused() {
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
        let mut query: Option<JoinHandle<Option<Ended>>> = None;
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
                    if let Some(ended) = answer {
                        if ended == Ended::Completed
                            && let Some(hash) = self.done_hash
                            && self.have == self.size
                        {
                            return Err(self.complete_alone(hash).await);
                        }
                        return Err(match ended {
                            Ended::Cancelled => {
                                self.end(None, Ended::Cancelled).await;
                                Outcome::CancelledByPeer
                            }
                            // Completed there without all of it here: the
                            // sender cannot know otherwise, but it cannot go on.
                            Ended::Completed | Ended::Failed => {
                                self.end(None, Ended::Failed).await;
                                Outcome::Failed(tr!(error_sender_failed).into())
                            }
                        });
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
            self.end(None, Ended::Failed).await;
            return Outcome::Failed(super::failure_text(&e));
        }
        if *self.hasher.as_ref().unwrap().finalize().as_bytes() != expected {
            self.end(None, Ended::Failed).await;
            return Outcome::Failed(tr!(error_damaged).into());
        }
        match self.finalize(None).await {
            Ok(path) => Outcome::Completed(Some(path)),
            Err(e) => {
                let outcome = failed_or_cancelled(e);
                self.end(None, outcome.ended()).await;
                outcome
            }
        }
    }

    /// Receives on one connection.
    async fn stream(&mut self, s: Streams) -> Result<PathBuf, End> {
        let Streams { conn, send, recv } = s;
        self.session.set_current(Some(conn.clone()));
        let _watch = AbortOnDrop(spawn_path_watch(self.shared.clone(), self.id, conn.clone()));
        self.status.new_connection();
        self.peer_suspended = false;
        let (msgs_tx, msgs_rx) = mpsc::unbounded_channel();
        let writer = tokio::spawn(control_writer(
            send,
            self.ctl.paused.subscribe(),
            msgs_rx,
            self.shared.clone(),
            self.session.clone(),
            self.status.clone(),
        ));

        let result = self.pump(&conn, recv, msgs_tx).await;
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
                        self.end(None, Ended::Cancelled).await;
                        Err(End::Final(Outcome::CancelledByPeer))
                    }
                    PeerClose::Failed => {
                        self.end(None, Ended::Failed).await;
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
                self.end(None, Ended::Failed).await;
                Err(End::Final(Outcome::Failed(super::failure_text(&e))))
            }
            Err(e) => {
                writer.abort();
                let outcome = failed_or_cancelled(e);
                let code = match outcome {
                    Outcome::Cancelled => CLOSE_CANCELLED,
                    _ => CLOSE_FAILED,
                };
                self.end(Some(code), outcome.ended()).await;
                Err(End::Final(outcome))
            }
        }
    }

    /// Fails once the part is no longer where the record says. Nothing keeps
    /// others from moving or deleting it on macOS during a connection, and
    /// writes to a file that is in the Trash would be lost: better to know
    /// now than after the rest of the data.
    #[cfg(unix)]
    async fn check_part(&self) -> anyhow::Result<()> {
        let session = self.session.clone();
        let there = off_runtime(move || {
            session
                .with_record(TransferRecord::clone)
                .is_none_or(|record| record.part_is_there())
        })
        .await;
        anyhow::ensure!(there, session::part_missing());
        Ok(())
    }

    /// After a restart: the hash of the part file as it is on disk.
    async fn rehash(&mut self) -> anyhow::Result<()> {
        if self.hasher.is_some() {
            return Ok(());
        }
        self.status.set(Phase::Verifying);
        if self.have == 0 {
            self.hasher = Some(blake3::Hasher::new());
            return Ok(());
        }
        // Another program may have the part open in a way that keeps this
        // run out (a virus scanner or a backup program after a crash, say):
        // all that arrived is kept, and the check waits for it as long as
        // that lasts, as a connection and the last step do (see
        // `open_for_connection`), saying so after a moment.
        let mut pauses = archive::HELD_RETRY_MS;
        let mut waiting = false;
        let mut file = loop {
            let (session, path) = (self.session.clone(), self.part_path.clone());
            match off_runtime(move || open_to_hash(&session, &path, pauses)).await {
                Err(e) if archive::is_busy(&e) => {
                    if !std::mem::replace(&mut waiting, true) {
                        self.status.set(Phase::PartInUse {
                            finishing: self.have == self.size,
                        });
                    }
                    pauses = &[];
                    tokio::time::sleep(PART_BUSY_PAUSE).await;
                }
                opened => break opened?,
            }
        };
        if waiting {
            self.status.set(Phase::Verifying);
        }
        let have = self.have;
        // More than arrived means that another program has written to it,
        // and the part cannot be trusted (see `archive::macos`). On Windows
        // the handle that writes keeps others out.
        #[cfg(unix)]
        if file.metadata()?.len() != have {
            bail!(tr!(error_damaged));
        }
        let cancel = self.ctl.cancel.clone();
        let shared = self.shared.clone();
        let id = self.id;
        let hasher = tokio::task::spawn_blocking(move || {
            let mut hasher = blake3::Hasher::new();
            let mut throttle = Throttle::new();
            session::hash_open(
                &mut file,
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
        #[cfg(unix)]
        {
            self.hashed_from_disk = true;
        }
        Ok(())
    }

    /// Receives the rest of the data, checks it and puts the item in place,
    /// telling the sender through `msgs` (see `control_writer`).
    async fn pump(
        &mut self,
        conn: &Connection,
        mut recv: RecvStream,
        msgs: mpsc::UnboundedSender<Msg>,
    ) -> anyhow::Result<PathBuf> {
        // Opened before anything else: the part is checked on the handle
        // that is written to, and stays the part while it is hashed after a
        // restart (see `open_own_part`).
        let (out, kept) =
            open_for_connection(&self.session, &self.status, &msgs, conn.closed(), self.cut)
                .await?;
        self.cut = false;
        let mut out = tokio::fs::File::from_std(out);
        // The hash so far is of what arrived on earlier connections of this
        // run, and nothing kept other programs from writing to the part
        // while none of them had it open: what counts at the end is what is
        // on disk (see below).
        #[cfg(windows)]
        let carried = self.hasher.is_some() && self.have > 0;
        self.rehash().await?;
        self.status.connected();
        self.shared.emit(Event::Progress {
            id: self.id,
            done: self.have,
            total: self.size,
        });
        // Shutdown waits until what arrived is on disk and in the record.
        let syncing = Syncing::new(self.session.clone());
        let received = self.read_frames(&mut recv, &mut out).await;
        // Whatever happened, what was written has to be on disk before `have`
        // is trusted, also after a power cut.
        out.flush().await?;
        sync_part(&out).await?;
        // On Windows a connection's handle keeps other programs from writing
        // to the part, but between two connections nothing does: a part that
        // was written to before this connection is hashed again from the
        // disk, while this handle still holds it, so that what is checked
        // against the sender's hash is what the last step gets. (After a
        // restart the hash is of the disk anyway, see `rehash`.)
        #[cfg(windows)]
        let on_disk = if carried && received.is_ok() {
            // What arrived is noted first: the check can take a while, and
            // the program may be closed meanwhile.
            self.save_synced().await;
            syncing.over();
            self.hasher = None;
            self.rehash().await
        } else {
            Ok(())
        };
        drop(out);
        drop(kept);
        self.save_synced().await;
        drop(syncing);
        let hash = received?;
        #[cfg(windows)]
        on_disk?;
        #[cfg(test)]
        while held_before_verifying(&self.name) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // On macOS nothing keeps other programs from writing to the part
        // during a connection (see `archive::macos`), and the hash so far is
        // of what arrived: what counts is what is on disk.
        #[cfg(unix)]
        if !self.hashed_from_disk {
            self.hasher = None;
            self.rehash().await?;
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
        let placed = self.finalize(Some(&msgs)).await?;
        let _ = msgs.send(Msg::Result {
            ok: true,
            message: String::new(),
        });
        Ok(placed)
    }

    /// Notes in the record how much of the part file is on disk, and what
    /// the part begins with as far as that is on disk now (see
    /// `session::Head`).
    async fn save_synced(&mut self) {
        let have = self.have;
        let begins = self.head.as_ref().filter(|head| !head.is_empty()).map(|head| {
            let on_disk = head.len().min(usize::try_from(have).unwrap_or(usize::MAX));
            Head::of(&head[..on_disk])
        });
        // All that counts of it is noted: the bytes are not needed again.
        if self.head.as_ref().is_some_and(|head| head.len() >= HEAD_LEN) {
            self.head = None;
        }
        self.session
            .update(&self.shared.data_dir, move |s, d| s.save_synced(d, have, begins))
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
                    if let Some(head) = self.head.as_mut().filter(|head| head.len() < HEAD_LEN) {
                        let more = len.min(HEAD_LEN - head.len());
                        head.extend_from_slice(&buf[..more]);
                    }
                    #[cfg(unix)]
                    {
                        self.hashed_from_disk = false;
                    }
                    self.have += len as u64;
                    unsynced += len as u64;
                    if unsynced >= SYNC_EVERY {
                        sync_part(out).await?;
                        self.save_synced().await;
                        unsynced = 0;
                        #[cfg(unix)]
                        self.check_part().await?;
                    }
                    if throttle.ready() || self.have == self.size {
                        self.shared.emit(Event::Progress {
                            id: self.id,
                            done: self.have,
                            total: self.size,
                        });
                    }
                }
                Msg::Paused => self.status.change(|held| held.peer_paused = true),
                Msg::Resumed => self.status.change(|held| held.peer_paused = false),
                Msg::Done { hash } => {
                    anyhow::ensure!(self.have == self.size, tr!(error_protocol_end));
                    self.status.hold();
                    return Ok(hash);
                }
                other => bail!(tr!(error_unexpected_answer, answer = format!("{other:?}"))),
            }
        }
    }

    /// The size of the item as its offer stated it (see `limit::size_of`).
    fn offered(&self) -> u64 {
        self.session
            .with_record(|r| crate::limit::size_of(&r.offer))
            .unwrap_or(0)
    }

    /// The most that the folder may unpack to: what the offer said or the
    /// sender's limit as it is now, whichever is more, so that a sender whose
    /// program states less than it packs gets nothing past the limit. A
    /// folder can be a little larger than its offer said (a file that grew
    /// while it was packed), which a limit leaves room for unless the item
    /// was accepted over it. `None` without a limit: then the folder is
    /// held to what its zip states, which has to fit on the disk where it
    /// is more than the offer said (see `archive::unzip_within`).
    fn unpack_max(&self) -> Option<u64> {
        let limit = self.shared.limits.lock().unwrap().limit_for(&self.peer)?;
        Some(limit.max(self.offered()))
    }

    /// Moves the file into place, or unpacks the folder and keeps its zip
    /// as chosen, telling the sender through `msgs` while connected (see
    /// `control_writer`). Once it succeeds, the transfer has ended as
    /// completed.
    async fn finalize(
        &mut self,
        msgs: Option<&mpsc::UnboundedSender<Msg>>,
    ) -> anyhow::Result<PathBuf> {
        // The status of the step, also once it no longer waits for another
        // program that keeps the part out (see `Transfer::open_part`).
        let step = if self.unpacks() {
            self.status.set(Phase::Unpacking);
            #[cfg(test)]
            while HOLD_FINALIZE.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Phase::Unpacking
        } else {
            self.status.hold();
            Phase::Transferring
        };
        let shared = self.shared.clone();
        let id = self.id;
        let cancel = self.ctl.cancel.clone();
        let (kind, folder, times) = (self.kind, self.folder, self.times);
        let (dest, name) = (self.dest_dir.clone(), self.name.clone());
        let (status, size, msgs) = (self.status.clone(), self.size, msgs.cloned());
        let transfer = Transfer {
            session: self.session.clone(),
            data_dir: shared.data_dir.clone(),
            unpack_max: self.unpack_max(),
            offered: self.offered(),
            waits: Box::new(move |in_use| {
                if in_use {
                    status.set(Phase::PartInUse { finishing: true });
                    // All of it has arrived.
                    status.shared.emit(Event::Progress {
                        id,
                        done: size,
                        total: size,
                    });
                } else {
                    status.set(step);
                }
                if let Some(msgs) = &msgs {
                    let _ = msgs.send(Msg::PartInUse { in_use });
                }
            }),
        };
        // A panic in the step goes on in this task rather than turning into
        // a failure, so that one after the item is in place leaves the
        // transfer completed (see `net::guarded`).
        off_runtime(move || {
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
        .await
    }
}

/// Opens the part file for a connection (see `open_own_part`). While another
/// program keeps it out, such as a virus scanner, a sync client or a
/// program in which the user opened it, this tries again for as long as
/// that lasts, and after a moment the status says so (`Phase::PartInUse`,
/// or the user's pause, see `Status::part_in_use`) and the sender is told
/// through `msgs` (see `control_writer`): nothing is lost, and the sender
/// meanwhile waits for the data it sent to be read, so nothing else goes
/// over the network. The wait ends once the connection is `closed`, as lost
/// (`Lost`, which `stream` tells apart by how it ended: the sender
/// cancelled or exited, say). With `cut`, the part is first cut back to
/// what is known to be on disk (see `Receiver::cut`), with the same wait.
async fn open_for_connection(
    session: &Arc<Session>,
    status: &Status,
    msgs: &mpsc::UnboundedSender<Msg>,
    closed: impl Future,
    cut: bool,
) -> anyhow::Result<(std::fs::File, KeptPart)> {
    tokio::pin!(closed);
    let mut pauses = archive::HELD_RETRY_MS;
    let mut waiting = false;
    let opened = loop {
        let session = session.clone();
        let attempt = off_runtime(move || {
            // Once it is cut, cutting again does nothing (see
            // `TransferRecord::truncate_to_synced`). After a cut, the part
            // opens at once or the wait below says why it does not: one
            // round of trying again in silence is enough.
            if cut {
                cut_part(&session, pauses)?;
            }
            open_own_part(&session, if cut { &[] } else { pauses })
        })
        .await;
        match attempt {
            Err(e) if e.downcast_ref::<io::Error>().is_some_and(archive::is_busy) => {
                if !std::mem::replace(&mut waiting, true) {
                    status.part_in_use(true);
                    let _ = msgs.send(Msg::PartInUse { in_use: true });
                }
                pauses = &[];
                tokio::select! {
                    () = tokio::time::sleep(PART_BUSY_PAUSE) => {}
                    _ = &mut closed => break Err(lost(e)),
                }
            }
            opened => break opened,
        }
    };
    if waiting {
        // The caller gives the status once the part is open.
        status.part_in_use(false);
        if opened.is_ok() {
            let _ = msgs.send(Msg::PartInUse { in_use: false });
        }
    }
    opened
}

/// A running receive as its last step sees it: the session, the data folder
/// for its record, and what to do when the step waits for another program
/// that keeps the part out (`true`), and when it goes on (see
/// [`open_part`](Self::open_part)).
struct Transfer {
    session: Arc<Session>,
    data_dir: PathBuf,
    /// The most that a folder may unpack to (see `unpack_max`).
    unpack_max: Option<u64>,
    /// The size of the item as its offer stated it (see `Receiver::offered`).
    offered: u64,
    waits: Box<dyn Fn(bool) + Send>,
}

impl Transfer {
    fn files(&self) -> anyhow::Result<ReceiveFiles> {
        self.session.receive_files().ok_or_else(|| Cancelled.into())
    }

    /// Whether the transfer has ended or is to end: the user cancelled
    /// (see `Session::cancel`).
    fn to_end(&self) -> bool {
        self.session.with_record(|r| r.ended).unwrap_or(true)
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

    /// Opens the part file for the last step, if it is still the part (see
    /// `TransferRecord::open_part`): nothing holds it open between the
    /// connection and this step, so it may have been deleted and its name
    /// taken by another file, which is not this transfer's. Returns it with
    /// its identity: as the record knows it, or as it is now in a record of
    /// an earlier version, which trusts the name. While it is open, other
    /// programs can only read it, so what the step renames or unpacks is
    /// what was checked. With `renames`, the handle can also rename the part
    /// and give it its creation time back (see `moved`); otherwise it only
    /// reads.
    ///
    /// Another program keeps the step out while it has the part open for
    /// writing, since what was checked may be changing, and with `renames`
    /// also while it has it open without letting it be deleted, as with a
    /// rename by name. All of the item has arrived, so the step waits for
    /// it as long as that lasts, as a connection does (see
    /// `open_for_connection`): after a moment it says so (`waits`), and once
    /// the part is open, that it goes on. A cancel ends the wait (with
    /// `Cancelled`).
    fn open_part(&self, renames: bool) -> io::Result<(std::fs::File, FileId)> {
        let options = archive::last_step_options(renames);
        let mut pauses = archive::HELD_RETRY_MS;
        let mut waiting = false;
        let opened = loop {
            match open_checked(&self.session, &options, pauses) {
                Err(e) if archive::is_busy(&e) => {
                    if self.to_end() {
                        break Err(io::Error::other(Cancelled));
                    }
                    if !std::mem::replace(&mut waiting, true) {
                        (self.waits)(true);
                    }
                    pauses = &[];
                    std::thread::sleep(PART_BUSY_PAUSE);
                }
                opened => break opened,
            }
        };
        if waiting && opened.is_ok() {
            (self.waits)(false);
        }
        let (part, record) = opened?;
        let id = match record.part_identity() {
            Some(id) => id,
            None => FileId::of(&part)?,
        };
        Ok((part, id))
    }

    /// Renames the part file `part`, whose identity is `id` (see
    /// [`open_part`](Self::open_part)), to `dir\name`, or to the next free
    /// name, and returns where it went. Each name is noted in the record
    /// before it is tried (`Side::Receive::moving`), so that a crash in
    /// between never loses the part; once the transfer is to end, nothing is
    /// tried.
    fn move_part(
        &self,
        files: &mut ReceiveFiles,
        part: &std::fs::File,
        id: FileId,
        dir: &Path,
        name: &str,
    ) -> io::Result<PathBuf> {
        let moved = archive::move_open_to_unique_noted(part, dir, name, &mut |to| {
            files.moving = Some(Owned {
                path: to.to_path_buf(),
                id,
            });
            self.save(files)
        });
        match moved {
            Ok(to) => {
                self.moved(files, part, id, to.clone());
                Ok(to)
            }
            Err(e) => {
                self.unnote_move(files);
                Err(e)
            }
        }
    }

    /// Notes that the part file `part`, whose identity is `id`, has been
    /// renamed to `to`, once it has its creation time back where the rename
    /// may have given it another's (see `archive::keep_creation_time`); if
    /// that cannot be done, with the identity it has now. Best effort: the
    /// note of the rename (`Side::Receive::moving`) finds the part after a
    /// crash. Only a power cut right between the rename and giving the time
    /// back, on FAT or exFAT, leaves a part that a later start cannot tell:
    /// the item stays in place, and the transfer is reported as failed.
    fn moved(&self, files: &mut ReceiveFiles, part: &std::fs::File, id: FileId, to: PathBuf) {
        files.part = to;
        let now = archive::keep_creation_time(part, &id)
            .err()
            .and_then(|_| FileId::of(part).ok());
        let _ = match now {
            Some(now) => self.session.save_moved_part(&self.data_dir, files, now),
            None => self.save(files),
        };
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
        let owned = match Owned::made_at(folder) {
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
///
/// The dates are given before the transfer ends, but for a creation time
/// that waits until its record is gone: on FAT and exFAT, the part's own
/// creation time tells it from other files until then (see `FileId` and
/// `ItemTimes::split`). After a crash before the record is gone, a later
/// start gives them (see `Receiver::continue_restored`); one right after it
/// leaves the item with its own creation time.
fn finish_file(
    transfer: &Transfer,
    dest: &Path,
    name: &str,
    times: ItemTimes,
) -> anyhow::Result<PathBuf> {
    let mut files = transfer.files()?;
    let (part, id) = transfer.open_part(true)?;
    let (now, later) = times.split(Some(&id));
    let path = transfer.place(&mut || {
        let path = transfer.move_part(&mut files, &part, id, dest, name)?;
        now.apply_to(&part);
        Ok(path)
    })?;
    drop(part);
    later.apply(&path);
    Ok(path)
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
    let both = folder == ReceivedFolder::Both;
    // A zip that is kept is renamed; one that is only unpacked, or that has
    // its name already (after a restart), is only read.
    let renames = folder == ReceivedFolder::KeepZip || (both && !files.part_in_place());
    let (part, id) = transfer.open_part(renames)?;
    if folder == ReceivedFolder::KeepZip {
        return transfer.place(&mut || {
            Ok(transfer.move_part(&mut files, &part, id, dest, &zip_name(name))?)
        });
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
    let target = if both && !files.part_in_place() {
        new_pair(transfer, &mut files, &part, id, dest, name)?
    } else {
        // With its zip in place already (after a restart), the folder gets
        // the zip's name.
        let stem = match files.part.file_stem() {
            Some(stem) if both => stem.to_string_lossy().into_owned(),
            _ => name.to_string(),
        };
        let held = held_in(dest, &transfer.data_dir, Some(&transfer.session.token));
        let target = archive::create_unique_dir(dest, &stem, |candidate| {
            !held.contains(&dest.join(candidate))
        })?;
        transfer.note_folder(&mut files, &target)?;
        target
    };
    let mut partial = Cleanup(Some(target.clone()));
    // From the part that was checked, which is closed after it.
    archive::unzip_within(
        part,
        &target,
        true,
        transfer.unpack_max,
        Some(transfer.offered),
        cancel,
        progress,
    )?;
    // As for a file (see `finish_file`), by the folder's identity.
    let folder_id = files.unpacking.as_ref().map(|folder| folder.id);
    let (now, later) = times.split(folder_id.as_ref());
    let placed = transfer.place(&mut || {
        now.apply(&target);
        Ok(target.clone())
    })?;
    partial.0 = None;
    later.apply(&target);
    Ok(placed)
}

/// For a folder that is kept with its zip: a new folder, and the part `part`
/// (whose identity is `id`) moved next to it under the zip's name, both with
/// the same number ("Photos (2)" and "Photos (2).zip"), before anything is
/// unpacked. Both are noted in the record first. Returns the folder.
fn new_pair(
    transfer: &Transfer,
    files: &mut ReceiveFiles,
    part: &std::fs::File,
    id: FileId,
    dest: &Path,
    name: &str,
) -> anyhow::Result<PathBuf> {
    let zip_of = |stem: &str| dest.join(zip_name(stem));
    let held = held_in(dest, &transfer.data_dir, Some(&transfer.session.token));
    let free = |stem: &str| {
        !held.contains(&dest.join(stem))
            && !held.contains(&zip_of(stem))
            && !archive::name_taken(&zip_of(stem))
    };
    loop {
        let target = archive::create_unique_dir(dest, name, free)?;
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
        // As in `Transfer::move_part`.
        match archive::rename_open(part, &zip) {
            Ok(()) => {
                transfer.moved(files, part, id, zip);
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
    /// on FAT and exFAT the time tells the part from such a file. The time
    /// is a whole multiple of the 10 ms that those file systems keep.
    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn a_new_part_is_created_now() {
        let base = scratch("created-now");
        let old = base.join("a.txt.part");
        std::fs::write(&old, b"old").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(400 * 24 * 60 * 60);
        archive::set_times(&old, None, Some(long_ago)).unwrap();
        std::fs::remove_file(&old).unwrap();
        let temp = base.join("temp");
        let (part, id) = prepare_part(
            &base,
            &temp,
            &base.join("data"),
            "a.txt",
            &offered(Kind::File, 5),
            ReceivedFolder::Extract,
        )
        .unwrap();
        assert_eq!(part, old);
        assert_eq!(id, archive::file_id(&part).unwrap());
        let meta = std::fs::metadata(&part).unwrap();
        let age = SystemTime::now()
            .duration_since(meta.created().unwrap())
            .unwrap();
        assert!(age < Duration::from_secs(10 * 60), "{age:?}");
        // A whole multiple of the 10 ms that FAT and exFAT keep.
        #[cfg(windows)]
        assert_eq!(archive::testing::created(&part) % 100_000, 0);
        std::fs::remove_dir_all(&base).ok();
    }

    /// An offer of `size` bytes, for a new part.
    fn offered(kind: Kind, size: u64) -> Offer {
        Offer {
            kind,
            name: "x".into(),
            size,
            file_count: 1,
            content_size: size,
            largest_file: 5,
            sender_name: "Ana".into(),
            token: [1; 16],
            modified: None,
            created: None,
        }
    }

    /// The zip of a folder arrives in the destination folder, whatever is
    /// kept of it: the drive that the user chose is the one that needs the
    /// room. Only a zip that the destination cannot hold (4 GiB or more on
    /// FAT32) arrives in the temporary folder.
    #[test]
    fn the_zip_of_a_folder_arrives_in_the_destination() {
        let base = scratch("zip-in-destination");
        let (dest, temp) = (base.join("dest"), base.join("temp"));
        let new_part = |folder, size| {
            prepare_part(
                &dest,
                &temp,
                &base.join("data"),
                "Photos",
                &offered(Kind::Folder, size),
                folder,
            )
            .unwrap()
            .0
        };
        for (n, folder) in ReceivedFolder::ALL.into_iter().enumerate() {
            let part = new_part(folder, 5);
            assert_eq!(part.parent(), Some(dest.as_path()), "{folder:?}");
            let name = part.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with("Photos.zip") && name.ends_with(".part"), "{name}");
            assert_eq!(names_in(&dest).len(), n + 1);
        }
        assert!(!temp.exists(), "nothing is in the temporary folder");

        let big = session::FAT32_MAX_FILE + 1;
        archive::PRETEND_FAT.set(true);
        let (small, large, kept) = (
            new_part(ReceivedFolder::Extract, session::FAT32_MAX_FILE),
            new_part(ReceivedFolder::Extract, big),
            new_part(ReceivedFolder::KeepZip, big),
        );
        archive::PRETEND_FAT.set(false);
        assert_eq!(small.parent(), Some(dest.as_path()));
        assert_eq!(large.parent(), Some(temp.as_path()));
        assert_eq!(kept.parent(), Some(dest.as_path()));
        std::fs::remove_dir_all(&base).ok();
    }

    /// Where identities are weak (Android's shared storage), a new part
    /// does not take a name that a record still names, even with nothing
    /// under it now: the record would take the new part for its own, as the
    /// file system may give it the number of the one that was deleted.
    /// Elsewhere the identity tells them apart, and the name is taken.
    #[test]
    fn a_new_part_leaves_the_names_of_records_alone() {
        let base = scratch("held-names");
        let dest = base.join("dest");
        let held = dest.join("a.txt.part");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(&held, b"old").unwrap();
        let _transfer = receiving(&base, &held, Kind::File, ReceivedFolder::Extract);
        std::fs::remove_file(&held).unwrap();
        let new_part = |weak: bool| {
            archive::PRETEND_WEAK.set(weak);
            let made = prepare_part(
                &dest,
                &base.join("temp"),
                &base.join("data"),
                "a.txt",
                &offered(Kind::File, 5),
                ReceivedFolder::Extract,
            );
            archive::PRETEND_WEAK.set(false);
            made.unwrap().0
        };
        let weak = new_part(true);
        assert_ne!(weak, held);
        assert_eq!(weak.parent(), Some(dest.as_path()));
        std::fs::remove_file(&weak).unwrap();
        if !archive::file_id(&dest).unwrap().weak() {
            assert_eq!(new_part(false), held);
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// The part is opened by its name for each connection, and written to
    /// only while it is still the transfer's, as told by the handle that is
    /// written to: not once the user has deleted it and another file has
    /// taken its name, on FAT and exFAT too. While it is open, no other
    /// program can delete, rename or write to it.
    #[test]
    fn only_its_own_part_is_written_to() {
        let base = scratch("still-there");
        if !archive::testing::tells_new_items_apart(&base) {
            return;
        }
        let part = base.join("dest").join("a.txt.part");
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        // macOS has no way of keeping others out (see `archive::macos`).
        #[cfg(windows)]
        {
            let open = open_own_part(&transfer.session, &[]).unwrap();
            assert!(std::fs::remove_file(&part).is_err());
            assert!(std::fs::rename(&part, base.join("elsewhere")).is_err());
            assert!(std::fs::OpenOptions::new().write(true).open(&part).is_err());
            assert_eq!(std::fs::read(&part).unwrap(), b"abc", "it can be read");
            drop(open);
        }
        std::fs::remove_file(&part).unwrap();
        archive::testing::next_tick();
        std::fs::write(&part, b"another").unwrap();
        let missing = open_own_part(&transfer.session, &[]).err().unwrap();
        assert_eq!(missing.to_string(), tr!(error_part_missing));
        assert_eq!(std::fs::read(&part).unwrap(), b"another");
        transfer
            .session
            .end(None, Ended::Cancelled)
            .forget_blocking(&transfer.data_dir);
        assert!(archive::is_cancelled(
            &open_own_part(&transfer.session, &[]).err().unwrap()
        ));
        assert_eq!(std::fs::read(&part).unwrap(), b"another");

        // On FAT and exFAT the creation time tells.
        #[cfg(any(windows, target_os = "macos"))]
        {
            archive::PRETEND_FAT.set(true);
            std::fs::remove_file(&part).unwrap();
            std::fs::write(&part, b"abc").unwrap();
            let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
            drop(open_own_part(&transfer.session, &[]).unwrap());
            let created = std::fs::metadata(&part).unwrap().created().unwrap();
            std::fs::remove_file(&part).unwrap();
            std::fs::write(&part, b"another").unwrap();
            archive::set_times(&part, None, Some(created + Duration::from_secs(2))).unwrap();
            assert!(open_own_part(&transfer.session, &[]).is_err());
            archive::PRETEND_FAT.set(false);
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// A cancel during a connection deletes the part also while the last
    /// write of the connection still holds it open (which lets nothing
    /// delete it by its name), however long that takes: it goes once the
    /// write is over, rather than at the next start.
    #[test]
    fn a_cancel_deletes_a_part_that_a_write_still_holds() {
        let base = scratch("held-part");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let (writing, kept) = open_own_part(&transfer.session, &[]).unwrap();
        assert!(transfer.session.cancel_blocking(&transfer.data_dir));
        assert!(session::records(&transfer.data_dir).is_empty());
        // The connection is gone, and its last write still holds the part.
        drop(kept);
        // On macOS, deleting frees the name at once.
        #[cfg(windows)]
        assert!(archive::name_taken(&part), "until the write is over");
        drop(writing);
        assert!(!archive::name_taken(&part));
        std::fs::remove_dir_all(&base).ok();
    }

    /// The end of a running transfer tries again for a moment to delete a
    /// part that another program holds open, such as a virus scanner that
    /// reads it; a start leaves it for the next one instead of waiting (see
    /// `session::remove_file`).
    #[test]
    fn the_end_of_a_transfer_waits_a_moment_for_a_held_part() {
        let base = scratch("held-at-end");
        let part = base.join("a.txt.part");
        let held = archive::testing::hold_readable;
        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let scanner = held(&part);
        let reading = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(scanner);
        });
        assert!(transfer.session.cancel_blocking(&transfer.data_dir));
        reading.join().unwrap();
        assert!(!part.exists());
        assert!(session::records(&transfer.data_dir).is_empty());

        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let record = transfer.session.with_record(TransferRecord::clone).unwrap();
        let scanner = held(&part);
        record.discard(&transfer.data_dir);
        drop(scanner);
        assert!(part.exists());
        let left = session::records(&transfer.data_dir);
        assert!(left.len() == 1 && left[0].ended, "for the next start");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A cancel that comes while a connection opens the part, before the
    /// connection keeps it (see `Session::keep_part`), deletes it too: the
    /// connection gives up as cancelled, and the part goes once it is closed.
    #[test]
    fn a_cancel_while_the_part_is_opened_deletes_it() {
        let base = scratch("opened-part");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let (opened, _) =
            open_checked(&transfer.session, &archive::connection_options(), &[]).unwrap();
        assert!(transfer.session.cancel_blocking(&transfer.data_dir));
        let e = transfer.session.keep_part(&opened).err().unwrap();
        assert!(archive::is_cancelled(&e.into()));
        drop(opened);
        assert!(!archive::name_taken(&part));
        std::fs::remove_dir_all(&base).ok();
    }

    /// Another program that has the part open for writing keeps a
    /// connection out, which then waits for it as long as that lasts (see
    /// `open_for_connection`), and nothing else; once it lets go, the part
    /// opens. Meanwhile the status says so, unless the user paused the
    /// transfer.
    #[test]
    fn a_part_held_elsewhere_keeps_a_connection_out() {
        let base = scratch("busy-part");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let other = archive::testing::hold_writing(&part);
        let e = open_own_part(&transfer.session, &[]).err().unwrap();
        assert!(
            e.downcast_ref::<io::Error>().is_some_and(archive::is_busy),
            "{e:#}"
        );
        drop(other);
        drop(open_own_part(&transfer.session, &[]).unwrap());
        assert_eq!(std::fs::read(&part).unwrap(), b"abc");
        let mut held = HeldUp {
            peer_paused: true,
            ..HeldUp::default()
        };
        assert_eq!(held.phase(false), Phase::PausedByPeer);
        held.part_in_use = true;
        assert_eq!(held.phase(false), Phase::PartInUse { finishing: false });
        assert_eq!(held.phase(true), Phase::Paused);
        std::fs::remove_dir_all(&base).ok();
    }

    /// After a crash, the part may be longer than what is known to be on
    /// disk, and another program may have it open when the program starts
    /// again (a virus scanner, a backup program). The transfer goes on: the
    /// part is cut back once a connection can open it, which waits for that
    /// program (see `Receiver::cut`); cutting again changes nothing.
    #[test]
    fn a_held_part_is_cut_back_when_it_can_be() {
        let base = scratch("cut-later");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"abcdefgh").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let mut record = transfer.session.with_record(TransferRecord::clone).unwrap();
        record.peer = iroh::SecretKey::generate().public().to_string();
        if let Side::Receive { synced, .. } = &mut record.side {
            *synced = Some(3);
        }
        transfer.session.set_record(record.clone());
        let held = archive::testing::hold_readable(&part);
        let Ok(checked) = check_restored(record, &transfer.data_dir) else {
            panic!("the restore failed");
        };
        assert!(checked.cut);
        assert_eq!(std::fs::read(&part).unwrap(), b"abcdefgh", "all of it kept");
        assert!(archive::is_busy(
            &cut_part(&transfer.session, &[]).unwrap_err()
        ));
        drop(held);
        cut_part(&transfer.session, &[]).unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"abc");
        cut_part(&transfer.session, &[]).unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"abc");
        std::fs::remove_dir_all(&base).ok();
    }

    /// The phases emitted so far.
    fn phases(events: &std::sync::Mutex<Vec<Event>>) -> Vec<Phase> {
        events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                Event::Phase { phase, .. } => Some(*phase),
                _ => None,
            })
            .collect()
    }

    /// While another program keeps the part out, a connection waits for it
    /// (see `open_for_connection`): after a moment the status says so, and
    /// follows the user's pause, and the sender is told. A connection that
    /// is closed meanwhile ends the wait as lost, and a pause then changes
    /// the status no more. Once the program lets go, the part opens and
    /// the sender is told that the transfer goes on; the status is then for
    /// the caller to give. A program that lets go in a moment, as a virus
    /// scanner does, is not told about.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_connection_waits_for_a_part_held_elsewhere() {
        let base = scratch("held-wait");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"abc").unwrap();
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let session = transfer.session.clone();
        let (shared, events) = super::super::tests::recording_shared(&base);
        let (paused, paused_rx) = watch::channel(false);
        let status = Status::new(&shared, 1, paused_rx);
        let (msgs, mut told) = mpsc::unbounded_channel();
        let wait_for_status = async || {
            while phases(&events).is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };

        let other = archive::testing::hold_writing(&part);
        let (close, closed) = oneshot::channel::<()>();
        let waiting = tokio::spawn({
            let (session, status, msgs) = (session.clone(), status.clone(), msgs.clone());
            async move {
                open_for_connection(&session, &status, &msgs, closed, false)
                    .await
                    .map(drop)
            }
        });
        wait_for_status().await;
        assert_eq!(told.recv().await, Some(Msg::PartInUse { in_use: true }));
        paused.send_replace(true);
        status.change(|_| {});
        paused.send_replace(false);
        status.change(|_| {});
        let in_use = Phase::PartInUse { finishing: false };
        assert_eq!(phases(&events), [in_use, Phase::Paused, in_use]);
        close.send(()).unwrap();
        let e = waiting.await.unwrap().unwrap_err();
        assert!(e.is::<Lost>(), "{e:#}");
        status.change(|_| {});
        assert_eq!(phases(&events).len(), 3, "the pauses decide nothing now");
        assert!(told.try_recv().is_err());

        events.lock().unwrap().clear();
        let waiting = tokio::spawn({
            let (session, status, msgs) = (session.clone(), status.clone(), msgs.clone());
            async move {
                open_for_connection(
                    &session,
                    &status,
                    &msgs,
                    std::future::pending::<()>(),
                    false,
                )
                .await
                .map(drop)
            }
        });
        wait_for_status().await;
        drop(other);
        waiting.await.unwrap().unwrap();
        assert_eq!(told.recv().await, Some(Msg::PartInUse { in_use: true }));
        assert_eq!(told.recv().await, Some(Msg::PartInUse { in_use: false }));
        status.change(|_| {});
        assert_eq!(phases(&events), [in_use], "the caller gives the status");

        events.lock().unwrap().clear();
        let scanner = std::fs::OpenOptions::new().write(true).open(&part).unwrap();
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(scanner);
        });
        open_for_connection(
            &session,
            &status,
            &msgs,
            std::future::pending::<()>(),
            false,
        )
        .await
        .unwrap();
        letting_go.join().unwrap();
        assert!(phases(&events).is_empty() && told.try_recv().is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    /// `transfer` with its last step's word that it waits for another
    /// program, or goes on, noted in the result (see `Transfer::open_part`).
    #[cfg(any(windows, target_os = "macos"))]
    fn noting_waits(transfer: &mut Transfer) -> Arc<std::sync::Mutex<Vec<bool>>> {
        let noted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let notes = noted.clone();
        transfer.waits = Box::new(move |in_use| notes.lock().unwrap().push(in_use));
        noted
    }

    /// Lets go of `other` once `noted` says that the last step waits for it.
    #[cfg(windows)]
    fn let_go_when_waited_for(
        noted: &Arc<std::sync::Mutex<Vec<bool>>>,
        other: std::fs::File,
    ) -> std::thread::JoinHandle<()> {
        let noted = noted.clone();
        std::thread::spawn(move || {
            while noted.lock().unwrap().is_empty() {
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(other);
        })
    }

    /// The last step of a folder that is only unpacked does not rename its
    /// part, so it only reads it: another program that reads it without
    /// letting it be deleted, such as a virus scanner, does not stop it.
    /// One that renames needs to delete, as a rename by name did. A program
    /// that has the part open for writing keeps either out, since what was
    /// checked may be changing: the step waits for it as long as that lasts,
    /// saying so after a moment and once it goes on, and nothing of what
    /// arrived is lost.
    #[test]
    // Linux has nothing that keeps a file to its readers.
    #[cfg(any(windows, target_os = "macos"))]
    fn a_part_that_is_only_unpacked_is_only_read() {
        let base = scratch("only-read");
        let part = received_zip(&base, "recv.zip");
        let dest = part.parent().unwrap().to_path_buf();
        let mut transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Extract);
        let noted = noting_waits(&mut transfer);
        let other = archive::testing::hold_readable(&part);
        let (t, _) = times(1_500_000_000);
        let folder = finish(&transfer, &dest, ReceivedFolder::Extract, t);
        assert_eq!(std::fs::read(folder.join("a.txt")).unwrap(), b"alpha");
        assert!(noted.lock().unwrap().is_empty());
        drop(other);
        std::fs::remove_dir_all(&base).ok();

        // A writer does not keep out one who reads on macOS, which checks the
        // part on disk instead (see `Receiver::pump`).
        #[cfg(windows)]
        {
            let base = scratch("only-read-writer");
            let part = received_zip(&base, "recv.zip");
            let dest = part.parent().unwrap().to_path_buf();
            let mut transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Extract);
            let noted = noting_waits(&mut transfer);
            let writer = std::fs::OpenOptions::new().write(true).open(&part).unwrap();
            let letting_go = let_go_when_waited_for(&noted, writer);
            let folder = finish(&transfer, &dest, ReceivedFolder::Extract, t);
            letting_go.join().unwrap();
            assert_eq!(std::fs::read(folder.join("a.txt")).unwrap(), b"alpha");
            assert_eq!(*noted.lock().unwrap(), [true, false]);
            std::fs::remove_dir_all(&base).ok();
        }
    }

    /// A cancel while the last step waits for another program that keeps
    /// the part out ends the wait, and the step as cancelled.
    #[cfg(windows)]
    #[test]
    fn a_cancel_ends_the_wait_of_the_last_step() {
        let base = scratch("wait-cancel");
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"abcde").unwrap();
        let mut transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        let noted = noting_waits(&mut transfer);
        let other = std::fs::OpenOptions::new().write(true).open(&part).unwrap();
        let cancelling = std::thread::spawn({
            let (noted, session) = (noted.clone(), transfer.session.clone());
            let data_dir = transfer.data_dir.clone();
            move || {
                while noted.lock().unwrap().is_empty() {
                    std::thread::sleep(Duration::from_millis(20));
                }
                assert!(!session.cancel_blocking(&data_dir), "left to the step");
            }
        });
        let (t, _) = times(1_500_000_000);
        let e = {
            let _finishing = transfer.session.finishing().unwrap();
            finish_file(&transfer, &base, "a.txt", t).unwrap_err()
        };
        cancelling.join().unwrap();
        assert!(archive::is_cancelled(&e), "{e:#}");
        assert_eq!(*noted.lock().unwrap(), [true], "it never went on");
        assert_eq!(std::fs::read(&part).unwrap(), b"abcde");
        drop(other);
        std::fs::remove_dir_all(&base).ok();
    }

    /// The last step renames the part only while it is still the part: in
    /// between the connection and the step nothing holds it open, so the user
    /// may have deleted it and another file may have taken its name. The step
    /// opens it first, and makes nothing for a part that is not there.
    #[test]
    fn only_its_own_part_is_put_in_place() {
        let base = scratch("own-placed");
        if !archive::testing::tells_new_items_apart(&base) {
            return;
        }
        let part = base.join("a.txt.part");
        std::fs::write(&part, b"new").unwrap();
        let (t, _) = times(1_500_000_000);
        let transfer = receiving(&base, &part, Kind::File, ReceivedFolder::Extract);
        std::fs::remove_file(&part).unwrap();
        archive::testing::next_tick();
        std::fs::write(&part, b"another").unwrap();
        let e = finish_file(&transfer, &base, "a.txt", t).unwrap_err();
        assert_eq!(e.to_string(), tr!(error_part_missing));
        assert_eq!(names_in(&base), ["a.txt.part", "data"]);
        assert_eq!(std::fs::read(&part).unwrap(), b"another");
        assert_eq!(transfer.files().unwrap().moving, None, "no note left");
        std::fs::remove_dir_all(&base).ok();

        // The same for a folder kept with its zip.
        let base = scratch("own-placed-both");
        let part = received_zip(&base, "Photos.zip.part");
        let dest = part.parent().unwrap().to_path_buf();
        let transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Both);
        let zip = std::fs::read(&part).unwrap();
        std::fs::remove_file(&part).unwrap();
        archive::testing::next_tick();
        std::fs::write(&part, &zip).unwrap();
        let cancel = AtomicBool::new(false);
        let e = finish_with(
            &transfer,
            &dest,
            ReceivedFolder::Both,
            t,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert_eq!(format!("{e:#}"), tr!(error_part_missing));
        // Checked before anything is made for it.
        assert_eq!(names_in(&dest), ["Photos.zip.part"]);
        assert_eq!(transfer.files().unwrap().moving, None, "no note left");
        transfer
            .session
            .end(None, Ended::Cancelled)
            .forget_blocking(&transfer.data_dir);
        assert_eq!(names_in(&dest), ["Photos.zip.part"]);
        assert_eq!(std::fs::read(&part).unwrap(), zip);
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
                largest_file: 5,
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
                synced: None,
                folder,
                head: None,
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
        Transfer {
            session,
            data_dir,
            unpack_max: None,
            // As the offer above states it.
            offered: 5,
            waits: Box::new(|_| {}),
        }
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
        assert_eq!(
            session::tombstone(&transfer.data_dir, &[7; 16]),
            Some(Ended::Completed)
        );
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
            largest_file: 5,
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
        let created = archive::testing::CREATION_TIMES.then(|| meta.created().unwrap());
        assert_eq!(
            (meta.modified().unwrap(), created),
            (at, archive::testing::kept(at))
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
        assert!(archive::testing::has_mark(&path.join("a.txt")));
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

    /// A folder that would unpack to more than it may fails, and its
    /// folder is not left behind.
    #[test]
    fn a_folder_larger_than_it_may_be_fails() {
        let base = scratch("too-large");
        let dest = base.join("dest");
        let (t, _) = times(1_400_000_000);
        let part = received_zip(&base, "recv.zip");
        let mut transfer = receiving(&base, &part, Kind::Folder, ReceivedFolder::Extract);
        // "alpha" is five bytes.
        transfer.unpack_max = Some(4);
        let cancel = AtomicBool::new(false);
        let error = finish_with(
            &transfer,
            &dest,
            ReceivedFolder::Extract,
            t,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert_eq!(error.to_string(), tr!(error_folder_too_large));
        assert!(!dest.join("Photos").exists());
        transfer.unpack_max = Some(5);
        let path = finish(&transfer, &dest, ReceivedFolder::Extract, t);
        assert_eq!(std::fs::read(path.join("a.txt")).unwrap(), b"alpha");
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
                .end(None, Ended::Cancelled)
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
                .end(None, Ended::Cancelled)
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
        if !archive::testing::tells_new_items_apart(&base) {
            return;
        }
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
        archive::testing::next_tick();
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
        let base = scratch("held");
        let dest = base.join("dest");
        let (t, _) = times(1_400_000_000);
        let part = received_zip(&base, "recv.zip");
        let partial = dest.join("Photos");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("a.txt"), b"al").unwrap();
        let held = archive::testing::hold(&partial.join("a.txt"));
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
        archive::testing::link_folder(&partial.join("sub"), &outside);
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
