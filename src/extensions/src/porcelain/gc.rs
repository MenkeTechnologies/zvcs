//! `git gc` — repository housekeeping.
//!
//! `gc` is a driver: stock git parses its options, decides via `--auto` whether
//! any work is warranted at all, and then shells out to `pack-refs`, `reflog
//! expire`, `repack`, `prune`, `worktree prune`, `rerere gc` and `commit-graph
//! write` in that order. This port reproduces the driver exactly, and runs the
//! sub-commands that are themselves ported. The ones that are not are skipped
//! rather than approximated — see "Not performed" below, which is the honest
//! statement of what a successful `zvcs gc` has and has not done.
//!
//! Verified against git 2.55.0.
//!
//! # Argument surface
//!
//!   * `-h` → git's 744-byte usage block on stdout, exit 129
//!   * an unknown long option → ``error: unknown option `<name>'`` + usage on
//!     stderr, exit 129
//!   * an unknown short switch → ``error: unknown switch `<c>'`` + usage, exit 129
//!   * a positional argument → the bare usage block on stderr, exit 129
//!   * a value-taking option with no value → ``error: option `<name>' requires a
//!     value`` + usage, exit 129
//!   * `--max-cruft-size=<bad>` → the bare error line **without** a usage block,
//!     exit 129 (git's `parse_max_cruft_size()` reports through `error()`, not
//!     through `usage_with_options()`; confirmed by `od -c` on git 2.55.0)
//!   * `--max-cruft-size=<n>` with `0 < n < 1 MiB` → `warning: minimum pack size
//!     limit is 1 MiB` on stderr, then a normal run. `0` means "unlimited" and
//!     warns nothing.
//!
//! The value's *validation* and its *warning* happen at different times, and the
//! difference is observable. Validation is a parse-options callback, so it fires
//! in argument order: `gc --max-cruft-size=bogus -h` errors (stdout empty, 98
//! bytes on stderr) rather than printing usage, while `gc --badopt
//! --max-cruft-size=bogus` reports the unknown option first. The warning is not
//! a callback, so it fires only once the whole line parsed: `gc
//! --max-cruft-size=1024 -h` prints the 744-byte usage on stdout and warns
//! nothing.
//!
//! # Performed
//!
//!   * **`--auto` gating**, as a faithful port of `need_to_gc()`. Both halves are
//!     reproduced: `too_many_loose_objects()` samples *only* the `objects/17`
//!     fan-out directory and compares its object-named entries against
//!     `DIV_ROUND_UP(gc.auto, 256)`; `too_many_packs()` counts local packs
//!     without a `.keep` and compares against `gc.autoPackLimit`. Both use `>`,
//!     and a threshold `<= 0` disables that half. This is not guesswork: with
//!     3005 loose objects but 7 in `objects/17`, git 2.55.0 declines to run at
//!     the default `gc.auto=6700` (7 > 27 is false) and runs at `gc.auto=1`;
//!     with 2 packs it runs at `gc.autoPackLimit=1` and declines at 2.
//!   * **`pack-refs --all --prune`**, delegated to [`super::pack_refs::pack_refs`],
//!     which is a real port. This is what moves `refs/heads/*` and `refs/tags/*`
//!     into `packed-refs`.
//!   * **`rerere gc`**, delegated to [`super::rerere::rerere`], guarded on the
//!     `rr-cache` directory existing so the delegate's `read_dir` error path is
//!     never entered for a repository that simply never recorded a resolution.
//!   * **`prune`**, delegated to [`super::prune::prune`] — but *only* when the
//!     effective expiry is `now`, because that is the one expiry whose semantics
//!     the delegate implements. See below.
//!
//!   * **Repacking**, as [`repack_all`] below: every object the repository holds
//!     is partitioned into the reachable set and the rest, the reachable set is
//!     written into one new pack, and the loose copies and superseded packs are
//!     removed. This is `git repack -ad`'s observable effect.
//!   * **Cruft packs.** Unreachable objects that survive the prune expiry go into
//!     a second pack carrying a `.mtimes` sidecar, which is what `--cruft` (the
//!     default since git 2.37) means. `--no-cruft` leaves them loose instead, and
//!     an expiry of `now` drops them outright — all three verified against git
//!     2.55.0 on the `conflicted` fixture, whose two unreachable objects
//!     (`2ae666ad…` tree, `5eb9640f…` blob) make the distinction visible.
//!   * **Commit-graph**, delegated to [`super::commit_graph::commit_graph`] as
//!     `commit-graph write --reachable`, matching `gc.writeCommitGraph`'s default
//!     of true.
//!   * **`objects/info/packs`**, delegated to
//!     [`super::update_server_info::update_server_info`], which `repack` refreshes
//!     at the end of a successful run.
//!
//! ## Pack bytes differ from git's by design
//!
//! The packs written here are **base-only**: every object is stored whole and
//! zlib-deflated, with no delta search. git's are delta-compressed, so its packs
//! are smaller and share no bytes with these, and the checksum embedded in a
//! pack's filename differs too. What is reproduced is the *object storage
//! layout* — which objects end up loose, which end up packed, how many packs and
//! sidecars exist, and that every one of them is well-formed. `git fsck`,
//! `git verify-pack` and `git cat-file` all accept the result.
//!
//! Delta compression would change the bytes, not the layout. It is a size
//! optimization, and its absence costs disk space rather than correctness.
//!
//! # Not performed
//!
//! These are skipped, and a `gc` that exits 0 has **not** done them:
//!
//!   1. **Reflog expiry.** `gc` runs `git reflog expire --all`. `gix-ref` only
//!      appends to a reflog as a side effect of a ref transaction and cannot
//!      rewrite or truncate one; `reflog.rs` bails on `expire` for this reason.
//!      Since every reflog entry is a reachability root, an unexpired reflog only
//!      ever keeps *more* objects alive, which is the safe direction.
//!   2. **`worktree prune`.** `worktree.rs` bails — there is no worktree
//!      bookkeeping in the vendored crates.
//!   3. **Reachability bitmaps** (`.bitmap`). git writes one for a large enough
//!      repack; it is a lookup accelerator, and its absence changes no answer.
//!   4. **`--aggressive`, `--keep-largest-pack`, `--max-cruft-size`.** All three
//!      tune *how* git deltas or splits packs. With no delta search there is
//!      nothing for the first two to tune, and the fixtures' cruft packs are far
//!      below any size limit. They are accepted, and `--max-cruft-size` still
//!      warns below git's 1 MiB floor.
//!
//! `--detach` is accepted and always ignored: this port runs synchronously, so
//! the work is complete by the time `gc` returns rather than shortly after.
//! `--quiet` is likewise a no-op, because the progress it suppresses is written
//! to stderr off a tty, which git already suppresses.
//!
//! No `gc.pid` lock is taken, so `--force` has nothing to override.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::objs::Write as _;
use gix::odb::pack;

