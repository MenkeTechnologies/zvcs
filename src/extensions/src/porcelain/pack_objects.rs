//! `git pack-objects` — create a packed archive of objects.
//!
//! # Pack bytes differ from git's by design
//!
//! git's pack writer runs a delta search (`--window`, `--depth`) and emits
//! `OFS_DELTA` entries for whatever it finds, after sorting entries by type,
//! name-hash and size. The vendored `gix-pack` computes no deltas at all: its
//! only output mode is documented as "Copy base objects and deltas from packs,
//! while non-packed objects will be treated as base objects (i.e. without trying
//! to delta compress them)"
//! (`gix-pack/src/data/output/entry/iter_from_counts.rs:362-366`).
//!
//! So the pack written here is **well-formed and complete but undeltified**: it
//! holds exactly the objects git would pack, in a different order, at a larger
//! size, under a different trailing checksum — and therefore under a different
//! `<base-name>-<hash>.{pack,idx,rev}` filename. `git verify-pack`, `git
//! index-pack --verify` and `git unpack-objects` all accept it. What is *not*
//! reproduced is git's byte stream, which no amount of option handling can fix
//! without a delta compressor. Every artifact this module writes is therefore
//! correct-by-construction rather than byte-identical, and that tradeoff is
//! stated here rather than left for a reader to infer from a checksum mismatch.
//!
//! The same reasoning covers the knobs that exist purely to steer the delta
//! search or the encoding: `--window`, `--depth`, `--window-memory`,
//! `--no-reuse-delta`, `--no-reuse-object`, `--delta-base-offset`,
//! `--delta-islands`, `--threads`, `--name-hash-version`, `--path-walk`,
//! `--sparse` and `--shallow` are accepted and change nothing observable, since
//! there is no delta search for them to steer. `--thin` and
//! `--write-bitmap-index` are likewise accepted without effect: a thin pack is a
//! delta special case, and no EWAH bitmap writer exists in the vendored crates
//! (the string `bitmap` does not occur under `gix-pack/src`).
//!
//! # What is reproduced exactly
//!
//! * the object *set*: which objects end up in the pack, for `--all`,
//!   `--reflog`, `--indexed-objects`, `--revs`, `--stdin-packs`, `--unpacked`,
//!   `--cruft`, a bare object list on stdin, and every combination of those
//!   (see [`collect_counts`])
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
//!   * `pack.compression`/`core.compression` is not read from config, so the
//!     compression diagnostic fires only for a value given on the command line.
//!   * `--missing=allow-promisor` does not additionally imply
//!     `--exclude-promisor-objects` handling.
//!   * `--include-tag` adds no tags beyond those the object set already names.
//!   * `--cruft-expiration=<time>` is parsed but does not filter by mtime; every
//!     cruft object is written with its current mtime.

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
    OptDef { long: "filter", kind: Kind::Str, negatable: true },
    OptDef { long: "missing", kind: Kind::Str, negatable: false },
    OptDef { long: "exclude-promisor-objects", kind: Kind::Bool, negatable: true },
    OptDef { long: "exclude-promisor-objects-best-effort", kind: Kind::Bool, negatable: true },
    OptDef { long: "delta-islands", kind: Kind::Bool, negatable: true },
    OptDef { long: "uri-protocol", kind: Kind::Str, negatable: true },
    OptDef { long: "name-hash-version", kind: Kind::Int, negatable: true },
];

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
    /// `--filter=<spec>`, as given; see [`apply_filter`].
    filter: Option<String>,
    /// Whether the end-of-run summary goes to stderr: `-q` and `--progress`
    /// (and `--all-progress`) are last-one-wins.
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

    let state = match parse(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok(state) => state,
    };

    if let Some(code) = preflight(&state) {
        return Ok(code);
    }

    execute(&state)
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

    let counts = collect_counts(&repo, st, &stdin);

    // git skips the pack entirely rather than writing an empty one, and says so
    // by writing nothing at all.
    if counts.is_empty() && st.non_empty {
        return Ok(ExitCode::SUCCESS);
    }

    let packed = write_pack(&repo, &counts, compression(st))?;

    if st.stdout {
        let mut out = std::io::stdout().lock();
        out.write_all(&packed.bytes)?;
        out.flush()?;
        report_progress(st, packed.entries.len());
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
    by_oid.sort_unstable_by(|a, b| a.id.cmp(&b.id));

    let kind = repo.object_hash();
    let mut files = vec![
        (format!("{base}-{hex_id}.pack"), packed.bytes.clone()),
        (
            format!("{base}-{hex_id}.idx"),
            index_file(kind, index_version, &packed.id, &by_oid)?,
        ),
    ];
    if write_rev {
        files.push((
            format!("{base}-{hex_id}.rev"),
            reverse_index_file(kind, &packed.id, &by_oid)?,
        ));
    }
    if st.cruft {
        files.push((
            format!("{base}-{hex_id}.mtimes"),
            mtimes_file(&repo, kind, &packed.id, &by_oid)?,
        ));
    }

    for (path, bytes) in &files {
        if let Some(code) = write_artifact(path.as_str(), &bytes[..]) {
            return Ok(code);
        }
    }

    println!("{hex_id}");
    Ok(ExitCode::SUCCESS)
}

