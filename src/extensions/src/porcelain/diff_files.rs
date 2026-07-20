//! `git diff-files` — compare the files in the working tree against the index.
//!
//! ### Why this is a *stat* comparison, not a content comparison
//!
//! `git diff-files` never hashes worktree content. `run_diff_files()` calls
//! `ce_match_stat()` with `CE_MATCH_RACY_IS_DIRTY`, so a file whose cached stat
//! data no longer matches the filesystem is reported as modified even when its
//! bytes are identical to the staged blob — which is exactly why the destination
//! object id column is the null id: nothing was hashed to fill it in.
//!
//! ```text
//! $ cp -R repo copy && cd copy && git diff-files
//! :100644 100644 45b983be…  0000000000…  M    a.txt
//! ```
//!
//! gitoxide's high-level `Repository::status()` iterator answers a different
//! question: it re-hashes on a stat mismatch and swallows the resulting
//! `EntryStatus::NeedsUpdate` items inside `Iter::maybe_keep_index_change` so
//! callers only see real content changes. This module therefore drives the
//! lower-level `Repository::index_worktree_status()` with a [`StatOnly`] blob
//! comparator that never claims two blobs are equal. gix's own fast path still
//! returns "unchanged" before the comparator is consulted whenever the stat data
//! matches and the entry is not racily clean, so the result is git's rule.
//!
//! ### Supported invocations (stdout is byte-identical to stock git)
//!
//!   * `git diff-files` / `--raw` — `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>`.
//!   * `--name-only`, `--name-status`, `-z`, `--abbrev[=<n>]`, `--no-abbrev`, `--full-index`.
//!   * `--exit-code`, `--quiet`, `-s`/`--no-patch`.
//!   * `--line-prefix=<s>`, `--rotate-to=<p>`, `--skip-to=<p>`, `--relative[=<p>]`/`--no-relative`.
//!   * `--ignore-submodules[=all|dirty|untracked|none]`.
//!   * `[--] <pathspec>...`, including magic (`:!`, `:(icase)`, `:(glob)`) and globs,
//!     with the same revision-vs-path disambiguation git performs: an argument that
//!     resolves to a revision is a usage error (129), one that is neither a revision
//!     nor an existing path is `fatal: ambiguous argument` (128).
//!   * Options that only configure *patch* rendering (`--patience`, `--textconv`,
//!     `--word-diff`, `--src-prefix=`, `-W`, `-a`, …) are accepted because they
//!     provably do not change raw/name output.
//!
//! ### Not implemented (bailed on with a precise message, never faked)
//!
//! Anything that makes git inspect content and therefore changes *which* paths
//! are listed: `-p`/`--stat`/`--numstat`/`--dirstat`/`--summary`/`--check`/
//! `--binary`, the whitespace-ignoring family (`-w`, `-b`, `-I<re>`,
//! `--ignore-space-at-eol`, `--ignore-cr-at-eol`), the pickaxe (`-S`/`-G`), `-R`,
//! `--diff-filter`, rename/copy/rewrite detection (`-M`/`-C`/`-B`), and the
//! unmerged-stage selectors (`-0`…`-3`, `--base`/`--ours`/`--theirs`, `-c`/`--cc`).

use anyhow::Result;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

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

/// The `--relative[=<p>]` / `--no-relative` selection.
enum Relative {
    /// git's default for `diff-files`: paths stay repository-root relative.
    No,
    /// Bare `--relative`: use the current directory's prefix within the worktree.
    Cwd,
    /// `--relative=<p>`: use the given directory as the prefix.
    Path(BString),
}

/// Where the listing should be re-anchored, per `--rotate-to`/`--skip-to`.
enum Anchor {
    /// `--rotate-to=<p>`: move everything before `<p>` to the end.
    Rotate(BString),
    /// `--skip-to=<p>`: drop everything before `<p>`.
    Skip(BString),
}

