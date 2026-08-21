//! `git verify-pack` — validate a packed git archive and report its contents.
//!
//! Covered: the whole documented option surface — `-v`/`--verbose`/`--no-verbose`,
//! `-s`/`--stat-only`/`--no-stat-only`, clustered short flags (`-sv`), `--`, `-h`,
//! and `--object-format[=<hash>]` for `sha1`. Argument normalisation matches
//! `verify_one_pack()`: a `.pack` suffix is rewritten to `.idx`, anything else
//! gains a `.idx`, and the `<name>.pack: ok` / `<name>.pack: bad` line is printed
//! from the derived pack path exactly as git prints `p->pack_name`.
//!
//! The `-v` object table and the `-s` delta-chain histogram are reproduced
//! byte-for-byte, including git's pack-offset iteration order, the `%-6s` type
//! column, the `non delta:` line preceding the `chain length =` lines, the
//! `chain length > 15:` bucket, and the singular/plural `object`/`objects`
//! wording. The usage block and its exit code (129) match stock git, as does the
//! exit code on a pack that cannot be opened or does not verify (1).
//!
//! Sizes follow git exactly. `verify-pack` is a thin wrapper that runs
//! `index-pack --verify-stat`, so the table is `show_pack_info()`'s, and its
//! third column is `obj->size` — the size field decoded from the entry's own
//! pack header. For a base entry that is the object size; for a delta entry it
//! is the length of the delta instruction stream, *not* the size of the object
//! the delta reconstructs. The fourth column is the entry's on-disk span, the
//! distance to the next entry in pack-offset order or to the start of the pack
//! trailer for the last entry.
//!
//! `--object-format` is resolved per pack rather than up front, because that is
//! where git resolves it: a name it cannot use does not abort the command, it
//! fails each `<pack>` in turn — diagnostic repeated per argument, `<name>.pack:
//! bad` still printed under `-v`/`-s`, exit 1 rather than a `die()` code. This
//! covers `sha256`, which is a real backend here: a genuine SHA-256 pack
//! verifies, and a SHA-1 pack asked for as `sha256` fails the same way it does
//! under git, which sizes the index by the requested algorithm so the length
//! check fails before any object is read.
//!
//! Not covered exactly: the per-object diagnostics git writes to stderr for a
//! corrupt pack, which are replaced by a single terse `error:` line; stdout and
//! the exit code still match.
//! Nothing is written to the repository, so post-command state is unchanged.

use anyhow::Result;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::hash::ObjectId;
use gix::odb::pack;

/// Stock git's `verify-pack` usage block, byte-for-byte, including the trailing
/// blank line. Printed on `-h` (stdout) and for a usage error (stderr).
/// `cmd_verify_pack()`'s `struct option verify_pack_options[]`
/// (builtin/verify-pack.c), in table order, as [`super::resolve_long`] reads it.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "verbose",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "stat-only",                   neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "object-format",               neg: true,  arg: super::Arg::Required },
];

const USAGE: &str = "usage: git verify-pack [-v | --verbose] [-s | --stat-only] [--] <pack>.idx...\n\
                     \n\
                     \x20   -v, --[no-]verbose    verbose\n\
                     \x20   -s, --[no-]stat-only  show statistics only\n\
                     \x20   --[no-]object-format <hash>\n\
                     \x20                         specify the hash algorithm to use\n\
                     \n";

/// `MAX_CHAIN` from `pack-check.c`: chains longer than this collapse into the
/// single `chain length > 15` bucket.
const MAX_CHAIN: u32 = 15;

