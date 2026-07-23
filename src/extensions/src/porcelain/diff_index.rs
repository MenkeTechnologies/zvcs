//! `git diff-index` — compare a tree object against the working tree or the index.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The pair list is produced by
//! a direct port of git's `oneway_diff` (`diff-lib.c`): the tree is flattened, the index
//! is grouped by path, and every path in the union of the two is resolved into at most
//! one raw record. Against the working tree the destination side comes from `lstat` via
//! git's `ce_match_stat_basic`/`match_stat_data` rules, which is why a merely *touched*
//! file — same bytes, new inode or ctime — is reported as `M` with the null object id,
//! exactly as stock git reports it.
//!
//! Supported invocations (stdout is byte-identical to stock `git diff-index`):
//!
//!   * `git diff-index <tree-ish>`      — the default raw format:
//!     `:<srcmode> <dstmode> <srcsha> <dstsha> <status>\t<path>`.
//!   * `--cached`                       — compare `<tree-ish>` against the index only.
//!   * `--merge-base`                   — diff against `merge-base(HEAD, <commit>)`.
//!   * `-m`                             — treat files missing from the worktree as up
//!     to date instead of reporting them deleted.
//!   * `--raw`, `--name-only`, `--name-status` — output selection.
//!   * `-z`                             — NUL field/record terminators, paths unquoted.
//!   * `--abbrev[=<n>]`, `--no-abbrev`, `--full-index` — abbreviated / full object ids.
//!   * `--exit-code`, `--quiet`         — exit 1 when differences exist (`--quiet` is silent).
//!   * `-s` / `--no-patch`              — suppress output, exit 0 unless `--exit-code`.
//!   * `-R`                             — swap the two sides of every pair.
//!   * `--diff-filter=<letters>`        — include upper-case, exclude lower-case statuses.
//!   * `--line-prefix=<s>`              — prefix every emitted record.
//!   * `--relative[=<path>]`, `--no-relative` — limit to a subdirectory and strip it.
//!   * `-w`, `-b`, `--ignore-all-space`, `--ignore-space-change`,
//!     `--ignore-space-at-eol`, `--ignore-cr-at-eol`, `-I<s>`/`--ignore-matching-lines=<s>`
//!     — content comparison: pairs whose contents match once the requested folding is
//!     applied are dropped, and the surviving worktree side is hashed so the real object
//!     id shows up in the raw record instead of the null id, as git does.
//!   * `-S<s>`, `-G<s>`, `--pickaxe-all`, `--pickaxe-regex` — the pickaxe filters. `-S`
//!     is a literal kwset search; `-G`, `-I` and `-S --pickaxe-regex` are compiled with
//!     `regex::bytes` (Unicode off, byte semantics) to mirror git's `regcomp`.
//!   * `--dirstat[=<params>]` / `-X[<params>]`, `--dirstat-by-file[=<params>]`,
//!     `--cumulative` — the per-directory damage listing. Damage is scored by git's
//!     `diffcore_count_changes()` (shared with `diff-files`), by file, or by changed
//!     line count, and rendered through the same `gather_dirstat()` walk. Like git,
//!     `--dirstat` on its own replaces the raw listing, while `--raw --dirstat`
//!     prints both, and `--name-only`, `--name-status` and `-s` suppress it entirely.
//!   * `-p`/`-u`/`--patch`, `-U<n>`/`--unified=<n>`, `--patch-with-raw`,
//!     `--patch-with-stat` — the unified patch body (`builtin_diff()`), with git's
//!     `diff --git`/`index`/mode-change/`--- +++`/`@@` framing and the `\ No newline at
//!     end of file` marker. Context defaults to 3 and follows `-U<n>`.
//!   * `--stat[=<w>[,<n>[,<c>]]]`, `--stat-width=`, `--stat-name-width=`,
//!     `--stat-graph-width=`, `--stat-count=`, `--compact-summary`, `--numstat`,
//!     `--shortstat`, `--summary` — the diffstat, numeric stat, short stat and the
//!     create/delete/mode-change summary, all byte-identical to git.
//!   * `[--] <path>...`                 — pathspec limiting, resolved relative to the cwd
//!     while output paths stay repository-root relative, as git does. Positionals are
//!     resolved the way `setup_revisions` does: the first that names an object is the
//!     tree-ish, a second object is an extra revision (diff-index takes exactly one, so
//!     two or more exit 129 with the usage text), and once a positional is accepted as a
//!     path every later one must exist on disk. Without a `--` separator a mistyped
//!     revision that is neither object nor path exits 128 with the `ambiguous argument`
//!     text rather than silently matching nothing.
//!
//! Status letters produced: `A`, `D`, `T` (the `S_IFMT` bits of the two modes differ,
//! e.g. file ↔ symlink), `M`, and `U` for unmerged paths under `--cached`.
//!
//! Options that only steer colour or patch/stat *shaping* (`--color` bare,
//! `--ws-error-highlight=`, `--diff-algorithm=`, `--anchored=`, `--color-moved[=]`,
//! `--word-diff` bare, `--ignore-submodules` bare, `--ignore-blank-lines`, `-B`,
//! `-l<n>`, `-a`/`--text`, `-W`, …) are accepted and ignored for the raw, `--name-only`
//! and `--name-status` listings — stock git's bytes there are identical with and without
//! them. The full list is `render_only_option`. Because this module does not *port* those
//! shapers, a run that also asks for a content format (a patch or a stat) is declined
//! rather than rendering bytes that would diverge from git; the prefix family
//! (`--src-prefix=`/`--dst-prefix=`/`--no-prefix`/`--default-prefix`), `--full-index` and
//! `-D`/`--irreversible-delete` are the shapers this module does honour, so they compose
//! with the patch instead.
//!
//! A handful of options carry a value git validates during its single left-to-right
//! parse, so this module validates it too and reproduces git's exact code and message at
//! the option's argv position (a bad revision earlier in argv still wins first):
//!
//!   * `--submodule=<v>` — only `short|log|diff`; else exit 129
//!     `error: failed to parse --submodule option parameter: '<v>'`.
//!   * `--color=<when>` — only `always|auto|never` (case-insensitive); else exit 129
//!     ``error: option `color' expects "always", "auto", or "never"``.
//!   * `--word-diff=<mode>` — only `plain|color|porcelain|none`; else exit 129
//!     `error: bad --word-diff argument: <mode>`.
//!   * `--ignore-submodules=<v>` — only `none|untracked|dirty|all`; else exit 128
//!     `fatal: bad --ignore-submodules argument: <v>`.
//!   * `--skip-to=<path>` / `--rotate-to=<path>` — git reorders the queued pairs so
//!     output starts at `<path>` (skip drops the earlier pairs, rotate wraps them to the
//!     end); a `<path>` naming no queued pair is exit 128
//!     `fatal: No such path '<path>' in the diff`, but only for a non-empty diff.
//!
//! Patch and stat rendering is produced for real: `-p`/`-u`/`--patch`,
//! `-U<n>`/`--unified=<n>`, `--patch-with-raw`, `--patch-with-stat`, `--stat[=<w>]`,
//! `--stat-*-width=`, `--stat-count=`, `--numstat`, `--shortstat`, `--summary` and
//! `--compact-summary` all render git's exact bytes, through the same `builtin_diff()`,
//! `compute_diffstat()`/`show_stats()` and `diff_summary()` ports `diff-files` uses.
//! Every content format participates in git's content pruning: a pair whose two sides
//! turn out identical (a stat-dirty-but-unchanged file) is dropped and the survivors are
//! given the destination id the patch machinery hashed, exactly as git does.
//!
//! ### Honest limitations (bailed on with a precise message, never faked)
//!
//! * `--check` (whitespace-error report) and `--binary` (the base85 `GIT binary patch`
//!   payload) are not produced. Both are content-driven in git, so when no pair survives
//!   the content comparison the correct output is nothing at all and that is what is
//!   emitted; a run that would have produced real bytes is refused, not approximated.
//! * Rename/copy detection is off, which is git's default for `diff-index`. `-M`/`-C`
//!   and friends are accepted for their *observable* side effect on this listing — git
//!   hashes rename candidates, so an added path gains its real object id — but no rename
//!   is ever *paired*: additions and deletions stay separate `A`/`D` records rather than
//!   collapsing into an `R`/`C` record. git's `diffcore_rename` (its exact-id and
//!   similarity matching, the basename heuristic and the reported `R<score>` percentage)
//!   is not vendored; gitoxide's `rewrites::Tracker` deliberately deviates from git's
//!   algorithm, so it cannot reproduce git's pairing or scores byte for byte, and every
//!   other command in this tree (`diff`, `whatchanged`, …) likewise leaves rewrites off.
//! * `-G`/`-I`/`-S --pickaxe-regex` compile with the `regex` crate, not the platform's
//!   POSIX engine, so a pattern the two engines disagree about (rare metacharacter edge
//!   cases) can match differently, and an *invalid* pattern's fatal carries a different
//!   message tail: `-I` reproduces git's `error: invalid regex given to -I: '<pat>'`
//!   (exit 129) byte for byte, but `-G`/`-S` keep git's `fatal: invalid regex: ` prefix
//!   and exit 128 while the tail is the `regex` crate's message rather than `regerror`'s.
//! * A locally modified but committed-clean submodule is reported as unchanged; git also
//!   inspects the submodule worktree and would report it.
//! * With a bare `--abbrev` and no `core.abbrev` set, the length comes from gitoxide's
//!   unique-prefix computation for the first real id (falling back to 7); git derives it
//!   from the packed object count, so the two can differ on large packed repositories.
//! * Magic (`:(...)`) and glob (`* ? [`) pathspecs are matched through gitoxide's pathspec
//!   engine (git's own algorithm); purely literal paths and directory prefixes stay on the
//!   simpler in-module matcher. A malformed magic pathspec is the one degraded path: it
//!   exits with the generic error text rather than git's specific `fatal`.
//! * An unimplemented option is held until after the tree-ish has been resolved, so a
//!   missing tree-ish still exits 129 with git's usage text and an unresolvable one still
//!   exits 128 with git's `ambiguous argument` text, as stock git does.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, Diff, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use regex::bytes::Regex;

use super::diff_files::{count_changes_sides, render_dirstat, DirStat};

/// The file-type bits of a mode, as in `<sys/stat.h>`.
const S_IFMT: u32 = 0o170000;

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

/// Which whitespace differences the content comparison should fold away.
#[derive(Clone, Copy, Default)]
struct Ws {
    /// `-w` / `--ignore-all-space`
    all: bool,
    /// `-b` / `--ignore-space-change`
    change: bool,
    /// `--ignore-space-at-eol`
    at_eol: bool,
    /// `--ignore-cr-at-eol`
    cr: bool,
}

impl Ws {
    fn any(self) -> bool {
        self.all || self.change || self.at_eol || self.cr
    }
}

/// The `--stat` geometry, in git's own `-1 == unset` encoding (`diff.c`'s
/// `stat_width`/`stat_name_width`/`stat_graph_width`/`stat_count`).
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

