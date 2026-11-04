//! Folder packing/unpacking, file dates, the mark of the web and file name
//! helpers.

#[cfg(target_os = "macos")]
use std::os::macos::fs::FileTimesExt;
#[cfg(windows)]
use std::os::windows::fs::FileTimesExt;
use std::{
    fs::{File, FileTimes, Metadata, OpenOptions},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use zip::{
    CompressionMethod, DateTime, ExtraField, System, ZipArchive, ZipWriter,
    write::{FullFileOptions, SimpleFileOptions},
};

use crate::i18n::tr;

// What differs between the operating systems. Whatever the file system can and
// cannot do, what the functions promise stays the same; where a promise cannot
// be kept, the module says so.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use self::macos as os;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as os;

mod names;
pub use names::offer_name;

pub use os::{
    being_deleted, cannot_tell, connection_options, created_now, delete_held, delete_on_close,
    file_id, file_system, is_busy, keep_creation_time, last_step_options, length_options,
    mark_of_the_web, name_taken, rename_new, rename_open, set_times, temp_dir, temp_dir_for,
};
#[cfg(target_os = "macos")]
pub use os::{full_sync_refused, sync_as_far_as_possible};
use os::{hide, hide_open, is_taken};

const BUF_SIZE: usize = 256 * 1024;

/// The MS-DOS attributes of a zip entry that [`zip_folder`] notes.
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

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
        os::item_times(meta)
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

/// A `SystemTime` as a Windows file time.
pub fn system_to_filetime(time: SystemTime) -> Option<u64> {
    let intervals = |since: Duration| {
        since
            .as_secs()
            .checked_mul(10_000_000)?
            .checked_add(u64::from(since.subsec_nanos() / 100))
    };
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => FILETIME_UNIX_EPOCH.checked_add(intervals(since)?),
        Err(before) => FILETIME_UNIX_EPOCH.checked_sub(intervals(before.duration())?),
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

/// The MS-DOS date and time of a zip entry: the local time of `filetime`,
/// with the daylight saving rules of that date. Outside the years the
/// format can hold (1980–2107), the zip default.
fn dos_time(filetime: Option<u64>) -> DateTime {
    filetime
        .and_then(os::local_time)
        .and_then(|(year, month, day, hour, minute, second)| {
            DateTime::from_date_and_time(year, month, day, hour, minute, second).ok()
        })
        .unwrap_or_default()
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
    /// A file that can be run (see [`os::is_executable`]).
    executable: bool,
    size: u64,
    times: ItemTimes,
    /// What the name does not say (see [`names`]).
    fidelity: names::Fidelity,
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
        } else if self.executable {
            (0o100755, 0)
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
        if let Some(data) = self.fidelity.clone().encode() {
            options.add_extra_field(names::EXTRA_FIELD, data, false)?;
        }
        Ok(options)
    }
}

/// Whether this system writes zips for any receiver (see [`names`]). Windows'
/// own names are fine as they are.
const PORTABLE_NAMES: bool = cfg!(target_os = "macos");

/// The folder's items as they go into the zip. With `portable`, as [`names`]
/// says.
fn collect(root: &Path, portable: bool) -> anyhow::Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut namer = names::Namer::default();
    let mut root_links = Vec::new();
    // Each folder with its name in the zip, its depth and its entry.
    let mut stack = vec![(root.to_path_buf(), String::new(), 0, None)];
    while let Some((dir, prefix, levels, entry_of_dir)) = stack.pop() {
        let read = std::fs::read_dir(&dir)
            .with_context(|| tr!(error_cannot_read_folder, path = dir.display()))?;
        let mut links = Vec::new();
        for item in read {
            let item = item?;
            let own_name = item.file_name().to_string_lossy().into_owned();
            if portable && names::is_system_item(&own_name) {
                continue;
            }
            // symlink_metadata: do not follow links/junctions (avoids loops and escaping the folder).
            let meta = item.path().symlink_metadata()?;
            if meta.file_type().is_symlink() {
                if portable
                    && let Some(target) = std::fs::read_link(item.path())
                        .ok()
                        .and_then(|t| t.to_str().map(str::to_string))
                        .filter(|t| names::link_stays_inside(levels, t))
                {
                    links.push((own_name, target));
                }
                continue;
            }
            let (short, original) = if portable {
                let short = namer.name(&prefix, &own_name, meta.is_dir());
                // Only what composing does not explain: it would carry
                // nearly every name that came from HFS+ along otherwise.
                let same = names::composed(&own_name) == short;
                (short, (!same).then_some(own_name.clone()))
            } else {
                (own_name.clone(), None)
            };
            let name = format!("{prefix}{short}");
            let times = ItemTimes::of(&meta);
            // On macOS, a name with a dot in front is hidden just the same.
            let hidden = os::is_hidden(&meta) || (portable && own_name.starts_with('.'));
            let fidelity = names::Fidelity {
                name: original,
                links: Vec::new(),
            };
            if meta.is_dir() {
                out.push(Entry {
                    path: item.path(),
                    name: format!("{name}/"),
                    is_dir: true,
                    hidden,
                    executable: false,
                    size: 0,
                    times,
                    fidelity,
                });
                stack.push((
                    item.path(),
                    format!("{name}/"),
                    levels + 1,
                    Some(out.len() - 1),
                ));
            } else if meta.is_file() {
                out.push(Entry {
                    path: item.path(),
                    name,
                    is_dir: false,
                    hidden,
                    executable: os::is_executable(&meta),
                    size: meta.len(),
                    times,
                    fidelity,
                });
            }
        }
        match entry_of_dir {
            Some(entry) => {
                let entry: &mut Entry = &mut out[entry];
                entry.fidelity.links = links;
            }
            None => root_links = links,
        }
    }
    // The links at the top go on an entry for the folder itself, which
    // `unzip` of every version skips.
    if !root_links.is_empty() {
        out.push(Entry {
            path: root.to_path_buf(),
            name: "./".into(),
            is_dir: true,
            hidden: false,
            executable: false,
            size: 0,
            times: ItemTimes::default(),
            fidelity: names::Fidelity {
                name: None,
                links: root_links,
            },
        });
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
    zip_folder_as(root, dest, PORTABLE_NAMES, cancel, progress)
}

/// [`zip_folder`], with `portable` as in [`collect`].
fn zip_folder_as(
    root: &Path,
    dest: &Path,
    portable: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<Packed> {
    // The folder is walked once; the same list gives the statistics for the offer.
    let entries = collect(root, portable)?;
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
    if mark {
        os::mark_folder(dest);
    }
    // Empty on Windows, but not on a Mac.
    #[allow(clippy::default_constructed_unit_structs)]
    let mut unpacked_names = os::UnpackedNames::default();
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
        let fidelity = fidelity_of(&entry);
        if rel.as_os_str().is_empty() {
            if entry.is_dir() {
                unpacked_names.note_links(Path::new(""), fidelity.links);
                continue;
            }
            bail!(tr!(error_unsafe_path, path = format!("{:?}", entry.name())));
        }
        let (rel, links) = unpacked_names.place(rel, entry.is_dir(), fidelity);
        let target = dest.join(&rel);
        let times = ItemTimes::of_entry(&entry);
        let hidden = is_hidden(&entry);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            if mark {
                os::mark_folder(&target);
            }
            unpacked_names.note_links(&rel, links);
            folders.push((target, times, hidden));
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (file, target) = os::open_unpacked(target)?;
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
        os::keep_executable(out.get_ref(), entry.external_attributes());
        times.apply_to(out.get_ref());
        if hidden {
            let _ = hide_open(out.get_ref());
        }
    }
    // Last: nothing was written through them, and once they exist nothing is.
    unpacked_names.make_links(dest);
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

/// What the private field of a zip entry says (see [`names`]).
fn fidelity_of<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> names::Fidelity {
    entry
        .extra_data_fields()
        .find_map(|field| match field {
            ExtraField::Custom(custom) if custom.header_id == names::EXTRA_FIELD => {
                names::Fidelity::decode(&custom.data)
            }
            _ => None,
        })
        .unwrap_or_default()
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
    // APFS counts bytes, not characters, and the part and a number still have
    // to fit (`.zip.part`, " (99)").
    #[cfg(target_os = "macos")]
    let s = names::cut_to(&s, 240, names::bytes);
    s
}

/// The names to try for a new item, in order: `name`, then `name (2).ext`,
/// `name (3).ext`, … (a folder's number goes at the end, as folder names
/// have no extension). A name that looks free may be taken by the time it is
/// used, so nothing checks for a free name first and uses it later: the
/// functions below take each name in one step that never replaces anything.
fn numbered(name: &str, is_folder: bool) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = match (is_folder && !os::is_package(name), name.rfind('.')) {
        (false, Some(pos)) if pos > 0 => (&name[..pos], &name[pos..]),
        _ => (name, ""),
    };
    std::iter::once(name.to_string()).chain((2..).map(move |i| format!("{stem} ({i}){ext}")))
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
        os::file_id_of(item)
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

#[cfg(any(windows, test))]
/// The current time as a Windows file time.
fn filetime_now() -> u64 {
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    FILETIME_UNIX_EPOCH
        + since_epoch.as_secs() * 10_000_000
        + u64::from(since_epoch.subsec_nanos() / 100)
}

#[cfg(test)]
thread_local! {
    /// Tests: this thread sees every volume as FAT or exFAT (see [`FileId`]).
    pub static PRETEND_FAT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

/// The pauses between attempts on a file that another program holds for a
/// moment, such as a virus scanner that reads it: to delete it (see
/// [`delete_held`]), or to open the part file of a receive before the
/// transfer says that it waits for that program. About a second and a half
/// in all.
pub const HELD_RETRY_MS: &[u64] = &[50, 100, 200, 400, 800];

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

/// What tests do that differs between the operating systems.
#[cfg(test)]
pub mod testing {
    use super::*;
    #[cfg(windows)]
    use std::os::windows::fs::OpenOptionsExt;

    /// Keeps others from changing or deleting a file until it is dropped.
    pub struct Held {
        #[cfg(windows)]
        _file: File,
        #[cfg(target_os = "macos")]
        path: PathBuf,
    }

    /// Holds `path` as another program that has it open and shares nothing
    /// does.
    #[cfg(windows)]
    pub fn hold(path: &Path) -> Held {
        Held {
            _file: OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
                .unwrap(),
        }
    }

    /// Holds `path` as another program that has it open for reading and
    /// lets others read it does.
    #[cfg(windows)]
    pub fn hold_readable(path: &Path) -> Held {
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        Held {
            _file: OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(path)
                .unwrap(),
        }
    }

    /// Holds `path` as another program that has it open for writing does.
    #[cfg(windows)]
    pub fn hold_writing(path: &Path) -> Held {
        Held {
            _file: OpenOptions::new().write(true).open(path).unwrap(),
        }
    }

    /// No program can do that on macOS. What comes closest for the code under
    /// test is the immutable flag: changing or deleting the file is refused,
    /// until the flag is gone.
    #[cfg(target_os = "macos")]
    pub fn hold(path: &Path) -> Held {
        set_immutable(path, true);
        Held {
            path: path.to_path_buf(),
        }
    }

    /// See [`hold`].
    #[cfg(target_os = "macos")]
    pub fn hold_readable(path: &Path) -> Held {
        hold(path)
    }

    /// See [`hold`].
    #[cfg(target_os = "macos")]
    pub fn hold_writing(path: &Path) -> Held {
        hold(path)
    }

    #[cfg(target_os = "macos")]
    fn set_immutable(path: &Path, on: bool) {
        use std::{
            ffi::CString,
            os::{macos::fs::MetadataExt, unix::ffi::OsStrExt},
        };
        let flags = path.symlink_metadata().unwrap().st_flags();
        let flags = if on {
            flags | libc::UF_IMMUTABLE
        } else {
            flags & !libc::UF_IMMUTABLE
        };
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a C string that outlives the call.
        assert_eq!(unsafe { libc::chflags(c_path.as_ptr(), flags) }, 0);
    }

    #[cfg(target_os = "macos")]
    impl Drop for Held {
        fn drop(&mut self) {
            set_immutable(&self.path, false);
        }
    }

    /// Whether `path` has the mark of the web (see [`mark_of_the_web`]).
    #[cfg(windows)]
    pub fn has_mark(path: &Path) -> bool {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::read_to_string(stream).is_ok_and(|mark| mark == "[ZoneTransfer]\r\nZoneId=3\r\n")
    }

    /// Whether `path` has the mark of the web (see [`mark_of_the_web`]).
    #[cfg(target_os = "macos")]
    pub fn has_mark(path: &Path) -> bool {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let mut value = [0u8; 128];
        // SAFETY: C strings and a buffer of the given length, which outlive
        // the call.
        let len = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                c"com.apple.quarantine".as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        len > 0
            && value[..len as usize].starts_with(b"0083;")
            && value.windows(6).any(|w| w == b"Dodavo")
    }

    /// Makes `link`, a link to the folder `target`.
    #[cfg(windows)]
    pub fn link_folder(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Makes `link`, a link to the folder `target`.
    #[cfg(target_os = "macos")]
    pub fn link_folder(link: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    /// When the item at `path` was created, as a Windows file time.
    pub fn created(path: &Path) -> u64 {
        system_to_filetime(path.symlink_metadata().unwrap().created().unwrap()).unwrap()
    }

    /// Whether the item at `path` is hidden.
    pub fn hidden(path: &Path) -> bool {
        os::is_hidden(&path.symlink_metadata().unwrap())
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "archive/macos_volumes.rs"]
mod macos_volumes;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use os::{listed, wide_path};
    #[cfg(windows)]
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    #[cfg(windows)]
    use windows_sys::Win32::{
        Foundation::ERROR_ACCESS_DENIED,
        Storage::FileSystem::{DELETE, FILE_WRITE_ATTRIBUTES},
    };

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
        // With "/" for the separator of the system.
        let path = |name: &str| {
            entry_path(name).map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
        };
        assert_eq!(path("a/b.txt").as_deref(), Some("a/b.txt"));
        assert_eq!(path(r"a\b.txt").as_deref(), Some("a/b.txt"));
        assert_eq!(path("Kuća/Račun.pdf").as_deref(), Some("Kuća/Račun.pdf"));
        assert_eq!(path("a/../b.txt").as_deref(), Some("b.txt"));
        assert_eq!(path("./a//b/").as_deref(), Some("a/b"));
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
        assert!(testing::has_mark(&out.join("a.txt")));
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

        let hidden = |name: &str| testing::hidden(&out.join(name));
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
        assert!(testing::has_mark(&out.join("desktop.ini")));

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
            executable: false,
            size: 0,
            times: ItemTimes {
                modified: Some(long_ago),
                accessed: None,
                created: Some(long_ago),
            },
            fidelity: names::Fidelity::default(),
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
        let created = testing::created(&out);
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
        let created = testing::created(&a);
        #[cfg(windows)]
        assert_eq!(created % FAT_CREATION_STEP, 0, "whole hundredths");
        assert!(filetime_now() - created < 10 * 60 * 10_000_000);
        let b = base.join("b.txt");
        std::fs::rename(&a, &b).unwrap();
        // Renamed to the name of an item deleted moments before, it may get
        // that one's time on Windows; it gets its own back.
        #[cfg(windows)]
        {
            set_times(&b, None, Some(long_ago)).unwrap();
            assert!(!new.same_item(&file_id(&b).unwrap()));
            let renamed = OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .open(&b)
                .unwrap();
            keep_creation_time(&renamed, &new).unwrap();
            drop(renamed);
        }
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
    #[cfg(windows)]
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

    /// A zip as a Mac writes it unpacks under the rules of every receiver:
    /// on Windows under the names in the zip, which that accepts, and without
    /// links; on a Mac with each item's own name back and its links. What a
    /// peer says there is checked: it decides where things go.
    #[test]
    fn a_zip_for_any_receiver_unpacks_everywhere() {
        let base = scratch("any-receiver");
        let plain = |name: &str, is_dir, executable, fidelity| Entry {
            path: base.clone(),
            name: name.into(),
            is_dir,
            hidden: false,
            executable,
            size: 0,
            times: ItemTimes::default(),
            fidelity,
        };
        let named = |name: &str| names::Fidelity {
            name: Some(name.into()),
            links: Vec::new(),
        };
        let zip_path = base.join("mac.zip");
        let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
        let file = |zip: &mut ZipWriter<File>, entry: Entry, data: &[u8]| {
            let options = entry.options(SimpleFileOptions::default()).unwrap();
            zip.start_file(entry.name.as_str(), options).unwrap();
            zip.write_all(data).unwrap();
        };
        let root = plain(
            "./",
            true,
            false,
            names::Fidelity {
                name: None,
                links: vec![
                    ("to-a".into(), "a_b_.txt".into()),
                    ("to-sub".into(), "sub_/x.txt".into()),
                    // Those two must not come to be.
                    ("out".into(), "../secret".into()),
                    ("a/b".into(), "a_b_.txt".into()),
                ],
            },
        );
        zip.add_directory("./", root.options(SimpleFileOptions::default()).unwrap())
            .unwrap();
        file(
            &mut zip,
            plain("a_b_.txt", false, false, named("a:b?.txt")),
            b"a",
        );
        file(
            &mut zip,
            plain("run.sh", false, true, names::Fidelity::default()),
            b"r",
        );
        // A name that would go elsewhere is not used.
        file(
            &mut zip,
            plain("evil.txt", false, false, named("../evil.txt")),
            b"e",
        );
        let sub = plain("sub_/", true, false, named("su:b"));
        zip.add_directory(
            sub.name.as_str(),
            sub.options(SimpleFileOptions::default()).unwrap(),
        )
        .unwrap();
        file(
            &mut zip,
            plain("sub_/x.txt", false, false, names::Fidelity::default()),
            b"x",
        );
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

        assert_eq!(std::fs::read(out.join("evil.txt")).unwrap(), b"e");
        assert!(!base.join("evil.txt").exists());
        assert!(!out.join("out").exists() && !out.join("a").exists());
        #[cfg(windows)]
        {
            assert_eq!(std::fs::read(out.join("a_b_.txt")).unwrap(), b"a");
            assert_eq!(std::fs::read(out.join("sub_/x.txt")).unwrap(), b"x");
            assert!(!out.join("to-a").exists());
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::read(out.join("a:b?.txt")).unwrap(), b"a");
            assert_eq!(std::fs::read(out.join("su:b/x.txt")).unwrap(), b"x");
            assert!(!out.join("a_b_.txt").exists() && !out.join("sub_").exists());
            let mode = |name: &str| out.join(name).metadata().unwrap().permissions().mode() & 0o111;
            assert_ne!(mode("run.sh"), 0, "it can still be run");
            assert_eq!(mode("su:b/x.txt"), 0);
            assert!(out.join("to-a").is_symlink());
            assert!(out.join("to-sub").is_symlink());
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// What a Mac packs is what Windows accepts, whatever the names are here,
    /// and another Mac gets it back as it was.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_folder_with_any_names_goes_to_any_receiver() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("any-names");
        let src = base.join("src");
        std::fs::create_dir_all(src.join("su:b/in?ner")).unwrap();
        let nfd = "Kuc\u{301}a.jpg";
        for (name, data) in [
            ("a:b?.txt", "1"),
            ("CON.txt", "2"),
            ("trailing.", "3"),
            ("star*.log", "4"),
            ("su:b/in?ner/deep|file", "5"),
            (nfd, "6"),
            (".hidden", "7"),
            // What is not sent.
            (".DS_Store", "no"),
            ("._a.txt", "no"),
            ("Icon\r", "no"),
        ] {
            std::fs::write(src.join(name), data).unwrap();
        }
        let script = src.join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a:b?.txt", src.join("inside")).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", src.join("outside")).unwrap();
        std::os::unix::fs::symlink("../../secret", src.join("su:b/climbs")).unwrap();

        let zip = base.join("x.zip");
        let cancel = AtomicBool::new(false);
        let packed = zip_folder(&src, &zip, &cancel, &mut |_, _| {}).unwrap();
        assert_eq!(packed.files, 8);

        // Windows' rule (and what the released version would do with it).
        let mut archive = ZipArchive::new(File::open(&zip).unwrap()).unwrap();
        for i in 0..archive.len() {
            let entry = archive.by_index(i).unwrap();
            let name = entry.name().unwrap();
            assert!(entry_path(&name).is_some(), "{name}");
        }
        drop(archive);

        let out = base.join("out");
        unzip(
            File::open(&zip).unwrap(),
            &out,
            true,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        for (name, data) in [
            ("a:b?.txt", "1"),
            ("CON.txt", "2"),
            ("trailing.", "3"),
            ("star*.log", "4"),
            ("su:b/in?ner/deep|file", "5"),
            (".hidden", "7"),
        ] {
            assert_eq!(
                std::fs::read_to_string(out.join(name)).unwrap(),
                data,
                "{name}"
            );
        }
        // Normalization does not count here.
        assert_eq!(std::fs::read_to_string(out.join("Kuća.jpg")).unwrap(), "6");
        assert!(!out.join(".DS_Store").exists());
        assert!(!out.join("._a.txt").exists());
        assert_ne!(
            out.join("run.sh").metadata().unwrap().permissions().mode() & 0o111,
            0
        );
        assert_eq!(
            std::fs::read_link(out.join("inside")).unwrap(),
            Path::new("a:b?.txt")
        );
        assert!(!out.join("outside").exists() && !out.join("su:b/climbs").exists());
        assert!(testing::hidden(&out.join(".hidden")));
        assert!(
            testing::has_mark(&out.join("su:b")),
            "folders are marked too"
        );
        std::fs::remove_dir_all(&base).ok();
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
    #[cfg(windows)]
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
    #[cfg(windows)]
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
        let open = last_step_options(true).open(&open_part).unwrap();
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

    #[cfg(windows)]
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
