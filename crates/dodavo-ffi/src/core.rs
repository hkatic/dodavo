//! The state of the program, as `app.rs` holds it for the Win32 interface.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fs::File,
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Condvar, Mutex, MutexGuard, mpsc},
    time::{Duration, Instant},
};

use dodavo_core::{
    archive,
    config::{self, Config, Contact, ContactLimit, OverLimit, ReceivedFolder},
    i18n::{self, Lang, tr},
    identity,
    limit::{self, Unit},
    net::{
        self, Accept, Command, Event, Kind, NetHandle, Offer, Outcome, Role, TransferRecord,
        session::Side,
    },
    space::{self, Space},
    view::{Direction, Notice, Row, TransferView},
};
#[cfg(target_os = "macos")]
use dodavo_core::{format::format_size, update, view::percent};
// The texts of this system's user interface: the Mac's or Android's.
#[cfg(not(target_os = "macos"))]
use dodavo_core::i18n::android as table;
#[cfg(target_os = "macos")]
use dodavo_core::i18n::mac as table;
use serde::{Deserialize, Serialize};

use crate::Wake;

/// The table of this system's user interface (see `table`).
fn ui() -> &'static table::Strings {
    table::s()
}

/// How often the speed is worked out anew, also when no report comes.
const TICK: Duration = Duration::from_secs(1);
/// Copyright years and website, from Cargo.toml through `build.rs`.
const LICENSE: &str = include_str!("../../../LICENSE");
#[cfg(target_os = "macos")]
const COMPONENTS: &str = include_str!("../../../res/third-party-licenses-macos.txt");
#[cfg(not(target_os = "macos"))]
const COMPONENTS: &str = include_str!("../../../res/third-party-licenses-android.txt");

/// The command line.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Options {
    profile: Option<String>,
    /// Started to run in the background (at login).
    hidden: bool,
    /// Android: the folders that only the app can ask the system for (see
    /// `platform::set_dirs`).
    #[cfg(not(target_os = "macos"))]
    data_dir: PathBuf,
    #[cfg(not(target_os = "macos"))]
    temp_dir: PathBuf,
    #[cfg(not(target_os = "macos"))]
    downloads_dir: PathBuf,
    /// Android: the code of the language that the app's own texts are in, so
    /// that what this says always matches them.
    #[cfg(not(target_os = "macos"))]
    language: Option<String>,
    #[cfg(not(target_os = "macos"))]
    languages: Vec<String>,
    #[cfg(not(target_os = "macos"))]
    device_name: Option<String>,
}

/// What the user interface needs to begin.
#[derive(Debug, Default, Serialize)]
pub struct Started {
    /// Nothing can go on: this says why, and the program ends.
    fatal: Option<String>,
    /// This profile runs already.
    already_running: bool,
    /// The settings could not be read: say so.
    warning: Option<String>,
    my_id: String,
    profile: Option<String>,
    language: String,
    version: String,
    author: String,
    copyright: String,
    website: String,
    #[cfg(target_os = "macos")]
    donate_url: String,
    #[cfg(target_os = "macos")]
    start_hidden: bool,
    #[cfg(target_os = "macos")]
    show_in_menu_bar: bool,
    #[cfg(target_os = "macos")]
    open_at_login: bool,
    /// Each language by its code, with its own name for it.
    languages: Vec<(&'static str, &'static str)>,
}

/// A message for the user interface.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Out {
    Status {
        online: bool,
        text: String,
    },
    Row {
        row: Row,
        /// A change in what it is doing, to say aloud.
        announce: Option<String>,
    },
    RowGone {
        id: u64,
    },
    Notice {
        /// The transfer it is about.
        id: u64,
        notice: Notice,
        path: Option<PathBuf>,
    },
    Offer {
        offer: OfferOut,
        notice: Notice,
    },
    /// The sender gave up while the user was still deciding.
    OfferGone {
        id: u64,
        notice: Notice,
    },
    /// An offer was declined because its item is over the sender's limit,
    /// and the user wants to be told. Nothing follows.
    Declined {
        notice: Notice,
    },
    /// No send reads the document `uri` any more: the app's permission for
    /// it can go.
    DocumentDone {
        uri: String,
    },
    /// Whether the computer must stay awake.
    Busy {
        busy: bool,
    },
    #[cfg(target_os = "macos")]
    UpdateAvailable {
        headline: String,
        notes: String,
        /// The user asked: comes to the front.
        manual: bool,
    },
    #[cfg(target_os = "macos")]
    /// What the user asked for has an answer that needs no window.
    UpdateAnswer {
        text: String,
    },
    #[cfg(target_os = "macos")]
    UpdateProgress {
        text: String,
        fraction: Option<f64>,
    },
    #[cfg(target_os = "macos")]
    /// The disk image is here and checked.
    UpdateReady {
        path: PathBuf,
    },
    #[cfg(target_os = "macos")]
    UpdateFailed {
        text: String,
    },
    Error {
        text: String,
    },
    /// Nothing can go on: this says why, and the program ends.
    Fatal {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize)]
struct OfferOut {
    id: u64,
    from_id: String,
    from_name: String,
    known: bool,
    headline: String,
    description: String,
    /// For a sender who is not a recipient: who it is, and what to tick.
    unknown_warning: Option<String>,
    add_label: Option<String>,
    dir: PathBuf,
    space: SpaceOut,
    /// Says that the item is over the sender's limit, when it is.
    over_limit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SpaceOut {
    text: String,
    fits: bool,
    enough: Option<bool>,
}

impl From<Space> for SpaceOut {
    fn from(space: Space) -> Self {
        SpaceOut {
            text: space.text,
            fits: space.fits,
            enough: space.enough,
        }
    }
}

/// What the user interface can ask for.
#[derive(Debug, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
enum Call {
    Texts,
    Plural {
        key: String,
        n: u64,
    },
    Send {
        peer_id: String,
        paths: Vec<PathBuf>,
    },
    /// Documents that the user chose in Android's pickers, read where they
    /// are (see `dodavo_core::source`).
    SendDocuments {
        peer_id: String,
        documents: Vec<dodavo_core::source::Document>,
    },
    /// The documents that sends still read: what the app must keep its
    /// permission for.
    Documents,
    /// Without a folder (Android): Downloads.
    Space {
        id: u64,
        #[serde(default)]
        dir: Option<PathBuf>,
    },
    /// What stands in the way of accepting into `dir`.
    CheckAccept {
        id: u64,
        #[serde(default)]
        dir: Option<PathBuf>,
    },
    Accept {
        id: u64,
        #[serde(default)]
        dir: Option<PathBuf>,
    },
    Decline {
        id: u64,
    },
    Pause {
        id: u64,
    },
    Resume {
        id: u64,
    },
    Cancel {
        id: u64,
    },
    RemoveRow {
        id: u64,
    },
    ClearFinished,
    /// The recipients, as `[{name, id, limit}]` (see `RecipientLimit`).
    Contacts,
    CheckRecipient {
        name: String,
        contact_id: String,
        editing: Option<String>,
        #[serde(default)]
        limit: Option<RecipientLimit>,
    },
    /// Without `limit`, the recipient is under the general one.
    AddRecipient {
        name: String,
        contact_id: String,
        #[serde(default)]
        limit: Option<RecipientLimit>,
    },
    /// Without `limit`, the recipient keeps the one they have.
    EditRecipient {
        old_id: String,
        name: String,
        contact_id: String,
        #[serde(default)]
        limit: Option<RecipientLimit>,
    },
    RemoveRecipient {
        contact_id: String,
    },
    /// Blocks the sender `contact_id` under `name` (see `Config::block`):
    /// their offers are declined from now on, and so are those that wait for
    /// an answer, whose ids the reply lists (`declined`) for their windows to
    /// close. A recipient is removed.
    Block {
        name: String,
        contact_id: String,
    },
    Unblock {
        contact_id: String,
    },
    /// The blocked senders, as `[{name, id}]` in the order of their names.
    Blocked,
    Settings,
    SetSettings {
        settings: Settings,
    },
    SettingsProblem {
        settings: Settings,
    },
    RunningCount,
    CancelAll,
    Licenses,
    Shutdown,
    /// Gives this installation a new Contact ID. Every transfer is cancelled
    /// and every waiting offer declined first (`declined`: their windows are
    /// to close). The answer has the Contact ID that holds from now on, and
    /// an error when that is still the old one.
    #[cfg(target_os = "macos")]
    ChangeContactId,
    #[cfg(target_os = "macos")]
    CheckForUpdates,
    #[cfg(target_os = "macos")]
    DownloadUpdate,
    #[cfg(target_os = "macos")]
    CancelUpdate,
    #[cfg(target_os = "macos")]
    RemindLater,
    /// The update window was closed.
    #[cfg(target_os = "macos")]
    UpdateClosed,
    /// Android: the system says that the network changed.
    #[cfg(not(target_os = "macos"))]
    NetworkChanged,
    /// Android: the user chose another language for the app, whose texts
    /// are in the language with this code now.
    #[cfg(not(target_os = "macos"))]
    SetLanguage {
        code: String,
    },
}

/// The settings as the Settings window shows them.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    my_name: String,
    open_at_login: bool,
    hide_at_login: bool,
    show_in_menu_bar: bool,
    check_updates: bool,
    /// A language code, or none for the system's.
    language: Option<String>,
    always_save: bool,
    always_save_dir: Option<PathBuf>,
    auto_accept: bool,
    received_folder: ReceivedFolder,
    #[serde(flatten)]
    limit: LimitSettings,
}

