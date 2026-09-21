//! `git zreview` — aggregate the pending (uncommitted) change across the fleet.
//!
//! For every selected repo that has uncommitted work, print its `git status
//! --short` block grouped under the repo path, plus a per-repo diffstat, so you
//! can review everything about to be committed across many repos in one screen —
//! the read-side companion to `zcommitall`. Runs the status probe through this
//! binary (`current_exe`) over the shared worker pool, so it is fleet-parallel
//! and never shells out to a `git` on `PATH`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::Result;

use crate::superset::query::{parallel_map, selected};

/// Run a git subcommand (via this binary) in `workdir`, returning captured
/// stdout on success, or `None` on failure.
fn git_out(workdir: &Path, sub: &str, extra: &[&str]) -> Option<String> {
    let exe = crate::hosted::git_exe().ok()?;
    let o = Command::new(exe).arg(sub).args(extra).current_dir(workdir).output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// One repo's pending change: the short-status block and a compact diffstat.
struct Pending {
    status: String,
    stat: String,
}

/// What one repository contributes to the review.
enum Review {
    /// Nothing to review.
    Clean,
    /// The status block and diffstat to show.
    Pending(Pending),
    /// The status probe did not run — a git dir that is gone, or one whose
    /// permissions deny it. Distinct from `Clean` on purpose: both used to be
    /// `None`, so a repository holding uncommitted work was dropped from the
    /// screen that exists to show uncommitted work, and the summary counted it
    /// among the "indexed" without a word.
    Unreadable,
}

fn review_repo(workdir: &Path) -> Review {
    let Some(status) = git_out(workdir, "status", &["--short"]) else {
        return Review::Unreadable;
    };
    if status.trim().is_empty() {
        return Review::Clean;
    }
    // Diffstat of tracked changes (staged + unstaged), for a size-at-a-glance line.
    let stat = git_out(workdir, "diff", &["HEAD", "--stat"]).unwrap_or_default();
    Review::Pending(Pending { status, stat })
}

pub fn zreview(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let pending: Vec<(PathBuf, Review)> = {
        let per = parallel_map(&repos, |_gd, wd| review_repo(wd));
        repos.iter().map(|(_, wd)| wd.clone()).zip(per).collect()
    };
    let mut repos_with_change = 0usize;
    let mut total_entries = 0usize;
    let mut unreadable = 0usize;
    for (wd, p) in &pending {
        let p = match p {
            Review::Clean => continue,
            Review::Unreadable => {
                // Shown, not skipped: a repository that cannot be read is the
                // one thing a review screen must not leave out silently.
                unreadable += 1;
                println!("\x1b[1m== {} ==\x1b[0m  (unreadable)", wd.display());
                println!();
                continue;
            }
            Review::Pending(p) => p,
        };
        repos_with_change += 1;
        let entries = p.status.lines().filter(|l| !l.trim().is_empty()).count();
        total_entries += entries;
        println!("\x1b[1m== {} ==\x1b[0m", wd.display());
        print!("{}", p.status);
        let stat = p.stat.trim_end();
        if !stat.is_empty() {
            // Keep only the summary line of --stat (the last "N files changed…").
            if let Some(last) = stat.lines().last() {
                println!("\x1b[2m{}\x1b[0m", last.trim());
            }
        }
        println!();
    }
    eprintln!(
        "zreview: {repos_with_change} repo(s) with {total_entries} pending change(s) across {} indexed{}",
        repos.len(),
        crate::superset::query::unreadable_note(unreadable)
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_nonempty_status() {
        // review_repo returns None on a clean tree; the aggregate must not count it.
        let p = Pending { status: " M a\n?? b\n".into(), stat: String::new() };
        let entries = p.status.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(entries, 2);
        // Whitespace-only status has no real entries to review.
        let empty = Pending { status: "   \n".into(), stat: String::new() };
        assert_eq!(empty.status.lines().filter(|l| !l.trim().is_empty()).count(), 0);
    }
}
