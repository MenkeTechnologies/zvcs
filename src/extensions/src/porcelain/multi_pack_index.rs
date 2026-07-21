//! `git multi-pack-index` — write, verify, expire and compact a multi-pack-index (MIDX).
//!
//! Covered: the `write`, `verify` and `expire` sub-commands in their default
//! (v1, non-incremental, non-bitmap) form, `compact`'s argument handling and
//! chain lookup, the global `--object-dir=<dir>` / `--object-dir <dir>` /
//! `--no-object-dir` and `--progress` / `--no-progress` options, and the `-h`
//! usage blocks for the top level and for `write`, `verify`, `expire`,
//! `repack` and `compact` — each reproduced byte-for-byte along with git's exit
//! code 129. `write`, `verify` and `expire` print nothing on success, so the
//! interesting comparison is the on-disk artifact: the MIDX produced here is
//! byte-identical to stock git's for the default case, because
//! `gix_pack::multi_index::write_from_index_paths` emits the same header
//! (`MIDX`, version 1, hash id, chunk count, zero base files, pack count), the
//! same four chunks in the same order (`PNAM`, `OIDF`, `OIDL`, `OOFF`, plus
//! `LOFF` when a pack exceeds 2 GiB), the same 4-byte `PNAM` padding, the same
//! pack ordering (index basenames sorted), and the same duplicate resolution
//! (highest `.idx` mtime wins, ties broken by ascending pack index) that
//! `midx-write.c`'s `midx_oid_compare()` uses when no preferred pack is given.
//!
//! `expire` is a direct port of `midx.c`'s `expire_midx_packs()`: tally how many
//! MIDX entries name each pack, drop every pack with a zero tally unless it is
//! `.keep`-protected or a cruft pack (`.mtimes`), unlink those pack files, and
//! only then rewrite the MIDX from what is left — leaving the MIDX untouched
//! when nothing was dropped, which is what git does and what a differential
//! state comparison notices.
//!
//! `compact <from> <to>` collapses a range of MIDX chain layers. Its argument
//! handling and endpoint lookup are ported exactly (wrong argument count →
//! usage block and 129; an endpoint that names no layer → `fatal: could not
//! find MIDX: <arg>` and 128, `from` checked before `to`; identical endpoints →
//! `fatal: MIDX compaction endpoints must be unique` and 128). The lookup set is
//! the `multi-pack-index.d/multi-pack-index-chain` layer list when a chain
//! exists, and otherwise the flat MIDX's own trailing checksum, matching git's
//! treatment of a flat MIDX as a one-layer chain. Only the compaction itself is
//! unported — see below.
//!
//! Every sub-command honours `--` as its operand terminator exactly as git's
//! parse-options does: the first `--` ends option parsing and every following
//! token — including another `--` or a `--flag`-shaped word — is a literal
//! operand (an endpoint for `compact`, a rejected extra for the others). A
//! top-level `--` before any sub-command leaves git's `OPT_SUBCOMMAND` parser
//! with nothing to dispatch, so it reports `need a subcommand`.
//!
//! `repack`'s argument handling is ported even though its batched-repack
//! execution is not (see below): `-h`, the two common options, the
//! `--batch-size=<n>` `OPT_MAGNITUDE` grammar with git's three distinct value
//! diagnostics, the `--` terminator and the leftover-operand usage block all
//! match git byte-for-byte, because git rejects a malformed `repack` invocation
//! during option parsing before it writes a single pack. A well-formed `repack`
//! reproduces git's no-op cases exactly: `midx_repack()` exits 0 with no output
//! and no state change when the object store has no MIDX, and when the MIDX names
//! fewer than two packs — a batch needs two packs to collapse one into another.
//! Only a MIDX naming two or more packs reaches the missing writer and bails.
//!
//! `write --stdin-packs` reads a set of `.idx` basenames from stdin (git's
//! `read_packs_from_stdin` + `write_midx_file_only`) and indexes only the packs
//! it names whose `.pack` sibling exists, ignoring any existing MIDX. The `.idx`
//! list is fed straight to `write_from_index_paths`, so the artifact is
//! byte-identical to a full `write` restricted to that pack set (and an empty
//! resulting set reproduces `error: no pack files to index.` / exit 255).
//!
//! `write --refs-snapshot=<path>` (and its separate-argument form) is accepted
//! and discarded: git only consults the snapshot when generating a multi-pack
//! bitmap, so without `--bitmap` it never influences a single output byte — git
//! does not even open the file.
//!
//! `write --preferred-pack=<name>` is honoured wherever it is observable without
//! a bitmap, which is duplicate-object resolution. When the named pack is not
//! among those being indexed, git warns `unknown preferred pack: '<name>'` and
//! falls back to its default resolution (newest `.idx` mtime, then lowest pack
//! index) — exactly what `write_from_index_paths` already does — so that path is
//! reproduced warning-for-warning and byte-for-byte. A *known* preferred pack
//! only changes the winner when an object id is shared across the indexed packs;
//! that single case bails (see below).
//!
//! git's two `write` cross-flag validations run before any artifact is produced
//! and are reproduced as usage errors (exit 129): `--no-write-chain-file`
//! without `--incremental` (`cannot use --no-write-chain-file without
//! --incremental`, checked first) and `--base` without `--no-write-chain-file`
//! (`cannot use --base without --no-write-chain-file`). `--write-chain-file`
//! without `--incremental` is a no-op that still writes a flat MIDX, and is
//! accepted as such.
//!
//! Not covered — these `bail!` rather than producing a diverging artifact:
//!
//!   * `write --bitmap` — the vendored `gix-pack` has no multi-pack bitmap
//!     writer at all (`src/ported/gix-pack/src/multi_index/` has `write.rs` and
//!     `verify.rs` but no bitmap module), so the emitted `.bitmap`/`.rev` could
//!     not match git's.
//!   * `write --preferred-pack=<name>` when the named pack is present *and* the
//!     indexed packs share an object id — the only case the value changes the
//!     MIDX bytes. `write_from_index_paths` takes only a path list and resolves
//!     duplicates by mtime/index, with no hook for the preferred-pack tie-break.
//!   * `write --incremental` (with or without `--base=` /
//!     `--no-write-chain-file`), and the actual collapsing step of `compact`
//!     once both endpoints resolve — these read and write MIDX chain layers
//!     under `<objdir>/pack/multi-pack-index.d/`. `gix_pack::multi_index::Version`
//!     has only `V1`, the writer always emits zero base files, and the reader
//!     discards the base-file count outright (`multi_index/init.rs:100`:
//!     `let (_num_base_files, data) = data.split_at(1); // TODO: handle base
//!     files once it's clear what this does`), so a layer cannot even be read
//!     back correctly, let alone merged.
//!   * `repack`'s execution when a MIDX names two or more packs — this creates
//!     new pack files from batched old ones and then rewrites the MIDX;
//!     `gix-pack` has no pack-repacking driver. Its argument parsing and every
//!     no-op state (no MIDX, or a MIDX with fewer than two packs) are fully
//!     reproduced (above); only this final batching step bails.
//!
//! `verify` uses `verify_integrity_fast`, which is the exact scope of git's
//! `verify_midx_file`: trailing checksum, fan-out monotonicity, OID ordering,
//! and every recorded pack offset checked against the referenced `.idx` —
//! without traversing pack data. Failure exits 1 like git; the diagnostic text
//! on stderr is gitoxide's, not git's `incorrect checksum` / `incorrect object
//! offset for oid[N] = ...` wording.

