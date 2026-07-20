//! `git switch` — move `HEAD` to a branch, backed by the vendored gitoxide
//! ref store and worktree-state checkout.
//!
//! Supported invocations (the common forms):
//!   * `git switch <branch>`                    → attach `HEAD` to an existing
//!                                                local branch and update the
//!                                                worktree/index to its tip.
//!   * `git switch -c|--create <new> [<start>]` → create `refs/heads/<new>` at
//!                                                `<start>` (default `HEAD`),
//!                                                attach `HEAD` to it, and update
//!                                                the worktree if `<start>`
//!                                                differs from the current tip.
//!   * `-q`/`--quiet`                           → suppress the informational
//!                                                messages (nothing else changes).
//!
//! Stream and exit-code conventions, reproduced from git 2.55.0 rather than
//! inferred: the informational messages (`Switched to branch '<b>'`, `Switched to
//! a new branch '<b>'`, `Already on '<b>'`) all go to **stderr**, leaving stdout
//! empty on success. Failures print `fatal: <reason>` on stderr and exit 128;
//! option-parsing failures print `error: <reason>` and exit 129.
//!
//! Deferred (bailed with a precise reason, never faked): carrying local
//! modifications across a real worktree change (a clean worktree is required
//! whenever files must change), `--detach`, `-C`/`--force-create`, `--orphan`,
//! `--track`, `--merge`, `--guess`, unknown options, and the `-`/`@{-N}`
//! previous-branch shorthand.
//!
//! Two known divergences from git that are *not* fixable from this file:
//!   * No `.git/logs/HEAD` reflog line is written — see [`attach_head`].
//!   * git tolerates a dirty worktree, carrying modifications across the switch
//!     and reporting them as `<status>\t<path>` on **stdout**, and only aborts
//!     (`error: Your local changes …`, exit 1) when a dirty path actually
//!     differs between the two trees. This module instead refuses any dirty
//!     worktree once files must change, which is narrower than git but never
//!     silently loses work.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// git's fatal error convention: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git's option-parsing error convention: `error: <msg>` on stderr, exit 129.
fn usage_error(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(129))
}

/// Parsed command line: the `-c` value (when creating), quiet mode, positionals.
struct Parsed<'a> {
    create: Option<&'a str>,
    quiet: bool,
    positionals: Vec<&'a str>,
}

/// Either a fully parsed command line, or the exit code of an option-parsing
/// failure that has already been reported on stderr.
enum Parse<'a> {
    Ok(Parsed<'a>),
    Failed(ExitCode),
}

/// Parse `switch`'s command line the way git's parse-options does: `-c` takes a
/// value (attached as `-cNAME`/`--create=NAME` or as the following argument),
/// `-q` is a boolean, short options cluster (`-qc NAME`), and `--` ends options.
fn parse<'a>(args: &'a [String]) -> Result<Parse<'a>> {
    let mut create: Option<&'a str> = None;
    let mut quiet = false;
    let mut positionals: Vec<&'a str> = Vec::new();
    let mut only_positional = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        if only_positional {
            positionals.push(a);
            i += 1;
            continue;
        }
        if a == "--" {
            only_positional = true;
            i += 1;
            continue;
        }
        // `-` and `@{-N}` are the previous-branch shorthand, which needs the
        // HEAD reflog walk this module does not implement.
        if a == "-" || a.starts_with("@{-") {
            bail!("switching to the previous branch ('{a}') is not supported");
        }

        // Long options, with or without an attached `=value`.
        if let Some(long) = a.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            match name {
                "create" => {
                    create = Some(match attached {
                        Some(v) => v,
                        None => {
                            i += 1;
                            match args.get(i) {
                                Some(v) => v.as_str(),
                                None => {
                                    return usage_error("option `create' requires a value")
                                        .map(Parse::Failed)
                                }
                            }
                        }
                    });
                }
                "quiet" => quiet = true,
                "force-create" => {
                    bail!("force-create (-C) is not supported; delete the branch and use -c")
                }
                "detach" => bail!("switching to a detached HEAD (--detach) is not supported"),
                "orphan" => bail!("creating an orphan branch (--orphan) is not supported"),
                "merge" => bail!("three-way merge on switch (--merge) is not supported"),
                "track" | "no-track" | "guess" | "no-guess" => {
                    bail!("upstream tracking/guessing (--{name}) is not supported")
                }
                "force" | "discard-changes" => {
                    bail!("discarding local changes on switch (--{name}) is not supported")
                }
                _ => bail!("unsupported flag {a:?}"),
            }
            i += 1;
            continue;
        }

        // Short option cluster: every char is a boolean except `c`, which takes
        // the remainder of the argument or the next one.
        if let Some(shorts) = a.strip_prefix('-') {
            let mut off = 0;
            while off < shorts.len() {
                let ch = shorts[off..].chars().next().expect("in-bounds");
                let next_off = off + ch.len_utf8();
                match ch {
                    'q' => {}
                    'c' => {
                        let rest = &shorts[next_off..];
                        if rest.is_empty() {
                            i += 1;
                            match args.get(i) {
                                Some(v) => create = Some(v.as_str()),
                                None => {
                                    return usage_error("switch `c' requires a value")
                                        .map(Parse::Failed)
                                }
                            }
                        } else {
                            create = Some(rest);
                        }
                        off = shorts.len();
                        continue;
                    }
                    'C' => {
                        bail!("force-create (-C) is not supported; delete the branch and use -c")
                    }
                    'd' => bail!("switching to a detached HEAD (--detach) is not supported"),
                    'm' => bail!("three-way merge on switch (--merge) is not supported"),
                    't' => bail!("upstream tracking/guessing (-t) is not supported"),
                    'f' => bail!("discarding local changes on switch (-f) is not supported"),
                    _ => bail!("unsupported flag \"-{ch}\""),
                }
                off = next_off;
            }
            i += 1;
            continue;
        }

        positionals.push(a);
        i += 1;
    }

    Ok(Parse::Ok(Parsed {
        create,
        quiet,
        positionals,
    }))
}

