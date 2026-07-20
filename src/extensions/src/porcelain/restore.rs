//! `git restore` — restore worktree (and/or `--staged` index) files from a source.
//!
//! Backed natively by the vendored gitoxide crates so tools on PATH observe the
//! same staged index. Supported invocation forms (matching stock `git restore`):
//!
//!   * `git restore <pathspec>...`                    worktree ← index (default)
//!   * `git restore --source=<tree> <pathspec>...`    worktree ← <tree>
//!   * `git restore --staged <pathspec>...`           index    ← HEAD (unstage)
//!   * `git restore --staged --source=<tree> ...`     index    ← <tree>
//!   * `git restore --staged --worktree [-s <tree>]`  both     ← HEAD (or <tree>)
//!
//! The default restore source is the index for `--worktree`, and `HEAD` when
//! `--staged` is given (either alone or combined). Restore is no-overlay: a path
//! that exists in the target but not in the source is removed. `--overlay`,
//! interactive `--patch`, `--pathspec-from-file`, submodule recursion, and merge
//! conflict resolution (`--ours`/`--theirs`/`--merge`/`--conflict`) are rejected
//! with a precise error rather than faked.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};

/// True if `path` matches any of the (repo-root-relative, slash-separated)
/// pathspecs. A spec matches its own exact path, or any path under it as a
/// directory prefix. `match_all` (a `.` or empty spec) matches everything.
fn path_matches(path: &BStr, match_all: bool, specs: &[Vec<u8>]) -> bool {
    if match_all {
        return true;
    }
    let p: &[u8] = path.as_ref();
    specs.iter().any(|s| {
        p == s.as_slice() || (p.len() > s.len() && &p[..s.len()] == s.as_slice() && p[s.len()] == b'/')
    })
}

