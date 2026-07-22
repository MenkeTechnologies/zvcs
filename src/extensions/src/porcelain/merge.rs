//! `git merge` — fast-forward, `--no-ff` over a fast-forwardable history,
//! `--abort` and `--quit`.
//!
//! What is served natively via the vendored gitoxide crates:
//!
//! * A fast-forward merge: the ref being merged is a descendant of the current
//!   `HEAD` (their merge-base is `HEAD` itself). The branch `HEAD` points to is
//!   advanced (or `HEAD` itself on a detached head), and the clean worktree +
//!   index are moved to the new tree.
//! * `--no-ff` over that same fast-forwardable history. The merged tree is then
//!   exactly the tree of the ref being merged — when the merge-base *is* our
//!   own commit, the three-way merge of every path resolves to theirs — so the
//!   merge commit is written directly with no three-way machinery involved.
//! * A real merge of diverged histories (with or without `--no-ff`), via the
//!   shared three-way merge in [`crate::merge_apply`]: `Auto-merging`/`CONFLICT`
//!   reporting, a clean two-parent merge commit, or — on conflict —
//!   `MERGE_HEAD`/`MERGE_MSG` plus the conflicted index and worktree markers, then
//!   `Automatic merge failed; fix conflicts and then commit the result.` (exit 1).
//! * `--abort` / `--quit`: `--quit` drops the in-progress merge state files;
//!   `--abort` additionally restores the index and the merge-affected worktree
//!   paths to `HEAD`, as `git reset --merge` does.
//!
//! What is refused rather than faked:
//!
//! * An octopus merge (multiple refs).
//! * Every flag outside the set the argument loop in `merge()` accepts
//!   (`--ff`, `--no-ff`, `--ff-only`, `--stat`/`--no-stat`/`--summary`/
//!   `--no-summary`/`-n`, `-m`/`--message`, `--abort`, `--quit`).
//!
//! Known fidelity gaps, stated rather than hidden: the diffstat is computed
//! with rename detection off, while `git merge` enables it, so a merge that
//! renames a file reports it as a delete plus a create instead of a `rename`
//! summary line; and diffstat column widths measure Unicode scalar values
//! rather than terminal columns, so a path containing wide characters pads
//! differently.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::object::tree::diff::{Action, Change as TreeChange};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// The mutually exclusive top-level modes of `git merge`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Merge,
    Abort,
    Quit,
}

/// How the fast-forward question is answered, mirroring git's `fast_forward`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ff {
    Allow,
    Never,
    Only,
}

pub fn merge(args: &[String]) -> Result<ExitCode> {
    let mut op = Op::Merge;
    let mut ff = Ff::Allow;
    let mut show_stat = true;
    let mut message: Option<String> = None;
    let mut refs: Vec<&str> = Vec::new();

    // git reads merge.ff and merge.stat as the defaults; the CLI flags below
    // override them (`--ff`/`--no-ff`/`--ff-only`, `--stat`/`--no-stat`).
    // merge.suppressDest is consulted later, in `dest_suppressed`, when the
    // default merge message's title is composed.
    if let Ok(repo) = gix::discover(".") {
        let snap = repo.config_snapshot();
        match snap.string("merge.ff").map(|v| v.to_string().to_ascii_lowercase()).as_deref() {
            Some("only") => ff = Ff::Only,
            Some("false" | "no" | "off" | "0") => ff = Ff::Never,
            Some(_) => ff = Ff::Allow, // true/yes/on/1/valueless → allow
            None => {}
        }
        if snap.boolean("merge.stat") == Some(false) {
            show_stat = false;
        }
    }

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--abort" => op = Op::Abort,
            "--quit" => op = Op::Quit,
            "--ff" => ff = Ff::Allow,
            "--no-ff" => ff = Ff::Never,
            "--ff-only" => ff = Ff::Only,
            "--stat" | "--summary" => show_stat = true,
            "--no-stat" | "--no-summary" | "-n" => show_stat = false,
            "-m" | "--message" => {
                i += 1;
                match args.get(i) {
                    Some(m) => message = Some(m.clone()),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--message=") => message = Some(a["--message=".len()..].to_string()),
            _ if a.len() > 2 && a.starts_with("-m") => message = Some(a[2..].to_string()),
            _ if a.len() > 1 && a.starts_with('-') => {
                anyhow::bail!("unsupported flag {a}")
            }
            _ => refs.push(a),
        }
        i += 1;
    }

    match op {
        // git: `--abort`/`--quit` expect no arguments; `usage_msg_opt` exits 129.
        Op::Abort | Op::Quit if !refs.is_empty() => {
            let which = if op == Op::Abort { "--abort" } else { "--quit" };
            eprintln!("fatal: {which} expects no arguments");
            Ok(ExitCode::from(129))
        }
        Op::Abort => abort(),
        Op::Quit => quit(),
        Op::Merge => do_merge(&refs, ff, show_stat, message),
    }
}

