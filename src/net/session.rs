//! What a transfer needs beyond one connection: the record of an unfinished
//! transfer, kept on disk so it can continue after a lost connection or a
//! restart; tombstones of transfers that ended while the other side was away;
//! the registry that routes a new connection to the transfer it continues;
//! and the hashing of data that was already transferred.

use std::{
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, atomic::AtomicBool, atomic::Ordering},
    time::{Duration, SystemTime},
};

use iroh::{
    EndpointId,
    endpoint::{Connection, ConnectionError, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use super::{
    CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_SUSPENDED, CancelToken, ReceivedFolder,
    protocol::{Kind, Offer, Token, token_hex},
};

/// Records and tombstones older than this are forgotten at startup.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Temporary archives without a record are deleted once they are this old.
const TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// The sender keeps a copy of its hash state this often, so that it can
/// continue from an earlier byte without hashing the whole file again.
const SNAPSHOT_EVERY: u64 = 64 * 1024 * 1024;
const HASH_BUF: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Send,
    Receive,
}

/// The files of a transfer on this side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Send {
        /// What the user chose to send.
        source: PathBuf,
        /// Size and modification time of `data_path` when the transfer
        /// started; changed data cannot be continued.
        source_len: u64,
        source_mtime: Option<SystemTime>,
        /// What is streamed: the file itself, or the zip of the folder.
        data_path: PathBuf,
    },
    Receive {
        dest_dir: PathBuf,
        /// The `.part` file. For a folder that is only unpacked, its zip in
        /// the temporary folder (see [`temporary_part`]); for a folder whose
        /// zip is kept, `<name>.zip.part` in `dest_dir`.
        part_path: PathBuf,
        /// How much of the part file is known to be on disk, so that a power
        /// cut cannot leave data that looks received but never got written.
        /// `None` in records of earlier versions: the file length is trusted.
        #[serde(default)]
        synced: Option<u64>,
        /// For folders: what is kept of the archive, as chosen when the
        /// transfer was accepted. Records of earlier versions only unpack.
        #[serde(default)]
        folder: ReceivedFolder,
        /// The folder that a received folder is being unpacked into. This
        /// transfer created it, and it is complete only once the record is
        /// gone, so the next start removes it and unpacks again
        /// ([`TransferRecord::remove_unpacked`]).
        #[serde(default)]
        unpacking: Option<PathBuf>,
    },
}

/// Whether the part file of a receive is only temporary: the zip of a folder
/// that is only unpacked, which waits in the temporary folder and is deleted
/// at the end. Every other part is the item itself, under a provisional name
/// in the destination folder.
pub fn temporary_part(kind: Kind, folder: ReceivedFolder) -> bool {
    kind == Kind::Folder && folder == ReceivedFolder::Extract
}

fn temp_prefix(role: Role) -> &'static str {
    match role {
        Role::Send => "send-",
        Role::Receive => "recv-",
    }
}

/// A transfer's temporary archive in the profile's temporary folder: the zip
/// of a folder being sent, or a [`temporary_part`].
pub fn temp_archive(temp_dir: &Path, role: Role, token: &Token) -> PathBuf {
    temp_dir.join(format!("{}{}.zip", temp_prefix(role), token_hex(token)))
}

/// Whether a file name is that of a [`temp_archive`].
fn is_temp_archive(name: &str) -> bool {
    [Role::Send, Role::Receive]
        .into_iter()
        .any(|role| name.starts_with(temp_prefix(role)))
        && name.ends_with(".zip")
}

/// An unfinished transfer, stored as `transfers\<token>.json` in the data folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRecord {
    pub token: Token,
    /// The other side's Contact ID.
    pub peer: String,
    pub offer: Offer,
    /// The name of the other side as shown in the window.
    pub peer_name: String,
    /// The user paused the transfer on this side.
    pub paused: bool,
    pub created: SystemTime,
    pub side: Side,
    /// The sender's hash of the whole item once `Done` arrived: with it the
    /// receiver can check and finish the item without the sender.
    #[serde(default)]
    pub done_hash: Option<[u8; 32]>,
}

pub fn records_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("transfers")
}

fn record_path(data_dir: &Path, token: &Token) -> PathBuf {
    records_dir(data_dir).join(format!("{}.json", token_hex(token)))
}

fn tombstone_path(data_dir: &Path, token: &Token) -> PathBuf {
    records_dir(data_dir).join(format!("{}.gone", token_hex(token)))
}

