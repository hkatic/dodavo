//! Version numbers as written in Cargo.toml (semantic versioning), so that the
//! program can tell whether a published version is newer than itself.
//!
//! This file is also compiled into the release tool (`src/bin/dodavo-release.rs`),
//! so it must not refer to the rest of the program.

use std::{cmp::Ordering, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (`1.2.0-beta.1` → `["beta", "1"]`); empty for a release.
    pub pre: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ident {
    Num(u64),
    Alnum(String),
}

impl Version {
    /// Parses `MAJOR.MINOR.PATCH`, optionally followed by `-pre.release` and
    /// `+build.metadata` (which is ignored for comparisons, as semver says).
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let (text, _build) = match text.split_once('+') {
            Some((v, b)) => (v, Some(b)),
            None => (text, None),
        };
        let (core, pre) = match text.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (text, None),
        };
        let mut numbers = core.split('.');
        let major = number(numbers.next()?)?;
        let minor = number(numbers.next()?)?;
        let patch = number(numbers.next()?)?;
        if numbers.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => p.split('.').map(ident).collect::<Option<Vec<_>>>()?,
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// The version of the running program.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a valid version")
    }
}

fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return None;
    }
    s.parse().ok()
}

fn ident(s: &str) -> Option<Ident> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return number(s).map(Ident::Num);
    }
    Some(Ident::Alnum(s.to_string()))
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            // Numeric identifiers always have lower precedence than alphanumeric ones.
            (Ident::Num(_), Ident::Alnum(_)) => Ordering::Less,
            (Ident::Alnum(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Alnum(a), Ident::Alnum(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release is newer than any of its pre-releases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match id {
                Ident::Num(n) => write!(f, "{n}")?,
                Ident::Alnum(s) => f.write_str(s)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s:?} should parse"))
    }

    #[test]
    fn parses_and_prints() {
        assert_eq!(v("0.1.0").to_string(), "0.1.0");
        assert_eq!(v(" 1.2.3 ").to_string(), "1.2.3");
        assert_eq!(v("1.2.3-beta.1").to_string(), "1.2.3-beta.1");
        assert_eq!(v("1.2.3+build.7").to_string(), "1.2.3");
        assert_eq!(
            v("1.2.3-rc.1+build").pre,
            vec![Ident::Alnum("rc".into()), Ident::Num(1)]
        );
        for bad in [
            "",
            "1.0",
            "v1.0.0",
            "1.0.0.0",
            "01.0.0",
            "1.0.0-",
            "1.0.0-a..b",
            "a.b.c",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?} should not parse");
        }
        assert!(Version::current() >= v("1.0.0"));
    }

    #[test]
    fn orders_like_semver() {
        let ascending = [
            "0.1.0",
            "0.1.1",
            "0.2.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "2.0.0",
        ];
        for pair in ascending.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("1.0.0+a"), v("1.0.0+b"));
        assert_eq!(v("1.0.0").cmp(&v("1.0.0")), Ordering::Equal);
    }
}