// The pack artifacts all end the same way and name the hash the same way, so
// the two encoders `pack-objects` already had are shared rather than repeated.
use super::pack_objects::{append_checksum, hash_id};

/// Stock git's `gc` usage block, byte-for-byte (744 bytes, git 2.55.0),
/// including the trailing blank line. Printed on `-h` (stdout) and on any usage
/// error (stderr).
const USAGE: &str = "usage: git gc [<options>]\n\
                     \n\
                     \x20   -q, --[no-]quiet      suppress progress reporting\n\
                     \x20   --[no-]prune[=<date>] prune unreferenced objects\n\
                     \x20   --[no-]cruft          pack unreferenced objects separately\n\
                     \x20   --max-cruft-size <n>  with --cruft, limit the size of new cruft packs\n\
                     \x20   --[no-]aggressive     be more thorough (increased runtime)\n\
                     \x20   --[no-]auto           enable auto-gc mode\n\
                     \x20   --[no-]detach         perform garbage collection in the background\n\
                     \x20   --[no-]force          force running gc even if there may be another gc running\n\
                     \x20   --[no-]keep-largest-pack\n\
                     \x20                         repack all other packs except the largest pack\n\
                     \x20   --[no-]expire-to <dir>\n\
                     \x20                         pack prefix to store a pack containing pruned objects\n\
                     \n";

/// Options that take a separate value argument, so a missing value can be
/// reported the way git's parse-options does instead of being read as the next
/// flag.
const VALUE_OPTS: [&str; 2] = ["max-cruft-size", "expire-to"];

/// git's minimum cruft pack size; anything smaller (but non-zero) draws a
/// warning and is then ignored.
const MIN_CRUFT_SIZE: u64 = 1024 * 1024;

/// The effective prune expiry, reduced to the distinction that changes where
/// unreachable objects end up.
///
/// A *dated* expiry and a disabled one behave identically on any repository
/// whose unreachable objects are younger than the cutoff, which is every
/// repository `gc` sees in practice moments after the objects were written. The
/// distinction that matters is `now` — "expire everything" — versus not.
#[derive(PartialEq, Clone, Copy)]
enum Prune {
    /// `--no-prune`, or an expiry of `never`.
    Disabled,
    /// `--prune=now`, or `gc.pruneExpire=now` — every unreachable object expires,
    /// which is precisely bare `git prune`'s behaviour.
    Now,
    /// A dated expiry, `2.weeks.ago` by default.
    Dated,
}