/// The settings as Android's Settings screen shows them. Received items go to
/// Downloads (`Download/Dodavo`), and the system keeps the app's language.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    my_name: String,
    auto_accept: bool,
    received_folder: ReceivedFolder,
    #[serde(flatten)]
    limit: LimitSettings,
}

/// The general limit on the size of an item, as the settings show it: these
/// are fields of `Settings` for the user interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LimitSettings {
    limit_size: bool,
    /// A whole number of `limit_unit`, as the user typed it.
    limit_number: String,
    /// "mb" or "gb" (see `Unit::code`).
    limit_unit: String,
    over_limit: OverLimit,
}

impl LimitSettings {
    fn of(cfg: &Config) -> LimitSettings {
        let (number, unit) = limit::to_input(cfg.size_limit);
        LimitSettings {
            limit_size: cfg.limit_size,
            limit_number: number.to_string(),
            limit_unit: unit.code().to_string(),
            over_limit: cfg.over_limit,
        }
    }

    fn bytes(&self) -> Result<u64, String> {
        limit_bytes(&self.limit_number, &self.limit_unit)
    }

    /// Without the limit, a size that cannot be used is left as it was.
    fn apply_to(&self, cfg: &mut Config) {
        cfg.limit_size = self.limit_size;
        if let Ok(bytes) = self.bytes() {
            cfg.size_limit = bytes;
        }
        cfg.over_limit = self.over_limit;
    }

    fn problem(&self) -> Option<String> {
        self.limit_size.then(|| self.bytes().err()).flatten()
    }
}

/// The limit that the user entered as `number` of the unit with the code
/// `unit`, or what to tell them.
fn limit_bytes(number: &str, unit: &str) -> Result<u64, String> {
    limit::from_input(number, Unit::from_code(unit).unwrap_or(Unit::Mb))
}

/// A recipient's limit as the user interface shows and gives it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecipientLimit {
    /// "general" (as the settings say), "none" (any size) or "own".
    kind: String,
    /// The size of a limit of their own, as in `LimitSettings`. For the
    /// other kinds: what the editor offers when the user chooses "own".
    #[serde(default)]
    number: String,
    #[serde(default)]
    unit: String,
}

impl RecipientLimit {
    fn of(contact: &Contact, cfg: &Config) -> RecipientLimit {
        let (kind, bytes) = match contact.limit {
            ContactLimit::General => ("general", cfg.size_limit),
            ContactLimit::Unlimited => ("none", cfg.size_limit),
            ContactLimit::Bytes(bytes) => ("own", bytes),
        };
        let (number, unit) = limit::to_input(bytes);
        RecipientLimit {
            kind: kind.to_string(),
            number: number.to_string(),
            unit: unit.code().to_string(),
        }
    }

    /// The limit it stands for, or what to tell the user.
    fn limit(&self) -> Result<ContactLimit, String> {
        match self.kind.as_str() {
            "own" => limit_bytes(&self.number, &self.unit).map(ContactLimit::Bytes),
            "none" => Ok(ContactLimit::Unlimited),
            _ => Ok(ContactLimit::General),
        }
    }
}

/// A recipient as the user interface lists them.
#[derive(Debug, Serialize)]
struct RecipientOut<'a> {
    name: &'a str,
    id: &'a str,
    limit: RecipientLimit,
}

struct PendingOffer {
    from_id: String,
    from_name: String,
    offer: Offer,
}

struct State {
    cfg: Config,
    data_dir: PathBuf,
    temp_dir: PathBuf,
    net: NetHandle,
    online: Option<bool>,
    transfers: BTreeMap<u64, TransferView>,
    offers: HashMap<u64, PendingOffer>,
    /// What was last shown of each transfer, and when.
    shown: HashMap<u64, (Row, Instant)>,
    busy: bool,
    /// The document file that each send of one reads, for as long as it
    /// runs (see `Out::DocumentDone`).
    documents: HashMap<u64, String>,
    /// The running update check or download, if any.
    #[cfg(target_os = "macos")]
    update_job: Option<update::Job>,
    /// The update that the window shows, if it is open.
    #[cfg(target_os = "macos")]
    update: Option<update::UpdateInfo>,
    /// When the update check is next looked at (see `update::tick`).
    #[cfg(target_os = "macos")]
    next_update_look: Instant,
}

pub struct Core {
    state: Mutex<State>,
    /// This installation's Contact ID, which the user can change.
    my_id: Mutex<String>,
    outbox: &'static Outbox,
    /// Events of the network on their way to the thread that handles them.
    _events: mpsc::Sender<Event>,
}

/// The messages that wait for the user interface. Android's waits for them
/// (`Core::wait`), the Mac's is woken (`wake`).
pub struct Outbox {
    queue: Mutex<VecDeque<Out>>,
    ready: Condvar,
    wake: Option<Wake>,
}

impl Outbox {
    fn push(&self, message: Out) {
        let mut queue = lock(&self.queue);
        // A newer state of a row replaces one that was not taken yet.
        if let Out::Row {
            row,
            announce: None,
        } = &message
        {
            // The newest one: it is what the row shows when this has been taken.
            for waiting in queue.iter_mut().rev() {
                if let Out::Row {
                    row: older,
                    announce,
                } = waiting
                    && older.id == row.id
                {
                    *waiting = Out::Row {
                        row: row.clone(),
                        announce: announce.take(),
                    };
                    return;
                }
            }
        }
        queue.push_back(message);
        drop(queue);
        self.ready.notify_all();
        if let Some(wake) = &self.wake {
            wake.call();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Holds the lock that says this profile runs, for as long as the program does.
/// `None` when another copy holds it, which only `EWOULDBLOCK` says: a folder
/// that cannot be written, or a volume without `flock`, is an error.
fn take_instance_lock(data_dir: &Path) -> io::Result<Option<File>> {
    std::fs::create_dir_all(data_dir)?;
    let file = File::create(data_dir.join("running.lock"))?;
    // SAFETY: the descriptor is open during the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EWOULDBLOCK) => Ok(None),
        _ => Err(error),
    }
}

/// What kept a folder from being used, in the user's language.
fn folder_error_text(error: &io::Error) -> String {
    match error.raw_os_error() {
        // To `error_text`, these say that the part of a transfer has gone.
        Some(code @ (libc::ENOENT | libc::ENOTDIR)) => {
            ui().error_system.replace("{code}", &code.to_string())
        }
        Some(code) => dodavo_core::platform::error_text(code).unwrap_or_else(|| error.to_string()),
        None => error.to_string(),
    }
}

/// The message for a folder that cannot be used.
fn folder_unusable(path: &Path, error: &io::Error) -> String {
    ui()
        .folder_unusable
        .replace("{path}", &path.display().to_string())
        .replace("{error}", &folder_error_text(error))
}

/// Whether the user can tell a file from a folder with a path.
/// The document file that a restored send reads, if it reads one.
fn document_read(record: &TransferRecord) -> Option<String> {
    match &record.side {
        Side::Send { source, .. }
            if record.offer.kind == Kind::File && dodavo_core::source::is_document(source) =>
        {
            source.to_str().map(str::to_string)
        }
        _ => None,
    }
}

fn kind_of(path: &Path) -> Kind {
    if path.is_dir() {
        Kind::Folder
    } else {
        Kind::File
    }
}

fn plain_notice(title: &str, text: String) -> Notice {
    Notice {
        title: title.to_string(),
        text,
    }
}

impl Core {
    pub fn start(options: &str, wake: Option<Wake>) -> (Core, Started) {
        let options: Options = serde_json::from_str(options).unwrap_or_default();
        #[cfg(not(target_os = "macos"))]
        dodavo_core::platform::set_dirs(dodavo_core::platform::Dirs {
            data: options.data_dir.clone(),
            temp: options.temp_dir.clone(),
            downloads: options.downloads_dir.clone(),
            languages: options.languages.clone(),
            device_name: options.device_name.clone(),
        });
        let profile = options.profile.as_deref().map(|p| {
            p.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        });
        let profile = profile.filter(|p| !p.is_empty());
        let data_dir = config::data_dir(profile.as_deref());

        // The language comes from the settings, so the message about an
        // unusable settings file has to wait until it is chosen.
        let (cfg, cfg_error) = match Config::load(&data_dir) {
            Ok(cfg) => (cfg, None),
            Err(e) => (Config::default(), Some(e)),
        };
        // On Android, the language of the app's own texts, which the system
        // keeps.
        #[cfg(not(target_os = "macos"))]
        i18n::init(options.language.as_deref());
        #[cfg(target_os = "macos")]
        i18n::init(cfg.language.as_deref());

        let outbox: &'static Outbox = Box::leak(Box::new(Outbox {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            wake,
        }));
        install_panic_hook(outbox);

