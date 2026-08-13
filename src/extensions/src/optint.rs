//! `parse-options.c`'s integer value grammar, in one place.
//!
//! Every `OPT_INTEGER`/`OPT_UNSIGNED` option in git reads its value through
//! `git_parse_signed()` / `git_parse_unsigned()` (config.c), which is C
//! `strtoimax`/`strtoumax` **with base 0** followed by `get_unit_factor()`. That
//! grammar is wider than a decimal parse in three ways a caller notices:
//!
//!   * `0x10` is hexadecimal and `010` is octal — base 0, not base 10;
//!   * a leading `+` and leading ASCII whitespace are accepted;
//!   * a single `k`/`m`/`g` suffix (either case) scales by 1024/1024²/1024³ —
//!     which is what the option's own diagnostic has always advertised.
//!
//! Parsing these with Rust's `str::parse::<i64>()` rejects all three, so
//! `git grep --threads=0x10`, `git show-branch --more=1k` and
//! `git describe --candidates=+1` failed against a stock git that accepts them.
//! `010` is worse than a rejection: it parses as ten instead of eight.
//!
//! [`integer`] and [`unsigned`] return the value or the exact diagnostic
//! `parse-options.c:260-320` prints, so a call site only has to supply the
//! option's name in git's own `optname()` spelling and print what comes back.

/// How git renders the option in its diagnostics: `option \`more'` for a long
/// option, `switch \`C'` for a short one. Built by [`long_opt`] / [`short_opt`].
pub type OptName = String;

/// `optname()` for a long option: ``option `<name>'``.
pub fn long_opt(name: &str) -> OptName {
    format!("option `{name}'")
}

/// `optname()` for a short option: ``switch `<c>'``.
pub fn short_opt(c: char) -> OptName {
    format!("switch `{c}'")
}

/// The three ways `parse-options` rejects an integer, each with git's message.
///
/// The strings carry no `error: ` prefix and no trailing newline: call sites
/// differ in whether they print through `error()`, append a usage block, or
/// collect the text into a `Result`, and adding the prefix here would make the
/// common case wrong for half of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntError {
    /// `%s expects a numerical value` — the value was the empty string.
    Empty(String),
    /// `%s expects an integer value with an optional k/m/g suffix`, or the
    /// `non-negative` wording for `OPT_UNSIGNED`.
    NotANumber(String),
    /// `value %s for %s not in range [%jd,%jd]`.
    OutOfRange(String),
}

impl IntError {
    /// The message text, without git's `error: ` prefix.
    pub fn message(&self) -> &str {
        match self {
            IntError::Empty(m) | IntError::NotANumber(m) | IntError::OutOfRange(m) => m,
        }
    }
}

impl std::fmt::Display for IntError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// The inclusive upper bound `parse-options` derives from an option's
/// `precision` (the byte width of the target): `INTMAX_MAX >> (64 - 8 * n)`.
///
/// Nearly every `OPT_INTEGER` targets a C `int`, hence the default of 4.
pub const fn upper_bound(precision: u32) -> i64 {
    if precision >= 8 {
        i64::MAX
    } else {
        i64::MAX >> (64 - 8 * precision)
    }
}

/// `OPTION_INTEGER` with the given `precision` in bytes (4 for a C `int`).
///
/// Mirrors `parse-options.c:260-288`: an empty value, then `git_parse_signed()`
/// with the option's upper bound, then the explicit `value < lower_bound` check
/// that catches the one value `git_parse_signed` lets through.
pub fn integer_prec(name: &OptName, arg: &str, precision: u32) -> Result<i64, IntError> {
    let upper = upper_bound(precision);
    let lower = -upper - 1;
    if arg.is_empty() {
        return Err(IntError::Empty(format!("{name} expects a numerical value")));
    }
    let value = match parse_signed(arg, upper) {
        Ok(v) => v,
        Err(ParseFail::Range) => return Err(range_error(name, arg, lower, upper)),
        Err(ParseFail::Invalid) => {
            return Err(IntError::NotANumber(format!(
                "{name} expects an integer value with an optional k/m/g suffix"
            )))
        }
    };
    if value < lower {
        return Err(range_error(name, arg, lower, upper));
    }
    Ok(value)
}

