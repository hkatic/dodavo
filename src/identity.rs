//! The identity of this installation, which stays the same until the user
//! asks for a new one. The public half of the key is the "Contact ID" other
//! people use to send us files.

use std::{
    io::{ErrorKind, Write},
    path::Path,
};

use anyhow::{Context, anyhow};
use iroh::SecretKey;

use crate::i18n::{tr, trn};

const KEY_FILE: &str = "secret.key";
/// A key on its way to replace the current one (see [`replace`]).
const NEW_KEY_FILE: &str = "secret.key.new";

/// Loads the secret key, creating one if there is none yet. An existing key
/// file that cannot be read or has the wrong size is an error: replacing it
/// would silently give the user a new Contact ID.
pub fn load_or_create(dir: &Path) -> anyhow::Result<SecretKey> {
    let path = dir.join(KEY_FILE);
    // What a change of the key left behind when it did not get to its
    // rename: the key in use is still the one in `secret.key`.
    let _ = std::fs::remove_file(dir.join(NEW_KEY_FILE));
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
            write_new_secret(&path, &key.to_bytes())?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| tr!(error_cannot_read, path = path.display())),
    }
}

/// A new key, for [`replace`].
pub fn new_key() -> SecretKey {
    SecretKey::generate()
}

/// Makes `key` the key that the next [`load_or_create`] returns, which gives
/// the user a new Contact ID. The key is written beside the current one and
/// renamed over it once it is on the disk, so that a crash or a failure
/// leaves one whole key, the old or the new, and never a damaged file, which
/// the next start would refuse.
pub fn replace(dir: &Path, key: &SecretKey) -> std::io::Result<()> {
    let new = dir.join(NEW_KEY_FILE);
    let _ = std::fs::remove_file(&new);
    let written = write_new_secret(&new, &key.to_bytes())
        .and_then(|()| std::fs::rename(&new, dir.join(KEY_FILE)));
    if written.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    written
}

/// Writes `key` to `path`, which must not exist yet, and waits until it is
/// on the disk.
#[cfg(windows)]
fn write_new_secret(path: &Path, key: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(key)?;
    file.sync_all()
}

/// Readable by this user only.
#[cfg(unix)]
fn write_new_secret(path: &Path, key: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(key)?;
    file.sync_all()
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
    fn a_replaced_key_is_the_one_that_loads() {
        let dir = scratch("replace");
        let first = load_or_create(&dir).unwrap();
        let new = SecretKey::generate();
        replace(&dir, &new).unwrap();
        assert!(!dir.join(NEW_KEY_FILE).exists());
        let loaded = load_or_create(&dir).unwrap();
        assert_eq!(loaded.public(), new.public());
        assert_ne!(loaded.public(), first.public());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_change_that_did_not_finish_keeps_the_key() {
        let dir = scratch("unfinished");
        let first = load_or_create(&dir).unwrap();
        // As a crash before the rename leaves it.
        std::fs::write(dir.join(NEW_KEY_FILE), [7u8; 32]).unwrap();
        let again = load_or_create(&dir).unwrap();
        assert_eq!(first.public(), again.public());
        assert!(!dir.join(NEW_KEY_FILE).exists());
        // And a leftover does not stand in the way of the next change.
        std::fs::write(dir.join(NEW_KEY_FILE), [7u8; 32]).unwrap();
        let new = SecretKey::generate();
        replace(&dir, &new).unwrap();
        assert_eq!(load_or_create(&dir).unwrap().public(), new.public());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_key_cannot_replace_one_in_a_folder_that_is_gone() {
        let dir = scratch("gone");
        assert!(replace(&dir, &SecretKey::generate()).is_err());
        assert!(!dir.exists());
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
