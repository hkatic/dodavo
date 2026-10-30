# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Dodavo is a Windows-only Rust app (classic Win32 UI) for sending files and folders directly between computers over [iroh](https://iroh.computer). Peers are identified by a permanent **Contact ID** (the iroh public key), not an IP address. See README.md for the user-facing feature list.

## Commands

- Build: `cargo build --release` → `target\release\Dodavo.exe`. Needs the MSVC toolchain and Windows SDK (`rc.exe`); `build.rs` embeds `res/app.rc`, `res/app.manifest` and `res/app.ico` via `embed-resource`. `.cargo/config.toml` links the C runtime statically for every target. 32-bit: `cargo build --target i686-pc-windows-msvc`.
- Installer: `powershell -ExecutionPolicy Bypass -File installer\build.ps1` → `target\installer\Dodavo-<version>-setup.exe` + `.sha256`. Needs Inno Setup 6.6+ (32-bit edition). It builds four programs: x64 and x86 with stable Rust for Windows 10+, and x64 and x86 for Windows 7 SP1–8.1 with the Tier 3 `*-win7-windows-msvc` targets on a nightly pinned in `build.ps1` (`-Zbuild-std`, own `target\win7` dir). `-SkipWin7` builds a Windows 10-only installer. Setup picks the build by Windows version and install mode (`[Files]` in `Dodavo.iss`). Signs with signtool when `DODAVO_SIGN_THUMBPRINT` or `DODAVO_SIGN_PFX` is set (see README). The version comes from `Cargo.toml` (`build.rs` passes it to `res/app.rc`).
- Import check: `powershell -ExecutionPolicy Bypass -File installer\check-imports.ps1 <exe> [-Profile win7]` fails when an exe imports a DLL or function newer than its Windows baseline. `build.ps1` runs it on every build; run it after touching Win32 calls or dependencies.
- Test everything: `cargo test`
- Single test: `cargo test <name>`, e.g. `cargo test end_to_end` or `cargo test archive::`
- `net::tests::end_to_end` starts two real iroh endpoints in one process and needs internet access (address lookup / relays). It can take up to ~90 s before timing out.
- Run two instances side by side: `Dodavo.exe --profile alice` and `Dodavo.exe --profile bob` (separate identity and settings). `--minimized` starts in the tray.
- Force theme for testing: set `DODAVO_THEME=dark` or `DODAVO_THEME=light`. Force language: `DODAVO_LANG=en|hr|sr-Latn|sl`. `DODAVO_LEGACY_DPI=1` uses the Windows 7/8 DPI code path (system DPI only) on a current Windows.
- Debug builds keep a console window; release builds use the `windows` subsystem. A panic hook shows a message box.

## Architecture

Two threads that never share UI state:

1. **UI thread** (`main.rs` message loop): owns all windows and the `App` state in `app.rs`, stored in a `thread_local!`. Access it only through `app::with(|a| …)`; do not create windows or show message boxes inside that closure, because they run nested message loops and would re-borrow the `RefCell`.
2. **Network thread** (`net::start`): a tokio multi-thread runtime that owns the iroh `Endpoint`.

They communicate in two directions:
- UI → net: `net::Command` (`Send`, `Respond`, `Cancel`) over an unbounded mpsc channel (`app::net_send`).
- net → UI: `net::Event` is pushed by `app::event_sink` into a global `EVENTS` queue, which then posts `WM_APP_NET` to the main window. `app::process_events` drains the queue on the UI thread. If a target window is busy (for example, a modal message box is open), the handler returns the event, which is requeued and retried after 250 ms by a timer. Keep that contract when adding events. `Progress` events may be dropped; the others must not be.

Transfer ids come from `net::next_transfer_id()` and are used in both directions. `App.transfers` maps id to progress window, and `App.offers` maps id to incoming-offer window.

### Networking (`src/net/`)

- `protocol.rs`: ALPN `dodavo/1`. Each transfer uses one connection with one bidirectional stream: `Offer` → `Accept|Decline` → raw bytes → `Done{blake3 hash}` → `Result`. Control messages are postcard-encoded with a u32 LE length prefix, capped at 64 KiB. Changing `Msg`/`Offer` breaks compatibility with existing installs.
- `send.rs` / `receive.rs`: one task per transfer. The receiver parks a `oneshot` in `Shared.pending` until the UI answers with `Command::Respond`. Cancellation uses `CancelToken`, which works both in async code and as an `AtomicBool` flag for blocking zip/unzip in `spawn_blocking`. A cancel closes the connection with `CLOSE_CANCELLED` so the peer can report `CancelledByPeer`; a local failure closes it with `CLOSE_FAILED` so the peer reports a failure instead. A checksum mismatch is reported over the stream (`Result{ok:false}`) and the connection is left open until the sender has read it.
- Received files are written as `<name>.part` and renamed at the end. Folders are received as a temp zip and extracted into a unique destination. A `Cleanup` drop guard removes partial output on failure.
- `bind()` in `net/mod.rs` configures the endpoint (n0 preset relays + mDNS). This is where a self-hosted relay would be configured.

### Other modules

