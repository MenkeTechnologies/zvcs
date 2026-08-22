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
pub(crate) fn ws_tab_width(rule: u32) -> usize {
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
pub(crate) fn ws_blank_line(line: &[u8]) -> bool {
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

/// git's `enum color_diff` (diff.h:452-476), in its own order.
///
/// The last six slots — `contextDimmed`, `oldDimmed`, `newDimmed`, `contextBold`,
/// `oldBold`, `newBold` — are only consulted under
/// `o->flags.dual_color_diffed_diffs`, which `range-diff`'s `output()` alone sets
/// (range-diff.c:524-525). They are the *inner* diff's colors: a diff-of-diffs line
/// is painted twice over, once for the outer sign that `set_sign` carries and once
/// for the inner marker the content starts with, and the bold/dimmed pair is what
/// keeps the two legible against each other (diff.c:1483-1497, :1528-1541).
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
    /// `commit` — a commit name. Yellow. Only `range-diff`'s pair header reaches
    /// this from the diff family; the patch body never emits it.
    Commit,
    /// `whitespace` — the whitespace-error markup. Red background.
    Whitespace,
    /// `func` — the section heading after the second `@@`. No color by default.
    Func,
    /// `oldMoved` — a removed line that reappears elsewhere. Bold magenta.
    OldMoved,
    /// `oldMovedAlternative` — the zebra's second shade for removed lines. Bold blue.
    OldMovedAlt,
    /// `oldMovedDimmed` — a `dimmed-zebra` block interior. Faint.
    OldMovedDim,
    /// `oldMovedAlternativeDimmed` — the alternate block interior. Faint italic.
    OldMovedAltDim,
    /// `newMoved` — an added line that came from elsewhere. Bold cyan.
    NewMoved,
    /// `newMovedAlternative` — the zebra's second shade for added lines. Bold yellow.
    NewMovedAlt,
    /// `newMovedDimmed` — a `dimmed-zebra` block interior. Faint.
    NewMovedDim,
    /// `newMovedAlternativeDimmed` — the alternate block interior. Faint italic.
    NewMovedAltDim,
    /// `contextDimmed` — a diff-of-diffs line the outer diff removes whose inner
    /// marker is neither `+`, `-` nor `@`. Faint.
    ContextDim,
    /// `oldDimmed` — an outer-removed line whose inner marker is `-`. Faint red.
    OldDim,
    /// `newDimmed` — an outer-removed line whose inner marker is `+`. Faint green.
    NewDim,
    /// `contextBold` — an outer-added line whose inner marker is none of the three. Bold.
    ContextBold,
    /// `oldBold` — an outer-added line whose inner marker is `-`. Bold red.
    OldBold,
    /// `newBold` — an outer-added line whose inner marker is `+`. Bold green.
    NewBold,
}

/// The number of slots in [`DiffSlot`].
const NSLOTS: usize = 22;

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
    SlotDef { names: &["color.diff.commit", "diff.color.commit"], default_spec: "yellow" },
    SlotDef {
        names: &["color.diff.whitespace", "diff.color.whitespace"],
        // `GIT_COLOR_BG_RED`: no foreground change, red background.
        default_spec: "normal red",
    },
    SlotDef { names: &["color.diff.func", "diff.color.func"], default_spec: "" },
    // `GIT_COLOR_BOLD_MAGENTA` … `GIT_COLOR_FAINT_ITALIC`, in `diff_colors[]` order.
    SlotDef {
        names: &["color.diff.oldMoved", "diff.color.oldMoved"],
        default_spec: "bold magenta",
    },
    SlotDef {
        names: &["color.diff.oldMovedAlternative", "diff.color.oldMovedAlternative"],
        default_spec: "bold blue",
    },
    SlotDef {
        names: &["color.diff.oldMovedDimmed", "diff.color.oldMovedDimmed"],
        default_spec: "dim",
    },
    SlotDef {
        names: &[
            "color.diff.oldMovedAlternativeDimmed",
            "diff.color.oldMovedAlternativeDimmed",
        ],
        default_spec: "dim italic",
    },
    SlotDef {
        names: &["color.diff.newMoved", "diff.color.newMoved"],
        default_spec: "bold cyan",
    },
    SlotDef {
        names: &["color.diff.newMovedAlternative", "diff.color.newMovedAlternative"],
        default_spec: "bold yellow",
    },
    SlotDef {
        names: &["color.diff.newMovedDimmed", "diff.color.newMovedDimmed"],
        default_spec: "dim",
    },
    SlotDef {
        names: &[
            "color.diff.newMovedAlternativeDimmed",
            "diff.color.newMovedAlternativeDimmed",
        ],
        default_spec: "dim italic",
    },
    // The dual-color pairs (diff.c:99-104): `GIT_COLOR_FAINT`, `GIT_COLOR_FAINT_RED`,
    // `GIT_COLOR_FAINT_GREEN`, `GIT_COLOR_BOLD`, `GIT_COLOR_BOLD_RED`,
    // `GIT_COLOR_BOLD_GREEN`.
    SlotDef {
        names: &["color.diff.contextDimmed", "diff.color.contextDimmed"],
        default_spec: "dim",
    },
    SlotDef {
        names: &["color.diff.oldDimmed", "diff.color.oldDimmed"],
        default_spec: "dim red",
    },
    SlotDef {
        names: &["color.diff.newDimmed", "diff.color.newDimmed"],
        default_spec: "dim green",
    },
    SlotDef {
        names: &["color.diff.contextBold", "diff.color.contextBold"],
        default_spec: "bold",
    },
    SlotDef {
        names: &["color.diff.oldBold", "diff.color.oldBold"],
        default_spec: "bold red",
    },
    SlotDef {
        names: &["color.diff.newBold", "diff.color.newBold"],
        default_spec: "bold green",
    },
];

/// The resolved SGR sequence for every diff slot, or a disabled table whose
/// sequences — and whose reset — are all empty, matching `diff_get_color()`'s
/// `want_color(use_color) ? diff_colors[ix] : ""`.
#[derive(Clone)]
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
// --color-moved / --color-moved-ws (diff.c `parse_color_moved`)
// ---------------------------------------------------------------------------

/// git's `enum color_moved` (diff.h:397).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ColorMoved {
    /// `no` — no move detection at all.
    No,
    /// `plain` — every line with a counterpart anywhere is painted, no blocks.
    Plain,
    /// `blocks` — block detection, but one shade per side.
    Blocks,
    /// `zebra` — adjacent blocks alternate between the two shades.
    Zebra,
    /// `dimmed-zebra` — as `zebra`, with block interiors dimmed.
    ZebraDim,
}

/// `COLOR_MOVED_DEFAULT` (diff.h:403).
pub(crate) const COLOR_MOVED_DEFAULT: ColorMoved = ColorMoved::Zebra;
/// `COLOR_MOVED_MIN_ALNUM_COUNT` (diff.h:404): a block with fewer alphanumeric
/// characters than this is not interesting enough to paint.
const COLOR_MOVED_MIN_ALNUM_COUNT: u32 = 20;

/// `XDF_IGNORE_WHITESPACE` (xdiff.h:33).
pub(crate) const XDF_IGNORE_WHITESPACE: u32 = 1 << 1;
/// `XDF_IGNORE_WHITESPACE_CHANGE` (xdiff.h:34).
pub(crate) const XDF_IGNORE_WHITESPACE_CHANGE: u32 = 1 << 2;
/// `XDF_IGNORE_WHITESPACE_AT_EOL` (xdiff.h:35).
pub(crate) const XDF_IGNORE_WHITESPACE_AT_EOL: u32 = 1 << 3;
/// `XDF_IGNORE_CR_AT_EOL` (xdiff.h:36).
const XDF_IGNORE_CR_AT_EOL: u32 = 1 << 4;
/// `XDF_WHITESPACE_FLAGS` (xdiff.h:37).
const XDF_WHITESPACE_FLAGS: u32 =
    XDF_IGNORE_WHITESPACE | XDF_IGNORE_WHITESPACE_CHANGE | XDF_IGNORE_WHITESPACE_AT_EOL | XDF_IGNORE_CR_AT_EOL;
/// `COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE` (diff.h:407).
pub(crate) const COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE: u32 = 1 << 5;
/// `COLOR_MOVED_WS_ERROR` (diff.h:408): the sentinel `parse_color_moved_ws()` ORs
/// in when it has already reported a bad mode, which the callers test for.
pub(crate) const COLOR_MOVED_WS_ERROR: u32 = 1 << 0;

