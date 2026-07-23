//! `git merge-recursive-ours` — the `recursive` merge strategy back-end.
//!
//! `merge-recursive-ours` is not a separate implementation: `git.c` registers
//! `merge-recursive`, `merge-recursive-ours`, `merge-recursive-theirs` and
//! `merge-subtree` as four names for the single builtin `cmd_merge_recursive`,
//! and that function only ever inspects `argv[0]` to test for a `-subtree`
//! suffix. The `-ours` suffix is **not** read, so this command is byte-for-byte
//! `git merge-recursive` with a different usage string. Verified against git
//! 2.55.0: `merge-recursive-ours <base> -- <ours> <theirs>` and
//! `merge-recursive <base> -- <ours> <theirs>` produced identical stdout,
//! identical exit status and an identical conflicted index (same three stage
//! entries, same blob ids) — the "ours" preference is `--ours`, an *option*,
//! not the command name.
//!
//! ### Not covered: the merge itself
//!
//! The command is `RUN_SETUP | NEED_WORK_TREE`; its entire product is a mutated
//! index (stage 1/2/3 entries for conflicted paths) and a mutated worktree
//! (merged content, conflict-marker files, deletions, renames). The vendored
//! `gix` in this tree is built with `merge` but **not** `worktree-mutation`
//! (`src/extensions/Cargo.toml:24`), so `gix-worktree-state` — the only
//! checkout substrate gitoxide has — is not compiled in, and `Cargo.toml` is
//! not this module's to change. `gix::Repository::merge_commits` is available
//! and computes a merged tree, but it explicitly makes "no change to the
//! worktree or index" (`src/ported/gix/src/repository/merge.rs:168`), and there
//! is no ported `unpack_trees` equivalent to apply a result to the worktree —
//! the same gap `read-tree` documents for `-u -m`.
//!
//! Therefore the merge is **not** attempted. Everything `cmd_merge_recursive`
//! does before `merge_recursive_generic()` is reproduced exactly; the merge call
//! itself bails naming the missing substrate rather than writing a tree that
//! would leave the repository in a state stock git never produces. A partially
//! applied merge is worse than a refused one.
//!
//! ### Covered (stdout, stderr and exit code verified against git 2.55.0)
//!
//! * `-h` or `--help-all` as the *sole* argument — usage line on **stdout**,
//!   exit 129, without requiring a repository (`git.c` answers it before
//!   `setup_git_directory()`). With any other argument alongside, both are
//!   ordinary arguments and take the `argc < 4` path below (stderr, 129).
//! * Outside a repository: `fatal: not a git repository (or any of the parent
//!   directories): .git`, exit 128 — before any argument is examined.
//! * In a bare repository: `fatal: this operation must be run in a work tree`,
//!   exit 128 — the `NEED_WORK_TREE` check, also before argument handling.
//! * `argc < 4` (fewer than three arguments here): the usage line on
//!   **stderr**, exit 129 (`usagef()`).
//! * Argument scan, in `cmd_merge_recursive`'s order:
//!   - a bare `--` ends the scan;
//!   - `--<opt>` goes through [`parse_merge_opt`], and an unaccepted one is
//!     `fatal: unknown option --<opt>`, exit 128;
//!   - anything else is a merge base, resolved with
//!     `fatal: could not parse object '<arg>'`, exit 128 on failure;
//!   - past 20 bases, `warning: cannot handle more than 20 bases. Ignoring
//!     <arg>.` on stderr and the base is dropped.
//! * Exactly three arguments must follow the `--`, else `fatal: not handling
//!   anything other than two heads merge.`, exit 128.
//! * An index carrying unmerged entries is `die_resolve_conflict("merge")`: the
//!   four-line `error:`/`hint:`/`hint:`/`fatal:` block on stderr, exit 128 —
//!   and this precedes head resolution, so a bogus `<head>` is not reported.
//! * `<head>` / `<remote>` resolution failure: `fatal: could not resolve ref
//!   '<arg>'`, exit 128.
//!
//! Reaching the end of that list means the arguments are valid and the merge
//! would run; that is where this port stops.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// Stock git's usage line for this name (`builtin_merge_recursive_usage` with
/// `argv[0]` substituted), byte-for-byte — note the literal `...` after
/// `<remote>` and the absence of any trailing space.
const USAGE: &str = "usage: git merge-recursive-ours <base>... -- <head> <remote> ...\n";

/// `bases[]` in `cmd_merge_recursive` is `ARRAY_SIZE(bases)-1` deep.
const MAX_BASES: usize = 20;

