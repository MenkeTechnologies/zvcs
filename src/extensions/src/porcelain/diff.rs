//! `git diff` — show changes between trees, the index and the worktree.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). Supported invocations:
//!
//! * `git diff`                       — index vs. worktree (unstaged changes)
//! * `git diff --cached [<rev>]`      — `<rev>`-tree (default `HEAD`) vs. the index (staged)
//! * `git diff --staged [<rev>]`      — alias of `--cached`
//! * `git diff <rev>`                 — `<rev>`-tree vs. the worktree
//! * `git diff <revA> <revB>`         — tree vs. tree (also `<revA>..<revB>`)
//!
//! Output formats follow `diff.c`'s model: `--raw`, `--numstat`, `--stat`,
//! `--shortstat`, `--dirstat`, `--name-only`, `--name-status` and the unified
//! patch can be combined, are emitted in git's fixed order (raw, numstat, stat,
//! shortstat, dirstat, summary, blank line, patch), and
//! `--name-only`/`--name-status`/`-s` suppress every other format exactly like
//! `diff_setup_done()` does.
//!
//! `--no-index <a> <b>` compares two paths on disk with no repository involved and
//! is answered by [`super::diff_no_index`], which the entry point dispatches to
//! before discovery is attempted.
//!
//! Beyond the format selectors, these options are honored: `-R` (reverse, for
//! tree/tree and `--cached` pairs), `-z`, `--full-index`, `--abbrev[=<n>]`,
//! `--no-prefix`/`--default-prefix`/`--src-prefix=`/`--dst-prefix=`/`--line-prefix=`,
//! `--summary`, `--compact-summary`/`--no-compact-summary`, `--diff-filter=<...>`,
//! `--color[=always|auto|never]`/`--no-color` and `--ws-error-highlight=<kind>` (the
//! patch and the stat graph are painted from the `color.diff.*` slots, with git's
//! `ws.c` whitespace-error markup driven by `core.whitespace`),
//! `--patch-with-raw`, `--patch-with-stat`, `--exit-code`, `--quiet`,
//! `--minimal`/`--diff-algorithm=<myers|minimal|histogram>`,
//! `--indent-heuristic`/`--no-indent-heuristic` (with the `diff.indentHeuristic`
//! default), `-O<file>` (with the `diff.orderFile` default),
//! `-W`/`--function-context` (which grows each hunk to its enclosing function
//! through the `xdl_emit_diff` port in [`super::diff_pairs`]),
//! `--dirstat[=<params>]`/`--dirstat-by-file[=<params>]`/`--cumulative` (whose
//! damage is `diffcore_count_changes()`'s, and whose walk is shared with
//! `diff-files`),
//! `-w`/`--ignore-all-space`, `-b`/`--ignore-space-change` and
//! `--ignore-space-at-eol` (a pair they leave with no changed line disappears
//! completely: `builtin_diff()` never flushes its deferred header, so there is no
//! `diff --git` block, `builtin_diffstat()` drops the entry so `--stat`,
//! `--numstat` and `--shortstat` stay silent, `diff_flush()` renders each pair
//! quietly before the raw/name formats print it so `--raw`, `--name-only` and
//! `--name-status` skip it too, and `--exit-code`/`--quiet` report no change
//! because `diff_from_contents` makes the status follow what was actually
//! emitted), and
//! merge-base ranges `<a>...<b>` (diffed as `merge-base(a,b)` against `b`).
//!
//! ### Submodules
//!
//! Gitlink (`160000`) changes render in all three of git's formats, selected by
//! `--submodule[=<format>]` (a bare `--submodule` is `log`, as `diff_opt_submodule()`
//! has it) and defaulted from `diff.submodule`:
//!
//! * `short` — the synthetic `Subproject commit <oid>` blob each side stands for.
//! * `log` — `Submodule <path> <a><..|...><b>[ (rewind)]:` and the
//!   `--left-right --first-parent` commit list, shared with [`super::diff_pairs`].
//! * `diff` — the same header, then the submodule's own `git diff` piped through
//!   with the gitlink path glued onto both prefixes, exactly as
//!   `show_submodule_inline_diff()` spawns it.
//!
//! Every pair source is covered: tree/tree, `--cached`, `<rev>` vs. the worktree
//! and the plain index-vs-worktree diff. A worktree gitlink's post-image is the
//! commit the submodule has checked out, and the `DIRTY_SUBMODULE_MODIFIED` bit it
//! carries prints git's `-dirty` marker (and `Submodule <path> contains modified
//! content` under `log`/`diff`). Untracked files inside a submodule are not damage:
//! `diff_setup_done()` sets `ignore_untracked_in_submodules` for every diff, so the
//! status walk is asked for the same thing.
//!
//! ### Rename, copy and rewrite detection
//!
//! The three `diffcore_std()` passes that reshape the change queue are ported in
//! [`super::diffcore_rename`] and run here: `-B`/`--break-rewrites[=<n>[/<m>]]`, then
//! `-M`/`--find-renames[=<n>]` or `-C`/`--find-copies[=<n>]` (twice, or
//! `--find-copies-harder`, for copies from unmodified sources), then the merge-broken
//! pass. `--no-renames`, `--rename-empty`/`--no-rename-empty` and the `-l<n>` rename
//! limit are honored, as are the `diff.renames` and `diff.renameLimit` config keys;
//! `git diff` is a porcelain, so detection defaults to on exactly as
//! `init_diff_ui_defaults()` makes it. Every format reports the result the way stock
//! git does — the `R<n>`/`C<n>` status letters and second path in `--raw` and
//! `--name-status`, `similarity index <n>%` with `rename from`/`rename to` (or
//! `copy from`/`copy to`) and `dissimilarity index <n>%` in the patch, the
//! `pfx{old => new}sfx` compression in `--stat`/`--numstat`/`--summary`, and the
//! `warning: exhaustive rename detection was skipped ...` pair `diff_result_code()`
//! prints on stderr after the diff.
//!
//! ### Honest limitations (bailed on with a precise message, never faked)
//!
//! * `-R` on a worktree diff bails: the worktree "new" side has no object id to move
//!   onto the old side within this pipeline.
//! * A type change (regular file ↔ symlink) in the worktree bails.
//! * `--ignore-submodules[=<when>]` is accepted and inert: gitlink changes are
//!   reported whatever it says, apart from the untracked files every diff ignores.
//! * `-c diff.submodule=<bad value>` warns once. Stock git repeats the warning when
//!   the value arrives through `-c` (measured: two lines from
//!   `git -c diff.submodule=bogus diff`, one from the same key in a config file);
//!   zvcs prints one either way, and matches stock byte for byte for the file case.
//! * The `patience` diff algorithm has no imara-diff equivalent and bails (the
//!   `--patience` alias and `diff.algorithm=patience` both surface the same error).
//! * `--line-prefix=<s>` is reproduced by a whole-buffer pass and so only tracks the
//!   newline-terminated formats; combining it with `-z` (NUL-separated records) is
//!   not byte-faithful.
//! * Magic pathspecs (`:(...)`) and glob pathspecs bail; literal path / directory-prefix
//!   filtering is supported.
//! * `--color-moved[=<mode>]`, `--color-moved-ws=`, `--word-diff[=]`,
//!   `--word-diff-regex=` and `--color-words[=]` are rejected — moved-block detection
//!   and the word-diff machinery are not ported, and accepting them while color is on
//!   would print lines in the wrong slot. `diff.colorMoved`/`diff.colorMovedWS` are
//!   likewise not read.
//! * `git diff` on an unmerged path renders the combined (`--cc`) patch, and only that —
//!   the duplicate stage-2-vs-worktree pair the raw/name/stat formats also report is not
//!   given a `diff --git` section. `--cached` renders git's `* Unmerged path` line.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::pipeline::{Mode, WorktreeRoots};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, ResourceKind, UnifiedDiff};
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;

use super::diff_color;
use super::diff_files;
use super::diffcore_rename;

// ---------------------------------------------------------------------------
// output formats — mirrors DIFF_FORMAT_* in diff.h
// ---------------------------------------------------------------------------

const F_RAW: u32 = 1 << 0;
const F_NUMSTAT: u32 = 1 << 1;
const F_DIFFSTAT: u32 = 1 << 2;
const F_SHORTSTAT: u32 = 1 << 3;
const F_NAME: u32 = 1 << 4;
const F_NAME_STATUS: u32 = 1 << 5;
const F_PATCH: u32 = 1 << 6;
const F_NO_OUTPUT: u32 = 1 << 7;
const F_SUMMARY: u32 = 1 << 8;
const F_DIRSTAT: u32 = 1 << 9;

/// The exact `git diff` usage stream, printed on a usage error (exit 129).
const USAGE: &str = "usage: git diff [<options>] [<commit>] [--] [<path>...]\n   or: git diff [<options>] --cached [--merge-base] [<commit>] [--] [<path>...]\n   or: git diff [<options>] [--merge-base] <commit> [<commit>...] <commit> [--] [<path>...]\n   or: git diff [<options>] <commit>...<commit> [--] [<path>...]\n   or: git diff [<options>] <blob> <blob>\n   or: git diff [<options>] --no-index [--] <path> <path> [<pathspec>...]\n\ncommon diff options:\n  -z            output diff-raw with lines terminated with NUL.\n  -p            output patch format.\n  -u            synonym for -p.\n  --patch-with-raw\n                output both a patch and the diff-raw format.\n  --stat        show diffstat instead of patch.\n  --numstat     show numeric diffstat instead of patch.\n  --patch-with-stat\n                output a patch and prepend its diffstat.\n  --name-only   show only names of changed files.\n  --name-status show names and status of changed files.\n  --full-index  show full object name on index lines.\n  --abbrev=<n>  abbreviate object names in diff-tree header and diff-raw.\n  -R            swap input file pairs.\n  -B            detect complete rewrites.\n  -M            detect renames.\n  -C            detect copies.\n  --find-copies-harder\n                try unchanged files as candidate for copy detection.\n  -l<n>         limit rename attempts up to <n> paths.\n  -O<file>      reorder diffs according to the <file>.\n  -S<string>    find filepair whose only one side contains the string.\n  --pickaxe-all\n                show all files diff when -S is used and hit is found.\n  -a  --text    treat all files as text.\n\n";

/// Print the usage stream and return git's usage-error exit code (129).
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Rendering options resolved from the command line and shared by every output
/// format (raw / name / patch). Mirrors the fields of `struct diff_options` that
/// affect byte-level formatting.
struct Render {
    /// Object-name abbreviation length for the patch `index` line.
    abbrev: usize,
    /// The same for `--raw`, which `--no-abbrev` widens on its own.
    raw_abbrev: usize,
    /// `--full-index`: emit the full object name on the patch `index` line.
    full_index: bool,
    /// `--binary`: emit a `GIT binary patch` payload for a binary pair, and widen
    /// that pair's `index` line to full object names.
    binary: bool,
    /// `-z`: terminate `--raw`/`--name-only`/`--name-status` records with NUL and
    /// suppress path C-quoting.
    z: bool,
    /// The `a/` (source) path prefix; `b/` under `-R`, empty under `--no-prefix`.
    src_prefix: Vec<u8>,
    /// The `b/` (destination) path prefix.
    dst_prefix: Vec<u8>,
    hash_kind: gix::hash::Kind,
}

/// git's `enum diff_submodule_format` (diff.h), selected by `--submodule[=<format>]`
/// and `diff.submodule`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SubmoduleFormat {
    /// `DIFF_SUBMODULE_SHORT`: the default — a gitlink pair diffs as the synthetic
    /// one-line `Subproject commit <oid>` blob each side stands for.
    Short,
    /// `DIFF_SUBMODULE_LOG`: `show_submodule_diff_summary()`'s header plus the
    /// `--left-right --first-parent` commit list.
    Log,
    /// `DIFF_SUBMODULE_INLINE_DIFF`: the same header, then the submodule's own
    /// `git diff` piped through with the gitlink path glued onto both prefixes.
    InlineDiff,
}

/// `parse_submodule_params()` (diff.c:194): the three format names, or `None` for
/// the value git refuses.
fn parse_submodule_params(value: &str) -> Option<SubmoduleFormat> {
    match value {
        "log" => Some(SubmoduleFormat::Log),
        "short" => Some(SubmoduleFormat::Short),
        "diff" => Some(SubmoduleFormat::InlineDiff),
        _ => None,
    }
}

/// How lines are compared, mirroring xdiff's `XDF_*` whitespace flags.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Whitespace {
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

/// The "new" side of a change.
enum NewSide {
    /// The path no longer exists (a deletion).
    Absent,
    /// A concrete object in the database (tree/index diffs).
    Blob(ObjectId, EntryKind),
    /// Content that must be read from the worktree at this path (worktree diffs).
    Worktree(EntryKind),
}

/// A single file-level change, normalized across all diff sources.
struct Delta {
    path: BString,
    /// `None` means the path did not exist before (an addition).
    old: Option<(ObjectId, EntryKind)>,
    new: NewSide,
    /// An unmerged (conflicted) index entry: rendered as status `U`, counted as
    /// zero changes by the stat formats, and never diffed through the blob pipeline.
    unmerged: bool,
    /// Stage 2 / stage 3 blobs, present only for the combined (`--cc`) patch of an
    /// unmerged worktree path.
    stages: Option<(ObjectId, ObjectId)>,
    /// The rename/copy *source* path (git's `p->one->path`) when detection matched
    /// this destination to a differently-named source. `None` means both sides of the
    /// pair name the same path.
    src_path: Option<BString>,
    /// git's `p->score`: the similarity of a rename/copy, or the dissimilarity of a
    /// `-B` rewrite, in [`diffcore_rename::MAX_SCORE`] units. Zero when unscored.
    score: u32,
    /// The status letter assigned by `diff_resolve_rename_copy()`, or `0` when the
    /// diffcore rename pass did not run and [`status_char`] must derive it.
    status: u8,
    /// The object id `--raw` prints for the post-image. A worktree side normally has
    /// none (git prints all-zero), but `hash_filespec()` gives one to every rename
    /// candidate, and `diff_flush_raw()` then prints that real id.
    new_id: Option<ObjectId>,
    /// git's `p->two->dirty_submodule`: the `DIRTY_SUBMODULE_*` bits describing what
    /// the submodule worktree holds beyond its recorded commit. Always zero for a
    /// pair whose post-image is an object.
    dirty_submodule: u8,
    /// The commit a *worktree* gitlink post-image stands for: the one the submodule
    /// currently has checked out, which `run_diff_files()` writes into `p->two->oid`
    /// while leaving the filespec invalid. `None` for every other pair.
    new_commit: Option<ObjectId>,
}

impl Delta {
    fn new_kind(&self) -> Option<EntryKind> {
        match self.new {
            NewSide::Absent => None,
            NewSide::Blob(_, k) | NewSide::Worktree(k) => Some(k),
        }
    }

    /// The pre-image path: the rename/copy source when there is one, else [`Delta::path`].
    fn old_path(&self) -> &BString {
        self.src_path.as_ref().unwrap_or(&self.path)
    }

    /// `DIFF_FILE_VALID(p->two)`: whether the pair has a post-image at all, which is
    /// what `show_dirstat()` tests before charging a deletion's whole size as damage.
    fn new_valid(&self) -> bool {
        !matches!(self.new, NewSide::Absent)
    }

    /// `true` for a rename or copy, i.e. a pair whose two sides have different paths.
    fn renamed(&self) -> bool {
        matches!(self.status, b'R' | b'C')
    }

    /// git's `complete_rewrite`: a `-B` break that survived as a modification, which
    /// renders as a whole-file replacement rather than a hunk-by-hunk diff.
    fn complete_rewrite(&self) -> bool {
        self.status == b'M' && self.score != 0
    }

    fn plain(path: BString, old: Option<(ObjectId, EntryKind)>, new: NewSide) -> Self {
        Delta {
            path,
            old,
            new,
            unmerged: false,
            stages: None,
            src_path: None,
            score: 0,
            status: 0,
            new_id: None,
            dirty_submodule: 0,
            new_commit: None,
        }
    }

    /// `builtin_diff()`'s submodule branch (diff.c:3870): both sides are either
    /// absent or a gitlink, and the pair is not the phoney one a `--stat`-only run
    /// queues. Such a pair renders from the submodule's own repository under
    /// `--submodule=log` / `--submodule=diff` instead of as a blob diff.
    fn is_submodule_pair(&self) -> bool {
        let old_ok = match self.old {
            None => true,
            Some((_, k)) => k == EntryKind::Commit,
        };
        let new_ok = match self.new {
            NewSide::Absent => true,
            NewSide::Blob(_, k) | NewSide::Worktree(k) => k == EntryKind::Commit,
        };
        old_ok && new_ok && (self.old.is_some() || self.new_valid())
    }
}

/// Per-delta blob analysis: the new-side object id plus line counts and the
/// rendered hunks (only computed when a patch is actually requested).
struct Analysis {
    new_id: ObjectId,
    added: u32,
    deleted: u32,
    binary: bool,
    /// `None` when the two sides are byte-identical (e.g. a pure mode change).
    hunks: Option<Vec<u8>>,
    /// `check_blank_at_eof()`: where the run of blank lines the change lengthened
    /// begins in the pre- and post-image. `(0, 0)` switches the check off.
    blank_at_eof: (usize, usize),
    /// `show_dirstat()`'s per-file damage in its default (content) mode: how many
    /// bytes of this file the change touched, from `diffcore_count_changes()`.
    /// Computed only when `--dirstat` asked for it, since it costs a second pass
    /// over both images — and for a binary pair, a read of both blobs.
    damage: u64,
    /// The pre- and post-images of a binary pair, kept only when `--binary` will
    /// turn them into a `GIT binary patch`. The blob pipeline withholds the data
    /// for a binary pair, so this is a deliberate second read and is skipped
    /// whenever the payload is not going to be emitted.
    images: Option<(Vec<u8>, Vec<u8>)>,
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

pub fn diff(args: &[String]) -> Result<ExitCode> {
    // `builtin_diff()` splits before anything else: `--no-index` compares two paths
    // on disk and needs no repository, so it cannot wait for discovery below.
    if args.iter().take_while(|a| *a != "--").any(|a| a == "--no-index") {
        return super::diff_no_index::run(args);
    }
    let mut cached = false;
    let mut ctx: u32 = 3;
    let mut ws = Whitespace::Keep;
    let mut fmt: u32 = 0;
    let mut trailing_paths: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    // Formatting / behavior options resolved below.
    let mut reverse = false;
    let mut z = false;
    let mut full_index = false;
    let mut binary = false;
    let mut want_exit_code = false;
    let mut quiet = false;
    // `--relative[=<path>]`: the prefix stripped from every reported path (with a
    // trailing slash), or `None` when paths are shown from the repository root.
    let mut relative: Option<String> = None;
    // `--check`: `DIFF_FORMAT_CHECKDIFF`.
    let mut check = false;
    let mut src_prefix: Vec<u8> = b"a/".to_vec();
    let mut dst_prefix: Vec<u8> = b"b/".to_vec();
    // `--line-prefix=<s>`: prepended to every emitted line (`diff_line_prefix()`).
    let mut line_prefix: Vec<u8> = Vec::new();
    // `--compact-summary`: annotate `--stat` names with create/delete/mode info.
    let mut compact_summary = false;
    let mut func_context = false;
    // `--dirstat`'s parameter block (`struct dirstat_opts`), shared with the
    // `diff-files`/`diff-index` port that renders it.
    let mut dirstat = super::diff_files::DirStat::default();
    let mut diff_filter: Option<Vec<u8>> = None;
    let mut algorithm: Option<gix::diff::blob::Algorithm> = None;
    // `XDF_INDENT_HEURISTIC`, on unless `diff.indentHeuristic` or
    // `--no-indent-heuristic` turns it off (`git_diff_basic_config()` sets
    // `diff_indent_heuristic`, whose default is 1).
    let mut indent_heuristic = true;
    // `-O<file>` / `diff.orderFile` (`diffcore_order`): the queue is reordered so the
    // paths matching an earlier pattern in the file come first.
    let mut order_file: Option<String> = None;
    // Default resolved from `core.abbrev` after repo discovery (see below);
    // `7` is only a placeholder until then. `--abbrev[=<n>]` overrides explicitly.
    let mut abbrev: usize = 7;
    let mut abbrev_explicit = false;
    // `--no-abbrev`: the width `--raw` prints, when it differs from the `index` line.
    let mut raw_abbrev: Option<usize> = None;
    // `diff.algorithm` default, applied after argument parsing so a `--minimal` /
    // `--histogram` / `--diff-algorithm=` flag always wins (git precedence).
    let mut config_algorithm: Option<ConfigAlgorithm> = None;
    // `--stat` width limits (`show_stats()`), `0` == unset. Seeded from
    // `diff.statNameWidth`/`diff.statGraphWidth` below, then overwritten by an
    // explicit `--stat-name-width=`/`--stat-graph-width=` flag (git precedence;
    // a `--stat-name-width=0` flag legitimately overrides a positive config).
    let mut stat_name_width: i64 = 0;
    let mut stat_graph_width: i64 = 0;
    // `diff.suppressBlankEmpty`: emit an empty context line as `"\n"` rather than
    // the default `" \n"` (`fn_out_consume()`); no CLI flag exists for it.
    let mut suppress_blank_empty = false;
    // `--color[=<when>]` / `--no-color`. `None` leaves the decision to
    // `color.diff` / `diff.color` / `color.ui` and the terminal test.
    let mut color_when: Option<diff_color::ColorWhen> = None;
    // `--ws-error-highlight=<kind>`, seeded from `diff.wsErrorHighlight` (default
    // `WSEH_NEW`) once the repository's config is readable, below.
    let mut ws_error_highlight: u32;
    // `--color-moved*` / `--word-diff*` / `--color-words`, layered over
    // `diff.colorMoved` / `diff.colorMovedWS` / `diff.wordRegex` below.
    let mut move_word = diff_color::MoveWordOpts::default();
    // The `diffcore_std()` rename/copy/break knobs. `git diff` is a porcelain, so
    // `init_diff_ui_defaults()` has already turned rename detection on by default;
    // `diff.renames` and the `-M`/`-C`/`--no-renames` flags override it below.
    let mut ro = diffcore_rename::Options {
        detect_rename: diffcore_rename::DETECT_RENAME,
        ..Default::default()
    };
    // `diff.renameLimit` (`git_diff_basic_config()`), applied in `diff_setup_done()`
    // only when `-l<n>` did not already set an explicit limit.
    let mut rename_limit_default = diffcore_rename::DEFAULT_RENAME_LIMIT;
    // `--submodule[=<format>]` / `diff.submodule`, seeded from config below.
    let mut submodule_format = SubmoduleFormat::Short;

    // Revisions and pathspecs are classified in a single left-to-right pass, so an
    // invalid option value, an ambiguous positional, and any "too many operands"
    // error surface in git's own argument order — `setup_revisions()` is one pass,
    // and the earliest failing token is the one whose exit code git reports.
    let mut repo = gix::discover(".")?;
    // Object-heavy path: give gix the caches it does not enable by default —
    // a decoded-object cache and a git-sized delta-base cache (gix ships a
    // 64-entry linked list; git's core.deltaBaseCacheLimit default is 96MB).
    repo.object_cache_size_if_unset(16 * 1024 * 1024);
    repo.objects.set_pack_cache(|| {
        Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(96 * 1024 * 1024))
    });

    // Config-provided defaults, overridden by the CLI flags parsed below (git's
    // precedence: diff.context < -U, diff.srcPrefix/dstPrefix/noPrefix < the
    // corresponding --*-prefix / --no-prefix flags).
    {
        let snap = repo.config_snapshot();
        if let Some(n) = snap.integer("diff.context") {
            if n >= 0 {
                ctx = n as u32;
            }
        }
        if snap.boolean("diff.noPrefix") == Some(true) {
            src_prefix.clear();
            dst_prefix.clear();
        } else {
            if let Some(p) = snap.string("diff.srcPrefix") {
                src_prefix = p.into();
            }
            if let Some(p) = snap.string("diff.dstPrefix") {
                dst_prefix = p.into();
            }
        }
        // `diff.algorithm` names the default algorithm. git validates it while
        // loading config — an unknown name is a hard error (exit 128) even when a
        // CLI flag would override it — so classify it eagerly here. `patience` is a
        // valid name git renders, but imara-diff has no patience variant, so it is
        // remembered as unrenderable and only rejected if actually used below.
        if let Some(name) = snap.string("diff.algorithm") {
            config_algorithm = Some(parse_config_algorithm(name.as_ref())?);
        }
        // `diff.indentHeuristic` (`git_diff_basic_config()`): the default landing spot
        // for a slidable hunk. A command-line `--[no-]indent-heuristic` overrides it.
        if let Some(b) = snap.boolean("diff.indentHeuristic") {
            indent_heuristic = b;
        }
        // `diff.orderFile` (`git_diff_ui_config()`): `diff_setup()` seeds
        // `options->orderfile` from it before parse-options runs, so a `-O<file>`
        // on the command line simply overwrites it below.
        if let Some(p) = snap.string("diff.orderFile") {
            order_file = Some(p.to_str_lossy().into_owned());
        }
        // `diff.statNameWidth`/`diff.statGraphWidth` cap the `--stat` name/graph
        // columns (`git_diff_ui_config()` -> `diff_stat_name_width` /
        // `diff_stat_graph_width`). Only a positive limit has any effect in
        // `show_stats()`; a non-positive config leaves the column uncapped.
        if let Some(n) = snap.integer("diff.statNameWidth") {
            if n > 0 {
                stat_name_width = n;
            }
        }
        if let Some(n) = snap.integer("diff.statGraphWidth") {
            if n > 0 {
                stat_graph_width = n;
            }
        }
        if snap.boolean("diff.suppressBlankEmpty") == Some(true) {
            suppress_blank_empty = true;
        }
        // `diff.renames` (`git_diff_ui_config()`): `false` disables detection,
        // `copies`/`copy` asks for `-C`, anything else truthy is plain `-M`.
        if let Some(v) = snap.string("diff.renames") {
            ro.detect_rename = diffcore_rename::config_rename(Some(v.as_ref()));
        }
        // `diff.renameLimit` (`git_diff_basic_config()`).
        if let Some(n) = snap.integer("diff.renameLimit") {
            rename_limit_default = n;
        }
        // `diff.submodule` (`git_diff_ui_config()`, diff.c:453): a value git cannot
        // parse is a warning, not a fatal, and leaves the format alone.
        if let Some(v) = snap.string("diff.submodule") {
            let raw = v.to_str_lossy().into_owned();
            match parse_submodule_params(&raw) {
                Some(f) => submodule_format = f,
                None => eprintln!(
                    "warning: Unknown value for 'diff.submodule' config variable: '{raw}'"
                ),
            }
        }
    }
    // `diff.wsErrorHighlight` (`git_diff_basic_config()`): a value git rejects is
    // a fatal config error, reported before any diff is computed.
    match diff_color::ws_error_highlight_default(&repo) {
        Ok(v) => ws_error_highlight = v,
        Err(bad) => {
            eprintln!("error: unknown value for config 'diff.wserrorhighlight': {bad}");
            return Ok(ExitCode::from(128));
        }
    }

