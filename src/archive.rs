//! Folder packing/unpacking, file dates, the mark of the web and file name
//! helpers.

use std::{
    fs::{File, FileTimes, Metadata, OpenOptions},
    io::{self, BufWriter, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{FileTimesExt, MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, FILETIME,
        GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, MAX_PATH, SYSTEMTIME,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
        FILE_BASIC_INFO, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS,
        FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_WRITE_ATTRIBUTES, FileBasicInfo,
        FileDispositionInfo, FileIdInfo, FileRenameInfo, FindClose, FindFirstFileW,
        GetFileInformationByHandle, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
        MoveFileExW, SetFileInformationByHandle, WIN32_FIND_DATAW,
    },
    System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
};
use zip::{
    CompressionMethod, DateTime, ExtraField, System, ZipArchive, ZipWriter,
    write::{FullFileOptions, SimpleFileOptions},
};

use crate::i18n::tr;

const BUF_SIZE: usize = 256 * 1024;

/// The zip extra field with the exact Windows file times of an entry
/// (PKWARE APPNOTE 4.5.5): modified, accessed and created, in 100 ns units
/// since 1601 (UTC). The standard MS-DOS date of an entry is local time in
/// 2-second steps; 7-Zip and others write this field too.
const NTFS_EXTRA_FIELD: u16 = 0x000a;
/// 100 ns intervals from 1601-01-01 to 1970-01-01.
pub(crate) const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
/// The pauses between attempts of a rename that another program blocks
/// (see [`rename_new`]), or of opening a file that another program has open
/// for now (see [`while_busy`]): about nine seconds in all.
pub const RENAME_RETRY_MS: &[u64] = &[100, 200, 400, 800, 1600, 2000, 2000, 2000];

/// The error of work that stopped because the user cancelled it: packing or
/// unpacking a folder, or the last step of a receive. Telling it apart from
/// a failure matters when a cancel came too late and something then failed
/// for real; check for it with [`is_cancelled`].
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `error` is a [`Cancelled`], also inside an I/O error (whose
/// `source` skips the error it carries).
pub fn is_cancelled(error: &anyhow::Error) -> bool {
    error.chain().any(|e| {
        e.is::<Cancelled>()
            || e.downcast_ref::<io::Error>()
                .and_then(io::Error::get_ref)
                .is_some_and(|inner| inner.is::<Cancelled>())
    })
}

/// The dates of a file or folder as Windows file times (100 ns since 1601,
/// UTC). `None` when not known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemTimes {
    pub modified: Option<u64>,
    pub accessed: Option<u64>,
    pub created: Option<u64>,
}

impl ItemTimes {
    pub fn of(meta: &Metadata) -> Self {
        let known = |t: u64| (t != 0).then_some(t);
        ItemTimes {
            modified: known(meta.last_write_time()),
            accessed: known(meta.last_access_time()),
            created: known(meta.creation_time()),
        }
    }

    /// Gives `path` the modified and created dates. Best effort: a file
    /// system without them, or a missing permission, never fails a transfer.
    /// The date accessed is left alone; it changes by itself anyway.
    pub fn apply(&self, path: &Path) {
        let _ = set_times(
            path,
            self.modified.and_then(filetime_to_system),
            self.created.and_then(filetime_to_system),
        );
    }

    /// The dates that can be given to an item of a transfer at once, and
    /// those that wait until its record is gone: where the identity `id`
    /// that the record notes for it tells it by its creation time (on FAT
    /// and exFAT, see [`FileId`]), the sender's creation time would make it
    /// look like another item until then, so that one waits.
    pub fn split(self, id: Option<&FileId>) -> (ItemTimes, ItemTimes) {
        if id.is_some_and(FileId::by_creation_time) {
            let later = ItemTimes {
                created: self.created,
                ..ItemTimes::default()
            };
            let now = ItemTimes {
                created: None,
                ..self
            };
            (now, later)
        } else {
            (self, ItemTimes::default())
        }
    }

    /// Like [`apply`](Self::apply), on a file that is open for writing and
    /// whose data is all written: no need to open it once more.
    pub fn apply_to(&self, file: &File) {
        if let Some(times) = file_times(
            self.modified.and_then(filetime_to_system),
            self.created.and_then(filetime_to_system),
        ) {
            let _ = file.set_times(times);
        }
    }

    /// The payload of the NTFS extra field.
    fn ntfs_extra(&self) -> [u8; 32] {
        let mut field = [0u8; 32];
        // Four reserved bytes, then attribute 1 with its 24 bytes of times.
        field[4..6].copy_from_slice(&1u16.to_le_bytes());
        field[6..8].copy_from_slice(&24u16.to_le_bytes());
        for (i, time) in [self.modified, self.accessed, self.created]
            .into_iter()
            .enumerate()
        {
            let at = 8 + i * 8;
            field[at..at + 8].copy_from_slice(&time.unwrap_or(0).to_le_bytes());
        }
        field
    }

    /// The times stored with a zip entry by [`zip_folder`].
    fn of_entry<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> Self {
        let known = |t: u64| (t != 0).then_some(t);
        entry
            .extra_data_fields()
            .find_map(|field| match field {
                ExtraField::Ntfs(ntfs) => Some(ItemTimes {
                    modified: known(ntfs.mtime()),
                    accessed: known(ntfs.atime()),
                    created: known(ntfs.ctime()),
                }),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// A Windows file time as a `SystemTime`.
pub fn filetime_to_system(filetime: u64) -> Option<SystemTime> {
    let since = |intervals: u64| {
        Duration::new(
            intervals / 10_000_000,
            (intervals % 10_000_000 * 100) as u32,
        )
    };
    if filetime >= FILETIME_UNIX_EPOCH {
        SystemTime::UNIX_EPOCH.checked_add(since(filetime - FILETIME_UNIX_EPOCH))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(since(FILETIME_UNIX_EPOCH - filetime))
    }
}

/// The dates to set, or `None` when there are none.
fn file_times(modified: Option<SystemTime>, created: Option<SystemTime>) -> Option<FileTimes> {
    if modified.is_none() && created.is_none() {
        return None;
    }
    let mut times = FileTimes::new();
    if let Some(modified) = modified {
        times = times.set_modified(modified);
    }
    if let Some(created) = created {
        times = times.set_created(created);
    }
    Some(times)
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

/// The MS-DOS date and time of a zip entry: the local time of `filetime`,
/// with the daylight saving rules of that date. Outside the years the
/// format can hold (1980–2107), the zip default.
fn dos_time(filetime: Option<u64>) -> DateTime {
    filetime
        .and_then(local_time)
        .and_then(|t| {
            DateTime::from_date_and_time(
                t.wYear,
                t.wMonth as u8,
                t.wDay as u8,
                t.wHour as u8,
                t.wMinute as u8,
                t.wSecond as u8,
            )
            .ok()
        })
        .unwrap_or_default()
}

fn local_time(filetime: u64) -> Option<SYSTEMTIME> {
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
    ok.then_some(local)
}

/// Extensions of formats that are already compressed; storing them is faster
/// than deflating them again for practically no size difference.
const STORE_EXTENSIONS: &[&str] = &[
    "zip", "7z", "rar", "gz", "tgz", "bz2", "xz", "zst", "cab", "jpg", "jpeg", "png", "gif",
    "webp", "heic", "avif", "mp3", "aac", "ogg", "opus", "flac", "m4a", "mp4", "m4v", "mkv", "mov",
    "avi", "webm", "wmv", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub", "jar", "apk", "msi",
    "iso",
];

struct Entry {
    path: PathBuf,
    /// Relative name inside the archive, '/' separated.
    name: String,
    is_dir: bool,
    /// The file or folder is hidden (see [`unzip`]).
    hidden: bool,
    size: u64,
    times: ItemTimes,
}

impl Entry {
    /// The entry's dates go both into the standard MS-DOS field and, exactly,
    /// into the NTFS extra field; its MS-DOS attributes say whether it is a
    /// folder and whether it is hidden, under the Unix mode that the zip
    /// crate would write by itself.
    fn options(
        &self,
        base: SimpleFileOptions,
    ) -> anyhow::Result<FullFileOptions<'static, 'static>> {
        let (mode, folder) = if self.is_dir {
            (0o040755, FILE_ATTRIBUTE_DIRECTORY)
        } else {
            (0o100644, 0)
        };
        let hidden = if self.hidden {
            FILE_ATTRIBUTE_HIDDEN
        } else {
            0
        };
        let mut options = base
            .last_modified_time(dos_time(self.times.modified))
            .system(System::Dos)
            .external_attributes(mode << 16 | folder | hidden)
            .into_full_options();
        options.add_extra_field(NTFS_EXTRA_FIELD, self.times.ntfs_extra(), false)?;
        Ok(options)
    }
}

fn collect(root: &Path) -> anyhow::Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let read = std::fs::read_dir(&dir)
            .with_context(|| tr!(error_cannot_read_folder, path = dir.display()))?;
        for item in read {
            let item = item?;
            // symlink_metadata: do not follow links/junctions (avoids loops and escaping the folder).
            let meta = item.path().symlink_metadata()?;
            if meta.file_type().is_symlink() {
                continue;
            }
            let name = format!("{prefix}{}", item.file_name().to_string_lossy());
            let times = ItemTimes::of(&meta);
            let hidden = meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0;
            if meta.is_dir() {
                out.push(Entry {
                    path: item.path(),
                    name: format!("{name}/"),
                    is_dir: true,
                    hidden,
                    size: 0,
                    times,
                });
                stack.push((item.path(), format!("{name}/")));
            } else if meta.is_file() {
                out.push(Entry {
                    path: item.path(),
                    name,
                    is_dir: false,
                    hidden,
                    size: meta.len(),
                    times,
                });
            }
        }
    }
    Ok(out)
}

/// What [`zip_folder`] packed: the number of files, their total
/// (uncompressed) size and the size of the largest one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packed {
    pub files: u64,
    pub size: u64,
    pub largest: u64,
}

/// Packs the *contents* of `root` into a zip file at `dest` and says what
/// it packed. `progress(done_bytes, total_bytes)` is called regularly.
pub fn zip_folder(
    root: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<Packed> {
    // The folder is walked once; the same list gives the statistics for the offer.
    let entries = collect(root)?;
    let file_count = entries.iter().filter(|e| !e.is_dir).count() as u64;
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let largest = entries.iter().map(|e| e.size).max().unwrap_or(0);
    let mut zip = ZipWriter::new(BufWriter::with_capacity(BUF_SIZE, File::create(dest)?));
    let mut done = 0u64;
    let mut buf = vec![0u8; BUF_SIZE];
    for entry in &entries {
        if cancel.load(Ordering::Relaxed) {
            bail!(Cancelled);
        }
        if entry.is_dir {
            zip.add_directory(
                entry.name.as_str(),
                entry.options(SimpleFileOptions::default())?,
            )?;
            continue;
        }
        let ext = Path::new(&entry.name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let options = if STORE_EXTENSIONS.contains(&ext.as_str()) {
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
        } else {
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(1))
        }
        .large_file(entry.size >= u32::MAX as u64);
        zip.start_file(entry.name.as_str(), entry.options(options)?)?;
        let mut file = File::open(&entry.path)
            .with_context(|| tr!(error_cannot_open, path = entry.path.display()))?;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
            if cancel.load(Ordering::Relaxed) {
                bail!(Cancelled);
            }
        }
    }
    zip.finish()?.flush()?;
    Ok(Packed {
        files: file_count,
        size: total,
        largest,
    })
}

/// Extracts the open zip file `zip` into the folder `dest`, which must be new
/// (see [`create_unique_dir`]): what is created in it follows links, so
/// nothing but this extraction may have put anything there. Entries trying
/// to escape the destination ("../", absolute paths) are rejected. Files and
/// folders get the dates stored with them, and stay hidden if they were (see
/// [`is_hidden`]); with `mark`, files also get the mark of the web. The dates
/// of `dest` itself are the caller's business: an entry for it ("./") is
/// skipped.
pub fn unzip(
    zip: File,
    dest: &Path,
    mark: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<()> {
    let mut archive = ZipArchive::new(zip)?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        total += archive.by_index(i)?.size();
    }
    std::fs::create_dir_all(dest)?;
    let mut done = 0u64;
    let mut buf = vec![0u8; BUF_SIZE];
    // Creating anything in a folder changes its modified date, so the dates
    // of folders are set at the end.
    let mut folders = Vec::new();
    for i in 0..archive.len() {
        if cancel.load(Ordering::Relaxed) {
            bail!(Cancelled);
        }
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.name().ok().and_then(|name| entry_path(&name)) else {
            bail!(tr!(error_unsafe_path, path = format!("{:?}", entry.name())));
        };
        if rel.as_os_str().is_empty() {
            if entry.is_dir() {
                continue;
            }
            bail!(tr!(error_unsafe_path, path = format!("{:?}", entry.name())));
        }
        let target = dest.join(rel);
        let times = ItemTimes::of_entry(&entry);
        let hidden = is_hidden(&entry);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            folders.push((target, times, hidden));
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            // Also reading the attributes: a hidden file is hidden through
            // this handle (see `hide_open`).
            .access_mode(GENERIC_WRITE | FILE_READ_ATTRIBUTES)
            .open(&target)?;
        // The mark before the data and the dates: writing it changes the
        // modified date.
        if mark {
            let _ = mark_of_the_web(&target);
        }
        let mut out = BufWriter::with_capacity(BUF_SIZE, file);
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
            // Also within a file: a large one would keep being written
            // long after the cancel.
            if cancel.load(Ordering::Relaxed) {
                bail!(Cancelled);
            }
        }
        out.flush()?;
        // All written: the dates, and whether it is hidden, go on through
        // the same handle.
        times.apply_to(out.get_ref());
        if hidden {
            let _ = hide_open(out.get_ref());
        }
    }
    // Children before their parents, so that nothing touches a folder after
    // its date has been set.
    folders.sort_by_key(|(path, ..)| std::cmp::Reverse(path.components().count()));
    for (path, times, hidden) in &folders {
        times.apply(path);
        if *hidden {
            let _ = hide(path);
        }
    }
    Ok(())
}

/// Whether a zip entry is a hidden file or folder, as [`zip_folder`] notes
/// it (MS-DOS attributes). That is all that is kept of the attributes. A
/// sender's read-only file would hold up the deletion of a folder that is
/// unpacked only in part, on FAT drives and before Windows 10; a folder
/// that is read-only or a system folder makes File Explorer follow the
/// `desktop.ini` in it, which can point anywhere; and a system file is
/// hidden even from those who show hidden files.
fn is_hidden<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> bool {
    entry.system() == System::Dos && entry.external_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

/// Makes the file or folder at `path` hidden (see [`hide_open`]).
fn hide(path: &Path) -> io::Result<()> {
    let item = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    hide_open(&item)
}

/// Makes the open file or folder `item` hidden, keeping its other
/// attributes and its dates (the zero dates of the change leave them as
/// they are). Its handle needs to read and write the attributes.
fn hide_open(item: &File) -> io::Result<()> {
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

/// The path of a zip entry named `name` inside the folder it is unpacked
/// into, without "." and ".." ("a/../b" is "b", and nothing creates "a"):
/// empty for the folder itself. `None` for a name that reaches above that
/// folder, or that has a part Windows would not take as one plain file or
/// folder name (see [`is_plain_name`]): a drive ("C:"), a stream ("a:b"), a
/// device ("CON"), a name it would shorten ("a.").
///
/// Checked here rather than with the zip crate's `enclosed_name`, whose check
/// for device names panics on many ordinary names, such as "Kuća.jpg" or
/// any Cyrillic one (it splits the name at byte 3; zip 9.1 and 9.2).
fn entry_path(name: &str) -> Option<PathBuf> {
    let mut parts = Vec::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ if is_plain_name(part) => parts.push(part),
            _ => return None,
        }
    }
    Some(parts.iter().collect())
}

/// Whether `name` can be a single file or folder name on Windows as it is:
/// not empty, at most 255 UTF-16 units, none of the characters Windows does
/// not allow in a name (":" would also make it a drive or a stream), no
/// trailing dot or space (which Windows drops), and not a device name.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.encode_utf16().count() <= 255
        && !name.chars().any(|c| {
            matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 32
        })
        && !name.ends_with(['.', ' '])
        && !is_device_name(name)
}

/// Whether Windows takes the file name `name` for a device: "CON", "nul.txt",
/// "COM1 .log", "lpt²", whatever follows the first dot.
fn is_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches([' ', '.']);
    const DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"];
    if DEVICES.iter().any(|d| stem.eq_ignore_ascii_case(d)) {
        return true;
    }
    // COM and LPT with one digit, 1 to 9 or a superscript ¹, ² or ³. `get`,
    // because byte 3 may be inside a character ("Kuća").
    match (stem.get(..3), stem.get(3..)) {
        (Some(prefix), Some(number))
            if prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT") =>
        {
            let mut chars = number.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('1'..='9' | '¹' | '²' | '³'), None)
            )
        }
        _ => false,
    }
}

