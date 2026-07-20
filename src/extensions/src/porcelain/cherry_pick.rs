//! `git cherry-pick <commit>...` — replay the change each commit introduces on
//! top of `HEAD`, recording a new commit for each.
//!
//! ## What is served
//!
//! Each pick is a three-way tree merge with *base* = the picked commit's first
//! parent, *ours* = the current `HEAD` tree and *theirs* = the picked commit's
//! tree. This port resolves the merge at **file granularity only**: a path is
//! taken from *theirs* when *ours* still matches *base*, kept from *ours* when
//! *theirs* matches *base*, and kept when both sides made the identical change.
//!
//! When a path was changed differently on both sides, a *content* (hunk-level)
//! merge would be required. The vendored `gix-merge` blob/tree merge is not
//! compiled into this binary (the `merge` feature of the vendored `gix` crate is
//! off), so that case `bail!`s with the path named rather than producing a wrong
//! tree. The same applies to conflicted picks: no `CHERRY_PICK_HEAD`,
//! `.git/sequencer` state or conflict markers are ever written, so
//! `--continue` / `--skip` / `--abort` / `--quit` are refused as well.
//!
//! Stdout for a successful pick is byte-identical to stock git: the
//! `[<branch> <abbrev>] <subject>` summary, the optional ` Author:` line (only
//! when the picked author differs from the configured committer, matching
//! `print_commit_summary`), the always-present ` Date:` line carrying the
//! preserved author date, and git's short-stat plus create/delete/mode-change
//! block. `--ff` prints nothing, exactly like git.
//!
//! Repository state matches too: the author signature (name, email and time) is
//! preserved from the picked commit, the committer comes from configuration, and
//! the reflog entry is `cherry-pick: <subject>` (or `cherry-pick: fast-forward`).
//! Mailmap is not applied to the printed identities, so a repository that
//! rewrites the picked author via `.mailmap` would see a different ` Author:`.
//!
//! ## Supported flags
//!
//! `-x`, `--ff`, `--allow-empty`, `--allow-empty-message`, `--no-edit`, `-r`
//! (a documented no-op in git), `--no-gpg-sign` (no-op here as we never sign).
//! Everything else — `-e`, `-n`, `-s`, `-m`, `-S`, `--strategy`, `-X`,
//! `--cleanup`, `--empty`, `--keep-redundant-commits`, the sequencer verbs and
//! commit *ranges* — is refused with a precise message.
//!
//! The worktree-update helper below is a verbatim port of the one in
//! `porcelain::merge`; it cannot be shared because that module is private and
//! this port may only add a single file.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::objs::tree::EntryMode;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

