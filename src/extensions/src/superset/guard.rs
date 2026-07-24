//! `git zguard` (alias `git zpolicy`) — declarative fleet-wide command policy.
//!
//! Rules that REFUSE or WARN on a git command *before it runs* — the veto
//! evolution of `zintercept`'s around-advice, promoted to a first-class policy
//! layer. A rule is `(action, pattern, optional predicate, message)`:
//!
//!   git zguard deny 'push*--force*'                 # refuse force-push
//!   git zguard warn 'rm*-rf*'                        # warn, but allow
//!   git zguard deny 'commit*' --when detached        # no commits on a detached HEAD
//!   git zguard deny 'commit*' --when unsigned         # require signed commits
//!   git zguard deny 'push*' --when protected          # no push while on main/master
//!   git zguard list | rm <id> | clear | test <cmd...>
//!
//! Because the shadow `git` is the sole dispatcher, the check runs for every
//! command through it — a pattern-based command policy at the binary level no VCS
//! has natively. Persisted to `$ZVCS_HOME/guards.tsv` (machine-wide); the hot path
//! is a single `stat` when no rule is set.

use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Deny,
    Warn,
}
impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Action::Deny => "deny",
            Action::Warn => "warn",
        }
    }
    fn parse(s: &str) -> Option<Action> {
        match s {
            "deny" => Some(Action::Deny),
            "warn" => Some(Action::Warn),
            _ => None,
        }
    }
}

/// A repo-state condition a rule can require, evaluated against the cwd repo.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Pred {
    Detached,
    Dirty,
    Protected,
    Unsigned,
}
impl Pred {
    fn as_str(self) -> &'static str {
        match self {
            Pred::Detached => "detached",
            Pred::Dirty => "dirty",
            Pred::Protected => "protected",
            Pred::Unsigned => "unsigned",
        }
    }
    fn parse(s: &str) -> Option<Pred> {
        match s {
            "detached" => Some(Pred::Detached),
            "dirty" => Some(Pred::Dirty),
            "protected" => Some(Pred::Protected),
            "unsigned" => Some(Pred::Unsigned),
            _ => None,
        }
    }
}

struct Guard {
    id: u64,
    action: Action,
    pred: Option<Pred>,
    pattern: String,
    message: String,
}

fn registry_path() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("guards.tsv")
}

/// Whether any guard is registered — a single `stat`, the whole cost on the
/// dispatch hot path when there are none.
pub fn active() -> bool {
    registry_path().exists()
}

fn load() -> Vec<Guard> {
    let Ok(data) = std::fs::read_to_string(registry_path()) else { return Vec::new() };
    data.lines().filter_map(parse_line).collect()
}

// `id \t action \t pred(or "-") \t pattern \t message`
fn parse_line(line: &str) -> Option<Guard> {
    let mut it = line.splitn(5, '\t');
    let id = it.next()?.parse().ok()?;
    let action = Action::parse(it.next()?)?;
    let pred_s = it.next()?;
    let pred = if pred_s == "-" { None } else { Pred::parse(pred_s) };
    let pattern = it.next()?.to_string();
    let message = it.next().unwrap_or("").to_string();
    Some(Guard { id, action, pred, pattern, message })
}

fn save(list: &[Guard]) -> Result<()> {
    let path = registry_path();
    if list.is_empty() {
        let _ = std::fs::remove_file(&path); // absent file => active() is false
        return Ok(());
    }
    let _ = std::fs::create_dir_all(crate::superset::zdaemon::zvcs_home());
    let mut out = String::new();
    for g in list {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            g.id,
            g.action.as_str(),
            g.pred.map(Pred::as_str).unwrap_or("-"),
            g.pattern.replace(['\t', '\n'], " "),
            g.message.replace(['\t', '\n'], " "),
        ));
    }
    std::fs::write(&path, out).map_err(|e| anyhow!("write guards: {e}"))
}

fn next_id(list: &[Guard]) -> u64 {
    list.iter().map(|g| g.id).max().unwrap_or(0) + 1
}

/// The dispatcher's verdict for a command.
pub enum Verdict {
    Allow,
    Deny(String),
    Warn(Vec<String>),
}

