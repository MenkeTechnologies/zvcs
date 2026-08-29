//! `git zprecache` — compute the log caches for a repository's recent commits
//! before anyone asks for them.
//!
//! The daemon already does this on its own whenever a watched repo's refs move
//! (`zvcs.precache`, on by default). This verb is the same work on demand: after
//! a large clone or a fetch that landed while the daemon was down, one pass
//! leaves `log --stat`, `log --numstat`, `log --shortstat`, `--name-status` and
//! the abbreviated formats reading from the ledger instead of the object store.
//!
//! What it precomputes is safe to precompute because none of it can go stale: a
//! commit's abbreviation is fixed once the object exists, and a tree pair's
//! change list and per-file line tallies are a pure function of two immutable
//! trees. Nothing here guesses at what the user will run — it fills caches whose
//! entries are correct forever.
//!
//! git has no equivalent and cannot: no part of git is running between two
//! commands, so its first `log --stat` after a fetch always pays in full.

use anyhow::Result;
use std::process::ExitCode;

/// Commits warmed when `-n` is not given. The recent end of a history is the
/// part that gets read interactively.
const DEFAULT_LIMIT: usize = 200;

pub fn zprecache(args: &[String]) -> Result<ExitCode> {
    let mut limit = DEFAULT_LIMIT;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--count" => {
                i += 1;
                let Some(v) = args.get(i).and_then(|v| v.parse::<usize>().ok()) else {
                    anyhow::bail!("zprecache: -n needs a commit count");
                };
                limit = v;
            }
            "-q" | "--quiet" => quiet = true,
            other if other.starts_with('-') => {
                anyhow::bail!("zprecache: unknown option {other}")
            }
            other => anyhow::bail!("zprecache: unexpected argument {other}"),
        }
        i += 1;
    }

    let repo = crate::setup::discover()?;
    let warmed = crate::porcelain::warm_log_caches(&repo, limit);
    if !quiet {
        println!("{warmed}");
    }
    Ok(ExitCode::SUCCESS)
}
