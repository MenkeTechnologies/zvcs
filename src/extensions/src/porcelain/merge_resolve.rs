//! `git merge-resolve` — resolve two trees using the multi-base `read-tree`
//! strategy (the "resolve" merge strategy back-end).
//!
//! Stock `git-merge-resolve` is a 60-line POSIX shell driver
//! (`git-merge-resolve.sh`) that sources `git-sh-setup` and then chains five
//! plumbing commands: `git diff-index`, `git update-index -q --refresh`,
//! `git read-tree -u -m --aggressive $bases $head $remotes`, `git write-tree`,
//! and `git merge-index -o git-merge-one-file -a`. The merge itself lives
//! entirely in the two that have no substrate here:
//!
//! * `read-tree -u -m --aggressive <base> <ours> <theirs>` — the three-way index
//!   merge with worktree update. The ported `read-tree` rejects `--aggressive`
//!   (`read_tree.rs:120`) and rejects more than one tree-ish with `-m`
//!   (`read_tree.rs:183`), because gitoxide has no `unpack_trees` equivalent.
//!   `gix`'s `merge` feature provides blob and tree merges, not the index
//!   unpack-trees state machine with its stage-1/2/3 bookkeeping and worktree
//!   writes, so it is not a substitute.
//! * `git-merge-one-file` — the per-path shell resolver `merge-index` spawns.
//!   The ported `merge-index` execs whatever program it is handed
//!   (`merge_index.rs:43`); there is no `git-merge-one-file` binary in this tree
//!   to hand it.
//!
//! Everything the script does *before* it touches the index is reproduced
//! natively and byte-for-byte; every path that would mutate the index or the
//! worktree bails with a message naming the missing substrate rather than
//! writing an approximation of a merge.
//!
//! ### Covered (verified against git 2.55.0 on Darwin: stdout, stderr, exit code)
//!
//! * `-h` as the first argument — `git-sh-setup`'s `$LONG_USAGE` path with an
//!   empty `USAGE`, i.e. the single line `usage: git merge-resolve ` (note the
//!   trailing space) on **stdout**, exit 0. It is handled at source time, so it
//!   wins over every check below, including a dirty index, and needs no
//!   repository.
//! * `git_dir_init` running before any argument is looked at: outside a
//!   repository, `fatal: not a git repository (or any of the parent
//!   directories): .git` on stderr, exit 128.
//! * The `git diff-index --quiet --cached HEAD --` pre-flight, which the script
//!   runs *first*, before it parses a single argument: on any tree↔index
//!   difference, `Error: Your local changes to the following files would be
//!   overwritten by merge` followed by the changed paths each indented by four
//!   spaces — both on **stdout**, as `gettextln` and the script's `sed` pipeline
//!   emit them — then exit 2. Paths are quoted per `core.quotePath`.
//! * The argument split: everything before the first `--` is a merge base, the
//!   first argument after it is `$head`, the rest are the heads to merge.
//! * The "not handling octopus" guard — two or more heads after `$head` exits 2
//!   silently, so `git merge` can fall back to another strategy.
//! * The baseless-merge guard — no merge base before `--` exits 2 silently. A
//!   bare `git merge-resolve` with no arguments at all lands here.
//!
//! ### Not covered
//!
//! Any invocation that survives all four guards reaches `read-tree -u -m
//! --aggressive` and bails. Nothing is written: no index refresh, no worktree
//! update, no `write-tree`, and no `Trying simple merge.` /
//! `Simple merge failed, trying Automatic merge.` output is faked. The
//! script's exit-1 path (`merge-index` reporting an unresolved conflict) is
//! likewise unreachable from here.
//!
//! An unborn `HEAD` also bails: stock git lets `diff-index`'s
//! `fatal: ambiguous argument 'HEAD'` through, which is not reproduced. So does
//! an unmerged index, whose `U` records the ported `diff-index` does not emit.

use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::Repository;

/// `git-sh-setup`'s `$LONG_USAGE` for a script that sets neither `USAGE` nor
/// `OPTIONS_SPEC`: `usage: $dashless $USAGE` with `$USAGE` empty, so the line
/// ends in a space. `echo` supplies the newline.
const LONG_USAGE: &str = "usage: git merge-resolve \n";

/// The script's argument loop: merge bases, then `--`, then `$head`, then the
/// heads to merge.
struct Args {
    bases: Vec<String>,
    head: Option<String>,
    remotes: Vec<String>,
}

/// Reproduce the `case ",$sep_seen,$head,$arg," in` dispatch verbatim: `--`
/// flips the separator (every time it appears), the first argument after it
/// becomes `$head`, later ones accumulate into `$remotes`, and anything before
/// it is a merge base.
fn parse(args: &[String]) -> Args {
    let mut sep_seen = false;
    let mut bases = Vec::new();
    let mut head: Option<String> = None;
    let mut remotes = Vec::new();

    for arg in args {
        if arg == "--" {
            sep_seen = true;
        } else if !sep_seen {
            bases.push(arg.clone());
        } else if head.is_none() {
            head = Some(arg.clone());
        } else {
            remotes.push(arg.clone());
        }
    }

    Args {
        bases,
        head,
        remotes,
    }
}