impl TransferRecord {
    pub fn role(&self) -> Role {
        match self.side {
            Side::Send { .. } => Role::Send,
            Side::Receive { .. } => Role::Receive,
        }
    }

    pub fn peer_id(&self) -> Option<EndpointId> {
        crate::identity::parse_contact_id(&self.peer)
    }

    /// The bytes this side already has (receiver: what is on disk of the part
    /// file, at most what is known to be flushed).
    pub fn have(&self) -> u64 {
        match &self.side {
            Side::Receive {
                part_path, synced, ..
            } => {
                let len = fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);
                synced.map_or(len, |s| s.min(len))
            }
            Side::Send { .. } => 0,
        }
    }

    /// Cuts the part file back to what is known to be on disk: after a power
    /// cut the end of the file may never have been written.
    pub fn truncate_to_synced(&self) -> io::Result<()> {
        if let Side::Receive {
            part_path,
            synced: Some(synced),
            ..
        } = &self.side
        {
            let len = fs::metadata(part_path)?.len();
            if len > *synced {
                fs::OpenOptions::new()
                    .write(true)
                    .open(part_path)?
                    .set_len(*synced)?;
            }
        }
        Ok(())
    }

    /// Whether the files the transfer needs still exist.
    pub fn files_present(&self) -> bool {
        match &self.side {
            Side::Send {
                source, data_path, ..
            } => source.exists() && data_path.exists(),
            Side::Receive { part_path, .. } => part_path.exists(),
        }
    }

    /// Whether the data of a send still looks like it did at the start: the
    /// file itself, or the zip made of the folder (changes inside the folder
    /// after packing do not matter).
    pub fn source_unchanged(&self) -> bool {
        match &self.side {
            Side::Send {
                data_path,
                source_len,
                source_mtime,
                ..
            } => match fs::metadata(data_path) {
                Ok(m) => m.len() == *source_len && m.modified().ok() == *source_mtime,
                Err(_) => false,
            },
            Side::Receive { .. } => true,
        }
    }

    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
        let path = record_path(data_dir, &self.token);
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp)?;
            io::Write::write_all(&mut file, &serde_json::to_vec_pretty(self)?)?;
            // On disk before the rename: a power cut leaves the old record or
            // the new one, never half of one.
            file.sync_all()?;
        }
        fs::rename(&tmp, &path)
    }

    /// Forgets the record (the data stays: the transfer is complete).
    pub fn remove(&self, data_dir: &Path) {
        let _ = fs::remove_file(record_path(data_dir, &self.token));
    }

    /// The folder setting of a receive (`None` for a send).
    pub fn received_folder(&self) -> Option<ReceivedFolder> {
        match &self.side {
            Side::Receive { folder, .. } => Some(*folder),
            Side::Send { .. } => None,
        }
    }

    /// Whether the part file of a receive is a [`temporary_part`].
    pub fn part_is_temporary(&self) -> bool {
        self.received_folder()
            .is_some_and(|folder| temporary_part(self.offer.kind, folder))
    }

    /// Ends a completed transfer: forgets the record and deletes what was
    /// only temporary. The part file of a received item has already been
    /// renamed into place, so its old name may by now belong to another
    /// transfer of the same name and must not be deleted; only a temporary
    /// part is.
    pub fn complete(&self, data_dir: &Path) {
        match &self.side {
            Side::Receive { part_path, .. } => {
                self.remove(data_dir);
                if self.part_is_temporary() {
                    let _ = fs::remove_file(part_path);
                }
            }
            Side::Send { .. } => self.discard(data_dir),
        }
    }

    /// The program ended while a received folder was being unpacked: what is
    /// in the folder is incomplete. Forgets the folder in the record first,
    /// then removes it, so that a crash in between can leave the folder but
    /// never names one that may by then be somebody else's; the next attempt
    /// unpacks into a new folder.
    pub fn remove_unpacked(&mut self, data_dir: &Path) {
        if let Side::Receive { unpacking, .. } = &mut self.side
            && let Some(folder) = unpacking.take()
        {
            let _ = self.save(data_dir);
            let _ = fs::remove_dir_all(folder);
        }
    }

    /// Forgets the record and deletes the partial or temporary data.
    pub fn discard(&self, data_dir: &Path) {
        self.remove(data_dir);
        match &self.side {
            Side::Send {
                source, data_path, ..
            } => {
                if data_path != source {
                    let _ = fs::remove_file(data_path);
                }
            }
            Side::Receive { part_path, .. } => {
                let _ = fs::remove_file(part_path);
            }
        }
    }
}

