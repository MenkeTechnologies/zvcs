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
//!   * **Reporting a previous failure**, as [`report_last_gc_error`]: a
//!     non-empty `$GIT_DIR/gc.log` that has not aged past `gc.logExpiry`
//!     (default `1.day.ago`) is printed and the whole run abandoned, exit 0.
//!     Reached only on git's detaching path — `--detach`, or `--auto` with
//!     `gc.autoDetach` — and only after the `--auto` threshold gate. The key
//!     itself is validated at config-read time by `repo_config_get_expiry()`'s
//!     "must resolve to the past" rule; see [`log_expiry`].
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
//! ## Pack bytes differ from git's, but the compression does not
//!
//! The packs come from `pack-objects`' writer, so they are delta-compressed by
//! git's own machinery ported into [`gix_pack::data::output::delta`], under the
//! repository's `pack.*` settings and `repack.useDeltaBaseOffset`, with
//! `--aggressive` widening the search to `gc.aggressiveWindow` /
//! `gc.aggressiveDepth`.
//!
//! The cruft pack is searched separately, under `repack.cruftWindow`,
//! `repack.cruftWindowMemory`, `repack.cruftDepth` and `repack.cruftThreads`,
//! each falling back to what the reachable pack used. git reaches the same
//! split by running `repack --cruft`, which keeps a second set of
//! `pack-objects` arguments for the cruft child; see [`cruft_delta_options`].
//!
//! The bytes still differ from git's, because objects are enumerated in this
//! module's own order rather than git's `compute_write_order()` — so the
//! checksum embedded in a pack's filename differs too. What is reproduced is the *object storage layout* —
//! which objects end up loose, which end up packed, how many packs and sidecars
//! exist, and that every one of them is well-formed. `git fsck`,
//! `git verify-pack` and `git cat-file` all accept the result.
//!
//! # Not performed
//!
//! These are skipped, and a `gc` that exits 0 has **not** done them:
//!
//!   1. **Reachability bitmaps** (`.bitmap`). git writes one for a large enough
//!      repack; it is a lookup accelerator, and its absence changes no answer.
//!   2. **`--keep-largest-pack`, `--max-cruft-size`.** Both select *which*
//!      objects a pack holds rather than how it is compressed: this port always
//!      rewrites every pack, and the fixtures' cruft packs are far below any
//!      size limit. They are accepted, and `--max-cruft-size` (with its
//!      `gc.maxCruftSize` default) still warns below git's 1 MiB floor.
//!      `--aggressive` *is* honoured: it widens the delta search to
//!      `gc.aggressiveWindow` and `gc.aggressiveDepth`, and passes the `-f`
//!      git's own `--aggressive` pushes, so no delta is kept from an existing
//!      pack.
//!
//!   3. **`gc.repackFilter` / `gc.repackFilterTo`.** git forwards these to its
//!      `repack` child as `--filter=` / `--filter-to=`, which makes it write a
//!      *second* pack holding the filtered-out objects. [`repack_all`] has no
//!      counterpart for that split — it partitions the store into reachable and
//!      unreachable and writes one pack for each — so a valid filter is read and
//!      then not applied. What *is* reproduced is the pair of refusals the child
//!      raises for a spec it cannot parse and for `--filter-to` without
//!      `--filter`; see [`check_repack_filter`], which explains why the split is
//!      not simply bolted on.
//!   4. **Writing `gc.log`.** A failure is reported on stderr here rather than
//!      captured to a file, there being no detached child whose output would
//!      otherwise be lost, and the file is not removed after a successful run
//!      either (`builtin/gc.c:99-101`). Reading one *is* done — see
//!      [`report_last_gc_error`] — because a `gc.log` stock git left in a shared
//!      repository has to stop this `gc` for as long as it stops that one.
//!
//! `--detach` does not run the work in the background: this port is synchronous,
//! so the work is complete by the time `gc` returns rather than shortly after.
//! The flag is still *read*, because git gates `report_last_gc_error()` on
//! `opts.detach > 0` — so it, and `gc.autoDetach` under `--auto`, decide whether
//! a previous failure's `gc.log` is reported and this run abandoned.
//! `--quiet` suppresses the progress meters the pack write reports, which git
//! writes to stderr and only on a terminal; see [`crate::progress`].
//!
//! No `gc.pid` lock is taken, so `--force` has nothing to override.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
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
use super::{Arg, LongOpt};