/// Parsed command-line options for a single `diff-files` invocation.
struct Opts {
    format: Format,
    nul: bool,                     // -z: NUL field/record terminators, no path quoting
    abbrev: Option<Option<usize>>, // --abbrev[=N]: None=full, Some(None)=auto, Some(Some(n))=N
    exit_code: bool,               // --exit-code/--quiet: exit 1 when anything differs
    line_prefix: Vec<u8>,          // --line-prefix=<s>, emitted before every record
    anchor: Option<Anchor>,
    relative: Relative,
    /// `--ignore-submodules[=<when>]`; `None` leaves gix on its configured default.
    ignore_submodules: Option<gix::submodule::config::Ignore>,
}

/// One record of git's raw output. A conflicted path produces two of these.
struct Delta {
    src_mode: u32,
    dst_mode: u32,
    src_id: ObjectId,
    dst_id: ObjectId,
    /// `M`, `T`, `D`, `A` or `U`.
    status: u8,
    /// Repository-root relative path.
    path: BString,
    /// The `U` record git prints ahead of the stage-2 comparison for a conflict.
    unmerged: bool,
}

/// A fatal condition that has to reach the shell with git's own exit code,
/// since `anyhow::bail!` would collapse everything to 1.
enum Fatal {
    /// git's `usage(diff_files_usage)`, exit 129.
    Usage,
    /// `fatal: ambiguous argument '<arg>': …`, exit 128.
    Ambiguous(String),
    /// `fatal: '<rest>': not an integer` from `-n<rest>`, exit 128.
    NotAnInteger(String),
    /// `error: -n requires an argument`, exit 128.
    MissingArgument(&'static str),
    /// `fatal: empty string is not a valid pathspec…`, exit 128.
    EmptyPathspec,
    /// `fatal: No such path '<p>' in the diff` from `--rotate-to`/`--skip-to`, exit 128.
    NoSuchPath(String),
}

impl Fatal {
    /// Report on stderr the way git does and hand back git's exit code.
    fn report(self) -> ExitCode {
        let mut err = std::io::stderr().lock();
        match self {
            Fatal::Usage => {
                let _ = writeln!(
                    err,
                    "usage: git diff-files [-q] [-0 | -1 | -2 | -3 | -c | --cc] \
                     [<common-diff-options>] [<path>...]"
                );
                return ExitCode::from(129);
            }
            Fatal::Ambiguous(arg) => {
                let _ = writeln!(
                    err,
                    "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
                     Use '--' to separate paths from revisions, like this:\n\
                     'git <command> [<revision>...] -- [<file>...]'"
                );
            }
            Fatal::NotAnInteger(v) => {
                let _ = writeln!(err, "fatal: '{v}': not an integer");
            }
            Fatal::MissingArgument(flag) => {
                let _ = writeln!(err, "error: {flag} requires an argument");
            }
            Fatal::EmptyPathspec => {
                let _ = writeln!(
                    err,
                    "fatal: empty string is not a valid pathspec. \
                     please use . instead if you meant to match all paths"
                );
            }
            Fatal::NoSuchPath(p) => {
                let _ = writeln!(err, "fatal: No such path '{p}' in the diff");
            }
        }
        ExitCode::from(128)
    }
}

/// The flag list quoted back at the user when an unimplemented option shows up.
const PORTED: &str = "--raw, --name-only, --name-status, -z, --abbrev[=<n>], --no-abbrev, \
                      --exit-code, --quiet, -s/--no-patch, -q, --no-renames, --full-index, \
                      --line-prefix=<s>, --rotate-to=<p>, --skip-to=<p>, --relative[=<p>], \
                      --ignore-submodules[=<when>]";

/// Values git accepts for `--diff-algorithm=`.
const DIFF_ALGORITHMS: &[&str] = &["myers", "minimal", "patience", "histogram", "default"];

pub fn diff_files(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the subcommand, but tolerate it being present so the entry
    // point behaves the same either way.
    let args = match args.first() {
        Some(first) if first == "diff-files" => &args[1..],
        _ => args,
    };

    let repo = gix::discover(".")?;
    match parse(&repo, args) {
        Ok(Parsed::Run { opts, paths }) => run(&repo, opts, paths),
        Ok(Parsed::Unsupported(flag)) => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(
                err,
                "zvcs: diff-files: unsupported flag {flag:?} (ported: {PORTED})"
            );
            Ok(ExitCode::from(1))
        }
        Err(fatal) => Ok(fatal.report()),
    }
}

