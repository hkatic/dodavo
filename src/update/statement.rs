//! What the release signature covers, and how it is checked.
//!
//! The signature is not over the installer itself but over a short statement
//! that names the installer's SHA-256 (and size, version and format). The hash
//! binds the statement to the file and is computed while the file is
//! downloaded, so a 20 MB installer never has to be read twice.
//!
//! This file is also compiled into the release tool (`src/bin/dodavo-release.rs`),
//! so it must not refer to the rest of the program.

use std::{
    io::{self, Read},
    path::Path,
};

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

use super::manifest::{self, Manifest};

pub const STATEMENT_PREFIX: &str = "dodavo-update";

/// The signed text: `dodavo-update\n<format>\n<version>\n<sha256 hex>\n<size>\n`.
pub fn statement(format: u32, version: &str, sha256_hex: &str, size: u64) -> String {
    format!("{STATEMENT_PREFIX}\n{format}\n{version}\n{sha256_hex}\n{size}\n")
}

/// Whether the manifest's signature was made by one of the trusted keys.
pub fn verify(keys: &[VerifyingKey], m: &Manifest) -> bool {
    let Some(bytes) = manifest::from_hex(&m.signature) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&bytes) else {
        return false;
    };
    let text = statement(m.format, &m.version, &m.installer.sha256, m.installer.size);
    keys.iter()
        .any(|key| key.verify_strict(text.as_bytes(), &signature).is_ok())
}

/// A public key from its 64 hex digits.
pub fn parse_public_key(hex: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = manifest::from_hex(hex)?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Used by the release tool and tests; the program hashes while downloading.
#[allow(dead_code)]
pub fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Signs a statement with a secret key; returns the signature as hex.
#[cfg(any(test, feature = "release-tool"))]
pub fn sign(secret: &[u8; 32], text: &str) -> String {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(secret);
    manifest::to_hex(&key.sign(text.as_bytes()).to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn signed(secret: &[u8; 32]) -> Manifest {
        let mut m = manifest::example();
        m.signature = sign(
            secret,
            &statement(m.format, &m.version, &m.installer.sha256, m.installer.size),
        );
        m
    }

    #[test]
    fn signatures_verify_and_tampering_fails() {
        let secret = [7u8; 32];
        let key = SigningKey::from_bytes(&secret).verifying_key();
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
        let m = signed(&secret);
        assert!(m.validate().is_ok());
        assert!(verify(&[key], &m));
        // Any trusted key in the list will do.
        assert!(verify(&[other, key], &m));
        assert!(!verify(&[other], &m));
        assert!(!verify(&[], &m));

        let mut t = m.clone();
        t.installer.size += 1;
        assert!(!verify(&[key], &t));
        let mut t = m.clone();
        t.version = "0.2.1".into();
        assert!(!verify(&[key], &t));
        let mut t = m.clone();
        t.format += 1;
        assert!(!verify(&[key], &t));
        let mut t = m.clone();
        let mut sha = t.installer.sha256.clone();
        sha.replace_range(0..1, if sha.starts_with('e') { "f" } else { "e" });
        t.installer.sha256 = sha;
        assert!(!verify(&[key], &t));
        let mut t = m.clone();
        t.signature = "0".repeat(128);
        assert!(!verify(&[key], &t));
        let mut t = m;
        t.signature = "zz".into();
        assert!(!verify(&[key], &t));
    }

    #[test]
    fn public_keys_parse_from_hex() {
        let key = SigningKey::from_bytes(&[3u8; 32]).verifying_key();
        let hex = manifest::to_hex(&key.to_bytes());
        assert_eq!(parse_public_key(&hex).unwrap(), key);
        assert_eq!(parse_public_key(&format!(" {hex}\n")).unwrap(), key);
        assert!(parse_public_key(&hex[..62]).is_none());
        assert!(parse_public_key("x").is_none());
    }

    #[test]
    fn hashes_files() {
        let path = std::env::temp_dir().join(format!("dodavo-sha-test-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            manifest::to_hex(&sha256_file(&path).unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            manifest::to_hex(&sha256_file(&path).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&path).ok();
    }
}