// ---------------------------------------------------------------------------
// --abort / --quit
// ---------------------------------------------------------------------------

/// The state files `remove_merge_branch_state()` (branch.c) unlinks.
const MERGE_STATE_FILES: &[&str] = &["MERGE_HEAD", "MERGE_RR", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"];

/// The extra state `remove_branch_state()` unlinks on top of the merge state;
/// `git merge --abort` reaches it by running `git reset --merge`.
const BRANCH_STATE_FILES: &[&str] = &["SQUASH_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD"];

fn remove_merge_state(git_dir: &Path, and_branch_state: bool) {
    for name in MERGE_STATE_FILES {
        let _ = std::fs::remove_file(git_dir.join(name));
    }
    if and_branch_state {
        for name in BRANCH_STATE_FILES {
            let _ = std::fs::remove_file(git_dir.join(name));
        }
        let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
    }
}

/// `git merge --quit`: forget the in-progress merge, leaving index and worktree
/// exactly as they are.
fn quit() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    remove_merge_state(repo.git_dir(), false);
    Ok(ExitCode::SUCCESS)
}

/// `git merge --abort`: `git reset --merge` plus dropping the merge state.
///
/// The reset is confined to the paths the merge touched — every path that has a
/// conflicted stage, or whose index entry disagrees with `HEAD` — so unrelated
/// local modifications and untracked files survive, as they do under git.
fn abort() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    if !repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: There is no merge to abort (MERGE_HEAD missing).");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let head = repo.head()?;
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    let head_tree = repo.find_object(head_id)?.peel_to_tree()?.id;

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);
    update_worktree(&repo, &old_index, head_tree, &should_interrupt)?;

    // git's `reset_refs()` records the pre-reset HEAD in ORIG_HEAD.
    set_orig_head(&repo, head_id)?;
    remove_merge_state(repo.git_dir(), true);

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