/// `git verify-pack` — verify pack index and pack data, optionally listing objects.
///
/// Supported forms (matching stock git byte-for-byte, including exit codes):
///   * `git verify-pack <pack>.idx`          → verify silently, exit 0/1
///   * `-v` / `--verbose`                    → object table, histogram, `<pack>: ok`
///   * `-s` / `--stat-only`                  → histogram only, no verification
///   * `-sv`, `--no-verbose`, `--no-stat-only`, `--`
///   * `--object-format[=]sha1`, `--no-object-format`
///   * `-h`                                  → usage on stdout, exit 129
///
/// Several `<pack>` arguments are processed in order; the exit code is 1 if any
/// one of them failed, exactly as `cmd_verify_pack()` accumulates `err`.
pub fn verify_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0; `verify-pack` only takes pack paths
    // as positionals, so dropping a leading copy is unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("verify-pack") => &args[1..],
        _ => args,
    };

    let mut verbose = false;
    let mut stat_only = false;
    let mut object_format: Option<String> = None;
    let mut packs: Vec<&str> = Vec::new();
    let mut end_of_opts = false;

    let mut it = args.iter();
    while let Some(typed) = it.next() {
        let typed = typed.as_str();
        let a = typed;
        if end_of_opts {
            packs.push(typed);
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, which is why it is absent from [`LONG_OPTS`] and
        // matched here rather than after resolution. verify-pack's table has no
        // `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block `-h`
        // prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // Respell a unique abbreviation as the name it resolves to, so an
        // abbreviation lands on the arm its full spelling lands on.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "--stat-only" => stat_only = true,
            "--no-stat-only" => stat_only = false,
            "--no-object-format" => object_format = None,
            "--object-format" => match it.next() {
                Some(v) => object_format = Some(v.clone()),
                // parse-options: a missing required value is a usage error.
                // `optname()` names a *long* option `option `<name>'`; the
                // `switch` wording is `OPT_SHORT`'s, and `--object-format` has no
                // short spelling at all (parse-options.c:30-45).
                None => {
                    return Ok(crate::parseopt::requires_value(
                        crate::parseopt::OptName::Long("object-format"),
                    ))
                }
            },
            s if s.starts_with("--object-format=") => {
                object_format = Some(s["--object-format=".len()..].to_string());
            }
            s if s.starts_with("--") => {
                return Ok(usage_error(Some(&format!("unknown option `{}'", &s[2..]))));
            }
            s if s.len() > 1 && s.starts_with('-') => {
                // Clustered short switches, e.g. `-sv`.
                for c in s[1..].chars() {
                    match c {
                        'v' => verbose = true,
                        's' => stat_only = true,
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(Some(&format!("unknown switch `{c}'")))),
                    }
                }
            }
            // A non-option argument is handed back unchanged by the resolver, so the
            // argv slice itself is pushed and the operand keeps `args`' lifetime.
            _ => packs.push(typed),
        }
    }

    if packs.is_empty() {
        return Ok(usage_error(None));
    }

    // Resolving `--object-format` is deliberately *not* a fatal, up-front step.
    // Stock git defers the consequence to `verify_one_pack()`, so a rejected
    // algorithm behaves exactly like a pack that could not be opened: the
    // diagnostic is repeated once per `<pack>` argument, each pack still prints
    // its `<name>.pack: bad` line under `-v`/`-s`, and the process exits 1 rather
    // than with a `die()` code. Verified against git 2.55.0:
    //   $ git verify-pack -v --object-format=bogus nope1 nope2
    //   stdout: "nope1.pack: bad" / "nope2.pack: bad"
    //   stderr: "fatal: unknown hash algorithm 'bogus'" (twice), exit 1
    let choice = match object_format.as_deref() {
        None => {
            // git falls back to the repository's algorithm; verify-pack also runs
            // outside a repository, where SHA-1 is the only thing it can assume.
            HashChoice::Kind(
                gix::discover(".")
                    .map(|r| r.object_hash())
                    .unwrap_or(gix::hash::Kind::Sha1),
            )
        }
        Some("sha1") => HashChoice::Kind(gix::hash::Kind::Sha1),
        Some("sha256") => HashChoice::Kind(gix::hash::Kind::Sha256),
        Some(other) => HashChoice::Unknown(other.to_string()),
    };

    let mut err = false;
    for path in packs {
        if !verify_one(path, verbose, stat_only, &choice) {
            err = true;
        }
    }
    Ok(if err {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// git's parse-options failure shape: an optional `error: <msg>` line followed by
/// the usage block, both on stderr, exit 129. A missing `<pack>` argument goes
/// straight to `usage_with_options()` with no `error:` line.
fn usage_error(msg: Option<&str>) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{USAGE}"),
        None => eprint!("{USAGE}"),
    }
    ExitCode::from(129)
}