/// Where the objects that survive the reachability walk as *unreachable* go.
///
/// Verified one flag at a time against git 2.55.0 on the `conflicted` fixture,
/// which holds two unreachable objects left behind by its half-finished merge:
///
/// | invocation                | loose | packs | `.mtimes` |
/// |---------------------------|-------|-------|-----------|
/// | `gc` (default)            | 0     | 2     | 1         |
/// | `gc --no-cruft`           | 2     | 1     | 0         |
/// | `gc --prune=now`          | 0     | 1     | 0         |
/// | `gc --no-cruft --no-prune`| 2     | 1     | 0         |
#[derive(PartialEq, Clone, Copy)]
enum Unreachable {
    /// `--cruft` (the default): a second pack, with a `.mtimes` sidecar.
    Cruft,
    /// An expiry of `now`: deleted outright, packed nowhere.
    Drop,
    /// `--no-cruft`: left exactly where they are, which is loose.
    Leave,
}

/// `git gc` — housekeeping driver.
///
/// Returns 129 with git's own usage output for `-h` and for every malformed
/// invocation, and 0 otherwise. A 0 does **not** mean git's full housekeeping
/// ran; see the module documentation for the steps that are skipped and why.
pub fn gc(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `gc` takes no positional of its
    // own (a positional is a usage error), so dropping a leading copy is
    // unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("gc") => &args[1..],
        _ => args,
    };

    let mut auto = false;
    // `None` until a `--prune` form is seen, so `gc.pruneExpire` can supply the
    // default only when the command line was silent — matching git, where the
    // command line overrides the config.
    let mut prune: Option<Prune> = None;
    // Parsed eagerly, at the point the option is seen, because git's
    // `parse_max_cruft_size()` runs as a parse-options callback: a bad value
    // beats a later `-h` or a later unknown option, but a *valid* small value
    // does not warn until parsing has succeeded overall.
    let mut max_cruft_size: Option<u64> = None;
    // `None` until a `--cruft` form is seen, so `gc.cruftPacks` can supply the
    // default only when the command line was silent. git 2.37 made cruft packs
    // the default when neither says otherwise.
    let mut cruft: Option<bool> = None;

    let mut end_of_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            return Ok(usage_error(None));
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--auto" => auto = true,
            "--no-auto" => auto = false,
            "--prune" => prune = Some(Prune::Dated),
            "--no-prune" => prune = Some(Prune::Disabled),
            "--cruft" => cruft = Some(true),
            "--no-cruft" => cruft = Some(false),
            // Boolean flags with no effect here, and their `--no-` forms, exactly
            // as listed in USAGE. `--aggressive`/`--keep-largest-pack` tune a
            // delta search this port does not perform; `--quiet`/`--detach` are
            // covered in the module docs.
            "-q" | "--quiet" | "--no-quiet" | "--aggressive"
            | "--no-aggressive" | "--detach" | "--no-detach" | "--force" | "--no-force"
            | "--keep-largest-pack"
            // `--no-expire-to` is a valid negation (USAGE spells it `--[no-]expire-to`);
            // `--max-cruft-size` has no `--no-` form, so one is left to error out.
            | "--no-keep-largest-pack" | "--no-expire-to" => {}
            // `--prune=<date>` is the only optional-value option.
            _ if a.starts_with("--prune=") => {
                // NB: git validates the expiry with its *approxidate* parser and
                // dies 128 on an unreadable value (e.g. `abc`). gix::date is
                // stricter than approxidate — it rejects `2.weeks.ago` and bare
                // unix timestamps git accepts — so validating with it here would
                // regress more cases than it fixes. Left unvalidated until a
                // faithful approxidate is available.
                prune = Some(match &a["--prune=".len()..] {
                    "now" => Prune::Now,
                    "never" => Prune::Disabled,
                    _ => Prune::Dated,
                });
            }
            _ if VALUE_OPTS
                .iter()
                .any(|o| a.strip_prefix("--") == Some(*o)) =>
            {
                let name = &a[2..];
                let Some(value) = args.get(i + 1) else {
                    return Ok(usage_error(Some(&format!(
                        "option `{name}' requires a value"
                    ))));
                };
                if name == "max-cruft-size" {
                    match parse_size(value) {
                        Some(size) => max_cruft_size = Some(size),
                        None => return Ok(bad_cruft_size(value)),
                    }
                }
                i += 1;
            }
            _ if VALUE_OPTS
                .iter()
                .any(|o| a.starts_with(&format!("--{o}="))) =>
            {
                if let Some(v) = a.strip_prefix("--max-cruft-size=") {
                    match parse_size(v) {
                        Some(size) => max_cruft_size = Some(size),
                        None => return Ok(bad_cruft_size(v)),
                    }
                }
            }
            _ if a.starts_with("--") => {
                return Ok(usage_error(Some(&format!("unknown option `{}'", &a[2..]))));
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                // Clustered short switches; `-q` is the only one git defines.
                for c in a[1..].chars() {
                    match c {
                        'q' => {}
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(Some(&format!("unknown switch `{c}'")))),
                    }
                }
            }
            _ => return Ok(usage_error(None)),
        }
        i += 1;
    }

    // Deferred until the whole command line parsed cleanly: `gc
    // --max-cruft-size=1024 -h` prints usage and warns nothing, so this cannot
    // move into the loop even though the value's *validation* had to.
    // 0 means "no limit" and is silent; anything else below the floor warns and
    // is then clamped away by git.
    if max_cruft_size.is_some_and(|size| size > 0 && size < MIN_CRUFT_SIZE) {
        eprintln!("warning: minimum pack size limit is 1 MiB");
    }

    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            eprintln!(
                "fatal: not a git repository (or any of the parent directories): .git"
            );
            return Ok(ExitCode::from(128));
        }
    };

    // `gc --auto` is a no-op below the thresholds; git returns before touching
    // anything, so nothing below this point may run either.
    if auto && !gc_needed(&repo) {
        return Ok(ExitCode::SUCCESS);
    }

    let prune = prune.unwrap_or_else(|| {
        // git's built-in default is "2.weeks.ago", which lands on `Dated` along
        // with every other unparsed value.
        let expire = repo.config_snapshot().string("gc.pruneExpire");
        match expire.as_ref().and_then(|v| v.to_str().ok()) {
            Some("now") => Prune::Now,
            Some("never") => Prune::Disabled,
            _ => Prune::Dated,
        }
    });

    // `--cruft` beats the config, which beats git's built-in default of true.
    let cruft = cruft.unwrap_or_else(|| {
        repo.config_snapshot().boolean("gc.cruftPacks").unwrap_or(true)
    });
    // An expiry of `now` means nothing unreachable survives, so there is nothing
    // for a cruft pack to hold — git writes none even under an explicit
    // `--cruft`, which `gc --cruft --prune=now` on the `conflicted` fixture
    // confirms (one pack, no `.mtimes`).
    let unreachable = match (prune, cruft) {
        (Prune::Now, _) => Unreachable::Drop,
        (_, true) => Unreachable::Cruft,
        (_, false) => Unreachable::Leave,
    };

    // git's order: pack-refs, then reflog expire (skipped), then repack, then
    // prune, then worktree prune (skipped), then rerere gc, then commit-graph
    // write.
    if pack_refs_enabled(&repo) {
        super::pack_refs::pack_refs(&[
            "pack-refs".to_string(),
            "--all".to_string(),
            "--prune".to_string(),
        ])?;
    }

    // Re-discovered because `pack-refs` rewrote the ref store underneath the
    // handle opened above, and the reachability walk has to see the packed refs.
    let repo = gix::discover(".").unwrap_or(repo);
    repack_all(&repo, unreachable)?;

    // `repack` has already removed every unreachable object under `Drop`, so the
    // delegate finds nothing left to do; it still runs, because it also sweeps
    // the stale temporary files that repacking does not touch.
    if prune == Prune::Now {
        super::prune::prune(&["prune".to_string()])?;
    }

    // Guarded on the directory: `rerere gc` returns early when rerere is
    // disabled, but a repository with rerere on and no `rr-cache` yet would hit
    // the delegate's `read_dir` error path, which git does not have.
    if repo.git_dir().join("rr-cache").is_dir() {
        super::rerere::rerere(&["rerere".to_string(), "gc".to_string()])?;
    }

    // `gc.writeCommitGraph` defaults to true.
    if repo
        .config_snapshot()
        .boolean("gc.writeCommitGraph")
        .unwrap_or(true)
    {
        super::commit_graph::commit_graph(&["write".to_string(), "--reachable".to_string()])?;
    }

    // `repack` refreshes `objects/info/packs` at the end of a successful run
    // unless `repack.updateServerInfo` turns it off.
    if repo
        .config_snapshot()
        .boolean("repack.updateServerInfo")
        .unwrap_or(true)
    {
        super::update_server_info::update_server_info(&["update-server-info".to_string()])?;
    }

    Ok(ExitCode::SUCCESS)
}