pub fn switch(args: &[String]) -> Result<ExitCode> {
    let parsed = match parse(args)? {
        Parse::Ok(p) => p,
        Parse::Failed(code) => return Ok(code),
    };

    let repo = gix::discover(".")?;

    match parsed.create {
        Some(new_branch) => switch_create(&repo, new_branch, &parsed.positionals, parsed.quiet),
        None => {
            if parsed.positionals.is_empty() {
                return fatal("missing branch or commit argument");
            }
            switch_existing(&repo, &parsed.positionals, parsed.quiet)
        }
    }
}

/// `git switch <branch>` — attach `HEAD` to an existing local branch.
fn switch_existing(repo: &gix::Repository, positionals: &[&str], quiet: bool) -> Result<ExitCode> {
    if positionals.len() > 1 {
        return fatal("only one reference expected");
    }
    let branch = positionals[0];
    let full = format!("refs/heads/{branch}");

    // A name that cannot even form a ref is reported exactly as a missing one.
    let Ok(full_name) = FullName::try_from(full.as_str()) else {
        return fatal(format!("invalid reference: {branch}"));
    };

    // Already on the requested branch → git reports it and exits 0 without
    // touching the worktree.
    if repo.head_name()?.as_ref().map(|n| n.as_bstr()) == Some(full_name.as_bstr()) {
        if !quiet {
            eprintln!("Already on '{branch}'");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Resolve the target tip (read-only, cheap error path before any lock).
    let mut reference = match repo.try_find_reference(full.as_str())? {
        Some(r) => r,
        None => return fatal(format!("invalid reference: {branch}")),
    };
    let target = reference.into_fully_peeled_id()?.detach();

    let mut head = repo.head()?;
    let current_commit = head.try_peel_to_id()?.map(|id| id.detach());
    let from_desc = describe_head(repo)?;

    // A worktree/index rewrite is only needed when the target commit differs
    // from where HEAD currently resolves; two branches on the same commit (or an
    // unborn HEAD) still just re-point HEAD.
    let needs_worktree = current_commit != Some(target);

    // Serialize the whole read-modify-write through the repo coordinator, held
    // across the ref move and worktree update.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if needs_worktree {
        if repo.is_dirty()? {
            bail!("your local changes would be overwritten; commit or stash them (dirty-worktree switch is unsupported)");
        }
        let old = repo.index_or_load_from_head()?.into_owned();
        update_clean_worktree(repo, &old, target)?;
    }

    attach_head(repo, &full_name, &from_desc, branch)?;
    if !quiet {
        eprintln!("Switched to branch '{branch}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git switch -c <new> [<start>]` — create a local branch and attach `HEAD`.
fn switch_create(
    repo: &gix::Repository,
    branch: &str,
    positionals: &[&str],
    quiet: bool,
) -> Result<ExitCode> {
    if positionals.len() > 1 {
        return fatal("only one reference expected");
    }
    let start = positionals.first().copied();
    let full = format!("refs/heads/{branch}");

    // Validate as a local branch name before any store access. git follows the
    // fatal with two `hint:` lines unless `advice.refSyntax` is disabled.
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        eprintln!("fatal: '{branch}' is not a valid branch name");
        if repo
            .config_snapshot()
            .boolean("advice.refSyntax")
            .unwrap_or(true)
        {
            eprintln!("hint: See 'git help check-ref-format'");
            eprintln!("hint: Disable this message with \"git config set advice.refSyntax false\"");
        }
        return Ok(ExitCode::from(128));
    }
    let full_name: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{branch}': {e}"))?;

    // Current tip and the start-point the new branch is created at.
    let mut head = repo.head()?;
    let current_commit = head.try_peel_to_id()?.map(|id| id.detach());
    // `None` here means "no commit to start from", which is only reachable on an
    // unborn HEAD without an explicit start-point.
    let start_commit: Option<ObjectId> = match start {
        Some(s) => match repo.rev_parse_single(BStr::new(s)) {
            Ok(id) => Some(id.detach()),
            Err(_) => return fatal(format!("invalid reference: {s}")),
        },
        None => current_commit,
    };
    let from_desc = describe_head(repo)?;

    // Serialize the create + attach (+ optional worktree update).
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() {
        return fatal(format!("a branch named '{branch}' already exists"));
    }

    // Unborn HEAD and no start-point: git points HEAD at the new branch and
    // creates no ref at all — the branch materialises with the first commit.
    // Verified against git 2.55.0: exit 0, `Switched to a new branch '<b>'`,
    // `.git/refs/heads` left empty.
    let Some(start_commit) = start_commit else {
        attach_head(repo, &full_name, &from_desc, branch)?;
        if !quiet {
            eprintln!("Switched to a new branch '{branch}'");
        }
        return Ok(ExitCode::SUCCESS);
    };

    // A worktree rewrite is needed only when the start-point differs from the
    // current tip; `switch -c foo` at HEAD is a pure re-label that keeps any
    // local modifications intact.
    let needs_worktree = current_commit != Some(start_commit);

    // Capture the old index (mirroring the clean worktree) before mutating, only
    // when a worktree rewrite is actually required.
    let old = if needs_worktree {
        if repo.is_dirty()? {
            bail!("your local changes would be overwritten; commit or stash them (dirty-worktree switch is unsupported)");
        }
        Some(repo.index_or_load_from_head()?.into_owned())
    } else {
        None
    };

    // Create the branch ref, then (if needed) move the worktree/index, then HEAD.
    repo.reference(
        full.as_str(),
        start_commit,
        PreviousValue::MustNotExist,
        format!("branch: Created from {from_desc}"),
    )?;
    if let Some(old) = old {
        update_clean_worktree(repo, &old, start_commit)?;
    }

    attach_head(repo, &full_name, &from_desc, branch)?;
    if !quiet {
        eprintln!("Switched to a new branch '{branch}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// Point `HEAD` symbolically at `branch_ref`, requesting a
/// `checkout: moving from <from> to <to>` reflog entry.
///
/// Known gap: the vendored `gix-ref` deliberately drops the reflog entry for a
/// symbolic-target update unless `expected` is `ExistingMustMatch(Object(..))`
/// (see `gix-ref/src/store/file/transaction/commit.rs:52`, "no reflog for symref
/// changes as there is no OID involved"), so no `.git/logs/HEAD` line is written
/// here even though git writes one. Reproducing git's line needs a reflog-append
/// API `gix-ref` does not expose; `checkout.rs:557` has the same limitation and
/// this deliberately stays consistent with it rather than diverging.
fn attach_head(
    repo: &gix::Repository,
    branch_ref: &FullName,
    from_desc: &str,
    to_short: &str,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("checkout: moving from {from_desc} to {to_short}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_ref.clone()),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

/// Human description of where `HEAD` currently is, for the reflog `from` field:
/// the short branch name when attached, else the abbreviated commit, else the
/// literal `HEAD` for an unborn ref.
fn describe_head(repo: &gix::Repository) -> Result<String> {
    if let Some(name) = repo.head_name()? {
        return Ok(name.shorten().to_string());
    }
    let mut head = repo.head()?;
    match head.try_peel_to_id()? {
        Some(id) => Ok(id.shorten_or_id().to_string()),
        None => Ok("HEAD".to_string()),
    }
}

/// Move a clean worktree and its index to the tree of commit `new_commit`,
/// writing only the files that changed.
///
/// Ported from the `zsync` reconcile path: the change set is derived by
/// comparing the current index (mirroring the clean worktree) against the target
/// tree, so additions/modifications are checked out through `gix-worktree-state`
/// (correct mode/symlink/filter handling) and removals are deleted from disk.
/// The rewritten index reuses prior stats for unchanged entries and fresh stats
/// for changed ones, keeping a later status check cheap.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    // Index by path of the current entries, for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // The full target index (all new-tree entries) — what is eventually written.
    let mut new_index = repo.index_from_tree(&new_tree_id)?;

    // A second copy reduced to just the changed entries (added, or content/mode
    // differs from `old`) — the subset actually checked out to the worktree.
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

    // Write the changed files into the (clean) worktree, overwriting in place.
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
        &should_interrupt,
        opts,
    )?;

    // Remove files present in the old tree but not the new one.
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

    // Fill in the target index stats: changed entries get their fresh stat;
    // unchanged entries reuse the previous stat.
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

    // Drop any stale cache-tree extension before persisting.
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}
