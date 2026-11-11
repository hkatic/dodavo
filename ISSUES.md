# Known issues

What is wrong in Dodavo now, most important first. Work that is not a defect is in [TODO.md](TODO.md).

- **High:** loses data, makes the two sides disagree about a transfer, or stops normal use.
- **Medium:** many users will notice it, or its cause is not known yet.
- **Low:** rare or cosmetic.

Remove an item in the commit that fixes it. This file is public, so keep secrets and private addresses or paths out of it, and write plain tasks and facts.

## High

None known.

## Medium

### Three network tests time out on GitHub's Mac runner (not explained)

`a_cancel_waits_for_the_verdict_across_a_restart`, `a_send_needs_its_data_again_when_the_receiver_lacks_it` and `finish_alone_after_restart_during_unpacking` reconnect after a restart, and wait more than 120 s there, while they pass on a Mac elsewhere and on the Windows runner. It may be that the runner keeps an unsigned test program from the local network (Local Network privacy), which would leave a slow way through a relay; that was not checked. If so, the same wait could be what a user who denies that permission sees. CI runs them without letting them fail the run.

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

### A dialog in one transfer's window holds up the events of all of them

`app::process_events` stops at the first event whose window is busy and tries again every 250 ms. So while a question is open in one progress window (closing it, or a lost connection with "Ask every time"), an event for that window holds back the offers and the progress of all the others. Only that transfer's events should wait, in their order.

### On a Mac, several offers from one person open a window each, on top of each other

Each offer gets its own `IncomingWindowController`, centred on the same spot, so the ones behind are easily missed and their transfers wait. Windows puts them in one window with one answer (`incoming::open_or_add`).

## Low

- **A transfer cancelled by a change of the Contact ID stays waiting on a side that was not connected.** Change My Contact ID cancels every transfer, which the other side learns only if it is connected at that moment. One that is not goes on asking the old Contact ID, which nobody answers any more, and shows its transfer as waiting for the connection until the user cancels it or it expires after 30 days. A tombstone cannot answer either, since the old identity is gone.
### On Android, a file that another app saves under the name of a waiting part can be taken for it

Android tells Dodavo no birth time of a file before Android 11, nor on its shared storage (`FileId::weak`), and ext4 gives the number of a deleted file to the next one at once. So a part file that the user deletes while its transfer waits, followed by another file of the same name, looks like the part to the record. Dodavo's own new parts and folders leave the names of records alone (`session::held_names`), and a file shorter than what reached the disk is not taken for the part (`TransferRecord::part_shorter`); a longer one that another app saves into `Download/Dodavo` under that name is. `FS_IOC_GETVERSION` (the inode generation) would tell them apart on ext4 where the app may ask for it.

### Automatic acceptance checks the free space of each item alone

Each offer from a recipient is checked by itself (`Event::Offer` in `src/app.rs`), so several large items that fit one by one all start, and the later ones fail once the disk is full. The Incoming Transfer window counts them together. The check would have to include what is still to arrive for the transfers already running.

### A Mac says nothing when sending to a recipient who has just been removed

`Core::send` returns no error when `contact_by_id` finds nobody, so no transfer appears and no message says why. It takes a recipient removed or given another ID while the send is on its way, and a new text in the Mac table.

### An all-users uninstall leaves other accounts' entries in File Explorer's menus

Each account writes its own "Send with Dodavo" entry and Send to shortcut when Dodavo starts there, and the uninstaller reaches only the account that runs it. In the other accounts both stay, and fail when used, until they are deleted by hand or Dodavo is installed again. The same goes for their "Start with Windows" value.

### Colliding names in a zip overwrite each other on Windows

`archive::unzip` opens each entry with `create` and `truncate`, so of two entries whose names are the same when case does not count, the later one replaces the earlier one. A zip from Dodavo on Windows cannot have that, and one from a Mac has unique names (`archive::names`), so it takes a hand-made zip. The Mac creates only new files.

### A very long name overflows with `.part` on Windows

`sanitize_file_name` does not shorten, and `.part` or `.zip.part` is appended afterwards, so an offered name of 251 UTF-16 units or more fails where it is created. A Mac sends at most 240, and shortens what it receives.

### Between connections, nothing keeps a part as it was on Windows

A share mode holds the part only during a connection, while `have` and the hash in memory are trusted across connections. A program that writes to the part in between is noticed only by the check at the end, which fails the transfer. A Mac hashes the part from the disk at the end anyway.

### A Windows sender shows 0 B/s while a Mac checks what arrived

A Mac hashes a part from the disk after `Done`, which takes a while on a large item. Meanwhile a sender on Windows shows Sending at 100%, and its tooltip a speed of zero, since `Transfer::rate` lacks the check for all of it being done that `Progress::rate` has.

### A file made under the part's name on FAT can be taken for the part

On FAT32 and exFAT, a part is recognized by its creation time, and Windows gives a new file the creation time of a file of that name deleted within about 15 seconds ("tunnelling"). `TransferRecord::part_shorter` catches a new file shorter than what was received, but not one of that length or longer. Such a file would be written to and then deleted, as was seen with a shorter one before that check. It takes a part being deleted and a new file of exactly its name appearing within seconds while Dodavo is not running.

### Check for Updates can leave a second box open

Choosing Check for Updates while an automatic check is running opens a "Checking for updates…" message box, and the answer then opens on top of it, so both have to be closed (`check_now` in `src/ui/update.rs`). The automatic check takes only a moment, so this is rare.

### Some error details stay in English

Most failure texts are translated (`net::failure_text`), but these are not: other errors of a dial (the last case of `net::connect_error_text`), network errors of the update check (`update::http`), and a failure to start networking (`Event::BindFailed`). They appear in every language.

### The help overstates the wait before the daily update check

All four help pages say that it comes about 15 seconds after Dodavo has started and is online. When a check is due, it starts as soon as Dodavo goes online (`app::handle_net_event` calls `update::tick`). This makes no difference to the user.
