//! `git pack-objects` — create a packed archive of objects.
//!
//! # Pack layout matches git's
//!
//! The pack written here is delta-compressed by
//! [`gix_pack::data::output::delta`], which is git's own delta machinery ported
//! from `diff-delta.c` and `builtin/pack-objects.c`: the same Rabin-fingerprint
//! encoder, the same sliding window sorted by type, name-hash and size, and the
//! same `--window`/`--depth`/`--window-memory` heuristics deciding which pairs
//! are worth deltifying. `--delta-base-offset` selects `OBJ_OFS_DELTA` over
//! `OBJ_REF_DELTA` exactly as it does in git.
//!
//! Objects are enumerated in `traverse_commit_list()`'s order (see
//! [`traverse_commit_list`]) and written in `compute_write_order()`'s (see
//! [`compute_write_order`]), so the entry sequence, the type and size of every
//! entry, and which entry deltifies against which at what depth all agree with
//! stock git — checked entry-for-entry with `verify-pack -v` on the `linear`,
//! `branched` and `merged` harness fixtures, including the tag-tip regrouping
//! `--include-tag` exercises.
//!
//! **The deflate stream is the one part not written here**, so it is worth
//! saying what has actually been observed of it rather than assuming. The
//! vendored `gix-zlib` compresses through `zlib-rs` where git links zlib, and
//! at the pack's default level the two have matched byte-for-byte on every
//! corpus checked so far: the `packed` fixture's 31-object pack, a 402-object
//! pack of small text blobs, and a 102 KiB pack mixing prose, C source and
//! incompressible random bytes each came out with the same length and the same
//! `<base-name>-<hash>.{pack,idx,rev}` name from both implementations. That is
//! agreement on the inputs measured, not a guarantee for every input: the two
//! are different implementations and nothing here pins them together. A
//! divergence would show up as a pack that verifies but is named differently,
//! which is the shape to look for. `git verify-pack`, `git index-pack --verify`
//! and `git unpack-objects` all accept what is written either way.
//!
//! Deltas *are* reused from an existing pack, as `check_object()` reuses them:
//! see [`reused_delta_bases`]. What is reused is the delta *relationship* — the
//! base, and therefore the chain — and not the stored bytes, so
//! `write_reuse_object()`'s byte copy still has no counterpart and the delta is
//! recomputed from base and target at write time. Since it is recomputed by the
//! same encoder that produced it, the instruction stream comes out the same.
//!
//! One layout input is still absent, and is not reachable from the default
//! configuration: delta islands do not split the write order into layers — with
//! islands off `oe_layer()` is 0 throughout and there is exactly one layer,
//! which is what [`compute_write_order`] reproduces.
//!
//! The knobs with nothing to steer are the ones tied to substrate that is still
//! missing: `--no-reuse-object` (the stored bytes are never copied, so there is
//! no object reuse to switch off), `--delta-islands`,
//! `--name-hash-version`, `--path-walk`, `--sparse` and `--shallow`. `--thin`
//! is likewise accepted without effect, a thin pack needing bases outside the
//! pack. `--write-bitmap-index` *does* write a `.bitmap` — see [`bitmap_file`] —
//! unless the pack is missing part of the closure a bitmap must cover, in which
//! case it warns as git does and writes none.
//!
//! # What is reproduced exactly
//!
//! * the object *set*: which objects end up in the pack, for `--all`,
//!   `--reflog`, `--indexed-objects`, `--revs`, `--stdin-packs`, `--unpacked`,
//!   `--cruft`, a bare object list on stdin, and every combination of those
//!   (see [`collect_counts`]). A bare object list packs exactly the ids it
//!   names — `add_object_entry()` adds one object and no traversal follows — and
//!   an id naming nothing is carried to the write and dies there, as git's does
//! * the object *order*, both the enumeration order and the write order
//! * the *artifacts*: `<base>-<hash>.pack`, `.idx` (v1 and v2), `.rev`, and
//!   `.mtimes` for `--cruft` — the files whose presence and count callers and
//!   state probes observe
//! * the exit codes and diagnostics, including the `error:`/`fatal:` pair and
//!   exit 128 git emits when the output path cannot be written
//! * `-h` → git's 4170-byte usage block on stdout, exit 129
//! * git's parse-options behaviour for every option in the table, including
//!   unambiguous long-option abbreviation (`--stdi` → `--stdin-packs`), `--no-`
//!   negations, `=value` vs. separate-argv values, and `-q`/`-h`
//! * the parse-options diagnostics, each byte-for-byte: `unknown option`,
//!   `unknown switch`, `ambiguous option`, `takes no value`, `requires a value`,
//!   and the integer/magnitude value-type messages
//! * the value-callback `fatal:`s git raises *during* parsing, in argv order:
//!   `--index-version` (git's `strtoul` grammar, including the `,<offset>` tail
//!   and the `off32_limit` sign check), `--missing`, `--stdin-packs=<mode>`, and
//!   `--filter` (git's full `gently_parse_list_objects_filter` grammar:
//!   `blob:none`, `blob:limit=<n>`, `tree:<depth>`, `sparse:oid=`, the dropped
//!   `sparse:path=`, `object:type=<t>`, and recursive `combine:` with its
//!   percent-decode and reserved-character checks)
//! * the usage-on-no-output rule (`pack_to_stdout != !base_name`, plus a second
//!   positional) and every post-parse `fatal:` git emits before it touches the
//!   object database, in git's own order: bad compression level, `--thin`
//!   without `--stdout`, the `--keep-unreachable`/`--unpack-unreachable`
//!   conflict, the two `cannot use internal rev list with ...` diagnostics, the
//!   `--stdin-packs`/`--cruft` conflict, `--max-pack-size` with `--stdout`, and
//!   `--name-hash-version`
//! * the empty object set: no source named and nothing on stdin yields git's
//!   12-byte header plus trailing checksum, which *is* byte-identical because
//!   there is no entry to order and no delta to compute. `--non-empty`
//!   suppresses it entirely (no output, exit 0).
//!
//! (all checked against git 2.55.0.)
//!
//! # Remaining gaps
//!
//! Stated so this doc claims no more than the code does:
//!
//!   * `--filter=<spec>` implements `blob:none`, `blob:limit=<n>`, `tree:<n>`
//!     and `object:type=<t>`; `sparse:oid=` and `combine:` are accepted and
//!     ignored, as no sparse-spec reader exists in the vendored crates.
//!   * `--max-pack-size` does not split the output across several packs; one
//!     pack is always written. Splitting only ever triggers on repositories far
//!     larger than the limit, and the split boundary is a function of the delta
//!     encoding this module does not have.
//!   * the compression *diagnostic* (`bad pack compression level <n>`) fires
//!     only for a value given on the command line; an out-of-range
//!     `pack.compression`/`core.compression` is ignored rather than fatal.
//!   * `pack.packSizeLimit` supplies `--max-pack-size`'s default, is validated
//!     the moment the config is read (ahead of parse-options, so a bad value is
//!     fatal even for `-h`), and warns below git's 1 MiB floor — but, like
//!     `--max-pack-size` itself, does not split the output.
//!   * `--missing=allow-promisor` does not additionally imply
//!     `--exclude-promisor-objects` handling.
//!   * `--include-tag` adds no tags beyond those the object set already names.
//!   * `--cruft-expiration=<time>` is parsed but does not filter by mtime; every
//!     cruft object is written with its current mtime.
//!
//! # Configuration honoured
//!
//!   * `pack.indexVersion`, `pack.writeReverseIndex` — the `.idx` format and
//!     whether a `.rev` accompanies the pack.
//!   * `pack.packSizeLimit` — as above.
//!   * `pack.window`, `pack.depth`, `pack.windowMemory`, `pack.deltaCacheSize`,
//!     `pack.deltaCacheLimit`, `pack.threads` — the delta search, each
//!     overridable by its command-line counterpart. A window or depth of zero
//!     turns the search off and every object is stored whole.
//!   * `pack.compression`, falling back to `core.compression` — the zlib level
//!     every entry is deflated at, shadowed by `--compression`.
//!   * `core.fsync` / `core.fsyncMethod` — the pack is hardened when the `pack`
//!     component is in the set, its `.idx`/`.rev`/`.mtimes` when `pack-metadata`
//!     is. `core.fsyncObjectFiles` is read for its deprecation warning and its
//!     effect on the `loose-object` component.
//!   * `pack.usePathWalk` — the default for `--path-walk`, resolved by
//!     [`resolve_path_walk`] exactly as `builtin/pack-objects.c:5188-5197` does.
//!     The walk itself is not implemented, but the *diagnostics* the flag gates
//!     are: `--path-walk` (however it was turned on) together with
//!     `--delta-islands`, or with a `--filter` carrying a non-zero `tree:<n>`,
//!     warns `cannot use <option> with --path-walk` and switches the walk back
//!     off — see [`path_walk_filter_compatible`].
//!   * `pack.useSparse`, `pack.readReverseIndex`,
//!     `pack.useBitmapBoundaryTraversal` — read through
//!     [`crate::repo_settings::RepoSettings`], which git resolves in
//!     `prepare_repo_settings()` before `git_pack_config()` and therefore before
//!     parse-options; a value that is not a boolean is fatal here just as it is
//!     in git, `-h` included. None of the three steers anything in this port;
//!     each field on `RepoSettings` says what it would have steered.
//!   * `pack.useBitmaps`, `pack.allowPackReuse` — read in
//!     [`PackConfig::load`], git's `git_pack_config()`. Neither reading an
//!     existing `.bitmap` nor verbatim pack reuse exists here, so both are
//!     validated and reported and then unused; `pack.allowPackReuse` has its own
//!     tri-state grammar and its own `fatal:`, which is why it is not a plain
//!     boolean.

use anyhow::Result;
use gix::hash::ObjectId;
use gix::odb::pack;
use gix::odb::pack::FindExt;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::ExitCode;

/// Stock git's `pack-objects` usage block, byte-for-byte (4170 bytes, git
/// 2.55.0), including the trailing blank line. Printed on `-h` (stdout), after
/// the `unknown option` / `unknown switch` diagnostics (stderr), on stdout after
/// the `ambiguous option` diagnostic, and on stderr on its own when neither an
/// output file nor `--stdout` was given.
const USAGE: &str = r#"usage: git pack-objects [-q | --progress | --all-progress] [--all-progress-implied]
                        [--no-reuse-delta] [--delta-base-offset] [--non-empty]
                        [--local] [--incremental] [--window=<n>] [--depth=<n>]
                        [--revs [--unpacked | --all]] [--keep-pack=<pack-name>]
                        [--cruft] [--cruft-expiration=<time>]
                        [--stdout [--filter=<filter-spec>] | <base-name>]
                        [--shallow] [--keep-true-parents] [--[no-]sparse]
                        [--name-hash-version=<n>] [--path-walk] < <object-list>

    -q, --[no-]quiet      do not show progress meter
    --[no-]progress       show progress meter
    --[no-]all-progress   show progress meter during object writing phase
    --[no-]all-progress-implied
                          similar to --all-progress when progress meter is shown
    --index-version <version>[,<offset>]
                          write the pack index file in the specified idx format version
    --max-pack-size <n>   maximum size of each output pack file
    --[no-]local          ignore borrowed objects from alternate object store
    --[no-]incremental    ignore packed objects
    --[no-]window <n>     limit pack window by objects
    --window-memory <n>   limit pack window by memory in addition to object limit
    --[no-]depth <n>      maximum length of delta chain allowed in the resulting pack
    --[no-]reuse-delta    reuse existing deltas
    --[no-]reuse-object   reuse existing objects
    --[no-]delta-base-offset
                          use OFS_DELTA objects
    --[no-]threads <n>    use threads when searching for best delta matches
    --[no-]non-empty      do not create an empty pack output
    --[no-]revs           read revision arguments from standard input
    --unpacked            limit the objects to those that are not yet packed
    --all                 include objects reachable from any reference
    --reflog              include objects referred by reflog entries
    --indexed-objects     include objects referred to by the index
    --[no-]stdin-packs[=<mode>]
                          read packs from stdin
    --[no-]stdout         output pack to stdout
    --[no-]include-tag    include tag objects that refer to objects to be packed
    --[no-]keep-unreachable
                          keep unreachable objects
    --[no-]pack-loose-unreachable
                          pack loose unreachable objects
    --[no-]unpack-unreachable[=<time>]
                          unpack unreachable objects newer than <time>
    --[no-]cruft          create a cruft pack
    --[no-]cruft-expiration[=<time>]
                          expire cruft objects older than <time>
    --[no-]sparse         use the sparse reachability algorithm
    --[no-]thin           create thin packs
    --[no-]path-walk      use the path-walk API to walk objects when possible
    --[no-]shallow        create packs suitable for shallow fetches
    --[no-]honor-pack-keep
                          ignore packs that have companion .keep file
    --[no-]keep-pack <name>
                          ignore this pack
    --[no-]compression <n>
                          pack compression level
    --[no-]keep-true-parents
                          do not hide commits by grafts
    --[no-]use-bitmap-index
                          use a bitmap index if available to speed up counting objects
    --[no-]write-bitmap-index
                          write a bitmap index together with the pack index
    --[no-]filter <args>  object filtering
    --missing <action>    handling for missing objects
    --[no-]exclude-promisor-objects
                          do not pack objects in promisor packfiles
    --[no-]exclude-promisor-objects-best-effort
                          implies --missing=allow-any
    --[no-]delta-islands  respect islands during delta compression
    --[no-]uri-protocol <protocol>
                          exclude any configured uploadpack.blobpackfileuri with this protocol
    --[no-]name-hash-version <n>
                          use the specified name-hash function to group similar objects

"#;

/// `USAGE_FULL`: the same block with the one `PARSE_OPT_HIDDEN` entry left in
/// (4265 bytes, git 2.55.0). `--write-bitmap-index-quiet` is hidden from `-h`
/// and shown by `--help-all`; the flag is otherwise a normal table entry, so it
/// resolves — and makes `--write-bit` ambiguous — either way.
const USAGE_ALL: &str = r#"usage: git pack-objects [-q | --progress | --all-progress] [--all-progress-implied]
                        [--no-reuse-delta] [--delta-base-offset] [--non-empty]
                        [--local] [--incremental] [--window=<n>] [--depth=<n>]
                        [--revs [--unpacked | --all]] [--keep-pack=<pack-name>]
                        [--cruft] [--cruft-expiration=<time>]
                        [--stdout [--filter=<filter-spec>] | <base-name>]
                        [--shallow] [--keep-true-parents] [--[no-]sparse]
                        [--name-hash-version=<n>] [--path-walk] < <object-list>

    -q, --[no-]quiet      do not show progress meter
    --[no-]progress       show progress meter
    --[no-]all-progress   show progress meter during object writing phase
    --[no-]all-progress-implied
                          similar to --all-progress when progress meter is shown
    --index-version <version>[,<offset>]
                          write the pack index file in the specified idx format version
    --max-pack-size <n>   maximum size of each output pack file
    --[no-]local          ignore borrowed objects from alternate object store
    --[no-]incremental    ignore packed objects
    --[no-]window <n>     limit pack window by objects
    --window-memory <n>   limit pack window by memory in addition to object limit
    --[no-]depth <n>      maximum length of delta chain allowed in the resulting pack
    --[no-]reuse-delta    reuse existing deltas
    --[no-]reuse-object   reuse existing objects
    --[no-]delta-base-offset
                          use OFS_DELTA objects
    --[no-]threads <n>    use threads when searching for best delta matches
    --[no-]non-empty      do not create an empty pack output
    --[no-]revs           read revision arguments from standard input
    --unpacked            limit the objects to those that are not yet packed
    --all                 include objects reachable from any reference
    --reflog              include objects referred by reflog entries
    --indexed-objects     include objects referred to by the index
    --[no-]stdin-packs[=<mode>]
                          read packs from stdin
    --[no-]stdout         output pack to stdout
    --[no-]include-tag    include tag objects that refer to objects to be packed
    --[no-]keep-unreachable
                          keep unreachable objects
    --[no-]pack-loose-unreachable
                          pack loose unreachable objects
    --[no-]unpack-unreachable[=<time>]
                          unpack unreachable objects newer than <time>
    --[no-]cruft          create a cruft pack
    --[no-]cruft-expiration[=<time>]
                          expire cruft objects older than <time>
    --[no-]sparse         use the sparse reachability algorithm
    --[no-]thin           create thin packs
    --[no-]path-walk      use the path-walk API to walk objects when possible
    --[no-]shallow        create packs suitable for shallow fetches
    --[no-]honor-pack-keep
                          ignore packs that have companion .keep file
    --[no-]keep-pack <name>
                          ignore this pack
    --[no-]compression <n>
                          pack compression level
    --[no-]keep-true-parents
                          do not hide commits by grafts
    --[no-]use-bitmap-index
                          use a bitmap index if available to speed up counting objects
    --[no-]write-bitmap-index
                          write a bitmap index together with the pack index
    --[no-]write-bitmap-index-quiet
                          write a bitmap index if possible
    --[no-]filter <args>  object filtering
    --missing <action>    handling for missing objects
    --[no-]exclude-promisor-objects
                          do not pack objects in promisor packfiles
    --[no-]exclude-promisor-objects-best-effort
                          implies --missing=allow-any
    --[no-]delta-islands  respect islands during delta compression
    --[no-]uri-protocol <protocol>
                          exclude any configured uploadpack.blobpackfileuri with this protocol
    --[no-]name-hash-version <n>
                          use the specified name-hash function to group similar objects

"#;

/// How an option consumes (and validates) its value.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `OPT_BOOL`/`OPT_SET_INT`: no value; `--opt=x` is an error.
    Bool,
    /// `OPT_INTEGER`: signed, optional single `k`/`m`/`g` suffix.
    Int,
    /// `OPT_MAGNITUDE`: as `Int` but non-negative.
    Magnitude,
    /// `OPT_STRING`/`OPT_CALLBACK`: any value, from `=` or the next argv entry.
    Str,
    /// `PARSE_OPT_OPTARG`: value only ever comes from `=`.
    OptStr,
}

/// One entry of git's `pack-objects` option table.
struct OptDef {
    long: &'static str,
    kind: Kind,
    /// Whether `--no-<long>` is accepted (`--[no-]` in the usage block).
    negatable: bool,
}

/// The long-option table **in git's declaration order**, which is the order the
/// usage block lists them in. The order is load-bearing: parse-options resolves
/// an ambiguous abbreviation by reporting the last two matches it walked past,
/// so reordering this array changes the text of `ambiguous option` diagnostics.
const OPTS: &[OptDef] = &[
    OptDef { long: "quiet", kind: Kind::Bool, negatable: true },
    OptDef { long: "progress", kind: Kind::Bool, negatable: true },
    OptDef { long: "all-progress", kind: Kind::Bool, negatable: true },
    OptDef { long: "all-progress-implied", kind: Kind::Bool, negatable: true },
    OptDef { long: "index-version", kind: Kind::Str, negatable: false },
    OptDef { long: "max-pack-size", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "local", kind: Kind::Bool, negatable: true },
    OptDef { long: "incremental", kind: Kind::Bool, negatable: true },
    OptDef { long: "window", kind: Kind::Int, negatable: true },
    OptDef { long: "window-memory", kind: Kind::Magnitude, negatable: false },
    OptDef { long: "depth", kind: Kind::Int, negatable: true },
    OptDef { long: "reuse-delta", kind: Kind::Bool, negatable: true },
    OptDef { long: "reuse-object", kind: Kind::Bool, negatable: true },
    OptDef { long: "delta-base-offset", kind: Kind::Bool, negatable: true },
    OptDef { long: "threads", kind: Kind::Int, negatable: true },
    OptDef { long: "non-empty", kind: Kind::Bool, negatable: true },
    OptDef { long: "revs", kind: Kind::Bool, negatable: true },
    OptDef { long: "unpacked", kind: Kind::Bool, negatable: false },
    OptDef { long: "all", kind: Kind::Bool, negatable: false },
    OptDef { long: "reflog", kind: Kind::Bool, negatable: false },
    OptDef { long: "indexed-objects", kind: Kind::Bool, negatable: false },
    OptDef { long: "stdin-packs", kind: Kind::OptStr, negatable: true },
    OptDef { long: "stdout", kind: Kind::Bool, negatable: true },
    OptDef { long: "include-tag", kind: Kind::Bool, negatable: true },
    OptDef { long: "keep-unreachable", kind: Kind::Bool, negatable: true },
    OptDef { long: "pack-loose-unreachable", kind: Kind::Bool, negatable: true },
    OptDef { long: "unpack-unreachable", kind: Kind::OptStr, negatable: true },
    OptDef { long: "cruft", kind: Kind::Bool, negatable: true },
    OptDef { long: "cruft-expiration", kind: Kind::OptStr, negatable: true },
    OptDef { long: "sparse", kind: Kind::Bool, negatable: true },
    OptDef { long: "thin", kind: Kind::Bool, negatable: true },
    OptDef { long: "path-walk", kind: Kind::Bool, negatable: true },
    OptDef { long: "shallow", kind: Kind::Bool, negatable: true },
    OptDef { long: "honor-pack-keep", kind: Kind::Bool, negatable: true },
    OptDef { long: "keep-pack", kind: Kind::Str, negatable: true },
    OptDef { long: "compression", kind: Kind::Int, negatable: true },
    OptDef { long: "keep-true-parents", kind: Kind::Bool, negatable: true },
    OptDef { long: "use-bitmap-index", kind: Kind::Bool, negatable: true },
    OptDef { long: "write-bitmap-index", kind: Kind::Bool, negatable: true },
    // `PARSE_OPT_HIDDEN` (pack-objects.c:5134-5137): absent from `-h`, present in
    // `--help-all`, and a first-class entry for resolution either way — which is
    // what makes `--write-bit` ambiguous with `--write-bitmap-index`.
    OptDef { long: "write-bitmap-index-quiet", kind: Kind::Bool, negatable: true },
    OptDef { long: "filter", kind: Kind::Str, negatable: true },
    OptDef { long: "missing", kind: Kind::Str, negatable: false },
    OptDef { long: "exclude-promisor-objects", kind: Kind::Bool, negatable: true },
    OptDef { long: "exclude-promisor-objects-best-effort", kind: Kind::Bool, negatable: true },
    OptDef { long: "delta-islands", kind: Kind::Bool, negatable: true },
    OptDef { long: "uri-protocol", kind: Kind::Str, negatable: true },
    OptDef { long: "name-hash-version", kind: Kind::Int, negatable: true },
];

