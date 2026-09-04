//! `git merge-resolve` — resolve two trees using the `read-tree` "resolve" merge
//! strategy back-end.
//!
//! Stock `git-merge-resolve` is a 60-line POSIX shell driver
//! (`git-merge-resolve.sh`) that sources `git-sh-setup` and then chains five
//! plumbing commands: `git diff-index`, `git update-index -q --refresh`,
//! `git read-tree -u -m --aggressive $bases $head $remotes`, `git write-tree`,
//! and `git merge-index -o git-merge-one-file -a`.
//!
//! The standard `git merge -s resolve` invocation is `<base> -- <head>
//! <remote>` — a single merge base with one head and one remote — and the script
//! runs that through two plumbing commands this build already ports:
//!
//! ```sh
//! git update-index -q --refresh
//! git read-tree -u -m --aggressive $bases $head $remotes || exit 2
//! echo "Trying simple merge."
//! if result_tree=$(git write-tree 2>/dev/null)
//! then
//! 	exit 0
//! else
//! 	echo "Simple merge failed, trying Automatic merge."
//! 	if git merge-index -o git-merge-one-file -a
//! 	then exit 0
//! 	else exit 1
//! 	fi
//! fi
//! ```
//!
//! So that is what runs here: [`super::read_tree`] and [`super::merge_index`]
//! are called in process, in that order, with the same arguments. Nothing about
//! the merge is re-derived — the index stages, the worktree bytes, the
//! `Auto-merging <path>` / `Added <path> in both, but differently.` lines,
//! `git-merge-one-file`'s refusals (`ERROR: <path>: Not handling case …`), the
//! `ERROR: content conflict in <path>` / `fatal: merge program failed` pair and
//! the `.merge_file_XXXXXX` conflict-marker labels all come from those two
//! ports, which is the only way they can match a chain whose output includes
//! `mkstemp` names.
//!
//! ### Covered (verified against git on Darwin: stdout, stderr, exit code)
//!
//! * `-h`, the outside-a-repository fatal, the `git diff-index --quiet --cached
//!   HEAD --` pre-flight (`Error: Your local changes …`, exit 2, `core.quotePath`
//!   quoting), the argument split, the octopus guard (exit 2), and the baseless
//!   guard (exit 2).
//! * The single-base merge end to end: index stages, worktree contents, the
//!   `Trying simple merge.` / `Simple merge failed …` framing, every per-path
//!   line, and the exit code (0 clean, 1 conflicted, 2 declined).
//!
//! ### Option-shaped operands
//!
//! The script interpolates `$bases`, `$head` and `$remotes` into the `read-tree`
//! command line **unquoted**, so anything in them that looks like an option is
//! one — and read-tree's `parse_options` sees it before it looks at a single
//! tree. That is the whole story behind `-X<opt>`: `try_merge_command` turns it
//! into `--<opt>` and puts it ahead of the merge base, so
//! `git merge -s resolve -Xours` arrives here with `--ours` in `$bases`. Nothing
//! in this file decides what to do about that; read-tree's own scan does, and
//! `|| exit 2` maps its refusal to status 2 — `error: unknown option `ours\''
//! plus read-tree's usage block.
//!
//! Several merge bases are just more trees on the `read-tree` command line, so
//! a criss-cross history goes through the same chain; `--aggressive` files every
//! tree before the head under stage 1 (unpack-trees.c:1211-1226).
//!
//! ### Floors (bail rather than approximate)
//!
//! * An unborn `HEAD` and an already-unmerged index, both of which the
//!   `diff-index` pre-flight would have to diagnose in git's own words.

