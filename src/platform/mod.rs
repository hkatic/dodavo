//! What differs between the operating systems, below the user interface.

#[cfg(unix)]
mod unix;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub use self::linux::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;
