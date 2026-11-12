//! Makes the built-in sounds (`res/sounds/*.wav`): short tones, each with a
//! shape of its own, so that an event can be told by ear. Used by
//! `examples/gen_sounds.rs`, which writes the files, and by the test that
//! checks them; the program itself only plays the files. No `crate::` paths
//! here: the example includes this file by its path.

/// Samples per second. Enough for tones below 4 kHz, and half the size.
pub const RATE: u32 = 22_050;

/// The names of the sounds, as their files are called.
pub const NAMES: [&str; 11] = [
    "message_received",
    "message_sent",
    "typing",
    "message_failed",
    "incoming",
    "started",
    "completed",
    "failed",
    "cancelled",
    "connection_lost",
    "continues",
];

/// How a note sounds.
#[derive(Clone, Copy)]
enum Voice {
    /// A soft bell: the tone and a little of its octave.
    Bell,
    /// Rounder and quieter, for what should not draw attention.
    Soft,
    /// With odd harmonics, which sounds like something went wrong.
    Buzz,
}

/// One note of a sound.
struct Note {
    /// When it begins, in seconds.
    at: f64,
    /// Its pitch at the beginning and at the end, in Hz.
    from: f64,
    to: f64,
    /// How long it sounds, in seconds.
    length: f64,
    /// How loud, 1 being the loudest a sound may get.
    level: f64,
    voice: Voice,
}

fn note(at: f64, freq: f64, length: f64, level: f64, voice: Voice) -> Note {
    Note {
        at,
        from: freq,
        to: freq,
        length,
        level,
        voice,
    }
}

fn sweep(at: f64, from: f64, to: f64, length: f64, level: f64, voice: Voice) -> Note {
    Note {
        at,
        from,
        to,
        length,
        level,
        voice,
    }
}

fn notes(name: &str) -> Vec<Note> {
    use Voice::*;
    match name {
        // Two notes up: someone says something.
        "message_received" => vec![
            note(0.0, 659.26, 0.16, 0.5, Bell),
            note(0.09, 880.0, 0.30, 0.5, Bell),
        ],
        // A short quiet rise: it is on its way.
        "message_sent" => vec![sweep(0.0, 440.0, 660.0, 0.11, 0.3, Soft)],
        // Two ticks, like keys.
        "typing" => vec![
            note(0.0, 1244.5, 0.03, 0.16, Soft),
            note(0.075, 1396.9, 0.03, 0.16, Soft),
        ],
        // Two low notes down: it did not get there.
        "message_failed" => vec![
            note(0.0, 329.63, 0.16, 0.4, Buzz),
            note(0.13, 246.94, 0.30, 0.4, Buzz),
        ],
        // Three notes up: something is offered.
        "incoming" => vec![
            note(0.0, 523.25, 0.15, 0.5, Bell),
            note(0.11, 659.26, 0.15, 0.5, Bell),
            note(0.22, 783.99, 0.36, 0.5, Bell),
        ],
        // A rise that sets off.
        "started" => vec![sweep(0.0, 300.0, 600.0, 0.20, 0.4, Soft)],
        // A chord, note by note, that ends high: it is there.
        "completed" => vec![
            note(0.0, 392.0, 0.14, 0.42, Bell),
            note(0.08, 523.25, 0.14, 0.42, Bell),
            note(0.16, 659.26, 0.14, 0.42, Bell),
            note(0.24, 783.99, 0.50, 0.46, Bell),
        ],
        // Three notes down, in a minor key.
        "failed" => vec![
            note(0.0, 392.0, 0.17, 0.4, Buzz),
            note(0.14, 311.13, 0.17, 0.4, Buzz),
            note(0.28, 261.63, 0.42, 0.4, Buzz),
        ],
        // Two gentle notes down: it was called off.
        "cancelled" => vec![
            note(0.0, 523.25, 0.14, 0.38, Soft),
            note(0.11, 392.0, 0.28, 0.38, Soft),
        ],
        // A fall.
        "connection_lost" => vec![sweep(0.0, 600.0, 250.0, 0.38, 0.4, Soft)],
        // The fall the other way, and a note on top: it is back.
        "continues" => vec![
            sweep(0.0, 250.0, 600.0, 0.24, 0.4, Soft),
            note(0.22, 880.0, 0.22, 0.36, Bell),
        ],
        _ => panic!("no sound is called {name}"),
    }
}

/// The samples of the sound `name`.
pub fn samples(name: &str) -> Vec<i16> {
    let notes = notes(name);
    let rate = RATE as f64;
    let end = notes.iter().map(|n| n.at + n.length).fold(0.0, f64::max);
    // A little silence at the end, so that nothing cuts the last note.
    let mut mix = vec![0f64; ((end + 0.02) * rate) as usize];
    for n in &notes {
        let first = (n.at * rate) as usize;
        let count = (n.length * rate) as usize;
        let mut phase = 0f64;
        for i in 0..count {
            let t = i as f64 / rate;
            let part = t / n.length;
            let freq = n.from + (n.to - n.from) * part;
            phase += std::f64::consts::TAU * freq / rate;
            let wave = match n.voice {
                Voice::Bell => phase.sin() + 0.25 * (2.0 * phase).sin(),
                Voice::Soft => phase.sin(),
                Voice::Buzz => phase.sin() + 0.33 * (3.0 * phase).sin() + 0.2 * (5.0 * phase).sin(),
            };
            // In at once but without a click, and out along a curve that
            // ends at silence.
            let attack = (t / 0.005).min(1.0);
            let decay = (-4.0 * part).exp() * (1.0 - part);
            mix[first + i] += wave * n.level * attack * decay;
        }
    }
    mix.into_iter()
        .map(|v| (v.clamp(-1.0, 1.0) * 30_000.0).round() as i16)
        .collect()
}

/// The sound `name` as a WAV file: 16 bits, one channel.
pub fn wav(name: &str) -> Vec<u8> {
    let samples = samples(name);
    let data = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    // PCM, one channel.
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
