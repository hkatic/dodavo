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
    protocol::{Ended, Kind, Offer, Token, token_hex},
};
use crate::archive::{self, Cancelled, FileId};

/// Records and tombstones older than this are forgotten at startup (see
/// [`load_all`] and [`Clock`]).
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
// A few hundred bytes, once per transfer: not worth a box.
#[allow(clippy::large_enum_variant)]
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
        /// How much of the data had gone out when the record was last saved
        /// (every `send::SAVE_SENT_EVERY` bytes while it flows): what a
        /// restored send shows until the receiver says how much it has,
        /// which can be less. 0 in records of earlier versions.
        #[serde(default)]
        sent: u64,
    },
    Receive {
        dest_dir: PathBuf,
        /// The files that the last step of the receive changes.
        #[serde(flatten)]
        files: ReceiveFiles,
        /// The identity of the part file (see [`Owned`]): what has its name
        /// is the part only while it is that file, so that a later start
        /// never cuts, deletes or completes something else of the same name
        /// (on FAT and exFAT, the part's creation time tells, which the part
        /// keeps until the transfer is done with it: see `receive::finish_file`).
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
    /// The transfer has ended and its part was deleted, while something
    /// else of it could not be (see [`TransferRecord::discard`]): nothing
    /// under the part's name is this transfer's any more, and a later start
    /// leaves the name alone.
    #[serde(default)]
    pub part_gone: bool,
}

