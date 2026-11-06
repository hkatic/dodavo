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
        notify_saved,
        // The system's errors.
        error_no_access,
        error_name_too_long,
        error_read_only,
        error_system,
        // What the state of the program says (`crates/dodavo-ffi`).
        status_offline,
        status_online,
        network_problem,
        space_confirm,
        choose_save_folder,
        contact_invalid,
        name_taken,
        contact_id_taken,
        invalid_save_folder,
        enter_my_name,
        own_contact_id,
        invalid_contact_id,
        enter_recipient_name,
        panic_message,
        add_to_recipients,
        wants_folder,
        wants_file,
        file_description,
        folder_unusable,
        identity_failed,
        settings_backup,
        settings_not_saved,
        settings_unreadable,
        unknown_sender_warning,
        // The screens of the app (`android/`), as `R.string` (see
        // `crates/dodavo-android`): from here on, the texts go into the
        // app's resources, and those above do not.
        nav_recipients,
        nav_transfers,
        more_options,
        recipient_options,
        menu_share_my_id,
        menu_copy_my_id,
        menu_settings,
        menu_help,
        menu_about,
        action_send_files,
        action_send_folder,
        action_edit,
        action_copy_id,
        action_block,
        action_remove,
        add_recipient,
        edit_recipient,
        recipients_empty,
        recipient_name_label,
        recipient_name_help,
        contact_id_label,
        contact_id_help,
        paste,
        save,
        close,
        cancel,
        navigate_up,
        remove_confirm,
        remove_text,
        remove,
        block_confirm,
        block_text,
        block_recipient_text,
        block,
        id_copied,
        share_id_title,
        transfers_empty,
        clear_finished,
        sending_to,
        receiving_from,
        pause,
        resume,
        cancel_transfer,
        keep_transferring,
        cancel_confirm,
        open,
        share,
        show_folder,
        remove_from_list,
        succeeded_marker,
        failed_marker,
        incoming_title,
        saved_in,
        accept,
        decline,
        block_sender,
        accept_anyway,
        settings_title,
        my_name_label,
        my_name_help,
        my_id_label,
        auto_accept,
        auto_accept_help,
        received_folder_label,
        received_extract,
        received_zip,
        received_both,
        blocked_senders,
        blocked_none,
        unblock,
        unblock_named,
        language_setting,
        language_setting_help,
        language_system,
        notifications_setting,
        notifications_setting_help,
        receive_background,
        receive_background_help,
        about_title,
        about_text,
        about_notice,
        copyright_line,
        version,
        website,
        privacy_policy,
        licenses,
        license_dodavo,
        license_components,
        help_title,
        starting,
        cannot_start,
        ok,
        no_app_to_open,
        item_unreadable,
        // Notifications.
        channel_incoming,
        channel_transfers,
        channel_finished,
        channel_background,
        notify_transfers,
        notify_ready,
        notify_ready_text,
        review,
    }
    plurals {
        folder_description,
    }
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
        const OS_KEYS: [&str; 10] = [
            "space_ok",
            "space_short",
            "space_fat",
            "tx_part_in_use",
            "tx_peer_part_in_use",
            "phase_part_in_use",
            "phase_peer_part_in_use",
            "part_in_use_balloon",
            "peer_part_in_use_balloon",
            "notify_saved",
        ];
        for lang in Lang::ALL {
            let main: std::collections::HashMap<_, _> = lang.strings().texts().into_iter().collect();
            let own: std::collections::HashMap<_, _> = strings(lang).texts().into_iter().collect();
            for key in OS_KEYS {
                assert!(
                    placeholders(own[key]).is_subset(&placeholders(main[key])),
                    "{lang:?}: {key}"
                );
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
                    "Windows", "Explorer", "Finder", "macOS", "Mac ", "program ", "programa",
                    "drive", "disk",
                ] {
                    assert!(!text.contains(word), "{lang:?}: {key}: {word}");
                }
            }
        }
    }
}
