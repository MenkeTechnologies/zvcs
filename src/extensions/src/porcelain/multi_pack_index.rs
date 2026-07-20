//! `git multi-pack-index` — write and verify a multi-pack-index (MIDX).
//!
//! Covered: the `write` and `verify` sub-commands in their default (v1,
//! non-incremental, non-bitmap) form, the global `--object-dir=<dir>` /
//! `--object-dir <dir>` / `--no-object-dir` and `--progress` / `--no-progress`
//! options, and the `-h` usage blocks for the top level and for `write`,
//! `verify`, `expire` and `repack` — each reproduced byte-for-byte along with
//! git's exit code 129. `write` and `verify` print nothing on success, so the
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
//! Not covered — these `bail!` rather than producing a diverging artifact:
//!
//!   * `write --bitmap` / `--preferred-pack=` / `--refs-snapshot=` — the
//!     vendored `gix-pack` has no multi-pack bitmap writer at all
//!     (`src/ported/gix-pack/src/multi_index/` has `write.rs` and `verify.rs`
//!     but no bitmap module), and `--preferred-pack` only has an observable
//!     effect through bitmap generation and duplicate tie-breaking that the
//!     writer does not expose.
//!   * `write --incremental` / `--base=` / `--write-chain-file`, and the
//!     `compact` sub-command — these produce MIDX chains under
//!     `<objdir>/pack/multi-pack-index.d/`; `gix_pack::multi_index::Version`
//!     has only `V1` and the writer always emits zero base files, so there is
//!     no chain substrate to build on.
//!   * `write --stdin-packs` — the writer takes a path list, so this could be
//!     fed, but git's cruft-pack handling for the stdin set is not modelled and
//!     a wrong pack set is a silently wrong index.
//!   * `expire` and `repack` — both delete or create pack files and then
//!     rewrite the MIDX; `gix-pack` has no pack-repacking driver, and getting
//!     either wrong destroys objects.
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

/// `git multi-pack-index repack -h`.
const REPACK_USAGE: &str = "\
usage: git multi-pack-index [<options>] repack [--batch-size=<size>]

    --[no-]object-dir <directory>
                          object directory containing set of packfile and pack-index pairs
    --[no-]progress       force progress reporting
    --batch-size <n>      during repack, collect pack-files of smaller size into a batch that is larger than this size

";

/// `git multi-pack-index` — write or verify the multi-pack-index.
///
/// Supported forms (stdout, exit code and resulting MIDX file matching stock git):
///   * `git multi-pack-index write`                    → `<objdir>/pack/multi-pack-index`
///   * `git multi-pack-index verify`                   → silent, exit 0 (exit 1 on damage)
///   * `--object-dir=<dir>` / `--object-dir <dir>` / `--no-object-dir` on either
///   * `--progress` / `--no-progress` (accepted; progress is discarded, git's
///     own progress goes to stderr and never to stdout)
///   * `-h` at the top level and on `write` / `verify` / `expire` / `repack`
///
/// Every other flag and the `compact` / `expire` / `repack` sub-commands
/// `bail!` — see the module docs for the specific missing substrate.
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
        if let Some(name) = a.strip_prefix("--") {
            let name = name.split('=').next().unwrap_or(name);
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
        // These three parse cleanly but have no implementation behind them; a
        // usage block would misrepresent that, so only `-h` is honoured.
        Some("expire") => {
            if rest.iter().any(|a| *a == "-h") {
                print!("{EXPIRE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            bail!("unsupported subcommand \"expire\" (ported: write, verify) — deleting packs and rewriting the MIDX needs a pack-expiry driver that gix-pack does not provide")
        }
        Some("repack") => {
            if rest.iter().any(|a| *a == "-h") {
                print!("{REPACK_USAGE}");
                return Ok(ExitCode::from(129));
            }
            bail!("unsupported subcommand \"repack\" (ported: write, verify) — batched repacking needs a pack writer that gix-pack does not provide")
        }
        Some("compact") => {
            bail!("unsupported subcommand \"compact\" (ported: write, verify) — MIDX chains need a v2 incremental writer; gix_pack::multi_index::Version has only V1")
        }
        Some(other) => Ok(usage_error(
            Some(&format!("unknown subcommand: `{other}'")),
            USAGE,
        )),
    }
}

