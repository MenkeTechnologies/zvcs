//! Colored patch output — a port of git's `diff.c` emit layer and `ws.c`
//! whitespace-error markup.
//!
//! The diff commands in this port assemble a plain unified patch into a byte
//! buffer first, so coloring is applied by re-running that buffer through a port
//! of `fn_out_consume()`: the same line-kind dispatch git uses, feeding the same
//! `emit_diff_symbol()` → `emit_line_ws_markup()` → `emit_line_0()` /
//! `ws_check_emit()` chain. Reading the patch back is exactly what git's own
//! callback does — xdiff hands it `"+foo\n"` / `"-foo\n"` / `" foo\n"` / `"@@ …"`
//! strings and the sign byte is what selects the color — so the byte-level output
//! is identical rather than approximated.
//!
//! What lives here:
//!
//! * [`DiffColors`] — the `color.diff.<slot>` / `diff.color.<slot>` table with
//!   git's built-in defaults from `diff_colors[]` (diff.c:81).
//! * [`parse_ws_error_highlight`] — `--ws-error-highlight=<kind>` and
//!   `diff.wsErrorHighlight`.
//! * [`parse_whitespace_rule`] / [`whitespace_rule_cfg`] — `core.whitespace`.
//! * [`check_blank_at_eof`] — the `WS_BLANK_AT_EOF` pre-pass that decides whether
//!   an added blank line at the end of a file is painted as a whitespace error.
//! * [`colorize_patch`] — the `fn_out_consume()` re-emitter.

use super::color::{parse_color_spec, want_color_stdout_raw};

/// git's reset sequence — `ESC [ m`, not `ESC [ 0 m` (`GIT_COLOR_RESET`).
const RESET: &str = "\x1b[m";
/// `GIT_COLOR_REVERSE`, used by the dual-color hunk header.
const REVERSE: &str = "\x1b[7m";

// ---------------------------------------------------------------------------
// whitespace rules (ws.h)
// ---------------------------------------------------------------------------

/// `WS_BLANK_AT_EOL` — trailing whitespace on a line.
pub(crate) const WS_BLANK_AT_EOL: u32 = 1 << 6;
/// `WS_SPACE_BEFORE_TAB` — a space that precedes a tab in the indent.
pub(crate) const WS_SPACE_BEFORE_TAB: u32 = 1 << 7;
/// `WS_INDENT_WITH_NON_TAB` — an indent of `tabwidth` spaces or more.
pub(crate) const WS_INDENT_WITH_NON_TAB: u32 = 1 << 8;
/// `WS_CR_AT_EOL` — a carriage return before the newline is not an error.
pub(crate) const WS_CR_AT_EOL: u32 = 1 << 9;
/// `WS_BLANK_AT_EOF` — a blank line added at the end of the file.
pub(crate) const WS_BLANK_AT_EOF: u32 = 1 << 10;
/// `WS_TAB_IN_INDENT` — any tab in the indent.
pub(crate) const WS_TAB_IN_INDENT: u32 = 1 << 11;
/// `WS_INCOMPLETE_LINE` — a final line with no newline terminator.
pub(crate) const WS_INCOMPLETE_LINE: u32 = 1 << 12;
/// `WS_TRAILING_SPACE` — the `trailing-space` alias for both blank-at-eol rules.
pub(crate) const WS_TRAILING_SPACE: u32 = WS_BLANK_AT_EOL | WS_BLANK_AT_EOF;
/// `WS_DEFAULT_RULE` — `blank-at-eol,blank-at-eof,space-before-tab` with tab width 8.
pub(crate) const WS_DEFAULT_RULE: u32 = WS_TRAILING_SPACE | WS_SPACE_BEFORE_TAB | 8;
/// `WS_TAB_WIDTH_MASK` — the low six bits hold the tab width.
const WS_TAB_WIDTH_MASK: u32 = (1 << 6) - 1;

/// `ws_tab_width()`.
fn ws_tab_width(rule: u32) -> usize {
    (rule & WS_TAB_WIDTH_MASK) as usize
}

/// `WSEH_NEW` — highlight whitespace errors on added lines.
pub(crate) const WSEH_NEW: u32 = 1 << 16;
/// `WSEH_CONTEXT` — highlight whitespace errors on context lines.
pub(crate) const WSEH_CONTEXT: u32 = 1 << 17;
/// `WSEH_OLD` — highlight whitespace errors on removed lines.
pub(crate) const WSEH_OLD: u32 = 1 << 18;

/// One row of git's `whitespace_rule_names[]` (ws.c:21).
struct WsRuleName {
    name: &'static str,
    bits: u32,
}

/// `whitespace_rule_names[]`, in the order git searches it — the search is a
/// prefix match over the length of the token the user typed, so an unambiguous
/// abbreviation such as `trailing` also selects `trailing-space`.
const WS_RULE_NAMES: &[WsRuleName] = &[
    WsRuleName { name: "trailing-space", bits: WS_TRAILING_SPACE },
    WsRuleName { name: "space-before-tab", bits: WS_SPACE_BEFORE_TAB },
    WsRuleName { name: "indent-with-non-tab", bits: WS_INDENT_WITH_NON_TAB },
    WsRuleName { name: "cr-at-eol", bits: WS_CR_AT_EOL },
    WsRuleName { name: "blank-at-eol", bits: WS_BLANK_AT_EOL },
    WsRuleName { name: "blank-at-eof", bits: WS_BLANK_AT_EOF },
    WsRuleName { name: "tab-in-indent", bits: WS_TAB_IN_INDENT },
    WsRuleName { name: "incomplete-line", bits: WS_INCOMPLETE_LINE },
];

