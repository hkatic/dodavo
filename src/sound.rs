//! The sounds of the program: which events have one, the built-in sound of
//! each, and which one a transfer's change calls for. Playing them is each
//! user interface's (`ui::sound` on Windows, `Sounds.swift` on a Mac), and
//! what the user chose for an event is in `config::Sounds`.

use crate::net::{Outcome, Phase};

/// An event that has a sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SoundEvent {
    /// A chat message arrived.
    MessageReceived,
    /// A chat message of the user's is with the other person.
    MessageSent,
    /// The other person began to write a message.
    Typing,
    /// A chat message could not be sent.
    MessageFailed,
    /// Someone offers an item, or one from a known sender began to arrive.
    Incoming,
    /// A transfer began: a send was started, or an offer accepted.
    Started,
    Completed,
    /// A transfer failed, or the other computer could not be reached.
    Failed,
    /// A transfer was declined or cancelled, on either side.
    Cancelled,
    /// A transfer lost its connection and waits for the next one.
    ConnectionLost,
    /// A transfer that waited goes on.
    Continues,
}

impl SoundEvent {
    /// In the order shown in the settings.
    pub const ALL: [SoundEvent; 11] = [
        SoundEvent::MessageReceived,
        SoundEvent::MessageSent,
        SoundEvent::Typing,
        SoundEvent::MessageFailed,
        SoundEvent::Incoming,
        SoundEvent::Started,
        SoundEvent::Completed,
        SoundEvent::Failed,
        SoundEvent::Cancelled,
        SoundEvent::ConnectionLost,
        SoundEvent::Continues,
    ];

    /// The event's name in the settings file and of its built-in sound's
    /// file. Never change one: the settings of earlier versions name it.
    pub fn key(self) -> &'static str {
        match self {
            SoundEvent::MessageReceived => "message_received",
            SoundEvent::MessageSent => "message_sent",
            SoundEvent::Typing => "typing",
            SoundEvent::MessageFailed => "message_failed",
            SoundEvent::Incoming => "incoming",
            SoundEvent::Started => "started",
            SoundEvent::Completed => "completed",
            SoundEvent::Failed => "failed",
            SoundEvent::Cancelled => "cancelled",
            SoundEvent::ConnectionLost => "connection_lost",
            SoundEvent::Continues => "continues",
        }
    }

    /// The event that the settings file calls `key`.
    pub fn from_key(key: &str) -> Option<SoundEvent> {
        SoundEvent::ALL.into_iter().find(|event| event.key() == key)
    }

    /// The built-in sound: a WAV file (see `sound_synth`).
    pub fn wav(self) -> &'static [u8] {
        macro_rules! built_in {
            ($name:literal) => {
                include_bytes!(concat!("../res/sounds/", $name, ".wav"))
            };
        }
        match self {
            SoundEvent::MessageReceived => built_in!("message_received"),
            SoundEvent::MessageSent => built_in!("message_sent"),
            SoundEvent::Typing => built_in!("typing"),
            SoundEvent::MessageFailed => built_in!("message_failed"),
            SoundEvent::Incoming => built_in!("incoming"),
            SoundEvent::Started => built_in!("started"),
            SoundEvent::Completed => built_in!("completed"),
            SoundEvent::Failed => built_in!("failed"),
            SoundEvent::Cancelled => built_in!("cancelled"),
            SoundEvent::ConnectionLost => built_in!("connection_lost"),
            SoundEvent::Continues => built_in!("continues"),
        }
    }
}

/// The sound of a transfer that goes from the phase `before` to `phase`:
/// when it loses its connection, and when it goes on after that.
pub fn phase_sound(before: Option<Phase>, phase: Phase) -> Option<SoundEvent> {
    let lost = |phase| matches!(phase, Phase::WaitingForConnection { lost: true });
    // The first phase of a transfer is no change: one that is picked up
    // after a restart begins as waiting for its connection.
    let before = before?;
    match phase {
        _ if lost(before) == lost(phase) => None,
        _ if lost(phase) => Some(SoundEvent::ConnectionLost),
        Phase::Transferring | Phase::Verifying => Some(SoundEvent::Continues),
        _ => None,
    }
}

