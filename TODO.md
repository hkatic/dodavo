# To do

What still has to be done, most important first. Defects are in [ISSUES.md](ISSUES.md), together with what it takes to fix them.

- **High:** do this soon.
- **Medium:** do this when the occasion comes, or when there is time.
- **Low:** worth doing, but nothing depends on it.

Remove an item in the commit that settles it. This file is public, so keep secrets and private addresses or paths out of it, and write plain tasks and facts.

## High

- **Go through the Mac version with VoiceOver by hand.** `macos/tests/voiceover-checklist.md` is the list. Listen for what is awkward rather than wrong, and try it with braille, with the keys that VoiceOver takes itself, and through a change between online and offline.
- **Exchange transfers between a Mac and released Windows 1.0.0.** Both directions, file and folder in all three ways of keeping one, names Windows cannot take, pause, cancel on each side, restart on each side and a lost connection. `examples/peer.rs` does it without the interface.
- **Try what macOS decides at a login and on older systems.** Telling a start at login (`LoginItem.launchedAtLogin`), also with `SMAppService`; the Local Network and Files and Folders prompts during an automatic receive at login; opening a received app that has the quarantine mark; `LSSharedFileList` on macOS 11 and 12.
- **Try the changed settings on a Mac.** The Settings window shows Downloads with "Always save" on, and "Start transfers automatically" is unavailable with it off; an offer from a recipient then asks where to save.

- **Finish the TalkBack pass of the Android app on a phone.** Gone through on 2026-11-06 (Android 16): recipients, the editor, the recipient menu and its questions, an offer in front and in the background, Transfers, Settings, About, Licenses and Help. Left from `android/tests/talkback-checklist.md`: sharing to Dodavo, the actions of the offer notification, Blocked senders with a blocked sender, the largest font and display size, and braille.
- **Exchange transfers between Android and released Windows 1.0.0.** Both directions, file and folder in all three ways of keeping one, names Windows cannot take, pause, cancel on each side, a restart of the phone during a transfer and airplane mode.
- **Prepare the release on Google Play.** `android/play/checklist.md` (the developer account exists): the closed test with 12 testers for 14 days, the store listing with screenshots, Data safety (the answers marked "Decide"), the foreground service declarations with their videos.

