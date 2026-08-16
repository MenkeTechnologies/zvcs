//! `git repack` — pack unpacked objects into a pack.
//!
//! The argument surface is covered byte-for-byte, and the command then does the
//! repacking for real: it writes a pack, its `.idx` and its `.rev`, optionally
//! prunes the loose objects it just packed (`-d`) and refreshes
//! `objects/info/packs` (unless `-n` or `repack.updateServerInfo` is false).
//!
//! # Pack bytes differ from git's, but the compression does not
//!
//! The pack is built by `pack-objects`' writer, so it is delta-compressed by
//! git's own machinery ported into [`gix_pack::data::output::delta`], honouring
//! `pack.window`, `pack.depth`, `pack.windowMemory`, `pack.deltaCacheSize`,
//! `pack.deltaCacheLimit`, `pack.threads` and `pack.compression`.
//! `repack.useDeltaBaseOffset` (default true, as in git) decides whether a delta
//! names its base by pack offset or by object id.
//!
//! The bytes still differ from git's, because objects are enumerated in this
//! module's own order rather than git's `compute_write_order()`; since a pack's
//! filename embeds its checksum, the name differs too. What the pack *is* is
//! valid, complete and comparable in size, with a correct `.idx` and `.rev`
//! beside it. `-f` *is* honoured — it becomes `pack-objects --no-reuse-delta`,
//! which the writer acts on, because deltas are otherwise kept from the pack
//! they are already in. `-F` controls reuse of a stored entry's *bytes*, which
//! this writer never does, so it is accepted as a no-op.
//!
//! # Argument surface
//!
//! Covered because these paths are byte-verifiable without touching the object
//! database:
//! ```text
//!   * `-h` → git's 2699-byte usage block on stdout, exit 129
//!   * git's parse-options behaviour for every option in the table, including
//!     unambiguous long-option abbreviation (`--qui` → `--quiet`), `--no-`
//!     negations, `=value` vs. separate-argv values, clustered short switches,
//!     and `-g<n>` / `-g <n>`
//!   * the five distinct parse-options diagnostics, each byte-for-byte:
//!     `unknown option`, `unknown switch`, `ambiguous option`, `takes no value`,
//!     `requires a value`, plus the integer/magnitude value-type messages and
//!     the `not in range [-2147483648,2147483647]` message for an integer that
//!     overflows a C `int` once its `k`/`m`/`g` suffix is applied
//!   * `--filter` spec validation, which is a parse-options callback and so
//!     dies (exit 128) at its own position in argv: `invalid filter-spec`,
//!     `expected 'tree:<depth>'`, `expected something after combine:`,
//!     `sparse:path filters support has been dropped`, and the
//!     `object:type=<type>` message
//!   * the pre-flight option-conflict `fatal:`s that stock git emits before it
//!     does any work at all (exit 128): the `-A`/`-k`/`--cruft` triad, geometric
//!     vs. `-a`/`-A`, incremental-with-bitmaps, `--filter-to` without
//!     `--filter`, and — last of the five — `invalid --name-hash-version
//!     option: <n>` for any version above 2
//! ```
//! (all checked against git 2.55.0.)
//!
//! # What repacking does here
//!
//! ```text
//!   * **The object set** is git's `--all --reflog --indexed-objects`: the
//!     closure over every ref, `HEAD`, every reflog entry, and the index (its
//!     blobs at every stage plus the cache-tree), which is exactly the seed
//!     [`super::prune::collect_roots`] already builds for `prune` and
//!     [`super::prune::close_over`] already closes. Verified against git 2.55.0
//!     on the eight harness fixtures: the sets agree object-for-object,
//!     including the `conflicted` fixture, where the two objects left over from
//!     the aborted merge are reachable from neither refs nor index and so are
//!     packed by neither implementation.
//!   * **Incremental vs. `-a`.** Without `-a`/`-A`/`--cruft`, objects an
//!     existing pack already holds are excluded, and a run with nothing left to
//!     pack prints `Nothing new to pack.` on *stdout* and writes no pack — git's
//!     wording, stream and exit code, including the way `-q`/`--quiet`
//!     suppresses just that notice. That notice does not end the run: `-d` still
//!     prunes and `--write-midx` still writes, exactly as `if (!names.nr)` in
//!     `cmd_repack()` only reports and falls through. With `-a` the whole set is
//!     repacked regardless.
//!   * **`.idx` and `.rev`** are written straight from the pack writer's own
//!     record of where each object landed, the `.rev` unless
//!     `pack.writeReverseIndex` is false. The pack is named after its trailing
//!     checksum, as git names it.
//!   * **`-d`** removes the packs the new one supersedes and then prunes the
//!     loose objects now present in a pack, delegating to the real
//!     [`super::prune_packed::prune_packed`] port. A pack with a `.keep`, and
//!     any pack named by `--keep-pack`, is left alone.
//!   * **`--filter`** makes git write a *second* pack for the filtered-out
//!     objects, so two `.pack`/`.idx`/`.rev` triples appear rather than one.
//!     `blob:none` and `blob:limit=<n>` are applied to the traversal, which the
//!     index objects are then unioned back into — the model git's own output
//!     confirms (on the `branched` fixture, `blob:none` yields 11 of 13 objects:
//!     13 less 4 blobs, plus the 2 blobs the index holds).
//!     That second pack is not the traversal's leftovers but *the existing packs
//!     minus the new one*: `write_filtered_pack()` runs `pack-objects
//!     --stdin-packs` over the non-kept and cruft packs with the new pack
//!     excluded by `^`. Two consequences, both reproduced. A filtered-out object
//!     that was only ever loose is not in it and simply stays loose —
//!     `prune-packed` removes a loose object only when a pack holds it. And an
//!     incremental run with nothing new to pack copies *every* packed object
//!     into it, since there is no new pack to subtract; only when the new pack
//!     already covers the old ones is the second pack written empty.
//!   * **`--write-midx` over an empty new pack.** `write_midx_included_packs()`
//!     hands `multi-pack-index write` a `--preferred-pack`: the first of the
//!     packs this run wrote, `names` sorted and cruft packs skipped. An empty
//!     pack cannot be that, so `write_midx_internal()` reports `error: cannot
//!     select preferred pack <objdir>/pack/pack-<hash>.pack with no objects` and
//!     `goto cleanup`s with `result` still at its `-1` initializer, which the
//!     child turns into exit 255 and `cmd_repack()` hands back unchanged. That
//!     is the pairing `--filter` reaches whenever the empty second pack sorts
//!     first — its name being a constant, it usually does. Everything after the
//!     MIDX write is skipped: the new packs stay, `-d` deletes nothing,
//!     `prune-packed` and `update-server-info` do not run, and no
//!     `multi-pack-index` or `.bitmap` is left behind.
//!   * **`-b` with `--write-midx`** writes no pack `.bitmap`:
//!     `cmd_repack()` pushes `--write-bitmap-index` at `pack-objects` only
//!     `if (write_midx == REPACK_WRITE_MIDX_NONE)`, the bitmap belonging to the
//!     MIDX in that case.
//! ```
//!
//! # Deliberate gaps, so this doc claims no more than the code does
//!
//! ```text
//!   * **`--cruft`** writes no `.mtimes`, there being no reader or writer for
//!     that format in `gix-pack`. On any repository whose objects are all
//!     reachable — every harness fixture — git writes no cruft pack either, so
//!     this is only observable where unreachable objects exist.
//!   * **`--max-pack-size`** does not split the output; one pack is always
//!     written. Its diagnostics *are* reproduced: a value below 1 MiB warns
//!     `warning: minimum pack size limit is 1 MiB`, and `pack.packSizeLimit`
//!     supplies the default (validated ahead of parse-options, so an unreadable
//!     value is fatal even for `-h`).
//!   * **`--geometric`** repacks everything rather than selecting the subset of
//!     packs that restores a geometric size progression.
//!   * **`--filter-to=<value>`** is read as a directory to put the filtered pack
//!     in; git reads it as a pack *prefix*, so `--filter-to=/tmp/x` gives git
//!     `/tmp/x-<hash>.pack` and this port `/tmp/x/pack-<hash>.pack`. What the
//!     pack contains, and the fact that it stays out of the `names` list that
//!     `--preferred-pack` is drawn from when it lands outside the object store,
//!     are the same either way.
//!   * **`--write-midx`** writes `objects/pack/multi-pack-index` over the packs
//!     the run leaves behind, through the same writer `git multi-pack-index
//!     write` uses. `--write-midx=incremental` asks for a MIDX *chain* instead
//!     and is refused. A MIDX written together with `-b` carries no
//!     `multi-pack-index-<hash>.bitmap`: that is a different format from the
//!     pack bitmap this port writes, and `gix-pack` has no writer for it. The
//!     pack `.bitmap` is not written in its place either, git putting none there
//!     under `--write-midx`, so the run is a MIDX with no bitmap at all.
//!   * **`--filter=tree:<depth>`** is accepted but not applied to the traversal;
//!     unlike the blob filters its interaction with `--indexed-objects` did not
//!     reduce to a rule the observed output confirms, and guessing one would put
//!     the wrong object set in the pack. Observable only under `--filter=tree:*`
//!     *together with* `-d`, where a loose object git prunes may survive.
//!   * **`--window`/`--window-memory`/`--depth`/`--threads`** are forwarded to
//!     the delta search, shadowing the `pack.*` keys of the same name.
//!     **`-f`/`-F`/`--path-walk`/`--delta-islands`/`--name-hash-version`** tune
//!     parts of git's search that have no counterpart here, and stay no-ops.
//!   * `repack.writeBitmaps`, or its older spelling `pack.writeBitmaps`, turns
//!     `-b` on by itself; `--no-write-bitmap-index` overrides either.
//!   * `repack.useDeltaBaseOffset` *is* read, and picks `OBJ_OFS_DELTA` over
//!     `OBJ_REF_DELTA`. `repack.packKeptObjects` is not: it tunes a kept-object
//!     exclusion this writer does not perform. `repack.cruftWindow` /
//!     `repack.cruftWindowMemory` / `repack.cruftDepth` / `repack.cruftThreads`
//!     are not read *here* either, because `--cruft` writes no cruft pack in
//!     this module — but they are not dead: git reaches a cruft pack through
//!     `gc`, and [`super::gc`] does produce one and does tune its delta search
//!     with all four. `repack.updateServerInfo` *is* honoured, since the closing
//!     `update-server-info` it gates is real; see [`execute`].
//!   * `--filter=sparse:oid=<rev>` is accepted on syntax alone — git's rejection
//!     of it depends on resolving and parsing the named blob;
//!   * `combine:` sub-specs are not percent-decoded;
//!   * with an invalid *integer* value earlier in argv than an invalid filter
//!     spec, git reports the filter (`--window=x --filter=bogus:spec` → exit
//!     128) while this reports the integer (exit 129). The mechanism behind
//!     that inversion was not identified, and the ordering is otherwise
//!     positional, so the positional behaviour is what is implemented.
//! ```

