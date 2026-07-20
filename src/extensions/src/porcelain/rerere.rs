//! `git rerere` — reuse recorded resolution of conflicted merges.
//!
//! What is ported here is git's *bookkeeping* half of `rerere.c`: the on-disk
//! formats (`$GIT_DIR/MERGE_RR`, `$GIT_COMMON_DIR/rr-cache/<id>[/[N.]{pre,post,this}image]`),
//! the enablement rule, and the five subcommands that only read or prune those
//! files. Those are byte-faithful ports of the C, including output ordering and
//! the exact set of files each verb unlinks.
//!
//! What is *not* ported is the recording/replaying half, which is built on
//! `ll_merge()`/`xdl_merge()` — git regenerates the conflicted merge from index
//! stages 1/2/3, normalises the conflict hunks, and SHA-1s them to derive the
//! conflict id. gitoxide vendors a blob merge (`gix-merge`), but nothing in the
//! vendored crates reproduces `xdl_merge`'s output byte-for-byte, and the
//! conflict id is a hash *of that output* — an approximation would silently
//! write records under wrong ids and mis-replay resolutions later. So the paths
//! that would need it `bail!` instead of guessing.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{Algorithm, Diff, InternedInput, UnifiedDiff};

/// git's `rerere_usage` plus its option block, verbatim.
const USAGE: &str = "\
usage: git rerere [clear | forget <pathspec>... | diff | status | remaining | gc]

    --[no-]rerere-autoupdate
                          register clean resolutions in index

";

/// `rr_dir->status[variant]` bits from `rerere.c`.
const RR_HAS_POSTIMAGE: u8 = 1;
const RR_HAS_PREIMAGE: u8 = 2;