/// The outcome of argument parsing: either a runnable request, or the first
/// real-git flag we have not ported.
enum Parsed {
    Run { opts: Opts, paths: Vec<BString> },
    Unsupported(String),
}

/// Parse `args` the way `setup_revisions()` plus `cmd_diff_files()` do.
///
/// Argument classification is strictly left to right, because git reports the
/// first problem it walks into: `git diff-files -u does-not-exist` fails on the
/// path (128), never on `-u`, since `-u` is a perfectly valid git flag that
/// simply never gets to influence anything. Flags we have not ported are
/// therefore recorded and reported only after every argument has been validated.
fn parse(repo: &gix::Repository, args: &[String]) -> Result<Parsed, Fatal> {
    let mut opts = Opts {
        format: Format::Raw,
        nul: false,
        abbrev: None,
        exit_code: false,
        line_prefix: Vec::new(),
        anchor: None,
        relative: Relative::No,
        ignore_submodules: None,
    };
    let mut quiet = false;
    let mut paths: Vec<BString> = Vec::new();
    let mut unsupported: Option<String> = None;
    let mut after_dashdash = false;

    for a in args {
        let s = a.as_str();
        if after_dashdash {
            if s.is_empty() {
                return Err(Fatal::EmptyPathspec);
            }
            paths.push(s.into());
            continue;
        }
        if s == "--" {
            after_dashdash = true;
            continue;
        }
        if s.starts_with('-') && s.len() > 1 {
            match classify(s, &mut opts, &mut quiet)? {
                Flag::Handled => {}
                Flag::Unsupported => {
                    if unsupported.is_none() {
                        unsupported = Some(s.to_owned());
                    }
                }
                Flag::Unknown => return Err(Fatal::Usage),
            }
            continue;
        }
        // A bare argument is a revision, an existing path, or an error — git
        // tries them in that order and dies on the first one that fits none.
        if repo.rev_parse_single(s).is_ok() {
            return Err(Fatal::Usage);
        }
        if !looks_like_pathspec(s) && !names_an_existing_file(s) {
            return Err(Fatal::Ambiguous(s.to_owned()));
        }
        paths.push(s.into());
    }

    if quiet {
        opts.format = Format::Silent;
    }
    Ok(match unsupported {
        Some(flag) => Parsed::Unsupported(flag),
        None => Parsed::Run { opts, paths },
    })
}

/// What parsing decided about a single dash-prefixed argument.
enum Flag {
    /// Recognized, and either applied or provably a no-op for this output format.
    Handled,
    /// A real git flag that would change the result and is not ported.
    Unsupported,
    /// Not a git flag at all — git answers with its usage text.
    Unknown,
}

/// Options that only configure how a *patch* is rendered. `diff-files` produces
/// raw/name output here, which none of them can reach, so accepting them yields
/// byte-identical results rather than silently dropping a behavior.
const PATCH_ONLY: &[&str] = &[
    "--indent-heuristic",
    "--no-indent-heuristic",
    "--minimal",
    "--patience",
    "--histogram",
    "--submodule",
    "--color",
    "--no-color",
    "--color-moved",
    "--no-color-moved",
    "--no-color-moved-ws",
    "--word-diff",
    "--color-words",
    "--ws-error-highlight",
    "--irreversible-delete",
    "-D",
    "--text",
    "-a",
    "--function-context",
    "-W",
    "--ext-diff",
    "--no-ext-diff",
    "--textconv",
    "--no-textconv",
    "--no-prefix",
    "--default-prefix",
    "--ita-invisible-in-index",
    "--ita-visible-in-index",
    "--ignore-blank-lines",
    "--find-copies-harder",
    "--pickaxe-all",
    "--pickaxe-regex",
    // Rename detection is off for diff-files, so its knobs cannot bite.
    "--no-renames",
    "--rename-empty",
    "--no-rename-empty",
    // Raw output already carries full object ids.
    "--full-index",
    // diff-files' "stay quiet about removed files"; zvcs never warns about them.
    "-q",
];

