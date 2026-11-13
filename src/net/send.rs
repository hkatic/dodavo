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
    protocol::{self, CHUNK, Ended, Kind, Msg, Offer, Token},
    session::{
        self, Control, Lost, PeerClose, Role, Session, Side, Snapshots, Streams, TransferRecord,
        lost, off_runtime, peer_close,
    },
    spawn_path_watch,
};
use crate::{archive, i18n::tr, source};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(40);
/// How often the record notes how much has gone out (see
/// `Side::Send::sent`), as the receiver notes what is on its disk.
pub(crate) const SAVE_SENT_EVERY: u64 = 64 * 1024 * 1024;
/// How long the receiver may take to answer `Continue`.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(15);

/// Deletes the temporary zip of a folder transfer when dropped (until the
/// transfer has a record, which then owns the zip), and then the note of a
/// zip that is not in the temporary folder (see `session::note_packing`).
struct TempFile(Option<PathBuf>, Option<PathBuf>);

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
        if let Some(note) = &self.1 {
            let _ = std::fs::remove_file(note);
        }
    }
}

/// Where the zip of a folder is packed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PackIn {
    /// In the temporary folder.
    Temp,
    /// In the folder that contains the folder being sent.
    Beside,
}

/// Where to pack a zip of about `needed` bytes: in the temporary folder,
/// unless its drive lacks the room while the folder being sent is on another
/// drive that has it (a large folder on an external drive, with a small
/// system drive). `temp` and `beside` are those drives with their free
/// space (see `platform::disk_free`), `None` where it cannot be told. With
/// room in neither, the temporary folder it is: a zip can come out smaller
/// than its files, and otherwise packing fails with the system's own words
/// for a full disk.
pub(crate) fn pack_in(
    needed: u64,
    temp: Option<&(PathBuf, u64)>,
    beside: Option<&(PathBuf, u64)>,
) -> PackIn {
    match (temp, beside) {
        (Some((temp_root, temp_free)), Some((root, free)))
            if *temp_free < needed
                && *free >= needed
                && !root.as_os_str().eq_ignore_ascii_case(temp_root.as_os_str()) =>
        {
            PackIn::Beside
        }
        _ => PackIn::Temp,
    }
}

/// Makes the empty zip that the folder at `source` is packed into, for the
/// send of `token`, and returns it with the note of one that is not in the
/// temporary folder (see [`pack_in`], `session::note_packing`). A folder
/// that takes no new file beside it (a read-only drive) is packed in the
/// temporary folder after all. Blocking.
fn new_zip(
    shared: &Shared,
    source: &std::path::Path,
    token: &Token,
) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
    let temp_dir = shared.temp_dir_now();
    let in_temp = || {
        std::fs::create_dir_all(&temp_dir)?;
        Ok((session::temp_archive(&temp_dir, Role::Send, token), None))
    };
    let Some(parent) = source.parent().filter(|_| !source::is_document(source)) else {
        return in_temp();
    };
    // The temporary folder may not be there yet: its drive is that of the
    // nearest folder above it that is.
    let temp = temp_dir.ancestors().find_map(crate::platform::disk_free);
    let beside = crate::platform::disk_free(parent);
    let needed = match (&temp, &beside) {
        // Only where it can matter: it reads the whole folder's listing.
        (Some((temp_root, _)), Some((root, _)))
            if !root.as_os_str().eq_ignore_ascii_case(temp_root.as_os_str()) =>
        {
            archive::folder_size(source).unwrap_or(0)
        }
        _ => 0,
    };
    if pack_in(needed, temp.as_ref(), beside.as_ref()) == PackIn::Temp {
        return in_temp();
    }
    let zip = session::temp_archive(parent, Role::Send, token);
    let Ok(note) = session::note_packing(&shared.data_dir, token, &zip) else {
        return in_temp();
    };
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&zip);
    if made.is_err() {
        let _ = std::fs::remove_file(&note);
        return in_temp();
    }
    Ok((zip, Some(note)))
}

