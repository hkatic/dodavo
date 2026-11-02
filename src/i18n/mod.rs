//! User interface translations.
//!
//! Every language is one `Strings` table (`en.rs`, `hr.rs`, …). Adding a key to
//! the `strings!` list below makes every table that lacks it fail to compile, so
//! no language can be left behind. Use `tr!(key)` for plain text,
//! `tr!(key, name = value)` to fill `{name}` placeholders and
//! `trn!(key, n, …)` for texts that depend on a number (`{n}` is filled in).
//!
//! The language follows the Windows display language unless one is chosen in
//! the settings; `DODAVO_LANG=hr` (or `en`, `sr-Latn`, `sl`) overrides both.

mod en;
mod hr;
mod sl;
mod sr_latn;

use std::{fmt::Display, sync::OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Hr,
    SrLatn,
    Sl,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::En, Lang::Hr, Lang::SrLatn, Lang::Sl];

    /// The code stored in the settings.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Hr => "hr",
            Lang::SrLatn => "sr-Latn",
            Lang::Sl => "sl",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL
            .into_iter()
            .find(|l| l.code().eq_ignore_ascii_case(code.trim()))
    }

    /// The name of the language in that language (for the settings list).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Hr => "Hrvatski",
            Lang::SrLatn => "Srpski (latinica)",
            Lang::Sl => "Slovenščina",
        }
    }

    /// The Windows locale name, used to ask Windows for matching system texts.
    fn locale_name(self) -> &'static str {
        match self {
            Lang::En => "en-US",
            Lang::Hr => "hr-HR",
            Lang::SrLatn => "sr-Latn-RS",
            Lang::Sl => "sl-SI",
        }
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::En => &en::EN,
            Lang::Hr => &hr::HR,
            Lang::SrLatn => &sr_latn::SR_LATN,
            Lang::Sl => &sl::SL,
        }
    }

    /// Maps a Windows language name such as `hr-HR` to a supported language.
    fn from_locale(name: &str) -> Option<Lang> {
        let primary = name.split('-').next()?.to_ascii_lowercase();
        match primary.as_str() {
            "en" => Some(Lang::En),
            "hr" => Some(Lang::Hr),
            // Serbian in Cyrillic script gets the Latin translation, which every
            // Serbian reader can read, rather than English.
            "sr" => Some(Lang::SrLatn),
            "sl" => Some(Lang::Sl),
            _ => None,
        }
    }
}

/// CLDR plural categories used by the supported languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    One,
    Two,
    Few,
    Other,
}

pub fn plural_category(lang: Lang, n: u64) -> Category {
    let (n10, n100) = (n % 10, n % 100);
    match lang {
        Lang::En => {
            if n == 1 {
                Category::One
            } else {
                Category::Other
            }
        }
        Lang::Hr | Lang::SrLatn => {
            if n10 == 1 && n100 != 11 {
                Category::One
            } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
                Category::Few
            } else {
                Category::Other
            }
        }
        Lang::Sl => match n100 {
            1 => Category::One,
            2 => Category::Two,
            3 | 4 => Category::Few,
            _ => Category::Other,
        },
    }
}

/// A text with one form per plural category. Languages without a category
/// repeat the `other` form there.
pub struct Plural {
    pub one: &'static str,
    pub two: &'static str,
    pub few: &'static str,
    pub other: &'static str,
}

