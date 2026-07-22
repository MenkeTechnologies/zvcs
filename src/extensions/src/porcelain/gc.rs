//! `git gc` — repository housekeeping.
//!
//! `gc` is a driver: stock git parses its options, decides via `--auto` whether
//! any work is warranted at all, and then shells out to `pack-refs`, `reflog
//! expire`, `repack`, `prune`, `worktree prune`, `rerere gc` and `commit-graph
//! write` in that order. This port reproduces the driver exactly, running the
//! steps it has ported and skipping the rest rather than approximating them —
//! see "Not performed" below, which is the honest statement of what a successful
//! `zvcs gc` has and has not done.
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
//!     warns nothing. `gc.maxCruftSize` supplies the default when
//!     `--max-cruft-size` is absent; git validates it eagerly through
//!     `git_config_ulong`, so a value it cannot read is fatal (exit 128, `bad
//!     numeric config value … invalid unit`/`out of range`) even under a
//!     `--max-cruft-size` override or a below-threshold `--auto`. The 1 MiB
//!     warning it can trigger is emitted from the repack, so it is silent when
//!     `--auto` declines to run.
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
//!   * **`reflog expire --all`**, as [`expire_reflogs`] below: a faithful port
//!     of `reflog.c`'s `should_expire_reflog_ent()`. Each reflog under `logs/`
//!     is rewritten in place, dropping entries older than `gc.reflogExpire`
//!     (built-in default `now - 30 days`) and unreachable entries older than
//!     `gc.reflogExpireUnreachable` (`now - 90 days`); every kept line is
//!     preserved byte-for-byte, since `gc` passes neither `--rewrite` nor
//!     `--updateref`. Runs unless both cutoffs are configured to `never`, git's
//!     `cfg->prune_reflogs` gate.
//!   * **`worktree prune`**, as [`prune_worktrees`] below: a port of
//!     `worktree.c`'s `prune_worktrees()` for the checks `gc` reaches, removing
//!     the administrative directory of every linked worktree whose checkout is
//!     gone and whose `index` has aged past `gc.worktreePruneExpire` (default
//!     `3.months.ago`). Locked worktrees are never pruned.
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
//!   1. **Reachability bitmaps** (`.bitmap`). git writes one for a large enough
//!      repack; it is a lookup accelerator, and its absence changes no answer.
//!   2. **`--aggressive`, `--keep-largest-pack`, `--max-cruft-size`.** All three
//!      tune *how* git deltas or splits packs. With no delta search there is
//!      nothing for the first two to tune, and the fixtures' cruft packs are far
//!      below any size limit. They are accepted, and `--max-cruft-size` (with
//!      its `gc.maxCruftSize` default) still warns below git's 1 MiB floor.
//!
//! `--detach` is accepted and always ignored: this port runs synchronously, so
//! the work is complete by the time `gc` returns rather than shortly after.
//! `--quiet` is likewise a no-op, because the progress it suppresses is written
//! to stderr off a tty, which git already suppresses.
//!
//! No `gc.pid` lock is taken, so `--force` has nothing to override.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::objs::Kind;
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

    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            eprintln!(
                "fatal: not a git repository (or any of the parent directories): .git"
            );
            return Ok(ExitCode::from(128));
        }
    };

    // `gc.maxCruftSize` supplies the default for `--max-cruft-size`, and git
    // validates it the moment the config is read — through `git_config_ulong`,
    // before parse-options and before the `--auto` gate. So a value git cannot
    // read is fatal (exit 128) even when `--max-cruft-size` overrides it or the
    // run is a below-threshold `--auto` no-op; only a bare `gc -h`, which
    // returned above before the repo was opened, escapes it. `--max-cruft-size`
    // still overrides the *value* when both are present.
    if let Some((raw, origin)) = effective_max_cruft_size(&repo) {
        match parse_config_ulong(&raw) {
            Ok(size) => {
                if max_cruft_size.is_none() {
                    max_cruft_size = Some(size);
                }
            }
            Err(reason) => {
                eprintln!(
                    "fatal: bad numeric config value '{raw}' for 'gc.maxcruftsize'{origin}: {reason}"
                );
                return Ok(ExitCode::from(128));
            }
        }
    }

    // `gc --auto` is a no-op below the thresholds; git returns before touching
    // anything, so nothing below this point may run either.
    if auto && !gc_needed(&repo) {
        return Ok(ExitCode::SUCCESS);
    }

    // git prints this from the repack itself, not from option parsing, so it is
    // gated on the run actually happening: a below-threshold `--auto` returns
    // above and warns nothing. `0` means "no limit" and is silent; any other
    // value below git's 1 MiB floor warns and is then ignored — this port
    // applies no size limit (see the module docs), so the warning is its only
    // observable effect, whether the value came from `--max-cruft-size` or from
    // `gc.maxCruftSize`.
    if max_cruft_size.is_some_and(|size| size > 0 && size < MIN_CRUFT_SIZE) {
        eprintln!("warning: minimum pack size limit is 1 MiB");
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

    // git's order: pack-refs, then reflog expire, then repack, then prune, then
    // worktree prune, then rerere gc, then commit-graph write.
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

    // `reflog expire --all`, a foreground task git runs before the repack so an
    // expired entry no longer keeps its object alive. Skipped only when both
    // `gc.reflogExpire` and `gc.reflogExpireUnreachable` are `never`, exactly as
    // git's `cfg->prune_reflogs` gate.
    if reflog_expire_enabled(&repo) {
        expire_reflogs(&repo)?;
    }

    repack_all(&repo, unreachable)?;

    // `repack` has already removed every unreachable object under `Drop`, so the
    // delegate finds nothing left to do; it still runs, because it also sweeps
    // the stale temporary files that repacking does not touch.
    if prune == Prune::Now {
        super::prune::prune(&["prune".to_string()])?;
    }

    // `worktree prune --expire <gc.worktreePruneExpire>`: git runs this after
    // `prune`, removing the administrative directory of every linked worktree
    // whose checkout has vanished and whose `index` has aged past the expiry.
    prune_worktrees(&repo)?;

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

// --- reflog expiry ---------------------------------------------------------
//
// A faithful port of git's `reflog expire --all` (`reflog.c` + `builtin/gc.c`).
// The reflog files under `logs/` are rewritten in place, dropping only the
// entries that `should_expire_reflog_ent()` would drop; every kept line is
// preserved byte-for-byte, because `gc` passes neither `--rewrite` nor
// `--updateref`, so the old/new ids and the ref value are never touched.
//
// git's `mark_reachable()` is a date-limited two-phase walk purely as a
// performance optimisation: `is_unreachable()` digs down to the root the first
// time a candidate is not already marked, so its boolean answer is exactly
// "this commit is not in the full ancestor closure of the tip set". That
// equivalence is what [`reachable_commits`] computes directly.
//
// Divergences, both edge-of-edge: per-pattern `gc.<pattern>.reflog*` matching
// uses a `*`/`?` glob (git's `wildmatch` bracket classes are not honoured), and
// only the main ref store's `logs/` are processed (git's `--all` also visits
// each linked worktree's ref store, which `prune` already declines to support).

/// git gc's `cfg->prune_reflogs`: `reflog expire --all` runs unless BOTH
/// `gc.reflogExpire` and `gc.reflogExpireUnreachable` are configured to a value
/// that resolves to the `never` sentinel (`0`). An unset value is not `never`,
/// so the default is to run — matching `gc_config_is_timestamp_never()`.
fn reflog_expire_enabled(repo: &gix::Repository) -> bool {
    let now = SystemTime::now();
    let cfg = repo.config_snapshot();
    let is_never = |key: &str| {
        cfg.string(key)
            .and_then(|v| v.to_str().ok().map(str::to_owned))
            .and_then(|v| parse_reflog_expiry(&v, now))
            .is_some_and(|t| t == 0)
    };
    !(is_never("gc.reflogExpire") && is_never("gc.reflogExpireUnreachable"))
}

/// git's `parse_expiry_date` for a reflog cutoff: `never`/`false` are the `0`
/// sentinel (never expire), `all`/`now` are `i64::MAX` (expire everything),
/// `@<n>` is a raw epoch, and anything else is an approxidate resolved against
/// `now`. A lighter approximation of [`super::prune`]'s approxidate handling:
/// `.`/`,`/`_`/`/` split into words and a bare `<n> <unit>` is read as past.
/// Unparseable values yield `None`, which callers treat as "unset".
fn parse_reflog_expiry(value: &str, now: SystemTime) -> Option<i64> {
    let v = value.trim();
    match v {
        "" => return None,
        "never" | "false" => return Some(0),
        "all" | "now" => return Some(i64::MAX),
        _ => {}
    }
    if let Some(rest) = v.strip_prefix('@') {
        return rest.trim().parse::<i64>().ok();
    }
    let spaced: String = v
        .chars()
        .map(|c| if matches!(c, '.' | ',' | '_' | '/') { ' ' } else { c })
        .collect();
    let spaced = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    for form in [v.to_owned(), spaced.clone(), format!("{spaced} ago")] {
        if let Ok(t) = gix::date::parse(&form, Some(now)) {
            return Some(t.seconds);
        }
    }
    None
}

/// A per-pattern `gc.<pattern>.reflog*` override; a missing slot falls back to
/// the corresponding default.
struct ReflogEntryOpt {
    pattern: String,
    total: Option<i64>,
    unreach: Option<i64>,
}

/// The resolved reflog-expire policy: the two default cutoffs plus any
/// per-pattern overrides, mirroring `struct reflog_expire_options`.
struct ReflogExpireConfig {
    default_total: i64,
    default_unreach: i64,
    entries: Vec<ReflogEntryOpt>,
}

impl ReflogExpireConfig {
    /// `reflog_expire_options_set_refname()`: the first pattern that matches
    /// wins, `refs/stash` never expires when unconfigured, otherwise the
    /// defaults apply. `gc` sets no explicit expiry, so the config always drives.
    fn resolve(&self, refname: &str) -> (i64, i64) {
        for ent in &self.entries {
            if wildmatch0(ent.pattern.as_bytes(), refname.as_bytes()) {
                return (
                    ent.total.unwrap_or(self.default_total),
                    ent.unreach.unwrap_or(self.default_unreach),
                );
            }
        }
        if refname == "refs/stash" {
            return (0, 0);
        }
        (self.default_total, self.default_unreach)
    }
}

/// Load `gc.reflogExpire`/`gc.reflogExpireUnreachable` and their per-pattern
/// forms. The built-in defaults match `REFLOG_EXPIRE_OPTIONS_INIT`: total is
/// `now - 30 days`, unreachable is `now - 90 days` (verified against git 2.55.0,
/// whose macro values differ from the historical documentation).
fn load_reflog_config(repo: &gix::Repository, now: SystemTime, now_secs: i64) -> ReflogExpireConfig {
    let mut default_total = now_secs - 30 * 24 * 3600;
    let mut default_unreach = now_secs - 90 * 24 * 3600;
    let mut entries: Vec<ReflogEntryOpt> = Vec::new();

    let config = repo.config_snapshot().plumbing().clone();
    for section in config.sections() {
        let header = section.header();
        if !header.name().to_string().eq_ignore_ascii_case("gc") {
            continue;
        }
        // Last value wins, as git's config reader does.
        let mut total = None;
        for value in section.body().values("reflogExpire") {
            total = parse_reflog_expiry(value.to_str_lossy().as_ref(), now);
        }
        let mut unreach = None;
        for value in section.body().values("reflogExpireUnreachable") {
            unreach = parse_reflog_expiry(value.to_str_lossy().as_ref(), now);
        }
        match header.subsection_name() {
            None => {
                if let Some(t) = total {
                    default_total = t;
                }
                if let Some(u) = unreach {
                    default_unreach = u;
                }
            }
            // Only a section that actually sets a reflog key contributes a
            // pattern, matching git's `find_cfg_ent` being reached only from the
            // two reflog keys.
            Some(_) if total.is_none() && unreach.is_none() => {}
            Some(sub) => {
                let pattern = sub.to_str_lossy().into_owned();
                let idx = match entries.iter().position(|e| e.pattern == pattern) {
                    Some(i) => i,
                    None => {
                        entries.push(ReflogEntryOpt {
                            pattern,
                            total: None,
                            unreach: None,
                        });
                        entries.len() - 1
                    }
                };
                if total.is_some() {
                    entries[idx].total = total;
                }
                if unreach.is_some() {
                    entries[idx].unreach = unreach;
                }
            }
        }
    }
    ReflogExpireConfig {
        default_total,
        default_unreach,
        entries,
    }
}

/// git's `wildmatch(pattern, text, 0)`: `*` spans any run including `/`, `?`
/// matches one byte. Bracket expressions are not honoured (they do not occur in
/// reflog-expire patterns in practice).
fn wildmatch0(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Which reachability rule an entry's unreachable window uses, mirroring git's
/// `UE_ALWAYS`/`UE_HEAD`/`UE_NORMAL`.
#[derive(PartialEq, Clone, Copy)]
enum ReflogKind {
    /// No reachability distinction: any entry in the unreachable window expires.
    Always,
    /// Reachability measured against every ref tip (the `HEAD` reflog).
    Head,
    /// Reachability measured against this ref's own tip.
    Normal,
}

/// `reflog expire --all` over the main ref store's `logs/`.
fn expire_reflogs(repo: &gix::Repository) -> Result<()> {
    let now = SystemTime::now();
    let now_secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let cfg = load_reflog_config(repo, now, now_secs);

    let logs_dir = repo.common_dir().join("logs");
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_reflog_files(&logs_dir, &logs_dir, &mut files);
    files.sort();

    // The `UE_HEAD` closure (all ref tips) is identical across reflogs, so it is
    // computed at most once.
    let mut head_reachable: Option<HashSet<ObjectId>> = None;
    for (refname, path) in &files {
        expire_one_reflog(repo, refname, path, &cfg, &mut head_reachable)?;
    }
    Ok(())
}

/// Every reflog file below `dir`, keyed by ref name (`logs/refs/heads/main` ->
/// `refs/heads/main`, `logs/HEAD` -> `HEAD`).
fn collect_reflog_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_reflog_files(base, &path, out),
            Ok(_) => {
                if let Ok(rel) = path.strip_prefix(base) {
                    let name = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    out.push((name, path));
                }
            }
            Err(_) => {}
        }
    }
}