use anyhow::{bail, Result};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::odb::pack::multi_index;

/// Top-level usage block, byte-for-byte (730 bytes including the trailing blank
/// line). Printed on `-h` (stdout) and for a usage error (stderr).
const USAGE: &str = "\
usage: git multi-pack-index [<options>] write [--preferred-pack=<pack>]
         [--[no-]bitmap] [--[no-]incremental] [--[no-]stdin-packs]
         [--refs-snapshot=<path>] [--[no-]write-chain-file]
         [--base=<checksum>]
   or: git multi-pack-index [<options>] compact [--[no-]incremental]
         [--[no-]bitmap] [--base=<checksum>] [--[no-]write-chain-file]
         <from> <to>
   or: git multi-pack-index [<options>] verify
   or: git multi-pack-index [<options>] expire
   or: git multi-pack-index [<options>] repack [--batch-size=<size>]

    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting

";

/// `git multi-pack-index write -h` (988 bytes).
const WRITE_USAGE: &str = "\
usage: git multi-pack-index [<options>] write [--preferred-pack=<pack>]
         [--[no-]bitmap] [--[no-]incremental] [--[no-]stdin-packs]
         [--refs-snapshot=<path>] [--[no-]write-chain-file]
         [--base=<checksum>]

    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting
    --[no-]preferred-pack <preferred-pack>
                          pack for reuse when computing a multi-pack bitmap
    --[no-]bitmap         write multi-pack bitmap
    --[no-]base <checksum>
                          base MIDX for incremental writes
    --[no-]incremental    write a new incremental MIDX
    --[no-]write-chain-file
                          write the multi-pack-index chain file
    --[no-]stdin-packs    write multi-pack index containing only given indexes
    --[no-]refs-snapshot <file>
                          refs snapshot for selecting bitmap commits

";

/// `git multi-pack-index verify -h` (225 bytes).
const VERIFY_USAGE: &str = "\
usage: git multi-pack-index [<options>] verify

    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting

";

/// `git multi-pack-index expire -h`.
const EXPIRE_USAGE: &str = "\
usage: git multi-pack-index [<options>] expire

    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting

";

/// `git multi-pack-index compact -h` (622 bytes). Note the option order: git
/// lists `base` before `bitmap` here, the reverse of the `write` block.
const COMPACT_USAGE: &str = "\
usage: git multi-pack-index [<options>] compact [--[no-]incremental]
         [--[no-]bitmap] [--base=<checksum>] [--[no-]write-chain-file]
         <from> <to>

    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting
    --[no-]base <checksum>
                          base MIDX for incremental writes
    --[no-]bitmap         write multi-pack bitmap
    --[no-]incremental    write a new incremental MIDX
    --[no-]write-chain-file
                          write the multi-pack-index chain file

";

/// `git multi-pack-index repack -h`.
const REPACK_USAGE: &str = "\
usage: git multi-pack-index [<options>] repack [--batch-size=<size>]

    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting
    --batch-size <n>      during repack, collect pack-files of smaller size into a batch that is larger than this size

";

/// `git multi-pack-index` — write, verify, expire and compact the multi-pack-index.
///
/// Supported forms (stdout, exit code and resulting MIDX file matching stock git):
///   * `git multi-pack-index write`      → `<objdir>/pack/multi-pack-index`
///   * `git multi-pack-index verify`     → silent, exit 0 (exit 1 on damage)
///   * `git multi-pack-index expire`     → silent, exit 0; drops packs the MIDX
///     no longer resolves any object to and rewrites the MIDX if it dropped any
///   * `git multi-pack-index compact <from> <to>` → endpoint resolution and its
///     two `fatal:` exits; the collapsing step itself is unported
///   * `--object-dir=<dir>` / `--object-dir <dir>` / `--no-object-dir` on any
///   * `--progress` / `--no-progress` (accepted; progress is discarded, git's
///     own progress goes to stderr and never to stdout)
///   * `-h` at the top level and on every sub-command
///
/// `write` also honours `--stdin-packs`, `--preferred-pack=` and
/// `--refs-snapshot=` (see the module docs), and reproduces git's two cross-flag
/// usage errors. `write --bitmap` / `--incremental`, a `--preferred-pack` that
/// would break a cross-pack duplicate tie, and a `repack` whose MIDX names two
/// or more packs `bail!` — see the module docs for the specific missing
/// substrate.
pub fn multi_pack_index(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0.
    let args = match args.first().map(String::as_str) {
        Some("multi-pack-index") => &args[1..],
        _ => args,
    };

    let mut object_dir: Option<PathBuf> = None;
    let mut subcommand: Option<&str> = None;
    let mut rest: Vec<&str> = Vec::new();

    // git's parse-options runs with PARSE_OPT_STOP_AT_NON_OPTION, so the first
    // bare word is the sub-command and everything after it belongs to that
    // sub-command's own parser (which re-accepts the two common options).
    let mut it = args.iter().map(String::as_str);
    while let Some(a) = it.next() {
        if subcommand.is_some() {
            rest.push(a);
            continue;
        }
        match take_common(a, &mut it, &mut object_dir)? {
            Common::Consumed => continue,
            Common::MissingValue(name) => {
                return Ok(usage_error(Some(&format!("option `{name}' requires a value")), USAGE))
            }
            Common::NotCommon => {}
        }
        if a == "-h" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // A bare `--` terminates option parsing. Any subcommand would have been
        // matched above (`subcommand.is_some()` takes the earlier branch), so at
        // the top level `--` leaves git's `OPT_SUBCOMMAND` parser with no
        // subcommand and it falls through to `need a subcommand`.
        if a == "--" {
            break;
        }
        // git's `unknown option` names everything after the `--`, including any
        // `=<value>`: `--base=deadbeef` reports ``unknown option `base=deadbeef'``.
        if let Some(name) = a.strip_prefix("--") {
            return Ok(usage_error(Some(&format!("unknown option `{name}'")), USAGE));
        }
        if a.len() > 1 && a.starts_with('-') {
            let c = a.chars().nth(1).expect("checked length");
            return Ok(usage_error(Some(&format!("unknown switch `{c}'")), USAGE));
        }
        subcommand = Some(a);
    }

    match subcommand {
        None => Ok(usage_error(Some("need a subcommand"), USAGE)),
        Some("write") => write(&rest, object_dir),
        Some("verify") => verify(&rest, object_dir),
        Some("expire") => expire(&rest, object_dir),
        Some("compact") => compact(&rest, object_dir),
        Some("repack") => repack(&rest, object_dir),
        Some(other) => Ok(usage_error(
            Some(&format!("unknown subcommand: `{other}'")),
            USAGE,
        )),
    }
}

