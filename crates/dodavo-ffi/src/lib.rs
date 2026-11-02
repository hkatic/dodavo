//! What the Mac user interface (`macos/`, in Swift) calls: the state of the
//! program, kept here, behind a few C functions that take and give JSON.
//!
//! Swift draws what this says and passes on what the user does, nothing more,
//! so that what a transfer shows and what can be done with it are worked out
//! once and tested in Rust (see `dodavo_core::view`). The functions may be
//! called from the main thread only. Messages for the user interface wait in a
//! queue: `wake` says that there are some (from any thread), and the main
//! thread takes them with `dodavo_poll`, as the Win32 interface does with
//! `WM_APP_NET`.

use std::{
    ffi::{CStr, CString, c_char, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::OnceLock,
};

mod core;

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
    let reply = catch_unwind(AssertUnwindSafe(|| {
        let (core, started) = Core::start(text_of(options), wake);
        let _ = CORE.set(core);
        serde_json::to_string(&started).unwrap_or_default()
    }));
    to_c(reply.unwrap_or_else(|_| "{\"fatal\":\"The program could not start.\"}".into()))
}

/// The next message for the user interface, as JSON, or null when there is
/// none. Free it with [`dodavo_free`].
#[unsafe(no_mangle)]
pub extern "C" fn dodavo_poll() -> *mut c_char {
    let next = catch_unwind(|| CORE.get().and_then(Core::poll));
    match next {
        Ok(Some(message)) => to_c(message),
        _ => std::ptr::null_mut(),
    }
}

/// Does what `call` (JSON) says and returns the answer as JSON (see
/// [`core::Call`]). Free it with [`dodavo_free`].
///
/// # Safety
///
/// `call` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dodavo_call(call: *const c_char) -> *mut c_char {
    let call = text_of(call);
    let reply = catch_unwind(AssertUnwindSafe(|| {
        CORE.get()
            .map_or_else(|| "null".to_string(), |core| core.call(call))
    }));
    to_c(reply.unwrap_or_else(|_| "null".into()))
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

#[cfg(test)]
mod tests {
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
