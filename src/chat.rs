//! The chat with one person as it is kept on the disk:
//! `<data_dir>/chats/<contact id>.json`, the newest messages last. The
//! messages themselves travel in `net::chat`; a user interface writes down
//! what it shows.

use std::{
    collections::HashMap,
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

    /// The message of `time` with `text` that the user wrote and that is not
    /// marked as failed, from the end: of two that are the same, each
    /// failure marks one.
    pub fn find_sent(&self, time: u64, text: &str) -> Option<usize> {
        self.entries
            .iter()
            .rposition(|e| e.mine && !e.failed && e.time == time && e.text == text)
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

    /// Forgets the messages at `indices` (others than those it has are
    /// passed over, and one named twice goes once) and saves the chat, or
    /// deletes its file when nothing is left. The messages are gone from the
    /// chat also when saving failed.
    pub fn remove(&mut self, indices: &[usize]) -> io::Result<()> {
        let before = self.entries.len();
        let mut index = 0;
        self.entries.retain(|_| {
            let keep = !indices.contains(&index);
            index += 1;
            keep
        });
        if self.entries.len() == before {
            Ok(())
        } else if self.entries.is_empty() {
            self.clear()
        } else {
            self.save()
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
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name() != SENDING_FILE)
        })
        .unwrap_or(false)
}

/// The file beside the chats that notes the messages on their way (see
/// [`Sending`]). No Contact ID makes this name (see [`file`]).
const SENDING_FILE: &str = "sending.json";

/// A message of the user's that the other side has not confirmed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnItsWay {
    /// The other person's Contact ID, as the chat is known by.
    pub peer: String,
    /// As in its [`Entry`], by which it is found in the chat (see
    /// [`History::find_sent`]).
    pub time: u64,
    pub text: String,
}

/// The messages of the user's that are on their way, by the numbers that
/// the network knows them by (`net::Command::Chat`). Not in the windows: a
/// window may be closed before its message has arrived or failed.
///
/// They are noted on the disk too (`chats/sending.json`, which is there
/// only while a message is on its way), because a chat shows a message as
/// sent from the moment it is written. A program that ends before the
/// other side has confirmed one never learns whether it arrived: the next
/// start then marks it as not sent in its chat (see [`Sending::start`]),
/// and the user sends it again if need be.
#[derive(Debug)]
pub struct Sending {
    path: PathBuf,
    messages: HashMap<u64, OnItsWay>,
}

impl Sending {
    /// For a start of the program, before any chat is shown: marks what the
    /// last run left on its way as not sent, each message in its chat, and
    /// begins with nothing on its way.
    pub fn start(data_dir: &Path) -> Sending {
        let path = dir(data_dir).join(SENDING_FILE);
        let left: Vec<OnItsWay> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        for message in &left {
            let mut chat = History::load(data_dir, &message.peer);
            if let Some(index) = chat.find_sent(message.time, &message.text) {
                let _ = chat.set_failed(index, true);
            }
        }
        // Also one that could not be read: it says nothing to anyone.
        let _ = std::fs::remove_file(&path);
        Sending {
            path,
            messages: HashMap::new(),
        }
    }

    /// For a program that will not get to send anything (another copy of
    /// it runs, whose chats these are): nothing is touched.
    pub fn idle(data_dir: &Path) -> Sending {
        Sending {
            path: dir(data_dir).join(SENDING_FILE),
            messages: HashMap::new(),
        }
    }

    /// Whether nothing is on its way.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// How many messages are on their way.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// The numbers of the messages on their way.
    pub fn ids(&self) -> Vec<u64> {
        self.messages.keys().copied().collect()
    }

    /// The message `id` to `peer`, which is `entry` in the chat, is on its
    /// way.
    pub fn add(&mut self, id: u64, peer: &str, entry: &Entry) {
        self.messages.insert(
            id,
            OnItsWay {
                peer: peer.to_string(),
                time: entry.time,
                text: entry.text.clone(),
            },
        );
        self.save();
    }

