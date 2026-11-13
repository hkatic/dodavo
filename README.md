# Dodavo

*Dodavo* comes from the Croatian *dodati*, "to hand over", and is pronounced "doh-DAH-voh".

Send files and folders directly from one device to another: Windows PCs, Macs with Apple silicon, and Android phones and tablets. You don't need an account, a cloud upload, a ZIP file you made yourself, or a link. What follows is mostly about Windows; [the Mac version](#the-mac-version) and [the Android version](#the-android-version) have their own sections.

- **Send File / Send Folder** to a person in your recipient list (from the File menu, the right-click menu, the buttons, or by dragging items onto a person).
- **Several recipients at once:** check their boxes in the list (Space) and send as usual; each person gets a transfer of their own.
- **Send with Dodavo** in File Explorer's context menu, and Dodavo in its Send to menu, on Windows 7 to 11. On Windows 11 they are under *Show more options*, which Shift+F10 opens right away. Settings can remove them.
- Folders are zipped automatically, sent, and unpacked on the recipient's computer. In Settings the recipient can choose to keep only the ZIP file, or both the folder and the ZIP file.
- Files and folders keep their Date modified and Date created: single files carry them in the offer, and folder ZIPs store them both as MS-DOS dates and exactly in the NTFS extra field, which is what Dodavo restores when it unpacks.
- The recipient sees an *Incoming Transfer* window with the name and size of the item and whether there is enough free disk space for it (a folder that is unpacked needs room for the download and for unpacking). They can accept or decline and choose where to save. Several items from one person share one window and one answer.
- Anyone who keeps sending you things you don't want can be blocked, from the Incoming Transfer window or the recipient list: Dodavo then declines their offers without asking, and they only see them declined. Settings lists the blocked senders, with Unblock.
- You can limit the size of an item that someone can send you (in Settings, off by default), for everyone and for each recipient separately. An item over the limit never starts by itself: the Incoming Transfer window says that it is over the limit, and you can still accept it. Dodavo can decline such items instead, with or without a notification; the sender then only sees the offer declined.
- On Windows and on a Mac you can change your Contact ID (**Change My Contact ID…** in the File menu, or **Change…** in Settings): the old one stops working for good, transfers that are under way are cancelled, and everyone who sends you files needs the new one
- From people in your recipient list, items go by themselves to Downloads or a folder of your choice ("Always save to this folder" and "Start transfers automatically", both on by default). Without "Always save to this folder", Dodavo asks where to save each item. Offers from anyone else always ask first.
- Received files get the Mark of the Web (the `Zone.Identifier` stream, Internet zone), as browser downloads do, so SmartScreen and Office's Protected View treat them the same way.
- The progress window shows how much has been transferred, the total size, the current speed and the time remaining, with buttons to pause and resume the transfer (on either side) and to minimize it to the background. Closing it lets the transfer continue in the background or cancels it. On Windows the window stays when the transfer ends and says how it ended, with **Open the file** and **Open the folder** after a receive, unless "Close this window when the transfer ends" is checked: that one check box, in every progress window and in Settings, closes each window as soon as its transfer ends, however it ended, with a notification that says how.
- A folder is packed into a ZIP archive before it is sent, in the folder for temporary files, which Settings lets you choose on Windows and on a Mac. A drive with the FAT32 file system cannot hold a file of 4 GB or more: where a temporary archive that large would go to such a drive, Dodavo asks first and offers to choose another folder (a setting, on by default).
- **Chat** with a person, on Windows and on a Mac (Ctrl+M, or the Chat button of the Incoming Transfer and progress windows; Shift-Command-M on a Mac): short text messages that go directly between the two computers, encrypted like the files. A message that arrives is read out by screen readers, the history is a list that the arrow keys read, and the window says when the other person is typing. On Windows, several messages of the history can be selected, copied (Ctrl+C) and deleted from your computer (Delete), also from its context menu, and the main window has a **Chat…** button. Conversations are kept on your computer until you delete them. Settings decide whether anyone who is not blocked may write to you, or only your recipients. With several boxes checked, or several recipients selected on a Mac, the same command asks for one message and sends it to each of them as a private message; there are no group chats.
- **Sounds** of Dodavo's own, on Windows and on a Mac, for a message that arrives, is sent or fails, for typing, and for transfers that are offered, start, complete, fail, are declined or cancelled, lose their connection and continue. In Settings each one can be turned off or replaced by a sound file of yours (a WAV file on Windows), and all of them at once.
- The settings are on five tabs on Windows: General, Transfers, Receiving, Chat and Sounds.
- The recipient list shows the state of the transfer with each person, and the Transfers menu (in the menu bar and on the notification area icon) lists the running transfers and brings their windows back, also from the background.
- A transfer survives a lost internet connection, sleep, a crash, a shutdown and even a power cut on either computer: it waits and continues where it stopped as soon as both are online again, and the whole item is still checked with a checksum at the end. Received data is flushed to disk regularly, a receiver stopped during its last step finishes on its own after a restart, and the computer is kept from sleeping automatically while data flows. By default Dodavo waits and says so in a notification; in Settings it can ask every time, or cancel instead.
- A classic Win32 interface that follows the Windows light/dark app mode.
- Minimizes to the notification area so files can be received in the background, with an optional "Start with Windows" and "Always start minimized". Notifications report finished, paused and interrupted transfers, also while their windows are hidden.
- Checks once a day whether a newer version has been released and installs it on request: the release notes are shown, the setup program is downloaded, checked against its published size, SHA-256 and Ed25519 signature, and run silently, and Dodavo starts again by itself. Help → Check for Updates… checks at any time; the daily check can be turned off in Settings.
- Full help for keyboard and screen reader users (Help → View Help, or F1), in every supported language.
- Free software under the GNU General Public License, version 3 or later: Help → License shows the license and the licenses of the components built into Dodavo, and Help → About the version, author and website. Help → Donate opens the author's PayPal page (https://paypal.me/hrvix).
- Available in English, Croatian, Serbian (Latin) and Slovenian. It follows the Windows display language, and you can choose another one in Settings.

