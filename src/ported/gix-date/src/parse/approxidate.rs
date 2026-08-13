//! Port of git's approximate date parser — `date.c` from git v2.55.0.
//!
//! [`parse()`][crate::parse()] implements the *strict* half of git's date handling: a set of
//! known formats, each either matching or not. git's command line is far more permissive. Every
//! `--since`/`--until`/`--before`/`--after`/`--expire`/`--mtime` value goes through
//! [`approxidate_careful()`], which first tries [`parse_date_basic()`] — a byte-at-a-time
//! tokenizer, not a format list — and, when that fails, falls back to [`approxidate_str()`]'s
//! fuzzy pass that folds whatever number/word fragments it recognizes into "now".
//!
//! The behavioral difference that matters most: a bare integer is a unix timestamp to git
//! **only at `100000000` and above** (`match_digit()`, date.c:686). Below that it is a
//! day-of-month, a two-digit year, a four-digit year or a timezone, and `0` is nothing at all —
//! so `--since=0` means *now*, not the epoch.
//!
//! ## Relationship to the C
//!
//! Every function here keeps its C name and its C control flow, including the parts that read
//! oddly out of context (`set_date()` mutating `tm` on a path that then reports failure,
//! `match_tz()` letting a failed `strtoul` advance the cursor by one). Deviations are called out
//! individually in the doc comment of the function that has them.
//!
//! ## "now" and the local timezone
//!
//! git reads the clock in `get_time()` and the zone from `localtime_r`/`mktime`. Both are
//! parameters here instead: the public entry points take `now` as epoch seconds, and the
//! `*_in` variants additionally take the [`TimeZone`], so the whole parser is a pure function of
//! its inputs and can be tested against fixed instants.

use jiff::{
    SignedDuration, Timestamp,
    civil::{self, Date},
    tz::TimeZone,
};

use crate::{SecondsSinceUnixEpoch, Time};

/// git's `struct tm`, with the same "-1 means unset" convention `parse_date_basic()` relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    /// `tm_isdst`: `1` in DST, `0` outside it, `-1` unknown. Carried from `localtime_r()` and fed
    /// back to [`mktime()`], which is how a relative date computed in summer keeps summer's
    /// offset when it lands on a winter day.
    isdst: i32,
}

impl Tm {
    /// The all-unset `tm` `parse_date_basic()` starts from (date.c:892-898).
    const fn unset() -> Self {
        Tm {
            sec: -1,
            min: -1,
            hour: -1,
            mday: -1,
            mon: -1,
            year: -1,
            wday: 0,
            yday: 0,
            isdst: -1,
        }
    }
}

/// `date[i]`, with C's guarantee that reading at or past the NUL terminator yields `0`.
fn at(date: &[u8], i: usize) -> u8 {
    date.get(i).copied().unwrap_or(0)
}

/// `strtoumax(date + i, &end, 10)` — git's `parse_timestamp`.
///
/// Returns the value and the index one past the last digit consumed. As in C, a run with no
/// digits leaves the cursor where it started, and an overflowing run saturates.
fn parse_timestamp(date: &[u8], i: usize) -> (u64, usize) {
    let start = i;
    let mut j = i;
    while at(date, j).is_ascii_whitespace() {
        j += 1;
    }
    if matches!(at(date, j), b'+' | b'-') {
        j += 1;
    }
    let digits = j;
    let mut value: u64 = 0;
    while at(date, j).is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add(u64::from(at(date, j) - b'0'));
        j += 1;
    }
    if j == digits {
        return (0, start);
    }
    (value, j)
}

