//! Persistent settings and the recipient list, stored as JSON in the user's
//! roaming application data folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    /// The recipient's Contact ID (iroh endpoint id / public key).
    pub id: String,
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            settings_version: SETTINGS_VERSION,
            my_name: crate::platform::user_name()
                .unwrap_or_else(|| crate::i18n::s().unknown_user.into()),
            contacts: Vec::new(),
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
            received_folder: ReceivedFolder::Extract,
            tray_hint_shown: false,
            language: None,
            check_updates: true,
            explorer_menu: true,
            update_last_check: None,
            update_remind_after: None,
        }
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

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("dodavo-cfg-test-{}", std::process::id()));
        let mut cfg = Config::default();
        cfg.contacts.push(Contact {
            name: "Ana".into(),
            id: "abc".into(),
        });
        cfg.always_save = false;
        cfg.always_save_dir = Some(PathBuf::from(r"C:\Temp"));
        cfg.language = Some("hr".into());
        cfg.close_running_transfer = CloseAction::Background;
        cfg.connection_lost = ConnectionLostAction::Ask;
        cfg.show_after_send = false;
        cfg.received_folder = ReceivedFolder::Both;
        cfg.auto_accept = false;
        cfg.check_updates = false;
        cfg.explorer_menu = false;
        cfg.update_last_check = Some(1_700_000_000);
        cfg.update_remind_after = Some(1_700_086_400);
        cfg.save(&dir).unwrap();
        assert_eq!(Config::load(&dir).unwrap(), cfg);
        assert!(cfg.contact_by_id("ABC").is_some());
        std::fs::remove_dir_all(&dir).ok();
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
