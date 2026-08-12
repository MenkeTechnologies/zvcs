//! git's `GIT binary patch` payload — `diff.c`'s `emit_binary_diff()` and
//! `emit_binary_diff_body()`, plus `base85.c`'s `encode_85()`.
//!
//! Every `git diff` front-end that accepts `--binary` renders the same payload, so
//! it lives here once rather than in each of them.
//!
//! The payload is a deflate stream in base85, which makes it byte-identical to
//! stock git only if the deflate is. That is why this could not be written before
//! [`gix::zlib::deflate`] transcribed zlib: a `zlib-ng`-lineage encoder produces a
//! valid patch that `git apply` accepts and that no `diff` comparison against git
//! matches.

use gix::zlib::deflate::{compress, Wrap};

/// `base85.c`'s `en85[]`. The decoder below maps each character back to its value;
/// anything outside the alphabet makes a line invalid.
const EN85: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

/// `decode_85()`: five alphabet characters carry four bytes, the last group padded.
///
/// The inverse of [`encode_85`], and what `git apply` reads a `GIT binary patch`
/// with. It lives beside the encoder so the two cannot drift apart.
pub(crate) fn decode_base85(input: &[u8], want: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(want);
    let mut left = want;
    let mut chunk = input.chunks(5);
    while left > 0 {
        let group = chunk.next()?;
        if group.len() != 5 {
            return None;
        }
        let mut acc: u32 = 0;
        for &ch in group {
            let de = EN85.iter().position(|c| *c == ch)? as u32;
            acc = acc.checked_mul(85)?.checked_add(de)?;
        }
        // git emits the four bytes most-significant first by rotating the
        // accumulator a byte at a time.
        let take = left.min(4);
        for _ in 0..take {
            acc = acc.rotate_left(8);
            out.push(acc as u8);
        }
        left -= take;
    }
    Some(out)
}

/// `encode_85()`: four input bytes big-endian into one 32-bit accumulator, then
/// five base-85 digits most-significant first. A short final group is padded with
/// zero bytes but still emits all five digits, so the decoder needs the byte count
/// the line header carries.
fn encode_85(out: &mut Vec<u8>, data: &[u8]) {
    for group in data.chunks(4) {
        let mut acc: u32 = 0;
        for (i, b) in group.iter().enumerate() {
            acc |= u32::from(*b) << (24 - 8 * i);
        }
        let mut digits = [0u8; 5];
        for slot in digits.iter_mut().rev() {
            *slot = EN85[(acc % 85) as usize];
            acc /= 85;
        }
        out.extend_from_slice(&digits);
    }
}

/// `emit_binary_diff_body()`: one direction of the patch.
///
/// `two` is what this block reconstructs and `one` is what it reconstructs it from.
/// git deflates the literal post-image, tries a delta against `one`, deflates that
/// too, and keeps whichever came out smaller — literal on a tie.
fn emit_body(out: &mut Vec<u8>, one: &[u8], two: &[u8], level: i32) {
    let deflated = compress(two, level, Wrap::Zlib);

    // `diff_delta(one, two, ..., max_delta_size = deflate_size)`. Comparing the raw
    // delta against the *deflated* literal is git's own conservative bound; the real
    // choice is made below, once the delta has been deflated as well.
    let delta = if one.is_empty() || two.is_empty() {
        None
    } else {
        gix::odb::pack::data::output::delta::Index::new(one)
            .and_then(|index| index.create(two, deflated.len() as u64))
    };

    // `<n>` is the *uncompressed* length in both forms: the delta's for `delta`, the
    // post-image's for `literal`.
    let (header, size, data) = match delta {
        Some(delta) => {
            let raw_len = delta.len();
            let deflated_delta = compress(&delta, level, Wrap::Zlib);
            if deflated_delta.len() < deflated.len() {
                ("delta ", raw_len, deflated_delta)
            } else {
                ("literal ", two.len(), deflated)
            }
        }
        None => ("literal ", two.len(), deflated),
    };

    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(size.to_string().as_bytes());
    out.push(b'\n');

    // 52 raw bytes per line, prefixed by a length character: 'A'..'Z' for 1..=26
    // bytes, 'a'..'z' for 27..=52.
    for line in data.chunks(52) {
        let n = line.len();
        out.push(if n <= 26 {
            b'A' + (n as u8) - 1
        } else {
            b'a' + (n as u8) - 27
        });
        encode_85(out, line);
        out.push(b'\n');
    }
    out.push(b'\n');
}

/// `emit_binary_diff()`: the header, the forward block, then the reverse block that
/// `git apply -R` uses.
///
/// `level` is git's `zlib_compression_level` — `core.looseCompression`, then
/// `core.compression`, then `Z_BEST_SPEED`. Note this is the *loose* level, not
/// `pack.compression`.
pub(crate) fn emit(out: &mut Vec<u8>, one: &[u8], two: &[u8], level: i32) {
    out.extend_from_slice(b"GIT binary patch\n");
    emit_body(out, one, two, level);
    emit_body(out, two, one, level);
}

