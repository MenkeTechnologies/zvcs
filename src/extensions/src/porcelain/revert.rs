//! `git revert <commit>...` — record new commits that undo earlier ones.
//!
//! A revert is a three-way merge with the roles rotated: the *base* is the tree
//! of the commit being reverted, *ours* is the current `HEAD` tree, and *theirs*
//! is the tree of that commit's parent. Applying it yields a tree in which the
//! reverted commit's changes are backed out while later work is preserved.
//!
//! What this port covers, byte-for-byte against stock git:
//!   * `git revert <commit>...` — one commit per argument, applied left to right
//!   * `-n` / `--no-commit`, `-s` / `--signoff`, `-m <n>` / `--mainline <n>`
//!   * `--no-edit` (accepted; this port never opens an editor)
//!   * the generated message (`Revert "…"` / `Reapply "…"`, the
//!     `This reverts commit <oid>.` body, the merge `, reversing / changes made
//!     to <oid>.` variant, and the `Signed-off-by` trailer)
//!   * the summary block (`[<branch> <short-oid>] <subject>`, the ` Date:` line
//!     the sequencer always prints, the short-stat and create/delete/mode lines)
//!   * the `revert: <subject>` reflog message, and the `REVERT_HEAD`,
//!     `MERGE_MSG` and `AUTO_MERGE` files written by `--no-commit`
//!   * the refusal paths: merge without `-m`, missing parent, an index that
//!     differs from `HEAD`, and affected files that are locally modified or
//!     would clobber untracked files — same text, same exit codes (128/129)
//!
//! What this port does NOT do, and refuses rather than approximates:
//!   * **content-level (blob) three-way merge.** The vendored `gix` in this
//!     crate is built without the `merge` feature, so `gix-merge` — both the
//!     blob and the tree merge drivers — is not linked in. Only the trivial
//!     resolutions are performed here: a path taken from *theirs* when *ours*
//!     never moved off the base, from *ours* when *theirs* never moved, or the
//!     shared value when both agree. A path that both sides changed away from
//!     the base needs a real blob merge and is refused. Rename detection is part
//!     of the same missing substrate, so a reverted path that was later renamed
//!     lands in that refused set too.
//!   * `--no-commit` on top of a **pre-existing** staged change. git merges into
//!     the index as it stands; this port merges trees, so it refuses instead.
//!     Several `-n` reverts in one invocation do stack correctly — each step
//!     merges against the tree the previous one staged.
//!   * the sequencer (`--continue`, `--skip`, `--abort`, `--quit`), commit
//!     ranges (`a..b`, which need a rev walk feeding a sequencer todo list),
//!     `--edit`, `--cleanup`, `--strategy`/`-X`, `-S`/`--gpg-sign` and
//!     `--reference` — each bails with a precise message.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::objs::tree::{EntryKind, EntryMode};
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

/// Verbatim `git revert -h` text, printed on a usage error exactly as git does.
const USAGE: &str = "\
usage: git revert [--[no-]edit] [-n] [-m <parent-number>] [-s] [-S[<keyid>]] <commit>...
   or: git revert (--continue | --skip | --abort | --quit)

    --quit                end revert or cherry-pick sequence
    --continue            resume revert or cherry-pick sequence
    --abort               cancel revert or cherry-pick sequence
    --skip                skip current commit and continue
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -n, --no-commit       don't automatically commit
    --commit              opposite of --no-commit
    -e, --[no-]edit       edit the commit message
    -s, --[no-]signoff    add a Signed-off-by trailer
    -m, --[no-]mainline <parent-number>
                          select mainline parent
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --[no-]strategy <strategy>
                          merge strategy
    -X, --[no-]strategy-option <option>
                          option for merge strategy
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
    --[no-]reference      use the 'reference' format to refer to commits
";

/// A flattened tree: repository-relative path → (blob/tree-leaf id, entry kind).
type Flat = BTreeMap<BString, (ObjectId, EntryKind)>;

