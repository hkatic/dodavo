//! Whether an offer fits on the disk when saved into a folder: what the
//! Incoming Transfer window says, and what automatic acceptance goes by.

use std::path::Path;

use crate::{
    format::format_size,
    i18n::{os_tr, tr},
    net::{Kind, Offer, ReceivedFolder},
    platform::disk_free,
};

/// Whether an offer fits on the disk when saved into a folder.
pub struct Space {
    /// `None` if the free space could not be determined.
    pub enough: Option<bool>,
    /// `false` if the transfer would save a file there that is larger than
    /// the file system can hold (FAT32): then it fails, however much room
    /// there is, and the text says so.
    pub fits: bool,
    /// A sentence describing the result.
    pub text: String,
}

/// The largest file FAT32 can hold: 4 GiB less one byte.
const FAT32_MAX_FILE: u64 = u32::MAX as u64;

/// The largest single file a transfer of `offer` saves in the destination
/// folder: the file, the zip of a folder where it is kept, and the largest
/// file of a folder where it is unpacked. (The zip that is only unpacked is
/// in the temporary folder, which is not on FAT32 in practice, and which
/// choosing another folder could not help with.)
fn largest_saved_file(offer: &Offer, folder: ReceivedFolder) -> u64 {
    match (offer.kind, folder) {
        (Kind::File, _) | (Kind::Folder, ReceivedFolder::KeepZip) => offer.size,
        (Kind::Folder, ReceivedFolder::Both) => offer.size.max(offer.largest_file),
        (Kind::Folder, ReceivedFolder::Extract) => offer.largest_file,
    }
}

/// Bytes needed on the destination drive and on the drive of the temporary
/// folder. A folder that is only unpacked arrives as a zip in the temporary
/// folder and is then unpacked into the destination, so both need room at
/// the same time. A zip that is kept arrives in the destination.
fn space_needed(offer: &Offer, folder: ReceivedFolder, same_volume: bool) -> (u64, u64) {
    match (offer.kind, folder) {
        (Kind::File, _) | (Kind::Folder, ReceivedFolder::KeepZip) => (offer.size, 0),
        (Kind::Folder, ReceivedFolder::Both) => (offer.size.saturating_add(offer.content_size), 0),
        (Kind::Folder, ReceivedFolder::Extract) if same_volume => {
            (offer.size.saturating_add(offer.content_size), 0)
        }
        (Kind::Folder, ReceivedFolder::Extract) => (offer.content_size, offer.size),
    }
}

/// The name the Finder shows.
#[cfg(target_os = "macos")]
fn drive_name(root: &Path) -> String {
    crate::platform::volume_name(root).unwrap_or_else(|| root.display().to_string())
}

/// `C:\` is shown as `C:`; other volume paths (e.g. network shares) as they are.
#[cfg(not(target_os = "macos"))]
fn drive_name(root: &Path) -> String {
    let s = root.display().to_string();
    match s.strip_suffix('\\') {
        Some(d) if d.len() == 2 && d.ends_with(':') => d.to_string(),
        _ => s,
    }
}