    let mut revs: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut in_rev_region = true;
    // The first argument git would not resolve to an option, held until the whole command
    // line has been read. See [`invalid_option`].
    let mut invalid_arg: Option<String> = None;
    // `--ws-error-highlight <kind>`, `--color-moved-ws <modes>` and
    // `--word-diff-regex <re>` all spell their value as the next argument when it is
    // not glued on with `=`; parse-options consumes it before anything else, `--`
    // included. This holds the flag still waiting for that value.
    let mut pending_value: Option<String> = None;

    for a in args {
        if let Some(flag) = pending_value.take() {
            if flag == "--ws-error-highlight" {
                match diff_color::parse_ws_error_highlight(a) {
                    Ok(v) => ws_error_highlight = v,
                    Err(accepted) => {
                        eprintln!(
                            "error: unknown value after ws-error-highlight={}",
                            &a[..accepted]
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            } else if flag == "-O" {
                order_file = Some(a.clone());
            } else if let Some(Err(msg)) =
                move_word.parse_flag(&format!("{flag}={a}"), &mut color_when)
            {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            continue;
        }
        if diff_color::needs_separate_value(a) {
            pending_value = Some(a.clone());
            continue;
        }
        if after_dashdash {
            trailing_paths.push(a.clone());
            continue;
        }
        // `--color-moved[=<mode>]`, `--color-moved-ws=<modes>`, `--word-diff[=<mode>]`,
        // `--word-diff-regex=<re>` and `--color-words[=<re>]`.
        if let Some(res) = move_word.parse_flag(a, &mut color_when) {
            if let Err(msg) = res {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            continue;
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "--cached" | "--staged" => cached = true,
            "--raw" => fmt |= F_RAW,
            // `--check`: report whitespace errors instead of a diff (`diff.c`s
            // `DIFF_FORMAT_CHECKDIFF`), which is why it replaces every other format.
            "--check" => check = true,
            "--no-check" => check = false,
            "--numstat" => fmt |= F_NUMSTAT,
            "--shortstat" => fmt |= F_SHORTSTAT,
            "--stat" => fmt |= F_DIFFSTAT,
            "--name-only" => fmt |= F_NAME,
            "--name-status" => fmt |= F_NAME_STATUS,
            "-p" | "-u" | "--patch" => fmt |= F_PATCH,
            "-s" | "--no-patch" => fmt |= F_NO_OUTPUT,
            "--summary" => fmt |= F_SUMMARY,
            // `--dirstat[=<params>]` / `--dirstat-by-file[=<params>]` / `--cumulative`
            // (`diff_opt_dirstat()`), all of which turn the format on.
            "--dirstat" => fmt |= F_DIRSTAT,
            "--dirstat-by-file" => {
                fmt |= F_DIRSTAT;
                dirstat.by_file = true;
            }
            "--cumulative" => {
                fmt |= F_DIRSTAT;
                dirstat.cumulative = true;
            }
            s if s.starts_with("--dirstat=") || s.starts_with("--dirstat-by-file=") => {
                let by_file = s.starts_with("--dirstat-by-file=");
                let params = s.split_once('=').map(|(_, v)| v).unwrap_or_default();
                let errors = super::diff_files::parse_dirstat_params(params, &mut dirstat);
                if !errors.is_empty() {
                    // `parse_dirstat_opt()`'s `die()`, carrying the accumulated text.
                    eprint!("fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n");
                    return Ok(ExitCode::from(128));
                }
                if by_file {
                    dirstat.by_file = true;
                }
                fmt |= F_DIRSTAT;
            }
            // `--compact-summary` (`diff_opt_compact_summary()`): sets the
            // stat-with-summary flag AND turns on `--stat`. `--no-compact-summary`
            // only clears the flag; it never touches the output format.
            "--compact-summary" => {
                compact_summary = true;
                fmt |= F_DIFFSTAT;
            }
            "--no-compact-summary" => compact_summary = false,
            // `--patch-with-raw` / `--patch-with-stat` request two formats at once.
            "--patch-with-raw" => fmt |= F_PATCH | F_RAW,
            "--patch-with-stat" => fmt |= F_PATCH | F_DIFFSTAT,
            "-w" | "--ignore-all-space" => ws = Whitespace::IgnoreAll,
            "-b" | "--ignore-space-change" => ws = Whitespace::IgnoreChange,
            "--ignore-space-at-eol" => ws = Whitespace::IgnoreAtEol,
            "-R" => reverse = true,
            "-z" => z = true,
            "--exit-code" => want_exit_code = true,
            "--quiet" => {
                quiet = true;
                want_exit_code = true;
            }
            "--full-index" => full_index = true,
            "--binary" => binary = true,
            "--abbrev" => {
                abbrev = 7;
                abbrev_explicit = true;
                raw_abbrev = None;
            }
            // `--no-abbrev` is `revs->abbrev = 0`: the raw format prints whole ids,
            // while the `index` line falls back to the configured default.
            "--no-abbrev" => {
                raw_abbrev = Some(repo.object_hash().len_in_hex());
                abbrev_explicit = false;
            }
            "--no-prefix" => {
                src_prefix.clear();
                dst_prefix.clear();
            }
            "--default-prefix" => {
                src_prefix = b"a/".to_vec();
                dst_prefix = b"b/".to_vec();
            }
            // Diff-algorithm selection; the last flag on the command line wins.
            "--minimal" => algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal),
            "--myers" => algorithm = Some(gix::diff::blob::Algorithm::Myers),
            "--histogram" => algorithm = Some(gix::diff::blob::Algorithm::Histogram),
            // `--patience` aliases `--diff-algorithm=patience`.
            "--patience" => algorithm = Some(gix::diff::blob::Algorithm::Patience),
            // Accepted here rather than implemented.
            //
            // Rename detection is *not* in this list any more — `-M`, `-C`,
            // `--find-renames`, `--find-copies`, `--no-renames` and
            // `--rename-empty`/`--no-rename-empty` are parsed above and fed to
            // `diffcore_rename`, so they change the output exactly as stock git's do.
            //
            // KNOWN DIVERGENCE, do not describe these as no-ops: `--ignore-blank-lines`
            // genuinely changes stock's output and is not honored here. Measured against
            // git 2.55.0 on a blank-line-only edit, stock prints nothing and this prints
            // the full hunk. `diff_pairs.rs` has the real implementation (a port of
            // `xdl_mark_ignorable_lines`); wiring this command through it is the fix.
            // The remaining entries are believed to match zvcs's default behavior, but
            // that has not been measured flag by flag — treat them as unverified.
            "--ignore-cr-at-eol" => ws = Whitespace::IgnoreCrAtEol,
            "--ignore-blank-lines" | "--text" | "-a"
            | "--no-ext-diff" | "--ext-diff" | "--textconv"
            | "--no-textconv" | "--ita-invisible-in-index" | "--ita-visible-in-index" => {}
            // `XDF_INDENT_HEURISTIC` (`OPT_BIT` on `xdl_opts`): where a hunk that can
            // slide freely finally lands.
            // `-W`/`--function-context` (`XDL_EMIT_FUNCCONTEXT`): grow every hunk
            // out to the enclosing function on both ends.
            "-W" | "--function-context" => func_context = true,
            "--no-function-context" => func_context = false,
            "--indent-heuristic" => indent_heuristic = true,
            "--no-indent-heuristic" => indent_heuristic = false,
            // `-O<file>` (`OPT_FILENAME('O', ...)`): the value may be glued on or be
            // the next argument, and the last one on the line wins.
            "-O" => pending_value = Some(a.to_string()),
            s if s.starts_with("-O") => order_file = Some(s["-O".len()..].to_owned()),
            // `--color[=<when>]` / `--no-color` (`OPT_COLOR_FLAG`).
            "--color" => color_when = Some(diff_color::ColorWhen::Always),
            "--no-color" => color_when = Some(diff_color::ColorWhen::Never),
            s if s.starts_with("--color=") => {
                match diff_color::parse_color_when(&s["--color=".len()..]) {
                    Some(w) => color_when = Some(w),
                    None => {
                        eprintln!(
                            "error: option `color' expects \"always\", \"auto\", or \"never\""
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--ws-error-highlight=<kind>` (`diff_opt_ws_error_highlight()`).
            s if s.starts_with("--ws-error-highlight=") => {
                let raw = &s["--ws-error-highlight=".len()..];
                match diff_color::parse_ws_error_highlight(raw) {
                    Ok(v) => ws_error_highlight = v,
                    Err(accepted) => {
                        eprintln!(
                            "error: unknown value after ws-error-highlight={}",
                            &raw[..accepted]
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            "--ws-error-highlight" => pending_value = Some(a.to_string()),
            s if s == "--ignore-submodules" || s.starts_with("--ignore-submodules=") => {}
            // `--submodule[=<format>]` (`diff_opt_submodule()`, diff.c:5916): the
            // bare form is `log`, not `short`.
            "--submodule" => submodule_format = SubmoduleFormat::Log,
            s if s.starts_with("--submodule=") => {
                let raw = &s["--submodule=".len()..];
                match parse_submodule_params(raw) {
                    Some(f) => submodule_format = f,
                    None => {
                        eprintln!(
                            "error: failed to parse --submodule option parameter: '{raw}'"
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--diff-filter=") => {
                diff_filter = Some(s.as_bytes()["--diff-filter=".len()..].to_vec());
            }
            s if s.starts_with("--abbrev=") => {
                // `diff_opt_parse()` reads the value with `strtoul()` and clamps it
                // to [MINIMUM_ABBREV, hexsz]; a non-numeric value is zero, never an
                // error.
                abbrev = crate::abbrev::parse_abbrev_arg(
                    &s["--abbrev=".len()..],
                    repo.object_hash().len_in_hex(),
                );
                abbrev_explicit = true;
                raw_abbrev = None;
            }
            // `--relative[=<path>]`/`--no-relative`: `diff_opt_relative()`. With no
            // value the prefix is the current directory inside the repository;
            // with one it is that path. Either way git stores it with a trailing
            // slash so a plain prefix match cannot cross a name boundary.
            "--relative" => relative = Some(cwd_prefix(&repo)),
            "--no-relative" => relative = None,
            s if s.starts_with("--relative=") => {
                let mut p = s["--relative=".len()..].to_string();
                if !p.is_empty() && !p.ends_with('/') {
                    p.push('/');
                }
                relative = Some(p);
            }
            s if s.starts_with("--src-prefix=") => {
                src_prefix = s.as_bytes()["--src-prefix=".len()..].to_vec();
            }
            s if s.starts_with("--dst-prefix=") => {
                dst_prefix = s.as_bytes()["--dst-prefix=".len()..].to_vec();
            }
            s if s.starts_with("--line-prefix=") => {
                line_prefix = s.as_bytes()["--line-prefix=".len()..].to_vec();
            }
            s if s.starts_with("--diff-algorithm=") => {
                match &s["--diff-algorithm=".len()..] {
                    "myers" | "default" => algorithm = Some(gix::diff::blob::Algorithm::Myers),
                    "minimal" => algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal),
                    "histogram" => algorithm = Some(gix::diff::blob::Algorithm::Histogram),
                    "patience" => algorithm = Some(gix::diff::blob::Algorithm::Patience),
                    other => crate::git_fatal!("diff algorithm {other:?} is not available"),
                }
            }
            // `--stat-name-width=<n>` / `--stat-graph-width=<n>` override the
            // `diff.statNameWidth`/`diff.statGraphWidth` defaults (`diff_opt_stat()`),
            // and, like every `--stat*` flag, request the diffstat format.
            s if s.starts_with("--stat-name-width=") => {
                fmt |= F_DIFFSTAT;
                match s["--stat-name-width=".len()..].parse::<i64>() {
                    Ok(n) => stat_name_width = n,
                    Err(_) => {
                        eprintln!("error: stat-name-width expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--stat-graph-width=") => {
                fmt |= F_DIFFSTAT;
                match s["--stat-graph-width=".len()..].parse::<i64>() {
                    Ok(n) => stat_graph_width = n,
                    Err(_) => {
                        eprintln!("error: stat-graph-width expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--stat=") || s.starts_with("--stat-") => fmt |= F_DIFFSTAT,
            // ---- rename / copy / break detection (`diffcore_std()`) -----------
            // `diff_opt_find_renames()`: an absent value parses as score 0, which
            // `diffcore_rename()` then replaces with DEFAULT_RENAME_SCORE.
            "-M" | "--find-renames" => {
                ro.rename_score = 0;
                ro.detect_rename = diffcore_rename::DETECT_RENAME;
            }
            s if s.starts_with("--find-renames=") || (s.starts_with("-M") && s.len() > 2) => {
                let raw = s.strip_prefix("--find-renames=").unwrap_or(&s[2..]);
                let (score, rest) = diffcore_rename::parse_rename_score(raw);
                if !rest.is_empty() {
                    eprintln!("error: invalid argument to find-renames");
                    return Ok(ExitCode::from(129));
                }
                ro.rename_score = score;
                ro.detect_rename = diffcore_rename::DETECT_RENAME;
            }
            // `diff_opt_find_copies()`: a second `-C` means `--find-copies-harder`.
            "-C" | "--find-copies" => {
                ro.rename_score = 0;
                if ro.detect_rename == diffcore_rename::DETECT_COPY {
                    ro.find_copies_harder = true;
                } else {
                    ro.detect_rename = diffcore_rename::DETECT_COPY;
                }
            }
            s if s.starts_with("--find-copies=") || (s.starts_with("-C") && s.len() > 2) => {
                let raw = s.strip_prefix("--find-copies=").unwrap_or(&s[2..]);
                let (score, rest) = diffcore_rename::parse_rename_score(raw);
                if !rest.is_empty() {
                    eprintln!("error: invalid argument to find-copies");
                    return Ok(ExitCode::from(129));
                }
                ro.rename_score = score;
                if ro.detect_rename == diffcore_rename::DETECT_COPY {
                    ro.find_copies_harder = true;
                } else {
                    ro.detect_rename = diffcore_rename::DETECT_COPY;
                }
            }
            "--find-copies-harder" => ro.find_copies_harder = true,
            "--no-find-copies-harder" => ro.find_copies_harder = false,
            "--no-renames" => ro.detect_rename = 0,
            "--rename-empty" => ro.rename_empty = true,
            "--no-rename-empty" => ro.rename_empty = false,
            // `diff_opt_break_rewrites()`: `-B[<n>][/<m>]`, packed as `n | (m << 16)`.
            "-B" | "--break-rewrites" => ro.break_opt = 0,
            s if s.starts_with("--break-rewrites=") || (s.starts_with("-B") && s.len() > 2) => {
                let raw = s.strip_prefix("--break-rewrites=").unwrap_or(&s[2..]);
                match diffcore_rename::parse_break_opt(raw) {
                    Ok(v) => ro.break_opt = v,
                    Err(()) => {
                        eprintln!("error: break-rewrites expects <n>/<m> form");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `-l<n>`: `OPT_INTEGER('l', NULL, &options->rename_limit, ...)`.
            "-l" => {
                eprintln!("error: switch `l' requires a value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("-l") && s.len() > 2 => {
                match crate::optint::integer(&crate::optint::short_opt('l'), &s[2..]) {
                    Ok(n) => ro.rename_limit = n,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `-U` / `--unified[=<n>]`: git's `diff_opt_unified()` enables patch
            // output unconditionally, so any of these implies `-p` even alongside
            // `--raw`/`--stat`/`--numstat`. A bare `-U` / `--unified` keeps the
            // default context; an attached value is parsed with strtol semantics.
            "-U" | "--unified" => fmt |= F_PATCH,
            s if s.starts_with("-U") || s.starts_with("--unified=") => {
                let val = s.strip_prefix("--unified=").unwrap_or(&s[2..]);
                match parse_unified(val) {
                    UnifiedValue::Context(n) => {
                        ctx = n;
                        fmt |= F_PATCH;
                    }
                    UnifiedValue::NotNumeric => {
                        eprintln!("error: --unified expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                    UnifiedValue::Negative => {
                        eprintln!("error: --unified expects a non-negative integer");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // A name git itself does not resolve is its usage error, not a gap here. The
            // report is deferred to after the loop because which of `cmd_diff`'s four
            // dispatch targets receives the leftover — and so whether the `error:` line
            // precedes the usage block — depends on whether a revision turns up, which
            // may still be later in argv (`git diff --no-such-flag HEAD`).
            s if s.starts_with('-') && !is_known_option(s) => {
                invalid_arg.get_or_insert_with(|| s.to_owned());
            }
            s if s.starts_with('-') => bail!("unsupported option {s:?}"),
            s => {
                // A positional is a revision while we are still in the revision
                // region, otherwise a pathspec. Once a positional is neither a
                // resolvable revision nor an existing path, git dies with the
                // "ambiguous argument" fatal (128) at exactly this point — before
                // any later option-value or operand-count check can fire.
                if in_rev_region {
                    if s.contains("...") && looks_like_range(s) {
                        // `A...B` diffs the merge-base of A and B against B, exactly
                        // like `git diff $(git merge-base A B) B`. Empty sides default
                        // to `HEAD`, mirroring `setup_revisions()`.
                        let (l, r) = s.split_once("...").expect("checked contains");
                        let left = if l.is_empty() { "HEAD" } else { l };
                        let right = if r.is_empty() { "HEAD" } else { r };
                        let lid = repo.rev_parse_single(left)?.object()?.peel_to_commit()?.id;
                        let rid = repo.rev_parse_single(right)?.object()?.peel_to_commit()?.id;
                        let base = repo.merge_base(lid, rid)?.detach();
                        revs.push(base.to_hex().to_string());
                        revs.push(right.to_string());
                        continue;
                    }
                    if s.contains("..") && looks_like_range(s) {
                        let (l, r) = s.split_once("..").expect("checked contains");
                        revs.push(if l.is_empty() { "HEAD".into() } else { l.into() });
                        revs.push(if r.is_empty() { "HEAD".into() } else { r.into() });
                        continue;
                    }
                    if repo.rev_parse_single(s).is_ok() {
                        revs.push(s.to_string());
                        continue;
                    }
                    if std::fs::symlink_metadata(s).is_err() {
                        eprintln!(
                            "fatal: ambiguous argument '{s}': unknown revision or path not in the working tree."
                        );
                        eprintln!("Use '--' to separate paths from revisions, like this:");
                        eprintln!("'git <command> [<revision>...] -- [<file>...]'");
                        return Ok(ExitCode::from(128));
                    }
                    in_rev_region = false;
                }
                paths.push(s.to_string());
            }
        }
    }
    // A value-taking option left at the end of the command line never reaches its
    // callback: parse-options reports it and exits 129 before any revision or
    // pathspec is looked at.
    if let Some(flag) = pending_value {
        eprintln!("error: {}", diff_color::missing_value(&flag));
        return Ok(ExitCode::from(129));
    }
    // `cmd_diff`'s dispatch (builtin/diff.c:611): with no tree-ish pending the leftover
    // reaches `builtin_diff_files()`, which names it; with one it reaches
    // `builtin_diff_index()`, which prints the usage block alone. `--cached` counts as a
    // pending tree-ish because `cmd_diff` supplies HEAD for it.
    if let Some(arg) = &invalid_arg {
        return Ok(invalid_option(arg, !revs.is_empty() || cached));
    }
    paths.extend(trailing_paths);

    // Apply the `diff.algorithm` default only when no `--minimal`/`--histogram`/
    // `--patience`/`--diff-algorithm=` flag set the algorithm on the command line
    // (git's precedence).
    if algorithm.is_none() {
        if let Some(ConfigAlgorithm::Use(a)) = config_algorithm {
            algorithm = Some(a);
        }
    }

    // `diff_setup_done()`: --name-only / --name-status / -s are mutually exclusive
    // and, when present, suppress every other output format.
    if (fmt & (F_NAME | F_NAME_STATUS | F_NO_OUTPUT)).count_ones() > 1 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }
    if fmt & (F_NAME | F_NAME_STATUS | F_NO_OUTPUT) != 0 {
        fmt &= !(F_RAW | F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_PATCH);
    }
    // `--name-only`/`--name-status` suppress `--summary`, but `-s` does not.
    if fmt & (F_NAME | F_NAME_STATUS) != 0 {
        fmt &= !F_SUMMARY;
    }
    if fmt == 0 {
        fmt = F_PATCH;
    }

    // `cmd_diff()` rejects `--cached`/`--staged` with two or more revisions as a
    // usage error (129), printing the full usage stream — this is checked after
    // `setup_revisions()`, so an earlier ambiguous positional (128) wins.
    if cached && revs.len() >= 2 {
        return Ok(usage_error());
    }

    // Three or more revisions request a dense combined ("--cc") diff of the first
    // revision against the rest, exactly like `builtin_diff_combined()`.
    if !cached && revs.len() >= 3 {
        return combined_multi(&repo, &revs, &paths, fmt, ctx, &line_prefix);
    }

    // ---- collect the normalized change list -------------------------------
    let hash_kind = repo.object_hash();
    let mut deltas: Vec<Delta> = Vec::new();
    let mut worktree_mode = false;
    let mut cache;
    // The pre-image tree, kept so `--find-copies-harder` can add the unmodified
    // pairs git's tree walk emits under `DIFF_OPT_FIND_COPIES_HARDER`.
    let mut old_tree_id: Option<ObjectId> = None;

    if cached {
        if revs.len() == 2 {
            bail!("--cached with two revisions is not supported");
        }
        old_tree_id = Some(tree_id_for(&repo, revs.first())?);
        collect_tree_index(&repo, revs.first(), &mut deltas)?;
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else if revs.len() == 2 {
        let old_tree = repo.rev_parse_single(revs[0].as_str())?.object()?.peel_to_tree()?;
        old_tree_id = Some(old_tree.id);
        let new_tree = repo.rev_parse_single(revs[1].as_str())?.object()?.peel_to_tree()?;
        let changes =
            repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(gix::diff::Options::default()))?;
        for change in changes {
            collect_tree_change(change, &mut deltas)?;
        }
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else {
        let workdir = repo
            .workdir()
            .ok_or_else(|| crate::fatal::need_work_tree())?
            .to_owned();
        if revs.len() == 1 {
            old_tree_id = Some(tree_id_for(&repo, revs.first())?);
            collect_tree_worktree(&repo, &revs[0], &paths, &mut deltas)?;
        } else {
            collect_index_worktree(&repo, &workdir, &paths, &mut deltas)?;
        }
        cache = repo.diff_resource_cache(
            Mode::ToGit,
            WorktreeRoots {
                old_root: None,
                new_root: Some(workdir.clone()),
            },
        )?;
        worktree_mode = true;
    }

    // For tree/index sources, apply literal pathspec filtering here (the worktree
    // iterators already filtered via `patterns`).
    if !worktree_mode && !paths.is_empty() {
        let specs = super::log::PathspecMatcher::new(&repo, &paths)?;
        deltas.retain(|d| specs.matches(&d.path));
    }

    // `--relative[=<path>]` narrows the change list to what lives under the
    // prefix. The names are shortened later, once every side has been read:
    // stripping here would leave the worktree reads looking for `one.txt` at the
    // repository root.
    if let Some(prefix) = &relative {
        deltas.retain(|d| d.path.starts_with(prefix.as_bytes()));
    }

    // `-R`: swap the two sides of every pair. The worktree "new" side has no object
    // id to move onto the old side, so a reversed worktree diff genuinely cannot be
    // expressed through this pipeline.
    if reverse {
        if worktree_mode {
            bail!("-R (reverse) with a worktree diff is not supported");
        }
        std::mem::swap(&mut src_prefix, &mut dst_prefix);
        for d in &mut deltas {
            reverse_delta(d);
        }
    }

    // `diff_setup_done()`: `--quiet` turns rename and copy detection off outright.
    if quiet {
        ro.detect_rename = 0;
        ro.find_copies_harder = false;
    }
    // `diff_setup_done()`: `diff.renameLimit` fills in an unset `-l`.
    if ro.detect_rename != 0 && ro.rename_limit < 0 {
        ro.rename_limit = rename_limit_default;
    }
    ro.hash_kind = hash_kind;

    // `--find-copies-harder` asks for copies whose source was *not* itself modified.
    // git supplies those by making the tree walk emit unchanged pairs
    // (`DIFF_OPT_FIND_COPIES_HARDER` in `tree-diff.c`); reproduce that by adding one
    // unmodified pair per pre-image blob the change list does not already cover.
    // `diffcore_rename()`'s write-back drops every surviving unmodified pair, so they
    // can only ever act as copy sources.
    if ro.find_copies_harder && ro.detect_rename == diffcore_rename::DETECT_COPY {
        add_unmodified_pairs(&repo, old_tree_id, &paths, worktree_mode, &mut deltas)?;
    }

    // ---- diffcore_std(): break, rename/copy, merge-broken -----------------
    // Sorting here rather than after the filter puts the queue in the path order
    // git's tree walk produces, which is the order the rename passes iterate in.
    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));
    let mut rename_warnings = diffcore_rename::Warnings::default();
    if ro.detect_rename != 0 || ro.break_opt != -1 {
        rename_warnings = run_diffcore_rename(&repo, &mut cache, &mut deltas, &ro, worktree_mode)?;
    }

    // `--diff-filter`: keep only deltas whose status letter is selected.
    if let Some(filter) = &diff_filter {
        deltas.retain(|d| diff_filter_selected(filter, status_char(d)));
    }

    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    // `-O<file>` / `diff.orderFile` (`diffcore_order`): stably reorder the queue so
    // pairs whose path matches an earlier pattern in the order file come first. git
    // runs it last in `diffcore_std()`, after rename detection and `--diff-filter`.
    if let Some(of) = &order_file {
        let order = diff_files::read_order_file(of);
        deltas.sort_by_cached_key(|d| diff_files::match_order(&order, d.path.as_slice()));
    }

    // ---- analyze every delta once -----------------------------------------
    // `--quiet`/`-s` produce no output, so the patch bodies are never needed.
    let workdir = repo.workdir().map(|p| p.to_owned());
    // `diff_setup_done()` (diff.c:4899): the whitespace-ignoring options make "is
    // there a change?" a question only the rendered content can answer.
    let from_contents = ws != Whitespace::Keep;
    // `diff_flush()` (diff.c:6828): `--quiet`/`-s` produce no output, but with
    // `diff_from_contents` and `--exit-code` git still runs the patch machinery with
    // its output redirected to `/dev/null` purely to learn the exit status.
    let exit_needs_patch = quiet && want_exit_code && from_contents;
    // `diff_flush()` (diff.c:7210): under `diff_from_contents` the raw/name formats
    // run every pair through `diff_flush_patch_quietly()` first and skip the ones
    // that report no change, so they too need the patch machinery.
    let names_need_patch = from_contents && !quiet && fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0;
    let want_patch =
        (fmt & F_PATCH != 0 || check || exit_needs_patch || names_need_patch) && (!quiet || exit_needs_patch);
    // Analysis reads both sides of every changed file, so it runs only for the
    // formats that consume it: the counts (`--numstat`/`--stat`/`--shortstat`)
    // and the patch body. `--name-only`, `--name-status`, `--raw` and `--summary`
    // render from the change list alone, and paying for blob reads there is pure
    // waste — the same reason `--quiet` skips it.
    // `--check` walks the same hunks the patch body is built from, so it needs
    // the analysis even though it prints no patch.
    // `--dirstat` needs the analysis too: its `lines` mode reads the same counts
    // the stat formats do, and its default mode needs each pair's content damage.
    let want_dirstat = fmt & F_DIRSTAT != 0;
    let want_analysis = check
        || want_dirstat
        || exit_needs_patch
        || names_need_patch
        || fmt & (F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_PATCH) != 0;
    let mut analyses: Vec<Analysis> = Vec::new();
    if (!quiet || exit_needs_patch) && want_analysis {
        analyses = analyze_all(
            &repo,
            &mut cache,
            &deltas,
            ctx,
            ws,
            indent_heuristic,
            hash_kind,
            workdir.as_deref(),
            want_patch,
            algorithm,
            worktree_mode,
            want_dirstat && !dirstat.by_line && !dirstat.by_file,
            // Only a rendered patch carries the payload, so a `--stat`-only run with
            // `--binary` reads nothing extra.
            binary && want_patch,
            func_context,
        )?;
    }

    // With no explicit `--abbrev`, the `index` line honors `core.abbrev`
    // (git's DEFAULT_ABBREV / auto), not a hardcoded 7. `--full-index` still
    // wins at render time regardless of this length.
    if !abbrev_explicit {
        abbrev = crate::abbrev::configured_abbrev(&repo, repo.object_hash().len_in_hex());
    }

    let r = Render {
        raw_abbrev: raw_abbrev.unwrap_or(abbrev),
        abbrev,
        full_index,
        binary,
        z,
        src_prefix,
        dst_prefix,
        hash_kind,
    };

    // `--color[=<when>]` and `--no-color`, falling back to `color.diff` /
    // `diff.color` / `color.ui` and the terminal test.
    let colors = diff_color::DiffColors::resolve(&repo, diff_color::resolve_color(&repo, color_when));
    let ws_rule = diff_color::whitespace_rule_cfg(&repo);
    let extra = match move_word.resolve(&repo) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(128));
        }
    };

    // `--relative`: git reports each path through `relative_path()` at output
    // time, so the shortening happens here — after every blob has been read by
    // its real path, and in one place rather than at each of the format writers.
    if let Some(prefix) = &relative {
        for d in &mut deltas {
            d.path = d.path[prefix.len()..].into();
            if let Some(src) = &d.src_path {
                if src.starts_with(prefix.as_bytes()) {
                    d.src_path = Some(src[prefix.len()..].into());
                }
            }
        }
    }

    // `--check` replaces every other format: `diff_flush()` routes the queue
    // through `diff_flush_checkdiff()`, which prints one line per added line
    // that breaks a whitespace rule and nothing else. Its exit status is 2, the
    // one `git diff --check` gives a caller that greps for problems.
    if check {
        let mut found = false;
        for (delta, analysis) in deltas.iter().zip(analyses.iter()) {
            found |= report_whitespace(delta, analysis, ws_rule);
        }
        return Ok(if found {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        });
    }

    // ---- render, in `diff_flush()` order ----------------------------------
    // `diff_flush()` bails out before printing anything at all when the change
    // queue is empty, so even `--shortstat` stays silent on a clean tree.
    let mut out: Vec<u8> = Vec::new();
    let mut separator = false;
    // `o->found_changes`: what the whitespace-ignoring options make the exit status
    // depend on. Only a format that actually emitted something sets it — see the
    // `diff_from_contents` rewrite of `has_changes` at the end of `diff_flush()`
    // (diff.c:6861).
    let mut found_changes = false;
    if !quiet && !deltas.is_empty() {
        if fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0 {
            let mut scratch: Vec<u8> = Vec::new();
            for (i, delta) in deltas.iter().enumerate() {
                // `diff_flush()` (diff.c:7210): with `diff_from_contents` the pair is
                // rendered quietly first and dropped when it reports nothing, so a
                // whitespace-only change is absent from these formats as well.
                if names_need_patch {
                    let an = &analyses[i];
                    if !pair_reports_change(&mut scratch, &repo, delta, an, ctx, &r, submodule_format)? {
                        continue;
                    }
                }
                if fmt & (F_RAW | F_NAME_STATUS) != 0 {
                    render_raw(&mut out, delta, fmt, &r);
                } else {
                    out.extend_from_slice(&name_field(&delta.path, r.z));
                    out.push(if r.z { 0 } else { b'\n' });
                }
                // `flush_one_pair()` (diff.c:6323) sets `found_changes` for every
                // pair it prints, whatever the whitespace options did.
                found_changes = true;
            }
            separator = true;
        }

        if fmt & (F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT) != 0 {
            let stat_pairs = diffstat_pairs(&deltas, &analyses);
            // `compute_diffstat()` (diff.c:7168) *assigns* `found_changes` from the
            // number of surviving entries, so a stat format that dropped every pair
            // clears a `found_changes` an earlier raw/name format had set.
            found_changes = !stat_pairs.is_empty();
            if fmt & F_NUMSTAT != 0 {
                render_numstat(&mut out, &stat_pairs, z);
            }
            if fmt & F_DIFFSTAT != 0 {
                render_stat(
                    &mut out,
                    &stat_pairs,
                    compact_summary,
                    stat_name_width,
                    stat_graph_width,
                    &colors,
                );
            }
            if fmt & F_SHORTSTAT != 0 {
                render_shortstat(&mut out, &stat_pairs);
            }
            separator = true;
        }

        // `diff_flush()`: dirstat sits between the stat formats and the summary.
        // Its `lines` mode reuses the diffstat counts (a binary pair is charged one
        // unit per 64 bytes), `--dirstat-by-file` charges one unit per changed file,
        // and the default mode uses the content damage computed with the pair.
        if fmt & F_DIRSTAT != 0 {
            let files: Vec<(BString, u64)> = deltas
                .iter()
                .zip(analyses.iter())
                .map(|(d, an)| {
                    let damage = if dirstat.by_file {
                        1
                    } else if dirstat.by_line {
                        // For a binary pair `added`/`deleted` are the two sizes,
                        // which `show_dirstat_by_line()` charges in 64-byte units.
                        let lines = u64::from(an.added) + u64::from(an.deleted);
                        if an.binary { lines.div_ceil(64) } else { lines }
                    } else if an.damage == 0 {
                        // `show_dirstat()` charges a pair that changed at all a
                        // single unit, so a mode-only change still shows up.
                        1
                    } else {
                        an.damage
                    };
                    (d.path.clone(), damage)
                })
                .collect();
            super::diff_files::render_dirstat(&mut out, files, &dirstat);
            separator = true;
        }

        if fmt & F_SUMMARY != 0 {
            render_summary(&mut out, &deltas);
            separator = true;
        }

        if fmt & F_PATCH != 0 {
            if separator {
                out.push(b'\n');
            }
            // `run_diff_files()` queues an unmerged path twice — once as the `U`
            // pair and once as the ordinary stage-2-vs-worktree modification — and
            // the raw/name/stat formats above print both. The patch format prints
            // only the combined (`--cc`) patch for such a path; the duplicate pair
            // contributes no `diff --git` section of its own.
            let unmerged: BTreeSet<&BString> =
                deltas.iter().filter(|d| d.unmerged).map(|d| &d.path).collect();
            // The whole patch is assembled uncolored and then re-emitted in one
            // pass through git's `fn_out_consume()` chain, carrying each pair's own
            // whitespace state so the blank-at-EOF check never leaks from one file
            // to the next. Emitting only after the last pair is what
            // `diff_flush_patch_all_file_pairs()` does, and it is what lets
            // `--color-moved` recognize a block that moved between two files.
            let paint_opts = diff_color::PaintOptions {
                ws_error_highlight,
                suppress_blank_empty,
                ..Default::default()
            };
            let mut plain: Vec<u8> = Vec::new();
            let mut files: Vec<diff_color::FilePaint> = Vec::new();
            // `--submodule=log`/`=diff` write their lines through
            // `diff_emit_submodule_*()`, which paints each one itself instead of
            // handing it to `fn_out_consume()`. Draining the assembled patch at
            // every such pair keeps both the order and those colours intact;
            // `--color-moved` is the only thing a split buffer would cost, and this
            // command rejects it outright.
            let sub_abbrev = crate::abbrev::configured_abbrev(&repo, repo.object_hash().len_in_hex());
            for (delta, an) in deltas.iter().zip(&analyses) {
                if !delta.unmerged && unmerged.contains(&delta.path) {
                    continue;
                }
                if submodule_format != SubmoduleFormat::Short
                    && !delta.unmerged
                    && delta.is_submodule_pair()
                {
                    out.extend_from_slice(&diff_color::colorize_patch_ex(
                        &plain,
                        &colors,
                        &paint_opts,
                        &files,
                        diff_color::FilePaint::new(ws_rule),
                        &extra,
                    ));
                    plain.clear();
                    files.clear();
                    render_submodule(
                        &mut out,
                        &repo,
                        delta,
                        submodule_format,
                        sub_abbrev,
                        &colors,
                        &r,
                    );
                    // `builtin_diff()`'s submodule branches set `o->found_changes`
                    // unconditionally (diff.c:3570, diff.c:3579).
                    found_changes = true;
                    continue;
                }
                let before = plain.len();
                render_patch(&mut plain, &repo, delta, an, ctx, &r)?;
                if plain.len() != before {
                    files.push(diff_color::FilePaint { ws_rule, blank_at_eof: an.blank_at_eof });
                    // Every `builtin_diff()` arm that emits a header or a hunk sets
                    // `o->found_changes`, so having written anything is the answer.
                    found_changes = true;
                }
            }
            out.extend_from_slice(&diff_color::colorize_patch_ex(
                &plain,
                &colors,
                &paint_opts,
                &files,
                diff_color::FilePaint::new(ws_rule),
                &extra,
            ));
        }
    }

    // `diff_flush()` (diff.c:6828): the no-output formats still render every pair
    // into `/dev/null` under `diff_from_contents`, stopping at the first pair that
    // reports a change, because that is the only way to know the exit status.
    if exit_needs_patch {
        let mut sink: Vec<u8> = Vec::new();
        for (delta, an) in deltas.iter().zip(&analyses) {
            if pair_reports_change(&mut sink, &repo, delta, an, ctx, &r, submodule_format)? {
                found_changes = true;
                break;
            }
        }
    }

    // `diff.suppressBlankEmpty`: `fn_out_consume()` rewrites any emitted line that
    // is exactly `" \n"` (an empty context line) to `"\n"` before it is prefixed.
    let out = apply_suppress_blank_empty(out, suppress_blank_empty);

    // `--line-prefix`: `diff_line_prefix()` prepends the string to every emitted
    // line, so a whole-buffer pass over the newline-terminated output reproduces it.
    let out = apply_line_prefix(out, &line_prefix);

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    // `diff_result_code()` calls `diff_warn_rename_limit()` after stdout is flushed,
    // so the `-l` / `diff.renameLimit` warnings land after the diff itself.
    rename_warnings.emit("diff.renameLimit");
    // `--exit-code`/`--quiet`: exit 1 when any difference was reported.
    //
    // `diff_change()` sets `has_changes` as each pair is queued, so normally a
    // non-empty queue is the whole answer. A whitespace-ignoring option turns on
    // `diff_from_contents` (diff.c:4899) and that queue-time shortcut is skipped:
    // `diff_flush()` re-derives `has_changes` from `found_changes` instead
    // (diff.c:6861), which only the formats that emitted something ever set.
    if want_exit_code {
        let changed = if from_contents { found_changes } else { !deltas.is_empty() };
        if changed {
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Reverse (`-R`) one object-backed pair: the new side becomes the old side and
/// vice-versa. Worktree pairs are never reversed (rejected earlier).
fn reverse_delta(d: &mut Delta) {
    let new_as_old = match &d.new {
        NewSide::Blob(id, k) => Some((*id, *k)),
        NewSide::Absent => None,
        NewSide::Worktree(_) => return,
    };
    let old_as_new = match d.old {
        Some((id, k)) => NewSide::Blob(id, k),
        None => NewSide::Absent,
    };
    d.old = new_as_old;
    d.new = old_as_new;
    if let Some((a, b)) = d.stages {
        d.stages = Some((b, a));
    }
}

/// `--find-copies-harder`: append an unmodified pair for every pre-image blob that
/// the change list does not already mention, so copy detection can use it as a
/// source. The pre-image is the old tree when there is one, otherwise the index (the
/// pre-image of a plain `git diff`).
fn add_unmodified_pairs(
    repo: &gix::Repository,
    old_tree_id: Option<ObjectId>,
    paths: &[String],
    worktree_mode: bool,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let seen: BTreeSet<BString> = deltas
        .iter()
        .filter(|d| d.old.is_some())
        .map(|d| d.path.clone())
        .collect();
    // The same pathspec filtering the change list went through. Worktree sources
    // were already limited by the dirwalk, which matches pathspecs itself.
    let mut specs = if worktree_mode || paths.is_empty() {
        None
    } else {
        Some(super::log::PathspecMatcher::new(repo, paths)?)
    };
    let mut add = |path: BString, id: ObjectId, kind: EntryKind| {
        if seen.contains(&path) {
            return;
        }
        if let Some(specs) = specs.as_mut() {
            if !specs.matches(&path) {
                return;
            }
        }
        deltas.push(Delta::plain(path, Some((id, kind)), NewSide::Blob(id, kind)));
    };

    match old_tree_id {
        Some(tree_id) => {
            let tree = repo.find_object(tree_id)?.peel_to_tree()?;
            for entry in tree.iter().filter_map(std::result::Result::ok) {
                walk_tree_blobs(repo, &entry.inner, BString::default(), &mut add)?;
            }
        }
        None => {
            let index = repo.index_or_empty()?;
            for entry in index.entries() {
                if entry.stage() != gix::index::entry::Stage::Unconflicted {
                    continue;
                }
                let Some(kind) = index_mode_kind(entry.mode) else {
                    continue;
                };
                add(entry.path(&index).into(), entry.id, kind);
            }
        }
    }
    Ok(())
}

/// Recursive helper for [`add_unmodified_pairs`]: yield every non-tree entry of
/// `entry` (itself included when it is not a tree) with its full path.
fn walk_tree_blobs(
    repo: &gix::Repository,
    entry: &gix::objs::tree::EntryRef<'_>,
    prefix: BString,
    add: &mut impl FnMut(BString, ObjectId, EntryKind),
) -> Result<()> {
    let mut path = prefix;
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(entry.filename);
    if entry.mode.is_tree() {
        let sub = repo.find_object(entry.oid)?.peel_to_tree()?;
        for child in sub.iter().filter_map(std::result::Result::ok) {
            walk_tree_blobs(repo, &child.inner, path.clone(), add)?;
        }
    } else if let Some(kind) = mode_kind(u32::from(entry.mode.value())) {
        add(path, entry.oid.to_owned(), kind);
    }
    Ok(())
}

/// Reads a filespec's content for [`diffcore_rename`], mirroring
/// `diff_populate_filespec()`.
///
/// An object-backed side is read straight from the odb. A worktree side (identified
/// by carrying no object id) goes through the same blob platform the patch pipeline
/// uses, so any checkout filter is applied exactly once and identically; when that
/// platform declines to expose the bytes (its binary/oversized path) the raw file is
/// read instead, which is what git's own populate does for such a blob.
struct DeltaContent<'a> {
    repo: &'a gix::Repository,
    cache: &'a mut gix::diff::blob::Platform,
    workdir: Option<std::path::PathBuf>,
}

impl DeltaContent<'_> {
    fn read(&mut self, spec: &diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        if spec.oid_valid && !spec.oid.is_null() {
            return self.repo.find_object(spec.oid).ok().map(|o| o.detach().data);
        }
        let kind = mode_kind(spec.mode)?;
        let path = spec.path.as_bstr();
        let null = self.repo.object_hash().null();
        self.cache
            .set_resource(null, kind, path, ResourceKind::NewOrDestination, &self.repo.objects)
            .ok()?;
        if let Some(res) = self.cache.resource(ResourceKind::NewOrDestination) {
            if let Some(buf) = res.data.as_slice() {
                return Some(buf.to_vec());
            }
        }
        let base = self.workdir.as_deref()?;
        let full = base.join(gix::path::from_bstr(path));
        if kind == EntryKind::Link {
            return std::fs::read_link(&full)
                .ok()
                .map(|t| gix::path::into_bstr(t).into_owned().into());
        }
        std::fs::read(&full).ok()
    }
}

impl diffcore_rename::Content for DeltaContent<'_> {
    fn size(&mut self, spec: &diffcore_rename::FileSpec) -> Option<u64> {
        // `check_size_only = 1` exists to skip inflating the blob; the odb header is
        // the cheap answer for an object-backed side. A worktree side has to be read.
        if spec.oid_valid && !spec.oid.is_null() {
            if let Ok(header) = self.repo.find_header(spec.oid) {
                if header.kind() == gix::object::Kind::Blob {
                    return Some(header.size());
                }
            }
        }
        self.read(spec).map(|d| d.len() as u64)
    }

    fn data(&mut self, spec: &diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        self.read(spec)
    }
}

/// The `diffcore_std()` rename/copy/break slice, bridged onto this module's [`Delta`]
/// list: build git's filespec/filepair queue, run the passes, resolve every status
/// letter, then rebuild the delta list from the surviving pairs.
///
/// Unmerged deltas never enter the queue. git represents a conflicted path as a pair
/// with *both* sides invalid, which the rename passes carry through untouched; this
/// port models it as a delta with its own `stages`, so it is simply held aside and
/// re-appended (the caller re-sorts by path afterwards, restoring git's order).
fn run_diffcore_rename(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    deltas: &mut Vec<Delta>,
    opts: &diffcore_rename::Options,
    worktree_mode: bool,
) -> Result<diffcore_rename::Warnings> {
    let mut held: Vec<Delta> = Vec::new();
    let mut q = diffcore_rename::Queue::default();
    // spec index -> the "new" side it came from, so a surviving pair can be turned
    // back into a delta that still knows whether to read the worktree. A gitlink
    // pair also has to keep the submodule state it arrived with, which no filespec
    // has room for.
    let mut new_sides: BTreeMap<usize, NewSide> = BTreeMap::new();
    let mut submodule_state: BTreeMap<usize, (u8, Option<ObjectId>, Option<ObjectId>)> =
        BTreeMap::new();

    for d in deltas.drain(..) {
        if d.unmerged {
            held.push(d);
            continue;
        }
        let one = match d.old {
            Some((id, k)) => q.add_spec(diffcore_rename::FileSpec::new(
                d.path.clone(),
                kind_mode(k),
                id,
                true,
            )),
            None => q.add_spec(diffcore_rename::FileSpec::absent(d.path.clone())),
        };
        let two = match &d.new {
            NewSide::Absent => q.add_spec(diffcore_rename::FileSpec::absent(d.path.clone())),
            NewSide::Blob(id, k) => q.add_spec(diffcore_rename::FileSpec::new(
                d.path.clone(),
                kind_mode(*k),
                *id,
                true,
            )),
            NewSide::Worktree(k) => q.add_spec(diffcore_rename::FileSpec::new(
                d.path.clone(),
                kind_mode(*k),
                repo.object_hash().null(),
                false,
            )),
        };
        new_sides.insert(two, clone_new_side(&d.new));
        if d.dirty_submodule != 0 || d.new_commit.is_some() {
            submodule_state.insert(two, (d.dirty_submodule, d.new_commit, d.new_id));
        }
        q.add_pair(one, two);
    }

    let workdir = if worktree_mode {
        repo.workdir().map(|p| p.to_owned())
    } else {
        None
    };
    let mut content = DeltaContent { repo, cache, workdir };
    let warnings = diffcore_rename::run(&mut q, opts, &mut content);
    diffcore_rename::resolve_rename_copy(&mut q);

    for p in &q.pairs {
        let one = &q.specs[p.one];
        let two = &q.specs[p.two];
        let path = if two.valid() { two.path.clone() } else { one.path.clone() };
        let src_path = if one.valid() && two.valid() && one.path != two.path {
            Some(one.path.clone())
        } else {
            None
        };
        let old = if one.valid() {
            mode_kind(one.mode).map(|k| (one.oid, k))
        } else {
            None
        };
        let new = if two.valid() {
            new_sides
                .get(&p.two)
                .map(clone_new_side)
                .unwrap_or(NewSide::Absent)
        } else {
            NewSide::Absent
        };
        let sub_state = two.valid().then(|| submodule_state.get(&p.two).copied()).flatten();
        deltas.push(Delta {
            path,
            old,
            new,
            unmerged: false,
            stages: None,
            src_path,
            score: p.score,
            status: p.status,
            // `hash_filespec()` may have given a worktree post-image a real id; a
            // gitlink never goes through it, so its own id survives the queue.
            new_id: match sub_state {
                Some((_, _, id)) => id,
                None => (two.valid() && two.oid_valid).then_some(two.oid),
            },
            dirty_submodule: sub_state.map(|(d, _, _)| d).unwrap_or(0),
            new_commit: sub_state.and_then(|(_, c, _)| c),
        });
    }
    deltas.extend(held);
    Ok(warnings)
}

/// `NewSide` is not `Clone` (it is normally moved once), so copy it explicitly.
fn clone_new_side(n: &NewSide) -> NewSide {
    match n {
        NewSide::Absent => NewSide::Absent,
        NewSide::Blob(id, k) => NewSide::Blob(*id, *k),
        NewSide::Worktree(k) => NewSide::Worktree(*k),
    }
}

/// An [`EntryKind`] as the numeric mode git's filespecs carry.
fn kind_mode(k: EntryKind) -> u32 {
    u32::from_str_radix(mode_str(k), 8).unwrap_or(0o100644)
}

/// The inverse of [`kind_mode`]: a numeric mode back into an [`EntryKind`].
fn mode_kind(mode: u32) -> Option<EntryKind> {
    match mode & 0o170000 {
        0o100000 => Some(if mode & 0o111 != 0 {
            EntryKind::BlobExecutable
        } else {
            EntryKind::Blob
        }),
        0o120000 => Some(EntryKind::Link),
        0o160000 => Some(EntryKind::Commit),
        0o040000 => Some(EntryKind::Tree),
        _ => None,
    }
}

/// `--diff-filter`: an uppercase letter selects a status, its lowercase excludes it.
/// When only exclusions are given every other status is kept; when any inclusion is
/// present, unlisted statuses are dropped — matching `diff_opt_diff_filter()`.
fn diff_filter_selected(filter: &[u8], status: u8) -> bool {
    let up = status.to_ascii_uppercase();
    if filter.iter().any(|&f| f == up.to_ascii_lowercase()) {
        return false;
    }
    let has_include = filter.iter().any(|f| f.is_ascii_uppercase());
    if has_include {
        filter.contains(&up)
    } else {
        true
    }
}

// ---------------------------------------------------------------------------
// change collection
// ---------------------------------------------------------------------------

/// `<tree>` vs. the index (`--cached`). gitoxide's index diff skips unmerged
/// entries, so those are re-added here the way `do_oneway_diff()` does: a single
/// `U` pair whose old side comes from the tree.
fn collect_tree_index(
    repo: &gix::Repository,
    spec: Option<&String>,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let tree_id = tree_id_for(repo, spec)?;
    let index = repo.index_or_load_from_head()?;
    repo.tree_index_status(
        &tree_id,
        &index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            collect_index_change(change, deltas);
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;

    let tree = repo.find_object(tree_id)?.peel_to_tree()?;
    for path in unmerged_paths(&index) {
        let old = tree_entry(&tree, &path)?;
        deltas.push(Delta {
            path,
            old,
            new: NewSide::Absent,
            unmerged: true,
            stages: None,
            src_path: None,
            score: 0,
            status: 0,
            new_id: None,
            dirty_submodule: 0,
            new_commit: None,
        });
    }
    Ok(())
}

/// `<tree>` vs. the worktree. Reproduces `diff-index`: start from the tree-to-index
/// difference, then let the index-to-worktree difference override the "new" side.
fn collect_tree_worktree(
    repo: &gix::Repository,
    spec: &str,
    paths: &[String],
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let tree_id = repo.rev_parse_single(spec)?.object()?.peel_to_tree()?.id;
    let patterns: Vec<BString> = paths.iter().map(|p| BString::from(p.as_str())).collect();

    // Path -> new side and its `dirty_submodule` bits, in index order (the order
    // `diff-index` reports in).
    let mut new_sides: BTreeMap<BString, (NewSide, u8, Option<ObjectId>)> = BTreeMap::new();

    let iter = repo
        .status(gix::progress::Discard)?
        .head_tree(tree_id)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
        .index_worktree_submodules(submodule_status())
        .index_worktree_options_mut(|o| {
            o.dirwalk_options = None; // exclude untracked files, matching `git diff`
            o.rewrites = None; // no rename detection
        })
        .into_iter(patterns)?;

    for item in iter {
        match item? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                let deleted = matches!(change, ChangeRef::Deletion { .. });
                let (loc, _, entry_mode, oid) = change.fields();
                let (location, id) = (loc.to_owned(), oid.to_owned());
                match if deleted { None } else { index_mode_kind(entry_mode) } {
                    // A gitlink the index already moved: the worktree pass below
                    // overrides this with the submodule's checked-out `HEAD` when
                    // the two disagree.
                    Some(k) => {
                        new_sides.insert(location, (NewSide::Blob(id, k), 0, None));
                    }
                    None => {
                        new_sides.insert(location, (NewSide::Absent, 0, None));
                    }
                }
            }
            gix::status::Item::IndexWorktree(item) => {
                if let Some((path, new, dirty, head)) = worktree_new_side(item)? {
                    new_sides.insert(path, (new, dirty, head));
                }
            }
        }
    }

    let tree = repo.find_object(tree_id)?.peel_to_tree()?;
    for (path, (new, dirty, head)) in new_sides {
        let old = tree_entry(&tree, &path)?;
        // A path that neither existed in the tree nor exists now is not a change.
        if old.is_none() && matches!(new, NewSide::Absent) {
            continue;
        }
        // Unchanged content that only travelled through the index is not a change —
        // unless it is a submodule whose worktree is dirty, which `diff_unmodified_pair()`
        // (diff.c:6528) keeps precisely because of that bit.
        if let (Some((oid, ok)), NewSide::Blob(nid, nk)) = (&old, &new) {
            if oid == nid && ok == nk && dirty == 0 {
                continue;
            }
        }
        // The same test for a gitlink whose post-image is the submodule's worktree:
        // the tree already names the commit it has checked out.
        if let (Some((oid, EntryKind::Commit)), Some(h)) = (&old, head) {
            if *oid == h && dirty == 0 {
                continue;
            }
        }
        let mut delta = Delta::plain(path, old, new);
        delta.dirty_submodule = dirty;
        delta.new_commit = head;
        // As in `collect_index_worktree()`: only a gitlink that did not move keeps
        // a printable post-image id.
        delta.new_id = head.filter(|h| old.map(|(id, _)| id) == Some(*h));
        deltas.push(delta);
    }
    Ok(())
}

/// git's `ignore_untracked_in_submodules` default (`diff_setup_done()`, diff.c:5169):
/// a diff never counts untracked files inside a submodule as damage, so the status
/// walk is told the same thing rather than paying for a dirwalk whose result would
/// be dropped.
fn submodule_status() -> gix::status::Submodule {
    gix::status::Submodule::Given {
        ignore: gix::submodule::config::Ignore::Untracked,
        check_dirty: false,
    }
}

/// The index vs. the worktree (plain `git diff`).
fn collect_index_worktree(
    repo: &gix::Repository,
    workdir: &std::path::Path,
    paths: &[String],
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    let index = repo.index_or_empty()?;
    let conflicts = conflict_stages(&index);
    let patterns: Vec<BString> = paths.iter().map(|p| BString::from(p.as_str())).collect();
    let iter = repo
        .status(gix::progress::Discard)?
        .index_worktree_submodules(submodule_status())
        .index_worktree_options_mut(|o| {
            o.dirwalk_options = None; // exclude untracked files, matching `git diff`
            o.rewrites = None; // no rename detection
        })
        .into_index_worktree_iter(patterns)?;

    let mut seen_conflicts: Vec<BString> = Vec::new();
    for item in iter {
        let item = item?;
        if let gix::status::index_worktree::Item::Modification {
            rela_path, status, ..
        } = &item
        {
            if matches!(
                status,
                gix::status::plumbing::index_as_worktree::EntryStatus::Conflict { .. }
            ) {
                if !seen_conflicts.contains(rela_path) {
                    seen_conflicts.push(rela_path.clone());
                }
                continue;
            }
        }
        if let Some((path, new, dirty, head)) = worktree_new_side(item)? {
            // A worktree entry with no index counterpart cannot happen here (the
            // dirwalk is off), so the old side is always the index entry.
            let entry = index
                .entry_by_path(path.as_bstr())
                .ok_or_else(|| anyhow::anyhow!("no index entry for {path:?}"))?;
            let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
            let mut delta = Delta::plain(path, Some((entry.id, old_kind)), new);
            delta.dirty_submodule = dirty;
            delta.new_commit = head;
            // `run_diff_files()` leaves a moved gitlink's post-image invalid, so
            // `--raw` prints all-zero for it; a submodule that is only dirty keeps
            // the id, since the gitlink itself did not move.
            delta.new_id = head.filter(|h| *h == entry.id);
            deltas.push(delta);
        }
    }

    // `run_diff_files()` reports an unmerged path twice: once as the `U` pair, and
    // once as the ordinary stage-2-vs-worktree modification.
    for path in seen_conflicts {
        let stages = conflicts.get(&path);
        let wt_kind = worktree_kind(workdir, &path);
        deltas.push(Delta {
            path: path.clone(),
            old: None,
            new: match wt_kind {
                Some(k) => NewSide::Worktree(k),
                None => NewSide::Absent,
            },
            unmerged: true,
            stages: stages.map(|s| (s.ours.0, s.theirs.0)),
            src_path: None,
            score: 0,
            status: 0,
            new_id: None,
            dirty_submodule: 0,
            new_commit: None,
        });
        if let (Some(s), Some(k)) = (stages, wt_kind) {
            deltas.push(Delta::plain(path, Some((s.ours.0, s.ours.1)), NewSide::Worktree(k)));
        }
    }
    Ok(())
}

/// The "new" side an index-vs-worktree status item implies, together with the
/// `DIRTY_SUBMODULE_*` bits and the checked-out submodule commit it carries, or
/// `None` when the item is not a change.
fn worktree_new_side(
    item: gix::status::index_worktree::Item,
) -> Result<Option<(BString, NewSide, u8, Option<ObjectId>)>> {
    use gix::status::index_worktree::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let Item::Modification {
        entry,
        rela_path,
        status,
        ..
    } = item
    else {
        // Untracked/ignored entries never appear in `git diff` (the dirwalk is off),
        // and rename tracking is disabled.
        return Ok(None);
    };
    let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
    if matches!(old_kind, EntryKind::Commit) {
        // A gitlink's post-image is the commit its worktree currently has checked
        // out — `run_diff_files()` fills the filespec with that id and leaves it
        // marked invalid, which is why `--raw` still prints all-zero for it once it
        // has moved. Local damage inside the submodule rides along as
        // `two->dirty_submodule`.
        return Ok(match status {
            EntryStatus::Change(Change::SubmoduleModification(sm)) => {
                let head = sm.checked_out_head_id.unwrap_or(entry.id);
                let dirty = if sm.changes.as_ref().is_some_and(|c| !c.is_empty()) {
                    super::diff_pairs::DIRTY_SUBMODULE_MODIFIED
                } else {
                    0
                };
                Some((rela_path, NewSide::Worktree(EntryKind::Commit), dirty, Some(head)))
            }
            // The whole submodule directory is gone from the worktree.
            EntryStatus::Change(Change::Removed) => Some((rela_path, NewSide::Absent, 0, None)),
            EntryStatus::Change(Change::Type { .. }) => {
                bail!("type change at {rela_path:?} is not supported")
            }
            _ => None,
        });
    }
    Ok(match status {
        EntryStatus::Change(Change::Modification {
            executable_bit_changed,
            ..
        }) => {
            let new_kind = if executable_bit_changed {
                toggle_exec(old_kind)
            } else {
                old_kind
            };
            Some((rela_path, NewSide::Worktree(new_kind), 0, None))
        }
        EntryStatus::Change(Change::Removed) => Some((rela_path, NewSide::Absent, 0, None)),
        EntryStatus::Change(Change::Type { .. }) => {
            bail!("type change at {rela_path:?} is not supported")
        }
        // A conflicted path still has worktree content; only `git diff` with no
        // revision treats it specially, and that caller intercepts it first.
        EntryStatus::Conflict { .. } => Some((rela_path, NewSide::Worktree(old_kind), 0, None)),
        // Submodule content modification, intent-to-add, and stat-only refreshes
        // produce no textual diff.
        EntryStatus::Change(Change::SubmoduleModification(_))
        | EntryStatus::IntentToAdd
        | EntryStatus::NeedsUpdate(_) => None,
    })
}

/// The stage 2 ("ours") and stage 3 ("theirs") blobs of a conflicted path.
struct Stages {
    ours: (ObjectId, EntryKind),
    theirs: (ObjectId, EntryKind),
}

fn conflict_stages(index: &gix::index::State) -> BTreeMap<BString, Stages> {
    let mut per_path: BTreeMap<BString, [Option<(ObjectId, EntryKind)>; 2]> = BTreeMap::new();
    for entry in index.entries() {
        let slot = match entry.stage() {
            gix::index::entry::Stage::Ours => 0,
            gix::index::entry::Stage::Theirs => 1,
            _ => continue,
        };
        let kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
        per_path
            .entry(entry.path(index).to_owned())
            .or_default()[slot] = Some((entry.id, kind));
    }
    per_path
        .into_iter()
        .filter_map(|(path, [ours, theirs])| {
            Some((
                path,
                Stages {
                    ours: ours?,
                    theirs: theirs?,
                },
            ))
        })
        .collect()
}

/// Every path with at least one non-zero stage, in index order.
fn unmerged_paths(index: &gix::index::State) -> Vec<BString> {
    let mut out: Vec<BString> = Vec::new();
    for entry in index.entries() {
        if entry.stage() == gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let path = entry.path(index).to_owned();
        if out.last() != Some(&path) {
            out.push(path);
        }
    }
    out
}

fn worktree_kind(workdir: &std::path::Path, path: &BString) -> Option<EntryKind> {
    let full = workdir.join(gix::path::from_bstr(path.as_bstr()));
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if meta.is_symlink() {
        return Some(EntryKind::Link);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return Some(EntryKind::BlobExecutable);
        }
    }
    Some(EntryKind::Blob)
}

fn tree_entry(tree: &gix::Tree<'_>, path: &BString) -> Result<Option<(ObjectId, EntryKind)>> {
    let components: Vec<&[u8]> = path.as_slice().split(|b| *b == b'/').collect();
    let entry = tree.lookup_entry(components)?;
    Ok(entry.map(|e| (e.object_id(), e.mode().kind())))
}

/// A single revision spec into a tree id, defaulting to `HEAD^{tree}` (or the empty
/// tree if `HEAD` is unborn) when no spec is given.
fn tree_id_for(repo: &gix::Repository, spec: Option<&String>) -> Result<ObjectId> {
    Ok(match spec {
        Some(s) => repo.rev_parse_single(s.as_str())?.object()?.peel_to_tree()?.id,
        None => repo.head_tree_id_or_empty()?.detach(),
    })
}

/// `true` if a token looks like a revision range rather than a filename that merely
/// contains `..` (e.g. `../foo`). Ranges don't contain `/` and don't start with `.`.
fn looks_like_range(tok: &str) -> bool {
    !tok.starts_with('.') && !tok.contains('/')
}

/// Every option name stock `git diff` resolves — the union of what `setup_revisions()`
/// and `diff_opt_parse()` consume plus the handful `cmd_diff` dispatches on. A name
/// missing from this table is one git itself rejects, which is the only case that may
/// take the `invalid option` path; anything present here that this port has not
/// implemented keeps saying so, rather than blaming git for the gap.
///
/// Established by running every candidate name through stock git 2.55.0 and keeping
/// those it does not answer with `error: invalid option:`.
const KNOWN_LONG: &[&str] = &[
    "--abbrev",
    "--abbrev-commit",
    "--all",
    "--anchored",
    "--base",
    "--binary",
    "--bisect",
    "--branches",
    "--break-rewrites",
    "--cached",
    "--cc",
    "--check",
    "--children",
    "--color",
    "--color-moved",
    "--color-moved-ws",
    "--color-words",
    "--compact-summary",
    "--count",
    "--cumulative",
    "--date-order",
    "--default-prefix",
    "--diff-algorithm",
    "--diff-filter",
    "--diff-merges",
    "--dirstat",
    "--dirstat-by-file",
    "--dst-prefix",
    "--exclude-hidden",
    "--exit-code",
    "--ext-diff",
    "--find-copies",
    "--find-copies-harder",
    "--find-object",
    "--find-renames",
    "--follow",
    "--full-index",
    "--function-context",
    "--histogram",
    "--ignore-all-space",
    "--ignore-blank-lines",
    "--ignore-cr-at-eol",
    "--ignore-matching-lines",
    "--ignore-space-at-eol",
    "--ignore-space-change",
    "--ignore-submodules",
    "--indent-heuristic",
    "--inter-hunk-context",
    "--irreversible-delete",
    "--ita-invisible-in-index",
    "--ita-visible-in-index",
    "--left-only",
    "--left-right",
    "--line-prefix",
    "--max-age",
    "--max-count",
    "--max-depth",
    "--min-age",
    "--minimal",
    "--name-only",
    "--name-status",
    "--no-abbrev",
    "--no-color",
    "--no-color-moved",
    "--no-color-moved-ws",
    "--no-compact-summary",
    "--no-diff-merges",
    "--no-exit-code",
    "--no-ext-diff",
    "--no-find-copies-harder",
    "--no-follow",
    "--no-full-index",
    "--no-function-context",
    "--no-ignore-matching-lines",
    "--no-indent-heuristic",
    "--no-index",
    "--no-max-parents",
    "--no-merges",
    "--no-min-parents",
    "--no-notes",
    "--no-patch",
    "--no-prefix",
    "--no-quiet",
    "--no-relative",
    "--no-rename-empty",
    "--no-renames",
    "--no-text",
    "--no-textconv",
    "--notes",
    "--numstat",
    "--ours",
    "--output",
    "--output-indicator-context",
    "--output-indicator-new",
    "--output-indicator-old",
    "--parents",
    "--patch",
    "--patch-with-raw",
    "--patch-with-stat",
    "--patience",
    "--pickaxe-all",
    "--pickaxe-regex",
    "--quiet",
    "--raw",
    "--relative",
    "--remerge-diff",
    "--remotes",
    "--remove-empty",
    "--rename-empty",
    "--reverse",
    "--right-only",
    "--rotate-to",
    "--shortstat",
    "--skip-to",
    "--sparse",
    "--src-prefix",
    "--staged",
    "--stat",
    "--stat-count",
    "--stat-graph-width",
    "--stat-name-width",
    "--stat-width",
    "--stdin",
    "--summary",
    "--tags",
    "--text",
    "--textconv",
    "--theirs",
    "--topo-order",
    "--unified",
    "--unpacked",
    "--word-diff",
    "--word-diff-regex",
    "--ws-error-highlight",
];

/// The short options the same probe found stock `git diff` accepts.
const KNOWN_SHORT: &[u8] = b"abcghilmnpqrstuvwzBCDEFGIMOPRSUWX0123";

/// Whether stock `git diff` resolves `arg` to an option at all.
///
/// A long option is looked up without its `=<value>`; a short option carries its value
/// attached (`-S<string>`, `-U3`), so only the letter is looked up.
fn is_known_option(arg: &str) -> bool {
    match arg.starts_with("--") {
        true => KNOWN_LONG.contains(&arg.split_once('=').map_or(arg, |(n, _)| n)),
        false => arg.as_bytes().get(1).is_some_and(|c| KNOWN_SHORT.contains(c)),
    }
}

/// `builtin_diff_files()` (builtin/diff.c:267) reporting an argument no part of
/// `setup_revisions()` claimed:
///
/// ```text
/// error(_("invalid option: %s"), argv[1]);
/// usage(builtin_diff_usage);
/// ```
///
/// The other three dispatch targets — `builtin_diff_index()` (diff.c:150),
/// `builtin_diff_tree()` (diff.c:187) and `builtin_diff_combined()` (diff.c:220) — reach
/// the same leftover through a bare `usage(builtin_diff_usage)` with no `error()` ahead
/// of it, so the message appears only when no revision was given. Both exit 129, since
/// `usage()` is what ends the process either way.
fn invalid_option(arg: &str, have_revision: bool) -> ExitCode {
    if !have_revision {
        eprintln!("error: invalid option: {arg}");
    }
    usage_error()
}

/// A validated `diff.algorithm` config value.
enum ConfigAlgorithm {
    Use(gix::diff::blob::Algorithm),
}

/// Parse a `diff.algorithm` config value the way git's config loader does:
/// case-insensitively, accepting `myers`/`default`, `minimal`, `histogram` and
/// `patience`. Any other name is a hard config error (git exits 128) — rendered
/// here as the same "not available" bail the `--diff-algorithm=` flag uses.
fn parse_config_algorithm(name: &gix::bstr::BStr) -> Result<ConfigAlgorithm> {
    use gix::diff::blob::Algorithm::{Histogram, Myers, MyersMinimal, Patience};
    let lower = name.to_ascii_lowercase();
    Ok(match lower.as_slice() {
        b"myers" | b"default" => ConfigAlgorithm::Use(Myers),
        b"minimal" => ConfigAlgorithm::Use(MyersMinimal),
        b"histogram" => ConfigAlgorithm::Use(Histogram),
        b"patience" => ConfigAlgorithm::Use(Patience),
        _ => crate::git_fatal!("diff algorithm {:?} is not available", name.to_str_lossy()),
    })
}

/// The three outcomes of parsing a `-U`/`--unified` value, mirroring the two
/// distinct `error()` paths in git's `diff_opt_unified()`.
enum UnifiedValue {
    Context(u32),
    /// Trailing non-digit bytes (`*s != '\0'`) — "expects a numerical value".
    NotNumeric,
    /// A negative integer — "expects a non-negative integer".
    Negative,
}

/// Parse a `-U`/`--unified` value with git's `strtol(arg, &s, 10)` semantics:
/// leading whitespace and an optional sign are skipped, decimal digits are read,
/// and any trailing byte that is not part of the number (`*s != '\0'`) makes the
/// value non-numerical. An empty string yields context 0 (`strtol("")` performs no
/// conversion and leaves `*s` at the terminating NUL, which git accepts). Overflow
/// saturates rather than wrapping to a negative like git's `int` truncation would.
fn parse_unified(arg: &str) -> UnifiedValue {
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = matches!(b.get(i), Some(b'-'));
    if matches!(b.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digits_start = i;
    let mut val: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
        i += 1;
    }
    // No digits consumed: strtol performs no conversion and leaves `s` at the
    // original pointer (offset 0), so anything but a wholly empty string is junk.
    let end = if i == digits_start { 0 } else { i };
    if end < b.len() {
        return UnifiedValue::NotNumeric;
    }
    if neg && val != 0 {
        return UnifiedValue::Negative;
    }
    UnifiedValue::Context(val.min(u32::MAX as i64) as u32)
}

/// Convert an index-entry mode into an [`EntryKind`], or `None` for tree entries.
fn index_mode_kind(mode: gix::index::entry::Mode) -> Option<EntryKind> {
    mode.to_tree_entry_mode().map(|m| m.kind())
}

/// Record a change from a tree-vs-index diff. Gitlink (`160000`) entries flow
/// through as `EntryKind::Commit` deltas, which `analyze()` renders as the
/// `Subproject commit <oid>` short-format submodule diff.
fn collect_index_change(change: gix::diff::index::ChangeRef<'_, '_>, deltas: &mut Vec<Delta>) {
    use gix::diff::index::ChangeRef;
    match change {
        ChangeRef::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if let Some(k) = index_mode_kind(entry_mode) {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    None,
                    NewSide::Blob(id.into_owned(), k),
                ));
            }
        }
        ChangeRef::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if let Some(k) = index_mode_kind(entry_mode) {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    Some((id.into_owned(), k)),
                    NewSide::Absent,
                ));
            }
        }
        ChangeRef::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => {
            let ok = index_mode_kind(previous_entry_mode);
            let nk = index_mode_kind(entry_mode);
            if let (Some(ok), Some(nk)) = (ok, nk) {
                deltas.push(Delta::plain(
                    location.into_owned(),
                    Some((previous_id.into_owned(), ok)),
                    NewSide::Blob(id.into_owned(), nk),
                ));
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeRef::Rewrite { .. } => {}
    }
}

/// Record a change from a tree-vs-tree diff.
fn collect_tree_change(
    change: gix::object::tree::diff::ChangeDetached,
    deltas: &mut Vec<Delta>,
) -> Result<()> {
    use gix::object::tree::diff::ChangeDetached;
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            // Gitlinks (`160000`) flow through as `EntryKind::Commit` and are rendered
            // by `analyze()` as a `Subproject commit` submodule diff.
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(location, None, NewSide::Blob(id, entry_mode.kind())));
            }
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(location, Some((id, entry_mode.kind())), NewSide::Absent));
            }
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            if !entry_mode.is_tree() {
                deltas.push(Delta::plain(
                    location,
                    Some((previous_id, previous_entry_mode.kind())),
                    NewSide::Blob(id, entry_mode.kind()),
                ));
            }
        }
        // Rewrites are disabled, so this never fires; ignore defensively.
        ChangeDetached::Rewrite { .. } => {}
    }
    Ok(())
}

