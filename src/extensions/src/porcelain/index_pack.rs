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
//!   * `--threads=<n>` (`0` = auto), `--object-format=sha1`, `--`, and `-h`
//!     (usage on stdout, exit 129). A missing/duplicate `<pack-file>` or an
//!     unknown flag prints the same usage on stderr with exit 129.
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
//! guaranteed to reproduce git's resulting pack hash), `--verify`, `--strict`,
//! `--fsck-objects`, `--check-self-contained-and-connected`, `--index-version`,
//! `--max-input-size`, `--promisor`, `--progress-title`, `--stdin` combined
//! with an explicit `<pack-file>` or with `-o`, `--keep` without `--stdin`,
//! and `--object-format=sha256`.
//!
//! Two further limits, documented rather than papered over: `-v` is accepted
//! but no progress is drawn on stderr (stdout is unaffected, so the compared
//! bytes still match), and a pack containing REF_DELTA entries — which stock
//! git resolves in-pack — fails here, since
//! `gix_pack::index::write_data_iter_to_stream` refuses ref-deltas outright.
//! Packs written by `git pack-objects` use OFS_DELTA unless
//! `--no-delta-base-offset` was passed.

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

/// Parsed command line for a single `index-pack` invocation.
struct Opts {
    stdin: bool,                 // --stdin: read the pack from standard input
    keep: Option<Option<String>>, // --keep / --keep=<msg>
    index_out: Option<PathBuf>,  // -o <index-file>
    rev_index: Option<bool>,     // --rev-index / --no-rev-index (None = config)
    threads: Option<usize>,      // --threads=<n>, None = all logical cores
    pack: Option<PathBuf>,       // the positional <pack-file>
}

pub fn index_pack(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        stdin: false,
        keep: None,
        index_out: None,
        rev_index: None,
        threads: None,
        pack: None,
    };

    let mut no_more_opts = false;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();

        if no_more_opts || a == "-" || !a.starts_with('-') {
            if opts.pack.is_some() {
                return Ok(usage_error());
            }
            opts.pack = Some(PathBuf::from(a));
            i += 1;
            continue;
        }

        match a {
            "--" => no_more_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-v" | "--verbose" => {} // progress is not drawn; stdout is unaffected
            "--stdin" => opts.stdin = true,
            "--keep" => opts.keep = Some(None),
            "--rev-index" => opts.rev_index = Some(true),
            "--no-rev-index" => opts.rev_index = Some(false),
            "-o" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error());
                };
                opts.index_out = Some(PathBuf::from(v));
            }
            "--threads" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error());
                };
                opts.threads = parse_threads(v)?;
            }
            // Recognised by stock git, but not ported — never silently ignored.
            "--fix-thin" => bail!(
                "unsupported flag \"--fix-thin\" (thin-pack completion would not reproduce git's pack hash)"
            ),
            "--verify" => bail!("unsupported flag \"--verify\" (ported: -v, -o, --stdin, --keep, --rev-index, --threads)"),
            "--check-self-contained-and-connected" => bail!(
                "unsupported flag \"--check-self-contained-and-connected\" (ported: -v, -o, --stdin, --keep, --rev-index, --threads)"
            ),
            "--promisor" => bail!("unsupported flag \"--promisor\" (ported: -v, -o, --stdin, --keep, --rev-index, --threads)"),
            _ if a.starts_with("--keep=") => opts.keep = Some(Some(a["--keep=".len()..].to_string())),
            _ if a.starts_with("--threads=") => opts.threads = parse_threads(&a["--threads=".len()..])?,
            _ if a.starts_with("-o") => opts.index_out = Some(PathBuf::from(&a[2..])),
            _ if a.starts_with("--object-format=") => {
                let fmt = &a["--object-format=".len()..];
                if fmt != "sha1" {
                    bail!("unsupported object format {fmt:?} (ported: sha1)");
                }
            }
            _ if a.starts_with("--strict") => bail!("unsupported flag \"--strict\" (no fsck pass is run here)"),
            _ if a.starts_with("--fsck-objects") => {
                bail!("unsupported flag \"--fsck-objects\" (no fsck pass is run here)")
            }
            _ if a.starts_with("--index-version=") => bail!("unsupported flag {a:?} (only index version 2 is written)"),
            _ if a.starts_with("--max-input-size=") => bail!("unsupported flag {a:?} (input size is not bounded here)"),
            _ if a.starts_with("--progress-title") => bail!("unsupported flag {a:?} (no progress is drawn)"),
            _ if a.starts_with("--pack_header=") => bail!("unsupported flag {a:?} (internal fetch fast-path is not ported)"),
            // Genuinely unknown: git answers with the usage block and 129.
            _ => return Ok(usage_error()),
        }
        i += 1;
    }

    if opts.stdin {
        if opts.pack.is_some() {
            bail!("unsupported: `--stdin <pack-file>` (the pack copy is always named pack-<hash>.pack under objects/pack)");
        }
        if opts.index_out.is_some() {
            bail!("unsupported: `--stdin -o <index-file>` (the index is always written beside the pack)");
        }
        return index_from_stdin(&opts);
    }

    if opts.keep.is_some() {
        bail!("unsupported: `--keep` without `--stdin`");
    }
    let Some(pack_path) = opts.pack.clone() else {
        return Ok(usage_error());
    };
    index_pack_file(&opts, &pack_path)
}

/// Index a `.pack` already on disk, writing the index beside it (or to `-o`).
///
/// Mirrors stock git: the `.pack` suffix is only mandatory when the index name
/// has to be derived from it, and the pack hash is printed on its own line.
fn index_pack_file(opts: &Opts, pack_path: &Path) -> Result<ExitCode> {
    let index_path = match &opts.index_out {
        Some(p) => p.clone(),
        None => {
            let name = pack_path.to_string_lossy();
            let Some(stem) = name.strip_suffix(".pack") else {
                eprintln!("fatal: packfile name '{name}' does not end with '.pack'");
                return Ok(ExitCode::from(128));
            };
            PathBuf::from(format!("{stem}.idx"))
        }
    };

    let file = match fs::File::open(pack_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "fatal: could not open '{}' for reading: {}",
                pack_path.display(),
                strerror(&e)
            );
            return Ok(ExitCode::from(128));
        }
    };

    let mut entries = pack::data::input::BytesToEntriesIter::new_from_header(
        io::BufReader::new(file),
        pack::data::input::Mode::Verify,
        pack::data::input::EntryDataMode::Crc32,
        Kind::Sha1,
    )?;
    let pack_version = entries.version();

    // Write to a sibling temporary first so a failure never leaves a half index
    // in place, exactly as git's `git_mkstemp`/`rename` dance does.
    let tmp = with_suffix(&index_path, ".tmp");
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
    fs::rename(&tmp, &index_path)?;

    if want_rev_index(opts) {
        write_rev_index(&index_path, &outcome.data_hash)?;
    }
    set_read_only(&index_path)?;

    println!("{}", outcome.data_hash);
    Ok(ExitCode::SUCCESS)
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
fn parse_threads(value: &str) -> Result<Option<usize>> {
    let n: usize = value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid number of threads {value:?}"))?;
    Ok((n != 0).then_some(n))
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

/// git's answer to a missing, duplicated or unrecognised argument: the usage
/// block on stderr, exit 129.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}