/// `cmd_gc()`'s `struct option builtin_gc_options[]` (builtin/gc.c), in table
/// order, as [`super::resolve_long`] reads it. `--max-cruft-size` is the only
/// `PARSE_OPT_NONEG` entry; `--prune` is `PARSE_OPT_OPTARG`.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
    LongOpt { name: "prune", neg: true, arg: Arg::Optional },
    LongOpt { name: "cruft", neg: true, arg: Arg::None },
    LongOpt { name: "max-cruft-size", neg: false, arg: Arg::Required },
    LongOpt { name: "aggressive", neg: true, arg: Arg::None },
    LongOpt { name: "auto", neg: true, arg: Arg::None },
    LongOpt { name: "detach", neg: true, arg: Arg::None },
    LongOpt { name: "force", neg: true, arg: Arg::None },
    LongOpt { name: "keep-largest-pack", neg: true, arg: Arg::None },
    LongOpt { name: "expire-to", neg: true, arg: Arg::Required },
    LongOpt { name: "skip-foreground-tasks", neg: true, arg: Arg::None },
];

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

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]skip-foreground-tasks`.
/// Captured byte-for-byte from stock git 2.55.0's `git gc --help-all`.
const USAGE_ALL: &str = r#"usage: git gc [<options>]

    -q, --[no-]quiet      suppress progress reporting
    --[no-]prune[=<date>] prune unreferenced objects
    --[no-]cruft          pack unreferenced objects separately
    --max-cruft-size <n>  with --cruft, limit the size of new cruft packs
    --[no-]aggressive     be more thorough (increased runtime)
    --[no-]auto           enable auto-gc mode
    --[no-]detach         perform garbage collection in the background
    --[no-]force          force running gc even if there may be another gc running
    --[no-]keep-largest-pack
                          repack all other packs except the largest pack
    --[no-]expire-to <dir>
                          pack prefix to store a pack containing pruned objects
    --[no-]skip-foreground-tasks
                          skip maintenance tasks typically done in the foreground

"#;

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
    // `--aggressive`: widen the delta search to `gc.aggressiveWindow` /
    // `gc.aggressiveDepth`, which is what git's own `--aggressive` forwards to
    // its `repack` child.
    let mut aggressive = false;
    // `-q`: suppress the progress meters the pack write reports. Progress is on
    // a terminal only, so a piped or redirected run is quiet either way.
    let mut quiet = false;
    // `None` until a `--prune` form is seen, so `gc.pruneExpire` can supply the
    // default only when the command line was silent — matching git, where the
    // command line overrides the config.
    let mut prune: Option<Prune> = None;
    // The raw text of the last `--prune=<value>`, kept so `parse_expiry_date()`
    // can be applied once after parsing, the way `cmd_gc()` applies it to
    // `prune_expire_arg` — last occurrence wins, and an unreadable earlier one is
    // never seen.
    let mut prune_raw: Option<String> = None;
    // Parsed eagerly, at the point the option is seen, because git's
    // `parse_max_cruft_size()` runs as a parse-options callback: a bad value
    // beats a later `-h` or a later unknown option, but a *valid* small value
    // does not warn until parsing has succeeded overall.
    let mut max_cruft_size: Option<u64> = None;
    // `None` until a `--cruft` form is seen, so `gc.cruftPacks` can supply the
    // default only when the command line was silent. git 2.37 made cruft packs
    // the default when neither says otherwise.
    let mut cruft: Option<bool> = None;
    // git's `opts.detach`, which starts at `-1` ("not asked either way"). `None`
    // here is that `-1`: it is distinct from `Some(false)`, because only the
    // unasked state lets `gc.autoDetach` fill it in under `--auto`.
    let mut detach: Option<bool> = None;

    let mut end_of_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            return Ok(usage_error(None));
        }
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        let a = resolved.as_ref();
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // `if (internal_help && !strcmp(arg + 2, "help-all"))`
            // (parse-options.c:1122), an exact match tested ahead of
            // parse_long_opt(): never an abbreviation, never with an
            // `=<value>`, and rendered as `USAGE_FULL`.
            "--help-all" => {
                print!("{USAGE_ALL}");
                return Ok(ExitCode::from(129));
            }
            "--auto" => auto = true,
            "--no-auto" => auto = false,
            "--prune" => prune = Some(Prune::Dated),
            "--no-prune" => prune = Some(Prune::Disabled),
            "--cruft" => cruft = Some(true),
            "--no-cruft" => cruft = Some(false),
            "--aggressive" => aggressive = true,
            "--no-aggressive" => aggressive = false,
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            // Boolean flags with no effect here, and their `--no-` forms, exactly
            // as listed in USAGE. `--keep-largest-pack` selects which packs to
            // rewrite, which this port does not vary; `--detach` is covered in
            // the module docs.
            // `--detach` no longer runs the work in the background here — this
            // port is synchronous — but git gates `report_last_gc_error()` on
            // `opts.detach > 0` (builtin/gc.c:962), so the flag still decides
            // whether a previous failure's `gc.log` is reported. See
            // [`report_last_gc_error`].
            "--detach" => detach = Some(true),
            "--no-detach" => detach = Some(false),
            "--force" | "--no-force"
            | "--keep-largest-pack"
            // `--no-expire-to` is a valid negation (USAGE spells it `--[no-]expire-to`);
            // `--max-cruft-size` has no `--no-` form, so one is left to error out.
            | "--no-keep-largest-pack" | "--no-expire-to" => {}
            // `--prune=<date>` is the only optional-value option.
            _ if a.starts_with("--prune=") => {
                // `--prune` is an `OPT_STRING`: the value is only kept here and
                // checked once, after parsing, on whichever occurrence came last.
                prune_raw = Some(a["--prune=".len()..].to_string());
                prune = Some(Prune::Dated);
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

    let repo = match crate::setup::discover() {
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
    match crate::config::config_ulong(&repo, "gc.maxCruftSize") {
        Ok(Some(size)) => {
            if max_cruft_size.is_none() {
                max_cruft_size = Some(size);
            }
        }
        Ok(None) => {}
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    }

    // `gc.logExpiry`, read by `gc_config()` right after `gc.maxCruftSize`
    // (builtin/gc.c:211). git 2.55.0 reports the cruft size first when both are
    // unreadable, in either `-c` order, which is why this sits below that block.
    let gc_log_expire = match log_expiry(&repo) {
        Ok(value) => value,
        Err(rejection) => {
            eprintln!("fatal: {}", rejection.into_fatal());
            return Ok(ExitCode::from(128));
        }
    };

    // `gc.repackFilter` / `gc.repackFilterTo`, read by `gc_config()` immediately
    // after `gc.logExpiry` (builtin/gc.c:222-230) and forwarded verbatim to the
    // `repack` child as `--filter=<v>` / `--filter-to=<v>` (:653-656), each only
    // when it is set *and* non-empty. Nothing is validated at this point; the
    // child's own parse-options does that, which is why the check below sits
    // beside the repack rather than here.
    let snap = repo.config_snapshot();
    let repack_filter = snap
        .string("gc.repackFilter")
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty());
    let repack_filter_to = snap
        .string("gc.repackFilterTo")
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty());
    drop(snap);

    // `cmd_gc()`: `if (prune_expire_arg && parse_expiry_date(prune_expire_arg, &dummy))
    // die(_("failed to parse prune expiry value %s"), prune_expire_arg)`. It runs
    // after `parse_options()` and after the stray-positional usage error, and it
    // sees only the last `--prune=` on the line — `--prune== --prune=2.weeks.ago`
    // is accepted for exactly that reason.
    if let Some(raw) = &prune_raw {
        let Some(expiry) = crate::date::parse_expiry_date(raw) else {
            eprintln!("fatal: failed to parse prune expiry value {raw}");
            return Ok(ExitCode::from(128));
        };
        // `parse_expiry_date()` folds four words to the two extremes:
        // `never`/`false` expire nothing, `all`/`now` expire everything.
        // Comparing the resolved timestamp catches all four without spelling
        // them out a second time.
        prune = Some(if expiry == 0 {
            Prune::Disabled
        } else if expiry == i64::MAX {
            Prune::Now
        } else {
            Prune::Dated
        });
    }

    // `gc --auto` is a no-op below the thresholds; git returns before touching
    // anything, so nothing below this point may run either.
    if auto && !gc_needed(&repo) {
        return Ok(ExitCode::SUCCESS);
    }

    // `cmd_gc()`: `if (cfg.detach_auto && opts.detach < 0) opts.detach = 1;`
    // inside the `--auto` branch (builtin/gc.c:930-931), so only `--auto` lets
    // `gc.autoDetach` (default true) turn detaching on. Without `--auto` the
    // flag stays at `-1` unless it was given, and `-1` is not `> 0`.
    let detach_positive = match detach {
        Some(explicit) => explicit,
        None => auto && repo.config_snapshot().boolean("gc.autoDetach").unwrap_or(true),
    };

    // `if (opts.detach > 0) { ret = report_last_gc_error(); … }`
    // (builtin/gc.c:962-972). A previous failure that is still within
    // `gc.logExpiry` is reported and this run is abandoned, with an exit of 0
    // — `ret = 1` is folded back to 0 at :966 so an auto-gc never fails a
    // command that only invoked it as housekeeping.
    if detach_positive {
        match report_last_gc_error(&repo, &gc_log_expire) {
            LastGcError::None => {}
            LastGcError::Reported => return Ok(ExitCode::SUCCESS),
            LastGcError::Unreadable(code) => return Ok(code),
        }
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
    let repo = crate::setup::discover().unwrap_or(repo);

    // `reflog expire --all`, a foreground task git runs before the repack so an
    // expired entry no longer keeps its object alive. Skipped only when both
    // `gc.reflogExpire` and `gc.reflogExpireUnreachable` are `never`, exactly as
    // git's `cfg->prune_reflogs` gate.
    if reflog_expire_enabled(&repo) {
        expire_reflogs(&repo)?;
    }

    // The `repack` child's own argument diagnostics, which `gc` reaches here —
    // after `pack-refs` and `reflog expire` have already run, and after the
    // `--auto` gate, so a below-threshold `gc --auto` never sees them. git
    // reports the child's `die()` and then `run_command`'s own line, and hands
    // the 128 back:
    //
    // ```text
    // $ git -c gc.repackFilter=bogusfilter gc
    // fatal: invalid filter-spec 'bogusfilter'
    // fatal: failed to run repack
    // ```
    //
    // A bad spec is rejected while parse-options is still running
    // (`builtin/repack.c`'s `OPT_PARSE_LIST_OBJECTS_FILTER`), so it beats the
    // `--filter-to` pairing check at `builtin/repack.c:407-408`.
    if let Some(code) = check_repack_filter(repack_filter.as_deref(), repack_filter_to.as_deref()) {
        return Ok(code);
    }

    // `git_pack_config()`'s `pack.useBitmaps` and `pack.allowPackReuse`, which
    // git reaches through the `pack-objects` grandchild its `repack` child
    // starts. That is why `gc -h` prints usage under a bad value and a real `gc`
    // dies: the read belongs to the packing, not to the option parsing. This
    // port packs inline, so it is arranged here, immediately before
    // [`repack_all`]. See [`super::pack_objects::PackConfig`].
    match crate::repo_settings::RepoSettings::load(&repo)
        .and_then(|settings| super::pack_objects::PackConfig::load(&repo, &settings))
    {
        Ok(_) => {}
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    }

    repack_all(
        &repo,
        unreachable,
        delta_options(&repo, aggressive, crate::progress::enabled(quiet)),
        quiet,
    )?;

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
        // `rerere()` is handed the arguments the verb was dispatched with, so the
        // verb itself is not one of them: a leading "rerere" reads as an unknown
        // subcommand and prints the usage block instead of collecting anything.
        super::rerere::rerere(&["gc".to_string()])?;
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

/// How the repack's pack writer is steered.
///
/// `repack.useDeltaBaseOffset` decides how a delta names its base, defaulting to
/// by-offset as it does in git. `--aggressive` then substitutes
/// `gc.aggressiveWindow` (250) and `gc.aggressiveDepth` (50) for `pack.window`
/// and `pack.depth`, which is exactly what git's `--aggressive` pushes onto its
/// `repack` child's argument list — and, like git, a value of zero or less is
/// dropped rather than forwarded, leaving the `pack.*` value in place.
fn delta_options(
    repo: &gix::Repository,
    aggressive: bool,
    progress: bool,
) -> super::pack_objects::WriteOptions {
    let snapshot = repo.config_snapshot();
    let mut options = super::pack_objects::WriteOptions {
        allow_ofs_delta: snapshot.boolean("repack.useDeltaBaseOffset").unwrap_or(true),
        progress,
        ..super::pack_objects::WriteOptions::default()
    };
    if aggressive {
        // `strvec_push(&repack_args, "-f")` (builtin/gc.c:920), which reaches
        // `pack-objects` as `--no-reuse-delta`: the point of `--aggressive` is
        // to search every pair again rather than keep the deltas already on
        // disk, and a widened window that reused them would not use it.
        options.no_reuse_delta = true;
        let positive = |key: &str, default: i64| {
            let value = snapshot.integer(key).unwrap_or(default);
            usize::try_from(value).ok().filter(|n| *n > 0)
        };
        options.window = positive("gc.aggressiveWindow", 250);
        options.depth = positive("gc.aggressiveDepth", 50);
    }
    options
}

/// How the *cruft* pack's writer is steered, which is not the same as the
/// reachable pack's.
///
/// git gets here in two hops: `gc` runs `repack --cruft`, and `repack` keeps a
/// second `struct pack_objects_args` for the cruft child. Each of the four
/// `repack.cruft*` values goes into that second set, and any it does not set is
/// copied from the first — so an unset key leaves the cruft pack searching
/// exactly as the reachable pack does, and a set one overrides `pack.window`,
/// `pack.depth`, `pack.windowMemory` or `pack.threads` for the cruft pack alone.
///
/// Why the split exists: a cruft pack is objects nobody references, rewritten
/// on every `gc`. Spending the reachable pack's delta budget on it is waste, so
/// git lets the two be tuned apart.
fn cruft_delta_options(
    repo: &gix::Repository,
    base: super::pack_objects::WriteOptions,
) -> super::pack_objects::WriteOptions {
    let snapshot = repo.config_snapshot();
    let mut options = base;
    // git stores these as strings and lets the `pack-objects` child parse them,
    // so a value it cannot read fails there rather than here. A negative window,
    // depth or thread count is not a number `pack-objects` accepts, so it is
    // left to the inherited value instead of being forced through.
    let count = |key: &str| {
        snapshot
            .integer(key)
            .and_then(|value| usize::try_from(value).ok())
    };
    if let Some(window) = count("repack.cruftWindow") {
        options.window = Some(window);
    }
    if let Some(depth) = count("repack.cruftDepth") {
        options.depth = Some(depth);
    }
    if let Some(threads) = count("repack.cruftThreads") {
        options.threads = Some(threads);
    }
    if let Some(limit) = snapshot
        .integer("repack.cruftWindowMemory")
        .and_then(|value| u64::try_from(value).ok())
    {
        options.window_memory = Some(limit);
    }
    options
}

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
fn repack_all(
    repo: &gix::Repository,
    unreachable: Unreachable,
    delta: super::pack_objects::WriteOptions,
    quiet: bool,
) -> Result<()> {
    let hash = repo.object_hash();
    let objdir = repo.objects.store_ref().path().to_path_buf();
    let pack_dir = objdir.join("pack");

    // Everything the store already holds, and where. A loose object also
    // remembers its path and mtime: the path so it can be unlinked once packed,
    // the mtime because a cruft pack has to record it.
    let loose = loose_objects(&objdir, hash);
    let rewritable = local_packs(&pack_dir, hash, big_pack_threshold(repo));

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
        // `gc` reaches its repacking through a `repack -d -l` child
        // (`builtin/gc.c:897`, with `-a`/`-A`/`--cruft` appended by
        // `add_repack_all_option()` and `-q` by `builtin/gc.c:926-927`), so
        // `repack`'s own `if (!names.nr) printf_ln(_("Nothing new to pack."))`
        // (`builtin/repack.c:460-462`) lands on `gc`'s stdout. An empty object
        // store is exactly the case that leaves `pack-objects` with nothing to
        // write, so `git init --bare b && git -C b gc` says so; this port packs
        // inline, so the notice is emitted here, on the same terms.
        if !quiet {
            println!("Nothing new to pack.");
        }
        return Ok(());
    }

    // `repack` runs `pack-objects --all`, whose ref walk dies on a ref naming an object
    // the repository does not have — and `gc` adds its own line when the child fails
    // (`fatal: failed to run repack`, builtin/gc.c). The repack stops here, before a pack
    // is written from a reachability set that ref could not contribute to.
    //
    // `pack-objects` prints only the death: the
    // `error: <ref> does not point to a valid object!` a `git gc` run also shows comes
    // from the `pack-refs --all --prune` child that ran before it (confirmed under
    // `GIT_TRACE=1` on git 2.55.0), and this port runs that same step above.
    if let Some(name) = super::prune::bad_object_ref(repo) {
        eprintln!("fatal: bad object {name}");
        eprintln!("fatal: failed to run repack");
        return Err(anyhow::Error::new(crate::fatal::Silent(128)));
    }

    let mut roots = Vec::new();
    super::prune::collect_roots(repo, &mut roots)?;
    let reachable = super::prune::close_over(repo, roots);

    // `existing` is already sorted and deduplicated, so both halves come out in
    // the oid order a pack index wants.
    let (keep, rest): (Vec<ObjectId>, Vec<ObjectId>) =
        existing.into_iter().partition(|id| reachable.contains(id));

    // The traversal above is what git reports as `Enumerating objects`, ahead of
    // the counting/compressing/writing meters the pack writer drives.
    {
        let mut enumerating =
            crate::progress::Meter::unknown("Enumerating objects", delta.progress);
        enumerating.advance(keep.len());
        enumerating.done();
    }

    // The new pack has to be written before anything is removed: every object in
    // it is read back out of the very packs and loose files being replaced.
    let mut written = Vec::new();
    if let Some(base) = write_bundle(repo, &pack_dir, &keep, None, delta)? {
        written.push(base);
    }
    if unreachable == Unreachable::Cruft {
        // git reports the cruft pass separately. Its second line, `Traversing
        // cruft objects`, counts a walk out from the cruft tips that has no
        // counterpart here: `rest` is already the complete unreachable set,
        // partitioned out of everything the store holds, so there is nothing left
        // to traverse and no honest number to print for it.
        let mut enumerating =
            crate::progress::Meter::unknown("Enumerating cruft objects", delta.progress);
        enumerating.advance(rest.len());
        enumerating.done();

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
        if let Some(base) = write_bundle(repo, &pack_dir, &rest, Some(&mtimes), cruft_delta_options(repo, delta))? {
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
    // The packs are gone; a multi-pack-index still naming them would answer every
    // lookup for their objects with an offset into a file that no longer exists.
    super::multi_pack_index::drop_stale_midx(&pack_dir);
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

/// `gc.bigPackThreshold`, in bytes, or zero when it is unset.
///
/// git's `find_base_packs()` turns it into a `--keep-pack=<name>` for every
/// local pack at or above the size, which is the same instruction a `.keep` file
/// carries: leave the pack where it is and do not copy its objects into the new
/// one. Rewriting a pack that is already large is the expensive half of a `gc`
/// and rarely buys anything, so this is how a big repository keeps `gc` cheap.
///
/// An unreadable value is treated as unset, matching git's `git_config_ulong`,
/// which warns and moves on rather than dying here.
fn big_pack_threshold(repo: &gix::Repository) -> u64 {
    crate::config::config_ulong(repo, "gc.bigPackThreshold")
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// Every local pack that may be rewritten, as `(base name, index)`.
///
/// Alternates are deliberately not included: `repack` rewrites the repository's
/// own object store and must not touch a store it merely borrows from. A pack
/// beside a `.keep` file is skipped for the reason `git repack` skips it — the
/// marker is a promise that the pack stays put — and so is one at or above
/// `big_pack_threshold` bytes, which git keeps for the same reason by a
/// different route.
fn local_packs(
    pack_dir: &Path,
    hash: gix::hash::Kind,
    big_pack_threshold: u64,
) -> Vec<(String, pack::index::File)> {
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
        if big_pack_threshold > 0 {
            let size = std::fs::metadata(pack_dir.join(format!("{base}.pack")))
                .map(|md| md.len())
                .unwrap_or(0);
            if size >= big_pack_threshold {
                continue;
            }
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
/// The pack comes from `pack-objects`' writer, so it is delta-compressed under
/// the repository's `pack.*` settings, with `repack.useDeltaBaseOffset` choosing
/// how a delta names its base and `gc --aggressive` widening the search per
/// `gc.aggressiveWindow` / `gc.aggressiveDepth`. Its entries are therefore *not*
/// in `ids` order — a base has to precede the deltas that need it — so the
/// index's three parallel columns are rebuilt from the writer's own record of
/// where each object landed.
///
/// An object the writer could not read is absent from the pack, and is dropped
/// from the index and the `.mtimes` alongside it rather than left as a dangling
/// entry.
fn write_bundle(
    repo: &gix::Repository,
    pack_dir: &Path,
    ids: &[ObjectId],
    mtimes: Option<&HashMap<ObjectId, u32>>,
    delta: super::pack_objects::WriteOptions,
) -> Result<Option<String>> {
    if ids.is_empty() {
        return Ok(None);
    }
    let hash = repo.object_hash();
    std::fs::create_dir_all(pack_dir)
        .with_context(|| format!("create {}", pack_dir.display()))?;

    let packed = super::pack_objects::packed_for(repo, ids, delta)
        .with_context(|| format!("build a pack of {} objects", ids.len()))?;
    if packed.entries.is_empty() {
        return Ok(None);
    }

    // A v2 index stores its three columns in object-id order, whatever order the
    // pack itself is in.
    let mut by_oid = packed.entries.clone();
    by_oid.sort_unstable_by_key(|entry| entry.id);
    let written: Vec<ObjectId> = by_oid.iter().map(|entry| entry.id).collect();
    let offsets: Vec<u64> = by_oid.iter().map(|entry| entry.offset).collect();
    let crcs: Vec<u32> = by_oid.iter().map(|entry| entry.crc32).collect();

    // The pack is built under a temporary name because its final name is its own
    // checksum, which is only known once the last byte is in. This is also how
    // git writes it. The name carries the pid because concurrent runs against one
    // object store are the norm here, and a shared name would have them writing
    // over each other's bytes before either rename.
    let tmp = pack_dir.join(format!("tmp_pack_zvcs_gc_{}", std::process::id()));
    std::fs::write(&tmp, &packed.bytes).with_context(|| format!("create {}", tmp.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        // git installs pack artifacts read-only; a failure to set the mode is not
        // fatal there and is not here either.
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444));
    }

    let pack_hash = packed.id;
    let base = format!("pack-{pack_hash}");
    std::fs::rename(&tmp, pack_dir.join(format!("{base}.pack")))
        .with_context(|| format!("install {base}.pack"))?;

    let pack_id = pack_hash.as_slice();
    write_sidecar(pack_dir, &base, "idx", &index_bytes(hash, &written, &offsets, &crcs, pack_id)?)?;
    write_sidecar(pack_dir, &base, "rev", &reverse_index_bytes(hash, &offsets, pack_id)?)?;
    if let Some(mtimes) = mtimes {
        let stamps: Vec<u32> = written.iter().map(|id| mtimes.get(id).copied().unwrap_or(0)).collect();
        write_sidecar(pack_dir, &base, "mtimes", &mtimes_bytes(hash, &stamps, pack_id)?)?;
    }
    Ok(Some(base))
}

/// Install one `.idx`/`.rev`/`.mtimes` beside the pack it belongs to.
///
/// Written under a temporary name and renamed into place, as git installs every
/// pack artifact. The rename is what makes a rerun work: these files are left
/// `0444` (git's mode, matching [`super::pack_objects`]), and a pack whose object
/// set has not changed hashes to the name it had last time, so writing straight
/// to the destination would hit the read-only file a previous run left there and
/// fail with `EACCES`. A rename replaces its destination whatever its mode is.
fn write_sidecar(pack_dir: &Path, base: &str, ext: &str, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let path = pack_dir.join(format!("{base}.{ext}"));
    let tmp = pack_dir.join(format!("tmp_{ext}_zvcs_gc_{}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    // Set the mode while the file is still under its temporary name: a failure
    // there is not fatal, exactly as git does not check its own chmod.
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444));
    std::fs::rename(&tmp, &path).with_context(|| format!("install {}", path.display()))
}

/// A v2 pack index: the `\xfftOc` signature, the 256-entry fan-out, then the
/// ids, their CRC32s and their offsets as three parallel columns, and finally
/// the pack's checksum and the index's own.
///
/// Only the 32-bit offset column is emitted, so an entry beyond 2 GiB into the
/// pack cannot be indexed; it would need the 64-bit spill table that
/// [`super::pack_objects::index_file`] writes. Reaching that needs a repository
/// whose whole reachable set exceeds 2 GiB after delta compression, which is
/// past what this writer — which assembles the pack in memory — supports
/// anyway.
fn index_bytes(
    hash: gix::hash::Kind,
    ids: &[ObjectId],
    offsets: &[u64],
    crcs: &[u32],
    pack_id: &[u8],
) -> Result<Vec<u8>> {
    const LARGE_OFFSET_THRESHOLD: u64 = 0x7fff_ffff;
    if let Some(offset) = offsets.iter().find(|o| **o > LARGE_OFFSET_THRESHOLD) {
        crate::git_fatal!("pack offset {offset} needs a 64-bit index offset table, which is not written");
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
pub(super) fn gc_needed(repo: &gix::Repository) -> bool {
    // `gc.auto` at zero or below disables automatic gc outright — git returns
    // before it ever counts packs, so a repository over `gc.autoPackLimit` is
    // still left alone.
    let auto_threshold = repo.config_snapshot().integer("gc.auto").unwrap_or(6700);
    if auto_threshold <= 0 {
        return false;
    }
    too_many_packs(repo) || too_many_loose_objects(repo, auto_threshold)
}

/// git's `too_many_loose_objects()`: the *approximate* loose-object count — the
/// entries of the single `objects/17/` fan-out directory, extrapolated by 256 —
/// against `limit` rounded up to the next multiple of 256. Both sides of git's
/// comparison carry that factor of 256, so it divides out and the sampled count
/// is compared against `DIV_ROUND_UP(limit, 256)` directly.
///
/// A missing `objects/17` is zero objects; any other read failure makes git
/// return 0 here too, so an unreadable directory never triggers the task.
pub(super) fn too_many_loose_objects(repo: &gix::Repository, limit: i64) -> bool {
    let rounded = limit.div_euclid(256) + i64::from(limit.rem_euclid(256) != 0);
    let name_len = repo.object_hash().len_in_hex() - 2;
    let Ok(entries) = std::fs::read_dir(repo.objects.store_ref().path().join("17")) else {
        return false;
    };
    let mut loose: i64 = 0;
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
        if loose > rounded {
            return true;
        }
    }
    false
}

/// git's `too_many_packs()`: more local, non-`.keep` packs than
/// `gc.autoPackLimit` (default 50). A non-positive limit disables the check.
pub(super) fn too_many_packs(repo: &gix::Repository) -> bool {
    let limit = repo.config_snapshot().integer("gc.autoPackLimit").unwrap_or(50);
    if limit <= 0 {
        return false;
    }
    let mut packs: i64 = 0;
    let Ok(entries) = std::fs::read_dir(repo.objects.store_ref().path().join("pack")) else {
        return false;
    };
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
    packs > limit
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

/// The `repack` child's diagnostics for the two arguments `gc` forwards from
/// `gc.repackFilter` and `gc.repackFilterTo`, or `None` when it would have
/// started.
///
/// # What this does and does not do
///
/// The *diagnostics* are ported; the object split they gate is not. git's
/// `--filter` makes `repack` write a **second** pack holding what the old packs
/// held and the new one does not, built by a `pack-objects --stdin-packs` pass
/// that [`repack_all`] has no counterpart for — it partitions the store into
/// reachable and unreachable and writes one pack for each, with no third set and
/// no `^`-excluded input. Splitting the reachable half here instead would also
/// have to decide what happens to a filtered-out object that was only ever
/// loose, which git leaves alone because `--stdin-packs` never enumerates it,
/// and getting that wrong deletes objects. So a *valid* filter is read and then
/// not applied, and this is listed with the other "Not performed" steps in the
/// module docs.
///
/// What is reproduced is the pair of refusals, which are reachable from
/// configuration alone and stop the `gc` in git exactly as they stop it here.
fn check_repack_filter(filter: Option<&str>, filter_to: Option<&str>) -> Option<ExitCode> {
    let failed = |message: &str| {
        eprintln!("fatal: {message}");
        eprintln!("fatal: failed to run repack");
        Some(ExitCode::from(128))
    };
    if let Some(spec) = filter {
        if let Err(message) = super::pack_objects::gently_parse_filter(spec.as_bytes()) {
            return failed(&message);
        }
    } else if filter_to.is_some() {
        return failed("option '--filter-to' can only be used along with '--filter'");
    }
    None
}

/// How git refuses a `gc.logExpiry` it will not accept: `git_die_config()`
/// prints an `error:` line naming the key and its value, then a `fatal:` line
/// naming where the value came from.
struct LogExpiryRejection {
    /// The offending value, as written.
    value: String,
    /// The `fatal:` clause: `unable to parse '<var>' from command-line config`
    /// for `-c`/environment, `bad config variable '<var>' in file '<path>'`
    /// otherwise.
    origin: String,
}

impl LogExpiryRejection {
    /// Print the `error:` line and return the message to die with.
    ///
    /// git's file-backed clause also names the line the variable sits on
    /// (`… in file '.git/config' at line 9`); gitoxide's config metadata carries
    /// the source path but not the line, so that clause is dropped — the same
    /// limitation, and the same wording, as [`crate::default_config`]'s port of
    /// the sibling `config_error_nonbool` diagnostic.
    fn into_fatal(self) -> String {
        eprintln!("error: Invalid gc.logexpiry: '{}'", self.value);
        self.origin
    }
}

/// `repo_config_get_expiry(r, "gc.logexpiry", &out)` (config.c:2468-2481), whose
/// validation is *not* `parse_expiry_date`:
///
/// ```c
/// int ret = repo_config_get_string(r, key, output);
/// if (ret) return ret;
/// if (strcmp(*output, "now")) {
///         timestamp_t now = approxidate("now");
///         if (approxidate(*output) >= now)
///                 git_die_config(r, key, _("Invalid %s: '%s'"), key, *output);
/// }
/// ```
///
/// So the literal `now` is let through as a special case, and everything else
/// has to resolve to a moment strictly in the past. That is a wider net than
/// "unparseable": `approxidate()` answers *now* for anything it cannot read, so
/// `bogus`, an empty value, `false` and `all` are all rejected while `never`,
/// `1.day.ago` and `2 weeks ago` are accepted — each verified against git
/// 2.55.0.
///
/// The returned string is `cfg.gc_log_expire`, defaulting to git's
/// `"1.day.ago"` (builtin/gc.c:160).
fn log_expiry(repo: &gix::Repository) -> Result<String, LogExpiryRejection> {
    let Some((value, origin)) = last_value_with_source(repo, "gc.logExpiry") else {
        return Ok("1.day.ago".to_string());
    };
    if value != "now" && crate::date::approxidate(&value) >= crate::date::now_seconds() {
        return Err(LogExpiryRejection { value, origin });
    }
    Ok(value)
}

/// The last value configured for `key` plus the `git_die_config()` clause naming
/// where it came from.
///
/// `crate::config::last_value_with_origin` reports the origin as the numeric
/// diagnostic's ` in file <path>` suffix, which is a different sentence from the
/// one `git_die_config` builds; this walks the same merged config and builds
/// that one instead, distinguishing a `-c`/environment value from a file.
fn last_value_with_source(repo: &gix::Repository, key: &str) -> Option<(String, String)> {
    use gix::config::Source;

    let (section_name, name) = key.split_once('.')?;
    let var = key.to_lowercase();
    let config = repo.config_snapshot().plumbing().clone();
    let mut found: Option<(String, gix::config::file::Metadata)> = None;
    for section in config.sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case(section_name)
        {
            continue;
        }
        for value in section.body().values(name) {
            found = Some((value.to_str_lossy().into_owned(), section.meta().clone()));
        }
    }
    let (raw, meta) = found?;
    let origin = match meta.source {
        Source::Cli | Source::Env => format!("unable to parse '{var}' from command-line config"),
        _ => match &meta.path {
            Some(path) => {
                let shown = path.to_string_lossy();
                let shown = shown.strip_prefix("./").unwrap_or(&shown);
                format!("bad config variable '{var}' in file '{shown}'")
            }
            None => format!("bad config variable '{var}'"),
        },
    };
    Some((raw, origin))
}

/// What [`report_last_gc_error`] found.
enum LastGcError {
    /// No `gc.log`, or one that has aged past `gc.logExpiry`, or an empty one:
    /// the run continues.
    None,
    /// A non-empty `gc.log` inside the expiry. git warns and abandons the run
    /// with an exit of 0.
    Reported,
    /// `gc.log` could not be stat'd or read for a reason other than its absence;
    /// git has already printed `die_message_errno()`'s line and returns 128.
    Unreadable(ExitCode),
}

/// Port of `report_last_gc_error()` (builtin/gc.c:791-831).
///
/// A `gc` that fails while detached writes its diagnostics to `$GIT_DIR/gc.log`
/// (builtin/gc.c:1003-1010). The next detaching `gc` reads that file back and,
/// if it is still recent enough to matter, prints it and does nothing —
/// "a previous gc failed … it is likely to fail in the same way".
///
/// `gc.logExpiry` is what "recent enough" means: the file is skipped once its
/// mtime is older than `parse_expiry_date(gc.logExpiry)`, so the default
/// `1.day.ago` makes a stale failure stop blocking auto-gc after a day. The
/// caller has already checked git's `opts.detach > 0` gate.
///
/// This port never *writes* `gc.log` — it runs synchronously, so a failure is
/// reported on stderr where the user is already looking, and there is no
/// detached child whose output would otherwise be lost. Reading one is still
/// right: stock git and this binary share a repository, so a `gc.log` stock git
/// left behind must stop this `gc` for exactly as long as it stops that one.
fn report_last_gc_error(repo: &gix::Repository, gc_log_expire: &str) -> LastGcError {
    let gc_log_path = repo.git_dir().join("gc.log");
    let metadata = match std::fs::metadata(&gc_log_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LastGcError::None,
        Err(e) => {
            eprintln!("fatal: cannot stat '{}': {e}", gc_log_path.display());
            return LastGcError::Unreadable(ExitCode::from(128));
        }
    };

    // `if (st.st_mtime < gc_log_expire_time) goto done;` — an unreadable
    // `gc.logExpiry` never reaches here (it is fatal above), and
    // `parse_expiry_date` folds `never` to 0, which no mtime is below.
    let expire_time = crate::date::parse_expiry_date(gc_log_expire).unwrap_or(0);
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if mtime < expire_time {
        return LastGcError::None;
    }

    let contents = match std::fs::read_to_string(&gc_log_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fatal: cannot read '{}': {e}", gc_log_path.display());
            return LastGcError::Unreadable(ExitCode::from(128));
        }
    };
    // `else if (len > 0)`: an empty log is not a failure report.
    if contents.is_empty() {
        return LastGcError::None;
    }

    eprintln!(
        "warning: The last gc run reported the following. Please correct the root cause\n\
         and remove {}\n\
         Automatic cleanup will not be performed until the file is removed.\n\
         \n\
         {contents}",
        gc_log_path.display()
    );
    LastGcError::Reported
}

/// git gc's `cfg->prune_reflogs`: `reflog expire --all` runs unless BOTH
/// `gc.reflogExpire` and `gc.reflogExpireUnreachable` are configured to a value
/// that resolves to the `never` sentinel (`0`). An unset value is not `never`,
/// so the default is to run — matching `gc_config_is_timestamp_never()`.
fn reflog_expire_enabled(repo: &gix::Repository) -> bool {
    let cfg = repo.config_snapshot();
    let is_never = |key: &str| {
        cfg.string(key)
            .and_then(|v| v.to_str().ok().map(str::to_owned))
            .and_then(|v| parse_reflog_expiry(&v))
            .is_some_and(|t| t == 0)
    };
    !(is_never("gc.reflogExpire") && is_never("gc.reflogExpireUnreachable"))
}

/// git's `parse_expiry_date()` (date.c:957) for a reflog cutoff, through the one shared parser:
/// `never`/`false` are the `0` sentinel (never expire), `all`/`now` are `i64::MAX` (expire
/// everything), and anything else is an approxidate. An empty or unreadable value yields `None`,
/// which callers treat as "unset".
pub(super) fn parse_reflog_expiry(value: &str) -> Option<i64> {
    crate::date::parse_expiry_date(value.trim())
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
pub(super) struct ReflogExpireConfig {
    default_total: i64,
    default_unreach: i64,
    entries: Vec<ReflogEntryOpt>,
}

impl ReflogExpireConfig {
    /// `reflog_expire_options_set_refname()` (`reflog.c:99-133`): the first
    /// pattern that matches wins, `refs/stash` never expires when unconfigured,
    /// otherwise the defaults apply. `gc` sets no explicit expiry, so the config
    /// always drives.
    ///
    /// A matching entry supplies **both** cutoffs, and the one it does not
    /// configure is `0` — the `never` sentinel — not the global default:
    ///
    /// ```c
    /// if (!wildmatch(ent->pattern, ref, 0)) {
    ///         if (!(cb->explicit_expiry & REFLOG_EXPIRE_TOTAL))
    ///                 cb->expire_total = ent->expire_total;
    ///         if (!(cb->explicit_expiry & REFLOG_EXPIRE_UNREACH))
    ///                 cb->expire_unreachable = ent->expire_unreachable;
    ///         return;
    /// }
    /// ```
    ///
    /// `ent` comes from `find_cfg_ent()`, which allocates it with
    /// `FLEX_ALLOC_MEM` — a zeroing allocation — so an unconfigured slot holds
    /// `0`, and `0` is what `should_expire_reflog_ent()` reads as "never
    /// expire". Configuring one half of a pattern therefore switches the *other*
    /// half off, which is not what the documentation suggests and is easy to get
    /// wrong: this returned `self.default_*` for the unset half until a
    /// differential run caught it. Verified against git 2.55.0 on a branch whose
    /// three reflog entries are all 400 days old, one tip reachable:
    ///
    /// | configuration                                     | entries kept |
    /// |---------------------------------------------------|--------------|
    /// | (none)                                            | 0            |
    /// | `gc.reflogExpireUnreachable=never`                | 0            |
    /// | `gc.<refs/heads/*>.reflogExpireUnreachable=never` | 3            |
    /// | `gc.<refs/heads/*>.reflogExpire=never`            | 3            |
    /// | `gc.<refs/heads/*>.reflogExpireUnreachable=now`   | 1            |
    /// | `gc.<refs/heads/*>.reflogExpire=now`              | 0            |
    /// | `gc.<refs/tags/*>.reflogExpireUnreachable=never`  | 0            |
    pub(super) fn resolve(&self, refname: &str) -> (i64, i64) {
        for ent in &self.entries {
            if wildmatch0(ent.pattern.as_bytes(), refname.as_bytes()) {
                return (ent.total.unwrap_or(0), ent.unreach.unwrap_or(0));
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
pub(super) fn load_reflog_config(repo: &gix::Repository, now_secs: i64) -> ReflogExpireConfig {
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
            total = parse_reflog_expiry(value.to_str_lossy().as_ref());
        }
        let mut unreach = None;
        for value in section.body().values("reflogExpireUnreachable") {
            unreach = parse_reflog_expiry(value.to_str_lossy().as_ref());
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
pub(super) fn expire_reflogs(repo: &gix::Repository) -> Result<()> {
    let now = SystemTime::now();
    let now_secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let cfg = load_reflog_config(repo, now_secs);

    // ```c
    // worktrees = get_worktrees();
    // for (p = worktrees; *p; p++) {
    //         if (!all_worktrees && !(*p)->is_current)
    //                 continue;
    //         refs_for_each_reflog(get_worktree_ref_store(*p), collect_reflog, &collected);
    // }
    // ```
    //
    // (`cmd_reflog_expire()`, builtin/reflog.c.) `--all` reaches every worktree's ref
    // store, and a linked worktree keeps its own `logs/HEAD` under its admin directory —
    // which is not below the common `logs/` this used to be the whole of.
    let mut dirs: Vec<PathBuf> = vec![repo.common_dir().join("logs")];
    if let Ok(entries) = std::fs::read_dir(repo.common_dir().join("worktrees")) {
        for entry in entries.flatten() {
            dirs.push(entry.path().join("logs"));
        }
    }
    dirs.push(repo.git_dir().join("logs"));
    dirs.sort();
    dirs.dedup();

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for dir in &dirs {
        collect_reflog_files(dir, dir, &mut files);
    }
    files.sort();
    files.dedup();

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
#[allow(clippy::too_many_arguments)]
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
pub(super) fn prune_worktrees(repo: &gix::Repository) -> Result<()> {
    let now = SystemTime::now();
    let now_secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);

    // `gc.worktreePruneExpire` (default `3.months.ago`); an empty value disables
    // the step, matching git's `cfg.prune_worktrees_expire` guard.
    let default_expire =
        parse_reflog_expiry("3.months.ago").unwrap_or(now_secs - 90 * 24 * 3600);
    let expire = match repo.config_snapshot().string("gc.worktreePruneExpire") {
        Some(v) => {
            let raw = v.to_str_lossy().into_owned();
            if raw.is_empty() {
                return Ok(());
            }
            parse_reflog_expiry(&raw).unwrap_or(default_expire)
        }
        None => default_expire,
    };

    // git's gc runs `git worktree prune --expire <gc.worktreePruneExpire>` as a child
    // (builtin/gc.c), so this calls the same `prune_worktrees()` the subcommand does rather
    // than a second, thinner copy of `should_prune_worktree()` — the copy here read a
    // relative `gitdir` recording as a path from the current directory, which reports a
    // healthy `worktree add --relative-paths` checkout as prunable and deletes it.
    super::worktree::prune_worktrees(repo, false, false, expire.max(0) as u64);
    Ok(())
}

/// git's size parser for `--max-cruft-size`: `OPT_UNSIGNED` over a `size_t`, so
/// base 0 (`0x400`) and a `k`/`m`/`g` suffix both read, and the bound is the one
/// that prints as `[0,-1]`. `None` is git's `-1` return, which the caller turns
/// into exit 129.
fn parse_size(raw: &str) -> Option<u64> {
    crate::optint::unsigned(&crate::optint::long_opt("max-cruft-size"), raw).ok()
}

/// `parse_max_cruft_size()` reports through `error()` rather than
/// `usage_with_options()`, so these are the only failures that print *no* usage
/// block — stderr is the single line and nothing else (57 and 98 bytes
/// respectively, both exit 129).
///
/// An empty value never reaches the k/m/g parser: parse-options rejects it first
/// with its generic integer message, and a value past `size_t` reports the range
/// clause instead, which is why the three messages differ.
fn bad_cruft_size(raw: &str) -> ExitCode {
    let name = crate::optint::long_opt("max-cruft-size");
    match crate::optint::unsigned(&name, raw) {
        Ok(_) => {}
        Err(e) => eprintln!("error: {e}"),
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