impl ReceiveFiles {
    /// The files of a new receive, whose part is at `part`.
    pub fn new(part: PathBuf) -> Self {
        ReceiveFiles {
            part,
            unpacking: None,
            moving: None,
            leftovers: Vec::new(),
            part_gone: false,
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

    /// The new folder at `path`, which the transfer has just made: given the
    /// time of its creation where that can be done (see
    /// [`archive::created_now`]), otherwise as it is.
    pub fn made_at(path: &Path) -> io::Result<Owned> {
        Ok(Owned {
            path: path.to_path_buf(),
            id: archive::created_now(path).or_else(|_| archive::file_id(path))?,
        })
    }

    /// Whether the item at `path` is still this one.
    pub fn is_there(&self) -> bool {
        match archive::file_id(&self.path) {
            Ok(now) => self.id.same_item(&now),
            // Not to be taken for gone: a later start would delete it.
            Err(e) => archive::cannot_tell(&e),
        }
    }

    /// Deletes the folder with all it holds (links in it, not what they point
    /// to), if it is still this one. Whether it is gone: also when something
    /// else has the name now (that is not the transfer's to delete); not when
    /// that cannot be told yet.
    pub fn remove_folder(&self) -> bool {
        match archive::file_id(&self.path) {
            Ok(now) if self.id.same_item(&now) => match fs::remove_dir_all(&self.path) {
                Ok(()) => true,
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            },
            Ok(_) => true,
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        }
    }

    /// Deletes the file if it is still this one; whether it is gone (as
    /// with [`remove_folder`](Self::remove_folder)). `patient` as with
    /// [`remove_file`].
    fn remove_file(&self, patient: bool) -> bool {
        deleted(&self.path, patient, || match archive::file_id(&self.path) {
            Ok(now) if self.id.same_item(&now) => fs::remove_file(&self.path),
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        })
    }
}

/// Deletes a file; whether it is gone. With `patient` (at the end of a
/// running transfer, off the runtime's worker threads), one that another
/// program holds open for a moment, such as a virus scanner, or that is
/// being deleted already (a part whose connection was opening it when the
/// transfer ended, see [`Session::keep_part`]), is tried again for about a
/// second and a half. Otherwise, as at a start, it stays for a later try
/// (see [`TransferRecord::discard`]). The part of a receive that a
/// connection writes to is deleted through the connection's handle instead.
fn remove_file(path: &Path, patient: bool) -> bool {
    deleted(path, patient, || fs::remove_file(path))
}

/// Runs `delete`, which deletes the file at `path`, with `patient` again
/// while the file is held or being deleted (see [`remove_file`] and
/// [`archive::delete_held`]); whether it is gone.
fn deleted(path: &Path, patient: bool, delete: impl FnMut() -> io::Result<()>) -> bool {
    let pauses = if patient { archive::HELD_RETRY_MS } else { &[] };
    match archive::retried(pauses, |e| archive::delete_held(e, path), delete) {
        Ok(()) => true,
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}

/// The error of a receive whose part file is gone, or is no longer the part:
/// "the partly transferred data is no longer there".
pub(crate) fn part_missing() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        crate::i18n::tr!(error_part_missing),
    )
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
    /// `paused` (a record of an earlier version that says so is put right
    /// when it is read, see [`load_all`]).
    #[serde(default)]
    pub cancelled: bool,
    /// The transfer has ended without its item, and its files are being
    /// deleted ([`discard`](Self::discard)): a later start finishes that
    /// instead of continuing the transfer.
    #[serde(default)]
    pub ended: bool,
    /// When the transfer ended (see [`end_now`](Self::end_now)): a later
    /// start gives up its files [`MAX_AGE`] after that. `None` in records
    /// of earlier versions, which ended when their record file was last
    /// written (see [`load_all`]).
    #[serde(default)]
    pub ended_at: Option<SystemTime>,
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
    /// file, if it is still the part, at most what is known to be flushed;
    /// sender: all of them once the receiver confirmed them, otherwise what
    /// had gone out by the last save, which the receiver may not all have,
    /// see `Side::Send::sent`).
    pub fn have(&self) -> u64 {
        match &self.side {
            Side::Receive { synced, .. } => {
                let len = self.part_len().unwrap_or(0);
                synced.map_or(len, |s| s.min(len))
            }
            Side::Send { .. } if self.done_hash.is_some() => self.offer.size,
            Side::Send { sent, .. } => (*sent).min(self.offer.size),
        }
    }

    /// Cuts the part file back to what is known to be on disk: after a power
    /// cut the end of the file may never have been written. Fails if the
    /// part is not there (see [`open_part`](Self::open_part)).
    pub fn truncate_to_synced(&self) -> io::Result<()> {
        if let Side::Receive {
            synced: Some(synced),
            ..
        } = &self.side
        {
            // Only then for writing: the part, or the item already in place,
            // may be read-only, or open in a program that lets nothing else
            // write to it.
            if self.part_len()? > *synced {
                self.open_part(fs::OpenOptions::new().write(true))?
                    .set_len(*synced)?;
            }
        }
        Ok(())
    }

    /// The length of the part file of a receive, if it is still the part
    /// (see [`open_part`](Self::open_part)).
    fn part_len(&self) -> io::Result<u64> {
        let part = self.open_part(&archive::length_options())?;
        Ok(part.metadata()?.len())
    }

    /// Opens the part file of a receive with `options` if it is still the
    /// part (see [`part_is_there`](Self::part_is_there)), as told by the
    /// handle that is opened: what is used is what was checked, not a file
    /// that took the name in between. Fails with "the partly transferred
    /// data is no longer there" otherwise.
    pub fn open_part(&self, options: &fs::OpenOptions) -> io::Result<fs::File> {
        let files = self
            .receive_files()
            .filter(|files| !files.part_gone)
            .ok_or_else(part_missing)?;
        let part = options.open(&files.part).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => part_missing(),
            _ => e,
        })?;
        match self.part_identity() {
            Some(id) if !FileId::of(&part).is_ok_and(|now| id.same_item(&now)) => {
                Err(part_missing())
            }
            _ if self.part_shorter() => Err(part_missing()),
            _ => Ok(part),
        }
    }

    /// On FAT and exFAT: whether what has the part's name is shorter than
    /// what the record knows was written to the part and reached the disk
    /// (`synced`). Then it is not the part, although its identity says so:
    /// there the identity is only the creation time, which Windows also
    /// gives a file made under the part's name moments after the part was
    /// deleted (see `FileId::same_item`), and such a file must be neither
    /// written to nor deleted. Where the identity has the part's number, it
    /// tells the part from any other file, and a part that lost its end goes
    /// on from what is there (see [`have`](Self::have)); on FAT such a part
    /// (of a drive pulled out during a write, say) cannot be told from
    /// another file, so it is left as it is, for the user to delete. (By the
    /// part's name: a connection's handle may not read the size.)
    fn part_shorter(&self) -> bool {
        if !self.part_identity().is_some_and(|id| id.by_creation_time()) {
            return false;
        }
        match (&self.side, self.receive_files()) {
            (
                Side::Receive {
                    synced: Some(synced),
                    ..
                },
                Some(files),
            ) => fs::metadata(&files.part).is_ok_and(|m| m.len() < *synced),
            _ => false,
        }
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

    /// Whether the transfer can go on at a later start: the files it needs
    /// are there, or it is a send whose data the receiver may have, which
    /// goes on without the data until the receiver says whether it needs it
    /// (see `Sender::reconnect`).
    pub fn can_go_on(&self) -> bool {
        self.files_present() || self.receiver_may_have_item()
    }

    /// The identity of a receive's part file, if known: noted when the part
    /// was created, or with the name it was being renamed to.
    pub fn part_identity(&self) -> Option<FileId> {
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
    /// was created (see [`Owned`]), or (in records of earlier versions)
    /// anything of its name, and not shorter than what reached the disk (see
    /// [`part_shorter`](Self::part_shorter)).
    pub fn part_is_there(&self) -> bool {
        match (self.receive_files(), self.owned_part()) {
            (Some(files), _) if files.part_gone || self.part_shorter() => false,
            (Some(_), Some(part)) => part.is_there(),
            (Some(files), None) => files.part.exists(),
            (None, _) => false,
        }
    }

    /// Deletes the part file of a receive if it is still this transfer's
    /// (see [`Owned::remove_file`]); whether it is gone. `patient` as with
    /// [`remove_file`].
    fn remove_part(&self, patient: bool) -> bool {
        match (self.receive_files(), self.owned_part()) {
            // Not the part (see `part_shorter`): there is none to delete.
            (Some(files), _) if files.part_gone || self.part_shorter() => true,
            (Some(_), Some(part)) => part.remove_file(patient),
            (Some(files), None) => remove_file(&files.part, patient),
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
        self.complete_with(data_dir, false);
    }

    /// [`complete`](Self::complete), with `patient` as with [`remove_file`].
    fn complete_with(&self, data_dir: &Path, patient: bool) {
        match &self.side {
            Side::Receive { files, .. } => {
                self.remove(data_dir);
                if self.part_is_temporary() {
                    remove_file(&files.part, patient);
                }
                // Once: the record that would keep trying is gone.
                for folder in &files.leftovers {
                    folder.remove_folder();
                }
            }
            // No need to mark the record as ended first (see `discard`): one
            // that is left completes at the next start.
            Side::Send { .. } => {
                if self.delete_files(patient).is_none() {
                    self.remove(data_dir);
                }
            }
        }
    }

    /// Marks the transfer as ended, now (see [`ended`](Self::ended)); whether
    /// it had not ended before.
    pub fn end_now(&mut self) -> bool {
        if self.ended {
            return false;
        }
        self.ended = true;
        self.ended_at = Some(SystemTime::now());
        true
    }

    /// Ends the transfer without its item: deletes the partial or temporary
    /// data, and the folders a received folder was being unpacked into,
    /// each only while it is still this transfer's. The record is marked as
    /// ended first and goes last, so that a crash in between, or a file that
    /// cannot be deleted now, leaves it for the next start to finish the
    /// deletion, never to continue the transfer. A record that stays names
    /// only what is left to delete: a name whose file is gone may soon be
    /// another transfer's, and on FAT and exFAT only the name would tell
    /// (see [`Owned`]).
    pub fn discard(&self, data_dir: &Path) {
        self.discard_with(data_dir, None, false);
    }

    /// [`discard`](Self::discard) at the end of a running transfer: `open`
    /// is a second handle to the part file of a receive whose connection
    /// still writes to it (see [`Session::keep_part`]), which cannot be
    /// deleted by its name then and goes through that handle once the
    /// connection lets go of it. `patient` as with [`remove_file`].
    fn discard_with(&self, data_dir: &Path, open: Option<&fs::File>, patient: bool) {
        let mut record = self.clone();
        if record.end_now() {
            let _ = record.save(data_dir);
        }
        let noted = record.clone();
        if let Some(part) = open
            && archive::delete_on_close(part).is_ok()
        {
            record.mark_part_gone();
        }
        match record.delete_files(patient) {
            None => record.remove(data_dir),
            Some(left) if left != noted => {
                let _ = left.save(data_dir);
            }
            Some(_) => {}
        }
    }

    /// Gives up the files of a transfer that ended [`MAX_AGE`] ago (see
    /// [`Fate::GivenUp`]): deletes what it can of them once more, and then
    /// forgets the record however that went, so that no later start tries
    /// again.
    pub fn give_up(&self, data_dir: &Path) {
        let _ = self.delete_files(false);
        self.remove(data_dir);
    }

    /// Deletes what a transfer that ends without its item leaves behind (see
    /// [`discard`](Self::discard)), and nothing else. `None` if all of it is
    /// gone, otherwise the record as it is left: naming only what could not
    /// be deleted. `patient` as with [`remove_file`].
    fn delete_files(&self, patient: bool) -> Option<TransferRecord> {
        match &self.side {
            Side::Send {
                source, data_path, ..
            } => (data_path != source && !remove_file(data_path, patient)).then(|| self.clone()),
            Side::Receive { files, .. } => {
                let part = self.remove_part(patient);
                // Every one is tried, whether or not the others go.
                let kept = files
                    .unpacking
                    .iter()
                    .chain(&files.leftovers)
                    .filter(|folder| !folder.remove_folder())
                    .cloned()
                    .collect::<Vec<_>>();
                if part && kept.is_empty() {
                    return None;
                }
                let mut left = self.clone();
                if let Some(files) = left.receive_files_mut() {
                    files.unpacking = None;
                    files.leftovers = kept;
                    if part {
                        // `moving` names the part too, under its new name.
                        files.part_gone = true;
                        files.moving = None;
                    }
                }
                Some(left)
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

    /// Notes that the part file of a receive is gone (see
    /// [`ReceiveFiles::part_gone`]).
    fn mark_part_gone(&mut self) {
        if let Some(files) = self.receive_files_mut() {
            files.part_gone = true;
        }
    }

    /// After a restart: if the part file was renamed before the program
    /// ended, but the record not yet changed with it (see
    /// [`ReceiveFiles::moving`]), the record follows it. Whether it did.
    ///
    /// The part has moved if its identity has the new name (on FAT and
    /// exFAT, its creation time: see [`FileId`]). Where the identity does
    /// not tell (records of earlier versions on FAT and exFAT), anything
    /// under either name counts as the part, such as a file of the new name
    /// that appeared after a crash before the rename, or another transfer's
    /// part that got the old name after a crash right after it. There the
    /// sizes decide: the part is renamed only in the last step, when it has
    /// all of the item, so it has moved if what has the new name is as large
    /// as the item and what has the old name (if anything) is not. When
    /// both are, the other records tell: the part has moved if another
    /// record of this profile whose part is under its name names the old
    /// name as its own part (`parts`: the token and the part of every such
    /// record read at this start, whether its transfer goes on or is about
    /// to expire, see `Verdict::part_under_its_name`), since another transfer
    /// could only take the name once this one's rename had freed it. A
    /// record whose transfer is done with (its item completed or it ended,
    /// and only the record stayed behind) is not among them: it had the
    /// name before this one, which tells nothing about this one's rename,
    /// and a newer such record is as good as impossible, since every start
    /// follows a moved part.
    /// Otherwise the content tells, against what the sender sent
    /// (`done_hash`, known since before the last step): what has the old
    /// name is the part if it hashes to that (also when what has the new
    /// name does too, an identical copy of the item received earlier, say:
    /// this transfer keeps its part, which then gets a free name, and no
    /// renamed part is left behind as an orphan), and otherwise what has
    /// the new name is the part if it hashes to that (what has the old name
    /// is then something else's, such as the part of another profile that
    /// receives into the same folder). Reading the files is blocking, and
    /// rare: only this twice-ambiguous case does it, at a start, before the
    /// endpoint accepts connections (a peer that dials meanwhile sees a lost
    /// connection and dials again).
    pub fn follow_moved_part(&mut self, parts: &[(Token, PathBuf)]) -> bool {
        let Some(files) = self.receive_files().filter(|files| !files.part_gone) else {
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

/// Remembers how a transfer ended, for the other side to learn later. A
/// transfer that completed stays completed: a later report that it ended
/// (after a restart that could not tell, say) does not change that.
pub fn write_tombstone(data_dir: &Path, token: &Token, ended: Ended) {
    if ended != Ended::Completed && tombstone(data_dir, token) == Some(Ended::Completed) {
        return;
    }
    let path = tombstone_path(data_dir, token);
    if fs::create_dir_all(path.parent().unwrap()).is_ok() {
        let text = match ended {
            Ended::Completed => "completed",
            Ended::Cancelled => "cancelled",
            Ended::Failed => "failed",
        };
        let _ = fs::write(path, text);
    }
}

/// How the transfer ended on this side, if it did. ("ended", which earlier
/// versions wrote for any end without the item, reads as cancelled, as they
/// reported it.)
pub fn tombstone(data_dir: &Path, token: &Token) -> Option<Ended> {
    fs::read_to_string(tombstone_path(data_dir, token))
        .ok()
        .map(|s| match s.trim() {
            "completed" => Ended::Completed,
            "failed" => Ended::Failed,
            _ => Ended::Cancelled,
        })
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Whether the file at `path` was last written longer ago than `max_age`.
fn older_than(path: &Path, max_age: Duration) -> bool {
    modified(path)
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > max_age)
}

/// The file in the records folder that notes when the program last started
/// (see [`Clock`]).
const LAST_START: &str = "last-start";

/// What tells at a start whether something of an earlier run is old: the
/// time now, and the time at the start before this one. Something is old
/// only if it was older than [`MAX_AGE`] at both, so that a clock that is
/// wrong at one start (years ahead, say, until the time is synced) does not
/// end every interrupted transfer at once: at the next start, with the
/// right time, nothing of that is old. A clock that stays wrong ends them
/// at the next start. Unknown at a start, the time of the last start makes
/// nothing old.
struct Clock {
    now: SystemTime,
    last_start: Option<SystemTime>,
}

impl Clock {
    /// Reads the time of the last start, and notes this one instead.
    fn start(data_dir: &Path) -> Clock {
        let path = records_dir(data_dir).join(LAST_START);
        let last_start = fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)));
        let now = SystemTime::now();
        if let Ok(since) = now.duration_since(SystemTime::UNIX_EPOCH) {
            let _ = fs::create_dir_all(records_dir(data_dir))
                .and_then(|()| fs::write(&path, since.as_secs().to_string()));
        }
        Clock { now, last_start }
    }

    /// Whether `time` is longer ago than [`MAX_AGE`], now and at the last
    /// start.
    fn is_old(&self, time: SystemTime) -> bool {
        let older_at = |at: SystemTime| at.duration_since(time).is_ok_and(|age| age > MAX_AGE);
        older_at(self.now) && self.last_start.is_some_and(older_at)
    }

    /// Whether the file at `path` was last written longer ago than
    /// [`MAX_AGE`] (see [`is_old`](Self::is_old)).
    fn is_old_file(&self, path: &Path) -> bool {
        modified(path).is_some_and(|time| self.is_old(time))
    }
}

/// Whether `path` is where the record of `token` is kept (see
/// [`record_path`]).
fn is_record_of(path: &Path, token: &Token) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case(&token_hex(token)))
}

/// The records that [`load_all`] read, each list in the order in which the
/// transfers were created.
#[derive(Debug, Default)]
pub struct Found {
    /// The records whose transfers go on.
    pub going_on: Vec<TransferRecord>,
    /// The records whose transfers are over, each with its [`Fate`].
    pub over: Vec<(TransferRecord, Fate)>,
}

/// The unfinished transfers from earlier runs. On the way, the records of
/// transfers that completed are forgotten here (their tombstone says so;
/// only the record, or something temporary, could not be deleted then), and
/// so are old tombstones. A part file that was renamed right before the
/// program ended is followed (see [`TransferRecord::follow_moved_part`]).
/// Every other record is returned: one whose transfer goes on, and one whose
/// transfer is over, with its [`Fate`]. Old is longer ago than [`MAX_AGE`]
/// (see [`Clock`]): a transfer in which nothing happened for that long
/// expires, unless it is a receive that has all of its item, which finishes
/// without the sender (its item may even be in place already, see
/// [`TransferRecord::placed_item`]), and the files of one that ended that
/// long ago are given up. Only the files of an expired transfer are deleted
/// here, before any transfer goes on (see [`Fate::Expired`]); those of the
/// others that are over are left to the start, which deletes them in the
/// background: deleting a folder can take a while, and the transfers that
/// go on would wait for it. A record file whose name is not its token's
/// (a copy of one, say) is no record, and is set aside like a damaged one.
pub fn load_all(data_dir: &Path) -> Found {
    let mut found = Found::default();
    let clock = Clock::start(data_dir);
    let Ok(entries) = fs::read_dir(records_dir(data_dir)) else {
        return found;
    };
    let (mut json, other): (Vec<PathBuf>, Vec<PathBuf>) = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.file_name() != Some(LAST_START.as_ref()))
        .partition(|path| path.extension().and_then(|e| e.to_str()) == Some("json"));
    // In the order of their names, whatever the file system's (NTFS lists
    // a folder by name, FAT and exFAT as the entries were made), so that
    // what the records make of each other does not depend on it.
    json.sort();
    // The records first: whether a transfer completed is in its tombstone,
    // which may be about to go. What becomes of each is decided here, once,
    // and the rest of the start keeps to it.
    let read = json
        .into_iter()
        .map(|path| {
            let record = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<TransferRecord>(&text).ok())
                .filter(|record| is_record_of(&path, &record.token))
                .map(|mut record| {
                    let verdict = if tombstone(data_dir, &record.token) == Some(Ended::Completed) {
                        Verdict::Completed
                    } else if record.ended {
                        // A record of an earlier version ended when it was
                        // written last. Noted, so that a later save keeps it.
                        let ended_at = *record
                            .ended_at
                            .get_or_insert_with(|| modified(&path).unwrap_or(clock.now));
                        Verdict::Over(if clock.is_old(ended_at) {
                            Fate::GivenUp
                        } else {
                            Fate::Ended
                        })
                    } else if clock.is_old_file(&path) && !record.finishes_alone() {
                        Verdict::Over(Fate::Expired)
                    } else {
                        Verdict::GoesOn
                    };
                    (record, verdict)
                });
            (path, record)
        })
        .collect::<Vec<_>>();
    // Whose part is under which name, for `follow_moved_part`, before any
    // record is followed or deleted (the verdict tells which names are
    // taken).
    let parts = parts_of(
        read.iter()
            .filter_map(|(_, read)| read.as_ref().map(|(record, verdict)| (record, *verdict))),
    );
    for (path, read) in read {
        match read {
            // Its item is in place: nothing of it may be deleted.
            Some((record, Verdict::Completed)) => record.complete(data_dir),
            Some((mut record, verdict)) => {
                // The part file was renamed right before the program ended.
                // Also when the transfer is over: its files are where the
                // part went, and its old name may belong to another transfer
                // by now.
                let mut changed = record.follow_moved_part(&parts);
                // A cancelled send is never paused (see
                // `TransferRecord::cancelled`); a record of an earlier
                // version may say otherwise, and is put right here, before
                // anything reads it.
                if record.cancel_pending() && record.paused {
                    record.paused = false;
                    changed = true;
                }
                match verdict {
                    Verdict::Over(Fate::Expired) => record.discard(data_dir),
                    _ if changed => {
                        let _ = record.save(data_dir);
                    }
                    _ => {}
                }
                match verdict {
                    Verdict::Over(fate) => found.over.push((record, fate)),
                    _ => found.going_on.push(record),
                }
            }
            None if clock.is_old_file(&path) => {
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
        if clock.is_old_file(&path) {
            let _ = fs::remove_file(&path);
        }
    }
    found.going_on.sort_by_key(|r| r.created);
    found.over.sort_by_key(|(r, _)| r.created);
    found
}

/// Test helper: every record [`load_all`] returns, without what becomes of
/// it.
#[cfg(test)]
pub(crate) fn records(data_dir: &Path) -> Vec<TransferRecord> {
    let found = load_all(data_dir);
    let mut records = found.going_on;
    records.extend(found.over.into_iter().map(|(record, _)| record));
    records.sort_by_key(|r| r.created);
    records
}

/// Test helper: notes a start now, as the last one (see [`Clock`]), so
/// that what is older than [`MAX_AGE`] at the next start was so at that one
/// too.
#[cfg(test)]
pub(crate) fn started_before(data_dir: &Path) {
    Clock::start(data_dir);
}

/// What a start makes of a record it read (see [`load_all`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Its item is in place (the tombstone says so): only the record, or
    /// something temporary, is left to delete, which `load_all` does.
    Completed,
    /// Its transfer goes on.
    GoesOn,
    /// Its transfer is over.
    Over(Fate),
}

impl Verdict {
    /// Whether the part file of a receive with this verdict is still under
    /// the name its record gives it (see
    /// [`TransferRecord::follow_moved_part`]). A transfer that goes on, or
    /// has only gone stale, has its part under its name until it is renamed
    /// in the last step; one that completed renamed it into place and gave
    /// the name up, and one that ended (given up or not) may have renamed it
    /// too (its files are deleted where they went).
    fn part_under_its_name(self) -> bool {
        matches!(self, Verdict::GoesOn | Verdict::Over(Fate::Expired))
    }
}

/// How the transfer of a record that [`load_all`] returns is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    /// It ended in an earlier run, and only its files are left to delete:
    /// the start deletes them ([`TransferRecord::discard`]), and the record
    /// stays until they are gone.
    Ended,
    /// It ended longer ago than [`MAX_AGE`] (see
    /// [`TransferRecord::ended_at`]), and some of its files could not be
    /// deleted since: the start deletes what it can of them once more, and
    /// then forgets the record however that went
    /// ([`TransferRecord::give_up`]). A peer that asks about the transfer
    /// later is told that it is gone, as it was until then: the tombstone of
    /// a transfer that ended says no more than none.
    GivenUp,
    /// Nothing happened in it for longer than [`MAX_AGE`]: `load_all` has
    /// ended it ([`TransferRecord::discard`]), and the start reports that.
    /// Its files are deleted there, before any transfer goes on, and not
    /// later in the background: so that a newer transfer never takes the
    /// name of something about to be deleted (on FAT and exFAT, only the
    /// name tells, see [`Owned`]), and so that the record stays until they
    /// are gone. That is quick: a part file or a temporary archive (only
    /// the last step of a receive makes folders, and a receive that got
    /// that far finishes alone).
    Expired,
}

/// The part file of every receive among `records` whose verdict says that
/// it is still under its name, with the record's token (see
/// [`TransferRecord::follow_moved_part`]).
fn parts_of<'a>(
    records: impl IntoIterator<Item = (&'a TransferRecord, Verdict)>,
) -> Vec<(Token, PathBuf)> {
    records
        .into_iter()
        .filter(|(_, verdict)| verdict.part_under_its_name())
        .filter_map(|(r, _)| Some((r.token, r.receive_files()?.part.clone())))
        .collect()
}

/// Deletes temporary archives (`send-*.zip`, `recv-*.zip`) in `dir` that no
/// record refers to and that are older than a day: leftovers of transfers
/// that ended before they had a record, such as a crash while packing a
/// folder or an offer that was never accepted. `dir` must be this profile's
/// own temporary folder, because the archives of another profile's transfers
/// are unknown here and would look like leftovers.
pub fn cleanup_temp<'a>(dir: &Path, records: impl IntoIterator<Item = &'a TransferRecord>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let in_use: Vec<&Path> = records
        .into_iter()
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
            && older_than(&path, TEMP_MAX_AGE)
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
    // An empty range without opening the file, which may be gone.
    if to <= from {
        return Ok(());
    }
    hash_open(
        &mut fs::File::open(path)?,
        from,
        to,
        hasher,
        cancel,
        progress,
    )
}

