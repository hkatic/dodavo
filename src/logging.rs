//! The log: `Dodavo.log` in the data folder, which users send to the
//! developers when something went wrong. It is always kept, holds at most
//! about 2 MiB (the older half goes when it passes that), and never leaves
//! the device unless the user sends it.
//!
//! What goes in is for developers and must not tell anything of the user:
//! English text written here, numbers, kinds, outcomes, codes of the system's
//! errors, the transfer's number (`#12`), the beginning of its random token
//! (which both sides log, so that their logs can be put side by side) and
//! peers by an alias that changes at every start ([`peer`]). Never file or
//! folder names or paths, names of people or devices, chat text, Contact
//! IDs, IP addresses, the time zone (times are UTC) or whether the user
//! needs a screen reader or larger text. Errors are written by their type
//! ([`cause`]), never by their text, which may hold a path or what the
//! other side said; and every line goes through [`scrub`], which takes out
//! what slipped through anyway. What the networking libraries log is
//! kept from warnings on, scrubbed the same way (see [`Libraries`]).
//!
//! Lines go to a thread of their own, so that no caller waits for the disk
//! (a busy disk can hold up a small write for long). Before [`init`] they
//! wait in a small buffer; a process that never calls it (a second start
//! that hands its items to the running one, the tests) writes nothing.

use std::{
    cell::Cell,
    collections::HashMap,
    error::Error,
    fmt,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use iroh::EndpointId;

use crate::net::protocol::{Token, token_hex};

/// The log's file name in the data folder.
pub const FILE_NAME: &str = "Dodavo.log";
/// Past this size the older lines go...
const MAX_SIZE: u64 = 2 * 1024 * 1024;
/// ...and this much of the newest stays.
const KEEP: u64 = 1024 * 1024;
/// After a trim that failed (a viewer that maps the file), how much more is
/// written before it is tried again.
const TRIM_AGAIN_AFTER: u64 = 256 * 1024;
/// The lines that wait for the writer, at most.
const QUEUE: usize = 4096;
/// The lines kept before [`init`], at most.
const EARLY: usize = 200;
/// The longest line, in characters.
const LINE_MAX: usize = 1000;
/// How a line ends: Notepad on Windows 7 knows no other end.
#[cfg(windows)]
const EOL: &[u8] = b"\r\n";
#[cfg(not(windows))]
const EOL: &[u8] = b"\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
        }
    }
}

/// Writes a line of the log at the error level (see [`write`]).
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)+) => {
        $crate::logging::write($crate::logging::Level::Error, module_path!(), format_args!($($arg)+))
    };
}

/// Writes a line of the log at the warning level (see [`write`]).
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)+) => {
        $crate::logging::write($crate::logging::Level::Warn, module_path!(), format_args!($($arg)+))
    };
}

/// Writes a line of the log at the info level (see [`write`]).
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)+) => {
        $crate::logging::write($crate::logging::Level::Info, module_path!(), format_args!($($arg)+))
    };
}

/// Writes a line of the log at the debug level, which only `DODAVO_LOG=debug`
/// keeps (see [`write`]).
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)+) => {
        $crate::logging::write($crate::logging::Level::Debug, module_path!(), format_args!($($arg)+))
    };
}

/// How much is kept: 0 as usual, 1 with `DODAVO_LOG=debug` (our own debug
/// lines and the libraries' from debug on), 2 with `DODAVO_LOG=trace`.
fn verbosity() -> u8 {
    static VERBOSITY: OnceLock<u8> = OnceLock::new();
    *VERBOSITY.get_or_init(|| {
        match std::env::var("DODAVO_LOG")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "debug" => 1,
            "trace" => 2,
            _ => 0,
        }
    })
}

/// Whether lines of `level` are kept.
pub fn enabled(level: Level) -> bool {
    level
        <= if verbosity() >= 1 {
            Level::Debug
        } else {
            Level::Info
        }
}

/// A line for the writer, or a question whether it has written all before.
enum Line {
    Text(String),
    Flush(SyncSender<()>),
}

struct Logger {
    lines: SyncSender<Line>,
    path: PathBuf,
    /// Lines that found the queue full.
    dropped: Arc<AtomicU64>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();
/// The lines before [`init`].
static BEFORE: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    /// Set while this thread writes a line, so that a panic in the middle of
    /// it (in a `Display` of what it writes) writes none.
    static IN_LOG: Cell<bool> = const { Cell::new(false) };
}

/// Marks this thread as writing a line, while it lives; `None` if it is
/// already, or if the thread is ending.
struct Writing;

impl Writing {
    fn begin() -> Option<Writing> {
        IN_LOG
            .try_with(|busy| (!busy.replace(true)).then_some(Writing))
            .ok()
            .flatten()
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        let _ = IN_LOG.try_with(|busy| busy.set(false));
    }
}

/// Starts the log in `data_dir`, with a header that tells what the program
/// runs on: `ui` names the user interface, `extra` adds lines of its own
/// (whether a profile is used, the install mode). Writes the lines that came
/// before. Only the first call counts. The path of the log, unless no thread
/// could be started for it.
pub fn init(data_dir: &Path, ui: &str, extra: &[String]) -> Option<PathBuf> {
    let mut before = lock(&BEFORE);
    if let Some(logger) = LOGGER.get() {
        return Some(logger.path.clone());
    }
    let _ = std::fs::create_dir_all(data_dir);
    let path = data_dir.join(FILE_NAME);
    let (lines, queue) = sync_channel(QUEUE);
    let dropped = Arc::new(AtomicU64::new(0));
    let writer = Writer::new(path.clone(), dropped.clone());
    std::thread::Builder::new()
        .name("log".into())
        .spawn(move || writer.run(queue))
        .ok()?;
    for text in header(ui, extra) {
        let _ = lines.try_send(Line::Text(line(Level::Info, "start", &text)));
    }
    for text in before.drain(..) {
        let _ = lines.try_send(Line::Text(text));
    }
    let _ = LOGGER.set(Logger {
        lines,
        path: path.clone(),
        dropped,
    });
    drop(before);
    install_libraries();
    Some(path)
}

/// Where the log is, once [`init`] has run.
pub fn path() -> Option<PathBuf> {
    LOGGER.get().map(|logger| logger.path.clone())
}

/// Waits at most `timeout` until all lines before are in the file: before
/// the program ends, and before the user is shown the file.
pub fn flush(timeout: Duration) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    // The writer cannot wait for itself.
    if std::thread::current().name() == Some("log") {
        return;
    }
    let (done, written) = sync_channel(1);
    if logger.lines.try_send(Line::Flush(done)).is_ok() {
        let _ = written.recv_timeout(timeout);
    }
}

