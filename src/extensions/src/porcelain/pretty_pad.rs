//! git's `pretty.c` column padding (`%<`/`%>`/`%><`/`%>>`) and line wrapping (`%w`).
//!
//! Ported from git 2.55.0: [`PadState`] is the `flush_type`/`truncate`/`padding`
//! triple of `struct format_commit_context`, [`PadState::parse`] is
//! `parse_padding_placeholder()`, [`PadState::apply`] is the second half of
//! `format_and_pad_commit()`, and [`WrapState::rewrap_message_tail`] is
//! `rewrap_message_tail()` plus `strbuf_wrap()`.
//!
//! The state is *deferred*: a padding placeholder sets it and expands to nothing,
//! and the next placeholder's output is what gets padded — literal text in
//! between passes through untouched and leaves the state pending. Widths are
//! display columns (`utf8_strnwidth`), not bytes, so a CJK subject consumes two
//! columns per glyph.

use crate::utf8::{display_mode_esc_sequence_len, strbuf_utf8_replace, utf8_strnwidth};

/// `FORMATTING_LIMIT` (pretty.c:31): the largest width a format may ask for, so a
/// format string cannot make git allocate arbitrarily much.
const FORMATTING_LIMIT: i64 = 16 * 1024;

/// One code unit of a format string.
///
/// Every character these atoms are built from — `<`, `>`, `|`, `(`, `)`, `,`, the
/// digits, the `trunc` keywords — is ASCII, so the parser can walk a `&[char]`
/// (what `log` and `show` index by) or a `&[u8]` (what `shortlog`, `reflog` and
/// `format-rev` index by) and return a count in whichever unit the caller uses.
/// A non-ASCII unit matches no syntax character in either representation — a
/// UTF-8 continuation byte is never `,` or `)` — so both walks agree on where an
/// atom ends. git parses `const char *`, which is the `u8` case.
pub(crate) trait Unit: Copy {
    /// The ASCII byte this unit stands for, or `None` when it is not ASCII.
    fn ascii(self) -> Option<u8>;
}

impl Unit for char {
    fn ascii(self) -> Option<u8> {
        self.is_ascii().then_some(self as u8)
    }
}

impl Unit for u8 {
    fn ascii(self) -> Option<u8> {
        self.is_ascii().then_some(self)
    }
}

/// `units[at]` as an ASCII byte, or `None` past the end or on a non-ASCII unit.
fn at<U: Unit>(units: &[U], i: usize) -> Option<u8> {
    units.get(i).and_then(|u| u.ascii())
}

/// `strcspn(units + from, set)`: the offset from `from` of the first unit in
/// `set`, or `None` when the string ends first (C leaves the cursor on the NUL,
/// which every caller here rejects).
fn find<U: Unit>(units: &[U], from: usize, set: &[u8]) -> Option<usize> {
    units
        .get(from..)?
        .iter()
        .position(|u| u.ascii().is_some_and(|b| set.contains(&b)))
}

/// Which side of the field the padded text sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FlushType {
    /// No padding is pending.
    None,
    /// `%<`: text left, spaces right.
    Right,
    /// `%>`: spaces left, text right.
    Left,
    /// `%>>`: like `%>`, but trailing spaces already in the buffer are eaten first.
    LeftAndSteal,
    /// `%><`: text centered.
    Both,
}

/// What to do when the text is wider than the field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TruncType {
    /// No `,trunc` modifier: the text overflows the field in full.
    None,
    /// `,ltrunc`: `..` replaces the head.
    Left,
    /// `,mtrunc`: `..` replaces the middle.
    Middle,
    /// `,trunc`: `..` replaces the tail.
    Right,
}

/// The pending padding request a `%<`/`%>` placeholder leaves behind.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PadState {
    pub(crate) flush: FlushType,
    pub(crate) trunc: TruncType,
    /// The field width. Negative means "pad *to* this column" (`%<|`), which is
    /// resolved against what the line already holds when the padding is applied.
    pub(crate) padding: i32,
}

impl Default for PadState {
    fn default() -> Self {
        PadState { flush: FlushType::None, trunc: TruncType::None, padding: 0 }
    }
}

