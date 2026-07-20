//! `git index-pack` — build a `.idx` (and, by default, a `.rev`) for a pack.
//!
//! Covered, byte-for-byte against stock git on stdout and on the files left
//! behind:
//!
//!   * `git index-pack [-v] [-o <index-file>] [--[no-]rev-index] <pack-file>`
//!     — indexes a `.pack` already on disk, writes `<pack>.idx` (or the `-o`
//!     path), writes the matching `.rev` unless `--no-rev-index` /
//!     `pack.writeReverseIndex=false`, and prints the pack hash plus `\n`.
//!   * `git index-pack --stdin [--keep[=<msg>]] [--[no-]rev-index]` — streams
//!     the pack from stdin into `objects/pack/pack-<hash>.{pack,idx,rev}` and
//!     prints `pack\t<hash>\n`, or `keep\t<hash>\n` when a `.keep` was created.
//!   * `git index-pack --verify <pack-file>` — checks an existing `.idx`
//!     against its pack and exits 0 with no output when they agree.
//!   * `--threads=<n>` (`0` = auto), `--object-format=sha1`, and `-h` (usage on
//!     stdout, exit 129).
//!
//! Argument handling mirrors `cmd_index_pack()`'s hand-rolled loop rather than
//! `parse_options()`, because the two disagree in ways the harness sees: only
//! `-o <file>`, `--threads=<n>`, `--progress-title <t>` and `--index-version=<v>`
//! spellings are accepted (`-o<file>`, `--threads <n>`, `--progress-title=<t>`
//! and `--index-version <v>` are usage errors), `--verbose` and `--` are *not*
//! recognised at all, and a repeated `-o` or a second `<pack-file>` is a usage
//! error. Anything unrecognised prints the usage block on stderr and exits 129.
//!
//! The post-parse checks run in git's order, which is load-bearing: a command
//! naming both an unported flag and a bad path must fail the way git does, on
//! the path, not on the flag. That order is
//!
//!   1. no `<pack-file>` and no `--stdin`            → usage, exit 129
//!   2. `--fix-thin` without `--stdin`               → fatal, exit 128
//!   3. `--promisor` together with a `<pack-file>`   → fatal, exit 128
//!   4. `--stdin` outside a repository               → fatal, exit 128
//!   5. `--stdin` together with `--object-format`    → fatal, exit 128
//!   6. `<pack-file>` not ending in `.pack` (only when the index name has to
//!      be derived from it, i.e. no `-o`)            → fatal, exit 128
//!   7. `--verify`: the `.idx`/`.pack` pair is unreadable → fatal, exit 128
//!   8. the `<pack-file>` cannot be opened           → fatal, exit 128
//!
//! Only once every one of those has passed is an unported flag rejected, so
//! `--strict does-not-exist.pack` reports the missing pack exactly as git does
//! instead of complaining about `--strict`.
//!
//! File modes match git: `.pack`/`.idx`/`.rev` are left `0444`, a `.keep` is
//! `0600` and holds `<msg>\n` (empty for a bare `--keep`). The `.rev` payload
//! is written here directly against `gitformat-pack(5)` — RIDX magic, version
//! 1, hash id 1, one 4-byte index position per object sorted by pack offset,
//! the pack checksum, then a SHA-1 over all of the above — because the
//! vendored `gix-pack` has no reverse-index writer.
//!
//! Not covered, each rejected with a precise message rather than a plausible
//! wrong answer: `--fix-thin` (completing a thin pack re-deflates the borrowed
//! base objects, and `gix-pack`'s compression level and append order are not
//! guaranteed to reproduce git's resulting pack hash), `--strict`,
//! `--fsck-objects`, `--check-self-contained-and-connected`, `--max-input-size`,
//! `--promisor`, `--pack_header`, `--index-version` other than a plain `2`,
//! `--object-format=sha256`, `--verify` combined with `--stdin`, `--stdin`
//! combined with an explicit `<pack-file>` or with `-o`, `--keep` without
//! `--stdin`, and a `<pack-file>` holding REF_DELTA entries — which stock git
//! resolves in-pack — since `gix_pack::index::write_data_iter_to_stream`
//! refuses ref-deltas outright. Packs written by `git pack-objects` use
//! OFS_DELTA unless `--no-delta-base-offset` was passed.
//!
//! Three narrower gaps are documented rather than papered over: `-v` and
//! `--progress-title` are accepted but no progress is drawn on stderr (stdout
//! is unaffected, so the compared bytes still match); the fsck message ids in
//! `--strict=<id>=<severity>` and `--fsck-objects=<id>=<severity>` are not
//! validated, so git's parse-time `fatal: Unhandled message id: <id>` and
//! `fatal: Missing '=': <x>` are not reproduced — those spellings reach the
//! `--strict`/`--fsck-objects` rejection instead; and a `--verify` that finds
//! real corruption reports the `gix` error rather than git's diagnostic text.