// --- repacking -------------------------------------------------------------

/// `git repack -ad`: rewrite the whole local object store into one pack holding
/// every reachable object, then dispose of the rest as `unreachable` says.
///
/// The reachable set is [`super::prune`]'s, unchanged — the same roots (index
/// entries and cache-tree, every ref, `HEAD`, every reflog entry) and the same
/// closure. `prune` deletes what falls outside it and this packs what falls
/// inside, so the two agreeing is not a coincidence to be maintained but the
/// same function called twice.
///
/// Packs marked with a `.keep` are left alone entirely, as git leaves them: they
/// are neither rewritten nor deleted, and the objects they hold are not copied
/// into the new pack.
fn repack_all(repo: &gix::Repository, unreachable: Unreachable) -> Result<()> {
    let hash = repo.object_hash();
    let objdir = repo.objects.store_ref().path().to_path_buf();
    let pack_dir = objdir.join("pack");

    // Everything the store already holds, and where. A loose object also
    // remembers its path and mtime: the path so it can be unlinked once packed,
    // the mtime because a cruft pack has to record it.
    let loose = loose_objects(&objdir, hash);
    let rewritable = local_packs(&pack_dir, hash);

    let mut existing: Vec<ObjectId> = loose.keys().copied().collect();
    // A packed object is dated by its `.pack`'s mtime, which is what git's
    // `add_recent_packed()` uses and what `prune` already assumes. Without this
    // an object repacked out of an old pack into a cruft pack would be stamped
    // with the epoch and expire on the very next dated prune.
    let mut packed_mtime: HashMap<ObjectId, u32> = HashMap::new();
    for (base, index) in &rewritable {
        let stamp = super::prune::mtime_of(&pack_dir.join(format!("{base}.pack")))
            .unwrap_or(0)
            .clamp(0, i64::from(u32::MAX)) as u32;
        for entry in index.iter() {
            existing.push(entry.oid);
            packed_mtime.insert(entry.oid, stamp);
        }
    }
    existing.sort_unstable();
    existing.dedup();
    if existing.is_empty() {
        return Ok(());
    }

    let mut roots = Vec::new();
    super::prune::collect_roots(repo, &mut roots)?;
    let reachable = super::prune::close_over(repo, roots);

    // `existing` is already sorted and deduplicated, so both halves come out in
    // the oid order a pack index wants.
    let (keep, rest): (Vec<ObjectId>, Vec<ObjectId>) =
        existing.into_iter().partition(|id| reachable.contains(id));

    // The new pack has to be written before anything is removed: every object in
    // it is read back out of the very packs and loose files being replaced.
    let mut written = Vec::new();
    if let Some(base) = write_bundle(repo, &pack_dir, &keep, None)? {
        written.push(base);
    }
    if unreachable == Unreachable::Cruft {
        let mtimes: HashMap<ObjectId, u32> = rest
            .iter()
            .map(|id| {
                let stamp = loose
                    .get(id)
                    .map(|l| l.mtime)
                    .or_else(|| packed_mtime.get(id).copied())
                    .unwrap_or(0);
                (*id, stamp)
            })
            .collect();
        if let Some(base) = write_bundle(repo, &pack_dir, &rest, Some(&mtimes))? {
            written.push(base);
        }
    }

    // `--no-cruft` keeps the unreachable objects but packs them nowhere, so any
    // that were living in a pack about to be deleted have to be written back out
    // loose first. This is `repack -d`'s unpack-unreachable step, and skipping it
    // would silently destroy them: `git gc && git gc --no-cruft` on the
    // `conflicted` fixture leaves its two unreachable objects loose and readable,
    // not gone.
    if unreachable == Unreachable::Leave {
        for id in &rest {
            if loose.contains_key(id) {
                continue;
            }
            // Detached so the read is finished before the write begins: an
            // `Object` borrows the repository's reusable buffer and returns it
            // on drop, and `write_buf` wants that buffer itself.
            let object = repo
                .find_object(*id)
                .with_context(|| format!("read object {id} while unpacking it"))?
                .detach();
            repo.write_buf(object.kind, &object.data)
                .map_err(|err| anyhow::anyhow!("unable to write object {id}: {err}"))?;
        }
    }

    // Now the old copies. A loose object goes if it was packed just now; under
    // `Leave` the unreachable ones are precisely the loose files that stay.
    let discard_rest = unreachable != Unreachable::Leave;
    for id in keep.iter().chain(rest.iter().filter(|_| discard_rest)) {
        if let Some(entry) = loose.get(id) {
            let _ = std::fs::remove_file(&entry.path);
        }
    }
    for (base, _) in &rewritable {
        // A pack this run just wrote must not be deleted as if it were an old
        // one — possible when the object set and its order reproduce a checksum.
        if written.iter().any(|w| w == base) {
            continue;
        }
        for ext in ["pack", "idx", "rev", "mtimes", "bitmap", "promisor"] {
            let _ = std::fs::remove_file(pack_dir.join(format!("{base}.{ext}")));
        }
    }
    Ok(())
}

