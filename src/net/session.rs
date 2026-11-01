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
        /// The files that the last step of the receive changes.
        #[serde(flatten)]
        files: ReceiveFiles,
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
    },
}

/// The files of a receive that its last step changes, which it works on as
/// a copy ([`Session::receive_files`]) and notes in the record before each
/// change on disk ([`Session::save_receive_files`]). Stored with the other
/// fields of `Side::Receive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiveFiles {
    /// The `.part` file. For a folder that is only unpacked, its zip in the
    /// temporary folder (see [`temporary_part`]); for a folder whose zip is
    /// kept, `<name>.zip.part` in the destination folder. Once it has been
    /// renamed to its final name (see `moving`), the item itself.
    #[serde(rename = "part_path")]
    pub part: PathBuf,
    /// The folder that a received folder is being unpacked into, noted
    /// before anything is written to it. It is complete only once the
    /// record is gone: a later start deletes it and unpacks the archive
    /// again into a new folder, and ending the transfer otherwise deletes
    /// it. Records of earlier versions have only its path (see
    /// [`unpacking_of_any_version`]).
    #[serde(default, deserialize_with = "unpacking_of_any_version")]
    pub unpacking: Option<Owned>,
    /// Where the part file is being renamed to, with the part's identity:
    /// noted before each attempt and cleared when one fails, so that after a
    /// crash right after the rename the record finds the part again
    /// ([`TransferRecord::follow_moved_part`]) and never takes a new file
    /// under the part's old name for it.
    #[serde(default)]
    pub moving: Option<Owned>,
    /// Folders of the transfer that have to go but could not be deleted
    /// yet, such as an incomplete one of an earlier run in which another
    /// program holds a file open. Ending the transfer deletes them: a
    /// completed one tries once, a failed or cancelled one keeps its
    /// record until they are gone.
    #[serde(default)]
    pub leftovers: Vec<Owned>,
}

impl ReceiveFiles {
    /// The files of a new receive, whose part is at `part`.
    pub fn new(part: PathBuf) -> Self {
        ReceiveFiles {
            part,
            unpacking: None,
            moving: None,
            leftovers: Vec::new(),
        }
    }

    /// Whether the part file has its final name already.
    pub fn part_in_place(&self) -> bool {
        self.moving.as_ref().is_some_and(|m| m.path == self.part)
    }
}

