//! `git filter-branch` — rewrite branches. **The rewrite itself is not ported:
//! every path that would touch a commit bails.**
//!
//! Stock `git-filter-branch` is a 665-line POSIX shell script
//! (`$(git --exec-path)/git-filter-branch`, `#!/bin/sh` on line 1). It is not a
//! builtin and has no C equivalent. Its entire purpose is to `eval` user-supplied
//! shell fragments once per commit — `--tree-filter` runs in a checked-out
//! worktree, `--index-filter` runs against a scratch `GIT_INDEX_FILE`,
//! `--commit-filter` defaults to the literal string `git commit-tree "$@"` and is
//! prepended with a block of shell helpers (`map`, `skip_commit`,
//! `git_commit_non_empty_tree`) that the user's fragment is expected to call.
//! Reproducing it means shipping a `/bin/sh` evaluator plus the `git commit-tree`,
//! `git checkout-index`, `git update-index`, `git read-tree` and `git update-ref`
//! plumbing those fragments invoke by name from a subprocess — that is a shell
//! interpreter, not a gitoxide port, and nothing under `src/ported/` provides it.
//! Approximating it is the worst outcome available: the command rewrites history
//! in place and moves `refs/heads/*`, so a plausible-looking reimplementation
//! corrupts repositories rather than failing loudly.
//!
//! What *is* ported is the surface that is byte-verifiable before the first
//! commit is read. All of it was checked against git 2.55.0 on Darwin:
//!
//!   * The startup warning block and `Proceeding with filter-branch...`, on
//!     **stdout**, including the ten-second `sleep` and the
//!     `FILTER_BRANCH_SQUELCH_WARNING` / `GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS`
//!     squelch (script lines 91-104).
//!   * `-h` **as the first argument only** — handled by `git-sh-setup` (line 88
//!     of that file) because `OPTIONS_SPEC` is empty, so it prints
//!     `usage: git filter-branch <USAGE>` on **stdout** and exits 0, before the
//!     clean-worktree check. `-h` anywhere else is an ordinary option and reaches
//!     the parser, which rejects it.
//!   * `require_clean_work_tree 'rewrite branches'` (script line 108, skipped for
//!     a bare repository), byte for byte: the unborn-`HEAD` `fatal: Needed a
//!     single revision` from its `git rev-parse --verify HEAD`, and the
//!     `Cannot rewrite branches: You have unstaged changes.` /
//!     `Cannot rewrite branches: Your index contains uncommitted changes.` /
//!     `Additionally, your index contains uncommitted changes.` triple. All on
//!     stderr, exit 1. filter-branch passes no `$2`, so there is no
//!     `Please commit or stash them.` line.
//!   * The whole hand-rolled option loop (lines 126-207). It is a `case` chain,
//!     not `parse_options`: no `=value` form, no abbreviation, no clustering,
//!     every non-boolean switch takes its value as a separate argument, and a
//!     switch appearing as the last argument (`case "$#" in 1) usage`) is a usage
//!     error. Unknown options and the missing-value case both print
//!     `usage: git filter-branch <USAGE>` on **stderr**, exit 1.
//!   * `Cannot set --prune-empty and --commit-filter at the same time` (line 215)
//!     and `<tempdir> already exists, please remove it` (line 224), stderr,
//!     exit 1. The second reports the `-d` value as written, not resolved.
//!
//! NOT reproduced — these `bail!` rather than pretending to have rewritten:
//!
//!   1. **The rewrite.** See above; there is no shell evaluator to run the
//!      filters, and `--commit-filter` has no meaning without one.
//!   2. **`-f`'s `rm -rf "$tempdir"`** (line 222). Stock deletes the scratch
//!      directory before it starts; this port refuses the run instead of
//!      performing a destructive side effect it will not follow through on.
//!   3. Everything past that point: the `refs/original/` backup scan and its
//!      `Cannot create a new backup.` refusal, `git rev-parse --no-flags
//!      --revs-only --symbolic-full-name --default HEAD "$@"` head selection,
//!      `WARNING: not rewriting '<ref>' (not a committish)`,
//!      `You must specify a ref to rewrite.`, the `Rewrite <sha> (n/m)` progress
//!      line, `--state-branch`'s `filter.map` load/store, `--tag-name-filter`
//!      tag rewriting, and the `WARNING: Ref '<ref>' is unchanged` epilogue.
//!      All of them are downstream of a commit having been filtered.
//!
//! Note also that submodule changes are compared here, whereas git's
//! `require_clean_work_tree` passes `--ignore-submodules`; `gix::status` has no
//! equivalent knob, the same gap `rebase.rs` carries.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::bstr::BString;

