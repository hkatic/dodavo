//! The texts of the Android user interface. Those of the transfers are in the
//! table of `i18n`, with what differs on Android here under the same name (see
//! [`android_tr`](crate::android_tr)). Written with Google's terms for each
//! language: what Android itself calls things in its own translations.
//! Linux uses this table too, so that its tests go through these texts.

use super::{Lang, Plural};

mod en;
mod hr;
mod sl;
mod sr_latn;

super::strings! {
    Strings;
    texts {
        // What the transfers say where the system has its own words.
        space_ok,
        space_short,
        space_fat,
        tx_part_in_use,
        tx_peer_part_in_use,
        phase_part_in_use,
        phase_peer_part_in_use,
        part_in_use_balloon,
        peer_part_in_use_balloon,
        // The system's errors.
        error_no_access,
        error_name_too_long,
        error_read_only,
        error_system,
    }
    plurals {}
}

/// The table of `lang`.
pub fn strings(lang: Lang) -> &'static Strings {
    match lang {
        Lang::En => &en::EN,
        Lang::Hr => &hr::HR,
        Lang::SrLatn => &sr_latn::SR_LATN,
        Lang::Sl => &sl::SL,
    }
}

/// The texts of the current language.
pub fn s() -> &'static Strings {
    strings(super::lang())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(text: &str) -> BTreeSet<String> {
        let mut set = BTreeSet::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else { break };
            set.insert(after[..end].to_string());
            rest = &after[end + 1..];
        }
        set
    }

    #[test]
    fn tables_match_english() {
        let en = strings(Lang::En);
        for lang in Lang::ALL {
            let table = strings(lang);
            assert_eq!(table.lang, lang);
            for ((key, text), (_, en_text)) in table.texts().into_iter().zip(en.texts()) {
                assert!(!text.is_empty(), "{lang:?}: {key} is empty");
                assert_eq!(placeholders(text), placeholders(en_text), "{lang:?}: {key}");
            }
            for ((key, plural), (_, en_plural)) in table.plurals().into_iter().zip(en.plurals()) {
                for (form, en_form) in [
                    (plural.one, en_plural.one),
                    (plural.two, en_plural.two),
                    (plural.few, en_plural.few),
                    (plural.other, en_plural.other),
                ] {
                    assert!(!form.is_empty(), "{lang:?}: {key} is empty");
                    assert_eq!(placeholders(form), placeholders(en_form), "{lang:?}: {key}");
                }
            }
        }
    }

    /// A text that stands in for one of the main table (`os_tr!`) has only
    /// placeholders that the code fills there: `fill` keeps unknown ones.
    #[test]
    fn texts_of_the_main_table_take_its_placeholders() {
        for lang in Lang::ALL {
            let main: std::collections::HashMap<_, _> = lang.strings().texts().into_iter().collect();
            for (key, text) in strings(lang).texts() {
                if let Some(main_text) = main.get(key) {
                    assert!(
                        placeholders(text).is_subset(&placeholders(main_text)),
                        "{lang:?}: {key}"
                    );
                }
            }
        }
    }

    /// Nothing of Windows or the Mac, nor their access keys.
    #[test]
    fn no_other_systems_in_the_android_texts() {
        for lang in Lang::ALL {
            for (key, text) in strings(lang).texts() {
                assert!(!text.contains(['&', '\t']), "{lang:?}: {key}");
                for word in [
                    "Windows", "Explorer", "Finder", "macOS", "Mac", "program", "drive", "disk",
                ] {
                    assert!(!text.contains(word), "{lang:?}: {key}: {word}");
                }
            }
        }
    }
}