/// Reads `ReceiveFiles::unpacking` as any version wrote it. Earlier versions
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
    /// Sending side: all of the data and `Done` have gone out. The receiver
    /// may have all of it, and the item is the receiver's once it has
    /// checked it, so until the sender learns the receiver's verdict a cancel
    /// waits for it (see [`Session::defer_cancel`]), also after a restart.
    #[serde(default)]
    pub done_sent: bool,
    /// Sending side: the user cancelled while the cancel waited for the
    /// receiver's verdict (see [`Session::defer_cancel`]). It goes on
    /// waiting after a restart, and ends the transfer as cancelled unless
    /// the receiver has the item by then (see
    /// [`cancel_pending`](Self::cancel_pending)). A cancelled send is never
    /// `paused`.
    #[serde(default)]
    pub cancelled: bool,
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
    /// file, at most what is known to be flushed; sender: all of them once
    /// the receiver confirmed them, otherwise none known).
    pub fn have(&self) -> u64 {
        match &self.side {
            Side::Receive { files, synced, .. } => {
                let len = fs::metadata(&files.part).map(|m| m.len()).unwrap_or(0);
                synced.map_or(len, |s| s.min(len))
            }
            Side::Send { .. } if self.done_hash.is_some() => self.offer.size,
            Side::Send { .. } => 0,
        }
    }

    /// Cuts the part file back to what is known to be on disk: after a power
    /// cut the end of the file may never have been written. Fails if the
    /// part is not there (see [`part_is_there`](Self::part_is_there)).
    pub fn truncate_to_synced(&self) -> io::Result<()> {
        if let Side::Receive {
            files,
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
            let len = fs::metadata(&files.part)?.len();
            if len > *synced {
                fs::OpenOptions::new()
                    .write(true)
                    .open(&files.part)?
                    .set_len(*synced)?;
            }
        }
        Ok(())
    }

    /// Sending side: the user cancelled, and the cancel still waits for the
    /// receiver's verdict: a later start cancels the transfer again (see
    /// [`cancelled`](Self::cancelled)). Not once the receiver has confirmed
    /// the item, when the cancel came too late.
    pub fn cancel_pending(&self) -> bool {
        self.role() == Role::Send && self.cancelled && self.done_hash.is_none()
    }

    /// Sending side: all of the data has gone out, so the receiver may have
    /// the item (`done_sent`), or it has confirmed that (`done_hash`). The
    /// transfer goes on without the data until the receiver says whether it
    /// needs it again.
    pub fn receiver_may_have_item(&self) -> bool {
        self.role() == Role::Send && (self.done_sent || self.done_hash.is_some())
    }

    /// Receiving side: all of the item arrived and matched the sender's
    /// hash (`done_hash`), so the transfer finishes without the sender,
    /// however long it was away; unless it has ended.
    fn finishes_alone(&self) -> bool {
        self.role() == Role::Receive && self.done_hash.is_some() && !self.ended
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
            Side::Receive { part_id, files, .. } => part_id.or(files.moving.as_ref().map(|m| m.id)),
            Side::Send { .. } => None,
        }
    }

    /// The part file of a receive as an [`Owned`], if its identity is known.
    fn owned_part(&self) -> Option<Owned> {
        let path = &self.receive_files()?.part;
        self.part_identity().map(|id| Owned {
            path: path.clone(),
            id,
        })
    }

    /// Whether the part file of a receive is there: the same file as when it
    /// was created, or (in records of earlier versions) anything of its name.
    pub fn part_is_there(&self) -> bool {
        match (self.receive_files(), self.owned_part()) {
            (Some(_), Some(part)) => part.is_there(),
            (Some(files), None) => files.part.exists(),
            (None, _) => false,
        }
    }

    /// Deletes the part file of a receive if it is still this transfer's
    /// (see [`Owned::remove_file`]); whether it is gone.
    fn remove_part(&self) -> bool {
        match (self.receive_files(), self.owned_part()) {
            (Some(_), Some(part)) => part.remove_file(),
            (Some(files), None) => remove_file(&files.part),
            (None, _) => true,
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
    /// part is. A record that cannot be deleted now is completed again at
    /// the next start, and never continued or discarded: the tombstone,
    /// which is written first, says that the transfer completed (see
    /// [`load_all`]).
    pub fn complete(&self, data_dir: &Path) {
        match &self.side {
            Side::Receive { files, .. } => {
                self.remove(data_dir);
                if self.part_is_temporary() {
                    let _ = fs::remove_file(&files.part);
                }
                // Once: the record that would keep trying is gone.
                for folder in &files.leftovers {
                    folder.remove_folder();
                }
            }
            // No need to mark the record as ended first (see `discard`): one
            // that is left completes at the next start.
            Side::Send { .. } => {
                if self.delete_files() {
                    self.remove(data_dir);
                }
            }
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
        if self.delete_files() {
            self.remove(data_dir);
        }
    }

    /// Deletes what a transfer that ends without its item leaves behind (see
    /// [`discard`](Self::discard)); whether all of it is gone.
    fn delete_files(&self) -> bool {
        match &self.side {
            Side::Send {
                source, data_path, ..
            } => data_path == source || remove_file(data_path),
            Side::Receive { files, .. } => {
                let part = self.remove_part();
                // Every one is tried, whether or not the others go.
                let kept = files
                    .unpacking
                    .iter()
                    .chain(&files.leftovers)
                    .filter(|folder| !folder.remove_folder())
                    .count();
                part && kept == 0
            }
        }
    }

    /// What the last step of a receive works with (`None` for a send).
    pub fn receive_files(&self) -> Option<&ReceiveFiles> {
        match &self.side {
            Side::Receive { files, .. } => Some(files),
            Side::Send { .. } => None,
        }
    }

    fn receive_files_mut(&mut self) -> Option<&mut ReceiveFiles> {
        match &mut self.side {
            Side::Receive { files, .. } => Some(files),
            Side::Send { .. } => None,
        }
    }

    /// After a restart: if the part file was renamed before the program
    /// ended, but the record not yet changed with it (see
    /// [`ReceiveFiles::moving`]), the record follows it. Whether it did.
    ///
    /// The part has moved if its identity has the new name. On FAT and
    /// exFAT, where every item of a volume has the same identity, anything
    /// under either name counts as the part, such as a file of the new name
    /// that appeared after a crash before the rename, or another transfer's
    /// part that got the old name after a crash right after it. There the
    /// sizes decide: the part is renamed only in the last step, when it has
    /// all of the item, so it has moved if what has the new name is as large
    /// as the item and what has the old name (if anything) is not. When
    /// both are, the other records tell first: the part has moved if another
    /// record of this profile names the old name as its own part (`parts`:
    /// the token and the part of every record read at this start), since
    /// another transfer could only take the name once this one's rename had
    /// freed it. Otherwise the content tells, against what the sender sent
    /// (`done_hash`, known since before the last step): what has the old
    /// name is the part if it hashes to that (also when what has the new
    /// name does too, an identical copy of the item received earlier, say:
    /// this transfer keeps its part, which then gets a free name, and no
    /// renamed part is left behind as an orphan), and otherwise what has
    /// the new name is the part if it hashes to that (what has the old name
    /// is then something else's, such as the part of another profile that
    /// receives into the same folder). Reading the files is blocking, and
    /// rare: only this twice-ambiguous case does it.
    pub fn follow_moved_part(&mut self, parts: &[(Token, PathBuf)]) -> bool {
        let Some(files) = self.receive_files() else {
            return false;
        };
        let Some(moving) = files.moving.clone().filter(|m| m.path != files.part) else {
            return false;
        };
        let full = |path: &Path| {
            fs::symlink_metadata(path)
                .ok()
                .filter(fs::Metadata::is_file)
                .is_some_and(|m| m.len() == self.offer.size)
        };
        let claimed = |path: &Path| {
            parts
                .iter()
                .any(|(token, part)| *token != self.token && part == path)
        };
        let has_content = |path: &Path| {
            self.done_hash
                .is_some_and(|hash| hash_file(path).is_ok_and(|got| got == hash))
        };
        let moved = moving.is_there()
            && (moving.id.tells_apart()
                || (full(&moving.path)
                    && (!full(&files.part)
                        || claimed(&files.part)
                        || (!has_content(&files.part) && has_content(&moving.path)))));
        if !moved {
            return false;
        }
        if let Some(files) = self.receive_files_mut() {
            files.part = moving.path;
        }
        true
    }

    /// After a restart: the received item, if it was in place already and
    /// only the end of the transfer was interrupted (a file, or a folder kept
    /// only as its zip, whose part had got its final name and is still
    /// there, not something else of that name).
    pub fn placed_item(&self) -> Option<&Path> {
        let files = self.receive_files()?;
        let single = self.offer.kind == Kind::File
            || self.received_folder() == Some(ReceivedFolder::KeepZip);
        (single && files.part_in_place() && self.part_is_there()).then_some(files.part.as_path())
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

/// The unfinished transfers from earlier runs. On the way, the records of
/// transfers that completed are forgotten (their tombstone says so; only the
/// record, or something temporary, could not be deleted then), old records
/// go with their partial data, and old tombstones go. A receive that has all
/// of its item never gets old: it finishes without the sender (its item may
/// even be in place already, see [`TransferRecord::placed_item`]).
pub fn load_all(data_dir: &Path) -> Vec<TransferRecord> {
    let mut records = Vec::new();
    let Ok(entries) = fs::read_dir(records_dir(data_dir)) else {
        return records;
    };
    let (json, other): (Vec<PathBuf>, Vec<PathBuf>) = entries
        .flatten()
        .map(|entry| entry.path())
        .partition(|path| path.extension().and_then(|e| e.to_str()) == Some("json"));
    // The records first: whether a transfer completed is in its tombstone,
    // which may be about to go.
    let read = json
        .into_iter()
        .map(|path| {
            let record = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<TransferRecord>(&text).ok());
            (path, record)
        })
        .collect::<Vec<_>>();
    // Whose part is under which name, for `follow_moved_part`: all the
    // records read, before any of them is followed or deleted.
    let parts = parts_of(read.iter().filter_map(|(_, record)| record.as_ref()));
    for (path, record) in read {
        match record {
            // Its item is in place: nothing of it may be deleted.
            Some(record) if tombstone(data_dir, &record.token) == Some(true) => {
                record.complete(data_dir);
            }
            Some(mut record) if is_old(&path, MAX_AGE) && !record.finishes_alone() => {
                // Its files are where a rename right before the end put them.
                record.follow_moved_part(&parts);
                record.delete_files();
                let _ = fs::remove_file(&path);
            }
            Some(mut record) => {
                // The part file was renamed right before the program ended.
                // Also when the transfer had ended: its files are where the
                // part went, and its old name may belong to another transfer
                // by now.
                if record.follow_moved_part(&parts) {
                    let _ = record.save(data_dir);
                }
                records.push(record);
            }
            None if is_old(&path, MAX_AGE) => {
                let _ = fs::remove_file(&path);
            }
            // A damaged record is kept aside instead of being read again at
            // every start.
            None => {
                let _ = fs::rename(&path, path.with_extension("json.bad"));
            }
        }
    }
    for path in other {
        if is_old(&path, MAX_AGE) {
            let _ = fs::remove_file(&path);
        }
    }
    records.sort_by_key(|r| r.created);
    records
}

/// The part file of every receive among `records`, with the record's token
/// (see [`TransferRecord::follow_moved_part`]).
fn parts_of<'a>(records: impl IntoIterator<Item = &'a TransferRecord>) -> Vec<(Token, PathBuf)> {
    records
        .into_iter()
        .filter_map(|r| Some((r.token, r.receive_files()?.part.clone())))
        .collect()
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
            Side::Receive { files, .. } => files.part.as_path(),
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

/// The blake3 hash of the whole file at `path`. Blocking.
fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let len = fs::metadata(path)?.len();
    let mut hasher = blake3::Hasher::new();
    hash_range(
        path,
        0,
        len,
        &mut hasher,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )?;
    Ok(*hasher.finalize().as_bytes())
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
    joined(&mut tokio::task::spawn_blocking(work)).await
}

/// Waits for what `task` returns. A panic in it goes on here; a task that
/// the runtime dropped because it is shutting down never returns, since
/// nothing waits for it any more.
pub(crate) async fn joined<T>(task: &mut tokio::task::JoinHandle<T>) -> T {
    match task.await {
        Ok(value) => value,
        Err(error) => match error.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(_) => std::future::pending().await,
        },
    }
}

fn lock(mutex: &Mutex<()>) -> MutexGuard<'_, ()> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How far a running transfer has come, as far as ending it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// A cancel ends the transfer.
    Running,
    /// A cancel is left to the transfer's task, which acts on it at its next
    /// step. On the receiving side, the last step runs (see
    /// [`Session::finishing`]) and stops. On the sending side, the receiver
    /// may have all of the item and the sender waits for its verdict, or the
    /// record is being saved for the first time (see
    /// [`Session::defer_cancel`]).
    Finishing,
    /// The item is the receiver's. On the receiving side, it is being put in
    /// place ([`Session::place`]), or it was in place before a restart; on
    /// the sending side, the receiver has confirmed all of it (see
    /// [`Session::hand_over`]). A cancel comes too late.
    HandedOver,
    /// The transfer has ended on this side ([`Session::end`], a cancel, or
    /// once the item is in place): a cancel comes too late and changes
    /// nothing of how it ended.
    Ended,
}

