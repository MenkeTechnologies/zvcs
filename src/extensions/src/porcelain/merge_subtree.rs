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
//! Both halves are ported here. The subtree shift ([`shift_tree`],
//! [`shift_tree_by`], [`match_trees`], [`score_trees`], [`splice_tree`]) is
//! reimplemented directly from `match-trees.c` on gitoxide's tree
//! reader/editor. The merge itself reuses the same driver as the sibling
//! `merge-recursive` port: `Repository::merge_trees` produces the merged tree
//! and structured conflicts, which are rendered to git's `Auto-merging` /
//! `CONFLICT` message strings and written back to the index and worktree with
//! stage 1/2/3 entries.
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
//!   * `parse_merge_opt()`'s accept/reject grammar for the options the merge can
//!     honour (`--no-renames`, `--find-renames[=<n>]`, `--rename-threshold=<n>`,
//!     `--histogram`, `--diff-algorithm=<myers|minimal|histogram>`, and the
//!     subtree family `--subtree` / `--subtree=<path>`).
//!
//! Deliberate floors, refused rather than approximated (identical to the
//! `merge-recursive` port, which shares gitoxide's merge substrate):
//!   * `--ours` / `--theirs` / `--renormalize` / `--no-renormalize` /
//!     `--patience` / `--diff-algorithm=patience` / `--ignore-*` — no
//!     `gix-merge` knob, so honouring them would silently produce a different
//!     merge than the flag asks for;
//!   * conflict classes outside the content family (rename/rename,
//!     rename/delete, modify/delete, directory/file, submodule, binary):
//!     `gix-merge` reports these under a different taxonomy, so reproducing
//!     git's message text would mean inventing it — they error before anything
//!     is written;
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
use std::cmp::Ordering;
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
use gix::object::tree::{EntryKind, EntryMode};

/// Verbatim `builtin_merge_recursive_usage`, already interpolated with the
/// `merge-subtree` command name that dispatch reaches this module under.
const USAGE: &str = "usage: git merge-subtree <base>... -- <head> <remote> ...";

