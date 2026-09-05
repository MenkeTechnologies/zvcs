//! `date.c`'s `show_date()` and `parse_date_format()`.
//!
//! git renders every timestamp it prints through one function, `show_date()`,
//! driven by a `struct date_mode` that `parse_date_format()` builds from a
//! `--date=<fmt>` value or a `%(authordate:<fmt>)` atom modifier. This module is
//! that pair, ported so the `--date=`-style vocabulary is answered from one
//! place instead of being re-derived per command.
//!
//! Two details make a hand-rolled calendar the wrong tool here and libc the
//! right one:
//!
//!   * `format:<strftime>` hands the format to the platform `strftime(3)`, so
//!     every conversion specifier — including locale-dependent ones — has to be
//!     the *same* `strftime` git calls, not a re-implementation of it.
//!   * `<fmt>-local` and `human` read the process's own zone and wall clock
//!     through `localtime_r()`, which no pure calculation can substitute for.
//!
//! So the broken-down time comes from `gmtime_r`/`localtime_r` exactly as
//! `time_to_tm()` / `time_to_tm_local()` (date.c:70-84) get it.
//!
//! Timezone convention, throughout: `tz` is git's, an integer in `[-+]HHMM`
//! *decimal* form — `+0530` is `530`, `-0800` is `-800` — which is what the
//! `%+05d` in `show_date()` prints directly; the callers read it straight off
//! the object header, which is where git reads it too.

use std::ffi::CString;

/// `enum date_mode_type` (date.h).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DateType {
    Normal,
    Human,
    Relative,
    Short,
    Iso8601,
    Iso8601Strict,
    Rfc2822,
    Strftime,
    Raw,
    Unix,
}

/// `struct date_mode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DateMode {
    pub(crate) kind: DateType,
    /// The `-local` suffix: ignore the recorded zone, use the process's own.
    pub(crate) local: bool,
    /// `strftime_fmt`, set only for [`DateType::Strftime`].
    pub(crate) strftime_fmt: Option<String>,
}

impl DateMode {
    pub(crate) fn new(kind: DateType) -> Self {
        DateMode { kind, local: false, strftime_fmt: None }
    }
}

/// The two `die()`s in `parse_date_format()`, kept apart because their wording
/// differs and both are user-visible.
#[derive(Debug)]
pub(crate) enum DateFormatError {
    /// `die("date format missing colon separator: %s", format)`.
    MissingColon(String),
    /// `die("unknown date format %s", format)`.
    Unknown(String),
}

impl std::fmt::Display for DateFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DateFormatError::MissingColon(s) => {
                write!(f, "date format missing colon separator: {s}")
            }
            DateFormatError::Unknown(s) => write!(f, "unknown date format {s}"),
        }
    }
}

/// `parse_date_type()` (date.c:990-1019) — longest-prefix first, so
/// `iso8601-strict` is tried before `iso8601` and `iso-strict` before `iso`.
///
/// Returns the type and the unconsumed tail.
fn parse_date_type(format: &str) -> Option<(DateType, &str)> {
    const TABLE: &[(&str, DateType)] = &[
        ("relative", DateType::Relative),
        ("iso8601-strict", DateType::Iso8601Strict),
        ("iso-strict", DateType::Iso8601Strict),
        ("iso8601", DateType::Iso8601),
        ("iso", DateType::Iso8601),
        ("rfc2822", DateType::Rfc2822),
        ("rfc", DateType::Rfc2822),
        ("short", DateType::Short),
        ("default", DateType::Normal),
        ("human", DateType::Human),
        ("raw", DateType::Raw),
        ("unix", DateType::Unix),
        ("format", DateType::Strftime),
    ];
    TABLE
        .iter()
        .find_map(|(name, kind)| format.strip_prefix(name).map(|rest| (*kind, rest)))
}