/// `git merge-resolve` — see the module docs for what is and is not covered.
pub fn merge_resolve(args: &[String]) -> Result<ExitCode> {
    // `git-sh-setup` inspects only `$1`, and does so before `git_dir_init` and
    // before the script's own first line of logic.
    if args.first().map(String::as_str) == Some("-h") {
        print!("{LONG_USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    // `git_dir_init`, which every non-`-h` invocation reaches first.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // `if ! git diff-index --quiet --cached HEAD --` — the script's first
    // action, ahead of the argument loop, so it fires even for arguments that
    // would be rejected by the guards below.
    let dirty = dirty_paths(&repo)?;
    if !dirty.is_empty() {
        println!("Error: Your local changes to the following files would be overwritten by merge");
        for path in &dirty {
            println!("    {}", quote_path(path));
        }
        return Ok(ExitCode::from(2));
    }

    let parsed = parse(args);

    // `case "$remotes" in ?*' '?*) exit 2` — the pattern needs a non-empty run
    // on both sides of a space in the trailing-space-separated list, which is
    // exactly "two or more heads". Resolve declines rather than octopus-merging.
    if parsed.remotes.len() >= 2 {
        return Ok(ExitCode::from(2));
    }

    // `if test '' = "$bases"` — a baseless merge is declined silently. With no
    // arguments at all, `$bases` is empty and this is the exit taken.
    if parsed.bases.is_empty() {
        return Ok(ExitCode::from(2));
    }

    // Past this point the script refreshes the index, reads three or more trees
    // into it, and updates the worktree.
    let heads = parsed.remotes.len() + usize::from(parsed.head.is_some());
    bail!(
        "unsupported: merging {} base(s) and {heads} head(s) needs \
         `read-tree -u -m --aggressive` (the three-way index merge with worktree update, which \
         gitoxide has no unpack_trees equivalent for) and, on fallback, \
         `merge-index -o git-merge-one-file -a` (no git-merge-one-file in this tree) \
         (ported: -h usage, the dirty-index pre-flight, the argument split, the octopus and \
         baseless guards)",
        parsed.bases.len()
    );
}

/// The paths `git diff-index --cached --name-only HEAD --` would print, sorted
/// bytewise as the index — and therefore git's diff queue — orders them.
fn dirty_paths(repo: &Repository) -> Result<Vec<BString>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let head_tree = match repo.head_commit().ok().and_then(|c| c.tree_id().ok()) {
        Some(id) => id.detach(),
        None => bail!(
            "unsupported: merge-resolve against an unborn HEAD (git lets diff-index's \
             `fatal: ambiguous argument 'HEAD'` through, which is not reproduced)"
        ),
    };

    let index = repo.index_or_empty()?;
    let index_state: &gix::index::State = &index;
    if index_state.entries().iter().any(|e| e.stage_raw() != 0) {
        bail!(
            "unsupported: unmerged (conflicted) index entries — diff-index's U records are not ported"
        );
    }

    let mut paths: BTreeSet<BString> = BTreeSet::new();
    repo.tree_index_status(
        &head_tree,
        index_state,
        None,
        TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            match change {
                ChangeRef::Addition { location, .. }
                | ChangeRef::Deletion { location, .. }
                | ChangeRef::Modification { location, .. } => {
                    paths.insert(location.into_owned());
                }
                // Rename tracking is disabled above, so this never fires.
                ChangeRef::Rewrite { .. } => {}
            }
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;

    Ok(paths.into_iter().collect())
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// The `case ",$sep_seen,$head,$arg," in` dispatch: bases accumulate before
    /// the separator, the first argument after it is the head, the rest are the
    /// heads to merge.
    #[test]
    fn splits_bases_head_and_remotes() {
        let a = parse(&v(&["base1", "base2", "--", "head", "r1"]));
        assert_eq!(a.bases, v(&["base1", "base2"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1"]));

        // No separator at all: everything is a merge base, so there is no head
        // to merge — but `$bases` is non-empty, so the baseless guard does not
        // fire and the caller reaches the unported read-tree.
        let a = parse(&v(&["head", "r1"]));
        assert_eq!(a.bases, v(&["head", "r1"]));
        assert_eq!(a.head, None);
        assert!(a.remotes.is_empty());

        // A second `--` re-sets `sep_seen`, which is already `yes`, so it is
        // consumed rather than becoming a head — as in the script.
        let a = parse(&v(&["b", "--", "head", "--", "r1"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1"]));

        // No arguments: no bases, which is the silent exit-2 path.
        let a = parse(&[]);
        assert!(a.bases.is_empty());
    }

    /// Paths are emitted verbatim unless they need C quoting, matching
    /// `core.quotePath=true`.
    #[test]
    fn quotes_paths_like_git() {
        assert_eq!(quote_path("dir/file.txt"), "dir/file.txt");
        assert_eq!(quote_path("a b.txt"), "a b.txt");
        assert_eq!(quote_path("a\tb"), "\"a\\tb\"");
        assert_eq!(quote_path("q\"uote"), "\"q\\\"uote\"");
        // Non-ASCII bytes are octal-escaped, byte by byte.
        assert_eq!(quote_path("é"), "\"\\303\\251\"");
    }
}
