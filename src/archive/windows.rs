//! Windows: dates, attributes, the identity of files and renames that replace
//! nothing, through Win32.

use std::{
    fs::{File, FileTimes, Metadata, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::{FileTimesExt, MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    time::SystemTime,
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, FILETIME,
        GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, MAX_PATH, SYSTEMTIME,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_BASIC_INFO, FILE_DISPOSITION_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES,
        FILE_RENAME_INFO, FILE_SHARE_READ, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FileBasicInfo,
        FileDispositionInfo, FileIdInfo, FileRenameInfo, FindClose, FindFirstFileW,
        GetFileInformationByHandle, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
        MoveFileExW, SetFileInformationByHandle, WIN32_FIND_DATAW,
    },
    System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
};

#[cfg(test)]
use super::PRETEND_FAT;
use super::{
    FAT_CREATION_STEP, FILE_ATTRIBUTE_HIDDEN, FileId, ItemTimes, RENAME_RETRY_MS,
    filetime_to_system, retried,
};

pub(super) fn item_times(meta: &Metadata) -> ItemTimes {
    let known = |t: u64| (t != 0).then_some(t);
    ItemTimes {
        modified: known(meta.last_write_time()),
        accessed: known(meta.last_access_time()),
        created: known(meta.creation_time()),
    }
}

/// Sets the modified and created dates of a file or folder.
pub fn set_times(
    path: &Path,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
) -> io::Result<()> {
    let Some(times) = super::file_times(modified, created) else {
        return Ok(());
    };
    // Writing the attributes is all that is needed; backup semantics let
    // the same call open a folder.
    OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .set_times(times)
}

/// Marks a received file as coming from another computer (the Internet
/// zone), as browsers mark downloads, so that SmartScreen and Office's
/// Protected View treat it the same way. The mark is the `Zone.Identifier`
/// stream of the file; file systems without streams (FAT32, exFAT) cannot
/// keep it, which is fine. Writing the stream changes the file's modified
/// date, so dates are set after the mark.
pub fn mark_of_the_web(path: &Path) -> io::Result<()> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n")
}

