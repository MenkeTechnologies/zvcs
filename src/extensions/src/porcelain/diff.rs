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
//! Discovery coming up empty is *also* answered there. `cmd_diff()` sets up
//! gently and, finding no repository, sets `DIFF_NO_INDEX_IMPLICIT` rather than
//! dying — `git diff` outside a working tree is git's "colourful `diff(1)`", not
//! an error — so the two operands are compared if there are two, and if there
//! are not the command says so in a shape no other verb has: a `warning:`
//! pointing at `--no-index`, the `--no-index` usage block, and exit 129 rather
//! than the `fatal:` / 128 every command that needs a repository dies with.
//!
//! Beyond the format selectors, these options are honored: `-R` (reverse — the two
//! filespecs are swapped as `diff_change()` swaps them, before diffcore and every
//! format sees the pair, so a worktree diff reverses too: the file becomes the
//! pre-image, the blob platform's worktree root moves to that side, and the
//! `index` line names it by the hash of what is on disk while `--raw` keeps
//! printing all-zero for a side that has no id of its own), `-z`, `--full-index`,
//! `--abbrev[=<n>]`,
//! `--no-prefix`/`--default-prefix`/`--src-prefix=`/`--dst-prefix=`/`--line-prefix=`,
//! `--summary`, `--compact-summary`/`--no-compact-summary`, `--diff-filter=<...>`,
//! `--color[=always|auto|never]`/`--no-color` and `--ws-error-highlight=<kind>` (the
//! patch and the stat graph are painted from the `color.diff.*` slots, with git's
//! `ws.c` whitespace-error markup driven by `core.whitespace`),
//! `--patch-with-raw`, `--patch-with-stat`, `--exit-code`, `--quiet`,
//! `--minimal`/`--patience`/`--histogram`/`--diff-algorithm=<v>` (both the `=<v>`
//! and separated forms, `<v>` matched case-insensitively against `myers`,
//! `default`, `minimal`, `patience` and `histogram`), `--find-object=<object-id>`
//! (repeatable; `diffcore_pickaxe()`'s objfind kind, compared against the recorded
//! ids so an id the repository lacks simply matches nothing),
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
//! emitted),
//! `-I<regex>`/`--ignore-matching-lines=<regex>` and `--ignore-blank-lines` (both
//! spellings of the value for `-I`, and both marking a change ignorable through the
//! same `xdl_emit_diff` port),
//! `--inter-hunk-context=<n>` (`xecfg.interhunkctxlen`, again through that port),
//! `-a`/`--text`/`--no-text` (a textual patch for content the pipeline classifies as
//! binary, while `builtin_diffstat()` — which never reads the flag — keeps reporting
//! `Bin <a> -> <b> bytes`),
//! `-D`/`--irreversible-delete` (a deletion stops at its header),
//! `--skip-to=<path>`/`--rotate-to=<path>` (`diffcore_rotate()`, with
//! `cmd_diff()`'s `rotate_to_strict` making a target that names no queued pair
//! `fatal: No such path '<p>' in the diff` at 128, though only for a non-empty queue),
//! `--output=<file>` (opened and truncated during the option scan, as `xfopen` does,
//! so an unopenable path is fatal before anything else on the line is judged), and
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
//! * A pair whose two sides differ in `S_IFMT` — a regular file that became a
//!   symlink, a blob that became a gitlink — renders as a deletion section followed
//!   by a creation section, which is what `run_diff()` (diff.c:5052) does. The stat,
//!   raw, name and summary formats are handed the unsplit pair, so they still show
//!   one row, one `T` record and one `mode change` line.
//! * A tracked path whose name a *directory* has since taken follows
//!   `check_removed()` (diff-lib.c:22): a plain directory makes it an ordinary
//!   deletion, and a directory that is a repository makes it a `100644 => 160000`
//!   type change, `resolve_gitlink_ref()` being `gix::open()` + `head_id()`. The
//!   mixed blob/gitlink pair a type change produces is split for the patch and left
//!   whole for the diffstat, which counts the blob's lines against the one-line image
//!   `diff_populate_filespec()` (diff.c:4110) synthesises for the gitlink.
//! * `--ignore-submodules[=<when>]` is accepted and inert: gitlink changes are
//!   reported whatever it says, apart from the untracked files every diff ignores.
//! * `-c diff.submodule=<bad value>` warns once. Stock git repeats the warning when
//!   the value arrives through `-c` (measured: two lines from
//!   `git -c diff.submodule=bogus diff`, one from the same key in a config file);
//!   zvcs prints one either way, and matches stock byte for byte for the file case.
//! * `--line-prefix=<s>` is reproduced by a whole-buffer pass and so only tracks the
//!   newline-terminated formats; combining it with `-z` (NUL-separated records) is
//!   not byte-faithful.
//! * All four of `parse_algorithm_value()`'s names drive the vendored xdiff port in
//!   `src/ported/gix-imara-diff` (`patience.rs` is `xdiff/xpatience.c`,
//!   `histogram.rs` is `xdiff/xhistogram.c`). Measured against stock 2.55.0 over the
//!   59 most recent consecutive commit pairs of this repository: `myers` 0/59
//!   divergent, `minimal` 0/59, `patience` 0/59, `histogram` 0/59. Getting the last
//!   two there took four fixes, all in the shared engine — so `diff-index`,
//!   `diff-files` and `format-patch` carry them identically: `xdl_trim_ends()` is
//!   now skipped for patience and histogram the way `xdl_prepare_env()` skips
//!   `xdl_optimize_ctxs()` for them (`xprepare.c:460-462`); histogram now runs the
//!   full port of `xdl_change_compact()` (`Diff::compact_with`), which includes the
//!   histogram-only re-diff of a merged group at `xdiffi.c:940-958` that the
//!   slide-only `postprocess::postprocess_with` has no step for; histogram's own
//!   Myers fall-back re-enters `xdl_do_diff()` (`xhistogram.c:229-239`) instead of
//!   calling `myers::diff` bare, so the region gets trimmed and its records cleaned
//!   the way the sub-diff's own `xdl_prepare_env()` would; and `try_lcs()`'s
//!   occurrence skip is bounded by the before-side end of the region, `ae`
//!   (`xhistogram.c:207-217`), not by the after-side one.
//!   `--no-index` reads the same four names: `diff_no_index()` builds its option
//!   table with `add_diff_options()` (diff-no-index.c:372), so every spelling
//!   `git diff` takes — `--diff-algorithm=<v>`, the separated `--diff-algorithm
//!   <v>`, `--minimal`, `--patience`, `--histogram` — and the `diff.algorithm`
//!   default reach it too. Its own default is Myers, git's zero-valued
//!   `diff_algorithm` (diff.c:78).
//! * `-S<string>` and `-G<regex>` filter the queue at `diffcore_pickaxe()`'s place
//!   in `diffcore_std()`, sharing [`super::diff_pickaxe`] with `diff-index` and
//!   `diff-files`. Both spellings of the value are taken (`-Sfoo` and `-S foo`),
//!   `--pickaxe-regex` promotes `-S` to a regex, and `--pickaxe-all` keeps the
//!   whole queue once one pair matched. What is *not* shared is the regex engine:
//!   a pattern that will not compile gets `fatal: invalid regex: <message>` at 128
//!   as git does, but the message after the colon is the `regex` crate's rather
//!   than the platform `regcomp`'s.
//! * `-I<regex>` / `--ignore-matching-lines=<regex>` and `--ignore-blank-lines` mark a
//!   change ignorable exactly as `xdl_mark_ignorable_regex` and
//!   `xdl_mark_ignorable_lines` do, through the `xdl_emit_diff` port in
//!   [`super::diff_pairs::emit_unified`] — so an isolated one leaves the counts as well
//!   as the patch. Only `-I` raises `diff_from_contents` (diff.c:4899), which is what
//!   also drops the pair from `--raw`/`--name-status` and leaves
//!   `diff_fill_oid_info()`'s real hash on a worktree side's raw record;
//!   `--ignore-blank-lines` is deliberately not on that list and keeps both.
//!   The regex engine is the `regex` crate rather than the platform `regcomp`, so
//!   which *patterns compile* can differ even though the message
//!   (`error: invalid regex given to -I: '<pat>'`, 129) does not.
//! * Userdiff drivers are honoured: the driver a path's `diff` gitattribute names is
//!   resolved by [`resolve_drivers`] out of git's built-in table
//!   ([`crate::userdiff`]) overlaid with `diff.<name>.*`, and three of its fields
//!   reach the output.
//!   - `funcname`/`xfuncname` (and every built-in driver's own pattern) supply the
//!     `@@ … @@ <section>` heading, in place of xdiff's `def_ff` heuristic. The same
//!     pattern also drives `-L :<funcname>` for `blame` and `log`.
//!   - `textconv` converts each side before xdiff ([`apply_textconv`]); the counts
//!     `--stat`/`--numstat` print still come from the unconverted images, because
//!     `builtin_diffstat()` never calls `fill_textconv()`. `--no-textconv` turns it
//!     off, and `diff.<driver>.cachetextconv` memoises each oid-valid side's converted
//!     bytes in the `refs/notes/textconv/<driver>` cache
//!     ([`super::notes::Cache`]), exactly as `notes_cache_get()`/`notes_cache_put()`
//!     do — a stale converter command invalidates the whole cache, and a worktree
//!     side, having no valid id, is converted afresh every time.
//!   - `command`, plus `GIT_EXTERNAL_DIFF` and `diff.external`, replace the whole
//!     section with the program's stdout, through the `run_external_diff()` port in
//!     [`super::diff_pairs`] that `diff-files`, `diff-index` and `diff-pairs` share.
//!     `git diff` allows it by default and `--no-ext-diff` refuses it; `log`/`show`
//!     leave `flags.allow_external` down until `--ext-diff` raises it.
//! * Magic pathspecs (`:(...)`) and glob pathspecs bail; literal path / directory-prefix
//!   filtering is supported.
//! * `--anchored=<text>` is implemented: the anchor prefixes reach
//!   `xdl_do_patience_diff()`'s `is_anchor()` (xdiff/xpatience.c:71-79) through
//!   `gix_imara_diff::Diff::compute_with_anchors`, which the ported
//!   `src/ported/gix-imara-diff/src/patience.rs` grew for it. Like git's, the option
//!   pins the algorithm to patience and is repeatable, and a later `--patience`
//!   discards every anchor named before it. The plumbing verbs (`diff-files`,
//!   `diff-index`, `diff-tree`) still refuse it.
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
use super::diffstat::{self, StatWidths};
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
    /// `-D`/`--irreversible-delete` (`o->flags.irreversible_delete`): `builtin_diff()`
    /// (diff.c:3596) emits a deletion.s header and jumps to the end, so the pair loses
    /// its `---`/`+++` pair and its hunks. No other format reads the flag.
    irreversible_delete: bool,
    /// `-a`/`--text` (`o->flags.text`): `builtin_diff()`.s binary arm is guarded by
    /// `!o->flags.text`, so with this on a binary pair gets its patch rather than
    /// `Binary files ... differ` — even beside `--binary`, whose payload lives inside
    /// the arm this skips.
    text: bool,
    /// `-z`: terminate `--raw`/`--name-only`/`--name-status` records with NUL and
    /// suppress path C-quoting.
    z: bool,
    /// The `a/` (source) path prefix; `b/` under `-R`, empty under `--no-prefix`.
    src_prefix: Vec<u8>,
    /// The `b/` (destination) path prefix.
    dst_prefix: Vec<u8>,
    /// `o->output_indicators[]` (diff.h:381) — the added, removed and context sign
    /// bytes `--output-indicator-new`/`-old`/`-context` replace. `diff.c` applies
    /// them in `emit_line_ws_markup()` (diff.c:1369), i.e. at emit time only: the
    /// stored hunk text keeps git's canonical `+`/`-`/` ` so `--check`, the pickaxe
    /// and the diffstat all keep reading a real unified diff.
    indicators: (u8, u8, u8),
    hash_kind: gix::hash::Kind,
}

/// The `xdiff` knobs that decide which changes make it into the hunk stream at all,
/// as opposed to how wide the context around them is.
///
/// All four reach the same place: `builtin_diff()` fills `xpp.flags`,
/// `xpp.ignore_regex` and `xecfg.interhunkctxlen` from `diff_options` and hands them
/// to `xdl_diff()`. `builtin_diffstat()` fills the same three, which is why they
/// change `--stat`/`--numstat` as well as the patch. `DIFF_OPT_TEXT` is the one that
/// does not: the diffstat asks `diff_filespec_is_binary()` directly and never sees
/// it.
#[derive(Default)]
struct IgnoreOpts {
    /// `--ignore-blank-lines`: `XDF_IGNORE_BLANK_LINES`, which
    /// `xdl_mark_ignorable_lines()` turns into an `ignore` bit on an all-blank change.
    blank_lines: bool,
    /// `-I<re>` / `--ignore-matching-lines=<re>`: `xpp.ignore_regex`, which
    /// `xdl_mark_ignorable_regex()` turns into the same bit.
    lines: Vec<super::diff_pickaxe::Needle>,
    /// `--inter-hunk-context=<n>`: `xecfg.interhunkctxlen`, the gap two changes may
    /// span and still share one hunk.
    inter_hunk_ctx: usize,
    /// `-a`/`--text`: `DIFF_OPT_TEXT`, which diffs content git would otherwise
    /// report as `Binary files ... differ`.
    text: bool,
}

/// `diff_opt_output`'s `xfopen(arg, "w")`: create or truncate the file the whole diff
/// stream is written to. The failure is git's `xfopen` `die()`, which carries the
/// C-library reason and exits 128.
pub(crate) fn open_output_file(path: &str) -> std::result::Result<std::fs::File, ExitCode> {
    std::fs::File::create(path).map_err(|e| {
        eprintln!("fatal: could not open '{path}' for writing: {}", super::diff_pairs::io_reason(&e));
        ExitCode::from(128)
    })
}

/// `--inter-hunk-context=<n>`'s value through the shared `parse-options` integer
/// grammar. git declares it `OPT_UNSIGNED`, so base 0, an optional `+`, one optional
/// `k`/`m`/`g` suffix and a C `int`'s worth of range — which is what makes the
/// out-of-range message read `[0,4294967295]`. Measured against 2.55.0:
/// `--inter-hunk-context=` is ``option `inter-hunk-context' expects a numerical
/// value``, `=bad` and `=-1` are the `non-negative integer ... k/m/g suffix` wording,
/// and `= 4` (leading space) is accepted, all at 129.
pub(crate) fn parse_inter_hunk_context(v: &str) -> std::result::Result<usize, String> {
    crate::optint::unsigned_prec(&crate::optint::long_opt("inter-hunk-context"), v, 4)
        .map(|n| n as usize)
        .map_err(|e| e.message().to_owned())
}

impl IgnoreOpts {
    /// Whether any change can come out marked ignorable, which is what forces the
    /// `xdl_emit_diff` port — the counts then have to come from the emitted records
    /// rather than from the change script.
    fn marks_changes(&self) -> bool {
        self.blank_lines || !self.lines.is_empty()
    }
}

/// git's `enum diff_submodule_format` (diff.h), selected by `--submodule[=<format>]`
/// and `diff.submodule`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmoduleFormat {
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

/// Diff options that `git log`, `git show` and `git whatchanged` accept and that
/// cannot change a byte of what this port prints, because each one sets a
/// `diff_options` field to the value these commands already run at.
///
/// This is not a "we have not got round to it" list — every entry is a *negative*
/// or default-valued spelling whose callback, read in git 2.55.0, writes the state
/// this port is permanently in:
///
/// * `--ita-visible-in-index` / `--ita-invisible-in-index` — `flags.ita_invisible_in_index`
///   is read only by the index-vs-worktree and index-vs-tree walks (diff-lib.c);
///   `log`/`show`/`whatchanged` diff two trees and never see an intent-to-add entry.
///
/// Confirmed by measurement as well as by reading: for each entry, stock 2.55.0's
/// output is byte-identical with and without it across `-p`, `-p --stat`, `--raw`,
/// `--numstat`, `--shortstat`, `--summary`, `--name-status` and `-p -M -C` on
/// `log`, `show` and `whatchanged`, over a fixture carrying a rename, an empty-file
/// rename, a symlink, an exec-bit flip and a tab in a pathname.
///
/// The negations that *used* to live here — `--no-compact-summary`,
/// `--no-color-moved`, `--color-moved=no`, `--no-color-moved-ws`, `--word-diff=none`,
/// `--no-relative`, `--no-textconv` and `--no-ext-diff` — have moved out because
/// their positive spellings are now ported: each is handled by the option parser that
/// owns the flag it clears, so `--compact-summary --no-compact-summary` really does
/// undo the first flag instead of being swallowed twice.
pub(crate) fn history_noop_diff_option(a: &str) -> bool {
    matches!(a, "--ita-visible-in-index" | "--ita-invisible-in-index")
}

/// `parse_submodule_params()` (diff.c:194): the three format names, or `None` for
/// the value git refuses.
pub(crate) fn parse_submodule_params(value: &str) -> Option<SubmoduleFormat> {
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
    /// `!p->one->oid_valid`: the pre-image is the file in the worktree, not an
    /// object, so its id is the null one and its content is read by path. Only a
    /// reversed worktree diff (`-R`) produces one — everywhere else the pre-image
    /// comes out of a tree or the index.
    old_worktree: bool,
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
    /// The id `--raw` prints for a worktree-backed pre-image: git's `p->one->oid`,
    /// which `run_diff_files()` leaves null for a worktree side unless the submodule
    /// sits exactly where the index says. Only a reversed worktree diff sets it.
    old_raw_id: Option<ObjectId>,
    /// git's `p->one->dirty_submodule`, which only a reversed worktree diff sets:
    /// `diff_change()` swaps the two dirty flags along with the filespecs, so the
    /// `-dirty` marker moves to the pre-image with the worktree side it describes.
    old_dirty_submodule: u8,
    /// git's `p->two->dirty_submodule`: the `DIRTY_SUBMODULE_*` bits describing what
    /// the submodule worktree holds beyond its recorded commit. Always zero for a
    /// pair whose post-image is an object.
    dirty_submodule: u8,
    /// The commit a *worktree* gitlink post-image stands for: the one the submodule
    /// currently has checked out, which `run_diff_files()` writes into `p->two->oid`
    /// while leaving the filespec invalid. `None` for every other pair.
    new_commit: Option<ObjectId>,
    /// The userdiff drivers this pair's two filespec paths select, resolved once by
    /// [`resolve_drivers`] after the queue is built.
    drivers: PairDrivers,
    /// `fill_textconv()`'s two images, present only when one of the pair's drivers
    /// configures `diff.<name>.textconv`. Filled by [`apply_textconv`] ahead of the
    /// analysis; a side whose own driver has no converter carries its raw content
    /// here, which is what `fill_textconv(NULL, df)` hands back.
    textconv: Option<(Vec<u8>, Vec<u8>)>,
}

/// `userdiff_find_by_path()` for a pair's two filespecs. git looks the two sides up
/// separately and uses them for different things: `run_diff_cmd()` takes the external
/// command from `attr_path`, which is `p->one->path` (diff.c:5036); `builtin_diff()`
/// asks `one` for the funcname pattern and falls back to `two` (diff.c:4036-4038); and
/// `get_textconv()` converts each side through its *own* path's driver
/// (diff.c:3891-3892). The two differ only for a rename or a copy.
#[derive(Clone, Default)]
struct PairDrivers {
    /// `p->one->path`, git's `attr_path`.
    one: Option<std::sync::Arc<crate::userdiff::Driver>>,
    /// `p->two->path`.
    two: Option<std::sync::Arc<crate::userdiff::Driver>>,
}

impl PairDrivers {
    /// `pe = diff_funcname_pattern(o, one); if (!pe) pe = diff_funcname_pattern(o, two);`
    fn funcname(&self) -> Option<&crate::userdiff::FuncName> {
        self.one
            .as_ref()
            .and_then(|d| d.funcname.as_ref())
            .or_else(|| self.two.as_ref().and_then(|d| d.funcname.as_ref()))
    }

    /// `init_diff_words_data()` (diff.c:2347-2348), which asks the two sides in the
    /// same order and for the same reason the funcname pattern is asked for above:
    ///
    /// ```c
    /// if (!o->word_regex) o->word_regex = userdiff_word_regex(one, o->repo->index);
    /// if (!o->word_regex) o->word_regex = userdiff_word_regex(two, o->repo->index);
    /// ```
    ///
    /// The assignment lands in a `memcpy`'d *copy* of the options (diff.c:2336-2337),
    /// so a pattern found for one pair does not carry over to the next one — which is
    /// why this is answered per pair rather than latched.
    fn word_regex(&self) -> Option<&str> {
        self.one
            .as_ref()
            .and_then(|d| d.settings.word_regex.as_deref())
            .or_else(|| self.two.as_ref().and_then(|d| d.settings.word_regex.as_deref()))
    }
}