/// The `-p`/`--patch` body for one commit: its tree diffed against `parent`'s
/// tree (the empty tree for a root commit), rendered as git's `diff --git` patch
/// with `ctx` lines of context. This runs the exact delta pipeline `git diff`'s
/// tree-vs-tree path uses — `collect_tree_change` → `analyze` → `render_patch` —
/// so `git log -p` and `git diff` produce byte-identical patches (same index-line
/// abbreviation, `a/`/`b/` prefixes, and hunk formatting). Merge commits are the
/// caller's concern: git shows no diff for them without `-m`/`-c`/`--cc`, so `log`
/// only invokes this for commits with a single parent (or none).
#[allow(dead_code)] // shared single-parent patch renderer; kept for `log -p`/`diff` parity.
pub(crate) fn commit_patch(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    ctx: u32,
) -> Result<Vec<u8>> {
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;
    let r = patch_render(repo, &PatchOpts { ctx, ..Default::default() });
    commit_patch_with(repo, &mut cache, &r, commit.id, parent, &PatchOpts { ctx, ..Default::default() }, None, false)
}

/// The render settings `log -p`/`show` use for a patch body. Resolved once per
/// worker rather than per commit: `core.abbrev` cannot change mid-command, and
/// reading it is a config lookup.
fn patch_render(repo: &gix::Repository, opts: &PatchOpts) -> Render {
    let hash_kind = repo.object_hash();
    Render {
        // `git log -p`/`git show` honor core.abbrev on the index line, same as
        // `git diff` — resolved once here rather than hardcoded.
        abbrev: opts
            .index_abbrev
            .unwrap_or_else(|| crate::abbrev::configured_abbrev(repo, hash_kind.len_in_hex())),
        // This renderer only produces patches, so the raw width never applies.
        raw_abbrev: crate::abbrev::configured_abbrev(repo, hash_kind.len_in_hex()),
        full_index: opts.full_index,
        // The history commands do not parse `--binary` yet, so a binary pair there
        // still renders as `Binary files … differ`, exactly as before.
        binary: false,
        z: false,
        src_prefix: opts.src_prefix.clone(),
        dst_prefix: opts.dst_prefix.clone(),
        hash_kind,
    }
}

