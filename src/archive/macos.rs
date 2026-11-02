//! macOS: dates, attributes, the identity of files and renames that replace
//! nothing, on POSIX.
//!
//! POSIX lets no one keep a file to themselves. Where Windows stops other
//! programs with a share mode, here they can write to the part of a receive,
//! rename it or delete it while it is open, and deleting never waits for
//! anyone. So `receive` checks the part on disk where Windows relies on the
//! share mode, and a part that was deleted or moved meanwhile is noticed by
//! its identity (`receive::Receiver::pump`).

use std::{
    ffi::{CStr, CString, c_char},
    fs::{File, Metadata, OpenOptions},
    io,
    mem::MaybeUninit,
    os::{
        fd::AsRawFd,
        macos::fs::MetadataExt as _,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt as _, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    time::SystemTime,
};

#[cfg(test)]
use super::PRETEND_FAT;
use super::{
    FileId, ItemTimes, RENAME_RETRY_MS, file_times, filetime_to_system, retried, system_to_filetime,
};
use crate::platform::{c_text, statfs_of};

/// Nothing is opened through a link at the end of the path, and a named pipe
/// that has taken a name does not block the opening.
const SAFE_OPEN: i32 = libc::O_NOFOLLOW | libc::O_NONBLOCK;

fn c_path(path: &Path) -> io::Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

/// How to open a file or folder only to look at it or to change its dates.
fn look_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(SAFE_OPEN);
    options
}

pub(super) fn item_times(meta: &Metadata) -> ItemTimes {
    let known = |time: io::Result<SystemTime>| time.ok().and_then(system_to_filetime);
    ItemTimes {
        modified: known(meta.modified()),
        accessed: known(meta.accessed()),
        created: known(meta.created()),
    }
}

/// Sets the modified and created dates of a file or folder.
pub fn set_times(
    path: &Path,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
) -> io::Result<()> {
    let Some(times) = file_times(modified, created) else {
        return Ok(());
    };
    look_options().open(path)?.set_times(times)
}

/// Marks a received file as coming from another computer, as browsers mark
/// downloads, so that Gatekeeper asks before it is first opened. The mark is
/// the `com.apple.quarantine` attribute: flags for a download, the time, who
/// wrote it, and an id. File systems without attributes keep it beside the
/// file, or not at all, which is fine.
pub fn mark_of_the_web(path: &Path) -> io::Result<()> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let id: [u8; 16] = rand::random();
    let id: String = id.iter().map(|b| format!("{b:02X}")).collect();
    let value = format!(
        "0083;{now:08x};Dodavo;{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    );
    let path = c_path(path)?;
    // SAFETY: a C string for the path, another for the name, and the bytes of
    // `value` with their length; all outlive the call.
    let status = unsafe {
        libc::setxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A folder gets the mark as well: an app that is a folder is checked by the
/// mark on that folder.
pub(super) fn mark_folder(path: &Path) {
    let _ = mark_of_the_web(path);
}

/// The local time of `filetime`, with the daylight saving rules of that date:
/// year, month, day, hour, minute and second.
pub(super) fn local_time(filetime: u64) -> Option<(u16, u8, u8, u8, u8, u8)> {
    let seconds = filetime_to_system(filetime).map(|time| {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(since) => since.as_secs() as libc::time_t,
            Err(before) => -(before.duration().as_secs() as libc::time_t),
        }
    })?;
    let mut local = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both point to local values, which the call reads and fills.
    if unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() {
        return None;
    }
    // SAFETY: filled by the call.
    let local = unsafe { local.assume_init() };
    Some((
        u16::try_from(local.tm_year + 1900).ok()?,
        (local.tm_mon + 1) as u8,
        local.tm_mday as u8,
        local.tm_hour as u8,
        local.tm_min as u8,
        local.tm_sec as u8,
    ))
}

/// Whether the file or folder is hidden.
pub(super) fn is_hidden(meta: &Metadata) -> bool {
    meta.st_flags() & libc::UF_HIDDEN != 0
}

/// How [`unzip`](super::unzip) opens the file of an entry: only a new one, so
/// that entries whose names are the same here keep their data apart.
pub(super) fn unpack_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NONBLOCK);
    options
}

/// Makes the file or folder at `path` hidden (see [`hide_open`]).
pub(super) fn hide(path: &Path) -> io::Result<()> {
    hide_open(&look_options().open(path)?)
}

