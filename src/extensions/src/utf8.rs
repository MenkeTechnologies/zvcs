//! git's `utf8.c` display-width and wrapping primitives.
//!
//! Ported function for function from git 2.55.0: [`display_mode_esc_sequence_len`],
//! `pick_one_utf8_char`, [`utf8_width`], [`utf8_strnwidth`], [`strbuf_utf8_replace`],
//! `strbuf_add_indented_text` and [`strbuf_add_wrapped_text`]. The column widths come
//! from git's own interval tables (see [`crate::unicode_width`]), because
//! `--pretty=format:%<(<N>)` measures padding in these columns and any drift from
//! git's Unicode revision would show up as a column of difference in aligned output.
//!
//! Where C walks a NUL-terminated `const char *`, this walks a byte slice and
//! treats both the end of the slice and an embedded NUL as the terminator, which
//! is what the C sees.

use crate::unicode_width::{DOUBLE_WIDTH, ZERO_WIDTH};

/// `display_mode_esc_sequence_len()`: the length of the ANSI SGR sequence at the
/// start of `s` (`ESC [ [0-9;]* m`), or 0 when there is none.
pub(crate) fn display_mode_esc_sequence_len(s: &[u8]) -> usize {
    let mut p = 0;
    if s.first() != Some(&0x1b) {
        return 0;
    }
    p += 1;
    if s.get(p) != Some(&b'[') {
        return 0;
    }
    p += 1;
    while matches!(s.get(p), Some(c) if c.is_ascii_digit() || *c == b';') {
        p += 1;
    }
    if s.get(p) != Some(&b'm') {
        return 0;
    }
    p + 1
}

