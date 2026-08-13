//! Parse-time value checks for the options `add_diff_options()` contributes.
//!
//! Every command that renders a diff — `diff`, `diff-tree`, `diff-files`,
//! `diff-index`, `log`, `show`, `format-patch`, `range-diff` — shares one option
//! table, so they all reject a malformed `--submodule=`, `--word-diff=` or
//! `--unified=` value identically, with the same `error:` line and exit 129.
//! They also all reject it *before* a single revision is resolved, which is what
//! makes these checks observable even in commands whose diff rendering is not
//! ported: get the ordering wrong and a bad-value invocation reports a revision
//! error instead of the option error git reports.
//!
//! Reproducing that per command produced divergent accept sets, so the sets and
//! the messages live here once. The callbacks are `diff_opt_submodule()`,
//! `diff_opt_word_diff()`, `diff_opt_stat()`, `diff_opt_unified()`,
//! `diff_opt_diff_algorithm()` and the `OPTION_UNSIGNED` case for
//! `--inter-hunk-context`, all in git 2.55.0's `diff.c`/`parse-options.c`.

/// `parse_submodule_params()`: the three `--submodule=<format>` names, matched
/// case-sensitively.
pub const SUBMODULE_FORMATS: [&str; 3] = ["log", "short", "diff"];

/// `diff_opt_word_diff()`: the four `--word-diff=<mode>` names, case-sensitive.
pub const WORD_DIFF_MODES: [&str; 4] = ["plain", "color", "porcelain", "none"];

/// `parse_algorithm_value()`: the `--diff-algorithm=<name>` set. Matched
/// case-*insensitively*, unlike the two above, and `default` is an alias for
/// `myers`.
pub const DIFF_ALGORITHMS: [&str; 5] = ["myers", "minimal", "patience", "histogram", "default"];

/// C `strtoul(v, &end, 10)` followed by git's `if (*end) return error(...)`:
/// true when the whole string is consumed.
///
/// `strtoul` accepts leading whitespace and a sign and wraps on overflow, and it
/// consumes nothing from an empty string — which still leaves `*end == '\0'`.
/// That is why `--stat-width=`, `--stat-width=-1` and `--stat-width=<2^90>` are
/// all accepted while `--stat-width=abc` and `--stat-width=false` are not.
pub fn strtoul_consumes_all(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    if matches!(b.get(i), Some(b'+' | b'-')) {
        i += 1;
        // A sign with no digit after it leaves `end` back at the start, so the
        // sign itself is unconsumed and the value is rejected.
        if !matches!(b.get(i), Some(c) if c.is_ascii_digit()) {
            return false;
        }
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i == b.len()
}

/// C `strtol(v, &end, 10)` into a `long`, or `None` when bytes trail the number.
///
/// Overflow saturates rather than failing, and an empty string yields `0`, both
/// as `strtol` does. `--unified` reads its value this way and *then* rejects a
/// negative result, which is why `--unified=-1` and `--unified=<huge>` get
/// different answers: the latter saturates to `LONG_MAX`, which is not negative.
pub fn strtol_long(v: &str) -> Option<i64> {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = matches!(b.get(i), Some(b'-'));
    let signed = matches!(b.get(i), Some(b'+' | b'-'));
    if signed {
        i += 1;
    }
    let digits_start = i;
    let mut magnitude: u64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(u64::from(b[i] - b'0'));
        i += 1;
    }
    // A lone sign is not a number: `end` stays at the start, so the sign trails.
    if signed && i == digits_start {
        return None;
    }
    if i != b.len() {
        return None;
    }
    const LONG_MIN_MAGNITUDE: u64 = i64::MAX as u64 + 1;
    Some(match negative {
        true if magnitude >= LONG_MIN_MAGNITUDE => i64::MIN,
        true => -(magnitude as i64),
        false if magnitude > i64::MAX as u64 => i64::MAX,
        false => magnitude as i64,
    })
}

/// Check the value of one shared diff option, by its long name without dashes.
///
/// `value` is `None` for the bare spelling, which every option here either
/// accepts (`--submodule`, `--word-diff`, `--unified`) or never sees.
/// `Err` carries the message git prints after `error: `, and always means exit
/// 129. Names this does not know are `Ok`: it validates the options git checks
/// at parse time, not the whole table.
pub fn check(name: &str, value: Option<&str>) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    match name {
        "submodule" => match SUBMODULE_FORMATS.contains(&value) {
            true => Ok(()),
            false => Err(format!(
                "failed to parse --submodule option parameter: '{value}'"
            )),
        },
        "word-diff" => match WORD_DIFF_MODES.contains(&value) {
            true => Ok(()),
            false => Err(format!("bad --word-diff argument: {value}")),
        },
        "diff-algorithm" => {
            let lowered = value.to_ascii_lowercase();
            match DIFF_ALGORITHMS.contains(&lowered.as_str()) {
                true => Ok(()),
                false => Err(
                    "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" \
                     and \"histogram\""
                        .to_string(),
                ),
            }
        }
        "stat-width" | "stat-name-width" | "stat-graph-width" | "stat-count" => {
            match strtoul_consumes_all(value) {
                true => Ok(()),
                false => Err(format!("{name} expects a numerical value")),
            }
        }
        // `diff_opt_char()` tests `arg[1]`, so it is a *byte* length check: the
        // empty value is accepted (it stores NUL) and anything past one byte,
        // including a single multi-byte character, is not.
        "output-indicator-new" | "output-indicator-old" | "output-indicator-context" => {
            match value.len() <= 1 {
                true => Ok(()),
                false => Err(format!("{name} expects a character, got '{value}'")),
            }
        }
        "ws-error-highlight" => match ws_error_highlight_bad_at(value) {
            None => Ok(()),
            Some(consumed) => Err(format!(
                "unknown value after ws-error-highlight={}",
                &value[..consumed]
            )),
        },
        // Reported as `--unified` whether it was spelled `-U<n>` or
        // `--unified=<n>`, because the callback passes the literal name.
        "unified" => match strtol_long(value) {
            None => Err("--unified expects a numerical value".to_string()),
            Some(n) if n < 0 => Err("--unified expects a non-negative integer".to_string()),
            Some(_) => Ok(()),
        },
        _ => Ok(()),
    }
}

