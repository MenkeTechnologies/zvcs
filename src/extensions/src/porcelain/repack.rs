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
//!     overflows a C `int` once its `k`/`m`/`g` suffix is applied — the last two
//!     for the options repack itself parses (`-g`, `--name-hash-version`,
//!     `--max-pack-size`, `--max-cruft-size`, `--combine-cruft-below-size`); for
//!     the four it forwards to `pack-objects` the same messages arrive later, see
//!     below
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
//!     repacked regardless — but `-a` is not an exemption from the notice:
//!     `builtin/repack.c:460-462` gates it on `!names.nr` alone, and an *empty*
//!     object store gives `pack-objects` nothing to write however total the
//!     `-a` is. `git init --bare b && git -C b repack -ad` prints it, which is
//!     also where `git gc`'s copy comes from (`builtin/gc.c:897` runs
//!     `repack -d -l`, `-a`/`-A` appended by `add_repack_all_option()`).
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
//!   * **`--filter-to=<value>`** is a pack *prefix*, not a directory:
//!     `write_filtered_pack()` hands it to `pack-objects` as the `base-name`
//!     positional (`prepare_pack_objects()`, `repack.c:43`), so the artifacts are
//!     `<value>-<hash>.pack` / `.idx` / `.rev`. `--filter-to=/tmp/x` therefore
//!     writes `/tmp/x-<hash>.pack`, and a `<value>` that happens to name an
//!     existing directory writes a *sibling* of it rather than anything inside
//!     it. git creates no directory for it either: when `<value>`'s parent does
//!     not exist, `pack-objects` cannot move its temporary file into place and
//!     the run ends 128 with
//!     `error: unable to write file <value>-<hash>.pack: No such file or
//!     directory` followed by `fatal: unable to rename temporary file to
//!     '<value>-<hash>.pack'`. Without `--filter-to` the destination is the run's
//!     own `packtmp` prefix, `<packdir>/.tmp-<pid>-pack`, which is what puts the
//!     filtered pack beside the main one.
//!   * **The locality rule**, `write_pack_opts_is_local()` (`repack.c:86-89`), is
//!     a plain `starts_with()` over the two *strings* — the destination as typed
//!     and `packdir` as `cmd_repack()` built it, which is
//!     `repo_get_object_directory()` plus `/pack` and so ordinarily relative
//!     (`.git/objects/pack` in a worktree, `objects/pack` in a bare repository
//!     opened as `.`). No path is resolved, so an *absolute* `--filter-to` naming
//!     the object store is still non-local. A non-local pack is kept out of
//!     `names` ("avoid putting packs written outside of the repository in the
//!     list of names", `repack.c:107-115`) and so is neither installed as
//!     `pack-<hash>` nor eligible to be `--preferred-pack`; it simply stays where
//!     it was written. A *local* `--filter-to` is the losing case in git:
//!     `generated_pack_populate()` looks for the artifacts under `packtmp`, finds
//!     none, and `generated_pack_install()` dies `fatal: pack-objects did not
//!     write a '.pack' file for pack <packtmp>-<hash>` (exit 128) — after the
//!     pack has been written at the prefix, and before `-d`, the MIDX write and
//!     `update-server-info`. All of it verified against git 2.55.0.
//!   * **`--window`/`--window-memory`/`--depth`/`--threads`** are `OPT_STRING`s
//!     in repack's own table (`builtin/repack.c:206-213`): repack never parses
//!     them, it forwards them verbatim and `pack-objects` is what rejects a bad
//!     one. So the diagnostic arrives *after* the whole of repack's parse and
//!     after its pre-flight `fatal:`s, in the order `prepare_pack_objects()`
//!     pushes them (window, window-memory, depth, threads) rather than in argv
//!     order: `--threads=x --window=y` reports `window`, and
//!     `--window= --filter=false` reports the filter spec. The messages are
//!     parse-options' own — `expects an integer value with an optional k/m/g
//!     suffix`, `expects a non-negative integer value...` for the magnitude,
//!     `expects a numerical value` for an empty one, and the
//!     `not in range [-2147483648,2147483647]` overflow — all at exit 129. The
//!     accepted values then shadow `pack.window`, `pack.windowMemory`,
//!     `pack.depth` and `pack.threads` in the delta search.
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
//!   * **`--cruft`** writes the cruft pack and its `.mtimes` sidecar, but dates
//!     an object that came out of an existing *cruft* pack by that pack's file
//!     mtime rather than by the stamp its `.mtimes` recorded — there is no
//!     reader for that format here. `gc.recentObjectsHook`
//!     (`load_gc_recent_objects()`, reachable.c:189-191) is not consulted under
//!     `--cruft-expiration` either, so an object it would have called recent is
//!     dated by its own mtime. `--combine-cruft-below-size` and
//!     `--max-cruft-size` do not size or split the result: one cruft pack is
//!     always written, and `--expire-to` writes no second one — with no
//!     `--cruft-expiration` beside it the pack it would hold is empty and
//!     `--non-empty` suppresses it anyway (`cmd_repack()`,
//!     builtin/repack.c:510-544).
//!   * **`--max-pack-size`** does not split the output; one pack is always
//!     written. Its diagnostics *are* reproduced: a value below 1 MiB warns
//!     `warning: minimum pack size limit is 1 MiB`, and `pack.packSizeLimit`
//!     supplies the default (validated ahead of parse-options, so an unreadable
//!     value is fatal even for `-h`).
//!   * **`--geometric`** selects the subset of packs that restores a geometric
//!     size progression, through the ported `init_pack_geometry()` /
//!     `split_pack_geometry()` (builtin/repack.c:323-445): the new pack holds
//!     the objects of every pack below the split plus every loose object, minus
//!     anything a surviving pack already holds, and `-d` removes the rolled-up
//!     packs alone. What it does *not* model is git's overflow deaths (`pack %s
//!     too large to consider in geometric progression` / `to roll up`), which
//!     need a pack whose object count overflows `uint32_t` times the factor —
//!     the arithmetic here is 64-bit and saturating, so those packs are simply
//!     weighed.
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
//!   * **`-f`/`-F`/`--path-walk`/`--delta-islands`/`--name-hash-version`** tune
//!     parts of git's search that have no counterpart here, and stay no-ops.
//!   * `repack.writeBitmaps`, or its older spelling `pack.writeBitmaps`, turns
//!     `-b` on by itself; `--no-write-bitmap-index` overrides either.
//!   * `repack.useDeltaBaseOffset` *is* read, and picks `OBJ_OFS_DELTA` over
//!     `OBJ_REF_DELTA`. `repack.packKeptObjects` is not: it tunes a kept-object
//!     exclusion this writer does not perform. `repack.cruftWindow` /
//!     `repack.cruftWindowMemory` / `repack.cruftDepth` / `repack.cruftThreads`
//!     are not read here either: the cruft pack this module writes uses the same
//!     delta search the main pack does. [`super::gc`] does tune its own cruft
//!     pack with all four. `repack.updateServerInfo` *is* honoured, since the closing
//!     `update-server-info` it gates is real; see [`execute`].
//!   * `--filter=sparse:oid=<rev>` is accepted on syntax alone — git's rejection
//!     of it depends on resolving and parsing the named blob;
//!   * `combine:` sub-specs are not percent-decoded.
//!   * `repack.midxSplitFactor` and `repack.midxNewLayerThreshold` size the
//!     geometric merge of incremental MIDX layers, which `--write-midx=incremental`
//!     refuses above — but git range-checks both *unconditionally*
//!     (`builtin/repack.c:291-296`), so the refusals are reproduced and the
//!     values themselves steer nothing. `repack.midxMustContainCruft` *is*
//!     honoured: it decides whether the `multi-pack-index` covers the cruft
//!     packs, through [`resolve_midx_cruft`].
//!   * `pack.useBitmaps` and `pack.allowPackReuse` are read where git's
//!     `pack-objects` child would have read them — in [`execute`], so `repack -h`
//!     still prints usage — and validated, but neither bitmap-accelerated
//!     counting nor verbatim pack reuse exists here. The four `pack.*` booleans
//!     `prepare_repo_settings()` owns are handled the same way, one layer up in
//!     [`crate::repo_settings`].
//! ```

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::odb::pack;

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
    // `OPT_STRING` in git's table (`builtin/repack.c:206-213`): repack does not
    // parse these, it forwards them to `pack-objects`, which is what rejects a
    // bad value — see [`forwarded_value_check`].
    OptDef { long: "window", kind: Kind::Str, negatable: true },
    OptDef { long: "window-memory", kind: Kind::Str, negatable: true },
    OptDef { long: "depth", kind: Kind::Str, negatable: true },
    OptDef { long: "threads", kind: Kind::Str, negatable: true },
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
    /// `--cruft-expiration=<approxidate>`, kept as typed: its presence picks
    /// `enumerate_and_traverse_cruft_objects()` over `enumerate_cruft_objects()`
    /// (pack-objects.c:4349-4352), and its parsed value dates the cut.
    cruft_expiration: Option<String>,
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
    /// `geometric_factor` itself: `--geometric=0` parses but is *falsy*, and
    /// `if (geometric_factor)` (builtin/repack.c) is what gates the whole geometric path —
    /// including the `--pack-loose-unreachable` it hands `pack-objects`.
    geometric_factor: f64,
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
    /// The last `--filter-to` value: a pack *prefix*, not a directory, and so
    /// the `destination` of git's `write_pack_opts` for the filtered pack.
    filter_to_prefix: Option<String>,
    /// The last `--window`, `--window-memory`, `--depth` and `--threads`, each
    /// as the raw string git's `OPT_STRING` kept and `None` once the matching
    /// `--no-` form cleared it. Validated by [`forwarded_value_check`] rather
    /// than at parse time, because git's validation happens in the
    /// `pack-objects` child.
    window: Option<String>,
    window_memory: Option<String>,
    depth: Option<String>,
    threads: Option<String>,
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
    // rather than printing usage. The same is true of the three `repack.midx*`
    // keys `repack_config()` reads (`builtin/repack.c:97-110`), which is why they
    // are loaded here rather than next to the MIDX write they steer.
    let mut midx_cfg = MidxConfig::DEFAULT;
    let pack_size_limit_cfg = match crate::setup::discover() {
        Ok(repo) => {
            let limit = match crate::config::config_ulong(&repo, "pack.packSizeLimit") {
                Ok(limit) => limit,
                Err(message) => {
                    eprintln!("fatal: {message}");
                    return Ok(ExitCode::from(128));
                }
            };
            midx_cfg = match MidxConfig::load(&repo) {
                Ok(cfg) => cfg,
                Err(message) => {
                    eprintln!("fatal: {message}");
                    return Ok(ExitCode::from(128));
                }
            };
            limit
        }
        Err(_) => None,
    };

    let state = match parse(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok(state) => state,
    };

    if let Some(code) = preflight(&state, &midx_cfg) {
        return Ok(code);
    }

    // The four `OPT_STRING`s repack forwards are parsed by the `pack-objects`
    // child, which `cmd_repack()` starts only after the pre-flight `fatal:`s
    // above — so their diagnostics come last, and in the child's argv order.
    if let Some(code) = forwarded_value_check(&state) {
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

    execute(&state, &midx_cfg)
}