/// [`hash_range`] of a file that is open already.
pub fn hash_open(
    file: &mut fs::File,
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
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
    /// The transfer has ended on this side, as it says ([`Session::end`], a
    /// cancel, or once the item is in place): a cancel comes too late and
    /// changes nothing of how it ended.
    Ended(Ended),
}

/// What a session's lock guards: the record and the stage, which change
/// together. File work never holds the lock (see [`Session::save_with`]).
struct State {
    /// The record on disk, once the transfer was accepted; taken when the
    /// transfer ends.
    record: Option<TransferRecord>,
    stage: Stage,
    /// Receiving side: where the item was put, once it is in place (see
    /// [`Session::place`]).
    placed: Option<PathBuf>,
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
    /// Receiving side: a second handle to the part file while a connection
    /// writes to it (see [`keep_part`](Self::keep_part)), which the end of
    /// the transfer takes to delete the part through.
    open_part: Mutex<Option<fs::File>>,
}

/// The part file of a receive is open for a connection while this exists
/// (see [`Session::keep_part`]).
pub(crate) struct KeptPart(Arc<Session>);

impl Drop for KeptPart {
    fn drop(&mut self) {
        lock(&self.0.open_part).take();
    }
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
    ended: Ended,
    /// The session's lock for writing the record (see `Session::writing`).
    writing: Option<Arc<Mutex<()>>>,
    /// A second handle to the part file of a receive, if a connection was
    /// writing to it (see [`Session::keep_part`]).
    part: Option<fs::File>,
}

