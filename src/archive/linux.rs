//! Linux, and Android on top of it: the identity of files, and the calls
//! that only Linux has. The rest is plain POSIX (`super::unix`).
//!
//! There is no mark of the web that file managers heed, no hidden attribute
//! (a name that starts with a dot is hidden), and no creation time that can be
//! set. On Android, Dodavo writes received items to the shared Download
//! folder, which the system serves through FUSE: there Dodavo sees only its
//! own files, inode numbers are those of the file system below, and no birth
//! time is known (see [`FileId::weak`]).

use std::{
    ffi::CString,
    fs::{File, Metadata},
    io,
    mem::MaybeUninit,
    os::{fd::AsRawFd, unix::ffi::OsStrExt, unix::fs::MetadataExt as _},
    path::{Path, PathBuf},
};

#[cfg(test)]
use super::PRETEND_FAT;
pub use super::unix::*;
use super::{FileId, names::is_mac_name, system_to_filetime};
use crate::platform::statfs_of;

const EXT4: i64 = 0xEF53;
const F2FS: i64 = 0xF2F5_2010;
const FUSE: i64 = 0x6573_5546;
const SDCARDFS: i64 = 0x5DCA_2DF5;
const MSDOS: i64 = 0x4D44;
const EXFAT: i64 = 0x2011_BAB0;
const BTRFS: i64 = 0x9123_683E;
const XFS: i64 = 0x5846_5342;
const TMPFS: i64 = 0x0102_1994;

