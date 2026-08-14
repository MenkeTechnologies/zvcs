//! `git stash` — save the dirty worktree/index to `refs/stash` and restore it.
//!
//! Ported onto the vendored gitoxide. The stash object model mirrors stock git
//! exactly: a stash is a merge commit `W` whose tree is the *worktree* snapshot,
//! whose first parent is `HEAD` (the base) and whose second parent is an *index*
//! commit `I` whose tree is the staged snapshot. Entries are tracked as reflog
//! lines on `refs/stash`, newest first (`stash@{0}`).
//!
//! ### What is implemented faithfully
//!
//! * `push` (the default, plus explicit `push` / `save`, with `-m/--message`):
//!   builds `I` and `W`, appends the `refs/stash` reflog entry, and resets the
//!   tracked worktree + index back to `HEAD`. Untracked files are left in place
//!   unless `-u`/`-a` asks for them. `save` is the same command with the message
//!   taken from the positional words (`save_stash()` calls the same
//!   `do_push_stash()`), so every option `push` takes works there too.
//! * Pathspec-limited pushes (`-- <pathspec>…`, `--pathspec-from-file`,
//!   `--pathspec-file-nul`), over the same engine as `ls-files`/`grep`, so
//!   `:(glob)`/`:(icase)`/`:!` behave identically. A pathspec narrows the
//!   worktree tree `W` and the set of paths reset — never the index commit `I`,
//!   which git always captures whole, so a staged change to an unmatched path
//!   rides along in the stash and stays staged afterwards.
//! * `-k/--keep-index`, which resets to `I` instead of `HEAD` and so leaves the
//!   staged state staged with its content on disk.
//! * `-S/--staged`, taking the index diff alone and leaving unstaged work. The
//!   reset then subtracts the staged diff from the *worktree* — git's
//!   `git apply -R` of that patch — so an unstaged edit elsewhere in the same
//!   file survives, and an edit that overlaps the staged one makes the whole
//!   reset fail (`Cannot remove worktree changes`, exit 1) with the stash entry
//!   kept, since `apply` is all-or-nothing. Reverting a staged *deletion* means
//!   writing the file back, so a path still sitting in the worktree — what
//!   `git rm --cached` leaves — is reported as
//!   `<path>: already exists in working directory` and stops the reset the same
//!   way.
//! * `-u/--include-untracked` and `-a/--all`, capturing untracked (and for
//!   `-a`, ignored) files into a parentless third parent and deleting them from
//!   the worktree, along with the directories they leave empty (git's
//!   `clean --force -d`). Refused together with `-S`, as git refuses it.
//! * `list`  — one `stash@{N}: <message>` line per reflog entry, newest first,
//!   over `log -g --first-parent` as `list_stash()` runs it, so the diff options
//!   the reflog port renders (`--name-only`, `--name-status`, `--numstat`,
//!   `--shortstat`, `--summary`, `--raw`) describe each entry's own change.
//!   `-p` and `--stat` render each entry's patch and histogram through the same
//!   machinery `log -p`/`diff --stat` use; `--dirstat`/`--patch-with-stat` are
//!   refused there rather than silently dropped.
//! * `pop` / `apply` — a three-way merge of the stash onto the current tree
//!   (base: the stash's own base, ours: the current index, theirs: `W`), so a
//!   moved `HEAD` or unrelated local work is fine; a conflict leaves the markers
//!   and the unmerged index in place, keeps the entry and exits 1. Untracked
//!   files captured in the third parent come back untracked and never overwrite
//!   a file already sitting there; `pop` then drops the entry.
//!   `--index` first replays the stash's staged state onto the current index
//!   (`apply_cached()`); when that cannot be done the command stops there with
//!   `conflicts in index. Try without --index.` and nothing on disk has moved.
//!   The index afterwards is `unstage_changes_unless_new()`: everything the
//!   merge staged is unstaged again *except* paths that did not exist in the
//!   pre-apply index, which stay staged — so a stash that had `git add`ed a new
//!   file restores it as `A `, not as untracked. `--index` (or `stash.index=true`,
//!   which is exactly that option's default) instead restores the stash's own
//!   staged tree; `--no-index` countermands the config. `--label-ours`/
//!   `--label-theirs`/`--label-base` name the conflict sides. Both then run
//!   `git status` unless `-q`/`--quiet` was given, which also silences `pop`'s
//!   `Dropped …` line.
//! * `drop` / `clear` — remove one / all entries, rewriting the reflog exactly
//!   like `git reflog delete --rewrite --updateref`. The ref itself moves (or is
//!   deleted) through the ref store, so a `refs/stash` that lives in
//!   `packed-refs` is removed rather than left resolving to a dropped stash.
//!   Both parse their argv first: `drop` takes only `OPT__QUIET`, and `clear`
//!   takes nothing at all (`PARSE_OPT_STOP_AT_NON_OPTION` over an empty table,
//!   so an operand is `error: git stash clear with arguments is unimplemented`
//!   at 1). An unrecognized option is refused at 129 *before* an entry is
//!   removed — `git stash drop --zzbogus` must not drop, and
//!   `git stash clear --zzbogus` must not erase everything.
//! * `<stash>` resolution is `get_stash_info()`: `stash@{n}` (or a bare `n`) is a
//!   reflog entry, and anything else is any stash-like commit — two parents at
//!   least — which `apply`, `show` and `branch` take as readily as git does.
//!   `drop` and `pop` rewrite the reflog, so `assert_stash_ref()` keeps them to
//!   an entry of it. An entry past the end is `rev-parse`'s
//!   `fatal: log for 'stash' only has <n> entries`.
//! * `create` — build the `I`/`W` commit graph and print the `W` id without
//!   storing it or touching the worktree (`do_create_stash`).
//! * `store` — point `refs/stash` at an existing stash-like commit, appending
//!   the reflog entry (`do_store_stash`).
//! * `show` — diff the stash base tree against its worktree tree, formatted per
//!   the diff options / `stash.showStat`+`stash.showPatch` config; rendering is
//!   delegated to the `diff` porcelain (git's own `diff_tree_oid` machinery).
//!   `-u`/`--only-untracked` (and `stash.showIncludeUntracked`, which seeds them
//!   before the options are read) bring the untracked `^3` tree into the diff.
//! * `branch` — create+check out a branch at the stash base, apply there, drop.
//! * `create_autostash` / `apply_autostash` — the rebase/pull `--autostash`
//!   helpers. Unlike `apply`/`pop`, the re-apply here IS a real three-way merge
//!   (`merge_apply::three_way_merge`) of the stash onto the moved `HEAD`, since
//!   autostash by definition re-applies over a tree the rebase/merge advanced.
//!
//! `-p/--patch` is `stash_patch()`: the hunk selector (`super::add_patch`) runs
//! against a scratch index at `$GIT_DIR/index.stash.<pid>` seeded from `HEAD` and
//! exported to its children as `GIT_INDEX_FILE`, so the chosen hunks land there
//! and the real index is untouched. That index becomes the stash's `W` tree, and
//! the `diff-tree -p -U1 HEAD <W>` it amounts to is reversed out of the worktree
//! with `git apply -R` — which is how an unselected edit in the same file survives
//! the push. `--patch` defaults `--keep-index` on, overrides `--staged`, refuses
//! `-u`/`-a` and `--pathspec-from-file`, and takes `-U<n>`,
//! `--inter-hunk-context=<n>` and `--[no-]auto-advance` (which every other mode
//! refuses with `the option '<opt>' requires '--patch'`).
//!
//! ### Honest boundaries (precise bail, never fake success)
//!
//! * `stash.index` is honored for `apply`/`pop` (and `branch`, which git always
//!   applies with the index). git also lets it reach the `--autostash` re-apply
//!   of `merge`/`rebase`/`pull`; that path always behaves as `--no-index` here,
//!   because restoring the staged state across a *moved* `HEAD` needs a second
//!   three-way merge of the index tree that is not ported.
//! * Content blobs are produced through the repo filter pipeline, so CRLF /
//!   clean filters are honored just like git.
//! * What the object database is left holding matches git, which is not the same as
//!   what the emulation needs: `-S` computes its reset by merging a tree that stands
//!   for the worktree, and `apply --index` answers "would this patch apply?" the same
//!   way. git writes neither (it shells out to `git apply -R` / `--check`), so both go
//!   through a memory-backed object handle and nothing reaches disk. The `reset --hard`
//!   reflog line follows git too: only the pathspec-less, non-`-S` push runs that reset,
//!   and `-u` always writes its untracked parent — with the empty tree when the pathspec
//!   left nothing to collect.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::index::ChangeRef;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::objs::tree::EntryKind;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

/// The usage block `parse_options()` prints for `git stash push`, verbatim
/// from `git stash -h` on 2.50.1 — an unknown flag is answered with this and
/// exit 129, which is what scripts test for.
const PUSH_USAGE: &str = r#"usage: git stash [push] [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]
                 [-u | --include-untracked] [-a | --all] [(-m | --message) <message>]
                 [--pathspec-from-file=<file> [--pathspec-file-nul]]
                 [--] [<pathspec>...]

    -k, --[no-]keep-index keep index
    -S, --[no-]staged     stash staged changes only
    -p, --[no-]patch      stash in patch mode
    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -q, --[no-]quiet      quiet mode
    -u, --[no-]include-untracked
                          include untracked files in stash
    -a, --[no-]all        include ignore files
    -m, --[no-]message <message>
                          stash message
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

"#;

/// The command's own usage, which `git stash -h` writes to **stdout** before
/// exiting 129 — every subcommand form, not just `push`'s.
///
/// `export`/`import` are listed because git lists them; asking for either still
/// gets this port's "is not a stash command", as it does for any other word.
const STASH_USAGE: &str = r#"usage: git stash list [<log-options>]
   or: git stash show [-u | --include-untracked | --only-untracked] [<diff-options>] [<stash>]
   or: git stash drop [-q | --quiet] [<stash>]
   or: git stash pop [--index] [-q | --quiet] [<stash>]
   or: git stash apply [--index] [-q | --quiet] [--label-ours=<label>] [--label-theirs=<label>] [--label-base=<label>] [<stash>]
   or: git stash branch <branchname> [<stash>]
   or: git stash [push] [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]
                 [-u | --include-untracked] [-a | --all] [(-m | --message) <message>]
                 [--pathspec-from-file=<file> [--pathspec-file-nul]]
                 [--] [<pathspec>...]
   or: git stash save [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]
                 [-u | --include-untracked] [-a | --all] [<message>]
   or: git stash clear
   or: git stash create [<message>]
   or: git stash store [(-m | --message) <message>] [-q | --quiet] <commit>
   or: git stash export (--print | --to-ref <ref>) [<stash>...]
   or: git stash import <commit>

"#;

/// `git stash save -h`: the same option table as `push`, minus the pathspec
/// options `save` does not take.
/// The block a *push-assumed* failure renders (1978 bytes, git 2.55.0).
///
/// `git stash <flag>` with no sub-command re-enters `push_stash()` with
/// `push_assumed` set, and that call renders `git_stash_usage` — the same usage
/// lines `-h` prints — against the *push* option table, so the block carries
/// push's twelve options where the `-h` block carries none (`cmd_stash`'s table
/// is `OPT_SUBCOMMAND`s only). Same lines, different option list, 883 bytes
/// apart: `git stash --bogus` and `git stash -h` do not print the same thing.
const STASH_PUSH_USAGE: &str = r#"usage: git stash list [<log-options>]
   or: git stash show [-u | --include-untracked | --only-untracked] [<diff-options>] [<stash>]
   or: git stash drop [-q | --quiet] [<stash>]
   or: git stash pop [--index] [-q | --quiet] [<stash>]
   or: git stash apply [--index] [-q | --quiet] [--label-ours=<label>] [--label-theirs=<label>] [--label-base=<label>] [<stash>]
   or: git stash branch <branchname> [<stash>]
   or: git stash [push] [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]
                 [-u | --include-untracked] [-a | --all] [(-m | --message) <message>]
                 [--pathspec-from-file=<file> [--pathspec-file-nul]]
                 [--] [<pathspec>...]
   or: git stash save [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]
                 [-u | --include-untracked] [-a | --all] [<message>]
   or: git stash clear
   or: git stash create [<message>]
   or: git stash store [(-m | --message) <message>] [-q | --quiet] <commit>
   or: git stash export (--print | --to-ref <ref>) [<stash>...]
   or: git stash import <commit>

    -k, --[no-]keep-index keep index
    -S, --[no-]staged     stash staged changes only
    -p, --[no-]patch      stash in patch mode
    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -q, --[no-]quiet      quiet mode
    -u, --[no-]include-untracked
                          include untracked files in stash
    -a, --[no-]all        include ignore files
    -m, --[no-]message <message>
                          stash message
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

"#;

const SAVE_USAGE: &str = r#"usage: git stash save [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]
                 [-u | --include-untracked] [-a | --all] [<message>]

    -k, --[no-]keep-index keep index
    -S, --[no-]staged     stash staged changes only
    -p, --[no-]patch      stash in patch mode
    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -q, --[no-]quiet      quiet mode
    -u, --[no-]include-untracked
                          include untracked files in stash
    -a, --[no-]all        include ignore files
    -m, --[no-]message <message>
                          stash message

"#;

/// `git stash apply -h`.
const APPLY_USAGE: &str = r#"usage: git stash apply [--index] [-q | --quiet] [--label-ours=<label>] [--label-theirs=<label>] [--label-base=<label>] [<stash>]

    -q, --[no-]quiet      be quiet, only report errors
    --[no-]index          attempt to recreate the index
    --[no-]label-ours <label>
                          label for the upstream side in conflict markers
    --[no-]label-theirs <label>
                          label for the stashed side in conflict markers
    --[no-]label-base <label>
                          label for the base in diff3 conflict markers

"#;

/// `git stash pop -h`, which has no conflict-label options.
const POP_USAGE: &str = r#"usage: git stash pop [--index] [-q | --quiet] [<stash>]

    -q, --[no-]quiet      be quiet, only report errors
    --[no-]index          attempt to recreate the index

"#;

/// `git stash drop -h`.
const DROP_USAGE: &str = r#"usage: git stash drop [-q | --quiet] [<stash>]

    -q, --[no-]quiet      be quiet, only report errors

"#;

/// `git stash clear -h`. `clear_stash()`'s option table is just `OPT_END()`, so
/// the block is the usage line alone.
const CLEAR_USAGE: &str = "usage: git stash clear\n\n";

/// `git stash show -h`.
const SHOW_USAGE: &str = r#"usage: git stash show [-u | --include-untracked | --only-untracked] [<diff-options>] [<stash>]

    -u, --[no-]include-untracked
                          include untracked files in the stash
    --only-untracked      only show untracked files in the stash

"#;

/// `git stash branch -h`, whose option table is empty.
const BRANCH_USAGE: &str = r#"usage: git stash branch <branchname> [<stash>]

"#;

/// `git stash store -h`.
const STORE_USAGE: &str = r#"usage: git stash store [(-m | --message) <message>] [-q | --quiet] <commit>

    -q, --[no-]quiet      be quiet
    -m, --[no-]message <message>
                          stash message

"#;

/// `git stash export -h` over `export_stash()`'s table (builtin/stash.c:2431-2438).
/// `--print` is an `OPT_CMDMODE`, which is why it alone has no `--[no-]` row.
const EXPORT_USAGE: &str = "\
usage: git stash export (--print | --to-ref <ref>) [<stash>...]

    --print               print the object ID instead of writing it to a ref
    --[no-]to-ref <ref>   save the data to the given ref

";

/// `git stash import -h`. `import_stash()`'s table is `OPT_END()` alone.
const IMPORT_USAGE: &str = "\
usage: git stash import <commit>

";

/// `git stash list -h`, which forwards everything else to `git log`.
const LIST_USAGE: &str = r#"usage: git stash list [<log-options>]

"#;

// ---------------------------------------------------------------------------
// Option tables — one per `struct option options[]` in builtin/stash.c.
//
// `stash` is the command where a single shared table would be wrong in both
// directions: `--label-ours` resolves under `apply` and is unknown under `pop`,
// `--pathspec-from-file` resolves under `push` and is unknown under `save`, and
// `--index` resolves under both `apply` and `pop` but under nothing else. Each
// table below is its own sub-command's, in the C's declaration order.
//
// Five of the thirteen are deliberately absent, all for the same reason:
// `register_abbrev()` returns without recording anything when
// `PARSE_OPT_KEEP_UNKNOWN_OPT` is set (parse-options.c:502-503), so under that
// flag a name matches exactly or not at all and there is nothing for a resolver
// to do. `cmd_stash`'s own table (`OPT_SUBCOMMAND`s), `list_stash()`,
// `store_stash()` and `show_stash()` all parse that way — `git stash show
// --no-only-untracked` is not an unknown option in stock git, it is forwarded
// verbatim to `setup_revisions()`. `create_stash()` has no table at all.
// ---------------------------------------------------------------------------

use super::{Arg, LongOpt};

