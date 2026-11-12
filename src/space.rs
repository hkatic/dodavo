//! Whether an offer fits on the disk when saved into a folder: what the
//! Incoming Transfer window says, and what automatic acceptance goes by.

use std::path::Path;

use crate::{
    format::format_size,
    i18n::{os_tr, tr},
    net::{
        Kind, Offer, ReceivedFolder,
        session::{self, FAT32_MAX_FILE},
    },
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

/// The largest single file a transfer of `offer` has to save in the
/// destination folder: the file, the zip of a folder where it is kept, and
/// the largest file of a folder where it is unpacked. (The zip that is only
/// unpacked arrives there too, but in the temporary folder where the
/// destination cannot hold it: see `session::part_in_temp`.)
fn largest_saved_file(offer: &Offer, folder: ReceivedFolder) -> u64 {
    match (offer.kind, folder) {
        (Kind::File, _) | (Kind::Folder, ReceivedFolder::KeepZip) => offer.size,
        (Kind::Folder, ReceivedFolder::Both) => offer.size.max(offer.largest_file),
        (Kind::Folder, ReceivedFolder::Extract) => offer.largest_file,
    }
}

/// Bytes needed on the destination drive and on the drive of the temporary
/// folder. Everything arrives in the destination: a folder that is unpacked
/// as a zip, which is then unpacked next to it, so both need room at the
/// same time. `in_temp` says that the zip arrives in the temporary folder
/// (see `session::part_in_temp`), which may be on another drive
/// (`same_volume`).
fn space_needed(
    offer: &Offer,
    folder: ReceivedFolder,
    in_temp: bool,
    same_volume: bool,
) -> (u64, u64) {
    match (offer.kind, folder) {
        (Kind::File, _) | (Kind::Folder, ReceivedFolder::KeepZip) => (offer.size, 0),
        (Kind::Folder, ReceivedFolder::Extract) if in_temp && !same_volume => {
            (offer.content_size, offer.size)
        }
        (Kind::Folder, ReceivedFolder::Both | ReceivedFolder::Extract) => {
            (offer.size.saturating_add(offer.content_size), 0)
        }
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
    check_space_of(&[offer], dir, temp, folder)
}

/// The same for several offers that are answered together: all of them have
/// to fit at the same time.
pub fn check_space_of(offers: &[&Offer], dir: &Path, temp: &Path, folder: ReceivedFolder) -> Space {
    let unknown = || Space {
        enough: None,
        fits: true,
        text: tr!(space_unknown).to_string(),
    };
    let Some((dest_root, dest_free)) = disk_free(dir) else {
        return unknown();
    };
    let mut volumes = vec![(dest_root.clone(), dest_free)];
    let any_folder = offers.iter().any(|offer| offer.kind == Kind::Folder);
    let in_temp = |offer: &Offer| session::part_in_temp(offer.kind, folder, offer.size, dir);
    let uses_temp = offers.iter().any(|offer| in_temp(offer));
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
    let largest = offers
        .iter()
        .map(|offer| largest_saved_file(offer, folder))
        .max()
        .unwrap_or(0);
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
    let (dest_need, temp_need) = offers
        .iter()
        .map(|offer| space_needed(offer, folder, in_temp(offer), same_volume))
        .fold((0u64, 0u64), |(dest, temp), (more_dest, more_temp)| {
            (
                dest.saturating_add(more_dest),
                temp.saturating_add(more_temp),
            )
        });
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
    if any_folder && folder != ReceivedFolder::KeepZip {
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
            assert_eq!(space_needed(&file, mode, false, true), (100, 0));
            assert_eq!(space_needed(&file, mode, false, false), (100, 0));
        }
        // The zip and the unpacked folder exist at the same time, both in
        // the destination, wherever the temporary folder is.
        assert_eq!(space_needed(&folder, Extract, false, true), (250, 0));
        assert_eq!(space_needed(&folder, Extract, false, false), (250, 0));
        // Only a zip that the destination cannot hold is in the temporary
        // folder, which counts when that is another drive.
        assert_eq!(space_needed(&folder, Extract, true, true), (250, 0));
        assert_eq!(space_needed(&folder, Extract, true, false), (150, 100));
        assert_eq!(space_needed(&folder, KeepZip, false, false), (100, 0));
        assert_eq!(space_needed(&folder, Both, false, false), (250, 0));
    }

    /// The zip of a folder that is only unpacked goes to the temporary
    /// folder only where the destination cannot hold a file that large.
    #[test]
    fn only_a_zip_too_large_for_fat32_is_in_the_temporary_folder() {
        use ReceivedFolder::*;
        let dir = std::env::temp_dir();
        let in_temp = |kind, folder, size| session::part_in_temp(kind, folder, size, &dir);
        let big = FAT32_MAX_FILE + 1;
        assert!(!in_temp(Kind::Folder, Extract, big));
        crate::archive::PRETEND_FAT.set(true);
        let on_fat = [
            in_temp(Kind::Folder, Extract, big),
            in_temp(Kind::Folder, Extract, FAT32_MAX_FILE),
            in_temp(Kind::Folder, KeepZip, big),
            in_temp(Kind::Folder, Both, big),
            in_temp(Kind::File, Extract, big),
        ];
        crate::archive::PRETEND_FAT.set(false);
        assert_eq!(on_fat, [true, false, false, false, false]);
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

    /// Offers answered together have to fit together, and the largest file of
    /// any of them has to fit on the file system.
    #[test]
    fn several_offers_count_together() {
        let dir = std::env::temp_dir();
        let free = disk_free(&dir).expect("free space").1;
        let mut half = offer(Kind::File);
        // With a margin, as other programs change the free space meanwhile.
        half.size = free / 2 + free / 50;
        let one = check_space_of(&[&half], &dir, &dir, ReceivedFolder::Extract);
        assert_eq!(one.enough, Some(true), "{}", one.text);
        let two = check_space_of(&[&half, &half], &dir, &dir, ReceivedFolder::Extract);
        assert_eq!(two.enough, Some(false), "{}", two.text);

        let small = offer(Kind::File);
        let mut big = offer(Kind::File);
        big.size = FAT32_MAX_FILE + 1;
        crate::archive::PRETEND_FAT.set(true);
        let together = check_space_of(&[&small, &big], &dir, &dir, ReceivedFolder::Extract);
        crate::archive::PRETEND_FAT.set(false);
        assert!(!together.fits);
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
