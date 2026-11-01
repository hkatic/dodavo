//! What a transfer needs beyond one connection: the record of an unfinished
//! transfer, kept on disk so it can continue after a lost connection or a
//! restart; tombstones of transfers that ended while the other side was away;
//! the registry that routes a new connection to the transfer it continues;
//! and the hashing of data that was already transferred.

use std::{
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError, atomic::AtomicBool, atomic::Ordering},
    time::{Duration, SystemTime},
};

use iroh::{
    EndpointId,
    endpoint::{Connection, ConnectionError, RecvStream, SendStream},
};
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::{mpsc, watch};

use super::{
    CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_SUSPENDED, CancelToken, ReceivedFolder,
    protocol::{Kind, Offer, Token, token_hex},
};
use crate::archive::{self, Cancelled, FileId};

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
        /// zip is kept, `<name>.zip.part` in `dest_dir`. Once it has been
        /// renamed to its final name (see `moving`), the item itself.
        part_path: PathBuf,
        /// The identity of the part file (see [`Owned`]): what has its name
        /// is the part only while it is that file, so that a later start
        /// never cuts, deletes or completes something else of the same name.
        /// `None` in records of earlier versions, which trust the name.
        #[serde(default)]
        part_id: Option<FileId>,
        /// How much of the part file is known to be on disk, so that a power
        /// cut cannot leave data that looks received but never got written.
        /// `None` in records of earlier versions: the file length is trusted.
        #[serde(default)]
        synced: Option<u64>,
        /// For folders: what is kept of the archive, as chosen when the
        /// transfer was accepted. Records of earlier versions only unpack.
        #[serde(default)]
        folder: ReceivedFolder,
        /// The folder that a received folder is being unpacked into, noted
        /// before anything is written to it. It is complete only once the
        /// record is gone: a later start deletes it and unpacks the archive
        /// again into a new folder, and ending the transfer otherwise deletes
        /// it. Records of earlier versions have only its path (see
        /// [`unpacking_of_any_version`]).
        #[serde(default, deserialize_with = "unpacking_of_any_version")]
        unpacking: Option<Owned>,
        /// Where the part file is being renamed to, with the part's identity:
        /// noted before each attempt, so that after a crash right after the
        /// rename the record finds the part again
        /// ([`TransferRecord::follow_moved_part`]) and never takes a new file
        /// under the part's old name for it.
        #[serde(default)]
        moving: Option<Owned>,
        /// Folders of the transfer that have to go but could not be deleted
        /// yet, such as an incomplete one of an earlier run in which another
        /// program holds a file open. Ending the transfer deletes them: a
        /// completed one tries once, a failed or cancelled one keeps its
        /// record until they are gone.
        #[serde(default)]
        leftovers: Vec<Owned>,
    },
}

/// Reads `Side::Receive::unpacking` as any version wrote it. Earlier versions
/// noted only the folder's path, which is trusted as they did: the folder is
/// whatever has that name now.
fn unpacking_of_any_version<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Owned>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Noted {
        Owned(Owned),
        Path(PathBuf),
    }
    Ok(match Option::<Noted>::deserialize(deserializer)? {
        Some(Noted::Owned(owned)) => Some(owned),
        Some(Noted::Path(path)) => Owned::at(&path).ok(),
        None => None,
    })
}

/// A file or folder of a transfer, by path and identity: what is at `path`
/// belongs to the transfer only while it is the same item. So a later start
/// never deletes or writes to what has taken its place, such as a folder of
/// the same name that the user made after deleting this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owned {
    pub path: PathBuf,
    pub id: FileId,
}

impl Owned {
    /// What is at `path` now.
    pub fn at(path: &Path) -> io::Result<Owned> {
        Ok(Owned {
            path: path.to_path_buf(),
            id: archive::file_id(path)?,
        })
    }

    /// Whether the item at `path` is still this one.
    pub fn is_there(&self) -> bool {
        archive::file_id(&self.path).is_ok_and(|id| id == self.id)
    }

    /// Deletes the folder with all it holds (links in it, not what they point
    /// to), if it is still this one. Whether it is gone: also when something
    /// else has the name now (that is not the transfer's to delete); not when
    /// that cannot be told yet.
    pub fn remove_folder(&self) -> bool {
        match archive::file_id(&self.path) {
            Ok(id) if id == self.id => match fs::remove_dir_all(&self.path) {
                Ok(()) => true,
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            },
            Ok(_) => true,
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        }
    }