/// Nothing to mark: no file manager here looks for where a file came from.
pub fn mark_of_the_web(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn mark_folder(_path: &Path) {}

/// Only a name that starts with a dot hides an item here, and that the name
/// says by itself (see `collect`).
pub(super) fn is_hidden(_meta: &Metadata) -> bool {
    false
}

/// No folder is a document or an app here.
pub(super) fn is_package(_name: &str) -> bool {
    false
}

/// Nothing to do: an item is hidden by its name (see [`is_hidden`]).
pub(super) fn hide(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn hide_open(_item: &File) -> io::Result<()> {
    Ok(())
}

/// The type of the volume's file system, as `statfs` gives it.
fn kind_of(volume: &libc::statfs) -> i64 {
    #[cfg(test)]
    if PRETEND_FAT.get() {
        return MSDOS;
    }
    volume.f_type as i64
}

/// A number for the volume: from `f_fsid`, which ext4 and f2fs derive from
/// the volume's UUID, or where that is zero (FUSE), from the type of its file
/// system.
fn volume_number(volume: &libc::statfs) -> u64 {
    // SAFETY: two 32-bit numbers, which is all that `fsid_t` is.
    let [low, high]: [i32; 2] = unsafe { std::mem::transmute(volume.f_fsid) };
    let fsid = u64::from(high as u32) << 32 | u64::from(low as u32);
    if fsid != 0 {
        fsid
    } else {
        kind_of(volume) as u64
    }
}

/// When the item at `path` (not what a link leads to) was born, in
/// nanoseconds since 1970, if the file system keeps that and says so.
pub(super) fn birth_time(path: &Path) -> Option<u64> {
    // Android's filter of system calls ends an app that asks for this before
    // Android 11.
    #[cfg(target_os = "android")]
    if crate::platform::api_level() < 30 {
        return None;
    }
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut info = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: a C string and a structure to fill, which outlive the call.
    // Through `syscall`, as the C library of Android before 11 has no
    // `statx` to link to.
    let status = unsafe {
        libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW | libc::AT_STATX_SYNC_AS_STAT,
            libc::STATX_BTIME,
            info.as_mut_ptr(),
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: filled by the call (and zeroed before it).
    let info = unsafe { info.assume_init() };
    if info.stx_mask & libc::STATX_BTIME == 0 {
        return None;
    }
    let born = u64::try_from(info.stx_btime.tv_sec).ok()?;
    born.checked_mul(1_000_000_000)?
        .checked_add(u64::from(info.stx_btime.tv_nsec))
        .filter(|&born| born != 0)
}

/// The identity of the item at `path` with `meta` on `volume`: the volume,
/// the inode number, and the birth time where the file system says it. FAT
/// and exFAT have no inode numbers of their own, so there it is the
/// creation time (see [`FileId`]).
fn identity(path: &Path, meta: &Metadata, volume: &libc::statfs) -> FileId {
    let kind = kind_of(volume);
    let fat = matches!(kind, MSDOS | EXFAT);
    let mut file = [0; 16];
    if !fat {
        file[..8].copy_from_slice(&meta.ino().to_le_bytes());
        if let Some(born) = birth_time(path) {
            file[8..].copy_from_slice(&born.to_le_bytes());
        }
    }
    FileId {
        volume: volume_number(volume),
        file,
        created: fat
            .then(|| meta.created().ok().and_then(system_to_filetime))
            .flatten(),
    }
}

/// The identity of an open file or folder, by where it is now: Linux tells
/// the birth time only by path.
pub(super) fn file_id_of(item: &File) -> io::Result<FileId> {
    let mut volume = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is open during the call, which fills the local
    // structure.
    if unsafe { libc::fstatfs(item.as_raw_fd(), volume.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: filled by the call.
    let volume = unsafe { volume.assume_init() };
    let meta = item.metadata()?;
    let path = open_path(item)?;
    // Only if the path still leads to this item: otherwise without the time.
    let same = path
        .symlink_metadata()
        .is_ok_and(|now| now.dev() == meta.dev() && now.ino() == meta.ino());
    let mut id = identity(&path, &meta, &volume);
    if !same {
        id.file[8..].fill(0);
    }
    Ok(id)
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
    Ok(identity(path, &meta, &statfs_of(on)?))
}

/// The file system of the volume that holds `path`, an existing file or
/// folder: "ext4", "F2FS", "FAT32", "exFAT", … Tests pretend "FAT32" with
/// `PRETEND_FAT`.
pub fn file_system(path: &Path) -> Option<String> {
    let volume = statfs_of(path).ok()?;
    Some(
        match kind_of(&volume) {
            EXT4 => "ext4",
            F2FS => "F2FS",
            // Android's shared storage, which keeps the files on the volume
            // of the app data below it.
            FUSE | SDCARDFS => "FUSE",
            MSDOS => "FAT32",
            EXFAT => "exFAT",
            BTRFS => "Btrfs",
            XFS => "XFS",
            TMPFS => "tmpfs",
            other => return Some(format!("{other:#x}")),
        }
        .into(),
    )
}

/// Renames `from` to `to`, failing with `AlreadyExists` where something has
/// the name. Android's shared storage refuses to do that in one step on some
/// versions of the system (`EINVAL`), and its filter of system calls does not
/// let an app ask before Android 11: then the name is taken with a
/// placeholder first.
pub(super) fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "android")]
    if crate::platform::api_level() < 30 {
        return rename_with_placeholder(from, to);
    }
    let (c_from, c_to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are C strings that outlive the call. Through `syscall`,
    // as the C library of Android before 11 has no `renameat2`.
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            c_from.as_ptr(),
            libc::AT_FDCWD,
            c_to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::ENOTSUP) => rename_with_placeholder(from, to),
        _ => Err(error),
    }
}

/// Where the open file `file` is now.
pub(super) fn open_path(file: &File) -> io::Result<PathBuf> {
    if file.metadata()?.nlink() == 0 {
        return Err(io::ErrorKind::NotFound.into());
    }
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

/// Whether `name`, from a peer, can be a single name here as it is. Android's
/// shared storage refuses names that FAT could not take, so there only those
/// that any system takes are used.
pub(super) fn takes_name(name: &str) -> bool {
    #[cfg(target_os = "android")]
    return is_mac_name(name) && super::is_plain_name(name);
    #[cfg(not(target_os = "android"))]
    is_mac_name(name)
}