/// `parse_color_moved()`: the boolean spellings first (`git_parse_maybe_bool`), then
/// the mode names. `None` is git's `error()` return.
pub(crate) fn parse_color_moved(arg: &str) -> Option<ColorMoved> {
    match crate::optint::maybe_bool(arg) {
        Some(false) => return Some(ColorMoved::No),
        Some(true) => return Some(COLOR_MOVED_DEFAULT),
        None => {}
    }
    match arg {
        "no" => Some(ColorMoved::No),
        "plain" => Some(ColorMoved::Plain),
        "blocks" => Some(ColorMoved::Blocks),
        "zebra" => Some(ColorMoved::Zebra),
        "default" => Some(COLOR_MOVED_DEFAULT),
        "dimmed-zebra" | "dimmed_zebra" => Some(ColorMoved::ZebraDim),
        _ => None,
    }
}

/// `parse_color_moved_ws()`: a comma-separated mode list. A bad mode, or
/// `allow-indentation-change` mixed with any of the three xdiff whitespace modes,
/// leaves [`COLOR_MOVED_WS_ERROR`] set in the result.
pub(crate) fn parse_color_moved_ws(arg: &str) -> u32 {
    let mut ret = 0u32;
    // `string_list_split_f(&l, arg, ",", -1, STRING_LIST_SPLIT_TRIM)`.
    for tok in arg.split(',') {
        match tok.trim() {
            "no" => ret = 0,
            "ignore-space-change" => ret |= XDF_IGNORE_WHITESPACE_CHANGE,
            "ignore-space-at-eol" => ret |= XDF_IGNORE_WHITESPACE_AT_EOL,
            "ignore-all-space" => ret |= XDF_IGNORE_WHITESPACE,
            "allow-indentation-change" => ret |= COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE,
            _ => ret |= COLOR_MOVED_WS_ERROR,
        }
    }
    if (ret & COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE) != 0 && (ret & XDF_WHITESPACE_FLAGS) != 0 {
        ret |= COLOR_MOVED_WS_ERROR;
    }
    ret
}

/// `diff.colorMoved` (`git_diff_ui_config()`), which seeds `--color-moved`'s
/// argument-less form. An unparsable value is git's `-1` return, reported by the
/// caller; `None` means the key is unset.
pub(crate) fn color_moved_cfg(repo: &gix::Repository) -> Option<Result<ColorMoved, String>> {
    let raw = repo.config_snapshot().string("diff.colorMoved")?.to_string();
    Some(parse_color_moved(&raw).ok_or(raw))
}

/// `diff.colorMovedWS` (`git_diff_ui_config()`). `Err` carries the value git
/// rejects, which it reports before failing the whole run.
pub(crate) fn color_moved_ws_cfg(repo: &gix::Repository) -> Option<Result<u32, String>> {
    let raw = repo.config_snapshot().string("diff.colorMovedWS")?.to_string();
    let v = parse_color_moved_ws(&raw);
    Some(if (v & COLOR_MOVED_WS_ERROR) != 0 { Err(raw) } else { Ok(v) })
}

// ---------------------------------------------------------------------------
// --word-diff / --color-words (diff.c `diff_words_*`)
// ---------------------------------------------------------------------------

/// git's `enum diff_words_type`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum WordDiff {
    /// `none` — an ordinary line diff.
    #[default]
    None,
    /// `porcelain` — `+word` / `-word` / ` word` records terminated by `~`.
    Porcelain,
    /// `plain` — `{+added+}` / `[-removed-]` inline markers.
    Plain,
    /// `color` — no markers at all; the color alone distinguishes the sides.
    Color,
}

/// One `struct diff_words_style_elem`: the literal wrapper around a run of words
/// and, when coloring is on, the SGR sequence that also wraps it.
struct WordStyleElem {
    prefix: &'static str,
    suffix: &'static str,
    color: String,
}

/// One row of `diff_words_styles[]` (diff.c:1993).
struct WordStyle {
    new_word: WordStyleElem,
    old_word: WordStyleElem,
    ctx: WordStyleElem,
    newline: &'static str,
}

impl WordStyle {
    /// The row for `mode`, with `init_diff_words_data()`'s color assignment applied.
    /// git writes the colors into the shared static table whenever `want_color()`
    /// holds, so `--word-diff=plain --color` really does emit colored `{+…+}`.
    fn new(mode: WordDiff, colors: &DiffColors) -> Self {
        let (new_word, old_word, ctx, newline) = match mode {
            WordDiff::Porcelain => (("+", "\n"), ("-", "\n"), (" ", "\n"), "~\n"),
            WordDiff::Plain => (("{+", "+}"), ("[-", "-]"), ("", ""), "\n"),
            _ => (("", ""), ("", ""), ("", ""), "\n"),
        };
        let paint = |slot: DiffSlot| {
            if colors.enabled() {
                colors.get(slot).to_string()
            } else {
                String::new()
            }
        };
        WordStyle {
            new_word: WordStyleElem { prefix: new_word.0, suffix: new_word.1, color: paint(DiffSlot::New) },
            old_word: WordStyleElem { prefix: old_word.0, suffix: old_word.1, color: paint(DiffSlot::Old) },
            ctx: WordStyleElem { prefix: ctx.0, suffix: ctx.1, color: paint(DiffSlot::Context) },
            newline,
        }
    }
}

/// `diff.wordRegex` — the last fallback `init_diff_words_data()` consults once the
/// command line and the userdiff driver have both come up empty.
pub(crate) fn word_regex_cfg(repo: &gix::Repository) -> Option<String> {
    repo.config_snapshot().string("diff.wordRegex").map(|v| v.to_string())
}

