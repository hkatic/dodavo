//! Checks of the Android help pages (`res/help/android`), which the app shows
//! in a WebView, as `dodavo-ffi` has for those of the Mac: they follow WCAG
//! 2.2, have the same structure in every language, say what the program says,
//! and only what the Android version does (no Donate, no updates of its own).

use std::collections::BTreeSet;

use dodavo_core::i18n::{Lang, android};

fn page(lang: Lang) -> &'static str {
    match lang {
        Lang::En => include_str!("../../../res/help/android/en.html"),
        Lang::Hr => include_str!("../../../res/help/android/hr.html"),
        Lang::SrLatn => include_str!("../../../res/help/android/sr-Latn.html"),
        Lang::Sl => include_str!("../../../res/help/android/sl.html"),
    }
}

fn attributes<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let marker = format!(" {attr}=\"");
    html.match_indices(&marker)
        .map(|(at, _)| {
            let rest = &html[at + marker.len()..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect()
}

fn heading_levels(html: &str) -> Vec<u8> {
    html.match_indices("<h")
        .filter_map(|(at, _)| html[at + 2..].chars().next()?.to_digit(10))
        .map(|level| level as u8)
        .collect()
}

/// What a page shows: without tags, as the texts of the program are written.
fn visible(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[test]
fn pages_are_well_formed() {
    for lang in Lang::ALL {
        let html = page(lang);
        let name = lang.code();
        assert!(html.starts_with("<!DOCTYPE html>"), "{name}: doctype");
        assert!(
            html.contains(&format!("<html lang=\"{}\">", lang.code())),
            "{name}: the page language must match"
        );
        assert!(html.contains("<meta charset=\"utf-8\">"), "{name}: charset");
        assert!(
            html.contains("<meta name=\"viewport\""),
            "{name}: a phone needs the viewport"
        );
        assert!(html.contains("<title>"), "{name}: title");
        assert!(html.contains("<main id=\"main\""), "{name}: main landmark");
        assert!(html.contains("href=\"#main\""), "{name}: skip link");
        let ids = attributes(html, "id");
        let unique: BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "{name}: ids must be unique");
        for href in attributes(html, "href") {
            if let Some(target) = href.strip_prefix('#') {
                assert!(ids.contains(&target), "{name}: link to missing #{target}");
            } else {
                assert_eq!(href, "help.css", "{name}: the page links only to its style");
            }
        }
        let levels = heading_levels(html);
        assert_eq!(levels.first(), Some(&1), "{name}: starts with <h1>");
        assert_eq!(
            levels.iter().filter(|&&l| l == 1).count(),
            1,
            "{name}: one <h1>"
        );
        for pair in levels.windows(2) {
            assert!(pair[1] <= pair[0] + 1, "{name}: heading levels skip");
        }
        for tag in [
            "p", "ol", "ul", "li", "section", "table", "tr", "td", "th", "kbd", "code", "strong",
            "a",
        ] {
            let open = html.matches(&format!("<{tag}>")).count()
                + html.matches(&format!("<{tag} ")).count();
            let close = html.matches(&format!("</{tag}>")).count();
            assert_eq!(open, close, "{name}: <{tag}> is not balanced");
        }
    }
}

#[test]
fn pages_have_the_same_structure() {
    let en = page(Lang::En);
    for lang in Lang::ALL {
        let html = page(lang);
        assert_eq!(
            attributes(html, "id"),
            attributes(en, "id"),
            "{lang:?}: ids"
        );
        assert_eq!(
            heading_levels(html),
            heading_levels(en),
            "{lang:?}: headings"
        );
    }
}

/// The contents and the links name their headings.
#[test]
fn links_name_their_headings() {
    for lang in Lang::ALL {
        let html = page(lang);
        for (at, _) in html.match_indices("<li><a href=\"#") {
            let rest = &html[at + "<li><a href=\"#".len()..];
            let (target, rest) = rest.split_once("\">").unwrap();
            let (text, _) = rest.split_once("</a>").unwrap();
            let heading = format!("<h2 id=\"{target}\">{text}</h2>");
            assert!(html.contains(&heading), "{lang:?}: {target}: {text}");
        }
    }
}

/// The pages quote the program, in each language.
#[test]
fn pages_use_the_program_texts() {
    for lang in Lang::ALL {
        let t = android::strings(lang);
        let page = visible(page(lang));
        for (key, text) in [
            ("nav_recipients", t.nav_recipients),
            ("nav_transfers", t.nav_transfers),
            ("more_options", t.more_options),
            ("menu_share_my_id", t.menu_share_my_id),
            ("menu_copy_my_id", t.menu_copy_my_id),
            ("menu_settings", t.menu_settings),
            ("menu_help", t.menu_help),
            ("menu_about", t.menu_about),
            ("action_send_files", t.action_send_files),
            ("action_send_folder", t.action_send_folder),
            ("action_edit", t.action_edit),
            ("action_copy_id", t.action_copy_id),
            ("action_block", t.action_block),
            ("action_remove", t.action_remove),
            ("add_recipient", t.add_recipient),
            ("paste", t.paste),
            ("save", t.save),
            ("block", t.block),
            ("clear_finished", t.clear_finished),
            ("pause", t.pause),
            ("resume", t.resume),
            ("cancel_transfer", t.cancel_transfer),
            ("open", t.open),
            ("share", t.share),
            ("show_folder", t.show_folder),
            ("remove_from_list", t.remove_from_list),
            ("incoming_title", t.incoming_title),
            ("accept", t.accept),
            ("decline", t.decline),
            ("review", t.review),
            ("block_sender", t.block_sender),
            ("settings_title", t.settings_title),
            ("my_name_label", t.my_name_label),
            ("my_name_help", t.my_name_help),
            ("my_id_label", t.my_id_label),
            ("receive_background", t.receive_background),
            ("notify_ready", t.notify_ready),
            ("auto_accept", t.auto_accept),
            ("auto_accept_help", t.auto_accept_help),
            ("received_folder_label", t.received_folder_label),
            ("received_extract", t.received_extract),
            ("received_zip", t.received_zip),
            ("received_both", t.received_both),
            ("blocked_senders", t.blocked_senders),
            ("unblock", t.unblock),
            ("language_setting", t.language_setting),
            ("notifications_setting", t.notifications_setting),
            ("about_title", t.about_title),
            ("licenses", t.licenses),
            ("item_unreadable", t.item_unreadable),
        ] {
            assert!(page.contains(text), "{lang:?}: {key}: {text}");
        }
        // Where received items go, as the Incoming screen says.
        assert!(page.contains("Download/Dodavo"), "{lang:?}: Download/Dodavo");
    }
}

/// A manual says what the program needs and does. What was tested, and how it
/// was made, belongs in `TODO.md`. The Android version has no Donate (Google
/// Play's rules for payments) and is updated only by Google Play.
#[test]
fn the_pages_keep_to_what_the_program_does() {
    for lang in Lang::ALL {
        let page = visible(page(lang)).to_lowercase();
        for word in [
            "tested",
            "testing",
            "as is",
            "testir",
            "takav kakav",
            "preizku",
            "takšen, kot je",
            // Donate and its translations.
            "donat",
            "donir",
            "paypal",
            // An updater of its own.
            "update",
            "ažurir",
            "posodob",
        ] {
            assert!(!page.contains(word), "{lang:?}: {word}");
        }
    }
}

/// Nothing of Windows or the Mac in the pages, except where it is the other
/// side.
#[test]
fn the_pages_are_about_android() {
    for lang in Lang::ALL {
        let page = visible(page(lang));
        for word in [
            "F1",
            "Ctrl+",
            "Explorer",
            "SmartScreen",
            "Setup",
            "%APPDATA%",
            "Finder",
            "Dock",
            "VoiceOver",
            "⌘",
            "~/Library",
        ] {
            assert!(!page.contains(word), "{lang:?}: {word}");
        }
        let lower = page.to_lowercase();
        for word in ["click", "klikn"] {
            assert!(!lower.contains(word), "{lang:?}: {word} (on Android, one taps)");
        }
    }
}
