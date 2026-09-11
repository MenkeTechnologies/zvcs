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
//!   * **`pack-refs --all --prune`** (plus `--auto` when the gc run is itself an
//!     automatic one), delegated to [`super::pack_refs::pack_refs`],
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
//!   * **Repacking**, delegated to [`super::repack::repack`] with the argument
//!     list [`repack_argv`] builds. `gc` writes no pack of its own: it assembles
//!     one `repack` command line and runs it (`builtin/gc.c:897`, `:919-927`,
//!     `:948-959`, `:1016-1022`), and every choice about which objects land in
//!     which pack is the child's. Cruft packs, the `--cruft`/`-a`/`-A` split,
//!     `--keep-pack=`, `--filter=`/`--filter-to=` and the reachability bitmap
//!     all come with it rather than being reproduced here.
//!   * **Commit-graph**, delegated to [`super::commit_graph::commit_graph`] as
//!     `commit-graph write --reachable`, matching `gc.writeCommitGraph`'s default
//!     of true.
//!   * **`objects/info/packs`**, delegated to
//!     [`super::update_server_info::update_server_info`], which `repack` refreshes
//!     at the end of a successful run.
//!
//! ## The pack is stock's, byte for byte
//!
//! It was not always. This module used to pack inline, partitioning the object
//! store by object id, and that enumeration order is not the one
//! `type_size_sort()` and `compute_write_order()` work from — so the pack, and
//! the checksum its filename carries, differed from stock's while every other
//! observable stayed the same. Measured on a five-commit fixture with a tag and
//! a second branch, git 2.55.0 against the port before the delegation:
//!
//! ```text
//! $ git  gc -q && ls .git/objects/pack/*.pack
//! pack-7e6496f9d634596b18115a5246892b08ad4f2b2a.pack
//! $ zvcs gc -q && ls .git/objects/pack/*.pack
//! pack-7f2cb2cba512c726b828561efbe48764c73e4661.pack
//! ```
//!
//! Driving the same fixture through the child's own argument list —
//! `pack-refs --all --prune`, `reflog expire --all`, then
//! `repack -d -l --cruft --cruft-expiration=2.weeks.ago` — already produced
//! `7e6496f9…` under both binaries, which is what made the second packing path
//! the defect rather than the pack writer.
//!
//! # Not performed
//!
//! These are skipped, and a `gc` that exits 0 has **not** done them:
//!
//!   1. **Reusing the deltas an existing pack already holds.** `pack-objects`
//!      keeps them unless `--no-reuse-delta` says otherwise, and the port's
//!      writer always searches afresh — so a `gc` over a repository whose
//!      objects are *already* packed can still write a different pack from
//!      stock's. Stock's own second `gc` moves to a new checksum and stays
//!      there; this port's is idempotent from the first:
//!
//!      ```text
//!      $ git  gc -q; git  gc -q   # 7e6496f9… then 4bbc768e…
//!      $ zvcs gc -q; zvcs gc -q   # 7e6496f9… both times
//!      ```
//!
//!      Confirmed to be delta reuse and nothing else: stock's second pass under
//!      `-f`, which is `--no-reuse-delta`, reproduces the port's answer. The gap
//!      is in the pack writer, not here.
//!   2. **Writing `gc.log`.** A failure is reported on stderr here rather than
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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::objs::Kind;

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
    // `--expire-to=<dir>`, git's `cfg.repack_expire_to`. It is forwarded to the
    // `repack` child verbatim (builtin/gc.c:642-643) and nowhere else, which is
    // why the value is kept rather than dropped with the rest of `VALUE_OPTS`.
    let mut expire_to: Option<String> = None;
    // The raw text of the last `--prune=<value>`, kept so `parse_expiry_date()`
    // can be applied once after parsing, the way `cmd_gc()` applies it to
    // `prune_expire_arg` — last occurrence wins, and an unreadable earlier one is
    // never seen.
    let mut prune_raw: Option<String> = None;
    // git's `prune_expire_arg`, which starts at a sentinel meaning "the command
    // line said nothing" (builtin/gc.c:860-861). `None` here is that sentinel;
    // `Some(None)` is `--no-prune`'s NULL and `Some(Some(v))` is `--prune=<v>`.
    // Only a non-sentinel value replaces `cfg.prune_expire` (:912-915), so a
    // bare `--prune` — an `OPTARG` whose `defval` *is* the sentinel — leaves
    // `gc.pruneExpire` and the built-in `2.weeks.ago` in charge. The text is
    // kept verbatim because `add_repack_all_option()` forwards it verbatim.
    let mut prune_expire_arg: Option<Option<String>> = None;
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
    // git's `int keep_largest_pack = -1`: unasked, on, or off, with the last two
    // both overriding `gc.bigPackThreshold`.
    let mut keep_largest_pack: Option<bool> = None;

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
            "--no-prune" => {
                prune = Some(Prune::Disabled);
                prune_expire_arg = Some(None);
            }
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
            // ```c
            // if (keep_largest_pack != -1) {
            //         if (keep_largest_pack)
            //                 find_base_packs(&keep_pack, 0);
            // } else if (cfg.big_pack_threshold) {
            //         find_base_packs(&keep_pack, cfg.big_pack_threshold);
            // }
            // ```
            //
            // (`builtin/gc.c:951-956`.) The flag is a tri-state: given, it
            // *overrides* `gc.bigPackThreshold` — including `--no-keep-largest-pack`,
            // which keeps nothing at all where the config would have kept the big
            // packs.
            "--keep-largest-pack" => keep_largest_pack = Some(true),
            "--no-keep-largest-pack" => keep_largest_pack = Some(false),
            "--force" | "--no-force" => {}
            // `--no-expire-to` is a valid negation (USAGE spells it `--[no-]expire-to`);
            // `--max-cruft-size` has no `--no-` form, so one is left to error out.
            // An `OPT_STRING` unset stores a NULL rather than a value, so the
            // repack child is handed no `--expire-to` at all.
            "--no-expire-to" => expire_to = None,
            // `--prune=<date>` is the only optional-value option.
            _ if a.starts_with("--prune=") => {
                // `--prune` is an `OPT_STRING`: the value is only kept here and
                // checked once, after parsing, on whichever occurrence came last.
                prune_raw = Some(a["--prune=".len()..].to_string());
                prune_expire_arg = Some(Some(a["--prune=".len()..].to_string()));
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
                if name == "expire-to" {
                    expire_to = Some(value.clone());
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
                if let Some(v) = a.strip_prefix("--expire-to=") {
                    expire_to = Some(v.to_string());
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
    // anything, so nothing below this point may run either. `need_to_gc()`
    // decides in two steps and the second one was missing here: the counters
    // first, and then the `pre-auto-gc` hook, which gets the last word.
    //
    // The decision also *is* the repack's argument list on this path: git calls
    // `need_to_gc()` with the half-built `repack_args` and lets it append, so
    // which of the two counters tripped is what separates a full rewrite from
    // an incremental one. It runs here, ahead of the foreground tasks
    // (builtin/gc.c:936 versus :1012), so the counts it reads are the ones from
    // before `pack-refs` and `reflog expire` touched anything.
    let auto_repack = match auto {
        false => None,
        true => match auto_repack_choice(&repo).filter(|_| pre_auto_gc_allows(&repo)) {
            Some(choice) => Some(choice),
            None => return Ok(ExitCode::SUCCESS),
        },
    };

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

    // `cfg.prune_expire` itself, as the text `add_repack_all_option()` forwards.
    // `GC_CONFIG_INIT` seeds it with `"2.weeks.ago"` (builtin/gc.c:161),
    // `gc.pruneExpire` replaces it (:201-204), and a non-sentinel
    // `prune_expire_arg` replaces it again (:912-915) — `--no-prune`'s NULL
    // included, which is the one case that leaves the repack child with no
    // expiry argument at all.
    let prune_expire: Option<String> = match prune_expire_arg {
        Some(explicit) => explicit,
        None => Some(
            repo.config_snapshot()
                .string("gc.pruneExpire")
                .and_then(|v| v.to_str().ok().map(str::to_string))
                .unwrap_or_else(|| "2.weeks.ago".to_string()),
        ),
    };

    // `--cruft` beats the config, which beats git's built-in default of true.
    let cruft = cruft.unwrap_or_else(|| {
        repo.config_snapshot().boolean("gc.cruftPacks").unwrap_or(true)
    });
    // git's order: pack-refs, then reflog expire, then repack, then prune, then
    // worktree prune, then rerere gc, then commit-graph write.
    if pack_refs_enabled(&repo) {
        // `maintenance_task_pack_refs()` (builtin/gc.c:168-175) runs
        // `pack-refs --all --prune`, and an *automatic* run adds `--auto` on top
        // — which is what makes an auto-gc leave `packed-refs` alone until the
        // loose-ref count is worth the rewrite. Confirmed against git 2.55.0:
        //
        // ```text
        // $ GIT_TRACE=1 git -c gc.auto=1 -c gc.autoPackLimit=1 gc --auto --quiet
        // trace: run_command: git pack-refs --all --prune --auto
        // $ GIT_TRACE=1 git gc --quiet
        // trace: run_command: git pack-refs --all --prune
        // ```
        //
        // The threshold itself is [`super::pack_refs`]'s port of
        // `should_pack_refs()`; all that is decided here is whether to ask for it.
        let mut argv = vec![
            "pack-refs".to_string(),
            "--all".to_string(),
            "--prune".to_string(),
        ];
        if auto {
            argv.push("--auto".to_string());
        }
        super::pack_refs::pack_refs(&argv)?;
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
    // dies: the read belongs to the packing, not to the option parsing. The
    // delegate reaches it too, but only after its own argument diagnostics, so
    // the read is arranged here to keep git's order. See
    // [`super::pack_objects::PackConfig`].
    match crate::repo_settings::RepoSettings::load(&repo)
        .and_then(|settings| super::pack_objects::PackConfig::load(&repo, &settings))
    {
        Ok(_) => {}
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    }

    // `gc` does not pack anything itself. It builds one `repack` argument list
    // and runs it as a child (builtin/gc.c:1016-1022), and every choice about
    // *which* objects land in *which* pack is that child's — so the pack's
    // bytes, and the checksum its filename carries, are `repack`'s to decide.
    //
    // Duplicating the packing here instead is what made `gc` write a pack stock
    // git never writes. The two paths enumerated objects differently: this
    // module partitioned the store by object id, while `repack`'s port ranks by
    // the `pack-objects` traversal that `setup_revisions()` seeds from
    // `--all --reflog --indexed-objects`, and that order is the final tiebreak
    // of `type_size_sort()` and the iteration order of `compute_write_order()`.
    // Measured on a five-commit fixture with a tag and a second branch:
    //
    // ```text
    // $ git gc -q && ls .git/objects/pack/*.pack     # stock 2.55.0
    // pack-7e6496f9d634596b18115a5246892b08ad4f2b2a.pack
    // $ zvcs gc -q && ls .git/objects/pack/*.pack    # before this change
    // pack-7f2cb2cba512c726b828561efbe48764c73e4661.pack
    // ```
    //
    // and the same fixture driven through the child's own argument list —
    // `pack-refs --all --prune`, `reflog expire --all`, then
    // `repack -d -l --cruft --cruft-expiration=2.weeks.ago` — already produced
    // `7e6496f9…` under *both* binaries. So the divergence was never in the
    // pack writer; it was in `gc` having a second one.
    let argv = repack_argv(
        &repo,
        &RepackArgs {
            aggressive,
            quiet,
            auto: auto_repack.as_ref(),
            prune_expire: prune_expire.as_deref(),
            cruft,
            max_cruft_size,
            expire_to: expire_to.as_deref(),
            keep_largest_pack,
            repack_filter: repack_filter.as_deref(),
            repack_filter_to: repack_filter_to.as_deref(),
        },
    );
    // ```c
    // if (run_command(&repack_cmd))
    //         die(FAILED_RUN, repack_args.v[0]);
    // ```
    //
    // (`builtin/gc.c:1021-1022`; `FAILED_RUN` is `"failed to run %s"`.) The
    // child has already said what went wrong, so `gc` adds one line naming the
    // step that failed and exits 128:
    //
    // ```text
    // $ git gc --quiet          # a ref naming an object the repository lacks
    // error: refs/heads/dangling does not point to a valid object!
    // fatal: bad object refs/heads/dangling
    // fatal: failed to run repack
    // ```
    //
    // The delegate reports through the error it returns rather than through a
    // child's stderr, so the child's line is rendered here, in its place, before
    // `gc`'s own. An error that is neither of git's two shapes is this port
    // speaking for itself and is left to the caller to render as such.
    if let Err(err) = super::repack::repack(&argv) {
        match err.downcast_ref::<crate::fatal::Fatal>() {
            Some(fatal) => eprintln!("fatal: {fatal}"),
            None if err.downcast_ref::<crate::fatal::Silent>().is_none() => return Err(err),
            None => {}
        }
        eprintln!("fatal: failed to run repack");
        return Err(anyhow::Error::new(crate::fatal::Silent(crate::fatal::EXIT_FATAL)));
    }

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
    //
    // `gc` reaches `write_commit_graph_reachable()` *without* having called
    // `disable_replace_refs()`, so the replace half of `commit_graph_compatible()`
    // is live here in a way it never is for the `commit-graph` command: a
    // repository holding a `refs/replace/*` entry gets no graph out of `gc`,
    // while `git commit-graph write --reachable` in the same repository writes
    // one. Both were measured on git 2.55.0. Grafts and shallowness are checked
    // inside the write itself, which is where git checks them too.
    if repo
        .config_snapshot()
        .boolean("gc.writeCommitGraph")
        .unwrap_or(true)
        && !super::commit_graph::replacements_in_use(&repo)
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
//
// `gc` never writes a pack. It assembles one `repack` command line and runs it
// (`builtin/gc.c:897` seeds it, :919-927 and :948-959 finish it, :1016-1022 runs
// it), so everything below is argument construction and the packing itself
// belongs to [`super::repack`].

/// What `need_to_gc()` appended to the repack's arguments, which is also which
/// of its two counters tripped.
///
/// ```c
/// if (too_many_packs(cfg)) {
///         [...]
///         add_repack_all_option(cfg, &keep_pack, repack_args);
/// } else if (too_many_loose_objects(cfg->gc_auto_threshold))
///         add_repack_incremental_option(repack_args);
/// else
///         return 0;
/// ```
///
/// (`builtin/gc.c:679-710`, git 2.55.0.) The two are not interchangeable: too
/// many *packs* asks for the rewrite that reduces their number, while too many
/// loose *objects* asks only that the loose ones be gathered, leaving every
/// existing pack where it is.
enum AutoRepack {
    /// `too_many_packs()`: the full rewrite, carrying the `--keep-pack=` names
    /// `find_base_packs()` chose.
    AllIntoOne(Vec<String>),
    /// `too_many_loose_objects()`: `add_repack_incremental_option()`, which is
    /// `--no-write-bitmap-index` and nothing else (`builtin/gc.c:659-662`).
    Incremental,
}

/// `need_to_gc()`'s counters, and the arguments the winning one implies.
///
/// `None` is git's `return 0` — no collection is due — which `cmd_gc()` turns
/// into an exit before anything is touched. The `pre-auto-gc` veto that closes
/// the same function is [`pre_auto_gc_allows`], applied by the caller.
///
/// # The memory estimate, and why the kept pack is dropped here
///
/// git's `too_many_packs` branch keeps the largest pack only when repacking it
/// would not fit in half of physical RAM:
///
/// ```c
/// struct packed_git *p = find_base_packs(&keep_pack, 0);
/// mem_have = total_ram();
/// mem_want = estimate_repack_memory(cfg, p);
/// if (!mem_have || mem_want < mem_have / 2)
///         string_list_clear(&keep_pack, 0);
/// ```
///
/// (`builtin/gc.c:690-702`.) `estimate_repack_memory()` (:574-618) is a sum of
/// C `sizeof`s — `struct object_entry`, `struct blob`, `struct tree`,
/// `struct object *`, `off_t + uint32_t` — times the approximate object count,
/// plus the pack's own bytes and the two delta caches. Those sizes are the C
/// build's, not anything observable from here, so the estimate is not
/// reproducible byte for byte. What *is* certain is the comparison's outcome
/// for any repository at which `gc` is a fast operation: the per-object terms
/// come to a few hundred bytes, so a repository would need on the order of ten
/// million objects before the estimate reached half the RAM of a machine that
/// could hold it. This port therefore takes the branch git takes there and
/// clears the list. `gc.bigPackThreshold` is unaffected — it is the other
/// branch, and it is reproduced exactly.
fn auto_repack_choice(repo: &gix::Repository) -> Option<AutoRepack> {
    // `gc.auto` at zero or below disables automatic gc outright — git returns
    // before it ever counts packs, so a repository over `gc.autoPackLimit` is
    // still left alone.
    let auto_threshold = repo.config_snapshot().integer("gc.auto").unwrap_or(6700);
    if auto_threshold <= 0 {
        return None;
    }
    if too_many_packs(repo) {
        let pack_dir = repo.objects.store_ref().path().join("pack");
        let keep = match big_pack_threshold(repo) {
            0 => Vec::new(),
            threshold => {
                let kept = find_base_packs(&pack_dir, threshold);
                // ```c
                // if (keep_pack.nr >= cfg->gc_auto_pack_limit) {
                //         cfg->big_pack_threshold = 0;
                //         string_list_clear(&keep_pack, 0);
                //         find_base_packs(&keep_pack, 0);
                // }
                // ```
                //
                // (`builtin/gc.c:684-688`.) Keeping as many packs as the limit
                // allows would leave the collection with nothing to do, so the
                // threshold is abandoned and only the largest pack is kept.
                let limit = repo.config_snapshot().integer("gc.autoPackLimit").unwrap_or(50);
                match i64::try_from(kept.len()).is_ok_and(|n| n >= limit) {
                    true => find_base_packs(&pack_dir, 0),
                    false => kept,
                }
            }
        };
        return Some(AutoRepack::AllIntoOne(keep));
    }
    match too_many_loose_objects(repo, auto_threshold) {
        true => Some(AutoRepack::Incremental),
        false => None,
    }
}

/// Everything `cmd_gc()` has resolved by the time it finishes the repack's
/// argument list, gathered so [`repack_argv`] reads as the C does.
struct RepackArgs<'a> {
    /// `--aggressive`, which pushes `-f` plus the two `gc.aggressive*` values.
    aggressive: bool,
    /// `opts.quiet`, which pushes `-q`.
    quiet: bool,
    /// `None` for a manual run — the `else` at `builtin/gc.c:948` — and `Some`
    /// for an automatic one, carrying what `need_to_gc()` decided.
    auto: Option<&'a AutoRepack>,
    /// `cfg.prune_expire`, verbatim. `None` is `--no-prune`'s NULL.
    prune_expire: Option<&'a str>,
    /// `cfg.cruft_packs`.
    cruft: bool,
    /// `cfg.max_cruft_size`; zero and unset are the same to git's `if`.
    max_cruft_size: Option<u64>,
    /// `cfg.repack_expire_to`.
    expire_to: Option<&'a str>,
    /// `keep_largest_pack`, git's tri-state `int` at `-1` until asked.
    keep_largest_pack: Option<bool>,
    /// `gc.repackFilter`, already filtered to a non-empty value.
    repack_filter: Option<&'a str>,
    /// `gc.repackFilterTo`, likewise.
    repack_filter_to: Option<&'a str>,
}

/// The `repack` command line `gc` runs, built in git's order.
///
/// ```c
/// strvec_pushl(&repack_args, "repack", "-d", "-l", NULL);
/// [...]
/// if (aggressive) {
///         strvec_push(&repack_args, "-f");
///         if (cfg.aggressive_depth > 0)
///                 strvec_pushf(&repack_args, "--depth=%d", cfg.aggressive_depth);
///         if (cfg.aggressive_window > 0)
///                 strvec_pushf(&repack_args, "--window=%d", cfg.aggressive_window);
/// }
/// if (opts.quiet)
///         strvec_push(&repack_args, "-q");
/// ```
///
/// (`builtin/gc.c:897, 919-927`, git 2.55.0.) The order matters only in that it
/// is the order stock hands the child, and the child is the same port either
/// way; it is reproduced because a divergence in it would be a divergence in
/// what was read.
fn repack_argv(repo: &gix::Repository, args: &RepackArgs<'_>) -> Vec<String> {
    let mut argv = vec!["repack".to_string(), "-d".to_string(), "-l".to_string()];
    if args.aggressive {
        // `-f` reaches `pack-objects` as `--no-reuse-delta`: the point of
        // `--aggressive` is to search every pair again rather than keep the
        // deltas already on disk, and a widened window that reused them would
        // not use it.
        argv.push("-f".to_string());
        let snapshot = repo.config_snapshot();
        let depth = snapshot.integer("gc.aggressiveDepth").unwrap_or(50);
        if depth > 0 {
            argv.push(format!("--depth={depth}"));
        }
        let window = snapshot.integer("gc.aggressiveWindow").unwrap_or(250);
        if window > 0 {
            argv.push(format!("--window={window}"));
        }
    }
    if args.quiet {
        argv.push("-q".to_string());
    }

    match args.auto {
        Some(AutoRepack::Incremental) => argv.push("--no-write-bitmap-index".to_string()),
        Some(AutoRepack::AllIntoOne(keep)) => add_repack_all_option(&mut argv, args, keep),
        None => {
            // ```c
            // if (keep_largest_pack != -1) {
            //         if (keep_largest_pack)
            //                 find_base_packs(&keep_pack, 0);
            // } else if (cfg.big_pack_threshold) {
            //         find_base_packs(&keep_pack, cfg.big_pack_threshold);
            // }
            // ```
            //
            // (`builtin/gc.c:951-956`.) The flag is a tri-state that *overrides*
            // `gc.bigPackThreshold` — `--no-keep-largest-pack` included, which
            // keeps nothing at all where the config would have kept the big packs.
            let pack_dir = repo.objects.store_ref().path().join("pack");
            let keep = match args.keep_largest_pack {
                Some(true) => find_base_packs(&pack_dir, 0),
                Some(false) => Vec::new(),
                None => match big_pack_threshold(repo) {
                    0 => Vec::new(),
                    threshold => find_base_packs(&pack_dir, threshold),
                },
            };
            add_repack_all_option(&mut argv, args, &keep);
        }
    }
    argv
}

/// `add_repack_all_option()` (`builtin/gc.c:628-657`, git 2.55.0).
///
/// ```c
/// if (cfg->prune_expire && !strcmp(cfg->prune_expire, "now")
///         && !(cfg->cruft_packs && cfg->repack_expire_to))
///         strvec_push(args, "-a");
/// else if (cfg->cruft_packs) {
///         strvec_push(args, "--cruft");
///         if (cfg->prune_expire)
///                 strvec_pushf(args, "--cruft-expiration=%s", cfg->prune_expire);
///         if (cfg->max_cruft_size)
///                 strvec_pushf(args, "--max-cruft-size=%lu", cfg->max_cruft_size);
///         if (cfg->repack_expire_to)
///                 strvec_pushf(args, "--expire-to=%s", cfg->repack_expire_to);
/// } else {
///         strvec_push(args, "-A");
///         if (cfg->prune_expire)
///                 strvec_pushf(args, "--unpack-unreachable=%s", cfg->prune_expire);
/// }
/// ```
///
/// The first test is a literal `strcmp` against `"now"`, not a parsed expiry:
/// `--prune=all` means the same date to `parse_expiry_date()` and still takes
/// the cruft branch, where `--cruft-expiration=all` expires the same objects by
/// a different route. `--expire-to` is the exception that keeps `-a` away even
/// under `now`, because the objects being expired have somewhere to go.
///
/// What the three branches do to an unreachable object, one flag at a time
/// against git 2.55.0 on the `conflicted` fixture — whose half-finished merge
/// leaves two behind (`2ae666ad…` tree, `5eb9640f…` blob):
///
/// | invocation                 | branch    | loose | packs | `.mtimes` |
/// |----------------------------|-----------|-------|-------|-----------|
/// | `gc` (default)             | `--cruft` | 0     | 2     | 1         |
/// | `gc --no-cruft`            | `-A`      | 2     | 1     | 0         |
/// | `gc --prune=now`           | `-a`      | 0     | 1     | 0         |
/// | `gc --no-cruft --no-prune` | `-A`      | 2     | 1     | 0         |
///
/// The second row and the fourth agree because `-A`'s
/// `--unpack-unreachable=<date>` only loosens what is *older* than the date, and
/// nothing in a fixture is; `--no-prune` drops the argument entirely and reaches
/// the same place by never expiring at all.
fn add_repack_all_option(argv: &mut Vec<String>, args: &RepackArgs<'_>, keep_pack: &[String]) {
    if args.prune_expire == Some("now") && !(args.cruft && args.expire_to.is_some()) {
        argv.push("-a".to_string());
    } else if args.cruft {
        argv.push("--cruft".to_string());
        if let Some(expire) = args.prune_expire {
            argv.push(format!("--cruft-expiration={expire}"));
        }
        if let Some(size) = args.max_cruft_size.filter(|n| *n != 0) {
            argv.push(format!("--max-cruft-size={size}"));
        }
        if let Some(dir) = args.expire_to {
            argv.push(format!("--expire-to={dir}"));
        }
    } else {
        argv.push("-A".to_string());
        if let Some(expire) = args.prune_expire {
            argv.push(format!("--unpack-unreachable={expire}"));
        }
    }

    // `keep_one_pack()` (builtin/gc.c:621-626) pushes the *basename* of each
    // pack `find_base_packs()` chose, which is the `.pack` file's name.
    for base in keep_pack {
        argv.push(format!("--keep-pack={base}.pack"));
    }

    if let Some(filter) = args.repack_filter {
        argv.push(format!("--filter={filter}"));
    }
    if let Some(filter_to) = args.repack_filter_to {
        argv.push(format!("--filter-to={filter_to}"));
    }
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

/// `find_base_packs()` (builtin/gc.c:485-505): the packs `gc` asks `repack` to
/// keep.
///
/// ```c
/// repo_for_each_pack(the_repository, p) {
///         if (!p->pack_local || p->is_cruft)
///                 continue;
///         if (limit) {
///                 if (p->pack_size >= limit)
///                         string_list_append(packs, p->pack_name);
///         } else if (!base || base->pack_size < p->pack_size) {
///                 base = p;
///         }
/// }
/// ```
///
/// A `limit` of 0 means "the single largest", which is `--keep-largest-pack`;
/// any other value is `gc.bigPackThreshold`, and every pack at or over it is
/// kept. A cruft pack — one with a `.mtimes` beside it — is never a base: it
/// holds the unreachable objects the next collection has to be free to rewrite.
fn find_base_packs(pack_dir: &Path, limit: u64) -> Vec<String> {
    let Some(names) = super::prune::read_dir_raw(pack_dir) else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    let mut largest: Option<(String, u64)> = None;
    for name in names {
        let name = name.to_string_lossy().into_owned();
        let Some(base) = name.strip_suffix(".idx") else { continue };
        if pack_dir.join(format!("{base}.mtimes")).exists() {
            continue;
        }
        let Ok(size) = std::fs::metadata(pack_dir.join(format!("{base}.pack"))).map(|md| md.len())
        else {
            continue;
        };
        match limit {
            0 => {
                if largest.as_ref().is_none_or(|(_, biggest)| *biggest < size) {
                    largest = Some((base.to_string(), size));
                }
            }
            limit if size >= limit => kept.push(base.to_string()),
            _ => {}
        }
    }
    kept.extend(largest.map(|(base, _)| base));
    kept
}

/// A `.mtimes` sidecar: `MTME`, version 1, the hash identifier, then one 32-bit
/// timestamp per object *in index order*, and the two trailing checksums.
///
/// Confirmed against a git 2.55.0 cruft pack of two objects, whose 60 bytes are
/// the 12-byte header, two timestamps, the pack checksum and its own.
pub(super) fn mtimes_bytes(hash: gix::hash::Kind, stamps: &[u32], pack_id: &[u8]) -> Result<Vec<u8>> {
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

/// `need_to_gc()`'s closing two lines (builtin/gc.c:404-406):
///
/// ```c
/// if (run_hooks("pre-auto-gc"))
///         return 0;
/// return 1;
/// ```
///
/// Once the counters say a collection is warranted, the `pre-auto-gc` hook can
/// still veto it by exiting non-zero, and the veto is silent — `cmd_gc()` just
/// returns 0. Only an automatic run consults it; plain `gc` never calls
/// `need_to_gc()` at all. A hook that cannot be run does not veto.
pub(super) fn pre_auto_gc_allows(repo: &gix::Repository) -> bool {
    crate::hooks::run(repo, "pre-auto-gc", &[], None).unwrap_or(true)
}

/// `need_to_gc()`'s counters: true when either the loose-object or the
/// pack-count heuristic trips. Ported from `builtin/gc.c`; both halves compare
/// with `>`, and a non-positive threshold disables that half. The `pre-auto-gc`
/// veto that closes the same function is [`pre_auto_gc_allows`].
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
/// # Why the check is here and not left to the child
///
/// The split a valid filter asks for *is* performed — [`super::repack`] writes
/// the second pack, and `gc` reaches it by forwarding `--filter=`/`--filter-to=`
/// like git does. What this covers is the pair of refusals an *invalid* pairing
/// draws, and their two extra lines:
///
/// ```text
/// $ git -c gc.repackFilter=bogusfilter gc
/// fatal: invalid filter-spec 'bogusfilter'
/// fatal: failed to run repack
/// ```
///
/// The spec is rejected while the child's parse-options is still running, so it
/// beats every pre-flight the child would otherwise reach first — which is why
/// it is answered before the delegate is entered rather than after.
///
/// Both values come from configuration alone, so a repository can be left in a
/// state where every `gc` stops here.
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