/// `parse_whitespace_rule()`: a comma-separated list of rule names, each
/// optionally prefixed with `-` to turn it off, plus `tabwidth=<n>`. Unknown
/// names are ignored, exactly as git ignores them.
pub(crate) fn parse_whitespace_rule(spec: &str) -> u32 {
    let mut rule = WS_DEFAULT_RULE;
    let bytes = spec.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() {
        // `string + strspn(string, ", \t\n\r")`: skip separators and blanks.
        while pos < bytes.len() && matches!(bytes[pos], b',' | b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        let end = bytes[pos..]
            .iter()
            .position(|b| *b == b',')
            .map(|i| pos + i)
            .unwrap_or(bytes.len());
        let mut start = pos;
        let negated = start < end && bytes[start] == b'-';
        if negated {
            start += 1;
        }
        if start >= end {
            break;
        }
        let token = &spec[start..end];
        for r in WS_RULE_NAMES {
            if r.name.as_bytes().starts_with(token.as_bytes()) {
                if negated {
                    rule &= !r.bits;
                } else {
                    rule |= r.bits;
                }
                break;
            }
        }
        if let Some(arg) = token.strip_prefix("tabwidth=") {
            // git's `atoi`: leading digits, 0 for anything else. Out-of-range
            // widths are warned about and left alone.
            let digits: String = arg.chars().take_while(|c| c.is_ascii_digit()).collect();
            let width: u32 = digits.parse().unwrap_or(0);
            if width > 0 && width < 0o100 {
                rule &= !WS_TAB_WIDTH_MASK;
                rule |= width;
            }
        }
        pos = end + 1;
    }
    rule
}

/// git's `whitespace_rule_cfg`, from `core.whitespace`. Per-path overrides come
/// from the `whitespace` gitattribute, which this port does not read, so every
/// file in a diff shares the configured rule.
pub(crate) fn whitespace_rule_cfg(repo: &gix::Repository) -> u32 {
    match repo.config_snapshot().string("core.whitespace") {
        Some(v) => parse_whitespace_rule(&v.to_string()),
        None => WS_DEFAULT_RULE,
    }
}

/// `parse_one_token()`: match `token` at the head of `arg` when what follows is
/// either the end of the string or a comma, and consume it.
fn parse_one_token<'a>(arg: &mut &'a str, token: &str) -> bool {
    if let Some(rest) = arg.strip_prefix(token) {
        if rest.is_empty() || rest.starts_with(',') {
            *arg = rest;
            return true;
        }
    }
    false
}

/// `parse_ws_error_highlight()`: a comma-separated list of `none`, `default`,
/// `all`, `new`, `old` and `context`. `none`, `default` and `all` *replace* the
/// accumulated set; the three side names add to it.
///
/// The error carries the byte length of the prefix git had already accepted when
/// it hit the bad token — its `-1 - (arg - orig_arg)` return, which the callers
/// print back as `unknown value after ws-error-highlight=<prefix>`.
pub(crate) fn parse_ws_error_highlight(spec: &str) -> Result<u32, usize> {
    let mut arg = spec;
    let mut val = 0u32;
    while !arg.is_empty() {
        if parse_one_token(&mut arg, "none") {
            val = 0;
        } else if parse_one_token(&mut arg, "default") {
            val = WSEH_NEW;
        } else if parse_one_token(&mut arg, "all") {
            val = WSEH_NEW | WSEH_OLD | WSEH_CONTEXT;
        } else if parse_one_token(&mut arg, "new") {
            val |= WSEH_NEW;
        } else if parse_one_token(&mut arg, "old") {
            val |= WSEH_OLD;
        } else if parse_one_token(&mut arg, "context") {
            val |= WSEH_CONTEXT;
        } else {
            return Err(spec.len() - arg.len());
        }
        // Step over the comma that `parse_one_token` left in place.
        if !arg.is_empty() {
            arg = &arg[1..];
        }
    }
    Ok(val)
}

/// git's `ws_error_highlight_default` (`WSEH_NEW`), overridable by
/// `diff.wsErrorHighlight`. Returns `Err` with the offending value when the
/// config holds a spec git rejects.
pub(crate) fn ws_error_highlight_default(repo: &gix::Repository) -> Result<u32, String> {
    match repo.config_snapshot().string("diff.wsErrorHighlight") {
        Some(v) => {
            let raw = v.to_string();
            parse_ws_error_highlight(&raw).map_err(|_| raw)
        }
        None => Ok(WSEH_NEW),
    }
}

// ---------------------------------------------------------------------------
// blank-at-eof detection (diff.c `count_trailing_blank` / `check_blank_at_eof`)
// ---------------------------------------------------------------------------

/// `ws_blank_line()`: a line made only of whitespace (C `isspace`, so the
/// vertical tab and form feed count too).
fn ws_blank_line(line: &[u8]) -> bool {
    line.iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
}

/// `count_lines()`: newline-terminated lines plus a trailing unterminated one.
/// A zero-length buffer has no lines at all.
fn count_lines(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let mut count = data.iter().filter(|b| **b == b'\n').count();
    if data.last() != Some(&b'\n') {
        count += 1;
    }
    count
}