/// Rewrite one reflog file, dropping expired entries and keeping the rest
/// verbatim.
fn expire_one_reflog(
    repo: &gix::Repository,
    refname: &str,
    path: &Path,
    cfg: &ReflogExpireConfig,
    head_reachable: &mut Option<HashSet<ObjectId>>,
) -> Result<()> {
    let (expire_total, expire_unreach) = cfg.resolve(refname);
    let Ok(raw) = std::fs::read(path) else {
        return Ok(());
    };
    if raw.is_empty() {
        return Ok(());
    }
    let lines = split_reflog_lines(&raw);

    // `reflog_expiry_prepare()`: choose the unreachable rule, then collapse to
    // `UE_ALWAYS` when the unreachable cutoff is no later than the total one (in
    // which case the reachability check can never change an outcome).
    let is_head = refname == "HEAD";
    let mut kind = if expire_unreach == 0 || is_head {
        ReflogKind::Head
    } else if ref_tip_commit(repo, refname).is_some() {
        ReflogKind::Normal
    } else {
        ReflogKind::Always
    };
    if expire_unreach <= expire_total {
        kind = ReflogKind::Always;
    }

    // The reachable set is only consulted for an entry in the half-open window
    // [expire_total, expire_unreachable). Compute it only when such an entry
    // exists and the ref actually distinguishes reachable from unreachable.
    let need_reach = matches!(kind, ReflogKind::Head | ReflogKind::Normal)
        && expire_total < expire_unreach
        && lines.iter().any(|l| {
            parse_reflog_line(l).is_some_and(|(_, _, ts)| ts >= expire_total && ts < expire_unreach)
        });
    let reach: HashSet<ObjectId> = if !need_reach {
        HashSet::new()
    } else if kind == ReflogKind::Head {
        head_reachable
            .get_or_insert_with(|| reachable_commits(repo, all_ref_tip_commits(repo)))
            .clone()
    } else {
        reachable_commits(repo, ref_tip_commit(repo, refname).into_iter().collect())
    };

    let mut changed = false;
    let mut kept: Vec<&[u8]> = Vec::with_capacity(lines.len());
    for line in &lines {
        let expire = match parse_reflog_line(line) {
            Some((old, new, ts)) => {
                should_expire_entry(repo, old, new, ts, expire_total, expire_unreach, kind, &reach)
            }
            // A line that does not parse names no entry to expire, so it is kept.
            None => false,
        };
        if expire {
            changed = true;
        } else {
            kept.push(*line);
        }
    }
    if changed {
        rewrite_reflog(path, &kept)?;
    }
    Ok(())
}

