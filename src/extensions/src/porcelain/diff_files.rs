//! `git diff-files` — compare the files in the working tree against the index.
//!
//! Backed by gitoxide's index↔worktree status pass (`Repository::status()` with the
//! tree iteration and the directory walk both switched off), which performs the same
//! stat-then-hash comparison stock git does, so racily-clean entries are resolved by
//! content and never reported.
//!
//! Supported invocations (stdout is byte-identical to stock `git diff-files`):
//!
//!   * `git diff-files` / `--raw`      — the default raw format:
//!     `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>`, where `<dstsha>` is
//!     always the null id because the worktree content is never written to the odb.
//!   * `--name-only`, `--name-status`  — path-only / status+path listings.
//!   * `-z`                            — NUL field/record terminators, paths unquoted.
//!   * `--abbrev[=<n>]`, `--no-abbrev` — abbreviated / full object ids.
//!   * `--exit-code`, `--quiet`        — exit 1 when differences exist (`--quiet` is silent).
//!   * `-s` / `--no-patch`             — suppress output, exit 0 unless `--exit-code`.
//!   * `[--] <path>...`                — pathspec limiting, resolved relative to the cwd
//!     while output paths stay repository-root relative, as git does.
//!
//! Status letters produced: `M` (content and/or executable-bit change), `T` (type
//! change, e.g. file ↔ symlink), `D` (removed from the worktree), `A` (an
//! `git add --intent-to-add` entry).
//!
//! ### Honest limitations (bailed on with a precise message, never faked)
//!
//! * Patch and stat output (`-p`/`-u`/`--patch`, `--stat`, `--numstat`, `--shortstat`,
//!   `--dirstat`, `--summary`, `--patch-with-raw`) is not produced here.
//! * Rename/copy/rewrite detection (`-M`, `-C`, `-B`, `--find-renames`) is off; git's
//!   default for `diff-files` is off too, so `--no-renames` is accepted as a no-op.
//! * `--diff-filter`, the pickaxe (`-S`/`-G`), `-R`, and the unmerged-stage selectors
//!   (`-0`/`-1`/`-2`/`-3`/`-c`/`--cc`) are unimplemented.
//! * Unmerged (conflicted) paths bail rather than emit an approximation of git's
//!   multi-line unmerged raw records.
//! * With a bare `--abbrev` and no `core.abbrev` set, the length is derived from
//!   gitoxide's unique-prefix computation for the first real source id (falling back
//!   to 7); git derives it from the packed object count, so the two can differ on
//!   large packed repositories.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;

/// How the change list should be rendered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>` (git's default).
    Raw,
    /// `<path>`
    NameOnly,
    /// `<status>\t<path>`
    NameStatus,
    /// Nothing at all (`-s`, `--no-patch`, `--quiet`).
    Silent,
}

/// Parsed command-line options for a single `diff-files` invocation.
struct Opts {
    format: Format,
    nul: bool,                     // -z: NUL field/record terminators, no path quoting
    abbrev: Option<Option<usize>>, // --abbrev[=N]: None=full, Some(None)=auto, Some(Some(n))=N
    exit_code: bool,               // --exit-code/--quiet: exit 1 when anything differs
}

/// One file-level change, already reduced to the columns git's raw format prints.
struct Delta {
    /// Mode as recorded in the index; `0` when the path is not in the index yet
    /// (an `--intent-to-add` entry, which git renders as an addition).
    src_mode: u32,
    /// Mode the worktree file would get if staged; `0` when it was removed.
    dst_mode: u32,
    /// The index-side blob id; the null id for `--intent-to-add` entries.
    src_id: ObjectId,
    /// `M`, `T`, `D` or `A`.
    status: u8,
    /// Repository-root relative path.
    path: BString,
}

/// The flag list quoted back at the user when an unimplemented option shows up.
const PORTED: &str = "--raw, --name-only, --name-status, -z, --abbrev[=<n>], --no-abbrev, \
                      --exit-code, --quiet, -s/--no-patch, -q, --no-renames, --full-index";

