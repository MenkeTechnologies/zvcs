//! The count-and-age arm of `handle_revision_opt()` (revision.c:2341-2399) —
//! the options every history-walking verb inherits from `setup_revisions()`
//! rather than from its own option table.
//!
//! These are not parse-options entries. `setup_revisions()` hands each argv word
//! `handle_revision_opt()` does not claim back to the caller, so a verb that
//! passes `PARSE_OPT_KEEP_UNKNOWN_OPT` (or never runs parse-options at all, like
//! `diff-tree` and `diff-files`) reaches this arm for `--max-count`, `--skip`,
//! `-<digits>`, `-n`, the six date spellings and `--max-count-oldest`. The
//! spellings, the argv arithmetic and the three `die()` wordings are the same for
//! every one of them, so they live here once instead of being re-derived per verb.
//!
//! Three shapes are easy to get wrong and are the reason this module exists:
//!
//! * `-<digits>` is gated on `isdigit(arg[1])` **alone** (revision.c:2366), so
//!   `-1x` enters the arm and dies in [`parse_count`] — it is not "not an option".
//! * every long spelling takes its value attached *or* as the next argv word,
//!   because `parse_long_opt()` (diff.c:5380-5399) accepts both and dies
//!   `Option '--<name>' requires a value` when the separate form runs off the end.
//! * the value parsers are C's `strtol`/`strtoumax`, which skip leading
//!   whitespace and accept a sign, so `--max-count=' 5'` is five.

/// git's `strtol_i()` (git-compat-util.h:978-989):
///
/// ```c
/// static inline int strtol_i(char const *s, int base, int *result)
/// {
///         long ul;
///         char *p;
///
///         errno = 0;
///         ul = strtol(s, &p, base);
///         if (errno || *p || p == s || (int) ul != ul)
///                 return -1;
///         *result = ul;
///         return 0;
/// }
/// ```
///
/// `strtol` skips leading whitespace and accepts one sign, the whole string must
/// be consumed, at least one digit must be converted, and the result must survive
/// the round trip through `int` — which is why `3000000000` is rejected on every
/// platform git builds on even though it fits a `long`.
fn strtol_i(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = match b.get(i) {
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
    let digits_at = i;
    let mut num: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        num = num.checked_mul(10)?.checked_add(i64::from(b[i] - b'0'))?;
        // `strtol` itself saturates and sets ERANGE; either way `parse_count` dies.
        if num > i64::from(u32::MAX) {
            return None;
        }
        i += 1;
    }
    // `p == arg` (nothing converted), then `*p` (trailing bytes), then the
    // `(int) ul != ul` narrowing.
    if i == digits_at || i != b.len() {
        return None;
    }
    let num = if negative { -num } else { num };
    i32::try_from(num).ok()
}

/// `parse_count()` (revision.c:2277-2284):
///
/// ```c
/// static int parse_count(const char *arg)
/// {
///         int count;
///
///         if (strtol_i(arg, 10, &count) < 0)
///                 die("'%s': not an integer", arg);
///         return count;
/// }
/// ```
///
/// `Err` carries the `die()` text without its `fatal: ` prefix.
pub fn parse_count(arg: &str) -> Result<i32, String> {
    strtol_i(arg).ok_or_else(|| format!("'{arg}': not an integer"))
}

/// `parse_age()` (revision.c:2286-2296), read through `parse_timestamp`
/// (`strtoumax`):
///
/// ```c
/// static timestamp_t parse_age(const char *arg)
/// {
///         timestamp_t num;
///         char *p;
///
///         errno = 0;
///         num = parse_timestamp(arg, &p, 10);
///         if (errno || *p || p == arg)
///                 die("'%s': not a number of seconds since epoch", arg);
///         return num;
/// }
/// ```
///
/// The token is read **unsigned**, so `-1` wraps to `UINTMAX_MAX`. That one value
/// is indistinguishable from the option never having been given, because
/// `repo_init_revisions()` leaves `max_age`/`min_age` at `-1` and every reader
/// tests against that sentinel — hence `Ok(None)`.
pub fn parse_age(arg: &str) -> Result<Option<i64>, String> {
    let die = || format!("'{arg}': not a number of seconds since epoch");
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = match b.get(i) {
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
    let digits_at = i;
    let mut num: u64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        num = num
            .checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(b[i] - b'0')))
            .ok_or_else(die)?;
        i += 1;
    }
    if i == digits_at || i != b.len() {
        return Err(die());
    }
    let num = if negative { num.wrapping_neg() } else { num };
    if num == u64::MAX {
        return Ok(None);
    }
    // A bound past `i64::MAX` can only exclude every commit there is, which is
    // what the saturating conversion leaves it doing.
    Ok(Some(i64::try_from(num).unwrap_or(i64::MAX)))
}