/// Whether `offer` fits into `dir`, with `folder` deciding what is kept of a
/// folder and `temp` being the folder of the temporary archives.
pub fn check_space(offer: &Offer, dir: &Path, temp: &Path, folder: ReceivedFolder) -> Space {
    let unknown = || Space {
        enough: None,
        fits: true,
        text: tr!(space_unknown).to_string(),
    };
    let Some((dest_root, dest_free)) = disk_free(dir) else {
        return unknown();
    };
    let mut volumes = vec![(dest_root.clone(), dest_free)];
    let uses_temp = offer.kind == Kind::Folder && folder == ReceivedFolder::Extract;
    if uses_temp {
        let Some(temp) = disk_free(temp) else {
            return unknown();
        };
        volumes.push(temp);
    }
    let same_volume = volumes
        .get(1)
        .is_none_or(|(root, _)| root.as_os_str().eq_ignore_ascii_case(dest_root.as_os_str()));
    // A file system that limits the size of a file (FAT32, and FAT) fails
    // the transfer once a file reaches that size, however much room there is.
    let largest = largest_saved_file(offer, folder);
    if largest > FAT32_MAX_FILE
        && matches!(
            crate::archive::file_system(&dest_root).as_deref(),
            Some("FAT32" | "FAT")
        )
    {
        return Space {
            enough: None,
            fits: false,
            text: os_tr!(
                space_fat,
                drive = drive_name(&dest_root),
                size = format_size(largest)
            ),
        };
    }
    let (dest_need, temp_need) = space_needed(offer, folder, same_volume);
    let mut checks = vec![(&volumes[0], dest_need)];
    if !same_volume {
        checks.push((&volumes[1], temp_need));
    }
    let short = checks.iter().find(|((_, free), need)| need > free);
    let ((root, free), needed) = short.copied().unwrap_or(checks[0]);
    let args = (format_size(needed), format_size(*free), drive_name(root));
    let mut text = if short.is_some() {
        os_tr!(space_short, needed = args.0, free = args.1, drive = args.2)
    } else {
        os_tr!(space_ok, needed = args.0, free = args.1, drive = args.2)
    };
    if offer.kind == Kind::Folder && folder != ReceivedFolder::KeepZip {
        text = format!("{text} {}", tr!(space_folder_note));
    }
    Space {
        enough: Some(short.is_none()),
        fits: true,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(kind: Kind) -> Offer {
        Offer {
            kind,
            name: "x".into(),
            size: 100,
            file_count: 3,
            content_size: 150,
            largest_file: 150,
            sender_name: String::new(),
            token: [0; 16],
            modified: None,
            created: None,
        }
    }

    #[test]
    fn needed_space() {
        use ReceivedFolder::*;
        let (file, folder) = (offer(Kind::File), offer(Kind::Folder));
        for mode in ReceivedFolder::ALL {
            assert_eq!(space_needed(&file, mode, true), (100, 0));
            assert_eq!(space_needed(&file, mode, false), (100, 0));
        }
        // The zip and the unpacked folder exist at the same time; the zip
        // that is only unpacked is in the temporary folder.
        assert_eq!(space_needed(&folder, Extract, true), (250, 0));
        assert_eq!(space_needed(&folder, Extract, false), (150, 100));
        // A zip that is kept arrives in the destination.
        assert_eq!(space_needed(&folder, KeepZip, false), (100, 0));
        assert_eq!(space_needed(&folder, Both, false), (250, 0));
    }

    /// What has to fit on a file system that limits the size of a file: the
    /// file, or the zip of a folder where it is kept, and the largest file
    /// of a folder where it is unpacked.
    #[test]
    fn largest_saved_files() {
        use ReceivedFolder::*;
        let (file, mut folder) = (offer(Kind::File), offer(Kind::Folder));
        folder.largest_file = 80;
        for mode in ReceivedFolder::ALL {
            assert_eq!(largest_saved_file(&file, mode), 100);
        }
        assert_eq!(largest_saved_file(&folder, Extract), 80);
        assert_eq!(largest_saved_file(&folder, KeepZip), 100);
        assert_eq!(largest_saved_file(&folder, Both), 100);
        folder.largest_file = 120;
        assert_eq!(largest_saved_file(&folder, Both), 120);
    }

    /// On FAT32, a file of 4 GiB or more does not fit, however much room
    /// there is: the text says so, and the transfer is not accepted.
    #[test]
    fn a_file_too_large_for_fat32_does_not_fit() {
        let dir = std::env::temp_dir();
        let mut big = offer(Kind::File);
        big.size = FAT32_MAX_FILE;
        crate::archive::PRETEND_FAT.set(true);
        assert!(check_space(&big, &dir, &dir, ReceivedFolder::Extract).fits);
        big.size += 1;
        let space = check_space(&big, &dir, &dir, ReceivedFolder::Extract);
        crate::archive::PRETEND_FAT.set(false);
        assert!(!space.fits);
        assert!(space.text.contains("FAT"), "{}", space.text);
        assert!(check_space(&big, &dir, &dir, ReceivedFolder::Extract).fits);
    }

    #[cfg(windows)]
    #[test]
    fn drive_names() {
        assert_eq!(drive_name(Path::new(r"C:\")), "C:");
        assert_eq!(
            drive_name(Path::new(r"\\server\share\")),
            r"\\server\share\"
        );
    }
}
