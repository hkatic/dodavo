//! Folder packing/unpacking, file dates, the mark of the web and file name
//! helpers.

use std::{
    fs::{File, FileTimes, Metadata, OpenOptions},
    io::{self, BufWriter, Read, Write},
    os::windows::fs::{FileTimesExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use anyhow::{Context, bail};
use windows_sys::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES},
    System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
};
use zip::{
    CompressionMethod, DateTime, ExtraField, ZipArchive, ZipWriter,
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
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

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

/// Sets the modified and created dates of a file or folder.
pub fn set_times(
    path: &Path,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
) -> io::Result<()> {
    if modified.is_none() && created.is_none() {
        return Ok(());
    }
    let mut times = FileTimes::new();
    if let Some(modified) = modified {
        times = times.set_modified(modified);
    }
    if let Some(created) = created {
        times = times.set_created(created);
    }
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
    size: u64,
    times: ItemTimes,
}

impl Entry {
    /// The entry's dates go both into the standard MS-DOS field and, exactly,
    /// into the NTFS extra field.
    fn options(
        &self,
        base: SimpleFileOptions,
    ) -> anyhow::Result<FullFileOptions<'static, 'static>> {
        let mut options = base
            .last_modified_time(dos_time(self.times.modified))
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
            if meta.is_dir() {
                out.push(Entry {
                    path: item.path(),
                    name: format!("{name}/"),
                    is_dir: true,
                    size: 0,
                    times,
                });
                stack.push((item.path(), format!("{name}/")));
            } else if meta.is_file() {
                out.push(Entry {
                    path: item.path(),
                    name,
                    is_dir: false,
                    size: meta.len(),
                    times,
                });
            }
        }
    }
    Ok(out)
}

/// Packs the *contents* of `root` into a zip file at `dest` and returns the
/// number of files and their total (uncompressed) size.
/// `progress(done_bytes, total_bytes)` is called regularly.
pub fn zip_folder(
    root: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<(u64, u64)> {
    // The folder is walked once; the same list gives the statistics for the offer.
    let entries = collect(root)?;
    let file_count = entries.iter().filter(|e| !e.is_dir).count() as u64;
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let mut zip = ZipWriter::new(BufWriter::with_capacity(BUF_SIZE, File::create(dest)?));
    let mut done = 0u64;
    let mut buf = vec![0u8; BUF_SIZE];
    for entry in &entries {
        if cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
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
                bail!("Cancelled");
            }
        }
    }
    zip.finish()?.flush()?;
    Ok((file_count, total))
}

/// Extracts `zip_path` into the (new) folder `dest`. Entries trying to escape
/// the destination ("../", absolute paths) are rejected. Files and folders
/// get the dates stored with them; with `mark`, files also get the mark of
/// the web. The dates of `dest` itself are the caller's business.
pub fn unzip(
    zip_path: &Path,
    dest: &Path,
    mark: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> anyhow::Result<()> {
    let mut archive = ZipArchive::new(File::open(zip_path)?)?;
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
            bail!("Cancelled");
        }
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            bail!(tr!(error_unsafe_path, path = format!("{:?}", entry.name())));
        };
        let target = dest.join(rel);
        let times = ItemTimes::of_entry(&entry);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            folders.push((target, times));
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut out = BufWriter::with_capacity(BUF_SIZE, File::create(&target)?);
            loop {
                let n = entry.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                done += n as u64;
                progress(done, total);
            }
            out.flush()?;
        }
        // The file is closed: the mark first, as it changes the modified date.
        if mark {
            let _ = mark_of_the_web(&target);
        }
        times.apply(&target);
    }
    // Children before their parents, so that nothing touches a folder after
    // its date has been set.
    folders.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, times) in &folders {
        times.apply(path);
    }
    Ok(())
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
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return format!("_{s}");
    }
    s
}

/// Returns `dir\name`, or `dir\name (2).ext`, `dir\name (3).ext`, ... if it already exists.
pub fn unique_path(dir: &Path, name: &str, is_folder: bool) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match (is_folder, name.rfind('.')) {
        (false, Some(pos)) if pos > 0 => (&name[..pos], &name[pos..]),
        _ => (name, ""),
    };
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .expect("infinite iterator")
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
        assert_eq!(stats, (3, 5 + 1_000_000 + 3));
        let out = base.join("out");
        unzip(&zip, &out, false, &cancel, &mut |_, _| {}).unwrap();
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
        unzip(&zip, &out, true, &cancel, &mut |_, _| {}).unwrap();
        for (name, modified, created) in items {
            assert_eq!(dates(&out.join(name)), (modified, created), "{name}");
        }
        let mark = std::fs::read_to_string(out.join("a.txt:Zone.Identifier")).unwrap();
        assert_eq!(mark, "[ZoneTransfer]\r\nZoneId=3\r\n");
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
        assert!(unzip(&zip_path, &base.join("out"), false, &cancel, &mut |_, _| {}).is_err());
        assert!(!base.join("evil.txt").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn unique_names() {
        let base = scratch("unique");
        assert_eq!(unique_path(&base, "a.txt", false), base.join("a.txt"));
        std::fs::write(base.join("a.txt"), b"").unwrap();
        std::fs::write(base.join("a (2).txt"), b"").unwrap();
        assert_eq!(unique_path(&base, "a.txt", false), base.join("a (3).txt"));
        std::fs::create_dir(base.join("my.folder")).unwrap();
        assert_eq!(
            unique_path(&base, "my.folder", true),
            base.join("my.folder (2)")
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_file_name("..\\..\\x.txt"), ".._.._x.txt");
        assert_eq!(sanitize_file_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_file_name(".."), "Received");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("name. "), "name");
    }
}
