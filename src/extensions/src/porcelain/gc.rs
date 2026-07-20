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
//! # Not performed
//!
//! These are skipped, and a `gc` that exits 0 has **not** done them. Each is
//! blocked on substrate that does not exist in the vendored crates under
//! `src/ported`, not on effort:
//!
//!   1. **Repacking.** `gc`'s headline job is `git repack -d`, whose purpose is a
//!      delta-compressed pack. `gix-pack`'s output entry iterator has exactly one
//!      mode, `Mode::PackCopyAndBaseObjects`, documented as "Copy base objects and
//!      deltas from packs, while non-packed objects will be treated as base
//!      objects (i.e. without trying to delta compress them)"
//!      (`gix-pack/src/data/output/entry/iter_from_counts.rs:362`). There is no
//!      delta search, so a pack written here would differ from git's in size and
//!      in every byte. `repack.rs` bails for this same reason and is not called.
//!      **Loose objects therefore stay loose.**
//!   2. **Cruft packs.** `--cruft` is the default since git 2.37 and needs a
//!      `.mtimes` file beside the pack. `grep -rl mtimes gix-pack/src gix-odb/src`
//!      returns nothing — there is neither a reader nor a writer.
//!   3. **Commit-graph.** `gc.writeCommitGraph` defaults to true.
//!      `gix-commitgraph/src` ships `access`, `file`, `init` and `verify` and no
//!      writer, so `objects/info/commit-graph` is never written or refreshed.
//!   4. **Reflog expiry.** `gc` runs `git reflog expire --all`. `gix-ref` only
//!      appends to a reflog as a side effect of a ref transaction and cannot
//!      rewrite or truncate one; `reflog.rs` bails on `expire` for this reason.
//!   5. **`worktree prune`.** `worktree.rs` bails — there is no worktree
//!      bookkeeping in the vendored crates.
//!   6. **Pruning at any expiry other than `now`.** The default is
//!      `gc.pruneExpire=2.weeks.ago`, and `prune.rs` implements no `--expire`: it
//!      lacks both git's approxidate parser and the second traversal
//!      `add_unseen_recent_objects_to_traversal()` performs to keep objects
//!      *referenced by* recently-written unreachable objects alive. Rather than
//!      guess, a dated expiry skips the prune step entirely. That errs toward
//!      keeping objects git would have deleted, which is the safe direction for a
//!      destructive command — but it does mean the default `gc` prunes nothing.
//!      `--prune=now` and `gc.pruneExpire=now` *are* run, because bare
//!      `git prune` is exactly `git prune --expire=now`: on a conflicted fixture
//!      both remove the same two objects (`2ae666ad…` tree, `5eb9640f…` blob)
//!      while the default `gc` keeps all 13.
//!
//! `--detach` is accepted and always ignored: this port runs synchronously.
//! Backgrounding only ever changes *when* the above work happens, and since the
//! blocked steps never happen at all, forking would add a race for no gain.
//! `--quiet` is likewise a no-op, because the progress it suppresses is written
//! to stderr off a tty, which git already suppresses.
//!
//! No `gc.pid` lock is taken, so `--force` has nothing to override. git uses the
//! lock to keep two concurrent `gc`s from fighting over the pack directory; with
//! repacking absent there is no pack directory contention to serialize.

use anyhow::Result;
use std::process::ExitCode;

use gix::bstr::ByteSlice;

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

/// The effective prune expiry, reduced to the only distinction this port can act
/// on: whether *everything* unreachable expires, or nothing does.
#[derive(PartialEq)]
enum Prune {
    /// `--no-prune`, or an expiry of `never`.
    Disabled,
    /// `--prune=now`, or `gc.pruneExpire=now` — every unreachable object expires,
    /// which is precisely bare `git prune`'s behaviour.
    Now,
    /// A dated expiry, `2.weeks.ago` by default. Skipped; see the module docs.
    Dated,
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
            // Boolean flags with no effect here, and their `--no-` forms, exactly
            // as listed in USAGE. `--cruft`/`--aggressive`/`--keep-largest-pack`
            // only ever modify the repack that this port does not run;
            // `--quiet`/`--detach` are covered in the module docs.
            "-q" | "--quiet" | "--no-quiet" | "--cruft" | "--no-cruft" | "--aggressive"
            | "--no-aggressive" | "--detach" | "--no-detach" | "--force" | "--no-force"
            | "--keep-largest-pack"
            // `--no-expire-to` is a valid negation (USAGE spells it `--[no-]expire-to`);
            // `--max-cruft-size` has no `--no-` form, so one is left to error out.
            | "--no-keep-largest-pack" | "--no-expire-to" => {}
            // `--prune=<date>` is the only optional-value option.
            _ if a.starts_with("--prune=") => {
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

    // git's order: pack-refs, then reflog expire (skipped), then repack
    // (skipped), then prune, then worktree prune (skipped), then rerere gc,
    // then commit-graph write (skipped).
    if pack_refs_enabled(&repo) {
        super::pack_refs::pack_refs(&[
            "pack-refs".to_string(),
            "--all".to_string(),
            "--prune".to_string(),
        ])?;
    }

    if prune == Prune::Now {
        super::prune::prune(&["prune".to_string()])?;
    }

    // Guarded on the directory: `rerere gc` returns early when rerere is
    // disabled, but a repository with rerere on and no `rr-cache` yet would hit
    // the delegate's `read_dir` error path, which git does not have.
    if repo.git_dir().join("rr-cache").is_dir() {
        super::rerere::rerere(&["rerere".to_string(), "gc".to_string()])?;
    }

    Ok(ExitCode::SUCCESS)
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