/// Makes a name received from the network safe to use as a single file or
/// folder name on Windows.
pub fn sanitize_file_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect();
    // Windows does not allow trailing dots or spaces.
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let s = s.trim_start().to_string();
    if s.is_empty() || s == "." || s == ".." {
        return "Received".into();
    }
    if is_device_name(&s) {
        return format!("_{s}");
    }
    s
}

/// The names to try for a new item, in order: `name`, then `name (2).ext`,
/// `name (3).ext`, … (a folder's number goes at the end, as folder names
/// have no extension). A name that looks free may be taken by the time it is
/// used, so nothing checks for a free name first and uses it later: the
/// functions below take each name in one step that never replaces anything.
fn numbered(name: &str, is_folder: bool) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = match (is_folder, name.rfind('.')) {
        (false, Some(pos)) if pos > 0 => (&name[..pos], &name[pos..]),
        _ => (name, ""),
    };
    std::iter::once(name.to_string()).chain((2..).map(move |i| format!("{stem} ({i}){ext}")))
}

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
fn listed(path: &Path) -> bool {
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
fn is_taken(error: &io::Error, path: &Path) -> bool {
    error.kind() == io::ErrorKind::AlreadyExists
        || (error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) && name_taken(path))
}

/// The identity of a file or folder: its volume and its number there (the
/// 128-bit file ID, which is unique on ReFS too; on Windows 7, which has
/// none, the 64-bit file index). It stays with the item when the item is
/// renamed, and no other item has it while this one exists; NTFS and ReFS
/// never give it to another item later either.
///
/// FAT and exFAT have no such numbers: the one they report can change when
/// an item is renamed, and a new item can get the one of a deleted item.
/// There the identity is the volume and the time the item was created,
/// which tells it from an item made under its name later (see
/// [`same_item`](Self::same_item)). Windows gives a new item, or one renamed
/// to a name, the creation time of an item of that name that was deleted or
/// renamed moments before ("tunnelling"), so new items of a transfer are
/// given the time of their creation ([`created_now`]), and a renamed one
/// its own time back ([`keep_creation_time`]). In identities noted by
/// earlier versions, and where the file system keeps no creation time,
/// every item of a volume has the same identity: an item counts as the same
/// one while it has its name, as if there were no identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    volume: u64,
    file: [u8; 16],
    /// FAT and exFAT: when the item was created (a Windows file time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<u64>,
}