/// Prefixes of valued patch-only options, same reasoning as [`PATCH_ONLY`].
const PATCH_ONLY_VALUED: &[&str] = &[
    "--anchored=",
    "--inter-hunk-context=",
    "--output-indicator-new=",
    "--output-indicator-old=",
    "--output-indicator-context=",
    "--submodule=",
    "--color=",
    "--color-moved=",
    "--color-moved-ws=",
    "--word-diff=",
    "--word-diff-regex=",
    "--color-words=",
    "--ws-error-highlight=",
    "--src-prefix=",
    "--dst-prefix=",
    "--diff-merges=",
    "-l",
];

/// Real git flags whose effect on *which records are printed* we do not produce.
const KNOWN_UNSUPPORTED: &[&str] = &[
    "-p",
    "-u",
    "--patch",
    "--patch-with-raw",
    "--patch-with-stat",
    "--stat",
    "--compact-summary",
    "--numstat",
    "--shortstat",
    "--dirstat",
    "--dirstat-by-file",
    "--cumulative",
    "--summary",
    "--check",
    "--binary",
    "--find-object",
    "-B",
    "--break-rewrites",
    "-M",
    "--find-renames",
    "-C",
    "--find-copies",
    "-R",
    "--ignore-cr-at-eol",
    "--ignore-space-at-eol",
    "-b",
    "--ignore-space-change",
    "-w",
    "--ignore-all-space",
    "-0",
    "-1",
    "-2",
    "-3",
    "--base",
    "--ours",
    "--theirs",
    "-c",
    "--cc",
];

/// Prefixes of real git flags in the same category as [`KNOWN_UNSUPPORTED`].
const KNOWN_UNSUPPORTED_VALUED: &[&str] = &[
    "-U",
    "--unified=",
    "--stat=",
    "--stat-width=",
    "--stat-name-width=",
    "--stat-count=",
    "--stat-graph-width=",
    "--dirstat=",
    "--dirstat-by-file=",
    "--break-rewrites=",
    "--find-renames=",
    "--find-copies=",
    "--diff-filter=",
    "-S",
    "-G",
    "-I",
    "--ignore-matching-lines=",
    "-B",
    "-M",
    "-C",
    "-O",
    "--output=",
];

fn classify(s: &str, opts: &mut Opts, quiet: &mut bool) -> Result<Flag, Fatal> {
    match s {
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
            *quiet = true;
        }
        "--relative" => opts.relative = Relative::Cwd,
        "--no-relative" => opts.relative = Relative::No,
        "--ignore-submodules" => {
            opts.ignore_submodules = Some(gix::submodule::config::Ignore::All);
        }
        _ => return classify_valued(s, opts),
    }
    Ok(Flag::Handled)
}

