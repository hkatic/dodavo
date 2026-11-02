//! What differs between the operating systems, below the user interface.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;
