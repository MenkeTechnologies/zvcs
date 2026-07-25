//! `git zsigs` — fleet commit-signature check (the "verify-sigs" verb).
//!
//! Across every selected repo it checks the top `-n` commits (HEAD only by
//! default) and flags any that are **not a good signature** — unsigned (`N`),
//! bad (`B`), or unverifiable (`E`/`X`/`Y`/`R`). Verification uses the shared
//! [`crate::gitsig`] substrate: the signature and payload are reconstructed
//! natively, then run through gpg (or ssh-keygen), the same tools git uses. Each
//! offending commit prints as `<code> <repo> <sha> <subject>`; the run exits
//! non-zero if any offender is found, so it gates a push / CI the way an
//! unsigned-commit policy needs.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;

use crate::gitsig::evaluate;
use crate::superset::query::{parallel_map, selected};

/// One commit that failed the signature check in a repo.
struct Offender {
    code: char,
    sha: String,
    subject: String,
}

struct RepoResult {
    workdir: std::path::PathBuf,
    offenders: Vec<Offender>,
    checked: usize,
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn subject(raw: &[u8]) -> String {
    // First line of the commit message (after the blank line separating headers).
    let msg = match raw.windows(2).position(|w| w == b"\n\n") {
        Some(p) => &raw[p + 2..],
        None => raw,
    };
    let line = msg.split(|&b| b == b'\n').next().unwrap_or(b"");
    String::from_utf8_lossy(line).chars().take(60).collect()
}

/// Check the top `n` first-parent commits of one repo.
fn check_repo(git_dir: &Path, workdir: &Path, n: usize) -> RepoResult {
    let mut res = RepoResult { workdir: workdir.to_path_buf(), offenders: Vec::new(), checked: 0 };
    let Ok(repo) = gix::open(git_dir) else { return res };
    let Ok(mut commit) = repo.head_commit() else { return res };
    for _ in 0..n {
        res.checked += 1;
        let (status, _key) = evaluate(&commit.data);
        if !status.is_good() {
            res.offenders.push(Offender {
                code: status.code(),
                sha: commit.id().to_string(),
                subject: subject(&commit.data),
            });
        }
        // Advance to the first parent, if any.
        let Some(parent) = commit.parent_ids().next() else { break };
        let Ok(pc) = repo.find_commit(parent) else { break };
        commit = pc;
    }
    res
}

pub fn zsigs(args: &[String]) -> Result<ExitCode> {
    let mut n = 1usize;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" | "--count" => n = it.next().and_then(|v| v.parse().ok()).unwrap_or(n).max(1),
            other => rest.push(other.to_string()),
        }
    }

    let Some(repos) = selected(&rest)? else { return Ok(ExitCode::SUCCESS) };
    let results = parallel_map(&repos, |gd, wd| check_repo(gd, wd, n));

    let mut flagged_repos = 0usize;
    let mut total_offenders = 0usize;
    let mut total_checked = 0usize;
    for r in &results {
        total_checked += r.checked;
        if r.offenders.is_empty() {
            continue;
        }
        flagged_repos += 1;
        for o in &r.offenders {
            total_offenders += 1;
            // N/B are the real problems; colorize them red, the rest yellow.
            let color = if o.code == 'N' || o.code == 'B' { "\x1b[31m" } else { "\x1b[33m" };
            println!("{color}{}\x1b[0m {}  {}  {}", o.code, r.workdir.display(), short(&o.sha), o.subject);
        }
    }
    eprintln!(
        "zsigs: {total_offenders} unverified commit(s) in {flagged_repos} repo(s) ({total_checked} checked across {})",
        repos.len()
    );
    Ok(if total_offenders > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitsig::GStatus;

    #[test]
    fn subject_is_first_message_line() {
        let raw = b"tree x\nauthor a\ncommitter c\n\nthe subject\n\nbody\n";
        assert_eq!(subject(raw), "the subject");
    }

    #[test]
    fn offender_flagging_uses_is_good() {
        // Only non-good statuses are offenders.
        assert!(!GStatus::Good.is_good() == false);
        assert!(GStatus::NoSignature.is_good() == false);
        assert!(GStatus::Bad.is_good() == false);
        assert!(GStatus::GoodUnknown.is_good());
    }
}