pub fn restore(args: &[String]) -> Result<ExitCode> {
    // --- Argument parsing ---------------------------------------------------
    let mut staged = false;
    let mut worktree = false;
    let mut source: Option<String> = None;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if after_dashdash {
            pathspecs.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "--staged" | "-S" => staged = true,
            "--worktree" | "-W" => worktree = true,
            "-s" | "--source" => {
                i += 1;
                match args.get(i) {
                    Some(v) => source = Some(v.clone()),
                    None => bail!("option `--source` requires a value"),
                }
            }
            // Accepted no-ops: quiet/progress/default no-overlay/default no-recurse.
            "-q" | "--quiet" | "--progress" | "--no-progress" | "--ignore-unmerged"
            | "--no-overlay" | "--no-recurse-submodules" => {}
            "-p" | "--patch" => bail!("interactive patch mode (-p/--patch) is not supported"),
            "--overlay" => bail!("--overlay is not supported (restore is no-overlay only)"),
            "--recurse-submodules" => bail!("--recurse-submodules is not supported"),
            "--ours" | "--theirs" | "-m" | "--merge" => {
                bail!("conflict resolution (--ours/--theirs/--merge/--conflict) is not supported")
            }
            "--pathspec-from-file" | "--pathspec-file-nul" => {
                bail!("--pathspec-from-file is not supported")
            }
            s if s.starts_with("--source=") => source = Some(s["--source=".len()..].to_string()),
            s if s.starts_with("--conflict") => {
                bail!("conflict resolution (--ours/--theirs/--merge/--conflict) is not supported")
            }
            s if s.starts_with("-s") && s.len() > 2 => source = Some(s[2..].to_string()),
            s if s.starts_with('-') && s != "-" => bail!("unknown option: {s}"),
            _ => pathspecs.push(a.clone()),
        }
        i += 1;
    }

    // Default target: worktree when neither is named.
    if !staged && !worktree {
        worktree = true;
    }
    if pathspecs.is_empty() {
        bail!("you must specify path(s) to restore");
    }

    // Normalize pathspecs: a `.` or empty spec restores everything; others are
    // matched as repo-root-relative paths (trailing slash trimmed).
    let mut match_all = false;
    let mut specs_bytes: Vec<Vec<u8>> = Vec::new();
    for p in &pathspecs {
        let t = p.trim_end_matches('/');
        if t.is_empty() || t == "." {
            match_all = true;
        } else {
            specs_bytes.push(t.as_bytes().to_vec());
        }
    }

    // --- Repository + lock --------------------------------------------------
    let repo = gix::discover(".")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("this operation must be run in a work tree"))?
        .to_owned();
    // Pathspecs are matched repo-root-relative; a subdirectory prefix is not yet
    // applied, so refuse to run from anywhere but the worktree root to avoid
    // silently restoring the wrong files.
    let cwd = std::env::current_dir()?;
    if cwd.canonicalize().ok() != workdir.canonicalize().ok() {
        bail!("run from the repository root; subdirectory-relative pathspecs are not yet supported");
    }

    // Serialize the whole read-modify-write through the repo coordinator so a
    // concurrent zvcs writer can't race `index.lock`. Held for the function.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- Resolve the restore source ----------------------------------------
    // `source_tree_id == None` means "the current index" (default worktree src).
    let source_tree_id: Option<ObjectId> = match &source {
        Some(rev) => Some(
            repo.rev_parse_single(rev.as_str())?
                .object()?
                .peel_to_tree()?
                .id,
        ),
        None if staged => Some(repo.head_tree_id_or_empty()?.detach()),
        None => None,
    };
    let source_is_index = source_tree_id.is_none();

    // The real worktree index — the write target for `--staged`, and the read
    // source in the default worktree case.
    let mut cur = repo.open_index()?;

    // The source, materialized as an index (a tree is unpacked into one).
    let source_index: gix::index::File = match &source_tree_id {
        Some(tid) => repo.index_from_tree(tid)?,
        None => cur.clone(),
    };

    // path -> (id, mode, flags, stat) for the source; used for staged writes.
    let mut source_map: HashMap<BString, (ObjectId, Mode, Flags, Stat)> = HashMap::new();
    {
        let b = source_index.path_backing();
        for e in source_index.entries() {
            source_map.insert(e.path_in(b).to_owned(), (e.id, e.mode, e.flags, e.stat));
        }
    }

    // Current index paths, plus a conflict guard: an unmerged (stage != 0) entry
    // that a spec targets cannot be restored without conflict-resolution flags.
    let mut cur_paths: HashSet<BString> = HashSet::new();
    {
        let b = cur.path_backing();
        for e in cur.entries() {
            let p = e.path_in(b);
            if e.stage_raw() != 0 && path_matches(p, match_all, &specs_bytes) {
                bail!(
                    "path '{}' is unmerged; conflict resolution is not supported",
                    p
                );
            }
            cur_paths.insert(p.to_owned());
        }
    }

    // Validate every explicit pathspec matches something git knows about (the
    // union of source and index paths), mirroring git's fatal pathspec error.
    if !match_all {
        for (spec, raw) in specs_bytes.iter().zip(pathspecs.iter().filter(|p| {
            let t = p.trim_end_matches('/');
            !(t.is_empty() || t == ".")
        })) {
            let single = [spec.clone()];
            let hit = source_map
                .keys()
                .chain(cur_paths.iter())
                .any(|p| path_matches(BStr::new(p), false, &single));
            if !hit {
                bail!("pathspec '{raw}' did not match any file(s) known to git");
            }
        }
    }

    // --- Classify each matched path relative to source vs. index -----------
    // updates: present in both  → overwrite index entry (staged) / rewrite file
    // inserts: source only      → add to index (staged)
    // removals: index only      → drop from index (staged) / delete file (wt)
    let mut updates: Vec<(BString, ObjectId, Mode, Stat)> = Vec::new();
    let mut inserts: Vec<(BString, ObjectId, Mode, Flags, Stat)> = Vec::new();
    let mut removals: HashSet<BString> = HashSet::new();

    let mut candidates: HashSet<&BString> = HashSet::new();
    candidates.extend(source_map.keys());
    candidates.extend(cur_paths.iter());
    for path in candidates {
        if !path_matches(BStr::new(path), match_all, &specs_bytes) {
            continue;
        }
        match (source_map.get(path), cur_paths.contains(path)) {
            (Some((id, mode, _flags, stat)), true) => {
                updates.push((path.clone(), *id, *mode, *stat));
            }
            (Some((id, mode, flags, stat)), false) => {
                inserts.push((path.clone(), *id, *mode, *flags, *stat));
            }
            (None, true) => {
                removals.insert(path.clone());
            }
            (None, false) => {}
        }
    }

    // --- Apply staged (index) mutations ------------------------------------
    if staged {
        for (path, id, mode, stat) in &updates {
            if let Ok(idx) = cur.entry_index_by_path(BStr::new(path)) {
                let e = &mut cur.entries_mut()[idx];
                e.id = *id;
                e.mode = *mode;
                e.stat = *stat;
            }
        }
        if !removals.is_empty() {
            cur.remove_entries(|_, p, _| removals.contains(&p.to_owned()));
        }
        for (path, id, mode, flags, stat) in &inserts {
            cur.dangerously_push_entry(*stat, *id, *flags, *mode, BStr::new(path));
        }
        if !inserts.is_empty() {
            cur.sort_entries();
        }
    }

    // --- Apply worktree checkout -------------------------------------------
    let mut fresh_stats: HashMap<BString, Stat> = HashMap::new();
    if worktree {
        let should_interrupt = AtomicBool::new(false);

        // Subset of the source restricted to matched entries; checked out over
        // the existing worktree.
        let mut subset = source_index.clone();
        subset.remove_entries(|_, p, _| !path_matches(p, match_all, &specs_bytes));

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

        // Capture the fresh filesystem stats produced by the checkout.
        {
            let b = subset.path_backing();
            for e in subset.entries() {
                fresh_stats.insert(e.path_in(b).to_owned(), e.stat);
            }
        }

        // No-overlay: delete worktree files present before but absent in source.
        for path in &removals {
            if let Some(full) = repo.workdir_path(BStr::new(path)) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // --- Persist the index --------------------------------------------------
    // Written when the index itself changed (--staged), or when the default
    // worktree restore refreshed stats so a later status stays clean. A pure
    // `--source` worktree restore leaves the index untouched (content now
    // differs from it, which git reflects as an unstaged modification).
    let index_write_needed = staged || (worktree && source_is_index);
    if index_write_needed {
        if worktree {
            for (e, p) in cur.entries_mut_with_paths() {
                if let Some(stat) = fresh_stats.get(&p.to_owned()) {
                    e.stat = *stat;
                }
            }
        }
        cur.remove_tree();
        cur.write(gix::index::write::Options::default())?;
    }

    Ok(ExitCode::SUCCESS)
}