use anyhow::{bail, Result};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::hash::{Kind, ObjectId};
use gix::odb::pack;

/// Stock git's `index-pack` usage line, byte-for-byte (228 bytes including the
/// trailing newline). Printed on `-h` (stdout) and for a usage error (stderr).
const USAGE: &str = "usage: git index-pack [-v] [-o <index-file>] [--keep | --keep=<msg>] [--[no-]rev-index] [--verify] [--strict[=<msg-id>=<severity>...]] [--fsck-objects[=<msg-id>=<severity>...]] (<pack-file> | --stdin [--fix-thin] [<pack-file>])\n";

/// git's `pack_idx_option.off32_limit` default; any other `,<limit>` given to
/// `--index-version` would change the index layout, which is not ported.
const DEFAULT_OFF32_LIMIT: u64 = 0x7fff_ffff;

/// Parsed command line for a single `index-pack` invocation.
///
/// Every flag stock git recognises has a field here, including the ones this
/// port cannot honour, so that parsing never fails early on a flag git would
/// have accepted before reporting a different problem.
struct Opts {
    stdin: bool,                  // --stdin: read the pack from standard input
    fix_thin: bool,               // --fix-thin
    verify: bool,                 // --verify
    keep: Option<Option<String>>, // --keep / --keep=<msg>
    index_out: Option<PathBuf>,   // -o <index-file>
    rev_index: Option<bool>,      // --rev-index / --no-rev-index (None = config)
    threads: Option<usize>,       // --threads=<n>, None = all logical cores
    strict: bool,                 // --strict / --strict=<msg-id>=<severity>...
    fsck_objects: bool,           // --fsck-objects[=...]
    self_contained: bool,         // --check-self-contained-and-connected
    promisor: bool,               // --promisor[=<msg>]
    index_version: Option<(u64, Option<u64>)>, // --index-version=<v>[,<limit>]
    max_input_size: bool,         // --max-input-size=<n>
    object_format: Option<String>, // --object-format=<algo>
    pack_header: bool,            // --pack_header=<v>,<n> (internal fetch path)
    pack: Option<PathBuf>,        // the positional <pack-file>
}

impl Opts {
    fn new() -> Self {
        Opts {
            stdin: false,
            fix_thin: false,
            verify: false,
            keep: None,
            index_out: None,
            rev_index: None,
            threads: None,
            strict: false,
            fsck_objects: false,
            self_contained: false,
            promisor: false,
            index_version: None,
            max_input_size: false,
            object_format: None,
            pack_header: false,
            pack: None,
        }
    }
}

