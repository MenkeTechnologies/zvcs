//! Relative date rendering, ported from git's `show_date_relative` (date.c).
//!
//! gitoxide's `gix-date` can PARSE relative dates ("2 minutes ago" → a
//! timestamp) but has no relative/human FORMAT direction, so this is the shared
//! renderer for `--date=relative` and the `%ar`/`%cr` pretty atoms. Every
//! command (log, shortlog, for-each-ref, blame) routes here so the output is
//! identical across the binary.

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