/// `git merge-recursive-ours` — validate a recursive-strategy invocation.
///
/// Performs every check `cmd_merge_recursive` performs, in the same order and
/// with the same messages and exit codes, then refuses the merge itself (see
/// the module docs for the missing substrate).
pub fn merge_recursive_ours(args: &[String]) -> Result<ExitCode> {
    // `git.c` answers a lone `-h`/`--help-all` on stdout before RUN_SETUP, so
    // it works outside a repository and in a bare one.
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // RUN_SETUP, then NEED_WORK_TREE — both before any argument is looked at.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }

    // `if (argc < 4) usagef(...)` — argc counts argv[0], which `args` omits.
    if args.len() < 3 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // The argument scan. `end` is where a bare `--` stopped it, or args.len().
    let mut bases: Vec<gix::hash::ObjectId> = Vec::new();
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
            let Ok(id) = repo.rev_parse_single(arg.as_str()) else {
                eprintln!("fatal: could not parse object '{arg}'");
                return Ok(ExitCode::from(128));
            };
            bases.push(id.detach());
        } else {
            eprintln!("warning: cannot handle more than {MAX_BASES} bases. Ignoring {arg}.");
        }
    }

    // `if (argc - i != 3)` — exactly `--`, <head>, <remote> may remain. With no
    // `--` at all, `end == args.len()` and nothing remains, which also dies.
    if args.len() - end != 3 {
        eprintln!("fatal: not handling anything other than two heads merge.");
        return Ok(ExitCode::from(128));
    }

    // `repo_read_index_unmerged()` → `die_resolve_conflict("merge")`. This runs
    // *before* the two heads are resolved, so it wins over a bad ref name.
    let index = repo.index_or_empty()?;
    if index
        .entries()
        .iter()
        .any(|e| e.stage() != gix::index::entry::Stage::Unconflicted)
    {
        eprintln!("error: Merging is not possible because you have unmerged files.");
        eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }

    let branch1 = &args[end + 1];
    let branch2 = &args[end + 2];
    for branch in [branch1, branch2] {
        if repo.rev_parse_single(branch.as_str()).is_err() {
            eprintln!("fatal: could not resolve ref '{branch}'");
            return Ok(ExitCode::from(128));
        }
    }

    // merge_recursive_generic() would run here, writing the merged result to
    // the index and the worktree.
    bail!(
        "unsupported: the recursive merge itself (ported: argument validation, \
         base/head resolution, and every usage/fatal path). Applying a merge \
         result needs worktree checkout, which gitoxide only provides through \
         gix-worktree-state behind the `worktree-mutation` feature; this crate \
         builds gix with `merge` only, and gix's merge_commits/merge_trees make \
         no change to the index or worktree"
    )
}

/// `parse_merge_opt()` from `merge-recursive.c`: whether `s` (the text after
/// `--`) names a recursive-strategy option. Returns false for `return -1`,
/// which the caller turns into `fatal: unknown option --<s>`.
fn parse_merge_opt(s: &str) -> bool {
    match s {
        "ours"
        | "theirs"
        | "subtree"
        | "patience"
        | "histogram"
        | "ignore-space-change"
        | "ignore-all-space"
        | "ignore-space-at-eol"
        | "ignore-cr-at-eol"
        | "renormalize"
        | "no-renormalize"
        | "no-renames"
        | "find-renames" => true,
        _ => {
            if s.starts_with("subtree=") {
                return true;
            }
            if let Some(v) = s.strip_prefix("diff-algorithm=") {
                return parse_algorithm_value(v);
            }
            if let Some(v) = s
                .strip_prefix("find-renames=")
                .or_else(|| s.strip_prefix("rename-threshold="))
            {
                return parse_rename_score(v);
            }
            false
        }
    }
}

/// `parse_algorithm_value()` from `diff.c` — the four accepted names, matched
/// case-insensitively (`--diff-algorithm=MYERS` is accepted by git).
fn parse_algorithm_value(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "myers" | "default" | "minimal" | "patience" | "histogram"
    )
}

/// `parse_rename_score()` from `merge-recursive.c`, plus the caller's
/// `*arg != 0` check: digits with at most one `.`, optionally closed by a
/// single trailing `%` and nothing after it. An empty value is accepted (git
/// reads it as the score 0).
fn parse_rename_score(v: &str) -> bool {
    let mut seen_dot = false;
    let mut chars = v.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '.' if !seen_dot => seen_dot = true,
            // `%` ends the number; the caller rejects any trailing text.
            '%' => return chars.next().is_none(),
            c if c.is_ascii_digit() => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exactly_the_strategy_options_git_accepts() {
        // Verified one by one against git 2.55.0: each of these ran the merge,
        // rather than dying with `fatal: unknown option`.
        for ok in [
            "ours",
            "theirs",
            "subtree",
            "subtree=sub/dir",
            "patience",
            "histogram",
            "diff-algorithm=myers",
            "diff-algorithm=default",
            "diff-algorithm=MYERS",
            "diff-algorithm=minimal",
            "ignore-space-change",
            "ignore-all-space",
            "ignore-space-at-eol",
            "ignore-cr-at-eol",
            "renormalize",
            "no-renormalize",
            "no-renames",
            "find-renames",
            "find-renames=50",
            "find-renames=50%",
            "find-renames=0.5",
            "find-renames=",
            "find-renames=101",
            "rename-threshold=50",
        ] {
            assert!(parse_merge_opt(ok), "--{ok} should be accepted");
        }

        // And each of these git rejected with `fatal: unknown option`.
        for bad in [
            "minimal",            // only valid as diff-algorithm=minimal
            "no-renames=x",       // takes no value
            "verbose",            // parse-options only, not parse_merge_opt
            "quiet",
            "diff-algorithm=bogus",
            "diff-algorithm=",
            "find-renames=abc",
        ] {
            assert!(!parse_merge_opt(bad), "--{bad} should be rejected");
        }
    }

    #[test]
    fn rename_score_rejects_trailing_text_and_second_dot() {
        assert!(parse_rename_score("5.5%"));
        assert!(!parse_rename_score("50%x"));
        assert!(!parse_rename_score("5.5.5"));
        assert!(!parse_rename_score("-1"));
    }
}
