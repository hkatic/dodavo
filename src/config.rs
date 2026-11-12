//! Persistent settings and the recipient list, stored as JSON in the user's
//! roaming application data folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    /// The recipient's Contact ID (iroh endpoint id / public key).
    pub id: String,
    /// How large an item this recipient may send (see `Config::limit_for`).
    /// A blocked sender's entry has none.
    #[serde(default, skip_serializing_if = "ContactLimit::is_general")]
    pub limit: ContactLimit,
}

impl Contact {
    /// A contact under the general limit.
    pub fn new(name: impl Into<String>, id: impl Into<String>) -> Contact {
        Contact {
            name: name.into(),
            id: id.into(),
            limit: ContactLimit::General,
        }
    }
}

/// How large an item a recipient may send.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContactLimit {
    /// As the settings say for everyone.
    #[default]
    General,
    /// Any size, whatever the settings say.
    Unlimited,
    /// At most this many bytes, whatever the settings say.
    Bytes(u64),
}

impl ContactLimit {
    fn is_general(&self) -> bool {
        *self == ContactLimit::General
    }
}

/// What happens to an offered item that is larger than its sender's limit.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverLimit {
    /// It never starts by itself: it waits for the user's answer, which may
    /// be to accept it.
    #[default]
    Ask,
    /// It is declined, and nothing here shows it.
    Decline,
    /// It is declined, and a notification says so.
    DeclineAndTell,
}

impl OverLimit {
    /// In the order shown in the settings.
    pub const ALL: [OverLimit; 3] = [
        OverLimit::Ask,
        OverLimit::Decline,
        OverLimit::DeclineAndTell,
    ];
}

/// What happens when the window of a running transfer is closed.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseAction {
    #[default]
    Ask,
    Background,
    Cancel,
}

impl CloseAction {
    /// In the order shown in the settings.
    pub const ALL: [CloseAction; 3] = [
        CloseAction::Ask,
        CloseAction::Background,
        CloseAction::Cancel,
    ];
}

/// What happens when the connection is lost during a transfer. The transfer
/// itself always waits for the other side; this only decides whether the user
/// is asked, and whether the transfer is cancelled instead.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionLostAction {
    Ask,
    #[default]
    Wait,
    Cancel,
}

impl ConnectionLostAction {
    /// In the order shown in the settings.
    pub const ALL: [ConnectionLostAction; 3] = [
        ConnectionLostAction::Ask,
        ConnectionLostAction::Wait,
        ConnectionLostAction::Cancel,
    ];
}

/// What is kept of a received folder. It arrives as a zip archive; the
/// choice is made when the transfer is accepted and stays with the transfer.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceivedFolder {
    /// Unpack the folder and delete the archive.
    #[default]
    Extract,
    /// Keep the archive as `<name>.zip` and do not unpack it.
    KeepZip,
    /// Unpack the folder and keep the archive next to it.
    Both,
}