/// `write`: index every pack in the object directory into a fresh MIDX.
fn write(rest: &[&str], mut object_dir: Option<PathBuf>) -> Result<ExitCode> {
    let mut it = rest.iter().copied();
    while let Some(a) = it.next() {
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
            // Already the defaults; accepting them changes nothing.
            "--no-bitmap" | "--no-incremental" | "--no-stdin-packs" | "--no-write-chain-file"
            | "--no-preferred-pack" | "--no-refs-snapshot" | "--no-base" => {}
            "--bitmap" => bail!(
                "unsupported flag \"--bitmap\" (ported: --object-dir, --progress) — gix-pack has no multi-pack bitmap writer"
            ),
            "--incremental" | "--write-chain-file" => bail!(
                "unsupported flag {a:?} (ported: --object-dir, --progress) — gix-pack writes v1 MIDX only, with no chain support"
            ),
            "--stdin-packs" => bail!(
                "unsupported flag \"--stdin-packs\" (ported: --object-dir, --progress) — git's cruft-pack rules for the stdin set are not modelled"
            ),
            _ if a.starts_with("--preferred-pack") => bail!(
                "unsupported flag {a:?} (ported: --object-dir, --progress) — preferred-pack tie-breaking is not exposed by gix_pack::multi_index::write_from_index_paths"
            ),
            _ if a.starts_with("--refs-snapshot") => bail!(
                "unsupported flag {a:?} (ported: --object-dir, --progress) — only meaningful with --bitmap, which is unported"
            ),
            _ if a.starts_with("--base") => bail!(
                "unsupported flag {a:?} (ported: --object-dir, --progress) — requires incremental MIDX layers, which gix-pack cannot write"
            ),
            _ if a.starts_with("--") => {
                let name = a.trim_start_matches('-').split('=').next().unwrap_or(a);
                return Ok(usage_error(
                    Some(&format!("unknown option `{name}'")),
                    WRITE_USAGE,
                ));
            }
            _ => return Ok(usage_error(None, WRITE_USAGE)),
        }
    }

    let repo = gix::discover(".")?;
    let objdir = object_dir.unwrap_or_else(|| repo.objects.store_ref().path().to_path_buf());
    let pack_dir = objdir.join("pack");

    // An existing MIDX chain would have to be torn down and rewritten; we do
    // not model that, and silently leaving it beside a fresh flat MIDX would
    // leave the repository in a state git never produces.
    if pack_dir.join("multi-pack-index.d").exists() {
        bail!("an incremental multi-pack-index chain is present at {:?}; rewriting it is unported", pack_dir.join("multi-pack-index.d"));
    }

    let index_paths = pack_indices(&pack_dir);
    if index_paths.is_empty() {
        // `midx-write.c`: `error(_("no pack files to index."))`, and cmd_* hands
        // the -1 straight back to git, which exits 255.
        eprintln!("error: no pack files to index.");
        return Ok(ExitCode::from(255));
    }

    // git writes through `multi-pack-index.lock` and renames on success, so a
    // failed write never replaces a good index.
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
        multi_index::write::Options {
            object_hash: repo.object_hash(),
        },
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
    for name in dir_names(&pack_dir) {
        if name.starts_with("multi-pack-index-") && (name.ends_with(".bitmap") || name.ends_with(".rev")) {
            fs::remove_file(pack_dir.join(name)).ok();
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// `verify`: check the MIDX against the pack indices it references.
fn verify(rest: &[&str], mut object_dir: Option<PathBuf>) -> Result<ExitCode> {
    let mut it = rest.iter().copied();
    while let Some(a) = it.next() {
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
                let name = a.trim_start_matches('-').split('=').next().unwrap_or(a);
                return Ok(usage_error(
                    Some(&format!("unknown option `{name}'")),
                    VERIFY_USAGE,
                ));
            }
            // `git multi-pack-index verify extra` prints the usage block with no
            // `error:` line at all.
            _ => return Ok(usage_error(None, VERIFY_USAGE)),
        }
    }

    let repo = gix::discover(".")?;
    let objdir = object_dir.unwrap_or_else(|| repo.objects.store_ref().path().to_path_buf());
    let midx = objdir.join("pack").join("multi-pack-index");

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
        for block in [USAGE, WRITE_USAGE, VERIFY_USAGE, EXPIRE_USAGE, REPACK_USAGE] {
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
    }
}