/// [`OPTS`] projected into the shape the shared `parse_long_opt()` port reads
/// (`porcelain::resolve_long`). Derived rather than written out a second time,
/// so the two can never drift and the indices stay interchangeable: [`OPTS`]
/// says what a value does once the name is resolved, this says what the
/// resolver itself reads while resolving it.
const LONG_OPTS: &[super::LongOpt] = &{
    let mut out = [super::LongOpt { name: "", neg: false, arg: super::Arg::None }; OPTS.len()];
    let mut i = 0;
    while i < OPTS.len() {
        let def = &OPTS[i];
        out[i] = super::LongOpt {
            name: def.long,
            neg: def.negatable,
            arg: match def.kind {
                Kind::Bool => super::Arg::None,
                Kind::OptStr => super::Arg::Optional,
                _ => super::Arg::Required,
            },
        };
        i += 1;
    }
    out
};

/// The only `--missing=<action>` values git accepts.
const MISSING_ACTIONS: [&str; 3] = ["error", "allow-any", "allow-promisor"];

/// The only `--stdin-packs=<mode>` values; a bare `--stdin-packs` is the empty mode.
const STDIN_PACKS_MODES: [&str; 2] = ["", "follow"];

/// The flag state git derives while parsing, i.e. everything the post-parse
/// checks look at. Options that no check consults are accepted and dropped,
/// since the command bails before they could matter.
#[derive(Default)]
struct State {
    stdout: bool,
    thin: bool,
    cruft: bool,
    stdin_packs: bool,
    unpacked: bool,
    keep_unreachable: bool,
    unpack_unreachable: bool,
    non_empty: bool,
    /// The three options that name a source of objects all by themselves.
    all: bool,
    reflog: bool,
    indexed_objects: bool,
    /// `--revs` and the other options that turn on git's internal rev list
    /// without `--unpacked`'s stdin-packs exemption.
    internal_rev_list: bool,
    /// `--exclude-promisor-objects` turns the internal rev list on *after* the
    /// `--stdin-packs` check has already run, so it feeds only the `--cruft`
    /// one; `--exclude-promisor-objects-best-effort` feeds both. Kept apart
    /// from `internal_rev_list` because both are assignments, not accumulations:
    /// their `--no-` forms switch them back off.
    exclude_promisor: bool,
    exclude_promisor_best_effort: bool,
    /// `--compression=<n>`, as the integer git parsed.
    compression: Option<i64>,
    /// `--name-hash-version=<n>`, as the integer git parsed.
    name_hash_version: Option<i64>,
    /// `--max-pack-size=<n>`, as the magnitude git parsed. Zero counts as unset,
    /// which is why this is the number and not a flag.
    max_pack_size: Option<i64>,
    /// `--index-version=<v>[,<offset>]`, just the `<v>`; `None` falls back to
    /// `pack.indexVersion` and then to 2.
    index_version: Option<u64>,
    /// `--revs`: stdin carries rev-list arguments rather than an object list.
    revs: bool,
    /// `--incremental`: leave out objects an existing pack already holds.
    incremental: bool,
    /// `--window=<n>`: how many objects the delta search compares each object
    /// against. Overrides `pack.window`.
    window: Option<i64>,
    /// `--depth=<n>`: the longest delta chain to produce. Overrides `pack.depth`.
    depth: Option<i64>,
    /// `--window-memory=<n>`: the delta window's memory ceiling in bytes.
    /// Overrides `pack.windowMemory`.
    window_memory: Option<u64>,
    /// `--threads=<n>`: how many threads search for deltas. Overrides
    /// `pack.threads`; zero means one per logical core.
    threads: Option<i64>,
    /// `--delta-base-offset` / `--no-delta-base-offset`: whether deltas name
    /// their base by pack offset (`OBJ_OFS_DELTA`) or by object id
    /// (`OBJ_REF_DELTA`). Unset leaves git's default, which is by object id.
    delta_base_offset: Option<bool>,
    /// `--reuse-delta` / `--no-reuse-delta`: whether a delta already on disk is
    /// kept rather than searched for again. Unset leaves git's default, on.
    reuse_delta: Option<bool>,
    /// `--filter=<spec>`, as given; see [`apply_filter`].
    filter: Option<String>,
    /// `--write-bitmap-index`: write a `.bitmap` beside the `.idx`.
    write_bitmap_index: bool,
    /// `--delta-islands`: honour `pack.island` in the delta search.
    delta_islands: bool,
    /// `--sparse` / `--no-sparse`. `None` leaves git's `sparse` at the value
    /// `prepare_repo_settings()` seeded it with (`pack.useSparse`, default on).
    sparse: Option<bool>,
    /// `--path-walk` / `--no-path-walk`. `None` is git's `path_walk = -1`, which
    /// [`resolve_path_walk`] then resolves against `pack.usePathWalk`.
    path_walk: Option<bool>,
    /// `--use-bitmap-index` / `--no-use-bitmap-index`. `None` is git's
    /// `use_bitmap_index = -1`; `pack.useBitmaps` only fills it in much later
    /// (`builtin/pack-objects.c:5334`), which is why the `--path-walk` default
    /// at :5189 sees the flag alone.
    use_bitmap_index: Option<bool>,
    /// Whether the phase meters and the end-of-run summary go to stderr. Seeded
    /// from `isatty(2)` in [`parse`], then `-q` and `--progress` (and
    /// `--all-progress`) override it, last one wins.
    progress: bool,
    /// Non-option arguments; at most one (the base name) is legal.
    positionals: Vec<String>,
}

impl State {
    /// The internal-rev-list flag as the `--stdin-packs` check sees it.
    fn rev_list_at_stdin_packs_check(&self) -> bool {
        self.internal_rev_list || self.exclude_promisor_best_effort
    }

    /// The same flag as the later `--cruft` check sees it, by which point
    /// `--exclude-promisor-objects` has set it too.
    fn rev_list_at_cruft_check(&self) -> bool {
        self.rev_list_at_stdin_packs_check() || self.exclude_promisor
    }
}

/// The outcome of parsing: either a fully-formed request, or a diagnostic that
/// has already decided the exit code.
enum Parsed {
    Ok(State),
    Exit(ExitCode),
}

/// `enum pack_reuse` (`builtin/pack-objects.c:246-251`): how much of an existing
/// pack `pack-objects` may copy verbatim into the new one.
///
/// Nothing here reuses pack bytes — every entry is re-encoded from the object it
/// names — so the value only ever records what git *would* have done. It exists
/// as its own type rather than a boolean because `pack.allowPackReuse` is a
/// tri-state whose invalid values get their own diagnostic.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum PackReuse {
    /// `NO_PACK_REUSE` — the value a false boolean selects.
    None,
    /// `SINGLE_PACK_REUSE` — git's default, and what a true boolean selects.
    Single,
    /// `MULTI_PACK_REUSE` — `multi`, or `feature.experimental` by way of
    /// `r->settings.pack_use_multi_pack_reuse`.
    Multi,
}

/// The `git_pack_config()` keys this port reads that are not already resolved
/// elsewhere — `pack.useBitmaps` and `pack.allowPackReuse`.
///
/// git reads them in `repo_config(the_repository, git_pack_config, NULL)`
/// (`builtin/pack-objects.c:5173`), which runs *before* `parse_options`, so a
/// value git cannot read is fatal ahead of every parse diagnostic including
/// `-h`. Verified against git 2.55.0 inside a repository:
///
/// ```text
/// $ git -c pack.useBitmaps=bogus pack-objects -h
/// fatal: bad boolean config value 'bogus' for 'pack.usebitmaps'
/// $ git -c pack.allowPackReuse=bogus pack-objects -h
/// fatal: invalid pack.allowPackReuse value: 'bogus'
/// ```
#[derive(Copy, Clone, Debug)]
pub(crate) struct PackConfig {
    /// `use_bitmap_index_default` (`builtin/pack-objects.c:244`, set at :3709),
    /// the fallback for `--use-bitmap-index`. On in git.
    ///
    /// It only ever decides whether an existing `.bitmap` is *read* to shortcut
    /// counting; `gix-pack` has no bitmap reader, so the object list is walked
    /// either way and the pack is the same either way.
    pub(crate) use_bitmap_index_default: bool,
    /// `allow_pack_reuse` (`builtin/pack-objects.c:251`), seeded from
    /// `r->settings.pack_use_multi_pack_reuse` at :5167 and then overridden by
    /// `pack.allowPackReuse` at :3729-3743.
    pub(crate) allow_pack_reuse: PackReuse,
}

impl PackConfig {
    /// `git_pack_config()` for the two keys above, with `settings` supplying the
    /// `MULTI_PACK_REUSE` seed `prepare_repo_settings()` already resolved.
    ///
    /// `Err` carries the exact line git's `die()` prints, minus the `fatal: `
    /// prefix.
    pub(crate) fn load(
        repo: &gix::Repository,
        settings: &crate::repo_settings::RepoSettings,
    ) -> Result<Self, String> {
        // builtin/pack-objects.c:5167-5168, which runs before git_pack_config
        // and is therefore the value the config overrides rather than the other
        // way round.
        let mut allow_pack_reuse = if settings.pack_use_multi_pack_reuse {
            PackReuse::Multi
        } else {
            PackReuse::Single
        };

        // builtin/pack-objects.c:3729-3743. The boolean half is
        // `git_parse_maybe_bool_text`, the *word*-only grammar — not
        // `git_parse_maybe_bool` — so `true`/`yes`/`on`/`false`/`no`/`off` and
        // the empty string are booleans while the digits are not: git 2.55.0
        // rejects `pack.allowPackReuse=1` and `=0` with this key's own `die()`,
        // which is neither a `bad boolean config value` nor a numeric
        // diagnostic. The two mode words are compared with `strcasecmp`.
        match crate::config::last_value_implicit(repo, "pack.allowpackreuse") {
            None => {}
            // A key written with no `=` at all: `git_parse_maybe_bool_text(NULL)`
            // answers 1 before it looks at any text, so it is `SINGLE_PACK_REUSE`.
            Some(None) => allow_pack_reuse = PackReuse::Single,
            Some(Some(raw)) => {
                allow_pack_reuse = match crate::optint::maybe_bool_text(&raw) {
                    Some(true) => PackReuse::Single,
                    Some(false) => PackReuse::None,
                    None if raw.eq_ignore_ascii_case("single") => PackReuse::Single,
                    None if raw.eq_ignore_ascii_case("multi") => PackReuse::Multi,
                    // The one diagnostic here that names the key in git's
                    // camelCase: it is a `die()` with a literal
                    // `pack.allowPackReuse` in the format string, not the
                    // lowercased `var` the config reader hands the callback.
                    None => return Err(format!("invalid pack.allowPackReuse value: '{raw}'")),
                };
            }
        }

        // builtin/pack-objects.c:3725-3728, a plain `git_config_bool`.
        let use_bitmap_index_default =
            crate::repo_settings::config_bool_strict(repo, "pack.usebitmaps")?.unwrap_or(true);

        Ok(PackConfig {
            use_bitmap_index_default,
            allow_pack_reuse,
        })
    }
}

/// `path_walk_filter_compatible()` (`path-walk.c:686-689`), which is
/// `prepare_filters(NULL, options)` — the `info == NULL` half of
/// `prepare_filters_one()`.
///
/// With no `path_walk_info` to fill in, every filter form returns 1 except a
/// `tree:<depth>` with a non-zero depth, which `error()`s first and then returns
/// 0; `combine:` recurses and fails on the first sub-filter that does. So the
/// answer is "does this spec contain a non-zero `tree:<n>` anywhere", and the
/// depth that answer found is the one git names.
///
/// `Err(depth)` is git's 0 return, carrying the number for the
/// `error: tree:<n> filter not supported by the path-walk API` line the caller
/// prints. Verified against git 2.55.0: `--filter=tree:0` is compatible,
/// `--filter=tree:1` and `--filter=combine:blob:none+tree:2` are not.
fn path_walk_filter_compatible(spec: &[u8]) -> Result<(), u64> {
    if let Some(rest) = spec.strip_prefix(b"tree:".as_slice()) {
        // The spec reached here only after `gently_parse_filter` accepted it, so
        // the magnitude parses; a `None` cannot occur and is treated as the
        // compatible depth zero rather than invented into a diagnostic.
        let depth = git_parse_ulong(rest).unwrap_or(0);
        if depth != 0 {
            return Err(depth);
        }
        return Ok(());
    }
    if let Some(body) = spec.strip_prefix(b"combine:".as_slice()) {
        // `parse_combine_filter`'s split, minus the validation it already did:
        // percent-decode each segment and recurse, skipping the empty segments a
        // leading or trailing `+` leaves behind.
        for segment in body.split(|&c| c == b'+') {
            if segment.is_empty() {
                continue;
            }
            path_walk_filter_compatible(&url_percent_decode(segment))?;
        }
    }
    Ok(())
}

/// `builtin/pack-objects.c:5188-5197`: the `--path-walk` default, resolved
/// immediately after the no-output usage check and before any of the post-parse
/// `die()`s.
///
/// ```c
/// if (path_walk < 0) {
///         if (use_bitmap_index > 0 || !use_internal_rev_list)
///                 path_walk = 0;
///         else if (the_repository->gitdir &&
///                  the_repository->settings.pack_use_path_walk)
///                 path_walk = 1;
///         else
///                 path_walk = git_env_bool("GIT_TEST_PACK_PATH_WALK", 0);
/// }
/// ```
///
/// `use_bitmap_index` is still `-1` here unless `--use-bitmap-index` was given
/// explicitly — `pack.useBitmaps` is not folded in until line 5334 — so the
/// first arm reads the *flag*, not the config. `GIT_TEST_PACK_PATH_WALK` is
/// git's own test hook and is honoured for the same reason the other
/// `GIT_TEST_*` hooks are.
fn resolve_path_walk(st: &State, settings: Option<&crate::repo_settings::RepoSettings>) -> bool {
    if let Some(explicit) = st.path_walk {
        return explicit;
    }
    if st.use_bitmap_index == Some(true) || !st.internal_rev_list {
        return false;
    }
    match settings {
        Some(s) if s.pack_use_path_walk => true,
        // `the_repository->gitdir` is false outside a repository, which is the
        // `None` case here; both fall through to the environment hook.
        _ => std::env::var("GIT_TEST_PACK_PATH_WALK")
            .ok()
            .and_then(|v| crate::optint::maybe_bool(&v))
            .unwrap_or(false),
    }
}

/// `git pack-objects` — argument validation, pre-flight checks, and the empty
/// pack; a pack with entries in it is not ported.
///
/// Returns 129 with git's own output for `-h`, for every malformed invocation,
/// and when neither `--stdout` nor exactly one base name was given; 128 for the
/// value and option conflicts git rejects before it opens the object database.
/// An invocation that survives both packs nothing when nothing named an object,
/// and otherwise bails, naming the substrate that is missing; see the module
/// documentation for the full list.
pub fn pack_objects(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `pack-objects` does take a
    // positional (the base name), so the leading verb must be dropped rather
    // than counted as one.
    let args = match args.first().map(String::as_str) {
        Some("pack-objects") => &args[1..],
        _ => args,
    };

    // `cmd_pack_objects()` reads configuration twice before `parse_options`, and
    // both reads can be fatal ahead of every parse diagnostic — `-h` included.
    // Verified against git 2.55.0 inside a repo: `pack-objects -h` reports the
    // bad value instead of the usage block for each key below. Outside a
    // repository git never gets that far ("not a git repository" wins), which is
    // why both reads are skipped when discovery fails.
    //
    //   1. `prepare_repo_settings(the_repository)` (:5164) — the `feature.*`
    //      macros, `core.packedGit*`, and the four `pack.*` booleans in
    //      [`crate::repo_settings::RepoSettings`].
    //   2. `repo_config(the_repository, git_pack_config, NULL)` (:5173) —
    //      `pack.packSizeLimit` and [`PackConfig`]'s two keys.
    //
    // The order matters: `pack.useSparse=bogus pack.packSizeLimit=bogus` reports
    // the boolean, because settings are prepared first.
    let mut settings = None;
    let pack_size_limit_cfg = match gix::discover(".") {
        Ok(repo) => {
            let loaded = match crate::repo_settings::RepoSettings::load(&repo) {
                Ok(s) => s,
                Err(message) => {
                    eprintln!("fatal: {message}");
                    return Ok(ExitCode::from(128));
                }
            };
            let limit = match crate::config::config_ulong(&repo, "pack.packSizeLimit") {
                Ok(limit) => limit,
                Err(message) => {
                    eprintln!("fatal: {message}");
                    return Ok(ExitCode::from(128));
                }
            };
            match PackConfig::load(&repo, &loaded) {
                Ok(PackConfig { use_bitmap_index_default, allow_pack_reuse }) => {
                    // git carries these to `:5334` (whether an existing
                    // `.bitmap` shortcuts the object count) and `:4708` (how
                    // much of an existing pack is copied verbatim). Neither
                    // reader nor copier exists here, so the resolved values are
                    // dropped rather than pretended into an effect.
                    let _ = (use_bitmap_index_default, allow_pack_reuse);
                }
                Err(message) => {
                    eprintln!("fatal: {message}");
                    return Ok(ExitCode::from(128));
                }
            }
            settings = Some(loaded);
            limit
        }
        Err(_) => None,
    };

    let state = match parse(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok(state) => state,
    };

    if let Some(code) = preflight(&state, settings.as_ref()) {
        return Ok(code);
    }

    // git's post-parse `pack_size_limit` resolution, in its own order:
    //
    // ```c
    // if (!pack_to_stdout && !pack_size_limit)
    //     pack_size_limit = pack_size_limit_cfg;
    // if (pack_to_stdout && pack_size_limit)          /* handled in preflight() */
    //     die(_("--max-pack-size cannot be used to build a pack for transfer"));
    // if (pack_size_limit && pack_size_limit < 1024*1024) {
    //     warning(_("minimum pack size limit is 1 MiB"));
    //     pack_size_limit = 1024*1024;
    // }
    // ```
    //
    // The config value therefore never reaches the `--stdout` die (it is only
    // adopted when *not* writing to stdout), and an explicit `--max-pack-size`
    // shadows it entirely. This port writes one pack regardless of the limit (see
    // the module docs), so the sub-1-MiB warning is the whole of its observable
    // effect — the same position `gc` puts its `gc.maxCruftSize` warning in.
    let pack_size_limit = match st_max_pack_size(&state) {
        Some(explicit) => Some(explicit),
        None if !state.stdout => pack_size_limit_cfg,
        None => None,
    };
    if pack_size_limit.is_some_and(|n| n > 0 && n < MIN_PACK_SIZE_LIMIT) {
        eprintln!("warning: minimum pack size limit is 1 MiB");
    }

    execute(&state)
}

/// git's 1 MiB floor for `pack_size_limit`: any smaller non-zero limit warns and
/// is then raised to this.
const MIN_PACK_SIZE_LIMIT: u64 = 1024 * 1024;

/// `--max-pack-size` as an unsigned limit, or `None` when it was absent or zero
/// (git's "unset"). The option is an `OPT_MAGNITUDE`, so a negative value cannot
/// reach here.
fn st_max_pack_size(st: &State) -> Option<u64> {
    st.max_pack_size.filter(|n| *n > 0).and_then(|n| u64::try_from(n).ok())
}

