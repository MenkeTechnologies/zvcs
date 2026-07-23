//! `git merge-ours` — the "ours" merge strategy back-end.
//!
//! A faithful port of `builtin/merge-ours.c`, which is a genuine C builtin (not
//! one of the `git-merge-*` shell drivers). The whole command is four lines of
//! logic: the resulting tree of an "ours" merge is *the current index*, so the
//! only thing the strategy has to verify is that the index still matches `HEAD`.
//! If it does the strategy succeeds (exit 0) and `git merge` goes on to write
//! the merge commit from that index; if it does not, the strategy declines
//! (exit 2) and `git merge` tries the next one.
//!
//! Every argument is ignored — the C code never parses `<base>... -- HEAD
//! <remote>...`; the strategy result does not depend on the commits it is
//! handed. Consequently there is no unrecognised-flag path to bail on: stock
//! git accepts `git merge-ours --whatever` silently and so does this port.
//!
//! ### Covered (byte-identical stdout/stderr and exit code against git 2.55)
//!
//! * Exit 0, no output, when the index is identical to the `HEAD` tree.
//! * Exit 2, no output, when it differs — including a staged addition,
//!   deletion, content change, mode change, an intent-to-add entry, and an
//!   index carrying unmerged (conflict) stages.
//! * Exit 128 with `fatal: your current branch '<name>' does not have any
//!   commits yet` on an unborn `HEAD`.
//! * Exit 128 with `fatal: not a git repository (or any of the parent
//!   directories): .git` outside a repository.
//! * `-h` as the sole argument: usage line on **stdout**, exit 129.
//!   `--help-all` as the sole argument: the same line on **stderr**, exit 129
//!   (this asymmetry is what `show_usage_with_options_if_asked()` produces in
//!   2.55). With any other argument present neither is special-cased.
//!
//! ### Honest limitations
//!
//! * The index↔`HEAD` comparison is done by expanding the `HEAD` tree into an
//!   in-memory index and comparing `(path, mode, oid)` per entry, which is what
//!   `index_differs_from()` computes for a cached diff. A **sparse index** whose
//!   entries are collapsed sparse-directory records is not expanded, so such a
//!   repository can report a difference where git reports none. Sparse
//!   checkouts with a full index compare correctly.
//! * Nothing is written to the object database or the index; the command is
//!   read-only, as in git.

use anyhow::Result;
use std::process::ExitCode;

/// Stock git's usage line, byte-for-byte (`builtin_merge_ours_usage`).
const USAGE: &str = "usage: git merge-ours <base>... -- HEAD <remote>...\n";

/// `git merge-ours` — succeed iff the index still matches `HEAD`.
pub fn merge_ours(args: &[String]) -> Result<ExitCode> {
    // show_usage_with_options_if_asked(): only when it is the *sole* argument.
    if args.len() == 1 {
        if args[0] == "-h" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        if args[0] == "--help-all" {
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
    }
    // Every other argument is ignored, exactly as the C builtin ignores argv.

    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // Resolve HEAD to its tree. An unborn HEAD is a hard failure in git: the
    // `repo_get_oid("HEAD")` inside index_differs_from() dies with the
    // branch-specific message below.
    let mut head = repo.head()?;
    let Some(head_commit) = head.try_peel_to_id()? else {
        let branch = repo
            .head_name()?
            .map(|n| n.shorten().to_string())
            .unwrap_or_else(|| "HEAD".to_string());
        eprintln!("fatal: your current branch '{branch}' does not have any commits yet");
        return Ok(ExitCode::from(128));
    };
    let head_tree_id = repo.find_commit(head_commit.detach())?.tree_id()?.detach();

    // A missing index file is an empty index, not an error (git's read_index()
    // succeeds with zero entries there).
    let index = repo.index_or_empty()?;

    if index_differs_from(&index, &repo.index_from_tree(&head_tree_id)?) {
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether the worktree `index` differs from `head`, the index expanded from the
/// `HEAD` tree — the cached-diff question `index_differs_from()` answers.
///
/// Any unmerged stage counts as a difference (git's diff machinery reports
/// unmerged paths as changed), as does any mismatch in the entry set or in a
/// matched entry's path, mode or blob id.
fn index_differs_from(index: &gix::index::File, head: &gix::index::File) -> bool {
    if index
        .entries()
        .iter()
        .any(|e| e.stage() != gix::index::entry::Stage::Unconflicted)
    {
        return true;
    }
    if index.entries().len() != head.entries().len() {
        return true;
    }

    let (lhs, rhs) = (index.path_backing(), head.path_backing());
    // Both sides are in canonical index order, so a positional walk is a
    // path-ordered walk.
    index
        .entries()
        .iter()
        .zip(head.entries())
        .any(|(a, b)| a.id != b.id || a.mode != b.mode || a.path_in(lhs) != b.path_in(rhs))
}