/// `write`: index every pack in the object directory into a fresh MIDX.
fn write(rest: &[&str], mut object_dir: Option<PathBuf>) -> Result<ExitCode> {
    let mut after_dd = false;
    // git's write option array is all last-wins booleans plus three string
    // options; only the flags that reach the flat-write path or a validation
    // `error()` need to be remembered.
    let mut bitmap = false;
    let mut incremental = false;
    let mut no_chain = false;
    let mut base = false;
    let mut stdin_packs = false;
    let mut preferred: Option<String> = None;
    let mut it = rest.iter().copied();
    while let Some(a) = it.next() {
        if after_dd {
            // Everything past `--` is an operand and `write` takes none, so git
            // prints the bare usage block with no `error:` line.
            return Ok(usage_error(None, WRITE_USAGE));
        }
        if a == "--" {
            after_dd = true;
            continue;
        }
        match take_common(a, &mut it, &mut object_dir)? {
            Common::Consumed => continue,
            Common::MissingValue(name) => {
                return Ok(usage_error(
                    Some(&format!("option `{name}' requires a value")),
                    WRITE_USAGE,
                ))
            }
            Common::NotCommon => {}
        }
        match a {
            "-h" => {
                print!("{WRITE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--bitmap" => bitmap = true,
            "--no-bitmap" => bitmap = false,
            "--incremental" => incremental = true,
            "--no-incremental" => incremental = false,
            "--write-chain-file" => no_chain = false,
            "--no-write-chain-file" => no_chain = true,
            "--stdin-packs" => stdin_packs = true,
            "--no-stdin-packs" => stdin_packs = false,
            "--no-refs-snapshot" => {}
            "--no-base" => base = false,
            "--no-preferred-pack" => preferred = None,
            // `--base`/`--refs-snapshot`/`--preferred-pack` are git `OPT_STRING`s:
            // a `=<value>` inline form and a separate-argument form, the latter
            // erroring `option `<name>' requires a value` (exit 129) when nothing
            // follows.
            "--base" => match it.next() {
                Some(_) => base = true,
                None => {
                    return Ok(usage_error(
                        Some("option `base' requires a value"),
                        WRITE_USAGE,
                    ))
                }
            },
            _ if a.starts_with("--base=") => base = true,
            "--refs-snapshot" => {
                if it.next().is_none() {
                    return Ok(usage_error(
                        Some("option `refs-snapshot' requires a value"),
                        WRITE_USAGE,
                    ));
                }
            }
            _ if a.starts_with("--refs-snapshot=") => {}
            "--preferred-pack" => match it.next() {
                Some(v) => preferred = Some(v.to_string()),
                None => {
                    return Ok(usage_error(
                        Some("option `preferred-pack' requires a value"),
                        WRITE_USAGE,
                    ))
                }
            },
            _ if a.starts_with("--preferred-pack=") => {
                preferred = Some(a["--preferred-pack=".len()..].to_string())
            }
            _ if a.starts_with("--") => {
                return Ok(usage_error(
                    Some(&format!("unknown option `{}'", &a[2..])),
                    WRITE_USAGE,
                ));
            }
            _ => return Ok(usage_error(None, WRITE_USAGE)),
        }
    }

    // `cmd_multi_pack_index_write` runs two `usage_with_options()` validations
    // (exit 129) before it writes anything, in this order.
    if no_chain && !incremental {
        return Ok(usage_error(
            Some("cannot use --no-write-chain-file without --incremental"),
            WRITE_USAGE,
        ));
    }
    if base && !no_chain {
        return Ok(usage_error(
            Some("cannot use --base without --no-write-chain-file"),
            WRITE_USAGE,
        ));
    }

    // `--bitmap` needs a multi-pack bitmap writer and `--incremental` needs a
    // MIDX chain writer; gix-pack has neither, so these still bail honestly. A
    // valid `--base` only ever reaches here alongside `--incremental` (it errors
    // out above otherwise), so the incremental bail covers it.
    if bitmap {
        bail!(
            "multi-pack-index write --bitmap is not portable here — gix-pack has no multi-pack bitmap writer, so the emitted .bitmap/.rev would not match git's"
        );
    }
    if incremental {
        bail!(
            "multi-pack-index write --incremental is not portable here — gix-pack writes a single flat v1 MIDX with zero base files (multi_index/init.rs discards the base-file count), so a chain layer under multi-pack-index.d/ cannot be written"
        );
    }

    let (repo, pack_dir) = object_store(object_dir)?;
    reject_chain(&pack_dir)?;

    // The pack set: every pack in the object store, or — with `--stdin-packs` —
    // only those whose `.idx` basename git read from stdin (existing MIDX
    // ignored, matching `write_midx_file_only`).
    let mut index_paths = pack_indices(&pack_dir);
    if stdin_packs {
        let wanted = read_stdin_pack_names();
        index_paths.retain(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| wanted.contains(n))
        });
    }

    // `--preferred-pack` only steers duplicate-object resolution and bitmap
    // reuse. git warns and falls back to its default resolution when the named
    // pack is not among those being indexed; that default is exactly the
    // `write_from_index_paths` tie-break (newest `.idx` mtime, then lowest pack
    // index), so an unknown preferred pack stays byte-identical. A *known*
    // preferred pack changes the winner only when the same object id appears in
    // more than one of the indexed packs — the one case gix-pack's writer cannot
    // reproduce, so it bails there and nowhere else.
    if let Some(name) = &preferred {
        if preferred_pack_present(name, &index_paths) {
            if has_cross_pack_duplicates(&index_paths, repo.object_hash())? {
                bail!(
                    "multi-pack-index write --preferred-pack={name} cannot be reproduced here — the indexed packs share at least one object id and gix_pack::multi_index::write_from_index_paths does not expose the preferred-pack tie-break git uses to resolve the duplicate"
                );
            }
        } else {
            eprintln!("warning: unknown preferred pack: '{name}'");
        }
    }

    if !write_midx_from(&pack_dir, repo.object_hash(), index_paths)? {
        // `midx-write.c`: `error(_("no pack files to index."))`, and cmd_* hands
        // the -1 straight back to git, which exits 255.
        eprintln!("error: no pack files to index.");
        return Ok(ExitCode::from(255));
    }
    Ok(ExitCode::SUCCESS)
}