pub fn revert(args: &[String]) -> Result<ExitCode> {
    // `dispatch` hands over the operand list without the verb; tolerate a
    // leading literal `revert` so the module also works if it is ever wired
    // with the full argv.
    let args = match args.first() {
        Some(a) if a == "revert" => &args[1..],
        _ => args,
    };

    let mut no_commit = false;
    let mut signoff = false;
    let mut mainline: Option<usize> = None;
    let mut specs: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            specs.push(a);
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "-n" | "--no-commit" => no_commit = true,
            "--commit" => no_commit = false,
            "-s" | "--signoff" => signoff = true,
            "--no-signoff" => signoff = false,
            "--no-edit" => {} // this port never opens an editor
            "-m" | "--mainline" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error());
                };
                mainline = Some(parse_mainline(v)?);
            }
            "-e" | "--edit" => bail!("--edit is not supported (no editor is spawned)"),
            "--continue" | "--skip" | "--abort" | "--quit" => {
                bail!("sequencer subcommand {a} is not supported (no .git/sequencer state is kept)")
            }
            "--reference" => bail!("--reference is not supported"),
            "--rerere-autoupdate" | "--no-rerere-autoupdate" => {
                bail!("rerere is not supported")
            }
            _ if a.starts_with("--mainline=") => {
                mainline = Some(parse_mainline(&a["--mainline=".len()..])?);
            }
            _ if a.starts_with("-m") && a.len() > 2 => {
                mainline = Some(parse_mainline(&a[2..])?);
            }
            _ if a.starts_with("--cleanup") => bail!("--cleanup is not supported"),
            _ if a.starts_with("--strategy") || a.starts_with("-X") => {
                bail!("merge strategies are not selectable (gix-merge is not linked in)")
            }
            _ if a.starts_with("-S") || a.starts_with("--gpg-sign") || a == "--no-gpg-sign" => {
                bail!("GPG signing is not supported")
            }
            _ => return Ok(usage_error()),
        }
        i += 1;
    }

    if specs.is_empty() {
        return Ok(usage_error());
    }
    if let Some(m) = mainline {
        if m == 0 {
            eprintln!("error: option `mainline' expects a number greater than zero");
            return Ok(ExitCode::from(129));
        }
    }
    for s in &specs {
        if s.contains("..") || s.starts_with('^') {
            bail!("commit ranges are not supported (the sequencer's rev walk is not ported): {s:?}");
        }
    }

    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }
    // Every step below mutates the index, the worktree and a ref: serialize the
    // whole sequence through the repo coordinator, as the other writers do.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // With `-n` nothing is committed between steps, so each further revert must
    // stack onto the tree the previous one produced rather than onto `HEAD`.
    let mut staged_tree: Option<ObjectId> = None;
    for spec in specs {
        match revert_one(&repo, spec, mainline, no_commit, signoff, staged_tree)? {
            Step::Failed(code) => return Ok(code),
            Step::Done { staged } => staged_tree = staged,
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Outcome of one `<commit>` operand.
enum Step {
    /// git reported a refusal itself (text already on stderr); stop with `code`.
    Failed(ExitCode),
    /// Applied. `staged` carries the tree a `--no-commit` step left in the index,
    /// which the next operand merges against.
    Done { staged: Option<ObjectId> },
}

/// Revert a single commit, advancing `HEAD` unless `no_commit` is set.
///
/// `staged_tree` is the tree a previous `--no-commit` step left in the index; it
/// replaces the `HEAD` tree as the *ours* side when present. `Err` is reserved
/// for the cases this port genuinely cannot serve.
fn revert_one(
    repo: &gix::Repository,
    spec: &str,
    mainline: Option<usize>,
    no_commit: bool,
    signoff: bool,
    staged_tree: Option<ObjectId>,
) -> Result<Step> {
    let Ok(target) = repo.rev_parse_single(spec) else {
        eprintln!("fatal: bad revision '{spec}'");
        return Ok(Step::Failed(ExitCode::from(128)));
    };
    let target = target.object()?.peel_to_commit()?;
    let target_id = target.id;
    let parents: Vec<ObjectId> = target.parent_ids().map(|id| id.detach()).collect();
    let is_merge = parents.len() > 1;

    // Parent selection — git's rules, including `-m 1` being a silent no-op on a
    // non-merge commit and `-m N>1` there being an error.
    let parent_id: Option<ObjectId> = if is_merge {
        let Some(m) = mainline else {
            eprintln!("error: commit {target_id} is a merge but no -m option was given.");
            eprintln!("fatal: revert failed");
            return Ok(Step::Failed(ExitCode::from(128)));
        };
        match parents.get(m - 1) {
            Some(p) => Some(*p),
            None => {
                eprintln!("error: commit {target_id} does not have parent {m}");
                eprintln!("fatal: revert failed");
                return Ok(Step::Failed(ExitCode::from(128)));
            }
        }
    } else {
        if let Some(m) = mainline {
            if m > 1 {
                eprintln!("error: commit {target_id} does not have parent {m}");
                eprintln!("fatal: revert failed");
                return Ok(Step::Failed(ExitCode::from(128)));
            }
        }
        parents.first().copied()
    };

    let head = repo.head()?;
    if head.is_unborn() {
        eprintln!("fatal: Your current branch does not have any commits yet");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    drop(head);

    let hash = repo.object_hash();
    let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();
    let base_tree = target.tree_id()?.detach();
    // A root commit has no parent: reverting it means going back to nothing.
    let theirs_tree = match parent_id {
        Some(p) => repo.find_commit(p)?.tree_id()?.detach(),
        None => ObjectId::empty_tree(hash),
    };

    // --- the merge --------------------------------------------------------
    // *ours* is `HEAD`, or the tree a previous `--no-commit` step already staged.
    let ours_tree = staged_tree.unwrap_or(head_tree);
    let base = flatten(repo, base_tree)?;
    let ours = flatten(repo, ours_tree)?;
    let theirs = flatten(repo, theirs_tree)?;
    let merged = merge_trivially(&base, &ours, &theirs, target_id)?;
    let merged_tree = write_tree(repo, &merged)?;

    // --- refuse to clobber ------------------------------------------------
    let changed: Vec<BString> = ours
        .keys()
        .chain(merged.keys())
        .filter(|p| ours.get(*p) != merged.get(*p))
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let wt = scan_worktree(repo)?;
    // A `--no-commit` step of our own already made the index differ from `HEAD`;
    // that is expected and `ours_tree` accounts for it.
    if wt.index_dirty && staged_tree.is_none() {
        if no_commit {
            // git would merge into the index as it stands, which needs the index
            // itself as a merge side — not modelled by this tree-only path.
            bail!("--no-commit with pre-existing staged changes is not supported (the revert is computed against HEAD, not the index)");
        }
        eprintln!("error: your local changes would be overwritten by revert.");
        eprintln!("hint: commit your changes or stash them to proceed.");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    let mut clobbered: Vec<&BString> = changed.iter().filter(|p| wt.modified.contains(*p)).collect();
    if !clobbered.is_empty() {
        clobbered.sort();
        eprintln!("error: Your local changes to the following files would be overwritten by merge:");
        for p in clobbered {
            eprintln!("\t{p}");
        }
        eprintln!("Please commit your changes or stash them before you merge.");
        eprintln!("Aborting");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    let mut untracked: Vec<&BString> = changed
        .iter()
        .filter(|p| !ours.contains_key(*p) && merged.contains_key(*p) && wt.untracked.contains(*p))
        .collect();
    if !untracked.is_empty() {
        untracked.sort();
        eprintln!("error: The following untracked working tree files would be overwritten by merge:");
        for p in untracked {
            eprintln!("\t{p}");
        }
        eprintln!("Please move or remove them before you merge.");
        eprintln!("Aborting");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }

    // --- message ----------------------------------------------------------
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("no committer identity configured"))??;
    let message = build_message(&target, target_id, parent_id.filter(|_| is_merge), signoff, committer)?;
    let subject = message.lines().next().unwrap_or("").to_string();

    // --- apply to index + worktree ---------------------------------------
    let changed_set: HashSet<BString> = changed.iter().cloned().collect();
    apply(repo, &changed_set, merged_tree, &merged)?;

    if no_commit {
        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("REVERT_HEAD"), format!("{target_id}\n"))?;
        std::fs::write(git_dir.join("MERGE_MSG"), &message)?;
        std::fs::write(git_dir.join("AUTO_MERGE"), format!("{merged_tree}\n"))?;
        return Ok(Step::Done {
            staged: Some(merged_tree),
        });
    }

    // A revert that changes nothing produces no commit; git reports this via the
    // commit machinery and exits 1.
    if merged_tree == ours_tree {
        if !wt.modified.is_empty() || !wt.untracked.is_empty() {
            bail!("revert is a no-op and the `git status` advice for a dirty worktree is not ported");
        }
        match repo.head_name()? {
            Some(name) => println!("On branch {}", name.shorten()),
            None => println!("HEAD detached at {}", head_id.to_hex_with_len(7)),
        }
        println!("nothing to commit, working tree clean");
        return Ok(Step::Failed(ExitCode::from(1)));
    }

    // --- write the commit and move HEAD ----------------------------------
    let author = repo
        .author()
        .ok_or_else(|| anyhow::anyhow!("no author identity configured"))??;
    let author_time = author.time()?;
    let commit = gix::objs::Commit {
        message: message.clone().into(),
        tree: merged_tree,
        author: author.into(),
        committer: committer.into(),
        encoding: None,
        parents: std::iter::once(head_id).collect(),
        extra_headers: Default::default(),
    };
    let new_id = repo.write_object(&commit)?.detach();
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("revert: {subject}").into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(head_id)),
            new: Target::Object(new_id),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;

    // A committed revert clears any leftover in-progress markers, as git does.
    for f in ["REVERT_HEAD", "MERGE_MSG", "AUTO_MERGE"] {
        let _ = std::fs::remove_file(repo.git_dir().join(f));
    }

    print_summary(repo, new_id, &subject, &author_time, ours_tree, merged_tree)?;
    Ok(Step::Done { staged: None })
}

/// `-m <n>` value parsing; a non-numeric value is a usage error like git's.
fn parse_mainline(v: &str) -> Result<usize> {
    v.parse::<usize>()
        .map_err(|_| anyhow::anyhow!("option `mainline' expects a numerical value"))
}

/// git's parse-options failure: full usage on stderr, exit 129.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Flatten a tree into `path -> (id, kind)` via the index representation, which
/// already expands nested trees into full slash-separated paths.
fn flatten(repo: &gix::Repository, tree: ObjectId) -> Result<Flat> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    let mut out = Flat::new();
    for e in index.entries() {
        let path = e.path_in(backing);
        let mode: EntryMode = e
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("tree entry `{path}` has an unrepresentable mode"))?;
        out.insert(path.to_owned(), (e.id, mode.kind()));
    }
    Ok(out)
}