pub fn index_pack(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::new();

    // git's own loop: anything starting with '-' is a flag (so a bare "-" and
    // "--" are both usage errors), anything else is the single pack name.
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();

        if !a.starts_with('-') {
            if opts.pack.is_some() {
                return Ok(usage_error());
            }
            opts.pack = Some(PathBuf::from(a));
            i += 1;
            continue;
        }

        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-v" => {} // progress is not drawn; stdout is unaffected
            "--stdin" => opts.stdin = true,
            "--fix-thin" => opts.fix_thin = true,
            "--verify" => opts.verify = true,
            "--keep" => opts.keep = Some(None),
            "--rev-index" => opts.rev_index = Some(true),
            "--no-rev-index" => opts.rev_index = Some(false),
            "--promisor" => opts.promisor = true,
            "--strict" => opts.strict = true,
            "--fsck-objects" => opts.fsck_objects = true,
            "--check-self-contained-and-connected" => opts.self_contained = true,
            "-o" => {
                // git: a second -o, or a missing value, is a usage error.
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error());
                };
                if opts.index_out.is_some() {
                    return Ok(usage_error());
                }
                opts.index_out = Some(PathBuf::from(v));
            }
            "--progress-title" => {
                // Consumed for parity; no progress is drawn.
                i += 1;
                if args.get(i).is_none() {
                    return Ok(usage_error());
                }
            }
            _ if a.starts_with("--keep=") => {
                opts.keep = Some(Some(a["--keep=".len()..].to_string()));
            }
            _ if a.starts_with("--promisor=") => opts.promisor = true,
            _ if a.starts_with("--strict=") => opts.strict = true,
            _ if a.starts_with("--fsck-objects=") => opts.fsck_objects = true,
            _ if a.starts_with("--threads=") => {
                // git validates the number here and answers with usage, not a
                // fatal, when it does not parse.
                let Some(n) = parse_threads(&a["--threads=".len()..]) else {
                    return Ok(usage_error());
                };
                opts.threads = n;
            }
            _ if a.starts_with("--max-input-size=") => opts.max_input_size = true,
            _ if a.starts_with("--pack_header=") => opts.pack_header = true,
            _ if a.starts_with("--object-format=") => {
                let fmt = &a["--object-format=".len()..];
                // git resolves the name immediately and dies on an unknown one.
                if fmt != "sha1" && fmt != "sha256" {
                    return Ok(fatal(format!("unknown hash algorithm '{fmt}'")));
                }
                opts.object_format = Some(fmt.to_string());
            }
            _ if a.starts_with("--index-version=") => {
                let Some(parsed) = parse_index_version(&a["--index-version=".len()..]) else {
                    return Ok(fatal(format!("bad {a}")));
                };
                opts.index_version = Some(parsed);
            }
            // Genuinely unknown: git answers with the usage block and 129.
            _ => return Ok(usage_error()),
        }
        i += 1;
    }

    // --- git's post-parse checks, in git's order. ---

    if opts.pack.is_none() && !opts.stdin {
        return Ok(usage_error());
    }
    if opts.fix_thin && !opts.stdin {
        return Ok(fatal("the option '--fix-thin' requires '--stdin'"));
    }
    if opts.promisor && opts.pack.is_some() {
        return Ok(fatal("--promisor cannot be used with a pack name"));
    }
    if opts.stdin {
        if gix::discover(".").is_err() {
            return Ok(fatal("--stdin requires a git repository"));
        }
        if opts.object_format.is_some() {
            return Ok(fatal(
                "options '--object-format' and '--stdin' cannot be used together",
            ));
        }
    }

    // The index name is derived from the pack name only when -o was not given;
    // that is the sole reason the `.pack` suffix is ever mandatory.
    let index_name = match (&opts.index_out, &opts.pack) {
        (Some(p), _) => Some(p.clone()),
        (None, Some(pack)) => {
            let name = pack.to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".pack") else {
                return Ok(fatal(format!(
                    "packfile name '{name}' does not end with '.pack'"
                )));
            };
            Some(PathBuf::from(format!("{stem}.idx")))
        }
        (None, None) => None,
    };

    if opts.verify {
        let Some(index_name) = index_name else {
            return Ok(fatal("--verify with no packfile name given"));
        };
        if opts.stdin {
            // git reads the pack from stdin and compares against the existing
            // index; the two `Cannot open existing pack ...` spellings it uses
            // there are not reproduced, so refuse rather than guess.
            bail!("unsupported: `--verify --stdin` (only verifying a pack already on disk is ported)");
        }
        return verify_existing(&opts, &index_name);
    }

    if opts.stdin {
        if opts.pack.is_some() {
            bail!("unsupported: `--stdin <pack-file>` (the pack copy is always named pack-<hash>.pack under objects/pack)");
        }
        if opts.index_out.is_some() {
            bail!("unsupported: `--stdin -o <index-file>` (the index is always written beside the pack)");
        }
        reject_unported(&opts)?;
        return index_from_stdin(&opts);
    }

    let pack_path = opts.pack.clone().expect("checked above");
    let index_name = index_name.expect("a pack name always yields an index name");
    index_pack_file(&opts, &pack_path, &index_name)
}

