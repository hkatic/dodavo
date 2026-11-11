//! What the promises of `archive::macos` come to on real volumes: disk images
//! of each file system, made by `hdiutil`, which needs no special rights. The
//! tests with `PRETEND_FAT` cannot tell how a file system really behaves.

use std::{
    fs::File,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

use super::*;

/// A disk image of the file system `fs`, attached for as long as it lives.
struct Volume {
    image: PathBuf,
    mount: PathBuf,
}

impl Volume {
    fn new(fs: &str) -> Volume {
        let dir = std::env::temp_dir().join(format!(
            "dodavo-volume-{}-{}",
            fs.replace(' ', "-"),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("volume.dmg");
        let made = Command::new("hdiutil")
            .args([
                "create",
                "-quiet",
                "-size",
                "64m",
                "-volname",
                "DODAVOTEST",
                "-fs",
                fs,
            ])
            .arg(&image)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let mount = dir.join("mount");
        std::fs::create_dir(&mount).unwrap();
        let volume = Volume { image, mount };
        volume.attach();
        volume
    }

    fn attach(&self) {
        let attached = Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&self.mount)
            .arg(&self.image)
            .output()
            .unwrap();
        assert!(attached.status.success(), "{attached:?}");
    }

    /// Something may still be looking at a new volume, such as Spotlight, and then
    /// `hdiutil` says for a moment that it is busy. What keeps it for half a
    /// minute is not the test, whose files are closed by now: the volume is
    /// then taken from it, and what the test goes on to read after mounting it
    /// again shows that nothing was lost.
    fn detach(&self) {
        let detach = |force: bool| {
            let mut hdiutil = Command::new("hdiutil");
            hdiutil.args(["detach", "-quiet"]);
            if force {
                hdiutil.arg("-force");
            }
            hdiutil.arg(&self.mount).output().unwrap()
        };
        for _ in 0..30 {
            if detach(false).status.success() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let detached = detach(true);
        assert!(detached.status.success(), "{detached:?}");
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mount)
            .output();
        if let Some(dir) = self.image.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Everything a receive does with its files, on one volume.
fn receive_on(fs: &str, name: &str, by_number: bool, by_time: bool) {
    let volume = Volume::new(fs);
    let root = volume.mount.clone();
    assert_eq!(file_system(&root).as_deref(), Some(name));

    // A part, taken without replacing anything, and marked.
    std::fs::write(root.join("a.txt"), b"theirs").unwrap();
    let part = create_unique_file(&root, "a.txt.part").unwrap();
    assert_eq!(part, root.join("a.txt.part"));
    let _ = mark_of_the_web(&part);
    // What FAT and exFAT keep of it goes into a file beside this one.
    let id = created_now(&part).unwrap();
    assert_eq!(id.numbered(), by_number, "{fs}");
    assert_eq!(id.by_creation_time(), by_time, "{fs}");
    assert!(id.same_item(&file_id(&part).unwrap()), "{fs}");

    // A connection appends, and syncs as a receive does.
    let mut out = connection_options().open(&part).unwrap();
    assert!(id.same_item(&FileId::of(&out).unwrap()), "{fs}");
    out.write_all(b"data").unwrap();
    match out.sync_data() {
        Err(e) if full_sync_refused(&e) => sync_as_far_as_possible(&out).unwrap(),
        done => done.unwrap(),
    }
    drop(out);

    // A file made under the name of one deleted before is not that one,
    // where the identity can tell at all.
    let other = root.join("other.bin");
    std::fs::write(&other, b"x").unwrap();
    let other_id = file_id(&other).unwrap();
    std::fs::remove_file(&other).unwrap();
    // FAT32 here reads creation times in whole seconds.
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    std::fs::write(&other, b"y").unwrap();
    if by_time {
        assert!(!other_id.same_item(&file_id(&other).unwrap()), "{fs}");
    }
    std::fs::remove_file(&other).unwrap();

    // The last step: renamed while open, never over what is there.
    let open = last_step_options(true).open(&part).unwrap();
    let e = rename_open(&open, &root.join("a.txt")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{fs}: {e}");
    let moved = move_open_to_unique_noted(&open, &root, "a.txt", &mut |_| Ok(())).unwrap();
    assert_eq!(moved, root.join("a (2).txt"));
    assert!(!part.exists(), "{fs}");
    assert!(id.same_item(&FileId::of(&open).unwrap()), "{fs}");
    assert!(id.same_item(&file_id(&moved).unwrap()), "{fs}");
    keep_creation_time(&open, &id).unwrap();
    drop(open);
    assert_eq!(std::fs::read(&moved).unwrap(), b"data");
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"theirs");

    // Dates, where the file system keeps them.
    let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
    let times = ItemTimes {
        modified: system_to_filetime(at),
        accessed: None,
        created: system_to_filetime(at),
    };
    let (now, _) = times.split(Some(&id));
    now.apply(&moved);
    let meta = moved.symlink_metadata().unwrap();
    assert_eq!(meta.modified().unwrap(), at, "{fs}");
    if !by_time {
        assert_eq!(meta.created().unwrap(), at, "{fs}");
    }
    if by_time {
        assert!(
            id.same_item(&file_id(&moved).unwrap()),
            "{fs}: created kept"
        );
    }

    // A folder with a date, and a hidden item, unpacked.
    let src = root.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("sub/b.txt"), b"bravo").unwrap();
    std::fs::write(src.join("c.txt"), b"charlie").unwrap();
    let zip = root.join("x.zip");
    let cancel = AtomicBool::new(false);
    zip_folder(&src, &zip, &cancel, &mut |_, _| {}).unwrap();
    let mut read = File::open(&zip).unwrap();
    let mut head = [0; 2];
    read.read_exact(&mut head).unwrap();
    read.rewind().unwrap();
    assert_eq!(&head, b"PK");
    let dest = create_unique_dir(&root, "Unpacked", |_| true).unwrap();
    unzip(read, &dest, true, &cancel, &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(dest.join("sub/b.txt")).unwrap(), b"bravo");
    assert_eq!(std::fs::read(dest.join("c.txt")).unwrap(), b"charlie");
    hide(&dest.join("c.txt")).unwrap();
    assert!(testing::hidden(&dest.join("c.txt")), "{fs}");

    // The identity survives the volume being mounted again.
    let kept = moved.clone();
    let before = file_id(&kept).unwrap();
    volume.detach();
    volume.attach();
    let after = file_id(&kept).unwrap();
    assert!(before.same_item(&after), "{fs}: {before:?} {after:?}");
    assert_eq!(std::fs::read(&kept).unwrap(), b"data");

    // Deleting is by name, whoever has the file open.
    let open = File::open(&kept).unwrap();
    std::fs::remove_file(&kept).unwrap();
    assert!(!name_taken(&kept), "{fs}");
    assert_eq!(
        rename_open(&open, &root.join("back.txt"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
        "{fs}"
    );
    assert!(free_space_of(&root).is_some());
}

fn free_space_of(root: &Path) -> Option<u64> {
    crate::platform::disk_free(root).map(|(_, free)| free)
}

#[test]
fn on_apfs() {
    receive_on("APFS", "APFS", true, false);
}

#[test]
fn on_hfs_plus() {
    receive_on("HFS+", "HFS+", true, false);
}

#[test]
fn on_fat32() {
    receive_on("MS-DOS FAT32", "FAT32", false, true);
}

#[test]
fn on_exfat() {
    receive_on("ExFAT", "exFAT", false, true);
}