## Download

Download the setup program, `Dodavo-<version>-setup.exe`, from the [latest release](https://github.com/hkatic/dodavo/releases/latest) and run it. By default it installs Dodavo for the current user, without administrator rights. Dodavo then checks for new versions by itself and installs them on request.

If Windows warns when you start it, [System requirements](#system-requirements) says what to do, and how to verify the download.

## System requirements

Windows 7 with Service Pack 1, Windows 8.1, Windows 10 and Windows 11, 32-bit and 64-bit (Windows on ARM runs the x64 or x86 build through its emulation). One setup program installs the build that fits the Windows version and architecture.

Windows 7 and 8.1 get a separate build (see [Building](#building)). On Windows 7 without the SHA-2 code signing updates from 2019 (KB4474419 and KB4490628), Windows cannot verify SHA-256 signatures and shows the setup program as coming from an unknown publisher.

Dodavo carries its own list of trusted certificate authorities (the Mozilla root set bundled by iroh) for its encrypted connections to the relay and address lookup servers, so it does not depend on the Windows certificate store and connects even on a Windows 7 whose root certificates were never updated.

Windows may flag the setup program when it is started: Windows 8.1 and later show SmartScreen's "Windows protected your PC" (choose **More info**, then **Run anyway**), and Windows 7 asks whether to run software whose publisher could not be verified. Verify the download first: `certutil -hashfile Dodavo-<version>-setup.exe SHA256` must match the published `.sha256` file.

## The Mac version

A native Mac app in Swift and AppKit, which follows Apple's terms and interface guidelines and is built for VoiceOver. It exchanges transfers with Windows copies in both directions.

- macOS 11 Big Sur or later, on Apple silicon. Intel Macs may follow.
- **Send…** chooses files and folders, also several at once, for one recipient or for all that are selected in the table. Items can also be dragged onto a recipient, dropped on the Dock icon, or sent with the Finder's service **Send with Dodavo**.
- All transfers are in one **Transfers** window, like the Finder's copy window: closing it does nothing to them, and each row has Pause or Resume, Cancel Transfer, and at the end Open and Show in Finder.
- Closing the main window leaves Dodavo running, so that it can receive; ⌘Q quits. It can show in the menu bar, and open at login.
- Notifications report transfers, with their own actions, and VoiceOver announces what changes while Dodavo is in front.
- **Chat** (⇧⌘M, the toolbar, and the Chat button of the Incoming Transfer window and of each row in Transfers) has one window for each person. The conversation is a table that VoiceOver reads message by message; a message that arrives while that window is in front is announced, and otherwise its notification tells of it.
- Received items get `com.apple.quarantine`, as downloads do. Settings and the Contact ID are in `~/Library/Application Support/Dodavo`.
- Folder names that Windows cannot take (`:`, `?`, …) arrive changed on Windows and as they were on another Mac; items that only mean something to macOS are left out. Finder tags and other extended attributes do not travel.

Building: `macos/build.sh`, which needs Xcode's command line tools and Rust (`rustup`), writes `target/macos/Dodavo.app`. `macos/release.sh` makes the final release in one command: tests, build, signing with a Developer ID certificate, notarization and a check of the result. Two copies side by side: `open -n target/macos/Dodavo.app --args --profile alice`.

## The Android version

A native Android app in Kotlin, with Jetpack Compose and Material 3, on the same Rust core, which follows Android's terms and design and is built for TalkBack. It exchanges transfers with Windows and Mac copies in both directions.

- Android 8.0 or later, on phones and tablets (arm64 and x86_64).
- Two screens, **Recipients** and **Transfers**. A tap on a recipient opens Android's file picker; its menu has **Send a folder**, **Select** (for sending to several recipients at once, as a long press does), **Edit**, **Copy Contact ID**, **Block** and **Remove**. Other apps can share items to Dodavo.
- A picked file is read where it is, for as long as its transfer lasts, also after a restart; a picked folder is packed straight from where it is. What another app shares is copied first, and the copy goes with its transfer.
- Received items go to `Download/Dodavo`. From recipients, transfers can start by themselves.
- An offer that arrives while Dodavo is not in front comes as a notification, with **Review** and **Decline**. While data moves, a notification shows the transfer. With **Receive in the background** on (off by default), Dodavo stays reachable while it is closed, also after a restart of the device.
- The Contact ID lives in the app's own storage and is not backed up; Android's backup takes the recipients and settings.
- Updates come from Google Play: the Android version has no updater of its own, and no Donate link (Google Play's rules for payments). Help, About and the licenses are in the app.

Building: `android/build.sh [--debug]`, which needs JDK 21, the Android SDK with NDK r30, `cargo-ndk` and the Rust targets `aarch64-linux-android` and `x86_64-linux-android`, writes the app bundle (or a debug APK) to `target/android/`, and checks its native library with `android/check-elf.sh`. `android/release.sh` makes the final release in one command, signed with the upload key; `android/play/` has what Play Console asks for. `android/test-on-device.sh` runs the core's tests on a connected device.

## How connections work

Every installation has a **Contact ID** (a public key), which stays the same until you change it. You share that instead of an IP address: click **Copy My Contact ID** and send it to the other person, and they add you with **Add Recipient**.

Connections use [iroh](https://iroh.computer):

- **Direct:** computers connect directly through NAT and firewalls ("hole punching"), so no port forwarding is needed.
- **Local network:** computers on the same network find each other through mDNS.
- **Relay fallback:** when a direct path is impossible (strict corporate firewalls, some mobile networks), data goes through number0's free public relay servers. It stays end-to-end encrypted, so the relay cannot read it.
- **Integrity check:** each transfer is verified with a BLAKE3 checksum before the file is saved.

The public relays are free and need no registration, but they are rate-limited and meant for moderate use. If the app is used by many people, run your own `iroh-relay` on a small VPS and point the endpoint at it (`src/net/mod.rs`, `bind()`).

## Building

```
cargo build --release
```

The program is `target\release\Dodavo.exe`, a single file with no runtime dependencies (the C runtime is linked statically, see `.cargo\config.toml`), which also runs without being installed. Building needs the MSVC toolchain and the Windows SDK (for `rc.exe`). The version shown in the program's file properties comes from `Cargo.toml`. For 32-bit Windows, add `--target i686-pc-windows-msvc` (`rustup target add i686-pc-windows-msvc` once).

### Windows 7 and 8.1 builds

Since Rust 1.78, the normal `*-pc-windows-msvc` targets require Windows 10: their standard library imports Windows 8 functions, and a program built with them does not start on Windows 7. Windows 7 lives on in the Tier 3 targets `x86_64-win7-windows-msvc` and `i686-win7-windows-msvc`, which rustup does not ship. `installer\build.ps1` builds them from source with a pinned nightly toolchain and `-Zbuild-std` (`cargo +nightly-… build --release -Zbuild-std --target x86_64-win7-windows-msvc`), in its own `target\win7` directory. The stable toolchain stays the default for everything else, and the Windows 10/11 builds never touch the nightly.

Two rules keep every build starting on its oldest Windows version:

- Never import a Win32 function statically that is newer than Windows 7 SP1. Look it up at run time with `GetProcAddress` and fall back, as `src\ui\window.rs` does for the per-monitor DPI functions (Windows 10 version 1607) and `src\ui\theme.rs` for dark mode. The `windows` and `windows-sys` crates bind some functions to DLLs that only exist from Windows 8 on (for example `CoTaskMemFree` to `combase.dll`); `src\ui\shell.rs` declares such functions itself.
- `installer\check-imports.ps1` reads an executable's import table and fails on anything newer than its Windows baseline (`-Profile win10` or `win7`). `build.ps1` runs it on every build before packaging; run it yourself after changing Win32 calls or dependencies:

```
powershell -ExecutionPolicy Bypass -File installer\check-imports.ps1 target\release\Dodavo.exe
```

The import check makes sure that these builds load on Windows 7.

### Installer

For distribution there is an [Inno Setup](https://jrsoftware.org/isinfo.php) installer. It needs Inno Setup 6.6 or later (`winget install JRSoftware.InnoSetup`):

```
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

This builds the program four times (x64 and x86, each for Windows 10 and later with stable Rust and for Windows 7 SP1 to 8.1 with the pinned nightly, see above), checks their imports, and writes `target\installer\Dodavo-<version>-setup.exe`, plus a `.sha256` file to publish next to it. Add `-Test` to run `cargo test` first, and `-SkipWin7` to build only the Windows 10 programs and an installer that requires Windows 10. The installer needs the 32-bit edition of Inno Setup, whose setup program also runs on 32-bit Windows.

The four programs are also left in `target\installer\staging\<x64|x86|x64-win7|x86-win7>\Dodavo.exe`, so a single build can be copied to a test machine without running Setup.

Before building, `build.ps1` runs `installer\licenses.ps1`, which writes `res\third-party-licenses.txt`: the licenses of every crate compiled into the program, which their licenses (MIT, Apache-2.0, BSD, ISC, MPL-2.0 and others) require to go with every copy, and at the end those of the Rust standard library, which every Rust program contains. The program shows the file in Help → License, and Setup installs it as `THIRD-PARTY-LICENSES.txt` next to the program, together with `LICENSE.txt` (both with Windows line ends, for Notepad on older Windows). The script needs [cargo-about](https://github.com/EmbarkStudios/cargo-about) (`cargo install cargo-about --locked --features cli`) and reads `installer\about.toml` (which licenses are accepted) and `installer\about.hbs` (the layout). For the standard library it reads the `COPYRIGHT-library.html` of the stable toolchain and of the pinned nightly, and their `rust-src` (`library\Cargo.lock`), which it installs with rustup. The file is committed and only rewritten when its text changes, so a plain `cargo build` needs no extra tool; after changing dependencies or toolchains, run the script and commit the file together with `Cargo.lock`.

- Setup carries all four builds (about 20 MB) and installs the one for the running Windows: by version (`MinVersion`/`OnlyBelowVersion: 10.0`) and by install mode (64-bit on x64 and on Windows 11 on ARM, 32-bit otherwise).
- By default Dodavo is installed for the current user (`%LOCALAPPDATA%\Programs\Dodavo`) without administrator rights. Setup also offers to install for all users.
- Setup runs in English, Croatian, Serbian (Latin) or Slovenian, following the Windows display language. Croatian and Serbian come from `installer\languages`, which are adapted copies of the user-contributed Inno Setup translations.
- Running a newer setup upgrades in place. Setup closes a running Dodavo when you choose Install, and the uninstaller after you confirm; running transfers continue at its next start.
- Uninstalling removes the program, its shortcuts, its entries in File Explorer's menus and the "Start with Windows" entry. It keeps `%APPDATA%\Dodavo`, so a reinstall keeps the same Contact ID.
- For unattended installs, use `/VERYSILENT /CURRENTUSER` (or `/ALLUSERS` from an elevated prompt).

**Code signing.** Unsigned downloads trigger SmartScreen warnings. When a certificate is configured, the script signs the program, the setup and the uninstaller with `signtool` from the Windows SDK and timestamps the signatures:

| Environment variable | Parameter | Meaning |
| --- | --- | --- |
| `DODAVO_SIGN_THUMBPRINT` | `-CertificateThumbprint` | SHA-1 thumbprint of a certificate in the Windows certificate store (also hardware tokens) |
| `DODAVO_SIGN_PFX` | `-PfxPath` | a `.pfx` file, used if no thumbprint is set |
| `DODAVO_SIGN_PFX_PASSWORD` | `-PfxPassword` | password of the `.pfx` file |
| `DODAVO_SIGN_TIMESTAMP_URL` | `-TimestampUrl` | timestamp server (default `http://timestamp.digicert.com`) |

If none is set, the installer is built unsigned, which is fine for testing; [System requirements](#system-requirements) describes the warnings Windows then shows and how to verify the file.

### Releasing an update

Installed copies look for new versions on GitHub: once a day they fetch `https://github.com/hkatic/dodavo/releases/latest/download/update.json`, the update manifest attached to the latest published release (drafts and pre-releases are skipped by GitHub's `latest`). The manifest names the version, the installer's URL, size and SHA-256, the release notes per language, and an Ed25519 signature over those facts, made with a key that is kept outside the repository; the matching public key is compiled into the program (`src\update\keys.rs`), so a tampered manifest or installer is never run.

A release therefore consists of three files on the GitHub release `v<version>`: `Dodavo-<version>-setup.exe`, its `.sha256`, and `update.json`. The Mac version adds three of its own, `Dodavo-<version>-macos.dmg`, its `.sha256`, and `update-macos.json`, made by `macos/make-dmg.sh` and `macos/publish.sh`, and looks for its own manifest in the same way, signed so that it never verifies on Windows. **Every release has to carry both manifests**, because each system reads the latest release: without its manifest, that system's check fails. A release for one system only carries the other's manifest of the version that system is on. Both scripts add their files to the same draft, which goes public when it has all of them. The steps:

1. Set the new version in `Cargo.toml` and `crates/dodavo-ffi/Cargo.toml`, and write the release notes as plain text in `release-notes\<version>\en.txt` (required), `hr.txt`, `sr-Latn.txt` and `sl.txt`; the program shows the user's language and falls back to English.
2. Commit, tag `v<version>`, and build the installer with `installer\build.ps1`.
3. Run `installer\publish.ps1` (with the signing key in `DODAVO_UPDATE_KEY_FILE` or `-KeyFile`). It builds the release tool (`cargo build --release --features release-tool --bin dodavo-release`), writes and verifies `target\installer\update.json`, and with `-Publish` creates the GitHub release as a draft with `gh` and publishes it after a confirmation. Without `gh`, upload the three files by hand.

`dodavo-release keygen` makes the key pair once; `dodavo-release verify` checks a manifest against an installer and the built-in keys. How Dodavo and Setup hand over to each other is described in `installer\Dodavo.iss` (`[Code]`) and `src\update\mod.rs`.

## Development

- `cargo test` runs the unit tests and an end-to-end transfer test between two endpoints in one process. That test needs internet access for address lookup.
- `--send <paths>` offers items for sending, as File Explorer's menus do: everything after it is a path, and a running instance takes them.
- `--profile <name>` uses a separate identity and settings folder, so two instances can run side by side: `Dodavo.exe --profile alice` and `--profile bob`.
- The environment variable `DODAVO_THEME=dark` or `DODAVO_THEME=light` overrides the system app mode, for testing. Likewise, `DODAVO_LANG=en`, `hr`, `sr-Latn` or `sl` overrides the language, and `DODAVO_LEGACY_DPI=1` makes the program scale as on Windows 7 and 8 (system DPI only, no per-monitor functions).
- To test updates against a local web server, `DODAVO_UPDATE_URL` replaces the manifest URL (plain `http://` is allowed) and `DODAVO_UPDATE_KEY` replaces the trusted public keys (hex, several separated by `;`), for example with a key pair made by `dodavo-release keygen`. `--updated` is the argument Setup starts Dodavo with after an update.
- Settings and the identity key are stored in `%APPDATA%\Dodavo` (an existing `%APPDATA%\QuickAndEasyFileTransfer` folder from before the rename is moved there automatically).

### Source layout

| Path | Purpose |
| --- | --- |
| `src/net/` | iroh endpoint, transfer protocol, sending and receiving |
| `src/view.rs` | what a transfer shows and what can be done with it, for the Mac and Android interfaces so far |
| `src/platform/` | what differs between the systems, below the interface |
| `crates/dodavo-ffi/` | the state of the Mac and Android programs, and what Swift and Kotlin call |
| `macos/` | the Mac interface in Swift, its bundle, and scripts to build and publish it |
| `crates/dodavo-android/` | the library the Android app loads (JNI), and the Android string resources made from `src/i18n/android` |
| `android/` | the Android app in Kotlin (Gradle project), and scripts to build, check and release it |
| `src/source.rs` | what a send reads: paths, and Android's documents, which only the app can open |
| `src/archive.rs` | zipping and unzipping folders (rejects unsafe paths), safe file names |
| `src/i18n/` | user interface translations, one string table per language |
| `src/app.rs` | application state; passes network and update events to the UI thread |
| `src/update/` | update check and download: manifest, version comparison, signature check, handing over to Setup |
| `src/bin/dodavo-release.rs` | release tool (signing key, update manifest), built with `--features release-tool` |
| `release-notes/` | release notes per version and language, embedded in the update manifest |
| `src/ui/window.rs` | small framework over raw Win32 windows (DPI scaling, fonts, modal loops) |
| `src/ui/theme.rs` | light/dark mode for title bar, menus, and controls |
| `src/ui/*.rs` | main window, incoming dialog, progress, recipient, settings and update dialogs, About and License, tray |
| `LICENSE`, `res/third-party-licenses.txt` | the GNU GPL v3, and the licenses of the crates and the Rust standard library built into the program (generated, see "Installer") |
| `installer/` | Inno Setup script, build script, publish script, import check and Setup translations |

## Privacy

[PRIVACY.md](PRIVACY.md) says what Dodavo keeps and what others see of it. The developer runs no server and receives nothing.

## License

Copyright © 2026 Hrvoje Katić

Dodavo is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See [LICENSE](LICENSE) for the full text.

Dodavo contains open-source components by other authors, each under its own license; they are listed with their licenses in [res/third-party-licenses.txt](res/third-party-licenses.txt).
