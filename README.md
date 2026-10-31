# Dodavo

*Dodavo* comes from the Croatian *dodati*, "to hand over", and is pronounced "doh-DAH-voh".

Send files and folders directly from one Windows computer to another. You don't need an account, a cloud upload, a ZIP file you made yourself, or a link.

- **Send File / Send Folder** to a person in your recipient list (from the File menu, the right-click menu, the buttons, or by dragging items onto a person).
- Folders are zipped automatically, sent, and unpacked on the recipient's computer.
- The recipient sees an *Incoming Transfer* window with the name and size of the item and whether there is enough free disk space for it (folders need room for the download and for unpacking). They can accept or decline and choose where to save. The last location is remembered, and the default is the Downloads folder.
- Settings let you choose "Always save to this folder" and "Start transfers automatically" (only for people in your recipient list; saved to the chosen folder, or to Downloads).
- The progress window shows how much has been transferred, the total size, the current speed and the time remaining, with buttons to pause and resume the transfer (on either side) and to minimize it to the background. Closing it lets the transfer continue in the background or cancels it. When a file or folder has been received, Dodavo offers to open it. Both questions have a "Don't ask me again" option, and both can be changed again in Settings.
- The recipient list shows the state of the transfer with each person, and the Transfers menu (in the menu bar and on the notification area icon) lists the running transfers and brings their windows back, also from the background.
- A transfer survives a lost Internet connection, sleep, and even a restart of Dodavo on either computer: it waits and continues where it stopped as soon as both are online again, and the whole item is still checked with a checksum at the end. Dodavo asks whether to wait or to cancel; the answer can be made permanent in Settings.
- A classic Win32 interface that follows the Windows light/dark app mode.
- Minimizes to the notification area so files can be received in the background, with an optional "Start with Windows" and "Always start minimized". Notifications report finished, paused and interrupted transfers, also while their windows are hidden.
- Full help for keyboard and screen reader users (Help → View Help, or F1), in every supported language.
- Available in English, Croatian, Serbian (Latin) and Slovenian. It follows the Windows display language, and you can choose another one in Settings.

## System requirements

Windows 7 with Service Pack 1, Windows 8.1, Windows 10 and Windows 11, 32-bit and 64-bit (Windows on ARM runs the x64 or x86 build through its emulation). One setup program installs the build that fits the Windows version and architecture.