pub fn cherry_pick(args: &[String]) -> Result<ExitCode> {
    // --- argument parsing ------------------------------------------------
    let mut specs: Vec<&str> = Vec::new();
    let mut record_origin = false;
    let mut allow_ff = false;
    let mut allow_empty = false;
    let mut allow_empty_message = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-x" => record_origin = true,
            "--ff" => allow_ff = true,
            "--allow-empty" => allow_empty = true,
            "--allow-empty-message" => allow_empty_message = true,
            // git documents `-r` as a no-op retained for compatibility, and we
            // never sign, so `--no-gpg-sign` is satisfied by construction.
            "-r" | "--no-gpg-sign" | "--no-edit" => {}
            "-e" | "--edit" => anyhow::bail!("`-e`/`--edit` (editor mode) is not supported"),
            "-n" | "--no-commit" => {
                anyhow::bail!("`-n`/`--no-commit` is not supported; each pick is committed")
            }
            "-s" | "--signoff" => {
                anyhow::bail!("`-s`/`--signoff` is not supported (trailer placement is unported)")
            }
            "-m" | "--mainline" => {
                anyhow::bail!("`-m`/`--mainline` (picking a merge commit) is not supported")
            }
            "--continue" | "--skip" | "--abort" | "--quit" => {
                anyhow::bail!("sequencer state (.git/sequencer, CHERRY_PICK_HEAD) is not implemented, so `{a}` is unavailable")
            }
            "--" => {}
            s if s.starts_with("--mainline=") => {
                anyhow::bail!("`--mainline` (picking a merge commit) is not supported")
            }
            s if s.starts_with("--strategy") || s.starts_with("-X") => {
                anyhow::bail!("merge strategies are not supported; only trivially-resolvable picks are served")
            }
            s if s.starts_with("--cleanup") => anyhow::bail!("`--cleanup` is not supported"),
            s if s.starts_with("--empty") || s == "--keep-redundant-commits" => {
                anyhow::bail!("`--empty`/`--keep-redundant-commits` is not supported")
            }
            s if s.starts_with("-S") || s.starts_with("--gpg-sign") => {
                anyhow::bail!("commit signing is not supported")
            }
            s if s.starts_with('-') => anyhow::bail!("unsupported option `{s}`"),
            s if s.contains("..") => {
                anyhow::bail!("commit ranges are not supported; name each commit individually")
            }
            s => specs.push(s),
        }
        i += 1;
    }

    if specs.is_empty() {
        anyhow::bail!("no commit specified");
    }

    // --- repository + serialized read-modify-write -----------------------
    let repo = gix::discover(".")?;
    // The whole sequence (tree build, commit, HEAD move, worktree update) is one
    // logical write; hold the coordinator lock across all of it, like `merge`.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let hash = repo.object_hash();

    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot cherry-pick onto an unborn branch");
    }
    let mut head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    drop(head);

    // git requires a clean worktree; refuse rather than clobber uncommitted work.
    if repo.is_dirty()? {
        anyhow::bail!("your local changes would be overwritten by cherry-pick");
    }

    // Committer identity, shared by every pick in the sequence.
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let committer_ident = format!("{} <{}>", committer.name, committer.email);

    let should_interrupt = AtomicBool::new(false);
    // Index mirroring the current (clean) worktree; carried forward across picks
    // so filesystem stats are reused and a later `status` stays cheap.
    let mut index = repo.index_or_load_from_head()?.into_owned();

    for spec in specs {
        let pick = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?;
        let pick_id = pick.id;

        let parents: Vec<ObjectId> = pick.parent_ids().map(|id| id.detach()).collect();
        match parents.len() {
            0 => anyhow::bail!("cannot cherry-pick the root commit {spec}"),
            1 => {}
            _ => anyhow::bail!("commit {spec} is a merge but no -m option was given"),
        }
        let base_id = parents[0];

        let pick_tree = pick.tree_id()?.detach();
        let base_tree = repo.find_commit(base_id)?.tree_id()?.detach();
        let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();

        // `--ff`: HEAD is exactly the picked commit's parent, so replaying the
        // change is a fast-forward. git prints nothing in this case.
        if allow_ff && head_id == base_id {
            advance_head(&repo, head_id, pick_id, "cherry-pick: fast-forward".into())?;
            index = update_clean_worktree(&repo, &index, pick_id, &should_interrupt)?;
            head_id = pick_id;
            continue;
        }

        // --- three-way tree merge, file granularity ----------------------
        let base = flatten(&repo, base_tree)?;
        let ours = flatten(&repo, head_tree)?;
        let theirs = flatten(&repo, pick_tree)?;

        let mut paths: Vec<&BString> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
        paths.sort_unstable();
        paths.dedup();

        let mut resolved: Vec<(BString, EntryMode, ObjectId)> = Vec::with_capacity(paths.len());
        for path in paths {
            let (b, o, t) = (base.get(path), ours.get(path), theirs.get(path));
            let keep = if t == b {
                // The pick did not touch this path — our side stands.
                o
            } else if o == b {
                // We did not touch it — take the pick's version.
                t
            } else if o == t {
                // Both sides made the identical change.
                o
            } else {
                anyhow::bail!(
                    "cherry-picking {} needs a three-way content merge for {path}; the vendored gix-merge blob merge is not compiled in (the `merge` feature is off), so only trivially-resolvable picks are served",
                    pick_id.to_hex_with_len(7)
                );
            };
            if let Some((mode, id)) = keep {
                resolved.push((path.clone(), *mode, *id));
            }
        }

        // Build the merged tree with the plumbing editor, exactly like `commit`
        // builds a tree from the index.
        let mut editor = gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, hash);
        for (path, mode, id) in &resolved {
            editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), mode.kind(), *id)?;
        }
        let tree_id = editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;

        // --- empty-result guards -----------------------------------------
        // git distinguishes a commit that was empty to begin with (allowed with
        // --allow-empty) from one that *became* empty (needs --empty=keep, which
        // requires sequencer state we do not implement).
        if tree_id == head_tree {
            if pick_tree == base_tree {
                if !allow_empty {
                    anyhow::bail!(
                        "commit {} is empty; use --allow-empty to keep it",
                        pick_id.to_hex_with_len(7)
                    );
                }
            } else {
                anyhow::bail!(
                    "cherry-picking {} produces an empty commit; sequencer state (CHERRY_PICK_HEAD, .git/sequencer) is not implemented",
                    pick_id.to_hex_with_len(7)
                );
            }
        }

        // --- message -----------------------------------------------------
        let mut message: BString = pick.message_raw()?.to_owned();
        if message.trim().is_empty() && !allow_empty_message {
            anyhow::bail!("the commit message of {spec} is empty (use --allow-empty-message)");
        }
        if message.last() != Some(&b'\n') {
            message.push(b'\n');
        }
        if record_origin {
            if !has_conforming_footer(&message) {
                message.push(b'\n');
            }
            message.extend_from_slice(b"(cherry picked from commit ");
            message.extend_from_slice(pick_id.to_string().as_bytes());
            message.extend_from_slice(b")\n");
        }
        let subject = gix::objs::commit::MessageRef::from_bytes(message.as_bstr())
            .summary()
            .to_str_lossy()
            .into_owned();

        // --- write the commit, preserving the original author -------------
        let author = pick.author()?;
        let author_ident = format!("{} <{}>", author.name, author.email);
        let author_time = author.time()?;

        let commit = gix::objs::Commit {
            message: message.clone(),
            tree: tree_id,
            author: author.to_owned()?,
            committer: committer.to_owned()?,
            encoding: None,
            parents: std::iter::once(head_id).collect(),
            extra_headers: Default::default(),
        };
        let new_id = repo.write_object(&commit)?.detach();

        let reflog = gix::reference::log::message("cherry-pick", message.as_bstr(), 1);
        advance_head(&repo, head_id, new_id, reflog)?;
        index = update_clean_worktree(&repo, &index, new_id, &should_interrupt)?;

        // --- summary, matching git's `print_commit_summary` ----------------
        let branch_label = match repo.head_name()? {
            Some(name) => name.shorten().to_string(),
            None => "detached HEAD".to_string(),
        };
        println!("[{branch_label} {}] {subject}", new_id.attach(&repo).shorten_or_id());
        if author_ident != committer_ident {
            println!(" Author: {author_ident}");
        }
        {
            use gix::date::time::format;
            println!(" Date: {}", author_time.format_or_unix(format::DEFAULT));
        }
        print_diffstat(&repo, head_tree, tree_id, &resolved)?;

        head_id = new_id;
    }

    Ok(ExitCode::SUCCESS)
}