/// Deletes the copy of a shared item when dropped (until the transfer has a
/// record, which then owns it; see `Side::Send::staging`).
struct Staged(Option<PathBuf>);

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(folder) = &self.0 {
            let _ = std::fs::remove_dir_all(folder);
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

/// What a send sends: a file or folder by its path, or a document that the
/// app describes (see [`source::Document`]), whose URI is then the path.
pub(super) struct What {
    pub path: PathBuf,
    pub kind: Kind,
    pub document: Option<source::Document>,
}

impl What {
    pub fn document(document: source::Document) -> Self {
        What {
            path: PathBuf::from(&document.uri),
            kind: if document.items.is_some() {
                Kind::Folder
            } else {
                Kind::File
            },
            document: Some(document),
        }
    }
}

/// A new transfer: prepares the data, makes the offer and streams it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    to: EndpointId,
    what: What,
    my_name: String,
    peer_name: String,
    ctl: Arc<Control>,
) -> Outcome {
    tokio::select! {
        outcome = start(shared, endpoint, id, to, what, my_name, peer_name, &ctl) => outcome,
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

/// Why a dial did not get a connection.
enum Undialed {
    /// The other side could not be reached, with the reason for the user.
    Unreachable(String),
    /// Its Dodavo does not take the transfers of this version (see
    /// `net::no_such_protocol`): trying again changes nothing until one of
    /// the two is updated.
    OtherVersion,
}

/// Dials the receiver of the transfer `id`. The log has why a dial failed,
/// once for a reason that repeats (see `logging::Repeats`).
async fn dial(
    endpoint: &Endpoint,
    to: EndpointId,
    id: u64,
    repeats: &mut crate::logging::Repeats,
) -> Result<Connection, Undialed> {
    let began = std::time::Instant::now();
    let result = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(to, protocol::ALPN)).await;
    let why = match &result {
        Ok(Ok(_)) => {
            repeats.clear();
            crate::log_info!("#{id} connected in {} ms", began.elapsed().as_millis());
            None
        }
        Ok(Err(e)) if super::no_such_protocol(e) => {
            Some("the receiver runs another version".into())
        }
        Ok(Err(e)) => Some(crate::logging::connect_error(e)),
        Err(_) => Some(format!("no connection in {} s", CONNECT_TIMEOUT.as_secs())),
    };
    if let Some(why) = why
        && let Some(left_out) = repeats.note(&why)
    {
        crate::log_info!(
            "#{id} dial failed{}: {why}",
            crate::logging::left_out(left_out)
        );
    }
    match result {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) if super::no_such_protocol(&e) => Err(Undialed::OtherVersion),
        Ok(Err(e)) => Err(Undialed::Unreachable(super::connect_error_text(&e))),
        Err(_) => Err(Undialed::Unreachable(tr!(error_timeout).into())),
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
    /// Another program on the receiving side keeps the partly received file
    /// out, so the receiver does not read what is sent (`Msg::PartInUse`).
    peer_part_in_use: bool,
    /// The last connection was closed by the receiver on purpose (it is
    /// exiting): the wait for the next one is not a network problem.
    peer_suspended: bool,
    /// The record's notes of how much has gone out (see `note_sent`).
    notes: SentNotes,
}

/// The record's notes of how much has gone out (see `Side::Send::sent`).
/// Each is written on a blocking thread, since it is synced to disk, so
/// that it does not hold up the data. A note that is still being written
/// when the send ends is not waited for: the transfer has ended as it did
/// (completed, or as the receiver says), which a panic in a note must not
/// turn into a failure, and the panic hook has told the user about it.
#[derive(Default)]
struct SentNotes {
    /// How much had gone out by the last note.
    noted: u64,
    /// The write of the last note, until it is waited for.
    writing: Option<tokio::task::JoinHandle<()>>,
}

impl SentNotes {
    /// Notes `done` with `write`, unless the last note is still being
    /// written: then this one is left out. A panic in the last one goes on
    /// here, so that the transfer fails (see `net::guarded`).
    async fn note(&mut self, done: u64, write: impl FnOnce() + Send + 'static) {
        if self.writing.as_ref().is_some_and(|w| !w.is_finished()) {
            return;
        }
        // It has finished, so this only takes its result, though it may
        // yield once (tokio's budget for a task can make any wait for
        // another task do that).
        self.wait().await;
        self.noted = done;
        self.writing = Some(tokio::task::spawn_blocking(write));
    }

    /// Waits until the last note is written. A panic in it goes on here, so
    /// that the transfer fails (see `net::guarded`).
    async fn wait(&mut self) {
        if let Some(mut writing) = self.writing.take() {
            session::joined(&mut writing).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    to: EndpointId,
    what: What,
    my_name: String,
    peer_name: String,
    ctl: &Control,
) -> Outcome {
    let What {
        path,
        kind,
        document,
    } = what;
    let token = protocol::new_token();
    crate::log_info!(
        "#{id} send token={} to {} kind={kind:?}{}",
        crate::logging::token(&token),
        crate::logging::peer(&to),
        if document.is_some() {
            ", a document"
        } else {
            ""
        }
    );
    let (session, _unused) = Session::new(id, token, to, Role::Send);
    shared.add_session(session.clone());
    let name = match &document {
        Some(document) if !document.name.is_empty() => document.name.clone(),
        _ => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr!(untitled).into()),
    };
    // Any receiver can take it as it is, and shows it as it is (see
    // `archive::names`).
    #[cfg(not(windows))]
    let name = archive::offer_name(&name);
    // The item's own dates, for the receiver to give to what it saves: a
    // document's as the app says.
    let times = match &document {
        Some(document) => archive::ItemTimes {
            modified: document
                .modified
                .and_then(source::millis_to_system)
                .and_then(archive::system_to_filetime),
            ..Default::default()
        },
        None => std::fs::metadata(&path)
            .map(|m| archive::ItemTimes::of(&m))
            .unwrap_or_default(),
    };
    let items = document.and_then(|d| d.items);
    // How errors name it: a document by its name, as the app shows it.
    let shown = if source::is_document(&path) {
        name.clone()
    } else {
        path.display().to_string()
    };

    // A copy of a shared item is the transfer's (see `Side::Send::staging`).
    let staging = session::staged_folder(&shared.temp_dir, &path);
    let mut staged = Staged(staging.clone());

    // 1. Prepare the data (zip folders). `size` is what gets streamed.
    let mut temp = TempFile(None, None);
    let what = Prepare {
        path: &path,
        shown: &shown,
        kind,
        items,
    };
    let prepared = prepare(shared, id, what, &token, ctl, &mut temp).await;
    let (data_path, packed, size) = match prepared {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(super::failed_text(id, "preparing", &e)),
    };
    crate::log_info!(
        "#{id} prepared: size={size} files={} content={} largest={}",
        packed.files,
        packed.size,
        packed.largest
    );
    // A folder's copy is in the zip now, which is the data from here on.
    if kind == Kind::Folder
        && let Some(folder) = staging.clone()
    {
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(folder)).await;
    }
    // What a later run compares to notice a changed source: the file, or the
    // zip the folder was packed into.
    let (source_len, source_mtime) = match source::metadata(&data_path) {
        Ok(m) => (m.len(), m.modified().ok()),
        Err(_) => (0, None),
    };

    // 2. Connect.
    shared.emit(Event::Phase {
        id,
        phase: Phase::Connecting,
    });
    let conn = match dial(endpoint, to, id, &mut crate::logging::Repeats::new()).await {
        Ok(conn) => conn,
        Err(Undialed::Unreachable(e)) => return Outcome::Unreachable(e),
        // Not something that being online or starting Dodavo there helps
        // with, which is what an unreachable recipient is told.
        Err(Undialed::OtherVersion) => {
            return Outcome::Failed(tr!(error_other_version).into());
        }
    };
    session.set_current(Some(conn.clone()));
    let path_watch = AbortOnDrop(spawn_path_watch(shared.clone(), id, conn.clone()));

    // 3. Offer and wait for the answer.
    let offer = Offer {
        kind,
        name,
        size,
        file_count: packed.files,
        content_size: packed.size,
        largest_file: packed.largest,
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
            other => {
                crate::log_warn!("#{id} the answer to the offer was {}", other.kind_name());
                bail!(tr!(error_unexpected_answer, answer = format!("{other:?}")))
            }
        }
    }
    .await;
    let streams = match answer {
        Ok(Some(streams)) => streams,
        Ok(None) => {
            crate::log_info!("#{id} the receiver declined");
            session.close_current(CLOSE_OK, b"declined");
            return Outcome::Declined;
        }
        // Not accepted yet, so there is nothing to continue later.
        Err(e) => {
            let how = peer_close(&conn);
            crate::log_info!(
                "#{id} no answer to the offer ({how:?}): {}",
                conn.close_reason().map_or_else(
                    || "the connection is open".into(),
                    |r| crate::logging::connection_error(&r)
                )
            );
            return match how {
                PeerClose::Cancelled => Outcome::CancelledByPeer,
                PeerClose::Failed => Outcome::Failed(tr!(error_recipient_failed).into()),
                PeerClose::Suspended | PeerClose::Lost => {
                    session.close_current(CLOSE_FAILED, b"failed");
                    Outcome::Failed(super::failed_text(id, "the offer", &e))
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
            sent: 0,
            staging,
        },
        done_hash: None,
        done_sent: false,
        cancelled: false,
        ended: false,
        ended_at: None,
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
                    .end(Some(CLOSE_FAILED), Ended::Failed)
                    .forget(&shared.data_dir)
                    .await;
                Outcome::Failed(super::failed_text(id, "saving the record", &e.into()))
            }
        };
    }
    // The record names the zip now; the note of it goes with `temp`.
    temp.0 = None;
    staged.0 = None;
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
        peer_part_in_use: false,
        peer_suspended: false,
        notes: SentNotes::default(),
    };
    // From here on `stream` watches each connection, and only while it
    // lasts: a watcher of this one could still report it after it is gone.
    drop(path_watch);
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
        peer_part_in_use: false,
        peer_suspended: false,
        notes: SentNotes::default(),
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