use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;

/// Stock git's `repack` usage block, byte-for-byte (2699 bytes, git 2.55.0),
/// including the trailing blank line. Printed on `-h` (stdout) and after the
/// `unknown option` / `unknown switch` diagnostics (stderr).
const USAGE: &str = r#"usage: git repack [-a] [-A] [-d] [-f] [-F] [-l] [-n] [-q] [-b] [-m]
       [--window=<n>] [--depth=<n>] [--threads=<n>] [--keep-pack=<pack-name>]
       [--write-midx[=<mode>]] [--name-hash-version=<n>] [--path-walk]

    -a                    pack everything in a single pack
    -A                    same as -a, and turn unreachable objects loose
    --[no-]cruft          same as -a, pack unreachable cruft objects separately
    --[no-]cruft-expiration <approxidate>
                          with --cruft, expire objects older than this
    --combine-cruft-below-size <n>
                          with --cruft, only repack cruft packs smaller than this
    --max-cruft-size <n>  with --cruft, limit the size of new cruft packs
    -d                    remove redundant packs, and run git-prune-packed
    -f                    pass --no-reuse-delta to git-pack-objects
    -F                    pass --no-reuse-object to git-pack-objects
    --[no-]name-hash-version <n>
                          specify the name hash version to use for grouping similar objects by path
    --[no-]path-walk      pass --path-walk to git-pack-objects
    -n                    do not run git-update-server-info
    -q, --[no-]quiet      be quiet
    -l, --[no-]local      pass --local to git-pack-objects
    -b, --[no-]write-bitmap-index
                          write bitmap index
    -i, --[no-]delta-islands
                          pass --delta-islands to git-pack-objects
    --[no-]unpack-unreachable <approxidate>
                          with -A, do not loosen objects older than this
    -k, --[no-]keep-unreachable
                          with -a, repack unreachable objects
    --[no-]window <n>     size of the window used for delta compression
    --[no-]window-memory <bytes>
                          same as the above, but limit memory size instead of entries count
    --[no-]depth <n>      limits the maximum delta depth
    --[no-]threads <n>    limits the maximum number of threads
    --max-pack-size <n>   maximum size of each packfile
    --[no-]filter <args>  object filtering
    --[no-]pack-kept-objects
                          repack objects in packs marked with .keep
    --[no-]keep-pack <name>
                          do not repack this pack
    -g, --[no-]geometric <n>
                          find a geometric progression with factor <N>
    --[no-]write-midx[=<mode>]
                          write a multi-pack index of the resulting packs
    --[no-]expire-to <dir>
                          pack prefix to store a pack containing pruned objects
    --[no-]filter-to <dir>
                          pack prefix to store a pack containing filtered out objects

"#;

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `-m`.
/// Captured byte-for-byte from stock git 2.55.0's `git repack --help-all`.
const USAGE_ALL: &str = r#"usage: git repack [-a] [-A] [-d] [-f] [-F] [-l] [-n] [-q] [-b] [-m]
       [--window=<n>] [--depth=<n>] [--threads=<n>] [--keep-pack=<pack-name>]
       [--write-midx[=<mode>]] [--name-hash-version=<n>] [--path-walk]

    -a                    pack everything in a single pack
    -A                    same as -a, and turn unreachable objects loose
    --[no-]cruft          same as -a, pack unreachable cruft objects separately
    --[no-]cruft-expiration <approxidate>
                          with --cruft, expire objects older than this
    --combine-cruft-below-size <n>
                          with --cruft, only repack cruft packs smaller than this
    --max-cruft-size <n>  with --cruft, limit the size of new cruft packs
    -d                    remove redundant packs, and run git-prune-packed
    -f                    pass --no-reuse-delta to git-pack-objects
    -F                    pass --no-reuse-object to git-pack-objects
    --[no-]name-hash-version <n>
                          specify the name hash version to use for grouping similar objects by path
    --[no-]path-walk      pass --path-walk to git-pack-objects
    -n                    do not run git-update-server-info
    -q, --[no-]quiet      be quiet
    -l, --[no-]local      pass --local to git-pack-objects
    -b, --[no-]write-bitmap-index
                          write bitmap index
    -i, --[no-]delta-islands
                          pass --delta-islands to git-pack-objects
    --[no-]unpack-unreachable <approxidate>
                          with -A, do not loosen objects older than this
    -k, --[no-]keep-unreachable
                          with -a, repack unreachable objects
    --[no-]window <n>     size of the window used for delta compression
    --[no-]window-memory <bytes>
                          same as the above, but limit memory size instead of entries count
    --[no-]depth <n>      limits the maximum delta depth
    --[no-]threads <n>    limits the maximum number of threads
    --max-pack-size <n>   maximum size of each packfile
    --[no-]filter <args>  object filtering
    --[no-]pack-kept-objects
                          repack objects in packs marked with .keep
    --[no-]keep-pack <name>
                          do not repack this pack
    -g, --[no-]geometric <n>
                          find a geometric progression with factor <N>
    --[no-]write-midx[=<mode>]
                          write a multi-pack index of the resulting packs
    -m                    write a multi-pack index of the resulting packs
    --[no-]expire-to <dir>
                          pack prefix to store a pack containing pruned objects
    --[no-]filter-to <dir>
                          pack prefix to store a pack containing filtered out objects