    /// Deletes the file if it is still this one; whether it is gone (as
    /// with [`remove_folder`](Self::remove_folder)).
    fn remove_file(&self) -> bool {
        match archive::file_id(&self.path) {
            Ok(id) if id == self.id => remove_file(&self.path),
            Ok(_) => true,
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        }
    }
}

/// The files of a receive that its last step changes (see `Side::Receive`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiveFiles {
    pub part: PathBuf,
    pub unpacking: Option<Owned>,
    pub moving: Option<Owned>,
    pub leftovers: Vec<Owned>,
}

impl ReceiveFiles {
    /// Whether the part file has its final name already.
    pub fn part_in_place(&self) -> bool {
        self.moving.as_ref().is_some_and(|m| m.path == self.part)
    }
}

/// Deletes a file; whether it is gone.
fn remove_file(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
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
    /// The sender's hash of the whole item. The receiver notes it once
    /// `Done` arrived and the data matched it: from then on it can finish
    /// the item without the sender. The sender notes it once the receiver
    /// confirmed that (`Verified`): from then on a cancel comes too late
    /// there (see [`Session::hand_over`]).
    #[serde(default)]
    pub done_hash: Option<[u8; 32]>,
    /// The transfer has ended without its item, and its files are being
    /// deleted ([`discard`](Self::discard)): a later start finishes that
    /// instead of continuing the transfer.
    #[serde(default)]
    pub ended: bool,
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
    /// cut the end of the file may never have been written. Fails if the
    /// part is not there (see [`part_is_there`](Self::part_is_there)).
    pub fn truncate_to_synced(&self) -> io::Result<()> {
        if let Side::Receive {
            part_path,
            synced: Some(synced),
            ..
        } = &self.side
        {
            if !self.part_is_there() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    crate::i18n::tr!(error_part_missing),
                ));
            }
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
            Side::Receive { .. } => self.part_is_there(),
        }
    }

    /// The identity of a receive's part file, if known: noted when the part
    /// was created, or with the name it was being renamed to.
    fn part_identity(&self) -> Option<FileId> {
        match &self.side {
            Side::Receive {
                part_id, moving, ..
            } => part_id.or(moving.as_ref().map(|m| m.id)),
            Side::Send { .. } => None,
        }
    }

    /// The part file of a receive as an [`Owned`], if its identity is known.
    fn owned_part(&self) -> Option<Owned> {
        match &self.side {
            Side::Receive { part_path, .. } => self.part_identity().map(|id| Owned {
                path: part_path.clone(),
                id,
            }),
            Side::Send { .. } => None,
        }
    }

    /// Whether the part file of a receive is there: the same file as when it
    /// was created, or (in records of earlier versions) anything of its name.
    pub fn part_is_there(&self) -> bool {
        match (&self.side, self.owned_part()) {
            (Side::Receive { .. }, Some(part)) => part.is_there(),
            (Side::Receive { part_path, .. }, None) => part_path.exists(),
            (Side::Send { .. }, _) => false,
        }
    }

    /// Deletes the part file of a receive if it is still this transfer's
    /// (see [`Owned::remove_file`]); whether it is gone.
    fn remove_part(&self) -> bool {
        match (&self.side, self.owned_part()) {
            (Side::Receive { .. }, Some(part)) => part.remove_file(),
            (Side::Receive { part_path, .. }, None) => remove_file(part_path),
            (Side::Send { .. }, _) => true,
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
            Side::Receive {
                part_path,
                leftovers,
                ..
            } => {
                self.remove(data_dir);
                if self.part_is_temporary() {
                    let _ = fs::remove_file(part_path);
                }
                // Once: the record that would keep trying is gone.
                for folder in leftovers {
                    folder.remove_folder();
                }
            }
            Side::Send { .. } => self.discard(data_dir),
        }
    }

    /// Ends the transfer without its item: deletes the partial or temporary
    /// data, and the folders a received folder was being unpacked into,
    /// each only while it is still this transfer's. The record is marked as
    /// ended first and goes last, so that a crash in between, or a file that
    /// cannot be deleted now, leaves it for the next start to finish the
    /// deletion, never to continue the transfer.
    pub fn discard(&self, data_dir: &Path) {
        if !self.ended {
            let mut ended = self.clone();
            ended.ended = true;
            let _ = ended.save(data_dir);
        }
        let gone = match &self.side {
            Side::Send {
                source, data_path, ..
            } => data_path == source || remove_file(data_path),
            Side::Receive {
                unpacking,
                leftovers,
                ..
            } => {
                let part = self.remove_part();
                // Every one is tried, whether or not the others go.
                let kept = unpacking
                    .iter()
                    .chain(leftovers)
                    .filter(|folder| !folder.remove_folder())
                    .count();
                part && kept == 0
            }
        };
        if gone {
            self.remove(data_dir);
        }
    }

    /// What the last step of a receive works with (`None` for a send).
    pub(crate) fn receive_files(&self) -> Option<ReceiveFiles> {
        match &self.side {
            Side::Receive {
                part_path,
                unpacking,
                moving,
                leftovers,
                ..
            } => Some(ReceiveFiles {
                part: part_path.clone(),
                unpacking: unpacking.clone(),
                moving: moving.clone(),
                leftovers: leftovers.clone(),
            }),
            Side::Send { .. } => None,
        }
    }

    fn set_receive_files(&mut self, files: &ReceiveFiles) {
        if let Side::Receive {
            part_path,
            unpacking,
            moving,
            leftovers,
            ..
        } = &mut self.side
        {
            part_path.clone_from(&files.part);
            unpacking.clone_from(&files.unpacking);
            moving.clone_from(&files.moving);
            leftovers.clone_from(&files.leftovers);
        }
    }

    /// After a restart: if the part file was renamed before the program
    /// ended, but the record not yet changed with it (see
    /// `Side::Receive::moving`), the record follows it. Whether it did.
    pub fn follow_moved_part(&mut self) -> bool {
        if let Side::Receive {
            part_path,
            moving: Some(moving),
            ..
        } = &mut self.side
            && *part_path != moving.path
            && moving.is_there()
        {
            part_path.clone_from(&moving.path);
            return true;
        }
        false
    }

    /// After a restart: the received item, if it was in place already and
    /// only the end of the transfer was interrupted (a file, or a folder kept
    /// only as its zip, whose part had got its final name and is still
    /// there, not something else of that name).
    pub fn placed_item(&self) -> Option<&Path> {
        let files = self.receive_files()?;
        let single = self.offer.kind == Kind::File
            || self.received_folder() == Some(ReceivedFolder::KeepZip);
        match &self.side {
            Side::Receive { part_path, .. }
                if single && files.part_in_place() && self.part_is_there() =>
            {
                Some(part_path)
            }
            _ => None,
        }
    }
}

