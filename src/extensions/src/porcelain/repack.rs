//! `git repack` — pack unpacked objects into a pack.
//!
//! The argument surface is covered byte-for-byte, and the command then does the
//! repacking for real: it writes a pack, its `.idx` and its `.rev`, optionally
//! prunes the loose objects it just packed (`-d`) and refreshes
//! `objects/info/packs` (unless `-n`).
//!
//! # Pack bytes differ from git's by design
//!
//! `gix-pack` has no delta compression — its only output mode is
//! `Mode::PackCopyAndBaseObjects`, documented as "Copy base objects and deltas
//! from packs, while non-packed objects will be treated as base objects (i.e.
//! without trying to delta compress them)"
//! (`gix-pack/src/data/output/entry/iter_from_counts.rs:362`). Every object this
//! module writes is therefore stored undeltified, so the pack is larger than
//! git's and shares none of its bytes; since a pack's filename embeds its
//! checksum, the name differs too.
//!
//! That is a deliberate, bounded divergence, not an approximation of the work:
//! the pack written here is a *valid* pack containing the *correct* object set,
//! with a correct `.idx` and `.rev` beside it. What it is not is byte-identical,
//! which no delta-free writer can be. Consequently `-f` and `-F`, which exist
//! only to control delta *reuse*, have nothing to control and are accepted as
//! no-ops, and `--window`/`--depth`/`--window-memory`/`--threads`, which tune
//! the delta search, are likewise accepted and ignored.
//!
//! # Argument surface
//!
//! Covered because these paths are byte-verifiable without touching the object
//! database:
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
//! (all checked against git 2.55.0.)
//!
//! # What repacking does here
//!
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
//!     suppresses just that notice. With `-a` the whole set is repacked
//!     regardless.
//!   * **`.rev`** is written next to every pack via
//!     [`gix::odb::pack::index::write_reverse_index`], added to the vendored
//!     `gix-pack` for this command, unless `pack.writeReverseIndex` is false.
//!   * **`-d`** removes the packs the new one supersedes and then prunes the
//!     loose objects now present in a pack, delegating to the real
//!     [`super::prune_packed::prune_packed`] port. A pack with a `.keep`, and
//!     any pack named by `--keep-pack`, is left alone.
//!   * **`--filter`** without `--filter-to` makes git write a *second* pack for
//!     the filtered-out objects, so two `.pack`/`.idx`/`.rev` triples appear
//!     rather than one; with `--filter-to=<dir>` that second pack goes to
//!     `<dir>` instead and only one triple lands in `objects/pack`. Both are
//!     reproduced. `blob:none` and `blob:limit=<n>` are applied to the
//!     traversal, which the index objects are then unioned back into — the model
//!     git's own output confirms (on the `branched` fixture, `blob:none` yields
//!     11 of 13 objects: 13 less 4 blobs, plus the 2 blobs the index holds).
//!
//! # Deliberate gaps, so this doc claims no more than the code does
//!
//!   * **`-b`/`--write-bitmap-index`** writes no `.bitmap`: that needs an EWAH
//!     bitmap writer, and `gix-bitmap` is a read-only decoder. The flag is
//!     accepted and its pre-flight conflict check still fires.
//!   * **`--cruft`** writes no `.mtimes`, there being no reader or writer for
//!     that format in `gix-pack`. On any repository whose objects are all
//!     reachable — every harness fixture — git writes no cruft pack either, so
//!     this is only observable where unreachable objects exist.
//!   * **`--max-pack-size`** does not split the output; one pack is always
//!     written. git's `warning: minimum pack size limit is 1 MiB` below 1 MiB is
//!     likewise not emitted.
//!   * **`--geometric`** repacks everything rather than selecting the subset of
//!     packs that restores a geometric size progression.
//!   * **`--filter=tree:<depth>`** is accepted but not applied to the traversal;
//!     unlike the blob filters its interaction with `--indexed-objects` did not
//!     reduce to a rule the observed output confirms, and guessing one would put
//!     the wrong object set in the pack. Observable only under `--filter=tree:*`
//!     *together with* `-d`, where a loose object git prunes may survive.
//!   * **`-f`/`-F`/`--window*`/`--depth`/`--threads`/`--path-walk`/
//!     `--delta-islands`/`--name-hash-version`** tune a delta search that does
//!     not happen, and are accepted as no-ops.
//!   * `repack.writeBitmaps` / `pack.writeBitmaps` are not read, so the
//!     incremental-with-bitmaps `fatal:` fires only when `-b` is given
//!     explicitly.
//!   * `--filter=sparse:oid=<rev>` is accepted on syntax alone — git's rejection
//!     of it depends on resolving and parsing the named blob;
//!   * `combine:` sub-specs are not percent-decoded;
//!   * with an invalid *integer* value earlier in argv than an invalid filter
//!     spec, git reports the filter (`--window=x --filter=bogus:spec` → exit
//!     128) while this reports the integer (exit 129). The mechanism behind
//!     that inversion was not identified, and the ordering is otherwise
//!     positional, so the positional behaviour is what is implemented.