/// `count_trailing_blank()`: how many blank lines sit at the end of the buffer.
/// git deliberately steps over the final newline first, so the last line of a
/// newline-terminated file is not itself examined here.
fn count_trailing_blank(mf: &[u8]) -> usize {
    if mf.is_empty() {
        return 0;
    }
    let mut cnt = 0usize;
    // `ptr` is git's cursor, an index one past the last byte still under
    // consideration expressed as `Some(index)`; `None` means it went below the
    // start of the buffer and the walk is over.
    let mut ptr: isize = mf.len() as isize - 1;
    if mf[ptr as usize] == b'\n' {
        ptr -= 1;
    }
    while 0 < ptr {
        let mut prev_eol = ptr;
        while prev_eol >= 0 {
            if mf[prev_eol as usize] == b'\n' {
                break;
            }
            prev_eol -= 1;
        }
        let from = (prev_eol + 1) as usize;
        let len = (ptr - prev_eol) as usize;
        if !ws_blank_line(&mf[from..from + len]) {
            break;
        }
        cnt += 1;
        ptr = prev_eol - 1;
    }
    cnt
}

/// `check_blank_at_eof()`: the 1-based line numbers at which the run of blank
/// lines that the change *lengthened* begins, in the pre-image and the post-image.
/// `(0, 0)` when the post-image does not end in more blank lines than the
/// pre-image did, which switches the whole check off for that file.
pub(crate) fn check_blank_at_eof(old: &[u8], new: &[u8]) -> (usize, usize) {
    let l1 = count_trailing_blank(old);
    let l2 = count_trailing_blank(new);
    if l2 <= l1 {
        return (0, 0);
    }
    (count_lines(old) - l1 + 1, count_lines(new) - l2 + 1)
}

// ---------------------------------------------------------------------------
// the color.diff.<slot> table (diff.c `diff_colors[]` / `color_diff_slots[]`)
// ---------------------------------------------------------------------------

/// git's `enum color_diff`, restricted to the slots the diff commands can reach.
/// The dual-color slots (`contextDimmed`, `oldBold`, …) are only consulted by
/// `range-diff --dual-color`, so they are deliberately absent.
#[derive(Clone, Copy)]
pub(crate) enum DiffSlot {
    /// `context` (also spelled `plain`) — unchanged lines. No color by default.
    Context = 0,
    /// `meta` — the `diff --git`, `index`, `---`/`+++` and mode lines. Bold.
    Meta,
    /// `frag` — the `@@ … @@` hunk header. Cyan.
    Frag,
    /// `old` — removed lines. Red.
    Old,
    /// `new` — added lines. Green.
    New,
    /// `whitespace` — the whitespace-error markup. Red background.
    Whitespace,
    /// `func` — the section heading after the second `@@`. No color by default.
    Func,
}

/// The number of slots in [`DiffSlot`].
const NSLOTS: usize = 7;

/// One row of the slot table: the `color.diff.` and `diff.color.` spellings git
/// accepts for the slot, and its built-in default spec.
struct SlotDef {
    names: &'static [&'static str],
    default_spec: &'static str,
}

/// git's `color_diff_slots[]` paired with `diff_colors[]`. Both the
/// `color.diff.<slot>` and the `diff.color.<slot>` spelling reach the same slot
/// (`git_diff_basic_config()` strips either prefix), and `plain` is a second name
/// for `context` (`parse_diff_color_slot()`).
const SLOT_DEFS: [SlotDef; NSLOTS] = [
    SlotDef {
        names: &["color.diff.context", "color.diff.plain", "diff.color.context", "diff.color.plain"],
        default_spec: "",
    },
    SlotDef { names: &["color.diff.meta", "diff.color.meta"], default_spec: "bold" },
    SlotDef { names: &["color.diff.frag", "diff.color.frag"], default_spec: "cyan" },
    SlotDef { names: &["color.diff.old", "diff.color.old"], default_spec: "red" },
    SlotDef { names: &["color.diff.new", "diff.color.new"], default_spec: "green" },
    SlotDef {
        names: &["color.diff.whitespace", "diff.color.whitespace"],
        // `GIT_COLOR_BG_RED`: no foreground change, red background.
        default_spec: "normal red",
    },
    SlotDef { names: &["color.diff.func", "diff.color.func"], default_spec: "" },
];

/// The resolved SGR sequence for every diff slot, or a disabled table whose
/// sequences — and whose reset — are all empty, matching `diff_get_color()`'s
/// `want_color(use_color) ? diff_colors[ix] : ""`.
pub(crate) struct DiffColors {
    enabled: bool,
    slots: [String; NSLOTS],
}

impl DiffColors {
    /// Colors turned off: every slot, and the reset, is the empty string.
    pub(crate) fn disabled() -> Self {
        DiffColors { enabled: false, slots: Default::default() }
    }

    /// Read every slot from `repo`'s config. `enabled` is the already-decided
    /// `want_color()` answer; when it is false this is [`DiffColors::disabled`].
    pub(crate) fn resolve(repo: &gix::Repository, enabled: bool) -> Self {
        if !enabled {
            return Self::disabled();
        }
        let snapshot = repo.config_snapshot();
        let slots = std::array::from_fn(|i| {
            let def = &SLOT_DEFS[i];
            let spec = last_set(&snapshot, def.names).unwrap_or_else(|| def.default_spec.to_string());
            // A spec git accepts but this port cannot render falls back to the
            // built-in default rather than to no color at all.
            parse_color_spec(&spec)
                .or_else(|| parse_color_spec(def.default_spec))
                .unwrap_or_default()
        });
        DiffColors { enabled: true, slots }
    }

