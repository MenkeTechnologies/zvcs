//! `git zrollback` — fleet-wide undo of the last mutating operation.
//!
//! The multi-repo evolution of `zundo`: across every selected repo it resolves
//! `HEAD@{steps}` from the reflog and (with `--apply`) `reset --hard`s to it,
//! rewinding the last commit / merge / rebase / reset. It is **dry-run by
//! default** — with no `--apply` it only prints what each repo *would* do — and
//! it refuses to lose work: a repo is skipped (not rolled back) when its tree is
//! dirty, when it is mid-operation, or when rolling back would diverge from its
//! remote (the discarded commits are already pushed). `--force` overrides those
//! guards. `reset --hard` itself is reflogged, so a rollback is itself undoable.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::Result;

use crate::superset::query::{parallel_map, selected};

/// One reflog entry (`OLD NEW IDENT... UNIXTIME TZ\tMESSAGE`). Kept local rather
/// than shared with `oplog.rs` so this verb owns its own parse.
struct Entry {
    old: String,
    msg: String,
}

fn parse_line(line: &str) -> Option<Entry> {
    let (header, msg) = line.split_once('\t')?;
    let toks: Vec<&str> = header.split_whitespace().collect();
    if toks.len() < 4 {
        return None;
    }
    Some(Entry { old: toks[0].to_string(), msg: msg.to_string() })
}

fn read_head_reflog(git_dir: &Path) -> Vec<Entry> {
    match std::fs::read_to_string(git_dir.join("logs/HEAD")) {
        Ok(c) => c.lines().filter_map(parse_line).collect(),
        Err(_) => Vec::new(),
    }
}

/// The rollback target `HEAD@{steps}` = the `old` side of the reflog entry
/// `steps` from the end. `None` if there aren't that many steps, or the target
/// is the all-zero pre-initial state (nothing to roll back to).
fn target(entries: &[Entry], steps: usize) -> Option<(String, String)> {
    if steps == 0 || entries.len() < steps {
        return None;
    }
    let e = &entries[entries.len() - steps];
    if e.old.chars().all(|c| c == '0') {
        return None;
    }
    Some((e.old.clone(), e.msg.clone()))
}

/// True if a merge / rebase / cherry-pick / revert is in progress — rolling back
/// mid-operation is unsafe, so it is a guard.
fn mid_operation(git_dir: &Path) -> bool {
    let f = |n: &str| git_dir.join(n).exists();
    f("MERGE_HEAD")
        || f("CHERRY_PICK_HEAD")
        || f("REVERT_HEAD")
        || f("rebase-merge")
        || f("rebase-apply")
}

/// Sync states where the local HEAD is at or behind its remote, so discarding a
/// commit would diverge from what is already pushed.
fn diverges_from_remote(sync: &str) -> bool {
    matches!(sync, "up-to-date" | "behind" | "diverged")
}

enum Verdict {
    Rollback { to: String, msg: String, applied: Option<bool> },
    SkipNothing,
    SkipDirty,
    SkipMidOp,
    SkipDiverge(String),
}

struct Plan {
    workdir: PathBuf,
    current: String,
    verdict: Verdict,
}

fn plan_repo(git_dir: &Path, workdir: &Path, steps: usize, apply: bool, force: bool) -> Plan {
    let mk = |verdict| Plan { workdir: workdir.to_path_buf(), current: String::new(), verdict };
    let Ok(repo) = gix::open(git_dir) else { return mk(Verdict::SkipNothing) };
    let (dirty, _detached, sync, _head, current) = crate::superset::status::compute(&repo);

    let entries = read_head_reflog(git_dir);
    let Some((to, msg)) = target(&entries, steps) else {
        return Plan { workdir: workdir.to_path_buf(), current, verdict: Verdict::SkipNothing };
    };

    // Guards, unless --force.
    if !force {
        if mid_operation(git_dir) {
            return Plan { workdir: workdir.to_path_buf(), current, verdict: Verdict::SkipMidOp };
        }
        if dirty {
            return Plan { workdir: workdir.to_path_buf(), current, verdict: Verdict::SkipDirty };
        }
        if diverges_from_remote(&sync) {
            return Plan { workdir: workdir.to_path_buf(), current, verdict: Verdict::SkipDiverge(sync) };
        }
    }

    let applied = if apply { Some(reset_hard(workdir, &to)) } else { None };
    Plan { workdir: workdir.to_path_buf(), current, verdict: Verdict::Rollback { to, msg, applied } }
}