/// Evaluate the registered guards against a resolved command (`sub` + `args`).
/// Returns `Deny` on the first matching deny rule (the command is refused), else
/// `Warn` with every matching warn message, else `Allow`.
pub fn check(sub: &str, args: &[String]) -> Verdict {
    let guards = load();
    if guards.is_empty() {
        return Verdict::Allow;
    }
    let full = if args.is_empty() { sub.to_string() } else { format!("{sub} {}", args.join(" ")) };
    let mut warns = Vec::new();
    for g in &guards {
        if !crate::superset::intercepts::intercept_matches(&g.pattern, sub, &full) {
            continue;
        }
        if let Some(pred) = g.pred {
            if !predicate_holds(pred, args) {
                continue;
            }
        }
        let msg = message_for(g, &full);
        match g.action {
            Action::Deny => return Verdict::Deny(msg),
            Action::Warn => warns.push(msg),
        }
    }
    if warns.is_empty() {
        Verdict::Allow
    } else {
        Verdict::Warn(warns)
    }
}

fn message_for(g: &Guard, full: &str) -> String {
    let label = if g.action == Action::Deny { "blocked" } else { "warning" };
    if !g.message.is_empty() {
        return format!("zvcs: {label}: {}", g.message);
    }
    let cond = g.pred.map(|p| format!(" [{}]", p.as_str())).unwrap_or_default();
    format!("zvcs: {label}: `git {full}`{cond} matches policy `{}` (#{})", g.pattern, g.id)
}

/// Whether a commit's args alone settle its signing: `Some(true)` = signed
/// (`-S`, `-S<keyid>` attached, `--gpg-sign`, `--gpg-sign=<keyid>`), `Some(false)`
/// = explicitly `--no-gpg-sign`, `None` = not on the command line (config decides).
fn signed_from_args(args: &[String]) -> Option<bool> {
    if args.iter().any(|a| a == "--no-gpg-sign") {
        return Some(false);
    }
    if args
        .iter()
        .any(|a| a.starts_with("-S") || a == "--gpg-sign" || a.starts_with("--gpg-sign="))
    {
        return Some(true);
    }
    None
}

/// Evaluate a predicate against the repo at the cwd. Missing repo → false (can't
/// prove the condition, so never block on it).
fn predicate_holds(pred: Pred, args: &[String]) -> bool {
    if pred == Pred::Unsigned {
        return match signed_from_args(args) {
            Some(signed) => !signed,
            // No sign flag on the command line → the config decides.
            None => {
                let Ok(repo) = gix::discover(".") else { return false };
                !repo.config_snapshot().boolean("commit.gpgsign").unwrap_or(false)
            }
        };
    }
    let Ok(repo) = gix::discover(".") else { return false };
    match pred {
        Pred::Detached => repo.head_name().ok().flatten().is_none(),
        Pred::Dirty => repo.is_dirty().unwrap_or(false),
        Pred::Protected => repo
            .head_name()
            .ok()
            .flatten()
            .map(|n| {
                let s = n.shorten().to_string();
                s == "main" || s == "master"
            })
            .unwrap_or(false),
        Pred::Unsigned => unreachable!(),
    }
}

pub fn zguard(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => list(),
        "clear" => {
            save(&[])?;
            println!("cleared all guards");
            Ok(ExitCode::SUCCESS)
        }
        "rm" | "remove" => {
            let id: u64 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: git zguard rm <id>"))?;
            let mut guards = load();
            let before = guards.len();
            guards.retain(|g| g.id != id);
            save(&guards)?;
            if guards.len() < before {
                println!("removed guard #{id}");
            } else {
                println!("no guard #{id}");
            }
            Ok(ExitCode::SUCCESS)
        }
        "deny" => add(Action::Deny, &args[1..]),
        "warn" => add(Action::Warn, &args[1..]),
        "test" => test(&args[1..]),
        other => bail!("unknown zguard subcommand `{other}` (deny|warn|list|rm|clear|test)"),
    }
}

