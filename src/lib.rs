//! The parts of Dodavo that do not depend on a user interface: the transfers,
//! the settings, the translations and the update check. Each platform's user
//! interface is built on top of this: the Win32 one in `src/main.rs` and
//! `src/ui`.

/// The author, the website (the repository on GitHub) and the years of the
/// copyright notice, from Cargo.toml.
pub const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
pub const WEBSITE: &str = env!("CARGO_PKG_REPOSITORY");
pub const COPYRIGHT_YEARS: &str = env!("DODAVO_COPYRIGHT_YEARS");
/// The page that Help → Donate opens. The help pages name it too (a test
/// checks them).
pub const DONATE_URL: &str = "https://paypal.me/hrvix";

pub mod archive;
pub mod config;
pub mod format;
pub mod i18n;
pub mod identity;
pub mod net;
pub mod platform;
pub mod space;
pub mod update;
pub mod view;