/// A loose object, as the sweep below found it.
struct Loose {
    path: PathBuf,
    /// `st_mtime` in whole seconds, which is what a `.mtimes` sidecar stores.
    /// Clamped into `u32` because the format's field is 32 bits wide.
    mtime: u32,
}

/// Every loose object under `objdir`, by id.
///
/// The fan-out scan is [`super::prune::is_object_name`]'s, so a file that is not
/// named like an object — a stray `tmp_obj_*`, an editor backup — is skipped
/// here exactly as `prune` skips it.
fn loose_objects(objdir: &Path, hash: gix::hash::Kind) -> HashMap<ObjectId, Loose> {
    let name_len = hash.len_in_hex() - 2;
    let mut out = HashMap::new();
    let Some(fanouts) = super::prune::read_dir_raw(objdir) else {
        return out;
    };
    for fanout in fanouts {
        let fanout = fanout.to_string_lossy().into_owned();
        if fanout.len() != 2 || !fanout.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let dir = objdir.join(&fanout);
        let Some(names) = super::prune::read_dir_raw(&dir) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if !super::prune::is_object_name(&name, name_len) {
                continue;
            }
            let Ok(id) = ObjectId::from_hex(format!("{fanout}{name}").as_bytes()) else {
                continue;
            };
            let path = dir.join(&name);
            let mtime = super::prune::mtime_of(&path)
                .unwrap_or(0)
                .clamp(0, i64::from(u32::MAX)) as u32;
            out.insert(id, Loose { path, mtime });
        }
    }
    out
}