/// Remembers that a transfer ended while the other side could not be told.
/// A transfer that completed stays completed: a later report that it ended
/// (after a restart that could not tell, say) does not change that.
pub fn write_tombstone(data_dir: &Path, token: &Token, completed: bool) {
    if !completed && tombstone(data_dir, token) == Some(true) {
        return;
    }
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
            return Err(io::Error::new(io::ErrorKind::Interrupted, Cancelled));
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

/// Runs blocking file work (saving a record syncs it to disk, deleting a
/// folder can take long) on a thread of its own instead of one of the
/// runtime's worker threads, and waits for it.
pub(crate) async fn off_runtime<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => match error.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            // The runtime is shutting down: nothing waits for this any more.
            Err(_) => std::future::pending().await,
        },
    }
}

/// How far a running transfer has come, as far as ending it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// A cancel ends the transfer.
    Running,
    /// The last step of a receive runs (see [`Session::finishing`]): a
    /// cancel is left to it.
    Finishing,
    /// The item is the receiver's. On the receiving side, it is being put in
    /// place or is in place ([`Session::place`]); on the sending side, the
    /// receiver has confirmed all of it ([`Session::hand_over`]). A cancel
    /// comes too late.
    HandedOver,
}

/// What a session's lock guards: the record and the stage, which change
/// together.
struct State {
    /// The record on disk, once the transfer was accepted; taken when the
    /// transfer ends.
    record: Option<TransferRecord>,
    stage: Stage,
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
    state: Mutex<State>,
    /// The receiver is writing data to the part file, or getting it onto the
    /// disk and into the record after a connection ended. Shutdown waits for
    /// that, so that the next start continues from everything received.
    pub syncing: AtomicBool,
}