/// What the header says of the program and of the system it runs on.
fn header(ui: &str, extra: &[String]) -> Vec<String> {
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let win7 = if cfg!(target_vendor = "win7") {
        ", Windows 7 build"
    } else {
        ""
    };
    let mut lines = vec![
        "========================================".to_string(),
        format!(
            "Dodavo {} ({ui}, {} {}{win7}, {build} build)",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        ),
    ];
    lines.extend(
        crate::platform::os_description()
            .into_iter()
            .map(|line| format!("system: {line}")),
    );
    if let Ok(count) = std::thread::available_parallelism() {
        lines.push(format!("processors: {count}"));
    }
    let mut set: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.starts_with("DODAVO_"))
        .collect();
    set.sort();
    if !set.is_empty() {
        lines.push(format!("environment sets {}", set.join(", ")));
    }
    lines.extend(extra.iter().cloned());
    lines.push(
        "times are UTC; peers appear as aliases that change at every start; \
         transfers as #number and the beginning of their token, which both sides log"
            .into(),
    );
    lines
}

/// Writes a line (see the macros [`log_info!`] and the others): `module` is
/// where it comes from (`module_path!()`).
pub fn write(level: Level, module: &str, args: fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let Some(_writing) = Writing::begin() else {
        return;
    };
    let message = scrub(&flatten(&args.to_string()), false);
    deliver(line(level, short_module(module), &message));
}

/// A line from the user interface of the Mac or of Android, which writes
/// only texts of its own (see `Call::Log` in the bridge).
pub fn external(level: &str, message: &str) {
    let level = match level {
        "error" => Level::Error,
        "warn" | "warning" => Level::Warn,
        "debug" => Level::Debug,
        _ => Level::Info,
    };
    write(level, "ui", format_args!("{message}"));
}

/// Writes what a panic says (see the panic hooks of the user interfaces),
/// and waits a moment for it to reach the file: the program may end next.
pub fn panic(info: &std::panic::PanicHookInfo<'_>) {
    {
        let Some(_writing) = Writing::begin() else {
            return;
        };
        let location = info.location().map_or_else(
            || "an unknown place".to_string(),
            |l| format!("{}:{}:{}", trim_location(l.file()), l.line(), l.column()),
        );
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let payload: String = scrub(&flatten(&payload), true).chars().take(500).collect();
        let message = format!("panicked at {location}: {payload}");
        deliver(line(Level::Error, "panic", &scrub(&message, false)));
    }
    flush(Duration::from_millis(300));
}

/// The place of a panic, without the folders of the computer that built the
/// program (its user's name): from the crate on for a library, from the
/// standard library's own folder for that, the last parts otherwise.
fn trim_location(file: &str) -> String {
    let file = file.replace('\\', "/");
    for marker in ["/registry/src/", "/git/checkouts/"] {
        if let Some(at) = file.find(marker) {
            let rest = &file[at + marker.len()..];
            // Past the folder of the index or of the checkout.
            return rest.split_once('/').map_or(rest, |(_, r)| r).to_string();
        }
    }
    if let Some(at) = file.find("/rustc/") {
        let rest = &file[at + "/rustc/".len()..];
        return rest.split_once('/').map_or(rest, |(_, r)| r).to_string();
    }
    let absolute = file.starts_with('/') || file.as_bytes().get(1) == Some(&b':');
    if absolute {
        let parts: Vec<&str> = file.split('/').filter(|p| !p.is_empty()).collect();
        return parts[parts.len().saturating_sub(3)..].join("/");
    }
    file
}

/// `dodavo_core::net::send` as `net::send`.
fn short_module(module: &str) -> &str {
    for prefix in ["dodavo_core::", "Dodavo::", "dodavo_"] {
        if let Some(rest) = module.strip_prefix(prefix) {
            return rest;
        }
    }
    module
}

/// A whole line: time, level, thread, where from and what.
fn line(level: Level, module: &str, message: &str) -> String {
    let thread = std::thread::current();
    format!(
        "{} {} [{}] {}: {}",
        timestamp(SystemTime::now()),
        level.label(),
        thread.name().unwrap_or("-"),
        module,
        message
    )
}

/// One line without breaks (so that nothing can fake a line), and not
/// longer than [`LINE_MAX`].
fn flatten(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(LINE_MAX + 8));
    let mut chars = text.chars().peekable();
    let mut count = 0;
    while let Some(c) = chars.next() {
        if count >= LINE_MAX {
            out.push('…');
            break;
        }
        match c {
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str(" | ");
            }
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
        count += 1;
    }
    out
}

fn deliver(text: String) {
    #[cfg(test)]
    capture(&text);
    if let Some(logger) = LOGGER.get() {
        logger.send(text);
        return;
    }
    let mut before = lock(&BEFORE);
    // `init` may have come meanwhile.
    if let Some(logger) = LOGGER.get() {
        drop(before);
        logger.send(text);
    } else if before.len() < EARLY {
        before.push(text);
    }
}