/// `strtol(date + i, &end, 10)`, saturating instead of setting `ERANGE`.
fn strtol(date: &[u8], i: usize) -> (i64, usize) {
    let negative = {
        let mut j = i;
        while at(date, j).is_ascii_whitespace() {
            j += 1;
        }
        at(date, j) == b'-'
    };
    let (value, end) = parse_timestamp(date, i);
    let value = i64::try_from(value).unwrap_or(i64::MAX);
    (if negative { -value } else { value }, end)
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

const WEEKDAY_NAMES: [&str; 7] = [
    "Sundays",
    "Mondays",
    "Tuesdays",
    "Wednesdays",
    "Thursdays",
    "Fridays",
    "Saturdays",
];

/// `timezone_names[]` (date.c:378-427): name, offset in hours, and a DST flag git adds on top.
const TIMEZONE_NAMES: &[(&str, i32, i32)] = &[
    ("IDLW", -12, 0),
    ("NT", -11, 0),
    ("CAT", -10, 0),
    ("HST", -10, 0),
    ("HDT", -10, 1),
    ("YST", -9, 0),
    ("YDT", -9, 1),
    ("PST", -8, 0),
    ("PDT", -8, 1),
    ("MST", -7, 0),
    ("MDT", -7, 1),
    ("CST", -6, 0),
    ("CDT", -6, 1),
    ("EST", -5, 0),
    ("EDT", -5, 1),
    ("AST", -3, 0),
    ("ADT", -3, 1),
    ("WAT", -1, 0),
    ("GMT", 0, 0),
    ("UTC", 0, 0),
    ("Z", 0, 0),
    ("WET", 0, 0),
    ("BST", 0, 1),
    ("CET", 1, 0),
    ("MET", 1, 0),
    ("MEWT", 1, 0),
    ("MEST", 1, 1),
    ("CEST", 1, 1),
    ("MESZ", 1, 1),
    ("FWT", 1, 0),
    ("FST", 1, 1),
    ("EET", 2, 0),
    ("EEST", 2, 1),
    ("WAST", 7, 0),
    ("WADT", 7, 1),
    ("CCT", 8, 0),
    ("JST", 9, 0),
    ("EAST", 10, 0),
    ("EADT", 10, 1),
    ("GST", 10, 0),
    ("NZT", 12, 0),
    ("NZST", 12, 0),
    ("NZDT", 12, 1),
    ("IDLE", 12, 0),
];

/// `tm_to_time_t()` (date.c:18): `mktime` for UTC, without normalizing `tm_wday`/`tm_yday`.
///
/// Returns `-1` for "cannot represent", exactly as the C does — callers compare against `-1`
/// rather than using an `Option`, and one of them (`set_date()`) treats `-1` as "no opinion".
fn tm_to_time_t(tm: &Tm) -> i64 {
    const MDAYS: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let year = i64::from(tm.year) - 70;
    let month = i64::from(tm.mon);
    let mut day = i64::from(tm.mday);

    if !(0..=129).contains(&year) {
        return -1;
    }
    if !(0..=11).contains(&month) {
        return -1;
    }
    if month < 2 || (year + 2) % 4 != 0 {
        day -= 1;
    }
    if tm.hour < 0 || tm.min < 0 || tm.sec < 0 {
        return -1;
    }
    #[allow(clippy::cast_sign_loss)]
    let days = year * 365 + (year + 1) / 4 + MDAYS[month as usize] + day;
    days * 24 * 60 * 60 + i64::from(tm.hour) * 3600 + i64::from(tm.min) * 60 + i64::from(tm.sec)
}

/// `localtime_r()` for an arbitrary zone; `None` where the C returns `NULL`.
fn broken_down(t: i64, tz: &TimeZone) -> Option<Tm> {
    let timestamp = Timestamp::from_second(t).ok()?;
    let zoned = timestamp.to_zoned(tz.clone());
    Some(Tm {
        sec: i32::from(zoned.second()),
        min: i32::from(zoned.minute()),
        hour: i32::from(zoned.hour()),
        mday: i32::from(zoned.day()),
        mon: i32::from(zoned.month()) - 1,
        year: i32::from(zoned.year()) - 1900,
        wday: i32::from(zoned.weekday().to_sunday_zero_offset()),
        yday: i32::from(zoned.day_of_year()) - 1,
        isdst: i32::from(tz.to_offset_info(timestamp).dst().is_dst()),
    })
}

/// `gmtime_r()`.
fn gmtime_r(t: i64) -> Option<Tm> {
    broken_down(t, &TimeZone::UTC)
}

/// `mktime()`: POSIX field normalization followed by a local-time-to-epoch conversion.
///
/// Returns `-1` on failure, as the C does. Unlike the C this never writes normalized fields back
/// into `tm` — every git call site immediately overwrites `tm` with a `localtime_r()` of the
/// result (`update_tm()`, date.c:1104) or discards it (`parse_date_basic()`, date.c:936).
fn mktime(tm: &Tm, tz: &TimeZone) -> i64 {
    // POSIX normalizes out-of-range months into the year, then everything else into the date.
    let total_months = i64::from(tm.year) * 12 + i64::from(tm.mon);
    let year = 1900 + total_months.div_euclid(12);
    let month = total_months.rem_euclid(12) + 1;
    if !(-9999..=9999).contains(&year) {
        return -1;
    }
    let Ok(first) = Date::new(year as i16, month as i8, 1) else {
        return -1;
    };
    let offset_seconds = (i64::from(tm.mday) - 1) * 24 * 60 * 60
        + i64::from(tm.hour) * 3600
        + i64::from(tm.min) * 60
        + i64::from(tm.sec);
    let Ok(shifted) = first
        .to_datetime(civil::Time::midnight())
        .checked_add(SignedDuration::from_secs(offset_seconds))
    else {
        return -1;
    };
    let Ok(zoned) = tz.to_zoned(shifted) else {
        return -1;
    };
    let seconds = zoned.timestamp().as_second();

    // POSIX lets `tm_isdst` override what the zone would pick on its own, and libc obliges by
    // shifting the result an hour when the caller insists on the other DST state. That is how
    // `1 year ago` asked in summer keeps summer's offset even though the answer is in winter.
    // A negative `tm_isdst` means "you decide", which is what `parse_date_basic()` always asks
    // for and what the zone lookup above already answered.
    if tm.isdst < 0 {
        return seconds;
    }
    let actual = i32::from(tz.to_offset_info(zoned.timestamp()).dst().is_dst());
    match (tm.isdst, actual) {
        (1, 0) => seconds - 3600,
        (0, 1) => seconds + 3600,
        _ => seconds,
    }
}

/// `match_string()` (date.c:431): how many leading bytes of `date[i..]` case-insensitively match
/// `expected`, or `0` for a genuine mismatch.
///
/// The asymmetry is load-bearing: the walk is driven by the *input*, so a shorter input that runs
/// into a non-alphanumeric byte stops early and reports a partial match, while a longer
/// alphanumeric input than `expected` is a mismatch.
fn match_string(date: &[u8], i: usize, expected: &str) -> usize {
    let expected = expected.as_bytes();
    let mut n = 0usize;
    loop {
        let d = at(date, i + n);
        if d == 0 {
            break;
        }
        let s = expected.get(n).copied().unwrap_or(0);
        if d == s || d.to_ascii_uppercase() == s.to_ascii_uppercase() {
            n += 1;
            continue;
        }
        if !d.is_ascii_alphanumeric() {
            break;
        }
        return 0;
    }
    n
}

/// `skip_alpha()` (date.c:447): the length of the alphabetic run starting at `date[i]`, counting
/// the first byte unconditionally.
fn skip_alpha(date: &[u8], i: usize) -> usize {
    let mut n = 0usize;
    loop {
        n += 1;
        if !at(date, i + n).is_ascii_alphabetic() {
            return n;
        }
    }
}

/// `match_alpha()` (date.c:459): a month, weekday or timezone name, `AM`/`PM`, the `T` of a
/// compact ISO-8601 timestamp — or, failing all of those, an alphabetic run to skip over.
fn match_alpha(date: &[u8], i: usize, tm: &mut Tm, offset: &mut i32) -> usize {
    for (idx, name) in MONTH_NAMES.iter().enumerate() {
        let m = match_string(date, i, name);
        if m >= 3 {
            tm.mon = idx as i32;
            return m;
        }
    }

    for (idx, name) in WEEKDAY_NAMES.iter().enumerate() {
        let m = match_string(date, i, name);
        if m >= 3 {
            tm.wday = idx as i32;
            return m;
        }
    }

    for &(name, off, dst) in TIMEZONE_NAMES {
        let m = match_string(date, i, name);
        if m >= 3 || m == name.len() {
            // "This is bogus, but we like summer" (date.c:487).
            let off = off + dst;
            if *offset == -1 {
                *offset = 60 * off;
            }
            return m;
        }
    }

    if match_string(date, i, "PM") == 2 {
        tm.hour = (tm.hour % 12) + 12;
        return 2;
    }
    if match_string(date, i, "AM") == 2 {
        tm.hour = tm.hour % 12;
        return 2;
    }

    // ISO-8601 allows yyyymmDD'T'HHMMSS, with less precision.
    if at(date, i) == b'T' && at(date, i + 1).is_ascii_digit() && tm.hour == -1 {
        tm.min = 0;
        tm.sec = 0;
        return 1;
    }

    skip_alpha(date, i)
}

/// `set_date()` (date.c:515): commit a year/month/day triple to `tm` if it is a plausible date.
///
/// With `now_tm` present the triple is validated against a copy first and rejected if it lands
/// more than ten days in the future; without it the fields are written straight into `tm`, which
/// is why the `-1` return can still leave `tm.mon`/`tm.mday` modified. That is the C behavior and
/// callers depend on it.
fn set_date(year: i64, month: i64, day: i64, now_tm: Option<&Tm>, now: i64, tm: &mut Tm) -> i32 {
    if !(month > 0 && month < 13 && day > 0 && day < 32) {
        return -1;
    }

    let Some(now_tm) = now_tm else {
        tm.mon = (month - 1) as i32;
        tm.mday = day as i32;
        if year == -1 {
            return 1;
        }
        if (1970..2100).contains(&year) {
            tm.year = (year - 1900) as i32;
        } else if year > 70 && year < 100 {
            tm.year = year as i32;
        } else if year < 38 {
            tm.year = (year + 100) as i32;
        } else {
            return -1;
        }
        return 0;
    };

    let mut check = *tm;
    check.mon = (month - 1) as i32;
    check.mday = day as i32;
    if year == -1 {
        check.year = now_tm.year;
    } else if (1970..2100).contains(&year) {
        check.year = (year - 1900) as i32;
    } else if year > 70 && year < 100 {
        check.year = year as i32;
    } else if year < 38 {
        check.year = (year + 100) as i32;
    } else {
        return -1;
    }

    // "It does not make sense to specify timestamp way into the future" (date.c:634).
    let specified = tm_to_time_t(&check);
    if specified != -1 && now + 10 * 24 * 3600 < specified {
        return -1;
    }
    tm.mon = check.mon;
    tm.mday = check.mday;
    if year != -1 {
        tm.year = check.year;
    }
    0
}

/// `set_time()` (date.c:557). The 61st second is accepted, for leap seconds.
fn set_time(hour: i64, minute: i64, second: i64, tm: &mut Tm) -> i32 {
    if (0..=24).contains(&hour) && (0..60).contains(&minute) && (0..=60).contains(&second) {
        tm.hour = hour as i32;
        tm.min = minute as i32;
        tm.sec = second as i32;
        return 0;
    }
    -1
}

/// `is_date_known()` (date.c:571).
fn is_date_known(tm: &Tm) -> bool {
    tm.year != -1 && tm.mon != -1 && tm.mday != -1
}

/// `match_multi_number()` (date.c:576): `num<sep>num[<sep>num]`, read as a time for `:` and as a
/// date for `-`, `/` and `.`.
///
/// Returns the number of bytes consumed from `date[start..]`, or `0` for no match.
fn match_multi_number(
    num: u64,
    c: u8,
    date: &[u8],
    start: usize,
    mut end: usize,
    tm: &mut Tm,
    now: i64,
) -> usize {
    let (num2, next) = strtol(date, end + 1);
    end = next;
    let mut num3: i64 = -1;
    if at(date, end) == c && at(date, end + 1).is_ascii_digit() {
        let (v, next) = strtol(date, end + 1);
        num3 = v;
        end = next;
    }

    match c {
        b':' => {
            if num3 < 0 {
                num3 = 0;
            }
            if set_time(num as i64, num2, num3, tm) == 0 {
                // A `.<digits>` tail after a full HH:MM:SS is a fractional second, but only once
                // the date is already known — otherwise it is somebody else's separator.
                if at(date, end) == b'.' && at(date, end + 1).is_ascii_digit() && is_date_known(tm)
                {
                    end = strtol(date, end + 1).1;
                }
            } else {
                return 0;
            }
        }
        b'-' | b'/' | b'.' => {
            let refuse_future = gmtime_r(now);
            let refuse_future = refuse_future.as_ref();

            let mut matched = false;
            if num > 70 {
                // yyyy-mm-dd?
                if set_date(num as i64, num2, num3, None, now, tm) == 0 {
                    matched = true;
                } else if set_date(num as i64, num3, num2, None, now, tm) == 0 {
                    // yyyy-dd-mm?
                    matched = true;
                }
            }
            // "Our eastern European friends say dd.mm.yy[yy] is the norm there, so giving
            // precedence to mm/dd/yy[yy] form only when separator is not '.'" (date.c:616).
            if !matched
                && c != b'.'
                && set_date(num3, num as i64, num2, refuse_future, now, tm) == 0
            {
                matched = true;
            }
            if !matched && set_date(num3, num2, num as i64, refuse_future, now, tm) == 0 {
                matched = true;
            }
            if !matched
                && c == b'.'
                && set_date(num3, num as i64, num2, refuse_future, now, tm) == 0
            {
                matched = true;
            }
            if !matched {
                return 0;
            }
        }
        _ => {}
    }
    end - start
}

/// `nodate()` (date.c:646): nothing of the date or time has been filled in yet.
fn nodate(tm: &Tm) -> bool {
    (tm.year & tm.mon & tm.mday & tm.hour & tm.min & tm.sec) < 0
}

/// `maybeiso8601()` (date.c:661): a compact ISO-8601 `T` was seen, so minutes and seconds were
/// zeroed while the hour is still unknown.
fn maybeiso8601(tm: &Tm) -> bool {
    tm.hour == -1 && tm.min == 0 && tm.sec == 0
}

/// `match_digit()` (date.c:671): the number-shaped half of `parse_date_basic()`.
///
/// This is where a bare integer becomes a unix timestamp — but only from `100000000` up, so that
/// `20070606` can still be read as a `YYYYMMDD` date.
///
/// ### Deviation
///
/// git hands `match_multi_number()` a literal `0` here, which that function reads as "call
/// `time(NULL)` yourself". `now` is threaded through instead so the ten-day-in-the-future check
/// stays a function of the caller's clock.
fn match_digit(
    date: &[u8],
    start: usize,
    tm: &mut Tm,
    offset: &mut i32,
    tm_gmt: &mut bool,
    now: i64,
) -> usize {
    let (num, mut end) = parse_timestamp(date, start);

    // Seconds since 1970, for anything with more than 8 digits.
    if num >= 100_000_000 && nodate(tm) {
        if let Some(utc) = gmtime_r(num as i64) {
            *tm = utc;
            *tm_gmt = true;
            return end - start;
        }
    }

    // num[-.:/]num[same]num
    if matches!(at(date, end), b':' | b'.' | b'/' | b'-') && at(date, end + 1).is_ascii_digit() {
        let m = match_multi_number(num, at(date, end), date, start, end, tm, now);
        if m != 0 {
            return m;
        }
    }

    // How many digits did the caller actually give us? The guess below keys off that.
    let mut n = 0usize;
    loop {
        n += 1;
        if !at(date, start + n).is_ascii_digit() {
            break;
        }
    }

    // 8 digits: compact ISO-8601 date YYYYmmDD. 6 digits: compact ISO-8601 time HHMMSS.
    if n == 8 || n == 6 {
        let num1 = (num / 10000) as i64;
        let num2 = ((num % 10000) / 100) as i64;
        let num3 = (num % 100) as i64;
        if n == 8 {
            // git passes `time(NULL)` here, but `now_tm` is `NULL` so `set_date()` never reads it.
            set_date(num1, num2, num3, None, now, tm);
        } else if set_time(num1, num2, num3, tm) == 0
            && at(date, end) == b'.'
            && at(date, end + 1).is_ascii_digit()
        {
            end = parse_timestamp(date, end + 1).1;
        }
        return end - start;
    }

    // Reduced precision of ISO-8601's time: HHMM or HH.
    if maybeiso8601(tm) {
        let mut num1 = num as i64;
        let mut num2 = 0i64;
        if n == 4 {
            num1 = (num / 100) as i64;
            num2 = (num % 100) as i64;
        }
        if (n == 4 || n == 2) && !nodate(tm) && set_time(num1, num2, 0, tm) == 0 {
            return n;
        }
        // It looked like an ISO-8601 time but was not; roll the zeroing back.
        tm.min = -1;
        tm.sec = -1;
    }

    // Four-digit year or a timezone?
    if n == 4 {
        if num <= 1400 && *offset == -1 {
            let minutes = (num % 100) as i32;
            let hours = (num / 100) as i32;
            *offset = hours * 60 + minutes;
        } else if num > 1900 && num < 2100 {
            tm.year = (num as i64 - 1900) as i32;
        }
        return n;
    }

    // Ignore lots of numerals; days and months are one or two digits.
    if n > 2 {
        return n;
    }

    // Day-of-month wins over month or year in the 1-12 range: `01 Apr 05` is April 1st, 2005.
    if num > 0 && num < 32 && tm.mday < 0 {
        tm.mday = num as i32;
        return n;
    }

    if n == 2 && tm.year < 0 {
        if num < 10 && tm.mday >= 0 {
            tm.year = num as i32 + 100;
            return n;
        }
        if num >= 70 {
            tm.year = num as i32;
            return n;
        }
    }

    if num > 0 && num < 13 && tm.mon < 0 {
        tm.mon = num as i32 - 1;
    }

    n
}

/// `match_tz()` (date.c:798): a `±hhmm`, `±hh:mm` or `±hh` zone suffix.
///
/// The cursor advance is whatever `strtoul` consumed even when the zone is rejected as "random
/// crap" — including the one byte a failed `strtoul` past a `:` still eats.
fn match_tz(date: &[u8], start: usize, offp: &mut i32) -> usize {
    let (hour, mut end) = parse_timestamp(date, start + 1);
    let mut hour = hour as i64;
    let n = end - (start + 1);
    let mut min: i64 = 0;

    if n == 4 {
        min = hour % 100;
        hour /= 100;
    } else if n != 2 {
        min = 99; // random crap
    } else if at(date, end) == b':' {
        let (m, next) = parse_timestamp(date, end + 1);
        min = m as i64;
        end = if next == end + 1 { end + 1 } else { next };
        if end - (start + 1) != 5 {
            min = 99; // random crap
        }
    }

    if min < 60 && hour < 24 {
        let mut offset = (hour * 60 + min) as i32;
        if at(date, start) == b'-' {
            offset = -offset;
        }
        *offp = offset;
    }
    end - start
}

/// timestamp of 2099-12-31T23:59:59Z, including 32 leap days (date.c:876).
const TIMESTAMP_MAX: i64 = ((2100 - 1970) * 365 + 32) * 24 * 60 * 60 - 1;

/// `match_object_header_date()` (date.c:850): the raw `<timestamp> <±hhmm>` behind an `@`.
fn match_object_header_date(date: &[u8]) -> Option<(i64, i32)> {
    if !at(date, 0).is_ascii_digit() {
        return None;
    }
    let (stamp, end) = parse_timestamp(date, 0);
    if at(date, end) != b' ' || stamp == u64::MAX || !matches!(at(date, end + 1), b'+' | b'-') {
        return None;
    }
    let sign_idx = end + 1;
    let digits_start = end + 2;
    let (ofs, ofs_end) = strtol(date, digits_start);
    let tail = at(date, ofs_end);
    if (tail != 0 && tail != b'\n') || ofs_end != digits_start + 4 {
        return None;
    }
    let mut ofs = (ofs / 100) * 60 + (ofs % 100);
    if at(date, sign_idx) == b'-' {
        ofs = -ofs;
    }
    Some((i64::try_from(stamp).unwrap_or(i64::MAX), ofs as i32))
}

/// `parse_date_basic()` (date.c:879): git's strict-ish date parser, in the given timezone.
///
/// Returns the instant and its offset in *minutes*, or `None` where the C returns `-1`.
///
/// ### Deviation
///
/// `timestamp_t` is `uintmax_t` in C, so two of the overflow guards at the end are unsigned
/// comparisons. They are signed here. The values differ only for `tm` combinations that place the
/// instant before the epoch, which `tm_to_time_t()` has already rejected for every year it
/// accepts except the first days of 1970 with an unset day-of-month.
fn parse_date_basic_in(date: &[u8], now: i64, tz: &TimeZone) -> Option<(i64, i32)> {
    let mut tm = Tm::unset();
    let mut offset: i32 = -1;
    let mut tm_gmt = false;

    if at(date, 0) == b'@' {
        if let Some(parsed) = match_object_header_date(&date[1..]) {
            return Some(parsed);
        }
    }

    let mut i = 0usize;
    loop {
        let c = at(date, i);
        if c == 0 || c == b'\n' {
            break;
        }
        let mut m = 0usize;
        if c.is_ascii_alphabetic() {
            m = match_alpha(date, i, &mut tm, &mut offset);
        } else if c.is_ascii_digit() {
            m = match_digit(date, i, &mut tm, &mut offset, &mut tm_gmt, now);
        } else if matches!(c, b'-' | b'+') && at(date, i + 1).is_ascii_digit() {
            m = match_tz(date, i, &mut offset);
        }
        if m == 0 {
            m = 1; // BAD CRAP
        }
        i += m;
    }

    // Not `mktime()`, which would use the local timezone for a value we already treat as UTC.
    let mut timestamp = tm_to_time_t(&tm);
    if timestamp == -1 {
        return None;
    }

    if offset == -1 {
        // `gmtime_r()` in `match_digit()` may have clobbered `tm`.
        tm.isdst = -1;
        let temp_time = mktime(&tm, tz);
        offset = if timestamp > temp_time {
            ((timestamp - temp_time) / 60) as i32
        } else {
            -(((temp_time - timestamp) / 60) as i32)
        };
    }

    if !tm_gmt {
        if offset > 0 && i64::from(offset) * 60 > timestamp {
            return None;
        }
        if offset < 0 && -i64::from(offset) * 60 > TIMESTAMP_MAX - timestamp {
            return None;
        }
        timestamp -= i64::from(offset) * 60;
    }

    Some((timestamp, offset))
}

/// `update_tm()` (date.c:1080): fill the unset date fields from `now`, subtract `sec`, and
/// re-derive `tm` from the result.
///
/// `tm.mday` below `-1` is the deferred day adjustment `date_time()` leaves behind: `-2` means
/// yesterday, `-3` the day before that.
///
/// ### Deviation
///
/// `localtime_r()` here is [`broken_down()`], which is bounded by jiff's year range of
/// ±9999. A relative offset large enough to push the instant before year -9999 (roughly
/// 4.4 million days) leaves `tm` untouched, the same as the C's `localtime_r()` returning
/// `NULL` — but a platform whose `localtime_r()` happily extrapolates the proleptic calendar
/// that far (macOS does) then re-reads month/day/time from that result and lands somewhere
/// else. Verified boundary: `1000000 day ago` agrees with stock git, `6255520 day ago` does
/// not.
fn update_tm(tm: &mut Tm, now: &Tm, mut sec: i64, tz: &TimeZone) -> i64 {
    if tm.mday < 0 {
        let offset = tm.mday + 1;
        if sec == 0 && offset < 0 {
            sec = i64::from(-offset) * 24 * 60 * 60;
        }
        tm.mday = now.mday;
    }
    if tm.mon < 0 {
        tm.mon = now.mon;
    }
    if tm.year < 0 {
        tm.year = now.year;
        if tm.mon > now.mon {
            tm.year -= 1;
        }
    }

    let n = mktime(tm, tz) - sec;
    if let Some(updated) = broken_down(n, tz) {
        *tm = updated;
    }
    n
}

/// `pending_number()` (date.c:1108): a number nobody claimed is a day-of-month, then a month,
/// then a year.
///
/// A pending `0` is dropped outright — the reason `--since=0` resolves to *now* rather than to
/// the epoch.
fn pending_number(tm: &mut Tm, num: &mut i32) {
    let number = *num;
    if number == 0 {
        return;
    }
    *num = 0;
    if tm.mday < 0 && number < 32 {
        tm.mday = number;
    } else if tm.mon < 0 && number < 13 {
        tm.mon = number - 1;
    } else if tm.year < 0 {
        if number > 1969 && number < 2100 {
            tm.year = number - 1900;
        } else if number > 69 && number < 100 {
            tm.year = number;
        } else if number < 38 {
            tm.year = 100 + number;
        }
    }
}

/// `date_time()` (date.c:1143): snap to `hour` today, or yesterday when today's has passed.
fn date_time(tm: &mut Tm, hour: i32) {
    if tm.mday < 0 && tm.hour < hour {
        tm.mday = -2; // eventually handled by update_tm()
    }
    tm.hour = hour;
    tm.min = 0;
    tm.sec = 0;
}

/// The `special[]` table (date.c:1222): words that rewrite `tm` outright.
///
/// Returns `true` when `name` matched, having applied its effect.
fn apply_special(name: &str, tm: &mut Tm, now: &Tm, num: &mut i32, tz: &TimeZone) {
    match name {
        "yesterday" => {
            *num = 0;
            tm.mday = -1;
            update_tm(tm, now, 24 * 60 * 60, tz);
        }
        "noon" => {
            pending_number(tm, num);
            date_time(tm, 12);
        }
        "midnight" => {
            pending_number(tm, num);
            date_time(tm, 0);
        }
        "tea" => {
            pending_number(tm, num);
            date_time(tm, 17);
        }
        "PM" => {
            let n = *num;
            *num = 0;
            let mut hour = tm.hour;
            if n != 0 {
                hour = n;
                tm.min = 0;
                tm.sec = 0;
            }
            tm.hour = (hour % 12) + 12;
        }
        "AM" => {
            let n = *num;
            *num = 0;
            let mut hour = tm.hour;
            if n != 0 {
                hour = n;
                tm.min = 0;
                tm.sec = 0;
            }
            tm.hour = hour % 12;
        }
        "never" => {
            if let Some(epoch) = broken_down(0, tz) {
                *tm = epoch;
            }
            *num = 0;
        }
        "now" => {
            *num = 0;
            update_tm(tm, now, 0, tz);
        }
        "today" => {
            if tm.hour == now.hour && tm.min == now.min && tm.sec == now.sec {
                date_time(tm, 0);
            }
            *num = 0;
            tm.mday = -1;
            update_tm(tm, now, 0, tz);
        }
        _ => {}
    }
}

/// The `special[]` names, in the C's order — `match_string()` is tried against each in turn.
const SPECIAL_NAMES: [&str; 9] = [
    "yesterday",
    "noon",
    "midnight",
    "tea",
    "PM",
    "AM",
    "never",
    "now",
    "today",
];

/// `number_name[]` (date.c:1236); index 0 is never matched (the loop starts at 1).
const NUMBER_NAMES: [&str; 11] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];