/// Flatten a tree into `path -> (mode, blob id)`, recursively, dropping entries
/// whose mode has no tree representation (which cannot occur for a real tree).
fn flatten(
    repo: &gix::Repository,
    tree: ObjectId,
) -> Result<HashMap<BString, (EntryMode, ObjectId)>> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    let mut out = HashMap::with_capacity(index.entries().len());
    for e in index.entries() {
        if let Some(mode) = e.mode.to_tree_entry_mode() {
            out.insert(e.path_in(backing).to_owned(), (mode, e.id));
        }
    }
    Ok(out)
}

/// Point `HEAD` (writing through to its branch when attached) at `new`, with
/// `reflog` as the reflog message, requiring `old` to still be the current tip.
fn advance_head(
    repo: &gix::Repository,
    old: ObjectId,
    new: ObjectId,
    reflog: BString,
) -> Result<()> {
    let name: FullName = "HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: reflog,
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name,
        deref: true,
    })?;
    Ok(())
}

/// git's short-stat plus the create/delete/mode-change block, for the diff from
/// `old_tree` to `new_tree`. Ported from `porcelain::commit`, which derives the
/// file count from the flattened entry sets (so a rename counts as a delete plus
/// a create) and the line counts from a rename-free tree diff.
fn print_diffstat(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    new_entries: &[(BString, EntryMode, ObjectId)],
) -> Result<()> {
    let old_entries = flatten(repo, old_tree)?;
    let new_paths: HashSet<&BString> = new_entries.iter().map(|(p, _, _)| p).collect();

    let mut files_changed: u64 = 0;
    let mut summary: Vec<(BString, String)> = Vec::new();
    for (path, mode, id) in new_entries {
        match old_entries.get(path) {
            None => {
                files_changed += 1;
                summary.push((path.clone(), format!("create mode {} {path}", octal(*mode))));
            }
            Some((old_mode, old_id)) => {
                if old_id != id || old_mode != mode {
                    files_changed += 1;
                }
                if old_mode != mode {
                    summary.push((
                        path.clone(),
                        format!("mode change {} => {} {path}", octal(*old_mode), octal(*mode)),
                    ));
                }
            }
        }
    }
    for (path, (mode, _)) in &old_entries {
        if !new_paths.contains(path) {
            files_changed += 1;
            summary.push((path.clone(), format!("delete mode {} {path}", octal(*mode))));
        }
    }

    if files_changed == 0 {
        return Ok(());
    }

    let old = repo.find_tree(old_tree)?;
    let new = repo.find_tree(new_tree)?;
    let mut platform = old.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let stats = platform.stats(&new)?;

    let (ins, del) = (stats.lines_added, stats.lines_removed);
    let mut line = format!(" {files_changed} file{} changed", plural(files_changed));
    if ins > 0 || del == 0 {
        line.push_str(&format!(", {ins} insertion{}(+)", plural(ins)));
    }
    if del > 0 || ins == 0 {
        line.push_str(&format!(", {del} deletion{}(-)", plural(del)));
    }
    println!("{line}");

    summary.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, l) in &summary {
        println!(" {l}");
    }
    Ok(())
}