/// `parse_rename_score()` (`diff.c`): consume a similarity score off the front of
/// `v` and return how many bytes it took.
///
/// The grammar is digits with at most one `.`, optionally closed by a `%` — and
/// the loop simply *stops* at anything else rather than failing, which is what
/// makes the callers' `if (*arg != 0)` the actual validation. So `-M50`, `-M50%`,
/// `-M.5` and a bare `-M` are all fine while `-Mabc` leaves `abc` unconsumed.
/// Only the length matters here: the score itself cannot change whether a command
/// is accepted.
pub fn rename_score_len(v: &str) -> usize {
    let b = v.as_bytes();
    let mut i = 0;
    let mut dot = false;
    while i < b.len() {
        match b[i] {
            b'.' if !dot => dot = true,
            // `%` is always at the end.
            b'%' => return i + 1,
            c if c.is_ascii_digit() => {}
            _ => break,
        }
        i += 1;
    }
    i
}

/// `diff_opt_find_renames()` / `diff_opt_find_copies()`: the whole value must be
/// a score. `Err` is the `error:` line, exit 129.
///
/// `name` is `find-renames` or `find-copies` — `opt->long_name`, so `-M` and `-C`
/// are reported by their long spellings.
pub fn check_rename_score(name: &str, value: &str) -> Result<(), String> {
    match rename_score_len(value) == value.len() {
        true => Ok(()),
        false => Err(format!("invalid argument to {name}")),
    }
}

/// `diff_opt_break_rewrites()`: one score, then optionally `/` and a second one,
/// with nothing left over. `Err` is the `error:` line, exit 129.
pub fn check_break_rewrites(value: &str) -> Result<(), String> {
    const ERR: &str = "break-rewrites expects <n>/<m> form";
    let rest = &value[rename_score_len(value)..];
    let Some(second) = rest.strip_prefix('/') else {
        return match rest.is_empty() {
            true => Ok(()),
            false => Err(ERR.to_string()),
        };
    };
    match rename_score_len(second) == second.len() {
        true => Ok(()),
        false => Err(ERR.to_string()),
    }
}

/// `parse_ws_error_highlight()`: how far it got before meeting a token it does
/// not know, or `None` when the whole value parses.
///
/// The tokens are `none`, `default`, `all`, `new`, `old` and `context`, and
/// `parse_one_token()` requires each to be followed by end-of-string or a comma
/// — so `all,new` parses and `allnew` does not. The returned offset is what git
/// echoes back in the message: it points *past* the separator of the last token
/// that did parse, which is why `--ws-error-highlight=all,v1` reports `all,` and
/// `--ws-error-highlight=allnew` reports nothing at all.
fn ws_error_highlight_bad_at(value: &str) -> Option<usize> {
    const TOKENS: [&str; 6] = ["none", "default", "all", "new", "old", "context"];
    let b = value.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let rest = &value[i..];
        let matched = TOKENS.iter().find(|t| {
            rest.strip_prefix(*t)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with(','))
        });
        match matched {
            Some(t) => i += t.len(),
            None => return Some(i),
        }
        // git steps over whatever single byte separates two tokens.
        if i < b.len() {
            i += 1;
        }
    }
    None
}

/// `func_by_opt()` (`diff-merges.c`): every value `--diff-merges=<style>` takes,
/// each spelled long and short. Matched case-sensitively.
///
/// Unlike the options [`check`] covers, a bad value here is not a parse-options
/// error: `--diff-merges` is passed through to the revision machinery, which
/// `die()`s with `invalid value for '--diff-merges': '<v>'`. Callers own the
/// message because they own the exit status it lands on — 128 in `log`, 255 in
/// `range-diff`, which reports it through the inner log it runs.
pub const DIFF_MERGES_VALUES: [&str; 13] = [
    "off",
    "none",
    "1",
    "first-parent",
    "separate",
    "c",
    "combined",
    "cc",
    "dense-combined",
    "r",
    "remerge",
    "m",
    "on",
];

