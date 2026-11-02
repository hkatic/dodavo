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
    #[default]
    Ask,
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
    /// Name shown to the people we send files to.
    pub my_name: String,
    pub contacts: Vec<Contact>,
    /// When set, received files are saved here without asking for a location.
    pub always_save: bool,
    pub always_save_dir: Option<PathBuf>,
    /// Start transfers from known recipients without confirmation, saving to
    /// `always_save_dir` when `always_save` is set and to Downloads otherwise.
    /// Offers from anyone else are always shown first.
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
    /// When the last check succeeded (Unix seconds).
    pub update_last_check: Option<u64>,
    /// "Remind Me Later": no update window before this time (Unix seconds).
    pub update_remind_after: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            my_name: std::env::var("USERNAME")
                .unwrap_or_else(|_| crate::i18n::s().unknown_user.into()),
            contacts: Vec::new(),
            always_save: false,
            always_save_dir: None,
            auto_accept: true,
            last_save_dir: None,
            start_with_windows: false,
            minimize_to_tray: true,
            start_minimized: false,
            close_running_transfer: CloseAction::Ask,
            connection_lost: ConnectionLostAction::Ask,
            ask_after_receive: true,
            show_after_send: true,
            received_folder: ReceivedFolder::Extract,
            tray_hint_shown: false,
            language: None,
            check_updates: true,
            update_last_check: None,
            update_remind_after: None,
        }
    }
}

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
        match serde_json::from_slice(&bytes) {
            Ok(cfg) => Ok(cfg),
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

    /// The folder suggested for saving a received transfer.
    pub fn suggested_save_dir(&self) -> PathBuf {
        if self.always_save
            && let Some(dir) = &self.always_save_dir
        {
            return dir.clone();
        }
        self.last_save_dir
            .clone()
            .filter(|d| d.is_dir())
            .unwrap_or_else(crate::platform::downloads_dir)
    }

    /// The folder used for transfers that start without asking.
    pub fn auto_save_dir(&self) -> PathBuf {
        match &self.always_save_dir {
            Some(dir) if self.always_save => dir.clone(),
            _ => crate::platform::downloads_dir(),
        }
    }
}

/// `%APPDATA%\Dodavo`, or a separate sub folder per `--profile`.
pub fn data_dir(profile: Option<&str>) -> PathBuf {
    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let base = appdata.join("Dodavo");
    // Keep the identity and settings of versions released under the old name.
    let legacy = appdata.join("QuickAndEasyFileTransfer");
    if !base.exists() && legacy.is_dir() {
        let _ = std::fs::rename(&legacy, &base);
    }
    match profile {
        Some(p) => base.join("profiles").join(p),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("dodavo-cfg-test-{}", std::process::id()));
        let mut cfg = Config::default();
        cfg.contacts.push(Contact {
            name: "Ana".into(),
            id: "abc".into(),
        });
        cfg.always_save = true;
        cfg.always_save_dir = Some(PathBuf::from(r"C:\Temp"));
        cfg.language = Some("hr".into());
        cfg.close_running_transfer = CloseAction::Background;
        cfg.connection_lost = ConnectionLostAction::Wait;
        cfg.show_after_send = false;
        cfg.received_folder = ReceivedFolder::Both;
        cfg.auto_accept = false;
        cfg.check_updates = false;
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
        assert_eq!(cfg.received_folder, ReceivedFolder::Extract);
        assert_eq!(cfg.close_running_transfer, CloseAction::Ask);
        assert_eq!(cfg.connection_lost, ConnectionLostAction::Ask);
        assert_eq!(cfg.language, None);
        assert!(cfg.check_updates);
        assert_eq!(cfg.update_last_check, None);
        assert_eq!(cfg.update_remind_after, None);
    }
}
