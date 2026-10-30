# Dodavo

*Dodavo* comes from the Croatian *dodati*, "to hand over", and is pronounced "doh-DAH-voh".

Send files and folders directly from one Windows computer to another. You don't need an account, a cloud upload, a ZIP file you made yourself, or a link.

- **Send File / Send Folder** to a person in your recipient list (from the File menu, the right-click menu, the buttons, or by dragging items onto a person).
- Folders are zipped automatically, sent, and unpacked on the recipient's computer.
- The recipient sees an *Incoming Transfer* window with the name and size of the item and whether there is enough free disk space for it (folders need room for the download and for unpacking). They can accept or decline and choose where to save. The last location is remembered, and the default is the Downloads folder.
- Settings let you choose "Always save to this folder" and "Start transfers automatically" (only for people in your recipient list; saved to the chosen folder, or to Downloads).
- The progress window shows how much has been transferred, the total size, the current speed and the time remaining. Closing it lets the transfer continue in the background or cancels it. When a file or folder has been received, Dodavo offers to open it. Both questions have a "Don't ask me again" option, and both can be changed again in Settings.
- A classic Win32 interface that follows the Windows light/dark app mode.
- Minimizes to the notification area so files can be received in the background, with an optional "Start with Windows" and "Always start minimized".
- Full help for keyboard and screen reader users (Help → View Help, or F1), in every supported language.
- Available in English, Croatian, Serbian (Latin) and Slovenian. It follows the Windows display language, and you can choose another one in Settings.

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

The program is `target\release\Dodavo.exe`, a single file with no runtime dependencies, which also runs without being installed. Building needs the MSVC toolchain and the Windows SDK (for `rc.exe`). The version shown in the program's file properties comes from `Cargo.toml`.

### Installer

For distribution there is an [Inno Setup](https://jrsoftware.org/isinfo.php) installer. It needs Inno Setup 6.6 or later (`winget install JRSoftware.InnoSetup`):

```
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

This builds the release program and writes `target\installer\Dodavo-<version>-setup.exe`, plus a `.sha256` file to publish next to it. Add `-Test` to run `cargo test` first.

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

If none is set, the installer is built unsigned.

## Development

- `cargo test` runs the unit tests and an end-to-end transfer test between two endpoints in one process. That test needs internet access for address lookup.
- `--profile <name>` uses a separate identity and settings folder, so two instances can run side by side: `Dodavo.exe --profile alice` and `--profile bob`.
- The environment variable `DODAVO_THEME=dark` or `DODAVO_THEME=light` overrides the system app mode, for testing. Likewise, `DODAVO_LANG=en`, `hr`, `sr-Latn` or `sl` overrides the language.
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
| `installer/` | Inno Setup script, build script and Setup translations |
