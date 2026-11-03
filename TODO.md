# To do

What still has to be done, most important first. Defects are in [ISSUES.md](ISSUES.md), together with what it takes to fix them.

- **High:** do this soon; until it is done, a serious problem could go unnoticed.
- **Medium:** do this when the occasion comes, or when there is time.
- **Low:** worth doing, but nothing depends on it.

Remove an item in the commit that settles it. This file is public, so keep secrets and private addresses or paths out of it.

## High

- **Go through the Mac version with VoiceOver by hand.** `macos/tests/voiceover-checklist.md` is the list. All windows, sheets, menus and transfers were driven with VoiceOver running on macOS 27, reading what it said through its AppleScript, and what that found is fixed; still, nobody has listened to it for what is awkward rather than wrong, or used it with braille. What it could not reach: the keys that VoiceOver takes itself, and the change between online and offline.
- **Exchange transfers between a Mac and released Windows 1.0.0.** Both directions, file and folder in all three ways of keeping one, names Windows cannot take, pause, cancel on each side, restart on each side and a lost connection. `examples/peer.rs` does it without the interface. Only `archive::unzip` under Windows' rules has been tried so far, not a live Windows copy.
- **Try what macOS itself decides.** Tried on macOS 27: a notarized copy with the mark of a download, opened from its disk image and from a folder (where macOS translocates it), moving it to Applications from both, an update between copies signed by the team and the refusal of one that is not, Open at Login being registered, and notifications with their actions. Not tried: telling a start at login (`LoginItem.launchedAtLogin`), which needs a real login, also with `SMAppService`; the Local Network and Files and Folders prompts during an automatic receive at login; opening a received app that has the quarantine mark; `LSSharedFileList` on macOS 11 and 12.
- **Publish the first Mac release.** `macos/publish.sh` has never been run, and 1.0.1 needs its release notes in all four languages. The app and its disk image can be notarized now (`DODAVO_SIGN_IDENTITY` and the notary profile `dodavo`, on the Mac that has the certificate). A release has to carry both manifests (see README).

- **Install the published 1.0.0 from the release page.** Nobody has run the published setup program yet. Try a new installation for the current user and for all users, an installation over an older copy, and uninstalling.
- **Send between two computers on different networks.** All live tests so far ran on one PC, so connecting through real NAT, and falling back to a relay there, has never been tried.
- **Go through the main tasks with real screen readers.** Use NVDA, JAWS and Narrator. The scripted checks only cover what MSAA and UI Automation expose, not what is actually spoken.
- **Back up the update signing key.** Keep copies in at least two places besides the PC that signs the releases. If the key is lost, installed copies can never accept an automatic update again.

## Medium

- **Run the Mac version on the oldest macOS at hand.** A virtual machine should reach macOS 12, if this Mac can boot one. macOS 11 needs a Mac that has it; until then it is provided as is.
- **Have Windows' progress window use `view::TransferView`.** It follows the same rules with its own state, so a change has to be made in both. Do it with live tests on Windows at hand.
- **Have the Serbian (Latin) Mac texts reviewed.** macOS has no Serbian of its own to take the terms from, unlike Croatian and Slovenian; these follow the Croatian ones and the Windows table.
- **Commit a `.gitattributes`.** Line ends now depend on each machine's own settings, and files show as changed on a Mac that differ only in those (`res/app.manifest`).
- **Build for Intel Macs.** Add `x86_64-apple-darwin` and combine the two with `lipo`, once there is one to test on.

- **Run the first real update when the next version is out.** Downloading and installing an update has only been tested against a local web server. Against the real GitHub URL, only the check has run so far, which answered that 1.0.0 is the latest version.
- **Test on Windows 7 SP1 and Windows 8.1.** No such system has been available. The builds for them pass the import check and start on Windows 11, and the README says that they are provided as is.
- **Run transfers with the 32-bit builds.** For 1.0.0 they were only started and closed on Windows 11.
- **Check the notifications live.** Windows 11 notifications were not visible to the test scripts, so only unit tests cover them.

## Low

- **Trim the Mac's license file.** `macos/licenses.sh` takes all of Rust's `COPYRIGHT-library.html`, 1.4 MB of it; `installer/licenses.ps1` picks what is linked.
- **Make a Liquid Glass icon for macOS 26 and later.** `macos/Resources/make-icon.swift` draws one for the older grid, which those systems show as it is.

- **Report the zip crate's panic to its authors.** `enclosed_name` panics on ordinary names such as "Kuća.jpg" in zip 9.1 and 9.2, because `is_windows_reserved_device_name` splits a name at a byte index. Dodavo avoids it with its own check (`archive::entry_path`). There is no record that this was reported.
