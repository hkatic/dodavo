//! The public keys whose signatures on an update manifest are trusted.
//!
//! Generated once with `dodavo-release keygen`; the matching secret key is
//! kept outside the repository (see the release guide). To rotate a key, add
//! the new public key here in a release that is still signed with the old
//! key, and switch to signing with the new key once that release is out.
//!
//! This file is also compiled into the release tool (`src/bin/dodavo-release.rs`).

/// Each entry is an Ed25519 public key as 64 hex digits.
pub const PUBLIC_KEYS_HEX: &[&str] = &[
    // Generated 2026-10-31; the secret is in the private release folder.
    "9b47c1cc53a9a2d21ce6270870faf37e4703ddc85472a191cc4860acd0e4c142",
];
