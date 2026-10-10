use std::{borrow::Cow, fmt::Display, str::FromStr};

use bstr::{BStr, BString};

use crate::{Error, Integer};

impl Integer {
    /// Canonicalize values as simple decimal numbers.
    /// An optional suffix of k, m, or g (case-insensitive), will cause the
    /// value to be multiplied by 1024 (k), 1048576 (m), or 1073741824 (g) respectively.
    ///
    /// Returns the result if there is no multiplication overflow.
    pub fn to_decimal(&self) -> Option<i64> {
        match self.suffix {
            None => Some(self.value),
            Some(suffix) => match suffix {
                Suffix::Kibi => self.value.checked_mul(1024),
                Suffix::Mebi => self.value.checked_mul(1024 * 1024),
                Suffix::Gibi => self.value.checked_mul(1024 * 1024 * 1024),
            },
        }
    }
}

impl Display for Integer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)?;
        if let Some(suffix) = self.suffix {
            write!(f, "{suffix}")
        } else {
            Ok(())
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Integer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if let Some(suffix) = self.suffix {
            serializer.serialize_i64(self.value << suffix.bitwise_offset())
        } else {
            serializer.serialize_i64(self.value)
        }
    }
}

fn int_err(input: impl Into<BString>) -> Error {
    Error::new(
        "Integers needs to be positive or negative numbers which may have a suffix like 1k, 42, or 50G",
        input,
    )
}

impl TryFrom<&BStr> for Integer {
    type Error = Error;

    fn try_from(s: &BStr) -> Result<Self, Self::Error> {
        let text = std::str::from_utf8(s).map_err(|err| int_err(s).with_err(err))?;
        let (value, rest) = strtoimax(text).ok_or_else(|| int_err(s))?;
        if rest.is_empty() {
            return Ok(Self { value, suffix: None });
        }
        let suffix = rest.parse().map_err(|_| int_err(s))?;
        Ok(Self {
            value,
            suffix: Some(suffix),
        })
    }
}

impl TryFrom<&str> for Integer {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(BStr::new(value))
    }
}

impl TryFrom<Cow<'_, BStr>> for Integer {
    type Error = Error;

    fn try_from(c: Cow<'_, BStr>) -> Result<Self, Self::Error> {
        Self::try_from(c.as_ref())
    }
}

impl TryFrom<BString> for Integer {
    type Error = Error;

    fn try_from(value: BString) -> Result<Self, Self::Error> {
        Self::try_from(BStr::new(&value))
    }
}

/// Integer suffixes that are supported by `git-config`.
///
/// These values are base-2 unit of measurements, not the base-10 variants.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub enum Suffix {
    /// Multiply the value by 2^10.
    Kibi,
    /// Multiply the value by 2^20.
    Mebi,
    /// Multiply the value by 2^30.
    Gibi,
}

impl Suffix {
    /// Returns the number of bits that the suffix shifts left by.
    #[must_use]
    pub const fn bitwise_offset(self) -> usize {
        match self {
            Self::Kibi => 10,
            Self::Mebi => 20,
            Self::Gibi => 30,
        }
    }
}

impl Display for Suffix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kibi => write!(f, "k"),
            Self::Mebi => write!(f, "m"),
            Self::Gibi => write!(f, "g"),
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Suffix {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            Self::Kibi => "k",
            Self::Mebi => "m",
            Self::Gibi => "g",
        })
    }
}

impl FromStr for Suffix {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "k" | "K" => Ok(Self::Kibi),
            "m" | "M" => Ok(Self::Mebi),
            "g" | "G" => Ok(Self::Gibi),
            _ => Err(()),
        }
    }
}

impl TryFrom<&BStr> for Suffix {
    type Error = ();

    fn try_from(s: &BStr) -> Result<Self, Self::Error> {
        Self::from_str(std::str::from_utf8(s).map_err(|_| ())?)
    }
}

/// `strtoimax(text, &end, 0)` as `git_parse_signed()` calls it (config.c): leading whitespace is
/// skipped, one sign is taken, and the base is read from the prefix — `0x`/`0X` is hex, a leading
/// `0` is octal, anything else decimal. Returns the value and the text after the digits, or `None`
/// where C reports no conversion (`end == value`) or `ERANGE`.
fn strtoimax(text: &str) -> Option<(i64, &str)> {
    let text = text.trim_start_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    let (negative, unsigned) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (radix, digits) = match unsigned.as_bytes() {
        [b'0', b'x' | b'X', next, ..] if next.is_ascii_hexdigit() => (16, &unsigned[2..]),
        [b'0', ..] => (8, unsigned),
        _ => (10, unsigned),
    };
    let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let magnitude = i128::from_str_radix(&digits[..end], radix).ok()?;
    let value = if negative { -magnitude } else { magnitude };
    Some((i64::try_from(value).ok()?, &digits[end..]))
}
