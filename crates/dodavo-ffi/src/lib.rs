//! What the Mac user interface (`macos/`, in Swift) and Android's (`android/`,
//! in Kotlin, through `crates/dodavo-android`) call: the state of the
//! program, kept here, behind a few functions that take and give JSON.
//!
//! The user interface draws what this says and passes on what the user does,
//! nothing more, so that what a transfer shows and what can be done with it
//! are worked out once and tested in Rust (see `dodavo_core::view`). On a Mac
//! the functions may be called from the main thread only. Messages for the
//! user interface wait in a queue: on a Mac, `wake` says that there are some
//! (from any thread), and the main thread takes them with `dodavo_poll`, as
//! the Win32 interface does with `WM_APP_NET`; Android's app has a thread wait
//! for them ([`wait`]).

use std::{
    ffi::{CStr, CString, c_char, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::OnceLock,
};

mod core;
#[cfg(all(test, target_os = "macos"))]
mod help;

use core::Core;

/// What to call when a message waits: `wake(context)`, from any thread.
#[derive(Clone, Copy)]
pub struct Wake {
    function: extern "C" fn(*mut c_void),
    context: *mut c_void,
}

// SAFETY: the C side says that it can be called from any thread with this
// context, which is all that is done with it.
unsafe impl Send for Wake {}
// SAFETY: as above.
unsafe impl Sync for Wake {}

impl Wake {
    fn call(&self) {
        (self.function)(self.context);
    }
}

static CORE: OnceLock<Core> = OnceLock::new();

/// Starts the program: `options` is JSON with its command line (see
/// [`core::Options`]). Returns JSON with what the user interface needs to
/// begin (see [`core::Started`]). Only the first call starts it; the later
/// ones get the same answer, as Android's activities come and go while the
/// program runs. A panic gives a `fatal` answer.
pub fn start(options: &str, wake: Option<Wake>) -> String {
    static STARTED: OnceLock<String> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            catch_unwind(AssertUnwindSafe(|| {
                let (core, started) = Core::start(options, wake);
                let _ = CORE.set(core);
                serde_json::to_string(&started).unwrap_or_default()
            }))
            .unwrap_or_else(|_| "{\"fatal\":\"The program could not start.\"}".into())
        })
        .clone()
}

/// Does what `call` (JSON) says and returns the answer as JSON (see
/// [`core::Call`]): `null` before the start, for what cannot be read, and
/// after a panic.
pub fn call(call: &str) -> String {
    catch_unwind(AssertUnwindSafe(|| {
        CORE.get()
            .map_or_else(|| "null".to_string(), |core| core.call(call))
    }))
    .unwrap_or_else(|_| "null".into())
}

/// The next message for the user interface, as JSON, or none.
pub fn poll() -> Option<String> {
    catch_unwind(|| CORE.get().and_then(Core::poll)).ok().flatten()
}

/// The next message, waiting for one at most `timeout` (Android).
pub fn wait(timeout: std::time::Duration) -> Option<String> {
    match CORE.get() {
        Some(core) => catch_unwind(AssertUnwindSafe(|| core.wait(timeout)))
            .ok()
            .flatten(),
        None => {
            std::thread::sleep(timeout);
            None
        }
    }
}

/// The built-in sound of the event that the settings call `event` (see
/// `dodavo_core::sound::SoundEvent::key`): a WAV file, in memory for as long
/// as the program runs.
pub fn sound(event: &str) -> Option<&'static [u8]> {
    dodavo_core::sound::SoundEvent::from_key(event).map(dodavo_core::sound::SoundEvent::wav)
}

fn to_c(text: String) -> *mut c_char {
    // No NUL in JSON, which escapes it.
    CString::new(text).unwrap_or_default().into_raw()
}

fn text_of<'a>(text: *const c_char) -> &'a str {
    if text.is_null() {
        return "";
    }
    // SAFETY: the caller passes a NUL-terminated string that outlives the call.
    unsafe { CStr::from_ptr(text) }.to_str().unwrap_or("")
}

/// Starts the program: `options` is JSON with its command line (see
/// [`core::Options`]). Returns JSON with what the user interface needs to begin
/// (see [`core::Started`]); free it with [`dodavo_free`].
///
/// # Safety
///
/// `options` is a NUL-terminated string, and `context` is valid for `wake` for
/// as long as the program runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dodavo_start(
    options: *const c_char,
    wake: extern "C" fn(*mut c_void),
    context: *mut c_void,
) -> *mut c_char {
    let wake = Wake {
        function: wake,
        context,
    };
    to_c(start(text_of(options), Some(wake)))
}

/// The next message for the user interface, as JSON, or null when there is
/// none. Free it with [`dodavo_free`].
#[unsafe(no_mangle)]
pub extern "C" fn dodavo_poll() -> *mut c_char {
    poll().map_or(std::ptr::null_mut(), to_c)
}

/// Does what `call` (JSON) says and returns the answer as JSON (see
/// [`core::Call`]). Free it with [`dodavo_free`].
///
/// # Safety
///
/// `call` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dodavo_call(call: *const c_char) -> *mut c_char {
    to_c(self::call(text_of(call)))
}

/// The built-in sound of `event` (see [`sound`]): its bytes, whose number is
/// written to `len`. They are not to be freed. Null for an event that has
/// no sound.
///
/// # Safety
///
/// `event` is a NUL-terminated string, and `len` points to a `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dodavo_sound(event: *const c_char, len: *mut usize) -> *const u8 {
    let wav = sound(text_of(event));
    if !len.is_null() {
        // SAFETY: as the caller says.
        unsafe { *len = wav.map_or(0, <[u8]>::len) };
    }
    wav.map_or(std::ptr::null(), <[u8]>::as_ptr)
}

/// Frees a string that one of these functions returned.
///
/// # Safety
///
/// `text` came from one of these functions and was not freed yet, or is null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dodavo_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: as the caller says.
        drop(unsafe { CString::from_raw(text) });
    }
}

/// The Swift enum of the text keys (`macos/Sources/Dodavo/Keys.generated.swift`).
#[cfg(target_os = "macos")]
pub fn swift_keys() -> String {
    let table = dodavo_core::i18n::mac::strings(dodavo_core::i18n::Lang::En);
    let mut out = String::from(
        "// Made by `cargo run -p dodavo-ffi --example gen_keys`. Do not edit.\n\n\
         /// The texts of the user interface (`src/i18n/mac`).\n\
         enum K: String {\n",
    );
    for (key, _) in table.texts() {
        out.push_str(&format!("    case {key}\n"));
    }
    out.push_str("}\n\n/// The texts that depend on a number.\nenum PluralK: String {\n");
    for (key, _) in table.plurals() {
        out.push_str(&format!("    case {key}\n"));
    }
    out.push_str("}\n");
    out
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn every_event_gives_its_built_in_sound() {
        for event in dodavo_core::sound::SoundEvent::ALL {
            let wav = super::sound(event.key()).expect(event.key());
            assert_eq!((&wav[..4], &wav[8..12]), (&b"RIFF"[..], &b"WAVE"[..]));
        }
        assert!(super::sound("something_new").is_none());
    }

    #[test]
    fn the_swift_keys_are_current() {
        let file = include_str!("../../../macos/Sources/Dodavo/Keys.generated.swift");
        assert_eq!(
            file,
            super::swift_keys(),
            "run: cargo run -p dodavo-ffi --example gen_keys > macos/Sources/Dodavo/Keys.generated.swift"
        );
    }
}