/// git's 1 MiB floor for `pack_size_limit`: any smaller non-zero limit warns and
/// is then raised to this.
const MIN_PACK_SIZE_LIMIT: u64 = 1024 * 1024;

/// The three `repack.midx*` keys `repack_config()` reads
/// (`builtin/repack.c:97-110`), with their defaults from
/// `DEFAULT_MIDX_SPLIT_FACTOR` / `DEFAULT_MIDX_NEW_LAYER_THRESHOLD`
/// (`builtin/repack.c:242-243`).
///
/// # Where each is honoured
///
/// * `midx_must_contain_cruft` reaches [`execute`], which drops cruft packs from
///   the `multi-pack-index` when it is false. That is the same set
///   `repack-midx.c:199-235` computes.
/// * `split_factor` and `new_layer_threshold` steer the geometric merge of
///   *incremental* MIDX layers (`--write-midx=incremental`), which this port
///   refuses outright — see the module docs. Their range checks are still
///   reproduced, because git runs them unconditionally at
///   `builtin/repack.c:291-296` whether or not a MIDX is being written at all.
///
/// # One deviation, in the diagnostics' order
///
/// git reads these through a `repo_config` *callback*, so when two of them hold
/// unreadable values the one that appears first in the configuration is the one
/// named. This port reads key by key in a fixed order (`repack.midxSplitFactor`,
/// then `repack.midxNewLayerThreshold`, then `repack.midxMustContainCruft`, all
/// after `pack.packSizeLimit`), so it names the first of those instead. Each key
/// on its own is byte-identical; only the pairing differs.
#[derive(Copy, Clone, Debug)]
struct MidxConfig {
    /// `repack.midxSplitFactor`, git's `git_config_int`. Checked against its
    /// floor of 2 later, in [`preflight`].
    split_factor: i64,
    /// `repack.midxNewLayerThreshold`, floor 1.
    new_layer_threshold: i64,
    /// `repack.midxMustContainCruft`, git's `git_config_bool`, default true.
    must_contain_cruft: bool,
}

impl MidxConfig {
    /// `builtin/repack.c:242-243`, the values `repack_config()` starts from.
    const DEFAULT: Self = MidxConfig {
        split_factor: 2,
        new_layer_threshold: 8,
        must_contain_cruft: true,
    };

    /// Read the three keys, `Err` carrying git's `die()` line minus `fatal: `.
    fn load(repo: &gix::Repository) -> Result<Self, String> {
        let mut cfg = Self::DEFAULT;
        if let Some(v) = crate::config::config_int(repo, "repack.midxsplitfactor")? {
            cfg.split_factor = v;
        }
        if let Some(v) = crate::config::config_int(repo, "repack.midxnewlayerthreshold")? {
            cfg.new_layer_threshold = v;
        }
        if let Some(v) = crate::repo_settings::config_bool_strict(repo, "repack.midxmustcontaincruft")? {
            cfg.must_contain_cruft = v;
        }
        Ok(cfg)
    }
}

