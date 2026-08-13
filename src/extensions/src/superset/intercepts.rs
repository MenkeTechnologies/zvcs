//! `git zintercept` — AOP command interception, ported from zshrs
//! (`src/extensions/intercepts.rs`). Register before / after / around advice
//! against a git subcommand pattern; the advice runs whenever a matching command
//! is dispatched.
//!
//! zshrs holds intercepts in memory in its long-running shell. zvcs's `git` is a
//! fresh process per command, so the registry is persisted to
//! `$ZVCS_HOME/intercepts.tsv` and loaded at dispatch. When the file is absent
//! the hot path is a single `stat` (the `active` check in [`crate::dispatch`]).
//! Advice `code` is a shell command run via `sh -c`, with the intercepted
//! command exposed through `INTERCEPT_NAME` / `INTERCEPT_ARGS` / `INTERCEPT_CMD`
//! (and `INTERCEPT_STATUS` / `INTERCEPT_MS` / `INTERCEPT_US` for after-advice).
//!
//! Usage:
//!   git zintercept before <pattern> -- <cmd>   run <cmd> before a match
//!   git zintercept after  <pattern> -- <cmd>   run <cmd> after (status/timing set)
//!   git zintercept around <pattern> -- <cmd>   replace; <cmd> runs $INTERCEPT_CMD to proceed
//!   git zintercept list | remove <id> | clear
//!
//!   <pattern> matches the git subcommand: `commit`, `push`, `commit *`, `*`/`all`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Instant;

use anyhow::{bail, Result};

/// AOP advice type — before, after, or around. Ported from zshrs `AdviceKind`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AdviceKind {
    /// Run code before the command executes.
    Before,
    /// Run code after the command executes. INTERCEPT_STATUS/MS available.
    After,
    /// Wrap the command. Code runs $INTERCEPT_CMD to run the original.
    Around,
}

impl AdviceKind {
    fn as_str(self) -> &'static str {
        match self {
            AdviceKind::Before => "before",
            AdviceKind::After => "after",
            AdviceKind::Around => "around",
        }
    }
    fn parse(s: &str) -> Option<AdviceKind> {
        match s {
            "before" => Some(AdviceKind::Before),
            "after" => Some(AdviceKind::After),
            "around" => Some(AdviceKind::Around),
            _ => None,
        }
    }
}

/// One AOP intercept registered against a command pattern. Ported from zshrs
/// `Intercept`.
#[derive(Debug, Clone)]
pub struct Intercept {
    /// Pattern matched against the git subcommand. Glob: "commit *", "*", "all".
    pub pattern: String,
    pub kind: AdviceKind,
    /// Shell code run as advice.
    pub code: String,
    /// Unique id for removal.
    pub id: u32,
}

/// Match an intercept pattern against a command name or full command string.
/// Ported verbatim (semantics) from zshrs `intercept_matches`: exact match, glob
/// ("commit *", "_*", "*"), or "all". zvcs uses its `regex` dep for the glob step
/// (no `glob` crate); `*`→`.*`, `?`→`.`, everything else escaped.
pub(crate) fn intercept_matches(pattern: &str, cmd_name: &str, full_cmd: &str) -> bool {
    if pattern == "*" || pattern == "all" {
        return true;
    }
    if pattern == cmd_name {
        return true;
    }
    if pattern.contains('*') || pattern.contains('?') {
        if let Some(re) = glob_to_regex(pattern) {
            return re.is_match(cmd_name) || re.is_match(full_cmd);
        }
    }
    false
}

/// Compile a `*`/`?` glob to an anchored regex.
fn glob_to_regex(pat: &str) -> Option<regex::Regex> {
    let mut re = String::from("^");
    for ch in pat.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).ok()
}

// ---------------------------------------------------------------------------
// Registry — persisted to `$ZVCS_HOME/intercepts.tsv`, one `id\tkind\tpattern\tcode`
// per line (zvcs's per-process adaptation of zshrs's in-memory `exec.intercepts`).
// ---------------------------------------------------------------------------

fn registry_path() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("intercepts.tsv")
}

/// Whether any intercept is registered — a single `stat`, the whole cost on the
/// dispatch hot path when there are none.
pub fn active() -> bool {
    registry_path().exists()
}

