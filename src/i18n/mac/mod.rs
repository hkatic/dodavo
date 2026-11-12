//! The texts of the Mac user interface. Those of the transfers are in the
//! table of `i18n`, with what differs on a Mac here under the same name (see
//! [`mac_tr`](crate::mac_tr)). Written with Apple's terms for each language: what
//! macOS itself calls things in its own translations.

use super::{Lang, Plural};

mod en;
mod hr;
mod sl;
mod sr_latn;

super::strings! {
    Strings;
    texts {
        col_name,
        col_contact_id,
        col_transfer,
        status_connecting,
        status_online,
        status_offline,
        network_problem,
        contact_invalid,
        recipient_gone,
        enter_recipient_name,
        invalid_contact_id,
        own_contact_id,
        contact_id_taken,
        name_taken,
        remove_confirm,
        pause,
        resume,
        cancel,
        accept,
        decline,
        close,
        wants_file,
        wants_folder,
        file_description,
        unknown_sender_warning,
        add_to_recipients,
        space_folder_note,
        choose_save_folder,
        folder_unusable,
        space_confirm,
        cancel_confirm,
        my_name_label_plain,
        my_id_label,
        copy,
        always_save_folder_label,
        received_extract,
        received_both,
        enter_my_name,
        invalid_save_folder,
        temp_folder_label,
        temp_folder_default,
        use_default,
        pick_temp_folder,
        invalid_temp_folder,
        warn_fat_temp,
        temp_fat_title,
        temp_fat_receive,
        temp_fat_send,
        temp_fat_choose,
        continue_anyway,
        dont_warn_again,
        settings_not_saved,
        settings_title,
        about_text,
        license_title,
        license_dodavo,
        license_components,
        license_intro,
        website_failed,
        help_failed,
        update_title,
        update_headline,
        release_notes_label,
        install_update,
        remind_later,
        cancel_download,
        update_downloading,
        update_verifying,
        update_check_failed,
        update_failed,
        update_cancel_confirm,
        update_not_installed,
        settings_unreadable,
        settings_backup,
        identity_failed,
        incoming_balloon_title,
        incoming_balloon,
        receiving_balloon_title,
        receiving_balloon,
        cancelled_balloon_title,
        restored_balloon_title,
        restored_balloon,
        restored_cancelling_balloon,
        update_balloon_title,
        update_balloon,
        updated_balloon_title,
        updated_balloon,
        menu_about,
        menu_check_updates,
        menu_settings,
        menu_preferences,
        menu_services,
        menu_hide,
        menu_hide_others,
        menu_show_all,
        menu_quit,
        menu_file,
        menu_send,
        menu_add_recipient,
        menu_edit_recipient,
        menu_remove_recipient,
        menu_block_recipient,
        menu_copy_id,
        menu_change_id,
        menu_close,
        menu_edit,
        menu_undo,
        menu_redo,
        menu_cut,
        menu_copy,
        menu_paste,
        menu_delete,
        menu_select_all,
        menu_transfers,
        menu_show_transfers,
        menu_no_transfers,
        menu_show_transfer,
        menu_window,
        menu_minimize,
        menu_zoom,
        menu_bring_all_to_front,
        menu_help,
        menu_dodavo_help,
        menu_license,
        menu_donate,
        menu_open_dodavo,
        status_item_label,
        recipients_label,
        toolbar_send,
        toolbar_add,
        toolbar_copy_id,
        toolbar_transfers,
        empty_list,
        id_copied,
        change_id_button,
        change,
        change_id_confirm,
        change_id_text,
        change_id_done,
        change_id_done_text,
        change_id_busy,
        change_id_failed,
        change_id_not_ready,
        add_recipient_title,
        edit_recipient_title,
        recipient_name_label,
        recipient_name_help,
        contact_id_label,
        recipient_hint,
        add,
        save,
        remove,
        remove_text,
        block_confirm,
        block_text,
        block_recipient_text,
        block,
        block_sender,
        send_panel_message,
        send_panel_message_many,
        names_and_more,
        send_panel_prompt,
        pick_recipient_title,
        pick_recipient_text,
        no_recipients_title,
        no_recipients_text,
        send_action,
        send_failed,
        transfers_title,
        transfers_empty,
        clear_finished,
        cancel_transfer,
        keep_transferring,
        open,
        show_in_finder,
        remove_from_list,
        sending_to,
        receiving_from,
        failed_marker,
        succeeded_marker,
        incoming_title,
        save_to,
        choose,
        choose_folder_prompt,
        accept_anyway,
        space_ok,
        space_short,
        space_fat,
        settings_general,
        settings_receiving,
        settings_blocked,
        blocked_senders_label,
        blocked_entry,
        blocked_none,
        unblock,
        my_name_label,
        my_name_help,
        open_at_login,
        hide_at_login,
        show_in_menu_bar,
        check_updates,
        language_label,
        language_system,
        always_save,
        auto_accept,
        auto_accept_note,
        limit_size,
        limit_size_label,
        limit_unit_name,
        unit_mb,
        unit_gb,
        over_limit_label,
        over_ask,
        over_decline,
        over_decline_tell,
        recipient_limit_label,
        recipient_limit_help,
        recipient_limit_general,
        recipient_limit_none,
        recipient_limit_own,
        received_folder_label,
        received_zip,
        login_item_failed,
        quit_text,
        quit_keep,
        quit_cancel_transfers,
        dont_quit,
        about_notice,
        license_show_label,
        move_title,
        move_text,
        move_button,
        move_later,
        move_failed,
        system_settings,
        system_preferences,
        error_no_access,
        error_name_too_long,
        error_read_only,
        error_system,
        ns_local_network,
        ns_downloads,
        ns_documents,
        ns_desktop,
        ns_removable,
        ns_network,
        service_send,
        tx_part_in_use,
        tx_peer_part_in_use,
        phase_part_in_use,
        phase_peer_part_in_use,
        part_in_use_balloon,
        peer_part_in_use_balloon,
        notify_saved,
        update_installing,
        update_err_not_signed,
        update_err_mount,
        update_err_install,
        update_move_first,
        update_checking,
        space_unknown,
        panic_message,
        language_restart,
        update_latest,
        update_manual,
        cancelled_text,
    }
    plurals {
        folder_description,
        quit_instruction,
        change_id_transfers,
        drop_confirm_many,
        drop_confirm_recipients,
        transfers_in_progress,
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

    /// What only makes sense on Windows does not come along, nor do its
    /// access keys.
    #[test]
    fn no_windows_in_the_mac_texts() {
        for lang in Lang::ALL {
            for (key, text) in strings(lang).texts() {
                assert!(!text.contains('&') || text.contains("Privacy & Security"), "{lang:?}: {key}");
                assert!(!text.contains('\t'), "{lang:?}: {key}");
                for word in [
                    "Windows",
                    "Explorer",
                    "Setup",
                    "program ",
                    "programa",
                    "taskbar",
                    "notification area",
                    "područj",
                    "območj",
                    "oblast",
                ] {
                    assert!(!text.contains(word), "{lang:?}: {key}: {word}");
                }
            }
        }
    }
}