/// `parse_long_opt()` (diff.c:5380-5399): `--name=<value>` stuck, or `--name`
/// followed by its value in the next argv slot.
///
/// ```c
/// if (*arg == '=') { /* stuck form: --option=value */
///         *optarg = arg + 1;
///         return 1;
/// }
/// if (*arg != '\0')
///         return 0;
/// /* separate form: --option value */
/// if (!argv[1])
///         die("Option '--%s' requires a value", opt);
/// *optarg = argv[1];
/// return 2;
/// ```
///
/// `None` is "this word is not that option". `Some(Err(..))` is the `die()`.
pub fn long_opt<'a, S: AsRef<str>>(
    name: &str,
    args: &'a [S],
    i: usize,
) -> Option<Result<(&'a str, usize), String>> {
    let rest = args[i].as_ref().strip_prefix("--")?.strip_prefix(name)?;
    if let Some(value) = rest.strip_prefix('=') {
        return Some(Ok((value, 1)));
    }
    if !rest.is_empty() {
        return None;
    }
    match args.get(i + 1) {
        Some(v) => Some(Ok((v.as_ref(), 2))),
        None => Some(Err(format!("Option '--{name}' requires a value"))),
    }
}

/// What one recognised word sets on `struct rev_info`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Count {
    /// `revs->max_count`. `None` is git's `-1`: no limit. Also clears `no_walk`.
    MaxCount(Option<usize>),
    /// `revs->max_count` with `max_count_type = 1` (revision.c:2349-2359).
    MaxCountOldest(Option<usize>),
    /// `revs->skip_count`, which the walk only ever tests with `> 0`, so a
    /// negative value is accepted and skips nothing.
    Skip(usize),
    /// `revs->max_age` — the `--since`/`--after`/`--max-age` side. `None` is the
    /// `-1` sentinel; see [`parse_age`].
    MaxAge(Option<i64>),
    /// `revs->min_age` — the `--until`/`--before`/`--min-age` side.
    MinAge(Option<i64>),
    /// `revs->max_age_as_filter`, the display-time twin of `--since`.
    MaxAgeAsFilter(i64),
}

/// The `rev_info` fields this arm writes, plus the bookkeeping the two
/// `die_for_incompatible_opt2()` calls read (revision.c:2342-2363).
///
/// Folding every hit through one [`Counts::apply`] is what keeps the conflict
/// diagnostics identical across verbs instead of being re-derived per verb.
#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub struct Counts {
    /// `revs->max_count`, as a limit. `None` is git's `-1`: no limit.
    pub max_count: Option<usize>,
    /// `revs->max_count_type == 1`: the limit counts from the *oldest* end.
    /// `retrieve_oldest_commits()` (revision.c:4596-4657) runs the whole walk and
    /// keeps its last `max_count` commits, still in walk order.
    pub max_count_oldest: bool,
    /// `revs->skip_count`.
    pub skip: usize,
    /// `revs->max_age`.
    pub max_age: Option<i64>,
    /// `revs->min_age`.
    pub min_age: Option<i64>,
    /// `revs->max_age_as_filter`.
    pub max_age_as_filter: Option<i64>,
    /// Whether any count spelling cleared `revs->no_walk`.
    pub no_walk_cleared: bool,
    /// `revs->max_count != -1` for the conflict test, which a `--max-count=-1`
    /// leaves false exactly as the C does.
    max_count_set: bool,
}

impl Counts {
    /// Fold one [`Count`] in, raising the `die()` text when it collides with what
    /// is already set.
    ///
    /// ```c
    /// if ((argcount = parse_long_opt("max-count", argv, &optarg))) {
    ///         if (revs->max_count_type == 1)
    ///                 die_for_incompatible_opt2(1, "--max-count", 1, "--max-count-oldest");
    /// ...
    /// } else if ((argcount = parse_long_opt("max-count-oldest", argv, &optarg))) {
    ///         if (revs->max_count_type == 0 && revs->max_count != -1)
    ///                 die_for_incompatible_opt2(1, "--max-count", 1, "--max-count-oldest");
    ///         if (revs->skip_count > 0)
    ///                 die_for_incompatible_opt2(1, "--skip", 1, "--max-count-oldest");
    /// ...
    /// } else if ((argcount = parse_long_opt("skip", argv, &optarg))) {
    ///         if (revs->max_count_type == 1)
    ///                 die_for_incompatible_opt2(1, "--skip", 1, "--max-count-oldest");
    /// ```
    ///
    /// (revision.c:2341-2364.) The named order is fixed by the call sites, so the
    /// wording never depends on which option was typed first.
    pub fn apply(&mut self, what: Count) -> Result<(), String> {
        const MC: &str = "options '--max-count' and '--max-count-oldest' cannot be used together";
        const SK: &str = "options '--skip' and '--max-count-oldest' cannot be used together";
        match what {
            Count::MaxCount(n) => {
                if self.max_count_oldest {
                    return Err(MC.to_string());
                }
                self.max_count = n;
                self.max_count_set = n.is_some();
                self.max_count_oldest = false;
                self.no_walk_cleared = true;
            }
            Count::MaxCountOldest(n) => {
                if !self.max_count_oldest && self.max_count_set {
                    return Err(MC.to_string());
                }
                if self.skip > 0 {
                    return Err(SK.to_string());
                }
                self.max_count = n;
                self.max_count_set = n.is_some();
                self.max_count_oldest = true;
                self.no_walk_cleared = true;
            }
            Count::Skip(n) => {
                if self.max_count_oldest {
                    return Err(SK.to_string());
                }
                self.skip = n;
            }
            Count::MaxAge(v) => self.max_age = v,
            Count::MinAge(v) => self.min_age = v,
            Count::MaxAgeAsFilter(v) => self.max_age_as_filter = Some(v),
        }
        Ok(())
    }
}