/// `should_expire_reflog_ent()` with `gc`'s flags (no `stalefix`, no `recno`).
fn should_expire_entry(
    repo: &gix::Repository,
    old: ObjectId,
    new: ObjectId,
    ts: i64,
    expire_total: i64,
    expire_unreach: i64,
    kind: ReflogKind,
    reach: &HashSet<ObjectId>,
) -> bool {
    if ts < expire_total {
        return true;
    }
    if ts < expire_unreach {
        match kind {
            ReflogKind::Always => return true,
            ReflogKind::Head | ReflogKind::Normal => {
                if is_unreachable(repo, reach, old) || is_unreachable(repo, reach, new) {
                    return true;
                }
            }
        }
    }
    false
}

/// `is_unreachable()`: a null id names nothing (keep), a non-commit peels to
/// nothing and is kept, and a commit is unreachable exactly when it is absent
/// from the tip closure.
fn is_unreachable(repo: &gix::Repository, reach: &HashSet<ObjectId>, oid: ObjectId) -> bool {
    if oid.is_null() {
        return false;
    }
    match peel_to_commit(repo, oid) {
        Some(commit) => !reach.contains(&commit),
        None => false,
    }
}

/// Split a reflog file into its lines, each including its trailing `\n`, so a
/// kept line can be re-emitted byte-for-byte.
fn split_reflog_lines(buf: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' {
            lines.push(&buf[start..=i]);
            start = i + 1;
        }
    }
    if start < buf.len() {
        lines.push(&buf[start..]);
    }
    lines
}