/// What a session's lock guards: the record and the stage, which change
/// together. File work never holds the lock (see [`Session::save_with`]).
struct State {
    /// The record on disk, once the transfer was accepted; taken when the
    /// transfer ends.
    record: Option<TransferRecord>,
    stage: Stage,
    /// A cancel came while the stage was `Finishing`, and was left to the
    /// transfer's task.
    cancel_left: bool,
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
    /// Held while the record is written, or deleted when the transfer ends,
    /// so that the writes happen in the order of the changes and none brings
    /// a record back after the end deleted it. Only threads of their own
    /// take it, never the runtime's worker threads.
    writing: Arc<Mutex<()>>,
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
    /// The session's lock for writing the record (see `Session::writing`).
    writing: Option<Arc<Mutex<()>>>,
}

impl Ending {
    /// Nothing to see to: the transfer had no record (yet).
    pub fn nothing() -> Self {
        Ending {
            record: None,
            completed: false,
            writing: None,
        }
    }

    pub fn forget_blocking(self, data_dir: &Path) {
        // After a save that is still under way, which would otherwise bring
        // the record back once it is deleted.
        let _writing = self.writing.as_deref().map(lock);
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
    /// The transfer's task acts on the cancel at its next step (see
    /// `Stage::Finishing`). On the receiving side, the last step stops and
    /// ends the transfer, and the record is marked as ended; on the sending
    /// side, the transfer ends as cancelled unless the receiver has the item
    /// by then, and the record is marked as cancelled. Either mark is still
    /// to be saved ([`Session::save_record`]), so that a later start does
    /// not lose the cancel.
    LeftToStep,
    /// Too late: the transfer ends by itself, as it would have without the
    /// cancel (completed, once the item is the receiver's).
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
                cancel_left: false,
            }),
            writing: Arc::new(Mutex::new(())),
            syncing: AtomicBool::new(false),
        };
        (Arc::new(session), rx)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn ending(&self, record: Option<TransferRecord>, completed: bool) -> Ending {
        Ending {
            record: record.map(Box::new),
            completed,
            writing: Some(self.writing.clone()),
        }
    }

    /// The transfer was accepted: from now on it has this record.
    pub fn set_record(&self, record: TransferRecord) {
        self.state().record = Some(record);
    }

    /// `f` of the record, if the transfer has one (and has not ended). `f`
    /// runs under the session's lock: no file work in it.
    pub fn with_record<T>(&self, f: impl FnOnce(&TransferRecord) -> T) -> Option<T> {
        self.state().record.as_ref().map(f)
    }

    /// Runs `change` on the session off the runtime's worker threads (see
    /// [`off_runtime`]): saving the record syncs it to disk.
    pub async fn update<T: Send + 'static>(
        self: &Arc<Self>,
        data_dir: &Path,
        change: impl FnOnce(&Session, &Path) -> T + Send + 'static,
    ) -> T {
        let (session, data_dir) = (self.clone(), data_dir.to_path_buf());
        off_runtime(move || change(&session, &data_dir)).await
    }

    pub fn set_current(&self, conn: Option<Connection>) -> Option<Connection> {
        std::mem::replace(&mut self.current.lock().unwrap(), conn)
    }

    pub fn close_current(&self, code: u32, reason: &[u8]) {
        if let Some(conn) = self.set_current(None) {
            conn.close(code.into(), reason);
        }
    }

    /// Changes the record with `change` and saves it; `None` if the transfer
    /// has no record (any more). Blocking, but without the session's lock
    /// during the write: a cancel, or the router, never waits for the disk.
    /// The writes still happen in the order of the changes.
    fn save_with<T>(
        &self,
        data_dir: &Path,
        change: impl FnOnce(&mut TransferRecord) -> T,
    ) -> Option<(T, io::Result<()>)> {
        let _writing = lock(&self.writing);
        let (value, record) = {
            let mut state = self.state();
            let record = state.record.as_mut()?;
            let value = change(record);
            (value, record.clone())
        };
        Some((value, record.save(data_dir)))
    }

    /// Updates the stored pause state of the record. A cancelled send stays
    /// unpaused (see `TransferRecord::cancelled`): the transfer's task may
    /// still note a pause that the cancel has undone meanwhile.
    pub fn save_paused(&self, data_dir: &Path, paused: bool) {
        self.save_with(data_dir, |record| {
            record.paused = paused && !record.cancelled;
        });
    }

    /// Remembers how much of the part file is on disk.
    pub fn save_synced(&self, data_dir: &Path, on_disk: u64) {
        self.save_with(data_dir, |record| {
            if let Side::Receive { synced, .. } = &mut record.side {
                *synced = Some(on_disk);
            }
        });
    }

    /// Remembers the sender's final hash (see `TransferRecord::done_hash`).
    pub fn save_done_hash(&self, data_dir: &Path, hash: [u8; 32]) {
        self.save_with(data_dir, |record| record.done_hash = Some(hash));
    }

    /// Sending side: the receiver asked to continue, so it does not have all
    /// of the item after all, and `Done` goes out again later (see
    /// `TransferRecord::done_sent`, noted by [`done_sent`](Self::done_sent)).
    pub fn clear_done_sent(&self, data_dir: &Path) {
        self.save_with(data_dir, |record| record.done_sent = false);
    }

    /// Saves the record as it is now (the first time, or after a cancel left
    /// to the last step); `None` if the transfer has no record (any more).
    pub fn save_record(&self, data_dir: &Path) -> Option<io::Result<()>> {
        self.save_with(data_dir, |_| ()).map(|((), saved)| saved)
    }

    /// The files the last step of a receive works with.
    pub fn receive_files(&self) -> Option<ReceiveFiles> {
        self.state()
            .record
            .as_ref()
            .and_then(TransferRecord::receive_files)
            .cloned()
    }

    /// Notes `files` in the record of a receive and saves it, so that a later
    /// start, and the end of the transfer, know them. Fails with
    /// [`Cancelled`] once the transfer has ended or is to end (a cancel left
    /// to the last step): the step stops then, before it does anything more.
    /// The note is kept also then. A record that cannot be saved does not
    /// stop the step, which would throw away an item that arrived intact
    /// (the end of the transfer deletes its files): the note is made all the
    /// same, and only a crash before the next save that works loses it.
    pub fn save_receive_files(&self, data_dir: &Path, files: &ReceiveFiles) -> io::Result<()> {
        let ended = self.save_with(data_dir, |record| {
            if let Some(noted) = record.receive_files_mut() {
                noted.clone_from(files);
            }
            record.ended
        });
        match ended {
            Some((false, _)) => Ok(()),
            _ => Err(io::Error::other(Cancelled)),
        }
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
    /// deletes the partial or temporary data. A cancel from now on comes too
    /// late: the transfer has ended the way the caller reports.
    pub fn end(&self, code: Option<u32>, completed: bool) -> Ending {
        match code {
            Some(code) => self.close_current(code, b""),
            None => {
                self.set_current(None);
            }
        }
        let record = {
            let mut state = self.state();
            state.stage = Stage::Ended;
            state.record.take()
        };
        self.ending(record, completed)
    }

    /// The user cancelled. A running transfer ends like with
    /// [`end`](Self::end), with the connection closed as cancelled. While
    /// the last step of a receive runs ([`finishing`](Self::finishing)), the
    /// cancel is left to the step: the record is marked as ended, so that
    /// nothing more is put in place and (once it is saved) a later start
    /// deletes the files instead of finishing them, and the step ends the
    /// transfer once it has stopped. While a send waits for the receiver
    /// ([`defer_cancel`](Self::defer_cancel)), the cancel waits too, and the
    /// record is marked as cancelled, so that it waits on after a restart.
    /// Once the item is the receiver's ([`place`](Self::place),
    /// [`hand_over`](Self::hand_over)), or the transfer has ended, a cancel
    /// comes too late.
    pub fn cancel(&self) -> Cancel {
        let record = {
            let mut state = self.state();
            match state.stage {
                Stage::HandedOver | Stage::Ended => return Cancel::TooLate,
                Stage::Finishing => {
                    state.cancel_left = true;
                    if let Some(record) = state.record.as_mut() {
                        match self.role {
                            Role::Receive => record.ended = true,
                            Role::Send => {
                                record.cancelled = true;
                                // A cancelled send is not paused any more
                                // (see `Control::cancel`); the record says so
                                // at once, in case the run ends before the
                                // transfer's task notes it.
                                record.paused = false;
                            }
                        }
                    }
                    return Cancel::LeftToStep;
                }
                Stage::Running => {
                    state.stage = Stage::Ended;
                    state.record.take()
                }
            }
        };
        self.close_current(CLOSE_CANCELLED, b"");
        Cancel::Ended(self.ending(record, false))
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
                let _ = self.save_record(data_dir);
                false
            }
            Cancel::TooLate => false,
        }
    }

    /// Sending side: from now on a cancel waits for the transfer's task (see
    /// `Stage::Finishing`). Called before `Done` goes out: from then on the
    /// receiver may have all of the item, which is the receiver's once it
    /// has checked it, so a cancel waits for its verdict (also after a
    /// restart, see `TransferRecord::done_sent` and
    /// `TransferRecord::cancelled`). And while the record is saved for the
    /// first time, so that a cancel meanwhile cannot leave it behind.
    pub fn defer_cancel(&self) {
        Self::defer(&mut self.state());
    }

    fn defer(state: &mut State) {
        if state.stage == Stage::Running {
            state.stage = Stage::Finishing;
        }
    }

    /// Sending side: all of the data has gone out and `Done` follows. From
    /// now on a cancel waits for the receiver's verdict
    /// ([`defer_cancel`](Self::defer_cancel)), and the record says so
    /// (`TransferRecord::done_sent`) in the same step, so that a cancel
    /// which finds the one finds the other too (`net::cancelled` tells by
    /// it whether the cancel waits). Whether the record changed; the caller
    /// saves it then ([`save_record`](Self::save_record)).
    pub fn done_sent(&self) -> bool {
        let mut state = self.state();
        Self::defer(&mut state);
        match state.record.as_mut() {
            Some(record) if !record.done_sent => {
                record.done_sent = true;
                true
            }
            _ => false,
        }
    }

    /// Sending side: whether a cancel waits (see
    /// [`defer_cancel`](Self::defer_cancel)).
    pub fn deferring(&self) -> bool {
        self.state().stage == Stage::Finishing
    }

    /// Sending side: what [`defer_cancel`](Self::defer_cancel) waited for
    /// came without the hand-over (the receiver asks to continue, so it does
    /// not have the item; or the record is on disk). A cancel ends the
    /// transfer again, and one that came meanwhile ends it now, as
    /// [`cancel`](Self::cancel) does; the result is what that leaves to do.
    pub fn stop_deferring(&self) -> Option<Ending> {
        let record = {
            let mut state = self.state();
            if state.stage != Stage::Finishing {
                return None;
            }
            if !std::mem::take(&mut state.cancel_left) {
                state.stage = Stage::Running;
                return None;
            }
            state.stage = Stage::Ended;
            state.record.take()
        };
        self.close_current(CLOSE_CANCELLED, b"");
        Some(self.ending(record, false))
    }

    /// Whether a cancel would end the transfer now (see [`cancel`](Self::cancel)).
    pub fn running(&self) -> bool {
        self.state().stage == Stage::Running
    }

    /// The item is the receiver's from now on, and a cancel comes too late
    /// (see `Stage::HandedOver`). Sending side: the receiver has confirmed
    /// that all of it arrived intact, and it is the receiver's to put in
    /// place or to cancel. Receiving side: it was in place already before a
    /// restart (see `TransferRecord::placed_item`).
    pub fn hand_over(&self) {
        let mut state = self.state();
        if state.stage != Stage::Ended {
            state.stage = Stage::HandedOver;
        }
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
            state.stage = Stage::Ended;
            state.record.take()
        };
        self.ending(record, true).forget_blocking(data_dir);
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
    /// The user cancelled, but the transfer does not end for it now: the
    /// cancel came too late (see `Cancel::TooLate`), or it waits for the
    /// receiver's verdict and is in the record (see `Cancel::LeftToStep`).
    /// Shutdown does not wait for it.
    pub cancel_waits: AtomicBool,
}