/// The sound of a transfer that ended with `outcome`.
pub fn outcome_sound(outcome: &Outcome) -> SoundEvent {
    match outcome {
        Outcome::Completed(_) => SoundEvent::Completed,
        Outcome::Declined | Outcome::Cancelled | Outcome::CancelledByPeer => SoundEvent::Cancelled,
        Outcome::Unreachable(_) | Outcome::Failed(_) => SoundEvent::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sound_synth as synth;

    #[test]
    fn a_lost_connection_and_its_return_have_sounds() {
        let lost = Phase::WaitingForConnection { lost: true };
        let waiting = Phase::WaitingForConnection { lost: false };
        assert_eq!(
            phase_sound(Some(Phase::Transferring), lost),
            Some(SoundEvent::ConnectionLost)
        );
        assert_eq!(phase_sound(Some(lost), lost), None);
        assert_eq!(
            phase_sound(Some(lost), Phase::Transferring),
            Some(SoundEvent::Continues)
        );
        assert_eq!(
            phase_sound(Some(lost), Phase::Verifying),
            Some(SoundEvent::Continues)
        );
        // Paused meanwhile, or picked up after a restart: nothing went on.
        assert_eq!(phase_sound(Some(lost), Phase::Paused), None);
        assert_eq!(phase_sound(Some(waiting), Phase::Transferring), None);
        assert_eq!(phase_sound(None, Phase::Connecting), None);
        // A transfer of the last run begins like this, with nothing lost.
        assert_eq!(phase_sound(None, lost), None);
        assert_eq!(phase_sound(Some(Phase::Connecting), Phase::Transferring), None);
    }

    #[test]
    fn every_end_of_a_transfer_has_its_sound() {
        assert_eq!(outcome_sound(&Outcome::Completed(None)), SoundEvent::Completed);
        for outcome in [Outcome::Declined, Outcome::Cancelled, Outcome::CancelledByPeer] {
            assert_eq!(outcome_sound(&outcome), SoundEvent::Cancelled);
        }
        assert_eq!(outcome_sound(&Outcome::Failed("x".into())), SoundEvent::Failed);
        assert_eq!(outcome_sound(&Outcome::Unreachable("x".into())), SoundEvent::Failed);
    }

    #[test]
    fn an_event_is_found_by_its_key() {
        for event in SoundEvent::ALL {
            assert_eq!(SoundEvent::from_key(event.key()), Some(event));
        }
        assert_eq!(SoundEvent::from_key("something_new"), None);
    }

    #[test]
    fn every_event_has_its_file() {
        let keys: Vec<_> = SoundEvent::ALL.iter().map(|e| e.key()).collect();
        assert_eq!(keys, synth::NAMES);
    }

    /// The files in `res/sounds` are what `examples/gen_sounds.rs` writes.
    /// A sample may differ by one: the sine is not the same to the last bit
    /// on every system.
    #[test]
    fn the_sounds_are_current() {
        for event in SoundEvent::ALL {
            let made = synth::wav(event.key());
            let kept = event.wav();
            assert_eq!(made.len(), kept.len(), "{}: run gen_sounds", event.key());
            assert_eq!(made[..44], kept[..44], "{}", event.key());
            let samples = |wav: &[u8]| -> Vec<i16> {
                wav[44..]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect()
            };
            for (a, b) in samples(&made).iter().zip(samples(kept)) {
                assert!((a - b).abs() <= 1, "{}: run gen_sounds", event.key());
            }
        }
    }

    #[test]
    fn the_sounds_are_short_and_not_silent() {
        for event in SoundEvent::ALL {
            let samples = synth::samples(event.key());
            let seconds = samples.len() as f64 / synth::RATE as f64;
            assert!(seconds < 1.0, "{}: {seconds} s", event.key());
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak > 2000 && peak < 30_000, "{}: {peak}", event.key());
        }
    }
}