fn do_merge(
    refs: &[&str],
    ff: Ff,
    show_stat: bool,
    message: Option<String>,
) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    if repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: You have not concluded your merge (MERGE_HEAD exists).");
        eprintln!("Please, commit your changes before you merge.");
        return Ok(ExitCode::from(128));
    }

    if refs.is_empty() {
        // git dies here rather than defaulting to anything.
        eprintln!("fatal: No remote for the current branch.");
        return Ok(ExitCode::from(128));
    }
    if refs.len() > 1 {
        anyhow::bail!("octopus merge (multiple refs) is not supported");
    }
    let spec = refs[0];

    // Current HEAD state. An unborn branch has no commit to fast-forward from;
    // a real merge into it would be a checkout, which is out of scope.
    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot merge into an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    // Owned branch name when attached; `None` when detached.
    let branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);

    // Resolve the ref to merge and peel it to a commit (tags included).
    let target_id = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?.id;

    let base = repo.merge_base(local_id, target_id)?.detach();
    if base == target_id {
        // Target already reachable from HEAD (or identical). git checks this
        // before it consults --no-ff, so --no-ff does not force a commit here.
        println!("Already up to date.");
        return Ok(ExitCode::SUCCESS);
    }
    let diverged = base != local_id;
    if diverged && ff == Ff::Only {
        eprintln!("fatal: Not possible to fast-forward, aborting.");
        return Ok(ExitCode::from(128));
    }

    // From here on we mutate a ref, the index and the worktree. Serialize the
    // whole read-modify-write through the repo coordinator (a no-op if no
    // daemon is running), matching the zsync/zbump write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Never clobber uncommitted work.
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to merge");
    }

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let head_tree = repo.find_object(local_id)?.peel_to_tree()?.id;
    let target_tree = repo.find_object(target_id)?.peel_to_tree()?.id;
    let should_interrupt = AtomicBool::new(false);

    // The ref to move: the attached branch, or HEAD itself when detached. Both
    // are direct (non-symbolic) refs here, so `deref` is false either way.
    let name: FullName = match &branch {
        Some(b) => b.clone(),
        None => "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
    };

    // Diverged histories: a genuine three-way merge (`ort` strategy) of HEAD and
    // the target against their merge base. On a clean merge we write the two-parent
    // merge commit; on conflict we record MERGE_HEAD/MERGE_MSG and stop, exactly as
    // git does, leaving the conflicted index and worktree for the user to resolve.
    if diverged {
        let base_tree = repo.find_object(base)?.peel_to_tree()?.id;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(b"merged common ancestors")),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(spec.as_bytes())),
        };
        let applied = crate::merge_apply::three_way_merge(
            &repo,
            base_tree,
            head_tree,
            target_tree,
            &old_index,
            labels,
            &should_interrupt,
        )?;
        let mut index = applied.index;
        index.write(Default::default())?;
        set_orig_head(&repo, local_id)?;

        if applied.conflicts.is_empty() {
            let msg = merge_message(&repo, spec, branch.as_ref(), message)?;
            let author = repo
                .author()
                .ok_or_else(|| anyhow::anyhow!("author identity is not configured"))??;
            let committer = repo
                .committer()
                .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
            let commit = gix::objs::Commit {
                message: msg.into(),
                tree: applied.tree_id,
                author: author.to_owned()?,
                committer: committer.to_owned()?,
                encoding: None,
                parents: [local_id, target_id].into_iter().collect(),
                extra_headers: Default::default(),
            };
            let new_id = repo.write_object(&commit)?.detach();
            advance(
                &repo,
                name,
                local_id,
                new_id,
                format!("merge {spec}: Merge made by the 'ort' strategy."),
            )?;
            println!("Merge made by the 'ort' strategy.");
            if show_stat {
                print!("{}", diffstat(&repo, head_tree, applied.tree_id)?);
            }
            return Ok(ExitCode::SUCCESS);
        }

        // Conflicts: record the in-progress merge and stop with git's message.
        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("MERGE_HEAD"), format!("{target_id}\n"))?;
        std::fs::write(git_dir.join("MERGE_MODE"), b"")?;
        let mut merge_msg = merge_message(&repo, spec, branch.as_ref(), message)?.into_bytes();
        merge_msg.extend_from_slice(b"\n# Conflicts:\n");
        for path in &applied.conflicts {
            merge_msg.extend_from_slice(b"#\t");
            merge_msg.extend_from_slice(&path[..]);
            merge_msg.push(b'\n');
        }
        std::fs::write(git_dir.join("MERGE_MSG"), &merge_msg)?;
        println!("Automatic merge failed; fix conflicts and then commit the result.");
        return Ok(ExitCode::from(1));
    }

    if ff == Ff::Never {
        // The merge-base is our own commit, so a three-way merge of every path
        // resolves to theirs: the merged tree is exactly the target's tree.
        let msg = merge_message(&repo, spec, branch.as_ref(), message)?;
        let author = repo
            .author()
            .ok_or_else(|| anyhow::anyhow!("author identity is not configured"))??;
        let committer = repo
            .committer()
            .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
        let commit = gix::objs::Commit {
            message: msg.into(),
            tree: target_tree,
            author: author.to_owned()?,
            committer: committer.to_owned()?,
            encoding: None,
            parents: [local_id, target_id].into_iter().collect(),
            extra_headers: Default::default(),
        };
        let new_id = repo.write_object(&commit)?.detach();

        set_orig_head(&repo, local_id)?;
        advance(&repo, name, local_id, new_id, format!("merge {spec}: Merge made by the 'ort' strategy."))?;
        update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;

        println!("Merge made by the 'ort' strategy.");
    } else {
        set_orig_head(&repo, local_id)?;
        advance(&repo, name, local_id, target_id, format!("merge {spec}: Fast-forward"))?;
        update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;

        println!(
            "Updating {}..{}",
            local_id.to_hex_with_len(7),
            target_id.to_hex_with_len(7)
        );
        println!("Fast-forward");
    }

    if show_stat {
        print!("{}", diffstat(&repo, head_tree, target_tree)?);
    }
    Ok(ExitCode::SUCCESS)
}