"#;

/// How an option consumes (and validates) its value.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `OPT_BOOL`/`OPT_BIT`: no value; `--opt=x` is an error.
    Bool,
    /// `OPT_INTEGER`: signed base-0 integer (`0x` hex, leading-`0` octal, decimal),
    /// with an optional single `k`/`m`/`g` suffix.
    Int,
    /// `OPT_MAGNITUDE`: as `Int` but non-negative.
    Magnitude,
    /// `OPT_STRING`: any value, taken from `=` or the next argv entry.
    Str,
    /// `PARSE_OPT_OPTARG`: value only ever comes from `=`.
    OptStr,
}

/// One entry of git's `repack` option table.
struct OptDef {
    long: &'static str,
    kind: Kind,
    /// Whether `--no-<long>` is accepted (`--[no-]` in the usage block).
    negatable: bool,
}

/// The long-option table **in git's declaration order**. The order is
/// load-bearing: parse-options resolves an ambiguous abbreviation by reporting
/// the last two matches it walked past, so reordering this array changes the
/// text of `ambiguous option` diagnostics.
const OPTS: &[OptDef] = &[
    OptDef { long: "cruft", kind: Kind::Bool, negatable: true },
    OptDef { long: "cruft-expiration", kind: Kind::Str, negatable: true },
    OptDef { long: "combine-cruft-below-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "max-cruft-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "name-hash-version", kind: Kind::Int, negatable: true },
    OptDef { long: "path-walk", kind: Kind::Bool, negatable: true },
    OptDef { long: "quiet", kind: Kind::Bool, negatable: true },
    OptDef { long: "local", kind: Kind::Bool, negatable: true },
    OptDef { long: "write-bitmap-index", kind: Kind::Bool, negatable: true },
    OptDef { long: "delta-islands", kind: Kind::Bool, negatable: true },
    OptDef { long: "unpack-unreachable", kind: Kind::Str, negatable: true },
    OptDef { long: "keep-unreachable", kind: Kind::Bool, negatable: true },
    OptDef { long: "window", kind: Kind::Int, negatable: true },
    OptDef { long: "window-memory", kind: Kind::Magnitude, negatable: true },
    OptDef { long: "depth", kind: Kind::Int, negatable: true },
    OptDef { long: "threads", kind: Kind::Int, negatable: true },
    OptDef { long: "max-pack-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "filter", kind: Kind::Str, negatable: true },
    OptDef { long: "pack-kept-objects", kind: Kind::Bool, negatable: true },
    OptDef { long: "keep-pack", kind: Kind::Str, negatable: true },
    OptDef { long: "geometric", kind: Kind::Int, negatable: true },
    OptDef { long: "write-midx", kind: Kind::OptStr, negatable: true },
    OptDef { long: "expire-to", kind: Kind::Str, negatable: true },
    OptDef { long: "filter-to", kind: Kind::Str, negatable: true },
];

/// The only accepted `--write-midx=<mode>` values; a bare `--write-midx` and
/// `--write-midx=` are equivalent to the empty mode.
const WRITE_MIDX_MODES: [&str; 2] = ["", "incremental"];

/// The flag state git derives while parsing, i.e. everything the pre-flight
/// conflict checks look at.
#[derive(Default)]
struct State {
    /// `ALL_INTO_ONE`, set by `-a`, `-A` and `--cruft`.
    all_into_one: bool,
    /// `LOOSEN_UNREACHABLE`, set by `-A` and by `--unpack-unreachable`.
    loosen_unreachable: bool,
    keep_unreachable: bool,
    cruft: bool,
    /// git's tri-state `write_bitmaps`: `None` is "nobody said", which
    /// `repack.writeBitmaps` / `pack.writeBitmaps` then answer, and which
    /// finally falls back to "only when everything goes into one pack in a bare
    /// repository".
    write_bitmap: Option<bool>,
    /// `-f`: `po_args.no_reuse_delta`, passed on as `--no-reuse-delta`.
    no_reuse_delta: bool,
    write_midx: bool,
    /// `--write-midx=incremental`, which asks for a MIDX *chain* rather than a
    /// single `multi-pack-index`. Tracked separately so it can be refused.
    write_midx_incremental: bool,
    geometric: bool,
    filter: bool,
    filter_to: bool,
    /// The scaled value of the last `--name-hash-version`; 0 when unset or
    /// cleared by `--no-name-hash-version`, which is the default git accepts.
    name_hash_version: i64,
    /// `-d`: drop the packs the new one supersedes, then `prune-packed`.
    delete_redundant: bool,
    /// `-n`: skip the closing `update-server-info`.
    no_server_info: bool,
    /// `-q`/`--quiet`, which suppresses the `Nothing new to pack.` notice.
    quiet: bool,
    /// The last `--filter` spec, already validated.
    filter_spec: Option<String>,
    /// The last `--filter-to` directory, which diverts the filtered-out pack.
    filter_to_dir: Option<String>,
    /// Every `--keep-pack` name; those packs survive `-d`.
    keep_packs: Vec<String>,
    /// `--max-pack-size=<n>` as the magnitude git parsed, which repack forwards
    /// to its `pack-objects` child. Zero is git's "unset".
    max_pack_size: Option<u64>,
}

/// The outcome of parsing: either a fully-formed request, or a diagnostic that
/// has already decided the exit code.
enum Parsed {
    Ok(State),
    Exit(ExitCode),
}

/// `git repack` — argument validation and pre-flight conflict checks only; the
/// repacking itself is not ported.
///
/// Returns 129 with git's own output for `-h` and for every malformed
/// invocation, and 128 for the option conflicts git rejects before doing any
/// work. Any invocation that survives both bails, naming the substrate that is
/// missing; see the module documentation for the full list.
pub fn repack(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `repack` has no positional of its
    // own (stray positionals are silently ignored by git), so dropping a leading
    // copy of the verb cannot change the result.
    let args = match args.first().map(String::as_str) {
        Some("repack") => &args[1..],
        _ => args,
    };

    // git reads the pack config before parse-options here too, so an unreadable
    // `pack.packSizeLimit` is fatal ahead of every parse diagnostic — verified
    // against git 2.55.0, where `repack -h` under a bad value reports the number
    // rather than printing usage.
    let pack_size_limit_cfg = match gix::discover(".") {
        Ok(repo) => match crate::config::config_ulong(&repo, "pack.packSizeLimit") {
            Ok(limit) => limit,
            Err(message) => {
                eprintln!("fatal: {message}");
                return Ok(ExitCode::from(128));
            }
        },
        Err(_) => None,
    };

    let state = match parse(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok(state) => state,
    };

    if let Some(code) = preflight(&state) {
        return Ok(code);
    }

    // git's repack does not apply the limit itself: it forwards
    // `--max-pack-size` (or, absent one, `pack.packSizeLimit`) to the
    // `pack-objects` child, and the warning below is the child's. It therefore
    // precedes everything repack prints, including `Nothing new to pack.` — which
    // is where it lands here too, since this port packs inline instead of
    // spawning. Like `pack-objects`, this port writes one pack whatever the limit
    // says, so the warning is its only observable effect.
    let pack_size_limit = state.max_pack_size.filter(|n| *n > 0).or(pack_size_limit_cfg);
    if pack_size_limit.is_some_and(|n| n > 0 && n < MIN_PACK_SIZE_LIMIT) {
        eprintln!("warning: minimum pack size limit is 1 MiB");
    }

    execute(&state)
}

/// git's 1 MiB floor for `pack_size_limit`: any smaller non-zero limit warns and
/// is then raised to this.
const MIN_PACK_SIZE_LIMIT: u64 = 1024 * 1024;

/// Do the repacking, for a repository discovered from the current directory.
///
/// git reaches the object database only after every check above, so this is also
/// where "not a git repository" is diagnosed.
fn execute(st: &State) -> Result<ExitCode> {
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };
    let objdir = repo.objects.store_ref().path().to_path_buf();
    let pack_dir = objdir.join("pack");

    // git refreshes `objects/info/packs` at the end of a successful run unless
    // `-n` was given or `repack.updateServerInfo` is false (default true). git
    // keeps a single `run_update_server_info`, seeded from the config and cleared
    // by `-n` (an `OPT_NEGBIT`), so `-n` always wins over a config that enables
    // it and there is no way to turn it back on from the command line.
    let run_server_info = !st.no_server_info
        && repo
            .config_snapshot()
            .boolean("repack.updateServerInfo")
            .unwrap_or(true);

    // git's `--all --reflog --indexed-objects`, which `prune` already builds.
    let mut roots = Vec::new();
    super::prune::collect_roots(&repo, &mut roots)?;
    let reachable = super::prune::close_over(&repo, roots);

    let existing = super::prune::pack_indices(&repo, &objdir);
    let candidates: Vec<ObjectId> = reachable
        .into_iter()
        // Without `-a`/`-A`/`--cruft`, a repack is incremental: anything an
        // existing pack already holds is left where it is.
        .filter(|id| st.all_into_one || !existing.iter().any(|f| f.lookup(*id).is_some()))
        .collect();
    // `cmd_repack()` passes `--indexed-objects` alongside `--filter`, and
    // pack-objects unions those back in afterwards: an object the index names
    // stays in the main pack whatever the spec says. `do_add_index_objects_to_pending()`
    // skips gitlinks, so this does too.
    let indexed: HashSet<ObjectId> = repo
        .index_or_empty()
        .map(|index| {
            index
                .entries()
                .iter()
                .filter(|entry| entry.mode != gix::index::entry::Mode::COMMIT)
                .map(|entry| entry.id)
                .collect()
        })
        .unwrap_or_default();

    // `--filter` *splits* the object set, it does not shrink it: what the spec
    // rejects goes into a second pack of its own. Dropping those objects instead
    // would destroy them, because `-d` is about to delete the packs they live in
    // now — `git repack -a -d --filter=blob:none` must leave every blob readable.
    let mut to_pack: Vec<ObjectId> = candidates
        .into_iter()
        .filter(|id| indexed.contains(id) || keeps_object(st, id, &repo))
        .collect();
    // The pack's entry order is ours to choose; sorting makes a run reproducible.
    to_pack.sort();

    // `if (!names.nr)`: git says so and carries on, and it says so about the
    // *first* `pack-objects` alone — the notice sits between that child and the
    // cruft and filtered packs, so a run that goes on to write a filtered pack
    // still prints it. Everything after the pack write still runs too — in
    // particular `-d`'s `prune_packed_objects()`, which is what drops the loose
    // copies of objects an *existing* pack already holds, and `--write-midx`.
    // Returning here instead left those loose objects behind.
    if to_pack.is_empty() && !st.all_into_one && !st.quiet {
        println!("Nothing new to pack.");
    }

    // `write_filtered_pack()` (`repack-filtered.c`) drives the second pack with
    // `pack-objects --stdin-packs`, fed the existing non-kept and cruft packs
    // with the just-written pack excluded by `^`. So it holds what those packs
    // held and the new pack does not — never the run's own traversal, which is
    // why an incremental run with nothing new to pack still copies every packed
    // object into it rather than writing an empty pack. `--stdin-packs`
    // enumerates objects *out of packs*, so a filtered-out object that was only
    // ever loose is not in it: `prune-packed` leaves a loose object no pack holds
    // alone, so it simply stays loose. Keeping it here instead would move it into
    // a pack git never writes.
    let in_new_pack: HashSet<ObjectId> = to_pack.iter().copied().collect();
    let mut filtered_out: Vec<ObjectId> = existing
        .iter()
        .filter(|f| droppable(st, f.path()))
        .flat_map(|f| f.iter().map(|e| e.oid))
        .filter(|id| !in_new_pack.contains(id))
        .collect();
    filtered_out.sort();
    filtered_out.dedup();

    // Which packs `-d` may drop: everything that existed before this run, minus
    // any protected by a `.keep` or named by `--keep-pack`. Captured before the
    // new pack lands so it is never a candidate for its own removal.
    let superseded: Vec<PathBuf> = if st.delete_redundant && st.all_into_one {
        existing.iter().map(|f| f.path().to_path_buf()).filter(|p| droppable(st, p)).collect()
    } else {
        Vec::new()
    };
    drop(existing);

    fs::create_dir_all(&pack_dir)?;
    let write_rev = repo
        .config_snapshot()
        .boolean("pack.writeReverseIndex")
        .unwrap_or(true);
    // git's repack reports the pack write through its `pack-objects` child, so
    // the meters appear here on the same terms: a terminal, and no `-q`.
    let progress = crate::progress::enabled(st.quiet);
    {
        let mut enumerating = crate::progress::Meter::unknown("Enumerating objects", progress);
        enumerating.advance(to_pack.len());
        enumerating.done();
    }
    // git's `names`: every pack this run wrote *into the repository's own*
    // `objects/pack`, each with the number of objects it holds. It is the set
    // `write_midx_included_packs()` picks the preferred pack out of, so it is
    // collected as the packs are written.
    let mut new_packs: Vec<(String, usize)> = Vec::new();
    // Everything filtered out is about to be written elsewhere, so a run whose
    // spec rejects the whole set still has a second pack to produce.
    let written = if to_pack.is_empty() {
        PathBuf::new()
    } else {
        let path = write_pack(
            &repo,
            &to_pack,
            &pack_dir,
            write_rev,
            progress,
            // `cmd_repack()` asks `pack-objects` for a `.bitmap` only when no
            // MIDX is being written (`if (write_midx == REPACK_WRITE_MIDX_NONE)`
            // around both `--write-bitmap-index` pushes): with `--write-midx` the
            // bitmap belongs to the MIDX instead, and git leaves the packs bare.
            write_bitmaps(st, &repo) && !st.write_midx,
            st.no_reuse_delta,
        )?;
        new_packs.push((pack_base_name(&path), to_pack.len()));
        path
    };

    // With `--filter` git writes a second pack holding the filtered-out objects,
    // in `--filter-to=<dir>` when given and beside the first one otherwise. The
    // objects have to travel with it: they are only reachable through this pack
    // once `-d` removes the ones they came from.
    if st.filter {
        let dir = match &st.filter_to_dir {
            Some(d) => PathBuf::from(d),
            None => pack_dir.clone(),
        };
        fs::create_dir_all(&dir)?;
        let base = if filtered_out.is_empty() {
            // git still writes the pack, its index and its reverse index when the
            // spec rejected nothing; their presence is what marks a filtered run.
            write_empty_pack(repo.object_hash(), &dir, write_rev)?
        } else {
            // No bitmap: a bitmap describes a reachability closure, and this pack
            // is deliberately a fragment of one.
            let path =
                write_pack(&repo, &filtered_out, &dir, write_rev, progress, false, st.no_reuse_delta)?;
            pack_base_name(&path)
        };
        // `finish_pack_objects_cmd()` keeps a pack out of `names` when it was
        // written outside the object store ("avoid putting packs written outside
        // of the repository in the list of names"), which is what `--filter-to`
        // pointing elsewhere does.
        if is_local_pack_dir(&dir, &pack_dir) {
            new_packs.push((base, filtered_out.len()));
        }
    }

    // `write_midx_included_packs()`. With no `--geometric` geometry to name a
    // preferred pack, git points `multi-pack-index write` at the first pack it
    // just wrote — `names` sorted, cruft packs skipped, which this writer never
    // produces. `write_midx_internal()` then refuses a preferred pack holding no
    // objects, and `--filter` produces exactly that when the spec rejected
    // nothing already packed: an empty second pack, whose constant name sorts
    // first more often than not. The refusal is an `error()` followed by
    // `goto cleanup`, leaving `result` at its `-1` initializer, so the child
    // exits 255 and `cmd_repack()` returns that untouched.
    //
    // Everything after the MIDX write is skipped by that `goto cleanup`: the new
    // packs stay installed, `-d` deletes nothing, `prune-packed` does not run and
    // neither does `update-server-info`.
    if st.write_midx && !st.write_midx_incremental {
        new_packs.sort();
        if let Some((name, 0)) = new_packs.first() {
            // git names the pack the way it opened it, i.e. under the object
            // directory as `get_object_directory()` renders it.
            let shown = super::prune_packed::display_objdir(&repo, &objdir);
            eprintln!(
                "error: cannot select preferred pack {} with no objects",
                shown.join("pack").join(format!("{name}.pack")).display()
            );
            return Ok(ExitCode::from(255));
        }
    }

    if st.delete_redundant {
        for index_path in superseded {
            // An identical object set hashes to the same name, in which case the
            // "superseded" pack *is* the one just written.
            if index_path == written {
                continue;
            }
            for ext in ["pack", "idx", "rev", "bitmap", "mtimes", "promisor"] {
                let _ = fs::remove_file(index_path.with_extension(ext));
            }
        }
        // A multi-pack-index naming a pack this just deleted would keep sending
        // lookups to a missing file, so git drops it along with the packs.
        super::multi_pack_index::drop_stale_midx(&pack_dir);
        // git finishes `-d` by running `git prune-packed`, which is a real port.
        let _ = super::prune_packed::prune_packed(&["prune-packed".to_string(), "-q".to_string()])?;
    }

    // `repack_write_midx()`. git writes the index before it deletes the packs
    // `-d` supersedes, working from the set it has already marked for removal;
    // writing it here instead, from the packs actually left in the directory,
    // reaches the same set without having to model the marks.
    if st.write_midx_incremental {
        bail!(
            "unsupported flag \"--write-midx=incremental\" \
             (the MIDX chain protocol is not modelled by the vendored gix-pack multi-index writer)"
        );
    }
    if st.write_midx {
        super::multi_pack_index::write_midx(&pack_dir, repo.object_hash())?;
    }

    if run_server_info {
        let _ = super::update_server_info::update_server_info(&["update-server-info".to_string()])?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Encode `ids` as a pack, install it in `pack_dir` as `pack-<checksum>.pack`,
/// and write its `.idx` (and optionally its `.rev`) beside it.
///
/// The index is built here rather than by re-reading the pack, because the
/// writer already knows where every object landed and what its CRC is — and
/// because `gix-pack`'s bundle writer refuses a pack containing `OBJ_REF_DELTA`
/// entries, which is precisely what `repack.useDeltaBaseOffset=false` asks for.
/// The naming rule is unchanged: a pack is named after its own trailing
/// checksum, which is what git does and what `gc` does here.
/// Whether this run writes a `.bitmap`, resolved as `cmd_repack()` resolves
/// `write_bitmaps`.
///
/// `-b` / `--no-write-bitmap-index` decides outright. Failing that
/// `repack.writeBitmaps`, or its older spelling `pack.writeBitmaps`, does. With
/// nobody having said anything git falls back to writing one only when the whole
/// repository goes into a single pack *and* the repository is bare — the case
/// where the pack is guaranteed to hold the closure a bitmap needs.
fn write_bitmaps(st: &State, repo: &gix::Repository) -> bool {
    if let Some(explicit) = st.write_bitmap {
        return explicit;
    }
    let snapshot = repo.config_snapshot();
    if let Some(configured) = snapshot
        .boolean("repack.writeBitmaps")
        .or_else(|| snapshot.boolean("pack.writeBitmaps"))
    {
        return configured;
    }
    st.all_into_one && repo.is_bare()
}

fn write_pack(
    repo: &gix::Repository,
    ids: &[ObjectId],
    pack_dir: &Path,
    write_rev: bool,
    progress: bool,
    write_bitmap: bool,
    no_reuse_delta: bool,
) -> Result<PathBuf> {
    let allow_ofs_delta = repo
        .config_snapshot()
        .boolean("repack.useDeltaBaseOffset")
        .unwrap_or(true);
    // git's `repack.useDeltaIslands`, which `cmd_repack()` turns into
    // `pack-objects --delta-islands`.
    let use_delta_islands = repo
        .config_snapshot()
        .boolean("repack.useDeltaIslands")
        .unwrap_or(false);
    let packed = super::pack_objects::packed_for(
        repo,
        ids,
        super::pack_objects::WriteOptions {
            allow_ofs_delta,
            progress,
            use_delta_islands,
            no_reuse_delta,
            ..super::pack_objects::WriteOptions::default()
        },
    )?;
    if packed.entries.is_empty() {
        crate::git_fatal!("pack writer produced no files for {} objects", ids.len());
    }

    let kind = repo.object_hash();
    let base = pack_dir.join(format!("pack-{}", packed.id));
    install(&base.with_extension("pack"), &packed.bytes)?;

    // Both companions index into the pack in object-id order.
    let mut by_oid = packed.entries.clone();
    by_oid.sort_unstable_by_key(|entry| entry.id);
    let index_path = base.with_extension("idx");
    install(
        &index_path,
        &super::pack_objects::index_file(kind, 2, &packed.id, &by_oid)?,
    )?;
    if write_rev {
        install(
            &base.with_extension("rev"),
            &super::pack_objects::reverse_index_file(kind, &packed.id, &by_oid)?,
        )?;
    }
    if write_bitmap {
        let mut options = super::pack_objects::BitmapOptions::from_repo(repo);
        options.write = true;
        if let Some(bytes) = super::pack_objects::bitmap_file(repo, &packed, &options) {
            fs::write(base.with_extension("bitmap"), bytes)?;
        }
    }
    Ok(index_path)
}

/// Put one pack artifact in place, `0444` and by rename, as git installs them.
///
/// The mode is why the rename matters: a pack whose object set has not changed
/// hashes to the name it already has on disk, so writing straight to that path
/// would land on the read-only file the last run left and fail with `EACCES`.
fn install(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = path.parent().unwrap_or(Path::new("."));
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("tmp");
    let tmp = dir.join(format!("tmp_{ext}_zvcs_repack_{}", std::process::id()));
    fs::write(&tmp, bytes)
        .with_context(|| format!("unable to write {}", tmp.display()))?;
    // git does not check its own chmod either, so a filesystem that refuses the
    // mode is not a failure.
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o444));
    fs::rename(&tmp, path).with_context(|| format!("unable to rename to {}", path.display()))
}

