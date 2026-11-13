//! Whether an offer fits on the disk when saved into a folder: what the
//! Incoming Transfer window says, and what automatic acceptance goes by.

use std::path::{Path, PathBuf};

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
    /// On Windows and on a Mac: says that the transfer would keep an archive
    /// in the folder for temporary files that the file system there cannot
    /// hold (FAT32), so that it would fail unless the user chooses another
    /// folder for them. Whether the user wants to be told is the caller's to
    /// see to (`Config::warn_fat_temp`).
    pub temp_warning: Option<String>,
}

/// Whether the volume at `root` limits a file to less than 4 GiB (FAT32, and
/// FAT).
fn limits_files(root: &Path) -> bool {
    matches!(
        crate::archive::file_system(root).as_deref(),
        Some("FAT32" | "FAT")
    )
}

/// What to tell the user before the folder at `path` is sent with `temp` as
/// the folder for temporary files: that the volume on which it would be
/// packed (see `send::pack_in`) cannot hold an archive as large as the
/// folder's files are together. Nothing for a file, which is sent from where
/// it is, and nothing on Android, where the app's own storage holds the
/// archives. It reads the whole folder's listing, but only where one of the
/// two volumes limits its files. Blocking.
#[cfg(any(windows, target_os = "macos"))]
pub fn send_temp_warning(path: &Path, temp: &Path) -> Option<String> {
    use crate::net::send::{PackIn, pack_in};
    if crate::source::is_document(path) || !path.is_dir() {
        return None;
    }
    // The temporary folder may not be there yet: its drive is that of the
    // nearest folder above it that is.
    let temp = temp.ancestors().find_map(disk_free);
    let beside = path.parent().and_then(disk_free);
    let limited = |volume: &Option<(std::path::PathBuf, u64)>| {
        volume.as_ref().is_some_and(|(root, _)| limits_files(root))
    };
    if !limited(&temp) && !limited(&beside) {
        return None;
    }
    let size = crate::archive::folder_size(path).ok()?;
    if size <= FAT32_MAX_FILE {
        return None;
    }
    let volume = match pack_in(size, temp.as_ref(), beside.as_ref()) {
        PackIn::Temp => temp,
        PackIn::Beside => beside,
    };
    let (root, _) = volume.filter(|(root, _)| limits_files(root))?;
    let item = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    Some(os_tr!(
        temp_fat_send,
        item = item,
        drive = drive_name(&root),
        size = format_size(size)
    ))
}