/// `OPTION_INTEGER` for the usual `int`-sized target.
pub fn integer(name: &OptName, arg: &str) -> Result<i64, IntError> {
    integer_prec(name, arg, 4)
}

/// `OPTION_UNSIGNED` with the given `precision` in bytes.
///
/// Mirrors `parse-options.c:289-320`. The rejection wording differs from
/// [`integer`]'s — git says `non-negative` — and the range clause names `0` as
/// the lower bound, so a negative value reports out-of-range rather than
/// "not a number".
pub fn unsigned_prec(name: &OptName, arg: &str, precision: u32) -> Result<u64, IntError> {
    let upper: u64 = if precision >= 8 {
        u64::MAX
    } else {
        u64::MAX >> (64 - 8 * precision)
    };
    if arg.is_empty() {
        return Err(IntError::Empty(format!("{name} expects a numerical value")));
    }
    match parse_unsigned(arg, upper) {
        Ok(v) => Ok(v),
        // git formats *both* bounds with `PRIdMAX` even here, so the unsigned
        // upper bound is printed as its signed reinterpretation: a `size_t`
        // option reports `[0,-1]`, which is what `git gc --max-cruft-size` and
        // `git multi-pack-index --batch-size` print.
        Err(ParseFail::Range) => Err(IntError::OutOfRange(format!(
            "value {arg} for {name} not in range [0,{}]",
            upper as i64
        ))),
        Err(ParseFail::Invalid) => Err(IntError::NotANumber(format!(
            "{name} expects a non-negative integer value with an optional k/m/g suffix"
        ))),
    }
}

/// `OPTION_UNSIGNED` for a `size_t`-sized target, which is what the byte-count
/// options (`--max-pack-size`, `--window-memory`, `--max-cruft-size`) use.
pub fn unsigned(name: &OptName, arg: &str) -> Result<u64, IntError> {
    unsigned_prec(name, arg, 8)
}

/// C `strtol(arg, &end, 10)` for the option callbacks that roll their own
/// parse instead of using `OPT_INTEGER` — `commit-graph`'s `--max-new-filters`,
/// `describe`'s `--abbrev`, `grep`'s `-C`.
///
/// Two properties distinguish it from `str::parse`: leading whitespace and a
/// leading `+` are consumed, and **overflow saturates** at `LONG_MIN`/`LONG_MAX`
/// without the caller seeing an error — which is why stock git accepts
/// `--max-new-filters=99999999999999999999999999`. `None` means the value had no
/// digits at all or trailing junk, the two cases those callbacks report.
pub fn strtol_all(arg: &str) -> Option<i64> {
    let (negative, rest) = split_sign(arg);
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 || end != rest.len() {
        return None;
    }
    let mut acc: i64 = 0;
    for c in rest.chars() {
        let d = c.to_digit(10).expect("scanned as a digit") as i64;
        acc = acc.saturating_mul(10).saturating_add(d);
    }
    Some(if negative { -acc } else { acc })
}

fn range_error(name: &OptName, arg: &str, lower: i64, upper: i64) -> IntError {
    IntError::OutOfRange(format!("value {arg} for {name} not in range [{lower},{upper}]"))
}

/// Why the C parser returned zero: `errno == ERANGE` or `errno == EINVAL`.
enum ParseFail {
    Range,
    Invalid,
}