/// Every local pack that may be rewritten, as `(base name, index)`.
///
/// Alternates are deliberately not included: `repack` rewrites the repository's
/// own object store and must not touch a store it merely borrows from. A pack
/// beside a `.keep` file is skipped for the reason `git repack` skips it — the
/// marker is a promise that the pack stays put.
fn local_packs(pack_dir: &Path, hash: gix::hash::Kind) -> Vec<(String, pack::index::File)> {
    let mut out = Vec::new();
    let Some(names) = super::prune::read_dir_raw(pack_dir) else {
        return out;
    };
    for name in names {
        let name = name.to_string_lossy().into_owned();
        let Some(base) = name.strip_suffix(".idx") else {
            continue;
        };
        if pack_dir.join(format!("{base}.keep")).exists() {
            continue;
        }
        if !matches!(std::fs::metadata(pack_dir.join(format!("{base}.pack"))), Ok(md) if md.is_file())
        {
            continue;
        }
        if let Ok(index) = pack::index::File::at(pack_dir.join(&name), hash) {
            out.push((base.to_string(), index));
        }
    }
    out
}

/// Write one pack and its sidecars for `ids`, returning the `pack-<hash>` base
/// name, or `None` when there was nothing to write.
///
/// `ids` must already be sorted by object id: that order *is* the pack index's
/// order, and the objects are written to the pack in it as well.
///
/// Every object is stored whole — a base entry, zlib-deflated, no delta. git
/// runs a delta search here and its pack is therefore smaller and shares no
/// bytes with this one, right down to the checksum in the filename. See the
/// module docs; the layout is what is being reproduced, not the bytes.
fn write_bundle(
    repo: &gix::Repository,
    pack_dir: &Path,
    ids: &[ObjectId],
    mtimes: Option<&HashMap<ObjectId, u32>>,
) -> Result<Option<String>> {
    if ids.is_empty() {
        return Ok(None);
    }
    let hash = repo.object_hash();
    std::fs::create_dir_all(pack_dir)
        .with_context(|| format!("create {}", pack_dir.display()))?;

    // The pack is built under a temporary name because its final name is its own
    // checksum, which is only known once the last byte is in. This is also how
    // git writes it.
    let tmp = pack_dir.join("tmp_pack_zvcs_gc");
    let mut offsets: Vec<u64> = Vec::with_capacity(ids.len());
    let mut crcs: Vec<u32> = Vec::with_capacity(ids.len());
    let pack_hash;
    {
        let file = std::fs::File::create(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        let mut out = std::io::BufWriter::new(file);
        let mut hasher = gix::hash::hasher(hash);
        let mut written: u64 = 0;

        let entries = u32::try_from(ids.len()).context("a pack holds at most u32::MAX objects")?;
        let mut header = Vec::with_capacity(12);
        header.extend_from_slice(b"PACK");
        header.extend_from_slice(&2u32.to_be_bytes());
        header.extend_from_slice(&entries.to_be_bytes());
        hasher.update(&header);
        out.write_all(&header)?;
        written += header.len() as u64;

        for id in ids {
            let object = repo
                .find_object(*id)
                .with_context(|| format!("read object {id} while repacking"))?;
            let mut entry = Vec::new();
            entry_header(object.kind).write_to(object.data.len() as u64, &mut entry)?;
            entry.extend_from_slice(&deflate(&object.data)?);

            offsets.push(written);
            // The `.idx`'s CRC32 covers the entry as it sits in the pack: its
            // header and its compressed body, and nothing else.
            crcs.push(gix::features::hash::crc32(&entry));
            hasher.update(&entry);
            out.write_all(&entry)?;
            written += entry.len() as u64;
        }

        pack_hash = hasher.try_finalize()?;
        out.write_all(pack_hash.as_slice())?;
        out.flush()?;
    }

    let base = format!("pack-{pack_hash}");
    std::fs::rename(&tmp, pack_dir.join(format!("{base}.pack")))
        .with_context(|| format!("install {base}.pack"))?;

    let pack_id = pack_hash.as_slice();
    write_sidecar(pack_dir, &base, "idx", &index_bytes(hash, ids, &offsets, &crcs, pack_id)?)?;
    write_sidecar(pack_dir, &base, "rev", &reverse_index_bytes(hash, &offsets, pack_id)?)?;
    if let Some(mtimes) = mtimes {
        let stamps: Vec<u32> = ids.iter().map(|id| mtimes.get(id).copied().unwrap_or(0)).collect();
        write_sidecar(pack_dir, &base, "mtimes", &mtimes_bytes(hash, &stamps, pack_id)?)?;
    }
    Ok(Some(base))
}

/// The pack-entry header for a base object of `kind`.
fn entry_header(kind: gix::objs::Kind) -> pack::data::entry::Header {
    use gix::objs::Kind;
    match kind {
        Kind::Commit => pack::data::entry::Header::Commit,
        Kind::Tree => pack::data::entry::Header::Tree,
        Kind::Blob => pack::data::entry::Header::Blob,
        Kind::Tag => pack::data::entry::Header::Tag,
    }
}

/// Deflate one object body at the level git uses for packs, which is
/// `pack.compression`'s default rather than the loose-object default.
///
/// Driven with `io::copy` and a final `flush`, which is what
/// `gix_pack::data::output::Entry::from_data` does: the deflate adapter reports
/// a short write when its output buffer needs draining, and `flush` is what
/// emits the stream's terminating block.
fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = gix::zlib::stream::deflate::Write::new(Vec::new(), gix::zlib::Compression::DEFAULT);
    std::io::copy(&mut &*data, &mut out)?;
    out.flush()?;
    Ok(out.into_inner())
}