/// The diff options a history command can hand to the per-commit patch renderer.
///
/// These are the `diff_options` fields `setup_revisions()` fills from the same flags
/// `git diff` takes; everything not listed keeps `diff_setup()`'s default.
#[derive(Clone)]
pub(crate) struct PatchOpts {
    /// `-U<n>`/`--unified=<n>`: context lines.
    pub ctx: u32,
    /// `-w`/`-b`/`--ignore-space-at-eol`.
    pub ws: Whitespace,
    /// `--full-index`: the whole object name on the `index` line.
    pub full_index: bool,
    /// `-a`/`--text`: diff a binary file as text.
    pub text: bool,
    /// `-W`/`--function-context`.
    pub func_context: bool,
    /// `--no-prefix`/`--src-prefix=`/`--dst-prefix=`.
    pub src_prefix: Vec<u8>,
    pub dst_prefix: Vec<u8>,
    /// `--no-renames`/`--renames`/`-M[<n>]`. `None` leaves `diff.renames` — and the
    /// porcelain default of on — in charge.
    pub renames: Option<u8>,
    /// `-M<n>`'s similarity threshold in `MAX_SCORE` units; `0` is git's own 50%.
    pub rename_score: u32,
    /// `-C`/`--find-copies`: pair a new file with an unchanged one it was copied
    /// from. A second `-C` (`--find-copies-harder`) widens the candidate set.
    pub find_copies_harder: bool,
    /// `-B[<n>][/<m>]`: `diffcore_break()`'s packed score, `-1` when off.
    pub break_opt: i64,
    /// The `index` line's abbreviation, when it must differ from `core.abbrev`:
    /// `--no-abbrev` zeroes `revs->abbrev`, which the raw format reads as "print the
    /// whole id" while the `index` line falls back to the configured default.
    pub index_abbrev: Option<usize>,
}