/// git's `zlib_compression_level`: `core.looseCompression`, else `core.compression`,
/// else `Z_BEST_SPEED`. Public for the front-ends that resolve it once per run.
///
/// An out-of-range configured value is ignored rather than fatal, matching how the
/// other compression-level readers in this port treat one.
pub(crate) fn loose_compression_level(repo: &gix::Repository) -> i32 {
    let snapshot = repo.config_snapshot();
    let configured = snapshot
        .integer("core.looseCompression")
        .or_else(|| snapshot.integer("core.compression"));
    match configured {
        // git maps a configured -1 to Z_DEFAULT_COMPRESSION, which zlib reads as 6.
        Some(-1) => 6,
        Some(level) if (0..=9).contains(&level) => level as i32,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line lifted verbatim from stock git 2.55.0's `git diff --binary` over the
    /// parity `patches` fixture:
    ///
    /// ```text
    /// delta 14
    /// VcmZqRXyDj<p2?7LGe6^bCIBMd1X}<A
    /// ```
    ///
    /// `V` announces 22 payload bytes, which are the level-1 zlib stream of the
    /// 14-byte delta below. Encoding those 22 bytes has to reproduce the line, which
    /// pins the alphabet, the big-endian grouping and the digit order all at once —
    /// the three things an ASCII-85 variant can plausibly get wrong.
    const STOCK_LINE: &str = "cmZqRXyDj<p2?7LGe6^bCIBMd1X}<A";
    const STOCK_PAYLOAD: [u8; 22] = [
        0x78, 0x01, 0x6b, 0xe0, 0x68, 0xe0, 0xd8, 0x7c, 0x9e, 0xc9, 0x90, 0x71, 0x33, 0x3f, 0xe3,
        0x79, 0x26, 0x00, 0x22, 0xde, 0x04, 0x5b,
    ];
    const STOCK_DELTA: [u8; 14] = [
        0x80, 0x08, 0x80, 0x08, 0xb3, 0xcf, 0x02, 0x31, 0x01, 0xb3, 0x0f, 0x01, 0xcf, 0x02,
    ];

    #[test]
    fn encode_85_reproduces_a_stock_git_line() {
        let mut out = Vec::new();
        encode_85(&mut out, &STOCK_PAYLOAD);
        assert_eq!(String::from_utf8(out).unwrap(), STOCK_LINE);
    }

    /// The payload that line carries is the delta deflated at `Z_BEST_SPEED`, which
    /// is git's default `zlib_compression_level` — so the whole chain from raw delta
    /// to armoured text is reproduced, not just the armour.
    #[test]
    fn the_payload_is_the_delta_deflated_at_best_speed() {
        assert_eq!(compress(&STOCK_DELTA, 1, Wrap::Zlib), STOCK_PAYLOAD);
    }

    /// `encode_85()` pads a short final group with zero bytes but still emits five
    /// digits, so the byte count has to come from the line header rather than the
    /// digit count. Cross-checked against the decoder `git apply` uses, which is an
    /// independent implementation.
    #[test]
    fn every_length_round_trips_through_the_apply_decoder() {
        let data: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        for len in 0..=data.len().min(200) {
            let mut encoded = Vec::new();
            encode_85(&mut encoded, &data[..len]);
            assert_eq!(encoded.len(), len.div_ceil(4) * 5, "{len} bytes fills whole groups");
            let decoded = decode_base85(&encoded, len)
                .unwrap_or_else(|| panic!("{len} bytes decode"));
            assert_eq!(decoded, &data[..len], "{len} bytes round-trip");
        }
    }

    /// `emit_binary_diff_body()`'s line framing: at most 52 raw bytes per line, a
    /// length character of `'A'..='Z'` for 1..=26 and `'a'..='z'` for 27..=52, and one
    /// blank line closing each of the two blocks.
    #[test]
    fn line_framing_matches_emit_binary_diff_body() {
        // Incompressible, so the literal form wins and the payload length is
        // predictable enough to check the chunking on.
        let mut s: u32 = 0x1234_5678;
        let two: Vec<u8> = (0..500)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s >> 24) as u8
            })
            .collect();

        let mut out = Vec::new();
        emit(&mut out, &[], &two, 1);
        let text = String::from_utf8(out).expect("base85 is ASCII");
        let mut lines = text.split('\n');

        assert_eq!(lines.next(), Some("GIT binary patch"));
        // An empty `one` means no delta is attempted, so this is the literal form and
        // `<n>` is the post-image length.
        assert_eq!(lines.next(), Some(&format!("literal {}", two.len())[..]));

        let mut payload = 0usize;
        for line in lines.by_ref() {
            if line.is_empty() {
                break;
            }
            let n = match line.as_bytes()[0] {
                c @ b'A'..=b'Z' => usize::from(c - b'A') + 1,
                c @ b'a'..=b'z' => usize::from(c - b'a') + 27,
                c => panic!("bad length character {c:?}"),
            };
            assert!(n <= 52, "at most 52 raw bytes per line");
            assert_eq!(line.len() - 1, n.div_ceil(4) * 5, "digits match the byte count");
            payload += n;
        }
        assert_eq!(payload, compress(&two, 1, Wrap::Zlib).len(), "every payload byte is armoured");

        // The reverse block reconstructs `one`, which is empty here.
        assert_eq!(lines.next(), Some("literal 0"));
    }

    /// git picks the delta only when it deflates *smaller* than the literal, and ties
    /// go to the literal.
    #[test]
    fn a_near_identical_pair_uses_the_delta_form() {
        let mut one = vec![0u8; 8192];
        for (i, b) in one.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut two = one.clone();
        two[4000] ^= 0xff;

        let mut out = Vec::new();
        emit(&mut out, &one, &two, 1);
        let text = String::from_utf8(out).expect("base85 is ASCII");
        assert!(
            text.lines().nth(1).is_some_and(|l| l.starts_with("delta ")),
            "a one-byte change should delta, got: {:?}",
            text.lines().nth(1)
        );

        // An unrelated pair has nothing to copy, so the literal wins.
        let three: Vec<u8> = (0..8192u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        let mut out = Vec::new();
        emit(&mut out, &one, &three, 1);
        let text = String::from_utf8(out).expect("base85 is ASCII");
        assert!(
            text.lines().nth(1).is_some_and(|l| l.starts_with("literal ")),
            "an unrelated pair should not delta, got: {:?}",
            text.lines().nth(1)
        );
    }
}
