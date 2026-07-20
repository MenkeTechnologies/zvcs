//! `git merge-recursive-theirs` — the recursive merge back-end, invoked under its
//! `-theirs` alias name.
//!
//! `merge-recursive-theirs` is not a shell driver and not a distinct strategy: it
//! is one of the alias entries in git's command table that all route to
//! `cmd_merge_recursive` in `builtin/merge-recursive.c`. Only the `-subtree`
//! suffix is inspected there (`ends_with(argv[0], "-subtree")` sets
//! `subtree_shift`); the `-ours` and `-theirs` suffixes are **inert**. Verified
//! against git 2.55: `git merge-recursive-theirs <base> -- <ours> <theirs>` and
//! `git merge-recursive <base> -- <ours> <theirs>` produce identical stdout,
//! identical exit code, and an identical conflicted worktree — the "theirs"
//! favouring is *not* applied. The only argv-dependent output is the usage line,
//! which echoes the invoked name.
//!
//! ### What this port covers, byte-identically (verified against git 2.55)
//!
//! Every path up to the point where the merge itself would run:
//!
//! * Setup, in git's order (`RUN_SETUP | NEED_WORK_TREE` precede the builtin):
//!   `fatal: not a git repository (or any of the parent directories): .git` and
//!   `fatal: this operation must be run in a work tree`, both exit 128.
//! * `-h` or `--help-all` as the *sole* argument: the usage line on **stdout**,
//!   exit 129, printed before repository setup (so it works outside a repo).
//!   With any other argument present, `-h` is just a rev and fails to parse.
//! * Fewer than three arguments: the usage line on **stderr**, exit 129.
//! * `--<opt>` parsing with git's exact accepted set (see [`parse_merge_opt`]);
//!   anything else is `fatal: unknown option --<opt>`, exit 128.
//! * `--` ends option/base scanning. Bases are resolved *while* scanning, so a
//!   bad base is reported before the arity check.
//! * A base that does not resolve: `fatal: could not parse object '<arg>'`, 128.
//! * More than 20 bases: `warning: cannot handle more than 20 bases. Ignoring
//!   <arg>.` on stderr, once per surplus base, and the base is dropped.
//! * Not exactly two arguments after `--` (including no `--` at all):
//!   `fatal: not handling anything other than two heads merge.`, exit 128.
//! * An index carrying unmerged stages, checked *after* the arity check and
//!   *before* ref resolution — `die_resolve_conflict("merge")`:
//!   `error: Merging is not possible because you have unmerged files.` plus the
//!   two `hint:` lines plus `fatal: Exiting because of an unresolved conflict.`,
//!   exit 128.
//! * `<head>`/`<remote>` that do not resolve:
//!   `fatal: could not resolve ref '<arg>'`, exit 128.
//! * `better_branch_name()`: an argument whose length equals the hex length of
//!   the repository's hash is looked up as `$GITHEAD_<arg>`, and the value (when
//!   set) replaces it as the merge label. This is what makes `git merge` produce
//!   `<<<<<<< <branch>` rather than `<<<<<<< <oid>` markers.
//!
//! ### What this port does not do
//!
//! The merge itself. Once the operands are validated this bails; it never
//! partially mutates the index or the worktree.
//!
//! `merge_recursive_generic()` performs a three-way tree merge and then writes
//! the result *into the live index and worktree*: conflicted files are
//! materialised on disk with conflict markers labelled by `branch1`/`branch2`,
//! the index gains stage 1/2/3 entries, unmerged-away paths are unlinked,
//! rename/delete and directory-rename conflicts get renamed-aside copies, and
//! the whole update is guarded so that dirty worktree files are never
//! overwritten. It also emits an ordered message stream on stdout
//! (`Auto-merging <p>`, `CONFLICT (content): Merge conflict in <p>`,
//! `CONFLICT (rename/delete): ...`, `Adding as <p>~<branch> instead`,
//! `Removing <p>`, `Skipped <p> (merged same as existing)`, …) whose exact text
//! and ordering the exit code (0 clean / 1 conflicted) is derived from.
//!
//! The vendored gitoxide has the tree-merge half only: `gix_merge::tree` /
//! `Repository::merge_trees` / `Repository::merge_commits` merge trees into the
//! object database and return `gix_merge::tree::Conflict` values, and
//! `gix_merge::tree::apply_index_entries` can stamp conflict stages into an
//! in-memory `gix_index::State`. Missing substrate: a merge-aware worktree
//! updater (nothing writes conflict-marked blobs to disk, unlinks removed paths,
//! or refuses to clobber local modifications), and any mapping from
//! `gix_merge::tree::Conflict` to merge-recursive's message text and emission
//! order. Synthesising either would produce output that diverges from git while
//! looking plausible, so it is not attempted here.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// `builtin_merge_recursive_usage`, with `%s` filled in by the invoked name.
const USAGE: &str = "usage: git merge-recursive-theirs <base>... -- <head> <remote> ...\n";

/// `ARRAY_SIZE(bases) - 1` in `cmd_merge_recursive` — 21 slots, 20 usable.
const MAX_BASES: usize = 20;