/// What [`prepare`] prepares.
struct Prepare<'a> {
    path: &'a PathBuf,
    /// How errors name it.
    shown: &'a str,
    kind: Kind,
    /// Those of a folder that the app listed (see [`source::Document`]).
    items: Option<Vec<source::Item>>,
}

/// Zips a folder (reporting progress) or measures a file. Returns what is
/// streamed: its path, what it holds (see `archive::Packed`), and the stream
/// size.
async fn prepare(
    shared: &Arc<Shared>,
    id: u64,
    what: Prepare<'_>,
    token: &Token,
    ctl: &Control,
    temp: &mut TempFile,
) -> anyhow::Result<(PathBuf, archive::Packed, u64)> {
    let Prepare {
        path,
        shown,
        kind,
        items,
    } = what;
    match kind {
        Kind::File => {
            let source = path.clone();
            let size = tokio::task::spawn_blocking(move || source::metadata(&source))
                .await?
                .with_context(|| tr!(error_cannot_read, path = shown))?
                .len();
            let packed = archive::Packed {
                files: 1,
                size,
                largest: size,
            };
            Ok((path.clone(), packed, size))
        }
        Kind::Folder => {
            shared.emit(Event::Phase {
                id,
                phase: Phase::Preparing,
            });
            // What another app shared, or a listed document, has no folder
            // of its own to pack beside.
            let (zip_path, note) = if items.is_some() {
                tokio::fs::create_dir_all(&shared.temp_dir).await?;
                let zip = session::temp_archive(&shared.temp_dir, Role::Send, token);
                (zip, None)
            } else {
                let (shared, source, token) = (shared.clone(), path.clone(), *token);
                tokio::task::spawn_blocking(move || new_zip(&shared, &source, &token)).await??
            };
            let beside = note.is_some();
            (temp.0, temp.1) = (Some(zip_path.clone()), note);
            let shared2 = shared.clone();
            let cancel = ctl.cancel.clone();
            let src = path.clone();
            let zip_dst = zip_path.clone();
            let packed = tokio::task::spawn_blocking(move || {
                crate::log_info!(
                    "#{id} packing the folder {} (file system {})",
                    if beside {
                        "beside it"
                    } else {
                        "in the temporary folder"
                    },
                    zip_dst
                        .ancestors()
                        .skip(1)
                        .find(|d| d.is_dir())
                        .and_then(archive::file_system)
                        .unwrap_or_else(|| "unknown".into())
                );
                let mut throttle = Throttle::new();
                let mut progress = |done, total| {
                    if throttle.ready() || done == total {
                        shared2.emit(Event::Progress { id, done, total });
                    }
                };
                match items {
                    Some(items) => archive::zip_listed(&items, &zip_dst, cancel.flag(), &mut progress),
                    None => archive::zip_folder(&src, &zip_dst, cancel.flag(), &mut progress),
                }
            })
            .await??;
            // Among the user's own files it is in nobody's way: only now,
            // as Windows lets no one write a hidden file anew.
            if beside {
                archive::keep_out_of_sight(&zip_path);
            }
            let size = tokio::fs::metadata(&zip_path).await?.len();
            Ok((zip_path, packed, size))
        }
    }
}