/// `$USAGE` from `git-filter-branch` lines 106-112, verbatim. The continuation
/// lines are indented with a single tab, as in the script.
const USAGE: &str = "\
[--setup <command>] [--subdirectory-filter <directory>] [--env-filter <command>]
\t[--tree-filter <command>] [--index-filter <command>]
\t[--parent-filter <command>] [--msg-filter <command>]
\t[--commit-filter <command>] [--tag-name-filter <command>]
\t[--original <namespace>]
\t[-d <directory>] [-f | --force] [--state-branch <branch>]
\t[--] [<rev-list options>...]";

/// The startup warning, lines 93-102 of the script. Emitted on stdout unless
/// squelched, followed by a ten-second sleep and `Proceeding with ...`.
const WARNING: &str = "\
WARNING: git-filter-branch has a glut of gotchas generating mangled history
\t rewrites.  Hit Ctrl-C before proceeding to abort, then use an
\t alternative filtering tool such as 'git filter-repo'
\t (https://github.com/newren/git-filter-repo/) instead.  See the
\t filter-branch manual page for more details; to squelch this warning,
\t set FILTER_BRANCH_SQUELCH_WARNING=1.
";

/// Every switch in the inner `case "$ARG"` (lines 168-205). Each takes exactly
/// one argument, supplied as the following `argv` element — the script offers no
/// `--opt=value` form.
const VALUED: &[&str] = &[
    "-d",
    "--setup",
    "--subdirectory-filter",
    "--env-filter",
    "--tree-filter",
    "--index-filter",
    "--parent-filter",
    "--msg-filter",
    "--commit-filter",
    "--tag-name-filter",
    "--original",
    "--state-branch",
];

/// `usage()` as `git-sh-setup` defines it when `OPTIONS_SPEC` is empty
/// (git-sh-setup line 80): `die "usage: $dashless $USAGE"` — stderr, exit 1.
fn usage() -> ExitCode {
    eprintln!("usage: git filter-branch {USAGE}");
    ExitCode::from(1)
}

/// `die "$*"` after `git-sh-setup` overrides the script's own version: a single
/// line on stderr, exit 1, with no leading blank line.
fn die(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

/// `(unstaged, staged)` for `require_clean_work_tree`, matching git's
/// `diff-files` / `diff-index --cached HEAD` pair.
fn dirty_state(repo: &gix::Repository) -> Result<(bool, bool)> {
    let mut unstaged = false;
    let mut staged = false;
    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(_) => staged = true,
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                match iw {
                    // Untracked files and stat-only staleness do not make the
                    // tree dirty, exactly as `diff-files` sees it after the
                    // `git update-index --refresh` the script runs first.
                    Item::Modification { status, .. } => match status {
                        EntryStatus::NeedsUpdate(_) => {}
                        _ => unstaged = true,
                    },
                    Item::Rewrite { .. } => unstaged = true,
                    Item::DirectoryContents { .. } => {}
                }
            }
        }
    }
    Ok((unstaged, staged))
}

