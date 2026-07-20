//! `git merge-recursive` — the low-level recursive-strategy driver: merge
//! `<remote>` into `<head>` over zero or more explicit merge bases, updating the
//! index and the working tree in place.
//!
//! Unlike `merge-tree`, this command is a mutator. The merge itself is done by
//! the vendored `gix-merge` tree merge (three-way content merges, rename
//! detection, recursive merge-base consolidation via
//! `Repository::virtual_merge_base`); the resulting tree is then materialised
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
//!   * `Auto-merging <path>` / `CONFLICT (content|add/add): Merge conflict in
//!     <path>` on stdout, conflict markers labelled with the `<head>` and
//!     `<remote>` argument strings (or their `GITHEAD_<oid>` environment
//!     override, exactly as git's `better_branch_name` does)
//!   * exit 0 for a clean merge, 1 when conflicts remain, 128 for the fatal paths
//!   * `--no-renames`, `--find-renames`, `--find-renames=<n>`,
//!     `--rename-threshold=<n>`, `--histogram`, `--diff-algorithm=<myers|minimal|histogram>`
//!
//! Not covered, and refused rather than approximated:
//!   * `--ours` / `--theirs` / `--subtree[=<path>]` / `--renormalize` /
//!     `--no-renormalize` / `--patience` / `--diff-algorithm=patience` /
//!     `--ignore-space-change` / `--ignore-all-space` / `--ignore-space-at-eol` /
//!     `--ignore-cr-at-eol` — no equivalent knob on `gix-merge`'s tree/blob
//!     options, so honouring them would mean producing a different merge than
//!     the flag asks for
//!   * conflict classes outside the content family (rename/rename, rename/delete,
//!     modify/delete, directory/file, submodule, binary). `gix-merge` reports
//!     these as structured resolutions, not as git's message strings; rendering
//!     them would mean inventing text, so they error out *before* anything is
//!     written
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
use gix::diff::tree_with_rewrites::Change;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::apply_index_entries::RemovalMode;
use gix::merge::tree::{Conflict, Resolution, TreatAsUnresolved};

/// Verbatim `git merge-recursive` usage line (git exits 129 after printing it).
const USAGE: &str = "usage: git merge-recursive <base>... -- <head> <remote> ...\n";

/// git's `bases[21]` array holds one spare slot, so at most 20 bases are kept.
const MAX_BASES: usize = 20;

/// One informational message, carrying its own trailing newline.
struct Message {
    text: String,
}

/// Rename detection as requested on the command line.
enum Renames {
    /// git's default: detection on, threshold from config.
    Default,
    /// `--no-renames`.
    Off,
    /// `--find-renames` (no value) — detection on at the default threshold.
    On,
    /// `--find-renames=<n>` / `--rename-threshold=<n>`, as a similarity fraction.
    Threshold(f32),
}

