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
//!   * `git restore --ours/--theirs <pathspec>...`    worktree ← unmerged stage 2/3
//!   * `git restore --merge [--conflict=<style>] ...` worktree ← recreated conflict
//!   * `git restore --overlay ...`                    keep target files absent in source
//!   * `git restore --pathspec-from-file=<f> ...`     read pathspecs from a file/stdin
//!   * `git restore --recurse-submodules <pathspec>`  also restore matched submodule worktrees
//!
//! The default restore source is the index for `--worktree`, and `HEAD` when
//! `--staged` is given (either alone or combined). Restore is no-overlay by
//! default: a path present in the target but not the source is removed; with
//! `--overlay` such files are kept. `--ours`/`--theirs` pick the stage-2/stage-3
//! blob of an unmerged path; `--merge`/`--conflict` recreate the 3-way conflict
//! (with markers) in the worktree. With `--recurse-submodules`, any matched,
//! active submodule whose gitlink appears in the restore source has its worktree
//! reset to the recorded commit (local modifications overwritten, submodule HEAD
//! detached), matching git-restore(1). Interactive `--patch` stays unimplemented
//! (no non-interactive hunk-selection semantics here).

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU8;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::diff::blob::{Algorithm, InternedInput};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::merge::blob::builtin_driver::text::{Conflict, ConflictStyle, Labels, Options};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

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

/// Which unmerged stage a conflict-resolution flag selects.
#[derive(Copy, Clone, PartialEq)]
enum Pick {
    Ours,
    Theirs,
}

/// Resolve a (possibly subdirectory-relative) pathspec to a repo-root-relative,
/// slash-separated path. Returns `Ok(None)` when the spec designates the whole
/// tree (a `.`/empty at the worktree root), `Ok(Some(path))` for a concrete
/// path, and `Err(())` when the spec escapes the worktree.
fn resolve_spec(prefix: &[String], wd: &Path, raw: &str) -> Result<Option<String>, ()> {
    // Absolute pathspec: resolve lexically against the worktree root.
    if raw.starts_with('/') {
        let wds = wd.to_string_lossy();
        if raw == wds {
            return Ok(None);
        }
        return match raw.strip_prefix(&*wds).and_then(|r| r.strip_prefix('/')) {
            Some(rest) if rest.is_empty() => Ok(None),
            Some(rest) => Ok(Some(rest.trim_end_matches('/').to_string())),
            None => Err(()),
        };
    }
    let mut comps: Vec<&str> = prefix.iter().map(String::as_str).collect();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if comps.pop().is_none() {
                    return Err(());
                }
            }
            other => comps.push(other),
        }
    }
    if comps.is_empty() {
        Ok(None)
    } else {
        Ok(Some(comps.join("/")))
    }
}

/// Perform a 3-way text merge of the three unmerged stages and return the
/// merged bytes (with conflict markers, using git's `ours`/`base`/`theirs`
/// labels). A missing stage is treated as empty content.
fn three_way_merge(
    repo: &gix::Repository,
    base: Option<ObjectId>,
    ours: Option<ObjectId>,
    theirs: Option<ObjectId>,
    style: ConflictStyle,
) -> Result<Vec<u8>> {
    let load = |o: Option<ObjectId>| -> Result<Vec<u8>> {
        Ok(match o {
            Some(id) => repo.find_object(id)?.detach().data,
            None => Vec::new(),
        })
    };
    let base_b = load(base)?;
    let our_b = load(ours)?;
    let their_b = load(theirs)?;

    let mut input = InternedInput::new(our_b.as_slice(), their_b.as_slice());
    let mut out = Vec::new();
    let opts = Options {
        diff_algorithm: Algorithm::Myers,
        conflict: Conflict::Keep {
            style,
            marker_size: NonZeroU8::new(7).expect("7 != 0"),
        },
    };
    // The free 3-way text merge is re-exported as the value `builtin_driver::text`
    // (a function that shares its name with the `text` module), so it is invoked
    // by its full path rather than a `text::merge` alias.
    gix::merge::blob::builtin_driver::text(
        &mut out,
        &mut input,
        Labels {
            ancestor: Some(BStr::new("base")),
            current: Some(BStr::new("ours")),
            other: Some(BStr::new("theirs")),
        },
        our_b.as_slice(),
        base_b.as_slice(),
        their_b.as_slice(),
        opts,
    );
    Ok(out)
}