impl Logger {
    fn send(&self, text: String) {
        if self.lines.try_send(Line::Text(text)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// `2026-11-13T14:03:22.123Z`.
fn timestamp(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// The date of a day counted from 1970-01-01, as Howard Hinnant's
/// `civil_from_days` works it out.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The thread that writes the file.
struct Writer {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    /// No trim before the file is this large (see [`TRIM_AGAIN_AFTER`]).
    trim_from: u64,
    /// When to try again to open a file that could not be opened.
    open_again: Option<Instant>,
    dropped: Arc<AtomicU64>,
}

impl Writer {
    fn new(path: PathBuf, dropped: Arc<AtomicU64>) -> Writer {
        Writer {
            path,
            file: None,
            size: 0,
            trim_from: 0,
            open_again: None,
            dropped,
        }
    }

    fn run(mut self, queue: Receiver<Line>) {
        self.open();
        for next in queue {
            match next {
                Line::Text(text) => {
                    let dropped = self.dropped.swap(0, Ordering::Relaxed);
                    if dropped > 0 {
                        self.write(&line(
                            Level::Warn,
                            "logging",
                            &format!("{dropped} lines were left out: they came faster than the disk took them"),
                        ));
                    }
                    self.write(&text);
                }
                Line::Flush(done) => {
                    let _ = done.send(());
                }
            }
        }
    }

    /// Opens the file for appending: a line always goes at its end, also
    /// after a trim through another handle.
    fn open(&mut self) {
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(file) => {
                self.size = file.metadata().map_or(0, |m| m.len());
                self.file = Some(file);
                self.open_again = None;
                if self.size > MAX_SIZE {
                    self.trim();
                }
            }
            Err(_) => self.open_again = Some(Instant::now() + Duration::from_secs(10)),
        }
    }

    fn write(&mut self, text: &str) {
        if self.file.is_none() {
            if self.open_again.is_none_or(|at| Instant::now() < at) {
                return;
            }
            self.open();
        }
        let Some(file) = &mut self.file else {
            return;
        };
        let mut bytes = Vec::with_capacity(text.len() + EOL.len());
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(EOL);
        if file.write_all(&bytes).is_err() {
            self.file = None;
            self.open_again = Some(Instant::now() + Duration::from_secs(10));
            return;
        }
        self.size += bytes.len() as u64;
        if self.size > MAX_SIZE && self.size >= self.trim_from {
            self.trim();
        }
    }

    fn trim(&mut self) {
        match trim_file(&self.path) {
            Ok(size) => {
                self.size = size;
                self.trim_from = 0;
            }
            Err(_) => self.trim_from = self.size + TRIM_AGAIN_AFTER,
        }
    }
}

/// Keeps the newest [`KEEP`] bytes of the file, from the first whole line
/// in them on, after a line that says so. Through a handle of its own, as
/// the writer's appends: the file is never empty meanwhile.
fn trim_file(path: &Path) -> io::Result<u64> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let size = file.metadata()?.len();
    if size <= KEEP {
        return Ok(size);
    }
    file.seek(SeekFrom::Start(size - KEEP))?;
    let mut tail = Vec::with_capacity(KEEP as usize);
    (&mut file).take(KEEP).read_to_end(&mut tail)?;
    let start = tail
        .iter()
        .position(|&b| b == b'\n')
        .map_or(tail.len(), |at| at + 1);
    let mut marker = line(
        Level::Info,
        "logging",
        "older lines were removed to keep the log small",
    )
    .into_bytes();
    marker.extend_from_slice(EOL);
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&marker)?;
    file.write_all(&tail[start..])?;
    let size = (marker.len() + tail.len() - start) as u64;
    file.set_len(size)?;
    Ok(size)
}

/// The key of the aliases of this run (see [`peer`]).
fn peer_key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(rand::random)
}

/// How the log names a peer: `peer#4c1e9a`, the same all through a run and
/// another at the next start, which nobody can turn back into its Contact
/// ID.
pub fn peer(id: &EndpointId) -> String {
    alias(id.as_bytes())
}

/// [`peer`] of a Contact ID as text.
pub fn peer_str(id: &str) -> String {
    match crate::identity::parse_contact_id(&id.to_ascii_lowercase()) {
        Some(id) => peer(&id),
        None => alias(id.trim().to_ascii_lowercase().as_bytes()),
    }
}

fn alias(bytes: &[u8]) -> String {
    let hash = blake3::keyed_hash(peer_key(), bytes);
    let hash = hash.as_bytes();
    format!("peer#{:02x}{:02x}{:02x}", hash[0], hash[1], hash[2])
}

/// How the log names a transfer's token: its first 8 hex digits, which tell
/// it from the others and are the same in the other side's log.
pub fn token(token: &Token) -> String {
    token_hex(token)[..8].to_string()
}

/// What `<message>` stands for in [`cause`].
const MESSAGE: &str = "<message>";

/// An error for the log: the causes that are known by their type, after one
/// another, and `<message>` for the texts of our own on top of them (which
/// may hold a path, or what the other side said). For an I/O error its kind
/// and the system's code, for a connection how it ended and the codes, never
/// the reason that the other side gave.
pub fn cause(error: &anyhow::Error) -> String {
    let mut parts = Parts::default();
    for cause in error.chain() {
        if parts.add(cause) {
            break;
        }
    }
    parts.text()
}

/// [`cause`] of an error and its sources.
pub fn cause_dyn(error: &(dyn Error + 'static)) -> String {
    let mut parts = Parts::default();
    parts.walk(error);
    parts.text()
}

/// [`cause`] of any value that may be an error: what `session::lost` is
/// given.
pub fn cause_any(value: &dyn std::any::Any) -> String {
    use iroh::endpoint::{
        ClosedStream, ConnectionError, ReadError, ReadExactError, ReadToEndError, StoppedError,
        WriteError,
    };
    if let Some(error) = value.downcast_ref::<anyhow::Error>() {
        return cause(error);
    }
    macro_rules! typed {
        ($($t:ty),*) => {
            $(if let Some(error) = value.downcast_ref::<$t>() {
                return cause_dyn(error);
            })*
        };
    }
    typed!(
        io::Error,
        ConnectionError,
        ReadError,
        WriteError,
        ReadExactError,
        ReadToEndError,
        ClosedStream,
        StoppedError
    );
    MESSAGE.into()
}

/// An I/O error for the log: its kind and the system's code.
pub fn io(error: &io::Error) -> String {
    cause_dyn(error)
}

/// An error of reading JSON for the log: what kind, and where. Never its
/// text, which can quote what the file holds.
pub fn json(error: &serde_json::Error) -> String {
    format!(
        "JSON {:?} error at line {} column {}",
        error.classify(),
        error.line(),
        error.column()
    )
}

/// A connection that ended, for the log: how, with the codes, and the
/// reason only where it is one of ours.
pub fn connection_error(error: &iroh::endpoint::ConnectionError) -> String {
    use iroh::endpoint::ConnectionError;
    #[allow(unreachable_patterns)]
    match error {
        ConnectionError::VersionMismatch => "version mismatch".into(),
        ConnectionError::TransportError(e) => format!("transport error {:?}", e.code),
        ConnectionError::ConnectionClosed(close) => {
            format!("aborted by peer {:?}", close.error_code)
        }
        ConnectionError::ApplicationClosed(close) => {
            let code = u64::from(close.error_code);
            let name = match code {
                0 => "ok".to_string(),
                1 => "cancelled".to_string(),
                2 => "failed".to_string(),
                3 => "suspended".to_string(),
                code => format!("code {code}"),
            };
            match our_reason(&close.reason) {
                Some(reason) => format!("closed by peer ({name}, {reason})"),
                None => format!("closed by peer ({name})"),
            }
        }
        ConnectionError::Reset => "reset by peer".into(),
        ConnectionError::TimedOut => "timed out".into(),
        ConnectionError::LocallyClosed => "closed here".into(),
        ConnectionError::CidsExhausted => "connection IDs exhausted".into(),
        _ => "connection error".into(),
    }
}

/// The reason of a close where Dodavo gives it (see `net`); any other text
/// could be anything that the other side chose.
fn our_reason(reason: &[u8]) -> Option<&'static str> {
    const OURS: [&str; 12] = [
        "done",
        "exiting",
        "replaced",
        "dropped",
        "declined",
        "failed",
        "gone",
        "retry",
        "cancelled",
        "query",
        "finishing",
        "idle",
    ];
    OURS.into_iter().find(|ours| ours.as_bytes() == reason)
}