/// Remembers that a transfer ended while the other side could not be told.
pub fn write_tombstone(data_dir: &Path, token: &Token, completed: bool) {
    let path = tombstone_path(data_dir, token);
    if fs::create_dir_all(path.parent().unwrap()).is_ok() {
        let _ = fs::write(path, if completed { "completed" } else { "ended" });
    }
}

/// `Some(completed)` if the transfer ended on this side.
pub fn tombstone(data_dir: &Path, token: &Token) -> Option<bool> {
    fs::read_to_string(tombstone_path(data_dir, token))
        .ok()
        .map(|s| s.trim() == "completed")
}

fn is_old(path: &Path, max_age: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > max_age)
}

/// The unfinished transfers from earlier runs. Old records (with their
/// partial data) and old tombstones are deleted on the way.
pub fn load_all(data_dir: &Path) -> Vec<TransferRecord> {
    let mut records = Vec::new();
    let Ok(entries) = fs::read_dir(records_dir(data_dir)) else {
        return records;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_json = path.extension().and_then(|e| e.to_str()) == Some("json");
        let record = is_json
            .then(|| fs::read_to_string(&path).ok())
            .flatten()
            .and_then(|text| serde_json::from_str::<TransferRecord>(&text).ok());
        if is_old(&path, MAX_AGE) {
            if let Some(record) = &record {
                record.discard(data_dir);
            }
            let _ = fs::remove_file(&path);
            continue;
        }
        match record {
            Some(record) => records.push(record),
            // A damaged record is kept aside instead of being read again at
            // every start.
            None if is_json => {
                let _ = fs::rename(&path, path.with_extension("json.bad"));
            }
            None => {}
        }
    }
    records.sort_by_key(|r| r.created);
    records
}

/// Deletes temporary archives (`send-*.zip`, `recv-*.zip`) in `dir` that no
/// record refers to and that are older than a day: leftovers of transfers
/// that ended before they had a record, such as a crash while packing a
/// folder or an offer that was never accepted. `dir` must be this profile's
/// own temporary folder, because the archives of another profile's transfers
/// are unknown here and would look like leftovers.
pub fn cleanup_temp(dir: &Path, records: &[TransferRecord]) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let in_use: Vec<&Path> = records
        .iter()
        .map(|r| match &r.side {
            Side::Send { data_path, .. } => data_path.as_path(),
            Side::Receive { part_path, .. } => part_path.as_path(),
        })
        .collect();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_temp_archive(&name)
            && !in_use.contains(&path.as_path())
            && is_old(&path, TEMP_MAX_AGE)
        {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Copies of a hash state at regular offsets of the data being sent.
pub struct Snapshots(Vec<(u64, blake3::Hasher)>);

impl Snapshots {
    pub fn new() -> Self {
        Snapshots(Vec::new())
    }

    /// Call with the hash of the first `offset` bytes whenever a frame was sent.
    pub fn record(&mut self, offset: u64, hasher: &blake3::Hasher) {
        let last = self.0.last().map_or(0, |(o, _)| *o);
        if offset >= last + SNAPSHOT_EVERY {
            self.0.push((offset, hasher.clone()));
        }
    }

    /// The latest snapshot at or before `offset`, or a fresh hasher at 0.
    pub fn best(&self, offset: u64) -> (u64, blake3::Hasher) {
        self.0
            .iter()
            .rev()
            .find(|(o, _)| *o <= offset)
            .map(|(o, h)| (*o, h.clone()))
            .unwrap_or_else(|| (0, blake3::Hasher::new()))
    }

    /// Forgets snapshots beyond `offset` (the data from there on is sent again).
    pub fn rewind(&mut self, offset: u64) {
        self.0.retain(|(o, _)| *o <= offset);
    }
}

/// Hashes the bytes `from..to` of `path` into `hasher`. Blocking; call it in
/// `spawn_blocking`. `progress(done, total)` counts the bytes of this call.
pub fn hash_range(
    path: &Path,
    from: u64,
    to: u64,
    hasher: &mut blake3::Hasher,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> io::Result<()> {
    let total = to.saturating_sub(from);
    if total == 0 {
        return Ok(());
    }
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; HASH_BUF];
    let mut done = 0u64;
    while done < total {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let want = HASH_BUF.min((total - done) as usize);
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                crate::i18n::tr!(error_file_shorter),
            ));
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    Ok(())
}