/// `push_stash()`'s table (builtin/stash.c:1917-1938). It is also the table the
/// bare `git stash <flag>` form parses against, via the push-assumed re-entry.
pub(super) const PUSH_OPTS: &[LongOpt] = &[
    LongOpt { name: "keep-index", neg: true, arg: Arg::None },
    LongOpt { name: "staged", neg: true, arg: Arg::None },
    LongOpt { name: "patch", neg: true, arg: Arg::None },
    LongOpt { name: "auto-advance", neg: true, arg: Arg::None },
    // `OPT_DIFF_UNIFIED` / `OPT_DIFF_INTERHUNK_CONTEXT` are
    // `OPT_INTEGER_F(..., PARSE_OPT_NONEG)` (parse-options.h:627-628).
    LongOpt { name: "unified", neg: false, arg: Arg::Required },
    LongOpt { name: "inter-hunk-context", neg: false, arg: Arg::Required },
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
    LongOpt { name: "include-untracked", neg: true, arg: Arg::None },
    LongOpt { name: "all", neg: true, arg: Arg::None },
    LongOpt { name: "message", neg: true, arg: Arg::Required },
    LongOpt { name: "pathspec-from-file", neg: true, arg: Arg::Required },
    LongOpt { name: "pathspec-file-nul", neg: true, arg: Arg::None },
];

/// `save_stash()`'s table (builtin/stash.c:2024-2043): `push`'s first ten
/// entries. The two pathspec options are `push`-only, so `git stash save
/// --pathspec-from-file=x` is an unknown option where `push` accepts it.
pub(super) const SAVE_OPTS: &[LongOpt] = PUSH_OPTS.split_at(10).0;

/// `apply_stash()`'s table (builtin/stash.c:780-791).
pub(super) const APPLY_OPTS: &[LongOpt] = &[
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
    LongOpt { name: "index", neg: true, arg: Arg::None },
    LongOpt { name: "label-ours", neg: true, arg: Arg::Required },
    LongOpt { name: "label-theirs", neg: true, arg: Arg::Required },
    LongOpt { name: "label-base", neg: true, arg: Arg::Required },
];

/// `pop_stash()`'s table (builtin/stash.c:886-891): `apply`'s first two. The
/// three conflict-label options belong to `apply` alone.
pub(super) const POP_OPTS: &[LongOpt] = APPLY_OPTS.split_at(2).0;

/// `drop_stash()`'s table (builtin/stash.c:862-865): `OPT__QUIET` alone.
pub(super) const DROP_OPTS: &[LongOpt] = APPLY_OPTS.split_at(1).0;

/// `export_stash()`'s table (builtin/stash.c:2431-2438). `--print` is an
/// `OPT_CMDMODE`, i.e. `OPT_SET_INT_F(..., PARSE_OPT_NONEG)`, so it has no
/// `--no-` sense; `--to-ref` is a plain `OPT_STRING` and does.
const EXPORT_OPTS: &[LongOpt] = &[
    LongOpt { name: "print", neg: false, arg: Arg::None },
    LongOpt { name: "to-ref", neg: true, arg: Arg::Required },
];

/// `clear_stash()` (:319-321), `branch_stash()` (:918-920) and `import_stash()`
/// (:2276-2278) each declare `OPT_END()` and nothing else, so no long option
/// resolves and every `--x` is unknown.
const NO_OPTS: &[LongOpt] = &[];

/// Respell one token the way `parse_long_opt()` reads it against `table`, or
/// hand back the already-rendered ambiguity refusal.
fn canonical<'a>(
    tok: &'a str,
    table: &'static [LongOpt],
    usage: &str,
) -> std::result::Result<std::borrow::Cow<'a, str>, ExitCode> {
    match super::canonical_long(tok, table) {
        super::Long::Name(name) => Ok(name),
        super::Long::Ambiguous(first, second) => {
            Err(super::ambiguous_option(tok, &first, &second, usage))
        }
    }
}

/// `parse_options()`' built-in `-h`: the usage block on stdout, exit 129. It
/// fires wherever the flag appears in the subcommand's own arguments.
///
/// `--help-all` is the same block. `parse_options_step()` tests it with a
/// `strcmp()` of its own, ahead of `parse_long_opt()`, so the name never
/// abbreviates and never takes an `=<value>`; and because that test sits inside
/// the argv loop *after* the `--` and `--end-of-options` breaks, it is not seen
/// past either terminator — hence the `take_while`. None of stash's option
/// tables has a `PARSE_OPT_HIDDEN` entry, so its `USAGE_FULL` is this block.
fn usage_requested(args: &[String], usage: &str) -> Option<ExitCode> {
    let help_all = args
        .iter()
        .take_while(|a| a.as_str() != "--" && a.as_str() != "--end-of-options")
        .any(|a| a == "--help-all");
    (help_all || args.iter().any(|a| a == "-h")).then(|| {
        print!("{usage}");
        ExitCode::from(129)
    })
}

pub fn stash(args: &[String]) -> Result<ExitCode> {
    // `-h` is answered before the repository is even looked for, which is why it
    // works outside one.
    if args.first().is_none_or(|a| a.starts_with('-')) {
        if let Some(code) = usage_requested(args, STASH_USAGE) {
            return Ok(code);
        }
    }

    // `git stash` writes commits, but not under the strict identity check —
    // it saves fine with `user.useConfigOnly` set and nothing configured.
    let mut repo = gix::discover(".")?;
    crate::ensure_reflog_identity(&mut repo);

    match args.first().map(String::as_str) {
        None => {
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, &PushOpts::with_message(None))
        }
        Some("push") => {
            if let Some(code) = usage_requested(&args[1..], PUSH_USAGE) {
                return Ok(code);
            }
            let opts = match parse_push_options(&args[1..], PUSH_USAGE)? {
                Ok(o) => o,
                Err(code) => return Ok(code),
            };
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, &opts)
        }
        // `save` is `push` with the message taken from the positional words:
        // git's `save_stash()` builds the same `do_push_stash()` call, so every
        // option `push` implements is available here too.
        Some("save") => {
            if let Some(code) = usage_requested(&args[1..], SAVE_USAGE) {
                return Ok(code);
            }
            let opts = match parse_save_options(&args[1..])? {
                Ok(o) => o,
                Err(code) => return Ok(code),
            };
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, &opts)
        }
        Some("list") => {
            if let Some(code) = usage_requested(&args[1..], LIST_USAGE) {
                return Ok(code);
            }
            list(&repo, &args[1..])
        }
        Some("pop") => {
            if let Some(code) = usage_requested(&args[1..], POP_USAGE) {
                return Ok(code);
            }
            let opts = match parse_apply_options(&repo, &args[1..], POP_OPTS, POP_USAGE)? {
                Ok(o) => o,
                Err(code) => return Ok(code),
            };
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            apply_or_pop(&repo, &opts, true)
        }
        Some("apply") => {
            if let Some(code) = usage_requested(&args[1..], APPLY_USAGE) {
                return Ok(code);
            }
            let opts = match parse_apply_options(&repo, &args[1..], APPLY_OPTS, APPLY_USAGE)? {
                Ok(o) => o,
                Err(code) => return Ok(code),
            };
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            apply_or_pop(&repo, &opts, false)
        }
        Some("drop") => {
            if let Some(code) = usage_requested(&args[1..], DROP_USAGE) {
                return Ok(code);
            }
            let (quiet, named) = match parse_drop_options(&args[1..]) {
                Ok(parsed) => parsed,
                Err(code) => return Ok(code),
            };
            let spec = match resolve_stash(&repo, &named)? {
                Ok(spec) => spec,
                Err(code) => return Ok(code),
            };
            if let Some(code) = require_stash_ref(&spec) {
                return Ok(code);
            }
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            do_drop_stash(&repo, &spec, quiet)
        }
        Some("clear") => {
            // ```c
            // static int clear_stash(int argc, const char **argv, const char *prefix)
            // {
            //         struct option options[] = { OPT_END() };
            //         argc = parse_options(argc, argv, prefix, options,
            //                              git_stash_clear_usage,
            //                              PARSE_OPT_STOP_AT_NON_OPTION);
            //         if (argc)
            //                 return error(_("git stash clear with arguments is unimplemented"));
            //         return do_clear_stash();
            // }
            // ```
            //
            // The table is empty, so *every* option is unknown; the first
            // operand stops parsing and makes whatever follows the caller's
            // problem. Skipping this entirely is what let `git stash clear
            // --zzbogus` silently erase every entry.
            let rest = &args[1..];
            let mut stop = 0;
            while stop < rest.len() {
                let a = rest[stop].as_str();
                if a == "--" || a == "--end-of-options" {
                    stop += 1;
                    break;
                }
                if !a.starts_with('-') || a == "-" {
                    break;
                }
                // `--help-all`'s own `strcmp()` in `parse_options_step()` runs
                // after the two breaks above and before `parse_long_opt()`:
                // exact name only, and `USAGE_FULL` is this block because
                // `clear`'s table is empty of hidden entries — of entries at
                // all, in fact.
                if a == "-h" || a == "--help-all" {
                    return Ok(super::show_usage(CLEAR_USAGE));
                }
                return Ok(usage_error(a, CLEAR_USAGE));
            }
            if stop < rest.len() {
                eprintln!("error: git stash clear with arguments is unimplemented");
                return Ok(ExitCode::FAILURE);
            }
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            clear(&repo)
        }
        Some("show") => {
            if let Some(code) = usage_requested(&args[1..], SHOW_USAGE) {
                return Ok(code);
            }
            show_stash(&repo, &args[1..])
        }
        Some("branch") => {
            if let Some(code) = usage_requested(&args[1..], BRANCH_USAGE) {
                return Ok(code);
            }
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            branch_stash(&repo, &args[1..])
        }
        // `create` has no option table at all — every argument is message text,
        // `-h` included, which is why it prints a stash id rather than usage.
        Some("create") => create_stash(&repo, &args[1..]),
        Some("store") => {
            if let Some(code) = usage_requested(&args[1..], STORE_USAGE) {
                return Ok(code);
            }
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            store_stash(&repo, &args[1..])
        }
        Some(flag) if flag.starts_with('-') => {
            // Implicit push with options, e.g. `git stash -m msg` or `git stash -u`.
            // `cmd_stash` re-enters `push_stash()` with `push_assumed` set, which
            // renders `git_stash_usage` against push's option table — the same
            // option surface as `git stash push`, a different usage block.
            let opts = match parse_push_options(args, STASH_PUSH_USAGE)? {
                Ok(o) => o,
                Err(code) => return Ok(code),
            };
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            push(&repo, &opts)
        }
        // Neither body is ported, but `parse_options()` runs before either one,
        // so the option surface is still git's: `-h` and `--help-all` print the
        // sub-command's block on stdout and an unclaimed flag is refused with it
        // on stderr, both at 129. Only a well-formed invocation reaches the bail.
        Some(sub @ ("export" | "import")) => {
            let (usage, table) = match sub {
                "export" => (EXPORT_USAGE, EXPORT_OPTS),
                _ => (IMPORT_USAGE, NO_OPTS),
            };
            let rest = &args[1..];
            let mut i = 0;
            while let Some(a) = rest.get(i).map(String::as_str) {
                i += 1;
                // `PARSE_OPT_KEEP_DASHDASH` leaves the `--` in argv but still
                // ends the option scan, so nothing past it is an option.
                if a == "--" {
                    break;
                }
                if super::asks_for_help(a, "") {
                    return Ok(super::show_usage(usage));
                }
                if !a.starts_with('-') || a == "-" {
                    continue;
                }
                let resolved = match canonical(a, table, usage) {
                    Ok(name) => name,
                    Err(code) => return Ok(code),
                };
                match resolved.as_ref() {
                    "--print" | "--no-to-ref" => {}
                    // `OPT_STRING` takes the next argument when no `=<value>` is
                    // attached, and `get_arg()` refuses at 129 when there is
                    // none — which is also what keeps a following `-h` from
                    // being read as a request for help.
                    "--to-ref" => match rest.get(i) {
                        Some(_) => i += 1,
                        None => return Ok(super::missing_option_value("--to-ref")),
                    },
                    other if other.starts_with("--to-ref=") => {}
                    _ => return Ok(usage_error(a, usage)),
                }
            }
            crate::git_fatal!("`stash {sub}` is not ported")
        }
        Some(other) => crate::git_fatal!("{other} is not a stash command"),
    }
}