/// A dial that failed, for the log (see [`connection_error`]).
pub fn connect_error(error: &iroh::endpoint::ConnectError) -> String {
    use iroh::endpoint::ConnectError;
    match error {
        ConnectError::Connect { source, .. } => format!("connect: {}", library(source)),
        ConnectError::Connecting { source, .. } => format!("connecting: {}", connecting(source)),
        ConnectError::Connection { source, .. } => {
            format!("connection: {}", connection_error(source))
        }
        _ => "connect: other error".into(),
    }
}

fn connecting(error: &iroh::endpoint::ConnectingError) -> String {
    use iroh::endpoint::ConnectingError;
    match error {
        ConnectingError::ConnectionError { source, .. } => connection_error(source),
        other => library(other),
    }
}

/// An error of a library and its sources: their own texts, scrubbed, as
/// they hold no texts of ours.
fn library(error: &(dyn Error + 'static)) -> String {
    let mut parts = Parts {
        library: true,
        ..Parts::default()
    };
    parts.walk(error);
    parts.text()
}

/// The causes of an error, as [`cause`] writes them.
#[derive(Default)]
struct Parts {
    parts: Vec<String>,
    /// Past the first error of a known type: what follows are its sources,
    /// which hold no text of ours.
    library: bool,
}

impl Parts {
    fn push(&mut self, part: String) {
        if part == MESSAGE && self.parts.last().is_some_and(|last| last == MESSAGE) {
            return;
        }
        if part.is_empty() || self.parts.last() == Some(&part) {
            return;
        }
        self.parts.push(part);
    }

    fn text(&self) -> String {
        self.parts.join(": ")
    }

    fn walk(&mut self, error: &(dyn Error + 'static)) {
        let mut next = Some(error);
        while let Some(error) = next {
            if self.parts.len() >= 12 || self.add(error) {
                return;
            }
            next = error.source();
        }
    }

    /// Adds one cause; whether that told all, so that its sources are left.
    fn add(&mut self, error: &(dyn Error + 'static)) -> bool {
        use iroh::endpoint::{
            BindError, ClosedStream, ConnectError, ConnectWithOptsError, ConnectingError,
            ConnectionError, ReadError, ReadExactError, ReadToEndError, StoppedError, WriteError,
        };
        if let Some(e) = error.downcast_ref::<io::Error>() {
            let mut text = format!("{:?}", e.kind());
            if let Some(code) = e.raw_os_error() {
                text.push_str(&format!(" (os error {code})"));
            }
            self.push(text);
            self.library = true;
            if let Some(inner) = e.get_ref() {
                self.walk(inner);
            }
            return true;
        }
        if let Some(e) = error.downcast_ref::<ConnectionError>() {
            self.push(connection_error(e));
            return true;
        }
        if let Some(e) = error.downcast_ref::<ConnectError>() {
            self.push(connect_error(e));
            return true;
        }
        if let Some(e) = error.downcast_ref::<ConnectingError>() {
            self.push(connecting(e));
            return true;
        }
        if let Some(e) = error.downcast_ref::<tokio::task::JoinError>() {
            self.push(
                if e.is_panic() {
                    "task panicked"
                } else {
                    "task cancelled"
                }
                .into(),
            );
            return true;
        }
        if error.is::<tokio::time::error::Elapsed>() {
            self.push("timed out".into());
            return true;
        }
        if let Some(e) = error.downcast_ref::<serde_json::Error>() {
            self.push(json(e));
            return true;
        }
        if error.is::<crate::archive::Cancelled>() {
            self.push("cancelled".into());
            return true;
        }
        if let Some(e) = error.downcast_ref::<crate::net::session::Lost>() {
            self.push(format!("connection lost: {}", e.log));
            return true;
        }
        if let Some(e) = error.downcast_ref::<reqwest::Error>() {
            self.push(request_error(e));
            self.library = true;
            return false;
        }
        // Errors of the libraries with texts of their own, which say what
        // went wrong in their words; their sources follow.
        let known = error.is::<ReadError>()
            || error.is::<WriteError>()
            || error.is::<ReadExactError>()
            || error.is::<ReadToEndError>()
            || error.is::<ClosedStream>()
            || error.is::<StoppedError>()
            || error.is::<BindError>()
            || error.is::<ConnectWithOptsError>()
            || error.is::<zip::result::ZipError>()
            || error.is::<postcard::Error>()
            || error.is::<rustls::Error>();
        if known || self.library {
            self.push(scrub(&flatten(&error.to_string()), true));
            self.library = true;
        } else {
            self.push(MESSAGE.into());
        }
        false
    }
}

/// A request of the update that failed, by what kind of failure it was.
fn request_error(error: &reqwest::Error) -> String {
    let mut kinds = Vec::new();
    for (is, kind) in [
        (error.is_timeout(), "timeout"),
        (error.is_connect(), "connect"),
        (error.is_request(), "request"),
        (error.is_body(), "body"),
        (error.is_decode(), "decode"),
        (error.is_redirect(), "redirect"),
        (error.is_builder(), "builder"),
    ] {
        if is {
            kinds.push(kind.to_string());
        }
    }
    if let Some(status) = error.status() {
        kinds.push(format!("status {}", status.as_u16()));
    }
    format!("request failed ({})", kinds.join(", "))
}

/// Notes a failure that may repeat for long (a dial every few seconds, a
/// network that cannot be bound): a line for the first, for one that says
/// something else, and once an hour, with how many were left out.
#[derive(Debug, Default)]
pub struct Repeats {
    last: Option<(String, Instant)>,
    left_out: u32,
}

impl Repeats {
    pub const fn new() -> Repeats {
        Repeats {
            last: None,
            left_out: 0,
        }
    }

    /// Whether `what` gets a line, and how many like it were left out
    /// before it.
    pub fn note(&mut self, what: &str) -> Option<u32> {
        let now = Instant::now();
        match &self.last {
            Some((last, at))
                if last == what && now.duration_since(*at) < Duration::from_secs(3600) =>
            {
                self.left_out += 1;
                None
            }
            _ => {
                self.last = Some((what.to_string(), now));
                Some(std::mem::take(&mut self.left_out))
            }
        }
    }

    /// Forgets the last one: the next is a first again.
    pub fn clear(&mut self) {
        self.last = None;
        self.left_out = 0;
    }
}

/// What a line says of the lines left out before it (see [`Repeats`]):
/// nothing when there were none.
pub fn left_out(count: u32) -> String {
    match count {
        0 => String::new(),
        1 => " (after 1 more like the last)".into(),
        n => format!(" (after {n} more like the last)"),
    }
}

/// Takes out of a line what must not be in the log, wherever it came from:
/// IP and MAC addresses (by their kind: `<ip:lan>`, `<ip:public>`, ...),
/// what looks like a key or a Contact ID (`<id>`), the host and path of
/// URLs (but for a few that say nothing of the user), and anything from the
/// first path on (`<path…>`). `third_party` for lines of the libraries,
/// where any long run of hex digits is taken out, also of digits only (a
/// short ID can be).
pub fn scrub(text: &str, third_party: bool) -> String {
    let text = scrub_urls(text);
    let text = match path_start(&text) {
        Some(at) => format!("{}<path…>", &text[..at]),
        None => text,
    };
    scrub_words(&text, third_party)
}

/// Hosts that may stay in a URL: they say nothing of the user.
const PUBLIC_HOSTS: [&str; 6] = [
    "github.com",
    "api.github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "dns.iroh.link",
];

fn scrub_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let before = &rest[..at];
        let scheme_start = before
            .char_indices()
            .rev()
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let scheme = &before[scheme_start..];
        out.push_str(before);
        out.push_str("://");
        let after = &rest[at + 3..];
        if !scheme.starts_with(|c: char| c.is_ascii_alphabetic()) {
            rest = after;
            continue;
        }
        let end = after
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ',' | ';' | '|')
            })
            .unwrap_or(after.len());
        let url = &after[..end];
        let authority_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let authority = &url[..authority_end];
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = split_port(host_port);
        let bare = host.trim_end_matches('.').to_ascii_lowercase();
        if PUBLIC_HOSTS.contains(&bare.as_str()) {
            out.push_str(host);
        } else if !host.is_empty() {
            out.push_str("<host>");
        }
        out.push_str(port);
        if authority_end < url.len() {
            out.push_str("/…");
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// `host:443` as `host` and `:443`.
fn split_port(host_port: &str) -> (&str, &str) {
    if host_port.starts_with('[')
        && let Some(close) = host_port.find(']')
    {
        return host_port.split_at(close + 1);
    }
    match host_port.rfind(':') {
        Some(at)
            if !host_port[..at].contains(':')
                && host_port[at + 1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            host_port.split_at(at)
        }
        _ => (host_port, ""),
    }
}

/// The folders where paths that hold names begin on the Mac, Android and
/// Linux.
const POSIX_ROOTS: [&str; 13] = [
    "/Users/",
    "/home/",
    "/storage/",
    "/sdcard/",
    "/data/",
    "/Volumes/",
    "/private/",
    "/var/",
    "/tmp/",
    "/mnt/",
    "/media/",
    "/root/",
    "/run/",
];

/// Where the first path begins: `C:\`, `C:/`, `\\server`, `\\?\`, one of the
/// [`POSIX_ROOTS`] or `~/`.
fn path_start(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let boundary = |i: usize| i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
    for i in 0..b.len() {
        match b[i] {
            c if c.is_ascii_alphabetic()
                && b.get(i + 1) == Some(&b':')
                && matches!(b.get(i + 2), Some(b'\\' | b'/'))
                && boundary(i) =>
            {
                return Some(i);
            }
            b'\\'
                if b.get(i + 1) == Some(&b'\\')
                    && b.get(i + 2).is_some_and(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, b'?' | b'.' | b'$')
                    }) =>
            {
                return Some(i);
            }
            b'/' if boundary(i) && POSIX_ROOTS.iter().any(|root| text[i..].starts_with(root)) => {
                return Some(i);
            }
            b'~' if b.get(i + 1) == Some(&b'/') && boundary(i) => return Some(i),
            _ => {}
        }
    }
    None
}