/// Compile a word regex the way git's `regcomp(..., REG_EXTENDED | REG_NEWLINE)`
/// does: on bytes, without Unicode mode, and with `^`/`$` anchoring at embedded
/// newlines while `.` stops at them.
pub(crate) fn compile_word_regex(pat: &str) -> Result<regex::bytes::Regex, String> {
    regex::bytes::RegexBuilder::new(pat)
        .unicode(false)
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

/// The `--color-moved` / `--word-diff` state, kept apart from [`PaintOptions`] so
/// the plain colorizer keeps its existing shape.
#[derive(Clone, Default)]
pub(crate) struct ExtraPaint {
    /// `o->color_moved`.
    pub(crate) color_moved: Option<ColorMoved>,
    /// `o->color_moved_ws_handling`.
    pub(crate) color_moved_ws: u32,
    /// `o->word_diff`.
    pub(crate) word_diff: Option<WordDiff>,
    /// `o->word_regex`, already compiled.
    pub(crate) word_regex: Option<regex::bytes::Regex>,
}

impl ExtraPaint {
    /// `o->color_moved`, defaulting to `COLOR_MOVED_NO`.
    fn moved(&self) -> ColorMoved {
        self.color_moved.unwrap_or(ColorMoved::No)
    }

    /// `o->word_diff`, defaulting to `DIFF_WORDS_NONE`.
    fn words(&self) -> WordDiff {
        self.word_diff.unwrap_or(WordDiff::None)
    }

    /// Whether the assembled patch has to be re-emitted even with color off.
    pub(crate) fn rewrites_uncolored(&self) -> bool {
        self.words() != WordDiff::None
    }
}

/// What the `--color-moved` and `--word-diff` family of flags accumulated on the
/// command line, before the configuration defaults they layer over are known.
///
/// The five diff commands parse their options in different places relative to
/// opening the repository, so the flag state is collected here and turned into an
/// [`ExtraPaint`] by [`MoveWordOpts::resolve`] once a repository is in hand.
#[derive(Default)]
pub(crate) struct MoveWordOpts {
    /// `--color-moved[=<mode>]` / `--no-color-moved`. The outer `None` means no
    /// flag was given; the inner `None` is the argument-less form, which defers to
    /// `diff.colorMoved` and only then to `COLOR_MOVED_DEFAULT`.
    moved: Option<Option<ColorMoved>>,
    /// `--color-moved-ws=<modes>` / `--no-color-moved-ws`.
    moved_ws: Option<u32>,
    /// `--word-diff[=<mode>]` / `--color-words[=<re>]` / `--word-diff-regex=<re>`.
    word_diff: WordDiff,
    /// `o->word_regex` as spelled on the command line.
    word_regex: Option<String>,
}

/// parse-options' complaint about an option given without its value: a `--long-name`
/// is an "option", a single-letter `-x` is a "switch", and the name loses its dashes.
/// The caller prefixes `error: ` and exits 129.
pub(crate) fn missing_value(flag: &str) -> String {
    match flag.strip_prefix("--") {
        Some(name) => format!("option `{name}' requires a value"),
        None => format!("switch `{}' requires a value", flag.trim_start_matches('-')),
    }
}

/// The options in this family that parse-options declares with a *required*
/// argument (`OPT_STRING`/`OPT_CALLBACK` without `PARSE_OPT_OPTARG`), so a bare
/// `--color-moved-ws` / `--word-diff-regex` takes the next argv entry as its value
/// and, when there is none, dies with `option \`<name>' requires a value`.
///
/// `--color-moved` and `--word-diff` are `PARSE_OPT_OPTARG` and `--color-words`
/// takes an optional regex, so none of those three belongs here — their bare form
/// is meaningful on its own and [`MoveWordOpts::parse_flag`] already handles it.
pub(crate) fn needs_separate_value(s: &str) -> bool {
    matches!(s, "--color-moved-ws" | "--word-diff-regex")
}

impl MoveWordOpts {
    /// Handle one argument. `None` means the argument belongs to someone else;
    /// `Some(Err(msg))` is the text git writes to stderr before exiting 129.
    ///
    /// `color_when` is threaded through because `--color-words` and
    /// `--word-diff=color` set `options->use_color = GIT_COLOR_ALWAYS` outright,
    /// so a later `--no-color` still wins and an earlier one does not.
    pub(crate) fn parse_flag(
        &mut self,
        s: &str,
        color_when: &mut Option<ColorWhen>,
    ) -> Option<Result<(), String>> {
        match s {
            "--color-moved" => {
                self.moved = Some(None);
            }
            "--no-color-moved" => {
                self.moved = Some(Some(ColorMoved::No));
            }
            "--no-color-moved-ws" => {
                self.moved_ws = Some(0);
            }
            // `diff_opt_word_diff()` with no argument only promotes a diff that is
            // not already a word diff.
            "--word-diff" => {
                if self.word_diff == WordDiff::None {
                    self.word_diff = WordDiff::Plain;
                }
            }
            "--color-words" => {
                *color_when = Some(ColorWhen::Always);
                self.word_diff = WordDiff::Color;
                self.word_regex = None;
            }
            _ if s.starts_with("--color-moved=") => {
                let arg = &s["--color-moved=".len()..];
                match parse_color_moved(arg) {
                    Some(m) => self.moved = Some(Some(m)),
                    None => {
                        return Some(Err(format!(
                            "error: color moved setting must be one of 'no', 'default', \
                             'blocks', 'zebra', 'dimmed-zebra', 'plain'\n\
                             error: bad --color-moved argument: {arg}"
                        )))
                    }
                }
            }
            _ if s.starts_with("--color-moved-ws=") => {
                let arg = &s["--color-moved-ws=".len()..];
                let v = parse_color_moved_ws(arg);
                if (v & COLOR_MOVED_WS_ERROR) != 0 {
                    return Some(Err(format!(
                        "{}error: invalid mode '{arg}' in --color-moved-ws",
                        color_moved_ws_diagnostics(arg)
                    )));
                }
                self.moved_ws = Some(v);
            }
            _ if s.starts_with("--word-diff=") => {
                let arg = &s["--word-diff=".len()..];
                match arg {
                    "plain" => self.word_diff = WordDiff::Plain,
                    "color" => {
                        *color_when = Some(ColorWhen::Always);
                        self.word_diff = WordDiff::Color;
                    }
                    "porcelain" => self.word_diff = WordDiff::Porcelain,
                    "none" => self.word_diff = WordDiff::None,
                    _ => return Some(Err(format!("error: bad --word-diff argument: {arg}"))),
                }
            }
            _ if s.starts_with("--word-diff-regex=") => {
                if self.word_diff == WordDiff::None {
                    self.word_diff = WordDiff::Plain;
                }
                self.word_regex = Some(s["--word-diff-regex=".len()..].to_string());
            }
            _ if s.starts_with("--color-words=") => {
                *color_when = Some(ColorWhen::Always);
                self.word_diff = WordDiff::Color;
                self.word_regex = Some(s["--color-words=".len()..].to_string());
            }
            _ => return None,
        }
        Some(Ok(()))
    }

    // There is deliberately no `is_active()` "were any of these flags given?"
    // predicate here, and a caller must not reintroduce one as a cheap gate around
    // `resolve()`. These three fields only ever hold what `parse_flag` read off the
    // command line, whereas the paint that actually applies is decided by `resolve()`
    // *layering those flags over config* — `diff.colorMoved` (line 592),
    // `diff.colorMovedWS` (line 599) and `diff.wordRegex` (line 667). So "no flag was
    // given" does not imply "no extra paint applies": `git -c diff.colorMoved=zebra
    // diff` moves-colors with an empty `MoveWordOpts`. Every caller therefore calls
    // `resolve()` unconditionally and branches on the resulting [`ExtraPaint`]
    // (diff.rs:2246, diff_pairs.rs:1656, diff_files.rs:1849, diff_index.rs:1696).

    /// Layer the flags over `diff.colorMoved`, `diff.colorMovedWS` and
    /// `diff.wordRegex`. `Err` carries the message git writes before exiting 128.
    pub(crate) fn resolve(&self, repo: &gix::Repository) -> Result<ExtraPaint, String> {
        // An unparsable configured default is git's `-1` return from
        // `git_diff_ui_config()`; this port has no fatal-config path in the diff
        // commands, so it falls back the way `diff.wsErrorHighlight` already does.
        let cfg_moved = match color_moved_cfg(repo) {
            Some(Ok(m)) => m,
            Some(Err(_)) | None => ColorMoved::No,
        };
        let color_moved = match self.moved {
            None => cfg_moved,
            Some(Some(m)) => m,
            // `diff_opt_color_moved()`'s argument-less arm.
            Some(None) => {
                if cfg_moved == ColorMoved::No {
                    COLOR_MOVED_DEFAULT
                } else {
                    cfg_moved
                }
            }
        };
        let cfg_ws = match color_moved_ws_cfg(repo) {
            Some(Ok(v)) => v,
            Some(Err(_)) | None => 0,
        };
        let color_moved_ws = self.moved_ws.unwrap_or(cfg_ws);

        // `init_diff_words_data()`: the command line first, then the userdiff
        // driver — which is the `default` driver with no word regex for every path
        // this port resolves — and only then `diff.wordRegex`.
        let regex_src = self.word_regex.clone().or_else(|| word_regex_cfg(repo));
        let word_regex = match regex_src {
            Some(pat) => Some(
                compile_word_regex(&pat)
                    .map_err(|_| format!("fatal: invalid regular expression: {pat}"))?,
            ),
            None => None,
        };

        Ok(ExtraPaint {
            color_moved: Some(color_moved),
            color_moved_ws,
            word_diff: Some(self.word_diff),
            word_regex,
        })
    }
}

/// The per-mode diagnostics `parse_color_moved_ws()` prints before its caller
/// reports the whole value as invalid.
fn color_moved_ws_diagnostics(arg: &str) -> String {
    let mut out = String::new();
    let mut ret = 0u32;
    for tok in arg.split(',') {
        let tok = tok.trim();
        match tok {
            "no" => ret = 0,
            "ignore-space-change" => ret |= XDF_IGNORE_WHITESPACE_CHANGE,
            "ignore-space-at-eol" => ret |= XDF_IGNORE_WHITESPACE_AT_EOL,
            "ignore-all-space" => ret |= XDF_IGNORE_WHITESPACE,
            "allow-indentation-change" => ret |= COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE,
            _ => out.push_str(&format!(
                "error: unknown color-moved-ws mode '{tok}', possible values are \
                 'ignore-space-change', 'ignore-space-at-eol', 'ignore-all-space', \
                 'allow-indentation-change'\n"
            )),
        }
    }
    if (ret & COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE) != 0 && (ret & XDF_WHITESPACE_FLAGS) != 0 {
        out.push_str(
            "error: color-moved-ws: allow-indentation-change cannot be combined with \
             other whitespace modes\n",
        );
    }
    out
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
pub(crate) fn ws_check_emit(
    out: &mut Vec<u8>,
    line: &[u8],
    ws_rule: u32,
    set: &str,
    reset: &str,
    ws: &str,
) {
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
pub(crate) fn is_c_space(b: u8) -> bool {
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

/// `DIFF_SYMBOL_MOVED_LINE` (diff.c:860).
const MOVED_LINE: u32 = 1 << 20;
/// `DIFF_SYMBOL_MOVED_LINE_ALT` (diff.c:861).
const MOVED_LINE_ALT: u32 = 1 << 21;
/// `DIFF_SYMBOL_MOVED_LINE_UNINTERESTING` (diff.c:862).
const MOVED_LINE_UNINTERESTING: u32 = 1 << 22;
/// `DIFF_SYMBOL_MOVED_LINE_ZEBRA_MASK` (diff.c:1191).
const MOVED_LINE_ZEBRA_MASK: u32 = MOVED_LINE | MOVED_LINE_ALT;
/// `INDENT_BLANKLINE` (diff.c:928) — `INT_MIN`, the marker for a line whose body
/// is nothing but whitespace, whose indent therefore constrains nothing.
const INDENT_BLANKLINE: i64 = i32::MIN as i64;

/// The kind of one entry in git's `o->emitted_symbols`, restricted to the symbols
/// an assembled unified patch can decompose into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `DIFF_SYMBOL_HEADER` and the file-pair label lines.
    Meta,
    /// `DIFF_SYMBOL_CONTEXT_FRAGINFO` — the `@@ … @@` line.
    Frag,
    /// `DIFF_SYMBOL_PLUS`.
    Plus,
    /// `DIFF_SYMBOL_MINUS`.
    Minus,
    /// `DIFF_SYMBOL_CONTEXT`.
    Context,
    /// `DIFF_SYMBOL_CONTEXT_INCOMPLETE` — `\ No newline at end of file`.
    Incomplete,
    /// A line no symbol claims, written through untouched.
    Raw,
    /// `DIFF_SYMBOL_WORD_DIFF` — word-diff bytes that already carry their own
    /// markers and colors, written through untouched.
    WordRaw,
    /// `DIFF_SYMBOL_WORDS` — a context line in `plain`/`color` word-diff mode,
    /// whose sign byte is dropped.
    WordsCtx,
    /// `DIFF_SYMBOL_WORDS_PORCELAIN` — a context line in `porcelain` mode, kept
    /// whole and followed by the record terminator.
    WordsPorcelainCtx,
}

/// One entry of `o->emitted_symbols`.
struct Sym {
    kind: Kind,
    /// For [`Kind::Plus`], [`Kind::Minus`] and [`Kind::Context`] the content with
    /// its sign byte already stripped; for everything else the bytes as they stand.
    line: Vec<u8>,
    /// `flags & WS_RULE_MASK`.
    ws_rule: u32,
    /// `o->ws_error_highlight & <this line's side>`.
    highlight: bool,
    /// `DIFF_SYMBOL_CONTENT_BLANK_LINE_EOF`.
    blank_at_eof: bool,
    /// `o->output_indicators[...]`, or 0 when `diff.suppressBlankEmpty` drops it.
    sign: u8,
    /// The interned line identity `add_lines_to_move_detection()` assigns.
    id: u32,
    /// `DIFF_SYMBOL_MOVED_LINE` and friends.
    flags: u32,
    /// `es->indent_off`.
    indent_off: usize,
    /// `es->indent_width`.
    indent_width: i64,
}

impl Sym {
    /// A symbol that carries only bytes: no whitespace state, no move identity.
    fn plain(kind: Kind, line: &[u8]) -> Self {
        Sym {
            kind,
            line: line.to_vec(),
            ws_rule: 0,
            highlight: false,
            blank_at_eof: false,
            sign: 0,
            id: 0,
            flags: 0,
            indent_off: 0,
            indent_width: 0,
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
    colorize_patch_ex(patch, colors, opts, files, default_file, &ExtraPaint::default())
}

/// [`colorize_patch`] with `--color-moved` and `--word-diff` in play.
///
/// This mirrors `diff_flush_patch_all_file_pairs()`: the whole patch is decomposed
/// into the symbol list git accumulates in `o->emitted_symbols`, the move detector
/// runs over that list, and only then is every symbol written out. Feeding it the
/// entire patch rather than one file section at a time is what lets a block moved
/// *between* files be recognized, exactly as git recognizes it.
pub(crate) fn colorize_patch_ex(
    patch: &[u8],
    colors: &DiffColors,
    opts: &PaintOptions,
    files: &[FilePaint],
    default_file: FilePaint,
    extra: &ExtraPaint,
) -> Vec<u8> {
    let word_diff = extra.words();
    // With nothing to paint and no word diff to compute, the patch is already
    // exactly what git would print.
    if !colors.enabled() && word_diff == WordDiff::None {
        return patch.to_vec();
    }
    let mut syms = build_syms(patch, colors, opts, files, default_file, extra);
    // `o->emitted_symbols` is only allocated when both are true, so the detector
    // never runs — and no line is ever marked — with color off.
    if colors.enabled() && extra.moved() != ColorMoved::No {
        mark_color_as_moved(&mut syms, extra.moved(), extra.color_moved_ws);
        if extra.moved() == ColorMoved::ZebraDim {
            dim_moved_lines(&mut syms);
        }
    }
    emit_syms(&syms, colors)
}

/// `fn_out_consume()`: split the assembled patch into the symbol list, taking the
/// word-diff branch when one is active.
fn build_syms(
    patch: &[u8],
    colors: &DiffColors,
    opts: &PaintOptions,
    files: &[FilePaint],
    default_file: FilePaint,
    extra: &ExtraPaint,
) -> Vec<Sym> {
    let (ind_new, ind_old, ind_ctx) = opts.indicators;
    let ws_error_highlight = opts.ws_error_highlight;
    let word_diff = extra.words();
    let style = WordStyle::new(word_diff, colors);

    let mut syms: Vec<Sym> = Vec::new();
    let mut file_no: usize = 0;
    let mut cur = default_file;
    let mut in_hunk = false;
    let mut lno_pre = 0usize;
    let mut lno_post = 0usize;
    let mut last_kind = ind_ctx;
    // The `diff_words->minus` / `->plus` accumulators.
    let mut words = WordsPair::default();

    for line in split_keep_terminator(patch) {
        let first = line.first().copied().unwrap_or(0);
        let is_content = first == ind_new || first == ind_old || first == ind_ctx || first == b'\\';
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
                // `if (ecbdata->diff_words) diff_words_flush(ecbdata);`
                words.flush(&mut syms, &style, extra);
                syms.push(Sym::plain(Kind::Frag, line));
                continue;
            }
            // A header ends the file pair, which is where `free_diff_words_data()`
            // flushes whatever the last hunk left behind.
            words.flush(&mut syms, &style, extra);
            syms.push(Sym::plain(Kind::Meta, line));
            continue;
        }

        // `fn_out_consume()`'s word-diff branch runs before its sign switch: `+`
        // and `-` lines are swallowed into the buffers, the incomplete-line marker
        // is eaten outright, and anything else flushes and is emitted as a word
        // record.
        if word_diff != WordDiff::None {
            // `fn_out_consume()` tests `line[0] == '@'` ahead of its word-diff
            // branch, so a hunk header that directly follows content still flushes
            // the buffers and prints as a hunk header.
            if first == b'@' {
                let (a, b) = find_lno(line);
                lno_pre = a;
                lno_post = b;
                last_kind = ind_ctx;
                words.flush(&mut syms, &style, extra);
                syms.push(Sym::plain(Kind::Frag, line));
                continue;
            }
            if first == ind_old {
                words.minus.text.extend_from_slice(&line[1..]);
                continue;
            }
            if first == ind_new {
                words.plus.text.extend_from_slice(&line[1..]);
                continue;
            }
            if line.starts_with(b"\\ ") {
                continue;
            }
            words.flush(&mut syms, &style, extra);
            let kind = if word_diff == WordDiff::Porcelain {
                Kind::WordsPorcelainCtx
            } else {
                Kind::WordsCtx
            };
            syms.push(Sym::plain(kind, line));
            continue;
        }

        // `fn_out_consume()` dispatches on the sign byte. The three signs are
        // compared before `@`, so an `--output-indicator-*` that reuses `@` still
        // reads as a content line, exactly as git's `switch` would.
        match first {
            c if c == ind_new => {
                lno_post += 1;
                let blank_at_eof = new_blank_line_at_eof(&cur, lno_pre, lno_post, &line[1..]);
                syms.push(Sym {
                    blank_at_eof,
                    highlight: (ws_error_highlight & WSEH_NEW) != 0,
                    sign: ind_new,
                    ws_rule: cur.ws_rule,
                    ..Sym::plain(Kind::Plus, &line[1..])
                });
                last_kind = ind_new;
            }
            c if c == ind_old => {
                lno_pre += 1;
                syms.push(Sym {
                    highlight: (ws_error_highlight & WSEH_OLD) != 0,
                    sign: ind_old,
                    ws_rule: cur.ws_rule,
                    ..Sym::plain(Kind::Minus, &line[1..])
                });
                last_kind = ind_old;
            }
            c if c == ind_ctx => {
                lno_pre += 1;
                lno_post += 1;
                let body = &line[1..];
                // `emit_line_ws_markup()` drops the sign of an empty context line
                // under `diff.suppressBlankEmpty`, which leaves nothing at all to
                // paint and so emits a bare newline.
                let sign = if opts.suppress_blank_empty && body == b"\n" { 0 } else { ind_ctx };
                syms.push(Sym {
                    highlight: (ws_error_highlight & WSEH_CONTEXT) != 0,
                    sign,
                    ws_rule: cur.ws_rule,
                    ..Sym::plain(Kind::Context, body)
                });
                last_kind = ind_ctx;
            }
            b'@' => {
                let (a, b) = find_lno(line);
                lno_pre = a;
                lno_post = b;
                last_kind = ind_ctx;
                syms.push(Sym::plain(Kind::Frag, line));
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
                let highlight =
                    (cur.ws_rule & WS_INCOMPLETE_LINE) != 0 && (ws_error_highlight & side) != 0;
                syms.push(Sym { highlight, ..Sym::plain(Kind::Incomplete, line) });
            }
            _ => syms.push(Sym::plain(Kind::Raw, line)),
        }
    }
    words.flush(&mut syms, &style, extra);
    syms
}

/// Write the symbol list out, choosing each line's color the way
/// `emit_diff_symbol_from_struct()` does.
fn emit_syms(syms: &[Sym], colors: &DiffColors) -> Vec<u8> {
    let on = colors.enabled();
    let reset = colors.reset();
    let meta = colors.get(DiffSlot::Meta);
    let context = colors.get(DiffSlot::Context);
    let frag = colors.get(DiffSlot::Frag);
    let func = colors.get(DiffSlot::Func);

    let mut out: Vec<u8> = Vec::with_capacity(syms.len() * 48);
    for s in syms {
        match s.kind {
            Kind::Meta => emit_header_line(&mut out, &s.line, meta, reset),
            Kind::Frag => emit_hunk_header(&mut out, on, &s.line, frag, context, func, reset),
            Kind::Plus | Kind::Minus | Kind::Context => {
                let ck = match s.kind {
                    Kind::Plus => ContentKind::Plus,
                    Kind::Minus => ContentKind::Minus,
                    _ => ContentKind::Context,
                };
                // `dual` is false for every command that reaches this re-emitter:
                // `o->flags.dual_color_diffed_diffs` is set in exactly one place,
                // range-diff's `output()` (range-diff.c:524-525), and range-diff
                // paints its diff-of-diffs line by line rather than through here.
                emit_content_symbol(
                    &mut out,
                    colors,
                    false,
                    ck,
                    s.flags,
                    s.sign,
                    &s.line,
                    s.ws_rule,
                    s.highlight,
                    s.blank_at_eof,
                );
            }
            Kind::Incomplete => {
                let set = match s.highlight {
                    true => colors.get(DiffSlot::Whitespace),
                    false => context,
                };
                emit_line(&mut out, on, set, reset, &s.line);
            }
            Kind::Raw | Kind::WordRaw => out.extend_from_slice(&s.line),
            // `line++; len--;` — the sign byte never reaches the output.
            Kind::WordsCtx => emit_line(&mut out, on, context, reset, &s.line[1..]),
            Kind::WordsPorcelainCtx => {
                emit_line(&mut out, on, context, reset, &s.line);
                out.extend_from_slice(b"~\n");
            }
        }
    }
    out
}

/// The `DIFF_SYMBOL_PLUS` / `DIFF_SYMBOL_MINUS` color switch (diff.c:1459, 1504),
/// keyed on the three move flags. Context lines never carry them.
fn content_slot(kind: ContentKind, flags: u32) -> DiffSlot {
    let mv = flags & (MOVED_LINE | MOVED_LINE_ALT | MOVED_LINE_UNINTERESTING);
    let alt = MOVED_LINE | MOVED_LINE_ALT;
    let alt_dim = alt | MOVED_LINE_UNINTERESTING;
    let dim = MOVED_LINE | MOVED_LINE_UNINTERESTING;
    match kind {
        ContentKind::Plus => match mv {
            m if m == alt_dim => DiffSlot::NewMovedAltDim,
            m if m == alt => DiffSlot::NewMovedAlt,
            m if m == dim => DiffSlot::NewMovedDim,
            m if m == MOVED_LINE => DiffSlot::NewMoved,
            _ => DiffSlot::New,
        },
        ContentKind::Minus => match mv {
            m if m == alt_dim => DiffSlot::OldMovedAltDim,
            m if m == alt => DiffSlot::OldMovedAlt,
            m if m == dim => DiffSlot::OldMovedDim,
            m if m == MOVED_LINE => DiffSlot::OldMoved,
            _ => DiffSlot::Old,
        },
        _ => DiffSlot::Context,
    }
}

// ---------------------------------------------------------------------------
// word diff (diff.c `diff_words_fill` … `diff_words_show`)
// ---------------------------------------------------------------------------

/// One `struct diff_words_buffer`: the concatenated content of one side of the
/// hunk, plus the `(begin, end)` span of every word found in it.
#[derive(Default)]
struct WordsBuffer {
    text: Vec<u8>,
    /// `buffer->orig`. Index 0 is git's fake empty "0th" word at the text start,
    /// so the word xdiff calls record *k* lives at `orig[k + 1]`.
    orig: Vec<(usize, usize)>,
}

/// The `diff_words->minus` / `->plus` pair, and the flush that turns them into
/// output.
#[derive(Default)]
struct WordsPair {
    minus: WordsBuffer,
    plus: WordsBuffer,
}

impl WordsPair {
    /// `diff_words_flush()`: run the word diff if either side holds anything.
    fn flush(&mut self, syms: &mut Vec<Sym>, style: &WordStyle, extra: &ExtraPaint) {
        if extra.words() == WordDiff::None {
            return;
        }
        if self.minus.text.is_empty() && self.plus.text.is_empty() {
            return;
        }
        let mut out: Vec<u8> = Vec::new();
        self.show(&mut out, style, extra.word_regex.as_ref());
        if !out.is_empty() {
            syms.push(Sym::plain(Kind::WordRaw, &out));
        }
    }

    /// `diff_words_show()`.
    fn show(&mut self, out: &mut Vec<u8>, style: &WordStyle, re: Option<&regex::bytes::Regex>) {
        // Special case: only removal.
        if self.plus.text.is_empty() {
            let minus = std::mem::take(&mut self.minus.text);
            write_helper(out, &style.old_word, style.newline, &minus);
            self.minus.orig.clear();
            return;
        }

        diff_words_fill(&mut self.minus, re);
        diff_words_fill(&mut self.plus, re);
        // The word lists xdiff compares: one record per word, in order.
        let minus_words: Vec<&[u8]> =
            self.minus.orig[1..].iter().map(|(b, e)| &self.minus.text[*b..*e]).collect();
        let plus_words: Vec<&[u8]> =
            self.plus.orig[1..].iter().map(|(b, e)| &self.plus.text[*b..*e]).collect();

        let mut current_plus = 0usize;
        for (i1, n1, i2, n2) in word_hunks(&minus_words, &plus_words) {
            // `xdl_emit_hunk_hdr()` hands the callback a 1-based start, decremented
            // again when the side is empty — which is exactly the index of the fake
            // 0th word when the change sits at the very beginning.
            let minus_first = if n1 != 0 { i1 + 1 } else { i1 };
            let plus_first = if n2 != 0 { i2 + 1 } else { i2 };
            let (minus_begin, minus_end) = if n1 != 0 {
                (self.minus.orig[minus_first].0, self.minus.orig[minus_first + n1 - 1].1)
            } else {
                let at = self.minus.orig[minus_first].1;
                (at, at)
            };
            let (plus_begin, plus_end) = if n2 != 0 {
                (self.plus.orig[plus_first].0, self.plus.orig[plus_first + n2 - 1].1)
            } else {
                let at = self.plus.orig[plus_first].1;
                (at, at)
            };

            if current_plus != plus_begin {
                write_helper(out, &style.ctx, style.newline, &self.plus.text[current_plus..plus_begin]);
            }
            if minus_begin != minus_end {
                write_helper(out, &style.old_word, style.newline, &self.minus.text[minus_begin..minus_end]);
            }
            if plus_begin != plus_end {
                write_helper(out, &style.new_word, style.newline, &self.plus.text[plus_begin..plus_end]);
            }
            current_plus = plus_end;
        }
        if current_plus != self.plus.text.len() {
            write_helper(out, &style.ctx, style.newline, &self.plus.text[current_plus..]);
        }
        self.minus = WordsBuffer::default();
        self.plus = WordsBuffer::default();
    }
}

/// `fn_out_diff_words_write_helper()`: wrap each newline-free run of `buf` in the
/// style element's color and literal markers, and separate the runs with the
/// style's record terminator.
///
/// git batches the pieces into `DIFF_SYMBOL_WORD_DIFF` symbols so that a
/// `--graph` prefix can be inserted between them; that prefix is empty for these
/// commands, so appending straight to the buffer produces the same bytes.
fn write_helper(out: &mut Vec<u8>, st: &WordStyleElem, newline: &str, buf: &[u8]) {
    let mut buf = buf;
    while !buf.is_empty() {
        let nl = buf.iter().position(|b| *b == b'\n');
        let content = nl.unwrap_or(buf.len());
        if content != 0 {
            if !st.color.is_empty() {
                out.extend_from_slice(st.color.as_bytes());
            }
            out.extend_from_slice(st.prefix.as_bytes());
            out.extend_from_slice(&buf[..content]);
            out.extend_from_slice(st.suffix.as_bytes());
            if !st.color.is_empty() {
                out.extend_from_slice(RESET.as_bytes());
            }
        }
        let Some(nl) = nl else { return };
        out.extend_from_slice(newline.as_bytes());
        buf = &buf[nl + 1..];
    }
}

/// `diff_words_fill()`: record the span of every word in `buffer.text`.
fn diff_words_fill(buffer: &mut WordsBuffer, re: Option<&regex::bytes::Regex>) {
    // The fake empty "0th" word at the start of the text.
    buffer.orig.clear();
    buffer.orig.push((0, 0));
    let text = &buffer.text;
    let mut i = 0usize;
    while i < text.len() {
        let Some((begin, end)) = find_word_boundaries(text, re, i) else {
            return;
        };
        buffer.orig.push((begin, end));
        // `i = j - 1` followed by the loop's own `i++`.
        i = end;
    }
}

/// `find_word_boundaries()`: the span of the next word at or after `begin`, or
/// `None` once the text is exhausted.
fn find_word_boundaries(
    text: &[u8],
    re: Option<&regex::bytes::Regex>,
    mut begin: usize,
) -> Option<(usize, usize)> {
    while let Some(re) = re {
        if begin >= text.len() {
            break;
        }
        let m = re.find(&text[begin..])?;
        // A match that spans a newline is cut at it, so no word ever straddles a
        // line: the word list xdiff sees is one word per record.
        let end = match text[begin + m.start()..begin + m.end()].iter().position(|b| *b == b'\n') {
            Some(p) => begin + m.start() + p,
            None => begin + m.end(),
        };
        begin += m.start();
        if begin == end {
            // An empty match cannot advance; step over one byte and retry.
            begin += 1;
        } else {
            return (begin < end).then_some((begin, end));
        }
    }

    // The default splitting, used whenever no word regex is in force: a word is a
    // maximal run of non-whitespace.
    while begin < text.len() && is_c_space(text[begin]) {
        begin += 1;
    }
    if begin >= text.len() {
        return None;
    }
    let mut end = begin + 1;
    while end < text.len() && !is_c_space(text[end]) {
        end += 1;
    }
    Some((begin, end))
}

/// `xdi_diff_outf(&minus, &plus, fn_out_diff_words_aux, …)` with `xecfg.ctxlen`
/// zero: the change script over the two word lists, as `(i1, n1, i2, n2)` with
/// zero-based record starts.
fn word_hunks(before: &[&[u8]], after: &[&[u8]]) -> Vec<(usize, usize, usize, usize)> {
    use gix::diff::blob::{Algorithm, Diff, InternedInput};
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|w| w.to_vec()));
    input.update_after(after.iter().map(|w| w.to_vec()));
    // `xpp.flags` is zero here, so this is plain Myers with xdiff's ordinary
    // change compaction and no indent heuristic.
    let mut d = Diff::compute(Algorithm::Myers, &input);
    d.postprocess_no_heuristic(&input);
    d.hunks()
        .map(|h| {
            (
                h.before.start as usize,
                h.before.len(),
                h.after.start as usize,
                h.after.len(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// moved-block detection (diff.c `add_lines_to_move_detection` … `dim_moved_lines`)
// ---------------------------------------------------------------------------

/// One `struct moved_entry`, with git's two pointer chains expressed as indices
/// into the arena that holds them.
struct MovedEntry {
    /// The symbol this entry stands for.
    sym: usize,
    /// The next entry of the same side directly below this one, if any.
    next_line: Option<usize>,
    /// The next entry anywhere with the same interned line, same side.
    next_match: Option<usize>,
}

/// One `struct moved_block`: the entry the block has advanced to, and the block's
/// whitespace delta under `allow-indentation-change`.
#[derive(Clone, Copy)]
struct MovedBlock {
    entry: usize,
    wsd: i64,
}

/// `fill_es_indent_data()`: the byte offset at which the line's content begins and
/// the visual width of the indentation before it, or [`INDENT_BLANKLINE`] when the
/// line has no content at all.
fn fill_es_indent_data(s: &mut Sym) {
    let line = &s.line;
    let len = line.len();
    let at = |i: usize| line.get(i).copied().unwrap_or(0);
    let tab_width = {
        let w = (s.ws_rule & WS_TAB_WIDTH_MASK) as usize;
        if w == 0 { 1 } else { w }
    };

    // Skip any \v \f \r at the start of the indentation.
    let mut off = 0usize;
    while at(off) == 0x0c || at(off) == 0x0b || (off + 1 < len && at(off) == b'\r') {
        off += 1;
    }

    // The visual width of the indentation.
    let mut width = 0usize;
    loop {
        if at(off) == b' ' {
            width += 1;
            off += 1;
        } else if at(off) == b'\t' {
            width += tab_width - (width % tab_width);
            loop {
                off += 1;
                if at(off) != b'\t' {
                    break;
                }
                width += tab_width;
            }
        } else {
            break;
        }
    }

    // A line whose remainder is entirely whitespace constrains no indentation.
    if line[off.min(len)..].iter().all(|b| is_c_space(*b)) {
        s.indent_width = INDENT_BLANKLINE;
        s.indent_off = len;
    } else {
        s.indent_off = off;
        s.indent_width = width as i64;
    }
}

/// `compute_ws_delta()`.
fn compute_ws_delta(a_width: i64, b_width: i64) -> i64 {
    if a_width == INDENT_BLANKLINE && b_width == INDENT_BLANKLINE {
        return INDENT_BLANKLINE;
    }
    a_width - b_width
}

/// `xdl_hash_record_with_whitespace()`'s canonical form of a line, which is what
/// makes two lines hash — and, per `xdl_recmatch()`, compare — equal. With no
/// whitespace flag in force that is the bytes themselves; every flag drops or
/// rewrites some run of whitespace up to the first newline.
fn moved_key(line: &[u8], indent_off: usize, ws: u32) -> Vec<u8> {
    let s = &line[indent_off.min(line.len())..];
    let flags = ws & XDF_WHITESPACE_FLAGS;
    if flags == 0 {
        // `xdl_recmatch()` short-circuits on `memcmp`, so identity is the key.
        return s.to_vec();
    }
    let cr_at_eol_only = flags == XDF_IGNORE_CR_AT_EOL;
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() && s[i] != b'\n' {
        if cr_at_eol_only {
            if s[i] == b'\r' && i + 1 < s.len() && s[i + 1] == b'\n' {
                i += 1;
                continue;
            }
        } else if is_c_space(s[i]) {
            let start = i;
            while i + 1 < s.len() && is_c_space(s[i + 1]) && s[i + 1] != b'\n' {
                i += 1;
            }
            let at_eol = i + 1 >= s.len() || s[i + 1] == b'\n';
            if (flags & XDF_IGNORE_WHITESPACE) != 0 {
                // Every whitespace byte is dropped.
            } else if (flags & XDF_IGNORE_WHITESPACE_CHANGE) != 0 && !at_eol {
                out.push(b' ');
            } else if (flags & XDF_IGNORE_WHITESPACE_AT_EOL) != 0 && !at_eol {
                out.extend_from_slice(&s[start..=i]);
            }
            i += 1;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `add_lines_to_move_detection()`: intern every `+`/`-` line, chain the entries
/// that sit directly below one another on the same side, and bucket them by line
/// identity and side.
fn add_lines_to_move_detection(
    syms: &mut [Sym],
    ws: u32,
) -> (Vec<MovedEntry>, Vec<(Option<usize>, Option<usize>)>) {
    let mut arena: Vec<MovedEntry> = Vec::new();
    // `entry_list[id]` = (add, del).
    let mut lists: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    let mut interned: std::collections::HashMap<Vec<u8>, u32> = std::collections::HashMap::new();
    let mut prev_line: Option<usize> = None;

    for n in 0..syms.len() {
        let kind = syms[n].kind;
        if kind != Kind::Plus && kind != Kind::Minus {
            prev_line = None;
            continue;
        }
        if (ws & COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE) != 0 {
            fill_es_indent_data(&mut syms[n]);
        }
        let key = moved_key(&syms[n].line, syms[n].indent_off, ws);
        let id = *interned.entry(key).or_insert_with(|| {
            lists.push((None, None));
            (lists.len() - 1) as u32
        });
        syms[n].id = id;

        let e = arena.len();
        arena.push(MovedEntry { sym: n, next_line: None, next_match: None });
        if let Some(p) = prev_line {
            if syms[arena[p].sym].kind == kind {
                arena[p].next_line = Some(e);
            }
        }
        prev_line = Some(e);

        let bucket = &mut lists[id as usize];
        if kind == Kind::Plus {
            arena[e].next_match = bucket.0;
            bucket.0 = Some(e);
        } else {
            arena[e].next_match = bucket.1;
            bucket.1 = Some(e);
        }
    }
    (arena, lists)
}

/// `cmp_in_block_with_wsd()`: whether `cur` fails to continue the block that `l`
/// is being tested against. Returns git's non-zero "does not match".
fn cmp_in_block_with_wsd(syms: &[Sym], cur: usize, l: usize, wsd: &mut i64) -> bool {
    let a_width = syms[cur].indent_width;
    let b_width = syms[l].indent_width;
    // The text of each line must match.
    if syms[cur].id != syms[l].id {
        return true;
    }
    // Two blank lines constrain nothing, and the text already matched.
    if a_width == INDENT_BLANKLINE {
        return false;
    }
    let delta = b_width - a_width;
    // A block that has been blank so far takes this line's delta as its own.
    if *wsd == INDENT_BLANKLINE {
        *wsd = delta;
    }
    delta != *wsd
}

/// `pmb_advance_or_null()`: drop every potential block that this line fails to
/// continue, advancing the survivors by one entry.
fn pmb_advance_or_null(syms: &[Sym], arena: &[MovedEntry], l: usize, pmb: &mut Vec<MovedBlock>, ws: u32) {
    let mut j = 0usize;
    for i in 0..pmb.len() {
        let cur = arena[pmb[i].entry].next_line;
        let matched = match cur {
            None => false,
            Some(c) => {
                if (ws & COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE) != 0 {
                    !cmp_in_block_with_wsd(syms, arena[c].sym, l, &mut pmb[i].wsd)
                } else {
                    syms[arena[c].sym].id == syms[l].id
                }
            }
        };
        if matched {
            // `pmb[j] = pmb[i]` carries the whitespace delta the compare may have
            // just filled in, then advances the block.
            pmb[j] = MovedBlock { entry: cur.expect("matched implies an entry"), wsd: pmb[i].wsd };
            j += 1;
        }
    }
    pmb.truncate(j);
}

/// `fill_potential_moved_blocks()`: this line starts a new block, so every entry
/// with the same text on the other side becomes a candidate.
fn fill_potential_moved_blocks(
    syms: &[Sym],
    arena: &[MovedEntry],
    first_match: Option<usize>,
    l: usize,
    pmb: &mut Vec<MovedBlock>,
    ws: u32,
) {
    let mut m = first_match;
    while let Some(e) = m {
        let wsd = if (ws & COLOR_MOVED_WS_ALLOW_INDENTATION_CHANGE) != 0 {
            compute_ws_delta(syms[l].indent_width, syms[arena[e].sym].indent_width)
        } else {
            0
        };
        pmb.push(MovedBlock { entry: e, wsd });
        m = arena[e].next_match;
    }
}

/// `adjust_last_block()`: a block that carries fewer than
/// `COLOR_MOVED_MIN_ALNUM_COUNT` alphanumeric characters is not worth painting, so
/// its lines lose their move flags. The return value is git's "the block stands".
fn adjust_last_block(syms: &mut [Sym], mode: ColorMoved, n: usize, block_length: usize) -> bool {
    if mode == ColorMoved::Plain {
        return block_length != 0;
    }
    let mut alnum_count = 0u32;
    for i in 1..block_length + 1 {
        for c in &syms[n - i].line {
            if !c.is_ascii_alphanumeric() {
                continue;
            }
            alnum_count += 1;
            if alnum_count >= COLOR_MOVED_MIN_ALNUM_COUNT {
                return true;
            }
        }
    }
    for i in 1..block_length + 1 {
        syms[n - i].flags &= !MOVED_LINE_ZEBRA_MASK;
    }
    false
}

/// `mark_color_as_moved()`: walk the symbol list, keeping the set of blocks the
/// run of `+` (or `-`) lines could still be a copy of, and flag each line that a
/// surviving block covers.
fn mark_color_as_moved(syms: &mut [Sym], mode: ColorMoved, ws: u32) {
    let (arena, lists) = add_lines_to_move_detection(syms, ws);

    let mut pmb: Vec<MovedBlock> = Vec::new();
    let mut flipped_block = false;
    let mut block_length = 0usize;
    // `DIFF_SYMBOL_BINARY_DIFF_HEADER` is git's "no side" sentinel here.
    let mut moved_symbol: Option<Kind> = None;

    let mut n = 0usize;
    while n < syms.len() {
        let kind = syms[n].kind;
        let mut matched = match kind {
            Kind::Plus => lists[syms[n].id as usize].1,
            Kind::Minus => lists[syms[n].id as usize].0,
            _ => {
                flipped_block = false;
                None
            }
        };

        if !pmb.is_empty() && (matched.is_none() || moved_symbol != Some(kind)) {
            if !adjust_last_block(syms, mode, n, block_length) && block_length > 1 {
                // Rewind in case another match starts at the block's second line.
                matched = None;
                n -= block_length;
            }
            pmb.clear();
            block_length = 0;
            flipped_block = false;
        }
        let Some(first_match) = matched else {
            moved_symbol = None;
            n += 1;
            continue;
        };

        if mode == ColorMoved::Plain {
            syms[n].flags |= MOVED_LINE;
            n += 1;
            continue;
        }

        pmb_advance_or_null(syms, &arena, n, &mut pmb, ws);

        if pmb.is_empty() {
            let contiguous = adjust_last_block(syms, mode, n, block_length);
            if !contiguous && block_length > 1 {
                n -= block_length;
            } else {
                fill_potential_moved_blocks(syms, &arena, Some(first_match), n, &mut pmb, ws);
            }
            flipped_block =
                contiguous && !pmb.is_empty() && moved_symbol == Some(kind) && !flipped_block;
            moved_symbol = if pmb.is_empty() { None } else { Some(kind) };
            block_length = 0;
        }

        if !pmb.is_empty() {
            block_length += 1;
            syms[n].flags |= MOVED_LINE;
            if flipped_block && mode != ColorMoved::Blocks {
                syms[n].flags |= MOVED_LINE_ALT;
            }
        }
        n += 1;
    }
    adjust_last_block(syms, mode, syms.len(), block_length);
}

/// `dim_moved_lines()`: a moved line that is neither the first nor the last of its
/// shade run is interior, and `dimmed-zebra` paints it faintly.
fn dim_moved_lines(syms: &mut [Sym]) {
    let side = |k: Kind| k == Kind::Plus || k == Kind::Minus;
    for n in 0..syms.len() {
        if !side(syms[n].kind) || (syms[n].flags & MOVED_LINE) == 0 {
            continue;
        }
        let zebra = syms[n].flags & MOVED_LINE_ZEBRA_MASK;
        let alt = syms[n].flags & MOVED_LINE_ALT;
        // A neighbour that is not itself a `+`/`-` line is treated as absent.
        let prev = (n > 0 && side(syms[n - 1].kind)).then(|| n - 1);
        let next = (n + 1 < syms.len() && side(syms[n + 1].kind)).then(|| n + 1);

        // Inside a block?
        let inside = |i: Option<usize>| i.is_some_and(|i| syms[i].flags & MOVED_LINE_ZEBRA_MASK == zebra);
        if inside(prev) && inside(next) {
            syms[n].flags |= MOVED_LINE_UNINTERESTING;
            continue;
        }

        // A bound against a differently-shaded moved line stays interesting.
        let interesting_bound = |i: Option<usize>| {
            i.is_some_and(|i| (syms[i].flags & MOVED_LINE) != 0 && (syms[i].flags & MOVED_LINE_ALT) != alt)
        };
        if interesting_bound(prev) || interesting_bound(next) {
            continue;
        }
        syms[n].flags |= MOVED_LINE_UNINTERESTING;
    }
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

/// `emit_line_ws_markup()` (diff.c:1362-1396): one `+`/`-`/context line, choosing
/// between the plain single-span emission and the three-way whitespace-marked one.
///
/// `set_sign` is `None` for every ordinary diff. Only the dual-color diff-of-diffs
/// fills it in, and when it does the sign column is additionally reversed —
/// `emit_line_0(o, set_sign, set, !!set_sign, …)` (diff.c:1385, :1391) — which is
/// what makes the outer `+`/`-` stand out from the inner one it precedes.
///
/// An empty context line under `diff.suppressBlankEmpty` loses its sign in git;
/// the callers of this module apply that rewrite as a separate whole-buffer pass
/// over identical bytes, so the sign is always kept here.
#[allow(clippy::too_many_arguments)]
fn emit_content(
    out: &mut Vec<u8>,
    on: bool,
    set_sign: Option<&str>,
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
        // `if (!ws && !set_sign)` — the whole line in one color, applied before
        // the sign so the sign is painted too.
        None if set_sign.is_none() => emit_line_0(out, on, Some(set), None, false, reset, sign, line),
        // `else if (!ws)` — sign color, then content color, with the reverse video
        // `!!set_sign` asks for.
        None => emit_line_0(out, on, set_sign, Some(set), true, reset, sign, line),
        Some(ws) if blank_at_eof => {
            emit_line_0(out, on, Some(ws), None, false, reset, sign, line)
        }
        Some(ws) => {
            // `emit_line_0(o, set_sign ? set_sign : set, NULL, !!set_sign, …, "", 0)`.
            let head = set_sign.unwrap_or(set);
            emit_line_0(out, on, Some(head), None, set_sign.is_some(), reset, sign, b"");
            ws_check_emit(out, line, ws_rule, set, reset, ws);
        }
    }
}

/// The three content symbols `emit_diff_symbol_from_struct()` colors by hand:
/// `DIFF_SYMBOL_CONTEXT`, `DIFF_SYMBOL_PLUS` and `DIFF_SYMBOL_MINUS`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentKind {
    Context,
    Plus,
    Minus,
}

/// `emit_diff_symbol_from_struct()`'s `DIFF_SYMBOL_CONTEXT` / `_PLUS` / `_MINUS`
/// arms (diff.c:1441-1546), colors chosen and the line handed to
/// [`emit_content`].
///
/// `move_flags` carries the `DIFF_SYMBOL_MOVED_LINE*` bits that pick the moved-line
/// palette; `dual` is `o->flags.dual_color_diffed_diffs`.
///
/// With `dual` on, `set` — whatever the move switch just chose — becomes `set_sign`
/// and `set` is re-picked from the *inner* diff's marker, the first byte of the
/// content. That second lookup is the whole of "dual color": the outer diff's sign
/// keeps its own red/green while the inner diff's `+`/`-`/`@` re-tints the rest of
/// the line, bold under an outer `+` and dimmed under an outer `-`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_content_symbol(
    out: &mut Vec<u8>,
    colors: &DiffColors,
    dual: bool,
    kind: ContentKind,
    move_flags: u32,
    sign: u8,
    line: &[u8],
    ws_rule: u32,
    mut highlight: bool,
    blank_at_eof: bool,
) {
    let base = colors.get(content_slot(kind, move_flags));
    // `char c = !len ? 0 : line[0];` — the inner diff's own marker.
    let c = line.first().copied().unwrap_or(0);
    let mut set_sign: Option<&str> = None;
    let mut set = base;
    if dual {
        match kind {
            // diff.c:1445-1454: no `set_sign`, so the outer context sign simply
            // takes the inner marker's own color.
            ContentKind::Context => {
                set = match c {
                    b'+' => colors.get(DiffSlot::New),
                    b'@' => colors.get(DiffSlot::Frag),
                    b'-' => colors.get(DiffSlot::Old),
                    _ => set,
                };
            }
            // diff.c:1485-1497. The `flags &= ~DIFF_SYMBOL_CONTENT_WS_MASK` at
            // :1497 clears the `WSEH_*` side bit along with the rule bits, so an
            // outer-added line never takes the whitespace-marked path here.
            ContentKind::Plus => {
                set_sign = Some(base);
                set = match c {
                    b'-' => colors.get(DiffSlot::OldBold),
                    b'@' => colors.get(DiffSlot::Frag),
                    b'+' => colors.get(DiffSlot::NewBold),
                    _ => colors.get(DiffSlot::ContextBold),
                };
                highlight = false;
            }
            // diff.c:1530-1541 — the same switch, dimmed, and with no mask clear:
            // an outer-removed line still honours `--ws-error-highlight=old`.
            ContentKind::Minus => {
                set_sign = Some(base);
                set = match c {
                    b'+' => colors.get(DiffSlot::NewDim),
                    b'@' => colors.get(DiffSlot::Frag),
                    b'-' => colors.get(DiffSlot::OldDim),
                    _ => colors.get(DiffSlot::ContextDim),
                };
            }
        }
    }
    emit_content(
        out,
        colors.enabled(),
        set_sign,
        set,
        colors.reset(),
        colors.get(DiffSlot::Whitespace),
        sign,
        line,
        ws_rule,
        highlight,
        blank_at_eof,
    );
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

/// `emit_hunk_header()` under `o->flags.suppress_hunk_header_line_count`
/// (diff.c:1733-1798), the only shape `range-diff` ever prints: a bare `@@` where
/// the line counts would be (diff.c:1764-1765), then the section name the
/// `section_headers` userdiff driver found.
///
/// `dual` is `o->flags.dual_color_diffed_diffs`, which prefixes `GIT_COLOR_REVERSE`
/// (diff.c:1761-1762). It is a separate entry point from [`emit_hunk_header`]
/// because the two flags always travel together: `output()` sets
/// `dual_color_diffed_diffs` and `suppress_hunk_header_line_count` in the same
/// breath (range-diff.c:524-526), and nothing else sets either.
///
/// `func_line` is the section name with no separator; xdiff writes exactly one
/// space between the closing `@@` and the name, which is the `cp`..`ep` run
/// diff.c:1778-1785 paints in the context color.
pub(crate) fn emit_hunk_header_suppressed(
    out: &mut Vec<u8>,
    colors: &DiffColors,
    dual: bool,
    func_line: &[u8],
) {
    let reset = colors.reset();
    if dual && colors.enabled() {
        out.extend_from_slice(REVERSE.as_bytes());
    }
    out.extend_from_slice(colors.get(DiffSlot::Frag).as_bytes());
    out.extend_from_slice(b"@@");
    out.extend_from_slice(reset.as_bytes());
    if !func_line.is_empty() {
        out.extend_from_slice(colors.get(DiffSlot::Context).as_bytes());
        out.push(b' ');
        out.extend_from_slice(reset.as_bytes());
        out.extend_from_slice(colors.get(DiffSlot::Func).as_bytes());
        out.extend_from_slice(func_line);
        out.extend_from_slice(reset.as_bytes());
    }
    out.push(b'\n');
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

/// `ws_check()` (ws.c): which rules an added line breaks, as `WS_*` bits.
///
/// The trailing newline (and, under `cr-at-eol`, a carriage return before it)
/// is set aside first so a line's "end" is its last real character; the rest
/// mirrors `ws_check_emit_1`'s two passes — trailing blanks from the right, the
/// indent from the left. `WS_BLANK_AT_EOF` is not decided here: it needs the
/// whole hunk, which is what [`check_blank_at_eof`] answers for the caller.
pub(crate) fn ws_check(line: &[u8], ws_rule: u32) -> u32 {
    let mut result = 0u32;
    let mut len = line.len();
    if len > 0 && line[len - 1] == b'\n' {
        len -= 1;
    }
    if (ws_rule & WS_CR_AT_EOL) != 0 && len > 0 && line[len - 1] == b'\r' {
        len -= 1;
    }

    // Trailing whitespace, scanned right to left.
    let mut trailing_whitespace: Option<usize> = None;
    if (ws_rule & WS_BLANK_AT_EOL) != 0 {
        for i in (0..len).rev() {
            if is_c_space(line[i]) {
                trailing_whitespace = Some(i);
                result |= WS_BLANK_AT_EOL;
            } else {
                break;
            }
        }
    }
    let trailing_whitespace = trailing_whitespace.unwrap_or(len);

    // The indent, scanned left to right.
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
            result |= WS_SPACE_BEFORE_TAB;
        } else if (ws_rule & WS_TAB_IN_INDENT) != 0 {
            result |= WS_TAB_IN_INDENT;
        }
        written = i + 1;
        i += 1;
    }
    if (ws_rule & WS_INDENT_WITH_NON_TAB) != 0 && i - written >= ws_tab_width(ws_rule) {
        result |= WS_INDENT_WITH_NON_TAB;
    }
    result
}

/// `whitespace_error_string()`: the comma-joined description `--check` and
/// `apply` print for one line's set of broken rules.
pub(crate) fn whitespace_error_string(ws: u32) -> String {
    let mut err = String::new();
    let mut add = |s: &str| {
        if !err.is_empty() {
            err.push_str(", ");
        }
        err.push_str(s);
    };
    if ws & WS_TRAILING_SPACE == WS_TRAILING_SPACE {
        add("trailing whitespace");
    } else {
        if ws & WS_BLANK_AT_EOL != 0 {
            add("trailing whitespace");
        }
        if ws & WS_BLANK_AT_EOF != 0 {
            add("new blank line at EOF");
        }
    }
    if ws & WS_SPACE_BEFORE_TAB != 0 {
        add("space before tab in indent");
    }
    if ws & WS_INDENT_WITH_NON_TAB != 0 {
        add("indent with spaces");
    }
    if ws & WS_TAB_IN_INDENT != 0 {
        add("tab in indent");
    }
    err
}
