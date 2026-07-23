//! `git merge-octopus` — resolve two or more trees (the octopus merge strategy).
//!
//! Stock `git-merge-octopus` is a POSIX shell driver (`git-merge-octopus.sh`)
//! that sources `git-sh-setup` and then orchestrates four plumbing commands per
//! head being merged: `git merge-base --all`, `git read-tree -u -m`,
//! `git write-tree`, and `git merge-index -o git-merge-one-file -a`. It folds
//! each remote head into the accumulated result tree (`$MRT`) — fast-forwarding
//! while the reference set (`$MRC`) is still a single commit that is the merge
//! base, otherwise three-way merging — and records every merged head as a parent
//! of the eventual commit `git merge` writes.
//!
//! The index/worktree mutation the script drives through `read-tree -u -m` and
//! `merge-index -o git-merge-one-file -a` runs here through the shared octopus
//! engine [`crate::merge_apply::three_way_merge`] — the same tree-merge, worktree
//! checkout, and stage-1/2/3 index application that backs the porcelain
//! `git merge <a> <b>` octopus in `merge.rs`. A fast-forward is expressed as the
//! degenerate three-way whose base equals ours, which yields the target tree
//! conflict-free; a real head is a three-way against its merge base.
//!
//! ### Known divergences from the shell script
//!
//! * **Conflict rendering.** When a head conflicts, `three_way_merge` emits git's
//!   merge-ort porcelain lines (`Auto-merging <path>`, `CONFLICT (<kind>): Merge
//!   conflict in <path>`) rather than `git-merge-one-file`'s `Auto-merging` +
//!   stderr `ERROR: content conflict in <path>`. This matches the porcelain
//!   octopus already shipped, and only differs on the octopus-failure path (a
//!   clean octopus — the common case — merges non-overlapping heads and prints
//!   no conflict lines at all).
//! * **Multiple merge bases.** `three_way_merge` merges against a single base
//!   (`common[0]`), as the porcelain octopus driver does, rather than passing all
//!   `merge-base --all` results to `read-tree`. Criss-cross histories therefore
//!   use the first best base instead of a recursive virtual base.
//! * **`Simple merge did not work`** is triggered by intersecting each side's
//!   changed-path set against the base (`side_changes`), reproducing when
//!   `read-tree --aggressive` would have left a path unmerged. Identical edits on
//!   both sides are excluded (they compare equal), matching the script.
//!
//! ### Covered (verified against git 2.55.0: stdout, stderr, exit code)
//!
//! * `-h` as the first argument — `git-sh-setup`'s `$LONG_USAGE` path with an
//!   empty `USAGE`, i.e. the single line `usage: git merge-octopus ` (note the
//!   trailing space) on **stdout**, exit 0, and no repository required.
//! * `git_dir_init` running before any argument is looked at: outside a
//!   repository, `fatal: not a git repository (or any of the parent
//!   directories): .git` on stderr, exit 128.
//! * The argument split: everything before the first `--` is a merge base and
//!   is discarded, the first argument after it is `$head`, the rest are the
//!   heads to merge.
//! * The "this is not an octopus" guard — fewer than two heads to merge exits 2
//!   silently, so `git merge` can fall back to another strategy.
//! * The `git diff-index --quiet --cached HEAD --` pre-flight: on any
//!   tree↔index difference, `Error: Your local changes to the following files
//!   would be overwritten by merge` followed by the changed paths each indented
//!   by four spaces — both on **stdout**, as `gettextln` and the script's `sed`
//!   pipeline emit them — then exit 2. Paths are quoted per `core.quotePath`.
//! * The merge-base pass over every head: `$GITHEAD_<sha1>` (then the
//!   uppercased `$GITHEAD_<SHA1>`) as the pretty name, `Already up to date with
//!   <name>` on stdout for a head already reachable, and
//!   `Unable to find common commit with <name>` on stderr with exit 1 (the
//!   script's `die`, which prints no `fatal:` prefix) when `merge-base --all`
//!   fails or finds nothing.
//! * The all-heads-already-up-to-date run completes exactly as git does: those
//!   lines on stdout, exit 0, and the repository untouched.
//! * The fast-forward branch (`Fast-forwarding to: <name>`), advancing both the
//!   index/worktree and the `$MRC`/`$MRT` bookkeeping to the head being merged.
//! * The three-way branch: `Trying simple merge with <name>`, the conditional
//!   `Simple merge did not work, trying automatic merge.`, the merge itself, and
//!   the `Automated merge did not work.` / `Should not be doing an octopus.`
//!   refusal (exit 2) when a non-final head leaves an unresolved conflict.
//! * The final exit status is `$OCTOPUS_FAILURE`: 0 for a fully clean run, 1 when
//!   the last head merged with an unresolved conflict left in the worktree/index.
//!
//! ### Not covered
//!
//! An unborn `HEAD` bails: stock git runs `diff-index` against it twice and lets
//! the resulting `fatal: ambiguous argument 'HEAD'` through, which is not
//! reproduced. So does an unmerged index, whose `U` records the ported
//! `diff-index` does not emit either. Both are rejected by `dirty_paths` before
//! any merging begins.

