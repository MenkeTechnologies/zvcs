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
//! comparator that reports a difference without reading content. gix's own fast
//! path still returns "unchanged" before the comparator is consulted whenever the
//! stat data matches and the entry is not racily clean, so the result is git's
//! rule. The one case where git *does* read content is a racy entry — one whose
//! mtime is at or after the index timestamp, where the stat comparison cannot be
//! trusted — and [`StatOnly`] reproduces `ce_modified_check_fs()` there.
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
//!   * `--diff-algorithm=<myers|minimal|histogram|default>` and the `--minimal` /
//!     `--histogram` aliases select the xdiff algorithm the content pass runs (an
//!     explicit flag overrides the `diff.algorithm` config default).
//!   * `-R`, `--diff-filter=<letters>`, `-S<string>`, `-G<regex>`, `--pickaxe-all`,
//!     `--pickaxe-regex`, `--find-object=<id>`. `-G`, `-I` and `-S --pickaxe-regex`
//!     compile with `regex::bytes` (Unicode off, byte semantics) to mirror git's
//!     `regcomp`; `--find-object` is `pickaxe_match()`'s objfind branch and, because
//!     git never hashes the worktree side for objfind, matches on the staged blob id.
//!   * `-O<orderfile>` reorders the queued pairs by the order file's glob patterns
//!     (`diffcore_order`); `--output=<file>` writes every rendered byte to `<file>`.
//!   * `-c`/`--cc` and the free combined diff `git diff-files -p` produces for a
//!     conflict: `run_diff_files()` routes an unmerged path that kept both stage #2
//!     and stage #3 through `show_combined_diff()`, so the patch is a `diff --cc`
//!     (or `diff --combined` under a bare `-c`) and, when a raw/name format is also
//!     on, the record is the `::`-prefixed combined form.
//!   * `-0`/`-1`/`-2`/`-3`, `--base`/`--ours`/`--theirs` (unmerged stage selection).
//!   * `--color[=always|auto|never]`/`--no-color` and `--ws-error-highlight=<kind>`:
//!     the patch and the stat graph are painted from the `color.diff.*` slots, with
//!     git's `ws.c` whitespace-error markup driven by `core.whitespace`.
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
//!   * `-I<regex>`/`--ignore-matching-lines=<regex>` together with a *patch* format:
//!     the counts drop `-I`-only hunks, but the unified writer renders in one pass and
//!     cannot suppress a hunk mid-stream, so `-p -I` bails rather than print a wrong
//!     patch. `-I` with raw/stat output is fully supported.
//!   * `--binary` for content that is actually binary (the `GIT binary patch`
//!     literal/delta encoding is not produced).
//!   * `--patience` / `--diff-algorithm=patience` together with a format that consumes
//!     the line diff (`-p`, `--numstat`/`--stat`/`--shortstat`, `--check`,
//!     `--dirstat=lines`): imara-diff has no patience variant, so rather than silently
//!     substituting Myers this bails. `--patience` with a raw/name/summary listing is a
//!     no-op, since those never diff line content.
//!   * `-M`/`--find-renames` rename *pairing* (an intent-to-add worktree file matched
//!     to a staged deletion): accepted as a no-op, so such a pair is still reported as
//!     a separate `D`+`A` rather than a single `R`.

use anyhow::Result;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::pipeline::{Mode, WorktreeRoots};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::{Algorithm, InternedInput, ResourceKind};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use gix::prelude::ObjectIdExt;
use regex::bytes::Regex;

use super::diff_color;

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

/// Output-format flags whose git option is an `OPT_BITOP` that *clears*
/// `DIFF_FORMAT_NO_OUTPUT` when it fires. `--name-only`/`--name-status`/`--check`
/// are plain `OPT_BIT`s and are deliberately excluded — they never clear it, which
/// is why `-s --name-only` dies while `-s --raw` prints raw.
const F_POSITIVE: u32 = F_RAW | F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_DIRSTAT | F_SUMMARY | F_PATCH;

/// Every output-format bit `-s`/`--no-patch` clears when it sets `NO_OUTPUT`.
const F_ALL_FORMATS: u32 = F_POSITIVE | F_NAME | F_NAME_STATUS | F_CHECKDIFF;

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

/// A search pattern: a literal substring (git's kwset path for a plain `-S`) or a
/// compiled regular expression (git's `-G`, `-I`, and `-S --pickaxe-regex`, all of
/// which call `regcomp` with `REG_EXTENDED | REG_NEWLINE`).
enum Needle {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl Needle {
    /// Whether `hay` contains a match — used by `-G` on each changed line and by `-I`.
    fn is_match(&self, hay: &[u8]) -> bool {
        match self {
            Needle::Literal(n) => count_occurrences(hay, n) > 0,
            Needle::Regex(re) => re.is_match(hay),
        }
    }