fn scrub_words(text: &str, third_party: bool) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let word_char = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        let boundary = i == 0 || !(word_char(b[i - 1]) || matches!(b[i - 1], b'.' | b'-' | b':'));
        if b[i] == b'['
            && let Some(close) = text[i..].find(']').filter(|&c| c <= 64)
        {
            let inner = &text[i + 1..i + close];
            if let Some(ip) = ipv6_text(inner) {
                out.push('[');
                out.push_str(&ip);
                out.push(']');
                i += close + 1;
                continue;
            }
        }
        let starts_v6 = b[i] == b':' && b.get(i + 1) == Some(&b':');
        if boundary && (b[i].is_ascii_alphanumeric() || starts_v6) {
            // An address: digits, hex, dots, colons, a zone after `%`.
            let end = run_end(b, i, |c| {
                c.is_ascii_alphanumeric() || matches!(c, b':' | b'.' | b'%' | b'-')
            });
            let candidate = text[i..end].trim_end_matches(['.', ':', '-']);
            if let Some(address) = address_text(candidate) {
                out.push_str(&address);
                i += candidate.len();
                continue;
            }
        }
        if boundary && b[i].is_ascii_alphanumeric() {
            // A host: three labels or more, the last one letters.
            let end = run_end(b, i, |c| {
                c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-')
            });
            let candidate = text[i..end].trim_end_matches(['.', '-']);
            if is_host(candidate) {
                if PUBLIC_HOSTS.contains(&candidate.to_ascii_lowercase().as_str()) {
                    out.push_str(candidate);
                } else {
                    out.push_str("<host>");
                }
                i += candidate.len();
                continue;
            }
        }
        if b[i].is_ascii_alphanumeric() {
            let end = run_end(b, i, |c| c.is_ascii_alphanumeric());
            let word = &text[i..end];
            if is_id(word, third_party) {
                out.push_str("<id>");
            } else {
                out.push_str(word);
            }
            i = end;
            continue;
        }
        // Whatever else, as it is (also a character of several bytes).
        let c = text[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn run_end(b: &[u8], start: usize, take: impl Fn(u8) -> bool) -> usize {
    b[start..]
        .iter()
        .position(|&c| !take(c))
        .map_or(b.len(), |n| start + n)
}

/// An IP address (IPv4 with its port too) or a MAC address, as the log
/// writes it.
fn address_text(candidate: &str) -> Option<String> {
    if candidate.len() < 7 {
        // Shorter than any address but `::1`, which `ipv6_text` takes.
        return ipv6_text(candidate);
    }
    if let Ok(address) = candidate.parse::<SocketAddrV4>() {
        return Some(format!(
            "{}:{}",
            ip_kind(IpAddr::V4(*address.ip()), &address.ip().to_string()),
            address.port()
        ));
    }
    if let Ok(ip) = candidate.parse::<Ipv4Addr>() {
        return Some(ip_kind(IpAddr::V4(ip), candidate));
    }
    if let Some(ip) = ipv6_text(candidate) {
        return Some(ip);
    }
    is_mac(candidate).then(|| "<mac>".into())
}

/// An IPv6 address, with or without its zone, as the log writes it.
fn ipv6_text(candidate: &str) -> Option<String> {
    let bare = candidate.split('%').next().unwrap_or(candidate);
    if bare.matches(':').count() < 2 {
        return None;
    }
    let ip = bare.parse::<Ipv6Addr>().ok()?;
    Some(ip_kind(IpAddr::V6(ip), bare))
}

/// `<ip:lan>`, `<ip:public>`, `<ip:loopback>` or `<ip:link-local>`; an
/// address that is none (`0.0.0.0`, `::`) as it is.
fn ip_kind(ip: IpAddr, text: &str) -> String {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        ip => ip,
    };
    let kind = match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => return text.to_string(),
        IpAddr::V4(v4) if v4.is_loopback() => "loopback",
        IpAddr::V4(v4) if v4.is_link_local() => "link-local",
        IpAddr::V4(v4)
            if v4.is_private() || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64) =>
        {
            "lan"
        }
        IpAddr::V4(v4) if v4.is_multicast() || v4.is_broadcast() => "multicast",
        IpAddr::V4(_) => "public",
        IpAddr::V6(v6) if v6.is_unspecified() => return text.to_string(),
        IpAddr::V6(v6) if v6.is_loopback() => "loopback",
        IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => "link-local",
        IpAddr::V6(v6) if v6.segments()[0] & 0xfe00 == 0xfc00 => "lan",
        IpAddr::V6(v6) if v6.is_multicast() => "multicast",
        IpAddr::V6(_) => "public",
    };
    format!("<ip:{kind}>")
}