/// Run the command proper: work out the object set, encode it into a pack, and
/// write the pack plus its companion files.
///
/// git reaches the object database only after the checks above, so this is also
/// where "not a git repository" is diagnosed.
fn execute(st: &State) -> Result<ExitCode> {
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // git reads stdin in every mode that has one — an object list, a rev-list
    // argument list under `--revs`, or pack names under `--stdin-packs`.
    let mut stdin = Vec::new();
    std::io::stdin().read_to_end(&mut stdin).ok();

    let counts = match collect_counts(&repo, st, &stdin) {
        Ok(counts) => counts,
        Err(message) => {
            // `vreportf()` appends its newline unconditionally, so a message
            // that already ends in one — this one echoes the offending line
            // verbatim — leaves a blank line behind. git's output has it.
            eprint!("fatal: {message}\n");
            return Ok(ExitCode::from(128));
        }
    };
    // git reports the object list it just built as `Enumerating objects`, a
    // count with no total because the traversal is what decides the total.
    {
        let mut enumerating = crate::progress::Meter::unknown("Enumerating objects", st.progress);
        enumerating.advance(counts.len());
        enumerating.done();
    }

    // git skips the pack entirely rather than writing an empty one, and says so
    // by writing nothing at all.
    if counts.is_empty() && st.non_empty {
        return Ok(ExitCode::SUCCESS);
    }

    let delta = match DeltaConfig::from_repo(&repo) {
        Ok(cfg) => cfg.apply(st),
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    };
    let packed = write_pack(&repo, &counts, compression(&repo, st), &delta, st.progress)?;

    if st.stdout {
        let mut out = std::io::stdout().lock();
        out.write_all(&packed.bytes)?;
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    // `preflight` has already established that exactly one positional is present
    // whenever `--stdout` is not.
    let base = st.positionals[0].as_str();
    let hex_id = packed.id.to_string();

    let index_version = st
        .index_version
        .or_else(|| {
            repo.config_snapshot()
                .integer("pack.indexVersion")
                .and_then(|v| u64::try_from(v).ok())
        })
        .unwrap_or(2);
    let write_rev = repo
        .config_snapshot()
        .boolean("pack.writeReverseIndex")
        .unwrap_or(true);

    // Sorted by object id: that is the order the `.idx` stores entries in, and
    // the order `.rev` and `.mtimes` index into.
    let mut by_oid = packed.entries.clone();
    by_oid.sort_unstable_by_key(|a| a.id);

    let kind = repo.object_hash();
    // The pack is the `pack` fsync component; everything written beside it is
    // `pack-metadata`, per `core.fsync`'s component list.
    use crate::config::FsyncComponent::{Pack, PackMetadata};
    let mut files = vec![
        (format!("{base}-{hex_id}.pack"), packed.bytes.clone(), Pack),
        (
            format!("{base}-{hex_id}.idx"),
            index_file(kind, index_version, &packed.id, &by_oid)?,
            PackMetadata,
        ),
    ];
    if write_rev {
        files.push((
            format!("{base}-{hex_id}.rev"),
            reverse_index_file(kind, &packed.id, &by_oid)?,
            PackMetadata,
        ));
    }
    if st.cruft {
        files.push((
            format!("{base}-{hex_id}.mtimes"),
            mtimes_file(&repo, kind, &packed.id, &by_oid)?,
            PackMetadata,
        ));
    }
    if st.write_bitmap_index {
        let mut bitmap = BitmapOptions::from_repo(&repo);
        bitmap.write = true;
        if let Some(bytes) = bitmap_file(&repo, &packed, &bitmap) {
            files.push((format!("{base}-{hex_id}.bitmap"), bytes, PackMetadata));
        }
    }

    let fsync = match crate::config::FsyncPolicy::load(&repo) {
        Ok(policy) => policy,
        Err(message) => {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    };
    // `finish_tmp_packfile()` (pack-write.c) builds every artifact under a
    // temporary name in `objects/pack` and only then renames them into place,
    // pack first. That ordering is observable: a destination directory that does
    // not exist fails at the *pack* rename, after the `.idx`, `.rev` and
    // `.mtimes` temporaries already exist. Only the pack's temporary is
    // registered with git's `tempfile` machinery, so `die()` unlinks that one and
    // leaves `tmp_idx_*` / `tmp_rev_*` / `tmp_mtimes_*` behind in the object
    // store.
    let pack_dir = repo.git_dir().join("objects").join("pack");
    let _ = std::fs::create_dir_all(&pack_dir);
    let mut staged_files: Vec<(std::path::PathBuf, &String, crate::config::FsyncComponent)> =
        Vec::with_capacity(files.len());
    for (path, bytes, component) in &files {
        let tmp = temp_artifact_path(&pack_dir, path, staged_files.len());
        if let Some(code) = write_artifact(&tmp, path.as_str(), &bytes[..], &fsync, *component) {
            return Ok(code);
        }
        staged_files.push((tmp, path, *component));
    }
    for (i, (tmp, path, _)) in staged_files.iter().enumerate() {
        if let Err(err) = std::fs::rename(tmp, path.as_str()) {
            eprintln!("error: unable to write file {path}: {}", errno_text(&err));
            eprintln!("fatal: unable to rename temporary file to '{path}'");
            // Only the temporary being renamed right now is the one git's
            // `tempfile` list owns; the ones still queued behind it stay.
            let _ = std::fs::remove_file(tmp);
            let _ = i;
            return Ok(ExitCode::from(128));
        }
    }

    println!("{hex_id}");
    Ok(ExitCode::SUCCESS)
}

/// The zlib level pack entries are deflated at.
///
/// `--compression=<n>` wins, then `pack.compression`, then `core.compression`,
/// then zlib's own default — git's `git_default_config()` order. Out-of-range
/// command-line values never reach here (`preflight` rejects them), and `-1` is
/// zlib's "use the default"; an out-of-range *config* value is ignored the same
/// way, since git's own reader clamps rather than dies for these two keys.
fn compression(repo: &gix::Repository, st: &State) -> gix::zlib::Compression {
    let configured = || {
        let snapshot = repo.config_snapshot();
        snapshot
            .integer("pack.compression")
            .or_else(|| snapshot.integer("core.compression"))
    };
    let level = match st.compression {
        Some(level) => Some(level),
        None => configured(),
    };
    match level {
        Some(level) if (0..=9).contains(&level) => {
            gix::zlib::Compression::new(level as i32).unwrap_or(gix::zlib::Compression::DEFAULT)
        }
        _ => gix::zlib::Compression::DEFAULT,
    }
}

/// How many threads the delta search will really use, for the line git prints
/// before it starts one: `pack.threads` when set, otherwise one per logical core
/// (git's `online_cpus()`). Kept in step with `delta::search`, which resolves
/// zero the same way.
fn resolved_threads(configured: usize) -> usize {
    match configured {
        0 => std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
        n => n,
    }
}

/// git's end-of-run summary, which `--progress`/`--all-progress` put on stderr
/// and `-q` (or the absence of both, stderr not being a terminal here)
/// suppresses.
///
/// The reuse counts are always zero, which is the truth about the pack written
/// here — nothing is ever copied out of an existing pack — rather than a
/// stand-in for git's numbers; see the module docs.
fn report_progress(progress: bool, total: usize, deltas: usize) {
    if progress {
        eprintln!("Total {total} (delta {deltas}), reused 0 (delta 0), pack-reused 0 (from 0)");
    }
}

/// One entry as it was written into the pack.
#[derive(Clone)]
pub(crate) struct PackedEntry {
    pub(crate) id: ObjectId,
    /// Byte offset of the entry header within the pack.
    pub(crate) offset: u64,
    /// CRC-32 over the entry's bytes in the pack (header plus compressed data),
    /// which is what a v2 `.idx` stores.
    pub(crate) crc32: u32,
    /// The object's own type, which stays the object's type even when the entry
    /// holding it is a delta. A `.bitmap`'s four type bitmaps are built from
    /// this.
    pub(crate) kind: gix::object::Kind,
    /// git's `pack_name_hash()` of the path the object was last seen at, which
    /// is what a `.bitmap`'s hash-cache extension stores.
    pub(crate) name_hash: u32,
}

/// A finished pack held in memory, alongside the per-entry data its `.idx`,
/// `.rev` and `.mtimes` companions need.
pub(crate) struct Packed {
    pub(crate) bytes: Vec<u8>,
    pub(crate) id: ObjectId,
    pub(crate) entries: Vec<PackedEntry>,
}

/// Build a complete packfile from an explicit set of object ids and return its
/// raw bytes (`PACK` header … trailing hash). Used by `send-pack` to produce the
/// pack streamed to a remote's receive-pack. Objects that cannot be read are
/// dropped, matching [`write_pack`]'s own tolerance.
///
/// The repository's `pack.*` delta settings apply, but `OBJ_REF_DELTA` is used
/// rather than `OBJ_OFS_DELTA`, which is what `pack-objects` does when nothing
/// passed `--delta-base-offset`. A receiver that predates offset deltas can
/// still read the result.
pub(crate) fn pack_bytes_for(repo: &gix::Repository, ids: &[ObjectId]) -> Result<Vec<u8>> {
    pack_bytes_with(repo, ids, false)
}

/// [`pack_bytes_for`], with the caller choosing how deltas name their base.
///
/// `git repack` and `git gc` set `allow_ofs_delta` from
/// `repack.useDeltaBaseOffset`, whose default is true; a pack written for a
/// remote leaves it false, which is `pack-objects`' own default.
pub(crate) fn pack_bytes_with(
    repo: &gix::Repository,
    ids: &[ObjectId],
    allow_ofs_delta: bool,
) -> Result<Vec<u8>> {
    Ok(packed_for(
        repo,
        ids,
        WriteOptions {
            allow_ofs_delta,
            ..WriteOptions::default()
        },
    )?
    .bytes)
}

/// What a caller outside this module can steer the pack writer with. Everything
/// left unset comes from the repository's `pack.*` config, as it does for
/// `pack-objects` itself.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WriteOptions {
    /// `repack.useDeltaBaseOffset`: emit `OBJ_OFS_DELTA` rather than
    /// `OBJ_REF_DELTA`.
    pub(crate) allow_ofs_delta: bool,
    /// Shadows `pack.window`, as `gc --aggressive` does with
    /// `--window=<gc.aggressiveWindow>`.
    pub(crate) window: Option<usize>,
    /// Shadows `pack.depth`, as `gc --aggressive` does with
    /// `--depth=<gc.aggressiveDepth>`.
    pub(crate) depth: Option<usize>,
    /// Shadows `pack.windowMemory`, as a cruft pack does with
    /// `--window-memory=<repack.cruftWindowMemory>`.
    pub(crate) window_memory: Option<u64>,
    /// Shadows `pack.threads`, as a cruft pack does with
    /// `--threads=<repack.cruftThreads>`.
    pub(crate) threads: Option<usize>,
    /// Report the counting, compressing and writing phases on stderr the way
    /// git's progress meter does. See [`crate::progress`].
    pub(crate) progress: bool,
    /// `repack.useDeltaIslands`: pass `--delta-islands` on, so the search
    /// honours `pack.island`.
    pub(crate) use_delta_islands: bool,
    /// `--no-reuse-delta`, which `repack -f` passes and `gc --aggressive` gets
    /// through the `-f` it pushes onto its `repack` (`builtin/gc.c:920`).
    /// Stated in the negative so `Default` keeps git's default, which is to
    /// reuse.
    pub(crate) no_reuse_delta: bool,
}

/// Build the pack for `ids` and hand back its bytes together with the per-entry
/// offsets and CRCs an `.idx`, `.rev` or `.mtimes` needs.
pub(crate) fn packed_for(
    repo: &gix::Repository,
    ids: &[ObjectId],
    options: WriteOptions,
) -> Result<Packed> {
    let counts: Vec<pack::data::output::Count> = ids
        .iter()
        .map(|id| pack::data::output::Count {
            id: *id,
            entry_pack_location: pack::data::output::count::PackLocation::NotLookedUp,
        })
        .collect();
    let mut delta = DeltaConfig::from_repo(repo).unwrap_or_default();
    delta.allow_ofs_delta = options.allow_ofs_delta;
    delta.use_islands = options.use_delta_islands;
    delta.reuse_delta &= !options.no_reuse_delta;
    if let Some(window) = options.window {
        delta.search.window = window;
    }
    if let Some(depth) = options.depth {
        delta.search.depth = depth;
    }
    if let Some(limit) = options.window_memory {
        delta.search.window_memory_limit = limit;
    }
    if let Some(threads) = options.threads {
        delta.search.threads = threads;
    }
    write_pack(
        repo,
        &counts,
        compression(repo, &State::default()),
        &delta,
        options.progress,
    )
}

/// git's delta-search knobs, resolved the way `cmd_pack_objects` resolves them:
/// `git_pack_config()` first, then the command line, then the post-parse clamps.
#[derive(Debug, Clone, Copy)]
struct DeltaConfig {
    search: pack::data::output::delta::Options,
    /// `--delta-base-offset`: emit `OBJ_OFS_DELTA` instead of `OBJ_REF_DELTA`.
    /// git's `allow_ofs_delta`, which defaults off — `git repack` is what turns
    /// it on, from `repack.useDeltaBaseOffset`.
    allow_ofs_delta: bool,
    /// `--delta-islands`: restrict the search to bases at least as reachable as
    /// their targets, per `pack.island`. git's `use_delta_islands`, which
    /// `git repack` turns on from `repack.useDeltaIslands`.
    use_islands: bool,
    /// `--[no-]reuse-delta`: whether an object already stored as a delta in one
    /// of the repository's packs keeps that delta instead of being searched for
    /// a fresh one. git's `reuse_delta`, which defaults on
    /// (`pack-objects.c:209`) and is what `check_object()` acts on.
    reuse_delta: bool,
}

impl Default for DeltaConfig {
    fn default() -> Self {
        DeltaConfig {
            search: pack::data::output::delta::Options::default(),
            allow_ofs_delta: false,
            use_islands: false,
            reuse_delta: true,
        }
    }
}

impl DeltaConfig {
    /// The `pack.*` half, as `git_pack_config()` reads it. `Err` carries the
    /// `fatal:` line git dies with for a value it cannot read.
    fn from_repo(repo: &gix::Repository) -> Result<Self, String> {
        let mut cfg = DeltaConfig::default();
        let search = &mut cfg.search;
        if let Some(v) = crate::config::config_int(repo, "pack.window")? {
            search.window = usize::try_from(v).unwrap_or(0);
        }
        if let Some(v) = crate::config::config_int(repo, "pack.depth")? {
            search.depth = usize::try_from(v).unwrap_or(0);
        }
        if let Some(v) = crate::config::config_ulong(repo, "pack.windowMemory")? {
            search.window_memory_limit = v;
        }
        if let Some(v) = crate::config::config_int(repo, "pack.deltaCacheSize")? {
            search.max_delta_cache_size = u64::try_from(v).unwrap_or(0);
        }
        if let Some(v) = crate::config::config_int(repo, "pack.deltaCacheLimit")? {
            search.cache_max_small_delta_size = u64::try_from(v).unwrap_or(0);
        }
        if let Some(v) = crate::config::config_int(repo, "pack.threads")? {
            if v < 0 {
                return Err(format!("invalid number of threads specified ({v})"));
            }
            search.threads = v as usize;
        }
        Ok(cfg)
    }

    /// Apply the command line over the config, then git's post-parse clamps from
    /// `cmd_pack_objects`: a negative `--window`/`--depth` means "off", and a
    /// depth or delta-cache limit beyond what git's bit-fields can hold is
    /// lowered with a warning.
    fn apply(mut self, st: &State) -> Self {
        if let Some(v) = st.window {
            self.search.window = usize::try_from(v).unwrap_or(0);
        }
        if let Some(v) = st.depth {
            self.search.depth = usize::try_from(v).unwrap_or(0);
        }
        if let Some(v) = st.window_memory {
            self.search.window_memory_limit = v;
        }
        if let Some(v) = st.threads {
            self.search.threads = usize::try_from(v).unwrap_or(0);
        }
        if let Some(v) = st.delta_base_offset {
            self.allow_ofs_delta = v;
        }
        if let Some(v) = st.reuse_delta {
            self.reuse_delta = v;
        }
        self.use_islands |= st.delta_islands;

        // `depth >= 1 << OE_DEPTH_BITS` and `cache_max_small_delta_size >= 1 <<
        // OE_Z_DELTA_BITS`, with git's own wording.
        const MAX_DEPTH: usize = (1 << 12) - 1;
        const MAX_CACHE_LIMIT: u64 = (1 << 20) - 1;
        if self.search.depth > MAX_DEPTH {
            eprintln!(
                "warning: delta chain depth {} is too deep, forcing {MAX_DEPTH}",
                self.search.depth
            );
            self.search.depth = MAX_DEPTH;
        }
        if self.search.cache_max_small_delta_size > MAX_CACHE_LIMIT {
            eprintln!("warning: pack.deltaCacheLimit is too high, forcing {MAX_CACHE_LIMIT}");
            self.search.cache_max_small_delta_size = MAX_CACHE_LIMIT;
        }
        self
    }
}

/// Encode `counts` into a version-2 pack, delta-compressing it first.
///
/// Three phases, matching git's: resolve every object's type and size, run the
/// sliding-window delta search over them, then serialise. The write order is the
/// order `counts` arrives in, except that a delta's base is always emitted
/// first — which is what makes an `OBJ_OFS_DELTA`'s backwards distance
/// representable and what git's `write_one()` recursion guarantees too.
fn write_pack(
    repo: &gix::Repository,
    counts: &[pack::data::output::Count],
    level: gix::zlib::Compression,
    delta: &DeltaConfig,
    progress: bool,
) -> Result<Packed> {
    use crate::progress::Meter;
    use pack::data::output::delta;

    // Entries are encoded before the header is written, because the header
    // carries the entry *count*, which is known only once every object in the
    // set has been shown to be readable.
    const HEADER_LEN: u64 = 12;

    // Phase 1: types and sizes, from the object headers alone.
    //
    // This is the pass git reports as `Counting objects`: one step per object in
    // the set the traversal handed over. An object whose header cannot be read is
    // fatal — git's `check_object()` tolerates it (recording `OBJ_BAD` so that a
    // missing *preferred base* can be ignored), but the entry stays in the pack
    // list and `write_no_reuse_object()` dies `unable to read <oid>` when the
    // write reaches it. Raising it here rather than at the write is a step
    // earlier than git; stdout is empty and the exit code 128 either way, since
    // git's pack bytes are still sitting in the hashfile buffer when it dies.
    let mut counting = Meter::counted("Counting objects", counts.len(), progress);
    let mut objects: Vec<delta::Object> = Vec::with_capacity(counts.len());
    for count in counts {
        use gix::odb::HeaderExt;
        counting.tick();
        let Ok(header) = repo.objects.header(count.id) else {
            crate::git_fatal!("unable to read {}", count.id);
        };
        objects.push(delta::Object {
            id: count.id,
            kind: header.kind(),
            size: header.size(),
            name_hash: 0,
        });
    }
    counting.done();
    assign_name_hashes(repo, &mut objects);

    // Phase 2: the delta search, steered by `pack.island` when asked. git
    // announces the thread count first, then reports the search itself as
    // `Compressing objects`. The search parallelises internally and reports
    // nothing until it returns, so the meter goes from nothing to complete in one
    // step — the line git leaves on screen either way.
    //
    // Both lines are skipped when no delta can be found at all, which is git's
    // `if (nr_deltas)` gate: a one-object pack has nothing to deltify against,
    // and a zero window or depth disables the search. git counts its delta
    // *candidates* there, which this port does not work out separately, so the
    // count below is every object handed to the search.
    let islands = if delta.use_islands {
        load_delta_islands(repo, &objects)
    } else {
        delta::Islands::default()
    };
    let searchable = objects.len() > 1 && delta.search.window > 0 && delta.search.depth > 0;
    let compressing_progress = progress && searchable;
    if compressing_progress {
        eprintln!(
            "Delta compression using up to {} threads",
            resolved_threads(delta.search.threads)
        );
    }
    // `check_object()`'s delta reuse runs before the search, because what it
    // settles is also what the search must leave alone: an object that keeps a
    // delta from an existing pack is not a search candidate at all.
    let reused = if delta.reuse_delta {
        reused_delta_bases(repo, &objects, &islands)
    } else {
        vec![None; objects.len()]
    };
    let already_deltified: Vec<bool> = reused.iter().map(Option::is_some).collect();
    let mut compressing =
        Meter::counted("Compressing objects", objects.len(), compressing_progress);
    let mut deltas = delta::find_deltas(
        &objects,
        &islands,
        &delta.search,
        &already_deltified,
        || repo.clone(),
        |repo, id| {
            let mut buf = Vec::new();
            repo.objects.find(id, &mut buf).ok().map(|(object, _)| object.data.to_owned())
        },
    );
    // The reused pairs join the result the search left a hole for. `data: None`
    // sends [`write_entry`] down `get_delta()`'s recompute path rather than
    // git's `write_reuse_object()` byte copy; the delta encoder is the same one
    // that produced the on-disk entry, so it reproduces the same instructions.
    for at in 0..reused.len() {
        let Some(entry) = reused[at] else { continue };
        deltas[at] = Some(delta::Delta {
            base: entry.base,
            size: entry.size,
            depth: reused_depth(&reused, &deltas, at),
            data: None,
        });
    }
    compressing.advance(objects.len());
    compressing.done();

    // Phase 3: serialise, base before delta. `write_entry` recurses into an
    // object's base first, so the meter counts positions reached rather than
    // entries appended — the same total, and monotone, which is what the display
    // needs.
    let write_order = compute_write_order(&objects, &deltas, &tag_tips(repo));
    let mut writing = Meter::counted("Writing objects", objects.len(), progress);
    let mut body: Vec<u8> = Vec::new();
    let mut entries: Vec<PackedEntry> = Vec::with_capacity(objects.len());
    let mut offsets: Vec<Option<u64>> = vec![None; objects.len()];
    let mut written_deltas = 0usize;
    for at in write_order {
        writing.tick();
        write_entry(
            repo,
            at,
            &objects,
            &deltas,
            delta.allow_ofs_delta,
            level,
            &mut body,
            &mut offsets,
            &mut entries,
            &mut written_deltas,
        )?;
    }
    writing.done();
    // git's closing summary belongs to the pack write itself, so every caller —
    // `pack-objects` either way it emits, `repack` and `gc` — reports it.
    report_progress(progress, entries.len(), written_deltas);

    let kind = repo.object_hash();
    let mut bytes = Vec::with_capacity(HEADER_LEN as usize + body.len() + kind.len_in_bytes());
    bytes.extend_from_slice(b"PACK");
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    bytes.append(&mut body);

    let mut hasher = gix::hash::hasher(kind);
    hasher.update(&bytes[..]);
    let id = hasher.try_finalize()?;
    bytes.extend_from_slice(id.as_slice());
    Ok(Packed {
        bytes,
        id,
        entries,
    })
}

/// git's `check_object()` delta reuse: for each position in `objects`, the
/// position of the base it should keep deltifying against, or `None` when it has
/// no delta to reuse and must be searched for one.
///
/// git reads an object's *existing* representation before searching for a new
/// one. When the odb resolved the object out of a pack and stored it there as an
/// `OBJ_OFS_DELTA` or `OBJ_REF_DELTA`, `check_object()` resolves that entry's
/// base to an object id and hands it to `can_reuse_delta()`, which keeps the
/// pair when the base is itself in the set being packed. The delta then survives
/// the repack untouched — which is why a clone of a packed repository reproduces
/// the source pack's delta bases rather than whatever a fresh window scan
/// happens to prefer, and why a pack written without this differs from git's.
///
/// Only the *first* index that holds an object is consulted, because that is the
/// one the odb resolves it from and therefore git's `IN_PACK(entry)`. An object
/// stored whole, or stored as a delta on a base that is not being packed, gets
/// `None` and goes to the search.
fn reused_delta_bases(
    repo: &gix::Repository,
    objects: &[pack::data::output::delta::Object],
    islands: &pack::data::output::delta::Islands,
) -> Vec<Option<ReusedDelta>> {
    use gix::odb::store::structure::Record;

    let mut out: Vec<Option<ReusedDelta>> = vec![None; objects.len()];
    let at: std::collections::HashMap<ObjectId, usize> =
        objects.iter().enumerate().map(|(i, o)| (o.id, i)).collect();
    // Which objects some earlier index already answered for, so a second copy in
    // a later pack cannot override the representation the odb actually reads.
    let mut decided = vec![false; objects.len()];
    let Ok(records) = repo.objects.store_ref().structure() else {
        return out;
    };
    let hash = repo.object_hash();

    for record in records {
        let Record::Index { path, .. } = record else { continue };
        let Ok(idx) = pack::index::File::at(&path, hash) else { continue };
        let Ok(data) = pack::data::File::at(path.with_extension("pack"), hash) else {
            continue;
        };

        // An `OBJ_OFS_DELTA` names its base by a backwards distance, so turning
        // one into an object id needs the index's offset -> id mapping, which is
        // `offset_to_pack_pos()` followed by `nth_packed_object_id()`.
        let mut by_offset: Vec<(u64, ObjectId)> =
            idx.iter().map(|e| (e.pack_offset, e.oid)).collect();
        by_offset.sort_unstable_by_key(|(offset, _)| *offset);
        let id_at = |offset: u64| -> Option<ObjectId> {
            by_offset
                .binary_search_by_key(&offset, |(o, _)| *o)
                .ok()
                .map(|i| by_offset[i].1)
        };

        for entry in idx.iter() {
            let Some(&target) = at.get(&entry.oid) else { continue };
            if decided[target] {
                continue;
            }
            decided[target] = true;
            let Ok(packed) = data.entry(entry.pack_offset) else { continue };
            let base_id = match packed.header {
                pack::data::entry::Header::OfsDelta { base_distance } => {
                    match entry.pack_offset.checked_sub(base_distance) {
                        Some(offset) => id_at(offset),
                        None => None,
                    }
                }
                pack::data::entry::Header::RefDelta { base_id } => Some(base_id),
                _ => None,
            };
            // `can_reuse_delta()`: the base has to be one we are sending, and
            // islands must allow the pair. git's second arm — a base the
            // receiver is known to have from a reachability bitmap — belongs to
            // `--thin`, which this port does not produce.
            let Some(base) = base_id.and_then(|id| at.get(&id).copied()) else { continue };
            if base == target || !islands.in_same_island(target, base) {
                continue;
            }
            out[target] = Some(ReusedDelta {
                base,
                // `SET_DELTA_SIZE(entry, in_pack_size)`: a delta entry's header
                // size is the length of its instruction stream.
                size: packed.decompressed_size,
            });
        }
    }
    out
}

/// How long the delta chain ending at `at` is, counting `at` itself.
///
/// A reused delta's base may itself be a reused delta, or an object the search
/// deltified; both are followed. The walk is bounded by the chain length rather
/// than trusted to terminate — an existing pack's chains are acyclic, but the
/// bound costs nothing and a corrupt one must not hang the writer.
fn reused_depth(
    reused: &[Option<ReusedDelta>],
    searched: &[Option<pack::data::output::delta::Delta>],
    at: usize,
) -> u32 {
    let mut depth = 0u32;
    let mut cursor = at;
    for _ in 0..reused.len() {
        let next = match reused[cursor] {
            Some(entry) => entry.base,
            None => match &searched[cursor] {
                Some(delta) => delta.base,
                None => return depth,
            },
        };
        depth += 1;
        cursor = next;
    }
    depth
}

/// One delta [`reused_delta_bases`] kept from an existing pack.
#[derive(Debug, Clone, Copy)]
struct ReusedDelta {
    /// Index into the object slice of the base this one is expressed against.
    base: usize,
    /// The length of the delta instruction stream as the existing pack stores it.
    size: u64,
}

/// The objects `mark_tagged()` marks: for every reference under `refs/tags`, the
/// id it holds and the id it peels to.
///
/// That is the whole input to [`compute_write_order`]'s regrouping, and it is
/// why a repository with no tags writes its pack in enumeration order.
fn tag_tips(repo: &gix::Repository) -> HashSet<ObjectId> {
    let mut out = HashSet::new();
    let Ok(platform) = repo.references() else { return out };
    let Ok(tags) = platform.prefixed("refs/tags/") else { return out };
    for reference in tags.flatten() {
        let mut reference = reference;
        if let Some(id) = reference.target().try_id() {
            out.insert(id.to_owned());
        }
        if let Ok(id) = reference.peel_to_id() {
            out.insert(id.detach());
        }
    }
    out
}

/// `compute_write_order()`: the positions of `objects`, in the order the pack
/// writes them.
///
/// With nothing tagged this is the identity. `compute_layer_order()`'s first
/// loop copies entries across until it meets a tagged one, and where that never
/// happens it copies all of them — which is why an ordinary pack comes out in
/// the order it was enumerated in. A tag tip stops that loop at `last_untagged`,
/// and everything from there is regrouped: the tag tips first, then the
/// remaining commits and tags, then the trees, then everything else walked
/// family by family so a delta base and its deltas stay together.
///
/// Delta islands are not modelled here. git splits the order into one layer per
/// island and runs `compute_layer_order()` per layer; with no islands `oe_layer()`
/// is always 0 and there is exactly one layer, which is the case reproduced.
fn compute_write_order(
    objects: &[pack::data::output::delta::Object],
    deltas: &[Option<pack::data::output::delta::Delta>],
    tagged: &HashSet<ObjectId>,
) -> Vec<usize> {
    let n = objects.len();

    // `delta_child`/`delta_sibling`, built by walking backwards and pushing each
    // delta to the front of its base's child list — which leaves every child list
    // in ascending, i.e. original recency, order.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in (0..n).rev() {
        if let Some(d) = &deltas[i] {
            if d.base < n {
                children[d.base].insert(0, i);
            }
        }
    }
    // Where each entry sits in its parent's child list, so a "sibling" is the
    // next one along.
    let mut slot: Vec<usize> = vec![0; n];
    for list in &children {
        for (at, &child) in list.iter().enumerate() {
            slot[child] = at;
        }
    }
    let parent = |i: usize| deltas[i].as_ref().map(|d| d.base).filter(|b| *b < n);
    let sibling = |i: usize| {
        let p = parent(i)?;
        children[p].get(slot[i] + 1).copied()
    };

    let mut wo: Vec<usize> = Vec::with_capacity(n);
    let mut filled = vec![false; n];
    let add = |wo: &mut Vec<usize>, filled: &mut Vec<bool>, i: usize| {
        if !filled[i] {
            filled[i] = true;
            wo.push(i);
        }
    };

    // `add_descendants_to_write_order()`, ported as its own loop: at each node
    // reached "from above" the node and all its later siblings are added, then
    // the walk descends into the first child and repeats; a node with no child
    // steps right, or climbs until it can, without adding again.
    let add_descendants = |wo: &mut Vec<usize>, filled: &mut Vec<bool>, root: usize| {
        let mut add_to_order = true;
        let mut e = Some(root);
        while let Some(cur) = e {
            if add_to_order {
                add(wo, filled, cur);
                let mut s = sibling(cur);
                while let Some(next) = s {
                    add(wo, filled, next);
                    s = sibling(next);
                }
            }
            if let Some(child) = children[cur].first().copied() {
                add_to_order = true;
                e = Some(child);
                continue;
            }
            add_to_order = false;
            if let Some(next) = sibling(cur) {
                e = Some(next);
                continue;
            }
            let mut up = parent(cur);
            while let Some(p) = up {
                if sibling(p).is_some() {
                    break;
                }
                up = parent(p);
            }
            match up {
                Some(p) => e = sibling(p),
                None => return,
            }
        }
    };

    // 1. Everything up to the first tagged entry, in enumeration order.
    let mut last_untagged = n;
    for i in 0..n {
        if tagged.contains(&objects[i].id) {
            last_untagged = i;
            break;
        }
        add(&mut wo, &mut filled, i);
    }
    // 2. The tag tips.
    for i in last_untagged..n {
        if tagged.contains(&objects[i].id) {
            add(&mut wo, &mut filled, i);
        }
    }
    // 3. The remaining commits and tags.
    for i in last_untagged..n {
        if matches!(objects[i].kind, gix::object::Kind::Commit | gix::object::Kind::Tag) {
            add(&mut wo, &mut filled, i);
        }
    }
    // 4. The trees.
    for i in last_untagged..n {
        if objects[i].kind == gix::object::Kind::Tree {
            add(&mut wo, &mut filled, i);
        }
    }
    // 5. The rest, family by family from each family's root.
    for i in last_untagged..n {
        if !filled[i] {
            let mut root = i;
            while let Some(p) = parent(root) {
                root = p;
            }
            add_descendants(&mut wo, &mut filled, root);
        }
    }
    wo
}