/// `parse_date_format()` (date.c:1022-1049).
///
/// ```c
/// /* "auto:foo" is "if tty/pager, then foo, otherwise normal" */
/// if (skip_prefix(format, "auto:", &p)) {
///         if (isatty(1) || pager_in_use())
///                 format = p;
///         else
///                 format = "default";
/// }
/// ```
///
/// The `auto:` arm is part of this function in git, so it is part of it here: the
/// tail is never validated when it is not taken, which is why `auto:bogus` is a
/// silent `default` in a pipe rather than the fatal a bare `bogus` would be.
pub(crate) fn parse_date_format(format: &str) -> Result<DateMode, DateFormatError> {
    let format = match format.strip_prefix("auto:") {
        Some(rest) => {
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) || crate::pager::in_use() {
                rest
            } else {
                "default"
            }
        }
        None => format,
    };
    // "historical alias": `local` alone is `default-local`.
    let format = if format == "local" { "default-local" } else { format };

    let (kind, rest) =
        parse_date_type(format).ok_or_else(|| DateFormatError::Unknown(format.to_string()))?;
    let (local, rest) = match rest.strip_prefix("-local") {
        Some(r) => (true, r),
        None => (false, rest),
    };

    if kind == DateType::Strftime {
        let fmt = rest
            .strip_prefix(':')
            .ok_or_else(|| DateFormatError::MissingColon(format.to_string()))?;
        Ok(DateMode { kind, local, strftime_fmt: Some(fmt.to_string()) })
    } else if rest.is_empty() {
        Ok(DateMode { kind, local, strftime_fmt: None })
    } else {
        Err(DateFormatError::Unknown(format.to_string()))
    }
}

/// `%+05d` on git's `tz` integer: always signed, zero-padded to four digits.
fn tz_str(tz: i32) -> String {
    format!("{tz:+05}")
}

/// `gm_time_t()` (date.c:48-66): shift a UTC timestamp into the wall clock of
/// zone `tz`, so that reading it back with `gmtime_r` yields that zone's time.
fn gm_time_t(time: i64, tz: i32) -> i64 {
    let minutes = tz.abs();
    let minutes = (minutes / 100) * 60 + (minutes % 100);
    let minutes = if tz < 0 { -minutes } else { minutes };
    time + i64::from(minutes) * 60
}

/// `time_to_tm()`: `gmtime_r(gm_time_t(time, tz))`.
fn time_to_tm(time: i64, tz: i32) -> Option<libc::tm> {
    let t = gm_time_t(time, tz) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `gmtime_r` fills the `tm` this frame owns; NULL means the
    // timestamp is out of range, which git treats as a failed conversion.
    let ok = unsafe { libc::gmtime_r(&t, &mut tm) };
    if ok.is_null() {
        None
    } else {
        Some(tm)
    }
}

/// `time_to_tm_local()`: `localtime_r(time)`.
fn time_to_tm_local(time: i64) -> Option<libc::tm> {
    let t = time as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: as above, with the process's own zone.
    let ok = unsafe { libc::localtime_r(&t, &mut tm) };
    if ok.is_null() {
        None
    } else {
        Some(tm)
    }
}

/// `tm_to_time_t()` (date.c:18-36): like `mktime` but without normalising
/// `tm_wday`/`tm_yday`, and reading the broken-down time as UTC. Outside
/// 1970..=2099 git returns -1 and so does this.
fn tm_to_time_t(tm: &libc::tm) -> i64 {
    const MDAYS: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let year = i64::from(tm.tm_year) - 70;
    let month = i64::from(tm.tm_mon);
    let mut day = i64::from(tm.tm_mday);

    if !(0..=129).contains(&year) || !(0..=11).contains(&month) {
        return -1;
    }
    // `mdays[]` already counts the leap day for March onwards, so it is taken
    // back out except in a leap year.
    if month < 2 || (year + 2) % 4 != 0 {
        day -= 1;
    }
    if tm.tm_hour < 0 || tm.tm_min < 0 || tm.tm_sec < 0 {
        return -1;
    }
    (year * 365 + (year + 1) / 4 + MDAYS[month as usize] + day) * 24 * 60 * 60
        + i64::from(tm.tm_hour) * 60 * 60
        + i64::from(tm.tm_min) * 60
        + i64::from(tm.tm_sec)
}

/// `local_time_tzoffset()` (date.c:89-108): the process's zone offset at `t`,
/// in git's `[-+]HHMM` form, together with the local broken-down time.
fn local_time_tzoffset(time: i64) -> (i32, Option<libc::tm>) {
    let Some(tm) = time_to_tm_local(time) else {
        return (0, None);
    };
    let t_local = tm_to_time_t(&tm);
    if t_local == -1 {
        return (0, Some(tm)); /* error; just use +0000 */
    }
    let (eastwest, offset) =
        if t_local < time { (-1i64, time - t_local) } else { (1i64, t_local - time) };
    let offset = offset / 60; /* in minutes */
    let offset = (offset % 60) + (offset / 60) * 100;
    ((offset * eastwest) as i32, Some(tm))
}