fn load() -> Vec<Intercept> {
    let Ok(data) = std::fs::read_to_string(registry_path()) else { return Vec::new() };
    data.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Intercept> {
    let mut it = line.splitn(4, '\t');
    let id = it.next()?.parse().ok()?;
    let kind = AdviceKind::parse(it.next()?)?;
    let pattern = it.next()?.to_string();
    let code = it.next()?.to_string();
    Some(Intercept { pattern, kind, code, id })
}

fn save(list: &[Intercept]) -> Result<()> {
    let path = registry_path();
    if list.is_empty() {
        let _ = std::fs::remove_file(&path); // absent file => `active()` is false
        return Ok(());
    }
    let _ = std::fs::create_dir_all(crate::superset::zdaemon::zvcs_home());
    let mut out = String::new();
    for i in list {
        let code = i.code.replace(['\t', '\n'], " ");
        out.push_str(&format!("{}\t{}\t{}\t{}\n", i.id, i.kind.as_str(), i.pattern, code));
    }
    let mut f = std::fs::File::create(&path)?;
    f.write_all(out.as_bytes())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatch hook — before / around / after orchestration. Ported from zshrs
// `run_intercepts`, adapted to per-process execution (advice is a shell command;
// the original runs in a child so after-advice sees its exit status).
// ---------------------------------------------------------------------------

/// Called from [`crate::dispatch::run`] for every command. Returns `Some(result)`
/// when interception fully handled the command (an around, or before+after
/// wrapping), or `None` to let normal in-process dispatch proceed (no match, or
/// before-only advice — which has already run by the time this returns `None`).
pub fn maybe_intercept(sub: &str, args: &[String]) -> Option<Result<ExitCode>> {
    // Loop guard: a command re-invoked from inside advice (or the around-proceed)
    // carries ZVCS_INTERCEPTED and is never re-intercepted.
    if std::env::var_os("ZVCS_INTERCEPTED").is_some() || !active() {
        return None;
    }
    let full_cmd_args: Vec<&str> = std::iter::once(sub).chain(args.iter().map(|s| s.as_str())).collect();
    let full = full_cmd_args.join(" ");
    let matching: Vec<Intercept> =
        load().into_iter().filter(|i| intercept_matches(&i.pattern, sub, &full)).collect();
    if matching.is_empty() {
        return None;
    }

    // Base environment exposed to every advice process.
    let intercept_cmd = format!("git {full}");
    let base_env: Vec<(&str, String)> = vec![
        ("INTERCEPT_NAME", sub.to_string()),
        ("INTERCEPT_ARGS", args.join(" ")),
        ("INTERCEPT_CMD", intercept_cmd),
    ];

    // Before advice.
    for a in matching.iter().filter(|i| i.kind == AdviceKind::Before) {
        run_advice(&a.code, &base_env);
    }

    // Around advice — first match wins; it replaces the command and runs
    // $INTERCEPT_CMD itself to proceed (that child skips re-interception).
    if let Some(a) = matching.iter().find(|i| i.kind == AdviceKind::Around) {
        let status = run_advice(&a.code, &base_env);
        return Some(Ok(ExitCode::from(status as u8)));
    }

    let has_after = matching.iter().any(|i| i.kind == AdviceKind::After);
    if !has_after {
        // Only before advice ran; let normal dispatch execute the original.
        return None;
    }

    // After advice: run the original in a child (so we capture its status), then
    // fire after-advice with the status and timing set.
    let t0 = Instant::now();
    let status = run_original(sub, args);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut after_env = base_env;
    after_env.push(("INTERCEPT_STATUS", status.to_string()));
    after_env.push(("INTERCEPT_MS", format!("{ms:.3}")));
    after_env.push(("INTERCEPT_US", format!("{:.0}", ms * 1000.0)));
    for a in matching.iter().filter(|i| i.kind == AdviceKind::After) {
        run_advice(&a.code, &after_env);
    }
    Some(Ok(ExitCode::from(status as u8)))
}

/// Run advice as a shell command, inheriting stdio, with the INTERCEPT_* env and
/// the loop guard set. Returns its exit status (127 if the shell can't spawn).
fn run_advice(code: &str, env: &[(&str, String)]) -> i32 {
    crate::external::shell()
        .arg("-c")
        .arg(code)
        .env("ZVCS_INTERCEPTED", "1")
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(127)
}

/// Run the original git command in a child process (so after-advice sees its
/// status), with the loop guard set so it is not re-intercepted.
fn run_original(sub: &str, args: &[String]) -> i32 {
    let Ok(exe) = std::env::current_exe() else { return 127 };
    Command::new(exe)
        .arg(sub)
        .args(args)
        .env("ZVCS_INTERCEPTED", "1")
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(127)
}

// ---------------------------------------------------------------------------
// `git zintercept` — register / list / remove / clear. Ported from zshrs
// `builtin_intercept`.
// ---------------------------------------------------------------------------

const USAGE: &str = "\
usage: git zintercept before|after|around <pattern> -- <cmd>   register advice
       git zintercept list                                     show all
       git zintercept remove <id>                              remove one
       git zintercept clear                                    remove all

  <pattern> matches the git subcommand: e.g. commit, push, 'commit *', '*'/all.
  advice <cmd> is a shell command; it sees INTERCEPT_NAME/ARGS/CMD, and (after)
  INTERCEPT_STATUS/MS/US. An around advice runs \"$INTERCEPT_CMD\" to proceed.";

pub fn zintercept(args: &[String]) -> Result<ExitCode> {
    let Some(action) = args.first().map(String::as_str) else {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    match action {
        "list" => list(),
        "clear" => {
            let n = load().len();
            save(&[])?;
            println!("cleared {n} intercept(s)");
            Ok(ExitCode::SUCCESS)
        }
        "remove" | "rm" => {
            let id: u32 = args.get(1).and_then(|s| s.parse().ok()).ok_or_else(|| {
                anyhow::anyhow!("usage: git zintercept remove <id>")
            })?;
            let mut list = load();
            let before = list.len();
            list.retain(|i| i.id != id);
            if list.len() == before {
                bail!("no intercept with id {id}");
            }
            save(&list)?;
            println!("removed intercept {id}");
            Ok(ExitCode::SUCCESS)
        }
        "before" | "after" | "around" => register(action, &args[1..]),
        _ => {
            eprintln!("{USAGE}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// `git zintercept <kind> <pattern> [--] <cmd...>`.
fn register(kind_str: &str, rest: &[String]) -> Result<ExitCode> {
    let kind = AdviceKind::parse(kind_str).expect("checked by caller");
    let Some(pattern) = rest.first().cloned() else {
        bail!("git zintercept {kind_str}: requires <pattern> -- <cmd>");
    };
    // Everything after the pattern (skipping a leading `--`) is the advice code,
    // joined with spaces; surrounding `{ }` are stripped (zshrs brace form).
    let mut tail = &rest[1..];
    if tail.first().map(|s| s == "--").unwrap_or(false) {
        tail = &tail[1..];
    }
    let code = tail.join(" ");
    let code = code.trim();
    let code = if code.starts_with('{') && code.ends_with('}') && code.len() >= 2 {
        code[1..code.len() - 1].trim().to_string()
    } else {
        code.to_string()
    };
    if code.is_empty() {
        bail!("git zintercept {kind_str}: requires <pattern> -- <cmd>");
    }

    let mut list = load();
    let id = list.iter().map(|i| i.id).max().unwrap_or(0) + 1;
    list.push(Intercept { pattern: pattern.clone(), kind, code: code.clone(), id });
    save(&list)?;
    let preview = if code.len() > 50 { format!("{}...", &code[..47]) } else { code };
    println!("intercept #{id}: {} {pattern} \u{2192} {preview}", kind.as_str());
    Ok(ExitCode::SUCCESS)
}

fn list() -> Result<ExitCode> {
    let list = load();
    if list.is_empty() {
        println!("no intercepts registered");
        return Ok(ExitCode::SUCCESS);
    }
    println!("{:>4}  {:<7}  {:<18}  CODE", "ID", "KIND", "PATTERN");
    for i in &list {
        let preview = if i.code.len() > 44 { format!("{}...", &i.code[..41]) } else { i.code.clone() };
        println!("{:>4}  {:<7}  {:<18}  {}", i.id, i.kind.as_str(), i.pattern, preview);
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    // intercept_matches — ported verbatim from zshrs's tests.
    #[test]
    fn star_and_all_match_anything() {
        assert!(intercept_matches("*", "anything", "anything --here"));
        assert!(intercept_matches("all", "git", "git status"));
    }

    #[test]
    fn exact_match_on_cmd_name() {
        assert!(intercept_matches("commit", "commit", "commit -m x"));
        assert!(!intercept_matches("commit", "push", "push origin"));
    }

    #[test]
    fn glob_matches_prefix_and_single_char() {
        assert!(intercept_matches("commit *", "commit", "commit -m x"));
        assert!(intercept_matches("_*", "_files", "_files"));
        assert!(!intercept_matches("_*", "files", "files"));
        assert!(intercept_matches("l?", "ls", "ls"));
        assert!(!intercept_matches("l?", "lsof", "lsof"));
    }

    #[test]
    fn non_glob_miss_returns_false() {
        assert!(!intercept_matches("nope", "git", "git push"));
        assert!(!intercept_matches("[invalid", "git", "git push"));
    }

    #[test]
    fn advice_kind_roundtrips() {
        for k in [AdviceKind::Before, AdviceKind::After, AdviceKind::Around] {
            assert_eq!(AdviceKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(AdviceKind::parse("nonsense"), None);
    }

    #[test]
    fn registry_line_roundtrips() {
        let i = Intercept {
            pattern: "commit *".into(),
            kind: AdviceKind::Around,
            code: "echo wrap; eval \"$INTERCEPT_CMD\"".into(),
            id: 7,
        };
        let line = format!("{}\t{}\t{}\t{}", i.id, i.kind.as_str(), i.pattern, i.code);
        let p = parse_line(&line).unwrap();
        assert_eq!(p.id, 7);
        assert_eq!(p.kind, AdviceKind::Around);
        assert_eq!(p.pattern, "commit *");
        assert_eq!(p.code, "echo wrap; eval \"$INTERCEPT_CMD\"");
        assert!(parse_line("garbage").is_none());
    }
}