/// `git stash push` — snapshot tracked changes, then reset the worktree+index to HEAD.
fn push(repo: &gix::Repository, opts: &PushOpts) -> Result<ExitCode> {
    // `-S` computes its reset by merging trees that describe the *worktree*, which
    // git never writes: it reverses the staged patch over the files themselves
    // (`git apply -R`). Those intermediate objects go here instead of into the
    // repository, so a `-S` push — successful or not — leaves the same object
    // database behind that git leaves.
    let scratch = repo.clone().with_object_memory();

    // An unborn HEAD has no base to stash against. `do_create_stash()` says so on
    // stderr — unprefixed, and not at all under `-q` — and returns -1.
    if repo.head_id().is_err() {
        if !opts.quiet {
            eprintln!("You do not have the initial commit yet");
        }
        return Ok(ExitCode::FAILURE);
    }

    // `do_push_stash()` starts with `repo_refresh_and_write_index()`, which fails
    // on an unmerged index after announcing every conflicted path.
    if let Some(code) = refuse_unmerged_index(repo)? {
        return Ok(code);
    }

    // A pathspec naming nothing git tracks is an error, before any work — git
    // reports the normalized spec, magic prefix and all.
    if !opts.pathspecs.is_empty() {
        if let Some(spec) = first_unmatched_spec(repo, &opts.pathspecs)? {
            bail!("pathspec '{spec}' did not match any file(s) known to git");
        }
    }

    // Untracked-only changes are not stashed without `-u`, matching git.
    let untracked_wanted = opts.untracked != Untracked::No;
    if !repo.is_dirty()? && !untracked_wanted {
        if !opts.quiet {
            println!("No local changes to save");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `do_push_stash()`: `--patch` defaults `--keep-index` on, because the index
    // is not what the selection came from.
    let keep_index = opts.keep_index.unwrap_or(opts.patch);

    let StashBuild {
        w_commit,
        stash_msg,
        head_tree_id,
        i_tree_id,
        affected,
        untracked_paths,
        worktree_tree,
        patch,
    } = build_stash_commit(repo, &scratch, opts.message.as_deref(), opts)?;

    // `stash_patch()` returning an empty patch is "the user took nothing":
    // `do_create_stash()` stops there, so no stash is stored and no reset runs.
    if let Some(patch) = &patch {
        if patch.is_empty() {
            if !opts.quiet {
                eprintln!("No changes selected");
            }
            return Ok(ExitCode::FAILURE);
        }
    }

    // Nothing survived the filter: a pathspec that matched only clean paths, or
    // `-S` with an empty index diff. git distinguishes the two.
    if affected.is_empty() && untracked_paths.is_empty() {
        if opts.staged_only {
            // `stash_staged()`: stderr, unprefixed.
            eprintln!("No staged changes");
            return Ok(ExitCode::FAILURE);
        }
        if !opts.quiet {
            println!("No local changes to save");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Append the reflog entry and move refs/stash to the new W commit.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: true,
                message: stash_msg.clone().into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(w_commit),
        },
        name: "refs/stash".try_into().map_err(|e| anyhow!("invalid ref name refs/stash: {e}"))?,
        deref: false,
    })?;

    // `do_push_stash()` announces the stash as soon as `do_store_stash()` has
    // written it — before the reset below, which is why a `-S` whose worktree
    // cleanup fails has still reported the stash as saved.
    if !opts.quiet {
        println!("Saved working directory and index state {stash_msg}");
    }

    // Patch mode subtracts exactly the hunks that were taken: git pipes the same
    // patch it built the stash from back through `git apply -R`, which leaves
    // every *unselected* edit on disk untouched — a tree-level reset could not,
    // since two hunks of one file can land on opposite sides of the selection.
    if let Some(patch) = &patch {
        let (ok, _) = super::add_patch::run_git(
            &["apply".to_string(), "-R".to_string()],
            Some(patch),
            false,
            None,
        )?;
        if !ok {
            if !opts.quiet {
                eprintln!("Cannot remove worktree changes");
            }
            return Ok(ExitCode::FAILURE);
        }
        // `keep_index < 1`: the index still describes the pre-stash state, so git
        // refreshes it against the worktree it just rewound.
        if !keep_index {
            let mut args: Vec<String> =
                ["reset", "-q", "--refresh", "--"].iter().map(|s| (*s).to_string()).collect();
            args.extend(opts.pathspecs.iter().cloned());
            let (ok, _) = super::add_patch::run_git(&args, None, false, None)?;
            if !ok {
                return Ok(ExitCode::FAILURE);
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Reset the tracked worktree + index (untracked files handled separately
    // below). `--keep-index` resets to the index tree rather than HEAD, which is
    // precisely what leaves the staged changes staged and their content on disk.
    //
    // `-S` is neither: git subtracts the staged diff from the worktree with
    // `git apply -R` and leaves every *unstaged* edit where it is (`do_push_stash`,
    // builtin/stash.c) — including under `-k`, which only skips the index reset
    // that follows. Reverting the whole path to `HEAD` here would delete work the
    // stash does not hold.
    let worktree_target = match (opts.staged_only, keep_index) {
        (true, _) => match revert_staged_in_worktree(
            &scratch,
            i_tree_id,
            head_tree_id,
            worktree_tree.unwrap_or(i_tree_id),
        )? {
            Ok(tree) => tree,
            // `apply -R` failed: an unstaged edit sits on top of the staged one
            // it would have to undo. git leaves the worktree and index untouched
            // and fails, with the stash entry it just wrote still in place.
            Err(blocked) => {
                for path in blocked {
                    // git-apply names the hunk it could not place before the
                    // per-path verdict.
                    if let Some(line) = first_hunk_line(repo, head_tree_id, i_tree_id, &path) {
                        eprintln!("error: patch failed: {path}:{line}");
                    }
                    eprintln!("error: {path}: patch does not apply");
                }
                if !opts.quiet {
                    eprintln!("Cannot remove worktree changes");
                }
                return Ok(ExitCode::FAILURE);
            }
        },
        (false, true) => i_tree_id,
        (false, false) => head_tree_id,
    };
    // The index is rebuilt wholesale from its target tree, so that tree has to
    // describe EVERY path — not just the reset ones. Starting from `I` (what the
    // index holds right now) and reverting only the affected paths is what keeps
    // a staged change to an unmatched path staged, which is what git does under
    // a pathspec. Reverting nothing is `--keep-index`.
    let index_target = if keep_index {
        i_tree_id
    } else {
        revert_paths_in_tree(repo, i_tree_id, head_tree_id, &affected)?
    };
    // `-S`'s target tree lives in the scratch odb, so both readers go through it; for
    // every other mode it is a real tree and the proxy just reads through.
    let target_map = tree_map(&scratch, worktree_target)?;
    let should_interrupt = AtomicBool::new(false);
    let fresh = sync_worktree(&scratch, worktree_target, &affected, &target_map, &should_interrupt)?;
    let old_index = repo.open_index()?;
    write_target_index(repo, index_target, &old_index, &fresh)?;

    // The untracked files now live in the stash's third parent, so git removes
    // them from the worktree — with `git clean --force --quiet -d`, whose `-d`
    // also takes the directories they leave empty.
    let workdir = repo.workdir().map(std::path::Path::to_path_buf);
    if let Some(root) = workdir {
        for path in &untracked_paths {
            let abs = root.join(gix::path::from_bstr(path.as_bstr()).as_ref());
            let _ = std::fs::remove_file(&abs);
        }
        remove_emptied_dirs(repo, &untracked_paths);
    }

    // `do_push_stash()` clears the worktree with `reset --hard`, which logs
    // `reset: moving to HEAD` on `HEAD` — the branch does not move, so only that one
    // log gains an entry. Only the pathspec-less path runs that reset: with a
    // pathspec git restores the named paths through `checkout-index`/`clean` instead,
    // and `-S` reverses the staged patch with `apply -R`; neither logs anything.
    if opts.pathspecs.is_empty() && !opts.staged_only && !opts.patch {
        if let Ok(head_id) = repo.head_id() {
            let id = head_id.detach();
            super::checkout::record_head_move(repo, Some(id), Some(id), "reset: moving to HEAD");
            // The same `reset --hard` also runs `reset_refs()`, which records
            // the pre-reset `HEAD` in `ORIG_HEAD` (builtin/reset.c:314). The
            // reset names no revision, so the value is `HEAD` itself — but it
            // still *moves*, overwriting whatever an earlier operation left
            // there, which is what a `git stash push` in a repository that has
            // stashed before is observed to do. `--keep-index` and `-u`/`-a`
            // take the same branch of `do_push_stash()`; only a pathspec
            // (`git apply -R`) or `-S` skips the reset, and neither writes it.
            super::reset::set_orig_head(repo, id)?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Pathspec matching for a push, over the same `repo.pathspec()` engine `rm`,
/// `grep` and `jump` use — so `:(glob)`, `:(icase)` and `:!` behave identically
/// here and a bare `*` keeps crossing `/` the way git's pathspecs do.
/// Of `candidates`, the paths any spec selects.
///
/// The engine is built once for the whole batch: `repo.pathspec()` borrows both
/// the repository and the index, so a per-path matcher would either re-open the
/// index on every call or need a self-referential struct.
fn select_matching(
    repo: &gix::Repository,
    specs: &[String],
    candidates: &[BString],
) -> Result<HashSet<BString>> {
    let index = repo.open_index()?;
    let patterns: Vec<BString> = specs.iter().map(|s| BString::from(s.as_str())).collect();
    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    let mut out = HashSet::new();
    for path in candidates {
        if ps.pattern_matching_relative_path(path.as_bstr(), Some(false)).is_some() {
            out.insert(path.clone());
        }
    }
    Ok(out)
}

/// The first spec naming nothing in the index, in git's normalized form
/// (`:(prefix:0)<path>`), or `None` when every spec matched something.
///
/// Exclusions select by removing, so they are never reported as unmatched.
fn first_unmatched_spec(repo: &gix::Repository, specs: &[String]) -> Result<Option<String>> {
    let index = repo.open_index()?;
    let backing = index.path_backing();
    for raw in specs {
        if raw.starts_with(":!") || raw.starts_with(":^") {
            continue;
        }
        let one = [BString::from(raw.as_str())];
        let mut ps = repo.pathspec(
            true,
            &one,
            false,
            &index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )?;
        let hit = index
            .entries()
            .iter()
            .any(|e| ps.pattern_matching_relative_path(e.path_in(backing), Some(false)).is_some());
        if !hit {
            return Ok(Some(format!(":(prefix:0){raw}")));
        }
    }
    Ok(None)
}

/// Untracked files a `-u`/`-a` push takes, restricted by any pathspec.
///
/// `-a` additionally takes ignored files. Directories are walked into rather
/// than captured whole, since the third parent's tree stores blobs by path.
fn collect_untracked(repo: &gix::Repository, opts: &PushOpts) -> Result<Vec<BString>> {
    let index = repo.open_index()?;
    let patterns: Vec<BString> =
        opts.pathspecs.iter().map(|s| BString::from(s.as_str())).collect();
    let want_ignored = opts.untracked == Untracked::All;
    let options = repo
        .dirwalk_options()?
        .emit_ignored(want_ignored.then_some(gix::dir::walk::EmissionMode::Matching))
        .emit_untracked(gix::dir::walk::EmissionMode::Matching);

    let mut out: Vec<BString> = Vec::new();
    let mut collect = repo.dirwalk_iter(index, patterns, Default::default(), options)?;
    for item in &mut collect {
        let entry = item?.entry;
        match entry.status {
            gix::dir::entry::Status::Untracked => {}
            gix::dir::entry::Status::Ignored(_) if want_ignored => {}
            _ => continue,
        }
        // Only regular files land in the tree; a nested repository is left alone.
        if entry.disk_kind == Some(gix::dir::entry::Kind::Directory)
            || entry.disk_kind == Some(gix::dir::entry::Kind::Repository)
        {
            continue;
        }
        out.push(entry.rela_path.clone());
    }
    out.sort();
    Ok(out)
}

/// The stash object graph produced from the current tracked changes: the `W`
/// merge commit plus the data `push`/`create` need afterwards.
struct StashBuild {
    /// The stash (`W`) merge commit id — the value stored in `refs/stash`.
    w_commit: ObjectId,
    /// The reflog / commit message (`WIP on …` or `On <branch>: …`).
    stash_msg: String,
    /// HEAD's tree, used by `push` to reset the worktree/index afterwards.
    head_tree_id: ObjectId,
    /// The index tree `I`. `--keep-index` resets to this instead of HEAD, which
    /// is what leaves the staged changes staged.
    i_tree_id: ObjectId,
    /// Every path touched by the staged/unstaged changes, for the reset step.
    affected: HashSet<BString>,
    /// The tree the worktree currently holds, built only for `-S`: there the
    /// stash (`W` == `I`) is not what is on disk, and the reset has to subtract
    /// the staged diff from *this* tree rather than jump to `HEAD`.
    worktree_tree: Option<ObjectId>,
    /// Untracked (and with `-a`, ignored) files captured into the third parent,
    /// which `push` deletes from the worktree afterwards.
    untracked_paths: Vec<BString>,
    /// `-p`: the `diff-tree -p -U1 HEAD <w_tree>` text `stash_patch()` produced,
    /// which `push` reverses out of the worktree with `git apply -R`. Empty means
    /// nothing was selected — `do_create_stash()` stops there. `None` without `-p`.
    patch: Option<Vec<u8>>,
}

/// Build the stash commit graph (`I` index commit then `W` merge commit) from
/// the current tracked staged + unstaged changes, without touching `refs/stash`
/// or the worktree. Faithful port of `do_create_stash` (git builtin/stash.c),
/// shared by `push` (which then stores + resets) and `create` (which prints the
/// `W` id). The caller has already verified HEAD is born and the tree is dirty.
fn build_stash_commit(
    repo: &gix::Repository,
    // `-S` needs a picture of what the worktree currently holds, which git never
    // hashes (it reverses the staged patch over the files themselves). Building it here
    // as a tree is how the reset is computed, so the objects it needs go to this
    // memory-backed handle and leave no trace in the repository's object database.
    scratch: &gix::Repository,
    message: Option<&str>,
    opts: &PushOpts,
) -> Result<StashBuild> {
    let head_id = repo.head_id()?.detach();
    let head_tree_id = repo.head_tree_id()?.detach();
    let branch = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "(no branch)".to_string(),
    };
    let head_short = repo.head_id()?.shorten_or_id().to_string();
    let subject = repo.head_commit()?.message()?.summary().to_string();

    let stash_msg = match message {
        Some(m) => format!("On {branch}: {m}"),
        None => format!("WIP on {branch}: {head_short} {subject}"),
    };
    let index_msg = format!("index on {branch}: {head_short} {subject}");

    // Collect staged (HEAD↔index) and unstaged (index↔worktree) tracked changes.
    // Rename tracking and the untracked dirwalk are disabled so only concrete
    // per-path additions/deletions/modifications are reported.
    let mut staged: Vec<ChangeRef<'static, 'static>> = Vec::new();
    let mut wt_mods: Vec<(BString, bool)> = Vec::new(); // (path, is_removed)
    {
        let iter = repo
            .status(gix::progress::Discard)?
            .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
            .index_worktree_rewrites(None)
            .untracked_files(gix::status::UntrackedFiles::None)
            .index_worktree_options_mut(|opts| opts.dirwalk_options = None)
            .into_iter(Vec::new())?;
        for item in iter {
            match item? {
                gix::status::Item::TreeIndex(change) => staged.push(change),
                gix::status::Item::IndexWorktree(iw) => {
                    use gix::status::index_worktree::Item as Iw;
                    use gix::status::plumbing::index_as_worktree::{Change as Wt, EntryStatus};
                    if let Iw::Modification { rela_path, status, .. } = iw {
                        match status {
                            EntryStatus::Change(Wt::Removed) => wt_mods.push((rela_path, true)),
                            EntryStatus::Change(Wt::Modification { .. })
                            | EntryStatus::Change(Wt::Type { .. })
                            | EntryStatus::Change(Wt::SubmoduleModification(_)) => {
                                wt_mods.push((rela_path, false));
                            }
                            EntryStatus::Conflict { .. } => {
                                crate::git_fatal!("cannot stash: unmerged (conflicted) entries present")
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Build the index tree `I` = HEAD tree + staged changes.
    let mut affected: HashSet<BString> = HashSet::new();
    let mut i_editor = repo.edit_tree(head_tree_id)?;
    for change in &staged {
        match change {
            ChangeRef::Addition { location, entry_mode, id, .. }
            | ChangeRef::Modification { location, entry_mode, id, .. } => {
                let path: BString = (**location).to_owned();
                let oid: ObjectId = (**id).to_owned();
                i_editor.upsert(path.as_bstr(), entry_kind(*entry_mode)?, oid)?;
                affected.insert(path);
            }
            ChangeRef::Deletion { location, .. } => {
                let path: BString = (**location).to_owned();
                i_editor.remove(path.as_bstr())?;
                affected.insert(path);
            }
            ChangeRef::Rewrite { source_location, location, entry_mode, id, .. } => {
                let source: BString = (**source_location).to_owned();
                let path: BString = (**location).to_owned();
                let oid: ObjectId = (**id).to_owned();
                i_editor.remove(source.as_bstr())?;
                i_editor.upsert(path.as_bstr(), entry_kind(*entry_mode)?, oid)?;
                affected.insert(source);
                affected.insert(path);
            }
        }
    }
    let i_tree_id = i_editor.write()?.detach();

    // A pathspec narrows which *worktree* changes are taken and which paths are
    // reset — never the index tree above, which git always captures whole (a
    // staged change to an unmatched path still rides along in `I`, and stays
    // staged in the worktree afterwards).
    //
    // `-S` takes the staged changes alone, so the unstaged set is dropped
    // entirely and `W` ends up identical to `I`. The unstaged work does not
    // disappear, though — the reset in `push` has to preserve it — so the tree
    // the worktree actually holds is built first and handed back.
    let worktree_tree = if opts.staged_only {
        let tree = tree_with_worktree_mods(scratch, i_tree_id, &wt_mods)?;
        wt_mods.clear();
        Some(tree)
    } else {
        None
    };
    if !opts.pathspecs.is_empty() {
        let mut candidates: Vec<BString> = wt_mods.iter().map(|(p, _)| p.clone()).collect();
        candidates.extend(affected.iter().cloned());
        let keep = select_matching(repo, &opts.pathspecs, &candidates)?;
        wt_mods.retain(|(path, _)| keep.contains(path));
        affected.retain(|path| keep.contains(path));
    }

    // Build the worktree tree `W` = `I` + unstaged worktree changes. Blobs are
    // produced through the filter pipeline so they are byte-identical to git's.
    for (path, _) in &wt_mods {
        affected.insert(path.clone());
    }
    // `do_create_stash()` picks one of three ways to build `W`: the hunks the
    // user selected (`stash_patch`), the staged changes alone (`stash_staged`,
    // handled above), or the whole worktree (`stash_working_tree`).
    let (w_tree_id, patch) = if opts.patch {
        let (tree, patch) = stash_patch(repo, opts)?;
        (tree, Some(patch))
    } else {
        (tree_with_worktree_mods(repo, i_tree_id, &wt_mods)?, None)
    };

    // `I` commit (parent: HEAD), then `W` merge commit (parents: HEAD, I).
    //
    // The newline asymmetry is git's, not a typo: `do_create_stash` terminates
    // the index commit's message but commits the stash message verbatim, so a
    // `W` commit body ends on the last byte of the message with no trailing LF.
    // Appending one here changes the commit id and diverges `refs/stash` plus
    // every object-listing probe from stock git. Verified byte-for-byte against
    // git 2.55.0 for the `push`, `push -m`, and `save` message forms.
    let index_commit = repo.new_commit(format!("{index_msg}\n"), i_tree_id, [head_id])?.id().detach();

    // `-u`/`-a` capture the untracked files into a THIRD parent: a parentless
    // commit whose tree is just those files (`do_create_stash`'s `u_commit`).
    // They are not part of `I` or `W` — `stash pop` restores them from this
    // commit — and `push` deletes them from the worktree afterwards.
    let mut untracked_paths: Vec<BString> = Vec::new();
    let mut parents = vec![head_id, index_commit];
    if opts.untracked != Untracked::No {
        untracked_paths = collect_untracked(repo, opts)?;
        let (mut pipeline, wt_index) = repo.filter_pipeline(None)?;
        let empty = repo.empty_tree().id().detach();
        let mut u_editor = repo.edit_tree(empty)?;
        for path in &untracked_paths {
            if let Some((id, kind, _md)) =
                pipeline.worktree_file_to_object(path.as_bstr(), &wt_index)?
            {
                u_editor.upsert(path.as_bstr(), kind, id)?;
            }
        }
        let u_tree_id = u_editor.write()?.detach();
        let u_msg = format!("untracked files on {branch}: {head_short} {subject}");
        // Parentless, like git's `u_commit`: the untracked tree stands alone. git
        // writes this parent whenever `-u`/`-a` was asked for — even when nothing was
        // collected, in which case its tree is the empty one.
        let no_parents: Vec<ObjectId> = Vec::new();
        let u_commit =
            repo.new_commit(format!("{u_msg}\n"), u_tree_id, no_parents)?.id().detach();
        parents.push(u_commit);
    }

    let w_commit = repo.new_commit(stash_msg.as_str(), w_tree_id, parents)?.id().detach();

    Ok(StashBuild {
        w_commit,
        stash_msg,
        head_tree_id,
        i_tree_id,
        affected,
        untracked_paths,
        worktree_tree,
        patch,
    })
}

/// `stash_patch()` (builtin/stash.c): run the hunk selector against a scratch
/// index seeded from `HEAD`, and answer the tree it staged plus the patch that
/// tree amounts to.
///
/// The scratch index is `$GIT_DIR/index.stash.<pid>`, exported to every child of
/// the selector as `GIT_INDEX_FILE`, so the `apply --cached` that stages a chosen
/// hunk lands there and the user's real index is never touched. The patch is
/// `diff-tree -p -U1 HEAD <w_tree>`, generated with one line of context precisely
/// so that reversing it later subtracts only the selected hunks and leaves an
/// unselected edit nearby in place.
///
/// An empty patch is git's "nothing was selected", which the caller turns into
/// `No changes selected` without storing anything.
fn stash_patch(repo: &gix::Repository, opts: &PushOpts) -> Result<(ObjectId, Vec<u8>)> {
    let index_file = repo.git_dir().join(format!("index.stash.{}", std::process::id()));
    let head_tree = repo.head_commit()?.tree_id()?.detach();
    // `create_index_from_tree()`; `write_ondisk_index` also clears a stale file
    // an interrupted run left behind, which is git's `remove_path()` on entry.
    let seeded = super::history::write_ondisk_index(repo, head_tree, &index_file);
    let result = (|| -> Result<(ObjectId, Vec<u8>)> {
        seeded?;
        // `run_add_p(the_repository, ADD_P_STASH, …, NULL, ps, 0)`: no revision
        // (the mode's diff command already names `HEAD`) and no `DISALLOW_EDIT`.
        // Its status is deliberately dropped, exactly as `stash_patch()` drops it:
        // what was selected is decided by the tree the selector left behind.
        let _ = super::add_patch::run_in_index(
            repo,
            super::add_patch::Mode::Stash,
            &index_file,
            opts.interactive,
            &opts.pathspecs,
        )?;
        let w_tree = super::history::tree_of_index_file(repo, &index_file)
            .ok_or_else(|| anyhow!("Cannot save the current worktree state"))?;
        let args: Vec<String> = ["diff-tree", "-p", "-U1", "HEAD", "--no-color"]
            .iter()
            .map(|s| (*s).to_string())
            .chain([w_tree.to_string(), "--".to_string()])
            .collect();
        let (ok, patch) = super::add_patch::run_git(&args, None, true, None)?;
        if !ok {
            bail!("Cannot save the current worktree state");
        }
        Ok((w_tree, patch))
    })();
    let _ = std::fs::remove_file(&index_file);
    result
}

/// `base` with the unstaged worktree changes in `wt_mods` applied — the tree the
/// files on disk currently amount to, when `base` is the index tree.
///
/// Blobs go through the repository's filter pipeline, so what lands in the tree
/// is byte-identical to what `git add` would have written.
fn tree_with_worktree_mods(
    repo: &gix::Repository,
    base: ObjectId,
    wt_mods: &[(BString, bool)],
) -> Result<ObjectId> {
    if wt_mods.is_empty() {
        return Ok(base);
    }
    let mut editor = repo.edit_tree(base)?;
    let (mut pipeline, wt_index) = repo.filter_pipeline(None)?;
    for (path, removed) in wt_mods {
        if *removed {
            editor.remove(path.as_bstr())?;
            continue;
        }
        match pipeline.worktree_file_to_object(path.as_bstr(), &wt_index)? {
            Some((id, kind, _md)) => {
                editor.upsert(path.as_bstr(), kind, id)?;
            }
            None => {
                editor.remove(path.as_bstr())?;
            }
        }
    }
    Ok(editor.write()?.detach())
}

/// The worktree tree a `-S` reset should end at: `worktree_tree` with the staged
/// diff (`head_tree`→`index_tree`) taken back out, or `None` when that is not
/// possible.
///
/// git does this with `git apply -R` of the staged patch against the files on
/// disk (`do_push_stash`, builtin/stash.c). The equivalent three-way merge is
/// `index_tree` as the base — what the staged change produced — with the
/// worktree as *ours* and `HEAD` as *theirs*: an unstaged edit elsewhere in the
/// file survives, and an unstaged edit that overlaps the staged one conflicts,
/// which is `apply -R` failing. `apply` is all-or-nothing, so a conflict on any
/// path returns `None` and nothing is written.
fn revert_staged_in_worktree(
    repo: &gix::Repository,
    index_tree: ObjectId,
    head_tree: ObjectId,
    worktree_tree: ObjectId,
) -> Result<std::result::Result<ObjectId, Vec<BString>>> {
    // Reverting a staged *deletion* means writing the file back, and `git apply`
    // refuses to create a path that is already there — a `git rm --cached` leaves
    // exactly that: the entry gone from the index, the file still on disk. git
    // reports it per path and abandons the whole reset.
    let head = tree_map(repo, head_tree)?;
    let staged = tree_map(repo, index_tree)?;
    let mut in_the_way: Vec<BString> = head
        .keys()
        .filter(|path| !staged.contains_key(*path))
        .filter(|path| {
            repo.workdir_path(path.as_bstr())
                .is_some_and(|full| full.symlink_metadata().is_ok())
        })
        .cloned()
        .collect();
    if !in_the_way.is_empty() {
        in_the_way.sort();
        for path in &in_the_way {
            eprintln!("error: {path}: already exists in working directory");
        }
        // Reported with its own wording, so the caller's per-path
        // `patch does not apply` lines would be a second, wrong explanation.
        return Ok(Err(Vec::new()));
    }

    // Nothing unstaged: the reverse patch is the plain reset to HEAD.
    if worktree_tree == index_tree {
        return Ok(Ok(head_tree));
    }
    let labels = gix::merge::blob::builtin_driver::text::Labels::default();
    let mut merge = repo.merge_trees(
        index_tree,
        worktree_tree,
        head_tree,
        labels,
        repo.tree_merge_options()?,
    )?;
    let unresolved = gix::merge::tree::TreatAsUnresolved::git();
    let blocked: Vec<BString> = merge
        .conflicts
        .iter()
        .filter(|c| c.is_unresolved(unresolved))
        .map(|c| c.changes_in_resolution().0.location().to_owned())
        .collect();
    if !blocked.is_empty() {
        return Ok(Err(blocked));
    }
    Ok(Ok(merge.tree.write()?.detach()))
}

/// The line `git apply` names when the staged patch for `path` cannot be placed:
/// the old-side start of its first hunk, counting the one line of context
/// `stash_staged()`'s `diff-tree -U1` carries.
///
/// Both blobs come from trees, so this is the same patch geometry the reverse
/// apply would have worked from — no worktree state is involved.
fn first_hunk_line(
    repo: &gix::Repository,
    head_tree: ObjectId,
    index_tree: ObjectId,
    path: &BString,
) -> Option<u32> {
    use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
    use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};

    let blob = |tree: ObjectId| -> Option<Vec<u8>> {
        let id = tree_map(repo, tree).ok()?.get(path).map(|(id, _)| *id)?;
        Some(repo.find_object(id).ok()?.detach().data)
    };
    let (old, new) = (blob(head_tree)?, blob(index_tree)?);
    let worktree = repo
        .workdir_path(path.as_bstr())
        .and_then(|full| std::fs::read(full).ok())?;

    /// Every hunk of the staged patch as `(old-side start, new-side range)`.
    #[derive(Default)]
    struct Hunks(Vec<(u32, std::ops::Range<usize>)>);
    impl ConsumeHunk for Hunks {
        type Out = Vec<(u32, std::ops::Range<usize>)>;
        fn consume_hunk(
            &mut self,
            header: HunkHeader,
            _lines: &[(DiffLineKind, &[u8])],
        ) -> std::io::Result<()> {
            let start = header.after_hunk_start.saturating_sub(1) as usize;
            self.0
                .push((header.before_hunk_start, start..start + header.after_hunk_len as usize));
            Ok(())
        }
        fn finish(self) -> Self::Out {
            self.0
        }
    }

    let input = InternedInput::new(old.as_slice(), new.as_slice());
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    // `stash_staged()` generates the patch with `-U1`, and a hunk header's start
    // counts that context line.
    let hunks = UnifiedDiff::new(&diff, &input, Hunks::default(), ContextSize::symmetrical(1))
        .consume()
        .ok()?;

    // `git apply` names the hunk it could not place, not the first one. A hunk
    // reverse-applies only if its *new* side — what the staged change produced —
    // is still in the worktree to be taken back out; `apply` searches for it with
    // some drift, so its presence anywhere is the test.
    let index_lines: Vec<&[u8]> = new.split_inclusive(|b| *b == b'\n').collect();
    let worktree_lines: Vec<&[u8]> = worktree.split_inclusive(|b| *b == b'\n').collect();
    let applies = |range: &std::ops::Range<usize>| -> bool {
        let want = match index_lines.get(range.clone()) {
            Some(lines) if !lines.is_empty() => lines,
            _ => return true,
        };
        worktree_lines.windows(want.len()).any(|w| w == want)
    };
    hunks
        .iter()
        .find(|(_, range)| !applies(range))
        .or_else(|| hunks.first())
        .map(|(before_start, _)| *before_start)
}

/// Whether `theirs` merges into `ours` over `base` without conflicts, reporting
/// the blocked paths when it does not.
///
/// This is a question, not a step: git asks it with `git apply --cached --check`, which
/// writes nothing. The tree merge that answers it here resolves content merges, and
/// resolving one means writing the merged blob — so it runs against a memory-backed
/// handle and the object database is left exactly as it was.
fn merge_trees_cleanly(
    repo: &gix::Repository,
    base: ObjectId,
    ours: ObjectId,
    theirs: ObjectId,
) -> Result<std::result::Result<(), Vec<BString>>> {
    let repo = &repo.clone().with_object_memory();
    let labels = gix::merge::blob::builtin_driver::text::Labels::default();
    let merge = repo.merge_trees(base, ours, theirs, labels, repo.tree_merge_options()?)?;
    let unresolved = gix::merge::tree::TreatAsUnresolved::git();
    let blocked: Vec<BString> = merge
        .conflicts
        .iter()
        .filter(|c| c.is_unresolved(unresolved))
        .map(|c| c.changes_in_resolution().0.location().to_owned())
        .collect();
    match blocked.is_empty() {
        true => Ok(Ok(())),
        false => Ok(Err(blocked)),
    }
}

/// `repo_refresh_and_write_index()` refusing an unmerged index, which is how a
/// `stash push` in the middle of a conflicted merge ends: every unmerged path on
/// stdout, then `error: could not write index` and exit 1.
fn refuse_unmerged_index(repo: &gix::Repository) -> Result<Option<ExitCode>> {
    let index = repo.open_index()?;
    let backing = index.path_backing();
    let mut unmerged: Vec<&BStr> = index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() != 0)
        .map(|e| e.path_in(backing))
        .collect();
    // The index holds up to three stages per conflicted path; git names each path once.
    unmerged.dedup();
    if unmerged.is_empty() {
        return Ok(None);
    }
    for path in unmerged {
        println!("{path}: needs merge");
    }
    eprintln!("error: could not write index");
    Ok(Some(ExitCode::FAILURE))
}

/// `save_state()` (builtin/merge.c): the `git stash create` snapshot a merge takes of a
/// dirty worktree before running a strategy, so `restore_state()` can put the tree back
/// if the strategy fails. `None` when there is nothing to snapshot, which is git's
/// empty output from `stash create`.
pub(crate) fn create_snapshot(repo: &gix::Repository) -> Result<Option<ObjectId>> {
    if repo.head_id().is_err() || !repo.is_dirty()? {
        return Ok(None);
    }
    let built = build_stash_commit(repo, repo, None, &PushOpts::with_message(None))?;
    Ok(Some(built.w_commit))
}

/// `git stash create [<message>…]` — build the stash commit graph and print the
/// `W` commit id without storing it or touching the worktree. Port of
/// `create_stash` (builtin/stash.c): the message is every remaining arg joined
/// by a space (no option parsing), and a clean tree prints nothing (exit 0).
fn create_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    if repo.head_id().is_err() {
        eprintln!("You do not have the initial commit yet");
        return Ok(ExitCode::FAILURE);
    }
    // `check_changes_tracked_files`: no tracked changes → nothing to create.
    if !repo.is_dirty()? {
        return Ok(ExitCode::SUCCESS);
    }
    let message = if args.is_empty() { None } else { Some(args.join(" ")) };
    let built = build_stash_commit(repo, repo, message.as_deref(), &PushOpts::with_message(None))?;
    println!("{}", built.w_commit);
    Ok(ExitCode::SUCCESS)
}

/// `git stash store [-m <msg>] [-q] <commit>` — point `refs/stash` at an existing
/// stash-like commit, appending the reflog entry. Port of `do_store_stash`.
/// `git stash store -m <msg> -q <commit>`, which `apply_save_autostash_oid()` runs when
/// an autostash could not be re-applied: the snapshot becomes a real `stash` so the
/// "Your changes are safe in the stash" line is true.
pub(crate) fn store_commit(repo: &gix::Repository, oid: ObjectId, message: &str) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: true,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(oid),
        },
        name: "refs/stash".try_into().map_err(|e| anyhow!("invalid ref name refs/stash: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

fn store_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let (message, quiet, commit) = parse_store_options(args)?;
    let oid = match repo.rev_parse_single(commit.as_str()) {
        Ok(id) => id.detach(),
        Err(_) => {
            if !quiet {
                eprintln!("Cannot update refs/stash with {commit}");
            }
            return Ok(ExitCode::FAILURE);
        }
    };
    let stash_msg = message.unwrap_or_else(|| "Created via \"git stash store\".".to_string());
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: true,
                message: stash_msg.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(oid),
        },
        name: "refs/stash".try_into().map_err(|e| anyhow!("invalid ref name refs/stash: {e}"))?,
        deref: false,
    })?;
    Ok(ExitCode::SUCCESS)
}

/// `git stash show [<diff-options>] [<stash>]` — show the change the stash would
/// apply. Port of `show_stash`: the diff is always `b_commit`→`w_commit` (base
/// tree vs. stashed worktree tree); the user's diff options only pick the format.
/// With no options the `stash.showStat` (default on) / `stash.showPatch` config
/// decides. Delegates the actual rendering to the `diff` porcelain, which is the
/// same machinery git's `diff_tree_oid` drives, so output is byte-identical.
fn show_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut diff_flags: Vec<String> = Vec::new();
    let mut stash_specs: Vec<String> = Vec::new();
    // `show_include_untracked` / `show_only_untracked`: what the `^3` tree the
    // stash carries contributes to the diff. `stash.showIncludeUntracked` seeds
    // it before the options are read — unlike `stash.showStat`/`showPatch`, it
    // applies even when diff options were given (`show_stash`, builtin/stash.c).
    let mut untracked_mode = if repo.config_snapshot().boolean("stash.showIncludeUntracked")
        == Some(true)
    {
        ShowUntracked::Include
    } else {
        ShowUntracked::No
    };
    for a in args {
        match a.as_str() {
            // `show_stash()` parses under `PARSE_OPT_KEEP_UNKNOWN_OPT`, where
            // `register_abbrev()` returns without recording
            // (parse-options.c:502-503), so these names match exactly or not at
            // all. `--only-untracked` is `OPT_SET_INT_F(..., PARSE_OPT_NONEG)`
            // and so has no negation; `-u` is a plain `OPT_SET_INT`, and its
            // `--no-` sense resets the field to `UNTRACKED_NONE`.
            "-u" | "--include-untracked" => untracked_mode = ShowUntracked::Include,
            "--no-include-untracked" => untracked_mode = ShowUntracked::No,
            "--only-untracked" => untracked_mode = ShowUntracked::Only,
            s if s.starts_with('-') && s != "-" => diff_flags.push(a.clone()),
            _ => stash_specs.push(a.clone()),
        }
    }

    // Resolve the stash: a `stash@{n}` / bare `N` / (default) `stash@{0}` goes
    // through the reflog; anything else is resolved as an arbitrary stash-like
    // commit, matching git's `get_stash_info`.
    let commit_id = match resolve_stash(repo, &stash_specs)? {
        Ok(spec) => spec.id,
        Err(code) => return Ok(code),
    };

    // What `setup_revisions()` declines is `show_stash`'s problem, not the diff
    // parser's. git pushes every dashed word into `revision_args`, runs
    // `setup_revisions_from_strvec()` — which *consumes* what it understands and
    // leaves the rest at the front rather than complaining — and then:
    //
    // ```c
    //         setup_revisions_from_strvec(&revision_args, &rev, NULL);
    //         if (revision_args.nr > 1)
    //                 goto usage;
    // ```
    //
    // with `usage:` reaching `usage_with_options(git_stash_show_usage, options)`.
    // So the block a rejected option prints is *stash show's*, on stderr at 129,
    // with no `error:` line — never `git diff`'s, which is what forwarding the
    // token to this port's `diff` produced instead.
    if diff_flags.iter().any(|f| !show_accepts(f)) {
        eprint!("{SHOW_USAGE}");
        return Ok(ExitCode::from(129));
    }

    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    let b_commit = parents[0];

    // No diff options given → apply config defaults (git: revision_args.nr == 1).
    if diff_flags.is_empty() {
        let snap = repo.config_snapshot();
        let show_stat = snap.boolean("stash.showStat").unwrap_or(true);
        let show_patch = snap.boolean("stash.showPatch").unwrap_or(false);
        if !show_stat && !show_patch {
            return Ok(ExitCode::SUCCESS);
        }
        if show_stat {
            diff_flags.push("--stat".to_string());
        }
        if show_patch {
            diff_flags.push("-p".to_string());
        }
    }

    // `show_stash`'s two sides. Without `-u` they are the base commit and the
    // stash's own (tracked) worktree tree. `-u` unions the untracked `^3` tree
    // into the right-hand side — the two are disjoint by construction, since a
    // path the stash tracked cannot also have been untracked — and
    // `--only-untracked` diffs the empty tree against `^3` alone, which is why
    // it reports the untracked files as additions and says nothing about the
    // tracked ones.
    let u_tree = match parents.get(2) {
        Some(id) => Some(repo.find_commit(*id)?.tree_id()?.detach()),
        None => None,
    };
    let (left, right) = match (untracked_mode, u_tree) {
        // `case UNTRACKED_ONLY:` diffs the `^3` tree and nothing else, so a stash
        // that captured no untracked files has nothing to show — not even the
        // tracked changes it does carry.
        (ShowUntracked::Only, None) => return Ok(ExitCode::SUCCESS),
        (ShowUntracked::No, _) | (_, None) => (b_commit.to_string(), commit_id.to_string()),
        (ShowUntracked::Only, Some(u)) => (repo.empty_tree().id().to_string(), u.to_string()),
        (ShowUntracked::Include, Some(u)) => {
            let w_tree = commit.tree_id()?.detach();
            let mut editor = repo.edit_tree(w_tree)?;
            for (path, (id, mode)) in tree_map(repo, u)? {
                let kind = mode
                    .to_tree_entry_mode()
                    .ok_or_else(|| anyhow!("untracked entry `{path}` has an unrepresentable mode"))?
                    .kind();
                editor.upsert(path.as_bstr(), kind, id)?;
            }
            (b_commit.to_string(), editor.write()?.detach().to_string())
        }
    };
    diff_flags.push(left);
    diff_flags.push(right);
    super::diff::diff(&diff_flags)
}

/// The seven names `git diff` resolves that `git stash show` does not.
///
/// `cmd_diff()` claims these itself, before and outside `setup_revisions()`
/// (builtin/diff.c) — so they exist for `git diff` and are leftovers for
/// `show_stash()`, which only ever runs `setup_revisions()`. Verified by putting
/// every name in [`super::diff::is_known_option`]'s table through stock git
/// 2.55.0 as `git stash show <name>`: these seven answer with the `stash show`
/// usage block, the other 164 do not.
const SHOW_NOT_A_REVISION_OPT: &[&str] = &[
    "--base",
    "--cached",
    "--no-index",
    "--ours",
    "--staged",
    "--theirs",
    "-q",
];

/// Whether `setup_revisions()` would claim this option for `git stash show`.
///
/// The union `git diff` accepts, less the handful only `cmd_diff()` reaches.
fn show_accepts(flag: &str) -> bool {
    let name = flag.split_once('=').map_or(flag, |(n, _)| n);
    super::diff::is_known_option(flag) && !SHOW_NOT_A_REVISION_OPT.contains(&name)
}

/// What `git stash show`'s `-u`/`--only-untracked` ask of the `^3` tree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShowUntracked {
    No,
    Include,
    Only,
}

/// `git stash branch <branchname> [<stash>]` — create and check out `<branchname>`
/// at the stash's base commit, apply the stash there (so a stash made on a since-
/// changed branch applies cleanly), then drop it. Port of `branch_stash`.
fn branch_stash(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut positionals: Vec<&str> = Vec::new();
    for a in args {
        // `branch_stash()`'s table is `OPT_END()` alone, so nothing resolves and
        // every dashed word is git's own `unknown option`/`unknown switch` with
        // the block on stderr at 129.
        if let Err(code) = canonical(a, NO_OPTS, BRANCH_USAGE) {
            return Ok(code);
        }
        if a.starts_with('-') && a.as_str() != "-" {
            return Ok(usage_error(a, BRANCH_USAGE));
        }
        positionals.push(a);
    }
    let branch = match positionals.first() {
        Some(b) => (*b).to_string(),
        None => {
            eprintln!("No branch name specified");
            return Ok(ExitCode::FAILURE);
        }
    };
    // `branch_stash()` takes any stash-like commit; only an entry of the reflog
    // is dropped afterwards (`is_stash_ref`).
    let specs: Vec<String> = positionals[1..].iter().map(|s| (*s).to_string()).collect();
    let stash = match resolve_stash(repo, &specs)? {
        Ok(spec) => spec,
        Err(code) => return Ok(code),
    };
    let commit_id = stash.id;
    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.len() < 2 {
        crate::git_fatal!("'{commit_id}' is not a stash-like commit");
    }
    let b_commit = parents[0];

    // `git checkout -b <branch> <b_commit>`; only apply if the checkout succeeds.
    super::checkout::checkout(&["-b".to_string(), branch, b_commit.to_string()])?;

    // The checkout moved HEAD/worktree on disk; re-open so the restore sees it.
    // `branch_stash` calls `do_apply_stash(..., 1, ...)`: the index is always
    // restored here, whatever `stash.index` says, so the staged state a stash
    // captured comes back staged on the new branch.
    let repo = gix::discover(".")?;
    let restored = match restore_stash_commit(&repo, commit_id, true, &ConflictLabels::default())? {
        Ok(restored) => restored,
        Err(code) => return Ok(code),
    };

    // `do_apply_stash` ends by running `git status` (non-quiet).
    super::status::status(&[])?;

    // An untracked path that could not be written keeps the entry, as for `pop`.
    if !restored {
        return Ok(ExitCode::from(1));
    }

    // Only a `refs/stash` entry is dropped — `is_stash_ref` is false for a
    // stash-like commit named directly, and git leaves that one alone.
    if stash.is_stash_ref {
        return do_drop_stash(&repo, &stash, false);
    }
    Ok(ExitCode::SUCCESS)
}

/// `git stash list` — newest first, `stash@{N}: <reflog message>`.
fn list(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    // `git stash list` is `git log --format="%gd: %gs" -g <log-opts> refs/stash`,
    // so delegate to the reflog machinery on `refs/stash` rather than duplicate its
    // format engine (`%H`, `%gd`, `%gs`, dates, `--pretty`, …). Only the default
    // format differs: stash uses `%gd: %gs`, injected when the caller gives none.
    // With no stash ref, git prints nothing (exit 0) — reflog would instead fatal
    // on an unknown ref, so short-circuit that here.
    if repo.try_find_reference("refs/stash")?.is_none() {
        return Ok(ExitCode::SUCCESS);
    }
    let has_format = args
        .iter()
        .any(|a| a.starts_with("--format") || a.starts_with("--pretty"));
    let mut rf: Vec<String> = Vec::new();
    if !has_format {
        rf.push("--format=%gd: %gs".into());
    }
    // `list_stash()` runs `log -g --first-parent`, and the `--first-parent` is
    // what gives a diff option anything to render: every stash entry is a merge
    // commit, which is otherwise skipped. Without it `stash list --name-only`
    // silently prints subjects alone. It has to go in through `log`'s entry
    // point, not `git reflog show`'s — only the former installs the tweak hook
    // that makes `--first-parent` mean that.
    rf.push("--first-parent".into());
    rf.extend(args.iter().cloned());
    rf.push("refs/stash".into());
    // `list_stash()` ends on `return run_command(&cp)`, and `cmd_stash` returns
    // `!!fn(...)` (builtin/stash.c:2496), so the child's status only survives as
    // "did it fail": `git stash list --zzbogus` prints `git log`'s
    // `fatal: unrecognized argument: --zzbogus` and then exits **1**, not 128.
    // The forwarded parser's message is git's; the forwarded parser's exit code
    // is not.
    let status = super::reflog_show_as_log_status(&rf)?;
    Ok(ExitCode::from(u8::from(status != 0)))
}

/// `git stash apply` / `pop` — restore `stash@{n}` onto a clean worktree+index.
///
/// Port of `do_apply_stash`'s tail: the restore, then `git status` (skipped under
/// `-q`), and for `pop` the `Dropped …` line — which `-q` silences too.
fn apply_or_pop(repo: &gix::Repository, opts: &ApplyOptions, pop: bool) -> Result<ExitCode> {
    let stash = match resolve_stash(repo, &opts.specs)? {
        Ok(spec) => spec,
        Err(code) => return Ok(code),
    };
    // `pop` rewrites the reflog afterwards, so — unlike `apply` — it takes only
    // an entry of it.
    if pop {
        if let Some(code) = require_stash_ref(&stash) {
            return Ok(code);
        }
    }
    let commit_id = stash.id;

    let restored = match restore_stash_commit(repo, commit_id, opts.restore_index, &opts.labels)? {
        Ok(restored) => restored,
        // `--index` could not replay the stash's staged state onto ours: git
        // reports it and stops before the worktree is touched — and a `pop` says
        // the entry survived, as it does for every other failure.
        Err(code) => {
            if pop {
                println!("The stash entry is kept in case you need it again.");
            }
            return Ok(code);
        }
    };

    if !opts.quiet {
        super::status::status(&[])?;
    }
    // `pop` drops the entry only once the apply succeeded; an untracked path it
    // could not write keeps the stash, and says so.
    if !restored {
        if pop {
            println!("The stash entry is kept in case you need it again.");
        }
        return Ok(ExitCode::from(1));
    }
    if pop {
        return do_drop_stash(repo, &stash, opts.quiet);
    }
    Ok(ExitCode::SUCCESS)
}

/// Restore the stash onto the current tree with a three-way merge, shared by
/// `apply`/`pop` and `branch`.
///
/// `Err(code)` is `do_apply_stash`'s early refusal: `--index` asks for the
/// stash's staged state to be replayed onto the current index (`apply_cached()`),
/// and when that patch does not apply git says so and stops — before the merge,
/// so nothing on disk moves.
///
/// `restore_index` is `do_apply_stash`'s `index` argument: with it the index is
/// rebuilt from the stash's `I` (staged) tree, so what was staged when the stash
/// was made is staged again; without it the index is reset to the stash's base,
/// which is what leaves every restored change *unstaged* — git's default, and
/// the reason a plain `git stash apply` reports ` M` rather than `M ` for a path
/// that had been `git add`ed. `git stash branch` always passes `true`.
/// Returns `false` when an untracked path could not be restored — git's
/// `restore_untracked()` failure, which fails the apply without undoing what it
/// did manage to write.
fn restore_stash_commit(
    repo: &gix::Repository,
    commit_id: ObjectId,
    restore_index: bool,
    labels: &ConflictLabels,
) -> Result<std::result::Result<bool, ExitCode>> {
    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.len() < 2 {
        crate::git_fatal!("'{commit_id}' is not a stash-like commit");
    }
    // A third parent is the untracked capture written by `push -u`/`-a`; its
    // tree is restored to the worktree after the tracked paths, below.
    let u_tree = match parents.get(2) {
        Some(id) => Some(repo.find_commit(*id)?.tree_id()?.detach()),
        None => None,
    };
    if parents.len() > 3 {
        crate::git_fatal!("'{commit_id}' is not a stash-like commit");
    }
    let base_tree = repo.find_commit(parents[0])?.tree_id()?.detach();
    let i_tree = repo.find_commit(parents[1])?.tree_id()?.detach();
    let w_tree = commit.tree_id()?.detach();

    // `do_apply_stash` is a three-way merge, not a checkout: the stash's base is
    // the merge base, the *current index* is ours, and the stash's worktree tree
    // is theirs. That is what lets a stash land on a moved HEAD or next to
    // unrelated local work — and what makes the local-changes refusal below
    // git's own, since merge-ort ends in the same `unpack_trees` gate every
    // merge does.
    let old_index = repo.open_index()?;
    if old_index.entries().iter().any(|e| e.stage_raw() != 0) {
        crate::git_fatal!("cannot apply a stash in the middle of a merge");
    }
    let c_tree = super::merge::index_tree(repo, &old_index)?;

    // `--index` replays the stash's staged state onto the current index first
    // (`apply_cached()` of the `w_commit^!` diff), and a patch that does not
    // apply ends the whole command right there — before the worktree merge, so a
    // refusal leaves everything as it was. The equivalent test is whether the
    // stash's index tree merges into ours over the stash's base without
    // conflicts. The patch git-apply could not place is named the same way as
    // for `-S`: the diff here is the stash's own (`w_commit^!`), so its hunks
    // are the ones that failed.
    let has_index = restore_index && i_tree != base_tree && i_tree != c_tree;
    if has_index {
        if let Err(blocked) = merge_trees_cleanly(repo, base_tree, c_tree, i_tree)? {
            for path in blocked {
                if let Some(line) = first_hunk_line(repo, base_tree, i_tree, &path) {
                    eprintln!("error: patch failed: {path}:{line}");
                }
                eprintln!("error: {path}: patch does not apply");
            }
            eprintln!("error: conflicts in index. Try without --index.");
            return Ok(Err(ExitCode::FAILURE));
        }
    }

    // `merge_trees()` (merge-recursive.c): a merge whose base already equals the
    // other side has nothing to bring in and says so on stdout, `-q` included — which
    // is what an entry holding only untracked files looks like from here.
    if w_tree == base_tree {
        println!("Already up to date.");
    }

    let should_interrupt = AtomicBool::new(false);
    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: labels.base.as_deref().map(|l| BStr::new(l.as_bytes())),
        // `o.branch1` / `o.branch2` — the names a conflict's markers carry.
        current: Some(BStr::new(labels.ours.as_bytes())),
        other: Some(BStr::new(labels.theirs.as_bytes())),
    };
    let merged = crate::merge_apply::three_way_merge_guarded(
        repo,
        base_tree,
        c_tree,
        w_tree,
        &old_index,
        labels,
        &should_interrupt,
        true,
        &crate::merge_apply::StrategyOptions::default(),
        c_tree,
    )?;
    // A merge that did not come out clean — an unresolved conflict, or a checkout
    // the local changes refused — leaves the index restore undone. git says so
    // when `--index` asked for one, then jumps straight to the untracked files:
    // `if (ret) { … if (index) "Index was not unstashed."; goto restore_untracked; }`.
    let mut applied = match merged {
        crate::merge_apply::Merged::Applied(applied) => Some(applied),
        crate::merge_apply::Merged::Refused(clobber) => {
            clobber.report("merge");
            None
        }
    };
    // `merge_switch_to_result()`'s `write_auto_merge` region again — `stash
    // apply` merges through `merge_ort_nonrecursive()` like everything else, so
    // a stash that really merges leaves `.git/AUTO_MERGE` behind and
    // `git diff AUTO_MERGE` works on the result.
    //
    // Both of that wrapper's early returns are honoured: it bails before
    // `merge_switch_to_result()` when the base already equals the other side
    // (the `Already up to date.` above, which is what an untracked-only entry
    // looks like), and a refused checkout returns from inside
    // `merge_switch_to_result()` before the region is entered.
    if w_tree != base_tree {
        if let Some(a) = applied.as_ref() {
            crate::merge_apply::write_auto_merge(repo, a.tree_id)?;
        }
    }
    let merged_cleanly = applied.as_ref().is_some_and(|a| a.conflicts.is_empty());
    if !merged_cleanly {
        // git leaves the conflicted worktree in place and fails the apply; the
        // caller keeps the stash and reports it.
        if let Some(applied) = applied.as_mut() {
            applied.index.write(Default::default())?;
        }
        if restore_index {
            eprintln!("Index was not unstashed.");
        }
    }

    // `do_apply_stash`'s tail: the index goes back to what it was (so everything
    // the merge brought in shows up unstaged), or — under `--index` — to the
    // stash's own staged tree. Worktree stats come from the merge's own index so
    // a following `status` does not re-hash every file.
    if let Some(applied) = applied.filter(|_| merged_cleanly) {
        let fresh: HashMap<BString, Stat> = {
            let backing = applied.index.path_backing();
            applied
                .index
                .entries()
                .iter()
                .map(|e| (e.path_in(backing).to_owned(), e.stat))
                .collect()
        };
    //
    // `--index` restores the stash's staged state, but `do_apply_stash` first
    // checks whether the stash *had* one: with `i_tree` equal to the base (the
    // stash staged nothing) or already equal to ours, it sets `has_index = 0`
    // and leaves the index alone — which is what keeps the caller's own staged
    // work staged.
        let index_tree = if has_index {
            i_tree
        } else {
            unstage_changes_unless_new(repo, c_tree, applied.tree_id)?
        };
        write_target_index(repo, index_tree, &old_index, &fresh)?;
    }

    // Untracked files come back last and stay untracked — their stats are
    // deliberately dropped rather than fed to the index.
    //
    // `restore_untracked()` reads the `^3` tree into a scratch index and runs
    // `checkout-index --all` over it, which is best-effort: a path already on
    // disk is reported and skipped while every other path is still written, and
    // the summary error comes afterwards. Refusing the whole restore instead
    // would leave the *other* untracked files sitting in a stash the user has no
    // reason to suspect still holds them.
    let mut untracked_ok = true;
    if let Some(u_tree) = u_tree {
        let u_map = tree_map(repo, u_tree)?;
        let mut writable: HashSet<BString> = HashSet::new();
        // `checkout-index` reports collisions in index order, which is the tree's
        // sorted path order.
        let mut blocked: Vec<&BString> = Vec::new();
        for path in u_map.keys() {
            let occupied = repo
                .workdir_path(path.as_bstr())
                .is_some_and(|full| full.symlink_metadata().is_ok());
            if occupied {
                blocked.push(path);
            } else {
                writable.insert(path.clone());
            }
        }
        blocked.sort();
        for path in blocked {
            eprintln!("{path} already exists, no checkout");
            untracked_ok = false;
        }
        let _ = sync_worktree(repo, u_tree, &writable, &u_map, &should_interrupt)?;
    }
    if !untracked_ok {
        eprintln!("error: could not restore untracked files from stash");
    }
    Ok(Ok(merged_cleanly && untracked_ok))
}

/// Create an *autostash* from the current dirty worktree+index: build the
/// stash-like `W` commit (as `git stash create` does) and reset the tracked
/// worktree and index back to `HEAD`, leaving a clean tree. Returns the `W`
/// commit id. Unlike [`push`] it never touches `refs/stash` — the caller
/// (rebase/pull `--autostash`) owns the commit and re-applies it directly via
/// [`apply_autostash`]. The caller has already checked the tree is dirty.
pub fn create_autostash(repo: &gix::Repository) -> Result<ObjectId> {
    let StashBuild { w_commit, head_tree_id, affected, .. } =
        build_stash_commit(repo, repo, Some("autostash"), &PushOpts::with_message(None))?;

    // Reset the tracked worktree + index back to HEAD (untracked files untouched),
    // exactly as `push` does after storing the stash.
    let head_map = tree_map(repo, head_tree_id)?;
    let should_interrupt = AtomicBool::new(false);
    let fresh = sync_worktree(repo, head_tree_id, &affected, &head_map, &should_interrupt)?;
    let old_index = repo.open_index()?;
    write_target_index(repo, head_tree_id, &old_index, &fresh)?;
    Ok(w_commit)
}

/// Re-apply an autostash `W` commit onto the *current* `HEAD` with a real
/// three-way merge — the case `apply`/`pop` refuse. The three sides are the
/// stash's base (`W`'s first parent tree = `HEAD` when the stash was made),
/// *ours* (the current `HEAD` tree, e.g. the just-rebased tip), and *theirs*
/// (the stashed worktree tree `W`). Prints git's `Applied autostash.` on a clean
/// apply, or the conflict notice (leaving the changes recoverable) otherwise.
/// Returns the conflicted paths (empty on a clean apply).
pub fn apply_autostash(repo: &gix::Repository, commit_id: ObjectId, quiet: bool) -> Result<Vec<BString>> {
    let commit = repo.find_commit(commit_id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.len() < 2 {
        crate::git_fatal!("'{commit_id}' is not a stash-like commit");
    }
    let base = repo.find_commit(parents[0])?.tree_id()?.detach();
    let theirs = commit.tree_id()?.detach();
    let ours = repo.head_tree_id()?.detach();
    let old_index = repo.index_or_load_from_head()?.into_owned();

    // `do_apply_stash()`'s labels, which is what a stash conflict is marked with
    // wherever it is applied from: `Updated upstream` / `Stash base` / `Stashed changes`.
    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some(BStr::new(b"Stash base")),
        current: Some(BStr::new(b"Updated upstream")),
        other: Some(BStr::new(b"Stashed changes")),
    };
    let should_interrupt = AtomicBool::new(false);
    // `apply_autostash()` re-applies with `git stash apply --index --quiet`, so the
    // merge's own `Auto-merging`/`CONFLICT` block is suppressed; the caller reports
    // the outcome in git's own words.
    let applied = crate::merge_apply::three_way_merge_verbose(
        repo,
        base,
        ours,
        theirs,
        &old_index,
        labels,
        &should_interrupt,
        !quiet,
    )?;
    // The re-apply is a `git stash apply` child like any other, so it records
    // the merged tree as `AUTO_MERGE` too. It only outlives the command when
    // nothing removes it afterwards: `finish()` (builtin/merge.c:539) applies
    // the autostash and its caller then runs `remove_merge_branch_state()`, so a
    // `git merge --autostash` that completes leaves none, while a `rebase
    // --autostash` — whose `finish_rebase()` has no such cleanup — does.
    if base != theirs {
        crate::merge_apply::write_auto_merge(repo, applied.tree_id)?;
    }

    if applied.conflicts.is_empty() {
        // three_way_merge already wrote the merged content to the worktree. git's
        // autostash re-applies with `stash apply` (no `--index`), so the restored
        // changes stay UNSTAGED: reset the index to HEAD rather than persisting the
        // merged index, leaving worktree-vs-index as the user's local changes.
        let mut head_index = repo.index_from_tree(&ours)?;
        head_index.write(Default::default())?;
        if !quiet {
            // `apply_save_autostash_oid()` reports this on **stderr**, alongside
            // every other line the autostash machinery prints.
            eprintln!("Applied autostash.");
        }
    } else {
        // Keep the conflicted index (stages 1/2/3) so the user can resolve, exactly
        // as a conflicting `git stash apply` leaves it.
        let mut index = applied.index;
        index.write(Default::default())?;
        if !quiet {
            // git keeps the changes recoverable in the stash on a conflicting apply.
            eprintln!("Applying autostash resulted in conflicts.");
            eprintln!("Your changes are safe in the stash.");
            eprintln!("You can run \"git stash pop\" or \"git stash drop\" at any time.");
        }
    }
    Ok(applied.conflicts)
}

/// `git stash clear` — remove every entry (ref + reflog), silently if none.
fn clear(repo: &gix::Repository) -> Result<ExitCode> {
    delete_stash_ref(repo)?;
    Ok(ExitCode::SUCCESS)
}

/// `do_clear_stash()`: `delete_ref(NULL, ref_stash, …)`.
///
/// The deletion goes through the ref store rather than unlinking
/// `.git/refs/stash`, because `refs/stash` can just as well live in
/// `packed-refs` — after a `git pack-refs --all` it usually does. Removing only
/// the loose file leaves the packed one resolving to the cleared stash, so the
/// commit stays reachable and `git rev-parse refs/stash` still answers.
fn delete_stash_ref(repo: &gix::Repository) -> Result<()> {
    if repo.try_find_reference("refs/stash")?.is_none() {
        return Ok(());
    }
    repo.edit_reference(RefEdit {
        change: Change::Delete { expected: PreviousValue::Any, log: RefLog::AndReference },
        name: "refs/stash".try_into().map_err(|e| anyhow!("invalid ref name refs/stash: {e}"))?,
        deref: false,
    })?;
    // The reflog is the stash's list, so it goes with the ref whether or not the
    // ref store keeps logs of deleted references around.
    let _ = std::fs::remove_file(repo.common_dir().join("logs/refs/stash"));
    Ok(())
}

// ---------------------------------------------------------------------------
// Tree / index / worktree helpers
// ---------------------------------------------------------------------------

/// Convert an index entry mode to the tree entry kind used by the tree editor.
fn entry_kind(mode: Mode) -> Result<EntryKind> {
    Ok(mode
        .to_tree_entry_mode()
        .ok_or_else(|| anyhow!("index entry has an invalid mode"))?
        .into())
}

/// Flatten a tree into `path -> (blob id, mode)`.
fn tree_map(repo: &gix::Repository, tree_id: ObjectId) -> Result<HashMap<BString, (ObjectId, Mode)>> {
    let idx = repo.index_from_tree(&tree_id)?;
    let backing = idx.path_backing();
    let mut map = HashMap::with_capacity(idx.entries().len());
    for e in idx.entries() {
        map.insert(e.path_in(backing).to_owned(), (e.id, e.mode));
    }
    Ok(map)
}

/// Check out `affected` paths from `tree_id` into the worktree (overwriting),
/// deleting affected paths that don't exist in the target. Returns the fresh
/// filesystem stats produced for the written files, for index stat reuse.
fn sync_worktree(
    repo: &gix::Repository,
    tree_id: ObjectId,
    affected: &HashSet<BString>,
    target_map: &HashMap<BString, (ObjectId, Mode)>,
    should_interrupt: &AtomicBool,
) -> Result<HashMap<BString, Stat>> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Restrict a fresh target-tree index to just the affected, present paths.
    let mut subset = repo.index_from_tree(&tree_id)?;
    subset.remove_entries(|_, path, _| !affected.contains(&path.to_owned()));

    let mut opts = repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;

    let mut fresh = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            fresh.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Affected paths absent from the target tree are deletions.
    for path in affected {
        if !target_map.contains_key(path) {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // A checkout over an existing file truncates it in place, which keeps the
    // permissions the worktree copy already had — so a `chmod +x` would survive
    // the reset and leave the tree dirty right after a `stash push` reported it
    // saved. git's `reset --hard` puts the tree's mode back, so the executable
    // bit is restored explicitly here.
    #[cfg(unix)]
    for path in affected {
        let Some((_, mode)) = target_map.get(path) else { continue };
        let want_exec = match *mode {
            Mode::FILE => false,
            Mode::FILE_EXECUTABLE => true,
            // Symlinks and gitlinks carry no permission bits of their own.
            _ => continue,
        };
        let Some(full) = repo.workdir_path(path.as_bstr()) else { continue };
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        if !meta.is_file() {
            continue;
        }
        use std::os::unix::fs::PermissionsExt;
        let mut perms = meta.permissions();
        let current = perms.mode();
        let wanted = if want_exec { current | 0o111 } else { current & !0o111 };
        if wanted != current {
            perms.set_mode(wanted);
            let _ = std::fs::set_permissions(&full, perms);
        }
    }

    Ok(fresh)
}

/// Remove the directories `paths` leave empty, innermost first, stopping at the
/// worktree root.
///
/// git deletes the untracked files a `push -u` captured with
/// `git clean --force --quiet -d :/` (builtin/stash.c), and `-d` is what takes
/// the now-empty directories with them — without this a `push -u` leaves the
/// skeleton of every untracked directory behind.
fn remove_emptied_dirs(repo: &gix::Repository, paths: &[BString]) {
    let Some(root) = repo.workdir().and_then(|r| r.canonicalize().ok()) else { return };
    // Canonical throughout, so the root comparison holds on a workdir reached
    // through a symlink (`/tmp` → `/private/tmp` on macOS) — the parents walked
    // to below stay canonical because they are prefixes of a canonical path.
    let mut dirs: Vec<std::path::PathBuf> = paths
        .iter()
        .filter_map(|p| repo.workdir_path(p.as_bstr()))
        .filter_map(|p| p.parent().and_then(|d| d.canonicalize().ok()))
        .filter(|d| *d != root && d.starts_with(&root))
        .collect();
    // Deepest first, so a nested directory is gone before its parent is tried.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for dir in dirs {
        let mut current = dir;
        // `remove_dir` refuses a non-empty directory, which is exactly the stop
        // condition — and the root is never removed.
        while current != root
            && current.starts_with(&root)
            && std::fs::remove_dir(&current).is_ok()
        {
            let Some(parent) = current.parent().map(std::path::Path::to_path_buf) else { break };
            current = parent;
        }
    }
}

/// `unstage_changes_unless_new()` (builtin/stash.c): the index a plain
/// `stash apply` leaves behind.
///
/// The merge stages everything it resolved, and git then walks
/// `orig_tree`→index and restores each path's entry *from `orig_tree`* — but
/// only `if (p->one->oid_valid)`, i.e. only for paths that existed there. A path
/// the stash added is not in `orig_tree`, so its merged entry survives and the
/// file comes back **staged**, which is why `git stash apply` of a stash that
/// had `git add`ed a new file reports `A ` and not `??`.
///
/// The tree returned is therefore `orig_tree` plus the merged tree's entries for
/// paths `orig_tree` does not have.
fn unstage_changes_unless_new(
    repo: &gix::Repository,
    orig_tree: ObjectId,
    merged_tree: ObjectId,
) -> Result<ObjectId> {
    let orig = tree_map(repo, orig_tree)?;
    let merged = tree_map(repo, merged_tree)?;
    let mut editor = repo.edit_tree(orig_tree)?;
    let mut added = false;
    for (path, (id, mode)) in &merged {
        if orig.contains_key(path) {
            continue;
        }
        editor.upsert(path.as_bstr(), entry_kind(*mode)?, *id)?;
        added = true;
    }
    if !added {
        return Ok(orig_tree);
    }
    Ok(editor.write()?.detach())
}

/// `base` with `paths` taken from `source` instead — the tree the index should
/// end up at when only some paths are being reset.
///
/// A path absent from `source` was newly added, so reverting it means dropping
/// it from the tree entirely.
fn revert_paths_in_tree(
    repo: &gix::Repository,
    base: ObjectId,
    source: ObjectId,
    paths: &HashSet<BString>,
) -> Result<ObjectId> {
    if paths.is_empty() {
        return Ok(base);
    }
    let source_map = tree_map(repo, source)?;
    let mut editor = repo.edit_tree(base)?;
    for path in paths {
        match source_map.get(path) {
            Some((id, mode)) => {
                editor.upsert(path.as_bstr(), entry_kind(*mode)?, *id)?;
            }
            None => {
                editor.remove(path.as_bstr())?;
            }
        }
    }
    Ok(editor.write()?.detach())
}

/// Write the on-disk index to the state of `tree_id`, reusing `fresh` stats for
/// just-written files and the previous index stats for entries that didn't move,
/// so the next status check stays cheap.
fn write_target_index(
    repo: &gix::Repository,
    tree_id: ObjectId,
    old_index: &gix::index::File,
    fresh: &HashMap<BString, Stat>,
) -> Result<()> {
    let mut new_index = repo.index_from_tree(&tree_id)?;

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat, gix::index::entry::Flags)> =
        HashMap::with_capacity(old_index.entries().len());
    {
        let backing = old_index.path_backing();
        for e in old_index.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat, e.flags));
        }
    }

    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = fresh.get(&path) {
                e.stat = *stat;
            } else if let Some((id, mode, stat, _)) = old_map.get(&path) {
                if *id == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
            // A tree carries no `SKIP_WORKTREE`, so rebuilding the index from one would
            // drop the sparse-checkout state and report every out-of-cone path as
            // deleted. git keeps the bit (`update_sparsity()` leaves those entries
            // alone), so it is carried across here.
            if old_map
                .get(&path)
                .is_some_and(|(_, _, _, flags)| flags.contains(gix::index::entry::Flags::SKIP_WORKTREE))
            {
                // The bit only reaches the file through the v3 extended-flag word, which
                // `EXTENDED` is what asks for.
                e.flags |= gix::index::entry::Flags::SKIP_WORKTREE | gix::index::entry::Flags::EXTENDED;
            }
        }
    }

    new_index.remove_tree();
    new_index.write(gix::index::write::Options::default())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reflog helpers
// ---------------------------------------------------------------------------

/// Remove `stash@{n}` from the reflog, rewriting the chain and repointing the
/// ref, exactly like `git reflog delete --rewrite --updateref stash@{n}`.
/// Returns the dropped commit id.
fn drop_reflog_entry(repo: &gix::Repository, n: usize) -> Result<ObjectId> {
    let common = repo.common_dir();
    let log_path = common.join("logs/refs/stash");

    let data = std::fs::read(&log_path).map_err(|_| anyhow!("No stash entries found."))?;
    // Reflog lines are stored oldest-first, one per line.
    let mut lines: Vec<Vec<u8>> = data.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(<[u8]>::to_vec).collect();
    let len = lines.len();
    if n >= len {
        crate::git_fatal!("stash@{{{n}}} is not a valid reference");
    }
    let target = len - 1 - n; // stash@{0} is the last (newest) line

    let dropped = parse_new_oid(&lines[target])?;

    // Preserve chain consistency: the entry after the dropped one inherits the
    // dropped entry's previous oid (its new "old" side).
    if target + 1 < len {
        let prev = field_prev(&lines[target])?.to_vec();
        set_prev(&mut lines[target + 1], &prev)?;
    }
    lines.remove(target);

    if lines.is_empty() {
        // The last entry: `--updateref` on an emptied log deletes the ref, which
        // has to go through the ref store so a packed `refs/stash` goes too.
        delete_stash_ref(repo)?;
    } else {
        let newest = parse_new_oid(lines.last().expect("non-empty"))?;
        // Move the ref through the ref store (loose or packed, whichever holds
        // it), then lay down the rewritten log — the update appends an entry of
        // its own, and `reflog delete --rewrite` leaves no such trace.
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "reflog delete".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(newest),
            },
            name: "refs/stash"
                .try_into()
                .map_err(|e| anyhow!("invalid ref name refs/stash: {e}"))?,
            deref: false,
        })?;
        let mut out = Vec::with_capacity(data.len());
        for l in &lines {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
        std::fs::write(&log_path, &out)?;
    }

    Ok(dropped)
}

