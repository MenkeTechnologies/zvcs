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
use gix::bstr::ByteSlice;

use crate::superset::query::{parallel_map, selected};

/// One `HEAD` reflog entry. Kept local rather than shared with `oplog.rs` so
/// this verb owns its own reading.
struct Entry {
    old: String,
    msg: String,
}

/// The `HEAD` reflog (oldest→newest), read through the ref store, so a reftable
/// repository answers as well as a files one.
fn read_head_reflog(repo: &gix::Repository) -> Vec<Entry> {
    let mut out = Vec::new();
    let _ = crate::refstore::for_each_reflog_entry(repo, "HEAD", false, |e| {
        out.push(Entry::of(e));
        std::ops::ControlFlow::Continue(())
    });
    out
}

impl Entry {
    /// One entry as the ref store's reflog walk yields it.
    fn of(e: &crate::refstore::ReflogEntry) -> Self {
        Entry {
            old: e.old_oid.to_string(),
            msg: e.message.trim_end_with(|c| c == '\n').to_str_lossy().into_owned(),
        }
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
/// mid-operation is unsafe, so it is a guard. `CHERRY_PICK_HEAD` and
/// `REVERT_HEAD` are root refs, asked of the ref store; `MERGE_HEAD` is a file
/// in every ref storage format, as are the rebase state directories.
fn mid_operation(repo: &gix::Repository) -> bool {
    let git_dir = repo.git_dir();
    crate::refstore::state_ref_exists(repo, "MERGE_HEAD")
        || crate::refstore::state_ref_exists(repo, "CHERRY_PICK_HEAD")
        || crate::refstore::state_ref_exists(repo, "REVERT_HEAD")
        || git_dir.join("rebase-merge").exists()
        || git_dir.join("rebase-apply").exists()
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

    let entries = read_head_reflog(&repo);
    let Some((to, msg)) = target(&entries, steps) else {
        return Plan { workdir: workdir.to_path_buf(), current, verdict: Verdict::SkipNothing };
    };

    // Guards, unless --force.
    if !force {
        if mid_operation(&repo) {
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
        let entries: Vec<Entry> = lines
            .iter()
            .filter_map(|l| crate::refstore::parse_reflog_line(format!("{l}\n").as_bytes(), gix::hash::Kind::Sha1))
            .map(|e| Entry::of(&e))
            .collect();
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