        let mut started = Started {
            my_id: String::new(),
            profile: profile.clone(),
            language: i18n::lang().code().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            author: dodavo_core::AUTHOR.to_string(),
            copyright: dodavo_core::COPYRIGHT_YEARS.to_string(),
            website: dodavo_core::WEBSITE.to_string(),
            #[cfg(target_os = "macos")]
            donate_url: dodavo_core::DONATE_URL.to_string(),
            #[cfg(target_os = "macos")]
            start_hidden: options.hidden && cfg.start_minimized,
            #[cfg(target_os = "macos")]
            show_in_menu_bar: cfg.minimize_to_tray,
            #[cfg(target_os = "macos")]
            open_at_login: cfg.start_with_windows,
            languages: languages(),
            ..Started::default()
        };

        match take_instance_lock(&data_dir) {
            // Kept for the life of the program: it ends with the process.
            Ok(Some(instance)) => std::mem::forget(instance),
            Ok(None) => started.already_running = true,
            Err(e) => started.fatal = Some(folder_unusable(&data_dir, &e)),
        }

        if let Some(e) = cfg_error {
            let mut text = ui()
                .settings_unreadable
                .replace("{path}", &e.path.display().to_string())
                .replace("{error}", &e.error);
            if let Some(backup) = &e.backup {
                text.push_str("\n\n");
                text.push_str(
                    &ui()
                        .settings_backup
                        .replace("{backup}", &backup.display().to_string()),
                );
            }
            started.warning = Some(text);
        }

        let secret = if started.already_running || started.fatal.is_some() {
            None
        } else {
            match identity::load_or_create(&data_dir) {
                Ok(secret) => Some(secret),
                Err(e) => {
                    started.fatal = Some(
                        ui()
                            .identity_failed
                            .replace("{path}", &data_dir.display().to_string())
                            .replace("{error}", &format!("{e:#}")),
                    );
                    None
                }
            }
        };
        let temp_dir = archive::temp_dir_for(profile.as_deref());