- **Try blocking senders by hand on Windows and on a Mac.** Block Sender in the Incoming window (with several of the sender's offers waiting), Block on a recipient, and unblocking: on Windows the Blocked Senders dialog with NVDA, JAWS and Narrator, in light and dark mode; on a Mac the Blocked tab with VoiceOver (see `macos/tests/voiceover-checklist.md`). Check that an offer from a blocked sender shows nothing and is declined on the other side. Ctrl+B on Windows and Command-B on a Mac block the selected recipient, and Command-B blocks the sender in the Mac's Incoming Transfer window.
- **Try the size limit by hand on Windows, on a Mac and on Android.** In Settings: the box, the size, the unit and the list of what happens to a larger item; a recipient's own limit in Add and Edit Recipient; the line in the Incoming window, for one item and for several; each of the three actions, with the notification of the third while Dodavo is in front and while it is not. With NVDA, JAWS, Narrator, VoiceOver and TalkBack: that the size field and the unit list say what they are. Check that the Windows Settings window still fits a 768-pixel-high screen with the larger text sizes, and that the size and its unit sit on their label's line in the Mac's Settings and recipient sheet.
- **Try Change My Contact ID by hand on Windows and on a Mac.** From the File menu and from Settings, with NVDA, JAWS, Narrator and VoiceOver (see `macos/tests/voiceover-checklist.md`): the question is read, and No (Cancel on a Mac) changes nothing; with a transfer under way and an offer waiting, both end, and the other side sees a cancel and a declined offer; the Settings field shows the new Contact ID, Copy copies it, the old one no longer connects, the new one does, and it is still there after a restart. On the Mac, build it first: its Swift side has only been type-checked by CI.

- **Install the published 1.0.0 from the release page.** Try a new installation for the current user and for all users, an installation over an older copy, and uninstalling.
- **Send between two computers on different networks.** Check connecting through real NAT, and falling back to a relay there.
- **Go through the main tasks with real screen readers.** Use NVDA, JAWS and Narrator, and listen to what is spoken, which scripted checks through MSAA and UI Automation do not cover.
- **Back up the update signing key.** Keep copies in at least two places besides the PC that signs the releases. If the key is lost, installed copies can never accept an automatic update again.

## Medium

- **Change the Contact ID while a send waits for the receiver's verdict.** Such a send (all of its data went out, and the receiver has not said whether it has the item) cannot be ended by a cancel, so Change My Contact ID refuses while one exists (`net::IdentityError::Busy`), which can last until the receiver is back. End such a send at a change of the identity (the receiver cannot be asked under the new one), with a text that says that the receiver may have the item.
- **Use user-initiated data transfer jobs on Android 14 and later.** For transfers that the user starts (Send, Accept): Google recommends them over `dataSync`, which Android 15 limits to 6 hours a day. Transfers that start by themselves have to stay with the service.
- **Try the Android app on a phone with Android 8 to 10.** The emulator was the only one; such phones keep the shared storage on sdcardfs, not FUSE.
- **Let the user choose where received items go on Android.** Only `Download/Dodavo` now: another folder (from the folder picker) would need the core to write through the app, as it reads documents (`src/source.rs`).
- **Offer Accept and Block in the notification of an offer.** Only Review and Decline now; Accept would have to check the free space without a screen.
- **Show a running transfer as a Live Update on Android 16 and later** (`NotificationCompat.ProgressStyle`), and offer recent recipients as Direct Share targets.
- **Move the Android app to Material 3 Expressive** once Compose Material 3 1.5 is stable.

- **Run the Mac version on the oldest macOS at hand.** A virtual machine should reach macOS 12, if this Mac can boot one. macOS 11 needs a Mac that has it.
- **Have Windows' progress window use `view::TransferView`.** It follows the same rules with its own state, so a change has to be made in both. Do it with live tests on Windows at hand.
- **Have the Serbian (Latin) Mac texts reviewed.** macOS has no Serbian of its own to take the terms from, unlike Croatian and Slovenian; these follow the Croatian ones and the Windows table.
- **Build for Intel Macs.** Add `x86_64-apple-darwin` and combine the two with `lipo`, once there is one to test on.

- **Run the first real update on a Mac when the next version is out.** Have a copy of 1.0.1 download and install it from the real GitHub URL.
- **Run the builds for Windows 7 SP1 and Windows 8.1 on those systems.** Include "Send with Dodavo" and Send to.
- **Check the Send with Dodavo window with a screen reader.** Check that it comes to the front from the Send to menu, and what NVDA, JAWS and Narrator say when it does.
- **Have native speakers check the Serbian (Latin) and Slovenian texts of 1.0.1.** Including the Windows labels that the help quotes: "Pošalji u" and "Prikaži još opcija", "Pošlji v" and "Pokaži več možnosti".
- **Run transfers with the 32-bit builds.**
- **Check the notifications live on Windows 11.** Test scripts cannot see them, so watch them by hand.

## Low

- **Open the folder of a received folder on Android.** Show folder opens Android's view of the downloads, as there is no standard way to open a folder in a file manager.
- **Trim the Mac's license file.** `macos/licenses.sh` takes all of Rust's `COPYRIGHT-library.html`, 1.4 MB of it; `installer/licenses.ps1` picks what is linked.
- **Make a Liquid Glass icon for macOS 26 and later.** `macos/Resources/make-icon.swift` draws one for the older grid, which those systems show as it is.

- **Give Dodavo an entry in Windows 11's new File Explorer menu.** It takes a sparse package signed with a trusted certificate, and a COM DLL (`IExplorerCommand`), so it comes after code signing (see `ISSUES.md`). Windows 10 builds 19041 and later could use it too; Windows 7 and 8.1 keep the classic entries.
- **Show the items of one selection in one window.** Each item opens its own progress window, up to 100 from the context menu and more from Send to.
- **Report the zip crate's panic to its authors.** `enclosed_name` panics on ordinary names such as "Kuća.jpg" in zip 9.1 and 9.2, because `is_windows_reserved_device_name` splits a name at a byte index. Dodavo avoids it with its own check (`archive::entry_path`).