/// Six pairs of hex digits, between colons or dashes.
fn is_mac(candidate: &str) -> bool {
    let parts: Vec<&str> = candidate.split([':', '-']).collect();
    parts.len() == 6
        && candidate.len() == 17
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Three labels or more between dots, the last one of letters.
fn is_host(candidate: &str) -> bool {
    let labels: Vec<&str> = candidate.split('.').collect();
    labels.len() >= 3
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && labels
            .last()
            .is_some_and(|last| last.len() >= 2 && last.bytes().all(|b| b.is_ascii_alphabetic()))
}

/// Whether a word looks like a key, a hash or a Contact ID: ten hex digits
/// or more (with a letter among them, but for the libraries' lines), or
/// forty letters and digits or more, mixed (base32).
fn is_id(word: &str, third_party: bool) -> bool {
    let hex = word.len() >= 10
        && word.bytes().all(|b| b.is_ascii_hexdigit())
        && (third_party || word.bytes().any(|b| b.is_ascii_alphabetic()));
    let long = word.len() >= 40
        && word.bytes().any(|b| b.is_ascii_digit())
        && word.bytes().any(|b| b.is_ascii_alphabetic());
    hex || long
}

/// What the libraries log (iroh and the crates below it through `tracing`,
/// rustls and reqwest through `log`): from warnings on, or from debug on with
/// `DODAVO_LOG`. Spans are left out, with what they carry (relay hosts,
/// short IDs). A line that repeats is kept 5 times in 10 minutes.
struct Libraries;

/// How many times a line of a library is kept in [`WINDOW`].
const REPEATS: u32 = 5;
const WINDOW: Duration = Duration::from_secs(600);

struct Window {
    since: Instant,
    count: u32,
    left_out: u32,
}

static WINDOWS: LazyLock<Mutex<HashMap<u64, Window>>> = LazyLock::new(Default::default);

/// Whether a line of a library from the place `key` is kept, and how many
/// from there were left out before it.
fn admit(key: u64) -> Option<u32> {
    let mut windows = lock(&WINDOWS);
    if windows.len() > 1000 {
        windows.clear();
    }
    let now = Instant::now();
    let window = windows.entry(key).or_insert(Window {
        since: now,
        count: 0,
        left_out: 0,
    });
    if now.duration_since(window.since) >= WINDOW {
        let left_out = window.left_out;
        *window = Window {
            since: now,
            count: 1,
            left_out: 0,
        };
        return Some(left_out);
    }
    if window.count < REPEATS {
        window.count += 1;
        Some(0)
    } else {
        window.left_out += 1;
        None
    }
}

/// Writes a line of a library.
fn library_line(level: Level, key: u64, target: &str, message: &str) {
    let Some(_writing) = Writing::begin() else {
        return;
    };
    let Some(left_out) = admit(key) else {
        return;
    };
    let target = scrub(target, true);
    if left_out > 0 {
        deliver(line(
            level,
            &target,
            &format!("{left_out} more lines from here were left out in the last 10 minutes"),
        ));
    }
    let message = scrub(&flatten(message), true);
    deliver(line(level, &target, &message));
}

fn tracing_level(level: &tracing_core::Level) -> Level {
    use tracing_core::Level as T;
    match *level {
        T::ERROR => Level::Error,
        T::WARN => Level::Warn,
        T::INFO => Level::Info,
        _ => Level::Debug,
    }
}

fn tracing_filter() -> tracing_core::LevelFilter {
    match verbosity() {
        0 => tracing_core::LevelFilter::WARN,
        1 => tracing_core::LevelFilter::DEBUG,
        _ => tracing_core::LevelFilter::TRACE,
    }
}

impl tracing_core::Subscriber for Libraries {
    fn register_callsite(
        &self,
        metadata: &'static tracing_core::Metadata<'static>,
    ) -> tracing_core::subscriber::Interest {
        if self.enabled(metadata) {
            tracing_core::subscriber::Interest::always()
        } else {
            tracing_core::subscriber::Interest::never()
        }
    }

    fn enabled(&self, metadata: &tracing_core::Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() <= tracing_filter()
    }

    fn max_level_hint(&self) -> Option<tracing_core::LevelFilter> {
        Some(tracing_filter())
    }

    fn new_span(&self, _: &tracing_core::span::Attributes<'_>) -> tracing_core::span::Id {
        tracing_core::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing_core::span::Id, _: &tracing_core::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing_core::span::Id, _: &tracing_core::span::Id) {}

    fn event(&self, event: &tracing_core::Event<'_>) {
        let metadata = event.metadata();
        let mut fields = Fields::default();
        event.record(&mut fields);
        let key = metadata as *const _ as usize as u64;
        library_line(
            tracing_level(metadata.level()),
            key,
            metadata.target(),
            &fields.text(),
        );
    }

    fn enter(&self, _: &tracing_core::span::Id) {}

    fn exit(&self, _: &tracing_core::span::Id) {}
}

/// The fields of an event of `tracing`: its message, then the others as
/// `name=value`.
#[derive(Default)]
struct Fields {
    message: String,
    others: Vec<String>,
}

impl Fields {
    fn text(&self) -> String {
        let mut text = self.message.clone();
        for other in &self.others {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(other);
        }
        text
    }

    fn add(&mut self, field: &tracing_core::Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.others.push(format!("{}={value}", field.name()));
        }
    }
}