/// Byte offsets of the first two spaces in a reflog line (`<old> <new> …`).
fn split2(line: &[u8]) -> Result<(usize, usize)> {
    let s1 = line.iter().position(|b| *b == b' ').ok_or_else(|| anyhow!("malformed reflog line"))?;
    let s2 = line[s1 + 1..]
        .iter()
        .position(|b| *b == b' ')
        .map(|p| p + s1 + 1)
        .ok_or_else(|| anyhow!("malformed reflog line"))?;
    Ok((s1, s2))
}

fn parse_new_oid(line: &[u8]) -> Result<ObjectId> {
    let (s1, s2) = split2(line)?;
    ObjectId::from_hex(&line[s1 + 1..s2]).map_err(|e| anyhow!("invalid oid in reflog: {e}"))
}

fn field_prev(line: &[u8]) -> Result<&[u8]> {
    let (s1, _) = split2(line)?;
    Ok(&line[..s1])
}

fn set_prev(line: &mut Vec<u8>, prev: &[u8]) -> Result<()> {
    let (s1, _) = split2(line)?;
    line.splice(0..s1, prev.iter().copied());
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

/// Every non-flag argument — `parse_options()`'s leftovers, which is what
/// `get_stash_info()` counts when it refuses more than one revision.
/// `drop_stash()`'s option table is a single `OPT__QUIET(&quiet, …)` parsed with
/// no `PARSE_OPT_STOP_AT_NON_OPTION`, so an option may follow the `<stash>`
/// operand. Returns `(quiet, operands)`.
///
/// The point of parsing rather than filtering: [`positionals`] drops every
/// dash-prefixed word on the floor, so `git stash drop --zzbogus` used to fall
/// through to the default `stash@{0}` and delete it. An unrecognized option has
/// to be refused *before* anything is dropped.
///
/// With one entry in the table an abbreviation can never be ambiguous, so
/// `--q`, `--quie`, `--n` and `--no` all resolve — as they do in stock.
fn parse_drop_options(args: &[String]) -> std::result::Result<(bool, Vec<String>), ExitCode> {
    let mut quiet = false;
    let mut named = Vec::new();
    let mut only_names = false;

    for a in args {
        let a = a.as_str();
        if only_names || !a.starts_with('-') || a == "-" {
            named.push(a.to_string());
            continue;
        }
        if let Some(body) = a.strip_prefix("--") {
            if body.is_empty() || body == "end-of-options" {
                only_names = true;
                continue;
            }
            // The whole table is `OPT__QUIET`, so the only question is which
            // sense the name resolves to — including the `--n`/`--no` spelling
            // parse_long_opt reaches through "negated and abbreviated very much".
            // The negation has to be matched on the resolved *name*, not on a
            // bare `--no-` prefix: the table holds `quiet` alone, so a name it
            // does not hold comes back from the resolver untouched, and testing
            // only the prefix read every unknown `--no-<x>` as `--no-quiet` and
            // dropped the stash git refuses to drop.
            let set = match canonical(a, DROP_OPTS, DROP_USAGE)? {
                name if name.starts_with("--no-quiet") => false,
                name if name.starts_with("--quiet") => true,
                _ => return Err(usage_error(a, DROP_USAGE)),
            };
            if body.contains('=') {
                let shown = match set {
                    true => "quiet",
                    false => "no-quiet",
                };
                eprintln!("error: option `{shown}' takes no value");
                return Err(ExitCode::from(129));
            }
            quiet = set;
            continue;
        }
        // Short options group; `-h` was answered before we got here.
        for c in a[1..].chars() {
            match c {
                'q' => quiet = true,
                _ => return Err(usage_error(&format!("-{c}"), DROP_USAGE)),
            }
        }
    }
    Ok((quiet, named))
}

/// What a `<stash>` argument resolved to — `get_stash_info()` (builtin/stash.c).
pub(crate) struct StashSpec {
    /// `parse_stash_revision()`'s expansion of the argument: `refs/stash@{0}`
    /// when none was given, `refs/stash@{<n>}` for a bare number, and the
    /// argument verbatim otherwise. Every message about the stash names *this*,
    /// never the object id — `fatal: 'HEAD' is not a stash-like commit`.
    revision: String,
    /// The stash-like commit it named.
    id: ObjectId,
    /// Whether the text before the revision's first `@` names `refs/stash`, which
    /// is what `assert_stash_ref()` demands of `drop` and `pop`. It is a property
    /// of the *spelling*, not of the commit: `stash@{0}`, `refs/stash@{0}`,
    /// `refs/stash` and even `stash@{0}~0` all pass, while the same commit named
    /// by its id does not.
    is_stash_ref: bool,
}

/// `parse_stash_revision()`: an absent argument is the newest entry, a run of
/// digits is that entry, and anything else is passed through untouched.
fn parse_stash_revision(repo: &gix::Repository, commit: Option<&str>) -> Result<Option<String>> {
    Ok(match commit {
        None => {
            if repo.try_find_reference("refs/stash")?.is_none() {
                eprintln!("No stash entries found.");
                return Ok(None);
            }
            Some("refs/stash@{0}".to_string())
        }
        // `strspn(commit, "0123456789") == strlen(commit)`.
        Some(s) if s.bytes().all(|b| b.is_ascii_digit()) => Some(format!("refs/stash@{{{s}}}")),
        Some(s) => Some(s.to_string()),
    })
}

/// The `@{<n>}` a revision carries, as `reflog_delete()` reads it: the digits
/// after `@{` up to the closing brace, with anything past the brace ignored —
/// which is why `stash@{0}~0` drops entry 0. The outer `None` means there is no
/// `@{` at all; the inner one means its payload is not a number, i.e. an
/// `@{<date>}` expiry spec.
fn reflog_recno(revision: &str) -> Option<Option<usize>> {
    let spec = revision.find("@{").map(|at| &revision[at + 2..])?;
    let digits = spec.len() - spec.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if !spec[digits..].starts_with('}') {
        return Some(None);
    }
    // An empty run is `strtoul`'s 0, as in git.
    Some(Some(spec[..digits].parse().unwrap_or(0)))
}

/// How many entries a ref's reflog holds, or `None` when the ref is unknown.
fn reflog_len(repo: &gix::Repository, name: &str) -> Result<Option<usize>> {
    let Some(reference) = repo.try_find_reference(name)? else {
        return Ok(None);
    };
    let mut platform = reference.log_iter();
    Ok(Some(match platform.all()? {
        Some(iter) => iter.count(),
        None => 0,
    }))
}

/// Resolve a `<stash>` argument the way `get_stash_info()` does: expand it with
/// `parse_stash_revision()`, resolve *that* string, and require the result to
/// look like a stash — two parents at least.
///
/// The refusals are git's, with git's exit codes: more than one revision is
/// `Too many revisions specified:` and the list (1), an out-of-range reflog
/// entry is `get_oid_basic()`'s `fatal: log for '<ref>' only has <n> entries`
/// (128), an unresolvable name is `error: <revision> is not a valid reference`
/// (1), and a commit that is not stash-shaped is
/// `fatal: '<revision>' is not a stash-like commit` (128).
fn resolve_stash(
    repo: &gix::Repository,
    specs: &[String],
) -> Result<std::result::Result<StashSpec, ExitCode>> {
    if specs.len() > 1 {
        let listed: String = specs.iter().map(|s| format!(" '{s}'")).collect();
        eprintln!("Too many revisions specified:{listed}");
        return Ok(Err(ExitCode::FAILURE));
    }
    let Some(revision) = parse_stash_revision(repo, specs.first().map(String::as_str))? else {
        return Ok(Err(ExitCode::FAILURE));
    };

    let id = match repo.rev_parse_single(revision.as_str()) {
        Ok(id) => id.detach(),
        Err(_) => {
            // An `@{<n>}` past the end of the log never reaches `get_stash_info`:
            // `get_oid_basic()` dies on it while parsing the name.
            if let Some(Some(n)) = reflog_recno(&revision) {
                let named = revision.split('@').next().unwrap_or(&revision);
                if let Some(count) = reflog_len(repo, named)? {
                    if n >= count {
                        eprintln!("fatal: log for '{named}' only has {count} entries");
                        return Ok(Err(ExitCode::from(128)));
                    }
                }
            }
            eprintln!("error: {revision} is not a valid reference");
            return Ok(Err(ExitCode::FAILURE));
        }
    };

    // `assert_stash_like()`: `<rev>^1` and `<rev>^2` both have to exist.
    let stash_like = repo
        .find_commit(id)
        .map(|c| c.parent_ids().count() >= 2)
        .unwrap_or(false);
    if !stash_like {
        eprintln!("fatal: '{revision}' is not a stash-like commit");
        return Ok(Err(ExitCode::from(128)));
    }

    // `repo_dwim_ref()` over everything up to the first `@`.
    let named = revision.split('@').next().unwrap_or(&revision);
    let is_stash_ref = repo
        .try_find_reference(named)
        .ok()
        .flatten()
        .is_some_and(|r| r.name().as_bstr() == "refs/stash");
    Ok(Ok(StashSpec { revision, id, is_stash_ref }))
}

/// `assert_stash_ref()`: `drop` and `pop` rewrite the reflog, so they refuse a
/// revision that does not name `refs/stash`.
fn require_stash_ref(spec: &StashSpec) -> Option<ExitCode> {
    if spec.is_stash_ref {
        return None;
    }
    eprintln!("error: '{}' is not a stash reference", spec.revision);
    Some(ExitCode::FAILURE)
}

/// `do_drop_stash()`: delete the reflog entry the revision names, then report it
/// under that same spelling.
///
/// The deletion is `reflog_delete(rev, REWRITE|UPDATE_REF)`, which needs an
/// `@{…}` in the revision — a bare `refs/stash` is not a reflog spec and is
/// refused as one — and reads a number out of it. An `@{<date>}` payload sends
/// git into `approxidate()`-driven expiry, which this port does not have; it
/// says so rather than guess an entry.
fn do_drop_stash(repo: &gix::Repository, spec: &StashSpec, quiet: bool) -> Result<ExitCode> {
    let rev = &spec.revision;
    let recno = match reflog_recno(rev) {
        Some(Some(n)) => n,
        Some(None) => {
            eprintln!("error: {rev}: an `@{{<date>}}` reflog spec is not ported");
            eprintln!("error: {rev}: Could not drop stash entry");
            return Ok(ExitCode::FAILURE);
        }
        None => {
            eprintln!("error: not a reflog: {rev}");
            eprintln!("error: {rev}: Could not drop stash entry");
            return Ok(ExitCode::FAILURE);
        }
    };
    let dropped = drop_reflog_entry(repo, recno)?;
    if !quiet {
        println!("Dropped {rev} ({dropped})");
    }
    Ok(ExitCode::SUCCESS)
}

/// Which files beyond the tracked changes a push takes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Untracked {
    /// Tracked changes only — git's default.
    No,
    /// `-u`: untracked files as well, but not ignored ones.
    Include,
    /// `-a`: untracked *and* ignored files.
    All,
}