use anyhow::{bail, Result};
use std::collections::{BTreeSet, HashMap};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::Repository;

/// `git-sh-setup`'s `$LONG_USAGE` for a script that sets neither `USAGE` nor
/// `OPTIONS_SPEC`: `usage: $dashless $USAGE` with `$USAGE` empty, so the line
/// ends in a space. `echo` supplies the newline.
const LONG_USAGE: &str = "usage: git merge-octopus \n";

/// The script's argument loop: merge bases, then `--`, then `$head`, then the
/// heads to merge. Bases are collected but unused, exactly as in the script.
struct Args {
    head: Option<String>,
    remotes: Vec<String>,
}

/// Reproduce the `case ",$sep_seen,$head,$arg," in` dispatch verbatim: `--`
/// flips the separator (every time it appears), the first argument after it
/// becomes `$head`, later ones accumulate into `$remotes`, and anything before
/// it is a merge base.
fn parse(args: &[String]) -> Args {
    let mut sep_seen = false;
    let mut head: Option<String> = None;
    let mut remotes = Vec::new();

    for arg in args {
        if arg == "--" {
            sep_seen = true;
        } else if !sep_seen {
            // A merge base; the script keeps these in `$bases` and never reads it.
        } else if head.is_none() {
            head = Some(arg.clone());
        } else {
            remotes.push(arg.clone());
        }
    }

    Args { head, remotes }
}