fn classify_valued(s: &str, opts: &mut Opts) -> Result<Flag, Fatal> {
    if let Some(n) = s.strip_prefix("--abbrev=") {
        // git clamps rather than rejecting, so a bad value is a usage error.
        let n: usize = n.parse().map_err(|_| Fatal::Usage)?;
        opts.abbrev = Some(Some(n));
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--line-prefix=") {
        opts.line_prefix = v.as_bytes().to_vec();
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--rotate-to=") {
        opts.anchor = Some(Anchor::Rotate(v.into()));
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--skip-to=") {
        opts.anchor = Some(Anchor::Skip(v.into()));
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--relative=") {
        opts.relative = Relative::Path(v.trim_end_matches('/').into());
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--ignore-submodules=") {
        use gix::submodule::config::Ignore;
        opts.ignore_submodules = Some(match v {
            "all" => Ignore::All,
            "dirty" => Ignore::Dirty,
            "untracked" => Ignore::Untracked,
            "none" => Ignore::None,
            _ => return Err(Fatal::Usage),
        });
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--diff-algorithm=") {
        return if DIFF_ALGORITHMS.contains(&v) {
            Ok(Flag::Handled)
        } else {
            Err(Fatal::Usage)
        };
    }
    // `-n<count>` is `--max-count`; diff-files rejects any revision limiting,
    // but only after the value itself parses.
    if let Some(v) = s.strip_prefix("-n") {
        return if v.is_empty() {
            Err(Fatal::MissingArgument("-n"))
        } else if v.parse::<i32>().is_ok() {
            Err(Fatal::Usage)
        } else {
            Err(Fatal::NotAnInteger(v.to_owned()))
        };
    }
    if PATCH_ONLY.contains(&s) || PATCH_ONLY_VALUED.iter().any(|p| s.starts_with(p)) {
        return Ok(Flag::Handled);
    }
    if KNOWN_UNSUPPORTED.contains(&s) || KNOWN_UNSUPPORTED_VALUED.iter().any(|p| s.starts_with(p)) {
        return Ok(Flag::Unsupported);
    }
    Ok(Flag::Unknown)
}

/// git's `looks_like_pathspec()`: long-form magic, or an unescaped glob character.
fn looks_like_pathspec(arg: &str) -> bool {
    if arg.starts_with(":(") {
        return true;
    }
    let mut escaped = false;
    for b in arg.bytes() {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if matches!(b, b'*' | b'?' | b'[') {
            return true;
        }
    }
    false
}

/// git's `check_filename()`: strip the short-form magic prefixes, then `lstat`.
/// A bare `:/`, `:!` or `:^` is a whole-tree pathspec and needs no file behind it.
fn names_an_existing_file(arg: &str) -> bool {
    for magic in [":/", ":!", ":^"] {
        if let Some(rest) = arg.strip_prefix(magic) {
            return rest.is_empty() || Path::new(rest).symlink_metadata().is_ok();
        }
    }
    !arg.is_empty() && Path::new(arg).symlink_metadata().is_ok()
}

