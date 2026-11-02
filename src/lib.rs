//! The parts of Dodavo that do not depend on a user interface: the transfers,
//! the settings, the translations and the update check. Each platform's user
//! interface is built on top of this: the Win32 one in `src/main.rs` and
//! `src/ui`.

pub mod archive;
pub mod config;
pub mod format;
pub mod i18n;
pub mod identity;
pub mod net;
pub mod platform;
pub mod space;
pub mod update;