impl Default for PatchOpts {
    fn default() -> Self {
        PatchOpts {
            ctx: 3,
            ws: Whitespace::Keep,
            full_index: false,
            text: false,
            func_context: false,
            src_prefix: b"a/".to_vec(),
            dst_prefix: b"b/".to_vec(),
            renames: None,
            rename_score: 0,
            find_copies_harder: false,
            break_opt: -1,
            index_abbrev: None,
        }
    }
}

/// The patch bodies for a batch of commits, one per job, in the caller's order.
///
/// Every entry is an independent tree-to-tree diff over immutable objects, so the
/// batch is embarrassingly parallel — and git renders `log -p` on a single core
/// no matter how many are idle, because its diff machinery has no threading at
/// all. Workers pull from a shared cursor rather than taking a fixed slice, since
/// one commit that rewrites a large file costs more than a hundred that touch a
/// line each; a static split would leave every worker but that one idle.
///
/// Neither `gix::Repository` nor the blob platform is `Sync`, so a worker owns a
/// clone of each. The clone shares the underlying object store, so it costs a
/// handle rather than a re-open.
pub(crate) fn commit_patches(
    repo: &gix::Repository,
    jobs: &[(ObjectId, Option<ObjectId>)],
    opts: &PatchOpts,
    paths: &[String],
    follow: bool,
) -> Result<Vec<Vec<u8>>> {
    // Four commits per worker: below that the handle clones cost more than the
    // split saves, and a short `log -p -n 3` should not spawn anything.
    let workers = crate::threads::count(jobs.len(), 4);
    // The pathspec set is parsed once per worker, never per commit.
    let matcher = |repo: &gix::Repository| -> Result<Option<super::log::PathspecMatcher>> {
        if paths.is_empty() {
            return Ok(None);
        }
        Ok(Some(super::log::PathspecMatcher::new(repo, paths)?))
    };

    if workers <= 1 {
        let mut cache = repo.diff_resource_cache_for_tree_diff()?;
        let r = patch_render(repo, opts);
        let mut specs = matcher(repo)?;
        return jobs
            .iter()
            .map(|(id, parent)| {
                commit_patch_with(repo, &mut cache, &r, *id, *parent, opts, specs.as_mut(), follow)
            })
            .collect();
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut done: Vec<(usize, Vec<u8>)> = Vec::with_capacity(jobs.len());
    let mut failure: Option<anyhow::Error> = None;
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let proto = repo.clone();
            let cursor = &cursor;
            let matcher = &matcher;
            handles.push(scope.spawn(move || -> Result<Vec<(usize, Vec<u8>)>> {
                let repo = proto;
                let mut cache = repo.diff_resource_cache_for_tree_diff()?;
                let r = patch_render(&repo, opts);
                let mut specs = matcher(&repo)?;
                let mut mine = Vec::new();
                loop {
                    let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((id, parent)) = jobs.get(i) else { break };
                    mine.push((
                        i,
                        commit_patch_with(&repo, &mut cache, &r, *id, *parent, opts, specs.as_mut(), follow)?,
                    ));
                }
                Ok(mine)
            }));
        }
        for h in handles {
            match h.join() {
                Ok(Ok(mine)) => done.extend(mine),
                Ok(Err(e)) => {
                    failure.get_or_insert(e);
                }
                // A panicked worker is a bug in the diff pipeline, not a user
                // error; surface it as one rather than silently dropping patches.
                Err(_) => {
                    failure.get_or_insert_with(|| anyhow::anyhow!("diff worker panicked"));
                }
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }

    let mut out: Vec<Vec<u8>> = vec![Vec::new(); jobs.len()];
    for (i, patch) in done {
        out[i] = patch;
    }
    Ok(out)
}

/// One commit's patch, reusing a caller-owned blob platform and render settings.
fn commit_patch_with(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    r: &Render,
    commit_id: ObjectId,
    parent: Option<ObjectId>,
    opts: &PatchOpts,
    specs: Option<&mut super::log::PathspecMatcher>,
    // `--follow`: the limit names the file *at this commit*, and the rename that
    // brought it there is the very thing to show — so the tree diff is taken whole,
    // rename detection runs on it, and only then is the pair whose destination
    // matches kept. Limiting the tree diff first would hide the deletion side and
    // leave the rename rendered as an addition.
    follow: bool,
) -> Result<Vec<u8>> {
    let commit = repo.find_object(commit_id)?.try_into_commit()?;
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };

    let changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        Some(gix::diff::Options::default()),
    )?;
    let mut deltas: Vec<Delta> = Vec::new();
    for change in changes {
        collect_tree_change(change, &mut deltas)?;
    }
    // `diff_flush()` order: paths ascending. Tree diffs never produce unmerged
    // deltas, so the secondary key is inert here but kept for parity with `diff()`.
    // `-- <pathspec>`: git limits the patch to the paths it was asked about, the
    // same list that decided which commits are shown.
    let mut follow_specs = None;
    match specs {
        Some(s) if follow => follow_specs = Some(s),
        Some(s) => deltas.retain(|delta| s.matches(&delta.path)),
        None => {}
    }
    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    // `diffcore_std()`: `git log`/`git show` are porcelains, so rename detection is on
    // unless `diff.renames` turns it off — a `git mv` commit is one `R` section, not a
    // deletion plus an addition.
    let ro = diffcore_rename::Options {
        detect_rename: opts.renames.unwrap_or_else(|| {
            diffcore_rename::config_rename(
                repo.config_snapshot()
                    .string("diff.renames")
                    .as_deref()
                    .map(|v| v.as_bstr()),
            )
        }),
        rename_score: opts.rename_score,
        find_copies_harder: opts.find_copies_harder,
        break_opt: opts.break_opt,
        rename_limit: repo
            .config_snapshot()
            .integer("diff.renameLimit")
            .unwrap_or(diffcore_rename::DEFAULT_RENAME_LIMIT),
        hash_kind: repo.object_hash(),
        ..Default::default()
    };
    // `-B` runs through the same pass even with no rename detection behind it.
    if ro.detect_rename != 0 || ro.break_opt != -1 {
        run_diffcore_rename(repo, cache, &mut deltas, &ro, false)?;
        deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));
    }
    // The `--follow` limit, applied once the rename it is following exists as a
    // pair: the destination is the name the file has at this commit.
    if let Some(specs) = follow_specs {
        deltas.retain(|delta| specs.matches(&delta.path));
    }

    let hash_kind = repo.object_hash();
    let mut out: Vec<u8> = Vec::new();
    for delta in &deltas {
        // A worktree side never arises for a tree diff, so `workdir` is `None`.
        let an = analyze(
            cache,
            &repo.objects,
            delta,
            opts.ctx,
            opts.ws,
            true,
            hash_kind,
            None,
            true,
            None,
            None,
            false,
            r.binary,
            opts.func_context,
        )?;
        render_patch(&mut out, repo, delta, &an, opts.ctx, r)?;
    }
    Ok(out)
}

/// `git log -L` / `git show -L`'s per-commit patch: `line_log_queue_pairs()`'
/// filepairs rendered by the same pipeline as `-p`, with each file's hunks clipped
/// to the ranges tracked at this commit (`builtin_diff`'s `line_ranges`).
pub(crate) fn line_range_patch(
    repo: &gix::Repository,
    pairs: &[(super::line_log::Pair, Vec<super::line_log::Range>)],
    ctx: u32,
) -> Result<Vec<u8>> {
    let r = patch_render(repo, &PatchOpts { ctx, ..Default::default() });
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;
    let hash_kind = repo.object_hash();
    let mut out: Vec<u8> = Vec::new();
    for (pair, ranges) in pairs {
        let new = match pair.new {
            Some((id, kind)) => NewSide::Blob(id, kind),
            None => NewSide::Absent,
        };
        let mut delta = Delta::plain(pair.path.clone(), pair.old, new);
        if pair.renamed() {
            // The two sides name different files, so the header reads
            // `a/<old> b/<new>` — but NOT as a rename. `line_log_queue_pairs()`
            // hands `diff_flush()` a `diff_filepair_dup()`, which copies only the
            // two filespecs; the score and the `R` status stay zeroed, so
            // `fill_metainfo()` emits no `similarity index` block and
            // `diff_resolve_rename_copy()` re-derives a plain `M`.
            delta.src_path = Some(pair.old_path.clone());
        }
        let an = analyze(
            &mut cache,
            &repo.objects,
            &delta,
            ctx,
            Whitespace::Keep,
            true,
            hash_kind,
            None,
            true,
            None,
            Some(ranges),
            false,
            r.binary,
            false,
        )?;
        render_patch(&mut out, repo, &delta, &an, ctx, &r)?;
    }
    Ok(out)
}

fn toggle_exec(k: EntryKind) -> EntryKind {
    match k {
        EntryKind::Blob => EntryKind::BlobExecutable,
        EntryKind::BlobExecutable => EntryKind::Blob,
        other => other,
    }
}

/// The current directory as a repository-relative prefix with a trailing slash —
/// git's `prefix`, which is what a valueless `--relative` narrows to. Empty at
/// the top level, and empty when the cwd cannot be placed inside the worktree.
fn cwd_prefix(repo: &gix::Repository) -> String {
    let (Some(workdir), Ok(cwd)) = (repo.workdir(), std::env::current_dir()) else {
        return String::new();
    };
    let (Ok(workdir), Ok(cwd)) = (workdir.canonicalize(), cwd.canonicalize()) else {
        return String::new();
    };
    match cwd.strip_prefix(&workdir) {
        Ok(rel) if rel.as_os_str().is_empty() => String::new(),
        Ok(rel) => format!("{}/", rel.to_string_lossy()),
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// blob analysis
// ---------------------------------------------------------------------------

/// Analyze every delta, across the thread pool when the change set is large
/// enough to pay for it.
///
/// This is the one path that a cache cannot serve: `git diff` against the
/// WORKING TREE reads files whose contents are not content-addressed, so there is
/// no stable key to memoize on. What it does have is independence — each delta
/// reads its own pair of sides and diffs them with no reference to the others —
/// and git leaves that on the table, diffing every file on one core.
///
/// Each worker clones the repository handle and builds its own blob platform;
/// neither type is `Sync`, and the platform additionally carries per-diff scratch
/// state that must not be shared. The caller's `cache` is used as-is on the
/// sequential path so a small diff allocates nothing extra.
#[allow(clippy::too_many_arguments)]
fn analyze_all(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    deltas: &[Delta],
    ctx: u32,
    ws: Whitespace,
    indent_heuristic: bool,
    hash_kind: gix::hash::Kind,
    workdir: Option<&std::path::Path>,
    want_patch: bool,
    algorithm: Option<gix::diff::blob::Algorithm>,
    worktree_mode: bool,
    // Whether `--dirstat` will need each pair's content damage.
    want_dirstat: bool,
    // Whether `--binary` will need each binary pair's two images.
    want_binary: bool,
    // `-W`: emit hunks grown to enclosing-function boundaries.
    func_context: bool,
) -> Result<Vec<Analysis>> {
    // Two files per worker. A handle clone plus a fresh blob platform is real
    // setup, but analyzing one file means reading and diffing both its sides —
    // enough work that even the five-file diff of a few commits' worth of change
    // measures faster split than sequential.
    let workers = crate::threads::count(deltas.len(), 2);
    if workers <= 1 {
        return deltas
            .iter()
            .map(|d| {
                analyze(
                    cache,
                    &repo.objects,
                    d,
                    ctx,
                    ws,
                    indent_heuristic,
                    hash_kind,
                    workdir,
                    want_patch,
                    algorithm,
                    None,
                    want_dirstat,
                    want_binary,
                    func_context,
                )
            })
            .collect();
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut done: Vec<(usize, Analysis)> = Vec::with_capacity(deltas.len());
    let mut failure: Option<anyhow::Error> = None;
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let proto = repo.clone();
            let cursor = &cursor;
            handles.push(scope.spawn(move || -> Result<Vec<(usize, Analysis)>> {
                let repo = proto;
                // The worker's platform must resolve the same sides the caller's
                // does: a worktree diff reads its "new" side off disk through
                // `WorktreeRoots`, and a tree pair reads both sides from the odb.
                let mut cache = match (worktree_mode, workdir) {
                    (true, Some(root)) => repo.diff_resource_cache(
                        Mode::ToGit,
                        WorktreeRoots { old_root: None, new_root: Some(root.to_owned()) },
                    )?,
                    _ => repo.diff_resource_cache_for_tree_diff()?,
                };
                let mut mine = Vec::new();
                loop {
                    let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(delta) = deltas.get(i) else { break };
                    let an = analyze(
                        &mut cache,
                        &repo.objects,
                        delta,
                        ctx,
                        ws,
                        indent_heuristic,
                        hash_kind,
                        workdir,
                        want_patch,
                        algorithm,
                        None,
                        want_dirstat,
                        want_binary,
                        func_context,
                    )?;
                    mine.push((i, an));
                }
                Ok(mine)
            }));
        }
        for h in handles {
            match h.join() {
                Ok(Ok(mine)) => done.extend(mine),
                Ok(Err(e)) => {
                    failure.get_or_insert(e);
                }
                Err(_) => {
                    failure.get_or_insert_with(|| anyhow::anyhow!("diff worker panicked"));
                }
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }

    done.sort_by_key(|(i, _)| *i);
    Ok(done.into_iter().map(|(_, a)| a).collect())
}

#[allow(clippy::too_many_arguments)]
fn analyze(
    cache: &mut gix::diff::blob::Platform,
    objects: &gix::OdbHandle,
    delta: &Delta,
    ctx: u32,
    ws: Whitespace,
    // `XDF_INDENT_HEURISTIC`: run the slider post-processing pass.
    indent_heuristic: bool,
    hash_kind: gix::hash::Kind,
    workdir: Option<&std::path::Path>,
    want_patch: bool,
    algo_override: Option<gix::diff::blob::Algorithm>,
    // `builtin_diff`'s `line_ranges`: under `-L`, the tracked 0-based ranges the
    // emitted hunks are clipped to.
    line_ranges: Option<&[super::line_log::Range]>,
    // Whether `--dirstat` needs each pair's content damage, which is a second pass
    // over both images that nothing else asks for.
    want_dirstat: bool,
    // Whether `--binary` needs a binary pair's two images, which the blob pipeline
    // withholds and nothing else asks for.
    want_binary: bool,
    // `-W`: hunks grown to enclosing-function boundaries.
    func_context: bool,
) -> Result<Analysis> {
    let null = hash_kind.null();
    if delta.unmerged {
        return Ok(Analysis {
            new_id: null,
            added: 0,
            deleted: 0,
            binary: false,
            hunks: None,
            blank_at_eof: (0, 0),
            damage: 0,
            images: None,
        });
    }

    // Submodule (gitlink) pairs cannot be read through the blob pipeline. git
    // renders them as a synthetic one-line `Subproject commit <oid>` blob per side,
    // so a modification counts as one insertion and one deletion.
    let old_commit = match delta.old {
        Some((id, EntryKind::Commit)) => Some(id),
        _ => None,
    };
    let new_commit = match &delta.new {
        NewSide::Blob(id, EntryKind::Commit) => Some(*id),
        // A worktree gitlink names the commit the submodule has checked out.
        NewSide::Worktree(EntryKind::Commit) => delta.new_commit,
        _ => None,
    };
    if old_commit.is_some() || new_commit.is_some() {
        return analyze_gitlink(
            old_commit,
            new_commit,
            delta.dirty_submodule,
            null,
            ctx,
            want_patch,
            algo_override,
        );
    }

    let path = delta.path.as_bstr();
    // The pre-image is looked up under its own name, which for a rename/copy is the
    // source path (git passes `p->one->path` for that side).
    let old_side_path = delta.old_path().as_bstr();
    let old_kind = delta.old.map(|(_, k)| k).unwrap_or(EntryKind::Blob);
    match delta.old {
        Some((id, k)) => cache.set_resource(id, k, old_side_path, ResourceKind::OldOrSource, objects)?,
        None => cache.set_resource(null, old_kind, old_side_path, ResourceKind::OldOrSource, objects)?,
    };
    match &delta.new {
        NewSide::Blob(id, k) => {
            cache.set_resource(*id, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Worktree(k) => {
            // With `new_root` set on the cache, a null id reads from the worktree by path.
            cache.set_resource(null, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Absent => {
            cache.set_resource(null, old_kind, path, ResourceKind::NewOrDestination, objects)?;
        }
    };

    let prep = cache.prepare_diff()?;

    let new_id: ObjectId = match &delta.new {
        NewSide::Absent => null,
        NewSide::Blob(id, _) => *id,
        NewSide::Worktree(_) => {
            if !prep.new.id.is_null() {
                prep.new.id.to_owned()
            } else if let Some(buf) = prep.new.data.as_slice() {
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, buf)?
            } else {
                // Binary worktree content: hash the raw file (filters not applied).
                let base = workdir.ok_or_else(|| anyhow::anyhow!("missing work tree"))?;
                let full = base.join(gix::path::from_bstr(path));
                let bytes = std::fs::read(&full)?;
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &bytes)?
            }
        }
    };

    match prep.operation {
        Operation::SourceOrDestinationIsBinary => {
            // The blob pipeline withholds the data for a binary pair, so both images
            // are read back here — and only if `--dirstat` or `--binary` is going to
            // use them, since for a binary pair that is the whole file on both sides.
            let images = if want_dirstat || want_binary {
                let old_bytes = delta
                    .old
                    .map(|(id, _)| read_blob(objects, id))
                    .transpose()?
                    .unwrap_or_default();
                let new_bytes = match &delta.new {
                    NewSide::Blob(id, _) => read_blob(objects, *id)?,
                    NewSide::Worktree(_) => workdir
                        .map(|base| std::fs::read(base.join(gix::path::from_bstr(path))))
                        .transpose()?
                        .unwrap_or_default(),
                    NewSide::Absent => Vec::new(),
                };
                Some((old_bytes, new_bytes))
            } else {
                None
            };
            Ok(Analysis {
                new_id,
                // `diffstat_consume()` never sees a binary pair; `show_stats()` reads
                // the two *sizes* out of the filespecs instead and prints them as
                // `Bin <old> -> <new> bytes`, so that is what these two carry here.
                // Every consumer that counts lines skips a pair with `binary` set.
                added: blob_size_new(objects, delta, workdir, path)?,
                deleted: blob_size_old(objects, delta)?,
                binary: true,
                hunks: None,
                blank_at_eof: (0, 0),
                // `show_dirstat()` weighs a binary pair like any other, on the raw
                // bytes with `hash_chars()` in its 64-byte-chunk mode.
                damage: match (want_dirstat, &images) {
                    (true, Some((old_bytes, new_bytes))) => {
                        byte_damage(old_bytes, new_bytes, delta.old.is_some(), delta.new_valid(), true)
                    }
                    _ => 0,
                },
                images: want_binary.then_some(images).flatten(),
            })
        }
        Operation::ExternalCommand { .. } => {
            bail!("external diff drivers are not supported for {path:?}")
        }
        Operation::InternalDiff { algorithm } => {
            // `--minimal`/`--histogram`/`--diff-algorithm=` override the default.
            let algorithm = algo_override.unwrap_or(algorithm);
            let old_data = prep.old.data.as_slice().unwrap_or_default();
            let new_data = prep.new.data.as_slice().unwrap_or_default();
            // `check_blank_at_eof()` runs on the whole images, before xdiff, so the
            // emit layer can tell an added blank line at EOF from an ordinary one.
            let blank_at_eof = diff_color::check_blank_at_eof(old_data, new_data);

            // `builtin_diff()`: a `-B` rewrite that stayed a modification never runs
            // xdiff at all. `emit_rewrite_diff()` replaces the whole file instead —
            // one hunk deleting every old line and adding every new one — and
            // `diffstat` counts the same way (`count_lines()` on each side).
            if delta.complete_rewrite() {
                let deleted = count_lines(old_data);
                let added = count_lines(new_data);
                let hunks = want_patch.then(|| emit_rewrite_diff(old_data, new_data));
                return Ok(Analysis {
                    new_id,
                    added,
                    deleted,
                    binary: false,
                    hunks,
                    blank_at_eof,
                    damage: if want_dirstat {
                        byte_damage(old_data, new_data, delta.old.is_some(), delta.new_valid(), false)
                    } else {
                        0
                    },
                    images: None,
                });
            }
            let before: Vec<&[u8]> = byte_lines(old_data);
            let after: Vec<&[u8]> = byte_lines(new_data);
            let mut input: InternedInput<Vec<u8>> = InternedInput::default();
            input.update_before(before.iter().map(|l| normalize_line(l, ws)));
            input.update_after(after.iter().map(|l| normalize_line(l, ws)));

            // `xdl_change_compact()` measures `xdf->recs[i]->ptr`, the *original*
            // record, not the whitespace-normalized token the comparison used.
            let diff =
                super::diff_pairs::compute_compacted(algorithm, &input, &before, &after, indent_heuristic);
            let added = diff.count_additions();
            let deleted = diff.count_removals();
            let hunks = if want_patch && (added != 0 || deleted != 0) {
                match line_ranges {
                    // `-L`: xdiff runs with the context inflated to the widest
                    // tracked span so every change inside one range lands in a
                    // single hunk, and the sink clips back to the range bounds.
                    Some(rs) => {
                        let ctx = super::line_log::RangeSink::context(rs, ctx);
                        let sink = super::line_log::RangeSink::new(&before, &after, rs);
                        Some(
                            UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx))
                                .consume()?,
                        )
                    }
                    // `-W` changes the hunk *geometry*, not just the text inside
                    // it: both ends grow to the enclosing function and neighbours
                    // that end up overlapping merge. gitoxide's unified writer has
                    // one fixed context on both sides and cannot express that, so
                    // this takes the `xdl_emit_diff` port instead — the same
                    // emitter `git diff-pairs` runs, driven off the same change
                    // script.
                    None if func_context => {
                        let changes: Vec<super::diff_pairs::Change> = diff
                            .hunks()
                            .map(|h| super::diff_pairs::Change {
                                i1: h.before.start as usize,
                                chg1: h.before.len(),
                                i2: h.after.start as usize,
                                chg2: h.after.len(),
                                // `--ignore-blank-lines`/`-I` are not honoured on
                                // this path, so no change is ever ignorable.
                                ignore: false,
                            })
                            .collect();
                        let (_, _, buf) = super::diff_pairs::emit_unified(
                            &before,
                            &after,
                            &changes,
                            &super::diff_pairs::EmitGeometry {
                                ctx: ctx as usize,
                                inter_hunk_ctx: 0,
                                func_context: true,
                            },
                        );
                        Some(buf)
                    }
                    None => {
                        let sink = PatchSink {
                            buf: Vec::new(),
                            before: &before,
                            after: &after,
                            // No hunk has been emitted yet, so nothing bounds the
                            // first search.
                            func_prev: -1,
                            func_text: Vec::new(),
                        };
                        Some(
                            UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx))
                                .consume()?,
                        )
                    }
                }
            } else {
                None
            };
            Ok(Analysis {
                new_id,
                added,
                deleted,
                binary: false,
                hunks,
                blank_at_eof,
                damage: if want_dirstat {
                    byte_damage(old_data, new_data, delta.old.is_some(), delta.new_valid(), false)
                } else {
                    0
                },
                images: None,
            })
        }
    }
}

/// `diff_populate_filespec(..., CHECK_SIZE_ONLY)` for the pre-image: the blob's
/// size without reading it, which is all `show_stats()` wants for a binary pair.
fn blob_size_old(objects: &gix::OdbHandle, delta: &Delta) -> Result<u32> {
    use gix::objs::FindHeader;
    let Some((id, _)) = delta.old else { return Ok(0) };
    Ok(objects.try_header(&id).ok().flatten().map(|h| h.size).unwrap_or(0) as u32)
}

/// The post-image half of the same, reading a worktree side's size off disk.
fn blob_size_new(
    objects: &gix::OdbHandle,
    delta: &Delta,
    workdir: Option<&std::path::Path>,
    path: &gix::bstr::BStr,
) -> Result<u32> {
    use gix::objs::FindHeader;
    match &delta.new {
        NewSide::Absent => Ok(0),
        NewSide::Blob(id, _) => Ok(objects.try_header(id).ok().flatten().map(|h| h.size).unwrap_or(0) as u32),
        NewSide::Worktree(_) => {
            let Some(base) = workdir else { return Ok(0) };
            let full = base.join(gix::path::from_bstr(path));
            Ok(std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0) as u32)
        }
    }
}