/// Makes the open file or folder `item` hidden, keeping its other flags.
pub(super) fn hide_open(item: &File) -> io::Result<()> {
    let flags = item.metadata()?.st_flags() | libc::UF_HIDDEN;
    // SAFETY: the descriptor is open during the call.
    if unsafe { libc::fchflags(item.as_raw_fd(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether something has the name `path`. Deleting frees a name at once here,
/// whoever still has the file open.
pub fn name_taken(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Whether creating something at `path` failed because the name is taken.
pub(super) fn is_taken(error: &io::Error, _path: &Path) -> bool {
    error.kind() == io::ErrorKind::AlreadyExists
}

/// What a volume's file system says of the identity of its items.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Numbers {
    /// APFS and HFS+: an item keeps its number, and no other has it.
    Stable,
    /// FAT and exFAT: the number is made up, so the creation time tells.
    ByCreationTime,
    /// Elsewhere (SMB, say) nothing is known of them: the name tells.
    Unknown,
}

fn numbers_of(volume: &libc::statfs) -> Numbers {
    #[cfg(test)]
    if PRETEND_FAT.get() {
        return Numbers::ByCreationTime;
    }
    match c_text(&volume.f_fstypename).as_str() {
        "apfs" | "hfs" => Numbers::Stable,
        "msdos" | "exfat" => Numbers::ByCreationTime,
        _ => Numbers::Unknown,
    }
}

/// A number for the volume that stays the same when it is mounted again,
/// unlike `st_dev`: from its UUID, or if it has none, from `f_fsid`.
fn volume_number(volume: &libc::statfs) -> u64 {
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        uuid: [u8; 16],
    }
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = MaybeUninit::<Reply>::uninit();
    // The attributes of a volume are asked of its root.
    // SAFETY: `f_mntonname` is a NUL-terminated buffer of the structure.
    let root = unsafe { CStr::from_ptr(volume.f_mntonname.as_ptr().cast::<c_char>()) };
    // SAFETY: the call fills at most `size_of::<Reply>()` bytes of `reply`,
    // and only reads the others; all outlive it.
    let status = unsafe {
        libc::getattrlist(
            root.as_ptr(),
            (&raw mut request).cast(),
            reply.as_mut_ptr().cast(),
            size_of::<Reply>(),
            0,
        )
    };
    if status == 0 {
        // SAFETY: filled by the call; packed, so read as a copy.
        let uuid = unsafe { reply.assume_init() }.uuid;
        if uuid != [0; 16] {
            let (high, low) = uuid.split_at(8);
            let half = |bytes: &[u8]| u64::from_le_bytes(bytes.try_into().unwrap_or_default());
            return half(high) ^ half(low);
        }
    }
    // SAFETY: two 32-bit numbers, which is all that `fsid_t` is.
    let [low, high]: [u32; 2] = unsafe { std::mem::transmute(volume.f_fsid) };
    u64::from(high) << 32 | u64::from(low)
}

fn identity(meta: &Metadata, volume: &libc::statfs) -> FileId {
    let numbers = numbers_of(volume);
    let mut file = [0; 16];
    if numbers == Numbers::Stable {
        file[..8].copy_from_slice(&meta.ino().to_le_bytes());
    }
    FileId {
        volume: volume_number(volume),
        file,
        created: (numbers == Numbers::ByCreationTime)
            .then(|| meta.created().ok().and_then(system_to_filetime))
            .flatten(),
    }
}

/// The identity of an open file or folder.
pub(super) fn file_id_of(item: &File) -> io::Result<FileId> {
    let mut volume = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is open during the call, which fills the local
    // structure.
    if unsafe { libc::fstatfs(item.as_raw_fd(), volume.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: filled by the call.
    Ok(identity(&item.metadata()?, &unsafe {
        volume.assume_init()
    }))
}

/// The [`FileId`] of what is at `path` (a link itself, not what it points to).
/// Looking needs no rights to the item, only to the folders on the way.
pub fn file_id(path: &Path) -> io::Result<FileId> {
    let meta = path.symlink_metadata()?;
    // `statfs` follows a link, which may lead to another volume.
    let on = if meta.is_symlink() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    Ok(identity(&meta, &statfs_of(on)?))
}

/// Whether `error`, from looking at an item, leaves open whether the item is
/// still there. It is only gone when the system says so: without the right to
/// look, which the user can give at any time (Privacy & Security), or on a
/// volume that is not there for now, it may be just where it was.
pub fn cannot_tell(error: &io::Error) -> bool {
    !matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// The identity of the new file or folder at `path`. Unlike on Windows, a new
/// item never gets the creation time of an item deleted before it.
pub fn created_now(path: &Path) -> io::Result<FileId> {
    file_id(path)
}

/// Nothing to do here: a rename leaves the creation time alone.
pub fn keep_creation_time(_file: &File, _id: &FileId) -> io::Result<()> {
    Ok(())
}

/// The file system of the volume that holds `path`, an existing file or
/// folder: "APFS", "HFS+", "FAT32", "exFAT", … Tests pretend "FAT32" with
/// `PRETEND_FAT`.
pub fn file_system(path: &Path) -> Option<String> {
    #[cfg(test)]
    if PRETEND_FAT.get() {
        return Some("FAT32".into());
    }
    let name = c_text(&statfs_of(path).ok()?.f_fstypename);
    Some(match name.as_str() {
        "apfs" => "APFS".into(),
        "hfs" => "HFS+".into(),
        // The file system of FAT32 drives (and older FATs).
        "msdos" => "FAT32".into(),
        "exfat" => "exFAT".into(),
        _ => name,
    })
}

/// Renames `from` to `to` on the same volume, failing with `AlreadyExists`
/// where `std::fs::rename` would replace what is there. A rename that is
/// blocked for a moment (on a network volume) is tried again for a few
/// seconds; one that is denied is not, as that does not pass by itself.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    retried(
        RENAME_RETRY_MS,
        |e| e.raw_os_error() == Some(libc::EBUSY),
        || rename_exclusive(from, to),
    )
}

fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    let (c_from, c_to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are C strings that outlive the call.
    let status =
        unsafe { libc::renamex_np(c_from.as_ptr(), c_to.as_ptr(), libc::RENAME_EXCL as _) };
    if status == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::ENOTSUP) {
        return Err(error);
    }
    // The volume cannot do that in one step: the name is taken with something
    // that the rename then replaces. A crash in between leaves that behind,
    // empty, under a name the record never takes for the item (`Owned`).
    if from.is_dir() {
        std::fs::create_dir(to)?;
    } else {
        OpenOptions::new().write(true).create_new(true).open(to)?;
    }
    std::fs::rename(from, to).inspect_err(|_| {
        let _ = if from.is_dir() {
            std::fs::remove_dir(to)
        } else {
            std::fs::remove_file(to)
        };
    })
}

/// Where the open file `file` is now.
fn open_path(file: &File) -> io::Result<PathBuf> {
    if file.metadata()?.nlink() == 0 {
        return Err(io::ErrorKind::NotFound.into());
    }
    let mut path = vec![0 as c_char; libc::PATH_MAX as usize];
    // SAFETY: the descriptor is open, and the call writes at most `PATH_MAX`
    // bytes, a NUL included, to the buffer of that length.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(c_text(&path)))
}

/// Renames the open file `file` to `to` on the same volume, never replacing
/// anything, as [`rename_new`] does. POSIX renames by name only, so another
/// file may slip in under its name just then: afterwards this checks what was
/// renamed, and if that was not `file`, undoes it and fails.
pub fn rename_open(file: &File, to: &Path) -> io::Result<()> {
    let from = open_path(file)?;
    rename_new(&from, to)?;
    if FileId::of(file)?.same_item(&file_id(to)?) {
        return Ok(());
    }
    let _ = rename_new(to, &from);
    Err(io::Error::other(
        "the file was replaced while it was renamed",
    ))
}

/// Not possible here, and not needed: what has the part open cannot keep it
/// from being deleted by its name (`TransferRecord::delete_files`).
pub fn delete_on_close(_file: &File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Whether deleting the file at `path` failed with `error` only for now (see
/// [`is_busy`]). Whoever has a file open cannot keep it here, so this does not
/// come up as it does on Windows.
pub fn delete_held(error: &io::Error, _path: &Path) -> bool {
    is_busy(error)
}

/// Never: deleting a file that is open frees its name at once.
pub fn being_deleted(_path: &Path) -> bool {
    false
}

/// Whether the file is out of reach for now, in a way that passes without
/// anything here changing: a network volume calls it busy, or the user has not
/// (yet) given the right to it in Privacy & Security.
pub fn is_busy(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EBUSY | libc::ETXTBSY | libc::EPERM | libc::EACCES)
    )
}

/// How a connection opens the part file of a receive: to append to it.
pub fn connection_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.append(true).custom_flags(SAFE_OPEN);
    options
}

/// How the last step of a receive opens the part file: to read it.
pub fn last_step_options(_renames: bool) -> OpenOptions {
    look_options()
}

/// How to open a file only to tell its length and identity.
pub fn length_options() -> OpenOptions {
    look_options()
}

pub fn temp_dir() -> io::Result<PathBuf> {
    let dir = crate::platform::temp_base();
    std::fs::create_dir_all(&dir)?;
    crate::platform::exclude_from_backups(&dir);
    Ok(dir)
}

/// Where a profile keeps the temporary archives of its transfers: the folder
/// of [`temp_dir`], or a sub folder per `--profile`. Each profile cleans up
/// leftovers in its own folder only, so that it never deletes the archive of a
/// transfer that another profile is still running. The folder is created when
/// it is used.
pub fn temp_dir_for(profile: Option<&str>) -> PathBuf {
    if let Ok(base) = temp_dir() {
        return match profile {
            Some(p) => base.join("profiles").join(p),
            None => base,
        };
    }
    crate::platform::temp_base()
}
