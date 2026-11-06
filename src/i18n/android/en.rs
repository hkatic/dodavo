use super::Strings;
use crate::i18n::Lang;

pub const EN: Strings = Strings {
    lang: Lang::En,

    space_ok: "There is enough free space: {needed} needed, {free} available.",
    space_short: "Not enough free space: {needed} needed, but only {free} available.",
    space_fat: "This storage uses the FAT format, which cannot hold files of 4 GB or more, but this transfer would save a file of {size} there.",
    tx_part_in_use: "Waiting for another app",
    tx_peer_part_in_use: "Waiting for an app on the other side",
    phase_part_in_use: "Another app is using the partly received file, or Android has not allowed access to it. The transfer continues as soon as that changes.",
    phase_peer_part_in_use: "On the other side ({name}), another app is using the partly received file. The transfer continues as soon as that app closes it.",
    part_in_use_balloon: "“{item}”: another app is using the partly received file, or Android has not allowed access to it. The transfer continues as soon as that changes.",
    peer_part_in_use_balloon: "“{item}”: on the other side ({name}), another app is using the partly received file. The transfer continues as soon as that app closes it.",
    error_no_access: "Dodavo may not use this item.",
    error_name_too_long: "The name or path of an item is too long for this device.",
    error_read_only: "The storage is read-only.",
    error_system: "The system reported an error (code {code}).",
};