/// `typelen[]` (date.c:1244): relative units and their length in seconds.
const TYPELEN: [(&str, i64); 5] = [
    ("seconds", 1),
    ("minutes", 60),
    ("hours", 60 * 60),
    ("days", 24 * 60 * 60),
    ("weeks", 7 * 24 * 60 * 60),
];

/// `approxidate_alpha()` (date.c:1253): consume one alphabetic run and apply whatever it means.
///
/// Returns the index one past the run, which is where the caller resumes regardless of whether
/// anything matched.
fn approxidate_alpha(
    date: &[u8],
    start: usize,
    tm: &mut Tm,
    now: &Tm,
    num: &mut i32,
    touched: &mut bool,
    tz: &TimeZone,
) -> usize {
    let mut end = start;
    loop {
        end += 1;
        if !at(date, end).is_ascii_alphabetic() {
            break;
        }
    }

    for (i, name) in MONTH_NAMES.iter().enumerate() {
        if match_string(date, start, name) >= 3 {
            tm.mon = i as i32;
            *touched = true;
            return end;
        }
    }

    for name in SPECIAL_NAMES {
        if match_string(date, start, name) == name.len() {
            apply_special(name, tm, now, num, tz);
            *touched = true;
            return end;
        }
    }

    if *num == 0 {
        for (i, name) in NUMBER_NAMES.iter().enumerate().skip(1) {
            if match_string(date, start, name) == name.len() {
                *num = i as i32;
                *touched = true;
                return end;
            }
        }
        if match_string(date, start, "last") == 4 {
            *num = 1;
            *touched = true;
        }
        return end;
    }

    for (type_name, length) in TYPELEN {
        // `>= len - 1` is what makes both "day" and "days" match.
        if match_string(date, start, type_name) >= type_name.len() - 1 {
            update_tm(tm, now, length * i64::from(*num), tz);
            *num = 0;
            *touched = true;
            return end;
        }
    }

    for (i, name) in WEEKDAY_NAMES.iter().enumerate() {
        if match_string(date, start, name) >= 3 {
            let mut n = *num - 1;
            *num = 0;
            let mut diff = tm.wday - i as i32;
            if diff <= 0 {
                n += 1;
            }
            diff += 7 * n;
            update_tm(tm, now, i64::from(diff) * 24 * 60 * 60, tz);
            *touched = true;
            return end;
        }
    }

    if match_string(date, start, "months") >= 5 {
        update_tm(tm, now, 0, tz); // fill in date fields if needed
        let mut n = tm.mon - *num;
        *num = 0;
        while n < 0 {
            n += 12;
            tm.year -= 1;
        }
        tm.mon = n;
        *touched = true;
        return end;
    }

    if match_string(date, start, "years") >= 4 {
        update_tm(tm, now, 0, tz); // fill in date fields if needed
        tm.year -= *num;
        *num = 0;
        *touched = true;
        return end;
    }

    end
}