impl Control {
    pub fn new(paused: bool) -> Self {
        Control {
            cancel: CancelToken::new(),
            paused: watch::channel(paused).0,
            cancel_waits: AtomicBool::new(false),
        }
    }

    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    /// The user cancelled the transfer. A cancelled transfer is not paused
    /// any more (the window offers no Resume then), and the cancel of a send
    /// that waits for the receiver's verdict needs the connection: it is
    /// resumed first, so that the transfer never finds itself paused and
    /// cancelled at once (its tasks wait for a pause to end without watching
    /// the cancel).
    pub fn cancel(&self) {
        self.paused
            .send_if_modified(|paused| std::mem::replace(paused, false));
        self.cancel.cancel();
    }

    /// Whether the user cancelled the transfer.
    pub fn cancelled(&self) -> bool {
        self.cancel.flag().load(Ordering::Relaxed)
    }

    /// Whether the user cancelled and the transfer is about to end for it.
    pub fn cancelling(&self) -> bool {
        self.cancelled() && !self.cancel_waits.load(Ordering::Relaxed)
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
            done_sent: false,
            cancelled: false,
            ended: false,
        }
    }

    fn receive_side(dir: &Path, part: &Path, synced: Option<u64>) -> Side {
        Side::Receive {
            dest_dir: dir.to_path_buf(),
            files: ReceiveFiles::new(part.to_path_buf()),
            synced,
            part_id: None,
            folder: ReceivedFolder::Extract,
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
        assert_eq!(load_all(&dir)[0].receive_files(), Some(&files));
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
            if let Some(files) = r.receive_files_mut() {
                files.unpacking = Some(Owned::at(folder).unwrap());
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
        if let Some(files) = r.receive_files_mut() {
            files.moving = Some(Owned {
                path: placed.clone(),
                id: archive::file_id(&part).unwrap(),
            });
        }
        // Noted, but not renamed yet: nothing to follow, not even when a
        // file of the new name appeared meanwhile.
        assert!(!r.follow_moved_part(&[]));
        assert_eq!(r.placed_item(), None);
        fs::write(&placed, b"the user's").unwrap();
        assert!(!r.follow_moved_part(&[]));
        fs::remove_file(&placed).unwrap();

        fs::rename(&part, &placed).unwrap();
        fs::write(&part, b"another transfer").unwrap();
        assert!(r.follow_moved_part(&[]));
        assert_eq!(r.placed_item(), Some(placed.as_path()));
        assert!(!r.follow_moved_part(&[]), "once");

        // A folder kept with its zip is unpacked after its zip was moved.
        let mut both = r.clone();
        both.offer.kind = Kind::Folder;
        if let Side::Receive { folder, .. } = &mut both.side {
            *folder = ReceivedFolder::Both;
        }
        assert_eq!(both.placed_item(), None);
        fs::remove_dir_all(&dir).ok();
    }

    /// On FAT and exFAT, where every item of a volume has the same identity,
    /// the sizes tell whether the part moved: in the last step it has all of
    /// the item. When both names have something of its size, the other
    /// records tell, and then the content: the old name keeps the part
    /// unless another transfer's record names it, or only what has the new
    /// name holds what the sender sent.
    #[test]
    fn a_moved_part_is_followed_on_fat() {
        let dir = scratch("moved-fat");
        archive::PRETEND_FAT.set(true);
        let part = dir.join("a.bin.part");
        fs::write(&part, b"0123456789").unwrap();
        let placed = dir.join("a.bin");
        let mut r = record(18, receive_side(&dir, &part, None));
        r.done_hash = Some(*blake3::hash(b"0123456789").as_bytes());
        if let Side::Receive { part_id, files, .. } = &mut r.side {
            *part_id = Some(archive::file_id(&part).unwrap());
            files.moving = Some(Owned {
                path: placed.clone(),
                id: archive::file_id(&part).unwrap(),
            });
        }
        assert!(r.part_is_there());
        // The records of this start: this one, and one of another transfer
        // whose part has the same name (it can only have taken it once this
        // one's part was renamed).
        let mine = parts_of([&r]);
        assert_eq!(mine, vec![(r.token, part.clone())]);
        let mut other = r.clone();
        other.token = [19; 16];
        let theirs = parts_of([&r, &other]);

        // Noted, but not renamed: what has the new name is the user's, also
        // when it is as large as the item, even an identical copy of it
        // (the item received before): this transfer keeps its part, which
        // then gets a free name, and is not left behind.
        fs::write(&placed, b"the user's file").unwrap();
        assert!(!r.clone().follow_moved_part(&mine));
        fs::write(&placed, b"exactly 10").unwrap();
        assert!(!r.clone().follow_moved_part(&mine));
        fs::write(&placed, b"0123456789").unwrap();
        assert!(!r.clone().follow_moved_part(&mine));
        fs::remove_file(&placed).unwrap();

        // Renamed, and another transfer's part got the old name since.
        fs::rename(&part, &placed).unwrap();
        fs::write(&part, b"another").unwrap();
        let mut moved = r.clone();
        assert!(moved.follow_moved_part(&mine));
        assert_eq!(moved.placed_item(), Some(placed.as_path()));
        // Renamed, and the other transfer (of the same item) has all of it
        // under the old name: its record tells the two apart; without one
        // (another profile's transfer, say), the content does, as far as
        // it can: an identical copy under the old name is kept as the part.
        fs::write(&part, b"0123456789").unwrap();
        assert!(!r.clone().follow_moved_part(&mine));
        assert!(r.clone().follow_moved_part(&theirs));
        fs::write(&part, b"ten others").unwrap();
        assert!(r.clone().follow_moved_part(&mine));
        assert!(r.clone().follow_moved_part(&theirs));
        // Without the hash (a record of an earlier version), the old name
        // keeps the part unless a record claims it.
        let mut unknown = r.clone();
        unknown.done_hash = None;
        assert!(!unknown.clone().follow_moved_part(&mine));
        assert!(unknown.follow_moved_part(&theirs));
        // Renamed, with nothing under the old name.
        fs::remove_file(&part).unwrap();
        let mut moved = r.clone();
        assert!(moved.follow_moved_part(&mine));
        archive::PRETEND_FAT.set(false);
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
            files,
            ..
        } = &mut r.side
        {
            *folder = ReceivedFolder::KeepZip;
            *part_id = Some(archive::file_id(&zip).unwrap());
            files.moving = Some(Owned::at(&zip).unwrap());
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
            if let Some(files) = r.receive_files_mut() {
                files.leftovers.push(Owned::at(&old).unwrap());
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

    fn send_record(dir: &Path, token: u8) -> (TransferRecord, PathBuf) {
        let zip = dir.join(format!("send-{token}.zip"));
        fs::write(&zip, b"zip").unwrap();
        let record = record(
            token,
            Side::Send {
                source: dir.join("Photos"),
                source_len: 3,
                source_mtime: None,
                data_path: zip.clone(),
            },
        );
        record.save(dir).unwrap();
        (record, zip)
    }

    /// From `Done` on, a cancel on the sending side waits for the receiver's
    /// verdict: once it has confirmed the item, the cancel was too late; if
    /// it turns out not to have it, the cancel ends the transfer then.
    #[test]
    fn a_cancel_waits_for_the_receivers_verdict() {
        let dir = scratch("verdict");
        let peer = iroh::SecretKey::generate().public();

        // The receiver did not have it: the cancel ends the transfer.
        let (session, _rx) = Session::new(1, [20; 16], peer, Role::Send);
        let (mut r, zip) = send_record(&dir, 20);
        r.paused = true;
        session.set_record(r);
        assert!(session.done_sent());
        session.save_record(&dir);
        assert!(session.deferring());
        assert!(!session.cancel_blocking(&dir), "it waits");
        assert!(zip.exists() && !session.running());
        // A restart keeps the transfer, and the cancel, which waits on (not
        // paused: the window offers no Resume once cancelled).
        let stored = &load_all(&dir)[0];
        assert!(!stored.ended && stored.cancelled && stored.cancel_pending());
        assert!(!stored.paused);
        assert!(stored.receiver_may_have_item());
        let ending = session.stop_deferring().expect("the cancel ends it now");
        ending.forget_blocking(&dir);
        assert!(!zip.exists());
        assert!(load_all(&dir).is_empty());
        assert_eq!(tombstone(&dir, &[20; 16]), Some(false));
        assert!(!session.cancel_blocking(&dir), "it has ended");

        // The receiver confirmed it: too late, also after a restart.
        let (session, _rx) = Session::new(2, [21; 16], peer, Role::Send);
        let (r, zip) = send_record(&dir, 21);
        session.set_record(r);
        session.defer_cancel();
        assert!(!session.cancel_blocking(&dir));
        session.hand_over();
        session.save_done_hash(&dir, [1; 32]);
        assert!(!load_all(&dir)[0].cancel_pending());
        assert!(session.stop_deferring().is_none());
        assert!(!session.cancel_blocking(&dir));
        session.end(None, true).forget_blocking(&dir);
        assert!(!zip.exists());
        assert_eq!(tombstone(&dir, &[21; 16]), Some(true));

        // Without a cancel, the transfer just goes on; `Done` goes out again
        // later, so a restart until then does not wait for a verdict.
        let (session, _rx) = Session::new(3, [22; 16], peer, Role::Send);
        let (r, _zip) = send_record(&dir, 22);
        session.set_record(r);
        // `Done` goes out: the cancel waits from the same moment on as the
        // record says so, before anything is saved.
        assert!(session.done_sent(), "noted");
        assert!(session.deferring());
        assert_eq!(session.with_record(|r| r.done_sent), Some(true));
        assert!(!session.done_sent(), "noted once");
        session.save_record(&dir);
        assert!(
            load_all(&dir)
                .iter()
                .any(|r| r.token == [22; 16] && r.done_sent)
        );
        assert!(session.stop_deferring().is_none());
        assert!(session.running() && !session.deferring());
        session.clear_done_sent(&dir);
        let stored = load_all(&dir);
        let stored = stored.iter().find(|r| r.token == [22; 16]).unwrap();
        assert!(!stored.done_sent && !stored.receiver_may_have_item());
        assert!(session.cancel_blocking(&dir));
        fs::remove_dir_all(&dir).ok();
    }

    /// Once a transfer has ended on this side, a cancel changes nothing of
    /// how it ended, also while its files are still being seen to.
    #[test]
    fn a_cancel_after_the_end_is_too_late() {
        let dir = scratch("after-end");
        let peer = iroh::SecretKey::generate().public();
        let (session, _rx) = Session::new(1, [23; 16], peer, Role::Send);
        let (r, zip) = send_record(&dir, 23);
        session.set_record(r);
        let ending = session.end(None, true);
        assert!(matches!(session.cancel(), Cancel::TooLate));
        ending.forget_blocking(&dir);
        assert!(!zip.exists());
        assert_eq!(tombstone(&dir, &[23; 16]), Some(true));
        fs::remove_dir_all(&dir).ok();
    }

    /// A record of a completed receive that could not be deleted then is
    /// only completed at the next start: the folder that it names as being
    /// unpacked into is the received item, not something to delete.
    #[test]
    fn a_completed_record_that_stayed_keeps_the_item() {
        let dir = scratch("completed-stayed");
        let temp = dir.join("recv-24.zip");
        fs::write(&temp, b"zip").unwrap();
        let folder = dir.join("Photos");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("a.jpg"), b"received").unwrap();
        let mut r = record(24, receive_side(&dir, &temp, None));
        r.offer.kind = Kind::Folder;
        r.done_hash = Some([1; 32]);
        if let Some(files) = r.receive_files_mut() {
            files.unpacking = Some(Owned::at(&folder).unwrap());
        }
        r.save(&dir).unwrap();
        write_tombstone(&dir, &r.token, true);
        assert!(load_all(&dir).is_empty());
        assert_eq!(fs::read(folder.join("a.jpg")).unwrap(), b"received");
        assert!(!temp.exists(), "only what was temporary goes");
        assert!(!record_path(&dir, &r.token).exists());
        assert_eq!(tombstone(&dir, &r.token), Some(true));
        fs::remove_dir_all(&dir).ok();
    }

    /// The writes of a record keep the order of its changes, and none brings
    /// the record back once the transfer has ended and deleted it.
    #[test]
    fn saves_never_outlive_the_end() {
        let dir = scratch("saves");
        let peer = iroh::SecretKey::generate().public();
        let (session, _rx) = Session::new(1, [25; 16], peer, Role::Send);
        let (r, _zip) = send_record(&dir, 25);
        session.set_record(r);
        let savers: Vec<_> = (0..8)
            .map(|i| {
                let (session, dir) = (session.clone(), dir.clone());
                std::thread::spawn(move || session.save_paused(&dir, i % 2 == 0))
            })
            .collect();
        session.end(None, false).forget_blocking(&dir);
        for saver in savers {
            saver.join().unwrap();
        }
        assert!(load_all(&dir).is_empty());
        assert!(!record_path(&dir, &[25; 16]).exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_of_earlier_versions_still_load() {
        let dir = scratch("legacy");
        let part = dir.join("old.bin.part");
        fs::write(&part, b"12345").unwrap();
        // A record as written before `synced`, `done_hash`, `done_sent`,
        // `folder`, `unpacking`, `moving`, `part_id`, `leftovers`, `ended`
        // and the offer's times existed.
        let mut side = receive_side(&dir, &part, None);
        if let Side::Receive { folder, .. } = &mut side {
            *folder = ReceivedFolder::Both;
        }
        let mut json = serde_json::to_value(record(7, side)).unwrap();
        for field in ["done_hash", "done_sent", "ended"] {
            json.as_object_mut().unwrap().remove(field);
        }
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
            r.receive_files().unwrap().unpacking.clone()
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

        // A receive that has all of its item does not expire: its item is
        // in place already, or it is put in place without the sender.
        let make_old = |r: &TransferRecord| {
            r.save(&dir).unwrap();
            fs::File::options()
                .write(true)
                .open(record_path(&dir, &r.token))
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        };
        let item = dir.join("a.bin");
        fs::write(&item, b"0123456789").unwrap();
        let mut placed = record(26, receive_side(&dir, &item, Some(10)));
        placed.done_hash = Some([1; 32]);
        if let Side::Receive { part_id, files, .. } = &mut placed.side {
            *part_id = Some(archive::file_id(&item).unwrap());
            files.moving = Some(Owned::at(&item).unwrap());
        }
        make_old(&placed);
        let whole = dir.join("b.bin.part");
        fs::write(&whole, b"0123456789").unwrap();
        let mut verified = record(27, receive_side(&dir, &whole, Some(10)));
        verified.done_hash = Some([1; 32]);
        make_old(&verified);
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded
                .iter()
                .find(|r| r.token == [26; 16])
                .unwrap()
                .placed_item(),
            Some(item.as_path())
        );
        assert!(item.exists() && whole.exists());

        // Unless it has ended: then what it renamed goes.
        let mut ended = verified.clone();
        ended.ended = true;
        if let Some(files) = ended.receive_files_mut() {
            files.moving = Some(Owned {
                path: dir.join("b.bin"),
                id: archive::file_id(&whole).unwrap(),
            });
        }
        make_old(&ended);
        fs::rename(&whole, dir.join("b.bin")).unwrap();
        assert_eq!(load_all(&dir).len(), 1);
        assert!(!dir.join("b.bin").exists());
        assert!(item.exists());
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
