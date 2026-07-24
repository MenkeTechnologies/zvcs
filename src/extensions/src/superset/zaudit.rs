//! `git zaudit` — a queryable audit trail over the fleet command log.
//!
//! Where `zcommands` is the *live* feed, `zaudit` is the *historical, accountable*
//! side of the same `$ZVCS_HOME/commands.log`: filter by agent (the parent pid
//! that ran a command), by repo, or by command; restrict to state-changing
//! commands; or aggregate a `--summary` of who ran what. In a fleet of concurrent
//! agents this answers "which agent ran `push --force` to which repo, and when".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";

/// Subcommands that change repository state — what `--mutating` keeps.
const MUTATING: &[&str] = &[
    "commit", "push", "reset", "rebase", "merge", "checkout", "switch", "pull", "fetch", "tag",
    "branch", "rm", "mv", "stash", "cherry-pick", "revert", "clean", "gc", "prune", "am", "apply",
    "restore", "add", "stage", "zcommit", "zpush", "zreset", "zabort", "zcommitall", "zpushall",
    "zsync", "zbump", "zattach", "zcheckout", "zclean", "ztagall",
];

fn log_path() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("commands.log")
}

struct Rec {
    ts: i64,
    pid: u32,
    ppid: i32,
    cwd: String,
    argv: String,
}

fn parse(line: &str) -> Option<Rec> {
    let mut it = line.splitn(5, '\t');
    Some(Rec {
        ts: it.next()?.parse().ok()?,
        pid: it.next()?.parse().ok()?,
        ppid: it.next()?.parse().ok()?,
        cwd: it.next()?.to_string(),
        argv: it.next()?.to_string(),
    })
}

fn subcommand(argv: &str) -> &str {
    argv.split_whitespace().next().unwrap_or("")
}

struct Opts {
    agent: Option<i32>,
    repo: Option<String>,
    cmd: Option<String>,
    mutating: bool,
    limit: usize,
    summary: bool,
    json: bool,
}

fn opts(args: &[String]) -> Opts {
    let mut o = Opts { agent: None, repo: None, cmd: None, mutating: false, limit: 40, summary: false, json: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agent" | "-a" => o.agent = it.next().and_then(|v| v.parse().ok()),
            "--repo" | "-r" => o.repo = it.next().cloned(),
            "--cmd" | "-c" => o.cmd = it.next().cloned(),
            "--mutating" | "-m" => o.mutating = true,
            "--summary" | "-s" => o.summary = true,
            "--json" => o.json = true,
            "-n" | "--lines" => o.limit = it.next().and_then(|v| v.parse().ok()).unwrap_or(o.limit),
            _ => {}
        }
    }
    o
}

pub fn zaudit(args: &[String]) -> Result<ExitCode> {
    let o = opts(args);
    let Ok(data) = std::fs::read_to_string(log_path()) else {
        eprintln!("zvcs: no command log yet — run `git zcommands` to enable logging");
        return Ok(ExitCode::SUCCESS);
    };
    let recs: Vec<Rec> = data
        .lines()
        .filter_map(parse)
        .filter(|r| o.agent.map(|a| r.ppid == a).unwrap_or(true))
        .filter(|r| o.repo.as_deref().map(|s| r.cwd.contains(s)).unwrap_or(true))
        .filter(|r| o.cmd.as_deref().map(|s| r.argv.contains(s)).unwrap_or(true))
        .filter(|r| !o.mutating || MUTATING.contains(&subcommand(&r.argv)))
        .collect();

    if o.summary {
        return summary(&recs, o.json);
    }

    let start = recs.len().saturating_sub(o.limit);
    for r in &recs[start..] {
        emit(r, o.json);
    }
    Ok(ExitCode::SUCCESS)
}

/// Per-agent and per-command tallies over the matching records.
fn summary(recs: &[Rec], json: bool) -> Result<ExitCode> {
    let mut by_agent: BTreeMap<i32, usize> = BTreeMap::new();
    let mut by_cmd: BTreeMap<String, usize> = BTreeMap::new();
    for r in recs {
        *by_agent.entry(r.ppid).or_default() += 1;
        *by_cmd.entry(subcommand(&r.argv).to_string()).or_default() += 1;
    }
    if json {
        let obj = serde_json::json!({
            "total": recs.len(),
            "by_agent": by_agent.iter().map(|(k, v)| (k.to_string(), v)).collect::<BTreeMap<_, _>>(),
            "by_command": by_cmd,
        });
        println!("{obj}");
        return Ok(ExitCode::SUCCESS);
    }
    println!("{BOLD}{} commands audited{RESET}", recs.len());
    println!("\n{BOLD}by agent (ppid){RESET}");
    for (ppid, n) in rank(&by_agent) {
        println!("  {CYAN}{ppid:>8}{RESET}  {n}");
    }
    println!("\n{BOLD}by command{RESET}");
    for (cmd, n) in rank(&by_cmd) {
        println!("  {GREEN}{cmd:<16}{RESET}  {n}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Entries ranked by count, most first.
fn rank<K: Clone>(m: &BTreeMap<K, usize>) -> Vec<(K, usize)> {
    let mut v: Vec<(K, usize)> = m.iter().map(|(k, n)| (k.clone(), *n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

fn emit(r: &Rec, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({"ts": r.ts, "pid": r.pid, "ppid": r.ppid, "cwd": r.cwd, "argv": r.argv})
        );
        return;
    }
    println!(
        "{DIM}{} {:>7}\u{2190}{:<7}{RESET}  {CYAN}{:<18}{RESET}  {GREEN}git{RESET} {BOLD}{}{RESET}",
        hms(r.ts),
        r.pid,
        r.ppid,
        basename(&r.cwd),
        r.argv,
    );
}

fn basename(cwd: &str) -> String {
    Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| cwd.to_string())
}

fn hms(ts: i64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "--:--:--".to_string();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_mutating() {
        let r = parse("1700000000\t42\t900\t/x/repo\tpush --force origin main").unwrap();
        assert_eq!(r.ppid, 900);
        assert_eq!(subcommand(&r.argv), "push");
        assert!(MUTATING.contains(&subcommand(&r.argv)));
        assert!(!MUTATING.contains(&subcommand("status --short")));
        assert!(parse("junk").is_none());
    }

    #[test]
    fn summary_tallies_by_agent_and_command() {
        let recs = vec![
            parse("1\t10\t900\t/a\tpush origin").unwrap(),
            parse("2\t11\t900\t/b\tcommit -m x").unwrap(),
            parse("3\t12\t901\t/a\tpush origin").unwrap(),
        ];
        let mut by_agent: BTreeMap<i32, usize> = BTreeMap::new();
        let mut by_cmd: BTreeMap<String, usize> = BTreeMap::new();
        for r in &recs {
            *by_agent.entry(r.ppid).or_default() += 1;
            *by_cmd.entry(subcommand(&r.argv).to_string()).or_default() += 1;
        }
        assert_eq!(by_agent[&900], 2);
        assert_eq!(by_agent[&901], 1);
        assert_eq!(by_cmd["push"], 2);
        assert_eq!(by_cmd["commit"], 1);
    }
}