- `archive.rs`: folder zipping/unzipping, which rejects unsafe paths. It also provides `sanitize_file_name` and `unique_path`. Any name coming from a peer must pass through these.
- `i18n/`: translations. `mod.rs` lists every key in the `strings!` macro; each language (`en.rs`, `hr.rs`, `sr_latn.rs`, `sl.rs`) is one `Strings` const, so a missing translation fails to compile. Use `tr!(key)` / `tr!(key, name = value)` for `{name}` placeholders and `trn!(key, n, …)` for number-dependent text (CLDR plural categories; Slovenian has a dual). Build sentences as whole strings per variant (no "file"/"folder" word splicing), and keep peer names out of positions that need grammatical case (use "– primatelj: {name}" or parentheses). Tests check placeholders and that access keys are unique per window/menu; add new controls with access keys to the groups in `access_keys_are_unique`. The language comes from `DODAVO_LANG`, then the `language` setting, then the Windows display language (English fallback), and is fixed for the process lifetime.
- Help: `res/help/<lang code>.html` (one page per language, plus the shared `help.css`) is built in by `ui/help.rs`, written to `%TEMP%\Dodavo\help` and opened in the default browser in the current UI language (Help → View Help, or F1 in any window via `main_window::translate`). Pages follow WCAG 2.2: one `<h1>`, no skipped heading levels, landmarks, a skip link, `<kbd>` keys, and keyboard/screen reader instructions for every task. When you change a feature or a UI string, update all four pages. Tests check that the pages have identical ids and heading structure, that links resolve, and that they quote the program's current texts.
- `config.rs`: JSON settings and contacts in `%APPDATA%\Dodavo[\profile]`, including migration from the old `QuickAndEasyFileTransfer` folder. A damaged `config.json` is moved aside (`config.json.bad`) and reported at startup rather than silently replaced. `identity.rs` stores the secret key there and refuses to overwrite an existing but unreadable or damaged `secret.key`.
- `ui/window.rs`: a small framework over raw Win32. Each window owns a `Handler` trait object. Control rects are in 96-DPI units and scaled automatically. It handles fonts, DPI changes, theming, and modal loops (`run_modal`). Use `with_handler::<T>` to reach a window's handler from outside. New dialogs should follow the existing ones (`settings.rs`, `recipient.rs`, `incoming.rs`). The per-monitor DPI functions (Windows 10 1607) are looked up at run time in `dpi_api()` with system-DPI fallbacks.
- `ui/main_window.rs::translate` does accelerators and Tab navigation (`IsDialogMessageW`) for the main loop.
- `ui/theme.rs`: light/dark handling for the title bar, menus and controls. `ui/tray.rs`: the notification-area icon and balloons. `ui/shell.rs`: shell integration (Start with Windows, etc.).
- Single instance per profile through a named mutex. A second launch posts `show_message_id()` to the existing main window. Every instance also holds `Local\DodavoRunning`, which is the installer's `AppMutex`; keep both names in step.
- `installer/`: `Dodavo.iss` (per-user by default, fixed `AppId`, never change it), `build.ps1`, and `languages/` with adapted Croatian and Serbian (Latin) Inno Setup translations (formal address, Windows terms). Setup must offer the same languages as `src/i18n`. When you add a language, add its `.isl` (official or vendored and reviewed) to `[Languages]`. The help's "Installing and uninstalling" section quotes Setup's texts.

Win32 calls mostly use `windows-sys`. The `windows` crate is used only for COM shell APIs. Strings go through `ui::wide` / `ui::from_wide_ptr`.

## Guidelines

- **Windows 7 SP1 is the oldest supported Windows.** Never import a Win32 function statically that is newer than Windows 7 SP1 (for example the `*ForDpi` functions, `SetThreadDescription`, anything in `combase.dll`, `shcore.dll` or an `l1-2-*` API set): the loader then refuses to start the program on older Windows. Resolve it with `GetModuleHandleW`/`GetProcAddress` and fall back (pattern: `dpi_api()` in `ui/window.rs`, `api()` in `ui/theme.rs`), and avoid `LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_SYSTEM32` for anything that must work on an unpatched Windows 7. The `windows`/`windows-sys` crates bind some functions to Windows 8 DLLs (`CoTaskMemFree` → `combase.dll`); declare those yourself against the old DLL, as `ui/shell.rs` does. Stable Rust itself only supports Windows 10+, so the Windows 7 builds use the Tier 3 win7 targets (see Commands). After changing Win32 calls or dependencies, run `installer\check-imports.ps1` on the release exe. Structs like `NOTIFYICONDATAW` are packed on 32-bit: don't take references to their fields.
- **Avoid `unsafe`.** Use it only when there is no safe alternative, which in practice means direct Win32/FFI calls. Keep each `unsafe` block as small as possible, and prefer safe wrappers (for example, the helpers in `ui/mod.rs` and `ui/window.rs`) or safe crate APIs over new raw calls.
- **Accessibility is a high priority.** Every feature must work fully with screen readers (NVDA, JAWS, Narrator) and with the keyboard alone:
  - Every control needs an accessible name. Put a visible label right before it in creation order, or set the name explicitly.
  - Tab order must be logical, Enter and Escape must work, and menu items need access keys and shortcuts.
  - Status changes such as progress, completion and errors must be perceivable without seeing the screen.
  - Don't rely on color alone. Owner-drawn or custom-themed controls must keep working with assistive technology.
- **Localization.** Whenever you add or change a user-facing string, update the translations for every supported language in the same change. Never leave a language behind or fall back to English. When adding a new language, or translating into any language, check terminology, grammar and style against trusted sources for that language. Examples are official language institutes and dictionaries, and the Microsoft Terminology/Language Portal for Windows UI terms. Use Windows' own established terms for that language (for example "Downloads", "notification area", "Settings") so the app is consistent with the system.