/// The `pack-<hash>` stem of a pack artifact, which is the name git carries in
/// its `names` list and interpolates into `--preferred-pack=pack-%s.pack`.
fn pack_base_name(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string()
}

/// `write_pack_opts_is_local()`: whether a pack written into `dir` lands in the
/// repository's own object store, and so counts as one of the packs this run
/// wrote. git compares the two path strings with `starts_with()`; the same
/// question is asked here of the resolved paths, `--filter-to` being free to
/// name the pack directory by any route.
fn is_local_pack_dir(dir: &Path, pack_dir: &Path) -> bool {
    let resolve = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolve(dir).starts_with(resolve(pack_dir))
}

/// Write the empty pack, its index and its reverse index into `dir`, returning
/// its `pack-<hash>` stem.
///
/// An empty pack has no objects to name it after, so its checksum — and
/// therefore its filename — is a constant for a given hash function.
fn write_empty_pack(kind: gix::hash::Kind, dir: &Path, write_rev: bool) -> Result<String> {
    // The 12-byte v2 header with a zero object count, and the checksum over it.
    let mut pack = Vec::new();
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&0u32.to_be_bytes());
    append_checksum(&mut pack, kind)?;
    let pack_id = pack[pack.len() - kind.len_in_bytes()..].to_vec();

    // A v2 index over no objects: the signature, an all-zero 256-entry fanout,
    // no entries, and the pack's checksum.
    let mut idx = Vec::new();
    idx.extend_from_slice(&[0xff, b't', b'O', b'c']);
    idx.extend_from_slice(&2u32.to_be_bytes());
    idx.extend_from_slice(&[0u8; 256 * 4]);
    idx.extend_from_slice(&pack_id);
    append_checksum(&mut idx, kind)?;

    let base = format!("pack-{}", ObjectId::from_bytes_or_panic(&pack_id));
    fs::write(dir.join(format!("{base}.pack")), &pack)?;
    fs::write(dir.join(format!("{base}.idx")), &idx)?;

    if write_rev {
        // The same layout `gix-pack`'s writer produces, with no permutation.
        let mut rev = Vec::new();
        rev.extend_from_slice(b"RIDX");
        rev.extend_from_slice(&1u32.to_be_bytes());
        rev.extend_from_slice(&(if kind == gix::hash::Kind::Sha1 { 1u32 } else { 2 }).to_be_bytes());
        rev.extend_from_slice(&pack_id);
        append_checksum(&mut rev, kind)?;
        fs::write(dir.join(format!("{base}.rev")), &rev)?;
    }
    Ok(base)
}

