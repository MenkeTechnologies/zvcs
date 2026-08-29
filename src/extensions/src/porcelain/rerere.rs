//! `git rerere` — reuse recorded resolution of conflicted merges.
//!
//! A port of `rerere.c`: the on-disk formats (`$GIT_DIR/MERGE_RR`,
//! `$GIT_COMMON_DIR/rr-cache/<id>[/[N.]{pre,post,this}image]`), the enablement
//! rule, the five read/prune subcommands, and the record/replay engine that the
//! mergy commands drive through [`repo_rerere`].
//!
//! The engine is `handle_path()`: it reads a file that already carries conflict
//! markers, discards the diff3 common-ancestor section, orders the two sides so
//! the lexicographically smaller one comes first, and SHA-1s
//! `<side1> NUL <side2> NUL` to derive the conflict id. That id names the
//! `rr-cache/<id>` directory holding the `preimage` (the normalised conflict)
//! and, once the user has resolved it, the `postimage` (the resolution).
//!
//! Replaying a resolution is a three-way merge of the *current* normalised
//! conflict against `preimage` → `postimage`, i.e. git's `ll_merge()` call in
//! `merge()`. That runs on `gix-merge`'s text driver here, the same driver
//! behind every other three-way merge in this build (see `merge_apply.rs`), so
//! a replay lands exactly where `git merge` would have left the file.

// `print!`/`println!` here go through git's stdout buffer. `merge` reaches this
// module in-process and arms that buffer (see `crate::cstdio`), so both halves of
// its output have to be buffered or they interleave against each other; run as
// its own command nothing arms it and these are unbuffered writes as before.
use crate::cstdio::print;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{Algorithm, Diff, InternedInput, UnifiedDiff};
use gix::index::entry::{Flags, Mode, Stat};
use gix::merge::blob::builtin_driver::text;
use gix::merge::blob::Resolution;

/// git's `rerere_usage` plus its option block, verbatim.
const USAGE: &str = "\
usage: git rerere [clear | forget <pathspec>... | diff | status | remaining | gc]

    --[no-]rerere-autoupdate
                          register clean resolutions in index

";

/// `options[]` (builtin/rerere.c:60-65), the whole table: one `OPT_SET_INT`,
/// which is negatable and takes no value.
pub(super) const LONG_OPTS: &[super::LongOpt] = &[super::LongOpt {
    name: "rerere-autoupdate",
    neg: true,
    arg: super::Arg::None,
}];

/// `rr_dir->status[variant]` bits from `rerere.c`.
const RR_HAS_POSTIMAGE: u8 = 1;
const RR_HAS_PREIMAGE: u8 = 2;

/// `DEFAULT_CONFLICT_MARKER_SIZE` from `ll-merge.h`.
const DEFAULT_CONFLICT_MARKER_SIZE: usize = 7;

/// One `MERGE_RR` record: a conflict id (hex + variant) bound to a worktree path.
struct RrEntry {
    /// Repository-root-relative path, exactly as stored (raw bytes).
    path: BString,
    /// Lowercase hex conflict id naming the `rr-cache/<hex>` directory.
    hex: String,
    /// Variant index; 0 means the unsuffixed `preimage`/`postimage` files.
    /// `-1` is git's "not known yet", assigned by [`assign_variant`].
    variant: i32,
    /// git's `item->util != NULL`: whether this record still names a conflict
    /// that needs carrying over into the next `MERGE_RR`. Cleared once the path
    /// has been recorded or replayed, and never set on the "punted" entries
    /// `rerere_remaining()` splices in.
    live: bool,
}

/// The `rerere_dirs` strmap: which `preimage`/`postimage` variants each
/// `rr-cache/<hex>` directory holds, scanned on first use and kept in sync as
/// files are written and unlinked.
#[derive(Default)]
struct RrDirs {
    status: HashMap<String, Vec<u8>>,
}

impl RrDirs {
    /// `find_rerere_dir()`: the cached status vector, scanning the directory the
    /// first time an id is seen.
    fn get(&mut self, rr_cache: &Path, hex: &str) -> &mut Vec<u8> {
        self.status
            .entry(hex.to_owned())
            .or_insert_with(|| scan_rerere_dir(&rr_cache.join(hex)))
    }

    /// `fit_variant()`: grow the status vector so `variant` is addressable.
    fn fit(&mut self, rr_cache: &Path, hex: &str, variant: usize) {
        let status = self.get(rr_cache, hex);
        if status.len() <= variant {
            status.resize(variant + 1, 0);
        }
    }
}