/// A recognised word and how many argv slots it ate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hit {
    pub consumed: usize,
    pub what: Count,
}

/// `revs->max_count = parse_count(...)`: git stores the `int` and treats every
/// negative value as "no limit", because `get_revision_internal()` only ever
/// tests `if (revs->max_count)` and decrements `if (revs->max_count > 0)`
/// (revision.c:4537-4550).
fn count_limit(n: i32) -> Option<usize> {
    (n >= 0).then_some(n as usize)
}

/// The count-and-age arm of `handle_revision_opt()` (revision.c:2341-2399), in
/// source order — the order matters, because `-<digits>` is tested before `-n`
/// and `--since-as-filter` before `--since` would swallow its prefix.
///
/// `None` means the word belongs to some other arm (or to the caller's own option
/// table). `Some(Err(..))` is a `die()` text without its `fatal: ` prefix.
pub fn parse<S: AsRef<str>>(args: &[S], i: usize) -> Option<Result<Hit, String>> {
    let arg = args[i].as_ref();

    macro_rules! long {
        ($name:literal, $build:expr) => {
            if let Some(hit) = long_opt($name, args, i) {
                let build: fn(&str) -> Result<Count, String> = $build;
                return Some(hit.and_then(|(value, consumed)| {
                    build(value).map(|what| Hit { consumed, what })
                }));
            }
        };
    }

    long!("max-count", |v| parse_count(v)
        .map(|n| Count::MaxCount(count_limit(n))));
    // `--max-count-oldest` is the one arm with no `return argcount`
    // (revision.c:2349-2359): it falls through to the function's trailing
    // `return 1`, so the separate form consumes only the option word and leaves
    // its value behind as a revision — `git log --max-count-oldest 2` really is
    // `fatal: ambiguous argument '2'` in stock git 2.55.0.
    if let Some(hit) = long_opt("max-count-oldest", args, i) {
        return Some(hit.and_then(|(value, _)| {
            parse_count(value).map(|n| Hit {
                consumed: 1,
                what: Count::MaxCountOldest(count_limit(n)),
            })
        }));
    }
    long!("skip", |v| parse_count(v).map(|n| Count::Skip(n.max(0) as usize)));

    // `} else if ((*arg == '-') && isdigit(arg[1])) {` (revision.c:2366): the
    // second byte being a digit is the whole test, so `-1x` reaches
    // `parse_count("1x")` and dies there rather than falling through as an
    // unknown option.
    if let Some(rest) = arg.strip_prefix('-') {
        if rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return Some(
                parse_count(rest).map(|n| Hit {
                    consumed: 1,
                    what: Count::MaxCount(count_limit(n)),
                }),
            );
        }
    }
    // `-n <n>` (revision.c:2370-2375) and `-n<n>` (2376-2378). The bare form has
    // its own `error()`, which is not a `die()` — `setup_revisions()` turns the
    // negative return into `usage()` for the verbs that have one.
    if arg == "-n" {
        return Some(match args.get(i + 1) {
            Some(v) => parse_count(v.as_ref()).map(|n| Hit {
                consumed: 2,
                what: Count::MaxCount(count_limit(n)),
            }),
            None => Err("-n requires an argument".to_string()),
        });
    }
    if let Some(rest) = arg.strip_prefix("-n") {
        return Some(parse_count(rest).map(|n| Hit {
            consumed: 1,
            what: Count::MaxCount(count_limit(n)),
        }));
    }

    long!("max-age", |v| parse_age(v).map(Count::MaxAge));
    long!("since", |v| Ok(Count::MaxAge(Some(crate::date::approxidate(v)))));
    long!("since-as-filter", |v| Ok(Count::MaxAgeAsFilter(
        crate::date::approxidate(v)
    )));
    long!("after", |v| Ok(Count::MaxAge(Some(crate::date::approxidate(v)))));
    long!("min-age", |v| parse_age(v).map(Count::MinAge));
    long!("before", |v| Ok(Count::MinAge(Some(crate::date::approxidate(v)))));
    long!("until", |v| Ok(Count::MinAge(Some(crate::date::approxidate(v)))));

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn hit(v: &[&str]) -> Result<Hit, String> {
        parse(&args(v), 0).expect("recognised")
    }

    #[test]
    fn strtol_i_matches_c() {
        // Leading whitespace and a sign are consumed by `strtol`.
        assert_eq!(strtol_i(" 5"), Some(5));
        assert_eq!(strtol_i("\t+5"), Some(5));
        assert_eq!(strtol_i("-1"), Some(-1));
        // Trailing bytes, an empty digit run, a non-decimal base.
        assert_eq!(strtol_i("5x"), None);
        assert_eq!(strtol_i(""), None);
        assert_eq!(strtol_i("0x5"), None);
        // `(int) ul != ul`.
        assert_eq!(strtol_i("2147483647"), Some(i32::MAX));
        assert_eq!(strtol_i("3000000000"), None);
        assert_eq!(strtol_i("-3000000000"), None);
    }

    #[test]
    fn a_digit_second_byte_is_the_whole_test_for_the_head_form() {
        assert_eq!(hit(&["-2"]).unwrap().what, Count::MaxCount(Some(2)));
        // `-01` is `parse_count("01")`, not an unknown option.
        assert_eq!(hit(&["-01"]).unwrap().what, Count::MaxCount(Some(1)));
        assert_eq!(hit(&["-0"]).unwrap().what, Count::MaxCount(Some(0)));
        // The arm is entered, so the failure is `parse_count`'s die, not a
        // fall-through to the caller's unknown-option path.
        assert_eq!(hit(&["-1x"]).unwrap_err(), "'1x': not an integer");
        // A non-digit second byte is not this arm at all.
        assert!(parse(&args(["-p"].as_slice()), 0).is_none());
    }

    #[test]
    fn every_long_spelling_takes_a_stuck_or_a_separate_value() {
        assert_eq!(
            hit(&["--max-count", "3"]),
            Ok(Hit { consumed: 2, what: Count::MaxCount(Some(3)) })
        );
        assert_eq!(
            hit(&["--max-count=3"]),
            Ok(Hit { consumed: 1, what: Count::MaxCount(Some(3)) })
        );
        assert_eq!(
            hit(&["--skip"]).unwrap_err(),
            "Option '--skip' requires a value"
        );
        assert_eq!(
            hit(&["--since"]).unwrap_err(),
            "Option '--since' requires a value"
        );
        // `-n` has its own wording and is an `error()`, not a `die()`.
        assert_eq!(hit(&["-n"]).unwrap_err(), "-n requires an argument");
        assert_eq!(
            hit(&["-n", "4"]),
            Ok(Hit { consumed: 2, what: Count::MaxCount(Some(4)) })
        );
        assert_eq!(
            hit(&["-n4"]),
            Ok(Hit { consumed: 1, what: Count::MaxCount(Some(4)) })
        );
        assert_eq!(hit(&["-nfoo"]).unwrap_err(), "'foo': not an integer");
    }

    #[test]
    fn a_negative_count_is_no_limit_and_a_negative_skip_skips_nothing() {
        assert_eq!(hit(&["--max-count=-1"]).unwrap().what, Count::MaxCount(None));
        assert_eq!(hit(&["--skip=-1"]).unwrap().what, Count::Skip(0));
    }

    #[test]
    fn the_age_spellings_split_into_the_two_rev_info_fields() {
        assert_eq!(
            hit(&["--max-age=100"]).unwrap().what,
            Count::MaxAge(Some(100))
        );
        assert_eq!(
            hit(&["--min-age", "100"]).unwrap().what,
            Count::MinAge(Some(100))
        );
        // `strtoumax` wraps `-1` to the sentinel the field already holds.
        assert_eq!(hit(&["--max-age=-1"]).unwrap().what, Count::MaxAge(None));
        assert_eq!(
            hit(&["--max-age=1x"]).unwrap_err(),
            "'1x': not a number of seconds since epoch"
        );
        // `parse_long_opt`'s `if (*arg != '\0') return 0;` is what keeps
        // `--since` from claiming this word and reading `-as-filter=@100` as a
        // stuck value.
        assert!(matches!(
            hit(&["--since-as-filter=@100"]).unwrap().what,
            Count::MaxAgeAsFilter(_)
        ));
    }
}