/// One `MERGE_RR` record: a conflict id (hex + variant) bound to a worktree path.
struct RrEntry {
    /// Repository-root-relative path, exactly as stored (raw bytes).
    path: BString,
    /// Lowercase hex conflict id naming the `rr-cache/<hex>` directory.
    hex: String,
    /// Variant index; 0 means the unsuffixed `preimage`/`postimage` files.
    variant: u32,
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
/// Options: `--rerere-autoupdate` / `--no-rerere-autoupdate` are accepted (they
/// only steer the unported recording path), `-h` prints the usage block to
/// stdout with exit 129, and an unknown option or subcommand reproduces git's
/// `usage_with_options` failure on stderr with exit 129.
///
/// `git rerere` with no verb, and `git rerere forget <pathspec>`, are ported
/// only for the case git itself treats as a no-op — no unmerged index entries
/// and (for the no-verb form) an empty `MERGE_RR` — where the whole effect is
/// rewriting `MERGE_RR`. With an actual conflict in flight both need
/// `ll_merge()`, so they `bail!` rather than record a wrong conflict id.
pub fn rerere(args: &[String]) -> Result<ExitCode> {
    let rest = &args[1..];

    // git.c short-circuits a bare `-h` before repository setup, so it works
    // outside a repository; every other form runs RUN_SETUP first.
    if rest.len() == 1 && rest[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    // git's parse-options collects non-options and keeps scanning, so flags may
    // appear before or after the verb; `--` ends option parsing.
    let mut positional: Vec<&str> = Vec::new();
    let mut no_more_opts = false;
    for a in rest {
        if !no_more_opts {
            if a == "--" {
                no_more_opts = true;
                continue;
            }
            if a == "-h" {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            if a == "--rerere-autoupdate" || a == "--no-rerere-autoupdate" {
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
        None => cmd_record(&repo),
        Some("status") => cmd_status(&repo),
        Some("remaining") => cmd_remaining(&repo),
        Some("diff") => cmd_diff(&repo),
        Some("clear") => cmd_clear(&repo),
        Some("gc") => cmd_gc(&repo),
        Some("forget") => cmd_forget(&repo, positional.len() < 2),
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
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `rerere_remaining()`: the recorded paths that the index does not show as
/// resolved, plus the conflicted paths rerere cannot track at all.
fn cmd_remaining(repo: &gix::Repository) -> Result<ExitCode> {
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let mut entries = read_rr(repo)?;

    let index = repo.open_index().context("index file corrupt")?;
    let cache = index.entries();
    let mut resolved: Vec<BString> = Vec::new();

    // Port of `check_one_conflict()`, driven over the index in cache order.
    let mut i = 0usize;
    while i < cache.len() {
        let name: BString = cache[i].path(&index).to_owned();

        if cache[i].stage_raw() == 0 {
            resolved.push(name);
            i += 1;
            continue;
        }

        // Skip the common ancestor (stage #1) entries.
        let mut j = i;
        while j < cache.len() && cache[j].stage_raw() == 1 {
            j += 1;
        }

        // Only a plain stage #2 + stage #3 pair of regular files is a conflict
        // rerere can record; anything else is "punted" and always reported.
        let three_staged = j + 1 < cache.len()
            && cache[j].stage_raw() == 2
            && cache[j + 1].stage_raw() == 3
            && cache[j + 1].path(&index) == cache[j].path(&index)
            && is_regular_file(cache[j].mode)
            && is_regular_file(cache[j + 1].mode);
        if !three_staged {
            insert_path(&mut entries, &name);
        }

        while j < cache.len() && cache[j].path(&index) == name {
            j += 1;
        }
        i = j;
    }

    let mut out = Vec::new();
    for e in &entries {
        if resolved.binary_search(&e.path).is_err() {
            out.extend_from_slice(&e.path);
            out.push(b'\n');
        }
    }
    std::io::stdout().write_all(&out)?;
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
            .ok_or_else(|| anyhow::anyhow!("this operation must be run in a work tree"))?;
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
    std::io::stdout().write_all(&out)?;
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
        if status.get(e.variant as usize).copied().unwrap_or(0) & both != both {
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

        for variant in 0..status.len() {
            // `prune_one()`: a postimage dates the resolution, a preimage alone
            // dates an unresolved conflict; neither means nothing to prune.
            let post = variant_path(&id_dir, variant as u32, "postimage");
            let pre = variant_path(&id_dir, variant as u32, "preimage");
            let (then, cutoff) = match mtime_secs(&post) {
                Some(t) => (t, cutoff_resolve),
                None => match mtime_secs(&pre) {
                    Some(t) => (t, cutoff_noresolve),
                    None => continue,
                },
            };
            if then < cutoff {
                unlink_rr_item(&id_dir, variant as u32);
                status[variant] = 0;
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

/// `rerere_forget()` — ported only where git's own work reduces to rewriting
/// `MERGE_RR` unchanged, i.e. when the index holds no conflict to forget.
fn cmd_forget(repo: &gix::Repository, no_paths: bool) -> Result<ExitCode> {
    if no_paths {
        eprintln!("warning: 'git rerere forget' without paths is deprecated");
    }
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let index = repo.open_index().context("index file corrupt")?;
    if index.entries().iter().any(|e| e.stage_raw() != 0) {
        bail!("{}", unported_ll_merge("forgetting a live conflict"));
    }

    let entries = read_rr(repo)?;
    write_rr(repo, &entries)?;
    Ok(ExitCode::SUCCESS)
}

/// `repo_rerere()`/`do_plain_rerere()` — ported only for the state git treats as
/// a no-op: nothing conflicted in the index and nothing already recorded, where
/// the sole effect is committing an empty `MERGE_RR`.
fn cmd_record(repo: &gix::Repository) -> Result<ExitCode> {
    if !is_rerere_enabled(repo)? {
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let index = repo.open_index().context("index file corrupt")?;
    if index.entries().iter().any(|e| e.stage_raw() != 0) {
        bail!("{}", unported_ll_merge("recording a conflict resolution"));
    }
    if !read_rr(repo)?.is_empty() {
        bail!("{}", unported_ll_merge("replaying a recorded resolution"));
    }

    write_rr(repo, &[])?;
    Ok(ExitCode::SUCCESS)
}

/// The single phrasing for every path that needs the unported merge substrate.
fn unported_ll_merge(what: &str) -> String {
    format!(
        "{what} needs git's ll_merge/xdl_merge conflict regeneration to derive the conflict id; \
         that substrate is not in the vendored gitoxide crates (ported: status, remaining, diff, clear, gc)"
    )
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

/// `rerere_path()`: variant 0 uses the bare name, others append `.<variant>`.
fn variant_path(id_dir: &Path, variant: u32, file: &str) -> PathBuf {
    if variant == 0 {
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
            bail!("corrupt MERGE_RR");
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
            let v: u32 = digits.parse().map_err(|_| anyhow::anyhow!("corrupt MERGE_RR"))?;
            (v, end)
        } else {
            (0, hexsz)
        };
        if rec.get(tab_at) != Some(&b'\t') {
            bail!("corrupt MERGE_RR");
        }
        let path = BString::from(&rec[tab_at + 1..]);

        // `string_list_insert()` keeps the list sorted and unique; a repeated
        // path keeps its position and takes the later id.
        match out.binary_search_by(|e| e.path.cmp(&path)) {
            Ok(i) => {
                out[i].hex = hex;
                out[i].variant = variant;
            }
            Err(i) => out.insert(i, RrEntry { path, hex, variant }),
        }
    }
    Ok(out)
}

/// `write_rr()`: serialise the records back and replace `MERGE_RR` atomically,
/// mirroring git's write-to-lock-then-commit.
fn write_rr(repo: &gix::Repository, entries: &[RrEntry]) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    for e in entries {
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
fn unlink_rr_item(id_dir: &Path, variant: u32) {
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

/// git omits the `,len` field when the hunk spans exactly one line.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    if len == 1 {
        let _ = write!(out, "{start}");
    } else {
        let _ = write!(out, "{start},{len}");
    }
}
