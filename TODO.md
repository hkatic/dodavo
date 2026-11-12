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

- **Finish the TalkBack pass of the Android app on a phone.** Gone through on 2026-11-06 (Android 16): recipients, the editor, the recipient menu and its questions, an offer in front and in the background, Transfers, Settings, About, Licenses and Help. Left from `android/tests/talkback-checklist.md`: sharing to Dodavo, the actions of the offer notification, Blocked senders with a blocked sender, the largest font and display size, and braille.
- **Try the rest of the Android app's transfers on a phone.** Sending single files, sharing to Dodavo from another app, pause and cancel on each side, and a send to a recipient who was removed meanwhile.
- **Exchange transfers between Android and released Windows 1.0.0.** Both directions, file and folder in all three ways of keeping one, names Windows cannot take, pause, cancel on each side, a restart of the phone during a transfer and airplane mode.
- **Prepare the release on Google Play.** `android/play/checklist.md` (the developer account exists): the closed test with 12 testers for 14 days, the store listing with screenshots, Data safety (the answers marked "Decide"), the foreground service declarations with their videos.

- **Try the chat by hand on Windows.** Between two computers, with NVDA, JAWS and Narrator: a message that arrives while the chat window is in front is read once, with the name first, and so are several that arrive together; one that arrives while another window is in front is told by its notification, which opens the chat when selected; the history reads message by message with the arrow keys, also long ones that wrap; "… is typing" is said once; a message to someone who is offline, who takes messages from recipients only, and whose Dodavo has no chat (a released 1.0.x, a Mac, a phone) each says why it was not sent. Then the Chat buttons of the Incoming Transfer and progress windows, Block and Delete Conversation, the dark theme, another text size, and Windows 7.
- **Listen to the sounds on Windows, and try the Sounds tab.** Each event's sound at its event, that none plays twice or on top of Windows' notification sound, a WAV file of one's own, None, all sounds off, Play and Reset All, with NVDA, JAWS and Narrator. The built-in sounds are tones made by `examples/gen_sounds.rs`; recorded ones can take their place in `res/sounds` (then drop the test that compares them with the generator).
- **Try blocking senders by hand on Windows.** What is left is Block Sender with several of the sender's offers waiting, Block on a recipient (the menus and Ctrl+B), and the Blocked Senders dialog with NVDA, JAWS and Narrator, in light and dark mode.
- **Try the size limit by hand on Windows and on Android.** On Windows, what is left is changing the limit in the Settings window and in Add and Edit Recipient (it then holds without a restart), the line in the Incoming window for several items, and the notification of the third action while Dodavo is in front and while it is not. On Android, all of it: in Settings the box, the size, the unit and the list of what happens to a larger item; a recipient's own limit; the line in the Incoming window, for one item and for several; each of the three actions, with the notification of the third. With NVDA, JAWS, Narrator and TalkBack: that the size field and the unit list say what they are.
- **Try Change My Contact ID by hand on Windows.** What is left is the Change… button in Settings, that No changes nothing, that Copy copies the new Contact ID, and what NVDA, JAWS and Narrator read of the question and the message.
- **Try on a Mac what Change My Contact ID says while a send waits for the receiver's verdict.** Such a send keeps the Contact ID as it is (`net::IdentityError::Busy`), which only a test of the network shows so far: the message in the app, and what VoiceOver reads of it.

- **Try a large folder with two drives.** Receive a folder onto an external drive with less room on the system drive than the folder takes: the Incoming window counts only the external drive, `name.zip.part` arrives there and is gone at the end. Send a folder from an external drive under the same condition: `send-….zip` is packed next to it, hidden, and is gone at the end, also after a cancel and after ending Dodavo while it packs. With a FAT32 drive and a folder of 4 GB or more with only smaller files: its zip arrives in the temporary folder. On Windows, on a Mac, and the receiving on Android.
- **Try the tabs of the Windows Settings window by hand.** With NVDA, JAWS and Narrator: each says the tab and its position when the focus is on the tabs and after Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+Page Down and Ctrl+Page Up from a setting; the arrow keys change the tab; an error on OK shows the page of its field. In light, dark and high contrast, at 100 % and at a larger scale, with the larger text sizes, and on Windows 7.
- **Try the warning about a temporary file that is too large by hand.** With a FAT32 volume as the folder for temporary files, on Windows and on a Mac: send a folder of more than 4 GB, and accept one into a folder on a FAT32 volume with only the unpacked folder kept (by hand and from a recipient, which must not start by itself). Each choice of the question: another folder (the transfer then goes on without asking again), going on anyway, Cancel and Escape, and the check box, which clears the setting. With NVDA, JAWS, Narrator and VoiceOver. On a Mac all of it has only been built by CI: also the check box in Settings, and that Settings shows a folder chosen from the question when its window comes back to the front.
- **Try closing the progress window at the end by hand on Windows.** With the setting on: a completed send and receive, a declined, a cancelled and a failed transfer each close their window and give a notification that says how it ended; the two check boxes for the message and the question are unavailable. With NVDA, JAWS and Narrator.
- **Try the folder for temporary files by hand on Windows and on a Mac.** In Settings: the field and Browse… on Windows with NVDA, JAWS and Narrator; the path, Choose… and Use Default on a Mac with VoiceOver, where it has only been built by CI. Send a folder and see its zip in `Dodavo` inside the chosen folder; choose a folder on a drive that is then unplugged and see a send still work; clear the field and see the system's place used again.
- **Install the published 1.0.0 from the release page.** Try a new installation for the current user and for all users, an installation over an older copy, and uninstalling.
- **Send between two computers on different networks.** Check connecting through real NAT, and falling back to a relay there.
- **Go through the main tasks with real screen readers.** Use NVDA, JAWS and Narrator, and listen to what is spoken, which scripted checks through MSAA and UI Automation do not cover.
- **Back up the update signing key.** Keep copies in at least two places besides the PC that signs the releases. If the key is lost, installed copies can never accept an automatic update again.