/// A search pattern, either a literal substring (git's kwset path for a plain `-S`) or a
/// compiled regular expression (git's `-G`, `-I`, and `-S --pickaxe-regex`, all of which
/// call `regcomp` with `REG_EXTENDED | REG_NEWLINE`).
enum Needle {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl Needle {
    /// Whether `hay` contains a match — used by `-G` on each changed line and by `-I`.
    fn is_match(&self, hay: &[u8]) -> bool {
        match self {
            Needle::Literal(n) => contains(hay, n),
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
/// bytes, without Unicode mode so `.` and the character classes carry git's C-locale byte
/// semantics, and with multi-line mode standing in for `REG_NEWLINE` since matching is
/// done a line at a time. `Err` carries the engine's message for the (best-effort) fatal.
fn compile_regex(pat: &[u8]) -> std::result::Result<Regex, String> {
    let s = std::str::from_utf8(pat).map_err(|_| "invalid byte sequence in pattern".to_owned())?;
    regex::bytes::RegexBuilder::new(s)
        .unicode(false)
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

/// The pickaxe: `-S` compares occurrence counts, `-G` greps the changed lines.
enum Pickaxe {
    Occurrences(Needle),
    Grep(Needle),
}

/// Parsed command-line options for a single `diff-index` invocation.
struct Opts {
    cached: bool,                  // --cached: compare against the index, ignore the worktree
    match_missing: bool,           // -m: files missing from the worktree count as up to date
    format: Format,
    nul: bool,                     // -z: NUL field/record terminators, no path quoting
    abbrev: Option<Option<usize>>, // --abbrev[=N]: None=full, Some(None)=auto, Some(Some(n))=N
    exit_code: bool,               // --exit-code/--quiet: exit 1 when anything differs
    reverse: bool,                 // -R: swap the two sides
    line_prefix: Vec<u8>,          // --line-prefix=<s>
    relative: Option<BString>,     // --relative[=<dir>], repository-root relative, no trailing '/'
    filter_include: Vec<u8>,       // --diff-filter upper-case letters
    filter_exclude: Vec<u8>,       // --diff-filter lower-case letters, upper-cased
    ws: Ws,
    ignore_lines: Option<Needle>, // -I<s> / --ignore-matching-lines=<s>
    pickaxe: Option<Pickaxe>,
    pickaxe_all: bool,
    detect_rename: bool, // -M/-C: git hashes rename candidates, so additions gain real ids
    /// `-p`/`-u`/`--patch`: render the unified patch body.
    patch: bool,
    /// `-U<n>`/`--unified=<n>`: unified-diff context, git's default of 3.
    ctx: u32,
    /// `--numstat`: the `<added>\t<deleted>\t<path>` machine listing.
    numstat: bool,
    /// `--stat[=…]`/`--compact-summary`: the human diffstat.
    diffstat: bool,
    /// `--shortstat`: only the ` N files changed, …` summary line.
    shortstat: bool,
    /// `--summary`: the create/delete/mode-change extended listing.
    summary: bool,
    /// `--stat` geometry (`--stat=<w>,<n>,<c>`, `--stat-*-width=`, `--compact-summary`).
    stat: StatWidths,
    /// `--full-index`: emit the full object name on the patch `index` line.
    full_index: bool,
    /// `--src-prefix=`/`--no-prefix`; `-R` swaps the two. git's defaults are `a/`/`b/`.
    src_prefix: String,
    /// `--dst-prefix=`/`--no-prefix`.
    dst_prefix: String,
    /// `-D`/`--irreversible-delete`: a deletion shows its header and nothing else.
    irreversible_delete: bool,
    /// `--dirstat`/`-X`/`--dirstat-by-file`/`--cumulative`, once any of them is seen.
    dirstat: Option<DirStat>,
    /// Whether the pair listing itself is printed. git defaults `output_format` to
    /// `DIFF_FORMAT_RAW` only when nothing else was asked for, so a bare `--dirstat`
    /// prints directories alone while `--raw --dirstat` prints both.
    emit_pairs: bool,
    /// `--skip-to=<path>` / `--rotate-to=<path>`: `(is_skip, path)`, last one wins.
    /// git reorders the queued pairs at flush time so output starts at `<path>`; skip
    /// drops everything before it, rotate wraps the earlier pairs to the end. A `<path>`
    /// that names no queued pair is fatal (`No such path '<path>' in the diff`, exit 128),
    /// but only when the queue is non-empty — an all-clean diff never validates it.
    skip_or_rotate: Option<(bool, BString)>,
}

/// One file-level change, already reduced to the columns git's raw format prints.
/// A mode of `0` means the side does not exist.
struct Delta {
    src_mode: u32,
    dst_mode: u32,
    src_id: ObjectId,
    dst_id: ObjectId,
    /// An unmerged (conflicted) index entry, reported as `U` under `--cached`.
    unmerged: bool,
    /// Repository-root relative path.
    path: BString,
}

impl Delta {
    /// git's `diff_resolve_rename_copy` letter: absent source is an addition, absent
    /// destination a deletion, differing `S_IFMT` bits a type change, otherwise a
    /// modification. Unmerged pairs short-circuit to `U`.
    fn status(&self) -> u8 {
        if self.unmerged {
            b'U'
        } else if self.src_mode == 0 {
            b'A'
        } else if self.dst_mode == 0 {
            b'D'
        } else if (self.src_mode & S_IFMT) != (self.dst_mode & S_IFMT) {
            b'T'
        } else {
            b'M'
        }
    }

    fn old_valid(&self) -> bool {
        self.src_mode != 0
    }

    fn new_valid(&self) -> bool {
        self.dst_mode != 0
    }
}

/// What the index knows about one path, with the stages collapsed the way git's
/// `oneway_diff` sees them: stage 2 wins when a path is unmerged, and the stat data of
/// an unmerged entry is all zeroes, which is what makes it always compare dirty.
struct IdxInfo {
    mode: u32,
    id: ObjectId,
    stat: gix::index::entry::Stat,
    intent_to_add: bool,
    unmerged: bool,
}

/// The flag list quoted back at the user when an unimplemented option shows up.
const PORTED: &str = "--cached, --merge-base, -m, --raw, --name-only, --name-status, -z, \
                      --abbrev[=<n>], --no-abbrev, --full-index, --exit-code, --quiet, \
                      -s/--no-patch, -R, --diff-filter=, --line-prefix=, --relative[=], \
                      -w/-b/--ignore-*-space*, -I, -S, -G, --dirstat[=], -X, \
                      --dirstat-by-file[=], --cumulative, -p/-u/--patch, -U<n>/--unified=, \
                      --patch-with-raw, --patch-with-stat, --stat[=], --numstat, \
                      --shortstat, --summary, --compact-summary";

/// Stock `git diff-index`'s usage text, reproduced byte for byte (including the
/// trailing blank line) because it is written to stderr on every usage error.
const USAGE: &str = r"usage: git diff-index [-m] [--cached] [--merge-base] [<common-diff-options>] <tree-ish> [<path>...]

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

/// Options that steer only patch, stat or colour rendering — never the raw,
/// `--name-only` or `--name-status` listings this module emits.
///
/// Each entry was checked against stock `git diff-index` by diffing the raw output with
/// and without the option, both in a repository whose only differences are stat-dirty
/// (so every pair has a null destination id) and in one with real additions, deletions
/// and modifications. All of them leave those bytes and the exit status untouched.
/// Deliberately absent: `-U<n>`, `--unified=<n>`, `--binary`, `--check` and the stat
/// family, which look like rendering knobs but replace the raw listing. The dirstat
/// family also replaces it and is handled for real, in `apply_dirstat`.
fn render_only_option(a: &str) -> bool {
    const EXACT: &[&str] = &[
        "-a",
        "-B",
        "-D",
        "-W",
        "--break-rewrites",
        "--color",
        "--color-moved",
        "--color-words",
        "--default-prefix",
        "--ext-diff",
        "--full-index",
        "--function-context",
        "--histogram",
        "--ignore-blank-lines",
        "--ignore-submodules",
        "--indent-heuristic",
        "--irreversible-delete",
        "--ita-invisible-in-index",
        "--ita-visible-in-index",
        "--minimal",
        "--no-color",
        "--no-color-moved",
        "--no-color-moved-ws",
        "--no-diff-merges",
        "--no-ext-diff",
        "--no-function-context",
        "--no-indent-heuristic",
        "--no-prefix",
        "--no-rename-empty",
        "--no-renames",
        "--no-textconv",
        "--patience",
        "--rename-empty",
        "--submodule",
        "--text",
        "--textconv",
        "--word-diff",
    ];
    // NB: the value-validated options `--color=`, `--word-diff=`, `--ignore-submodules=`,
    // `--submodule=`, `--skip-to=` and `--rotate-to=` are handled by dedicated arms in the
    // parse loop (they can fail), so they deliberately do *not* appear here.
    const WITH_VALUE: &[&str] = &[
        "--anchored=",
        "--break-rewrites=",
        "--color-moved=",
        "--color-moved-ws=",
        "--diff-algorithm=",
        "--diff-merges=",
        "--dst-prefix=",
        "--inter-hunk-context=",
        "--output-indicator-context=",
        "--output-indicator-new=",
        "--output-indicator-old=",
        "--src-prefix=",
        "--word-diff-regex=",
        "--ws-error-highlight=",
    ];
    if EXACT.contains(&a) || WITH_VALUE.iter().any(|p| a.starts_with(*p)) {
        return true;
    }
    // `-B<n>` / `-B<n>/<m>` (break rewrites) and `-l<n>` (rename limit) carry a numeric
    // tail; neither changes this listing.
    let b = a.as_bytes();
    b.len() > 2 && b[0] == b'-' && (b[1] == b'B' || b[1] == b'l')
}

/// The two content-derived renderings this module still declines: `--check`
/// (whitespace-error report) and `--binary` (the base85 `GIT binary patch` payload).
/// Both are content-driven in git, so an all-clean pair list renders as nothing and
/// only a run with surviving pairs is refused rather than approximated. The patch and
/// stat families, by contrast, are ported below and rendered for real.
fn unsupported_format(a: &str) -> bool {
    matches!(a, "--check" | "--binary")
}

/// `--stat=<width>[,<name-width>[,<count>]]` (`diff_opt_stat()`), parsed leniently:
/// each comma-separated field that is a valid integer updates its slot, anything else
/// is left at its prior (`-1`/`0`) value. git validates these during its option scan;
/// a malformed width there is exit 129, which this module does not reproduce.
fn parse_stat_spec(v: &str, stat: &mut StatWidths) {
    let mut it = v.split(',');
    if let Some(w) = it.next() {
        if let Ok(n) = w.parse() {
            stat.width = n;
        }
    }
    if let Some(n) = it.next() {
        if let Ok(v) = n.parse() {
            stat.name_width = v;
        }
    }
    if let Some(c) = it.next() {
        if let Ok(v) = c.parse() {
            stat.count = v;
        }
    }
}

/// The context count of `-U<n>`/`--unified=<n>`: git reads it with `strtol`, so leading
/// digits win and garbage yields the default of 3.
fn parse_context(s: &str) -> u32 {
    let digits: String = s.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(3)
}

/// git parses `--abbrev=<n>` with `strtoul(arg, NULL, 10)`, which never fails: it skips
/// leading whitespace and an optional sign, reads the leading decimal digits, yields `0`
/// when there are none, and wraps a negative value to a huge number. `abbrev_len` then
/// clamps the result into git's `[4, hash-length]` range, so garbage abbreviates to 4 and
/// a negative one prints the full id, exactly as stock git does.
fn git_abbrev(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let mut negative = false;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        negative = b[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut val: usize = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add(usize::from(b[i] - b'0'));
        i += 1;
    }
    if i == start {
        0
    } else if negative {
        usize::MAX
    } else {
        val
    }
}

/// Short options whose value may be written as a separate argument (`-S fn` as well as
/// `-Sfn`).
fn short_option_takes_value(a: &str) -> bool {
    matches!(a, "-S" | "-G" | "-I" | "-O" | "-U" | "-l")
}

/// Set `slot` to `cand` when `cand` sits earlier in argv than whatever is already held,
/// so a deferred option-value error fires at the position git's single left-to-right
/// parse would hit it first. Options parsed in the main loop arrive in argv order, but
/// `-I`'s regex is compiled after the loop and can still be the earliest error.
fn set_earliest(slot: &mut Option<(usize, u8, Vec<u8>)>, cand: (usize, u8, Vec<u8>)) {
    if slot.as_ref().is_none_or(|(i, _, _)| cand.0 < *i) {
        *slot = Some(cand);
    }
}

pub fn diff_index(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand at index 0; tolerate its absence so the entry
    // point behaves the same either way.
    let args = match args.first() {
        Some(first) if first == "diff-index" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        cached: false,
        match_missing: false,
        format: Format::Raw,
        nul: false,
        abbrev: None,
        exit_code: false,
        reverse: false,
        line_prefix: Vec::new(),
        relative: None,
        filter_include: Vec::new(),
        filter_exclude: Vec::new(),
        ws: Ws::default(),
        ignore_lines: None,
        pickaxe: None,
        pickaxe_all: false,
        detect_rename: false,
        patch: false,
        ctx: 3,
        numstat: false,
        diffstat: false,
        shortstat: false,
        summary: false,
        stat: StatWidths::default(),
        full_index: false,
        src_prefix: "a/".to_owned(),
        dst_prefix: "b/".to_owned(),
        irreversible_delete: false,
        dirstat: None,
        emit_pairs: true,
        skip_or_rotate: None,
    };
    let mut quiet = false;
    let mut merge_base = false;
    // `--raw` given explicitly, which is what makes git print the pair listing
    // alongside `--dirstat` instead of only the directories.
    let mut raw_explicit = false;
    // `-S`/`-G` share one slot (the last one wins, as in git); `-I` composes with them.
    let mut pickaxe_arg: Option<(u8, Vec<u8>)> = None;
    // `(argv index, pattern)`: the index lets a `-I` that fails to compile fire at exactly
    // its argv position, as git's inline `regcomp` does.
    let mut ignore_arg: Option<(usize, Vec<u8>)> = None;
    let mut pickaxe_regex = false;
    // Positionals given before a `--` separator, paired with their argv index. git's
    // `setup_revisions` resolves each against the object database; the first that
    // resolves is the tree-ish and the rest are extra revisions or pathspecs (see the
    // scan below). The index is kept so a deferred `--submodule=` parse error can fire
    // at exactly its argv position relative to these, as git's single left-to-right
    // pass does.
    let mut positionals: Vec<(usize, String)> = Vec::new();
    let mut paths: Vec<BString> = Vec::new();
    let mut after_dashdash = false;
    // The first option git understands but this module does not. Held back rather than
    // raised immediately: git parses the whole command line before it looks at the
    // tree-ish, so a missing or unresolvable revision still has to win, exactly as it
    // does in stock git, and only a run that would otherwise have produced output is
    // refused.
    let mut unsupported: Option<String> = None;
    // The first `--check`/`--binary` rendering asked for, which this module still
    // declines; the patch and stat families are parsed into `opts` and rendered.
    let mut bad_format: Option<String> = None;
    // An accepted-and-ignored option that would reshape the *content* rendering (patch
    // prefixes, diff algorithm, word-diff, forced colour). Harmless for the raw and name
    // listings — the only reason it can be ignored there — but it would make the ported
    // patch/stat bytes diverge from git, so a run that also asks for a content format is
    // refused rather than emitting the wrong bytes. git honours these; this module does
    // not port them, so honesty wins over coverage.
    let mut content_altering: Option<String> = None;
    // A `-G`/`-S --pickaxe-regex` pattern that failed to compile. git compiles these in
    // `diffcore_pickaxe`, after the tree-ish is resolved, and dies with
    // `fatal: invalid regex: <msg>` (exit 128); the message tail comes from the platform
    // regex engine, so only the prefix and exit code are reproduced byte for byte here.
    let mut bad_regex: Option<Vec<u8>> = None;
    // The first option whose *value* git rejects during its single left-to-right parse,
    // as `(argv index, exit code, exact stderr bytes)`. git validates such values inline
    // with `handle_revision_arg`, so a bad revision appearing *earlier* in argv dies first
    // (exit 128, `ambiguous argument`) while the same bad option appearing earlier wins.
    // Held with its argv index — rather than returned the moment the flag is seen — so the
    // positional scan can fire whichever error git's single pass would hit first. Covers
    // `--submodule=` (129), `--color=` (129), `--word-diff=` (129) and
    // `--ignore-submodules=` (128); `get_or_insert` keeps the earliest since the scan runs
    // left to right.
    let mut deferred: Option<(usize, u8, Vec<u8>)> = None;

    // git `die()`s on a bad dirstat parameter the moment it parses it, before it looks
    // at anything else on the command line, so each call site returns straight away.
    macro_rules! dirstat {
        ($params:expr) => {
            if let Some(code) = apply_dirstat(&mut opts, $params) {
                return Ok(code);
            }
        };
    }

    let mut i = 0;
    while i < args.len() {
        let cur = i;
        let a = args[i].as_str();
        i += 1;
        if after_dashdash {
            paths.push(a.into());
            continue;
        }
        match a {
            "--" => after_dashdash = true,
            "--cached" => opts.cached = true,
            "--merge-base" => merge_base = true,
            "-m" => opts.match_missing = true,
            "--raw" => {
                opts.format = Format::Raw;
                raw_explicit = true;
            }
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
            "-R" => opts.reverse = true,
            "-w" | "--ignore-all-space" => opts.ws.all = true,
            "-b" | "--ignore-space-change" => opts.ws.change = true,
            "--ignore-space-at-eol" => opts.ws.at_eol = true,
            "--ignore-cr-at-eol" => opts.ws.cr = true,
            "--pickaxe-all" => opts.pickaxe_all = true,
            "--pickaxe-regex" => pickaxe_regex = true,
            // `diff_opt_dirstat()`: `--cumulative` and `--dirstat-by-file` are spelled
            // as parameter lists, and every spelling also turns the format on.
            "--dirstat" | "-X" => dirstat!(""),
            "--cumulative" => dirstat!("cumulative"),
            "--dirstat-by-file" => {
                dirstat!("files");
                dirstat!("");
            }
            "--relative" => opts.relative = Some(BString::default()),
            "--no-relative" => opts.relative = None,
            // `diff_opt_*`: the patch and stat output formats. `--patch-with-raw` also
            // keeps the raw listing, `--patch-with-stat` also prepends the diffstat.
            "-p" | "-u" | "--patch" => opts.patch = true,
            "--patch-with-raw" => {
                opts.patch = true;
                raw_explicit = true;
            }
            "--patch-with-stat" => {
                opts.patch = true;
                opts.diffstat = true;
            }
            "--stat" => opts.diffstat = true,
            "--numstat" => opts.numstat = true,
            "--shortstat" => opts.shortstat = true,
            "--summary" => opts.summary = true,
            "--compact-summary" => {
                opts.diffstat = true;
                opts.stat.with_summary = true;
            }
            "-U" | "--unified" => {
                let Some(value) = args.get(i) else {
                    eprint!("{}", USAGE);
                    return Ok(ExitCode::from(129));
                };
                i += 1;
                opts.patch = true;
                opts.ctx = parse_context(value);
            }
            s if s.starts_with("--unified=") => {
                opts.patch = true;
                opts.ctx = parse_context(&s["--unified=".len()..]);
            }
            s if s.len() > 2 && s.starts_with("-U") => {
                opts.patch = true;
                opts.ctx = parse_context(&s[2..]);
            }
            s if s.starts_with("--stat=") => {
                parse_stat_spec(&s["--stat=".len()..], &mut opts.stat);
                opts.diffstat = true;
            }
            s if s.starts_with("--stat-width=") => {
                if let Ok(n) = s["--stat-width=".len()..].parse() {
                    opts.stat.width = n;
                }
                opts.diffstat = true;
            }
            s if s.starts_with("--stat-name-width=") => {
                if let Ok(n) = s["--stat-name-width=".len()..].parse() {
                    opts.stat.name_width = n;
                }
                opts.diffstat = true;
            }
            s if s.starts_with("--stat-graph-width=") => {
                if let Ok(n) = s["--stat-graph-width=".len()..].parse() {
                    opts.stat.graph_width = n;
                }
                opts.diffstat = true;
            }
            s if s.starts_with("--stat-count=") => {
                if let Ok(n) = s["--stat-count=".len()..].parse() {
                    opts.stat.count = n;
                }
                opts.diffstat = true;
            }
            // Patch-shaping options this module *does* honour, so they never trip the
            // content-altering refusal below.
            "--full-index" => opts.full_index = true,
            "-D" | "--irreversible-delete" => opts.irreversible_delete = true,
            "--no-prefix" => {
                opts.src_prefix = String::new();
                opts.dst_prefix = String::new();
            }
            "--default-prefix" => {
                opts.src_prefix = "a/".to_owned();
                opts.dst_prefix = "b/".to_owned();
            }
            s if s.starts_with("--src-prefix=") => {
                opts.src_prefix = s["--src-prefix=".len()..].to_owned();
            }
            s if s.starts_with("--dst-prefix=") => {
                opts.dst_prefix = s["--dst-prefix=".len()..].to_owned();
            }
            "-M" | "-C" | "--find-renames" | "--find-copies" | "--find-copies-harder" => {
                opts.detect_rename = true;
            }
            "-S" | "-G" | "-I" => {
                let Some(value) = args.get(i) else {
                    eprint!("{}", USAGE);
                    return Ok(ExitCode::from(129));
                };
                i += 1;
                if a == "-I" {
                    ignore_arg = Some((cur, value.as_bytes().to_vec()));
                } else {
                    pickaxe_arg = Some((a.as_bytes()[1], value.as_bytes().to_vec()));
                }
            }
            s if s.starts_with("--dirstat=") => dirstat!(&s["--dirstat=".len()..]),
            s if s.starts_with("--dirstat-by-file=") => {
                dirstat!("files");
                dirstat!(&s["--dirstat-by-file=".len()..]);
            }
            // `-X` takes its parameters attached only; a following argument is a
            // positional, which is why `-X 10 HEAD` makes git complain about `10`.
            s if s.len() > 2 && s.starts_with("-X") => dirstat!(&s[2..]),
            s if s.starts_with("--find-renames=") || s.starts_with("--find-copies=") => {
                opts.detect_rename = true;
            }
            s if s.starts_with("--relative=") => {
                opts.relative = Some(trim_slashes(&s["--relative=".len()..]));
            }
            s if s.starts_with("--line-prefix=") => {
                opts.line_prefix = s["--line-prefix=".len()..].as_bytes().to_vec();
            }
            s if s.starts_with("--ignore-matching-lines=") => {
                ignore_arg = Some((cur, s["--ignore-matching-lines=".len()..].as_bytes().to_vec()));
            }
            s if s.starts_with("--diff-filter=") => {
                // `diff_opt_diff_filter()` rejects an unknown letter inline during the
                // single left-to-right parse: `error: unknown change class '<c>' in
                // <arg>` (exit 129). Deferred with its argv index so a bad revision or
                // option-value error earlier in argv still wins first, as it does in git.
                let val = &s["--diff-filter=".len()..];
                if let Some(bad) = parse_filter(val, &mut opts) {
                    set_earliest(
                        &mut deferred,
                        (
                            cur,
                            129,
                            format!("error: unknown change class '{}' in {s}\n", bad as char).into_bytes(),
                        ),
                    );
                }
            }
            s if s.starts_with("--abbrev=") => {
                // git parses this with `strtoul`, which never fails; `abbrev_len`
                // clamps the result into `[4, hash-length]` afterwards.
                opts.abbrev = Some(Some(git_abbrev(&s["--abbrev=".len()..])));
            }
            s if s.starts_with("--submodule=") => {
                // `parse_submodule_params()`: only these three spellings are valid, and
                // git rejects anything else (exit 129). The error is deferred with its
                // argv index rather than raised now, so a bad revision earlier in argv
                // still wins with git's 128, matching git's single left-to-right parse.
                let val = &s["--submodule=".len()..];
                if !matches!(val, "short" | "log" | "diff") {
                    deferred.get_or_insert((
                        cur,
                        129,
                        format!("error: failed to parse --submodule option parameter: '{val}'\n").into_bytes(),
                    ));
                }
            }
            s if s.starts_with("--color=") => {
                // `OPT_COLOR_FLAG` → `git_config_colorbool`: `--color=<when>` accepts only
                // `always`, `auto` or `never` (case-insensitively); anything else, empty
                // included, is exit 129. Bare `--color` (below, `render_only_option`) means
                // `always` and is always accepted.
                let val = &s["--color=".len()..];
                match val.to_ascii_lowercase().as_str() {
                    "always" => {
                        // Forced colour would tint the patch/stat; refuse if one is asked.
                        content_altering.get_or_insert_with(|| s.to_owned());
                    }
                    "auto" | "never" => {}
                    _ => {
                        deferred.get_or_insert((
                            cur,
                            129,
                            b"error: option `color' expects \"always\", \"auto\", or \"never\"\n".to_vec(),
                        ));
                    }
                }
            }
            s if s.starts_with("--word-diff=") => {
                // `diff_opt_word_diff`: `--word-diff=<mode>` accepts only `plain`, `color`,
                // `porcelain` or `none` (case-sensitively); anything else, empty included,
                // is exit 129. Bare `--word-diff` means `plain` and is accepted above.
                let val = &s["--word-diff=".len()..];
                match val {
                    "plain" | "color" | "porcelain" => {
                        // Word diff replaces the line-oriented patch body entirely.
                        content_altering.get_or_insert_with(|| s.to_owned());
                    }
                    "none" => {}
                    _ => {
                        deferred.get_or_insert((
                            cur,
                            129,
                            format!("error: bad --word-diff argument: {val}\n").into_bytes(),
                        ));
                    }
                }
            }
            s if s.starts_with("--ignore-submodules=") => {
                // `parse_ignore_submodules_arg`: `--ignore-submodules=<value>` accepts only
                // `none`, `untracked`, `dirty` or `all` (case-sensitively); anything else,
                // empty included, is `fatal: bad --ignore-submodules argument: <value>`
                // (exit 128). Bare `--ignore-submodules` is accepted above.
                let val = &s["--ignore-submodules=".len()..];
                if !matches!(val, "none" | "untracked" | "dirty" | "all") {
                    deferred.get_or_insert((
                        cur,
                        128,
                        format!("fatal: bad --ignore-submodules argument: {val}\n").into_bytes(),
                    ));
                }
            }
            s if s.starts_with("--skip-to=") => {
                opts.skip_or_rotate = Some((true, s["--skip-to=".len()..].into()));
            }
            s if s.starts_with("--rotate-to=") => {
                opts.skip_or_rotate = Some((false, s["--rotate-to=".len()..].into()));
            }
            s if s.len() > 2 && s.starts_with("-I") => {
                ignore_arg = Some((cur, s[2..].as_bytes().to_vec()));
            }
            s if s.len() > 2 && (s.starts_with("-S") || s.starts_with("-G")) => {
                pickaxe_arg = Some((s.as_bytes()[1], s[2..].as_bytes().to_vec()));
            }
            s if s.len() > 2 && (s.starts_with("-M") || s.starts_with("-C")) => {
                opts.detect_rename = true;
            }
            s => {
                if unsupported_format(s) {
                    bad_format.get_or_insert_with(|| s.to_owned());
                } else if render_only_option(s) {
                    // Ignored for the raw/name listings (their bytes are identical with
                    // and without it); recorded so a content format refuses rather than
                    // rendering the wrong bytes.
                    content_altering.get_or_insert_with(|| s.to_owned());
                } else if s.starts_with('-') && s.len() > 1 {
                    if short_option_takes_value(s) {
                        i += 1;
                    }
                    unsupported.get_or_insert_with(|| s.to_owned());
                } else {
                    positionals.push((cur, s.to_owned()));
                }
            }
        }
    }
    if quiet {
        opts.format = Format::Silent;
    }
    // `diff_setup_done()`: `--name-only`, `--name-status` and `-s` clear every other
    // output format, so `--dirstat`/`--stat`/`-p` next to one of them are dropped.
    if matches!(opts.format, Format::NameOnly | Format::NameStatus | Format::Silent) {
        opts.dirstat = None;
        opts.patch = false;
        opts.numstat = false;
        opts.diffstat = false;
        opts.shortstat = false;
        opts.summary = false;
    }
    // `diff_setup_done()` defaults `output_format` to the raw listing only when nothing
    // else was requested. Any positive non-raw format (patch, a stat family, a summary
    // or a dirstat) suppresses it unless `--raw`/`--patch-with-raw` asked for it back —
    // which is why a bare `--dirstat` or `-p` prints no raw records.
    if opts.format == Format::Raw {
        let any_other = opts.patch
            || opts.numstat
            || opts.diffstat
            || opts.shortstat
            || opts.summary
            || opts.dirstat.is_some();
        opts.emit_pairs = raw_explicit || !any_other;
    }
    // `-s`/`--quiet` mean "no output at all", which is exactly what an unrenderable
    // `--check`/`--binary` would have produced here anyway.
    if opts.format == Format::Silent {
        bad_format = None;
    }
    // A content format alongside an accepted-but-unported content-shaping option would
    // render bytes that diverge from git; decline the run rather than emit them. The raw
    // and name listings are unaffected, so those keep ignoring the option.
    let content_format = opts.patch || opts.numstat || opts.diffstat || opts.shortstat || opts.summary;
    if content_format && bad_format.is_none() {
        if let Some(opt) = content_altering.take() {
            bad_format = Some(opt);
        }
    }
    if let Some((kind, pat)) = pickaxe_arg {
        // `-S` is a literal kwset search unless `--pickaxe-regex` promotes it; `-G` is
        // always a regex. A regex that fails to compile is git's
        // `fatal: invalid regex: …` (exit 128), deferred to after the tree-ish just as
        // git compiles it inside `diffcore_pickaxe`.
        if kind == b'S' && !pickaxe_regex {
            opts.pickaxe = Some(Pickaxe::Occurrences(Needle::Literal(pat)));
        } else {
            match compile_regex(&pat) {
                Ok(re) => {
                    let needle = Needle::Regex(re);
                    opts.pickaxe = Some(if kind == b'S' {
                        Pickaxe::Occurrences(needle)
                    } else {
                        Pickaxe::Grep(needle)
                    });
                }
                Err(msg) => {
                    bad_regex.get_or_insert_with(|| format!("fatal: invalid regex: {msg}\n").into_bytes());
                }
            }
        }
    }
    if let Some((idx, pat)) = ignore_arg {
        // `-I` is always a regex (`diff_opt_ignore_regex`), compiled inline; a bad one is
        // `error: invalid regex given to -I: '<pat>'` (exit 129) at its argv position.
        match compile_regex(&pat) {
            Ok(re) => opts.ignore_lines = Some(Needle::Regex(re)),
            Err(_) => {
                let mut msg = b"error: invalid regex given to -I: '".to_vec();
                msg.extend_from_slice(&pat);
                msg.extend_from_slice(b"'\n");
                set_earliest(&mut deferred, (idx, 129, msg));
            }
        }
    }

    let repo = gix::discover(".")?;

    // git's `setup_revisions`: each positional before `--` is tried as a revision.
    // The first that resolves is the tree-ish; a further one that also resolves is
    // an extra revision. Once a positional fails to resolve and is accepted as a
    // path, `pathspec_mode` latches on and every later positional must be a path on
    // disk (`no such path`), while a non-revision that is not a path is the classic
    // `ambiguous argument`. diff-index then insists on exactly one revision — zero
    // or two or more print its usage — mirroring `builtin/diff-index.c`.
    let mut spec: Option<String> = None;
    let mut resolved: Option<ObjectId> = None;
    let mut pending = 0usize;
    let mut pathspec_mode = false;
    for (idx, arg) in &positionals {
        // git parses left to right: an option-value error sitting *before* this positional
        // would already have died at its argv position, so fire that deferred error now
        // rather than resolving a positional git never reached.
        if let Some((err_idx, code, msg)) = &deferred {
            if err_idx < idx {
                std::io::stderr().lock().write_all(msg)?;
                return Ok(ExitCode::from(*code));
            }
        }
        if pathspec_mode {
            if std::fs::symlink_metadata(arg).is_err() {
                eprintln!("fatal: {arg}: no such path in the working tree.");
                return Ok(ExitCode::from(128));
            }
            paths.push(arg.as_str().into());
        } else if let Ok(id) = repo.rev_parse_single(arg.as_str()) {
            pending += 1;
            if spec.is_none() {
                spec = Some(arg.clone());
                resolved = Some(id.detach());
            }
        } else if std::fs::symlink_metadata(arg).is_err() {
            eprintln!(
                "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'git <command> [<revision>...] -- [<file>...]'"
            );
            return Ok(ExitCode::from(128));
        } else {
            pathspec_mode = true;
            paths.push(arg.as_str().into());
        }
    }
    // An option-value error after every positional (or with no positional that failed
    // first) is git's next parse error, ahead of the "exactly one revision" usage check.
    if let Some((_, code, msg)) = &deferred {
        std::io::stderr().lock().write_all(msg)?;
        return Ok(ExitCode::from(*code));
    }
    if pending != 1 {
        eprint!("{}", USAGE);
        return Ok(ExitCode::from(129));
    }
    let spec = spec.expect("pending == 1 guarantees a resolved tree-ish");
    let resolved = resolved.expect("pending == 1 guarantees a resolved tree-ish");

    let base = if merge_base {
        let head = match repo.head_id() {
            Ok(id) => id.detach(),
            Err(_) => {
                eprintln!("fatal: no merge base found");
                return Ok(ExitCode::from(128));
            }
        };
        match repo.merge_base(head, resolved) {
            Ok(id) => id.detach(),
            Err(_) => {
                if !object_is_commit(&repo, &resolved) {
                    eprintln!("error: object {resolved} is a tree, not a commit");
                }
                eprintln!("fatal: no merge base found");
                return Ok(ExitCode::from(128));
            }
        }
    } else {
        resolved
    };

    let tree_id = match repo
        .find_object(base)
        .map_err(anyhow::Error::from)
        .and_then(|o| Ok(o.peel_to_tree()?.id))
    {
        Ok(id) => id,
        Err(_) => {
            eprintln!(
                "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'git <command> [<revision>...] -- [<file>...]'"
            );
            return Ok(ExitCode::from(128));
        }
    };

    if let Some(flag) = unsupported {
        bail!("unsupported flag {flag:?} (ported: {PORTED})");
    }
    if let Some(msg) = &bad_regex {
        std::io::stderr().lock().write_all(msg)?;
        return Ok(ExitCode::from(128));
    }

    // git resolves the tree-ish before it notices there is no worktree, so a bare repo
    // reaches this `fatal` (exit 128) rather than the earlier usage error.
    if !opts.cached && repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }

    // Magic (`:(…)`) and glob (`* ? [`) pathspecs go through gitoxide's pathspec engine,
    // git's own algorithm, which applies the cwd prefix, `:(top)`, `:(icase)`, `:(glob)`
    // and `:(exclude)` exactly as git does. Purely literal paths and directory prefixes
    // stay on the proven fast path below so their well-exercised behaviour is untouched.
    let needs_gix = paths
        .iter()
        .any(|p| p.first() == Some(&b':') || p.iter().any(|&b| matches!(b, b'*' | b'?' | b'[')));

    let mut deltas = collect(&repo, &tree_id, &opts)?;
    if !paths.is_empty() {
        if needs_gix {
            let index = repo.index_or_empty()?;
            let mut ps = repo.pathspec(
                false,
                &paths,
                false,
                &index,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )?;
            deltas.retain(|d| ps.is_included(d.path.as_bstr(), Some(false)));
        } else {
            // Pathspecs are cwd-relative in git while output paths are root-relative, so
            // lift every pattern into repository-root space before matching.
            let prefix = repo_prefix(&repo)?;
            let lifted: Vec<BString> = paths
                .iter()
                .map(|p| {
                    let mut full = prefix.clone();
                    full.extend_from_slice(p);
                    full
                })
                .collect();
            deltas.retain(|d| lifted.iter().any(|p| path_matches(&d.path, p)));
        }
    }
    if let Some(rel) = &opts.relative {
        if !rel.is_empty() {
            deltas.retain(|d| path_matches(&d.path, rel));
        }
    }
    // git emits index order, which is a plain byte-wise sort of the paths.
    deltas.sort_by(|a, b| a.path.cmp(&b.path));

    // git's `diffcore_std`: content comparison first (which also fills in the object id
    // it had to compute), then the pickaxe, then `--diff-filter`. Every content-reading
    // output format (patch, the stat family, `--summary`) participates: git runs each
    // pair through the patch machinery, drops the ones whose content turns out identical
    // (the stat-dirty-but-unchanged files), and hands the survivors the destination id it
    // hashed on the way, exactly as the whitespace family and pickaxe do.
    let content_output = (opts.patch
        || opts.numstat
        || opts.diffstat
        || opts.shortstat
        || opts.summary)
        && opts.format != Format::Silent;
    let content_driven = opts.ws.any()
        || opts.ignore_lines.is_some()
        || opts.pickaxe.is_some()
        || content_output
        || bad_format.is_some();
    if content_driven {
        apply_content_filter(&repo, &mut deltas, &opts)?;
        apply_pickaxe(&repo, &mut deltas, &opts)?;
    } else if opts.detect_rename {
        // git hashes every rename candidate; the only visible effect on this listing is
        // that a created path shows its real object id instead of the null id.
        fill_added_ids(&repo, &mut deltas, &opts)?;
    }

    if opts.reverse {
        for d in &mut deltas {
            if d.unmerged {
                // `diff_unmerge` builds its pair outside `diff_change`, which is where
                // git applies `-R`, so unmerged records are never swapped.
                continue;
            }
            std::mem::swap(&mut d.src_mode, &mut d.dst_mode);
            std::mem::swap(&mut d.src_id, &mut d.dst_id);
        }
    }
    if !opts.filter_include.is_empty() || !opts.filter_exclude.is_empty() {
        deltas.retain(|d| passes_filter(d.status(), &opts));
    }

    // git's `diff_flush()` reorders the queued pairs for `--skip-to`/`--rotate-to` before
    // any output format runs: it scans the queue for the first pair whose path matches and
    // `die()`s with exit 128 when none does — but only for a non-empty queue, so an
    // all-clean diff accepts any target. The comparison is against the repository-root
    // path, exactly as it is against `p->two->path`, so the target is used verbatim (never
    // cwd-prefixed). skip drops the pairs before the match; rotate wraps them to the end.
    if let Some((is_skip, target)) = &opts.skip_or_rotate {
        if !deltas.is_empty() {
            match deltas.iter().position(|d| d.path == *target) {
                Some(k) => {
                    if *is_skip {
                        deltas.drain(..k);
                    } else {
                        deltas.rotate_left(k);
                    }
                }
                None => {
                    let mut msg = b"fatal: No such path '".to_vec();
                    msg.extend_from_slice(target.as_slice());
                    msg.extend_from_slice(b"' in the diff\n");
                    std::io::stderr().lock().write_all(&msg)?;
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    if let Some(flag) = &bad_format {
        // `--check`/`--binary` are still declined rather than approximated. Both are
        // content-driven in git, so an all-clean pair list renders as nothing; only a run
        // that would produce real bytes is refused.
        if !deltas.is_empty() {
            bail!("unsupported output format {flag:?} (ported: {PORTED})");
        }
    } else if opts.format != Format::Silent {
        // Per-pair blob analysis for the content formats. The stat family and the patch
        // need the two sides' bytes and, for the patch, the rendered hunks; every other
        // format reads only the recorded modes and ids.
        let workdir = repo.workdir().map(Path::to_path_buf);
        let need_analyses = opts.patch || opts.numstat || opts.diffstat || opts.shortstat;
        let analyses: Vec<IdxAnalysis> = if need_analyses {
            deltas
                .iter()
                .map(|d| analyze_index_delta(&repo, workdir.as_deref(), d, &opts))
                .collect::<Result<_>>()?
        } else {
            Vec::new()
        };

        // git's `diff_flush()` order: the raw/name listing, then the stat family, the
        // dirstat and the summary, then a lone separator line and the patch. The raw
        // block carries its own `--line-prefix`; everything below is prefixed in one pass.
        let mut out: Vec<u8> = Vec::new();
        let mut rest: Vec<u8> = Vec::new();
        let mut separator = false;

        if opts.emit_pairs {
            out.extend_from_slice(&render(&repo, &deltas, &opts)?);
            if !deltas.is_empty() {
                separator = true;
            }
        }

        if !deltas.is_empty() {
            if opts.numstat || opts.diffstat || opts.shortstat {
                let stats = compute_diffstat(&deltas, &analyses, &opts);
                if opts.numstat {
                    render_numstat(&mut rest, &stats, &opts);
                }
                if opts.diffstat {
                    render_stat(&mut rest, &stats, &opts);
                }
                if opts.shortstat {
                    render_shortstat(&mut rest, &stats);
                }
                separator = true;
            }
            if let Some(ds) = &opts.dirstat {
                let files = dirstat_damage(&repo, &deltas, &opts, ds)?;
                render_dirstat(&mut rest, files, ds);
            }
            if opts.summary && !summary_is_empty(&deltas) {
                for d in &deltas {
                    render_summary(&mut rest, d, &opts);
                }
                separator = true;
            }
        }

        if opts.patch && !deltas.is_empty() {
            if separator {
                rest.push(b'\n');
            }
            for (d, an) in deltas.iter().zip(&analyses) {
                render_patch(&mut rest, d, an, &opts);
            }
        }

        // `diff_line_prefix()` precedes every rendered line of the stat/dirstat/summary/
        // patch block (the raw block already emitted its own prefix in `render`).
        if !opts.line_prefix.is_empty() {
            rest = prefix_lines(&rest, &opts.line_prefix);
        }
        out.extend_from_slice(&rest);
        std::io::stdout().lock().write_all(&out)?;
    }

    Ok(if opts.exit_code && !deltas.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// `parse_dirstat_opt()`: fold one parameter list into the accumulated `--dirstat`
/// state, turning the format on. Returns git's exit code when a parameter is bad,
/// having already written the `die()` text `parse_dirstat_params()` builds.
fn apply_dirstat(opts: &mut Opts, params: &str) -> Option<ExitCode> {
    let ds = opts.dirstat.get_or_insert_with(DirStat::default);
    let mut errors = String::new();
    // An empty list is not split at all, so `--dirstat=` is simply the default.
    if !params.is_empty() {
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
                    // anything else is an unknown parameter rather than a bad number.
                    None if p.as_bytes().first().is_some_and(u8::is_ascii_digit) => {
                        errors.push_str(&format!("  Failed to parse dirstat cut-off percentage '{p}'\n"));
                    }
                    None => errors.push_str(&format!("  Unknown dirstat parameter '{p}'\n")),
                },
            }
        }
    }
    if errors.is_empty() {
        return None;
    }
    eprint!("fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n");
    Some(ExitCode::from(128))
}

/// A dirstat cut-off percentage: a whole number plus at most one significant decimal
/// digit, with any further digits read and discarded, and nothing left over — exactly
/// what `parse_dirstat_params()`'s `strtoul` walk accepts.
fn parse_permille(p: &str) -> Option<u32> {
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

/// `show_dirstat()` and `show_dirstat_by_line()`: the damage each path contributes.
fn dirstat_damage(
    repo: &gix::Repository,
    deltas: &[Delta],
    opts: &Opts,
    ds: &DirStat,
) -> Result<Vec<(BString, u64)>> {
    let workdir = repo.workdir().map(Path::to_path_buf);
    let mut out = Vec::with_capacity(deltas.len());
    for d in deltas {
        if ds.by_line {
            // The by-line variant charges the diffstat's added plus deleted lines, and
            // an unmerged pair never gets counts of its own.
            let damage = if d.unmerged {
                0
            } else {
                let one = side_content(repo, workdir.as_deref(), d, true)?.unwrap_or_default();
                let two = side_content(repo, workdir.as_deref(), d, false)?.unwrap_or_default();
                if buffer_is_binary(&one) || buffer_is_binary(&two) {
                    // Binary files count bytes, which git normalises at 64 per "line".
                    ((one.len() + two.len()) as u64).div_ceil(64)
                } else {
                    let (added, deleted) = line_counts(&one, &two, opts);
                    added + deleted
                }
            };
            out.push((d.path.clone(), damage));
            continue;
        }
        // Two recorded, equal ids settle it: the content cannot have changed.
        if !d.src_id.is_null() && !d.dst_id.is_null() && d.src_id == d.dst_id {
            out.push((d.path.clone(), 0));
            continue;
        }
        if ds.by_file {
            out.push((d.path.clone(), 1));
            continue;
        }
        // `side_content` already answers `None` for a side with no mode, which is
        // exactly git's `DIFF_FILE_VALID` test.
        let one = side_content(repo, workdir.as_deref(), d, true)?;
        let two = side_content(repo, workdir.as_deref(), d, false)?;
        // Removed material is the original minus what survived, added is the new
        // material; both are damage done to the preimage.
        let damage = match (&one, &two) {
            (Some(one), Some(two)) => {
                let (copied, added) =
                    count_changes_sides(one, !buffer_is_binary(one), two, !buffer_is_binary(two));
                (one.len() as u64).saturating_sub(copied) + added
            }
            (Some(one), None) => one.len() as u64,
            (None, Some(two)) => two.len() as u64,
            // Neither side exists — nothing to charge, and no entry at all.
            (None, None) => continue,
        };
        // A zero score with a changed id still counts as one unit of damage.
        out.push((d.path.clone(), if damage == 0 { 1 } else { damage }));
    }
    Ok(out)
}

/// git's `buffer_is_binary()`: a NUL byte within the first 8000 bytes.
fn buffer_is_binary(buf: &[u8]) -> bool {
    buf[..buf.len().min(8000)].contains(&0)
}

/// The added and removed line counts a diffstat would report for the two sides.
fn line_counts(one: &[u8], two: &[u8], opts: &Opts) -> (u64, u64) {
    let before = split_lines(one);
    let after = split_lines(two);
    let fold = opts.ws.any();
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| if fold { fold_line(l, opts.ws) } else { l.to_vec() }));
    input.update_after(after.iter().map(|l| if fold { fold_line(l, opts.ws) } else { l.to_vec() }));
    let diff = Diff::compute(Algorithm::Myers, &input);
    (u64::from(diff.count_additions()), u64::from(diff.count_removals()))
}

/// `--diff-filter=<letters>`: upper-case selects, lower-case excludes. Returns the first
/// letter git does not know (as given, before case folding), or `None` when all are valid.
fn parse_filter(spec: &str, opts: &mut Opts) -> Option<u8> {
    const KNOWN: &[u8] = b"ACDMRTUXB*";
    for c in spec.bytes() {
        let upper = c.to_ascii_uppercase();
        if !KNOWN.contains(&upper) {
            return Some(c);
        }
        if c.is_ascii_lowercase() {
            opts.filter_exclude.push(upper);
        } else {
            opts.filter_include.push(upper);
        }
    }
    None
}

fn passes_filter(status: u8, opts: &Opts) -> bool {
    if opts.filter_exclude.contains(&status) {
        return false;
    }
    opts.filter_include.is_empty() || opts.filter_include.contains(&b'*') || opts.filter_include.contains(&status)
}

fn trim_slashes(s: &str) -> BString {
    BString::from(s.trim_matches('/').as_bytes().to_vec())
}

fn object_is_commit(repo: &gix::Repository, id: &ObjectId) -> bool {
    repo.find_object(*id).map(|o| o.kind == gix::objs::Kind::Commit).unwrap_or(false)
}

/// Diff `tree_id` against the index, then (unless `--cached`) fold in how the worktree
/// deviates from that index, exactly as git's `oneway_diff` does.
fn collect(repo: &gix::Repository, tree_id: &ObjectId, opts: &Opts) -> Result<Vec<Delta>> {
    let null = ObjectId::null(repo.object_hash());
    let mut tree: BTreeMap<BString, (u32, ObjectId)> = BTreeMap::new();
    flatten_tree(repo, tree_id, &BString::default(), &mut tree)?;

    let index = repo.index_or_empty()?;
    let index_state: &gix::index::State = &index;

    let mut idx: BTreeMap<BString, IdxInfo> = BTreeMap::new();
    for e in index_state.entries() {
        let path = BString::from(e.path(index_state).to_vec());
        let stage = e.stage_raw();
        match idx.get_mut(&path) {
            Some(slot) => {
                slot.unmerged = true;
                // Stage 2 ("ours") is the entry git's one-way merge keeps.
                if stage == 2 {
                    slot.mode = e.mode.bits();
                    slot.id = e.id;
                    slot.stat = e.stat;
                }
            }
            None => {
                idx.insert(
                    path,
                    IdxInfo {
                        mode: e.mode.bits(),
                        id: e.id,
                        stat: e.stat,
                        intent_to_add: e.flags.contains(gix::index::entry::Flags::INTENT_TO_ADD),
                        unmerged: stage != 0,
                    },
                );
            }
        }
    }

    let workdir: Option<PathBuf> = repo.workdir().map(Path::to_path_buf);
    if !opts.cached && workdir.is_none() {
        bail!("this operation must be run in a work tree");
    }
    let index_timestamp = index_state.timestamp().unix_seconds();

    let all: BTreeSet<&BString> = tree.keys().chain(idx.keys()).collect();
    let mut deltas = Vec::new();
    for path in all {
        let src = tree.get(path).copied();
        let Some(info) = idx.get(path) else {
            // In the tree but gone from the index: a plain deletion.
            let (mode, id) = src.expect("path came from one of the two maps");
            deltas.push(Delta {
                src_mode: mode,
                src_id: id,
                dst_mode: 0,
                dst_id: null,
                unmerged: false,
                path: path.clone(),
            });
            continue;
        };

        if info.unmerged && opts.cached {
            // git's `diff_unmerge`: one record with the tree side and an empty
            // destination, whatever the stages hold.
            let (mode, id) = src.unwrap_or((0, null));
            deltas.push(Delta {
                src_mode: mode,
                src_id: id,
                dst_mode: 0,
                dst_id: null,
                unmerged: true,
                path: path.clone(),
            });
            continue;
        }

        // git's `get_stat_data`.
        let mut dst_mode = info.mode;
        let mut dst_id = info.id;
        if !opts.cached {
            let workdir = workdir.as_deref().expect("checked above");
            let full = worktree_path(workdir, path);
            match std::fs::symlink_metadata(&full) {
                Ok(md) if md.is_dir() && (info.mode & S_IFMT) != 0o160000 => {
                    // A tracked file replaced by a directory counts as removed.
                    if !opts.match_missing {
                        if src.is_none() {
                            continue;
                        }
                        dst_mode = 0;
                        dst_id = null;
                    }
                }
                Ok(md) => {
                    // Submodules are left alone: deciding whether a checked-out
                    // submodule is dirty needs a full status of its own worktree.
                    if (info.mode & S_IFMT) != 0o160000
                        && (info.intent_to_add
                            || entry_is_dirty(repo, info, &md, index_timestamp, &full))
                    {
                        dst_mode = mode_from_stat(&md);
                        dst_id = null;
                    }
                }
                Err(_) => {
                    if !opts.match_missing {
                        if src.is_none() {
                            // git's `show_new_file` prints nothing for a staged
                            // addition whose worktree file is gone.
                            continue;
                        }
                        dst_mode = 0;
                        dst_id = null;
                    }
                }
            }
        }

        let (src_mode, src_id) = src.unwrap_or((0, null));
        if src_mode == dst_mode && src_id == dst_id {
            continue;
        }
        deltas.push(Delta {
            src_mode,
            src_id,
            dst_mode,
            dst_id,
            unmerged: false,
            path: path.clone(),
        });
    }

    Ok(deltas)
}

/// Flatten `tree_id` into `out`, keyed by repository-root relative path.
fn flatten_tree(
    repo: &gix::Repository,
    tree_id: &ObjectId,
    prefix: &BString,
    out: &mut BTreeMap<BString, (u32, ObjectId)>,
) -> Result<()> {
    let tree = repo.find_object(*tree_id)?.into_tree();
    let decoded = tree.decode()?;
    let entries: Vec<(BString, u32, ObjectId)> = decoded
        .entries
        .iter()
        .map(|e| {
            let mut path = prefix.clone();
            path.extend_from_slice(e.filename);
            (path, u32::from(e.mode.value()), e.oid.to_owned())
        })
        .collect();
    for (path, mode, id) in entries {
        if (mode & S_IFMT) == 0o040000 {
            let mut sub = path;
            sub.push(b'/');
            flatten_tree(repo, &id, &sub, out)?;
        } else {
            out.insert(path, (mode, id));
        }
    }
    Ok(())
}

/// git's `ie_match_stat` reduced to what `diff-index` needs: the entry is dirty when
/// its recorded type/permissions or any of its stat fields disagree with `lstat`.
fn entry_is_dirty(
    repo: &gix::Repository,
    info: &IdxInfo,
    md: &std::fs::Metadata,
    index_timestamp: i64,
    full: &Path,
) -> bool {
    if mode_changed(info.mode, md) || stat_data_changed(&info.stat, md) {
        return true;
    }
    // git's racy-timestamp rule: an entry whose mtime is at or after the index's own
    // timestamp cannot be trusted on stat alone, so the content has to decide. An index
    // with no timestamp of its own (never written) is never racy, as in `is_racy_stat`.
    if index_timestamp == 0 || i64::from(info.stat.mtime.secs) < index_timestamp {
        return false;
    }
    match std::fs::read(full) {
        Ok(data) => gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &data)
            .map(|id| id != info.id)
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// git's `ce_match_stat_basic` type and permission comparison.
fn mode_changed(entry_mode: u32, md: &std::fs::Metadata) -> bool {
    match entry_mode & S_IFMT {
        0o100000 => {
            if !md.is_file() {
                return true;
            }
            // Only the owner's execute bit is considered a mode change.
            (entry_mode ^ fs_mode(md)) & 0o100 != 0
        }
        0o120000 => !md.is_symlink(),
        0o160000 => !md.is_dir(),
        _ => true,
    }
}

/// git's `ce_mode_from_stat`/`create_ce_mode` with `trust_executable_bit` on.
fn mode_from_stat(md: &std::fs::Metadata) -> u32 {
    if md.is_symlink() {
        0o120000
    } else if md.is_dir() {
        0o160000
    } else if fs_mode(md) & 0o100 != 0 {
        0o100755
    } else {
        0o100644
    }
}

/// The absolute path of the worktree file for a repository-root relative `path`.
fn worktree_path(workdir: &Path, path: &BString) -> PathBuf {
    workdir.join(&*gix::path::from_bstr(path))
}

#[cfg(unix)]
fn fs_mode(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.mode()
}

#[cfg(not(unix))]
fn fs_mode(_md: &std::fs::Metadata) -> u32 {
    0
}

/// git's `match_stat_data` with its defaults (`core.trustctime` and `core.checkStat`
/// both on, nanoseconds and `st_dev` both off). Every comparison truncates to 32 bits
/// because that is the width the index stores.
#[cfg(unix)]
fn stat_data_changed(sd: &gix::index::entry::Stat, md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    sd.mtime.secs != md.mtime() as u32
        || sd.ctime.secs != md.ctime() as u32
        || sd.uid != md.uid()
        || sd.gid != md.gid()
        || sd.ino != md.ino() as u32
        || sd.size != md.size() as u32
}

#[cfg(not(unix))]
fn stat_data_changed(sd: &gix::index::entry::Stat, md: &std::fs::Metadata) -> bool {
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    sd.mtime.secs != mtime || sd.size != md.len() as u32
}

/// Drop every pair whose two sides carry the same content once the requested folding is
/// applied, and give the surviving worktree side the object id git had to compute in
/// order to decide that.
fn apply_content_filter(repo: &gix::Repository, deltas: &mut Vec<Delta>, opts: &Opts) -> Result<()> {
    let null = ObjectId::null(repo.object_hash());
    let workdir = repo.workdir().map(Path::to_path_buf);
    let mut keep = Vec::with_capacity(deltas.len());
    for d in deltas.drain(..) {
        if d.unmerged {
            keep.push(d);
            continue;
        }
        let same = d.src_mode != 0
            && d.dst_mode != 0
            && d.src_mode == d.dst_mode
            && sides_match(repo, workdir.as_deref(), &d, opts)?;
        if same {
            continue;
        }
        let mut d = d;
        if d.dst_id == null && d.dst_mode != 0 {
            if let Some(id) = hash_worktree(repo, workdir.as_deref(), &d.path)? {
                d.dst_id = id;
            }
        }
        keep.push(d);
    }
    *deltas = keep;
    Ok(())
}

/// `-M`/`-C` make git hash the rename candidates; the visible consequence for this
/// listing is that a created path carries its real object id.
fn fill_added_ids(repo: &gix::Repository, deltas: &mut [Delta], _opts: &Opts) -> Result<()> {
    let null = ObjectId::null(repo.object_hash());
    let workdir = repo.workdir().map(Path::to_path_buf);
    for d in deltas.iter_mut() {
        if d.src_mode == 0 && d.dst_mode != 0 && d.dst_id == null {
            if let Some(id) = hash_worktree(repo, workdir.as_deref(), &d.path)? {
                d.dst_id = id;
            }
        }
    }
    Ok(())
}

/// The pickaxe (`-S` counts occurrences, `-G` greps the changed lines).
fn apply_pickaxe(repo: &gix::Repository, deltas: &mut Vec<Delta>, opts: &Opts) -> Result<()> {
    let Some(pickaxe) = &opts.pickaxe else {
        return Ok(());
    };
    let workdir = repo.workdir().map(Path::to_path_buf);
    let mut hits = Vec::with_capacity(deltas.len());
    for d in deltas.iter() {
        let one = side_content(repo, workdir.as_deref(), d, true)?;
        let two = side_content(repo, workdir.as_deref(), d, false)?;
        let hit = match pickaxe {
            Pickaxe::Occurrences(needle) => {
                let a = one.as_deref().map(|b| needle.count(b)).unwrap_or(0);
                let b = two.as_deref().map(|b| needle.count(b)).unwrap_or(0);
                a != b
            }
            Pickaxe::Grep(needle) => match (one.as_deref(), two.as_deref()) {
                (None, None) => false,
                (None, Some(t)) | (Some(t), None) => needle.is_match(t),
                (Some(a), Some(b)) => changed_lines_hit(a, b, needle),
            },
        };
        hits.push(hit);
    }
    if opts.pickaxe_all && hits.iter().any(|h| *h) {
        return Ok(());
    }
    let mut it = hits.into_iter();
    deltas.retain(|_| it.next().unwrap_or(false));
    Ok(())
}

/// The bytes of one side of a pair, or `None` when that side does not exist.
fn side_content(
    repo: &gix::Repository,
    workdir: Option<&Path>,
    d: &Delta,
    source: bool,
) -> Result<Option<Vec<u8>>> {
    let null = ObjectId::null(repo.object_hash());
    let (mode, id) = if source { (d.src_mode, d.src_id) } else { (d.dst_mode, d.dst_id) };
    if mode == 0 {
        return Ok(None);
    }
    if (mode & S_IFMT) == 0o160000 {
        // A submodule has no blob to compare; git uses its recorded commit id.
        return Ok(Some(id.to_string().into_bytes()));
    }
    if id != null {
        return Ok(Some(repo.find_object(id)?.data.clone()));
    }
    let Some(workdir) = workdir else {
        return Ok(None);
    };
    Ok(read_worktree(workdir, &d.path))
}

/// `true` when the two sides of `d` hold the same content under the requested folding.
fn sides_match(repo: &gix::Repository, workdir: Option<&Path>, d: &Delta, opts: &Opts) -> Result<bool> {
    let null = ObjectId::null(repo.object_hash());
    // Identical recorded ids settle it without reading anything.
    if d.src_id != null && d.dst_id != null {
        if d.src_id == d.dst_id {
            return Ok(true);
        }
        if !opts.ws.any() && opts.ignore_lines.is_none() {
            return Ok(false);
        }
    }
    let (Some(one), Some(two)) = (
        side_content(repo, workdir, d, true)?,
        side_content(repo, workdir, d, false)?,
    ) else {
        return Ok(false);
    };
    Ok(contents_match(&one, &two, opts))
}

/// The hash the worktree file at `path` would get as a blob.
fn hash_worktree(repo: &gix::Repository, workdir: Option<&Path>, path: &BString) -> Result<Option<ObjectId>> {
    let Some(workdir) = workdir else {
        return Ok(None);
    };
    let Some(data) = read_worktree(workdir, path) else {
        return Ok(None);
    };
    Ok(Some(gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &data)?))
}

/// The bytes git would hash for the worktree entry at `path`: file contents, or the
/// target of a symlink.
fn read_worktree(workdir: &Path, path: &BString) -> Option<Vec<u8>> {
    let full = worktree_path(workdir, path);
    let md = std::fs::symlink_metadata(&full).ok()?;
    if md.is_symlink() {
        let target = std::fs::read_link(&full).ok()?;
        Some(gix::path::into_bstr(target).into_owned().into())
    } else {
        std::fs::read(&full).ok()
    }
}

/// Occurrences of `needle` in `haystack`, counted without overlap, as git's kwset does.
fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || needle.len() > haystack.len() {
        return 0;
    }
    let mut count = 0;
    let mut at = 0;
    while at + needle.len() <= haystack.len() {
        if &haystack[at..at + needle.len()] == needle {
            count += 1;
            at += needle.len();
        } else {
            at += 1;
        }
    }
    count
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    count_occurrences(haystack, needle) > 0
}

/// git's `-G`: does any line the diff adds or removes match `needle`?
fn changed_lines_hit(one: &[u8], two: &[u8], needle: &Needle) -> bool {
    let before = split_lines(one);
    let after = split_lines(two);
    let mut hit = false;
    for_each_changed_line(&before, &after, |line| {
        if needle.is_match(line) {
            hit = true;
        }
    });
    hit
}

/// `true` when the two blobs carry the same content once `opts`' whitespace folding and
/// `-I` line filtering are applied.
fn contents_match(one: &[u8], two: &[u8], opts: &Opts) -> bool {
    if !opts.ws.any() && opts.ignore_lines.is_none() {
        return one == two;
    }
    let before: Vec<Vec<u8>> = split_lines(one).into_iter().map(|l| fold_line(l, opts.ws)).collect();
    let after: Vec<Vec<u8>> = split_lines(two).into_iter().map(|l| fold_line(l, opts.ws)).collect();
    if before == after {
        return true;
    }
    let Some(pattern) = &opts.ignore_lines else {
        return false;
    };
    // `-I` drops a hunk whose every changed line matches, so the two sides count as
    // equal exactly when no changed line falls outside the pattern.
    let raw_before = split_lines(one);
    let raw_after = split_lines(two);
    let mut all_match = true;
    for_each_changed_line(&raw_before, &raw_after, |line| {
        if !pattern.is_match(line) {
            all_match = false;
        }
    });
    all_match
}

/// Split into lines, each keeping its terminator, as xdiff records them.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|&c| c == b'\n').collect()
}

/// Apply one line's worth of git's `XDF_IGNORE_*` folding.
fn fold_line(line: &[u8], ws: Ws) -> Vec<u8> {
    let mut s = line;
    if s.last() == Some(&b'\n') {
        s = &s[..s.len() - 1];
    }
    if ws.cr && s.last() == Some(&b'\r') {
        s = &s[..s.len() - 1];
    }
    if ws.all {
        return s.iter().copied().filter(|c| *c != b' ' && *c != b'\t').collect();
    }
    if ws.change {
        let mut out = Vec::with_capacity(s.len());
        let mut pending_blank = false;
        for &c in s {
            if c == b' ' || c == b'\t' {
                pending_blank = true;
            } else {
                if pending_blank && !out.is_empty() {
                    out.push(b' ');
                }
                pending_blank = false;
                out.push(c);
            }
        }
        return out;
    }
    if ws.at_eol {
        let mut end = s.len();
        while end > 0 && (s[end - 1] == b' ' || s[end - 1] == b'\t') {
            end -= 1;
        }
        return s[..end].to_vec();
    }
    s.to_vec()
}

/// Run a line diff and hand every added or removed line to `visit`.
fn for_each_changed_line(before: &[&[u8]], after: &[&[u8]], mut visit: impl FnMut(&[u8])) {
    let one: Vec<u8> = before.concat();
    let two: Vec<u8> = after.concat();
    let input = InternedInput::new(one.as_slice(), two.as_slice());
    let diff = Diff::compute(Algorithm::Myers, &input);
    for hunk in diff.hunks() {
        for i in hunk.before.clone() {
            if let Some(line) = before.get(i as usize) {
                visit(line);
            }
        }
        for i in hunk.after.clone() {
            if let Some(line) = after.get(i as usize) {
                visit(line);
            }
        }
    }
}

/// The repository-relative directory the command was invoked from, with a trailing
/// slash, or empty when it was run at the root.
fn repo_prefix(repo: &gix::Repository) -> Result<BString> {
    let Some(prefix) = repo.prefix()? else {
        return Ok(BString::default());
    };
    if prefix.as_os_str().is_empty() {
        return Ok(BString::default());
    }
    let mut out: BString = gix::path::into_bstr(prefix).into_owned();
    out.push(b'/');
    Ok(out)
}

/// `true` if `path` equals `pat` or lives under the directory `pat`.
fn path_matches(path: &BString, pat: &BString) -> bool {
    let pat: &[u8] = {
        let raw = pat.as_slice();
        match raw.strip_suffix(b"/") {
            Some(trimmed) => trimmed,
            None => raw,
        }
    };
    let path = path.as_slice();
    path == pat || (path.len() > pat.len() && path.starts_with(pat) && path[pat.len()] == b'/')
}

/// Render the whole listing into the exact bytes git would write.
fn render(repo: &gix::Repository, deltas: &[Delta], opts: &Opts) -> Result<Vec<u8>> {
    let hexsz = repo.object_hash().len_in_hex();
    let len = abbrev_len(repo, deltas, opts, hexsz);

    // Field separator (between status and path) and record terminator.
    let (sep, term): (u8, u8) = if opts.nul { (0, 0) } else { (b'\t', b'\n') };
    // `--relative=<dir>` reports paths relative to that directory.
    let strip = opts
        .relative
        .as_ref()
        .filter(|r| !r.is_empty())
        .map(|r| r.len() + 1)
        .unwrap_or(0);

    let mut out = Vec::new();
    for d in deltas {
        out.extend_from_slice(&opts.line_prefix);
        match opts.format {
            Format::Silent => unreachable!("silent output is short-circuited by the caller"),
            Format::NameOnly => {}
            Format::NameStatus => {
                out.push(d.status());
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
                out.push(d.status());
                out.push(sep);
            }
        }
        let path = &d.path.as_slice()[strip.min(d.path.len())..];
        if opts.nul {
            out.extend_from_slice(path);
        } else {
            out.extend_from_slice(quote_path(path).as_bytes());
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
/// id in the listing, falling back to git's minimum default of 7 when there is none.
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
                    .flat_map(|d| [&d.src_id, &d.dst_id])
                    .find(|id| !id.is_null())
                    .map(|id| id.attach(repo).shorten_or_id().hex_len())
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

// ---------------------------------------------------------------------------
// content formats (-p / --stat / --numstat / --shortstat / --summary)
//
// A direct port of git's `builtin_diff()`, `compute_diffstat()`/`show_stats()` and
// `diff_summary()`, mirroring the byte-for-byte renderers `diff-files` already uses.
// The pair list is diff-index's (a tree against the index or worktree), and the two
// sides' bytes come from the object database or the worktree via [`content_of`].
// ---------------------------------------------------------------------------

/// Per-delta blob analysis: line counts, the binary flag and, when a patch is asked
/// for, the rendered hunks. The two buffers are in the delta's own orientation, so
/// `-R` (which already swapped the delta's sides) is reflected here too.
struct IdxAnalysis {
    added: u32,
    deleted: u32,
    binary: bool,
    /// `None` when the two sides compare equal (a pure mode/type change) or a patch
    /// was not requested.
    hunks: Option<Vec<u8>>,
    old_data: Vec<u8>,
    new_data: Vec<u8>,
}

/// The bytes of one side of a pair, or `None` when that side does not exist. Unlike
/// [`side_content`], a recorded id that is not in the object database (the destination
/// id the content filter computed from the worktree, which is never written to the odb)
/// falls back to reading the worktree file, so the analysis always sees real bytes.
fn content_of(
    repo: &gix::Repository,
    workdir: Option<&Path>,
    mode: u32,
    id: ObjectId,
    path: &BString,
) -> Result<Option<Vec<u8>>> {
    let null = ObjectId::null(repo.object_hash());
    if mode == 0 {
        return Ok(None);
    }
    if (mode & S_IFMT) == 0o160000 {
        // A submodule has no blob; git uses its recorded commit id as the "content".
        return Ok(Some(id.to_string().into_bytes()));
    }
    if id != null {
        if let Ok(obj) = repo.find_object(id) {
            return Ok(Some(obj.data.clone()));
        }
        // The id was hashed from the worktree, so read that instead of failing.
    }
    Ok(workdir.and_then(|wd| read_worktree(wd, path)))
}

/// `builtin_diff()`'s internal-diff branch: intern the two sides (folded for the
/// whitespace family), count the added/removed lines the way `diffstat` does — dropping
/// change groups every line of which matches `-I` — and render the unified hunks when a
/// patch is wanted. Binary sides short-circuit to a byte-count analysis.
fn analyze_index_delta(
    repo: &gix::Repository,
    workdir: Option<&Path>,
    d: &Delta,
    opts: &Opts,
) -> Result<IdxAnalysis> {
    if d.unmerged {
        return Ok(IdxAnalysis {
            added: 0,
            deleted: 0,
            binary: false,
            hunks: None,
            old_data: Vec::new(),
            new_data: Vec::new(),
        });
    }

    let old_data = content_of(repo, workdir, d.src_mode, d.src_id, &d.path)?.unwrap_or_default();
    let new_data = content_of(repo, workdir, d.dst_mode, d.dst_id, &d.path)?.unwrap_or_default();

    // `diff_filespec_is_binary()`: a diff is binary if either present side is binary.
    let binary = (d.src_mode != 0 && buffer_is_binary(&old_data))
        || (d.dst_mode != 0 && buffer_is_binary(&new_data));
    if binary {
        return Ok(IdxAnalysis {
            added: 0,
            deleted: 0,
            binary: true,
            hunks: None,
            old_data,
            new_data,
        });
    }

    let before = split_lines(&old_data);
    let after = split_lines(&new_data);
    let fold = opts.ws.any();
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| if fold { fold_line(l, opts.ws) } else { l.to_vec() }));
    input.update_after(after.iter().map(|l| if fold { fold_line(l, opts.ws) } else { l.to_vec() }));

    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    // `xdl_mark_ignorable_regex()`: a change group whose every removed and added line
    // matches the `-I` pattern contributes nothing to the counts.
    let (added, deleted) = match &opts.ignore_lines {
        None => (diff.count_additions(), diff.count_removals()),
        Some(pat) => {
            let mut add = 0u32;
            let mut del = 0u32;
            for hunk in diff.hunks() {
                let ignorable = hunk.before.clone().all(|i| pat.is_match(before[i as usize]))
                    && hunk.after.clone().all(|i| pat.is_match(after[i as usize]));
                if ignorable {
                    continue;
                }
                del += hunk.before.clone().count() as u32;
                add += hunk.after.clone().count() as u32;
            }
            (add, del)
        }
    };

    let hunks = if opts.patch && (added != 0 || deleted != 0) {
        let sink = PatchSink {
            buf: Vec::new(),
            before: &before,
            after: &after,
        };
        Some(UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(opts.ctx)).consume()?)
    } else {
        None
    };
    // `before`/`after` borrow the two buffers, so release them before the move.
    drop(before);
    drop(after);
    Ok(IdxAnalysis {
        added,
        deleted,
        binary: false,
        hunks,
        old_data,
        new_data,
    })
}

/// The path as rendered, after `--relative` stripping — the same amount [`render`]
/// removes from the raw listing, so every format agrees on the printed name.
fn display_path(path: &BString, opts: &Opts) -> BString {
    match &opts.relative {
        Some(r) if !r.is_empty() => {
            let strip = (r.len() + 1).min(path.len());
            BString::from(path.as_slice()[strip..].to_vec())
        }
        _ => path.clone(),
    }
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

/// `compute_diffstat()`, including `builtin_diffstat()`'s rule that a plain `M` entry
/// with no added, no deleted and an unchanged mode is dropped outright.
fn compute_diffstat(deltas: &[Delta], analyses: &[IdxAnalysis], opts: &Opts) -> Vec<StatFile> {
    let mut out = Vec::new();
    for (d, an) in deltas.iter().zip(analyses) {
        if d.unmerged {
            out.push(StatFile {
                path: display_path(&d.path, opts),
                print_name: stat_print_name(d, opts),
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
        if d.status() == b'M' && added == 0 && deleted == 0 && d.src_mode == d.dst_mode && !an.binary {
            continue;
        }
        out.push(StatFile {
            path: display_path(&d.path, opts),
            print_name: stat_print_name(d, opts),
            added,
            deleted,
            binary: an.binary,
            is_unmerged: false,
        });
    }
    out
}

/// `fill_print_name()` plus `get_compact_summary()`.
fn stat_print_name(d: &Delta, opts: &Opts) -> Vec<u8> {
    let path = display_path(&d.path, opts);
    let mut name = quoted_name(&path);
    if !opts.stat.with_summary {
        return name;
    }
    let status = d.status();
    let comment: Option<&str> = if status == b'A' {
        Some(match d.dst_mode {
            0o120000 => "new +l",
            0o100755 => "new +x",
            _ => "new",
        })
    } else if status == b'D' {
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
    out.extend_from_slice(format!(" {files} file{} changed", if files == 1 { "" } else { "s" }).as_bytes());
    if insertions != 0 || deletions == 0 {
        out.extend_from_slice(
            format!(", {insertions} insertion{}(+)", if insertions == 1 { "" } else { "s" }).as_bytes(),
        );
    }
    if deletions != 0 || insertions == 0 {
        out.extend_from_slice(
            format!(", {deletions} deletion{}(-)", if deletions == 1 { "" } else { "s" }).as_bytes(),
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

/// `show_stats()`. A non-tty terminal width is git's `term_columns()` fallback of 80.
fn render_stat(out: &mut Vec<u8>, files: &[StatFile], opts: &Opts) {
    if files.is_empty() {
        return;
    }
    let sw = &opts.stat;
    let mut count: i64 = if sw.count != 0 { sw.count } else { files.len() as i64 };

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    let mut i: i64 = 0;
    while i < count && i < files.len() as i64 {
        let f = &files[i as usize];
        let change = (f.added + f.deleted) as i64;
        i += 1;
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
            out.extend_from_slice(format!("{:>width$}", "Bin", width = number_width.max(0) as usize).as_bytes());
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
            format!("{:>width$}", added + deleted, width = number_width.max(0) as usize).as_bytes(),
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
// --summary
// ---------------------------------------------------------------------------

/// `is_summary_empty()`.
fn summary_is_empty(deltas: &[Delta]) -> bool {
    for d in deltas {
        match d.status() {
            b'A' | b'D' => return false,
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
fn render_summary(out: &mut Vec<u8>, d: &Delta, opts: &Opts) {
    let path = display_path(&d.path, opts);
    match d.status() {
        b'D' => summary_mode_name(out, "delete", d.src_mode, &path),
        b'A' => summary_mode_name(out, "create", d.dst_mode, &path),
        _ => {
            if d.src_mode != 0 && d.dst_mode != 0 && d.src_mode != d.dst_mode {
                out.extend_from_slice(
                    format!(" mode change {} => {} ", mode_str(d.src_mode), mode_str(d.dst_mode)).as_bytes(),
                );
                out.extend_from_slice(&quoted_name(&path));
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

fn mode_str(mode: u32) -> String {
    format!("{mode:06o}")
}

// ---------------------------------------------------------------------------
// patch output
// ---------------------------------------------------------------------------

/// Render one delta as a `git diff` file section into `out` (`builtin_diff()`).
fn render_patch(out: &mut Vec<u8>, d: &Delta, an: &IdxAnalysis, opts: &Opts) {
    if d.unmerged {
        out.extend_from_slice(b"* Unmerged path ");
        out.extend_from_slice(display_path(&d.path, opts).as_ref());
        out.push(b'\n');
        return;
    }

    let path = display_path(&d.path, opts);
    // `-R` swaps the two prefixes, leaving the paths themselves alone.
    let (pa, pb): (&str, &str) = if opts.reverse {
        (&opts.dst_prefix, &opts.src_prefix)
    } else {
        (&opts.src_prefix, &opts.dst_prefix)
    };

    // `--full-index` shows the whole object name; otherwise git abbreviates to 7.
    let hexsz = d.src_id.kind().len_in_hex();
    let hlen = if opts.full_index { hexsz } else { 7 };
    let old_hash = if d.old_valid() {
        d.src_id.to_hex_with_len(hlen).to_string()
    } else {
        "0".repeat(hlen)
    };
    let new_hash = if d.new_valid() {
        d.dst_id.to_hex_with_len(hlen).to_string()
    } else {
        "0".repeat(hlen)
    };
    let content_differs = old_hash != new_hash;

    // `builtin_diff()` only emits the header once it has something to attach to it. A
    // stat-dirty file whose bytes and mode are unchanged produces nothing.
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
    out.extend_from_slice(&quote_two(pa, &path, pb, &path));
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
        quote_one(pa, &path)
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if d.new_valid() {
        quote_one(pb, &path)
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
        // The hunk buffer already carries git's canonical `+`/`-`/` ` markers.
        out.extend_from_slice(hunks);
    }
}

/// `DIFF_SYMBOL_FILEPAIR_{MINUS,PLUS}`: a name containing a space gets a trailing tab so
/// the header stays unambiguously parseable.
fn emit_file_line(out: &mut Vec<u8>, lead: &[u8], label: &[u8]) {
    out.extend_from_slice(lead);
    out.extend_from_slice(label);
    if label.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

/// `diff_line_prefix()` applied to every line of a rendered block.
fn prefix_lines(body: &[u8], prefix: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + prefix.len());
    for line in body.split_inclusive(|&b| b == b'\n') {
        out.extend_from_slice(prefix);
        out.extend_from_slice(line);
    }
    out
}

// ---------------------------------------------------------------------------
// path quoting (quote.c), matching git's default `core.quotePath=true`
// ---------------------------------------------------------------------------

/// The escape character for `b`, or `None` if it can be emitted verbatim. `Some(0)`
/// means "octal-escape this byte".
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
        0x00..=0x1f | 0x7f..=0xff => Some(0),
        _ => None,
    }
}

fn cq_needs_quote(s: &[u8]) -> bool {
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
    if !cq_needs_quote(s) {
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
    if !cq_needs_quote(prefix.as_bytes()) && !cq_needs_quote(s) {
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

/// Format one side of a hunk header (`@@ -<here> +<here> @@`), omitting the length when
/// it is 1 and using the pre-hunk line number when it is 0, like `git diff`.
fn fmt_range(start: u32, len: u32) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

/// A [`ConsumeHunk`] sink that renders unified-diff hunks into a byte buffer. The tokens
/// the differ compares may be whitespace-normalized, so line *content* is taken from the
/// original line tables, tracked by the cursors the hunk header establishes.
struct PatchSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
}

impl ConsumeHunk for PatchSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        self.buf.extend_from_slice(b"@@ -");
        self.buf
            .extend_from_slice(fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes());
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
            // A token without a terminator is the last line of a file that lacks a
            // trailing newline.
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