/// `git_parse_signed()` (config.c): `strtoimax(value, &end, 0)` followed by
/// `get_unit_factor(end)`, with the overflow test git performs *before*
/// multiplying so a value that would wrap reports `ERANGE` instead.
fn parse_signed(value: &str, max: i64) -> Result<i64, ParseFail> {
    let (negative, rest) = split_sign(value);
    let magnitude = strtoumax(rest)?;
    // `strtoimax` itself overflows before any unit is applied.
    let limit = if negative { i64::MAX as u128 + 1 } else { i64::MAX as u128 };
    if magnitude.digits > limit {
        return Err(ParseFail::Range);
    }
    let factor = unit_factor(magnitude.tail)?;
    let scaled = magnitude.digits.checked_mul(factor as u128).ok_or(ParseFail::Range)?;
    // git's own bound check, expressed without the division its C form needs.
    let bound = if negative { max as u128 + 1 } else { max as u128 };
    if scaled > bound {
        return Err(ParseFail::Range);
    }
    let scaled = scaled as i64;
    Ok(if negative { -scaled } else { scaled })
}

/// `git_parse_unsigned()` (config.c): as [`parse_signed`], except that a leading
/// `-` is refused up front the way git refuses it, and the bound is unsigned.
fn parse_unsigned(value: &str, max: u64) -> Result<u64, ParseFail> {
    let (negative, rest) = split_sign(value);
    if negative {
        return Err(ParseFail::Invalid);
    }
    let magnitude = strtoumax(rest)?;
    if magnitude.digits > u64::MAX as u128 {
        return Err(ParseFail::Range);
    }
    let factor = unit_factor(magnitude.tail)?;
    let scaled = magnitude.digits.checked_mul(factor as u128).ok_or(ParseFail::Range)?;
    if scaled > max as u128 {
        return Err(ParseFail::Range);
    }
    Ok(scaled as u64)
}

/// Strip the leading ASCII whitespace and optional sign `strtoimax` consumes.
fn split_sign(raw: &str) -> (bool, &str) {
    let rest = raw.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    match rest.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    }
}

/// The digit run `strtoumax` reads with base 0, plus whatever follows it.
struct Magnitude<'a> {
    digits: u128,
    tail: &'a str,
}

/// `strtoumax(nptr, &end, 0)` over an already-unsigned string: `0x`/`0X`
/// introduces hex, a leading `0` introduces octal, anything else is decimal.
fn strtoumax(rest: &str) -> Result<Magnitude<'_>, ParseFail> {
    let (radix, digits) = if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16u32, r)
    } else if rest.len() > 1 && rest.starts_with('0') {
        // Base 0 reads a leading zero as octal and the zero is itself a digit,
        // so `0k` is zero with a `k` suffix rather than an empty number.
        (8, rest)
    } else {
        (10, rest)
    };
    let split = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    let (number, tail) = digits.split_at(split);
    if number.is_empty() {
        return Err(ParseFail::Invalid);
    }
    // Saturate rather than wrap: anything past `u128` is `ERANGE` either way.
    let mut acc: u128 = 0;
    for c in number.chars() {
        let d = c.to_digit(radix).expect("scanned as a digit") as u128;
        acc = acc.saturating_mul(radix as u128).saturating_add(d);
        if acc >= u128::MAX / 2 {
            return Err(ParseFail::Range);
        }
    }
    Ok(Magnitude { digits: acc, tail })
}