/// `approxidate_digit()` (date.c:1351): consume one number, either as part of a `num-num-num`
/// group or as a value the next word will interpret.
fn approxidate_digit(date: &[u8], start: usize, tm: &mut Tm, num: &mut i32, now: i64) -> usize {
    let (number, end) = parse_timestamp(date, start);

    if matches!(at(date, end), b':' | b'.' | b'/' | b'-') && at(date, end + 1).is_ascii_digit() {
        let m = match_multi_number(number, at(date, end), date, start, end, tm, now);
        if m != 0 {
            return start + m;
        }
    }

    // Zero-padding is only accepted for small numbers: "Dec 02", never "Dec 0002".
    if at(date, start) != b'0' || end - start <= 2 {
        *num = number as i32;
    }
    end
}

/// `approxidate_str()` (date.c:1376): the fuzzy pass, run once `parse_date_basic()` has failed.
///
/// Returns the instant plus git's `error_ret` flag, which is set when nothing in the input looked
/// like a date fragment at all.
fn approxidate_str_in(date: &[u8], now_seconds: i64, tz: &TimeZone) -> (i64, bool) {
    let Some(mut tm) = broken_down(now_seconds, tz) else {
        return (now_seconds, true);
    };
    let now = tm;
    let mut number: i32 = 0;
    let mut touched = false;

    tm.year = -1;
    tm.mon = -1;
    tm.mday = -1;

    let mut i = 0usize;
    loop {
        let c = at(date, i);
        if c == 0 {
            break;
        }
        i += 1;
        if c.is_ascii_digit() {
            pending_number(&mut tm, &mut number);
            i = approxidate_digit(date, i - 1, &mut tm, &mut number, now_seconds);
            touched = true;
            continue;
        }
        if c.is_ascii_alphabetic() {
            i = approxidate_alpha(date, i - 1, &mut tm, &now, &mut number, &mut touched, tz);
        }
    }
    pending_number(&mut tm, &mut number);
    (update_tm(&mut tm, &now, 0, tz), !touched)
}

