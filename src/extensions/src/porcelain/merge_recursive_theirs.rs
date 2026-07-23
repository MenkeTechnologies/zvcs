//! `git merge-recursive-theirs` — the `recursive` merge strategy back-end,
//! invoked under its `-theirs` alias name.
//!
//! `merge-recursive-theirs` is not a separate implementation: `git.c` registers
//! `merge-recursive`, `merge-recursive-ours`, `merge-recursive-theirs` and
//! `merge-subtree` as four names for the single builtin `cmd_merge_recursive`,
//! and that function only ever inspects `argv[0]` to test for a `-subtree`
//! suffix (`builtin/merge-recursive.c:34`, `ends_with(argv[0], "-subtree")`).
//! The `-theirs` suffix is **not** read, so this command is byte-for-byte
//! `git merge-recursive` with a different usage string — the "theirs"
//! *preference* is `--theirs`, an option, not the command name. Verified against
//! git 2.55.0: `merge-recursive-theirs <base> -- <ours> <theirs>` and
//! `merge-recursive <base> -- <ours> <theirs>` produced identical stdout,
//! identical exit status and an identical conflicted index (same three stage
//! entries, same blob ids).
//!
//! ### Structure: one driver, reused
//!
//! Because the `-theirs` suffix is inert, the merge proper is delegated to the
//! sibling [`super::merge_recursive`], which ports `cmd_merge_recursive`'s body
//! (`builtin/merge-recursive.c:37-91`): the option/base scan, the 20-base cap
//! warning, base/`<head>`/`<remote>` resolution, the unmerged-index
//! precondition, the three-way recursive merge (`gix` tree merge, recursive
//! merge-base consolidation), and materialising the result into the index and
//! worktree. Re-porting that here would duplicate the driver, so only the
//! command-name-specific framing lives in this file — the same three paths
//! [`super::merge_recursive_ours`] frames, with this name's usage string.
//!
//! * `-h` / `--help-all` as the *sole* argument — the usage line for **this**
//!   name on **stdout**, exit 129, answered before `setup_git_directory()` (so
//!   it works outside a repository and in a bare one).
//! * `RUN_SETUP` / `NEED_WORK_TREE`: `fatal: not a git repository …` (128) and
//!   `fatal: this operation must be run in a work tree` (128), both before any
//!   argument is examined.
//! * `argc < 4` (fewer than three arguments here): the usage line for **this**
//!   name on **stderr**, exit 129 (`usagef()`).
//!
//! Everything past that framing is handed to the driver, with a single synthetic
//! leading element prepended so its `argv`-counting grammar lines up with git's.

use anyhow::Result;
use std::process::ExitCode;

/// Stock git's usage line for this name (`builtin_merge_recursive_usage` with
/// `argv[0]` substituted), byte-for-byte — note the literal `...` after
/// `<remote>` and the absence of any trailing space.
const USAGE: &str = "usage: git merge-recursive-theirs <base>... -- <head> <remote> ...\n";

/// `git merge-recursive-theirs` — the recursive-strategy merge under its
/// `-theirs` alias.
///
/// Handles the three command-name-specific paths (`-h`, `RUN_SETUP` /
/// `NEED_WORK_TREE`, and the `argc < 4` usage line), then delegates the merge
/// itself — argument scan, resolution and the three-way merge into the index and
/// worktree — to the shared [`super::merge_recursive`] driver. The `-theirs`
/// suffix is inert (see the module docs), so this is `git merge-recursive` with
/// a different usage string.
pub fn merge_recursive_theirs(args: &[String]) -> Result<ExitCode> {
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
    forwarded.push(String::from("merge-recursive-theirs"));
    forwarded.extend_from_slice(args);
    super::merge_recursive(&forwarded)
}