        // A key that is thrown away keeps the rest working the same way when
        // nothing will run anyway.
        let (events, received) = mpsc::channel::<Event>();
        let my_id;
        let net;
        match secret {
            Some(secret) => {
                my_id = secret.public().to_string();
                started.my_id = my_id.clone();
                let sink = events.clone();
                net = net::start(
                    secret,
                    data_dir.clone(),
                    temp_dir.clone(),
                    cfg.blocked_ids(),
                    cfg.limits(),
                    move |event| {
                        let _ = sink.send(event);
                    },
                );
            }
            None => {
                my_id = String::new();
                net = NetHandle::idle();
            }
        }
        let core = Core {
            state: Mutex::new(State {
                cfg,
                data_dir,
                temp_dir,
                net,
                online: None,
                transfers: BTreeMap::new(),
                offers: HashMap::new(),
                shown: HashMap::new(),
                busy: false,
                documents: HashMap::new(),
                #[cfg(target_os = "macos")]
                update_job: None,
                #[cfg(target_os = "macos")]
                update: None,
                #[cfg(target_os = "macos")]
                next_update_look: Instant::now() + update::START_DELAY,
            }),
            my_id: Mutex::new(my_id),
            outbox,
            _events: events,
        };
        std::thread::Builder::new()
            .name("events".into())
            .spawn(move || handle_events(received))
            .expect("the thread of the events");
        (core, started)
    }

    /// Shows again what the user interface shows, in the language it has now
    /// (Android: the user chose another one for the app).
    #[cfg(not(target_os = "macos"))]
    fn restate(&self) {
        let mut state = lock(&self.state);
        if let Some(online) = state.online {
            let text = if online {
                ui().status_online
            } else {
                ui().status_offline
            };
            self.outbox.push(Out::Status {
                online,
                text: text.to_string(),
            });
        }
        let ids: Vec<u64> = state.transfers.keys().copied().collect();
        for id in ids {
            self.show(&mut state, id, None);
        }
    }

    pub fn poll(&self) -> Option<String> {
        let message = lock(&self.outbox.queue).pop_front()?;
        serde_json::to_string(&message).ok()
    }

    /// The next message, waiting for one at most `timeout`: Android's app has
    /// a thread wait here, so that nothing here calls into Java.
    pub fn wait(&self, timeout: Duration) -> Option<String> {
        let queue = lock(&self.outbox.queue);
        let (mut queue, _) = self
            .outbox
            .ready
            .wait_timeout_while(queue, timeout, |queue| queue.is_empty())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let message = queue.pop_front()?;
        drop(queue);
        serde_json::to_string(&message).ok()
    }

    pub fn call(&self, call: &str) -> String {
        let Ok(call) = serde_json::from_str::<Call>(call) else {
            return "null".into();
        };
        self.answer(call)
    }

    fn answer(&self, call: Call) -> String {
        let json = |value: serde_json::Value| value.to_string();
        match call {
            Call::Texts => {
                let texts: BTreeMap<_, _> = ui().texts().into_iter().collect();
                serde_json::to_string(&texts).unwrap_or_default()
            }
            Call::Plural { key, n } => {
                let table = ui();
                let text = table
                    .plurals()
                    .into_iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, plural)| i18n::pick_plural(i18n::lang(), plural, n))
                    .unwrap_or_default();
                json(serde_json::json!(text.replace("{n}", &n.to_string())))
            }
            Call::Send { peer_id, paths } => {
                json(serde_json::json!({ "error": self.send(&peer_id, paths) }))
            }
            Call::SendDocuments { peer_id, documents } => {
                json(serde_json::json!({ "error": self.send_documents(&peer_id, documents) }))
            }
            Call::Documents => {
                let state = lock(&self.state);
                let uris: BTreeSet<&String> = state.documents.values().collect();
                json(serde_json::json!(uris))
            }
            Call::Space { id, dir } => {
                let dir = dir.unwrap_or_else(dodavo_core::platform::downloads_dir);
                let state = lock(&self.state);
                match state.offers.get(&id) {
                    Some(pending) => {
                        let out: SpaceOut = space::check_space(
                            &pending.offer,
                            &dir,
                            &state.temp_dir,
                            state.cfg.received_folder,
                        )
                        .into();
                        serde_json::to_string(&out).unwrap_or_default()
                    }
                    None => "null".into(),
                }
            }
            Call::CheckAccept { id, dir } => {
                let dir = dir.unwrap_or_else(dodavo_core::platform::downloads_dir);
                json(self.check_accept(id, &dir))
            }
            Call::Accept { id, dir } => {
                let dir = dir.unwrap_or_else(dodavo_core::platform::downloads_dir);
                self.accept(id, dir);
                "null".into()
            }
            Call::Decline { id } => {
                let mut state = lock(&self.state);
                state.offers.remove(&id);
                state.net.send(Command::Respond { id, accept: None });
                "null".into()
            }
            Call::Pause { id } => {
                lock(&self.state).net.send(Command::Pause { id });
                "null".into()
            }
            Call::Resume { id } => {
                lock(&self.state).net.send(Command::Resume { id });
                "null".into()
            }
            Call::Cancel { id } => {
                let mut state = lock(&self.state);
                state.net.send(Command::Cancel { id });
                if let Some(view) = state.transfers.get_mut(&id) {
                    view.on_cancel();
                }
                self.show(&mut state, id, Some(tr!(cancelling).to_string()));
                "null".into()
            }
            Call::RemoveRow { id } => {
                let mut state = lock(&self.state);
                if state.transfers.get(&id).is_some_and(|t| !t.is_active()) {
                    state.transfers.remove(&id);
                    state.shown.remove(&id);
                    self.outbox.push(Out::RowGone { id });
                }
                "null".into()
            }
            Call::ClearFinished => {
                let mut state = lock(&self.state);
                let finished: Vec<u64> = state
                    .transfers
                    .values()
                    .filter(|t| !t.is_active())
                    .map(TransferView::id)
                    .collect();
                for id in finished {
                    state.transfers.remove(&id);
                    state.shown.remove(&id);
                    self.outbox.push(Out::RowGone { id });
                }
                "null".into()
            }
            Call::Contacts => {
                let state = lock(&self.state);
                let recipients: Vec<RecipientOut> = state
                    .cfg
                    .contacts
                    .iter()
                    .map(|contact| RecipientOut {
                        name: &contact.name,
                        id: &contact.id,
                        limit: RecipientLimit::of(contact, &state.cfg),
                    })
                    .collect();
                serde_json::to_string(&recipients).unwrap_or_default()
            }
            Call::CheckRecipient {
                name,
                contact_id,
                editing,
                limit,
            } => {
                let state = lock(&self.state);
                let problem = recipient_problem(
                    &state.cfg,
                    &lock(&self.my_id),
                    &name,
                    &contact_id,
                    editing.as_deref(),
                )
                .or_else(|| limit.and_then(|limit| limit.limit().err()));
                json(serde_json::json!({ "problem": problem }))
            }
            Call::AddRecipient {
                name,
                contact_id,
                limit,
            } => {
                let mut state = lock(&self.state);
                // What cannot be used was reported by `CheckRecipient`.
                let limit = limit
                    .and_then(|limit| limit.limit().ok())
                    .unwrap_or_default();
                state.cfg.add_contact(Contact {
                    limit,
                    ..Contact::new(name.trim(), cleaned_id(&contact_id))
                });
                json(serde_json::json!({ "error": save(&state) }))
            }
            Call::EditRecipient {
                old_id,
                name,
                contact_id,
                limit,
            } => {
                let mut state = lock(&self.state);
                let old = state
                    .cfg
                    .contacts
                    .iter()
                    .find(|c| c.id.eq_ignore_ascii_case(&old_id))
                    .cloned();
                if let Some(old) = old {
                    let limit = limit
                        .and_then(|limit| limit.limit().ok())
                        .unwrap_or(old.limit);
                    let contact = Contact {
                        limit,
                        ..Contact::new(name.trim(), cleaned_id(&contact_id))
                    };
                    state.cfg.replace_contact(&old, contact);
                }
                json(serde_json::json!({ "error": save(&state) }))
            }
            Call::RemoveRecipient { contact_id } => {
                let mut state = lock(&self.state);
                state
                    .cfg
                    .contacts
                    .retain(|c| !c.id.eq_ignore_ascii_case(&contact_id));
                json(serde_json::json!({ "error": save(&state) }))
            }
            Call::Block { name, contact_id } => {
                let mut state = lock(&self.state);
                let contact_id = cleaned_id(&contact_id);
                state.cfg.block(name.trim(), &contact_id);
                let declined: Vec<u64> = state
                    .offers
                    .iter()
                    .filter(|(_, pending)| pending.from_id.eq_ignore_ascii_case(&contact_id))
                    .map(|(&id, _)| id)
                    .collect();
                for &id in &declined {
                    state.offers.remove(&id);
                    state.net.send(Command::Respond { id, accept: None });
                }
                json(serde_json::json!({ "error": save(&state), "declined": declined }))
            }
            Call::Unblock { contact_id } => {
                let mut state = lock(&self.state);
                state.cfg.unblock(&contact_id);
                json(serde_json::json!({ "error": save(&state) }))
            }
            Call::Blocked => {
                let state = lock(&self.state);
                let mut blocked = state.cfg.blocked.clone();
                blocked.sort_by_cached_key(|c| c.name.to_lowercase());
                serde_json::to_string(&blocked).unwrap_or_default()
            }
            Call::Settings => {
                let state = lock(&self.state);
                serde_json::to_string(&Settings::of(&state.cfg)).unwrap_or_default()
            }
            Call::SettingsProblem { settings } => {
                json(serde_json::json!({ "problem": settings.problem() }))
            }
            Call::SetSettings { settings } => {
                let mut state = lock(&self.state);
                settings.apply_to(&mut state.cfg);
                json(serde_json::json!({ "error": save(&state) }))
            }
            Call::RunningCount => {
                let state = lock(&self.state);
                let count = state.transfers.values().filter(|t| t.is_active()).count();
                json(serde_json::json!(count))
            }
            Call::CancelAll => {
                let mut state = lock(&self.state);
                let running: Vec<u64> = state
                    .transfers
                    .values()
                    .filter(|t| t.is_active())
                    .map(TransferView::id)
                    .collect();
                for id in running {
                    state.net.send(Command::Cancel { id });
                    if let Some(view) = state.transfers.get_mut(&id) {
                        view.on_cancel();
                    }
                }
                "null".into()
            }
            Call::Licenses => json(serde_json::json!({
                "dodavo": LICENSE,
                "components": COMPONENTS,
            })),
            #[cfg(target_os = "macos")]
            Call::ChangeContactId => self.change_contact_id(),
            #[cfg(target_os = "macos")]
            Call::CheckForUpdates => {
                self.check_now();
                "null".into()
            }
            #[cfg(target_os = "macos")]
            Call::DownloadUpdate => {
                self.download_update();
                "null".into()
            }
            #[cfg(target_os = "macos")]
            Call::CancelUpdate => {
                if let Some(job) = lock(&self.state).update_job.take() {
                    job.cancel.cancel();
                }
                "null".into()
            }
            #[cfg(target_os = "macos")]
            Call::RemindLater => {
                let mut state = lock(&self.state);
                state.cfg.update_remind_after = Some(now_secs() + update::SNOOZE.as_secs());
                let _ = save(&state);
                "null".into()
            }
            #[cfg(target_os = "macos")]
            Call::UpdateClosed => {
                lock(&self.state).update = None;
                "null".into()
            }
            #[cfg(not(target_os = "macos"))]
            Call::NetworkChanged => {
                lock(&self.state).net.send(Command::NetworkChange);
                "null".into()
            }
            #[cfg(not(target_os = "macos"))]
            Call::SetLanguage { code } => {
                if let Some(lang) = Lang::from_code(&code) {
                    i18n::switch(lang);
                    self.restate();
                }
                "null".into()
            }
            Call::Shutdown => {
                // Off the lock: closing takes a moment, and events still come.
                let net = lock(&self.state).net.clone();
                net.shutdown(Duration::from_secs(3));
                "null".into()
            }
        }
    }

    /// See [`Call::ChangeContactId`].
    #[cfg(target_os = "macos")]
    fn change_contact_id(&self) -> String {
        let answer = |error: Option<String>, declined: Vec<u64>| {
            serde_json::json!({
                "error": error,
                "my_id": *lock(&self.my_id),
                "declined": declined,
            })
            .to_string()
        };
        let (net, declined) = {
            let mut state = lock(&self.state);
            // Until the network is up, it takes no commands.
            if state.online.is_none() {
                return answer(Some(ui().change_id_not_ready.to_string()), Vec::new());
            }
            // The network declines the offers and cancels the transfers;
            // the rows say so until their ends arrive.
            let declined: Vec<u64> = state.offers.drain().map(|(id, _)| id).collect();
            for view in state.transfers.values_mut().filter(|t| t.is_active()) {
                view.on_cancel();
            }
            (state.net.clone(), declined)
        };
        // Off the lock: the ends of the transfers come meanwhile.
        let secret = identity::new_key();
        let new_id = secret.public().to_string();
        let error = match net.change_identity(secret, Duration::from_secs(20)) {
            Ok(()) => {
                *lock(&self.my_id) = new_id;
                None
            }
            Err(net::IdentityError::Busy) => Some(ui().change_id_busy.to_string()),
            Err(net::IdentityError::Failed(error)) => {
                Some(ui().change_id_failed.replace("{error}", &error))
            }
            Err(net::IdentityError::TimedOut) => Some(ui().change_id_not_ready.to_string()),
        };
        answer(error, declined)
    }

    /// Starts sending `paths`, each as a transfer of its own. An error text
    /// when that cannot be done.
    fn send(&self, peer_id: &str, paths: Vec<PathBuf>) -> Option<String> {
        let mut state = lock(&self.state);
        let contact = state.cfg.contact_by_id(peer_id)?.clone();
        let Some(to) = identity::parse_contact_id(&contact.id) else {
            return Some(ui().contact_invalid.replace("{name}", &contact.name));
        };
        for path in paths {
            let id = net::next_transfer_id();
            let item = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let kind = kind_of(&path);
            state.transfers.insert(
                id,
                TransferView::new(
                    id,
                    Direction::Send,
                    &contact.id,
                    &contact.name,
                    &item,
                    kind,
                    0,
                ),
            );
            self.show(&mut state, id, None);
            let my_name = state.cfg.my_name.clone();
            state.net.send(Command::Send {
                id,
                to,
                path,
                kind,
                my_name,
                peer_name: contact.name.clone(),
            });
        }
        None
    }

    /// Sends each of `documents` to `peer_id`, as [`send`](Self::send) does
    /// files: an error text if it cannot be done.
    fn send_documents(
        &self,
        peer_id: &str,
        documents: Vec<dodavo_core::source::Document>,
    ) -> Option<String> {
        let mut state = lock(&self.state);
        let contact = state.cfg.contact_by_id(peer_id)?.clone();
        let Some(to) = identity::parse_contact_id(&contact.id) else {
            return Some(ui().contact_invalid.replace("{name}", &contact.name));
        };
        for document in documents {
            let id = net::next_transfer_id();
            let kind = if document.items.is_some() {
                Kind::Folder
            } else {
                Kind::File
            };
            let view = TransferView::new(
                id,
                Direction::Send,
                &contact.id,
                &contact.name,
                &document.name,
                kind,
                0,
            );
            state.transfers.insert(id, view);
            // A folder is read only while it is packed.
            if kind == Kind::File {
                state.documents.insert(id, document.uri.clone());
            }
            self.show(&mut state, id, None);
            let my_name = state.cfg.my_name.clone();
            state.net.send(Command::SendDocument {
                id,
                to,
                document,
                my_name,
                peer_name: contact.name.clone(),
            });
        }
        None
    }

    fn check_accept(&self, id: u64, dir: &Path) -> serde_json::Value {
        let (offer, temp_dir, folder) = {
            let state = lock(&self.state);
            let Some(pending) = state.offers.get(&id) else {
                return serde_json::json!({});
            };
            (
                pending.offer.clone(),
                state.temp_dir.clone(),
                state.cfg.received_folder,
            )
        };
        if !dir.is_absolute() {
            return serde_json::json!({ "problem": ui().choose_save_folder });
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            return serde_json::json!({ "problem": folder_unusable(dir, &e) });
        }
        let space = space::check_space(&offer, dir, &temp_dir, folder);
        if !space.fits {
            return serde_json::json!({ "problem": space.text });
        }
        if space.enough == Some(false) {
            let details = ui().space_confirm.replace("{details}", &space.text);
            return serde_json::json!({ "confirm": details });
        }
        serde_json::json!({})
    }

    fn accept(&self, id: u64, dir: PathBuf) {
        let mut state = lock(&self.state);
        let Some(pending) = state.offers.remove(&id) else {
            return;
        };
        state.cfg.last_save_dir = Some(dir.clone());
        let _ = save(&state);
        self.begin_receive(&mut state, id, &pending, dir);
    }

    /// Accepts an offer: its transfer shows from now on.
    fn begin_receive(&self, state: &mut State, id: u64, pending: &PendingOffer, dir: PathBuf) {
        let folder = state.cfg.received_folder;
        let mut view = TransferView::new(
            id,
            Direction::Receive,
            &pending.from_id,
            &pending.from_name,
            &pending.offer.name,
            pending.offer.kind,
            pending.offer.size,
        );
        view.set_received_folder(folder);
        state.transfers.insert(id, view);
        self.show(state, id, None);
        state.net.send(Command::Respond {
            id,
            accept: Some(Accept { dir, folder }),
        });
    }

    /// Passes on the state of a transfer, and whether the computer has to stay
    /// awake.
    fn show(&self, state: &mut State, id: u64, announce: Option<String>) {
        let Some(view) = state.transfers.get(&id) else {
            return;
        };
        let row = view.row();
        state.shown.insert(id, (row.clone(), Instant::now()));
        self.outbox.push(Out::Row { row, announce });
        let busy = state.transfers.values().any(|t| t.row().moving_data);
        if busy != state.busy {
            state.busy = busy;
            self.outbox.push(Out::Busy { busy });
        }
    }

    fn give(&self, id: u64, notice: Option<Notice>, path: Option<PathBuf>) {
        if let Some(notice) = notice {
            self.outbox.push(Out::Notice { id, notice, path });
        }
    }

    fn on_event(&self, event: Event) {
        let mut state = lock(&self.state);
        match event {
            Event::BindFailed(error) => {
                let text = ui().network_problem.replace("{error}", &error);
                self.outbox.push(Out::Status {
                    online: false,
                    text,
                });
            }
            Event::Online(online) => {
                state.online = Some(online);
                // A check that was waiting for the connection.
                #[cfg(target_os = "macos")]
                if online {
                    self.look_for_update(&mut state);
                }
                let text = if online {
                    ui().status_online
                } else {
                    ui().status_offline
                };
                self.outbox.push(Out::Status {
                    online,
                    text: text.to_string(),
                });
            }
            Event::Offer { id, from, offer } => {
                drop(state);
                self.on_offer(id, from.to_string(), offer);
            }
            Event::OverLimit { from, offer } => {
                let (name, _) = peer_name(&state.cfg, &from.to_string(), &offer.sender_name);
                let (title, text) = limit::declined_notice(&name, &offer);
                self.outbox.push(Out::Declined {
                    notice: plain_notice(&title, text),
                });
            }
            Event::Restored {
                id,
                record,
                have,
                goes_on,
            } => {
                let name = peer_name(&state.cfg, &record.peer, &record.peer_name).0;
                let dir = match record.role() {
                    Role::Send => Direction::Send,
                    Role::Receive => Direction::Receive,
                };
                let mut view = TransferView::new(
                    id,
                    dir,
                    &record.peer,
                    &name,
                    &record.offer.name,
                    record.offer.kind,
                    record.offer.size,
                );
                view.set_progress(have, record.offer.size);
                if let Some(folder) = record.received_folder() {
                    view.set_received_folder(folder);
                }
                // Cancelled in the last run: the network side cancels it again.
                let cancelling = record.cancel_pending();
                if cancelling {
                    view.restore_cancel(record.done_sent);
                }
                state.transfers.insert(id, view);
                if let Some(uri) = document_read(&record) {
                    state.documents.insert(id, uri);
                }
                self.show(&mut state, id, None);
                // One that does not go on is reported as it ends, right after.
                if goes_on {
                    let text = if cancelling {
                        tr!(restored_cancelling_balloon, item = record.offer.name)
                    } else {
                        tr!(restored_balloon, item = record.offer.name)
                    };
                    self.give(
                        id,
                        Some(plain_notice(tr!(restored_balloon_title), text)),
                        None,
                    );
                }
            }
            Event::Phase { id, phase } => {
                let notice = state.transfers.get_mut(&id).and_then(|t| t.on_phase(phase));
                let status = state.transfers.get(&id).map(|t| t.row().status);
                self.show(&mut state, id, status);
                self.give(id, notice, None);
            }
            Event::HandedOver { id } => {
                if let Some(view) = state.transfers.get_mut(&id) {
                    // Rows have no focus to go by: only a user's own cancel
                    // gets the word later on.
                    view.on_handed_over(Instant::now(), false);
                }
                let status = state.transfers.get(&id).map(|t| t.row().status);
                self.show(&mut state, id, status);
            }
            Event::CancelWaits { id } => {
                let notice = state
                    .transfers
                    .get_mut(&id)
                    .and_then(TransferView::on_cancel_waits);
                self.show(&mut state, id, None);
                self.give(id, notice, None);
            }
            Event::Progress { id, done, total } => {
                let now = Instant::now();
                if let Some(view) = state.transfers.get_mut(&id) {
                    view.on_progress(now, done, total);
                }
                let due = state
                    .shown
                    .get(&id)
                    .is_none_or(|(_, at)| now.duration_since(*at) >= TICK);
                if due {
                    self.show(&mut state, id, None);
                }
            }
            Event::Path { id, direct } => {
                if let Some(view) = state.transfers.get_mut(&id) {
                    view.on_path(direct);
                }
                self.show(&mut state, id, None);
            }
            Event::Finished { id, outcome } => {
                // The sender gave up while the user was still deciding.
                if let Some(pending) = state.offers.remove(&id) {
                    let notice = plain_notice(
                        tr!(cancelled_balloon_title),
                        tr!(cancelled_by_peer, name = pending.from_name),
                    );
                    self.outbox.push(Out::OfferGone { id, notice });
                }
                let path = match &outcome {
                    Outcome::Completed(path) => path.clone(),
                    _ => None,
                };
                let notice = state
                    .transfers
                    .get_mut(&id)
                    .and_then(|t| t.on_finished(&outcome));
                let status = state.transfers.get(&id).map(|t| t.row().status);
                self.show(&mut state, id, status);
                self.give(id, notice, path);
                if let Some(uri) = state.documents.remove(&id)
                    && !state.documents.values().any(|other| *other == uri)
                {
                    self.outbox.push(Out::DocumentDone { uri });
                }
            }
        }
    }

    fn on_offer(&self, id: u64, from: String, offer: Offer) {
        let (name, known, cfg, temp_dir) = {
            let state = lock(&self.state);
            let (name, known) = peer_name(&state.cfg, &from, &offer.sender_name);
            (name, known, state.cfg.clone(), state.temp_dir.clone())
        };
        let pending = PendingOffer {
            from_id: from.clone(),
            from_name: name.clone(),
            offer,
        };
        // Without enough disk space, on a volume that cannot hold a file this
        // large, or for an item over the sender's limit, ask instead (the
        // window explains why). Off the lock: the first use of Downloads may
        // wait for the user to allow it.
        let auto_dir = cfg.auto_save_dir_for(&from, &pending.offer).filter(|d| {
            std::fs::create_dir_all(d).is_ok() && {
                let space = space::check_space(&pending.offer, d, &temp_dir, cfg.received_folder);
                space.fits && space.enough != Some(false)
            }
        });
        // The same goes for an offer that asks where to save.
        let asked = auto_dir.is_none().then(|| {
            let dir = cfg.suggested_save_dir();
            let space: SpaceOut =
                space::check_space(&pending.offer, &dir, &temp_dir, cfg.received_folder).into();
            (dir, space)
        });
        let mut state = lock(&self.state);
        if let Some(dir) = auto_dir {
            state.cfg.last_save_dir = Some(dir.clone());
            let _ = save(&state);
            self.begin_receive(&mut state, id, &pending, dir);
            let notice = plain_notice(
                tr!(receiving_balloon_title),
                tr!(receiving_balloon, name = name, item = pending.offer.name),
            );
            self.give(id, Some(notice), None);
            return;
        }
        let Some((dir, space)) = asked else {
            return;
        };
        let description = match pending.offer.kind {
            Kind::File => description_file(&pending.offer),
            Kind::Folder => description_folder(&pending.offer),
        };
        let headline = match pending.offer.kind {
            Kind::File => ui().wants_file,
            Kind::Folder => ui().wants_folder,
        }
        .replace("{name}", &name);
        let out = OfferOut {
            id,
            from_id: from.clone(),
            from_name: name.clone(),
            known,
            headline,
            description,
            unknown_warning: (!known).then(|| {
                ui()
                    .unknown_sender_warning
                    .replace("{id}", &short_id(&from))
            }),
            add_label: (!known).then(|| ui().add_to_recipients.replace("{name}", &name)),
            dir,
            space,
            over_limit: limit::notice(&[&pending.offer], cfg.limit_for(&from)),
        };
        let notice = plain_notice(
            tr!(incoming_balloon_title),
            tr!(incoming_balloon, name = name, item = pending.offer.name),
        );
        state.offers.insert(id, pending);
        self.outbox.push(Out::Offer { offer: out, notice });
    }

    #[cfg(target_os = "macos")]
    /// The daily check, when it is due: only when online, without a window
    /// or a job already, not while "Remind Me Later" lasts.
    fn tick_updates(&self, state: &mut State, now: Instant) {
        if now < state.next_update_look {
            return;
        }
        state.next_update_look = now + update::TICK;
        self.look_for_update(state);
    }

    #[cfg(target_os = "macos")]
    fn look_for_update(&self, state: &mut State) {
        if !state.cfg.check_updates
            || state.online != Some(true)
            || state.update_job.is_some()
            || state.update.is_some()
        {
            return;
        }
        let now = now_secs();
        // A last check in the future means the clock was changed: check again.
        let recent = state
            .cfg
            .update_last_check
            .is_some_and(|t| t <= now && now < t + update::INTERVAL.as_secs());
        let snoozed = state.cfg.update_remind_after.is_some_and(|t| now < t);
        if !recent && !snoozed {
            start_check(state, update::Trigger::Automatic);
        }
    }

    #[cfg(target_os = "macos")]
    /// Check for Updates…
    fn check_now(&self) {
        let mut state = lock(&self.state);
        if let Some(info) = state.update.clone() {
            self.outbox.push(update_available(&info, true));
            return;
        }
        // The daily check is on its way: this one takes its result.
        if let Some(job) = &mut state.update_job {
            job.trigger = update::Trigger::Manual;
            return;
        }
        start_check(&mut state, update::Trigger::Manual);
    }

    #[cfg(target_os = "macos")]
    fn download_update(&self) {
        let mut state = lock(&self.state);
        let Some(info) = state.update.clone() else {
            return;
        };
        let fail = |text: String| Out::UpdateFailed { text };
        let dir = match update::update_dir() {
            Ok(dir) => dir,
            Err(e) => {
                self.outbox
                    .push(fail(update::Error::Io(e.to_string()).text()));
                return;
            }
        };
        // Room for the disk image and for what comes out of it.
        if let Some((_, free)) = dodavo_core::platform::disk_free(&dir) {
            let needed = info.size.saturating_mul(2).saturating_add(64 * 1024 * 1024);
            if free < needed {
                self.outbox
                    .push(fail(update::Error::DiskSpace { needed, free }.text()));
                return;
            }
        }
        update::cleanup();
        let job = update::Job::new(update::Trigger::Manual);
        state.update_job = Some(job.clone());
        let dest = dir.join(&info.file_name);
        update::spawn_download(job, info, dest, update_sink);
    }

    #[cfg(target_os = "macos")]
    fn on_update(&self, event: update::Event) {
        let mut state = lock(&self.state);
        // A job that was given up, by a cancel, say.
        let Some(job) = state.update_job.clone().filter(|j| j.id == event.job()) else {
            return;
        };
        match event {
            update::Event::Checked { result, .. } => {
                state.update_job = None;
                let manual = job.trigger == update::Trigger::Manual;
                match result {
                    Ok(found) => {
                        state.cfg.update_last_check = Some(now_secs());
                        let _ = save(&state);
                        match found {
                            Some(info) => {
                                self.outbox.push(update_available(&info, manual));
                                state.update = Some(info);
                            }
                            None if manual => self.outbox.push(Out::UpdateAnswer {
                                text: ui()
                                    .update_latest
                                    .replace("{version}", env!("CARGO_PKG_VERSION")),
                            }),
                            None => {}
                        }
                    }
                    Err(e) if manual => self.outbox.push(Out::UpdateAnswer {
                        text: ui().update_check_failed.replace("{error}", &e.text()),
                    }),
                    Err(_) => {}
                }
            }
            update::Event::Progress { done, total, .. } => {
                let fraction = (total > 0).then(|| done as f64 / total as f64);
                let text = ui()
                    .update_downloading
                    .replace("{done}", &format_size(done))
                    .replace("{total}", &format_size(total))
                    .replace("{percent}", &percent(done, total).to_string());
                self.outbox.push(Out::UpdateProgress { text, fraction });
            }
            update::Event::Verifying { .. } => self.outbox.push(Out::UpdateProgress {
                text: ui().update_verifying.to_string(),
                fraction: None,
            }),
            update::Event::Downloaded { result, .. } => {
                state.update_job = None;
                self.outbox.push(match result {
                    Ok(path) => Out::UpdateReady { path },
                    Err(e) => Out::UpdateFailed { text: e.text() },
                });
            }
        }
    }

    /// About once a second: what is due without a report.
    fn tick(&self) {
        let mut state = lock(&self.state);
        let now = Instant::now();
        #[cfg(target_os = "macos")]
        self.tick_updates(&mut state, now);
        let ids: Vec<u64> = state.transfers.keys().copied().collect();
        for id in ids {
            let Some(view) = state.transfers.get_mut(&id) else {
                continue;
            };
            let notice = view.tick(now);
            let row = view.row();
            if state.shown.get(&id).is_none_or(|(shown, _)| *shown != row) {
                self.show(&mut state, id, None);
            }
            self.give(id, notice, None);
        }
    }
}