    /// Whether any coloring at all is emitted (git's `want_color(o->use_color)`).
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// `diff_get_color_opt()` for one slot.
    pub(crate) fn get(&self, slot: DiffSlot) -> &str {
        &self.slots[slot as usize]
    }

    /// `diff_get_color(use_color, DIFF_RESET)`.
    pub(crate) fn reset(&self) -> &'static str {
        if self.enabled {
            RESET
        } else {
            ""
        }
    }
}

/// `show_graph()` / `show_stats()`: wrap one run of text in a slot's color. With
/// coloring off both the sequence and the reset are empty, so the text passes
/// through untouched.
pub(crate) fn paint(out: &mut Vec<u8>, colors: &DiffColors, slot: DiffSlot, text: &[u8]) {
    out.extend_from_slice(colors.get(slot).as_bytes());
    out.extend_from_slice(text);
    out.extend_from_slice(colors.reset().as_bytes());
}

/// git applies config assignments in the order it reads them, so when several
/// spellings write the same slot the last occurrence wins. Walk the merged
/// snapshot in that order and return the value of the last name in `names` to
/// appear anywhere in it.
fn last_set(snapshot: &gix::config::Snapshot<'_>, names: &[&'static str]) -> Option<String> {
    let file = snapshot.plumbing();
    let mut winner: Option<&'static str> = None;
    for section in file.sections() {
        let header = section.header();
        for value_name in section.value_names() {
            for full in names {
                // A key is either `<section>.<value>` (`color.diff`) or
                // `<section>.<subsection>.<value>` (`color.diff.old`).
                let parts: Vec<&str> = full.split('.').collect();
                let (sec, sub, val) = match parts.as_slice() {
                    [sec, val] => (*sec, None, *val),
                    [sec, sub, val] => (*sec, Some(*sub), *val),
                    _ => continue,
                };
                let subsection_matches = match (sub, header.subsection_name()) {
                    (None, None) => true,
                    (Some(want), Some(have)) => have.eq_ignore_ascii_case(want.as_bytes()),
                    _ => false,
                };
                if header.name().eq_ignore_ascii_case(sec.as_bytes())
                    && subsection_matches
                    && value_name.eq_ignore_ascii_case(val)
                {
                    winner = Some(full);
                }
            }
        }
    }
    snapshot.string(winner?).map(|v| v.to_string())
}

/// git's `want_color()` for the diff commands: `color.diff`, its `diff.color`
/// alias (`git_diff_ui_config()` accepts both names for the one setting), and
/// `color.ui` as the fallback. Whichever of the two names the config assigns last
/// decides, because git simply overwrites `diff_use_color_default` each time.
pub(crate) fn want_diff_color(repo: &gix::Repository) -> bool {
    let snapshot = repo.config_snapshot();
    let raw = last_set(&snapshot, &["color.diff", "diff.color"])
        .or_else(|| snapshot.string("color.ui").map(|v| v.to_string()));
    want_color_stdout_raw(repo, raw.as_deref())
}

// ---------------------------------------------------------------------------
// emit layer (diff.c `emit_line_0` / `emit_line_ws_markup`, ws.c `ws_check_emit`)
// ---------------------------------------------------------------------------

/// `emit_line_0()`: write one output line, wrapping it in `set_sign` (applied
/// before the sign byte) and `set` (applied to the content), and closing with
/// `reset` when anything at all was written.
///
/// The trailing `\n` — and a `\r` before it — are held back so no SGR sequence
/// ever straddles the line terminator, which is what makes git's colored patches
/// safe to feed back through `git apply`.
#[allow(clippy::too_many_arguments)]
fn emit_line_0(
    out: &mut Vec<u8>,
    colors_on: bool,
    set_sign: Option<&str>,
    set: Option<&str>,
    reverse: bool,
    reset: &str,
    first: u8,
    line: &[u8],
) {
    let mut needs_reset = false;
    let mut len = line.len();
    let has_nl = len > 0 && line[len - 1] == b'\n';
    if has_nl {
        len -= 1;
    }
    let has_cr = len > 0 && line[len - 1] == b'\r';
    if has_cr {
        len -= 1;
    }

    'body: {
        if len == 0 && first == 0 {
            break 'body;
        }
        if reverse && colors_on {
            out.extend_from_slice(REVERSE.as_bytes());
            needs_reset = true;
        }
        if let Some(s) = set_sign {
            out.extend_from_slice(s.as_bytes());
            needs_reset = true;
        }
        if first != 0 {
            out.push(first);
        }
        if len == 0 {
            break 'body;
        }
        if let Some(s) = set {
            if set_sign.is_some_and(|sg| sg != s) {
                out.extend_from_slice(reset.as_bytes());
            }
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(&line[..len]);
        // git sets this unconditionally after the content: the line may itself
        // carry color codes, so the reset is always needed.
        needs_reset = true;
    }

    if needs_reset {
        out.extend_from_slice(reset.as_bytes());
    }
    if has_cr {
        out.push(b'\r');
    }
    if has_nl {
        out.push(b'\n');
    }
}

/// `emit_line()`: the no-sign, no-`set_sign` shorthand.
fn emit_line(out: &mut Vec<u8>, colors_on: bool, set: &str, reset: &str, line: &[u8]) {
    emit_line_0(out, colors_on, Some(set), None, false, reset, 0, line);
}