fn run(repo: &gix::Repository, opts: Opts, paths: Vec<BString>) -> Result<ExitCode> {
    let mut deltas = collect(repo, paths, &opts)?;

    // git emits index order, which for these records is a byte-wise path sort
    // with a conflict's `U` line kept ahead of its stage-2 comparison.
    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    // Rotation runs before `--relative` strips anything: `git diff-files
    // --relative=src --rotate-to=src/lib.rs` succeeds while `--rotate-to=lib.rs`
    // fails, so the anchor always names the repository-root relative path.
    match &opts.anchor {
        None => {}
        Some(Anchor::Rotate(p)) => match deltas.iter().position(|d| &d.path == p) {
            Some(i) => deltas.rotate_left(i),
            None => return Ok(Fatal::NoSuchPath(p.to_string()).report()),
        },
        Some(Anchor::Skip(p)) => match deltas.iter().position(|d| &d.path == p) {
            Some(i) => {
                deltas.drain(..i);
            }
            None => return Ok(Fatal::NoSuchPath(p.to_string()).report()),
        },
    }

    apply_relative(repo, &mut deltas, &opts.relative)?;

    if opts.format != Format::Silent {
        let text = render(repo, &deltas, &opts);
        std::io::stdout().lock().write_all(&text)?;
    }

    Ok(if opts.exit_code && !deltas.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// `--relative[=<p>]`: keep only records under `<p>`, with that prefix stripped.
fn apply_relative(
    repo: &gix::Repository,
    deltas: &mut Vec<Delta>,
    relative: &Relative,
) -> Result<()> {
    let prefix: BString = match relative {
        Relative::No => return Ok(()),
        Relative::Path(p) => p.clone(),
        Relative::Cwd => match repo.prefix()? {
            Some(p) => gix::path::into_bstr(p).into_owned(),
            None => return Ok(()),
        },
    };
    if prefix.is_empty() {
        return Ok(());
    }
    let mut needle: Vec<u8> = prefix.into();
    needle.push(b'/');
    deltas.retain_mut(
        |d| match d.path.strip_prefix(needle.as_slice()).map(|r| r.to_vec()) {
            Some(rest) => {
                d.path = rest.into();
                true
            }
            None => false,
        },
    );
    Ok(())
}

/// A blob comparator that never reports equality.
///
/// gix only consults this once the cheap stat comparison has already failed (or
/// flagged the entry as racily clean), which is precisely when git declares the
/// file modified without looking at content. Returning `Some` here is what turns
/// gix's content-accurate status into git's stat-accurate `diff-files`.
#[derive(Clone)]
struct StatOnly;

impl gix::status::plumbing::index_as_worktree::traits::CompareBlobs for StatOnly {
    type Output = ();

    fn compare_blobs<'a, 'b>(
        &mut self,
        _entry: &gix::index::Entry,
        _worktree_blob_size: u64,
        _data: impl gix::status::plumbing::index_as_worktree::traits::ReadData<'a>,
        _buf: &mut Vec<u8>,
    ) -> Result<Option<Self::Output>, gix::status::plumbing::index_as_worktree::Error> {
        Ok(Some(()))
    }
}

/// Accumulates one or two [`Delta`]s per visited index entry.
struct Collector<'a> {
    workdir: &'a Path,
    executable_bit: bool,
    null: ObjectId,
    deltas: Vec<Delta>,
}

impl Collector<'_> {
    /// The mode git would record for the worktree file at `rela_path`, or `0`
    /// when it is gone. Mirrors `ce_mode_from_stat()`, including the fact that a
    /// filesystem without a usable executable bit always yields `100644`.
    fn worktree_mode(&self, rela_path: &gix::bstr::BStr) -> u32 {
        let rela = gix::path::from_bstr(rela_path);
        let path = self.workdir.join(&*rela);
        let Ok(md) = std::fs::symlink_metadata(&path) else {
            return 0;
        };
        let ft = md.file_type();
        if ft.is_symlink() {
            return 0o120000;
        }
        if ft.is_dir() {
            return 0;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if self.executable_bit && md.permissions().mode() & 0o111 != 0 {
                return 0o100755;
            }
        }
        0o100644
    }
}