/// `git filter-branch` — see the module documentation for the ported surface.
///
/// Reproduces the startup warning, `-h`, the clean-worktree gate, the full
/// option loop and the two pre-flight `die`s. Any invocation that survives all
/// of them bails, naming the missing substrate; nothing is rewritten and no ref
/// is moved.
pub fn filter_branch(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail; tolerate the subcommand at
    // index 0 so both calling conventions behave identically.
    let args = match args.first() {
        Some(a) if a == "filter-branch" => &args[1..],
        _ => args,
    };

    // Script lines 91-104. The guard is `test -z "$A$B"`, i.e. both variables
    // empty or unset. It runs before anything else, `-h` included.
    let squelched = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if !squelched("FILTER_BRANCH_SQUELCH_WARNING")
        && !squelched("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS")
    {
        print!("{WARNING}");
        std::thread::sleep(std::time::Duration::from_secs(10));
        print!("Proceeding with filter-branch...\n\n");
    }

    // `git-sh-setup` line 88: `case "$1" in -h) echo "$LONG_USAGE"; exit`.
    // First argument only, stdout, exit 0, ahead of the worktree check.
    if args.first().is_some_and(|a| a == "-h") {
        println!("usage: git filter-branch {USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    let repo = gix::discover(".")?;

    // Script line 107: `if [ "$(is_bare_repository)" = false ]`.
    if !repo.is_bare() {
        // `git rev-parse --verify HEAD >/dev/null || exit 1` — an unborn or
        // broken HEAD stops here with rev-parse's own message.
        if repo.head_id().is_err() {
            eprintln!("fatal: Needed a single revision");
            return Ok(ExitCode::from(1));
        }
        let (unstaged, staged) = dirty_state(&repo)?;
        if unstaged {
            eprintln!("Cannot rewrite branches: You have unstaged changes.");
            if staged {
                eprintln!("Additionally, your index contains uncommitted changes.");
            }
            return Ok(ExitCode::from(1));
        }
        if staged {
            eprintln!("Cannot rewrite branches: Your index contains uncommitted changes.");
            return Ok(ExitCode::from(1));
        }
    }

    // --- the option loop, script lines 126-207 ---------------------------
    let mut tempdir = ".git-rewrite".to_string();
    let mut force = false;
    let mut prune_empty = false;
    // `--commit-filter` is the only valued switch whose mere presence changes a
    // later control-flow decision (the `--prune-empty` conflict), so it alone is
    // tracked by value; the rest only steer the rewrite and are recorded by name
    // so the bail can say which one was asked for.
    let mut commit_filter = false;
    let mut filters: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();

        // The outer `case "$1"`: `--` ends options, three booleans consume
        // themselves, any other `-*` falls through to the valued handling, and
        // anything else (including an empty string) ends the loop.
        match arg {
            "--" => {
                i += 1;
                break;
            }
            "--force" | "-f" => {
                force = true;
                i += 1;
                continue;
            }
            // Deprecated and inert: `$remap_to_ancestor` is set automatically.
            "--remap-to-ancestor" => {
                i += 1;
                continue;
            }
            "--prune-empty" => {
                prune_empty = true;
                i += 1;
                continue;
            }
            // `-*)` with an empty body: fall through. A bare `-` matches this
            // glob too, so it reaches the valued handling as an unknown option.
            _ if arg.starts_with('-') => {}
            _ => break,
        }

        // `case "$#" in 1) usage ;; esac` — the value must be a separate,
        // present argument. This fires for `--tree-filter` as the last word and
        // for a bare `-`, which reaches here as an unknown option with no value.
        if i + 1 >= args.len() {
            return Ok(usage());
        }
        let value = args[i + 1].as_str();
        i += 2;

        if !VALUED.contains(&arg) {
            // The inner `case "$ARG"`'s `*)` arm. Note this also catches every
            // `--opt=value` spelling, which the script does not understand.
            return Ok(usage());
        }
        match arg {
            "-d" => tempdir = value.to_string(),
            "--commit-filter" => {
                commit_filter = true;
                filters.push(arg.to_string());
            }
            _ => filters.push(arg.to_string()),
        }
    }
    let rev_list_args = &args[i.min(args.len())..];

    // Script lines 209-217: only `t,<non-empty>` is an error.
    if prune_empty && commit_filter {
        return Ok(die(
            "Cannot set --prune-empty and --commit-filter at the same time",
        ));
    }

    // Script lines 219-226. Without `-f` an existing scratch directory is fatal;
    // with `-f` git would `rm -rf` it, which this port deliberately does not do.
    if !force && std::path::Path::new(&tempdir).is_dir() {
        return Ok(die(&format!("{tempdir} already exists, please remove it")));
    }

    let asked_for = if !filters.is_empty() {
        filters.join(", ")
    } else if !rev_list_args.is_empty() {
        format!("rev-list arguments {rev_list_args:?}")
    } else {
        "the default HEAD rewrite".to_string()
    };
    bail!(
        "unsupported: rewriting history is not ported ({asked_for}); ported: the startup \
         warning, -h, require_clean_work_tree, the option loop and the --prune-empty/\
         --commit-filter and existing-tempdir refusals. Stock git-filter-branch is a /bin/sh \
         script that `eval`s the user's filter fragments once per commit and shells out to \
         git commit-tree/checkout-index/update-index/update-ref from them; no shell evaluator \
         and no such subprocess plumbing exists under src/ported, and an approximation would \
         silently mangle rewritten branches instead of failing"
    )
}
