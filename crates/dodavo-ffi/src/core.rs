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
    logging,
    net::{
        self, Accept, Command, Event, Kind, NetHandle, Offer, Outcome, Role, TransferRecord,
        session::Side,
    },
    sound::{self, SoundEvent},
    space::{self, Space},
    view::{Direction, Notice, Row, TransferView},
};
#[cfg(target_os = "macos")]
use dodavo_core::{
    config::{ChatFrom, SoundChoice},
    format::format_size,
    update,
    view::percent,
};
// The texts of this system's user interface: the Mac's or Android's.
#[cfg(not(target_os = "macos"))]
use dodavo_core::i18n::android as table;
#[cfg(target_os = "macos")]
use dodavo_core::i18n::mac as table;
use serde::{Deserialize, Serialize};

use crate::Wake;

/// The chats, where the user interface shows them: on a Mac so far.
#[cfg(target_os = "macos")]
mod chat;

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
    /// What the status says until the network says more: Android's app
    /// shows it from the start, and a Mac has the text itself.
    status: String,
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
    /// The longest chat message that can be written, in characters.
    #[cfg(target_os = "macos")]
    chat_max_chars: usize,
    /// Each language by its code, with its own name for it.
    languages: Vec<(&'static str, &'static str)>,
    /// Where the log is (see `dodavo_core::logging`), which the user can
    /// show or send; empty where there is none.
    log_path: String,
}

/// What the status says of the network: its problem while it has no
/// endpoint, and else whether it is online, or still connecting.
fn status(state: &State) -> Out {
    let text = match (&state.net_problem, state.online) {
        (Some(error), _) => ui().network_problem.replace("{error}", error),
        (None, None) => ui().status_connecting.to_string(),
        (None, Some(true)) => ui().status_online.to_string(),
        (None, Some(false)) => ui().status_offline.to_string(),
    };
    Out::Status {
        online: state.net_problem.is_none() && state.online == Some(true),
        text,
    }
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
    /// A chat message arrived, and is in the chat with `peer`, whose window
    /// opens for it if it is not open. `said` is the message for the window's
    /// status line and for VoiceOver, and `notice` what tells of it while
    /// the window is not in front. `gone`: how many of the oldest messages
    /// left the chat to make room.
    #[cfg(target_os = "macos")]
    ChatMessage {
        peer: String,
        name: String,
        /// Which change of the chats this is (see `chat::ChatOut`).
        seq: u64,
        entry: chat::EntryOut,
        gone: usize,
        said: String,
        /// The chat could not be saved: this says why.
        problem: Option<String>,
        notice: Notice,
    },
    /// A message of the user's is in the chat with `peer`, on its way.
    #[cfg(target_os = "macos")]
    ChatAdded {
        peer: String,
        seq: u64,
        entry: chat::EntryOut,
        gone: usize,
        problem: Option<String>,
    },
    /// A message of the user's was not delivered: `said` says why. It is the
    /// message at `index` of the chat with `peer`, when the window of that
    /// is open and still has it; `notice` tells of it when no window is.
    #[cfg(target_os = "macos")]
    ChatFailed {
        peer: String,
        name: String,
        seq: u64,
        index: Option<usize>,
        /// What the message said, to send it again.
        text: String,
        said: String,
        notice: Notice,
    },
    /// The other person of the chat with `peer` is typing, which `text`
    /// says, or has stopped. With `announce`, the window in front says it
    /// aloud and plays `sound`.
    #[cfg(target_os = "macos")]
    ChatTyping {
        peer: String,
        text: Option<String>,
        announce: bool,
        sound: Option<SoundOut>,
    },
    /// A sound to play now.
    #[cfg(target_os = "macos")]
    Sound(SoundOut),
    Error {
        text: String,
    },
    /// Nothing can go on: this says why, and the program ends.
    Fatal {
        text: String,
    },
}

/// A sound for the user interface to play: the built-in one of `event` (see
/// `SoundEvent::key`), or the user's `file` in its place, with the built-in
/// one for a file that cannot be played.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SoundOut {
    event: &'static str,
    file: Option<PathBuf>,
}

#[cfg(target_os = "macos")]
impl SoundOut {
    /// What `choice` plays at `event`; nothing for no sound.
    fn of(event: SoundEvent, choice: SoundChoice) -> Option<SoundOut> {
        let file = match choice {
            SoundChoice::Off => return None,
            SoundChoice::Default => None,
            SoundChoice::File(path) => Some(path),
        };
        Some(SoundOut {
            event: event.key(),
            file,
        })
    }
}

/// What plays at `event` now, as the settings say; nothing while sounds are
/// off, or for an event that has none.
#[cfg(target_os = "macos")]
fn sound_out(cfg: &Config, event: SoundEvent) -> Option<SoundOut> {
    cfg.sounds
        .playing(event)
        .and_then(|choice| SoundOut::of(event, choice))
}

/// An event that has a sound, as the settings list it.
#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
struct SoundSetting {
    /// See `SoundEvent::key`.
    event: &'static str,
    name: &'static str,
    /// "default" (the built-in sound), "off" or "file".
    kind: &'static str,
    file: Option<PathBuf>,
    /// What it plays, in words: the name of a file of the user's.
    shown: String,
}

#[cfg(target_os = "macos")]
impl SoundSetting {
    fn of(event: SoundEvent, cfg: &Config) -> SoundSetting {
        let (kind, file, shown) = match cfg.sounds.choice(event) {
            SoundChoice::Default => ("default", None, ui().sound_choice_default.to_string()),
            SoundChoice::Off => ("off", None, ui().sound_choice_off.to_string()),
            SoundChoice::File(path) => {
                let shown = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                ("file", Some(path), shown)
            }
        };
        SoundSetting {
            event: event.key(),
            name: sound_name(event),
            kind,
            file,
            shown,
        }
    }
}

/// The name of an event that has a sound.
#[cfg(target_os = "macos")]
fn sound_name(event: SoundEvent) -> &'static str {
    let table = ui();
    match event {
        SoundEvent::MessageReceived => table.sound_message_received,
        SoundEvent::MessageSent => table.sound_message_sent,
        SoundEvent::Typing => table.sound_typing,
        SoundEvent::MessageFailed => table.sound_message_failed,
        SoundEvent::Incoming => table.sound_incoming,
        SoundEvent::Started => table.sound_started,
        SoundEvent::Completed => table.sound_completed,
        SoundEvent::Failed => table.sound_failed,
        SoundEvent::Cancelled => table.sound_cancelled,
        SoundEvent::ConnectionLost => table.sound_connection_lost,
        SoundEvent::Continues => table.sound_continues,
    }
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