/// `approxidate_careful()` (date.c:1413) in the given timezone.
fn approxidate_careful_in(input: &str, now: SecondsSinceUnixEpoch, tz: &TimeZone) -> (i64, bool) {
    let bytes = input.as_bytes();
    if let Some((timestamp, _offset)) = parse_date_basic_in(bytes, now, tz) {
        return (timestamp, false);
    }
    approxidate_str_in(bytes, now, tz)
}

/// `parse_date_basic()` (date.c:879): git's non-fuzzy date parser, resolved in the system
/// timezone against `now` (epoch seconds).
///
/// `now` is only consulted by `match_multi_number()`, which refuses a `dd/mm/yy` reading that
/// would land more than ten days in the future. Returns `None` where git returns `-1`.
///
/// ### Deviation
///
/// git reads the clock directly (`time(NULL)`) for that ten-day check rather than going through
/// `get_time()`, so `GIT_TEST_DATE_NOW` does not reach it there. Here it does, which makes the
/// function deterministic for a given `now`.
pub fn parse_date_basic(input: &str, now: SecondsSinceUnixEpoch) -> Option<Time> {
    parse_date_basic_in(input.as_bytes(), now, &TimeZone::system())
        .map(|(seconds, offset_minutes)| Time::new(seconds, offset_minutes * 60))
}