/// The local time of `filetime`, with the daylight saving rules of that date:
/// year, month, day, hour, minute and second.
pub(super) fn local_time(filetime: u64) -> Option<(u16, u8, u8, u8, u8, u8)> {
    let filetime = FILETIME {
        dwLowDateTime: filetime as u32,
        dwHighDateTime: (filetime >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: both calls only read and write the local structures passed to
    // them; a null time zone means the current one.
    let ok = unsafe {
        FileTimeToSystemTime(&filetime, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
    };
    ok.then_some((
        local.wYear,
        local.wMonth as u8,
        local.wDay as u8,
        local.wHour as u8,
        local.wMinute as u8,
        local.wSecond as u8,
    ))
}

/// Whether the file or folder is hidden.
pub(super) fn is_hidden(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

/// Opens the file of a zip entry for [`unzip`](super::unzip).
pub(super) fn open_unpacked(target: PathBuf) -> io::Result<(File, PathBuf)> {
    let name = target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let parent = target.parent().unwrap_or(Path::new("")).to_path_buf();
    // Always a new file: a zip can hold two entries of one name, or of names
    // that differ only in case, and the later one gets a number.
    for candidate in super::numbered(&name, false) {
        let path = parent.join(candidate);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            // Also reading the attributes: a hidden file is hidden through
            // this handle (see `hide_open`).
            .access_mode(GENERIC_WRITE | FILE_READ_ATTRIBUTES)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("infinite iterator")
}

/// Whether a file can be run: Windows goes by its extension, which travels.
pub(super) fn is_executable(_meta: &Metadata) -> bool {
    false
}

/// See [`is_executable`].
pub(super) fn keep_executable(_file: &File, _attributes: u32) {}

/// Whether a folder of this name is a package to the system: none on Windows.
pub(super) fn is_package(_name: &str) -> bool {
    false
}

/// Where [`unzip`](super::unzip) puts entries: on Windows, under their names
/// in the zip, without links (see [`super::names`]).
#[derive(Default)]
pub(super) struct UnpackedNames;

impl UnpackedNames {
    pub fn place(
        &mut self,
        rel: PathBuf,
        _is_folder: bool,
        _fidelity: super::names::Fidelity,
    ) -> (PathBuf, Vec<(String, String)>) {
        (rel, Vec::new())
    }

    pub fn note_links(&mut self, _folder: &Path, _links: Vec<(String, String)>) {}

    pub fn make_links(&mut self, _dest: &Path) {}
}

/// Makes the file or folder at `path` hidden (see [`hide_open`]).
pub(super) fn hide(path: &Path) -> io::Result<()> {
    let item = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    hide_open(&item)
}

/// Makes the open file or folder `item` hidden, keeping its other
/// attributes and its dates (the zero dates of the change leave them as
/// they are). Its handle needs to read and write the attributes.
pub(super) fn hide_open(item: &File) -> io::Result<()> {
    let info = FILE_BASIC_INFO {
        FileAttributes: item.metadata()?.file_attributes() | FILE_ATTRIBUTE_HIDDEN,
        ..FILE_BASIC_INFO::default()
    };
    set_file_info(
        item,
        FileBasicInfo,
        std::slice::from_ref(&info),
        size_of::<FILE_BASIC_INFO>(),
    )
}

/// A folder gets no mark on Windows: the mark on each file is what counts.
pub(super) fn mark_folder(_path: &Path) {}

/// Whether something has the name `path`. Also true for a file that was
/// deleted while another program still had it open: it keeps its name until
/// it is closed, but cannot be opened, and creating something under that
/// name fails with "access denied" instead of "already exists".
pub fn name_taken(path: &Path) -> bool {
    path.symlink_metadata().is_ok() || listed(path)
}

/// Whether the folder lists `path`, which it still does for a deleted file
/// that is open somewhere (opening it fails then, at least on Windows
/// versions whose metadata calls open the file).
pub(super) fn listed(path: &Path) -> bool {
    // FindFirstFileW takes the name as a pattern, in which `*` and `?` are
    // wildcards, and so are `<`, `>` and `"` (the DOS wildcards). A name with
    // one of them would match other names; Windows allows none of them in a
    // name anyway (see `sanitize_file_name`), so nothing is listed under it.
    let pattern = path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().contains(['*', '?', '<', '>', '"']));
    if pattern {
        return false;
    }
    let Ok(wide) = wide_path(path) else {
        return false;
    };
    let mut found = WIN32_FIND_DATAW::default();
    // SAFETY: `wide` is a NUL-terminated wide string and `found` a local
    // structure that the call fills; both outlive it.
    let search = unsafe { FindFirstFileW(wide.as_ptr(), &mut found) };
    if search == INVALID_HANDLE_VALUE {
        return false;
    }
    // SAFETY: a search handle that FindFirstFileW returned, closed once.
    unsafe { FindClose(search) };
    true
}

/// Whether creating something at `path` failed because the name is taken.
pub(super) fn is_taken(error: &io::Error, path: &Path) -> bool {
    error.kind() == io::ErrorKind::AlreadyExists
        || (error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) && name_taken(path))
}

/// The identity of an open file or folder.
pub(super) fn file_id_of(item: &File) -> io::Result<FileId> {
    let handle = item.as_raw_handle();
    let fat = on_fat(handle);
    let created = if fat {
        // 0 where the file system keeps no creation time.
        Some(item.metadata()?.creation_time()).filter(|&t| t != 0)
    } else {
        None
    };
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the handle stays open during the call, which only fills
    // the local structure of the given size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0;
    if ok {
        return Ok(FileId {
            volume: info.VolumeSerialNumber,
            file: if fat { [0; 16] } else { info.FileId.Identifier },
            created,
        });
    }
    // Windows 7 does not know the 128-bit IDs.
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: as above.
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = [0; 16];
    if !fat {
        let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        file[..8].copy_from_slice(&index.to_le_bytes());
    }
    Ok(FileId {
        volume: u64::from(info.dwVolumeSerialNumber),
        file,
        created,
    })
}

/// Gives the new file or folder at `path` the time of its creation, and
/// returns its identity: Windows may give a new item the creation time of
/// one of the same name that was deleted moments before, which on FAT and
/// exFAT would make it look like that one (see [`FileId`]), so there a time
/// that cannot be set is an error. Elsewhere the item's number tells it, and
/// an item whose dates may not be changed is fine as it is. The time is a
/// whole multiple of the 10 ms that those file systems keep, so that it reads
/// back the same however they round it.
pub fn created_now(path: &Path) -> io::Result<FileId> {
    let opened = OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path);
    let item = match opened {
        Ok(item) => item,
        Err(e) => {
            return match file_id(path) {
                Ok(id) if id.numbered() => Ok(id),
                _ => Err(e),
            };
        }
    };
    let set =
        match filetime_to_system(super::filetime_now() / FAT_CREATION_STEP * FAT_CREATION_STEP) {
            Some(now) => item.set_times(FileTimes::new().set_created(now)),
            None => Err(io::ErrorKind::InvalidData.into()),
        };
    let id = FileId::of(&item)?;
    match set {
        Err(e) if !id.numbered() => Err(e),
        _ => Ok(id),
    }
}

/// Gives the open file `file`, which has just been renamed, its creation
/// time back where the identity `id` that it had before has one (on FAT and
/// exFAT): a rename gives it the time of an item of its new name that was
/// deleted moments before (see [`FileId`]). Fails if that was needed and
/// could not be done. The handle needs the right to write attributes.
pub fn keep_creation_time(file: &File, id: &FileId) -> io::Result<()> {
    let Some(created) = id.created else {
        return Ok(());
    };
    if id.numbered() || id.same_item(&FileId::of(file)?) {
        return Ok(());
    }
    let created = filetime_to_system(created).ok_or(io::ErrorKind::InvalidData)?;
    file.set_times(FileTimes::new().set_created(created))
}

/// The [`FileId`] of what is at `path` (a link itself, not what it points to).
pub fn file_id(path: &Path) -> io::Result<FileId> {
    // Reading it needs no access rights; backup semantics open folders too.
    let item = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    FileId::of(&item)
}

/// Whether an open file or folder is on a FAT or exFAT volume (see
/// [`FileId`]).
fn on_fat(handle: HANDLE) -> bool {
    #[cfg(test)]
    if PRETEND_FAT.get() {
        return true;
    }
    // "FAT", "FAT32" or "exFAT".
    file_system_of(handle).is_some_and(|name| name.to_ascii_uppercase().contains("FAT"))
}

/// The file system of the volume that holds `path`, an existing file or
/// folder (such as the root of the volume): "NTFS", "FAT32", "exFAT", …
/// Tests pretend "FAT32" with [`PRETEND_FAT`].
pub fn file_system(path: &Path) -> Option<String> {
    #[cfg(test)]
    if PRETEND_FAT.get() {
        return Some("FAT32".into());
    }
    let item = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    file_system_of(item.as_raw_handle())
}

/// See [`file_system`]: by an open handle.
fn file_system_of(handle: HANDLE) -> Option<String> {
    let mut name = [0u16; MAX_PATH as usize + 1];
    // SAFETY: the handle is open; the call writes at most `name.len()`
    // characters to the local buffer, and the other outputs are not wanted.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    } != 0;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    ok.then(|| String::from_utf16_lossy(&name[..len]))
}

/// Renames `from` to `to` on the same volume, failing with `AlreadyExists`
/// where `std::fs::rename` would replace a file that is there, and also where
/// a deleted file still holds the name (see [`name_taken`]). A rename that
/// another program blocks for a moment (a virus scanner reading the new
/// file, typically) is tried again for a few seconds.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    let (wide_from, wide_to) = (wide_path(from)?, wide_path(to)?);
    renamed(to, || {
        // SAFETY: both are NUL-terminated wide strings that outlive the call;
        // no flags: fail rather than replace, and stay on the volume.
        if unsafe { MoveFileExW(wide_from.as_ptr(), wide_to.as_ptr(), 0) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

/// Renames the open file `file` to `to` on the same volume, never replacing
/// anything, as [`rename_new`] does: what is renamed is the file that is
/// open, whatever has its old name by then. The handle needs delete access.
pub fn rename_open(file: &File, to: &Path) -> io::Result<()> {
    let name = wide_path(to)?;
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = offset + name.len() * size_of::<u16>();
    // Zeroed: replace nothing, no root folder (the name is a full path). In
    // whole words, aligned for the structure.
    let mut info = vec![0usize; size.div_ceil(size_of::<usize>())];
    let start = info.as_mut_ptr().cast::<u8>();
    // SAFETY: the buffer is large enough for the structure with the whole
    // name (and its NUL) after its header, and aligned for it; the name is
    // copied into the buffer, which it does not overlap.
    unsafe {
        (*start.cast::<FILE_RENAME_INFO>()).FileNameLength =
            ((name.len() - 1) * size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), start.add(offset).cast(), name.len());
    }
    renamed(to, || {
        set_file_info(file, FileRenameInfo, info.as_slice(), size)
    })
}

/// Runs `rename`, which renames something to `to` and replaces nothing:
/// fails with `AlreadyExists` where the name is taken (see [`is_taken`]), and
/// tries again for a few seconds while another program blocks it.
fn renamed(to: &Path, rename: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    // Of the last error: whether the name is taken.
    let mut taken = false;
    let blocked = |e: &io::Error| {
        taken = is_taken(e, to);
        !taken && (is_busy(e) || e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32))
    };
    match retried(RENAME_RETRY_MS, blocked, rename) {
        Err(e) if taken => Err(io::Error::new(io::ErrorKind::AlreadyExists, e)),
        done => done,
    }
}

/// Deletes the open file `file` once the last handle to it is closed; until
/// then it keeps its name (see [`name_taken`]), and nothing can open it. The
/// handle needs delete access.
pub fn delete_on_close(file: &File) -> io::Result<()> {
    let info = [FILE_DISPOSITION_INFO { DeleteFile: true }];
    set_file_info(
        file,
        FileDispositionInfo,
        &info,
        size_of::<FILE_DISPOSITION_INFO>(),
    )
}

/// Changes what `class` says of the open file `file`
/// (`SetFileInformationByHandle`) to the first `size` bytes of `info`.
fn set_file_info<T>(
    file: &File,
    class: FILE_INFO_BY_HANDLE_CLASS,
    info: &[T],
    size: usize,
) -> io::Result<()> {
    assert!(size <= size_of_val(info));
    // SAFETY: the handle is open, and the call reads `size` bytes of `info`,
    // which has them and outlives it.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            class,
            info.as_ptr().cast(),
            size as u32,
        )
    } != 0;
    if ok {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether deleting the file at `path` failed with `error` only for now:
/// another program has it open (see [`is_busy`]), it is being deleted
/// already and goes once the last program lets go of it (see
/// [`being_deleted`]), or it may be deleted but a program has it mapped
/// into memory, as a virus scanner or the search indexer may, which Windows
/// also reports as "access denied" (see [`deletable`]). Not where it is
/// read-only, or may not be deleted, which trying again never changes.
pub fn delete_held(error: &io::Error, path: &Path) -> bool {
    is_busy(error)
        || (error.kind() == io::ErrorKind::PermissionDenied
            && (being_deleted(path) || deletable(path)))
}

/// Whether the file at `path` may be deleted and is not read-only, so that
/// a deletion that was denied all the same was denied only for now (see
/// [`delete_held`]).
fn deletable(path: &Path) -> bool {
    OpenOptions::new()
        .access_mode(DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .and_then(|file| file.metadata())
        .is_ok_and(|m| m.is_file() && !m.permissions().readonly())
}

/// Whether the file at `path` is being deleted: it was deleted while a
/// program still has it open, so it keeps its name but cannot be opened
/// (see [`name_taken`]).
pub fn being_deleted(path: &Path) -> bool {
    let opened = OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path);
    opened.is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied) && listed(path)
}

/// Whether another program has the file open in a way that keeps this one
/// out for now: a sharing or lock violation.
pub fn is_busy(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error().map(|c| c as u32),
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    )
}

/// A path for a Win32 call: absolute, NUL-terminated, and with the `\\?\`
/// prefix when it is too long for the plain form (as `std::fs` does).
pub(super) fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    const VERBATIM: &str = r"\\?\";
    let full = std::path::absolute(path)?;
    let text: Vec<u16> = full.as_os_str().encode_wide().collect();
    let verbatim: Vec<u16> = VERBATIM.encode_utf16().collect();
    let mut wide = Vec::with_capacity(text.len() + 9);
    // CreateDirectory's limit, a little below MAX_PATH, to be safe.
    if text.len() >= 248 && !text.starts_with(&verbatim) {
        if text.starts_with(&verbatim[..2]) {
            // \\server\share\… → \\?\UNC\server\share\…
            wide.extend(r"\\?\UNC".encode_utf16());
            wide.extend_from_slice(&text[1..]);
        } else {
            wide.extend_from_slice(&verbatim);
            wide.extend_from_slice(&text);
        }
    } else {
        wide.extend_from_slice(&text);
    }
    wide.push(0);
    Ok(wide)
}