/// `get_unit_factor()` (config.c): nothing, or exactly one `k`/`m`/`g`.
fn unit_factor(tail: &str) -> Result<u64, ParseFail> {
    match tail.as_bytes() {
        [] => Ok(1),
        [u] => match u | 0x20 {
            b'k' => Ok(1024),
            b'm' => Ok(1024 * 1024),
            b'g' => Ok(1024 * 1024 * 1024),
            _ => Err(ParseFail::Invalid),
        },
        _ => Err(ParseFail::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base-0 grammar, checked against what stock git 2.55.0 accepts for
    /// `git show-branch --more=<v>` and `git grep --threads=<v>`.
    #[test]
    fn base_zero_and_unit_suffixes() {
        let n = long_opt("more");
        assert_eq!(integer(&n, "0x10"), Ok(16));
        assert_eq!(integer(&n, "010"), Ok(8));
        assert_eq!(integer(&n, "+1"), Ok(1));
        assert_eq!(integer(&n, "1k"), Ok(1024));
        assert_eq!(integer(&n, "2M"), Ok(2 * 1024 * 1024));
        assert_eq!(integer(&n, "-3"), Ok(-3));
        assert_eq!(integer(&n, " 7"), Ok(7));
        assert_eq!(integer(&n, "0"), Ok(0));
    }

    /// Each rejection carries the message git prints for it, verbatim.
    #[test]
    fn rejections_use_gits_wording() {
        let n = long_opt("more");
        assert_eq!(
            integer(&n, "").unwrap_err().message(),
            "option `more' expects a numerical value"
        );
        assert_eq!(
            integer(&n, "abc").unwrap_err().message(),
            "option `more' expects an integer value with an optional k/m/g suffix"
        );
        assert_eq!(
            integer(&n, "1x").unwrap_err().message(),
            "option `more' expects an integer value with an optional k/m/g suffix"
        );
        assert_eq!(
            integer(&n, "99999999999999999999999999").unwrap_err().message(),
            "value 99999999999999999999999999 for option `more' not in range \
             [-2147483648,2147483647]"
        );
        // The `int` bound is the option's, not `i64`'s: one past it is a range
        // error even though it fits a machine word.
        assert_eq!(
            integer(&n, "2147483648").unwrap_err().message(),
            "value 2147483648 for option `more' not in range [-2147483648,2147483647]"
        );
        assert_eq!(integer(&n, "2147483647"), Ok(2147483647));
        assert_eq!(integer(&n, "-2147483648"), Ok(-2147483648));
    }

    /// A `k` suffix multiplies *before* the bound is applied, so `4096k` is out
    /// of range for an `int` option even though `4096` is not.
    #[test]
    fn suffix_overflow_is_a_range_error() {
        let n = long_opt("window-memory");
        assert!(matches!(integer(&n, "4096k"), Ok(4194304)));
        assert!(matches!(
            integer(&n, "4096m").unwrap_err(),
            IntError::OutOfRange(_)
        ));
    }

    /// `OPT_UNSIGNED` differs in wording and refuses a negative outright.
    #[test]
    fn unsigned_wording_and_sign() {
        let n = long_opt("batch-size");
        assert_eq!(unsigned(&n, "0x400"), Ok(1024));
        assert_eq!(
            unsigned(&n, "-1").unwrap_err().message(),
            "option `batch-size' expects a non-negative integer value with an optional k/m/g suffix"
        );
        assert_eq!(
            unsigned(&n, "").unwrap_err().message(),
            "option `batch-size' expects a numerical value"
        );
        // A `size_t` bound is printed through git's `PRIdMAX`, so it reads -1.
        assert_eq!(
            unsigned(&n, "99999999999999999999999999").unwrap_err().message(),
            "value 99999999999999999999999999 for option `batch-size' not in range [0,-1]"
        );
        // An `unsigned int` bound is small enough to print as itself, which is
        // what `git grep --after-context` and `git apply -C` report.
        assert_eq!(
            unsigned_prec(&long_opt("after-context"), "99999999999999999999999999", 4)
                .unwrap_err()
                .message(),
            "value 99999999999999999999999999 for option `after-context' not in range \
             [0,4294967295]"
        );
    }

    /// `strtol` saturates rather than failing, so a value far past `long` is
    /// still accepted — stock git takes `--max-new-filters=<26 digits>`.
    #[test]
    fn strtol_saturates_and_requires_full_consumption() {
        assert_eq!(strtol_all("99999999999999999999999999"), Some(i64::MAX));
        assert_eq!(strtol_all("-7"), Some(-7));
        assert_eq!(strtol_all("+7"), Some(7));
        // Base 10, so a hex spelling stops at `x` and leaves trailing junk.
        assert_eq!(strtol_all("0x10"), None);
        assert_eq!(strtol_all("1k"), None);
        assert_eq!(strtol_all(""), None);
    }

    /// A short option is named `switch \`c'`, which is what `git apply -C` and
    /// `git diff -l` print.
    #[test]
    fn short_option_naming() {
        let n = short_opt('l');
        assert_eq!(
            integer(&n, "zz").unwrap_err().message(),
            "switch `l' expects an integer value with an optional k/m/g suffix"
        );
    }
}