/// Everything `stash push` accepts, after parsing.
pub(crate) struct PushOpts {
    pub message: Option<String>,
    pub quiet: bool,
    /// `-k`/`--no-keep-index`: leave the index staged after the reset. `None` is
    /// git's `keep_index = -1` — unset, which `--patch` turns into "keep".
    pub keep_index: Option<bool>,
    /// `-S`: stash the staged changes only, leaving unstaged work alone.
    pub staged_only: bool,
    pub untracked: Untracked,
    /// Empty means "everything", matching git's unrestricted push.
    pub pathspecs: Vec<String>,
    /// `-p`: pick the hunks to stash interactively.
    pub patch: bool,
    /// `-U<n>`/`--inter-hunk-context=<n>`/`--[no-]auto-advance`, which git parses
    /// into `struct interactive_options` and only `--patch` may use.
    pub interactive: super::add_patch::Options,
}

impl PushOpts {
    /// A plain push with just a message — what `save` and the bare form want.
    fn with_message(message: Option<String>) -> Self {
        PushOpts {
            message,
            quiet: false,
            keep_index: None,
            staged_only: false,
            untracked: Untracked::No,
            pathspecs: Vec::new(),
            patch: false,
            interactive: super::add_patch::Options::default(),
        }
    }
}

/// Parse `push` options.
///
/// Flag spellings follow `git stash push -h` on 2.55.0, including the `--no-`
/// negations git generates for every boolean. Note that `--only-untracked` is
/// *not* among them — git rejects it as an unknown option, so it is not
/// accepted here either.
fn parse_push_options(
    args: &[String],
    usage: &'static str,
) -> Result<std::result::Result<PushOpts, ExitCode>> {
    let mut o = PushOpts::with_message(None);
    let mut from_file: Option<String> = None;
    let mut nul = false;
    let mut rest_are_paths = false;
    // `OPT_DIFF_UNIFIED`/`OPT_DIFF_INTERHUNK_CONTEXT`/`--[no-]auto-advance`, the
    // hunk selector's knobs, parsed by the same helper `reset -p` uses.
    let mut patch_opts = super::reset::PatchDiffOpts::default();
    let mut i = 0;
    while i < args.len() {
        let orig = args[i].as_str();
        // Respell the token the way `parse_long_opt()` reads it. Two positions
        // are exempt because parse-options never looks them up: an argument owed
        // to `-U`/`--unified`/`--inter-hunk-context`, and everything past `--`.
        let resolved;
        let a: &str = if patch_opts.awaiting_value() || rest_are_paths {
            orig
        } else {
            resolved = match canonical(orig, PUSH_OPTS, usage) {
                Ok(name) => name,
                Err(code) => return Ok(Err(code)),
            };
            resolved.as_ref()
        };
        // A value still owed is taken verbatim, even past `--`; otherwise these
        // options are only recognised while options are still being read.
        if patch_opts.awaiting_value() || !rest_are_paths {
            match patch_opts.take_arg(a) {
                Err(code) => return Ok(Err(code)),
                Ok(true) => {
                    i += 1;
                    continue;
                }
                Ok(false) => {}
            }
        }
        if rest_are_paths {
            o.pathspecs.push(orig.to_string());
            i += 1;
            continue;
        }
        match a {
            "-m" | "--message" => {
                i += 1;
                let m = args.get(i).ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                o.message = Some(m.clone());
            }
            // `OPT_STRING`/`OPT_FILENAME`/`OPT_BOOL` negations: git NULLs the
            // pointer or clears the int, i.e. "as if never given".
            "--no-message" => o.message = None,
            "--no-pathspec-from-file" => from_file = None,
            "--no-pathspec-file-nul" => nul = false,
            "-q" | "--quiet" => o.quiet = true,
            "--no-quiet" => o.quiet = false,
            "-u" | "--include-untracked" => o.untracked = Untracked::Include,
            "--no-include-untracked" => o.untracked = Untracked::No,
            "-a" | "--all" => o.untracked = Untracked::All,
            "--no-all" => o.untracked = Untracked::No,
            "-k" | "--keep-index" => o.keep_index = Some(true),
            "--no-keep-index" => o.keep_index = Some(false),
            "-S" | "--staged" => o.staged_only = true,
            "--no-staged" => o.staged_only = false,
            "-p" | "--patch" => o.patch = true,
            "--no-patch" => o.patch = false,
            "--pathspec-file-nul" => nul = true,
            "--pathspec-from-file" => {
                i += 1;
                let f = args.get(i).ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                from_file = Some(f.clone());
            }
            "--" => rest_are_paths = true,
            other => {
                if let Some(f) = other.strip_prefix("--pathspec-from-file=") {
                    from_file = Some(f.to_string());
                } else if let Some(m) = other.strip_prefix("--message=") {
                    o.message = Some(m.to_string());
                } else if let Some(m) = other.strip_prefix("-m") {
                    o.message = Some(m.to_string());
                } else if other.starts_with('-') {
                    // `parse_options()`: the unknown flag, then the whole usage
                    // block, exit 129 — not a one-line `zvcs:` error.
                    return Ok(Err(usage_error(other, usage)));
                } else {
                    o.pathspecs.push(orig.to_string());
                }
            }
        }
        i += 1;
    }

    if let Err(code) = patch_opts.finish() {
        return Ok(Err(code));
    }

    if let Some(f) = from_file {
        // `push_stash()` refuses `--pathspec-from-file` against `--patch` and
        // `--staged` before it looks at the pathspec arguments.
        if o.patch {
            crate::git_fatal!(
                "options '--pathspec-from-file' and '--patch' cannot be used together"
            );
        }
        if o.staged_only {
            crate::git_fatal!(
                "options '--pathspec-from-file' and '--staged' cannot be used together"
            );
        }
        if !o.pathspecs.is_empty() {
            crate::git_fatal!("--pathspec-from-file is incompatible with pathspec arguments");
        }
        o.pathspecs = super::commit::read_pathspec_file(&f, nul)?;
    } else if nul {
        crate::git_fatal!("--pathspec-file-nul requires --pathspec-from-file");
    }

    // `push_stash()` checks the selector knobs in this order — `requires
    // '--patch'` first, `cannot be negative` second. `save_stash()` checks the
    // same two in the opposite order, which is why they are separate calls.
    if let Some(code) = patch_opts.require_patch_only(o.patch, "--patch") {
        return Ok(Err(code));
    }
    if let Some(code) = patch_opts.reject_negative() {
        return Ok(Err(code));
    }
    o.interactive = patch_opts.to_interactive(false);

    // git refuses the combination outright rather than picking a winner, on
    // stderr and unprefixed (`do_push_stash()`).
    if o.staged_only && o.untracked != Untracked::No {
        eprintln!("Can't use --staged and --include-untracked or --all at the same time");
        return Ok(Err(ExitCode::FAILURE));
    }
    // `--patch` and `-u`/`-a` describe two different things to capture, so git
    // takes neither; `--patch` silently wins over `--staged`.
    if o.patch && o.untracked != Untracked::No {
        eprintln!("Can't use --patch and --include-untracked or --all at the same time");
        return Ok(Err(ExitCode::FAILURE));
    }
    if o.patch {
        o.staged_only = false;
    }
    Ok(Ok(o))
}