impl Ending {
    /// Nothing to see to: the transfer had no record (yet).
    pub fn nothing() -> Self {
        Ending {
            record: None,
            ended: Ended::Cancelled,
            writing: None,
            part: None,
        }
    }

    /// Leaves a tombstone for the transfer and forgets its record, with what
    /// [`TransferRecord::complete`] or [`TransferRecord::discard`] deletes,
    /// trying again for a moment where a file is held (see `remove_file`).
    pub fn forget_blocking(self, data_dir: &Path) {
        // After a save that is still under way, which would otherwise bring
        // the record back once it is deleted.
        let _writing = self.writing.as_deref().map(lock);
        if let Some(record) = self.record {
            write_tombstone(data_dir, &record.token, self.ended);
            if self.ended == Ended::Completed {
                record.complete_with(data_dir, true);
            } else {
                record.discard_with(data_dir, self.part.as_ref(), true);
            }
        }
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
    /// by then, and the record is marked as cancelled.
    LeftToStep {
        /// The mark is still to be saved ([`Session::save_record`]), so that
        /// a later start does not lose the cancel. Not for a send's cancel
        /// from the last run, issued again at this start (see
        /// `TransferRecord::cancelled`): its record says so already.
        to_save: bool,
        /// Sending side: all of the data has gone out, so the cancel waits
        /// for the receiver's verdict (`TransferRecord::done_sent`, read
        /// together with the stage) instead of ending the transfer in a
        /// moment, as the cancel of a send whose record was being saved for
        /// the first time does, and the last step of a receive.
        waits: bool,
    },
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
                placed: None,
                cancel_left: false,
            }),
            writing: Arc::new(Mutex::new(())),
            syncing: AtomicBool::new(false),
            open_part: Mutex::new(None),
        };
        (Arc::new(session), rx)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn ending(&self, record: Option<TransferRecord>, ended: Ended) -> Ending {
        Ending {
            record: record.map(Box::new),
            ended,
            writing: Some(self.writing.clone()),
            part: lock(&self.open_part).take(),
        }
    }

    /// Receiving side: `part`, the part file that a connection has just
    /// opened to write to, stays open until the result is dropped, with a
    /// second handle kept here. Nothing else can delete the part meanwhile
    /// (see `receive::open_own_part`), and its last write may still be under
    /// way when a cancel ends the transfer, so the end deletes it through
    /// that handle: it goes once the write is over (see
    /// [`Ending::forget_blocking`]). Fails with [`Cancelled`] if the transfer
    /// has ended while the part was opened, which only a cancel does then:
    /// the part goes once `part` is closed. Blocking.
    pub fn keep_part(self: &Arc<Self>, part: &fs::File) -> io::Result<KeptPart> {
        // Without it, the end deletes the part by its name, trying again for
        // a moment (see `remove_file`).
        let second = part.try_clone().ok();
        let mut kept = lock(&self.open_part);
        // Under that lock: the end takes the handle after the record (see
        // `ending`), so it gets this one, or the record is gone already.
        if self.state().record.is_none() {
            drop(kept);
            let _ = archive::delete_on_close(part);
            return Err(io::Error::other(Cancelled));
        }
        *kept = second;
        Ok(KeptPart(self.clone()))
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

    /// Sending side: remembers how much of the data has gone out (see
    /// `Side::Send::sent`).
    pub fn save_sent(&self, data_dir: &Path, gone_out: u64) {
        self.save_with(data_dir, |record| {
            if let Side::Send { sent, .. } = &mut record.side {
                *sent = gone_out;
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
        self.save_files(data_dir, files, None)
    }

    /// Like [`save_receive_files`](Self::save_receive_files), noting also
    /// that the part file, which the last step has renamed, has the identity
    /// `id` now (see `receive::Transfer::moved`).
    pub fn save_moved_part(
        &self,
        data_dir: &Path,
        files: &ReceiveFiles,
        id: FileId,
    ) -> io::Result<()> {
        self.save_files(data_dir, files, Some(id))
    }

    fn save_files(
        &self,
        data_dir: &Path,
        files: &ReceiveFiles,
        id: Option<FileId>,
    ) -> io::Result<()> {
        let ended = self.save_with(data_dir, |record| {
            if let Side::Receive {
                files: noted,
                part_id,
                ..
            } = &mut record.side
            {
                noted.clone_from(files);
                if id.is_some() {
                    *part_id = id;
                }
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
    /// leaves a tombstone so the other side learns later how it `ended`, and
    /// deletes the partial or temporary data. A cancel from now on comes too
    /// late: the transfer has ended the way the caller reports, unless it
    /// had ended before, which stands (see [`ended`](Self::ended)).
    pub fn end(&self, code: Option<u32>, ended: Ended) -> Ending {
        match code {
            Some(code) => self.close_current(code, b""),
            None => {
                self.set_current(None);
            }
        }
        let record = {
            let mut state = self.state();
            if !matches!(state.stage, Stage::Ended(_)) {
                state.stage = Stage::Ended(ended);
            }
            state.record.take()
        };
        self.ending(record, ended)
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
                Stage::HandedOver | Stage::Ended(_) => return Cancel::TooLate,
                Stage::Finishing => {
                    state.cancel_left = true;
                    let (to_save, waits) = match (state.record.as_mut(), self.role) {
                        (None, _) => (false, false),
                        (Some(record), Role::Receive) => (record.end_now(), false),
                        (Some(record), Role::Send) => {
                            // A cancelled send is not paused any more (see
                            // `Control::cancel`); the record says so at once,
                            // in case the run ends before the transfer's
                            // task notes it (a record that said cancelled
                            // already is not paused, see `load_all`).
                            record.paused = false;
                            (
                                !std::mem::replace(&mut record.cancelled, true),
                                record.done_sent,
                            )
                        }
                    };
                    return Cancel::LeftToStep { to_save, waits };
                }
                Stage::Running => {
                    state.stage = Stage::Ended(Ended::Cancelled);
                    state.record.take()
                }
            }
        };
        self.close_current(CLOSE_CANCELLED, b"");
        Cancel::Ended(self.ending(record, Ended::Cancelled))
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
            Cancel::LeftToStep { to_save, .. } => {
                if to_save {
                    let _ = self.save_record(data_dir);
                }
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
    /// which finds the one finds the other too ([`cancel`](Self::cancel)
    /// tells by it whether the cancel waits, `Cancel::LeftToStep`). Whether
    /// the record changed; the caller saves it then
    /// ([`save_record`](Self::save_record)).
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
            state.stage = Stage::Ended(Ended::Cancelled);
            state.record.take()
        };
        self.close_current(CLOSE_CANCELLED, b"");
        Some(self.ending(record, Ended::Cancelled))
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
        if !matches!(state.stage, Stage::Ended(_)) {
            state.stage = Stage::HandedOver;
        }
    }

    /// Whether the item is the receiver's (see [`hand_over`](Self::hand_over)).
    pub fn handed_over(&self) -> bool {
        self.state().stage == Stage::HandedOver
    }

    /// How the transfer has ended on this side, if it has ([`end`](Self::end),
    /// a cancel, or once the item is in place), with where a received item
    /// was put (see [`place`](Self::place)).
    pub fn ended(&self) -> Option<(Ended, Option<PathBuf>)> {
        let state = self.state();
        match state.stage {
            Stage::Ended(ended) => Some((ended, state.placed.clone())),
            _ => None,
        }
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
        let (record, item) = {
            let mut state = self.state();
            match placed {
                Ok(item) => (Self::complete(&mut state, &item), item),
                Err(e) => {
                    // Still running: it can be cancelled, or end as failed.
                    state.stage = before;
                    return Err(e);
                }
            }
        };
        self.ending(record, Ended::Completed)
            .forget_blocking(data_dir);
        Ok(item)
    }

    /// Receiving side: the item was in place at `item` already before a
    /// restart, and only the end of the transfer was interrupted (see
    /// `receive::Receiver::continue_restored`). Ends the transfer as
    /// completed, as [`place`](Self::place) does once it has put it there.
    pub fn end_placed(&self, item: &Path) -> Ending {
        self.set_current(None);
        let record = Self::complete(&mut self.state(), item);
        self.ending(record, Ended::Completed)
    }

    /// Ends the transfer as completed, with the item in place at `item`;
    /// the record, for the end to forget.
    fn complete(state: &mut State, item: &Path) -> Option<TransferRecord> {
        state.stage = Stage::Ended(Ended::Completed);
        state.placed = Some(item.to_path_buf());
        state.record.take()
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
                largest_file: 10,
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
            ended_at: None,
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

    /// A part with no more than what is known to be on disk is not opened
    /// for writing: it may be open in a program that lets nothing else
    /// write to it, such as an item already in place that the user opened.
    /// One with more is cut.
    #[test]
    fn a_part_with_nothing_to_cut_is_left_alone() {
        let dir = scratch("nothing-to-cut");
        let part = dir.join("a.docx");
        fs::write(&part, b"12345").unwrap();
        let r = record(37, receive_side(&dir, &part, Some(5)));
        let other = archive::testing::hold_readable(&part);
        r.truncate_to_synced().unwrap();
        drop(other);
        fs::write(&part, b"1234567").unwrap();
        r.truncate_to_synced().unwrap();
        assert_eq!(fs::read(&part).unwrap(), b"12345");
        fs::remove_dir_all(&dir).ok();
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
        let loaded = records(&dir);
        assert_eq!(loaded, vec![r.clone()]);
        assert_eq!(loaded[0].role(), Role::Receive);
        r.discard(&dir);
        assert!(!part.exists());
        assert!(records(&dir).is_empty());

        let zip = dir.join("send.zip");
        fs::write(&zip, b"zip").unwrap();
        let s = record(
            2,
            Side::Send {
                source: dir.clone(),
                source_len: 0,
                source_mtime: None,
                data_path: zip.clone(),
                sent: 0,
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
        assert!(records(&dir).is_empty());

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
        assert_eq!(tombstone(&dir, &[1; 16]), Some(Ended::Cancelled));

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
        assert_eq!(tombstone(&dir, &[2; 16]), Some(Ended::Completed));
        assert!(records(&dir).is_empty());
        // That end stands, with where the item is, also when the transfer
        // is ended again (after a panic, see `net::guarded`).
        let completed = Some((Ended::Completed, Some(placed.clone())));
        assert_eq!(session.ended(), completed);
        session.end(None, Ended::Failed).forget_blocking(&dir);
        assert_eq!(session.ended(), completed);
        assert_eq!(tombstone(&dir, &[2; 16]), Some(Ended::Completed));

        // A step that fails leaves the transfer running, so it can still
        // be cancelled (or ended as failed).
        let (session, _rx) = Session::new(3, [3; 16], peer, Role::Receive);
        session.set_record(record(3, receive_side(&dir, &dir.join("c.part"), None)));
        assert!(
            session
                .place(&dir, &mut || anyhow::bail!("blocked"))
                .is_err()
        );
        assert_eq!(session.ended(), None);
        assert!(session.cancel_blocking(&dir));
        assert_eq!(session.ended(), Some((Ended::Cancelled, None)));
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
        assert!(records(&dir)[0].ended);
        // The step learns about it when it notes its next change, which the
        // record keeps all the same, so that it always knows the files.
        let mut files = session.receive_files().unwrap();
        files.moving = Some(Owned {
            path: dir.join("a.bin"),
            id: archive::file_id(&part).unwrap(),
        });
        let error = session.save_receive_files(&dir, &files).unwrap_err();
        assert!(archive::is_cancelled(&error.into()));
        assert_eq!(records(&dir)[0].receive_files(), Some(&files));
        assert!(session.place(&dir, &mut || Ok(dir.join("a.bin"))).is_err());
        drop(finishing);
        assert!(session.finishing().is_err(), "it has ended");
        session.end(None, Ended::Cancelled).forget_blocking(&dir);
        assert!(!part.exists());
        assert!(records(&dir).is_empty());
        assert_eq!(tombstone(&dir, &[1; 16]), Some(Ended::Cancelled));
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
        assert!(records(&dir).is_empty());

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
        assert!(records(&dir).is_empty());

        // A part that another program holds open cannot be deleted yet.
        fs::write(&part, b"zip").unwrap();
        let held = archive::testing::hold(&part);
        let r = record(12, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        r.discard(&dir);
        let left = records(&dir);
        assert_eq!(left.len(), 1);
        assert!(left[0].ended, "the next start deletes it");
        drop(held);
        left[0].discard(&dir);
        assert!(!part.exists());
        assert!(records(&dir).is_empty());
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

    /// On FAT and exFAT, in records of earlier versions, where every item of
    /// a volume has the same identity, the sizes tell whether the part moved: in the last step it has all of
    /// the item. When both names have something of its size, the other
    /// records tell, and then the content: the old name keeps the part
    /// unless a newer transfer's record names it, or only what has the new
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
            *part_id = Some(archive::file_id(&part).unwrap().without_creation_time());
            files.moving = Some(Owned {
                path: placed.clone(),
                id: archive::file_id(&part).unwrap().without_creation_time(),
            });
        }
        assert!(r.part_is_there());
        // The records of this start: this one, and one of another transfer
        // whose part has the same name: one that goes on can only have taken
        // it once this one's part was renamed; one that ended had it before
        // this one (and only its record stayed behind), which tells nothing,
        // so it is not among the parts.
        let mine = parts_of([(&r, Verdict::GoesOn)]);
        assert_eq!(mine, vec![(r.token, part.clone())]);
        let mut other = r.clone();
        other.token = [19; 16];
        let theirs = parts_of([(&r, Verdict::GoesOn), (&other, Verdict::GoesOn)]);
        let mut earlier = r.clone();
        earlier.token = [20; 16];
        earlier.ended = true;
        let stale = parts_of([
            (&r, Verdict::GoesOn),
            (&earlier, Verdict::Over(Fate::Ended)),
        ]);
        assert_eq!(stale, mine);
        assert_eq!(
            parts_of([(&r, Verdict::GoesOn), (&other, Verdict::Completed)]),
            mine
        );

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
        // under the old name: its record tells the two apart if its transfer
        // goes on; without one (another profile's transfer, say), or with
        // only one that ended, the content does, as far as it can: an
        // identical copy under the old name is kept as the part.
        fs::write(&part, b"0123456789").unwrap();
        assert!(!r.clone().follow_moved_part(&mine));
        assert!(r.clone().follow_moved_part(&theirs));
        assert!(!r.clone().follow_moved_part(&stale));
        fs::write(&part, b"ten others").unwrap();
        assert!(r.clone().follow_moved_part(&mine));
        assert!(r.clone().follow_moved_part(&theirs));
        assert!(r.clone().follow_moved_part(&stale));
        // Without the hash (a record of an earlier version), the old name
        // keeps the part unless the record of a transfer that goes on
        // claims it.
        let mut unknown = r.clone();
        unknown.done_hash = None;
        assert!(!unknown.clone().follow_moved_part(&mine));
        assert!(!unknown.clone().follow_moved_part(&stale));
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
        assert!(records(&dir).is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    /// Folders that could not be deleted yet go with the transfer: a failed
    /// one keeps its record until they are gone, a completed one tries once.
    #[test]
    fn leftover_folders_go_with_the_transfer() {
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
        let held = archive::testing::hold(&old.join("half.jpg"));
        r.discard(&dir);
        assert!(records(&dir)[0].ended, "the next start tries again");
        drop(held);
        records(&dir)[0].discard(&dir);
        assert!(!old.exists() && !part.exists());
        assert!(records(&dir).is_empty());

        let r = with_leftover(16);
        r.complete(&dir);
        assert!(!old.exists() && !part.exists());
        assert!(records(&dir).is_empty());
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
                sent: 0,
            },
        ));
        assert!(!session.handed_over());
        session.hand_over();
        assert!(!session.cancel_blocking(&dir));
        assert!(zip.exists() && session.with_record(|_| ()).is_some());
        session.end(None, Ended::Completed).forget_blocking(&dir);
        assert!(!zip.exists());
        assert_eq!(tombstone(&dir, &[17; 16]), Some(Ended::Completed));
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
                sent: 0,
            },
        );
        record.save(dir).unwrap();
        (record, zip)
    }

    /// A send notes how much has gone out, which a restart shows until the
    /// receiver says how much it has; all of it once the receiver has
    /// confirmed the item.
    #[test]
    fn a_restored_send_has_what_had_gone_out() {
        let dir = scratch("gone-out");
        let peer = iroh::SecretKey::generate().public();
        let (session, _rx) = Session::new(1, [21; 16], peer, Role::Send);
        let (r, _zip) = send_record(&dir, 21);
        assert_eq!(r.have(), 0);
        session.set_record(r);
        session.save_sent(&dir, 7);
        let mut stored = records(&dir).remove(0);
        assert_eq!(stored.have(), 7);
        stored.done_hash = Some([0; 32]);
        assert_eq!(stored.have(), 10);
        fs::remove_dir_all(&dir).ok();
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
        let stored = &records(&dir)[0];
        assert!(!stored.ended && stored.cancelled && stored.cancel_pending());
        assert!(!stored.paused);
        assert!(stored.receiver_may_have_item());
        let ending = session.stop_deferring().expect("the cancel ends it now");
        ending.forget_blocking(&dir);
        assert!(!zip.exists());
        assert!(records(&dir).is_empty());
        assert_eq!(tombstone(&dir, &[20; 16]), Some(Ended::Cancelled));
        assert!(!session.cancel_blocking(&dir), "it has ended");

        // The receiver confirmed it: too late, also after a restart.
        let (session, _rx) = Session::new(2, [21; 16], peer, Role::Send);
        let (r, zip) = send_record(&dir, 21);
        session.set_record(r);
        session.defer_cancel();
        assert!(!session.cancel_blocking(&dir));
        session.hand_over();
        session.save_done_hash(&dir, [1; 32]);
        assert!(!records(&dir)[0].cancel_pending());
        assert!(session.stop_deferring().is_none());
        assert!(!session.cancel_blocking(&dir));
        session.end(None, Ended::Completed).forget_blocking(&dir);
        assert!(!zip.exists());
        assert_eq!(tombstone(&dir, &[21; 16]), Some(Ended::Completed));

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
            records(&dir)
                .iter()
                .any(|r| r.token == [22; 16] && r.done_sent)
        );
        assert!(session.stop_deferring().is_none());
        assert!(session.running() && !session.deferring());
        session.clear_done_sent(&dir);
        let stored = records(&dir);
        let stored = stored.iter().find(|r| r.token == [22; 16]).unwrap();
        assert!(!stored.done_sent && !stored.receiver_may_have_item());
        assert!(session.cancel_blocking(&dir));
        fs::remove_dir_all(&dir).ok();
    }

    /// A record that says a send is both cancelled and paused (one of an
    /// earlier version) is put right when it is read, on disk too: the
    /// cancel waits on, and the transfer dials for the receiver's verdict.
    #[test]
    fn a_cancelled_send_is_not_paused_when_read() {
        let dir = scratch("cancelled-paused");
        let (mut r, _zip) = send_record(&dir, 26);
        r.paused = true;
        r.cancelled = true;
        r.done_sent = true;
        r.save(&dir).unwrap();
        let loaded = records(&dir);
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].cancel_pending() && !loaded[0].paused);
        let text = fs::read_to_string(record_path(&dir, &r.token)).unwrap();
        let stored: TransferRecord = serde_json::from_str(&text).unwrap();
        assert!(stored.cancelled && !stored.paused, "on disk too");
        // A pause that is not contradicted stays.
        r.cancelled = false;
        r.save(&dir).unwrap();
        assert!(records(&dir)[0].paused);
        fs::remove_dir_all(&dir).ok();
    }

    /// Only the record of a transfer that goes on claims a part's old name
    /// for `follow_moved_part`; one whose item completed (its tombstone says
    /// so, only the record stayed behind) had the name before and is left
    /// out, so the content decides then.
    #[test]
    fn a_completed_record_claims_no_part_name() {
        let dir = scratch("claim-completed");
        archive::PRETEND_FAT.set(true);
        let part = dir.join("b.bin.part");
        let placed = dir.join("b.bin");
        fs::write(&part, b"0123456789").unwrap();
        fs::write(&placed, b"0123456789").unwrap();
        let mut r = record(27, receive_side(&dir, &part, None));
        r.done_hash = Some(*blake3::hash(b"0123456789").as_bytes());
        if let Side::Receive { part_id, files, .. } = &mut r.side {
            *part_id = Some(archive::file_id(&part).unwrap().without_creation_time());
            files.moving = Some(Owned {
                path: placed.clone(),
                id: archive::file_id(&part).unwrap().without_creation_time(),
            });
        }
        r.save(&dir).unwrap();
        // Another record names the old name as its part, but its transfer
        // completed: the identical copy under the old name stays this one's.
        let mut other = r.clone();
        other.token = [28; 16];
        if let Some(files) = other.receive_files_mut() {
            files.moving = None;
        }
        other.save(&dir).unwrap();
        write_tombstone(&dir, &other.token, Ended::Completed);
        let loaded = records(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].receive_files().unwrap().part, part);
        assert!(part.exists() && placed.exists());
        // A transfer that goes on with that name took it after the rename.
        other.save(&dir).unwrap();
        let _ = fs::remove_file(tombstone_path(&dir, &other.token));
        let loaded = records(&dir);
        assert_eq!(loaded.len(), 2);
        let mine = loaded.iter().find(|l| l.token == r.token).unwrap();
        assert_eq!(mine.receive_files().unwrap().part, placed);
        archive::PRETEND_FAT.set(false);
        fs::remove_dir_all(&dir).ok();
    }

    /// On FAT and exFAT, where items have no numbers, an item of a transfer
    /// is told by its creation time (see `FileId`): a file made under the
    /// part's name later, such as another transfer's part after the user
    /// deleted this one, is neither continued, cut, counted, deleted nor
    /// taken for the part when a moved part is followed, and a folder made
    /// under the name of one that a receive was unpacking into is not
    /// deleted. The part read with another time zone (FAT keeps local time)
    /// is still the part, as far as time zones lie apart.
    #[test]
    fn items_are_told_by_their_creation_time_on_fat() {
        let dir = scratch("created-fat");
        archive::PRETEND_FAT.set(true);
        let made = |r: &mut TransferRecord, part: &Path| {
            if let Side::Receive { part_id, .. } = &mut r.side {
                *part_id = Some(archive::created_now(part).unwrap());
            }
            fs::metadata(part).unwrap().created().unwrap()
        };
        let part = dir.join("a.bin.part");
        fs::write(&part, b"0123456").unwrap();
        let mut r = record(36, receive_side(&dir, &part, Some(5)));
        let created = made(&mut r, &part);
        assert!(r.part_is_there() && r.can_go_on());
        assert_eq!(r.have(), 5);
        let at = |time: SystemTime| archive::set_times(&part, None, Some(time)).unwrap();
        let hour = Duration::from_secs(60 * 60);
        at(created - hour);
        assert!(r.part_is_there(), "read after a change of the time zone");
        at(created + 26 * hour);
        assert!(r.part_is_there(), "from 12 hours behind UTC to 14 ahead");
        at(created + 27 * hour);
        assert!(!r.part_is_there(), "further apart than time zones");
        at(created + Duration::from_millis(10));
        assert!(!r.part_is_there(), "created a moment later");
        at(created);
        r.truncate_to_synced().unwrap();
        assert_eq!(fs::read(&part).unwrap(), b"01234");
        // The user deleted it, and another transfer's part has the name now.
        fs::remove_file(&part).unwrap();
        fs::write(&part, b"another").unwrap();
        at(created + Duration::from_millis(1230));
        assert!(!r.part_is_there() && !r.can_go_on());
        assert_eq!(r.have(), 0, "nothing of it is this transfer's");
        assert!(r.truncate_to_synced().is_err());
        assert!(r.open_part(fs::OpenOptions::new().append(true)).is_err());
        r.save(&dir).unwrap();
        r.discard(&dir);
        assert_eq!(
            fs::read(&part).unwrap(),
            b"another",
            "neither cut nor deleted"
        );
        assert!(records(&dir).is_empty());
        // A record of an earlier version does not know the time: the name
        // tells, as before.
        if let Side::Receive { part_id, .. } = &mut r.side {
            *part_id = part_id.map(FileId::without_creation_time);
        }
        assert!(r.part_is_there());
        assert_eq!(r.have(), 5);

        // Renamed right before a crash, and under the old name now another
        // profile's part of an identical item, made at another time: the
        // part has moved.
        let part = dir.join("b.bin.part");
        let placed = dir.join("b.bin");
        fs::write(&part, b"0123456789").unwrap();
        let mut r = record(37, receive_side(&dir, &part, None));
        let created = made(&mut r, &part);
        r.done_hash = Some(*blake3::hash(b"0123456789").as_bytes());
        if let Side::Receive { part_id, files, .. } = &mut r.side {
            files.moving = Some(Owned {
                path: placed.clone(),
                id: part_id.unwrap(),
            });
        }
        let kept = r.clone();
        assert!(!r.follow_moved_part(&[]), "not renamed");
        fs::rename(&part, &placed).unwrap();
        fs::write(&part, b"0123456789").unwrap();
        archive::set_times(&part, None, Some(created + Duration::from_secs(5))).unwrap();
        assert!(r.follow_moved_part(&[]));
        assert_eq!(r.placed_item(), Some(placed.as_path()));
        // Without the times, the content tells, and keeps the old name.
        let mut earlier = kept;
        if let Side::Receive { part_id, files, .. } = &mut earlier.side {
            *part_id = part_id.map(FileId::without_creation_time);
            if let Some(moving) = &mut files.moving {
                moving.id = moving.id.without_creation_time();
            }
        }
        assert!(!earlier.follow_moved_part(&[]));

        // A receive was unpacking into "Photos"; the user deleted it and made
        // a folder of that name later, which is not the transfer's to delete.
        let folder = dir.join("Photos");
        fs::create_dir(&folder).unwrap();
        let unpacking = Owned::made_at(&folder).unwrap();
        let made_then = fs::metadata(&folder).unwrap().created().unwrap();
        fs::remove_dir(&folder).unwrap();
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("mine.jpg"), b"x").unwrap();
        archive::set_times(&folder, None, Some(made_then + Duration::from_secs(90))).unwrap();
        assert!(!unpacking.is_there());
        assert!(unpacking.remove_folder(), "gone, as far as it is concerned");
        assert!(folder.join("mine.jpg").exists());
        archive::set_times(&folder, None, Some(made_then)).unwrap();
        assert!(
            unpacking.remove_folder() && !folder.exists(),
            "its own goes"
        );
        archive::PRETEND_FAT.set(false);
        fs::remove_dir_all(&dir).ok();
    }

    /// A record that expires still claims its part's name: its part is
    /// under that name until `load_all` deletes it, so another transfer
    /// could only have taken the name after its own rename. The renamed
    /// part is followed, and the expired one's goes, whichever record is
    /// read first: `load_all` reads the records in the order of their names,
    /// which is the order of the tokens.
    #[test]
    fn an_expired_record_claims_its_part_name() {
        expired_record_claims_its_part_name("claim-expired", 29, 30);
        expired_record_claims_its_part_name("claim-expired-first", 30, 29);
    }

    fn expired_record_claims_its_part_name(name: &str, mine: u8, stale: u8) {
        let dir = scratch(name);
        archive::PRETEND_FAT.set(true);
        let part = dir.join("c.bin.part");
        let placed = dir.join("c.bin");
        fs::write(&part, b"0123456789").unwrap();
        fs::write(&placed, b"0123456789").unwrap();
        let mut r = record(mine, receive_side(&dir, &part, None));
        r.done_hash = Some(*blake3::hash(b"0123456789").as_bytes());
        if let Side::Receive { part_id, files, .. } = &mut r.side {
            *part_id = Some(archive::file_id(&part).unwrap().without_creation_time());
            files.moving = Some(Owned {
                path: placed.clone(),
                id: archive::file_id(&part).unwrap().without_creation_time(),
            });
        }
        r.save(&dir).unwrap();
        // Another receive of an identical item took the old name after the
        // rename, and has gone stale since.
        let other = record(stale, receive_side(&dir, &part, None));
        other.save(&dir).unwrap();
        let long_ago = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(record_path(&dir, &other.token))
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        started_before(&dir);
        let found = load_all(&dir);
        assert_eq!(found.going_on.len(), 1);
        let mine = &found.going_on[0];
        assert_eq!(mine.token, r.token);
        assert_eq!(mine.receive_files().unwrap().part, placed);
        assert_eq!(mine.placed_item(), Some(placed.as_path()));
        assert_eq!(found.over.len(), 1);
        let (expired, fate) = &found.over[0];
        assert_eq!((expired.token, *fate), (other.token, Fate::Expired));
        assert_eq!(expired.receive_files().unwrap().part, part);
        assert!(!part.exists(), "the expired record took its part along");
        assert!(
            !record_path(&dir, &other.token).exists(),
            "and went once it had"
        );
        assert!(placed.exists());
        archive::PRETEND_FAT.set(false);
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
        let ending = session.end(None, Ended::Completed);
        assert!(matches!(session.cancel(), Cancel::TooLate));
        ending.forget_blocking(&dir);
        assert!(!zip.exists());
        assert_eq!(tombstone(&dir, &[23; 16]), Some(Ended::Completed));
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
        write_tombstone(&dir, &r.token, Ended::Completed);
        assert!(records(&dir).is_empty());
        assert_eq!(fs::read(folder.join("a.jpg")).unwrap(), b"received");
        assert!(!temp.exists(), "only what was temporary goes");
        assert!(!record_path(&dir, &r.token).exists());
        assert_eq!(tombstone(&dir, &r.token), Some(Ended::Completed));
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
        session.end(None, Ended::Cancelled).forget_blocking(&dir);
        for saver in savers {
            saver.join().unwrap();
        }
        assert!(records(&dir).is_empty());
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
        let loaded = records(&dir);
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
        let loaded = records(&dir);
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
        assert_eq!(records(&dir).len(), 1);
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

    /// Opens `path` the way another program that holds it open does: it
    /// cannot be deleted until the handle is dropped.
    fn hold(path: &Path) -> archive::testing::Held {
        archive::testing::hold(path)
    }

    /// Sets the modification time of the record file of `r`.
    fn age_record(dir: &Path, r: &TransferRecord, time: SystemTime) {
        fs::File::options()
            .write(true)
            .open(record_path(dir, &r.token))
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    /// Something is old only if it was older than `MAX_AGE` at this start
    /// and at the last one, so that a clock that was wrong at one start
    /// ends nothing; without a last start, nothing is old. Each start notes
    /// its time for the next.
    #[test]
    fn the_age_needs_two_starts_to_agree() {
        let day = Duration::from_secs(24 * 60 * 60);
        let then = SystemTime::UNIX_EPOCH + 20_000 * day;
        let clock = |now: u32, last_start: Option<u32>| Clock {
            now: then + now * day,
            last_start: last_start.map(|days| then + days * day),
        };
        assert!(clock(31, Some(31)).is_old(then));
        assert!(clock(60, Some(31)).is_old(then));
        assert!(!clock(30, Some(30)).is_old(then), "not longer than MAX_AGE");
        assert!(!clock(31, None).is_old(then), "no last start known");
        // The clock is years ahead at this start, or was at the last one.
        assert!(!clock(2000, Some(10)).is_old(then));
        assert!(!clock(10, Some(2000)).is_old(then));
        // It went back: what was written then is in the future now.
        assert!(!clock(31, Some(31)).is_old(then + 40 * day));

        let dir = scratch("clock");
        let first = Clock::start(&dir);
        assert_eq!(first.last_start, None);
        let second = Clock::start(&dir);
        let noted = second.last_start.unwrap();
        assert!(noted <= first.now && first.now.duration_since(noted).unwrap() < day);
        fs::write(records_dir(&dir).join(LAST_START), "not a time").unwrap();
        assert_eq!(Clock::start(&dir).last_start, None, "a damaged note");
        fs::write(records_dir(&dir).join(LAST_START), u64::MAX.to_string()).unwrap();
        assert_eq!(Clock::start(&dir).last_start, None, "a note out of range");
        // It is no record and no old file of one.
        fs::File::options()
            .write(true)
            .open(records_dir(&dir).join(LAST_START))
            .unwrap()
            .set_modified(SystemTime::now() - MAX_AGE - day)
            .unwrap();
        assert!(records(&dir).is_empty());
        assert!(records_dir(&dir).join(LAST_START).exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// A transfer in which nothing happened for longer than `MAX_AGE` is
    /// ended by `load_all`, with its files, before any transfer goes on: the
    /// record goes once they are gone, and stays, ended, while they cannot
    /// be deleted.
    #[test]
    fn expired_records_take_their_data_along() {
        let dir = scratch("expired");
        let part = dir.join("old.part");
        fs::write(&part, b"old").unwrap();
        let r = record(6, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        let long_ago = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        age_record(&dir, &r, long_ago);
        let found = load_all(&dir);
        assert_eq!(
            found.going_on,
            std::slice::from_ref(&r),
            "no last start: not old yet"
        );
        assert!(found.over.is_empty());
        let found = load_all(&dir);
        assert!(found.going_on.is_empty());
        assert_eq!(found.over, [(r.clone(), Fate::Expired)]);
        assert!(
            !part.exists(),
            "the partial data of an expired record is deleted at once"
        );
        assert!(!record_path(&dir, &r.token).exists(), "and then the record");
        assert!(records(&dir).is_empty());

        // A part that another program holds open cannot be deleted now: the
        // transfer ends all the same, and its record stays, ended now, for
        // a later start to delete the part.
        let stuck = dir.join("stuck.part");
        fs::write(&stuck, b"stuck").unwrap();
        let s = record(7, receive_side(&dir, &stuck, None));
        s.save(&dir).unwrap();
        age_record(&dir, &s, long_ago);
        let held = hold(&stuck);
        let found = load_all(&dir);
        assert_eq!(found.over.len(), 1);
        assert_eq!(found.over[0].1, Fate::Expired);
        assert!(stuck.exists());
        drop(held);
        let found = load_all(&dir);
        assert_eq!(found.over.len(), 1);
        let (ended, fate) = &found.over[0];
        assert_eq!(*fate, Fate::Ended, "it ended at the last start");
        assert!(ended.ended);
        let since = SystemTime::now().duration_since(ended.ended_at.unwrap());
        assert!(since.unwrap() < Duration::from_secs(60 * 60));
        ended.discard(&dir);
        assert!(!stuck.exists());
        assert!(records(&dir).is_empty());

        // A receive that has all of its item does not expire: its item is
        // in place already, or it is put in place without the sender.
        let make_old = |r: &TransferRecord| {
            r.save(&dir).unwrap();
            age_record(&dir, r, long_ago);
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
        let found = load_all(&dir);
        assert!(found.over.is_empty());
        assert_eq!(found.going_on.len(), 2);
        assert_eq!(
            found
                .going_on
                .iter()
                .find(|r| r.token == [26; 16])
                .unwrap()
                .placed_item(),
            Some(item.as_path())
        );
        assert!(item.exists() && whole.exists());

        // Unless it has ended: then what it renamed goes, and the record is
        // kept until the start has deleted its files (as `net::restore` does
        // with every ended record).
        let mut ended = verified.clone();
        ended.ended = true;
        // It ended some days ago: following its part now does not make it
        // younger, so that it is given up `MAX_AGE` after its end.
        ended.ended_at = Some(SystemTime::now() - Duration::from_secs(10 * 24 * 60 * 60));
        if let Some(files) = ended.receive_files_mut() {
            files.moving = Some(Owned {
                path: dir.join("b.bin"),
                id: archive::file_id(&whole).unwrap(),
            });
        }
        ended.save(&dir).unwrap();
        fs::rename(&whole, dir.join("b.bin")).unwrap();
        let found = load_all(&dir);
        assert_eq!(found.going_on.len(), 1);
        assert_eq!(found.over.len(), 1);
        let (loaded, fate) = &found.over[0];
        assert_eq!(*fate, Fate::Ended);
        assert_eq!(loaded.receive_files().unwrap().part, dir.join("b.bin"));
        let saved: TransferRecord =
            serde_json::from_str(&fs::read_to_string(record_path(&dir, &ended.token)).unwrap())
                .unwrap();
        assert_eq!(
            saved.receive_files().unwrap().part,
            dir.join("b.bin"),
            "saved as followed"
        );
        assert_eq!(saved.ended_at, ended.ended_at, "as old as before");
        assert!(dir.join("b.bin").exists(), "left to the start to delete");
        loaded.discard(&dir);
        assert!(!dir.join("b.bin").exists());
        assert_eq!(records(&dir).len(), 1);
        assert!(item.exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// A record that has ended stays while its files cannot be deleted, so
    /// that every start tries again, but not forever: once its transfer
    /// ended longer ago than `MAX_AGE` (`ended_at`, or for a record of an
    /// earlier version the last write of its file), it is given up. The
    /// start deletes what it can of its files once more, in the background
    /// like those of the other ended records (`net::restore`), and then
    /// forgets the record, so that no later start tries again.
    #[test]
    fn an_ended_record_is_given_up_after_a_while() {
        let dir = scratch("ended-given-up");
        let part = dir.join("stuck.part");
        fs::write(&part, b"stuck").unwrap();
        let mut r = record(31, receive_side(&dir, &part, None));
        r.ended = true;
        r.save(&dir).unwrap();
        let held = hold(&part);
        let loaded = records(&dir);
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].ended);
        loaded[0].discard(&dir);
        assert!(part.exists(), "could not be deleted");
        assert!(
            record_path(&dir, &r.token).exists(),
            "kept for the next start"
        );
        let long_ago = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        age_record(&dir, &r, long_ago);
        let found = load_all(&dir);
        assert_eq!(
            found.over.len(),
            1,
            "given up: its files are tried once more"
        );
        let (given_up, fate) = &found.over[0];
        assert_eq!(*fate, Fate::GivenUp);
        assert!(given_up.ended);
        assert!(
            record_path(&dir, &r.token).exists(),
            "the record stays until then"
        );
        given_up.give_up(&dir);
        assert!(part.exists(), "still cannot be deleted");
        assert!(!record_path(&dir, &r.token).exists());
        assert!(records(&dir).is_empty(), "no later start tries again");

        // The record of this version knows when its transfer ended, however
        // recently its file was written.
        r.ended_at = Some(long_ago);
        r.save(&dir).unwrap();
        assert_eq!(load_all(&dir).over[0].1, Fate::GivenUp);
        r.ended_at = Some(SystemTime::now());
        r.save(&dir).unwrap();
        age_record(&dir, &r, long_ago);
        assert_eq!(load_all(&dir).over[0].1, Fate::Ended);
        drop(held);
        fs::remove_dir_all(&dir).ok();
    }

    /// What a given-up record's files are: those of a receive where a rename
    /// right before the end put them, the folder it was being unpacked into,
    /// and the temporary archive of a send, but never what the user chose
    /// to send.
    #[test]
    fn a_given_up_record_tries_its_files_once_more() {
        let dir = scratch("given-up-files");
        let long_ago = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
        // A folder whose zip is kept too: its zip was renamed into place and
        // it was being unpacked when the transfer ended, and neither could
        // be deleted then.
        let part = dir.join("d.zip.part");
        let placed = dir.join("d.zip");
        fs::write(&part, b"0123456789").unwrap();
        let unpacking = dir.join("d");
        fs::create_dir_all(&unpacking).unwrap();
        fs::write(unpacking.join("one.txt"), b"1").unwrap();
        let mut r = record(32, receive_side(&dir, &part, Some(10)));
        r.offer.kind = Kind::Folder;
        r.ended = true;
        r.ended_at = Some(long_ago);
        if let Side::Receive {
            part_id,
            files,
            folder,
            ..
        } = &mut r.side
        {
            *part_id = Some(archive::file_id(&part).unwrap());
            *folder = ReceivedFolder::Both;
            files.moving = Some(Owned {
                path: placed.clone(),
                id: archive::file_id(&part).unwrap(),
            });
            files.unpacking = Some(Owned::at(&unpacking).unwrap());
        }
        r.save(&dir).unwrap();
        fs::rename(&part, &placed).unwrap();
        // A send of a folder, whose zip could not be deleted then.
        let source = dir.join("Photos");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("one.jpg"), b"1").unwrap();
        let zip = dir.join("send.zip");
        fs::write(&zip, b"zip").unwrap();
        let mut s = record(
            33,
            Side::Send {
                source: source.clone(),
                source_len: 0,
                source_mtime: None,
                data_path: zip.clone(),
                sent: 0,
            },
        );
        s.ended = true;
        s.ended_at = Some(long_ago);
        s.save(&dir).unwrap();

        started_before(&dir);
        let found = load_all(&dir);
        assert!(found.going_on.is_empty());
        assert_eq!(found.over.len(), 2);
        assert!(found.over.iter().all(|(_, fate)| *fate == Fate::GivenUp));
        let (receive, _) = found.over.iter().find(|(l, _)| l.token == r.token).unwrap();
        assert_eq!(
            receive.receive_files().unwrap().part,
            placed,
            "followed the renamed part"
        );
        assert!(
            placed.exists() && unpacking.exists() && zip.exists(),
            "left to the start to delete"
        );
        for (l, _) in &found.over {
            l.give_up(&dir);
        }
        assert!(!placed.exists() && !part.exists());
        assert!(!unpacking.exists());
        assert!(!zip.exists());
        assert!(
            source.join("one.jpg").exists(),
            "what the user chose to send stays"
        );
        assert!(records(&dir).is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    /// A record kept because some of its files could not be deleted names
    /// only those: a name whose file is gone may be another transfer's by
    /// the next try, and on FAT and exFAT only the name would tell.
    #[test]
    fn a_record_that_stays_names_only_what_is_left() {
        let dir = scratch("what-is-left");
        archive::PRETEND_FAT.set(true);
        let zip = dir.join("d.zip");
        fs::write(&zip, b"0123456789").unwrap();
        let unpacking = dir.join("d");
        fs::create_dir_all(&unpacking).unwrap();
        fs::write(unpacking.join("one.txt"), b"1").unwrap();
        let mut r = record(34, receive_side(&dir, &zip, Some(10)));
        r.offer.kind = Kind::Folder;
        if let Side::Receive {
            part_id,
            files,
            folder,
            ..
        } = &mut r.side
        {
            *part_id = Some(archive::file_id(&zip).unwrap());
            *folder = ReceivedFolder::Both;
            files.moving = Some(Owned::at(&zip).unwrap());
            files.unpacking = Some(Owned::at(&unpacking).unwrap());
        }
        r.save(&dir).unwrap();
        let held = hold(&unpacking.join("one.txt"));
        r.discard(&dir);
        assert!(!zip.exists());
        assert!(unpacking.exists(), "could not be deleted");
        let left = &records(&dir)[0];
        assert!(left.ended && left.ended_at.is_some());
        let files = left.receive_files().unwrap();
        assert!(files.part_gone && files.moving.is_none());
        assert_eq!(files.unpacking, None);
        assert_eq!(files.leftovers, [Owned::at(&unpacking).unwrap()]);
        assert!(!left.part_is_there());
        // Another transfer's zip of the same name, received since.
        fs::write(&zip, b"another").unwrap();
        drop(held);
        let found = load_all(&dir);
        assert_eq!(found.over.len(), 1);
        let (left, fate) = &found.over[0];
        assert_eq!(*fate, Fate::Ended);
        assert_eq!(left.receive_files().unwrap().part, zip, "not followed");
        left.discard(&dir);
        assert!(!unpacking.exists());
        assert_eq!(fs::read(&zip).unwrap(), b"another");
        assert!(records(&dir).is_empty());
        archive::PRETEND_FAT.set(false);
        fs::remove_dir_all(&dir).ok();
    }

    /// A record is read only from the file of its token: a copy of one (a
    /// backup put back, or one made in Explorer) is set aside like a damaged
    /// record, not restored as a second transfer.
    #[test]
    fn a_copy_of_a_record_is_no_record() {
        let dir = scratch("copy");
        let part = dir.join("a.bin.part");
        fs::write(&part, b"12345").unwrap();
        let r = record(35, receive_side(&dir, &part, None));
        r.save(&dir).unwrap();
        let copy = records_dir(&dir).join(format!("{} - Copy.json", token_hex(&r.token)));
        fs::copy(record_path(&dir, &r.token), &copy).unwrap();
        assert_eq!(records(&dir), std::slice::from_ref(&r));
        assert!(!copy.exists());
        assert!(copy.with_extension("json.bad").exists());
        assert!(record_path(&dir, &r.token).exists());
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

    /// A tombstone tells how the transfer ended; a completed one stays
    /// completed, and the "ended" of earlier versions reads as cancelled.
    #[test]
    fn tombstones() {
        let dir = scratch("tomb");
        assert_eq!(tombstone(&dir, &[3; 16]), None);
        write_tombstone(&dir, &[3; 16], Ended::Cancelled);
        write_tombstone(&dir, &[4; 16], Ended::Completed);
        write_tombstone(&dir, &[5; 16], Ended::Failed);
        assert_eq!(tombstone(&dir, &[3; 16]), Some(Ended::Cancelled));
        assert_eq!(tombstone(&dir, &[4; 16]), Some(Ended::Completed));
        assert_eq!(tombstone(&dir, &[5; 16]), Some(Ended::Failed));
        write_tombstone(&dir, &[4; 16], Ended::Failed);
        assert_eq!(tombstone(&dir, &[4; 16]), Some(Ended::Completed));
        fs::write(tombstone_path(&dir, &[6; 16]), "ended").unwrap();
        assert_eq!(tombstone(&dir, &[6; 16]), Some(Ended::Cancelled));
        fs::remove_dir_all(&dir).ok();
    }

    /// On FAT and exFAT, a file made under the part's name moments after the
    /// part was deleted gets the part's creation time, and so its identity
    /// (see `TransferRecord::part_shorter`). One that is shorter than what
    /// reached the disk is still not the part: it is not opened, cut or
    /// deleted. Elsewhere the part's number tells, and a part that lost its
    /// end stays the part.
    #[test]
    fn a_shorter_file_under_the_parts_name_on_fat_is_not_the_part() {
        #[cfg(target_os = "macos")]
        use std::os::macos::fs::FileTimesExt;
        #[cfg(windows)]
        use std::os::windows::fs::FileTimesExt;
        let dir = scratch("shorter-fat");
        let part = dir.join("d.bin.part");
        for fat in [true, false] {
            archive::PRETEND_FAT.set(fat);
            fs::write(&part, b"0123456789").unwrap();
            let mut r = record(31, receive_side(&dir, &part, Some(8)));
            if let Side::Receive { part_id, .. } = &mut r.side {
                *part_id = Some(archive::file_id(&part).unwrap());
            }
            assert!(r.part_is_there());
            let created = fs::metadata(&part).unwrap().created().unwrap();
            fs::remove_file(&part).unwrap();
            fs::write(&part, b"new").unwrap();
            let times = fs::FileTimes::new().set_created(created);
            fs::File::options()
                .write(true)
                .open(&part)
                .unwrap()
                .set_times(times)
                .unwrap();
            if fat {
                assert!(
                    r.part_identity()
                        .unwrap()
                        .same_item(&archive::file_id(&part).unwrap())
                );
                assert!(!r.part_is_there());
                let opened = r.open_part(fs::OpenOptions::new().read(true));
                assert_eq!(
                    opened.err().unwrap().to_string(),
                    crate::i18n::tr!(error_part_missing)
                );
                assert!(r.truncate_to_synced().is_err());
                r.discard(&dir);
                assert_eq!(fs::read(&part).unwrap(), b"new", "neither cut nor deleted");
            } else {
                // Another file by its number.
                assert!(!r.part_is_there());
            }
            fs::remove_file(&part).ok();
        }
        // The part itself, shorter than noted (a disk that lost its end):
        // still the part, which goes on from what is there.
        fs::write(&part, b"012").unwrap();
        let mut r = record(32, receive_side(&dir, &part, Some(8)));
        if let Side::Receive { part_id, .. } = &mut r.side {
            *part_id = Some(archive::file_id(&part).unwrap());
        }
        assert!(r.part_is_there());
        assert_eq!(r.have(), 3);
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
                sent: 0,
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
                sent: 0,
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