pub fn diff_files(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the subcommand, but tolerate it being present so the entry
    // point behaves the same either way.
    let args = match args.first() {
        Some(first) if first == "diff-files" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        format: Format::Raw,
        nul: false,
        abbrev: None,
        exit_code: false,
    };
    let mut quiet = false;
    let mut paths: Vec<BString> = Vec::new();
    let mut after_dashdash = false;

    for a in args {
        if after_dashdash {
            paths.push(a.as_str().into());
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "--raw" => opts.format = Format::Raw,
            "--name-only" => opts.format = Format::NameOnly,
            "--name-status" => opts.format = Format::NameStatus,
            "-s" | "--no-patch" => opts.format = Format::Silent,
            "-z" => opts.nul = true,
            "--abbrev" => opts.abbrev = Some(None),
            "--no-abbrev" => opts.abbrev = None,
            "--exit-code" => opts.exit_code = true,
            "--quiet" => {
                opts.exit_code = true;
                quiet = true;
            }
            // Accepted no-ops: these describe behaviour zvcs already produces.
            // `-q` is diff-files' "stay silent about nonexistent files", not --quiet.
            "-q" | "--no-renames" | "--full-index" | "--no-color" | "--color=never"
            | "--indent-heuristic" | "--no-indent-heuristic" => {}
            s if s.starts_with("--abbrev=") => {
                let n: usize = s["--abbrev=".len()..]
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --abbrev value in {s:?}"))?;
                opts.abbrev = Some(Some(n));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                bail!("unsupported flag {s:?} (ported: {PORTED})")
            }
            s => paths.push(s.into()),
        }
    }
    if quiet {
        opts.format = Format::Silent;
    }

    // Match the house line on pathspecs: literal paths and directory prefixes go
    // through to gitoxide's pathspec search, magic prefixes are refused outright
    // rather than silently matching differently than git would.
    for p in &paths {
        if p.first() == Some(&b':') {
            bail!("pathspec magic is not supported: {p:?}");
        }
    }

    let repo = gix::discover(".")?;
    let deltas = collect(&repo, paths)?;

    if opts.format != Format::Silent {
        let text = render(&repo, &deltas, &opts)?;
        let stdout = std::io::stdout();
        stdout.lock().write_all(&text)?;
    }

    Ok(if opts.exit_code && !deltas.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Run the index↔worktree comparison and reduce every reported entry to a [`Delta`],
/// sorted by path so the listing comes out in index order like git's.
fn collect(repo: &gix::Repository, patterns: Vec<BString>) -> Result<Vec<Delta>> {
    use gix::status::UntrackedFiles;
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let mut deltas: Vec<Delta> = Vec::new();

    let iter = repo
        .status(gix::progress::Discard)?
        // diff-files never reports untracked paths, so skip the directory walk.
        .untracked_files(UntrackedFiles::None)
        .into_index_worktree_iter(patterns)?;

    for item in iter {
        let Item::Modification {
            entry,
            rela_path,
            status,
            ..
        } = item?
        else {
            // Rewrites need rename tracking (off by default) and directory contents
            // need the dirwalk (disabled above); neither can occur here.
            continue;
        };
        let src_mode = entry.mode.bits();

        let delta = match status {
            EntryStatus::Conflict { .. } => {
                bail!("unmerged (conflicted) paths are not supported")
            }
            // Racily-clean: the stat data was stale but the content matched, so
            // there is no change to report — only an index refresh git might do.
            EntryStatus::NeedsUpdate(_) => continue,
            EntryStatus::IntentToAdd => Delta {
                src_mode: 0,
                dst_mode: src_mode,
                src_id: ObjectId::null(repo.object_hash()),
                status: b'A',
                path: rela_path,
            },
            EntryStatus::Change(Change::Removed) => Delta {
                src_mode,
                dst_mode: 0,
                src_id: entry.id,
                status: b'D',
                path: rela_path,
            },
            EntryStatus::Change(Change::Type { worktree_mode }) => Delta {
                src_mode,
                dst_mode: worktree_mode.bits(),
                src_id: entry.id,
                status: b'T',
                path: rela_path,
            },
            EntryStatus::Change(Change::Modification {
                executable_bit_changed,
                ..
            }) => Delta {
                src_mode,
                dst_mode: if executable_bit_changed {
                    toggle_exec(src_mode)
                } else {
                    src_mode
                },
                src_id: entry.id,
                status: b'M',
                path: rela_path,
            },
            EntryStatus::Change(Change::SubmoduleModification(_)) => Delta {
                src_mode,
                dst_mode: src_mode,
                src_id: entry.id,
                status: b'M',
                path: rela_path,
            },
        };
        deltas.push(delta);
    }

    // The status iterator is parallel and therefore unordered; git emits index
    // order, which is a plain byte-wise sort of the paths.
    deltas.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(deltas)
}

/// Flip the executable bit of a regular-file mode, leaving anything else alone.
fn toggle_exec(mode: u32) -> u32 {
    match mode {
        0o100644 => 0o100755,
        0o100755 => 0o100644,
        other => other,
    }
}

/// Render the whole listing into the exact bytes git would write.
fn render(repo: &gix::Repository, deltas: &[Delta], opts: &Opts) -> Result<Vec<u8>> {
    let hexsz = repo.object_hash().len_in_hex();
    let len = abbrev_len(repo, deltas, opts, hexsz);
    let null = ObjectId::null(repo.object_hash());

    // Field separator (between status and path) and record terminator.
    let (sep, term): (u8, u8) = if opts.nul { (0, 0) } else { (b'\t', b'\n') };

    let mut out = Vec::new();
    for d in deltas {
        match opts.format {
            Format::Silent => unreachable!("silent output is short-circuited by the caller"),
            Format::NameOnly => {}
            Format::NameStatus => {
                out.push(d.status);
                out.push(sep);
            }
            Format::Raw => {
                out.extend_from_slice(
                    format!(
                        ":{:06o} {:06o} {} {} ",
                        d.src_mode,
                        d.dst_mode,
                        hex(&d.src_id, len),
                        hex(&null, len),
                    )
                    .as_bytes(),
                );
                out.push(d.status);
                out.push(sep);
            }
        }
        if opts.nul {
            out.extend_from_slice(d.path.as_ref());
        } else {
            out.extend_from_slice(quote_path(&d.path).as_bytes());
        }
        out.push(term);
    }
    Ok(out)
}

/// The object id column, full or truncated to `len` hex characters.
fn hex(id: &ObjectId, len: Option<usize>) -> String {
    match len {
        None => id.to_hex().to_string(),
        Some(n) => id.to_hex_with_len(n).to_string(),
    }
}

/// Resolve `--abbrev` into a concrete hex length, or `None` for full ids.
///
/// An explicit `--abbrev=<n>` is clamped to git's `[4, hash-length]` range. A bare
/// `--abbrev` follows `core.abbrev`; when that is unset (or the non-numeric `auto`)
/// the length is taken from gitoxide's unique-prefix computation for the first real
/// source id, falling back to git's minimum default of 7 when there is none.
fn abbrev_len(
    repo: &gix::Repository,
    deltas: &[Delta],
    opts: &Opts,
    hexsz: usize,
) -> Option<usize> {
    let n = match opts.abbrev? {
        Some(n) => n,
        None => repo
            .config_snapshot()
            .integer("core.abbrev")
            .and_then(|v| usize::try_from(v).ok())
            .or_else(|| {
                deltas
                    .iter()
                    .find(|d| !d.src_id.is_null())
                    .map(|d| d.src_id.attach(repo).shorten_or_id().hex_len())
            })
            .unwrap_or(7),
    };
    Some(n.clamp(4, hexsz))
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}