/// `git merge-recursive-theirs` — validate the operands of a recursive merge.
///
/// Returns git's exit code for every diagnostic path. The merge proper is not
/// implemented; see the module docs for the missing substrate.
pub fn merge_recursive_theirs(args: &[String]) -> Result<ExitCode> {
    // `handle_builtin()` answers `-h`/`--help-all` for NO_PARSEOPT commands
    // before `RUN_SETUP`, and only when it is the sole argument — so this works
    // outside a repository, and `-h` among real operands is just a bad rev.
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // RUN_SETUP | NEED_WORK_TREE, both before the builtin body.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }

    // `if (argc < 4) usagef(...)` — argc counts argv[0], `args` does not.
    if args.len() < 3 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // Scan options and bases until `--`. `starts_with(arg, "--")` claims the
    // argument, so a lone `-x` falls through to the rev parser.
    let mut bases: Vec<gix::ObjectId> = Vec::new();
    let mut end = args.len(); // index just past the scan, i.e. of `--` if present
    for (i, arg) in args.iter().enumerate() {
        if let Some(opt) = arg.strip_prefix("--") {
            if opt.is_empty() {
                end = i;
                break;
            }
            if !parse_merge_opt(opt) {
                eprintln!("fatal: unknown option {arg}");
                return Ok(ExitCode::from(128));
            }
            continue;
        }
        let Ok(id) = repo.rev_parse_single(arg.as_str()) else {
            eprintln!("fatal: could not parse object '{arg}'");
            return Ok(ExitCode::from(128));
        };
        if bases.len() < MAX_BASES {
            bases.push(id.detach());
        } else {
            eprintln!("warning: cannot handle more than {MAX_BASES} bases. Ignoring {arg}.");
        }
    }

    // `if (argc - i != 3)` — exactly `--`, `<head>`, `<remote>` must remain.
    // With no `--` the scan ran off the end and this trips too.
    if args.len() - end != 3 {
        eprintln!("fatal: not handling anything other than two heads merge.");
        return Ok(ExitCode::from(128));
    }

    // `repo_read_index_unmerged()` → `die_resolve_conflict("merge")`.
    // Ordered before ref resolution, exactly as in the C.
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
    let mut heads = Vec::with_capacity(2);
    for spec in [branch1, branch2] {
        let Ok(id) = repo.rev_parse_single(spec.as_str()) else {
            eprintln!("fatal: could not resolve ref '{spec}'");
            return Ok(ExitCode::from(128));
        };
        heads.push(id.detach());
    }

    // better_branch_name(): a raw object id is replaced by $GITHEAD_<oid> when
    // set, which is how `git merge` labels the conflict markers.
    let hexsz = repo.object_hash().len_in_hex();
    let (label1, label2) = (
        better_branch_name(branch1, hexsz),
        better_branch_name(branch2, hexsz),
    );
    let _ = (&bases, &heads, &label1, &label2);

    bail!(
        "merge-recursive-theirs: the three-way merge is unsupported \
         (ported: operand parsing, warnings and every fatal path; \
         missing substrate: gitoxide merges trees into the object database only \
         — there is no merge-aware worktree/index updater to materialise \
         conflict-marked files, and no mapping from gix_merge::tree::Conflict to \
         merge-recursive's message stream)"
    );
}

/// `parse_merge_opt()` from `merge-recursive.c`, reduced to accept/reject.
///
/// The option values are not retained because the merge does not run; only the
/// accept/reject decision is observable, and it must match git exactly since a
/// rejection is a `fatal: unknown option` with exit 128.
///
/// `s` is the argument with its leading `--` already stripped.
fn parse_merge_opt(s: &str) -> bool {
    match s {
        "ours" | "theirs" | "subtree" => return true,
        "patience" | "histogram" => return true,
        "ignore-space-change" | "ignore-all-space" | "ignore-space-at-eol"
        | "ignore-cr-at-eol" => return true,
        "renormalize" | "no-renormalize" => return true,
        "no-renames" | "find-renames" => return true,
        _ => {}
    }
    if s.strip_prefix("subtree=").is_some() {
        return true;
    }
    if let Some(name) = s.strip_prefix("diff-algorithm=") {
        // parse_diff_algorithm(): the four named algorithms, nothing else.
        return matches!(name, "myers" | "minimal" | "patience" | "histogram");
    }
    if let Some(score) = s
        .strip_prefix("find-renames=")
        .or_else(|| s.strip_prefix("rename-threshold="))
    {
        // `(o->rename_score = parse_rename_score(&arg)) == -1 || *arg` — the
        // score itself never fails, so acceptance is purely "was it all consumed".
        return rename_score_consumes_all(score);
    }
    false
}

/// Whether `parse_rename_score()` would consume all of `s`.
///
/// Its scanner takes digits, at most one `.` (the second one stops it), and a
/// single `%` which ends the scan; anything left over makes `parse_merge_opt`
/// reject the option. An empty value is accepted, matching git.
fn rename_score_consumes_all(s: &str) -> bool {
    let mut dot = false;
    let mut it = s.chars();
    for c in it.by_ref() {
        match c {
            '.' if !dot => dot = true,
            '0'..='9' => {}
            // '%' is always the end of the score; whatever follows is leftover.
            '%' => return it.as_str().is_empty(),
            _ => return false,
        }
    }
    true
}

/// `better_branch_name()`: swap a bare object id for `$GITHEAD_<id>` when that
/// environment variable is set, otherwise keep the argument as given.
fn better_branch_name(branch: &str, hexsz: usize) -> String {
    if branch.len() != hexsz {
        return branch.to_string();
    }
    std::env::var(format!("GITHEAD_{branch}")).unwrap_or_else(|_| branch.to_string())
}