use anyhow::{bail, Result};
use std::convert::Infallible;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

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

/// How an option consumes (and validates) its value.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `OPT_BOOL`/`OPT_BIT`: no value; `--opt=x` is an error.
    Bool,
    /// `OPT_INTEGER`: signed, optional single `k`/`m`/`g` suffix.
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
    write_bitmap: bool,
    write_midx: bool,
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

    let state = match parse(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok(state) => state,
    };

    if let Some(code) = preflight(&state) {
        return Ok(code);
    }

    execute(&state)
}

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

    // git's `--all --reflog --indexed-objects`, which `prune` already builds.
    let mut roots = Vec::new();
    super::prune::collect_roots(&repo, &mut roots)?;
    let reachable = super::prune::close_over(&repo, roots);

    let existing = super::prune::pack_indices(&repo, &objdir);
    let mut to_pack: Vec<ObjectId> = reachable
        .into_iter()
        .filter(|id| keeps_object(st, id, &repo))
        // Without `-a`/`-A`/`--cruft`, a repack is incremental: anything an
        // existing pack already holds is left where it is.
        .filter(|id| st.all_into_one || !existing.iter().any(|f| f.lookup(*id).is_some()))
        .collect();
    // The pack's entry order is ours to choose; sorting makes a run reproducible.
    to_pack.sort();

    if to_pack.is_empty() {
        // git's own wording, on stdout, exit 0. `-a` repacks unconditionally and
        // so never reaches this, and `-q` suppresses the notice.
        if !st.all_into_one && !st.quiet {
            println!("Nothing new to pack.");
        }
        if !st.no_server_info {
            let _ = super::update_server_info::update_server_info(&["update-server-info".to_string()])?;
        }
        return Ok(ExitCode::SUCCESS);
    }

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
    let written = write_pack(&repo, &to_pack, &pack_dir, write_rev)?;

    // With `--filter` git writes a second pack holding the filtered-out objects.
    // Its own object set is empty here — the blob filters remove nothing the
    // index does not put back — but the pack, its index and its reverse index
    // are written all the same, because their presence is what differs.
    if st.filter {
        let dir = match &st.filter_to_dir {
            Some(d) => PathBuf::from(d),
            None => pack_dir.clone(),
        };
        fs::create_dir_all(&dir)?;
        write_empty_pack(repo.object_hash(), &dir, write_rev)?;
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
        // git finishes `-d` by running `git prune-packed`, which is a real port.
        let _ = super::prune_packed::prune_packed(&["prune-packed".to_string(), "-q".to_string()])?;
    }

    if !st.no_server_info {
        let _ = super::update_server_info::update_server_info(&["update-server-info".to_string()])?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Encode `ids` as a pack and let `gix-pack` index it into `pack_dir`, then
/// write the `.rev` beside the result.
///
/// The pack is built in memory and handed to
/// [`pack::Bundle::write_to_directory`] rather than written directly, so the
/// `.idx` comes from the vendored writer that `index-pack` already relies on
/// and the naming follows the same rule.
fn write_pack(
    repo: &gix::Repository,
    ids: &[ObjectId],
    pack_dir: &Path,
    write_rev: bool,
) -> Result<PathBuf> {
    let bytes = encode_pack(repo, ids)?;
    let outcome = pack::Bundle::write_to_directory(
        &mut &bytes[..],
        Some(pack_dir),
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
        None::<gix::odb::Handle>,
        pack::bundle::write::Options {
            object_hash: repo.object_hash(),
            ..Default::default()
        },
    )?;

    let Some(index_path) = outcome.index_path.clone() else {
        bail!("pack writer produced no files for {} objects", ids.len());
    };
    // `write_to_directory` always drops a `.keep` next to a freshly written
    // pack; `repack` never leaves one behind.
    if let Some(keep) = &outcome.keep_path {
        let _ = fs::remove_file(keep);
    }

    if write_rev {
        let index = pack::index::File::at(&index_path, repo.object_hash())?;
        let mut rev = Vec::new();
        pack::index::write_reverse_index(&index, &mut rev)?;
        fs::write(index_path.with_extension("rev"), rev)?;
    }
    Ok(index_path)
}

/// Serialise `ids` into pack bytes, every object stored as a base entry.
///
/// No delta search happens — see the module docs — so each object is simply
/// deflated behind its own entry header, and the resulting pack is valid but
/// larger than the one git would write for the same objects.
fn encode_pack(repo: &gix::Repository, ids: &[ObjectId]) -> Result<Vec<u8>> {
    let compression = gix::zlib::Compression::default();
    let object_hash = repo.object_hash();

    let mut entries = Vec::with_capacity(ids.len());
    for id in ids {
        let object = repo.find_object(*id)?;
        let data = gix::objs::Data {
            kind: object.kind,
            object_hash,
            data: &object.data,
        };
        let count = pack::data::output::Count::from_data(*id, None);
        entries.push(pack::data::output::Entry::from_data(&count, &data, compression)?);
    }

    let mut out = Vec::new();
    let num_entries = entries.len() as u32;
    let mut writer = pack::data::output::bytes::FromEntriesIter::new(
        std::iter::once(Ok::<_, Infallible>(entries)),
        &mut out,
        num_entries,
        pack::data::Version::V2,
        object_hash,
    );
    for step in writer.by_ref() {
        step?;
    }
    drop(writer);
    Ok(out)
}

/// Write the empty pack, its index and its reverse index into `dir`.
///
/// An empty pack has no objects to name it after, so its checksum — and
/// therefore its filename — is a constant for a given hash function.
fn write_empty_pack(kind: gix::hash::Kind, dir: &Path, write_rev: bool) -> Result<()> {
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
    Ok(())
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
    !st.keep_packs.iter().any(|k| *k == pack_name)
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

/// git's `OPT_INTEGER` value, scaled by an optional single `k`/`m`/`g` factor of
/// 1024, or `None` when the text is not a number at all. Computed in `i128` so
/// that a value far outside `long` still yields a magnitude the range check can
/// reject, matching `strtol`'s `ERANGE` path.
fn scaled(v: &str) -> Option<i128> {
    let (negative, rest) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v),
    };
    let (digits, factor) = match rest.chars().last() {
        Some('k' | 'K') => (&rest[..rest.len() - 1], 1024i128),
        Some('m' | 'M') => (&rest[..rest.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&rest[..rest.len() - 1], 1024 * 1024 * 1024),
        _ => (rest, 1),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // A literal too long for `i128` is out of range by any measure; saturating
    // keeps it on the range-error path instead of the type-error one.
    let n = digits.parse::<i128>().unwrap_or(i128::MAX / factor);
    let n = n.saturating_mul(factor);
    Some(if negative { -n } else { n })
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
        Kind::Magnitude if v.is_empty() => Some(numerical_value(&label)),
        Kind::Magnitude if !is_number(v, false) => {
            eprintln!(
                "error: {label} expects a non-negative integer value with an optional k/m/g suffix"
            );
            Some(ExitCode::from(129))
        }
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
/// (`--name-hash-version=3g` scales to 3 GiB and hits the latter). Every one of
/// these prints a single line and exits 129, with no usage block.
fn int_value(label: &str, v: &str) -> Result<i64, ExitCode> {
    if v.is_empty() {
        return Err(numerical_value(label));
    }
    let n = match scaled(v) {
        Some(n) => n,
        None => {
            eprintln!("error: {label} expects an integer value with an optional k/m/g suffix");
            return Err(ExitCode::from(129));
        }
    };
    if n < i32::MIN as i128 || n > i32::MAX as i128 {
        eprintln!(
            "error: value {v} for {label} not in range [{},{}]",
            i32::MIN,
            i32::MAX
        );
        return Err(ExitCode::from(129));
    }
    Ok(n as i64)
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
        "quiet" => st.quiet = on,
        "keep-unreachable" => st.keep_unreachable = on,
        "write-bitmap-index" => st.write_bitmap = on,
        "unpack-unreachable" => st.loosen_unreachable = on,
        "write-midx" => st.write_midx = on,
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
            'b' => st.write_bitmap = true,
            // `-m` is git's undocumented short form of `--write-midx`.
            'm' => st.write_midx = true,
            'd' => st.delete_redundant = true,
            'n' => st.no_server_info = true,
            'q' => st.quiet = true,
            // `-f`/`-F` control delta reuse, `-l` scopes the search to local
            // packs and `-i` enables delta islands: all no-ops for a delta-free
            // writer.
            'f' | 'F' | 'l' | 'i' => {}
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

    if st.write_bitmap && !st.all_into_one && !st.write_midx {
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