/// `parse_options()`'s rejection: the unknown flag, then the subcommand's usage
/// block, then exit 129 — not a one-line `zvcs: stash: …` error, which is what a
/// caller checking `$?` for 129 sees. Both go to stderr, unlike the `-h` form.
/// A long option is named without its dashes, a short one by its letter alone,
/// matching parse-options' two spellings:
///
/// ```c
/// if (ctx.argv[0][1] == '-') {
///         error(_("unknown option `%s'"), ctx.argv[0] + 2);
/// } else if (isascii(*ctx.opt)) {
///         error(_("unknown switch `%c'"), *ctx.opt);
/// }
/// ```
fn usage_error(flag: &str, usage: &str) -> ExitCode {
    match flag.strip_prefix("--") {
        Some(long) => eprintln!("error: unknown option `{long}'"),
        None => eprintln!("error: unknown switch `{}'", &flag[1..]),
    }
    eprint!("{usage}");
    ExitCode::from(129)
}

/// Parse `save` options: `push`'s table minus the pathspec options, with the
/// message taken from the positional words rather than `-m`.
///
/// `save_stash()` (builtin/stash.c) hands `do_push_stash()` the same
/// `keep_index`/`patch_mode`/`include_untracked`/`only_staged` it parsed, so
/// everything `push` implements works here — the flags are not a smaller set,
/// only the message and pathspec handling differ.
fn parse_save_options(args: &[String]) -> Result<std::result::Result<PushOpts, ExitCode>> {
    let mut o = PushOpts::with_message(None);
    let mut words: Vec<String> = Vec::new();
    let mut rest_are_words = false;
    let mut patch_opts = super::reset::PatchDiffOpts::default();
    let mut i = 0;
    while i < args.len() {
        let orig = args[i].as_str();
        i += 1;
        let resolved;
        let a: &str = if patch_opts.awaiting_value() || rest_are_words {
            orig
        } else {
            resolved = match canonical(orig, SAVE_OPTS, SAVE_USAGE) {
                Ok(name) => name,
                Err(code) => return Ok(Err(code)),
            };
            resolved.as_ref()
        };
        if patch_opts.awaiting_value() || !rest_are_words {
            match patch_opts.take_arg(a) {
                Err(code) => return Ok(Err(code)),
                Ok(true) => continue,
                Ok(false) => {}
            }
        }
        if rest_are_words {
            words.push(orig.to_string());
            continue;
        }
        match a {
            "-q" | "--quiet" => o.quiet = true,
            "--no-quiet" => o.quiet = false,
            "-u" | "--include-untracked" => o.untracked = Untracked::Include,
            "--no-include-untracked" => o.untracked = Untracked::No,
            "-a" | "--all" => o.untracked = Untracked::All,
            "--no-all" => o.untracked = Untracked::No,
            "-k" | "--keep-index" => o.keep_index = Some(true),
            "--no-keep-index" => o.keep_index = Some(false),
            "-S" | "--staged" => o.staged_only = true,
            "--no-staged" => o.staged_only = false,
            "-p" | "--patch" => o.patch = true,
            "--no-patch" => o.patch = false,
            // `save` has `-m`/`--message` in its table too, but the positional
            // words win: `save_stash()` overwrites `stash_msg` with them whenever
            // `argc` is non-zero (stash.c:2049-2050), so the parsed value only
            // survives when nothing positional followed.
            "-m" | "--message" => {
                let Some(m) = args.get(i) else {
                    return Ok(Err(super::missing_option_value(a)));
                };
                i += 1;
                o.message = Some(m.clone());
            }
            "--no-message" => o.message = None,
            "--" => rest_are_words = true,
            other => {
                if let Some(m) = other.strip_prefix("--message=") {
                    o.message = Some(m.to_string());
                } else if let Some(m) = other.strip_prefix("-m") {
                    o.message = Some(m.to_string());
                } else if other.starts_with('-') && other != "-" {
                    return Ok(Err(usage_error(other, SAVE_USAGE)));
                } else {
                    words.push(orig.to_string());
                }
            }
        }
    }
    if let Err(code) = patch_opts.finish() {
        return Ok(Err(code));
    }
    // `save_stash()` checks these two in the opposite order to `push_stash()`:
    // `cannot be negative` first, `requires '--patch'` second.
    if let Some(code) = patch_opts.reject_negative() {
        return Ok(Err(code));
    }
    if let Some(code) = patch_opts.require_patch_only(o.patch, "--patch") {
        return Ok(Err(code));
    }
    o.interactive = patch_opts.to_interactive(false);
    if o.staged_only && o.untracked != Untracked::No {
        eprintln!("Can't use --staged and --include-untracked or --all at the same time");
        return Ok(Err(ExitCode::FAILURE));
    }
    if o.patch && o.untracked != Untracked::No {
        eprintln!("Can't use --patch and --include-untracked or --all at the same time");
        return Ok(Err(ExitCode::FAILURE));
    }
    if o.patch {
        o.staged_only = false;
    }
    // `if (argc) stash_msg = strbuf_join_argv(...)`: the words replace whatever
    // `-m` parsed, and an empty word list leaves that value alone.
    if !words.is_empty() {
        o.message = Some(words.join(" "));
    }
    Ok(Ok(o))
}

