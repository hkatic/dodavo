//! POSIX: what macOS and Linux (Android among them) share of dates, names,
//! renames that replace nothing, and how files are opened. What only one of
//! them can do is in its own module (`super::os`).
//!
//! POSIX lets no one keep a file to themselves. Where Windows stops other
//! programs with a share mode, here they can write to the part of a receive,
//! rename it or delete it while it is open, and deleting never waits for
//! anyone. So `receive` checks the part on disk where Windows relies on the
//! share mode, and a part that was deleted or moved meanwhile is noticed by
//! its identity (`receive::Receiver::pump`).

use std::{
    ffi::CString,
    fs::{File, Metadata, OpenOptions},
    io,
    mem::MaybeUninit,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt as _, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    time::SystemTime,
};

use super::{
    FileId, ItemTimes, RENAME_RETRY_MS, file_times, filetime_to_system,
    names::{Fidelity, bytes, cut_to, link_stays_inside},
    numbered,
    os::{file_id, open_path, rename_exclusive, takes_name},
    retried, system_to_filetime,
};

/// Nothing is opened through a link at the end of the path, and a named pipe
/// that has taken a name does not block the opening.
const SAFE_OPEN: i32 = libc::O_NOFOLLOW | libc::O_NONBLOCK;

pub(super) fn c_path(path: &Path) -> io::Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

/// How to open a file or folder only to look at it or to change its dates.
pub(super) fn look_options() -> OpenOptions {
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

/// Opens the file of a zip entry for [`unzip`](super::unzip): only a new one,
/// under the next free name if it is taken, so that entries whose names are
/// the same here keep their data apart. Composed and decomposed forms of a
/// name are the same on APFS, and on FAT and exFAT the attributes of a file
/// go into a file beside it (`._name`), which an entry may be called as well.
pub(super) fn open_unpacked(target: PathBuf) -> io::Result<(File, PathBuf)> {
    let name = target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let parent = target.parent().unwrap_or(Path::new("")).to_path_buf();
    for candidate in numbered(&name, false) {
        let path = parent.join(candidate);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("infinite iterator")
}

/// Whether a file can be run, by whoever may read it.
pub(super) fn is_executable(meta: &Metadata) -> bool {
    meta.mode() & 0o111 != 0
}

/// Lets those who may read the new file `file` run it, if the Unix mode in
/// the zip entry's attributes says it can be. Nothing else of the mode comes
/// along: a sender's read-only folder would hold up deleting a folder that was
/// unpacked in part, and its other bits are not the receiver's business.
pub(super) fn keep_executable(file: &File, attributes: u32) {
    let mode = attributes >> 16;
    if mode & 0o170000 != 0o100000 || mode & 0o111 == 0 {
        return;
    }
    let Ok(meta) = file.metadata() else { return };
    let readable = meta.mode() & 0o777;
    let runnable = readable | ((readable & 0o444) >> 2);
    // SAFETY: the descriptor is open during the call.
    unsafe { libc::fchmod(file.as_raw_fd(), runnable as libc::mode_t) };
}

/// Where [`unzip`](super::unzip) puts entries: under their own names where
/// the zip says so (see [`super::names`]) and macOS can take them.
#[derive(Default)]
pub(super) struct UnpackedNames {
    /// Each folder's path by its names in the zip, with its own names.
    folders: std::collections::HashMap<PathBuf, PathBuf>,
    /// The links to make: in which folder, name and target.
    links: Vec<(PathBuf, String, String)>,
}

impl UnpackedNames {
    /// The path of an entry whose path by its names in the zip is `rel`, and
    /// the links it brings along.
    pub fn place(
        &mut self,
        rel: PathBuf,
        is_folder: bool,
        fidelity: Fidelity,
    ) -> (PathBuf, Vec<(String, String)>) {
        let name = fidelity.name.filter(|name| takes_name(name)).or_else(|| {
            rel.file_name()
                .map(|name| cut_to(&name.to_string_lossy(), 240, bytes))
        });
        let parent = rel.parent().unwrap_or(Path::new(""));
        let mut own = self
            .folders
            .get(parent)
            .cloned()
            .unwrap_or_else(|| parent.to_path_buf());
        own.push(name.unwrap_or_default());
        if is_folder {
            self.folders.insert(rel, own.clone());
        }
        (own, fidelity.links)
    }

    /// Notes the links of the folder at the path `folder` in the new folder.
    pub fn note_links(&mut self, folder: &Path, links: Vec<(String, String)>) {
        let levels = folder.components().count();
        for (name, target) in links {
            if takes_name(&name) && link_stays_inside(levels, &target) {
                self.links.push((folder.to_path_buf(), name, target));
            }
        }
    }

    /// Makes the noted links in the new folder `dest`. A name that is taken
    /// stays with what has it.
    pub fn make_links(&mut self, dest: &Path) {
        for (folder, name, target) in self.links.drain(..) {
            let _ = std::os::unix::fs::symlink(target, dest.join(folder).join(name));
        }
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

/// Renames `from` to `to`, for a volume that cannot refuse to replace in one
/// step: the name is taken with something that the rename then replaces. A
/// crash in between leaves that behind, empty, under a name the record never
/// takes for the item (`Owned`).
pub(super) fn rename_with_placeholder(from: &Path, to: &Path) -> io::Result<()> {
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
