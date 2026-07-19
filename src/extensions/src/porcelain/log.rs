use anyhow::{anyhow, bail, Result};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// `git log` — commit history reachable from a starting revision (default `HEAD`),
/// newest first, in the `medium` format that stock `git log` prints.
///
/// Ported invocation forms (the ones the meta workflow leans on):
///   * `git log`                        → full history from `HEAD`
///   * `git log <rev>`                  → history from a resolved revision
///   * `git log -n N` / `--max-count=N` → limit to the first `N` commits
///   * `git log -N`                     → same shorthand git accepts (`git log -5`)
///   * `git log --oneline`              → `<abbrev> <subject>` per line
///
/// Anything else (`--graph`, `-p`, `--stat`, `--pretty`, date filters, pathspec
/// filtering, multiple revisions) is rejected with a precise message rather than
/// silently producing wrong output.
pub fn log(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut max_count: Option<usize> = None;
    let mut oneline = false;
    let mut rev: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            bail!("pathspec filtering is not ported");
        } else if a == "-n" || a == "--max-count" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
            max_count = Some(parse_count(a, v)?);
        } else if let Some(v) = a.strip_prefix("--max-count=") {
            max_count = Some(parse_count("--max-count", v)?);
        } else if a == "--oneline" {
            oneline = true;
        } else if a.starts_with('-') {
            let body = &a[1..];
            if let Some(num) = body.strip_prefix('n') {
                // `-nN` shorthand (e.g. `-n5`).
                max_count = Some(parse_count("-n", num)?);
            } else if !body.is_empty() && body.bytes().all(|c| c.is_ascii_digit()) {
                // `-N` shorthand (e.g. `-5`).
                max_count = Some(parse_count(a, body)?);
            } else {
                bail!(
                    "unsupported flag {a:?}; only plain history, -n/--max-count, and --oneline are ported"
                );
            }
        } else if rev.is_some() {
            bail!("multiple revisions are not ported");
        } else {
            rev = Some(a.clone());
        }
        i += 1;
    }

    // Resolve the starting tip. A bare `HEAD` may be unborn (a fresh branch with
    // no commits), which stock git reports as a fatal error, not empty output.
    let tip = match &rev {
        Some(spec) => repo.rev_parse_single(spec.as_str())?.detach(),
        None => match repo.head()?.try_peel_to_id()? {
            Some(id) => id.detach(),
            None => bail!("your current branch does not have any commits yet"),
        },
    };

    // Newest-first by commit time, matching the default `git log` ordering.
    let walk = repo
        .rev_walk([tip])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()?;

    let limit = max_count.unwrap_or(usize::MAX);
    let mut entries: Vec<String> = Vec::new();

    for info in walk.take(limit) {
        let commit = info?.object()?;

        if oneline {
            let short = commit.id().shorten()?;
            let raw = commit.message_raw()?;
            let subject = raw.lines().next().unwrap_or_default().to_str_lossy();
            entries.push(format!("{short} {subject}"));
            continue;
        }

        let author = commit.author()?;
        let time = author.time()?;
        let mut entry = format!("commit {}\n", commit.id());

        // A merge commit lists its parents right after the `commit` line.
        let parents: Vec<_> = commit.parent_ids().collect();
        if parents.len() > 1 {
            let mut line = String::from("Merge:");
            for pid in &parents {
                line.push(' ');
                line.push_str(&pid.shorten()?.to_string());
            }
            entry.push_str(&line);
            entry.push('\n');
        }

        entry.push_str(&format!("Author: {} <{}>\n", author.name.to_str_lossy(), author.email.to_str_lossy()));
        entry.push_str(&format!("Date:   {}\n\n", format_git_date(time.seconds, time.offset)));

        // The message is indented four spaces; blank lines stay blank. `.lines()`
        // drops the trailing newline(s) exactly like git's medium format does.
        let raw = commit.message_raw()?;
        let body: Vec<String> = raw
            .lines()
            .map(|l| {
                if l.is_empty() {
                    String::new()
                } else {
                    format!("    {}", l.to_str_lossy())
                }
            })
            .collect();
        entry.push_str(&body.join("\n"));

        entries.push(entry);
    }

    if !entries.is_empty() {
        let sep = if oneline { "\n" } else { "\n\n" };
        println!("{}", entries.join(sep));
    }

    Ok(ExitCode::SUCCESS)
}

/// Parse a positive commit count for `-n`/`--max-count`, with a git-shaped error.
fn parse_count(flag: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| anyhow!("invalid number {value:?} for {flag}"))
}

/// Format a commit time exactly like stock `git log`'s default (`DATE_NORMAL`)
/// mode: `Www Mmm <sp-padded-day> HH:MM:SS YYYY +ZZZZ`, in the commit's own
/// timezone offset. Done by hand because gix's exported `DEFAULT` format uses an
/// unpadded day (`%-d`) whereas git space-pads it (`%e`); nothing else in the
/// crate lets us construct a custom format string.
fn format_git_date(seconds: i64, offset: i32) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // Shift into the commit's local wall-clock time, then split into whole days
    // (since the Unix epoch) and the seconds within the day. `div_euclid` /
    // `rem_euclid` keep the split correct for pre-1970 (negative) timestamps.
    let local = seconds + offset as i64;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // 1970-01-01 (day 0) was a Thursday, index 4 with Sunday = 0.
    let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);

    let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    let (off_h, off_m) = (off / 3600, (off % 3600) / 60);

    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {} {}{:02}{:02}",
        WEEKDAYS[weekday],
        MONTHS[(month - 1) as usize],
        day,
        hour,
        min,
        sec,
        year,
        sign,
        off_h,
        off_m,
    )
}

/// Convert a day count since the Unix epoch into a civil `(year, month, day)`,
/// month and day 1-based. Howard Hinnant's `civil_from_days` algorithm, which is
/// exact for the whole representable range and needs no calendar tables.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month as u32, day)
}