/// A connection with its stream, handed to the transfer that continues on it.
pub(crate) struct Streams {
    pub conn: Connection,
    pub send: SendStream,
    pub recv: RecvStream,
}

/// A running transfer as seen by the connection router and by cancellation.
pub(crate) struct Session {
    pub id: u64,
    pub token: Token,
    pub peer: EndpointId,
    pub role: Role,
    /// Where a new connection for this transfer is delivered (receiver side).
    pub new_conn: mpsc::UnboundedSender<Streams>,
    /// The connection in use, closed on shutdown or when replaced.
    pub current: Mutex<Option<Connection>>,
    /// The record on disk, once the transfer was accepted.
    pub record: Mutex<Option<TransferRecord>>,
    /// The receiver is writing data to the part file, or getting it onto the
    /// disk and into the record after a connection ended. Shutdown waits for
    /// that, so that the next start continues from everything received.
    pub syncing: AtomicBool,
    /// A received item is being put in place, or is in place: a cancel comes
    /// too late (see [`Session::place`]). Changed only with `record` locked.
    placing: AtomicBool,
}

impl Session {
    pub fn new(
        id: u64,
        token: Token,
        peer: EndpointId,
        role: Role,
    ) -> (std::sync::Arc<Session>, mpsc::UnboundedReceiver<Streams>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = Session {
            id,
            token,
            peer,
            role,
            new_conn: tx,
            current: Mutex::new(None),
            record: Mutex::new(None),
            syncing: AtomicBool::new(false),
            placing: AtomicBool::new(false),
        };
        (std::sync::Arc::new(session), rx)
    }

    pub fn set_current(&self, conn: Option<Connection>) -> Option<Connection> {
        std::mem::replace(&mut self.current.lock().unwrap(), conn)
    }

    pub fn close_current(&self, code: u32, reason: &[u8]) {
        if let Some(conn) = self.set_current(None) {
            conn.close(code.into(), reason);
        }
    }

    /// Updates the stored pause state of the record.
    pub fn save_paused(&self, data_dir: &Path, paused: bool) {
        if let Some(record) = self.record.lock().unwrap().as_mut() {
            record.paused = paused;
            let _ = record.save(data_dir);
        }
    }

    /// Remembers how much of the part file is on disk.
    pub fn save_synced(&self, data_dir: &Path, on_disk: u64) {
        if let Some(record) = self.record.lock().unwrap().as_mut()
            && let Side::Receive { synced, .. } = &mut record.side
        {
            *synced = Some(on_disk);
            let _ = record.save(data_dir);
        }
    }

    /// Remembers the sender's final hash: from now on this side can finish
    /// the item on its own.
    pub fn save_done_hash(&self, data_dir: &Path, hash: [u8; 32]) {
        if let Some(record) = self.record.lock().unwrap().as_mut() {
            record.done_hash = Some(hash);
            let _ = record.save(data_dir);
        }
    }

    /// Notes the folder a received folder is unpacked into, before anything
    /// is written to it (see `Side::Receive::unpacking`).
    pub fn save_unpacking(&self, data_dir: &Path, folder: &Path) {
        if let Some(record) = self.record.lock().unwrap().as_mut()
            && let Side::Receive { unpacking, .. } = &mut record.side
        {
            *unpacking = Some(folder.to_path_buf());
            let _ = record.save(data_dir);
        }
    }

    /// Ends the transfer on this side: closes the connection (with `code`, if
    /// given), leaves a tombstone so the other side learns about it later, and
    /// forgets the record together with the partial or temporary data.
    pub fn end(&self, data_dir: &Path, code: Option<u32>, completed: bool) {
        match code {
            Some(code) => self.close_current(code, b""),
            None => {
                self.set_current(None);
            }
        }
        let record = self.record.lock().unwrap().take();
        forget(data_dir, record, completed);
    }

    /// The user cancelled a receive: ends it like [`end`](Self::end), unless
    /// its item is already being put in place, which a cancel can no longer
    /// stop. Returns whether it ended the transfer.
    pub fn cancel(&self, data_dir: &Path) -> bool {
        let record = {
            let mut record = self.record.lock().unwrap();
            if self.placing.load(Ordering::SeqCst) {
                return false;
            }
            record.take()
        };
        self.close_current(CLOSE_CANCELLED, b"");
        forget(data_dir, record, false);
        true
    }