/// git's `has_conforming_footer`: does the message end in a trailer block, so
/// that `-x` may append its line without inserting a blank line first?
///
/// The block is the last paragraph, and it only qualifies when it is not also
/// the first paragraph (a subject is never a footer) and every one of its lines
/// is a `token: value` trailer, an indented continuation, a `#` comment, or a
/// `(cherry picked from commit <id>)` line — the last form being what makes a
/// second `-x` append directly, as stock git does.
fn has_conforming_footer(msg: &[u8]) -> bool {
    // Drop trailing blank lines.
    let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').collect();
    while lines.last().is_some_and(|l| l.iter().all(u8::is_ascii_whitespace)) {
        lines.pop();
    }
    if lines.is_empty() {
        return false;
    }

    // Start of the last paragraph.
    let mut start = lines.len();
    while start > 0 && !lines[start - 1].iter().all(u8::is_ascii_whitespace) {
        start -= 1;
    }
    // No preceding blank line means this paragraph is the subject.
    if start == 0 {
        return false;
    }

    let mut saw_trailer = false;
    for line in &lines[start..] {
        if line.first().is_some_and(|b| b.is_ascii_whitespace()) || line.starts_with(b"#") {
            continue;
        }
        if line.starts_with(b"(cherry picked from commit ") && line.ends_with(b")") {
            saw_trailer = true;
            continue;
        }
        match line.iter().position(|&b| b == b':') {
            // A trailer token is non-empty and contains no whitespace.
            Some(sep)
                if sep > 0 && !line[..sep].iter().any(u8::is_ascii_whitespace) =>
            {
                saw_trailer = true;
            }
            _ => return false,
        }
    }
    saw_trailer
}

/// Move a clean worktree and its index from the state captured in `old` to the
/// tree of commit `new_commit`, writing only the files that changed, and return
/// the index that was persisted.
///
/// Verbatim port of `porcelain::merge`'s helper: added/modified files are
/// checked out via `gix-worktree-state`, removed files are deleted, and the new
/// index reuses prior stats for unchanged entries.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<gix::index::File> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    let mut new_index = repo.index_from_tree(&new_tree_id)?;
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    gix::worktree::state::checkout(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

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

    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    new_index.remove_tree();
    new_index.write(Default::default())?;
    Ok(new_index)
}

/// The git-internal octal representation of a tree entry mode, e.g. `100644`.
fn octal(mode: EntryMode) -> String {
    let mut buf = [0u8; 6];
    mode.as_bytes(&mut buf).to_string()
}

/// `""` for a count of 1, `"s"` otherwise — for git's `file`/`files` etc.
fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