impl ReceivedFolder {
    /// In the order shown in the settings.
    pub const ALL: [ReceivedFolder; 3] = [
        ReceivedFolder::Extract,
        ReceivedFolder::KeepZip,
        ReceivedFolder::Both,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// What the settings in the file mean (see `upgrade`). A file of 1.0.0
    /// does not have it, which reads as 0 rather than as the current version.
    #[serde(default)]
    pub settings_version: u32,
    /// Name shown to the people we send files to.
    pub my_name: String,
    pub contacts: Vec<Contact>,
    /// Senders whose offers are declined without asking (see
    /// `net::NetHandle::set_blocked`). They are not recipients at the same
    /// time: blocking one removes the recipient, and adding a recipient
    /// unblocks them.
    pub blocked: Vec<Contact>,
    /// Whether `size_limit` applies to everyone who has no limit of their own.
    pub limit_size: bool,
    /// The largest item, in bytes, that someone may send (see `limit_for`);
    /// kept while `limit_size` is off.
    pub size_limit: u64,
    /// What happens to an item over its sender's limit.
    pub over_limit: OverLimit,
    /// When set, received files go to `always_save_folder`. Otherwise the
    /// user is asked where to save each one.
    pub always_save: bool,
    /// The folder of `always_save`; `None` is Downloads, wherever that is.
    pub always_save_dir: Option<PathBuf>,
    /// Start transfers from known recipients without confirmation, as long as
    /// `always_save` says where they go. Offers from anyone else are always
    /// shown first.
    pub auto_accept: bool,
    /// The last folder the user picked for saving received files.
    pub last_save_dir: Option<PathBuf>,
    pub start_with_windows: bool,
    pub minimize_to_tray: bool,
    /// Always start hidden in the notification area (like `--minimized`).
    pub start_minimized: bool,
    /// What closing the window of a running transfer does.
    pub close_running_transfer: CloseAction,
    /// What happens when the connection is lost during a transfer.
    pub connection_lost: ConnectionLostAction,
    /// Ask whether to open a received item when its transfer is complete.
    pub ask_after_receive: bool,
    /// Show a message when the recipient has received a sent item.
    pub show_after_send: bool,
    /// Windows: close a transfer's progress window as soon as the transfer
    /// ends, however it ended, with a notification in place of the question
    /// after a receive, the message after a send and the window that a
    /// failed transfer otherwise leaves open.
    pub close_when_ended: bool,
    /// What is kept of a received folder.
    pub received_folder: ReceivedFolder,
    /// Whether the "still running in the notification area" hint was shown.
    pub tray_hint_shown: bool,
    /// User interface language code (see `i18n::Lang::code`); `None` follows Windows.
    pub language: Option<String>,
    /// Look for a newer version once a day (see `update`).
    pub check_updates: bool,
    /// "Send with Dodavo" in File Explorer's context menu, and Dodavo in its
    /// Send to menu (Windows; see `ui::shell::set_explorer_menu`).
    pub explorer_menu: bool,
    /// When the last check succeeded (Unix seconds).
    pub update_last_check: Option<u64>,
    /// "Remind Me Later": no update window before this time (Unix seconds).
    pub update_remind_after: Option<u64>,
    /// The folder that the user chose for temporary files, which Dodavo
    /// keeps in a folder of its own in it (see `temp_dir_for`); `None` is
    /// the system's place for them. Windows and the Mac.
    pub temp_dir: Option<PathBuf>,
    /// Windows and the Mac: tell the user before a transfer whose temporary
    /// archive is too large for the file system of the folder for temporary
    /// files (4 GiB or more on FAT32), so that another folder can be chosen
    /// (`space::send_temp_warning`, `Space::temp_warning`).
    pub warn_fat_temp: bool,
    /// What `temp_dir` was before: a start still clears leftovers there,
    /// until none are left (see `set_temp_dir`, `forget_temp_dirs`).
    pub temp_dirs_before: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            settings_version: SETTINGS_VERSION,
            my_name: crate::platform::user_name()
                .unwrap_or_else(|| crate::i18n::s().unknown_user.into()),
            contacts: Vec::new(),
            blocked: Vec::new(),
            limit_size: false,
            size_limit: 1 << 30,
            over_limit: OverLimit::Ask,
            always_save: true,
            always_save_dir: None,
            auto_accept: true,
            last_save_dir: None,
            start_with_windows: false,
            minimize_to_tray: true,
            start_minimized: false,
            close_running_transfer: CloseAction::Ask,
            connection_lost: ConnectionLostAction::Wait,
            ask_after_receive: true,
            show_after_send: true,
            close_when_ended: false,
            received_folder: ReceivedFolder::Extract,
            tray_hint_shown: false,
            language: None,
            check_updates: true,
            explorer_menu: true,
            update_last_check: None,
            update_remind_after: None,
            temp_dir: None,
            warn_fat_temp: true,
            temp_dirs_before: Vec::new(),
        }
    }
}

/// Dodavo's own folder for temporary files in the folder `base` that the
/// user chose, with a sub folder per `--profile` as in the system's place
/// (see `archive::temp_dir_for`): never `base` itself, where other things
/// are, since a start deletes old archives from its temporary folder.
fn temp_dir_in(base: &Path, profile: Option<&str>) -> PathBuf {
    let own = base.join("Dodavo");
    match profile {
        Some(profile) => own.join("profiles").join(profile),
        None => own,
    }
}

/// The version of `Config::settings_version` that this program writes.
const SETTINGS_VERSION: u32 = 1;

/// The settings file exists but could not be used.
#[derive(Debug)]
pub struct LoadError {
    pub path: PathBuf,
    pub error: String,
    /// Where the unusable file was moved so that it is not overwritten by the
    /// next save (`None` if moving it failed).
    pub backup: Option<PathBuf>,
}

