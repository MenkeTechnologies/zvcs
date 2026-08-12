//! `git index-pack` — build a `.idx` (and, by default, a `.rev`) for a pack.
//!
//! Covered, byte-for-byte against stock git on stdout and on the files left
//! behind:
//!
//!   * `git index-pack [-v] [-o <index-file>] [--[no-]rev-index] <pack-file>`
//!     — indexes a `.pack` already on disk, writes `<pack>.idx` (or the `-o`
//!     path), writes the matching `.rev` unless `--no-rev-index` /
//!     `pack.writeReverseIndex=false`, and prints the pack hash plus `\n`.
//!   * `git index-pack --stdin [--fix-thin] [--keep[=<msg>]] [--[no-]rev-index]
//!     [--max-input-size=<n>] [<pack-file>]` — streams the pack from stdin into
//!     `objects/pack/pack-<hash>.{pack,idx,rev}` (or into `<pack-file>` when one
//!     is named, opened `O_CREAT|O_EXCL`, with the index derived from it or from
//!     `-o`) and prints `pack\t<hash>\n`, or `keep\t<hash>\n` when a `.keep` was
//!     created. `--fix-thin` completes a thin pack by resolving its REF_DELTA
//!     bases against the object database; `--max-input-size=<n>` bounds the
//!     bytes read from stdin, dying with git's `pack exceeds maximum allowed
//!     size (<n>)` when exceeded.
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
//!   9. `parse_pack_header()`: fewer than twelve bytes of input is
//!      `fatal: early EOF`, a wrong magic is `fatal: pack signature mismatch`,
//!      and a version other than 2 or 3 is
//!      `fatal: pack version <n> unsupported` → exit 128
//!
//! Only once every one of those has passed is an unported flag rejected, so
//! `--check-self-contained-and-connected does-not-exist.pack` reports the
//! missing pack exactly as git does instead of complaining about the flag.
//!
//! Everything past option parsing is a `die()` in git, so nothing here exits 1:
//! a failure the checks above did not name still becomes a `fatal:` line and
//! exit 128. `--stdin` without a `<pack-file>` also leaves the same
//! `objects/pack/tmp_pack_XXXXXX` behind that git's `open_pack_file(NULL)` does
//! — created before the header is parsed, renamed into place on success, and
//! deliberately not cleaned up when the command dies.
//!
//! File modes match git: `.pack`/`.idx`/`.rev` are left `0444`, a `.keep` is
//! `0600` and holds `<msg>\n` (empty for a bare `--keep`). The `.rev` payload
//! is written here directly against `gitformat-pack(5)` — RIDX magic, version
//! 1, hash id 1, one 4-byte index position per object sorted by pack offset,
//! the pack checksum, then a SHA-1 over all of the above — because the
//! vendored `gix-pack` has no reverse-index writer.
//!
//! Thin-pack completion (`--fix-thin`) is honoured through the object database:
//! `gix_pack::data::input::LookupRefDeltaObjectsIter` resolves each REF_DELTA
//! base from the odb and injects it, so a thin pack is completed and indexed.
//! One caveat is documented rather than hidden: git appends the borrowed bases
//! at the end of the pack while `gix` injects each one just before its first
//! referencing delta, so a pack that actually needed completion is a valid,
//! self-contained pack but its hash need not equal the one stock git would
//! print. A self-contained stream (the common case) is copied through
//! byte-for-byte, so its hash matches git exactly.
//!
//! `--strict` and `--fsck-objects` are covered: see [`fsck_pack`]. Both run
//! `fsck_object()` over every object the pack holds, `--strict` adds
//! `fsck_walk()`/`check_objects()`'s link and type checks, and both finish with
//! `fsck_finish()`'s `.gitmodules`/`.gitattributes` lint. The checks reuse the
//! `fsck_object()` port in [`super::fsck`] and run before the index is renamed
//! into place, so a pack that fails them leaves no artifact behind.
//!
//! Not covered, each rejected with a precise message rather than a plausible
//! wrong answer: the `<msg-id>=<severity>` list form of `--strict=` /
//! `--fsck-objects=` (its grammar is validated, but the severities it asks for
//! are not applied — the checks run at git's defaults),
//! `--check-self-contained-and-connected` (git's connectivity pass over the
//! whole reachable set, which exceeds the vendored `gix-fsck` primitive),
//! `--promisor`, `--pack_header`, `--index-version` other than a plain `2`,
//! `--object-format=sha256`, `--verify` combined with `--stdin`, `--keep`
//! without `--stdin`, and a `<pack-file>` on disk (or a self-contained pack read
//! from stdin without `--fix-thin`) holding REF_DELTA entries — which stock git
//! resolves in-pack — since `gix_pack::index::write_data_iter_to_stream` refuses
//! ref-deltas outright and the odb lookup would duplicate an in-pack base rather
//! than reference it. Packs written by `git pack-objects` use OFS_DELTA unless
//! `--no-delta-base-offset` was passed.
//!
//! The fsck message-type list in `--strict=<id>=<severity>...` and
//! `--fsck-objects=<id>=<severity>...` IS validated at parse time by
//! `validate_fsck_msg_types`, mirroring git's `fsck_set_msg_types()`: a
//! malformed value dies (exit 128) with git's exact wording — `Missing '=':
//! '<tok>'`, `Unhandled message id: <id>`, `Unknown fsck message type:
//! '<sev>'`, or `Cannot demote <id> to <sev>` — before any positional check.
//! Only a *well-formed* value reaches the later rejection of the severity list
//! itself.
//!
//! Two narrower gaps are documented rather than papered over: `-v` and
//! `--progress-title` are accepted but no progress is drawn on stderr (stdout
//! is unaffected, so the compared bytes still match); and a `--verify` that
//! finds real corruption reports the `gix` error rather than git's diagnostic
//! text.