/// The last step of a receive runs while this exists (see
/// [`Session::finishing`]).
pub(crate) struct Finishing<'a>(&'a Session);

impl Drop for Finishing<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        // Once the item is the receiver's, it stays so.
        if state.stage == Stage::Finishing {
            state.stage = Stage::Running;
        }
    }
}

/// A transfer that has ended on this side, with its tombstone and files
/// still to see to: [`forget`](Self::forget) leaves the tombstone and
/// deletes what goes, which is file work that can take a while.
#[must_use]
pub(crate) struct Ending {
    /// Boxed: much larger than the rest of what a cancel returns.
    record: Option<Box<TransferRecord>>,
    completed: bool,
}

impl Ending {
    /// Nothing to see to: the transfer had no record (yet).
    pub fn nothing() -> Self {
        Ending {
            record: None,
            completed: false,
        }
    }

    pub fn forget_blocking(self, data_dir: &Path) {
        forget(data_dir, self.record.map(|r| *r), self.completed);
    }

    /// Like [`forget_blocking`](Self::forget_blocking), off the runtime's
    /// worker threads.
    pub async fn forget(self, data_dir: &Path) {
        if self.record.is_some() {
            let data_dir = data_dir.to_path_buf();
            off_runtime(move || self.forget_blocking(&data_dir)).await;
        }
    }
}

/// What a cancel did (see [`Session::cancel`]).
#[must_use]
pub(crate) enum Cancel {
    /// The transfer has ended.
    Ended(Ending),
    /// The last step runs and will end the transfer once it has stopped. The
    /// record is marked as ended, which is still to be saved
    /// ([`Session::save_record`]).
    LeftToStep,
    /// Too late: the transfer ends by itself, completed.
    TooLate,
}

impl Session {
    pub fn new(
        id: u64,
        token: Token,
        peer: EndpointId,
        role: Role,
    ) -> (Arc<Session>, mpsc::UnboundedReceiver<Streams>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = Session {
            id,
            token,
            peer,
            role,
            new_conn: tx,
            current: Mutex::new(None),
            state: Mutex::new(State {
                record: None,
                stage: Stage::Running,
            }),
            syncing: AtomicBool::new(false),
        };
        (Arc::new(session), rx)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// The transfer was accepted: from now on it has this record.
    pub fn set_record(&self, record: TransferRecord) {
        self.state().record = Some(record);
    }

    /// `f` of the record, if the transfer has one (and has not ended).
    pub fn with_record<T>(&self, f: impl FnOnce(&TransferRecord) -> T) -> Option<T> {
        self.state().record.as_ref().map(f)
    }

    /// Runs `change` on the session off the runtime's worker threads (see
    /// [`off_runtime`]): saving the record syncs it to disk.
    pub async fn update(
        self: &Arc<Self>,
        data_dir: &Path,
        change: impl FnOnce(&Session, &Path) + Send + 'static,
    ) {
        let (session, data_dir) = (self.clone(), data_dir.to_path_buf());
        off_runtime(move || change(&session, &data_dir)).await;
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
        if let Some(record) = self.state().record.as_mut() {
            record.paused = paused;
            let _ = record.save(data_dir);
        }
    }

    /// Remembers how much of the part file is on disk.
    pub fn save_synced(&self, data_dir: &Path, on_disk: u64) {
        if let Some(record) = self.state().record.as_mut()
            && let Side::Receive { synced, .. } = &mut record.side
        {
            *synced = Some(on_disk);
            let _ = record.save(data_dir);
        }
    }

    /// Remembers the sender's final hash (see `TransferRecord::done_hash`).
    pub fn save_done_hash(&self, data_dir: &Path, hash: [u8; 32]) {
        if let Some(record) = self.state().record.as_mut() {
            record.done_hash = Some(hash);
            let _ = record.save(data_dir);
        }
    }

    /// Saves the record as it is now (after a cancel left to the last step,
    /// say).
    pub fn save_record(&self, data_dir: &Path) {
        if let Some(record) = self.state().record.as_ref() {
            let _ = record.save(data_dir);
        }
    }

    /// The files the last step of a receive works with.
    pub fn receive_files(&self) -> Option<ReceiveFiles> {
        self.state()
            .record
            .as_ref()
            .and_then(TransferRecord::receive_files)
    }

    /// Changes the files of a receive in the record and saves it, so that
    /// the record always knows them. Fails if the record could not be saved
    /// (the change is made in memory all the same), and with [`Cancelled`]
    /// once the transfer has ended or is to end (a cancel left to the last
    /// step): the step stops then, before it does anything more.
    pub fn save_receive_files(&self, data_dir: &Path, files: &ReceiveFiles) -> io::Result<()> {
        let mut state = self.state();
        let record = state
            .record
            .as_mut()
            .ok_or_else(|| io::Error::other(Cancelled))?;
        record.set_receive_files(files);
        let saved = record.save(data_dir);
        if record.ended {
            return Err(io::Error::other(Cancelled));
        }
        saved
    }

    /// Starts the last step of a receive (unpacking a folder, then putting
    /// the item in place), which runs while the result exists. Meanwhile a
    /// cancel is left to the step: it notices the cancel, removes what it
    /// made and fails with [`Cancelled`], and then the transfer ends as
    /// cancelled (see [`cancel`](Self::cancel)). Fails if the transfer has
    /// ended already.
    pub fn finishing(&self) -> anyhow::Result<Finishing<'_>> {
        let mut state = self.state();
        if !state.record.as_ref().is_some_and(|r| !r.ended) {
            anyhow::bail!(Cancelled);
        }
        state.stage = Stage::Finishing;
        Ok(Finishing(self))
    }