macro_rules! strings {
    (texts { $($t:ident),* $(,)? } plurals { $($p:ident),* $(,)? }) => {
        pub struct Strings {
            pub lang: Lang,
            $(pub $t: &'static str,)*
            $(pub $p: Plural,)*
        }

        impl Strings {
            #[cfg(test)]
            pub fn texts(&self) -> Vec<(&'static str, &'static str)> {
                vec![$((stringify!($t), self.$t)),*]
            }

            #[cfg(test)]
            pub fn plurals(&self) -> Vec<(&'static str, &Plural)> {
                vec![$((stringify!($p), &self.$p)),*]
            }
        }
    };
}

strings! {
    texts {
        // Main window: menus.
        menu_file,
        menu_transfers,
        menu_help,
        menu_send_file,
        menu_send_folder,
        menu_add,
        menu_edit,
        menu_remove,
        menu_copy_id,
        menu_settings,
        menu_exit,
        menu_view_help,
        menu_how,
        menu_about,
        menu_license,
        // The recipient's actions menu.
        ctx_send_file,
        ctx_send_folder,
        ctx_add,
        ctx_edit,
        ctx_remove,
        ctx_show_transfer,
        ctx_show_transfers,
        tray_open,
        menu_show_transfers,
        menu_no_transfers,
        transfer_menu_entry,
        // Main window: controls and messages. The buttons have no access
        // keys: Alt+letter there belongs to the menu bar, and the buttons
        // have shortcuts (Ctrl+O …) instead.
        recipients_label,
        send_file,
        send_folder,
        add_recipient,
        copy_my_id,
        col_name,
        col_contact_id,
        col_transfer,
        // Main window: the transfer column and the Transfers menu.
        transfer_status,
        transfer_status_percent,
        tx_preparing,
        tx_connecting,
        tx_waiting_accept,
        tx_sending,
        tx_receiving,
        tx_paused,
        tx_paused_by_peer,
        tx_waiting_connection,
        tx_part_in_use,
        tx_peer_part_in_use,
        tx_verifying,
        tx_unpacking,
        tx_peer_unpacking,
        status_connecting,
        status_online,
        status_offline,
        empty_list,
        no_recipients_yet,
        select_recipient_first,
        pick_file_title,
        pick_folder_title,
        remove_confirm,
        id_copied,
        clipboard_busy,
        help_text,
        about_instruction,
        about_text,
        about_license,
        website_failed,
        tray_hint,
        help_failed,
        // License window.
        license_title,
        license_intro,
        license_show_label,
        license_dodavo,
        license_components,
        license_text_label,
        // Progress window.
        title_send_file,
        title_send_folder,
        title_receive_file,
        title_receive_folder,
        to_peer,
        from_peer,
        starting,
        connection_direct,
        connection_relay,
        phase_preparing,
        phase_connecting,
        phase_waiting,
        phase_sending,
        phase_receiving,
        phase_unpacking,
        phase_peer_unpacking,
        phase_handed_over,
        row_transferred,
        row_processed,
        row_speed,
        row_remaining,
        progress_bar,
        progress_of,
        speed_value,
        remaining_value,
        calculating,
        done_sent_file,
        done_sent_folder,
        done_saved_file,
        done_saved_folder,
        done_saved_folder_zip,
        declined_by_peer,
        declined,
        cancelled,
        cancelled_by_peer,
        unreachable,
        failed,
        close,
        cancelling,
        cancel_confirm,
        notify_done_title,
        notify_failed_title,
        notify_too_late_title,
        notify_saved,
        notify_status,
        // Progress window: pausing, the lost connection and restored transfers.
        pause,
        resume,
        minimize_background,
        phase_paused,
        phase_paused_by_peer,
        phase_waiting_connection,
        phase_verifying,
        phase_part_in_use,
        phase_peer_part_in_use,
        connection_lost_instruction,
        connection_lost_text,
        wait_for_connection,
        paused_balloon_title,
        paused_by_peer_balloon,
        connection_lost_balloon_title,
        connection_lost_balloon,
        continuing_balloon_title,
        continuing_balloon,
        part_in_use_balloon_title,
        part_in_use_balloon,
        peer_part_in_use_balloon,
        peer_unpacking_balloon,
        handed_over_title,
        handed_over_balloon,
        restored_balloon_title,
        restored_balloon,
        restored_cancelling_balloon,
        // Closing a running transfer.
        close_running_instruction,
        close_running_text,
        continue_background,
        cancel_transfer,
        dont_ask_again,
        dont_show_again,
        background_balloon_title,
        background_balloon,
        // Finished transfer.
        finished_question,
        sent_item,
        open_file,
        open_containing_folder,
        open_received_folder,
        // Incoming transfer window.
        incoming_title,
        incoming_balloon_title,
        incoming_balloon,
        cancelled_balloon_title,
        wants_file,
        wants_folder,
        file_description,
        unknown_sender_warning,
        add_to_recipients,
        save_to,
        browse,
        accept,
        decline,
        pick_save_folder,
        choose_save_folder,
        folder_unusable,
        space_ok,
        space_short,
        space_unknown,
        space_folder_note,
        space_confirm,
        space_fat,
        // Recipient dialog.
        add_recipient_title,
        edit_recipient_title,
        recipient_name_label,
        contact_id_label,
        recipient_hint,
        enter_recipient_name,
        invalid_contact_id,
        own_contact_id,
        contact_id_taken,
        name_taken,
        // Settings dialog.
        settings_title,
        my_name_label,
        sending_heading,
        show_after_send,
        receiving_heading,
        always_save,
        always_save_folder_label,
        auto_accept,
        auto_accept_note,
        received_folder_label,
        received_extract,
        received_keep_zip,
        received_both,
        general_heading,
        start_with_windows,
        start_minimized,
        keep_in_tray,
        check_updates,
        ask_after_receive,
        close_running_label,
        close_ask,
        close_background,
        close_cancel,
        connection_lost_label,
        lost_ask,
        lost_wait,
        lost_cancel,
        language_label,
        language_default,
        language_restart,
        my_id_label,
        copy,
        enter_my_name,
        invalid_save_folder,
        // Exiting while transfers run.
        exit_text,
        exit_stay,
        exit_keep,
        exit_cancel_transfers,
        // Notification area icon tooltip.
        tray_tip_one,
        tray_tip_one_speed,
        tray_tip_many,
        tray_tip_many_speed,
        // Updates: Help menu command, notifications and the Update Available window.
        menu_check_updates,
        update_checking,
        update_latest,
        update_check_failed,
        update_balloon_title,
        update_balloon,
        updated_balloon_title,
        updated_balloon,
        update_not_installed,
        update_title,
        update_headline,
        release_notes_label,
        install_update,
        remind_later,
        cancel_download,
        update_downloading,
        update_verifying,
        update_launching,
        update_launching_admin,
        update_cancel_confirm,
        update_failed,
        update_err_network,
        update_err_http,
        update_err_too_large,
        update_err_manifest,
        update_err_signature,
        update_err_size,
        update_err_checksum,
        update_err_io,
        update_err_disk_space,
        update_err_launch,
        update_err_setup_exit,
        // Common.
        ok,
        cancel,
        // Sizes and durations.
        decimal_separator,
        kilobytes,
        megabytes,
        gigabytes,
        terabytes,
        few_seconds,
        minutes_seconds,
        hours_minutes,
        // Application.
        settings_not_saved,
        settings_unreadable,
        settings_backup,
        unknown_sender,
        unknown_user,
        contact_invalid,
        network_problem,
        receiving_balloon_title,
        receiving_balloon,
        panic_message,
        panic_message_goes_on,
        identity_failed,
        identity_damaged,
        // Transfer errors.
        untitled,
        error_cannot_read,
        error_timeout,
        error_unexpected_answer,
        error_file_shorter,
        error_peer_reported,
        error_recipient_failed,
        error_sender_failed,
        error_protocol_end,
        error_message_too_large,
        error_damaged,
        error_checksum,
        error_cannot_read_folder,
        error_cannot_open,
        error_unsafe_path,
        error_source_changed,
        error_part_missing,
        error_expired,
        error_expired_receiver_may_have,
        error_bad_resume,
        error_connection_lost,
        error_unexpected,
        error_disk_full,
        error_file_missing,
        error_folder_missing,
    }
    plurals {
        drop_confirm,
        exit_instruction,
        transfers_in_progress,
        folder_description,
        bytes,
        seconds,
    }
}

impl Strings {
    /// Picks the plural form for `n`.
    pub fn pick(&self, p: &Plural, n: u64) -> &'static str {
        match plural_category(self.lang, n) {
            Category::One => p.one,
            Category::Two => p.two,
            Category::Few => p.few,
            Category::Other => p.other,
        }
    }
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// Chooses the language: `DODAVO_LANG`, then the setting, then Windows.
/// Call once at startup, on the UI thread, before any text is shown.
pub fn init(setting: Option<&str>) {
    let chosen = std::env::var("DODAVO_LANG")
        .ok()
        .as_deref()
        .and_then(Lang::from_code)
        .or_else(|| setting.and_then(Lang::from_code));
    let lang = chosen.unwrap_or_else(system_language);
    if chosen.is_some() {
        prefer_for_system_texts(lang);
    }
    let _ = LANG.set(lang);
}

pub fn lang() -> Lang {
    LANG.get().copied().unwrap_or(Lang::En)
}

/// The texts of the current language (English until `init` was called).
pub fn s() -> &'static Strings {
    lang().strings()
}