impl tracing_core::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing_core::Field, value: &dyn fmt::Debug) {
        self.add(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing_core::Field, value: &str) {
        self.add(field, value.to_string());
    }

    fn record_error(&mut self, field: &tracing_core::Field, value: &(dyn Error + 'static)) {
        self.add(field, library(value));
    }
}

fn log_filter() -> log::LevelFilter {
    match verbosity() {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    }
}

impl log::Log for Libraries {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log_filter()
    }

    fn log(&self, record: &log::Record<'_>) {
        if !log::Log::enabled(self, record.metadata()) {
            return;
        }
        let level = match record.level() {
            log::Level::Error => Level::Error,
            log::Level::Warn => Level::Warn,
            log::Level::Info => Level::Info,
            _ => Level::Debug,
        };
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::hash::DefaultHasher::new();
            record.target().hash(&mut hasher);
            record.line().hash(&mut hasher);
            hasher.finish()
        };
        library_line(level, key, record.target(), &record.args().to_string());
    }

    fn flush(&self) {}
}

static LIBRARIES: Libraries = Libraries;

/// Takes what the libraries log, once. `log` first: until `tracing` has its
/// subscriber, what is logged through it goes to `log`.
fn install_libraries() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if log::set_logger(&LIBRARIES).is_ok() {
            log::set_max_level(log_filter());
        }
        let _ =
            tracing_core::dispatcher::set_global_default(tracing_core::Dispatch::new(Libraries));
    });
}

/// The lines of the tests, which write no file.
#[cfg(test)]
static CAPTURED: Mutex<std::collections::VecDeque<String>> =
    Mutex::new(std::collections::VecDeque::new());

#[cfg(test)]
fn capture(text: &str) {
    let mut captured = lock(&CAPTURED);
    if captured.len() >= 50_000 {
        captured.pop_front();
    }
    captured.push_back(text.to_string());
}

/// What the tests have logged so far, also from the libraries (see
/// [`capture_libraries`]).
#[cfg(test)]
pub fn captured() -> Vec<String> {
    lock(&CAPTURED).iter().cloned().collect()
}

