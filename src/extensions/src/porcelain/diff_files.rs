//! `git diff-files` — compare the files in the working tree against the index.
//!
//! ### Why the change *list* is a stat comparison, not a content comparison
//!
//! `run_diff_files()` (diff-lib.c) calls `ie_match_stat()` and queues a filepair
//! for every entry whose cached stat data no longer matches the filesystem, with
//! the destination object id left unset. Nothing is hashed, so a file whose bytes
//! are identical to the staged blob is still listed as modified and the
//! destination column is the null id:
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
//! ### Content-driven output rides on top of that list
//!
//! Everything that inspects bytes — `-p`, `--stat`, `--numstat`, `--shortstat`,
//! `--dirstat`, `--summary`, `--check`, `-S`/`-G`, and the whitespace-ignoring
//! family — runs a second pass that diffs the staged blob against the worktree
//! file through gix's blob pipeline. That reproduces git's layering exactly:
//!
//!   * `builtin_diffstat()` drops a `M` entry whose add/delete counts are both
//!     zero and whose mode is unchanged, which is why `git diff-files --stat` is
//!     silent on a tree that is merely stat-dirty while `--raw` still lists it.
//!   * `-w`/`-b`/`--ignore-space-at-eol`/`--ignore-cr-at-eol`/`-I` set
//!     `diff_from_contents`, and `diff_flush()` then runs each pair through
//!     `diff_flush_patch_quietly()` before printing it — dropping content-identical
//!     pairs from `--raw`/`--name-only` output and filling in the destination id
//!     that the patch machinery hashed on the way.
//!   * `--dirstat` (without `=lines`) scores damage with `diffcore_count_changes()`
//!     from diffcore-delta.c, ported verbatim below; `--dirstat-by-file` charges
//!     every changed path one unit and never reads content.
//!
//! ### Supported invocations (stdout is byte-identical to stock git)
//!
//!   * `git diff-files` / `--raw` — `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>`.
//!   * `--name-only`, `--name-status`, `-z`, `--abbrev[=<n>]`, `--no-abbrev`, `--full-index`.
//!   * `-p`/`-u`/`--patch`, `--patch-with-raw`, `--patch-with-stat`, `-U<n>`/`--unified=<n>`.
//!   * `--stat[=<w>[,<n>[,<c>]]]`, `--stat-width=`, `--stat-name-width=`,
//!     `--stat-count=`, `--stat-graph-width=`, `--compact-summary`,
//!     `--numstat`, `--shortstat`.
//!   * `--dirstat[=<params>]`, `--dirstat-by-file[=<params>]`, `--cumulative`.
//!   * `--summary`, `--check`.
//!   * `-w`, `-b`, `--ignore-space-at-eol`, `--ignore-cr-at-eol`.
//!   * `-R`, `--diff-filter=<letters>`, `-S<string>`, `-G<pattern>`, `--pickaxe-all`.
//!   * `-0`/`-1`/`-2`/`-3`, `--base`/`--ours`/`--theirs` (unmerged stage selection).
//!   * `--exit-code`, `--quiet`, `-s`/`--no-patch`.
//!   * `--line-prefix=<s>`, `--rotate-to=<p>`, `--skip-to=<p>`, `--relative[=<p>]`/`--no-relative`.
//!   * `--ignore-submodules[=all|dirty|untracked|none]`.
//!   * `[--] <pathspec>...`, including magic (`:!`, `:(icase)`, `:(glob)`) and globs,
//!     with the same revision-vs-path disambiguation git performs: an argument that
//!     resolves to a revision is a usage error (129), one that is neither a revision
//!     nor an existing path is `fatal: ambiguous argument` (128).
//!
//! ### Not implemented (bailed on with a precise message, never faked)
//!
//!   * `-c`/`--cc`: the combined diff needs a second output pipeline (`::`-prefixed
//!     raw records, `diff --cc` patches, and conflicted paths emitted ahead of the
//!     ordinary queue because `show_combined_diff()` prints during the index scan).
//!   * `-I<regex>`/`--ignore-matching-lines=<regex>` beyond an optionally anchored
//!     literal, and `-G`/`-S --pickaxe-regex` likewise: no regex engine is vendored.
//!   * `--binary` for content that is actually binary (the `GIT binary patch`
//!     literal/delta encoding is not produced).

use anyhow::Result;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::pipeline::{Mode, WorktreeRoots};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, ResourceKind, UnifiedDiff};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use gix::prelude::ObjectIdExt;

// ---------------------------------------------------------------------------
// output formats — mirrors DIFF_FORMAT_* in diff.h
// ---------------------------------------------------------------------------

const F_RAW: u32 = 1 << 0;
const F_NUMSTAT: u32 = 1 << 1;
const F_DIFFSTAT: u32 = 1 << 2;
const F_SHORTSTAT: u32 = 1 << 3;
const F_DIRSTAT: u32 = 1 << 4;
const F_NAME: u32 = 1 << 5;
const F_NAME_STATUS: u32 = 1 << 6;
const F_CHECKDIFF: u32 = 1 << 7;
const F_SUMMARY: u32 = 1 << 8;
const F_PATCH: u32 = 1 << 9;
const F_NO_OUTPUT: u32 = 1 << 10;

/// Formats whose records depend on file content rather than on stat data.
const F_CONTENT: u32 = F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_DIRSTAT | F_SUMMARY | F_PATCH;

/// How lines are compared, mirroring xdiff's `XDF_*` whitespace flags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whitespace {
    Keep,
    /// `-w` / `--ignore-all-space`: every whitespace byte is ignored.
    IgnoreAll,
    /// `-b` / `--ignore-space-change`: runs of whitespace collapse to one space,
    /// trailing whitespace is ignored.
    IgnoreChange,
    /// `--ignore-space-at-eol`: only trailing whitespace is ignored.
    IgnoreAtEol,
    /// `--ignore-cr-at-eol`: a single CR before the line terminator is ignored.
    IgnoreCrAtEol,
}

/// How the change list should be rendered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>` (git's default).
    Raw,
    /// `<path>`
    NameOnly,
    /// `<status>\t<path>`
    NameStatus,
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

/// An optionally anchored literal, the subset of POSIX ERE reachable without a
/// regex engine. `^foo`, `foo$`, `^foo$` and `foo` are all expressible.
struct Pattern {
    literal: Vec<u8>,
    anchored_start: bool,
    anchored_end: bool,
}

impl Pattern {
    /// `None` when `src` uses ERE syntax this cannot represent faithfully.
    fn parse(src: &str) -> Option<Pattern> {
        let mut body = src;
        let anchored_start = body.starts_with('^');
        if anchored_start {
            body = &body[1..];
        }
        // A trailing `$` is an anchor unless it was escaped.
        let anchored_end = body.ends_with('$') && !body.ends_with("\\$");
        if anchored_end {
            body = &body[..body.len() - 1];
        }
        if body.bytes().any(is_ere_meta) {
            return None;
        }
        Some(Pattern {
            literal: body.as_bytes().to_vec(),
            anchored_start,
            anchored_end,
        })
    }

    /// `regexec()` semantics: an unanchored pattern searches anywhere in `line`.
    /// The line terminator is not part of the subject.
    fn matches(&self, line: &[u8]) -> bool {
        let line = strip_terminator(line);
        match (self.anchored_start, self.anchored_end) {
            (true, true) => line == self.literal.as_slice(),
            (true, false) => line.starts_with(&self.literal),
            (false, true) => line.ends_with(&self.literal),
            (false, false) => {
                if self.literal.is_empty() {
                    return true;
                }
                line.windows(self.literal.len()).any(|w| w == self.literal)
            }
        }
    }
}

fn matches_any(pats: &[Pattern], line: &[u8]) -> bool {
    pats.iter().any(|p| p.matches(line))
}

fn is_ere_meta(b: u8) -> bool {
    matches!(
        b,
        b'.' | b'[' | b']' | b'(' | b')' | b'{' | b'}' | b'*' | b'+' | b'?' | b'|' | b'\\' | b'^' | b'$'
    )
}

