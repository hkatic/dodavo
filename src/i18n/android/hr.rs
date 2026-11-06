use super::Strings;
use crate::i18n::Lang;

pub const HR: Strings = Strings {
    lang: Lang::Hr,

    space_ok: "Ima dovoljno slobodnog prostora: potrebno je {needed}, a dostupno je {free}.",
    space_short: "Nema dovoljno slobodnog prostora: potrebno je {needed}, a dostupno je samo {free}.",
    space_fat: "Ova pohrana koristi format FAT, koji ne može spremiti datoteke od 4 GB ili veće, a prijenos bi ondje spremio datoteku od {size}.",
    tx_part_in_use: "Čeka se druga aplikacija",
    tx_peer_part_in_use: "Čeka se aplikacija na drugoj strani",
    phase_part_in_use: "Djelomično primljenu datoteku koristi druga aplikacija ili Android nije dopustio pristup njoj. Prijenos se nastavlja čim se to promijeni.",
    phase_peer_part_in_use: "Na drugoj strani ({name}) djelomično primljenu datoteku koristi druga aplikacija. Prijenos se nastavlja čim je ta aplikacija zatvori.",
    part_in_use_balloon: "„{item}”: djelomično primljenu datoteku koristi druga aplikacija ili Android nije dopustio pristup njoj. Prijenos se nastavlja čim se to promijeni.",
    peer_part_in_use_balloon: "„{item}”: na drugoj strani ({name}) djelomično primljenu datoteku koristi druga aplikacija. Prijenos se nastavlja čim je ta aplikacija zatvori.",
    error_no_access: "Dodavo ne smije koristiti ovu stavku.",
    error_name_too_long: "Naziv ili putanja stavke predugi su za ovaj uređaj.",
    error_read_only: "Pohrana se može samo čitati.",
    error_system: "Sustav je prijavio pogrešku (kod {code}).",
};
