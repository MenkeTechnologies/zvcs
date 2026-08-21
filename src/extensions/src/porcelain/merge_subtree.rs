//! `git merge-subtree` — the `-subtree` spelling of git's `merge-recursive`
//! plumbing. `git-merge-subtree` is not a separate program: it is
//! `builtin/merge-recursive.c` (`cmd_merge_recursive`) invoked under a name
//! ending in `-subtree`, which is the sole trigger for `o.subtree_shift = ""`.
//! That single assignment routes the merge through `shift_tree()` in
//! `match-trees.c` — the tree-alignment search that scores candidate sub-trees
//! (`score_trees`/`match_trees`) and rewrites one side's tree with
//! `splice_tree()` so a project merged in as a subdirectory lines up with its
//! standalone history — and then performs the ordinary recursive merge on the
//! shifted trees.
//!
//! Both halves are ported. The subtree shift is `match-trees.c`'s
//! `shift_tree`/`shift_tree_by`/`match_trees`/`score_trees`/`splice_tree`,
//! which live once in [`crate::merge_apply`] and are reached from here through
//! this module's [`shift_tree_object`] — git has exactly one copy and so does
//! this port. The merge itself reuses the same driver as the sibling
//! `merge-recursive` port: `Repository::merge_trees` produces the merged tree
//! and structured conflicts, which [`crate::merge_msg`] renders to git's
//! `Auto-merging` / `CONFLICT` message strings before they are written back to
//! the index and worktree with stage 1/2/3 entries.
//!
//! `merge_trees_internal()` shifts both `merge` (the remote tree) and
//! `merge_base` (the ancestor tree) toward `head` before merging, so this
//! module does the same: `shift_tree_object(head, remote)` and
//! `shift_tree_object(head, base)`, then `merge_trees(base, head, remote)`.
//!
//! Covered, byte-for-byte against stock git before the merge starts:
//!   * the `argc < 4` usage guard (exit 129) — fewer than three arguments here,
//!     printed before the repository is touched;
//!   * the positional scan (`--`-prefixed strategy options, `--` terminator,
//!     bases resolved in encounter order), the 20-base ceiling warning, the
//!     `argc - i != 3` arity check, the unmerged-index precondition
//!     (`die_resolve_conflict`, advice-gated), and the `<head>`/`<remote>`
//!     resolution errors, all with git's exact wording and exit codes;
//!   * every branch of `parse_merge_opt()` — the subtree family
//!     (`--subtree` / `--subtree=<path>`), `--ours`, `--theirs`,
//!     `--renormalize`, `--no-renormalize`, `--no-renames`,
//!     `--find-renames[=<n>]`, `--rename-threshold=<n>`, `--patience`,
//!     `--histogram`, `--diff-algorithm=<myers|minimal|patience|histogram>` and
//!     the `--ignore-*-space*` / `--ignore-cr-at-eol` family. It is literally
//!     the porcelain's `-X` parser ([`crate::merge_apply::StrategyOptions`]),
//!     because `cmd_merge_recursive` calls the same `parse_merge_opt()`
//!     (builtin/merge-recursive.c:55-58).
//!
//! Deliberate floors, refused rather than approximated (identical to the
//! `merge-recursive` port, which shares gitoxide's merge substrate):
//!   * the conflict classes [`crate::merge_msg`] still cannot name: a gitlink
//!     content merge (git's `merge_submodule()` diagnostics and its
//!     `advice.submoduleMergeConflict` hint block are not ported) and
//!     `gix-merge`'s `Unknown` catch-all where neither side is a plain type
//!     clash — they error before anything is written;
//!   * `merge.conflictStyle = diff3|zdiff3`;
//!   * a dirty index/worktree: git's `unpack_trees` reconciles local changes
//!     that do not collide; this port requires the index to equal `<head>`'s
//!     tree and the worktree to be clean;
//!   * **two or more merge bases** (explicit, or computed for a criss-cross
//!     history): git builds a virtual merge base by recursively merging the
//!     bases, and that recursion applies the subtree shift at every level.
//!     `Repository::virtual_merge_base` cannot thread the shift through its
//!     recursion, so a faithful multi-base subtree merge is not possible on the
//!     current substrate. Zero or one merge base (the common case, including a
//!     single computed base) is fully handled.
//!
//! One deliberate divergence on a git bug is inherited from the resolution
//! path: stock git 2.55.0 segfaults (exit 139) when `<head>` or `<remote>` is a
//! full-length hex id naming a missing object; this module reports the missing
//! object instead.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::apply_index_entries::RemovalMode;
use gix::merge::tree::TreatAsUnresolved;