/// Move `name` from `old` to `new`, writing `reflog` as the reflog message.
fn advance(
    repo: &gix::Repository,
    name: FullName,
    old: ObjectId,
    new: ObjectId,
    reflog: String,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: reflog.into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// Point `ORIG_HEAD` at `id`, as git does before it moves `HEAD`.
fn set_orig_head(repo: &gix::Repository, id: ObjectId) -> Result<()> {
    let name: FullName = "ORIG_HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name ORIG_HEAD: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "updating ORIG_HEAD".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// The merge commit's message.
///
/// Port of `merge_name()` (builtin/merge.c) feeding `fmt_merge_msg_title()`
/// (fmt-merge-msg.c): the ref is described by the category it resolved into,
/// and ` into <branch>` is appended unless the current branch matches a
/// `merge.suppressDest` glob (defaulting to `main`/`master`), see
/// `dest_suppressed`.
fn merge_message(
    repo: &gix::Repository,
    spec: &str,
    branch: Option<&FullName>,
    explicit: Option<String>,
) -> Result<String> {
    if let Some(mut m) = explicit {
        if !m.ends_with('\n') {
            m.push('\n');
        }
        return Ok(m);
    }

    // gix resolves a partial name through the same rule list git's `dwim_ref`
    // uses ("", tags, heads, remotes), so the full name it lands on is the
    // category git would have reported. An invalid ref name (`main~2`) is not
    // an error here, it just means no ref matched.
    let described = match repo.try_find_reference(spec) {
        Ok(Some(r)) => {
            let full = r.name().as_bstr().to_str_lossy().into_owned();
            if full.starts_with("refs/heads/") {
                format!("branch '{spec}'")
            } else if full.starts_with("refs/tags/") {
                format!("tag '{spec}'")
            } else if full.starts_with("refs/remotes/") {
                format!("remote-tracking branch '{spec}'")
            } else {
                format!("commit '{spec}'")
            }
        }
        _ => match early_part_of_branch(repo, spec) {
            Some(d) => d,
            None => format!("commit '{spec}'"),
        },
    };

    let current = match branch {
        Some(b) => b.shorten().to_str_lossy().into_owned(),
        None => "HEAD".to_string(),
    };
    let mut out = format!("Merge {described}");
    if !dest_suppressed(repo, &current) {
        out.push_str(&format!(" into {current}"));
    }
    out.push('\n');
    Ok(out)
}

/// Port of `dest_suppressed()` and the default seeding in `fmt_merge_msg()`
/// (fmt-merge-msg.c): the merge title's ` into <branch>` is dropped when the
/// current branch matches any glob in `merge.suppressDest`, tested with
/// `wildmatch(pattern, branch, WM_PATHNAME)` — case-sensitive, and `*` does not
/// cross a `/`. The variable is multi-valued and accumulates in config order;
/// an empty value clears whatever was gathered so far. When the key is never
/// set at all, the list defaults to `main` then `master`.
fn dest_suppressed(repo: &gix::Repository, branch: &str) -> bool {
    let patterns = suppress_dest_patterns(repo);
    let value = branch.as_bytes().as_bstr();
    patterns
        .iter()
        .any(|p| gix::glob::wildmatch(p.as_bstr(), value, gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL))
}

/// The accumulated `merge.suppressDest` pattern list, resolving git's
/// empty-value-clears rule and its `main`/`master` default when unset.
///
/// Fidelity gap: a *valueless* `merge.suppressDest` (no `=`) makes git die with
/// `config_error_nonbool` at config-parse time; gix reports it as an empty
/// value, indistinguishable from `suppressDest=`, so here it clears the list
/// rather than aborting. This is a config-subsystem limitation shared across
/// keys, not specific to the merge logic.
fn suppress_dest_patterns(repo: &gix::Repository) -> Vec<BString> {
    match repo.config_snapshot().raw_values("merge.suppressDest") {
        Ok(values) => {
            let mut list: Vec<BString> = Vec::new();
            for v in values {
                if v.is_empty() {
                    list.clear();
                } else {
                    list.push(v);
                }
            }
            list
        }
        // `suppress_dest_pattern_seen` never set → the built-in default.
        Err(_) => vec![BString::from("main"), BString::from("master")],
    }
}

/// `merge_name()`'s second attempt: `<name>^^^` or `<name>~<number>` naming a
/// point inside an existing branch. The suffix is stripped and, if a branch by
/// the remaining name exists, that branch is what git reports — tagged
/// `(early part)` whenever the suffix actually walks back at least one commit.
fn early_part_of_branch(repo: &gix::Repository, spec: &str) -> Option<String> {
    let bytes = spec.as_bytes();
    let mut len = 0usize;
    let mut early = false;

    let carets = bytes.iter().rev().take_while(|&&b| b == b'^').count();
    if carets > 0 && carets < bytes.len() {
        len = carets;
        early = true;
    } else if carets == 0 {
        if let Some(tilde) = spec.rfind('~') {
            let digits = &bytes[tilde + 1..];
            if digits.iter().all(u8::is_ascii_digit) {
                len = 1 + digits.len();
                // "name~" means "name~1"; "name~0" walks back nothing.
                early = digits.is_empty() || digits.iter().any(|&b| b != b'0');
            }
        }
    }

    if len == 0 || len >= bytes.len() {
        return None;
    }
    let stripped = &spec[..bytes.len() - len];
    match repo.try_find_reference(format!("refs/heads/{stripped}").as_str()) {
        Ok(Some(_)) => Some(format!(
            "branch '{stripped}'{}",
            if early { " (early part)" } else { "" }
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Worktree + index transition
// ---------------------------------------------------------------------------

/// Move the worktree and its index from the state captured in `old` to
/// `new_tree`, writing only the paths that changed.
///
/// Ported from the `zsync` reconcile path: the change set is derived by
/// comparing the old index against the new tree-index (file-level granularity),
/// added/modified files are checked out via `gix-worktree-state`, removed files
/// are deleted, and the new index is written reusing prior stats for unchanged
/// entries so a later status stays cheap.
///
/// A path carrying any conflicted stage in `old` is always treated as changed:
/// its worktree file holds conflict markers rather than any indexed blob, so it
/// must be rewritten even when one of its stages happens to match the new tree.
fn update_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_tree: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Index the current entries by path for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    let mut conflicted: HashSet<BString> = HashSet::new();
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing).to_owned();
            if e.stage_raw() != 0 {
                conflicted.insert(path.clone());
            }
            old_map.insert(path, (e.id, e.mode, e.stat));
        }
    }

    // Full target index (all new-tree entries) — what is finally written; a
    // reduced copy of only the changed entries is what is checked out.
    let mut new_index = repo.index_from_tree(&new_tree)?;
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| {
        let path = path.to_owned();
        if conflicted.contains(&path) {
            return false;
        }
        match old_map.get(&path) {
            // Present before with identical content and mode → unchanged, drop it.
            Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
            // Absent before → an addition, keep it.
            None => false,
        }
    });

    // Write the changed files into the worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files present before but not in the new tree.
    let new_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if !new_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
    }

    // Fresh stats produced by the checkout for the changed entries.
    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Changed entries get their fresh stat; unchanged entries reuse the old one.
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode && !conflicted.contains(&path) {
                    e.stat = *stat;
                }
            }
        }
    }

    // Drop any stale cache-tree extension before persisting.
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Diffstat and summary (diff.c)
// ---------------------------------------------------------------------------

