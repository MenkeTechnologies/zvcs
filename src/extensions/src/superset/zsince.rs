//! `git zsince <duration|snapshot> [--kind K] [--repo R]` — everything that
//! happened across the whole tree since a point in time.
//!
//! A duration (`90s`, `45m`, `2d`, `1h30m`, or a bare number of seconds) counts
//! back from now; any other token is treated as a snapshot name and its creation
//! time is the baseline. Reads the `zevents` feed, scoped to the window — the
//! "what did the fleet do since X" delta that `zlog`'s full timeline doesn't give.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::ExitCode;

pub fn zsince(args: &[String]) -> Result<ExitCode> {
    let (json, args) = crate::superset::query::json_flag(args);
    let spec = args
        .first()
        .ok_or_else(|| anyhow!("usage: git zsince <duration|snapshot> [--kind K] [--repo R] [--json]"))?;

    let mut kind = None;
    let mut repo = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--kind" | "-k" => {
                kind = args.get(i + 1).cloned();
                i += 2;
            }
            "--repo" | "-r" => {
                repo = args.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let conn = crate::db::open_rw()?;
    let cutoff = match parse_duration(spec) {
        Some(secs) => now_secs() - secs,
        None => crate::db::snapshot_created_at(&conn, spec)?
            .ok_or_else(|| anyhow!("`{spec}` is neither a duration (90s/45m/2d/1h30m) nor a known snapshot"))?,
    };

    let events = crate::db::events_after_ts(&conn, cutoff, kind.as_deref(), repo.as_deref())?;
    if json {
        for e in &events {
            println!(
                "{}",
                serde_json::json!({
                    "id": e.id, "ts": e.ts, "kind": e.kind,
                    "repo": e.workdir.clone().or_else(|| e.git_dir.clone()),
                    "detail": e.detail, "before": e.sha_before, "after": e.sha_after,
                })
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    if events.is_empty() {
        println!("nothing since {spec}");
        return Ok(ExitCode::SUCCESS);
    }
    for e in &events {
        let detail = match e.kind.as_str() {
            "commit" => format!(
                "{}→{}",
                short(e.sha_before.as_deref()),
                short(e.sha_after.as_deref())
            ),
            _ => e.detail.clone().unwrap_or_default(),
        };
        println!("{}  {:<9} {:<24} {detail}", hms(e.ts), e.kind, repo_label(e));
    }
    println!("\n{} event(s) since {spec}", events.len());
    Ok(ExitCode::SUCCESS)
}

fn short(sha: Option<&str>) -> String {
    match sha {
        Some(s) if s.len() >= 10 => s[..10].to_string(),
        Some(s) => s.to_string(),
        None => "·".to_string(),
    }
}

fn repo_label(e: &crate::db::EventRow) -> String {
    if let Some(w) = &e.workdir {
        if let Some(n) = Path::new(w).file_name() {
            return n.to_string_lossy().into_owned();
        }
    }
    e.git_dir
        .as_deref()
        .and_then(|g| Path::new(g).parent().and_then(|p| p.file_name()))
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".into())
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse `90s` / `45m` / `2d` / `1h30m` / bare-seconds into seconds. `None` if the
/// token isn't a duration (then the caller treats it as a snapshot name).
pub(crate) fn parse_duration(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let mut total: i64 = 0;
    let mut num: i64 = 0;
    let mut saw_digit = false;
    let mut saw_unit = false;
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            num = num.checked_mul(10)?.checked_add(d as i64)?;
            saw_digit = true;
        } else {
            let mult = match c {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => return None,
            };
            total += num * mult;
            num = 0;
            saw_digit = false;
            saw_unit = true;
        }
    }
    // A trailing bare number (no unit) is seconds; a pure number is seconds too.
    if saw_digit {
        total += num;
    } else if !saw_unit {
        return None;
    }
    Some(total)
}

/// Local-time `HH:MM:SS` for an epoch-seconds timestamp.
fn hms(ts: i64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "--:--:--".to_string();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}