/// Whether `--diff-merges=<v>` names a style, i.e. whether `func_by_opt()`
/// returns a setup function rather than `NULL`.
pub fn diff_merges_is_valid(v: &str) -> bool {
    DIFF_MERGES_VALUES.contains(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three value shapes `strtoul` accepts that look like they should not:
    /// nothing at all, a negative number, and a number far past the type.
    #[test]
    fn strtoul_accepts_empty_negative_and_overflow() {
        assert!(strtoul_consumes_all(""));
        assert!(strtoul_consumes_all("-1"));
        assert!(strtoul_consumes_all("99999999999999999999999999"));
        assert!(strtoul_consumes_all(" 10"));

        assert!(!strtoul_consumes_all("abc"));
        assert!(!strtoul_consumes_all("false"));
        assert!(!strtoul_consumes_all("10 "));
        assert!(!strtoul_consumes_all("-"));
    }

    /// `--unified` separates "not a number" from "negative", and overflow lands
    /// on the non-negative side because `strtol` saturates at `LONG_MAX`.
    #[test]
    fn unified_separates_malformed_from_negative() {
        assert_eq!(check("unified", Some("3")), Ok(()));
        assert_eq!(check("unified", Some("")), Ok(()));
        assert_eq!(check("unified", Some(" 4")), Ok(()));
        assert_eq!(check("unified", Some("99999999999999999999999999")), Ok(()));

        assert_eq!(
            check("unified", Some("-1")),
            Err("--unified expects a non-negative integer".to_string())
        );
        assert_eq!(
            check("unified", Some("-99999999999999999999999999")),
            Err("--unified expects a non-negative integer".to_string())
        );
        for bad in ["v1", "abc", "4 "] {
            assert_eq!(
                check("unified", Some(bad)),
                Err("--unified expects a numerical value".to_string()),
                "{bad:?}"
            );
        }
    }

    /// `--submodule` and `--word-diff` are case-sensitive; `--diff-algorithm`
    /// is not. Verified against git 2.55.0, which accepts `--diff-algorithm=MYERS`
    /// and rejects `--submodule=LOG` and `--word-diff=PLAIN`.
    #[test]
    fn name_sets_match_git_case_sensitivity() {
        assert_eq!(check("submodule", Some("log")), Ok(()));
        assert!(check("submodule", Some("LOG")).is_err());
        assert!(check("submodule", Some("")).is_err());

        assert_eq!(check("word-diff", Some("porcelain")), Ok(()));
        assert!(check("word-diff", Some("PLAIN")).is_err());
        assert_eq!(
            check("word-diff", Some("")),
            Err("bad --word-diff argument: ".to_string())
        );

        assert_eq!(check("diff-algorithm", Some("MYERS")), Ok(()));
        assert_eq!(check("diff-algorithm", Some("default")), Ok(()));
        assert!(check("diff-algorithm", Some("")).is_err());

        // The bare spelling is always fine.
        assert_eq!(check("submodule", None), Ok(()));
        assert_eq!(check("word-diff", None), Ok(()));
    }

    /// `parse_one_token()` anchors each name to a comma or the end of the
    /// value, and the offset git echoes back points past the last separator it
    /// stepped over. Verified against git 2.55.0 with `git diff-tree
    /// --ws-error-highlight=<v> HEAD`.
    #[test]
    fn ws_error_highlight_reports_gits_own_offset() {
        for good in ["", "none", "default", "all", "new", "old", "context",
                     "all,new", "new,old,context", "all,", "none,"] {
            assert_eq!(check("ws-error-highlight", Some(good)), Ok(()), "{good:?}");
        }
        assert_eq!(
            check("ws-error-highlight", Some("all,v1")),
            Err("unknown value after ws-error-highlight=all,".to_string())
        );
        // `allnew` fails at offset 0: `all` is not a token unless a comma or
        // the end of the value follows it.
        for bad in ["v1", "allnew", "new-old", "NONE", "nonex", "alll"] {
            assert_eq!(
                check("ws-error-highlight", Some(bad)),
                Err("unknown value after ws-error-highlight=".to_string()),
                "{bad:?}"
            );
        }
    }

    /// `diff_opt_char()` is a byte-length test, so the empty value is legal and
    /// a two-byte value is not.
    #[test]
    fn output_indicator_takes_one_byte() {
        for name in ["output-indicator-new", "output-indicator-old", "output-indicator-context"] {
            assert_eq!(check(name, Some("X")), Ok(()));
            assert_eq!(check(name, Some("")), Ok(()));
            assert_eq!(
                check(name, Some("false")),
                Err(format!("{name} expects a character, got 'false'"))
            );
        }
    }

    /// Every style `func_by_opt()` maps, and nothing else.
    #[test]
    fn diff_merges_styles_are_the_thirteen_git_knows() {
        for good in DIFF_MERGES_VALUES {
            assert!(diff_merges_is_valid(good), "{good}");
        }
        for bad in ["", "99", "abc", "ON", "First-Parent"] {
            assert!(!diff_merges_is_valid(bad), "{bad}");
        }
    }
}