    /// Ends the transfer on this side: closes the connection (with `code`, if
    /// given) and takes the record. What the result's `forget` then does
    /// leaves a tombstone so the other side learns about the end later, and
    /// deletes the partial or temporary data.
    pub fn end(&self, code: Option<u32>, completed: bool) -> Ending {
        match code {
            Some(code) => self.close_current(code, b""),
            None => {
                self.set_current(None);
            }
        }
        let record = self.state().record.take().map(Box::new);
        Ending { record, completed }
    }

    /// The user cancelled. A running transfer ends like with
    /// [`end`](Self::end), with the connection closed as cancelled. While
    /// the last step of a receive runs ([`finishing`](Self::finishing)), the
    /// cancel is left to the step: the record is marked as ended, so that
    /// nothing more is put in place and (once it is saved) a later start
    /// deletes the files instead of finishing them, and the step ends the
    /// transfer once it has stopped. Once the item is the receiver's
    /// ([`place`](Self::place), [`hand_over`](Self::hand_over)), a cancel
    /// comes too late.
    pub fn cancel(&self) -> Cancel {
        let record = {
            let mut state = self.state();
            match state.stage {
                Stage::HandedOver => return Cancel::TooLate,
                Stage::Finishing => {
                    if let Some(record) = state.record.as_mut() {
                        record.ended = true;
                    }
                    return Cancel::LeftToStep;
                }
                Stage::Running => state.record.take().map(Box::new),
            }
        };
        self.close_current(CLOSE_CANCELLED, b"");
        Cancel::Ended(Ending {
            record,
            completed: false,
        })
    }

    /// [`cancel`](Self::cancel) with everything it leaves to do, on this
    /// thread; whether it ended the transfer.
    #[cfg(test)]
    pub fn cancel_blocking(&self, data_dir: &Path) -> bool {
        match self.cancel() {
            Cancel::Ended(ending) => {
                ending.forget_blocking(data_dir);
                true
            }
            Cancel::LeftToStep => {
                self.save_record(data_dir);
                false
            }
            Cancel::TooLate => false,
        }
    }

    /// The receiver has confirmed that all of the item arrived intact
    /// (sending side): it is the receiver's from now on, to put in place or
    /// to cancel, and a cancel here comes too late.
    pub fn hand_over(&self) {
        self.state().stage = Stage::HandedOver;
    }

    /// Whether the item is the receiver's (see [`hand_over`](Self::hand_over)).
    pub fn handed_over(&self) -> bool {
        self.state().stage == Stage::HandedOver
    }

