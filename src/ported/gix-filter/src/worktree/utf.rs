//! The UTF-16/UTF-32 half of `working-tree-encoding`, which `encoding_rs` cannot stand in for.
//!
//! git reaches `iconv` through `reencode_string_len()` (`utf8.c:564-622`, v2.55.0) and lets the
//! platform produce whatever it spells `UTF-16`. This crate's usual back-end cannot be asked the
//! same question: `encoding_rs::Encoding::for_label()` folds `UTF-16`, `UTF-16LE` and `UTF-16BE`
//! onto one value, and the encoder that value hands out routes through `output_encoding()`, which
//! answers `UTF-8` for the whole family — so "encode to UTF-16" is a no-op there and the worktree
//! receives UTF-8 bytes under a UTF-16 name. What follows is the four unit/byte-order combinations
//! written out by hand, wrapped in the byte-order-mark rules `reencode_string_len()` puts around
//! them and the ones `validate_encoding()` (`convert.c:269-319`) enforces before either direction
//! runs.
//!
//! ### Deviation
//!
//! git hands the attribute value to `iconv_open()` unchanged for every spelling its own
//! `same_utf_encoding()` does not recognise, so a platform-specific alias such as `UTF_16BE` is
//! still understood there. Here such a spelling falls through to `encoding_rs`, which does not know
//! it, and is reported as an encoding that could not be applied.

use bstr::BStr;

/// The byte-order marks git writes and checks for — `utf8.c:9-12`.
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF32_BE_BOM: &[u8] = &[0x00, 0x00, 0xFE, 0xFF];
const UTF32_LE_BOM: &[u8] = &[0xFF, 0xFE, 0x00, 0x00];

/// The width of one encoded unit.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Unit {
    /// Two bytes per unit, with surrogate pairs for anything above the basic plane.
    Utf16,
    /// Four bytes per unit, one per codepoint.
    Utf32,
}

/// The byte order of an encoded unit.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Order {
    Be,
    Le,
}

/// One of the UTF encodings git names in a `working-tree-encoding` attribute.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Utf {
    pub(crate) unit: Unit,
    /// The byte order to write, or the one to assume when no mark says otherwise.
    pub(crate) order: Order,
    /// Whether a byte-order mark is part of the encoded form.
    pub(crate) bom: bool,
}

impl Utf {
    /// The mark that announces [`Self::order`], or `None` if this form carries none.
    fn bom(&self) -> Option<&'static [u8]> {
        self.bom.then(|| match (self.unit, self.order) {
            (Unit::Utf16, Order::Be) => UTF16_BE_BOM,
            (Unit::Utf16, Order::Le) => UTF16_LE_BOM,
            (Unit::Utf32, Order::Be) => UTF32_BE_BOM,
            (Unit::Utf32, Order::Le) => UTF32_LE_BOM,
        })
    }
}

/// What is left of `name` after a leading `utf` and one optional `-`, or `None` if it does not start
/// with `utf`. This is the halves `same_utf_encoding()` compares — `utf8.c:423-431`.
pub(crate) fn strip_utf(name: &[u8]) -> Option<&[u8]> {
    let rest = name.get(..3).filter(|p| p.eq_ignore_ascii_case(b"utf"))?;
    let rest = &name[rest.len()..];
    Some(rest.strip_prefix(b"-").unwrap_or(rest))
}

/// Port of `same_utf_encoding()` — `utf8.c:423-431`. `UTF-16BE` and `UTF16be` name the same thing.
pub(crate) fn same_utf_encoding(src: &BStr, dst: &str) -> bool {
    match (strip_utf(src), strip_utf(dst.as_bytes())) {
        (Some(src), Some(dst)) => src.eq_ignore_ascii_case(dst),
        _ => false,
    }
}

/// Port of `same_encoding()` — `utf8.c:442-453`.
pub(crate) fn same_encoding(src: &BStr, dst: &str) -> bool {
    same_utf_encoding(src, dst) || src.eq_ignore_ascii_case(dst.as_bytes())
}