/// The pack header is 12 bytes, so every entry offset is that much past its
/// position in the body being assembled.
const PACK_HEADER_LEN: u64 = 12;

/// Append one object to `body`, first appending whatever it deltifies against.
///
/// git's `write_one()`: recursion over the delta chain is what guarantees a base
/// precedes its deltas no matter which order the caller walks the objects in.
/// The chain is at most `pack.depth` long, so the recursion is bounded by the
/// same limit git bounds it by.
#[expect(clippy::too_many_arguments)]
fn write_entry(
    repo: &gix::Repository,
    at: usize,
    objects: &[pack::data::output::delta::Object],
    deltas: &[Option<pack::data::output::delta::Delta>],
    allow_ofs_delta: bool,
    level: gix::zlib::Compression,
    body: &mut Vec<u8>,
    offsets: &mut [Option<u64>],
    entries: &mut Vec<PackedEntry>,
    written_deltas: &mut usize,
) -> Result<()> {
    if offsets[at].is_some() {
        return Ok(());
    }
    if let Some(delta) = &deltas[at] {
        write_entry(
            repo,
            delta.base,
            objects,
            deltas,
            allow_ofs_delta,
            level,
            body,
            offsets,
            entries,
            written_deltas,
        )?;
    }

    let mut buf = Vec::new();
    let Ok((object, _location)) = repo.objects.find(&objects[at].id, &mut buf) else {
        // Readable header, unreadable body: drop it, and with it any delta that
        // named it as a base, which the `offsets` gap below takes care of.
        return Ok(());
    };

    // A delta whose base was itself dropped cannot be written as a delta.
    let base_offset = deltas[at].as_ref().and_then(|delta| offsets[delta.base]);
    let payload = match (&deltas[at], base_offset) {
        (Some(delta), Some(_)) => delta_bytes(repo, &objects[delta.base].id, delta, &object)?,
        _ => None,
    };

    let start = body.len();
    let offset = PACK_HEADER_LEN + start as u64;
    let (header, decompressed_size, raw) = match (payload, base_offset) {
        (Some(delta), Some(base_offset)) => {
            let header = if allow_ofs_delta {
                pack::data::entry::Header::OfsDelta {
                    base_distance: offset - base_offset,
                }
            } else {
                pack::data::entry::Header::RefDelta {
                    base_id: objects[deltas[at].as_ref().expect("delta present").base].id,
                }
            };
            let size = delta.len() as u64;
            *written_deltas += 1;
            (header, size, delta)
        }
        _ => {
            let header = match object.kind {
                gix::object::Kind::Tree => pack::data::entry::Header::Tree,
                gix::object::Kind::Blob => pack::data::entry::Header::Blob,
                gix::object::Kind::Commit => pack::data::entry::Header::Commit,
                gix::object::Kind::Tag => pack::data::entry::Header::Tag,
            };
            (header, object.data.len() as u64, object.data.to_owned())
        }
    };

    header.write_to(decompressed_size, body)?;
    deflate_into(&raw, level, body)?;
    offsets[at] = Some(offset);
    entries.push(PackedEntry {
        id: objects[at].id,
        offset,
        crc32: gix::features::hash::crc32(&body[start..]),
        kind: object.kind,
        name_hash: objects[at].name_hash,
    });
    Ok(())
}

/// The delta bytes for `at`: the ones the search cached, or a fresh delta
/// against the same base when the cache had no room for them. git does the same
/// in `get_delta()`.
///
/// `None` means the base could not be re-read or could not be indexed, in which
/// case the caller writes a base object instead — always valid, only larger.
fn delta_bytes(
    repo: &gix::Repository,
    base_id: &ObjectId,
    delta: &pack::data::output::delta::Delta,
    target: &gix::objs::Data<'_>,
) -> Result<Option<Vec<u8>>> {
    use pack::data::output::delta::Index;
    if let Some(data) = &delta.data {
        return Ok(Some(data.clone()));
    }
    let mut buf = Vec::new();
    let Ok((base, _location)) = repo.objects.find(base_id, &mut buf) else {
        return Ok(None);
    };
    let Some(index) = Index::new(base.data) else {
        return Ok(None);
    };
    Ok(index.create(target.data, 0))
}

/// Deflate `data` at `level` straight onto the end of `out`.
fn deflate_into(data: &[u8], level: gix::zlib::Compression, out: &mut Vec<u8>) -> Result<()> {
    let mut deflate = gix::zlib::stream::deflate::Write::new(std::mem::take(out), level);
    std::io::copy(&mut &data[..], &mut deflate)?;
    deflate.flush()?;
    *out = deflate.into_inner();
    Ok(())
}

/// Attach git's `pack_name_hash()` to every object that a tree in this set names.
///
/// The hash is what makes the delta search's sort put successive revisions of
/// one file next to each other; without it the sort falls back to size alone and
/// the window rarely holds two related objects at once. git computes it during
/// its object walk, where the path is already in hand; the walk that produced
/// `objects` here keeps no paths, so this recovers them by descending the trees
/// in the set once.
///
/// Objects no tree names — commits, tags, and root trees — keep a hash of zero,
/// which is what git gives them too.
fn assign_name_hashes(repo: &gix::Repository, objects: &mut [pack::data::output::delta::Object]) {
    use pack::data::output::delta::name_hash;
    use std::collections::VecDeque;

    let mut position: std::collections::HashMap<ObjectId, usize> = std::collections::HashMap::new();
    for (at, object) in objects.iter().enumerate() {
        position.insert(object.id, at);
    }

    // Seed with the root tree of every commit in the set; those are the only
    // trees whose path is known without a parent.
    let mut queue: VecDeque<(ObjectId, Vec<u8>)> = VecDeque::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    for object in objects.iter() {
        if object.kind != gix::object::Kind::Commit {
            continue;
        }
        let Ok(commit) = repo.find_commit(object.id) else {
            continue;
        };
        if let Ok(tree) = commit.tree_id() {
            if seen.insert(tree.detach()) {
                queue.push_back((tree.detach(), Vec::new()));
            }
        }
    }

    let mut buf = Vec::new();
    while let Some((id, prefix)) = queue.pop_front() {
        let Ok((object, _location)) = repo.objects.find(&id, &mut buf) else {
            continue;
        };
        if object.kind != gix::object::Kind::Tree {
            continue;
        }
        let Ok(tree) = gix::objs::TreeRef::from_bytes(object.data, repo.object_hash()) else {
            continue;
        };
        for entry in &tree.entries {
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(entry.filename);
            let child = entry.oid.to_owned();
            if let Some(&at) = position.get(&child) {
                if objects[at].name_hash == 0 {
                    objects[at].name_hash = name_hash(&path);
                }
            }
            if entry.mode.is_tree() && seen.insert(child) {
                queue.push_back((child, path));
            }
        }
    }
}

/// The `.idx` for a pack, in version 1 or 2.
///
/// `sorted` must be ordered by object id, which is the order both formats store
/// entries in and the order the 256-entry fan-out summarises.
///
/// A v2 index cannot represent an offset of 2 GiB or more inline; git spills
/// those into a 64-bit table flagged by the high bit. Packs written here are far
/// below that, but the table is emitted correctly rather than assumed away.
pub(super) fn index_file(
    kind: gix::hash::Kind,
    version: u64,
    pack_id: &ObjectId,
    sorted: &[PackedEntry],
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let v2 = version >= 2;
    if v2 {
        bytes.extend_from_slice(&[0xff, b't', b'O', b'c']);
        bytes.extend_from_slice(&2u32.to_be_bytes());
    }

    // Fan-out: for each leading byte, how many ids sort at or below it.
    let mut fanout = [0u32; 256];
    for entry in sorted {
        fanout[entry.id.as_slice()[0] as usize] += 1;
    }
    let mut running = 0u32;
    for slot in &mut fanout {
        running += *slot;
        *slot = running;
    }
    for slot in fanout {
        bytes.extend_from_slice(&slot.to_be_bytes());
    }

    if v2 {
        for entry in sorted {
            bytes.extend_from_slice(entry.id.as_slice());
        }
        for entry in sorted {
            bytes.extend_from_slice(&entry.crc32.to_be_bytes());
        }
        let mut large: Vec<u64> = Vec::new();
        for entry in sorted {
            match u32::try_from(entry.offset) {
                Ok(small) if small & 0x8000_0000 == 0 => {
                    bytes.extend_from_slice(&small.to_be_bytes());
                }
                _ => {
                    let slot = large.len() as u32;
                    large.push(entry.offset);
                    bytes.extend_from_slice(&(slot | 0x8000_0000).to_be_bytes());
                }
            }
        }
        for offset in large {
            bytes.extend_from_slice(&offset.to_be_bytes());
        }
    } else {
        // v1 interleaves a 4-byte offset with each id.
        for entry in sorted {
            bytes.extend_from_slice(&(entry.offset as u32).to_be_bytes());
            bytes.extend_from_slice(entry.id.as_slice());
        }
    }

    bytes.extend_from_slice(pack_id.as_slice());
    append_checksum(&mut bytes, kind)?;
    Ok(bytes)
}

/// The `.rev` for a pack: `RIDX`, the format version, the hash identifier, then
/// the index positions of the entries ordered by their offset in the pack.
///
/// `gix_pack::index::write_reverse_index` writes the same bytes from a parsed
/// `.idx` on disk. That entry point is the one to use when a pack is already
/// indexed; this one exists because the entries here have not been written
/// anywhere yet — and must not be, since the destination may be unwritable and
/// the resulting diagnostic has to name the `.pack`, not a temporary.
pub(super) fn reverse_index_file(
    kind: gix::hash::Kind,
    pack_id: &ObjectId,
    sorted: &[PackedEntry],
) -> Result<Vec<u8>> {
    let mut by_offset: Vec<(u64, u32)> = sorted
        .iter()
        .enumerate()
        .map(|(position, entry)| (entry.offset, position as u32))
        .collect();
    by_offset.sort_unstable();

    let mut bytes = Vec::with_capacity(12 + 4 * sorted.len() + 2 * kind.len_in_bytes());
    bytes.extend_from_slice(b"RIDX");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&hash_id(kind).to_be_bytes());
    for (_, position) in &by_offset {
        bytes.extend_from_slice(&position.to_be_bytes());
    }
    bytes.extend_from_slice(pack_id.as_slice());
    append_checksum(&mut bytes, kind)?;
    Ok(bytes)
}

/// Load the delta islands `pack.island` describes, as marks over `objects`;
/// git's `load_delta_islands()` followed by the propagation
/// `propagate_island_marks()` and `resolve_tree_islands()` do.
///
/// # What an island is for
///
/// On a server hosting many forks of one repository, every fork's refs live in
/// one object store. Left alone the delta search will happily express a blob
/// only fork A can reach as a delta against one only fork B can reach, and then
/// serving fork A means sending fork B's object too. An island is the set of
/// refs one fork owns; marking objects with the islands that reach them lets
/// the search refuse exactly those cross-fork deltas.
///
/// # How the marks are derived
///
/// Each `pack.island` value is a regex over ref names, anchored at the start.
/// A ref belongs to the island named by joining that regex's capture groups
/// with `-`, so `refs/heads/(.*)` puts every branch on its own island and
/// `refs/remotes/([^/]+)/` puts each remote on one. Later values win over
/// earlier ones. Two islands whose refs point at exactly the same objects are
/// collapsed into one, which is what keeps a thousand identical forks from
/// costing a thousand bits.
///
/// The marks then flow the way objects are reachable: a commit passes its marks
/// to its tree and its parents, and a tree to everything it names — trees first
/// in ascending depth, so a subtree that several parents share ends up with all
/// of their marks rather than whichever happened to be walked last.
///
/// An empty result means islands are switched off, which the search reads as
/// "every pair is eligible".
fn load_delta_islands(
    repo: &gix::Repository,
    objects: &[pack::data::output::delta::Object],
) -> pack::data::output::delta::Islands {
    use pack::data::output::delta::Islands;

    let snapshot = repo.config_snapshot();
    let patterns: Vec<String> = snapshot
        .strings("pack.island")
        .map(|values| values.iter().map(|v| v.to_string()).collect())
        .unwrap_or_default();
    if patterns.is_empty() {
        return Islands::default();
    }
    let core_name = snapshot.string("pack.islandCore").map(|v| v.to_string());

    // git anchors every island regex at the start of the ref name, adding the
    // `^` itself when the value does not already carry one.
    let mut expressions = Vec::with_capacity(patterns.len());
    for pattern in &patterns {
        let anchored = if pattern.starts_with('^') {
            pattern.clone()
        } else {
            format!("^{pattern}")
        };
        match regex::Regex::new(&anchored) {
            Ok(re) => expressions.push(re),
            Err(err) => {
                eprintln!("fatal: failed to load island regex for 'pack.island': {anchored} ({err})");
                return Islands::default();
            }
        }
    }

    // Every ref, bucketed by the island name its matching regex names.
    let mut islands: std::collections::BTreeMap<String, Vec<ObjectId>> = std::collections::BTreeMap::new();
    if let Ok(platform) = repo.references() {
        if let Ok(all) = platform.all() {
            for reference in all.flatten() {
                let name = reference.name().as_bstr().to_string();
                // git walks the regexes backwards, so the last configured value
                // that matches is the one that names the island.
                let Some(matched) = expressions.iter().rev().find_map(|re| re.captures(&name)) else {
                    continue;
                };
                let island_name = matched
                    .iter()
                    .skip(1)
                    .flatten()
                    .map(|group| group.as_str())
                    .collect::<Vec<_>>()
                    .join("-");
                let mut reference = reference;
                let Ok(id) = reference.peel_to_id_in_place() else {
                    continue;
                };
                islands.entry(island_name).or_default().push(id.detach());
            }
        }
    }
    if islands.is_empty() {
        return Islands::default();
    }

    // git's `deduplicate_islands()`: an island is identified by the sum of the
    // first eight bytes of each of its object ids, and only the first island
    // with a given sum survives. Forks that have not diverged therefore share
    // one bit rather than each burning their own.
    let hash_of = |ids: &Vec<ObjectId>| -> u64 {
        ids.iter().fold(0u64, |sum, id| {
            let bytes: [u8; 8] = id.as_bytes()[..8].try_into().expect("8 bytes");
            sum.wrapping_add(u64::from_ne_bytes(bytes))
        })
    };
    let core_hash = core_name
        .as_deref()
        .and_then(|name| islands.get(name))
        .map(&hash_of);
    let mut seen: HashSet<u64> = HashSet::new();
    let surviving: Vec<(u64, &Vec<ObjectId>)> = islands
        .values()
        .map(|ids| (hash_of(ids), ids))
        .filter(|(hash, _)| seen.insert(*hash))
        .collect();

    let position: std::collections::HashMap<ObjectId, usize> = objects
        .iter()
        .enumerate()
        .map(|(at, object)| (object.id, at))
        .collect();
    let width = surviving.len().div_ceil(32).max(1);
    let mut marks: Vec<Option<Vec<u32>>> = vec![None; objects.len()];
    let mut core_bit = None;
    for (bit, (hash, ids)) in surviving.iter().enumerate() {
        if core_hash == Some(*hash) {
            core_bit = Some(bit);
        }
        for id in ids.iter() {
            if let Some(&at) = position.get(id) {
                marks[at].get_or_insert_with(|| vec![0u32; width])[bit / 32] |= 1 << (bit % 32);
            }
        }
    }
    // `pack.islandCore` names the island whose objects git puts in the first
    // pack layer; nothing here writes layered packs, so the only thing left to
    // do with it is to have resolved which island it is, which the mark above
    // records.
    let _ = core_bit;

    propagate_island_marks(repo, objects, &position, &mut marks, width);

    Islands {
        marks: marks
            .into_iter()
            .map(|mark| mark.map(std::sync::Arc::from))
            .collect(),
    }
}