/// Append the hash of everything written so far, which is how every one of
/// git's pack artifacts terminates.
fn append_checksum(bytes: &mut Vec<u8>, kind: gix::hash::Kind) -> Result<()> {
    let mut hasher = gix::hash::hasher(kind);
    hasher.update(&bytes[..]);
    bytes.extend_from_slice(hasher.try_finalize()?.as_slice());
    Ok(())
}

/// Whether `id` survives the `--filter` spec.
///
/// `blob:none` drops every blob and `blob:limit=<n>` every blob over `n` bytes.
/// Both are applied to the traversal only; the index objects the caller already
/// folded in are what git unions back afterwards, and since this filter runs
/// over the closed set the two coincide for every blob the index names.
/// `tree:<depth>` is accepted but not applied — see the module docs.
fn keeps_object(st: &State, id: &ObjectId, repo: &gix::Repository) -> bool {
    let Some(spec) = st.filter_spec.as_deref() else {
        return true;
    };
    let limit = if spec == "blob:none" {
        Some(0)
    } else {
        spec.strip_prefix("blob:limit=").and_then(scaled).map(|n| n as u64)
    };
    let Some(limit) = limit else {
        return true;
    };
    match repo.find_object(*id) {
        Ok(obj) if obj.kind == gix::objs::Kind::Blob => obj.data.len() as u64 <= limit,
        _ => true,
    }
}