/// Three-way merge restricted to the trivially resolvable cases.
///
/// For each path, with `b`/`o`/`t` the base/ours/theirs values: identical sides
/// resolve to that value, a side equal to the base yields the other side. A path
/// both sides moved off the base needs a content merge, which is not available
/// here (see the module note) and is refused instead of guessed.
fn merge_trivially(base: &Flat, ours: &Flat, theirs: &Flat, target: ObjectId) -> Result<Flat> {
    let mut paths: Vec<&BString> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
    paths.sort();
    paths.dedup();

    let mut out = Flat::new();
    let mut conflicts: Vec<&BString> = Vec::new();
    for p in paths {
        let (b, o, t) = (base.get(p), ours.get(p), theirs.get(p));
        let resolved = if o == t {
            o
        } else if o == b {
            t
        } else if t == b {
            o
        } else {
            conflicts.push(p);
            continue;
        };
        if let Some(v) = resolved {
            out.insert(p.clone(), *v);
        }
    }
    if !conflicts.is_empty() {
        let list: Vec<String> = conflicts.iter().map(|p| p.to_string()).collect();
        bail!(
            "reverting {target} needs a content-level merge for {} (gix-merge is not linked in this build; only trivial three-way resolutions are ported)",
            list.join(", ")
        );
    }
    Ok(out)
}