/// Read a blob's bytes straight from the object database, for the one case the
/// blob pipeline declines to hand them over: a pair it classified as binary.
fn read_blob(objects: &gix::OdbHandle, id: ObjectId) -> Result<Vec<u8>> {
    use gix::prelude::FindExt;
    let mut buf = Vec::new();
    objects.find_blob(&id, &mut buf)?;
    Ok(buf.clone())
}

/// `show_dirstat()`'s content-mode damage for one pair (diff.c:3033-3068): the
/// bytes of the pre-image that did not survive, plus the bytes the post-image
/// gained. A pair with only one side is charged that side's whole size, which is
/// how a pure addition or deletion is weighed against a modification.
///
/// `diffcore_count_changes()` is the same chunk-hashing counter rename detection
/// scores with, so `--dirstat` and `-M` agree about how much a file moved.
fn byte_damage(old_data: &[u8], new_data: &[u8], old_valid: bool, new_valid: bool, binary: bool) -> u64 {
    match (old_valid, new_valid) {
        (true, true) => {
            let (copied, added) =
                super::diff_files::count_changes_sides(old_data, !binary, new_data, !binary);
            (old_data.len() as u64).saturating_sub(copied) + added
        }
        (true, false) => old_data.len() as u64,
        (false, true) => new_data.len() as u64,
        (false, false) => 0,
    }
}

/// `diff_filespec_is_binary()`: a NUL byte in the first 8000 bytes, which is the
/// only test `--no-index` has to go on for a path with no attributes behind it.
pub(crate) fn looks_binary(buf: &[u8]) -> bool {
    buf.get(..8000).unwrap_or(buf).contains(&0)
}

/// The hunk body of one `--no-index` pair, produced by the same `xdl_emit_diff`
/// port the tracked patch path uses so the two cannot drift apart.
pub(crate) fn no_index_body(
    old_data: &[u8],
    new_data: &[u8],
    geom: &super::diff_pairs::EmitGeometry,
    ws: Whitespace,
    binary: bool,
) -> (u32, u32, Vec<u8>) {
    if binary {
        return (0, 0, Vec::new());
    }
    let before: Vec<&[u8]> = byte_lines(old_data);
    let after: Vec<&[u8]> = byte_lines(new_data);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| normalize_line(l, ws)));
    input.update_after(after.iter().map(|l| normalize_line(l, ws)));
    let diff = super::diff_pairs::compute_compacted(
        gix::diff::blob::Algorithm::Histogram,
        &input,
        &before,
        &after,
        true,
    );
    let changes: Vec<super::diff_pairs::Change> = diff
        .hunks()
        .map(|h| super::diff_pairs::Change {
            i1: h.before.start as usize,
            chg1: h.before.len(),
            i2: h.after.start as usize,
            chg2: h.after.len(),
            ignore: false,
        })
        .collect();
    super::diff_pairs::emit_unified(&before, &after, &changes, geom)
}

/// `--stat` for `--no-index` rows, which have two names rather than one. Each row
/// becomes the synthetic rename pair `show_stats()` would have been handed, so the
/// column widths and the `{a => b}/c` name compaction are the tracked ones.
pub(crate) fn render_rows_stat(
    out: &mut Vec<u8>,
    rows: &[(BString, BString, u32, u32, bool)],
    colors: &diff_color::DiffColors,
) {
    let (deltas, analyses) = synthetic_rows(rows);
    render_stat(out, &diffstat_pairs(&deltas, &analyses), false, 0, 0, colors);
}

/// `--numstat` for the same rows.
pub(crate) fn render_rows_numstat(out: &mut Vec<u8>, rows: &[(BString, BString, u32, u32, bool)], z: bool) {
    let (deltas, analyses) = synthetic_rows(rows);
    render_numstat(out, &diffstat_pairs(&deltas, &analyses), z);
}

/// The `(Delta, Analysis)` pair a `--no-index` row stands for: a rename when the
/// two names differ, an ordinary modification when they do not.
fn synthetic_rows(rows: &[(BString, BString, u32, u32, bool)]) -> (Vec<Delta>, Vec<Analysis>) {
    let null = gix::hash::Kind::Sha1.null();
    let mut deltas = Vec::with_capacity(rows.len());
    let mut analyses = Vec::with_capacity(rows.len());
    for (a, b, added, deleted, binary) in rows {
        let mut d = Delta::plain(b.clone(), Some((null, EntryKind::Blob)), NewSide::Blob(null, EntryKind::Blob));
        if a != b {
            d.src_path = Some(a.clone());
            d.status = b'R';
        }
        deltas.push(d);
        analyses.push(Analysis {
            new_id: null,
            added: *added,
            deleted: *deleted,
            binary: *binary,
            hunks: None,
            blank_at_eof: (0, 0),
            damage: 0,
            images: None,
        });
    }
    (deltas, analyses)
}

/// `count_lines()`: lines in a buffer, counting an unterminated final line.
fn count_lines(data: &[u8]) -> u32 {
    if data.is_empty() {
        return 0;
    }
    let mut count = data.iter().filter(|&&b| b == b'\n').count() as u32;
    if data[data.len() - 1] != b'\n' {
        count += 1; // no trailing newline
    }
    count
}

/// `add_line_count()`: the range half of a rewrite's `@@` line.
fn rewrite_line_count(count: u32) -> String {
    match count {
        0 => "0,0".to_string(),
        1 => "1".to_string(),
        n => format!("1,{n}"),
    }
}

/// `emit_rewrite_diff()`: a `-B` rewrite's body — a single hunk spanning both whole
/// files, every old line removed and every new line added, with no context.
fn emit_rewrite_diff(old_data: &[u8], new_data: &[u8]) -> Vec<u8> {
    let lc_a = count_lines(old_data);
    let lc_b = count_lines(new_data);
    let mut out = Vec::new();
    push_str(&mut out, "@@ -");
    push_str(&mut out, &rewrite_line_count(lc_a));
    push_str(&mut out, " +");
    push_str(&mut out, &rewrite_line_count(lc_b));
    push_str(&mut out, " @@\n");
    if lc_a != 0 {
        emit_rewrite_lines(&mut out, b'-', old_data);
    }
    if lc_b != 0 {
        emit_rewrite_lines(&mut out, b'+', new_data);
    }
    out
}

/// `emit_rewrite_lines()`: every line of `data` prefixed by `prefix`, with git's
/// incomplete-last-line marker when the buffer does not end in a newline.
fn emit_rewrite_lines(out: &mut Vec<u8>, prefix: u8, data: &[u8]) {
    let mut rest = data;
    let mut ended_with_newline = false;
    while !rest.is_empty() {
        let (line, tail) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => {
                ended_with_newline = true;
                (&rest[..=i], &rest[i + 1..])
            }
            None => {
                ended_with_newline = false;
                (rest, &rest[rest.len()..])
            }
        };
        out.push(prefix);
        out.extend_from_slice(line);
        if !ended_with_newline {
            out.push(b'\n');
        }
        rest = tail;
    }
    if !ended_with_newline {
        push_str(out, "\\ No newline at end of file\n");
    }
}

/// Diff a submodule (gitlink) pair as git's `show_submodule_summary`-free short
/// format does: one `Subproject commit <full-oid>` line per present side. The new
/// object id on the `index` line is the new commit id (or null when removed).
///
/// `dirty` is `two->dirty_submodule`: `diff_populate_gitlink()` (diff.c:4475) glues
/// `-dirty` onto the post-image line whenever any of its bits is set, so a submodule
/// with local damage differs from its own recorded commit.
fn analyze_gitlink(
    old_commit: Option<ObjectId>,
    new_commit: Option<ObjectId>,
    dirty: u8,
    null: ObjectId,
    ctx: u32,
    want_patch: bool,
    algo_override: Option<gix::diff::blob::Algorithm>,
) -> Result<Analysis> {
    let line = |id: ObjectId, dirty: bool| -> Vec<u8> {
        let mut v = b"Subproject commit ".to_vec();
        v.extend_from_slice(id.to_hex().to_string().as_bytes());
        if dirty {
            v.extend_from_slice(b"-dirty");
        }
        v.push(b'\n');
        v
    };
    let before: Vec<Vec<u8>> = old_commit.map(|id| vec![line(id, false)]).unwrap_or_default();
    let after: Vec<Vec<u8>> = new_commit
        .map(|id| vec![line(id, dirty != 0)])
        .unwrap_or_default();
    let before_r: Vec<&[u8]> = before.iter().map(|l| l.as_slice()).collect();
    let after_r: Vec<&[u8]> = after.iter().map(|l| l.as_slice()).collect();

    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before_r.iter().map(|l| l.to_vec()));
    input.update_after(after_r.iter().map(|l| l.to_vec()));
    let algorithm = algo_override.unwrap_or(gix::diff::blob::Algorithm::Myers);
    let diff = diff_with_slider_heuristics(algorithm, &input);
    // The `-dirty` marker moves the patch but not the stat formats: measured against
    // git 2.55.0 on a submodule whose worktree is damaged at the commit the index
    // already records, `git diff` prints the `-Subproject commit <oid>` /
    // `+Subproject commit <oid>-dirty` hunk while `git diff --numstat` prints
    // `0\t0\tsub` and `--shortstat` prints `1 file changed, 0 insertions(+),
    // 0 deletions(-)`. So the counts come from the two commit ids alone.
    let (added, deleted) = if dirty != 0 && old_commit == new_commit {
        (0, 0)
    } else {
        (diff.count_additions(), diff.count_removals())
    };
    let hunks = if want_patch && (diff.count_additions() != 0 || diff.count_removals() != 0) {
        let sink = PatchSink {
            buf: Vec::new(),
            before: &before_r,
            after: &after_r,
            // No hunk has been emitted yet, so nothing bounds the first search.
            func_prev: -1,
            func_text: Vec::new(),
        };
        Some(UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx)).consume()?)
    } else {
        None
    };
    Ok(Analysis {
        new_id: new_commit.unwrap_or(null),
        added,
        deleted,
        binary: false,
        hunks,
        // A synthetic `Subproject commit <oid>` blob never ends in a blank line.
        blank_at_eof: (0, 0),
        images: None,
        // The same synthetic images `builtin_diff()` hands the rest of the diff
        // machinery, so a submodule bump is damage like any other content change.
        damage: byte_damage(
            &before.concat(),
            &after.concat(),
            old_commit.is_some(),
            new_commit.is_some(),
            false,
        ),
    })
}