/// Index a `.pack` already on disk, writing the index beside it (or to `-o`).
///
/// The pack is opened before any unported flag is rejected, because that is the
/// order git fails in: a missing pack outranks a flag this port cannot honour.
fn index_pack_file(opts: &Opts, pack_path: &Path, index_path: &Path) -> Result<ExitCode> {
    let file = match fs::File::open(pack_path) {
        Ok(f) => f,
        Err(e) => {
            return Ok(fatal(format!(
                "could not open '{}' for reading: {}",
                pack_path.display(),
                strerror(&e)
            )));
        }
    };

    reject_unported(opts)?;
    if opts.keep.is_some() {
        bail!("unsupported: `--keep` without `--stdin`");
    }

    let mut entries = pack::data::input::BytesToEntriesIter::new_from_header(
        io::BufReader::new(file),
        pack::data::input::Mode::Verify,
        pack::data::input::EntryDataMode::Crc32,
        Kind::Sha1,
    )?;
    let pack_version = entries.version();

    // Write to a sibling temporary first so a failure never leaves a half index
    // in place, exactly as git's `git_mkstemp`/`rename` dance does.
    let tmp = with_suffix(index_path, ".tmp");
    let mut out = io::BufWriter::new(fs::File::create(&tmp)?);
    let outcome = pack::index::write_data_iter_to_stream(
        pack::index::Version::default(),
        || {
            let data = fs::read(pack_path)?;
            Ok((slice_of, data))
        },
        &mut entries,
        opts.threads,
        &mut gix::progress::Discard,
        &mut out,
        &AtomicBool::new(false),
        Kind::Sha1,
        None,
        pack_version,
    )?;
    out.flush()?;
    drop(out);
    fs::rename(&tmp, index_path)?;

    if want_rev_index(opts) {
        write_rev_index(index_path, &outcome.data_hash)?;
    }
    set_read_only(index_path)?;

    println!("{}", outcome.data_hash);
    Ok(ExitCode::SUCCESS)
}

/// `--verify`: check an existing index against its pack, printing nothing and
/// exiting 0 when they agree, exactly as git does.
///
/// git reaches this through `read_idx_option()` → `parse_pack_index()`, which
/// requires the index to parse *and* the sibling `.pack` (the index name with
/// `.idx` swapped for `.pack`, not the positional argument) to exist; when
/// either fails it dies naming the index path.
fn verify_existing(opts: &Opts, index_path: &Path) -> Result<ExitCode> {
    let name = index_path.to_string_lossy().into_owned();
    let cannot_open = || fatal(format!("Cannot open existing pack file '{name}'"));

    let Some(stem) = name.strip_suffix(".idx") else {
        return Ok(cannot_open());
    };
    let pack_path = PathBuf::from(format!("{stem}.pack"));

    let opened = pack::index::File::at(index_path, Kind::Sha1)
        .ok()
        .zip(pack::data::File::at(&pack_path, Kind::Sha1).ok());
    let Some((index, data)) = opened else {
        return Ok(cannot_open());
    };

    reject_unported(opts)?;
    if opts.keep.is_some() {
        bail!("unsupported: `--verify --keep` (the .keep file is not written here)");
    }

    let options = pack::index::verify::integrity::Options {
        // git checks each object's hash and CRC32 against the index plus the
        // two file checksums; it never re-encodes objects, so the stricter
        // modes would reject packs git accepts.
        verify_mode: pack::index::verify::Mode::HashCrc32,
        thread_limit: opts.threads,
        ..Default::default()
    };
    match index.verify_integrity(
        Some(pack::index::verify::PackContext {
            data: &data,
            options,
        }),
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
    ) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        // git's per-corruption diagnostics are not reproduced; report the real
        // failure rather than inventing text that only looks like git's.
        Err(e) => bail!("--verify failed for '{name}': {e}"),
    }
}