/// Parse `store` options: `-m/--message <msg>`, `-q/--quiet`, and exactly one
/// positional `<commit>`. Port of `store_stash`'s option table, which requires
/// precisely one non-option argument.
fn parse_store_options(args: &[String]) -> Result<(Option<String>, bool, String)> {
    let mut message = None;
    let mut quiet = false;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-m" | "--message" => {
                i += 1;
                let m = args.get(i).ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                message = Some(m.clone());
            }
            "-q" | "--quiet" => quiet = true,
            other => {
                if let Some(m) = other.strip_prefix("--message=") {
                    message = Some(m.to_string());
                } else {
                    positionals.push(other.to_string());
                }
            }
        }
        i += 1;
    }
    if positionals.len() != 1 {
        crate::git_fatal!("\"git stash store\" requires one <commit> argument");
    }
    Ok((message, quiet, positionals.into_iter().next().expect("exactly one")))
}

/// The resolved `git stash apply` / `git stash pop` command line.
struct ApplyOptions {
    /// The leftover positionals — the `<stash>` as typed, if any. They are
    /// resolved late because `apply` accepts any stash-like commit while `pop`
    /// requires a reflog entry, and because `get_stash_info()` is what refuses
    /// more than one of them.
    specs: Vec<String>,
    /// `--index`: rebuild the index from the stash's staged (`I`) tree.
    restore_index: bool,
    /// `-q`/`--quiet`: skip the trailing `git status` and `pop`'s `Dropped …`.
    quiet: bool,
    /// `--label-ours`/`--label-theirs`/`--label-base`: `o.branch1`/`o.branch2`/
    /// `o.ancestor`, the names the conflict markers carry. `apply` alone takes
    /// them; git's `pop` option table has no such entries.
    labels: ConflictLabels,
}