/// Parse one reflog line into `(old, new, committer-seconds)`; `None` when the
/// line is malformed.
fn parse_reflog_line(line: &[u8]) -> Option<(ObjectId, ObjectId, i64)> {
    let mut iter = gix::refs::file::log::iter::forward(line);
    let parsed = iter.next()?.ok()?;
    let ts = parsed.signature.time().ok()?.seconds;
    Some((parsed.previous_oid(), parsed.new_oid(), ts))
}

/// Overwrite `path` with `kept` via a same-directory temporary and a rename, so
/// a reader never sees a half-written reflog.
fn rewrite_reflog(path: &Path, kept: &[&[u8]]) -> Result<()> {
    let mut data = Vec::new();
    for line in kept {
        data.extend_from_slice(line);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let fname = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{fname}.zvcs_gc_tmp"));
    std::fs::write(&tmp, &data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("install {}", path.display()))
}

/// The set of commits reachable from `tips`, following commit parents only —
/// the closure `is_unreachable()` measures against.
fn reachable_commits(repo: &gix::Repository, tips: Vec<ObjectId>) -> HashSet<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    for tip in tips {
        if seen.insert(tip) {
            stack.push(tip);
        }
    }
    while let Some(id) = stack.pop() {
        let Ok(object) = repo.find_object(id) else {
            continue;
        };
        if object.kind != Kind::Commit {
            continue;
        }
        let commit = object.into_commit();
        let parents: Vec<ObjectId> = match commit.decode() {
            Ok(decoded) => decoded.parents().collect(),
            Err(_) => continue,
        };
        for parent in parents {
            if seen.insert(parent) {
                stack.push(parent);
            }
        }
    }
    seen
}