    /// Puts a received item in place with `step` (renaming the part file,
    /// say) and ends the transfer as completed right away, while the sender
    /// may still be reading the result: from then on the part's old name may
    /// belong to another transfer, and neither a late cancel nor a crash may
    /// treat it as this transfer's data. A cancel that comes while `step`
    /// runs is too late (see [`cancel`](Self::cancel)); one that came before
    /// has ended the transfer, and then `step` does not run.
    pub fn place(
        &self,
        data_dir: &Path,
        step: &mut dyn FnMut() -> anyhow::Result<PathBuf>,
    ) -> anyhow::Result<PathBuf> {
        {
            let record = self.record.lock().unwrap();
            anyhow::ensure!(record.is_some(), "Cancelled");
            self.placing.store(true, Ordering::SeqCst);
        }
        let placed = step();
        let record = {
            let mut record = self.record.lock().unwrap();
            if placed.is_err() {
                self.placing.store(false, Ordering::SeqCst);
                return placed;
            }
            record.take()
        };
        forget(data_dir, record, true);
        placed
    }
}

/// Leaves a tombstone for an ended transfer and forgets its record, with
/// what `complete` or `discard` deletes.
fn forget(data_dir: &Path, record: Option<TransferRecord>, completed: bool) {
    if let Some(record) = record {
        write_tombstone(data_dir, &record.token, completed);
        if completed {
            record.complete(data_dir);
        } else {
            record.discard(data_dir);
        }
    }
}

/// What the user can do to a running transfer.
pub(crate) struct Control {
    pub cancel: CancelToken,
    pub paused: watch::Sender<bool>,
}

impl Control {
    pub fn new(paused: bool) -> Self {
        Control {
            cancel: CancelToken::new(),
            paused: watch::channel(paused).0,
        }
    }

    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }
}

/// The connection failed in a way that a new connection can recover from.
#[derive(Debug)]
pub(crate) struct Lost(pub String);

impl std::fmt::Display for Lost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Lost {}

/// Wraps a stream or connection error as a lost connection.
pub(crate) fn lost(e: impl std::fmt::Display) -> anyhow::Error {
    Lost(e.to_string()).into()
}

/// Why a connection ended, as far as the other side's intent goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PeerClose {
    /// The user on the other side cancelled.
    Cancelled,
    /// The transfer failed on the other side.
    Failed,
    /// The other side went away for now (exiting, or replacing the
    /// connection); the transfer continues later, like after a loss.
    Suspended,
    /// Anything else: a network problem or a timeout.
    Lost,
}

