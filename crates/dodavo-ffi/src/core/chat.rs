//! The chats of the Mac user interface, as `ui/chat.rs` has them for the Win32
//! one: the conversations are kept on the disk (`dodavo_core::chat`) and the
//! messages travel in `net::chat`. This holds the chats whose windows are
//! open, says what a window shows, and tells the user interface what arrives.
//! A window draws that and passes on what the user does.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use dodavo_core::{
    chat::{self, Entry, History, MAX_CHARS, Sending},
    identity,
    net::Command,
    sound::SoundEvent,
    view::Notice,
};
use serde::Serialize;

use super::{Core, Out, State, lock, peer_name, short_id, sound_out, ui};

/// How much of a message its notification shows, in characters.
const NOTICE_CHARS: usize = 200;
/// "… is typing" goes when the other side has not said so again for this long.
const PEER_TYPING: Duration = Duration::from_secs(8);
/// The other side is told that the user stopped typing once no key came for
/// this long.
const MY_TYPING: Duration = Duration::from_secs(5);
/// While the user types, the other side is told so again this often.
const TYPING_AGAIN: Duration = Duration::from_secs(3);

/// A message as a chat window shows it. The window writes the time as the
/// user's Mac does, and the line around it (`chat_head`, `chat_entry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct EntryOut {
    time: u64,
    mine: bool,
    failed: bool,
    text: String,
}

impl EntryOut {
    fn of(entry: &Entry) -> EntryOut {
        EntryOut {
            time: entry.time,
            mine: entry.mine,
            failed: entry.failed,
            text: entry.text.clone(),
        }
    }
}

/// A chat as its window shows it.
#[derive(Debug, Serialize)]
pub(super) struct ChatOut {
    /// The other person's Contact ID, as the chat is known by.
    peer: String,
    name: String,
    /// Whether the other person is a recipient.
    known: bool,
    title: String,
    /// For a person who is not a recipient: who it is.
    warning: Option<String>,
    entries: Vec<EntryOut>,
    /// Which change of the chats these messages are after (see
    /// `Chats::changes`): a window leaves out what it is told of a change
    /// that it has already.
    seq: u64,
    /// The longest message that can be written, in characters.
    max_chars: usize,
}

/// A chat whose window is open.
struct Open {
    history: History,
    /// The last change of its messages (see `Chats::changes`).
    seq: u64,
    /// What the other person calls themselves, for one who is no recipient.
    their_name: String,
    /// When the other person last said that they are typing, while the
    /// window says so.
    peer_typing: Option<Instant>,
    /// When the other side was last told that the user is typing, while it
    /// believes so.
    told_typing: Option<Instant>,
    /// When the user last typed, while the message is not sent.
    typed: Option<Instant>,
}

impl Open {
    fn load(state: &State, key: &str) -> Open {
        Open {
            history: History::load(&state.data_dir, key),
            seq: state.chats.changes,
            their_name: String::new(),
            peer_typing: None,
            told_typing: None,
            typed: None,
        }
    }
}

/// The chats that are open, and the messages on their way.
pub(super) struct Chats {
    /// By the other person's Contact ID, as `key_of` writes it.
    open: HashMap<String, Open>,
    /// By their numbers, and noted on the disk, so that one that the
    /// program ends on is not kept as sent (see `Sending`). Not in the
    /// chats: a window may be closed before its message has arrived or
    /// failed.
    unconfirmed: Sending,
    last_message: u64,
    /// How many times the messages of an open chat changed. What tells a
    /// window of a change carries its number, and so does what a window
    /// reads when it opens: a window that opens for a message has that
    /// message already when it is told of it, and one that opens again is
    /// not told of what its chat was before.
    changes: u64,
}

impl Chats {
    /// The chats of a copy that keeps them in `data_dir`. One that runs
    /// (`running`; another copy does otherwise, whose chats these are)
    /// first marks what its last run left on its way as not sent.
    pub(super) fn new(data_dir: &std::path::Path, running: bool) -> Chats {
        Chats {
            open: HashMap::new(),
            unconfirmed: if running {
                Sending::start(data_dir)
            } else {
                Sending::idle(data_dir)
            },
            last_message: 0,
            changes: 0,
        }
    }

