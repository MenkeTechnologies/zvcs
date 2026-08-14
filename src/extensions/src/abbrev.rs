//! Object-name abbreviation length, resolved the way git resolves it.
//!
//! git abbreviates object ids in `diff` `index` lines, `blame`/`annotate`
//! output, `log --oneline`, and elsewhere to the length named by `core.abbrev`
//! (default `auto`). This is the single shared resolver so every command agrees
//! on the length — a hardcoded `7` diverges from any user whose config sets
//! `core.abbrev` (e.g. `core.abbrev = 10`).

use gix::bstr::ByteSlice;

/// git's effective `core.abbrev`: an explicit number, `auto`/absent → derived
/// from the object count, or `no`/`off`/`false` → the full hash length.
pub fn configured_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    let value = repo
        .config_snapshot()
        .string("core.abbrev")
        .as_ref()
        .and_then(|v| v.to_str().ok().map(str::to_ascii_lowercase));
    resolve(value, hexsz, || auto_abbrev(repo, hexsz))
}

/// git's `FALLBACK_DEFAULT_ABBREV` (object-name.h:140) — the width `auto` means
/// when there is no object database to size it against.
pub const FALLBACK_DEFAULT_ABBREV: usize = 7;

/// The same length for a command running *outside* a repository, which is
/// `git diff --no-index`: its two operands need not live in one.
///
/// `diff_abbrev_oid()` (diff.c:4842-4856) branches on
/// `startup_info->have_repository`. With a repository it asks the object database
/// for a length that is unique there; without one it just cuts the hex at
/// `options->abbrev`, which is still `default_abbrev` — git reads the system and
/// per-user config whether or not it found a repository, so a user's
/// `core.abbrev = 10` applies to a `--no-index` comparison of two paths in `/tmp`.
/// Only `auto` differs: with no object count to work from it lands on
/// [`FALLBACK_DEFAULT_ABBREV`].
pub fn global_abbrev(hexsz: usize) -> usize {
    let config = crate::config::global_config();
    let value = config
        .string("core.abbrev")
        .as_ref()
        .and_then(|v| v.to_str().ok().map(str::to_ascii_lowercase));
    resolve(value, hexsz, || FALLBACK_DEFAULT_ABBREV)
}

/// `git_default_core_config()`'s `core.abbrev` arm (environment.c:349-363), over
/// a value already lowercased: `auto` and an unreadable number defer to `auto`,
/// a false-y word means the whole name, anything else is the number itself.
fn resolve(value: Option<String>, hexsz: usize, auto: impl Fn() -> usize) -> usize {
    match value {
        None => auto(),
        Some(v) => match v.as_str() {
            "auto" => auto(),
            "no" | "off" | "false" => hexsz,
            other => other.parse::<usize>().unwrap_or_else(|_| auto()),
        },
    }
}

/// git's `MINIMUM_ABBREV`: the shortest id `--abbrev=<n>` can ask for.
pub const MINIMUM_ABBREV: usize = 4;

/// `--abbrev=<n>`'s value for every command that reaches it through
/// `setup_revisions()` — `diff`, `diff-files`, `diff-tree`, `diff-index`, `log`,
/// `show`.
///
/// `revision.c` claims the option before `diff_opt_parse()` can and reads it with
/// `revs->abbrev = strtoul(optarg, NULL, 10)`, into the **`unsigned int`** that
/// `revision.h` declares, then clamps to `[MINIMUM_ABBREV, hexsz]`. Nothing here
/// is an error — that is the difference from
/// [`parse_opt_abbrev_value`], which the commands reaching `OPT__ABBREV`
/// directly (`cherry`, `blame`, `describe`, …) use and which *does* reject a
/// malformed value.
///
/// The 32-bit unsigned truncation is observable and asymmetric with the signed
/// one: against git 2.55.0 with a 40-hex hash, `--abbrev=-1` and
/// `--abbrev=99999999999999999999999999` both print the full name (they saturate
/// to `ULONG_MAX`, whose low 32 bits exceed `hexsz`), while `--abbrev=4294967296`
/// prints four characters (it truncates to 0).
pub fn parse_abbrev_arg(v: &str, hexsz: usize) -> usize {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = matches!(b.get(i), Some(b'-'));
    if matches!(b.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    // `strtoul` saturates at `ULONG_MAX` and negates by wrapping, both in the
    // unsigned `long` this host has as 64 bits.
    let mut magnitude: u64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(u64::from(b[i] - b'0'));
        i += 1;
    }
    let wide = match negative {
        true => magnitude.wrapping_neg(),
        false => magnitude,
    };
    (wide as u32 as usize).clamp(MINIMUM_ABBREV, hexsz)
}

