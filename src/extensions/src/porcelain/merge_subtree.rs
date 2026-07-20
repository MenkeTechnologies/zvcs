//! `git merge-subtree` — the `-subtree` spelling of git's `merge-recursive`
//! plumbing. **The merge itself is not ported**; only the argument handling and
//! pre-flight checks that stock git performs before it starts merging are
//! reproduced here.
//!
//! `git-merge-subtree` is not a separate program: it is `builtin/merge-recursive.c`
//! (`cmd_merge_recursive`) invoked under a name ending in `-subtree`, which is
//! the sole trigger for `o.subtree_shift = ""`. That single assignment is the
//! command's entire reason to exist, and it routes the merge through
//! `shift_tree()` in `match-trees.c` — the tree-alignment search that scores
//! candidate sub-trees (`score_trees`/`match_trees`) and rewrites one side's
//! tree with `splice_tree()` so a project merged in as a subdirectory lines up
//! with its standalone history. There is no counterpart to any of that in the
//! vendored gitoxide crates under `src/ported`: a search for
//! `shift_tree`/`match_trees`/`score_trees`/`splice_tree`/`subtree_shift`
//! returns nothing outside a single unrelated doc comment. `gix-merge` merges
//! two trees against a common ancestor, which is `merge-recursive` *without*
//! the shift — so wiring it up here would produce a command that is confidently
//! wrong for exactly the inputs `merge-subtree` is used on.
//!
//! Two further gaps stand behind that one, either of which alone would rule out
//! byte-identical behaviour:
//!
//! * **Message stream.** `merge-ort` narrates the merge on stdout/stderr
//!   (`Auto-merging <path>`, `CONFLICT (content): Merge conflict in <path>`,
//!   `Removing <path>`, the rename/rename and rename/delete forms, …). Those
//!   strings appear nowhere in `gix-merge`, which reports structured
//!   `Conflict`/`Resolution` records under a different taxonomy; reproducing
//!   git's exact wording *and* per-path ordering would mean inventing text.
//! * **Post-command state.** The command's real output is the index and
//!   worktree it rewrites: stage 1/2/3 unmerged entries plus conflict-marked
//!   files on disk. `Repository::merge_trees` documents that it makes "no
//!   change to the worktree or index".
//!
//! So the merge is refused rather than approximated. What *is* covered, matched
//! against git 2.55.0, is everything `cmd_merge_recursive` does before it calls
//! `merge_recursive_generic()`:
//!
//! * The `argc < 4` usage guard — fewer than three arguments prints
//!   `usage: git merge-subtree <base>... -- <head> <remote> ...` on stderr and
//!   exits 129, before the repository is touched.
//! * The positional scan: any argument starting with `--` is a strategy option
//!   (`--` alone ends the scan), everything else is a merge base. Bases are
//!   resolved in encounter order, so a bad base is reported before any later
//!   argument is looked at. A full-length hex id is taken verbatim without an
//!   existence check, as `repo_get_oid()` does; anything else goes through
//!   revision parsing and fails with `fatal: could not parse object '<arg>'`
//!   (exit 128).
//! * The 20-base ceiling: the 21st and later bases are skipped with
//!   `warning: cannot handle more than 20 bases. Ignoring <arg>.` on stderr and
//!   are never parsed.
//! * `parse_merge_opt()`'s exact accept/reject set (see [`parse_merge_opt`]),
//!   including the `find-renames=`/`rename-threshold=` score grammar and
//!   `diff-algorithm=`'s four legal values. An unaccepted option is
//!   `fatal: unknown option <arg>`, exit 128.
//! * The `argc - i != 3` arity check —
//!   `fatal: not handling anything other than two heads merge.`, exit 128.
//! * `repo_read_index_unmerged()` → `die_resolve_conflict("merge")`: the
//!   `error:` line, the two `hint:` lines (suppressed when
//!   `advice.resolveConflict` is false) and `fatal: Exiting because of an
//!   unresolved conflict.`, exit 128.
//! * Resolving `<head>` and `<remote>`, failing with
//!   `fatal: could not resolve ref '<arg>'`, exit 128.
//!
//! Only once all of those have passed does the command bail with the substrate
//! it is missing. One deliberate divergence on a git bug: stock git 2.55.0
//! segfaults (exit 139) when `<head>` or `<remote>` is a full-length hex id
//! naming a missing object; this module reports the missing object instead.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::hash::ObjectId;

