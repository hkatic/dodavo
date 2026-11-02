# Known issues

What is wrong in Dodavo now, most important first. Work that is not a defect is in [TODO.md](TODO.md).

- **High:** loses data, makes the two sides disagree about a transfer, or stops normal use.
- **Medium:** many users will notice it, or it is not explained yet and could turn out to be worse.
- **Low:** rare or cosmetic.

Remove an item in the commit that fixes it. This file is public, so keep secrets and private addresses or paths out of it.

## High

None known.

## Medium

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

### A file made under the part's name on FAT can be taken for the part

On FAT32 and exFAT, a part is recognized by its creation time, and Windows gives a new file the creation time of a file of that name deleted within about 15 seconds ("tunnelling"). `TransferRecord::part_shorter` catches a new file shorter than what was received, but not one of that length or longer. Such a file would be written to and then deleted, as was seen with a shorter one before that check. It takes a part being deleted and a new file of exactly its name appearing within seconds while Dodavo is not running.

### Check for Updates can leave a second box open

Choosing Check for Updates while an automatic check is running opens a "Checking for updates…" message box, and the answer then opens on top of it, so both have to be closed (`check_now` in `src/ui/update.rs`). The automatic check takes only a moment, so this is rare.

### Some error details stay in English

Most failure texts are translated (`net::failure_text`), but these are not: other errors of a dial (the last case of `net::connect_error_text`), network errors of the update check (`update::http`), and a failure to start networking (`Event::BindFailed`). They appear in every language.

### The help overstates the wait before the daily update check

All four help pages say that it comes about 15 seconds after Dodavo has started and is online. When a check is due, it starts as soon as Dodavo goes online (`app::handle_net_event` calls `update::tick`). This makes no difference to the user.