/// `bisearch()` over one of git's interval tables.
fn bisearch(ch: u32, table: &[(u32, u32)]) -> bool {
    table.binary_search_by(|&(first, last)| {
        if ch < first {
            std::cmp::Ordering::Greater
        } else if ch > last {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

/// `git_wcwidth()`: 0 for NUL and the zero-width table, -1 for the other C0/C1
/// controls and DEL, 2 for the double-width table, 1 for everything else.
fn git_wcwidth(ch: u32) -> i32 {
    if ch == 0 {
        return 0;
    }
    if ch < 32 || (0x7f..0xa0).contains(&ch) {
        return -1;
    }
    if bisearch(ch, ZERO_WIDTH) {
        return 0;
    }
    if bisearch(ch, DOUBLE_WIDTH) {
        return 2;
    }
    1
}

/// `pick_one_utf8_char()`: decode the character at `s[*pos]`, advancing `*pos`.
///
/// `None` is C's `*start = NULL` — the bytes are not valid UTF-8, and every
/// caller treats that as "give up on UTF-8 for this buffer". A truncated
/// sequence at the end of the slice is invalid for the same reason it is in C:
/// the continuation byte the decoder needs is the terminator.
fn pick_one_utf8_char(s: &[u8], pos: &mut usize) -> Option<u32> {
    let b = |i: usize| -> u32 { u32::from(*s.get(*pos + i).unwrap_or(&0)) };
    let avail = s.len().saturating_sub(*pos);
    if avail < 1 {
        return None;
    }
    let (ch, incr) = if b(0) < 0x80 {
        (b(0), 1)
    } else if b(0) & 0xe0 == 0xc0 {
        if avail < 2 || b(1) & 0xc0 != 0x80 || b(0) & 0xfe == 0xc0 {
            return None;
        }
        (((b(0) & 0x1f) << 6) | (b(1) & 0x3f), 2)
    } else if b(0) & 0xf0 == 0xe0 {
        if avail < 3
            || b(1) & 0xc0 != 0x80
            || b(2) & 0xc0 != 0x80
            // overlong?
            || (b(0) == 0xe0 && b(1) & 0xe0 == 0x80)
            // surrogate?
            || (b(0) == 0xed && b(1) & 0xe0 == 0xa0)
            // U+FFFE or U+FFFF?
            || (b(0) == 0xef && b(1) == 0xbf && b(2) & 0xfe == 0xbe)
        {
            return None;
        }
        (((b(0) & 0x0f) << 12) | ((b(1) & 0x3f) << 6) | (b(2) & 0x3f), 3)
    } else if b(0) & 0xf8 == 0xf0 {
        if avail < 4
            || b(1) & 0xc0 != 0x80
            || b(2) & 0xc0 != 0x80
            || b(3) & 0xc0 != 0x80
            // overlong?
            || (b(0) == 0xf0 && b(1) & 0xf0 == 0x80)
            // > U+10FFFF?
            || (b(0) == 0xf4 && b(1) > 0x8f)
            || b(0) > 0xf4
        {
            return None;
        }
        (
            ((b(0) & 0x07) << 18) | ((b(1) & 0x3f) << 12) | ((b(2) & 0x3f) << 6) | (b(3) & 0x3f),
            4,
        )
    } else {
        return None;
    };
    *pos += incr;
    Some(ch)
}

/// `utf8_width()`: the column width of the character at `s[*pos]`, advancing
/// `*pos` past it. `None` for invalid UTF-8 (C's `*start = NULL`).
pub(crate) fn utf8_width(s: &[u8], pos: &mut usize) -> Option<i32> {
    pick_one_utf8_char(s, pos).map(git_wcwidth)
}

/// `utf8_strnwidth()`: the columns `s` occupies, skipping ANSI SGR sequences when
/// `skip_ansi`. Invalid UTF-8 answers the byte length, as git does.
pub(crate) fn utf8_strnwidth(s: &[u8], skip_ansi: bool) -> i32 {
    let mut pos = 0usize;
    let mut width: i32 = 0;
    while pos < s.len() {
        if skip_ansi {
            loop {
                let skip = display_mode_esc_sequence_len(&s[pos..]);
                if skip == 0 {
                    break;
                }
                pos += skip;
            }
            if pos >= s.len() {
                break;
            }
        }
        match utf8_width(s, &mut pos) {
            Some(w) => {
                if w > 0 {
                    width += w;
                }
            }
            None => return s.len() as i32,
        }
    }
    width
}

/// `strbuf_utf8_replace()`: replace the glyphs of `src` that occupy columns
/// `pos..pos + width` with a single `subst`, leaving ANSI sequences in place.
/// Broken UTF-8 leaves the buffer untouched, as git's early `goto out` does.
pub(crate) fn strbuf_utf8_replace(src: &[u8], pos: i32, width: i32, subst: &[u8]) -> Vec<u8> {
    let mut dst: Vec<u8> = Vec::with_capacity(src.len());
    let mut at = 0usize;
    let mut w: i32 = 0;
    let mut subst_pending = true;
    while at < src.len() {
        loop {
            let n = display_mode_esc_sequence_len(&src[at..]);
            if n == 0 {
                break;
            }
            dst.extend_from_slice(&src[at..at + n]);
            at += n;
        }
        if at >= src.len() {
            break;
        }
        let old = at;
        let Some(mut glyph_width) = utf8_width(src, &mut at) else {
            // Broken utf-8, do nothing.
            return src.to_vec();
        };
        // A control character is copied through without contributing width.
        if glyph_width < 0 {
            glyph_width = 0;
        }
        if glyph_width != 0 && w >= pos && w < pos + width {
            if subst_pending {
                dst.extend_from_slice(subst);
                subst_pending = false;
            }
        } else {
            dst.extend_from_slice(&src[old..at]);
        }
        w += glyph_width;
    }
    dst
}

/// C's `isspace()` in the "C" locale.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strbuf_add_indented_text()`: the `width <= 0` case of the wrapper — every line
/// gets `indent` spaces, the first from `indent1` and the rest from `indent2`.
fn strbuf_add_indented_text(buf: &mut Vec<u8>, text: &[u8], indent1: i32, indent2: i32) {
    let mut indent = indent1.max(0);
    let mut at = 0usize;
    while at < text.len() && text[at] != 0 {
        // `strchrnul()` stops at the newline or at the terminator, and an embedded
        // NUL is the terminator as far as the C sees.
        let mut eol = at
            + text[at..]
                .iter()
                .position(|&c| c == b'\n' || c == 0)
                .unwrap_or(text.len() - at);
        if text.get(eol) == Some(&b'\n') {
            eol += 1;
        }
        buf.resize(buf.len() + indent as usize, b' ');
        buf.extend_from_slice(&text[at..eol]);
        at = eol;
        indent = indent2.max(0);
    }
}

/// `strbuf_add_wrapped_text()`: word-wrap `text` to `width` columns, indenting the
/// first line by `indent1` and the rest by `indent2`.
///
/// A negative `indent1` means "`-indent1` columns are already consumed". Text that
/// turns out not to be valid UTF-8 is re-wrapped counting bytes, which is git's
/// `assume_utf8` retry.
pub(crate) fn strbuf_add_wrapped_text(
    buf: &mut Vec<u8>,
    text: &[u8],
    indent1: i32,
    indent2: i32,
    width: i32,
) {
    if width <= 0 {
        strbuf_add_indented_text(buf, text, indent1, indent2);
        return;
    }
    let orig_len = buf.len();
    let mut assume_utf8 = true;
    'retry: loop {
        let mut at = 0usize;
        let mut bol = 0usize;
        let mut indent = indent1;
        let mut w = indent1;
        let mut space: Option<usize> = None;
        if indent < 0 {
            w = -indent;
            space = Some(0);
        }
        loop {
            loop {
                let skip = display_mode_esc_sequence_len(&text[at.min(text.len())..]);
                if skip == 0 {
                    break;
                }
                at += skip;
            }
            let c = text.get(at).copied().unwrap_or(0);
            if c == 0 || is_space(c) {
                // git reaches its `new_line:` label both by falling off the end of
                // the fitting branch and by jumping into it from a `%n` run.
                let mut new_line = false;
                if w <= width || space.is_none() {
                    let mut start = bol;
                    if c == 0 && at == start {
                        return;
                    }
                    match space {
                        Some(s) => start = s,
                        None => buf.resize(buf.len() + indent.max(0) as usize, b' '),
                    }
                    buf.extend_from_slice(&text[start..at]);
                    if c == 0 {
                        return;
                    }
                    space = Some(at);
                    if c == b'\t' {
                        w |= 0x07;
                    } else if c == b'\n' {
                        let s = at + 1;
                        space = Some(s);
                        let next = text.get(s).copied().unwrap_or(0);
                        if next == b'\n' {
                            buf.push(b'\n');
                            new_line = true;
                        } else if !next.is_ascii_alphanumeric() {
                            new_line = true;
                        } else {
                            buf.push(b' ');
                        }
                    }
                    if !new_line {
                        w += 1;
                        at += 1;
                    }
                } else {
                    new_line = true;
                }
                if new_line {
                    buf.push(b'\n');
                    let s = space.unwrap_or(0);
                    at = s + usize::from(is_space(text.get(s).copied().unwrap_or(0)));
                    bol = at;
                    space = None;
                    indent = indent2;
                    w = indent2;
                }
                continue;
            }
            if assume_utf8 {
                match utf8_width(text, &mut at) {
                    Some(gw) => w += gw,
                    None => {
                        assume_utf8 = false;
                        buf.truncate(orig_len);
                        continue 'retry;
                    }
                }
            } else {
                w += 1;
                at += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_match_git_wcwidth() {
        // ASCII, Latin-1 accents, CJK (wide), a combining mark, and a control.
        assert_eq!(utf8_strnwidth(b"short", false), 5);
        assert_eq!(utf8_strnwidth("café naïve accents".as_bytes(), false), 18);
        assert_eq!(utf8_strnwidth("日本語".as_bytes(), false), 6);
        // U+0301 COMBINING ACUTE ACCENT contributes no column.
        assert_eq!(utf8_strnwidth("e\u{0301}".as_bytes(), false), 1);
        // Control characters are -1 in git_wcwidth and are not accumulated.
        assert_eq!(utf8_strnwidth(b"a\nb", false), 2);
        // Invalid utf-8 answers the byte length.
        assert_eq!(utf8_strnwidth(&[0xff, 0xfe], false), 2);
    }

    #[test]
    fn ansi_sequences_are_skipped_only_when_asked() {
        let s = b"\x1b[31mred\x1b[m";
        assert_eq!(utf8_strnwidth(s, true), 3);
        // Without skip_ansi the ESC counts as a control (0) and the rest as text.
        assert_eq!(utf8_strnwidth(s, false), 3 + "[31m".len() as i32 + "[m".len() as i32);
        assert_eq!(display_mode_esc_sequence_len(b"\x1b[1;31mx"), 7);
        assert_eq!(display_mode_esc_sequence_len(b"\x1b[1;31"), 0);
        assert_eq!(display_mode_esc_sequence_len(b"x"), 0);
    }

    #[test]
    fn utf8_replace_spans_columns_not_bytes() {
        // git's trunc_right on "first commit subject" at padding 10:
        // replace(padding - 2, len - (padding - 2), "..").
        let got = strbuf_utf8_replace(b"first commit subject", 8, 20 - 8, b"..");
        assert_eq!(got, b"first co..");
        // Wide characters count two columns each: 日本語のサブジェクト wide is 25
        // columns, so trunc_right in a 10-wide field (pos 8, width 25 - 8) keeps
        // the first four glyphs and replaces the other 17 columns with `..`.
        let src = "日本語のサブジェクト wide".as_bytes();
        let got = strbuf_utf8_replace(src, 8, 25 - 8, b"..");
        assert_eq!(got, "日本語の..".as_bytes());
        // A negative position (padding < 2) replaces from the first glyph.
        assert_eq!(strbuf_utf8_replace(b"short", -1, 6, b".."), b"..");
        // Broken utf-8 is left alone.
        assert_eq!(strbuf_utf8_replace(&[0xff, b'a'], 0, 1, b".."), vec![0xff, b'a']);
    }

    #[test]
    fn wrapped_text_matches_git_layout() {
        let mut buf = Vec::new();
        strbuf_add_wrapped_text(&mut buf, b"aaa bbb ccc ddd eee fff ggg", 4, 0, 20);
        assert_eq!(buf, b"    aaa bbb ccc ddd\neee fff ggg");
        // indent2 applies to every line after the first.
        let mut buf = Vec::new();
        strbuf_add_wrapped_text(&mut buf, b"aaa bbb ccc ddd eee fff ggg", 2, 4, 20);
        assert_eq!(buf, b"  aaa bbb ccc ddd\n    eee fff ggg");
        // width <= 0 indents without wrapping.
        let mut buf = Vec::new();
        strbuf_add_wrapped_text(&mut buf, b"one\ntwo\n", 2, 1, 0);
        assert_eq!(buf, b"  one\n two\n");
    }
}
