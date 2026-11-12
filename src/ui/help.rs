//! The help: one HTML page per language (`res/help/`), built into the program
//! and opened in the default web browser in the language of the user interface.

use std::{io, path::PathBuf};

use windows_sys::Win32::Foundation::HWND;

use super::{error, shell};
use crate::{
    archive,
    i18n::{self, Lang, tr},
};

const STYLE: &str = include_str!("../../res/help/help.css");

fn page(lang: Lang) -> &'static str {
    match lang {
        Lang::En => include_str!("../../res/help/en.html"),
        Lang::Hr => include_str!("../../res/help/hr.html"),
        Lang::SrLatn => include_str!("../../res/help/sr-Latn.html"),
        Lang::Sl => include_str!("../../res/help/sl.html"),
    }
}

/// Writes the page of the current language and its style sheet to the
/// temporary folder (every time, so they match this version of the program).
fn write_page() -> io::Result<PathBuf> {
    let dir = archive::temp_dir()?.join("help");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("help.css"), STYLE)?;
    let lang = i18n::lang();
    let path = dir.join(format!("{}.html", lang.code()));
    std::fs::write(&path, page(lang))?;
    Ok(path)
}

/// Opens the help in the default web browser.
pub fn show(owner: HWND) {
    let result = write_page().and_then(|path| {
        if shell::open_path(&path) {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    });
    if let Err(e) = result {
        error(owner, &tr!(help_failed, error = e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The values of `attr="…"` in `html`.
    fn attributes<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
        let pattern = format!(" {attr}=\"");
        html.match_indices(&pattern)
            .map(|(i, _)| {
                let rest = &html[i + pattern.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect()
    }

    /// The levels of the `<h1>`…`<h6>` elements, in document order.
    fn heading_levels(html: &str) -> Vec<u8> {
        html.match_indices("<h")
            .filter_map(|(i, _)| {
                let b = html.as_bytes();
                let level = b.get(i + 2)?;
                let after = b.get(i + 3)?;
                ((b'1'..=b'6').contains(level) && matches!(after, b'>' | b' '))
                    .then(|| level - b'0')
            })
            .collect()
    }

    /// A menu or button text as it reads on screen, without the access key
    /// marker, the shortcut and a trailing ellipsis.
    fn visible(text: &str) -> String {
        crate::ui::without_shortcut(text)
            .replace('&', "")
            .trim_end_matches('…')
            .to_string()
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
            for label in attributes(html, "aria-labelledby") {
                assert!(ids.contains(&label), "{name}: label #{label} is missing");
            }

            let levels = heading_levels(html);
            assert_eq!(levels.first(), Some(&1), "{name}: starts with <h1>");
            assert_eq!(levels.iter().filter(|&&l| l == 1).count(), 1, "{name}: one <h1>");
            for pair in levels.windows(2) {
                assert!(pair[1] <= pair[0] + 1, "{name}: heading levels skip");
            }

            for tag in ["section", "table", "dl", "ol", "ul", "nav", "main"] {
                assert_eq!(
                    html.matches(&format!("<{tag}")).count(),
                    html.matches(&format!("</{tag}>")).count(),
                    "{name}: unbalanced <{tag}>"
                );
            }
        }
    }

    /// Every page has the same sections, so the languages cannot drift apart.
    #[test]
    fn pages_have_the_same_structure() {
        let en_ids = attributes(page(Lang::En), "id");
        for lang in Lang::ALL {
            assert_eq!(attributes(page(lang), "id"), en_ids, "{lang:?}: ids differ");
            assert_eq!(
                heading_levels(page(lang)),
                heading_levels(page(Lang::En)),
                "{lang:?}: headings differ"
            );
        }
    }

    /// A manual says what the program needs and does. What was tested, and how it
    /// was made, belongs in `TODO.md`.
    #[test]
    fn pages_keep_to_what_the_program_does() {
        for lang in Lang::ALL {
            let page = page(lang).to_lowercase();
            for word in [
                "tested",
                "as is",
                "testira",
                "kakav jest",
                "probna verzija",
                "preizkušen",
                "kakršen je",
                "preizkusna različica",
            ] {
                assert!(!page.contains(word), "{lang:?}: {word}");
            }
        }
    }

    /// A link names the topic it leads to, so it cannot keep an old title.
    /// Links to the settings read "in Settings", in the case the sentence needs.
    #[test]
    fn links_name_their_headings() {
        for lang in Lang::ALL {
            let html = page(lang);
            for (i, marker) in html.match_indices("<a href=\"#") {
                let (target, rest) = html[i + marker.len()..].split_once("\">").unwrap();
                let (text, _) = rest.split_once("</a>").unwrap();
                let id = format!(" id=\"{target}\">");
                let Some(start) = html.find(&id) else { continue };
                let (heading, _) = html[start + id.len()..].split_once("</h").unwrap();
                if !matches!(target, "settings" | "settings-receiving") {
                    assert_eq!(text, heading, "{lang:?}: the link to #{target}");
                }
            }
        }
    }

    /// The help names controls exactly as the program shows them.
    #[test]
    fn pages_use_the_program_texts() {
        for lang in Lang::ALL {
            let s = lang.strings();
            let html = page(lang);
            let texts = [
                s.menu_file,
                s.menu_transfers,
                s.menu_show_transfers,
                s.menu_no_transfers,
                s.ctx_show_transfer,
                s.col_transfer,
                s.menu_help,
                s.menu_view_help,
                s.menu_how,
                s.menu_about,
                s.menu_license,
                s.menu_donate,
                s.license_title,
                s.license_show_label,
                s.license_text_label,
                s.license_dodavo,
                s.license_components,
                s.menu_send_file,
                s.menu_send_folder,
                s.menu_add,
                s.menu_edit,
                s.menu_remove,
                s.menu_copy_id,
                s.menu_change_id,
                s.change_id_button,
                s.menu_settings,
                s.menu_exit,
                s.ctx_edit,
                s.ctx_remove,
                s.ctx_block,
                s.menu_block,
                s.block_sender,
                s.blocked_senders_button,
                s.blocked_title,
                s.unblock,
                s.tray_open,
                s.add_recipient_title,
                s.edit_recipient_title,
                s.col_name,
                s.col_contact_id,
                s.status_connecting,
                s.status_online,
                s.status_offline,
                s.incoming_title,
                s.incoming_balloon_title,
                s.receiving_balloon_title,
                s.save_to,
                s.browse,
                s.accept,
                s.decline,
                s.phase_preparing,
                s.phase_sending,
                s.phase_receiving,
                s.phase_unpacking,
                s.phase_part_in_use,
                s.tx_part_in_use,
                s.tx_peer_part_in_use,
                s.row_transferred,
                s.row_processed,
                s.row_speed,
                s.row_remaining,
                s.connection_direct,
                s.connection_relay,
                s.cancel,
                s.close,
                s.pause,
                s.resume,
                s.minimize_background,
                s.phase_paused,
                s.phase_verifying,
                s.wait_for_connection,
                s.connection_lost_label,
                s.lost_ask,
                s.lost_wait,
                s.lost_cancel,
                s.exit_stay,
                s.exit_keep,
                s.exit_cancel_transfers,
                s.continue_background,
                s.cancel_transfer,
                s.dont_ask_again,
                s.open_file,
                s.open_containing_folder,
                s.open_received_folder,
                s.ok,
                s.my_name_label,
                s.settings_tab_general,
                s.show_after_send,
                s.close_when_ended,
                s.dont_show_again,
                s.settings_tab_transfers,
                s.always_save,
                s.always_save_folder_label,
                s.auto_accept,
                s.received_folder_label,
                s.received_extract,
                s.received_keep_zip,
                s.received_both,
                s.done_saved_folder_zip,
                s.ask_after_receive,
                s.settings_tab_receiving,
                s.temp_folder_label,
                s.warn_fat_temp,
                s.temp_fat_choose,
                s.temp_fat_continue,
                s.dont_warn_again,
                s.explorer_verb,
                s.explorer_menu,
                s.send_to_title,
                s.incoming_items_label,
                s.send_to_items_label,
                s.send_to_recipient_label,
                s.send_to_send,
                s.ctx_add,
                s.cancel,
                s.connection_lost_balloon_title,
                s.start_with_windows,
                s.start_minimized,
                s.keep_in_tray,
                s.close_ask,
                s.close_background,
                s.close_cancel,
                s.language_default,
                s.copy,
                s.close_running_label,
                s.language_label,
                s.cancelled,
                s.cancelling,
                s.tx_peer_unpacking,
                s.notify_too_late_title,
                s.handed_over_title,
                s.restored_balloon_title,
                s.menu_check_updates,
                s.check_updates,
                s.update_title,
                s.release_notes_label,
                s.install_update,
                s.remind_later,
                s.cancel_download,
                s.update_balloon_title,
                s.updated_balloon_title,
                s.error_recipient_failed,
                s.error_sender_failed,
                s.limit_size,
                s.limit_size_label,
                s.over_limit_label,
                s.over_ask,
                s.over_decline,
                s.over_decline_tell,
                s.limit_declined_title,
                s.recipient_limit_label,
                s.recipient_limit_general,
                s.recipient_limit_none,
                s.recipient_limit_own,
            ];
            for text in texts {
                let text = visible(text);
                let text = text.trim_end_matches(':');
                assert!(html.contains(text), "{lang:?}: the help does not mention {text:?}");
            }
            for lang in Lang::ALL {
                assert!(html.contains(lang.native_name()), "{lang:?}: language list");
            }
            // The author, as in Cargo.toml.
            assert!(html.contains(crate::ui::APP_AUTHOR), "{lang:?}: author");
            assert!(
                html.contains(&format!("href=\"{}\"", crate::ui::DONATE_URL)),
                "{lang:?}: the donation page"
            );
        }
    }
}