/// `ws_check_emit_1()` in its emitting mode: split the line into indent, body and
/// trailing whitespace, painting each run that violates `ws_rule` with `ws` and
/// the untouched middle with `set`.
fn ws_check_emit(out: &mut Vec<u8>, line: &[u8], ws_rule: u32, set: &str, reset: &str, ws: &str) {
    let mut len = line.len();
    let mut trailing_newline = false;
    let mut trailing_cr = false;
    if len > 0 && line[len - 1] == b'\n' {
        trailing_newline = true;
        len -= 1;
    }
    if (ws_rule & WS_CR_AT_EOL) != 0 && len > 0 && line[len - 1] == b'\r' {
        trailing_cr = true;
        len -= 1;
    }

    // The index at which the run of trailing whitespace starts, or `len` when the
    // rule is off or the line has none.
    let mut trailing_whitespace: Option<usize> = None;
    if (ws_rule & WS_BLANK_AT_EOL) != 0 {
        for i in (0..len).rev() {
            if is_c_space(line[i]) {
                trailing_whitespace = Some(i);
            } else {
                break;
            }
        }
    }
    let trailing_whitespace = trailing_whitespace.unwrap_or(len);

    // Indentation: everything up to the first byte that is neither space nor tab.
    let mut written = 0usize;
    let mut i = 0usize;
    while i < trailing_whitespace {
        if line[i] == b' ' {
            i += 1;
            continue;
        }
        if line[i] != b'\t' {
            break;
        }
        if (ws_rule & WS_SPACE_BEFORE_TAB) != 0 && written < i {
            out.extend_from_slice(ws.as_bytes());
            out.extend_from_slice(&line[written..i]);
            out.extend_from_slice(reset.as_bytes());
            out.push(line[i]);
        } else if (ws_rule & WS_TAB_IN_INDENT) != 0 {
            out.extend_from_slice(&line[written..i]);
            out.extend_from_slice(ws.as_bytes());
            out.push(line[i]);
            out.extend_from_slice(reset.as_bytes());
        } else {
            out.extend_from_slice(&line[written..=i]);
        }
        written = i + 1;
        i += 1;
    }

    // A long enough all-space indent is `indent-with-non-tab`.
    if (ws_rule & WS_INDENT_WITH_NON_TAB) != 0 && i - written >= ws_tab_width(ws_rule) {
        out.extend_from_slice(ws.as_bytes());
        out.extend_from_slice(&line[written..i]);
        out.extend_from_slice(reset.as_bytes());
        written = i;
    }

    if trailing_whitespace > written {
        out.extend_from_slice(set.as_bytes());
        out.extend_from_slice(&line[written..trailing_whitespace]);
        out.extend_from_slice(reset.as_bytes());
    }
    if trailing_whitespace != len {
        out.extend_from_slice(ws.as_bytes());
        out.extend_from_slice(&line[trailing_whitespace..len]);
        out.extend_from_slice(reset.as_bytes());
    }
    if trailing_cr {
        out.push(b'\r');
    }
    if trailing_newline {
        out.push(b'\n');
    }
}

/// C's `isspace` in the default locale, which counts the vertical tab and form
/// feed that Rust's `is_ascii_whitespace` leaves out.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

// ---------------------------------------------------------------------------
// the re-emitter (diff.c `fn_out_consume`)
// ---------------------------------------------------------------------------

/// Everything `builtin_diff()` computes per file pair that the emit layer needs.
#[derive(Clone, Copy)]
pub(crate) struct FilePaint {
    /// `whitespace_rule()` for the file.
    pub(crate) ws_rule: u32,
    /// `blank_at_eof_in_preimage` / `blank_at_eof_in_postimage`.
    pub(crate) blank_at_eof: (usize, usize),
}

impl FilePaint {
    /// The state for a file whose pre/post images were not examined: git's
    /// `check_blank_at_eof()` leaves both counters at zero in that case, which
    /// disables the blank-at-EOF check for the file.
    pub(crate) fn new(ws_rule: u32) -> Self {
        FilePaint { ws_rule, blank_at_eof: (0, 0) }
    }
}

/// The parts of `struct diff_options` the emit layer reads that are the same for
/// every file pair in one invocation.
#[derive(Clone, Copy)]
pub(crate) struct PaintOptions {
    /// `o->ws_error_highlight`: which of `WSEH_NEW`/`WSEH_OLD`/`WSEH_CONTEXT` are on.
    pub(crate) ws_error_highlight: u32,
    /// `o->output_indicators[]` — the added, removed and context sign bytes, which
    /// `--output-indicator-new`/`-old`/`-context` replace.
    pub(crate) indicators: (u8, u8, u8),
    /// `diff_suppress_blank_empty` (`diff.suppressBlankEmpty`): drop the sign from
    /// an otherwise empty context line, so it is printed as a bare newline.
    /// `emit_line_ws_markup()` applies it before any color is chosen, which is why
    /// it has to be known here rather than patched into the finished bytes.
    pub(crate) suppress_blank_empty: bool,
}

impl Default for PaintOptions {
    /// git's defaults: highlight the new side, sign with `+`, `-` and a space, and
    /// keep the sign on an empty context line.
    fn default() -> Self {
        PaintOptions {
            ws_error_highlight: WSEH_NEW,
            indicators: (b'+', b'-', b' '),
            suppress_blank_empty: false,
        }
    }
}