impl Config {
    /// Reads the settings. A missing file means the defaults; a file that
    /// cannot be read or parsed is an error, so the caller can tell the user
    /// instead of silently losing the recipient list on the next save.
    pub fn load(dir: &Path) -> Result<Config, LoadError> {
        let path = dir.join("config.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => {
                return Err(LoadError {
                    path,
                    error: e.to_string(),
                    backup: None,
                });
            }
        };
        match serde_json::from_slice::<Config>(&bytes) {
            Ok(mut cfg) => {
                cfg.upgrade();
                Ok(cfg)
            }
            Err(e) => {
                // Under a free name, never over an earlier one.
                let backup =
                    crate::archive::move_to_unique(&path, dir, "config.json.bad", false).ok();
                Err(LoadError {
                    path,
                    error: e.to_string(),
                    backup,
                })
            }
        }
    }

    /// Gives settings of an earlier version what they mean now. Every field
    /// is saved, so a file cannot tell a choice from what was the default then.
    fn upgrade(&mut self) {
        if self.settings_version < 1 {
            // "Ask every time" was the default of 1.0.0, and nearly everyone's
            // value. Whoever wants it can choose it again, and then it stays.
            if self.connection_lost == ConnectionLostAction::Ask {
                self.connection_lost = ConnectionLostAction::Wait;
            }
            // In 1.0.0, without the box a known sender's transfer went to
            // Downloads by itself, whatever folder was stored; without the
            // box now, Dodavo asks. So those transfers go on as they did. With
            // automatic transfers off, they were asked anyway.
            if !self.always_save && self.auto_accept {
                self.always_save = true;
                self.always_save_dir = None;
            }
        }
        self.settings_version = self.settings_version.max(SETTINGS_VERSION);
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        // Write to a temporary file first so a crash never leaves a truncated config behind.
        let tmp = dir.join("config.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, dir.join("config.json"))
    }

    pub fn contact_by_id(&self, id: &str) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.id.eq_ignore_ascii_case(id))
    }

    /// Adds `contact` to the recipients, in the order of their names. A
    /// recipient is no blocked sender, so they are unblocked.
    pub fn add_contact(&mut self, contact: Contact) {
        self.unblock(&contact.id);
        self.contacts.push(contact);
        self.sort_contacts();
    }

    /// Puts `contact` in the place of `old`, as `add_contact` would add it.
    pub fn replace_contact(&mut self, old: &Contact, contact: Contact) {
        self.unblock(&contact.id);
        if let Some(slot) = self.contacts.iter_mut().find(|c| *c == old) {
            *slot = contact;
        }
        self.sort_contacts();
    }

    fn sort_contacts(&mut self) {
        self.contacts.sort_by_key(|c| c.name.to_lowercase());
    }

    pub fn is_blocked(&self, id: &str) -> bool {
        self.blocked.iter().any(|c| c.id.eq_ignore_ascii_case(id))
    }

    /// Blocks the sender `id`, under `name`, and removes them from the
    /// recipients; a sender who is blocked already keeps their entry.
    pub fn block(&mut self, name: &str, id: &str) {
        self.contacts.retain(|c| !c.id.eq_ignore_ascii_case(id));
        if !self.is_blocked(id) {
            self.blocked.push(Contact::new(name, id));
        }
    }

    /// Takes `id` off the blocked list; `false` if it was not on it.
    pub fn unblock(&mut self, id: &str) -> bool {
        let before = self.blocked.len();
        self.blocked.retain(|c| !c.id.eq_ignore_ascii_case(id));
        self.blocked.len() != before
    }

    /// The Contact IDs of the blocked senders, for the network
    /// (`net::start`, `net::NetHandle::set_blocked`). They compare without
    /// case, as everywhere, and an entry that is not a Contact ID cannot send
    /// anything, so it is left out.
    pub fn blocked_ids(&self) -> Vec<iroh::EndpointId> {
        self.blocked
            .iter()
            .filter_map(|c| c.id.to_ascii_lowercase().parse().ok())
            .collect()
    }

    /// The largest item, in bytes, that the sender `id` may send; `None` for
    /// any size. A recipient's own limit comes first, then the general one,
    /// which also holds for senders who are not recipients.
    pub fn limit_for(&self, id: &str) -> Option<u64> {
        match self.contact_by_id(id).map(|c| c.limit) {
            Some(ContactLimit::Bytes(bytes)) => Some(bytes),
            Some(ContactLimit::Unlimited) => None,
            Some(ContactLimit::General) | None => self.limit_size.then_some(self.size_limit),
        }
    }

    /// The limits as the network needs them (`net::start`,
    /// `net::NetHandle::set_limits`). A recipient whose entry is not a
    /// Contact ID cannot send anything, so it is left out.
    pub fn limits(&self) -> crate::net::Limits {
        crate::net::Limits {
            action: self.over_limit,
            general: self.limit_size.then_some(self.size_limit),
            per_peer: self
                .contacts
                .iter()
                .filter(|c| c.limit != ContactLimit::General)
                .filter_map(|c| {
                    let id = c.id.to_ascii_lowercase().parse().ok()?;
                    Some((id, self.limit_for(&c.id)))
                })
                .collect(),
        }
    }

    /// Where the temporary archives of `profile`'s transfers go by the
    /// user's choice; `None` without one, which is the system's place
    /// (`archive::temp_dir_for`). Android has no such choice: its folders
    /// are the app's.
    pub fn temp_dir_for(&self, profile: Option<&str>) -> Option<PathBuf> {
        if cfg!(target_os = "android") {
            return None;
        }
        self.temp_dir
            .as_deref()
            .map(|base| temp_dir_in(base, profile))
    }

    /// Where the temporary archives of `profile`'s new transfers go: the
    /// user's choice, or the system's place. (A transfer falls back on the
    /// system's place while the chosen folder cannot be made, see
    /// `net::Shared::temp_dir_now`.)
    pub fn temp_dir_in_use(&self, profile: Option<&str>) -> PathBuf {
        self.temp_dir_for(profile)
            .unwrap_or_else(|| crate::archive::temp_dir_for(profile))
    }

    /// The same for the folders chosen before (see `temp_dirs_before`).
    pub fn temp_dirs_before_for(&self, profile: Option<&str>) -> Vec<PathBuf> {
        self.temp_dirs_before
            .iter()
            .map(|base| temp_dir_in(base, profile))
            .collect()
    }

    /// Chooses the folder for temporary files (`None`: the system's place),
    /// and remembers the one before, where a transfer under way may still
    /// have its archive and a start has to look for leftovers.
    pub fn set_temp_dir(&mut self, dir: Option<PathBuf>) {
        if dir == self.temp_dir {
            return;
        }
        if let Some(before) = self.temp_dir.take()
            && !self.temp_dirs_before.contains(&before)
        {
            self.temp_dirs_before.push(before);
        }
        if let Some(dir) = &dir {
            self.temp_dirs_before.retain(|before| before != dir);
        }
        self.temp_dir = dir;
    }

    /// Forgets the folders chosen before in which `profile` has nothing
    /// left (`has_files` says whether its folder there has); whether any
    /// was forgotten.
    pub fn forget_temp_dirs(
        &mut self,
        profile: Option<&str>,
        has_files: impl Fn(&Path) -> bool,
    ) -> bool {
        let before = self.temp_dirs_before.len();
        self.temp_dirs_before
            .retain(|base| has_files(&temp_dir_in(base, profile)));
        self.temp_dirs_before.len() != before
    }

    /// The folder of "Always save received files to this folder".
    pub fn always_save_folder(&self) -> PathBuf {
        self.always_save_dir
            .clone()
            .unwrap_or_else(crate::platform::downloads_dir)
    }

    /// The folder suggested for saving a received transfer.
    pub fn suggested_save_dir(&self) -> PathBuf {
        if self.always_save {
            return self.always_save_folder();
        }
        self.last_save_dir
            .clone()
            .filter(|d| d.is_dir())
            .unwrap_or_else(crate::platform::downloads_dir)
    }

    /// Where a transfer offered by a sender who is (`known`) or is not a
    /// recipient starts by itself; `None` if the user has to answer the offer.
    pub fn auto_save_dir(&self, known: bool) -> Option<PathBuf> {
        (known && self.auto_accept && self.always_save).then(|| self.always_save_folder())
    }

    /// Where `offer` from the sender `from` starts by itself; `None` if the
    /// user has to answer it, which is so for every item over the sender's
    /// limit, whatever `over_limit` says (where that is to decline, the
    /// network has declined it already).
    pub fn auto_save_dir_for(&self, from: &str, offer: &crate::net::Offer) -> Option<PathBuf> {
        let known = self.contact_by_id(from).is_some();
        self.auto_save_dir(known)
            .filter(|_| !crate::limit::over(offer, self.limit_for(from)))
    }
}