/// Whether `-d` may remove the pack whose index is at `index_path`: a `.keep`
/// beside it, or a `--keep-pack` naming it, pins it in place.
fn droppable(st: &State, index_path: &Path) -> bool {
    if index_path.with_extension("keep").exists() {
        return false;
    }
    let pack_name = index_path
        .with_extension("pack")
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    !st.keep_packs.contains(&pack_name)
}

/// Walk `args` exactly the way git's parse-options walks them, emitting git's
/// diagnostics verbatim on the first malformed entry.
fn parse(args: &[String]) -> Parsed {
    let mut st = State::default();
    let mut end_of_opts = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        if end_of_opts || !a.starts_with('-') || a == "-" {
            // Positionals are accepted and ignored, as git does.
            i += 1;
            continue;
        }

        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match, tested after the `--` break
        // above and before any table lookup, so it never abbreviates and never
        // takes an `=<value>`. It renders `USAGE_FULL`, which for `repack` is
        // `USAGE` plus the hidden `-m`.
        if a == "--help-all" {
            return Parsed::Exit(super::show_usage(USAGE_ALL));
        }

        if let Some(body) = a.strip_prefix("--") {
            match long_opt(body, args, &mut i, &mut st) {
                Some(code) => return Parsed::Exit(code),
                None => continue,
            }
        }

        // Clustered short switches, e.g. `-adq` or `-g2`.
        match short_opts(&a[1..], args, &mut i, &mut st) {
            Some(code) => return Parsed::Exit(code),
            None => continue,
        }
    }

    Parsed::Ok(st)
}

/// Handle one `--...` entry. Advances `i` past everything it consumed, or
/// returns the exit code of a diagnostic.
fn long_opt(body: &str, args: &[String], i: &mut usize, st: &mut State) -> Option<ExitCode> {
    let (name, inline) = match body.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (body, None),
    };

    let (idx, negated) = match resolve_long(name) {
        Resolved::Unique(idx, negated) => (idx, negated),
        Resolved::Ambiguous(first, second) => {
            // Verified quirk: unlike every other diagnostic here, the ambiguity
            // message goes to stderr while its usage block goes to *stdout*.
            eprintln!("error: ambiguous option: {name} (could be --{first} or --{second})");
            print!("{USAGE}");
            return Some(ExitCode::from(129));
        }
        Resolved::Unknown => {
            // git echoes the argument as written, `=value` included.
            eprint!("error: unknown option `{body}'\n{USAGE}");
            return Some(ExitCode::from(129));
        }
    };

    let def = &OPTS[idx];
    // The diagnostics name the matched form, not the abbreviation the user typed.
    let shown = if negated {
        format!("no-{}", def.long)
    } else {
        def.long.to_string()
    };

    // A negation never takes a value, and neither does a boolean.
    if (negated || def.kind == Kind::Bool) && inline.is_some() {
        eprintln!("error: option `{shown}' takes no value");
        return Some(ExitCode::from(129));
    }

    if negated {
        set_long(idx, true, None, st);
        *i += 1;
        return None;
    }

    let value = match def.kind {
        Kind::Bool => None,
        // `PARSE_OPT_OPTARG` only ever reads a value glued on with `=`.
        Kind::OptStr => Some(inline.unwrap_or("")),
        _ => match inline {
            Some(v) => Some(v),
            None => match args.get(*i + 1) {
                Some(v) => {
                    *i += 1;
                    Some(v.as_str())
                }
                None => {
                    eprintln!("error: option `{shown}' requires a value");
                    return Some(ExitCode::from(129));
                }
            },
        },
    };

    if let Some(v) = value {
        if let Some(code) = check_value(def, &shown, v) {
            return Some(code);
        }
        // `--filter` is an `OPT_PARSE_LIST_OBJECTS_FILTER` callback, so a bad
        // spec dies where it sits in argv rather than at the end of parsing:
        // `--filter=bogus:spec --zzz` reports the filter, `--zzz
        // --filter=bogus:spec` reports the unknown option (git 2.55.0).
        if def.long == "filter" {
            if let Some(msg) = filter_error(v) {
                return Some(fatal(&msg));
            }
        }
    }

    set_long(idx, false, value, st);
    *i += 1;
    None
}

/// Mirror C `strtol`/`strtoumax` with **base 0**, which is how git parses every
/// numeric option value (`OPT_INTEGER`, `OPT_MAGNITUDE`) and the numbers inside a
/// `--filter` spec: skip leading ASCII whitespace, an optional `+`/`-` sign, then
/// a base-0 integer — `0x`/`0X` hexadecimal, a leading `0` octal, otherwise
/// decimal. Returns `(negative, magnitude, unparsed-remainder)`, or `None` when no
/// digit is consumed at all (git's `end == value`, an `EINVAL`). The magnitude is
/// accumulated in `u128` and saturates, so a literal too large for any integer
/// type still reaches the caller's range check instead of wrapping.
fn c_strtol(s: &str) -> Option<(bool, u128, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let neg = match b.get(i) {
        Some(b'+') => {
            i += 1;
            false
        }
        Some(b'-') => {
            i += 1;
            true
        }
        _ => false,
    };

    // base-0 prefix detection: `0x`/`0X` before a hex digit is hexadecimal, a bare
    // leading `0` is octal, everything else decimal. A `0x` with no hex digit
    // after it parses as just the `0` (`strtol` stops at the `x`).
    let (base, start): (u32, usize) = if b.get(i) == Some(&b'0')
        && matches!(b.get(i + 1), Some(b'x' | b'X'))
    {
        match b.get(i + 2) {
            Some(c) if (*c as char).is_ascii_hexdigit() => (16, i + 2),
            _ => return Some((neg, 0, &s[i + 1..])),
        }
    } else if b.get(i) == Some(&b'0') {
        (8, i)
    } else {
        (10, i)
    };

    let mut val: u128 = 0;
    let mut j = start;
    while j < b.len() {
        match (b[j] as char).to_digit(base) {
            Some(d) => {
                val = val.saturating_mul(base as u128).saturating_add(d as u128);
                j += 1;
            }
            None => break,
        }
    }
    if j == start {
        return None;
    }
    Some((neg, val, &s[j..]))
}