/// git's `term_columns()` without the `TIOCGWINSZ` probe, which the vendored
/// crates do not expose: `COLUMNS` when it parses to a positive number, else 80.
/// Only `%<|(-<N>)` (pad to `N` columns from the right edge) reads it.
fn term_columns() -> i64 {
    if let Ok(value) = std::env::var("COLUMNS") {
        // atoi(): leading digits, no error for trailing junk.
        let digits: String = value
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '+')
            .collect();
        if let Ok(n) = digits.parse::<i64>() {
            if n > 0 {
                return n;
            }
        }
    }
    80
}

/// `strtol(.., 10)`: optional whitespace, optional sign, then digits. Returns the
/// value and how many units were consumed (0 when no conversion happened, which
/// is C's `next == start`). Overflow saturates, as `strtol` does at
/// `LONG_MAX`/`LONG_MIN` — every caller compares against `FORMATTING_LIMIT`, so a
/// saturated value is rejected either way.
///
/// The leading run is C's `isspace()` in the "C" locale — space, `\t`, `\n`,
/// `\v`, `\f`, `\r` — and nothing else. Notably not `char::is_whitespace`, whose
/// Unicode set would skip a NO-BREAK SPACE that git's byte-wise `strtol` stops
/// dead on.
fn strtol<U: Unit>(units: &[U]) -> (i64, usize) {
    let mut i = 0;
    while matches!(at(units, i), Some(b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')) {
        i += 1;
    }
    let negative = match at(units, i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let first_digit = i;
    let mut value: i64 = 0;
    while let Some(d) = at(units, i).and_then(|c| (c as char).to_digit(10)) {
        value = value.saturating_mul(10).saturating_add(i64::from(d));
        i += 1;
    }
    if i == first_digit {
        return (0, 0);
    }
    (if negative { -value } else { value }, i)
}

impl PadState {
    /// `parse_padding_placeholder()`: read a `%<`/`%>` column atom whose `<`/`>`
    /// sits at `units[pos]`, recording the request in `self`. Returns how many
    /// units the placeholder consumed, or `None` when this is not one — in which
    /// case git prints the `%` literally and rescans from `units[pos]`.
    ///
    /// A malformed truncation modifier (`%<(20,bogus)`) answers `None` *after* the
    /// width and flush type have already been stored, which is git's own ordering:
    /// the atom prints literally and the padding still applies to the next
    /// placeholder.
    pub(crate) fn parse<U: Unit>(&mut self, units: &[U], pos: usize) -> Option<usize> {
        self.parse_reporting(units, pos).0
    }

    /// [`PadState::parse`], additionally reporting whether the atom *stored* a
    /// request into `self` — that is, whether it reached
    /// `c->padding = …; c->flush_type = …`.
    ///
    /// The two answers differ only for a malformed truncation modifier, which
    /// stores the width and flush type and *then* reports failure. A caller that
    /// streams the format can ignore the distinction, because the running state is
    /// the same object git mutates. A caller that resolves the format once and
    /// replays snapshots per record cannot: [`PadState::apply`] clears `flush`
    /// when it lays a field out, so replaying a snapshot from an atom that never
    /// stored one would reopen a field git had already closed.
    ///
    /// Equality against the prior state is not a substitute — `%<(9,trunc)` then
    /// `%<(9,bogus)` stores the identical triple the second time, and that store
    /// still has to reopen the field.
    pub(crate) fn parse_reporting<U: Unit>(
        &mut self,
        units: &[U],
        pos: usize,
    ) -> (Option<usize>, bool) {
        let mut stored = false;
        let consumed = self.parse_impl(units, pos, &mut stored);
        (consumed, stored)
    }

    /// `parse_padding_placeholder()` itself. `stored` is set the moment the width
    /// and flush type are written, which is before the truncation modifier is
    /// validated.
    fn parse_impl<U: Unit>(&mut self, units: &[U], pos: usize, stored: &mut bool) -> Option<usize> {
        let mut ch = pos;
        let flush = match at(units, ch)? {
            b'<' => {
                ch += 1;
                FlushType::Right
            }
            b'>' => {
                ch += 1;
                match at(units, ch) {
                    Some(b'<') => {
                        ch += 1;
                        FlushType::Both
                    }
                    Some(b'>') => {
                        ch += 1;
                        FlushType::LeftAndSteal
                    }
                    _ => FlushType::Left,
                }
            }
            _ => return None,
        };

        // The next value means "wide enough to that column".
        let mut to_column = false;
        if at(units, ch) == Some(b'|') {
            to_column = true;
            ch += 1;
        }
        if at(units, ch) != Some(b'(') {
            return None;
        }

        let start = ch + 1;
        // `strcspn(start, ",)")`: an unterminated atom leaves `end` on the NUL,
        // which git rejects.
        let mut end = start + find(units, start, b",)")?;
        if end == start {
            return None;
        }
        let (mut width, consumed) = strtol(units.get(start..)?);
        // Cap the padding, or a format could pad the buffer by arbitrarily many
        // bytes and exhaust memory.
        if !(-FORMATTING_LIMIT..=FORMATTING_LIMIT).contains(&width) {
            return None;
        }
        if consumed == 0 || width == 0 {
            return None;
        }
        if width < 0 {
            if to_column {
                width += term_columns();
            }
            if width < 0 {
                return None;
            }
        }
        let width = width as i32;
        self.padding = if to_column { -width } else { width };
        self.flush = flush;
        *stored = true;

        if at(units, end) == Some(b',') {
            let start = end + 1;
            end = start + find(units, start, b")")?;
            if end == start {
                return None;
            }
            // `skip_prefix(&next, "trunc)", …)`: the keyword and its closing paren
            // in one comparison, so `,truncX)` is not a truncation modifier.
            // Non-ASCII units cannot equal any keyword byte, so `0xff` stands in.
            let modifier: Vec<u8> =
                units[start..=end].iter().map(|u| u.ascii().unwrap_or(0xff)).collect();
            self.trunc = if modifier.starts_with(b"trunc)") {
                TruncType::Right
            } else if modifier.starts_with(b"ltrunc)") {
                TruncType::Left
            } else if modifier.starts_with(b"mtrunc)") {
                TruncType::Middle
            } else {
                return None;
            };
        } else {
            self.trunc = TruncType::None;
        }

        Some(end - pos + 1)
    }

    /// The second half of `format_and_pad_commit()`: `local` is what the
    /// placeholder produced (git's `local_sb`), and this lays it out in the pending
    /// field at the end of `sb`, clearing the request.
    ///
    /// `padding` is the field width **as it was before the placeholder expanded** —
    /// git reads `c->padding` into a local at the top of `format_and_pad_commit()`,
    /// so a nested `%<(…)` inside the padded placeholder changes the state for the
    /// *next* field, not this one. The flush and truncation modes, by contrast, are
    /// read afterwards and so do see a nested change, which this mirrors.
    ///
    /// `graph_width` is `pretty_ctx->graph_width` — the columns `--graph` will put
    /// in front of this line, which count against a `%<|` column target.
    pub(crate) fn apply(
        &mut self,
        sb: &mut Vec<u8>,
        local: Vec<u8>,
        padding: i32,
        graph_width: i32,
    ) {
        let mut local = local;
        let mut padding = padding;
        if padding < 0 {
            // Pad to a column: subtract what the current line already holds.
            let start = match sb.iter().rposition(|&b| b == b'\n') {
                Some(p) => &sb[p..],
                None => &sb[..],
            };
            let occupied = utf8_strnwidth(start, true) + graph_width;
            padding = -padding - occupied;
        }
        let len = utf8_strnwidth(&local, true);

        if self.flush == FlushType::LeftAndSteal {
            let mut ch: isize = sb.len() as isize - 1;
            while len > padding && ch > 0 {
                if sb[ch as usize] == b' ' {
                    ch -= 1;
                    padding += 1;
                    continue;
                }
                // Check for trailing ansi sequences.
                if sb[ch as usize] != b'm' {
                    break;
                }
                let mut p = ch - 1;
                while p > 0 && ch - p < 10 && sb[p as usize] != 0x1b {
                    p -= 1;
                }
                if sb[p as usize] != 0x1b
                    || (ch + 1 - p) as usize != display_mode_esc_sequence_len(&sb[p as usize..])
                {
                    break;
                }
                // Got a good ansi sequence: put it back into the local buffer, as
                // the bytes it sits in are about to be cut.
                let seq: Vec<u8> = sb[p as usize..=ch as usize].to_vec();
                local.splice(0..0, seq);
                ch = p - 1;
            }
            sb.truncate((ch + 1).max(0) as usize);
            self.flush = FlushType::Left;
        }

        if len > padding {
            local = match self.trunc {
                TruncType::Left => strbuf_utf8_replace(&local, 0, len - (padding - 2), b".."),
                TruncType::Middle => {
                    strbuf_utf8_replace(&local, padding / 2 - 1, len - (padding - 2), b"..")
                }
                TruncType::Right => {
                    strbuf_utf8_replace(&local, padding - 2, len - (padding - 2), b"..")
                }
                TruncType::None => local,
            };
            sb.extend_from_slice(&local);
        } else {
            let sb_len = sb.len();
            let offset = match self.flush {
                FlushType::Left => padding - len,
                FlushType::Both => (padding - len) / 2,
                _ => 0,
            } as usize;
            // The padding was measured in columns; the buffer is bytes, so the
            // run of spaces has to cover the text's byte length instead.
            let fill = (padding - len) as usize + local.len();
            sb.resize(sb_len + fill, b' ');
            sb[sb_len + offset..sb_len + offset + local.len()].copy_from_slice(&local);
        }
        self.flush = FlushType::None;
    }
}

/// The `%w(<width>,<indent1>,<indent2>)` wrapping state — git's `width`,
/// `indent1`, `indent2` and `wrap_start` in `struct format_commit_context`.
#[derive(Clone, Copy, Default)]
pub(crate) struct WrapState {
    width: usize,
    indent1: usize,
    indent2: usize,
    /// Where in the output buffer the text governed by the current setting starts.
    wrap_start: usize,
}

impl WrapState {
    /// `rewrap_message_tail()`: switching wrap parameters first wraps everything
    /// written under the old ones. A no-op when nothing changed, which is why the
    /// closing call with `(0, 0, 0)` leaves an unwrapped format alone.
    pub(crate) fn rewrap_message_tail(
        &mut self,
        sb: &mut Vec<u8>,
        new_width: usize,
        new_indent1: usize,
        new_indent2: usize,
    ) {
        if self.width == new_width && self.indent1 == new_indent1 && self.indent2 == new_indent2 {
            return;
        }
        if self.wrap_start < sb.len() {
            strbuf_wrap(sb, self.wrap_start, self.width, self.indent1, self.indent2);
        }
        self.wrap_start = sb.len();
        self.width = new_width;
        self.indent1 = new_indent1;
        self.indent2 = new_indent2;
    }

    /// Parse a `%w(...)` atom whose `w` sits at `units[pos]` and apply it,
    /// returning how many units it consumed. `None` when it is not a well-formed
    /// `%w(…)`, which git prints literally.
    pub(crate) fn parse_and_apply<U: Unit>(
        &mut self,
        sb: &mut Vec<u8>,
        units: &[U],
        pos: usize,
    ) -> Option<usize> {
        let (consumed, width, indent1, indent2) = parse_wrap(units, pos)?;
        self.rewrap_message_tail(sb, width, indent1, indent2);
        Some(consumed)
    }
}

/// The parse half of a `%w(<width>,<indent1>,<indent2>)` atom whose `w` sits at
/// `units[pos]`: the units it consumes and the three parameters it asks for.
/// `None` for anything git would print literally.
///
/// Split out from [`WrapState::parse_and_apply`] because `format-rev` parses its
/// format once into an item list and applies the atoms per record, so the two
/// halves run at different times there.
pub(crate) fn parse_wrap<U: Unit>(
    units: &[U],
    pos: usize,
) -> Option<(usize, usize, usize, usize)> {
    if at(units, pos + 1) != Some(b'(') {
        return None;
    }
    let start = pos + 2;
    let end = start + find(units, start, b")")?;
    let (mut width, mut indent1, mut indent2) = (0i64, 0i64, 0i64);
    if end > start {
        // `strtoul` three times, each separated by a comma; anything else before
        // the `)` makes the whole atom literal.
        let mut cur = start;
        let (w, used) = strtoul(units.get(cur..)?);
        width = w;
        cur += used;
        if at(units, cur) == Some(b',') {
            let (v, used) = strtoul(units.get(cur + 1..)?);
            indent1 = v;
            cur += 1 + used;
            if at(units, cur) == Some(b',') {
                let (v, used) = strtoul(units.get(cur + 1..)?);
                indent2 = v;
                cur += 1 + used;
            }
        }
        if at(units, cur) != Some(b')') {
            return None;
        }
    }
    // Limit the format, or it could prepend arbitrarily many bytes when
    // rewrapping.
    if width > FORMATTING_LIMIT || indent1 > FORMATTING_LIMIT || indent2 > FORMATTING_LIMIT {
        return None;
    }
    Some((end - pos + 1, width as usize, indent1 as usize, indent2 as usize))
}

/// `strtoul(.., 10)`: like [`strtol`] but the C function wraps a negative value
/// around into an enormous unsigned one, which every caller then rejects against
/// `FORMATTING_LIMIT`. `i64::MAX` stands in for that.
fn strtoul<U: Unit>(units: &[U]) -> (i64, usize) {
    let (value, used) = strtol(units);
    if value < 0 {
        return (i64::MAX, used);
    }
    (value, used)
}

/// `strbuf_wrap()`: re-wrap `sb[pos..]` in place, keeping the head untouched.
fn strbuf_wrap(sb: &mut Vec<u8>, pos: usize, width: usize, indent1: usize, indent2: usize) {
    let mut tmp: Vec<u8> = Vec::with_capacity(sb.len());
    if pos > 0 {
        tmp.extend_from_slice(&sb[..pos]);
    }
    crate::utf8::strbuf_add_wrapped_text(
        &mut tmp,
        &sb[pos..],
        indent1 as i32,
        indent2 as i32,
        width as i32,
    );
    *sb = tmp;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(fmt: &str) -> (Option<usize>, PadState) {
        let chars: Vec<char> = fmt.chars().collect();
        let mut st = PadState::default();
        let n = st.parse(&chars, 0);
        (n, st)
    }

    #[test]
    fn parses_every_flush_and_truncation_form() {
        let (n, st) = parse("<(20)");
        assert_eq!(n, Some(5));
        assert_eq!((st.flush, st.trunc, st.padding), (FlushType::Right, TruncType::None, 20));
        let (n, st) = parse(">(20)");
        assert_eq!(n, Some(5));
        assert_eq!(st.flush, FlushType::Left);
        let (n, st) = parse("><(20)");
        assert_eq!(n, Some(6));
        assert_eq!(st.flush, FlushType::Both);
        let (n, st) = parse(">>(20)");
        assert_eq!(n, Some(6));
        assert_eq!(st.flush, FlushType::LeftAndSteal);
        // `|` makes the width a column target, stored negated.
        let (n, st) = parse("<|(20)");
        assert_eq!(n, Some(6));
        assert_eq!(st.padding, -20);
        for (fmt, want) in [
            ("<(10,trunc)", TruncType::Right),
            ("<(10,ltrunc)", TruncType::Left),
            ("<(10,mtrunc)", TruncType::Middle),
        ] {
            let (n, st) = parse(fmt);
            assert_eq!(n, Some(fmt.len()), "{fmt}");
            assert_eq!(st.trunc, want, "{fmt}");
        }
    }

    #[test]
    fn rejects_what_git_prints_literally() {
        // Width 0, a missing width, a bare atom, an unterminated paren, a
        // non-decimal width, and a negative width without `|` are all refused.
        for fmt in ["<(0)", "<()", "<", "<(20", "<(0x10)", "<(-5)", "<(20000)", "^(4)"] {
            assert_eq!(parse(fmt).0, None, "{fmt}");
        }
        // strtol stops at the first non-digit, and git never checks that it
        // reached the `)`, so trailing junk is accepted.
        assert_eq!(parse("<(20abc)").0, Some(8));
        assert_eq!(parse("<( 20)").0, Some(6));
        assert_eq!(parse("<(+20)").0, Some(6));
    }

    /// The width is read by C's `strtol`, whose leading-whitespace run is
    /// `isspace()` in the "C" locale. Both expectations were captured from stock
    /// git 2.55.0:
    ///
    /// ```text
    /// $ git log -1 --format="[%<($(printf '\xc2\xa0')20)%s]"   # U+00A0
    /// [%<( 20)é combining test]                                 # atom rejected
    /// $ git log -1 --format="[%<($(printf '\013')20)%s]"        # vertical tab
    /// [é combining test    ]                                    # atom accepted
    /// ```
    ///
    /// `char::is_whitespace` would accept the NO-BREAK SPACE, because its set is
    /// Unicode's; `u8::is_ascii_whitespace` would reject the vertical tab,
    /// because its set omits `\v`. Neither is `isspace()`.
    #[test]
    fn width_whitespace_is_c_isspace_not_unicode() {
        // U+00A0 is whitespace to Unicode but not to `strtol`: no conversion
        // happens, so the atom is not a placeholder at all.
        assert_eq!(parse("<(\u{a0}20)").0, None);
        // `\v` is whitespace to `strtol` but not to `is_ascii_whitespace`.
        let (n, st) = parse("<(\u{b}20)");
        assert_eq!(n, Some(6));
        assert_eq!(st.padding, 20);
        // The remaining four of C's six, for completeness.
        for ws in [' ', '\t', '\n', '\u{c}', '\r'] {
            assert_eq!(parse(&format!("<({ws}20)")).0, Some(6), "{ws:?}");
        }
    }

    /// The parser walks `&[char]` for `log`/`show` and `&[u8]` for `shortlog`,
    /// `reflog` and `format-rev`. Both must agree on the atom, and each must
    /// report its count in its own units — which differ the moment a multi-byte
    /// character follows the atom.
    #[test]
    fn byte_and_char_walks_agree() {
        // No `%<|(-N)` here: that form reads `COLUMNS`, which
        // `column_target_reads_columns_env` mutates in the same process.
        for fmt in ["<(20)", ">>(6,trunc)", "<|(20)", "><(9,mtrunc)", "<(0)", "<(20,bogus)"] {
            let chars: Vec<char> = fmt.chars().collect();
            let mut a = PadState::default();
            let mut b = PadState::default();
            let (na, sa) = a.parse_reporting(&chars, 0);
            let (nb, sb) = b.parse_reporting(fmt.as_bytes(), 0);
            assert_eq!(na, nb, "{fmt}");
            assert_eq!(sa, sb, "{fmt}");
            assert_eq!((a.flush, a.trunc, a.padding), (b.flush, b.trunc, b.padding), "{fmt}");
        }
        // A wide character after the atom: the counts are in different units, so
        // each caller can index with what it already has.
        let fmt = "<(4)\u{65e5}";
        let chars: Vec<char> = fmt.chars().collect();
        assert_eq!(PadState::default().parse(&chars, 0), Some(4));
        assert_eq!(PadState::default().parse(fmt.as_bytes(), 0), Some(4));
        // …and a wide character *inside* the atom is rejected identically, since
        // no continuation byte can be mistaken for `,` or `)`.
        let fmt = "<(2\u{65e5}0)";
        let chars: Vec<char> = fmt.chars().collect();
        assert_eq!(PadState::default().parse(&chars, 0), Some(chars.len()));
        assert_eq!(PadState::default().parse(fmt.as_bytes(), 0), Some(fmt.len()));
    }

    /// `format-rev` resolves the format once and replays the atoms per record, so
    /// it needs to know whether an atom *stored* a request — `apply()` clears
    /// `flush` between records, and replaying a snapshot from an atom that stored
    /// nothing would reopen a field git had already closed.
    ///
    /// Captured from stock git 2.55.0, which shows the two halves of the
    /// distinction. A rejected width leaves the earlier field to be flushed by the
    /// *next* atom:
    ///
    /// ```text
    /// $ git log -1 --format='%<(9)%>(16385,bogus)%ci'
    ///          %>(16385,bogus)2005-04-07 22:13:13 +0200
    /// ```
    ///
    /// while a bad modifier over a good width stores, and so opens one of its own.
    #[test]
    fn parse_reports_whether_the_atom_stored_a_request() {
        // Rejected before `c->padding = …`: nothing stored.
        for fmt in ["<(0)", "<()", "<(20", "<(0x10)", "<(-5)", "<(20000)", "^(4)"] {
            let mut st = PadState::default();
            let (n, stored) = st.parse_reporting(&fmt.chars().collect::<Vec<_>>(), 0);
            assert_eq!(n, None, "{fmt}");
            assert!(!stored, "{fmt} must not store");
        }
        // A bad truncation modifier reports failure but has already stored.
        let mut st = PadState::default();
        let (n, stored) = st.parse_reporting(&"<(20,bogus)".chars().collect::<Vec<_>>(), 0);
        assert_eq!(n, None);
        assert!(stored);
        assert_eq!((st.flush, st.padding), (FlushType::Right, 20));
        // A well-formed atom stores too.
        let mut st = PadState::default();
        let (n, stored) = st.parse_reporting(&"<(20)".chars().collect::<Vec<_>>(), 0);
        assert_eq!(n, Some(5));
        assert!(stored);
        // A rejected atom leaves an already-pending request exactly as it was, so
        // a replaying caller must not overwrite it.
        let mut st = PadState { flush: FlushType::Left, trunc: TruncType::Right, padding: 7 };
        let (n, stored) = st.parse_reporting(&">(16385,bogus)".chars().collect::<Vec<_>>(), 0);
        assert_eq!(n, None);
        assert!(!stored);
        assert_eq!((st.flush, st.trunc, st.padding), (FlushType::Left, TruncType::Right, 7));
    }

    #[test]
    fn bad_truncation_modifier_still_leaves_the_padding_pending() {
        // git assigns `c->padding`/`c->flush_type` before parsing the modifier, so
        // `%<(20,bogus)` prints literally *and* pads the next placeholder.
        let (n, st) = parse("<(20,bogus)");
        assert_eq!(n, None);
        assert_eq!((st.flush, st.padding), (FlushType::Right, 20));
    }

    #[test]
    fn column_target_reads_columns_env() {
        // `%<|(-5)` is "five columns short of the terminal width".
        let saved = std::env::var("COLUMNS").ok();
        // SAFETY: single-threaded test process; restored below.
        unsafe { std::env::set_var("COLUMNS", "40") };
        let (n, st) = parse("<|(-5)");
        assert_eq!(n, Some(6));
        assert_eq!(st.padding, -35);
        unsafe { std::env::remove_var("COLUMNS") };
        match saved {
            Some(v) => unsafe { std::env::set_var("COLUMNS", v) },
            None => {}
        }
    }

    /// Every expectation here was captured from stock git 2.55.0 on a repo whose
    /// subjects are `short`, `café naïve accents` and `日本語のサブジェクト wide`.
    #[test]
    fn padding_layout_matches_git() {
        let case = |fmt: &str, text: &str| -> String {
            let chars: Vec<char> = fmt.chars().collect();
            let mut st = PadState::default();
            st.parse(&chars, 0).expect("parses");
            let mut sb: Vec<u8> = Vec::new();
            let padding = st.padding;
            st.apply(&mut sb, text.as_bytes().to_vec(), padding, 0);
            String::from_utf8(sb).unwrap()
        };
        assert_eq!(case("<(20)", "short"), "short               ");
        assert_eq!(case(">(20)", "short"), "               short");
        assert_eq!(case("><(20)", "short"), "       short        ");
        // Wide glyphs are two columns: the CJK subject is already 21 columns, so
        // a 20-wide field just overflows.
        assert_eq!(case("<(20)", "日本語のサブジェクト wide"), "日本語のサブジェクト wide");
        // 18 columns of accented text leaves two spaces in a 20-wide field, and
        // the bytes are longer than the columns.
        assert_eq!(case("<(20)", "café naïve accents"), "café naïve accents  ");
        assert_eq!(case("<(10,trunc)", "café naïve accents"), "café naï..");
        assert_eq!(case("<(10,ltrunc)", "café naïve accents"), ".. accents");
        assert_eq!(case("<(10,mtrunc)", "café naïve accents"), "café..ents");
        assert_eq!(case("<(10,trunc)", "日本語のサブジェクト wide"), "日本語の..");
        assert_eq!(case("<(10,mtrunc)", "日本語のサブジェクト wide"), "日本..wide");
        // Fields narrower than the `..` marker collapse to it.
        assert_eq!(case("<(2,trunc)", "short"), "..");
        assert_eq!(case("<(3,mtrunc)", "short"), "..t");
        assert_eq!(case("<(4,mtrunc)", "short"), "s..t");
    }

    #[test]
    fn column_target_measures_the_current_line() {
        let chars: Vec<char> = "<|(20)".chars().collect();
        let mut st = PadState::default();
        st.parse(&chars, 0).unwrap();
        let mut sb: Vec<u8> = b"e9b8f46789 ".to_vec();
        let padding = st.padding;
        st.apply(&mut sb, b"short".to_vec(), padding, 0);
        assert_eq!(sb, b"e9b8f46789 short    ");
        // Only the text after the last newline counts.
        let mut st = PadState::default();
        st.parse(&chars, 0).unwrap();
        let mut sb: Vec<u8> = b"aaaaaaaaaaaaaaaaaaaaaaaaa\nxx".to_vec();
        let padding = st.padding;
        st.apply(&mut sb, b"y".to_vec(), padding, 0);
        assert_eq!(sb, "aaaaaaaaaaaaaaaaaaaaaaaaa\nxxy                 ".as_bytes());
        // A graph prefix counts against the target.
        let mut st = PadState::default();
        st.parse(&chars, 0).unwrap();
        let mut sb: Vec<u8> = Vec::new();
        let padding = st.padding;
        st.apply(&mut sb, b"short".to_vec(), padding, 2);
        assert_eq!(sb, b"short             ");
    }

    #[test]
    fn steal_eats_preceding_spaces_only_as_far_as_needed() {
        let chars: Vec<char> = ">>(6)".chars().collect();
        let mut st = PadState::default();
        st.parse(&chars, 0).unwrap();
        let mut sb: Vec<u8> = b"xx  ".to_vec();
        let padding = st.padding;
        st.apply(&mut sb, b"e9b8f46789".to_vec(), padding, 0);
        assert_eq!(sb, b"xxe9b8f46789");
        // A field the text fits in steals nothing and right-aligns instead.
        let chars: Vec<char> = ">>(10)".chars().collect();
        let mut st = PadState::default();
        st.parse(&chars, 0).unwrap();
        let mut sb: Vec<u8> = b"x".to_vec();
        let padding = st.padding;
        st.apply(&mut sb, b"short".to_vec(), padding, 0);
        assert_eq!(sb, b"x     short");
    }

    #[test]
    fn wrap_atom_parses_and_wraps() {
        let mut sb: Vec<u8> = Vec::new();
        let mut wrap = WrapState::default();
        let chars: Vec<char> = "w(20,4)".chars().collect();
        assert_eq!(wrap.parse_and_apply(&mut sb, &chars, 0), Some(7));
        sb.extend_from_slice(b"aaa bbb ccc ddd eee fff ggg");
        wrap.rewrap_message_tail(&mut sb, 0, 0, 0);
        assert_eq!(sb, b"    aaa bbb ccc ddd\neee fff ggg");
        // `%w()` is width 0: no wrapping at all.
        let mut sb: Vec<u8> = Vec::new();
        let mut wrap = WrapState::default();
        let chars: Vec<char> = "w()".chars().collect();
        assert_eq!(wrap.parse_and_apply(&mut sb, &chars, 0), Some(3));
        sb.extend_from_slice(b"aaa bbb ccc ddd eee fff ggg");
        wrap.rewrap_message_tail(&mut sb, 0, 0, 0);
        assert_eq!(sb, b"aaa bbb ccc ddd eee fff ggg");
        // Malformed atoms are literal.
        let mut sb: Vec<u8> = Vec::new();
        let mut wrap = WrapState::default();
        assert_eq!(wrap.parse_and_apply(&mut sb, &"w(abc)".chars().collect::<Vec<_>>(), 0), None);
        assert_eq!(wrap.parse_and_apply(&mut sb, &"w(20".chars().collect::<Vec<_>>(), 0), None);
        assert_eq!(wrap.parse_and_apply(&mut sb, &"wx".chars().collect::<Vec<_>>(), 0), None);
    }
}