Windows 10 and 11 are what Dodavo is developed and tested on. Windows 7 and 8.1 are no longer supported by Microsoft and get a separate build (see [Building](#building)); it starts and runs there as far as can be checked without those systems, but it is provided as is. On Windows 7 without the SHA-2 code signing updates from 2019 (KB4474419 and KB4490628), Windows cannot verify SHA-256 signatures and shows the setup program as coming from an unknown publisher.

Dodavo carries its own list of trusted certificate authorities (the Mozilla root set bundled by iroh) for its encrypted connections to the relay and address lookup servers, so it does not depend on the Windows certificate store and connects even on a Windows 7 whose root certificates were never updated.

An unsigned build (see [Code signing](#installer)) is flagged when it is started: Windows 8.1 and later show SmartScreen's "Windows protected your PC" (choose **More info**, then **Run anyway**), and Windows 7 asks whether to run software whose publisher could not be verified. Verify the download first: `certutil -hashfile Dodavo-<version>-setup.exe SHA256` must match the published `.sha256` file.

## How connections work

Every installation has a permanent **Contact ID** (a public key). You share that instead of an IP address: click **Copy My Contact ID** and send it to the other person, and they add you with **Add Recipient**.

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

Windows 7 cannot be tested here; the import check proves that the program loads, and the builds are tried on Windows 11.

### Installer

For distribution there is an [Inno Setup](https://jrsoftware.org/isinfo.php) installer. It needs Inno Setup 6.6 or later (`winget install JRSoftware.InnoSetup`):

```
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

This builds the program four times (x64 and x86, each for Windows 10 and later with stable Rust and for Windows 7 SP1 to 8.1 with the pinned nightly, see above), checks their imports, and writes `target\installer\Dodavo-<version>-setup.exe`, plus a `.sha256` file to publish next to it. Add `-Test` to run `cargo test` first, and `-SkipWin7` to build only the Windows 10 programs and an installer that requires Windows 10. The installer needs the 32-bit edition of Inno Setup, whose setup program also runs on 32-bit Windows.

The four programs are also left in `target\installer\staging\<x64|x86|x64-win7|x86-win7>\Dodavo.exe`, so a single build can be copied to a test machine without running Setup.

- Setup carries all four builds (about 20 MB) and installs the one for the running Windows: by version (`MinVersion`/`OnlyBelowVersion: 10.0`) and by install mode (64-bit on x64 and on Windows 11 on ARM, 32-bit otherwise).
- By default Dodavo is installed for the current user (`%LOCALAPPDATA%\Programs\Dodavo`) without administrator rights. Setup also offers to install for all users.
- Setup runs in English, Croatian, Serbian (Latin) or Slovenian, following the Windows display language. Croatian and Serbian come from `installer\languages`, which are adapted copies of the user-contributed Inno Setup translations.
- Running a newer setup upgrades in place. Setup and the uninstaller ask you to close Dodavo first.
- Uninstalling removes the program, its shortcuts and the "Start with Windows" entry. It keeps `%APPDATA%\Dodavo`, so a reinstall keeps the same Contact ID.
- For unattended installs, use `/VERYSILENT /CURRENTUSER` (or `/ALLUSERS` from an elevated prompt).

**Code signing.** Unsigned downloads trigger SmartScreen warnings. When a certificate is configured, the script signs the program, the setup and the uninstaller with `signtool` from the Windows SDK and timestamps the signatures:

| Environment variable | Parameter | Meaning |
| --- | --- | --- |
| `DODAVO_SIGN_THUMBPRINT` | `-CertificateThumbprint` | SHA-1 thumbprint of a certificate in the Windows certificate store (also hardware tokens) |
| `DODAVO_SIGN_PFX` | `-PfxPath` | a `.pfx` file, used if no thumbprint is set |
| `DODAVO_SIGN_PFX_PASSWORD` | `-PfxPassword` | password of the `.pfx` file |
| `DODAVO_SIGN_TIMESTAMP_URL` | `-TimestampUrl` | timestamp server (default `http://timestamp.digicert.com`) |

If none is set, the installer is built unsigned, which is fine for testing; [System requirements](#system-requirements) describes the warnings Windows then shows and how to verify the file.

## Development

- `cargo test` runs the unit tests and an end-to-end transfer test between two endpoints in one process. That test needs internet access for address lookup.
- `--profile <name>` uses a separate identity and settings folder, so two instances can run side by side: `Dodavo.exe --profile alice` and `--profile bob`.
- The environment variable `DODAVO_THEME=dark` or `DODAVO_THEME=light` overrides the system app mode, for testing. Likewise, `DODAVO_LANG=en`, `hr`, `sr-Latn` or `sl` overrides the language, and `DODAVO_LEGACY_DPI=1` makes the program scale as on Windows 7 and 8 (system DPI only, no per-monitor functions).
- Settings and the identity key are stored in `%APPDATA%\Dodavo` (an existing `%APPDATA%\QuickAndEasyFileTransfer` folder from before the rename is moved there automatically).

### Source layout

| Path | Purpose |
| --- | --- |
| `src/net/` | iroh endpoint, transfer protocol, sending and receiving |
| `src/archive.rs` | zipping and unzipping folders (rejects unsafe paths), safe file names |
| `src/i18n/` | user interface translations, one string table per language |
| `src/app.rs` | application state; passes network events to the UI thread |
| `src/ui/window.rs` | small framework over raw Win32 windows (DPI scaling, fonts, modal loops) |
| `src/ui/theme.rs` | light/dark mode for title bar, menus, and controls |
| `src/ui/*.rs` | main window, incoming dialog, progress, recipient and settings dialogs, tray |
| `installer/` | Inno Setup script, build script, import check and Setup translations |