/// `git merge-octopus` — see the module docs for what is and is not covered.
pub fn merge_octopus(args: &[String]) -> Result<ExitCode> {
    // `git-sh-setup` inspects only `$1`, and does so before `git_dir_init`.
    if args.first().map(String::as_str) == Some("-h") {
        print!("{LONG_USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    // `git_dir_init`, which every non-`-h` invocation reaches first.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    let parsed = parse(args);

    // `case "$remotes" in ?*' '?*)` — anything but two or more heads to merge
    // is not an octopus, and exits 2 without a word so `git merge` can pick
    // another strategy.
    if parsed.remotes.len() < 2 {
        return Ok(ExitCode::from(2));
    }

    // `if ! git diff-index --quiet --cached HEAD --`
    let dirty = dirty_paths(&repo)?;
    if !dirty.is_empty() {
        println!("Error: Your local changes to the following files would be overwritten by merge");
        for path in &dirty {
            println!("    {}", quote_path(path));
        }
        return Ok(ExitCode::from(2));
    }

    // `MRC=$(git rev-parse --verify -q $head)` — git leaves `$MRC` empty when
    // `$head` does not resolve and lets the first `merge-base` fail; we peel it
    // to a commit (`git merge` always spells `$head` as a commit id) to seed both
    // the merge-base peer set and the running result tree.
    let head_spec = parsed.head.as_deref().unwrap_or("");
    let head_commit = commit_reference(&repo, head_spec);

    // `MRC` — git's "merge reference commit" set: initially just `$head`, later
    // *replaced* by a fast-forwarded head or *extended* by each merged head. It
    // is both the merge-base peer set and (as trees) the accumulated result.
    let mut mrc: Vec<ObjectId> = head_commit.map(|c| vec![c]).unwrap_or_default();
    // `MRT=$(git write-tree)` — the "merge result tree". The `diff-index --cached
    // HEAD` pre-flight above forced the index to equal `$head`'s tree, so this is
    // exactly that tree. Unused when `$head` is unresolvable (the first head dies).
    let mut mrt: ObjectId = match head_commit {
        Some(c) => repo.find_object(c)?.peel_to_tree()?.id,
        None => repo.empty_tree().id,
    };
    // `NON_FF_MERGE` is exactly `mrc.len() > 1` (only a three-way merge extends
    // the set), so it needs no separate flag; `OCTOPUS_FAILURE` does.
    let mut octopus_failure = false;
    let mut cur_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);

    for sha1 in &parsed.remotes {
        // `case "$OCTOPUS_FAILURE" in 1)` — a prior head left an unresolved
        // conflict and there is still a head to merge, which an octopus refuses.
        if octopus_failure {
            println!("Automated merge did not work.");
            println!("Should not be doing an octopus.");
            return Ok(ExitCode::from(2));
        }

        let pretty = pretty_name(sha1);

        // `common=$(git merge-base --all $SHA1 $MRC) || die ...`
        let sha1_commit = commit_reference(&repo, sha1);
        let common = match sha1_commit {
            Some(c) => merge_base_all(&repo, c, &mrc)?,
            None => Vec::new(),
        };
        if common.is_empty() {
            eprintln!("Unable to find common commit with {pretty}");
            return Ok(ExitCode::from(1));
        }

        // `case "$LF$common$LF" in *"$LF$SHA1$LF"*)` — a literal line-wise
        // comparison against the argument as spelled, so only a full object id
        // can match. `git merge` always passes full ids.
        if common.iter().any(|id| id.to_string() == *sha1) {
            println!("Already up to date with {pretty}");
            continue;
        }
        // `common` is non-empty, so `$SHA1` resolved to a commit.
        let sha1_commit = sha1_commit.expect("a non-empty merge base implies a resolved head");
        let head_tree = repo.find_object(sha1_commit)?.peel_to_tree()?.id;

        // `if test "$common,$NON_FF_MERGE" = "$MRC,0"` — while `$MRC` is still a
        // single commit that IS the sole merge base, git fast-forwards to this
        // head instead of three-way merging. `mrc.len() == 1` is `NON_FF_MERGE == 0`.
        if mrc.len() == 1 && common.len() == 1 && common[0] == mrc[0] {
            // `eval_gettextln "Fast-forwarding to: $pretty_name"`
            println!("Fast-forwarding to: {pretty}");
            // `git read-tree -u -m $head $SHA1`: a fast-forward is a degenerate
            // three-way whose base equals ours (`mrt == tree($MRC)` here), so the
            // shared engine yields exactly `$SHA1`'s tree, conflict-free, and
            // updates the worktree — the two-tree merge's observable result.
            let labels = gix::merge::blob::builtin_driver::text::Labels {
                ancestor: Some(BStr::new(b"merged common ancestors")),
                current: Some(BStr::new(b"HEAD")),
                other: Some(BStr::new(pretty.as_bytes())),
            };
            let applied = crate::merge_apply::three_way_merge(
                &repo,
                mrt,
                mrt,
                head_tree,
                &cur_index,
                labels,
                &should_interrupt,
            )?;
            cur_index = applied.index;
            cur_index.write(Default::default())?;
            // `MRC=$SHA1 MRT=$(git write-tree)`
            mrc = vec![sha1_commit];
            mrt = applied.tree_id;
            continue;
        }

        // `NON_FF_MERGE=1`; `eval_gettextln "Trying simple merge with $pretty_name"`
        println!("Trying simple merge with {pretty}");

        // The script's `read-tree -u -m --aggressive $common $MRT $SHA1` resolves
        // trivially, and only when `write-tree` then fails — i.e. some path
        // changed on both sides to a different result — does it print "Simple
        // merge did not work" and fall to `merge-index`. The shared engine folds
        // both phases into one pass, so that trigger is recovered by intersecting
        // each side's changed-path set against the merge base.
        let base_tree = repo.find_object(common[0])?.peel_to_tree()?;
        let ours_changes = side_changes(&repo, base_tree.id, mrt)?;
        let theirs_changes = side_changes(&repo, base_tree.id, head_tree)?;
        let needs_auto_merge = ours_changes.iter().any(|(path, ours_state)| {
            theirs_changes
                .get(path)
                .is_some_and(|theirs_state| theirs_state != ours_state)
        });
        if needs_auto_merge {
            println!("Simple merge did not work, trying automatic merge.");
        }

        // `read-tree -u -m --aggressive $common $MRT $SHA1` followed, on unmerged
        // entries, by `merge-index -o git-merge-one-file -a`: both via the shared
        // octopus engine, which also emits git's `Auto-merging`/`CONFLICT` lines.
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(b"merged common ancestors")),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(pretty.as_bytes())),
        };
        let applied = crate::merge_apply::three_way_merge(
            &repo,
            base_tree.id,
            mrt,
            head_tree,
            &cur_index,
            labels,
            &should_interrupt,
        )?;
        cur_index = applied.index;
        cur_index.write(Default::default())?;
        if !applied.conflicts.is_empty() {
            // `git-merge-one-file` left conflict markers → `OCTOPUS_FAILURE=1`.
            // The last head may fail (loop ends, exit 1); an earlier one makes the
            // next iteration print the octopus failure and exit 2.
            octopus_failure = true;
        }

        // `MRC="$MRC $SHA1"; MRT=$next`
        mrc.push(sha1_commit);
        mrt = applied.tree_id;
    }

    // `exit "$OCTOPUS_FAILURE"`
    if octopus_failure {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// `eval pretty_name=\${GITHEAD_$SHA1:-$SHA1}`, then the uppercased retry.
/// `${x:-y}` treats an empty value as unset, hence the `filter`.
fn pretty_name(sha1: &str) -> String {
    let lookup = |key: String| std::env::var(key).ok().filter(|v| !v.is_empty());
    if let Some(name) = lookup(format!("GITHEAD_{sha1}")) {
        return name;
    }
    let upper: String = sha1
        .chars()
        .map(|c| if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c })
        .collect();
    lookup(format!("GITHEAD_{upper}")).unwrap_or_else(|| sha1.to_string())
}

