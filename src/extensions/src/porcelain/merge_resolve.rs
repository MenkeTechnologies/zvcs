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
//! <remote>` — a single merge base with one head and one remote. That case is
//! served here as a real three-way merge: the merge is computed by the vendored
//! `gix-merge` tree merge with rename detection **disabled** (the resolve
//! strategy, unlike recursive, never detects renames — matching
//! `read-tree --aggressive` + `git-merge-one-file`, neither of which does), the
//! resulting tree is materialised into the worktree, and `.git/index` is written
//! with stage 1/2/3 entries for every path that stayed conflicted. This is the
//! same tree-merge/worktree/index machinery `merge_recursive.rs` drives
//! (`repo.merge_trees`, `crate::worktree::checkout_subset`,
//! `index_changed_after_applying_conflicts`).
//!
//! The script's control-flow output is reproduced on top of that result:
//! `Trying simple merge.` is printed once the merge succeeds; when any path
//! changed on both sides — i.e. `read-tree --aggressive` would have left it
//! unmerged and `write-tree` would have failed — `Simple merge failed, trying
//! Automatic merge.` follows, then `git-merge-one-file`'s per-path lines
//! (`Auto-merging <path>` / `Added <path> in both, but differently.`) with an
//! `ERROR: content conflict in <path>` on stderr for each path that stayed
//! conflicted. Exit 0 for a clean merge, 1 when a conflict remains.
//!
//! ### Covered (verified against git on Darwin: stdout, stderr, exit code)
//!
//! * `-h`, the outside-a-repository fatal, the `git diff-index --quiet --cached
//!   HEAD --` pre-flight (`Error: Your local changes …`, exit 2, `core.quotePath`
//!   quoting), the argument split, the octopus guard (exit 2), and the baseless
//!   guard (exit 2) — all as before.
//! * The single-base three-way merge: index stages, worktree contents, the
//!   `Trying simple merge.` / `Simple merge failed …` framing, the per-path
//!   `Auto-merging` / `Added … differently.` lines, and the exit code.
//!
//! ### Floors (bail rather than approximate)
//!
//! * Two or more merge bases: `read-tree`'s multi-base `--aggressive` merge is a
//!   stage-collapsing `unpack_trees` state machine gitoxide has no equivalent
//!   for, and the recursive strategy's virtual merge base is a *different*
//!   algorithm, so it is not substituted.
//! * Any shape other than exactly one head and one remote (which would drive
//!   `read-tree`'s two-way or multi-tree merge).
//! * Conflict classes outside the content-merge family (rename/delete,
//!   modify/delete, directory/file, symlink, submodule) and binary content
//!   merges: `git-merge-one-file`'s refusals / `git merge-file`'s binary handling
//!   are not rendered here, exactly as `merge_recursive.rs` refuses them.
//! * `merge.conflictStyle` other than the default `merge`.
//! * An unborn `HEAD`, an already-unmerged index, and a worktree with local
//!   changes that would be overwritten.
//!
//! Conflicted file *contents* are only identical to stock git up to the
//! conflict-marker labels, which git derives from `git merge-file`'s random
//! temp-file names — the same documented non-fidelity as `merge_one_file.rs`.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::tree_with_rewrites::Change;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::apply_index_entries::RemovalMode;
use gix::merge::tree::{Conflict, Resolution, TreatAsUnresolved};
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
    let Ok(repo) = gix::discover(".") else {
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

    // Past this point the script refreshes the index, reads three or more trees
    // into it, and updates the worktree.

    // Multiple merge bases route through read-tree's multi-base --aggressive
    // merge — a stage-collapsing unpack_trees state machine with no gitoxide
    // equivalent. The recursive strategy's virtual merge base is a *different*
    // algorithm, so it is not substituted here.
    if parsed.bases.len() > 1 {
        bail!(
            "unsupported: {} merge bases need read-tree's multi-base --aggressive merge \
             (a stage-collapsing unpack_trees state machine gitoxide has no equivalent for); \
             the recursive strategy's virtual merge base is a different algorithm and is not \
             substituted (ported: the single-base three-way resolve merge)",
            parsed.bases.len()
        );
    }

    // The standard invocation is `<base> -- <head> <remote>`: exactly one head
    // and one remote. Anything else would drive read-tree's two-way or
    // multi-tree merge, which is not ported.
    let (head_spec, remote_spec) = match (parsed.head.as_deref(), parsed.remotes.as_slice()) {
        (Some(head), [remote]) => (head, remote.as_str()),
        _ => bail!(
            "unsupported: merge-resolve without exactly one head and one remote \
             (`<base> -- <head> <remote>`) would drive read-tree's two-way or multi-tree merge, \
             which is not ported (ported: the standard single-base three-way resolve merge)"
        ),
    };
    let base_spec = parsed.bases[0].as_str();

    // `git read-tree … $bases $head $remotes` resolves each argument as a
    // tree-ish; a failure there makes the script exit 2 (`read-tree … || exit 2`).
    let base_tree = match resolve_tree(&repo, base_spec)? {
        Ok(id) => id,
        Err(code) => return Ok(code),
    };
    let head_tree = match resolve_tree(&repo, head_spec)? {
        Ok(id) => id,
        Err(code) => return Ok(code),
    };
    let remote_tree = match resolve_tree(&repo, remote_spec)? {
        Ok(id) => id,
        Err(code) => return Ok(code),
    };

    // The resolve strategy never detects renames (only recursive does), matching
    // read-tree --aggressive + git-merge-one-file, so rewrite tracking is off.
    let mut plumbing_opts: gix::merge::plumbing::tree::Options = repo.tree_merge_options()?.into();
    plumbing_opts.rewrites = None;
    let tree_options: gix::merge::tree::Options = plumbing_opts.into();

    // A non-default conflict style changes the marker text git merge-file would
    // emit, and gix cannot reproduce the diff3/zdiff3 ancestor label, so refuse
    // rather than write different markers.
    if let Some(style) = repo.config_snapshot().string("merge.conflictStyle") {
        if style != "merge" {
            bail!(
                "unsupported: merge.conflictStyle={style} (only the default `merge` style is ported)"
            );
        }
    }

    let labels = Labels {
        ancestor: None,
        current: Some(BStr::new(head_spec.as_bytes())),
        other: Some(BStr::new(remote_spec.as_bytes())),
    };
    let mut outcome = repo.merge_trees(base_tree, head_tree, remote_tree, labels, tree_options)?;

    // Render git-merge-one-file's per-path messages first: a conflict class this
    // port cannot render must fail before a single byte of index or worktree is
    // written.
    let rendered = render_resolve_messages(&repo, &outcome.conflicts)?;

    // write-tree fails exactly when read-tree --aggressive left unmerged entries,
    // i.e. whenever a path changed on both sides — that is the automatic phase.
    let had_unmerged = !outcome.conflicts.is_empty();

    // The `diff-index --cached HEAD` guard above proved the index equals HEAD;
    // guard the worktree too, as merge-recursive does, before writing.
    if repo.is_dirty()? {
        bail!("your local changes would be overwritten by merge; commit or stash them first");
    }

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let how = TreatAsUnresolved::git();
    // A missing base makes git-merge-one-file report a conflict even when the two
    // additions merge cleanly, so the add/add signal is folded in here.
    let conflicted = outcome.has_unresolved_conflicts(how) || rendered.add_add_conflict;
    let merged_tree = outcome.tree.write()?.detach();

    let old_stats = stats_by_path(&old_index);
    let written = apply_to_worktree(&repo, &old_stats, merged_tree)?;

    // Fresh stats for the files we just wrote, previous stats for the ones we
    // left alone, so a following `git status` does not see the tree as dirty.
    let mut index = repo.index_from_tree(&merged_tree)?;
    {
        let backing = index.path_backing().to_owned();
        for e in index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((_, _, stat)) = written.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_stats.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }
    outcome.index_changed_after_applying_conflicts(&mut index, how, RemovalMode::Prune);
    index.remove_tree();
    index.write(Default::default())?;

    // `echo "Trying simple merge."` — printed once read-tree succeeds.
    println!("Trying simple merge.");
    if had_unmerged {
        println!("Simple merge failed, trying Automatic merge.");
    }
    for line in &rendered.stdout {
        println!("{line}");
    }
    for line in &rendered.stderr {
        eprintln!("{line}");
    }

    Ok(if conflicted {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Resolve `spec` to a tree id the way `git read-tree` does. On failure the
/// script's `read-tree … || exit 2` takes over, so this reports git's fatal and
/// asks the caller to exit 2.
fn resolve_tree(repo: &Repository, spec: &str) -> Result<std::result::Result<ObjectId, ExitCode>> {
    let Ok(obj) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: Not a valid object name {spec}");
        return Ok(Err(ExitCode::from(2)));
    };
    let peeled = obj
        .object()
        .map_err(anyhow::Error::from)
        .and_then(|obj| obj.peel_to_tree().map_err(anyhow::Error::from));
    let Ok(tree) = peeled else {
        eprintln!("fatal: failed to unpack tree object {spec}");
        return Ok(Err(ExitCode::from(2)));
    };
    Ok(Ok(tree.id))
}

/// The stdout/stderr `git-merge-index git-merge-one-file -a` would produce for
/// the merge outcome, plus the exit-code signal an add/add carries.
struct Rendered {
    /// `Auto-merging …` / `Added … differently.` lines, ordered by path as
    /// `merge-index` drives `merge-one-file` over the sorted unmerged index.
    stdout: Vec<String>,
    /// `ERROR: content conflict in <path>` for each path that stayed conflicted.
    stderr: Vec<String>,
    /// Whether any add/add path forced a conflict independent of the blob merge.
    add_add_conflict: bool,
}

/// Turn gix's structured conflicts into `git-merge-one-file`'s messages.
///
/// Only the content-merge family is rendered — the same family
/// `merge_recursive.rs` handles. Any other resolution class, a symlink/submodule
/// mode, or a binary blob errors out (a documented floor) before anything is
/// written, rather than inventing text `git-merge-one-file` would not print.
fn render_resolve_messages(repo: &Repository, conflicts: &[Conflict]) -> Result<Rendered> {
    let mut rows: Vec<(BString, String, Option<String>)> = Vec::new();
    let mut add_add_conflict = false;

    for conflict in conflicts {
        let (ours, theirs) = conflict.changes_in_resolution();
        let path = ours.location().to_owned();
        let merged_blob = match &conflict.resolution {
            Ok(Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { merged_blob }) => {
                merged_blob
            }
            _ => bail!(
                "unsupported: conflict at {path} is not a content merge; read-tree --aggressive + \
                 git-merge-one-file resolve rename/delete, modify/delete, directory/file and \
                 submodule cases this port does not render"
            ),
        };

        for change in [ours, theirs] {
            let (mode, id) = change_state(change);
            if !mode.is_blob() {
                bail!(
                    "unsupported: conflict at {path} involves a symlink or submodule; \
                     git-merge-one-file's `Not merging …` refusals are not ported"
                );
            }
            if is_binary(repo, &id)? {
                bail!(
                    "unsupported: conflict at {path} is a binary content merge; git merge-file's \
                     binary handling is not ported"
                );
            }
        }

        let is_add_add =
            matches!(ours, Change::Addition { .. }) && matches!(theirs, Change::Addition { .. });
        let (line, conflicted) = if is_add_add {
            // Base absent: git-merge-one-file always reports a content conflict
            // here, even when the two additions merge without markers.
            add_add_conflict = true;
            (format!("Added {path} in both, but differently."), true)
        } else {
            let conflicted = merged_blob.resolution == gix::merge::blob::Resolution::Conflict;
            (format!("Auto-merging {path}"), conflicted)
        };
        let err = conflicted.then(|| format!("ERROR: content conflict in {path}"));
        rows.push((path, line, err));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for (_, line, err) in rows {
        stdout.push(line);
        if let Some(e) = err {
            stderr.push(e);
        }
    }
    Ok(Rendered {
        stdout,
        stderr,
        add_add_conflict,
    })
}

/// The post-change mode and id of `change` (the rename destination for rewrites).
fn change_state(change: &Change) -> (gix::object::tree::EntryMode, ObjectId) {
    match change {
        Change::Addition { entry_mode, id, .. }
        | Change::Deletion { entry_mode, id, .. }
        | Change::Modification { entry_mode, id, .. }
        | Change::Rewrite { entry_mode, id, .. } => (*entry_mode, *id),
    }
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes of the blob.
fn is_binary(repo: &Repository, id: &ObjectId) -> Result<bool> {
    let data = repo.find_object(*id)?.data.clone();
    let head = &data[..data.len().min(8000)];
    Ok(head.contains(&0))
}

/// Index entries keyed by path, carrying the id, mode and stat data.
fn stats_by_path(index: &gix::index::File) -> HashMap<BString, (ObjectId, Mode, Stat)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat)))
        .collect()
}