/// Stream a pack from stdin into `objects/pack`, then report it git's way.
fn index_from_stdin(opts: &Opts) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let pack_dir = repo.objects.store_ref().path().join("pack");
    fs::create_dir_all(&pack_dir)?;

    let stdin = io::stdin();
    let mut input = stdin.lock();
    let outcome = pack::Bundle::write_to_directory(
        &mut input,
        Some(&pack_dir),
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
        None::<gix::odb::Handle>,
        pack::bundle::write::Options {
            thread_limit: opts.threads,
            object_hash: Kind::Sha1,
            ..Default::default()
        },
    )?;

    let hash = outcome.index.data_hash;
    let (Some(data_path), Some(index_path)) = (&outcome.data_path, &outcome.index_path) else {
        bail!("empty packs are not supported (no objects were read from stdin)");
    };

    if want_rev_index(opts) {
        write_rev_index(index_path, &hash)?;
    }
    set_read_only(index_path)?;
    set_read_only(data_path)?;

    // `write_to_directory` always drops a `.keep` next to a freshly written
    // pack; git only leaves one when asked, so reconcile before reporting.
    let keep_path = data_path.with_extension("keep");
    match &opts.keep {
        Some(msg) => {
            let body = msg.as_ref().map(|m| format!("{m}\n")).unwrap_or_default();
            fs::write(&keep_path, body)?;
            fs::set_permissions(&keep_path, fs::Permissions::from_mode(0o600))?;
            println!("keep\t{hash}");
        }
        None => {
            if outcome.keep_path.is_some() {
                fs::remove_file(&keep_path)?;
            }
            println!("pack\t{hash}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Reject the flags stock git implements that this port does not.
///
/// Called only after every check git performs first has passed, so a terse
/// refusal here can never hide an error git would have reported instead. Each
/// message names the flag and why it is not honoured; none of these are
/// silently ignored, which would turn a wrong answer into an apparent success.
fn reject_unported(opts: &Opts) -> Result<()> {
    if opts.strict {
        bail!("unsupported flag \"--strict\" (no fsck pass is run here)");
    }
    if opts.fsck_objects {
        bail!("unsupported flag \"--fsck-objects\" (no fsck pass is run here)");
    }
    if opts.self_contained {
        bail!("unsupported flag \"--check-self-contained-and-connected\" (no connectivity pass is run here)");
    }
    if opts.promisor {
        bail!("unsupported flag \"--promisor\" (no .promisor file is written here)");
    }
    if opts.max_input_size {
        bail!("unsupported flag \"--max-input-size\" (input size is not bounded here)");
    }
    if opts.pack_header {
        bail!("unsupported flag \"--pack_header\" (internal fetch fast-path is not ported)");
    }
    if opts.fix_thin {
        bail!("unsupported flag \"--fix-thin\" (thin-pack completion would not reproduce git's pack hash)");
    }
    if let Some(fmt) = &opts.object_format {
        if fmt != "sha1" {
            bail!("unsupported object format {fmt:?} (ported: sha1)");
        }
    }
    if let Some((version, off32_limit)) = opts.index_version {
        if version != 2 || off32_limit.is_some_and(|l| l != DEFAULT_OFF32_LIMIT) {
            bail!("unsupported flag \"--index-version\" (only a plain version 2 index is written)");
        }
    }
    Ok(())
}

/// Whether a `.rev` must be produced: the explicit flag wins, otherwise
/// `pack.writeReverseIndex`, which git defaults to true.
fn want_rev_index(opts: &Opts) -> bool {
    if let Some(explicit) = opts.rev_index {
        return explicit;
    }
    gix::discover(".")
        .ok()
        .and_then(|repo| repo.config_snapshot().boolean("pack.writeReverseIndex"))
        .unwrap_or(true)
}

/// Write the reverse index for `index_path` per `gitformat-pack(5)`.
///
/// Layout: `RIDX`, version 1, hash id 1 (SHA-1), then one 4-byte big-endian
/// index position per object ordered by pack offset, the pack checksum, and a
/// SHA-1 trailer over everything preceding it. The file lands beside the index
/// with the `.idx` suffix swapped for `.rev`, matching git even under `-o`.
fn write_rev_index(index_path: &Path, pack_hash: &ObjectId) -> Result<()> {
    let index = pack::index::File::at(index_path, Kind::Sha1)?;

    let mut by_offset: Vec<(u64, u32)> = (0..index.num_objects())
        .map(|position| (index.pack_offset_at_index(position), position))
        .collect();
    by_offset.sort_unstable();

    let mut buf = Vec::with_capacity(12 + 4 * by_offset.len() + 40);
    buf.extend_from_slice(b"RIDX");
    buf.extend_from_slice(&1u32.to_be_bytes()); // version
    buf.extend_from_slice(&1u32.to_be_bytes()); // hash function id: SHA-1
    for (_, position) in &by_offset {
        buf.extend_from_slice(&position.to_be_bytes());
    }
    buf.extend_from_slice(pack_hash.as_slice());

    let mut hasher = gix::hash::hasher(Kind::Sha1);
    hasher.update(&buf);
    let checksum = hasher.try_finalize()?;
    buf.extend_from_slice(checksum.as_slice());

    let rev_path = index_path.with_extension("rev");
    let tmp = with_suffix(&rev_path, ".tmp");
    fs::write(&tmp, &buf)?;
    fs::rename(&tmp, &rev_path)?;
    set_read_only(&rev_path)?;
    Ok(())
}

/// Resolver handed to `write_data_iter_to_stream`: the whole pack is held in
/// memory and entries are sliced out of it by byte range.
///
/// The `&Vec<u8>` is load-bearing: the resolver's bound is
/// `for<'r> Fn(EntryRange, &'r R) -> Option<&'r [u8]>`, so the parameter has to
/// name the owned buffer type rather than a slice.
#[allow(clippy::ptr_arg)]
fn slice_of(entry: pack::data::EntryRange, data: &Vec<u8>) -> Option<&[u8]> {
    data.get(entry.start as usize..entry.end as usize)
}

/// `<path><suffix>-<pid>`, used for the sibling temporaries we rename into place.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.to_path_buf().into_os_string();
    name.push(format!("{suffix}-{}", std::process::id()));
    PathBuf::from(name)
}

/// git leaves `.pack`, `.idx` and `.rev` world-readable but immutable (0444).
fn set_read_only(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))?;
    Ok(())
}