/// Re-emit an assembled unified patch with color, following `fn_out_consume()`'s
/// dispatch on the first byte of each line.
///
/// `files` supplies the per-file-pair state in the order the `diff --git` headers
/// appear; a section past the end of the slice falls back to `default_file`.
pub(crate) fn colorize_patch(
    patch: &[u8],
    colors: &DiffColors,
    opts: &PaintOptions,
    files: &[FilePaint],
    default_file: FilePaint,
) -> Vec<u8> {
    if !colors.enabled() {
        return patch.to_vec();
    }
    let on = true;
    let reset = colors.reset();
    let meta = colors.get(DiffSlot::Meta);
    let context = colors.get(DiffSlot::Context);
    let frag = colors.get(DiffSlot::Frag);
    let func = colors.get(DiffSlot::Func);
    let old = colors.get(DiffSlot::Old);
    let new = colors.get(DiffSlot::New);
    let ws_color = colors.get(DiffSlot::Whitespace);

    let (ind_new, ind_old, ind_ctx) = opts.indicators;
    let ws_error_highlight = opts.ws_error_highlight;

    let mut out: Vec<u8> = Vec::with_capacity(patch.len() * 2);
    let mut file_no: usize = 0;
    let mut cur = default_file;
    let mut in_hunk = false;
    let mut lno_pre = 0usize;
    let mut lno_post = 0usize;
    let mut last_kind = ind_ctx;

    for line in split_keep_terminator(patch) {
        let first = line.first().copied().unwrap_or(0);
        let is_content =
            first == ind_new || first == ind_old || first == ind_ctx || first == b'\\';
        if in_hunk && !is_content && first != b'@' {
            in_hunk = false;
        }
        if !in_hunk {
            if line.starts_with(b"diff --git ") || line.starts_with(b"diff --cc ") {
                cur = files.get(file_no).copied().unwrap_or(default_file);
                file_no += 1;
            }
            if first == b'@' {
                in_hunk = true;
                let (a, b) = find_lno(line);
                lno_pre = a;
                lno_post = b;
                last_kind = ind_ctx;
                emit_hunk_header(&mut out, on, line, frag, context, func, reset);
                continue;
            }
            emit_header_line(&mut out, line, meta, reset);
            continue;
        }

        // `fn_out_consume()` dispatches on the sign byte. The three signs are
        // compared before `@`, so an `--output-indicator-*` that reuses `@` still
        // reads as a content line, exactly as git's `switch` would.
        match first {
            c if c == ind_new => {
                lno_post += 1;
                let blank_at_eof = new_blank_line_at_eof(&cur, lno_pre, lno_post, &line[1..]);
                emit_content(
                    &mut out,
                    on,
                    new,
                    reset,
                    ws_color,
                    ind_new,
                    &line[1..],
                    cur.ws_rule,
                    (ws_error_highlight & WSEH_NEW) != 0,
                    blank_at_eof,
                );
                last_kind = ind_new;
            }
            c if c == ind_old => {
                lno_pre += 1;
                emit_content(
                    &mut out,
                    on,
                    old,
                    reset,
                    ws_color,
                    ind_old,
                    &line[1..],
                    cur.ws_rule,
                    (ws_error_highlight & WSEH_OLD) != 0,
                    false,
                );
                last_kind = ind_old;
            }
            c if c == ind_ctx => {
                lno_pre += 1;
                lno_post += 1;
                let body = &line[1..];
                // `emit_line_ws_markup()` drops the sign of an empty context line
                // under `diff.suppressBlankEmpty`, which leaves nothing at all to
                // paint and so emits a bare newline.
                let sign = if opts.suppress_blank_empty && body == b"\n" {
                    0
                } else {
                    ind_ctx
                };
                emit_content(
                    &mut out,
                    on,
                    context,
                    reset,
                    ws_color,
                    sign,
                    body,
                    cur.ws_rule,
                    (ws_error_highlight & WSEH_CONTEXT) != 0,
                    false,
                );
                last_kind = ind_ctx;
            }
            b'@' => {
                let (a, b) = find_lno(line);
                lno_pre = a;
                lno_post = b;
                last_kind = ind_ctx;
                emit_hunk_header(&mut out, on, line, frag, context, func, reset);
            }
            // `\ No newline at end of file`: painted with the whitespace color
            // only when `incomplete-line` is both an enabled rule and a
            // highlighted side, otherwise with the context color.
            b'\\' => {
                lno_pre += 1;
                let side = if last_kind == ind_new {
                    WSEH_NEW
                } else if last_kind == ind_old {
                    WSEH_OLD
                } else {
                    WSEH_CONTEXT
                };
                let set = if (cur.ws_rule & WS_INCOMPLETE_LINE) != 0
                    && (ws_error_highlight & side) != 0
                {
                    ws_color
                } else {
                    context
                };
                emit_line(&mut out, on, set, reset, line);
            }
            _ => out.extend_from_slice(line),
        }
    }
    out
}

/// `new_blank_line_at_eof()`: an added blank line counts as a whitespace error
/// only inside the run of blank lines the change lengthened.
fn new_blank_line_at_eof(cur: &FilePaint, lno_pre: usize, lno_post: usize, line: &[u8]) -> bool {
    let (pre, post) = cur.blank_at_eof;
    if (cur.ws_rule & WS_BLANK_AT_EOF) == 0 || pre == 0 || post == 0 {
        return false;
    }
    if pre > lno_pre || post > lno_post {
        return false;
    }
    ws_blank_line(line)
}

