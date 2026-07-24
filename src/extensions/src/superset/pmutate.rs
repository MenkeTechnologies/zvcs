//! Parallel git mutations across the indexed repo set — `zfetch`, `zgc`,
//! `zfsck`, `zprune`, `zcheckout`, `ztagall`, `zcommitall`, `zpushall`,
//! `zclean`.
//!
//! Each verb selects repos via [`Selector`] and runs a git subcommand in every
//! one concurrently through the shared [`parallel_map`], invoking this very
//! binary (`current_exe`) so the operation runs through zvcs's own porcelain and
//! fair per-repo lane — not a `git` looked up on `PATH`. Output is grouped per
//! repo, failures are recorded in the ledger, and repos where the operation does
//! not apply (branch absent, nothing to commit, not ahead) are skipped, not
//! forced.

use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{bail, Result};

use crate::superset::query::{parallel_map, select_repos, selected};
use crate::superset::select::Selector;

/// What happened to one repo.
enum Outcome {
    Ran(bool, String),
    Skipped(#[allow(dead_code)] String),
}

/// Run a git subcommand (via this binary) in `workdir`, capturing its output.
fn git_in(workdir: &Path, sub: &str, extra: &[&str]) -> Outcome {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Outcome::Ran(false, format!("cannot resolve zvcs binary: {e}")),
    };
    match Command::new(exe).arg(sub).args(extra).current_dir(workdir).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            Outcome::Ran(o.status.success(), s)
        }
        Err(e) => Outcome::Ran(false, format!("failed to run `git {sub}`: {e}")),
    }
}

/// Fan `action` across `repos` in parallel, print grouped output, record
/// failures, and return an aggregate exit code (failure if any repo failed).
fn fan_out(
    repos: &[(std::path::PathBuf, std::path::PathBuf)],
    label: &str,
    action: impl Fn(&Path, &Path) -> Outcome + Sync,
) -> ExitCode {
    let outcomes = parallel_map(repos, |gd, wd| action(gd, wd));
    let (mut ok, mut failed, mut skipped) = (0usize, 0usize, 0usize);
    for ((git_dir, wd), outcome) in repos.iter().zip(&outcomes) {
        match outcome {
            Outcome::Ran(true, out) => {
                emit(wd, out);
                ok += 1;
            }
            Outcome::Ran(false, out) => {
                emit(wd, out);
                let _ = crate::db::record_failure(git_dir, label, &format!("{}: {label} failed", wd.display()));
                failed += 1;
            }
            Outcome::Skipped(_) => skipped += 1,
        }
    }
    eprintln!("{label}: {ok} ok, {failed} failed, {skipped} skipped ({} repos)", repos.len());
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Print a repo's grouped output block (header, then any non-empty output).
fn emit(workdir: &Path, output: &str) {
    println!("== {} ==", workdir.display());
    let trimmed = output.trim_end();
    if !trimmed.is_empty() {
        println!("{trimmed}");
    }
}

/// `git zfetch [selectors]` — `git fetch` in every indexed repo, in parallel.
pub fn zfetch(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    Ok(fan_out(&repos, "zfetch", |_gd, wd| git_in(wd, "fetch", &[])))
}

/// `git zgc [selectors]` — `git gc` in every indexed repo, in parallel.
pub fn zgc(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    Ok(fan_out(&repos, "zgc", |_gd, wd| git_in(wd, "gc", &[])))
}

/// `git zfsck [selectors]` — `git fsck` in every indexed repo, in parallel.
pub fn zfsck(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    Ok(fan_out(&repos, "zfsck", |_gd, wd| git_in(wd, "fsck", &[])))
}

/// `git zprune [selectors]` — `git prune` in every indexed repo, in parallel.
pub fn zprune(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    Ok(fan_out(&repos, "zprune", |_gd, wd| git_in(wd, "prune", &[])))
}

/// `git zcheckout [selectors] <branch>` — check out `<branch>` in every indexed
/// repo that has it (repos without the branch are skipped, never created).
pub fn zcheckout(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let Some(branch) = rest.into_iter().next() else {
        bail!("usage: git zcheckout [selectors] <branch>");
    };
    let Some(repos) = select_repos(&sel)? else { return Ok(ExitCode::SUCCESS) };
    let branch = &branch;
    Ok(fan_out(&repos, "zcheckout", |gd, wd| {
        let exists = gix::open(gd)
            .ok()
            .map(|r| r.try_find_reference(&format!("refs/heads/{branch}")).ok().flatten().is_some())
            .unwrap_or(false);
        if !exists {
            return Outcome::Skipped(format!("no branch {branch}"));
        }
        git_in(wd, "checkout", &[branch.as_str()])
    }))
}

/// `git ztagall [selectors] <tag>` — create tag `<tag>` at HEAD in every indexed
/// repo (a repo that already has the tag reports the failure).
pub fn ztagall(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let Some(tag) = rest.into_iter().next() else {
        bail!("usage: git ztagall [selectors] <tag>");
    };
    let Some(repos) = select_repos(&sel)? else { return Ok(ExitCode::SUCCESS) };
    let tag = &tag;
    Ok(fan_out(&repos, "ztagall", |_gd, wd| git_in(wd, "tag", &[tag.as_str()])))
}

/// `git zcommitall [selectors] -m <msg>` — commit tracked changes (`commit -a`)
/// in every dirty indexed repo with `<msg>`; clean repos are skipped.
pub fn zcommitall(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let msg = rest.windows(2).find(|w| w[0] == "-m").map(|w| w[1].clone());
    let Some(msg) = msg else {
        bail!("usage: git zcommitall [selectors] -m <msg>");
    };
    let Some(repos) = select_repos(&sel)? else { return Ok(ExitCode::SUCCESS) };
    let msg = &msg;
    Ok(fan_out(&repos, "zcommitall", |gd, wd| {
        let dirty = gix::open(gd).ok().map(|r| r.is_dirty().unwrap_or(false)).unwrap_or(false);
        if !dirty {
            return Outcome::Skipped("nothing to commit".into());
        }
        git_in(wd, "commit", &["-a", "-m", msg.as_str()])
    }))
}

/// `git zpushall [selectors]` — `git push` every indexed repo that is ahead of
/// its upstream; repos not ahead are skipped.
pub fn zpushall(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    Ok(fan_out(&repos, "zpushall", |gd, wd| {
        let ahead = gix::open(gd)
            .ok()
            .and_then(|r| crate::superset::analytics::ahead_behind(&r))
            .map(|(a, _)| a > 0)
            .unwrap_or(false);
        if !ahead {
            return Outcome::Skipped("not ahead".into());
        }
        git_in(wd, "push", &[])
    }))
}

/// `git zclean -f [selectors]` — remove untracked files and directories
/// (`git clean -fd`) in every indexed repo. Destructive, so `-f` is required.
pub fn zclean(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    if !rest.iter().any(|a| a == "-f" || a == "--force") {
        bail!("git zclean deletes untracked files across every repo; pass -f to confirm");
    }
    let Some(repos) = select_repos(&sel)? else { return Ok(ExitCode::SUCCESS) };
    Ok(fan_out(&repos, "zclean", |_gd, wd| git_in(wd, "clean", &["-fd"])))
}