    /// Puts a received item in place with `step` (renaming the part file,
    /// say) and ends the transfer as completed right away: from then on the
    /// part's old name may belong to another transfer, and neither a late
    /// cancel nor a crash may treat it as this transfer's data. A cancel that
    /// comes while `step` runs is too late (see [`cancel`](Self::cancel));
    /// one that came before has ended the transfer, or marked it as ended,
    /// and then `step` does not run. Blocking.
    pub fn place(
        &self,
        data_dir: &Path,
        step: &mut dyn FnMut() -> anyhow::Result<PathBuf>,
    ) -> anyhow::Result<PathBuf> {
        let before = {
            let mut state = self.state();
            if !state.record.as_ref().is_some_and(|r| !r.ended) {
                anyhow::bail!(Cancelled);
            }
            std::mem::replace(&mut state.stage, Stage::HandedOver)
        };
        let placed = step();
        let record = {
            let mut state = self.state();
            if placed.is_err() {
                // Still running: it can be cancelled, or end as failed.
                state.stage = before;
                return placed;
            }
            state.record.take()
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
            ended: false,
        }
    }

    fn receive_side(dir: &Path, part: &Path, synced: Option<u64>) -> Side {
        Side::Receive {
            dest_dir: dir.to_path_buf(),
            part_path: part.to_path_buf(),
            synced,
            part_id: None,
            folder: ReceivedFolder::Extract,
            unpacking: None,
            moving: None,
            leftovers: Vec::new(),
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
        session.set_record(record(1, receive_side(&dir, &part, None)));
        assert!(session.cancel_blocking(&dir));
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
        session.set_record(r);
        let placed = session
            .place(&dir, &mut || {
                assert!(!session.cancel_blocking(&dir), "too late while placing");
                fs::rename(&part, dir.join("b.bin"))?;
                Ok(dir.join("b.bin"))
            })
            .unwrap();
        assert!(!session.cancel_blocking(&dir));
        assert_eq!(fs::read(&placed).unwrap(), b"data");
        assert_eq!(tombstone(&dir, &[2; 16]), Some(true));
        assert!(load_all(&dir).is_empty());

        // A step that fails leaves the transfer running, so it can still
        // be cancelled (or ended as failed).
        let (session, _rx) = Session::new(3, [3; 16], peer, Role::Receive);
        session.set_record(record(3, receive_side(&dir, &dir.join("c.part"), None)));
        assert!(
            session
                .place(&dir, &mut || anyhow::bail!("blocked"))
                .is_err()
        );
        assert!(session.cancel_blocking(&dir));
        fs::remove_dir_all(&dir).ok();
    }

    /// While the last step runs, a cancel is left to it: the record is
    /// marked as ended at once (a crash now deletes, never finishes), and
    /// nothing is put in place any more; the transfer ends when the step has
    /// stopped.
    #[test]
    fn a_cancel_during_the_last_step_is_left_to_it() {
        let dir = scratch("deferred");
        let peer = iroh::SecretKey::generate().public();
        let part = dir.join("a.bin.part");
        fs::write(&part, b"data").unwrap();
        let (session, _rx) = Session::new(1, [1; 16], peer, Role::Receive);
        let r = record(1, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        session.set_record(r);
        let finishing = session.finishing().unwrap();
        assert!(!session.cancel_blocking(&dir), "left to the step");
        assert!(part.exists());
        assert!(load_all(&dir)[0].ended);
        // The step learns about it when it notes its next change, which the
        // record keeps all the same, so that it always knows the files.
        let mut files = session.receive_files().unwrap();
        files.moving = Some(Owned {
            path: dir.join("a.bin"),
            id: archive::file_id(&part).unwrap(),
        });
        let error = session.save_receive_files(&dir, &files).unwrap_err();
        assert!(archive::is_cancelled(&error.into()));
        assert_eq!(load_all(&dir)[0].receive_files().unwrap(), files);
        assert!(session.place(&dir, &mut || Ok(dir.join("a.bin"))).is_err());
        drop(finishing);
        assert!(session.finishing().is_err(), "it has ended");
        session.end(None, false).forget_blocking(&dir);
        assert!(!part.exists());
        assert!(load_all(&dir).is_empty());
        assert_eq!(tombstone(&dir, &[1; 16]), Some(false));
        fs::remove_dir_all(&dir).ok();
    }

    /// Discarding a receive deletes the folder it was unpacking into, but
    /// only while that is the same folder; what cannot be deleted now keeps
    /// the record, marked as ended, for the next start.
    #[test]
    fn discarding_deletes_only_its_own_files() {
        let dir = scratch("discard");
        let with_folder = |token: u8, part: &Path, folder: &Path| {
            let mut r = record(token, receive_side(&dir, part, None));
            if let Side::Receive { unpacking, .. } = &mut r.side {
                *unpacking = Some(Owned::at(folder).unwrap());
            }
            r.save(&dir).unwrap();
            r
        };

        // Its own folder, half unpacked.
        let part = dir.join("Photos.zip.part");
        fs::write(&part, b"zip").unwrap();
        let partial = dir.join("Photos");
        fs::create_dir_all(partial.join("sub")).unwrap();
        fs::write(partial.join("sub").join("half.jpg"), b"ha").unwrap();
        let r = with_folder(10, &part, &partial);
        r.discard(&dir);
        assert!(!partial.exists() && !part.exists());
        assert!(load_all(&dir).is_empty());

        // The user deleted it and made a folder of the same name.
        fs::write(&part, b"zip").unwrap();
        fs::create_dir(&partial).unwrap();
        let r = with_folder(11, &part, &partial);
        fs::remove_dir(&partial).unwrap();
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("mine.txt"), b"mine").unwrap();
        r.discard(&dir);
        assert_eq!(fs::read(partial.join("mine.txt")).unwrap(), b"mine");
        assert!(!part.exists());
        assert!(load_all(&dir).is_empty());

        // A part that another program holds open cannot be deleted yet.
        fs::write(&part, b"zip").unwrap();
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&part)
                .unwrap()
        };
        let r = record(12, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        r.discard(&dir);
        let left = load_all(&dir);
        assert_eq!(left.len(), 1);
        assert!(left[0].ended, "the next start deletes it");
        drop(held);
        left[0].discard(&dir);
        assert!(!part.exists());
        assert!(load_all(&dir).is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    /// After a crash right after the part was renamed, the record follows
    /// it there, and not a new file under the part's old name.
    #[test]
    fn a_moved_part_is_followed() {
        let dir = scratch("moved");
        let part = dir.join("a.bin.part");
        fs::write(&part, b"data").unwrap();
        let placed = dir.join("a.bin");
        let mut r = record(13, receive_side(&dir, &part, None));
        if let Side::Receive { moving, .. } = &mut r.side {
            *moving = Some(Owned {
                path: placed.clone(),
                id: archive::file_id(&part).unwrap(),
            });
        }
        // Noted, but not renamed yet: nothing to follow.
        assert!(!r.follow_moved_part());
        assert_eq!(r.placed_item(), None);

        fs::rename(&part, &placed).unwrap();
        fs::write(&part, b"another transfer").unwrap();
        assert!(r.follow_moved_part());
        assert_eq!(r.placed_item(), Some(placed.as_path()));
        assert!(!r.follow_moved_part(), "once");

        // A folder kept with its zip is unpacked after its zip was moved.
        let mut both = r.clone();
        both.offer.kind = Kind::Folder;
        if let Side::Receive { folder, .. } = &mut both.side {
            *folder = ReceivedFolder::Both;
        }
        assert_eq!(both.placed_item(), None);
        fs::remove_dir_all(&dir).ok();
    }

    /// What has the part's name is the part only while it is the same file:
    /// a later start neither cuts nor completes nor deletes something else
    /// that took the name, such as a download saved under the name of a
    /// kept zip that the user had deleted.
    #[test]
    fn only_its_own_part_is_touched() {
        let dir = scratch("identity");
        let zip = dir.join("Photos (2).zip");
        fs::write(&zip, b"received").unwrap();
        let mut r = record(14, receive_side(&dir, &zip, Some(3)));
        r.offer.kind = Kind::Folder;
        if let Side::Receive {
            folder,
            part_id,
            moving,
            ..
        } = &mut r.side
        {
            *folder = ReceivedFolder::KeepZip;
            *part_id = Some(archive::file_id(&zip).unwrap());
            *moving = Some(Owned::at(&zip).unwrap());
        }
        assert!(r.files_present());
        assert_eq!(r.placed_item(), Some(zip.as_path()));

        fs::remove_file(&zip).unwrap();
        fs::write(&zip, b"a download").unwrap();
        assert!(!r.files_present());
        assert_eq!(r.placed_item(), None);
        assert!(r.truncate_to_synced().is_err());
        r.save(&dir).unwrap();
        r.discard(&dir);
        assert_eq!(fs::read(&zip).unwrap(), b"a download");
        assert!(load_all(&dir).is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    /// Folders that could not be deleted yet go with the transfer: a failed
    /// one keeps its record until they are gone, a completed one tries once.
    #[test]
    fn leftover_folders_go_with_the_transfer() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("leftovers");
        let part = dir.join("recv.zip");
        let old = dir.join("Photos");
        let with_leftover = |token: u8| {
            fs::create_dir_all(&old).unwrap();
            fs::write(old.join("half.jpg"), b"ha").unwrap();
            fs::write(&part, b"zip").unwrap();
            let mut r = record(token, receive_side(&dir, &part, None));
            r.offer.kind = Kind::Folder;
            if let Side::Receive { leftovers, .. } = &mut r.side {
                leftovers.push(Owned::at(&old).unwrap());
            }
            r.save(&dir).unwrap();
            r
        };

        let r = with_leftover(15);
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(old.join("half.jpg"))
            .unwrap();
        r.discard(&dir);
        assert!(load_all(&dir)[0].ended, "the next start tries again");
        drop(held);
        load_all(&dir)[0].discard(&dir);
        assert!(!old.exists() && !part.exists());
        assert!(load_all(&dir).is_empty());

        let r = with_leftover(16);
        r.complete(&dir);
        assert!(!old.exists() && !part.exists());
        assert!(load_all(&dir).is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    /// Once the receiver has confirmed all of the item, a cancel on the
    /// sending side comes too late; the transfer still ends as usual.
    #[test]
    fn a_cancel_after_the_hand_over_is_too_late() {
        let dir = scratch("handed");
        let peer = iroh::SecretKey::generate().public();
        let zip = dir.join("send.zip");
        fs::write(&zip, b"zip").unwrap();
        let (session, _rx) = Session::new(1, [17; 16], peer, Role::Send);
        session.set_record(record(
            17,
            Side::Send {
                source: dir.join("Photos"),
                source_len: 3,
                source_mtime: None,
                data_path: zip.clone(),
            },
        ));
        assert!(!session.handed_over());
        session.hand_over();
        assert!(!session.cancel_blocking(&dir));
        assert!(zip.exists() && session.with_record(|_| ()).is_some());
        session.end(None, true).forget_blocking(&dir);
        assert!(!zip.exists());
        assert_eq!(tombstone(&dir, &[17; 16]), Some(true));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_of_earlier_versions_still_load() {
        let dir = scratch("legacy");
        let part = dir.join("old.bin.part");
        fs::write(&part, b"12345").unwrap();
        // A record as written before `synced`, `done_hash`, `folder`,
        // `unpacking`, `moving`, `part_id`, `leftovers`, `ended` and the
        // offer's times existed.
        let mut side = receive_side(&dir, &part, None);
        if let Side::Receive { folder, .. } = &mut side {
            *folder = ReceivedFolder::Both;
        }
        let mut json = serde_json::to_value(record(7, side)).unwrap();
        json.as_object_mut().unwrap().remove("done_hash");
        json.as_object_mut().unwrap().remove("ended");
        let receive = json["side"]["receive"].as_object_mut().unwrap();
        for field in [
            "synced",
            "folder",
            "unpacking",
            "moving",
            "part_id",
            "leftovers",
        ] {
            receive.remove(field);
        }
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
        assert!(loaded[0].files_present(), "without an identity, the name");

        // The version before noted the folder being unpacked into by its
        // path only: it is the folder of that name.
        let partial = dir.join("Photos");
        fs::create_dir(&partial).unwrap();
        for (token, folder) in [(9, &partial), (10, &dir.join("gone"))] {
            let mut json =
                serde_json::to_value(record(token, receive_side(&dir, &part, None))).unwrap();
            let receive = json["side"]["receive"].as_object_mut().unwrap();
            receive.insert("unpacking".into(), serde_json::json!(folder));
            for field in ["moving", "part_id", "leftovers"] {
                receive.remove(field);
            }
            fs::write(record_path(&dir, &[token; 16]), json.to_string()).unwrap();
        }
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 3);
        let unpacking = |token: u8| {
            let r = loaded.iter().find(|r| r.token == [token; 16]).unwrap();
            r.receive_files().unwrap().unpacking
        };
        assert_eq!(unpacking(9), Some(Owned::at(&partial).unwrap()));
        assert_eq!(unpacking(10), None);
        for token in [9, 10] {
            fs::remove_file(record_path(&dir, &[token; 16])).unwrap();
        }

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