/// `emit_line_ws_markup()`: one `+`/`-`/context line, choosing between the plain
/// single-span emission and the three-way whitespace-marked one.
///
/// An empty context line under `diff.suppressBlankEmpty` loses its sign in git;
/// the callers of this module apply that rewrite as a separate whole-buffer pass
/// over identical bytes, so the sign is always kept here.
#[allow(clippy::too_many_arguments)]
fn emit_content(
    out: &mut Vec<u8>,
    on: bool,
    set: &str,
    reset: &str,
    ws_color: &str,
    sign: u8,
    line: &[u8],
    ws_rule: u32,
    highlight: bool,
    blank_at_eof: bool,
) {
    let ws = if highlight && !ws_color.is_empty() {
        Some(ws_color)
    } else {
        None
    };
    match ws {
        None => emit_line_0(out, on, Some(set), None, false, reset, sign, line),
        Some(ws) if blank_at_eof => {
            emit_line_0(out, on, Some(ws), None, false, reset, sign, line)
        }
        Some(ws) => {
            emit_line_0(out, on, Some(set), None, false, reset, sign, b"");
            ws_check_emit(out, line, ws_rule, set, reset, ws);
        }
    }
}

/// `emit_hunk_header()`: the `@@ … @@` range in `frag`, the blanks that separate
/// it from the section heading in `context`, and the heading itself in `func`.
fn emit_hunk_header(
    out: &mut Vec<u8>,
    on: bool,
    line: &[u8],
    frag: &str,
    context: &str,
    func: &str,
    reset: &str,
) {
    // A hunk header is at least `@@ -x +y @@`, ten bytes; anything shorter is
    // emitted as a plain context marker.
    let second = line.windows(2).skip(2).position(|w| w == b"@@").map(|i| i + 2);
    let Some(at) = second else {
        emit_line(out, on, context, reset, line);
        return;
    };
    if line.len() < 10 || !line.starts_with(b"@@") {
        emit_line(out, on, context, reset, line);
        return;
    }
    let mut ep = at + 2; // just past the closing `@@`
    let org_len = line.len();
    let mut len = org_len;

    let mut msg: Vec<u8> = Vec::with_capacity(org_len + 32);
    msg.extend_from_slice(frag.as_bytes());
    msg.extend_from_slice(&line[..ep]);
    msg.extend_from_slice(reset.as_bytes());

    // Strip up to two trailing `\r`/`\n` bytes, which are re-appended verbatim.
    for i in 1..3 {
        if len >= i && matches!(line[len - i], b'\r' | b'\n') {
            len -= 1;
        }
    }

    let cp = ep;
    while ep < len && matches!(line[ep], b' ' | b'\t') {
        ep += 1;
    }
    if ep != cp {
        msg.extend_from_slice(context.as_bytes());
        msg.extend_from_slice(&line[cp..ep]);
        msg.extend_from_slice(reset.as_bytes());
    }
    if ep < len {
        msg.extend_from_slice(func.as_bytes());
        msg.extend_from_slice(&line[ep..len]);
        msg.extend_from_slice(reset.as_bytes());
    }
    msg.extend_from_slice(&line[len..org_len]);
    if msg.last() != Some(&b'\n') {
        msg.push(b'\n');
    }
    // `DIFF_SYMBOL_CONTEXT_FRAGINFO` is emitted with empty set and reset: the
    // buffer already carries its own sequences.
    emit_line_0(out, on, Some(""), None, false, "", 0, &msg);
}