#[cfg(target_os = "macos")]
fn update_sink(event: update::Event) {
    if let Some(core) = crate::CORE.get() {
        core.on_update(event);
    }
}

#[cfg(target_os = "macos")]
fn start_check(state: &mut State, trigger: update::Trigger) {
    let job = update::Job::new(trigger);
    state.update_job = Some(job.clone());
    update::spawn_check(job, i18n::lang().code().to_string(), update_sink);
}

#[cfg(target_os = "macos")]
fn update_available(info: &update::UpdateInfo, manual: bool) -> Out {
    Out::UpdateAvailable {
        headline: ui()
            .update_headline
            .replace("{version}", &info.version)
            .replace("{current}", env!("CARGO_PKG_VERSION")),
        notes: info.notes.clone(),
        manual,
    }
}

#[cfg(target_os = "macos")]
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn handle_events(events: mpsc::Receiver<Event>) {
    // Set as soon as `Core::start`, which started this thread, returns. The
    // network runs already, and only a progress event may be lost.
    let core = crate::CORE.wait();
    loop {
        match events.recv_timeout(TICK) {
            Ok(event) => core.on_event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => core.tick(),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// A failure on a thread is told, and the program ends if nothing can go on.
fn install_panic_hook(outbox: &'static Outbox) {
    std::panic::set_hook(Box::new(move |info| {
        let text = ui().panic_message.replace("{info}", &info.to_string());
        match std::thread::current().name() {
            // The thread of the network, or this library's own: nothing can
            // be sent or received any more.
            Some("network" | "events" | "main") => outbox.push(Out::Fatal { text }),
            // A transfer's task fails, and the program goes on (`net::guarded`).
            _ => outbox.push(Out::Error { text }),
        }
    }));
}

/// Saves the settings, which also tells the network whom they block and
/// what the limits are (also if the file cannot be saved). Returns the text
/// of an error.
fn save(state: &State) -> Option<String> {
    state.net.set_blocked(state.cfg.blocked_ids());
    state.net.set_limits(state.cfg.limits());
    state.cfg.save(&state.data_dir).err().map(|e| {
        ui()
            .settings_not_saved
            .replace("{error}", &e.to_string())
    })
}

fn cleaned_id(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What is wrong with what the user entered for a recipient.
fn recipient_problem(
    cfg: &Config,
    my_id: &str,
    name: &str,
    contact_id: &str,
    editing: Option<&str>,
) -> Option<String> {
    let name = name.trim();
    let id = cleaned_id(contact_id);
    if name.is_empty() {
        return Some(ui().enter_recipient_name.to_string());
    }
    if identity::parse_contact_id(&id).is_none() {
        return Some(ui().invalid_contact_id.to_string());
    }
    if id.eq_ignore_ascii_case(my_id) {
        return Some(ui().own_contact_id.to_string());
    }
    let others = || {
        cfg.contacts
            .iter()
            .filter(|c| editing.is_none_or(|old| !c.id.eq_ignore_ascii_case(old)))
    };
    if let Some(c) = others().find(|c| c.id.eq_ignore_ascii_case(&id)) {
        return Some(ui().contact_id_taken.replace("{name}", &c.name));
    }
    if let Some(c) = others().find(|c| c.name.to_lowercase() == name.to_lowercase()) {
        return Some(ui().name_taken.replace("{name}", &c.name));
    }
    None
}

/// Display name for a peer: our own name for them if they are a recipient.
fn peer_name(cfg: &Config, id: &str, fallback: &str) -> (String, bool) {
    match cfg.contact_by_id(id) {
        Some(c) => (c.name.clone(), true),
        None => (
            if fallback.trim().is_empty() {
                tr!(unknown_sender).into()
            } else {
                fallback.trim().to_string()
            },
            false,
        ),
    }
}

/// Shortens a Contact ID for display.
fn short_id(id: &str) -> String {
    if id.len() <= 16 {
        id.to_string()
    } else {
        format!("{}…{}", &id[..8], &id[id.len() - 6..])
    }
}

fn description_file(offer: &Offer) -> String {
    ui()
        .file_description
        .replace("{name}", &offer.name)
        .replace("{size}", &dodavo_core::format::format_size(offer.size))
}

fn description_folder(offer: &Offer) -> String {
    let table = ui();
    i18n::pick_plural(i18n::lang(), &table.folder_description, offer.file_count)
        .replace("{n}", &offer.file_count.to_string())
        .replace("{name}", &offer.name)
        .replace(
            "{size}",
            &dodavo_core::format::format_size(offer.content_size),
        )
}

#[cfg(target_os = "macos")]
impl Settings {
    fn of(cfg: &Config) -> Settings {
        Settings {
            my_name: cfg.my_name.clone(),
            open_at_login: cfg.start_with_windows,
            hide_at_login: cfg.start_minimized,
            show_in_menu_bar: cfg.minimize_to_tray,
            check_updates: cfg.check_updates,
            language: cfg.language.clone(),
            always_save: cfg.always_save,
            // Downloads, when no other folder was chosen.
            always_save_dir: Some(cfg.always_save_folder()),
            auto_accept: cfg.auto_accept,
            received_folder: cfg.received_folder,
            limit: LimitSettings::of(cfg),
        }
    }

    fn apply_to(&self, cfg: &mut Config) {
        self.limit.apply_to(cfg);
        cfg.my_name = self.my_name.trim().to_string();
        cfg.start_with_windows = self.open_at_login;
        cfg.start_minimized = self.hide_at_login;
        cfg.minimize_to_tray = self.show_in_menu_bar;
        cfg.check_updates = self.check_updates;
        cfg.language = self.language.clone();
        cfg.always_save = self.always_save;
        // Unset, it follows Downloads, and needs no permission to be shown.
        cfg.always_save_dir = self
            .always_save_dir
            .clone()
            .filter(|d| *d != dodavo_core::platform::downloads_dir());
        cfg.auto_accept = self.auto_accept;
        cfg.received_folder = self.received_folder;
    }

    fn problem(&self) -> Option<String> {
        if self.my_name.trim().is_empty() {
            return Some(ui().enter_my_name.to_string());
        }
        if self.always_save {
            // Downloads exists, and creating it might ask for permission
            // to use a folder that the settings do not even touch.
            let usable = self.always_save_dir.as_ref().is_some_and(|d| {
                *d == dodavo_core::platform::downloads_dir()
                    || (d.is_absolute() && std::fs::create_dir_all(d).is_ok())
            });
            if !usable {
                return Some(ui().invalid_save_folder.to_string());
            }
        }
        self.limit.problem()
    }
}

#[cfg(not(target_os = "macos"))]
impl Settings {
    fn of(cfg: &Config) -> Settings {
        Settings {
            my_name: cfg.my_name.clone(),
            auto_accept: cfg.auto_accept,
            received_folder: cfg.received_folder,
            limit: LimitSettings::of(cfg),
        }
    }

    fn apply_to(&self, cfg: &mut Config) {
        self.limit.apply_to(cfg);
        cfg.my_name = self.my_name.trim().to_string();
        // Always into Downloads, without asking where.
        cfg.always_save = true;
        cfg.always_save_dir = None;
        cfg.auto_accept = self.auto_accept;
        cfg.received_folder = self.received_folder;
    }

    fn problem(&self) -> Option<String> {
        if self.my_name.trim().is_empty() {
            return Some(ui().enter_my_name.to_string());
        }
        self.limit.problem()
    }
}

/// A language's own name for the Settings window.
fn languages() -> Vec<(&'static str, &'static str)> {
    Lang::ALL
        .iter()
        .map(|l| (l.code(), l.native_name()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty folder of the test's own.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dodavo-ffi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_a_lock_held_by_another_copy_means_already_running() {
        let dir = test_dir("lock-held");
        let first = take_instance_lock(&dir).unwrap();
        assert!(first.is_some());
        // The lock belongs to the open file, so this meets it as another copy does.
        assert!(take_instance_lock(&dir).unwrap().is_none());
        drop(first);
        assert!(take_instance_lock(&dir).unwrap().is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_that_cannot_be_made_is_an_error_with_its_own_text() {
        let dir = test_dir("lock-no-folder");
        let in_the_way = dir.join("file");
        std::fs::write(&in_the_way, b"").unwrap();
        let error = take_instance_lock(&in_the_way.join("profile")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
        assert_eq!(
            folder_error_text(&error),
            ui()
                .error_system
                .replace("{code}", &libc::ENOTDIR.to_string())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `Core` without a network, with one offer waiting.
    fn core_with_offer(id: u64, dir: &Path) -> Core {
        let offer = Offer {
            kind: Kind::File,
            name: "file.txt".into(),
            size: 1,
            file_count: 0,
            content_size: 0,
            largest_file: 1,
            sender_name: "sender".into(),
            token: [0; 16],
            modified: None,
            created: None,
        };
        let pending = PendingOffer {
            from_id: String::new(),
            from_name: String::new(),
            offer,
        };
        Core {
            state: Mutex::new(State {
                cfg: Config::default(),
                data_dir: dir.to_path_buf(),
                temp_dir: dir.to_path_buf(),
                net: NetHandle::idle(),
                online: None,
                transfers: BTreeMap::new(),
                offers: HashMap::from([(id, pending)]),
                shown: HashMap::new(),
                busy: false,
                documents: HashMap::new(),
                #[cfg(target_os = "macos")]
                update_job: None,
                #[cfg(target_os = "macos")]
                update: None,
                #[cfg(target_os = "macos")]
                next_update_look: Instant::now(),
            }),
            my_id: Mutex::new(String::new()),
            outbox: Box::leak(Box::new(Outbox {
                queue: Mutex::new(VecDeque::new()),
                ready: Condvar::new(),
                wake: None,
            })),
            _events: mpsc::channel().0,
        }
    }

    #[test]
    fn accepting_into_a_folder_that_cannot_be_made_says_which_and_why() {
        let dir = test_dir("accept-no-folder");
        let in_the_way = dir.join("file");
        std::fs::write(&in_the_way, b"").unwrap();
        let chosen = in_the_way.join("received");

        let answer = core_with_offer(1, &dir).check_accept(1, &chosen);

        let problem = answer["problem"].as_str().unwrap();
        let reason = ui()
            .error_system
            .replace("{code}", &libc::ENOTDIR.to_string());
        assert!(problem.contains(chosen.to_str().unwrap()), "{problem}");
        assert!(problem.contains(&reason), "{problem}");
        assert!(!problem.contains('{'), "{problem}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An item over the limit of a recipient whose transfers start by
    /// themselves waits for an answer, and its window says why.
    #[test]
    fn an_item_over_the_limit_waits_for_an_answer() {
        let dir = test_dir("offer-over-limit");
        let from = identity::load_or_create(&dir).unwrap().public().to_string();
        let core = core_with_offer(1, &dir);
        let offer = {
            let mut state = lock(&core.state);
            state.cfg.contacts.push(Contact::new("Ana", from.clone()));
            state.cfg.always_save_dir = Some(dir.clone());
            state.cfg.limit_size = true;
            state.cfg.size_limit = 4;
            Offer {
                size: 5,
                ..state.offers[&1].offer.clone()
            }
        };

        core.on_offer(2, from.clone(), offer.clone());

        let shown = lock(&core.outbox.queue).pop_front();
        let Some(Out::Offer { offer: shown, .. }) = shown else {
            panic!("the offer was not shown: {shown:?}");
        };
        assert_eq!(shown.id, 2);
        let said = shown.over_limit.expect("the window says why");
        assert!(!said.contains('{'), "{said}");
        assert!(lock(&core.state).transfers.is_empty());

        // Within the limit, it starts by itself, as it would without one.
        lock(&core.state).cfg.size_limit = 5;
        core.on_offer(3, from, offer);
        assert!(lock(&core.state).transfers.contains_key(&3));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The settings and a recipient's limit take a whole number of a unit,
    /// and say what is wrong with anything else.
    #[test]
    fn a_limit_is_a_whole_number_of_a_unit() {
        let mut cfg = Config::default();
        let mut settings = LimitSettings::of(&cfg);
        assert!(!settings.limit_size);
        assert_eq!((settings.limit_number.as_str(), settings.limit_unit.as_str()), ("1", "gb"));
        // Without the limit, what the field holds is not looked at.
        settings.limit_number = "none".into();
        assert_eq!(settings.problem(), None);
        settings.apply_to(&mut cfg);
        assert_eq!(cfg.size_limit, 1 << 30);
        settings.limit_size = true;
        assert!(settings.problem().is_some());
        settings.limit_number = "250".into();
        settings.limit_unit = "mb".into();
        settings.over_limit = OverLimit::DeclineAndTell;
        assert_eq!(settings.problem(), None);
        settings.apply_to(&mut cfg);
        assert!(cfg.limit_size);
        assert_eq!(cfg.size_limit, 250 << 20);
        assert_eq!(cfg.over_limit, OverLimit::DeclineAndTell);

        let own = Contact {
            limit: ContactLimit::Bytes(2 << 30),
            ..Contact::new("Ana", "id")
        };
        let shown = RecipientLimit::of(&own, &cfg);
        assert_eq!(
            (shown.kind.as_str(), shown.number.as_str(), shown.unit.as_str()),
            ("own", "2", "gb")
        );
        assert_eq!(shown.limit(), Ok(own.limit));
        // The editor offers the general size for a limit of their own.
        let general = RecipientLimit::of(&Contact::new("Bo", "id"), &cfg);
        assert_eq!(
            (general.kind.as_str(), general.number.as_str(), general.unit.as_str()),
            ("general", "250", "mb")
        );
        assert_eq!(general.limit(), Ok(ContactLimit::General));
        let none = RecipientLimit {
            kind: "none".into(),
            ..general.clone()
        };
        assert_eq!(none.limit(), Ok(ContactLimit::Unlimited));
        let bad = RecipientLimit {
            kind: "own".into(),
            number: "0".into(),
            unit: "mb".into(),
        };
        assert!(bad.limit().is_err());
    }

    #[test]
    fn a_taken_contact_id_or_name_says_who_has_it() {
        let new_id = |name| {
            let dir = test_dir(name);
            let id = identity::load_or_create(&dir).unwrap().public().to_string();
            std::fs::remove_dir_all(&dir).unwrap();
            id
        };
        let (taken, free) = (new_id("recipient-taken"), new_id("recipient-free"));
        let mut cfg = Config::default();
        cfg.contacts.push(Contact::new("Ana", taken.clone()));

        assert_eq!(
            recipient_problem(&cfg, "", "Someone", &taken, None),
            Some(ui().contact_id_taken.replace("{name}", "Ana"))
        );
        assert_eq!(
            recipient_problem(&cfg, "", "ANA", &free, None),
            Some(ui().name_taken.replace("{name}", "Ana"))
        );
        // Editing that recipient: nobody else has them.
        assert_eq!(
            recipient_problem(&cfg, "", "Ana", &taken, Some(&taken)),
            None
        );
    }
}
