//! Arbitrary bytes carried losslessly through a Rust `String`.
//!
//! git's `main(int argc, const char **argv)` sees every argument as a
//! NUL-terminated byte string and never asks whether it is UTF-8: a
//! `commit -m` message, a pathspec, a ref name and a `-c` value all reach the
//! object database, the index or the config file byte for byte. The port's
//! verbs take `&[String]`, and `std::env::args()` panics on the first argument
//! that is not UTF-8 — so `git commit -m "$(printf 'caf\xe9')"` died with a Rust
//! panic (exit 101) where git records the byte.
//!
//! Instead of widening every verb's signature, argv is decoded with the scheme
//! below, which is a bijection between byte strings and a subset of `String`:
//!
//! * a well-formed UTF-8 sequence becomes its own `char`, except
//! * any byte that is not part of a well-formed sequence, and every byte of a
//!   well-formed encoding of one of the reserved code points
//!   `U+10FF80..=U+10FFFF`, becomes the reserved code point `U+10FF00 + byte`.
//!
//! Only bytes `0x80..=0xFF` can ever be escaped (ASCII is always well formed), so
//! the 128 reserved code points — private-use, at the very end of plane 16 —
//! cover every case, and because a literal reserved code point in the input is
//! itself escaped byte by byte, [`to_bytes`] recovers the exact input for every
//! string [`from_bytes`] produced. Valid UTF-8 without reserved code points (all
//! real-world text) decodes to the identical `String`, so verbs that never look
//! for raw bytes are unaffected; the ones that hand an argument to git's byte
//! world — a commit message, a ref name, a path — call [`to_bytes`] at that edge.
//!
//! The same pair carries non-UTF-8 file content through code that holds text as
//! `String` — a `commit -F` message in ISO-8859-1, say — so the bytes written
//! back out are the bytes read in.

use std::borrow::Cow;

/// The first code point of the escape range; byte `b` maps to `BASE + b`.
const BASE: u32 = 0x10_FF00;
/// The lowest code point [`from_bytes`] ever emits as an escape.
const RESERVED_LO: u32 = BASE + 0x80;

fn escape(out: &mut String, bytes: &[u8]) {
    for &b in bytes {
        out.push(char::from_u32(BASE + u32::from(b)).expect("U+10FF80..=U+10FFFF are scalar values"));
    }
}

/// Decode `bytes` into the escaped `String` form. Borrows when `bytes` is UTF-8
/// holding no reserved code point, which is the overwhelmingly common case.
pub fn from_bytes(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        // Every reserved code point encodes as `F4 8F BE/BF xx`, so a string
        // without a 0xF4 byte cannot hold one.
        if !bytes.contains(&0xF4) || !s.chars().any(|c| u32::from(c) >= RESERVED_LO) {
            return Cow::Borrowed(s);
        }
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if u32::from(c) >= RESERVED_LO {
                let mut buf = [0u8; 4];
                escape(&mut out, c.encode_utf8(&mut buf).as_bytes());
            } else {
                out.push(c);
            }
        }
        escape(&mut out, chunk.invalid());
    }
    Cow::Owned(out)
}

/// [`from_bytes`] for an owned buffer, without a copy when it is plain UTF-8.
pub fn from_vec(bytes: Vec<u8>) -> String {
    match from_bytes(&bytes) {
        Cow::Borrowed(_) => String::from_utf8(bytes).expect("from_bytes borrowed a valid UTF-8 slice"),
        Cow::Owned(s) => s,
    }
}

/// [`from_bytes`] for one `argv` element.
pub fn from_os(arg: std::ffi::OsString) -> String {
    use std::os::unix::ffi::OsStringExt;
    from_vec(arg.into_vec())
}

/// The bytes a [`from_bytes`] string stands for. Borrows when `s` holds no
/// escape, i.e. whenever it came from valid UTF-8 without reserved code points.
pub fn to_bytes(s: &str) -> Cow<'_, [u8]> {
    if !s.as_bytes().contains(&0xF4) || !s.chars().any(|c| u32::from(c) >= RESERVED_LO) {
        return Cow::Borrowed(s.as_bytes());
    }
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        let v = u32::from(c);
        if v >= RESERVED_LO {
            out.push((v - BASE) as u8);
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    Cow::Owned(out)
}

/// [`to_bytes`] as an `OsString`, for passing an argument on to a child process.
pub fn to_os(s: &str) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(to_bytes(s).into_owned())
}

/// The `argv` of this process, every element decoded with [`from_os`].
pub fn args() -> Vec<String> {
    std::env::args_os().map(from_os).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(b: &[u8]) {
        let s = from_bytes(b);
        assert_eq!(&*to_bytes(&s), b, "{b:?} via {s:?}");
    }

    #[test]
    fn utf8_is_unchanged_and_borrowed() {
        assert!(matches!(from_bytes("café ✓".as_bytes()), Cow::Borrowed("café ✓")));
        assert!(matches!(to_bytes("café ✓"), Cow::Borrowed(_)));
    }

    #[test]
    fn invalid_bytes_and_reserved_code_points_round_trip() {
        round_trip(b"caf\xe9");
        round_trip(b"\x80\xff\xfe");
        round_trip(b"\xf4\x8f\xbe\x80 literal reserved");
        round_trip(b"\xf4\x8f\xbf\xbf\xe9\xf4\x8f\xbe");
        round_trip(b"\xe3\x81");
        // An escaped byte never collides with the literal code point.
        assert_ne!(from_bytes(b"\xe9"), from_bytes("\u{10FFE9}".as_bytes()));
    }
}
