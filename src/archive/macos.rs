//! macOS: the mark of the web, hidden items, the identity of files, and the
//! calls that only macOS has. The rest is plain POSIX (`super::unix`).

use std::{
    ffi::{CStr, c_char},
    fs::{File, Metadata},
    io,
    mem::MaybeUninit,
    os::{fd::AsRawFd, macos::fs::MetadataExt as _, unix::fs::MetadataExt as _},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[cfg(test)]
use super::PRETEND_FAT;
pub use super::unix::*;
use super::{FileId, names::is_mac_name, system_to_filetime};
use crate::platform::{c_text, statfs_of};

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

/// Whether the file or folder is hidden.
pub(super) fn is_hidden(meta: &Metadata) -> bool {
    meta.st_flags() & libc::UF_HIDDEN != 0
}

/// Whether a folder of this name is a document or an app to the Finder: its
/// extension has to stay at the end of the name, or it no longer is.
pub(super) fn is_package(name: &str) -> bool {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    [
        "app",
        "appex",
        "bundle",
        "framework",
        "fcpbundle",
        "imovielibrary",
        "kext",
        "key",
        "logicx",
        "mpkg",
        "musiclibrary",
        "numbers",
        "pages",
        "photoslibrary",
        "pkg",
        "plugin",
        "prefPane",
        "qlgenerator",
        "rtfd",
        "saver",
        "tvlibrary",
        "xcodeproj",
        "xcworkspace",
    ]
    .iter()
    .any(|known| known.eq_ignore_ascii_case(extension))
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

/// Renames `from` to `to`, failing with `AlreadyExists` where something has
/// the name.
pub(super) fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
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
    rename_with_placeholder(from, to)
}

/// Where the open file `file` is now.
pub(super) fn open_path(file: &File) -> io::Result<PathBuf> {
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

/// Whether `name`, from a peer, can be a single name here as it is.
pub(super) fn takes_name(name: &str) -> bool {
    is_mac_name(name)
}

/// Whether writing to the medium failed with `error` because the file system
/// cannot do that (`F_FULLFSYNC`, which SMB refuses, say).
pub fn full_sync_refused(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EINVAL | libc::ENOTTY)
    )
}

/// Hands what was written to the file system, without asking it to go on to
/// the medium (see [`full_sync_refused`]).
pub fn sync_as_far_as_possible(file: &File) -> io::Result<()> {
    // SAFETY: the descriptor is open during the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
