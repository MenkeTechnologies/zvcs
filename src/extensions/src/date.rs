//! Date arguments and relative date rendering, both ported from git's `date.c`.
//!
//! Two directions, one module:
//!
//! * **Reading** a `--since`/`--until`/`--before`/`--after`/`--expire`/`--mtime` value is
//!   [`approxidate()`] and friends, thin wrappers that supply git's `get_time()` to the port in
//!   [`gix::date::parse::approxidate_careful`]. Every verb goes through them, so a date argument
//!   means the same thing everywhere in the binary. Do not reach for
//!   [`gix::date::parse`][gix::date::parse()] for a command-line date: it reads a bare integer as
//!   a unix timestamp, and git does not below `100000000` — `--since=0` means *now* to git.
//! * **Writing** a relative date is [`show_date_relative()`]. `gix-date` parses relative dates
//!   but has no format direction, so this is the shared renderer for `--date=relative` and the
//!   `%ar`/`%cr` pretty atoms.

/// The "now" reference git resolves in `get_time()`: `GIT_TEST_DATE_NOW`
/// (epoch seconds) when set — so relative output is reproducible under test —
/// otherwise the wall clock.
pub fn now_seconds() -> i64 {
    if let Ok(v) = std::env::var("GIT_TEST_DATE_NOW") {
        if let Ok(n) = v.trim().parse::<i64>() {
            return n;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// git's `approxidate()` (date.h:69): resolve a command-line date to epoch seconds.
///
/// This is the only date-argument parser in the binary. It never fails — anything git cannot read
/// resolves to the current time, which is what makes `--since=garbage` a no-op limiter rather
/// than an error.
///
/// Use [`approxidate_careful()`] instead when the caller has to distinguish "git could not read
/// this" from "git read this as now".
pub fn approxidate(value: &str) -> i64 {
    gix::date::parse::approxidate(value, now_seconds())
}

/// git's `approxidate_careful()` (date.c:1413): [`approxidate()`] plus the `error_ret` flag,
/// `true` when nothing in `value` looked like a date at all.
pub fn approxidate_careful(value: &str) -> (i64, bool) {
    gix::date::parse::approxidate_careful(value, now_seconds())
}

/// git's `parse_date_basic()` (date.c:879): the strict half, which is what `parse_date()` and
/// therefore `GIT_AUTHOR_DATE`/`--date=` use before anything falls back to approxidate.
///
/// `None` is git's `-1` return.
pub fn parse_date_basic(value: &str) -> Option<gix::date::Time> {
    gix::date::parse::parse_date_basic(value, now_seconds())
}

/// git's `parse_expiry_date()` (date.c:957): [`approxidate_careful()`] with four words taken over
/// first — `never`/`false` expire nothing, `all`/`now` expire everything.
///
/// `None` is git's non-zero return, which every caller reports as
/// `fatal: invalid timestamp '<value>' given to '--<option>'`.
pub fn parse_expiry_date(value: &str) -> Option<i64> {
    gix::date::parse::parse_expiry_date(value, now_seconds())
}

/// git's `Q_(...)` pluralization: `"1 second ago"` vs `"N seconds ago"`.
fn ago(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("{n} {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

/// Port of `show_date_relative()` (date.c): render `time` relative to `now`,
/// byte-for-byte as git does — the same rounding thresholds (90s→minutes,
/// 90m→hours, 36h→days, 14d→weeks, 70w→months, 12mo→years) and the
/// "N years, M months ago" form under five years. `now < time` is "in the future".
pub fn show_date_relative(time: i64, now: i64) -> String {
    if now < time {
        return "in the future".to_string();
    }
    let mut diff = now - time;
    if diff < 90 {
        return ago(diff, "second");
    }
    // Turn it into minutes.
    diff = (diff + 30) / 60;
    if diff < 90 {
        return ago(diff, "minute");
    }
    // Turn it into hours.
    diff = (diff + 30) / 60;
    if diff < 36 {
        return ago(diff, "hour");
    }
    // Number of days from here on.
    diff = (diff + 12) / 24;
    if diff < 14 {
        return ago(diff, "day");
    }
    // Weeks for the past 10 weeks or so.
    if diff < 70 {
        return ago((diff + 3) / 7, "week");
    }
    // Months for the past 12 months or so.
    if diff < 365 {
        return ago((diff + 15) / 30, "month");
    }
    // Years and months for the past 5 years or so.
    if diff < 1825 {
        let totalmonths = (diff * 12 * 2 + 365) / (365 * 2);
        let years = totalmonths / 12;
        let months = totalmonths % 12;
        if months != 0 {
            let y = if years == 1 {
                format!("{years} year")
            } else {
                format!("{years} years")
            };
            return if months == 1 {
                format!("{y}, {months} month ago")
            } else {
                format!("{y}, {months} months ago")
            };
        }
        return ago(years, "year");
    }
    // Otherwise, just years.
    ago((diff + 183) / 365, "year")
}

#[cfg(test)]
mod tests {
    use super::show_date_relative;

    // Thresholds verified against git 2.55.0 via `GIT_TEST_DATE_NOW`.
    #[test]
    fn matches_git_thresholds() {
        let t = 1_000_000_000;
        assert_eq!(show_date_relative(t, t), "0 seconds ago");
        assert_eq!(show_date_relative(t, t + 1), "1 second ago");
        assert_eq!(show_date_relative(t, t + 89), "89 seconds ago");
        assert_eq!(show_date_relative(t, t + 90), "2 minutes ago"); // (90+30)/60=2
        assert_eq!(show_date_relative(t, t + 3600), "60 minutes ago");
        assert_eq!(show_date_relative(t, t + 86_400), "24 hours ago");
        assert_eq!(show_date_relative(t, t + 100_000_000), "3 years, 2 months ago");
        assert_eq!(show_date_relative(t, t - 5), "in the future");
    }
}
