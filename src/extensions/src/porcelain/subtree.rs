//! `git subtree` — split a subdirectory out to, and merge it back from, its own
//! history.
//!
//! **Nothing is ported.** Every invocation bails. This module exists so the
//! dispatcher has an entry point that fails loudly and names what is missing,
//! rather than one that silently diverges from stock git.
//!
//! Stock `git-subtree` is a ~1100-line POSIX shell script
//! (`git-subtree.sh`, installed as `$(git --exec-path)/git-subtree`). It is not
//! a C builtin and has no plumbing equivalent: it is a driver that shells out to
//! `git rev-parse --parseopt --stuck-long`, `git-sh-setup`, `git log --grep`,
//! `git commit-tree`, `git merge -s subtree`, `git fetch`, `git push`,
//! `git read-tree`, and `git diff-index`, and keeps its own on-disk rewrite
//! cache under `$GIT_DIR/subtree-cache/$$`.
//!
//! The substrate that is absent from the vendored gitoxide, concretely:
//!
//!   * **The history-rewrite engine.** `split` walks every commit that touched
//!     `<prefix>`, synthesises a parallel commit for each one with the subtree
//!     as its root, and re-parents it against the already-rewritten ancestors
//!     (`copy_commit`/`copy_or_skip`/`process_split_commit` in the script). This
//!     is the same class of machinery as `filter-branch`, which is likewise not
//!     ported. gitoxide exposes commit and tree writing, but no rewrite driver
//!     and no equivalent of the `notree`/`cache_set` bookkeeping the script
//!     depends on for identity and for pruning empty commits.
//!
//!   * **The `subtree` merge strategy.** `add`/`merge`/`pull` end in
//!     `git merge -s subtree` (or `-s ours` for `--squash`). The gix `merge`
//!     feature provides blob and tree merges, but the `subtree` strategy is a
//!     shift-the-tree preprocessing step implemented in git's `merge-recursive`
//!     C code; it has no gitoxide counterpart.
//!
//!   * **The `git-subtree-dir:`/`git-subtree-mainline:`/`git-subtree-split:`
//!     commit-message protocol.** Rejoin points are discovered by `git log
//!     --grep` over those trailers, and their exact wording, ordering, and the
//!     squash-commit body format are the interchange format between repositories
//!     that were split by different git versions. Approximating them produces
//!     histories that later `git subtree` runs silently mis-link.
//!
//!   * **Transport.** `pull` and `push` run `git fetch`/`git push` against an
//!     arbitrary remote as an inner step of the command.
//!
//! An approximation is worse than a failure here: `split` writes commits and
//! `add`/`merge`/`pull` write merge commits into the user's history. A
//! plausible-looking reimplementation would produce a repository that looks
//! fine and rejoins wrong on the next invocation.
//!
//! Also not reproduced: the `git rev-parse --parseopt` usage block (exit 129 on
//! no arguments or `-h`), since that text is version-specific and belongs to the
//! implementation that is absent.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// The subcommands `git-subtree.sh:145-157` accepts.
const COMMANDS: &[&str] = &["add", "merge", "split", "pull", "push"];

/// `git subtree` — unported; see the module documentation.
pub fn subtree(args: &[String]) -> Result<ExitCode> {
    // Report the most specific thing the caller asked for, so the message names
    // the actual flag or subcommand rather than a generic refusal.
    let flag = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .find(|a| a.starts_with('-') && a.as_str() != "-");
    if let Some(flag) = flag {
        bail!(
            "unsupported flag {flag:?} (ported: nothing — git subtree is a shell driver \
             with no gitoxide substrate: no history-rewrite engine for 'split', no \
             'subtree' merge strategy for add/merge/pull, and no git-subtree-dir/-split \
             trailer protocol)"
        );
    }

    let command = args.iter().find(|a| a.as_str() != "--");
    match command.map(String::as_str) {
        Some(c) if COMMANDS.contains(&c) => bail!(
            "unsupported: 'git subtree {c}' is not ported. Stock git-subtree is a shell \
             script driving commit rewriting and the 'subtree' merge strategy; the \
             vendored gitoxide implements neither, and an approximation would write \
             wrong commits into history"
        ),
        Some(c) => bail!("fatal: unknown command '{c}'"),
        None => bail!(
            "unsupported: git subtree is not ported (no history-rewrite engine, no \
             'subtree' merge strategy, no git-subtree-* trailer protocol in gitoxide)"
        ),
    }
}