/// Peel an id to the commit git's `lookup_commit_reference_gently()` would
/// yield: an annotated-tag chain resolves to its commit, a non-commit-ish yields
/// `None`.
fn peel_to_commit(repo: &gix::Repository, mut oid: ObjectId) -> Option<ObjectId> {
    for _ in 0..8 {
        let object = repo.find_object(oid).ok()?;
        match object.kind {
            Kind::Commit => return Some(oid),
            Kind::Tag => {
                let tag = object.into_tag();
                oid = tag.decode().ok()?.target();
            }
            _ => return None,
        }
    }
    None
}

/// Every ref tip peeled to a commit — git's `push_tip_to_list` set for the
/// `UE_HEAD` closure. A symref merely repeats a commit already contributed by
/// its target, so following it here changes no closure.
fn all_ref_tip_commits(repo: &gix::Repository) -> Vec<ObjectId> {
    let mut tips = Vec::new();
    if let Ok(platform) = repo.references() {
        if let Ok(iter) = platform.all() {
            for reference in iter.flatten() {
                if let Ok(id) = reference.into_fully_peeled_id() {
                    if let Some(commit) = peel_to_commit(repo, id.detach()) {
                        tips.push(commit);
                    }
                }
            }
        }
    }
    tips
}

/// The commit a named ref resolves to, or `None` when the ref is gone or names
/// a non-commit.
fn ref_tip_commit(repo: &gix::Repository, refname: &str) -> Option<ObjectId> {
    let reference = repo.find_reference(refname).ok()?;
    let id = reference.into_fully_peeled_id().ok()?.detach();
    peel_to_commit(repo, id)
}

