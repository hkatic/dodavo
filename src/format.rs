//! Sizes and durations as the user interface shows them.

use crate::i18n::{self, Strings, fill, tr, trn};

pub fn format_size(bytes: u64) -> String {
    format_size_in(i18n::s(), bytes)
}

fn format_size_in(s: &Strings, bytes: u64) -> String {
    if bytes < 1024 {
        return fill(s.pick(&s.bytes, bytes), &[("n", &bytes)]);
    }
    let units = [s.kilobytes, s.megabytes, s.gigabytes, s.terabytes];
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < units.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    let number = if v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}").replace('.', s.decimal_separator)
    };
    format!("{number} {}", units[unit])
}

pub fn format_duration(secs: u64) -> String {
    match secs {
        0..=4 => tr!(few_seconds).into(),
        5..=59 => trn!(seconds, secs),
        60..=3599 => tr!(minutes_seconds, m = secs / 60, s = secs % 60),
        _ => tr!(hours_minutes, h = secs / 3600, m = (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn sizes() {
        let en = Lang::En.strings();
        assert_eq!(format_size_in(en, 1), "1 byte");
        assert_eq!(format_size_in(en, 512), "512 bytes");
        assert_eq!(format_size_in(en, 1536), "1.5 KB");
        assert_eq!(format_size_in(en, 250 * 1024 * 1024), "250 MB");
        let hr = Lang::Hr.strings();
        assert_eq!(format_size_in(hr, 1536), "1,5 kB");
        assert_eq!(format_size_in(hr, 3), "3 bajta");
        assert_eq!(format_size_in(Lang::Sl.strings(), 2), "2 bajta");
    }
}