/// The first supported language in the user's Windows display languages.
fn system_language() -> Lang {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let mut count = 0u32;
    let mut len = 0u32;
    let ok = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if ok == 0 || len == 0 {
        return Lang::En;
    }
    let mut buf = vec![0u16; len as usize];
    let ok = unsafe {
        GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buf.as_mut_ptr(), &mut len)
    };
    if ok == 0 {
        return Lang::En;
    }
    // A list of NUL separated names, ending with an empty one.
    String::from_utf16_lossy(&buf)
        .split('\0')
        .filter(|s| !s.is_empty())
        .find_map(Lang::from_locale)
        .unwrap_or(Lang::En)
}

/// Asks Windows to use `lang` for texts it draws for us (e.g. message box
/// buttons), if that language is installed. Otherwise Windows keeps its own.
fn prefer_for_system_texts(lang: Lang) {
    use windows_sys::Win32::Globalization::{MUI_LANGUAGE_NAME, SetThreadPreferredUILanguages};
    let mut names: Vec<u16> = lang.locale_name().encode_utf16().collect();
    names.extend([0, 0]);
    let mut count = 0u32;
    unsafe { SetThreadPreferredUILanguages(MUI_LANGUAGE_NAME, names.as_ptr(), &mut count) };
}

/// Replaces `{name}` placeholders in `template`. Unknown placeholders are kept.
pub fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}').and_then(|end| {
            args.iter()
                .find(|(k, _)| *k == &after[..end])
                .map(|a| (end, a))
        }) {
            Some((end, (_, value))) => {
                out.push_str(&value.to_string());
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `tr!(key)` gives the text; `tr!(key, name = value, …)` fills placeholders.
macro_rules! tr {
    ($key:ident) => {
        $crate::i18n::s().$key
    };
    ($key:ident, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::fill(
            $crate::i18n::s().$key,
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// `trn!(key, n, name = value, …)` picks the plural form for `n` and fills
/// `{n}` and the other placeholders.
macro_rules! trn {
    ($key:ident, $n:expr $(, $name:ident = $value:expr)* $(,)?) => {{
        let s = $crate::i18n::s();
        let n = $n;
        $crate::i18n::fill(
            s.pick(&s.$key, n as u64),
            &[("n", &n as &dyn ::std::fmt::Display) $(, (stringify!($name), &$value as &dyn ::std::fmt::Display))*],
        )
    }};
}

pub(crate) use {tr, trn};

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
        let en = Lang::En.strings();
        for lang in Lang::ALL {
            let s = lang.strings();
            assert_eq!(s.lang, lang);
            for ((key, text), (_, en_text)) in s.texts().into_iter().zip(en.texts()) {
                assert!(!text.is_empty(), "{lang:?}: {key} is empty");
                assert_eq!(
                    placeholders(text),
                    placeholders(en_text),
                    "{lang:?}: placeholders of {key}"
                );
            }
            for ((key, p), (_, en_p)) in s.plurals().into_iter().zip(en.plurals()) {
                let mut expected = placeholders(en_p.other);
                expected.insert("n".into());
                for form in [p.one, p.two, p.few, p.other] {
                    assert_eq!(
                        placeholders(form),
                        expected,
                        "{lang:?}: placeholders of {key}"
                    );
                }
            }
        }
    }

    /// Access keys must be unique among the controls of one window or menu.
    #[test]
    fn access_keys_are_unique() {
        fn key(text: &str) -> Option<char> {
            let mut chars = text.chars();
            while let Some(c) = chars.next() {
                if c == '&' {
                    match chars.next() {
                        Some('&') | None => {}
                        Some(k) => return k.to_lowercase().next(),
                    }
                }
            }
            None
        }
        for lang in Lang::ALL {
            let s = lang.strings();
            let groups: [(&str, Vec<&str>); 21] = [
                ("menu bar", vec![s.menu_file, s.menu_transfers, s.menu_help]),
                (
                    "File menu",
                    vec![
                        s.menu_send_file,
                        s.menu_send_folder,
                        s.menu_add,
                        s.menu_edit,
                        s.menu_remove,
                        s.menu_copy_id,
                        s.menu_settings,
                        s.menu_exit,
                    ],
                ),
                (
                    "Help menu",
                    vec![
                        s.menu_view_help,
                        s.menu_how,
                        s.menu_check_updates,
                        s.menu_license,
                        s.menu_about,
                    ],
                ),
                // `ctx_show_transfers` replaces `ctx_show_transfer` when there are
                // several, so the two share their access key.
                (
                    "context menu",
                    vec![
                        s.ctx_send_file,
                        s.ctx_send_folder,
                        s.ctx_show_transfer,
                        s.ctx_edit,
                        s.ctx_remove,
                    ],
                ),
                // Without a selected recipient.
                ("context menu (empty list)", vec![s.ctx_add]),
                ("Transfers menu", vec![s.menu_show_transfers]),
                (
                    "tray menu",
                    vec![s.tray_open, s.menu_transfers, s.menu_copy_id, s.menu_exit],
                ),
                // Alt+letter in the main window reaches both the menu bar
                // and the controls; the control would win.
                (
                    "main window",
                    vec![
                        s.menu_file,
                        s.menu_transfers,
                        s.menu_help,
                        s.recipients_label,
                    ],
                ),
                (
                    "settings",
                    vec![
                        s.my_name_label,
                        s.show_after_send,
                        s.always_save,
                        s.always_save_folder_label,
                        s.browse,
                        s.auto_accept,
                        s.received_folder_label,
                        s.ask_after_receive,
                        s.start_with_windows,
                        s.start_minimized,
                        s.keep_in_tray,
                        s.check_updates,
                        s.close_running_label,
                        s.connection_lost_label,
                        s.language_label,
                        s.copy,
                    ],
                ),
                (
                    "progress window",
                    vec![s.pause, s.resume, s.minimize_background],
                ),
                (
                    "close running transfer",
                    vec![s.continue_background, s.cancel_transfer, s.dont_ask_again],
                ),
                (
                    "connection lost",
                    vec![s.wait_for_connection, s.cancel_transfer, s.dont_ask_again],
                ),
                (
                    "exit",
                    vec![s.exit_stay, s.exit_keep, s.exit_cancel_transfers],
                ),
                (
                    "received file",
                    vec![s.open_file, s.open_containing_folder, s.dont_ask_again],
                ),
                (
                    "received folder",
                    vec![s.open_received_folder, s.dont_ask_again],
                ),
                // Its Close button is Windows' own (task dialog).
                ("sent", vec![s.dont_show_again]),
                (
                    "recipient",
                    vec![s.recipient_name_label, s.contact_id_label],
                ),
                (
                    "incoming",
                    vec![
                        s.add_to_recipients,
                        s.save_to,
                        s.browse,
                        s.accept,
                        s.decline,
                    ],
                ),
                (
                    "update",
                    vec![
                        s.release_notes_label,
                        s.install_update,
                        s.remind_later,
                        s.cancel_download,
                    ],
                ),
                // The Close button of both is Windows' own (task dialog) or
                // has no access key (Enter and Escape close the window).
                ("about", vec![s.menu_license]),
                ("license", vec![s.license_show_label, s.license_text_label]),
            ];
            for (group, texts) in groups {
                let mut seen = BTreeSet::new();
                for text in texts {
                    let k = key(text).unwrap_or_else(|| {
                        panic!("{lang:?}: {text:?} in {group} has no access key")
                    });
                    assert!(
                        seen.insert(k),
                        "{lang:?}: access key '{k}' repeats in {group}"
                    );
                }
            }
            // The main window's buttons would take Alt+letter from the menu bar.
            for text in [s.send_file, s.send_folder, s.add_recipient, s.copy_my_id] {
                assert_eq!(
                    key(text),
                    None,
                    "{lang:?}: the main window button {text:?} must not have an access key"
                );
            }
        }
    }

    #[test]
    fn plural_rules() {
        use Category::*;
        let hr = [
            (1, One),
            (2, Few),
            (4, Few),
            (5, Other),
            (11, Other),
            (12, Other),
            (14, Other),
            (21, One),
            (22, Few),
            (25, Other),
            (111, Other),
            (0, Other),
        ];
        for (n, c) in hr {
            assert_eq!(plural_category(Lang::Hr, n), c, "hr {n}");
            assert_eq!(plural_category(Lang::SrLatn, n), c, "sr {n}");
        }
        let sl = [
            (1, One),
            (2, Two),
            (3, Few),
            (4, Few),
            (5, Other),
            (101, One),
            (102, Two),
            (103, Few),
            (111, Other),
            (0, Other),
        ];
        for (n, c) in sl {
            assert_eq!(plural_category(Lang::Sl, n), c, "sl {n}");
        }
        assert_eq!(plural_category(Lang::En, 1), One);
        assert_eq!(plural_category(Lang::En, 0), Other);
    }

    #[test]
    fn fill_placeholders() {
        assert_eq!(
            fill("{a} and {b} {c}", &[("a", &1), ("b", &"x")]),
            "1 and x {c}"
        );
        assert_eq!(fill("{ {a}}", &[("a", &"y")]), "{ y}");
    }

    #[test]
    fn locale_names() {
        assert_eq!(Lang::from_locale("hr-HR"), Some(Lang::Hr));
        assert_eq!(Lang::from_locale("sr-Cyrl-RS"), Some(Lang::SrLatn));
        assert_eq!(Lang::from_locale("sl-SI"), Some(Lang::Sl));
        assert_eq!(Lang::from_locale("de-DE"), None);
        assert_eq!(Lang::from_code("SR-latn"), Some(Lang::SrLatn));
    }
}