/// Verbatim `builtin_merge_recursive_usage`, already interpolated with the
/// `merge-subtree` command name that dispatch reaches this module under.
const USAGE: &str = "usage: git merge-subtree <base>... -- <head> <remote> ...";

/// The most merge bases `cmd_merge_recursive` will hold (`ARRAY_SIZE(bases) - 1`).
const MAX_BASES: usize = 20;

/// `git merge-subtree` — three-way merge with subtree alignment.
///
/// Argument handling and pre-flight checks are faithful; the merge is refused.
/// See the module docs for the exact split.
pub fn merge_subtree(args: &[String]) -> Result<ExitCode> {
    // `if (argc < 4) usagef(...)`. argc counts argv[0], so this is three
    // arguments here, and it fires before the repository is opened.
    if args.len() < 3 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    // The positional scan. `end` ends up at the index of the `--` that stopped
    // it, or at args.len() when no `--` was seen — mirroring C's `i` shifted by
    // one for the missing argv[0].
    let mut bases: Vec<ObjectId> = Vec::new();
    let mut end = args.len();
    for (idx, arg) in args.iter().enumerate() {
        if let Some(opt) = arg.strip_prefix("--") {
            if opt.is_empty() {
                end = idx;
                break;
            }
            if !parse_merge_opt(opt) {
                eprintln!("fatal: unknown option {arg}");
                return Ok(ExitCode::from(128));
            }
            continue;
        }
        if bases.len() < MAX_BASES {
            let Some(oid) = resolve_object(&repo, arg) else {
                eprintln!("fatal: could not parse object '{arg}'");
                return Ok(ExitCode::from(128));
            };
            bases.push(oid);
        } else {
            // C warns and does not parse; the count is always plural here.
            eprintln!("warning: cannot handle more than {MAX_BASES} bases. Ignoring {arg}.");
        }
    }

    // `if (argc - i != 3)`: exactly `--`, `<head>`, `<remote>` must remain.
    if args.len() - end != 3 {
        eprintln!("fatal: not handling anything other than two heads merge.");
        return Ok(ExitCode::from(128));
    }

    // `repo_read_index_unmerged()` runs before the two heads are resolved.
    if index_has_unmerged_entries(&repo)? {
        eprintln!("error: Merging is not possible because you have unmerged files.");
        if repo.config_snapshot().boolean("advice.resolveConflict") != Some(false) {
            eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
            eprintln!("hint: as appropriate to mark resolution and make a commit.");
        }
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }

    let branch1 = &args[end + 1];
    let branch2 = &args[end + 2];
    for branch in [branch1, branch2] {
        if resolve_object(&repo, branch).is_none() {
            eprintln!("fatal: could not resolve ref '{branch}'");
            return Ok(ExitCode::from(128));
        }
    }

    // Everything git checks has passed; the merge is what cannot be done.
    bail!(
        "merge-subtree cannot be performed: gitoxide has no subtree-shift substrate \
         (no match-trees score_trees/match_trees/splice_tree, so o.subtree_shift is \
         unimplementable), no merge-ort message stream, and gix-merge does not write \
         the index or worktree (ported: argument parsing, base/head resolution, \
         unmerged-index check, all usage and fatal paths)"
    );
}

/// `repo_get_oid()` as this command needs it: a full-length hex id is accepted
/// verbatim, without checking that the object exists, and anything else is a
/// revision expression. `None` is C's non-zero return.
fn resolve_object(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let hexsz = repo.object_hash().len_in_hex();
    if spec.len() == hexsz && spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        if let Ok(id) = ObjectId::from_hex(spec.as_bytes()) {
            return Some(id);
        }
    }
    repo.rev_parse_single(spec).ok().map(|id| id.detach())
}