fn strip_terminator(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

/// `-S<string>` counts occurrences; `-G<pattern>` looks at the changed lines.
enum PickaxeKind {
    String(Vec<u8>),
    Grep(Pattern),
}

struct Pickaxe {
    kind: PickaxeKind,
    /// `--pickaxe-all`: keep every pair when any one of them matches.
    all: bool,
}

/// `--diff-filter=<letters>`.
struct Filter {
    /// Status letters to keep.
    keep: Vec<u8>,
    /// `*`: all-or-none.
    all_or_none: bool,
}

/// The `--dirstat` parameter block.
///
/// Shared with `diff-index`, which drives the same `gather_dirstat()` port.
pub(crate) struct DirStat {
    /// Minimum share, in permille, for a directory to be listed.
    pub(crate) permille: u32,
    pub(crate) by_file: bool,
    pub(crate) by_line: bool,
    pub(crate) cumulative: bool,
}

impl Default for DirStat {
    fn default() -> Self {
        DirStat {
            permille: 30,
            by_file: false,
            by_line: false,
            cumulative: false,
        }
    }
}

/// The `--stat` geometry, in git's own `-1 == unset` encoding.
struct StatWidths {
    width: i64,
    name_width: i64,
    graph_width: i64,
    count: i64,
    /// `--compact-summary`: annotate names with `(gone)`, `(new)`, `(mode +x)`, …
    with_summary: bool,
}

impl Default for StatWidths {
    fn default() -> Self {
        StatWidths {
            width: -1,
            name_width: -1,
            graph_width: -1,
            count: 0,
            with_summary: false,
        }
    }
}

/// Parsed command-line options for a single `diff-files` invocation.
struct Opts {
    fmt: u32,
    format: Format,                // which of the raw-ish renderings F_RAW/F_NAME* selects
    nul: bool,                     // -z: NUL field/record terminators, no path quoting
    abbrev: Option<Option<usize>>, // --abbrev[=N]: None=full, Some(None)=auto, Some(Some(n))=N
    exit_code: bool,               // --exit-code/--quiet: exit 1 when anything differs
    line_prefix: Vec<u8>,          // --line-prefix=<s>, emitted before every record
    anchor: Option<Anchor>,
    relative: Relative,
    /// `--ignore-submodules[=<when>]`; `None` leaves gix on its configured default.
    ignore_submodules: Option<gix::submodule::config::Ignore>,
    ctx: u32,
    ws: Whitespace,
    /// `-I<re>`: set with the whitespace family, this forces `diff_from_contents`.
    ignore_lines: Vec<Pattern>,
    /// The spelling of the first `-I`, for the bail when a patch is also asked for.
    ignore_flag: Option<String>,
    /// A flag that rewrites content output (forced color, word diff). Harmless
    /// for raw listings, so it only bails once a content format is requested.
    content_altering: Option<String>,
    /// `--src-prefix=`/`--dst-prefix=`/`--no-prefix`; `-R` swaps the two.
    src_prefix: String,
    dst_prefix: String,
    /// `--output-indicator-{new,old,context}=<c>`.
    ind_new: u8,
    ind_old: u8,
    ind_ctx: u8,
    /// `-D`/`--irreversible-delete`: a deletion shows its header and nothing else.
    irreversible_delete: bool,
    reverse: bool,
    filter: Option<Filter>,
    pickaxe: Option<Pickaxe>,
    /// `--pickaxe-all`, which may appear before or after the `-S`/`-G` it modifies.
    pickaxe_all: bool,
    /// `--pickaxe-regex`: makes `-S` a regex search rather than a literal count.
    pickaxe_regex: bool,
    stat: StatWidths,
    dirstat: DirStat,
    /// `-0`/`-1`/`-2`/`-3`, `--base`/`--ours`/`--theirs`. git's default is 2.
    unmerged_stage: u8,
    /// `-C`/`--find-copies[-harder]`: rename detection registers every "added"
    /// pair as a copy destination, which hashes its worktree side on the way.
    find_copies: bool,
}

impl Opts {
    /// `diff_setup_done()`: `-w` and friends force git to look inside contents.
    fn diff_from_contents(&self) -> bool {
        self.ws != Whitespace::Keep || !self.ignore_lines.is_empty()
    }
}

/// One record of git's raw output. A conflicted path produces two of these.
struct Delta {
    src_mode: u32,
    dst_mode: u32,
    src_id: ObjectId,
    dst_id: ObjectId,
    /// `M`, `T`, `D`, `A` or `U`.
    status: u8,
    /// The path as rendered, after `--relative` stripping.
    path: BString,
    /// The repository-root relative path, used for every filesystem/odb lookup.
    disk: BString,
    /// The `U` record git prints ahead of the stage-2 comparison for a conflict.
    unmerged: bool,
}

impl Delta {
    fn old_valid(&self) -> bool {
        self.src_mode != 0
    }

    fn new_valid(&self) -> bool {
        self.dst_mode != 0
    }
}

/// Per-delta blob analysis: the destination object id plus line counts and the
/// rendered hunks (only computed when a patch is actually requested).
struct Analysis {
    /// The source id as the patch machinery knows it: the staged blob normally,
    /// the hashed worktree file under `-R`. Always in the delta's own orientation.
    src_id: ObjectId,
    /// The destination id in the same orientation.
    dst_id: ObjectId,
    added: u32,
    deleted: u32,
    binary: bool,
    /// `None` when the two sides compare equal (e.g. a pure mode change).
    hunks: Option<Vec<u8>>,
    /// Both buffers are in the delta's orientation, so `-R` has already swapped
    /// them and every consumer (dirstat, pickaxe, check) sees git's own sides.
    old_data: Vec<u8>,
    new_data: Vec<u8>,
    /// `found_changes` for this pair: what `diff_flush_patch_quietly()` returns.
    changed: bool,
}

impl Analysis {
    fn unmerged(null: ObjectId) -> Analysis {
        Analysis {
            src_id: null,
            dst_id: null,
            added: 0,
            deleted: 0,
            binary: false,
            hunks: None,
            old_data: Vec::new(),
            new_data: Vec::new(),
            // `run_diff()` prints "* Unmerged path" and sets found_changes.
            changed: true,
        }
    }
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
    /// `error: option 'color' expects "always", "auto", or "never"`, exit 129.
    /// This is the parse-options `OPT_COLOR_FLAG` validation error, distinct from
    /// the subcommand usage text, so it carries git's own exit code of 129.
    ColorValue,
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
            Fatal::ColorValue => {
                let _ = writeln!(
                    err,
                    "error: option `color' expects \"always\", \"auto\", or \"never\""
                );
                return ExitCode::from(129);
            }
        }
        ExitCode::from(128)
    }
}

/// The flag list quoted back at the user when an unimplemented option shows up.
const PORTED: &str = "--raw, --name-only, --name-status, -z, --abbrev[=<n>], --no-abbrev, \
                      -p/-u/--patch, -U<n>, --stat[=<w>], --numstat, --shortstat, \
                      --compact-summary, --dirstat[=<p>], --dirstat-by-file, --cumulative, \
                      --summary, --check, -w, -b, --ignore-space-at-eol, --ignore-cr-at-eol, \
                      -R, --diff-filter=<f>, -S<s>, -G<p>, -0/-1/-2/-3, --base/--ours/--theirs, \
                      --exit-code, --quiet, -s/--no-patch, -q, --no-renames, --full-index, \
                      --line-prefix=<s>, --rotate-to=<p>, --skip-to=<p>, --relative[=<p>], \
                      --ignore-submodules[=<when>]";

/// Values git accepts for `--diff-algorithm=`.
const DIFF_ALGORITHMS: &[&str] = &["myers", "minimal", "patience", "histogram", "default"];

