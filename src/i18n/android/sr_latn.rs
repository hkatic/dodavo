use super::Strings;
use crate::i18n::Lang;

pub const SR_LATN: Strings = Strings {
    lang: Lang::SrLatn,

    space_ok: "Ima dovoljno slobodnog prostora: potrebno je {needed}, a dostupno je {free}.",
    space_short: "Nema dovoljno slobodnog prostora: potrebno je {needed}, a dostupno je samo {free}.",
    space_fat: "Ova memorija koristi format FAT, koji ne može da sačuva datoteke od 4 GB ili veće, a prenos bi tamo sačuvao datoteku od {size}.",
    tx_part_in_use: "Čeka se druga aplikacija",
    tx_peer_part_in_use: "Čeka se aplikacija na drugoj strani",
    phase_part_in_use: "Delimično primljenu datoteku koristi druga aplikacija ili Android nije dozvolio pristup njoj. Prenos se nastavlja čim se to promeni.",
    phase_peer_part_in_use: "Na drugoj strani ({name}) delimično primljenu datoteku koristi druga aplikacija. Prenos se nastavlja čim je ta aplikacija zatvori.",
    part_in_use_balloon: "„{item}“: delimično primljenu datoteku koristi druga aplikacija ili Android nije dozvolio pristup njoj. Prenos se nastavlja čim se to promeni.",
    peer_part_in_use_balloon: "„{item}“: na drugoj strani ({name}) delimično primljenu datoteku koristi druga aplikacija. Prenos se nastavlja čim je ta aplikacija zatvori.",
    error_no_access: "Dodavo ne sme da koristi ovu stavku.",
    error_name_too_long: "Naziv ili putanja stavke su predugi za ovaj uređaj.",
    error_read_only: "Memorija može samo da se čita.",
    error_system: "Sistem je prijavio grešku (kod {code}).",
};