/// The form `name` takes on its way *out* to the working tree, or `None` if this is not a UTF
/// encoding we write ourselves.
///
/// `reencode_string_len()` rewrites `UTF-16LE-BOM` and `UTF-16BE-BOM` into the plain
/// byte-order-fixed names and prepends the mark itself (`utf8.c:588-595`); a bare `UTF-16`/`UTF-32`
/// it hands to `iconv`, which per RFC 2781 answers big-endian behind a mark — the same choice git
/// makes for itself where `ICONV_OMITS_BOM` is set (`utf8.c:596-605`). Both were measured against
/// git 2.55.0: `working-tree-encoding=UTF-16` on a blob emits `feff 0066 006e …`.
///
/// `UTF-32LE-BOM` and `UTF-32BE-BOM` have no such rewriting arm, so git passes them to
/// `iconv_open()`, which does not know them — hence `None` here as well.
pub(crate) fn to_worktree(name: &BStr) -> Option<Utf> {
    let (unit, order, bom) = match strip_utf(name)?.to_ascii_lowercase().as_slice() {
        b"16" | b"16be-bom" => (Unit::Utf16, Order::Be, true),
        b"16le-bom" => (Unit::Utf16, Order::Le, true),
        b"16be" => (Unit::Utf16, Order::Be, false),
        b"16le" => (Unit::Utf16, Order::Le, false),
        b"32" => (Unit::Utf32, Order::Be, true),
        b"32be" => (Unit::Utf32, Order::Be, false),
        b"32le" => (Unit::Utf32, Order::Le, false),
        _ => return None,
    };
    Some(Utf { unit, order, bom })
}

/// The form `name` takes on its way *in* from the working tree, or `None` if this is not a UTF
/// encoding we read ourselves.
///
/// Only `UTF-16LE-BOM` is rewritten for reading, and to plain `UTF-16` — "the same as UTF-16 for
/// reading" (`utf8.c:576-578`) — which leaves the byte order to the mark. `UTF-16BE-BOM` has no
/// such arm and reaches `iconv_open()` unchanged, which refuses it; measured on git 2.55.0,
/// `hash-object` with that attribute answers `fatal: failed to encode 'x.bin' from UTF-16BE-BOM to
/// UTF-8`. The `bom` flag here reads as "a mark is expected, and it decides the byte order", with
/// [`Utf::order`] the fallback RFC 2781 names — though `validate_encoding()` has already refused
/// the marked and unmarked forms that do not belong.
pub(crate) fn to_git(name: &BStr) -> Option<Utf> {
    let (unit, order, bom) = match strip_utf(name)?.to_ascii_lowercase().as_slice() {
        b"16" | b"16le-bom" => (Unit::Utf16, Order::Be, true),
        b"16be" => (Unit::Utf16, Order::Be, false),
        b"16le" => (Unit::Utf16, Order::Le, false),
        b"32" => (Unit::Utf32, Order::Be, true),
        b"32be" => (Unit::Utf32, Order::Be, false),
        b"32le" => (Unit::Utf32, Order::Le, false),
        _ => return None,
    };
    Some(Utf { unit, order, bom })
}

/// Port of `has_prohibited_utf_bom()` — `utf8.c:631-644`. A byte-order-fixed name and a mark in the
/// data contradict each other.
pub(crate) fn has_prohibited_utf_bom(enc: &BStr, data: &[u8]) -> bool {
    ((same_utf_encoding(enc, "UTF-16BE") || same_utf_encoding(enc, "UTF-16LE"))
        && (data.starts_with(UTF16_BE_BOM) || data.starts_with(UTF16_LE_BOM)))
        || ((same_utf_encoding(enc, "UTF-32BE") || same_utf_encoding(enc, "UTF-32LE"))
            && (data.starts_with(UTF32_BE_BOM) || data.starts_with(UTF32_LE_BOM)))
}

