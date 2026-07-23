//! `git merge-recursive-ours` — the `recursive` merge strategy back-end,
//! invoked under its `-ours` alias name.
//!
//! `merge-recursive-ours` is not a separate implementation: `git.c` registers
//! `merge-recursive`, `merge-recursive-ours`, `merge-recursive-theirs` and
//! `merge-subtree` as four names for the single builtin `cmd_merge_recursive`,
//! and that function only ever inspects `argv[0]` to test for a `-subtree`
//! suffix (`builtin/merge-recursive.c:34`, `ends_with(argv[0], "-subtree")`).
//! The `-ours` suffix is **not** read, so this command is byte-for-byte
//! `git merge-recursive` with a different usage string — the "ours" *preference*
//! is `--ours`, an option, not the command name. Verified against git 2.55.0:
//! `merge-recursive-ours <base> -- <ours> <theirs>` and
//! `merge-recursive <base> -- <ours> <theirs>` produced identical stdout,
//! identical exit status and an identical conflicted index (same three stage
//! entries, same blob ids).
//!
//! ### Structure: one driver, reused
//!
//! Because the `-ours` suffix is inert, the merge proper is delegated to the
//! sibling [`super::merge_recursive`], which ports `cmd_merge_recursive`'s body
//! (`builtin/merge-recursive.c:37-91`): the option/base scan, the 20-base cap
//! warning, base/`<head>`/`<remote>` resolution, the unmerged-index
//! precondition, the three-way recursive merge (`gix` tree merge, recursive
//! merge-base consolidation), and materialising the result into the index and
//! worktree. Re-porting that here would duplicate the driver, so only the
//! command-name-specific framing lives in this file:
//!
//! * `-h` / `--help-all` as the *sole* argument — the usage line for **this**
//!   name on **stdout**, exit 129, answered before `setup_git_directory()` (so
//!   it works outside a repository and in a bare one).
//! * `RUN_SETUP` / `NEED_WORK_TREE`: `fatal: not a git repository …` (128) and
//!   `fatal: this operation must be run in a work tree` (128), both before any
//!   argument is examined. `merge_recursive` reports repo-discovery failure as a
//!   generic error, so these faithful messages are emitted here first.
//! * `argc < 4` (fewer than three arguments here): the usage line for **this**
//!   name on **stderr**, exit 129 (`usagef()`).
//!
//! Everything past that framing is handed to the driver. `git.c`'s dispatch
//! strips `argv[0]` before this function sees `args`, but `cmd_merge_recursive`
//! (and its Rust port) indexes `argv` starting at `1` with `argc` counting the
//! command name; a single synthetic leading element is prepended so the driver's
//! argument grammar lines up exactly with git's.

use anyhow::Result;
use std::process::ExitCode;

/// Stock git's usage line for this name (`builtin_merge_recursive_usage` with
/// `argv[0]` substituted), byte-for-byte — note the literal `...` after
/// `<remote>` and the absence of any trailing space.
const USAGE: &str = "usage: git merge-recursive-ours <base>... -- <head> <remote> ...\n";

/// `git merge-recursive-ours` — the recursive-strategy merge under its `-ours`
/// alias.
///
/// Handles the three command-name-specific paths (`-h`, `RUN_SETUP` /
/// `NEED_WORK_TREE`, and the `argc < 4` usage line), then delegates the merge
/// itself — argument scan, resolution and the three-way merge into the index and
/// worktree — to the shared [`super::merge_recursive`] driver. The `-ours`
/// suffix is inert (see the module docs), so this is `git merge-recursive` with
/// a different usage string.
pub fn merge_recursive_ours(args: &[String]) -> Result<ExitCode> {
    // `git.c` answers a lone `-h`/`--help-all` on stdout before RUN_SETUP, so
    // it works outside a repository and in a bare one.
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // RUN_SETUP, then NEED_WORK_TREE — both before any argument is looked at,
    // and with git's exact messages (the driver reports discovery failure as a
    // generic error, so the faithful text is produced here).
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }

    // `if (argc < 4) usagef(...)` — argc counts argv[0], which `args` omits, so
    // git's `< 4` is `< 3` here. Emit this command's usage line, not the
    // driver's `merge-recursive` one.
    if args.len() < 3 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // Delegate to the shared driver. `cmd_merge_recursive` (and its port) counts
    // `argv[0]` and scans from index 1; dispatch stripped the command name, so
    // prepend one synthetic element to realign the grammar. Its value is never
    // read (the port's scan starts at index 1), and `args.len() >= 3` here means
    // the forwarded slice is `>= 4` long, so the driver's own `< 4` usage path —
    // which would print the wrong command name — is never reached.
    let mut forwarded = Vec::with_capacity(args.len() + 1);
    forwarded.push(String::from("merge-recursive-ours"));
    forwarded.extend_from_slice(args);
    super::merge_recursive(&forwarded)
}

/// `parse_merge_opt()` from `merge-recursive.c`: whether `s` (the text after
/// `--`) names a recursive-strategy option. Retained as a spec-lock of git
/// 2.55.0's accepted option set; the live parser is the shared driver's
/// [`super::merge_recursive`], so this is compiled only under test.
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