    /// Non-overlapping match count — used by `-S` to compare the two sides.
    fn count(&self, hay: &[u8]) -> usize {
        match self {
            Needle::Literal(n) => count_occurrences(hay, n),
            Needle::Regex(re) => re.find_iter(hay).count(),
        }
    }
}

/// Compile a `-G`/`-I`/`-S --pickaxe-regex` pattern the way git's `regcomp` does: on
/// bytes, without Unicode mode so `.` and the character classes carry git's C-locale
/// byte semantics, and with multi-line mode standing in for `REG_NEWLINE` since matching
/// is done a line at a time. `Err` carries the engine's message for the fatal.
fn compile_regex(pat: &[u8]) -> std::result::Result<Regex, String> {
    let s = std::str::from_utf8(pat).map_err(|_| "invalid byte sequence in pattern".to_owned())?;
    regex::bytes::RegexBuilder::new(s)
        .unicode(false)
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

fn matches_any(pats: &[Needle], line: &[u8]) -> bool {
    pats.iter().any(|p| p.is_match(line))
}

fn strip_terminator(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

/// `-S<string>` counts occurrences; `-G<pattern>` looks at the changed lines;
/// `--find-object=<id>` keeps a pair that touches one of the named object ids.
enum PickaxeKind {
    /// `-S`: a literal count by default, a regex count under `--pickaxe-regex`.
    Occurrences(Needle),
    /// `-G`: a regex over the added and removed lines.
    Grep(Needle),
    /// `--find-object=<id>`: `pickaxe_match()`'s `DIFF_PICKAXE_KIND_OBJFIND` branch.
    ObjFind(Vec<ObjectId>),
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

/// `parse_dirstat_params()` (diff.c:141): fold a comma-separated parameter list into
/// `ds`, returning the accumulated complaint text (empty when every parameter parsed).
/// The caller decides what to do with it — `--dirstat=` dies, `diff.dirstat` warns.
///
/// An empty list is not split at all, so `--dirstat=` simply keeps the defaults.
pub(crate) fn parse_dirstat_params(params: &str, ds: &mut DirStat) -> String {
    let mut errors = String::new();
    if params.is_empty() {
        return errors;
    }
    for p in params.split(',') {
        match p {
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
            _ => match parse_permille(p) {
                Some(permille) => ds.permille = permille,
                // git only reaches its `strtoul` when the first byte is a digit;
                // anything else is an unknown parameter, not a bad number.
                None if p.as_bytes().first().is_some_and(u8::is_ascii_digit) => errors
                    .push_str(&format!("  Failed to parse dirstat cut-off percentage '{p}'\n")),
                None => errors.push_str(&format!("  Unknown dirstat parameter '{p}'\n")),
            },
        }
    }
    errors
}

/// A dirstat cut-off percentage: a whole number plus at most one significant decimal
/// digit, with any further digits read and discarded, and nothing left over — exactly
/// what `parse_dirstat_params()`'s `strtoul` walk accepts.
pub(crate) fn parse_permille(p: &str) -> Option<u32> {
    let b = p.as_bytes();
    if !b.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let end = b.iter().position(|c| !c.is_ascii_digit()).unwrap_or(b.len());
    // git reads this with `strtoul`, which saturates rather than failing; a threshold
    // that large simply never matches.
    let whole: u32 = p[..end].parse().unwrap_or(u32::MAX / 10);
    let mut permille = whole.saturating_mul(10);
    let mut rest = &b[end..];
    if rest.first() == Some(&b'.') && rest.get(1).is_some_and(u8::is_ascii_digit) {
        permille = permille.saturating_add(u32::from(rest[1] - b'0'));
        rest = &rest[2..];
        let extra = rest.iter().position(|c| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest = &rest[extra..];
    }
    rest.is_empty().then_some(permille)
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
    binary: bool,                  // --binary: emit a GIT binary patch for a binary pair
    line_prefix: Vec<u8>,          // --line-prefix=<s>, emitted before every record
    anchor: Option<Anchor>,
    relative: Relative,
    /// `--ignore-submodules[=<when>]`; `None` leaves gix on its configured default.
    ignore_submodules: Option<gix::submodule::config::Ignore>,
    ctx: u32,
    ws: Whitespace,
    /// `-I<re>`: set with the whitespace family, this forces `diff_from_contents`.
    ignore_lines: Vec<Needle>,
    /// The spelling of the first `-I`, for the bail when a patch is also asked for.
    ignore_flag: Option<String>,
    /// A flag that rewrites content output (word diff). Harmless for raw
    /// listings, so it only bails once a content format is requested.
    content_altering: Option<String>,
    /// `--color[=<when>]` / `--no-color`; `None` defers to `color.diff` /
    /// `diff.color` / `color.ui` and the terminal test.
    color_when: Option<diff_color::ColorWhen>,
    /// `--ws-error-highlight=<kind>`, seeded from `diff.wsErrorHighlight`.
    ws_error_highlight: u32,
    /// `--color-moved*` / `--word-diff*` / `--color-words`, resolved against
    /// `diff.colorMoved` / `diff.colorMovedWS` / `diff.wordRegex` at render time.
    move_word: diff_color::MoveWordOpts,
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
    /// The finalized pickaxe, built after the whole line is read so `--pickaxe-regex`
    /// and `--pickaxe-all` (which may follow the `-S`/`-G`) can fold in.
    pickaxe: Option<Pickaxe>,
    /// The raw `-S`/`-G` argument, kept until the finalize pass: `b'S'` counts
    /// occurrences, `b'G'` greps changed lines. Only the last one on the line wins.
    pickaxe_pending: Option<(u8, Vec<u8>)>,
    /// `--find-object=<id>` arguments, resolved against the odb after parsing so a bad
    /// id is reported only once every earlier argument has validated (git's deferral).
    find_object_args: Vec<String>,
    /// `-O<file>`: reorder the queued pairs by the glob patterns in `<file>`.
    order_file: Option<String>,
    /// `--output=<file>`: write every rendered byte to `<file>` instead of stdout.
    output_file: Option<String>,
    /// `--pickaxe-all`, which may appear before or after the `-S`/`-G` it modifies.
    pickaxe_all: bool,
    /// `--pickaxe-regex`: makes `-S` a regex search rather than a literal count.
    pickaxe_regex: bool,
    /// The `DIFF_PICKAXE_KIND_*` bits `diff_setup_done()` tests, accumulated
    /// across the whole command line. Distinct from [`Opts::pickaxe_pending`],
    /// which keeps only the last `-S`/`-G`: the bits are sticky, which is how
    /// `-G<re> -S<s>` is rejected even though only one search would have run.
    pickaxe_kinds: u8,
    stat: StatWidths,
    dirstat: DirStat,
    /// `-0`/`-1`/`-2`/`-3`, `--base`/`--ours`/`--theirs`. git's default is 2.
    unmerged_stage: u8,
    /// `-C`/`--find-copies[-harder]`: rename detection registers every "added"
    /// pair as a copy destination, which hashes its worktree side on the way.
    find_copies: bool,
    /// `run_diff_files()` shows a combined diff (`show_combined_diff()`) for an
    /// unmerged path that still has both stage #2 and stage #3, instead of the
    /// `* Unmerged path` marker plus a two-way diff against one stage. This is on
    /// whenever git's `revs->combine_merges` is: set by `-c`/`--cc`, or set for
    /// free by `diff_merges_set_dense_combined_if_unset()` when no explicit stage
    /// was requested and a patch is being produced.
    combine_merges: bool,
    /// `revs->dense_combined_merges`: `--cc` (and the free default) densify the
    /// header to `diff --cc`; a bare `-c` leaves it `diff --combined`.
    dense_combined: bool,
    /// True once any of `-0`/`-1`/`-2`/`-3`/`--base`/`--ours`/`--theirs` is seen,
    /// i.e. git's `revs->max_count != -1`, which suppresses the free combined diff.
    explicit_stage: bool,
    /// `-c`/`--cc` set `revs->merges_need_diff`, which forces the patch format when
    /// no other output format was requested.
    merges_need_diff: bool,
    /// `--full-index`: the combined header prints full object ids.
    full_index: bool,
    /// `--inter-hunk-context=<n>`: `xecfg.interhunkctxlen`, the extra gap two change
    /// groups may leave between them and still land in one hunk. Applied by the shared
    /// `xdl_emit_diff` port in [`super::diff_pairs::emit_unified`].
    inter_hunk_ctx: usize,
    /// `--diff-algorithm=`/`--minimal`/`--histogram`/`--patience`: the xdiff algorithm
    /// the CLI selected. `None` leaves gix on the repo's `diff.algorithm` config default
    /// (git precedence: an explicit CLI flag overrides config).
    algorithm: Option<Algorithm>,
    /// `XDF_INDENT_HEURISTIC`: where a hunk that can slide freely finally lands.
    /// `git_diff_heuristic_config()` runs from `git_diff_basic_config()`, so
    /// `diff.indentHeuristic` reaches plumbing too, and
    /// `--[no-]indent-heuristic` overrides it.
    indent_heuristic: bool,
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

/// One unmerged path that `run_diff_files()` routes through `show_combined_diff()`
/// because it kept both stage #2 (ours) and stage #3 (theirs). git records these
/// two stages as `dpath->parent[0]` and `dpath->parent[1]`, with the worktree file
/// as the single result side.
struct CombinedPath {
    path: BString,
    /// `parent[0]` = stage #2, `parent[1]` = stage #3: their staged blob and mode.
    parents: [(ObjectId, u32); 2],
    /// The result mode, i.e. the worktree file's mode (`0` when it is gone).
    wt_mode: u32,
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
    /// `fatal: option '<opt>' must come before non-option arguments`, exit 128.
    /// `setup_revisions()` refuses any dashed option once a pathspec has been seen.
    OptionAfterArg(String),
    /// `fatal: bad --ignore-submodules argument: <v>`, exit 128.
    BadIgnoreSubmodules(String),
    /// `fatal: Failed to parse --dirstat/-X option parameter:\n<errmsg>`, exit 128 —
    /// `parse_dirstat_opt()`'s `die()`, carrying the text `parse_dirstat_params()`
    /// accumulated (which already ends in a newline of its own).
    DirStatParams(String),
    /// `error: option 'inter-hunk-context' expects a numerical value`, exit 129.
    /// The `OPT_MAGNITUDE` empty-value branch.
    Magnitude(&'static str),
    /// `error: option '<opt>' expects a non-negative integer value with an optional
    /// k/m/g suffix`, exit 129. The `OPT_MAGNITUDE` bad-value branch.
    BadMagnitude(&'static str),
    /// `error: unknown value after ws-error-highlight=<prefix>`, exit 129, where
    /// `<prefix>` is the accepted portion of the value before the offending token.
    WsErrorHighlight(String),
    /// An already-formatted `error: …` block from an option callback that git
    /// reports verbatim before parse-options exits 129 — the `--color-moved`,
    /// `--color-moved-ws` and `--word-diff` argument errors.
    OptionError(String),
    /// `fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be
    /// used together`, exit 128. `diff_setup_done()` dies when `-s` (NO_OUTPUT) is
    /// left set alongside a name/status/check format.
    NameStatusNoPatch,
    /// `diff_setup_done()`: more than one of `-G`, `-S` and `--find-object`.
    PickaxeKinds,
    /// `diff_setup_done()`: `-G` together with `--pickaxe-regex`, which only `-S`
    /// takes.
    PickaxeGRegex,
    /// `diff_setup_done()`: `--pickaxe-all` together with `--find-object`.
    PickaxeAllObjfind,
    /// `fatal: invalid regex: <msg>`, exit 128. git compiles the `-G`/`-S --pickaxe-regex`
    /// pattern in `diffcore_pickaxe` setup, after argument validation; the message tail
    /// is the `regex` crate's rather than the platform `regerror`'s.
    InvalidRegexPickaxe(String),
    /// `error: invalid regex given to -I: '<pat>'`, exit 129. `diff_opt_ignore_regex`
    /// compiles inline, so this fires at the flag's own argv position.
    InvalidRegexIgnore(String),
    /// `error: unable to resolve '<arg>'`, exit 128. `diff_opt_find_object` fails to
    /// resolve the `--find-object` argument to an object id.
    UnableToResolve(String),
}

/// `diff_files_usage` — the synopsis *and* the `common diff options:` table
/// `usage()` prints under it. The synopsis alone was only its first line.
const USAGE: &str = r"usage: git diff-files [-q] [-0 | -1 | -2 | -3 | -c | --cc] [<common-diff-options>] [<path>...]

common diff options:
  -z            output diff-raw with lines terminated with NUL.
  -p            output patch format.
  -u            synonym for -p.
  --patch-with-raw
                output both a patch and the diff-raw format.
  --stat        show diffstat instead of patch.
  --numstat     show numeric diffstat instead of patch.
  --patch-with-stat
                output a patch and prepend its diffstat.
  --name-only   show only names of changed files.
  --name-status show names and status of changed files.
  --full-index  show full object name on index lines.
  --abbrev=<n>  abbreviate object names in diff-tree header and diff-raw.
  -R            swap input file pairs.
  -B            detect complete rewrites.
  -M            detect renames.
  -C            detect copies.
  --find-copies-harder
                try unchanged files as candidate for copy detection.
  -l<n>         limit rename attempts up to <n> paths.
  -O<file>      reorder diffs according to the <file>.
  -S<string>    find filepair whose only one side contains the string.
  --pickaxe-all
                show all files diff when -S is used and hit is found.
  -a  --text    treat all files as text.

";

impl Fatal {
    /// Report on stderr the way git does and hand back git's exit code.
    fn report(self) -> ExitCode {
        let mut err = std::io::stderr().lock();
        match self {
            Fatal::Usage => {
                let _ = write!(err, "{USAGE}");
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
            Fatal::DirStatParams(errors) => {
                let _ = write!(
                    err,
                    "fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n"
                );
            }
            Fatal::ColorValue => {
                let _ = writeln!(
                    err,
                    "error: option `color' expects \"always\", \"auto\", or \"never\""
                );
                return ExitCode::from(129);
            }
            Fatal::OptionAfterArg(opt) => {
                let _ = writeln!(err, "fatal: option '{opt}' must come before non-option arguments");
            }
            Fatal::BadIgnoreSubmodules(v) => {
                let _ = writeln!(err, "fatal: bad --ignore-submodules argument: {v}");
            }
            Fatal::Magnitude(opt) => {
                let _ = writeln!(err, "error: option `{opt}' expects a numerical value");
                return ExitCode::from(129);
            }
            Fatal::BadMagnitude(opt) => {
                let _ = writeln!(
                    err,
                    "error: option `{opt}' expects a non-negative integer value with an optional k/m/g suffix"
                );
                return ExitCode::from(129);
            }
            Fatal::WsErrorHighlight(prefix) => {
                let _ = writeln!(err, "error: unknown value after ws-error-highlight={prefix}");
                return ExitCode::from(129);
            }
            Fatal::OptionError(msg) => {
                let _ = writeln!(err, "{msg}");
                return ExitCode::from(129);
            }
            Fatal::NameStatusNoPatch => {
                let _ = writeln!(
                    err,
                    "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
                );
            }
            Fatal::PickaxeKinds => {
                let _ = writeln!(
                    err,
                    "fatal: options '-G', '-S', and '--find-object' cannot be used together"
                );
            }
            Fatal::PickaxeGRegex => {
                let _ = writeln!(
                    err,
                    "fatal: options '-G' and '--pickaxe-regex' cannot be used together, \
                     use '--pickaxe-regex' with '-S'"
                );
            }
            Fatal::PickaxeAllObjfind => {
                let _ = writeln!(
                    err,
                    "fatal: options '--pickaxe-all' and '--find-object' cannot be used together, \
                     use '--pickaxe-all' with '-G' and '-S'"
                );
            }
            Fatal::InvalidRegexPickaxe(msg) => {
                let _ = writeln!(err, "fatal: invalid regex: {msg}");
            }
            Fatal::InvalidRegexIgnore(pat) => {
                let _ = writeln!(err, "error: invalid regex given to -I: '{pat}'");
                return ExitCode::from(129);
            }
            Fatal::UnableToResolve(arg) => {
                let _ = writeln!(err, "error: unable to resolve '{arg}'");
            }
        }
        ExitCode::from(128)
    }
}

/// Status letters `--diff-filter` understands.
const FILTER_LETTERS: &[u8] = b"ACDMRTUXB";

pub fn diff_files(args: &[String]) -> Result<ExitCode> {
    // `show_usage_if_asked(argc, argv, diff_files_usage)` (builtin/diff-files.c:32):
    // a lone `-h` answers on stdout at 129, before anything else runs.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

    // Dispatch strips the subcommand, but tolerate it being present so the entry
    // point behaves the same either way.
    let args = match args.first() {
        Some(first) if first == "diff-files" => &args[1..],
        _ => args,
    };

    let repo = gix::discover(".")?;
    init_quote_path(&repo);
    match parse(&repo, args) {
        Ok(Parsed::Run { opts, paths }) => run(&repo, opts, paths),
        // A real `diff-files` flag this port has not implemented. It names the
        // flag and stops there: an inventory of what *is* implemented is this
        // port's state, not anything git would say, and it goes stale the moment
        // a flag lands. A flag git itself does not know never reaches here — it
        // takes parse-options' own `unknown option` path at 129.
        Ok(Parsed::Unsupported(flag)) => anyhow::bail!("unsupported flag {flag:?}"),
        Err(fatal) => Ok(fatal.report()),
    }
}

/// The outcome of argument parsing: either a runnable request, or the first
/// real-git flag we have not ported.
#[allow(clippy::large_enum_variant)] // Boxing would churn every construct/match site.
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
        binary: false,
        line_prefix: Vec::new(),
        anchor: None,
        relative: Relative::No,
        ignore_submodules: None,
        ctx: 3,
        ws: Whitespace::Keep,
        ignore_lines: Vec::new(),
        ignore_flag: None,
        content_altering: None,
        color_when: None,
        // `diff.wsErrorHighlight`, or git's `WSEH_NEW` default.
        ws_error_highlight: diff_color::ws_error_highlight_default(repo)
            .unwrap_or(diff_color::WSEH_NEW),
        move_word: diff_color::MoveWordOpts::default(),
        src_prefix: "a/".to_owned(),
        dst_prefix: "b/".to_owned(),
        ind_new: b'+',
        ind_old: b'-',
        ind_ctx: b' ',
        irreversible_delete: false,
        reverse: false,
        filter: None,
        pickaxe: None,
        pickaxe_pending: None,
        find_object_args: Vec::new(),
        order_file: None,
        output_file: None,
        pickaxe_all: false,
        pickaxe_regex: false,
        pickaxe_kinds: 0,
        stat: StatWidths::default(),
        dirstat: DirStat::default(),
        unmerged_stage: 2,
        find_copies: false,
        combine_merges: false,
        dense_combined: false,
        explicit_stage: false,
        merges_need_diff: false,
        full_index: false,
        inter_hunk_ctx: 0,
        algorithm: None,
        indent_heuristic: super::diff_pairs::indent_heuristic_default(repo),
    };
    let mut quiet = false;
    let mut paths: Vec<BString> = Vec::new();
    let mut unsupported: Option<String> = None;
    // `setup_revisions()` does not reject an option it does not know: it hands it
    // back in the leftover argv, and only once the whole line has been walked does
    // `cmd_diff_files()`'s `while (1 < argc && argv[1][0] == '-')` loop reach it and
    // call `usage(diff_files_usage)`. So an unknown flag loses to anything the rest
    // of the line dies on — `git diff-files --bogus-flag README.md -not-a-flag`
    // reports the trailing `-not-a-flag` (128), not the bogus flag (129).
    let mut unknown: Option<String> = None;
    let mut after_dashdash = false;
    // `setup_revisions()` records the first pathspec; from then on any dashed
    // option is rejected before it is even classified.
    let mut seen_non_option = false;
    // `--ws-error-highlight <kind>`, `--color-moved-ws <modes>` and
    // `--word-diff-regex <re>` all spell their value as the next argument when it is
    // not glued on with `=`, which parse-options consumes before anything else —
    // `--` included. This holds the flag still waiting for that value.
    let mut pending_value: Option<String> = None;

    for a in args {
        let s = a.as_str();
        if let Some(flag) = pending_value.take() {
            if flag == "-I" {
                // `OPT_CALLBACK_F('I', "ignore-matching-lines", …)`: a required value,
                // so parse-options takes the next argument when none is glued on.
                record_ignore_lines(&flag, s, &mut opts)?;
            } else if flag == "--ws-error-highlight" {
                opts.ws_error_highlight = parse_ws_error_highlight_opt(s)?;
            } else {
                let Opts { move_word, color_when, .. } = &mut opts;
                if let Some(res) = move_word.parse_flag(&format!("{flag}={s}"), color_when) {
                    res.map_err(Fatal::OptionError)?;
                }
            }
            continue;
        }
        if s == "-I" || s == "--ignore-matching-lines" {
            pending_value = Some("-I".to_string());
            continue;
        }
        if s == "--ws-error-highlight" || diff_color::needs_separate_value(s) {
            pending_value = Some(s.to_string());
            continue;
        }
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
            // "option '<opt>' must come before non-option arguments": this beats
            // both flag-validity (129) and per-value checks, so it is tested first.
            if seen_non_option {
                return Err(Fatal::OptionAfterArg(s.to_owned()));
            }
            let fmt_before = opts.fmt;
            match classify(s, &mut opts, &mut quiet)? {
                Flag::Handled => {}
                Flag::Unsupported => {
                    if unsupported.is_none() {
                        unsupported = Some(s.to_owned());
                    }
                }
                Flag::Unknown => {
                    if unknown.is_none() {
                        unknown = Some(s.to_owned());
                    }
                }
            }
            // `OPT_BITOP`: a positive output-format flag clears `NO_OUTPUT`, so a
            // later `--raw`/`-p`/`--stat` re-enables output after an earlier `-s`.
            if (opts.fmt & !fmt_before) & F_POSITIVE != 0 {
                opts.fmt &= !F_NO_OUTPUT;
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
        seen_non_option = true;
    }

    // A value-taking option left at the end of the command line never reaches its
    // callback: parse-options reports it and exits 129 before anything else runs.
    if let Some(flag) = pending_value {
        return Err(Fatal::OptionError(format!(
            "error: {}",
            diff_color::missing_value(&flag)
        )));
    }

    // `diff_merges_setup_revs()`: `-c`/`--cc` set `merges_need_diff`, which forces
    // the patch format when nothing else was asked for.
    if opts.merges_need_diff && opts.fmt == 0 {
        opts.fmt |= F_PATCH;
    }

    // `diff_setup_done()`'s opening `HAS_MULTI_BITS(output_format & check_mask)`:
    // any *two* of `--name-only`, `--name-status`, `--check` and `-s` is fatal.
    // `-s` reaches this with the others still set only when it came first, since
    // its own `OPT_BITOP` clears them in argument order — which is why
    // `--name-only -s` is accepted and `-s --name-only` is not.
    if (opts.fmt & (F_NAME | F_NAME_STATUS | F_CHECKDIFF | F_NO_OUTPUT)).count_ones() > 1 {
        return Err(Fatal::NameStatusNoPatch);
    }
    // Only then does it clear every other content/patch format. `-s` (NO_OUTPUT)
    // is not in this trigger — its `OPT_BITOP` already did the clearing.
    if opts.fmt & (F_NAME | F_NAME_STATUS | F_CHECKDIFF) != 0 {
        opts.fmt &= !F_POSITIVE;
    }
    // The three pickaxe `HAS_MULTI_BITS` fatals `diff_setup_done()` raises next,
    // in its order. Each is "more than one bit of this mask is set", so they fire
    // on the *combination* rather than on either option alone.
    if opts.pickaxe_kinds.count_ones() > 1 {
        return Err(Fatal::PickaxeKinds);
    }
    if opts.pickaxe_kinds & PICKAXE_KIND_G != 0 && opts.pickaxe_regex {
        return Err(Fatal::PickaxeGRegex);
    }
    if opts.pickaxe_kinds & PICKAXE_KIND_OBJFIND != 0 && opts.pickaxe_all {
        return Err(Fatal::PickaxeAllObjfind);
    }
    // `setup_revisions()` has now finished (its closing `diff_setup_done()` is the
    // check just above), so this is where `cmd_diff_files()` walks the leftover argv
    // and answers the first unrecognized option with its usage text.
    if unknown.is_some() {
        return Err(Fatal::Usage);
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

    // `cmd_diff_files()`: "diff-files --base -p should not combine merges because
    // it was not asked to". With no explicit stage and a patch on the way,
    // `diff_merges_set_dense_combined_if_unset()` turns on a dense combined diff
    // for free; `-c`/`--cc` have already set `combine_merges` by this point, so the
    // "if unset" guard leaves their (possibly non-dense) choice alone.
    if !opts.explicit_stage && opts.fmt & F_PATCH != 0 && !opts.combine_merges {
        opts.combine_merges = true;
        opts.dense_combined = true;
    }

    // `--pickaxe-all` / `--pickaxe-regex` may appear on either side of the `-S`/`-G`
    // they modify, so the pickaxe is finalized once the whole line has been read.
    // `-G` is always a regex; `-S` is a literal kwset search unless `--pickaxe-regex`
    // promotes it. git compiles the regex in `diffcore_pickaxe` setup, after argument
    // validation, so a bad pattern is `fatal: invalid regex` (128) only once every
    // earlier argument has passed.
    if let Some((kind, raw)) = opts.pickaxe_pending.take() {
        let needle = if kind == b'S' && !opts.pickaxe_regex {
            Needle::Literal(raw)
        } else {
            match compile_regex(&raw) {
                Ok(re) => Needle::Regex(re),
                Err(msg) => return Err(Fatal::InvalidRegexPickaxe(msg)),
            }
        };
        opts.pickaxe = Some(Pickaxe {
            kind: if kind == b'S' {
                PickaxeKind::Occurrences(needle)
            } else {
                PickaxeKind::Grep(needle)
            },
            all: opts.pickaxe_all,
        });
    }
    // `--find-object` is `DIFF_PICKAXE_KIND_OBJFIND`, which takes precedence over
    // `-S`/`-G` in `pickaxe_match()`, so it overwrites any pending occurrence/grep
    // pickaxe. Each argument is resolved to an object id (git's `repo_get_oid`).
    if !opts.find_object_args.is_empty() {
        let mut ids = Vec::with_capacity(opts.find_object_args.len());
        for arg in std::mem::take(&mut opts.find_object_args) {
            match repo.rev_parse_single(arg.as_str()) {
                Ok(id) => ids.push(id.detach()),
                Err(_) => return Err(Fatal::UnableToResolve(arg)),
            }
        }
        opts.pickaxe = Some(Pickaxe {
            kind: PickaxeKind::ObjFind(ids),
            all: opts.pickaxe_all,
        });
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
    "--submodule",
    // Colored *moves* are not detected by this port; the flag is left in the
    // unsupported list below rather than silently accepted.
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
    // `revision.c`'s `--no-notes` turns off a display that is off by default here,
    // so it cannot change any output this command produces.
    "--no-notes",
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
    // diff-files' "stay quiet about removed files"; zvcs never warns about them.
    "-q",
];

/// `DIFF_PICKAXE_KIND_S`: a `-S<string>` search was requested.
const PICKAXE_KIND_S: u8 = 1;
/// `DIFF_PICKAXE_KIND_G`: a `-G<regex>` search was requested.
const PICKAXE_KIND_G: u8 = 2;
/// `DIFF_PICKAXE_KIND_OBJFIND`: a `--find-object=<id>` search was requested.
const PICKAXE_KIND_OBJFIND: u8 = 4;

/// Prefixes of valued options in the same category as [`ACCEPTED_NOOP`].
const ACCEPTED_NOOP_VALUED: &[&str] = &[
    "--anchored=",
    "--submodule=",
    "--diff-merges=",
    "-l",
    "--break-rewrites=",
    "--find-renames=",
];

/// Real git flags whose effect on the output we do not produce. `--find-object`,
/// `-O` and `--output=` were formerly here and are now implemented.
const KNOWN_UNSUPPORTED: &[&str] = &[];

/// Prefixes of real git flags in the same category as [`KNOWN_UNSUPPORTED`].
const KNOWN_UNSUPPORTED_VALUED: &[&str] = &[];

fn classify(s: &str, opts: &mut Opts, quiet: &mut bool) -> Result<Flag, Fatal> {
    // `--color-moved[=<mode>]`, `--color-moved-ws=<modes>`, `--word-diff[=<mode>]`,
    // `--word-diff-regex=<re>` and `--color-words[=<re>]`.
    {
        let Opts { move_word, color_when, .. } = opts;
        if let Some(res) = move_word.parse_flag(s, color_when) {
            res.map_err(Fatal::OptionError)?;
            return Ok(Flag::Handled);
        }
    }
    match s {
        "--raw" => opts.fmt |= F_RAW,
        "--name-only" => opts.fmt |= F_NAME,
        "--name-status" => opts.fmt |= F_NAME_STATUS,
        "-p" | "-u" | "--patch" => opts.fmt |= F_PATCH,
        // `--binary` turns patch output on and additionally replaces a binary pair's
        // `Binary files … differ` line with the base85 payload.
        "--binary" => {
            opts.fmt |= F_PATCH;
            opts.binary = true;
        }
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
        // `OPT_BITOP('s', "no-patch", …, NO_OUTPUT, all-other-formats)`: sets
        // NO_OUTPUT and clears every other output-format bit, in argument order.
        "-s" | "--no-patch" => {
            opts.fmt &= !F_ALL_FORMATS;
            opts.fmt |= F_NO_OUTPUT;
        }
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
        // `--color[=<when>]` / `--no-color` (`OPT_COLOR_FLAG`).
        "--color" => opts.color_when = Some(diff_color::ColorWhen::Always),
        "--no-color" => opts.color_when = Some(diff_color::ColorWhen::Never),
        "--pickaxe-all" => opts.pickaxe_all = true,
        "--pickaxe-regex" => opts.pickaxe_regex = true,
        "-w" | "--ignore-all-space" => opts.ws = Whitespace::IgnoreAll,
        "-b" | "--ignore-space-change" => opts.ws = Whitespace::IgnoreChange,
        "--ignore-space-at-eol" => opts.ws = Whitespace::IgnoreAtEol,
        "--ignore-cr-at-eol" => opts.ws = Whitespace::IgnoreCrAtEol,
        "-C" | "--find-copies" | "--find-copies-harder" => opts.find_copies = true,
        // `-c`/`--cc`: request a combined diff and, per `common_setup()`, imply the
        // patch format when nothing else is asked for. `--cc` also densifies.
        "-c" => {
            opts.combine_merges = true;
            opts.dense_combined = false;
            opts.merges_need_diff = true;
        }
        "--cc" => {
            opts.combine_merges = true;
            opts.dense_combined = true;
            opts.merges_need_diff = true;
        }
        "--full-index" => opts.full_index = true,
        "-0" => {
            opts.unmerged_stage = 0;
            opts.explicit_stage = true;
        }
        "-1" | "--base" => {
            opts.unmerged_stage = 1;
            opts.explicit_stage = true;
        }
        "-2" | "--ours" => {
            opts.unmerged_stage = 2;
            opts.explicit_stage = true;
        }
        "-3" | "--theirs" => {
            opts.unmerged_stage = 3;
            opts.explicit_stage = true;
        }
        // Diff-algorithm aliases. Each sets the CLI algorithm, which overrides the
        // `diff.algorithm` config default at the diff site, so the last algorithm flag
        // on the line wins.
        "--minimal" => opts.algorithm = Some(Algorithm::MyersMinimal),
        "--histogram" => opts.algorithm = Some(Algorithm::Histogram),
        "--patience" => opts.algorithm = Some(Algorithm::Patience),
        // `XDF_INDENT_HEURISTIC`: where a hunk that can slide freely finally lands.
        "--indent-heuristic" => opts.indent_heuristic = true,
        "--no-indent-heuristic" => opts.indent_heuristic = false,
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
        // `revision.c`'s `--abbrev`, not `parse_opt_abbrev_cb`: `setup_revisions()`
        // claims the option before `diff_opt_parse()` ever sees it and reads the
        // value with `strtoul(optarg, NULL, 10)`, which cannot fail. So a value
        // with no digits is 0 and every value is clamped to `[MINIMUM_ABBREV,
        // hexsz]` — `git diff-files --abbrev=abc` prints 4-character ids, it does
        // not complain. (Commands that reach `OPT__ABBREV` directly, such as
        // `cherry`, do reject it; see `crate::abbrev::parse_opt_abbrev_value`.)
        opts.abbrev = Some(Some(crate::abbrev::parse_abbrev_arg(n, 40)));
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
            _ => return Err(Fatal::BadIgnoreSubmodules(v.to_owned())),
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
            // `diff_opt_char()` rejects only a value longer than one byte, and
            // then with its own `error:` line rather than the usage block. The
            // empty value is legal: it stores NUL, which prints as nothing.
            let name = lead.trim_start_matches('-').trim_end_matches('=');
            crate::diffopt::check(name, Some(v))
                .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
            let c = v.as_bytes().first().copied().unwrap_or(0);
            match slot {
                0 => opts.ind_new = c,
                1 => opts.ind_old = c,
                _ => opts.ind_ctx = c,
            }
            return Ok(Flag::Handled);
        }
    }
    // parse-options `OPT_COLOR_FLAG` accepts only a case-insensitive `always`,
    // `auto`, `never` and the boolean spellings `true`/`false`; any other value
    // (including empty) is a usage error with git's own message and exit 129.
    if let Some(v) = s.strip_prefix("--color=") {
        match diff_color::parse_color_when(v) {
            Some(w) => opts.color_when = Some(w),
            None => return Err(Fatal::ColorValue),
        }
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--find-copies=") {
        crate::diffopt::check_rename_score("find-copies", v)
            .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
        opts.find_copies = true;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--find-renames=") {
        crate::diffopt::check_rename_score("find-renames", v)
            .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--break-rewrites=") {
        crate::diffopt::check_break_rewrites(v)
            .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--diff-algorithm=") {
        match v {
            "myers" | "default" => opts.algorithm = Some(Algorithm::Myers),
            "minimal" => opts.algorithm = Some(Algorithm::MyersMinimal),
            "histogram" => opts.algorithm = Some(Algorithm::Histogram),
            "patience" => opts.algorithm = Some(Algorithm::Patience),
            _ => return Err(Fatal::Usage),
        }
        return Ok(Flag::Handled);
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
        // Finalized after the line is read, since `--pickaxe-regex` may still follow.
        opts.pickaxe_kinds |= PICKAXE_KIND_S;
        opts.pickaxe_pending = Some((b'S', v.as_bytes().to_vec()));
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("-G") {
        // `-G` is always a regex; it is compiled in the finalize pass so a bad pattern
        // is reported after every earlier argument, as git's `diffcore_pickaxe` does.
        opts.pickaxe_kinds |= PICKAXE_KIND_G;
        opts.pickaxe_pending = Some((b'G', v.as_bytes().to_vec()));
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--find-object=") {
        opts.pickaxe_kinds |= PICKAXE_KIND_OBJFIND;
        opts.find_object_args.push(v.to_owned());
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("-O") {
        if v.is_empty() {
            return Err(Fatal::MissingArgument("-O"));
        }
        opts.order_file = Some(v.to_owned());
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("--output=") {
        opts.output_file = Some(v.to_owned());
        return Ok(Flag::Handled);
    }
    // `-B<n>`, `-M<n>`, `-C<n>`: the score itself is irrelevant without renames,
    // but `parse_rename_score()` must still consume the whole value or the
    // callback reports it — with the *long* name, since that is `opt->long_name`.
    if let Some(v) = s.strip_prefix("-C") {
        crate::diffopt::check_rename_score("find-copies", v)
            .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
        opts.find_copies = true;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("-M") {
        crate::diffopt::check_rename_score("find-renames", v)
            .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
        return Ok(Flag::Handled);
    }
    if let Some(v) = s.strip_prefix("-B") {
        crate::diffopt::check_break_rewrites(v)
            .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
        return Ok(Flag::Handled);
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
    // `--inter-hunk-context=<n>` is an `OPT_MAGNITUDE` (`xecfg.interhunkctxlen`): two
    // change groups closer than `2 * ctxlen + interhunk` records land in one hunk. git
    // validates the number first and rejects a bad one at 129, so that check runs before
    // the value is taken; zero is git's own default and changes nothing.
    if let Some(v) = s.strip_prefix("--inter-hunk-context=") {
        opts.inter_hunk_ctx = validate_magnitude(v, "inter-hunk-context")? as usize;
        return Ok(Flag::Handled);
    }
    // `--ws-error-highlight=<kinds>`: a comma list drawn from old/new/context/all/
    // default/none, deciding which sides get whitespace-error markup.
    if let Some(v) = s.strip_prefix("--ws-error-highlight=") {
        opts.ws_error_highlight = parse_ws_error_highlight_opt(v)?;
        return Ok(Flag::Handled);
    }
    // `--submodule=<format>` cannot change this port's output — a gitlink pair is
    // reported the same way for all three formats here — but `diff_opt_submodule()`
    // still rejects a name that is not one of them, at parse time and before any
    // path is looked at. So the value is checked even though it is then ignored.
    if let Some(v) = s.strip_prefix("--submodule=") {
        crate::diffopt::check("submodule", Some(v))
            .map_err(|msg| Fatal::OptionError(format!("error: {msg}")))?;
        return Ok(Flag::Handled);
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
    // `diff_opt_ignore_regex` compiles inline, so a bad pattern is git's
    // `error: invalid regex given to -I: '<pat>'` (exit 129) at this argv position.
    match compile_regex(value.as_bytes()) {
        Ok(re) => {
            opts.ignore_lines.push(Needle::Regex(re));
            if opts.ignore_flag.is_none() {
                opts.ignore_flag = Some(flag.to_owned());
            }
            Ok(Flag::Handled)
        }
        Err(_) => Err(Fatal::InvalidRegexIgnore(value.to_owned())),
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

/// `parse_dirstat_opt()` (diff.c:5454): fold one `--dirstat=<param>,…` list into `ds`,
/// dying with the complaint `parse_dirstat_params()` accumulated.
fn parse_dirstat_spec(v: &str, ds: &mut DirStat) -> Result<(), Fatal> {
    let errors = parse_dirstat_params(v, ds);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Fatal::DirStatParams(errors))
    }
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

/// `OPT_UNSIGNED`'s value, through the shared `parse-options` grammar: base 0
/// (so `0x10` is sixteen and `010` is eight), an optional leading `+`, and one
/// optional `k`/`m`/`g` suffix. The target is a C `int`, which is what makes the
/// range clause read `[0,4294967295]`.
///
/// `None` for anything that does not match, which every caller turns into git's 129.
pub(crate) fn parse_magnitude(v: &str) -> Option<u64> {
    crate::optint::unsigned_prec(&crate::optint::long_opt(""), v, 4).ok()
}

/// `OPT_UNSIGNED` validation. The empty value, the unreadable one and the
/// out-of-range one each carry their own message.
fn validate_magnitude(v: &str, opt: &'static str) -> Result<u64, Fatal> {
    use crate::optint::IntError;
    match crate::optint::unsigned_prec(&crate::optint::long_opt(opt), v, 4) {
        Ok(n) => Ok(n),
        Err(IntError::Empty(_)) => Err(Fatal::Magnitude(opt)),
        Err(IntError::NotANumber(_)) => Err(Fatal::BadMagnitude(opt)),
        Err(IntError::OutOfRange(m)) => Err(Fatal::OptionError(format!("error: {m}"))),
    }
}

/// How many times `needle` occurs in `hay`, without overlaps.
fn count_occurrences_of(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let mut n = 0usize;
    let mut i = 0usize;
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

/// `diff_opt_ws_error_highlight()`: parse the comma list, turning git's negative
/// return — the length of the value it had already accepted — into the message
/// tail it prints.
fn parse_ws_error_highlight_opt(v: &str) -> Result<u32, Fatal> {
    diff_color::parse_ws_error_highlight(v)
        .map_err(|accepted| Fatal::WsErrorHighlight(v[..accepted].to_owned()))
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
        .ok_or_else(|| crate::fatal::need_work_tree())?
        .to_owned();
    let hash_kind = repo.object_hash();
    // `--color[=<when>]` / `--no-color`, falling back to `color.diff` /
    // `diff.color` / `color.ui` and the terminal test.
    let colors =
        diff_color::DiffColors::resolve(repo, diff_color::resolve_color(repo, opts.color_when));
    let ws_rule = diff_color::whitespace_rule_cfg(repo);
    let extra = match opts.move_word.resolve(repo) {
        Ok(e) => e,
        Err(msg) => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{msg}");
            return Ok(ExitCode::from(128));
        }
    };
    let (mut deltas, mut combined) = collect(repo, paths, &opts)?;

    // git emits index order, which for these records is a byte-wise path sort
    // with a conflict's `U` line kept ahead of its stage-2 comparison.
    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));
    combined.sort_by(|a, b| a.path.cmp(&b.path));

    // `-O<file>` (`diffcore_order`): stably reorder the queue so pairs whose path
    // matches an earlier pattern in the order file come first. This runs on the
    // repository-root relative path, before `--relative` strips anything and before
    // `--rotate-to`/`--skip-to` re-anchor, matching git's `diff_flush` order.
    if let Some(of) = &opts.order_file {
        let order = read_order_file(of);
        deltas.sort_by_cached_key(|d| match_order(&order, d.path.as_slice()));
        combined.sort_by_cached_key(|c| match_order(&order, c.path.as_slice()));
    }

    if opts.reverse {
        for d in &mut deltas {
            reverse_delta(d);
        }
    }

    // Rotation runs before `--relative` strips anything: `git diff-files
    // --relative=src --rotate-to=src/lib.rs` succeeds while `--rotate-to=lib.rs`
    // fails, so the anchor always names the repository-root relative path.
    //
    // `diffcore_rotate()` opens with `if (!q->nr) return;`, so an empty diff is not
    // a missing path: `git diff-files --skip-to=src/lib.rs` on a clean worktree
    // prints nothing and exits 0 rather than dying, even though nothing in the diff
    // is named `src/lib.rs`.
    match &opts.anchor {
        _ if deltas.is_empty() && combined.is_empty() => {}
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
    apply_relative_combined(repo, &mut combined, &opts.relative)?;

    // Content is needed by every non-raw format, by the whitespace family's
    // pruning, and by the `-S`/`-G` pickaxe. `--find-object` reads only the recorded
    // object ids (git's objfind never populates a filespec), so it needs no content.
    let pickaxe_needs_content = matches!(
        opts.pickaxe.as_ref().map(|p| &p.kind),
        Some(PickaxeKind::Occurrences(_) | PickaxeKind::Grep(_))
    );
    let want_content = opts.fmt & (F_CONTENT | F_CHECKDIFF) != 0
        || opts.diff_from_contents()
        || pickaxe_needs_content
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

    // `show_combined_diff()` runs inline during the index scan, so a combined
    // path's output leads the section the queue then fills. It emits at most one
    // form per path: a `::`-prefixed raw/name record when a raw-ish format is on,
    // otherwise a `diff --cc` patch.
    let combine_raw = opts.fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0;
    let mut combined_patch: Vec<u8> = Vec::new();
    if !combined.is_empty() {
        if combine_raw {
            for c in &combined {
                render_combined_raw(&mut out, repo, c, &opts);
            }
            separator = true;
        } else if opts.fmt & F_PATCH != 0 {
            for c in &combined {
                render_combined_patch(&mut combined_patch, repo, c, &opts, &workdir)?;
            }
        }
    }

    if !deltas.is_empty() {
        if opts.fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0 {
            out.extend_from_slice(&render_raw(repo, &deltas, &opts));
            separator = true;
        }
        if opts.fmt & F_CHECKDIFF != 0 {
            let pairs: Vec<CheckPair<'_>> = deltas
                .iter()
                .zip(&analyses)
                .map(|(d, an)| CheckPair {
                    checkable: !d.unmerged && d.new_valid() && !an.binary,
                    path: &d.path,
                    old_data: &an.old_data,
                    new_data: &an.new_data,
                    hunks: an.hunks.as_deref(),
                })
                .collect();
            check_failed = render_check(&mut rest, &pairs, ws_rule, &colors);
        }

        let dirstat_by_line = opts.fmt & F_DIRSTAT != 0 && opts.dirstat.by_line;
        if opts.fmt & (F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT) != 0 || dirstat_by_line {
            let stats = compute_diffstat(&deltas, &analyses, &opts);
            if opts.fmt & F_NUMSTAT != 0 {
                render_numstat(&mut rest, &stats, &opts);
            }
            if opts.fmt & F_DIFFSTAT != 0 {
                render_stat(&mut rest, &stats, &opts, &colors);
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
    }

    if opts.fmt & F_PATCH != 0 && (!combined_patch.is_empty() || !deltas.is_empty()) {
        if separator {
            rest.push(b'\n');
        }
        // The whole patch is assembled uncolored first, then re-emitted in one pass
        // through git's `fn_out_consume()` chain with each file pair's whitespace
        // state — `diff_flush_patch_all_file_pairs()`'s ordering, which is what lets
        // `--color-moved` and `--word-diff` see every pair at once. The combined
        // (`--cc`) sections carry no per-pair pre/post images, so they go through the
        // default state, exactly as `check_blank_at_eof()` leaves it when the two
        // sides were never compared.
        let paint_opts = diff_color::PaintOptions {
            ws_error_highlight: opts.ws_error_highlight,
            indicators: (opts.ind_new, opts.ind_old, opts.ind_ctx),
            // `diff.suppressBlankEmpty` is not read by this module, so the sign of
            // an empty context line is always kept, as git's default does.
            suppress_blank_empty: false,
        };
        let mut plain = combined_patch.clone();
        // Every `diff --cc` section ahead of the first ordinary pair consumes a slot,
        // so the per-file states line up with the headers the re-emitter counts.
        let combined_sections =
            count_occurrences_of(&combined_patch, b"\ndiff --cc ") + usize::from(combined_patch.starts_with(b"diff --cc "));
        let mut files: Vec<diff_color::FilePaint> =
            vec![diff_color::FilePaint::new(ws_rule); combined_sections];
        // `fill_metainfo()`'s abbreviation length (diff.c:4915):
        //     int abbrev = o->abbrev ? o->abbrev : DEFAULT_ABBREV;
        //     if (o->flags.full_index) abbrev = hexsz;
        // so `--full-index` wins outright, an explicit `--abbrev=<n>` (already clamped
        // to `[MINIMUM_ABBREV, hexsz]` by the parser) is used verbatim, and every other
        // spelling — no flag, a bare `--abbrev`, `--no-abbrev` — leaves `o->abbrev` at 0
        // and falls back to `DEFAULT_ABBREV`, which `core.abbrev` sets. `--no-abbrev`
        // widens only the raw listing, never this line.
        let hexsz = repo.object_hash().len_in_hex();
        // `--binary`'s payload is deflated at git's `zlib_compression_level`; read it
        // once rather than per file.
        let zlib_level = super::binary_patch::loose_compression_level(repo);
        let patch_abbrev = match (opts.full_index, opts.abbrev) {
            (true, _) => hexsz,
            (false, Some(Some(n))) => n.clamp(crate::abbrev::MINIMUM_ABBREV, hexsz),
            (false, _) => crate::abbrev::configured_abbrev(repo, hexsz),
        };
        for (d, an) in deltas.iter().zip(&analyses) {
            let before = plain.len();
            render_patch(&mut plain, d, an, &opts, patch_abbrev, zlib_level);
            if plain.len() != before {
                files.push(diff_color::FilePaint {
                    ws_rule,
                    blank_at_eof: diff_color::check_blank_at_eof(&an.old_data, &an.new_data),
                });
            }
        }
        rest.extend_from_slice(&diff_color::colorize_patch_ex(
            &plain,
            &colors,
            &paint_opts,
            &files,
            diff_color::FilePaint::new(ws_rule),
            &extra,
        ));
    }

    if !opts.line_prefix.is_empty() {
        rest = prefix_lines(&rest, &opts.line_prefix);
    }
    out.extend_from_slice(&rest);

    // `--output=<file>` points git's output `FILE*` at a file; every rendered byte
    // goes there instead of stdout, while the exit status is still computed below.
    match &opts.output_file {
        Some(path) => std::fs::write(path, &out)?,
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(&out)?;
            stdout.flush()?;
        }
    }

    // `diff_result_code()`: bit 0 is `--exit-code`, bit 1 is `--check`.
    let mut code = 0u8;
    let has_changes = !combined.is_empty()
        || if opts.diff_from_contents() {
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

/// `--relative[=<p>]` for the combined-diff paths, mirroring [`apply_relative`].
fn apply_relative_combined(
    repo: &gix::Repository,
    combined: &mut Vec<CombinedPath>,
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
    combined.retain_mut(
        |c| match c.path.strip_prefix(needle.as_slice()).map(|r| r.to_vec()) {
            Some(rest) => {
                c.path = rest.into();
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

/// `has_changes()` for `-S`, `diff_grep()` for `-G`, and `pickaxe_match()`'s objfind
/// branch for `--find-object`.
fn pickaxe_hit(px: &Pickaxe, d: &Delta, an: &Analysis) -> bool {
    // `DIFF_PICKAXE_KIND_OBJFIND`: a pair matches when either valid side's recorded
    // object id is in the set. The worktree side's id is left null at this stage (git
    // never hashes it for objfind), so in practice only the staged side can match.
    if let PickaxeKind::ObjFind(ids) = &px.kind {
        return (d.old_valid() && ids.contains(&d.src_id)) || (d.new_valid() && ids.contains(&d.dst_id));
    }
    if !d.old_valid() && !d.new_valid() {
        return false;
    }
    match &px.kind {
        PickaxeKind::Occurrences(needle) => {
            if let Needle::Literal(n) = needle {
                if n.is_empty() {
                    return false;
                }
            }
            let old = if d.old_valid() { needle.count(&an.old_data) } else { 0 };
            let new = if d.new_valid() { needle.count(&an.new_data) } else { 0 };
            match (d.old_valid(), d.new_valid()) {
                (false, true) => new != 0,
                (true, false) => old != 0,
                _ => old != new,
            }
        }
        PickaxeKind::Grep(needle) => {
            // With one side missing, git greps the whole surviving blob; otherwise
            // only the added and removed lines are examined.
            if !d.old_valid() {
                return needle.is_match(&an.new_data);
            }
            if !d.new_valid() {
                return needle.is_match(&an.old_data);
            }
            match &an.hunks {
                None => false,
                Some(h) => byte_lines(h).iter().any(|l| {
                    matches!(l.first().copied(), Some(b'+') | Some(b'-')) && needle.is_match(&l[1..])
                }),
            }
        }
        PickaxeKind::ObjFind(_) => unreachable!("objfind handled above"),
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

/// `prepare_order()`: read the order file into a list of glob patterns. Blank lines
/// and lines beginning with `#` are skipped; a leading `\#` is an escaped literal `#`.
/// git silently proceeds with no patterns when the file cannot be read.
pub(crate) fn read_order_file(path: &str) -> Vec<Vec<u8>> {
    let data = std::fs::read(path).unwrap_or_default();
    let mut order = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let pat = if line.starts_with(b"\\#") { &line[1..] } else { line };
        order.push(pat.to_vec());
    }
    order
}

/// `match_order()`: the index of the first order-file pattern that matches `path`,
/// or `order.len()` when none does. git matches the full path, then repeatedly strips
/// the trailing `/component` and retries so a pattern can name a parent directory.
pub(crate) fn match_order(order: &[Vec<u8>], path: &[u8]) -> usize {
    use gix::glob::wildmatch::Mode;
    for (i, pat) in order.iter().enumerate() {
        let mut p = path;
        loop {
            if gix::glob::wildmatch(pat.as_bstr(), p.as_bstr(), Mode::empty()) {
                return i;
            }
            match p.iter().rposition(|&b| b == b'/') {
                Some(idx) => p = &p[..idx],
                None => break,
            }
        }
    }
    order.len()
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

/// The blob comparator this module drives gix with: `FastEq`, which reports a change
/// only when the content really differs.
///
/// gix consults a comparator in the two cases where the cheap stat check did not settle
/// the entry, and `ie_match_stat()` treats them differently:
///
/// * the stat data genuinely differs (a bare `touch`) — `ce_match_stat_basic()` already
///   returned non-zero, git never opens the file, and `diff-files`, which does not
///   refresh the index, reports the path as modified.
/// * the stat data matches but the entry is *racy* (its mtime is at or after the index
///   timestamp), so the stat comparison cannot be trusted. git falls through to
///   `ce_modified_check_fs()`, which hashes the worktree file and reports the path only
///   when the hash differs.
///
/// gix calls the comparator identically in both, so the split is made afterwards: a
/// content-equal entry comes back as `EntryStatus::NeedsUpdate(new_stat)`, and
/// [`Collector::visit_entry`] re-runs the stat comparison on that payload to tell the
/// racily-clean entry (drop it) from the merely touched one (report `M`).
type StatOnly = gix::status::plumbing::index_as_worktree::traits::FastEq;

/// Accumulates one or two [`Delta`]s per visited index entry.
struct Collector<'a> {
    /// `core.trustCTime`/`core.checkStat`, for re-testing a `NeedsUpdate` payload.
    stat_opts: gix::index::entry::stat::Options,
    workdir: &'a Path,
    executable_bit: bool,
    null: ObjectId,
    /// `-0`/`-1`/`-2`/`-3`: which conflict stage the second record compares.
    unmerged_stage: u8,
    /// `revs->combine_merges`: an unmerged path with both stage #2 and stage #3 is
    /// diverted to [`Collector::combined`] instead of producing a `U` marker plus a
    /// two-way record.
    combine: bool,
    deltas: Vec<Delta>,
    combined: Vec<CombinedPath>,
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

                // `run_diff_files()`: when `combine_merges` is on and both stage #2
                // and stage #3 survive (`num_compare_stages == 2`), the whole path
                // is shown as one combined diff and never enters the ordinary queue.
                if self.combine {
                    if let (Some(s2), Some(s3)) = (entries[1].as_ref(), entries[2].as_ref()) {
                        self.combined.push(CombinedPath {
                            path,
                            parents: [(s2.id, s2.mode.bits()), (s3.id, s3.mode.bits())],
                            wt_mode,
                        });
                        return;
                    }
                }

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
            // gix emits this when the content comparison proved the entry clean even
            // though the cheap check did not settle it, which covers two of git's
            // cases at once. `ie_match_stat()` separates them by *why* it had to look:
            //
            // * the stat data matched and the entry was merely racy — git's
            //   `ce_modified_check_fs()` ran, found the content equal, and reported
            //   nothing. Dropping the entry is right.
            // * the stat data really differs (a bare `touch`) — `ce_match_stat_basic()`
            //   already returned non-zero, git never opened the file, and `diff-files`,
            //   which does not refresh the index, reports `M` with a null worktree id.
            //
            // The new stat is in hand here, so the same comparison decides it.
            EntryStatus::NeedsUpdate(new_stat) => {
                if new_stat.matches(&entry.stat, self.stat_opts) {
                    return;
                }
                Delta {
                    src_mode,
                    dst_mode: src_mode,
                    src_id: entry.id,
                    dst_id: self.null,
                    status: b'M',
                    path: path.clone(),
                    disk: path,
                    unmerged: false,
                }
            }
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

/// Run the index↔worktree stat comparison and reduce every entry to [`Delta`]s,
/// diverting combined unmerged paths into a separate list.
fn collect(
    repo: &gix::Repository,
    patterns: Vec<BString>,
    opts: &Opts,
) -> Result<(Vec<Delta>, Vec<CombinedPath>)> {
    let index = repo.index_or_empty()?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| crate::fatal::need_work_tree())?
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
        stat_opts: repo.stat_options()?,
        workdir: workdir.as_path(),
        executable_bit: caps.executable_bit,
        null: ObjectId::null(repo.object_hash()),
        unmerged_stage: opts.unmerged_stage,
        combine: opts.combine_merges,
        deltas: Vec::new(),
        combined: Vec::new(),
    };
    let mut progress = gix::progress::Discard;
    let should_interrupt = AtomicBool::new(false);

    repo.index_worktree_status(
        &index,
        patterns,
        &mut collector,
        gix::status::plumbing::index_as_worktree::traits::FastEq,
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

    Ok((collector.deltas, collector.combined))
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
        Operation::SourceOrDestinationIsBinary => {
            // The blob pipeline hands back only the *size* of content it classified as
            // binary, so `prep.*.data` is empty here. `--binary` needs the real bytes,
            // and only `--binary` does, so they are read back just for it.
            let (old_data, new_data) = if opts.binary {
                let staged = if blob_mode == 0 {
                    Vec::new()
                } else {
                    let mut buf = Vec::new();
                    use gix::prelude::FindExt;
                    objects.find_blob(&blob_id, &mut buf)?;
                    buf
                };
                let worktree = if wt_mode == 0 {
                    Vec::new()
                } else {
                    std::fs::read(workdir.join(gix::path::from_bstr(path))).unwrap_or_default()
                };
                if swapped {
                    (worktree, staged)
                } else {
                    (staged, worktree)
                }
            } else {
                (old_data, new_data)
            };
            Ok(Analysis {
                src_id,
                dst_id,
                added: 0,
                deleted: 0,
                binary: true,
                hunks: None,
                old_data,
                new_data,
                changed: old_mode == 0 || new_mode == 0 || mode_changed || src_id != dst_id,
            })
        }
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

            // An explicit `--diff-algorithm=`/`--minimal`/`--histogram` on the command
            // line overrides the `diff.algorithm` config default gix resolved into
            // `algorithm` (git precedence: flag beats config). `xdl_change_compact()`
            // scores `xdf->recs[i]->ptr`, the *original* record, so the indents come
            // from `before`/`after` rather than the normalized interner.
            let diff = super::diff_pairs::compute_compacted(
                opts.algorithm.unwrap_or(algorithm),
                &input,
                &before,
                &after,
                opts.indent_heuristic,
            );
            // `xdl_mark_ignorable_regex()`: a change group whose every removed and added
            // line matches an `-I` pattern is marked ignorable, which keeps it out of
            // the counts and stops `xdl_get_hunk()` from opening a hunk for it.
            let changes: Vec<super::diff_pairs::Change> = diff
                .hunks()
                .map(|h| {
                    let ignore = !opts.ignore_lines.is_empty()
                        && h.before
                            .clone()
                            .all(|i| matches_any(&opts.ignore_lines, before[i as usize]))
                        && h.after
                            .clone()
                            .all(|i| matches_any(&opts.ignore_lines, after[i as usize]));
                    super::diff_pairs::Change {
                        i1: h.before.start as usize,
                        chg1: h.before.len(),
                        i2: h.after.start as usize,
                        chg2: h.after.len(),
                        ignore,
                    }
                })
                .collect();
            let (added, deleted) = changes.iter().filter(|c| !c.ignore).fold(
                (0u32, 0u32),
                |(a, d), c| (a + c.chg2 as u32, d + c.chg1 as u32),
            );
            let hunks = if want_patch && (added != 0 || deleted != 0) {
                // The `xdl_emit_diff` port `git diff`, `diff-pairs` and `--no-index`
                // already share, so `--inter-hunk-context=<n>` merges hunks the same way
                // in every one of them instead of each writer inventing its own geometry.
                let (_, _, buf) = super::diff_pairs::emit_unified(
                    &before,
                    &after,
                    &changes,
                    &super::diff_pairs::EmitGeometry {
                        ctx: opts.ctx as usize,
                        inter_hunk_ctx: opts.inter_hunk_ctx,
                        func_context: false,
                    },
                );
                Some(buf)
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
fn render_stat(
    out: &mut Vec<u8>,
    files: &[StatFile],
    opts: &Opts,
    colors: &diff_color::DiffColors,
) {
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
            // `show_stats()` paints the two byte counts with the old/new colors.
            out.push(b' ');
            diff_color::paint(out, colors, diff_color::DiffSlot::Old, deleted.to_string().as_bytes());
            out.extend_from_slice(b" -> ");
            diff_color::paint(out, colors, diff_color::DiffSlot::New, added.to_string().as_bytes());
            out.extend_from_slice(b" bytes\n");
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
        // `show_graph()`: each run carries its own color and emits nothing at all
        // when it is empty.
        if add > 0 {
            diff_color::paint(out, colors, diff_color::DiffSlot::New, &b"+".repeat(add as usize));
        }
        if del > 0 {
            diff_color::paint(out, colors, diff_color::DiffSlot::Old, &b"-".repeat(del as usize));
        }
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

/// `builtin_checkdiff()` (diff.c:4281) driving `checkdiff_consume()` (diff.c:3555),
/// under `core.whitespace`. Returns `o->flags.check_failed`, which is
/// `diff_result_code()`'s bit 1.
///
/// The hunk stream it walks is the one the command already computed, so unlike stock
/// git — which runs a private `xecfg.ctxlen = 1`, `xpp.flags = 0` diff for the check —
/// `-U<n>` and the whitespace-ignoring options do reach it here.
/// One pair as `--check` needs to see it, so the walk below can serve every command
/// that has a pair list and a hunk stream rather than being tied to one module's types.
pub(crate) struct CheckPair<'a> {
    /// Skipped entirely when false: an unmerged record, a deletion, or a binary pair.
    pub(crate) checkable: bool,
    pub(crate) path: &'a BString,
    pub(crate) old_data: &'a [u8],
    pub(crate) new_data: &'a [u8],
    /// The rendered unified hunks; `None` when the pair produced none.
    pub(crate) hunks: Option<&'a [u8]>,
}

pub(crate) fn render_check(
    out: &mut Vec<u8>,
    pairs: &[CheckPair<'_>],
    ws_rule: u32,
    colors: &diff_color::DiffColors,
) -> bool {
    let mut failed = false;
    let set = colors.get(diff_color::DiffSlot::New);
    let ws_color = colors.get(diff_color::DiffSlot::Whitespace);
    let reset = colors.reset();
    for pair in pairs {
        if !pair.checkable {
            continue;
        }
        let name = quoted_name(pair.path);
        let new_lines = byte_lines(pair.new_data);
        // Only the added lines are checked: `--check` reports what the change
        // *introduces*, so the preimage is deliberately not examined.
        let Some(hunks) = pair.hunks else {
            continue;
        };
        let mut lineno = 0usize;
        // `checkdiff_consume()` remembers the previous record's marker so the
        // `\ No newline at end of file` line can tell which side it belongs to.
        let mut last_line_kind = 0u8;
        for line in byte_lines(hunks) {
            let kind = line.first().copied().unwrap_or(0);
            let previous_kind = last_line_kind;
            last_line_kind = kind;
            match line.first().copied() {
                Some(b'@') => {
                    lineno = hunk_new_start(line).saturating_sub(1);
                }
                Some(b' ') => lineno += 1,
                Some(b'\\') => {
                    // The incomplete last line, reported only when the record it
                    // follows was an added one.
                    if ws_rule & diff_color::WS_INCOMPLETE_LINE != 0 && previous_kind == b'+' {
                        failed = true;
                        out.extend_from_slice(&name);
                        out.extend_from_slice(
                            format!(
                                ":{lineno}: {}.\n",
                                whitespace_error_string(diff_color::WS_INCOMPLETE_LINE)
                            )
                            .as_bytes(),
                        );
                    }
                }
                Some(b'+') => {
                    lineno += 1;
                    let raw = &line[1..];
                    if is_conflict_marker(raw) {
                        failed = true;
                        out.extend_from_slice(&name);
                        out.extend_from_slice(
                            format!(":{lineno}: leftover conflict marker\n").as_bytes(),
                        );
                    }
                    let bad = ws_check(raw, ws_rule);
                    if bad == 0 {
                        continue;
                    }
                    failed = true;
                    out.extend_from_slice(&name);
                    out.extend_from_slice(
                        format!(":{lineno}: {}.\n", whitespace_error_string(bad)).as_bytes(),
                    );
                    // `emit_line(o, set, reset, line, 1)` prints the marker, then
                    // `ws_check_emit()` repaints the body around its offending runs.
                    out.extend_from_slice(set.as_bytes());
                    out.push(b'+');
                    out.extend_from_slice(reset.as_bytes());
                    diff_color::ws_check_emit(out, raw, ws_rule, set, reset, ws_color);
                }
                _ => {}
            }
        }
        if ws_rule & diff_color::WS_BLANK_AT_EOF != 0 {
            if let Some(at) = check_blank_at_eof(pair.old_data, &new_lines) {
                failed = true;
                out.extend_from_slice(&name);
                out.extend_from_slice(
                    format!(":{at}: {}.\n", whitespace_error_string(diff_color::WS_BLANK_AT_EOF))
                        .as_bytes(),
                );
            }
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
    is_conflict_marker_sized(line, DEFAULT_CONFLICT_MARKER_SIZE)
}

/// `ll_merge_marker_size()`'s default when no `conflict-marker-size` attribute
/// applies (`DEFAULT_CONFLICT_MARKER_SIZE`, merge-ll.h).
pub(crate) const DEFAULT_CONFLICT_MARKER_SIZE: usize = 7;

/// `is_conflict_marker()` (diff.c:3522): `marker_size` repeats of one of
/// `=`, `>`, `<`, `|` followed by a whitespace byte. The caller passes the line
/// *with* its terminator, which is what can satisfy the trailing-space test.
pub(crate) fn is_conflict_marker_sized(line: &[u8], marker_size: usize) -> bool {
    if line.len() < marker_size + 1 {
        return false;
    }
    let first = line[0];
    if !matches!(first, b'=' | b'>' | b'<' | b'|') {
        return false;
    }
    if line[1..marker_size].iter().any(|b| *b != first) {
        return false;
    }
    diff_color::is_c_space(line[marker_size])
}

/// `ws_check()` (ws.c:267) — `ws_check_emit_1()` run with no output stream, so it
/// only accumulates the `WS_*` bits the line violates under `ws_rule`. The caller
/// passes the line body *including* its terminator, exactly as `checkdiff_consume()`
/// hands `line + 1` to it.
pub(crate) fn ws_check(line: &[u8], ws_rule: u32) -> u32 {
    use diff_color::{
        is_c_space, ws_tab_width, WS_BLANK_AT_EOL, WS_CR_AT_EOL, WS_INCOMPLETE_LINE,
        WS_INDENT_WITH_NON_TAB, WS_SPACE_BEFORE_TAB, WS_TAB_IN_INDENT,
    };
    let mut result = 0u32;
    let mut len = line.len();
    let mut trailing_newline = false;

    // The logic is simpler with the trailing newline (and, under `cr-at-eol`, the
    // carriage return before it) temporarily out of the way.
    if len > 0 && line[len - 1] == b'\n' {
        trailing_newline = true;
        len -= 1;
    }
    if (ws_rule & WS_CR_AT_EOL) != 0 && len > 0 && line[len - 1] == b'\r' {
        len -= 1;
    }

    let mut trailing_whitespace: Option<usize> = None;
    if (ws_rule & WS_BLANK_AT_EOL) != 0 {
        for i in (0..len).rev() {
            if is_c_space(line[i]) {
                trailing_whitespace = Some(i);
                result |= WS_BLANK_AT_EOL;
            } else {
                break;
            }
        }
    }
    let trailing_whitespace = trailing_whitespace.unwrap_or(len);

    if !trailing_newline && (ws_rule & WS_INCOMPLETE_LINE) != 0 {
        result |= WS_INCOMPLETE_LINE;
    }

    // The indent: everything up to the first byte that is neither space nor tab.
    // git's chain is an `else if`, so `space-before-tab` masks `tab-in-indent` on a
    // tab that both rules would flag.
    let mut written = 0usize;
    let mut i = 0usize;
    while i < trailing_whitespace {
        if line[i] == b' ' {
            i += 1;
            continue;
        }
        if line[i] != b'\t' {
            break;
        }
        if (ws_rule & WS_SPACE_BEFORE_TAB) != 0 && written < i {
            result |= WS_SPACE_BEFORE_TAB;
        } else if (ws_rule & WS_TAB_IN_INDENT) != 0 {
            result |= WS_TAB_IN_INDENT;
        }
        written = i + 1;
        i += 1;
    }

    if (ws_rule & WS_INDENT_WITH_NON_TAB) != 0 && i - written >= ws_tab_width(ws_rule) {
        result |= WS_INDENT_WITH_NON_TAB;
    }
    result
}

/// `whitespace_error_string()` (ws.c:114): the comma-joined wording, in git's own
/// order, with `trailing-space` collapsing its two constituent bits into one phrase.
pub(crate) fn whitespace_error_string(ws: u32) -> String {
    use diff_color::{
        WS_BLANK_AT_EOF, WS_BLANK_AT_EOL, WS_INCOMPLETE_LINE, WS_INDENT_WITH_NON_TAB,
        WS_SPACE_BEFORE_TAB, WS_TAB_IN_INDENT, WS_TRAILING_SPACE,
    };
    let mut err = String::new();
    if ws & WS_TRAILING_SPACE == WS_TRAILING_SPACE {
        err.push_str("trailing whitespace");
    } else {
        if ws & WS_BLANK_AT_EOL != 0 {
            err.push_str("trailing whitespace");
        }
        if ws & WS_BLANK_AT_EOF != 0 {
            if !err.is_empty() {
                err.push_str(", ");
            }
            err.push_str("new blank line at EOF");
        }
    }
    for (bit, text) in [
        (WS_SPACE_BEFORE_TAB, "space before tab in indent"),
        (WS_INDENT_WITH_NON_TAB, "indent with spaces"),
        (WS_TAB_IN_INDENT, "tab in indent"),
        (WS_INCOMPLETE_LINE, "no newline at the end of file"),
    ] {
        if ws & bit != 0 {
            if !err.is_empty() {
                err.push_str(", ");
            }
            err.push_str(text);
        }
    }
    err
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
///
/// `hlen` is the `index` line's hex width: `fill_metainfo()` abbreviates with
/// `o->flags.full_index ? the_hash_algo->hexsz : DEFAULT_ABBREV`, and `DEFAULT_ABBREV`
/// is what `core.abbrev` sets — not a hardcoded 7.
fn render_patch(
    out: &mut Vec<u8>,
    d: &Delta,
    an: &Analysis,
    opts: &Opts,
    hlen: usize,
    zlib_level: i32,
) {
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

    // `fill_metainfo()` widens the `index` line to full object names under `--binary`,
    // but only for a pair that really is binary; text pairs keep the abbreviation.
    let hlen = if opts.binary && an.binary {
        an.src_id.kind().len_in_hex()
    } else {
        hlen
    };

    let old_hash = if d.old_valid() {
        an.src_id.to_hex_with_len(hlen).to_string()
    } else {
        "0".repeat(hlen)
    };
    let new_hash = if d.new_valid() {
        an.dst_id.to_hex_with_len(hlen).to_string()
    } else {
        "0".repeat(hlen)
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
        if opts.binary {
            // `emit_binary_diff()`: the payload replaces the whole `Binary files …`
            // line, and there is no `---`/`+++` pair in front of it.
            super::binary_patch::emit(out, &an.old_data, &an.new_data, zlib_level);
        } else {
            out.extend_from_slice(b"Binary files ");
            out.extend_from_slice(&old_label);
            out.extend_from_slice(b" and ");
            out.extend_from_slice(&new_label);
            out.extend_from_slice(b" differ\n");
        }
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
// combined diff (combine-diff.c)
//
// A faithful port of `combine_diff()`, `make_hunks()`/`give_context()`,
// `dump_sline()` and `show_combined_header()`/`show_raw_diff()`. It is driven only
// for an unmerged path that kept both stage #2 and stage #3, which is the sole way
// `git diff-files` reaches `show_combined_diff()`.
// ---------------------------------------------------------------------------

/// `struct lline`: a line lost from one or more parents relative to the result.
#[derive(Clone, Default)]
struct Lline {
    line: Vec<u8>,
    /// Bit `p` on = parent `p` had this line.
    parent_map: u64,
}

/// `struct sline`: one line surviving in the merge result, plus the lines lost
/// from the parents that hang in front of it.
#[derive(Default)]
struct Sline {
    /// Accumulated, coalesced lost lines (across parents processed so far).
    lost: Vec<Lline>,
    /// Lost lines from the parent currently being diffed, before coalescing.
    plost: Vec<Lline>,
    /// Offset and length of this line's bytes inside the result buffer; `len`
    /// excludes the trailing newline, exactly like `sline[lno].len` in git.
    bol: usize,
    len: usize,
    /// Bit `p` on = parent `p` differs here (a `+` for that column). Bit
    /// `num_parent` is the "interesting" mark; bit `num_parent+1` is `no_pre_delete`.
    flag: u64,
    /// First line number in each parent, per `combine_diff()`'s accounting.
    p_lno: Vec<u64>,
}

/// `append_lost()`: strip a trailing newline and record the line as lost from the
/// parent named by `mask`.
fn append_lost(sline: &mut Sline, line: &[u8], mask: u64) {
    let line = if line.last() == Some(&b'\n') {
        &line[..line.len() - 1]
    } else {
        line
    };
    sline.plost.push(Lline {
        line: line.to_vec(),
        parent_map: mask,
    });
}

/// Read one parent's blob for the combined diff, mirroring `grab_blob()`: a
/// gitlink becomes a `Subproject commit` stub and a null id an empty buffer.
fn read_parent_blob(repo: &gix::Repository, id: &ObjectId, mode: u32) -> Result<Vec<u8>> {
    if mode == 0o160000 {
        return Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes());
    }
    if id.is_null() {
        return Ok(Vec::new());
    }
    Ok(repo.find_object(*id)?.data.clone())
}

/// `buffer_is_binary()`: a NUL byte within the first 8000 bytes.
fn buffer_is_binary(buf: &[u8]) -> bool {
    let n = buf.len().min(8000);
    buf[..n].contains(&0)
}

/// `combine_diff()` for one parent: diff its blob against the result with zero
/// context, recording `+` marks on result lines and `-` (lost) lines, then assign
/// the per-parent line numbers.
fn combine_diff_parent(
    parent: &[u8],
    result: &[u8],
    sline: &mut [Sline],
    cnt: usize,
    n: usize,
    ws: Whitespace,
    indent_heuristic: bool,
) {
    let before = byte_lines(parent);
    let after = byte_lines(result);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| normalize(l, ws)));
    input.update_after(after.iter().map(|l| normalize(l, ws)));
    // git's combined diff runs xdiff with `xpp.flags = opt->xdl_opts`; Myers is
    // git's default algorithm. `xdl_change_compact()` measures the original records.
    let diff = super::diff_pairs::compute_compacted(
        Algorithm::Myers,
        &input,
        &before,
        &after,
        indent_heuristic,
    );
    let nmask = 1u64 << n;

    // With zero context every hunk is one raw change group. git hangs all of a
    // hunk's `-` lines on `sline[nb]` where `nb` is the result line the additions
    // begin at (`after.start`), and marks each `+` line's result position.
    for hunk in diff.hunks() {
        let bucket = hunk.after.start as usize;
        for bi in hunk.before.clone() {
            append_lost(&mut sline[bucket], before[bi as usize], nmask);
        }
        for aj in hunk.after.clone() {
            sline[aj as usize].flag |= nmask;
        }
    }

    // Assign this parent's line numbers, coalescing its lost lines into the
    // accumulated set as it goes.
    let mut p_lno = 1u64;
    for (lno, s) in sline.iter_mut().enumerate().take(cnt + 1) {
        s.p_lno[n] = p_lno;
        if !s.plost.is_empty() {
            let plost = std::mem::take(&mut s.plost);
            let base = std::mem::take(&mut s.lost);
            s.lost = coalesce_lines(base, plost, n, ws);
        }
        for ll in &s.lost {
            if ll.parent_map & nmask != 0 {
                p_lno += 1;
            }
        }
        if lno < cnt && s.flag & nmask == 0 {
            p_lno += 1;
        }
    }
    sline[cnt + 1].p_lno[n] = p_lno;
}

/// `reuse_combine_diff()`: parent `i` equals parent `j`, so copy `j`'s marks.
fn reuse_combine_diff(sline: &mut [Sline], cnt: usize, i: usize, j: usize) {
    let imask = 1u64 << i;
    let jmask = 1u64 << j;
    for s in sline.iter_mut().take(cnt + 1) {
        s.p_lno[i] = s.p_lno[j];
        for ll in &mut s.lost {
            if ll.parent_map & jmask != 0 {
                ll.parent_map |= imask;
            }
        }
        if s.flag & jmask != 0 {
            s.flag |= imask;
        }
    }
    sline[cnt + 1].p_lno[i] = sline[cnt + 1].p_lno[j];
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Coalesce {
    Match,
    Base,
    New,
}

/// `coalesce_lines()`: merge a parent's lost lines into the accumulated set by
/// LCS, so a line lost from several parents is shown once with all their columns.
fn coalesce_lines(base: Vec<Lline>, newlines: Vec<Lline>, parent: usize, ws: Whitespace) -> Vec<Lline> {
    if newlines.is_empty() {
        return base;
    }
    if base.is_empty() {
        return newlines;
    }
    let m = base.len();
    let k = newlines.len();
    let mut lcs = vec![vec![0i32; k + 1]; m + 1];
    let mut dir = vec![vec![Coalesce::Base; k + 1]; m + 1];
    for cell in dir[0][1..=k].iter_mut() {
        *cell = Coalesce::New;
    }
    for i in 1..=m {
        for j in 1..=k {
            if lines_equal(&base[i - 1].line, &newlines[j - 1].line, ws) {
                lcs[i][j] = lcs[i - 1][j - 1] + 1;
                dir[i][j] = Coalesce::Match;
            } else if lcs[i][j - 1] >= lcs[i - 1][j] {
                lcs[i][j] = lcs[i][j - 1];
                dir[i][j] = Coalesce::New;
            } else {
                lcs[i][j] = lcs[i - 1][j];
                dir[i][j] = Coalesce::Base;
            }
        }
    }
    let mut out: Vec<Lline> = Vec::with_capacity(m + k);
    let (mut i, mut j) = (m, k);
    while i != 0 || j != 0 {
        match dir[i][j] {
            Coalesce::Match => {
                let mut l = base[i - 1].clone();
                l.parent_map |= 1u64 << parent;
                out.push(l);
                i -= 1;
                j -= 1;
            }
            Coalesce::New => {
                out.push(newlines[j - 1].clone());
                j -= 1;
            }
            Coalesce::Base => {
                out.push(base[i - 1].clone());
                i -= 1;
            }
        }
    }
    out.reverse();
    out
}

/// `match_string_spaces()` reduced to what the whitespace flags require here.
fn lines_equal(a: &[u8], b: &[u8], ws: Whitespace) -> bool {
    normalize(a, ws) == normalize(b, ws)
}

/// `interesting()`.
fn sline_interesting(sl: &Sline, all_mask: u64) -> bool {
    sl.flag & all_mask != 0 || !sl.lost.is_empty()
}

/// `adjust_hunk_tail()`.
fn adjust_hunk_tail(sline: &[Sline], all_mask: u64, hunk_begin: usize, i: usize) -> usize {
    if hunk_begin < i && sline[i - 1].flag & all_mask == 0 {
        i - 1
    } else {
        i
    }
}

/// `find_next()`.
fn find_next(sline: &[Sline], mark: u64, mut i: usize, cnt: usize, look_for_uninteresting: bool) -> usize {
    while i <= cnt {
        let marked = sline[i].flag & mark != 0;
        if look_for_uninteresting == !marked {
            return i;
        }
        i += 1;
    }
    i
}

/// `give_context()`.
fn give_context(sline: &mut [Sline], cnt: usize, num_parent: usize, context: usize) -> bool {
    let all_mask = (1u64 << num_parent) - 1;
    let mark = 1u64 << num_parent;
    let no_pre_delete = 2u64 << num_parent;

    let mut i = find_next(sline, mark, 0, cnt, false);
    if cnt < i {
        return false;
    }

    while i <= cnt {
        let mut j = i.saturating_sub(context);
        while j < i {
            if sline[j].flag & mark == 0 {
                sline[j].flag |= no_pre_delete;
            }
            sline[j].flag |= mark;
            j += 1;
        }

        loop {
            // Where does the next uninteresting line start?
            let mut j = find_next(sline, mark, i, cnt, true);
            if cnt < j {
                return true;
            }
            let k = find_next(sline, mark, j, cnt, false);
            j = adjust_hunk_tail(sline, all_mask, i, j);

            if k < j + context {
                while j < k {
                    sline[j].flag |= mark;
                    j += 1;
                }
                i = k;
                continue;
            }

            i = k;
            let kk = if j + context < cnt + 1 { j + context } else { cnt + 1 };
            while j < kk {
                sline[j].flag |= mark;
                j += 1;
            }
            break;
        }
    }
    true
}

/// `make_hunks()`: mark interesting lines, thin single-parent hunks under `dense`,
/// and expand context.
fn make_hunks(sline: &mut [Sline], cnt: usize, num_parent: usize, dense: bool, context: usize) -> bool {
    let all_mask = (1u64 << num_parent) - 1;
    let mark = 1u64 << num_parent;

    for s in sline.iter_mut().take(cnt + 1) {
        if sline_interesting(s, all_mask) {
            s.flag |= mark;
        } else {
            s.flag &= !mark;
        }
    }
    if !dense {
        return give_context(sline, cnt, num_parent, context);
    }

    let mut i = 0usize;
    while i <= cnt {
        while i <= cnt && sline[i].flag & mark == 0 {
            i += 1;
        }
        if cnt < i {
            break;
        }
        let hunk_begin = i;
        let mut j = i + 1;
        while j <= cnt {
            if sline[j].flag & mark == 0 {
                let mut la = adjust_hunk_tail(sline, all_mask, hunk_begin, j);
                la = if la + context < cnt + 1 { la + context } else { cnt + 1 };
                let mut contin = false;
                while la > 0 && j < la {
                    la -= 1;
                    if sline[la].flag & mark != 0 {
                        contin = true;
                        break;
                    }
                }
                if !contin {
                    break;
                }
                j = la;
            }
            j += 1;
        }
        let hunk_end = j;

        // Is the hunk really interesting? Only when the set of parents the result
        // differs from is not the same on every line.
        let mut same_diff = 0u64;
        let mut has_interesting = false;
        let mut jj = i;
        while jj < hunk_end && !has_interesting {
            let this_diff = sline[jj].flag & all_mask;
            if this_diff != 0 {
                if same_diff == 0 {
                    same_diff = this_diff;
                } else if same_diff != this_diff {
                    has_interesting = true;
                    break;
                }
            }
            for ll in &sline[jj].lost {
                if has_interesting {
                    break;
                }
                let this_diff = ll.parent_map;
                if same_diff == 0 {
                    same_diff = this_diff;
                } else if same_diff != this_diff {
                    has_interesting = true;
                }
            }
            jj += 1;
        }

        if !has_interesting && same_diff != all_mask {
            for s in &mut sline[hunk_begin..hunk_end] {
                s.flag &= !mark;
            }
        }
        i = hunk_end;
    }

    give_context(sline, cnt, num_parent, context)
}

/// `show_parent_lno()`: the ` -<l0>,<len>` field for one parent in the hunk header.
fn show_parent_lno(out: &mut Vec<u8>, sline: &[Sline], l0: usize, l1: usize, n: usize, null_context: u64) {
    let a = sline[l0].p_lno[n];
    let b = sline[l1].p_lno[n];
    out.extend_from_slice(format!(" -{},{}", a, b - a - null_context).as_bytes());
}

/// `hunk_comment_line()`.
fn hunk_comment_line(sl: &Sline, result: &[u8]) -> bool {
    if sl.len == 0 {
        return false;
    }
    let ch = result[sl.bol];
    ch.is_ascii_alphabetic() || ch == b'_' || ch == b'$'
}

/// `dump_sline()`: render every combined hunk. `ind_*` carry the
/// `--output-indicator-*` overrides that the two-way writer also honors.
#[allow(clippy::too_many_arguments)]
fn dump_sline(
    out: &mut Vec<u8>,
    sline: &[Sline],
    cnt: usize,
    num_parent: usize,
    result: &[u8],
    result_deleted: bool,
    context: usize,
    line_prefix: &[u8],
) {
    let mark = 1u64 << num_parent;
    let no_pre_delete = 2u64 << num_parent;
    if result_deleted {
        return;
    }

    let mut lno = 0usize;
    loop {
        let mut hunk_comment: Option<usize> = None;
        while lno <= cnt && sline[lno].flag & mark == 0 {
            if hunk_comment_line(&sline[lno], result) {
                hunk_comment = Some(lno);
            }
            lno += 1;
        }
        if cnt < lno {
            break;
        }
        let mut hunk_end = lno + 1;
        while hunk_end <= cnt {
            if sline[hunk_end].flag & mark == 0 {
                break;
            }
            hunk_end += 1;
        }
        let mut rlines = (hunk_end - lno) as u64;
        if cnt < hunk_end {
            rlines -= 1; // pointing at the last delete hunk
        }

        let mut null_context = 0u64;
        if context == 0 {
            for s in &sline[lno..hunk_end] {
                if s.flag & (mark - 1) == 0 {
                    null_context += 1;
                }
            }
            rlines -= null_context;
        }

        out.extend_from_slice(line_prefix);
        for _ in 0..=num_parent {
            out.push(b'@');
        }
        for i in 0..num_parent {
            show_parent_lno(out, sline, lno, hunk_end, i, null_context);
        }
        out.extend_from_slice(format!(" +{},{} ", lno + 1, rlines).as_bytes());
        for _ in 0..=num_parent {
            out.push(b'@');
        }

        if let Some(hc) = hunk_comment {
            let bol = sline[hc].bol;
            let hcbytes = &result[bol..];
            let mut comment_end = 0usize;
            for i in 0..40 {
                let Some(&ch) = hcbytes.get(i) else { break };
                if ch == b'\n' {
                    break;
                }
                if !ch.is_ascii_whitespace() {
                    comment_end = i;
                }
            }
            if comment_end != 0 {
                out.extend_from_slice(b" ");
                out.extend_from_slice(&hcbytes[..comment_end]);
            }
        }
        out.push(b'\n');

        while lno < hunk_end {
            let sl = &sline[lno];
            lno += 1;
            let lost: &[Lline] = if sl.flag & no_pre_delete != 0 {
                &[]
            } else {
                &sl.lost
            };
            for ll in lost {
                out.extend_from_slice(line_prefix);
                for j in 0..num_parent {
                    out.push(if ll.parent_map & (1u64 << j) != 0 { b'-' } else { b' ' });
                }
                out.extend_from_slice(&ll.line);
                out.push(b'\n');
            }
            if cnt < lno {
                break;
            }
            out.extend_from_slice(line_prefix);
            if sl.flag & (mark - 1) == 0 {
                // Present in every parent: a context line only there to hang the
                // lost lines. Under --unified=0 it must not be printed at all.
                if context == 0 {
                    continue;
                }
            }
            let mut p_mask = 1u64;
            for _ in 0..num_parent {
                out.push(if p_mask & sl.flag != 0 { b'+' } else { b' ' });
                p_mask <<= 1;
            }
            out.extend_from_slice(&result[sl.bol..sl.bol + sl.len]);
            out.push(b'\n');
        }
    }
}

/// `repo_find_unique_abbrev()` for a combined header id: the shortest unique
/// prefix, but at least `min` characters, and exactly `min` zeros for the null id.
fn header_hex(repo: &gix::Repository, id: &ObjectId, min: Option<usize>) -> String {
    let Some(min) = min else {
        return id.to_hex().to_string(); // --full-index
    };
    if id.is_null() {
        return "0".repeat(min);
    }
    let uniq = id.attach(repo).shorten_or_id().hex_len();
    id.to_hex_with_len(uniq.max(min)).to_string()
}

/// The combined header's abbreviation floor: `--full-index` → full, otherwise
/// `core.abbrev` (git's `DEFAULT_ABBREV`), defaulting to 7 when unset.
fn header_abbrev(repo: &gix::Repository, opts: &Opts) -> Option<usize> {
    if opts.full_index {
        return None;
    }
    let n = repo
        .config_snapshot()
        .integer("core.abbrev")
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(7);
    Some(n.clamp(4, repo.object_hash().len_in_hex()))
}

/// `show_combined_header()` for a working-tree combined diff (`show_file_header`
/// is always set for diff-files). `line_prefix` is empty here because the caller
/// funnels this output through `prefix_lines()`, which stamps `--line-prefix` once.
fn show_combined_header(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    c: &CombinedPath,
    opts: &Opts,
    line_prefix: &[u8],
    show_file_header: bool,
) {
    let dense = opts.dense_combined;
    let a_prefix = opts.src_prefix.as_str();
    let b_prefix = opts.dst_prefix.as_str();
    let abbrev = header_abbrev(repo, opts);

    out.extend_from_slice(line_prefix);
    out.extend_from_slice(if dense { b"diff --cc " } else { b"diff --combined " });
    out.extend_from_slice(&quoted_name(&c.path));
    out.push(b'\n');

    out.extend_from_slice(line_prefix);
    out.extend_from_slice(b"index ");
    for (i, (id, _)) in c.parents.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        out.extend_from_slice(header_hex(repo, id, abbrev).as_bytes());
    }
    out.extend_from_slice(b"..");
    out.extend_from_slice(header_hex(repo, &repo.object_hash().null(), abbrev).as_bytes());
    out.push(b'\n');

    let mode_differs = c.parents.iter().any(|(_, m)| *m != c.wt_mode);
    let deleted = c.wt_mode == 0;
    // diff-files always records DIFF_STATUS_MODIFIED for both parents, so the
    // "added if nobody had it" branch never fires here.
    if mode_differs {
        out.extend_from_slice(line_prefix);
        if deleted {
            out.extend_from_slice(b"deleted file ");
        }
        out.extend_from_slice(b"mode ");
        for (i, (_, m)) in c.parents.iter().enumerate() {
            if i != 0 {
                out.push(b',');
            }
            out.extend_from_slice(format!("{m:06o}").as_bytes());
        }
        if c.wt_mode != 0 {
            out.extend_from_slice(format!("..{:06o}", c.wt_mode).as_bytes());
        }
        out.push(b'\n');
    }

    if !show_file_header {
        return;
    }

    // `dump_quoted_path()`: the `--- `/`+++ ` head sits outside the quoted unit,
    // and `quote_two_c_style(prefix, path)` quotes prefix+path together. diff-files
    // never records an added parent, so the `---` side always names a/<path>.
    out.extend_from_slice(line_prefix);
    out.extend_from_slice(b"--- ");
    out.extend_from_slice(&quote_one(a_prefix, &c.path));
    out.push(b'\n');

    out.extend_from_slice(line_prefix);
    if deleted {
        out.extend_from_slice(b"+++ /dev/null");
    } else {
        out.extend_from_slice(b"+++ ");
        out.extend_from_slice(&quote_one(b_prefix, &c.path));
    }
    out.push(b'\n');
}

/// The full `diff --cc` section for one combined path.
fn render_combined_patch(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    c: &CombinedPath,
    opts: &Opts,
    workdir: &Path,
) -> Result<()> {
    const NUM_PARENT: usize = 2;

    let full = workdir.join(gix::path::from_bstr(c.path.as_bstr()));
    let (result, result_deleted) = match std::fs::read(&full) {
        Ok(b) => (b, false),
        Err(_) => (Vec::new(), true),
    };

    let parents: Vec<Vec<u8>> = c
        .parents
        .iter()
        .map(|(id, m)| read_parent_blob(repo, id, *m))
        .collect::<Result<_>>()?;

    let is_binary = buffer_is_binary(&result) || parents.iter().any(|p| buffer_is_binary(p));
    if is_binary {
        show_combined_header(out, repo, c, opts, b"", false);
        out.extend_from_slice(b"Binary files differ\n");
        return Ok(());
    }

    // Split the result into lines; `len` excludes the newline, and a final line
    // without one is still a line, mirroring show_patch_diff()'s sline setup.
    let mut cnt = result.iter().filter(|&&b| b == b'\n').count();
    if !result.is_empty() && *result.last().unwrap() != b'\n' {
        cnt += 1;
    }
    let mut sline: Vec<Sline> = (0..cnt + 2)
        .map(|_| Sline {
            p_lno: vec![0; NUM_PARENT],
            ..Sline::default()
        })
        .collect();
    if cnt > 0 {
        let mut lno = 0usize;
        for (i, &b) in result.iter().enumerate() {
            if b == b'\n' {
                sline[lno].len = i - sline[lno].bol;
                lno += 1;
                if lno < cnt {
                    sline[lno].bol = i + 1;
                }
            }
        }
        if *result.last().unwrap() != b'\n' {
            sline[cnt - 1].len = result.len() - sline[cnt - 1].bol;
        }
    }

    // `n` indexes both `c.parents` and `parents`, bounds the inner `0..n`, and is
    // passed by value to the combine helpers — not a plain single-slice index.
    #[allow(clippy::needless_range_loop)]
    for n in 0..NUM_PARENT {
        let mut reused = false;
        for j in 0..n {
            if c.parents[n].0 == c.parents[j].0 {
                reuse_combine_diff(&mut sline, cnt, n, j);
                reused = true;
                break;
            }
        }
        if !reused {
            combine_diff_parent(
                &parents[n],
                &result,
                &mut sline,
                cnt,
                n,
                opts.ws,
                opts.indent_heuristic,
            );
        }
    }

    // working_tree_file is always true for diff-files, so the header and body are
    // shown regardless of make_hunks()'s verdict.
    let _ = make_hunks(&mut sline, cnt, NUM_PARENT, opts.dense_combined, opts.ctx as usize);
    show_combined_header(out, repo, c, opts, b"", true);
    dump_sline(
        out,
        &sline,
        cnt,
        NUM_PARENT,
        &result,
        result_deleted,
        opts.ctx as usize,
        b"",
    );
    Ok(())
}

/// `show_raw_diff()`: the `::`-prefixed combined raw record, or the combined
/// name / name-status line.
fn render_combined_raw(out: &mut Vec<u8>, repo: &gix::Repository, c: &CombinedPath, opts: &Opts) {
    let (sep, term): (u8, u8) = if opts.nul { (0, 0) } else { (b'\t', b'\n') };
    let null = repo.object_hash().null();

    out.extend_from_slice(&opts.line_prefix);
    if opts.fmt & F_RAW != 0 {
        for _ in &c.parents {
            out.push(b':');
        }
        for (_, m) in &c.parents {
            out.extend_from_slice(format!("{m:06o} ").as_bytes());
        }
        out.extend_from_slice(format!("{:06o}", c.wt_mode).as_bytes());
        for (id, _) in &c.parents {
            out.push(b' ');
            out.extend_from_slice(combined_raw_hex(repo, id, opts).as_bytes());
        }
        out.push(b' ');
        out.extend_from_slice(combined_raw_hex(repo, &null, opts).as_bytes());
        out.push(b' ');
    }
    if opts.fmt & (F_RAW | F_NAME_STATUS) != 0 {
        // Both parents are DIFF_STATUS_MODIFIED for diff-files.
        for _ in &c.parents {
            out.push(b'M');
        }
        out.push(sep);
    }
    if opts.nul {
        out.extend_from_slice(c.path.as_ref());
    } else {
        out.extend_from_slice(&quoted_name(&c.path));
    }
    out.push(term);
}

/// `diff_aligned_abbrev()` for a combined raw id: `diff-files` sets `rev.abbrev`
/// to 0, so the default is the full id; `--abbrev=<n>` shortens it.
fn combined_raw_hex(repo: &gix::Repository, id: &ObjectId, opts: &Opts) -> String {
    let hexsz = repo.object_hash().len_in_hex();
    match opts.abbrev {
        None => id.to_hex().to_string(),
        Some(Some(n)) => id.to_hex_with_len(n.clamp(4, hexsz)).to_string(),
        Some(None) => {
            // Bare `--abbrev` follows core.abbrev / the unique prefix.
            if id.is_null() {
                let n = repo
                    .config_snapshot()
                    .integer("core.abbrev")
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(7)
                    .clamp(4, hexsz);
                "0".repeat(n)
            } else {
                let uniq = id.attach(repo).shorten_or_id().hex_len();
                let floor = repo
                    .config_snapshot()
                    .integer("core.abbrev")
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(7);
                id.to_hex_with_len(uniq.max(floor).clamp(4, hexsz)).to_string()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// path quoting (quote.c)
// ---------------------------------------------------------------------------

/// git's `quote_path_fully` global, seeded from `core.quotePath` (default true) by
/// [`init_quote_path`]. git keeps this in one place and every `quote_c_style()` caller
/// reads it, so the four raw/name emitters that share this module's quoting share the
/// setting too.
static QUOTE_PATH_FULLY: AtomicBool = AtomicBool::new(true);

/// Seed [`QUOTE_PATH_FULLY`] from `core.quotePath`, git's `git_default_core_config()`.
/// Call once, right after the repository is open and before anything is rendered.
pub(crate) fn init_quote_path(repo: &gix::Repository) {
    let on = repo
        .config_snapshot()
        .boolean("core.quotePath")
        .unwrap_or(true);
    QUOTE_PATH_FULLY.store(on, atomic::Ordering::Relaxed);
}

/// The escape character for `b`, or `None` if it can be emitted verbatim.
/// `Some(0)` means "octal-escape this byte".
///
/// This is git's `cq_lookup[]` table combined with `cq_must_quote()`: entries the table
/// marks `-1` are never quoted, the named escapes and `"`/`\` are always quoted (their
/// table entries are `>= ' '`, so `quote_path_fully` cannot switch them off), controls
/// and DEL are always octal-escaped, and the high half (table entry `0`) is octal-escaped
/// only while `quote_path_fully` is on.
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
        // Table entry 1: quoted whatever `core.quotePath` says.
        0x00..=0x1f | 0x7f => Some(0),
        // Table entry 0: quoted only while `quote_path_fully` is on.
        0x80..=0xff => QUOTE_PATH_FULLY
            .load(atomic::Ordering::Relaxed)
            .then_some(0),
        _ => None,
    }
}

/// `quote_c_style(s, NULL, NULL, 0)` used as a predicate: whether quoting would
/// change the string at all.
pub(crate) fn needs_c_quote(s: &[u8]) -> bool {
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
pub(crate) fn quoted_name(path: &BString) -> Vec<u8> {
    quoted_name_bytes(path.as_slice())
}

/// [`quoted_name`] over a plain byte slice, for the callers that never hold a `BString`.
pub(crate) fn quoted_name_bytes(s: &[u8]) -> Vec<u8> {
    if !needs_c_quote(s) {
        return s.to_vec();
    }
    let mut out = vec![b'"'];
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// `quote_two_c_style()` for a single prefixed name (the `---`/`+++` lines).
pub(crate) fn quote_one(prefix: &str, path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_c_quote(prefix.as_bytes()) && !needs_c_quote(s) {
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
pub(crate) fn quote_two(pa: &str, a: &BString, pb: &str, b: &BString) -> Vec<u8> {
    let mut out = quote_one(pa, a);
    out.push(b' ');
    out.extend_from_slice(&quote_one(pb, b));
    out
}
