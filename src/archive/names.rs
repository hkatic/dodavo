//! What a folder's zip calls its items, when the sender's file system allows
//! more than Windows does.
//!
//! A released Windows receiver stops at the first entry whose name Windows
//! cannot take (`is_plain_name`), after all the data has arrived, and the
//! sender cannot tell what system the other side has. So a sender on another
//! system writes only names that Windows can take, unique when case does not
//! count, and leaves out what only means something to macOS. What that loses
//! travels in a private extra field of the zip, which Windows ignores: another
//! Mac gets the item's own name and the links back.

use serde::{Deserialize, Serialize};

use super::{is_plain_name, numbered, sanitize_file_name};

/// The private extra field. Not one of those in the zip crate's
/// `EXTRA_FIELD_MAPPING`, which it would try to parse.
pub(super) const EXTRA_FIELD: u16 = 0x6464;

/// The room for that field, well below what a zip entry's header can hold
/// with its name and other fields (64 KiB).
const MAX_FIELD: usize = 60_000;

/// What the name in the zip does not say of an item.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Fidelity {
    /// The item's own name, where the zip calls it something else.
    pub name: Option<String>,
    /// The links in a folder (on its entry): name and what it points to.
    pub links: Vec<(String, String)>,
}

impl Fidelity {
    /// The field's data, as far as it fits: a version, then the values.
    pub fn encode(mut self) -> Option<Vec<u8>> {
        if self.name.is_none() && self.links.is_empty() {
            return None;
        }
        loop {
            let values = postcard::to_stdvec(&self).ok()?;
            if values.len() < MAX_FIELD {
                let mut data = vec![1];
                data.extend(values);
                return Some(data);
            }
            // Only so many links fit; better a folder without some of them
            // than none at all.
            self.links.pop()?;
        }
    }

    pub fn decode(data: &[u8]) -> Option<Fidelity> {
        match data.split_first() {
            Some((1, values)) => postcard::from_bytes(values).ok(),
            _ => None,
        }
    }
}

/// `name` in the composed form that Windows and other systems expect ("ć" as
/// one character). HFS+, and files that came from it, keep names decomposed.
pub(super) fn composed(name: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(name)
        .into_owned()
}

/// `name` cut to at most `max` of what `measure` counts, from the end of the
/// part before its extension.
pub(super) fn cut_to(name: &str, max: usize, measure: fn(&str) -> usize) -> String {
    if measure(name) <= max {
        return name.to_string();
    }
    let (stem, extension) = match name.rfind('.') {
        Some(at) if at > 0 && name.len() - at <= 32 => name.split_at(at),
        _ => (name, ""),
    };
    let mut stem = stem.to_string();
    while !stem.is_empty() && measure(&stem) + measure(extension) > max {
        stem.pop();
    }
    format!("{stem}{extension}")
}

/// Counts what Windows limits a name to.
pub(super) fn utf16_units(name: &str) -> usize {
    name.encode_utf16().count()
}

/// Counts what APFS and Linux file systems limit a name to.
#[cfg(any(unix, test))]
pub(super) fn bytes(name: &str) -> usize {
    name.len()
}

/// `name` as Windows can take it, with room for a number (see [`numbered`]).
fn plain(name: &str) -> String {
    cut_to(&sanitize_file_name(&composed(name)), 245, utf16_units)
}

/// The name of a sent item, in the offer, as [`plain`], with room for what
/// the receiver adds (`.zip.part`).
pub fn offer_name(name: &str) -> String {
    cut_to(&sanitize_file_name(&composed(name)), 240, utf16_units)
}

/// What only means something to macOS, and is not sent: Finder's settings and
/// custom icons, the attributes macOS keeps beside files on FAT and exFAT, and
/// the folders of its services at the root of a volume.
pub(super) fn is_system_item(name: &str) -> bool {
    name.starts_with("._")
        || matches!(
            name,
            ".DS_Store"
                | "Icon\r"
                | ".fseventsd"
                | ".Spotlight-V100"
                | ".Trashes"
                | ".TemporaryItems"
                | ".DocumentRevisions-V100"
                | ".apdisk"
                | ".VolumeIcon.icns"
        )
}

/// Gives the items of a folder names that are unique once case does not count.
#[derive(Default)]
pub(super) struct Namer {
    taken: std::collections::HashSet<String>,
}

impl Namer {
    /// The name in the zip of the item `name` in the folder `parent`, a name
    /// in the zip ending with "/", or empty for the top.
    pub fn name(&mut self, parent: &str, name: &str, is_folder: bool) -> String {
        let plain = plain(name);
        for candidate in numbered(&plain, is_folder) {
            let key = format!("{parent}{}", candidate.to_lowercase());
            if is_plain_name(&candidate) && self.taken.insert(key) {
                return candidate;
            }
        }
        unreachable!("infinite iterator")
    }
}

/// Whether a link in the folder `parent` (a path inside the sent folder, with
/// this many levels) pointing to `target` stays inside the sent folder. An
/// absolute target, or one that climbs out, means nothing on the other side
/// and must not reach anything there.
pub(super) fn link_stays_inside(levels: usize, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') || target.contains('\0') {
        return false;
    }
    let mut depth = levels;
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => match depth.checked_sub(1) {
                Some(up) => depth = up,
                None => return false,
            },
            _ => depth += 1,
        }
    }
    true
}