/// `approxidate_careful()` (date.c:1413): [`parse_date_basic()`], then git's fuzzy fallback.
///
/// Returns the instant in epoch seconds plus git's `error_ret` flag — `true` when the input
/// contained nothing date-like, in which case the instant is `now`. Callers that use git's
/// `approxidate()` macro simply ignore the flag.
///
/// This is what every `--since`/`--until`/`--before`/`--after`/`--expire`/`--mtime` value goes
/// through in git, and it is *not* [`parse()`][crate::parse()]: a bare integer below
/// `100000000` is a day-of-month or a year here, never a unix timestamp, and `0` is nothing at
/// all.
///
/// ```
/// // 2005-04-07T22:13:13Z, the date of git's own first commit.
/// let now = 1_112_911_993;
/// assert_eq!(gix_date::parse::approxidate_careful("0", now), (now, false));
/// assert_eq!(gix_date::parse::approxidate_careful("1700000000", now), (1_700_000_000, false));
/// ```
pub fn approxidate_careful(input: &str, now: SecondsSinceUnixEpoch) -> (SecondsSinceUnixEpoch, bool) {
    approxidate_careful_in(input, now, &TimeZone::system())
}

/// `approxidate()` (date.h:69): [`approxidate_careful()`] with the error flag discarded.
pub fn approxidate(input: &str, now: SecondsSinceUnixEpoch) -> SecondsSinceUnixEpoch {
    approxidate_careful(input, now).0
}

/// `parse_expiry_date()` (date.c:957): [`approxidate_careful()`] with four words taken over.
///
/// `never`/`false` expire nothing (`0`); `all`/`now` expire everything. git's "everything" is
/// `TIME_MAX`, which is `uintmax_t`'s maximum; the signed equivalent [`i64::MAX`] is used here.
/// `None` reports git's non-zero return, which every caller turns into a fatal error.
pub fn parse_expiry_date(input: &str, now: SecondsSinceUnixEpoch) -> Option<SecondsSinceUnixEpoch> {
    match input {
        "never" | "false" => return Some(0),
        // git takes "now" over so that it means "expire everything that happened in the past".
        "all" | "now" => return Some(i64::MAX),
        _ => {}
    }
    let (timestamp, error) = approxidate_careful(input, now);
    if error { None } else { Some(timestamp) }
}


#[cfg(test)]
mod tests {
    use super::{TimeZone, approxidate_careful_in, approxidate_str_in, parse_date_basic_in};

    /// 2023-10-27T10:00:00Z. A Friday, so weekday-relative inputs have something to move from,
    /// and inside European/US summer time, so the DST cases below are not degenerate.
    const NOW: i64 = 1_698_400_800;

    /// 2024-01-15T12:00:00Z — the same zones, in winter.
    const WINTER: i64 = 1_705_320_000;

