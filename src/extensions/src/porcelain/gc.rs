//! `git gc` — repository housekeeping. **Not ported: this module bails.**
//!
//! What is covered here is only the argument surface, and only because those
//! paths are byte-verifiable without doing any housekeeping at all:
//!   * `-h` → git's 744-byte usage block on stdout, exit 129
//!   * an unknown long option → `error: unknown option `<name>'` + usage on
//!     stderr, exit 129
//!   * an unknown short switch → `error: unknown switch `<c>'` + usage, exit 129
//!   * a positional argument → the bare usage block on stderr, exit 129
//!   * a value-taking option with no value → `error: option `<name>' requires a
//!     value` + usage, exit 129
//! (checked against git 2.55.0; see the `USAGE` constant.)
//!
//! Everything else — i.e. `gc` actually running — bails with the specific
//! substrate that is missing. It is deliberately *not* approximated, because
//! stock `git gc` writes nothing to stdout and exits 0, so any partial
//! implementation would be indistinguishable from success while leaving the
//! repository in a state stock git would never produce. That is the worst
//! possible failure mode for a differential harness that compares post-command
//! repository state.
//!
//! The missing substrate, concretely, in the vendored crates under `src/ported`:
//!
//!   1. **Delta compression.** `gc` delegates the bulk of its work to
//!      `git repack -d`, whose whole purpose is to produce a delta-compressed
//!      pack. `gix-pack`'s pack writer cannot compute deltas: its only mode is
//!      documented as "Copy base objects and deltas from packs, while non-packed
//!      objects will be treated as base objects (i.e. without trying to delta
//!      compress them)" (`gix-pack/src/data/output/entry/iter_from_counts.rs`).
//!      A pack written through it would differ from git's in size and in every
//!      byte, and loose objects would be stored undeltified.
//!   2. **Cruft packs.** `--cruft` is the default since git 2.37; it requires
//!      writing a `.mtimes` file alongside the pack. No `.mtimes` reader or
//!      writer exists anywhere in `gix-pack`.
//!   3. **Commit-graph writing.** `gc.writeCommitGraph` defaults to true.
//!      `gix-commitgraph` is read-only — it ships `file`, `init`, `access` and
//!      `verify` modules and no writer (`ls src/ported/gix-commitgraph/src`).
//!   4. **Reflog expiry.** `gc` runs `git reflog expire --all`. `gix-ref` only
//!      ever appends to a reflog as a side effect of a ref transaction; it
//!      cannot rewrite or truncate one. This is the same gap already documented
//!      in `reflog.rs`, which bails on `expire` for the same reason.
//!   5. **Loose-object pruning.** `gc` runs `git prune`. `gix-odb`'s loose store
//!      exposes no removal API, and there is no reachability-plus-mtime pruner.
//!   6. `git worktree prune` and `git rerere gc`, both of which `gc` invokes,
//!      have no counterpart in the vendored crates either.
//!
//! `--auto` is bailed on as well rather than being treated as a no-op. Its
//! decision is made by git's `too_many_loose_objects()` / `too_many_packs()`
//! heuristic, which the manual explicitly declines to specify beyond the
//! `gc.auto` and `gc.autoPackLimit` thresholds. Returning 0 without checking
//! would be correct only below those thresholds and silently wrong above them.

use anyhow::{bail, Result};
use std::process::ExitCode;

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

/// `git gc` — argument validation only; the housekeeping itself is not ported.
///
/// Returns 129 with git's own usage output for `-h` and for every malformed
/// invocation. Any well-formed invocation bails, naming the substrate that is
/// missing; see the module documentation for the full list.
pub fn gc(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `gc` takes no positional of its
    // own (a positional is a usage error), so dropping a leading copy is
    // unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("gc") => &args[1..],
        _ => args,
    };

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
            // Boolean flags and their `--no-` forms, exactly as listed in USAGE.
            "-q" | "--quiet" | "--no-quiet" | "--prune" | "--no-prune" | "--cruft"
            | "--no-cruft" | "--aggressive" | "--no-aggressive" | "--auto" | "--no-auto"
            | "--detach" | "--no-detach" | "--force" | "--no-force" | "--keep-largest-pack"
            // `--no-expire-to` is a valid negation (USAGE spells it `--[no-]expire-to`);
            // `--max-cruft-size` has no `--no-` form, so one is left to error out.
            | "--no-keep-largest-pack" | "--no-expire-to" => {}
            // `--prune=<date>` is the only optional-value option.
            _ if a.starts_with("--prune=") => {}
            _ if VALUE_OPTS
                .iter()
                .any(|o| a.strip_prefix("--") == Some(*o)) =>
            {
                let name = &a[2..];
                if args.get(i + 1).is_none() {
                    return Ok(usage_error(Some(&format!(
                        "option `{name}' requires a value"
                    ))));
                }
                i += 1;
            }
            _ if VALUE_OPTS
                .iter()
                .any(|o| a.starts_with(&format!("--{o}="))) => {}
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

    bail!(
        "gc is not ported: repacking needs delta compression (gix-pack writes base objects only), \
         cruft packs need a .mtimes writer, gc.writeCommitGraph needs a commit-graph writer \
         (gix-commitgraph is read-only), reflog expiry needs reflog rewriting in gix-ref, and \
         pruning needs loose-object removal in gix-odb — none exist in the vendored crates \
         (ported: -h and argument validation only)"
    )
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