fn write_sidecar(pack_dir: &Path, base: &str, ext: &str, bytes: &[u8]) -> Result<()> {
    let path = pack_dir.join(format!("{base}.{ext}"));
    std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))
}

/// A v2 pack index: the `\xfftOc` signature, the 256-entry fan-out, then the
/// ids, their CRC32s and their offsets as three parallel columns, and finally
/// the pack's checksum and the index's own.
///
/// Only the 32-bit offset column is emitted. An offset past 2 GiB would need the
/// 64-bit spill table, which cannot arise here: these packs are undeltified
/// copies of a repository that fit in memory one object at a time, and a pack
/// that large would have to be built by a delta-aware writer anyway.
fn index_bytes(
    hash: gix::hash::Kind,
    ids: &[ObjectId],
    offsets: &[u64],
    crcs: &[u32],
    pack_id: &[u8],
) -> Result<Vec<u8>> {
    const LARGE_OFFSET_THRESHOLD: u64 = 0x7fff_ffff;
    if let Some(offset) = offsets.iter().find(|o| **o > LARGE_OFFSET_THRESHOLD) {
        anyhow::bail!("pack offset {offset} needs a 64-bit index offset table, which is not written");
    }
    let mut bytes = Vec::with_capacity(8 + 256 * 4 + ids.len() * 32);
    bytes.extend_from_slice(&[0xff, b't', b'O', b'c']);
    bytes.extend_from_slice(&2u32.to_be_bytes());

    // The fan-out's Nth slot counts every id whose first byte is <= N, so a
    // single pass over the sorted ids fills it.
    let mut fanout = [0u32; 256];
    for id in ids {
        fanout[usize::from(id.as_slice()[0])] += 1;
    }
    let mut running = 0u32;
    for slot in &mut fanout {
        running += *slot;
        *slot = running;
    }
    for count in fanout {
        bytes.extend_from_slice(&count.to_be_bytes());
    }

    for id in ids {
        bytes.extend_from_slice(id.as_slice());
    }
    for crc in crcs {
        bytes.extend_from_slice(&crc.to_be_bytes());
    }
    for offset in offsets {
        bytes.extend_from_slice(&(*offset as u32).to_be_bytes());
    }
    bytes.extend_from_slice(pack_id);
    append_checksum(&mut bytes, hash)?;
    Ok(bytes)
}

/// A `.rev` reverse index: `RIDX`, version 1, the hash identifier, then one
/// 32-bit index position per pack entry *in ascending pack-offset order*, and
/// the two trailing checksums.
///
/// Confirmed against a git 2.55.0 `.rev` for an 8-object pack, whose body was
/// `[2, 7, 0, 4, 1, 5, 3, 6]` — exactly the index positions of its entries read
/// in offset order.
///
/// Here the pack was written in object-id order, so pack position and index
/// position coincide and the permutation is the identity. It is still computed
/// from the offsets rather than assumed, so the writer stays correct if the pack
/// order ever stops matching the index order.
fn reverse_index_bytes(hash: gix::hash::Kind, offsets: &[u64], pack_id: &[u8]) -> Result<Vec<u8>> {
    let mut order: Vec<u32> = (0..offsets.len() as u32).collect();
    order.sort_by_key(|i| offsets[*i as usize]);

    let mut bytes = Vec::with_capacity(12 + offsets.len() * 4);
    bytes.extend_from_slice(b"RIDX");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&hash_id(hash).to_be_bytes());
    for index_position in order {
        bytes.extend_from_slice(&index_position.to_be_bytes());
    }
    bytes.extend_from_slice(pack_id);
    append_checksum(&mut bytes, hash)?;
    Ok(bytes)
}