impl Sender<'_> {
    fn emit(&self, phase: Phase) {
        self.shared.emit(Event::Phase { id: self.id, phase });
    }

    /// Notes in the record how much has gone out (see `SentNotes`).
    async fn note_sent(&mut self, done: u64) {
        let (session, data_dir) = (self.session.clone(), self.shared.data_dir.clone());
        self.notes
            .note(done, move || session.save_sent(&data_dir, done))
            .await;
    }

    /// The status while connected: paused here, paused there, held up by a
    /// program there, or running.
    fn emit_pause_phase(&self) {
        self.emit(if self.ctl.is_paused() {
            Phase::Paused
        } else if self.peer_paused {
            Phase::PausedByPeer
        } else if self.peer_part_in_use {
            Phase::PartInUse { finishing: false }
        } else {
            Phase::Transferring
        });
    }

    /// The status while disconnected, after saying that there is no
    /// connection now (a paused transfer can be without one too).
    fn emit_waiting(&self) {
        self.shared.emit(Event::Path {
            id: self.id,
            direct: None,
        });
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
    async fn end(&self, code: Option<u32>, ended: Ended) {
        let ending = self.session.end(code, ended);
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
        let mut repeats = crate::logging::Repeats::new();
        loop {
            if self.shared.shutting_down.load(Ordering::SeqCst) {
                // Exiting: the transfer continues at the next start, and the
                // receiver was told so by the suspended connection.
                std::future::pending::<()>().await;
            }
            if *self.paused_rx.borrow() {
                // Paused: nothing to do until the user resumes (a cancel
                // resumes the transfer too, see `Control::cancel`).
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
                crate::log_warn!("#{} what is sent changed since the transfer began", self.id);
                self.end(None, Ended::Failed).await;
                return Err(Outcome::Failed(tr!(error_source_changed).into()));
            }
            let dialed = tokio::select! {
                r = dial(self.endpoint, self.to, self.id, &mut repeats) => r,
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
                                .update(&self.shared.data_dir, |s, d| s.clear_done_sent(d))
                                .await;
                            if !self.source_unchanged().await {
                                crate::log_warn!(
                                    "#{} the receiver needs the data again, which changed",
                                    self.id
                                );
                                self.end(Some(CLOSE_FAILED), Ended::Failed).await;
                                return Err(Outcome::Failed(tr!(error_source_changed).into()));
                            }
                        }
                        return Ok(streams);
                    }
                    Err(Some(ended)) => {
                        let outcome = if ended == Ended::Completed {
                            Outcome::Completed(None)
                        } else if self.ctl.cancelled() && !handed_over {
                            // Ended there too, without the item, as the user
                            // wanted here.
                            Outcome::Cancelled
                        } else if ended == Ended::Cancelled {
                            // Ended there, by the receiver: also when the
                            // user's cancel here came too late, since the
                            // item was the receiver's by then.
                            Outcome::CancelledByPeer
                        } else {
                            Outcome::Failed(tr!(error_recipient_failed).into())
                        };
                        self.end(None, outcome.ended()).await;
                        return Err(outcome);
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

    /// `Continue` on a new connection. `Err(Some(ended))` if the receiver no
    /// longer has the transfer (and how it ended there), `Err(None)` to try
    /// again later.
    async fn handshake(&self, conn: Connection) -> Result<(Streams, u64), Option<Ended>> {
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
        let id = self.id;
        match answer {
            Ok((Msg::ContinueFrom { have }, send, recv)) if have <= self.size => {
                crate::log_info!("#{id} the receiver has {have} of {} bytes", self.size);
                Ok((Streams { conn, send, recv }, have))
            }
            Ok((Msg::Gone { ended }, _, _)) => {
                crate::log_info!("#{id} the receiver no longer has the transfer ({ended:?})");
                conn.close(CLOSE_OK.into(), b"gone");
                Err(Some(ended))
            }
            other => {
                match other {
                    Ok((Msg::ContinueFrom { have }, ..)) => crate::log_warn!(
                        "#{id} the receiver says it has {have} bytes of {}",
                        self.size
                    ),
                    Ok((msg, ..)) => {
                        crate::log_warn!("#{id} the answer to Continue was {}", msg.kind_name())
                    }
                    Err(e) => crate::log_info!(
                        "#{id} no answer to Continue: {}",
                        crate::logging::cause(&e)
                    ),
                }
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
        self.peer_part_in_use = false;
        self.peer_suspended = false;

        let result = self.pump(&mut send, &mut ctl_rx, have).await;
        if let Err(e) = &result
            && e.is::<Lost>()
        {
            crate::log_info!(
                "#{} the connection ended ({:?}): {}; the connection says {}",
                self.id,
                peer_close(&conn),
                crate::logging::cause(e),
                conn.close_reason().map_or_else(
                    || "nothing".into(),
                    |r| crate::logging::connection_error(&r)
                )
            );
        }
        match result {
            Ok(outcome) => {
                self.end(Some(CLOSE_OK), Ended::Completed).await;
                Ok(outcome)
            }
            Err(e) if e.is::<Lost>() => match peer_close(&conn) {
                PeerClose::Cancelled => {
                    self.end(None, Ended::Cancelled).await;
                    Err(End::Final(Outcome::CancelledByPeer))
                }
                PeerClose::Failed => {
                    self.end(None, Ended::Failed).await;
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
                self.end(Some(CLOSE_FAILED), Ended::Failed).await;
                Err(End::Final(Outcome::Failed(super::failed_text(
                    self.id, "sending", &e,
                ))))
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
        // What the receiver has, which can be less than what had gone out:
        // after the last note is written, so that this one comes last.
        self.notes.wait().await;
        self.note_sent(have).await;
        let mut hasher = self.hasher_at(have).await?;
        self.emit_pause_phase();
        self.shared.emit(Event::Progress {
            id: self.id,
            done: have,
            total: self.size,
        });
        let data = self.data_path.clone();
        let mut file =
            tokio::fs::File::from_std(tokio::task::spawn_blocking(move || source::open(&data)).await??);
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
                        if done - self.notes.noted >= SAVE_SENT_EVERY {
                            self.note_sent(done).await;
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
                    Some(Msg::PartInUse { in_use }) => {
                        self.peer_part_in_use = in_use;
                        self.emit_pause_phase();
                    }
                    Some(Msg::Result { ok: false, message }) => {
                        crate::log_warn!("#{} the receiver reports a failure while data flows", self.id);
                        bail!(tr!(error_peer_reported, message = message))
                    }
                    Some(other) => {
                        crate::log_warn!("#{} the receiver sent {} while data flows", self.id, other.kind_name());
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
        // can close the connection behind it; the record says so in the same
        // step, so that a cancel meanwhile knows that it waits.
        if self.session.done_sent() {
            self.session
                .update(&self.shared.data_dir, |s, d| s.save_record(d))
                .await;
        }
        protocol::write_msg(send, &Msg::Done { hash })
            .await
            .map_err(lost)?;
        send.finish().map_err(lost)?;
        crate::log_info!(
            "#{} all data sent; waiting for the receiver's verdict",
            self.id
        );

        // Wait for the receiver's verdict: first that all of it arrived
        // intact, then, once it is saved there, the result. `Some(unpacking)`
        // once it did.
        let mut verified = None;
        loop {
            match ctl_rx.recv().await {
                Some(Msg::Verified { unpacking }) => {
                    verified = Some(unpacking);
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
                    crate::log_warn!(
                        "#{} the receiver reports a failure (verified before: {verified:?})",
                        self.id
                    );
                    bail!(tr!(error_peer_reported, message = message))
                }
                // A program there keeps the part out, and the receiver waits
                // for it: as it began to read (all of the data may have been
                // sent before it could), or in its last step.
                Some(Msg::PartInUse { in_use }) => self.emit(match (in_use, verified) {
                    (true, _) => Phase::PartInUse { finishing: true },
                    (false, Some(true)) => Phase::PeerUnpacking,
                    (false, _) => Phase::Transferring,
                }),
                Some(Msg::Paused | Msg::Resumed) => {}
                Some(other) => {
                    crate::log_warn!(
                        "#{} the receiver sent {} after all data",
                        self.id,
                        other.kind_name()
                    );
                    bail!(tr!(error_unexpected_answer, answer = format!("{other:?}")))
                }
                None => return Err(lost(tr!(error_connection_lost))),
            }
        }
    }
}

#[cfg(test)]
mod packing_tests {
    use super::*;

    /// A folder is packed in the temporary folder, unless that drive lacks
    /// the room and the folder's own drive, another one, has it.
    #[test]
    fn a_folder_is_packed_where_there_is_room() {
        let drive = |name: &str, free| Some((PathBuf::from(name), free));
        let pack = |temp: Option<(PathBuf, u64)>, beside: Option<(PathBuf, u64)>| {
            pack_in(100, temp.as_ref(), beside.as_ref())
        };
        assert_eq!(pack(drive("C:\\", 100), drive("E:\\", 500)), PackIn::Temp);
        assert_eq!(pack(drive("C:\\", 99), drive("E:\\", 100)), PackIn::Beside);
        // No room there either, the same drive, or nothing known.
        assert_eq!(pack(drive("C:\\", 99), drive("E:\\", 99)), PackIn::Temp);
        assert_eq!(pack(drive("C:\\", 99), drive("c:\\", 500)), PackIn::Temp);
        assert_eq!(pack(None, drive("E:\\", 500)), PackIn::Temp);
        assert_eq!(pack(drive("C:\\", 99), None), PackIn::Temp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outcome of `task` as a transfer's task, with a panic in it turned
    /// into a failure (see `net::guarded`).
    async fn as_task(task: impl Future<Output = Outcome> + Send + 'static) -> Outcome {
        // Nothing is written there: the transfer has no session.
        let (shared, _) = super::super::tests::recording_shared(&std::env::temp_dir());
        super::super::guarded(&shared, 1, task).await
    }

    fn failed_unexpectedly(outcome: &Outcome) -> bool {
        matches!(outcome, Outcome::Failed(text) if text == tr!(error_unexpected))
    }

    /// A panic in the write of a note of how much has gone out fails the
    /// send, whether the next note or the next connection comes to it,
    /// rather than going unnoticed with the note (see `SentNotes`).
    #[tokio::test]
    async fn a_note_that_panics_fails_the_send() {
        let outcome = as_task(async {
            let mut notes = SentNotes::default();
            notes.note(1, || panic!("boom")).await;
            while !notes.writing.as_ref().unwrap().is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            notes.note(2, || ()).await;
            Outcome::Completed(None)
        })
        .await;
        assert!(failed_unexpectedly(&outcome), "next note: {outcome:?}");
        let outcome = as_task(async {
            let mut notes = SentNotes::default();
            notes.note(1, || panic!("boom")).await;
            notes.wait().await;
            Outcome::Completed(None)
        })
        .await;
        assert!(
            failed_unexpectedly(&outcome),
            "next connection: {outcome:?}"
        );
    }

    /// While a note is still being written, the next one is left out at
    /// once, and the wait for the notes waits for that write.
    #[tokio::test]
    async fn a_note_is_left_out_while_the_last_is_written() {
        let mut notes = SentNotes::default();
        let (go, held) = std::sync::mpsc::channel::<()>();
        notes
            .note(1, move || {
                let _ = held.recv();
            })
            .await;
        let left_out = notes.note(2, || panic!("written"));
        tokio::time::timeout(Duration::from_secs(10), left_out)
            .await
            .expect("not left out");
        assert_eq!(notes.noted, 1);
        go.send(()).unwrap();
        notes.wait().await;
        assert!(notes.writing.is_none());
    }
}
