# Known issues

What is wrong in Dodavo now, most important first. Work that is not a defect is in [TODO.md](TODO.md).

- **High:** loses data, makes the two sides disagree about a transfer, or stops normal use.
- **Medium:** many users will notice it, or its cause is not known yet.
- **Low:** rare or cosmetic.

Remove an item in the commit that fixes it. This file is public, so keep secrets and private addresses or paths out of it, and write plain tasks and facts.

## High

None known.

## Medium

### Windows warns about a new download

SmartScreen shows "Windows protected your PC" on a new download, and Windows 7 asks about an unknown publisher. Automatic updates are not affected. A code-signing certificate ends this: `installer\build.ps1` signs everything once one is configured (`DODAVO_SIGN_THUMBPRINT` or `DODAVO_SIGN_PFX`).

### Transfers through a relay are slow

Without a direct path, data goes through number0's free public relays. A relay-only test of 500 MB ran at a flat 0.96 MB/s, about 18 minutes per GB, which looks like a rate limit (2026-11-01; both sides were on one PC, so its upload was also in the path). The fix is our own `iroh-relay`, configured in `bind()` in `src/net/mod.rs`.

### Nothing explains why peers on different protocol versions cannot connect

After a change of the ALPN, the two sides cannot connect, and the user only sees iroh's own English text (the last case of `net::connect_error_text`). Decide how a newer version handles this before the first change of the ALPN. A new release cannot change what 1.0.0 itself shows.

### An offer was reported as declined although nobody answered (not explained)

Seen once on 2026-11-01: an offer from a contact waited in the Incoming Transfer window, because there was not enough space, and within 5 minutes the sender was told that the recipient had declined. A repeat that was watched for 5.5 minutes stayed open as it should. `receive::handle` sends `Decline` when the user answers no, when the answer's sender is dropped, and on a cancel; which of these it was is not known.

### Free space did not come back after a failed transfer on a full disk (not explained)

Seen once on 2026-11-01: after a 500 MB transfer failed on a 200 MB NTFS volume, the volume stayed at 204 KB free although no files were left on it. FAT32 and exFAT volumes got all their space back. It may be a deleted part that another program, such as Defender or the indexer, still held open; that was not checked.

### In dark mode on Windows 11, the labels of check boxes are hard to read

The labels of check boxes are drawn in grey on the dark background, with less contrast than the other texts of the same window (Settings, the Incoming Transfer window), as seen on Windows 11 build 26300. `theme::theme_child` gives them the classic look so that they take the text color of `WM_CTLCOLORSTATIC`, which they do not there. Light mode and high contrast are not affected.

### On a Mac, several offers from one person open a window each, on top of each other

Each offer gets its own `IncomingWindowController`, centred on the same spot, so the ones behind are easily missed and their transfers wait. Windows puts them in one window with one answer (`incoming::open_or_add`). Each window also speaks of its one item only, so the line that counts the items over the size limit (`limit::notice`) never shows on a Mac.

## Low

- **A transfer cancelled by a change of the Contact ID stays waiting on a side that was not connected.** Change My Contact ID cancels every transfer, which the other side learns only if it is connected at that moment. One that is not goes on asking the old Contact ID, which nobody answers any more, and shows its transfer as waiting for the connection until the user cancels it or it expires after 30 days. A tombstone cannot answer either, since the old identity is gone.

### On Android, a file that another app saves under the name of a waiting part can be taken for it

Android tells Dodavo no birth time of a file before Android 11, nor on its shared storage (`FileId::weak`), and ext4 gives the number of a deleted file to the next one at once. So a part file that the user deletes while its transfer waits, followed by another file of the same name, looks like the part to the record. Every receive has its part in `Download/Dodavo`, also a folder that is only unpacked (`<name>.zip.part`). Dodavo's own new parts and folders leave the names of records alone (`session::held_names`), and a file shorter than what reached the disk is not taken for the part (`TransferRecord::part_shorter`); a longer one that another app saves into `Download/Dodavo` under that name is. `FS_IOC_GETVERSION` (the inode generation) would tell them apart on ext4 where the app may ask for it.

### Without a size limit, a folder can unpack to more than its offer says

The data that arrives is held to the size in the offer (`Receiver::read_frames`), and with a size limit for the sender, what a folder's zip unpacks to is held to the larger of the offer and the limit (`Receiver::unpack_max`). Without a limit nothing holds it, so a sender whose program was changed can get a folder past the check of the free space, which goes by the offer; the transfer then fails when the disk is full, and its folder is removed. A sender's own count can differ from what it packs when a file grows meanwhile, so the two cannot simply be required to match.

### The size limit counts each item alone

Each item is its own transfer, so several items that are each within the limit can together be larger than it. A limit on what one sender may send in total would have to count what is waiting and under way from them.

### Automatic acceptance checks the free space of each item alone

Each offer from a recipient is checked by itself (`Event::Offer` in `src/app.rs`), so several large items that fit one by one all start, and the later ones fail once the disk is full. The Incoming Transfer window counts them together. The check would have to include what is still to arrive for the transfers already running.

### An all-users uninstall leaves other accounts' entries in File Explorer's menus

Each account writes its own "Send with Dodavo" entry and Send to shortcut when Dodavo starts there, and the uninstaller reaches only the account that runs it. In the other accounts both stay, and fail when used, until they are deleted by hand or Dodavo is installed again. The same goes for their "Start with Windows" value.

### Between connections, nothing keeps a part as it was on Windows

A share mode holds the part only during a connection, while `have` and the hash in memory are trusted across connections. A program that writes to the part in between is noticed only by the check at the end, which fails the transfer. A Mac hashes the part from the disk at the end anyway.

### A file made under the part's name on FAT can be taken for the part

On FAT32 and exFAT, a part is recognized by its creation time, and Windows gives a new file the creation time of a file of that name deleted within about 15 seconds ("tunnelling"). `TransferRecord::part_shorter` catches a new file shorter than what was received, but not one of that length or longer. Such a file would be written to and then deleted, as was seen with a shorter one before that check. It takes a part being deleted and a new file of exactly its name appearing within seconds while Dodavo is not running.

### The Android app carries two libraries that it does not use

`iroh` and `iroh-relay` are also built as libraries of their own (`cdylib`), and cargo-ndk copies every `.so` it finds, so `libiroh-….so` and `libiroh_relay-….so` go into the app beside `libdodavo.so`, which does not need them: about 1.3 MB for each ABI. `buildRust` in `android/app/build.gradle.kts` would have to keep only `libdodavo.so`.

### A finished transfer can show the progress it had at its start

A progress window that is busy (a question is open) drops the reports of progress. At the end it shows the whole size, but only if it ever learned the size: one that was busy from its first report on keeps its first text beside "Done!".

### Some error details stay in English

Most failure texts are translated (`net::failure_text`), but these are not: other errors of a dial (the last case of `net::connect_error_text`), network errors of the update check (`update::http`), and a failure to start networking (`Event::BindFailed`). They appear in every language.

