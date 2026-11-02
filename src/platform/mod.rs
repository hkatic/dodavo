//! What differs between the operating systems, below the user interface.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;