/// `git rerere` — dispatch to the subcommand named by the first non-option arg.
///
/// Ported verbs, matching stock git byte-for-byte:
///   * `git rerere status`    → the recorded conflicted paths, one per line.
///   * `git rerere remaining` → recorded paths minus those the index shows as
///     resolved, plus paths git "punts" on (submodules, add/add without both
///     regular-file stages).
///   * `git rerere diff`      → `--- a/<path>` / `+++ b/<path>` and a 3-line
///     context unified diff of the recorded preimage against the worktree file.
///   * `git rerere clear`     → drop preimages that never gained a postimage,
///     remove now-empty id directories, unlink `MERGE_RR`.
///   * `git rerere gc`        → prune by mtime, honouring `gc.rerereResolved`
///     (default 60 days) and `gc.rerereUnresolved` (default 15 days).
///
///   * `git rerere` (no verb) → `do_plain_rerere()`: record a preimage for every
///     conflicted path, record a postimage for every path the user has since
///     resolved, and replay any conflict whose resolution is already known.
///   * `git rerere forget <pathspec>` → drop the recorded resolution for a path
///     and re-record its preimage from the index stages. A path with nothing
///     recorded prints `error: no remembered resolution for '<path>'` and still
///     exits 0: `rerere_forget()` discards each path's result and returns only
///     `write_rr()`'s (rerere.c:1152, :1155).
///
/// Options: `--rerere-autoupdate` / `--no-rerere-autoupdate` override
/// `rerere.autoupdate` for this run, `-h` prints the usage block to stdout with
/// exit 129, and an unknown option or subcommand reproduces git's
/// `usage_with_options` failure on stderr with exit 129.
pub fn rerere(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the verb; every element here is a real argument.
    let rest = args;

    // git.c short-circuits a bare `-h` before repository setup, so it works
    // outside a repository; every other form runs RUN_SETUP first.
    if rest.len() == 1 && rest[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = crate::setup::discover()?;

    // git's parse-options collects non-options and keeps scanning, so flags may
    // appear before or after the verb; `--` ends option parsing.
    let mut positional: Vec<&str> = Vec::new();
    let mut no_more_opts = false;
    // `OPT_RERERE_AUTOUPDATE`: `None` leaves `rerere.autoupdate` in charge.
    let mut autoupdate: Option<bool> = None;
    for a in rest {
        if !no_more_opts {
            if a == "--" {
                no_more_opts = true;
                continue;
            }
            // parse_options_step() tests `--help-all` with a `strcmp()` of its
            // own, ahead of parse_long_opt() and after the `--` break above, so
            // the name never abbreviates and never takes an `=<value>` — that
            // is why it is absent from `LONG_OPTS`. This table has no
            // `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block
            // `-h` prints. Unlike `-h` it is not short-circuited by git.c
            // before `RUN_SETUP`, so it lands here rather than above.
            if a == "-h" || a == "--help-all" {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // Any unambiguous prefix names the option, so `--rerere-au` and
            // `--r` are both `--rerere-autoupdate` and `--n` is its negation.
            let resolved = match super::canonical_long(a, LONG_OPTS) {
                super::Long::Name(name) => name,
                super::Long::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(a, &first, &second, USAGE))
                }
            };
            if resolved == "--rerere-autoupdate" {
                autoupdate = Some(true);
                continue;
            }
            if resolved == "--no-rerere-autoupdate" {
                autoupdate = Some(false);
                continue;
            }
            if let Some(name) = a.strip_prefix("--") {
                eprint!("error: unknown option `{name}'\n{USAGE}");
                return Ok(ExitCode::from(129));
            }
            if a.len() > 1 && a.starts_with('-') {
                let switch = a[1..].chars().next().unwrap_or('?');
                eprint!("error: unknown switch `{switch}'\n{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
        positional.push(a.as_str());
    }

    match positional.first().copied() {
        None => cmd_record(&repo, autoupdate),
        Some("status") => cmd_status(&repo),
        Some("remaining") => cmd_remaining(&repo),
        Some("diff") => cmd_diff(&repo),
        Some("clear") => cmd_clear(&repo),
        Some("gc") => cmd_gc(&repo),
        Some("forget") => cmd_forget(&repo, &positional[1..]),
        Some(_) => {
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

/// `rerere_status()`: print every path recorded in `MERGE_RR`, sorted.
fn cmd_status(repo: &gix::Repository) -> Result<ExitCode> {
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let entries = read_rr(repo)?;

    let mut out = Vec::new();
    for e in &entries {
        out.extend_from_slice(&e.path);
        out.push(b'\n');
    }
    crate::cstdio::write_bytes_io(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `rerere_remaining()`: the recorded paths that the index does not show as
/// resolved, plus the conflicted paths rerere cannot track at all.
fn cmd_remaining(repo: &gix::Repository) -> Result<ExitCode> {
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let mut entries = read_rr(repo)?;

    let index = read_index(repo)?;
    let mut resolved: Vec<BString> = Vec::new();

    let mut i = 0usize;
    while i < index.entries().len() {
        let name: BString = index.entries()[i].path(&index).to_owned();
        let (next, kind) = check_one_conflict(&index, i);
        match kind {
            ConflictType::Punted => insert_path(&mut entries, &name),
            ConflictType::Resolved => resolved.push(name),
            ConflictType::ThreeStaged => {}
        }
        i = next;
    }

    let mut out = Vec::new();
    for e in &entries {
        if resolved.binary_search(&e.path).is_err() {
            out.extend_from_slice(&e.path);
            out.push(b'\n');
        }
    }
    crate::cstdio::write_bytes_io(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `diff_two()` per recorded path: the preimage against the current worktree
/// file, as a 3-line-context unified diff with no function headers (git builds
/// this with `xpp.flags = 0`, so the indent heuristic is off here).
fn cmd_diff(repo: &gix::Repository) -> Result<ExitCode> {
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let entries = read_rr(repo)?;
    let rr_cache = rr_cache_dir(repo);

    let mut out: Vec<u8> = Vec::new();
    for e in &entries {
        let id_dir = rr_cache.join(&e.hex);
        let fail = || anyhow::anyhow!("unable to generate diff for '{}'", id_dir.display());

        let minus = std::fs::read(variant_path(&id_dir, e.variant, "preimage")).map_err(|_| fail())?;
        let worktree = repo
            .workdir_path(&e.path)
            .ok_or_else(|| crate::fatal::need_work_tree())?;
        let plus = std::fs::read(&worktree).map_err(|_| fail())?;

        out.extend_from_slice(b"--- a/");
        out.extend_from_slice(&e.path);
        out.extend_from_slice(b"\n+++ b/");
        out.extend_from_slice(&e.path);
        out.push(b'\n');

        let input = InternedInput::new(minus.as_slice(), plus.as_slice());
        let diff = Diff::compute(Algorithm::Myers, &input);
        UnifiedDiff::new(&diff, &input, HunkWriter { out: &mut out }, ContextSize::symmetrical(3))
            .consume()?;
    }
    crate::cstdio::write_bytes_io(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `rerere_clear()`: forget every record whose resolution was never completed,
/// then drop `MERGE_RR` itself.
fn cmd_clear(repo: &gix::Repository) -> Result<ExitCode> {
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let entries = read_rr(repo)?;
    let rr_cache = rr_cache_dir(repo);
    for e in &entries {
        let id_dir = rr_cache.join(&e.hex);
        let status = scan_rerere_dir(&id_dir);
        let both = RR_HAS_PREIMAGE | RR_HAS_POSTIMAGE;
        if status.get(e.variant.max(0) as usize).copied().unwrap_or(0) & both != both {
            unlink_rr_item(&id_dir, e.variant);
            let _ = std::fs::remove_dir(&id_dir);
        }
    }
    let _ = std::fs::remove_file(merge_rr_path(repo));
    Ok(ExitCode::SUCCESS)
}

/// `rerere_gc()`: prune records by mtime — resolved ones (those with a
/// postimage) after `gc.rerereResolved` days, unresolved ones after
/// `gc.rerereUnresolved` days — then remove id directories left with nothing.
fn cmd_gc(repo: &gix::Repository) -> Result<ExitCode> {
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let mut cutoff_noresolve = now - 15 * 86400;
    let mut cutoff_resolve = now - 60 * 86400;
    // `git_config_get_expiry_in_days()` also accepts an approxidate string, but
    // its caller ignores the parse failure, so an unparsable value leaves the
    // default cutoff in place — which is exactly what falling through does.
    let cfg = repo.config_snapshot();
    if let Some(days) = cfg.string("gc.rerereResolved").and_then(|v| parse_days(v.as_bstr())) {
        cutoff_resolve = now - days * 86400;
    }
    if let Some(days) = cfg.string("gc.rerereUnresolved").and_then(|v| parse_days(v.as_bstr())) {
        cutoff_noresolve = now - days * 86400;
    }

    let rr_cache = rr_cache_dir(repo);
    let dir = std::fs::read_dir(&rr_cache).context("unable to open rr-cache directory")?;

    let mut to_remove: Vec<PathBuf> = Vec::new();
    for ent in dir.flatten() {
        let id_dir = ent.path();
        let mut status = scan_rerere_dir(&id_dir);

        for (variant, slot) in status.iter_mut().enumerate() {
            // `prune_one()`: a postimage dates the resolution, a preimage alone
            // dates an unresolved conflict; neither means nothing to prune.
            let post = variant_path(&id_dir, variant as i32, "postimage");
            let pre = variant_path(&id_dir, variant as i32, "preimage");
            let (then, cutoff) = match mtime_secs(&post) {
                Some(t) => (t, cutoff_resolve),
                None => match mtime_secs(&pre) {
                    Some(t) => (t, cutoff_noresolve),
                    None => continue,
                },
            };
            if then < cutoff {
                unlink_rr_item(&id_dir, variant as i32);
                *slot = 0;
            }
        }

        if status.iter().all(|&s| s == 0) {
            to_remove.push(id_dir);
        }
    }

    for id_dir in to_remove {
        let _ = std::fs::remove_dir(&id_dir);
    }
    Ok(ExitCode::SUCCESS)
}

/// `rerere_forget()`: drop the recorded resolution for each matched path and
/// re-record its preimage, so the user can resolve the conflict again.
fn cmd_forget(repo: &gix::Repository, paths: &[&str]) -> Result<ExitCode> {
    if paths.is_empty() {
        eprintln!("warning: 'git rerere forget' without paths is deprecated");
    }
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let index = read_index(repo)?;
    let mut rr = read_rr(repo)?;
    let mut dirs = RrDirs::default();
    let rr_cache = rr_cache_dir(repo);
    // `read_rr()` populates the strmap through `new_rerere_id_hex()`.
    for e in &rr {
        dirs.get(&rr_cache, &e.hex);
    }

    // `rerere_forget()` walks its own `find_conflict()` list and keeps only the
    // paths the pathspec matches; `git rerere forget` takes literal paths here.
    //
    // `rerere_forget_one_path()`'s return value is discarded (rerere.c:1152): a
    // path with no remembered resolution prints `error: no remembered resolution
    // for '<path>'` and the command still exits 0. Only `write_rr()` decides the
    // status (rerere.c:1155), so a per-path failure must not reach the caller —
    // the diagnostic has already been printed by `forget_one_path`.
    for path in find_conflict(&index) {
        if !paths.is_empty() && !paths.iter().any(|p| p.as_bytes() == path.as_slice()) {
            continue;
        }
        let _ = forget_one_path(repo, &index, &path, &mut rr, &mut dirs);
    }

    write_rr(repo, &rr)?;
    Ok(ExitCode::SUCCESS)
}

/// `rerere_forget_one_path()`: recreate the conflict from the index stages,
/// unlink its postimage, and rewrite the preimage from the same regenerated
/// content.
fn forget_one_path(
    repo: &gix::Repository,
    index: &gix::index::File,
    path: &BString,
    rr: &mut Vec<RrEntry>,
    dirs: &mut RrDirs,
) -> Result<()> {
    let rr_cache = rr_cache_dir(repo);
    let marker_size = marker_size(repo, index, path.as_ref())?;

    let (found, hash) = handle_cache(repo, index, path, marker_size, true, None)?;
    if found < 1 {
        eprintln!("error: could not parse conflict hunks in '{path}'");
        crate::git_fatal!("could not parse conflict hunks in '{path}'");
    }
    let hex = hash.expect("hash requested").to_string();
    let id_dir = rr_cache.join(&hex);

    // Find the variant whose recorded resolution still replays cleanly; that is
    // the one the user is asking to forget.
    let status_nr = dirs.get(&rr_cache, &hex).len();
    let mut variant = 0usize;
    while variant < status_nr {
        let both = RR_HAS_PREIMAGE | RR_HAS_POSTIMAGE;
        if dirs.get(&rr_cache, &hex)[variant] & both == both {
            let this = variant_path(&id_dir, variant as i32, "thisimage");
            handle_cache(repo, index, path, marker_size, false, Some(&this))?;
            let cur = std::fs::read(&this)
                .with_context(|| format!("failed to update conflicted state in '{path}'"))?;
            if try_merge(&id_dir, variant as i32, &cur).is_some() {
                break;
            }
        }
        variant += 1;
    }
    if status_nr <= variant {
        eprintln!("error: no remembered resolution for '{path}'");
        crate::git_fatal!("no remembered resolution for '{path}'");
    }

    let post = variant_path(&id_dir, variant as i32, "postimage");
    if let Err(e) = std::fs::remove_file(&post) {
        if e.kind() == std::io::ErrorKind::NotFound {
            eprintln!("error: no remembered resolution for '{path}'");
        } else {
            eprintln!("error: cannot unlink '{}': {e}", post.display());
        }
        crate::git_fatal!("no remembered resolution for '{path}'");
    }
    dirs.get(&rr_cache, &hex)[variant] &= !RR_HAS_POSTIMAGE;

    handle_cache(
        repo,
        index,
        path,
        marker_size,
        false,
        Some(&variant_path(&id_dir, variant as i32, "preimage")),
    )?;
    dirs.get(&rr_cache, &hex)[variant] |= RR_HAS_PREIMAGE;
    eprintln!("Updated preimage for '{path}'");

    let entry = RrEntry {
        path: path.clone(),
        hex,
        variant: variant as i32,
        live: true,
    };
    match rr.binary_search_by(|e| e.path.cmp(path)) {
        Ok(i) => rr[i] = entry,
        Err(i) => rr.insert(i, entry),
    }
    eprintln!("Forgot resolution for '{path}'");
    Ok(())
}

/// `git rerere` with no verb — `repo_rerere()` with the flags parse-options built.
fn cmd_record(repo: &gix::Repository, autoupdate: Option<bool>) -> Result<ExitCode> {
    repo_rerere(repo, autoupdate)?;
    Ok(ExitCode::SUCCESS)
}

/// `repo_rerere()`: the entry point every mergy command calls once it has left a
/// conflicted index and worktree behind.
///
/// Records a preimage for each newly conflicted path, records a postimage for
/// each path whose conflict the user has since resolved, and replays any
/// conflict whose resolution is already on file — staging the replayed result
/// when `autoupdate` (or `rerere.autoupdate`) says so. A no-op when rerere is
/// disabled, which is the `fd < 0` early return in the C.
pub fn repo_rerere(repo: &gix::Repository, autoupdate: Option<bool>) -> Result<()> {
    if !is_rerere_enabled(repo)? {
        return Ok(());
    }
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // `git_rerere_config()` reads `rerere.autoupdate`; the `RERERE_AUTOUPDATE`
    // / `RERERE_NOAUTOUPDATE` flags then override it for this run.
    let autoupdate = autoupdate.unwrap_or_else(|| {
        repo.config_snapshot()
            .boolean("rerere.autoupdate")
            .unwrap_or(false)
    });

    let mut rr = read_rr(repo)?;
    let mut dirs = RrDirs::default();
    let rr_cache = rr_cache_dir(repo);
    for e in &rr {
        dirs.get(&rr_cache, &e.hex);
    }

    do_plain_rerere(repo, &mut rr, &mut dirs, autoupdate)?;
    write_rr(repo, &rr)
}

/// `do_plain_rerere()`: assign a conflict id to every conflicted path, then let
/// [`rerere_one_path`] record or replay each record.
fn do_plain_rerere(
    repo: &gix::Repository,
    rr: &mut Vec<RrEntry>,
    dirs: &mut RrDirs,
    autoupdate: bool,
) -> Result<()> {
    let index = read_index(repo)?;
    let rr_cache = rr_cache_dir(repo);
    let mut update: Vec<BString> = Vec::new();

    // `MERGE_RR` records paths with conflicts immediately after the merge failed.
    // Some of them may have been hand-resolved since, but the first run catches
    // them all and registers their preimages.
    for path in find_conflict(&index) {
        let marker_size = marker_size(repo, &index, path.as_ref())?;
        let (found, hash) = handle_file(repo, &path, marker_size, true, None)?;
        if found != 0 {
            if let Ok(i) = rr.binary_search_by(|e| e.path.cmp(&path)) {
                remove_variant(&rr_cache, &rr[i].hex, rr[i].variant, dirs);
                rr.remove(i);
            }
        }
        if found < 1 {
            continue;
        }

        let hex = hash.expect("hash requested").to_string();
        dirs.get(&rr_cache, &hex);
        let entry = RrEntry {
            path: path.clone(),
            hex: hex.clone(),
            variant: -1,
            live: true,
        };
        match rr.binary_search_by(|e| e.path.cmp(&path)) {
            Ok(i) => rr[i] = entry,
            Err(i) => rr.insert(i, entry),
        }
        std::fs::create_dir_all(rr_cache.join(&hex))
            .with_context(|| format!("could not create directory '{}'", rr_cache.join(&hex).display()))?;
    }

    for i in 0..rr.len() {
        rerere_one_path(repo, &index, rr, i, dirs, &mut update, autoupdate)?;
    }
    if !update.is_empty() {
        update_paths(repo, &update)?;
    }
    Ok(())
}

/// `do_rerere_one_path()`: record the resolution the user just made, replay a
/// resolution already on file, or (failing both) record a fresh preimage.
fn rerere_one_path(
    repo: &gix::Repository,
    index: &gix::index::File,
    rr: &mut [RrEntry],
    at: usize,
    dirs: &mut RrDirs,
    update: &mut Vec<BString>,
    autoupdate: bool,
) -> Result<()> {
    let rr_cache = rr_cache_dir(repo);
    let (path, hex, mut variant) = {
        let e = &rr[at];
        // The "punted" entries `rerere_remaining()` splices in carry no id, and
        // `do_rerere_one_path()` is never reached for them (`util` is NULL).
        if !e.live || e.hex.is_empty() {
            return Ok(());
        }
        (e.path.clone(), e.hex.clone(), e.variant)
    };
    let id_dir = rr_cache.join(&hex);
    let marker_size = marker_size(repo, index, path.as_ref())?;

    // Has the user resolved it already? A conflict-marker-free worktree file
    // with a preimage on record *is* the resolution.
    if variant >= 0 && handle_file(repo, &path, marker_size, false, None)?.0 == 0 {
        let Some(src) = repo.workdir_path(&path) else {
            crate::git_fatal!("this operation must be run in a work tree");
        };
        std::fs::copy(&src, variant_path(&id_dir, variant, "postimage"))
            .with_context(|| format!("could not write postimage for '{path}'"))?;
        dirs.fit(&rr_cache, &hex, variant as usize);
        dirs.get(&rr_cache, &hex)[variant as usize] |= RR_HAS_POSTIMAGE;
        eprintln!("Recorded resolution for '{path}'.");
        rr[at].live = false;
        return Ok(());
    }

    // Does any existing resolution apply cleanly?
    let status_nr = dirs.get(&rr_cache, &hex).len();
    for v in 0..status_nr {
        let both = RR_HAS_PREIMAGE | RR_HAS_POSTIMAGE;
        if dirs.get(&rr_cache, &hex)[v] & both != both {
            continue;
        }
        if !replay(repo, &path, &id_dir, v as i32, marker_size)? {
            continue;
        }
        // Another variant already applies cleanly, so ours is redundant.
        if variant >= 0 && variant != v as i32 {
            remove_variant(&rr_cache, &hex, variant, dirs);
        }
        if autoupdate {
            if !update.contains(&path) {
                update.push(path.clone());
            }
        } else {
            eprintln!("Resolved '{path}' using previous resolution.");
        }
        rr[at].live = false;
        return Ok(());
    }

    // None of the existing ones applies; we need a new variant.
    variant = assign_variant(&rr_cache, &hex, variant, dirs);
    rr[at].variant = variant;

    handle_file(
        repo,
        &path,
        marker_size,
        false,
        Some(&variant_path(&id_dir, variant, "preimage")),
    )?;
    if dirs.get(&rr_cache, &hex)[variant as usize] & RR_HAS_POSTIMAGE != 0 {
        let stray = variant_path(&id_dir, variant, "postimage");
        std::fs::remove_file(&stray)
            .with_context(|| format!("cannot unlink stray '{}'", stray.display()))?;
        dirs.get(&rr_cache, &hex)[variant as usize] &= !RR_HAS_POSTIMAGE;
    }
    dirs.get(&rr_cache, &hex)[variant as usize] |= RR_HAS_PREIMAGE;
    eprintln!("Recorded preimage for '{path}'");
    Ok(())
}

/// `assign_variant()`: reuse the record's own variant when it has one, else take
/// the first free slot in the id directory.
fn assign_variant(rr_cache: &Path, hex: &str, variant: i32, dirs: &mut RrDirs) -> i32 {
    let status = dirs.get(rr_cache, hex);
    let variant = if variant < 0 {
        status.iter().position(|&s| s == 0).unwrap_or(status.len()) as i32
    } else {
        variant
    };
    dirs.fit(rr_cache, hex, variant as usize);
    variant
}

/// `remove_variant()`: drop both recorded images of one variant and clear its bits.
fn remove_variant(rr_cache: &Path, hex: &str, variant: i32, dirs: &mut RrDirs) {
    if hex.is_empty() || variant < 0 {
        return;
    }
    let id_dir = rr_cache.join(hex);
    let _ = std::fs::remove_file(variant_path(&id_dir, variant, "postimage"));
    let _ = std::fs::remove_file(variant_path(&id_dir, variant, "preimage"));
    dirs.fit(rr_cache, hex, variant as usize);
    dirs.get(rr_cache, hex)[variant as usize] = 0;
}

/// `merge()`: normalise the live conflict into `thisimage`, three-way merge it
/// against `preimage` → `postimage`, and write the result over the worktree file
/// when that merge comes out clean. Returns whether the replay succeeded.
fn replay(
    repo: &gix::Repository,
    path: &BString,
    id_dir: &Path,
    variant: i32,
    marker_size: usize,
) -> Result<bool> {
    let this = variant_path(id_dir, variant, "thisimage");
    if handle_file(repo, path, marker_size, false, Some(&this))?.0 < 0 {
        return Ok(false);
    }
    let Ok(cur) = std::fs::read(&this) else {
        return Ok(false);
    };
    let Some(merged) = try_merge(id_dir, variant, &cur) else {
        return Ok(false);
    };

    // Mark that "postimage" was used, to help gc.
    let post = variant_path(id_dir, variant, "postimage");
    if let Ok(f) = std::fs::OpenOptions::new().append(true).open(&post) {
        let _ = f.set_modified(SystemTime::now());
    }

    let Some(dst) = repo.workdir_path(path) else {
        crate::git_fatal!("this operation must be run in a work tree");
    };
    std::fs::write(&dst, &merged).with_context(|| format!("could not write '{path}'"))?;
    Ok(true)
}

/// `try_merge()`: `ll_merge(preimage → postimage)` applied to `cur`, returning
/// the merged bytes only when the merge resolved without conflict markers.
fn try_merge(id_dir: &Path, variant: i32, cur: &[u8]) -> Option<Vec<u8>> {
    let base = std::fs::read(variant_path(id_dir, variant, "preimage")).ok()?;
    let other = std::fs::read(variant_path(id_dir, variant, "postimage")).ok()?;

    let mut input = InternedInput::default();
    let mut out = Vec::new();
    // git passes empty side labels here; they only ever appear inside conflict
    // markers, and a conflicted result is rejected below.
    let labels = text::Labels {
        ancestor: None,
        current: Some(BStr::new(b"")),
        other: Some(BStr::new(b"")),
    };
    let resolution = text(
        &mut out,
        &mut input,
        labels,
        cur,
        &base,
        &other,
        text::Options::default(),
    );
    (resolution == Resolution::Complete).then_some(out)
}

/// `update_paths()`: stage each replayed resolution at stage #0, replacing the
/// unmerged entries the merge left behind.
fn update_paths(repo: &gix::Repository, update: &[BString]) -> Result<()> {
    let mut index = read_index(repo)?;

    for path in update {
        let Some(abs) = repo.workdir_path(path) else {
            crate::git_fatal!("this operation must be run in a work tree");
        };
        let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;
        let bytes = std::fs::read(&abs).with_context(|| format!("could not open '{path}'"))?;
        let id = repo.write_blob(&bytes)?.detach();
        let mode = if md.is_executable() {
            Mode::FILE_EXECUTABLE
        } else {
            Mode::FILE
        };
        index.remove_entries(|_, p, _| p == path.as_bstr());
        index.dangerously_push_entry(Stat::from_fs(&md)?, id, Flags::empty(), mode, path.as_ref());
        eprintln!("Staged '{path}' using previous resolution.");
    }

    index.sort_entries();
    index.remove_tree();
    // Staging a previous resolution rewrites the real index, so it carries the
    // repository's index-write options (read-cache.c:2830-2831).
    index
        .write(crate::config::index_write_options(repo))
        .context("unable to write new index file")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The conflict-marker normaliser (`handle_path()` and friends)
// ---------------------------------------------------------------------------

/// `check_one_conflict()`'s verdict for one index path.
#[derive(PartialEq)]
enum ConflictType {
    /// Stage #0: the path carries no conflict.
    Resolved,
    /// Conflicted in a shape rerere cannot record (submodules, delete/modify,
    /// add/add without two regular-file stages).
    Punted,
    /// Regular-file stages #2 and #3: the shape rerere records.
    ThreeStaged,
}

/// `check_one_conflict()`: classify the index entry at `i` and return the index
/// of the next path along with that verdict.
fn check_one_conflict(index: &gix::index::File, i: usize) -> (usize, ConflictType) {
    let cache = index.entries();
    let name = cache[i].path(index);

    if cache[i].stage_raw() == 0 {
        return (i + 1, ConflictType::Resolved);
    }

    // Skip the common ancestor (stage #1) entries.
    let mut j = i;
    while j < cache.len() && cache[j].stage_raw() == 1 {
        j += 1;
    }

    let mut kind = ConflictType::Punted;
    if j + 1 < cache.len()
        && cache[j].stage_raw() == 2
        && cache[j + 1].stage_raw() == 3
        && cache[j + 1].path(index) == name
        && is_regular_file(cache[j].mode)
        && is_regular_file(cache[j + 1].mode)
    {
        kind = ConflictType::ThreeStaged;
    }

    while j < cache.len() && cache[j].path(index) == name {
        j += 1;
    }
    (j, kind)
}

/// `find_conflict()`: the conflicted paths rerere can handle, in index order.
fn find_conflict(index: &gix::index::File) -> Vec<BString> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < index.entries().len() {
        let name = index.entries()[i].path(index).to_owned();
        let (next, kind) = check_one_conflict(index, i);
        if kind == ConflictType::ThreeStaged {
            out.push(name);
        }
        i = next;
    }
    out
}

/// `ll_merge_marker_size()`: the `conflict-marker-size` attribute, or 7.
fn marker_size(repo: &gix::Repository, index: &gix::index::File, path: &BStr) -> Result<usize> {
    let mut stack = repo.attributes_only(
        index,
        gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
    )?;
    let mode = Some(Mode::FILE);
    // The first descent loads the `.gitattributes` files along the path so the
    // collection knows the attribute name before the outcome is sized.
    let _ = stack.at_entry(path, mode)?;
    let mut outcome = gix::attrs::search::Outcome::default();
    outcome.initialize_with_selection(stack.attributes_collection(), ["conflict-marker-size"]);
    stack.at_entry(path, mode)?.matching_attributes(&mut outcome);

    for m in outcome.iter_selected() {
        if let gix::attrs::StateRef::Value(v) = m.assignment.state {
            // `ATTR_INT_VALUE_SET` + `atoi`: only a positive integer wins.
            if let Ok(n) = v.as_bstr().to_str().unwrap_or("").parse::<usize>() {
                if n > 0 {
                    return Ok(n);
                }
            }
        }
    }
    Ok(DEFAULT_CONFLICT_MARKER_SIZE)
}

/// `strbuf_getwholeline()` over an in-memory buffer: lines keep their `\n`.
struct LineReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> LineReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn next_line(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        let end = match memchr::memchr(b'\n', &self.data[start..]) {
            Some(i) => start + i + 1,
            None => self.data.len(),
        };
        self.pos = end;
        Some(&self.data[start..end])
    }
}

/// `is_cmarker()`: exactly `marker_size` marker characters, then whitespace —
/// and, for the `<`/`>` markers that always carry a label, specifically a space.
fn is_cmarker(buf: &[u8], marker_char: u8, marker_size: usize) -> bool {
    if buf.len() < marker_size || !buf[..marker_size].iter().all(|&b| b == marker_char) {
        return false;
    }
    // A C string ends in NUL, which is neither ' ' nor a space character, so a
    // line that is *only* markers fails both tests below.
    let next = buf.get(marker_size).copied().unwrap_or(0);
    let want_sp = marker_char == b'<' || marker_char == b'>';
    if want_sp && next != b' ' {
        return false;
    }
    matches!(next, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `rerere_strbuf_putconflict()`: a bare marker line, with no label.
fn put_conflict(out: &mut Vec<u8>, ch: u8, size: usize) {
    out.extend(std::iter::repeat(ch).take(size));
    out.push(b'\n');
}

/// Which side of a conflict hunk the reader is inside.
#[derive(PartialEq, Clone, Copy)]
enum Side {
    One,
    Two,
    Original,
}

/// `handle_conflict()`: consume one `<<<<<<< … >>>>>>>` hunk, emit it normalised
/// (ancestor section dropped, sides sorted) and fold both sides into `ctx`.
///
/// Returns 1 when a complete hunk was consumed, -1 when the markers were
/// malformed. Nested `<<<<<<<` markers recurse, exactly as in the C.
fn handle_conflict(
    out: &mut Vec<u8>,
    io: &mut LineReader<'_>,
    marker_size: usize,
    mut ctx: Option<&mut gix::hash::Hasher>,
) -> i32 {
    let mut hunk = Side::One;
    let (mut one, mut two) = (Vec::new(), Vec::new());
    let mut has_conflicts = -1;

    while let Some(line) = io.next_line() {
        if is_cmarker(line, b'<', marker_size) {
            let mut conflict = Vec::new();
            if handle_conflict(&mut conflict, io, marker_size, None) < 0 {
                break;
            }
            if hunk == Side::One {
                one.extend_from_slice(&conflict);
            } else {
                two.extend_from_slice(&conflict);
            }
        } else if is_cmarker(line, b'|', marker_size) {
            if hunk != Side::One {
                break;
            }
            hunk = Side::Original;
        } else if is_cmarker(line, b'=', marker_size) {
            if hunk != Side::One && hunk != Side::Original {
                break;
            }
            hunk = Side::Two;
        } else if is_cmarker(line, b'>', marker_size) {
            if hunk != Side::Two {
                break;
            }
            // `strbuf_cmp()` is memcmp-then-length, i.e. `Vec<u8>`'s own order.
            if one > two {
                std::mem::swap(&mut one, &mut two);
            }
            has_conflicts = 1;
            put_conflict(out, b'<', marker_size);
            out.extend_from_slice(&one);
            put_conflict(out, b'=', marker_size);
            out.extend_from_slice(&two);
            put_conflict(out, b'>', marker_size);
            if let Some(ctx) = ctx.as_deref_mut() {
                // `git_hash_update(ctx, one.buf, one.len + 1)`: a strbuf is always
                // NUL-terminated, so the terminator is part of the hashed run.
                ctx.update(&one);
                ctx.update(b"\0");
                ctx.update(&two);
                ctx.update(b"\0");
            }
            break;
        } else if hunk == Side::One {
            one.extend_from_slice(line);
        } else if hunk == Side::Two {
            two.extend_from_slice(line);
        }
        // `hunk == RR_ORIGINAL` discards the diff3 common-ancestor section.
    }
    has_conflicts
}

/// `handle_path()`: normalise every conflict in `data`, optionally writing the
/// normalised text to `out`, and derive the conflict id when `want_hash`.
///
/// Returns 1 if conflict hunks were found, 0 if there were none, -1 on a
/// malformed hunk.
fn handle_path(
    data: &[u8],
    marker_size: usize,
    want_hash: bool,
    hash_kind: gix::hash::Kind,
    mut out: Option<&mut Vec<u8>>,
) -> (i32, Option<gix::ObjectId>) {
    let mut ctx = want_hash.then(|| gix::hash::hasher(hash_kind));
    let mut io = LineReader::new(data);
    let mut has_conflicts = 0;

    while let Some(line) = io.next_line() {
        if is_cmarker(line, b'<', marker_size) {
            let mut normalised = Vec::new();
            has_conflicts =
                handle_conflict(&mut normalised, &mut io, marker_size, ctx.as_mut());
            if has_conflicts < 0 {
                break;
            }
            if let Some(out) = out.as_deref_mut() {
                out.extend_from_slice(&normalised);
            }
        } else if let Some(out) = out.as_deref_mut() {
            out.extend_from_slice(line);
        }
    }

    let hash = ctx.map(|c| c.try_finalize().expect("no SHA-1 collision attack"));
    (has_conflicts, hash)
}

/// `handle_file()`: run [`handle_path`] over the worktree file at `path`,
/// writing the normalised text to `output` when one is given.
fn handle_file(
    repo: &gix::Repository,
    path: &BString,
    marker_size: usize,
    want_hash: bool,
    output: Option<&Path>,
) -> Result<(i32, Option<gix::ObjectId>)> {
    let Some(abs) = repo.workdir_path(path) else {
        crate::git_fatal!("this operation must be run in a work tree");
    };
    let data = match std::fs::read(&abs) {
        Ok(d) => d,
        Err(e) => {
            // `error_errno(_("could not open '%s'"), path)`.
            eprintln!("error: could not open '{path}': {}", os_err_message(&e));
            return Ok((-1, None));
        }
    };

    let mut buf = output.map(|_| Vec::new());
    let (found, hash) = handle_path(
        &data,
        marker_size,
        want_hash,
        repo.object_hash(),
        buf.as_mut(),
    );
    if found < 0 {
        if let Some(output) = output {
            let _ = std::fs::remove_file(output);
        }
        eprintln!("error: could not parse conflict hunks in '{path}'");
        return Ok((-1, None));
    }
    if let (Some(output), Some(buf)) = (output, buf) {
        std::fs::write(output, &buf)
            .with_context(|| format!("could not write '{}'", output.display()))?;
    }
    Ok((found, hash))
}

/// `handle_cache()`: reproduce the conflicted merge in-core from the index
/// stages, then run [`handle_path`] over that instead of the worktree file.
fn handle_cache(
    repo: &gix::Repository,
    index: &gix::index::File,
    path: &BString,
    marker_size: usize,
    want_hash: bool,
    output: Option<&Path>,
) -> Result<(i32, Option<gix::ObjectId>)> {
    let mut stages: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for e in index.entries() {
        if e.path(index) != path.as_bstr() {
            continue;
        }
        let stage = e.stage_raw();
        if (1..=3).contains(&stage) {
            let slot = (stage - 1) as usize;
            if stages[slot].is_empty() {
                stages[slot] = repo.find_object(e.id)?.detach().data;
            }
        }
    }

    let mut input = InternedInput::default();
    let mut merged = Vec::new();
    let labels = text::Labels {
        ancestor: None,
        current: Some(BStr::new(b"ours")),
        other: Some(BStr::new(b"theirs")),
    };
    text(
        &mut merged,
        &mut input,
        labels,
        &stages[1],
        &stages[0],
        &stages[2],
        text::Options {
            conflict: text::Conflict::Keep {
                style: text::ConflictStyle::Merge,
                marker_size: u8::try_from(marker_size)
                    .ok()
                    .and_then(|n| n.try_into().ok())
                    .unwrap_or_else(|| text::Conflict::DEFAULT_MARKER_SIZE.try_into().unwrap()),
            },
            ..Default::default()
        },
    );

    let mut buf = output.map(|_| Vec::new());
    let (found, hash) = handle_path(
        &merged,
        marker_size,
        want_hash,
        repo.object_hash(),
        buf.as_mut(),
    );
    if let (Some(output), Some(buf)) = (output, buf) {
        std::fs::write(output, &buf)
            .with_context(|| format!("could not write '{}'", output.display()))?;
    }
    Ok((found, hash))
}

// ---------------------------------------------------------------------------
// On-disk state
// ---------------------------------------------------------------------------

/// `git_path("rr-cache")` — shared across linked worktrees, hence the common dir.
fn rr_cache_dir(repo: &gix::Repository) -> PathBuf {
    repo.common_dir().join("rr-cache")
}

/// `git_path_merge_rr()` — per-worktree, hence the git dir.
fn merge_rr_path(repo: &gix::Repository) -> PathBuf {
    repo.git_dir().join("MERGE_RR")
}

/// `rerere_path()`: variant 0 (or the not-yet-assigned -1) uses the bare name,
/// others append `.<variant>`.
fn variant_path(id_dir: &Path, variant: i32, file: &str) -> PathBuf {
    if variant <= 0 {
        id_dir.join(file)
    } else {
        id_dir.join(format!("{file}.{variant}"))
    }
}

/// `is_rerere_enabled()`: an explicit `rerere.enabled=false` disables it; unset
/// means "enabled only if `rr-cache` already exists"; true creates `rr-cache`.
fn is_rerere_enabled(repo: &gix::Repository) -> Result<bool> {
    let configured = repo.config_snapshot().boolean("rerere.enabled");
    if configured == Some(false) {
        return Ok(false);
    }
    let rr_cache = rr_cache_dir(repo);
    let exists = rr_cache.is_dir();
    if configured.is_none() {
        return Ok(exists);
    }
    if !exists {
        std::fs::create_dir(&rr_cache)
            .with_context(|| format!("could not create directory '{}'", rr_cache.display()))?;
    }
    Ok(true)
}

/// `read_rr()`: parse `MERGE_RR` into path-sorted records. A missing file is an
/// empty record set, but a malformed one is fatal, exactly as in git.
///
/// Record layout is `<hex>[.<variant>]\t<path>\0`.
fn read_rr(repo: &gix::Repository) -> Result<Vec<RrEntry>> {
    let hexsz = repo.object_hash().len_in_hex();
    let Ok(data) = std::fs::read(merge_rr_path(repo)) else {
        return Ok(Vec::new());
    };

    let mut out: Vec<RrEntry> = Vec::new();
    let mut rest: &[u8] = &data;
    while !rest.is_empty() {
        let (rec, next) = match rest.iter().position(|&b| b == 0) {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        rest = next;

        // "There has to be the hash, tab, path and then NUL".
        if rec.len() < hexsz + 2 {
            crate::git_fatal!("corrupt MERGE_RR");
        }
        let hex = std::str::from_utf8(&rec[..hexsz])
            .ok()
            .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| anyhow::anyhow!("corrupt MERGE_RR"))?
            .to_owned();

        let (variant, tab_at) = if rec[hexsz] == b'.' {
            let start = hexsz + 1;
            let mut end = start;
            while end < rec.len() && rec[end].is_ascii_digit() {
                end += 1;
            }
            let digits = std::str::from_utf8(&rec[start..end]).unwrap_or("");
            let v: i32 = digits.parse().map_err(|_| anyhow::anyhow!("corrupt MERGE_RR"))?;
            (v, end)
        } else {
            (0, hexsz)
        };
        if rec.get(tab_at) != Some(&b'\t') {
            crate::git_fatal!("corrupt MERGE_RR");
        }
        let path = BString::from(&rec[tab_at + 1..]);

        // `string_list_insert()` keeps the list sorted and unique; a repeated
        // path keeps its position and takes the later id.
        match out.binary_search_by(|e| e.path.cmp(&path)) {
            Ok(i) => {
                out[i].hex = hex;
                out[i].variant = variant;
            }
            Err(i) => out.insert(
                i,
                RrEntry {
                    path,
                    hex,
                    variant,
                    live: true,
                },
            ),
        }
    }
    Ok(out)
}

/// `write_rr()`: serialise the records back and replace `MERGE_RR` atomically,
/// mirroring git's write-to-lock-then-commit. Records whose conflict has been
/// recorded or replayed (`util == NULL`) are dropped, which is how a fully
/// resolved session leaves an empty `MERGE_RR` behind.
fn write_rr(repo: &gix::Repository, entries: &[RrEntry]) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    for e in entries {
        if !e.live {
            continue;
        }
        if e.variant > 0 {
            write!(buf, "{}.{}\t", e.hex, e.variant)?;
        } else {
            write!(buf, "{}\t", e.hex)?;
        }
        buf.extend_from_slice(&e.path);
        buf.push(0);
    }

    let target = merge_rr_path(repo);
    let lock = target.with_file_name("MERGE_RR.lock");
    std::fs::write(&lock, &buf).context("unable to write rerere record")?;
    std::fs::rename(&lock, &target).context("unable to write rerere record")?;
    Ok(())
}

/// `scan_rerere_dir()`: which variants of an id directory have a preimage
/// and/or a postimage, indexed by variant number.
fn scan_rerere_dir(id_dir: &Path) -> Vec<u8> {
    let mut status: Vec<u8> = Vec::new();
    let Ok(dir) = std::fs::read_dir(id_dir) else {
        return status;
    };
    for ent in dir.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };

        let (bit, suffix) = if let Some(s) = name.strip_prefix("postimage") {
            (RR_HAS_POSTIMAGE, s)
        } else if let Some(s) = name.strip_prefix("preimage") {
            (RR_HAS_PREIMAGE, s)
        } else {
            continue;
        };

        let variant: usize = if suffix.is_empty() {
            0
        } else if let Some(digits) = suffix.strip_prefix('.') {
            match digits.parse() {
                Ok(v) => v,
                Err(_) => continue,
            }
        } else {
            continue;
        };

        if status.len() <= variant {
            status.resize(variant + 1, 0);
        }
        status[variant] |= bit;
    }
    status
}

/// `unlink_rr_item()`: drop the in-progress and both recorded images of one
/// variant. Missing files are not an error, matching `unlink_or_warn()`.
fn unlink_rr_item(id_dir: &Path, variant: i32) {
    for file in ["thisimage", "postimage", "preimage"] {
        let _ = std::fs::remove_file(variant_path(id_dir, variant, file));
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// `S_ISREG()` on an index entry mode — symlinks and gitlinks are excluded.
fn is_regular_file(mode: gix::index::entry::Mode) -> bool {
    mode.bits() & 0o170000 == 0o100000
}

/// git's `strerror` text for a failed `open()`: Rust renders an OS error as
/// `<strerror> (os error N)`, and git shows only the `<strerror>` prefix.
fn os_err_message(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(at) => s[..at].to_string(),
        None => s,
    }
}

/// The integer-days form of a `gc.rerere*` expiry value.
fn parse_days(value: &gix::bstr::BStr) -> Option<i64> {
    value.to_str().ok()?.trim().parse().ok()
}

/// Seconds since the epoch of a file's mtime, or `None` if it does not exist.
fn mtime_secs(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    })
}

/// Add `path` to a path-sorted record list if it is not already present, giving
/// it no conflict id — `string_list_insert()` on the "punted" branch.
fn insert_path(entries: &mut Vec<RrEntry>, path: &BString) {
    if let Err(i) = entries.binary_search_by(|e| e.path.cmp(path)) {
        entries.insert(
            i,
            RrEntry {
                path: path.clone(),
                hex: String::new(),
                variant: 0,
                live: false,
            },
        );
    }
}

/// Emits hunks the way rerere's `diff_two()` does: a bare `@@ -a,b +c,d @@`
/// header (no function context, since `XDL_EMIT_FUNCNAMES` is unset there),
/// prefixed lines, and the no-newline marker.
struct HunkWriter<'a> {
    out: &'a mut Vec<u8>,
}

impl ConsumeHunk for HunkWriter<'_> {
    type Out = ();

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        self.out.extend_from_slice(b"@@ -");
        write_range(self.out, header.before_hunk_start, header.before_hunk_len);
        self.out.extend_from_slice(b" +");
        write_range(self.out, header.after_hunk_start, header.after_hunk_len);
        self.out.extend_from_slice(b" @@\n");

        for &(kind, content) in lines {
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
                self.out.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) {}
}

/// `repo_read_index()`: the index every entry point here reads through.
///
/// git reaches the cache with `repo_read_index(r)`, which reports "no index
/// file" as an index with zero entries rather than as an error — so `git rerere`
/// in a repository that has never staged anything (a fresh `init`, or a
/// `commit --allow-empty` before the first `add`) finds no conflicted paths and
/// returns quietly. `gix`'s `open_index()` fails on the missing file instead, so
/// the empty state is substituted here.
///
/// `open_index`'s `Err` variant is large; boxing it would churn every call site.
#[allow(clippy::result_large_err)]
fn read_index(repo: &gix::Repository) -> Result<gix::index::File> {
    if repo.index_path().exists() {
        return repo.open_index().context("index file corrupt");
    }
    Ok(gix::index::File::from_state(
        gix::index::State::new(repo.object_hash()),
        repo.index_path(),
    ))
}

/// git omits the `,len` field when the hunk spans exactly one line.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    if len == 1 {
        let _ = write!(out, "{start}");
    } else {
        let _ = write!(out, "{start},{len}");
    }
}