/// Flow island marks from commits to their trees and parents, and from trees to
/// their entries; git's `propagate_island_marks()` and `resolve_tree_islands()`.
///
/// Commits are walked newest first, which is the order git's own revision walk
/// hands them over, so a mark reaches an ancestor before that ancestor is asked
/// to pass it on. Trees are then walked in ascending depth for the same reason:
/// a tree must have collected every mark that reaches it before it hands them
/// to its children.
fn propagate_island_marks(
    repo: &gix::Repository,
    objects: &[pack::data::output::delta::Object],
    position: &std::collections::HashMap<ObjectId, usize>,
    marks: &mut [Option<Vec<u32>>],
    width: usize,
) {
    fn merge(marks: &mut [Option<Vec<u32>>], into: usize, from: &[u32], width: usize) {
        let target = marks[into].get_or_insert_with(|| vec![0u32; width]);
        for (word, bits) in target.iter_mut().zip(from) {
            *word |= bits;
        }
    }

    let mut commits: Vec<(usize, i64)> = Vec::new();
    for (at, object) in objects.iter().enumerate() {
        if object.kind != gix::object::Kind::Commit {
            continue;
        }
        let Ok(commit) = repo.find_commit(object.id) else {
            continue;
        };
        commits.push((at, commit.time().map(|time| time.seconds).unwrap_or(0)));
    }
    commits.sort_by(|a, b| b.1.cmp(&a.1));

    for (at, _) in commits {
        let Some(source) = marks[at].clone() else { continue };
        let Ok(commit) = repo.find_commit(objects[at].id) else {
            continue;
        };
        if let Ok(tree) = commit.tree_id() {
            if let Some(&tree_at) = position.get(&tree.detach()) {
                merge(marks, tree_at, &source, width);
            }
        }
        for parent in commit.parent_ids() {
            if let Some(&parent_at) = position.get(&parent.detach()) {
                merge(marks, parent_at, &source, width);
            }
        }
    }

    // Tree depth, as git's `show_object()` records it from the path a tree was
    // seen at: a root tree is depth zero and every path component adds one.
    let depths = tree_depths(repo, objects, position);
    let mut trees: Vec<(usize, u32)> = objects
        .iter()
        .enumerate()
        .filter(|(_, object)| object.kind == gix::object::Kind::Tree)
        .map(|(at, _)| (at, depths.get(&at).copied().unwrap_or(0)))
        .collect();
    trees.sort_by_key(|(_, depth)| *depth);

    let mut buf = Vec::new();
    for (at, _) in trees {
        let Some(source) = marks[at].clone() else { continue };
        let Ok((object, _location)) = repo.objects.find(&objects[at].id, &mut buf) else {
            continue;
        };
        let Ok(tree) = gix::objs::TreeRef::from_bytes(object.data, repo.object_hash()) else {
            continue;
        };
        let children: Vec<ObjectId> = tree
            .entries
            .iter()
            .filter(|entry| !entry.mode.is_commit())
            .map(|entry| entry.oid.to_owned())
            .collect();
        for child in children {
            if let Some(&child_at) = position.get(&child) {
                merge(marks, child_at, &source, width);
            }
        }
    }
}

/// How deep in a tree each object was first seen, keyed by its position in
/// `objects`.
///
/// git gets this for free from the path its object walk carries; the walk that
/// produced `objects` keeps no paths, so this recovers the depths by descending
/// from every commit's root tree once.
fn tree_depths(
    repo: &gix::Repository,
    objects: &[pack::data::output::delta::Object],
    position: &std::collections::HashMap<ObjectId, usize>,
) -> std::collections::HashMap<usize, u32> {
    use std::collections::VecDeque;

    let mut out: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
    let mut queue: VecDeque<(ObjectId, u32)> = VecDeque::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    for object in objects {
        if object.kind != gix::object::Kind::Commit {
            continue;
        }
        let Ok(commit) = repo.find_commit(object.id) else {
            continue;
        };
        if let Ok(tree) = commit.tree_id() {
            if seen.insert(tree.detach()) {
                queue.push_back((tree.detach(), 0));
            }
        }
    }

    let mut buf = Vec::new();
    while let Some((id, depth)) = queue.pop_front() {
        if let Some(&at) = position.get(&id) {
            out.entry(at).and_modify(|d| *d = (*d).max(depth)).or_insert(depth);
        }
        let Ok((object, _location)) = repo.objects.find(&id, &mut buf) else {
            continue;
        };
        let Ok(tree) = gix::objs::TreeRef::from_bytes(object.data, repo.object_hash()) else {
            continue;
        };
        let children: Vec<ObjectId> = tree
            .entries
            .iter()
            .filter(|entry| entry.mode.is_tree())
            .map(|entry| entry.oid.to_owned())
            .collect();
        for child in children {
            if seen.insert(child) {
                queue.push_back((child, depth + 1));
            }
        }
    }
    out
}

/// Whether a `.bitmap` accompanies the pack, and what goes in it.
///
/// git resolves these in `git_pack_config()`; `write` on top of that is the
/// `-b`/`--write-bitmap-index` flag or `repack.writeBitmaps`, depending on which
/// command is asking.
#[derive(Debug, Clone, Default)]
pub(crate) struct BitmapOptions {
    /// Write a `.bitmap` at all.
    pub(crate) write: bool,
    /// `pack.writeBitmapHashCache`, on by default in git.
    pub(crate) hash_cache: bool,
    /// `pack.writeBitmapLookupTable`, off by default in git.
    pub(crate) lookup_table: bool,
    /// `pack.preferBitmapTips`: ref prefixes whose tips are pulled into the
    /// selection ahead of whatever the spacing rule would otherwise pick.
    pub(crate) preferred_tips: Vec<String>,
    /// `bitmapPseudoMerge.<name>.*`: the groups that batch un-bitmapped ref tips
    /// into shared bitmaps. Empty means the `.bitmap` carries no pseudo-merge
    /// section at all, which is git's default.
    pub(crate) pseudo_merges: Vec<PseudoMergeGroup>,
}

impl BitmapOptions {
    /// The `pack.*` half, with git's defaults. `write` is left to the caller.
    pub(crate) fn from_repo(repo: &gix::Repository) -> Self {
        let snapshot = repo.config_snapshot();
        BitmapOptions {
            write: false,
            hash_cache: snapshot.boolean("pack.writeBitmapHashCache").unwrap_or(true),
            lookup_table: snapshot.boolean("pack.writeBitmapLookupTable").unwrap_or(false),
            preferred_tips: snapshot
                .strings("pack.preferBitmapTips")
                .map(|values| values.iter().map(|v| v.to_string()).collect())
                .unwrap_or_default(),
            pseudo_merges: load_pseudo_merges_from_config(repo),
        }
    }
}

/// One `bitmapPseudoMerge.<name>.*` section; git's `struct pseudo_merge_group`.
///
/// The defaults are `pseudo_merge_group_init()`'s.
#[derive(Debug, Clone)]
pub(crate) struct PseudoMergeGroup {
    /// `.pattern`, anchored at the start of a ref name because git anchors it —
    /// it prepends a `^` to any value that does not already open with one.
    pattern: regex::Regex,
    /// `.decay`, the exponent `k` in the `f(n) = C * n^-k` series that sizes
    /// each successive unstable merge, so a larger value front-loads them.
    decay: f64,
    /// `.maxMerges`, the ceiling on how many merges the unstable half produces.
    max_merges: u32,
    /// `.sampleRate`, the fraction of eligible unstable tips that actually
    /// become parents. One in `1/sampleRate`, counted by position.
    sample_rate: f64,
    /// `.threshold`: a tip committed after this is too new to be worth merging
    /// at all, since it is still likely to be walked directly.
    threshold: i64,
    /// `.stableThreshold`: a tip committed before this is unlikely to move
    /// again, so it goes into a fixed-size merge that stays valid across
    /// repacks instead of a decaying one.
    stable_threshold: i64,
    /// `.stableSize`, how many tips one stable merge takes.
    stable_size: u32,
}

/// git's `MIN_PSEUDO_MERGE_SIZE`: below this the group stops decaying and takes
/// everything that is left in one merge.
const MIN_PSEUDO_MERGE_SIZE: usize = 8;

/// git's `load_pseudo_merges_from_config()`.
///
/// # Deliberate departure from git
///
/// Five things take the whole of git's `pack-objects` down with an exit of 128:
/// a group with no pattern, a pattern that will not compile, a `threshold`
/// older than the group's own `stableThreshold`, an unparseable timestamp, and
/// a value that is not a number where one is wanted. Here each is reported with
/// git's wording and then drops its group, which costs the pack that group's
/// pseudo-merges and nothing else — the same choice [`load_delta_islands`]
/// already makes for a `pack.island` regex that will not compile. Dropping
/// rather than defaulting is the point: a mistyped threshold must not quietly
/// become the built-in one and fill the file with merges nobody asked for.
///
/// The out-of-range numeric values are not in that set. git only warns about
/// those and carries on with the default, and so does this.
fn load_pseudo_merges_from_config(repo: &gix::Repository) -> Vec<PseudoMergeGroup> {
    /// What a group looks like before its pattern has been checked for.
    struct Partial {
        pattern: Option<regex::Regex>,
        /// Set by anything git would `die()` over, which drops the group.
        invalid: bool,
        decay: f64,
        max_merges: u32,
        sample_rate: f64,
        threshold: i64,
        stable_threshold: i64,
        stable_size: u32,
    }

    let now = std::time::SystemTime::now();
    let seconds_now = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // `DEFAULT_PSEUDO_MERGE_*`. The two thresholds are `approxidate()` calls in
    // git, so they move with the clock rather than being fixed offsets.
    let default = |pattern| Partial {
        pattern,
        invalid: false,
        decay: 1.0,
        max_merges: 64,
        sample_rate: 1.0,
        threshold: seconds_now - 7 * 24 * 60 * 60,
        stable_threshold: seconds_now - 30 * 24 * 60 * 60,
        stable_size: 512,
    };

    let snapshot = repo.config_snapshot();
    let Some(sections) = snapshot.plumbing().sections_by_name("bitmapPseudoMerge") else {
        return Vec::new();
    };

    // Insertion-ordered so that a group keeps the position its first mention
    // gave it, and later sections only overwrite values.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Partial> = std::collections::HashMap::new();

    for section in sections {
        // git's `parse_config_key()` requires a subsection; `bitmapPseudoMerge.decay`
        // with no name in between matches nothing.
        let Some(subsection) = section.header().subsection_name() else {
            continue;
        };
        if subsection.is_empty() {
            continue;
        }
        let name = subsection.to_string();
        if !groups.contains_key(&name) {
            order.push(name.clone());
            groups.insert(name.clone(), default(None));
        }
        let group = groups.get_mut(&name).expect("just inserted");

        let value = |key: &str| section.value(key).map(|v| v.to_string());
        // git names the variable back at the user through `parse_config_key()`,
        // which has already lowered the section and the key while leaving the
        // subsection alone.
        let var = |key: &str| format!("bitmappseudomerge.{name}.{}", key.to_lowercase());
        let bad_number = |raw: &str, key: &str| {
            eprintln!("fatal: bad numeric config value '{raw}' for '{}'", var(key));
        };

        if let Some(raw) = value("pattern") {
            // git prepends the anchor itself unless the value carries one.
            let anchored = if raw.starts_with('^') { raw.clone() } else { format!("^{raw}") };
            match regex::Regex::new(&anchored) {
                Ok(re) => group.pattern = Some(re),
                Err(_) => {
                    // git's `die()` here prints `sub` with `%s` rather than
                    // `%.*s`, so the subsection runs on into the key it was
                    // parsed out of and the name comes out as `<name>.pattern`.
                    eprintln!("fatal: failed to load pseudo-merge regex for {name}.pattern: '{anchored}'");
                    group.pattern = None;
                    group.invalid = true;
                }
            }
        }
        if let Some(raw) = value("decay") {
            match raw.parse::<f64>() {
                Ok(decay) if decay >= 0.0 => group.decay = decay,
                Ok(_) => {
                    eprintln!("warning: {} must be non-negative, using default", var("decay"));
                    group.decay = 1.0;
                }
                Err(_) => {
                    bad_number(&raw, "decay");
                    group.invalid = true;
                }
            }
        }
        if let Some(raw) = value("sampleRate") {
            match raw.parse::<f64>() {
                Ok(rate) if rate > 0.0 && rate <= 1.0 => group.sample_rate = rate,
                Ok(_) => {
                    eprintln!(
                        "warning: {} must be between 0 (exclusive) and 1, using default",
                        var("sampleRate")
                    );
                    group.sample_rate = 1.0;
                }
                Err(_) => {
                    bad_number(&raw, "sampleRate");
                    group.invalid = true;
                }
            }
        }
        if let Some(raw) = value("threshold") {
            match parse_expiry_date(&raw) {
                Some(at) => group.threshold = at,
                None => {
                    eprintln!("error: '{raw}' for '{}' is not a valid timestamp", var("threshold"));
                    group.invalid = true;
                }
            }
        }
        if let Some(raw) = value("maxMerges") {
            match raw.parse::<i64>() {
                Ok(max) if max >= 0 => group.max_merges = max.min(i64::from(u32::MAX)) as u32,
                Ok(_) => {
                    eprintln!("warning: {} must be non-negative, using default", var("maxMerges"));
                    group.max_merges = 64;
                }
                Err(_) => {
                    bad_number(&raw, "maxMerges");
                    group.invalid = true;
                }
            }
        }
        if let Some(raw) = value("stableThreshold") {
            match parse_expiry_date(&raw) {
                Some(at) => group.stable_threshold = at,
                None => {
                    eprintln!("error: '{raw}' for '{}' is not a valid timestamp", var("stableThreshold"));
                    group.invalid = true;
                }
            }
        }
        if let Some(raw) = value("stableSize") {
            match raw.parse::<i64>() {
                Ok(size) if size > 0 => group.stable_size = size.min(i64::from(u32::MAX)) as u32,
                Ok(_) => {
                    eprintln!("warning: {} must be positive, using default", var("stableSize"));
                    group.stable_size = 512;
                }
                Err(_) => {
                    bad_number(&raw, "stableSize");
                    group.invalid = true;
                }
            }
        }
    }

    let mut out = Vec::new();
    for name in order {
        let partial = groups.remove(&name).expect("named on the way in");
        if partial.invalid {
            continue;
        }
        let Some(pattern) = partial.pattern else {
            eprintln!("fatal: pseudo-merge group '{name}' missing required pattern");
            continue;
        };
        if partial.threshold < partial.stable_threshold {
            eprintln!("fatal: pseudo-merge group '{name}' has unstable threshold before stable one");
            continue;
        }
        out.push(PseudoMergeGroup {
            pattern,
            decay: partial.decay,
            max_merges: partial.max_merges,
            sample_rate: partial.sample_rate,
            threshold: partial.threshold,
            stable_threshold: partial.stable_threshold,
            stable_size: partial.stable_size,
        });
    }
    out
}

// git's `parse_expiry_date()` (date.c:957), which `--unpack-unreachable` and
// `--cruft-expiration` both take (`builtin/pack-objects.c:4981`). Shared with every other
// expiry option; `1.week.ago` needs no rewriting, since approxidate's own tokenizer treats
// every non-alphanumeric byte as a separator.
use crate::date::parse_expiry_date;

/// The `.bitmap` for a freshly written pack, or `None` when one cannot be
/// written.
///
/// git's `bitmap_writer_*` sequence, in the order `write_pack_file()` runs it:
/// build the four type bitmaps over the pack, select the commits worth an entry,
/// compute each one's reachable set, and serialise. The serialising half lives in
/// [`gix_pack::data::output::bitmap`]; this is the half that needs a repository.
///
/// # Reachability, and what "cannot be written" means
///
/// A bitmap answers "which objects in this pack does commit X reach", so it is
/// only meaningful when the pack holds *every* such object. When the walk
/// reaches something the pack does not have, git warns and fails the whole
/// command; this warns with git's wording and writes no `.bitmap`, leaving a
/// pack that is correct and merely uncached.
///
/// # Deliberate departure from git
///
/// git's `bitmap_builder_init()` runs a first-parent topological walk to find
/// the *maximal* commits — the ones whose bitmaps can be shared between several
/// selected commits — and builds those too, so a selected commit's bitmap is
/// often assembled from parts rather than walked for. The set of bits each
/// selected commit ends up with is the same either way; sharing decides how much
/// walking it takes to get there. Here each selected commit is walked from, in
/// ascending date order, stopping at any commit whose bitmap is already known —
/// which is git's own `fill_bitmap_commit()` short-circuit and recovers most of
/// the sharing on a linear history.
pub(crate) fn bitmap_file(
    repo: &gix::Repository,
    packed: &Packed,
    options: &BitmapOptions,
) -> Option<Vec<u8>> {
    use pack::data::output::bitmap;

    if packed.entries.is_empty() {
        return None;
    }

    // The two coordinate systems a `.bitmap` uses: bits address pack positions
    // (offset order, which is how `packed.entries` was built), entry headers and
    // the hash cache address index positions (object id order).
    let pack_position: std::collections::HashMap<ObjectId, u32> = packed
        .entries
        .iter()
        .enumerate()
        .map(|(at, entry)| (entry.id, at as u32))
        .collect();
    let mut by_oid: Vec<&PackedEntry> = packed.entries.iter().collect();
    by_oid.sort_unstable_by_key(|entry| entry.id);
    let index_position: std::collections::HashMap<ObjectId, u32> = by_oid
        .iter()
        .enumerate()
        .map(|(at, entry)| (entry.id, at as u32))
        .collect();

    let kinds: Vec<gix::object::Kind> = packed.entries.iter().map(|entry| entry.kind).collect();
    let name_hashes: Vec<u32> = by_oid.iter().map(|entry| entry.name_hash).collect();

    // git's `indexed_commits`: every commit in the object list, newest first.
    let preferred = preferred_tip_commits(repo, &options.preferred_tips);
    let mut commits: Vec<(ObjectId, i64, bool, bool)> = Vec::new();
    for entry in &packed.entries {
        if entry.kind != gix::object::Kind::Commit {
            continue;
        }
        let Ok(commit) = repo.find_commit(entry.id) else {
            continue;
        };
        let date = commit.time().map(|time| time.seconds).unwrap_or(0);
        let merge = commit.parent_ids().count() > 1;
        commits.push((entry.id, date, merge, preferred.contains(&entry.id)));
    }
    if commits.is_empty() {
        return None;
    }
    commits.sort_by(|a, b| b.1.cmp(&a.1));

    let selected_ids = select_commits(&commits);

    // Ascending date, so that walking a commit is most likely to run into an
    // ancestor whose bitmap has already been computed.
    let mut order: Vec<ObjectId> = selected_ids.clone();
    let dates: std::collections::HashMap<ObjectId, i64> =
        commits.iter().map(|(id, date, _, _)| (*id, *date)).collect();
    order.sort_by_key(|id| dates.get(id).copied().unwrap_or(0));

    let words = packed.entries.len().div_ceil(64);
    let mut computed: std::collections::HashMap<ObjectId, Vec<u64>> = std::collections::HashMap::new();
    for id in &order {
        let Some(reachable) = reachable_bitmap(repo, *id, words, &pack_position, &computed) else {
            return None;
        };
        computed.insert(*id, reachable);
    }

    // git's `select_pseudo_merges()` runs at the tail of commit selection, so
    // that it can tell which tips already got an entry of their own; and
    // `build_pseudo_merge_bitmaps()` runs after the entries have been built, so
    // the walks below can reuse everything already computed.
    let bitmapped: HashSet<ObjectId> = selected_ids.iter().copied().collect();
    let mut pseudo_merges: Vec<bitmap::PseudoMerge> = Vec::new();
    for parents in select_pseudo_merges(repo, &options.pseudo_merges, &pack_position, &bitmapped) {
        // git builds one bitmap by walking the synthetic merge, whose parents
        // are these commits. Walking each parent and OR-ing reaches the same
        // set, and leaves each parent memoised for the merges that follow.
        let mut reachable = vec![0u64; words];
        for parent in &parents {
            if !computed.contains_key(parent) {
                let Some(walked) = reachable_bitmap(repo, *parent, words, &pack_position, &computed) else {
                    return None;
                };
                computed.insert(*parent, walked);
            }
            for (word, bits) in reachable.iter_mut().zip(&computed[parent]) {
                *word |= bits;
            }
        }
        let mut parent_bits = vec![0u64; words];
        for parent in &parents {
            let at = pack_position[parent] as usize;
            parent_bits[at / 64] |= 1 << (at % 64);
        }
        pseudo_merges.push(bitmap::PseudoMerge {
            parents: parent_bits,
            reachable,
        });
    }

    let selected: Vec<bitmap::Commit> = order
        .iter()
        .map(|id| bitmap::Commit {
            index_position: index_position[id],
            date: dates.get(id).copied().unwrap_or(0),
            reachable: computed.remove(id).expect("just computed"),
        })
        .collect();

    bitmap::write(
        repo.object_hash(),
        &packed.id,
        &kinds,
        &name_hashes,
        selected,
        &pseudo_merges,
        bitmap::Options {
            hash_cache: options.hash_cache,
            lookup_table: options.lookup_table,
        },
    )
    .ok()
}

/// The ref tips a group matched, split by how likely they are to move; git's
/// `struct pseudo_merge_matches`.
#[derive(Default)]
struct PseudoMergeMatches {
    /// Committed before `stableThreshold`. These go into fixed-size merges,
    /// which the next repack can reproduce byte for byte and reuse.
    stable: Vec<(ObjectId, i64)>,
    /// Committed after `stableThreshold` but before `threshold`, and carrying no
    /// bitmap of their own. These go into the decaying merges.
    unstable: Vec<(ObjectId, i64)>,
}