/// A file-header line: the metainfo color, or verbatim for the lines git emits
/// through `DIFF_SYMBOL_BINARY_FILES` / `DIFF_SYMBOL_BINARY_DIFF_*`, which carry
/// no color at all.
fn emit_header_line(out: &mut Vec<u8>, line: &[u8], meta: &str, reset: &str) {
    const META_PREFIXES: [&[u8]; 11] = [
        b"diff --git ",
        b"diff --cc ",
        b"diff --combined ",
        b"index ",
        b"new file mode ",
        b"deleted file mode ",
        b"old mode ",
        b"new mode ",
        b"similarity index ",
        b"dissimilarity index ",
        b"mode ",
    ];
    // `DIFF_SYMBOL_FILEPAIR_MINUS` / `_PLUS` put the disambiguating tab *after*
    // the reset, so it is never swallowed by a terminal's color handling.
    if line.starts_with(b"--- ") || line.starts_with(b"+++ ") {
        let body = strip_eol(line);
        let (body, tab) = match body.strip_suffix(b"\t") {
            Some(rest) => (rest, true),
            None => (body, false),
        };
        out.extend_from_slice(meta.as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(reset.as_bytes());
        if tab {
            out.push(b'\t');
        }
        out.push(b'\n');
        return;
    }
    let is_meta = META_PREFIXES.iter().any(|p| line.starts_with(p))
        || line.starts_with(b"rename from ")
        || line.starts_with(b"rename to ")
        || line.starts_with(b"copy from ")
        || line.starts_with(b"copy to ");
    if !is_meta {
        out.extend_from_slice(line);
        return;
    }
    // `fill_metainfo()` / `builtin_diff()` wrap each header line in `meta` and
    // close it with the reset before the newline.
    let body = strip_eol(line);
    out.extend_from_slice(meta.as_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(reset.as_bytes());
    out.push(b'\n');
}

/// `find_lno()`: the pre- and post-image start line numbers of a hunk header.
fn find_lno(line: &[u8]) -> (usize, usize) {
    let Some(minus) = line.iter().position(|b| *b == b'-') else {
        return (0, 0);
    };
    let pre = parse_leading_number(&line[minus + 1..]);
    let Some(plus) = line[minus..].iter().position(|b| *b == b'+') else {
        return (pre, 0);
    };
    let post = parse_leading_number(&line[minus + plus + 1..]);
    (pre, post)
}

/// C `strtol` over the leading decimal digits, yielding 0 when there are none.
fn parse_leading_number(bytes: &[u8]) -> usize {
    let mut n = 0usize;
    for b in bytes {
        if !b.is_ascii_digit() {
            break;
        }
        n = n * 10 + (b - b'0') as usize;
    }
    n
}

/// The line without its `\n` (and a `\r` before it stays put, matching git's
/// header formatting, which only ever strips the newline).
fn strip_eol(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// Split a buffer into lines, each keeping its terminator; a trailing fragment
/// without one is yielded as its own line.
fn split_keep_terminator(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut start = 0usize;
    std::iter::from_fn(move || {
        if start >= buf.len() {
            return None;
        }
        let end = buf[start..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| start + i + 1)
            .unwrap_or(buf.len());
        let line = &buf[start..end];
        start = end;
        Some(line)
    })
}

// ---------------------------------------------------------------------------
// --color / --no-color / --color=<when>
// ---------------------------------------------------------------------------

/// The `--color[=<when>]` tri-state, as `git_config_colorbool()` classifies it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorWhen {
    /// `--color`, `--color=always`, `--color=true`.
    Always,
    /// `--color=never`, `--no-color`, `--color=false`.
    Never,
    /// `--color=auto` — decided by whether stdout is a terminal.
    Auto,
}

/// Parse the argument of `--color=<when>`.
///
/// `parse_opt_color_flag_cb()` routes the value through `git_config_colorbool()`
/// with a `NULL` variable name, which recognizes only `always`, `auto` and
/// `never` (case-insensitively) and returns -1 for everything else — the boolean
/// spellings a *config* value may use are not accepted here. `None` is that -1,
/// which the caller reports as
/// ``option `color' expects "always", "auto", or "never"``.
pub(crate) fn parse_color_when(arg: &str) -> Option<ColorWhen> {
    if arg.eq_ignore_ascii_case("always") {
        return Some(ColorWhen::Always);
    }
    if arg.eq_ignore_ascii_case("never") {
        return Some(ColorWhen::Never);
    }
    if arg.eq_ignore_ascii_case("auto") {
        return Some(ColorWhen::Auto);
    }
    None
}

/// Resolve the final on/off answer: an explicit `--color=<when>` wins over the
/// config, and `auto` (like an unset flag) defers to `color.diff` / `color.ui`
/// and the terminal test.
pub(crate) fn resolve_color(repo: &gix::Repository, when: Option<ColorWhen>) -> bool {
    match when {
        Some(ColorWhen::Always) => true,
        Some(ColorWhen::Never) => false,
        Some(ColorWhen::Auto) | None => want_diff_color(repo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_error_highlight_tokens() {
        assert_eq!(parse_ws_error_highlight("default"), Ok(WSEH_NEW));
        assert_eq!(parse_ws_error_highlight("none"), Ok(0));
        assert_eq!(
            parse_ws_error_highlight("all"),
            Ok(WSEH_NEW | WSEH_OLD | WSEH_CONTEXT)
        );
        assert_eq!(parse_ws_error_highlight("old,new"), Ok(WSEH_OLD | WSEH_NEW));
        // `none` resets what came before it, `old` then adds to the empty set.
        assert_eq!(parse_ws_error_highlight("new,none,old"), Ok(WSEH_OLD));
        // The error carries how much of the value git had already accepted, which
        // is what its `unknown value after ws-error-highlight=<prefix>` prints.
        assert_eq!(parse_ws_error_highlight("bogus"), Err(0));
        assert_eq!(parse_ws_error_highlight("old,bogus"), Err(4));
        assert_eq!(parse_ws_error_highlight("newish"), Err(0));
    }

    #[test]
    fn whitespace_rule_spec() {
        assert_eq!(parse_whitespace_rule(""), WS_DEFAULT_RULE);
        assert_eq!(
            parse_whitespace_rule("-blank-at-eol"),
            WS_DEFAULT_RULE & !WS_BLANK_AT_EOL
        );
        assert_eq!(
            parse_whitespace_rule("tab-in-indent"),
            WS_DEFAULT_RULE | WS_TAB_IN_INDENT
        );
        assert_eq!(parse_whitespace_rule("tabwidth=4") & WS_TAB_WIDTH_MASK, 4);
        // Out-of-range widths leave the default in place.
        assert_eq!(parse_whitespace_rule("tabwidth=99") & WS_TAB_WIDTH_MASK, 8);
    }

    #[test]
    fn blank_at_eof_positions() {
        // The post-image grew a blank line at the end, so the run starts at the
        // line after the last non-blank one on each side.
        assert_eq!(check_blank_at_eof(b"a\n", b"a\n\n"), (2, 2));
        // No new blank line: the check is switched off entirely.
        assert_eq!(check_blank_at_eof(b"a\n\n", b"a\n"), (0, 0));
        assert_eq!(check_blank_at_eof(b"a\n", b"a\nb\n"), (0, 0));
    }

    #[test]
    fn hunk_header_line_numbers() {
        assert_eq!(find_lno(b"@@ -1,5 +2,6 @@\n"), (1, 2));
        assert_eq!(find_lno(b"@@ -0,0 +1,2 @@\n"), (0, 1));
    }
}
