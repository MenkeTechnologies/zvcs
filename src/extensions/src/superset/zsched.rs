//! `git zsched` — daemon-hosted scheduled fleet commands (a built-in cron for
//! the tree).
//!
//! Schedules live in `$ZVCS_HOME/schedule.tsv`, one per line as
//! `id \t interval_secs \t command`. The **CLI owns every write** to that file
//! (add / rm / clear); the daemon's scheduler thread only ever *reads* it, and
//! tracks each schedule's last-fire time in memory. That split means the CLI and
//! the daemon never contend on the file — a lost in-memory timing update at
//! worst re-fires one tick later, never corrupts the schedule set. Because the
//! thread re-reads the file every tick, `git zsched add`/`rm` take effect within
//! one tick with no daemon reload.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::superset::zdaemon::{log_line, zvcs_home};

/// How often the daemon scheduler wakes to check for due schedules. Finer than
/// any useful schedule interval, so a job fires within this of its due time.
const SCHED_TICK: Duration = Duration::from_secs(5);

fn schedule_path() -> PathBuf {
    zvcs_home().join("schedule.tsv")
}

/// One schedule: a stable id, its interval, and the command to run.
#[derive(Clone)]
struct Sched {
    id: u64,
    interval: u64,
    command: String,
}

fn parse(line: &str) -> Option<Sched> {
    let mut it = line.splitn(3, '\t');
    Some(Sched {
        id: it.next()?.parse().ok()?,
        interval: it.next()?.parse().ok()?,
        command: it.next()?.to_string(),
    })
}

fn read_schedules() -> Vec<Sched> {
    match std::fs::read_to_string(schedule_path()) {
        Ok(c) => c.lines().filter_map(parse).collect(),
        Err(_) => Vec::new(),
    }
}