/// git's `select_pseudo_merges()`: match every ref against every group, split
/// the matches by age, and partition each split into merges.
///
/// Returns one parent list per merge, in the order the merges are written.
/// `bitmapped` is the set of commits that already got an entry of their own,
/// git's `bitmap_writer_has_bitmapped_object_id()`; a tip in there is not worth
/// batching, since a reader will find its exact bitmap anyway.
///
/// # Deliberate departure from git
///
/// git holds a group's matches in a `strmap` and walks it with
/// `strmap_for_each_entry()`, so the order the capture-group names are
/// partitioned in is hash order and varies with the names present. This walks
/// them in sorted order, which is the same set of merges in a fixed sequence.
fn select_pseudo_merges(
    repo: &gix::Repository,
    groups: &[PseudoMergeGroup],
    pack_position: &std::collections::HashMap<ObjectId, u32>,
    bitmapped: &HashSet<ObjectId>,
) -> Vec<Vec<ObjectId>> {
    if groups.is_empty() {
        return Vec::new();
    }

    let mut matched: Vec<std::collections::BTreeMap<String, PseudoMergeMatches>> =
        groups.iter().map(|_| Default::default()).collect();

    let Ok(platform) = repo.references() else {
        return Vec::new();
    };
    let Ok(all) = platform.all() else {
        return Vec::new();
    };
    for reference in all.flatten() {
        let name = reference.name().as_bstr().to_string();
        let mut reference = reference;
        // git peels the ref first, since it is the commit a tag points at that
        // can be a merge parent, not the tag object.
        let Ok(id) = reference.peel_to_id_in_place() else {
            continue;
        };
        let id = id.detach();
        // git's `packlist_find()`: a tip the pack does not hold cannot be one.
        if !pack_position.contains_key(&id) {
            continue;
        }
        let Ok(commit) = repo.find_commit(id) else {
            continue;
        };
        let date = commit.time().map(|time| time.seconds).unwrap_or(0);
        let has_bitmap = bitmapped.contains(&id);

        for (group, matches) in groups.iter().zip(&mut matched) {
            let Some(captures) = group.pattern.captures(&name) else {
                continue;
            };
            // git hands `regexec()` sixteen capture slots and joins every one
            // that matched with a dash, so `refs/heads/(.*)/(.*)` buckets by
            // both halves at once. The whole-match slot is skipped only when the
            // pattern brought groups of its own.
            if captures.len() > 15 && captures.get(15).is_some() {
                eprintln!("warning: pseudo-merge regex from config has too many capture groups (max=14)");
            }
            let first = usize::from(captures.len() > 1);
            let bucket = (first..captures.len().min(16))
                .filter_map(|at| captures.get(at))
                .map(|group| group.as_str())
                .collect::<Vec<_>>()
                .join("-");

            let entry = matches.entry(bucket).or_default();
            if date <= group.stable_threshold {
                entry.stable.push((id, date));
            } else if date <= group.threshold && !has_bitmap {
                entry.unstable.push((id, date));
            }
        }
    }

    let mut out = Vec::new();
    for (group, matches) in groups.iter().zip(&mut matched) {
        for entry in matches.values_mut() {
            // git's `sort_pseudo_merge_matches()`: oldest first, so that the
            // merges a repack reproduces stay aligned as history grows.
            entry.stable.sort_by_key(|(_, date)| *date);
            entry.unstable.sort_by_key(|(_, date)| *date);
            partition_pseudo_merges(group, entry, &mut out);
        }
    }
    out
}

/// git's `select_pseudo_merges_1()`: cut one group's matches into merges.
///
/// The stable half is cut into equal `stableSize` slices. The unstable half is
/// cut into slices that shrink along a power series, so the newest tips — the
/// ones most likely to be asked for — land in small merges that a reader
/// satisfies easily, while old ones are swept up in bulk. Once a slice would
/// fall below [`MIN_PSEUDO_MERGE_SIZE`] the remainder goes into one final merge,
/// since a merge of two or three tips saves a reader almost nothing.
fn partition_pseudo_merges(group: &PseudoMergeGroup, matches: &PseudoMergeMatches, out: &mut Vec<Vec<ObjectId>>) {
    if matches.stable.is_empty() && matches.unstable.is_empty() {
        return;
    }

    let stable_size = (group.stable_size as usize).max(1);
    let mut stable_merges_nr = matches.stable.len() / stable_size;
    if matches.stable.len() % stable_size != 0 {
        stable_merges_nr += 1;
    }

    let mut at = 0usize;
    for _ in 0..stable_merges_nr {
        let mut parents = Vec::new();
        // git's do-while: take one, then keep taking until the running count
        // lands on a multiple of `stableSize`.
        loop {
            let Some((id, _)) = matches.stable.get(at) else {
                break;
            };
            parents.push(*id);
            at += 1;
            if at % stable_size == 0 {
                break;
            }
        }
        if !parents.is_empty() {
            out.push(parents);
        }
    }

    // git's `j % (uint32_t)(1.0 / group->sample_rate)`: one tip in every
    // `1/sampleRate`, counted by position rather than sampled at random, so two
    // runs over the same history choose the same tips.
    let stride = ((1.0 / group.sample_rate) as usize).max(1);

    let mut at = 0usize;
    for merge in 0..group.max_merges as usize {
        let size = pseudo_merge_group_size(group, matches.unstable.len(), merge);
        let end = if size < MIN_PSEUDO_MERGE_SIZE {
            matches.unstable.len()
        } else {
            at + size
        };
        let mut parents = Vec::new();
        while at < end && at < matches.unstable.len() {
            if at % stride == 0 {
                parents.push(matches.unstable[at].0);
            }
            at += 1;
        }
        if !parents.is_empty() {
            out.push(parents);
        }
        if end >= matches.unstable.len() {
            break;
        }
    }
}

/// git's `pseudo_merge_group_size()`: how many tips the `at`-th unstable merge
/// takes.
///
/// Sizes follow `f(n) = C * n^-k`, with `k` the decay and `C` scaled so that the
/// `maxMerges` sizes sum to the number of unstable tips.
fn pseudo_merge_group_size(group: &PseudoMergeGroup, unstable_nr: usize, at: usize) -> usize {
    // git's `gitexp()` declares its exponent `int` while `decay` is a `double`,
    // so C truncates at the call site and a decay of 1.5 behaves as 1. Rounding
    // here instead would size every merge differently from git.
    let decay = group.decay as i32;
    let mut scale = 0.0f64;
    for n in 0..group.max_merges {
        scale += 1.0 / gitexp(f64::from(n + 1), decay);
    }
    scale = unstable_nr as f64 / scale;
    ((scale / gitexp((at + 1) as f64, decay)) + 0.5) as usize
}

/// git's `gitexp()`: exponentiation by squaring, for a non-negative integer
/// power. `decay` is validated non-negative when it is read, so the loop always
/// terminates.
fn gitexp(mut base: f64, mut exp: i32) -> f64 {
    let mut result = 1.0;
    loop {
        if exp % 2 != 0 {
            result *= base;
        }
        exp >>= 1;
        if exp == 0 {
            break;
        }
        base *= base;
    }
    result
}

/// The commits every `pack.preferBitmapTips` prefix points at, git's
/// `for_each_preferred_bitmap_tip()` feeding `mark_bitmap_preferred_tip()`.
///
/// A prefix without a trailing slash grows one, so `refs/heads` matches
/// `refs/heads/main` but not `refs/headsfoo`. Tags are peeled, since it is the
/// commit that can carry a bitmap, not the tag object.
fn preferred_tip_commits(repo: &gix::Repository, prefixes: &[String]) -> HashSet<ObjectId> {
    let mut out = HashSet::new();
    for prefix in prefixes {
        let prefix = if prefix.ends_with('/') {
            prefix.clone()
        } else {
            format!("{prefix}/")
        };
        let Ok(platform) = repo.references() else { continue };
        let Ok(iter) = platform.prefixed(prefix.as_bytes()) else {
            continue;
        };
        for reference in iter.flatten() {
            let mut reference = reference;
            let Ok(id) = reference.peel_to_id_in_place() else {
                continue;
            };
            if repo.find_commit(id).is_ok() {
                out.insert(id.detach());
            }
        }
    }
    out
}

/// git's `bitmap_writer_select_commits()`: take every commit when there are few
/// of them, otherwise walk the date-ordered list in widening steps and take one
/// commit from each step.
///
/// `commits` is `(id, date, is_merge, is_preferred_tip)`, newest first. Within a
/// step git prefers a commit that a `pack.preferBitmapTips` ref points at, then
/// the last merge it saw, and falls back to the commit the step lands on.
fn select_commits(commits: &[(ObjectId, i64, bool, bool)]) -> Vec<ObjectId> {
    /// git's `next_commit_index()`: no gap at all for the first hundred commits,
    /// then a gap that widens to a hundred, then to five thousand.
    fn next_commit_index(idx: usize) -> usize {
        const MIN_COMMITS: usize = 100;
        const MAX_COMMITS: usize = 5000;
        const MUST_REGION: usize = 100;
        const MIN_REGION: usize = 20000;

        if idx <= MUST_REGION {
            return 0;
        }
        if idx <= MIN_REGION {
            return (idx - MUST_REGION).min(MIN_COMMITS);
        }
        (idx - MIN_REGION).min(MAX_COMMITS).max(MIN_COMMITS)
    }

    if commits.len() < 100 {
        return commits.iter().map(|(id, _, _, _)| *id).collect();
    }

    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        let next = next_commit_index(i);
        if i + next >= commits.len() {
            break;
        }
        let mut chosen = commits[i + next].0;
        if next != 0 {
            for step in 0..=next {
                let (id, _, merge, preferred) = commits[i + step];
                if preferred {
                    chosen = id;
                    break;
                }
                if merge {
                    chosen = id;
                }
            }
        }
        out.push(chosen);
        i += next + 1;
    }
    out
}

/// Every object in the pack reachable from `start`, as a plain bitmap over pack
/// positions; git's `fill_bitmap_commit()` plus `fill_bitmap_tree()`.
///
/// `None` means the pack does not hold the whole closure, which is the one
/// condition under which no bitmap may be written.
fn reachable_bitmap(
    repo: &gix::Repository,
    start: ObjectId,
    words: usize,
    pack_position: &std::collections::HashMap<ObjectId, u32>,
    computed: &std::collections::HashMap<ObjectId, Vec<u64>>,
) -> Option<Vec<u64>> {
    fn get(bitmap: &[u64], at: u32) -> bool {
        bitmap[at as usize / 64] & (1 << (at % 64)) != 0
    }
    fn set(bitmap: &mut [u64], at: u32) {
        bitmap[at as usize / 64] |= 1 << (at % 64);
    }

    let mut bitmap = vec![0u64; words];
    let mut queue: Vec<ObjectId> = vec![start];
    let mut trees: Vec<ObjectId> = Vec::new();

    while let Some(id) = queue.pop() {
        if id != start {
            if let Some(known) = computed.get(&id) {
                for (word, bits) in bitmap.iter_mut().zip(known) {
                    *word |= bits;
                }
                continue;
            }
        }
        let Some(&pos) = pack_position.get(&id) else {
            return warn_not_closed(&id);
        };
        set(&mut bitmap, pos);

        let Ok(commit) = repo.find_commit(id) else {
            return warn_not_closed(&id);
        };
        let Ok(tree) = commit.tree_id() else {
            return warn_not_closed(&id);
        };
        trees.push(tree.detach());
        for parent in commit.parent_ids() {
            let parent = parent.detach();
            let Some(&at) = pack_position.get(&parent) else {
                return warn_not_closed(&parent);
            };
            if !get(&bitmap, at) {
                set(&mut bitmap, at);
                queue.push(parent);
            }
        }
    }

    for tree in trees {
        let Some(&pos) = pack_position.get(&tree) else {
            return warn_not_closed(&tree);
        };
        if get(&bitmap, pos) {
            continue;
        }
        fill_tree(repo, &mut bitmap, tree, pos, pack_position)?;
    }
    Some(bitmap)
}

/// git's warning when an object the walk reached is not in the pack, and the
/// `None` that abandons the `.bitmap` because of it.
fn warn_not_closed<T>(id: &ObjectId) -> Option<T> {
    eprintln!(
        "warning: Failed to write bitmap index. Packfile doesn't have full closure \
         (object {id} is missing)"
    );
    None
}

/// git's `fill_bitmap_tree()`: set the tree's own bit, then descend into every
/// entry whose bit is not set yet.
///
/// A set bit means the whole subtree below it is already accounted for, which is
/// what keeps this from re-walking shared trees.
fn fill_tree(
    repo: &gix::Repository,
    bitmap: &mut [u64],
    tree: ObjectId,
    pos: u32,
    pack_position: &std::collections::HashMap<ObjectId, u32>,
) -> Option<()> {
    bitmap[pos as usize / 64] |= 1 << (pos % 64);

    let mut buf = Vec::new();
    let Ok((object, _location)) = repo.objects.find(&tree, &mut buf) else {
        return warn_not_closed(&tree);
    };
    let Ok(parsed) = gix::objs::TreeRef::from_bytes(object.data, repo.object_hash()) else {
        return warn_not_closed(&tree);
    };
    let entries: Vec<(ObjectId, gix::object::tree::EntryMode)> = parsed
        .entries
        .iter()
        .map(|entry| (entry.oid.to_owned(), entry.mode))
        .collect();

    for (id, mode) in entries {
        if mode.is_tree() {
            let Some(&child) = pack_position.get(&id) else {
                return warn_not_closed(&id);
            };
            if bitmap[child as usize / 64] & (1 << (child % 64)) != 0 {
                continue;
            }
            fill_tree(repo, bitmap, id, child, pack_position)?;
        } else if mode.is_blob_or_symlink() {
            let Some(&child) = pack_position.get(&id) else {
                return warn_not_closed(&id);
            };
            bitmap[child as usize / 64] |= 1 << (child % 64);
        }
        // A gitlink names a commit in another repository, which is never packed
        // here; git skips it too.
    }
    Some(())
}

/// The `.mtimes` a cruft pack carries: `MTME`, the format version, the hash
/// identifier, then one 32-bit mtime per entry in index (object id) order.
///
/// git records the mtime of the loose file or the value the object's previous
/// cruft pack carried, so that a second `--cruft` run does not reset the clock.
/// Only the loose half is available here; an object with no loose file on disk
/// falls back to the current time, exactly as git does for one it has no record
/// for.
fn mtimes_file(
    repo: &gix::Repository,
    kind: gix::hash::Kind,
    pack_id: &ObjectId,
    sorted: &[PackedEntry],
) -> Result<Vec<u8>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    let objects = repo.objects.store_ref().path().to_path_buf();

    let mut bytes = Vec::with_capacity(12 + 4 * sorted.len() + 2 * kind.len_in_bytes());
    bytes.extend_from_slice(b"MTME");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&hash_id(kind).to_be_bytes());
    for entry in sorted {
        let hex = entry.id.to_string();
        let path = objects.join(&hex[..2]).join(&hex[2..]);
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(now, |d| d.as_secs() as u32);
        bytes.extend_from_slice(&mtime.to_be_bytes());
    }
    bytes.extend_from_slice(pack_id.as_slice());
    append_checksum(&mut bytes, kind)?;
    Ok(bytes)
}

/// Work out which objects this invocation packs, in the order they are written.
///
/// git has three mutually exclusive ways of naming an object set, and this
/// mirrors them:
///
///   1. **`--stdin-packs`** — the named packs' objects, plus every loose object
///      when `--unpacked` is also given. No reachability walk is involved, which
///      is why `--stdin-packs` rejects every other rev-list option.
///   2. **`--cruft`** — everything the object database holds that the packs
///      named on stdin do not already cover. That is the set git carries into a
///      cruft pack, and it is deliberately *not* filtered by reachability: the
///      caller has already decided these objects are the leftovers.
///   3. **the rev list** — `--all`, `--reflog` and `--revs` supply commit tips
///      whose full ancestry is walked, `--indexed-objects` adds the index's
///      blobs and cache-tree, and an invocation with none of those reads a plain
///      object list from stdin. Tips are expanded to their trees and blobs by
///      `gix-pack`'s counter rather than by a walk written here.
///
/// `--unpacked` and `--incremental` then drop anything already in a pack, and
/// `--filter` drops whatever its spec excludes.
///
/// Objects reached by a walk that cannot be found are skipped rather than fatal:
/// a reflog naming a pruned commit is ordinary, and git drops those too. An id
/// read straight off stdin is different — git carries it into the pack list and
/// dies on it at write time — so those are kept; see [`read_object_list_from_stdin`].
///
/// `Err` carries a `fatal:` body: the one diagnostic git raises while building
/// the set rather than while parsing argv.
fn collect_counts(
    repo: &gix::Repository,
    st: &State,
    stdin: &[u8],
) -> Result<Vec<pack::data::output::Count>, String> {
    let mut ids: Vec<ObjectId> = if st.stdin_packs {
        // Here `--unpacked` *adds* the loose objects rather than restricting the
        // set: it is the one rev-list-implying option `--stdin-packs` accepts,
        // and it means "the named packs, plus whatever no pack covers".
        let mut ids = objects_in_named_packs(repo, stdin);
        if st.unpacked {
            ids.extend(loose_objects(repo));
        }
        ids
    } else if st.cruft {
        let covered: HashSet<ObjectId> = objects_in_named_packs(repo, stdin).into_iter().collect();
        let mut ids = loose_objects(repo);
        for index in super::prune::pack_indices(repo, repo.objects.store_ref().path()) {
            ids.extend((0..index.num_objects()).map(|n| index.oid_at_index(n).to_owned()));
        }
        ids.retain(|id| !covered.contains(id));
        ids
    } else {
        let mut ids = rev_list_objects(repo, st, stdin)?;
        // Restricting to what no pack holds only makes sense for a set derived
        // from a reachability walk; the two branches above name their packs
        // outright.
        if st.unpacked || st.incremental {
            let loose: HashSet<ObjectId> = loose_objects(repo).into_iter().collect();
            ids.retain(|id| loose.contains(id));
        }
        ids
    };

    dedup(&mut ids);
    apply_filter(repo, st.filter.as_deref(), &mut ids);

    Ok(ids
        .into_iter()
        .map(|id| pack::data::output::Count {
            id,
            entry_pack_location: pack::data::output::count::PackLocation::NotLookedUp,
        })
        .collect())
}

/// `read_object_list_from_stdin()`: the object list git reads when its internal
/// rev list is off.
///
/// Each `fgets` line is a raw object id — `parse_oid_hex`, never a rev — with an
/// optional path after it that only steers the delta search. A `-` prefix names
/// a *preferred base*, an object the search may delta against without packing
/// it; nothing here has preferred bases, so such a line is validated and then
/// ignored. Anything that is not a hex id is fatal, and the returned message
/// carries git's own two-line wording, the offending line included verbatim.
///
/// Ids are *not* checked against the object database: git does not check them
/// here either, and one that names nothing is carried all the way to the write,
/// where it becomes `unable to read <oid>`.
fn read_object_list_from_stdin(stdin: &[u8], hex_len: usize) -> Result<Vec<ObjectId>, String> {
    let mut out = Vec::new();
    // `split_inclusive` hands back exactly what `fgets` would: every line keeps
    // its newline, a final unterminated line is still a line, and the empty
    // remainder after a trailing newline is EOF rather than a blank line.
    for line in stdin.split_inclusive(|b| *b == b'\n') {
        let (body, what) = match line.split_first() {
            Some((b'-', rest)) => (rest, "edge object ID"),
            _ => (line, "object ID"),
        };
        // `parse_oid_hex()` reads exactly `the_hash_algo->hexsz` characters, so
        // the width comes from the repository being packed, not from a constant.
        let hex = body.get(..hex_len).filter(|h| h.iter().all(u8::is_ascii_hexdigit));
        let Some(id) = hex.and_then(|h| ObjectId::from_hex(h).ok()) else {
            let line = String::from_utf8_lossy(line);
            return Err(format!("expected {what}, got garbage:\n {line}"));
        };
        // A preferred base is never packed, only deltified against.
        if what == "object ID" {
            out.push(id);
        }
    }
    Ok(out)
}

/// The rev-list half of [`collect_counts`]: tips, their ancestry, and the trees
/// and blobs hanging off every commit reached.
fn rev_list_objects(
    repo: &gix::Repository,
    st: &State,
    stdin: &[u8],
) -> Result<Vec<ObjectId>, String> {
    // `revs->pending`, in the order git fills it. `cmd_pack_objects()` builds the
    // rev-list argv as `--all`, `--reflog`, `--indexed-objects`, then the stdin
    // specs, and `setup_revisions()` adds each option's objects as it reaches it,
    // so this order is the order the traversal will see them in. Tips stay
    // unpeeled: an annotated tag's own object belongs in the pack, and
    // `handle_commit()` is what peels it.
    let mut pending: Vec<ObjectId> = Vec::new();
    let mut from_stdin: Vec<ObjectId> = Vec::new();

    if st.all {
        if let Ok(platform) = repo.references() {
            if let Ok(all) = platform.all() {
                for reference in all {
                    let Ok(mut reference) = reference else { continue };
                    if let Ok(id) = reference.follow_to_object() {
                        // ```c
                        // if (!repo_has_object_file(repo, oid)) {
                        //         error(_("%s does not point to a valid object!"), refname);
                        //         return 0;
                        // }
                        // ```
                        //
                        // (`ref_resolves_to_object()`, refs.c) and then, because the ref was
                        // still pended, `die("bad object %s", name)` from `get_reference()`
                        // (revision.c). A pack built from a ref list that quietly dropped
                        // one would be missing whatever only that ref reaches.
                        if repo.find_object(id).is_err() {
                            let name = reference.name().as_bstr().to_string();
                            eprintln!("error: {name} does not point to a valid object!");
                            return Err(format!("bad object {name}"));
                        }
                        pending.push(id.detach());
                    }
                }
            }
        }
        // A symbolic HEAD repeats a ref already collected; a detached one is
        // only reachable here.
        if let Ok(head) = repo.head() {
            if let Some(id) = head.id() {
                pending.push(id.detach());
            }
        }
    }

    if st.reflog {
        pending.extend(reflog_objects(repo));
    }

    if st.indexed_objects {
        if let Ok(index) = repo.index_or_empty() {
            for entry in index.entries() {
                // git's `add_index_objects_to_pending()` skips gitlinks, whose
                // ids name commits in another repository.
                if entry.mode != gix::index::entry::Mode::COMMIT {
                    pending.push(entry.id);
                }
            }
            if let Some(tree) = index.tree() {
                push_cache_tree(tree, &mut pending);
            }
        }
    }

    // stdin is rev-list arguments when git's internal rev list is on, and a
    // plain object list otherwise.
    if st.revs {
        // `get_object_list()` brackets this loop with
        // `cfg->warn_on_object_refname_ambiguity = 0`, so a full-length hex on
        // stdin never draws the ambiguity warning the same name on argv would.
        let _quiet_ambiguity = crate::objname::AmbiguityWarnings::off();
        for line in stdin.split(|b| *b == b'\n') {
            let Ok(spec) = std::str::from_utf8(line) else { continue };
            let spec = spec.trim();
            // Exclusions would need a boundary-aware walk; the sets this
            // command is asked for in practice are `--all`-shaped, so a
            // `^rev` is skipped rather than silently treated as inclusion.
            if spec.is_empty() || spec.starts_with('^') || spec.starts_with('-') {
                continue;
            }
            // The bracket above only reaches `get_oid_basic()`'s full-hex branch.
            // The plain-name warning under it carries no such gate, so stock
            // `printf dup | git pack-objects --revs --stdout` prints it.
            crate::objname::warn_ambiguous_refname(repo, spec);
            if let Ok(id) = repo.rev_parse_single(spec) {
                pending.push(id.detach());
            }
        }
    } else if !st.internal_rev_list {
        from_stdin = read_object_list_from_stdin(stdin, repo.object_hash().len_in_hex())?;
    }

    // An id read off stdin becomes a pack entry *as it stands*: with no internal
    // rev list there is no traversal, so `add_object_entry()` adds that one
    // object and nothing under it — naming a commit packs the commit, not its
    // tree. git checks nothing at that point either, so an id that resolves to
    // no object stays in the list and becomes `unable to read <oid>` when the
    // write reaches it. Both are why these bypass the traversal, which expands
    // trees and can only report objects it could read.
    let mut out = from_stdin;
    out.extend(traverse_commit_list(repo, pending));
    Ok(out)
}