/// git's `get_unit_factor`: the text after the digits must be empty or exactly one
/// of `k`/`m`/`g` (either case). A multi-character remainder (`10kg`, `10x`) is
/// not a valid suffix and makes the whole value invalid.
fn unit_factor(rest: &str) -> Option<u128> {
    if rest.is_empty() {
        Some(1)
    } else if rest.eq_ignore_ascii_case("k") {
        Some(1024)
    } else if rest.eq_ignore_ascii_case("m") {
        Some(1024 * 1024)
    } else if rest.eq_ignore_ascii_case("g") {
        Some(1024 * 1024 * 1024)
    } else {
        None
    }
}

/// The scaled signed value of a git number — a base-0 integer times its optional
/// `k`/`m`/`g` factor — or `None` when the text is not a valid number. Used where
/// only the value matters and the caller applies its own sign/range rule
/// (`blob:limit=<n>`, `tree:<depth>`, `--name-hash-version`).
fn scaled(v: &str) -> Option<i128> {
    let (neg, val, rest) = c_strtol(v)?;
    let factor = unit_factor(rest)?;
    let n = val.saturating_mul(factor).min(i128::MAX as u128) as i128;
    Some(if neg { -n } else { n })
}

/// Validate a `--filter` spec the way `gently_parse_list_objects_filter` does,
/// returning the text git puts after `fatal: ` for the first rejection.
///
/// `sparse:oid=<rev>` is accepted on syntax alone: git's own rejection of it
/// depends on resolving the object, and this command bails before reaching the
/// object database either way.
///
/// Not covered: the percent-decoding git applies to each `combine:` sub-spec.
fn filter_error(spec: &str) -> Option<String> {
    let invalid = || Some(format!("invalid filter-spec '{spec}'"));

    if let Some(rest) = spec.strip_prefix("combine:") {
        if rest.is_empty() {
            return Some("expected something after combine:".to_string());
        }
        // Empty sub-specs are skipped, so `combine:+` is accepted.
        return rest.split('+').filter(|s| !s.is_empty()).find_map(filter_error);
    }

    match spec {
        "blob:none" => None,
        _ if spec.starts_with("blob:limit=") => {
            // `git_parse_ulong`: digits with an optional k/m/g, never signed.
            match scaled(&spec["blob:limit=".len()..]) {
                Some(n) if n >= 0 => None,
                _ => invalid(),
            }
        }
        _ if spec.starts_with("tree:") => match scaled(&spec["tree:".len()..]) {
            Some(n) if n >= 0 => None,
            _ => Some("expected 'tree:<depth>'".to_string()),
        },
        _ if spec.starts_with("sparse:path=") => {
            Some("sparse:path filters support has been dropped".to_string())
        }
        _ if spec.starts_with("sparse:oid=") => None,
        _ if spec.starts_with("object:type=") => {
            let ty = &spec["object:type=".len()..];
            if matches!(ty, "blob" | "tree" | "commit" | "tag") {
                None
            } else {
                Some(format!("'{ty}' for 'object:type=<type>' is not a valid object type"))
            }
        }
        _ => invalid(),
    }
}

/// Validate a value against the option's parse-options type, emitting git's
/// exact type diagnostic on failure.
fn check_value(def: &OptDef, shown: &str, v: &str) -> Option<ExitCode> {
    // parse-options names a long option as ``option `x'`` and a short one as
    // ``switch `x'``; only `-g` reaches the value checks by its short form.
    let label = format!("option `{shown}'");
    match def.kind {
        Kind::Int => int_value(&label, v).err(),
        Kind::Magnitude => magnitude_value(&label, v),
        Kind::OptStr if def.long == "write-midx" && !WRITE_MIDX_MODES.contains(&v) => {
            eprintln!("error: unknown value for write-midx: {v}");
            Some(ExitCode::from(129))
        }
        _ => None,
    }
}

/// git's diagnostic for an empty value, which every numeric option shares.
fn numerical_value(label: &str) -> ExitCode {
    eprintln!("error: {label} expects a numerical value");
    ExitCode::from(129)
}

/// Parse an `OPT_INTEGER` value for the already-formatted `label` (e.g.
/// ``option `geometric'`` or ``switch `g'``), emitting git's type diagnostic for
/// non-numbers and its range diagnostic for anything a C `int` cannot hold
/// (`--name-hash-version=3g` scales to 3 GiB and hits the latter). The accepted
/// range is `[-2147483648, 2147483647]`: git's `git_parse_signed` allows the
/// magnitude to reach `INT_MAX + 1` when the value is negative, so `INT_MIN` is in
/// range. Every one of these prints a single line and exits 129, with no usage
/// block.
fn int_value(label: &str, v: &str) -> Result<i64, ExitCode> {
    if v.is_empty() {
        return Err(numerical_value(label));
    }
    // A non-number (`end == value`) and an unrecognised suffix (`10x`, `10kg`) are
    // both `EINVAL` in git and share this one diagnostic.
    let (neg, val, factor) = match c_strtol(v).and_then(|(n, val, rest)| unit_factor(rest).map(|f| (n, val, f))) {
        Some(parsed) => parsed,
        None => {
            eprintln!("error: {label} expects an integer value with an optional k/m/g suffix");
            return Err(ExitCode::from(129));
        }
    };
    let product = val.saturating_mul(factor);
    let max = i32::MAX as u128 + if neg { 1 } else { 0 };
    if product > max {
        eprintln!(
            "error: value {v} for {label} not in range [{},{}]",
            i32::MIN,
            i32::MAX
        );
        return Err(ExitCode::from(129));
    }
    let n = product as i64;
    Ok(if neg { -n } else { n })
}

/// Parse an `OPT_MAGNITUDE` value: as [`int_value`] but non-negative, with git's
/// `unsigned long` ceiling (`u64::MAX`, printed by git as `-1`). git's
/// `git_parse_unsigned` rejects a literal leading `-` outright, before parsing,
/// with the type diagnostic rather than a range one.
fn magnitude_value(label: &str, v: &str) -> Option<ExitCode> {
    if v.is_empty() {
        return Some(numerical_value(label));
    }
    let type_err = || {
        eprintln!(
            "error: {label} expects a non-negative integer value with an optional k/m/g suffix"
        );
        Some(ExitCode::from(129))
    };
    if v.starts_with('-') {
        return type_err();
    }
    let (neg, val, rest) = match c_strtol(v) {
        Some(parsed) => parsed,
        None => return type_err(),
    };
    // A sign can only remain after leading whitespace here; git would let
    // `strtoumax` wrap it, but that path is unreachable in practice, so reject it.
    if neg {
        return type_err();
    }
    let factor = match unit_factor(rest) {
        Some(f) => f,
        None => return type_err(),
    };
    match val.checked_mul(factor) {
        Some(p) if p <= u64::MAX as u128 => None,
        _ => {
            eprintln!("error: value {v} for {label} not in range [0,-1]");
            Some(ExitCode::from(129))
        }
    }
}

