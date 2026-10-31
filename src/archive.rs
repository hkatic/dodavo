//! Folder packing/unpacking and file name helpers.

use std::{
    fs::File,
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, bail};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::i18n::tr;

const BUF_SIZE: usize = 256 * 1024;

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
            if meta.is_dir() {
                out.push(Entry {
                    path: item.path(),
                    name: format!("{name}/"),
                    is_dir: true,
                    size: 0,
                });
                stack.push((item.path(), format!("{name}/")));
            } else if meta.is_file() {
                out.push(Entry {
                    path: item.path(),
                    name,
                    is_dir: false,
                    size: meta.len(),
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
            zip.add_directory(entry.name.as_str(), SimpleFileOptions::default())?;
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
        zip.start_file(entry.name.as_str(), options)?;
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
/// the destination ("../", absolute paths) are rejected.
pub fn unzip(
    zip_path: &Path,
    dest: &Path,
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
    for i in 0..archive.len() {
        if cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
        }
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            bail!(tr!(error_unsafe_path, path = format!("{:?}", entry.name())));
        };
        let target = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
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
        unzip(&zip, &out, &cancel, &mut |_, _| {}).unwrap();
        assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(out.join("sub/deeper/b.bin")).unwrap().len(),
            1_000_000
        );
        assert_eq!(std::fs::read(out.join("sub/photo.jpg")).unwrap(), b"jpg");
        assert!(out.join("empty").is_dir());
        std::fs::remove_dir_all(&base).ok();
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
        assert!(unzip(&zip_path, &base.join("out"), &cancel, &mut |_, _| {}).is_err());
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