/// The conflict-marker names a stash apply uses, with git's defaults.
struct ConflictLabels {
    ours: String,
    theirs: String,
    base: Option<String>,
}

impl Default for ConflictLabels {
    fn default() -> Self {
        // `do_apply_stash()`: `Updated upstream` / `Stash base` / `Stashed changes`.
        ConflictLabels {
            ours: "Updated upstream".to_string(),
            theirs: "Stashed changes".to_string(),
            base: Some("Stash base".to_string()),
        }
    }
}

/// Parse the `apply`/`pop` option table (`-q`/`--quiet`, `--index`/`--no-index`,
/// at most one `<stash>`).
///
/// `stash.index` seeds `restore_index` before the loop, exactly as `git_config`
/// runs before `parse_options`, so an explicit `--no-index` on the command line
/// countermands the config and an explicit `--index` is redundant with it.
fn parse_apply_options(
    repo: &gix::Repository,
    args: &[String],
    table: &'static [LongOpt],
    usage: &'static str,
) -> Result<std::result::Result<ApplyOptions, ExitCode>> {
    let mut restore_index = repo.config_snapshot().boolean("stash.index").unwrap_or(false);
    let mut quiet = false;
    let mut specs: Vec<String> = Vec::new();
    let mut labels = ConflictLabels::default();
    /// Does the caller's table hold this long name at all? `POP_OPTS` and
    /// `DROP_OPTS` are prefixes of `APPLY_OPTS`, so the arms below have to ask
    /// rather than assume: an entry missing from the table never resolves, and
    /// `parse_long_opt()` answers `PARSE_OPT_UNKNOWN` for it.
    let held = |name: &str| table.iter().any(|o| o.name == name);
    let mut i = 0;
    while i < args.len() {
        let orig = args[i].as_str();
        i += 1;
        let resolved = match canonical(orig, table, usage) {
            Ok(name) => name,
            Err(code) => return Ok(Err(code)),
        };
        let a: &str = resolved.as_ref();

        // A `PARSE_OPT_NOARG` entry rejects an attached value outright:
        // `get_value()` answers ``error: option `%s' takes no value``
        // (parse-options.c:88-90) and exits 129 with no block. The name quoted
        // is the entry's, not the abbreviation's — `--q=x` names `quiet` — and
        // the `no-` sense keeps its own spelling, which is why this reads the
        // resolved token rather than the one typed.
        if let Some((stem, _)) = a.split_once('=') {
            let bare = stem.trim_start_matches('-');
            let named = bare.strip_prefix("no-").unwrap_or(bare);
            if table.iter().any(|o| o.name == named && o.arg == super::Arg::None) {
                eprintln!("error: option `{bare}' takes no value");
                return Ok(Err(ExitCode::from(129)));
            }
        }

        // The three conflict-label options are `OPT_STRING`s, so each takes its
        // value attached *or* as the next argument. They belong to `apply`
        // alone, which is why `held` gates the whole block on the caller's
        // table: `pop` is handed `apply`'s first two entries and `drop` its
        // first. The gate has to sit *ahead* of the value split, because the
        // `=<value>` spelling is exactly the one that would otherwise reach the
        // label arms without ever being looked up — a restriction that only
        // holds for the valueless form is not a restriction, and the attached
        // form would have silently applied the stash git refuses to apply.
        let label = ["label-ours", "label-theirs", "label-base"]
            .into_iter()
            .filter(|name| held(*name))
            .find_map(|name| match a.strip_prefix(&format!("--{name}")) {
                Some("") => Some((name, None)),
                Some(rest) => rest.strip_prefix('=').map(|v| (name, Some(v.to_string()))),
                None => None,
            });
        if let Some((name, attached)) = label {
            let value = match attached {
                Some(v) => v,
                None => match args.get(i) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => return Ok(Err(super::missing_option_value(&format!("--{name}")))),
                },
            };
            match name {
                "label-ours" => labels.ours = value,
                "label-theirs" => labels.theirs = value,
                _ => labels.base = Some(value),
            }
            continue;
        }

        match a {
            "--index" => restore_index = true,
            "--no-index" => restore_index = false,
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            // An `OPT_STRING` negation NULLs the pointer, and `do_apply_stash()`
            // substitutes its own default for a NULL label. The negation exists
            // only where the entry does, so this arm asks the table too.
            "--no-label-ours" | "--no-label-theirs" | "--no-label-base"
                if held(&a["--no-".len()..]) =>
            {
                let d = ConflictLabels::default();
                match a {
                    "--no-label-ours" => labels.ours = d.ours,
                    "--no-label-theirs" => labels.theirs = d.theirs,
                    _ => labels.base = d.base,
                }
            }
            other if other.starts_with('-') && other != "-" => {
                // Neither table has `--patch`; `git stash apply -p` is
                // ``error: unknown switch `p'`` in stock git too.
                return Ok(Err(usage_error(other, usage)));
            }
            _ => specs.push(orig.to_string()),
        }
    }
    Ok(Ok(ApplyOptions {
        specs,
        restore_index,
        quiet,
        labels,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// A table restriction that only holds for the valueless spelling is not a
    /// restriction. `pop_stash()` gets `apply`'s first two entries and
    /// `drop_stash()` its first, so the three `--label-*` options resolve for
    /// `apply` alone — and the resolver has to say so for `--label-ours=x` just
    /// as it does for `--label-ours`, because the attached form is the one that
    /// reached the label arms unchecked and *popped the stash* git refuses to
    /// pop. Both spellings, both shorter tables, plus the entries each table
    /// does hold, so a table shrunk too far fails here too.
    #[test]
    fn conflict_labels_resolve_for_apply_alone_in_both_spellings() {
        // `held` is the gate the parser consults; it reads the table, so the
        // membership half of the invariant is asserted against the table.
        let held = |name: &str, table: &'static [LongOpt]| table.iter().any(|o| o.name == name);
        // An abbreviation is the half the resolver decides: a name no entry
        // claims comes back untouched, whatever it carries after the `=`.
        let resolves = |tok: &str, table: &'static [LongOpt]| {
            !matches!(super::super::canonical_long(tok, table),
                      super::super::Long::Name(n) if n == tok)
        };
        for name in ["label-ours", "label-theirs", "label-base"] {
            assert!(held(name, APPLY_OPTS), "{name} is one of apply's five");
            for (sub, table) in [("pop", POP_OPTS), ("drop", DROP_OPTS)] {
                assert!(!held(name, table), "{name} must not be in {sub}'s table");
            }
        }
        for tok in ["--label-o", "--label-o=x", "--no-label-o", "--lab=x"] {
            assert!(resolves(tok, APPLY_OPTS), "{tok} abbreviates for apply");
            for (sub, table) in [("pop", POP_OPTS), ("drop", DROP_OPTS)] {
                assert!(!resolves(tok, table), "{tok} must stay unknown for {sub}");
            }
        }
        // What the shorter tables *do* hold, so this fails just as loudly if one
        // is truncated past the entries git actually gives that subcommand.
        for table in [APPLY_OPTS, POP_OPTS, DROP_OPTS] {
            assert!(held("quiet", table), "every one of the three has OPT__QUIET");
        }
        assert!(held("index", POP_OPTS), "index is pop's second entry");
        assert!(!held("index", DROP_OPTS), "drop is OPT__QUIET alone");
    }

    #[test]
    fn store_options_parse_message_quiet_and_commit() {
        // `-m <msg>` form.
        let (msg, quiet, commit) = parse_store_options(&v(&["-m", "hello", "deadbeef"])).unwrap();
        assert_eq!(msg.as_deref(), Some("hello"));
        assert!(!quiet);
        assert_eq!(commit, "deadbeef");

        // `--message=` form plus `-q`.
        let (msg, quiet, commit) = parse_store_options(&v(&["--message=hi", "-q", "cafe"])).unwrap();
        assert_eq!(msg.as_deref(), Some("hi"));
        assert!(quiet);
        assert_eq!(commit, "cafe");

        // Bare commit, no options.
        let (msg, quiet, commit) = parse_store_options(&v(&["abc123"])).unwrap();
        assert_eq!(msg, None);
        assert!(!quiet);
        assert_eq!(commit, "abc123");
    }

    #[test]
    fn store_options_require_exactly_one_commit() {
        // git: `"git stash store" requires one <commit> argument` on 0 or >1.
        let none = parse_store_options(&v(&[])).unwrap_err().to_string();
        assert_eq!(none, "\"git stash store\" requires one <commit> argument");
        let many = parse_store_options(&v(&["a", "b"])).unwrap_err().to_string();
        assert_eq!(many, "\"git stash store\" requires one <commit> argument");
    }

    /// `git stash drop`'s argv used to be filtered through `positionals()`,
    /// which threw away every dash-prefixed word — so `git stash drop --zzbogus`
    /// silently fell through to the default `stash@{0}` and deleted it. An
    /// unrecognized option has to be refused before anything is dropped.
    #[test]
    fn drop_refuses_an_unknown_option_instead_of_ignoring_it() {
        assert!(parse_drop_options(&v(&["--zzbogus"])).is_err());
        assert!(parse_drop_options(&v(&["-Z"])).is_err());
        assert!(parse_drop_options(&v(&["stash@{1}", "--zzbogus"])).is_err());
        // A value on a flag is refused too.
        assert!(parse_drop_options(&v(&["--quiet=1"])).is_err());
    }

    /// The single `OPT__QUIET` entry, in every spelling stock resolves. With one
    /// option in the table an abbreviation can never be ambiguous, so `--q`,
    /// `--quie`, `--n` and `--no` all resolve — rejecting them would be a
    /// fabricated refusal of something git accepts.
    #[test]
    fn drop_resolves_quiet_in_every_spelling() {
        for spelling in ["-q", "--quiet", "--q", "--quie"] {
            let (quiet, named) = parse_drop_options(&v(&[spelling])).expect(spelling);
            assert!(quiet, "{spelling} must set quiet");
            assert!(named.is_empty());
        }
        for spelling in ["--no-quiet", "--no-q", "--n", "--no"] {
            let (quiet, _) = parse_drop_options(&v(&[spelling])).expect(spelling);
            assert!(!quiet, "{spelling} must clear quiet");
        }
    }

    /// `drop_stash()` parses with no `PARSE_OPT_STOP_AT_NON_OPTION`, so an
    /// option may follow the operand, and `--` ends option parsing.
    #[test]
    fn drop_keeps_operands_and_honours_dash_dash() {
        let (quiet, named) = parse_drop_options(&v(&["stash@{1}", "-q"])).unwrap();
        assert!(quiet);
        assert_eq!(named, v(&["stash@{1}"]));

        // After `--`, an option-shaped word is an operand, not a refusal.
        let (_, named) = parse_drop_options(&v(&["--", "--zzbogus"])).unwrap();
        assert_eq!(named, v(&["--zzbogus"]));
    }

    /// `show_stash()` runs `setup_revisions()` and nothing else, so the options
    /// `cmd_diff()` claims for itself are leftovers here — and a leftover is what
    /// sends git to `usage_with_options(git_stash_show_usage, options)`.
    ///
    /// Forwarding the token to this port's `diff` instead printed *`git diff`'s*
    /// block, which is the wrong command's synopsis for a `git stash show`
    /// command line. The seven names below are the entire difference between the
    /// two surfaces, measured by putting every name `diff` resolves through stock
    /// git 2.55.0 as `git stash show <name>`; the accepted list is a sample of
    /// the 164 that answered normally, including the negations that make
    /// `--no-stat` (rejected, no table entry) and `--no-renames` (accepted) two
    /// different answers.
    #[test]
    fn show_accepts_what_setup_revisions_claims_and_nothing_cmd_diff_owns() {
        for flag in ["--base", "--cached", "--no-index", "--ours", "--staged", "--theirs", "-q"] {
            assert!(!show_accepts(flag), "stash show must not claim {flag}");
        }
        for flag in [
            "--stat",
            "--numstat",
            "--exit-code",
            "--no-color",
            "--no-renames",
            "--no-prefix",
            "-U3",
            "--find-object=deadbeef",
        ] {
            assert!(show_accepts(flag), "stash show must claim {flag}");
        }
        // Not a diff option at all, so it is a leftover for the same reason.
        for flag in ["--zzbogus", "--zzbogus=x", "--no-stat"] {
            assert!(!show_accepts(flag), "stash show must not claim {flag}");
        }
    }
}