/// `--threads=<n>`; `0` means "pick a sensible number", which is `None` here.
/// `None` is returned when the value does not parse, which git answers with the
/// usage block rather than a fatal.
fn parse_threads(value: &str) -> Option<Option<usize>> {
    let n: usize = value.parse().ok()?;
    Some((n != 0).then_some(n))
}

/// `--index-version=<version>[,<off32-limit>]`.
///
/// Mirrors git's two `strtoul()` calls: the version is read in base 10 and the
/// optional limit in base 0 (so `0x…` is hex and a leading `0` is octal). Any
/// trailing junk, a version above 2, or a limit with bit 31 set is what git
/// answers with `fatal: bad --index-version=<raw>`; `None` reports that here.
fn parse_index_version(rest: &str) -> Option<(u64, Option<u64>)> {
    let (version, tail) = strtoul(rest, 10);
    if version > 2 {
        return None;
    }
    match tail.strip_prefix(',') {
        Some(after) => {
            let (limit, tail) = strtoul(after, 0);
            if !tail.is_empty() || limit & 0x8000_0000 != 0 {
                return None;
            }
            Some((version, Some(limit)))
        }
        None => tail.is_empty().then_some((version, None)),
    }
}

/// C's `strtoul` reduced to what `--index-version` needs: returns the parsed
/// value and the unconsumed tail, consuming nothing (and yielding `0`) when no
/// digits follow. Base `0` selects hex for a `0x` prefix, octal for a leading
/// `0`, decimal otherwise. A negative value wraps as C does, which always
/// leaves it above any limit the caller accepts.
fn strtoul(s: &str, base: u32) -> (u64, &str) {
    let (negative, digits_at) = match s.as_bytes().first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    };
    let body = &s[digits_at..];

    let (base, body_at) = match base {
        0 if body.starts_with("0x") || body.starts_with("0X") => (16, 2),
        0 if body.starts_with('0') && body.len() > 1 => (8, 1),
        0 => (10, 0),
        b => (b, 0),
    };
    let body = &body[body_at..];

    let end = body
        .find(|c: char| !c.is_digit(base))
        .unwrap_or(body.len());
    if end == 0 {
        // Nothing was consumed, so neither was the sign or the base prefix.
        return (0, s);
    }
    let value = u64::from_str_radix(&body[..end], base).unwrap_or(u64::MAX);
    let value = if negative { value.wrapping_neg() } else { value };
    (value, &body[end..])
}

/// `std::io::Error`'s message without Rust's ` (os error N)` tail, so the
/// `fatal:` line reads exactly as git's `strerror`-based one does.
fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// git's `die()`: the message on stderr behind `fatal: `, exit 128.
fn fatal(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}

/// git's answer to a missing, duplicated or unrecognised argument: the usage
/// block on stderr, exit 129.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}