use anyhow::{bail, Result};
use std::fs;
use std::io::{self, Read, Write};
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
    msg_types: Option<String>,    // the `<msg-id>=<severity>...` list, if one was given
    self_contained: bool,         // --check-self-contained-and-connected
    promisor: bool,               // --promisor[=<msg>]
    index_version: Option<(u64, Option<u64>)>, // --index-version=<v>[,<limit>]
    max_input_size: Option<u64>,  // --max-input-size=<n> (None or 0 = no bound)
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
            msg_types: None,
            self_contained: false,
            promisor: false,
            index_version: None,
            max_input_size: None,
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
    //
    // `args` holds only the arguments; `dispatch::run` takes the `index-pack`
    // verb as a separate parameter and never passes it through here, so the
    // scan starts at 0. Starting at 1 silently dropped the first argument,
    // which turned every `index-pack <pack-file>` into a "no pack name" usage
    // error instead of the fatal git reports for the path.
    let mut i = 0;
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
            _ if a.starts_with("--strict=") => {
                // git parses the fsck message-type list here, in the argument
                // loop, and dies before any positional check when it is
                // malformed. Reproduce that so a bad `--strict=<v>` reports
                // git's exact fatal rather than the deferred usage block.
                if let Err(code) = validate_fsck_msg_types(&a["--strict=".len()..]) {
                    return Ok(code);
                }
                opts.strict = true;
                opts.msg_types = Some(a["--strict=".len()..].to_string());
            }
            _ if a.starts_with("--fsck-objects=") => {
                if let Err(code) = validate_fsck_msg_types(&a["--fsck-objects=".len()..]) {
                    return Ok(code);
                }
                opts.fsck_objects = true;
                opts.msg_types = Some(a["--fsck-objects=".len()..].to_string());
            }
            _ if a.starts_with("--threads=") => {
                // git validates the number here and answers with usage, not a
                // fatal, when it does not parse.
                let Some(n) = parse_threads(&a["--threads=".len()..]) else {
                    return Ok(usage_error());
                };
                opts.threads = n;
            }
            _ if a.starts_with("--max-input-size=") => {
                // git: `max_input_size = strtoumax(arg, NULL, 10)`; base 10,
                // trailing junk ignored, and 0 leaves the bound disabled.
                let (n, _) = strtoul(&a["--max-input-size=".len()..], 10);
                opts.max_input_size = (n != 0).then_some(n);
            }
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
            anyhow::bail!("unsupported: `--verify --stdin` (only verifying a pack already on disk is ported)");
        }
        return verify_existing(&opts, &index_name);
    }

    if opts.stdin {
        reject_unported(&opts)?;
        return Ok(die_on_error(index_from_stdin(
            &opts,
            opts.pack.as_deref(),
            index_name.as_deref(),
        )));
    }

    let pack_path = opts.pack.clone().expect("checked above");
    let index_name = index_name.expect("a pack name always yields an index name");
    Ok(die_on_error(index_pack_file(&opts, &pack_path, &index_name)))
}

/// Every way `index-pack` can fail past option parsing is a `die()` in git, so
/// none of them may surface as the dispatcher's exit 1. Whatever the pack
/// machinery could not do becomes git's exit 128 with a `fatal:` line; the
/// wording of the cases git names explicitly is produced ahead of this, by
/// [`parse_pack_header`] and the callers' own checks.
fn die_on_error(outcome: Result<ExitCode>) -> ExitCode {
    match outcome {
        Ok(code) => code,
        Err(e) => fatal(format!("{e:#}")),
    }
}