    /// `Europe/Berlin` and `America/New_York` as POSIX TZ rules rather than tzdb lookups, so the
    /// tests run identically on a container with no `/usr/share/zoneinfo`. Both rules reproduce
    /// every offset the expectations below were captured under.
    const BERLIN: &str = "CET-1CEST,M3.5.0,M10.5.0/3";
    const NEW_YORK: &str = "EST5EDT,M3.2.0,M11.1.0/2";

    fn zone(posix: &str) -> TimeZone {
        TimeZone::posix(posix).expect("hard-coded POSIX TZ rule is valid")
    }

    /// Every expectation in this module was captured by running git 2.55.0 itself, not by
    /// reading `date.c`:
    ///
    /// ```text
    /// TZ=<zone> GIT_TEST_DATE_NOW=<now> git rev-parse --since=<value>   # prints --max-age=<n>
    /// ```
    ///
    /// `builtin/rev-parse.c:248` feeds `--since` straight to `approxidate()`, so the printed
    /// `--max-age` is the parser's output with nothing in between.
    fn stock(input: &str, now: i64, tz: &TimeZone) -> i64 {
        approxidate_careful_in(input, now, tz).0
    }

    #[test]
    fn utc_vectors_from_stock_git() {
        let utc = TimeZone::UTC;
        // (input, what `git rev-parse --since=<input>` printed under TZ=UTC at NOW)
        let cases: &[(&str, i64)] = &[
            // The gap this port closes: small integers are not unix timestamps.
            ("0", 1_698_400_800),  // dropped entirely, so: now
            ("1", 1_696_154_400),  // the 1st of this month, at this time of day
            ("2", 1_696_240_800),  // the 2nd
            ("10", 1_696_932_000), // the 10th
            ("31", 1_698_746_400), // the 31st
            ("32", 1_982_484_000), // too big for a day: read as the year 2032
            ("70", 25_869_600),    // two-digit year 1970
            ("99", 941_018_400),   // two-digit year 1999
            ("100", 1_698_400_800), // nothing at all
            ("1400", 1_698_400_800),
            ("1900", 1_698_400_800),
            ("1970", 25_869_600), // four-digit year
            ("2023", 1_698_400_800),
            ("2100", 1_698_400_800), // out of the four-digit year range
            ("99999999", 1_698_400_800),
            ("100000000", 100_000_000), // the threshold: a timestamp from here up
            ("1112911993", 1_112_911_993),
            ("1700000000", 1_700_000_000),
            // Tokenized, not lexed: `0`, `x`, `10`.
            ("0x10", 1_696_932_000),
            // Nothing date-like resolves to now.
            ("abc", 1_698_400_800),
            ("", 1_698_400_800),
            (" ", 1_698_400_800),
            ("bogus", 1_698_400_800),
            ("20050407", 1_698_400_800), // no time of day, so `parse_date_basic()` rejects it
            // The `special[]` table.
            ("now", 1_698_400_800),
            ("yesterday", 1_698_314_400),
            ("midnight", 1_698_364_800),
            ("noon", 1_698_321_600), // 10:00 is before noon, so yesterday's
            ("tea", 1_698_339_600),  // likewise
            ("never", 0),
            ("today", 1_698_364_800),
            ("10am", 1_698_400_800),
            ("3pm", 1_698_418_800),
            // Relative units, in every spelling git's tokenizer accepts.
            ("1 second ago", 1_698_400_799),
            ("5.seconds.ago", 1_698_400_795),
            ("2 minutes ago", 1_698_400_680),
            ("ten minutes ago", 1_698_400_200),
            ("3 hours ago", 1_698_390_000),
            ("1 day ago", 1_698_314_400),
            ("three days ago", 1_698_141_600),
            ("5 days ago", 1_697_968_800),
            ("1 week ago", 1_697_796_000),
            ("1.week.ago", 1_697_796_000),
            ("last week", 1_697_796_000),
            ("2 weeks ago", 1_697_191_200),
            ("2.weeks.ago", 1_697_191_200),
            ("2weeks", 1_697_191_200),
            ("1 month ago", 1_695_808_800),
            ("1 year ago", 1_666_864_800),
            // A bare weekday is swallowed by the `!*num` branch and changes nothing.
            ("friday", 1_698_400_800),
            ("monday", 1_698_400_800),
            ("last friday", 1_697_796_000),
            // Absolute forms, which `parse_date_basic()` takes before the fuzzy pass runs.
            ("@1700000000", 1_700_000_000),
            ("@1112911993 +0200", 1_112_911_993),
            ("2005-04-07T22:13:13Z", 1_112_911_993),
            ("2005-04-07 22:13:13 +0000", 1_112_911_993),
            ("Thu, 7 Apr 2005 22:13:13 +0000", 1_112_911_993),
            ("2005-04-07 22:13:13 EST", 1_112_929_993),
            ("20050407T221313Z", 1_112_911_993),
            ("1979-02-26 18:30:00", 288_901_800),
            // Date-only forms that only the fuzzy pass can complete.
            ("1979-02-26", 288_871_200),
            ("12/25/2020", 1_608_890_400),
            ("25.12.2020", 1_608_890_400),
            // `<ref>@{<date>}` selectors (`object-name.c:780` is approxidate too). The raw
            // header spelling is *not* a raw header to approxidate: `42` is neither a day nor a
            // year, `+0030` is zero-padded past two digits and so is dropped, and the answer is
            // plain `now`.
            ("42 +0030", 1_698_400_800),
            ("2.days.ago", 1_698_228_000),
            // Relative offsets big enough to overflow a C `int` seconds product. clang computes
            // the multiply in 64 bits (signed overflow is UB, so it may assume it away), and
            // these agree with stock git exactly, including the two that land before the epoch.
            ("10000 day ago", 834_400_800),
            ("24855 day ago", -449_071_200),
            ("24856 day ago", -449_157_600),
            ("100000 day ago", 1_673_431_200),
            ("1000000 day ago", 1_669_716_000),
        ];
        for &(input, expected) in cases {
            assert_eq!(stock(input, NOW, &utc), expected, "--since={input:?}");
        }
    }

    /// git's `error_ret`, captured as the exit status of
    /// `git reflog expire --dry-run --expire=<value> --all`, which fails with
    /// `fatal: invalid timestamp` exactly when `parse_expiry_date()` reports an error.
    #[test]
    fn error_flag_matches_stock_git() {
        let utc = TimeZone::UTC;
        for accepted in [
            "0",
            "0x10",
            "@0",
            "1 day ago",
            "1700000000",
            "42 +0030",
            "2.days.ago",
            "6255520 day ago",
        ] {
            assert!(
                !approxidate_careful_in(accepted, NOW, &utc).1,
                "{accepted:?} is accepted by stock git"
            );
        }
        for rejected in ["abc", "", " ", "bogus", "zzz", "foo"] {
            assert!(
                approxidate_careful_in(rejected, NOW, &utc).1,
                "{rejected:?} is rejected by stock git"
            );
        }
    }