/// Reset a submodule's worktree to `commit`, overwriting local modifications,
/// and detach its `HEAD` there — the behavior `git restore --recurse-submodules`
/// applies to each matched active submodule (git-restore(1), `submodule_move_head`
/// in git's `builtin/checkout.c`).
///
/// The recorded commit is peeled to its tree, unpacked into an index, and checked
/// out over the existing worktree with overwrite; files tracked before but absent
/// in the target tree are deleted, the submodule index is rewritten with fresh
/// stats, and `HEAD` is repointed to the detached commit.
fn restore_submodule_worktree(
    sm_repo: &gix::Repository,
    commit: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let sm_workdir = match sm_repo.workdir() {
        Some(w) => w.to_owned(),
        // A bare submodule checkout has no worktree to restore.
        None => return Ok(()),
    };
    let tree_id = sm_repo.find_object(commit)?.peel_to_tree()?.id;

    // Target index (all target-tree entries) — the write target and deletion set.
    let mut target_index = sm_repo.index_from_tree(&tree_id)?;
    let new_paths: HashSet<BString> = {
        let b = target_index.path_backing();
        target_index.entries().iter().map(|e| e.path_in(b).to_owned()).collect()
    };

    // Files tracked in the submodule's current index but gone from the target.
    let old_paths: Vec<BString> = match sm_repo.open_index() {
        Ok(idx) => {
            let b = idx.path_backing();
            idx.entries().iter().map(|e| e.path_in(b).to_owned()).collect()
        }
        Err(_) => Vec::new(),
    };

    // Check out the full target index over the existing worktree (a separate copy
    // is passed since `checkout` takes the index's path backing out).
    let mut subset = target_index.clone();
    let mut opts =
        sm_repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = sm_repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        sm_workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files that the target tree no longer tracks.
    for p in &old_paths {
        if !new_paths.contains(p) {
            if let Some(full) = sm_repo.workdir_path(BStr::new(p)) {
                let _ = std::fs::remove_file(full);
            }
        }
    }

    // Copy the fresh checkout stats into the target index before persisting it.
    let mut fresh: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let b = subset.path_backing();
        for e in subset.entries() {
            fresh.insert(e.path_in(b).to_owned(), e.stat);
        }
    }
    {
        let b = target_index.path_backing().to_owned();
        for e in target_index.entries_mut() {
            if let Some(stat) = fresh.get(&e.path_in(&b).to_owned()) {
                e.stat = *stat;
            }
        }
    }
    target_index.remove_tree();
    target_index.write(gix::index::write::Options::default())?;

    // Detach the submodule HEAD at the restored commit (git detaches here).
    sm_repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("restore: moving to {commit}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(commit),
        },
        name: "HEAD".try_into().map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