## Medium

- Try the check boxes of the recipient lists (the main window and Send with Dodavo) with NVDA, JAWS and Narrator on Windows: each row says whether it is checked, and Space says the change. Try them in the dark theme and on Windows 7 as well.
- Try sending to several recipients by hand on the Mac: the selection with Shift, Command and VoiceOver's VO-Command-Return, the Send panel's message, a drop on a selected row, and the list in the question for items from the Dock and Services.
- Try the selection of recipients by hand on Android with TalkBack, on the Recipients screen and when another app shares items, on API 26 as well.

- **Add the chat to the Mac version.** The core has it (`net::chat`, `chat::History`): the bridge has to start the network with chat (`net::start_with_chat`), pass messages, typing and failures to Swift (`Out`) and take sends and the settings (`Call`), and the app needs a chat window per person that VoiceOver reads (an announcement for a message in front, a notification otherwise), with its texts in `src/i18n/mac/` and its help pages. Until then a Mac refuses chat connections, and who writes to it is told that it has no chat.
- **Add the chat to the Android app.** As on the Mac, with a chat screen, a notification channel for messages and TalkBack's live region. Messaging between users changes the Google Play listing, the Data safety answers (in-app messages) and the content rating in `android/play/`, and `Dodavo/chats` has to be left out of Android's backup (`data_extraction_rules.xml`, `backup_rules.xml`) or the backup's paragraph in `PRIVACY.md` has to name it; `PRIVACY.md` says "on Windows" of the chat in two places.
- **Add the sounds to the Mac and Android versions.** `sound::SoundEvent` and the settings (`config::Sounds`) are shared, and the built-in WAV files are in the core; each app needs a player and a settings page, and on Android the notification channels decide part of it.
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

- **Let a chat message have several lines on Windows.** The Message field is one line, so that Enter sends; a field of several lines would need Enter and Ctrl+Enter (or Shift+Enter) told apart, and Tab left to the window (`window::read_only_text_proc` does that for text that is only read).
- **Keep chat messages for someone who is offline.** A message is delivered now or fails; one that waits would need a record on the disk and a dial that tries again, as a transfer has.
- Pack a folder once for several recipients. Each of its transfers makes its own archive now, and Android copies a shared item once for each recipient.

- **Open the folder of a received folder on Android.** Show folder opens Android's view of the downloads, as there is no standard way to open a folder in a file manager.
- **Make `android/check-elf.sh` work in Git Bash on Windows.** Given an APK there, it finds no native library in it, although they are there; given the `.so` files, it works.
- **Trim the Mac's license file.** `macos/licenses.sh` takes all of Rust's `COPYRIGHT-library.html`, 1.4 MB of it; `installer/licenses.ps1` picks what is linked.
- **Make a Liquid Glass icon for macOS 26 and later.** `macos/Resources/make-icon.swift` draws one for the older grid, which those systems show as it is.

- **Give Dodavo an entry in Windows 11's new File Explorer menu.** It takes a sparse package signed with a trusted certificate, and a COM DLL (`IExplorerCommand`), so it comes after code signing (see `ISSUES.md`). Windows 10 builds 19041 and later could use it too; Windows 7 and 8.1 keep the classic entries.
- **Show the items of one selection in one window.** Each item opens its own progress window, up to 100 from the context menu and more from Send to.
- **Have `net::watch_online` wait for changes and report only real ones.** It polls `home_relay_status()` every 250 ms until its first report and every 2 s after it, where the watcher's changes can be awaited (`Watcher::updated` in n0-watcher 1.0), raced against the end of `ONLINE_GRACE`. Each new endpoint also starts from nothing reported, so Change My Contact ID while online reports "online" a second time; pass in what the endpoint before it reported, which is nothing after an `Event::BindFailed`.
- **Report the zip crate's panic to its authors.** `enclosed_name` panics on ordinary names such as "Kuća.jpg" in zip 9.1 and 9.2, because `is_windows_reserved_device_name` splits a name at a byte index. Dodavo avoids it with its own check (`archive::entry_path`).