/// One pair's driver word regex, compiled the way `init_diff_words_data()` compiles
/// it — `regcomp(…, REG_EXTENDED | REG_NEWLINE)` — and memoised per pattern, since a
/// run over a thousand files of one language would otherwise compile the same text a
/// thousand times.
///
/// `None` whenever the driver regex cannot be reached at all: no `--word-diff`, or a
/// `--word-diff-regex`/`--color-words=<re>` on the command line, which is the state
/// in which git never calls `userdiff_word_regex()` and therefore never notices that
/// the driver's pattern would not have compiled.
///
/// `die(_("invalid regular expression: %s"))` (diff.c:2358) is the refusal.
fn driver_word_regex(
    cache: &mut std::collections::HashMap<String, std::sync::Arc<regex::bytes::Regex>>,
    drivers: &PairDrivers,
    want: bool,
) -> Result<Option<std::sync::Arc<regex::bytes::Regex>>> {
    if !want {
        return Ok(None);
    }
    let Some(pat) = drivers.word_regex() else {
        return Ok(None);
    };
    if let Some(hit) = cache.get(pat) {
        return Ok(Some(hit.clone()));
    }
    let re = std::sync::Arc::new(
        diff_color::compile_word_regex(pat)
            .map_err(|_| crate::fatal::die(format!("invalid regular expression: {pat}")))?,
    );
    cache.insert(pat.to_string(), re.clone());
    Ok(Some(re))
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
            old_worktree: false,
            unmerged: false,
            stages: None,
            src_path: None,
            score: 0,
            status: 0,
            new_id: None,
            old_raw_id: None,
            old_dirty_submodule: 0,
            dirty_submodule: 0,
            new_commit: None,
            drivers: PairDrivers::default(),
            textconv: None,
        }
    }

    /// `run_diff()` (diff.c:5052): both sides are valid and their `S_IFMT` bits
    /// differ, so the patch formats render the pair as a deletion followed by a
    /// creation. A permission-only change (`100644` → `100755`) is *not* one: both
    /// modes are `S_IFREG`, and git keeps that as a single `old mode`/`new mode`
    /// section.
    fn type_changed(&self) -> bool {
        !self.unmerged
            && match (self.old, self.new_kind()) {
                (Some((_, ok)), Some(nk)) => ifmt_class(ok) != ifmt_class(nk),
                _ => false,
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

/// `external_diff()` (diff.c:5026): the program every pair goes to unless its own
/// driver names one. `GIT_EXTERNAL_DIFF` first — kept even when it is the empty
/// string, which `xstrdup_or_null()` does not turn into NULL — and `diff.external`
/// with `diff.trustExitCode` second.
///
/// `git_diff_ui_config()` owns the two configuration keys, so only the porcelains
/// (`diff`, `log`, `show`) read them; the plumbing verbs see the environment alone.
fn external_diff_program(
    repo: &gix::Repository,
) -> Result<Option<super::diff_pairs::ExternalDiff>> {
    if let Some(env) = super::diff_pairs::external_diff_env().map_err(crate::fatal::die)? {
        return Ok(Some(env));
    }
    let snapshot = repo.config_snapshot();
    let Some(cmd) = snapshot.string("diff.external") else {
        return Ok(None);
    };
    Ok(Some(super::diff_pairs::ExternalDiff {
        cmd: cmd.to_string(),
        trust_exit_code: snapshot.boolean("diff.trustExitCode").unwrap_or(false),
    }))
}

/// `run_diff_cmd()`'s driver override (diff.c:4952-4956): a pair whose `attr_path`
/// selects a driver configuring `diff.<name>.command` goes to that program in
/// preference to [`external_diff_program`]'s.
fn external_for_pair(
    delta: &Delta,
    env: Option<&super::diff_pairs::ExternalDiff>,
) -> Option<super::diff_pairs::ExternalDiff> {
    if let Some(drv) = &delta.drivers.one {
        if let Some(cmd) = &drv.settings.external {
            return Some(super::diff_pairs::ExternalDiff {
                cmd: cmd.clone(),
                trust_exit_code: drv.settings.trust_exit_code,
            });
        }
    }
    env.cloned()
}

/// This pair as the shared [`super::diff_pairs::run_external_diff`] engine sees it.
///
/// `old_id`/`new_id` are `diff_fill_oid_info()`'s (diff.c:4990), which the caller
/// already has: for a tree pair they are the queue's own ids, and for a worktree side
/// the hash the analysis computed. git fills that hash without raising `oid_valid`,
/// which is why the id can be real while `prepare_temp_file()` still hands the driver
/// the worktree path and names it by the null id.
fn ext_pair(
    delta: &Delta,
    old_id: ObjectId,
    new_id: ObjectId,
    null: ObjectId,
) -> super::diff_pairs::ExtPair {
    let kind = status_char(delta);
    super::diff_pairs::ExtPair {
        old_path: delta.old_path().clone(),
        new_path: delta.path.clone(),
        old_id: if delta.old.is_some() { old_id } else { null },
        new_id: if delta.new_valid() { new_id } else { null },
        old_mode: delta.old.map_or(0, |(_, k)| kind_mode(k)),
        new_mode: delta.new_kind().map_or(0, kind_mode),
        old_oid_valid: !delta.old_worktree,
        new_oid_valid: !matches!(delta.new, NewSide::Worktree(_)),
        kind,
        // `fill_metainfo()` prints `similarity index %d%%` through
        // `similarity_index(p->score)`, so the engine is handed the percentage.
        score: match kind {
            b'C' | b'R' | b'M' => diffcore_rename::similarity_index(delta.score),
            _ => 0,
        },
    }
}

/// The state one command's external-diff invocations share: the gitattributes stack
/// `run_diff_cmd()` resolves `attr_path` through, `external_diff()`'s program, and
/// `o->diff_path_counter`.
///
/// `index_read` is false because this port never populates `r->index` before
/// diffing, which is what `reuse_worktree_file()` requires: measured against git
/// 2.55.0, `GIT_EXTERNAL_DIFF=echo git diff HEAD~1 HEAD` hands the driver two
/// temporary files even when the worktree already holds the post-image byte for
/// byte. A side that has no object at all still reaches the driver as its worktree
/// path, through `prepare_temp_file()`'s `!oid_valid` branch.
fn ext_context<'a, 'repo>(
    drivers: super::diff_pairs::Drivers<'a, 'repo>,
    env: Option<super::diff_pairs::ExternalDiff>,
) -> super::diff_pairs::ExtCtx<'a, 'repo> {
    super::diff_pairs::ExtCtx {
        drivers,
        env,
        counter: std::cell::Cell::new(0),
        index_read: std::cell::Cell::new(false),
        index: std::cell::OnceCell::new(),
    }
}

/// `fill_textconv()` (diff.c:7793) for every queued pair, ahead of the analysis.
///
/// git converts a side inside `builtin_diff()` as the pair is rendered
/// (diff.c:4027-4028). This runs the converters in one pass instead, because
/// [`analyze_all`] fans the pairs across worker threads and a textconv program is an
/// external process that has to be started once per side, in queue order, the way git
/// starts it.
///
/// A pair whose drivers configure no converter is left alone, so the ordinary diff
/// pays nothing for this pass beyond the walk.
///
/// One consequence of converting up front: `die(_("unable to read files to diff"))`
/// arrives before anything has been printed, where git — which converts as it
/// renders — has already flushed whatever preceded the failing pair. Measured
/// against git 2.55.0, `git -c diff.<drv>.textconv=false diff -p --stat` prints the
/// stat block and then the fatal, while this prints the fatal alone. Both exit 128,
/// and a run whose *first* rendered pair is the failing one agrees byte for byte.
///
/// `diff.<driver>.cachetextconv` is honoured: a converted side whose filespec is
/// oid-valid is looked up in, and written back to, the `refs/notes/textconv/<driver>`
/// cache [`super::notes::Cache`] ports.
fn apply_textconv(
    repo: &gix::Repository,
    drivers: &mut DriverCache<'_>,
    deltas: &mut [Delta],
    workdir: Option<&std::path::Path>,
) -> Result<()> {
    // `driver->textconv_cache`: git hangs one off each `struct userdiff_driver`, which
    // is process-global, so a run builds at most one per driver name
    // (`userdiff_get_textconv()`, userdiff.c:432-439).
    let mut caches: std::collections::HashMap<String, super::notes::Cache> =
        std::collections::HashMap::new();
    for d in deltas.iter_mut() {
        // `builtin_diff()` returns from its submodule branches (diff.c:3870-3887)
        // before `get_textconv()` is ever called, and an unmerged pair never reaches
        // `builtin_diff()` at all.
        if d.unmerged || d.is_submodule_pair() {
            continue;
        }
        let one_drv = d.drivers.one.clone();
        let two_drv = d.drivers.two.clone();
        let one = one_drv.as_ref().and_then(|x| x.settings.textconv.clone());
        let two = two_drv.as_ref().and_then(|x| x.settings.textconv.clone());
        if one.is_none() && two.is_none() {
            continue;
        }
        let old_path = d.old_path().clone();
        // `get_textconv()` (diff.c:7745) answers NULL for an invalid filespec, so an
        // addition's pre-image and a deletion's post-image are never converted —
        // `fill_textconv()` then returns the empty string for them.
        let old_img = match (&one, d.old) {
            (Some(pgm), Some((id, _))) => {
                let raw = match d.old_worktree {
                    true => read_worktree_bytes(workdir, &old_path).unwrap_or_default(),
                    false => read_blob(&repo.objects, id)?,
                };
                // `df->oid_valid`: the worktree side of a `git diff` pair reaches
                // `fill_textconv()` with a null id and is never cached.
                let key = (!d.old_worktree).then_some(id);
                cached_textconv(
                    repo,
                    drivers,
                    &mut caches,
                    one_drv.as_deref(),
                    pgm,
                    old_path.as_bstr(),
                    key,
                    &raw,
                )?
            }
            (_, Some((id, _))) => match d.old_worktree {
                true => read_worktree_bytes(workdir, &old_path).unwrap_or_default(),
                false => read_blob(&repo.objects, id)?,
            },
            (_, None) => Vec::new(),
        };
        let new_raw = match &d.new {
            NewSide::Absent => None,
            NewSide::Blob(id, _) => Some((Some(*id), read_blob(&repo.objects, *id)?)),
            NewSide::Worktree(_) => {
                Some((None, read_worktree_bytes(workdir, &d.path).unwrap_or_default()))
            }
        };
        let new_img = match (&two, new_raw) {
            (Some(pgm), Some((key, raw))) => cached_textconv(
                repo,
                drivers,
                &mut caches,
                two_drv.as_deref(),
                pgm,
                d.path.as_bstr(),
                key,
                &raw,
            )?,
            (_, Some((_, raw))) => raw,
            (_, None) => Vec::new(),
        };
        d.textconv = Some((old_img, new_img));
    }
    Ok(())
}

/// `fill_textconv()`'s cache arms around [`run_textconv`] (diff.c:7086-7108):
///
/// ```c
/// if (driver->textconv_cache && df->oid_valid) {
///         *outbuf = notes_cache_get(driver->textconv_cache, &df->oid, &size);
///         if (*outbuf)
///                 return size;
/// }
/// *outbuf = run_textconv(r, driver->textconv, df, &size);
/// if (!*outbuf)
///         die("unable to read files to diff");
/// if (driver->textconv_cache && df->oid_valid) {
///         notes_cache_put(driver->textconv_cache, &df->oid, *outbuf, size);
///         notes_cache_write(driver->textconv_cache);
/// }
/// ```
///
/// `driver->textconv_cache` exists only when the driver set
/// `diff.<name>.cachetextconv` (`textconv_want_cache`, userdiff.c:432), and `key` is
/// `Some` only for an oid-valid filespec — a worktree side is converted afresh every
/// time. The write is best-effort: git ignores the error "as we might be in a
/// readonly repository".
#[allow(clippy::too_many_arguments)]
fn cached_textconv(
    repo: &gix::Repository,
    drivers: &mut DriverCache<'_>,
    caches: &mut std::collections::HashMap<String, super::notes::Cache>,
    driver: Option<&crate::userdiff::Driver>,
    program: &str,
    path: &gix::bstr::BStr,
    key: Option<gix::hash::ObjectId>,
    raw: &[u8],
) -> Result<Vec<u8>> {
    let cache_key = match (driver, key) {
        (Some(drv), Some(_)) if drv.settings.cache_textconv => Some(drv.name.clone()),
        _ => None,
    };
    let Some(name) = cache_key else {
        return run_textconv(drivers, program, path, raw);
    };
    let key = key.expect("cache_key is None without an oid");
    if !caches.contains_key(&name) {
        // `userdiff_get_textconv()` (userdiff.c:436): `textconv/<driver>`, validity
        // string the converter command.
        let cache = super::notes::Cache::init(repo, &format!("textconv/{name}"), program)?;
        caches.insert(name.clone(), cache);
    }
    if let Some(hit) = caches[&name].get(repo, &key) {
        return Ok(hit);
    }
    let out = run_textconv(drivers, program, path, raw)?;
    caches.get_mut(&name).expect("just inserted").put(repo, key, &out);
    Ok(out)
}

/// `run_textconv()` (diff.c:7758): materialise the blob the way `prepare_temp_file()`
/// does — its worktree form, under its own basename in a private directory — run the
/// program over it through the shell, and take its stdout.
///
/// `if (!*outbuf) die(_("unable to read files to diff"))`: a program that could not be
/// started, or that exited non-zero, is fatal.
///
/// One departure: `prepare_temp_file()` hands the program the *worktree file itself*
/// for a side that has no object of its own (`!one->oid_valid`), where this always
/// writes a temporary copy. The bytes agree for every path whose worktree form is its
/// stored form; a path with a smudge filter would have that filter applied twice, and
/// a converter that reads its argument's directory sees the temporary one. No such
/// converter is reachable from the cases this is measured on, and the fix is a second
/// entry point on [`super::cat_file::Textconv`] rather than a change here.
fn run_textconv(
    drivers: &mut DriverCache<'_>,
    program: &str,
    path: &gix::bstr::BStr,
    raw: &[u8],
) -> Result<Vec<u8>> {
    match drivers.run_program(program, path, raw)? {
        Some(text) => Ok(text),
        None => Err(crate::fatal::die("unable to read files to diff")),
    }
}

/// `userdiff_find_by_path()` over the whole queue, plus `xdiff_set_find_func()` once
/// per distinct driver.
///
/// git resolves this inside `run_diff_cmd()` and `builtin_diff()`, per pair, as each
/// one is rendered. Doing it in a single pass here keeps the gitattributes stack and
/// the compiled regexes out of the worker threads [`analyze_all`] fans the pairs
/// across, and compiles each driver's pattern once instead of once per file.
///
/// A pattern that will not compile is `die(_("Invalid regexp to look for hunk header:
/// %s"))`. git raises it when the first pair carrying that driver reaches xdiff, so a
/// pair rendered *before* that one has already printed; this port renders nothing
/// until every pair has been analyzed, so its refusal always arrives with no output.
/// The two agree whenever the offending pair is the first one — measured against git
/// 2.55.0, `git -c diff.markdown.xfuncname='^[' diff HEAD~3 HEAD` over a three-file
/// change prints the fatal and nothing else.
fn resolve_drivers(drivers: &mut DriverCache<'_>, deltas: &mut [Delta]) -> Result<()> {
    for d in deltas.iter_mut() {
        let one_path = d.old_path().clone();
        let one = drivers.for_path(one_path.as_bstr()).map_err(crate::fatal::die)?;
        d.drivers.two = if d.path == one_path {
            one.clone()
        } else {
            drivers.for_path(d.path.as_bstr()).map_err(crate::fatal::die)?
        };
        d.drivers.one = one;
    }
    Ok(())
}

/// The [`crate::userdiff::Lookup`] a command holds for its whole run: one
/// gitattributes stack, one compiled driver per name, and the worktree filter the
/// textconv programs are fed through. Shared by [`resolve_drivers`],
/// [`apply_textconv`] and every commit one `log -p` worker renders.
pub(crate) type DriverCache<'repo> = crate::userdiff::Lookup<'repo>;

/// The `S_IFMT` class of a tree entry mode — what `run_diff()` compares when it
/// decides a pair is a type change (diff.c:5054). `100644` and `100755` are both
/// `S_IFREG` and therefore share a class; a blob, a symlink, a tree and a gitlink
/// each get their own.
fn ifmt_class(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Blob | EntryKind::BlobExecutable => 0,
        EntryKind::Link => 1,
        EntryKind::Tree => 2,
        EntryKind::Commit => 3,
    }
}

/// The two halves `run_diff()` renders for a type change (diff.c:5052): the
/// pre-image against an invalid post-image, then an invalid pre-image against the
/// post-image. git allocates each null filespec with the *other* side's path, but
/// `diffcore_rename` never scores a rename across a type change, so both halves
/// carry the pair's single name.
///
/// Only the patch formats see this. `run_diffstat()` (diff.c:5078) and
/// `diff_flush_raw()` are handed the unsplit pair, which is why `--stat` shows one
/// row and `--raw`/`--name-status` one `T` record.
fn split_type_change(d: &Delta) -> Option<(Delta, Delta)> {
    let old = d.old.filter(|_| d.type_changed())?;
    let deletion = Delta {
        path: d.path.clone(),
        old: Some(old),
        old_worktree: d.old_worktree,
        new: NewSide::Absent,
        unmerged: false,
        stages: None,
        src_path: None,
        score: 0,
        status: b'D',
        new_id: None,
        old_raw_id: None,
        old_dirty_submodule: 0,
        dirty_submodule: 0,
        new_commit: None,
        drivers: d.drivers.clone(),
        // The deletion half's post-image is git's invalid filespec, which
        // `fill_textconv()` renders as nothing at all.
        textconv: d.textconv.as_ref().map(|(o, _)| (o.clone(), Vec::new())),
    };
    let creation = Delta {
        path: d.path.clone(),
        old: None,
        old_worktree: false,
        new: match d.new {
            NewSide::Absent => return None,
            NewSide::Blob(id, k) => NewSide::Blob(id, k),
            NewSide::Worktree(k) => NewSide::Worktree(k),
        },
        unmerged: false,
        stages: None,
        src_path: None,
        score: 0,
        status: b'A',
        new_id: d.new_id,
        old_raw_id: None,
        old_dirty_submodule: 0,
        dirty_submodule: d.dirty_submodule,
        new_commit: d.new_commit,
        drivers: d.drivers.clone(),
        textconv: d.textconv.as_ref().map(|(_, n)| (Vec::new(), n.clone())),
    };
    Some((deletion, creation))
}

/// Per-delta blob analysis: the new-side object id plus line counts and the
/// rendered hunks (only computed when a patch is actually requested).
struct Analysis {
    /// The pre-image id the `index` line prints. Normally [`Delta::old`]'s own id,
    /// but a worktree-backed pre-image ([`Delta::old_worktree`]) has none until its
    /// content is hashed, which is what `diff_populate_filespec()` does for git.
    old_id: ObjectId,
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
    // `setup_revisions()`'s `seen_dashdash`, established by a scan of the whole
    // argument vector before anything is resolved — so it is already in force
    // for the arguments standing in front of the separator.
    let seen_dashdash = args.iter().any(|a| a == "--");

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
    // `options->a_prefix` / `options->b_prefix`, which `diff_setup()` leaves NULL
    // when `diff.mnemonicPrefix` is on so that the comparison the command actually
    // runs can name them (diff.c:5149-5153). `None` is that NULL; anything that
    // assigns a prefix — config, a `--*-prefix` flag, or the mnemonic fill below —
    // makes it `Some`, and `diff_set_mnemonic_prefix()` only ever fills a `None`.
    let mut src_prefix: Option<Vec<u8>> = None;
    let mut dst_prefix: Option<Vec<u8>> = None;
    // `static int diff_mnemonic_prefix` (diff.c:69), read by `git_diff_ui_config()`
    // (diff.c:406-409) — the *ui* callback, which is why the plumbing verbs
    // (`diff-files`, `diff-index`, `diff-tree`) ignore the key entirely.
    let mnemonic_prefix;
    // `--line-prefix=<s>`: prepended to every emitted line (`diff_line_prefix()`).
    let mut line_prefix: Vec<u8> = Vec::new();
    // `--compact-summary`: annotate `--stat` names with create/delete/mode info.
    let mut compact_summary = false;
    let mut func_context = false;
    // `cmd_diff()` (builtin/diff.c:635) sets `flags.allow_textconv` before the option
    // parse, so `git diff` converts by default and only `--no-textconv` stops it.
    let mut allow_textconv = true;
    // `flags.allow_external`, raised beside it: `git diff` runs an external driver by
    // default where `log`/`show` need `--ext-diff` to ask for one.
    let mut allow_external = true;
    // The `xdiff` knobs fed to [`super::diff_pairs::emit_unified`]: `xpp.flags`'
    // `XDF_IGNORE_BLANK_LINES`, `xpp.ignore_regex` (`-I<re>`),
    // `xecfg.interhunkctxlen` and `DIFF_OPT_TEXT`.
    let mut ignore = IgnoreOpts::default();
    // `-D`/`--irreversible-delete` (`diff_opt_irreversible_delete`): a deletion emits
    // its header and stops.
    let mut irreversible_delete = false;
    // `--skip-to=<p>` / `--rotate-to=<p>` (`diffcore_rotate`): where the queue is
    // re-anchored, and which of the two it is. The last one on the line wins.
    let mut skip_or_rotate: Option<(bool, BString)> = None;
    // `--output=<file>` (`diff_opt_output`): git's `xfopen(arg, "w")`, so the file is
    // created and truncated during the option scan and every rendered byte goes there
    // instead of to stdout.
    let mut output_file: Option<std::fs::File> = None;
    // `revs.diffopt.flags.ita_invisible_in_index`, which `cmd_diff()` sets before
    // its option scan: an `add -N` entry is a *creation* to the index-vs-worktree
    // walk rather than a modification of the empty blob it stands in for.
    let mut ita_invisible = true;
    // `--dirstat`'s parameter block (`struct dirstat_opts`), shared with the
    // `diff-files`/`diff-index` port that renders it.
    let mut dirstat = super::diff_files::DirStat::default();
    let mut diff_filter: Option<Vec<u8>> = None;
    let mut algorithm: Option<gix::diff::blob::Algorithm> = None;
    // `diff_options.anchors` — the repeatable `--anchored=<text>` list.
    let mut anchors: Vec<String> = Vec::new();
    // `options->objfind`, the one oidset every `--find-object=<id>` inserts into.
    // Empty means the objfind pickaxe was never requested.
    let mut find_object_ids: Vec<ObjectId> = Vec::new();
    // `DIFF_OPT_PICKAXE_ALL`: with any pickaxe kind set, one matching pair keeps the
    // whole queue. On its own it has no effect on the output and is kept only so
    // `diff_setup_done()`'s objfind conflict can be raised.
    let mut pickaxe_all = false;
    // `o->pickaxe` and the `DIFF_PICKAXE_KIND_*` bit, as typed: the kind letter and
    // the raw pattern. Compiled after the scan, since `--pickaxe-regex` may follow
    // the `-S` it promotes and `diff_setup_done()`'s conflicts outrank a bad regex.
    let mut pickaxe_arg: Option<(u8, Vec<u8>)> = None;
    // `DIFF_PICKAXE_REGEX`.
    let mut pickaxe_regex = false;
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
    let mut config_algorithm: Option<gix::diff::blob::Algorithm> = None;
    // The `--stat` geometry (`show_stats()`), in git's sentinel encoding.
    // `builtin/diff.c:510` calls `init_diffstat_widths()`, so all three widths
    // start at `-1`: the total is the terminal width and the name/graph columns
    // take `diff.statNameWidth`/`diff.statGraphWidth`, seeded below and then
    // overwritten by an explicit `--stat*` flag (git precedence; a
    // `--stat-name-width=0` flag legitimately overrides a positive config).
    let mut sw = StatWidths::default();
    // `diff.suppressBlankEmpty`: emit an empty context line as `"\n"` rather than
    // the default `" \n"` (`fn_out_consume()`); no CLI flag exists for it.
    let mut suppress_blank_empty = false;
    // `--color[=<when>]` / `--no-color`. `None` leaves the decision to
    // `color.diff` / `diff.color` / `color.ui` and the terminal test.
    let mut color_when: Option<diff_color::ColorWhen> = None;
    // `--ws-error-highlight=<kind>`, seeded from `diff.wsErrorHighlight` once the
    // repository's config is readable, below. git's own starting value is
    // `ws_error_highlight_default = WSEH_NEW` (diff.c), which is what an absent —
    // or, at this point, unreachable — config leaves standing.
    let mut ws_error_highlight: u32 = diff_color::WSEH_NEW;
    // `--color-moved*` / `--word-diff*` / `--color-words`, layered over
    // `diff.colorMoved` / `diff.colorMovedWS` / `diff.wordRegex` below.
    let mut move_word = diff_color::MoveWordOpts::default();
    // `options->output_indicators[]` (diff.c:5143-5145), replaced by
    // `--output-indicator-new`/`-old`/`-context`.
    let mut indicators = (b'+', b'-', b' ');
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
    // `setup_git_directory_gently(&nongit)`: with no repository `cmd_diff()`
    // carries on with `nongit` set, and every one of its remaining branches then
    // leads to `diff_no_index()` with `implicit_no_index` — the `die()` at the
    // end of the function is only reachable once that has been ruled out. Any
    // setup failure counts, not just an empty search: a `$GIT_DIR` that names no
    // repository leaves `nongit` set too.
    let mut repo = match crate::setup::discover() {
        Ok(repo) => repo,
        Err(_) => return super::diff_no_index::run_implicit(args),
    };
    // Object-heavy path: give gix the caches it does not enable by default —
    // a decoded-object cache and a git-sized delta-base cache (gix ships a
    // 64-entry linked list; git's core.deltaBaseCacheLimit default is 96MB).
    repo.object_cache_size_if_unset(16 * 1024 * 1024);
    repo.objects.set_pack_cache(|| {
        Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(96 * 1024 * 1024))
    });

    // `cmd_diff()`'s *implicit* `--no-index` (builtin/diff.c:466-476), the arm that
    // fires inside a repository:
    //
    // ```c
    // for (i = 1; i < argc; i++) {
    //         if (!strcmp(argv[i], "--")) { i++; break; }
    //         if (!strcmp(argv[i], "--no-index")) no_index = DIFF_NO_INDEX_EXPLICIT;
    //         if (argv[i][0] != '-') break;
    // }
    // …
    // if (nongit || ((argc == i + 2) &&
    //                (!path_inside_repo(the_repository, prefix, argv[i]) ||
    //                 !path_inside_repo(the_repository, prefix, argv[i + 1]))))
    //         no_index = DIFF_NO_INDEX_IMPLICIT;
    // ```
    //
    // Exactly two operands, at least one of them naming somewhere outside the
    // worktree: git reads that as "be a colourful `diff(1)`" rather than as a
    // revision pair. The count is of *all* remaining argv entries, `--` included,
    // which is what makes `git diff .. --` the no-index usage block (`..` escapes
    // the worktree, two entries follow the options) while `git diff .. -- a.txt` is
    // three and stays an ordinary diff.
    {
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--" {
                i += 1;
                break;
            }
            if !args[i].starts_with('-') {
                break;
            }
            i += 1;
        }
        if args.len() == i + 2 {
            let prefix = cwd_prefix(&repo);
            let outside = |p: &String| !path_inside_repo(&repo, &prefix, p);
            if outside(&args[i]) || outside(&args[i + 1]) {
                return super::diff_no_index::run_implicit(args);
            }
        }
    }

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
        // `diff.relative` seeds the very flag `--relative` sets (`options->flags
        // .relative_name = diff_relative`, diff.c:4639), so the config alone both
        // narrows the change list to the current directory and shortens the paths
        // reported. `--no-relative` clears it again, which falls out of the flags
        // below assigning `relative` unconditionally.
        if snap.boolean("diff.relative") == Some(true) {
            relative = Some(cwd_prefix(&repo));
        }
        // `diff_setup()`'s prefix decision, which is the whole of the
        // `diff.mnemonicPrefix` mechanism:
        //
        // ```c
        // if (diff_no_prefix) {
        //         diff_set_noprefix(options);
        // } else if (!diff_mnemonic_prefix) {
        //         diff_set_default_prefix(options);
        // }
        // ```
        //
        // (diff.c:5149-5153, with `diff_set_noprefix()` at 3728-3731 and
        // `diff_set_default_prefix()` at 3733-3737.) Three consequences, each
        // confirmed against stock git 2.55.0:
        //
        //   * `diff.noPrefix` wins over `diff.mnemonicPrefix` — both prefixes
        //     become the empty string and the mnemonic fill, which only writes a
        //     NULL slot, can no longer reach them.
        //   * with `diff.mnemonicPrefix` on and `diff.noPrefix` off, *neither*
        //     prefix is assigned here, so `diff.srcPrefix`/`diff.dstPrefix` are
        //     silently ignored: `git -c diff.mnemonicPrefix=true -c
        //     diff.srcPrefix=S/ diff` prints `i/`, not `S/`.
        //   * without `diff.mnemonicPrefix` nothing changes — the configured
        //     prefixes, or `a/` and `b/`, are installed up front as before.
        mnemonic_prefix = snap.boolean("diff.mnemonicPrefix") == Some(true);
        if snap.boolean("diff.noPrefix") == Some(true) {
            src_prefix = Some(Vec::new());
            dst_prefix = Some(Vec::new());
        } else if !mnemonic_prefix {
            src_prefix = Some(
                snap.string("diff.srcPrefix")
                    .map_or_else(|| b"a/".to_vec(), |p| p.into()),
            );
            dst_prefix = Some(
                snap.string("diff.dstPrefix")
                    .map_or_else(|| b"b/".to_vec(), |p| p.into()),
            );
        }
        // `diff.algorithm` names the default algorithm (`git_diff_ui_config()`, which
        // only the porcelain runs — the two plumbing verbs never read it).
        //
        // Only the *value* is read here. An unknown name never reaches this point:
        // git rejects it while loading config, and this port does the same in
        // `crate::diff_config`'s `diff.algorithm` arm, which `dispatch.rs` runs
        // for `diff` before the verb is called — and only ever gets that far when
        // `gix::discover()` succeeded, which is the same condition this block is
        // under. Refusing again here printed the wrong text anyway: it named the
        // *option* (`diff algorithm "bogus" is not available`) where the config
        // reader says `unknown value for config 'diff.algorithm': bogus` followed
        // by a `fatal:` naming the file and line. The no-repository route
        // (`--no-index`, which the gate cannot cover) keeps its own copy of the
        // refusal in [`super::diff_no_index`].
        if let Some(name) = snap.string("diff.algorithm") {
            config_algorithm = super::diff_optval::parse_algorithm_value(&name.to_str_lossy());
        }
        // `diff.indentHeuristic` (`git_diff_basic_config()`): the default landing spot
        // for a slidable hunk. A command-line `--[no-]indent-heuristic` overrides it.
        if let Some(b) = snap.boolean("diff.indentHeuristic") {
            indent_heuristic = b;
        }
        // `diff.interHunkContext` (`git_diff_ui_config()`): `diff_setup()` seeds
        // `options->interhunkcontext` from `diff_interhunk_context_default` before
        // parse-options runs, so `--inter-hunk-context=<n>` simply overwrites it
        // below. A negative value is the config reader's own refusal.
        if let Some(n) = snap.integer("diff.interHunkContext") {
            if n < 0 {
                crate::git_fatal!("bad config variable 'diff.interhunkcontext'");
            }
            ignore.inter_hunk_ctx = n as usize;
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
                sw.name_width = n;
            }
        }
        if let Some(n) = snap.integer("diff.statGraphWidth") {
            if n > 0 {
                sw.graph_width = n;
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
    // `diff.wsErrorHighlight` (`git_diff_basic_config()`). A value git rejects is
    // a fatal config error, but it is not reported here: `crate::diff_config`'s
    // `diff.wserrorhighlight` arm refuses it in `dispatch.rs`, before this verb
    // runs, with the `error:` line *and* the `fatal: bad config variable …` line
    // that names the file — the second of which this site never printed. So an
    // unparsable value cannot arrive, and the default stands for the `None` that
    // an absent one gives. Same shape as `diff-index`, `diff-files` and `log`.
    if let Ok(v) = diff_color::ws_error_highlight_default(&repo) {
        ws_error_highlight = v;
    }

    let mut revs: Vec<String> = Vec::new();
    // `obj->flags & UNINTERESTING` for each entry of `revs`, which `cmd_diff()` carries
    // from `revs->pending` into its `ent` array (builtin/diff.c:576,589) and
    // `builtin_diff_tree()` then reads to decide which of two trees is the pre-image.
    let mut revs_uninteresting: Vec<bool> = Vec::new();
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
            // `--` is not a value. `setup_revisions()` cuts the option region at
            // the separator before it parses a single option:
            //
            // ```c
            // /* First, search for "--" */
            // ...
            //         for (i = 1; i < argc; i++) {
            //                 const char *arg = argv[i];
            //                 if (strcmp(arg, "--"))
            //                         continue;
            //                 ...
            //                 argv[i] = NULL;
            //                 argc = i;
            // ```
            //
            // (`revision.c`.) So the option ahead of it has no slot left to spend
            // and `get_arg()` refuses (parse-options.c:59-60) — `git diff -S --`
            // is a missing value, not a search for the string `--`, and
            // `git diff --output --` never opens a file called `--`. Every other
            // token, an option-looking one included, *is* taken: `git diff -S -p`
            // searches for `-p`.
            if a == "--" {
                return Ok(missing_value_refusal(&flag));
            }
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
            } else if let Some(slot) = stat_width_slot_of(&mut sw, &flag) {
                match a.parse::<i64>() {
                    Ok(n) => *slot = n,
                    Err(_) => {
                        eprintln!("error: {} expects a numerical value", &flag[2..]);
                        return Ok(ExitCode::from(129));
                    }
                }
            } else if flag == "-O" {
                order_file = Some(a.clone());
            } else if flag == "-l" {
                match parse_rename_limit(a) {
                    Ok(n) => ro.rename_limit = n,
                    Err(code) => return Ok(code),
                }
            } else if flag == "-n" {
                if let Err(code) = check_max_count(a) {
                    return Ok(code);
                }
            } else if flag == "--anchored" {
                // `OPT_CALLBACK_F(0, "anchored", options, N_("<text>"), …, PARSE_OPT_NONEG,
                // diff_opt_anchored)` (diff.c:6228-6230): the separated form of a
                // required-argument callback, so this entry is the anchor text.
                algorithm = Some(gix::diff::blob::Algorithm::Patience);
                anchors.push(a.clone());
            } else if flag == "--diff-algorithm" {
                // The separated form of an `OPT_CALLBACK_F` with a required argument:
                // parse-options has already taken this entry as the value, so it reaches
                // the same `parse_algorithm_value()` the `=` form does.
                match super::diff_optval::parse_algorithm_value(a) {
                    Some(alg) => algorithm = Some(alg),
                    None => {
                        eprintln!("{}", super::diff_optval::DIFF_ALGORITHM_ERR);
                        return Ok(ExitCode::from(129));
                    }
                }
            } else if flag == "--find-object" {
                match crate::objname::find_object(&repo, a) {
                    Ok(id) => find_object_ids.push(id),
                    Err(e) => return Ok(e.report()),
                }
            } else if flag == "--skip-to" || flag == "--rotate-to" {
                skip_or_rotate = Some((flag == "--skip-to", a.as_str().into()));
            } else if flag == "--output" {
                match open_output_file(a) {
                    Ok(f) => output_file = Some(f),
                    Err(code) => return Ok(code),
                }
            } else if flag == "-I" || flag == "--ignore-matching-lines" {
                // The separated spelling of `diff_opt_ignore_regex`, reaching the
                // same `regcomp` the glued one does.
                match super::diff_pickaxe::compile_regex(a.as_bytes()) {
                    Ok(re) => ignore.lines.push(super::diff_pickaxe::Needle::Regex(re)),
                    Err(_) => {
                        eprintln!("error: invalid regex given to -I: '{a}'");
                        return Ok(ExitCode::from(129));
                    }
                }
            } else if flag == "--inter-hunk-context" {
                match parse_inter_hunk_context(a) {
                    Ok(n) => ignore.inter_hunk_ctx = n,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        return Ok(ExitCode::from(129));
                    }
                }
            } else if flag == "-S" || flag == "-G" {
                // The separated spelling of the same callback the glued one reaches.
                let kind = flag.as_bytes()[1];
                if a.is_empty() {
                    eprintln!("{}", super::diff_optval::pickaxe_empty(kind));
                    return Ok(ExitCode::from(129));
                }
                pickaxe_arg = Some((kind, a.as_bytes().to_vec()));
            } else if indicator_slot(&flag).is_some() {
                if let Err(msg) = set_indicator(&mut indicators, &flag, a) {
                    eprintln!("{msg}");
                    return Ok(ExitCode::from(129));
                }
            } else if flag == "--diff-merges" {
                if !is_diff_merges_value(a) {
                    eprintln!("fatal: invalid value for '--diff-merges': '{a}'");
                    return Ok(ExitCode::from(128));
                }
            } else if let Some(Err(msg)) =
                move_word.parse_flag(&format!("{flag}={a}"), &mut color_when)
            {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            continue;
        }
        // `--diff-algorithm` and `--find-object` are `OPT_CALLBACK_F` without
        // `PARSE_OPT_OPTARG`, so parse-options takes the next argv entry as the value
        // — even one that looks like a revision — and a bare flag at the end of the
        // line is `error: option `<name>' requires a value` (129), which the
        // `pending_value` check after this loop already produces.
        //
        // `-S` and `-G` are the same declaration as short options (diff.c:6270-6275),
        // so a bare one takes the next entry too. Treating it as an *empty* pattern
        // instead left the real pattern behind to be read as a revision, which is why
        // `git diff -S dd HEAD~1` died with `fatal: ambiguous argument 'dd'`.
        //
        // Behind a `--` none of that applies: `setup_revisions()` has already
        // moved everything after the separator into `prune_data` (`revision.c`),
        // so the token is a pathspec and not an option at all. `git diff -- -S`
        // limits the diff to a path called `-S`, and claiming it here made that a
        // usage error — which is why the pathspec test comes first.
        if after_dashdash {
            trailing_paths.push(a.clone());
            continue;
        }
        if diff_color::needs_separate_value(a)
            || indicator_slot(a).is_some()
            || a == "--diff-merges"
            || matches!(
                a.as_str(),
                "--diff-algorithm" | "--find-object" | "-S" | "-G" | "--anchored"
            )
        {
            pending_value = Some(a.clone());
            continue;
        }
        // The value checks `diff_opt_parse`'s callbacks run as each option is seen,
        // ahead of every other decision this loop makes about the argument.
        if let Some(line) = super::diff_optval::reject(a) {
            eprintln!("{line}");
            return Ok(ExitCode::from(129));
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
            // Declared `PARSE_OPT_NONEG` (diff.c), so `--no-check` is not an option
            // at all and falls through to the usage error below.
            "--check" => check = true,
            "--numstat" => fmt |= F_NUMSTAT,
            "--shortstat" => fmt |= F_SHORTSTAT,
            "--stat" => fmt |= F_DIFFSTAT,
            "--name-only" => fmt |= F_NAME,
            "--name-status" => fmt |= F_NAME_STATUS,
            "-p" | "-u" | "--patch" => fmt |= F_PATCH,
            // `OPT_SET_INT_F('s', "no-patch", &options->output_format, ...,
            // DIFF_FORMAT_NO_OUTPUT, PARSE_OPT_NONEG)`: an *assignment*, so `-s`
            // wipes every format bit already set — `--check` included, since
            // `DIFF_FORMAT_CHECKDIFF` is one of them. Measured against 2.55.0:
            // `git diff --name-only -s` prints nothing and exits 0 (the `--name-only`
            // bit is gone, so the mutual-exclusion check below sees only one), while
            // `git diff -s --stat` prints the stat block (the `--stat` bit arrives
            // after the assignment).
            "-s" | "--no-patch" => {
                fmt = F_NO_OUTPUT;
                check = false;
            }
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
            // `OPT_BOOL` (diff.c:6256): the negation exists and the last spelling
            // on the line wins.
            "--exit-code" => want_exit_code = true,
            "--no-exit-code" => want_exit_code = false,
            "--quiet" => {
                quiet = true;
                want_exit_code = true;
            }
            "--full-index" => full_index = true,
            // `diff_opt_binary()` (diff.c:5613) is not a plain flag: it calls
            // `enable_patch_output()` first, so `--binary` turns the patch on and
            // clears `DIFF_FORMAT_NO_OUTPUT`. Measured against 2.55.0, `git diff
            // --binary --stat` prints the stat block *and* the patch, and `git diff
            // -s --binary` prints the patch — while `--binary -s` prints nothing,
            // since `-s` assigns the format afterwards.
            "--binary" => {
                binary = true;
                fmt |= F_PATCH;
                fmt &= !F_NO_OUTPUT;
            }
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
            // `diff_opt_no_prefix()` -> `diff_set_noprefix()` (diff.c:5774-5783,
            // 3728-3731): both prefixes become the empty string. They are assigned,
            // not cleared to NULL, so the mnemonic fill can no longer reach them —
            // `--no-prefix` beats `diff.mnemonicPrefix`.
            "--no-prefix" => {
                src_prefix = Some(Vec::new());
                dst_prefix = Some(Vec::new());
            }
            // `diff_opt_default_prefix()` (diff.c:5785-5796) frees `diff_src_prefix`
            // and `diff_dst_prefix` *before* calling `diff_set_default_prefix()`, so
            // it installs the literal `a/` and `b/` even when `diff.srcPrefix` /
            // `diff.dstPrefix` named something else.
            "--default-prefix" => {
                src_prefix = Some(b"a/".to_vec());
                dst_prefix = Some(b"b/".to_vec());
            }
            // Diff-algorithm selection; the last flag on the command line wins.
            "--minimal" => algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal),
            "--myers" => algorithm = Some(gix::diff::blob::Algorithm::Myers),
            "--histogram" => algorithm = Some(gix::diff::blob::Algorithm::Histogram),
            // ```c
            // static int diff_opt_patience(const struct option *opt, const char *arg, int unset)
            // {
            //         /*
            //          * Both --patience and --anchored use PATIENCE_DIFF
            //          * internally, so remove any anchors previously
            //          * specified.
            //          */
            //         for (i = 0; i < options->anchors_nr; i++)
            //                 free(options->anchors[i]);
            //         options->anchors_nr = 0;
            //         options->ignore_driver_algorithm = 1;
            //         return set_diff_algorithm(options, "patience");
            // }
            // ```
            //
            // (`diff.c:5839-5858`.) `--patience` is not just an alias: it *drops* every
            // anchor named before it, so `--anchored=x --patience` is a plain patience
            // diff while `--patience --anchored=x` is anchored.
            "--patience" => {
                algorithm = Some(gix::diff::blob::Algorithm::Patience);
                anchors.clear();
            }
            // Accepted here rather than implemented.
            //
            // Rename detection is *not* in this list any more — `-M`, `-C`,
            // `--find-renames`, `--find-copies`, `--no-renames` and
            // `--rename-empty`/`--no-rename-empty` are parsed above and fed to
            // `diffcore_rename`, so they change the output exactly as stock git's do.
            //
            // The remaining entries are believed to match zvcs's default behavior, but
            // that has not been measured flag by flag — treat them as unverified.
            // `revision.c`'s `--no-notes` turns off a display that is off by
            // default here, so it cannot change any output this command produces.
            "--no-notes" => {}
            "--ignore-cr-at-eol" => ws = Whitespace::IgnoreCrAtEol,
            // `cmd_diff()` (builtin/diff.c) raises `flags.allow_textconv` before
            // parsing, so `--textconv` only restores git's default for this command.
            "--textconv" => allow_textconv = true,
            "--no-textconv" => allow_textconv = false,
            // `cmd_diff()` raises `flags.allow_external` before parsing, so
            // `--ext-diff` only restores git's default for this command.
            "--ext-diff" => allow_external = true,
            "--no-ext-diff" => allow_external = false,
            // `cmd_diff()` (builtin/diff.c:635) raises
            // `flags.ita_invisible_in_index` before parsing, so `git diff`'s default
            // is "invisible" and only `--ita-visible-in-index` lowers it again.
            "--ita-invisible-in-index" => ita_invisible = true,
            "--ita-visible-in-index" => ita_invisible = false,
            // `XDF_IGNORE_BLANK_LINES` (`OPT_BIT` on `xdl_opts`).
            "--ignore-blank-lines" => ignore.blank_lines = true,
            // `DIFF_OPT_TEXT` (`OPT_BIT` on `flags.text`): diff content git would
            // otherwise report as `Binary files ... differ`.
            "-a" | "--text" => ignore.text = true,
            "--no-text" => ignore.text = false,
            // `OPT_CALLBACK_F('I', "ignore-matching-lines", ..., diff_opt_ignore_regex)`:
            // every occurrence appends to `xpp.ignore_regex`, and the value may be
            // glued on or stand as the next argument. The pattern is compiled here,
            // as the callback does, so a bad one is reported at this argv position.
            s if s == "--ignore-matching-lines" || s == "-I" => {
                pending_value = Some(s.to_string())
            }
            s if s.starts_with("--ignore-matching-lines=") || s.starts_with("-I") => {
                let value = match s.strip_prefix("--ignore-matching-lines=") {
                    Some(v) => v,
                    None => &s["-I".len()..],
                };
                match super::diff_pickaxe::compile_regex(value.as_bytes()) {
                    Ok(re) => ignore.lines.push(super::diff_pickaxe::Needle::Regex(re)),
                    Err(_) => {
                        eprintln!("error: invalid regex given to -I: '{value}'");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `diff_opt_irreversible_delete`: `builtin_diff()` (diff.c:3596) emits the
            // header of a pair whose post-image label is `/dev/null` and jumps to the
            // end, so a deletion loses its `---`/`+++` pair and its hunks. The other
            // formats never see the flag.
            "-D" | "--irreversible-delete" => irreversible_delete = true,
            // `diffcore_rotate()` (diff.c:6763): `--skip-to` drops every pair before
            // the named one, `--rotate-to` wraps them to the end. Both are
            // `OPT_STRING`, so the value may stand as the next argument, and the last
            // one on the line is the one `diffcore_std()` reads.
            "--skip-to" | "--rotate-to" => pending_value = Some(a.to_string()),
            s if s.starts_with("--skip-to=") => {
                skip_or_rotate = Some((true, s["--skip-to=".len()..].into()));
            }
            s if s.starts_with("--rotate-to=") => {
                skip_or_rotate = Some((false, s["--rotate-to=".len()..].into()));
            }
            // `diff_opt_output`: `xfopen(arg, "w")`, which happens *during* the option
            // scan — measured against 2.55.0, an unopenable path is fatal even when
            // the command line also carries an unknown option or an unresolvable
            // revision, both of which are reported after the scan.
            "--output" => pending_value = Some(a.to_string()),
            s if s.starts_with("--output=") => {
                match open_output_file(&s["--output=".len()..]) {
                    Ok(f) => output_file = Some(f),
                    Err(code) => return Ok(code),
                }
            }
            // `OPT_INTEGER_F(0, "inter-hunk-context", ..., PARSE_OPT_NONEG)`:
            // `xecfg.interhunkctxlen`. Same two spellings as any `OPT_INTEGER`.
            "--inter-hunk-context" => pending_value = Some(a.to_string()),
            s if s.starts_with("--inter-hunk-context=") => {
                match parse_inter_hunk_context(&s["--inter-hunk-context=".len()..]) {
                    Ok(n) => ignore.inter_hunk_ctx = n,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
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
            // `--output-indicator-new`/`-old`/`-context=<char>` (`diff_opt_char()`,
            // diff.c:5593): a single byte replaces the `+`/`-`/` ` this side of a
            // hunk line is written with. An empty value stores NUL, which
            // `emit_line_0()` writes as no sign at all.
            s if indicator_slot(s.split_once('=').map_or(s, |(n, _)| n)).is_some()
                && s.contains('=') =>
            {
                let (name, val) = s.split_once('=').expect("guarded by contains");
                match set_indicator(&mut indicators, name, val) {
                    Ok(()) => {}
                    Err(msg) => {
                        eprintln!("{msg}");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--expand-tabs[=<n>]` / `--no-expand-tabs` set `revs->expand_tabs_in_log`
            // (revision.c:2575-2583), which only `pretty.c`'s `pp_remainder()` reads
            // when it lays out a commit message. `git diff` never prints one, so the
            // flag is inert here — but its value is still validated, since
            // `strtol_i()` failing is a `die()` before any diff runs.
            "--expand-tabs" | "--no-expand-tabs" => {}
            s if s.starts_with("--expand-tabs=") => {
                let val = &s["--expand-tabs=".len()..];
                // `strtol_i(arg, 10, &val) < 0 || val < 0` (revision.c:2580): a value
                // that is not a whole decimal integer, or is negative, is fatal.
                if !matches!(val.parse::<i32>(), Ok(n) if n >= 0) {
                    eprintln!("fatal: '{val}': not a non-negative integer");
                    return Ok(ExitCode::from(128));
                }
            }
            // `--diff-merges=<v>` / `--no-diff-merges` (diff-merges.c:139-145) touch
            // only `rev_info`'s merge-diff fields, and every one of them is read
            // exclusively by `log_tree_commit()` (log-tree.c:1105-1168).
            // `builtin/diff.c` never walks commits, so none of them can reach output
            // here — but the value is still rejected the same way, because
            // `set_diff_merges()` dies before `cmd_diff()` gets going.
            "--no-diff-merges" => {}
            s if s.starts_with("--diff-merges=") => {
                let val = &s["--diff-merges=".len()..];
                if !is_diff_merges_value(val) {
                    eprintln!("fatal: invalid value for '--diff-merges': '{val}'");
                    return Ok(ExitCode::from(128));
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
            // `OPT_STRING_F(0, "src-prefix", &options->a_prefix, …)` (diff.c:6106-6110)
            // writes the slot directly, so it fills one side while leaving the other
            // for the mnemonic prefix: `-c diff.mnemonicPrefix=true diff
            // --src-prefix=SRC/` prints `SRC/` against `w/`.
            s if s.starts_with("--src-prefix=") => {
                src_prefix = Some(s.as_bytes()["--src-prefix=".len()..].to_vec());
            }
            s if s.starts_with("--dst-prefix=") => {
                dst_prefix = Some(s.as_bytes()["--dst-prefix=".len()..].to_vec());
            }
            s if s.starts_with("--line-prefix=") => {
                line_prefix = s.as_bytes()["--line-prefix=".len()..].to_vec();
            }
            // `diff_opt_diff_algorithm()`. The unknown-value `error()` is already
            // raised by `diff_optval::reject` at the top of this loop (git's own
            // callback order), so only a value it accepted reaches this arm — matched
            // case-insensitively by the one [`super::diff_optval::parse_algorithm_value`]
            // port, which is why `--diff-algorithm=MYERS` is Myers and not a fatal.
            s if s.starts_with("--diff-algorithm=") => {
                algorithm = super::diff_optval::parse_algorithm_value(&s["--diff-algorithm=".len()..]);
            }
            // `--anchored=<text>`, the attached form of the same callback. Every
            // occurrence appends, and each one re-pins the algorithm to patience — so a
            // `--histogram` in between is undone by the next `--anchored`.
            s if s.starts_with("--anchored=") => {
                algorithm = Some(gix::diff::blob::Algorithm::Patience);
                anchors.push(s["--anchored=".len()..].to_string());
            }
            // `diff_opt_find_object()`: each occurrence inserts into one `objfind`
            // oidset, resolved through [`crate::objname::find_object`] at the flag's own
            // argv position so its `error()` competes with every other argument in order.
            s if s.starts_with("--find-object=") => {
                match crate::objname::find_object(&repo, &s["--find-object=".len()..]) {
                    Ok(id) => find_object_ids.push(id),
                    Err(e) => return Ok(e.report()),
                }
            }
            // `--stat=<width>[,<name-width>[,<count>]]` (`diff_opt_stat()`), which
            // like every `--stat*` flag also requests the diffstat format.
            s if s.starts_with("--stat=") => {
                fmt |= F_DIFFSTAT;
                diffstat::parse_stat_geometry(&mut sw, &s["--stat=".len()..]);
            }
            // The four `--stat-*` widths are one `OPT_CALLBACK_F` each, so both the
            // glued `--opt=<n>` and the separated `--opt <n>` spelling reach it.
            s if is_stat_width_flag(s) => {
                fmt |= F_DIFFSTAT;
                pending_value = Some(s.to_string());
            }
            s if s.split_once('=').is_some_and(|(k, _)| is_stat_width_flag(k)) => {
                fmt |= F_DIFFSTAT;
                let (k, v) = s.split_once('=').expect("matched above");
                match v.parse::<i64>() {
                    Ok(n) => *stat_width_slot_of(&mut sw, k).expect("matched above") = n,
                    Err(_) => {
                        eprintln!("error: {} expects a numerical value", &k[2..]);
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if s.starts_with("--stat-") => fmt |= F_DIFFSTAT,
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
            //
            // `OPT_INTEGER` carries no `PARSE_OPT_OPTARG`, so parse-options takes
            // the *next* argv entry when nothing is glued on — `git diff -l 5` is
            // `git diff -l5`. Refusing the bare token outright instead made every
            // separated spelling a usage error, and it hid the value's own
            // diagnostic: `git diff -l foo` is ``switch `l' expects an integer
            // value with an optional k/m/g suffix``, which only [`crate::optint`]
            // below can word.
            "-l" => pending_value = Some(a.to_string()),
            s if s.starts_with("-l") && s.len() > 2 => {
                match parse_rename_limit(&s[2..]) {
                    Ok(n) => ro.rename_limit = n,
                    Err(code) => return Ok(code),
                }
            }
            // `-n<count>` / `-n <count>`: `handle_revision_opt()`'s short spelling
            // of `--max-count` (`revision.c`), which every `setup_revisions()`
            // caller accepts whether or not it walks. `cmd_diff()` does not — it
            // diffs the trees `setup_revisions()` left pending — so the count has
            // no effect and `git diff -n 1` prints the whole diff. What it does
            // have is a value parser, and `git diff -n foo` is
            // `fatal: 'foo': not an integer` at 128 exactly as `git log -n foo` is,
            // so the value is checked here and then dropped.
            "-n" => pending_value = Some(a.to_string()),
            s if s.starts_with("-n") && s.len() > 2 => {
                if let Err(code) = check_max_count(&s[2..]) {
                    return Ok(code);
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
            // `-h` is `diff_opt_parse()`'s internal help, which `usage()`s the
            // block on **stderr** at 129 with no `error:` line — diff never
            // routes it to stdout, unlike the parse_options porcelain.
            "-h" => return Ok(usage_error()),
            // `--pickaxe-all` is `DIFF_OPT_PICKAXE_ALL`, and on its own it changes
            // nothing: `diffcore_pickaxe()` reads it only once a pickaxe kind is
            // set, so `git diff --pickaxe-all` is a plain diff (measured against
            // stock 2.55.0). Accepting it as the no-op it is also lets
            // `diff_setup_done()`'s objfind conflict below see it.
            "--pickaxe-all" => pickaxe_all = true,
            // `DIFF_PICKAXE_REGEX`: it promotes `-S` from a kwset search to a
            // `regcomp`, and it may appear on either side of the `-S` it modifies —
            // `diffcore_pickaxe()` reads `o->pickaxe_opts` once, after the whole
            // scan — so the pattern is only compiled below.
            "--pickaxe-regex" => pickaxe_regex = true,
            "--no-pickaxe-regex" => pickaxe_regex = false,
            // `OPT_PICKAXE_S`/`OPT_PICKAXE_G` (diff.c:6270-6275). The pattern is
            // kept raw and the kind recorded: `--pickaxe-regex` may still be ahead,
            // and `diff_setup_done()`'s two `HAS_MULTI_BITS` `die()`s run after the
            // whole option scan, so a conflict has to beat a compile failure.
            s if s.starts_with("-S") => {
                pickaxe_arg = Some((b'S', s.as_bytes()[2..].to_vec()));
            }
            s if s.starts_with("-G") => {
                pickaxe_arg = Some((b'G', s.as_bytes()[2..].to_vec()));
            }
            s if s.starts_with('-') => bail!("unsupported option {s:?}"),
            s => {
                // A positional is a revision while we are still in the revision
                // region, otherwise a pathspec. Once a positional is neither a
                // resolvable revision nor an existing path, git dies with the
                // "ambiguous argument" fatal (128) at exactly this point — before
                // any later option-value or operand-count check can fire.
                // `handle_revision_arg_1()` refuses a bare `..` ahead of
                // `handle_dotdot()`, so it is the pathspec for the parent
                // directory rather than `HEAD..HEAD` — and the pathspec layer
                // then rejects it for leaving the repository. See
                // [`crate::objname::is_parent_directory_pathspec`].
                if in_rev_region && crate::objname::is_parent_directory_pathspec(s, seen_dashdash) {
                    in_rev_region = false;
                }
                if in_rev_region {
                    // `handle_dotdot_1()` in full, shared with every other command
                    // that takes a range: see [`crate::objname::dotdot`]. Both of
                    // the endings it dies on — an endpoint whose object is absent,
                    // and an `A...B` endpoint that is not a commit — are
                    // `dotdot_missing()`, and `dotdot_fatal` renders either with
                    // whatever `lookup_commit_reference()` printed ahead of it.
                    // `handle_dotdot_1()` resolves *both* endpoints through
                    // `repo_get_oid_with_context()` before either is looked up, so
                    // a range reaches `get_oid_basic()` once per endpoint and warns
                    // twice. A non-range token warns once, at the `resolve` below.
                    if let Some(range) = crate::objname::split_range(s) {
                        crate::objname::warn_ambiguous_refname(&repo, range.a);
                        crate::objname::warn_ambiguous_refname(&repo, range.b);
                    }
                    if let Some(fatal) = crate::objname::dotdot_fatal(&repo, s) {
                        eprint!("{fatal}");
                        return Ok(ExitCode::from(128));
                    }
                    if let crate::objname::Dotdot::Ok { a, b } =
                        crate::objname::dotdot(&repo, s)
                    {
                        let range = crate::objname::split_range(s)
                            .expect("dotdot() answers Ok only for a token that split");
                        if range.symmetric {
                            // `A...B` diffs the merge-base of A and B against B,
                            // exactly like `git diff $(git merge-base A B) B`.
                            // `a` and `b` are already what
                            // `lookup_commit_reference()` peeled the endpoints to.
                            let base = repo.merge_base(a, b)?.detach();
                            revs.push(base.to_hex().to_string());
                        } else {
                            revs.push(range.a.to_owned());
                        }
                        // `handle_dotdot_1()`: `a_flags = flags_exclude` for `A..B`, and
                        // the merge bases a symmetric range pends go in with the same
                        // `flags_exclude` — so the left entry is UNINTERESTING and the
                        // right one (`b_flags = flags`) is not. That is already the
                        // order these two are pushed in, so the swap below never fires
                        // for a range.
                        revs_uninteresting.push(true);
                        revs.push(range.b.to_owned());
                        revs_uninteresting.push(false);
                        continue;
                    }
                    // `if (*arg == '^') { local_flags = UNINTERESTING | BOTTOM; arg++; }`
                    // — the mark is a flag, and everything downstream
                    // (`get_oid_with_context()`, `verify_non_filename()`,
                    // `get_reference()`) sees the shortened name. The flag itself is
                    // kept because `builtin_diff_tree()` reads it back off the second
                    // of two trees (see the swap below); with one tree it changes
                    // nothing, so `^HEAD` diffs exactly what `HEAD` does.
                    let (bare, uninteresting) = crate::objname::uninteresting_mark(s);
                    // `handle_revision_arg_1()` resolves the whole token with
                    // `get_oid_with_context()` — see [`crate::objname`], which is why
                    // a full-length hex gets this far even when the object is absent.
                    if let Some(id) = crate::objname::resolve(&repo, bare) {
                        // `verify_non_filename()`: a name that is simultaneously a
                        // revision and a working-tree path is refused outright rather
                        // than guessed at.
                        if std::fs::symlink_metadata(bare).is_ok() {
                            eprintln!(
                                "fatal: ambiguous argument '{bare}': both revision and filename"
                            );
                            eprintln!("Use '--' to separate paths from revisions, like this:");
                            eprintln!("'git <command> [<revision>...] -- [<file>...]'");
                            return Ok(ExitCode::from(128));
                        }
                        // `get_reference()`'s `die("bad object %s", name)`: the name
                        // resolved, the object is simply not there. The name printed is
                        // the one past the mark, which is where the pointer stands.
                        if repo.find_object(id).is_err() {
                            eprintln!("fatal: bad object {bare}");
                            return Ok(ExitCode::from(128));
                        }
                        revs.push(bare.to_string());
                        revs_uninteresting.push(uninteresting);
                        continue;
                    }
                    // `if (seen_dashdash || *arg == '^') die(_("bad revision '%s'"), arg);`
                    // — a marked operand never reaches the pathspec fallback, and keeps
                    // its mark in the message because `setup_revisions()` still holds
                    // the original `argv[i]`.
                    //
                    // `seen_dashdash` is the same gate and it is *not* positional:
                    // `setup_revisions()` scans the whole vector for the separator up
                    // front (see the scan this function opens with), so a `--` anywhere
                    // makes every earlier operand revision-only. That is why
                    // `git diff nosuchthing..HEAD --` is `bad revision`, while the same
                    // operand without the separator is still free to become a pathspec
                    // and gets the `ambiguous argument` wording below instead.
                    if uninteresting || seen_dashdash {
                        eprint!(
                            "{}",
                            super::log::bad_revision_message_in_gated(&repo, s, seen_dashdash)
                        );
                        return Ok(ExitCode::from(128));
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
    // The anchor list is final once the scan is: `--patience` may have cleared it and
    // a later `--anchored` may have refilled it. It reaches the blob differ through
    // the process-wide slot rather than through a parameter — see
    // [`super::diff_pairs::set_anchor_texts`].
    super::diff_pairs::set_anchor_texts(anchors);

    // A value-taking option left at the end of the command line never reaches its
    // callback: parse-options reports it and exits 129 before any revision or
    // pathspec is looked at.
    if let Some(flag) = pending_value {
        return Ok(missing_value_refusal(&flag));
    }
    // `diff_setup_done()`'s two pickaxe `die()`s, in git's order. They close
    // `setup_revisions()`, so they run once the whole option scan and every
    // positional is behind them — measured against stock 2.55.0, they beat an
    // unknown option in either argv position and lose to a bad positional, to a
    // rejected option *value* and to `diff_opt_find_object()`'s own `error()`,
    // all of which fire while the scan is still running. Both texts and the
    // whole-argv kind scan are [`super::diff_optval`]'s, shared with the two
    // plumbing verbs.
    if super::diff_optval::pickaxe_conflict(args) {
        eprintln!("{}", super::diff_optval::PICKAXE_CONFLICT);
        return Ok(ExitCode::from(128));
    }
    if pickaxe_all && !find_object_ids.is_empty() {
        eprintln!("{}", super::diff_optval::PICKAXE_ALL_OBJFIND_CONFLICT);
        return Ok(ExitCode::from(128));
    }
    // `cmd_diff`'s dispatch (builtin/diff.c:611): with no tree-ish pending the leftover
    // reaches `builtin_diff_files()`, which names it; with one it reaches
    // `builtin_diff_index()`, which prints the usage block alone. `--cached` counts as a
    // pending tree-ish because `cmd_diff` supplies HEAD for it.
    if let Some(arg) = &invalid_arg {
        return Ok(invalid_option(arg, !revs.is_empty() || cached));
    }
    // `diffcore_pickaxe()` compiles the needle once, past the two conflict `die()`s
    // above: `-S` is a literal kwset search unless `--pickaxe-regex` promotes it,
    // `-G` is always a regex, and `--find-object` overrides both because
    // `pickaxe_match()` tests `o->objfind` before it looks at a needle at all.
    // A pattern that will not compile is git's `fatal: invalid regex: …` at 128.
    let mut pickaxe = None;
    if let Some((kind, pat)) = pickaxe_arg {
        if kind == b'S' && !pickaxe_regex {
            pickaxe = Some(super::diff_pickaxe::Kind::Occurrences(
                super::diff_pickaxe::Needle::Literal(pat),
            ));
        } else {
            match super::diff_pickaxe::compile_regex(&pat) {
                Ok(re) => {
                    let needle = super::diff_pickaxe::Needle::Regex(re);
                    pickaxe = Some(if kind == b'S' {
                        super::diff_pickaxe::Kind::Occurrences(needle)
                    } else {
                        super::diff_pickaxe::Kind::Grep(needle)
                    });
                }
                Err(msg) => {
                    eprintln!("fatal: invalid regex: {msg}");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }
    if !find_object_ids.is_empty() {
        pickaxe = Some(super::diff_pickaxe::Kind::ObjFind(std::mem::take(&mut find_object_ids)));
    }
    paths.extend(trailing_paths);
    // `parse_pathspec()` runs inside `setup_revisions()`, so a rejected element
    // is fatal here — before the tree/index/worktree dispatch below, which for a
    // plain `git diff` builds no `PathspecMatcher` at all and would otherwise
    // meet the failure inside gitoxide's status iterator.
    if let Some(msg) = crate::pathspec::parse_pathspec_fatal(&repo, &paths) {
        eprintln!("fatal: {msg}");
        return Ok(ExitCode::from(128));
    }

    // `cmd_diff()` sorts the pending objects into two arrays before it dispatches
    // (builtin/diff.c:576-604):
    //
    // ```c
    // obj = deref_tag(the_repository, obj, NULL, 0);
    // if (!obj)
    //         die(_("invalid object '%s' given."), name);
    // if (obj->type == OBJ_COMMIT)
    //         obj = &repo_get_commit_tree(the_repository,
    //                                     ((struct commit *)obj))->object;
    //
    // if (obj->type == OBJ_TREE) {
    //         …
    //         add_object_array(obj, name, &ent);
    //         …
    // } else if (obj->type == OBJ_BLOB) {
    //         if (2 <= blobs)
    //                 die(_("more than two blobs given: '%s'"), name);
    //         blob[blobs] = entry;
    //         blobs++;
    //
    // } else {
    //         die(_("unhandled object '%s' given."), name);
    // }
    // ```
    //
    // and then (builtin/diff.c:611-631):
    //
    // ```c
    // if (!ent.nr) {
    //         switch (blobs) {
    //         case 0:  builtin_diff_files(&rev, argc, argv); break;
    //         case 1:  if (paths != 1) usage(builtin_diff_usage);
    //                  builtin_diff_b_f(&rev, argc, argv, blob); break;
    //         case 2:  if (paths) usage(builtin_diff_usage);
    //                  builtin_diff_blobs(&rev, argc, argv, blob); break;
    //         default: usage(builtin_diff_usage);
    //         }
    // }
    // else if (blobs)
    //         usage(builtin_diff_usage);
    // ```
    //
    // So a blob operand is not a tree-ish that failed to peel — it is its own
    // arm, and every shape but "one blob and exactly one path" or "two blobs and
    // no path" is the plain usage block at 129. Without this the blob fell through
    // to the tree dispatch below and surfaced gitoxide's "was blob while trying to
    // peel to tree" at exit 1. Only a blob (or a type git refuses outright) is
    // intercepted here; a tree-ish operand takes exactly the path it always did.
    //
    // The loop reads `entry->item` — the object `setup_revisions()` already
    // attached to the pending entry — and `entry->name` only to *name* it in a
    // `die()`. It does not resolve the name a second time, so this classification
    // costs no second trip through `get_oid_basic()` and therefore no second
    // ambiguity warning. [`crate::objname::resolve_quiet`] is that distinction:
    // `revs` holds names already resolved (and already warned about) in the
    // operand loop above, so warning again here made `git diff <40-hex-ref>` say
    // twice what stock says once.
    let mut blobs = 0usize;
    let mut trees = 0usize;
    for name in &revs {
        let Some(id) = crate::objname::resolve_quiet(&repo, name) else {
            continue;
        };
        let Ok(object) = repo.find_object(id) else {
            continue;
        };
        // `deref_tag()` walks the whole tag chain; a chain that cannot be walked is
        // git's NULL and the `die()` above it.
        let Ok(peeled) = object.peel_tags_to_end() else {
            crate::git_fatal!("invalid object '{name}' given.");
        };
        match peeled.kind {
            // `repo_get_commit_tree()`: a commit *is* its tree from here on.
            gix::object::Kind::Commit | gix::object::Kind::Tree => trees += 1,
            gix::object::Kind::Blob => {
                if blobs >= 2 {
                    crate::git_fatal!("more than two blobs given: '{name}'");
                }
                blobs += 1;
            }
            gix::object::Kind::Tag => crate::git_fatal!("unhandled object '{name}' given."),
        }
    }
    if blobs > 0 {
        // `paths` is `rev.prune_data.nr`, the pathspec elements — the `--`-separated
        // tail included, which is why this is read after `trailing_paths` merged in.
        let two_blobs_no_path = trees == 0 && blobs == 2 && paths.is_empty();
        let blob_vs_file = trees == 0 && blobs == 1 && paths.len() == 1;
        if !two_blobs_no_path && !blob_vs_file {
            return Ok(usage_error());
        }
        // The two arms that *would* have rendered: `builtin_diff_blobs()` and
        // `builtin_diff_b_f()`. Neither is ported, so they are refused rather than
        // approximated with the tree machinery below, which cannot express them.
        bail!("unsupported: diff of a blob operand");
    }

    // Apply the `diff.algorithm` default only when no `--minimal`/`--histogram`/
    // `--patience`/`--diff-algorithm=` flag set the algorithm on the command line
    // (git's precedence).
    if algorithm.is_none() {
        if let Some(a) = config_algorithm {
            algorithm = Some(a);
        }
    }

    // `diff_setup_done()`: --name-only / --name-status / -s are mutually exclusive
    // and, when present, suppress every other output format.
    // `HAS_MULTI_BITS(options->output_format & (DIFF_FORMAT_NAME |
    // DIFF_FORMAT_NAME_STATUS | DIFF_FORMAT_CHECKDIFF | DIFF_FORMAT_NO_OUTPUT))`.
    // `--check` is `DIFF_FORMAT_CHECKDIFF`, one of the four, which is why the message
    // names it.
    if (fmt & (F_NAME | F_NAME_STATUS | F_NO_OUTPUT)).count_ones() + u32::from(check) > 1 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }
    // The clearing that follows the check names only three of the four: `-s` is an
    // assignment, so by the time it is read there is nothing left to clear, and a
    // format that arrives *after* it survives — measured, `git diff -s --stat` prints
    // the stat block and `git diff -s --raw` prints the raw records.
    if fmt & (F_NAME | F_NAME_STATUS) != 0 || check {
        fmt &= !(F_RAW | F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_PATCH);
    }
    // `--name-only`/`--name-status` suppress `--summary`, but `-s` does not.
    if fmt & (F_NAME | F_NAME_STATUS) != 0 {
        fmt &= !F_SUMMARY;
    }
    // `cmd_diff()` (builtin/diff.c): the patch default only fills an output format
    // that is still *empty*, and `--check` has already put `DIFF_FORMAT_CHECKDIFF`
    // there — which is why `git diff --check` prints checkdiff lines and no patch.
    if fmt == 0 && !check {
        fmt = F_PATCH;
    }

    // `builtin_diff_tree()` (builtin/diff.c:196):
    //
    // ```c
    // /*
    //  * We saw two trees, ent0 and ent1.  If ent1 is uninteresting,
    //  * swap them.
    //  */
    // if (ent1->item->flags & UNINTERESTING)
    //         swap = 1;
    // oid[swap] = &ent0->item->oid;
    // oid[1 - swap] = &ent1->item->oid;
    // ```
    //
    // The one place a `^` mark is visible in this command's *output* rather than only
    // in its diagnostics: `git diff HEAD ^HEAD~1` diffs `HEAD~1` against `HEAD`, not
    // the other way round. It reads `ent1` alone, so `^A ^B` swaps and `^A B` does not.
    if revs.len() == 2 && revs_uninteresting.get(1) == Some(&true) {
        revs.swap(0, 1);
    }

    // `cmd_diff()` rejects `--cached`/`--staged` with two or more revisions as a
    // usage error (129), printing the full usage stream — this is checked after
    // `setup_revisions()`, so an earlier ambiguous positional (128) wins.
    if cached && revs.len() >= 2 {
        return Ok(usage_error());
    }

    // `diff_set_mnemonic_prefix()` (diff.c:3720-3726):
    //
    // ```c
    // void diff_set_mnemonic_prefix(struct diff_options *options, const char *a, const char *b)
    // {
    //         if (!options->a_prefix)
    //                 options->a_prefix = a;
    //         if (!options->b_prefix)
    //                 options->b_prefix = b;
    // }
    // ```
    //
    // Only a slot `diff_setup()` left NULL is filled, so config and the
    // `--*-prefix` flags both win, per side. git calls it from whichever
    // comparison the command ended up running, and the letter names that
    // comparison's two ends:
    //
    //   * `run_diff_files()` — `i/` vs `w/` (diff-lib.c:121), which is bare
    //     `git diff`: the index against the worktree.
    //   * `run_diff_index()` — `c/` vs `i/` when `--cached`, `c/` vs `w/`
    //     otherwise (diff-lib.c:663), which is `git diff <commit>`.
    //   * `builtin_diff_no_index()` — `1/` vs `2/` (diff-no-index.c:425), handled
    //     in [`super::diff_no_index`].
    //   * `builtin_diff_b_f()` — `o/` vs `w/` (builtin/diff.c:100), a blob against
    //     a file; this port refuses a blob operand before reaching it.
    //
    // A tree-against-tree comparison — `git diff <commit> <commit>`, `A...B`, and
    // the three-or-more-revision combined form — has no such call, so it falls
    // through to `builtin_diff()`'s own `diff_set_mnemonic_prefix(o, "a/", "b/")`
    // (diff.c:3838) and stays `a/`/`b/` even with the key on. `--cc` output goes
    // further and never consults the option at all: `show_combined_header()` reads
    // `opt->a_prefix ? opt->a_prefix : "a/"` (combine-diff.c:931-932).
    //
    // Every letter here was read back from stock git 2.55.0 rather than inferred.
    {
        let (a, b): (&[u8], &[u8]) = if cached {
            (b"c/", b"i/")
        } else if revs.len() >= 2 {
            // Tree vs tree: no mnemonic call, so `builtin_diff()`'s fallback.
            (b"a/", b"b/")
        } else if revs.len() == 1 {
            (b"c/", b"w/")
        } else {
            (b"i/", b"w/")
        };
        // The call is unconditional in git — it is only ever a no-op because
        // `diff_setup()` already filled both slots whenever the key is off.
        src_prefix.get_or_insert_with(|| a.to_vec());
        dst_prefix.get_or_insert_with(|| b.to_vec());
    }
    // `builtin_diff()`'s own `diff_set_mnemonic_prefix(o, "a/", "b/")` (diff.c:3838):
    // the last resort for a slot nothing above claimed.
    let mut src_prefix = src_prefix.unwrap_or_else(|| b"a/".to_vec());
    let mut dst_prefix = dst_prefix.unwrap_or_else(|| b"b/".to_vec());

    // Three or more revisions request a dense combined ("--cc") diff of the first
    // revision against the rest, exactly like `builtin_diff_combined()`
    // (builtin/diff.c:211) handing them to `diff_tree_combined()`
    // (combine-diff.c:1491).
    //
    // `diff_tree_combined()` serves the requested formats from two different
    // sources. The stat family is "computed solely against the first parent"
    // (combine-diff.c:1370-1377, 1571-1584): an ordinary two-tree diff of parent 0
    // against the result, flushed through a *copy* of the diff options before
    // anything combined is printed. Raw/name/name-status and the patch come from
    // the combined path set instead. So the revisions are rewritten here to that
    // first-parent pair and the format mask is narrowed to the stat family, letting
    // the ordinary machinery below render the stat half; the combined half is
    // appended once it has.
    let combined = !cached && revs.len() >= 3;
    // `show_combined_header()` reads `opt->a_prefix`/`opt->b_prefix` as they were
    // configured, so the pair is captured before `-R` swaps them for the ordinary
    // pair machinery below.
    let mut combined_req = CombinedRequest {
        result: String::new(),
        parents: Vec::new(),
        fmt,
        reverse,
        a_prefix: src_prefix.clone(),
        b_prefix: dst_prefix.clone(),
    };
    if combined {
        combined_req.result = revs[0].clone();
        combined_req.parents = revs[1..].to_vec();
        revs = vec![combined_req.parents[0].clone(), combined_req.result.clone()];
        fmt &= F_NUMSTAT | F_DIFFSTAT | F_SHORTSTAT | F_SUMMARY | F_DIRSTAT;
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
        // No second revision can reach here: `cmd_diff()`'s `--cached` arity check
        // above returns `usage_error()` (129) for `revs.len() >= 2` before any of this
        // runs, so the tree-vs-index collection below always has exactly one endpoint.
        old_tree_id = Some(tree_id_for(&repo, revs.first())?);
        collect_tree_index(&repo, revs.first(), &mut deltas, ita_invisible)?;
        cache = repo.diff_resource_cache_for_tree_diff()?;
    } else if revs.len() == 2 {
        let old_tree = rev_object(&repo, revs[0].as_str())?.peel_to_tree()?;
        old_tree_id = Some(old_tree.id);
        let new_tree = rev_object(&repo, revs[1].as_str())?.peel_to_tree()?;
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
            collect_index_worktree(&repo, &workdir, &paths, &mut deltas, ita_invisible)?;
        }
        // The side the platform resolves by *reading the path* rather than by id.
        // `-R` swaps the two filespecs, so the worktree side becomes the pre-image
        // and the root has to travel with it.
        cache = repo.diff_resource_cache(
            Mode::ToGit,
            if reverse {
                WorktreeRoots {
                    old_root: Some(workdir.clone()),
                    new_root: None,
                }
            } else {
                WorktreeRoots {
                    old_root: None,
                    new_root: Some(workdir.clone()),
                }
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

    // `-R`: swap the two sides of every pair, before diffcore and every format sees
    // them, the way `diff_change()` does.
    if reverse {
        std::mem::swap(&mut src_prefix, &mut dst_prefix);
        let null = repo.object_hash().null();
        for d in &mut deltas {
            reverse_delta(d, null);
        }
    }

    // `diff_setup_done()` (diff.c:5288): `--find-copies-harder` on its own turns copy
    // detection on. A bare `-C -C` reaches here through the second `-C`, but a lone
    // `--find-copies-harder` sets only the flag, and without this the whole pass —
    // including the unmodified-source pairs below — would never run.
    if ro.find_copies_harder {
        ro.detect_rename = diffcore_rename::DETECT_COPY;
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

    // `diffcore_pickaxe()` (diff.c:7517), which `diffcore_std()` runs immediately
    // after the rename passes and before `diffcore_order()`.
    if let Some(pickaxe) = &pickaxe {
        let null = repo.object_hash().null();
        if let super::diff_pickaxe::Kind::ObjFind(ids) = pickaxe {
            // `pickaxe_match()` answers the objfind kind from the *recorded* ids and
            // returns before it would fill either filespec, so no content is read.
            deltas.retain(|d| {
                super::diff_pickaxe::objfind_hit(
                    ids,
                    // `DIFF_FILE_VALID(p->one)` and `p->one->oid`: a worktree pre-image
                    // (only `-R` produces one) carries `old_raw_id`, which is that field.
                    d.old.map(|(id, _)| if d.old_worktree { d.old_raw_id.unwrap_or(null) } else { id }),
                    // `DIFF_FILE_VALID(p->two)` and `p->two->oid`.
                    match &d.new {
                        NewSide::Absent => None,
                        NewSide::Blob(id, _) => Some(*id),
                        NewSide::Worktree(_) => Some(d.new_id.unwrap_or(null)),
                    },
                )
            });
        } else {
            // `-S`/`-G` fill both filespecs, so each surviving pair is read once here.
            let objects = repo.objects.clone();
            let pickaxe_workdir = repo.workdir().map(|p| p.to_owned());
            // `pickaxe_match()` (diffcore-pickaxe.c:148-170) resolves each side's
            // `get_textconv()` before it reads anything and then searches
            // `fill_textconv()`'s images, not the raw blobs:
            //
            // ```c
            // if (o->flags.allow_textconv) {
            //         textconv_one = get_textconv(o->repo, p->one);
            //         textconv_two = get_textconv(o->repo, p->two);
            // }
            // …
            // mf1.size = fill_textconv(o->repo, textconv_one, p->one, &mf1.ptr);
            // mf2.size = fill_textconv(o->repo, textconv_two, p->two, &mf2.ptr);
            // ```
            //
            // So `git -c diff.markdown.textconv='tr a-z A-Z <' log -S 'MORE PROSE'`
            // finds the commit and `-S 'more prose'` does not — measured against git
            // 2.55.0, which is the reverse of what searching the blobs answers.
            //
            // This is a second converter pass over the queue: `apply_textconv()`
            // below runs for the *patch*, which is downstream of the filter and sees
            // only the pairs that survived it. git pays the same price — the images
            // `pickaxe_match()` allocates are freed before `diff_flush()` converts
            // again.
            //
            // Not modelled here: `if (textconv_one == textconv_two &&
            // diff_unmodified_pair(p)) return 0;` (diffcore-pickaxe.c:160). It is
            // unreachable in this port — the tree and worktree walks only queue
            // changed pairs, and the unmodified pairs `--find-copies-harder` adds are
            // dropped again by `diffcore_rename()`'s write-back above.
            // One attribute stack for both jobs, the way `diff_filespec_load_driver()`
            // reaches one `attr_check`: `get_textconv()` needs it to find a converter
            // and `diff_filespec_is_binary()` needs it for the driver's `binary`
            // tri-state, whether or not conversion is allowed.
            let mut conv = super::cat_file::Textconv::new(&repo)?;
            let mut hits = Vec::with_capacity(deltas.len());
            for d in deltas.iter() {
                let one_raw = pickaxe_side(&objects, pickaxe_workdir.as_deref(), d, true)?;
                let two_raw = pickaxe_side(&objects, pickaxe_workdir.as_deref(), d, false)?;
                let one = pickaxe_textconv(&mut conv, allow_textconv, d.old_path(), one_raw)?;
                let two = pickaxe_textconv(&mut conv, allow_textconv, &d.path, two_raw)?;
                // `if ((o->pickaxe_opts & DIFF_PICKAXE_KIND_G) && !o->flags.text &&
                //  ((!textconv_one && diff_filespec_is_binary(o->repo, p->one)) ||
                //   (!textconv_two && diff_filespec_is_binary(o->repo, p->two))))
                //         return 0;` (diffcore-pickaxe.c:164-168).
                //
                // `-S` has no such guard: measured against git 2.55.0 over a pair
                // whose post-image carries a NUL byte, `diff --name-only -S NEEDLE`
                // listed it and `-G NEEDLE` did not.
                let skip_binary = matches!(pickaxe, super::diff_pickaxe::Kind::Grep(_))
                    && !ignore.text
                    && (pickaxe_binary_side(&repo, &mut conv, d.old_path(), &one)?
                        || pickaxe_binary_side(&repo, &mut conv, &d.path, &two)?);
                hits.push(!skip_binary && pickaxe.content_hit(one.side.as_deref(), two.side.as_deref()));
            }
            // `DIFF_OPT_PICKAXE_ALL`: one hit keeps the whole queue.
            if !(pickaxe_all && hits.iter().any(|h| *h)) {
                let mut it = hits.into_iter();
                deltas.retain(|_| it.next().unwrap_or(false));
            }
        }
    }

    // `--diff-filter`: keep only deltas whose status letter is selected.
    if let Some(filter) = &diff_filter {
        deltas.retain(|d| diff_filter_selected(filter, status_char(d)));
    }

    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    // `-O<file>` / `diff.orderFile` (`diffcore_order`): stably reorder the queue so
    // pairs whose path matches an earlier pattern in the order file come first. git
    // runs it last in `diffcore_std()`, after rename detection and `--diff-filter`.
    // `diffcore_order()` opens with `if (!q->nr) return;`, so an order file that
    // cannot be read is only fatal when there is a queue to reorder.
    if let (Some(of), false) = (&order_file, deltas.is_empty()) {
        let order = diff_files::read_order_file(of)?;
        deltas.sort_by_cached_key(|d| diff_files::match_order(&order, d.path.as_slice()));
    }

    // `diffcore_rotate()` (diff.c:6763): re-anchor the queue on the named pair —
    // `--skip-to` drops everything before it, `--rotate-to` wraps it to the end. The
    // comparison is against `p->two->path`, the repository-root relative name, so it
    // runs here rather than after `--relative` has shortened anything. The function
    // opens with `if (!q->nr) return;`, which is why `git diff --skip-to=nowhere` on a
    // clean tree prints nothing and exits 0 instead of dying.
    if let Some((is_skip, target)) = &skip_or_rotate {
        if !deltas.is_empty() {
            match deltas.iter().position(|d| d.path == *target) {
                Some(k) if *is_skip => deltas.drain(..k).for_each(drop),
                Some(k) => deltas.rotate_left(k),
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

    // `userdiff_find_by_path()` for every queued pair, on the repository-relative
    // names — `run_diff()` reads `attr_path` off `p->one->path` before
    // `strip_prefix()` shortens anything (diff.c:5036-5038), and `--relative`'s
    // shortening here happens later still.
    let mut drivers = DriverCache::new(&repo)?;
    resolve_drivers(&mut drivers, &mut deltas)?;

    // ---- analyze every delta once -----------------------------------------
    // `--quiet`/`-s` produce no output, so the patch bodies are never needed.
    let workdir = repo.workdir().map(|p| p.to_owned());
    // `diff_setup_done()` (diff.c:4899): the four whitespace-ignoring options and
    // `-I<re>` make "is there a change?" a question only the rendered content can
    // answer, so they raise `flags.diff_from_contents`. `--ignore-blank-lines` is
    // deliberately *not* on that list, and the difference is visible: measured
    // against 2.55.0, `git diff -w --raw` and `git diff -I<re> --raw` print a real
    // post-image object name for a worktree side while `git diff
    // --ignore-blank-lines --raw` prints all-zero.
    // `diff_setup_done()` (diff.c:5360): "External diffs could declare non-identical
    // contents equal", so `--exit-code`/`--quiet` beside an allowed external driver
    // also has to look at what the rendering pass found.
    let from_contents = ws != Whitespace::Keep
        || !ignore.lines.is_empty()
        || (allow_external && want_exit_code);
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
    // `fill_textconv()` is reached from `builtin_diff()` and `emit_rewrite_diff()`
    // and from nowhere else: `builtin_diffstat()`, `run_checkdiff()`,
    // `show_dirstat()` and every name/raw format read the filespec straight. So a
    // `--stat`-only or `--raw`-only run never starts a converter, and a converter
    // that would have died never gets the chance — measured against git 2.55.0,
    // `git -c diff.markdown.textconv=false diff --raw HEAD~2 HEAD~1` prints the raw
    // record and exits 0 while the same command with `-p` is fatal.
    if allow_textconv && (fmt & F_PATCH != 0 || exit_needs_patch) {
        apply_textconv(&repo, &mut drivers, &mut deltas, repo.workdir())?;
    }

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
            reverse,
            want_dirstat && !dirstat.by_line && !dirstat.by_file,
            // Only a rendered patch carries the payload, so a `--stat`-only run with
            // `--binary` reads nothing extra.
            binary && want_patch,
            func_context,
            &ignore,
        )?;
    }

    // `external_diff()` (diff.c:5026), read once for the whole command as git's
    // function-scoped static is.
    let ext_program = match allow_external {
        true => external_diff_program(&repo)?,
        false => None,
    };

    // `run_diff()` splits a type change into a deletion patch and a creation patch
    // (diff.c:5052) — but only for the patch formats, so the split lives beside
    // `deltas` rather than in it. The entry is `None` for every pair git renders
    // whole, which is all of them in the overwhelmingly common case; the two halves
    // are analyzed here so the patch loop has hunks for each.
    let mut splits: Vec<Option<(Delta, Analysis, Delta, Analysis)>> = Vec::new();
    // `if (!pgm && ... (S_IFMT & one->mode) != (S_IFMT & two->mode))`: with an
    // environment or configured external program in play the pair goes to the driver
    // whole. A `diff.<driver>.command` reached through the path's attribute does not
    // suppress the split — `run_diff()` tests `pgm` before either half re-resolves
    // the attribute.
    if want_patch && ext_program.is_none() && deltas.iter().any(Delta::type_changed) {
        // The deletion half's post-image is git's invalid filespec — no content at
        // all. A worktree diff's blob platform resolves a null object id by *reading
        // the path*, which for a type change is the file that replaced the old one,
        // so that half is analyzed through a platform with no worktree root, where a
        // null id really is empty. The creation half keeps the caller's platform,
        // since its post-image is exactly the worktree file.
        let mut null_cache = repo.diff_resource_cache_for_tree_diff()?;
        // `-R` swaps which half owns the worktree: the deletion half's *pre*-image is
        // then the file, and the creation half's pre-image is the invalid filespec.
        let reversed_worktree = reverse && worktree_mode;
        for delta in &deltas {
            splits.push(match split_type_change(delta) {
                Some((del, add)) => {
                    let (del_an, add_an) = if reversed_worktree {
                        let del_an = analyze(
                            &mut cache,
                            &repo.objects,
                            &del,
                            ctx,
                            ws,
                            indent_heuristic,
                            hash_kind,
                            workdir.as_deref(),
                            true,
                            algorithm,
                            None,
                            false,
                            binary,
                            func_context,
                            &ignore,
                        )?;
                        let add_an = analyze(
                            &mut null_cache,
                            &repo.objects,
                            &add,
                            ctx,
                            ws,
                            indent_heuristic,
                            hash_kind,
                            None,
                            true,
                            algorithm,
                            None,
                            false,
                            binary,
                            func_context,
                            &ignore,
                        )?;
                        (del_an, add_an)
                    } else {
                        let del_an = analyze(
                            &mut null_cache,
                            &repo.objects,
                            &del,
                            ctx,
                            ws,
                            indent_heuristic,
                            hash_kind,
                            None,
                            true,
                            algorithm,
                            None,
                            false,
                            binary,
                            func_context,
                            &ignore,
                        )?;
                        let add_an = analyze(
                            &mut cache,
                            &repo.objects,
                            &add,
                            ctx,
                            ws,
                            indent_heuristic,
                            hash_kind,
                            workdir.as_deref(),
                            true,
                            algorithm,
                            None,
                            false,
                            binary,
                            func_context,
                            &ignore,
                        )?;
                        (del_an, add_an)
                    };
                    Some((del, del_an, add, add_an))
                }
                None => None,
            });
        }
    }
    /// The pairs a patch format renders for one queued delta: the delta itself, or
    /// the deletion/creation halves of a type change.
    fn patch_steps<'a>(
        delta: &'a Delta,
        an: &'a Analysis,
        split: Option<&'a (Delta, Analysis, Delta, Analysis)>,
    ) -> Vec<(&'a Delta, &'a Analysis)> {
        match split {
            Some((del, del_an, add, add_an)) => vec![(del, del_an), (add, add_an)],
            None => vec![(delta, an)],
        }
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
        text: ignore.text,
        irreversible_delete,
        z,
        src_prefix,
        dst_prefix,
        indicators,
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
    //
    // `diff_setup_done()` lets `DIFF_FORMAT_CHECKDIFF` clear every other output
    // format (diff.c), and `diff_tree_combined()` in turn never looks at
    // `DIFF_FORMAT_CHECKDIFF` — so a combined diff asked for `--check` prints
    // nothing at all, whatever else was asked for alongside it, and exits 0.
    if check && combined {
        return Ok(ExitCode::SUCCESS);
    }
    if check {
        let mut found = false;
        let mut buf: Vec<u8> = Vec::new();
        for (delta, analysis) in deltas.iter().zip(analyses.iter()) {
            found |= report_whitespace_to(&mut buf, delta, analysis, ws_rule, &colors);
        }
        // `checkdiff_consume()` writes through `emit_line()` like every other
        // format, so `--line-prefix` reaches these lines too.
        let buf = apply_line_prefix(buf, &line_prefix);
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&buf);
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
    // Where each external driver's stdout landed in `out`, so `--line-prefix` can skip
    // it: git's child writes past `emit_line()` entirely.
    let mut ext_spans: Vec<(usize, usize)> = Vec::new();
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
                    // The quiet render above is `run_diff()`, so it has already left
                    // `diff_fill_oid_info()`'s hash on a worktree side.
                    render_raw(&mut out, delta, fmt, &r, names_need_patch.then(|| &analyses[i]));
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
                diffstat::show_stats(&mut out, &stat_rows(&stat_pairs, compact_summary), &sw, &colors);
            }
            if fmt & F_SHORTSTAT != 0 {
                diffstat::show_shortstats(&mut out, &stat_rows(&stat_pairs, compact_summary));
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
            // `diff_flush()` (diff.c:7243): `if (output_format & DIFF_FORMAT_SUMMARY
            // && !is_summary_empty(q))` — an empty summary writes nothing and does
            // not raise `separator`, so `-p --summary` over a plain content change
            // runs the patch straight on.
            let before = out.len();
            render_summary(&mut out, &deltas);
            separator |= out.len() != before;
        }

        if fmt & F_PATCH != 0 {
            if separator {
                // `DIFF_SYMBOL_SEPARATOR` is `o->line_termination` (diff.c:1436-1440),
                // so `-z` separates the earlier block from the patch with a NUL
                // instead of a blank line. The `diff_line_prefix(o)` in front of it
                // arrives with the whole-buffer `apply_line_prefix()` below.
                out.push(if r.z { 0 } else { b'\n' });
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
                indicators,
                ..Default::default()
            };
            let mut plain: Vec<u8> = Vec::new();
            let mut files: Vec<diff_color::FilePaint> = Vec::new();
            // `init_diff_words_data()` compiles the pair's driver word regex once
            // per pair; one compilation per distinct pattern is the same answer.
            let mut word_res: std::collections::HashMap<String, std::sync::Arc<regex::bytes::Regex>> =
                std::collections::HashMap::new();
            let want_driver_words = extra.wants_driver_word_regex();
            // `--submodule=log`/`=diff` write their lines through
            // `diff_emit_submodule_*()`, which paints each one itself instead of
            // handing it to `fn_out_consume()`. Draining the assembled patch at
            // every such pair keeps both the order and those colours intact;
            // `--color-moved` is the only thing a split buffer would cost, and this
            // command rejects it outright.
            let sub_abbrev = crate::abbrev::configured_abbrev(&repo, repo.object_hash().len_in_hex());
            // `run_diff_cmd()` reaches an external program only when one is
            // configured somewhere: the environment, `diff.external`, or a driver a
            // queued path's `diff` attribute names. With none of those the whole
            // apparatus — a second gitattributes stack included — is never built.
            let want_ext = allow_external
                && (ext_program.is_some()
                    || deltas.iter().any(|d| {
                        d.drivers.one.as_ref().is_some_and(|x| x.settings.external.is_some())
                    }));
            let ext_drivers = match want_ext {
                true => Some(std::cell::RefCell::new(super::cat_file::Textconv::new(&repo)?)),
                false => None,
            };
            let ext = ext_drivers
                .as_ref()
                .map(|d| ext_context(d, ext_program.clone()));
            // `GIT_DIFF_PATH_TOTAL`: `q->nr`, the queue as `diff_flush()` received it.
            let ext_total = deltas.len();
            let ext_naming = super::diff_pairs::IndexNaming {
                base_abbrev: r.abbrev,
                full_index: r.full_index,
                abbrev_explicit: abbrev_explicit.then_some(abbrev),
            };
            let null_id = repo.object_hash().null();
            for (i, (queued, queued_an)) in deltas.iter().zip(&analyses).enumerate() {
                if !queued.unmerged && unmerged.contains(&queued.path) {
                    continue;
                }
                // The submodule branch lives in `builtin_diff()`, downstream of
                // `run_diff()`'s split, so each half is tested on its own: the
                // deletion half of a gitlink-to-blob change is a submodule pair even
                // though the whole pair is not.
                for (delta, an) in patch_steps(queued, queued_an, splits.get(i).and_then(|s| s.as_ref()))
                {
                    // `run_diff_cmd()` (diff.c:4969) hands the pair to the program and
                    // returns: the driver's stdout *is* this pair's section, written
                    // straight to git's own output descriptor, so it is never coloured
                    // and never carries `--line-prefix`. That is upstream of
                    // `builtin_diff()`, so it also pre-empts the submodule branches
                    // below.
                    let pgm = match (&ext, delta.unmerged) {
                        (Some(ctx), false) => external_for_pair(delta, ctx.env.as_ref()),
                        _ => None,
                    };
                    if let (Some(ctx), Some(pgm)) = (ext.as_ref(), pgm) {
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
                        let run = super::diff_pairs::run_external_diff(
                            &pgm,
                            &repo,
                            ctx,
                            &ext_pair(delta, an.old_id, an.new_id, null_id),
                            &ext_naming,
                            ext_total,
                            true,
                        )
                        .map_err(crate::fatal::die)?;
                        found_changes |= run.found_changes;
                        let at = out.len();
                        out.extend_from_slice(&run.stdout);
                        ext_spans.push((at, out.len()));
                        // `die(_("external diff died, stopping at %s"))` fires only
                        // after the child's own output has gone out, which it already
                        // has above.
                        if let Some(msg) = run.died {
                            // git's child wrote straight to the output descriptor, so
                            // everything printed before the failure is already out.
                            let done = apply_line_prefix_except(
                                std::mem::take(&mut out),
                                &line_prefix,
                                &ext_spans,
                            );
                            let mut stdout = std::io::stdout().lock();
                            stdout.write_all(&done)?;
                            stdout.flush()?;
                            return Err(crate::fatal::die(msg));
                        }
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
                        files.push(diff_color::FilePaint {
                            ws_rule,
                            blank_at_eof: an.blank_at_eof,
                            word_regex: driver_word_regex(
                                &mut word_res,
                                &delta.drivers,
                                want_driver_words,
                            )?,
                        });
                        // Every `builtin_diff()` arm that emits a header or a hunk sets
                        // `o->found_changes`, so having written anything is the answer.
                        found_changes = true;
                    }
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
        'pairs: for (i, (queued, queued_an)) in deltas.iter().zip(&analyses).enumerate() {
            // `diff_flush_patch_quietly()` goes through `run_diff()` too, so a type
            // change is asked about as its two halves.
            for (delta, an) in patch_steps(queued, queued_an, splits.get(i).and_then(|s| s.as_ref()))
            {
                if pair_reports_change(&mut sink, &repo, delta, an, ctx, &r, submodule_format)? {
                    found_changes = true;
                    break 'pairs;
                }
            }
        }
    }

    // `diff.suppressBlankEmpty`: `fn_out_consume()` rewrites any emitted line that
    // is exactly `" \n"` (an empty context line) to `"\n"` before it is prefixed.
    // Only the ordinary patch path is fed through that chain — `dump_sline()`
    // prints the combined patch straight out — so the rewrite runs before the
    // combined half is appended, not after.
    let out = apply_suppress_blank_empty(out, suppress_blank_empty);

    // `--line-prefix`: `diff_line_prefix()` prepends the string to every emitted
    // line, so a whole-buffer pass over the newline-terminated output reproduces it
    // for the ordinary half. The combined half prefixes itself instead, because
    // `show_combined_header()` leaves the prefix off two of the lines it prints.
    let mut out = apply_line_prefix_except(out, &line_prefix, &ext_spans);

    // The combined half of `diff_tree_combined()` (combine-diff.c:1611-1631): the
    // raw/name formats and the patch are served from the path set the result shares
    // with every parent, after the first-parent stat formats above.
    //
    // `--quiet` is `diff_setup_done()`'s `flags.quick`, which replaces the whole
    // output format with `DIFF_FORMAT_NO_OUTPUT` (diff.c) — the combined half has
    // nothing to print either.
    if combined && !quiet {
        emit_combined(&mut out, &repo, &combined_req, &paths, ctx, &r, &mut separator, &line_prefix, &colors)?;
    }

    // `--output=<file>` pointed git's output `FILE*` at a file back at parse time;
    // every rendered byte goes there instead of to stdout, while the exit status is
    // still computed below.
    match output_file {
        Some(mut f) => {
            f.write_all(&out)?;
            f.flush()?;
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(&out)?;
            stdout.flush()?;
        }
    }
    // `diff_result_code()` calls `diff_warn_rename_limit()` after stdout is flushed,
    // so the `-l` / `diff.renameLimit` warnings land after the diff itself.
    // A combined diff runs every diffcore pass on `diff_tree_combined()`'s *copy* of
    // the options (combine-diff.c:1524), so the limit it needed never reaches the
    // caller's and no warning is printed.
    if !combined {
        rename_warnings.emit("diff.renameLimit");
    }
    // `--exit-code`/`--quiet`: exit 1 when any difference was reported.
    //
    // `diff_change()` sets `has_changes` as each pair is queued, so normally a
    // non-empty queue is the whole answer. A whitespace-ignoring option turns on
    // `diff_from_contents` (diff.c:4899) and that queue-time shortcut is skipped:
    // `diff_flush()` re-derives `has_changes` from `found_changes` instead
    // (diff.c:6861), which only the formats that emitted something ever set.
    //
    // Neither happens for a combined diff: every pair it queues is queued on the
    // copy of the options (combine-diff.c:1524), so the caller's `has_changes` stays
    // clear and `git diff --exit-code <a> <b> <c>` exits 0 however much it printed.
    if want_exit_code && !combined {
        let changed = if from_contents { found_changes } else { !deltas.is_empty() };
        if changed {
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Reverse (`-R`) one pair: the new side becomes the old side and vice-versa,
/// exactly as `diff_change()`/`diff_addremove()` (diff.c) swap the two filespecs
/// before queueing them.
///
/// git swaps `oid_valid` along with the ids, which is how a worktree post-image —
/// a filespec with no id, read by path — becomes a worktree *pre*-image. The same
/// bit is [`Delta::old_worktree`] here, and the blob platform's worktree root
/// moves to the old side with it (see [`diff`]).
fn reverse_delta(d: &mut Delta, null: ObjectId) {
    let (new_as_old, new_was_worktree) = match &d.new {
        NewSide::Blob(id, k) => (Some((*id, *k)), false),
        // A worktree gitlink is the one worktree side that knows an id — the commit
        // the submodule has checked out, which `run_diff_files()` keeps beside the
        // invalid filespec — so the swap carries it onto the pre-image. The side is
        // still worktree-backed (its `oid_valid` is false, which is what `--raw`
        // reports); a gitlink simply never goes through the blob platform.
        NewSide::Worktree(EntryKind::Commit) => {
            (Some((d.new_commit.unwrap_or(null), EntryKind::Commit)), true)
        }
        // Every other post-image had no id, so the pre-image it becomes has none.
        NewSide::Worktree(k) => (Some((null, *k)), true),
        NewSide::Absent => (None, false),
    };
    let old_as_new = match (d.old, d.old_worktree) {
        (Some((_, k)), true) => NewSide::Worktree(k),
        (Some((id, k)), false) => NewSide::Blob(id, k),
        (None, _) => NewSide::Absent,
    };
    d.old = new_as_old;
    d.old_worktree = new_was_worktree;
    d.new = old_as_new;
    // The id `--raw` printed for the post-image now belongs to the pre-image, and
    // the new post-image prints its own — a real one when it came out of the object
    // database, none for a side that was only ever a file.
    d.old_raw_id = d.new_id;
    d.new_id = match d.new {
        NewSide::Blob(id, _) => Some(id),
        _ => None,
    };
    // `SWAP(old_dirty_submodule, new_dirty_submodule)`: the `-dirty` marker belongs
    // to the worktree side wherever it lands.
    std::mem::swap(&mut d.old_dirty_submodule, &mut d.dirty_submodule);
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
    // The same for the pre-image: `hash_filespec()` may give a worktree side a real
    // id mid-pass, so `oid_valid` alone no longer says where the content lives.
    let mut old_worktree_specs: BTreeSet<usize> = BTreeSet::new();
    // …and the printable id such a side may still carry, which no filespec has room
    // for either.
    let mut old_raw_ids: BTreeMap<usize, ObjectId> = BTreeMap::new();
    let mut submodule_state: BTreeMap<usize, (u8, Option<ObjectId>, Option<ObjectId>)> =
        BTreeMap::new();

    for d in deltas.drain(..) {
        if d.unmerged {
            held.push(d);
            continue;
        }
        let one = match d.old {
            Some((id, k)) => {
                let mut spec =
                    diffcore_rename::FileSpec::new(d.path.clone(), kind_mode(k), id, !d.old_worktree);
                spec.dirty_submodule = d.old_dirty_submodule;
                q.add_spec(spec)
            }
            None => q.add_spec(diffcore_rename::FileSpec::absent(d.path.clone())),
        };
        if d.old_worktree {
            old_worktree_specs.insert(one);
            if let Some(id) = d.old_raw_id {
                old_raw_ids.insert(one, id);
            }
        }
        let two = match &d.new {
            NewSide::Absent => q.add_spec(diffcore_rename::FileSpec::absent(d.path.clone())),
            NewSide::Blob(id, k) => {
                let mut spec =
                    diffcore_rename::FileSpec::new(d.path.clone(), kind_mode(*k), *id, true);
                spec.dirty_submodule = d.dirty_submodule;
                q.add_spec(spec)
            }
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
            old_worktree: old_worktree_specs.contains(&p.one),
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
            // `hash_filespec()` may have given the worktree *pre*-image a real id
            // too, and `diff_flush_raw()` prints that one; otherwise whatever the
            // side arrived with (a submodule sitting where the index says).
            old_raw_id: (one.valid() && one.oid_valid)
                .then_some(one.oid)
                .or_else(|| old_raw_ids.get(&p.one).copied()),
            old_dirty_submodule: q.specs[p.one].dirty_submodule,
            dirty_submodule: sub_state.map(|(d, _, _)| d).unwrap_or(0),
            new_commit: sub_state.and_then(|(_, c, _)| c),
            drivers: PairDrivers::default(),
            textconv: None,
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
pub(crate) fn diff_filter_selected(filter: &[u8], status: u8) -> bool {
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
    ita_invisible: bool,
) -> Result<()> {
    let tree_id = tree_id_for(repo, spec)?;
    let index = repo.index_or_load_from_head()?;
    let start = deltas.len();
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
    // `do_oneway_diff()` (diff-lib.c) nulls an `add -N` entry out only under
    // `ita_invisible_in_index`; with `--ita-visible-in-index` the entry is an
    // ordinary index record and the pair is the empty blob it names. gitoxide's
    // index diff drops every intent-to-add entry unconditionally
    // (`gix-diff/src/index/function.rs:283`), so the visible half is rebuilt here.
    if !ita_invisible {
        for e in index.entries() {
            if !e.flags.contains(gix::index::entry::Flags::INTENT_TO_ADD) {
                continue;
            }
            let Some(kind) = index_mode_kind(e.mode) else { continue };
            let path = e.path(&index).to_owned();
            let old = tree_entry(&tree, &path)?;
            if old == Some((e.id, kind)) {
                continue;
            }
            deltas.push(Delta::plain(path, old, NewSide::Blob(e.id, kind)));
        }
        deltas[start..].sort_by(|a, b| a.path.cmp(&b.path));
    }
    for path in unmerged_paths(&index) {
        let old = tree_entry(&tree, &path)?;
        deltas.push(Delta {
            path,
            old,
            new: NewSide::Absent,
            old_worktree: false,
            unmerged: true,
            stages: None,
            src_path: None,
            score: 0,
            status: 0,
            new_id: None,
            old_raw_id: None,
            old_dirty_submodule: 0,
            dirty_submodule: 0,
            new_commit: None,
            drivers: PairDrivers::default(),
            textconv: None,
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
    let tree_id = rev_object(repo, spec)?.peel_to_tree()?.id;
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
                if let Some((path, new, dirty, head)) = worktree_new_side(repo.workdir(), item)? {
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
    ita_invisible: bool,
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
        if let Some((path, new, dirty, head)) = worktree_new_side(Some(workdir), item)? {
            // A worktree entry with no index counterpart cannot happen here (the
            // dirwalk is off), so the old side is always the index entry.
            let entry = index
                .entry_by_path(path.as_bstr())
                .ok_or_else(|| anyhow::anyhow!("no index entry for {path:?}"))?;
            let old_kind = index_mode_kind(entry.mode).unwrap_or(EntryKind::Blob);
            // `run_diff_files()` (diff-lib.c): under `ita_invisible_in_index` an
            // `add -N` entry is reported through `diff_addremove('+', …)` — an
            // addition with no pre-image at all — rather than as a modification of
            // the empty blob the entry names. `--ita-visible-in-index` is the other
            // half: the entry stands, and the pair is the ordinary modification.
            // Only the entry gix reported *as* intent-to-add: a removed one took
            // `check_removed()`'s branch above the flag test in `run_diff_files()`
            // and is a plain deletion against the empty blob the entry names.
            let ita = ita_invisible
                && matches!(new, NewSide::Worktree(_))
                && entry.flags.contains(gix::index::entry::Flags::INTENT_TO_ADD);
            let old = (!ita).then_some((entry.id, old_kind));
            let mut delta = Delta::plain(path, old, new);
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
            old_worktree: false,
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
            old_raw_id: None,
            old_dirty_submodule: 0,
            dirty_submodule: 0,
            new_commit: None,
            drivers: PairDrivers::default(),
            textconv: None,
        });
        if let (Some(s), Some(k)) = (stages, wt_kind) {
            deltas.push(Delta::plain(path, Some((s.ours.0, s.ours.1)), NewSide::Worktree(k)));
        }
    }
    Ok(())
}

/// The post-image of an index-vs-worktree type change: the worktree's own type,
/// which `run_diff_files()` derives with `ce_mode_from_stat()` and `run_diff()`
/// then renders as a deletion followed by a creation.
///
/// A directory never reaches here. gix-status intercepts `metadata.is_dir()`
/// (`gix-status/src/index_as_worktree/function.rs:379-393`) before
/// `change_to_match_fs()` runs and reports `Change::Removed` for every non-submodule
/// entry, so `change_to_match_fs()`'s `stat.is_dir() => Mode::COMMIT` arm
/// (`gix-index/src/entry/mode.rs:60-61`) is unreachable from this path and
/// `new_kind` is never `Commit` unless `old_kind` already was. The directory case
/// is handled where gix raises it, in [`removed_or_gitlink`].
fn worktree_type_change(
    rela_path: BString,
    worktree_mode: gix::index::entry::Mode,
) -> Result<Option<(BString, NewSide, u8, Option<ObjectId>)>> {
    let Some(new_kind) = index_mode_kind(worktree_mode) else {
        bail!(
            "worktree mode {:o} at {rela_path:?} has no tree-entry equivalent",
            worktree_mode.bits()
        )
    };
    Ok(Some((rela_path, NewSide::Worktree(new_kind), 0, None)))
}

/// `check_removed()` (diff-lib.c:22) deciding whether a vanished blob really is a
/// deletion:
///
/// ```c
/// if (S_ISDIR(st->st_mode)) {
///         struct object_id sub;
///         if (!S_ISGITLINK(ce->ce_mode) &&
///             resolve_gitlink_ref(ce->name, "HEAD", &sub))
///                 return 1;
/// }
/// return 0;
/// ```
///
/// A tracked blob whose name a *directory* has taken is only removed when that
/// directory is not a repository. When it is one, `check_removed()` returns 0 and
/// `run_diff_files()` gives the pair `ce_mode_from_stat()`'s `S_IFGITLINK`, so the
/// change is a type change to `160000` — `T` in `--raw`, `mode change 100644 =>
/// 160000` in `--summary`, and a deletion section followed by a
/// `Subproject commit <oid>` creation in the patch.
///
/// gix-status cannot make that call: it reports `Change::Removed` for any
/// non-submodule entry whose path is a directory, so the lookup happens here.
/// `resolve_gitlink_ref()` is `gix::open(<dir>)` + `head_id()` — the same two calls
/// [`super::commit`] uses to stage a directory that turned out to be a submodule.
/// A repository whose `HEAD` is unborn resolves to nothing, and git treats that as
/// a plain removal too.
fn removed_or_gitlink(
    workdir: Option<&std::path::Path>,
    rela_path: BString,
) -> Option<(BString, NewSide, u8, Option<ObjectId>)> {
    let removed = (rela_path.clone(), NewSide::Absent, 0, None);
    let Some(workdir) = workdir else { return Some(removed) };
    let abs = workdir.join(gix::path::from_bstr(rela_path.as_bstr()).as_ref());
    if !abs.is_dir() {
        return Some(removed);
    }
    match gix::open(&abs)
        .ok()
        .and_then(|sub| sub.head_id().ok().map(|h| h.detach()))
    {
        // `run_diff_files()` leaves the worktree gitlink's filespec invalid, so the
        // post-image keeps no `--raw` id; the commit rides along for the patch.
        Some(head) => Some((rela_path, NewSide::Worktree(EntryKind::Commit), 0, Some(head))),
        None => Some(removed),
    }
}

/// The "new" side an index-vs-worktree status item implies, together with the
/// `DIRTY_SUBMODULE_*` bits and the checked-out submodule commit it carries, or
/// `None` when the item is not a change.
fn worktree_new_side(
    workdir: Option<&std::path::Path>,
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
            // The gitlink's path is no longer a directory. `ce_mode_from_stat()`
            // gives the pair the worktree's own type and `run_diff()` then splits it.
            EntryStatus::Change(Change::Type { worktree_mode }) => {
                worktree_type_change(rela_path, worktree_mode)?
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
        // gix reports every non-submodule entry whose path became a directory as a
        // removal; `check_removed()` only agrees when the directory is not a
        // repository. See [`removed_or_gitlink`].
        EntryStatus::Change(Change::Removed) => removed_or_gitlink(workdir, rela_path),
        EntryStatus::Change(Change::Type { worktree_mode }) => {
            worktree_type_change(rela_path, worktree_mode)?
        }
        // A conflicted path still has worktree content; only `git diff` with no
        // revision treats it specially, and that caller intercepts it first.
        EntryStatus::Conflict { .. } => Some((rela_path, NewSide::Worktree(old_kind), 0, None)),
        // gix reports an `add -N` entry as its own status rather than as a
        // modification. The file is known to exist — the removal and directory
        // tests both run ahead of the flag check
        // (`gix-status/src/index_as_worktree/function.rs:415-442`) — so its
        // post-image is the worktree's own content, at `ce_mode_from_stat()`'s mode.
        EntryStatus::IntentToAdd => workdir
            .and_then(|root| worktree_kind(root, &rela_path))
            .map(|k| (rela_path, NewSide::Worktree(k), 0, None)),
        // Submodule content modification and stat-only refreshes produce no
        // textual diff.
        EntryStatus::Change(Change::SubmoduleModification(_)) | EntryStatus::NeedsUpdate(_) => None,
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

/// One revision spec as the object `setup_revisions()` pended for it.
///
/// `get_oid_basic()` reads a `<ref>@{…}` operand with `repo_dwim_log()` and then
/// `read_ref_at()` (`object-name.c:742-789`), never with the revspec grammar, and
/// the two disagree: gitoxide answers with the selected entry's raw *new* id where
/// `read_ref_at()` keeps the ref's current value — the null id for the record a
/// `git branch -m` round trip leaves behind. See [`crate::objname::reflog_oid`].
/// The operand loop resolves the same way, so this only keeps the second look-up
/// (the one that wants a tree) in step with the first.
fn rev_object<'r>(repo: &'r gix::Repository, spec: &str) -> Result<gix::Object<'r>> {
    if crate::objname::is_reflog_operand(spec) {
        if let Some(id) = crate::objname::reflog_oid(repo, spec) {
            return Ok(repo.find_object(id)?);
        }
    }
    Ok(repo.rev_parse_single(spec)?.object()?)
}

/// A single revision spec into a tree id, defaulting to `HEAD^{tree}` (or the empty
/// tree if `HEAD` is unborn) when no spec is given.
fn tree_id_for(repo: &gix::Repository, spec: Option<&String>) -> Result<ObjectId> {
    Ok(match spec {
        Some(s) => rev_object(repo, s.as_str())?.peel_to_tree()?.id,
        None => repo.head_tree_id_or_empty()?.detach(),
    })
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
/// Which slot of `options->output_indicators[]` a `--output-indicator-*` name
/// names, in git's `OUTPUT_INDICATOR_NEW`/`_OLD`/`_CONTEXT` order (diff.h:378-380).
fn indicator_slot(name: &str) -> Option<usize> {
    match name {
        "--output-indicator-new" => Some(0),
        "--output-indicator-old" => Some(1),
        "--output-indicator-context" => Some(2),
        _ => None,
    }
}

/// `diff_opt_char()` (diff.c:5593): store `arg[0]`, refusing anything longer than
/// one byte. The empty string stores the NUL that terminates it, which
/// `emit_line_0()` then declines to write.
fn set_indicator(
    indicators: &mut (u8, u8, u8),
    name: &str,
    val: &str,
) -> std::result::Result<(), String> {
    let Some(slot) = indicator_slot(name) else {
        return Ok(());
    };
    if val.len() > 1 {
        return Err(format!("error: {} expects a character, got '{val}'", &name[2..]));
    }
    let c = val.as_bytes().first().copied().unwrap_or(0);
    match slot {
        0 => indicators.0 = c,
        1 => indicators.1 = c,
        _ => indicators.2 = c,
    }
    Ok(())
}

/// `func_by_opt()` (diff-merges.c:68-86): the `--diff-merges=<v>` values git maps to
/// a setup function. Anything else is `die("invalid value for '%s': '%s'")`.
fn is_diff_merges_value(v: &str) -> bool {
    matches!(
        v,
        "off"
            | "none"
            | "1"
            | "first-parent"
            | "separate"
            | "c"
            | "combined"
            | "cc"
            | "dense-combined"
            | "r"
            | "remerge"
            | "m"
            | "on"
    )
}

pub(crate) fn is_known_option(arg: &str) -> bool {
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

/// What `git diff` says about a value-taking option whose value slot is empty —
/// because the option stood last on the line, or because the next slot held the
/// `--` that `setup_revisions()` cuts the option region at.
///
/// Three parsers own these options and each words the refusal its own way, which
/// is why this is one function rather than an `eprintln!` at each site:
///
///   * parse-options' `get_arg()` (parse-options.c:59-60) — ``error: <name>
///     requires a value``, no usage block, exit **129**. That covers everything
///     in `diff_opt_parse()`'s table, short (`-S`, `-l`, `-O`) and long alike;
///     `optname()` decides between ``switch `<c>'`` and ``option `<name>'`` from
///     the spelling, which is what [`diff_color::missing_value`] renders.
///   * `parse_long_opt()` (`revision.c`) for `--diff-merges`, whose
///     `die("Option '--%s' requires a value", ...)` is a `fatal:` at exit **128**.
///   * `handle_revision_opt()`'s own `-n` check (`revision.c`), which is neither:
///     ``error: -n requires an argument`` — an `error:` line at exit **128**.
///
/// All three are the observed stderr and status of git 2.55.0.
///
/// `-l`'s and `-n`'s value, wherever it was written: glued on (`-l5`, `-n5`) or in
/// the next argv slot (`-l 5`, `-n 5`). Both spellings reach the same parser, so
/// both reach the same refusal.
///
/// `-l` is `OPT_INTEGER('l', NULL, &options->rename_limit, ...)`, so the
/// diagnostic is parse-options' integer wording at 129 — ``switch `l' expects an
/// integer value with an optional k/m/g suffix``.
fn parse_rename_limit(value: &str) -> Result<i64, ExitCode> {
    crate::optint::integer(&crate::optint::short_opt('l'), value).map_err(|e| {
        eprintln!("error: {e}");
        ExitCode::from(129)
    })
}

/// `-n`'s value through `parse_count()` (`revision.c`), whose `die()` is
/// `fatal: '<value>': not an integer` at 128. The parsed count is discarded:
/// `cmd_diff()` diffs the pending trees rather than walking, so `--max-count`
/// changes nothing it prints.
fn check_max_count(value: &str) -> Result<(), ExitCode> {
    match super::log::parse_max_count(value) {
        Ok(_) => Ok(()),
        Err(()) => {
            eprintln!("fatal: '{value}': not an integer");
            Err(ExitCode::from(128))
        }
    }
}

fn missing_value_refusal(flag: &str) -> ExitCode {
    if flag == "--diff-merges" {
        eprintln!("fatal: Option '--diff-merges' requires a value");
        return ExitCode::from(128);
    }
    if flag == "-n" {
        eprintln!("error: -n requires an argument");
        return ExitCode::from(128);
    }
    eprintln!("error: {}", diff_color::missing_value(flag));
    ExitCode::from(129)
}

/// `git_xmerge_config()`'s `diff.algorithm` arm:
///
/// ```c
/// if (!strcmp(var, "diff.algorithm")) {
///         int value_i = parse_algorithm_value(value);
///         if (value_i < 0)
///                 return error(_("unknown value for config '%s': %s"), var, value);
///         ...
/// }
/// ```
///
/// The same [`super::diff_optval::parse_algorithm_value`] table the flag reads, so
/// the config accepts exactly the four names, case-insensitively. An unrecognized
/// one is a hard config error (git exits 128) even when a CLI flag would have
/// overridden it.
pub(crate) fn parse_config_algorithm(name: &gix::bstr::BStr) -> Result<gix::diff::blob::Algorithm> {
    match super::diff_optval::parse_algorithm_value(&name.to_str_lossy()) {
        Some(alg) => Ok(alg),
        None => crate::git_fatal!("diff algorithm {:?} is not available", name.to_str_lossy()),
    }
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
    let mut drivers = DriverCache::new(repo)?;
    commit_patch_with(repo, &mut cache, &mut drivers, &r, commit.id, parent, &PatchOpts { ctx, ..Default::default() }, None, false)
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
        // `--binary`: `setup_revisions()` reaches the same `diff_opt_parse()` arm
        // (revision.c:2721) a bare `git diff` does, so a binary pair under
        // `log -p --binary` carries the `GIT binary patch` payload.
        binary: opts.binary,
        text: opts.text,
        // `-D`/`--irreversible-delete`.
        irreversible_delete: opts.irreversible_delete,
        z: false,
        src_prefix: opts.src_prefix.clone(),
        dst_prefix: opts.dst_prefix.clone(),
        indicators: opts.indicators,
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
    /// `flags.allow_textconv`: `cmd_log_init_defaults()` (builtin/log.c) raises it for
    /// every history command, and `--no-textconv` lowers it again. With it clear no
    /// `diff.<driver>.textconv` program is run.
    pub allow_textconv: bool,
    /// `flags.allow_external`: unlike `allow_textconv` the history commands leave this
    /// down, so `GIT_EXTERNAL_DIFF`, `diff.external` and a driver's
    /// `diff.<name>.command` are inert until `--ext-diff` raises it.
    pub allow_external: bool,
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
    /// `--rename-empty` / `--no-rename-empty` (`o->flags.rename_empty`,
    /// `diff_setup()`'s default 1). With it off, `record_if_better()`
    /// (diffcore-rename.c) refuses a pair whose surviving side is an empty blob, so
    /// an empty file that moved reports as a deletion plus an addition rather than
    /// an `R100`.
    pub rename_empty: bool,
    /// The `index` line's abbreviation, when it must differ from `core.abbrev`:
    /// `--no-abbrev` zeroes `revs->abbrev`, which the raw format reads as "print the
    /// whole id" while the `index` line falls back to the configured default.
    pub index_abbrev: Option<usize>,
    /// `--minimal`/`--patience`/`--histogram`/`--diff-algorithm=<v>`, and the
    /// `diff.algorithm` config default behind them (`git_diff_ui_config()`,
    /// diff.c:78). `None` leaves the `xdl_diff()` default (Myers) in charge.
    pub algorithm: Option<gix::diff::blob::Algorithm>,
    /// `--indent-heuristic`/`--no-indent-heuristic` (`XDF_INDENT_HEURISTIC`): run the
    /// slider post-processing pass. On by default since git 2.14, so this starts
    /// `true` and only `--no-indent-heuristic` clears it.
    pub indent_heuristic: bool,
    /// `--binary`: emit a `GIT binary patch` payload for a binary pair and widen that
    /// pair's `index` line to full object names.
    pub binary: bool,
    /// `-D`/`--irreversible-delete`: a deletion loses its `---`/`+++` pair and hunks.
    pub irreversible_delete: bool,
    /// `--ignore-blank-lines` (`XDF_IGNORE_BLANK_LINES`).
    pub blank_lines: bool,
    /// `-I<re>` / `--ignore-matching-lines=<re>` (`xpp.ignore_regex`): a change whose
    /// every line matches one of these is marked ignorable by
    /// `xdl_mark_ignorable_regex()` and drops out of the hunk set.
    pub ignore_lines: Vec<super::diff_pickaxe::Needle>,
    /// `--inter-hunk-context=<n>` (`xecfg.interhunkctxlen`): the gap two changes may
    /// span and still share one hunk.
    pub inter_hunk_ctx: usize,
    /// `--submodule[=<format>]` (`o->submodule_format`). `Short` is git's default and
    /// renders a gitlink pair as the synthetic `Subproject commit <oid>` blob; the
    /// other two take `builtin_diff()`'s submodule branches (diff.c:3870).
    pub submodule_format: SubmoduleFormat,
    /// `o->output_indicators[]` — see [`Render::indicators`].
    pub indicators: (u8, u8, u8),
    /// `--diff-filter=<letters>` (`o->filter`): the status letters a pair must
    /// carry to survive `diffcore_apply_filter()`. `None` keeps every pair.
    pub diff_filter: Option<Vec<u8>>,
    /// `--relative[=<path>]` (`o->flags.relative_name` plus `o->prefix`): the
    /// repository-relative prefix, with a trailing slash, that narrows the change
    /// list and is then stripped from every reported name. `None` is
    /// `--no-relative`, git's default.
    pub relative: Option<String>,
    /// `o->ws_error_highlight` (`--ws-error-highlight=<kind>` and
    /// `diff.wsErrorHighlight`), read by `emit_line_ws_markup()` (diff.c:1374).
    pub ws_error_highlight: u32,
    /// `o->color_moved` / `o->word_diff` / `o->word_regex`: the two families that
    /// re-emit the assembled patch rather than change how it is generated.
    pub extra: diff_color::ExtraPaint,
    /// The palette the re-emit pass paints with. `git log`/`git show` leave the
    /// patch body uncolored unless `--color-words`/`--word-diff=color` forced
    /// `use_color = GIT_COLOR_ALWAYS` (`diff_opt_word_diff()`), so this is the
    /// disabled table for an ordinary run.
    pub colors: diff_color::DiffColors,
}

impl Default for PatchOpts {
    fn default() -> Self {
        PatchOpts {
            ctx: 3,
            ws: Whitespace::Keep,
            full_index: false,
            text: false,
            func_context: false,
            allow_textconv: true,
            allow_external: false,
            src_prefix: b"a/".to_vec(),
            dst_prefix: b"b/".to_vec(),
            renames: None,
            rename_score: 0,
            find_copies_harder: false,
            break_opt: -1,
            rename_empty: true,
            index_abbrev: None,
            algorithm: None,
            // `XDF_INDENT_HEURISTIC` is git's default (diff.c `diff_setup()`).
            indent_heuristic: true,
            binary: false,
            irreversible_delete: false,
            blank_lines: false,
            ignore_lines: Vec::new(),
            inter_hunk_ctx: 0,
            submodule_format: SubmoduleFormat::Short,
            indicators: (b'+', b'-', b' '),
            diff_filter: None,
            relative: None,
            // `diff_setup()` (diff.c:5150): `WSEH_NEW`.
            ws_error_highlight: diff_color::WSEH_NEW,
            extra: diff_color::ExtraPaint::default(),
            colors: diff_color::DiffColors::disabled(),
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
        let mut drivers = DriverCache::new(repo)?;
        return jobs
            .iter()
            .map(|(id, parent)| {
                commit_patch_with(repo, &mut cache, &mut drivers, &r, *id, *parent, opts, specs.as_mut(), follow)
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
                let mut drivers = DriverCache::new(&repo)?;
                let mut mine = Vec::new();
                loop {
                    let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((id, parent)) = jobs.get(i) else { break };
                    mine.push((
                        i,
                        commit_patch_with(&repo, &mut cache, &mut drivers, &r, *id, *parent, opts, specs.as_mut(), follow)?,
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

/// The file pairs one commit's diff resolves to: the tree-to-tree change list with
/// the pathspec limit and `diffcore_rename()` already applied. Shared by the patch
/// renderer and by `--dirstat`, which needs the same queue but different per-pair
/// work.
fn commit_deltas(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    drivers: &mut DriverCache<'_>,
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
    // Whether `--relative`'s prefix is stripped from the reported names as well as
    // used to narrow the queue. See the block at the end of this function.
    strip: bool,
) -> Result<Vec<Delta>> {
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
    let mut specs = specs;
    if let Some(s) = specs.as_mut() {
        if !follow {
            deltas.retain(|delta| s.matches(&delta.path));
        }
    }
    deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));

    // `diffcore_std()`: `git log`/`git show` are porcelains, so rename detection is on
    // unless `diff.renames` turns it off — a `git mv` commit is one `R` section, not a
    // deletion plus an addition.
    let mut ro = diffcore_rename::Options {
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
        rename_empty: opts.rename_empty,
        rename_limit: repo
            .config_snapshot()
            .integer("diff.renameLimit")
            .unwrap_or(diffcore_rename::DEFAULT_RENAME_LIMIT),
        hash_kind: repo.object_hash(),
        ..Default::default()
    };
    // `diff_setup_done()` (diff.c:5288): `--find-copies-harder` turns copy detection
    // on by itself, whatever `diff.renames` or `--no-renames` asked for.
    if ro.find_copies_harder {
        ro.detect_rename = diffcore_rename::DETECT_COPY;
        // git supplies the unmodified copy sources by having the tree walk emit
        // equal entries as pairs (tree-diff.c:519, 557). Reproduce that here; the
        // pathspec limit is re-applied because those pairs bypassed the retain above,
        // and `diffcore_rename()`'s write-back drops whichever ones stay unclaimed.
        if let Some(old) = old_tree.as_ref() {
            let before = deltas.len();
            add_unmodified_pairs(repo, Some(old.id().detach()), &[], false, &mut deltas)?;
            if !follow {
                if let Some(s) = specs.as_mut() {
                    let added: Vec<Delta> = deltas
                        .split_off(before)
                        .into_iter()
                        .filter(|d| s.matches(&d.path))
                        .collect();
                    deltas.extend(added);
                }
            }
        }
        deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));
    }
    // `-B` runs through the same pass even with no rename detection behind it.
    if ro.detect_rename != 0 || ro.break_opt != -1 {
        run_diffcore_rename(repo, cache, &mut deltas, &ro, false)?;
        deltas.sort_by(|a, b| a.path.cmp(&b.path).then(b.unmerged.cmp(&a.unmerged)));
    }
    // The `--follow` limit, applied once the rename it is following exists as a
    // pair: the destination is the name the file has at this commit.
    if follow {
        if let Some(specs) = specs.as_mut() {
            deltas.retain(|delta| specs.matches(&delta.path));
        }
    }

    // `diffcore_apply_filter()` (diffcore-*.c), which `diffcore_std()` runs after
    // rename detection so a pair is judged by the status it finally carries.
    if let Some(filter) = &opts.diff_filter {
        deltas.retain(|d| diff_filter_selected(filter, status_char(d)));
    }

    // `userdiff_find_by_path()` runs on `attr_path`, which `run_diff()` captures
    // *before* `strip_prefix()` (diff.c:5036-5038) — so the lookup has to happen
    // while the pairs still carry their repository-relative names.
    resolve_drivers(drivers, &mut deltas)?;

    // `--relative[=<path>]` is two separate things in git. The *narrowing* is done
    // by `diff_queue()`'s prefix test (diff.c:7630, 7748) and so applies to every
    // format; the *shortening* is `strip_prefix()` (diff.c:5009), called only from
    // `run_diff`, `run_diffstat`, `run_checkdiff`, `diff_flush_raw` and
    // `flush_one_pair`. `diff_summary()` and `show_dirstat()` never call it, which is
    // why `--relative=src --summary` still prints `src/new/moved.txt` (measured
    // against stock 2.55.0) — hence `strip`.
    if let Some(prefix) = &opts.relative {
        deltas.retain(|d| d.path.starts_with(prefix.as_bytes()));
        for d in deltas.iter_mut().take(if strip { usize::MAX } else { 0 }) {
            d.path = d.path[prefix.len()..].into();
            if let Some(src) = &d.src_path {
                if src.starts_with(prefix.as_bytes()) {
                    d.src_path = Some(src[prefix.len()..].into());
                }
            }
        }
    }

    Ok(deltas)
}

/// One commit's `--dirstat` block: `show_dirstat()` (diff.c) over the same file
/// pairs `-p` would render.
///
/// The per-pair *damage* the default (content) mode weighs is
/// `diffcore_count_changes()`' byte count, which nothing else in a history command
/// computes — the stat formats carry line counts, not bytes — so this walks the
/// pairs itself with `want_dirstat` set rather than reusing the change list
/// `--stat` already cached. `--dirstat-by-file` and `--dirstat=lines` never need it
/// and are answered from the analysis' line tallies, exactly as `git diff` answers
/// them at diff.rs:2492.
pub(crate) fn commit_dirstat(
    repo: &gix::Repository,
    commit_id: ObjectId,
    parent: Option<ObjectId>,
    opts: &PatchOpts,
    specs: Option<&mut super::log::PathspecMatcher>,
    ds: &super::diff_files::DirStat,
    out: &mut Vec<u8>,
) -> Result<()> {
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;
    let mut drivers = DriverCache::new(repo)?;
    let deltas = commit_deltas(repo, &mut cache, &mut drivers, commit_id, parent, opts, specs, false, false)?;
    let hash_kind = repo.object_hash();
    let want_damage = !ds.by_file && !ds.by_line;
    let mut files: Vec<(BString, u64)> = Vec::with_capacity(deltas.len());
    for delta in &deltas {
        let an = analyze(
            &mut cache,
            &repo.objects,
            delta,
            opts.ctx,
            opts.ws,
            opts.indent_heuristic,
            hash_kind,
            None,
            // The hunks themselves are never emitted here, but the line tallies
            // `--dirstat=lines` weighs come from the same pass.
            false,
            opts.algorithm,
            None,
            want_damage,
            false,
            opts.func_context,
            &IgnoreOpts {
                text: opts.text,
                blank_lines: opts.blank_lines,
                lines: opts.ignore_lines.clone(),
                inter_hunk_ctx: opts.inter_hunk_ctx,
            },
        )?;
        let damage = if ds.by_file {
            1
        } else if ds.by_line {
            // For a binary pair `added`/`deleted` are the two sizes, which
            // `show_dirstat_by_line()` charges in 64-byte units.
            let lines = u64::from(an.added) + u64::from(an.deleted);
            if an.binary {
                lines.div_ceil(64)
            } else {
                lines
            }
        } else if an.damage == 0 {
            // `show_dirstat()` charges a pair that changed at all a single unit, so
            // a mode-only change still shows up.
            1
        } else {
            an.damage
        };
        files.push((delta.path.clone(), damage));
    }
    super::diff_files::render_dirstat(out, files, ds);
    Ok(())
}

/// One commit's patch, reusing a caller-owned blob platform and render settings.
fn commit_patch_with(
    repo: &gix::Repository,
    cache: &mut gix::diff::blob::Platform,
    drivers: &mut DriverCache<'_>,
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
    let mut deltas = commit_deltas(repo, cache, drivers, commit_id, parent, opts, specs, follow, true)?;
    // `fill_textconv()` lives in `builtin_diff()`, so only the patch renderer starts
    // a converter — `commit_dirstat()` shares `commit_deltas` and must not. A
    // tree-to-tree diff has no worktree side, so no filespec is ever read by path.
    if opts.allow_textconv {
        apply_textconv(repo, drivers, &mut deltas, None)?;
    }
    let deltas = deltas;
    let hash_kind = repo.object_hash();
    // `external_diff()` plus the per-path `diff.<name>.command` override, both inert
    // until `--ext-diff` raises `flags.allow_external`.
    let ext_program = match opts.allow_external {
        true => external_diff_program(repo)?,
        false => None,
    };
    let want_ext = opts.allow_external
        && (ext_program.is_some()
            || deltas
                .iter()
                .any(|d| d.drivers.one.as_ref().is_some_and(|x| x.settings.external.is_some())));
    let ext_drivers = match want_ext {
        true => Some(std::cell::RefCell::new(super::cat_file::Textconv::new(repo)?)),
        false => None,
    };
    let ext = ext_drivers.as_ref().map(|d| ext_context(d, ext_program.clone()));
    let ext_naming = super::diff_pairs::IndexNaming {
        base_abbrev: r.abbrev,
        full_index: r.full_index,
        abbrev_explicit: None,
    };
    let null_id = hash_kind.null();
    let mut out: Vec<u8> = Vec::new();
    // The patch is assembled uncolored and re-emitted through the same
    // `fn_out_consume()` chain `git diff` uses, so `--word-diff`, `--color-moved`
    // and `--ws-error-highlight` behave identically in a history command.
    let ws_rule = diff_color::whitespace_rule_cfg(repo);
    let paint_opts = diff_color::PaintOptions {
        ws_error_highlight: opts.ws_error_highlight,
        indicators: opts.indicators,
        ..Default::default()
    };
    let mut plain: Vec<u8> = Vec::new();
    let mut files: Vec<diff_color::FilePaint> = Vec::new();
    // As above: one compiled regex per distinct driver pattern, for the whole commit.
    let mut word_res: std::collections::HashMap<String, std::sync::Arc<regex::bytes::Regex>> =
        std::collections::HashMap::new();
    let want_driver_words = opts.extra.wants_driver_word_regex();
    for queued in &deltas {
        // `run_diff()` (diff.c:5052) renders a type change as a deletion followed by
        // a creation; every other pair is one section.
        let halves = split_type_change(queued);
        let steps: Vec<&Delta> = match &halves {
            Some((del, add)) => vec![del, add],
            None => vec![queued],
        };
        for delta in steps {
            // `run_diff_cmd()` (diff.c:4969) runs the driver and returns, upstream of
            // every `builtin_diff()` branch below. Its stdout is spliced in verbatim:
            // git hands the child its own output descriptor, so those bytes are never
            // re-coloured.
            let pgm = match (&ext, delta.unmerged) {
                (Some(ctx), false) => external_for_pair(delta, ctx.env.as_ref()),
                _ => None,
            };
            if let (Some(ctx), Some(pgm)) = (ext.as_ref(), pgm) {
                out.extend_from_slice(&diff_color::colorize_patch_ex(
                    &plain,
                    &opts.colors,
                    &paint_opts,
                    &files,
                    diff_color::FilePaint::new(ws_rule),
                    &opts.extra,
                ));
                plain.clear();
                files.clear();
                // A tree diff reads no side from a worktree, so the queue's own ids
                // are already `diff_fill_oid_info()`'s answer.
                let old_id = delta.old.map_or(null_id, |(id, _)| id);
                let new_id = match delta.new {
                    NewSide::Blob(id, _) => id,
                    _ => null_id,
                };
                let run = super::diff_pairs::run_external_diff(
                    &pgm,
                    repo,
                    ctx,
                    &ext_pair(delta, old_id, new_id, null_id),
                    &ext_naming,
                    deltas.len(),
                    true,
                )
                .map_err(crate::fatal::die)?;
                out.extend_from_slice(&run.stdout);
                if let Some(msg) = run.died {
                    // Everything the child printed before failing has already gone
                    // out in git; this buffer is the caller's, so it travels with the
                    // error rather than being dropped.
                    return Err(crate::fatal::die(msg));
                }
                continue;
            }
            // `builtin_diff()`'s submodule branches (diff.c:3870) sit downstream of
            // `run_diff()`'s type-change split, so each half is tested on its own —
            // the same placement `git diff` uses at line 2424 above. Under the default
            // `short` format a gitlink pair falls through to `render_patch`, which
            // diffs the synthetic `Subproject commit <oid>` blobs.
            //
            // These bytes are emitted already painted, from the same palette the rest
            // of the patch is re-emitted with. Measured against stock 2.55.0,
            // `git log -p --color=always` paints the patch body exactly as `git diff`
            // does — `log_tree_commit()` hands the diff machinery the run's
            // `o->use_color` — so `opts.colors` is what applies, not a disabled table.
            if opts.submodule_format != SubmoduleFormat::Short
                && !delta.unmerged
                && delta.is_submodule_pair()
            {
                // A submodule line is written already painted, so the patch built so
                // far has to be flushed through the colorizer first to keep the
                // order — the same drain `diff()` performs at diff.rs:2489.
                out.extend_from_slice(&diff_color::colorize_patch_ex(
                    &plain,
                    &opts.colors,
                    &paint_opts,
                    &files,
                    diff_color::FilePaint::new(ws_rule),
                    &opts.extra,
                ));
                plain.clear();
                files.clear();
                render_submodule(
                    &mut out,
                    repo,
                    delta,
                    opts.submodule_format,
                    crate::abbrev::configured_abbrev(repo, hash_kind.len_in_hex()),
                    &opts.colors,
                    r,
                );
                continue;
            }
            // A worktree side never arises for a tree diff, so `workdir` is `None`.
            let an = analyze(
                cache,
                &repo.objects,
                delta,
                opts.ctx,
                opts.ws,
                opts.indent_heuristic,
                hash_kind,
                None,
                true,
                opts.algorithm,
                None,
                false,
                r.binary,
                opts.func_context,
                &IgnoreOpts {
                    text: opts.text,
                    blank_lines: opts.blank_lines,
                    lines: opts.ignore_lines.clone(),
                    inter_hunk_ctx: opts.inter_hunk_ctx,
                },
            )?;
            let before = plain.len();
            render_patch(&mut plain, repo, delta, &an, opts.ctx, r)?;
            if plain.len() != before {
                files.push(diff_color::FilePaint {
                    ws_rule,
                    blank_at_eof: an.blank_at_eof,
                    word_regex: driver_word_regex(&mut word_res, &delta.drivers, want_driver_words)?,
                });
            }
        }
    }
    // `diff_flush_patch_all_file_pairs()`: the whole commit's patch is decomposed
    // and re-emitted in one pass, which is what lets `--color-moved` see a block
    // that moved between two files of the same commit.
    out.extend_from_slice(&diff_color::colorize_patch_ex(
        &plain,
        &opts.colors,
        &paint_opts,
        &files,
        diff_color::FilePaint::new(ws_rule),
        &opts.extra,
    ));
    Ok(out)
}

/// `git log -L` / `git show -L`'s per-commit patch: `line_log_queue_pairs()`'
/// filepairs rendered by the same pipeline as `-p`, with each file's hunks clipped
/// to the ranges tracked at this commit (`builtin_diff`'s `line_ranges`).
pub(crate) fn line_range_patch(
    repo: &gix::Repository,
    pairs: &[(super::line_log::Pair, Vec<super::line_log::Range>)],
    ctx: u32,
    ws: Whitespace,
) -> Result<Vec<u8>> {
    let r = patch_render(repo, &PatchOpts { ctx, ws, ..Default::default() });
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
            // `dump_diff_hacky_one()` renders through `rev->diffopt`, so a
            // whitespace rule reaches the `-L` patch the way it reaches every
            // other one — and a commit whose only change is whitespace then has
            // no hunk left to print, which is what stock does with `git log -L
            // <range>:<file> -w` (verified against git 2.55.0: the commit's
            // header still prints, its patch does not).
            ws,
            true,
            hash_kind,
            None,
            true,
            None,
            Some(ranges),
            false,
            r.binary,
            false,
            &IgnoreOpts::default(),
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
/// `path_inside_repo()` (setup.c:162), which is `prefix_path_gently()` reduced to
/// "did it return non-NULL":
///
/// ```c
/// int path_inside_repo(struct repository *repo, const char *prefix, const char *path)
/// {
///         int len = prefix ? strlen(prefix) : 0;
///         char *r = prefix_path_gently(repo, prefix, len, NULL, path);
///         if (r) { free(r); return 1; }
///         return 0;
/// }
/// ```
///
/// A *relative* path is resolved against the prefix and pushed through
/// `normalize_path_copy_len()`, which fails — and so answers "outside" — exactly
/// when a `..` component pops past the start. An *absolute* one is compared
/// against the worktree by `abspath_part_inside_repo()`, which works on real paths:
/// on macOS a worktree reached through `$TMPDIR` is `/var/…` while its real path is
/// `/private/var/…`, so a plain string compare would call an inside path an outside
/// one.
///
/// The file need not exist. `git diff` names paths that do not, and git's own
/// normalization is textual, so an unresolvable leaf falls back to resolving its
/// directory.
fn path_inside_repo(repo: &gix::Repository, prefix: &str, path: &str) -> bool {
    let real = |p: &std::path::Path| -> std::path::PathBuf {
        p.canonicalize().unwrap_or_else(|_| match (p.parent(), p.file_name()) {
            (Some(dir), Some(name)) => {
                dir.canonicalize().map(|d| d.join(name)).unwrap_or_else(|_| p.to_path_buf())
            }
            _ => p.to_path_buf(),
        })
    };
    if path.starts_with('/') {
        let Some(root) = repo.workdir() else { return false };
        let (path, root) = (real(std::path::Path::new(path)), real(root));
        return path.starts_with(&root);
    }
    // `normalize_path_copy_len()` over `prefix + path`: a `.` component disappears,
    // a `..` pops the one before it, and repeated slashes collapse. It reports the
    // escape by failing, which is a `..` left with nothing in front of it.
    let mut out: Vec<&str> = Vec::new();
    for component in format!("{prefix}{path}").split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() {
                    return false;
                }
            }
            c => out.push(c),
        }
    }
    true
}

pub(crate) fn cwd_prefix(repo: &gix::Repository) -> String {
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
    // `-R`: the worktree root belongs to the pre-image side, as it does on the
    // caller's platform.
    reverse: bool,
    // Whether `--dirstat` will need each pair's content damage.
    want_dirstat: bool,
    // Whether `--binary` will need each binary pair's two images.
    want_binary: bool,
    // `-W`: emit hunks grown to enclosing-function boundaries.
    func_context: bool,
    // See [`analyze`].
    ignore: &IgnoreOpts,
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
                    ignore,
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
                        if reverse {
                            WorktreeRoots { old_root: Some(root.to_owned()), new_root: None }
                        } else {
                            WorktreeRoots { old_root: None, new_root: Some(root.to_owned()) }
                        },
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
                        ignore,
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

/// Hand the blob platform git's *invalid filespec*: the side of a pair that does not
/// exist.
///
/// `diff_populate_filespec()` (diff.c:4062) returns immediately for a filespec whose
/// `oid_valid` and `is_stdin` are both clear and whose mode is zero — an absent side
/// never reaches the worktree, whichever side of the pair it is on. The blob platform
/// has no such state: it decides between "read this path off disk" and "resolve this id
/// in the odb" purely by whether a [`WorktreeRoots`] entry covers the side
/// (`gix-diff/src/blob/pipeline.rs:271`), and a null id only *looks* absent under a root
/// because the file is normally gone. When something else has taken the name — a
/// directory, most commonly, after `rm f && mkdir f` — the read fails and the whole diff
/// dies, where stock renders an ordinary deletion patch at 0.
///
/// So the root is lifted for the one call. With `roots.by_kind(kind)` `None`, the
/// pipeline's `id.is_null()` arm (`pipeline.rs:399`) reports no data at all, which is
/// exactly the empty filespec. The lift also moves the platform's cache key from the
/// path to the (null) id, so an absent side shares one entry instead of shadowing the
/// worktree entry for that path.
fn set_absent_resource(
    cache: &mut gix::diff::blob::Platform,
    kind: ResourceKind,
    mode: EntryKind,
    rela_path: &gix::bstr::BStr,
    objects: &gix::OdbHandle,
    null: ObjectId,
) -> Result<()> {
    let root = match kind {
        ResourceKind::OldOrSource => cache.filter.roots.old_root.take(),
        ResourceKind::NewOrDestination => cache.filter.roots.new_root.take(),
    };
    let res = cache.set_resource(null, mode, rela_path, kind, objects);
    match kind {
        ResourceKind::OldOrSource => cache.filter.roots.old_root = root,
        ResourceKind::NewOrDestination => cache.filter.roots.new_root = root,
    }
    res?;
    Ok(())
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
    // The `xpp`/`xecfg` knobs that decide which changes survive into the hunk stream
    // at all: `--ignore-blank-lines`, `-I<re>`, `--inter-hunk-context=<n>` and `-a`.
    ignore: &IgnoreOpts,
) -> Result<Analysis> {
    let null = hash_kind.null();
    // `builtin_diff()` installs `xecfg.find_func` from the pair's driver before it
    // runs xdiff, so every hunk of this pair reads its heading off the same pattern.
    let funcname = delta.drivers.funcname();
    if delta.unmerged {
        return Ok(Analysis {
            old_id: null,
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
    let old_is_gitlink = matches!(delta.old, Some((_, EntryKind::Commit)));
    let new_is_gitlink = matches!(delta.new_kind(), Some(EntryKind::Commit));
    // A pair with a gitlink on *one* side and a blob on the other. `run_diff()`
    // (diff.c:5052) splits it for the patch formats, but `builtin_diffstat()`
    // (diff.c:3900) is handed it whole and fills both sides with `fill_mmfile()` —
    // so the blob contributes its own lines while the gitlink contributes the
    // one-line image `diff_populate_filespec()` (diff.c:4110) synthesises for it.
    // Measured against git 2.55.0: a 3-line file replaced by a checked-out submodule
    // is `1  3  f` in `--numstat` and ` f | 4 +---` in `--stat`.
    let mixed_gitlink =
        delta.old.is_some() && delta.new_kind().is_some() && old_is_gitlink != new_is_gitlink;
    if (old_is_gitlink || new_is_gitlink) && !mixed_gitlink {
        return analyze_gitlink(
            old_commit,
            new_commit,
            delta.old_dirty_submodule,
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
    // The gitlink half of a mixed pair has no blob to hand the platform, so both
    // images are filled the way `fill_mmfile()` does and diffed directly.
    if mixed_gitlink {
        let before = filespec_image(objects, workdir, delta, true)?;
        let after = filespec_image(objects, workdir, delta, false)?;
        return analyze_images(
            Some(before),
            Some(after),
            delta.old.map_or(null, |(id, _)| id),
            match &delta.new {
                NewSide::Blob(id, _) => *id,
                NewSide::Worktree(EntryKind::Commit) => delta.new_commit.unwrap_or(null),
                _ => null,
            },
            false,
            ctx,
            want_patch,
            algo_override,
        );
    }
    match delta.old {
        Some((id, k)) => cache.set_resource(id, k, old_side_path, ResourceKind::OldOrSource, objects)?,
        // An addition's pre-image is git's invalid filespec — `diff_populate_filespec()`
        // returns an empty one without touching the worktree. See [`set_absent_resource`].
        None => set_absent_resource(cache, ResourceKind::OldOrSource, old_kind, old_side_path, objects, null)?,
    };
    match &delta.new {
        NewSide::Blob(id, k) => {
            cache.set_resource(*id, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        NewSide::Worktree(k) => {
            // With `new_root` set on the cache, a null id reads from the worktree by path.
            cache.set_resource(null, *k, path, ResourceKind::NewOrDestination, objects)?;
        }
        // A deletion's post-image is git's invalid filespec: no content, and never a
        // worktree read. See [`set_absent_resource`].
        NewSide::Absent => {
            set_absent_resource(cache, ResourceKind::NewOrDestination, old_kind, path, objects, null)?;
        }
    };

    // The platform's configured `diff.algorithm`, read before `prepare_diff()` takes
    // its borrow. Only the textconv path below needs it: everywhere else the
    // algorithm arrives with the operation.
    let platform_algorithm = cache.options.algorithm.unwrap_or_default();
    let prep = cache.prepare_diff()?;

    // `diff_populate_filespec()` hashes a filespec that has no id of its own, which
    // is how the `index` line of a reversed worktree diff still names both sides.
    let old_id: ObjectId = match (delta.old, delta.old_worktree) {
        (None, _) => null,
        (Some((id, _)), false) => id,
        (Some(_), true) => {
            if !prep.old.id.is_null() {
                prep.old.id.to_owned()
            } else if let Some(buf) = prep.old.data.as_slice() {
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, buf)?
            } else {
                let base = workdir.ok_or_else(|| anyhow::anyhow!("missing work tree"))?;
                let full = base.join(gix::path::from_bstr(old_side_path));
                let bytes = std::fs::read(&full)?;
                gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &bytes)?
            }
        }
    };

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

    // `builtin_diff()` (diff.c:3965) drops the binary test for a side that went
    // through a converter, so a pair with textconv always takes the textual path.
    let operation = match delta.textconv {
        Some(_) => Operation::InternalDiff {
            algorithm: platform_algorithm,
        },
        None => prep.operation,
    };
    match operation {
        Operation::SourceOrDestinationIsBinary => {
            // The blob pipeline withholds the data for a binary pair, so both images
            // are read back here — and only if `--dirstat`, `--binary` or `-a` is
            // going to use them, since for a binary pair that is the whole file on
            // both sides.
            let images = if want_dirstat || want_binary || ignore.text {
                let old_bytes = match (delta.old, delta.old_worktree) {
                    (None, _) => Vec::new(),
                    (Some(_), true) => workdir
                        .map(|base| std::fs::read(base.join(gix::path::from_bstr(old_side_path))))
                        .transpose()?
                        .unwrap_or_default(),
                    (Some((id, _)), false) => read_blob(objects, id)?,
                };
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
                old_id,
                new_id,
                // `diffstat_consume()` never sees a binary pair; `show_stats()` reads
                // the two *sizes* out of the filespecs instead and prints them as
                // `Bin <old> -> <new> bytes`, so that is what these two carry here.
                // Every consumer that counts lines skips a pair with `binary` set.
                added: blob_size_new(objects, delta, workdir, path)?,
                deleted: blob_size_old(objects, delta, workdir)?,
                binary: true,
                // `-a`/`--text` (`o->flags.text`) drops out of `builtin_diff()`'s
                // binary test, so the pair gets an ordinary textual patch while
                // `binary` above keeps `builtin_diffstat()` — which never reads the
                // flag — reporting `Bin <a> -> <b> bytes`.
                hunks: match (ignore.text && want_patch, &images) {
                    (true, Some((old_bytes, new_bytes))) => text_hunks(
                        old_bytes,
                        new_bytes,
                        ctx,
                        ws,
                        indent_heuristic,
                        algo_override.unwrap_or(gix::diff::blob::Algorithm::Myers),
                        func_context,
                        ignore,
                        funcname,
                    )?
                    .1,
                    _ => None,
                },
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
        // Unreachable: `prepare_diff()` only chooses this operation when
        // `Options::skip_internal_diff_if_external_is_configured` is set, and
        // `gix::diff::resource_cache()` (src/ported/gix/src/diff.rs:236) hardcodes it
        // to `false` for every platform this command builds. A `diff.<driver>.command`
        // is therefore ignored here rather than run — see the module header.
        Operation::ExternalCommand { .. } => {
            bail!("external diff drivers are not supported for {path:?}")
        }
        Operation::InternalDiff { algorithm } => {
            // `--minimal`/`--histogram`/`--diff-algorithm=` override the default.
            let algorithm = algo_override.unwrap_or(algorithm);
            let raw_old = prep.old.data.as_slice().unwrap_or_default();
            let raw_new = prep.new.data.as_slice().unwrap_or_default();
            // `builtin_diff()` (diff.c:4027-4028) hands xdiff the *converted* images,
            // while `builtin_diffstat()` (diff.c:4189) never calls `fill_textconv()`
            // at all and `show_dirstat()` weighs `diff_populate_filespec()`'s raw
            // bytes — so a `-p --stat` run over a textconv'd path prints a patch of
            // the converted text beside a stat of the original.
            let (old_data, new_data) = match &delta.textconv {
                Some((o, n)) => (o.as_slice(), n.as_slice()),
                None => (raw_old, raw_new),
            };
            // `check_blank_at_eof()` runs on the whole images, before xdiff, so the
            // emit layer can tell an added blank line at EOF from an ordinary one.
            let blank_at_eof = diff_color::check_blank_at_eof(old_data, new_data);

            // `builtin_diff()`: a `-B` rewrite that stayed a modification never runs
            // xdiff at all. `emit_rewrite_diff()` replaces the whole file instead —
            // one hunk deleting every old line and adding every new one — and
            // `diffstat` counts the same way (`count_lines()` on each side).
            if delta.complete_rewrite() {
                let deleted = count_lines(raw_old);
                let added = count_lines(raw_new);
                let hunks = want_patch.then(|| emit_rewrite_diff(old_data, new_data));
                return Ok(Analysis {
                    old_id,
                    new_id,
                    added,
                    deleted,
                    binary: false,
                    hunks,
                    blank_at_eof,
                    damage: if want_dirstat {
                        byte_damage(raw_old, raw_new, delta.old.is_some(), delta.new_valid(), false)
                    } else {
                        0
                    },
                    images: None,
                });
            }
            let ((added, deleted), hunks) = match line_ranges {
                // `-L`: xdiff runs with the context inflated to the widest tracked
                // span so every change inside one range lands in a single hunk, and
                // the sink clips back to the range bounds. `-L` is a history option
                // and never arrives beside `-I` or `--inter-hunk-context`.
                Some(rs) => {
                    let before: Vec<&[u8]> = byte_lines(old_data);
                    let after: Vec<&[u8]> = byte_lines(new_data);
                    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
                    input.update_before(before.iter().map(|l| normalize_line(l, ws)));
                    input.update_after(after.iter().map(|l| normalize_line(l, ws)));
                    let diff = super::diff_pairs::compute_compacted(
                        algorithm,
                        &input,
                        &before,
                        &after,
                        indent_heuristic,
                    );
                    let counts = (diff.count_additions(), diff.count_removals());
                    let hunks = if want_patch && (counts.0 != 0 || counts.1 != 0) {
                        let ctx = super::line_log::RangeSink::context(rs, ctx);
                        let sink = super::line_log::RangeSink::new(&before, &after, rs);
                        Some(
                            UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx))
                                .consume()?,
                        )
                    } else {
                        None
                    };
                    (counts, hunks)
                }
                None => text_hunks(
                    old_data,
                    new_data,
                    ctx,
                    ws,
                    indent_heuristic,
                    algorithm,
                    func_context,
                    ignore,
                    funcname,
                )
                .map(|(counts, hunks)| (counts, hunks.filter(|_| want_patch)))?,
            };
            // `builtin_diffstat()` diffs the unconverted filespecs, so the numbers
            // `--stat`/`--numstat` print are the raw ones even when the patch beside
            // them is of the converted text.
            let (added, deleted) = match &delta.textconv {
                Some(_) => {
                    text_hunks(
                        raw_old,
                        raw_new,
                        ctx,
                        ws,
                        indent_heuristic,
                        algorithm,
                        func_context,
                        ignore,
                        funcname,
                    )?
                    .0
                }
                None => (added, deleted),
            };
            Ok(Analysis {
                old_id,
                new_id,
                added,
                deleted,
                binary: false,
                hunks,
                blank_at_eof,
                damage: if want_dirstat {
                    byte_damage(raw_old, raw_new, delta.old.is_some(), delta.new_valid(), false)
                } else {
                    0
                },
                images: None,
            })
        }
    }
}

/// `xdl_diff()` over two whole images: intern both sides under the active whitespace
/// rules, run the chosen algorithm, and turn the change script into unified-diff text.
/// Returns `((additions, removals), hunks)`, with the counts read off the *emitted*
/// records — which is what `diffstat_consume()` counts, and the only way
/// `--ignore-blank-lines` and `-I` can reach `--stat`/`--numstat` as they do in git.
///
/// Two emitters answer to this, both producing the same bytes for a plain diff:
///
/// * gitoxide's unified writer through [`PatchSink`], the default.
/// * the in-tree `xdl_emit_diff` port in [`super::diff_pairs::emit_unified`], the same
///   emitter `git diff-pairs` runs, whenever the hunk *geometry* depends on something
///   the gitoxide writer cannot express: `-W`'s growth to function boundaries,
///   `--inter-hunk-context`'s merging of neighbours, or an `ignore` bit that
///   `xdl_get_hunk()` has to weigh against the distance to a real change.
#[allow(clippy::too_many_arguments)]
fn text_hunks(
    old_data: &[u8],
    new_data: &[u8],
    ctx: u32,
    ws: Whitespace,
    indent_heuristic: bool,
    algorithm: gix::diff::blob::Algorithm,
    func_context: bool,
    ignore: &IgnoreOpts,
    // `xecfg->find_func`: the path's userdiff driver funcname pattern, if any.
    funcname: Option<&crate::userdiff::FuncName>,
) -> Result<((u32, u32), Option<Vec<u8>>)> {
    let before: Vec<&[u8]> = byte_lines(old_data);
    let after: Vec<&[u8]> = byte_lines(new_data);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| normalize_line(l, ws)));
    input.update_after(after.iter().map(|l| normalize_line(l, ws)));

    // `xdl_change_compact()` measures `xdf->recs[i]->ptr`, the *original* record, not
    // the whitespace-normalized token the comparison used.
    let diff =
        super::diff_pairs::compute_compacted(algorithm, &input, &before, &after, indent_heuristic);

    if !func_context && ignore.inter_hunk_ctx == 0 && !ignore.marks_changes() {
        let added = diff.count_additions();
        let deleted = diff.count_removals();
        if added == 0 && deleted == 0 {
            return Ok(((0, 0), None));
        }
        let sink = PatchSink {
            buf: Vec::new(),
            before: &before,
            after: &after,
            funcname,
            // No hunk has been emitted yet, so nothing bounds the first search.
            func_prev: -1,
            func_text: Vec::new(),
        };
        let buf = UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx)).consume()?;
        return Ok(((added, deleted), Some(buf)));
    }

    // The change script in `xdchange_t` shape, carrying the `ignore` bit
    // `xdl_mark_ignorable_lines()` (`--ignore-blank-lines`) and
    // `xdl_mark_ignorable_regex()` (`-I<re>`) set on a change whose every pre- and
    // post-image record is ignorable. Both markers *assign* rather than or into
    // `xch->ignore` and the regex pass runs second, so `-I` has the final say
    // whenever it is present. The same rule [`super::diff_pairs`] applies.
    let changes: Vec<super::diff_pairs::Change> = diff
        .hunks()
        .map(|h| {
            let (i1, chg1) = (h.before.start as usize, h.before.len());
            let (i2, chg2) = (h.after.start as usize, h.after.len());
            let all = |pred: &dyn Fn(&[u8]) -> bool| {
                before[i1..i1 + chg1].iter().all(|l| pred(l))
                    && after[i2..i2 + chg2].iter().all(|l| pred(l))
            };
            let ignored = if !ignore.lines.is_empty() {
                all(&|l| ignore.lines.iter().any(|p| p.is_match(l)))
            } else if ignore.blank_lines {
                all(&|l| is_blank_record(l, ws))
            } else {
                false
            };
            super::diff_pairs::Change { i1, chg1, i2, chg2, ignore: ignored }
        })
        .collect();
    let (added, deleted, buf) = super::diff_pairs::emit_unified(
        &before,
        &after,
        &changes,
        &super::diff_pairs::EmitGeometry {
            ctx: ctx as usize,
            inter_hunk_ctx: ignore.inter_hunk_ctx,
            func_context,
            funcname,
        },
    );
    Ok(((added, deleted), (!buf.is_empty()).then_some(buf)))
}

/// `xdl_blankline()`: with no whitespace option in force a record is blank only when
/// it is empty or a bare terminator; once any `XDF_WHITESPACE_FLAGS` bit is set, any
/// record made entirely of whitespace counts.
fn is_blank_record(line: &[u8], ws: Whitespace) -> bool {
    if ws == Whitespace::Keep {
        return line.len() <= 1;
    }
    line.iter().all(|b| b.is_ascii_whitespace())
}

/// `diff_populate_filespec(..., CHECK_SIZE_ONLY)` for the pre-image: the blob's
/// size without reading it, which is all `show_stats()` wants for a binary pair.
fn blob_size_old(
    objects: &gix::OdbHandle,
    delta: &Delta,
    workdir: Option<&std::path::Path>,
) -> Result<u32> {
    use gix::objs::FindHeader;
    let Some((id, _)) = delta.old else { return Ok(0) };
    // A worktree-backed pre-image (`-R`) has no object to ask; its size is the
    // file's, exactly as [`blob_size_new`] reads it for the other side.
    if delta.old_worktree {
        let Some(base) = workdir else { return Ok(0) };
        let full = base.join(gix::path::from_bstr(delta.old_path().as_bstr()));
        return Ok(std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0) as u32);
    }
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

/// One side of a pair as `diffcore_pickaxe()` sees it: the bytes
/// `diff_populate_filespec()` would fill in, or `None` for `!DIFF_FILE_VALID`.
///
/// This is read before the blob platform runs, because `diffcore_pickaxe()` is part
/// of `diffcore_std()` and drops pairs the emitters would otherwise have analysed.
/// A gitlink contributes its recorded commit id, which is the one-line
/// `Subproject commit <oid>` blob git synthesises for it.
fn pickaxe_side(
    objects: &gix::OdbHandle,
    workdir: Option<&std::path::Path>,
    d: &Delta,
    source: bool,
) -> Result<Option<Vec<u8>>> {
    if d.unmerged {
        return Ok(None);
    }
    if source {
        let Some((id, kind)) = d.old else { return Ok(None) };
        if kind == EntryKind::Commit {
            return Ok(Some(id.to_string().into_bytes()));
        }
        // A worktree pre-image (only `-R` produces one) has no recorded blob; it is
        // read by path, exactly as the post-image below is.
        if d.old_worktree {
            return Ok(read_worktree_bytes(workdir, d.old_path()));
        }
        return Ok(Some(read_blob(objects, id)?));
    }
    match &d.new {
        NewSide::Absent => Ok(None),
        NewSide::Blob(id, EntryKind::Commit) => Ok(Some(id.to_string().into_bytes())),
        NewSide::Blob(id, _) => Ok(Some(read_blob(objects, *id)?)),
        NewSide::Worktree(EntryKind::Commit) => {
            Ok(d.new_commit.map(|id| id.to_string().into_bytes()))
        }
        NewSide::Worktree(_) => Ok(read_worktree_bytes(workdir, &d.path)),
    }
}

/// One side of a pair as `pickaxe_match()` (diffcore-pickaxe.c:130) hands it to the
/// search function.
struct PickaxeSide {
    /// `fill_textconv()`'s image, or the raw blob when no converter applies. `None`
    /// is a side that does not exist, which `has_changes()` counts as zero
    /// occurrences and `diff_grep()` treats as a whole-file add or delete.
    side: Option<Vec<u8>>,
    /// Whether `textconv_one`/`textconv_two` came back non-NULL, which is what the
    /// `-G` binary guard reads rather than "did the bytes change".
    converted: bool,
}

/// `get_textconv()` + `fill_textconv()` for one side of a pickaxe pair
/// (diffcore-pickaxe.c:148-170).
///
/// `get_textconv()` (diff.c:3762) answers NULL for a side that does not exist, so a
/// missing side is handed through as it is; a converter that could not be run is
/// `run_textconv()`'s NULL return, which `fill_textconv()` turns into
/// `die(_("unable to read files to diff"))` — the same fatal the patch pass raises,
/// only earlier, because the filter runs before anything is rendered.
fn pickaxe_textconv(
    conv: &mut super::cat_file::Textconv<'_>,
    allow: bool,
    path: &BString,
    raw: Option<Vec<u8>>,
) -> Result<PickaxeSide> {
    let Some(raw) = raw else {
        return Ok(PickaxeSide { side: None, converted: false });
    };
    if !allow {
        return Ok(PickaxeSide { side: Some(raw), converted: false });
    }
    match conv.convert(path.as_bstr(), &raw)? {
        super::cat_file::Converted::Text(t) => Ok(PickaxeSide { side: Some(t), converted: true }),
        super::cat_file::Converted::NoDriver => {
            Ok(PickaxeSide { side: Some(raw), converted: false })
        }
        super::cat_file::Converted::Failed => {
            Err(crate::fatal::die("unable to read files to diff"))
        }
    }
}

/// `!textconv_<side> && diff_filespec_is_binary(o->repo, p-><side>)`, the half of the
/// `-G` guard that applies to one side (diffcore-pickaxe.c:166-167).
///
/// `diff_filespec_is_binary()` (diff.c:3712-3733) takes the driver's `binary`
/// tri-state when it is not `-1` and otherwise sniffs the buffer for a NUL in the
/// first 8000 bytes; a side with no data at all is not binary.
///
/// Not modelled: the boolean attribute forms. `userdiff_find_by_path()`
/// (userdiff.c:540-542) answers `driver_false`, whose `.binary` is 1, for a path
/// marked `-diff`, so git calls such a path binary whatever its bytes are. The
/// attribute reader behind [`super::cat_file::Textconv::driver_name`] reports only
/// *named* drivers, so a `-diff` path is sniffed here instead — visible only as
/// `git log -G<re>` still considering a `-diff` text file.
fn pickaxe_binary_side(
    repo: &gix::Repository,
    conv: &mut super::cat_file::Textconv<'_>,
    path: &BString,
    side: &PickaxeSide,
) -> Result<bool> {
    if side.converted {
        return Ok(false);
    }
    let Some(data) = side.side.as_deref() else {
        return Ok(false);
    };
    if let Some(name) = conv.driver_name(path.as_bstr())? {
        // `parse_tristate()` (userdiff.c:471): `auto` is the `-1` that means "work it
        // out", anything else is `git_config_bool`.
        if let Some(v) = super::cat_file::diff_driver_config(repo, &name, "binary") {
            if !v.trim().eq_ignore_ascii_case("auto") {
                return Ok(crate::userdiff::config_bool(&v));
            }
        }
    }
    Ok(looks_binary(data))
}

/// The bytes of a worktree path, with a symlink contributing its target — which is
/// what git stores as the blob and therefore what the pickaxe searches.
fn read_worktree_bytes(workdir: Option<&std::path::Path>, path: &BString) -> Option<Vec<u8>> {
    let full = workdir?.join(gix::path::from_bstr(path.as_bstr()));
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&full).ok()?;
        return Some(gix::path::into_bstr(target).into_owned().into());
    }
    std::fs::read(&full).ok()
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
    geom: &super::diff_pairs::EmitGeometry<'_>,
    ws: Whitespace,
    binary: bool,
    algorithm: gix::diff::blob::Algorithm,
    ignore_blank_lines: bool,
) -> (u32, u32, Vec<u8>) {
    if binary {
        return (0, 0, Vec::new());
    }
    let before: Vec<&[u8]> = byte_lines(old_data);
    let after: Vec<&[u8]> = byte_lines(new_data);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| normalize_line(l, ws)));
    input.update_after(after.iter().map(|l| normalize_line(l, ws)));
    let diff = super::diff_pairs::compute_compacted(algorithm, &input, &before, &after, true);
    let changes: Vec<super::diff_pairs::Change> = diff
        .hunks()
        .map(|h| {
            // `xdl_mark_ignorable_lines()` (`--ignore-blank-lines`): a change group
            // whose every removed and added record is blank is marked, which keeps
            // `xdl_get_hunk()` from opening a hunk for it.
            let ignore = ignore_blank_lines
                && h.before.clone().all(|i| is_blank_record(before[i as usize], ws))
                && h.after.clone().all(|i| is_blank_record(after[i as usize], ws));
            super::diff_pairs::Change {
                i1: h.before.start as usize,
                chg1: h.before.len(),
                i2: h.after.start as usize,
                chg2: h.after.len(),
                ignore,
            }
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
    render_rows_stat_ex(out, rows, colors, &StatWidths::default(), false);
}

/// [`render_rows_stat`] with the geometry `--stat=<w>` / `--stat-width` /
/// `--stat-name-width` / `--stat-graph-width` / `--stat-count` selected, and with
/// `--compact-summary`'s annotation switch.
///
/// `diff --no-index` is still `builtin/diff.c`, so it too has run
/// `init_diffstat_widths()` and scales to the terminal; the four geometry flags
/// are on the `add_diff_options()` table it shares with `git diff`.
pub(crate) fn render_rows_stat_ex(
    out: &mut Vec<u8>,
    rows: &[(BString, BString, u32, u32, bool)],
    colors: &diff_color::DiffColors,
    widths: &StatWidths,
    compact: bool,
) {
    let (deltas, analyses) = synthetic_rows(rows);
    diffstat::show_stats(
        out,
        &stat_rows(&diffstat_pairs(&deltas, &analyses), compact),
        widths,
        colors,
    );
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
            old_id: null,
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
    old_dirty: u8,
    dirty: u8,
    null: ObjectId,
    ctx: u32,
    want_patch: bool,
    algo_override: Option<gix::diff::blob::Algorithm>,
) -> Result<Analysis> {
    analyze_images(
        old_commit.map(|id| subproject_image(id, old_dirty != 0)),
        new_commit.map(|id| subproject_image(id, dirty != 0)),
        old_commit.unwrap_or(null),
        new_commit.unwrap_or(null),
        // The `-dirty` marker moves the patch but not the stat formats: measured
        // against git 2.55.0 on a submodule whose worktree is damaged at the commit
        // the index already records, `git diff` prints the `-Subproject commit <oid>`
        // / `+Subproject commit <oid>-dirty` hunk while `git diff --numstat` prints
        // `0\t0\tsub` and `--shortstat` prints `1 file changed, 0 insertions(+),
        // 0 deletions(-)`. So the counts come from the two commit ids alone.
        (dirty | old_dirty) != 0 && old_commit == new_commit,
        ctx,
        want_patch,
        algo_override,
    )
}

/// `diff_populate_gitlink()` (diff.c:4475): the one-line image git gives a gitlink
/// filespec, with `-dirty` glued on whenever any `dirty_submodule` bit is set.
fn subproject_image(id: ObjectId, dirty: bool) -> Vec<u8> {
    let mut v = b"Subproject commit ".to_vec();
    v.extend_from_slice(id.to_hex().to_string().as_bytes());
    if dirty {
        v.extend_from_slice(b"-dirty");
    }
    v.push(b'\n');
    v
}

/// `diff_populate_filespec()` (diff.c:4062) for one side of a pair whose *other* side
/// is a gitlink: a gitlink contributes [`subproject_image`], anything else its own
/// bytes — the blob out of the object database, or the worktree file.
fn filespec_image(
    objects: &gix::OdbHandle,
    workdir: Option<&std::path::Path>,
    delta: &Delta,
    source: bool,
) -> Result<Vec<u8>> {
    let (commit, dirty) = if source {
        let id = match delta.old {
            Some((id, EntryKind::Commit)) => Some(id),
            _ => None,
        };
        (id, delta.old_dirty_submodule)
    } else {
        let id = match &delta.new {
            NewSide::Blob(id, EntryKind::Commit) => Some(*id),
            NewSide::Worktree(EntryKind::Commit) => delta.new_commit,
            _ => None,
        };
        (id, delta.dirty_submodule)
    };
    match commit {
        Some(id) => Ok(subproject_image(id, dirty != 0)),
        None => Ok(pickaxe_side(objects, workdir, delta, source)?.unwrap_or_default()),
    }
}

/// The whole-pair analysis of two images that never went through the blob platform:
/// the gitlink pairs above, and the mixed blob/gitlink pair `builtin_diffstat()` is
/// handed whole. `None` is git's invalid filespec — the side does not exist.
///
/// `zero_counts` forces the stat formats to zero for a pair whose two sides carry the
/// same object id, which is only the `-dirty` case above.
#[allow(clippy::too_many_arguments)]
fn analyze_images(
    before_bytes: Option<Vec<u8>>,
    after_bytes: Option<Vec<u8>>,
    old_id: ObjectId,
    new_id: ObjectId,
    zero_counts: bool,
    ctx: u32,
    want_patch: bool,
    algo_override: Option<gix::diff::blob::Algorithm>,
) -> Result<Analysis> {
    let old_data = before_bytes.as_deref().unwrap_or_default();
    let new_data = after_bytes.as_deref().unwrap_or_default();
    let before: Vec<&[u8]> = byte_lines(old_data);
    let after: Vec<&[u8]> = byte_lines(new_data);

    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| l.to_vec()));
    input.update_after(after.iter().map(|l| l.to_vec()));
    let algorithm = algo_override.unwrap_or(gix::diff::blob::Algorithm::Myers);
    let diff = diff_with_slider_heuristics(algorithm, &input);
    let (added, deleted) = if zero_counts {
        (0, 0)
    } else {
        (diff.count_additions(), diff.count_removals())
    };
    let hunks = if want_patch && (diff.count_additions() != 0 || diff.count_removals() != 0) {
        let sink = PatchSink {
            buf: Vec::new(),
            before: &before,
            after: &after,
            // Both images here are the synthetic `Subproject commit <oid>` line
            // `diff_populate_filespec()` writes for a gitlink, which no funcname
            // pattern is meant to read.
            funcname: None,
            // No hunk has been emitted yet, so nothing bounds the first search.
            func_prev: -1,
            func_text: Vec::new(),
        };
        Some(UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(ctx)).consume()?)
    } else {
        None
    };
    Ok(Analysis {
        old_id,
        new_id,
        added,
        deleted,
        binary: false,
        hunks,
        // A synthetic `Subproject commit <oid>` blob never ends in a blank line.
        blank_at_eof: (0, 0),
        images: None,
        // The same images `builtin_diff()` hands the rest of the diff machinery, so a
        // submodule bump is damage like any other content change.
        damage: byte_damage(
            old_data,
            new_data,
            before_bytes.is_some(),
            after_bytes.is_some(),
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
        // `XDF_IGNORE_CR_AT_EOL`: exactly one CR, and only where it sits against a
        // real line terminator. `ends_with_optional_cr()` (xdiff/xutils.c:159-171)
        // computes `complete = s && l[s-1] == '\n'` and only then accepts a CR in
        // front of it -- "do not ignore CR at the end of an incomplete line". So a
        // final line that gained a CR but no newline still differs, and stripping a
        // bare trailing CR here made `diff --quiet --ignore-cr-at-eol` report 0
        // where git reports 1.
        Whitespace::IgnoreCrAtEol => {
            let mut out = line.to_vec();
            if out.last() == Some(&b'\n') && out.len() >= 2 && out[out.len() - 2] == b'\r' {
                out.remove(out.len() - 2);
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
fn render_raw(out: &mut Vec<u8>, delta: &Delta, fmt: u32, r: &Render, filled: Option<&Analysis>) {
    let status = status_char(delta);
    if fmt & F_NAME_STATUS == 0 {
        let null = r.hash_kind.null().to_hex_with_len(r.raw_abbrev).to_string();
        // `diff_fill_oid_info()` (diff.c:4014) hashes a filespec that has no id of
        // its own with `index_path()`, leaving the real object name behind in
        // `p->one->oid`/`p->two->oid`. Only `run_diff()` calls it, so the raw format
        // sees a filled id exactly when `diff_from_contents` made it render each pair
        // through the patch machinery first — which is why `git diff -w --raw` prints
        // a worktree side's real name and plain `git diff --raw` prints all-zero.
        let hashed = |side: fn(&Analysis) -> ObjectId| {
            filled
                .map(side)
                .filter(|id| !id.is_null())
                .map(|id| id.to_hex_with_len(r.raw_abbrev).to_string())
        };
        let old_hash = match (delta.old, delta.old_worktree) {
            (None, _) => null.clone(),
            (Some((id, _)), false) => id.to_hex_with_len(r.raw_abbrev).to_string(),
            // A worktree pre-image has no id of its own, which git reports as
            // all-zero — unless the side kept a printable one (`hash_filespec()`, or
            // a submodule sitting where the index says).
            (Some(_), true) => match delta.old_raw_id {
                Some(id) => id.to_hex_with_len(r.raw_abbrev).to_string(),
                None => hashed(|an| an.old_id).unwrap_or_else(|| null.clone()),
            },
        };
        // Worktree content has no object id yet, which git reports as all-zero —
        // unless rename detection already hashed it (`hash_filespec()`).
        let new_hash = match (&delta.new, delta.unmerged) {
            (NewSide::Blob(id, _), false) => id.to_hex_with_len(r.raw_abbrev).to_string(),
            (NewSide::Worktree(k), false) => match delta.new_id {
                Some(id) => id.to_hex_with_len(r.raw_abbrev).to_string(),
                // A gitlink never goes through the blob platform, so the analysis
                // has no hash of the worktree to offer for one.
                None if *k == EntryKind::Commit => null,
                None => hashed(|an| an.new_id).unwrap_or(null),
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

pub(crate) use super::diff_pairs::pprint_rename;

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
            let (Some((_, old_kind)), Some(new_kind)) = (d.old, d.new_kind()) else {
                return true;
            };
            // `!DIFF_FILE_VALID(p->one) || !oid_eq(...)`: a side with no id of its
            // own (worktree content) can always differ, which is what
            // [`Analysis::old_id`] carries once it has been hashed.
            let may_differ = an.old_id != an.new_id;
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

/// The rows [`super::diffstat::show_stats`] renders, built from the pairs that
/// survived `compute_diffstat()`.
fn stat_rows(pairs: &[(&Delta, &Analysis)], compact: bool) -> Vec<diffstat::StatFile> {
    pairs
        .iter()
        .copied()
        .map(|(d, an)| diffstat::StatFile {
            print_name: stat_display_name(d, compact),
            added: u64::from(an.added),
            deleted: u64::from(an.deleted),
            binary: an.binary,
            is_unmerged: d.unmerged,
        })
        .collect()
}

/// Whether `flag` is one of the four `--stat-*` geometry options.
///
/// Each is an `OPT_CALLBACK_F` with a required argument (diff.c:6100-6111), so
/// parse-options accepts both the glued `--opt=<n>` and the separated `--opt <n>`
/// spelling; only the glued one used to be recognised here.
pub(crate) fn is_stat_width_flag(flag: &str) -> bool {
    matches!(flag, "--stat-width" | "--stat-name-width" | "--stat-graph-width" | "--stat-count")
}

/// The `StatWidths` slot a `--stat-*` flag writes.
pub(crate) fn stat_width_slot_of<'a>(sw: &'a mut StatWidths, flag: &str) -> Option<&'a mut i64> {
    match flag {
        "--stat-width" => Some(&mut sw.width),
        "--stat-name-width" => Some(&mut sw.name_width),
        "--stat-graph-width" => Some(&mut sw.graph_width),
        "--stat-count" => Some(&mut sw.count),
        _ => None,
    }
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
    compact_comment_for_kinds(d.old.map(|(_, k)| k), d.new_kind())
}

/// The same annotation from the two sides' entry kinds alone, which is all
/// `fill_print_name()` reads. Shared with the history commands, whose change
/// records carry raw mode words rather than a [`Delta`].
pub(crate) fn compact_comment_for_modes(old: Option<u32>, new: Option<u32>) -> Option<&'static str> {
    compact_comment_for_kinds(old.map(kind_of_mode), new.map(kind_of_mode))
}

/// `S_ISLNK`/`S_IXUSR` on a raw mode word, in the terms the comment is phrased in.
fn kind_of_mode(mode: u32) -> EntryKind {
    match mode & 0o170000 {
        0o120000 => EntryKind::Link,
        0o160000 => EntryKind::Commit,
        _ if mode & 0o111 != 0 => EntryKind::BlobExecutable,
        _ => EntryKind::Blob,
    }
}

fn compact_comment_for_kinds(
    old: Option<EntryKind>,
    new: Option<EntryKind>,
) -> Option<&'static str> {
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
    submodule_inline_section(
        out,
        repo,
        path,
        &one,
        &two,
        delta.dirty_submodule,
        abbrev,
        colors,
        &r.src_prefix,
        &r.dst_prefix,
        r.hash_kind,
    );
}

/// `show_submodule_inline_diff()` (submodule.c:640) whole: the shared
/// `Submodule <path> <a>..<b>` header, then the submodule's own diff beneath it.
///
/// Split out from [`render_submodule`] so [`super::diff_pairs`] — and through it
/// `diff-tree`, `log -p` and `show` — reaches the same implementation instead of
/// growing a second one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn submodule_inline_section(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    path: &BString,
    one: &ObjectId,
    two: &ObjectId,
    dirty: u8,
    abbrev: usize,
    colors: &diff_color::DiffColors,
    src_prefix: &[u8],
    dst_prefix: &[u8],
    hash_kind: gix::hash::Kind,
) {
    let hdr = super::diff_pairs::show_submodule_header(out, repo, path, one, two, dirty, abbrev);
    // "We need a valid left and right commit to display a difference."
    if !(hdr.left.is_some() || one.is_null()) || !(hdr.right.is_some() || two.is_null()) {
        return;
    }
    match submodule_inline_diff(
        repo, path, &hdr, one, two, dirty, colors, src_prefix, dst_prefix, hash_kind,
    ) {
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
    src_prefix: &[u8],
    dst_prefix: &[u8],
    hash_kind: gix::hash::Kind,
) -> Option<Vec<u8>> {
    let empty_tree = gix::ObjectId::empty_tree(hash_kind);
    let old_oid = if hdr.left.is_some() { *one } else { empty_tree };
    let new_oid = if hdr.right.is_some() { *two } else { empty_tree };

    let workdir = repo.workdir()?;
    let dir = workdir.join(gix::path::from_bstr(path.as_bstr()).as_ref());
    let exe = crate::hosted::git_exe().ok()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("diff").arg("--submodule=diff");
    cmd.arg(format!(
        "--color={}",
        if colors.enabled() { "always" } else { "never" }
    ));
    // `-R` swaps which prefix each side is given; every other option keeps them.
    let (src, dst) = (src_prefix, dst_prefix);
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
    let old_hash = if delta.old.is_some() {
        an.old_id.to_hex_with_len(hlen).to_string()
    } else {
        null_hash.clone()
    };
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

    // `builtin_diff()` (diff.c:3596):
    //
    // ```c
    // if (o->flags.irreversible_delete && lbl[1][0] == '/') {
    //         emit_diff_symbol(o, DIFF_SYMBOL_HEADER, header.buf, header.len, 0);
    //         ...
    //         goto free_ab_and_return;
    // }
    // ```
    //
    // The test is on the *label*, so it is a deletion — the post-image is
    // `/dev/null` — and the jump lands past the binary arm as well as past the hunks.
    if r.irreversible_delete && matches!(delta.new, NewSide::Absent) {
        return Ok(());
    }

    if an.binary && !r.text {
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
        write_hunks(out, hunks, r.indicators);
    }
    Ok(())
}

/// Write a rendered hunk body, substituting each line's leading marker with the
/// configured `--output-indicator-*` character.
///
/// `emit_line_ws_markup()` (diff.c:1369) reads `o->output_indicators[sign_index]`
/// at the moment a line is written, so the substitution belongs here rather than in
/// the hunk builder: `--check`, `-S`/`-G` and the stat formats all walk the same
/// stored hunk text and need git's canonical markers. `@@` headers and
/// `\ No newline at end of file` start with bytes no indicator slot owns and are
/// copied through. The default triple is a straight copy.
fn write_hunks(out: &mut Vec<u8>, hunks: &[u8], indicators: (u8, u8, u8)) {
    let (ind_new, ind_old, ind_ctx) = indicators;
    if (ind_new, ind_old, ind_ctx) == (b'+', b'-', b' ') {
        out.extend_from_slice(hunks);
        return;
    }
    for line in hunks.split_inclusive(|&b| b == b'\n') {
        match line.first() {
            Some(b' ') => push_indicator(out, ind_ctx),
            Some(b'-') => push_indicator(out, ind_old),
            Some(b'+') => push_indicator(out, ind_new),
            _ => {
                out.extend_from_slice(line);
                continue;
            }
        }
        out.extend_from_slice(&line[1..]);
    }
}

/// `emit_line_0()` writes the sign only when it is non-zero, which is how
/// `--output-indicator-new=` (an empty value) drops the marker entirely —
/// `diff_opt_char()` (diff.c:5602) stores `arg[0]`, and for an empty argument that
/// is the NUL terminator.
fn push_indicator(out: &mut Vec<u8>, sign: u8) {
    if sign != 0 {
        out.push(sign);
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
pub(crate) fn apply_line_prefix(out: Vec<u8>, prefix: &[u8]) -> Vec<u8> {
    apply_line_prefix_except(out, prefix, &[])
}

/// [`apply_line_prefix`], leaving the half-open byte ranges in `verbatim` untouched.
///
/// An external diff driver writes to git's own output descriptor, so `emit_line()`
/// never sees its bytes and `diff_line_prefix()` never reaches them. This port
/// captures that output through a pipe and splices it into the same buffer as
/// everything else, so the spans it occupies have to be marked and skipped here.
/// `verbatim` is in ascending order and does not overlap.
pub(crate) fn apply_line_prefix_except(
    out: Vec<u8>,
    prefix: &[u8],
    verbatim: &[(usize, usize)],
) -> Vec<u8> {
    if prefix.is_empty() || out.is_empty() {
        return out;
    }
    let mut res = Vec::with_capacity(out.len() + prefix.len() * 2);
    let mut at_line_start = true;
    let mut spans = verbatim.iter().peekable();
    for (i, &b) in out.iter().enumerate() {
        while spans.peek().is_some_and(|(_, end)| *end <= i) {
            spans.next();
        }
        let inside = spans.peek().is_some_and(|(start, end)| i >= *start && i < *end);
        if at_line_start && !inside {
            res.extend_from_slice(prefix);
        }
        res.push(b);
        // A span always ends on a record boundary, so the byte after it starts a line.
        at_line_start = b == b'\n';
    }
    res
}

// ---------------------------------------------------------------------------
// path quoting (quote.c)
// ---------------------------------------------------------------------------

pub(super) use crate::quote::quoted_name;

/// `name_a += (*name_a == '/')` (diff.c:1899-1900 in `builtin_diff()`, and again at
/// diff.c:3899-3900 where the `diff --git` pair is built): exactly one leading
/// slash comes off the name before the `a/` / `b/` prefix goes on. An index path
/// never starts with `/`, so this only bites when the name came from the file
/// system — but git applies it to every name it prefixes, and so does this.
fn strip_one_leading_slash(path: &BString) -> &[u8] {
    let bytes = path.as_slice();
    match bytes.first() {
        Some(b'/') => &bytes[1..],
        _ => bytes,
    }
}

/// `quote_two_c_style()` for a single prefixed name (the `---`/`+++` lines).
fn quote_one(prefix: &[u8], path: &BString) -> Vec<u8> {
    crate::quote::quote_two_c_style(prefix, strip_one_leading_slash(path))
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

/// The blob at `path` in `tree`: its entry mode, its id and its bytes. A path the
/// tree does not hold as a blob reads as `None` / the null oid / no content, which
/// is exactly what `diff_tree_paths()` records for an absent side (tree-diff.c:247).
fn tree_blob(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    path: &BString,
) -> Result<(Option<EntryKind>, ObjectId, Vec<u8>)> {
    match tree_entry(tree, path)? {
        Some((_, EntryKind::Commit)) => {
            bail!("submodule/gitlink change at {path:?} is not supported")
        }
        // A directory at this path contributes no blob content of its own.
        Some((_, EntryKind::Tree)) => Ok((None, repo.object_hash().null(), Vec::new())),
        Some((id, kind)) => Ok((Some(kind), id, blob_bytes(repo, id)?)),
        None => Ok((None, repo.object_hash().null(), Vec::new())),
    }
}

/// One entry of a combined diff's path set: the result side plus, for each parent,
/// the side that parent contributes. Mirrors `struct combine_diff_path`
/// (combine-diff.h) as `diff_tree_paths()` fills it in.
struct CombinedPath {
    path: BString,
    /// `None` when the result tree does not hold the path, which git records as
    /// mode `000000` and the null object id.
    kind: Option<EntryKind>,
    id: ObjectId,
    bytes: Vec<u8>,
    parents: Vec<CombinedSide>,
}

/// One parent's side of a [`CombinedPath`].
struct CombinedSide {
    kind: Option<EntryKind>,
    id: ObjectId,
    bytes: Vec<u8>,
    /// `p->parent[i].status` (tree-diff.c:237): `D` when the result dropped the
    /// path, `M` when this parent held it too, `A` when only the result holds it.
    status: u8,
}

/// The paths a combined diff covers: every path whose result-side entry differs
/// from *every* parent's, which is the intersection
/// `D(A,P1) ^ ... ^ D(A,Pn)` that `intersect_paths()` (combine-diff.c:31) folds and
/// `find_paths_multitree()` (combine-diff.c:1430) walks the trees for. A path that
/// matches even one parent is a one-sided change and never appears.
fn combined_path_set(
    repo: &gix::Repository,
    result_tree: &gix::Tree<'_>,
    parent_trees: &[gix::Tree<'_>],
    paths: &[String],
) -> Result<Vec<CombinedPath>> {
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

    let mut out = Vec::with_capacity(cand.len());
    for path in cand {
        let (kind, id, bytes) = tree_blob(repo, result_tree, &path)?;
        let mut parents = Vec::with_capacity(parent_trees.len());
        let mut shared_with_a_parent = false;
        for pt in parent_trees {
            let (p_kind, p_id, p_bytes) = tree_blob(repo, pt, &path)?;
            if (p_kind, p_id) == (kind, id) {
                shared_with_a_parent = true;
            }
            parents.push(CombinedSide {
                status: match (p_kind.is_some(), kind.is_some()) {
                    (_, false) => b'D',
                    (true, true) => b'M',
                    (false, true) => b'A',
                },
                kind: p_kind,
                id: p_id,
                bytes: p_bytes,
            });
        }
        if shared_with_a_parent {
            continue;
        }
        out.push(CombinedPath { path, kind, id, bytes, parents });
    }
    Ok(out)
}

/// `show_raw_diff()` (combine-diff.c:1228): the `--raw` / `--name-only` /
/// `--name-status` record for one combined path — one colon and one mode per
/// parent, then the result's mode, then one object name per parent and the
/// result's, then one status letter per parent.
///
/// `line_prefix` is printed only on the `--raw` branch (combine-diff.c:1244), so
/// `--line-prefix` with a bare `--name-only`/`--name-status` leaves these records
/// unprefixed.
fn render_combined_raw(
    out: &mut Vec<u8>,
    cp: &CombinedPath,
    fmt: u32,
    r: &Render,
    line_prefix: &[u8],
) {
    render_combined_raw_at(out, cp, fmt, r.raw_abbrev, r.z, line_prefix);
}

/// [`render_combined_raw`] with the two `Render` fields it reads passed directly,
/// so the history verbs — which carry their own abbreviation width and `-z` flag
/// rather than a `Render` — can emit the same record.
fn render_combined_raw_at(
    out: &mut Vec<u8>,
    cp: &CombinedPath,
    fmt: u32,
    raw_abbrev: usize,
    z: bool,
    line_prefix: &[u8],
) {
    if fmt & F_RAW != 0 {
        out.extend_from_slice(line_prefix);
        for _ in &cp.parents {
            out.push(b':');
        }
        for p in &cp.parents {
            push_str(out, &mode_octal(p.kind));
            out.push(b' ');
        }
        push_str(out, &mode_octal(cp.kind));
        for p in &cp.parents {
            out.push(b' ');
            push_str(out, &p.id.to_hex_with_len(raw_abbrev).to_string());
        }
        out.push(b' ');
        push_str(out, &cp.id.to_hex_with_len(raw_abbrev).to_string());
        out.push(b' ');
    }
    if fmt & (F_RAW | F_NAME_STATUS) != 0 {
        for p in &cp.parents {
            out.push(p.status);
        }
        // `-z` drops the inter-name terminator along with the record terminator.
        out.push(if z { 0 } else { b'\t' });
    }
    out.extend_from_slice(&name_field(&cp.path, z));
    out.push(if z { 0 } else { b'\n' });
}

/// `diff_tree_combined()`'s raw block (combine-diff.c:1600-1606): one
/// `show_raw_diff()` record per combined path, for `git show`/`git log` on a merge
/// under `-c`/`--cc`. `raw` picks the full `::<modes> <ids> <statuses>` form over
/// the bare `--name-status` one; `abbrev` is the width the caller's `--abbrev`
/// settled on, since the raw columns answer to it.
pub(crate) fn merge_combined_raw(
    repo: &gix::Repository,
    commit: ObjectId,
    parents: &[ObjectId],
    paths: &[String],
    abbrev: usize,
    z: bool,
    raw: bool,
) -> Result<Vec<u8>> {
    let result_tree = repo.find_commit(commit)?.tree()?;
    let mut parent_trees: Vec<gix::Tree<'_>> = Vec::with_capacity(parents.len());
    for p in parents {
        parent_trees.push(repo.find_commit(*p)?.tree()?);
    }
    let set = combined_path_set(repo, &result_tree, &parent_trees, paths)?;
    let fmt = if raw { F_RAW } else { F_NAME_STATUS };
    let mut out = Vec::new();
    for cp in &set {
        render_combined_raw_at(&mut out, cp, fmt, abbrev, z, b"");
    }
    Ok(out)
}

/// `dump_quoted_path()` (combine-diff.c:905): the line prefix, then the head, then
/// the name written plain. `emit_diff_symbol()`'s `FILEPAIR_MINUS`/`FILEPAIR_PLUS`
/// (diff.c) appends a tab when the name holds a space; this path never does.
fn dump_quoted_path(out: &mut Vec<u8>, line_prefix: &[u8], head: &[u8], name: &[u8]) {
    out.extend_from_slice(line_prefix);
    out.extend_from_slice(head);
    out.extend_from_slice(name);
    out.push(b'\n');
}

/// What `builtin_diff_combined()` (builtin/diff.c:211) hands `diff_tree_combined()`
/// that the ordinary pair machinery does not carry: which revision is the result,
/// which are its parents, and the two settings `show_combined_header()` reads
/// straight off the options rather than off a pair.
struct CombinedRequest {
    /// `ent[first_non_parent]`: the first revision on the command line.
    result: String,
    /// Every other revision, in command-line order.
    parents: Vec<String>,
    /// The output formats as they stood before the stat half narrowed them.
    fmt: u32,
    /// `-R`: `git diff` always takes `find_paths_generic()` (combine-diff.c:1378),
    /// because `cmd_diff()` sets `skip_stat_unmatch` (builtin/diff.c:525) — so the
    /// path set is folded out of the diff *queue*, whose pairs `diff_change()`
    /// (diff.c) has already swapped.
    reverse: bool,
    /// `opt->a_prefix ? : "a/"` (combine-diff.c:931), as configured: the combined
    /// header reads it directly, so `-R` does not swap it the way it swaps the
    /// ordinary `diff --git` pair's.
    a_prefix: Vec<u8>,
    /// `opt->b_prefix ? : "b/"` (combine-diff.c:932).
    b_prefix: Vec<u8>,
}

/// `-R` on a combined diff, as `intersect_paths()` (combine-diff.c:52) records it
/// off reversed pairs: it takes the result side from `pair->two` and parent `i`'s
/// from `pair->one`, and `diff_change()` has swapped those. So every parent ends
/// up holding what the result held, and the result holds what parent 0 held —
/// which is why `git diff -R <a> <b> <c>` shows one tree against the first
/// revision repeated, and drops the later parents' content entirely.
fn reverse_combined(set: &mut [CombinedPath]) {
    for cp in set {
        let (kind, id, bytes) = (cp.kind, cp.id, std::mem::take(&mut cp.bytes));
        cp.kind = cp.parents[0].kind;
        cp.id = cp.parents[0].id;
        cp.bytes = cp.parents[0].bytes.clone();
        for p in &mut cp.parents {
            // `diff_resolve_rename_copy()` (diffcore-rename.c) names a pair from
            // its sides in order: the old result is the pre-image now, so its
            // absence reads as `A` and the parent's as `D`.
            p.status = if kind.is_none() {
                b'A'
            } else if p.kind.is_none() {
                b'D'
            } else {
                b'M'
            };
            p.kind = kind;
            p.id = id;
            p.bytes = bytes.clone();
        }
    }
}

/// The combined half of `diff_tree_combined()` (combine-diff.c:1606-1626) for
/// `git diff <result> <parent>...`: the raw/name formats, then — separated by a
/// blank line from whatever came before it — the combined patch.
///
/// `separator` carries `needsep` in and out: the stat formats the caller already
/// rendered set it, the raw formats set it here, and the patch consumes it.
#[allow(clippy::too_many_arguments)]
fn emit_combined(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    req: &CombinedRequest,
    paths: &[String],
    ctx: u32,
    r: &Render,
    separator: &mut bool,
    line_prefix: &[u8],
    colors: &diff_color::DiffColors,
) -> Result<()> {
    // `-s` / `--no-patch` is an assignment, so it leaves nothing to serve unless a
    // later format flag put a bit back.
    if req.fmt & !F_NO_OUTPUT == 0 {
        return Ok(());
    }

    let result_tree = repo.rev_parse_single(req.result.as_str())?.object()?.peel_to_tree()?;
    let mut parent_trees: Vec<gix::Tree<'_>> = Vec::with_capacity(req.parents.len());
    for p in &req.parents {
        parent_trees.push(repo.rev_parse_single(p.as_str())?.object()?.peel_to_tree()?);
    }
    let mut set = combined_path_set(repo, &result_tree, &parent_trees, paths)?;
    // `if (num_paths)`: an empty intersection prints neither a separator nor a patch,
    // however much the first-parent stat formats already wrote.
    if set.is_empty() {
        return Ok(());
    }
    // Reversing pairs does not change *which* paths differ, only which side of each
    // one is the pre-image, so the intersection above is built first either way.
    if req.reverse {
        reverse_combined(&mut set);
    }

    if req.fmt & (F_RAW | F_NAME | F_NAME_STATUS) != 0 {
        for cp in &set {
            render_combined_raw(out, cp, req.fmt, r, line_prefix);
        }
        *separator = true;
    }

    if req.fmt & F_PATCH != 0 {
        if *separator {
            // `printf("%s%c", diff_line_prefix(opt), opt->line_termination)`
            // (combine-diff.c:1621): under `-z` that terminator is a NUL.
            out.extend_from_slice(line_prefix);
            out.push(if r.z { 0 } else { b'\n' });
        }
        let abbrev = if r.full_index {
            r.hash_kind.len_in_hex()
        } else {
            crate::abbrev::configured_abbrev(repo, r.hash_kind.len_in_hex())
        };
        out.extend_from_slice(&combined_patch(
            &set,
            ctx,
            true,
            abbrev,
            &req.a_prefix,
            &req.b_prefix,
            line_prefix,
            colors,
        )?);
    }
    Ok(())
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

    Ok(combined_path_set(repo, &result_tree, &parent_trees, paths)?
        .into_iter()
        .map(|cp| {
            let letters = cp.parents.iter().map(|p| char::from(p.status)).collect();
            (cp.path, letters)
        })
        .collect())
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
    merge_combined_patch_painted(
        repo,
        commit,
        parents,
        paths,
        ctx,
        dense,
        &diff_color::DiffColors::disabled(),
    )
}

/// [`merge_combined_patch`] with the palette `dump_sline()` paints with.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_combined_patch_painted(
    repo: &gix::Repository,
    commit: ObjectId,
    parents: &[ObjectId],
    paths: &[String],
    ctx: u32,
    dense: bool,
    colors: &diff_color::DiffColors,
) -> Result<Vec<u8>> {
    let result_tree = repo.find_commit(commit)?.tree()?;
    let mut parent_trees: Vec<gix::Tree<'_>> = Vec::with_capacity(parents.len());
    for p in parents {
        parent_trees.push(repo.find_commit(*p)?.tree()?);
    }
    combined_trees_patch_painted(repo, &result_tree, &parent_trees, paths, ctx, dense, colors)
}

/// The combined diff of `result_tree` against every parent tree, with the header
/// flavour chosen: `diff --cc` for the dense form and
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
    combined_trees_patch_painted(
        repo,
        result_tree,
        parent_trees,
        paths,
        ctx,
        dense,
        &diff_color::DiffColors::disabled(),
    )
}

/// [`combined_trees_patch_headed`] with the palette `dump_sline()` paints with, for
/// the callers that colorize (`git show --color-words` on a merge).
#[allow(clippy::too_many_arguments)]
pub(crate) fn combined_trees_patch_painted(
    repo: &gix::Repository,
    result_tree: &gix::Tree<'_>,
    parent_trees: &[gix::Tree<'_>],
    paths: &[String],
    ctx: u32,
    dense: bool,
    colors: &diff_color::DiffColors,
) -> Result<Vec<u8>> {
    let set = combined_path_set(repo, result_tree, parent_trees, paths)?;
    let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
    combined_patch(&set, ctx, dense, abbrev, b"a/", b"b/", b"", colors)
}

/// `show_patch_diff()` (combine-diff.c:1015) over an already-built path set: one
/// `show_combined_header()` plus `dump_sline()` per path that has hunks to show or
/// a mode that differs from a parent's.
///
/// `abbrev` is `show_combined_header()`'s own
/// `opt->flags.full_index ? the_hash_algo->hexsz : DEFAULT_ABBREV`
/// (combine-diff.c:933) — the `index` line here answers to `--full-index` but not
/// to `--abbrev`, which only reaches the raw format.
///
/// `a_prefix`/`b_prefix` are `opt->a_prefix ? : "a/"` and `opt->b_prefix ? : "b/"`
/// (combine-diff.c:931-932), which `--no-prefix` and `--src-prefix`/`--dst-prefix`
/// replace.
///
/// `line_prefix` goes on every line except the `mode <old>..<new>` one:
/// `show_combined_header()` prints that continuation with a bare `printf("mode ")`
/// (combine-diff.c:971), so `--line-prefix` misses it. Callers that prefix their
/// whole output afterwards pass an empty prefix here.
#[allow(clippy::too_many_arguments)]
fn combined_patch(
    set: &[CombinedPath],
    ctx: u32,
    dense: bool,
    abbrev: usize,
    a_prefix: &[u8],
    b_prefix: &[u8],
    line_prefix: &[u8],
    colors: &diff_color::DiffColors,
) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    for cp in set {
        if cp.parents.len() != NUM_PARENT {
            bail!("combined diff of more than two parents is not supported");
        }
        let parent_bytes: Vec<Vec<u8>> = cp.parents.iter().map(|p| p.bytes.clone()).collect();
        let (sline, cnt) = build_combined_sline(&cp.bytes, &parent_bytes, ctx);
        let show_hunks = sline_has_marks(&sline, cnt);
        // `for (i = 0; i < num_parent; i++) if (elem->parent[i].mode != elem->mode)`
        // (combine-diff.c:1123-1128).
        let mode_differs = cp.parents.iter().any(|p| p.kind != cp.kind);
        // `if (show_hunks || mode_differs || working_tree_file)` (combine-diff.c:1206):
        // a path whose content matches a parent but whose mode does not still gets a
        // header, with no hunks under it.
        if !show_hunks && !mode_differs {
            continue;
        }

        // ---- `show_combined_header()` (combine-diff.c:922) ------------------
        out.extend_from_slice(line_prefix);
        push_str(&mut out, if dense { "diff --cc " } else { "diff --combined " });
        out.extend_from_slice(&quoted_name(&cp.path));
        out.push(b'\n');
        out.extend_from_slice(line_prefix);
        push_str(&mut out, "index ");
        for (i, p) in cp.parents.iter().enumerate() {
            if i != 0 {
                out.push(b',');
            }
            push_str(&mut out, &p.id.to_hex_with_len(abbrev).to_string());
        }
        push_str(&mut out, "..");
        push_str(&mut out, &cp.id.to_hex_with_len(abbrev).to_string());
        out.push(b'\n');

        // `deleted` is "the result has no mode"; `added` is "and nobody had it",
        // i.e. every parent reported the path as added (combine-diff.c:958-983).
        let mut added = false;
        let mut deleted = false;
        if mode_differs {
            deleted = cp.kind.is_none();
            added = !deleted && cp.parents.iter().all(|p| p.status == b'A');
            if added {
                out.extend_from_slice(line_prefix);
                push_str(&mut out, "new file mode ");
                push_str(&mut out, &mode_octal(cp.kind));
            } else {
                if deleted {
                    out.extend_from_slice(line_prefix);
                    push_str(&mut out, "deleted file ");
                }
                push_str(&mut out, "mode ");
                for (i, p) in cp.parents.iter().enumerate() {
                    if i != 0 {
                        out.push(b',');
                    }
                    push_str(&mut out, &mode_octal(p.kind));
                }
                if cp.kind.is_some() {
                    push_str(&mut out, "..");
                    push_str(&mut out, &mode_octal(cp.kind));
                }
            }
            out.push(b'\n');
        }

        if added {
            dump_quoted_path(&mut out, line_prefix, b"--- ", b"/dev/null");
        } else {
            dump_quoted_path(&mut out, line_prefix, b"--- ", &quote_one(a_prefix, &cp.path));
        }
        if deleted {
            dump_quoted_path(&mut out, line_prefix, b"+++ ", b"/dev/null");
        } else {
            dump_quoted_path(&mut out, line_prefix, b"+++ ", &quote_one(b_prefix, &cp.path));
        }
        // `dump_sline()` prefixes every line it prints (combine-diff.c:809, 841,
        // 853), which a whole-buffer pass over its output reproduces.
        let mut hunks: Vec<u8> = Vec::new();
        dump_sline(&mut hunks, &sline, cnt, ctx);
        out.extend_from_slice(&apply_line_prefix(hunks, line_prefix));
    }
    Ok(colorize_combined(out, colors, line_prefix, NUM_PARENT))
}

/// The palette `show_patch_diff()` and `dump_sline()` paint a combined section
/// with (combine-diff.c:1015-1230): `c_meta` on the `diff --cc`/`index`/mode and
/// `---`/`+++` lines, `c_frag` on the `@@@` header, and, for a body line, `c_old`
/// when any of its sign columns is `-`, `c_new` when any is `+`, and `c_plain`
/// otherwise. Every one of them closes with `c_reset` — which is why a context
/// line comes out as the text followed by a bare reset.
///
/// Applied as a pass over the assembled section because git writes the color after
/// `diff_line_prefix(opt)` and before the text; the prefix is already in place
/// here, so it is stepped over rather than re-emitted. With a disabled table every
/// lookup is the empty string and the pass copies the bytes through unchanged.
fn colorize_combined(
    out: Vec<u8>,
    colors: &diff_color::DiffColors,
    line_prefix: &[u8],
    num_parent: usize,
) -> Vec<u8> {
    let reset = colors.reset();
    if reset.is_empty() {
        return out;
    }
    let mut res: Vec<u8> = Vec::with_capacity(out.len());
    for line in out.split_inclusive(|&b| b == b'\n') {
        let (nl, body) = match line.strip_suffix(b"\n") {
            Some(b) => (true, b),
            None => (false, line),
        };
        let rest = body.strip_prefix(line_prefix).unwrap_or(body);
        let slot = if rest.starts_with(b"diff --cc ")
            || rest.starts_with(b"diff --combined ")
            || rest.starts_with(b"index ")
            || rest.starts_with(b"--- ")
            || rest.starts_with(b"+++ ")
            || rest.starts_with(b"new file mode ")
            || rest.starts_with(b"deleted file mode ")
            || rest.starts_with(b"mode ")
        {
            diff_color::DiffSlot::Meta
        } else if rest.starts_with(b"@") {
            diff_color::DiffSlot::Frag
        } else {
            let signs = &rest[..num_parent.min(rest.len())];
            if signs.contains(&b'-') {
                diff_color::DiffSlot::Old
            } else if signs.contains(&b'+') {
                diff_color::DiffSlot::New
            } else {
                diff_color::DiffSlot::Context
            }
        };
        let split = body.len() - rest.len();
        res.extend_from_slice(&body[..split]);
        push_str(&mut res, colors.get(slot));
        res.extend_from_slice(rest);
        push_str(&mut res, reset);
        if nl {
            res.push(b'\n');
        }
    }
    res
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
    // This renders into the ordinary output buffer, which the caller prefixes as a
    // whole, so no prefix is emitted here.
    dump_quoted_path(out, b"", b"--- ", &quote_one(b"a/", &delta.path));
    dump_quoted_path(out, b"", b"+++ ", &quote_one(b"b/", &delta.path));

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
    /// `xecfg->find_func`: the path's userdiff driver funcname pattern, when it has
    /// one. `None` leaves git's built-in [`super::diff_pairs::def_ff`] in charge.
    funcname: Option<&'a crate::userdiff::FuncName>,
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
    func_line_with(None, before, start, limit)
}

/// [`func_line`] with `xecfg->find_func` supplied: a path whose `diff` gitattribute
/// selects a driver carrying a `funcname`/`xfuncname` pattern reads its headings off
/// that pattern instead of [`def_ff`], and a line the pattern rejects is simply not a
/// heading — there is no fall-back to the built-in heuristic.
pub(crate) fn func_line_with<'a>(
    ff: Option<&crate::userdiff::FuncName>,
    before: &[&'a [u8]],
    start: i64,
    limit: i64,
) -> Option<&'a [u8]> {
    let step: i64 = if start > limit { -1 } else { 1 };
    let mut l = start;
    while l != limit && l >= 0 && (l as usize) < before.len() {
        let rec = before[l as usize];
        let hit = match ff {
            Some(f) => f.find(rec, FUNC_LINE_MAX),
            None => def_ff(rec, FUNC_LINE_MAX),
        };
        if let Some(text) = hit {
            return Some(text);
        }
        l += step;
    }
    None
}

impl PatchSink<'_> {
    fn func_line(&self, start: i64, limit: i64) -> Option<&[u8]> {
        func_line_with(self.funcname, self.before, start, limit)
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
/// breaks a whitespace rule into `out`, and say whether any did.
///
/// The hunk text the analysis already produced is what git walks: each `@@`
/// header resets the new-file line counter, and every `+` line is checked and,
/// when it fails, printed under a `<path>:<line>: <problems>.` header. A blank
/// line inside the run the change lengthened at end-of-file additionally trips
/// `blank-at-eof`, which is why the analysis carries that boundary.
fn report_whitespace_to(
    out: &mut Vec<u8>,
    delta: &Delta,
    analysis: &Analysis,
    ws_rule: u32,
    colors: &diff_color::DiffColors,
) -> bool {
    let set = colors.get(diff_color::DiffSlot::New);
    let ws_color = colors.get(diff_color::DiffSlot::Whitespace);
    let reset = colors.reset();
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
                let _ = writeln!(
                    out,
                    "{}:{lineno}: {}.",
                    delta.path,
                    super::diff_color::whitespace_error_string(bad)
                );
                // `emit_line(o, set, reset, line, 1)` prints the `+` marker, then
                // `ws_check_emit()` repaints the body around its offending runs
                // (diff.c `checkdiff_consume`).
                push_str(out, set);
                out.push(b'+');
                push_str(out, reset);
                let mut with_nl: Vec<u8> = body.to_vec();
                if !with_nl.ends_with(b"\n") {
                    with_nl.push(b'\n');
                }
                diff_color::ws_check_emit(out, &with_nl, ws_rule, set, reset, ws_color);
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
        let _ = writeln!(
            out,
            "{}:{blank_at_eof}: {}.",
            delta.path,
            super::diff_color::whitespace_error_string(super::diff_color::WS_BLANK_AT_EOF)
        );
    }
    found
}

/// `--check` for one commit of a history verb: `diff_flush_checkdiff()` over the
/// pairs `log_tree_diff()` queued, in place of every other output format.
///
/// `DIFF_FORMAT_CHECKDIFF` clears the other format bits in `diff_setup_done()`, so
/// a commit under `--check` prints its header and then only this — no patch, no
/// stat, no raw. Returns `o->flags.check_failed`, which `diff_result_code()` turns
/// into the `02` bit of the exit status.
pub(crate) fn commit_check(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit_id: ObjectId,
    parent: Option<ObjectId>,
    opts: &PatchOpts,
    paths: &[String],
) -> Result<bool> {
    let colors = &opts.colors;
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;
    let mut specs = match paths.is_empty() {
        true => None,
        false => Some(super::log::PathspecMatcher::new(repo, paths)?),
    };
    let r = patch_render(repo, opts);
    let mut drivers = DriverCache::new(repo)?;
    let deltas = commit_deltas(
        repo,
        &mut cache,
        &mut drivers,
        commit_id,
        parent,
        opts,
        specs.as_mut(),
        false,
        true,
    )?;
    let hash_kind = repo.object_hash();
    let ws_rule = diff_color::whitespace_rule_cfg(repo);
    let mut found = false;
    for queued in &deltas {
        // `run_checkdiff()` sits downstream of `run_diff()`'s type-change split, as
        // the patch path does.
        let halves = split_type_change(queued);
        let steps: Vec<&Delta> = match &halves {
            Some((del, add)) => vec![del, add],
            None => vec![queued],
        };
        for delta in steps {
            let an = analyze(
                &mut cache,
                &repo.objects,
                delta,
                opts.ctx,
                opts.ws,
                opts.indent_heuristic,
                hash_kind,
                None,
                true,
                opts.algorithm,
                None,
                false,
                r.binary,
                opts.func_context,
                &IgnoreOpts {
                    text: opts.text,
                    blank_lines: opts.blank_lines,
                    lines: opts.ignore_lines.clone(),
                    inter_hunk_ctx: opts.inter_hunk_ctx,
                },
            )?;
            found |= report_whitespace_to(out, delta, &an, ws_rule, colors);
        }
    }
    Ok(found)
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

#[cfg(test)]
mod type_change_tests {
    use super::*;

    fn blob_id(byte: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[byte; 20])
    }

    fn pair(old: EntryKind, new: EntryKind) -> Delta {
        Delta::plain(
            BString::from("g.txt"),
            Some((blob_id(1), old)),
            NewSide::Blob(blob_id(2), new),
        )
    }

    /// `run_diff()` (diff.c:5054) compares `S_IFMT & mode`, so a permission change
    /// is not a type change while a blob/symlink or blob/gitlink swap is. Verified
    /// against git 2.55.0: `git diff` over a `100644` → `100755` change prints one
    /// section with `old mode`/`new mode`, and over `100644` → `120000` prints a
    /// deletion section followed by a creation section.
    #[test]
    fn only_a_change_of_s_ifmt_counts_as_a_type_change() {
        assert!(!pair(EntryKind::Blob, EntryKind::BlobExecutable).type_changed());
        assert!(!pair(EntryKind::Blob, EntryKind::Blob).type_changed());
        assert!(pair(EntryKind::Blob, EntryKind::Link).type_changed());
        assert!(pair(EntryKind::Link, EntryKind::BlobExecutable).type_changed());
        assert!(pair(EntryKind::Blob, EntryKind::Commit).type_changed());
        // A creation and a deletion have only one valid side, so neither is one.
        let creation = Delta::plain(
            BString::from("g.txt"),
            None,
            NewSide::Blob(blob_id(2), EntryKind::Link),
        );
        assert!(!creation.type_changed());
        let deletion = Delta::plain(
            BString::from("g.txt"),
            Some((blob_id(1), EntryKind::Blob)),
            NewSide::Absent,
        );
        assert!(!deletion.type_changed());
    }

    /// The two halves `run_diff()` hands to `run_diff_cmd()`: the pre-image against
    /// an invalid post-image, then an invalid pre-image against the post-image. Both
    /// keep the pair's single name, since no rename is ever scored across a type
    /// change.
    #[test]
    fn a_type_change_splits_into_a_deletion_then_a_creation() {
        let p = pair(EntryKind::Blob, EntryKind::Link);
        let (del, add) = split_type_change(&p).expect("blob -> symlink splits");

        assert_eq!(del.path, p.path);
        assert_eq!(del.old.map(|(id, k)| (id, k)), Some((blob_id(1), EntryKind::Blob)));
        assert!(matches!(del.new, NewSide::Absent));
        assert_eq!(del.status, b'D');

        assert_eq!(add.path, p.path);
        assert!(add.old.is_none());
        assert!(matches!(add.new, NewSide::Blob(id, EntryKind::Link) if id == blob_id(2)));
        assert_eq!(add.status, b'A');

        // Everything else stays whole, which is what keeps the split invisible to
        // every pair git renders as one section.
        assert!(split_type_change(&pair(EntryKind::Blob, EntryKind::BlobExecutable)).is_none());
        assert!(split_type_change(&pair(EntryKind::Blob, EntryKind::Blob)).is_none());
    }

    /// A worktree post-image splits the same way, with the creation half still
    /// reading the worktree: that is the half whose content is the new symlink.
    #[test]
    fn a_worktree_type_change_keeps_the_worktree_on_the_creation_half() {
        let p = Delta::plain(
            BString::from("g.txt"),
            Some((blob_id(1), EntryKind::Blob)),
            NewSide::Worktree(EntryKind::Link),
        );
        let (del, add) = split_type_change(&p).expect("blob -> worktree symlink splits");
        assert!(matches!(del.new, NewSide::Absent));
        assert!(matches!(add.new, NewSide::Worktree(EntryKind::Link)));
    }

    /// `diff_change()` swaps the two filespecs, `oid_valid` and the dirty-submodule
    /// bits included, before anything downstream sees the pair. A worktree
    /// post-image therefore becomes a worktree *pre*-image: no id of its own, read
    /// by path — which is the whole of what `-R` on a worktree diff needs.
    #[test]
    fn reversing_moves_the_worktree_side_onto_the_pre_image() {
        let null = ObjectId::null(gix::hash::Kind::Sha1);
        let mut d = Delta::plain(
            BString::from("f.txt"),
            Some((blob_id(1), EntryKind::Blob)),
            NewSide::Worktree(EntryKind::Blob),
        );
        reverse_delta(&mut d, null);
        assert_eq!(d.old, Some((null, EntryKind::Blob)));
        assert!(d.old_worktree, "the file is now the pre-image");
        assert!(matches!(d.new, NewSide::Blob(id, EntryKind::Blob) if id == blob_id(1)));
        // `--raw` prints the post-image id, which after the swap is the object's.
        assert_eq!(d.new_id, Some(blob_id(1)));

        // Reversing a deletion gives a creation, with no worktree side at all.
        let mut d = Delta::plain(
            BString::from("f.txt"),
            Some((blob_id(1), EntryKind::Blob)),
            NewSide::Absent,
        );
        reverse_delta(&mut d, null);
        assert_eq!(d.old, None);
        assert!(!d.old_worktree);
        assert!(matches!(d.new, NewSide::Blob(id, EntryKind::Blob) if id == blob_id(1)));
    }

    /// A worktree gitlink is the one worktree side that carries an id — the commit
    /// the submodule has checked out — so the swap moves that id onto the pre-image
    /// while `--raw` keeps printing what `p->one->oid` holds, and the `-dirty`
    /// marker travels with the side it describes.
    #[test]
    fn reversing_carries_a_submodules_commit_and_dirt_onto_the_pre_image() {
        let null = ObjectId::null(gix::hash::Kind::Sha1);
        let mut d = Delta::plain(
            BString::from("sub"),
            Some((blob_id(1), EntryKind::Commit)),
            NewSide::Worktree(EntryKind::Commit),
        );
        d.new_commit = Some(blob_id(2));
        d.dirty_submodule = 1;
        d.new_id = None; // the submodule moved, so git prints all-zero for it
        reverse_delta(&mut d, null);
        assert_eq!(d.old, Some((blob_id(2), EntryKind::Commit)));
        assert!(d.old_worktree);
        assert_eq!(d.old_dirty_submodule, 1);
        assert_eq!(d.dirty_submodule, 0);
        assert_eq!(d.old_raw_id, None, "a moved submodule has no printable id");
        assert!(matches!(d.new, NewSide::Blob(id, EntryKind::Commit) if id == blob_id(1)));
    }
}