    /// `parse_date_basic()` in isolation, including the offset it reports.
    ///
    /// Captured with `git commit --allow-empty --date=<value>` followed by
    /// `git log -1 --pretty=%ad --date=raw`: `builtin/commit.c:617` runs `parse_date()`, which is
    /// `parse_date_basic()` plus formatting, before it ever considers approxidate.
    #[test]
    fn parse_date_basic_vectors_from_stock_git() {
        let utc = TimeZone::UTC;
        let cases: &[(&str, i64, i32)] = &[
            ("2005-04-07T22:13:13Z", 1_112_911_993, 0),
            ("2005-04-07 22:13:13 +0000", 1_112_911_993, 0),
            ("Thu, 7 Apr 2005 22:13:13 +0000", 1_112_911_993, 0),
            ("@1112911993 +0200", 1_112_911_993, 120),
            ("2005-04-07 22:13:13 EST", 1_112_929_993, -300),
            ("20050407T221313Z", 1_112_911_993, 0),
            ("1979-02-26 18:30:00", 288_901_800, 0),
            ("2005-04-07 22:13:13", 1_112_911_993, 0),
            ("Apr 7 2005 22:13:13", 1_112_911_993, 0),
            ("2005.04.07 22:13:13", 1_112_911_993, 0),
            ("1112911993 +0530", 1_112_911_993, 330),
            ("2005-04-07T22:13:13+05:30", 1_112_892_193, 330),
            ("22:13:13 2005-04-07", 1_112_911_993, 0),
        ];
        for &(input, seconds, offset) in cases {
            assert_eq!(
                parse_date_basic_in(input.as_bytes(), NOW, &utc),
                Some((seconds, offset)),
                "parse_date_basic({input:?})"
            );
        }

        // Everything the fuzzy pass exists to catch is a failure here.
        for rejected in ["0", "abc", "1979-02-26", "1 day ago", "20050407", ""] {
            assert_eq!(
                parse_date_basic_in(rejected.as_bytes(), NOW, &utc),
                None,
                "parse_date_basic({rejected:?})"
            );
        }
    }

    /// The same values under two DST-observing zones, captured the same way.
    ///
    /// The interesting rows are the ones that cross a DST boundary: git carries `tm_isdst` from
    /// `localtime_r(now)` into `mktime()`, so a date resolved in October keeps summer's offset
    /// even when it lands in February.
    #[test]
    fn local_timezone_vectors_from_stock_git() {
        let berlin = zone(BERLIN);
        let new_york = zone(NEW_YORK);

        // TZ=Europe/Berlin, now in CEST.
        for &(input, expected) in &[
            ("0", 1_698_400_800),
            ("1", 1_696_154_400),
            ("1 day ago", 1_698_314_400),
            ("yesterday", 1_698_314_400),
            ("midnight", 1_698_357_600), // 2023-10-27T00:00+02:00
            ("today", 1_698_357_600),
            ("noon", 1_698_400_800), // local 12:00 is not yet past, so today's
            ("1 month ago", 1_695_808_800),
            ("2005-04-07T22:13:13Z", 1_112_911_993),
            ("1979-02-26 18:30:00", 288_898_200), // CET, via parse_date_basic
            ("1979-02-26", 288_871_200),          // CEST carried over from `now`
        ] {
            assert_eq!(stock(input, NOW, &berlin), expected, "Berlin --since={input:?}");
        }

        // TZ=America/New_York, now in EDT.
        for &(input, expected) in &[
            ("0", 1_698_400_800),
            ("1", 1_696_154_400),
            ("1 day ago", 1_698_314_400),
            ("yesterday", 1_698_314_400),
            ("midnight", 1_698_379_200), // 2023-10-27T00:00-04:00
            ("today", 1_698_379_200),
            ("noon", 1_698_336_000), // local 06:00 is before noon, so yesterday's
            ("1 month ago", 1_695_808_800),
            ("2005-04-07T22:13:13Z", 1_112_911_993),
            ("1979-02-26 18:30:00", 288_919_800), // EST, via parse_date_basic
            ("1979-02-26", 288_871_200),          // EDT carried over from `now`
        ] {
            assert_eq!(stock(input, NOW, &new_york), expected, "New York --since={input:?}");
        }
    }

    /// The mirror image: `now` in winter, the answer in summer, so `tm_isdst` is `0` where the
    /// zone would have picked DST.
    #[test]
    fn winter_now_summer_answer() {
        let utc = TimeZone::UTC;
        let berlin = zone(BERLIN);
        let new_york = zone(NEW_YORK);

        // All three land on the same instant: the wall clock of `now` wins over the zone.
        for tz in [&utc, &berlin, &new_york] {
            assert_eq!(stock("2024-07-15", WINTER, tz), 1_721_044_800);
            assert_eq!(stock("6 months ago", WINTER, tz), 1_689_422_400);
        }
        // With an explicit time of day `parse_date_basic()` handles it, and each zone's real
        // summer offset applies.
        assert_eq!(stock("2024-07-15 13:00:00", WINTER, &utc), 1_721_048_400);
        assert_eq!(stock("2024-07-15 13:00:00", WINTER, &berlin), 1_721_041_200);
        assert_eq!(stock("2024-07-15 13:00:00", WINTER, &new_york), 1_721_062_800);
    }

    /// `approxidate_str()` on its own, so a tokenizer bug cannot hide behind the
    /// `parse_date_basic()` short-circuit.
    #[test]
    fn fuzzy_pass_in_isolation() {
        let utc = TimeZone::UTC;
        let fuzzy = |s: &str| approxidate_str_in(s.as_bytes(), NOW, &utc);
        assert_eq!(fuzzy("0"), (NOW, false));
        assert_eq!(fuzzy("bogus"), (NOW, true));
        assert_eq!(fuzzy("5 days ago"), (NOW - 5 * 86_400, false));
        // Absolute input still parses here, just via the fuzzy field-filling route.
        assert_eq!(fuzzy("1979-02-26"), (288_871_200, false));
    }

    /// `parse_expiry_date()`'s four reserved words, ahead of any parsing.
    #[test]
    fn expiry_keywords() {
        assert_eq!(super::parse_expiry_date("never", NOW), Some(0));
        assert_eq!(super::parse_expiry_date("false", NOW), Some(0));
        assert_eq!(super::parse_expiry_date("all", NOW), Some(i64::MAX));
        assert_eq!(super::parse_expiry_date("now", NOW), Some(i64::MAX));
        assert_eq!(super::parse_expiry_date("bogus", NOW), None);
        assert_eq!(super::parse_expiry_date("0", NOW), Some(NOW));
    }
}