impl SpaceOut {
    /// What the Incoming window says of `space`. A user who wants to be told
    /// of a temporary archive that its volume cannot hold reads it here too:
    /// it is why such an offer did not start by itself.
    fn of(space: Space, cfg: &Config) -> SpaceOut {
        let text = match space.temp_warning.filter(|_| cfg.warn_fat_temp) {
            Some(warning) => format!("{} {warning}", space.text),
            None => space.text,
        };
        SpaceOut {
            text,
            fits: space.fits,
            enough: space.enough,
        }
    }
}

/// What the Mac's Incoming Transfer window says of the offers of one sender
/// that wait for one answer, as `OfferOut` says it of the one that the
/// window opened with.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Serialize)]
struct OffersOut {
    headline: String,
    /// The item, or what several come to.
    description: String,
    /// Each item, when there are several.
    items: Vec<String>,
    /// Says that something here is over the sender's limit, when it is.
    over_limit: Option<String>,
    space: SpaceOut,
}

/// The offers that a call is about: the one, or the several.
fn which(id: Option<u64>, ids: Vec<u64>) -> Vec<u64> {
    if ids.is_empty() {
        id.into_iter().collect()
    } else {
        ids
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
    /// Each of `paths` to each of the recipients.
    Send {
        peer_ids: Vec<String>,
        paths: Vec<PathBuf>,
    },
    /// Documents that the user chose in Android's pickers, read where they
    /// are (see `dodavo_core::source`).
    SendDocuments {
        peer_ids: Vec<String>,
        documents: Vec<dodavo_core::source::Document>,
    },
    /// The documents that sends still read: what the app must keep its
    /// permission for.
    Documents,
    /// Whether the offer `id`, or the offers `ids` that are answered
    /// together, fit into `dir`. Without a folder (Android): Downloads.
    Space {
        #[serde(default)]
        id: Option<u64>,
        #[serde(default)]
        ids: Vec<u64>,
        #[serde(default)]
        dir: Option<PathBuf>,
    },
    /// What stands in the way of accepting the offer `id`, or the offers
    /// `ids` together, into `dir`. With `ignore_temp`, the user has heard
    /// of the temporary archive that its volume cannot hold, and goes on
    /// anyway.
    CheckAccept {
        #[serde(default)]
        id: Option<u64>,
        #[serde(default)]
        ids: Vec<u64>,
        #[serde(default)]
        dir: Option<PathBuf>,
        #[serde(default)]
        ignore_temp: bool,
    },
    /// The Mac: what the Incoming Transfer window says of the offers `ids`
    /// of one sender, which wait for one answer (see `OffersOut`).
    #[cfg(target_os = "macos")]
    Offers {
        ids: Vec<u64>,
        dir: PathBuf,
    },
    /// The Mac: what to tell the user before `paths` are sent (see
    /// `space::send_temp_warning`).
    #[cfg(target_os = "macos")]
    CheckSend {
        paths: Vec<PathBuf>,
    },
    /// The Mac: the user chose another folder for temporary files where
    /// Dodavo told of one that cannot hold an archive.
    #[cfg(target_os = "macos")]
    SetTempDir {
        dir: PathBuf,
    },
    /// The Mac: whether to tell of a temporary archive that its volume
    /// cannot hold (`Config::warn_fat_temp`).
    #[cfg(target_os = "macos")]
    WarnFatTemp {
        on: bool,
    },
    /// Accepts the offer `id`, or the offers `ids` together.
    Accept {
        #[serde(default)]
        id: Option<u64>,
        #[serde(default)]
        ids: Vec<u64>,
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
    /// A line for the log from the user interface: a text of its own, never
    /// a name, a path or what the user wrote (see `dodavo_core::logging`).
    /// `level` is `error`, `warn`, `info` or `debug`.
    Log {
        level: String,
        message: String,
    },
    /// Waits a moment until the log has all lines before, so that the user
    /// can be shown or send it. The answer has its path, empty where there
    /// is none.
    FlushLog,
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
    /// The Mac: opens the chat with the person whose Contact ID is `peer`,
    /// or reads it again (see `chat::ChatOut`). `name` is what they call
    /// themselves, for someone who is not a recipient.
    #[cfg(target_os = "macos")]
    ChatOpen {
        peer: String,
        #[serde(default)]
        name: String,
    },
    /// The window of the chat with `peer` closed.
    #[cfg(target_os = "macos")]
    ChatClosed {
        peer: String,
    },
    /// Sends a chat message. It is in the chat at once (`Out::ChatAdded`);
    /// whether it arrived comes later, as a sound or as `Out::ChatFailed`.
    #[cfg(target_os = "macos")]
    ChatSend {
        peer: String,
        text: String,
    },
    /// One message to each of several recipients, as a private message.
    #[cfg(target_os = "macos")]
    ChatSendSeveral {
        peer_ids: Vec<String>,
        text: String,
    },
    /// The user is writing a message to `peer`, or has stopped.
    #[cfg(target_os = "macos")]
    ChatTyping {
        peer: String,
        on: bool,
    },
    /// Deletes the chat with `peer` from this Mac.
    #[cfg(target_os = "macos")]
    ChatDelete {
        peer: String,
    },
    /// Deletes every chat from this Mac.
    #[cfg(target_os = "macos")]
    ChatDeleteAll,
    /// Whether any chat is kept.
    #[cfg(target_os = "macos")]
    AnyChats,
    /// The Mac: whether sounds play (`on`), and the events that have one
    /// with what each plays (`events`, see `SoundSetting`).
    #[cfg(target_os = "macos")]
    Sounds,
    #[cfg(target_os = "macos")]
    SetSoundsOn {
        on: bool,
    },
    /// What `event` plays from now on: `kind` is "default", "off", or
    /// "file" with `file`.
    #[cfg(target_os = "macos")]
    SetSound {
        event: String,
        kind: String,
        #[serde(default)]
        file: Option<PathBuf>,
    },
    /// Every event plays its built-in sound again.
    #[cfg(target_os = "macos")]
    ResetSounds,
    /// Plays what `event` is set to, also while sounds are off: the
    /// settings try a sound with it.
    #[cfg(target_os = "macos")]
    TrySound {
        event: String,
    },
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
    /// The folder that the user chose for temporary files, or none for the
    /// system's place.
    #[serde(default)]
    temp_dir: Option<PathBuf>,
    /// Whether to tell of a temporary archive that its volume cannot hold.
    #[serde(default = "yes")]
    warn_fat_temp: bool,
    /// Who may send chat messages: "anyone" (who is not blocked) or
    /// "recipients".
    #[serde(default)]
    chat_from: ChatFrom,
    /// Tell the other person of a chat when the user is writing a message.
    #[serde(default = "yes")]
    send_typing: bool,
    /// Say aloud, and with a sound, that the other person of the chat in
    /// front is writing a message.
    #[serde(default = "yes")]
    announce_typing: bool,
    #[serde(flatten)]
    limit: LimitSettings,
}

#[cfg(target_os = "macos")]
fn yes() -> bool {
    true
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
    /// The profile's folder for temporary archives in the system's place
    /// (see `State::temp_in_use`).
    temp_dir: PathBuf,
    /// The `profile` this copy runs with.
    profile: Option<String>,
    net: NetHandle,
    /// Whether the network reports us online (`None` until it says).
    online: Option<bool>,
    /// Why the network has no endpoint, while it has none (see
    /// `Event::BindFailed`).
    net_problem: Option<String>,
    transfers: BTreeMap<u64, TransferView>,
    offers: HashMap<u64, PendingOffer>,
    /// Each receive that is under way: its offer, the folder it is saved
    /// into and what is kept of a folder, by which the room that it still
    /// takes is counted (see `State::arriving`).
    receiving: HashMap<u64, (Offer, PathBuf, ReceivedFolder)>,
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
    /// The chats whose windows are open, and the messages on their way.
    #[cfg(target_os = "macos")]
    chats: chat::Chats,
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
        let (mut cfg, cfg_error) = match Config::load(&data_dir) {
            Ok(cfg) => (cfg, None),
            Err(e) => (Config::default(), Some(e)),
        };
        // A folder for temporary files that was chosen before is looked at
        // for leftovers at each start, until nothing of Dodavo's is in it.
        if cfg.forget_temp_dirs(profile.as_deref(), net::session::has_temp_archives) {
            let _ = cfg.save(&data_dir);
        }
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
            status: ui().status_connecting.to_string(),
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
            #[cfg(target_os = "macos")]
            chat_max_chars: chat::Chats::MAX_CHARS,
            languages: languages(),
            ..Started::default()
        };

        match take_instance_lock(&data_dir) {
            // Kept for the life of the program: it ends with the process.
            Ok(Some(instance)) => {
                std::mem::forget(instance);
                // This copy's from here on: another one that finds the lock
                // taken never writes it. What was logged before is kept.
                #[cfg(target_os = "macos")]
                let ui_name = "Mac";
                #[cfg(not(target_os = "macos"))]
                let ui_name = "Android";
                let extra = [format!(
                    "profile: {}; start hidden: {}",
                    if profile.is_some() {
                        "named"
                    } else {
                        "default"
                    },
                    options.hidden
                )];
                if let Some(path) = logging::init(&data_dir, ui_name, &extra) {
                    started.log_path = path.display().to_string();
                }
                dodavo_core::log_info!("language {}; {}", i18n::lang().code(), cfg.log_summary());
            }
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
                    dodavo_core::log_error!(
                        "no identity, so Dodavo cannot start: {}",
                        logging::cause(&e)
                    );
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
        // Only a copy that runs: the chats are the running copy's otherwise.
        #[cfg(target_os = "macos")]
        let chats = chat::Chats::new(&data_dir, secret.is_some());
        let my_id;
        let net;
        match secret {
            Some(secret) => {
                my_id = secret.public().to_string();
                started.my_id = my_id.clone();
                let sink = events.clone();
                // Chat connections are taken only where chats are shown, so
                // that no message is confirmed to its sender and then shown
                // to nobody: on a Mac so far.
                #[cfg(target_os = "macos")]
                let chat = Some(cfg.chat_policy());
                #[cfg(not(target_os = "macos"))]
                let chat = None;
                net = net::start_with_chat(
                    secret,
                    data_dir.clone(),
                    net::TempDirs {
                        default: temp_dir.clone(),
                        chosen: cfg.temp_dir_for(profile.as_deref()),
                        before: cfg.temp_dirs_before_for(profile.as_deref()),
                    },
                    cfg.blocked_ids(),
                    cfg.limits(),
                    chat,
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
                profile: profile.clone(),
                net,
                online: None,
                net_problem: None,
                transfers: BTreeMap::new(),
                offers: HashMap::new(),
                receiving: HashMap::new(),
                shown: HashMap::new(),
                busy: false,
                documents: HashMap::new(),
                #[cfg(target_os = "macos")]
                update_job: None,
                #[cfg(target_os = "macos")]
                update: None,
                #[cfg(target_os = "macos")]
                next_update_look: Instant::now() + update::START_DELAY,
                #[cfg(target_os = "macos")]
                chats,
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
        self.outbox.push(status(&state));
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
            Call::Send { peer_ids, paths } => {
                json(serde_json::json!({ "error": self.send(&peer_ids, paths) }))
            }
            Call::SendDocuments {
                peer_ids,
                documents,
            } => json(serde_json::json!({ "error": self.send_documents(&peer_ids, documents) })),
            Call::Documents => {
                let state = lock(&self.state);
                let uris: BTreeSet<&String> = state.documents.values().collect();
                json(serde_json::json!(uris))
            }
            Call::Space { id, ids, dir } => {
                let dir = dir.unwrap_or_else(dodavo_core::platform::downloads_dir);
                let state = lock(&self.state);
                match state.space_for(&which(id, ids), &dir) {
                    Some(space) => {
                        serde_json::to_string(&SpaceOut::of(space, &state.cfg)).unwrap_or_default()
                    }
                    None => "null".into(),
                }
            }
            Call::CheckAccept {
                id,
                ids,
                dir,
                ignore_temp,
            } => {
                let dir = dir.unwrap_or_else(dodavo_core::platform::downloads_dir);
                json(self.check_accept(&which(id, ids), &dir, ignore_temp))
            }
            #[cfg(target_os = "macos")]
            Call::Offers { ids, dir } => {
                let state = lock(&self.state);
                match offers_out(&state, &ids, &dir) {
                    Some(out) => serde_json::to_string(&out).unwrap_or_default(),
                    None => "null".into(),
                }
            }
            #[cfg(target_os = "macos")]
            Call::CheckSend { paths } => {
                let (warn, temp_dir) = {
                    let state = lock(&self.state);
                    (state.cfg.warn_fat_temp, state.temp_in_use())
                };
                // Off the lock: it may read the listing of a folder.
                let warning = warn
                    .then(|| {
                        paths
                            .iter()
                            .find_map(|path| space::send_temp_warning(path, &temp_dir))
                    })
                    .flatten();
                json(serde_json::json!({ "temp_warning": warning }))
            }
            #[cfg(target_os = "macos")]
            Call::SetTempDir { dir } => {
                if !(dir.is_absolute() && std::fs::create_dir_all(&dir).is_ok()) {
                    return json(serde_json::json!({ "error": ui().invalid_temp_folder }));
                }
                let mut state = lock(&self.state);
                state.cfg.set_temp_dir(Some(dir));
                json(serde_json::json!({ "error": save(&state) }))
            }
            #[cfg(target_os = "macos")]
            Call::WarnFatTemp { on } => {
                let mut state = lock(&self.state);
                state.cfg.warn_fat_temp = on;
                json(serde_json::json!({ "error": save(&state) }))
            }
            Call::Accept { id, ids, dir } => {
                let dir = dir.unwrap_or_else(dodavo_core::platform::downloads_dir);
                self.accept(&which(id, ids), dir);
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
            Call::Log { level, message } => {
                logging::external(&level, &message);
                "null".into()
            }
            Call::FlushLog => {
                logging::flush(Duration::from_millis(500));
                json(serde_json::json!({
                    "path": logging::path().map(|p| p.display().to_string()).unwrap_or_default(),
                }))
            }
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
            #[cfg(target_os = "macos")]
            Call::ChatOpen { peer, name } => {
                serde_json::to_string(&self.chat_open(&peer, &name)).unwrap_or_default()
            }
            #[cfg(target_os = "macos")]
            Call::ChatClosed { peer } => {
                self.chat_closed(&peer);
                "null".into()
            }
            #[cfg(target_os = "macos")]
            Call::ChatSend { peer, text } => {
                json(serde_json::json!({ "error": self.chat_send(&peer, &text) }))
            }
            #[cfg(target_os = "macos")]
            Call::ChatSendSeveral { peer_ids, text } => {
                json(serde_json::json!({ "error": self.chat_send_several(&peer_ids, &text) }))
            }
            #[cfg(target_os = "macos")]
            Call::ChatTyping { peer, on } => {
                self.chat_typing(&peer, on);
                "null".into()
            }
            #[cfg(target_os = "macos")]
            Call::ChatDelete { peer } => {
                json(serde_json::json!({ "error": self.chat_delete(&peer) }))
            }
            #[cfg(target_os = "macos")]
            Call::ChatDeleteAll => json(serde_json::json!({ "error": self.chat_delete_all() })),
            #[cfg(target_os = "macos")]
            Call::AnyChats => json(serde_json::json!(self.any_chats())),
            #[cfg(target_os = "macos")]
            Call::Sounds => {
                let state = lock(&self.state);
                let events: Vec<SoundSetting> = SoundEvent::ALL
                    .into_iter()
                    .map(|event| SoundSetting::of(event, &state.cfg))
                    .collect();
                json(serde_json::json!({ "on": state.cfg.sounds.on, "events": events }))
            }
            #[cfg(target_os = "macos")]
            Call::SetSoundsOn { on } => {
                let mut state = lock(&self.state);
                state.cfg.sounds.on = on;
                json(serde_json::json!({ "error": save(&state) }))
            }
            #[cfg(target_os = "macos")]
            Call::SetSound { event, kind, file } => {
                let choice = match (kind.as_str(), file) {
                    ("off", _) => SoundChoice::Off,
                    ("file", Some(path)) => SoundChoice::File(path),
                    _ => SoundChoice::Default,
                };
                let mut state = lock(&self.state);
                if let Some(event) = SoundEvent::from_key(&event) {
                    state.cfg.sounds.set(event, choice);
                }
                json(serde_json::json!({ "error": save(&state) }))
            }
            #[cfg(target_os = "macos")]
            Call::ResetSounds => {
                let mut state = lock(&self.state);
                for event in SoundEvent::ALL {
                    state.cfg.sounds.set(event, SoundChoice::Default);
                }
                json(serde_json::json!({ "error": save(&state) }))
            }
            #[cfg(target_os = "macos")]
            Call::TrySound { event } => {
                let state = lock(&self.state);
                let sound = SoundEvent::from_key(&event)
                    .and_then(|event| SoundOut::of(event, state.cfg.sounds.choice(event)));
                if let Some(sound) = sound {
                    self.outbox.push(Out::Sound(sound));
                }
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
                dodavo_core::log_info!("the app quits");
                // Off the lock: closing takes a moment, and events still come.
                let net = lock(&self.state).net.clone();
                net.shutdown(Duration::from_secs(3));
                logging::flush(Duration::from_secs(1));
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
            if !state.net.is_up() {
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

    /// The recipients a send names, or why it cannot start: nothing goes to
    /// some of them only.
    fn recipients(state: &State, peer_ids: &[String]) -> Result<Vec<Contact>, String> {
        let mut contacts = Vec::new();
        for peer_id in peer_ids {
            // Removed, or given another Contact ID, while the send was on its
            // way.
            let Some(contact) = state.cfg.contact_by_id(peer_id) else {
                return Err(ui().recipient_gone.to_string());
            };
            if identity::parse_contact_id(&contact.id).is_none() {
                return Err(ui().contact_invalid.replace("{name}", &contact.name));
            }
            // Named twice, a recipient still gets each item once.
            if !contacts.contains(contact) {
                contacts.push(contact.clone());
            }
        }
        if contacts.is_empty() {
            return Err(ui().recipient_gone.to_string());
        }
        Ok(contacts)
    }

    /// Starts sending each of `paths` to each of the recipients, every one
    /// as a transfer of its own. An error text when that cannot be done.
    fn send(&self, peer_ids: &[String], paths: Vec<PathBuf>) -> Option<String> {
        let mut state = lock(&self.state);
        let contacts = match Self::recipients(&state, peer_ids) {
            Ok(contacts) => contacts,
            Err(error) => return Some(error),
        };
        for contact in &contacts {
            let Some(to) = identity::parse_contact_id(&contact.id) else {
                continue;
            };
            for path in &paths {
                let id = net::next_transfer_id();
                let item = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let kind = kind_of(path);
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
                    path: path.clone(),
                    kind,
                    my_name,
                    peer_name: contact.name.clone(),
                });
            }
        }
        // Once for all the items that start together.
        if !paths.is_empty() {
            self.sound(&state, SoundEvent::Started);
        }
        None
    }

    /// Sends each of `documents` to each of the recipients, as
    /// [`send`](Self::send) does files: an error text if it cannot be done.
    fn send_documents(
        &self,
        peer_ids: &[String],
        documents: Vec<dodavo_core::source::Document>,
    ) -> Option<String> {
        let mut state = lock(&self.state);
        let contacts = match Self::recipients(&state, peer_ids) {
            Ok(contacts) => contacts,
            Err(error) => return Some(error),
        };
        for contact in &contacts {
            let Some(to) = identity::parse_contact_id(&contact.id) else {
                continue;
            };
            for document in &documents {
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
                // A folder is read only while it is packed. A file that goes
                // to several recipients is read until the last of them ends.
                if kind == Kind::File {
                    state.documents.insert(id, document.uri.clone());
                }
                self.show(&mut state, id, None);
                let my_name = state.cfg.my_name.clone();
                state.net.send(Command::SendDocument {
                    id,
                    to,
                    document: document.clone(),
                    my_name,
                    peer_name: contact.name.clone(),
                });
            }
        }
        if !documents.is_empty() {
            self.sound(&state, SoundEvent::Started);
        }
        None
    }

    /// What stands in the way of accepting the offers `ids` together into
    /// `dir`: nothing for offers that wait no more.
    fn check_accept(&self, ids: &[u64], dir: &Path, ignore_temp: bool) -> serde_json::Value {
        let (offers, temp_dir, folder, warn, arriving) = {
            let state = lock(&self.state);
            let offers = state.waiting(ids);
            if offers.is_empty() {
                return serde_json::json!({});
            }
            (
                offers,
                state.temp_in_use(),
                state.cfg.received_folder,
                state.cfg.warn_fat_temp && !ignore_temp,
                state.arriving(),
            )
        };
        if !dir.is_absolute() {
            return serde_json::json!({ "problem": ui().choose_save_folder });
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            return serde_json::json!({ "problem": folder_unusable(dir, &e) });
        }
        let offers: Vec<&Offer> = offers.iter().collect();
        let space = space::check_space_of(&offers, dir, &temp_dir, folder, &arriving);
        if !space.fits {
            return serde_json::json!({ "problem": space.text });
        }
        if let Some(warning) = space.temp_warning.filter(|_| warn) {
            return serde_json::json!({ "temp_warning": warning });
        }
        if space.enough == Some(false) {
            let details = ui().space_confirm.replace("{details}", &space.text);
            return serde_json::json!({ "confirm": details });
        }
        serde_json::json!({})
    }

    /// Accepts those of the offers `ids` that still wait, into `dir`.
    fn accept(&self, ids: &[u64], dir: PathBuf) {
        let mut state = lock(&self.state);
        let accepted: Vec<(u64, PendingOffer)> = ids
            .iter()
            .filter_map(|id| Some((*id, state.offers.remove(id)?)))
            .collect();
        if accepted.is_empty() {
            return;
        }
        // Several offers answered together go to one folder: one save is
        // enough, and one sound.
        state.cfg.last_save_dir = Some(dir.clone());
        let _ = save(&state);
        for (id, pending) in &accepted {
            self.begin_receive(&mut state, *id, pending, dir.clone());
        }
        // The user's answer; a transfer that starts by itself has the sound
        // of an incoming item.
        self.sound(&state, SoundEvent::Started);
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
        state
            .receiving
            .insert(id, (pending.offer.clone(), dir.clone(), folder));
        self.show(state, id, None);
        state.net.send(Command::Respond {
            id,
            accept: Some(Accept { dir, folder }),
        });
    }

    /// Has the user interface play the sound of `event`, if the settings
    /// have one for it. A Mac plays them so far. Before what tells of the
    /// same event: a notification that follows a sound has none of its own.
    fn sound(&self, state: &State, event: SoundEvent) {
        #[cfg(target_os = "macos")]
        if let Some(sound) = sound_out(&state.cfg, event) {
            self.outbox.push(Out::Sound(sound));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (state, event);
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
            // What the network says of itself, each in place of what it said before.
            Event::BindFailed(error) => {
                (state.online, state.net_problem) = (None, Some(error));
                self.outbox.push(status(&state));
            }
            Event::Connecting => {
                (state.online, state.net_problem) = (None, None);
                self.outbox.push(status(&state));
            }
            Event::Online(online) => {
                (state.online, state.net_problem) = (Some(online), None);
                // A check that was waiting for the connection.
                #[cfg(target_os = "macos")]
                if online {
                    self.look_for_update(&mut state);
                }
                self.outbox.push(status(&state));
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
                if let Side::Receive {
                    dest_dir, folder, ..
                } = &record.side
                {
                    let receive = (record.offer.clone(), dest_dir.clone(), *folder);
                    state.receiving.insert(id, receive);
                }
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
                let before = state.transfers.get(&id).map(TransferView::phase);
                let notice = state.transfers.get_mut(&id).and_then(|t| t.on_phase(phase));
                let status = state.transfers.get(&id).map(|t| t.row().status);
                self.show(&mut state, id, status);
                // A lost connection, and going on after one.
                if let Some(event) = before.and_then(|before| sound::phase_sound(before, phase)) {
                    self.sound(&state, event);
                }
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
                    self.sound(&state, SoundEvent::Cancelled);
                    self.outbox.push(Out::OfferGone { id, notice });
                }
                let path = match &outcome {
                    Outcome::Completed(path) => path.clone(),
                    _ => None,
                };
                state.receiving.remove(&id);
                // A transfer ends once.
                let ended = state.transfers.get(&id).is_some_and(TransferView::is_active);
                let notice = state
                    .transfers
                    .get_mut(&id)
                    .and_then(|t| t.on_finished(&outcome));
                let status = state.transfers.get(&id).map(|t| t.row().status);
                self.show(&mut state, id, status);
                if ended {
                    self.sound(&state, sound::outcome_sound(&outcome));
                }
                self.give(id, notice, path);
                if let Some(uri) = state.documents.remove(&id)
                    && !state.documents.values().any(|other| *other == uri)
                {
                    self.outbox.push(Out::DocumentDone { uri });
                }
            }
            #[cfg(target_os = "macos")]
            Event::Chat { from, name, text } => {
                self.on_chat(&mut state, from.to_string(), name, text);
            }
            #[cfg(target_os = "macos")]
            Event::ChatSent { id, .. } => self.on_chat_sent(&mut state, id),
            #[cfg(target_os = "macos")]
            Event::ChatFailed { id, error, .. } => self.on_chat_failed(&mut state, id, &error),
            #[cfg(target_os = "macos")]
            Event::Typing { from, on } => self.on_peer_typing(&mut state, &from.to_string(), on),
            // The network here is started without chat, so nobody's message
            // is taken and none of these comes.
            #[cfg(not(target_os = "macos"))]
            Event::Chat { .. }
            | Event::ChatSent { .. }
            | Event::ChatFailed { .. }
            | Event::Typing { .. } => {}
        }
    }

    fn on_offer(&self, id: u64, from: String, offer: Offer) {
        let (name, known, cfg, temp_dir, arriving) = {
            let state = lock(&self.state);
            let (name, known) = peer_name(&state.cfg, &from, &offer.sender_name);
            let (cfg, temp_dir) = (state.cfg.clone(), state.temp_in_use());
            (name, known, cfg, temp_dir, state.arriving())
        };
        let pending = PendingOffer {
            from_id: from.clone(),
            from_name: name.clone(),
            offer,
        };
        // Without enough disk space beside what is still arriving, on a
        // volume that cannot hold a file this large, with a temporary
        // archive that its volume cannot hold, or for an item over the
        // sender's limit, ask instead (the window explains why). Off the
        // lock: the first use of Downloads may wait for the user to allow
        // it.
        let folder = cfg.received_folder;
        let auto_dir = cfg.auto_save_dir_for(&from, &pending.offer).filter(|d| {
            if let Err(e) = std::fs::create_dir_all(d) {
                dodavo_core::log_info!(
                    "#{id} the folder to save to cannot be made: {}",
                    logging::io(&e)
                );
                return false;
            }
            let space = space::check_space(&pending.offer, d, &temp_dir, folder, &arriving);
            let fat_temp = cfg.warn_fat_temp && space.temp_warning.is_some();
            let starts = space.fits && space.enough != Some(false) && !fat_temp;
            if !starts {
                dodavo_core::log_info!(
                    "#{id} the destination holds it {}, enough room {:?}, FAT32 temporary folder {fat_temp}",
                    space.fits,
                    space.enough
                );
            }
            starts
        });
        dodavo_core::log_info!(
            "#{id} from {}: {}",
            if known {
                "a recipient"
            } else {
                "someone who is no recipient"
            },
            if auto_dir.is_some() {
                "it starts by itself"
            } else {
                "the user is asked"
            }
        );
        // The same goes for an offer that asks where to save.
        let asked = auto_dir.is_none().then(|| {
            let dir = cfg.suggested_save_dir();
            let space = SpaceOut::of(
                space::check_space(&pending.offer, &dir, &temp_dir, folder, &arriving),
                &cfg,
            );
            (dir, space)
        });
        let mut state = lock(&self.state);
        self.sound(&state, SoundEvent::Incoming);
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
                    .push(fail(update::Error::io(&e).text()));
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
        #[cfg(target_os = "macos")]
        self.tick_chats(&mut state, now);
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
        // First, and on the disk before the program may end.
        dodavo_core::logging::panic(info);
        let info = info.to_string();
        match std::thread::current().name() {
            // The thread of the network, or this library's own: nothing can
            // be sent or received any more.
            Some("network" | "events" | "main") => outbox.push(Out::Fatal {
                text: ui().panic_message.replace("{info}", &info),
            }),
            // A transfer's task fails, and the program goes on (`net::guarded`).
            _ => outbox.push(Out::Error {
                text: ui().panic_message_goes_on.replace("{info}", &info),
            }),
        }
    }));
}

impl State {
    /// The receives that are under way, for the room that they still take
    /// on the disk, which a new item has to fit beside (see
    /// `space::check_space_of`).
    fn arriving(&self) -> Vec<space::Arriving> {
        self.receiving
            .iter()
            .filter_map(|(id, (offer, dir, folder))| {
                let view = self.transfers.get(id).filter(|view| view.is_active())?;
                Some(space::Arriving {
                    offer: offer.clone(),
                    dir: dir.clone(),
                    folder: *folder,
                    done: view.done(),
                    unpacking: view.phase() == Some(net::Phase::Unpacking),
                })
            })
            .collect()
    }

    /// Those of the offers `ids` that still wait for an answer, in that
    /// order.
    fn waiting(&self, ids: &[u64]) -> Vec<Offer> {
        ids.iter()
            .filter_map(|id| self.offers.get(id))
            .map(|pending| pending.offer.clone())
            .collect()
    }

    /// Whether the offers `ids` fit into `dir` together; `None` when none
    /// of them waits any more.
    fn space_for(&self, ids: &[u64], dir: &Path) -> Option<Space> {
        let offers = self.waiting(ids);
        if offers.is_empty() {
            return None;
        }
        let offers: Vec<&Offer> = offers.iter().collect();
        Some(space::check_space_of(
            &offers,
            dir,
            &self.temp_in_use(),
            self.cfg.received_folder,
            &self.arriving(),
        ))
    }

    /// The folder for the temporary archives of new transfers: the user's
    /// choice, or the system's place (see `Config::temp_dir_for`).
    fn temp_in_use(&self) -> PathBuf {
        self.cfg
            .temp_dir_for(self.profile.as_deref())
            .unwrap_or_else(|| self.temp_dir.clone())
    }
}

/// Saves the settings, which also tells the network whom they block, what
/// the limits are, who may send chat messages and where temporary archives
/// go (also if the file cannot be saved). Returns the text of an error.
fn save(state: &State) -> Option<String> {
    state.net.set_blocked(state.cfg.blocked_ids());
    state.net.set_limits(state.cfg.limits());
    state.net.set_chat(state.cfg.chat_policy());
    state
        .net
        .set_temp_dir(state.cfg.temp_dir_for(state.profile.as_deref()));
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

/// What the Mac's Incoming Transfer window says of the offers `ids`, saved
/// into `dir`; `None` when none of them waits any more.
#[cfg(target_os = "macos")]
fn offers_out(state: &State, ids: &[u64], dir: &Path) -> Option<OffersOut> {
    let pending: Vec<&PendingOffer> = ids.iter().filter_map(|id| state.offers.get(id)).collect();
    let first = pending.first()?;
    let name = &first.from_name;
    // Under the name it will be saved as: a peer's name may hold line
    // ends, which would fake items of the list.
    let describe = |offer: &Offer| {
        let named = Offer {
            name: archive::sanitize_file_name(&offer.name),
            ..offer.clone()
        };
        match offer.kind {
            Kind::File => description_file(&named),
            Kind::Folder => description_folder(&named),
        }
    };
    let (headline, description, items) = match pending.as_slice() {
        [one] => {
            let headline = match one.offer.kind {
                Kind::File => ui().wants_file,
                Kind::Folder => ui().wants_folder,
            };
            (headline.replace("{name}", name), describe(&one.offer), Vec::new())
        }
        several => {
            let count = several.len() as u64;
            let headline = i18n::pick_plural(i18n::lang(), &ui().wants_items, count)
                .replace("{n}", &count.to_string())
                .replace("{name}", name);
            let total = several
                .iter()
                .map(|pending| match pending.offer.kind {
                    Kind::File => pending.offer.size,
                    Kind::Folder => pending.offer.content_size,
                })
                .fold(0u64, u64::saturating_add);
            let total = ui()
                .items_total
                .replace("{size}", &dodavo_core::format::format_size(total));
            let items = several.iter().map(|pending| describe(&pending.offer)).collect();
            (headline, total, items)
        }
    };
    let offers: Vec<&Offer> = pending.iter().map(|pending| &pending.offer).collect();
    Some(OffersOut {
        headline,
        description,
        items,
        over_limit: limit::notice(&offers, state.cfg.limit_for(&first.from_id)),
        space: SpaceOut::of(state.space_for(ids, dir)?, &state.cfg),
    })
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
            temp_dir: cfg.temp_dir.clone(),
            warn_fat_temp: cfg.warn_fat_temp,
            chat_from: cfg.chat_from,
            send_typing: cfg.send_typing,
            announce_typing: cfg.announce_typing,
            limit: LimitSettings::of(cfg),
        }
    }

    fn apply_to(&self, cfg: &mut Config) {
        self.limit.apply_to(cfg);
        cfg.set_temp_dir(self.temp_dir.clone());
        cfg.warn_fat_temp = self.warn_fat_temp;
        cfg.chat_from = self.chat_from;
        cfg.send_typing = self.send_typing;
        cfg.announce_typing = self.announce_typing;
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
        if let Some(dir) = &self.temp_dir
            && !(dir.is_absolute() && std::fs::create_dir_all(dir).is_ok())
        {
            return Some(ui().invalid_temp_folder.to_string());
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
    pub(in crate::core) fn test_dir(name: &str) -> PathBuf {
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

    /// A send to someone who is no longer a recipient starts nothing, and
    /// says why.
    #[test]
    fn sending_to_a_removed_recipient_says_so() {
        let dir = test_dir("recipient-gone");
        let core = core_with_offer(1, &dir);
        let id = ["ab".repeat(32)];
        let gone = Some(ui().recipient_gone.to_string());
        assert_eq!(core.send(&id, vec![dir.join("file.txt")]), gone);
        assert_eq!(core.send_documents(&id, Vec::new()), gone);
        assert_eq!(core.send(&[], vec![dir.join("file.txt")]), gone);
        assert!(lock(&core.state).transfers.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Each recipient of a send gets each item as a transfer of its own, and
    /// nothing starts when one of them is no longer a recipient.
    #[test]
    fn a_send_to_several_recipients_is_a_transfer_for_each() {
        let dir = test_dir("several-recipients");
        let core = core_with_offer(1, &dir);
        let ids: Vec<String> = (0..2)
            .map(|_| identity::new_key().public().to_string())
            .collect();
        {
            let mut state = lock(&core.state);
            state.cfg.add_contact(Contact::new("Ana", &ids[0]));
            state.cfg.add_contact(Contact::new("Bob", &ids[1]));
        }
        let paths = vec![dir.join("a.txt"), dir.join("b.txt")];

        let mut with_a_stranger = ids.clone();
        with_a_stranger.push("ab".repeat(32));
        assert_eq!(
            core.send(&with_a_stranger, paths.clone()),
            Some(ui().recipient_gone.to_string())
        );
        assert!(lock(&core.state).transfers.is_empty());

        assert_eq!(core.send(&ids, paths.clone()), None);
        assert_eq!(lock(&core.state).transfers.len(), 4);
        // Named twice, a recipient gets each item once.
        let twice = [ids[0].clone(), ids[0].to_uppercase()];
        assert_eq!(core.send(&twice, paths), None);
        assert_eq!(lock(&core.state).transfers.len(), 6);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `Core` without a network, with one offer waiting.
    pub(in crate::core) fn core_with_offer(id: u64, dir: &Path) -> Core {
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
                profile: None,
                net: NetHandle::idle(),
                online: None,
                net_problem: None,
                transfers: BTreeMap::new(),
                offers: HashMap::from([(id, pending)]),
                receiving: HashMap::new(),
                shown: HashMap::new(),
                busy: false,
                documents: HashMap::new(),
                #[cfg(target_os = "macos")]
                update_job: None,
                #[cfg(target_os = "macos")]
                update: None,
                #[cfg(target_os = "macos")]
                next_update_look: Instant::now(),
                #[cfg(target_os = "macos")]
                chats: chat::Chats::new(dir, true),
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

    /// The status says what the network last said of itself: a network that
    /// has no endpoint any more is not online as it was before, and its
    /// problem is not shown once an endpoint is bound.
    #[test]
    fn the_status_says_what_the_network_last_said() {
        let dir = test_dir("status");
        let core = core_with_offer(1, &dir);
        // "Online" would look for an update.
        lock(&core.state).cfg.check_updates = false;
        let said = |event: Event| {
            core.on_event(event);
            let shown = lock(&core.outbox.queue).pop_back();
            let Some(Out::Status { online, text }) = shown else {
                panic!("no status was shown: {shown:?}");
            };
            (online, text, lock(&core.state).online)
        };
        let online = (true, ui().status_online.to_string(), Some(true));
        let offline = (false, ui().status_offline.to_string(), Some(false));
        let connecting = (false, ui().status_connecting.to_string(), None);

        assert_eq!(said(Event::Online(true)), online);
        let (shown_online, text, kept) = said(Event::BindFailed("no socket".into()));
        assert!(!shown_online && text.contains("no socket"), "{text}");
        assert_eq!(kept, None);
        assert_eq!(said(Event::Connecting), connecting);
        assert_eq!(said(Event::Online(false)), offline);
        assert_eq!(said(Event::Online(true)), online);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn accepting_into_a_folder_that_cannot_be_made_says_which_and_why() {
        let dir = test_dir("accept-no-folder");
        let in_the_way = dir.join("file");
        std::fs::write(&in_the_way, b"").unwrap();
        let chosen = in_the_way.join("received");

        let answer = core_with_offer(1, &dir).check_accept(&[1], &chosen, false);

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

        // An offer has its sound, before what tells of it.
        #[cfg(target_os = "macos")]
        {
            let sound = lock(&core.outbox.queue).pop_front();
            assert!(
                matches!(&sound, Some(Out::Sound(sound)) if sound.event == "incoming"),
                "{sound:?}"
            );
        }
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

    /// The sounds that the user interface is told to play, in the order
    /// they were asked for.
    #[cfg(target_os = "macos")]
    fn sounds(core: &Core) -> Vec<SoundOut> {
        lock(&core.outbox.queue)
            .drain(..)
            .filter_map(|out| match out {
                Out::Sound(sound) => Some(sound),
                _ => None,
            })
            .collect()
    }

    /// Another offer of the same sender as the one that waits, of a file
    /// `name` of `size` bytes.
    fn another_offer(core: &Core, id: u64, name: &str, size: u64) {
        let mut state = lock(&core.state);
        let first = &state.offers[&1];
        let pending = PendingOffer {
            from_id: first.from_id.clone(),
            from_name: "Ana".into(),
            offer: Offer {
                name: name.into(),
                size,
                token: [id as u8; 16],
                ..first.offer.clone()
            },
        };
        state.offers.insert(id, pending);
    }

    /// The offers of one sender that wait for one answer are told of
    /// together: how many there are, what they come to and each of them,
    /// under a name that cannot fake a line of the list.
    #[cfg(target_os = "macos")]
    #[test]
    fn several_offers_are_told_of_together() {
        let dir = test_dir("offers");
        let core = core_with_offer(1, &dir);
        another_offer(&core, 2, "photo.jpg\r\n.exe", 2048);
        let state = lock(&core.state);
        let both = offers_out(&state, &[1, 2], &dir).expect("two offers");
        assert!(both.headline.contains('2'), "{}", both.headline);
        assert!(both.description.contains("2.0 KB"), "{}", both.description);
        assert_eq!(both.items.len(), 2);
        assert!(both.items[0].contains("file.txt"), "{:?}", both.items);
        assert!(!both.items[1].contains(['\r', '\n']), "{:?}", both.items);
        assert!(both.space.fits);
        // One offer is told of as it is when its window opens, without a
        // list, and offers that wait no more are not told of at all.
        let one = offers_out(&state, &[1, 7], &dir).expect("one offer");
        assert_eq!(one.headline, ui().wants_file.replace("{name}", ""));
        assert!(one.items.is_empty());
        assert!(offers_out(&state, &[7], &dir).is_none());
        drop(state);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Offers that are accepted together start together, with one sound, and
    /// what they still have to write counts for the next one until they end.
    #[test]
    fn offers_accepted_together_take_their_room() {
        let dir = test_dir("offers-accepted");
        let core = core_with_offer(1, &dir);
        another_offer(&core, 2, "b.bin", 4096);
        assert_eq!(core.check_accept(&[1, 2], &dir, false), serde_json::json!({}));
        core.accept(&[1, 2, 7], dir.clone());
        #[cfg(target_os = "macos")]
        assert_eq!(sounds(&core).len(), 1);
        let needed = |core: &Core| -> Vec<(u64, u64)> {
            let mut arriving: Vec<_> = lock(&core.state)
                .arriving()
                .iter()
                .map(|a| (a.offer.size, a.done))
                .collect();
            arriving.sort();
            arriving
        };
        assert_eq!(needed(&core), [(1, 0), (4096, 0)]);
        assert!(lock(&core.state).offers.is_empty());
        core.on_event(Event::Progress {
            id: 2,
            done: 1024,
            total: 4096,
        });
        assert_eq!(needed(&core), [(1, 0), (4096, 1024)]);
        core.on_event(Event::Finished {
            id: 2,
            outcome: Outcome::Completed(None),
        });
        assert_eq!(needed(&core), [(1, 0)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A transfer has a sound when it starts, loses its connection, goes on
    /// and ends, each once, and as the settings say.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_transfer_has_its_sounds() {
        use dodavo_core::net::Phase;
        let dir = test_dir("sounds");
        let core = core_with_offer(1, &dir);
        let keys = |core: &Core| -> Vec<&'static str> {
            sounds(core).into_iter().map(|sound| sound.event).collect()
        };
        let lost = Phase::WaitingForConnection { lost: true };

        // The user's answer starts it.
        core.accept(&[1], dir.clone());
        assert_eq!(keys(&core), ["started"]);
        for phase in [Phase::Connecting, Phase::Transferring] {
            core.on_event(Event::Phase { id: 1, phase });
        }
        assert!(keys(&core).is_empty());
        core.on_event(Event::Phase { id: 1, phase: lost });
        core.on_event(Event::Phase { id: 1, phase: lost });
        assert_eq!(keys(&core), ["connection_lost"]);
        core.on_event(Event::Phase {
            id: 1,
            phase: Phase::Transferring,
        });
        assert_eq!(keys(&core), ["continues"]);
        let end = || Event::Finished {
            id: 1,
            outcome: Outcome::Completed(None),
        };
        core.on_event(end());
        core.on_event(end());
        assert_eq!(keys(&core), ["completed"]);

        // A file of the user's goes in the place of the built-in sound, an
        // event that is off plays nothing, and nothing does while sounds
        // are off, except what the settings try.
        let file = dir.join("done.aiff");
        let answer = |call: serde_json::Value| -> serde_json::Value {
            serde_json::from_str(&core.call(&call.to_string())).unwrap()
        };
        answer(serde_json::json!({ "call": "set_sound", "event": "failed", "kind": "file", "file": file }));
        answer(serde_json::json!({ "call": "set_sound", "event": "cancelled", "kind": "off" }));
        let listed = answer(serde_json::json!({ "call": "sounds" }));
        assert_eq!(listed["on"], true);
        assert_eq!(listed["events"].as_array().unwrap().len(), SoundEvent::ALL.len());
        let failed = &listed["events"][7];
        assert_eq!(
            (&failed["event"], &failed["kind"], &failed["shown"]),
            (&"failed".into(), &"file".into(), &"done.aiff".into())
        );
        assert_eq!(listed["events"][8]["kind"], "off");
        {
            let state = lock(&core.state);
            assert_eq!(
                sound_out(&state.cfg, SoundEvent::Failed),
                Some(SoundOut {
                    event: "failed",
                    file: Some(file.clone())
                })
            );
            assert_eq!(sound_out(&state.cfg, SoundEvent::Cancelled), None);
        }
        answer(serde_json::json!({ "call": "set_sounds_on", "on": false }));
        assert_eq!(sound_out(&lock(&core.state).cfg, SoundEvent::Failed), None);
        answer(serde_json::json!({ "call": "try_sound", "event": "failed" }));
        answer(serde_json::json!({ "call": "try_sound", "event": "cancelled" }));
        let tried = sounds(&core);
        assert_eq!(tried.len(), 1);
        assert_eq!(tried[0].file, Some(file));
        answer(serde_json::json!({ "call": "reset_sounds" }));
        answer(serde_json::json!({ "call": "try_sound", "event": "cancelled" }));
        assert_eq!(keys(&core), ["cancelled"]);
        assert!(!lock(&core.state).cfg.sounds.on);
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