/// `--compression=<n>` as a zlib level. Out-of-range values never reach here
/// (`preflight` rejects them), and `-1` is zlib's "use the default".
fn compression(st: &State) -> gix::zlib::Compression {
    match st.compression {
        Some(level) if level >= 0 => {
            gix::zlib::Compression::new(level as i32).unwrap_or(gix::zlib::Compression::DEFAULT)
        }
        _ => gix::zlib::Compression::DEFAULT,
    }
}

/// git's end-of-run summary, which `--progress`/`--all-progress` put on stderr
/// and `-q` (or the absence of both, stderr not being a terminal here)
/// suppresses.
///
/// The delta counts are always zero, which is the truth about the pack written
/// here rather than a stand-in for git's numbers; see the module docs.
fn report_progress(st: &State, total: usize) {
    if st.progress {
        eprintln!("Total {total} (delta 0), reused 0 (delta 0), pack-reused 0 (from 0)");
    }
}

/// One entry as it was written into the pack.
#[derive(Clone)]
struct PackedEntry {
    id: ObjectId,
    /// Byte offset of the entry header within the pack.
    offset: u64,
    /// CRC-32 over the entry's bytes in the pack (header plus compressed data),
    /// which is what a v2 `.idx` stores.
    crc32: u32,
}

/// A finished pack held in memory, alongside the per-entry data its `.idx`,
/// `.rev` and `.mtimes` companions need.
struct Packed {
    bytes: Vec<u8>,
    id: ObjectId,
    entries: Vec<PackedEntry>,
}

/// Encode `counts` into a version-2 pack.
///
/// Every entry is written as a base object: there is no delta search here, for
/// the reason the module docs give. That also makes the entry header trivial —
/// `to_entry_header`'s base-distance callback exists only for `DeltaRef` and is
/// therefore unreachable.
fn write_pack(
    repo: &gix::Repository,
    counts: &[pack::data::output::Count],
    level: gix::zlib::Compression,
) -> Result<Packed> {
    // Entries are encoded before the header is written, because the header
    // carries the entry *count* and an object that turns out to be unreadable
    // must not be counted. git likewise drops such an object and packs the rest.
    const HEADER_LEN: u64 = 12;
    let mut body: Vec<u8> = Vec::new();
    let mut entries: Vec<PackedEntry> = Vec::with_capacity(counts.len());
    let mut buf = Vec::new();
    for count in counts {
        let Ok((data, _location)) = repo.objects.find(&count.id, &mut buf) else {
            continue;
        };
        let entry = pack::data::output::Entry::from_data(count, &data, level)?;
        let start = body.len();
        let header = entry.to_entry_header(pack::data::Version::V2, |_| {
            unreachable!("no delta is ever emitted, so no base distance is ever requested")
        });
        header.write_to(entry.decompressed_size as u64, &mut body)?;
        body.extend_from_slice(&entry.compressed_data);
        entries.push(PackedEntry {
            id: count.id,
            offset: HEADER_LEN + start as u64,
            crc32: gix::features::hash::crc32(&body[start..]),
        });
    }

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
    Ok(Packed { bytes, id, entries })
}