/// Do the repacking, for a repository discovered from the current directory.
///
/// git reaches the object database only after every check above, so this is also
/// where "not a git repository" is diagnosed.
fn execute(st: &State, midx: &MidxConfig) -> Result<ExitCode> {
    let Ok(repo) = crate::setup::discover() else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };
    let objdir = repo.objects.store_ref().path().to_path_buf();
    let pack_dir = objdir.join("pack");

    // `git_pack_config()`'s two remaining keys. git reaches them in the
    // `pack-objects` child this port packs inline instead, so an unreadable
    // value is fatal for a real run and *not* for `repack -h` — which is what
    // git 2.55.0 does: `-c pack.allowPackReuse=bogus repack -h` prints the usage
    // block, while the same config on `repack -a -d` dies. See
    // [`super::pack_objects::PackConfig`] for what each would have steered.
    match crate::repo_settings::RepoSettings::load(&repo)
        .and_then(|settings| super::pack_objects::PackConfig::load(&repo, &settings))
    {
        Ok(_) => {}
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    }

    // `existing->midx_packs`, read before `-d` can delete a pack out from under
    // the MIDX; see [`resolve_midx_cruft`].
    let existing_midx_packs = super::multi_pack_index::midx_pack_names(&pack_dir);
    // git's `packdir` *string*, which is the only thing the locality rule looks
    // at; see [`git_packdir`].
    let packdir = git_packdir(&repo, &objdir);

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

    // The `pack-objects --all` child dies on a ref naming an object the repository does not
    // have — `get_reference()`'s `die("bad object %s", name)` (revision.c) — and `repack`
    // reports that failure and nothing else, since the child's `error:` line is the ref
    // store's and `-q` is passed to the child, not to the ref walk.
    if let Some(name) = super::prune::bad_object_ref(&repo) {
        crate::git_fatal!("bad object {name}");
    }

    // ```c
    // if (repo_has_promisor_remote(repo))
    //         strvec_push(&cmd.args, "--exclude-promisor-objects");
    // ```
    //
    // (`cmd_repack()`, builtin/repack.c:354-355.) What that does to the walk is
    // `odb_for_each_object(..., mark_uninteresting, ...,
    // ODB_FOR_EACH_OBJECT_PROMISOR_ONLY)` (revision.c:4001-4003): every object a
    // `.promisor` pack holds is marked UNINTERESTING before the traversal
    // starts, so it is neither packed nor walked through — which is also what
    // keeps the walk off the blobs such a pack promises but does not hold.
    // Those objects are not dropped: `repack_promisor_objects()` writes them
    // into a promisor pack of their own, below.
    let promisor_held = match super::rev_list::has_promisor_remote(&repo) {
        true => super::rev_list::promisor_pack_objects(&repo),
        false => HashSet::new(),
    };

    // git's `--all --reflog --indexed-objects`, which `prune` already builds.
    let mut roots = Vec::new();
    super::prune::collect_roots(&repo, &mut roots)?;
    let reachable = super::prune::close_over_excluding(&repo, roots, &promisor_held);

    let existing = super::prune::pack_indices(&repo, &objdir);
    // ```c
    // if (geometric_factor) {
    //         [...]
    //         init_pack_geometry(&geometry, &existing_kept_packs);
    //         split_pack_geometry(geometry, geometric_factor);
    // ```
    //
    // (`cmd_repack()`, builtin/repack.c:872-875.) The split is what turns
    // `--geometric` from "repack everything" into "repack only the light end".
    let geometry = match st.geometric_factor != 0.0 {
        true => Some(Geometry::compute(
            st,
            &existing,
            &pack_dir,
            st.geometric_factor,
        )),
        false => None,
    };
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
                // `--indexed-objects` adds the index's blobs to the same walk,
                // where the UNINTERESTING mark applies to them too.
                .filter(|id| !promisor_held.contains(id))
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
    // ```c
    // if (keep_unreachable)
    //         strvec_push(&cmd.args, "--keep-unreachable");
    // [...]
    // if (geometric_factor) {
    //         [...]
    //         if (!keep_unreachable)
    //                 strvec_push(&cmd.args, "--pack-loose-unreachable");
    // ```
    //
    // (`cmd_repack()`, builtin/repack.c.) Both flags tell `pack-objects` to carry the
    // objects the traversal did *not* reach into the new pack instead of leaving them
    // where they are — which matters because `-d` then runs `prune_packed_objects()`, and a
    // loose object that made it into the pack is deleted while one that did not stays
    // loose. `--cruft` is deliberately not here: it writes the unreachable objects to a
    // cruft pack of its own.
    if let Some(geo) = geometry.as_ref().filter(|_| !st.cruft) {
        // ```c
        // } else if (geometry) {
        //         strvec_push(&cmd.args, "--stdin-packs");
        //         strvec_push(&cmd.args, "--unpacked");
        // ```
        //
        // (`cmd_repack()`, builtin/repack.c:936-938), fed the packs below the
        // split as includes and the ones above it as `^` excludes
        // (:953-965). `read_packs_list_from_stdin()` marks every excluded pack
        // `pack_keep_in_core` with `ignore_packed_keep_in_core` set, so an
        // object one of them holds is dropped however it was reached, and
        // `--unpacked` adds `add_unreachable_loose_objects()` on top —
        // *every* loose object, reachable or not
        // (builtin/pack-objects.c:4465-4470, :3826-3834).
        let survivors = geo.surviving_objects(&existing);
        let mut packed: HashSet<ObjectId> = to_pack.iter().copied().collect();
        let rolled = geo.rolled_up_objects(&existing);
        for id in rolled
            .into_iter()
            .chain(super::prune::loose_object_ids(&repo, &objdir))
        {
            if !survivors.contains(&id) && packed.insert(id) {
                to_pack.push(id);
            }
        }
    } else if st.keep_unreachable && !st.cruft {
        let packed: HashSet<ObjectId> = to_pack.iter().copied().collect();
        for id in super::prune::all_object_ids(&repo, &objdir) {
            if !packed.contains(&id) {
                to_pack.push(id);
            }
        }
    }
    // The pack's entry order is ours to choose; sorting makes a run reproducible.
    to_pack.sort();
    to_pack.dedup();

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
    let mut in_new_pack: HashSet<ObjectId> = to_pack.iter().copied().collect();
    // `names` at that point holds the promisor pack too, so its objects are
    // `^`-excluded from the filtered pack alongside the main pack's.
    if st.all_into_one {
        in_new_pack.extend(promisor_held.iter().copied());
    }
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
    let superseded: Vec<PathBuf> = match (st.delete_redundant, st.all_into_one, geometry.as_ref()) {
        (false, _, _) => Vec::new(),
        (true, true, _) => existing
            .iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| droppable(st, p))
            .collect(),
        // ```c
        // for (i = 0; i < geometry->split; i++) {
        //         struct packed_git *p = geometry->pack[i];
        //         [...]
        //         remove_redundant_pack(packdir, buf.buf);
        // }
        // ```
        //
        // (`cmd_repack()`, builtin/repack.c:1133-1149.) Only the packs that were
        // rolled into the new one go; everything above the split is what the
        // progression is made of and stays where it is.
        (true, false, Some(geo)) => geo.rolled_up_paths(&existing, &pack_dir),
        (true, false, None) => Vec::new(),
    };
    // What `write_cruft_pack()` (repack-cruft.c:40-98) feeds `pack-objects
    // --cruft`: every local pack, the kept ones as INCLUDE and the rest — the
    // non-kept and the cruft packs alike — with a `-`. `read_cruft_objects()`
    // (pack-objects.c:4300-4357) marks the INCLUDE ones kept-in-core, so
    // `add_objects_in_unpacked_packs()` (:4501-4526) skips their objects and
    // takes everything else, and `add_unreachable_loose_objects()` (:4564-4568)
    // adds every loose object on top. `want_found_object()` then drops whatever
    // a kept pack — which by then includes the packs this run just wrote — also
    // holds.
    //
    // The stamp is the `.pack`'s mtime, which is what `add_recent_packed()` uses
    // for an object in an ordinary pack. An object that came out of an *existing
    // cruft* pack should carry the mtime that pack's `.mtimes` recorded for it
    // rather than the file's; that sidecar has no reader here, so it is dated by
    // its pack like any other.
    let (cruft_candidates, cruft_kept): (Vec<(ObjectId, u32)>, HashSet<ObjectId>) = match st.cruft {
        false => (Vec::new(), HashSet::new()),
        true => {
            let mut candidates = Vec::new();
            let mut kept = HashSet::new();
            for file in &existing {
                // `ODB_FOR_EACH_OBJECT_LOCAL_ONLY`: a borrowed pack is not this
                // repository's to repack.
                if file.path().parent() != Some(pack_dir.as_path()) {
                    continue;
                }
                if !droppable(st, file.path()) {
                    kept.extend(file.iter().map(|e| e.oid));
                    continue;
                }
                let stamp = super::prune::mtime_of(&file.path().with_extension("pack"))
                    .unwrap_or(0)
                    .clamp(0, i64::from(u32::MAX)) as u32;
                candidates.extend(file.iter().map(|e| (e.oid, stamp)));
            }
            (candidates, kept)
        }
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
    // git writes every pack a run produces under one temporary prefix and only
    // moves them into place afterwards, in a single `generated_pack_install()`
    // pass; anything that goes wrong in between therefore leaves the object
    // store as it found it. The prefix is `packtmp`, which is also the default
    // `--filter-to` destination.
    let packtmp_name = format!(".tmp-{}-pack", std::process::id());
    let packtmp = pack_dir.join(&packtmp_name);
    let packtmp_shown = format!("{packdir}/{packtmp_name}");
    // git's `names`: the hash of every pack this run wrote *into the
    // repository's own* `objects/pack`, with the number of objects it holds. It
    // is the set `write_midx_included_packs()` picks the preferred pack out of.
    let mut new_packs: Vec<(String, usize)> = Vec::new();
    // ```c
    // if (pack_everything & ALL_INTO_ONE) {
    //         repack_promisor_objects(repo, &po_args, &names, packtmp);
    // ```
    //
    // (builtin/repack.c:365-366.) `repack_promisor_objects()`
    // (repack-promisor.c:82-111) feeds `pack-objects` every object the promisor
    // packs hold — the objects `--exclude-promisor-objects` kept out of the main
    // pack — and then writes the empty `.promisor` file beside the result
    // (:56-69), so what `-d` is about to delete is replaced in kind rather than
    // lost. It runs before the main `pack-objects`, so its pack is the first
    // name in `names`.
    if st.all_into_one && !promisor_held.is_empty() {
        let mut ids: Vec<ObjectId> = promisor_held.iter().copied().collect();
        ids.sort();
        let path = write_pack(&repo, st, &ids, &packtmp, write_rev, progress, false)?;
        let hash = pack_hash(&pack_base_name(&path));
        // `write_promisor_file(promisor_name, NULL, 0)`: an empty file, named
        // for the pack it marks.
        fs::write(suffixed(&packtmp, &format!("-{hash}.promisor")), b"")?;
        new_packs.push((hash, ids.len()));
    }

    // Everything filtered out is about to be written elsewhere, so a run whose
    // spec rejects the whole set still has a second pack to produce.
    if !to_pack.is_empty() {
        let path = write_pack(
            &repo,
            st,
            &to_pack,
            &packtmp,
            write_rev,
            progress,
            // `cmd_repack()` asks `pack-objects` for a `.bitmap` only when no
            // MIDX is being written (`if (write_midx == REPACK_WRITE_MIDX_NONE)`
            // around both `--write-bitmap-index` pushes): with `--write-midx` the
            // bitmap belongs to the MIDX instead, and git leaves the packs bare.
            write_bitmaps(st, &repo) && !st.write_midx,
        )?;
        new_packs.push((pack_hash(&pack_base_name(&path)), to_pack.len()));
    }

    // `if (!names.nr)` (`builtin/repack.c:460-462`): git says so and carries on,
    // and it says so about the *first* `pack-objects` alone — the notice sits
    // between that child and the cruft and filtered packs, so a run that goes on
    // to write a filtered pack still prints it. Everything after the pack write
    // still runs too — in particular `-d`'s `prune_packed_objects()`, which is
    // what drops the loose copies of objects an *existing* pack already holds,
    // and `--write-midx`. Returning here instead left those loose objects behind.
    //
    // The gate is `!names.nr && !po_args.quiet` and nothing else: `-a` does not
    // exempt a run from it. `-a` normally has something to pack, so the two
    // conditions coincide almost everywhere — but an empty object store leaves
    // `pack-objects` with nothing to write whatever the mode, and stock prints
    // the notice there. Testing `all_into_one` here made
    // `git init --bare b && git -C b repack -ad` silent where stock says so.
    // The gate is `names` rather than `to_pack`, which are the same set
    // everywhere but a partial clone: there the promisor pack is a name the run
    // wrote, so a main pack-objects that found nothing left to do is silent.
    if new_packs.is_empty() && !st.quiet {
        println!("Nothing new to pack.");
    }

    // ```c
    // if (pack_everything & PACK_CRUFT) {
    //         [...]
    //         ret = write_cruft_pack(&opts, cruft_expiration,
    //                                combine_cruft_below_size, &names,
    //                                &existing);
    // ```
    //
    // (`cmd_repack()`, builtin/repack.c:480-506.) It runs after the main
    // `pack-objects` and before the filtered pack, so `names` — the set whose
    // objects are already delivered — is the main pack plus the promisor one.
    // What is left over is by construction what the traversal did not reach.
    if st.cruft {
        let mut stamps: HashMap<ObjectId, u32> = HashMap::new();
        for (id, stamp) in cruft_candidates {
            if in_new_pack.contains(&id) || cruft_kept.contains(&id) {
                continue;
            }
            // Two packs holding one object date it by the newer of them, which
            // is the copy `add_object_in_unpacked_pack()` reaches last.
            let slot = stamps.entry(id).or_insert(stamp);
            *slot = (*slot).max(stamp);
        }
        for id in super::prune::all_object_ids(&repo, &objdir) {
            if in_new_pack.contains(&id) || cruft_kept.contains(&id) {
                continue;
            }
            let stamp = super::prune::mtime_of(&loose_path(&objdir, &id))
                .unwrap_or(0)
                .clamp(0, i64::from(u32::MAX)) as u32;
            // A loose copy is the fresher one by definition: `add_loose_object()`
            // stamps it with its own `st_mtime` whatever a pack said.
            stamps.insert(id, stamp);
        }
        // ```c
        // if (cruft_expiration)
        //         enumerate_and_traverse_cruft_objects(&fresh_packs);
        // else
        //         enumerate_cruft_objects();
        // ```
        //
        // (`read_cruft_objects()`, pack-objects.c:4349-4352.) With a date, only
        // the objects *newer* than it are tips (`obj_is_recent()`,
        // reachable.c:183-192, a strict `>`), and the pack is their closure:
        // an older object still goes in when something recent reaches it, since
        // dropping it would break the recent object. `show_cruft_object()`
        // (pack-objects.c:4188-4201) stamps one reached that way with the
        // expiration itself rather than its real mtime, so it is still "too old"
        // on the next run with the same date.
        //
        // `load_gc_recent_objects()`'s `gc.recentObjectsHook` (reachable.c:189-191)
        // has no reader here, so an object it would have rescued is dated by its
        // own mtime like any other.
        if let Some(spec) = st.cruft_expiration.as_deref() {
            let expire = crate::date::parse_expiry_date(spec).unwrap_or(0);
            let expire = expire.clamp(0, i64::from(u32::MAX)) as u32;
            let tips: Vec<ObjectId> = stamps
                .iter()
                .filter(|(_, mtime)| **mtime > expire)
                .map(|(id, _)| *id)
                .collect();
            // `cruft_include_check()` (pack-objects.c:4208-4216): the walk stops
            // at anything a kept pack holds, which by now includes the packs this
            // run wrote.
            let mut delivered: HashSet<ObjectId> = in_new_pack.clone();
            delivered.extend(cruft_kept.iter().copied());
            let reached = super::prune::close_over_excluding(&repo, tips, &delivered);
            stamps = reached
                .into_iter()
                .map(|id| {
                    let mtime = stamps.get(&id).copied().filter(|m| *m > expire).unwrap_or(expire);
                    (id, mtime)
                })
                .collect();
        }

        let mut cruft: Vec<ObjectId> = stamps.keys().copied().collect();
        cruft.sort();
        // `--non-empty`: with nothing left over there is no cruft pack, which is
        // the ordinary case for a repository whose objects are all reachable.
        if !cruft.is_empty() {
            let path = write_pack(&repo, st, &cruft, &packtmp, write_rev, progress, false)?;
            let hash = pack_hash(&pack_base_name(&path));
            write_mtimes(&repo, &path, &suffixed(&packtmp, &format!("-{hash}.mtimes")), &stamps)?;
            new_packs.push((hash, cruft.len()));
        }
    }

    // With `--filter` git writes a second pack holding the filtered-out objects.
    // The objects have to travel with it: they are only reachable through this
    // pack once `-d` removes the ones they came from.
    if st.filter {
        // `write_pack_opts` for the filtered pack: `destination` is
        // `--filter-to` when given and `packtmp` otherwise (`cmd_repack()`,
        // `builtin/repack.c:547-557`). Both are pack *prefixes*, which
        // `pack-objects` completes with `-<hash>` and an extension.
        let destination = st.filter_to_prefix.as_deref().unwrap_or(&packtmp_shown);
        // `write_pack_opts_is_local()`: a plain prefix test on the two strings,
        // with no path resolution of either.
        let local = is_local_destination(destination, &packdir);
        let prefix = if destination == packtmp_shown {
            packtmp.clone()
        } else {
            PathBuf::from(destination)
        };
        let outcome = if filtered_out.is_empty() {
            // git still writes the pack, its index and its reverse index when the
            // spec rejected nothing; their presence is what marks a filtered run.
            write_empty_pack(repo.object_hash(), &prefix, write_rev)
        } else {
            // No bitmap: a bitmap describes a reachability closure, and this pack
            // is deliberately a fragment of one.
            write_pack(&repo, st, &filtered_out, &prefix, write_rev, progress, false)
                .map(|path| pack_base_name(&path))
        };
        let stem = match outcome {
            Ok(stem) => stem,
            // git creates no directory for `--filter-to`: `pack-objects` writes
            // its temporary files under the object store and then cannot move
            // them into a directory that is not there.
            Err(e) => match e.downcast_ref::<UnplaceablePack>() {
                Some(UnplaceablePack(path)) => {
                    let shown = path.display();
                    eprintln!("error: unable to write file {shown}: No such file or directory");
                    eprintln!("fatal: unable to rename temporary file to '{shown}'");
                    return Ok(ExitCode::from(128));
                }
                None => return Err(e),
            },
        };
        if local {
            if destination == packtmp_shown {
                // `finish_pack_objects_cmd()` appends a locally-written pack to
                // `names`, which is the set `--preferred-pack` is drawn from and
                // the set `generated_pack_install()` installs.
                new_packs.push((pack_hash(&stem), filtered_out.len()));
            } else {
                // A local `--filter-to` that is not `packtmp` is the one
                // combination git cannot complete: the pack is in `names`, but
                // `generated_pack_populate()` looked for its artifacts under
                // `packtmp` and found none, so `generated_pack_install()` dies —
                // after the pack was written at the prefix, and before anything
                // was installed.
                eprintln!(
                    "fatal: pack-objects did not write a '.pack' file for pack {packtmp_shown}-{}",
                    pack_hash(&stem)
                );
                return Ok(ExitCode::from(128));
            }
        }
    }

    // `generated_pack_install()`: every pack in `names` moves from `packtmp` to
    // `<packdir>/pack-<hash>`, extension by extension.
    let mut installed: Vec<PathBuf> = Vec::new();
    for (hash, _) in &new_packs {
        installed.push(install_pack(&pack_dir, &packtmp, hash)?);
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
        if let Some((hash, 0)) = new_packs.first() {
            // git names the pack the way it opened it, i.e. under the object
            // directory as `get_object_directory()` renders it.
            let shown = super::prune_packed::display_objdir(&repo, &objdir);
            eprintln!(
                "error: cannot select preferred pack {} with no objects",
                shown.join("pack").join(format!("pack-{hash}.pack")).display()
            );
            return Ok(ExitCode::from(255));
        }
    }

    if st.delete_redundant {
        for index_path in superseded {
            // An identical object set hashes to the same name, in which case the
            // "superseded" pack *is* one this run just installed.
            if installed.contains(&index_path) {
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
        if resolve_midx_cruft(midx.must_contain_cruft, &new_packs, existing_midx_packs.as_deref(), &pack_dir) {
            super::multi_pack_index::write_midx(&pack_dir, repo.object_hash())?;
        } else {
            super::multi_pack_index::write_midx_without_cruft(&pack_dir, repo.object_hash())?;
        }
    }

    if run_server_info {
        let _ = super::update_server_info::update_server_info(&["update-server-info".to_string()])?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Whether the `multi-pack-index` about to be written must include the cruft
/// packs — `repack.midxMustContainCruft` after the two overrides git applies.
///
/// The key is a *permission to omit*, not an instruction to omit, and git takes
/// that permission away twice:
///
/// 1. `builtin/repack.c:460-478`. When the run wrote no new pack, the surviving
///    non-cruft packs may still reference objects that only the cruft pack now
///    holds, so the MIDX has to cover them — unless a MIDX already exists, in
///    which case the next rule decides instead.
/// 2. `repack-midx.c:199-200`, `midx_has_unknown_packs()`. If the *existing*
///    MIDX names a pack the new one would not, that pack may be part of a
///    bitmap's reachability closure, and the cruft packs are kept for the same
///    reason. In git a pack from the old MIDX counts as known when it is in the
///    include list, or (with `--geometric`) below the split line, or in
///    `non_kept_packs` and not marked for deletion. This port has no geometric
///    split and no deletion marks — the packs still on disk at this point *are*
///    the survivors, and `write_midx_without_cruft` includes exactly the
///    non-cruft ones — so "known" collapses to "still in the new include set".
fn resolve_midx_cruft(
    must_contain_cruft: bool,
    new_packs: &[(String, usize)],
    existing_midx_packs: Option<&[String]>,
    pack_dir: &Path,
) -> bool {
    if must_contain_cruft {
        return true;
    }
    if new_packs.is_empty() && existing_midx_packs.is_none() {
        return true;
    }
    let Some(existing) = existing_midx_packs else {
        return false;
    };
    // The include set the new MIDX would carry: every non-cruft `.idx` left in
    // the directory.
    let include: std::collections::HashSet<String> = fs::read_dir(pack_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with(".idx"))
        .filter(|name| !pack_dir.join(name).with_extension("mtimes").is_file())
        .collect();
    existing.iter().any(|name| !include.contains(name))
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

/// Encode `ids` as a pack under the pack *prefix* `base_prefix`, which
/// `pack-objects` completes with `-<hash>` and the extension of each artifact,
/// and hand back the path of the `.idx`.
fn write_pack(
    repo: &gix::Repository,
    st: &State,
    ids: &[ObjectId],
    base_prefix: &Path,
    write_rev: bool,
    progress: bool,
    write_bitmap: bool,
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
            no_reuse_delta: st.no_reuse_delta,
            // The four `pack-objects` forwards, already validated by
            // [`forwarded_value_check`], shadowing `pack.window`,
            // `pack.windowMemory`, `pack.depth` and `pack.threads`. A negative
            // value is dropped rather than forwarded: git lets `OPT_INTEGER`
            // take one and the delta search here has no meaning for it.
            window: forwarded_size(st.window.as_deref()),
            window_memory: forwarded_size(st.window_memory.as_deref()).map(|n| n as u64),
            depth: forwarded_size(st.depth.as_deref()),
            threads: forwarded_size(st.threads.as_deref()),
            ..super::pack_objects::WriteOptions::default()
        },
    )?;
    if packed.entries.is_empty() {
        crate::git_fatal!("pack writer produced no files for {} objects", ids.len());
    }

    let kind = repo.object_hash();
    let base = suffixed(base_prefix, &format!("-{}", packed.id));
    install(&suffixed(&base, ".pack"), &packed.bytes)?;

    // Both companions index into the pack in object-id order.
    let mut by_oid = packed.entries.clone();
    by_oid.sort_unstable_by_key(|entry| entry.id);
    let index_path = suffixed(&base, ".idx");
    install(
        &index_path,
        &super::pack_objects::index_file(kind, 2, &packed.id, &by_oid)?,
    )?;
    if write_rev {
        install(
            &suffixed(&base, ".rev"),
            &super::pack_objects::reverse_index_file(kind, &packed.id, &by_oid)?,
        )?;
    }
    if write_bitmap {
        let mut options = super::pack_objects::BitmapOptions::from_repo(repo);
        options.write = true;
        if let Some(bytes) = super::pack_objects::bitmap_file(repo, &packed, &options) {
            fs::write(suffixed(&base, ".bitmap"), bytes)?;
        }
    }
    Ok(index_path)
}

/// The loose path an object id would live at, `objects/ab/cdef…`.
fn loose_path(objdir: &Path, id: &ObjectId) -> PathBuf {
    let hex = id.to_string();
    objdir.join(&hex[..2]).join(&hex[2..])
}

/// The `.mtimes` sidecar for a pack just written: one 32-bit stamp per object,
/// in the `.idx`'s order, which is object-id order.
///
/// `write_promisor_file()`'s neighbour in `finish_pack_objects_cmd()`: git has
/// `pack-objects --cruft` write it, this port writes it beside the pack the same
/// way it writes the `.idx` and `.rev`.
fn write_mtimes(
    repo: &gix::Repository,
    index_path: &Path,
    to: &Path,
    stamps: &HashMap<ObjectId, u32>,
) -> Result<()> {
    let hash = repo.object_hash();
    let index = pack::index::File::at(index_path, hash)
        .with_context(|| format!("read back {}", index_path.display()))?;
    let ordered: Vec<u32> =
        index.iter().map(|e| stamps.get(&e.oid).copied().unwrap_or(0)).collect();
    install(to, &super::gc::mtimes_bytes(hash, &ordered, index.pack_checksum().as_slice())?)
}

/// Put one pack artifact in place, `0444` and by rename, as git installs them.
///
/// The mode is why the rename matters: a pack whose object set has not changed
/// hashes to the name it already has on disk, so writing straight to that path
/// would land on the read-only file the last run left and fail with `EACCES`.
fn install(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // A bare `--filter-to=pfx` has no directory component at all, which is the
    // current one rather than a missing one.
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !dir.is_dir() {
        return Err(UnplaceablePack(path.to_path_buf()).into());
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("tmp");
    let tmp = dir.join(format!("tmp_{ext}_zvcs_repack_{}", std::process::id()));
    fs::write(&tmp, bytes)
        .with_context(|| format!("unable to write {}", tmp.display()))?;
    // git does not check its own chmod either, so a filesystem that refuses the
    // mode is not a failure.
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o444));
    fs::rename(&tmp, path).with_context(|| format!("unable to rename to {}", path.display()))
}

/// The `<name>-<hash>` stem of a pack artifact.
fn pack_base_name(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string()
}

/// The hash a pack is named after, which is what git carries in its `names` list
/// and interpolates into `pack-%s.pack`. It is the stem's last `-`-separated
/// field, the prefix before it being free to contain more of them.
fn pack_hash(stem: &str) -> String {
    stem.rsplit_once('-').map_or(stem, |(_, hash)| hash).to_string()
}

/// `generated_pack_install()`: move one pack from the run's temporary prefix to
/// `<packdir>/pack-<hash>`, and hand back the path of its installed `.idx`.
///
/// The extension order is git's `exts[]`, `.idx` last, so the pack is complete
/// before the index that makes it findable appears. An extension this run did
/// not write is *removed* at the destination rather than left there, which is
/// what keeps a previous run's `.bitmap` from outliving the pack it described.
fn install_pack(pack_dir: &Path, packtmp: &Path, hash: &str) -> Result<PathBuf> {
    let from = suffixed(packtmp, &format!("-{hash}"));
    let to = pack_dir.join(format!("pack-{hash}"));
    for ext in [".pack", ".rev", ".mtimes", ".bitmap", ".promisor", ".idx"] {
        let src = suffixed(&from, ext);
        let dst = suffixed(&to, ext);
        if src.exists() {
            fs::rename(&src, &dst)
                .with_context(|| format!("renaming pack to '{}' failed", dst.display()))?;
        } else if matches!(ext, ".pack" | ".idx") {
            // git's non-optional extensions, where an absent tempfile is a `die`
            // rather than something to clean up after.
            bail!("pack-objects did not write a '{ext}' file for pack {}", from.display());
        } else {
            let _ = fs::remove_file(&dst);
        }
    }
    Ok(suffixed(&to, ".idx"))
}

/// One of the four forwarded values as a size the delta search can use, or
/// `None` when the option was absent or its value negative.
fn forwarded_size(value: Option<&str>) -> Option<usize> {
    value.and_then(scaled).and_then(|n| usize::try_from(n).ok())
}

/// Append `suffix` to the last component of `prefix`, which is how a pack prefix
/// becomes a pack name: `objects/pack/pack` + `-<hash>` is
/// `objects/pack/pack-<hash>`. `Path::join` would make a new component of it.
fn suffixed(prefix: &Path, suffix: &str) -> PathBuf {
    let mut name = prefix.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// A pack artifact that cannot be moved into place because its directory does
/// not exist — git's `rename_tmp_packfile()` failure, reached through a
/// `--filter-to` prefix whose parent was never created. Carries the path git
/// names in both of the two lines it prints.
#[derive(Debug)]
struct UnplaceablePack(PathBuf);

impl std::fmt::Display for UnplaceablePack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unable to rename temporary file to '{}'", self.0.display())
    }
}

impl std::error::Error for UnplaceablePack {}

/// `write_pack_opts_is_local()` (`repack.c:86-89`): whether the pack this run is
/// about to write counts as one of the repository's own, and so belongs in
/// `names`.
///
/// git asks it of the two *strings* — `starts_with(opts->destination,
/// opts->packdir)` — and resolves neither, so an absolute `--filter-to` naming
/// the object store of a repository whose `packdir` is the usual relative
/// `.git/objects/pack` is *not* local. Verified against git 2.55.0, where
/// `--filter-to=$PWD/.git/objects/pack/x` leaves `x-<hash>.pack` in place and
/// exits 0 while `--filter-to=.git/objects/pack/x` exits 128.
fn is_local_destination(destination: &str, packdir: &str) -> bool {
    destination.starts_with(packdir)
}

/// git's `packdir` string: `mkpathdup("%s/pack", repo_get_object_directory())`.
///
/// The object directory is whatever setup left it as, which is relative to the
/// directory `cmd_repack()` runs in for a repository found there —
/// `.git/objects` in a worktree (git having already chdir'd to its root),
/// `objects` in a bare repository opened as `.`, `<dir>/objects` under an
/// explicit `--git-dir=<dir>` — and absolute only when it was given that way.
/// Read off `git rev-parse --git-path objects` under git 2.55.0 for each of
/// those four.
fn git_packdir(repo: &gix::Repository, objdir: &Path) -> String {
    let real_objdir = fs::canonicalize(objdir).unwrap_or_else(|_| objdir.to_path_buf());
    // git renders the object directory relative to the worktree root it chdir'd
    // to, or — with no worktree — to wherever the bare repository was found.
    let base = match repo.workdir() {
        Some(work) => fs::canonicalize(work).unwrap_or_else(|_| work.to_path_buf()),
        None => std::env::current_dir().ok().and_then(|cwd| fs::canonicalize(cwd).ok()).unwrap_or_default(),
    };
    let shown = real_objdir.strip_prefix(&base).unwrap_or(&real_objdir);
    shown.join("pack").to_string_lossy().into_owned()
}

/// Write the empty pack, its index and its reverse index under the pack prefix
/// `base_prefix`, returning the `<name>-<hash>` stem they share.
///
/// An empty pack has no objects to name it after, so its checksum — and
/// therefore its filename — is a constant for a given hash function.
fn write_empty_pack(kind: gix::hash::Kind, base_prefix: &Path, write_rev: bool) -> Result<String> {
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

    let base = suffixed(base_prefix, &format!("-{}", ObjectId::from_bytes_or_panic(&pack_id)));
    install(&suffixed(&base, ".pack"), &pack)?;
    install(&suffixed(&base, ".idx"), &idx)?;

    if write_rev {
        // The same layout `gix-pack`'s writer produces, with no permutation.
        let mut rev = Vec::new();
        rev.extend_from_slice(b"RIDX");
        rev.extend_from_slice(&1u32.to_be_bytes());
        rev.extend_from_slice(&(if kind == gix::hash::Kind::Sha1 { 1u32 } else { 2 }).to_be_bytes());
        rev.extend_from_slice(&pack_id);
        append_checksum(&mut rev, kind)?;
        install(&suffixed(&base, ".rev"), &rev)?;
    }
    Ok(pack_base_name(&suffixed(&base, ".idx")))
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
/// `struct pack_geometry` (builtin/repack.c:298-302): the packs a
/// `--geometric=<factor>` run weighs, ordered by object count, and the index
/// that separates the ones being rolled into a new pack from the ones left
/// alone.
///
/// The entries are positions in the caller's `existing` slice rather than the
/// index files themselves, so nothing here borrows the pack set the rest of
/// [`execute`] still needs to move around.
struct Geometry {
    /// `geometry->pack`: positions into `existing`, sorted by
    /// `geometry_cmp()` — ascending object count.
    packs: Vec<usize>,
    /// `geometry->split`. `packs[..split]` roll up; `packs[split..]` survive.
    split: usize,
}

/// `geometry_pack_weight()` (builtin/repack.c:304-309): a pack weighs what its
/// index says it holds.
fn geometry_pack_weight(index: &pack::index::File) -> u64 {
    u64::from(index.num_objects())
}

impl Geometry {
    /// `init_pack_geometry()` (builtin/repack.c:323-368) followed by
    /// `split_pack_geometry()` (:370-445).
    fn compute(st: &State, existing: &[pack::index::File], pack_dir: &Path, factor: f64) -> Self {
        // `for (p = get_all_packs(...))`, minus the two skips the loop makes:
        // a kept pack — `p->pack_keep` or a `--keep-pack` name, which is
        // exactly what [`droppable`] answers — and a cruft pack, which is the
        // one carrying a `.mtimes` sidecar.
        let mut packs: Vec<usize> = (0..existing.len())
            .filter(|&i| {
                let path = existing[i].path();
                path.parent() == Some(pack_dir)
                    && droppable(st, path)
                    && !path.with_extension("mtimes").exists()
            })
            .collect();
        // `QSORT(geometry->pack, geometry->pack_nr, geometry_cmp)`. git's
        // comparator looks at the weight alone and leaves ties in whatever
        // order the pack list had; the path breaks them here so a run is
        // reproducible.
        packs.sort_by_key(|&i| {
            (
                geometry_pack_weight(&existing[i]),
                existing[i].path().to_path_buf(),
            )
        });

        // `--geometric=<n>` is an integer option in git, so the factor is whole;
        // a negative one cannot reach here because `if (geometric_factor)` is
        // the gate and the parser has already rejected what is not a number.
        let factor = factor.max(0.0) as u64;
        let weight = |slot: usize| geometry_pack_weight(&existing[packs[slot]]);

        let split = if packs.is_empty() {
            0
        } else {
            // "First, count the number of packs (in descending order of size)
            // which already form a geometric progression." (:383-395)
            let mut i = packs.len() - 1;
            while i > 0 {
                if weight(i) < factor.saturating_mul(weight(i - 1)) {
                    break;
                }
                i -= 1;
            }
            let mut split = i;
            // "Move the split one to the right, since the top element in the
            // last-compared pair can't be in the progression." (:397-406)
            if split > 0 {
                split += 1;
            }
            // "creating that new pack may cause packs in the heavy half to no
            // longer form a geometric progression" — roll up as many of them as
            // it takes to restore it. (:408-443)
            let mut total: u64 = (0..split).map(weight).sum();
            for slot in split..packs.len() {
                if weight(slot) >= factor.saturating_mul(total) {
                    break;
                }
                total += weight(slot);
                split += 1;
            }
            split
        };

        Geometry { packs, split }
    }

    /// Every object the packs below the split hold: the include half of what
    /// `cmd_repack()` writes to `pack-objects --stdin-packs`
    /// (builtin/repack.c:960-961).
    fn rolled_up_objects(&self, existing: &[pack::index::File]) -> Vec<ObjectId> {
        self.packs[..self.split]
            .iter()
            .flat_map(|&i| existing[i].iter().map(|e| e.oid))
            .collect()
    }

    /// Every object the packs at or above the split hold: the `^` half of the
    /// same list (builtin/repack.c:962-963), which `pack-objects` turns into
    /// kept-in-core packs so their objects stay out of the new pack.
    fn surviving_objects(&self, existing: &[pack::index::File]) -> HashSet<ObjectId> {
        self.packs[self.split..]
            .iter()
            .flat_map(|&i| existing[i].iter().map(|e| e.oid))
            .collect()
    }

    /// The index paths `-d` may remove: the rolled-up packs, and only those.
    fn rolled_up_paths(&self, existing: &[pack::index::File], pack_dir: &Path) -> Vec<PathBuf> {
        self.packs[..self.split]
            .iter()
            .map(|&i| existing[i].path().to_path_buf())
            .filter(|p| p.parent() == Some(pack_dir))
            .collect()
    }
}

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

/// Validate the four options repack hands to `pack-objects` verbatim, emitting
/// the child's parse-options diagnostic for the first bad one.
///
/// The order is `prepare_pack_objects()`'s (`repack.c:17-24`) — window,
/// window-memory, depth, threads — not argv's, because the child sees them in
/// the order repack pushes them: `--threads=x --window=y` reports `window`.
/// `--window`, `--depth` and `--threads` are `OPT_INTEGER` in `pack-objects`
/// (negatives and all), `--window-memory` is unsigned.
fn forwarded_value_check(st: &State) -> Option<ExitCode> {
    let forwarded: [(&str, &Option<String>, Kind); 4] = [
        ("window", &st.window, Kind::Int),
        ("window-memory", &st.window_memory, Kind::Magnitude),
        ("depth", &st.depth, Kind::Int),
        ("threads", &st.threads, Kind::Int),
    ];
    for (name, value, kind) in forwarded {
        let Some(v) = value.as_deref() else { continue };
        let label = format!("option `{name}'");
        let failure = match kind {
            Kind::Magnitude => magnitude_value(&label, v),
            _ => int_value(&label, v).err(),
        };
        if failure.is_some() {
            return failure;
        }
    }
    None
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
        "cruft-expiration" => st.cruft_expiration = value.filter(|_| on).map(str::to_string),
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
        "geometric" => {
            st.geometric = on;
            st.geometric_factor = match on {
                true => value.and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0),
                false => 0.0,
            };
        }
        "filter" => {
            st.filter = on;
            st.filter_spec = if on { value.map(str::to_string) } else { None };
        }
        "filter-to" => {
            st.filter_to = on;
            st.filter_to_prefix = if on { value.map(str::to_string) } else { None };
        }
        // Kept verbatim, exactly as git's `OPT_STRING` does; `--no-<name>` sets
        // the pointer back to NULL and so drops the option entirely.
        "window" => st.window = value.filter(|_| on).map(str::to_string),
        "window-memory" => st.window_memory = value.filter(|_| on).map(str::to_string),
        "depth" => st.depth = value.filter(|_| on).map(str::to_string),
        "threads" => st.threads = value.filter(|_| on).map(str::to_string),
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
fn preflight(st: &State, midx: &MidxConfig) -> Option<ExitCode> {
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

    // `builtin/repack.c:291-296`, which sits between the bitmap-conflict die
    // above (:274) and the `--filter-to` die below (:407-408) — pinned against
    // git 2.55.0 by running each pair: `-c repack.midxSplitFactor=1 repack -b`
    // reports the bitmap conflict, `… repack --filter-to=x` reports the split
    // factor. git names the *option* both keys shadow, not the config key, and
    // prints the value as the `int` it read.
    //
    // The check is unconditional in git: it fires with no `--write-midx` on the
    // line and with nothing to pack, which is why it is here rather than beside
    // the MIDX write.
    if midx.split_factor < 2 {
        return Some(fatal(&format!(
            "invalid value for --midx-split-factor: {}",
            midx.split_factor
        )));
    }
    if midx.new_layer_threshold < 1 {
        return Some(fatal(&format!(
            "invalid value for --midx-new-layer-threshold: {}",
            midx.new_layer_threshold
        )));
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