/// What to tell the user before offers are accepted of which `largest` bytes
/// is the largest archive that arrives in the folder for temporary files, on
/// the volume at `root`.
#[cfg(any(windows, target_os = "macos"))]
fn receive_temp_warning(root: &Path, largest: u64) -> Option<String> {
    (largest > FAT32_MAX_FILE && limits_files(root)).then(|| {
        os_tr!(
            temp_fat_receive,
            drive = drive_name(root),
            size = format_size(largest)
        )
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn receive_temp_warning(_root: &Path, _largest: u64) -> Option<String> {
    None
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

/// A receive that is under way, as far as the room that it still takes goes:
/// what has not arrived yet is not on the disk, so the free space does not
/// show it, while a new item needs room beside it (see [`check_space_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arriving {
    pub offer: Offer,
    /// The folder that it is saved into.
    pub dir: PathBuf,
    /// What is kept of a folder.
    pub folder: ReceivedFolder,
    /// How far it has got: the bytes of its data that arrived, or once it is
    /// `unpacking`, the bytes that were unpacked.
    pub done: u64,
    pub unpacking: bool,
}

impl Arriving {
    /// How many bytes it still writes to the volume at `root`, with `temp`
    /// as the folder of the temporary archives. Rather too many than too
    /// few: while a part is checked after a restart, `done` counts what was
    /// checked.
    fn still_needs(&self, root: &Path, temp: &Path) -> u64 {
        let same = |a: &Path, b: &Path| a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
        let Some((dest_root, _)) = disk_free(&self.dir) else {
            return 0;
        };
        let offer = &self.offer;
        // The data: the file, or the zip of a folder, which is in the
        // temporary folder where the destination cannot hold it.
        let in_temp = session::part_in_temp(offer.kind, self.folder, offer.size, &self.dir);
        let data_root = in_temp
            .then(|| disk_free(temp))
            .flatten()
            .map_or_else(|| dest_root.clone(), |(root, _)| root);
        let data = if self.unpacking {
            0
        } else {
            offer.size.saturating_sub(self.done)
        };
        // What a folder unpacks to, next to its zip.
        let unpacks = offer.kind == Kind::Folder && self.folder != ReceivedFolder::KeepZip;
        let files = match (unpacks, self.unpacking) {
            (false, _) => 0,
            (true, false) => offer.content_size,
            (true, true) => offer.content_size.saturating_sub(self.done),
        };
        let mut needed = 0u64;
        if same(&data_root, root) {
            needed = needed.saturating_add(data);
        }
        if same(&dest_root, root) {
            needed = needed.saturating_add(files);
        }
        needed
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
/// folder and `temp` being the folder of the temporary archives, beside
/// what the receives that are under way (`arriving`) still have to write.
pub fn check_space(
    offer: &Offer,
    dir: &Path,
    temp: &Path,
    folder: ReceivedFolder,
    arriving: &[Arriving],
) -> Space {
    check_space_of(&[offer], dir, temp, folder, arriving)
}

/// The same for several offers that are answered together: all of them have
/// to fit at the same time, together with the rest of the receives that are
/// under way, whose room the free space still counts as free.
pub fn check_space_of(
    offers: &[&Offer],
    dir: &Path,
    temp: &Path,
    folder: ReceivedFolder,
    arriving: &[Arriving],
) -> Space {
    let unknown = || Space {
        enough: None,
        fits: true,
        text: tr!(space_unknown).to_string(),
        temp_warning: None,
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
    if largest > FAT32_MAX_FILE && limits_files(&dest_root) {
        return Space {
            enough: None,
            fits: false,
            text: os_tr!(
                space_fat,
                drive = drive_name(&dest_root),
                size = format_size(largest)
            ),
            temp_warning: None,
        };
    }
    // The zip that the destination cannot hold arrives in the temporary
    // folder, whose file system may not hold it either.
    let temp_warning = volumes.get(1).and_then(|(root, _)| {
        let largest = offers
            .iter()
            .filter(|offer| in_temp(offer))
            .map(|offer| offer.size)
            .max()
            .unwrap_or(0);
        receive_temp_warning(root, largest)
    });
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
    // What the receives under way still write to a volume is not free for
    // these.
    let taken = |root: &Path| {
        arriving
            .iter()
            .map(|receive| receive.still_needs(root, temp))
            .fold(0u64, u64::saturating_add)
    };
    let short = checks
        .iter()
        .find(|((root, free), need)| *need > free.saturating_sub(taken(root)));
    let ((root, free), needed) = short.copied().unwrap_or(checks[0]);
    let args = (format_size(needed), format_size(*free), drive_name(root));
    let mut text = if short.is_some() {
        os_tr!(space_short, needed = args.0, free = args.1, drive = args.2)
    } else {
        os_tr!(space_ok, needed = args.0, free = args.1, drive = args.2)
    };
    // The numbers are the system's, so what makes them too little is said.
    if short.is_some() && taken(root) > 0 {
        let size = format_size(taken(root));
        text = format!("{text} {}", tr!(space_arriving_note, size = size));
    }
    if any_folder && folder != ReceivedFolder::KeepZip {
        text = format!("{text} {}", tr!(space_folder_note));
    }
    Space {
        enough: Some(short.is_none()),
        fits: true,
        text,
        temp_warning,
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
        assert!(check_space(&big, &dir, &dir, ReceivedFolder::Extract, &[]).fits);
        big.size += 1;
        let space = check_space(&big, &dir, &dir, ReceivedFolder::Extract, &[]);
        crate::archive::PRETEND_FAT.set(false);
        assert!(!space.fits);
        assert!(space.text.contains("FAT"), "{}", space.text);
        assert!(check_space(&big, &dir, &dir, ReceivedFolder::Extract, &[]).fits);
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
        let one = check_space_of(&[&half], &dir, &dir, ReceivedFolder::Extract, &[]);
        assert_eq!(one.enough, Some(true), "{}", one.text);
        let two = check_space_of(&[&half, &half], &dir, &dir, ReceivedFolder::Extract, &[]);
        assert_eq!(two.enough, Some(false), "{}", two.text);

        let small = offer(Kind::File);
        let mut big = offer(Kind::File);
        big.size = FAT32_MAX_FILE + 1;
        crate::archive::PRETEND_FAT.set(true);
        let together = check_space_of(&[&small, &big], &dir, &dir, ReceivedFolder::Extract, &[]);
        crate::archive::PRETEND_FAT.set(false);
        assert!(!together.fits);
    }

    /// An item has to fit beside what the receives under way still have to
    /// write, which the free space does not show yet: the text then says
    /// what takes the room. What has arrived already is not counted twice.
    #[test]
    fn receives_under_way_take_their_room() {
        let dir = std::env::temp_dir();
        let free = disk_free(&dir).expect("free space").1;
        let mut half = offer(Kind::File);
        // With a margin, as other programs change the free space meanwhile.
        half.size = free / 2 + free / 50;
        let under_way = |done| Arriving {
            offer: half.clone(),
            dir: dir.clone(),
            folder: ReceivedFolder::Extract,
            done,
            unpacking: false,
        };
        let check = |arriving: &[Arriving]| {
            check_space(&half, &dir, &dir, ReceivedFolder::Extract, arriving)
        };
        assert_eq!(check(&[]).enough, Some(true));
        let beside = check(&[under_way(0)]);
        assert_eq!(beside.enough, Some(false), "{}", beside.text);
        let note = tr!(space_arriving_note, size = format_size(half.size));
        assert!(beside.text.ends_with(&note), "{}", beside.text);
        // All of it is on the disk: the free space has counted it.
        let arrived = check(&[under_way(half.size)]);
        assert_eq!(arrived.enough, Some(true), "{}", arrived.text);
        assert!(!arrived.text.contains(&note), "{}", arrived.text);
    }

    /// What a receive under way still writes: the rest of its data, and for
    /// a folder that is unpacked all of its files, until they are unpacked.
    #[test]
    fn what_a_receive_still_writes() {
        let dir = std::env::temp_dir();
        let root = disk_free(&dir).expect("free space").0;
        let needs = |kind, folder, done, unpacking| {
            Arriving {
                offer: offer(kind),
                dir: dir.clone(),
                folder,
                done,
                unpacking,
            }
            .still_needs(&root, &dir)
        };
        use ReceivedFolder::*;
        assert_eq!(needs(Kind::File, Extract, 0, false), 100);
        assert_eq!(needs(Kind::File, Extract, 40, false), 60);
        assert_eq!(needs(Kind::File, Extract, 500, false), 0);
        assert_eq!(needs(Kind::Folder, KeepZip, 40, false), 60);
        for mode in [Extract, Both] {
            assert_eq!(needs(Kind::Folder, mode, 40, false), 60 + 150);
            assert_eq!(needs(Kind::Folder, mode, 100, false), 150);
            assert_eq!(needs(Kind::Folder, mode, 50, true), 100);
            assert_eq!(needs(Kind::Folder, mode, 150, true), 0);
        }
        // Another volume has nothing to do with it.
        let elsewhere = Arriving {
            offer: offer(Kind::File),
            dir: dir.clone(),
            folder: Extract,
            done: 0,
            unpacking: false,
        };
        assert_eq!(elsewhere.still_needs(Path::new("/no/such/volume"), &dir), 0);
    }

    /// A zip that a FAT32 destination cannot hold arrives in the folder for
    /// temporary files, and the user is told when that is on FAT32 as well.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn an_archive_too_large_for_the_temporary_folder_is_told() {
        let dir = std::env::temp_dir();
        let mut big = offer(Kind::Folder);
        big.size = FAT32_MAX_FILE + 1;
        big.largest_file = 10;
        let plain = check_space(&big, &dir, &dir, ReceivedFolder::Extract, &[]);
        assert!(plain.fits && plain.temp_warning.is_none());
        crate::archive::PRETEND_FAT.set(true);
        let unpacked = check_space(&big, &dir, &dir, ReceivedFolder::Extract, &[]);
        // Kept as a zip, it does not fit in the destination at all.
        let kept = check_space(&big, &dir, &dir, ReceivedFolder::KeepZip, &[]);
        big.size = FAT32_MAX_FILE;
        let small = check_space(&big, &dir, &dir, ReceivedFolder::Extract, &[]);
        crate::archive::PRETEND_FAT.set(false);
        assert!(unpacked.fits);
        let warning = unpacked.temp_warning.expect("a warning");
        assert!(warning.contains("FAT"), "{warning}");
        assert!(!kept.fits && kept.temp_warning.is_none());
        assert!(small.fits && small.temp_warning.is_none());
    }

    /// A file is sent from where it is, and a small folder's archive fits
    /// anywhere: neither is worth a word, on FAT32 or not.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn a_small_folder_and_a_file_send_without_a_warning() {
        let dir = std::env::temp_dir().join(format!("dodavo-space-{}", std::process::id()));
        let folder = dir.join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(send_temp_warning(&folder, &dir), None);
        crate::archive::PRETEND_FAT.set(true);
        let on_fat = [
            send_temp_warning(&folder, &dir),
            send_temp_warning(&file, &dir),
        ];
        crate::archive::PRETEND_FAT.set(false);
        assert_eq!(on_fat, [None, None]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A folder whose archive can be too large for the drive it is packed
    /// on is told of before it is sent, by its name.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn a_folder_too_large_for_fat32_is_told_before_it_is_sent() {
        let dir = std::env::temp_dir().join(format!("dodavo-space-big-{}", std::process::id()));
        let folder = dir.join("Large folder");
        std::fs::create_dir_all(&folder).unwrap();
        // A file of that length without its data, where there is room for it.
        let made = std::fs::File::create(folder.join("big.bin"))
            .and_then(|file| file.set_len(FAT32_MAX_FILE + 1));
        if made.is_ok() {
            assert_eq!(send_temp_warning(&folder, &dir), None);
            crate::archive::PRETEND_FAT.set(true);
            let warning = send_temp_warning(&folder, &dir);
            crate::archive::PRETEND_FAT.set(false);
            let warning = warning.expect("a warning");
            assert!(warning.contains("Large folder"), "{warning}");
            assert!(warning.contains("FAT"), "{warning}");
        }
        std::fs::remove_dir_all(&dir).ok();
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