/// `index-pack.c::parse_pack_header`, which runs before a byte of pack body is
/// read: `fill()` dies with `early EOF` when the twelve header bytes are not
/// there at all, and the signature and version are checked in that order.
/// `Some` is the exit code to die with.
fn parse_pack_header(header: &[u8]) -> Option<ExitCode> {
    if header.len() < 12 {
        return Some(fatal("early EOF"));
    }
    if &header[0..4] != b"PACK" {
        return Some(fatal("pack signature mismatch"));
    }
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if version != 2 && version != 3 {
        return Some(fatal(format!("pack version {version} unsupported")));
    }
    None
}

/// The six-character tail `mkstemp()` puts on `tmp_pack_XXXXXX`. Uniqueness is
/// all that is asked of it — the name is never parsed, only replaced by a rename
/// or left behind.
fn mkstemp_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:03x}{:03x}", std::process::id() & 0xfff, nanos & 0xfff)
}

/// Read the twelve header bytes off `stream` without consuming more, so they can
/// be chained back in front of it. A short read is returned as-is and refused by
/// [`parse_pack_header`], which is what keeps an empty input answering
/// `early EOF` instead of a gitoxide streaming error.
fn peek_pack_header(stream: &mut dyn Read) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 12];
    let mut filled = 0;
    while filled < header.len() {
        match stream.read(&mut header[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(header[..filled].to_vec())
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

    // git bounds the pack it reads by `--max-input-size`; on disk the whole file
    // is the input, so its size is the byte count git's `consumed_bytes` reaches.
    if let Some(limit) = opts.max_input_size {
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        if size > limit {
            return Ok(fatal(format!(
                "pack exceeds maximum allowed size ({})",
                humanise(limit)
            )));
        }
    }

    reject_unported(opts)?;
    if opts.keep.is_some() {
        anyhow::bail!("unsupported: `--keep` without `--stdin`");
    }

    // `parse_pack_header()` is the first thing git does with the bytes, and its
    // three deaths outrank anything the pack decoder below could report.
    let mut file = io::BufReader::new(file);
    if let Some(code) = parse_pack_header(&peek_pack_header(&mut file)?) {
        return Ok(code);
    }
    drop(file);

    let hash = write_index_for_pack(opts, pack_path, index_path)?;

    if want_rev_index(opts) {
        write_rev_index(index_path, &hash)?;
    }
    set_read_only(index_path)?;

    println!("{hash}");
    Ok(ExitCode::SUCCESS)
}

/// Index the pack at `pack_path` into `index_path`, returning the pack checksum.
///
/// Shared by the named-pack path and the zero-object `--stdin` path. The index
/// is built in a sibling temporary and renamed into place, so a failure never
/// leaves a half-written index behind — the same `git_mkstemp`/`rename` dance
/// git performs.
fn write_index_for_pack(opts: &Opts, pack_path: &Path, index_path: &Path) -> Result<ObjectId> {
    let file = io::BufReader::new(fs::File::open(pack_path)?);
    let mut entries = pack::data::input::BytesToEntriesIter::new_from_header(
        file,
        pack::data::input::Mode::Verify,
        pack::data::input::EntryDataMode::Crc32,
        Kind::Sha1,
    )?;
    let pack_version = entries.version();

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

    // The fsck passes run against the finished index while it is still the
    // temporary, so a failure leaves the repository exactly as git leaves it:
    // the checks git runs before `write_idx_file()` have already failed by then.
    if opts.strict || opts.fsck_objects {
        if let Err(e) = fsck_pack(pack_path, &tmp, opts.strict) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    }

    fs::rename(&tmp, index_path)?;
    Ok(outcome.data_hash)
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
        anyhow::bail!("unsupported: `--verify --keep` (the .keep file is not written here)");
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
        Ok(_) => {}
        // git's per-corruption diagnostics are not reproduced; report the real
        // failure rather than inventing text that only looks like git's.
        Err(e) => crate::git_fatal!("--verify failed for '{name}': {e}"),
    }
    // `do_fsck_object` is independent of `--verify`: the object checks still run
    // over everything the pack holds.
    if opts.strict || opts.fsck_objects {
        fsck_pack(&pack_path, index_path, opts.strict)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Stream a pack from stdin, then report it git's way.
///
/// With no `<pack-file>` the pack lands in `objects/pack/pack-<hash>.{pack,idx}`
/// under a name derived from its content. A `<pack-file>` argument names the
/// copy git writes instead — opened `O_CREAT|O_EXCL`, so an existing path is
/// fatal — with the index name derived from it (or from `-o`). `--fix-thin`
/// completes a thin pack by resolving its REF_DELTA bases against the object
/// database; `--max-input-size=<n>` bounds the bytes read from stdin.
fn index_from_stdin(
    opts: &Opts,
    target_pack: Option<&Path>,
    target_index: Option<&Path>,
) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // git opens a named output pack with O_CREAT|O_EXCL before reading stdin, so
    // a path that already exists is fatal with xopen's create-mode wording.
    if let Some(p) = target_pack {
        if let Err(e) = fs::OpenOptions::new().write(true).create_new(true).open(p) {
            return Ok(fatal(format!(
                "unable to create '{}': {}",
                p.display(),
                strerror(&e)
            )));
        }
    }

    // `open_pack_file(NULL)`: with no `<pack-file>` named, git streams stdin into
    // `objects/pack/tmp_pack_XXXXXX` and renames that file into place once the
    // pack is complete. It is created before the header is even parsed and git
    // registers no cleanup for it, so every failure from here on leaves the
    // empty temporary behind — which is part of the state a failed
    // `index-pack --stdin` leaves in the repository, and so is reproduced.
    let temp_pack = match target_pack {
        Some(_) => None,
        None => {
            let dir = repo.objects.store_ref().path().join("pack");
            fs::create_dir_all(&dir)?;
            let path = dir.join(format!("tmp_pack_{}", mkstemp_suffix()));
            fs::File::create(&path)?;
            Some(path)
        }
    };

    // Bound the input to `--max-input-size` by reading at most one byte past the
    // limit: being able to read that extra byte proves the pack is too big,
    // exactly as git's `consumed_bytes > max_input_size` check does.
    let stdin = io::stdin();
    let mut cursor;
    let mut locked;
    let input: &mut dyn io::BufRead = match opts.max_input_size {
        Some(limit) => {
            let mut buf = Vec::new();
            stdin.lock().take(limit.saturating_add(1)).read_to_end(&mut buf)?;
            if buf.len() as u64 > limit {
                return Ok(fatal(format!(
                    "pack exceeds maximum allowed size ({})",
                    humanise(limit)
                )));
            }
            cursor = io::Cursor::new(buf);
            &mut cursor
        }
        None => {
            locked = stdin.lock();
            &mut locked
        }
    };

    // `parse_pack_header()` runs before any of the pack body is decoded, so an
    // input too short to hold a header is `early EOF` rather than whatever the
    // streaming decoder would have said about the truncation.
    let header = peek_pack_header(input)?;
    if let Some(code) = parse_pack_header(&header) {
        return Ok(code);
    }
    let object_count = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    let mut chained = io::Cursor::new(header).chain(input);
    let input: &mut dyn io::BufRead = &mut chained;

    // Where the pack is written before it is renamed onto its destination.
    let write_dir = match target_pack {
        Some(p) => match p.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        },
        None => repo.objects.store_ref().path().join("pack"),
    };

    // A header declaring zero objects is ordinary input to git: the object loop
    // runs zero times, the trailer is checked, and pack, index and rev index are
    // written like any other pack. gix's `write_to_directory` instead discards
    // both files when it indexed nothing, so the empty pack is staged and indexed
    // here through the same index writer the named-pack path uses.
    let (hash, data_path, index_path, gix_keep) = if object_count == 0 {
        let (hash, data_path, index_path) =
            index_empty_from_stdin(opts, input, target_pack, target_index, temp_pack.as_deref(), &write_dir)?;
        (hash, data_path, index_path, None)
    } else {
        // With `--fix-thin` the object database resolves REF_DELTA bases missing
        // from the pack, completing a thin pack in place; without it the pack is
        // copied through byte-for-byte and a thin base is a fatal `NotFound`.
        let resolver = opts.fix_thin.then(|| repo.objects.clone());
        let outcome = pack::Bundle::write_to_directory(
            input,
            Some(&write_dir),
            &mut gix::progress::Discard,
            &AtomicBool::new(false),
            resolver,
            pack::bundle::write::Options {
                thread_limit: opts.threads,
                object_hash: Kind::Sha1,
                ..Default::default()
            },
        )?;

        let hash = outcome.index.data_hash;
        let (Some(gix_data), Some(gix_index)) = (&outcome.data_path, &outcome.index_path) else {
            bail!("gitoxide indexed no objects from a pack whose header declared {object_count}");
        };

        // Before the pack is moved anywhere, so a failing check leaves nothing
        // in `objects/pack` — the state git leaves, since its own checks run
        // ahead of `write_idx_file()`.
        if opts.strict || opts.fsck_objects {
            if let Err(e) = fsck_pack(gix_data, gix_index, opts.strict) {
                for path in [gix_data, gix_index].into_iter().chain(outcome.keep_path.as_ref()) {
                    let _ = fs::remove_file(path);
                }
                return Err(e);
            }
        }

        // Move gix's hash-named files onto git's chosen destinations, if any.
        let (data_path, index_path): (PathBuf, PathBuf) = match (target_pack, target_index) {
            (Some(tp), Some(ti)) => {
                fs::rename(gix_index, ti)?;
                fs::rename(gix_data, tp)?;
                (tp.to_path_buf(), ti.to_path_buf())
            }
            (None, Some(ti)) => {
                fs::rename(gix_index, ti)?;
                (gix_data.clone(), ti.to_path_buf())
            }
            (Some(tp), None) => {
                fs::rename(gix_data, tp)?;
                (tp.to_path_buf(), gix_index.clone())
            }
            (None, None) => (gix_data.clone(), gix_index.clone()),
        };
        (hash, data_path, index_path, outcome.keep_path.clone())
    };

    // `write_to_directory` always drops a `.keep` beside the pack it wrote; git
    // only leaves one when asked, so drop gix's and, under `--keep`, write our
    // own beside the final pack holding the requested message.
    if let Some(kp) = &gix_keep {
        let _ = fs::remove_file(kp);
    }

    // git's temporary *becomes* the finished pack through a rename; gix wrote its
    // own file instead, so the placeholder goes now that the run has succeeded.
    if let Some(tmp) = &temp_pack {
        let _ = fs::remove_file(tmp);
    }

    if want_rev_index(opts) {
        write_rev_index(&index_path, &hash)?;
    }
    set_read_only(&index_path)?;
    set_read_only(&data_path)?;

    match &opts.keep {
        Some(msg) => {
            let keep_path = data_path.with_extension("keep");
            let body = msg.as_ref().map(|m| format!("{m}\n")).unwrap_or_default();
            fs::write(&keep_path, body)?;
            fs::set_permissions(&keep_path, fs::Permissions::from_mode(0o600))?;
            println!("keep\t{hash}");
        }
        None => println!("pack\t{hash}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Stream a pack whose header declares zero objects from `input` and index it.
///
/// Returns the pack checksum and the final pack and index paths. git streams
/// stdin straight into its output file — the named `<pack-file>` when there is
/// one, otherwise `tmp_pack_XXXXXX`, which is then renamed to the
/// content-addressed name — so that is what happens here; the index is built
/// beside the staged pack and renamed alongside it.
fn index_empty_from_stdin(
    opts: &Opts,
    input: &mut dyn io::BufRead,
    target_pack: Option<&Path>,
    target_index: Option<&Path>,
    temp_pack: Option<&Path>,
    write_dir: &Path,
) -> Result<(ObjectId, PathBuf, PathBuf)> {
    let staged = match (target_pack, temp_pack) {
        (Some(p), _) => p.to_path_buf(),
        (None, Some(t)) => t.to_path_buf(),
        (None, None) => write_dir.join(format!("tmp_pack_{}", mkstemp_suffix())),
    };
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes)?;
    fs::write(&staged, &bytes)?;

    // The pack's own checksum names it, so the index is written beside the
    // staged pack first and both are moved once the checksum is known.
    let staged_index = with_suffix(&staged, ".idx");
    let hash = write_index_for_pack(opts, &staged, &staged_index)?;

    let data_path = match target_pack {
        Some(p) => p.to_path_buf(),
        None => write_dir.join(format!("pack-{hash}.pack")),
    };
    if data_path != staged {
        fs::rename(&staged, &data_path)?;
    }
    let index_path = match target_index {
        Some(p) => p.to_path_buf(),
        None => data_path.with_extension("idx"),
    };
    if index_path != staged_index {
        fs::rename(&staged_index, &index_path)?;
    }
    Ok((hash, data_path, index_path))
}

/// `--strict` / `--fsck-objects`: the fsck passes git runs over the pack.
///
/// `fsck_options_init(&fsck_options, repo, FSCK_OPTIONS_MISSING_GITMODULES)`
/// gives `index-pack` `strict = 1` and *no* configuration: `git_index_pack_config()`
/// reads four `pack.*`/`core.*` keys and nothing from `fsck.*`. So every severity
/// is the static default from `fsck.h` with `fsck_msg_severity()`'s promotion of
/// an unconfigured warning to an error — see [`strict_severity`]. Three passes
/// can fail:
///
/// * `fsck_object()` from `sha1_object()`, on every object the pack holds →
///   `fsck error in packed object`.
/// * `fsck_walk()` plus `check_objects()`, under `--strict` only: every object a
///   packed object links to must be in the pack or already in the object
///   database, with the type the link implies → `did not receive expected
///   object <oid>` or `object <oid>: expected type <a>, found <b>`.
/// * `fsck_finish()`, which lints the `.gitmodules` and `.gitattributes` blobs
///   the tree walk queued → `fsck error in pack objects`.
///
/// git runs the first two before it writes the index and the third after the
/// rename. Here all three run against a finished index that has not been moved
/// into place, so a failure still leaves no artifact behind — the caller removes
/// the temporary it passed.
fn fsck_pack(pack_path: &Path, index_path: &Path, strict: bool) -> Result<()> {
    use super::fsck::{check_blob, check_object, Severity};
    use gix::object::Kind as ObjKind;

    let bundle = pack::Bundle {
        index: pack::index::File::at(index_path, Kind::Sha1)?,
        pack: pack::data::File::at(pack_path, Kind::Sha1)?,
    };

    // `sha1_object()` sees objects in the order they are unpacked, so the first
    // diagnostic is the one lowest in the pack.
    let mut order: Vec<(u64, u32)> = (0..bundle.index.num_objects())
        .map(|i| (bundle.index.pack_offset_at_index(i), i))
        .collect();
    order.sort_unstable();

    let in_pack: std::collections::HashSet<ObjectId> =
        (0..bundle.index.num_objects()).map(|i| bundle.index.oid_at_index(i).to_owned()).collect();

    let mut inflate = gix::zlib::Inflate::default();
    let mut cache = pack::cache::Never;
    let mut buf = Vec::new();
    let mut queued: Vec<(ObjectId, bool, bool)> = Vec::new();
    let mut links: Vec<(ObjectId, ObjKind)> = Vec::new();
    let mut object_error = false;

    for (_, index) in &order {
        let id = bundle.index.oid_at_index(*index).to_owned();
        let (object, _) = bundle.get_object_by_index(*index, &mut buf, &mut inflate, &mut cache)?;
        let (kind, data) = (object.kind, object.data.to_vec());

        let checked = check_object(kind, &data, true);
        // `init_tree_desc_gently()` and `update_tree_entry_gently()` call
        // `error()` themselves, with no msg-id and so no severity to consult.
        for line in &checked.raw {
            eprintln!("error: {line}");
            object_error = true;
        }
        for finding in &checked.findings {
            match strict_severity(finding.msg) {
                Severity::Ignore => {}
                Severity::Info | Severity::Warn => {
                    eprintln!("warning: object {id}: {}: {}", finding.msg.id, finding.text);
                }
                Severity::Error | Severity::Fatal => {
                    eprintln!("error: object {id}: {}: {}", finding.msg.id, finding.text);
                    object_error = true;
                }
            }
        }
        for blob in &checked.gitmodules {
            queued.push((*blob, true, false));
        }
        for blob in &checked.gitattributes {
            queued.push((*blob, false, true));
        }
        if strict {
            collect_links(kind, &data, &mut links);
        }
    }
    if object_error {
        crate::git_fatal!("fsck error in packed object");
    }

    // `check_objects()`: a linked object already in this pack carries
    // `FLAG_CHECKED` and is skipped; anything else has to be in the object
    // database with the type the link gave it.
    if strict && !links.is_empty() {
        let repo = gix::discover(".")?;
        use gix::odb::HeaderExt;
        for (id, expected) in &links {
            if in_pack.contains(id) {
                continue;
            }
            let Ok(header) = repo.objects.header(id) else {
                crate::git_fatal!("did not receive expected object {id}");
            };
            if header.kind() != *expected {
                crate::git_fatal!(
                    "object {id}: expected type {expected}, found {}",
                    header.kind()
                );
            }
        }
    }

    // `fsck_finish()`. The pack has been loaded into the store by now in git, so
    // a queued blob is looked up across the whole database — here the pack is
    // still only a pair of files, so it is consulted directly first.
    if !queued.is_empty() {
        let repo = gix::discover(".")?;
        let mut finish_error = false;
        let mut done: Vec<ObjectId> = Vec::new();
        for (id, as_modules, as_attrs) in &queued {
            if done.contains(id) {
                continue;
            }
            done.push(*id);
            let mut found = bundle
                .find(id, &mut buf, &mut inflate, &mut cache)
                .ok()
                .flatten()
                .map(|(object, _)| (object.kind, object.data.to_vec()));
            if found.is_none() {
                found = repo.find_object(*id).ok().map(|o| (o.kind, o.data.clone()));
            }
            let label = if *as_modules { ".gitmodules" } else { ".gitattributes" };
            let Some((kind, data)) = found else {
                // `fsck_objects_error_cb_print_missing_gitmodules()` answers a
                // missing `.gitmodules` by printing its id and *not* failing:
                // that id is what `unpack-objects`' caller fetches next.
                if *as_modules {
                    println!("{id}");
                } else {
                    eprintln!("error: object {id}: gitattributesMissing: unable to read {label} blob");
                    finish_error = true;
                }
                continue;
            };
            if kind != ObjKind::Blob {
                let msg = if *as_modules { "gitmodulesBlob" } else { "gitattributesBlob" };
                eprintln!("error: object {id}: {msg}: non-blob found at {label}");
                finish_error = true;
                continue;
            }
            for finding in check_blob(&data, *as_modules, *as_attrs) {
                match strict_severity(finding.msg) {
                    Severity::Ignore => {}
                    Severity::Info | Severity::Warn => {
                        eprintln!("warning: object {id}: {}: {}", finding.msg.id, finding.text);
                    }
                    Severity::Error | Severity::Fatal => {
                        eprintln!("error: object {id}: {}: {}", finding.msg.id, finding.text);
                        finish_error = true;
                    }
                }
            }
        }
        if finish_error {
            crate::git_fatal!("fsck error in pack objects");
        }
    }
    Ok(())
}

/// `fsck_msg_severity()` for `index-pack`: its options always carry `strict = 1`
/// and nothing configures a row, so an unconfigured warning becomes an error and
/// every other default is left alone.
fn strict_severity(msg: &super::fsck::Msg) -> super::fsck::Severity {
    use super::fsck::Severity;
    match msg.default {
        Severity::Warn => Severity::Error,
        other => other,
    }
}

/// `fsck_walk_{commit,tree,tag}`: the objects `obj` links to, each with the type
/// the link gives it. A gitlink is skipped — it names a commit in another
/// repository, which `fsck_walk_tree()` never follows.
fn collect_links(kind: gix::object::Kind, data: &[u8], out: &mut Vec<(ObjectId, gix::object::Kind)>) {
    use gix::object::Kind as ObjKind;
    match kind {
        ObjKind::Commit => {
            let Ok(commit) = gix::objs::CommitRef::from_bytes(data, Kind::Sha1) else { return };
            out.push((commit.tree(), ObjKind::Tree));
            out.extend(commit.parents().map(|id| (id, ObjKind::Commit)));
        }
        ObjKind::Tree => {
            let Ok(tree) = gix::objs::TreeRef::from_bytes(data, Kind::Sha1) else { return };
            for entry in tree.entries {
                match entry.mode.kind() {
                    gix::objs::tree::EntryKind::Commit => {}
                    gix::objs::tree::EntryKind::Tree => {
                        out.push((entry.oid.to_owned(), ObjKind::Tree));
                    }
                    _ => out.push((entry.oid.to_owned(), ObjKind::Blob)),
                }
            }
        }
        ObjKind::Tag => {
            let Ok(tag) = gix::objs::TagRef::from_bytes(data, Kind::Sha1) else { return };
            out.push((tag.target(), tag.target_kind));
        }
        ObjKind::Blob => {}
    }
}

/// Reject the flags stock git implements that this port does not.
///
/// Called only after every check git performs first has passed, so a terse
/// refusal here can never hide an error git would have reported instead. Each
/// message names the flag and why it is not honoured; none of these are
/// silently ignored, which would turn a wrong answer into an apparent success.
fn reject_unported(opts: &Opts) -> Result<()> {
    if let Some(list) = &opts.msg_types {
        bail!(
            "unsupported fsck severity list {list:?} \
             (the checks run at git's defaults; per-message severities are not honoured)"
        );
    }
    if opts.self_contained {
        bail!("unsupported flag \"--check-self-contained-and-connected\" (no connectivity pass is run here)");
    }
    if opts.promisor {
        bail!("unsupported flag \"--promisor\" (no .promisor file is written here)");
    }
    if opts.pack_header {
        bail!("unsupported flag \"--pack_header\" (internal fetch fast-path is not ported)");
    }
    if let Some(fmt) = &opts.object_format {
        if fmt != "sha1" {
            anyhow::bail!("unsupported object format {fmt:?} (ported: sha1)");
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

/// Give every `.idx` under `pack_dir` the `.rev` file git's `index-pack` would
/// have written beside it.
///
/// git indexes a fetched pack with `index-pack`, which writes the reverse index
/// whenever `pack.writeReverseIndex` allows it (the default). gitoxide's fetch
/// writes the pack and its index through `gix-pack`, which has no reverse-index
/// writer at all, so a clone or fetch that received a pack came out one file
/// short of git's. This fills that gap in after the fetch, for the packs that
/// are missing it; a pack that already has one is left alone.
///
/// Best-effort per pack: a `.rev` that cannot be produced is skipped rather than
/// failing the clone, since git's own `index-pack` treats the reverse index as
/// an accelerator and the repository is complete without it.
pub(super) fn write_missing_rev_indexes(pack_dir: &Path) {
    let Ok(entries) = fs::read_dir(pack_dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|ext| ext != "idx") || path.with_extension("rev").exists() {
            continue;
        }
        // The trailer of the `.idx` is the checksum of the pack it indexes, which
        // is the id the `.rev` records. Reading it from the index rather than the
        // filename keeps this correct for a pack named any other way.
        let Ok(index) = pack::index::File::at(&path, Kind::Sha1) else {
            continue;
        };
        let _ = write_rev_index(&path, &index.pack_checksum());
    }
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

/// Every fsck message id git recognises, each the enum name from
/// `FOREACH_FSCK_MSG_ID` (fsck.h) lowercased with underscores removed — the
/// exact string `parse_msg_id()` compares a lowercased user token against.
const FSCK_MSG_IDS: &[&str] = &[
    "nulinheader", "unterminatedheader", "badheadercontinuation", "baddate",
    "baddateoverflow", "bademail", "badgpgsig", "badheadtarget", "badname",
    "badobjectsha1", "badpackedrefentry", "badpackedrefheader", "badparentsha1",
    "badreferentname", "badrefcontent", "badreffiletype", "badrefname",
    "badrefoid", "badtimezone", "badtree", "badtreesha1", "badtype",
    "duplicateentries", "gitattributesblob", "gitattributeslarge",
    "gitattributeslinelength", "gitattributesmissing", "gitmodulesblob",
    "gitmoduleslarge", "gitmodulesmissing", "gitmodulesname", "gitmodulespath",
    "gitmodulessymlink", "gitmodulesupdate", "gitmodulesurl", "missingauthor",
    "missingcommitter", "missingemail", "missingnamebeforeemail", "missingobject",
    "missingspacebeforedate", "missingspacebeforeemail", "missingtag",
    "missingtagentry", "missingtree", "missingtype", "missingtypeentry",
    "multipleauthors", "packedrefentrynotterminated", "packedrefunsorted",
    "treenotsorted", "unknowntype", "zeropaddeddate", "badreftabletablename",
    "emptyname", "fullpathname", "hasdot", "hasdotdot", "hasdotgit",
    "largepathname", "nullsha1", "nulincommit", "zeropaddedfilemode",
    "badfilemode", "badtagname", "emptypackedrefsfile", "gitattributessymlink",
    "gitignoresymlink", "gitmodulesparse", "mailmapsymlink", "missingtaggerentry",
    "refmissingnewline", "symlinkref", "symreftargetisnotaref",
    "trailingrefcontent", "extraheaderentry",
];

/// The fsck message ids whose default severity is `FSCK_FATAL`; git refuses to
/// demote these to anything other than `error`.
const FSCK_FATAL_IDS: &[&str] = &["nulinheader", "unterminatedheader"];

/// Validate a `--strict=<v>` / `--fsck-objects=<v>` message-type list exactly as
/// git's `fsck_set_msg_types()` → `fsck_set_msg_type()` do, dying (fatal, exit
/// 128) with git's message on the first malformed token.
///
/// git splits the value on space, comma or pipe, skips empty tokens, lowercases
/// the id (the part before the first `=`), and for each token in order:
/// ```text
///   * no `=`                       → `Missing '=': '<id>'`
///   * unknown id                   → `Unhandled message id: <id>`
///   * severity not error/warn/ignore (case-sensitive) → `Unknown fsck message
///     type: '<severity>'`
///   * a `FSCK_FATAL` id set below `error` → `Cannot demote <id> to <severity>`
/// ```
/// A fully valid list returns `Ok`; the flag is still rejected later as
/// unported, but only after every check git performs first has passed.
fn validate_fsck_msg_types(values: &str) -> std::result::Result<(), ExitCode> {
    for token in values.split([' ', ',', '|']) {
        if token.is_empty() {
            continue;
        }
        let Some(eq) = token.find('=') else {
            return Err(fatal(format!("Missing '=': '{}'", token.to_ascii_lowercase())));
        };
        let id = token[..eq].to_ascii_lowercase();
        let severity = &token[eq + 1..];
        if !FSCK_MSG_IDS.contains(&id.as_str()) {
            return Err(fatal(format!("Unhandled message id: {id}")));
        }
        if !matches!(severity, "error" | "warn" | "ignore") {
            return Err(fatal(format!("Unknown fsck message type: '{severity}'")));
        }
        if severity != "error" && FSCK_FATAL_IDS.contains(&id.as_str()) {
            return Err(fatal(format!("Cannot demote {id} to {severity}")));
        }
    }
    Ok(())
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

/// `strbuf_humanise_bytes()` from `strbuf.c`, used for the `--max-input-size`
/// fatal's `(%s)`: git's truncating fraction arithmetic and its `>` (not `>=`)
/// unit boundaries, so `1048576` renders as `1024.00 KiB` and `1` as `1 byte`.
fn humanise(bytes: u64) -> String {
    if bytes > 1 << 30 {
        format!(
            "{}.{:02} GiB",
            bytes >> 30,
            (bytes & ((1 << 30) - 1)) / 10_737_419
        )
    } else if bytes > 1 << 20 {
        let x = bytes + 5243; // git's rounding nudge
        format!("{}.{:02} MiB", x >> 20, ((x & ((1 << 20) - 1)) * 100) >> 20)
    } else if bytes > 1 << 10 {
        let x = bytes + 5;
        format!("{}.{:02} KiB", x >> 10, ((x & ((1 << 10) - 1)) * 100) >> 10)
    } else if bytes == 1 {
        "1 byte".to_string()
    } else {
        format!("{bytes} bytes")
    }
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
