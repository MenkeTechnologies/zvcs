//! `xdl_recmatch()`'s whitespace rules (xdiff/xutils.c:173-250), expressed as
//! canonical line images.
//!
//! `-Xignore-space-change` and friends reach merge-ort as
//! `xpp.flags & XDF_WHITESPACE_FLAGS`, and every place that compares two records
//! — `xdl_prepare_env()`'s interning and `xdl_merge()`'s
//! `xdl_recs_cmp()`/`line_matches()` — routes the comparison through
//! `xdl_recmatch()`. The vendored `gix-merge` text driver interns whole lines
//! into equivalence classes instead of walking two records in step, so it takes
//! the rule as [`gix::merge::blob::builtin_driver::text::Canonicalize`]: a
//! function mapping a line (terminator included, as `xrecord_t` keeps it) to the
//! image its class is keyed by. Two lines belong to the same class exactly when
//! `xdl_recmatch()` would have returned 1 for them.
//!
//! Each function below is that image for one flag, derived from the flag's
//! branch in `xdl_recmatch()`:
//!
//! | flag | branch | image |
//! |---|---|---|
//! | `XDF_IGNORE_WHITESPACE` (`-Xignore-all-space`) | xutils.c:193-203 | every whitespace byte dropped |
//! | `XDF_IGNORE_WHITESPACE_CHANGE` (`-Xignore-space-change`) | xutils.c:204-216 | each internal whitespace run collapsed to one space, the trailing run dropped |
//! | `XDF_IGNORE_WHITESPACE_AT_EOL` (`-Xignore-space-at-eol`) | xutils.c:217-221 | the trailing whitespace run dropped |
//! | `XDF_IGNORE_CR_AT_EOL` (`-Xignore-cr-at-eol`) | xutils.c:222-229 | a `\r` directly before the terminating `\n` dropped |
//!
//! The first three end in `xdl_recmatch()`'s shared tail (xutils.c:238-248),
//! which lets one side run out early as long as the other has nothing but
//! whitespace left — that is what makes the trailing run droppable rather than
//! collapsible, and it is why `"a"` and `"a\n"` are one class under `-b` but two
//! under no flag at all.
//!
//! `XDF_IGNORE_CR_AT_EOL` is the exception: it does *not* fall through to that
//! tail (it `return`s at xutils.c:228), and `ends_with_optional_cr()` requires
//! the line to be `\n`-terminated before it will ignore the `\r`
//! (xutils.c:167-169, "do not ignore CR at the end of an incomplete line"). So a
//! file whose last line is `abc\r` with no newline still differs from `abc`.
//!
//! `git merge` can set several of these at once — `parse_merge_opt()` ORs each
//! into `opt->xdl_opts` (merge-ort.c:5567-5574) — but `xdl_recmatch()` tests them
//! in a fixed `if`/`else if` chain, so only the strongest one in force decides.
//! [`canonicalize_for`] reproduces that precedence.

/// `XDL_ISSPACE()` (xdiff/xmacros.h:33), which is C `isspace()` in the "C"
/// locale: space, `\t`, `\n`, `\v`, `\f`, `\r`.
///
/// Rust's `u8::is_ascii_whitespace` is not the same set — it omits `\v` — so the
/// membership test is spelled out.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `XDF_IGNORE_WHITESPACE` (`-Xignore-all-space`): two records match when their
/// non-whitespace bytes agree in order, so the class image is the line with
/// every whitespace byte removed.
fn ignore_all_space(line: &[u8]) -> Vec<u8> {
    line.iter().copied().filter(|&b| !is_space(b)).collect()
}

/// `XDF_IGNORE_WHITESPACE_CHANGE` (`-Xignore-space-change`): a run of whitespace
/// matches any other run, but a run does not match its absence
/// (xutils.c:206-215 only skips when *both* sides are on whitespace), so each run
/// collapses to a single space. The final run is dropped outright rather than
/// collapsed, because the tail at xutils.c:238-248 accepts a side that ran out
/// against leftover whitespace — which is what makes `"a"` match `"a\n"`.
fn ignore_space_change(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut in_run = false;
    for &b in line {
        if is_space(b) {
            in_run = true;
        } else {
            if in_run {
                out.push(b' ');
                in_run = false;
            }
            out.push(b);
        }
    }
    out
}

/// `XDF_IGNORE_WHITESPACE_AT_EOL` (`-Xignore-space-at-eol`): the two records are
/// walked byte for byte until they differ (xutils.c:218-221), then whatever is
/// left on either side must be whitespace — so the image is the line with its
/// trailing whitespace run removed.
fn ignore_space_at_eol(line: &[u8]) -> Vec<u8> {
    let end = line.iter().rposition(|&b| !is_space(b)).map_or(0, |i| i + 1);
    line[..end].to_vec()
}

/// `XDF_IGNORE_CR_AT_EOL` (`-Xignore-cr-at-eol`): `ends_with_optional_cr()`
/// ignores a `\r` only when it sits directly before the record's terminating
/// `\n` (xutils.c:159-171), so the image drops exactly that byte and nothing
/// else. A line with no `\n` keeps its `\r`.
fn ignore_cr_at_eol(line: &[u8]) -> Vec<u8> {
    match line.strip_suffix(b"\r\n") {
        Some(head) => {
            let mut out = Vec::with_capacity(head.len() + 1);
            out.extend_from_slice(head);
            out.push(b'\n');
            out
        }
        None => line.to_vec(),
    }
}