/// The folder of the settings and the identity (`%APPDATA%\Dodavo`, see
/// `platform::data_base`), or a separate sub folder per `--profile`.
pub fn data_dir(profile: Option<&str>) -> PathBuf {
    let base = crate::platform::data_base();
    match profile {
        Some(p) => base.join("profiles").join(p),
        None => base,
    }
}

/// Only the language setting, for a process that is about to end and has a
/// message to show. It leaves the file alone: what `Config::load` does with
/// a damaged one is for the instance that stays.
pub fn read_language(dir: &Path) -> Option<String> {
    let bytes = std::fs::read(dir.join("config.json")).ok()?;
    let settings: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    settings.get("language")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_the_language_leaves_a_damaged_file_alone() {
        let dir = std::env::temp_dir().join(format!("dodavo-cfg-lang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        assert_eq!(read_language(&dir), None);
        std::fs::write(&path, br#"{"language":"hr"}"#).unwrap();
        assert_eq!(read_language(&dir), Some("hr".to_string()));
        std::fs::write(&path, br#"{"language":null}"#).unwrap();
        assert_eq!(read_language(&dir), None);
        std::fs::write(&path, b"{\"language\": [").unwrap();
        assert_eq!(read_language(&dir), None);
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The folder for temporary files is the system's until the user
    /// chooses one, in which Dodavo keeps a folder of its own, per profile.
    /// What was chosen before is remembered until nothing is left there.
    #[test]
    #[cfg(not(target_os = "android"))]
    fn the_folder_for_temporary_files() {
        let earlier: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(earlier.temp_dir, None);
        assert!(earlier.temp_dirs_before.is_empty());

        let (a, b) = (PathBuf::from("a"), PathBuf::from("b"));
        let mut cfg = Config::default();
        assert_eq!(cfg.temp_dir_for(None), None);
        assert_eq!(cfg.temp_dir_in_use(None), crate::archive::temp_dir_for(None));
        cfg.set_temp_dir(Some(a.clone()));
        assert!(cfg.temp_dirs_before.is_empty());
        assert_eq!(cfg.temp_dir_for(None), Some(a.join("Dodavo")));
        assert_eq!(cfg.temp_dir_in_use(None), a.join("Dodavo"));
        assert_eq!(
            cfg.temp_dir_for(Some("ana")),
            Some(a.join("Dodavo").join("profiles").join("ana"))
        );
        // The same choice again changes nothing.
        cfg.set_temp_dir(Some(a.clone()));
        assert!(cfg.temp_dirs_before.is_empty());
        cfg.set_temp_dir(Some(b.clone()));
        assert_eq!(cfg.temp_dirs_before, std::slice::from_ref(&a));
        cfg.set_temp_dir(Some(a.clone()));
        assert_eq!(cfg.temp_dirs_before, std::slice::from_ref(&b));
        cfg.set_temp_dir(None);
        assert_eq!(cfg.temp_dirs_before, [b.clone(), a.clone()]);
        assert_eq!(
            cfg.temp_dirs_before_for(None),
            [b.join("Dodavo"), a.join("Dodavo")]
        );
        let in_b = b.join("Dodavo");
        assert!(cfg.forget_temp_dirs(None, |dir| dir == in_b));
        assert_eq!(cfg.temp_dirs_before, [b]);
        assert!(!cfg.forget_temp_dirs(None, |_| true));
    }

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("dodavo-cfg-test-{}", std::process::id()));
        let mut cfg = Config::default();
        cfg.contacts.push(Contact::new("Ana", "abc"));
        cfg.contacts.push(Contact {
            limit: ContactLimit::Bytes(5 << 20),
            ..Contact::new("Bo", "abd")
        });
        cfg.contacts.push(Contact {
            limit: ContactLimit::Unlimited,
            ..Contact::new("Cy", "abe")
        });
        cfg.blocked.push(Contact::new("Spam", "def"));
        cfg.limit_size = true;
        cfg.size_limit = 3 << 30;
        cfg.over_limit = OverLimit::DeclineAndTell;
        cfg.always_save = false;
        cfg.always_save_dir = Some(PathBuf::from(r"C:\Temp"));
        cfg.language = Some("hr".into());
        cfg.close_running_transfer = CloseAction::Background;
        cfg.connection_lost = ConnectionLostAction::Ask;
        cfg.show_after_send = false;
        cfg.close_when_ended = true;
        cfg.warn_fat_temp = false;
        cfg.received_folder = ReceivedFolder::Both;
        cfg.auto_accept = false;
        cfg.check_updates = false;
        cfg.explorer_menu = false;
        cfg.update_last_check = Some(1_700_000_000);
        cfg.update_remind_after = Some(1_700_086_400);
        cfg.save(&dir).unwrap();
        assert_eq!(Config::load(&dir).unwrap(), cfg);
        assert!(cfg.contact_by_id("ABC").is_some());
        assert!(cfg.is_blocked("DEF"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_blocked_sender_is_no_recipient() {
        let id = iroh::SecretKey::generate().public().to_string();
        let mut cfg = Config::default();
        cfg.contacts.push(Contact::new("Ana", id.clone()));
        cfg.block("Ana", &id.to_uppercase());
        assert!(cfg.contact_by_id(&id).is_none());
        assert!(cfg.is_blocked(&id));
        assert_eq!(cfg.blocked_ids(), vec![id.parse().unwrap()]);
        // Blocking again keeps the one entry, under its first name.
        cfg.block("Someone", &id);
        assert_eq!(cfg.blocked.len(), 1);
        assert_eq!(cfg.blocked[0].name, "Ana");
        assert!(cfg.unblock(&id));
        assert!(!cfg.unblock(&id));
        assert!(!cfg.is_blocked(&id));
        // Whoever becomes a recipient is unblocked.
        cfg.block("Ana", &id);
        cfg.add_contact(Contact::new("Ana", id.to_uppercase()));
        assert!(!cfg.is_blocked(&id));
        let other = iroh::SecretKey::generate().public().to_string();
        cfg.block("Bo", &other);
        let ana = cfg.contacts[0].clone();
        cfg.replace_contact(&ana, Contact::new("Ana", other.clone()));
        assert!(!cfg.is_blocked(&other));
        assert_eq!(cfg.contacts[0].id, other);
        // What is not a Contact ID never reaches the network.
        cfg.block("Typo", "not an id");
        assert!(cfg.blocked_ids().is_empty());
    }

    #[test]
    fn missing_file_means_defaults() {
        let dir = std::env::temp_dir().join(format!("dodavo-cfg-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Config::load(&dir).unwrap(), Config::default());
    }

    #[test]
    fn damaged_file_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("dodavo-cfg-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), b"{\"contacts\": [").unwrap();
        let err = Config::load(&dir).unwrap_err();
        let backup = err.backup.expect("moved aside");
        assert_eq!(backup, dir.join("config.json.bad"));
        assert_eq!(std::fs::read(&backup).unwrap(), b"{\"contacts\": [");
        assert!(!dir.join("config.json").exists());
        // The next start is clean, and the backup is not overwritten.
        assert_eq!(Config::load(&dir).unwrap(), Config::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_fields_use_defaults() {
        let cfg: Config = serde_json::from_str(r#"{"my_name":"X"}"#).unwrap();
        assert_eq!(cfg.my_name, "X");
        assert!(cfg.minimize_to_tray);
        assert!(!cfg.start_minimized);
        assert!(cfg.ask_after_receive);
        assert!(cfg.show_after_send);
        assert!(!cfg.close_when_ended);
        assert!(cfg.warn_fat_temp);
        assert!(cfg.auto_accept);
        assert!(cfg.always_save);
        assert_eq!(cfg.always_save_dir, None);
        assert_eq!(cfg.received_folder, ReceivedFolder::Extract);
        assert_eq!(cfg.close_running_transfer, CloseAction::Ask);
        assert_eq!(cfg.connection_lost, ConnectionLostAction::Wait);
        assert_eq!(cfg.language, None);
        assert!(cfg.check_updates);
        assert!(cfg.explorer_menu);
        assert_eq!(cfg.update_last_check, None);
        assert_eq!(cfg.update_remind_after, None);
        assert!(cfg.blocked.is_empty());
        assert!(!cfg.limit_size);
        assert_eq!(cfg.size_limit, 1 << 30);
        assert_eq!(cfg.over_limit, OverLimit::Ask);
    }

    /// A file of a version without limits reads, and its recipients are
    /// under the general limit; an entry is written as it was unless it has
    /// a limit of its own.
    #[test]
    fn recipients_of_earlier_versions_have_the_general_limit() {
        let cfg: Config =
            serde_json::from_str(r#"{"contacts":[{"name":"Ana","id":"abc"}]}"#).unwrap();
        assert_eq!(cfg.contacts, vec![Contact::new("Ana", "abc")]);
        assert_eq!(
            serde_json::to_string(&cfg.contacts[0]).unwrap(),
            r#"{"name":"Ana","id":"abc"}"#
        );
        let own = Contact {
            limit: ContactLimit::Bytes(7),
            ..Contact::new("Ana", "abc")
        };
        let json = serde_json::to_string(&own).unwrap();
        assert_eq!(json, r#"{"name":"Ana","id":"abc","limit":{"bytes":7}}"#);
        assert_eq!(serde_json::from_str::<Contact>(&json).unwrap(), own);
    }

    #[test]
    fn a_recipients_own_limit_comes_before_the_general_one() {
        let ids: Vec<String> = (0..4)
            .map(|_| iroh::SecretKey::generate().public().to_string())
            .collect();
        let mut cfg = Config::default();
        cfg.contacts = vec![
            Contact::new("General", ids[0].clone()),
            Contact {
                limit: ContactLimit::Unlimited,
                ..Contact::new("Any", ids[1].clone())
            },
            Contact {
                limit: ContactLimit::Bytes(10),
                ..Contact::new("Own", ids[2].clone())
            },
        ];
        // Without the general limit, only a recipient's own counts.
        assert_eq!(cfg.limit_for(&ids[0]), None);
        assert_eq!(cfg.limit_for(&ids[1]), None);
        assert_eq!(cfg.limit_for(&ids[2].to_uppercase()), Some(10));
        assert_eq!(cfg.limit_for(&ids[3]), None);
        cfg.limit_size = true;
        cfg.size_limit = 500;
        assert_eq!(cfg.limit_for(&ids[0]), Some(500));
        assert_eq!(cfg.limit_for(&ids[1]), None);
        assert_eq!(cfg.limit_for(&ids[2]), Some(10));
        // Someone who is not a recipient.
        assert_eq!(cfg.limit_for(&ids[3]), Some(500));

        cfg.over_limit = OverLimit::Decline;
        let limits = cfg.limits();
        assert_eq!(limits.action, OverLimit::Decline);
        let peer = |i: usize| ids[i].parse::<iroh::EndpointId>().unwrap();
        assert_eq!(limits.limit_for(&peer(0)), Some(500));
        assert_eq!(limits.limit_for(&peer(1)), None);
        assert_eq!(limits.limit_for(&peer(2)), Some(10));
        assert_eq!(limits.limit_for(&peer(3)), Some(500));
    }

    /// An item over its sender's limit never starts by itself.
    #[test]
    fn an_item_over_the_limit_waits_for_an_answer() {
        let id = iroh::SecretKey::generate().public().to_string();
        let mut cfg = Config {
            always_save_dir: Some(std::env::temp_dir()),
            limit_size: true,
            size_limit: 100,
            ..Config::default()
        };
        cfg.contacts.push(Contact::new("Ana", id.clone()));
        let mut offer = crate::limit::tests::offer(100, 0);
        assert!(cfg.auto_save_dir_for(&id, &offer).is_some());
        offer.size = 101;
        assert_eq!(cfg.auto_save_dir_for(&id, &offer), None);
        cfg.contacts[0].limit = ContactLimit::Unlimited;
        assert!(cfg.auto_save_dir_for(&id, &offer).is_some());
        // Not a recipient: asked anyway.
        let other = iroh::SecretKey::generate().public().to_string();
        offer.size = 1;
        assert_eq!(cfg.auto_save_dir_for(&other, &offer), None);
    }

    fn load_json(name: &str, json: &str) -> Config {
        let dir =
            std::env::temp_dir().join(format!("dodavo-cfg-upgrade-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), json).unwrap();
        let cfg = Config::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        cfg
    }

    /// The fields of a settings file of 1.0.0 that decide where a transfer goes.
    fn from_1_0_0(name: &str, always: bool, dir: Option<&str>, auto: bool, lost: &str) -> Config {
        let dir = dir.map_or("null".to_string(), |d| format!("{d:?}"));
        load_json(
            name,
            &format!(
                r#"{{"my_name":"X","always_save":{always},"always_save_dir":{dir},
                    "auto_accept":{auto},"connection_lost":"{lost}"}}"#
            ),
        )
    }

    #[test]
    fn a_new_installation_saves_by_itself_and_waits() {
        let cfg = Config::default();
        assert_eq!(cfg.settings_version, SETTINGS_VERSION);
        assert!(cfg.always_save);
        assert_eq!(cfg.always_save_dir, None);
        assert_eq!(cfg.connection_lost, ConnectionLostAction::Wait);
        assert_eq!(
            cfg.auto_save_dir(true),
            Some(crate::platform::downloads_dir())
        );
    }

    /// With 1.0.0's defaults, a known sender's transfers went to Downloads by
    /// themselves, also with another folder stored behind the cleared box.
    #[test]
    fn the_defaults_of_1_0_0_keep_doing_what_they_did() {
        for (name, dir) in [("no-folder", None), ("stale-folder", Some(r"D:\Stuff"))] {
            let cfg = from_1_0_0(name, false, dir, true, "ask");
            assert_eq!(cfg.settings_version, SETTINGS_VERSION);
            assert!(cfg.always_save);
            assert_eq!(cfg.always_save_dir, None);
            assert_eq!(cfg.connection_lost, ConnectionLostAction::Wait);
            assert_eq!(
                cfg.auto_save_dir(true),
                Some(crate::platform::downloads_dir())
            );
        }
    }

    /// Someone who was asked about every transfer still is.
    #[test]
    fn asking_about_every_transfer_stays() {
        let cfg = from_1_0_0("ask-all", false, None, false, "wait");
        assert!(!cfg.always_save);
        assert!(!cfg.auto_accept);
        assert_eq!(cfg.auto_save_dir(true), None);
    }

    #[test]
    fn a_chosen_folder_and_choices_for_a_lost_connection_stay() {
        let cfg = from_1_0_0("chosen", true, Some(r"D:\Inbox"), true, "cancel");
        assert_eq!(cfg.always_save_dir, Some(PathBuf::from(r"D:\Inbox")));
        assert_eq!(cfg.connection_lost, ConnectionLostAction::Cancel);
        assert_eq!(cfg.auto_save_dir(true), Some(PathBuf::from(r"D:\Inbox")));
    }

    /// Only a file of an earlier version is upgraded: these choices, made in
    /// this version, stay as they are at the next start.
    #[test]
    fn the_upgrade_happens_once() {
        let dir = std::env::temp_dir().join(format!("dodavo-cfg-once-{}", std::process::id()));
        let mut cfg = Config {
            connection_lost: ConnectionLostAction::Ask,
            always_save: false,
            ..Config::default()
        };
        cfg.save(&dir).unwrap();
        cfg = Config::load(&dir).unwrap();
        assert_eq!(cfg.connection_lost, ConnectionLostAction::Ask);
        assert!(!cfg.always_save);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn offers_start_by_themselves_only_with_the_folder_and_from_recipients() {
        let dir = PathBuf::from(r"D:\Inbox");
        let cfg = Config {
            always_save: true,
            always_save_dir: Some(dir.clone()),
            auto_accept: true,
            ..Config::default()
        };
        assert_eq!(cfg.auto_save_dir(true), Some(dir.clone()));
        // Someone who is not a recipient always has to be answered.
        assert_eq!(cfg.auto_save_dir(false), None);
        let no_auto = Config {
            auto_accept: false,
            ..cfg.clone()
        };
        assert_eq!(no_auto.auto_save_dir(true), None);
        let no_folder = Config {
            always_save: false,
            ..cfg.clone()
        };
        assert_eq!(no_folder.auto_save_dir(true), None);
    }

    #[test]
    fn the_suggested_folder_follows_the_box() {
        let dir = std::env::temp_dir();
        let cfg = Config {
            always_save: true,
            always_save_dir: Some(PathBuf::from(r"D:\Inbox")),
            last_save_dir: Some(dir.clone()),
            ..Config::default()
        };
        assert_eq!(cfg.suggested_save_dir(), PathBuf::from(r"D:\Inbox"));
        // With the box on, the last folder never takes the place of Downloads.
        let downloads = Config {
            always_save_dir: None,
            ..cfg.clone()
        };
        assert_eq!(
            downloads.suggested_save_dir(),
            crate::platform::downloads_dir()
        );
        let asks = Config {
            always_save: false,
            ..cfg
        };
        assert_eq!(asks.suggested_save_dir(), dir);
    }
}