pub fn restore(args: &[String]) -> Result<ExitCode> {
    // --- Argument parsing ---------------------------------------------------
    let mut staged = false;
    let mut worktree = false;
    let mut source: Option<String> = None;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    let mut overlay = false;
    let mut pick: Option<Pick> = None;
    let mut merge_flag = false;
    let mut conflict_style: Option<ConflictStyle> = None;
    let mut ignore_unmerged = false;
    let mut pathspec_from_file: Option<String> = None;
    let mut pathspec_file_nul = false;
    let mut recurse_submodules = false;

    // Parse a `--conflict` style value; git errors with exit 129 on unknown.
    let parse_conflict = |v: &str| -> Option<ConflictStyle> {
        match v {
            "merge" => Some(ConflictStyle::Merge),
            "diff3" => Some(ConflictStyle::Diff3),
            "zdiff3" => Some(ConflictStyle::ZealousDiff3),
            _ => None,
        }
    };

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
                    None => {
                        // git: short flags are "switch `s'", long are "option `source'".
                        if a == "-s" {
                            eprintln!("error: switch `s' requires a value");
                        } else {
                            eprintln!("error: option `source' requires a value");
                        }
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "--overlay" => overlay = true,
            "--no-overlay" => overlay = false,
            "--ours" | "-2" => pick = Some(Pick::Ours),
            "--theirs" | "-3" => pick = Some(Pick::Theirs),
            "-m" | "--merge" => merge_flag = true,
            "--conflict" => {
                i += 1;
                match args.get(i) {
                    Some(v) => match parse_conflict(v) {
                        Some(s) => conflict_style = Some(s),
                        None => {
                            eprintln!("error: unknown conflict style '{v}'");
                            return Ok(ExitCode::from(129));
                        }
                    },
                    None => {
                        eprintln!("error: option `conflict' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "--ignore-unmerged" => ignore_unmerged = true,
            "--pathspec-file-nul" => pathspec_file_nul = true,
            "--pathspec-from-file" => {
                i += 1;
                match args.get(i) {
                    Some(v) => pathspec_from_file = Some(v.clone()),
                    None => {
                        eprintln!("error: option `pathspec-from-file' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // Accepted no-ops: quiet/progress/default no-recurse/diff-context knobs
            // (context knobs only affect interactive `--patch`, unsupported here).
            "-q" | "--quiet" | "--progress" | "--no-progress"
            | "--ignore-skip-worktree-bits" | "--no-ignore-skip-worktree-bits" => {}
            "-U" | "--unified" | "--inter-hunk-context" => {
                i += 1;
            }
            "-p" | "--patch" => bail!("interactive patch mode (-p/--patch) is not supported"),
            "--recurse-submodules" => recurse_submodules = true,
            "--no-recurse-submodules" => recurse_submodules = false,
            s if s.starts_with("--source=") => source = Some(s["--source=".len()..].to_string()),
            s if s.starts_with("--conflict=") => {
                let v = &s["--conflict=".len()..];
                match parse_conflict(v) {
                    Some(style) => conflict_style = Some(style),
                    None => {
                        eprintln!("error: unknown conflict style '{v}'");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--pathspec-from-file=") => {
                pathspec_from_file = Some(s["--pathspec-from-file=".len()..].to_string());
            }
            s if s.starts_with("-U") && s.len() > 2 => {}
            s if s.starts_with("-s") && s.len() > 2 => source = Some(s[2..].to_string()),
            s if s.starts_with('-') && s != "-" => {
                eprintln!("error: unknown option `{}'", s.trim_start_matches('-'));
                return Ok(ExitCode::from(129));
            }
            _ => pathspecs.push(a.clone()),
        }
        i += 1;
    }

    let merge_active = merge_flag || conflict_style.is_some();
    let conflict_mode = pick.is_some() || merge_active;

    // --- Pathspec-from-file -------------------------------------------------
    if pathspec_file_nul && pathspec_from_file.is_none() {
        eprintln!("fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'");
        return Ok(ExitCode::from(128));
    }
    if let Some(f) = pathspec_from_file.clone() {
        if !pathspecs.is_empty() {
            eprintln!("fatal: '--pathspec-from-file' and pathspec arguments cannot be used together");
            return Ok(ExitCode::from(128));
        }
        let data = if f == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
            buf
        } else {
            std::fs::read(&f)?
        };
        let sep = if pathspec_file_nul { b'\0' } else { b'\n' };
        for part in data.split(|&c| c == sep) {
            let mut s = part;
            if !pathspec_file_nul && s.last() == Some(&b'\r') {
                s = &s[..s.len() - 1];
            }
            if s.is_empty() {
                continue;
            }
            pathspecs.push(String::from_utf8_lossy(s).into_owned());
        }
    }

    // --- Incompatible-flag combinations (git's fatal/exit-128 diagnostics) --
    if pick.is_some() && staged {
        eprintln!("fatal: '--ours' or '--theirs' cannot be used with --staged");
        return Ok(ExitCode::from(128));
    }
    if merge_active && staged {
        eprintln!("fatal: '--merge' or '--conflict' cannot be used with --staged");
        return Ok(ExitCode::from(128));
    }
    if conflict_mode && source.is_some() {
        eprintln!("fatal: '--merge', '--ours', or '--theirs' cannot be used when checking out of a tree");
        return Ok(ExitCode::from(128));
    }

    // Default target: worktree when neither is named.
    if !staged && !worktree {
        worktree = true;
    }
    if pathspecs.is_empty() {
        eprintln!("fatal: you must specify path(s) to restore");
        return Ok(ExitCode::from(128));
    }

    // --- Repository + lock --------------------------------------------------
    let repo = gix::discover(".")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("this operation must be run in a work tree"))?
        .to_owned();
    let cwd = std::env::current_dir()?;
    // Pathspecs given relative to the current directory are resolved against the
    // worktree root using this prefix, so `git restore` works from any subdir.
    let wd_c = workdir.canonicalize().unwrap_or_else(|_| workdir.clone());
    let cwd_c = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
    let prefix_components: Vec<String> = cwd_c
        .strip_prefix(&wd_c)
        .ok()
        .map(|rel| {
            rel.components()
                .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Normalize pathspecs: `.`/empty (at root) restores everything; the rest are
    // resolved to repo-root-relative paths. A spec escaping the worktree is fatal.
    let mut match_all = false;
    let mut specs: Vec<(String, Vec<u8>)> = Vec::new();
    for p in &pathspecs {
        match resolve_spec(&prefix_components, &wd_c, p) {
            Ok(None) => match_all = true,
            Ok(Some(rel)) => specs.push((p.clone(), rel.into_bytes())),
            Err(()) => {
                eprintln!("fatal: {p}: '{p}' is outside repository at '{}'", wd_c.display());
                return Ok(ExitCode::from(128));
            }
        }
    }
    let specs_bytes: Vec<Vec<u8>> = specs.iter().map(|(_, b)| b.clone()).collect();

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

    // Current index: stage blobs per path (index 0..=3) plus the path set.
    let mut stage_blobs: HashMap<BString, [Option<(ObjectId, Mode)>; 4]> = HashMap::new();
    let mut cur_paths: HashSet<BString> = HashSet::new();
    {
        let b = cur.path_backing();
        for e in cur.entries() {
            let p = e.path_in(b).to_owned();
            let s = e.stage_raw() as usize;
            stage_blobs.entry(p.clone()).or_insert([None, None, None, None])[s] = Some((e.id, e.mode));
            cur_paths.insert(p);
        }
    }
    let is_unmerged =
        |arr: &[Option<(ObjectId, Mode)>; 4]| arr[1].is_some() || arr[2].is_some() || arr[3].is_some();

    // Matched unmerged paths (sorted for git-identical diagnostic ordering).
    let mut unmerged_matched: Vec<BString> = Vec::new();
    for (p, arr) in &stage_blobs {
        if is_unmerged(arr) && path_matches(BStr::new(p), match_all, &specs_bytes) {
            unmerged_matched.push(p.clone());
        }
    }
    unmerged_matched.sort();

    // Validate every explicit pathspec matches something git knows about (the
    // union of source and index paths), mirroring git's pathspec error (exit 1).
    if !match_all {
        for (raw, spec) in &specs {
            let single = [spec.clone()];
            let hit = source_map
                .keys()
                .chain(cur_paths.iter())
                .any(|p| path_matches(BStr::new(p), false, &single));
            if !hit {
                eprintln!("error: pathspec '{raw}' did not match any file(s) known to git");
                return Ok(ExitCode::from(1));
            }
        }
    }

    // Unmerged handling for the pure worktree-from-index restore: without a
    // conflict-resolution flag such a path is an error (exit 1), unless
    // `--ignore-unmerged` downgrades it to a skipped warning.
    if source_is_index && worktree && !conflict_mode && !unmerged_matched.is_empty() {
        if ignore_unmerged {
            for p in &unmerged_matched {
                eprintln!("warning: path '{p}' is unmerged");
            }
        } else {
            for p in &unmerged_matched {
                eprintln!("error: path '{p}' is unmerged");
            }
            return Ok(ExitCode::from(1));
        }
    }

    // Conflict-resolution targets for the worktree: the resolved stage-0 blob
    // for each matched unmerged path (or removal when the chosen side deleted it).
    let mut resolved_entries: Vec<(BString, ObjectId, Mode)> = Vec::new();
    let mut resolved_remove: HashSet<BString> = HashSet::new();
    if conflict_mode {
        for p in &unmerged_matched {
            let arr = &stage_blobs[p];
            if merge_active {
                let (ours, theirs) = (arr[2], arr[3]);
                if ours.is_none() && theirs.is_none() {
                    resolved_remove.insert(p.clone());
                    continue;
                }
                let mode = ours.or(theirs).map(|(_, m)| m).expect("one side present");
                let merged = three_way_merge(
                    &repo,
                    arr[1].map(|(id, _)| id),
                    ours.map(|(id, _)| id),
                    theirs.map(|(id, _)| id),
                    conflict_style.unwrap_or(ConflictStyle::Merge),
                )?;
                let id = repo.write_blob(&merged)?.detach();
                resolved_entries.push((p.clone(), id, mode));
            } else {
                let want = match pick {
                    Some(Pick::Ours) => arr[2],
                    _ => arr[3],
                };
                match want {
                    Some((id, mode)) => resolved_entries.push((p.clone(), id, mode)),
                    None => {
                        resolved_remove.insert(p.clone());
                    }
                }
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
        // Resolve unmerged matched paths: drop all their stage entries so the
        // source (a tree) can re-add a single stage-0 entry below.
        if !unmerged_matched.is_empty() {
            let um: HashSet<BString> = unmerged_matched.iter().cloned().collect();
            cur.remove_entries(|_, p, e| e.stage_raw() != 0 && um.contains(&p.to_owned()));
        }
        let mut need_sort = false;
        for (path, id, mode, stat) in &updates {
            match cur.entry_index_by_path(BStr::new(path)) {
                Ok(idx) => {
                    let e = &mut cur.entries_mut()[idx];
                    e.id = *id;
                    e.mode = *mode;
                    e.stat = *stat;
                }
                Err(_) => {
                    cur.dangerously_push_entry(*stat, *id, Flags::empty(), *mode, BStr::new(path));
                    need_sort = true;
                }
            }
        }
        if !removals.is_empty() {
            cur.remove_entries(|_, p, _| removals.contains(&p.to_owned()));
        }
        for (path, id, mode, flags, stat) in &inserts {
            cur.dangerously_push_entry(*stat, *id, *flags, *mode, BStr::new(path));
        }
        if !inserts.is_empty() || need_sort {
            cur.sort_entries();
        }
    }

    // --- Apply worktree checkout -------------------------------------------
    let mut fresh_stats: HashMap<BString, Stat> = HashMap::new();
    if worktree {
        let should_interrupt = AtomicBool::new(false);

        // Subset of the source restricted to matched stage-0 entries, plus any
        // conflict-resolved entries; checked out over the existing worktree.
        let mut subset = source_index.clone();
        subset.remove_entries(|_, p, e| e.stage_raw() != 0 || !path_matches(p, match_all, &specs_bytes));
        for (path, id, mode) in &resolved_entries {
            subset.dangerously_push_entry(Stat::default(), *id, Flags::empty(), *mode, BStr::new(path));
        }
        if !resolved_entries.is_empty() {
            subset.sort_entries();
        }

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
        // `--overlay` suppresses these; conflict-resolution deletes (a side that
        // removed the file) are applied regardless of overlay.
        if !overlay {
            for path in &removals {
                if let Some(full) = repo.workdir_path(BStr::new(path)) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
        for path in &resolved_remove {
            if let Some(full) = repo.workdir_path(BStr::new(path)) {
                let _ = std::fs::remove_file(full);
            }
        }

        // --- Recurse into matched submodules --------------------------------
        // git-restore(1): when the restore location includes the working tree
        // and `--recurse-submodules` is given, every matched *active* submodule
        // has its worktree reset to the commit recorded in the superproject
        // (the restore source), overwriting local modifications and detaching
        // the submodule HEAD. Without the flag submodule worktrees are left
        // untouched. A submodule whose gitlink is absent from the source, is
        // inactive, or is uninitialized (no checked-out repo) is skipped.
        if recurse_submodules {
            if let Some(subs) = repo.submodules()? {
                for sm in subs {
                    let sm_path = sm.path()?;
                    if !path_matches(BStr::new(&sm_path), match_all, &specs_bytes) {
                        continue;
                    }
                    // Target commit = the gitlink recorded in the restore source.
                    let target = match source_map.get(&sm_path) {
                        Some((id, _, _, _)) => *id,
                        None => continue,
                    };
                    if !sm.is_active().unwrap_or(false) {
                        continue;
                    }
                    let sm_repo = match sm.open()? {
                        Some(r) => r,
                        None => continue,
                    };
                    restore_submodule_worktree(&sm_repo, target, &should_interrupt)?;
                }
            }
        }
    }

    // --- Persist the index --------------------------------------------------
    // Written when the index itself changed (--staged), or when the default
    // worktree restore refreshed stats so a later status stays clean. A pure
    // `--source` worktree restore leaves the index untouched (content now
    // differs from it, which git reflects as an unstaged modification).
    // Conflict-resolution (--ours/--theirs/--merge) leaves the unmerged stages
    // intact: only matched clean stage-0 entries get their stats refreshed.
    let index_write_needed = staged || (worktree && source_is_index);
    if index_write_needed {
        if worktree {
            // Only refresh stage-0 entries; unmerged stages (1..3) left for a
            // conflict-resolution restore must keep their recorded stats intact.
            for (e, p) in cur.entries_mut_with_paths() {
                if e.stage_raw() != 0 {
                    continue;
                }
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