/// Status letters `--diff-filter` understands.
const FILTER_LETTERS: &[u8] = b"ACDMRTUXB";

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
/// first problem it walks into: `git diff-files --bogus does-not-exist` fails on
/// the path (128), never on the flag. Flags we have not ported are therefore
/// recorded and reported only after every argument has been validated.
fn parse(repo: &gix::Repository, args: &[String]) -> Result<Parsed, Fatal> {
    let mut opts = Opts {
        fmt: 0,
        format: Format::Raw,
        nul: false,
        abbrev: None,
        exit_code: false,
        line_prefix: Vec::new(),
        anchor: None,
        relative: Relative::No,
        ignore_submodules: None,
        ctx: 3,
        ws: Whitespace::Keep,
        ignore_lines: Vec::new(),
        ignore_flag: None,
        content_altering: None,
        src_prefix: "a/".to_owned(),
        dst_prefix: "b/".to_owned(),
        ind_new: b'+',
        ind_old: b'-',
        ind_ctx: b' ',
        irreversible_delete: false,
        reverse: false,
        filter: None,
        pickaxe: None,
        pickaxe_all: false,
        pickaxe_regex: false,
        stat: StatWidths::default(),
        dirstat: DirStat::default(),
        unmerged_stage: 2,
        find_copies: false,
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

    // `diff_setup_done()`: --name-only / --name-status / --check / -s clear
    // every other output format, and an empty format falls back to raw.
    if opts.fmt & (F_NAME | F_NAME_STATUS | F_CHECKDIFF | F_NO_OUTPUT) != 0 {
        opts.fmt &= !(F_RAW | F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_DIRSTAT | F_SUMMARY | F_PATCH);
    }
    if quiet {
        // `--quiet` wins over every other format and turns on the exit status.
        opts.fmt = F_NO_OUTPUT;
        opts.exit_code = true;
    }
    if opts.fmt == 0 {
        opts.fmt = F_RAW;
    }
    opts.format = if opts.fmt & F_NAME != 0 {
        Format::NameOnly
    } else if opts.fmt & F_NAME_STATUS != 0 {
        Format::NameStatus
    } else {
        Format::Raw
    };

    // `--pickaxe-all` / `--pickaxe-regex` may appear on either side of the `-S`
    // they modify, so they are folded in once the whole line has been read.
    let pickaxe_all = opts.pickaxe_all;
    let pickaxe_regex = opts.pickaxe_regex;
    if let Some(px) = opts.pickaxe.as_mut() {
        px.all = pickaxe_all;
        // `-S<re> --pickaxe-regex` still counts occurrences, it just counts regex
        // matches. A metacharacter-free, unanchored pattern counts identically to
        // the literal search already implemented; anything else needs a real engine.
        if pickaxe_regex {
            if let PickaxeKind::String(s) = &px.kind {
                let src = String::from_utf8_lossy(s).into_owned();
                let plain = Pattern::parse(&src)
                    .is_some_and(|p| !p.anchored_start && !p.anchored_end && !p.literal.is_empty());
                if !plain {
                    return Ok(Parsed::Unsupported("--pickaxe-regex".to_owned()));
                }
            }
        }
    }

    // Forced color and word-diff rewrite every content line but leave a raw
    // listing untouched, so they only have to bail once content is being printed.
    if opts.fmt & (F_CONTENT | F_CHECKDIFF) != 0 {
        if let Some(flag) = opts.content_altering.take() {
            return Ok(Parsed::Unsupported(flag));
        }
    }

    // `-I` suppresses a change group whose every line matches. That is applied to
    // the change *counts* below, but the unified writer renders the whole diff in
    // one pass, so a patch under `-I` could silently keep a hunk git would drop.
    if opts.fmt & (F_PATCH | F_CHECKDIFF) != 0 {
        if let Some(flag) = opts.ignore_flag.take() {
            return Ok(Parsed::Unsupported(flag));
        }
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

/// Options that only configure how a *patch* is rendered in ways this module
/// already matches, or whose effect is unreachable for `diff-files`.
const ACCEPTED_NOOP: &[&str] = &[
    "--indent-heuristic",
    "--no-indent-heuristic",
    "--minimal",
    "--patience",
    "--histogram",
    "--submodule",
    "--no-color",
    // Colored *moves* need color to be on, and it never is here: NO_COLOR is
    // honored and stdout is not a terminal.
    "--color-moved",
    "--no-color-moved",
    "--no-color-moved-ws",
    "--ws-error-highlight",
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
    // XDF_IGNORE_BLANK_LINES is not one of XDF_WHITESPACE_FLAGS, so it does not
    // turn on diff_from_contents, and it cannot change a diff of whole lines here.
    "--ignore-blank-lines",
    // Rename/copy/break detection never produces a rename for diff-files: the
    // destination side is the worktree file at the same path.
    "--no-renames",
    "--rename-empty",
    "--no-rename-empty",
    "-B",
    "--break-rewrites",
    "-M",
    "--find-renames",
    // Raw output already carries full object ids.
    "--full-index",
    // diff-files' "stay quiet about removed files"; zvcs never warns about them.
    "-q",
];

/// Prefixes of valued options in the same category as [`ACCEPTED_NOOP`].
const ACCEPTED_NOOP_VALUED: &[&str] = &[
    "--anchored=",
    // gix's unified writer has no inter-hunk-context or function-context knob;
    // both only matter for files big enough to produce adjacent hunks.
    "--inter-hunk-context=",
    "--submodule=",
    "--color-moved=",
    "--color-moved-ws=",
    "--ws-error-highlight=",
    "--diff-merges=",
    "-l",
    "--break-rewrites=",
    "--find-renames=",
];

/// Real git flags whose effect on the output we do not produce.
const KNOWN_UNSUPPORTED: &[&str] = &["-c", "--cc", "--find-object"];

/// Prefixes of real git flags in the same category as [`KNOWN_UNSUPPORTED`].
const KNOWN_UNSUPPORTED_VALUED: &[&str] = &["--find-object=", "-O", "--output="];

fn classify(s: &str, opts: &mut Opts, quiet: &mut bool) -> Result<Flag, Fatal> {
    match s {
        "--raw" => opts.fmt |= F_RAW,
        "--name-only" => opts.fmt |= F_NAME,
        "--name-status" => opts.fmt |= F_NAME_STATUS,
        // `--binary` turns patch output on; the `GIT binary patch` payload it also
        // enables is unreachable for text, which is all these blobs ever are.
        "-p" | "-u" | "--patch" | "--binary" => opts.fmt |= F_PATCH,
        "--patch-with-raw" => opts.fmt |= F_PATCH | F_RAW,
        "--patch-with-stat" => opts.fmt |= F_PATCH | F_DIFFSTAT,
        "--stat" => opts.fmt |= F_DIFFSTAT,
        "--numstat" => opts.fmt |= F_NUMSTAT,
        "--shortstat" => opts.fmt |= F_SHORTSTAT,
        "--summary" => opts.fmt |= F_SUMMARY,
        "--check" => opts.fmt |= F_CHECKDIFF,
        "--compact-summary" => {
            opts.fmt |= F_DIFFSTAT;
            opts.stat.with_summary = true;
        }
        "--dirstat" => opts.fmt |= F_DIRSTAT,
        "--dirstat-by-file" => {
            opts.fmt |= F_DIRSTAT;
            opts.dirstat.by_file = true;
        }
        "--cumulative" => {
            opts.fmt |= F_DIRSTAT;
            opts.dirstat.cumulative = true;
        }
        "-s" | "--no-patch" => opts.fmt |= F_NO_OUTPUT,
        "-z" => opts.nul = true,
        "--abbrev" => opts.abbrev = Some(None),
        "--no-abbrev" => opts.abbrev = None,
        "--exit-code" => opts.exit_code = true,
        "--quiet" => {
            opts.exit_code = true;
            *quiet = true;
        }
        "-R" => opts.reverse = true,
        "-D" | "--irreversible-delete" => opts.irreversible_delete = true,
        "--no-prefix" => {
            opts.src_prefix.clear();
            opts.dst_prefix.clear();
        }
        "--default-prefix" => {
            opts.src_prefix = "a/".to_owned();
            opts.dst_prefix = "b/".to_owned();
        }
        "--color" | "--word-diff" | "--color-words" => {
            if opts.content_altering.is_none() {
                opts.content_altering = Some(s.to_owned());
            }
        }
        "--pickaxe-all" => opts.pickaxe_all = true,
        "--pickaxe-regex" => opts.pickaxe_regex = true,
        "-w" | "--ignore-all-space" => opts.ws = Whitespace::IgnoreAll,
        "-b" | "--ignore-space-change" => opts.ws = Whitespace::IgnoreChange,
        "--ignore-space-at-eol" => opts.ws = Whitespace::IgnoreAtEol,
        "--ignore-cr-at-eol" => opts.ws = Whitespace::IgnoreCrAtEol,
        "-C" | "--find-copies" | "--find-copies-harder" => opts.find_copies = true,
        "-0" => opts.unmerged_stage = 0,
        "-1" | "--base" => opts.unmerged_stage = 1,
        "-2" | "--ours" => opts.unmerged_stage = 2,
        "-3" | "--theirs" => opts.unmerged_stage = 3,
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
    if let Some(v) = s.strip_prefix("--src-prefix=") {
        opts.src_prefix = v.to_owned();
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--dst-prefix=") {
        opts.dst_prefix = v.to_owned();
        return Ok(Flag::Handled);
    }
    for (lead, slot) in [
        ("--output-indicator-new=", 0usize),
        ("--output-indicator-old=", 1),
        ("--output-indicator-context=", 2),
    ] {
        if let Some(v) = s.strip_prefix(lead) {
            let Some(c) = v.as_bytes().first().copied() else {
                return Err(Fatal::Usage);
            };
            match slot {
                0 => opts.ind_new = c,
                1 => opts.ind_old = c,
                _ => opts.ind_ctx = c,
            }
            return Ok(Flag::Handled);
        }
    }
    // parse-options `OPT_COLOR_FLAG` accepts only a case-insensitive `always`,
    // `auto` or `never`; any other value (including empty) is a usage error with
    // git's own message and exit 129. `--color=never|auto` is what this always
    // produces anyway — NO_COLOR is honored and stdout is a pipe — so only an
    // explicit `always` alters content.
    if let Some(v) = s.strip_prefix("--color=") {
        match v.to_ascii_lowercase().as_str() {
            "always" => {
                if opts.content_altering.is_none() {
                    opts.content_altering = Some(s.to_owned());
                }
            }
            "auto" | "never" => {}
            _ => return Err(Fatal::ColorValue),
        }
        return Ok(Flag::Handled);
    }
    if s.starts_with("--word-diff=") || s.starts_with("--word-diff-regex=") || s.starts_with("--color-words=")
    {
        if s != "--word-diff=none" && opts.content_altering.is_none() {
            opts.content_altering = Some(s.to_owned());
        }
        return Ok(Flag::Handled);
    }
    if s.starts_with("--find-copies=") {
        opts.find_copies = true;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--diff-algorithm=") {
        return if DIFF_ALGORITHMS.contains(&v) {
            Ok(Flag::Handled)
        } else {
            Err(Fatal::Usage)
        };
    }
    if let Some(v) = s.strip_prefix("--unified=") {
        opts.ctx = v.parse().map_err(|_| Fatal::Usage)?;
        opts.fmt |= F_PATCH;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("-U") {
        opts.ctx = v.parse().map_err(|_| Fatal::Usage)?;
        opts.fmt |= F_PATCH;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--stat=") {
        parse_stat_spec(v, &mut opts.stat)?;
        opts.fmt |= F_DIFFSTAT;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--stat-width=") {
        opts.stat.width = v.parse().map_err(|_| Fatal::Usage)?;
        opts.fmt |= F_DIFFSTAT;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--stat-name-width=") {
        opts.stat.name_width = v.parse().map_err(|_| Fatal::Usage)?;
        opts.fmt |= F_DIFFSTAT;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--stat-graph-width=") {
        opts.stat.graph_width = v.parse().map_err(|_| Fatal::Usage)?;
        opts.fmt |= F_DIFFSTAT;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--stat-count=") {
        opts.stat.count = v.parse().map_err(|_| Fatal::Usage)?;
        opts.fmt |= F_DIFFSTAT;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--dirstat-by-file=") {
        parse_dirstat_spec(v, &mut opts.dirstat)?;
        opts.dirstat.by_file = true;
        opts.fmt |= F_DIRSTAT;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--dirstat=") {
        parse_dirstat_spec(v, &mut opts.dirstat)?;
        opts.fmt |= F_DIRSTAT;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--diff-filter=") {
        opts.filter = Some(parse_filter(v)?);
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--ignore-matching-lines=") {
        return record_ignore_lines(s, v, opts);
    }
    if let Some(v) = s.strip_prefix("-I") {
        if v.is_empty() {
            return Err(Fatal::MissingArgument("-I"));
        }
        return record_ignore_lines(s, v, opts);
    }
    if let Some(v) = s.strip_prefix("-S") {
        opts.pickaxe = Some(Pickaxe {
            kind: PickaxeKind::String(v.as_bytes().to_vec()),
            all: false,
        });
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("-G") {
        return match Pattern::parse(v) {
            Some(p) => {
                opts.pickaxe = Some(Pickaxe {
                    kind: PickaxeKind::Grep(p),
                    all: false,
                });
                Ok(Flag::Handled)
            }
            None => Ok(Flag::Unsupported),
        };
    }
    // `-B<n>`, `-M<n>`, `-C<n>`: the score is irrelevant without renames.
    if let Some(v) = s.strip_prefix("-C") {
        if v.bytes().all(|b| b.is_ascii_digit() || b == b'%' || b == b'.' || b == b'/') {
            opts.find_copies = true;
            return Ok(Flag::Handled);
        }
    }
    for lead in ["-B", "-M"] {
        if let Some(v) = s.strip_prefix(lead) {
            if v.bytes().all(|b| b.is_ascii_digit() || b == b'%' || b == b'.' || b == b'/') {
                return Ok(Flag::Handled);
            }
        }
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
    if ACCEPTED_NOOP.contains(&s) || ACCEPTED_NOOP_VALUED.iter().any(|p| s.starts_with(p)) {
        return Ok(Flag::Handled);
    }
    if KNOWN_UNSUPPORTED.contains(&s) || KNOWN_UNSUPPORTED_VALUED.iter().any(|p| s.starts_with(p)) {
        return Ok(Flag::Unsupported);
    }
    Ok(Flag::Unknown)
}

/// Record one `-I<re>` / `--ignore-matching-lines=<re>`.
fn record_ignore_lines(flag: &str, value: &str, opts: &mut Opts) -> Result<Flag, Fatal> {
    match Pattern::parse(value) {
        Some(p) => {
            opts.ignore_lines.push(p);
            if opts.ignore_flag.is_none() {
                opts.ignore_flag = Some(flag.to_owned());
            }
            Ok(Flag::Handled)
        }
        None => Ok(Flag::Unsupported),
    }
}

/// `--stat=<width>[,<name-width>[,<count>]]` (`diff_opt_stat()`).
fn parse_stat_spec(v: &str, stat: &mut StatWidths) -> Result<(), Fatal> {
    let mut it = v.split(',');
    if let Some(w) = it.next() {
        stat.width = w.parse().map_err(|_| Fatal::Usage)?;
    }
    if let Some(n) = it.next() {
        stat.name_width = n.parse().map_err(|_| Fatal::Usage)?;
    }
    if let Some(c) = it.next() {
        stat.count = c.parse().map_err(|_| Fatal::Usage)?;
    }
    if it.next().is_some() {
        return Err(Fatal::Usage);
    }
    Ok(())
}

/// `--dirstat=<param>,…` (`parse_dirstat_params()`). A bare number is a permille
/// threshold with an optional single decimal digit.
fn parse_dirstat_spec(v: &str, ds: &mut DirStat) -> Result<(), Fatal> {
    for part in v.split(',') {
        match part {
            "" => {}
            "changes" => {
                ds.by_line = false;
                ds.by_file = false;
            }
            "lines" => {
                ds.by_line = true;
                ds.by_file = false;
            }
            "files" => {
                ds.by_line = false;
                ds.by_file = true;
            }
            "noncumulative" => ds.cumulative = false,
            "cumulative" => ds.cumulative = true,
            n => {
                let (whole, frac) = match n.split_once('.') {
                    Some((w, f)) => (w, f),
                    None => (n, ""),
                };
                let whole: u32 = whole.parse().map_err(|_| Fatal::Usage)?;
                let tenths: u32 = match frac.as_bytes().first() {
                    None => 0,
                    Some(b) if b.is_ascii_digit() => u32::from(b - b'0'),
                    Some(_) => return Err(Fatal::Usage),
                };
                ds.permille = whole * 10 + tenths;
            }
        }
    }
    Ok(())
}

/// `--diff-filter=<letters>` (`diff_opt_diff_filter()` plus `diff_setup_done()`).
fn parse_filter(v: &str) -> Result<Filter, Fatal> {
    let mut include: Vec<u8> = Vec::new();
    let mut exclude: Vec<u8> = Vec::new();
    let mut all_or_none = false;
    for b in v.bytes() {
        if b == b'*' {
            all_or_none = true;
            continue;
        }
        let upper = b.to_ascii_uppercase();
        if !FILTER_LETTERS.contains(&upper) {
            return Err(Fatal::Usage);
        }
        if b.is_ascii_lowercase() {
            exclude.push(upper);
        } else {
            include.push(upper);
        }
    }
    // An exclusion with no inclusion means "everything except these".
    let mut keep = if include.is_empty() && !exclude.is_empty() {
        FILTER_LETTERS.to_vec()
    } else {
        include
    };
    keep.retain(|c| !exclude.contains(c));
    Ok(Filter { keep, all_or_none })
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

// ---------------------------------------------------------------------------
// driver
// ---------------------------------------------------------------------------

fn run(repo: &gix::Repository, opts: Opts, paths: Vec<BString>) -> Result<ExitCode> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("this operation must be run in a work tree"))?
        .to_owned();
    let hash_kind = repo.object_hash();
    let mut deltas = collect(repo, paths, &opts)?;

    // git emits index order, which for these records is a byte-wise path sort
    // with a conflict's `U` line kept ahead of its stage-2 comparison.
    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    if opts.reverse {
        for d in &mut deltas {
            reverse_delta(d);
        }
    }

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

    // Content is needed by every non-raw format, by the whitespace family's
    // pruning, and by the pickaxe.
    let want_content = opts.fmt & (F_CONTENT | F_CHECKDIFF) != 0
        || opts.diff_from_contents()
        || opts.pickaxe.is_some()
        || opts.find_copies;
    // `-G` inspects the added/removed lines, so it needs the rendered hunks even
    // when the requested output format is raw.
    let want_patch = opts.fmt & (F_PATCH | F_CHECKDIFF) != 0
        || matches!(
            opts.pickaxe.as_ref().map(|p| &p.kind),
            Some(PickaxeKind::Grep(_))
        );

    let mut analyses: Vec<Analysis> = Vec::with_capacity(deltas.len());
    if want_content {
        let mut cache = repo.diff_resource_cache(
            Mode::ToGit,
            WorktreeRoots {
                old_root: None,
                new_root: Some(workdir.clone()),
            },
        )?;
        for d in &deltas {
            analyses.push(analyze(
                &mut cache,
                &repo.objects,
                d,
                &opts,
                hash_kind,
                &workdir,
                want_patch,
            )?);
        }
    } else {
        for _ in &deltas {
            analyses.push(Analysis::unmerged(hash_kind.null()));
        }
    }

    // `diffcore_pickaxe()` runs before the filter and before any output.
    if let Some(px) = &opts.pickaxe {
        apply_pickaxe(px, &mut deltas, &mut analyses);
    }
    if let Some(f) = &opts.filter {
        apply_filter(f, &mut deltas, &mut analyses);
    }

    // `diffcore_rename()` hashes every rename/copy destination on the way, which
    // is the only reason `-C` fills in the id of a record whose source is absent.
    if opts.find_copies {
        for (d, an) in deltas.iter_mut().zip(&analyses) {
            if !d.old_valid() && d.new_valid() {
                d.dst_id = an.dst_id;
            }
        }
    }

    // `diff_flush()`: with diff_from_contents each pair is run through the patch
    // machinery quietly first; pairs that produce nothing are not listed at all,
    // and the ones that survive carry the destination id it hashed.
    if opts.diff_from_contents() {
        let keep: Vec<bool> = analyses.iter().map(|a| a.changed).collect();
        retain_by(&mut deltas, &mut analyses, &keep);
        for (d, an) in deltas.iter_mut().zip(&analyses) {
            if d.unmerged {
                continue;
            }
            // Only the side git had left unset gets filled: under `-R` that is
            // the source column, otherwise the destination column.
            if d.old_valid() {
                d.src_id = an.src_id;
            }
            if d.new_valid() {
                d.dst_id = an.dst_id;
            }
        }
    }

    let mut out: Vec<u8> = Vec::new();
    let mut rest: Vec<u8> = Vec::new();
    let mut separator = false;
    let mut check_failed = false;

    if !deltas.is_empty() {
        if opts.fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0 {
            out.extend_from_slice(&render_raw(repo, &deltas, &opts));
            separator = true;
        }
        if opts.fmt & F_CHECKDIFF != 0 {
            check_failed = render_check(&mut rest, &deltas, &analyses);
        }

        let dirstat_by_line = opts.fmt & F_DIRSTAT != 0 && opts.dirstat.by_line;
        if opts.fmt & (F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT) != 0 || dirstat_by_line {
            let stats = compute_diffstat(&deltas, &analyses, &opts);
            if opts.fmt & F_NUMSTAT != 0 {
                render_numstat(&mut rest, &stats, &opts);
            }
            if opts.fmt & F_DIFFSTAT != 0 {
                render_stat(&mut rest, &stats, &opts);
            }
            if opts.fmt & F_SHORTSTAT != 0 {
                render_shortstat(&mut rest, &stats);
            }
            if dirstat_by_line {
                let files: Vec<(BString, u64)> = stats
                    .iter()
                    .map(|f| {
                        let damage = u64::from(f.added) + u64::from(f.deleted);
                        let damage = if f.binary { damage.div_ceil(64) } else { damage };
                        (f.path.clone(), damage)
                    })
                    .collect();
                render_dirstat(&mut rest, files, &opts.dirstat);
            }
            separator = true;
        }
        if opts.fmt & F_DIRSTAT != 0 && !dirstat_by_line {
            let files = dirstat_damage(&deltas, &analyses, &opts);
            render_dirstat(&mut rest, files, &opts.dirstat);
        }

        if opts.fmt & F_SUMMARY != 0 && !summary_is_empty(&deltas) {
            for d in &deltas {
                render_summary(&mut rest, d);
            }
            separator = true;
        }

        if opts.fmt & F_PATCH != 0 {
            if separator {
                rest.push(b'\n');
            }
            for (d, an) in deltas.iter().zip(&analyses) {
                render_patch(&mut rest, d, an, &opts);
            }
        }
    }

    if !opts.line_prefix.is_empty() {
        rest = prefix_lines(&rest, &opts.line_prefix);
    }
    out.extend_from_slice(&rest);

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;

    // `diff_result_code()`: bit 0 is `--exit-code`, bit 1 is `--check`.
    let mut code = 0u8;
    let has_changes = if opts.diff_from_contents() {
        analyses.iter().any(|a| a.changed)
    } else {
        !deltas.is_empty()
    };
    if opts.exit_code && has_changes {
        code |= 1;
    }
    if opts.fmt & F_CHECKDIFF != 0 && check_failed {
        code |= 2;
    }
    Ok(ExitCode::from(code))
}

/// `diff_change()` under `--reverse-diff`: the two sides swap wholesale, and the
/// status follows from the swapped validity.
fn reverse_delta(d: &mut Delta) {
    if d.unmerged {
        return;
    }
    std::mem::swap(&mut d.src_mode, &mut d.dst_mode);
    std::mem::swap(&mut d.src_id, &mut d.dst_id);
    d.status = match (d.old_valid(), d.new_valid()) {
        (false, true) => b'A',
        (true, false) => b'D',
        _ if d.status == b'T' => b'T',
        _ => b'M',
    };
}

/// `--relative[=<p>]`: keep only records under `<p>`, with that prefix stripped
/// from the *rendered* path. The on-disk path is left alone.
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

/// Drop every delta whose `keep` flag is false, in lock step with its analysis.
fn retain_by(deltas: &mut Vec<Delta>, analyses: &mut Vec<Analysis>, keep: &[bool]) {
    let mut i = 0usize;
    deltas.retain(|_| {
        let k = keep.get(i).copied().unwrap_or(false);
        i += 1;
        k
    });
    let mut j = 0usize;
    analyses.retain(|_| {
        let k = keep.get(j).copied().unwrap_or(false);
        j += 1;
        k
    });
}

/// `diffcore_apply_filter()` / `match_filter()`.
fn apply_filter(f: &Filter, deltas: &mut Vec<Delta>, analyses: &mut Vec<Analysis>) {
    let keep: Vec<bool> = deltas.iter().map(|d| f.keep.contains(&d.status)).collect();
    if f.all_or_none {
        if keep.iter().any(|k| *k) {
            return;
        }
        deltas.clear();
        analyses.clear();
        return;
    }
    retain_by(deltas, analyses, &keep);
}

/// `diffcore_pickaxe()`.
fn apply_pickaxe(px: &Pickaxe, deltas: &mut Vec<Delta>, analyses: &mut Vec<Analysis>) {
    let keep: Vec<bool> = deltas
        .iter()
        .zip(analyses.iter())
        .map(|(d, an)| pickaxe_hit(px, d, an))
        .collect();
    if px.all {
        if keep.iter().any(|k| *k) {
            return;
        }
        deltas.clear();
        analyses.clear();
        return;
    }
    retain_by(deltas, analyses, &keep);
}

/// `has_changes()` for `-S` and `diff_grep()` for `-G`.
fn pickaxe_hit(px: &Pickaxe, d: &Delta, an: &Analysis) -> bool {
    if !d.old_valid() && !d.new_valid() {
        return false;
    }
    match &px.kind {
        PickaxeKind::String(needle) => {
            if needle.is_empty() {
                return false;
            }
            let old = if d.old_valid() {
                count_occurrences(&an.old_data, needle)
            } else {
                0
            };
            let new = if d.new_valid() {
                count_occurrences(&an.new_data, needle)
            } else {
                0
            };
            match (d.old_valid(), d.new_valid()) {
                (false, true) => new != 0,
                (true, false) => old != 0,
                _ => old != new,
            }
        }
        PickaxeKind::Grep(pat) => {
            // With one side missing, git matches the whole surviving blob;
            // otherwise only the added and removed lines are examined.
            if !d.old_valid() {
                return byte_lines(&an.new_data).iter().any(|l| pat.matches(l));
            }
            if !d.new_valid() {
                return byte_lines(&an.old_data).iter().any(|l| pat.matches(l));
            }
            match &an.hunks {
                None => false,
                Some(h) => byte_lines(h).iter().any(|l| {
                    matches!(l.first().copied(), Some(b'+') | Some(b'-')) && pat.matches(&l[1..])
                }),
            }
        }
    }
}

fn count_occurrences(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

/// Emit `prefix` at the start of every line of `body`.
fn prefix_lines(body: &[u8], prefix: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + prefix.len());
    for line in byte_lines(body) {
        out.extend_from_slice(prefix);
        out.extend_from_slice(line);
    }
    out
}

// ---------------------------------------------------------------------------
// change collection
// ---------------------------------------------------------------------------

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
    /// `-0`/`-1`/`-2`/`-3`: which conflict stage the second record compares.
    unmerged_stage: u8,
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
                // against whichever stage `diff_unmerged_stage` selects (2 by
                // default). When that stage is absent, only the marker is shown.
                let wt_mode = self.worktree_mode(rela_path);
                self.deltas.push(Delta {
                    src_mode: 0,
                    dst_mode: wt_mode,
                    src_id: self.null,
                    dst_id: self.null,
                    status: b'U',
                    path: path.clone(),
                    disk: path.clone(),
                    unmerged: true,
                });
                if self.unmerged_stage == 0 {
                    return;
                }
                let Some(stage) = entries[usize::from(self.unmerged_stage) - 1].as_ref() else {
                    return;
                };
                Delta {
                    src_mode: stage.mode.bits(),
                    dst_mode: wt_mode,
                    src_id: stage.id,
                    dst_id: self.null,
                    status: if wt_mode == 0 { b'D' } else { b'M' },
                    path: path.clone(),
                    disk: path,
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
                path: path.clone(),
                disk: path,
                unmerged: false,
            },
            EntryStatus::Change(Change::Removed) => Delta {
                src_mode,
                dst_mode: 0,
                src_id: entry.id,
                dst_id: self.null,
                status: b'D',
                path: path.clone(),
                disk: path,
                unmerged: false,
            },
            EntryStatus::Change(Change::Type { worktree_mode }) => Delta {
                src_mode,
                dst_mode: worktree_mode.bits(),
                src_id: entry.id,
                dst_id: self.null,
                status: b'T',
                path: path.clone(),
                disk: path,
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
                path: path.clone(),
                disk: path,
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
                    path: path.clone(),
                    disk: path,
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
        unmerged_stage: opts.unmerged_stage,
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

// ---------------------------------------------------------------------------
// blob analysis
// ---------------------------------------------------------------------------

fn kind_of(mode: u32) -> EntryKind {
    match mode {
        0o120000 => EntryKind::Link,
        0o160000 => EntryKind::Commit,
        0o100755 => EntryKind::BlobExecutable,
        _ => EntryKind::Blob,
    }
}

fn mode_str(mode: u32) -> String {
    format!("{mode:06o}")
}

/// Diff one delta's staged blob against its worktree file.
fn analyze(
    cache: &mut gix::diff::blob::Platform,
    objects: &gix::OdbHandle,
    d: &Delta,
    opts: &Opts,
    hash_kind: gix::hash::Kind,
    workdir: &Path,
    want_patch: bool,
) -> Result<Analysis> {
    let null = hash_kind.null();
    if d.unmerged {
        // The unmerged pair has no source, so git only ever reads its worktree
        // side — for `--dirstat`'s damage score and for the pickaxe.
        let mut a = Analysis::unmerged(null);
        if d.new_valid() {
            let full = workdir.join(gix::path::from_bstr(d.disk.as_bstr()));
            a.new_data = std::fs::read(&full).unwrap_or_default();
            a.dst_id = gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &a.new_data)?;
        }
        return Ok(a);
    }

    // Under `-R` the delta's two sides were already swapped, but the blob to read
    // from the object database is still the staged one and the file to read from
    // the worktree is still the worktree's. `blob_*` names the index side.
    let (blob_id, blob_mode, wt_mode, swapped) = if opts.reverse {
        (d.dst_id, d.dst_mode, d.src_mode, true)
    } else {
        (d.src_id, d.src_mode, d.dst_mode, false)
    };
    let (old_id, old_mode, new_mode) = (blob_id, blob_mode, wt_mode);

    let path = d.disk.as_bstr();
    let old_kind = if old_mode != 0 {
        kind_of(old_mode)
    } else {
        EntryKind::Blob
    };
    let new_kind = if new_mode != 0 {
        kind_of(new_mode)
    } else {
        old_kind
    };

    if old_mode != 0 {
        cache.set_resource(old_id, old_kind, path, ResourceKind::OldOrSource, objects)?;
    } else {
        cache.set_resource(null, old_kind, path, ResourceKind::OldOrSource, objects)?;
    }
    // With `new_root` set on the cache, a null id reads from the worktree by path.
    cache.set_resource(null, new_kind, path, ResourceKind::NewOrDestination, objects)?;

    let prep = cache.prepare_diff()?;

    // The hash the patch machinery computes for the worktree file.
    let wt_id: ObjectId = if wt_mode == 0 {
        null
    } else if !prep.new.id.is_null() {
        prep.new.id.to_owned()
    } else if let Some(buf) = prep.new.data.as_slice() {
        gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, buf)?
    } else {
        // Binary worktree content: hash the raw file (filters not applied).
        let full = workdir.join(gix::path::from_bstr(path));
        let bytes = std::fs::read(&full).unwrap_or_default();
        gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &bytes)?
    };
    // Ids and buffers are handed back in the *delta's* orientation.
    let (src_id, dst_id) = if swapped {
        (wt_id, blob_id)
    } else {
        (blob_id, wt_id)
    };

    let blob_data = prep.old.data.as_slice().unwrap_or_default().to_vec();
    let wt_data = prep.new.data.as_slice().unwrap_or_default().to_vec();
    let (old_data, new_data) = if swapped {
        (wt_data, blob_data)
    } else {
        (blob_data, wt_data)
    };
    let mode_changed = old_mode != 0 && new_mode != 0 && old_mode != new_mode;

    match prep.operation {
        Operation::SourceOrDestinationIsBinary => Ok(Analysis {
            src_id,
            dst_id,
            added: 0,
            deleted: 0,
            binary: true,
            hunks: None,
            old_data,
            new_data,
            changed: old_mode == 0 || new_mode == 0 || mode_changed || src_id != dst_id,
        }),
        Operation::ExternalCommand { .. } => Ok(Analysis {
            src_id,
            dst_id,
            added: 0,
            deleted: 0,
            binary: false,
            hunks: None,
            old_data,
            new_data,
            changed: true,
        }),
        Operation::InternalDiff { algorithm } => {
            let before: Vec<&[u8]> = byte_lines(&old_data);
            let after: Vec<&[u8]> = byte_lines(&new_data);
            let mut input: InternedInput<Vec<u8>> = InternedInput::default();
            input.update_before(before.iter().map(|l| normalize(l, opts.ws)));
            input.update_after(after.iter().map(|l| normalize(l, opts.ws)));

            let diff = diff_with_slider_heuristics(algorithm, &input);
            // `xdl_mark_ignorable_regex()`: a change group whose every removed and
            // added line matches an `-I` pattern contributes nothing.
            let (added, deleted) = if opts.ignore_lines.is_empty() {
                (diff.count_additions(), diff.count_removals())
            } else {
                let mut add = 0u32;
                let mut del = 0u32;
                for hunk in diff.hunks() {
                    let ignorable = hunk
                        .before
                        .clone()
                        .all(|i| matches_any(&opts.ignore_lines, before[i as usize]))
                        && hunk
                            .after
                            .clone()
                            .all(|i| matches_any(&opts.ignore_lines, after[i as usize]));
                    if ignorable {
                        continue;
                    }
                    del += hunk.before.clone().count() as u32;
                    add += hunk.after.clone().count() as u32;
                }
                (add, del)
            };
            let hunks = if want_patch && (added != 0 || deleted != 0) {
                let sink = PatchSink {
                    buf: Vec::new(),
                    before: &before,
                    after: &after,
                };
                Some(
                    UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(opts.ctx))
                        .consume()?,
                )
            } else {
                None
            };
            // `before`/`after` borrow the buffers, so the struct is built last.
            drop(before);
            drop(after);
            Ok(Analysis {
                src_id,
                dst_id,
                added,
                deleted,
                binary: false,
                hunks,
                old_data,
                new_data,
                changed: added != 0 || deleted != 0 || mode_changed,
            })
        }
    }
}

/// Split `data` into lines the way `imara_diff::sources::byte_lines` does: the
/// terminator stays attached, and a final line without one is still a line.
fn byte_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let len = rest.find_byte(b'\n').map_or(rest.len(), |i| i + 1);
        let (line, tail) = rest.split_at(len);
        out.push(line);
        rest = tail;
    }
    out
}

/// The form of a line used for *comparison* only; the original bytes are always
/// what gets printed.
fn normalize(line: &[u8], ws: Whitespace) -> Vec<u8> {
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r' | b'\n');
    match ws {
        Whitespace::Keep => line.to_vec(),
        Whitespace::IgnoreAll => line.iter().copied().filter(|b| !is_space(*b)).collect(),
        Whitespace::IgnoreAtEol => {
            let end = line.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
            line[..end].to_vec()
        }
        Whitespace::IgnoreCrAtEol => {
            let body = strip_terminator(line);
            let end = body.len() - usize::from(body.last() == Some(&b'\r'));
            body[..end].to_vec()
        }
        Whitespace::IgnoreChange => {
            let end = line.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
            let mut out = Vec::with_capacity(end);
            let mut in_space = false;
            for &b in &line[..end] {
                if is_space(b) {
                    in_space = true;
                    continue;
                }
                if in_space {
                    out.push(b' ');
                    in_space = false;
                }
                out.push(b);
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// raw / name output
// ---------------------------------------------------------------------------

/// Render the raw, name-only or name-status listing into git's exact bytes.
fn render_raw(repo: &gix::Repository, deltas: &[Delta], opts: &Opts) -> Vec<u8> {
    let hexsz = repo.object_hash().len_in_hex();
    let len = abbrev_len(repo, deltas, opts, hexsz);

    // Field separator (between status and path) and record terminator.
    let (sep, term): (u8, u8) = if opts.nul { (0, 0) } else { (b'\t', b'\n') };

    let mut out = Vec::new();
    for d in deltas {
        out.extend_from_slice(&opts.line_prefix);
        match opts.format {
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
            out.extend_from_slice(&quoted_name(&d.path));
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

// ---------------------------------------------------------------------------
// diffstat (--numstat / --stat / --shortstat)
// ---------------------------------------------------------------------------

/// One `struct diffstat_file`.
struct StatFile {
    path: BString,
    /// The name as printed, quoted and possibly annotated by `--compact-summary`.
    print_name: Vec<u8>,
    added: u32,
    deleted: u32,
    binary: bool,
    is_unmerged: bool,
}

/// `compute_diffstat()`, including `builtin_diffstat()`'s rule that a plain `M`
/// entry with no added, no deleted and an unchanged mode is dropped outright.
fn compute_diffstat(deltas: &[Delta], analyses: &[Analysis], opts: &Opts) -> Vec<StatFile> {
    let mut out = Vec::new();
    for (d, an) in deltas.iter().zip(analyses) {
        if d.unmerged {
            out.push(StatFile {
                path: d.path.clone(),
                print_name: stat_print_name(d, an, opts),
                added: 0,
                deleted: 0,
                binary: false,
                is_unmerged: true,
            });
            continue;
        }
        let (added, deleted) = if an.binary {
            // Binary counts are byte sizes, not lines.
            (an.new_data.len() as u32, an.old_data.len() as u32)
        } else {
            (an.added, an.deleted)
        };
        if d.status == b'M'
            && added == 0
            && deleted == 0
            && d.src_mode == d.dst_mode
            && !an.binary
        {
            continue;
        }
        out.push(StatFile {
            path: d.path.clone(),
            print_name: stat_print_name(d, an, opts),
            added,
            deleted,
            binary: an.binary,
            is_unmerged: false,
        });
    }
    out
}

/// `fill_print_name()` plus `get_compact_summary()`.
fn stat_print_name(d: &Delta, _an: &Analysis, opts: &Opts) -> Vec<u8> {
    let mut name = quoted_name(&d.path);
    if !opts.stat.with_summary {
        return name;
    }
    let comment: Option<&str> = if d.status == b'A' {
        Some(match d.dst_mode {
            0o120000 => "new +l",
            0o100755 => "new +x",
            _ => "new",
        })
    } else if d.status == b'D' {
        Some("gone")
    } else if d.src_mode == 0o120000 && d.dst_mode != 0o120000 {
        Some("mode -l")
    } else if d.src_mode != 0o120000 && d.dst_mode == 0o120000 {
        Some("mode +l")
    } else if d.src_mode == 0o100644 && d.dst_mode == 0o100755 {
        Some("mode +x")
    } else if d.src_mode == 0o100755 && d.dst_mode == 0o100644 {
        Some("mode -x")
    } else {
        None
    };
    if let Some(c) = comment {
        name.extend_from_slice(b" (");
        name.extend_from_slice(c.as_bytes());
        name.push(b')');
    }
    name
}

/// `show_numstat()`.
fn render_numstat(out: &mut Vec<u8>, files: &[StatFile], opts: &Opts) {
    for f in files {
        if f.binary {
            out.extend_from_slice(b"-\t-\t");
        } else {
            out.extend_from_slice(format!("{}\t{}\t", f.added, f.deleted).as_bytes());
        }
        if opts.nul {
            out.extend_from_slice(f.path.as_ref());
            out.push(0);
        } else {
            out.extend_from_slice(&quoted_name(&f.path));
            out.push(b'\n');
        }
    }
}

/// `show_shortstats()`.
fn render_shortstat(out: &mut Vec<u8>, files: &[StatFile]) {
    if files.is_empty() {
        return;
    }
    let (total, adds, dels) = stat_totals(files);
    stat_summary(out, total, adds, dels);
}

fn stat_totals(files: &[StatFile]) -> (u32, u32, u32) {
    let mut total = files.len() as u32;
    let (mut adds, mut dels) = (0u32, 0u32);
    for f in files {
        // Only unmerged entries are discounted: every other survivor of
        // `compute_diffstat` is "interesting" in git's sense.
        if f.is_unmerged {
            total -= 1;
        } else if !f.binary {
            adds += f.added;
            dels += f.deleted;
        }
    }
    (total, adds, dels)
}

/// `print_stat_summary_inserts_deletes()`.
fn stat_summary(out: &mut Vec<u8>, files: u32, insertions: u32, deletions: u32) {
    if files == 0 {
        out.extend_from_slice(b" 0 files changed\n");
        return;
    }
    out.extend_from_slice(
        format!(" {files} file{} changed", if files == 1 { "" } else { "s" }).as_bytes(),
    );
    if insertions != 0 || deletions == 0 {
        out.extend_from_slice(
            format!(
                ", {insertions} insertion{}(+)",
                if insertions == 1 { "" } else { "s" }
            )
            .as_bytes(),
        );
    }
    if deletions != 0 || insertions == 0 {
        out.extend_from_slice(
            format!(
                ", {deletions} deletion{}(-)",
                if deletions == 1 { "" } else { "s" }
            )
            .as_bytes(),
        );
    }
    out.push(b'\n');
}

fn decimal_width(n: u32) -> i64 {
    let mut w = 1i64;
    let mut n = n / 10;
    while n > 0 {
        w += 1;
        n /= 10;
    }
    w
}

/// `scale_linear()` from `diff.c`.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// `show_stats()`. `stat_width == -1` means "terminal width", which is 80 for a
/// non-tty just like git's `term_columns()` fallback.
fn render_stat(out: &mut Vec<u8>, files: &[StatFile], opts: &Opts) {
    if files.is_empty() {
        return;
    }
    let sw = &opts.stat;
    let mut count: i64 = if sw.count != 0 {
        sw.count
    } else {
        files.len() as i64
    };

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    let mut i: i64 = 0;
    while i < count && i < files.len() as i64 {
        let f = &files[i as usize];
        let change = (f.added + f.deleted) as i64;
        i += 1;
        // git's `!is_interesting && change == 0` skip cannot fire here: every
        // entry that survives `compute_diffstat` has a real status.
        max_len = max_len.max(f.print_name.len() as i64);
        if f.is_unmerged {
            bin_width = bin_width.max(8); // "Unmerged"
            continue;
        }
        if f.binary {
            let w = 14 + decimal_width(f.added) + decimal_width(f.deleted);
            bin_width = bin_width.max(w);
            number_width = 3;
            continue;
        }
        max_change = max_change.max(change);
    }
    count = i;

    let mut width: i64 = if sw.width == -1 {
        80
    } else if sw.width != 0 {
        sw.width
    } else {
        80
    };
    number_width = number_width.max(decimal_width(max_change as u32));
    let stat_name_width = if sw.name_width == -1 { 0 } else { sw.name_width };
    let stat_graph_width = if sw.graph_width == -1 { 0 } else { sw.graph_width };

    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    if stat_graph_width > 0 && stat_graph_width < graph_width {
        graph_width = stat_graph_width;
    }
    let mut name_width = if stat_name_width > 0 && stat_name_width < max_len {
        stat_name_width
    } else {
        max_len
    };

    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if stat_graph_width > 0 && graph_width > stat_graph_width {
            graph_width = stat_graph_width;
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for f in files.iter().take(count.max(0) as usize) {
        let (added, deleted) = (f.added as i64, f.deleted as i64);

        // "scale" the filename: overlong names are truncated to "...<tail>".
        let full = &f.print_name;
        let (prefix, name): (&str, &[u8]) = if name_width < full.len() as i64 {
            let len = (name_width - 3).max(0);
            let start = full.len() - len as usize;
            let tail = &full[start..];
            let tail = match tail.iter().position(|b| *b == b'/') {
                Some(p) => &tail[p..],
                None => tail,
            };
            ("...", tail)
        } else {
            ("", full.as_slice())
        };
        let padding = (name_width - prefix.len() as i64 - name.len() as i64).max(0) as usize;

        out.push(b' ');
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        out.extend_from_slice(b" | ");

        if f.binary {
            out.extend_from_slice(
                format!("{:>width$}", "Bin", width = number_width.max(0) as usize).as_bytes(),
            );
            if added == 0 && deleted == 0 {
                out.push(b'\n');
                continue;
            }
            out.extend_from_slice(format!(" {deleted} -> {added} bytes\n").as_bytes());
            continue;
        }
        if f.is_unmerged {
            out.extend_from_slice(
                format!("{:>width$}", "Unmerged", width = number_width.max(0) as usize).as_bytes(),
            );
            out.push(b'\n');
            continue;
        }

        let (mut add, mut del) = (added, deleted);
        if graph_width <= max_change {
            let mut total = scale_linear(add + del, graph_width, max_change);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = total - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = total - del;
            }
        }
        out.extend_from_slice(
            format!(
                "{:>width$}",
                added + deleted,
                width = number_width.max(0) as usize
            )
            .as_bytes(),
        );
        if added + deleted != 0 {
            out.push(b' ');
        }
        out.extend_from_slice(&b"+".repeat(add.max(0) as usize));
        out.extend_from_slice(&b"-".repeat(del.max(0) as usize));
        out.push(b'\n');
    }

    if (count as usize) < files.len() {
        out.extend_from_slice(b" ...\n");
    }

    let (total, adds, dels) = stat_totals(files);
    stat_summary(out, total, adds, dels);
}

// ---------------------------------------------------------------------------
// --dirstat
// ---------------------------------------------------------------------------

/// `show_dirstat()`: damage per path, either one unit per file or the byte-level
/// score `diffcore_count_changes()` produces.
fn dirstat_damage(deltas: &[Delta], analyses: &[Analysis], opts: &Opts) -> Vec<(BString, u64)> {
    let mut out = Vec::new();
    for (d, an) in deltas.iter().zip(analyses) {
        // Both ids known and equal means the content cannot have changed.
        if d.old_valid() && d.new_valid() && !d.src_id.is_null() && !d.dst_id.is_null()
            && d.src_id == d.dst_id
        {
            out.push((d.path.clone(), 0));
            continue;
        }
        if opts.dirstat.by_file {
            out.push((d.path.clone(), 1));
            continue;
        }
        let damage = if d.old_valid() && d.new_valid() {
            let (copied, added) = count_changes(&an.old_data, &an.new_data, an.binary);
            (an.old_data.len() as u64).saturating_sub(copied) + added
        } else if d.old_valid() {
            an.old_data.len() as u64
        } else if d.new_valid() {
            an.new_data.len() as u64
        } else {
            continue;
        };
        out.push((d.path.clone(), if damage == 0 { 1 } else { damage }));
    }
    out
}

/// `conclude_dirstat()` + `gather_dirstat()`.
///
/// Shared with `diff-index`, whose `--dirstat` renders through this same walk.
pub(crate) fn render_dirstat(out: &mut Vec<u8>, mut files: Vec<(BString, u64)>, ds: &DirStat) {
    let changed: u64 = files.iter().map(|(_, d)| *d).sum();
    if changed == 0 {
        return;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut idx = 0usize;
    gather_dirstat(out, &files, &mut idx, changed, b"", 0, ds);
}

fn gather_dirstat(
    out: &mut Vec<u8>,
    files: &[(BString, u64)],
    idx: &mut usize,
    changed: u64,
    base: &[u8],
    baselen: usize,
    ds: &DirStat,
) -> u64 {
    let mut sum_changes: u64 = 0;
    let mut sources: u32 = 0;

    while *idx < files.len() {
        let name = files[*idx].0.as_slice();
        if name.len() < baselen {
            break;
        }
        if name[..baselen] != base[..baselen] {
            break;
        }
        let slash = name[baselen..].iter().position(|b| *b == b'/');
        let changes = match slash {
            Some(off) => {
                let newbaselen = baselen + off + 1;
                let newbase = name[..newbaselen].to_vec();
                sources += 1;
                gather_dirstat(out, files, idx, changed, &newbase, newbaselen, ds)
            }
            None => {
                let c = files[*idx].1;
                *idx += 1;
                sources += 2;
                c
            }
        };
        sum_changes += changes;
    }

    // Neither the top level nor a directory whose changes all came from one
    // subdirectory is reported.
    if baselen != 0 && sources != 1 && sum_changes != 0 {
        let permille = sum_changes * 1000 / changed;
        if permille >= u64::from(ds.permille) {
            out.extend_from_slice(
                format!("{:4}.{}% ", permille / 10, permille % 10).as_bytes(),
            );
            out.extend_from_slice(&base[..baselen]);
            out.push(b'\n');
            if !ds.cumulative {
                return 0;
            }
        }
    }
    sum_changes
}

/// `diffcore_count_changes()` from diffcore-delta.c: chunk both buffers on LF or
/// 64 bytes, hash each chunk, and compare the per-hash byte totals.
fn count_changes(src: &[u8], dst: &[u8], binary: bool) -> (u64, u64) {
    count_changes_sides(src, !binary, dst, !binary)
}

/// `diffcore_count_changes()` with the two `hash_chars()` calls given their own
/// `is_text` flags, which is how git derives them: `diff_filespec_is_binary()` is
/// asked about each filespec separately. `diff-index` needs that split because it
/// classifies the two sides independently.
pub(crate) fn count_changes_sides(src: &[u8], src_text: bool, dst: &[u8], dst_text: bool) -> (u64, u64) {
    let s = hash_chars(src, src_text);
    let d = hash_chars(dst, dst_text);

    let mut sc: u64 = 0;
    let mut la: u64 = 0;
    // Both maps iterate in hash order, which is the state git's `QSORT` leaves
    // its spanhash tables in before this merge walk.
    let dv: Vec<(u32, u64)> = d.into_iter().collect();
    let mut di = 0usize;

    for (shash, scnt) in s.iter() {
        while di < dv.len() && dv[di].0 < *shash {
            la += dv[di].1;
            di += 1;
        }
        let mut dcnt = 0u64;
        if di < dv.len() && dv[di].0 == *shash {
            dcnt = dv[di].1;
            di += 1;
        }
        if *scnt < dcnt {
            la += dcnt - *scnt;
            sc += *scnt;
        } else {
            sc += dcnt;
        }
    }
    while di < dv.len() {
        la += dv[di].1;
        di += 1;
    }
    (sc, la)
}

const HASHBASE: u32 = 107927;

/// `hash_chars()`: the per-chunk rolling hash, aggregated by hash value.
fn hash_chars(buf: &[u8], is_text: bool) -> BTreeMap<u32, u64> {
    let mut map: BTreeMap<u32, u64> = BTreeMap::new();
    let mut n: u32 = 0;
    let mut accum1: u32 = 0;
    let mut accum2: u32 = 0;
    let mut i = 0usize;
    while i < buf.len() {
        let c = buf[i];
        i += 1;
        // Ignore CR in a CRLF sequence if the content is text.
        if is_text && c == b'\r' && i < buf.len() && buf[i] == b'\n' {
            continue;
        }
        let old_1 = accum1;
        accum1 = (accum1 << 7) ^ (accum2 >> 25);
        accum2 = (accum2 << 7) ^ (old_1 >> 25);
        accum1 = accum1.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        // C computes this in `unsigned int`, so the multiply and add wrap at 2^32.
        let hashval = accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE;
        *map.entry(hashval).or_insert(0) += u64::from(n);
        n = 0;
        accum1 = 0;
        accum2 = 0;
    }
    if n > 0 {
        // C computes this in `unsigned int`, so the multiply and add wrap at 2^32.
        let hashval = accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE;
        *map.entry(hashval).or_insert(0) += u64::from(n);
    }
    map
}

// ---------------------------------------------------------------------------
// --summary
// ---------------------------------------------------------------------------

/// `is_summary_empty()`.
fn summary_is_empty(deltas: &[Delta]) -> bool {
    for d in deltas {
        match d.status {
            b'A' | b'D' | b'C' | b'R' => return false,
            _ => {
                if d.src_mode != 0 && d.dst_mode != 0 && d.src_mode != d.dst_mode {
                    return false;
                }
            }
        }
    }
    true
}

/// `diff_summary()`.
fn render_summary(out: &mut Vec<u8>, d: &Delta) {
    match d.status {
        b'D' => summary_mode_name(out, "delete", d.src_mode, &d.path),
        b'A' => summary_mode_name(out, "create", d.dst_mode, &d.path),
        _ => {
            if d.src_mode != 0 && d.dst_mode != 0 && d.src_mode != d.dst_mode {
                out.extend_from_slice(
                    format!(
                        " mode change {} => {} ",
                        mode_str(d.src_mode),
                        mode_str(d.dst_mode)
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(&quoted_name(&d.path));
                out.push(b'\n');
            }
        }
    }
}

/// `show_file_mode_name()`.
fn summary_mode_name(out: &mut Vec<u8>, verb: &str, mode: u32, path: &BString) {
    if mode != 0 {
        out.extend_from_slice(format!(" {verb} mode {} ", mode_str(mode)).as_bytes());
    } else {
        out.extend_from_slice(format!(" {verb} ").as_bytes());
    }
    out.extend_from_slice(&quoted_name(path));
    out.push(b'\n');
}

// ---------------------------------------------------------------------------
// --check
// ---------------------------------------------------------------------------

/// `builtin_checkdiff()` with git's default `core.whitespace`
/// (`blank-at-eol,space-before-tab,blank-at-eof`). Returns whether anything was
/// reported, which is `diff_result_code()`'s bit 1.
fn render_check(out: &mut Vec<u8>, deltas: &[Delta], analyses: &[Analysis]) -> bool {
    let mut failed = false;
    for (d, an) in deltas.iter().zip(analyses) {
        if d.unmerged || !d.new_valid() || an.binary {
            continue;
        }
        let name = quoted_name(&d.path);
        let new_lines = byte_lines(&an.new_data);
        // Only the added lines are checked: `--check` reports what the change
        // *introduces*, so the preimage is deliberately not examined.
        let Some(hunks) = &an.hunks else {
            continue;
        };
        let mut lineno = 0usize;
        for line in byte_lines(hunks) {
            match line.first().copied() {
                Some(b'@') => {
                    lineno = hunk_new_start(line).saturating_sub(1);
                }
                Some(b' ') => lineno += 1,
                Some(b'+') => {
                    lineno += 1;
                    // `is_conflict_marker()` sees the line terminator (it is what
                    // satisfies the "whitespace after the marker" requirement),
                    // while `ws_check()` strips it first, exactly like ws.c.
                    let raw = &line[1..];
                    if is_conflict_marker(raw) {
                        failed = true;
                        out.extend_from_slice(&name);
                        out.extend_from_slice(
                            format!(":{lineno}: leftover conflict marker\n").as_bytes(),
                        );
                    }
                    if let Some(err) = ws_check(strip_terminator(raw)) {
                        failed = true;
                        out.extend_from_slice(&name);
                        out.extend_from_slice(format!(":{lineno}: {err}.\n").as_bytes());
                    }
                }
                _ => {}
            }
        }
        if let Some(at) = check_blank_at_eof(&an.old_data, &new_lines) {
            failed = true;
            out.extend_from_slice(&name);
            out.extend_from_slice(format!(":{at}: new blank line at EOF.\n").as_bytes());
        }
    }
    failed
}

/// The `+<start>` field of a `@@ -a,b +c,d @@` header.
fn hunk_new_start(header: &[u8]) -> usize {
    let mut it = header.split(|b| *b == b'+');
    it.next();
    let Some(rest) = it.next() else { return 1 };
    let digits: Vec<u8> = rest.iter().copied().take_while(|b| b.is_ascii_digit()).collect();
    std::str::from_utf8(&digits)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1)
}

/// `is_conflict_marker()` with git's default marker size of 7.
fn is_conflict_marker(line: &[u8]) -> bool {
    const SIZE: usize = 7;
    if line.len() < SIZE + 1 {
        return false;
    }
    let first = line[0];
    if !matches!(first, b'=' | b'>' | b'<' | b'|') {
        return false;
    }
    if line[1..SIZE].iter().any(|b| *b != first) {
        return false;
    }
    line[SIZE].is_ascii_whitespace()
}

/// `ws_check()` restricted to git's default rule set, and
/// `whitespace_error_string()`'s comma-joined wording.
fn ws_check(line: &[u8]) -> Option<String> {
    let mut errs: Vec<&str> = Vec::new();
    if matches!(line.last().copied(), Some(b' ') | Some(b'\t')) {
        errs.push("trailing whitespace");
    }
    // Space immediately before a tab anywhere in the leading indent.
    let indent_len = line
        .iter()
        .position(|b| !matches!(*b, b' ' | b'\t'))
        .unwrap_or(line.len());
    if line[..indent_len]
        .windows(2)
        .any(|w| w[0] == b' ' && w[1] == b'\t')
    {
        errs.push("space before tab in indent");
    }
    if errs.is_empty() {
        None
    } else {
        Some(errs.join(", "))
    }
}

/// `check_blank_at_eof()`: the 1-based line of the first newly-added blank line
/// in the trailing run of blank lines, or `None`.
fn check_blank_at_eof(old: &[u8], new_lines: &[&[u8]]) -> Option<usize> {
    let old_lines = byte_lines(old);
    if new_lines.len() <= old_lines.len() {
        return None;
    }
    let trailing = |lines: &[&[u8]]| {
        lines
            .iter()
            .rev()
            .take_while(|l| {
                strip_terminator(l)
                    .iter()
                    .all(|b| matches!(*b, b' ' | b'\t'))
            })
            .count()
    };
    let l1 = trailing(&old_lines);
    let l2 = trailing(new_lines);
    if l2 <= l1 {
        return None;
    }
    Some(new_lines.len() - l2 + 1)
}

// ---------------------------------------------------------------------------
// patch output
// ---------------------------------------------------------------------------

/// Render one delta as a `git diff` file section into `out`.
fn render_patch(out: &mut Vec<u8>, d: &Delta, an: &Analysis, opts: &Opts) {
    if d.unmerged {
        out.extend_from_slice(b"* Unmerged path ");
        out.extend_from_slice(d.path.as_ref());
        out.push(b'\n');
        return;
    }

    // `-R` swaps the two prefixes, leaving the paths themselves alone.
    let (pa, pb): (&str, &str) = if opts.reverse {
        (&opts.dst_prefix, &opts.src_prefix)
    } else {
        (&opts.src_prefix, &opts.dst_prefix)
    };

    let old_hash = if d.old_valid() {
        an.src_id.to_hex_with_len(7).to_string()
    } else {
        "0000000".to_string()
    };
    let new_hash = if d.new_valid() {
        an.dst_id.to_hex_with_len(7).to_string()
    } else {
        "0000000".to_string()
    };
    let content_differs = old_hash != new_hash;

    // `builtin_diff()` only emits the header once it has something to attach to
    // it. A stat-dirty file whose bytes and mode are unchanged produces nothing,
    // which is why `git diff-files -p` is silent on a freshly copied worktree.
    let must_show = !d.old_valid()
        || !d.new_valid()
        || d.src_mode != d.dst_mode
        || content_differs
        || an.binary
        || an.hunks.is_some();
    if !must_show {
        return;
    }

    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&quote_two(pa, &d.path, pb, &d.path));
    out.push(b'\n');

    // File-creation / deletion / mode-change lines.
    match (d.old_valid(), d.new_valid()) {
        (false, true) => {
            out.extend_from_slice(b"new file mode ");
            out.extend_from_slice(mode_str(d.dst_mode).as_bytes());
            out.push(b'\n');
        }
        (true, false) => {
            out.extend_from_slice(b"deleted file mode ");
            out.extend_from_slice(mode_str(d.src_mode).as_bytes());
            out.push(b'\n');
        }
        (true, true) if d.src_mode != d.dst_mode => {
            out.extend_from_slice(b"old mode ");
            out.extend_from_slice(mode_str(d.src_mode).as_bytes());
            out.extend_from_slice(b"\nnew mode ");
            out.extend_from_slice(mode_str(d.dst_mode).as_bytes());
            out.push(b'\n');
        }
        _ => {}
    }

    // The `index <old>..<new>[ <mode>]` line only appears when content differs.
    if content_differs {
        out.extend_from_slice(b"index ");
        out.extend_from_slice(old_hash.as_bytes());
        out.extend_from_slice(b"..");
        out.extend_from_slice(new_hash.as_bytes());
        // Trailing mode only for an unchanged-mode modification.
        if d.old_valid() && d.new_valid() && d.src_mode == d.dst_mode {
            out.push(b' ');
            out.extend_from_slice(mode_str(d.dst_mode).as_bytes());
        }
        out.push(b'\n');
    }

    // `-D`: a deletion is shown by its header alone, with no recoverable preimage.
    if opts.irreversible_delete && !d.new_valid() {
        return;
    }

    let old_label = if d.old_valid() {
        quote_one(pa, &d.path)
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if d.new_valid() {
        quote_one(pb, &d.path)
    } else {
        b"/dev/null".to_vec()
    };

    if an.binary {
        out.extend_from_slice(b"Binary files ");
        out.extend_from_slice(&old_label);
        out.extend_from_slice(b" and ");
        out.extend_from_slice(&new_label);
        out.extend_from_slice(b" differ\n");
    } else if let Some(hunks) = &an.hunks {
        emit_file_line(out, b"--- ", &old_label);
        emit_file_line(out, b"+++ ", &new_label);
        for line in byte_lines(hunks) {
            let mut line = line.to_vec();
            match line.first().copied() {
                Some(b' ') => line[0] = opts.ind_ctx,
                Some(b'-') => line[0] = opts.ind_old,
                Some(b'+') => line[0] = opts.ind_new,
                _ => {}
            }
            out.extend_from_slice(&line);
        }
    }
}

/// `DIFF_SYMBOL_FILEPAIR_{MINUS,PLUS}`: a name containing a space gets a trailing
/// tab so the header stays unambiguously parseable.
fn emit_file_line(out: &mut Vec<u8>, lead: &[u8], label: &[u8]) {
    out.extend_from_slice(lead);
    out.extend_from_slice(label);
    if label.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

// ---------------------------------------------------------------------------
// path quoting (quote.c)
// ---------------------------------------------------------------------------

/// The escape character for `b`, or `None` if it can be emitted verbatim.
/// `Some(0)` means "octal-escape this byte".
fn cq_escape(b: u8) -> Option<u8> {
    match b {
        0x07 => Some(b'a'),
        0x08 => Some(b'b'),
        0x09 => Some(b't'),
        0x0a => Some(b'n'),
        0x0b => Some(b'v'),
        0x0c => Some(b'f'),
        0x0d => Some(b'r'),
        b'"' => Some(b'"'),
        b'\\' => Some(b'\\'),
        // Controls, DEL and (with the default `core.quotePath`) every high byte.
        0x00..=0x1f | 0x7f..=0xff => Some(0),
        _ => None,
    }
}

fn needs_quote(s: &[u8]) -> bool {
    s.iter().any(|b| cq_escape(*b).is_some())
}

/// The escaped body of `s`, without the surrounding double quotes.
fn cq_body(s: &[u8], out: &mut Vec<u8>) {
    for &b in s {
        match cq_escape(b) {
            None => out.push(b),
            Some(0) => {
                out.push(b'\\');
                out.push(((b >> 6) & 0o3) + b'0');
                out.push(((b >> 3) & 0o7) + b'0');
                out.push((b & 0o7) + b'0');
            }
            Some(c) => {
                out.push(b'\\');
                out.push(c);
            }
        }
    }
}

/// `write_name_quoted()`: the path, double-quoted and escaped only if needed.
fn quoted_name(path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_quote(s) {
        return s.to_vec();
    }
    let mut out = vec![b'"'];
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// `quote_two_c_style()` for a single prefixed name (the `---`/`+++` lines).
fn quote_one(prefix: &str, path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_quote(prefix.as_bytes()) && !needs_quote(s) {
        let mut out = prefix.as_bytes().to_vec();
        out.extend_from_slice(s);
        return out;
    }
    let mut out = vec![b'"'];
    cq_body(prefix.as_bytes(), &mut out);
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// The `diff --git <a> <b>` name pair.
fn quote_two(pa: &str, a: &BString, pb: &str, b: &BString) -> Vec<u8> {
    let mut out = quote_one(pa, a);
    out.push(b' ');
    out.extend_from_slice(&quote_one(pb, b));
    out
}

// ---------------------------------------------------------------------------
// unified-diff hunk sink
// ---------------------------------------------------------------------------

/// Format one side of a hunk header (`@@ -<here> +<here> @@`), omitting the length
/// when it is 1 and using the pre-hunk line number when it is 0, like `git diff`.
fn fmt_range(start: u32, len: u32) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

/// A [`ConsumeHunk`] sink that renders unified-diff hunks into a byte buffer.
///
/// The tokens the differ compares may be whitespace-normalized (`-w` and friends),
/// so line *content* is taken from the original line tables instead, tracked by the
/// cursors the hunk header establishes.
/// Hunks are always rendered with git's canonical `+`/`-`/` ` markers;
/// `--output-indicator-*` is applied when the patch is written out, so every
/// consumer of these bytes (`--check`, `-G`) can rely on the canonical form.
struct PatchSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
}

impl ConsumeHunk for PatchSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.buf.extend_from_slice(b"@@ -");
        self.buf.extend_from_slice(
            fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes(),
        );
        self.buf.extend_from_slice(b" +");
        self.buf
            .extend_from_slice(fmt_range(header.after_hunk_start, header.after_hunk_len).as_bytes());
        self.buf.extend_from_slice(b" @@\n");

        let mut bi = header.before_hunk_start.saturating_sub(1) as usize;
        let mut ai = header.after_hunk_start.saturating_sub(1) as usize;
        for (kind, fallback) in lines {
            let (marker, content): (u8, &[u8]) = match kind {
                DiffLineKind::Context => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    ai += 1;
                    (b' ', c)
                }
                DiffLineKind::Remove => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    (b'-', c)
                }
                DiffLineKind::Add => {
                    let c = self.after.get(ai).copied().unwrap_or(*fallback);
                    ai += 1;
                    (b'+', c)
                }
            };
            self.buf.push(marker);
            self.buf.extend_from_slice(content);
            // Tokens keep their line terminator; a token without one is the last
            // line of a file that lacks a trailing newline.
            if content.last() != Some(&b'\n') {
                self.buf.push(b'\n');
                self.buf.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}