/// Auto abbreviation length: `ceil(log2(objects) / 2)`, floored at 7 — the same
/// heuristic `gix` uses for `core.abbrev = auto`.
pub fn auto_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let mut len = (64 - count.leading_zeros()) as usize;
    len = len.div_ceil(2);
    len.max(7).min(hexsz)
}

/// The number `parse_opt_abbrev_cb()` (`parse-options-cb.c`) reads out of an
/// attached `--abbrev=<value>`, C quirks included.
///
/// Upstream is `v = strtol(arg, &arg, 10); if (*arg) return error(...)` storing
/// into an `int`, so:
///
///   * leading C whitespace and one optional `+`/`-` are skipped, then base-10
///     digits are consumed;
///   * `None` — upstream's ``option `%s' expects a numerical value`` — is
///     returned only when no digit is consumed or bytes trail the number, so
///     ``, `abc`, `0x10`, `12abc` and `8 ` are errors while ` 12` and `+12` are
///     not;
///   * overflow is **not** an error: `strtol` saturates to `LONG_MAX`/`LONG_MIN`;
///   * the `long` is then narrowed to the low 32 bits by the assignment.
///
/// That last step is what makes git's behaviour surprising, and is why this is
/// shared rather than reimplemented per command: with a 40-hex hash,
/// `--abbrev=99999999999999999999999999` truncates to `-1` and therefore means 4,
/// `--abbrev=-99999999999999999999999999` truncates to `0` and therefore prints
/// the full name, `--abbrev=4294967296` also means 0, and `--abbrev=4294967300`
/// means 4. All four are verified against git 2.55.0.
///
/// Callers apply the `MINIMUM_ABBREV`/`hexsz` clamp themselves, because upstream
/// applies it in two different places depending on the command.
pub fn parse_opt_abbrev_value(value: &str) -> Option<i32> {
    let b = value.as_bytes();
    let mut i = 0;
    // strtol's `isspace`: space, \t, \n, \v, \f, \r.
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = matches!(b.get(i), Some(b'-'));
    if matches!(b.get(i), Some(b'+' | b'-')) {
        i += 1;
    }

    let digits_start = i;
    // Accumulated as an unsigned magnitude so the negative side can reach
    // `LONG_MIN` exactly; folding the sign in as it goes would saturate at
    // `-LONG_MAX` and get `--abbrev=-99…9` wrong by one.
    let mut magnitude: u64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(u64::from(b[i] - b'0'));
        i += 1;
    }
    if i == digits_start || i != b.len() {
        return None;
    }

    const LONG_MIN_MAGNITUDE: u64 = i64::MAX as u64 + 1;
    let wide: i64 = match negative {
        true if magnitude >= LONG_MIN_MAGNITUDE => i64::MIN,
        true => -(magnitude as i64),
        false if magnitude > i64::MAX as u64 => i64::MAX,
        false => magnitude as i64,
    };
    Some(wide as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `core.abbrev`'s four shapes (environment.c:349-363). The `auto` fallback is
    /// a closure because it means two different things: the object-count estimate
    /// inside a repository, and `FALLBACK_DEFAULT_ABBREV` outside one, where
    /// `diff --no-index` reads it.
    #[test]
    fn core_abbrev_reads_the_way_git_reads_it() {
        let auto = || 12usize;
        // Absent and `auto` both defer.
        assert_eq!(resolve(None, 40, auto), 12);
        assert_eq!(resolve(Some("auto".into()), 40, auto), 12);
        // The false-y words mean the whole name.
        for word in ["no", "off", "false"] {
            assert_eq!(resolve(Some(word.into()), 40, auto), 40, "{word}");
        }
        // A number is itself — this is the `core.abbrev = 10` that makes a
        // `--no-index` `index` line ten characters wide rather than seven.
        assert_eq!(resolve(Some("10".into()), 40, auto), 10);
        // Anything unreadable falls back rather than erroring, which is what keeps
        // a bad config value from taking a diff down.
        assert_eq!(resolve(Some("nonsense".into()), 40, auto), 12);
        // With no repository the fallback is git's literal 7.
        assert_eq!(resolve(None, 40, || FALLBACK_DEFAULT_ABBREV), 7);
    }

    /// The C `long`-to-`int` narrowing inside `parse_opt_abbrev_cb`, checked
    /// against git 2.55.0 run as `git cherry -v --abbrev=<v> main feature` in a
    /// repository whose hash width is 40. `4` there prints a 4-character id and
    /// `0` prints the whole 40, which is what distinguishes these cases.
    #[test]
    fn strtol_narrowing_matches_git() {
        // Plain values survive unchanged.
        assert_eq!(parse_opt_abbrev_value("0"), Some(0));
        assert_eq!(parse_opt_abbrev_value("7"), Some(7));
        assert_eq!(parse_opt_abbrev_value("-5"), Some(-5));

        // 2^31 wraps to INT_MIN, 2^32 to 0, 2^32+4 to 4.
        assert_eq!(parse_opt_abbrev_value("2147483648"), Some(i32::MIN));
        assert_eq!(parse_opt_abbrev_value("4294967296"), Some(0));
        assert_eq!(parse_opt_abbrev_value("4294967300"), Some(4));
        assert_eq!(parse_opt_abbrev_value("9999999999"), Some(1_410_065_407));

        // Overflow saturates before it narrows, and the two directions land on
        // different values: LONG_MAX -> -1, LONG_MIN -> 0.
        assert_eq!(parse_opt_abbrev_value("99999999999999999999999999"), Some(-1));
        assert_eq!(parse_opt_abbrev_value("-99999999999999999999999999"), Some(0));
        // LONG_MIN exactly, which is reachable without saturating.
        assert_eq!(parse_opt_abbrev_value("-9223372036854775808"), Some(0));
    }

    /// Only "no digits" and "trailing bytes" are errors; leading whitespace and
    /// a leading sign are part of the grammar `strtol` accepts.
    #[test]
    fn strtol_accepts_only_a_whole_number() {
        assert_eq!(parse_opt_abbrev_value(" 12"), Some(12));
        assert_eq!(parse_opt_abbrev_value("+12"), Some(12));
        assert_eq!(parse_opt_abbrev_value("\t\n\x0b\x0c\r8"), Some(8));

        assert_eq!(parse_opt_abbrev_value(""), None);
        assert_eq!(parse_opt_abbrev_value(" "), None);
        assert_eq!(parse_opt_abbrev_value("+"), None);
        assert_eq!(parse_opt_abbrev_value("abc"), None);
        assert_eq!(parse_opt_abbrev_value("0x10"), None);
        assert_eq!(parse_opt_abbrev_value("12abc"), None);
        assert_eq!(parse_opt_abbrev_value("8 "), None);
    }

    /// The `unsigned int` truncation in `revision.c`'s `--abbrev`, checked
    /// against git 2.55.0 run as `git diff-files --abbrev=<v>` in a 40-hex
    /// repository, where 4 and 40 are distinguishable in the raw output.
    #[test]
    fn revs_abbrev_truncates_unsigned() {
        // Saturating to ULONG_MAX leaves all 32 low bits set, which beats hexsz.
        assert_eq!(parse_abbrev_arg("-1", 40), 40);
        assert_eq!(parse_abbrev_arg("99999999999999999999999999", 40), 40);
        // 2^32 truncates to 0, which is below the minimum.
        assert_eq!(parse_abbrev_arg("4294967296", 40), 4);
        assert_eq!(parse_abbrev_arg("4294967300", 40), 4);
        // No digits at all is `strtoul`'s 0, not an error.
        assert_eq!(parse_abbrev_arg("abc", 40), 4);
        assert_eq!(parse_abbrev_arg("", 40), 4);
        assert_eq!(parse_abbrev_arg("%H%n", 40), 4);
        // Leading digits win over trailing junk.
        assert_eq!(parse_abbrev_arg("12abc", 40), 12);
        assert_eq!(parse_abbrev_arg("0", 40), 4);
        assert_eq!(parse_abbrev_arg("7", 40), 7);
        assert_eq!(parse_abbrev_arg("41", 40), 40);
    }
}