/// Record the effect of long option `OPTS[idx]`; `negated` is true for the
/// `--no-<long>` form, which clears the flag instead of setting it. `value` is
/// the option's argument, already validated, for the one option whose value the
/// pre-flight checks read.
///
/// Only the flags the pre-flight checks consult are tracked; the rest are
/// accepted and dropped, since the command bails before they could matter.
///
/// `--no-cruft` clears `cruft` but leaves `all_into_one` alone: git's `-a`/`-A`
/// and `--cruft` all set the same `ALL_INTO_ONE` bit, and the `--no-` form of a
/// bit option only clears its own bit once it has been set by that option.
fn set_long(idx: usize, negated: bool, value: Option<&str>, st: &mut State) {
    let on = !negated;
    match OPTS[idx].long {
        // `--no-name-hash-version` restores the default, which git accepts.
        "name-hash-version" => {
            st.name_hash_version = match value {
                Some(v) if on => scaled(v).unwrap_or(0) as i64,
                _ => 0,
            }
        }
        "cruft" => {
            st.cruft = on;
            st.all_into_one |= on;
        }
        // Forwarded verbatim to `pack-objects`, where it shadows
        // `pack.packSizeLimit` and drives the 1 MiB floor warning.
        "max-pack-size" => {
            st.max_pack_size = value
                .filter(|_| on)
                .and_then(scaled)
                .and_then(|n| u64::try_from(n).ok())
        }
        "quiet" => st.quiet = on,
        "keep-unreachable" => st.keep_unreachable = on,
        "write-bitmap-index" => st.write_bitmap = Some(on),
        "unpack-unreachable" => st.loosen_unreachable = on,
        "write-midx" => {
            st.write_midx = on;
            st.write_midx_incremental = on && value == Some("incremental");
        }
        "geometric" => st.geometric = on,
        "filter" => {
            st.filter = on;
            st.filter_spec = if on { value.map(str::to_string) } else { None };
        }
        "filter-to" => {
            st.filter_to = on;
            st.filter_to_dir = if on { value.map(str::to_string) } else { None };
        }
        // A repeated `--keep-pack` accumulates; `--no-keep-pack` clears the list,
        // matching git's `string_list_clear()` on the negated form.
        "keep-pack" => match (on, value) {
            (true, Some(v)) => st.keep_packs.push(v.to_string()),
            (false, _) => st.keep_packs.clear(),
            _ => {}
        },
        _ => {}
    }
}

/// The result of matching a long-option name against the table.
enum Resolved {
    /// `(table index, is a `--no-` negation)`.
    Unique(usize, bool),
    /// The last two candidates walked past, in table order — the pair git names.
    Ambiguous(String, String),
    Unknown,
}

/// Resolve `name` (the text between `--` and any `=`) the way parse-options
/// does: an exact match wins outright, otherwise every prefix match is
/// collected and two or more of them is an ambiguity.
fn resolve_long(name: &str) -> Resolved {
    for (idx, o) in OPTS.iter().enumerate() {
        if o.long == name {
            return Resolved::Unique(idx, false);
        }
        if o.negatable && name.strip_prefix("no-") == Some(o.long) {
            return Resolved::Unique(idx, true);
        }
    }

    // git keeps only the last two matches it walked past and names those.
    let mut last: Option<(usize, bool)> = None;
    let mut prev: Option<(usize, bool)> = None;
    for (idx, o) in OPTS.iter().enumerate() {
        if o.long.starts_with(name) {
            prev = last;
            last = Some((idx, false));
        }
        if o.negatable && format!("no-{}", o.long).starts_with(name) {
            prev = last;
            last = Some((idx, true));
        }
    }

    let display = |(idx, neg): (usize, bool)| {
        if neg {
            format!("no-{}", OPTS[idx].long)
        } else {
            OPTS[idx].long.to_string()
        }
    };
    match (prev, last) {
        (Some(p), Some(l)) => Resolved::Ambiguous(display(p), display(l)),
        (None, Some(l)) => Resolved::Unique(l.0, l.1),
        _ => Resolved::Unknown,
    }
}

/// Handle one clustered short-switch entry (`cluster` excludes the leading `-`).
fn short_opts(cluster: &str, args: &[String], i: &mut usize, st: &mut State) -> Option<ExitCode> {
    let chars: Vec<char> = cluster.chars().collect();
    let mut c = 0;
    while c < chars.len() {
        match chars[c] {
            'h' => {
                print!("{USAGE}");
                return Some(ExitCode::from(129));
            }
            'a' => st.all_into_one = true,
            'A' => {
                st.all_into_one = true;
                st.loosen_unreachable = true;
            }
            'k' => st.keep_unreachable = true,
            'b' => st.write_bitmap = Some(true),
            // `-m` is git's undocumented short form of `--write-midx`.
            'm' => st.write_midx = true,
            'd' => st.delete_redundant = true,
            'n' => st.no_server_info = true,
            'q' => st.quiet = true,
            // `po_args.no_reuse_delta`, which `prepare_pack_objects()` turns
            // into `pack-objects --no-reuse-delta` (`repack.c:27-28`): every
            // delta is searched for afresh instead of being kept from the pack
            // it is already in.
            'f' => st.no_reuse_delta = true,
            // `-F` controls *object* reuse — copying a stored entry's bytes —
            // which this writer never does, so it already behaves as asked.
            // `-l` scopes the search to local packs and `-i` enables delta
            // islands; neither is modelled.
            'F' | 'l' | 'i' => {}
            'g' => {
                // The remainder of the cluster is the value, else the next argv.
                let rest: String = chars[c + 1..].iter().collect();
                let value = if rest.is_empty() {
                    match args.get(*i + 1) {
                        Some(v) => {
                            *i += 1;
                            v.clone()
                        }
                        None => {
                            eprintln!("error: switch `g' requires a value");
                            return Some(ExitCode::from(129));
                        }
                    }
                } else {
                    rest
                };
                if let Err(code) = int_value("switch `g'", &value) {
                    return Some(code);
                }
                st.geometric = true;
                *i += 1;
                return None;
            }
            other => {
                eprint!("error: unknown switch `{other}'\n{USAGE}");
                return Some(ExitCode::from(129));
            }
        }
        c += 1;
    }
    *i += 1;
    None
}

/// The option conflicts stock git rejects before it does any work, in git's own
/// order. Each prints `fatal: <msg>` on stderr and exits 128.
fn preflight(st: &State) -> Option<ExitCode> {
    // die_for_incompatible_opt3(-A, -k/--keep-unreachable, --cruft)
    let triad = [
        (st.loosen_unreachable, "-A"),
        (st.keep_unreachable, "-k/--keep-unreachable"),
        (st.cruft, "--cruft"),
    ];
    let set: Vec<&str> = triad.iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect();
    match set.len() {
        2 => return Some(fatal(&format!(
            "options '{}' and '{}' cannot be used together",
            set[0], set[1]
        ))),
        3 => return Some(fatal(&format!(
            "options '{}', '{}', and '{}' cannot be used together",
            set[0], set[1], set[2]
        ))),
        _ => {}
    }

    if st.geometric && st.all_into_one {
        return Some(fatal("options '--geometric' and '-A/-a' cannot be used together"));
    }

    if st.write_bitmap == Some(true) && !st.all_into_one && !st.write_midx {
        return Some(fatal(
            "Incremental repacks are incompatible with bitmap indexes.  Use\n\
             --no-write-bitmap-index or disable the pack.writeBitmaps configuration.",
        ));
    }

    if st.filter_to && !st.filter {
        return Some(fatal(
            "option '--filter-to' can only be used along with '--filter'",
        ));
    }

    // git only knows name hash versions 1 and 2; it leaves everything at or
    // below 0 alone, since 0 is the "unset" default and a negative value never
    // reaches the hashing code.
    if st.name_hash_version > 2 {
        return Some(fatal(&format!(
            "invalid --name-hash-version option: {}",
            st.name_hash_version
        )));
    }

    None
}

/// git's `die()` shape: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}
