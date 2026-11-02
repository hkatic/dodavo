//! The permanent identity of this installation. The public half of the key is
//! the "Contact ID" other people use to send us files.

use std::{io::ErrorKind, path::Path};

use anyhow::{Context, anyhow};
use iroh::SecretKey;

use crate::i18n::{tr, trn};

/// Loads the secret key, creating one if there is none yet. An existing key
/// file that cannot be read or has the wrong size is an error: replacing it
/// would silently give the user a new Contact ID.
pub fn load_or_create(dir: &Path) -> anyhow::Result<SecretKey> {
    let path = dir.join("secret.key");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let bytes = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
                anyhow!(tr!(identity_damaged, size = trn!(bytes, bytes.len() as u64)))
            })?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let key = SecretKey::generate();
            std::fs::create_dir_all(dir)?;
            write_secret(&path, &key.to_bytes())?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| tr!(error_cannot_read, path = path.display())),
    }
}

#[cfg(windows)]
fn write_secret(path: &Path, key: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, key)
}

/// Readable by this user only.
#[cfg(unix)]
fn write_secret(path: &Path, key: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(key)
}

/// Parses a Contact ID typed or pasted by the user (whitespace is ignored).
pub fn parse_contact_id(text: &str) -> Option<iroh::EndpointId> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    cleaned.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dodavo-id-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn key_is_created_once_and_kept() {
        let dir = scratch("keep");
        let first = load_or_create(&dir).unwrap();
        let again = load_or_create(&dir).unwrap();
        assert_eq!(first.public(), again.public());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn damaged_key_is_not_replaced() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.key");
        std::fs::write(&path, [7u8; 10]).unwrap();
        assert!(load_or_create(&dir).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [7u8; 10]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