/// Read the `--stdin-packs` name set from stdin: one `.idx` basename per line,
/// with git's `strbuf_getline` newline handling (a trailing `\r` is stripped and
/// a final line without a newline still counts). Names are matched verbatim
/// against pack-directory basenames — no trimming, no path components.
fn read_stdin_pack_names() -> std::collections::HashSet<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok();
    let mut names = std::collections::HashSet::new();
    let mut rest = buf.as_str();
    while !rest.is_empty() {
        let (line, tail) = match rest.split_once('\n') {
            Some((l, t)) => (l, t),
            None => (rest, ""),
        };
        names.insert(line.strip_suffix('\r').unwrap_or(line).to_string());
        rest = tail;
    }
    names
}

/// Whether `name` (a `--preferred-pack` value) identifies one of the packs about
/// to be indexed. git's `cmp_idx_or_pack_name` matches the stored `<base>.idx`
/// name against the value's `.idx` *or* `.pack` form; a value with no such
/// extension never matches.
fn preferred_pack_present(name: &str, index_paths: &[PathBuf]) -> bool {
    let base = match name
        .strip_suffix(".idx")
        .or_else(|| name.strip_suffix(".pack"))
    {
        Some(b) => b,
        None => return false,
    };
    let idx = format!("{base}.idx");
    index_paths
        .iter()
        .any(|p| p.file_name().and_then(|n| n.to_str()) == Some(idx.as_str()))
}