    pub fn get(&self, id: u64) -> Option<&OnItsWay> {
        self.messages.get(&id)
    }

    /// The message `id` has arrived or has failed, which its chat says by
    /// now: it is on its way no more.
    pub fn remove(&mut self, id: u64) -> Option<OnItsWay> {
        let message = self.messages.remove(&id)?;
        self.save();
        Some(message)
    }

    /// Writes down what is on its way, as well as that can be done: a
    /// message goes out also when its note cannot be written.
    fn save(&self) {
        if self.messages.is_empty() {
            let _ = std::fs::remove_file(&self.path);
            return;
        }
        let messages: Vec<&OnItsWay> = self.messages.values().collect();
        let Ok(json) = serde_json::to_vec(&messages) else {
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Through a temporary file, as a chat is saved.
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
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
    fn chosen_messages_are_deleted() {
        let dir = scratch("remove");
        let mut chat = History::load(&dir, "abc");
        for text in ["a", "b", "c", "d"] {
            chat.push(Entry::now(true, text)).1.unwrap();
        }
        // Twice, and one that is not there.
        chat.remove(&[3, 1, 1, 9]).unwrap();
        let texts = |chat: &History| -> Vec<String> {
            chat.entries().iter().map(|e| e.text.clone()).collect()
        };
        assert_eq!(texts(&chat), ["a", "c"]);
        assert_eq!(texts(&History::load(&dir, "abc")), ["a", "c"]);
        chat.remove(&[]).unwrap();
        assert_eq!(texts(&chat), ["a", "c"]);
        // The last ones: no chat is left on the disk.
        chat.remove(&[0, 1]).unwrap();
        assert!(chat.entries().is_empty());
        assert!(!any(&dir));
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

    /// A message that the last run left on its way is marked as not sent
    /// at the next start, and one that arrived or failed in time is left
    /// as its chat has it.
    #[test]
    fn a_message_left_on_its_way_is_marked_as_not_sent() {
        let dir = scratch("sending");
        let mut sending = Sending::start(&dir);
        let mut chat = History::load(&dir, "abc");
        let (first, second, third) = (
            Entry::now(true, "first"),
            Entry::now(true, "second"),
            Entry::now(true, "third"),
        );
        for (id, entry) in [(1, &first), (2, &second), (3, &third)] {
            chat.push(entry.clone()).1.unwrap();
            sending.add(id, "abc", entry);
        }
        assert_eq!(sending.get(2).map(|m| m.text.as_str()), Some("second"));
        // The first arrived; the program ends before the others have.
        assert_eq!(sending.remove(1).map(|m| m.text), Some("first".into()));
        assert_eq!(sending.remove(1), None);
        drop(sending);

        let sending = Sending::start(&dir);
        assert!(sending.get(2).is_none());
        let failed: Vec<bool> = History::load(&dir, "abc")
            .entries()
            .iter()
            .map(|e| e.failed)
            .collect();
        assert_eq!(failed, [false, true, true]);
        // Nothing is on its way now, and nothing is noted: another start
        // changes nothing, and the note is no chat.
        assert!(!dir.join("chats").join(SENDING_FILE).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The note of what is on its way goes once nothing is, and is not
    /// taken for a chat while it is there.
    #[test]
    fn the_note_of_messages_on_their_way_is_no_chat() {
        let dir = scratch("sending-note");
        let mut sending = Sending::start(&dir);
        sending.add(1, "abc", &Entry::now(true, "x"));
        let note = dir.join("chats").join(SENDING_FILE);
        assert!(note.exists());
        assert!(!any(&dir));
        sending.remove(1);
        assert!(!note.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_longest_message_fits_a_frame() {
        assert!(MAX_CHARS * 4 <= crate::net::protocol::MAX_CHAT_TEXT);
    }
}