/// Write a flattened tree back into the object database.
fn write_tree(repo: &gix::Repository, flat: &Flat) -> Result<ObjectId> {
    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for (path, (id, kind)) in flat {
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), *kind, *id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// The parts of `git status` this command needs to decide whether it may write.
struct WorktreeState {
    /// The index differs from `HEAD` anywhere (git's "local changes" refusal).
    index_dirty: bool,
    /// Tracked paths whose worktree content differs from the index.
    modified: HashSet<BString>,
    /// Untracked worktree paths.
    untracked: HashSet<BString>,
}

fn scan_worktree(repo: &gix::Repository) -> Result<WorktreeState> {
    let mut state = WorktreeState {
        index_dirty: false,
        modified: HashSet::new(),
        untracked: HashSet::new(),
    };
    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(_) => state.index_dirty = true,
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                match iw {
                    Item::Modification { rela_path, status, .. } => match status {
                        EntryStatus::Conflict { .. } => {
                            bail!("a merge is in progress (unmerged paths); resolve it first")
                        }
                        EntryStatus::IntentToAdd => {
                            state.modified.insert(rela_path);
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(_) => {
                            state.modified.insert(rela_path);
                        }
                    },
                    Item::DirectoryContents { entry, .. } => {
                        if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                            state.untracked.insert(entry.rela_path);
                        }
                    }
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }
    Ok(state)
}

/// Move the index and worktree onto `merged_tree`, touching only `changed`.
///
/// Unrelated index entries are carried over verbatim (with their stats), so a
/// locally modified file outside the revert's footprint stays exactly as it was
/// — matching git, which only refuses when an *affected* path is dirty.
fn apply(
    repo: &gix::Repository,
    changed: &HashSet<BString>,
    merged_tree: ObjectId,
    merged: &Flat,
) -> Result<()> {
    if changed.is_empty() {
        return Ok(());
    }
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Check out just the changed paths that exist in the merged tree.
    let mut subset = repo.index_from_tree(&merged_tree)?;
    subset.remove_entries(|_, path, _| !changed.contains(&path.to_owned()));
    let should_interrupt = AtomicBool::new(false);
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
        &should_interrupt,
        opts,
    )?;

    // Fresh stats produced by that checkout, plus the entry shape to stage.
    let mut fresh: HashMap<BString, (ObjectId, Mode, Flags, Stat)> = HashMap::new();
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            fresh.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.flags, e.stat));
        }
    }

    // Delete worktree files the revert removes.
    for path in changed {
        if !merged.contains_key(path) {
            if let Some(full) = repo.workdir_path(path.as_bstr()) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // Restage: drop every changed path from the current index, then push back
    // the ones the merged tree still has. Untouched entries keep their stats.
    let mut index = repo.index_or_load_from_head()?.into_owned();
    index.remove_entries(|_, path, _| changed.contains(&path.to_owned()));
    for path in changed {
        if let Some((id, mode, flags, stat)) = fresh.get(path) {
            index.dangerously_push_entry(*stat, *id, *flags, *mode, path.as_bstr());
        }
    }
    index.sort_entries();
    index.remove_tree();
    index.write(Default::default())?;
    Ok(())
}

/// Build the revert commit message exactly as the sequencer does.
fn build_message(
    target: &gix::Commit<'_>,
    target_id: ObjectId,
    merge_parent: Option<ObjectId>,
    signoff: bool,
    committer: gix::actor::SignatureRef<'_>,
) -> Result<String> {
    let raw = target.message_raw()?.to_string();
    let orig_subject = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();

    // Reverting a revert reads better as "Reapply"; the original subject already
    // carries the closing quote in that case.
    let mut msg = match orig_subject.strip_prefix("Revert \"") {
        Some(rest) if orig_subject.ends_with('"') => format!("Reapply \"{rest}"),
        _ => format!("Revert \"{orig_subject}\""),
    };
    msg.push_str("\n\nThis reverts commit ");
    msg.push_str(&target_id.to_string());
    if let Some(p) = merge_parent {
        msg.push_str(", reversing\nchanges made to ");
        msg.push_str(&p.to_string());
    }
    msg.push_str(".\n");
    if signoff {
        msg.push_str(&format!(
            "\nSigned-off-by: {} <{}>\n",
            committer.name, committer.email
        ));
    }
    Ok(msg)
}

/// The block git prints after a successful revert: the id/subject line, the
/// author `Date:` line the sequencer always requests, the short-stat, and the
/// create/delete/mode-change summary.
fn print_summary(
    repo: &gix::Repository,
    new_id: ObjectId,
    subject: &str,
    author_time: &gix::date::Time,
    old_tree: ObjectId,
    new_tree: ObjectId,
) -> Result<()> {
    let label = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "detached HEAD".to_string(),
    };
    let short = new_id.attach(repo).shorten_or_id();
    println!("[{label} {short}] {subject}");
    println!(
        " Date: {}",
        author_time.format_or_unix(gix::date::time::format::DEFAULT)
    );

    let old = flatten(repo, old_tree)?;
    let new = flatten(repo, new_tree)?;

    let mut files_changed: u64 = 0;
    let mut summary: Vec<(BString, String)> = Vec::new();
    for (path, (id, kind)) in &new {
        match old.get(path) {
            None => {
                files_changed += 1;
                summary.push((
                    path.clone(),
                    format!("create mode {} {path}", octal(*kind)),
                ));
            }
            Some((old_id, old_kind)) => {
                if old_id != id || old_kind != kind {
                    files_changed += 1;
                }
                if old_kind != kind {
                    summary.push((
                        path.clone(),
                        format!(
                            "mode change {} => {} {path}",
                            octal(*old_kind),
                            octal(*kind)
                        ),
                    ));
                }
            }
        }
    }
    for (path, (_, kind)) in &old {
        if !new.contains_key(path) {
            files_changed += 1;
            summary.push((
                path.clone(),
                format!("delete mode {} {path}", octal(*kind)),
            ));
        }
    }

    // Line counts from a real blob diff; rename detection is off so the file
    // accounting stays consistent with the counts above.
    let old_obj = repo.find_tree(old_tree)?;
    let new_obj = repo.find_tree(new_tree)?;
    let mut platform = old_obj.changes()?;
    platform.options(|o| {
        o.track_rewrites(None);
    });
    let stats = platform.stats(&new_obj)?;

    if files_changed > 0 {
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
    }
    Ok(())
}

/// The 6-digit octal mode git prints in create/delete/mode-change lines.
fn octal(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Tree => "040000",
        EntryKind::Blob => "100644",
        EntryKind::BlobExecutable => "100755",
        EntryKind::Link => "120000",
        EntryKind::Commit => "160000",
    }
}

/// `""` for a count of 1, `"s"` otherwise.
fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