/// `XDF_IGNORE_WHITESPACE` — `-Xignore-all-space` (xdiff/xdiff.h:33).
pub const XDF_IGNORE_WHITESPACE: u32 = 1 << 1;
/// `XDF_IGNORE_WHITESPACE_CHANGE` — `-Xignore-space-change` (xdiff/xdiff.h:34).
pub const XDF_IGNORE_WHITESPACE_CHANGE: u32 = 1 << 2;
/// `XDF_IGNORE_WHITESPACE_AT_EOL` — `-Xignore-space-at-eol` (xdiff/xdiff.h:35).
pub const XDF_IGNORE_WHITESPACE_AT_EOL: u32 = 1 << 3;
/// `XDF_IGNORE_CR_AT_EOL` — `-Xignore-cr-at-eol` (xdiff/xdiff.h:36).
pub const XDF_IGNORE_CR_AT_EOL: u32 = 1 << 4;

/// The canonical form `xdl_recmatch()` would compare under `xdl_opts`, or `None`
/// when no whitespace bit is set (git's plain `memcmp`, xutils.c:177-180).
///
/// The precedence is `xdl_recmatch()`'s own `if`/`else if` order
/// (xutils.c:193-222) and the comment above it: "-w matches everything that
/// matches with -b, and -b in turn matches everything that matches with
/// --ignore-space-at-eol, which in turn matches everything that matches with
/// --ignore-cr-at-eol".
pub fn canonicalize_for(
    xdl_opts: u32,
) -> Option<gix::merge::blob::builtin_driver::text::Canonicalize> {
    if xdl_opts & XDF_IGNORE_WHITESPACE != 0 {
        Some(ignore_all_space)
    } else if xdl_opts & XDF_IGNORE_WHITESPACE_CHANGE != 0 {
        Some(ignore_space_change)
    } else if xdl_opts & XDF_IGNORE_WHITESPACE_AT_EOL != 0 {
        Some(ignore_space_at_eol)
    } else if xdl_opts & XDF_IGNORE_CR_AT_EOL != 0 {
        Some(ignore_cr_at_eol)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pair here was read off `xdl_recmatch()`'s branches; the point is
    /// that the canonical images agree with what the C returns, including the
    /// cases where a naive "trim both sides" would not.
    fn same(f: fn(&[u8]) -> Vec<u8>, a: &str, b: &str) -> bool {
        f(a.as_bytes()) == f(b.as_bytes())
    }

    #[test]
    fn all_space_ignores_every_whitespace_byte() {
        assert!(same(ignore_all_space, "a b\n", "ab\n"));
        assert!(same(ignore_all_space, "\ta\tb", "a b "));
        assert!(!same(ignore_all_space, "ab\n", "ba\n"));
    }

    #[test]
    fn space_change_keeps_run_presence_but_not_run_length() {
        assert!(same(ignore_space_change, "a  b\n", "a\tb\n"));
        assert!(!same(ignore_space_change, "a b\n", "ab\n"));
        // A leading run is a run: its presence still distinguishes the lines.
        assert!(!same(ignore_space_change, "  a\n", "a\n"));
        assert!(same(ignore_space_change, "  a\n", "\ta\n"));
        // The tail at xutils.c:238-248 lets one side run out early.
        assert!(same(ignore_space_change, "a", "a\n"));
        assert!(same(ignore_space_change, "a   ", "a"));
    }

    #[test]
    fn space_at_eol_only_trims_the_end() {
        assert!(same(ignore_space_at_eol, "a b  \n", "a b\n"));
        assert!(same(ignore_space_at_eol, "a b\n", "a b"));
        assert!(!same(ignore_space_at_eol, "a  b\n", "a b\n"));
        assert!(!same(ignore_space_at_eol, " a\n", "a\n"));
    }

    #[test]
    fn cr_at_eol_needs_a_terminated_line() {
        assert!(same(ignore_cr_at_eol, "abc\r\n", "abc\n"));
        // "do not ignore CR at the end of an incomplete line" (xutils.c:167).
        assert!(!same(ignore_cr_at_eol, "abc\r", "abc"));
        // Only the `\r` adjacent to the terminator is dropped.
        assert!(!same(ignore_cr_at_eol, "abc\r\r\n", "abc\n"));
        assert!(!same(ignore_cr_at_eol, "a\rb\n", "ab\n"));
    }

    #[test]
    fn vertical_tab_is_whitespace_to_the_c_but_not_to_rust() {
        assert!(!0x0b_u8.is_ascii_whitespace());
        assert!(is_space(0x0b));
        assert!(same(ignore_all_space, "a\x0bb\n", "ab\n"));
    }

    #[test]
    fn precedence_follows_xdl_recmatch() {
        // `parse_merge_opt()` ORs the bits, `xdl_recmatch()` tests them in order.
        let both = XDF_IGNORE_WHITESPACE | XDF_IGNORE_CR_AT_EOL;
        let picked = canonicalize_for(both).expect("a rule is in force");
        assert_eq!(picked(b"a b\n"), ignore_all_space(b"a b\n"));

        let weaker = XDF_IGNORE_WHITESPACE_AT_EOL | XDF_IGNORE_CR_AT_EOL;
        let picked = canonicalize_for(weaker).expect("a rule is in force");
        assert_eq!(picked(b"a b \n"), ignore_space_at_eol(b"a b \n"));

        assert!(canonicalize_for(0).is_none());
    }
}