/// `git merge-base --all $SHA1 $MRC`: every best common ancestor of the head
/// commit `sha1` against the accumulated `mrc` commit set. Empty when `mrc` is
/// empty (an unresolvable `$head`) or the histories share no ancestor, which is
/// the script's `die` path either way.
fn merge_base_all(repo: &Repository, sha1: ObjectId, mrc: &[ObjectId]) -> Result<Vec<ObjectId>> {
    if mrc.is_empty() {
        return Ok(Vec::new());
    }
    Ok(repo
        .merge_bases_many(sha1, mrc)?
        .into_iter()
        .map(|id| id.detach())
        .collect())
}

/// The per-path resulting state of `side`'s tree relative to `base`: `Some(id)`
/// for a path added or modified to that blob, `None` for one deleted. This is the
/// input to the "changed on both sides" test that decides whether the script's
/// trivial `read-tree --aggressive` would have left a path unmerged (rename
/// tracking is off, matching `--aggressive`, so `Rewrite` never appears).
fn side_changes(
    repo: &Repository,
    base: ObjectId,
    side: ObjectId,
) -> Result<HashMap<BString, Option<ObjectId>>> {
    use gix::object::tree::diff::ChangeDetached;

    let base_tree = repo.find_object(base)?.peel_to_tree()?;
    let side_tree = repo.find_object(side)?.peel_to_tree()?;
    let changes =
        repo.diff_tree_to_tree(Some(&base_tree), Some(&side_tree), gix::diff::Options::default())?;

    let mut map = HashMap::new();
    for change in &changes {
        match change {
            ChangeDetached::Addition { location, id, .. }
            | ChangeDetached::Modification { location, id, .. } => {
                map.insert(location.clone(), Some(*id));
            }
            ChangeDetached::Deletion { location, .. } => {
                map.insert(location.clone(), None);
            }
            // Rename tracking is disabled by default, so this never fires.
            ChangeDetached::Rewrite { .. } => {}
        }
    }
    Ok(map)
}

/// Resolve `spec` and peel it to the commit it names, or `None`.
fn commit_reference(repo: &Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    object.peel_to_commit().ok().map(|c| c.id)
}

/// The paths `git diff-index --cached --name-only HEAD --` would print, sorted
/// bytewise as the index — and therefore git's diff queue — orders them.
fn dirty_paths(repo: &Repository) -> Result<Vec<BString>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let head_tree = match repo.head_commit().ok().and_then(|c| c.tree_id().ok()) {
        Some(id) => id.detach(),
        None => bail!(
            "unsupported: merge-octopus against an unborn HEAD (git lets diff-index's \
             `fatal: ambiguous argument 'HEAD'` through, which is not reproduced)"
        ),
    };

    let index = repo.index_or_empty()?;
    let index_state: &gix::index::State = &index;
    if index_state.entries().iter().any(|e| e.stage_raw() != 0) {
        bail!("unsupported: unmerged (conflicted) index entries — diff-index's U records are not ported");
    }

    let mut paths: BTreeSet<BString> = BTreeSet::new();
    repo.tree_index_status(
        &head_tree,
        index_state,
        None,
        TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            match change {
                ChangeRef::Addition { location, .. } => {
                    paths.insert(location.into_owned());
                }
                ChangeRef::Deletion { location, .. } => {
                    paths.insert(location.into_owned());
                }
                ChangeRef::Modification { location, .. } => {
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

    /// The `case ",$sep_seen,$head,$arg," in` dispatch: bases are dropped, the
    /// first argument after `--` is the head, the rest are merged.
    #[test]
    fn splits_bases_head_and_remotes() {
        let a = parse(&v(&["base1", "base2", "--", "head", "r1", "r2"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1", "r2"]));

        // No separator at all: everything is a merge base, so there is nothing
        // to merge and the caller exits 2.
        let a = parse(&v(&["head", "r1", "r2"]));
        assert_eq!(a.head, None);
        assert!(a.remotes.is_empty());

        // A second `--` re-sets `sep_seen`, which is already `yes`, so it is
        // consumed rather than becoming a head — as in the script.
        let a = parse(&v(&["--", "head", "--", "r1"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1"]));
    }

    /// `${GITHEAD_$SHA1:-$SHA1}` falls back to the id itself when no
    /// `GITHEAD_<id>` is exported, which is the case for this synthetic id.
    #[test]
    fn pretty_name_falls_back_to_the_id() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(pretty_name(id), id);
    }
}
