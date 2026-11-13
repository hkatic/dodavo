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

### A transfer under way does not say that the other side is on another protocol version

A new send to a Dodavo that does not take this version's transfers ends with word of that (`net::no_such_protocol`, `error_other_version`), and so does a chat with one that has none. A transfer that was under way when one side changed its ALPN only waits for the connection, and a released version that dials a newer one still shows iroh's own English text. Decide what a newer version does for both before the first change of that ALPN.

### An offer was reported as declined although nobody answered (not explained)

Seen once on 2026-11-01: an offer from a contact waited in the Incoming Transfer window, because there was not enough space, and within 5 minutes the sender was told that the recipient had declined. A repeat that was watched for 5.5 minutes stayed open as it should. `receive::handle` sends `Decline` when the user answers no, when the answer's sender is dropped, and on a cancel; which of these it was is not known.

### Free space did not come back after a failed transfer on a full disk (not explained)

Seen once on 2026-11-01: after a 500 MB transfer failed on a 200 MB NTFS volume, the volume stayed at 204 KB free although no files were left on it. FAT32 and exFAT volumes got all their space back. It may be a deleted part that another program, such as Defender or the indexer, still held open; that was not checked.

### In dark mode on Windows 11, the labels of check boxes are hard to read

The labels of check boxes are drawn in grey on the dark background, with less contrast than the other texts of the same window (Settings, the Incoming Transfer window), as seen on Windows 11 build 26300. `theme::theme_child` gives them the classic look so that they take the text color of `WM_CTLCOLORSTATIC`, which they do not there. Light mode and high contrast are not affected.

### While a message box or task dialog is open, keys do not work in Dodavo's other windows

A message box or a task dialog runs a message loop of Windows' own, which does not call `main_window::translate`, so until it is closed Tab, Enter, Escape and the access keys do nothing in the other Dodavo windows: in a chat window while a progress window asks whether to wait for a lost connection, say, Enter does not send. The windows of `window::run_modal` are not affected. A message hook (`SetWindowsHookExW` with `WH_MSGFILTER`) that passes those windows' messages to `translate` would fix it.

## Low

### A transfer's notification takes the place of a chat's

The notification area icon shows one notification at a time, and a click does what the last one was for (`App.balloon_action`), so a notification about a transfer that comes right after one about a chat message replaces it, and a click on it leads to the transfer. The message is in its chat window, whose taskbar button flashes.

### OK in Settings undoes some choices made meanwhile in other windows

Settings saves its fields with OK as they show them, so what was chosen in another window while Settings was open is undone: "Don't ask me again" in the questions of a progress window (`close_running_transfer`, `connection_lost`), and "Don't warn me again" and another folder in the warning about a temporary file (`warn_fat_temp`, `temp_dir`). `close_when_ended`, which every progress window has a check box for, is written only when its box was changed in Settings; the others could do the same.

- **A transfer cancelled by a change of the Contact ID stays waiting on a side that was not connected.** Change My Contact ID cancels every transfer, which the other side learns only if it is connected at that moment. One that is not goes on asking the old Contact ID, which nobody answers any more, and shows its transfer as waiting for the connection until the user cancels it or it expires after 30 days. A tombstone cannot answer either, since the old identity is gone.

### The size limit counts each item alone

Each item is its own transfer, so several items that are each within the limit can together be larger than it. A limit on what one sender may send in total would have to count what is waiting and under way from them.

### An all-users uninstall leaves other accounts' entries in File Explorer's menus

Each account writes its own "Send with Dodavo" entry and Send to shortcut when Dodavo starts there, and the uninstaller reaches only the account that runs it. In the other accounts both stay, and fail when used, until they are deleted by hand or Dodavo is installed again. The same goes for their "Start with Windows" value.

### Another copy of the same data under a waiting part's name can be taken for the part, where files have no lasting identity

On FAT32 and exFAT a part is recognized by its creation time, which Windows gives a new file of that name for about 15 seconds after the part was deleted ("tunnelling"), and on Android's shared storage by a number that ext4 gives to the next file at once (`FileId::weak`). There a file under the part's name counts as the part only when it is at least as long as what reached the disk and begins with the same 64 KB (`session::Head`, `TransferRecord::another_file`), which another program's download of the same data under that name does. Such a file would be written to and then deleted. A transfer that an earlier version began has no note of how its part begins, so for it any file of that length or longer passes.

### On a Mac and on Android, a file that is not there is told of as partly transferred data

A system error that says that a file or folder does not exist (`ENOENT`, `ENOTDIR`) is worded as "The partly transferred data is no longer there" (`platform::error_text`), whatever was missing. A send of a file that was deleted before it started fails with "Cannot read …: The partly transferred data is no longer there". It needs a text of its own for an item that is not there, in every language.