/// Split `data` into lines the way `imara_diff::sources::byte_lines` does: the
/// terminator stays attached, and a final line without one is still a line.
pub(crate) fn byte_lines(data: &[u8]) -> Vec<&[u8]> {
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
pub(crate) fn normalize_line(line: &[u8], ws: Whitespace) -> Vec<u8> {
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r' | b'\n');
    match ws {
        Whitespace::Keep => line.to_vec(),
        Whitespace::IgnoreAll => line.iter().copied().filter(|b| !is_space(*b)).collect(),
        Whitespace::IgnoreAtEol => {
            let end = line.iter().rposition(|b| !is_space(*b)).map_or(0, |i| i + 1);
            line[..end].to_vec()
        }
        // `XDF_IGNORE_CR_AT_EOL`: exactly one CR, and only where it sits against the
        // line terminator.
        Whitespace::IgnoreCrAtEol => {
            let mut out = line.to_vec();
            match out.last() {
                Some(b'\n') if out.len() >= 2 && out[out.len() - 2] == b'\r' => {
                    out.remove(out.len() - 2);
                }
                Some(b'\r') => {
                    out.pop();
                }
                _ => {}
            }
            out
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
// rendering
// ---------------------------------------------------------------------------

fn mode_octal(k: Option<EntryKind>) -> String {
    match k {
        None => "000000".to_string(),
        Some(k) => mode_str(k).to_string(),
    }
}

fn mode_str(k: EntryKind) -> &'static str {
    std::str::from_utf8(k.as_octal_str()).unwrap_or("100644")
}

/// `--raw` and `--name-status` (`diff_flush_raw()`).
fn render_raw(out: &mut Vec<u8>, delta: &Delta, fmt: u32, r: &Render) {
    let status = status_char(delta);
    if fmt & F_NAME_STATUS == 0 {
        let null = r.hash_kind.null().to_hex_with_len(r.raw_abbrev).to_string();
        let old_hash = delta
            .old
            .map(|(id, _)| id.to_hex_with_len(r.raw_abbrev).to_string())
            .unwrap_or_else(|| null.clone());
        // Worktree content has no object id yet, which git reports as all-zero —
        // unless rename detection already hashed it (`hash_filespec()`).
        let new_hash = match (&delta.new, delta.unmerged) {
            (NewSide::Blob(id, _), false) => id.to_hex_with_len(r.raw_abbrev).to_string(),
            (NewSide::Worktree(_), false) => match delta.new_id {
                Some(id) => id.to_hex_with_len(r.raw_abbrev).to_string(),
                None => null,
            },
            _ => null,
        };
        push_str(out, ":");
        push_str(out, &mode_octal(delta.old.map(|(_, k)| k)));
        push_str(out, " ");
        push_str(out, &mode_octal(delta.new_kind()));
        push_str(out, " ");
        push_str(out, &old_hash);
        push_str(out, " ");
        push_str(out, &new_hash);
        push_str(out, " ");
    }
    out.push(status);
    // `diff_flush_raw()`: a scored pair prints its similarity as three digits right
    // after the status letter (`R100`, `C085`, `M090` for a `-B` rewrite).
    if delta.score != 0 {
        push_str(out, &format!("{:03}", diffcore_rename::similarity_index(delta.score)));
    }
    // `-z`: the field / record separators become NUL and paths are not C-quoted.
    out.push(if r.z { 0 } else { b'\t' });
    // A rename/copy prints both names, source first, separated like any other field.
    if matches!(status, b'R' | b'C') {
        out.extend_from_slice(&name_field(delta.old_path(), r.z));
        out.push(if r.z { 0 } else { b'\t' });
    }
    out.extend_from_slice(&name_field(&delta.path, r.z));
    out.push(if r.z { 0 } else { b'\n' });
}

/// A path as a `--raw`/`--name-*` field: raw bytes under `-z`, otherwise C-quoted.
fn name_field(path: &BString, z: bool) -> Vec<u8> {
    if z {
        path.as_slice().to_vec()
    } else {
        quoted_name(path)
    }
}

/// `--summary` (`show_summary()` / `diff_summary_line()`): creation, deletion and
/// mode-change lines, one per delta in queue order.
fn render_summary(out: &mut Vec<u8>, deltas: &[Delta]) {
    for d in deltas {
        if d.unmerged {
            continue;
        }
        // `show_rename_copy()`: `<verb> <pprint'd names> (<n>%)`, then the mode change
        // line without a name.
        if d.renamed() {
            push_str(out, if d.status == b'C' { " copy " } else { " rename " });
            out.extend_from_slice(&pprint_rename(d.old_path(), &d.path));
            push_str(
                out,
                &format!(" ({}%)\n", diffcore_rename::similarity_index(d.score)),
            );
            summary_mode_change(out, d, false);
            continue;
        }
        match (d.old, d.new_kind()) {
            (None, Some(nk)) => {
                push_str(out, " create mode ");
                push_str(out, mode_str(nk));
                out.push(b' ');
                out.extend_from_slice(&quoted_name(&d.path));
                out.push(b'\n');
            }
            (Some((_, ok)), None) => {
                push_str(out, " delete mode ");
                push_str(out, mode_str(ok));
                out.push(b' ');
                out.extend_from_slice(&quoted_name(&d.path));
                out.push(b'\n');
            }
            _ => {
                // `diff_summary()`'s default arm: a `-B` rewrite prints its own line
                // and suppresses the name on the mode-change line that follows.
                if d.score != 0 {
                    push_str(out, " rewrite ");
                    out.extend_from_slice(&quoted_name(&d.path));
                    push_str(
                        out,
                        &format!(" ({}%)\n", diffcore_rename::similarity_index(d.score)),
                    );
                }
                summary_mode_change(out, d, d.score == 0);
            }
        }
    }
}

/// `show_mode_change()`: the ` mode change <old> => <new>` line, with the path only
/// when `show_name` (a plain modification; rename/copy/rewrite omit it).
fn summary_mode_change(out: &mut Vec<u8>, d: &Delta, show_name: bool) {
    let (Some((_, ok)), Some(nk)) = (d.old, d.new_kind()) else {
        return;
    };
    if ok == nk {
        return;
    }
    push_str(out, " mode change ");
    push_str(out, mode_str(ok));
    push_str(out, " => ");
    push_str(out, mode_str(nk));
    if show_name {
        out.push(b' ');
        out.extend_from_slice(&quoted_name(&d.path));
    }
    out.push(b'\n');
}

/// `pprint_rename()`: compress the common leading directory and trailing suffix of a
/// rename/copy into `pfx{old-mid => new-mid}sfx`.
fn pprint_rename(a: &[u8], b: &[u8]) -> Vec<u8> {
    let (la, lb) = (a.len(), b.len());
    // git walks NUL-terminated strings, so index past the end reads as NUL.
    let at = |s: &[u8], i: usize| -> u8 { if i < s.len() { s[i] } else { 0 } };

    // Common prefix, recorded up to and including the last shared slash.
    let mut pfx = 0usize;
    {
        let mut i = 0;
        while i < la && i < lb && a[i] == b[i] {
            if a[i] == b'/' {
                pfx = i + 1;
            }
            i += 1;
        }
    }

    // Common suffix, from the (virtual) terminators backwards, stopping at the prefix.
    let mut sfx = 0usize;
    {
        let pfx_adjust = if pfx > 0 { 1isize } else { 0 };
        let lo = pfx as isize - pfx_adjust;
        let mut oa = la as isize;
        let mut ob = lb as isize;
        while oa >= lo && ob >= lo && at(a, oa as usize) == at(b, ob as usize) {
            if at(a, oa as usize) == b'/' {
                sfx = la - oa as usize;
            }
            oa -= 1;
            ob -= 1;
        }
    }

    let a_mid = (la as isize - pfx as isize - sfx as isize).max(0) as usize;
    let b_mid = (lb as isize - pfx as isize - sfx as isize).max(0) as usize;

    let mut out = Vec::new();
    if pfx + sfx > 0 {
        out.extend_from_slice(&a[..pfx]);
        out.push(b'{');
        out.extend_from_slice(&a[pfx..pfx + a_mid]);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(&b[pfx..pfx + b_mid]);
        out.push(b'}');
        out.extend_from_slice(&a[la - sfx..]);
    } else {
        out.extend_from_slice(a);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(b);
    }
    out
}

/// `--name-status` letter for a delta. `diff_resolve_rename_copy()` has already
/// decided it whenever the diffcore rename pass ran; otherwise derive it here.
fn status_char(d: &Delta) -> u8 {
    if d.unmerged {
        return b'U';
    }
    if d.status != 0 {
        return d.status;
    }
    match (&d.old, &d.new) {
        (None, _) => b'A',
        (_, NewSide::Absent) => b'D',
        _ => b'M',
    }
}

/// The pairs the diffstat formats actually see.
///
/// `builtin_diffstat()` (diff.c:3882) throws an entry away again right after running
/// xdiff when the pair is a plain modification, both sides are present with the same
/// mode, and the comparison produced no changed line — "omit diffstats of modified
/// files where nothing changed", which is what a whitespace-ignoring option leaves
/// behind. With the entry gone, `--stat`, `--numstat` and `--shortstat` print nothing
/// at all rather than a `| 0` row plus a `1 file changed` summary.
///
/// The drop sits inside `builtin_diffstat()`'s `may_differ` arm, so it never applies
/// when the two ids are equal: a dirty submodule (same commit on both sides) keeps
/// its `0\t0\t<path>` row, and so does a pure mode change.
fn diffstat_pairs<'a>(deltas: &'a [Delta], analyses: &'a [Analysis]) -> Vec<(&'a Delta, &'a Analysis)> {
    deltas
        .iter()
        .zip(analyses)
        .filter(|(d, an)| {
            if d.unmerged || an.binary || d.complete_rewrite() {
                return true;
            }
            let (Some((old_id, old_kind)), Some(new_kind)) = (d.old, d.new_kind()) else {
                return true;
            };
            let may_differ = old_id != an.new_id;
            let dropped = may_differ
                && status_char(d) == b'M'
                && an.added == 0
                && an.deleted == 0
                && old_kind == new_kind;
            !dropped
        })
        .collect()
}

/// `--numstat` (`show_numstat()`).
///
/// A rename/copy prints the `pprint_rename`d name in the newline-terminated form and
/// the two raw names, each NUL-terminated and preceded by an extra NUL, under `-z`.
fn render_numstat(out: &mut Vec<u8>, pairs: &[(&Delta, &Analysis)], z: bool) {
    for (d, an) in pairs.iter().copied() {
        if an.binary {
            push_str(out, "-\t-\t");
        } else {
            push_str(out, &format!("{}\t{}\t", an.added, an.deleted));
        }
        if z {
            if d.renamed() {
                out.push(0);
                out.extend_from_slice(d.old_path());
                out.push(0);
            }
            out.extend_from_slice(&d.path);
            out.push(0);
        } else {
            if d.renamed() {
                out.extend_from_slice(&pprint_rename(d.old_path(), &d.path));
            } else {
                out.extend_from_slice(&quoted_name(&d.path));
            }
            out.push(b'\n');
        }
    }
}

/// `--shortstat` (`show_shortstats()`).
fn render_shortstat(out: &mut Vec<u8>, pairs: &[(&Delta, &Analysis)]) {
    // `show_shortstats()` (diff.c:2934) returns before the summary line when the
    // diffstat holds no entries, so a run whose every pair was dropped prints
    // nothing rather than ` 0 files changed`.
    if pairs.is_empty() {
        return;
    }
    let (files, adds, dels) = stat_totals(pairs);
    stat_summary(out, files, adds, dels);
}

fn stat_totals(pairs: &[(&Delta, &Analysis)]) -> (u32, u32, u32) {
    let mut files = pairs.len() as u32;
    let (mut adds, mut dels) = (0u32, 0u32);
    for (d, an) in pairs.iter().copied() {
        if d.unmerged {
            files -= 1;
        } else if !an.binary {
            adds += an.added;
            dels += an.deleted;
        }
    }
    (files, adds, dels)
}

/// `print_stat_summary_inserts_deletes()`.
pub(crate) fn stat_summary(out: &mut Vec<u8>, files: u32, insertions: u32, deletions: u32) {
    if files == 0 {
        push_str(out, " 0 files changed\n");
        return;
    }
    push_str(
        out,
        &format!(" {files} file{} changed", if files == 1 { "" } else { "s" }),
    );
    if insertions != 0 || deletions == 0 {
        push_str(
            out,
            &format!(
                ", {insertions} insertion{}(+)",
                if insertions == 1 { "" } else { "s" }
            ),
        );
    }
    if deletions != 0 || insertions == 0 {
        push_str(
            out,
            &format!(
                ", {deletions} deletion{}(-)",
                if deletions == 1 { "" } else { "s" }
            ),
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

/// `get_compact_summary()`: the parenthesized annotation `--compact-summary`
/// appends to a diffstat name. Mirrors `diff.c`'s status/mode ladder, in order:
/// creation (`new`/`new +x`/`new +l`), deletion (`gone`), then the symlink and
/// executable-bit mode transitions. Returns `None` when no annotation applies
/// (a content-only modification) so the name is printed bare.
fn compact_comment(d: &Delta) -> Option<&'static str> {
    // git computes the annotation from `p->one`/`p->two`; an unmerged pair has no
    // usable filespec modes here, so it carries no comment.
    if d.unmerged {
        return None;
    }
    let old = d.old.map(|(_, k)| k);
    let new = d.new_kind();
    // DIFF_STATUS_ADDED.
    if old.is_none() {
        return Some(match new {
            Some(EntryKind::Link) => "new +l",
            Some(EntryKind::BlobExecutable) => "new +x",
            _ => "new",
        });
    }
    // DIFF_STATUS_DELETED.
    if new.is_none() {
        return Some("gone");
    }
    let (ok, nk) = (old.expect("old present"), new.expect("new present"));
    let old_link = ok == EntryKind::Link;
    let new_link = nk == EntryKind::Link;
    if old_link && !new_link {
        Some("mode -l")
    } else if !old_link && new_link {
        Some("mode +l")
    } else if ok == EntryKind::Blob && nk == EntryKind::BlobExecutable {
        Some("mode +x")
    } else if ok == EntryKind::BlobExecutable && nk == EntryKind::Blob {
        Some("mode -x")
    } else {
        None
    }
}

/// The diffstat display name: the C-quoted path, plus the `--compact-summary`
/// annotation ` (<comment>)` when one applies (`fill_print_name()`).
fn stat_display_name(d: &Delta, compact: bool) -> Vec<u8> {
    // `fill_print_name()`: a rename/copy shows the compressed `pfx{a => b}sfx` form.
    let mut name = if d.renamed() {
        pprint_rename(d.old_path(), &d.path)
    } else {
        quoted_name(&d.path)
    };
    if compact {
        if let Some(c) = compact_comment(d) {
            name.push(b' ');
            name.push(b'(');
            name.extend_from_slice(c.as_bytes());
            name.push(b')');
        }
    }
    name
}

/// `--stat` (`show_stats()`), with git's default 80-column budget. `stat_name_width`
/// and `stat_graph_width` (`0` == unset) cap the filename and graph columns, coming
/// from `--stat-name-width`/`--stat-graph-width` or `diff.statNameWidth`/`diff.statGraphWidth`.
fn render_stat(
    out: &mut Vec<u8>,
    pairs: &[(&Delta, &Analysis)],
    compact: bool,
    stat_name_width: i64,
    stat_graph_width: i64,
    colors: &diff_color::DiffColors,
) {
    // `show_stats()` (diff.c:2664) returns immediately on an empty diffstat.
    if pairs.is_empty() {
        return;
    }
    let names: Vec<Vec<u8>> = pairs.iter().map(|(d, _)| stat_display_name(d, compact)).collect();

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    for (i, (d, an)) in pairs.iter().copied().enumerate() {
        let change = (an.added + an.deleted) as i64;
        max_len = max_len.max(names[i].len() as i64);
        if d.unmerged {
            bin_width = bin_width.max(8); // "Unmerged"
            continue;
        }
        if an.binary {
            let w = 14 + decimal_width(an.added) + decimal_width(an.deleted);
            bin_width = bin_width.max(w);
            number_width = 3;
            continue;
        }
        max_change = max_change.max(change);
    }

    // `width` is `options->stat_width ? options->stat_width : 80` for a plain `--stat`.
    let mut width: i64 = 80;
    number_width = number_width.max(decimal_width(max_change as u32));
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    // `diff.statGraphWidth`/`--stat-graph-width` caps the graph column.
    if stat_graph_width > 0 && stat_graph_width < graph_width {
        graph_width = stat_graph_width;
    }
    // `diff.statNameWidth`/`--stat-name-width` caps the filename column.
    let mut name_width = if stat_name_width > 0 && stat_name_width < max_len {
        stat_name_width
    } else {
        max_len
    };
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = (width * 3 / 8 - number_width - 6).max(6);
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

    for (i, (d, an)) in pairs.iter().copied().enumerate() {
        let (added, deleted) = (an.added as i64, an.deleted as i64);
        // "scale" the filename: overlong names are truncated to "...<tail>".
        let full = &names[i];
        let (prefix, name): (&str, &[u8]) = if name_width < full.len() as i64 {
            let len = name_width - 3;
            let start = full.len() - len.max(0) as usize;
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

        push_str(out, " ");
        push_str(out, prefix);
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        push_str(out, " | ");

        if an.binary {
            push_str(out, &format!("{:>width$}", "Bin", width = number_width as usize));
            if added == 0 && deleted == 0 {
                out.push(b'\n');
                continue;
            }
            // `show_stats()` paints the two byte counts with the old/new colors.
            out.push(b' ');
            diff_color::paint(out, colors, diff_color::DiffSlot::Old, deleted.to_string().as_bytes());
            push_str(out, " -> ");
            diff_color::paint(out, colors, diff_color::DiffSlot::New, added.to_string().as_bytes());
            push_str(out, " bytes\n");
            continue;
        }
        if d.unmerged {
            push_str(out, &format!("{:>width$}", "Unmerged", width = number_width as usize));
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
        push_str(
            out,
            &format!("{:>width$}", added + deleted, width = number_width as usize),
        );
        if added + deleted != 0 {
            push_str(out, " ");
        }
        // `show_graph()`: each run is wrapped in its own color and emits nothing
        // at all when it is empty.
        if add > 0 {
            diff_color::paint(out, colors, diff_color::DiffSlot::New, &b"+".repeat(add as usize));
        }
        if del > 0 {
            diff_color::paint(out, colors, diff_color::DiffSlot::Old, &b"-".repeat(del as usize));
        }
        out.push(b'\n');
    }

    let (files, adds, dels) = stat_totals(pairs);
    stat_summary(out, files, adds, dels);
}

/// `builtin_diff()`'s two submodule branches (diff.c:3870): under `--submodule=log`
/// the pair renders as `show_submodule_diff_summary()`, and under `--submodule=diff`
/// as the shared header followed by the submodule's own diff.
///
/// The bytes are written already painted — git emits them through
/// `diff_emit_submodule_*()`, which never passes them to `fn_out_consume()`.
fn render_submodule(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    format: SubmoduleFormat,
    abbrev: usize,
    colors: &diff_color::DiffColors,
    r: &Render,
) {
    let null = r.hash_kind.null();
    // `p->one->oid` / `p->two->oid`: the null id stands for the side that is absent.
    let one = delta.old.map(|(id, _)| id).unwrap_or(null);
    let two = match delta.new {
        NewSide::Blob(id, _) => id,
        NewSide::Worktree(_) => delta.new_commit.unwrap_or(null),
        NewSide::Absent => null,
    };
    let path = delta.old_path();
    if format == SubmoduleFormat::Log {
        super::diff_pairs::show_submodule_diff_summary(
            out,
            repo,
            path,
            &one,
            &two,
            delta.dirty_submodule,
            abbrev,
            colors,
        );
        return;
    }
    let hdr = super::diff_pairs::show_submodule_header(
        out,
        repo,
        path,
        &one,
        &two,
        delta.dirty_submodule,
        abbrev,
    );
    // "We need a valid left and right commit to display a difference."
    if !(hdr.left.is_some() || one.is_null()) || !(hdr.right.is_some() || two.is_null()) {
        return;
    }
    match submodule_inline_diff(repo, path, &hdr, &one, &two, delta.dirty_submodule, colors, r) {
        Some(text) => out.extend_from_slice(&text),
        // `diff_emit_submodule_error()`: the child could not be started, or it
        // failed once it had.
        None => out.extend_from_slice(b"(diff failed)\n"),
    }
}

/// `show_submodule_inline_diff()`'s child process (submodule.c:654): a whole second
/// `diff --submodule=diff` run *inside* the submodule, with the gitlink path glued
/// onto both prefixes so every file it names is reachable from the superproject.
/// git spawns `git`; this spawns the running binary, which answers the same options.
#[allow(clippy::too_many_arguments)]
fn submodule_inline_diff(
    repo: &gix::Repository,
    path: &BString,
    hdr: &super::diff_pairs::SubmoduleHeader,
    one: &ObjectId,
    two: &ObjectId,
    dirty: u8,
    colors: &diff_color::DiffColors,
    r: &Render,
) -> Option<Vec<u8>> {
    let empty_tree = gix::ObjectId::empty_tree(r.hash_kind);
    let old_oid = if hdr.left.is_some() { *one } else { empty_tree };
    let new_oid = if hdr.right.is_some() { *two } else { empty_tree };

    let workdir = repo.workdir()?;
    let dir = workdir.join(gix::path::from_bstr(path.as_bstr()).as_ref());
    let exe = std::env::current_exe().ok()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("diff").arg("--submodule=diff");
    cmd.arg(format!(
        "--color={}",
        if colors.enabled() { "always" } else { "never" }
    ));
    // `-R` swaps which prefix each side is given; every other option keeps them.
    let (src, dst) = (&r.src_prefix, &r.dst_prefix);
    let prefix = |lead: &str, base: &[u8]| -> std::ffi::OsString {
        let mut v = lead.as_bytes().to_vec();
        v.extend_from_slice(base);
        v.extend_from_slice(path);
        v.push(b'/');
        gix::path::from_byte_slice(&v).as_os_str().to_owned()
    };
    cmd.arg(prefix("--src-prefix=", src));
    cmd.arg(prefix("--dst-prefix=", dst));
    cmd.arg(old_oid.to_hex().to_string());
    // "If the submodule has modified content, we will diff against the work tree" —
    // so the second revision is left off and the child compares against its own
    // worktree instead.
    if dirty & super::diff_pairs::DIRTY_SUBMODULE_MODIFIED == 0 {
        cmd.arg(new_oid.to_hex().to_string());
    }
    if !dir.is_dir() {
        // `prepare_submodule_repo_env()`'s fallback to an absorbed git dir.
        let sub = hdr.sub.as_ref()?;
        cmd.current_dir(sub.git_dir());
        cmd.env("GIT_DIR", ".").env("GIT_WORK_TREE", ".");
    } else {
        cmd.current_dir(&dir);
        // `prepare_submodule_repo_env()`: the superproject's repository variables
        // must not leak into the child.
        cmd.env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_PREFIX")
            .env_remove("GIT_COMMON_DIR");
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

/// `diff_flush_patch_quietly()` (diff.c:6566): render the pair with the output
/// thrown away and report only whether it had anything to say. `scratch` is the
/// caller's reusable buffer — nothing in it is ever printed.
///
/// A submodule pair reports a change without rendering: `builtin_diff()`'s two
/// submodule branches set `o->found_changes` before writing a single byte.
#[allow(clippy::too_many_arguments)]
fn pair_reports_change(
    scratch: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    an: &Analysis,
    ctx: u32,
    r: &Render,
    submodule_format: SubmoduleFormat,
) -> Result<bool> {
    if !delta.unmerged && submodule_format != SubmoduleFormat::Short && delta.is_submodule_pair() {
        return Ok(true);
    }
    scratch.clear();
    render_patch(scratch, repo, delta, an, ctx, r)?;
    Ok(!scratch.is_empty())
}

/// Render one delta as a `git diff` file section into `out`.
fn render_patch(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    an: &Analysis,
    ctx: u32,
    r: &Render,
) -> Result<()> {
    if delta.unmerged {
        return render_combined(out, repo, delta, ctx);
    }

    // The `index` line honors `--abbrev` / `--full-index`. `fill_metainfo()` also
    // widens it to the full name under `--binary`, but only for a pair that really
    // is binary — text pairs in the same run keep the normal abbreviation.
    let hlen = if r.full_index || (r.binary && an.binary) {
        r.hash_kind.len_in_hex()
    } else {
        r.abbrev
    };
    let null_hash = r.hash_kind.null().to_hex_with_len(hlen).to_string();
    let old_hash = delta
        .old
        .map(|(id, _)| id.to_hex_with_len(hlen).to_string())
        .unwrap_or_else(|| null_hash.clone());
    let new_hash = if matches!(delta.new, NewSide::Absent) {
        null_hash.clone()
    } else {
        an.new_id.to_hex_with_len(hlen).to_string()
    };
    let content_differs = old_hash != new_hash;
    let new_kind = delta.new_kind();

    // `builtin_diff()` builds the header into a strbuf and hands it to
    // `fn_out_consume()` (diff.c:2364), which emits it only when the first hunk line
    // goes out. `must_show_header` is what forces it out early: `fill_metainfo()`
    // (diff.c:4491) sets it for a copy, a rename and a `-B` rewrite, and
    // `builtin_diff()` itself for a creation (diff.c:3613), a deletion (diff.c:3620)
    // and a mode change (diff.c:3627). A plain modification whose comparison found
    // nothing — the usual result of `-w`/`-b` on a whitespace-only edit — therefore
    // prints no `diff --git` line at all.
    let mode_changed = matches!((delta.old, new_kind), (Some((_, ok)), Some(nk)) if ok != nk);
    let must_show = delta.old.is_none()
        || matches!(delta.new, NewSide::Absent)
        || delta.renamed()
        || delta.complete_rewrite()
        || mode_changed
        || an.hunks.is_some()
        // The binary arm prints `Binary files ... differ` and its header with it,
        // but only once the two sides are known to differ (diff.c:3672).
        || (an.binary && content_differs);
    if !must_show {
        return Ok(());
    }

    push_str(out, "diff --git ");
    out.extend_from_slice(&quote_two(&r.src_prefix, delta.old_path(), &r.dst_prefix, &delta.path));
    out.push(b'\n');

    // File-creation / deletion / mode-change lines.
    match (delta.old, new_kind) {
        (None, Some(nk)) => {
            push_str(out, "new file mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        (Some((_, ok)), None) => {
            push_str(out, "deleted file mode ");
            push_str(out, mode_str(ok));
            out.push(b'\n');
        }
        (Some((_, ok)), Some(nk)) if ok != nk => {
            push_str(out, "old mode ");
            push_str(out, mode_str(ok));
            push_str(out, "\nnew mode ");
            push_str(out, mode_str(nk));
            out.push(b'\n');
        }
        _ => {}
    }

    // `fill_metainfo()`: the rename/copy or dissimilarity header, emitted between the
    // mode lines and the `index` line.
    if delta.renamed() {
        let verb = if delta.status == b'C' { "copy" } else { "rename" };
        push_str(
            out,
            &format!(
                "similarity index {}%\n{verb} from ",
                diffcore_rename::similarity_index(delta.score)
            ),
        );
        out.extend_from_slice(&quoted_name(delta.old_path()));
        push_str(out, &format!("\n{verb} to "));
        out.extend_from_slice(&quoted_name(&delta.path));
        out.push(b'\n');
    } else if delta.complete_rewrite() {
        push_str(
            out,
            &format!(
                "dissimilarity index {}%\n",
                diffcore_rename::similarity_index(delta.score)
            ),
        );
    }

    // The `index <old>..<new>[ <mode>]` line only appears when content differs.
    if content_differs {
        push_str(out, "index ");
        push_str(out, &old_hash);
        push_str(out, "..");
        push_str(out, &new_hash);
        // Trailing mode only for an unchanged-mode modification (not add/delete/mode-change).
        if let (Some((_, ok)), Some(nk)) = (delta.old, new_kind) {
            if ok == nk {
                out.push(b' ');
                push_str(out, mode_str(nk));
            }
        }
        out.push(b'\n');
    }

    let old_label = if delta.old.is_some() {
        quote_one(&r.src_prefix, delta.old_path())
    } else {
        b"/dev/null".to_vec()
    };
    let new_label = if matches!(delta.new, NewSide::Absent) {
        b"/dev/null".to_vec()
    } else {
        quote_one(&r.dst_prefix, &delta.path)
    };

    if an.binary {
        match (r.binary, &an.images) {
            // `emit_binary_diff()`: no `---`/`+++` pair, just the payload.
            (true, Some((one, two))) => super::binary_patch::emit(
                out,
                one,
                two,
                super::binary_patch::loose_compression_level(repo),
            ),
            _ => {
                push_str(out, "Binary files ");
                out.extend_from_slice(&old_label);
                push_str(out, " and ");
                out.extend_from_slice(&new_label);
                push_str(out, " differ\n");
            }
        }
    } else if let Some(hunks) = &an.hunks {
        emit_file_line(out, b"--- ", &old_label);
        emit_file_line(out, b"+++ ", &new_label);
        out.extend_from_slice(hunks);
    }
    Ok(())
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

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// `diff.suppressBlankEmpty`: git's `fn_out_consume()` rewrites any single emitted
/// line that is exactly `" \n"` — an empty *context* line — to `"\n"` before it is
/// written (its check is `len == 2 && line[0] == ' ' && line[1] == '\n'`). Blank
/// added/removed lines (`"+\n"`/`"-\n"`) and a context line whose content is one
/// space (`"  \n"`, 3 bytes) never match, so a line-oriented pass that drops the
/// leading space of every standalone `" \n"` line reproduces it byte-for-byte.
fn apply_suppress_blank_empty(out: Vec<u8>, on: bool) -> Vec<u8> {
    if !on || out.is_empty() {
        return out;
    }
    let mut res = Vec::with_capacity(out.len());
    let mut line_start = 0;
    for i in 0..out.len() {
        if out[i] == b'\n' {
            let line = &out[line_start..=i];
            if line == b" \n" {
                res.push(b'\n');
            } else {
                res.extend_from_slice(line);
            }
            line_start = i + 1;
        }
    }
    // A trailing line without a terminator cannot be `" \n"`; copy it verbatim.
    res.extend_from_slice(&out[line_start..]);
    res
}

/// `--line-prefix`: git's `emit_line_0()` writes `diff_line_prefix(o)` before every
/// emitted line, so prepending `prefix` at the buffer start and after each interior
/// newline reproduces it byte-for-byte for the newline-terminated formats (patch,
/// stat, summary, raw/name without `-z`). An empty buffer stays empty (git emits
/// nothing at all on a clean tree), and a trailing newline is not followed by a
/// dangling prefixed empty line.
fn apply_line_prefix(out: Vec<u8>, prefix: &[u8]) -> Vec<u8> {
    if prefix.is_empty() || out.is_empty() {
        return out;
    }
    let mut res = Vec::with_capacity(out.len() + prefix.len() * 2);
    res.extend_from_slice(prefix);
    for (i, &b) in out.iter().enumerate() {
        res.push(b);
        if b == b'\n' && i + 1 < out.len() {
            res.extend_from_slice(prefix);
        }
    }
    res
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
fn quote_one(prefix: &[u8], path: &BString) -> Vec<u8> {
    let s = path.as_slice();
    if !needs_quote(prefix) && !needs_quote(s) {
        let mut out = prefix.to_vec();
        out.extend_from_slice(s);
        return out;
    }
    let mut out = vec![b'"'];
    cq_body(prefix, &mut out);
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// The `diff --git <a> <b>` name pair.
fn quote_two(pa: &[u8], a: &BString, pb: &[u8], b: &BString) -> Vec<u8> {
    let mut out = quote_one(pa, a);
    out.push(b' ');
    out.extend_from_slice(&quote_one(pb, b));
    out
}

// ---------------------------------------------------------------------------
// combined ("--cc") diff for unmerged worktree paths
// ---------------------------------------------------------------------------

/// One line that a parent had but the merge result does not.
struct LostLine {
    line: Vec<u8>,
    /// Bit `n` set means parent `n` lost this line.
    parent_map: u32,
}

/// One line of the merge result, plus everything the parents lost in front of it.
/// Mirrors `struct sline` in `combine-diff.c`.
#[derive(Default)]
struct SLine {
    /// The line content without its terminator. Empty for the two trailer slots.
    bol: Vec<u8>,
    lost: Vec<LostLine>,
    /// Lines lost by the parent currently being processed, before coalescing.
    plost: Vec<Vec<u8>>,
    /// Bits `0..num_parent` mark parents that lack this line; bit `num_parent`
    /// is `mark` and bit `num_parent + 1` is `no_pre_delete`.
    flag: u32,
    /// Per-parent line number this sline starts at, filled by `combine_diff()`.
    p_lno: [u32; NUM_PARENT],
}

const NUM_PARENT: usize = 2;

/// Build the two-parent combined-diff `sline` table: the merge result plus, for
/// each parent, the lines that parent lost — coalesced and numbered exactly as
/// `combine_diff()` / `make_hunks()` do. Returns the table and the result line
/// count. Shared by the unmerged-worktree (`--cc`) and multi-revision paths.
fn build_combined_sline(result: &[u8], parents: &[Vec<u8>], ctx: u32) -> (Vec<SLine>, usize) {
    // Result lines, terminators stripped; a trailing incomplete line still counts.
    let mut cnt = result.iter().filter(|b| **b == b'\n').count();
    if !result.is_empty() && *result.last().expect("non-empty") != b'\n' {
        cnt += 1;
    }
    let mut sline: Vec<SLine> = (0..cnt + 2).map(|_| SLine::default()).collect();
    for (i, line) in byte_lines(result).into_iter().enumerate() {
        let end = line.len() - usize::from(line.last() == Some(&b'\n'));
        sline[i].bol = line[..end].to_vec();
    }

    let result_lines = byte_lines(result);
    for (n, parent) in parents.iter().enumerate() {
        let nmask = 1u32 << n;
        let before = byte_lines(parent);
        let mut input: InternedInput<Vec<u8>> = InternedInput::default();
        input.update_before(before.iter().map(|l| l.to_vec()));
        input.update_after(result_lines.iter().map(|l| l.to_vec()));
        // `xdi_diff_outf()` runs with git's default algorithm.
        let diff = diff_with_slider_heuristics(gix::diff::blob::Algorithm::Myers, &input);

        for hunk in diff.hunks() {
            // Removals hang off the result line that follows them, which for both
            // an empty and a non-empty "after" range is `after.start`.
            let bucket = hunk.after.start as usize;
            for i in hunk.before.clone() {
                let line = before[i as usize];
                let end = line.len() - usize::from(line.last() == Some(&b'\n'));
                sline[bucket].plost.push(line[..end].to_vec());
            }
            for i in hunk.after.clone() {
                sline[i as usize].flag |= nmask;
            }
        }

        // Assign per-parent line numbers, coalescing this parent's lost lines in.
        let mut p_lno: u32 = 1;
        // `lno` is compared against `cnt` and the range is narrower than `sline`;
        // faithful port of combine-diff.c, not a plain slice iteration.
        #[allow(clippy::needless_range_loop)]
        for lno in 0..=cnt {
            sline[lno].p_lno[n] = p_lno;
            let fresh = std::mem::take(&mut sline[lno].plost);
            coalesce_lines(&mut sline[lno].lost, fresh, n as u32);
            for ll in &sline[lno].lost {
                if ll.parent_map & nmask != 0 {
                    p_lno += 1;
                }
            }
            if lno < cnt && sline[lno].flag & nmask == 0 {
                p_lno += 1;
            }
        }
        sline[cnt + 1].p_lno[n] = p_lno;
    }

    make_hunks(&mut sline, cnt, ctx);
    (sline, cnt)
}

/// `true` if any result line survived dense filtering, i.e. the combined diff has
/// at least one hunk to emit for this path.
fn sline_has_marks(sline: &[SLine], cnt: usize) -> bool {
    sline.iter().take(cnt + 1).any(|s| s.flag & MARK != 0)
}

/// The file path a tree-to-tree change touches, or `None` for a directory-level
/// (tree) change — gitoxide reports those too, and the combined diff only cares
/// about blob leaves.
fn change_blob_location(change: &gix::object::tree::diff::ChangeDetached) -> Option<BString> {
    use gix::object::tree::diff::ChangeDetached;
    match change {
        ChangeDetached::Addition { location, entry_mode, .. }
        | ChangeDetached::Deletion { location, entry_mode, .. } => {
            (!entry_mode.is_tree()).then(|| location.clone())
        }
        ChangeDetached::Modification {
            location,
            entry_mode,
            previous_entry_mode,
            ..
        } => (!entry_mode.is_tree() || !previous_entry_mode.is_tree()).then(|| location.clone()),
        // Rewrites are disabled on the options we pass, so this never fires.
        ChangeDetached::Rewrite { .. } => None,
    }
}

/// The blob at `path` in `tree`: its id, whether it exists, and its bytes (the id
/// is the null oid and the bytes are empty when the path is absent from the tree).
fn tree_blob(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    path: &BString,
) -> Result<(ObjectId, bool, Vec<u8>)> {
    match tree_entry(tree, path)? {
        Some((_, EntryKind::Commit)) => {
            bail!("submodule/gitlink change at {path:?} is not supported")
        }
        // A directory at this path contributes no blob content of its own.
        Some((_, EntryKind::Tree)) => Ok((repo.object_hash().null(), false, Vec::new())),
        Some((id, _)) => Ok((id, true, blob_bytes(repo, id)?)),
        None => Ok((repo.object_hash().null(), false, Vec::new())),
    }
}

/// `git diff <rev0> <rev1> [<rev2> ...]` with three or more revisions: a dense
/// combined ("--cc") diff of the first revision (the result) against every other
/// revision (its parents), mirroring `builtin_diff_combined()`. A path is shown
/// only when the result differs from every parent, exactly as dense combined-diff
/// filtering requires — so equal revisions produce no output at all.
fn combined_multi(
    repo: &gix::Repository,
    revs: &[String],
    paths: &[String],
    fmt: u32,
    ctx: u32,
    line_prefix: &[u8],
) -> Result<ExitCode> {
    // `-s` / `--no-patch` suppresses all output; the combined patch is the only
    // combined format zvcs renders, so every other format falls back to it.
    if fmt & F_NO_OUTPUT != 0 {
        return Ok(ExitCode::SUCCESS);
    }

    let result_tree = repo.rev_parse_single(revs[0].as_str())?.object()?.peel_to_tree()?;
    let mut parent_trees: Vec<gix::Tree<'_>> = Vec::with_capacity(revs.len() - 1);
    for r in &revs[1..] {
        parent_trees.push(repo.rev_parse_single(r.as_str())?.object()?.peel_to_tree()?);
    }

    let out = combined_trees_patch(repo, &result_tree, &parent_trees, paths, ctx)?;
    let out = apply_line_prefix(out, line_prefix);

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The paths a merge's *combined* pair list holds, with one status letter per
/// parent — what `--name-only`/`--name-status` report for a merge under `-c`
/// (`show_raw_diff()` on a `combine_diff_path`). A path is listed only when the
/// result differs from every parent, the same filter the combined patch uses; the
/// stat formats do not go through here, because git leaves those on the
/// first-parent diff.
pub(crate) fn merge_combined_names(
    repo: &gix::Repository,
    commit: ObjectId,
    parents: &[ObjectId],
    paths: &[String],
) -> Result<Vec<(BString, String)>> {
    let result_tree = repo.find_commit(commit)?.tree()?;
    let mut parent_trees: Vec<gix::Tree<'_>> = Vec::with_capacity(parents.len());
    for p in parents {
        parent_trees.push(repo.find_commit(*p)?.tree()?);
    }

    let mut cand: BTreeSet<BString> = BTreeSet::new();
    for pt in &parent_trees {
        for change in repo.diff_tree_to_tree(
            Some(pt),
            Some(&result_tree),
            Some(gix::diff::Options::default()),
        )? {
            cand.insert(change.location().to_owned());
        }
    }
    if !paths.is_empty() {
        let specs = super::log::PathspecMatcher::new(repo, paths)?;
        cand.retain(|p| specs.matches(p));
    }

    let mut out = Vec::new();
    for path in &cand {
        let (_, res_present, res_bytes) = tree_blob(repo, &result_tree, path)?;
        let mut letters = String::with_capacity(parent_trees.len());
        let mut same_as_a_parent = false;
        for pt in &parent_trees {
            let (_, p_present, p_bytes) = tree_blob(repo, pt, path)?;
            if p_bytes == res_bytes && p_present == res_present {
                same_as_a_parent = true;
            }
            letters.push(match (p_present, res_present) {
                (false, true) => 'A',
                (true, false) => 'D',
                _ => 'M',
            });
        }
        // Dense filtering: a path that matches any parent is one-sided and elided.
        if same_as_a_parent {
            continue;
        }
        out.push((path.clone(), letters));
    }
    Ok(out)
}

/// One merge commit's combined patch, as `git log -c`/`--cc` shows it: the commit's
/// own tree against every parent's, with the header flavour `dense` selects.
pub(crate) fn merge_combined_patch(
    repo: &gix::Repository,
    commit: ObjectId,
    parents: &[ObjectId],
    paths: &[String],
    ctx: u32,
    dense: bool,
) -> Result<Vec<u8>> {
    let result_tree = repo.find_commit(commit)?.tree()?;
    let mut parent_trees: Vec<gix::Tree<'_>> = Vec::with_capacity(parents.len());
    for p in parents {
        parent_trees.push(repo.find_commit(*p)?.tree()?);
    }
    combined_trees_patch_headed(repo, &result_tree, &parent_trees, paths, ctx, dense)
}

/// The dense combined diff (`diff --cc`) of `result_tree` against every parent
/// tree, returned as bytes. Shared by `git diff -c`/`--cc` and `git show` on a
/// merge commit. A path appears only where the result differs from *all*
/// parents (git's dense-combined elision).
pub(crate) fn combined_trees_patch(
    repo: &gix::Repository,
    result_tree: &gix::Tree<'_>,
    parent_trees: &[gix::Tree<'_>],
    paths: &[String],
    ctx: u32,
) -> Result<Vec<u8>> {
    combined_trees_patch_headed(repo, result_tree, parent_trees, paths, ctx, true)
}

/// The same, with the header flavour chosen: `diff --cc` for the dense form and
/// `diff --combined` for a bare `-c` (`show_combined_header()` prints whichever
/// `opt->flags.dense_combined_merges` selected).
pub(crate) fn combined_trees_patch_headed(
    repo: &gix::Repository,
    result_tree: &gix::Tree<'_>,
    parent_trees: &[gix::Tree<'_>],
    paths: &[String],
    ctx: u32,
    dense: bool,
) -> Result<Vec<u8>> {
    // Candidate paths: everything that differs between the result and any parent.
    let mut cand: BTreeSet<BString> = BTreeSet::new();
    for pt in parent_trees {
        let changes = repo.diff_tree_to_tree(
            Some(pt),
            Some(result_tree),
            Some(gix::diff::Options::default()),
        )?;
        for change in changes {
            if let Some(loc) = change_blob_location(&change) {
                cand.insert(loc);
            }
        }
    }
    if !paths.is_empty() {
        let specs = super::log::PathspecMatcher::new(repo, paths)?;
        cand.retain(|p| specs.matches(p));
    }

    let null = repo.object_hash().null();
    let mut out: Vec<u8> = Vec::new();
    for path in &cand {
        let (res_id, res_present, res_bytes) = tree_blob(repo, result_tree, path)?;
        let mut parent_ids: Vec<ObjectId> = Vec::with_capacity(parent_trees.len());
        let mut parent_bytes: Vec<Vec<u8>> = Vec::with_capacity(parent_trees.len());
        for pt in parent_trees {
            let (pid, _present, pbytes) = tree_blob(repo, pt, path)?;
            parent_ids.push(pid);
            parent_bytes.push(pbytes);
        }

        // Dense combined diff shows a path only when the result differs from all
        // parents; matching any parent makes the change one-sided and elided.
        if parent_bytes.contains(&res_bytes) {
            continue;
        }
        if parent_bytes.len() != NUM_PARENT {
            bail!("combined diff of more than two parents is not supported");
        }

        let (sline, cnt) = build_combined_sline(&res_bytes, &parent_bytes, ctx);
        if !sline_has_marks(&sline, cnt) {
            continue;
        }

        push_str(&mut out, if dense { "diff --cc " } else { "diff --combined " });
        out.extend_from_slice(&quoted_name(path));
        out.push(b'\n');
        push_str(&mut out, "index ");
        let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
        for (i, pid) in parent_ids.iter().enumerate() {
            if i != 0 {
                out.push(b',');
            }
            push_str(&mut out, &pid.to_hex_with_len(abbrev).to_string());
        }
        push_str(&mut out, "..");
        let res_short = if res_present { res_id } else { null };
        push_str(&mut out, &res_short.to_hex_with_len(abbrev).to_string());
        out.push(b'\n');
        emit_file_line(&mut out, b"--- ", &quote_one(b"a/", path));
        emit_file_line(&mut out, b"+++ ", &quote_one(b"b/", path));
        dump_sline(&mut out, &sline, cnt, ctx);
    }
    Ok(out)
}

/// A combined diff of the two conflict stages against the working-tree file, as
/// `show_combined_diff()` renders it for `git diff` on a conflicted path.
///
/// Port of `show_patch_diff()` / `combine_diff()` / `make_hunks()` / `dump_sline()`
/// from `combine-diff.c`, specialized to the two-parent (stage 2 / stage 3) case.
fn render_combined(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    delta: &Delta,
    ctx: u32,
) -> Result<()> {
    let Some((ours, theirs)) = delta.stages else {
        // No stage 2/3 pair to combine (e.g. `--cached`): git prints the notice.
        push_str(out, "* Unmerged path ");
        out.extend_from_slice(&delta.path);
        out.push(b'\n');
        return Ok(());
    };
    let workdir = match repo.workdir() {
        Some(w) => w,
        None => {
            push_str(out, "* Unmerged path ");
            out.extend_from_slice(&delta.path);
            out.push(b'\n');
            return Ok(());
        }
    };
    let result = std::fs::read(workdir.join(gix::path::from_bstr(delta.path.as_bstr())))?;
    let parents = vec![blob_bytes(repo, ours)?, blob_bytes(repo, theirs)?];
    let (sline, cnt) = build_combined_sline(&result, &parents, ctx);

    // ---- header (`show_combined_header()`) --------------------------------
    push_str(out, "diff --cc ");
    out.extend_from_slice(&quoted_name(&delta.path));
    out.push(b'\n');
    push_str(out, "index ");
    let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
    push_str(out, &ours.to_hex_with_len(abbrev).to_string());
    push_str(out, ",");
    push_str(out, &theirs.to_hex_with_len(abbrev).to_string());
    push_str(out, "..");
    // The result lives only in the worktree, so it has no object id.
    push_str(out, &repo.object_hash().null().to_hex_with_len(abbrev).to_string());
    out.push(b'\n');
    emit_file_line(out, b"--- ", &quote_one(b"a/", &delta.path));
    emit_file_line(out, b"+++ ", &quote_one(b"b/", &delta.path));

    dump_sline(out, &sline, cnt, ctx);
    Ok(())
}

fn blob_bytes(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    Ok(repo.find_object(id)?.detach().data)
}

/// `coalesce_lines()`: LCS-merge `fresh` (the lines parent `parent` lost) into the
/// already-merged `base`, so a line lost by several parents is shown once.
fn coalesce_lines(base: &mut Vec<LostLine>, fresh: Vec<Vec<u8>>, parent: u32) {
    if fresh.is_empty() {
        return;
    }
    if base.is_empty() {
        *base = fresh
            .into_iter()
            .map(|line| LostLine {
                line,
                parent_map: 1 << parent,
            })
            .collect();
        return;
    }
    let (n, m) = (base.len(), fresh.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    // 0 = BASE, 1 = NEW, 2 = MATCH — the same encoding `combine-diff.c` uses.
    let mut dir = vec![vec![0u8; m + 1]; n + 1];
    for d in dir.iter_mut() {
        d[0] = 0;
    }
    for cell in dir[0].iter_mut().skip(1) {
        *cell = 1;
    }
    for i in 1..=n {
        for j in 1..=m {
            if base[i - 1].line == fresh[j - 1] {
                lcs[i][j] = lcs[i - 1][j - 1] + 1;
                dir[i][j] = 2;
            } else if lcs[i][j - 1] >= lcs[i - 1][j] {
                lcs[i][j] = lcs[i][j - 1];
                dir[i][j] = 1;
            } else {
                lcs[i][j] = lcs[i - 1][j];
                dir[i][j] = 0;
            }
        }
    }
    let mut merged: Vec<LostLine> = Vec::with_capacity(n + m);
    let (mut i, mut j) = (n, m);
    while i != 0 || j != 0 {
        match dir[i][j] {
            2 => {
                let mut ll = std::mem::replace(
                    &mut base[i - 1],
                    LostLine {
                        line: Vec::new(),
                        parent_map: 0,
                    },
                );
                ll.parent_map |= 1 << parent;
                merged.push(ll);
                i -= 1;
                j -= 1;
            }
            1 => {
                merged.push(LostLine {
                    line: fresh[j - 1].clone(),
                    parent_map: 1 << parent,
                });
                j -= 1;
            }
            _ => {
                merged.push(std::mem::replace(
                    &mut base[i - 1],
                    LostLine {
                        line: Vec::new(),
                        parent_map: 0,
                    },
                ));
                i -= 1;
            }
        }
    }
    merged.reverse();
    *base = merged;
}

const ALL_MASK: u32 = (1 << NUM_PARENT) - 1;
const MARK: u32 = 1 << NUM_PARENT;
const NO_PRE_DELETE: u32 = 2 << NUM_PARENT;

fn interesting(sl: &SLine) -> bool {
    sl.flag & ALL_MASK != 0 || !sl.lost.is_empty()
}

/// `adjust_hunk_tail()`.
fn adjust_hunk_tail(sline: &[SLine], hunk_begin: usize, i: usize) -> usize {
    if hunk_begin < i && sline[i - 1].flag & ALL_MASK == 0 {
        i - 1
    } else {
        i
    }
}

/// `find_next()`.
fn find_next(sline: &[SLine], i: usize, cnt: usize, look_for_uninteresting: bool) -> usize {
    let mut i = i;
    while i <= cnt {
        let marked = sline[i].flag & MARK != 0;
        if look_for_uninteresting != marked {
            return i;
        }
        i += 1;
    }
    i
}

/// `give_context()`.
fn give_context(sline: &mut [SLine], cnt: usize, context: usize) {
    let mut i = find_next(sline, 0, cnt, false);
    if cnt < i {
        return;
    }
    while i <= cnt {
        let mut j = i.saturating_sub(context);
        while j < i {
            if sline[j].flag & MARK == 0 {
                sline[j].flag |= NO_PRE_DELETE;
            }
            sline[j].flag |= MARK;
            j += 1;
        }
        loop {
            let mut j = find_next(sline, i, cnt, true);
            if cnt < j {
                return;
            }
            let k = find_next(sline, j, cnt, false);
            j = adjust_hunk_tail(sline, i, j);
            if k < j + context {
                while j < k {
                    sline[j].flag |= MARK;
                    j += 1;
                }
                i = k;
                continue;
            }
            i = k;
            let mut j2 = j;
            let end = (j + context).min(cnt + 1);
            while j2 < end {
                sline[j2].flag |= MARK;
                j2 += 1;
            }
            break;
        }
    }
}

/// `make_hunks()` with `dense` set, which is what `--cc` uses.
fn make_hunks(sline: &mut [SLine], cnt: usize, context: u32) {
    let context = context as usize;
    for sl in sline.iter_mut().take(cnt + 1) {
        if interesting(sl) {
            sl.flag |= MARK;
        } else {
            sl.flag &= !MARK;
        }
    }

    // Drop hunks whose every line differs from the same single set of parents:
    // those are changes only one side made, which `--cc` elides.
    let mut i = 0usize;
    while i <= cnt {
        while i <= cnt && sline[i].flag & MARK == 0 {
            i += 1;
        }
        if cnt < i {
            break;
        }
        let hunk_begin = i;
        let mut j = i + 1;
        while j <= cnt {
            if sline[j].flag & MARK == 0 {
                // Look past the gap: another marked line within `context` continues it.
                let mut la = adjust_hunk_tail(sline, hunk_begin, j);
                la = (la + context).min(cnt + 1);
                let mut contin = false;
                while la > 0 && j < la {
                    la -= 1;
                    if sline[la].flag & MARK != 0 {
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

        let mut same_diff: u32 = 0;
        let mut has_interesting = false;
        for sl in sline.iter().take(hunk_end).skip(i) {
            if has_interesting {
                break;
            }
            let this_diff = sl.flag & ALL_MASK;
            if this_diff != 0 {
                if same_diff == 0 {
                    same_diff = this_diff;
                } else if same_diff != this_diff {
                    has_interesting = true;
                    break;
                }
            }
            for ll in &sl.lost {
                if has_interesting {
                    break;
                }
                if same_diff == 0 {
                    same_diff = ll.parent_map;
                } else if same_diff != ll.parent_map {
                    has_interesting = true;
                }
            }
        }

        if !has_interesting && same_diff != ALL_MASK {
            for sl in sline.iter_mut().take(hunk_end).skip(hunk_begin) {
                sl.flag &= !MARK;
            }
        }
        i = hunk_end;
    }

    give_context(sline, cnt, context);
}

/// `dump_sline()`.
fn dump_sline(out: &mut Vec<u8>, sline: &[SLine], cnt: usize, context: u32) {
    let mut lno = 0usize;
    loop {
        while lno <= cnt && sline[lno].flag & MARK == 0 {
            lno += 1;
        }
        if cnt < lno {
            break;
        }
        let mut hunk_end = lno + 1;
        while hunk_end <= cnt && sline[hunk_end].flag & MARK != 0 {
            hunk_end += 1;
        }
        let mut rlines = hunk_end - lno;
        if cnt < hunk_end {
            rlines -= 1; // pointing at the last delete hunk
        }
        let mut null_context = 0usize;
        if context == 0 {
            for sl in sline.iter().take(hunk_end).skip(lno) {
                if sl.flag & (MARK - 1) == 0 {
                    null_context += 1;
                }
            }
            rlines = rlines.saturating_sub(null_context);
        }

        out.extend_from_slice(&b"@".repeat(NUM_PARENT + 1));
        for n in 0..NUM_PARENT {
            let l0 = sline[lno].p_lno[n];
            let l1 = sline[hunk_end].p_lno[n];
            push_str(
                out,
                &format!(" -{l0},{}", l1 as i64 - l0 as i64 - null_context as i64),
            );
        }
        push_str(out, &format!(" +{},{rlines} ", lno + 1));
        out.extend_from_slice(&b"@".repeat(NUM_PARENT + 1));
        out.push(b'\n');

        while lno < hunk_end {
            let sl = &sline[lno];
            lno += 1;
            if sl.flag & NO_PRE_DELETE == 0 {
                for ll in &sl.lost {
                    for n in 0..NUM_PARENT {
                        out.push(if ll.parent_map & (1 << n) != 0 { b'-' } else { b' ' });
                    }
                    out.extend_from_slice(&ll.line);
                    out.push(b'\n');
                }
            }
            if cnt < lno {
                break;
            }
            if sl.flag & (MARK - 1) == 0 && context == 0 {
                // Only there to hang lost lines in front of; not shown at -U0.
                continue;
            }
            for n in 0..NUM_PARENT {
                out.push(if sl.flag & (1 << n) != 0 { b'+' } else { b' ' });
            }
            out.extend_from_slice(&sl.bol);
            out.push(b'\n');
        }
    }
}

// ---------------------------------------------------------------------------
// unified-diff hunk sink
// ---------------------------------------------------------------------------

/// Format one side of a hunk header (`@@ -<here> +<here> @@`), omitting the length when
/// it is 1 and using the pre-hunk line number when it is 0, exactly like `git diff`.
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
struct PatchSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
    /// git's `funclineprev`: the line the previous hunk's search started from, and
    /// the limit for the next one, so a heading is never scanned for twice.
    func_prev: i64,
    /// git's `func_line`, which lives across the whole file rather than one hunk.
    /// A search that finds nothing leaves it alone, so a hunk deep inside a long
    /// function keeps the heading found for the hunk above it — the search window
    /// stops at the previous hunk precisely because that answer is still good.
    func_text: Vec<u8>,
}

/// git's `def_ff` (xdiff/xemit.c): the default answer to "does this line begin a
/// section?" when no `diff=<driver>` attribute supplies a `funcname` pattern.
///
/// A line qualifies when its first byte is an ASCII letter, `_`, or `$` — the
/// column-zero convention that C, shell, Rust and most other languages follow for
/// top-level definitions. The text is clipped to `sz` FIRST and only then stripped
/// of trailing whitespace, which is the order git uses; doing it the other way
/// around would keep a different byte count for a long line.
pub(crate) fn def_ff(rec: &[u8], sz: usize) -> Option<&[u8]> {
    let first = *rec.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let mut len = rec.len().min(sz);
    // C's `isspace` in the default locale, which includes the vertical tab that
    // Rust's `is_ascii_whitespace` leaves out.
    while len > 0 && matches!(rec[len - 1], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        len -= 1;
    }
    Some(&rec[..len])
}

/// The longest section heading git will keep: `struct func_line { char buf[80]; }`.
const FUNC_LINE_MAX: usize = 80;
/// git formats a hunk header into a 128-byte buffer and clips the heading to
/// whatever is left in it, so a long line number range shortens the heading.
pub(crate) const HUNK_HDR_MAX: usize = 128;

/// git's `get_func_line`: walk the pre-image from `start` toward `limit`
/// looking for a line that reads as a section heading. The direction follows
/// the endpoints — backward for the normal case where the previous hunk sits
/// above this one — and `limit` itself is never examined.
pub(crate) fn func_line<'a>(before: &[&'a [u8]], start: i64, limit: i64) -> Option<&'a [u8]> {
    let step: i64 = if start > limit { -1 } else { 1 };
    let mut l = start;
    while l != limit && l >= 0 && (l as usize) < before.len() {
        if let Some(text) = def_ff(before[l as usize], FUNC_LINE_MAX) {
            return Some(text);
        }
        l += step;
    }
    None
}

impl PatchSink<'_> {
    fn func_line(&self, start: i64, limit: i64) -> Option<&[u8]> {
        func_line(self.before, start, limit)
    }
}

impl ConsumeHunk for PatchSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        let mut hdr: Vec<u8> = Vec::with_capacity(HUNK_HDR_MAX);
        hdr.extend_from_slice(b"@@ -");
        hdr.extend_from_slice(fmt_range(header.before_hunk_start, header.before_hunk_len).as_bytes());
        hdr.extend_from_slice(b" +");
        hdr.extend_from_slice(fmt_range(header.after_hunk_start, header.after_hunk_len).as_bytes());
        hdr.extend_from_slice(b" @@");

        // The section heading: the nearest qualifying line at or above the hunk's
        // first pre-image line, searched no further back than the previous hunk's
        // own starting point. `before_hunk_start` is 1-based and git's `s1` is the
        // 0-based index of that same line.
        let s1 = header.before_hunk_start as i64 - 1;
        if let Some(func) = self.func_line(s1 - 1, self.func_prev) {
            self.func_text = func.to_vec();
        }
        self.func_prev = s1 - 1;
        if !self.func_text.is_empty() {
            // git clips the heading to what remains of its 128-byte header
            // buffer, reserving one byte for the newline.
            let room = HUNK_HDR_MAX.saturating_sub(hdr.len() + 2);
            hdr.push(b' ');
            hdr.extend_from_slice(&self.func_text[..self.func_text.len().min(room)]);
        }
        hdr.push(b'\n');
        self.buf.extend_from_slice(&hdr);

        let mut bi = header.before_hunk_start.saturating_sub(1) as usize;
        let mut ai = header.after_hunk_start.saturating_sub(1) as usize;
        for (kind, fallback) in lines {
            let (marker, content): (u8, &[u8]) = match kind {
                // `xdl_emit_diff()` emits every context record from `xe->xdf2`, the
                // *post-image* — all three context loops (pre, inter-change and post)
                // call `xdl_emit_record(&xe->xdf2, s2, " ", ecb)`. It only matters when
                // the two sides hold different bytes for a record the comparison called
                // equal, which is exactly what `-w`/`-b`/`--ignore-space-at-eol` do.
                DiffLineKind::Context => {
                    let c = self.after.get(ai).copied().unwrap_or(*fallback);
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
            // Tokens keep their line terminator; a token without one is the last line
            // of a file that lacks a trailing newline.
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

/// `checkdiff_consume()` (diff.c): report every added line of `delta` that
/// breaks a whitespace rule, and say whether any did.
///
/// The hunk text the analysis already produced is what git walks: each `@@`
/// header resets the new-file line counter, and every `+` line is checked and,
/// when it fails, printed under a `<path>:<line>: <problems>.` header. A blank
/// line inside the run the change lengthened at end-of-file additionally trips
/// `blank-at-eof`, which is why the analysis carries that boundary.
fn report_whitespace(delta: &Delta, analysis: &Analysis, ws_rule: u32) -> bool {
    let Some(hunks) = &analysis.hunks else {
        return false;
    };
    let mut found = false;
    let mut lineno = 0usize;
    for line in hunks.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b"@@") {
            // `@@ -a,b +c,d @@` — the new-side start, minus one so the first
            // line of the hunk lands on `c`.
            lineno = new_hunk_start(line).saturating_sub(1);
            continue;
        }
        match line.first() {
            Some(b' ') => lineno += 1,
            Some(b'+') => {
                lineno += 1;
                let body = &line[1..];
                let bad = super::diff_color::ws_check(body, ws_rule);
                if bad == 0 {
                    continue;
                }
                found = true;
                let mut stdout = std::io::stdout().lock();
                let _ = writeln!(
                    stdout,
                    "{}:{lineno}: {}.",
                    delta.path,
                    super::diff_color::whitespace_error_string(bad)
                );
                let _ = stdout.write_all(line);
                if !line.ends_with(b"\n") {
                    let _ = stdout.write_all(b"\n");
                }
            }
            _ => {}
        }
    }

    // `diff_flush_checkdiff` reports `blank-at-eof` once per file rather than per
    // line, naming where the lengthened run of blank lines starts and echoing
    // nothing — it is a property of the file, not of any one added line.
    let (blank_at_eof, _) = analysis.blank_at_eof;
    if ws_rule & super::diff_color::WS_BLANK_AT_EOF != 0 && blank_at_eof != 0 {
        found = true;
        println!(
            "{}:{blank_at_eof}: {}.",
            delta.path,
            super::diff_color::whitespace_error_string(super::diff_color::WS_BLANK_AT_EOF)
        );
    }
    found
}

/// The `+<start>` field of an `@@ -a,b +c,d @@` header.
fn new_hunk_start(header: &[u8]) -> usize {
    let Some(plus) = header.iter().position(|&b| b == b'+') else {
        return 1;
    };
    let digits: Vec<u8> = header[plus + 1..]
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    std::str::from_utf8(&digits)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}