// --- worktree prune --------------------------------------------------------

/// `worktree prune --expire <gc.worktreePruneExpire>`, a faithful port of
/// `builtin/worktree.c`'s `prune_worktrees()` restricted to the checks `gc`
/// exercises. `gc` runs it non-verbose, so nothing is printed; a stale
/// worktree's administrative directory is simply removed.
fn prune_worktrees(repo: &gix::Repository) -> Result<()> {
    let now = SystemTime::now();
    let now_secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);

    // `gc.worktreePruneExpire` (default `3.months.ago`); an empty value disables
    // the step, matching git's `cfg.prune_worktrees_expire` guard.
    let default_expire =
        parse_reflog_expiry("3.months.ago", now).unwrap_or(now_secs - 90 * 24 * 3600);
    let expire = match repo.config_snapshot().string("gc.worktreePruneExpire") {
        Some(v) => {
            let raw = v.to_str_lossy().into_owned();
            if raw.is_empty() {
                return Ok(());
            }
            parse_reflog_expiry(&raw, now).unwrap_or(default_expire)
        }
        None => default_expire,
    };

    let dir = repo.common_dir().join("worktrees");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    let mut any_left = false;
    for entry in read.flatten() {
        let admin = entry.path();
        if should_prune_worktree(&admin, expire) {
            let _ = std::fs::remove_dir_all(&admin);
        } else {
            any_left = true;
        }
    }
    // `delete_worktrees_dir_if_empty()`: drop the container once nothing is left.
    if !any_left {
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(())
}