/// Port of `is_missing_required_utf_bom()` — `utf8.c:646-657`. A bare `UTF-16`/`UTF-32` says nothing
/// about byte order, so the data has to.
pub(crate) fn is_missing_required_utf_bom(enc: &BStr, data: &[u8]) -> bool {
    (same_utf_encoding(enc, "UTF-16")
        && !(data.starts_with(UTF16_BE_BOM) || data.starts_with(UTF16_LE_BOM)))
        || (same_utf_encoding(enc, "UTF-32")
            && !(data.starts_with(UTF32_BE_BOM) || data.starts_with(UTF32_LE_BOM)))
}

/// The reason a working-tree buffer could not be read as `Utf`.
#[derive(Debug, Copy, Clone)]
pub(crate) struct Malformed;

/// Encode `src`, which is UTF-8, as `utf` into `buf`, replacing whatever it held.
///
/// This is the write half of what `iconv` does for the UTF family. It cannot fail: every `str` is a
/// sequence of scalar values and every scalar value has a UTF-16 and a UTF-32 form.
pub(crate) fn encode(src: &str, utf: Utf, buf: &mut Vec<u8>) {
    buf.clear();
    if let Some(bom) = utf.bom() {
        buf.extend_from_slice(bom);
    }
    match utf.unit {
        Unit::Utf16 => {
            for unit in src.encode_utf16() {
                match utf.order {
                    Order::Be => buf.extend_from_slice(&unit.to_be_bytes()),
                    Order::Le => buf.extend_from_slice(&unit.to_le_bytes()),
                }
            }
        }
        Unit::Utf32 => {
            for c in src.chars() {
                let unit = u32::from(c);
                match utf.order {
                    Order::Be => buf.extend_from_slice(&unit.to_be_bytes()),
                    Order::Le => buf.extend_from_slice(&unit.to_le_bytes()),
                }
            }
        }
    }
}

/// Decode `src`, which is `utf`-encoded, to UTF-8 in `buf`, replacing whatever it held.
///
/// This is the read half. `iconv` refuses a truncated unit, an unpaired surrogate or a codepoint
/// outside Unicode with `EILSEQ`/`EINVAL`, which reaches `reencode_string_len()` as a null result;
/// [`Malformed`] stands for all of them.
pub(crate) fn decode(src: &[u8], utf: Utf, buf: &mut Vec<u8>) -> Result<(), Malformed> {
    let (order, body) = split_bom(src, utf);
    buf.clear();
    match utf.unit {
        Unit::Utf16 => {
            let units = body.chunks_exact(2);
            if !units.remainder().is_empty() {
                return Err(Malformed);
            }
            let units = units.map(|u| match order {
                Order::Be => u16::from_be_bytes([u[0], u[1]]),
                Order::Le => u16::from_le_bytes([u[0], u[1]]),
            });
            for c in char::decode_utf16(units) {
                buf.extend_from_slice(c.map_err(|_| Malformed)?.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        Unit::Utf32 => {
            let units = body.chunks_exact(4);
            if !units.remainder().is_empty() {
                return Err(Malformed);
            }
            for u in units {
                let unit = match order {
                    Order::Be => u32::from_be_bytes([u[0], u[1], u[2], u[3]]),
                    Order::Le => u32::from_le_bytes([u[0], u[1], u[2], u[3]]),
                };
                let c = char::from_u32(unit).ok_or(Malformed)?;
                buf.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
    }
    Ok(())
}

/// The byte order `src` is in and the part of it that is content, once a leading mark has had its
/// say. A form that carries no mark keeps every byte and the order its name fixed.
fn split_bom(src: &[u8], utf: Utf) -> (Order, &[u8]) {
    if !utf.bom {
        return (utf.order, src);
    }
    let (be, le, len) = match utf.unit {
        Unit::Utf16 => (UTF16_BE_BOM, UTF16_LE_BOM, 2),
        Unit::Utf32 => (UTF32_BE_BOM, UTF32_LE_BOM, 4),
    };
    if src.starts_with(be) {
        (Order::Be, &src[len..])
    } else if src.starts_with(le) {
        (Order::Le, &src[len..])
    } else {
        (utf.order, src)
    }
}
