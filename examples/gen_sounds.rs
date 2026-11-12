//! Writes the built-in sounds into `res/sounds` (or the folder given):
//! `cargo run --example gen_sounds`. Run it after changing
//! `src/sound_synth.rs` and commit the files; a test checks that they are
//! current. The library includes the files, so cargo builds nothing while
//! one is missing: `rustc --edition 2024 examples/gen_sounds.rs` then makes
//! this program alone, which needs nothing else.

#[path = "../src/sound_synth.rs"]
mod synth;

fn main() -> std::io::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "res/sounds".into());
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir)?;
    for name in synth::NAMES {
        let path = dir.join(format!("{name}.wav"));
        let wav = synth::wav(name);
        // Only what changed: the program is rebuilt when a file is newer.
        if std::fs::read(&path).ok().as_deref() != Some(&wav[..]) {
            std::fs::write(&path, wav)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
