//! The limit on the size of an item that someone may send: what counts as
//! over it, the line the Incoming Transfer window shows then, and the size
//! as the settings take and show it (a whole number of MB or GB).

use crate::{
    format::format_size,
    i18n::{tr, trn},
    net::Offer,
};

/// The unit of a limit in the settings. 1024-based, as `format_size` shows
/// sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Mb,
    Gb,
}

impl Unit {
    /// In the order shown in the settings.
    pub const ALL: [Unit; 2] = [Unit::Mb, Unit::Gb];

    pub fn bytes(self) -> u64 {
        match self {
            Unit::Mb => 1 << 20,
            Unit::Gb => 1 << 30,
        }
    }

    /// As the settings show it.
    pub fn name(self) -> &'static str {
        match self {
            Unit::Mb => tr!(megabytes),
            Unit::Gb => tr!(gigabytes),
        }
    }

    /// As the user interfaces of the Mac and Android name it to the core.
    pub fn code(self) -> &'static str {
        match self {
            Unit::Mb => "mb",
            Unit::Gb => "gb",
        }
    }

    pub fn from_code(code: &str) -> Option<Unit> {
        Unit::ALL.into_iter().find(|unit| unit.code() == code)
    }
}

/// The largest number of a unit that the settings take: more than any disk
/// holds, and far from overflowing.
const MAX_NUMBER: u64 = 1_000_000;

/// The size of the item that `offer` announces, as the limit counts it: what
/// arrives or what is saved, whichever is more (a folder arrives as a zip,
/// which may be smaller or larger than its files).
pub fn size_of(offer: &Offer) -> u64 {
    offer.size.max(offer.content_size)
}

/// Whether the item of `offer` is larger than `limit` (`None`: no limit).
pub fn over(offer: &Offer, limit: Option<u64>) -> bool {
    limit.is_some_and(|limit| size_of(offer) > limit)
}

/// The limit in bytes that the user entered as `text` in `unit`: a whole
/// number from 1 on. The error is the message to show.
pub fn from_input(text: &str, unit: Unit) -> Result<u64, String> {
    let text = text.trim();
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    digits
        .then(|| text.parse::<u64>().ok())
        .flatten()
        .filter(|number| (1..=MAX_NUMBER).contains(number))
        .map(|number| number * unit.bytes())
        .ok_or_else(|| tr!(limit_invalid, max = MAX_NUMBER))
}

/// `bytes` as the settings show it: in GB where that is a whole number, and
/// otherwise in MB, rounded up, and never less than 1.
pub fn to_input(bytes: u64) -> (u64, Unit) {
    let gb = Unit::Gb.bytes();
    if bytes >= gb && bytes % gb == 0 {
        (bytes / gb, Unit::Gb)
    } else {
        (bytes.div_ceil(Unit::Mb.bytes()).max(1), Unit::Mb)
    }
}

/// What the Incoming Transfer window says about `offers`, which all come
/// from a sender whose limit is `limit`: `None` if none of them is over it.
pub fn notice(offers: &[&Offer], limit: Option<u64>) -> Option<String> {
    let limit = limit?;
    let count = offers
        .iter()
        .filter(|offer| over(offer, Some(limit)))
        .count();
    let size = format_size(limit);
    match (count, offers.len()) {
        (0, _) => None,
        (1, 1) => Some(tr!(limit_notice, limit = size)),
        _ => Some(trn!(limit_notice_items, count, limit = size)),
    }
}

/// The notification for an offer that was declined for its size
/// (`net::Event::OverLimit`): its title and its text.
pub fn declined_notice(sender: &str, offer: &Offer) -> (String, String) {
    let text = tr!(
        limit_declined,
        name = sender,
        // As it would have been saved: a peer's name may hold line ends.
        item = crate::archive::sanitize_file_name(&offer.name),
        size = format_size(size_of(offer))
    );
    (tr!(limit_declined_title).to_string(), text)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::net::Kind;

    pub(crate) fn offer(size: u64, content_size: u64) -> Offer {
        Offer {
            kind: if content_size > 0 {
                Kind::Folder
            } else {
                Kind::File
            },
            name: "x".into(),
            size,
            file_count: 1,
            content_size,
            largest_file: content_size,
            sender_name: String::new(),
            token: [0; 16],
            modified: None,
            created: None,
        }
    }

    #[test]
    fn the_larger_of_what_arrives_and_what_is_saved_counts() {
        assert!(!over(&offer(100, 0), Some(100)));
        assert!(over(&offer(101, 0), Some(100)));
        // A folder whose zip is smaller than its files, and the other way.
        assert!(over(&offer(10, 101), Some(100)));
        assert!(over(&offer(101, 10), Some(100)));
        assert!(!over(&offer(100, 100), Some(100)));
        assert!(!over(&offer(u64::MAX, u64::MAX), None));
    }

    #[test]
    fn input_is_a_whole_number_of_a_unit() {
        assert_eq!(from_input("1", Unit::Mb), Ok(1 << 20));
        assert_eq!(from_input(" 500 ", Unit::Mb), Ok(500 << 20));
        assert_eq!(from_input("2", Unit::Gb), Ok(2 << 30));
        assert_eq!(from_input("1000000", Unit::Gb), Ok(1_000_000 << 30));
        for bad in [
            "",
            "0",
            "-1",
            "+1",
            "1.5",
            "1,5",
            "ten",
            "1000001",
            "99999999999999999999",
        ] {
            assert!(from_input(bad, Unit::Mb).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_limit_is_shown_as_it_was_entered() {
        for (number, unit) in [(1, Unit::Mb), (500, Unit::Mb), (1, Unit::Gb), (20, Unit::Gb)] {
            let bytes = from_input(&number.to_string(), unit).unwrap();
            assert_eq!(to_input(bytes), (number, unit));
        }
        // 1024 MB is 1 GB, and 1536 MB has no whole number of GB.
        assert_eq!(to_input(1024 << 20), (1, Unit::Gb));
        assert_eq!(to_input(1536 << 20), (1536, Unit::Mb));
        // A value from elsewhere (a file edited by hand) is never shown as 0.
        assert_eq!(to_input(0), (1, Unit::Mb));
        assert_eq!(to_input((1 << 20) + 1), (2, Unit::Mb));
    }

    #[test]
    fn the_window_says_how_many_items_are_over_the_limit() {
        let (small, big) = (offer(10, 0), offer(200, 0));
        assert_eq!(notice(&[&big], None), None);
        assert_eq!(notice(&[&small], Some(100)), None);
        assert_eq!(notice(&[&small, &small], Some(100)), None);
        let one = notice(&[&big], Some(100)).unwrap();
        assert!(one.contains(&format_size(100)), "{one}");
        let some = notice(&[&small, &big, &big], Some(100)).unwrap();
        assert!(some.contains('2'), "{some}");
        assert_ne!(one, some);
    }
}
