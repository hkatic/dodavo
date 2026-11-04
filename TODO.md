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
- **Publish the first Mac release.** Build it with `macos/release.sh`, and publish it with `macos/publish.sh`. A release has to carry both manifests (see README).

- **Install the published 1.0.0 from the release page.** Try a new installation for the current user and for all users, an installation over an older copy, and uninstalling.
- **Send between two computers on different networks.** Check connecting through real NAT, and falling back to a relay there.
- **Go through the main tasks with real screen readers.** Use NVDA, JAWS and Narrator, and listen to what is spoken, which scripted checks through MSAA and UI Automation do not cover.
- **Back up the update signing key.** Keep copies in at least two places besides the PC that signs the releases. If the key is lost, installed copies can never accept an automatic update again.

## Medium

- **Run the Mac version on the oldest macOS at hand.** A virtual machine should reach macOS 12, if this Mac can boot one. macOS 11 needs a Mac that has it.
- **Have Windows' progress window use `view::TransferView`.** It follows the same rules with its own state, so a change has to be made in both. Do it with live tests on Windows at hand.
- **Have the Serbian (Latin) Mac texts reviewed.** macOS has no Serbian of its own to take the terms from, unlike Croatian and Slovenian; these follow the Croatian ones and the Windows table.
- **Commit a `.gitattributes`.** Line ends now depend on each machine's own settings, and files show as changed on a Mac that differ only in those (`res/app.manifest`).
- **Build for Intel Macs.** Add `x86_64-apple-darwin` and combine the two with `lipo`, once there is one to test on.

- **Run the first real update when the next version is out.** Download and install it from the real GitHub URL.
- **Run the builds for Windows 7 SP1 and Windows 8.1 on those systems.** Include "Send with Dodavo" and Send to.
- **Check the Send with Dodavo window with a screen reader.** Check that it comes to the front from the Send to menu, and what NVDA, JAWS and Narrator say when it does.
- **Have native speakers check the Serbian (Latin) and Slovenian texts of 1.0.1.** Including the Windows labels that the help quotes: "Pošalji u" and "Prikaži još opcija", "Pošlji v" and "Pokaži več možnosti".
- **Run transfers with the 32-bit builds.**
- **Check the notifications live on Windows 11.** Test scripts cannot see them, so watch them by hand.

## Low

- **Trim the Mac's license file.** `macos/licenses.sh` takes all of Rust's `COPYRIGHT-library.html`, 1.4 MB of it; `installer/licenses.ps1` picks what is linked.
- **Make a Liquid Glass icon for macOS 26 and later.** `macos/Resources/make-icon.swift` draws one for the older grid, which those systems show as it is.

- **Give Dodavo an entry in Windows 11's new File Explorer menu.** It takes a sparse package signed with a trusted certificate, and a COM DLL (`IExplorerCommand`), so it comes after code signing (see `ISSUES.md`). Windows 10 builds 19041 and later could use it too; Windows 7 and 8.1 keep the classic entries.
- **Show the items of one selection in one window.** Each item opens its own progress window, up to 100 from the context menu and more from Send to.
- **Report the zip crate's panic to its authors.** `enclosed_name` panics on ordinary names such as "Kuća.jpg" in zip 9.1 and 9.2, because `is_windows_reserved_device_name` splits a name at a byte index. Dodavo avoids it with its own check (`archive::entry_path`).
