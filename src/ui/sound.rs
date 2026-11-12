//! Plays the program's sounds (see `crate::sound`), as the settings say
//! (`config::Sounds`). One sound plays at a time: a new one takes the place
//! of one that still sounds.

use std::{cell::Cell, path::Path, time::Instant};

use windows_sys::Win32::Media::Audio::{
    PlaySoundW, SND_ASYNC, SND_FILENAME, SND_MEMORY, SND_NODEFAULT,
};

use super::wide;
use crate::{app, config::SoundChoice, sound::SoundEvent};

thread_local! {
    /// When a sound was last played.
    static PLAYED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Plays the sound of `event`, if the settings have one for it. Not to be
/// called inside `app::with`.
pub fn play(event: SoundEvent) {
    if let Some(choice) = app::with(|a| a.cfg.sounds.playing(event)) {
        play_choice(event, &choice);
    }
}

/// Plays what `choice` says for `event`, whether or not sounds are on (the
/// settings try a sound with it). A file that cannot be played gives the
/// built-in sound, so that the event is not missed.
pub fn play_choice(event: SoundEvent, choice: &SoundChoice) {
    let played = match choice {
        SoundChoice::Off => return,
        SoundChoice::File(path) => play_file(path),
        SoundChoice::Default => false,
    };
    if !played {
        let wav = event.wav();
        // SAFETY: the built-in sounds live as long as the program, which
        // `SND_ASYNC` needs of a sound in memory.
        unsafe {
            PlaySoundW(
                wav.as_ptr() as *const u16,
                std::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            )
        };
    }
    PLAYED.set(Some(Instant::now()));
}

fn play_file(path: &Path) -> bool {
    // Windows would play its default sound for a file that is no sound.
    if !is_wav(path) {
        return false;
    }
    let name = wide(&path.display().to_string());
    unsafe {
        PlaySoundW(
            name.as_ptr(),
            std::ptr::null_mut(),
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        ) != 0
    }
}

/// Whether `path` is a WAV file, as far as its beginning tells.
pub fn is_wav(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 12];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok()
        && &head[..4] == b"RIFF"
        && &head[8..] == b"WAVE"
}

/// Whether one of the program's sounds began a moment ago: the notification
/// that tells of the same event then leaves out Windows' own sound (see
/// `tray::balloon`).
pub fn just_played() -> bool {
    PLAYED
        .get()
        .is_some_and(|at| at.elapsed().as_millis() < 500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_sounds_are_wav_files() {
        let dir = std::env::temp_dir().join(format!("dodavo-sound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("a.wav");
        std::fs::write(&wav, SoundEvent::Completed.wav()).unwrap();
        assert!(is_wav(&wav));
        let other = dir.join("b.wav");
        std::fs::write(&other, b"ID3 not a wave file at all").unwrap();
        assert!(!is_wav(&other));
        assert!(!is_wav(&dir.join("missing.wav")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