/// Whether `name` can be a single name on macOS (and Linux) as it is. A name
/// from a peer must pass this before it is used: it decides where a file goes.
#[cfg(any(unix, test))]
pub(super) fn is_mac_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_windows_cannot_take_are_changed() {
        let mut namer = Namer::default();
        let mut name = |n: &str| namer.name("", n, false);
        assert_eq!(name("Račun.pdf"), "Račun.pdf");
        assert_eq!(name("a:b?.txt"), "a_b_.txt");
        assert_eq!(name("CON.txt"), "_CON.txt");
        assert_eq!(name("trailing."), "trailing");
        assert_eq!(name("back\\slash"), "back_slash");
        assert_eq!(name("Icon\r"), "Icon_");
        for hostile in ["a:b?.txt", "CON.txt", "trailing.", "x y ", "<>|*", "..."] {
            assert!(is_plain_name(&name(hostile)), "{hostile}");
        }
    }

    #[test]
    fn decomposed_names_are_composed() {
        let decomposed = "Kuc\u{301}a.jpg";
        assert_eq!(composed(decomposed), "Kuća.jpg");
        assert_eq!(Namer::default().name("", decomposed, false), "Kuća.jpg");
    }

    /// Names that are the same when case does not count, or once changed,
    /// get numbers, in each folder of their own.
    #[test]
    fn names_stay_unique() {
        let mut namer = Namer::default();
        assert_eq!(namer.name("", "Readme.txt", false), "Readme.txt");
        assert_eq!(namer.name("", "README.txt", false), "README (2).txt");
        assert_eq!(namer.name("", "a:b", false), "a_b");
        assert_eq!(namer.name("", "a?b", false), "a_b (2)");
        assert_eq!(namer.name("sub/", "Readme.txt", false), "Readme.txt");
        assert_eq!(namer.name("", "Folder", true), "Folder");
        assert_eq!(namer.name("", "folder", true), "folder (2)");
    }

    #[test]
    fn long_names_are_cut_before_the_extension() {
        let long = format!("{}.txt", "č".repeat(300));
        let name = Namer::default().name("", &long, false);
        assert!(is_plain_name(&name));
        assert!(name.ends_with(".txt"));
        assert!(utf16_units(&name) <= 245);
        // In bytes, as APFS counts, without splitting a character.
        let cut = cut_to(&long, 245, bytes);
        assert!(cut.len() <= 245 && cut.ends_with(".txt"));
        let name = offer_name(&long);
        assert!(utf16_units(&name) <= 240 && name.ends_with(".txt"));
    }

    #[test]
    fn system_items_are_told() {
        for name in [".DS_Store", "._a.txt", "Icon\r", ".Spotlight-V100"] {
            assert!(is_system_item(name), "{name:?}");
        }
        for name in [".gitignore", "Icon", "a.txt", ".hidden"] {
            assert!(!is_system_item(name), "{name:?}");
        }
    }

    #[test]
    fn links_stay_inside_the_folder() {
        assert!(link_stays_inside(0, "a.txt"));
        assert!(link_stays_inside(1, "../a.txt"));
        assert!(link_stays_inside(2, "../../b/c"));
        assert!(link_stays_inside(0, "a/../b"));
        assert!(!link_stays_inside(0, "../a.txt"));
        assert!(!link_stays_inside(1, "../../a.txt"));
        assert!(!link_stays_inside(0, "a/../../b"));
        assert!(!link_stays_inside(3, "/etc/passwd"));
        assert!(!link_stays_inside(0, ""));
    }

    #[test]
    fn the_field_goes_there_and_back() {
        let fidelity = Fidelity {
            name: Some("a:b?".into()),
            links: vec![("l".into(), "a.txt".into())],
        };
        let data = fidelity.clone().encode().unwrap();
        assert_eq!(Fidelity::decode(&data), Some(fidelity));
        assert_eq!(Fidelity::default().encode(), None);
        assert_eq!(Fidelity::decode(&[]), None);
        assert_eq!(Fidelity::decode(&[99, 0]), None, "a later version");
        assert_eq!(Fidelity::decode(&[1, 0xff, 0xff]), None);
        // Too many links for one entry: as many as fit.
        let many = Fidelity {
            name: None,
            links: (0..10_000)
                .map(|i| (format!("link{i}"), format!("target{i}")))
                .collect(),
        };
        let data = many.encode().unwrap();
        assert!(data.len() <= MAX_FIELD);
        assert!(!Fidelity::decode(&data).unwrap().links.is_empty());
    }

    #[test]
    fn names_from_a_peer_are_checked() {
        assert!(is_mac_name("a:b?.txt"));
        assert!(is_mac_name("back\\slash"));
        for bad in ["", ".", "..", "a/b", "/etc", "a\0b"] {
            assert!(!is_mac_name(bad), "{bad:?}");
        }
        assert!(!is_mac_name(&"x".repeat(256)));
    }
}