fn add(action: Action, rest: &[String]) -> Result<ExitCode> {
    let mut pred = None;
    let mut message = String::new();
    let mut pattern = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--when" => {
                pred = Some(
                    rest.get(i + 1)
                        .and_then(|s| Pred::parse(s))
                        .ok_or_else(|| anyhow!("--when needs one of: detached|dirty|protected|unsigned"))?,
                );
                i += 2;
            }
            "-m" | "--message" => {
                message = rest.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            p if pattern.is_none() => {
                pattern = Some(p.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let pattern = pattern
        .ok_or_else(|| anyhow!("usage: git zguard {} <pattern> [--when <pred>] [-m <msg>]", action.as_str()))?;
    let mut guards = load();
    let id = next_id(&guards);
    guards.push(Guard { id, action, pred, pattern: pattern.clone(), message });
    save(&guards)?;
    println!(
        "guard #{id}: {} `{pattern}`{}",
        action.as_str(),
        pred.map(|p| format!(" when {}", p.as_str())).unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

fn list() -> Result<ExitCode> {
    let guards = load();
    if guards.is_empty() {
        println!("no guards");
        return Ok(ExitCode::SUCCESS);
    }
    for g in &guards {
        println!(
            "#{}  {:<4} {}{}{}",
            g.id,
            g.action.as_str(),
            g.pattern,
            g.pred.map(|p| format!("  when {}", p.as_str())).unwrap_or_default(),
            if g.message.is_empty() { String::new() } else { format!("  — {}", g.message) },
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn test(cmd: &[String]) -> Result<ExitCode> {
    if cmd.is_empty() {
        bail!("usage: git zguard test <command...>");
    }
    match check(&cmd[0], &cmd[1..]) {
        Verdict::Deny(m) => {
            println!("DENY  {m}");
            Ok(ExitCode::from(1))
        }
        Verdict::Warn(ms) => {
            for m in ms {
                println!("WARN  {m}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Verdict::Allow => {
            println!("allow");
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(action: Action, pred: Option<Pred>, pattern: &str) -> Guard {
        Guard { id: 1, action, pred, pattern: pattern.into(), message: String::new() }
    }

    #[test]
    fn glob_deny_matches_command_line() {
        // A force-push deny matches the full command line, not just the verb.
        let guards = [g(Action::Deny, None, "push*--force*")];
        assert!(matches_any(&guards, "push", &["--force".into(), "origin".into()]));
        assert!(!matches_any(&guards, "push", &["origin".into(), "main".into()]));
        // rm -rf warn.
        let guards = [g(Action::Warn, None, "rm*-rf*")];
        assert!(matches_any(&guards, "rm", &["-rf".into(), "x".into()]));
    }

    // Test helper mirroring `check`'s match step without touching a real repo
    // (predicate-less rules only).
    fn matches_any(guards: &[Guard], sub: &str, args: &[String]) -> bool {
        let full = if args.is_empty() { sub.to_string() } else { format!("{sub} {}", args.join(" ")) };
        guards.iter().any(|g| {
            g.pred.is_none() && crate::superset::intercepts::intercept_matches(&g.pattern, sub, &full)
        })
    }

    #[test]
    fn signed_detection_handles_attached_and_negation() {
        let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(signed_from_args(&a(&["-S"])), Some(true));
        assert_eq!(signed_from_args(&a(&["-Skeyid"])), Some(true)); // attached form (was a false-positive)
        assert_eq!(signed_from_args(&a(&["--gpg-sign"])), Some(true));
        assert_eq!(signed_from_args(&a(&["--gpg-sign=ABC"])), Some(true));
        assert_eq!(signed_from_args(&a(&["--no-gpg-sign"])), Some(false)); // explicit unsigned
        assert_eq!(signed_from_args(&a(&["-m", "msg"])), None); // config decides
    }

    #[test]
    fn tsv_roundtrips_action_pred_pattern_message() {
        let line = "3\tdeny\tunsigned\tcommit*\tsign your commits";
        let p = parse_line(line).expect("parse");
        assert_eq!(p.id, 3);
        assert_eq!(p.action, Action::Deny);
        assert_eq!(p.pred, Some(Pred::Unsigned));
        assert_eq!(p.pattern, "commit*");
        assert_eq!(p.message, "sign your commits");
        // A predicate-less rule uses "-".
        let p2 = parse_line("1\twarn\t-\trm*-rf*\t").expect("parse");
        assert_eq!(p2.pred, None);
        assert_eq!(p2.action, Action::Warn);
    }
}
