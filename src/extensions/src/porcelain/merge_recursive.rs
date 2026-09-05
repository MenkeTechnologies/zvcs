//! `git merge-recursive` — the low-level recursive-strategy driver: merge
//! `<remote>` into `<head>` over zero or more explicit merge bases, updating the
//! index and the working tree in place.
//!
//! Unlike `merge-tree`, this command is a mutator. The merge itself is done by
//! the vendored `gix-merge` tree merge (three-way content merges, rename
//! detection, recursive merge-base consolidation via
//! `Repository::virtual_merge_base`, or by
//! [`crate::merge_apply::shifted_virtual_base_tree`] when a subtree shift has to
//! ride along with it); the resulting tree is then materialised
//! into the worktree and written to `.git/index` with stage 1/2/3 entries for
//! every unresolved path.
//!
//! Covered, byte-for-byte against stock git:
//!   * argument grammar `[--<option>]... <base>... -- <head> <remote>`, including
//!     the `--` terminator rule, the 20-base cap warning, and the
//!     `<base>` / `<head>` / `<remote>` resolution errors
//!   * the usage line (exit 129) when fewer than three arguments follow the
//!     subcommand name
//!   * the unmerged-index precondition block (exit 128)
//!   * merge-ort's informational messages on stdout, rendered by the shared
//!     [`crate::merge_msg`] in its strict mode (this command renders before it
//!     writes, so an unrenderable class costs nothing to refuse): the
//!     `Auto-merging` / `CONFLICT (content|add/add|submodule)` content family
//!     with its `warning: Cannot merge binary files` and symlink variants, plus
//!     `modify/delete`, `rename/delete`, `rename/rename`, `file/directory` and
//!     `distinct types`, sorted by primary path the way
//!     `merge_display_update_messages()` prints them. Conflict markers are
//!     labelled with the `<head>` and
//!     `<remote>` argument strings (or their `GITHEAD_<oid>` environment
//!     override, exactly as git's `better_branch_name` does)
//!   * exit 0 for a clean merge, 1 when conflicts remain, 128 for the fatal paths
//!   * every branch of `parse_merge_opt()` except the subtree family:
//!     `--ours`, `--theirs`, `--renormalize`, `--no-renormalize`,
//!     `--no-renames`, `--find-renames[=<n>]`, `--rename-threshold=<n>`,
//!     `--patience`, `--histogram`,
//!     `--diff-algorithm=<myers|minimal|patience|histogram>`,
//!     `--ignore-space-change`, `--ignore-all-space`, `--ignore-space-at-eol`
//!     and `--ignore-cr-at-eol`. `cmd_merge_recursive` calls the very same
//!     `parse_merge_opt()` the porcelain runs over `-X`
//!     (builtin/merge-recursive.c:55-58), so this shares
//!     [`crate::merge_apply::StrategyOptions`] with it rather than keeping a
//!     second opinion about which options are honourable.
//!   * `--subtree` and `--subtree=<path>`. `merge-recursive` and `merge-subtree`
//!     are one program — `cmd_merge_recursive()` only checks whether `argv[0]`
//!     ends in `-subtree` to seed `o.subtree_shift = ""`
//!     (builtin/merge-recursive.c:38-39) — so the flag reaches
//!     `merge_ort_internal()`'s shift (merge-ort.c:5243-5248) under either name.
//!     The shift itself is [`crate::merge_apply::shift_tree_object`], which is
//!     also what the porcelain's `-Xsubtree` runs.
//!
//!   * `--subtree` over a criss-cross history, or over two or more explicit
//!     bases. git shifts inside *every* level of the virtual-merge-base
//!     recursion, which `Repository::virtual_merge_base` cannot express, so that
//!     recursion is spelled out in
//!     [`crate::merge_apply::shifted_virtual_base_tree`] instead
//!
//! Not covered, and refused rather than approximated:
//!   * the conflict classes [`crate::merge_msg`] still cannot name: a gitlink
//!     content merge (git's `merge_submodule()` diagnostics and its
//!     `advice.submoduleMergeConflict` hint block are not ported) and
//!     `gix-merge`'s `Unknown` catch-all where neither side is a plain type
//!     clash. Both error out *before* anything is written
//!   * `merge.conflictStyle = diff3|zdiff3` — the ancestor label git uses here
//!     is not reproduced, so a non-default style is refused
//!   * git's `unpack_trees` reconciliation of a dirty index/worktree. Stock git
//!     accepts local changes that do not collide with the merge; this port
//!     requires the index to equal `<head>`'s tree and the worktree to be clean,
//!     and bails otherwise rather than risking a wrong write.

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