/// Verbatim `builtin_merge_recursive_usage`, already interpolated with the
/// `merge-subtree` command name that dispatch reaches this module under.
const USAGE: &str = "usage: git merge-subtree <base>... -- <head> <remote> ...\n";

/// The most merge bases `cmd_merge_recursive` will hold (`ARRAY_SIZE(bases) - 1`).
const MAX_BASES: usize = 20;



/// `o.subtree_shift`, whose default for a `-subtree`-suffixed invocation is the
/// empty string (automatic detection). `--subtree` (and `--subtree=`) select
/// [`Auto`](Self::Auto); `--subtree=<path>` selects [`By`](Self::By).
enum SubtreeShift {
    /// `!*subtree_shift`: run `shift_tree` to detect the alignment.
    Auto,
    /// A user-supplied shift prefix: run `shift_tree_by`.
    By(Vec<u8>),
}

/// `git merge-subtree` — three-way merge with subtree alignment.
pub fn merge_subtree(args: &[String]) -> Result<ExitCode> {
    // `if (argc < 4) usagef(...)`. argc counts argv[0], so this is three
    // arguments here, and it fires before the repository is opened.
    // `show_usage_if_asked(argc, argv, msg.buf)` (builtin/merge-recursive.c:45)
    // precedes the `argc < 4` refusal and prints to stdout instead of stderr.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }
    if args.len() < 3 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    // The positional scan. `end` ends up at the index of the `--` that stopped
    // it, or at args.len() when no `--` was seen — mirroring C's `i` shifted by
    // one for the missing argv[0].
    let mut bases: Vec<ObjectId> = Vec::new();
    // `if (ends_with(argv[0], "-subtree")) o.subtree_shift = "";`
    // (builtin/merge-recursive.c:34) — set before the option scan, so an
    // explicit `--subtree=<prefix>` still overrides it.
    let mut xopts = crate::merge_apply::StrategyOptions {
        subtree_shift: Some(gix::bstr::BString::default()),
        ..Default::default()
    };
    let mut end = args.len();
    for (idx, arg) in args.iter().enumerate() {
        if let Some(opt) = arg.strip_prefix("--") {
            if opt.is_empty() {
                end = idx;
                break;
            }
            // The same `parse_merge_opt()` the porcelain runs over `-X`
            // (builtin/merge-recursive.c:55-58), so the plumbing honours the
            // same set: `--ours`/`--theirs`, the `--ignore-*-space*` family and
            // `--renormalize` included.
            if crate::merge_apply::StrategyOptions::parse_from(xopts.clone(), &[opt.to_string()])
                .map(|updated| xopts = updated)
                .is_err()
            {
                eprintln!("fatal: unknown option {arg}");
                return Ok(ExitCode::from(128));
            }
            continue;
        }
        if bases.len() < MAX_BASES {
            let Some(oid) = resolve_object(&repo, arg) else {
                eprintln!("fatal: could not parse object '{arg}'");
                return Ok(ExitCode::from(128));
            };
            bases.push(oid);
        } else {
            // C warns and does not parse; the count is always plural here.
            eprintln!("warning: cannot handle more than {MAX_BASES} bases. Ignoring {arg}.");
        }
    }

    // `if (argc - i != 3)`: exactly `--`, `<head>`, `<remote>` must remain.
    if args.len() - end != 3 {
        eprintln!("fatal: not handling anything other than two heads merge.");
        return Ok(ExitCode::from(128));
    }

    // `repo_read_index_unmerged()` runs before the two heads are resolved.
    let old_index = repo.index_or_load_from_head()?.into_owned();
    if old_index.entries().iter().any(|e| e.stage_raw() != 0) {
        eprintln!("error: Merging is not possible because you have unmerged files.");
        // `error_resolve_conflict` (sequencer.c) prints the error unconditionally
        // and the two-line direction only under `advice.resolveConflict`.
        crate::advice::Advice::ResolveConflict.advise_plain_in(
            &repo,
            "Fix them up in the work tree, and then use 'git add/rm <file>'\n\
             as appropriate to mark resolution and make a commit.",
        );
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }

    let branch1 = &args[end + 1];
    let branch2 = &args[end + 2];
    let Some(head_id) = resolve_object(&repo, branch1) else {
        eprintln!("fatal: could not resolve ref '{branch1}'");
        return Ok(ExitCode::from(128));
    };
    let Some(remote_id) = resolve_object(&repo, branch2) else {
        eprintln!("fatal: could not resolve ref '{branch2}'");
        return Ok(ExitCode::from(128));
    };

    // Conflict markers carry git's `better_branch_name` labels.
    let label1 = better_branch_name(branch1);
    let label2 = better_branch_name(branch2);

    let style = repo.config_snapshot().string("merge.conflictStyle");
    if let Some(style) = style {
        if style != "merge" {
            bail!("merge.conflictStyle={style} is not ported (only the default `merge` style is)");
        }
    }

    // The trees to merge. `head` is the alignment target for the shift.
    let head_tree = commit_tree(&repo, head_id)?;
    let remote_tree = commit_tree(&repo, remote_id)?;

    // The ancestor tree, exactly as `merge_recursive_generic`/`merge_recursive`
    // derive it: an explicit base is used verbatim, otherwise the merge base of
    // the two commits is computed. The virtual-base recursion needed for two or
    // more bases cannot carry the subtree shift, so that case is a floor.
    let (base_tree, ancestor_label) = match bases.len() {
        0 => {
            let head_commit = commit_id(&repo, head_id)?;
            let remote_commit = commit_id(&repo, remote_id)?;
            let computed = repo.merge_bases_many(head_commit, &[remote_commit])?;
            match computed.len() {
                // "if there is no common ancestor, use an empty tree"
                0 => (ObjectId::empty_tree(repo.object_hash()), None),
                1 => (
                    repo.find_commit(computed[0].detach())?.tree_id()?.detach(),
                    None,
                ),
                _ => crate::git_fatal!(
                    "merge-subtree cannot be performed: the history has {} merge bases \
                     (criss-cross), whose virtual merge base git builds by recursively \
                     merging them with the subtree shift applied at each level; \
                     Repository::virtual_merge_base cannot thread the shift through its \
                     recursion",
                    computed.len()
                ),
            }
        }
        1 => (
            commit_tree(&repo, bases[0])?,
            Some("constructed merge base".to_string()),
        ),
        n => crate::git_fatal!(
            "merge-subtree cannot be performed: {n} explicit merge bases require a virtual \
             merge base built by recursively merging them with the subtree shift applied at \
             each level; Repository::virtual_merge_base cannot thread the shift through its \
             recursion"
        ),
    };

    // The command's reason to exist: shift both non-head trees toward head. An
    // empty prefix is git's `""`, i.e. work the shift out from the tree shapes.
    let subtree_shift = match xopts.subtree_shift.as_deref() {
        Some(prefix) if prefix.is_empty() => SubtreeShift::Auto,
        Some(prefix) => SubtreeShift::By(prefix.to_vec()),
        None => SubtreeShift::Auto,
    };
    let remote_shifted = shift_tree_object(&repo, head_tree, remote_tree, &subtree_shift)?;
    let base_shifted = shift_tree_object(&repo, head_tree, base_tree, &subtree_shift)?;

    // `-Xrenormalize` reaches the blob pipeline through the repository's own
    // `merge.renormalize`, so it is applied to a private clone first.
    let renormalized = crate::merge_apply::renormalized_repo(&repo, &xopts)?;
    let repo = renormalized.unwrap_or(repo);

    // The same `-X` knobs the porcelain applies, from the same place.
    let tree_options = crate::merge_apply::tree_merge_options(&repo, &xopts, None, false)?;

    let labels = Labels {
        ancestor: ancestor_label.as_deref().map(|s| BStr::new(s.as_bytes())),
        current: Some(BStr::new(label1.as_bytes())),
        other: Some(BStr::new(label2.as_bytes())),
    };
    let mut outcome = repo.merge_trees(
        base_shifted,
        head_tree,
        remote_shifted,
        labels,
        tree_options,
    )?;

    // Render every message first: an unrenderable conflict class must fail
    // before a single byte of index or worktree is touched.
    let messages = crate::merge_msg::render(
        &repo,
        &outcome.conflicts,
        &label1,
        &label2,
        crate::merge_msg::Operand1::Tree(head_tree),
        TreatAsUnresolved::git(),
        crate::merge_msg::Strictness::Refuse,
    )?;

    // Conservative precondition (documented deviation): the index must equal
    // `<head>`'s tree and the worktree must be clean.
    // merge-ort's `merge_start()` sanity check: `repo_index_has_changes()`
    // against `<head>`, which refuses the whole merge — naming the paths, two
    // spaces in, with no advice line — when the index carries a staged change.
    let staged = crate::merge_guard::index_changes_from_head(&repo, head_tree, &old_index)?;
    if !staged.is_empty() {
        crate::merge_guard::report_index_changes(&staged);
        return Ok(ExitCode::from(128));
    }

    let how = TreatAsUnresolved::git();
    let conflicted = outcome.has_unresolved_conflicts(how);
    let merged_tree = outcome.tree.write()?.detach();

    // `merge_switch_to_result()`'s `checkout()`: an `unpack_trees()` from
    // `<head>`'s tree to the merged one, which refuses rather than overwrite
    // local work — but **per path**, so an edit outside the merge's footprint is
    // not a reason to refuse anything. The blanket `repo.is_dirty()` this
    // replaced turned any uncommitted edit anywhere in the tree into a refusal,
    // which made the command unusable in a working repository, and it never
    // looked at untracked files at all: an untracked file standing where the
    // merge wanted to write one was silently overwritten at exit 0.
    //
    // git checks out *before* it displays the messages (merge-ort.c:4964), so a
    // refusal here prints only the `unpack_trees` block — the `Auto-merging`
    // lines belong to a merge that did not happen.
    let clobber = crate::merge_guard::verify_two_way(&repo, head_tree, merged_tree, &old_index)?;
    if !clobber.is_empty() {
        clobber.report("merge");
        return Ok(ExitCode::from(128));
    }

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
    // `unpack_trees()` ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`
    // (unpack-trees.c:2088-2092), so the index git leaves here carries a cache-tree.
    super::write_tree::rebuild_cache_tree(&repo, &mut index);
    index.write(crate::config::index_write_options(&repo))?;

    // `merge_ort_generic()` reaches `merge_switch_to_result()` like every other
    // merge-ort caller, so the plumbing verb leaves `AUTO_MERGE` behind too.
    crate::merge_apply::write_auto_merge(&repo, merged_tree)?;

    let mut buf: Vec<u8> = Vec::new();
    for m in &messages {
        buf.extend_from_slice(m.text.as_bytes());
    }
    std::io::stdout().lock().write_all(&buf)?;

    Ok(if conflicted {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `shift_tree_object()`: shift `two` (the remote or ancestor tree) so it lines
/// up with `head_tree`, either automatically or by the user-supplied prefix.
///
/// The `match-trees.c` port itself lives in [`crate::merge_apply`], shared with
/// `git merge -Xsubtree`, `merge-recursive` and `merge-tree`, because git has
/// exactly one — `merge_ort_nonrecursive_internal()` calls
/// `shift_tree_object()` and every caller reaches it from there. This module
/// carried a second copy whose `splice_tree()` was written on gitoxide's tree
/// editor rather than `match-trees.c`'s in-place buffer rewrite: it created any
/// missing intermediate trees where the C `die()`s, and re-serialized the parent
/// chain where the C preserves it byte for byte. Neither difference is reachable
/// from here — both entry points only ever splice at a prefix they have already
/// confirmed is a tree in `head_tree` — and stock 2.55.0 agrees with both copies
/// across the `--subtree`, `--subtree=<p>`, trailing-slash, nested, absent and
/// non-tree prefixes, so the faithful one is the one that survives.
fn shift_tree_object(
    repo: &gix::Repository,
    head_tree: ObjectId,
    two: ObjectId,
    shift: &SubtreeShift,
) -> Result<ObjectId> {
    // `merge_apply`'s entry point takes git's own `o.subtree_shift` string, whose
    // empty value *is* the automatic mode.
    let prefix: &[u8] = match shift {
        SubtreeShift::Auto => b"",
        SubtreeShift::By(prefix) => prefix,
    };
    crate::merge_apply::shift_tree_object(repo, head_tree, two, prefix.as_bstr())
}

/// `repo_get_oid()` as this command needs it: a full-length hex id is accepted
/// verbatim, without checking that the object exists, and anything else is a
/// revision expression. `None` is C's non-zero return.
///
/// `cmd_merge_recursive()` (`builtin/merge-recursive.c:62,82,84`) calls
/// `repo_get_oid()` once for each base and once for each of the two heads, so
/// every operand reaches `get_oid_basic()` exactly once — and earns its
/// `warning: refname … is ambiguous.` when the repository also holds a ref by
/// that name.
///
/// The hand-written copy this replaced re-derived only the *decode* half of that
/// branch and returned before anything could warn, so all three operands were
/// silent where stock warns once each. It also inherited gitoxide's reading of
/// `<rev>^!`, which `get_oid_1()` has no case for — see
/// [`crate::objname::resolve`], which is both halves and the whole grammar.
fn resolve_object(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    crate::objname::resolve(repo, spec)
}

/// Peel a resolved id to the commit it names (git's `lookup_commit_reference`).
fn commit_id(repo: &gix::Repository, id: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_object(id)?.peel_to_commit()?.id)
}

/// Peel a resolved id to its commit's tree.
fn commit_tree(repo: &gix::Repository, id: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_object(id)?.peel_to_commit()?.tree_id()?.detach())
}

/// git's `better_branch_name`: a full hex object id is replaced by
/// `$GITHEAD_<oid>` when that variable is set, so `git merge` can pass a
/// readable name down to the strategy. Anything else is used verbatim.
fn better_branch_name(branch: &str) -> String {
    let hexsz = gix::hash::Kind::Sha1.len_in_hex();
    if branch.len() != hexsz {
        return branch.to_owned();
    }
    std::env::var(format!("GITHEAD_{branch}")).unwrap_or_else(|_| branch.to_owned())
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
fn apply_to_worktree(
    repo: &gix::Repository,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One `parse_merge_opt()` call over the `-subtree` seed, as the driver runs
    /// it. `Err` is git's own `unknown option` refusal.
    fn parse(
        opt: &str,
    ) -> std::result::Result<crate::merge_apply::StrategyOptions, crate::merge_apply::StrategyOptionError>
    {
        crate::merge_apply::StrategyOptions::parse_from(
            crate::merge_apply::StrategyOptions {
                subtree_shift: Some(gix::bstr::BString::default()),
                ..Default::default()
            },
            &[opt.to_string()],
        )
    }

    #[test]
    fn honours_the_options_the_merge_can_apply() {
        for ok in [
            "subtree",
            "subtree=",
            "subtree=dir",
            "histogram",
            "diff-algorithm=myers",
            "diff-algorithm=default",
            "diff-algorithm=minimal",
            "diff-algorithm=histogram",
            "patience",
            "diff-algorithm=patience",
            "no-renames",
            "find-renames",
            "find-renames=",
            "find-renames=.",
            "find-renames=%",
            "find-renames=50",
            "find-renames=50%",
            "find-renames=5.5",
            "find-renames=5.5%",
            "rename-threshold=5",
            "rename-threshold=",
        ] {
            parse(ok).unwrap_or_else(|_| panic!("git accepts --{ok}"));
        }
    }

    /// These used to be refused; they now go through the shared
    /// `parse_merge_opt()` port and reach the merge, as they do in git.
    #[test]
    fn honours_the_rest_of_parse_merge_opt() {
        use crate::merge_apply::Variant;
        assert_eq!(parse("ours").expect("accepted").variant, Some(Variant::Ours));
        assert_eq!(parse("theirs").expect("accepted").variant, Some(Variant::Theirs));
        assert_eq!(parse("renormalize").expect("accepted").renormalize, Some(true));
        assert_eq!(parse("no-renormalize").expect("accepted").renormalize, Some(false));
        for ws in [
            "ignore-space-change",
            "ignore-all-space",
            "ignore-space-at-eol",
            "ignore-cr-at-eol",
        ] {
            assert_ne!(parse(ws).expect("accepted").xdl_opts, 0, "--{ws} sets an XDF flag");
        }
    }

    #[test]
    fn rejects_options_git_itself_rejects() {
        // Verified against git 2.55.0: each of these is `fatal: unknown option`.
        for bad in [
            "diff-algorithm=bogus",
            "find-renames=1x",
            "find-renames=bogus",
            "no-renames=1",
            "ort",
            "recursive",
            "verbose",
            "bogus",
        ] {
            assert!(parse(bad).is_err(), "git rejects --{bad}");
        }
    }

    #[test]
    fn subtree_flag_selects_the_shift_mode() {
        // `""` is git's automatic shift; anything else is the prefix to shift by.
        assert_eq!(parse("subtree").unwrap().subtree_shift.map(Vec::from), Some(b"".to_vec()));
        assert_eq!(parse("subtree=").unwrap().subtree_shift.map(Vec::from), Some(b"".to_vec()));
        assert_eq!(
            parse("subtree=lib/foo").unwrap().subtree_shift.map(Vec::from),
            Some(b"lib/foo".to_vec())
        );
    }

    #[test]
    fn parse_rename_score_matches_git() {
        // `parse_rename_score()` (diff.c) reads `<num>` as `num/scale` of
        // `MAX_SCORE` (60000), where `scale` is 10 per digit seen — so a bare
        // `100` is *ten* percent and only `100%` is a hundred. Measured against
        // stock 2.55.0 on a 95%-similar rename: `-Xfind-renames=100` detects it
        // and merges cleanly, `-Xfind-renames=100%` does not and conflicts.
        let score = |v: &str| {
            parse(&format!("find-renames={v}"))
                .unwrap_or_else(|_| panic!("git accepts --find-renames={v}"))
                .rename_score
        };
        assert_eq!(score("50"), 30000);
        assert_eq!(score("50%"), 30000);
        assert_eq!(score("100"), 6000);
        assert_eq!(score("100%"), 60000);
        assert_eq!(score("5.5"), 33000);
        assert_eq!(score("0"), 0);
        // An empty/./% value reads as score 0 — verified against git 2.55.0:
        // `git merge-subtree --find-renames= …` is accepted, not "unknown option".
        assert_eq!(score(""), 0);
        assert_eq!(score("."), 0);
        assert_eq!(score("%"), 0);
    }

}