/// Reuse the faithful porcelain reset (ref + index + worktree, reflogged).
fn reset_hard(workdir: &Path, target: &str) -> bool {
    let Ok(exe) = crate::hosted::git_exe() else { return false };
    Command::new(exe)
        .args(["reset", "--hard", target])
        .current_dir(workdir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn short(sha: &str) -> &str {
    match sha.char_indices().nth(12) {
        Some((i, _)) => &sha[..i],
        None => sha,
    }
}

pub fn zrollback(args: &[String]) -> Result<ExitCode> {
    let mut steps = 1usize;
    let mut apply = false;
    let mut force = false;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--apply" => apply = true,
            "--force" | "-f" => force = true,
            "--steps" | "-n" => steps = it.next().and_then(|v| v.parse().ok()).unwrap_or(steps).max(1),
            other => rest.push(other.to_string()),
        }
    }

    let Some(repos) = selected(&rest)? else { return Ok(ExitCode::SUCCESS) };
    let plans = parallel_map(&repos, |gd, wd| plan_repo(gd, wd, steps, apply, force));

    let (mut rolled, mut would, mut skipped, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for p in &plans {
        let wd = p.workdir.display();
        match &p.verdict {
            Verdict::Rollback { to, msg, applied } => match applied {
                Some(true) => {
                    println!("\x1b[32mrolled back\x1b[0m {wd}  {} → {}", short(&p.current), short(to));
                    rolled += 1;
                }
                Some(false) => {
                    println!("\x1b[31mFAILED\x1b[0m {wd}  reset --hard {} failed", short(to));
                    failed += 1;
                }
                None => {
                    println!("\x1b[33mwould roll back\x1b[0m {wd}  {} → {}  \"{}\"", short(&p.current), short(to), msg);
                    would += 1;
                }
            },
            Verdict::SkipNothing => {
                println!("\x1b[2mskip\x1b[0m {wd}  nothing to roll back");
                skipped += 1;
            }
            Verdict::SkipDirty => {
                println!("\x1b[2mskip\x1b[0m {wd}  dirty worktree (use --force to discard)");
                skipped += 1;
            }
            Verdict::SkipMidOp => {
                println!("\x1b[2mskip\x1b[0m {wd}  mid-operation (merge/rebase/cherry-pick/revert)");
                skipped += 1;
            }
            Verdict::SkipDiverge(sync) => {
                println!("\x1b[2mskip\x1b[0m {wd}  would diverge from remote ({sync}; use --force)");
                skipped += 1;
            }
        }
    }
    if apply {
        eprintln!("zrollback: {rolled} rolled back, {failed} failed, {skipped} skipped ({} repos)", repos.len());
    } else {
        eprintln!("zrollback: {would} would roll back, {skipped} skipped ({} repos) — pass --apply to execute", repos.len());
    }
    Ok(if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(old: &str, new: &str, msg: &str) -> String {
        format!("{old} {new} T <t@e.x> 1700000000 +0000\t{msg}")
    }

    #[test]
    fn target_resolves_nth_reflog_step() {
        let z = "0".repeat(40);
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = "c".repeat(40);
        // oldest→newest: z→a (initial), a→b (commit), b→c (commit). HEAD=c.
        let lines = [entry(&z, &a, "commit: init"), entry(&a, &b, "commit: two"), entry(&b, &c, "commit: three")];
        let entries: Vec<Entry> = lines.iter().filter_map(|l| parse_line(l)).collect();
        // HEAD@{1} = old of newest = b; HEAD@{2} = old of second-newest = a.
        assert_eq!(target(&entries, 1).unwrap().0, b);
        assert_eq!(target(&entries, 2).unwrap().0, a);
        // HEAD@{3} would be the all-zero pre-initial state → refused.
        assert!(target(&entries, 3).is_none());
        // Beyond history → none.
        assert!(target(&entries, 4).is_none());
        assert!(target(&entries, 0).is_none());
    }

    #[test]
    fn divergence_guard_classifies_sync_states() {
        // At/behind remote → rolling back diverges (guarded).
        assert!(diverges_from_remote("up-to-date"));
        assert!(diverges_from_remote("behind"));
        assert!(diverges_from_remote("diverged"));
        // Local-only commits → safe to discard.
        assert!(!diverges_from_remote("ahead"));
        assert!(!diverges_from_remote("no-upstream"));
    }
}