/// Materialise `merged_tree` into the worktree: write the files whose content or
/// mode changed relative to `old_stats`, and delete the ones the merge dropped.
/// Returns the freshly written entries, with the stat data checkout recorded.
///
/// This mirrors `merge_recursive.rs`'s private `apply_to_worktree`; the shared
/// primitive it drives is `crate::worktree::checkout_subset`.
fn apply_to_worktree(
    repo: &Repository,
    old_stats: &HashMap<BString, (ObjectId, Mode, Stat)>,
    merged_tree: ObjectId,
) -> Result<HashMap<BString, (ObjectId, Mode, Stat)>> {
    let should_interrupt = AtomicBool::new(false);

    let mut subset = repo.index_from_tree(&merged_tree)?;
    subset.remove_entries(|_, path, entry| match old_stats.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

    if !subset.entries().is_empty() {
        let workdir = repo
            .workdir()
            .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
            .to_owned();
        let mut opts =
            repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
        opts.destination_is_initially_empty = false;
        opts.overwrite_existing = true;
        let odb = repo.objects.clone().into_arc()?;
        crate::worktree::checkout_subset(
            &mut subset,
            workdir.as_path(),
            odb,
            &gix::progress::Discard,
            &gix::progress::Discard,
            &should_interrupt,
            opts,
        )?;
    }

    // Anything tracked before the merge but absent from the merged tree is gone.
    let merged_index = repo.index_from_tree(&merged_tree)?;
    let kept: HashSet<BString> = {
        let backing = merged_index.path_backing();
        merged_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    for path in old_stats.keys() {
        if !kept.contains(path) {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    Ok(stats_by_path(&subset))
}

/// The paths `git diff-index --cached --name-only HEAD --` would print, sorted
/// bytewise as the index — and therefore git's diff queue — orders them.
fn dirty_paths(repo: &Repository) -> Result<Vec<BString>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let head_tree = match repo.head_commit().ok().and_then(|c| c.tree_id().ok()) {
        Some(id) => id.detach(),
        None => bail!(
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