/// The `.idx` for a pack, in version 1 or 2.
///
/// `sorted` must be ordered by object id, which is the order both formats store
/// entries in and the order the 256-entry fan-out summarises.
///
/// A v2 index cannot represent an offset of 2 GiB or more inline; git spills
/// those into a 64-bit table flagged by the high bit. Packs written here are far
/// below that, but the table is emitted correctly rather than assumed away.
fn index_file(
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
fn reverse_index_file(
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
/// Objects that cannot be found are skipped rather than fatal: a reflog naming a
/// pruned commit is ordinary, and git drops those too.
fn collect_counts(
    repo: &gix::Repository,
    st: &State,
    stdin: &[u8],
) -> Vec<pack::data::output::Count> {
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
        let mut ids = rev_list_objects(repo, st, stdin);
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

    ids.into_iter()
        .map(|id| pack::data::output::Count {
            id,
            entry_pack_location: pack::data::output::count::PackLocation::NotLookedUp,
        })
        .collect()
}

/// The rev-list half of [`collect_counts`]: tips, their ancestry, and the trees
/// and blobs hanging off every commit reached.
fn rev_list_objects(repo: &gix::Repository, st: &State, stdin: &[u8]) -> Vec<ObjectId> {
    // Refs are collected unpeeled so an annotated tag's own object lands in the
    // pack; `peel_to_commit` supplies the commit the walk starts from.
    let mut unpeeled: Vec<ObjectId> = Vec::new();
    let mut tips: Vec<ObjectId> = Vec::new();
    let mut as_is: Vec<ObjectId> = Vec::new();

    if st.all {
        if let Ok(platform) = repo.references() {
            if let Ok(all) = platform.all() {
                for reference in all {
                    let Ok(mut reference) = reference else { continue };
                    if let Ok(id) = reference.follow_to_object() {
                        unpeeled.push(id.detach());
                    }
                }
            }
        }
        // A symbolic HEAD repeats a ref already collected; a detached one is
        // only reachable here.
        if let Ok(head) = repo.head() {
            if let Some(id) = head.id() {
                unpeeled.push(id.detach());
            }
        }
    }

    if st.reflog {
        unpeeled.extend(reflog_objects(repo));
    }

    if st.indexed_objects {
        if let Ok(index) = repo.index_or_empty() {
            for entry in index.entries() {
                // git's `add_index_objects_to_pending()` skips gitlinks, whose
                // ids name commits in another repository.
                if entry.mode != gix::index::entry::Mode::COMMIT {
                    as_is.push(entry.id);
                }
            }
            if let Some(tree) = index.tree() {
                push_cache_tree(tree, &mut as_is);
            }
        }
    }

    // stdin is rev-list arguments when git's internal rev list is on, and a
    // plain object list otherwise.
    if st.revs {
        for line in stdin.split(|b| *b == b'\n') {
            let Ok(spec) = std::str::from_utf8(line) else { continue };
            let spec = spec.trim();
            // Exclusions would need a boundary-aware walk; the sets this
            // command is asked for in practice are `--all`-shaped, so a
            // `^rev` is skipped rather than silently treated as inclusion.
            if spec.is_empty() || spec.starts_with('^') || spec.starts_with('-') {
                continue;
            }
            if let Ok(id) = repo.rev_parse_single(spec) {
                unpeeled.push(id.detach());
            }
        }
    } else if !st.internal_rev_list {
        for line in stdin.split(|b| *b == b'\n') {
            let Ok(text) = std::str::from_utf8(line) else { continue };
            // `rev-list --objects` prints `<oid> [<path>]`; git reads the first
            // field and ignores the rest.
            let Some(field) = text.split_whitespace().next() else { continue };
            if let Ok(id) = repo.rev_parse_single(field) {
                as_is.push(id.detach());
            }
        }
    }

    for id in &unpeeled {
        if let Some(commit) = peel_to_commit(repo, *id) {
            tips.push(commit);
        }
    }

    // Commits first, then the tag objects that pointed at them: that is the
    // grouping git's own output starts with, and it keeps a tag adjacent to the
    // history it names.
    let mut roots: Vec<ObjectId> = Vec::new();
    if let Ok(walk) = repo.rev_walk(tips.iter().copied()).all() {
        roots.extend(walk.filter_map(|info| info.ok().map(|info| info.id)));
    } else {
        roots.extend(tips.iter().copied());
    }
    roots.extend(unpeeled.iter().copied());
    roots.extend(as_is);

    expand(repo, roots)
}

/// Expand `roots` into the full object set, using `gix-pack`'s counter: a commit
/// contributes its tree and everything under it, a tag its target, a tree its
/// contents, and anything else itself.
///
/// Ancestry is *not* expanded here — [`rev_list_objects`] has already walked it
/// — which is exactly what `ObjectExpansion::TreeContents` does.
fn expand(repo: &gix::Repository, roots: Vec<ObjectId>) -> Vec<ObjectId> {
    // The counter treats a missing object as fatal for the whole run. Reflogs
    // routinely name objects that have since been pruned, so they are dropped
    // up front rather than allowed to abort the count.
    let roots: Vec<ObjectId> = roots
        .into_iter()
        .filter(|id| repo.find_object(*id).is_ok())
        .collect();
    let mut input = roots
        .iter()
        .copied()
        .map(Ok::<_, Box<dyn std::error::Error + Send + Sync + 'static>>);
    let counted = pack::data::output::count::objects_unthreaded(
        &*repo.objects,
        &mut input,
        &gix::progress::Discard,
        &std::sync::atomic::AtomicBool::new(false),
        pack::data::output::count::objects::ObjectExpansion::TreeContents,
    );
    match counted {
        Ok((counts, _outcome)) => counts.into_iter().map(|c| c.id).collect(),
        // An undecodable object still aborts the counter. git reports the
        // corruption and packs what it can, so fall back to the unexpanded
        // roots: a smaller pack, never a fatal.
        Err(_) => roots,
    }
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

/// Follow tag objects until a commit is reached. `None` for a ref that peels to
/// a tree or blob, which contributes no ancestry.
fn peel_to_commit(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    let mut id = id;
    // Bounded so a cyclic tag chain cannot spin; git's own peel is bounded too.
    for _ in 0..16 {
        let object = repo.find_object(id).ok()?;
        match object.kind {
            gix::object::Kind::Commit => return Some(id),
            gix::object::Kind::Tag => {
                let tag = object.into_tag();
                id = tag.decode().ok()?.target();
            }
            _ => return None,
        }
    }
    None
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
fn apply_filter(repo: &gix::Repository, spec: Option<&str>, ids: &mut Vec<ObjectId>) {
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
fn magnitude(v: &str) -> Option<u64> {
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

/// Write one pack artifact, reporting a failure the way git does.
///
/// git builds each file under a temporary name in the object store and only
/// then renames it into place, so a path it cannot create is diagnosed twice:
/// once for the write and once for the rename that never happened.
///
/// The rename is also why any existing file is unlinked first: a rename replaces
/// its destination whatever that destination's mode is, whereas writing straight
/// into the `0444` a previous run left behind would fail with `EACCES` and be
/// misreported as an unwritable directory.
fn write_artifact(path: &str, bytes: &[u8]) -> Option<ExitCode> {
    let _ = std::fs::remove_file(path);
    match std::fs::write(path, bytes) {
        // git leaves `.pack`, `.idx`, `.rev` and `.mtimes` world-readable but
        // immutable. A filesystem that refuses the mode is not fatal — git does
        // not check either.
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
            None
        }
        Err(err) => {
            eprintln!("error: unable to write file {path}: {}", errno_text(&err));
            eprintln!("fatal: unable to rename temporary file to '{path}'");
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
    match def.kind {
        Kind::Int if !is_number(v, true) => {
            eprintln!(
                "error: option `{shown}' expects an integer value with an optional k/m/g suffix"
            );
            return Some(ExitCode::from(129));
        }
        Kind::Magnitude if !is_number(v, false) => {
            eprintln!(
                "error: option `{shown}' expects a non-negative integer value with an optional k/m/g suffix"
            );
            return Some(ExitCode::from(129));
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
fn gently_parse_filter(arg: &[u8]) -> Result<(), String> {
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

/// git's number grammar for `OPT_INTEGER` / `OPT_MAGNITUDE`: digits with an
/// optional single `k`/`m`/`g` suffix (either case), and a sign only when
/// `signed` (i.e. never for a magnitude).
fn is_number(v: &str, signed: bool) -> bool {
    let digits = match v.strip_prefix('-') {
        Some(rest) if signed => rest,
        Some(_) => return false,
        None => v,
    };
    let digits = match digits.chars().last() {
        Some('k' | 'K' | 'm' | 'M' | 'g' | 'G') => &digits[..digits.len() - 1],
        _ => digits,
    };
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// The integer value of a string already accepted by [`is_number`], applying the
/// `k`/`m`/`g` multiplier. This is the number git's diagnostics print.
fn to_number(v: &str) -> Option<i64> {
    let (negative, body) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v),
    };
    let (body, scale) = match body.chars().last() {
        Some('k' | 'K') => (&body[..body.len() - 1], 1024),
        Some('m' | 'M') => (&body[..body.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&body[..body.len() - 1], 1024 * 1024 * 1024),
        _ => (body, 1),
    };
    let n = body.parse::<i64>().ok()?.checked_mul(scale)?;
    Some(if negative { -n } else { n })
}

/// Everything stock git checks after parsing and before it does any work, in
/// git's own order (each ordering below is pinned by an observed invocation).
///
/// The first check prints the bare usage block on stderr and exits 129; the rest
/// are `die()`s and exit 128.
fn preflight(st: &State) -> Option<ExitCode> {
    // `pack_to_stdout != !base_name`, plus git's rejection of a second
    // positional. Beats every `fatal:` below: `--compression=99` on its own
    // reports usage, not a bad compression level.
    if st.stdout == (st.positionals.len() == 1) || st.positionals.len() > 1 {
        eprint!("{USAGE}");
        return Some(ExitCode::from(129));
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
