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
//!   index/worktree and the `$MRC`/`$MRT` bookkeeping to the head being merged —
//!   including its `read-tree -u -m $head $SHA1` refusals (`Entry '<p>' would be
//!   overwritten by merge.`, `Entry '<p>' not uptodate.`, `Untracked working
//!   tree file '<p>' would be overwritten by merge.`, exit 128), whose old tree
//!   is the original `$head` argument rather than the running `$MRT`, and the
//!   textual `test "$common,$NON_FF_MERGE" = "$MRC,0"` that decides the branch:
//!   `$MRC` holds each fast-forwarded head **as spelled**, so a branch name can
//!   never equal `merge-base --all`'s object ids and the second consecutive
//!   fast-forward only happens when the caller passes full ids (as `git merge`
//!   does).
//! * The three-way branch's `read-tree -u -m --aggressive $common $MRT $SHA1 ||
//!   exit 2` refusals, with the same plumbing wording and exit 2.
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
use gix::prelude::ObjectIdExt;
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
    // `$MRC` is a *shell string*, and the fast-forward test below compares it
    // textually. It starts as `git rev-parse --verify -q $head`, i.e. a full
    // object id whatever `$head` was spelled as, but a fast-forward replaces it
    // with `$SHA1` — the head **as spelled on the command line**. Keeping only
    // the resolved ids made `merge-octopus -- <head> branch1 branch2` fast-forward
    // twice where stock's `$common` (always full ids) can never equal a branch
    // name, so stock three-way merges the second head instead.
    let mut mrc_text: Vec<String> = head_commit.map(|c| vec![c.to_string()]).unwrap_or_default();
    // `MRT=$(git write-tree)` — the "merge result tree". The `diff-index --cached
    // HEAD` pre-flight above forced the index to equal `HEAD`'s tree, which is
    // `$head`'s whenever `git merge` is the caller. Unused when `$head` is
    // unresolvable (the first head dies).
    let head_arg_tree: ObjectId = match head_commit {
        Some(c) => repo.find_object(c)?.peel_to_tree()?.id,
        None => repo.empty_tree().id,
    };
    // `$MRT` starts there and then tracks each folded-in head, while `$head`
    // stays put — the fast-forward's `read-tree` reads the latter, so both are
    // needed.
    let mut mrt: ObjectId = head_arg_tree;
    // `NON_FF_MERGE` is exactly `mrc.len() > 1` (only a three-way merge extends
    // the set), so it needs no separate flag; `OCTOPUS_FAILURE` does.
    let mut octopus_failure = false;
    let mut cur_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);
    // `pretty_name` is a plain shell variable that outlives one iteration of the
    // loop below, and a head whose spelling is not a shell name leaves it at the
    // previous iteration's value — see [`pretty_name`]. It starts out unset.
    let mut pretty = String::new();

    for sha1 in &parsed.remotes {
        // `case "$OCTOPUS_FAILURE" in 1)` — a prior head left an unresolved
        // conflict and there is still a head to merge, which an octopus refuses.
        if octopus_failure {
            println!("Automated merge did not work.");
            println!("Should not be doing an octopus.");
            return Ok(ExitCode::from(2));
        }

        pretty = pretty_name(sha1, &pretty);

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
        // `$common` is `merge-base --all`'s newline-separated output and `$MRC`
        // is the space-separated commit list, compared as whole strings.
        let common_text = common.iter().map(ObjectId::to_string).collect::<Vec<_>>().join("\n");
        if mrc.len() == 1 && common_text == mrc_text.join(" ") {
            // `eval_gettextln "Fast-forwarding to: $pretty_name"`
            println!("Fast-forwarding to: {pretty}");
            // `git read-tree -u -m $head $SHA1 || exit` (git-merge-octopus.sh:90):
            // a two-tree merge, which **refuses** rather than overwrite when the
            // index or the worktree has drifted off the old tree, and whose
            // `die()` takes the script down with it (`|| exit`, i.e. read-tree's
            // own 128).
            //
            // The old tree is `$head` — the original argument — **not** the
            // running `$MRT`. The two coincide only until the first head is
            // folded in, so a *second* consecutive fast-forward hands read-tree
            // an index that no longer matches `$head` and it dies. Passing `mrt`
            // instead made every such octopus succeed at exit 0 with a tree stock
            // refuses to write, and let an untracked file in the way of the very
            // first head be silently overwritten.
            let clobber =
                crate::merge_guard::verify_two_way(&repo, head_arg_tree, head_tree, &cur_index)?;
            if !clobber.is_empty() {
                clobber.report_plumbing();
                return Ok(ExitCode::from(128));
            }
            // Past the guard the two-tree merge writes `$SHA1`'s tree wholesale.
            // Expressed as the degenerate three-way whose base equals ours, the
            // shared engine yields exactly that, conflict-free, and updates the
            // worktree — the two-tree merge's observable result. A two-tree
            // read-tree never conflicts, so this label is never rendered; it is
            // merge-ort's single-base name for the sole base.
            let ancestor = common[0].attach(&repo).shorten_or_id().to_string();
            let labels = gix::merge::blob::builtin_driver::text::Labels {
                ancestor: Some(BStr::new(ancestor.as_bytes())),
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
            crate::index_racy::write(&repo, &mut cur_index)?;
            // `MRC=$SHA1 MRT=$(git write-tree)`
            mrc = vec![sha1_commit];
            mrc_text = vec![sha1.clone()];
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
        // `git read-tree -u -m --aggressive $common $MRT $SHA1 || exit 2`: the
        // three-tree merge refuses the same way its two-tree sibling above does,
        // but the script spells this one's failure `exit 2` rather than letting
        // read-tree's status through. Refusing here rather than over the merged
        // tree is also what keeps a failed octopus from leaving that tree in the
        // object database.
        let clobber =
            crate::merge_guard::verify_three_way(&repo, base_tree.id, mrt, head_tree, &cur_index)?;
        if !clobber.is_empty() {
            clobber.report_plumbing();
            return Ok(ExitCode::from(2));
        }
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
        // `merge_ort_internal()`'s ancestor name: `merged common ancestors` only
        // when several bases were folded together, otherwise the sole base's
        // abbreviated id. The shell script shows neither — `git merge-one-file`
        // runs `git merge-file` with no `-L`, so its `diff3` markers carry the
        // run's `.merge_file_XXXXXX` temporary names — and this driver renders
        // merge-ort conflicts, per the divergence noted at the top of the file.
        let ancestor = if common.len() > 1 {
            "merged common ancestors".to_string()
        } else {
            common[0].attach(&repo).shorten_or_id().to_string()
        };
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(ancestor.as_bytes())),
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
        crate::index_racy::write(&repo, &mut cur_index)?;
        if !applied.conflicts.is_empty() {
            // `git-merge-one-file` left conflict markers → `OCTOPUS_FAILURE=1`.
            // The last head may fail (loop ends, exit 1); an earlier one makes the
            // next iteration print the octopus failure and exit 2.
            octopus_failure = true;
        }

        // `MRC="$MRC $SHA1"; MRT=$next`
        mrc.push(sha1_commit);
        mrc_text.push(sha1.clone());
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
///
/// `previous` is the value `pretty_name` still holds from the previous loop
/// iteration. It matters because `$SHA1` is interpolated into a *parameter
/// name*: when the head is spelled as something that is not a shell name — a
/// tag such as `v0.1.0`, say, which `git merge` passes through verbatim — the
/// expansion is a "bad substitution", the whole `eval` fails without assigning,
/// and the loop goes on to print the *stale* `pretty_name`. Both `eval`s share
/// that fate, since uppercasing cannot rescue an invalid name.
fn pretty_name(sha1: &str, previous: &str) -> String {
    // `GITHEAD_$SHA1` is a valid parameter name only while `$SHA1` keeps the
    // expansion inside the shell's portable-name character set; the `GITHEAD_`
    // prefix already satisfies the leading-non-digit rule.
    if !sha1.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return previous.to_string();
    }
    let lookup = |key: String| std::env::var(key).ok().filter(|v| !v.is_empty());
    if let Some(name) = lookup(format!("GITHEAD_{sha1}")) {
        return name;
    }
    // `test "$SHA1" = "$pretty_name"` holds: the `:-` fallback just assigned
    // `$SHA1`. The retry's own `:-` default is that same value.
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
        None => anyhow::bail!(
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
        assert_eq!(pretty_name(id, "stale"), id);
    }

    /// A head spelled as anything outside the shell's parameter-name character
    /// set makes `${GITHEAD_$SHA1:-$SHA1}` a "bad substitution": the `eval`
    /// aborts before assigning, so the script goes on to print whatever the
    /// previous iteration left in `pretty_name` rather than the head itself.
    /// `git merge octopus main feature v0.1.0` is exactly this — the tag's dots
    /// make stock git announce "Trying simple merge with feature".
    #[test]
    fn pretty_name_keeps_the_previous_value_for_a_non_shell_name() {
        assert_eq!(pretty_name("v0.1.0", "feature"), "feature");
        assert_eq!(pretty_name("refs/tags/v1", "feature"), "feature");
        // Unset at the top of the loop, so the very first head yields nothing.
        assert_eq!(pretty_name("v0.1.0", ""), "");
        // An underscore is a name character, so this one substitutes normally.
        assert_eq!(pretty_name("my_head", "feature"), "my_head");
    }
}