/// The outcome of `--object-format`, applied once per `<pack>` argument.
///
/// git resolves the algorithm name late, so a name it cannot use is reported for
/// every pack rather than aborting the command — see the note in [`verify_pack`].
enum HashChoice {
    /// An algorithm this build can open a pack with — both of git's, since
    /// `gix-hash`'s `sha256` feature is on.
    Kind(gix::hash::Kind),
    /// Any other name — git's `unknown hash algorithm` path.
    Unknown(String),
}

/// Verify a single pack named by `path`, returning `false` when it is bad.
///
/// Mirrors `verify_one_pack()` + `verify_pack()`: the argument is normalised to
/// an `.idx` path, verification is skipped entirely under `--stat-only`, and the
/// report — object table, histogram, trailing `ok`/`bad` line — is only produced
/// when `-v` or `-s` was given.
fn verify_one(path: &str, verbose: bool, stat_only: bool, choice: &HashChoice) -> bool {
    // "foo.idx" stays, "foo.pack" becomes "foo.idx", "foo" becomes "foo.idx".
    let idx_path = PathBuf::from(match path.strip_suffix(".pack") {
        Some(base) => format!("{base}.idx"),
        None if path.ends_with(".idx") => path.to_string(),
        None => format!("{path}.idx"),
    });
    // `p->pack_name`, the name git reports on the ok/bad line.
    let pack_name = {
        let s = idx_path.to_string_lossy();
        format!("{}.pack", &s[..s.len() - 4])
    };
    let pack_path = PathBuf::from(&pack_name);

    // A rejected `--object-format` fails this pack the same way an unopenable one
    // does: `bad` on stdout under `-v`/`-s`, diagnostic on stderr, `false` here.
    let hash = match choice {
        HashChoice::Kind(k) => *k,
        HashChoice::Unknown(name) => {
            eprintln!("fatal: unknown hash algorithm '{name}'");
            if verbose || stat_only {
                println!("{pack_name}: bad");
            }
            return false;
        }
    };

    // `add_packed_git()` needs both halves; a missing or unreadable one yields the
    // same message for either, keyed on the `.idx` path git was asked to open.
    let opened = pack::index::File::at(&idx_path, hash)
        .ok()
        .zip(pack::data::File::at(&pack_path, hash).ok());
    let Some((idx, data)) = opened else {
        eprintln!(
            "fatal: Cannot open existing pack file '{}'",
            idx_path.display()
        );
        if verbose || stat_only {
            println!("{pack_name}: bad");
        }
        return false;
    };

    if !stat_only {
        let opts = pack::index::verify::integrity::Options {
            // Git checks each object's hash against the index and the two file
            // checksums; it never re-encodes objects, so the stricter modes would
            // reject packs git accepts.
            verify_mode: pack::index::verify::Mode::HashCrc32,
            ..Default::default()
        };
        let outcome = idx.verify_integrity(
            Some(pack::index::verify::PackContext {
                data: &data,
                options: opts,
            }),
            &mut gix::progress::Discard,
            &AtomicBool::new(false),
        );
        if let Err(e) = outcome {
            eprintln!("error: {e}");
            if verbose || stat_only {
                println!("{pack_name}: bad");
            }
            return false;
        }
    }

    if verbose || stat_only {
        match show_pack_info(&idx, &data, !stat_only) {
            Ok(()) => {
                if !stat_only {
                    println!("{pack_name}: ok");
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                println!("{pack_name}: bad");
                return false;
            }
        }
    }
    true
}

/// One row of the `-v` table, in pack-offset order.
struct Row {
    oid: ObjectId,
    offset: u64,
    kind: &'static str,
    /// Size decoded from this entry's own pack header (`obj->size`): the object
    /// size for a base entry, the delta stream length for a delta entry.
    size: u64,
    /// Bytes this entry occupies in the pack (`oi.disk_sizep`).
    disk: u64,
    /// Delta chain length; `0` for a base object.
    depth: u32,
    /// Immediate delta base, present exactly when `depth > 0`.
    base: Option<ObjectId>,
}

/// `show_pack_info()` — emit the object table (when `list` is set) followed by the
/// delta-chain histogram.
fn show_pack_info(
    idx: &pack::index::File,
    data: &pack::data::File,
    list: bool,
) -> Result<(), pack::data::decode::Error> {
    // git walks the pack in offset order, so the table is ascending by offset and
    // each entry's on-disk span is the gap to its successor.
    let mut entries: Vec<(u64, ObjectId)> = idx.iter().map(|e| (e.pack_offset, e.oid)).collect();
    entries.sort_unstable_by_key(|(off, _)| *off);

    // Offset -> object id, for naming the base of an OFS_DELTA entry. `entries` is
    // already sorted by offset, so a binary search suffices.
    let oid_at = |off: u64| -> Option<ObjectId> {
        entries
            .binary_search_by_key(&off, |(o, _)| *o)
            .ok()
            .map(|i| entries[i].1)
    };

    let mut inflate = gix::zlib::Inflate::default();
    // Resolving a REF_DELTA base only needs its in-pack entry.
    let resolve = |id: &gix::hash::oid| -> Option<pack::data::decode::header::ResolvedBase> {
        let i = idx.lookup(id)?;
        let entry = data.entry(idx.pack_offset_at_index(i)).ok()?;
        Some(pack::data::decode::header::ResolvedBase::InPack(entry))
    };

    let pack_end = data.pack_end() as u64;
    let mut rows: Vec<Row> = Vec::with_capacity(entries.len());
    for (i, &(offset, oid)) in entries.iter().enumerate() {
        let entry = data.entry(offset)?;
        let base = match entry.header {
            pack::data::entry::Header::OfsDelta { base_distance } => {
                oid_at(offset.saturating_sub(base_distance))
            }
            pack::data::entry::Header::RefDelta { base_id } => Some(base_id),
            _ => None,
        };
        // `obj->size` in `show_pack_info()` is the size decoded from *this*
        // entry's own header, which for a delta is the length of the delta
        // instruction stream — not the size of the object it reconstructs.
        let entry_size = entry.decompressed_size;
        let info = data.decode_header(entry, &mut inflate, &resolve)?;
        let next = entries.get(i + 1).map_or(pack_end, |(o, _)| *o);
        rows.push(Row {
            oid,
            offset,
            kind: type_name(info.kind),
            size: entry_size,
            disk: next.saturating_sub(offset),
            depth: info.num_deltas,
            base,
        });
    }

    // `chain_histogram[0]` collects everything past MAX_CHAIN, exactly as git does.
    let mut histogram = vec![0u64; MAX_CHAIN as usize + 1];
    let mut baseobjects = 0u64;

    for r in &rows {
        if list {
            print!("{} ", r.oid.to_hex());
        }
        if r.depth == 0 {
            if list {
                println!("{:<6} {} {} {}", r.kind, r.size, r.disk, r.offset);
            }
            baseobjects += 1;
        } else {
            if list {
                // A delta whose base is outside the pack cannot be named; git
                // never reaches this state for a non-thin pack.
                let base = r.base.map_or_else(String::new, |b| b.to_hex().to_string());
                println!(
                    "{:<6} {} {} {} {} {}",
                    r.kind, r.size, r.disk, r.offset, r.depth, base
                );
            }
            if r.depth <= MAX_CHAIN {
                histogram[r.depth as usize] += 1;
            } else {
                histogram[0] += 1;
            }
        }
    }

    if baseobjects != 0 {
        println!("non delta: {baseobjects} {}", objects(baseobjects));
    }
    // `cnt` is both the histogram index and the printed chain length, and the
    // range starts at 1 (index 0 is reported separately), so enumerate is a poor fit.
    #[allow(clippy::needless_range_loop)]
    for cnt in 1..=MAX_CHAIN as usize {
        let n = histogram[cnt];
        if n != 0 {
            println!("chain length = {cnt}: {n} {}", objects(n));
        }
    }
    if histogram[0] != 0 {
        let n = histogram[0];
        println!("chain length > {MAX_CHAIN}: {n} {}", objects(n));
    }
    Ok(())
}

/// The type column as `type_name()` in `object.c` spells it. Taken as a `&str`
/// rather than formatting `gix::object::Kind` directly, because that `Display`
/// impl writes through `write_str` and so silently ignores the `%-6s` padding.
fn type_name(kind: gix::object::Kind) -> &'static str {
    use gix::object::Kind::*;
    match kind {
        Commit => "commit",
        Tree => "tree",
        Blob => "blob",
        Tag => "tag",
    }
}

/// git's `Q_("... object", "... objects", n)` plural selection.
fn objects(n: u64) -> &'static str {
    if n == 1 {
        "object"
    } else {
        "objects"
    }
}