/// Lets the tests see what the libraries log.
#[cfg(test)]
pub fn capture_libraries() {
    install_libraries();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_405), (2025, 11, 13));
        let time = UNIX_EPOCH + Duration::from_millis(1_763_042_602_123);
        assert_eq!(timestamp(time), "2025-11-13T14:03:22.123Z");
    }

    #[test]
    fn addresses_go() {
        let cases = [
            ("from 192.168.1.20:4433 on", "from <ip:lan>:4433 on"),
            ("at 8.8.8.8.", "at <ip:public>."),
            ("cgnat 100.72.1.2", "cgnat <ip:lan>"),
            ("[2001:db8::1]:443", "[<ip:public>]:443"),
            ("v6 fe80::1%eth0 x", "v6 <ip:link-local> x"),
            ("::1", "<ip:loopback>"),
            ("mapped ::ffff:10.0.0.1", "mapped <ip:lan>"),
            ("ula fd12:3456::1", "ula <ip:lan>"),
            ("any 0.0.0.0:0", "any 0.0.0.0:0"),
            ("mac 3c:22:fb:01:02:03", "mac <mac>"),
            ("mac 3C-22-FB-01-02-03", "mac <mac>"),
        ];
        for (text, expected) in cases {
            assert_eq!(scrub(text, true), expected, "{text}");
        }
    }

    #[test]
    fn what_tells_nothing_stays() {
        for text in [
            "Dodavo 1.1.0 (windows x86_64)",
            "Windows 11, version 10.0, build 26300 update 1234",
            "iroh::socket::transports: probe failed",
            "error 0x80070005 (os error 5)",
            "#12 send token=3f8a9c12 peer=peer#4c1e9a size 123456789012",
            "at 14:03:22 after 1.5s",
            "socket.rs:599:5",
        ] {
            assert_eq!(scrub(text, false), text);
        }
    }

    #[test]
    fn ids_go() {
        let full = "ae58ff8833241ac82d6ff7611046ed67b5072d142c588d0063e942d9a75502b6";
        assert_eq!(scrub(&format!("id {full}."), false), "id <id>.");
        assert_eq!(
            scrub(&format!("PublicKey({full})"), false),
            "PublicKey(<id>)"
        );
        assert_eq!(scrub("remote=ae58ff8833 x", false), "remote=<id> x");
        // A short ID may be all digits: only the libraries' lines lose them.
        assert_eq!(scrub("remote=1234567890", true), "remote=<id>");
        assert_eq!(scrub("size 1234567890", false), "size 1234567890");
        let z32 = "yryyyyyyyyyyyyyyyyyyyyyyyyyy3pfy5n9h8wx6q1jo7ze4cbuwasfa";
        assert_eq!(scrub(&format!("_iroh.{z32} q"), true), "_iroh.<id> q");
    }

    #[test]
    fn paths_go() {
        for text in [
            r"Cannot read C:\Users\Ana Kovač\Desktop\plan.docx: denied",
            r#"path: "C:\\Users\\ana\\x.txt""#,
            r"\\server\share\Photos",
            r"\\?\C:\Users\ana",
            "/Users/ana/Desktop/x",
            "/storage/emulated/0/Download/Dodavo/x.jpg",
            "/data/user/0/com.hrvojekatic.dodavo/files",
            "~/Documents/x",
            "file:///C:/Users/ana/x",
        ] {
            let scrubbed = scrub(text, false);
            assert!(
                !scrubbed.contains("ana") && !scrubbed.contains("Ana"),
                "{text} → {scrubbed}"
            );
            assert!(
                !scrubbed.contains("Photos") && !scrubbed.contains("x.jpg"),
                "{scrubbed}"
            );
        }
        assert_eq!(
            scrub(r"failed at C:\Users\ana\x", false),
            "failed at <path…>"
        );
    }

    #[test]
    fn urls_and_hosts_go() {
        assert_eq!(
            scrub(
                "GET https://user:secret@example.com:8443/a/b?token=x done",
                true
            ),
            "GET https://<host>:8443/… done"
        );
        assert_eq!(
            scrub("https://github.com/hkatic/dodavo/releases/latest", true),
            "https://github.com/…"
        );
        assert_eq!(
            scrub(
                "relay https://euw1-1.relay.n0.iroh-canary.iroh.link./",
                true
            ),
            "relay https://<host>/…"
        );
        assert_eq!(
            scrub(r#"host: Some(Domain("use1-1.relay.n0.iroh.link"))"#, true),
            r#"host: Some(Domain("<host>"))"#
        );
        assert_eq!(
            scrub("content://com.android.providers.media/x", true),
            "content://<host>/…"
        );
    }

    #[test]
    fn lines_are_one() {
        assert_eq!(flatten("a\r\nb\nc\rd\te"), "a | b | c | d e");
        assert_eq!(flatten(&"x".repeat(2000)).chars().count(), LINE_MAX + 1);
    }

    #[test]
    fn aliases() {
        let id = iroh::SecretKey::generate().public();
        let alias = peer(&id);
        assert_eq!(alias, peer(&id));
        assert_eq!(alias, peer_str(&id.to_string().to_uppercase()));
        assert!(alias.starts_with("peer#") && alias.len() == 11);
        assert_ne!(alias, peer(&iroh::SecretKey::generate().public()));
        assert_eq!(token(&[0xab; 16]), "abababab");
    }

    #[test]
    fn causes_keep_texts_out() {
        // Access denied: ERROR_ACCESS_DENIED on Windows, EACCES elsewhere.
        let denied = if cfg!(windows) { 5 } else { 13 };
        let io = io::Error::from_raw_os_error(denied);
        let error = anyhow::Error::new(io).context(r"Cannot read C:\Users\ana\plan.docx");
        let text = cause(&error);
        assert!(
            text.starts_with(&format!("<message>: PermissionDenied (os error {denied})")),
            "{text}"
        );
        assert!(!text.contains("ana"));

        let custom = io::Error::other(r"C:\Users\ana\x is busy");
        let text = cause(&anyhow::Error::new(custom));
        assert!(!text.contains("ana"), "{text}");

        let json = serde_json::from_str::<u32>(r#""Ana's secret""#).unwrap_err();
        let text = cause(&anyhow::Error::new(json));
        assert!(text.starts_with("JSON Data error at line 1"), "{text}");
        assert!(!text.contains("secret"));

        let peer_said = anyhow::anyhow!("The other side says: Ana's photos are gone");
        assert_eq!(cause(&peer_said), "<message>");
    }

    #[test]
    fn connections_by_code() {
        use iroh::endpoint::{ApplicationClose, ConnectionError, VarInt};
        let close = |code: u32, reason: &'static [u8]| {
            ConnectionError::ApplicationClosed(ApplicationClose {
                error_code: VarInt::from_u32(code),
                reason: reason.into(),
            })
        };
        assert_eq!(
            connection_error(&close(3, b"exiting")),
            "closed by peer (suspended, exiting)"
        );
        assert_eq!(
            connection_error(&close(1, b"Ana says no")),
            "closed by peer (cancelled)"
        );
        let read = io::Error::from(iroh::endpoint::ReadError::ConnectionLost(close(9, b"x")));
        let text = cause(&anyhow::Error::new(read));
        assert!(text.contains("closed by peer (code 9)"), "{text}");
        assert_eq!(
            cause(&crate::net::session::lost(ConnectionError::TimedOut)),
            "connection lost: timed out"
        );
    }

    #[test]
    fn repeats() {
        let mut repeats = Repeats::new();
        assert_eq!(repeats.note("a"), Some(0));
        assert_eq!(repeats.note("a"), None);
        assert_eq!(repeats.note("a"), None);
        assert_eq!(repeats.note("b"), Some(2));
        repeats.clear();
        assert_eq!(repeats.note("b"), Some(0));
    }

    #[test]
    fn panics_lose_the_builders_folders() {
        assert_eq!(
            trim_location(
                r"C:\Users\dev\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\zip-9.1.3\src\read.rs"
            ),
            "zip-9.1.3/src/read.rs"
        );
        assert_eq!(
            trim_location("/rustc/abc123/library/core/src/option.rs"),
            "library/core/src/option.rs"
        );
        assert_eq!(trim_location(r"src\net\receive.rs"), "src/net/receive.rs");
        assert_eq!(
            trim_location("/Users/dev/git/Dodavo/src/net/send.rs"),
            "src/net/send.rs"
        );
    }

    #[test]
    fn trims_to_whole_lines() {
        let dir = std::env::temp_dir().join(format!("dodavo-log-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        let mut writer = Writer::new(path.clone(), Arc::default());
        writer.open();
        let text = "x".repeat(99);
        for _ in 0..30_000 {
            writer.write(&text);
        }
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size <= MAX_SIZE, "{size}");
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines = content.lines();
        assert!(
            lines
                .next()
                .unwrap()
                .ends_with("older lines were removed to keep the log small")
        );
        assert!(lines.all(|line| line.trim_end() == text));
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The system is told by its version and model, never by the names of
    /// the user or of the computer.
    #[test]
    fn the_system_is_told_without_its_names() {
        let header = header("test", &[]).join("\n");
        assert!(header.contains("system: "), "{header}");
        for name in ["USERNAME", "USER", "COMPUTERNAME", "HOSTNAME"] {
            if let Ok(value) = std::env::var(name)
                && value.len() >= 4
            {
                assert!(!header.contains(&value), "{name}: {header}");
            }
        }
        if let Some(home) = std::env::home_dir() {
            assert!(!header.contains(&home.display().to_string()), "{header}");
        }
    }

    #[test]
    fn lines_before_init_are_kept_for_it() {
        crate::log_info!("before init {}", 42);
        assert!(
            captured()
                .iter()
                .any(|line| line.ends_with("logging::tests: before init 42"))
        );
    }
}