/// Rewrite the whole schedule file (CLI-only). Atomic via temp + rename so a
/// concurrent daemon read never sees a half-written file.
fn write_schedules(scheds: &[Sched]) -> std::io::Result<()> {
    let path = schedule_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body: String = scheds.iter().map(|s| format!("{}\t{}\t{}\n", s.id, s.interval, s.command)).collect();
    let tmp = path.with_extension("tsv.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

// ---- daemon side ---------------------------------------------------------

/// Spawn the daemon's scheduler thread. Cheap while no schedules exist (one file
/// read per tick). Called once from the daemon's foreground startup.
pub fn spawn_scheduler() {
    std::thread::spawn(|| {
        // id → when it last fired (monotonic). An id seen for the first time is
        // initialized to "now" so its first fire is one full interval later, not
        // immediately on daemon start.
        let mut last: HashMap<u64, Instant> = HashMap::new();
        loop {
            std::thread::sleep(SCHED_TICK);
            let now = Instant::now();
            let scheds = read_schedules();
            let live: std::collections::HashSet<u64> = scheds.iter().map(|s| s.id).collect();
            last.retain(|id, _| live.contains(id)); // forget removed schedules
            for s in &scheds {
                let due = match last.get(&s.id) {
                    Some(&t) => now.duration_since(t) >= Duration::from_secs(s.interval),
                    None => false, // first sight: arm it, fire next interval
                };
                if last.get(&s.id).is_none() || due {
                    if due {
                        fire(s);
                    }
                    last.insert(s.id, now);
                }
            }
        }
    });
}

/// Run one schedule's command detached, logging the fire to the daemon log.
fn fire(s: &Sched) {
    log_line(&format!("[zsched] fire #{}: {}", s.id, s.command));
    let _ = Command::new("sh")
        .arg("-c")
        .arg(&s.command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ---- CLI side ------------------------------------------------------------

pub fn zsched(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        None | Some("list") => list(),
        Some("add") => add(&args[1..]),
        Some("rm") => remove(&args[1..]),
        Some("clear") => clear(),
        Some("run") => run_now(&args[1..]),
        Some(other) => bail!("git zsched: unknown subcommand `{other}` (add|list|rm|clear|run)"),
    }
}

fn list() -> Result<ExitCode> {
    let scheds = read_schedules();
    if scheds.is_empty() {
        println!("no schedules — add one with `git zsched add <duration> -- <command>`");
        return Ok(ExitCode::SUCCESS);
    }
    for s in &scheds {
        println!("\x1b[36m#{:<4}\x1b[0m every \x1b[1m{}\x1b[0m  {}", s.id, human(s.interval), s.command);
    }
    Ok(ExitCode::SUCCESS)
}

fn add(rest: &[String]) -> Result<ExitCode> {
    let Some(dur) = rest.first() else {
        bail!("usage: git zsched add <duration> -- <command>");
    };
    let Some(interval) = crate::superset::zsince::parse_duration(dur) else {
        bail!("git zsched: bad duration `{dur}` (e.g. 30s, 5m, 1h, 1h30m)");
    };
    // Command is everything after `--`, else everything after the duration.
    let after: &[String] = match rest.iter().position(|a| a == "--") {
        Some(i) => &rest[i + 1..],
        None => &rest[1..],
    };
    let command = after.join(" ");
    if command.trim().is_empty() {
        bail!("usage: git zsched add <duration> -- <command>");
    }
    let mut scheds = read_schedules();
    let id = scheds.iter().map(|s| s.id).max().unwrap_or(0) + 1;
    scheds.push(Sched { id, interval: interval.max(1) as u64, command: command.clone() });
    write_schedules(&scheds)?;
    println!("scheduled #{id}: every {} — {command}", human(interval.max(1) as u64));
    Ok(ExitCode::SUCCESS)
}

fn remove(rest: &[String]) -> Result<ExitCode> {
    let Some(id) = rest.first().and_then(|s| s.parse::<u64>().ok()) else {
        bail!("usage: git zsched rm <id>");
    };
    let mut scheds = read_schedules();
    let before = scheds.len();
    scheds.retain(|s| s.id != id);
    if scheds.len() == before {
        bail!("git zsched: no schedule #{id}");
    }
    write_schedules(&scheds)?;
    println!("removed #{id}");
    Ok(ExitCode::SUCCESS)
}

fn clear() -> Result<ExitCode> {
    write_schedules(&[])?;
    println!("cleared all schedules");
    Ok(ExitCode::SUCCESS)
}

/// `git zsched run <id>` — fire one schedule now, synchronously, for testing.
fn run_now(rest: &[String]) -> Result<ExitCode> {
    let Some(id) = rest.first().and_then(|s| s.parse::<u64>().ok()) else {
        bail!("usage: git zsched run <id>");
    };
    let Some(s) = read_schedules().into_iter().find(|s| s.id == id) else {
        bail!("git zsched: no schedule #{id}");
    };
    println!("running #{id}: {}", s.command);
    let status = Command::new("sh").arg("-c").arg(&s.command).status()?;
    Ok(if status.success() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Compact human duration: seconds → `90s` / `5m` / `2h` / `1d` (coarsest exact unit).
fn human(secs: u64) -> String {
    for (unit, sym) in [(86_400, 'd'), (3_600, 'h'), (60, 'm')] {
        if secs % unit == 0 && secs >= unit {
            return format!("{}{}", secs / unit, sym);
        }
    }
    format!("{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        let s = parse("7\t300\tgit zpull --dirty").unwrap();
        assert_eq!(s.id, 7);
        assert_eq!(s.interval, 300);
        assert_eq!(s.command, "git zpull --dirty");
        // Command may itself contain no tabs but arbitrary spaces/flags.
        assert!(parse("bad").is_none());
        assert!(parse("1\tnan\tcmd").is_none());
    }

    #[test]
    fn human_picks_coarsest_exact_unit() {
        assert_eq!(human(90), "90s");
        assert_eq!(human(300), "5m");
        assert_eq!(human(7200), "2h");
        assert_eq!(human(86_400), "1d");
        assert_eq!(human(3_600), "1h");
    }
}