/// `list-objects.c::traverse_commit_list()`: the order git shows objects in, and
/// so the order `add_object_entry()` fills `to_pack.objects` in.
///
/// Two phases, and the split between them is what makes the order what it is.
///
/// 1. **Commits.** `prepare_revision_walk()` peels every pending object: a tag
///    is re-added to `pending` under its own id and then followed, a commit is
///    appended to `revs->commits`, and a tree or blob stays in `pending`. That
///    list is then stably sorted by committer date, newest first — stably, so
///    equal dates keep pending order. `do_traverse()` pops it front to back,
///    showing each commit, appending its tree to `pending`, and inserting each
///    unseen parent with `commit_list_insert_by_date()`, which walks past every
///    entry whose date is `>=` its own and so lands *after* equal dates.
/// 2. **Everything else.** `traverse_trees_and_blobs()` walks `pending` in
///    order: a tag is shown, a blob is shown, and a tree is shown and then
///    descended into, entry by entry in tree order, recursing into subtrees as
///    they are met. Nothing is shown twice, so a tree whose contents an earlier
///    tree already covered contributes only itself.
///
/// A pending id naming nothing is dropped rather than fatal: `--reflog` routinely
/// names commits that have since been pruned, and git's `parse_object()` gives it
/// a NULL it skips.
pub(crate) fn traverse_commit_list(repo: &gix::Repository, pending: Vec<ObjectId>) -> Vec<ObjectId> {
    use gix::object::Kind;

    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut out: Vec<ObjectId> = Vec::new();
    // `revs->commits`: (id, committer date), kept in git's date order.
    let mut queue: Vec<(ObjectId, i64)> = Vec::new();
    // What `prepare_revision_walk()` leaves in `pending`, plus the trees the
    // commit loop appends to it.
    let mut objects: Vec<ObjectId> = Vec::new();

    // Phase 1a: `handle_commit()` per pending object.
    for id in pending {
        let mut id = id;
        // Bounded like git's own peel, so a cyclic tag chain cannot spin.
        for _ in 0..16 {
            let Ok(object) = repo.find_object(id) else { break };
            match object.kind {
                Kind::Tag => {
                    // `add_pending_object(revs, object, tag->tag)`: the tag
                    // object itself joins `pending` before its target is followed.
                    if seen.insert(id) {
                        objects.push(id);
                    }
                    let tag = object.into_tag();
                    let Ok(target) = tag.decode().map(|t| t.target()) else { break };
                    id = target;
                }
                Kind::Commit => {
                    if seen.insert(id) {
                        let date = commit_date(repo, id);
                        queue.push((id, date));
                    }
                    break;
                }
                Kind::Tree | Kind::Blob => {
                    if seen.insert(id) {
                        objects.push(id);
                    }
                    break;
                }
            }
        }
    }

    // `commit_list_sort_by_date()` — a stable merge sort, newest first.
    queue.sort_by(|a, b| b.1.cmp(&a.1));

    // Phase 1b: `do_traverse()`'s commit loop.
    let mut at = 0usize;
    while at < queue.len() {
        let (id, _) = queue[at];
        at += 1;
        let Ok(object) = repo.find_object(id) else { continue };
        let Ok(commit) = object.into_commit().decode().map(|c| (c.tree(), c.parents().collect::<Vec<_>>()))
        else {
            continue;
        };
        let (tree, parents) = commit;
        // `add_pending_tree()` runs *before* `show_commit()`, but the tree joins
        // `pending` while the commit joins the output, so the two never interleave.
        if seen.insert(tree) {
            objects.push(tree);
        }
        out.push(id);
        for parent in parents {
            if !seen.insert(parent) {
                continue;
            }
            let date = commit_date(repo, parent);
            // `commit_list_insert_by_date()`: before the first entry with a
            // strictly smaller date, i.e. after every entry of equal date. Only
            // the unpopped tail is a list in git, so the search starts there.
            let pos = queue[at..]
                .iter()
                .position(|(_, d)| *d < date)
                .map_or(queue.len(), |i| at + i);
            queue.insert(pos, (parent, date));
        }
    }

    // Phase 2: `traverse_trees_and_blobs()`. `objects` grows only in phase 1, so
    // the index walk is over a fixed list; subtrees are expanded in place.
    for id in objects {
        expand_tree(repo, id, &mut seen, &mut out);
    }
    out
}

/// `process_tree()`/`process_blob()`: show `id`, and when it is a tree descend
/// into it in entry order, recursing into each subtree as it is met. A gitlink
/// names a commit in another repository and is not followed, exactly as
/// `process_tree_contents()` skips it.
fn expand_tree(repo: &gix::Repository, id: ObjectId, seen: &mut HashSet<ObjectId>, out: &mut Vec<ObjectId>) {
    let Ok(object) = repo.find_object(id) else { return };
    let kind = object.kind;
    out.push(id);
    if kind != gix::object::Kind::Tree {
        return;
    }
    let Ok(tree) = gix::objs::TreeRef::from_bytes(&object.data, repo.object_hash()) else {
        return;
    };
    // The borrow of `object` has to end before the recursion re-enters the odb.
    let entries: Vec<(ObjectId, bool)> = tree
        .entries
        .iter()
        .filter(|e| e.mode.kind() != gix::objs::tree::EntryKind::Commit)
        .map(|e| (e.oid.to_owned(), e.mode.is_tree()))
        .collect();
    drop(object);
    for (child, _) in entries {
        if seen.insert(child) {
            expand_tree(repo, child, seen, out);
        }
    }
}

/// The committer date the date-ordered commit list is built on. A commit that
/// will not decode sorts as if it were epoch-old, which is where git's own
/// `parse_commit()` failure leaves it.
fn commit_date(repo: &gix::Repository, id: ObjectId) -> i64 {
    repo.find_object(id)
        .ok()
        .and_then(|o| o.try_into_commit().ok())
        .and_then(|c| c.committer().ok().map(|c| c.seconds()))
        .unwrap_or(0)
}