pub(crate) fn peer_close(conn: &Connection) -> PeerClose {
    match conn.close_reason() {
        Some(ConnectionError::ApplicationClosed(close))
            if close.error_code == CLOSE_CANCELLED.into() =>
        {
            PeerClose::Cancelled
        }
        Some(ConnectionError::ApplicationClosed(close))
            if close.error_code == CLOSE_FAILED.into() =>
        {
            PeerClose::Failed
        }
        Some(ConnectionError::ApplicationClosed(close))
            if close.error_code == CLOSE_SUSPENDED.into() =>
        {
            PeerClose::Suspended
        }
        _ => PeerClose::Lost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dodavo-session-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn record(token: u8, side: Side) -> TransferRecord {
        TransferRecord {
            token: [token; 16],
            peer: "ab".repeat(32),
            offer: Offer {
                kind: Kind::File,
                name: "a.bin".into(),
                size: 10,
                file_count: 1,
                content_size: 10,
                sender_name: "Ana".into(),
                token: [token; 16],
                modified: Some(130_000_000_000_000_000),
                created: None,
            },
            peer_name: "Ana".into(),
            paused: false,
            created: SystemTime::now(),
            side,
            done_hash: None,
        }
    }

    fn receive_side(dir: &Path, part: &Path, synced: Option<u64>) -> Side {
        Side::Receive {
            dest_dir: dir.to_path_buf(),
            part_path: part.to_path_buf(),
            synced,
            folder: ReceivedFolder::Extract,
            unpacking: None,
        }
    }

    #[test]
    fn records_roundtrip_and_discard() {
        let dir = scratch("records");
        let part = dir.join("a.bin.part");
        fs::write(&part, b"12345").unwrap();
        let r = record(1, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        assert_eq!(r.have(), 5);
        assert!(r.files_present());
        let loaded = load_all(&dir);
        assert_eq!(loaded, vec![r.clone()]);
        assert_eq!(loaded[0].role(), Role::Receive);
        r.discard(&dir);
        assert!(!part.exists());
        assert!(load_all(&dir).is_empty());

        let zip = dir.join("send.zip");
        fs::write(&zip, b"zip").unwrap();
        let s = record(
            2,
            Side::Send {
                source: dir.clone(),
                source_len: 0,
                source_mtime: None,
                data_path: zip.clone(),
            },
        );
        s.save(&dir).unwrap();
        s.remove(&dir);
        assert!(zip.exists(), "remove keeps the data");
        s.discard(&dir);
        assert!(!zip.exists(), "discard deletes the zip but not the source");
        assert!(dir.exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// A completed receive must not delete its part file's old name: the
    /// part has been renamed into place, and a new transfer of the same name
    /// may be using that name by now. Only the temporary zip of a folder that
    /// was unpacked goes.
    #[test]
    fn completing_keeps_a_reused_part_name() {
        let dir = scratch("complete");
        let part = dir.join("a.bin.part");
        fs::write(&part, b"another transfer").unwrap();
        let r = record(3, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        r.complete(&dir);
        assert!(part.exists(), "the name belongs to another transfer now");
        assert!(load_all(&dir).is_empty());

        let mut folder = record(4, receive_side(&dir, &dir.join("recv-4.zip"), None));
        folder.offer.kind = Kind::Folder;
        fs::write(dir.join("recv-4.zip"), b"zip").unwrap();
        folder.complete(&dir);
        assert!(!dir.join("recv-4.zip").exists(), "the temporary zip goes");

        let kept_part = dir.join("Photos.zip.part");
        fs::write(&kept_part, b"another transfer").unwrap();
        let mut side = receive_side(&dir, &kept_part, None);
        if let Side::Receive { folder, .. } = &mut side {
            *folder = ReceivedFolder::Both;
        }
        let mut kept = record(5, side);
        kept.offer.kind = Kind::Folder;
        assert!(!kept.part_is_temporary());
        kept.complete(&dir);
        assert!(kept_part.exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// A cancel ends a receive until its item is being put in place; from
    /// then on it comes too late, and after it nothing is put in place.
    #[test]
    fn cancelling_and_placing_exclude_each_other() {
        let dir = scratch("place");
        let peer = iroh::SecretKey::generate().public();

        // Cancelled first: the partial data goes and nothing is placed.
        let part = dir.join("a.bin.part");
        fs::write(&part, b"data").unwrap();
        let (session, _rx) = Session::new(1, [1; 16], peer, Role::Receive);
        *session.record.lock().unwrap() = Some(record(1, receive_side(&dir, &part, None)));
        assert!(session.cancel(&dir));
        assert!(!part.exists());
        let mut ran = false;
        let placed = session.place(&dir, &mut || {
            ran = true;
            Ok(dir.join("a.bin"))
        });
        assert!(placed.is_err() && !ran);
        assert_eq!(tombstone(&dir, &[1; 16]), Some(false));

        // Placed first: a cancel while placing or afterwards changes nothing,
        // and the transfer has ended as completed.
        let part = dir.join("b.bin.part");
        fs::write(&part, b"data").unwrap();
        let (session, _rx) = Session::new(2, [2; 16], peer, Role::Receive);
        let r = record(2, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        *session.record.lock().unwrap() = Some(r);
        let placed = session
            .place(&dir, &mut || {
                assert!(!session.cancel(&dir), "too late while placing");
                fs::rename(&part, dir.join("b.bin"))?;
                Ok(dir.join("b.bin"))
            })
            .unwrap();
        assert!(!session.cancel(&dir));
        assert_eq!(fs::read(&placed).unwrap(), b"data");
        assert_eq!(tombstone(&dir, &[2; 16]), Some(true));
        assert!(load_all(&dir).is_empty());

        // A step that fails leaves the transfer running, so it can still
        // be cancelled (or ended as failed).
        let (session, _rx) = Session::new(3, [3; 16], peer, Role::Receive);
        *session.record.lock().unwrap() =
            Some(record(3, receive_side(&dir, &dir.join("c.part"), None)));
        assert!(
            session
                .place(&dir, &mut || anyhow::bail!("blocked"))
                .is_err()
        );
        assert!(session.cancel(&dir));
        fs::remove_dir_all(&dir).ok();
    }

    /// After a restart during unpacking, the incomplete folder goes, and
    /// the record no longer names it.
    #[test]
    fn an_interrupted_unpacking_is_removed() {
        let dir = scratch("unpacked");
        let part = dir.join("Photos.zip.part");
        fs::write(&part, b"zip").unwrap();
        let partial = dir.join("Photos");
        fs::create_dir_all(partial.join("sub")).unwrap();
        fs::write(partial.join("sub").join("half.jpg"), b"ha").unwrap();
        let mut side = receive_side(&dir, &part, None);
        if let Side::Receive { unpacking, .. } = &mut side {
            *unpacking = Some(partial.clone());
        }
        let mut r = record(10, side);
        r.save(&dir).unwrap();
        r.remove_unpacked(&dir);
        assert!(!partial.exists());
        assert!(part.exists(), "the archive stays, to unpack again");
        let loaded = load_all(&dir);
        assert!(matches!(
            &loaded[0].side,
            Side::Receive {
                unpacking: None,
                ..
            }
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_of_earlier_versions_still_load() {
        let dir = scratch("legacy");
        let part = dir.join("old.bin.part");
        fs::write(&part, b"12345").unwrap();
        // A record as written before `synced`, `done_hash`, `folder`,
        // `unpacking` and the offer's times existed.
        let mut side = receive_side(&dir, &part, None);
        if let Side::Receive { folder, .. } = &mut side {
            *folder = ReceivedFolder::Both;
        }
        let mut json = serde_json::to_value(record(7, side)).unwrap();
        json.as_object_mut().unwrap().remove("done_hash");
        let receive = json["side"]["receive"].as_object_mut().unwrap();
        receive.remove("synced");
        receive.remove("folder");
        receive.remove("unpacking");
        let offer = json["offer"].as_object_mut().unwrap();
        offer.remove("modified");
        offer.remove("created");
        fs::create_dir_all(records_dir(&dir)).unwrap();
        fs::write(record_path(&dir, &[7; 16]), json.to_string()).unwrap();
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].done_hash, None);
        assert_eq!(loaded[0].received_folder(), Some(ReceivedFolder::Extract));
        assert_eq!(loaded[0].offer.modified, None);
        assert_eq!(
            loaded[0].have(),
            5,
            "without a sync mark the length is trusted"
        );

        // A damaged record is set aside, not read again and again.
        fs::write(record_path(&dir, &[8; 16]), "{ not json").unwrap();
        assert_eq!(load_all(&dir).len(), 1);
        assert!(!record_path(&dir, &[8; 16]).exists());
        assert!(
            records_dir(&dir)
                .join(format!("{}.json.bad", token_hex(&[8; 16])))
                .exists()
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn synced_mark_limits_and_truncates_the_part() {
        let dir = scratch("synced");
        let part = dir.join("f.part");
        fs::write(&part, b"123456789").unwrap();
        let r = record(9, receive_side(&dir, &part, Some(4)));
        assert_eq!(r.have(), 4);
        r.truncate_to_synced().unwrap();
        assert_eq!(fs::read(&part).unwrap(), b"1234");
        // A mark beyond the file never grows it.
        let r = record(9, receive_side(&dir, &part, Some(100)));
        assert_eq!(r.have(), 4);
        r.truncate_to_synced().unwrap();
        assert_eq!(fs::metadata(&part).unwrap().len(), 4);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expired_records_take_their_data_along() {
        let dir = scratch("expired");
        let part = dir.join("old.part");
        fs::write(&part, b"old").unwrap();
        let r = record(6, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        let long_ago = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(record_path(&dir, &r.token))
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert!(load_all(&dir).is_empty());
        assert!(
            !part.exists(),
            "the partial data of an expired record is deleted"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn temp_cleanup_removes_only_old_orphans() {
        let temp = scratch("cleanup-temp");
        let orphan = temp.join("send-00000000000000000000000000000001.zip");
        let used = temp.join("recv-00000000000000000000000000000002.zip");
        let fresh = temp.join("send-00000000000000000000000000000003.zip");
        let other = temp.join("notes-00000000000000000000000000000004.zip");
        for p in [&orphan, &used, &fresh, &other] {
            fs::write(p, b"z").unwrap();
        }
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        for p in [&orphan, &used, &other] {
            fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(two_days_ago)
                .unwrap();
        }
        let dir = scratch("cleanup");
        let r = record(2, receive_side(&dir, &used, None));
        cleanup_temp(&temp, &[r]);
        assert!(!orphan.exists(), "old and unreferenced: deleted");
        assert!(used.exists(), "referenced by a record: kept");
        assert!(fresh.exists(), "recent: kept");
        assert!(other.exists(), "not a Dodavo archive: kept");
        fs::remove_dir_all(&temp).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tombstones() {
        let dir = scratch("tomb");
        assert_eq!(tombstone(&dir, &[3; 16]), None);
        write_tombstone(&dir, &[3; 16], false);
        write_tombstone(&dir, &[4; 16], true);
        assert_eq!(tombstone(&dir, &[3; 16]), Some(false));
        assert_eq!(tombstone(&dir, &[4; 16]), Some(true));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn source_change_is_noticed() {
        let dir = scratch("source");
        let file = dir.join("f.txt");
        fs::write(&file, b"hello").unwrap();
        let meta = fs::metadata(&file).unwrap();
        let r = record(
            5,
            Side::Send {
                source: file.clone(),
                source_len: meta.len(),
                source_mtime: meta.modified().ok(),
                data_path: file.clone(),
            },
        );
        assert!(r.source_unchanged());
        fs::write(&file, b"hello world").unwrap();
        assert!(!r.source_unchanged());

        // A folder is sent as its zip: changes in the folder after packing
        // do not stop the transfer, a changed zip does.
        let folder = dir.join("folder");
        fs::create_dir_all(&folder).unwrap();
        let zip = dir.join("send-x.zip");
        fs::write(&zip, b"zip").unwrap();
        let meta = fs::metadata(&zip).unwrap();
        let f = record(
            6,
            Side::Send {
                source: folder.clone(),
                source_len: meta.len(),
                source_mtime: meta.modified().ok(),
                data_path: zip.clone(),
            },
        );
        fs::write(folder.join("new.txt"), b"later").unwrap();
        assert!(f.source_unchanged());
        fs::write(&zip, b"zip!").unwrap();
        assert!(!f.source_unchanged());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prefix_hash_matches_with_and_without_snapshots() {
        let dir = scratch("hash");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
        let file = dir.join("data.bin");
        fs::write(&file, &data).unwrap();
        let cancel = AtomicBool::new(false);
        let offset = 1_234_567u64;
        let expected = blake3::hash(&data[..offset as usize]);

        let mut plain = blake3::Hasher::new();
        let mut reported = 0;
        hash_range(&file, 0, offset, &mut plain, &cancel, &mut |d, t| {
            reported = d;
            assert_eq!(t, offset);
        })
        .unwrap();
        assert_eq!(plain.finalize(), expected);
        assert_eq!(reported, offset);

        // A snapshot before the offset: only the rest is hashed.
        let mut snapshots = Snapshots::new();
        let mut h = blake3::Hasher::new();
        h.update(&data[..1_000_000]);
        snapshots.0.push((1_000_000, h));
        let (from, mut hasher) = snapshots.best(offset);
        assert_eq!(from, 1_000_000);
        hash_range(&file, from, offset, &mut hasher, &cancel, &mut |_, _| {}).unwrap();
        assert_eq!(hasher.finalize(), expected);

        // Snapshots beyond the offset are not used.
        let (from, _) = snapshots.best(999_999);
        assert_eq!(from, 0);
        snapshots.rewind(500_000);
        assert!(snapshots.0.is_empty());

        // Cancellation stops the hashing.
        cancel.store(true, Ordering::Relaxed);
        let err = hash_range(
            &file,
            0,
            offset,
            &mut blake3::Hasher::new(),
            &cancel,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn snapshots_are_spaced() {
        let mut s = Snapshots::new();
        let h = blake3::Hasher::new();
        s.record(1, &h);
        assert!(s.0.is_empty());
        s.record(SNAPSHOT_EVERY, &h);
        s.record(SNAPSHOT_EVERY + 5, &h);
        s.record(3 * SNAPSHOT_EVERY, &h);
        assert_eq!(s.0.len(), 2);
    }
}