/// One diffstat row.
struct StatRow {
    /// Quoted path, as git's `fill_print_name` produces it.
    name: String,
    /// Inserted lines, or the new blob's byte size when `binary`.
    added: u64,
    /// Deleted lines, or the old blob's byte size when `binary`.
    deleted: u64,
    binary: bool,
}

/// git's `decimal_width`.
fn decimal_width(mut n: u64) -> i64 {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// git's `scale_linear`: at least one column for any non-zero change.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// Display width in Unicode scalar values (git measures terminal columns; wide
/// characters are counted as 1 here, see the module note).
fn display_width(s: &str) -> i64 {
    s.chars().count() as i64
}

/// git's `quote_c_style` as applied to diff path names.
fn quote_path(path: &[u8]) -> String {
    let needs = path
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(path).into_owned();
    }
    let mut out = String::from("\"");
    for &b in path {
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
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// The `DIFF_FORMAT_DIFFSTAT | DIFF_FORMAT_SUMMARY` block `finish()`
/// (builtin/merge.c) prints after a merge, rendered as one string.
fn diffstat(repo: &gix::Repository, old_tree: ObjectId, new_tree: ObjectId) -> Result<String> {
    let (rows, summary) = collect(repo, old_tree, new_tree)?;
    let mut out = String::new();
    emit_stats(&mut out, &rows);
    for line in &summary {
        out.push_str(&format!(" {line}\n"));
    }
    Ok(out)
}

/// Walk the tree-to-tree diff once, producing the stat rows and the summary
/// lines, both ordered by path as git's tree recursion orders them.
fn collect(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
) -> Result<(Vec<StatRow>, Vec<String>)> {
    let old = repo.find_tree(old_tree)?;
    let new = repo.find_tree(new_tree)?;
    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

    // Per row: path (for ordering), display name, line counts, and — when the
    // blob diff declined because a side is binary — the ids whose sizes git
    // reports instead. Sizes are looked up after the walk so the callback stays
    // infallible.
    let mut raw: Vec<(BString, String, Option<(u64, u64)>, Option<ObjectId>, Option<ObjectId>)> =
        Vec::new();
    let mut summary: Vec<(BString, String)> = Vec::new();

    let mut platform = old.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let _rewrites = platform.for_each_to_obtain_tree(&new, |change| {
        let path: BString = change.location().to_owned();
        let display = quote_path(&path[..]);
        let (old_id, new_id) = match change {
            TreeChange::Addition { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("create mode {:06o} {display}", entry_mode.value()),
                ));
                (None, Some(id.detach()))
            }
            TreeChange::Deletion { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("delete mode {:06o} {display}", entry_mode.value()),
                ));
                (Some(id.detach()), None)
            }
            TreeChange::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => {
                if previous_entry_mode.value() != entry_mode.value() {
                    summary.push((
                        path.clone(),
                        format!(
                            "mode change {:06o} => {:06o} {display}",
                            previous_entry_mode.value(),
                            entry_mode.value()
                        ),
                    ));
                }
                (Some(previous_id.detach()), Some(id.detach()))
            }
            // Rewrites cannot occur: rename tracking is off above.
            TreeChange::Rewrite { source_id, id, .. } => (Some(source_id.detach()), Some(id.detach())),
        };

        let counts = change
            .diff(&mut resource_cache)
            .ok()
            .and_then(|mut p| p.line_counts().ok())
            .flatten()
            .map(|c| (u64::from(c.insertions), u64::from(c.removals)));
        raw.push((path, display, counts, old_id, new_id));

        resource_cache.clear_resource_cache_keep_allocation();
        Ok::<_, std::convert::Infallible>(Action::Continue(()))
    })?;
    drop(platform);

    let blob_size = |id: Option<ObjectId>| -> Result<u64> {
        match id {
            // git's `diff_filespec_size` of an invalid filespec is 0.
            None => Ok(0),
            Some(id) => Ok(repo.find_object(id)?.data.len() as u64),
        }
    };

    let mut rows: Vec<(BString, StatRow)> = Vec::with_capacity(raw.len());
    for (path, name, counts, old_id, new_id) in raw {
        let row = match counts {
            Some((added, deleted)) => StatRow { name, added, deleted, binary: false },
            None => StatRow {
                name,
                added: blob_size(new_id)?,
                deleted: blob_size(old_id)?,
                binary: true,
            },
        };
        rows.push((path, row));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    summary.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((
        rows.into_iter().map(|(_, r)| r).collect(),
        summary.into_iter().map(|(_, l)| l).collect(),
    ))
}

