//! The update manifest (`update.json`): what the published release says about
//! itself. See the "Releasing an update" section of README.md for the format.
//!
//! This file is also compiled into the release tool (`src/bin/dodavo-release.rs`),
//! so it must not refer to the rest of the program.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::version::Version;

/// The manifest format this program understands. A manifest with a greater
/// number is ignored, so a future incompatible change never confuses old
/// installations; additive changes keep the number and use `#[serde(default)]`.
pub const FORMAT: u32 = 1;
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_NOTES_CHARS: usize = 64 * 1024;
/// The language whose release notes are required and shown when the user's
/// language has none.
pub const FALLBACK_LANG: &str = "en";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub format: u32,
    /// The released version, as in Cargo.toml.
    pub version: String,
    /// Release date (`YYYY-MM-DD`), for people reading the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    pub installer: Installer,
    /// Ed25519 signature (hex) over `statement::statement(...)`.
    pub signature: String,
    /// Release notes as plain text, by language code (`en`, `hr`, `sr-Latn`, `sl`).
    #[serde(default)]
    pub notes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Installer {
    pub url: String,
    /// Exact size in bytes.
    pub size: u64,
    /// SHA-256 of the file, 64 lowercase hex digits.
    pub sha256: String,
}

/// Parses and validates a manifest.
pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("the file is too large".into());
    }
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    manifest.validate()?;
    Ok(manifest)
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.format > FORMAT {
            return Err(format!(
                "manifest format {} is newer than this program understands ({FORMAT})",
                self.format
            ));
        }
        if self.format == 0 {
            return Err("manifest format 0 is not valid".into());
        }
        if Version::parse(&self.version).is_none() {
            return Err(format!("{:?} is not a valid version", self.version));
        }
        let installer = &self.installer;
        if !(installer.url.starts_with("https://") || installer.url.starts_with("http://")) {
            return Err("the installer URL is not an http(s) URL".into());
        }
        if installer.size == 0 || installer.size > MAX_INSTALLER_BYTES {
            return Err(format!(
                "installer size {} is not plausible",
                installer.size
            ));
        }
        if !is_hex(&installer.sha256, 64) {
            return Err("sha256 must be 64 lowercase hex digits".into());
        }
        if !is_hex(&self.signature, 128) {
            return Err("signature must be 128 lowercase hex digits".into());
        }
        if !self.notes.contains_key(FALLBACK_LANG) {
            return Err(format!("release notes in {FALLBACK_LANG:?} are missing"));
        }
        if let Some((lang, _)) = self
            .notes
            .iter()
            .find(|(_, n)| n.chars().count() > MAX_NOTES_CHARS)
        {
            return Err(format!("the release notes in {lang:?} are too long"));
        }
        Ok(())
    }

    pub fn parsed_version(&self) -> Option<Version> {
        Version::parse(&self.version)
    }

    /// The release notes for a language, or the English ones.
    pub fn notes_for(&self, lang: &str) -> &str {
        self.notes
            .get(lang)
            .or_else(|| self.notes.get(FALLBACK_LANG))
            .map_or("", String::as_str)
    }

    /// The file name the installer is saved under locally (never taken from
    /// the URL, so that a manifest cannot choose an arbitrary name).
    pub fn installer_file_name(&self) -> String {
        let version: String = self
            .version
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
            .collect();
        if cfg!(target_os = "macos") {
            format!("Dodavo-{version}-macos.dmg")
        } else {
            format!("Dodavo-{version}-setup.exe")
        }
    }
}

pub fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Used by the release tool and tests.
#[allow(dead_code)]
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
pub fn example() -> Manifest {
    Manifest {
        format: FORMAT,
        version: "1.1.0".into(),
        published: Some("2026-11-20".into()),
        installer: Installer {
            url: "https://github.com/hkatic/dodavo/releases/download/v1.1.0/Dodavo-1.1.0-setup.exe"
                .into(),
            size: 21_345_678,
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        },
        signature: "0".repeat(128),
        notes: [
            (
                "en".to_string(),
                "What's new\n\n- Automatic updates".to_string(),
            ),
            (
                "hr".to_string(),
                "Što je novo\n\n- Automatska ažuriranja".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_fallback() {
        let m = example();
        let json = serde_json::to_vec_pretty(&m).unwrap();
        let parsed = parse(&json).unwrap();
        assert_eq!(parsed, m);
        assert_eq!(
            parsed.notes_for("hr"),
            "Što je novo\n\n- Automatska ažuriranja"
        );
        assert_eq!(parsed.notes_for("sl"), "What's new\n\n- Automatic updates");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(parsed.installer_file_name(), "Dodavo-1.1.0-setup.exe");
        #[cfg(target_os = "macos")]
        assert_eq!(parsed.installer_file_name(), "Dodavo-1.1.0-macos.dmg");
        assert_eq!(parsed.parsed_version().unwrap().to_string(), "1.1.0");
        // `published` is optional.
        let json = br#"{"format":1,"version":"1.0.0","installer":{"url":"https://x/y","size":5,"sha256":"ab"},"signature":"cd","notes":{"en":"n"}}"#;
        let json = String::from_utf8_lossy(json)
            .replace("\"ab\"", &format!("\"{}\"", "a".repeat(64)))
            .replace("\"cd\"", &format!("\"{}\"", "c".repeat(128)));
        assert!(parse(json.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_bad_manifests() {
        let mut m = example();
        m.format = FORMAT + 1;
        assert!(m.validate().unwrap_err().contains("newer"));
        let mut m = example();
        m.version = "1.0".into();
        assert!(m.validate().is_err());
        let mut m = example();
        m.installer.sha256 = m.installer.sha256.to_uppercase();
        assert!(m.validate().is_err());
        let mut m = example();
        m.installer.size = 0;
        assert!(m.validate().is_err());
        let mut m = example();
        m.installer.size = MAX_INSTALLER_BYTES + 1;
        assert!(m.validate().is_err());
        let mut m = example();
        m.installer.url = "ftp://example.com/x".into();
        assert!(m.validate().is_err());
        let mut m = example();
        m.notes.remove("en");
        assert!(m.validate().is_err());
        let mut m = example();
        m.notes.insert("en".into(), "x".repeat(MAX_NOTES_CHARS + 1));
        assert!(m.validate().is_err());
        assert!(parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
        assert!(parse(b"not json").is_err());
        let mut m = example();
        m.version = "1.1.0/../x".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn hex() {
        assert_eq!(to_hex(&[0, 1, 254, 255]), "0001feff");
        assert_eq!(from_hex("0001FEff"), Some(vec![0, 1, 254, 255]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        assert!(is_hex("00ff", 4));
        assert!(!is_hex("00FF", 4));
        assert!(!is_hex("00ff", 6));
    }
}