pub fn temp_dir() -> io::Result<PathBuf> {
    let dir = crate::platform::temp_base();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Where a profile keeps the temporary archives of its transfers: the
/// folder of [`temp_dir`], or a sub folder per `--profile`. Each profile
/// cleans up leftovers in its own folder only, so that it never deletes the
/// archive of a transfer that another profile is still running. The folder is
/// created when it is used (temp cleaners may remove it meanwhile).
pub fn temp_dir_for(profile: Option<&str>) -> PathBuf {
    let base = crate::platform::temp_base();
    match profile {
        Some(p) => base.join("profiles").join(p),
        None => base,
    }
}

/// Whether `error`, from looking at an item, leaves open whether the item is
/// still there: not on Windows, where looking at an item needs no rights.
pub fn cannot_tell(_error: &io::Error) -> bool {
    false
}

/// How a connection opens the part file of a receive: to append to it, as
/// `append` would, and to delete it through a second handle at a cancel (see
/// `Session::keep_part`); other programs can only read it meanwhile.
pub fn connection_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .access_mode((FILE_GENERIC_WRITE & !FILE_WRITE_DATA) | DELETE)
        .share_mode(FILE_SHARE_READ);
    options
}

/// How the last step of a receive opens the part file: others can only read
/// it, and with `renames` it can be renamed through the handle.
pub fn last_step_options(renames: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .access_mode(if renames {
            FILE_GENERIC_READ | FILE_WRITE_ATTRIBUTES | DELETE
        } else {
            FILE_GENERIC_READ
        })
        .share_mode(FILE_SHARE_READ);
    options
}

/// How to open a file only to tell its length and identity: that needs no
/// access rights.
pub fn length_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.access_mode(0);
    options
}
