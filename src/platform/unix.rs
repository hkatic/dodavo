//! POSIX: what macOS and Linux (Android among them) share below the user
//! interface.

use std::{
    ffi::{CStr, CString, c_char},
    io,
    mem::MaybeUninit,
    os::unix::ffi::OsStrExt,
    path::Path,
};

/// What `statfs` says of the volume that holds `path`.
pub(crate) fn statfs_of(path: &Path) -> io::Result<libc::statfs> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut info = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: a C string and a structure to fill, which outlive the call.
    if unsafe { libc::statfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: filled by the call.
    Ok(unsafe { info.assume_init() })
}

/// A string in a fixed buffer of a C structure.
pub(crate) fn c_text(buffer: &[c_char]) -> String {
    // SAFETY: `c_char` and `u8` have the same size and alignment.
    let bytes = unsafe { &*(std::ptr::from_ref(buffer) as *const [u8]) };
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

/// The full name of the user's account, or else its short name.
pub(crate) fn account_name() -> Option<String> {
    let mut account = MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0 as c_char; 16 * 1024];
    // SAFETY: the call fills `account`, with its strings in `buffer`, of the
    // given length; all outlive it. `found` is `account` if there is one.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            account.as_mut_ptr(),
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if status == 0 && !found.is_null() {
        // SAFETY: filled by the call above, and `pw_gecos` is a C string in
        // `buffer` or null.
        let gecos = unsafe { account.assume_init_ref().pw_gecos };
        if !gecos.is_null() {
            // SAFETY: as above.
            let name = unsafe { CStr::from_ptr(gecos) }.to_string_lossy();
            let name = name.split(',').next().unwrap_or_default().trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    std::env::var("USER").ok()
}
