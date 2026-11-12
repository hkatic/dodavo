//! The chat with one person as it is kept on the disk:
//! `<data_dir>/chats/<contact id>.json`, the newest messages last. The
//! messages themselves travel in `net::chat`; a user interface writes down
//! what it shows.

use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

/// How many messages of a chat are kept: the newest.
pub const KEPT: usize = 2000;

/// The longest message that can be written, in characters. Its bytes fit
/// `net::protocol::MAX_CHAT_TEXT` whatever the characters are.
pub const MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// When it was sent or arrived here (Unix seconds).
    pub time: u64,
    /// Whether the user wrote it; otherwise the other person did.
    pub mine: bool,
    pub text: String,
    /// A message of the user's that could not be sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

impl Entry {
    /// A message of now.
    pub fn now(mine: bool, text: impl Into<String>) -> Entry {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Entry {
            time,
            mine,
            text: text.into(),
            failed: false,
        }
    }
}

/// The chat with one person.
#[derive(Debug)]
pub struct History {
    path: PathBuf,
    entries: Vec<Entry>,
}

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("chats")
}

/// The file of the chat with `peer`, a Contact ID as the settings or the
/// network have it. Only its letters and digits make the name, without
/// case, so that nothing in it can lead out of the folder.
fn file(data_dir: &Path, peer: &str) -> PathBuf {
    let name: String = peer
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    dir(data_dir).join(format!("{name}.json"))
}

impl History {
    /// Reads the chat with `peer`; an empty one if there is none yet. A file
    /// that cannot be read as one is set aside (`.json.bad`) rather than
    /// written over.
    pub fn load(data_dir: &Path, peer: &str) -> History {
        let path = file(data_dir, peer);
        let entries = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(entries) => entries,
                Err(_) => {
                    let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        History { path, entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Adds a message and saves the chat. The message is in the chat also
    /// when saving failed. Returns how many of the oldest messages went to
    /// make room, and whether it was saved.
    pub fn push(&mut self, entry: Entry) -> (usize, io::Result<()>) {
        self.entries.push(entry);
        let over = self.entries.len().saturating_sub(KEPT);
        self.entries.drain(..over);
        (over, self.save())
    }

    /// Notes whether the message at `index` could not be sent, and saves.
    pub fn set_failed(&mut self, index: usize, failed: bool) -> io::Result<()> {
        match self.entries.get_mut(index) {
            Some(entry) if entry.failed != failed => {
                entry.failed = failed;
                self.save()
            }
            _ => Ok(()),
        }
    }

    /// Forgets every message, and deletes the file.
    pub fn clear(&mut self) -> io::Result<()> {
        self.entries.clear();
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec(&self.entries).map_err(io::Error::other)?;
        // Through a temporary file, so that a crash never leaves half a chat.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// Deletes every chat that is kept. Chats that are open have to be cleared
/// by who shows them (`History::clear`).
pub fn delete_all(data_dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir(data_dir)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Whether any chat is kept.
pub fn any(data_dir: &Path) -> bool {
    std::fs::read_dir(dir(data_dir))
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dodavo-chat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_chat_is_kept_and_read_again() {
        let dir = scratch("kept");
        let mut chat = History::load(&dir, "ABC123");
        assert!(chat.entries().is_empty());
        assert!(!any(&dir));
        chat.push(Entry::now(true, "Šaljem ti slike.")).1.unwrap();
        chat.push(Entry::now(false, "Hvala!")).1.unwrap();
        chat.set_failed(0, true).unwrap();
        assert!(any(&dir));
        // The same person, however the ID is written.
        let again = History::load(&dir, "abc123");
        assert_eq!(again.entries(), chat.entries());
        assert!(again.entries()[0].failed && again.entries()[0].mine);
        assert!(!again.entries()[1].failed && !again.entries()[1].mine);
        // Another person's chat is another file.
        assert!(History::load(&dir, "abc124").entries().is_empty());
        chat.clear().unwrap();
        chat.clear().unwrap();
        assert!(History::load(&dir, "abc123").entries().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_newest_messages_are_kept() {
        let dir = scratch("cap");
        let mut chat = History::load(&dir, "abc");
        for i in 0..KEPT {
            chat.entries.push(Entry::now(true, i.to_string()));
        }
        let (gone, saved) = chat.push(Entry::now(false, "last"));
        saved.unwrap();
        assert_eq!(gone, 1);
        let again = History::load(&dir, "abc");
        assert_eq!(again.entries().len(), KEPT);
        assert_eq!(again.entries()[0].text, "1");
        assert_eq!(again.entries()[KEPT - 1].text, "last");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_damaged_chat_is_set_aside() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(dir.join("chats")).unwrap();
        std::fs::write(dir.join("chats").join("abc.json"), b"{not a chat").unwrap();
        let mut chat = History::load(&dir, "abc");
        assert!(chat.entries().is_empty());
        assert!(dir.join("chats").join("abc.json.bad").exists());
        chat.push(Entry::now(true, "new")).1.unwrap();
        assert_eq!(History::load(&dir, "abc").entries().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_contact_id_cannot_name_another_folder() {
        let dir = Path::new("data");
        assert_eq!(
            file(dir, r"..\..\AbC/9"),
            dir.join("chats").join("abc9.json")
        );
    }

    #[test]
    fn all_chats_are_deleted_together() {
        let dir = scratch("all");
        delete_all(&dir).unwrap();
        History::load(&dir, "a").push(Entry::now(true, "1")).1.unwrap();
        History::load(&dir, "b").push(Entry::now(true, "2")).1.unwrap();
        delete_all(&dir).unwrap();
        assert!(!any(&dir));
        assert!(History::load(&dir, "a").entries().is_empty());
    }

    #[test]
    fn the_longest_message_fits_a_frame() {
        assert!(MAX_CHARS * 4 <= crate::net::protocol::MAX_CHAT_TEXT);
    }
}
