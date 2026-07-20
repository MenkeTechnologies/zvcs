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
//! Sizes follow git exactly: the third column is the fully resolved object size
//! (for a delta, the result size read from the delta header, not the delta
//! stream), and the fourth is the entry's on-disk span — the distance to the next
//! entry in pack-offset order, or to the start of the pack trailer for the last
//! entry, matching `packed_object_info()`'s `disk_sizep`.
//!
//! Not covered exactly: `--object-format=sha256` bails, because the vendored gix
//! build does not enable the `sha256` feature, so no SHA-256 pack can be opened
//! at all. The per-object diagnostics git writes to stderr for a corrupt pack are
//! replaced by a single terse `error:` line; stdout and the exit code still match.
//! Nothing is written to the repository, so post-command state is unchanged.

use anyhow::{bail, Result};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::hash::ObjectId;
use gix::odb::pack;

/// Stock git's `verify-pack` usage block, byte-for-byte, including the trailing
/// blank line. Printed on `-h` (stdout) and for a usage error (stderr).
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
    while let Some(a) = it.next() {
        let a = a.as_str();
        if end_of_opts {
            packs.push(a);
            continue;
        }
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
                None => return Ok(usage_error(Some("switch `object-format' requires a value"))),
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
            s => packs.push(s),
        }
    }

    if packs.is_empty() {
        return Ok(usage_error(None));
    }

    let hash = match object_format.as_deref() {
        None => {
            // git falls back to the repository's algorithm; verify-pack also runs
            // outside a repository, where SHA-1 is the only thing it can assume.
            gix::discover(".")
                .map(|r| r.object_hash())
                .unwrap_or(gix::hash::Kind::Sha1)
        }
        Some("sha1") => gix::hash::Kind::Sha1,
        Some("sha256") => bail!(
            "--object-format=sha256 is unsupported: the vendored gix build does not \
             enable the sha256 feature, so no SHA-256 pack can be opened"
        ),
        Some(other) => {
            // git's `die()` path for an unrecognised algorithm name.
            eprintln!("fatal: unknown hash algorithm '{other}'");
            return Ok(ExitCode::from(128));
        }
    };

    let mut err = false;
    for path in packs {
        if !verify_one(path, verbose, stat_only, hash) {
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

/// Verify a single pack named by `path`, returning `false` when it is bad.
///
/// Mirrors `verify_one_pack()` + `verify_pack()`: the argument is normalised to
/// an `.idx` path, verification is skipped entirely under `--stat-only`, and the
/// report — object table, histogram, trailing `ok`/`bad` line — is only produced
/// when `-v` or `-s` was given.
fn verify_one(path: &str, verbose: bool, stat_only: bool, hash: gix::hash::Kind) -> bool {
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
    /// Fully resolved object size (`oi.sizep`).
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
        let info = data.decode_header(entry, &mut inflate, &resolve)?;
        let next = entries.get(i + 1).map_or(pack_end, |(o, _)| *o);
        rows.push(Row {
            oid,
            offset,
            kind: type_name(info.kind),
            size: info.object_size,
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