/// Whether the index holds any entry at a stage other than 0, i.e. whether
/// `repo_read_index_unmerged()` would report unmerged paths. A missing index is
/// an empty index.
fn index_has_unmerged_entries(repo: &gix::Repository) -> Result<bool> {
    let index = repo.index_or_empty()?;
    let state: &gix::index::State = &index;
    Ok(state.entries().iter().any(|e| e.stage_raw() != 0))
}

/// `parse_merge_opt()` from `merge-recursive.c`, reduced to accept/reject since
/// the merge these options would configure is not run. `s` is the argument with
/// its leading `--` stripped. Returns false where C returns -1.
fn parse_merge_opt(s: &str) -> bool {
    match s {
        "ours" | "theirs" | "subtree" | "patience" | "histogram" | "ignore-space-change"
        | "ignore-all-space" | "ignore-space-at-eol" | "ignore-cr-at-eol" | "renormalize"
        | "no-renormalize" | "no-renames" | "find-renames" => true,
        _ => {
            if let Some(arg) = s.strip_prefix("subtree=") {
                let _ = arg; // any value, including empty, is a shift prefix
                true
            } else if let Some(arg) = s.strip_prefix("diff-algorithm=") {
                matches!(arg, "myers" | "default" | "minimal" | "patience" | "histogram")
            } else if let Some(arg) = s
                .strip_prefix("find-renames=")
                .or_else(|| s.strip_prefix("rename-threshold="))
            {
                // C keeps the score only if parse_rename_score() consumed the
                // whole value; a trailing character is the sole rejection.
                rename_score_len(arg) == arg.len()
            } else {
                false
            }
        }
    }
}

/// How many leading bytes of `s` `parse_rename_score()` consumes: digits, at
/// most one `.`, and a single trailing `%` which ends the scan.
fn rename_score_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut dot = false;
    while i < bytes.len() {
        match bytes[i] {
            b'.' if !dot => {
                dot = true;
                i += 1;
            }
            b'%' => return i + 1,
            b'0'..=b'9' => i += 1,
            _ => break,
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exactly_git_s_strategy_options() {
        for ok in [
            "ours",
            "theirs",
            "subtree",
            "subtree=",
            "subtree=dir",
            "patience",
            "histogram",
            "diff-algorithm=myers",
            "diff-algorithm=default",
            "diff-algorithm=minimal",
            "diff-algorithm=patience",
            "diff-algorithm=histogram",
            "ignore-space-change",
            "ignore-all-space",
            "ignore-space-at-eol",
            "ignore-cr-at-eol",
            "renormalize",
            "no-renormalize",
            "no-renames",
            "find-renames",
            "find-renames=",
            "find-renames=.",
            "find-renames=%",
            "find-renames=50",
            "find-renames=50%",
            "find-renames=5.5",
            "find-renames=5.5%",
            "rename-threshold=5",
            "rename-threshold=",
        ] {
            assert!(parse_merge_opt(ok), "git accepts --{ok}");
        }
        // Verified against git 2.55.0: each of these is `fatal: unknown option`.
        for bad in [
            "diff-algorithm=bogus",
            "diff-algorithm=",
            "find-renames=1x",
            "find-renames=bogus",
            "ours=x",
            "no-renames=1",
            "ort",
            "recursive",
            "verbose",
            "bogus",
        ] {
            assert!(!parse_merge_opt(bad), "git rejects --{bad}");
        }
    }

    #[test]
    fn rename_score_stops_at_the_first_illegal_byte() {
        assert_eq!(rename_score_len("50"), 2);
        assert_eq!(rename_score_len("50%"), 3);
        assert_eq!(rename_score_len("5.5%"), 4);
        // A second dot ends the scan, leaving a tail that makes C reject.
        assert_eq!(rename_score_len("5.5.5"), 3);
        // `%` terminates even mid-string, so the tail survives to be rejected.
        assert_eq!(rename_score_len("5%0"), 2);
        assert_eq!(rename_score_len("bogus"), 0);
    }
}