/// Whether any object id appears in more than one of `index_paths`. Every pack
/// index lists unique ids, so a repeat across the set is a cross-pack duplicate —
/// the only situation in which `--preferred-pack` changes the MIDX bytes.
fn has_cross_pack_duplicates(index_paths: &[PathBuf], object_hash: gix::hash::Kind) -> Result<bool> {
    let mut seen: std::collections::HashSet<gix::ObjectId> = std::collections::HashSet::new();
    for path in index_paths {
        let idx = gix::odb::pack::index::File::at(path, object_hash)?;
        for entry in idx.iter() {
            if !seen.insert(entry.oid) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Rewrite `<pack_dir>/multi-pack-index` from every pack currently present in
/// `pack_dir`, returning `false` when there is nothing to index.
///
/// git writes through `multi-pack-index.lock` and renames on success, so a
/// failed write never replaces a good index; this does the same.
fn write_midx(pack_dir: &Path, object_hash: gix::hash::Kind) -> Result<bool> {
    write_midx_from(pack_dir, object_hash, pack_indices(pack_dir))
}

/// Write `<pack_dir>/multi-pack-index` from an explicit index-path set (used by
/// `--stdin-packs`, which indexes only the packs named on stdin); otherwise
/// identical to [`write_midx`].
fn write_midx_from(
    pack_dir: &Path,
    object_hash: gix::hash::Kind,
    index_paths: Vec<PathBuf>,
) -> Result<bool> {
    if index_paths.is_empty() {
        return Ok(false);
    }

    let lock = pack_dir.join("multi-pack-index.lock");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|e| anyhow::anyhow!("unable to create {:?}: {e}", lock))?;
    let mut out = BufWriter::new(file);

    let result = multi_index::write_from_index_paths(
        index_paths,
        &mut out,
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
        multi_index::write::Options { object_hash },
    );

    let flushed = out.flush();
    drop(out);
    if let Err(e) = result {
        fs::remove_file(&lock).ok();
        return Err(e.into());
    }
    if let Err(e) = flushed {
        fs::remove_file(&lock).ok();
        return Err(e.into());
    }
    fs::rename(&lock, pack_dir.join("multi-pack-index"))?;

    // `clear_midx_files_ext()`: a MIDX written without a bitmap invalidates any
    // bitmap and reverse index left over from an earlier `write --bitmap`.
    for name in dir_names(pack_dir) {
        if name.starts_with("multi-pack-index-")
            && (name.ends_with(".bitmap") || name.ends_with(".rev"))
        {
            fs::remove_file(pack_dir.join(name)).ok();
        }
    }
    Ok(true)
}

/// `verify`: check the MIDX against the pack indices it references.
fn verify(rest: &[&str], mut object_dir: Option<PathBuf>) -> Result<ExitCode> {
    let mut after_dd = false;
    let mut it = rest.iter().copied();
    while let Some(a) = it.next() {
        if after_dd {
            // Operands after `--`; `verify` takes none, so git prints the bare
            // usage block (this is also what `verify extra` produces).
            return Ok(usage_error(None, VERIFY_USAGE));
        }
        if a == "--" {
            after_dd = true;
            continue;
        }
        match take_common(a, &mut it, &mut object_dir)? {
            Common::Consumed => continue,
            Common::MissingValue(name) => {
                return Ok(usage_error(
                    Some(&format!("option `{name}' requires a value")),
                    VERIFY_USAGE,
                ))
            }
            Common::NotCommon => {}
        }
        match a {
            "-h" => {
                print!("{VERIFY_USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ if a.starts_with("--") => {
                return Ok(usage_error(
                    Some(&format!("unknown option `{}'", &a[2..])),
                    VERIFY_USAGE,
                ));
            }
            // `git multi-pack-index verify extra` prints the usage block with no
            // `error:` line at all.
            _ => return Ok(usage_error(None, VERIFY_USAGE)),
        }
    }

    let (_repo, pack_dir) = object_store(object_dir)?;
    let midx = pack_dir.join("multi-pack-index");

    // No MIDX is not an error for git: `verify_midx_file()` returns 0 silently.
    if !midx.exists() {
        return Ok(ExitCode::SUCCESS);
    }

    let file = multi_index::File::at(&midx, None)?;
    match file.verify_integrity_fast(&mut gix::progress::Discard, &AtomicBool::new(false)) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            // git reports each problem via `error()` and exits 1.
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}

/// `expire`: drop every pack the MIDX no longer names an object in.
///
/// A port of `midx.c`'s `expire_midx_packs()`. Note the ordering that a state
/// comparison is sensitive to: the pack files are unlinked first, and the MIDX
/// is rewritten only if at least one pack was actually dropped — an expire that
/// finds nothing to do leaves the existing MIDX byte-for-byte untouched.
fn expire(rest: &[&str], mut object_dir: Option<PathBuf>) -> Result<ExitCode> {
    let mut after_dd = false;
    let mut it = rest.iter().copied();
    while let Some(a) = it.next() {
        if after_dd {
            // Operands after `--`; `expire` takes none.
            return Ok(usage_error(None, EXPIRE_USAGE));
        }
        if a == "--" {
            after_dd = true;
            continue;
        }
        match take_common(a, &mut it, &mut object_dir)? {
            Common::Consumed => continue,
            Common::MissingValue(name) => {
                return Ok(usage_error(
                    Some(&format!("option `{name}' requires a value")),
                    EXPIRE_USAGE,
                ))
            }
            Common::NotCommon => {}
        }
        match a {
            "-h" => {
                print!("{EXPIRE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ if a.starts_with("--") => {
                return Ok(usage_error(
                    Some(&format!("unknown option `{}'", &a[2..])),
                    EXPIRE_USAGE,
                ));
            }
            _ => return Ok(usage_error(None, EXPIRE_USAGE)),
        }
    }

    let (repo, pack_dir) = object_store(object_dir)?;
    reject_chain(&pack_dir)?;

    // `expire_midx_packs()` opens the MIDX with `load_multi_pack_index()` and
    // returns 0 without a word when there is none.
    let midx = pack_dir.join("multi-pack-index");
    if !midx.exists() {
        return Ok(ExitCode::SUCCESS);
    }
    let file = multi_index::File::at(&midx, None)?;

    // `count[pack_int_id]++` over every entry: a pack no entry resolves to is a
    // pack whose objects are all reachable through some other pack in the MIDX.
    // A pack id past the end of the name list means the MIDX is damaged; that is
    // `verify`'s business, and expire must not delete packs on the strength of a
    // tally it could not complete.
    let mut counts = vec![0u32; file.num_indices() as usize];
    for entry in 0..file.num_objects() {
        let (pack, _offset) = file.pack_id_and_pack_offset_at_index(entry);
        match counts.get_mut(pack as usize) {
            Some(count) => *count += 1,
            None => bail!(
                "multi-pack-index entry {entry} names pack {pack}, but only {} packs are recorded",
                counts.len()
            ),
        }
    }

    let mut dropped = false;
    for (i, index_name) in file.index_names().iter().enumerate() {
        if counts[i] != 0 {
            continue;
        }
        let Some(base) = index_name.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // `unlink_pack_path()` refuses a `.keep` pack, and `expire_midx_packs()`
        // skips cruft packs (`.mtimes`) outright — both hold objects that are
        // not represented anywhere else even though the MIDX prefers a copy.
        if pack_dir.join(format!("{base}.keep")).exists()
            || pack_dir.join(format!("{base}.mtimes")).exists()
        {
            continue;
        }
        for ext in ["pack", "idx", "rev", "bitmap", "promisor"] {
            fs::remove_file(pack_dir.join(format!("{base}.{ext}"))).ok();
        }
        dropped = true;
    }

    if dropped {
        drop(file);
        // `write_midx_internal(object_dir, NULL, &packs_to_drop, ...)` rescans the
        // pack directory, which by now no longer holds the dropped packs.
        if !write_midx(&pack_dir, repo.object_hash())? {
            // Every pack went away, so there is nothing left to index and git's
            // `clear_midx_files()` takes the MIDX with it.
            fs::remove_file(&midx).ok();
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `compact <from> <to>`: collapse the MIDX chain layers between two endpoints.
///
/// Argument handling and endpoint resolution are ported; the collapsing step
/// itself is not — see the module docs for the missing `gix-pack` substrate.
fn compact(rest: &[&str], mut object_dir: Option<PathBuf>) -> Result<ExitCode> {
    let mut positional: Vec<&str> = Vec::new();
    let mut after_dd = false;
    let mut it = rest.iter().copied();
    while let Some(a) = it.next() {
        if after_dd {
            // Past `--` every token is a literal endpoint, even another `--` or a
            // `--flag`-looking word (`compact -- --base=z a` looks up `--base=z`).
            positional.push(a);
            continue;
        }
        if a == "--" {
            after_dd = true;
            continue;
        }
        match take_common(a, &mut it, &mut object_dir)? {
            Common::Consumed => continue,
            Common::MissingValue(name) => {
                return Ok(usage_error(
                    Some(&format!("option `{name}' requires a value")),
                    COMPACT_USAGE,
                ))
            }
            Common::NotCommon => {}
        }
        match a {
            "-h" => {
                print!("{COMPACT_USAGE}");
                return Ok(ExitCode::from(129));
            }
            // Accepted by git's compact option array; each only steers the write
            // of the compacted layer, which is unported and bails below anyway.
            "--bitmap"
            | "--no-bitmap"
            | "--incremental"
            | "--no-incremental"
            | "--write-chain-file"
            | "--no-write-chain-file"
            | "--no-base" => {}
            "--base" => match it.next() {
                Some(_) => {}
                None => {
                    return Ok(usage_error(
                        Some("option `base' requires a value"),
                        COMPACT_USAGE,
                    ))
                }
            },
            _ if a.starts_with("--base=") => {}
            _ if a.starts_with("--") => {
                return Ok(usage_error(
                    Some(&format!("unknown option `{}'", &a[2..])),
                    COMPACT_USAGE,
                ));
            }
            _ => positional.push(a),
        }
    }

    // `<from> <to>` are both required and there is no third form; anything else
    // is a bare usage block on stderr with no `error:` line.
    if positional.len() != 2 {
        return Ok(usage_error(None, COMPACT_USAGE));
    }
    let (from, to) = (positional[0], positional[1]);

    let (_repo, pack_dir) = object_store(object_dir)?;
    let layers = midx_chain_layers(&pack_dir);

    // git resolves `from` first, then `to`, and only then compares them: with
    // both endpoints bogus and equal it still reports the lookup failure.
    for endpoint in [from, to] {
        if !layers.iter().any(|l| l == endpoint) {
            eprintln!("fatal: could not find MIDX: {endpoint}");
            return Ok(ExitCode::from(128));
        }
    }
    if from == to {
        eprintln!("fatal: MIDX compaction endpoints must be unique");
        return Ok(ExitCode::from(128));
    }

    bail!("compacting MIDX chain layers {from}..{to} is unported — gix-pack reads and writes v1 MIDX with zero base files only (multi_index/init.rs discards the base-file count), so a chain layer cannot be read back, let alone merged")
}

/// `repack`: batch small packs into new ones and rewrite the MIDX.
///
/// The batched-repack execution itself is unported — `gix-pack` has no
/// pack-repacking driver — but git rejects a malformed invocation during option
/// parsing, long before any pack is written, so every argument-error path is
/// reproduced here byte-for-byte: `-h`, the `--object-dir` / `--progress`
/// commons, the `--batch-size=<n>` `OPT_MAGNITUDE` value grammar (base-0 numeric
/// parse plus optional k/m/g suffix, with git's three distinct diagnostics for
/// an empty, malformed or out-of-range value), the `--` operand terminator and
/// git's leftover-operand usage block. A well-formed invocation reproduces git's
/// no-op cases (no MIDX, or a MIDX naming fewer than two packs) as a silent exit
/// 0; only a MIDX naming two or more packs reaches the missing writer and
/// `bail!`s.
fn repack(rest: &[&str], mut object_dir: Option<PathBuf>) -> Result<ExitCode> {
    let mut after_dd = false;
    // `repack` collects non-option words without stopping, then rejects them all
    // once parsing succeeds; a bad option encountered first still wins, so this
    // only fires after the loop.
    let mut has_operand = false;
    let mut it = rest.iter().copied();
    while let Some(a) = it.next() {
        if after_dd {
            has_operand = true;
            continue;
        }
        if a == "--" {
            after_dd = true;
            continue;
        }
        match take_common(a, &mut it, &mut object_dir)? {
            Common::Consumed => continue,
            Common::MissingValue(name) => {
                return Ok(usage_error(
                    Some(&format!("option `{name}' requires a value")),
                    REPACK_USAGE,
                ))
            }
            Common::NotCommon => {}
        }
        match a {
            "-h" => {
                print!("{REPACK_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--batch-size" => match it.next() {
                Some(v) => {
                    if let Some(msg) = batch_size_error(v) {
                        return Ok(usage_error(Some(&msg), REPACK_USAGE));
                    }
                }
                None => {
                    return Ok(usage_error(
                        Some("option `batch-size' requires a value"),
                        REPACK_USAGE,
                    ))
                }
            },
            _ if a.starts_with("--batch-size=") => {
                let v = &a["--batch-size=".len()..];
                if let Some(msg) = batch_size_error(v) {
                    return Ok(usage_error(Some(&msg), REPACK_USAGE));
                }
            }
            _ if a.starts_with("--") => {
                return Ok(usage_error(
                    Some(&format!("unknown option `{}'", &a[2..])),
                    REPACK_USAGE,
                ));
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                let c = a.chars().nth(1).expect("checked length");
                return Ok(usage_error(Some(&format!("unknown switch `{c}'")), REPACK_USAGE));
            }
            // A bare word is an operand; `repack` takes none.
            _ => has_operand = true,
        }
    }

    if has_operand {
        return Ok(usage_error(None, REPACK_USAGE));
    }

    // `midx_repack()` (midx.c) is a silent no-op — exit 0, no output, no state
    // change — whenever there is nothing for it to batch. It loads the MIDX with
    // `load_multi_pack_index()` and returns 0 straight away when the object store
    // has none, which is the state of every repository that has never run
    // `multi-pack-index write`. It likewise does nothing when the MIDX names
    // fewer than two packs, because a batch needs at least two packs to collapse
    // one into another. git prints nothing and exits 0 in all of these cases,
    // regardless of `--batch-size`, so this reproduces them exactly.
    let (_repo, pack_dir) = object_store(object_dir)?;
    let midx = pack_dir.join("multi-pack-index");
    if !midx.exists() {
        return Ok(ExitCode::SUCCESS);
    }
    let file = multi_index::File::at(&midx, None)?;
    if file.num_indices() < 2 {
        return Ok(ExitCode::SUCCESS);
    }

    // A MIDX naming two or more packs is the only state in which `midx_repack()`
    // actually spawns `pack-objects` to rewrite a batch and then rewrites the
    // MIDX. That step needs a pack-repacking driver gix-pack does not provide —
    // its only output mode is `Mode::PackCopyAndBaseObjects` with no delta
    // compression — so collapsing packs is not yet ported.
    bail!(
        "multi-pack-index repack of a MIDX naming {} packs is not yet ported — batched repacking needs a pack writer that gix-pack does not provide",
        file.num_indices()
    )
}

/// Classify a `--batch-size` value the way git's `OPT_MAGNITUDE` does and return
/// the exact `error:` text git prints, or `None` when the value is accepted.
///
/// git parses it through `git_parse_ulong`: reject any `-` outright, run
/// `strtoumax(value, &end, 0)` (base 0 — `0x` hex, leading `0` octal, else
/// decimal, skipping leading whitespace and a single `+`), then require the
/// remainder to be an empty or a `k`/`m`/`g` suffix (case-insensitive). An empty
/// value, a malformed one and an out-of-`unsigned long`-range one each get a
/// distinct message; the range bound prints literally as `[0,-1]` in git 2.55.
fn batch_size_error(v: &str) -> Option<String> {
    match classify_magnitude(v) {
        MagValue::Ok => None,
        MagValue::Empty => Some("option `batch-size' expects a numerical value".to_string()),
        MagValue::Invalid => Some(
            "option `batch-size' expects a non-negative integer value with an optional k/m/g suffix"
                .to_string(),
        ),
        MagValue::Range => Some(format!("value {v} for option `batch-size' not in range [0,-1]")),
    }
}

/// The three outcomes of a magnitude parse that map to git's three diagnostics.
enum MagValue {
    Ok,
    /// The value was the empty string.
    Empty,
    /// Non-numeric, negative, or a bad unit suffix.
    Invalid,
    /// Numerically valid but larger than an `unsigned long` can hold.
    Range,
}

/// A faithful port of `git_parse_ulong` (base-0 numeric parse + k/m/g suffix),
/// classified into [`MagValue`]. The upper bound is `u64::MAX`, matching
/// `unsigned long` on git's 64-bit targets.
fn classify_magnitude(arg: &str) -> MagValue {
    if arg.is_empty() {
        return MagValue::Empty;
    }
    // `git_parse_unsigned` rejects a `-` anywhere before touching `strtoumax`,
    // which would otherwise accept negatives by wrapping.
    if arg.contains('-') {
        return MagValue::Invalid;
    }
    let b = arg.as_bytes();
    let mut i = 0;
    // strtoumax skips leading C-locale whitespace, then a single optional `+`.
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    if i < b.len() && b[i] == b'+' {
        i += 1;
    }
    // base 0: `0x`/`0X` -> hex, leading `0` -> octal, else decimal.
    let (radix, start): (u128, usize) =
        if i + 1 < b.len() && b[i] == b'0' && (b[i + 1] == b'x' || b[i + 1] == b'X') {
            (16, i + 2)
        } else if i < b.len() && b[i] == b'0' {
            (8, i)
        } else {
            (10, i)
        };
    let mut val: u128 = 0;
    let mut any = false;
    let mut too_big = false;
    let mut j = start;
    while j < b.len() {
        let digit = match b[j] {
            c @ b'0'..=b'9' => (c - b'0') as u128,
            c @ b'a'..=b'f' => (c - b'a' + 10) as u128,
            c @ b'A'..=b'F' => (c - b'A' + 10) as u128,
            _ => break,
        };
        if digit >= radix {
            break;
        }
        any = true;
        match val.checked_mul(radix).and_then(|v| v.checked_add(digit)) {
            Some(v) => val = v,
            None => too_big = true,
        }
        j += 1;
    }
    // git checks `strtoumax`'s ERANGE before it looks at the suffix.
    if too_big {
        return MagValue::Range;
    }
    if !any {
        return MagValue::Invalid;
    }
    let factor: u128 = match &arg[j..] {
        "" => 1,
        "k" | "K" => 1024,
        "m" | "M" => 1024 * 1024,
        "g" | "G" => 1024 * 1024 * 1024,
        _ => return MagValue::Invalid,
    };
    match val.checked_mul(factor) {
        Some(uval) if uval <= u64::MAX as u128 => MagValue::Ok,
        _ => MagValue::Range,
    }
}

/// Checksums of the MIDX layers `compact` can name, newest last.
///
/// A chain lists its layers in `multi-pack-index.d/multi-pack-index-chain`; a
/// flat MIDX is treated by git as a one-layer chain identified by its own
/// trailing checksum.
fn midx_chain_layers(pack_dir: &Path) -> Vec<String> {
    let chain = pack_dir
        .join("multi-pack-index.d")
        .join("multi-pack-index-chain");
    if let Ok(text) = fs::read_to_string(&chain) {
        return text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
    }
    let flat = pack_dir.join("multi-pack-index");
    match multi_index::File::at(&flat, None) {
        Ok(file) => vec![file.checksum().to_string()],
        Err(_) => Vec::new(),
    }
}

/// The repository and its `<objdir>/pack` directory, honouring `--object-dir`.
fn object_store(object_dir: Option<PathBuf>) -> Result<(gix::Repository, PathBuf)> {
    let repo = gix::discover(".")?;
    let objdir = object_dir.unwrap_or_else(|| repo.objects.store_ref().path().to_path_buf());
    let pack_dir = objdir.join("pack");
    Ok((repo, pack_dir))
}

/// Refuse to operate on a repository whose MIDX is a chain.
///
/// Tearing one down and replacing it with a flat MIDX is not modelled, and
/// leaving a fresh flat MIDX beside a live chain would put the repository in a
/// state git never produces.
fn reject_chain(pack_dir: &Path) -> Result<()> {
    let chain = pack_dir.join("multi-pack-index.d");
    if chain.exists() {
        bail!("an incremental multi-pack-index chain is present at {chain:?}; rewriting it is unported");
    }
    Ok(())
}

/// Outcome of trying to interpret `a` as one of the two options every
/// sub-command shares with the top level.
enum Common {
    /// Recognised and fully handled, including any separate value argument.
    Consumed,
    /// Recognised but the following value argument was missing; carries the
    /// option name for the error message.
    MissingValue(&'static str),
    /// Not one of the common options; the caller must handle it.
    NotCommon,
}

/// `--object-dir[=<dir>]`, `--no-object-dir`, `--progress` and `--no-progress`,
/// accepted identically at the top level and after any sub-command exactly as
/// git's per-sub-command option arrays repeat them.
fn take_common<'a>(
    a: &str,
    it: &mut impl Iterator<Item = &'a str>,
    object_dir: &mut Option<PathBuf>,
) -> Result<Common> {
    match a {
        // Progress is written to stderr by git and never influences stdout, so
        // discarding it keeps every compared byte identical.
        "--progress" | "--no-progress" => Ok(Common::Consumed),
        "--no-object-dir" => {
            *object_dir = None;
            Ok(Common::Consumed)
        }
        "--object-dir" => match it.next() {
            Some(v) => {
                *object_dir = Some(PathBuf::from(v));
                Ok(Common::Consumed)
            }
            None => Ok(Common::MissingValue("object-dir")),
        },
        _ => match a.strip_prefix("--object-dir=") {
            Some(v) => {
                *object_dir = Some(PathBuf::from(v));
                Ok(Common::Consumed)
            }
            None => Ok(Common::NotCommon),
        },
    }
}

/// Every `*.idx` in `dir` whose `.pack` sibling exists, sorted — the set
/// `for_each_file_in_pack_dir()` feeds to `add_pack_to_midx()`, minus the
/// entries `add_packed_git()` would reject for a missing pack. The writer sorts
/// again internally, so the final `PNAM` order is git's name order either way.
fn pack_indices(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for name in dir_names(dir) {
        let Some(base) = name.strip_suffix(".idx") else {
            continue;
        };
        if dir.join(format!("{base}.pack")).is_file() {
            out.push(dir.join(&name));
        }
    }
    out.sort();
    out
}

/// Sorted UTF-8 entry names of `dir`; a missing or unreadable directory yields
/// nothing, which is how git treats an object store with no `pack` directory.
fn dir_names(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// git's parse-options failure shape: an optional `error: <msg>` line followed
/// by the relevant usage block, both on stderr, exit 129.
fn usage_error(msg: Option<&str>, usage: &str) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{usage}"),
        None => eprint!("{usage}"),
    }
    ExitCode::from(129)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two shared option lines and the trailing blank line are what git's
    /// `usage_with_options()` appends to every block; an edit that drops either
    /// would go unnoticed until a differential run.
    #[test]
    fn usage_blocks_share_the_common_option_lines() {
        const COMMON: &str = "\
    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting
";
        for block in [
            USAGE,
            WRITE_USAGE,
            VERIFY_USAGE,
            EXPIRE_USAGE,
            REPACK_USAGE,
            COMPACT_USAGE,
        ] {
            assert!(block.contains(COMMON), "block missing common options:\n{block}");
            assert!(block.ends_with("\n\n"), "block must end with a blank line:\n{block}");
        }
    }

    /// Byte lengths measured from stock git 2.55 with
    /// `git multi-pack-index [<sub>] -h | wc -c`. A drift here means the usage
    /// text was edited and no longer reproduces git's stdout.
    #[test]
    fn usage_blocks_match_git_byte_lengths() {
        assert_eq!(USAGE.len(), 730);
        assert_eq!(WRITE_USAGE.len(), 988);
        assert_eq!(VERIFY_USAGE.len(), 225);
        assert_eq!(EXPIRE_USAGE.len(), 225);
        assert_eq!(REPACK_USAGE.len(), 366);
        assert_eq!(COMPACT_USAGE.len(), 622);
    }

    /// `compact`'s option list is its own: git lists `base` before `bitmap`
    /// there and omits the three `write`-only options entirely. Copying the
    /// `write` block over it would still pass the common-options check above.
    #[test]
    fn compact_usage_lists_its_own_options_in_gits_order() {
        let base = COMPACT_USAGE.find("--[no-]base").expect("base option listed");
        let bitmap = COMPACT_USAGE.find("--[no-]bitmap ").expect("bitmap option listed");
        assert!(base < bitmap, "compact lists base before bitmap");
        for absent in ["--[no-]stdin-packs", "--[no-]preferred-pack", "--[no-]refs-snapshot"] {
            assert!(
                !COMPACT_USAGE.contains(absent),
                "compact must not list {absent}"
            );
        }
        assert!(COMPACT_USAGE.contains("<from> <to>"), "compact takes two endpoints");
    }

    /// `--batch-size` is git's `OPT_MAGNITUDE`; every arm here is the verbatim
    /// classification stock git 2.55 gives (`git multi-pack-index repack
    /// --batch-size=<v>`), so a regression in the base-0 parse, the suffix set or
    /// the `unsigned long` bound would surface as a message/exit-code diff.
    #[test]
    fn batch_size_matches_opt_magnitude() {
        let ok = [
            "0", "1", "010", "0x10", "0X1F", "+1", " 1", "  10", "1k", "1K", "9g", "9G",
            "18446744073709551615",
        ];
        for v in ok {
            assert!(matches!(classify_magnitude(v), MagValue::Ok), "expected Ok for {v:?}");
            assert_eq!(batch_size_error(v), None, "{v:?} should be accepted");
        }
        assert!(matches!(classify_magnitude(""), MagValue::Empty));
        assert_eq!(
            batch_size_error("").as_deref(),
            Some("option `batch-size' expects a numerical value")
        );
        for v in ["abc", "-1", "-0", "1.5", ".5", "1kb", "1g1", "7 ", "0o17", "0b101"] {
            assert!(matches!(classify_magnitude(v), MagValue::Invalid), "expected Invalid for {v:?}");
            assert_eq!(
                batch_size_error(v).as_deref(),
                Some("option `batch-size' expects a non-negative integer value with an optional k/m/g suffix"),
                "{v:?}"
            );
        }
        for v in ["18446744073709551616", "20000000000000000000", "0xffffffffffffffffff"] {
            assert!(matches!(classify_magnitude(v), MagValue::Range), "expected Range for {v:?}");
            assert_eq!(
                batch_size_error(v),
                Some(format!("value {v} for option `batch-size' not in range [0,-1]"))
            );
        }
    }

    /// A flat MIDX is a one-layer chain for `compact`'s purposes, so an object
    /// store with neither a chain file nor a MIDX offers no endpoints at all —
    /// which is what turns `compact a b` into `could not find MIDX: a`.
    #[test]
    fn no_midx_means_no_compaction_endpoints() {
        let dir = std::env::temp_dir().join(format!("zvcs-midx-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("scratch dir");
        assert!(midx_chain_layers(&dir).is_empty());
        fs::remove_dir_all(&dir).ok();
    }
}