    /// The number of a change that is made now.
    fn change(&mut self) -> u64 {
        self.changes += 1;
        self.changes
    }

    /// The longest message that can be written, in characters.
    pub(super) const MAX_CHARS: usize = MAX_CHARS;
}

/// How a Contact ID names its chat: without spaces or case.
fn key_of(peer: &str) -> String {
    peer.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The message of `time` with `text` that the user wrote and that is not
/// marked as failed, from the end: of two that are the same, each failure
/// marks one.
fn find(history: &History, time: u64, text: &str) -> Option<usize> {
    history.find_sent(time, text)
}

impl Core {
    /// Opens the chat with the person whose Contact ID is `peer`, or reads
    /// it again: `name` is what they call themselves, for someone who is not
    /// a recipient.
    pub(super) fn chat_open(&self, peer: &str, name: &str) -> ChatOut {
        let mut state = lock(&self.state);
        let key = key_of(peer);
        if !state.chats.open.contains_key(&key) {
            let open = Open::load(&state, &key);
            state.chats.open.insert(key.clone(), open);
        }
        let state = &mut *state;
        let open = state.chats.open.get_mut(&key).expect("just put there");
        if !name.trim().is_empty() {
            open.their_name = name.trim().to_string();
        }
        let (name, known) = peer_name(&state.cfg, &key, &open.their_name);
        ChatOut {
            title: ui().chat_title.replace("{name}", &name),
            warning: (!known).then(|| ui().chat_unknown_warning.replace("{id}", &short_id(&key))),
            entries: open.history.entries().iter().map(EntryOut::of).collect(),
            seq: open.seq,
            max_chars: MAX_CHARS,
            peer: key,
            name,
            known,
        }
    }

    /// The window of the chat with `peer` closed: the user is not typing
    /// there any more.
    pub(super) fn chat_closed(&self, peer: &str) {
        let mut state = lock(&self.state);
        let key = key_of(peer);
        if let Some(open) = state.chats.open.remove(&key)
            && open.told_typing.is_some()
            && let Some(to) = identity::parse_contact_id(&key)
        {
            state.net.send(Command::Typing { to, on: false });
        }
    }

    /// Adds a message of the user's to the chat with `key` and sends it.
    /// `false` when `key` is no Contact ID.
    fn say(&self, state: &mut State, key: &str, text: &str) -> bool {
        let Some(to) = identity::parse_contact_id(key) else {
            return false;
        };
        let entry = Entry::now(true, text);
        state.chats.last_message += 1;
        let id = state.chats.last_message;
        state.chats.unconfirmed.add(id, key, &entry);
        let seq = state.chats.change();
        let (gone, saved) = match state.chats.open.get_mut(key) {
            Some(open) => {
                // The message itself says that the user has stopped typing.
                open.typed = None;
                open.told_typing = None;
                open.seq = seq;
                open.history.push(entry.clone())
            }
            // No window shows it: it is in the chat when one opens.
            None => History::load(&state.data_dir, key).push(entry.clone()),
        };
        state.net.send(Command::Chat {
            to,
            id,
            text: text.to_string(),
        });
        // A chat that cannot be saved is no reason not to send.
        let problem = saved
            .err()
            .map(|e| ui().chat_not_saved.replace("{error}", &e.to_string()));
        self.outbox.push(Out::ChatAdded {
            peer: key.to_string(),
            seq,
            entry: EntryOut::of(&entry),
            gone,
            problem,
        });
        true
    }

    /// What the user wrote, as it is sent: without the room around it, and
    /// no longer than a message may be.
    fn message(text: &str) -> String {
        text.trim().chars().take(MAX_CHARS).collect()
    }

    /// Sends `text` to `peer`. An error text when that cannot be done.
    pub(super) fn chat_send(&self, peer: &str, text: &str) -> Option<String> {
        let text = Self::message(text);
        if text.is_empty() {
            return None;
        }
        let mut state = lock(&self.state);
        let key = key_of(peer);
        if self.say(&mut state, &key, &text) {
            return None;
        }
        let their_name = state
            .chats
            .open
            .get(&key)
            .map(|open| open.their_name.clone())
            .unwrap_or_default();
        let (name, _) = peer_name(&state.cfg, &key, &their_name);
        Some(ui().contact_invalid.replace("{name}", &name))
    }

    /// Sends `text` to each of the recipients as a message of its own, which
    /// is kept in the chat with that person, where the answer arrives too.
    /// Nothing is sent unless every one of them is a recipient: an error
    /// text then.
    pub(super) fn chat_send_several(&self, peer_ids: &[String], text: &str) -> Option<String> {
        let text = Self::message(text);
        let mut state = lock(&self.state);
        let contacts = match Self::recipients(&state, peer_ids) {
            Ok(contacts) => contacts,
            Err(error) => return Some(error),
        };
        if text.is_empty() {
            return None;
        }
        for contact in &contacts {
            self.say(&mut state, &key_of(&contact.id), &text);
        }
        None
    }

    /// The user is writing a message to `peer`, or has stopped: the other
    /// side is told (again, every now and then, while they are), if the
    /// settings say to.
    pub(super) fn chat_typing(&self, peer: &str, on: bool) {
        let mut state = lock(&self.state);
        let state = &mut *state;
        let key = key_of(peer);
        let Some(open) = state.chats.open.get_mut(&key) else {
            return;
        };
        let Some(to) = identity::parse_contact_id(&key) else {
            return;
        };
        let now = Instant::now();
        if on {
            open.typed = Some(now);
            let due = open
                .told_typing
                .is_none_or(|at| now.duration_since(at) >= TYPING_AGAIN);
            if due && state.cfg.send_typing {
                open.told_typing = Some(now);
                state.net.send(Command::Typing { to, on: true });
            }
        } else {
            open.typed = None;
            if open.told_typing.take().is_some() {
                state.net.send(Command::Typing { to, on: false });
            }
        }
    }

    /// Deletes the chat with `peer` from this Mac. An error text when that
    /// cannot be done.
    pub(super) fn chat_delete(&self, peer: &str) -> Option<String> {
        let mut state = lock(&self.state);
        let key = key_of(peer);
        let seq = state.chats.change();
        let cleared = match state.chats.open.get_mut(&key) {
            Some(open) => {
                open.seq = seq;
                open.history.clear()
            }
            None => History::load(&state.data_dir, &key).clear(),
        };
        cleared
            .err()
            .map(|e| ui().chat_delete_failed.replace("{error}", &e.to_string()))
    }

    /// Deletes every chat from this Mac; the open ones show none either. An
    /// error text when that cannot be done.
    pub(super) fn chat_delete_all(&self) -> Option<String> {
        let mut state = lock(&self.state);
        if let Err(e) = chat::delete_all(&state.data_dir) {
            return Some(ui().chat_delete_failed.replace("{error}", &e.to_string()));
        }
        let seq = state.chats.change();
        for open in state.chats.open.values_mut() {
            open.seq = seq;
            // Its file is gone already.
            let _ = open.history.clear();
        }
        None
    }

    /// A chat message arrived.
    pub(super) fn on_chat(&self, state: &mut State, from: String, name: String, text: String) {
        // Blocked while the message was on its way here: it is not shown,
        // as none is from now on.
        if state.cfg.is_blocked(&from) {
            return;
        }
        let key = key_of(&from);
        if !state.chats.open.contains_key(&key) {
            let open = Open::load(state, &key);
            state.chats.open.insert(key.clone(), open);
        }
        let seq = state.chats.change();
        let open = state.chats.open.get_mut(&key).expect("just put there");
        if !name.trim().is_empty() {
            open.their_name = name.trim().to_string();
        }
        // Whoever wrote is not typing any more.
        open.peer_typing = None;
        open.seq = seq;
        let entry = Entry::now(false, text.as_str());
        let (gone, saved) = open.history.push(entry.clone());
        let (name, _) = peer_name(&state.cfg, &key, &open.their_name);
        let problem = saved
            .err()
            .map(|e| ui().chat_not_saved.replace("{error}", &e.to_string()));
        self.sound(state, SoundEvent::MessageReceived);
        self.outbox.push(Out::ChatMessage {
            said: ui()
                .chat_said
                .replace("{name}", &name)
                .replace("{text}", &text),
            notice: Notice {
                title: ui().chat_balloon_title.replace("{name}", &name),
                text: text.chars().take(NOTICE_CHARS).collect(),
            },
            peer: key,
            name,
            seq,
            entry: EntryOut::of(&entry),
            gone,
            problem,
        });
    }

    /// The message `id` arrived at the other side.
    pub(super) fn on_chat_sent(&self, state: &mut State, id: u64) {
        if state.chats.unconfirmed.remove(id).is_some() {
            self.sound(state, SoundEvent::MessageSent);
        }
    }

    /// The message `id` was not delivered, for the reason `error`.
    pub(super) fn on_chat_failed(&self, state: &mut State, id: u64, error: &str) {
        let Some(message) = state.chats.unconfirmed.remove(id) else {
            return;
        };
        let seq = state.chats.change();
        let (index, their_name) = match state.chats.open.get_mut(&message.peer) {
            Some(open) => {
                let index = find(&open.history, message.time, &message.text);
                if let Some(index) = index {
                    open.seq = seq;
                    let _ = open.history.set_failed(index, true);
                }
                (index, open.their_name.clone())
            }
            // Its window was closed meanwhile: the chat on the disk says
            // that the message was not sent.
            None => {
                let mut history = History::load(&state.data_dir, &message.peer);
                if let Some(index) = find(&history, message.time, &message.text) {
                    let _ = history.set_failed(index, true);
                }
                (None, String::new())
            }
        };
        let (name, _) = peer_name(&state.cfg, &message.peer, &their_name);
        let said = ui().chat_not_sent.replace("{error}", error);
        self.sound(state, SoundEvent::MessageFailed);
        self.outbox.push(Out::ChatFailed {
            notice: Notice {
                title: ui().chat_title.replace("{name}", &name),
                text: said.clone(),
            },
            peer: message.peer,
            name,
            seq,
            index,
            text: message.text,
            said,
        });
    }

    /// The other person of a chat is typing, or has stopped. Only an open
    /// chat shows it, and says it once.
    pub(super) fn on_peer_typing(&self, state: &mut State, from: &str, on: bool) {
        let key = key_of(from);
        let Some(open) = state.chats.open.get_mut(&key) else {
            return;
        };
        let stopped = Out::ChatTyping {
            peer: key.clone(),
            text: None,
            announce: false,
            sound: None,
        };
        if !on {
            if open.peer_typing.take().is_some() {
                self.outbox.push(stopped);
            }
            return;
        }
        if open.peer_typing.replace(Instant::now()).is_some() {
            return;
        }
        let (name, _) = peer_name(&state.cfg, &key, &open.their_name);
        let announce = state.cfg.announce_typing;
        self.outbox.push(Out::ChatTyping {
            peer: key,
            text: Some(ui().chat_typing.replace("{name}", &name)),
            announce,
            // Played with the announcement, in the window in front.
            sound: announce
                .then(|| sound_out(&state.cfg, SoundEvent::Typing))
                .flatten(),
        });
    }

    /// What is due in the chats without a word from anyone.
    pub(super) fn tick_chats(&self, state: &mut State, now: Instant) {
        for (key, open) in &mut state.chats.open {
            if open
                .peer_typing
                .is_some_and(|at| now.duration_since(at) >= PEER_TYPING)
            {
                open.peer_typing = None;
                self.outbox.push(Out::ChatTyping {
                    peer: key.clone(),
                    text: None,
                    announce: false,
                    sound: None,
                });
            }
            if open
                .typed
                .is_some_and(|at| now.duration_since(at) >= MY_TYPING)
            {
                open.typed = None;
                if open.told_typing.take().is_some()
                    && let Some(to) = identity::parse_contact_id(key)
                {
                    state.net.send(Command::Typing { to, on: false });
                }
            }
        }
    }

    /// Whether any chat is kept, for the button that deletes them all.
    pub(super) fn any_chats(&self) -> bool {
        chat::any(&lock(&self.state).data_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{core_with_offer, test_dir};
    use super::*;
    use dodavo_core::{
        config::{Contact, SoundChoice},
        net::Event,
    };

    /// A Contact ID of someone.
    fn id() -> String {
        identity::new_key().public().to_string()
    }

    fn taken(core: &Core) -> Vec<Out> {
        lock(&core.outbox.queue).drain(..).collect()
    }

    /// A message that arrives is kept, shown with the name its sender is
    /// known by, and has its sound and its notification.
    #[test]
    fn a_message_that_arrives_is_kept_and_shown() {
        let dir = test_dir("chat-arrives");
        let core = core_with_offer(1, &dir);
        let from = id();
        let event = |text: &str| Event::Chat {
            from: from.parse().unwrap(),
            name: "  Ana  ".into(),
            text: text.into(),
        };

        core.on_event(event("Šaljem ti slike."));

        let out = taken(&core);
        let [
            Out::Sound(sound),
            Out::ChatMessage {
                peer,
                name,
                seq,
                entry,
                said,
                notice,
                gone,
                problem,
            },
        ] = &out[..]
        else {
            panic!("{out:?}");
        };
        assert_eq!(sound.event, "message_received");
        assert_eq!((peer.as_str(), name.as_str()), (from.as_str(), "Ana"));
        assert_eq!((entry.mine, entry.failed, entry.text.as_str()), (false, false, "Šaljem ti slike."));
        assert_eq!(said, "Ana: Šaljem ti slike.");
        assert!(notice.title.contains("Ana") && notice.text == "Šaljem ti slike.");
        assert_eq!((*gone, problem), (0, &None));

        // The window that opens for it shows it, and who it is from, and
        // has it already when it is told of it.
        let chat = core.chat_open(&from.to_uppercase(), "");
        assert_eq!(chat.seq, *seq);
        assert_eq!((chat.peer.as_str(), chat.name.as_str(), chat.known), (from.as_str(), "Ana", false));
        assert!(chat.warning.is_some() && chat.title.contains("Ana"));
        assert_eq!(chat.entries.len(), 1);

        // A recipient is shown under the name the user gave them.
        lock(&core.state).cfg.add_contact(Contact::new("Sister", &from));
        let chat = core.chat_open(&from, "");
        assert_eq!((chat.name.as_str(), chat.known, &chat.warning), ("Sister", true, &None));

        // With the window closed, the next one is kept all the same, and
        // a window that opens later is told nothing that was before it.
        core.chat_closed(&from);
        core.on_event(event("Jesi li ih dobio?"));
        let out = taken(&core);
        let Some(Out::ChatMessage { seq: told, .. }) = out.last() else {
            panic!("{out:?}");
        };
        core.chat_closed(&from);
        core.on_event(Event::Chat {
            from: id().parse().unwrap(),
            name: String::new(),
            text: "Someone else writes meanwhile.".into(),
        });
        taken(&core);
        assert!(core.chat_open(&from, "").seq > *told);
        core.chat_closed(&from);
        assert_eq!(History::load(&dir, &from).entries().len(), 2);

        // Nothing from a sender who was blocked meanwhile.
        lock(&core.state).cfg.block("Sister", &from);
        core.on_event(event("Halo?"));
        assert!(taken(&core).is_empty());
        assert_eq!(History::load(&dir, &from).entries().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A message of the user's is in the chat at once, and marked there when
    /// it could not be delivered, also once its window is closed.
    #[test]
    fn a_message_that_fails_is_marked() {
        let dir = test_dir("chat-fails");
        let core = core_with_offer(1, &dir);
        let to = id();
        let failed = |core: &Core| {
            let id = lock(&core.state).chats.unconfirmed.ids()[0];
            core.on_event(Event::ChatFailed {
                to: to.parse().unwrap(),
                id,
                error: "offline".into(),
            });
            taken(core)
        };

        core.chat_open(&to, "Bo");
        assert_eq!(core.chat_send(&to, "  Jesi li tu?  "), None);
        let out = taken(&core);
        let [Out::ChatAdded { entry, .. }] = &out[..] else {
            panic!("{out:?}");
        };
        assert_eq!((entry.mine, entry.text.as_str()), (true, "Jesi li tu?"));

        let out = failed(&core);
        let [Out::Sound(sound), Out::ChatFailed { index, text, said, name, .. }] = &out[..] else {
            panic!("{out:?}");
        };
        assert_eq!(sound.event, "message_failed");
        assert_eq!((*index, text.as_str(), name.as_str()), (Some(0), "Jesi li tu?", "Bo"));
        assert!(said.contains("offline") && !said.contains('{'), "{said}");
        assert!(core.chat_open(&to, "").entries[0].failed);
        // Said once: the network says it once.
        assert!(lock(&core.state).chats.unconfirmed.is_empty());

        // The window closes before the next one fails.
        assert_eq!(core.chat_send(&to, "Halo?"), None);
        core.chat_closed(&to);
        taken(&core);
        let out = failed(&core);
        let [_, Out::ChatFailed { index: None, notice, .. }] = &out[..] else {
            panic!("{out:?}");
        };
        assert!(notice.text.contains("offline"));
        let kept = History::load(&dir, &to);
        assert!(kept.entries().iter().all(|e| e.mine && e.failed), "{kept:?}");

        // Confirmed, a message has its sound and nothing else.
        assert_eq!(core.chat_send(&to, "Tu sam."), None);
        taken(&core);
        let sent = lock(&core.state).chats.unconfirmed.ids()[0];
        core.on_event(Event::ChatSent {
            to: to.parse().unwrap(),
            id: sent,
        });
        let out = taken(&core);
        assert!(matches!(&out[..], [Out::Sound(sound)] if sound.event == "message_sent"), "{out:?}");

        // Nothing to send, and nobody to send to.
        assert_eq!(core.chat_send(&to, "   "), None);
        assert!(core.chat_send("not an id", "Halo?").is_some());
        assert!(taken(&core).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A message that the app quit on before the other side had confirmed
    /// it is not kept as sent: the next start marks it.
    #[test]
    fn a_message_that_the_app_quits_on_is_marked() {
        let dir = test_dir("chat-quit");
        let to = id();
        {
            let core = core_with_offer(1, &dir);
            assert_eq!(core.chat_send(&to, "Stiglo?"), None);
            let sent = lock(&core.state).chats.unconfirmed.ids()[0];
            core.on_event(Event::ChatSent {
                to: to.parse().unwrap(),
                id: sent,
            });
            assert_eq!(core.chat_send(&to, "A ovo?"), None);
        }
        let core = core_with_offer(1, &dir);
        let failed: Vec<bool> = core
            .chat_open(&to, "")
            .entries
            .iter()
            .map(|e| e.failed)
            .collect();
        assert_eq!(failed, [false, true]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One message to several people is a message of its own for each, and
    /// goes to all of them or to nobody.
    #[test]
    fn a_message_to_several_is_one_for_each() {
        let dir = test_dir("chat-several");
        let core = core_with_offer(1, &dir);
        let ids = [id(), id()];
        {
            let mut state = lock(&core.state);
            state.cfg.add_contact(Contact::new("Ana", &ids[0]));
            state.cfg.add_contact(Contact::new("Bo", &ids[1]));
        }
        let mut with_a_stranger = ids.to_vec();
        with_a_stranger.push(id());
        assert_eq!(
            core.chat_send_several(&with_a_stranger, "Stiže mapa."),
            Some(ui().recipient_gone.to_string())
        );
        assert!(taken(&core).is_empty());

        core.chat_open(&ids[0], "");
        assert_eq!(core.chat_send_several(&ids, "Stiže mapa."), None);
        assert_eq!(taken(&core).len(), 2);
        assert_eq!(lock(&core.state).chats.unconfirmed.len(), 2);
        for id in &ids {
            core.chat_closed(id);
            let kept = History::load(&dir, id);
            assert_eq!(kept.entries().len(), 1, "{id}");
            assert!(kept.entries()[0].mine);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// That the other person is typing is said once, with its sound only
    /// where it is announced, and lapses by itself.
    #[test]
    fn typing_is_said_once_and_lapses() {
        let dir = test_dir("chat-typing");
        let core = core_with_offer(1, &dir);
        let from = id();
        let typing = |on| {
            core.on_event(Event::Typing {
                from: from.parse().unwrap(),
                on,
            });
            taken(&core)
        };

        // No window shows it.
        assert!(typing(true).is_empty());
        core.chat_open(&from, "Ana");
        let out = typing(true);
        let [Out::ChatTyping { text: Some(text), announce: true, sound: Some(sound), .. }] =
            &out[..]
        else {
            panic!("{out:?}");
        };
        assert!(text.contains("Ana"));
        assert_eq!(sound.event, "typing");
        assert!(typing(true).is_empty());
        let out = typing(false);
        assert!(matches!(&out[..], [Out::ChatTyping { text: None, .. }]), "{out:?}");
        assert!(typing(false).is_empty());

        // Not announced, it has no sound either; and it lapses.
        {
            let mut state = lock(&core.state);
            state.cfg.announce_typing = false;
            state.cfg.sounds.set(SoundEvent::Typing, SoundChoice::Off);
        }
        let out = typing(true);
        assert!(
            matches!(&out[..], [Out::ChatTyping { text: Some(_), announce: false, sound: None, .. }]),
            "{out:?}"
        );
        {
            let mut state = lock(&core.state);
            core.tick_chats(&mut state, Instant::now() + PEER_TYPING - Duration::from_secs(1));
            assert!(lock(&core.outbox.queue).is_empty());
            core.tick_chats(&mut state, Instant::now() + PEER_TYPING);
        }
        let out = taken(&core);
        assert!(matches!(&out[..], [Out::ChatTyping { text: None, .. }]), "{out:?}");

        // The user's own typing is noted for the chat that is open, and is
        // over with the message, or after a while without a key.
        core.chat_typing(&from, true);
        assert!(lock(&core.state).chats.open[&from].told_typing.is_some());
        core.chat_send(&from, "Evo.");
        assert!(lock(&core.state).chats.open[&from].told_typing.is_none());
        core.chat_typing(&from, true);
        {
            let mut state = lock(&core.state);
            core.tick_chats(&mut state, Instant::now() + MY_TYPING);
            assert!(state.chats.open[&from].told_typing.is_none());
            state.cfg.send_typing = false;
        }
        core.chat_typing(&from, true);
        assert!(lock(&core.state).chats.open[&from].told_typing.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Deleting a chat, or all of them, leaves the open windows empty too.
    #[test]
    fn chats_are_deleted_from_the_disk_and_the_windows() {
        let dir = test_dir("chat-delete");
        let core = core_with_offer(1, &dir);
        let (a, b) = (id(), id());
        core.chat_open(&a, "");
        core.chat_send(&a, "1");
        core.chat_send(&b, "2");
        assert!(core.any_chats());
        assert_eq!(core.chat_delete(&a), None);
        assert!(core.chat_open(&a, "").entries.is_empty());
        assert!(core.any_chats());
        core.chat_send(&a, "3");
        assert_eq!(core.chat_delete_all(), None);
        assert!(!core.any_chats());
        assert!(core.chat_open(&a, "").entries.is_empty());
        assert!(core.chat_open(&b, "").entries.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