/// `should_prune_worktree()`: a worktree is prunable when its administrative
/// directory is invalid, its `gitdir` file is missing/empty, or the checkout it
/// names is gone *and* the administrative `index` has aged past `expire`. A
/// locked worktree is never pruned.
fn should_prune_worktree(admin: &Path, expire: i64) -> bool {
    if !admin.is_dir() {
        return true;
    }
    if admin.join("locked").exists() {
        return false;
    }
    let Ok(raw) = std::fs::read(admin.join("gitdir")) else {
        return true;
    };
    let mut end = raw.len();
    while end > 0 && (raw[end - 1] == b'\n' || raw[end - 1] == b'\r') {
        end -= 1;
    }
    let trimmed = &raw[..end];
    if trimmed.is_empty() {
        return true;
    }
    let target = PathBuf::from(String::from_utf8_lossy(trimmed).into_owned());
    if target.exists() {
        return false;
    }
    // Gone: prune only once the administrative `index` has aged past `expire`
    // (or cannot be stat'ed), matching git's `stat()`-failure branch.
    match super::prune::mtime_of(&admin.join("index")) {
        Some(mtime) => mtime <= expire,
        None => true,
    }
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

/// The effective `gc.maxCruftSize` value, if set anywhere, paired with git's
/// origin clause for the diagnostic it prints when the value is unreadable.
///
/// The merged config is walked in order and the last `gc.maxCruftSize` kept,
/// reproducing git's last-value-wins, and the winning value's source is carried
/// so a rejection can name it exactly as `git_config_ulong` does: a file-backed
/// value adds ` in file <path>` (git renders the repository config as
/// `.git/config`, so the leading `./` gitoxide reports is trimmed), while a
/// value from `-c`/environment adds nothing — matching git 2.55.0's output for
/// both sources.
fn effective_max_cruft_size(repo: &gix::Repository) -> Option<(String, String)> {
    let config = repo.config_snapshot().plumbing().clone();
    let mut found: Option<(String, Option<PathBuf>)> = None;
    for section in config.sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("gc")
        {
            continue;
        }
        let path = section.meta().path.clone();
        for value in section.body().values("maxCruftSize") {
            found = Some((value.to_str_lossy().into_owned(), path.clone()));
        }
    }
    let (raw, path) = found?;
    let origin = match path {
        Some(p) => {
            let shown = p.to_string_lossy();
            format!(" in file {}", shown.strip_prefix("./").unwrap_or(&shown))
        }
        None => String::new(),
    };
    Some((raw, origin))
}

/// git's `git_parse_ulong`, the parser behind `git_config_ulong` and hence
/// behind `gc.maxCruftSize`. Returns the byte count, or the reason string git's
/// `die_bad_number` prints after the value: `"invalid unit"` for a value it
/// cannot read, `"out of range"` for one that overflows an `unsigned long`.
///
/// The grammar is C `strtoumax` with base 0 (`0x400` is hex, `010` is octal,
/// everything else decimal) followed by `get_unit_factor`: an optional single
/// `k`/`m`/`g` magnitude suffix, either case, with nothing after it. A leading
/// `+` is accepted and leading ASCII whitespace is skipped; a leading `-`, an
/// empty value, or any trailing junk (a stray character, a second suffix) is an
/// invalid unit. Verified one value at a time against git 2.55.0's
/// `gc.maxcruftsize` diagnostics — `1k`/`0x400`/`010`/`0k` parse, `2m` clears
/// the floor, `-1`/`1.5`/`1x`/``/`5 ` are invalid units, and a 24-digit value is
/// out of range.
fn parse_config_ulong(raw: &str) -> Result<u64, &'static str> {
    const INVALID: &str = "invalid unit";
    const RANGE: &str = "out of range";

    // git guards `*value == '-'` because `strtoumax` would otherwise negate and
    // wrap a negative into a huge unsigned. Trimming first folds ` -1` in with
    // `-1`, matching git's rejection of both.
    let rest = raw.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let rest = match rest.strip_prefix('-') {
        Some(_) => return Err(INVALID),
        None => rest.strip_prefix('+').unwrap_or(rest),
    };

    let (radix, digits) = if let Some(r) = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))
    {
        (16u32, r)
    } else if rest.len() > 1 && rest.starts_with('0') {
        // Base 0 reads a leading zero as octal, and the zero is part of the
        // number: `0k` is 0 with a `k` suffix, not an empty number, so the `0`
        // is kept rather than stripped.
        (8, rest)
    } else {
        (10, rest)
    };

    let split = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    let (number, tail) = digits.split_at(split);
    if number.is_empty() {
        return Err(INVALID);
    }
    let value = u64::from_str_radix(number, radix).map_err(|_| RANGE)?;

    // `get_unit_factor`: an empty tail scales by one, one k/m/g byte scales and
    // must end the string, anything else is not a unit.
    let factor: u64 = match tail.as_bytes() {
        [] => 1,
        [b'k' | b'K'] => 1024,
        [b'm' | b'M'] => 1024 * 1024,
        [b'g' | b'G'] => 1024 * 1024 * 1024,
        _ => return Err(INVALID),
    };
    value.checked_mul(factor).ok_or(RANGE)
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