/// Port of `show_stats()` (diff.c) at merge's `stat_width = -1`, which resolves
/// to `term_columns()` — 80 whenever stdout is not a terminal and `COLUMNS` is
/// unset, as it is under the parity harness. Followed by
/// `print_stat_summary_inserts_deletes()`.
fn emit_stats(out: &mut String, files: &[StatRow]) {
    if files.is_empty() {
        return;
    }

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    for f in files {
        max_len = max_len.max(display_width(&f.name));
        if f.binary {
            // "Bin XXX -> YYY bytes"
            bin_width = bin_width.max(14 + decimal_width(f.added) + decimal_width(f.deleted));
            // Display change counts aligned with "Bin".
            number_width = 3;
            continue;
        }
        max_change = max_change.max((f.added + f.deleted) as i64);
    }

    let mut width: i64 = 80;
    number_width = number_width.max(decimal_width(max_change as u64));

    // Guarantee 3/8*16==6 for the graph part and 5/8*16==10 for the filename.
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width { max_change } else { bin_width - 4 };
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for f in files {
        // Scale the filename: elide the head, then resume at a path separator.
        let mut len = name_width;
        let mut prefix = "";
        let mut name: &str = &f.name;
        if name_width < display_width(name) {
            prefix = "...";
            len -= 3;
            if len < 0 {
                len = 0;
            }
            let mut name_len = display_width(name);
            let mut off = 0;
            while name_len > len && off < name.len() {
                let c = name[off..]
                    .chars()
                    .next()
                    .expect("off stays on a char boundary");
                off += c.len_utf8();
                name_len -= 1;
            }
            name = &name[off..];
            if let Some(slash) = name.find('/') {
                name = &name[slash..];
            }
        }
        let padding = (len - display_width(name)).max(0) as usize;
        let nw = number_width as usize;

        if f.binary {
            out.push_str(&format!(" {prefix}{name}{:padding$} | {:>nw$}", "", "Bin"));
            if f.added == 0 && f.deleted == 0 {
                out.push('\n');
            } else {
                out.push_str(&format!(" {} -> {} bytes\n", f.deleted, f.added));
            }
            continue;
        }

        let total = f.added + f.deleted;
        let mut add = f.added as i64;
        let mut del = f.deleted as i64;
        if graph_width <= max_change && max_change > 0 {
            let mut sum = scale_linear(add + del, graph_width, max_change);
            if sum < 2 && add > 0 && del > 0 {
                sum = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = sum - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = sum - del;
            }
        }

        out.push_str(&format!(
            " {prefix}{name}{:padding$} | {:>nw$}{}",
            "",
            total,
            if total > 0 { " " } else { "" },
        ));
        for _ in 0..add.max(0) {
            out.push('+');
        }
        for _ in 0..del.max(0) {
            out.push('-');
        }
        out.push('\n');
    }

    // Binary rows count as changed files but contribute no insertions/deletions.
    let mut adds: u64 = 0;
    let mut dels: u64 = 0;
    for f in files {
        if !f.binary {
            adds += f.added;
            dels += f.deleted;
        }
    }

    let n = files.len();
    let mut line = format!(" {n} {} changed", if n == 1 { "file" } else { "files" });
    if adds > 0 || dels == 0 {
        line.push_str(&format!(
            ", {adds} {}",
            if adds == 1 { "insertion(+)" } else { "insertions(+)" }
        ));
    }
    if dels > 0 || adds == 0 {
        line.push_str(&format!(
            ", {dels} {}",
            if dels == 1 { "deletion(-)" } else { "deletions(-)" }
        ));
    }
    out.push_str(&line);
    out.push('\n');
}