impl FileId {
    /// The identity of an open file or folder.
    pub fn of(item: &File) -> io::Result<FileId> {
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

    /// Whether the identity tells the item apart from the other items of its
    /// volume: by its number, or on FAT and exFAT by its creation time; not
    /// where the name has to tell (see [`FileId`]).
    pub fn tells_apart(&self) -> bool {
        self.numbered() || self.created.is_some()
    }

    /// Whether the identity has the item's number: not on FAT and exFAT.
    fn numbered(&self) -> bool {
        self.file != [0; 16]
    }

    /// Whether the identity tells the item by its creation time (on FAT and
    /// exFAT): giving the item another one makes it look like another item.
    pub fn by_creation_time(&self) -> bool {
        self.created.is_some()
    }

    /// Whether `now`, the identity of what has the item's name now, is this
    /// item: the same volume and number, and on FAT and exFAT the same
    /// creation time (see [`same_creation`]). Where this identity has no
    /// creation time, anything of the volume under the name is.
    pub fn same_item(&self, now: &FileId) -> bool {
        self.volume == now.volume
            && self.file == now.file
            && match (self.created, now.created) {
                (Some(noted), Some(now)) => same_creation(noted, now),
                (Some(_), None) => false,
                (None, _) => true,
            }
    }

    /// Tests: the identity as earlier versions noted it on FAT and exFAT,
    /// without the creation time.
    #[cfg(test)]
    pub fn without_creation_time(self) -> FileId {
        FileId {
            created: None,
            ..self
        }
    }
}

/// The precision of a creation time on FAT and exFAT, in Windows file time
/// units (100 ns): 10 ms.
const FAT_CREATION_STEP: u64 = 100_000;

/// Whether `a` and `b` (Windows file times) are the same creation time of an
/// item on FAT or exFAT, read at different moments. FAT keeps local time,
/// which Windows reads with the time zone of the moment, so after a change to
/// or from daylight saving time, or of the time zone, the same item seems to
/// have been created whole quarter hours earlier or later, at most 26 hours
/// (from 12 hours behind UTC to 14 ahead); an item made at another moment
/// differs by hundredths of a second as well. Both are taken to the 10 ms
/// that the file system keeps: a time read right after it was set may still
/// be finer.
fn same_creation(a: u64, b: u64) -> bool {
    const QUARTER_HOUR: u64 = 15 * 60 * 10_000_000 / FAT_CREATION_STEP;
    const MOST_APART: u64 = 26 * 60 * 60 * 10_000_000 / FAT_CREATION_STEP;
    let apart = (a / FAT_CREATION_STEP).abs_diff(b / FAT_CREATION_STEP);
    apart <= MOST_APART && apart.is_multiple_of(QUARTER_HOUR)
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
    let set = match filetime_to_system(filetime_now() / FAT_CREATION_STEP * FAT_CREATION_STEP) {
        Some(now) => item.set_times(FileTimes::new().set_created(now)),
        None => Err(io::ErrorKind::InvalidData.into()),
    };
    let id = FileId::of(&item)?;
    match set {
        Err(e) if !id.numbered() => Err(e),
        _ => Ok(id),
    }
}

/// The current time as a Windows file time.
fn filetime_now() -> u64 {
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    FILETIME_UNIX_EPOCH
        + since_epoch.as_secs() * 10_000_000
        + u64::from(since_epoch.subsec_nanos() / 100)
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

#[cfg(test)]
thread_local! {
    /// Tests: this thread sees every volume as FAT or exFAT (see [`FileId`]).
    pub static PRETEND_FAT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

/// Creates an empty file under the first free one of the numbered names. The
/// name is the caller's from then on.
pub fn create_unique_file(dir: &Path, name: &str) -> io::Result<PathBuf> {
    for candidate in numbered(name, false) {
        let path = dir.join(candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if is_taken(&e, &path) => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("infinite iterator")
}

/// Creates a folder under the first free one of the numbered names for which
/// `also_free(name)` holds as well, for a name that goes with it (such as
/// its ZIP file's). The folder did not exist before, so everything in it is
/// the caller's.
pub fn create_unique_dir(
    dir: &Path,
    name: &str,
    also_free: impl Fn(&str) -> bool,
) -> io::Result<PathBuf> {
    for candidate in numbered(name, true).filter(|n| also_free(n)) {
        let path = dir.join(candidate);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if is_taken(&e, &path) => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("infinite iterator")
}

/// Moves the file `from` to `dir\name`, or under the next free one of the
/// numbered names, never replacing anything (see [`rename_new`]). Returns
/// where it went.
pub fn move_to_unique(from: &Path, dir: &Path, name: &str, is_folder: bool) -> io::Result<PathBuf> {
    move_unique(dir, name, is_folder, &mut |_| Ok(()), &mut |to| {
        rename_new(from, to)
    })
}

/// Like [`move_to_unique`], for the open file `file` (see [`rename_open`]),
/// calling `before(to)` before each attempt to rename it to `to`; an error
/// there ends it.
pub fn move_open_to_unique_noted(
    file: &File,
    dir: &Path,
    name: &str,
    before: &mut dyn FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    move_unique(dir, name, false, before, &mut |to| rename_open(file, to))
}

/// Renames something with `rename(to)` to `dir\name`, or under the next free
/// one of the numbered names, calling `before(to)` before each attempt.
fn move_unique(
    dir: &Path,
    name: &str,
    is_folder: bool,
    before: &mut dyn FnMut(&Path) -> io::Result<()>,
    rename: &mut dyn FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    for candidate in numbered(name, is_folder) {
        let to = dir.join(candidate);
        if name_taken(&to) {
            continue;
        }
        before(&to)?;
        match rename(&to) {
            Ok(()) => return Ok(to),
            // Taken since the check.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("infinite iterator")
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

/// The pauses between attempts on a file that another program holds for a
/// moment, such as a virus scanner that reads it: to delete it (see
/// [`delete_held`]), or to open the part file of a receive before the
/// transfer says that it waits for that program. About a second and a half
/// in all.
pub const HELD_RETRY_MS: &[u64] = &[50, 100, 200, 400, 800];

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

/// Runs `open`, which opens a file, again while another program keeps it
/// out (see [`is_busy`]), pausing between attempts as `pauses` says (in ms):
/// a virus scanner reading the file, say. The last error once they are over.
pub fn while_busy<T>(pauses: &[u64], open: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retried(pauses, is_busy, open)
}

/// Runs `op` until it works or fails other than as `busy` says, pausing
/// between attempts as `pauses` says (in ms); the last error once they are
/// over.
pub fn retried<T>(
    pauses: &[u64],
    mut busy: impl FnMut(&io::Error) -> bool,
    mut op: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut pauses = pauses.iter();
    loop {
        match op() {
            Err(e) if busy(&e) => match pauses.next() {
                Some(ms) => std::thread::sleep(Duration::from_millis(*ms)),
                None => return Err(e),
            },
            done => return done,
        }
    }
}

/// A path for a Win32 call: absolute, NUL-terminated, and with the `\\?\`
/// prefix when it is too long for the plain form (as `std::fs` does).
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
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
    let dir = std::env::temp_dir().join("Dodavo");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Where a profile keeps the temporary archives of its transfers: the
/// folder of [`temp_dir`], or a sub folder per `--profile`. Each profile
/// cleans up leftovers in its own folder only, so that it never deletes the
/// archive of a transfer that another profile is still running. The folder is
/// created when it is used (temp cleaners may remove it meanwhile).
pub fn temp_dir_for(profile: Option<&str>) -> PathBuf {
    let base = std::env::temp_dir().join("Dodavo");
    match profile {
        Some(p) => base.join("profiles").join(p),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dodavo-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn zip_roundtrip() {
        let base = scratch("zip");
        let src = base.join("src");
        std::fs::create_dir_all(src.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(src.join("empty")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub/deeper/b.bin"), vec![7u8; 1_000_000]).unwrap();
        std::fs::write(src.join("sub/photo.jpg"), b"jpg").unwrap();

        let zip = base.join("x.zip");
        let cancel = AtomicBool::new(false);
        let stats = zip_folder(&src, &zip, &cancel, &mut |_, _| {}).unwrap();
        assert_eq!(
            stats,
            Packed {
                files: 3,
                size: 5 + 1_000_000 + 3,
                largest: 1_000_000
            }
        );
        let out = base.join("out");
        unzip(
            File::open(&zip).unwrap(),
            &out,
            false,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(out.join("sub/deeper/b.bin")).unwrap().len(),
            1_000_000
        );
        assert_eq!(std::fs::read(out.join("sub/photo.jpg")).unwrap(), b"jpg");
        assert!(out.join("empty").is_dir());
        assert!(!out.join("a.txt:Zone.Identifier").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    /// Names with characters of more than one byte anywhere, which made the
    /// zip crate's own check of entry names panic (byte 3 inside "ć", "Д",
    /// "😀"), go through packing and unpacking.
    #[test]
    fn names_in_any_script_round_trip() {
        let base = scratch("names");
        let src = base.join("src");
        let names = [
            "Kuća.jpg",
            "Račun 2026.pdf",
            "Moć.txt",
            "Crème.txt",
            "Документи/Ђорђе.txt",
            "Lošinj ljeto/Vožnja.txt",
            "😀 emoji/x.txt",
            "a😀.txt",
            "中文资料.txt",
            "COMé.txt",
        ];
        for name in names {
            let path = src.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name.as_bytes()).unwrap();
        }
        let zip = base.join("x.zip");
        let cancel = AtomicBool::new(false);
        zip_folder(&src, &zip, &cancel, &mut |_, _| {}).unwrap();
        let out = base.join("out");
        unzip(
            File::open(&zip).unwrap(),
            &out,
            false,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        for name in names {
            assert_eq!(
                std::fs::read(out.join(name)).unwrap(),
                name.as_bytes(),
                "{name}"
            );
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn entry_paths() {
        let path = |name: &str| entry_path(name).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(path("a/b.txt").as_deref(), Some(r"a\b.txt"));
        assert_eq!(path(r"a\b.txt").as_deref(), Some(r"a\b.txt"));
        assert_eq!(path("Kuća/Račun.pdf").as_deref(), Some(r"Kuća\Račun.pdf"));
        assert_eq!(path("a/../b.txt").as_deref(), Some("b.txt"));
        assert_eq!(path("./a//b/").as_deref(), Some(r"a\b"));
        assert_eq!(path("/a.txt").as_deref(), Some("a.txt"));
        assert_eq!(path("./").as_deref(), Some(""));
        for unsafe_name in [
            "../a.txt",
            "a/../../b.txt",
            r"..\a.txt",
            "C:/a.txt",
            "c:a.txt",
            "a/b:stream",
            "a/CON",
            "nul.txt",
            "a./b",
            "a /b",
            "a\u{1}b",
            "a*b",
        ] {
            assert_eq!(path(unsafe_name), None, "{unsafe_name}");
        }
        assert_eq!(path(&"x".repeat(255)).map(|p| p.len()), Some(255));
        assert_eq!(path(&"x".repeat(256)), None);
    }

    #[test]
    fn device_names() {
        for name in [
            "CON",
            "con.txt",
            "Nul",
            "aux.tar.gz",
            "COM1",
            "com9.log",
            "LPT1 .txt",
            "COM¹",
            "lpt³.x",
            "CONIN$",
            "clock$",
        ] {
            assert!(is_device_name(name), "{name}");
        }
        for name in [
            "CONSOLE",
            "COM",
            "COM0",
            "COM10",
            "COMx",
            "LPT",
            "Kuća",
            "Čćžšđ",
            "Документ",
            "😀 emoji",
            "a😀",
            "COMé",
            "NULL.txt",
            "x.con",
        ] {
            assert!(!is_device_name(name), "{name}");
        }
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn dates(path: &Path) -> (SystemTime, SystemTime) {
        let meta = std::fs::metadata(path).unwrap();
        (meta.modified().unwrap(), meta.created().unwrap())
    }

    /// Files and folders keep their modified and created dates through
    /// packing and unpacking, also with the mark of the web, which is
    /// written before the dates are set.
    #[test]
    fn zip_keeps_dates() {
        let base = scratch("zip-dates");
        let src = base.join("src");
        std::fs::create_dir_all(src.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(src.join("empty")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub/deeper/b.bin"), b"bin").unwrap();
        let items = [
            ("a.txt", at(1_500_000_000), at(1_400_000_000)),
            ("sub/deeper/b.bin", at(1_000_000_001), at(999_999_999)),
            // Before 1980, which the MS-DOS date cannot hold.
            ("sub/deeper", at(200_000_000), at(100_000_000)),
            ("sub", at(1_600_000_000), at(1_550_000_000)),
            ("empty", at(1_700_000_000), at(1_650_000_000)),
        ];
        for (name, modified, created) in items {
            set_times(&src.join(name), Some(modified), Some(created)).unwrap();
        }

        let zip = base.join("x.zip");
        let cancel = AtomicBool::new(false);
        zip_folder(&src, &zip, &cancel, &mut |_, _| {}).unwrap();
        // The archive itself carries the dates: an MS-DOS date that is not
        // the zip default, and the exact times.
        let mut archive = ZipArchive::new(File::open(&zip).unwrap()).unwrap();
        let entry = archive.by_name("a.txt").unwrap();
        assert_ne!(entry.last_modified(), Some(DateTime::default()));
        assert_eq!(entry.last_modified().map(|d| d.year()), Some(2017));
        let times = ItemTimes::of_entry(&entry);
        assert_eq!(
            times.modified.and_then(filetime_to_system),
            Some(at(1_500_000_000))
        );
        assert_eq!(
            times.created.and_then(filetime_to_system),
            Some(at(1_400_000_000))
        );
        drop(entry);
        assert!(
            archive.file_names().all(|n| !n.unwrap().starts_with("src")),
            "only the contents are packed"
        );

        let out = base.join("out");
        unzip(
            File::open(&zip).unwrap(),
            &out,
            true,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        for (name, modified, created) in items {
            assert_eq!(dates(&out.join(name)), (modified, created), "{name}");
        }
        let mark = std::fs::read_to_string(out.join("a.txt:Zone.Identifier")).unwrap();
        assert_eq!(mark, "[ZoneTransfer]\r\nZoneId=3\r\n");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Hidden files and folders of a sent folder arrive hidden, with their
    /// dates and the mark of the web; nothing else of the attributes is kept.
    #[test]
    fn zip_keeps_hidden_items_hidden() {
        let base = scratch("zip-hidden");
        let src = base.join("src");
        std::fs::create_dir_all(src.join("secret")).unwrap();
        std::fs::write(src.join("secret/in.txt"), b"in").unwrap();
        std::fs::write(src.join("desktop.ini"), b"[x]").unwrap();
        std::fs::write(src.join("plain.txt"), b"plain").unwrap();
        std::fs::write(src.join("locked.txt"), b"ro").unwrap();
        let attribute = |path: &Path| path.symlink_metadata().unwrap().file_attributes();
        for name in ["secret", "desktop.ini"] {
            hide(&src.join(name)).unwrap();
        }
        let mut readonly = src.join("locked.txt").metadata().unwrap().permissions();
        readonly.set_readonly(true);
        std::fs::set_permissions(src.join("locked.txt"), readonly).unwrap();
        let created = at(1_400_000_000);
        set_times(
            &src.join("desktop.ini"),
            Some(at(1_500_000_000)),
            Some(created),
        )
        .unwrap();
        set_times(&src.join("secret"), Some(at(1_600_000_000)), Some(created)).unwrap();

        let zip = base.join("x.zip");
        let cancel = AtomicBool::new(false);
        zip_folder(&src, &zip, &cancel, &mut |_, _| {}).unwrap();
        let out = base.join("out");
        unzip(
            File::open(&zip).unwrap(),
            &out,
            true,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();

        let hidden = |name: &str| attribute(&out.join(name)) & FILE_ATTRIBUTE_HIDDEN != 0;
        assert!(hidden("secret") && hidden("desktop.ini"));
        assert!(!hidden("plain.txt") && !hidden("secret/in.txt") && !hidden("locked.txt"));
        assert!(out.join("secret").is_dir());
        assert!(
            !out.join("locked.txt")
                .metadata()
                .unwrap()
                .permissions()
                .readonly()
        );
        assert_eq!(
            dates(&out.join("desktop.ini")),
            (at(1_500_000_000), created)
        );
        assert_eq!(dates(&out.join("secret")), (at(1_600_000_000), created));
        let mark = std::fs::read_to_string(out.join("desktop.ini:Zone.Identifier")).unwrap();
        assert_eq!(mark, "[ZoneTransfer]\r\nZoneId=3\r\n");

        let mut writable = src.join("locked.txt").metadata().unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        std::fs::set_permissions(src.join("locked.txt"), writable).unwrap();
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn file_times() {
        assert_eq!(
            filetime_to_system(FILETIME_UNIX_EPOCH),
            Some(SystemTime::UNIX_EPOCH)
        );
        assert_eq!(
            filetime_to_system(FILETIME_UNIX_EPOCH + 15_000_001),
            Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(1_500_000_100))
        );
        assert_eq!(
            filetime_to_system(0),
            SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(11_644_473_600))
        );
        // The field reads back as written.
        let times = ItemTimes {
            modified: Some(3),
            accessed: None,
            created: Some(u64::MAX),
        };
        let field = times.ntfs_extra();
        assert_eq!(field.len(), 32);
        assert_eq!(&field[4..8], &[1, 0, 24, 0]);
        assert_eq!(&field[8..16], &3u64.to_le_bytes());
        assert_eq!(&field[16..24], &[0; 8]);
        assert_eq!(&field[24..32], &u64::MAX.to_le_bytes());
        // Dates the MS-DOS format cannot hold fall back to its default.
        assert_eq!(dos_time(None), DateTime::default());
        assert_eq!(dos_time(Some(1)), DateTime::default());
    }

    #[test]
    fn zip_slip_rejected() {
        let base = scratch("slip");
        let zip_path = base.join("evil.zip");
        let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
        zip.start_file("../evil.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        let cancel = AtomicBool::new(false);
        assert!(
            unzip(
                File::open(&zip_path).unwrap(),
                &base.join("out"),
                false,
                &cancel,
                &mut |_, _| {}
            )
            .is_err()
        );
        assert!(!base.join("evil.txt").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    /// An entry for the folder itself ("./" in a crafted zip) does not give
    /// it the dates stored with it: those are the caller's, and on FAT and
    /// exFAT its creation time tells it from other folders. "a/../b.txt" is
    /// "b.txt", with no "a" made for it, and a file entry that names the
    /// folder itself is rejected.
    #[test]
    fn entries_for_the_folder_itself_are_skipped() {
        let base = scratch("dot");
        let long_ago = filetime_now() - 40 * 365 * 24 * 60 * 60 * 10_000_000;
        let entry = Entry {
            path: base.clone(),
            name: "./".into(),
            is_dir: true,
            hidden: false,
            size: 0,
            times: ItemTimes {
                modified: Some(long_ago),
                accessed: None,
                created: Some(long_ago),
            },
        };
        let zip_path = base.join("dot.zip");
        let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
        let options = entry.options(SimpleFileOptions::default()).unwrap();
        zip.add_directory("./", options).unwrap();
        zip.start_file("a/../b.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"b").unwrap();
        zip.finish().unwrap();
        let out = base.join("out");
        let cancel = AtomicBool::new(false);
        unzip(
            File::open(&zip_path).unwrap(),
            &out,
            false,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        let created = std::fs::metadata(&out).unwrap().creation_time();
        assert!(
            filetime_now() - created < 10 * 60 * 10_000_000,
            "its own date"
        );
        assert_eq!(std::fs::read(out.join("b.txt")).unwrap(), b"b");
        assert!(!out.join("a").exists());

        let zip_path = base.join("file.zip");
        let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
        zip.start_file("a/..", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        let out = base.join("out2");
        assert!(
            unzip(
                File::open(&zip_path).unwrap(),
                &out,
                false,
                &cancel,
                &mut |_, _| {}
            )
            .is_err()
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Where the identity tells an item by its creation time (FAT and
    /// exFAT), the sender's creation time waits until the transfer's record
    /// is gone; elsewhere, and without an identity, nothing waits.
    #[test]
    fn a_creation_time_that_tells_waits() {
        let base = scratch("split");
        let path = base.join("a.txt");
        std::fs::write(&path, b"a").unwrap();
        let times = ItemTimes {
            modified: Some(1),
            accessed: Some(2),
            created: Some(3),
        };
        let numbered = file_id(&path).unwrap();
        assert_eq!(times.split(Some(&numbered)), (times, ItemTimes::default()));
        assert_eq!(times.split(None), (times, ItemTimes::default()));
        PRETEND_FAT.set(true);
        let by_time = file_id(&path).unwrap();
        PRETEND_FAT.set(false);
        let (now, later) = times.split(Some(&by_time));
        assert_eq!(
            (now.modified, now.accessed, now.created),
            (Some(1), Some(2), None)
        );
        assert_eq!(
            later,
            ItemTimes {
                created: Some(3),
                ..ItemTimes::default()
            }
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// On FAT and exFAT, an item is told by its creation time, which a new
    /// item of the transfer takes from the clock, not from one of its name
    /// that was deleted moments before, and which a renamed one gets back.
    /// Elsewhere the number tells.
    #[test]
    fn creation_times_tell_items_apart_on_fat() {
        let base = scratch("created");
        PRETEND_FAT.set(true);
        let a = base.join("a.txt");
        std::fs::write(&a, b"a").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(400 * 24 * 60 * 60);
        set_times(&a, None, Some(long_ago)).unwrap();
        let old = file_id(&a).unwrap();
        assert!(old.tells_apart());
        std::fs::remove_file(&a).unwrap();
        std::fs::write(&a, b"b").unwrap();
        let new = created_now(&a).unwrap();
        assert!(!old.same_item(&new));
        assert!(new.same_item(&file_id(&a).unwrap()));
        let created = std::fs::metadata(&a).unwrap().creation_time();
        assert_eq!(created % FAT_CREATION_STEP, 0, "whole hundredths");
        assert!(filetime_now() - created < 10 * 60 * 10_000_000);
        // Renamed to the name of an item deleted moments before, it may get
        // that one's time; it gets its own back.
        let b = base.join("b.txt");
        std::fs::rename(&a, &b).unwrap();
        set_times(&b, None, Some(long_ago)).unwrap();
        assert!(!new.same_item(&file_id(&b).unwrap()));
        let renamed = OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .open(&b)
            .unwrap();
        keep_creation_time(&renamed, &new).unwrap();
        drop(renamed);
        assert!(new.same_item(&file_id(&b).unwrap()));
        // As earlier versions noted it, the name tells.
        assert!(!old.without_creation_time().tells_apart());
        assert!(old.without_creation_time().same_item(&new));
        assert!(!new.same_item(&new.without_creation_time()));
        PRETEND_FAT.set(false);
        let id = file_id(&b).unwrap();
        assert!(id.tells_apart());
        assert_eq!(id, id.without_creation_time(), "no time noted");
        assert_eq!(created_now(&b).unwrap(), id);
        std::fs::remove_dir_all(&base).ok();
    }

    /// An item whose dates may not be changed (an access rule that denies
    /// writing its attributes, as on some shared folders) is told by its
    /// number all the same; on FAT and exFAT, where only its creation time
    /// tells, that is an error.
    #[test]
    fn an_item_whose_dates_are_locked_is_told_by_its_number() {
        let base = scratch("locked-dates");
        let path = base.join("a.txt");
        std::fs::write(&path, b"a").unwrap();
        // Everyone (S-1-1-0) may not write the attributes.
        let icacls = |args: &[&str]| {
            let status = std::process::Command::new("icacls")
                .arg(&path)
                .args(args)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "icacls {args:?}");
        };
        icacls(&["/deny", "*S-1-1-0:(WA)"]);
        assert!(set_times(&path, None, Some(SystemTime::now())).is_err());
        assert_eq!(created_now(&path).unwrap(), file_id(&path).unwrap());
        PRETEND_FAT.set(true);
        assert!(created_now(&path).is_err());
        PRETEND_FAT.set(false);
        icacls(&["/remove:d", "*S-1-1-0"]);
        std::fs::remove_dir_all(&base).ok();
    }

    /// A creation time on FAT, read with another time zone, is the same one
    /// whole quarter hours apart, up to 26 hours; one read right after it
    /// was set may be finer than the 10 ms that the file system keeps.
    #[test]
    fn creation_times_on_fat_compare_across_time_zones() {
        let t = 133_000_000_000_000_000;
        let minute = 60 * 10_000_000;
        assert!(same_creation(t, t));
        assert!(same_creation(t, t + 37) && same_creation(t + 37, t));
        assert!(same_creation(t + 15 * minute, t) && same_creation(t, t + 15 * minute));
        assert!(same_creation(t, t - 26 * 60 * minute));
        assert!(!same_creation(t, t + 26 * 60 * minute + 15 * minute));
        assert!(!same_creation(t, t + FAT_CREATION_STEP));
        assert!(!same_creation(t, t + 14 * minute));
    }

    #[test]
    fn numbered_names() {
        let first = |name, is_folder| numbered(name, is_folder).take(3).collect::<Vec<_>>();
        assert_eq!(first("a.txt", false), ["a.txt", "a (2).txt", "a (3).txt"]);
        assert_eq!(
            first("my.folder", true),
            ["my.folder", "my.folder (2)", "my.folder (3)"]
        );
        assert_eq!(
            first(".hidden", false),
            [".hidden", ".hidden (2)", ".hidden (3)"]
        );
    }

    /// An item keeps its identity when renamed; another item of the same
    /// name, later, has a different one.
    #[test]
    fn file_ids_follow_the_item() {
        let base = scratch("ids");
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir(&a).unwrap();
        let id = file_id(&a).unwrap();
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(file_id(&b).unwrap(), id);
        std::fs::create_dir(&a).unwrap();
        assert_ne!(file_id(&a).unwrap(), id);
        let f = base.join("f.txt");
        std::fs::write(&f, b"x").unwrap();
        assert_ne!(file_id(&f).unwrap(), file_id(&a).unwrap());
        assert_eq!(
            file_id(&base.join("missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Windows refuses to delete a file that a program has mapped into
    /// memory, as a virus scanner or the search indexer may, with "access
    /// denied", as for a file that may not be deleted at all: before
    /// Windows 10, and on file systems without POSIX deletion. Such a file
    /// goes once the program lets go, so the end of a transfer tries again
    /// (see `delete_held`); not for a read-only file, where that never
    /// helps.
    #[test]
    fn a_mapped_file_is_tried_again() {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Memory::{
                CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY, UnmapViewOfFile,
            },
        };
        let base = scratch("mapped");
        let path = base.join("a.txt");
        std::fs::write(&path, b"mapped").unwrap();
        let file = File::open(&path).unwrap();
        // SAFETY: the handle is open; the mapping and its view are let go
        // below.
        let mapping = unsafe {
            CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        };
        assert!(!mapping.is_null());
        // SAFETY: as above.
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
        assert!(!view.Value.is_null());
        drop(file);
        // Deleted the classic way, as there.
        let deleting = OpenOptions::new().access_mode(DELETE).open(&path).unwrap();
        let e = delete_on_close(&deleting).unwrap_err();
        drop(deleting);
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        assert!(delete_held(&e, &path));
        // SAFETY: the view and the mapping made above, not used after this.
        unsafe {
            UnmapViewOfFile(view);
            CloseHandle(mapping);
        }
        std::fs::remove_file(&path).unwrap();

        let kept = base.join("kept.txt");
        std::fs::write(&kept, b"k").unwrap();
        let mut permissions = std::fs::metadata(&kept).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&kept, permissions.clone()).unwrap();
        let denied = io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32);
        assert!(!delete_held(&denied, &kept));
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&kept, permissions).unwrap();
        assert!(delete_held(&denied, &kept), "may be deleted now");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A file that was deleted while another program still has it open
    /// keeps its name until it is closed: the next free name is used then,
    /// instead of failing with "access denied".
    #[test]
    fn a_name_that_is_being_deleted_is_taken() {
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let base = scratch("pending");
        let path = base.join("a.txt");
        std::fs::write(&path, b"x").unwrap();
        let other = File::open(&path).unwrap();
        // Deleted the classic way, as on Windows before 10 and on file
        // systems without POSIX deletion: pending while `other` is open.
        let deleting = OpenOptions::new().access_mode(DELETE).open(&path).unwrap();
        delete_on_close(&deleting).unwrap();
        drop(deleting);
        assert!(File::open(&path).is_err(), "cannot be opened now");
        assert!(listed(&path) && name_taken(&path));
        assert!(being_deleted(&path));
        let denied = io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32);
        assert!(delete_held(&denied, &path));
        assert!(!being_deleted(&base.join("none.txt")));
        assert!(!delete_held(&denied, &base.join("none.txt")));
        assert!(!listed(&base.join("b.txt")) && !listed(&base.join("a*.txt")));
        // `<`, `>` and `"` are wildcards too: never used as a pattern.
        assert!(!listed(&base.join("a<")) && !listed(&base.join("a\"txt")));
        assert_eq!(
            create_unique_file(&base, "a.txt").unwrap(),
            base.join("a (2).txt")
        );
        let part = base.join("b.part");
        std::fs::write(&part, b"mine").unwrap();
        assert_eq!(
            rename_new(&part, &path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            move_to_unique(&part, &base, "a.txt", false).unwrap(),
            base.join("a (3).txt")
        );
        // The same for an open file, renamed through its handle; each name
        // that is tried is noted first.
        let open_part = base.join("c.part");
        std::fs::write(&open_part, b"open").unwrap();
        let open = OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_READ)
            .open(&open_part)
            .unwrap();
        assert_eq!(
            rename_open(&open, &path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut tried = Vec::new();
        let moved = move_open_to_unique_noted(&open, &base, "a.txt", &mut |to| {
            tried.push(to.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(moved, base.join("a (4).txt"));
        assert_eq!(tried, std::slice::from_ref(&moved));
        drop(open);
        assert_eq!(std::fs::read(&moved).unwrap(), b"open");
        assert!(!open_part.exists());
        drop(other);
        assert!(!name_taken(&path), "free once it is closed");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Unpacking stops within a file when cancelled, with an error that
    /// says so.
    #[test]
    fn unzip_stops_when_cancelled() {
        let base = scratch("unzip-cancel");
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let big = vec![3u8; 3 * BUF_SIZE];
        std::fs::write(src.join("big.bin"), &big).unwrap();
        std::fs::write(src.join("small.txt"), b"small").unwrap();
        let zip = base.join("x.zip");
        let cancel = AtomicBool::new(false);
        zip_folder(&src, &zip, &cancel, &mut |_, _| {}).unwrap();

        let out = base.join("out");
        let mut calls = 0;
        let stopped = unzip(
            File::open(&zip).unwrap(),
            &out,
            false,
            &cancel,
            &mut |_, _| {
                calls += 1;
                cancel.store(true, Ordering::Relaxed);
            },
        );
        assert!(is_cancelled(&stopped.unwrap_err()));
        assert_eq!(calls, 1, "stopped after the first block");
        // Also when it comes wrapped in an I/O error.
        let wrapped: anyhow::Error = io::Error::other(Cancelled).into();
        assert!(is_cancelled(&wrapped));
        assert!(!is_cancelled(&anyhow::anyhow!("disk full")));
        std::fs::remove_dir_all(&base).ok();
    }

    /// Putting something under a free name never replaces what is there,
    /// also when the name was taken after it was found free.
    #[test]
    fn free_names_are_taken_without_replacing() {
        let base = scratch("free");
        std::fs::write(base.join("a.txt"), b"theirs").unwrap();
        let created = create_unique_file(&base, "a.txt").unwrap();
        assert_eq!(created, base.join("a (2).txt"));
        assert_eq!(std::fs::read(base.join("a.txt")).unwrap(), b"theirs");

        let part = base.join("a.txt.part");
        std::fs::write(&part, b"mine").unwrap();
        let err = rename_new(&part, &base.join("a.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        let moved = move_to_unique(&part, &base, "a.txt", false).unwrap();
        assert_eq!(moved, base.join("a (3).txt"));
        assert_eq!(std::fs::read(&moved).unwrap(), b"mine");
        assert_eq!(std::fs::read(base.join("a.txt")).unwrap(), b"theirs");
        assert!(!part.exists());
        // Through a handle, the same: what moves is the file that is open.
        let open_part = base.join("b.txt.part");
        std::fs::write(&open_part, b"open").unwrap();
        let open = OpenOptions::new()
            .access_mode(windows_sys::Win32::Storage::FileSystem::DELETE)
            .open(&open_part)
            .unwrap();
        let err = rename_open(&open, &base.join("a.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        let moved = move_open_to_unique_noted(&open, &base, "a.txt", &mut |_| Ok(())).unwrap();
        assert_eq!(moved, base.join("a (4).txt"));
        drop(open);
        assert_eq!(std::fs::read(&moved).unwrap(), b"open");
        assert_eq!(std::fs::read(base.join("a.txt")).unwrap(), b"theirs");
        assert!(!open_part.exists());

        // A folder whose ZIP name must be free too: "Photos (2).zip" is
        // taken, so the pair is "Photos (3)" and "Photos (3).zip".
        std::fs::create_dir(base.join("Photos")).unwrap();
        std::fs::write(base.join("Photos (2).zip"), b"").unwrap();
        let folder = create_unique_dir(&base, "Photos", |n| {
            !name_taken(&base.join(format!("{n}.zip")))
        })
        .unwrap();
        assert_eq!(folder, base.join("Photos (3)"));
        assert!(folder.is_dir());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn long_paths_get_the_verbatim_prefix() {
        let as_text = |w: Vec<u16>| String::from_utf16(&w[..w.len() - 1]).unwrap();
        let short = as_text(wide_path(Path::new(r"C:\a\b.txt")).unwrap());
        assert_eq!(short, r"C:\a\b.txt");
        let long = format!(r"C:\{}\b.txt", "x".repeat(250));
        assert_eq!(
            as_text(wide_path(Path::new(&long)).unwrap()),
            format!(r"\\?\{long}")
        );
        let unc = format!(r"\\server\share\{}", "y".repeat(250));
        assert_eq!(
            as_text(wide_path(Path::new(&unc)).unwrap()),
            format!(r"\\?\UNC\server\share\{}", "y".repeat(250))
        );
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_file_name("..\\..\\x.txt"), ".._.._x.txt");
        assert_eq!(sanitize_file_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_file_name(".."), "Received");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("COM1 .log"), "_COM1 .log");
        assert_eq!(sanitize_file_name("lpt².txt"), "_lpt².txt");
        assert_eq!(sanitize_file_name("Kuća.jpg"), "Kuća.jpg");
        assert_eq!(sanitize_file_name("name. "), "name");
    }
}