impl<'index> gix::status::plumbing::index_as_worktree_with_renames::VisitEntry<'index>
    for Collector<'_>
{
    type ContentChange = ();
    type SubmoduleStatus = gix::submodule::Status;

    fn visit_entry(
        &mut self,
        entry: gix::status::plumbing::index_as_worktree_with_renames::Entry<
            'index,
            Self::ContentChange,
            Self::SubmoduleStatus,
        >,
    ) {
        use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
        use gix::status::plumbing::index_as_worktree_with_renames::Entry;

        let Entry::Modification {
            entry,
            rela_path,
            status,
            ..
        } = entry
        else {
            // The dirwalk and rename tracking are both disabled below, so no
            // other variant can be produced.
            return;
        };
        let src_mode = entry.mode.bits();
        let path: BString = rela_path.to_owned();

        let delta = match status {
            EntryStatus::Conflict { entries, .. } => {
                // git prints an unmerged marker, then repeats the comparison
                // against the stage-2 ("ours") entry, which is what
                // `diff_unmerged_stage` selects by default.
                let wt_mode = self.worktree_mode(rela_path);
                self.deltas.push(Delta {
                    src_mode: 0,
                    dst_mode: wt_mode,
                    src_id: self.null,
                    dst_id: self.null,
                    status: b'U',
                    path: path.clone(),
                    unmerged: true,
                });
                let Some(ours) = entries[1].as_ref() else {
                    return;
                };
                Delta {
                    src_mode: ours.mode.bits(),
                    dst_mode: wt_mode,
                    src_id: ours.id,
                    dst_id: self.null,
                    status: if wt_mode == 0 { b'D' } else { b'M' },
                    path,
                    unmerged: false,
                }
            }
            // Unreachable with [`StatOnly`]: gix only emits this when content
            // comparison proved the entry clean, which that comparator never does.
            EntryStatus::NeedsUpdate(_) => return,
            EntryStatus::IntentToAdd => Delta {
                src_mode: 0,
                dst_mode: src_mode,
                src_id: self.null,
                dst_id: self.null,
                status: b'A',
                path,
                unmerged: false,
            },
            EntryStatus::Change(Change::Removed) => Delta {
                src_mode,
                dst_mode: 0,
                src_id: entry.id,
                dst_id: self.null,
                status: b'D',
                path,
                unmerged: false,
            },
            EntryStatus::Change(Change::Type { worktree_mode }) => Delta {
                src_mode,
                dst_mode: worktree_mode.bits(),
                src_id: entry.id,
                dst_id: self.null,
                status: b'T',
                path,
                unmerged: false,
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
                dst_id: self.null,
                status: b'M',
                path,
                unmerged: false,
            },
            EntryStatus::Change(Change::SubmoduleModification(sm)) => {
                // A submodule whose checked-out `HEAD` still matches the index is
                // only "dirty" inside; git leaves the destination id filled in
                // rather than nulling it, since the gitlink itself is unchanged.
                let moved = sm.checked_out_head_id != sm.index_id;
                Delta {
                    src_mode,
                    dst_mode: src_mode,
                    src_id: entry.id,
                    dst_id: if moved { self.null } else { entry.id },
                    status: b'M',
                    path,
                    unmerged: false,
                }
            }
        };
        self.deltas.push(delta);
    }
}

/// Run the index↔worktree stat comparison and reduce every entry to [`Delta`]s.
fn collect(repo: &gix::Repository, patterns: Vec<BString>, opts: &Opts) -> Result<Vec<Delta>> {
    let index = repo.index_or_empty()?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("this operation must be run in a work tree"))?
        .to_owned();
    let caps = repo.filesystem_options()?;

    let submodules = match opts.ignore_submodules {
        Some(ignore) => gix::status::Submodule::Given {
            ignore,
            check_dirty: false,
        },
        None => gix::status::Submodule::default(),
    };
    let submodule = gix::status::index_worktree::BuiltinSubmoduleStatus::new(
        repo.clone().into_sync(),
        submodules,
    )?;

    let mut collector = Collector {
        workdir: workdir.as_path(),
        executable_bit: caps.executable_bit,
        null: ObjectId::null(repo.object_hash()),
        deltas: Vec::new(),
    };
    let mut progress = gix::progress::Discard;
    let should_interrupt = AtomicBool::new(false);

    repo.index_worktree_status(
        &index,
        patterns,
        &mut collector,
        StatOnly,
        submodule,
        &mut progress,
        &should_interrupt,
        gix::status::index_worktree::Options {
            sorting: Some(
                gix::status::plumbing::index_as_worktree_with_renames::Sorting::ByPathCaseSensitive,
            ),
            // diff-files never reports untracked paths, and rename detection is
            // off by default here, so neither extra pass is worth running.
            dirwalk_options: None,
            rewrites: None,
            thread_limit: None,
        },
    )?;

    Ok(collector.deltas)
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
fn render(repo: &gix::Repository, deltas: &[Delta], opts: &Opts) -> Vec<u8> {
    let hexsz = repo.object_hash().len_in_hex();
    let len = abbrev_len(repo, deltas, opts, hexsz);

    // Field separator (between status and path) and record terminator.
    let (sep, term): (u8, u8) = if opts.nul { (0, 0) } else { (b'\t', b'\n') };

    let mut out = Vec::new();
    for d in deltas {
        out.extend_from_slice(&opts.line_prefix);
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
                        hex(&d.dst_id, len),
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
    out
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