// `print!`/`println!` here go through git's stdout buffer. `merge` reaches this
// module in-process and arms that buffer (see `crate::cstdio`), so both halves of
// its output have to be buffered or they interleave against each other; run as
// its own command nothing arms it and these are unbuffered writes as before.
use crate::cstdio::{print, println};
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
    let Ok(repo) = crate::setup::discover() else {
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

    // Several merge bases need no special handling: they are simply more trees
    // on the `read-tree` command line, and `--aggressive` collapses each one to
    // stage 1 (unpack-trees.c:1211-1226). `git merge -s resolve` over a
    // criss-cross history is the case that reaches it.
    //
    // `git update-index -q --refresh` — without it a file whose stat data drifted
    // but whose content did not would fail read-tree's `verify_uptodate()`.
    let refresh: Vec<String> = vec!["-q".to_string(), "--refresh".to_string()];
    super::update_index::update_index(&refresh)?;

    // `git read-tree -u -m --aggressive $bases $head $remotes || exit 2`. The
    // operand lists are interpolated unquoted, so they go through verbatim and
    // read-tree's own scan decides what is an option; `|| exit 2` maps every
    // refusal it can produce — unknown option, bad tree-ish, `Merge requires
    // file-level merging`, `Entry '…' not uptodate` — to status 2.
    let mut read_tree_argv: Vec<String> =
        ["-u", "-m", "--aggressive"].iter().map(|s| s.to_string()).collect();
    read_tree_argv.extend(parsed.bases.iter().cloned());
    read_tree_argv.extend(parsed.head.iter().cloned());
    read_tree_argv.extend(parsed.remotes.iter().cloned());
    if status(super::read_tree::read_tree(&read_tree_argv)?) != 0 {
        return Ok(ExitCode::from(2));
    }

    // `echo "Trying simple merge."` — printed once read-tree has agreed.
    println!("Trying simple merge.");

    // `if result_tree=$(git write-tree 2>/dev/null)`. `write-tree` fails on an
    // unmerged index and says so on the stderr the script discards, so the test
    // is exactly "did `read-tree --aggressive` leave a stage behind"; asking the
    // index directly keeps those suppressed lines suppressed.
    let mut index = repo.open_index()?;
    if index.entries().iter().all(|e| e.stage_raw() == 0) {
        // The test is only half of what `write-tree` is here for. The other half
        // is its side effect: `write_index_as_tree()` (cache-tree.c:797-831)
        // fills the cache-tree in and writes the index back, so the index this
        // strategy leaves for its caller already names its root tree. Skipping
        // the call left `git rebase -s resolve` with a fully *invalid* extension
        // where stock has a fully valid one — `<root>=-1/2 …` against stock's
        // `<root>=10/2:c3f03c98…` — because nothing downstream rebuilds it: the
        // sequencer's own `write_index_as_tree()` finds the cache-tree already
        // there and, for the strategy path, there is no merge-ort repair.
        let _ = super::write_tree::refresh_cache_tree(&repo, &mut index, false)?;
        return Ok(ExitCode::SUCCESS);
    }

    println!("Simple merge failed, trying Automatic merge.");
    // `git merge-index -o git-merge-one-file -a`, whose own exit status the
    // script collapses to 0 or 1.
    let merge_index_argv: Vec<String> =
        ["-o", "git-merge-one-file", "-a"].iter().map(|s| s.to_string()).collect();
    Ok(match status(super::merge_index::merge_index(&merge_index_argv)?) {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

/// The numeric status an [`ExitCode`] carries; `ExitCode` exposes no accessor on
/// stable Rust, so probe the 256 values it can hold. The script branches on the
/// status of the programs it runs, so the ports of those programs have to hand
/// one back.
fn status(code: ExitCode) -> u8 {
    (0u8..=255).find(|&n| code == ExitCode::from(n)).unwrap_or(1)
}

/// The paths `git diff-index --cached --name-only HEAD --` would print, sorted
/// bytewise as the index — and therefore git's diff queue — orders them.
fn dirty_paths(repo: &Repository) -> Result<Vec<BString>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let head_tree = match repo.head_commit().ok().and_then(|c| c.tree_id().ok()) {
        Some(id) => id.detach(),
        None => anyhow::bail!(
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

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
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
