//! Checks of the Mac help pages (`res/help/macos`), as `ui::help` has for those
//! of Windows: they follow WCAG 2.2, have the same structure in every language,
//! and say what the program says.

use std::collections::BTreeSet;

use dodavo_core::i18n::{Lang, mac};

fn page(lang: Lang) -> &'static str {
    match lang {
        Lang::En => include_str!("../../../res/help/macos/en.html"),
        Lang::Hr => include_str!("../../../res/help/macos/hr.html"),
        Lang::SrLatn => include_str!("../../../res/help/macos/sr-Latn.html"),
        Lang::Sl => include_str!("../../../res/help/macos/sl.html"),
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
        assert!(html.contains("<title>"), "{name}: title");
        assert!(html.contains("<main id=\"main\""), "{name}: main landmark");
        assert!(html.contains("href=\"#main\""), "{name}: skip link");
        let ids = attributes(html, "id");
        let unique: BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "{name}: ids must be unique");
        for href in attributes(html, "href") {
            if let Some(target) = href.strip_prefix('#') {
                assert!(ids.contains(&target), "{name}: link to missing #{target}");
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
        let table = mac::strings(lang);
        let page = visible(page(lang));
        for (key, text) in [
            ("auto_accept_note", table.auto_accept_note),
            ("menu_send", table.menu_send),
            ("menu_add_recipient", table.menu_add_recipient),
            ("menu_edit_recipient", table.menu_edit_recipient),
            ("menu_remove_recipient", table.menu_remove_recipient),
            ("menu_block_recipient", table.menu_block_recipient),
            ("block_sender", table.block_sender),
            ("settings_blocked", table.settings_blocked),
            ("unblock", table.unblock),
            ("menu_copy_id", table.menu_copy_id),
            ("menu_change_id", table.menu_change_id),
            ("change_id_button", table.change_id_button),
            ("menu_show_transfers", table.menu_show_transfers),
            ("menu_quit", table.menu_quit),
            ("menu_settings", table.menu_settings),
            ("menu_preferences", table.menu_preferences),
            ("menu_check_updates", table.menu_check_updates),
            ("menu_license", table.menu_license),
            ("menu_donate", table.menu_donate),
            ("col_name", table.col_name),
            ("col_contact_id", table.col_contact_id),
            ("col_transfer", table.col_transfer),
            ("toolbar_send", table.toolbar_send),
            ("transfers_title", table.transfers_title),
            ("clear_finished", table.clear_finished),
            ("cancel_transfer", table.cancel_transfer),
            ("open", table.open),
            ("show_in_finder", table.show_in_finder),
            ("remove_from_list", table.remove_from_list),
            ("incoming_title", table.incoming_title),
            ("save_to", table.save_to),
            ("choose", table.choose),
            ("accept", table.accept),
            ("decline", table.decline),
            ("settings_general", table.settings_general),
            ("settings_receiving", table.settings_receiving),
            ("open_at_login", table.open_at_login),
            ("hide_at_login", table.hide_at_login),
            ("show_in_menu_bar", table.show_in_menu_bar),
            ("check_updates", table.check_updates),
            ("language_system", table.language_system),
            ("always_save", table.always_save),
            ("auto_accept", table.auto_accept),
            ("received_extract", table.received_extract),
            ("received_zip", table.received_zip),
            ("received_both", table.received_both),
            ("move_button", table.move_button),
            ("service_send", table.service_send),
            ("system_settings", table.system_settings),
        ] {
            assert!(page.contains(text), "{lang:?}: {key}: {text}");
        }
        assert!(
            page.contains(dodavo_core::DONATE_URL) || visible(self::page(lang)).contains("PayPal")
        );
    }
}

/// Nothing of Windows in the pages, except where it is the other side.
/// A manual says what the program needs and does. What was tested, and how it was
/// made, belongs in `TODO.md`.
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
            "notariz",
            "ovjeren",
            "overen",
            "overjen",
        ] {
            assert!(!page.contains(word), "{lang:?}: {word}");
        }
    }
}

#[test]
fn the_pages_are_about_the_mac() {
    for lang in Lang::ALL {
        let page = visible(page(lang));
        for word in [
            "F1",
            "Ctrl+",
            "File Explorer",
            "SmartScreen",
            "Setup",
            "taskbar",
            "%APPDATA%",
        ] {
            assert!(!page.contains(word), "{lang:?}: {word}");
        }
    }
}