/// Verbatim `git merge-recursive` usage line (git exits 129 after printing it).
const USAGE: &str = "usage: git merge-recursive <base>... -- <head> <remote> ...\n";

/// git's `bases[21]` array holds one spare slot, so at most 20 bases are kept.
const MAX_BASES: usize = 20;



/// `git merge-recursive [--<option>]... <base>... -- <head> <remote>`.
pub fn merge_recursive(args: &[String]) -> Result<ExitCode> {
    // git checks `argc < 4` counting argv[0], and `args` here starts *after* the
    // subcommand name, so the same gate is `args.len() < 3`.
    // `show_usage_if_asked(argc, argv, msg.buf)` (builtin/merge-recursive.c:45)
    // precedes the `argc < 4` refusal and prints to stdout instead of stderr.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }
    if args.len() < 3 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = crate::setup::discover()?;

    // `init_basic_merge_options(&o, …)`. The `-subtree` alias's
    // `o.subtree_shift = ""` is not seeded here: that name has its own driver
    // (`super::merge_subtree`), which does the shifting.
    let mut xopts = crate::merge_apply::StrategyOptions::default();
    let mut base_specs: Vec<&str> = Vec::new();

    // Leading `--…` arguments are merge options; everything else is a merge
    // base, until a bare `--` ends the base list. C's `i` starts at 1 because it
    // is indexing argv; `args` has no argv[0], so the scan starts at 0 — starting
    // at 1 silently dropped whatever was written first (a strategy option, a
    // base, or an option git itself rejects).
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(opt) = arg.strip_prefix("--") {
            if opt.is_empty() {
                break;
            }
            // `if (parse_merge_opt(&o, arg + 2)) die(_("unknown option %s"), arg);`
            // — the very same `parse_merge_opt()` the porcelain runs over `-X`,
            // so the plumbing honours the same set.
            if crate::merge_apply::StrategyOptions::parse_from(xopts.clone(), &[opt.to_string()])
                .map(|updated| xopts = updated)
                .is_err()
            {
                eprintln!("fatal: unknown option {arg}");
                return Ok(ExitCode::from(128));
            }
            i += 1;
            continue;
        }
        if base_specs.len() < MAX_BASES {
            base_specs.push(arg);
        } else {
            eprintln!("warning: cannot handle more than {MAX_BASES} bases. Ignoring {arg}.");
        }
        i += 1;
    }

    // git resolves the bases as it collects them, so a bad base is reported
    // before the "two heads" arity check.
    let mut bases: Vec<ObjectId> = Vec::with_capacity(base_specs.len());
    for spec in &base_specs {
        match resolve(&repo, spec) {
            Some(id) => bases.push(id),
            None => {
                eprintln!("fatal: could not parse object '{spec}'");
                return Ok(ExitCode::from(128));
            }
        }
    }

    // `i` sits on the `--`; exactly `-- <head> <remote>` must follow.
    if args.len() - i != 3 {
        eprintln!("fatal: not handling anything other than two heads merge.");
        return Ok(ExitCode::from(128));
    }
    let branch1 = args[i + 1].as_str();
    let branch2 = args[i + 2].as_str();

    // The unmerged-index precondition is checked before the heads are resolved.
    let old_index = repo.index_or_load_from_head()?.into_owned();
    if old_index.entries().iter().any(|e| e.stage_raw() != 0) {
        eprintln!("error: Merging is not possible because you have unmerged files.");
        // `error_resolve_conflict` (sequencer.c) prints the error unconditionally
        // and the two-line direction only under `advice.resolveConflict`.
        crate::advice::Advice::ResolveConflict.advise_plain(
            "Fix them up in the work tree, and then use 'git add/rm <file>'\n\
             as appropriate to mark resolution and make a commit.",
        );
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }

    let Some(head_id) = resolve(&repo, branch1) else {
        eprintln!("fatal: could not resolve ref '{branch1}'");
        return Ok(ExitCode::from(128));
    };
    let Some(remote_id) = resolve(&repo, branch2) else {
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

    // `-Xrenormalize` reaches the blob pipeline through the repository's own
    // `merge.renormalize`, so it is applied to a private clone before anything
    // reads a blob.
    let renormalized = crate::merge_apply::renormalized_repo(&repo, &xopts)?;
    let repo = renormalized.unwrap_or(repo);

    // The same `-X` knobs the porcelain applies, from the same place.
    let tree_options = crate::merge_apply::tree_merge_options(&repo, &xopts, None, false)?;

    let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();
    let remote_tree = repo.find_commit(remote_id)?.tree_id()?.detach();

    // `if (opt->subtree_shift) { side2 = shift_tree_object(repo, side1, side2,
    // opt->subtree_shift); merge_base = shift_tree_object(repo, side1,
    // merge_base, opt->subtree_shift); }` (merge-ort.c:5243-5248) — the shift
    // aligns the *remote* and the *base* onto the head, and it runs inside
    // `merge_ort_internal()`, which every caller reaches.
    //
    // `merge-recursive` and `merge-subtree` are one program: `cmd_merge_recursive()`
    // (builtin/merge-recursive.c:24-100) differs only in whether `argv[0]` ends
    // in `-subtree`, which seeds `o.subtree_shift = ""`
    // (builtin/merge-recursive.c:38-39). Both names then run the same
    // `parse_merge_opt()`, whose `subtree` / `subtree=<path>` branches
    // (merge-ort.c:5551-5554) are therefore reachable under either — so refusing
    // `--subtree` here was divergence, not a floor. Measured against stock
    // 2.55.0: `git merge-recursive --subtree=sub <base> -- main side` reports
    // `Auto-merging f.txt` / `CONFLICT (content): Merge conflict in f.txt` and
    // leaves three stages of `f.txt`, where the same command without the flag
    // reports a `sub/f.txt` modify/delete.
    //
    // The shift is applied through [`crate::merge_apply::shift_tree_object`],
    // which is `match-trees.c`'s `shift_tree`/`shift_tree_by`/`splice_tree` —
    // the same function the porcelain's `-Xsubtree` goes through, so the two
    // spellings cannot drift apart.
    let subtree_shift = xopts.subtree_shift.clone();

    // git's virtual commits are `alloc_commit_node()`s and are never written
    // (`make_virtual_commit()`, merge-ort.c:5006-5015): a criss-cross merge
    // leaves the merged base *tree* and the blobs it needed in the object store
    // and no commit at all. `gix-merge` writes its virtual commits, so the merge
    // runs against an in-memory object store and only the objects git would have
    // written are persisted afterwards — the same trick
    // [`super::merge::virtual_base_tree`] plays for the porcelain.
    let merge_repo = {
        let mut mem = repo.clone();
        mem.objects.enable_object_memory();
        mem
    };

    // With no explicit bases git computes them itself (recursively merging
    // multiple bases); with bases given it uses exactly those. A shift forces
    // the explicit-tree path even with no bases, because the base tree has to be
    // in hand to be shifted.
    let mut outcome = if bases.is_empty() && subtree_shift.is_none() {
        let labels = Labels {
            ancestor: None,
            current: Some(BStr::new(label1.as_bytes())),
            other: Some(BStr::new(label2.as_bytes())),
        };
        let commit_options =
            gix::merge::commit::Options::from(tree_options).with_allow_missing_merge_base(true);
        merge_repo
            .merge_commits(head_id, remote_id, labels, commit_options)?
            .tree_merge
    } else {
        let (base_tree, ancestor_label) = if bases.is_empty() {
            // The shift path derives the base the way `merge_ort_recursive()`
            // does before it calls `merge_ort_internal()`.
            let computed = repo.merge_bases_many(head_id, &[remote_id])?;
            match computed.len() {
                // "if there is no common ancestor, use an empty tree"
                0 => (ObjectId::empty_tree(repo.object_hash()), None),
                1 => (repo.find_commit(computed[0].detach())?.tree_id()?.detach(), None),
                // The virtual merge base is built by recursively merging the
                // bases, and `merge_ort_internal()` applies the shift at every
                // level of that recursion — which is why it cannot be delegated
                // to `Repository::virtual_merge_base`.
                //
                // `repo_get_merge_bases()` is followed by `commit_list_reverse()`
                // (merge-ort.c:5316-5322), so the list `pop_commit()` walks is the
                // reverse of the one the traversal produced.
                _ => {
                    let mut computed: Vec<ObjectId> =
                        computed.iter().map(|id| id.detach()).collect();
                    computed.reverse();
                    (
                        crate::merge_apply::shifted_virtual_base_tree(
                            &merge_repo,
                            &computed,
                            subtree_shift.as_ref().expect("the shift path").as_ref(),
                            &tree_options,
                        )?,
                        Some("merged common ancestors"),
                    )
                }
            }
        } else if bases.len() == 1 {
            (
                repo.find_commit(bases[0])?.tree_id()?.detach(),
                Some("constructed merge base"),
            )
        } else if let Some(prefix) = &subtree_shift {
            // `merge_ort_generic()` builds `ca` with `commit_list_insert()`
            // (merge-ort-wrappers.c:107-113), which *prepends* — so the last
            // `<base>` written on the command line is the head of the list and the
            // one `pop_commit()` takes first.
            let mut ca: Vec<ObjectId> = bases.clone();
            ca.reverse();
            (
                crate::merge_apply::shifted_virtual_base_tree(
                    &merge_repo,
                    &ca,
                    prefix.as_ref(),
                    &tree_options,
                )?,
                Some("merged common ancestors"),
            )
        } else {
            (
                // git's virtual commits are `alloc_commit_node()`s
                // (`make_virtual_commit()`, merge-ort.c:5006-5015) and are never
                // written; only the merged base *tree* and the blobs it needed
                // reach the object store. `Repository::virtual_merge_base` writes
                // its virtual commits too, so the recursion runs against an
                // in-memory store and only the objects git would have written are
                // persisted — the same path `git merge`'s criss-cross takes.
                super::merge::virtual_base_tree_with(&repo, &bases, Some(tree_options.clone()))?,
                Some("merged common ancestors"),
            )
        };
        let (base_tree, remote_tree) = match &subtree_shift {
            // Through `merge_repo`, not `repo`: a virtual merge base built above
            // lives only in the in-memory store until the merge is done, so the
            // shift has to be able to read it back.
            Some(prefix) => (
                crate::merge_apply::shift_tree_object(
                    &merge_repo,
                    head_tree,
                    base_tree,
                    prefix.as_ref(),
                )?,
                crate::merge_apply::shift_tree_object(
                    &merge_repo,
                    head_tree,
                    remote_tree,
                    prefix.as_ref(),
                )?,
            ),
            None => (base_tree, remote_tree),
        };
        let labels = Labels {
            ancestor: ancestor_label.map(|s| BStr::new(s.as_bytes())),
            current: Some(BStr::new(label1.as_bytes())),
            other: Some(BStr::new(label2.as_bytes())),
        };
        merge_repo.merge_trees(base_tree, head_tree, remote_tree, labels, tree_options)?
    };

    // Render every message first: an unrenderable conflict class must fail
    // before a single byte of index or worktree is touched.
    let messages = crate::merge_msg::render(
        &merge_repo,
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

    // Everything the merge produced now reaches the real object store *except*
    // the virtual commits, which git never writes. This is where git's own
    // writes have landed by the time `merge_switch_to_result()` starts its
    // checkout (merge-ort.c:4964), so a refusal below leaves the same objects
    // behind that stock leaves.
    {
        let written = merge_repo
            .objects
            .reset_object_memory()
            .expect("object memory was just enabled");
        for (_id, (kind, data)) in written.iter() {
            if *kind == gix::object::Kind::Commit {
                continue;
            }
            gix::objs::Write::write_buf(&repo, *kind, data)
                .map_err(|e| anyhow!("failed to write merge object: {e}"))?;
        }
    }

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
    // `merge_switch_to_result()` does the two in this order (merge-ort.c:4964-4975):
    // `checkout()` — an `unpack_trees()` onto the as-merged-as-possible tree whose
    // tail is `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`
    // (unpack-trees.c:2088-2092) — and only then
    // `record_conflicted_index_entries()`, which swaps each conflicted path's
    // stage-0 entry for its stage 1/2/3 ones and ends in
    // `remove_marked_cache_entries(index, 1)` (merge-ort.c:4509), the `1` being
    // `invalidate_cache_tree`.
    //
    // Repairing *after* the conflicts instead made the repair a no-op —
    // `verify_cache()` refuses an unmerged entry (cache-tree.c:218-234) and
    // `cache_tree_update()` returns before touching `istate->cache_tree` — and
    // `rebuild_cache_tree` had already dropped what the pre-merge index carried, so
    // a conflicting merge-recursive wrote an index with no `TREE` extension at all
    // where stock leaves the carried structure with the touched nodes at `-1`.
    // This is the same ordering `crate::merge_apply` runs for every porcelain
    // merge-shaped verb.
    super::write_tree::carry_and_repair_cache_tree(&repo, &old_index, &mut index);
    outcome.index_changed_after_applying_conflicts(&mut index, how, RemovalMode::Prune);
    // `remove_marked_cache_entries(index, 1)`: the stage-0 entry each conflicted
    // path had is gone, and its node — and every node above it — goes with it.
    for path in crate::merge_apply::unmerged_paths(&index) {
        index.invalidate_path_in_tree(path.as_ref());
    }
    super::write_tree::prepare_offset_table(&repo, &mut index);
    crate::index_racy::write(&repo, &mut index)?;

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

/// Resolve `spec` to a commit id, or `None` when git would fail to.
fn resolve(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
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
            crate::merge_apply::remove_worktree_entry(repo, path.as_bstr());
        }
    }

    Ok(stats_by_path(&subset))
}