/// `git merge-recursive [--<option>]... <base>... -- <head> <remote>`.
pub fn merge_recursive(args: &[String]) -> Result<ExitCode> {
    // git checks `argc < 4`, counting argv[0]; args[0] is the subcommand name.
    if args.len() < 4 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    let mut renames = Renames::Default;
    let mut diff_algorithm: Option<gix::diff::blob::Algorithm> = None;
    let mut base_specs: Vec<&str> = Vec::new();

    // Leading `--…` arguments are merge options; everything else is a merge
    // base, until a bare `--` ends the base list.
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(opt) = arg.strip_prefix("--") {
            if opt.is_empty() {
                break;
            }
            if !parse_merge_opt(opt, &mut renames, &mut diff_algorithm)? {
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
        eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
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

    // Tree-merge options, adjusted by the flags we honour.
    let mut plumbing_opts: gix::merge::plumbing::tree::Options = repo.tree_merge_options()?.into();
    if let Some(algorithm) = diff_algorithm {
        plumbing_opts.blob_merge.text.diff_algorithm = algorithm;
    }
    match renames {
        Renames::Default => {}
        Renames::Off => plumbing_opts.rewrites = None,
        Renames::On => plumbing_opts.rewrites = Some(gix::diff::Rewrites::default()),
        Renames::Threshold(percentage) => {
            plumbing_opts.rewrites = Some(gix::diff::Rewrites {
                percentage: Some(percentage),
                ..Default::default()
            });
        }
    }
    let tree_options: gix::merge::tree::Options = plumbing_opts.into();

    let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();
    let remote_tree = repo.find_commit(remote_id)?.tree_id()?.detach();

    // With no explicit bases git computes them itself (recursively merging
    // multiple bases); with bases given it uses exactly those.
    let mut outcome = if bases.is_empty() {
        let labels = Labels {
            ancestor: None,
            current: Some(BStr::new(label1.as_bytes())),
            other: Some(BStr::new(label2.as_bytes())),
        };
        let commit_options = gix::merge::commit::Options::from(tree_options)
            .with_allow_missing_merge_base(true);
        repo.merge_commits(head_id, remote_id, labels, commit_options)?
            .tree_merge
    } else {
        let ancestor_label = if bases.len() == 1 {
            "constructed merge base"
        } else {
            "merged common ancestors"
        };
        let base_tree = if bases.len() == 1 {
            repo.find_commit(bases[0])?.tree_id()?.detach()
        } else {
            repo.virtual_merge_base(bases.clone(), tree_options.clone())?
                .tree_id
                .detach()
        };
        let labels = Labels {
            ancestor: Some(BStr::new(ancestor_label.as_bytes())),
            current: Some(BStr::new(label1.as_bytes())),
            other: Some(BStr::new(label2.as_bytes())),
        };
        repo.merge_trees(base_tree, head_tree, remote_tree, labels, tree_options)?
    };

    // Render every message first: an unrenderable conflict class must fail
    // before a single byte of index or worktree is touched.
    let messages = render_messages(&repo, &outcome.conflicts)?;

    // Conservative precondition (documented deviation): the index must equal
    // `<head>`'s tree and the worktree must be clean.
    ensure_index_matches(&repo, &old_index, head_tree)?;
    if repo.is_dirty()? {
        bail!("your local changes would be overwritten by merge; commit or stash them first");
    }

    let how = TreatAsUnresolved::git();
    let conflicted = outcome.has_unresolved_conflicts(how);
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

/// git's `parse_merge_opt`: `Ok(true)` when the option is recognised *and*
/// implemented, `Ok(false)` when git itself would reject it (caller prints
/// `fatal: unknown option …`). Options git accepts but this port cannot honour
/// error out instead of being silently dropped.
fn parse_merge_opt(
    opt: &str,
    renames: &mut Renames,
    diff_algorithm: &mut Option<gix::diff::blob::Algorithm>,
) -> Result<bool> {
    const PORTED: &str = "ported: --no-renames, --find-renames[=<n>], --rename-threshold=<n>, --histogram, --diff-algorithm=<myers|minimal|histogram>";
    match opt {
        "no-renames" => *renames = Renames::Off,
        "find-renames" => *renames = Renames::On,
        "histogram" => *diff_algorithm = Some(gix::diff::blob::Algorithm::Histogram),
        "ours" | "theirs" | "subtree" | "renormalize" | "no-renormalize" | "patience"
        | "ignore-space-change" | "ignore-all-space" | "ignore-space-at-eol"
        | "ignore-cr-at-eol" => {
            bail!("unsupported flag \"--{opt}\" (no gix-merge equivalent; {PORTED})")
        }
        _ if opt.starts_with("subtree=") => {
            bail!("unsupported flag \"--{opt}\" (subtree shifting is not ported; {PORTED})")
        }
        _ if opt.starts_with("diff-algorithm=") => {
            let value = &opt["diff-algorithm=".len()..];
            *diff_algorithm = Some(match value {
                "myers" | "default" => gix::diff::blob::Algorithm::Myers,
                "minimal" => gix::diff::blob::Algorithm::MyersMinimal,
                "histogram" => gix::diff::blob::Algorithm::Histogram,
                "patience" => {
                    bail!("unsupported flag \"--{opt}\" (gix-merge has no patience diff; {PORTED})")
                }
                _ => return Ok(false),
            });
        }
        _ if opt.starts_with("find-renames=") || opt.starts_with("rename-threshold=") => {
            let value = &opt[opt.find('=').expect("checked above") + 1..];
            match parse_rename_score(value) {
                Some(fraction) => *renames = Renames::Threshold(fraction),
                None => return Ok(false),
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// git's `parse_rename_score`: an integer or float, optionally suffixed with
/// `%`, read as a similarity percentage. Returns the fraction in `0.0..=1.0`.
fn parse_rename_score(value: &str) -> Option<f32> {
    let digits = value.strip_suffix('%').unwrap_or(value);
    let number: f32 = digits.parse().ok()?;
    if !(0.0..=100.0).contains(&number) {
        return None;
    }
    Some(number / 100.0)
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

/// Refuse to merge unless the index is exactly `head_tree` — the state git's
/// `unpack_trees` pass is guaranteed to accept.
fn ensure_index_matches(
    repo: &gix::Repository,
    index: &gix::index::File,
    head_tree: ObjectId,
) -> Result<()> {
    let expected = repo.index_from_tree(&head_tree)?;
    let key = |file: &gix::index::File| -> Vec<(BString, ObjectId, Mode)> {
        let backing = file.path_backing();
        file.entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), e.id, e.mode))
            .collect()
    };
    if key(index) != key(&expected) {
        bail!("the index does not match <head>; staged changes are not supported by this port");
    }
    Ok(())
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
        gix::worktree::state::checkout(
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

/// Turn the structured conflict records into git's informational messages.
///
/// Only the content-merge family is rendered; git's text for those is
/// reproduced exactly. Any other resolution class — and any content merge over
/// binary data or symlinks, where git prepends a `warning:` line we cannot
/// reconstruct — errors out instead of guessing, before anything is written.
fn render_messages(repo: &gix::Repository, conflicts: &[Conflict]) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    for conflict in conflicts {
        let (ours, theirs) = conflict.changes_in_resolution();
        let path = ours.location().to_owned();
        let merged_blob = match &conflict.resolution {
            Ok(Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { merged_blob }) => {
                merged_blob
            }
            _ => bail!(
                "conflict at {path} is not a content merge; this conflict class is not ported"
            ),
        };

        for change in [ours, theirs] {
            let (mode, id) = change_state(change);
            if !mode.is_blob() {
                bail!("conflict at {path} involves a symlink or submodule; not ported");
            }
            if is_binary(repo, &id)? {
                bail!(
                    "conflict at {path} is a binary content merge; git's `warning: Cannot merge binary files` line is not ported"
                );
            }
        }

        out.push(Message {
            text: format!("Auto-merging {path}\n"),
        });
        if merged_blob.resolution == gix::merge::blob::Resolution::Conflict {
            // Both sides adding the same path is reported as `add/add`.
            let kind = if matches!(ours, Change::Addition { .. })
                && matches!(theirs, Change::Addition { .. })
            {
                "add/add"
            } else {
                "content"
            };
            out.push(Message {
                text: format!("CONFLICT ({kind}): Merge conflict in {path}\n"),
            });
        }
    }
    Ok(out)
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
fn is_binary(repo: &gix::Repository, id: &ObjectId) -> Result<bool> {
    let data = repo.find_object(*id)?.data.clone();
    let head = &data[..data.len().min(8000)];
    Ok(head.contains(&0))
}
