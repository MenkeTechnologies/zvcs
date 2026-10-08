use std::{borrow::Cow, ffi::OsString, fmt::Display};

use bstr::{BStr, BString, ByteSlice};

use crate::{Boolean, Error};

fn bool_err(input: impl Into<BString>) -> Error {
    Error::new(
        "Booleans need to be 'no', 'off', 'false', '' or 'yes', 'on', 'true' or any number",
        input,
    )
}

impl TryFrom<OsString> for Boolean {
    type Error = Error;

    fn try_from(value: OsString) -> Result<Self, Self::Error> {
        let value = gix_path::os_str_into_bstr(&value)
            .map_err(|_| Error::new("Illformed UTF-8", std::path::Path::new(&value).display().to_string()))?;
        Self::try_from(value)
    }
}

/// # Warning
///
/// The direct usage of `try_from("string")` is discouraged as it will produce the wrong result for values
/// obtained from `core.bool-implicit-true`, which have no separator and are implicitly true.
/// This method chooses to work correctly for `core.bool-empty=`, which is an empty string and resolves
/// to being `false`.
///
/// Instead of this, obtain booleans with `config.boolean(…)`, which handles the case were no separator is
/// present correctly.
impl TryFrom<&BStr> for Boolean {
    type Error = Error;

    fn try_from(value: &BStr) -> Result<Self, Self::Error> {
        if parse_true(value) {
            Ok(Boolean(true))
        } else if parse_false(value) {
            Ok(Boolean(false))
        } else {
            match parse_git_int(value) {
                Some(integer) => Ok(Boolean(integer != 0)),
                None => Err(bool_err(value)),
            }
        }
    }
}

impl TryFrom<&str> for Boolean {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(BStr::new(value))
    }
}

impl Boolean {
    /// Return true if the boolean is a true value.
    ///
    /// Note that the inner value is accessible directly as well.
    pub fn is_true(self) -> bool {
        self.0
    }
}

impl TryFrom<Cow<'_, BStr>> for Boolean {
    type Error = Error;
    fn try_from(c: Cow<'_, BStr>) -> Result<Self, Self::Error> {
        Self::try_from(c.as_ref())
    }
}

impl TryFrom<BString> for Boolean {
    type Error = Error;
    fn try_from(value: BString) -> Result<Self, Self::Error> {
        Self::try_from(BStr::new(&value))
    }
}

impl Display for Boolean {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Boolean> for bool {
    fn from(b: Boolean) -> Self {
        b.0
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Boolean {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bool(self.0)
    }
}

fn parse_true(value: &BStr) -> bool {
    value.eq_ignore_ascii_case(b"yes") || value.eq_ignore_ascii_case(b"on") || value.eq_ignore_ascii_case(b"true")
}

fn parse_false(value: &BStr) -> bool {
    value.eq_ignore_ascii_case(b"no")
        || value.eq_ignore_ascii_case(b"off")
        || value.eq_ignore_ascii_case(b"false")
        || value.is_empty()
}

/// git's `git_parse_int()` (config.c `git_parse_signed` with an `INT_MAX` ceiling), the number
/// `git_parse_maybe_bool()` falls back to: C `strtoimax` with base 0 — `0x` hex, a leading `0`
/// octal, otherwise decimal — then an optional single `k`/`m`/`g` unit (1024-based) and nothing
/// after it. `None` is its `EINVAL` or `ERANGE`.
fn parse_git_int(value: &BStr) -> Option<i64> {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = bytes.get(i) == Some(&b'-');
    if matches!(bytes.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let base: i64 = if bytes.get(i) == Some(&b'0') {
        if matches!(bytes.get(i + 1), Some(b'x' | b'X')) && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit) {
            i += 2;
            16
        } else {
            8
        }
    } else {
        10
    };
    let digits_start = i;
    let mut magnitude: i64 = 0;
    while let Some(digit) = bytes.get(i).and_then(|b| (*b as char).to_digit(16)) {
        if i64::from(digit) >= base {
            break;
        }
        magnitude = magnitude.checked_mul(base)?.checked_add(i64::from(digit))?;
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    let factor: i64 = match &bytes[i..] {
        b"" => 1,
        b"k" | b"K" => 1 << 10,
        b"m" | b"M" => 1 << 20,
        b"g" | b"G" => 1 << 30,
        _ => return None,
    };
    let limit = i64::from(i32::MAX) / factor;
    if magnitude > limit {
        return None;
    }
    Some(if negative { -magnitude } else { magnitude } * factor)
}