/// Every object id named by any reflog in this repository, old and new.
///
/// Null ids (a ref's creation or deletion line) name no object and are skipped,
/// as git's `parse_object()` returns NULL for them.
fn reflog_objects(repo: &gix::Repository) -> Vec<ObjectId> {
    let mut out = Vec::new();
    let null = ObjectId::null(repo.object_hash());
    let mut dirs = vec![repo.common_dir().join("logs")];
    let per_worktree = repo.git_dir().join("logs");
    if per_worktree != dirs[0] {
        dirs.push(per_worktree);
    }

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for dir in &dirs {
        collect_files(dir, &mut files);
    }
    for file in files {
        let Ok(buf) = std::fs::read(&file) else { continue };
        for line in gix::refs::file::log::iter::forward(&buf) {
            let Ok(line) = line else { continue };
            for id in [line.previous_oid(), line.new_oid()] {
                if id != null {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// Every regular file under `dir`, recursively.
fn collect_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_files(&path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

/// Add every valid cache-tree id, recursively. A section with no entry count is
/// invalid and its id meaningless, which git skips via `entry_count >= 0`.
fn push_cache_tree(tree: &gix::index::extension::Tree, out: &mut Vec<ObjectId>) {
    if tree.num_entries.is_some() {
        out.push(tree.id);
    }
    for child in &tree.children {
        push_cache_tree(child, out);
    }
}

/// The object ids held by the packs named on stdin, one name per line.
///
/// git accepts a pack's index name, its data name, or the bare base name; all
/// three are resolved against `objects/pack`.
fn objects_in_named_packs(repo: &gix::Repository, stdin: &[u8]) -> Vec<ObjectId> {
    let dir = repo.objects.store_ref().path().join("pack");
    let hash = repo.object_hash();
    let mut out = Vec::new();
    for line in stdin.split(|b| *b == b'\n') {
        let Ok(name) = std::str::from_utf8(line) else { continue };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let base = name
            .strip_suffix(".idx")
            .or_else(|| name.strip_suffix(".pack"))
            .unwrap_or(name);
        let Ok(index) = pack::index::File::at(dir.join(format!("{base}.idx")), hash) else {
            continue;
        };
        out.extend((0..index.num_objects()).map(|n| index.oid_at_index(n).to_owned()));
    }
    out
}

/// Every loose object in this repository's own object directory, in fan-out
/// order. Alternates are deliberately excluded: a loose object there is not this
/// repository's to pack, which is what `--local` means and what `--unpacked`
/// assumes.
fn loose_objects(repo: &gix::Repository) -> Vec<ObjectId> {
    let root = repo.objects.store_ref().path();
    let hex_len = repo.object_hash().len_in_hex();
    let mut out = Vec::new();
    let Ok(fanout) = std::fs::read_dir(root) else {
        return out;
    };
    for dir in fanout.flatten() {
        let prefix = dir.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let rest = entry.file_name().to_string_lossy().into_owned();
            if rest.len() + 2 != hex_len {
                continue;
            }
            if let Ok(id) = ObjectId::from_hex(format!("{prefix}{rest}").as_bytes()) {
                out.push(id);
            }
        }
    }
    out
}

/// Drop `--filter`ed objects from the set.
///
/// git evaluates a filter during the reachability walk, so a filtered-out tree
/// also hides everything below it. Applying it afterwards agrees for every spec
/// implemented here — `tree:0` removes all trees *and* all blobs, which is the
/// same closure — and specs that need the walk (`sparse:oid=`, `combine:`) are
/// left as no-ops rather than approximated.
pub(super) fn apply_filter(repo: &gix::Repository, spec: Option<&str>, ids: &mut Vec<ObjectId>) {
    use gix::object::Kind;
    let Some(spec) = spec else { return };

    let kind_of = |id: &ObjectId| repo.find_object(*id).ok().map(|o| o.kind);
    let size_of = |id: &ObjectId| repo.find_object(*id).ok().map(|o| o.data.len() as u64);

    if spec == "blob:none" {
        ids.retain(|id| kind_of(id) != Some(Kind::Blob));
    } else if let Some(limit) = spec.strip_prefix("blob:limit=") {
        let Some(limit) = magnitude(limit) else { return };
        ids.retain(|id| kind_of(id) != Some(Kind::Blob) || size_of(id).is_some_and(|n| n <= limit));
    } else if let Some(depth) = spec.strip_prefix("tree:") {
        // Only depth 0 is expressible without the walk's depth bookkeeping, and
        // it is the only depth in common use.
        if depth == "0" {
            ids.retain(|id| matches!(kind_of(id), Some(Kind::Commit | Kind::Tag)));
        }
    } else if let Some(want) = spec.strip_prefix("object:type=") {
        let want = match want {
            "blob" => Some(Kind::Blob),
            "tree" => Some(Kind::Tree),
            "commit" => Some(Kind::Commit),
            "tag" => Some(Kind::Tag),
            _ => None,
        };
        if let Some(want) = want {
            ids.retain(|id| kind_of(id) == Some(want));
        }
    }
}

/// git's `k`/`m`/`g` magnitude grammar, as `blob:limit=` uses it.
pub(super) fn magnitude(v: &str) -> Option<u64> {
    let (body, scale) = match v.chars().last() {
        Some('k' | 'K') => (&v[..v.len() - 1], 1024),
        Some('m' | 'M') => (&v[..v.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&v[..v.len() - 1], 1024 * 1024 * 1024),
        _ => (v, 1),
    };
    body.parse::<u64>().ok()?.checked_mul(scale)
}

/// Remove repeats while keeping first-seen order, which is the order objects are
/// written to the pack.
fn dedup(ids: &mut Vec<ObjectId>) {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.retain(|id| seen.insert(*id));
}

/// git's on-disk identifier for a hash function, as the `.rev` header carries it.
pub(super) fn hash_id(kind: gix::hash::Kind) -> u32 {
    match kind {
        gix::hash::Kind::Sha1 => 1,
        _ => 2,
    }
}

/// Append the hash of everything written so far, which is how every one of
/// git's pack artifacts terminates.
pub(super) fn append_checksum(bytes: &mut Vec<u8>, kind: gix::hash::Kind) -> Result<()> {
    let mut hasher = gix::hash::hasher(kind);
    hasher.update(&bytes[..]);
    bytes.extend_from_slice(hasher.try_finalize()?.as_slice());
    Ok(())
}

/// `odb_mkstemp(&tmpname, "pack/tmp_<kind>_XXXXXX")` — the temporary an artifact
/// is built under before it is renamed into place. `<kind>` is the destination's
/// extension, exactly as git names them: `tmp_pack_`, `tmp_idx_`, `tmp_rev_`,
/// `tmp_mtimes_`.
///
/// The suffix only has to be unique within this run; `mkstemp`'s six random
/// characters and this pid/slot pair are equally arbitrary to every reader.
fn temp_artifact_path(pack_dir: &std::path::Path, dest: &str, slot: usize) -> std::path::PathBuf {
    let kind = std::path::Path::new(dest)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("pack");
    pack_dir.join(format!("tmp_{kind}_{}{slot}", std::process::id()))
}

/// Write one pack artifact to its temporary, reporting a failure the way git
/// does: a path it cannot create is diagnosed twice, once for the write and once
/// for the rename that never happened. `dest` names the file in the diagnostic
/// even though `tmp` is what is being written, because that is the name git's
/// message carries.
fn write_artifact(
    tmp: &std::path::Path,
    dest: &str,
    bytes: &[u8],
    fsync: &crate::config::FsyncPolicy,
    component: crate::config::FsyncComponent,
) -> Option<ExitCode> {
    let path = tmp;
    let _ = std::fs::remove_file(path);
    match std::fs::write(path, bytes) {
        // git leaves `.pack`, `.idx`, `.rev` and `.mtimes` world-readable but
        // immutable. A filesystem that refuses the mode is not fatal — git does
        // not check either.
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt;
            // Harden before the mode change, while the file is still writable:
            // `core.fsync=pack` covers the pack itself and `pack-metadata` its
            // `.idx`/`.rev`/`.mtimes` companions, which is the split git uses.
            fsync.harden_path(component, path);
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
            None
        }
        Err(err) => {
            eprintln!("error: unable to write file {dest}: {}", errno_text(&err));
            eprintln!("fatal: unable to rename temporary file to '{dest}'");
            Some(ExitCode::from(128))
        }
    }
}

/// `strerror(errno)` on its own, which is what git's `%s` of `strerror` prints.
/// Rust appends its own ` (os error <n>)` to the same text; that suffix is the
/// only difference, so removing it leaves git's string exactly.
fn errno_text(err: &std::io::Error) -> String {
    let rendered = err.to_string();
    match rendered.find(" (os error ") {
        Some(at) => rendered[..at].to_string(),
        None => rendered,
    }
}

/// Walk `args` exactly the way git's parse-options walks them, emitting git's
/// diagnostics verbatim on the first entry it rejects.
fn parse(args: &[String]) -> Parsed {
    let mut st = State::default();
    // git's `cmd_pack_objects` starts with progress on and clears it when stderr
    // is not a terminal, so a run on a terminal reports without being asked and a
    // piped one stays silent. `-q` and `--progress` then override, last one wins.
    st.progress = crate::progress::enabled(false);
    let mut end_of_opts = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        if end_of_opts || !a.starts_with('-') || a == "-" {
            st.positionals.push(a.to_string());
            i += 1;
            continue;
        }

        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }

        if let Some(body) = a.strip_prefix("--") {
            // `if (internal_help && !strcmp(arg + 2, "help-all"))`
            // (parse-options.c:1124): checked before any table lookup, at
            // whatever position the option loop has reached, and rendered
            // `USAGE_FULL` — which for `pack-objects` means the block with the
            // hidden `--write-bitmap-index-quiet` left in.
            if body == "help-all" {
                return Parsed::Exit(super::show_usage(USAGE_ALL));
            }
            match long_opt(body, args, &mut i, &mut st) {
                Some(code) => return Parsed::Exit(code),
                None => continue,
            }
        }

        // Clustered short switches; `pack-objects` declares only `-q` (plus the
        // implicit `-h`).
        match short_opts(&a[1..], &mut i, &mut st) {
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

    let (idx, negated) = match resolve_long(body) {
        Resolved::Unique(idx, negated) => (idx, negated),
        Resolved::Ambiguous(first, second) => {
            // Verified quirk: unlike every other diagnostic here, the ambiguity
            // message goes to stderr while its usage block goes to *stdout* —
            // and it echoes the argument as typed, `=<value>` and all.
            return Some(super::ambiguous_option(body, &first, &second, USAGE));
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
        set_long(def.long, None, false, st);
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
    }

    set_long(def.long, value, true, st);
    *i += 1;
    None
}

/// Validate a value against the option's parse-options type and, for the four
/// options git validates in a callback, against that callback's own grammar.
///
/// The type diagnostics exit 129; the callback ones are `die()`s and exit 128.
/// Both fire during the parse walk, so they are reported in argv order and
/// before the no-output usage check.
fn check_value(def: &OptDef, shown: &str, v: &str) -> Option<ExitCode> {
    // The shared parse-options grammar: base 0 and a `k`/`m`/`g` suffix, with
    // git's bound for the option's target — `int` for `OPT_INTEGER`, `size_t`
    // for the byte counts, which is why the latter's range clause reads `[0,-1]`.
    let name = crate::optint::long_opt(shown);
    match def.kind {
        Kind::Int => {
            if let Err(e) = crate::optint::integer(&name, v) {
                eprintln!("error: {e}");
                return Some(ExitCode::from(129));
            }
        }
        Kind::Magnitude => {
            if let Err(e) = crate::optint::unsigned(&name, v) {
                eprintln!("error: {e}");
                return Some(ExitCode::from(129));
            }
        }
        _ => {}
    }

    match def.long {
        "index-version" => check_index_version(v),
        "missing" if !MISSING_ACTIONS.contains(&v) => {
            Some(fatal(&format!("invalid value for '--missing': '{v}'")))
        }
        "stdin-packs" if !STDIN_PACKS_MODES.contains(&v) => {
            Some(fatal(&format!("invalid value for 'stdin-packs': '{v}'")))
        }
        "filter" => check_filter_spec(v),
        _ => None,
    }
}

/// git's `--filter` callback (`OPT_PARSE_LIST_OBJECTS_FILTER` →
/// `gently_parse_list_objects_filter`), which validates the spec while parsing
/// and `die()`s (exit 128) on the first rejection, in argv order — before the
/// no-output usage check ever runs. `None` when git accepts the spec.
///
/// Ported from git 2.55.0 `list-objects-filter-options.c`, with pack-objects'
/// `allow_auto_filter = false`. Only validation is ported here; how an accepted
/// spec then shapes the object set is [`apply_filter`]'s job.
fn check_filter_spec(spec: &str) -> Option<ExitCode> {
    gently_parse_filter(spec.as_bytes()).err().map(|m| fatal(&m))
}

/// `gently_parse_list_objects_filter`: match the spec against git's fixed set of
/// filter forms, in git's declaration order (which decides which diagnostic a
/// near-miss like `blob:` or `object:` gets). `Err(msg)` carries the exact text
/// git puts after `fatal: `.
pub(super) fn gently_parse_filter(arg: &[u8]) -> Result<(), String> {
    // pack-objects does not set `allow_auto_filter`, so `auto` is always refused.
    if arg == b"auto" {
        return Err("'auto' filter not supported by this command".to_string());
    }
    if arg == b"blob:none" {
        return Ok(());
    }
    if let Some(v0) = arg.strip_prefix(b"blob:limit=".as_slice()) {
        // A bad magnitude is not its own diagnostic: git falls out of the
        // if/else chain to the generic `invalid filter-spec` at the bottom.
        if git_parse_ulong(v0).is_some() {
            return Ok(());
        }
    } else if let Some(v0) = arg.strip_prefix(b"tree:".as_slice()) {
        if git_parse_ulong(v0).is_none() {
            return Err("expected 'tree:<depth>'".to_string());
        }
        return Ok(());
    } else if arg.strip_prefix(b"sparse:oid=".as_slice()).is_some() {
        // Any oid name is accepted at parse time; resolution happens later.
        return Ok(());
    } else if arg.strip_prefix(b"sparse:path=".as_slice()).is_some() {
        return Err("sparse:path filters support has been dropped".to_string());
    } else if let Some(v0) = arg.strip_prefix(b"object:type=".as_slice()) {
        if !is_object_type(v0) {
            return Err(format!(
                "'{}' for 'object:type=<type>' is not a valid object type",
                String::from_utf8_lossy(v0)
            ));
        }
        return Ok(());
    } else if let Some(v0) = arg.strip_prefix(b"combine:".as_slice()) {
        return parse_combine_filter(v0);
    }

    Err(format!(
        "invalid filter-spec '{}'",
        String::from_utf8_lossy(arg)
    ))
}

/// `parse_combine_filter`: split on `+` into sub-filters (each of which is
/// parsed recursively), tolerating empty segments so a leading or trailing `+`
/// is accepted. An empty body is the one combine-specific error.
fn parse_combine_filter(arg: &[u8]) -> Result<(), String> {
    if arg.is_empty() {
        return Err("expected something after combine:".to_string());
    }
    let mut p = arg;
    loop {
        let end = p.iter().position(|&c| c == b'+').unwrap_or(p.len());
        let sub = &p[..end];
        if !sub.is_empty() {
            parse_combine_subfilter(sub)?;
        }
        if end == p.len() {
            break;
        }
        p = &p[end + 1..];
        if p.is_empty() {
            break;
        }
    }
    Ok(())
}

/// `parse_combine_subfilter`: percent-decode the segment, reject any reserved
/// character in the *raw* segment, then parse the decoded bytes recursively. The
/// `LOFC_AUTO` combine check git runs afterwards is unreachable here, since a
/// bare `auto` sub-filter is already refused by [`gently_parse_filter`].
fn parse_combine_subfilter(subspec: &[u8]) -> Result<(), String> {
    let decoded = url_percent_decode(subspec);
    if let Some(c) = has_reserved_character(subspec) {
        return Err(format!("must escape char in sub-filter-spec: '{c}'"));
    }
    gently_parse_filter(&decoded)
}

/// git's `RESERVED_NON_WS` set plus every byte at or below a space: the first
/// such byte in `sub` is the one git names in its escape diagnostic.
fn has_reserved_character(sub: &[u8]) -> Option<char> {
    const RESERVED_NON_WS: &[u8] = br#"~`!@#$^&*()[]{}\;'",<>?"#;
    sub.iter()
        .copied()
        .find(|&c| c <= b' ' || RESERVED_NON_WS.contains(&c))
        .map(|c| c as char)
}

/// `type_from_string_gently`, case-sensitively: the four named object types git
/// accepts after `object:type=`.
fn is_object_type(v: &[u8]) -> bool {
    matches!(v, b"commit" | b"tree" | b"blob" | b"tag")
}

/// `url_percent_decode` (`decode_plus = 0`): decode `%XX` where both digits are
/// hex and the byte is non-zero, and copy every other byte through unchanged —
/// which is exactly how git leaves a truncated or malformed `%` in place.
fn url_percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 3 <= s.len() {
            if let (Some(h), Some(l)) = (hexval(s[i + 1]), hexval(s[i + 2])) {
                let byte = (h << 4) | l;
                if byte > 0 {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// One hex digit's value, or `None` — the `hex2chr` half git's decoder uses.
fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// git's `git_parse_ulong` (via `git_parse_unsigned`), as `blob:limit=` and
/// `tree:` consume their value: the string must be non-empty and hold no `-`
/// anywhere, a base-0 `strtoumax` must convert at least one digit without
/// overflowing an `unsigned long`, and any trailing unit must be one of
/// `k`/`m`/`g` (either case). `None` is git's "0 return" (rejected value).
///
/// The `unsigned long` ceiling is 64-bit on every target this builds for, so
/// only the multiply can overflow `max`; `checked_mul` stands in for git's
/// `unsigned_mult_overflows` / `> max` pair.
fn git_parse_ulong(value: &[u8]) -> Option<u64> {
    if value.is_empty() || value.contains(&b'-') {
        return None;
    }
    let (val, end) = strtoumax_base0(value)?;
    let factor = unit_factor(end)?;
    val.checked_mul(factor)
}

/// `get_unit_factor`: an empty tail is a factor of one, `k`/`m`/`g` scale by
/// 2^10/2^20/2^30, and anything else is git's `0` (an invalid value).
fn unit_factor(end: &[u8]) -> Option<u64> {
    match end {
        b"" => Some(1),
        b"k" | b"K" => Some(1024),
        b"m" | b"M" => Some(1024 * 1024),
        b"g" | b"G" => Some(1024 * 1024 * 1024),
        _ => None,
    }
}

/// C's `strtoumax(value, &end, 0)` over the prefix git's numeric parser reads:
/// skip leading ASCII whitespace and an optional sign, auto-detect the base
/// (`0x` hex, a leading `0` octal, else decimal), and consume digits. Returns
/// the converted value and the unconsumed tail, or `None` when no digit was
/// converted or the magnitude overflows `u64` (git's `ERANGE`).
///
/// git rejects any `-` before this runs, so the negative branch is defensive
/// only; it wraps the way C would rather than inventing a value.
fn strtoumax_base0(value: &[u8]) -> Option<(u64, &[u8])> {
    let mut i = 0;
    while i < value.len() && value[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut negative = false;
    if i < value.len() && (value[i] == b'+' || value[i] == b'-') {
        negative = value[i] == b'-';
        i += 1;
    }

    let (base, start) = if value.len() > i + 2
        && value[i] == b'0'
        && (value[i + 1] | 0x20) == b'x'
        && value[i + 2].is_ascii_hexdigit()
    {
        (16u64, i + 2)
    } else if i < value.len() && value[i] == b'0' {
        (8u64, i)
    } else {
        (10u64, i)
    };

    let mut j = start;
    let mut val: u64 = 0;
    let mut overflow = false;
    while j < value.len() {
        let Some(d) = hexval(value[j]).map(u64::from).filter(|&d| d < base) else {
            break;
        };
        match val.checked_mul(base).and_then(|v| v.checked_add(d)) {
            Some(v) => val = v,
            None => overflow = true,
        }
        j += 1;
    }
    if j == start || overflow {
        return None;
    }
    if negative {
        val = 0u64.wrapping_sub(val);
    }
    Some((val, &value[j..]))
}

/// git's `parse_index_version()` callback, which is `strtoul`-shaped rather than
/// parse-options-shaped: the number is read greedily, an optional `,<offset>`
/// tail follows, and anything left over is an error. Both diagnostics quote the
/// argument as written, which is why `--index-version=-1` reports "unsupported"
/// (the unsigned read wraps past 2) rather than "bad".
fn check_index_version(v: &str) -> Option<ExitCode> {
    let (version, rest) = strtoul(v);
    if version > 2 {
        return Some(fatal(&format!("unsupported index version {v}")));
    }

    // The `,<offset>` tail is only read when a digit could follow the comma; a
    // bare trailing comma is left in `rest` and reported as a bad version.
    let (off32_limit, rest) = match rest.strip_prefix(',').filter(|t| !t.is_empty()) {
        Some(tail) => strtoul(tail),
        None => (0, rest),
    };
    if !rest.is_empty() || off32_limit & 0x8000_0000 != 0 {
        return Some(fatal(&format!("bad index version '{v}'")));
    }
    None
}

/// C's `strtoul` over a base-10 prefix of `s`: an optional sign, then digits,
/// wrapping on overflow and on a negative sign. Returns the value and the
/// unconsumed remainder (which is all of `s` when there are no digits).
fn strtoul(s: &str) -> (u64, &str) {
    let (negative, digits_at) = match s.as_bytes().first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    };
    let digits: String = s[digits_at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return (0, s);
    }

    let mut value: u64 = 0;
    for c in digits.chars() {
        value = value
            .wrapping_mul(10)
            .wrapping_add(u64::from(c as u8 - b'0'));
    }
    if negative {
        value = 0u64.wrapping_sub(value);
    }
    (value, &s[digits_at + digits.len()..])
}

/// Record the effect of long option `long`; `on` is false for the `--no-` form.
///
/// Only the flags the post-parse checks consult are tracked.
fn set_long(long: &str, value: Option<&str>, on: bool, st: &mut State) {
    match long {
        "stdout" => st.stdout = on,
        "thin" => st.thin = on,
        // git's `--cruft-expiration` callback sets the `cruft` flag itself, so
        // the expiration alone is enough to reach every `--cruft` diagnostic,
        // and `--no-cruft-expiration` clears it again.
        "cruft" | "cruft-expiration" => st.cruft = on,
        "stdin-packs" => st.stdin_packs = on,
        "unpacked" => st.unpacked = on,
        "incremental" => st.incremental = on,
        "non-empty" => st.non_empty = on,
        // Both write into git's single `write_bitmap_index`; the hidden one
        // stores `WRITE_BITMAP_QUIET` rather than `WRITE_BITMAP_TRUE`, and every
        // `if (write_bitmap_index)` in pack-objects.c treats the two alike. The
        // one place they differ (pack-objects.c:1416, :1883) only decides whether
        // a `warning()` is printed when bitmaps have to be turned back off, and
        // this port emits neither warning.
        "write-bitmap-index" | "write-bitmap-index-quiet" => st.write_bitmap_index = on,
        "delta-islands" => st.delta_islands = on,
        // The three `OPT_BOOL`s whose "unset" state the post-parse defaults read
        // apart from their false state, so each is recorded as a tri-state.
        "sparse" => st.sparse = Some(on),
        "path-walk" => st.path_walk = Some(on),
        "use-bitmap-index" => st.use_bitmap_index = Some(on),
        "quiet" => st.progress = !on,
        "progress" | "all-progress" => st.progress = on,
        "exclude-promisor-objects" => st.exclude_promisor = on,
        "exclude-promisor-objects-best-effort" => st.exclude_promisor_best_effort = on,
        "keep-unreachable" => {
            st.keep_unreachable = on;
            st.internal_rev_list |= on;
        }
        "unpack-unreachable" => {
            st.unpack_unreachable = on;
            st.internal_rev_list |= on;
        }
        "all" => {
            st.all = on;
            st.internal_rev_list |= on;
        }
        "reflog" => {
            st.reflog = on;
            st.internal_rev_list |= on;
        }
        "indexed-objects" => {
            st.indexed_objects = on;
            st.internal_rev_list |= on;
        }
        "revs" => {
            st.revs = on;
            st.internal_rev_list |= on;
        }
        "pack-loose-unreachable" => st.internal_rev_list |= on,
        "filter" => st.filter = on.then(|| value.unwrap_or("").to_string()),
        "compression" => st.compression = on.then(|| to_number(value.unwrap_or("0"))).flatten(),
        "name-hash-version" => {
            st.name_hash_version = on.then(|| to_number(value.unwrap_or("0"))).flatten();
        }
        "max-pack-size" => st.max_pack_size = on.then(|| to_number(value.unwrap_or("0"))).flatten(),
        // `--no-window`/`--no-depth`/`--no-threads` are `OPT_INTEGER` negations,
        // which parse-options resolves to zero rather than to "unset".
        "window" => st.window = Some(if on { to_number(value.unwrap_or("0")).unwrap_or(0) } else { 0 }),
        "depth" => st.depth = Some(if on { to_number(value.unwrap_or("0")).unwrap_or(0) } else { 0 }),
        "window-memory" => {
            st.window_memory = on
                .then(|| to_number(value.unwrap_or("0")))
                .flatten()
                .and_then(|n| u64::try_from(n).ok());
        }
        "threads" => st.threads = Some(if on { to_number(value.unwrap_or("0")).unwrap_or(0) } else { 0 }),
        "delta-base-offset" => st.delta_base_offset = Some(on),
        "reuse-delta" => st.reuse_delta = Some(on),
        // Already validated by `check_index_version`, so the `strtoul` prefix is
        // the version and the rest is the `,<offset>` tail.
        "index-version" => st.index_version = value.map(|v| strtoul(v).0),
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

/// Resolve `body` — everything after `--`, **including** any `=<value>` — through
/// the shared `parse_long_opt()` port, and translate the entry it picked back
/// into an index into [`OPTS`].
///
/// The `=<value>` must be passed through rather than split off first: git's
/// `starts_with("no-", arg)` test (parse-options.c:569) reads the *whole*
/// argument, so `--n` names every negatable option while `--n=x` names only the
/// two whose spelling starts with `n`. Splitting first collapsed those into one
/// answer and made `git pack-objects --n=x` report a pair stock git does not
/// (`--name-hash-version or --no-name-hash-version` where stock says
/// `--non-empty or --name-hash-version`).
fn resolve_long(body: &str) -> Resolved {
    match super::resolve_long(LONG_OPTS, body) {
        super::Resolved::One(opt, negated) => {
            let idx = LONG_OPTS
                .iter()
                .position(|o| std::ptr::eq(o, opt))
                .expect("the entry came from LONG_OPTS");
            Resolved::Unique(idx, negated)
        }
        super::Resolved::Ambiguous(first, second) => Resolved::Ambiguous(first, second),
        super::Resolved::Unknown => Resolved::Unknown,
    }
}

/// Handle one clustered short-switch entry (`cluster` excludes the leading `-`).
/// `-q` is the only declared switch; `-h` is parse-options' built-in.
fn short_opts(cluster: &str, i: &mut usize, st: &mut State) -> Option<ExitCode> {
    for c in cluster.chars() {
        match c {
            'h' => {
                print!("{USAGE}");
                return Some(ExitCode::from(129));
            }
            // `-q` and `--progress` write the same flag, so the last one wins.
            'q' => st.progress = false,
            other => {
                eprint!("error: unknown switch `{other}'\n{USAGE}");
                return Some(ExitCode::from(129));
            }
        }
    }
    *i += 1;
    None
}

/// The integer value of a string already accepted by [`check_value`], read with
/// the same shared grammar so `--window=0x10` is the sixteen git makes of it.
/// This is the number git's diagnostics print.
fn to_number(v: &str) -> Option<i64> {
    crate::optint::integer_prec(&crate::optint::long_opt(""), v, 8).ok()
}

/// Everything stock git checks after parsing and before it does any work, in
/// git's own order (each ordering below is pinned by an observed invocation).
///
/// The first check prints the bare usage block on stderr and exits 129; the rest
/// are `die()`s and exit 128.
fn preflight(st: &State, settings: Option<&crate::repo_settings::RepoSettings>) -> Option<ExitCode> {
    // `pack_to_stdout != !base_name`, plus git's rejection of a second
    // positional. Beats every `fatal:` below: `--compression=99` on its own
    // reports usage, not a bad compression level.
    if st.stdout == (st.positionals.len() == 1) || st.positionals.len() > 1 {
        eprint!("{USAGE}");
        return Some(ExitCode::from(129));
    }

    // `builtin/pack-objects.c:5216-5228`, which sits between the usage check
    // above (:5185) and the `bad pack compression level` die below (:5284):
    //
    // ```c
    // if (path_walk) {
    //         const char *option = NULL;
    //         if (!path_walk_filter_compatible(&filter_options))
    //                 option = "--filter";
    //         else if (use_delta_islands)
    //                 option = "--delta-islands";
    //         if (option) {
    //                 warning(_("cannot use %s with %s"), option, "--path-walk");
    //                 path_walk = 0;
    //         }
    // }
    // ```
    //
    // The walk itself is one of this module's stated gaps, so turning it back
    // off changes nothing here — but the warning does not depend on the walk,
    // and it is reachable from `pack.usePathWalk` alone, which is what makes
    // that key honoured rather than merely validated. `--filter` wins over
    // `--delta-islands` when both would fire, and the filter's own
    // `error: tree:<n> filter not supported by the path-walk API` is printed
    // first, from inside the compatibility check.
    if resolve_path_walk(st, settings) {
        let incompatible = match st.filter.as_deref() {
            Some(spec) => match path_walk_filter_compatible(spec.as_bytes()) {
                Err(depth) => {
                    eprintln!("error: tree:{depth} filter not supported by the path-walk API");
                    Some("--filter")
                }
                Ok(()) => st.delta_islands.then_some("--delta-islands"),
            },
            None => st.delta_islands.then_some("--delta-islands"),
        };
        if let Some(option) = incompatible {
            eprintln!("warning: cannot use {option} with --path-walk");
        }
    }

    // Beats `--thin`: `pack-objects base --thin --compression=99` reports the
    // compression level.
    if let Some(level) = st.compression {
        if !(-1..=9).contains(&level) {
            return Some(fatal(&format!("bad pack compression level {level}")));
        }
    }

    // Beats `--thin` and everything after it, and loses to the compression
    // level: `--stdout --max-pack-size=1m --compression=99` reports the
    // compression level, while `--stdout --max-pack-size=1m --thin` and
    // `--stdout --max-pack-size=1m --cruft --revs` both report this. A zero size
    // is git's "unset", so it does not trip the check.
    if st.max_pack_size.is_some_and(|n| n != 0) && st.stdout {
        return Some(fatal("--max-pack-size cannot be used to build a pack for transfer"));
    }

    // Beats the conflicts below: `pack-objects base --thin --cruft --revs`
    // reports the thin pack.
    if st.thin && !st.stdout {
        return Some(fatal("--thin cannot be used to build an indexable pack"));
    }

    // Beats the rev-list checks: adding `--cruft --revs` to this pair still
    // reports the pair.
    if st.keep_unreachable && st.unpack_unreachable {
        return Some(fatal(
            "options '--keep-unreachable' and '--unpack-unreachable' cannot be used together",
        ));
    }

    // `--unpacked` is deliberately absent from this condition: it is the one
    // rev-list-implying option documented as compatible with `--stdin-packs`,
    // and `--stdout --stdin-packs --unpacked` is accepted.
    if st.stdin_packs && st.rev_list_at_stdin_packs_check() {
        return Some(fatal("cannot use internal rev list with --stdin-packs"));
    }

    if st.stdin_packs && st.cruft {
        return Some(fatal(
            "options '--stdin-packs' and '--cruft' cannot be used together",
        ));
    }

    // Here `--unpacked` does count: `--stdout --cruft --unpacked` is rejected.
    // So does `--exclude-promisor-objects`, which has turned the internal rev
    // list on by the time this check runs even though it had not yet when the
    // `--stdin-packs` one above did.
    if st.cruft && (st.rev_list_at_cruft_check() || st.unpacked) {
        return Some(fatal("cannot use internal rev list with --cruft"));
    }

    // Last: `--stdout --name-hash-version=9 --cruft --revs` reports the cruft
    // conflict. A negative value selects git's default and is accepted.
    if let Some(version) = st.name_hash_version {
        if version >= 0 && !(1..=2).contains(&version) {
            return Some(fatal(&format!(
                "invalid --name-hash-version option: {version}"
            )));
        }
    }

    None
}

/// git's `die()` shape: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

#[cfg(test)]
mod tests {
    use super::{Kind, Resolved, LONG_OPTS, OPTS, USAGE, USAGE_ALL};

    /// The two usage blocks differ by exactly the one `PARSE_OPT_HIDDEN` entry.
    ///
    /// They are separate literals because each has to be byte-identical to what
    /// stock git prints (4170 and 4265 bytes for 2.55.0), and a copy-paste edit
    /// to one of them is invisible until a differential run. Deriving the
    /// relationship here is the check: `--help-all` is `-h` plus two lines, in
    /// the position the table puts them, and nothing else.
    #[test]
    fn help_all_is_the_h_block_plus_the_hidden_entry() {
        const HIDDEN: &str = concat!(
            "    --[no-]write-bitmap-index-quiet\n",
            "                          write a bitmap index if possible\n",
        );
        let at = USAGE
            .find("    --[no-]filter <args>")
            .expect("the entry after the hidden one");
        let (head, tail) = USAGE.split_at(at);
        assert_eq!(USAGE_ALL, format!("{head}{HIDDEN}{tail}"));
        assert_eq!(USAGE.len(), 4170);
        assert_eq!(USAGE_ALL.len(), 4265);
    }

    /// Resolution runs off the shared `parse_long_opt()` port, and the entry it
    /// returns has to map back onto the [`OPTS`] row that says what the value
    /// does. The projection keeps the two index-parallel by construction; this
    /// pins that the mapping is actually round-tripping.
    #[test]
    fn every_name_resolves_to_its_own_row() {
        assert_eq!(LONG_OPTS.len(), OPTS.len());
        for (want, def) in OPTS.iter().enumerate() {
            match super::resolve_long(def.long) {
                Resolved::Unique(got, false) => assert_eq!(
                    got, want,
                    "--{} resolved to --{}",
                    def.long, OPTS[got].long
                ),
                _ => panic!("--{} did not resolve to itself", def.long),
            }
        }
    }

    /// The `PARSE_OPT_NONEG` entries of this table have no `--no-` spelling.
    ///
    /// Each `--no-` below was run against stock 2.55.0 and answered
    /// ``error: unknown option `no-<name>'`` at exit 129. `--no-all` is the
    /// deliberate exception: `--all` is `OPT_SET_INT_F(... PARSE_OPT_NONEG)` and
    /// contributes nothing, but `--all-progress` and `--all-progress-implied`
    /// are both negatable and both start with `all`, so stock answers
    /// ``ambiguous option: no-all (could be --no-all-progress or
    /// --no-all-progress-implied)``. A NONEG entry removes its own negation, not
    /// the negations of the names that happen to extend it.
    #[test]
    fn noneg_entries_have_no_negation() {
        for name in [
            "index-version",
            "max-pack-size",
            "window-memory",
            "unpacked",
            "reflog",
            "indexed-objects",
            "missing",
        ] {
            assert!(
                matches!(super::resolve_long(name), Resolved::Unique(..)),
                "--{name} should resolve"
            );
            assert!(
                matches!(super::resolve_long(&format!("no-{name}")), Resolved::Unknown),
                "--no-{name} must not resolve (the entry is PARSE_OPT_NONEG)"
            );
        }
        assert!(matches!(super::resolve_long("all"), Resolved::Unique(..)));
        match super::resolve_long("no-all") {
            Resolved::Ambiguous(a, b) => assert_eq!(
                (a.as_str(), b.as_str()),
                ("no-all-progress", "no-all-progress-implied")
            ),
            _ => panic!("--no-all names the two negatable options that extend `all'"),
        }
        // …and a negatable entry still negates, so the loop cannot pass by
        // resolving nothing at all.
        assert!(matches!(super::resolve_long("no-thin"), Resolved::Unique(..)));
    }

    /// `starts_with("no-", arg)` (parse-options.c:569) reads the argument
    /// *including* any `=<value>`, which is what separates these two answers.
    /// Both were taken from stock 2.55.0.
    #[test]
    fn an_attached_value_changes_which_names_are_ambiguous() {
        match super::resolve_long("n") {
            Resolved::Ambiguous(a, b) => {
                assert_eq!((a.as_str(), b.as_str()), ("name-hash-version", "no-name-hash-version"))
            }
            _ => panic!("--n names every negatable option at once"),
        }
        match super::resolve_long("n=x") {
            Resolved::Ambiguous(a, b) => {
                assert_eq!((a.as_str(), b.as_str()), ("non-empty", "name-hash-version"))
            }
            _ => panic!("--n=x names only the two spelled with a leading n"),
        }
    }

    /// The hidden entry is a real table row, so it shadows nothing and makes the
    /// visible `--write-bitmap-index` no longer reachable by abbreviation.
    #[test]
    fn the_hidden_entry_makes_write_bit_ambiguous() {
        match super::resolve_long("write-bit") {
            Resolved::Ambiguous(a, b) => assert_eq!(
                (a.as_str(), b.as_str()),
                ("write-bitmap-index", "write-bitmap-index-quiet")
            ),
            _ => panic!("--write-bit must be ambiguous"),
        }
        assert!(matches!(
            super::resolve_long("write-bitmap-index"),
            Resolved::Unique(..)
        ));
        let quiet = OPTS
            .iter()
            .find(|d| d.long == "write-bitmap-index-quiet")
            .expect("hidden entry present");
        assert!(quiet.kind == Kind::Bool, "the hidden entry takes no value");
    }
}