/// The most merge bases `cmd_merge_recursive` will hold (`ARRAY_SIZE(bases) - 1`).
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
    if args.len() < 3 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    // The positional scan. `end` ends up at the index of the `--` that stopped
    // it, or at args.len() when no `--` was seen — mirroring C's `i` shifted by
    // one for the missing argv[0].
    let mut bases: Vec<ObjectId> = Vec::new();
    let mut renames = Renames::Default;
    let mut diff_algorithm: Option<gix::diff::blob::Algorithm> = None;
    // A `-subtree` invocation defaults `o.subtree_shift` to "", i.e. Auto.
    let mut subtree_shift = SubtreeShift::Auto;
    let mut end = args.len();
    for (idx, arg) in args.iter().enumerate() {
        if let Some(opt) = arg.strip_prefix("--") {
            if opt.is_empty() {
                end = idx;
                break;
            }
            if !parse_merge_opt(opt, &mut renames, &mut diff_algorithm, &mut subtree_shift)? {
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
        if repo.config_snapshot().boolean("advice.resolveConflict") != Some(false) {
            eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
            eprintln!("hint: as appropriate to mark resolution and make a commit.");
        }
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
                _ => bail!(
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
        n => bail!(
            "merge-subtree cannot be performed: {n} explicit merge bases require a virtual \
             merge base built by recursively merging them with the subtree shift applied at \
             each level; Repository::virtual_merge_base cannot thread the shift through its \
             recursion"
        ),
    };

    // The command's reason to exist: shift both non-head trees toward head.
    let remote_shifted = shift_tree_object(&repo, head_tree, remote_tree, &subtree_shift)?;
    let base_shifted = shift_tree_object(&repo, head_tree, base_tree, &subtree_shift)?;

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

/// `shift_tree_object()`: shift `two` (the remote or ancestor tree) so it lines
/// up with `head_tree`, either automatically or by the user-supplied prefix.
fn shift_tree_object(
    repo: &gix::Repository,
    head_tree: ObjectId,
    two: ObjectId,
    shift: &SubtreeShift,
) -> Result<ObjectId> {
    match shift {
        SubtreeShift::Auto => shift_tree(repo, head_tree, two, 0),
        SubtreeShift::By(prefix) => shift_tree_by(repo, head_tree, two, prefix),
    }
}

/// `shift_tree()` from `match-trees.c`: come up with a merge between `hash1` and
/// `hash2` that keeps a tree shape similar to `hash1`. `hash2` might correspond
/// to a subtree of `hash1` (prefix it with empty directories) or cover `hash1`
/// (pick a subtree of it). Returns the shifted `hash2`.
fn shift_tree(
    repo: &gix::Repository,
    hash1: ObjectId,
    hash2: ObjectId,
    depth_limit: i32,
) -> Result<ObjectId> {
    // NEEDSWORK (git's own comment): the recursion depth is hardcoded to 2.
    let depth_limit = if depth_limit == 0 { 2 } else { depth_limit };

    let base_score = score_trees(repo, hash1, hash2)?;
    let mut add_score = base_score;
    let mut del_score = base_score;
    let mut add_prefix: Vec<u8> = Vec::new();
    let mut del_prefix: Vec<u8> = Vec::new();

    // Does one's subtree resemble two? (prefix two with fake trees to match)
    match_trees(repo, hash1, hash2, &mut add_score, &mut add_prefix, &[], depth_limit)?;
    // Does two's subtree resemble one? (pick only a subtree of two)
    match_trees(repo, hash2, hash1, &mut del_score, &mut del_prefix, &[], depth_limit)?;

    if add_score < del_score {
        // We need to pick a subtree of two.
        if del_prefix.is_empty() {
            return Ok(hash2);
        }
        return match tree_entry(repo, hash2, &del_prefix)? {
            Some((oid, _)) => Ok(oid),
            None => bail!(
                "cannot find path {} in tree {hash2}",
                del_prefix.as_bstr()
            ),
        };
    }

    if add_prefix.is_empty() {
        return Ok(hash2);
    }
    splice_tree(repo, hash1, &add_prefix, hash2)
}

/// `shift_tree_by()` from `match-trees.c`: the user says the trees are shifted
/// by `shift_prefix`; work out which side to prefix (or unprefix) and do it.
fn shift_tree_by(
    repo: &gix::Repository,
    hash1: ObjectId,
    hash2: ObjectId,
    shift_prefix: &[u8],
) -> Result<ObjectId> {
    // Can hash2 be a tree at shift_prefix in hash1, and vice versa?
    let sub1 = tree_entry(repo, hash1, shift_prefix)?
        .filter(|(_, mode)| mode.is_tree())
        .map(|(oid, _)| oid);
    let sub2 = tree_entry(repo, hash2, shift_prefix)?
        .filter(|(_, mode)| mode.is_tree())
        .map(|(oid, _)| oid);

    let mut candidate = 0u8;
    if sub1.is_some() {
        candidate |= 1;
    }
    if sub2.is_some() {
        candidate |= 2;
    }

    if candidate == 3 {
        // Both are plausible -- evaluate the score.
        let sub1_oid = sub1.expect("candidate bit 1 implies sub1");
        let sub2_oid = sub2.expect("candidate bit 2 implies sub2");
        let mut best_score = score_trees(repo, hash1, hash2)?;
        candidate = 0;
        let score = score_trees(repo, sub1_oid, hash2)?;
        if score > best_score {
            candidate = 1;
            best_score = score;
        }
        let score = score_trees(repo, sub2_oid, hash1)?;
        if score > best_score {
            candidate = 2;
        }
    }

    if candidate == 0 {
        // Neither is plausible -- do not shift.
        return Ok(hash2);
    }
    if candidate == 1 {
        // Shift tree2 down by adding shift_prefix above it to match tree1.
        return splice_tree(repo, hash1, shift_prefix, hash2);
    }
    // candidate == 2: shift tree2 up by removing shift_prefix from it.
    Ok(sub2.expect("candidate 2 implies sub2 is a tree"))
}

/// `match_trees()` from `match-trees.c`: match `hash1` itself and each of its
/// subtrees against `hash2`, keeping the best-scoring prefix. Recurses into
/// subdirectories up to `recurse_limit` levels.
fn match_trees(
    repo: &gix::Repository,
    hash1: ObjectId,
    hash2: ObjectId,
    best_score: &mut i32,
    best_match: &mut Vec<u8>,
    base: &[u8],
    recurse_limit: i32,
) -> Result<()> {
    for entry in tree_entries(repo, hash1)? {
        if !entry.is_dir {
            continue;
        }
        let score = score_trees(repo, entry.oid, hash2)?;
        if *best_score < score {
            let mut matched = base.to_vec();
            matched.extend_from_slice(&entry.name);
            *best_match = matched;
            *best_score = score;
        }
        if recurse_limit > 0 {
            let mut newbase = base.to_vec();
            newbase.extend_from_slice(&entry.name);
            newbase.push(b'/');
            match_trees(
                repo,
                entry.oid,
                hash2,
                best_score,
                best_match,
                &newbase,
                recurse_limit - 1,
            )?;
        }
    }
    Ok(())
}

/// `splice_tree()` from `match-trees.c`: tree `oid1` has a subdirectory at
/// `prefix`; produce a new tree that replaces it with `oid2`. gitoxide's tree
/// editor performs the same rewrite, creating any intermediate trees.
fn splice_tree(
    repo: &gix::Repository,
    oid1: ObjectId,
    prefix: &[u8],
    oid2: ObjectId,
) -> Result<ObjectId> {
    let mut editor = repo.edit_tree(oid1)?;
    // `upsert` splits the path on '/' itself (ToComponents for &BStr), creating
    // any intermediate trees — same as splice_tree() building the parent chain.
    editor.upsert(BStr::new(prefix), EntryKind::Tree, oid2)?;
    Ok(editor.write()?.detach())
}

/// `score_trees()` from `match-trees.c`: walk two trees in git's canonical order
/// and accumulate a similarity score. Entries missing on one side, present but
/// differing, or matching are scored per git's `score_missing`/`score_differs`/
/// `score_matches`.
fn score_trees(repo: &gix::Repository, hash1: ObjectId, hash2: ObjectId) -> Result<i32> {
    let one = tree_entries(repo, hash1)?;
    let two = tree_entries(repo, hash2)?;
    let mut i = 0usize;
    let mut j = 0usize;
    let mut score = 0i32;
    loop {
        let cmp = if i < one.len() && j < two.len() {
            base_name_compare(&one[i].name, one[i].is_dir, &two[j].name, two[j].is_dir)
        } else if i < one.len() {
            // two lacks this entry
            Ordering::Less
        } else if j < two.len() {
            // two has more entries
            Ordering::Greater
        } else {
            break;
        };
        match cmp {
            Ordering::Less => {
                score += score_missing(one[i].is_dir, one[i].is_link);
                i += 1;
            }
            Ordering::Greater => {
                score += score_missing(two[j].is_dir, two[j].is_link);
                j += 1;
            }
            Ordering::Equal => {
                if one[i].oid != two[j].oid {
                    score += score_differs(
                        one[i].is_dir,
                        one[i].is_link,
                        two[j].is_dir,
                        two[j].is_link,
                    );
                } else {
                    score += score_matches(
                        one[i].is_dir,
                        one[i].is_link,
                        two[j].is_dir,
                        two[j].is_link,
                    );
                }
                i += 1;
                j += 1;
            }
        }
    }
    Ok(score)
}

/// `score_missing()`: penalty for a path present on one side only.
fn score_missing(is_dir: bool, is_link: bool) -> i32 {
    if is_dir {
        -1000
    } else if is_link {
        -500
    } else {
        -50
    }
}

/// `score_differs()`: penalty for a path present on both sides but differing.
fn score_differs(dir1: bool, link1: bool, dir2: bool, link2: bool) -> i32 {
    if dir1 != dir2 {
        -100
    } else if link1 != link2 {
        -50
    } else {
        -5
    }
}

/// `score_matches()`: reward for a path identical on both sides.
fn score_matches(dir1: bool, link1: bool, dir2: bool, link2: bool) -> i32 {
    if dir1 != dir2 {
        -100
    } else if link1 != link2 {
        -50
    } else if dir1 {
        1000
    } else if link1 {
        500
    } else {
        250
    }
}

/// One decoded tree entry, materialised so the borrow on the tree buffer ends.
struct TreeEntry {
    name: Vec<u8>,
    is_dir: bool,
    is_link: bool,
    oid: ObjectId,
}

/// The entries of `tree` in git's canonical (stored) order. The empty tree
/// yields no entries even when it is not physically present in the object db.
fn tree_entries(repo: &gix::Repository, tree: ObjectId) -> Result<Vec<TreeEntry>> {
    if tree == ObjectId::empty_tree(repo.object_hash()) {
        return Ok(Vec::new());
    }
    let tree = repo.find_tree(tree)?;
    let decoded = tree.decode()?;
    Ok(decoded
        .entries
        .iter()
        .map(|e| TreeEntry {
            name: e.filename.to_vec(),
            is_dir: e.mode.is_tree(),
            is_link: e.mode.is_link(),
            oid: e.oid.to_owned(),
        })
        .collect())
}

/// `get_tree_entry()`: look up `path` (slash-separated) in `tree`, returning the
/// entry's object id and mode, or `None` when the path is absent.
fn tree_entry(
    repo: &gix::Repository,
    tree: ObjectId,
    path: &[u8],
) -> Result<Option<(ObjectId, EntryMode)>> {
    let tree = repo.find_tree(tree)?;
    let components: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    Ok(tree
        .lookup_entry(components)?
        .map(|e| (e.object_id(), e.mode())))
}

/// git's `base_name_compare`: compare two tree entry names, treating a directory
/// as if its name had a trailing `/`.
fn base_name_compare(name1: &[u8], dir1: bool, name2: &[u8], dir2: bool) -> Ordering {
    let len = name1.len().min(name2.len());
    match name1[..len].cmp(&name2[..len]) {
        Ordering::Equal => {}
        other => return other,
    }
    trailing_byte(name1, len, dir1).cmp(&trailing_byte(name2, len, dir2))
}

/// The byte git compares past the shared prefix: the next name byte, or `/` for
/// a directory whose name ended exactly at `len`.
fn trailing_byte(name: &[u8], len: usize, is_dir: bool) -> u8 {
    let c = name.get(len).copied().unwrap_or(0);
    if c == 0 && is_dir {
        b'/'
    } else {
        c
    }
}

/// `repo_get_oid()` as this command needs it: a full-length hex id is accepted
/// verbatim, without checking that the object exists, and anything else is a
/// revision expression. `None` is C's non-zero return.
fn resolve_object(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let hexsz = repo.object_hash().len_in_hex();
    if spec.len() == hexsz && spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        if let Ok(id) = ObjectId::from_hex(spec.as_bytes()) {
            return Some(id);
        }
    }
    repo.rev_parse_single(spec).ok().map(|id| id.detach())
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

/// `parse_merge_opt()` from `merge-recursive.c`. `Ok(true)` when the option is
/// recognised *and* implemented, `Ok(false)` when git itself would reject it
/// (caller prints `fatal: unknown option …`), and an error for options git
/// accepts but this port cannot honour without silently changing the merge.
fn parse_merge_opt(
    opt: &str,
    renames: &mut Renames,
    diff_algorithm: &mut Option<gix::diff::blob::Algorithm>,
    subtree_shift: &mut SubtreeShift,
) -> Result<bool> {
    const PORTED: &str = "ported: --subtree[=<path>], --no-renames, --find-renames[=<n>], --rename-threshold=<n>, --histogram, --diff-algorithm=<myers|minimal|histogram>";
    match opt {
        "no-renames" => *renames = Renames::Off,
        "find-renames" => *renames = Renames::On,
        "histogram" => *diff_algorithm = Some(gix::diff::blob::Algorithm::Histogram),
        // `--subtree` sets o.subtree_shift = "" -> automatic detection.
        "subtree" => *subtree_shift = SubtreeShift::Auto,
        "ours" | "theirs" | "renormalize" | "no-renormalize" | "patience"
        | "ignore-space-change" | "ignore-all-space" | "ignore-space-at-eol"
        | "ignore-cr-at-eol" => {
            bail!("unsupported flag \"--{opt}\" (no gix-merge equivalent; {PORTED})")
        }
        _ if opt.starts_with("subtree=") => {
            let value = &opt["subtree=".len()..];
            // An empty value is git's "" (automatic detection); anything else is
            // a shift prefix.
            *subtree_shift = if value.is_empty() {
                SubtreeShift::Auto
            } else {
                SubtreeShift::By(value.as_bytes().to_vec())
            };
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

/// git's `parse_rename_score`: a run of digits with at most one `.`, optionally
/// closed by a single trailing `%`, read as a similarity percentage. An empty
/// value — and a lone `.` or `%` — reads as score 0, exactly as git's scanner
/// does (`strtoul` consumes nothing and returns 0). Returns the fraction.
fn parse_rename_score(value: &str) -> Option<f32> {
    let body = value.strip_suffix('%').unwrap_or(value);
    // Reject anything git's scanner would not consume as a number: only digits
    // and at most one decimal point.
    let mut seen_dot = false;
    for c in body.chars() {
        match c {
            '.' if !seen_dot => seen_dot = true,
            '0'..='9' => {}
            _ => return None,
        }
    }
    // "", ".", "%" → 0; git reads a missing/empty number as 0.
    let number: f32 = match body {
        "" | "." => 0.0,
        _ => body.parse().ok()?,
    };
    if number < 0.0 {
        return None;
    }
    Some(number / 100.0)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(opt: &str) -> Result<(bool, Renames, Option<gix::diff::blob::Algorithm>, SubtreeShift)> {
        let mut renames = Renames::Default;
        let mut algo = None;
        let mut shift = SubtreeShift::Auto;
        let accepted = parse_merge_opt(opt, &mut renames, &mut algo, &mut shift)?;
        Ok((accepted, renames, algo, shift))
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
            let (accepted, ..) = parse(ok).expect("git accepts and this port honours it");
            assert!(accepted, "git accepts --{ok}");
        }
    }

    #[test]
    fn refuses_options_git_accepts_but_this_port_cannot_honour() {
        for floored in [
            "ours",
            "theirs",
            "patience",
            "renormalize",
            "no-renormalize",
            "ignore-space-change",
            "ignore-all-space",
            "ignore-space-at-eol",
            "ignore-cr-at-eol",
            "diff-algorithm=patience",
        ] {
            assert!(parse(floored).is_err(), "--{floored} is a floor, not a silent drop");
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
            let (accepted, ..) = parse(bad).expect("rejection is Ok(false), not an error");
            assert!(!accepted, "git rejects --{bad}");
        }
    }

    #[test]
    fn subtree_flag_selects_the_shift_mode() {
        assert!(matches!(parse("subtree").unwrap().3, SubtreeShift::Auto));
        assert!(matches!(parse("subtree=").unwrap().3, SubtreeShift::Auto));
        match parse("subtree=lib/foo").unwrap().3 {
            SubtreeShift::By(prefix) => assert_eq!(prefix, b"lib/foo"),
            SubtreeShift::Auto => panic!("--subtree=<path> must select a prefixed shift"),
        }
    }

    #[test]
    fn parse_rename_score_matches_git() {
        assert_eq!(parse_rename_score("50"), Some(0.5));
        assert_eq!(parse_rename_score("50%"), Some(0.5));
        assert_eq!(parse_rename_score("100"), Some(1.0));
        assert_eq!(parse_rename_score("0"), Some(0.0));
        // An empty/./% value reads as score 0 — verified against git 2.55.0:
        // `git merge-subtree --find-renames= …` is accepted, not "unknown option".
        assert_eq!(parse_rename_score(""), Some(0.0));
        assert_eq!(parse_rename_score("."), Some(0.0));
        assert_eq!(parse_rename_score("%"), Some(0.0));
        // git does NOT cap the score at 100 — `--find-renames=101` is accepted
        // (verified against git 2.55.0), so the fraction can exceed 1.0.
        assert_eq!(parse_rename_score("101"), Some(1.01));
        // Non-numeric values are rejected (git: "error: unknown option").
        assert_eq!(parse_rename_score("1x"), None);
        assert_eq!(parse_rename_score("bogus"), None);
    }

    #[test]
    fn base_name_compare_treats_dirs_as_slash_suffixed() {
        // A directory "a" sorts after a file "a" (git compares it as "a/").
        assert_eq!(base_name_compare(b"a", false, b"a", true), Ordering::Less);
        assert_eq!(base_name_compare(b"a", true, b"a", false), Ordering::Greater);
        assert_eq!(base_name_compare(b"a", true, b"a", true), Ordering::Equal);
        // "ab" sorts after "a" regardless of kind.
        assert_eq!(base_name_compare(b"ab", false, b"a", false), Ordering::Greater);
        assert_eq!(base_name_compare(b"a", false, b"ab", false), Ordering::Less);
    }

    #[test]
    fn tree_similarity_scores_match_git() {
        // score_missing
        assert_eq!(score_missing(true, false), -1000);
        assert_eq!(score_missing(false, true), -500);
        assert_eq!(score_missing(false, false), -50);
        // score_differs
        assert_eq!(score_differs(true, false, false, false), -100);
        assert_eq!(score_differs(false, true, false, false), -50);
        assert_eq!(score_differs(false, false, false, false), -5);
        // score_matches
        assert_eq!(score_matches(true, false, false, false), -100);
        assert_eq!(score_matches(false, true, false, false), -50);
        assert_eq!(score_matches(true, false, true, false), 1000);
        assert_eq!(score_matches(false, true, false, true), 500);
        assert_eq!(score_matches(false, false, false, false), 250);
    }
}
