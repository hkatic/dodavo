//! What a send reads: a file or folder by its path, or on Android a document
//! that the user chose in the system's pickers (a `content://` URI), which
//! only the app can open (see [`set_opener`]). A send of a document reads it
//! where it is: a file for as long as the transfer lasts, also after a
//! restart (the app keeps its permission until the core says it is no longer
//! needed), and a folder only while it is packed, from what the app lists of
//! it ([`Document::items`]).

use std::{
    fs::{File, Metadata},
    io,
    path::Path,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

/// A document that the user chose, as the app describes it: the core cannot
/// ask the system about it itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Its URI, which [`open`] opens: the path of what a send reads.
    pub uri: String,
    /// The name that it shows.
    pub name: String,
    /// When it was last changed, in milliseconds since 1970, if known.
    #[serde(default)]
    pub modified: Option<i64>,
    /// For a folder, what it holds, each folder before what is in it;
    /// `None` for a file.
    #[serde(default)]
    pub items: Option<Vec<Item>>,
}

/// A file or folder in a [`Document`] that is a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Its names from the folder down: `["Photos", "a.jpg"]`.
    pub path: Vec<String>,
    #[serde(default)]
    pub folder: bool,
    #[serde(default)]
    pub size: u64,
    /// When it was last changed, in milliseconds since 1970, if known.
    #[serde(default)]
    pub modified: Option<i64>,
    /// What [`open`] opens to read a file.
    #[serde(default)]
    pub uri: String,
}

/// A time in milliseconds since 1970 as a [`SystemTime`].
pub fn millis_to_system(millis: i64) -> Option<SystemTime> {
    let millis = u64::try_from(millis).ok().filter(|&m| m > 0)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}

/// Whether `path` is a document that only the app can open, rather than a
/// path of the file system.
pub fn is_document(path: &Path) -> bool {
    cfg!(target_os = "android") && path.to_str().is_some_and(|p| p.starts_with("content://"))
}

/// Opens what `path` names for reading.
pub fn open(path: &Path) -> io::Result<File> {
    #[cfg(target_os = "android")]
    if is_document(path) {
        let opener = OPENER.get().ok_or(io::ErrorKind::Unsupported)?;
        return opener(path.to_str().unwrap_or_default());
    }
    File::open(path)
}

/// The size and dates of what `path` names (not of where a link leads).
pub fn metadata(path: &Path) -> io::Result<Metadata> {
    if is_document(path) {
        open(path)?.metadata()
    } else {
        std::fs::metadata(path)
    }
}

/// Whether what `path` names is there, as far as this program can tell.
pub fn exists(path: &Path) -> bool {
    if is_document(path) {
        open(path).is_ok()
    } else {
        path.exists()
    }
}

#[cfg(target_os = "android")]
type Opener = Box<dyn Fn(&str) -> io::Result<File> + Send + Sync>;

#[cfg(target_os = "android")]
static OPENER: std::sync::OnceLock<Opener> = std::sync::OnceLock::new();

/// How documents are opened: through the app, which alone may. Once, before
/// the network starts.
#[cfg(target_os = "android")]
pub fn set_opener(opener: impl Fn(&str) -> io::Result<File> + Send + Sync + 'static) {
    let _ = OPENER.set(Box::new(opener));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_not_documents_off_android() {
        assert!(!is_document(Path::new("/tmp/a.txt")));
        assert_eq!(
            is_document(Path::new("content://com.android.providers.media.documents/document/1")),
            cfg!(target_os = "android")
        );
    }

    #[test]
    fn times_in_milliseconds() {
        assert_eq!(
            millis_to_system(1_500),
            Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_500))
        );
        assert_eq!(millis_to_system(0), None);
        assert_eq!(millis_to_system(-5), None);
    }

    #[test]
    fn a_document_reads_as_json() {
        let document: Document = serde_json::from_str(
            r#"{"uri":"content://x/tree/1","name":"Photos","modified":1000,
                "items":[{"path":["a"],"folder":true},{"path":["a","b.jpg"],"size":3,"uri":"content://x/document/2"}]}"#,
        )
        .unwrap();
        let items = document.items.unwrap();
        assert!(items[0].folder);
        assert_eq!(items[1].path, ["a", "b.jpg"]);
        assert_eq!(items[1].modified, None);
    }
}
