use super::Strings;
use crate::i18n::Lang;

pub const SL: Strings = Strings {
    lang: Lang::Sl,

    space_ok: "Prostora je dovolj: potrebno je {needed}, na voljo je {free}.",
    space_short: "Prostora ni dovolj: potrebno je {needed}, na voljo pa je le {free}.",
    space_fat: "Ta shramba uporablja obliko FAT, ki ne more shraniti datotek, velikih 4 GB ali več, prenos pa bi tja shranil datoteko velikosti {size}.",
    tx_part_in_use: "Čakanje na drugo aplikacijo",
    tx_peer_part_in_use: "Čakanje na aplikacijo na drugi strani",
    phase_part_in_use: "Delno prejeto datoteko uporablja druga aplikacija ali pa Android ni dovolil dostopa do nje. Prenos se nadaljuje, takoj ko se to spremeni.",
    phase_peer_part_in_use: "Na drugi strani ({name}) delno prejeto datoteko uporablja druga aplikacija. Prenos se nadaljuje, takoj ko jo ta aplikacija zapre.",
    part_in_use_balloon: "»{item}«: delno prejeto datoteko uporablja druga aplikacija ali pa Android ni dovolil dostopa do nje. Prenos se nadaljuje, takoj ko se to spremeni.",
    peer_part_in_use_balloon: "»{item}«: na drugi strani ({name}) delno prejeto datoteko uporablja druga aplikacija. Prenos se nadaljuje, takoj ko jo ta aplikacija zapre.",
    error_no_access: "Dodavo te postavke ne sme uporabljati.",
    error_name_too_long: "Ime ali pot elementa je predolga za to napravo.",
    error_read_only: "Shramba je samo za branje.",
    error_system: "Sistem je sporočil napako (koda {code}).",
};