/// `local_tzoffset()`: the zone half of [`local_time_tzoffset`].
fn local_tzoffset(time: i64) -> i32 {
    local_time_tzoffset(time).0
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAY_NAMES: [&str; 7] =
    ["Sundays", "Mondays", "Tuesdays", "Wednesdays", "Thursdays", "Fridays", "Saturdays"];

/// `%.3s` on the tables above.
fn abbrev3(s: &str) -> &str {
    &s[..3]
}

/// `strbuf_addftime()` (strbuf.c) — the `%s`, `%z` and `%Z` substitutions git
/// makes before handing the format to the platform `strftime(3)`, because there
/// is no portable way to pass a zone through `struct tm`.
fn addftime(fmt: &str, tm: &libc::tm, tz: i32, suppress_tz_name: bool) -> String {
    if fmt.is_empty() {
        return String::new();
    }

    let mut munged = String::new();
    let mut rest = fmt;
    while let Some(at) = rest.find('%') {
        munged.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        if let Some(r) = rest.strip_prefix('%') {
            munged.push_str("%%");
            rest = r;
        } else if let Some(r) = rest.strip_prefix('s') {
            // ```c
            // case 's':
            //         strbuf_addf(&munged_fmt, "%"PRItime,
            //                     (timestamp_t)tm_to_time_t(tm) -
            //                     3600 * (tz_offset / 100) -
            //                     60 * (tz_offset % 100));
            // ```
            //
            // `timestamp_t` is `uintmax_t` and `PRItime` is `PRIuMAX`, so the whole
            // expression is *unsigned*: a broken-down time `tm_to_time_t()` cannot
            // represent gives -1, and git prints that as `18446744073709551615`
            // rather than `-1`. `--date=format-local:%s` on the epoch is exactly
            // that case (verified against git 2.55.0), and in `git blame` its
            // twenty digits are also what `blame_date_width` measures.
            let secs = (tm_to_time_t(tm) as u64)
                .wrapping_sub(3600u64.wrapping_mul((tz / 100) as u64))
                .wrapping_sub(60u64.wrapping_mul((tz % 100) as u64));
            munged.push_str(&secs.to_string());
            rest = r;
        } else if let Some(r) = rest.strip_prefix('z') {
            munged.push_str(&tz_str(tz));
            rest = r;
        } else if suppress_tz_name && rest.starts_with('Z') {
            rest = &rest[1..];
        } else {
            // Any other specifier is left for `strftime`, one `%` at a time.
            munged.push('%');
        }
    }
    munged.push_str(rest);

    format_tm(&munged, tm)
}

/// `strftime(3)` into a growing buffer, with git's disambiguation of the
/// "produced nothing" and "did not fit" cases: append a space to the format,
/// grow until something comes out, then drop that space again.
fn format_tm(fmt: &str, tm: &libc::tm) -> String {
    let Ok(c_fmt) = CString::new(fmt) else {
        return String::new();
    };
    let mut hint = 128usize;
    let mut buf = vec![0u8; hint];
    // SAFETY: `buf` is a live allocation of `hint` bytes and `c_fmt` is
    // NUL-terminated; `strftime` writes at most `hint` bytes including the NUL.
    let mut len = unsafe { libc::strftime(buf.as_mut_ptr().cast(), hint, c_fmt.as_ptr(), tm) };

    if len == 0 {
        let Ok(padded) = CString::new(format!("{fmt} ")) else {
            return String::new();
        };
        while len == 0 {
            // A format that genuinely renders to nothing would loop forever;
            // git has the same shape, but bound it rather than hang a command.
            if hint > 1 << 20 {
                return String::new();
            }
            hint *= 2;
            buf = vec![0u8; hint];
            // SAFETY: as above, with the grown buffer.
            len = unsafe { libc::strftime(buf.as_mut_ptr().cast(), hint, padded.as_ptr(), tm) };
        }
        len -= 1; /* drop munged space */
    }
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// `show_date_normal()` (date.c:221-286), the `DATE_NORMAL` / `DATE_HUMAN`
/// renderer. `human` is `human_tm`-aware; every other caller passes a zeroed
/// `human_tm` and `human_tz == -1`, which switches the whole hiding block off
/// except for `hide.tz`, and `tz == -1` never holds for a real zone.
fn show_date_normal(
    time: i64,
    tm: &libc::tm,
    tz: i32,
    human: Option<(&libc::tm, i32)>,
    local: bool,
    now: i64,
) -> String {
    let (human_tm, human_tz) = match human {
        Some((h, z)) => (Some(h), z),
        None => (None, -1),
    };

    let mut hide_tz = local || tz == human_tz;
    let mut hide_date = false;
    let mut hide_wday = false;
    let mut hide_time = false;
    let mut hide_seconds = false;

    let mut hide_year = false;
    if let Some(h) = human_tm {
        hide_year = tm.tm_year == h.tm_year;
        if hide_year && tm.tm_mon == h.tm_mon {
            if tm.tm_mday > h.tm_mday {
                /* Future date: think timezones */
            } else if tm.tm_mday == h.tm_mday {
                hide_date = true;
                hide_wday = true;
            } else if tm.tm_mday + 5 > h.tm_mday {
                /* Leave just weekday if it was a few days ago */
                hide_date = true;
            }
        }
    }

    /* Show "today" times as just relative times */
    if hide_wday {
        return crate::date::show_date_relative(time, now);
    }

    if human_tm.is_some_and(|h| h.tm_year != 0) {
        hide_seconds = true;
        hide_tz |= !hide_date;
        hide_wday = !hide_year;
        hide_time = !hide_year;
    }

    let mut out = String::new();
    if !hide_wday {
        out.push_str(abbrev3(WEEKDAY_NAMES[tm.tm_wday as usize]));
        out.push(' ');
    }
    if !hide_date {
        out.push_str(abbrev3(MONTH_NAMES[tm.tm_mon as usize]));
        out.push(' ');
        out.push_str(&tm.tm_mday.to_string());
        out.push(' ');
    }

    if !hide_time {
        out.push_str(&format!("{:02}:{:02}", tm.tm_hour, tm.tm_min));
        if !hide_seconds {
            out.push_str(&format!(":{:02}", tm.tm_sec));
        }
    } else {
        // `strbuf_rtrim()`.
        while out.ends_with(' ') {
            out.pop();
        }
    }

    if !hide_year {
        out.push_str(&format!(" {}", tm.tm_year + 1900));
    }
    if !hide_tz {
        out.push(' ');
        out.push_str(&tz_str(tz));
    }
    out
}

/// `show_date()` (date.c:288-370).
///
/// `now` is the wall clock `DATE_HUMAN` and `DATE_RELATIVE` measure against;
/// `get_time()` reads it from `gettimeofday()` unless `GIT_TEST_DATE_NOW` is
/// set, and callers pass the same value here so a test can pin it.
pub(crate) fn show_date(time: i64, tz: i32, mode: &DateMode, now: i64) -> String {
    let mut tz = tz;

    if mode.kind == DateType::Unix {
        return time.to_string();
    }

    // `DATE_HUMAN` fills the "current time" broken-down form and zone.
    let human = if mode.kind == DateType::Human {
        local_time_tzoffset(now)
    } else {
        (-1, None)
    };

    if mode.local {
        tz = local_tzoffset(time);
    }

    if mode.kind == DateType::Raw {
        return format!("{time} {}", tz_str(tz));
    }
    if mode.kind == DateType::Relative {
        return crate::date::show_date_relative(time, now);
    }

    let tm = if mode.local { time_to_tm_local(time) } else { time_to_tm(time, tz) };
    // `if (!tm) { tm = time_to_tm(0, 0, &tmbuf); tz = 0; }`
    let (tm, tz) = match tm {
        Some(tm) => (tm, tz),
        None => (time_to_tm(0, 0).expect("epoch is representable"), 0),
    };

    match mode.kind {
        DateType::Short => format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        DateType::Iso8601 => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            tz_str(tz),
        ),
        // RFC 3339 spells a zero offset `Z`, and git follows it here — the one
        // place a `%+05d` would have been wrong.
        DateType::Iso8601Strict => {
            let zone = if tz == 0 {
                "Z".to_string()
            } else {
                let sign = if tz >= 0 { '+' } else { '-' };
                let a = tz.abs();
                format!("{sign}{:02}:{:02}", a / 100, a % 100)
            };
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{zone}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec,
            )
        }
        DateType::Rfc2822 => format!(
            "{}, {} {} {} {:02}:{:02}:{:02} {}",
            abbrev3(WEEKDAY_NAMES[tm.tm_wday as usize]),
            tm.tm_mday,
            abbrev3(MONTH_NAMES[tm.tm_mon as usize]),
            tm.tm_year + 1900,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            tz_str(tz),
        ),
        DateType::Strftime => addftime(
            mode.strftime_fmt.as_deref().unwrap_or(""),
            &tm,
            tz,
            !mode.local,
        ),
        DateType::Normal | DateType::Human => show_date_normal(
            time,
            &tm,
            tz,
            human.1.as_ref().map(|h| (h, human.0)),
            mode.local,
            now,
        ),
        DateType::Unix | DateType::Raw | DateType::Relative => {
            unreachable!("returned above")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prefix table has to stay longest-first, or `iso-strict` is swallowed
    /// by `iso` and `iso8601-strict` by `iso8601` — git orders its `skip_prefix`
    /// chain for exactly this reason (date.c:992-995).
    #[test]
    fn the_longer_iso_spellings_win_over_the_shorter_ones() {
        for (spec, want) in [
            ("iso", DateType::Iso8601),
            ("iso8601", DateType::Iso8601),
            ("iso-strict", DateType::Iso8601Strict),
            ("iso8601-strict", DateType::Iso8601Strict),
            ("rfc", DateType::Rfc2822),
            ("rfc2822", DateType::Rfc2822),
        ] {
            let mode = parse_date_format(spec).expect(spec);
            assert_eq!(mode.kind, want, "{spec}");
            assert!(!mode.local, "{spec}");
        }
    }

    /// `-local` is a suffix on the *type*, so it composes with every spelling,
    /// and bare `local` is the historical alias for `default-local`.
    #[test]
    fn the_local_suffix_composes_and_bare_local_is_the_default_alias() {
        for spec in ["iso-local", "short-local", "raw-local", "unix-local", "human-local"] {
            assert!(parse_date_format(spec).expect(spec).local, "{spec}");
        }
        let alias = parse_date_format("local").expect("local");
        assert_eq!(alias.kind, DateType::Normal);
        assert!(alias.local);
    }

    /// The two `die()`s are different messages, and `format` without a colon is
    /// the one that is easy to collapse into the generic "unknown" arm.
    #[test]
    fn a_bare_format_is_a_missing_colon_and_a_typo_is_unknown() {
        assert!(matches!(
            parse_date_format("format"),
            Err(DateFormatError::MissingColon(s)) if s == "format"
        ));
        assert!(matches!(
            parse_date_format("format-local"),
            Err(DateFormatError::MissingColon(s)) if s == "format-local"
        ));
        assert!(matches!(
            parse_date_format("bogus"),
            Err(DateFormatError::Unknown(s)) if s == "bogus"
        ));
        // A recognised type with trailing junk is "unknown", not a partial match.
        assert!(matches!(
            parse_date_format("local-bogus"),
            Err(DateFormatError::Unknown(s)) if s == "local-bogus"
        ));
        // An empty strftime format is legal and renders to nothing.
        let empty = parse_date_format("format:").expect("format:");
        assert_eq!(empty.strftime_fmt.as_deref(), Some(""));
    }

    /// The `Z` in iso-strict is the whole reason this module exists: a zero
    /// offset is `Z`, a non-zero one is `+HH:MM`. Values measured from stock
    /// git 2.55.0.
    #[test]
    fn iso_strict_writes_z_for_utc_and_a_colon_offset_otherwise() {
        let mode = parse_date_format("iso-strict").unwrap();
        assert_eq!(show_date(1_700_000_000, 0, &mode, 0), "2023-11-14T22:13:20Z");
        assert_eq!(show_date(1_700_000_000, 530, &mode, 0), "2023-11-15T03:43:20+05:30");
        assert_eq!(show_date(1_700_000_000, -800, &mode, 0), "2023-11-14T14:13:20-08:00");
    }

    /// `%s` is computed by git, not `strftime`, and it has to undo the zone
    /// shift `gm_time_t()` applied — otherwise a non-UTC zone reports the wrong
    /// epoch. Measured from stock.
    #[test]
    fn strftime_percent_s_and_percent_z_survive_a_non_utc_zone() {
        let mode = parse_date_format("format:%s|%z|%Y-%m-%d %H:%M:%S").unwrap();
        assert_eq!(
            show_date(1_700_000_000, 530, &mode, 0),
            "1700000000|+0530|2023-11-15 03:43:20"
        );
        assert_eq!(
            show_date(1_700_000_000, -800, &mode, 0),
            "1700000000|-0800|2023-11-14 14:13:20"
        );
    }

    /// The remaining fixed-shape modes, all measured from stock git 2.55.0 at
    /// `1700000000 +0000`.
    #[test]
    fn the_fixed_shape_modes_match_stock() {
        for (spec, want) in [
            ("default", "Tue Nov 14 22:13:20 2023 +0000"),
            ("short", "2023-11-14"),
            ("iso", "2023-11-14 22:13:20 +0000"),
            ("rfc2822", "Tue, 14 Nov 2023 22:13:20 +0000"),
            ("unix", "1700000000"),
            ("raw", "1700000000 +0000"),
        ] {
            let mode = parse_date_format(spec).unwrap();
            assert_eq!(show_date(1_700_000_000, 0, &mode, 0), want, "{spec}");
        }
    }
}