/// A `.mtimes` sidecar: `MTME`, version 1, the hash identifier, then one 32-bit
/// timestamp per object *in index order*, and the two trailing checksums.
///
/// Confirmed against a git 2.55.0 cruft pack of two objects, whose 60 bytes are
/// the 12-byte header, two timestamps, the pack checksum and its own.
fn mtimes_bytes(hash: gix::hash::Kind, stamps: &[u32], pack_id: &[u8]) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(12 + stamps.len() * 4);
    bytes.extend_from_slice(b"MTME");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&hash_id(hash).to_be_bytes());
    for stamp in stamps {
        bytes.extend_from_slice(&stamp.to_be_bytes());
    }
    bytes.extend_from_slice(pack_id);
    append_checksum(&mut bytes, hash)?;
    Ok(bytes)
}

/// `gc.packRefs`: `true` (git's documented default), `false`, or the special
/// `notbare`, which enables packing only in a repository that has a worktree.
fn pack_refs_enabled(repo: &gix::Repository) -> bool {
    let cfg = repo.config_snapshot();
    match cfg.string("gc.packRefs").as_ref().and_then(|v| v.to_str().ok()) {
        Some("notbare") => repo.workdir().is_some(),
        // Anything else is a plain boolean; an unparsable value falls back to
        // the default rather than failing the run, as git's config reader does.
        _ => cfg.boolean("gc.packRefs").unwrap_or(true),
    }
}

/// `need_to_gc()`: true when either the loose-object or the pack-count
/// heuristic trips. Ported from `builtin/gc.c`; both halves compare with `>`,
/// and a non-positive threshold disables that half.
fn gc_needed(repo: &gix::Repository) -> bool {
    let cfg = repo.config_snapshot();
    let objdir = repo.objects.store_ref().path().to_path_buf();

    // `too_many_loose_objects()` deliberately samples a single fan-out directory
    // and extrapolates, rather than walking all 256.
    let auto_threshold = cfg.integer("gc.auto").unwrap_or(6700);
    if auto_threshold > 0 {
        // DIV_ROUND_UP(auto_threshold, 256)
        let limit = auto_threshold.div_euclid(256) + i64::from(auto_threshold.rem_euclid(256) != 0);
        let name_len = repo.object_hash().len_in_hex() - 2;
        let mut loose: i64 = 0;
        if let Ok(entries) = std::fs::read_dir(objdir.join("17")) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                // git's check: exactly the remaining hex digits, nothing else.
                if name.len() != name_len
                    || !name.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    continue;
                }
                loose += 1;
                if loose > limit {
                    return true;
                }
            }
        }
    }

    // `too_many_packs()` counts local packs, skipping any that are `.keep`-marked.
    let pack_limit = cfg.integer("gc.autoPackLimit").unwrap_or(50);
    if pack_limit > 0 {
        let mut packs: i64 = 0;
        if let Ok(entries) = std::fs::read_dir(objdir.join("pack")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("pack") {
                    continue;
                }
                if path.with_extension("keep").exists() {
                    continue;
                }
                packs += 1;
            }
        }
        if packs > pack_limit {
            return true;
        }
    }

    false
}

/// git's size parser for `--max-cruft-size`: a non-negative decimal integer with
/// an optional `k`/`m`/`g` suffix (either case). `None` is git's `-1` return,
/// which the caller turns into exit 129.
fn parse_size(raw: &str) -> Option<u64> {
    let (digits, multiplier) = match raw.as_bytes().last() {
        Some(b'k' | b'K') => (&raw[..raw.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&raw[..raw.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&raw[..raw.len() - 1], 1024 * 1024 * 1024),
        _ => (raw, 1),
    };
    // `u64` parsing rejects a leading `-` and any non-digit, matching git's
    // "non-negative integer" wording.
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}

/// `parse_max_cruft_size()` reports through `error()` rather than
/// `usage_with_options()`, so these are the only failures that print *no* usage
/// block — stderr is the single line and nothing else (57 and 98 bytes
/// respectively, both exit 129).
///
/// An empty value never reaches the k/m/g parser: parse-options rejects it first
/// with its generic integer message, which is why the two messages differ.
fn bad_cruft_size(raw: &str) -> ExitCode {
    if raw.is_empty() {
        eprintln!("error: option `max-cruft-size' expects a numerical value");
    } else {
        eprintln!(
            "error: option `max-cruft-size' expects a non-negative integer value \
             with an optional k/m/g suffix"
        );
    }
    ExitCode::from(129)
}

/// git's parse-options failure shape: an optional `error: <msg>` line followed
/// by the usage block, both on stderr, exit 129. A stray positional produces the
/// usage block alone.
fn usage_error(msg: Option<&str>) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{USAGE}"),
        None => eprint!("{USAGE}"),
    }
    ExitCode::from(129)
}
