# Known issues

What is wrong in Dodavo now, most important first. Work that is not a defect is in [TODO.md](TODO.md).

- **High:** loses data, makes the two sides disagree about a transfer, or stops normal use.
- **Medium:** many users will notice it, or it is not explained yet and could turn out to be worse.
- **Low:** rare or cosmetic.

Remove an item in the commit that fixes it. This file is public, so keep secrets and private addresses or paths out of it.

## High

None known.

## Medium

### Three network tests time out on GitHub's Mac runner (not explained)

`a_cancel_waits_for_the_verdict_across_a_restart`, `a_send_needs_its_data_again_when_the_receiver_lacks_it` and `finish_alone_after_restart_during_unpacking` reconnect after a restart, and wait more than 120 s there, while they pass on a Mac elsewhere and on the Windows runner. It may be that the runner keeps an unsigned test program from the local network (Local Network privacy), which would leave a slow way through a relay; that was not checked. If so, the same wait could be what a user who denies that permission sees. CI runs them without letting them fail the run.

### The Mac version is not code-signed or notarized yet

Without a Developer ID certificate, macOS 15 and later block a downloaded app until the user allows it in Privacy & Security, and permissions may be asked again after each update. `macos/build.sh` and `macos/make-dmg.sh` do it all once `DODAVO_SIGN_IDENTITY` and notary credentials are set. The certificate exists now, so only the credentials are missing.

### The setup program and the program are not code-signed

Every new download gets SmartScreen's "Windows protected your PC", or the unknown publisher question on Windows 7. Automatic updates are not affected. `installer\build.ps1` signs everything once a certificate is configured (`DODAVO_SIGN_THUMBPRINT` or `DODAVO_SIGN_PFX`), so only the certificate is missing.

### Transfers through a relay are slow

Without a direct path, data goes through number0's free public relays. A relay-only test of 500 MB ran at a flat 0.96 MB/s, about 18 minutes per GB, which looks like a rate limit (2026-11-01; both sides were on one PC, so its upload was also in the path). The fix is our own `iroh-relay`, configured in `bind()` in `src/net/mod.rs`.

### Nothing explains why peers on different protocol versions cannot connect

After a change of the ALPN, the two sides cannot connect, and the user only sees iroh's own English text (the last case of `net::connect_error_text`). Decide how a newer version handles this before the first change of the ALPN. A new release cannot change what 1.0.0 itself shows.

### An offer was reported as declined although nobody answered (not explained)

Seen once on 2026-11-01: an offer from a contact waited in the Incoming Transfer window, because there was not enough space, and within 5 minutes the sender was told that the recipient had declined. A repeat that was watched for 5.5 minutes stayed open as it should. `receive::handle` sends `Decline` when the user answers no, when the answer's sender is dropped, and on a cancel; which of these it was is not known.

### Free space did not come back after a failed transfer on a full disk (not explained)

Seen once on 2026-11-01: after a 500 MB transfer failed on a 200 MB NTFS volume, the volume stayed at 204 KB free although no files were left on it. FAT32 and exFAT volumes got all their space back. It may be a deleted part that another program, such as Defender or the indexer, still held open; that was not checked.

## Low

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
