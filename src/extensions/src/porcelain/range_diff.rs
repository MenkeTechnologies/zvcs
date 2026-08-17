//! `git range-diff` — compare two versions of a patch series.
//!
//! A port of upstream `range-diff.c`, `builtin/range-diff.c` and
//! `linear-assignment.c` on top of the vendored gitoxide. The pipeline is
//! reproduced stage for stage:
//!
//! 1. `read_patches()` — walk each commit range (merges excluded, oldest first)
//!    and render every commit into the *canonical patch text* upstream builds by
//!    post-processing `git log --no-color -p --no-merges --reverse --date-order
//!    --no-prefix --pretty=medium` output: a ` ## Metadata ##` block holding the
//!    (mailmap-resolved) `Author:` line, a ` ## Commit message ##` block holding
//!    the 4-space-indented, right-trimmed message, and one ` ## <path> ##`
//!    section per changed file whose hunk headers are rewritten to
//!    `@@ <path>: <function>` — so hunk line numbers never enter the comparison.
//!    Because upstream feeds the `diff --git` header block through
//!    `parse_git_diff_header()`, the `index`/`--- `/`+++ `/`new file mode` lines
//!    are consumed rather than kept: abbreviated blob ids are irrelevant to the
//!    output, and this port does not compute them.
//! 2. `find_exact_matches()` — hash the diff portion of every left patch and
//!    pair off byte-identical right patches. Upstream's hashmap chains are LIFO,
//!    so duplicate left patches match highest-index-first; reproduced.
//! 3. `get_correspondences()` — build the `n x n` cost matrix from `diffsize()`
//!    (a 3-context diff-of-diffs *without* the indent heuristic, counting hunks
//!    plus lines), pad it with `diffsize * creation_factor / 100` create/delete
//!    entries, and solve it with `compute_assignment()`, a direct port of
//!    `linear-assignment.c` (Jonker–Volgenant shortest augmenting path).
//! 4. `output()` — emit the `1:  abc123 ! 2:  def456 <subject>` pair headers and,
//!    for each matched pair, the diff-of-diffs indented by four spaces, with no
//!    file headers (`suppress_diff_headers`) and the hunk header reduced to `@@`
//!    plus a section name (`suppress_hunk_header_line_count`). The section name
//!    comes from upstream's `section_headers` userdiff driver — the two patterns
//!    `^ ## (.*) ##$` and `^.?@@ (.*)$` — ported by hand together with
//!    `ff_regexp()`'s 80-byte cap and trailing-whitespace trim, the backwards
//!    search bounded by the previous hunk, and `xdl_emit_diff()`'s quirk that a
//!    hunk with no match repeats the previous hunk's section name.
//!
//! ### Covered (stdout byte-identical to stock git, exit code included)
//!
//! * `range-diff <range1> <range2>`, `range-diff <rev1>...<rev2>` and
//!   `range-diff <base> <rev1> <rev2>`, dispatched with upstream's precedence
//!   (three committishes first, then two ranges, then one symmetric range). A
//!   `--` at argv index 1, 2 or 3 *forces* the matching form, reporting the same
//!   `not a symmetric range` / `not a commit range` / `not a revision` usage
//!   errors when its operands do not fit.
//! * Ranges spelled `<a>..<b>` or `<a>...<b>`, either side defaulting to `HEAD`
//!   when empty, plus every other spelling gitoxide's rev-parse resolves —
//!   `<rev>^!`, `<rev>^@`, `^<rev>` — recognised both as a range operand and for
//!   the walk.
//! * A trailing `-- <path>...` pathspec of plain paths and directory prefixes,
//!   which limits the range to commits touching a matched path and each rendered
//!   patch to the matched file sections, exactly as `git log -- <path>` does. A
//!   magic (`:(glob)`, `:!exclude`, …) or wildcard pathspec stops rather than
//!   match with different semantics.
//! * `--creation-factor=<n>` (and its `--creation-factor <n>` /
//!   `--no-creation-factor` spellings), `--left-only`, `--right-only`,
//!   `--no-dual-color`, `--no-color`, `--color=never`, `--color=auto`, `-p` /
//!   `-u` / `--patch`, and `--ws-error-highlight=<kind>` (a no-op with color
//!   off, which is the only mode this port emits). Dual and simple coloring are
//!   byte-identical once color is off.
//! * `--notes[=<ref>]` / `--no-notes`, upstream's `notes_callback()` reproduced
//!   through [`super::notes::DisplayOpt`]: notes are on by default (the inner
//!   `git log` uses a built-in pretty format, so `cmd_log_init_finish()` enables
//!   the default tree), a bare `--notes` re-adds that tree plus
//!   `notes.displayRef`, `--notes=<ref>` adds `<ref>` *instead* of the default —
//!   its block reads ` ## Notes (<ref>) ##` — and the two together print both
//!   blocks, in that order.
//! * `-s` / `--no-patch`: `DIFF_FORMAT_NO_OUTPUT`, which keeps the pair headers
//!   (`=`/`!`/`<`/`>` and the abbreviated ids) and drops every diff-of-diffs
//!   body — reproduced by suppressing the inner `patch_diff()` call. `--quiet`
//!   is the same output: `flags.quick` assigns `DIFF_FORMAT_NO_OUTPUT` and
//!   `exit_with_status` (diff.c:5348-5352), and the status is never read here.
//! * `--max-memory=<size>`: the cost matrix budget, checked in
//!   [`get_correspondences`] before any pairing is computed, so it fires even
//!   when one range is empty. The refusal is upstream's `die()` verbatim,
//!   `strbuf_humanise_bytes()` rendering included.
//! * `--output=<file>`: the whole page — pair headers included, since
//!   `output_pair_header()` also writes to `diffopt->file` — goes to the file
//!   and stdout stays empty. The file is opened while options are parsed, so a
//!   path that cannot be created is fatal ahead of every other check and a path
//!   that can is truncated even when the run then fails.
//! * The diff-of-diffs body format: `--diff-algorithm=<myers|minimal|patience|
//!   histogram>` with its `--minimal` / `--patience` / `--histogram` aliases,
//!   `--indent-heuristic` / `--no-indent-heuristic`, `-U<n>` / `--unified=<n>`,
//!   and the three `--output-indicator-*` markers (an empty value stores NUL,
//!   which drops the marker column entirely). None of them reaches
//!   [`diffsize`], whose `xpparam_t` upstream leaves zeroed, so the *matching*
//!   is unchanged by all of them.
//! * `--abbrev` / `--no-abbrev` / `--abbrev=<n>`: the abbreviation length of the
//!   ids in every pair header, ported from `find_unique_abbrev()` and
//!   `parse_opt_abbrev_cb()` (bare `--abbrev` is 7, `--no-abbrev` / `--abbrev=0`
//!   the full id, `--abbrev=<n>` clamps `<n>` to `[4, 40]`, and a non-numeric
//!   `<n>` is the 129 `error: option 'abbrev' expects a numerical value`).
//! * The diff options that upstream forwards but that touch only patch bytes
//!   this port already discards, or that address machinery the diff-of-diffs
//!   does not have, so accepting them changes nothing. Each was verified
//!   byte-identical to the flagless run on a both-sides-non-empty range against
//!   git 2.55.0, and each is accepted silently rather than deferred:
//!   * `--full-index` (the abbreviated/full `index` line is dropped) and
//!     `--binary` (text files gain no binary hunk; the `Binary files … differ`
//!     label is unchanged).
//!   * Every `--diff-merges` variant (`--no-diff-merges`, `--remerge-diff`,
//!     `--diff-merges=<fmt>`) *when neither range contains a merge*, which is
//!     the patch-series shape range-diff exists for. See the known deviation
//!     below for what happens when one does.
//!   * `--textconv` / `--no-textconv`: `get_textconv()` (diff.c:3762) asks
//!     `diff_filespec_load_driver()`, which returns immediately because a driver
//!     is already set (diff.c:2312-2313) — the hardcoded `section_headers`
//!     (range-diff.c:486), whose NULL `.textconv` makes
//!     `userdiff_get_textconv()` give up (userdiff.c:551-552).
//!   * `--src-prefix=` / `--dst-prefix=` / `--no-prefix` / `--default-prefix`:
//!     the prefixes only reach the `diff --git`, `---` and `+++` lines, which
//!     `suppress_diff_headers` drops (range-diff.c:523).
//!   * `--line-prefix=`: overwritten by the four-space `output_prefix_data`
//!     (range-diff.c:527-529) after the user's `diff_options` is memcpy'd in.
//!   * `--exit-code` / `--no-exit-code`: `cmd_range_diff()` returns
//!     `show_range_diff()`'s value (builtin/range-diff.c:189-196) and never
//!     calls `diff_result_code()`, so the status stays 0.
//!   * `--relative` / `--no-relative`, `--ignore-submodules[=<when>]`,
//!     `--submodule[=<fmt>]`, `--ita-invisible-in-index`,
//!     `--ita-visible-in-index` and `--max-depth=<n>`: there is no tree walk, no
//!     submodule and no index here — the two filespecs are the `is_stdin`
//!     buffers `get_filespec()` builds (range-diff.c:477-489).
//! * The failure paths, with upstream's exit status: a bad argument shape exits
//!   129, a two-range operand that names nothing exits 128 (`bad revision`, the
//!   fatal `is_range_diff_range()` raises), `--left-only` together with
//!   `--right-only` exits 255, and a range naming an unknown revision exits 255.
//!   Ahead of all of those come the four `diff_setup_done()` refusals, raised
//!   before any revision is resolved and in this order (diff.c:5259-5273,
//!   5364-5365), each exit 128:
//!   1. two or more of `--name-only` / `--name-status` / `--check` / `-s`;
//!   2. two or more of `-G` / `-S` / `--find-object`;
//!   3. `-G` together with `--pickaxe-regex`;
//!   4. `--pickaxe-all` together with `--find-object`;
//!   5. `--follow` in any form — range-diff routes a `-- <path>` to `log_arg`
//!      rather than to `diffopt.pathspec` (builtin/range-diff.c:128/148/179), so
//!      that pathspec is always empty and `diff_check_follow_pathspec()` always
//!      takes its `--follow requires exactly one pathspec` die.
//!
//! ### Option handling — nothing is silently ignored
//!
//! Upstream forwards most of the `git diff` option set to the inner patch
//! rendering. This port implements only the options listed above; every other
//! option is *deferred*, meaning it is recorded and never applied:
//!
//! * A deferred option configures the *outer* diff — the diff-of-diffs — and
//!   nothing else. `add_diff_options()` binds the whole `git diff` table to
//!   `diffopt` (builtin/range-diff.c:83), and only `--notes`, `--diff-merges`
//!   and `--remerge-diff` are `OPT_PASSTHRU_ARGV` entries that reach the inner
//!   `git log` (builtin/range-diff.c:56-66). The matching is out of reach too:
//!   `diffsize()` builds its own zeroed `xpparam_t` (range-diff.c:307).
//! * That outer diff runs in exactly one place — `patch_diff()`, which
//!   `output()` calls only for a *matched* pair and only when the format is not
//!   `DIFF_FORMAT_NO_OUTPUT` (range-diff.c:567-573). So when no pair matched, or
//!   `-s`/`--quiet` suppressed the bodies, no byte of the page can depend on a
//!   deferred option and the page is emitted (exit 0) exactly as upstream emits
//!   it — which covers the common `<old>...<new>` ancestor case, where one range
//!   is empty, and every disjoint pair of ranges besides. Otherwise, if a body
//!   would be produced, the run stops with a terse `unsupported flag` message on
//!   stderr rather than emitting a diff-of-diffs that ignored the option.
//! * A pair that matched *byte-identically* — the `=` pair, decided by
//!   `strcmp(a_util->patch, b_util->patch)` (range-diff.c:429) — is a third
//!   empty body, and the one that is easy to miss: `patch_diff()` does run, but
//!   the filepair it queues has nothing to report, so `diff_flush()` writes
//!   nothing under `-p`, `--stat`, `--numstat`, `--shortstat`,
//!   `--compact-summary`, `--dirstat`, `--summary` or `--check`, and the
//!   deferred option that asked for one of them is again unobservable. The
//!   exceptions are `--raw`, `--name-only` and `--name-status`, whose loop is
//!   gated on `check_pair_status()` alone and lists the pair whatever its
//!   content — `diff_unmodified_pair()` compares the two filespec *paths*, which
//!   `get_filespec()` always names `a` and `b` — and the stat group *together
//!   with* `-p`, where the empty stat still bumps `separator` and
//!   `DIFF_FORMAT_PATCH` turns that into one four-space line (diff.c:7197-7258).
//!   Those still stop.
//! * If the run instead ends earlier — a usage error, or a range that names an
//!   unknown revision — the deferred option never becomes observable, because
//!   upstream's behaviour on those two paths does not depend on it. That was
//!   checked against git 2.55 by running every flag this subcommand's parity
//!   grammar can emit with no range argument: all 84 produce the same
//!   `fatal: need two commit ranges` and the same exit status 129.
//! * The exception is an option upstream *validates while parsing*, before any
//!   revision is resolved: `--creation-factor` (`OPTION_INTEGER`),
//!   `--inter-hunk-context` and `--max-memory` (both k/m/g magnitudes via
//!   [`git_parse_unsigned`]), and `--find-object`, whose value
//!   `diff_opt_find_object()` resolves against the repository before it records
//!   anything (diff.c:5531). A malformed value for any of them is the 129
//!   `error:` upstream reports at parse time, not a deferred `unsupported
//!   flag`. An `--inter-hunk-context` or `--find-object` value upstream accepts
//!   is deferred like the rest, because honouring it would change the rendered
//!   patch text.
//!
//! An option this port does not recognise at all is deferred too, rather than
//! rejected: upstream accepts the whole `git diff` option list here, and
//! guessing at that list would turn an accepted option into a bogus usage
//! error. The one place the spelling still matters is arity — the long and
//! short options that take their value as a separate argv element are listed in
//! [`LONG_TAKES_VALUE`] and [`SHORT_TAKES_VALUE`] so the value is consumed and
//! not mistaken for a revision.
//!
//! ### Not covered — these stop rather than emit output that would diverge
//!
//! * Color in any form: `--color`, `--color=always`, and `--dual-color` (which
//!   upstream uses to *force* color on). The dual-color markup is not ported.
//! * The output formats that replace the patch body — `--stat` and its width
//!   options, `--compact-summary`, `--numstat`, `--shortstat`, `--dirstat`,
//!   `--summary`, `--raw`, `--name-only`, `--name-status`, `--check` — none of
//!   which this port renders.
//! * The pickaxe *filters* `-S`, `-G` and `--find-object`: `diffcore_pickaxe()`
//!   can drop the diff-of-diffs' single filepair, which empties the body. Their
//!   modifiers `--pickaxe-all` and `--pickaxe-regex` are accepted instead, since
//!   `diffcore_std()` never reaches the pickaxe unless one of those three set a
//!   kind bit (diff.c:7517). All five contribute their `pickaxe_opts` bit, for
//!   the three refusals listed above.
//! * `--inter-hunk-context=<n>`, which merges hunks closer than `<n>` context
//!   lines: gitoxide's `UnifiedDiff` exposes only a symmetrical context size, so
//!   the merging has no counterpart here.
//! * `--anchored=<text>`, which is patience diff plus anchor lines; gitoxide's
//!   `Algorithm::Patience` takes no anchors.
//! * The whitespace-comparison flags (`-w`, `-b`, `--ignore-space-at-eol`,
//!   `--ignore-cr-at-eol`, `--ignore-blank-lines`, `-I<regex>`), the rename and
//!   copy detection flags, `--word-diff`, `--color-moved`, `-R`,
//!   `--function-context`, `--diff-filter`, `--rotate-to` / `--skip-to`,
//!   `--ext-diff` and `-O`.
//! * A magic (`:(glob)`, `:!exclude`, …) or wildcard pathspec, and every other
//!   `git diff` option upstream forwards to the inner patches.
//! * Commits containing a rename that git's `diffcore-rename` would detect.
//!   These are found by re-running the tree diff with gitoxide's rename tracker
//!   at git's default 50% threshold, and refused: upstream's `old => new`
//!   section header depends on `diffcore-delta` similarity scoring and on
//!   rename-aware diff-queue ordering, neither of which is ported.
//! * `-h`: upstream's usage text concatenates the entire `git diff` option list,
//!   which is not ported.
//!
//! ### Known deviations, stated rather than hidden
//!
//! * Upstream orders each range with `--date-order`, i.e. commit-date order
//!   constrained by topology. This port implements the topological constraint
//!   exactly (Kahn's algorithm over in-range child counts, newest commit date
//!   first), and a commit-date tie falls back to the position the traversal
//!   reached the commit at — the stand-in for `prio_queue`'s insertion counter,
//!   which is what breaks the tie in `sort_in_topological_order()`. That
//!   traversal is asked for commit-date order so it mirrors the `revs->commits`
//!   list `prepare_revision_walk()` hands that function, rather than gitoxide's
//!   default breadth-first. What is *not* established is that gitoxide's own
//!   ordering of two commits sharing a second matches git's insertion order, so
//!   a merge-heavy range with tied commit dates may still order the tied
//!   commits differently. No such case has been produced: 100 randomly
//!   generated merge histories whose commit dates were drawn from three values
//!   (so ties are the norm) all matched git 2.55.0 exactly, both before and
//!   after the traversal order was changed.
//! * A usage error prints `fatal: <reason>` and the three-line synopsis on
//!   stderr and exits 129 like upstream, but without the ~90-line option list
//!   upstream prints after the synopsis. Stdout is empty either way.
//! * An unrecognised option reaches the usage error as "need two commit ranges"
//!   rather than upstream's "unknown option", because unrecognised options are
//!   deferred (see above). The exit status, 129, is the same.
//! * Any `--diff-merges` spelling — `--remerge-diff` and `--no-diff-merges`
//!   included, since both push onto `diff_merges_arg` — makes upstream set
//!   `range_diff_opts.include_merges` (builtin/range-diff.c:94-97), so a merge
//!   inside either range then gets its own pair-header line. This port always
//!   walks with merges excluded, so on such a range it prints one line fewer.
//!   Confirmed against git 2.55.0: on a range holding one merge,
//!   `range-diff -s --remerge-diff <r1> <r2>` lists the merge and the flagless
//!   run does not. These options are still accepted rather than deferred,
//!   because on a merge-free range — the patch series range-diff is for — they
//!   are byte-identical.
//! * `is_range_diff_range()` counts the *objects* `setup_revisions()` left
//!   pending and demands at least one positive and one negative. When both ends
//!   of `<a>..<b>` name the same commit, upstream sees one object carrying
//!   `UNINTERESTING`, counts zero positives, and rejects the operand — so
//!   `range-diff <tag>..<branch> …` with the tag *on* the branch tip is
//!   upstream's `need two commit ranges` (129) while this port accepts it and
//!   prints a page. Confirmed against git 2.55.0.
//! * `diff.algorithm` in config is not read: upstream's
//!   `repo_config(git_diff_ui_config)` (builtin/range-diff.c:79) makes it the
//!   default the command line then overrides, while this port always starts from
//!   Myers. Only a repository that sets that key is affected.

use anyhow::{anyhow, bail, Result};
use std::collections::{BinaryHeap, HashMap};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::BStr;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, Diff, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::prelude::ObjectIdExt;

use super::{diff_color, Arg, LongOpt};
use crate::objname;

/// `RANGE_DIFF_CREATION_FACTOR_DEFAULT`.
const CREATION_FACTOR_DEFAULT: i64 = 60;
/// `COST_MAX` from `linear-assignment.h`, the cost cap that prevents overflow.
const COST_MAX: i64 = 1 << 16;
/// `sizeof(struct func_line.buf)` — the hard cap on a hunk header's section name.
const FUNC_BUF_SIZE: usize = 80;
/// `FIRST_FEW_BYTES` — how far `buffer_is_binary()` looks for a NUL byte.
const FIRST_FEW_BYTES: usize = 8000;
/// The four-space `output_prefix` upstream installs for the diff-of-diffs.
const INDENT: &[u8] = b"    ";
/// `RANGE_DIFF_MAX_MEMORY_DEFAULT` — the cost matrix budget `--max-memory`
/// overrides, which `git range-diff -h` documents as "default 4G".
const MAX_MEMORY_DEFAULT: u64 = 4 * 1024 * 1024 * 1024;
/// `sizeof(int)`, the element size `get_correspondences()` multiplies the `n*n`
/// cost matrix by (range-diff.c:334).
const COST_ELEMENT_SIZE: u64 = 4;
/// Indices into [`Opts::indicators`], upstream's `OUTPUT_INDICATOR_*`.
const IND_NEW: usize = 0;
const IND_OLD: usize = 1;
const IND_CONTEXT: usize = 2;

/// The `diff_setup_done()` pickaxe bits (`diff.h`), each set by the option that
/// names it and never cleared. Upstream reports three separate `cannot be used
/// together` fatals over them (diff.c:5263-5273), all before any revision is
/// resolved.
const PICKAXE_ALL: u32 = 1;
const PICKAXE_REGEX: u32 = 1 << 1;
const PICKAXE_KIND_S: u32 = 1 << 2;
const PICKAXE_KIND_G: u32 = 1 << 3;
const PICKAXE_KIND_OBJFIND: u32 = 1 << 4;
/// `DIFF_PICKAXE_KINDS_MASK`: two or more of `-G`, `-S`, `--find-object`.
const PICKAXE_KINDS_MASK: u32 = PICKAXE_KIND_S | PICKAXE_KIND_G | PICKAXE_KIND_OBJFIND;
/// `DIFF_PICKAXE_KINDS_G_REGEX_MASK`: `-G` together with `--pickaxe-regex`.
const PICKAXE_G_REGEX_MASK: u32 = PICKAXE_KIND_G | PICKAXE_REGEX;
/// `DIFF_PICKAXE_KINDS_ALL_OBJFIND_MASK`: `--pickaxe-all` with `--find-object`.
const PICKAXE_ALL_OBJFIND_MASK: u32 = PICKAXE_ALL | PICKAXE_KIND_OBJFIND;

/// The `diff.h` `DIFF_FORMAT_*` bits, tracked because `diff_flush()` decides
/// from them whether the diff-of-diffs body is written at all and, for the
/// formats this port does not render, whether a deferred option is observable.
///
/// The first four are the group `diff_setup_done()` forbids combining
/// (diff.c:5259): two or more of `--name-only`, `--name-status`, `--check` and
/// `-s` is the fatal `cannot be used together` (exit 128) raised before any
/// revision is resolved. `-s`/`--no-patch` *assigns* `DIFF_FORMAT_NO_OUTPUT`,
/// clearing the earlier bits, so `--name-only -s` is one bit but `-s
/// --name-only` is two.
const FMT_NAME: u32 = 1 << 0;
const FMT_NAME_STATUS: u32 = 1 << 1;
const FMT_CHECKDIFF: u32 = 1 << 2;
const FMT_NO_OUTPUT: u32 = 1 << 3;
const FMT_RAW: u32 = 1 << 4;
const FMT_NUMSTAT: u32 = 1 << 5;
const FMT_DIFFSTAT: u32 = 1 << 6;
const FMT_SHORTSTAT: u32 = 1 << 7;
const FMT_DIRSTAT: u32 = 1 << 8;
const FMT_SUMMARY: u32 = 1 << 9;
const FMT_PATCH: u32 = 1 << 10;

/// `HAS_MULTI_BITS`'s operand in `diff_setup_done()` (diff.c:5259) — and, one
/// line later, the test that clears [`FMT_CLEARED_BY_EXCLUSIVE`].
const FMT_EXCLUSIVE: u32 = FMT_NAME | FMT_NAME_STATUS | FMT_CHECKDIFF | FMT_NO_OUTPUT;

/// The bits any of [`FMT_EXCLUSIVE`] wipes out (diff.c:5261-5262), which is why
/// `--check` prints no patch even next to an explicit `-p`.
const FMT_CLEARED_BY_EXCLUSIVE: u32 =
    FMT_RAW | FMT_NUMSTAT | FMT_DIFFSTAT | FMT_SHORTSTAT | FMT_DIRSTAT | FMT_SUMMARY | FMT_PATCH;

/// The three formats `diff_flush()` computes from one `diffstat_t` and closes
/// with a single `separator++` (diff.c:7197-7223) — the reason `-p --stat` puts
/// a blank line between the stat and the patch even when the stat is empty.
const FMT_STAT_GROUP: u32 = FMT_NUMSTAT | FMT_DIFFSTAT | FMT_SHORTSTAT;

/// The `output_format` bits `name` sets, and whether it leaves
/// `flags.dirstat_by_line` on — `None` for an option that is not an output
/// format at all.
///
/// Every entry that is not one of `--name-only`, `--name-status` or `--check`
/// also *unsets* `DIFF_FORMAT_NO_OUTPUT`, so meeting it revives the output an
/// earlier `-s` suppressed: that is the unset mask on `add_diff_options()`'s
/// `OPT_BITOP` entries (diff.c:6043-6099) and the explicit clear in the four
/// callbacks that set a bit by hand — `diff_opt_stat()` (diff.c:5445),
/// `parse_dirstat_opt()` (diff.c:5465), `enable_patch_output()` (diff.c:5502,
/// 5564, 5961) and `diff_opt_compact_summary()` (diff.c:5667). The three
/// exceptions are `OPT_BIT_F` entries with no unset mask, which is why `-s
/// --name-only` is still the `cannot be used together` fatal while `-s --patch
/// --name-only` is not.
///
/// Only positive spellings are listed. A `--no-<format>` reaches its callback's
/// `unset` branch, which leaves `output_format` alone.
fn format_bits(name: &str, inline: Option<&str>) -> Option<(u32, bool)> {
    // `parse_dirstat_opt()` walks the comma-separated parameters in order, so
    // the last of `lines`/`files` wins. The synonyms only prepend a parameter —
    // `--dirstat-by-file` a `files` and `--cumulative` a `cumulative` — and
    // neither can outrank a `lines` that follows it, so both start from off.
    let dirstat_by_line = || {
        let mut by_line = false;
        for param in inline.unwrap_or_default().split(',') {
            match param {
                "lines" => by_line = true,
                "files" => by_line = false,
                _ => {}
            }
        }
        by_line
    };
    if name.starts_with("-U") {
        return Some((FMT_PATCH, false));
    }
    let bits = match name {
        "-p" | "-u" | "--patch" | "--unified" | "--binary" => FMT_PATCH,
        "--raw" => FMT_RAW,
        "--patch-with-raw" => FMT_PATCH | FMT_RAW,
        "--patch-with-stat" => FMT_PATCH | FMT_DIFFSTAT,
        "--numstat" => FMT_NUMSTAT,
        "--shortstat" => FMT_SHORTSTAT,
        "--stat" | "--stat-width" | "--stat-name-width" | "--stat-graph-width"
        | "--stat-count" | "--compact-summary" => FMT_DIFFSTAT,
        "--summary" => FMT_SUMMARY,
        "-X" | "--dirstat" | "--cumulative" | "--dirstat-by-file" => {
            return Some((FMT_DIRSTAT, dirstat_by_line()))
        }
        "--name-only" => FMT_NAME,
        "--name-status" => FMT_NAME_STATUS,
        "--check" => FMT_CHECKDIFF,
        _ => return None,
    };
    Some((bits, false))
}

/// Whether `diff_flush()` writes a byte for range-diff's one filepair under
/// `output_format`, leaving out [`FMT_PATCH`], which this port renders itself.
///
/// The filepair is always the same shape — `get_filespec()` builds two `is_stdin`
/// buffers named `a` and `b`, both mode `0100644`, both present (range-diff.c:
/// 477-489) — which fixes three of the answers regardless of content:
///
/// * `--raw`, `--name-only` and `--name-status` always list it. Their loop is
///   gated on `check_pair_status()` alone, and `diff_unmodified_pair()` compares
///   the two filespec *paths*, so the pair counts as modified even when the two
///   texts are identical.
/// * `--summary` never writes: `is_summary_empty()` is true for a pair with no
///   creation, deletion, rename, copy or mode change, and this pair can have
///   none of them. It does not reach the `separator++` either.
/// * `--dirstat` never writes: both paths sit at the root, and `show_dirstat()`
///   reports directories. `--dirstat=lines` is the exception to the exception —
///   it rides inside the stat group's block (diff.c:7197-7223) and so bumps
///   `separator`, which `FMT_PATCH` turns into one four-space line.
///
/// The stat group writes only when the diff is non-empty, but bumps `separator`
/// either way. `--check` is the one format whose output cannot be predicted from
/// the pair's shape — it writes when an added line carries a whitespace error —
/// so it counts as writing.
fn format_writes(output_format: u32, dirstat_by_line: bool, diff_is_empty: bool) -> bool {
    if output_format & (FMT_RAW | FMT_NAME | FMT_NAME_STATUS | FMT_CHECKDIFF) != 0 {
        return true;
    }
    let bumps_separator =
        output_format & FMT_STAT_GROUP != 0 || (output_format & FMT_DIRSTAT != 0 && dirstat_by_line);
    (output_format & FMT_STAT_GROUP != 0 && !diff_is_empty)
        || (bumps_separator && output_format & FMT_PATCH != 0)
}

/// The terse rejection for a flag `git range-diff` accepts and this port has not
/// implemented.
///
/// It deliberately carries no inventory of what *is* ported: that list is an
/// implementation detail of this module, it goes stale the moment a flag lands,
/// and stock git never prints anything like it. A flag stock git itself does not
/// know takes the [`unknown_option`] path instead, which is parse-options' own
/// refusal.
fn unsupported_flag(flag: &str) -> String {
    format!("unsupported flag {flag:?}")
}

/// One commit rendered into its canonical patch text: upstream's
/// `struct patch_util` fused with the `string_list` item holding the text.
struct Patch {
    /// Position within its range, upstream's `util->i`.
    index: usize,
    /// `find_unique_abbrev()` of the commit id, for the pair header.
    abbrev: String,
    /// One-line subject (`CMIT_FMT_ONELINE`), for the pair header.
    subject: Vec<u8>,
    /// The full patch: metadata, message, and every file section.
    text: Vec<u8>,
    /// Offset of the first ` ## <path> ##` section. Left at 0 for a commit with
    /// no diff, exactly as upstream leaves `diff_offset` zeroed there, so that
    /// `diff()` then covers the whole patch.
    diff_offset: usize,
    /// Number of diff lines, upstream's `diffsize`, used for the creation cost.
    diffsize: i64,
    /// Index of the corresponding patch in the other range, or -1.
    matching: i64,
    /// Whether this left-hand patch has already been printed.
    shown: bool,
}

impl Patch {
    /// Upstream's `util->diff`: the patch text from the first file section on.
    fn diff(&self) -> &[u8] {
        &self.text[self.diff_offset..]
    }
}

/// `usage_with_options()` over `builtin_range_diff_usage` and range-diff's
/// option table — which is most of diff's, since `OPT_DIFF_OPTIONS` splices the
/// whole diff UI in. Reproduced in full: `-h` puts this on the user's stdout, so
/// the three synopsis lines the port used to print were visibly short.
const USAGE: &str = r#"usage: git range-diff [<options>] <old-base>..<old-tip> <new-base>..<new-tip>
   or: git range-diff [<options>] <old-tip>...<new-tip>
   or: git range-diff [<options>] <base> <old-tip> <new-tip>

    --[no-]creation-factor <n>
                          percentage by which creation is weighted
    --no-dual-color       use simple diff colors
    --dual-color          opposite of --no-dual-color
    --[no-]notes[=<notes>]
                          passed to 'git log'
    --[no-]diff-merges <style>
                          passed to 'git log'
    --[no-]max-memory <size>
                          maximum memory for cost matrix (default 4G)
    --[no-]remerge-diff   passed to 'git log'
    --[no-]left-only      only emit output related to the first range
    --[no-]right-only     only emit output related to the second range

Diff output format options
    -p, --patch           generate patch
    -s, --no-patch        suppress diff output
    -u                    generate patch
    -U, --unified[=<n>]   generate diffs with <n> lines context
    -W, --[no-]function-context
                          generate diffs with <n> lines context
    --raw                 generate the diff in raw format
    --patch-with-raw      synonym for '-p --raw'
    --patch-with-stat     synonym for '-p --stat'
    --numstat             machine friendly --stat
    --shortstat           output only the last line of --stat
    -X, --dirstat[=<param1>,<param2>...]
                          output the distribution of relative amount of changes for each sub-directory
    --cumulative          synonym for --dirstat=cumulative
    --dirstat-by-file[=<param1>,<param2>...]
                          synonym for --dirstat=files,<param1>,<param2>...
    --check               warn if changes introduce conflict markers or whitespace errors
    --summary             condensed summary such as creations, renames and mode changes
    --name-only           show only names of changed files
    --name-status         show only names and status of changed files
    --stat[=<width>[,<name-width>[,<count>]]]
                          generate diffstat
    --stat-width <width>  generate diffstat with a given width
    --stat-name-width <width>
                          generate diffstat with a given name width
    --stat-graph-width <width>
                          generate diffstat with a given graph width
    --stat-count <count>  generate diffstat with limited lines
    --[no-]compact-summary
                          generate compact summary in diffstat
    --binary              output a binary diff that can be applied
    --[no-]full-index     show full pre- and post-image object names on the "index" lines
    --[no-]color[=<when>] show colored diff
    --ws-error-highlight <kind>
                          highlight whitespace errors in the 'context', 'old' or 'new' lines in the diff
    -z                    do not munge pathnames and use NULs as output field terminators in --raw or --numstat
    --[no-]abbrev[=<n>]   use <n> digits to display object names
    --src-prefix <prefix> show the given source prefix instead of "a/"
    --dst-prefix <prefix> show the given destination prefix instead of "b/"
    --line-prefix <prefix>
                          prepend an additional prefix to every line of output
    --no-prefix           do not show any source or destination prefix
    --default-prefix      use default prefixes a/ and b/
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    --output-indicator-new <char>
                          specify the character to indicate a new line instead of '+'
    --output-indicator-old <char>
                          specify the character to indicate an old line instead of '-'
    --output-indicator-context <char>
                          specify the character to indicate a context instead of ' '

Diff rename options
    -B, --break-rewrites[=<n>[/<m>]]
                          break complete rewrite changes into pairs of delete and create
    -M, --find-renames[=<n>]
                          detect renames
    -D, --irreversible-delete
                          omit the preimage for deletes
    -C, --find-copies[=<n>]
                          detect copies
    --[no-]find-copies-harder
                          use unmodified files as source to find copies
    --no-renames          disable rename detection
    --[no-]rename-empty   use empty blobs as rename source
    --[no-]follow         continue listing the history of a file beyond renames
    -l <n>                prevent rename/copy detection if the number of rename/copy targets exceeds given limit

Diff algorithm options
    --minimal             produce the smallest possible diff
    -w, --ignore-all-space
                          ignore whitespace when comparing lines
    -b, --ignore-space-change
                          ignore changes in amount of whitespace
    --ignore-space-at-eol ignore changes in whitespace at EOL
    --ignore-cr-at-eol    ignore carrier-return at the end of line
    --ignore-blank-lines  ignore changes whose lines are all blank
    -I, --[no-]ignore-matching-lines <regex>
                          ignore changes whose all lines match <regex>
    --[no-]indent-heuristic
                          heuristic to shift diff hunk boundaries for easy reading
    --patience            generate diff using the "patience diff" algorithm
    --histogram           generate diff using the "histogram diff" algorithm
    --diff-algorithm <algorithm>
                          choose a diff algorithm
    --anchored <text>     generate diff using the "anchored diff" algorithm
    --word-diff[=<mode>]  show word diff, using <mode> to delimit changed words
    --word-diff-regex <regex>
                          use <regex> to decide what a word is
    --color-words[=<regex>]
                          equivalent to --word-diff=color --word-diff-regex=<regex>
    --[no-]color-moved[=<mode>]
                          moved lines of code are colored differently
    --[no-]color-moved-ws <mode>
                          how white spaces are ignored in --color-moved

Other diff options
    --[no-]relative[=<prefix>]
                          when run from subdir, exclude changes outside and show relative paths
    -a, --[no-]text       treat all files as text
    -R                    swap two inputs, reverse the diff
    --[no-]exit-code      exit with 1 if there were differences, 0 otherwise
    --[no-]quiet          disable all output of the program
    --[no-]ext-diff       allow an external diff helper to be executed
    --[no-]textconv       run external text conversion filters when comparing binary files
    --ignore-submodules[=<when>]
                          ignore changes to submodules in the diff generation
    --submodule[=<format>]
                          specify how differences in submodules are shown
    --ita-invisible-in-index
                          hide 'git add -N' entries from the index
    --ita-visible-in-index
                          treat 'git add -N' entries as real in the index
    -S <string>           look for differences that change the number of occurrences of the specified string
    -G <regex>            look for differences that change the number of occurrences of the specified regex
    --pickaxe-all         show all changes in the changeset with -S or -G
    --pickaxe-regex       treat <string> in -S as extended POSIX regular expression
    -O <file>             control the order in which files appear in the output
    --rotate-to <path>    show the change in the specified path first
    --skip-to <path>      skip the output to the specified path
    --find-object <object-id>
                          look for differences that change the number of occurrences of the specified object
    --diff-filter [(A|C|D|M|R|T|U|X|B)...[*]]
                          select files by diff type
    --max-depth <depth>   maximum tree depth to recurse
    --output <file>       output to a specific file

"#;

/// Long options of `git range-diff -h` whose value is a separate argv element
/// when the option is spelled without `=`. Consuming it keeps a value like the
/// `myers` of `--diff-algorithm myers` from being classified as a revision.
const LONG_TAKES_VALUE: &[&str] = &[
    "--anchored",
    "--color-moved-ws",
    "--creation-factor",
    "--diff-algorithm",
    "--diff-merges",
    "--dst-prefix",
    "--find-object",
    "--ignore-matching-lines",
    "--inter-hunk-context",
    "--line-prefix",
    "--max-depth",
    "--max-memory",
    "--output",
    "--output-indicator-context",
    "--output-indicator-new",
    "--output-indicator-old",
    "--rotate-to",
    "--skip-to",
    "--src-prefix",
    "--stat-count",
    "--stat-graph-width",
    "--stat-name-width",
    "--stat-width",
    "--word-diff-regex",
    "--ws-error-highlight",
];

/// Every long option `git range-diff` accepts, in `struct option` order: its own
/// `range_diff_options[]` (builtin/range-diff.c:50) followed by everything
/// `add_diff_options()` appends (diff.c:6041), since that call is
/// `parse_options_concat(range_diff_options, parseopts)` — verb table first.
///
/// `parse_options()` resolves an argument against this table before any revision is
/// looked at, so a name no entry claims is `error: unknown option` and 129 no matter
/// what the rest of the command line says — which is why an unrecognised option
/// cannot be deferred like an unimplemented one. Order is load-bearing twice over:
/// it decides which two spellings an `ambiguous option:` sentence names, and
/// `--no-patch` / `--no-prefix` / `--no-renames` are entries spelled with their own
/// `no-`, which parse-options reads as the *unset* sense of the stem.
pub(super) const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "creation-factor",             neg: true,  arg: Arg::Required },
    LongOpt { name: "no-dual-color",               neg: true,  arg: Arg::None },
    LongOpt { name: "notes",                       neg: true,  arg: Arg::Optional },
    LongOpt { name: "diff-merges",                 neg: true,  arg: Arg::Required },
    LongOpt { name: "max-memory",                  neg: true,  arg: Arg::Required },
    LongOpt { name: "remerge-diff",                neg: true,  arg: Arg::None },
    LongOpt { name: "left-only",                   neg: true,  arg: Arg::None },
    LongOpt { name: "right-only",                  neg: true,  arg: Arg::None },
    LongOpt { name: "patch",                       neg: false, arg: Arg::None },
    LongOpt { name: "no-patch",                    neg: true,  arg: Arg::None },
    LongOpt { name: "unified",                     neg: false, arg: Arg::Optional },
    LongOpt { name: "function-context",            neg: true,  arg: Arg::None },
    LongOpt { name: "raw",                         neg: false, arg: Arg::None },
    LongOpt { name: "patch-with-raw",              neg: false, arg: Arg::None },
    LongOpt { name: "patch-with-stat",             neg: false, arg: Arg::None },
    LongOpt { name: "numstat",                     neg: false, arg: Arg::None },
    LongOpt { name: "shortstat",                   neg: false, arg: Arg::None },
    LongOpt { name: "dirstat",                     neg: false, arg: Arg::Optional },
    LongOpt { name: "cumulative",                  neg: false, arg: Arg::None },
    LongOpt { name: "dirstat-by-file",             neg: false, arg: Arg::Optional },
    LongOpt { name: "check",                       neg: false, arg: Arg::None },
    LongOpt { name: "summary",                     neg: false, arg: Arg::None },
    LongOpt { name: "name-only",                   neg: false, arg: Arg::None },
    LongOpt { name: "name-status",                 neg: false, arg: Arg::None },
    LongOpt { name: "stat",                        neg: false, arg: Arg::Optional },
    LongOpt { name: "stat-width",                  neg: false, arg: Arg::Required },
    LongOpt { name: "stat-name-width",             neg: false, arg: Arg::Required },
    LongOpt { name: "stat-graph-width",            neg: false, arg: Arg::Required },
    LongOpt { name: "stat-count",                  neg: false, arg: Arg::Required },
    LongOpt { name: "compact-summary",             neg: true,  arg: Arg::None },
    LongOpt { name: "binary",                      neg: false, arg: Arg::None },
    LongOpt { name: "full-index",                  neg: true,  arg: Arg::None },
    LongOpt { name: "color",                       neg: true,  arg: Arg::Optional },
    LongOpt { name: "ws-error-highlight",          neg: false, arg: Arg::Required },
    LongOpt { name: "abbrev",                      neg: true,  arg: Arg::Optional },
    LongOpt { name: "src-prefix",                  neg: false, arg: Arg::Required },
    LongOpt { name: "dst-prefix",                  neg: false, arg: Arg::Required },
    LongOpt { name: "line-prefix",                 neg: false, arg: Arg::Required },
    LongOpt { name: "no-prefix",                   neg: false, arg: Arg::None },
    LongOpt { name: "default-prefix",              neg: false, arg: Arg::None },
    LongOpt { name: "inter-hunk-context",          neg: false, arg: Arg::Required },
    LongOpt { name: "output-indicator-new",        neg: false, arg: Arg::Required },
    LongOpt { name: "output-indicator-old",        neg: false, arg: Arg::Required },
    LongOpt { name: "output-indicator-context",    neg: false, arg: Arg::Required },
    LongOpt { name: "break-rewrites",              neg: false, arg: Arg::Optional },
    LongOpt { name: "find-renames",                neg: false, arg: Arg::Optional },
    LongOpt { name: "irreversible-delete",         neg: false, arg: Arg::None },
    LongOpt { name: "find-copies",                 neg: false, arg: Arg::Optional },
    LongOpt { name: "find-copies-harder",          neg: true,  arg: Arg::None },
    LongOpt { name: "no-renames",                  neg: false, arg: Arg::None },
    LongOpt { name: "rename-empty",                neg: true,  arg: Arg::None },
    LongOpt { name: "follow",                      neg: true,  arg: Arg::None },
    LongOpt { name: "minimal",                     neg: false, arg: Arg::None },
    LongOpt { name: "ignore-all-space",            neg: false, arg: Arg::None },
    LongOpt { name: "ignore-space-change",         neg: false, arg: Arg::None },
    LongOpt { name: "ignore-space-at-eol",         neg: false, arg: Arg::None },
    LongOpt { name: "ignore-cr-at-eol",            neg: false, arg: Arg::None },
    LongOpt { name: "ignore-blank-lines",          neg: false, arg: Arg::None },
    LongOpt { name: "ignore-matching-lines",       neg: true,  arg: Arg::Required },
    LongOpt { name: "indent-heuristic",            neg: true,  arg: Arg::None },
    LongOpt { name: "patience",                    neg: false, arg: Arg::None },
    LongOpt { name: "histogram",                   neg: false, arg: Arg::None },
    LongOpt { name: "diff-algorithm",              neg: false, arg: Arg::Required },
    LongOpt { name: "anchored",                    neg: false, arg: Arg::Required },
    LongOpt { name: "word-diff",                   neg: false, arg: Arg::Optional },
    LongOpt { name: "word-diff-regex",             neg: false, arg: Arg::Required },
    LongOpt { name: "color-words",                 neg: false, arg: Arg::Optional },
    LongOpt { name: "color-moved",                 neg: true,  arg: Arg::Optional },
    LongOpt { name: "color-moved-ws",              neg: true,  arg: Arg::Required },
    LongOpt { name: "relative",                    neg: true,  arg: Arg::Optional },
    LongOpt { name: "text",                        neg: true,  arg: Arg::None },
    LongOpt { name: "exit-code",                   neg: true,  arg: Arg::None },
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "ext-diff",                    neg: true,  arg: Arg::None },
    LongOpt { name: "textconv",                    neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-submodules",           neg: false, arg: Arg::Optional },
    LongOpt { name: "submodule",                   neg: false, arg: Arg::Optional },
    LongOpt { name: "ita-invisible-in-index",      neg: false, arg: Arg::None },
    LongOpt { name: "ita-visible-in-index",        neg: false, arg: Arg::None },
    LongOpt { name: "pickaxe-all",                 neg: false, arg: Arg::None },
    LongOpt { name: "pickaxe-regex",               neg: false, arg: Arg::None },
    LongOpt { name: "rotate-to",                   neg: false, arg: Arg::Required },
    LongOpt { name: "skip-to",                     neg: false, arg: Arg::Required },
    LongOpt { name: "find-object",                 neg: false, arg: Arg::Required },
    LongOpt { name: "diff-filter",                 neg: false, arg: Arg::Required },
    LongOpt { name: "max-depth",                   neg: false, arg: Arg::Required },
    LongOpt { name: "output",                      neg: false, arg: Arg::Required },
];

/// Every short option the same table accepts, `-h` aside (answered before this).
const KNOWN_SHORT: &[u8] = b"aBbCDGIlMOpRSsUuWwXz";

/// Short options whose value is a separate argv element. The remaining short
/// options either take no value (`-p`, `-R`, `-w`, …) or attach it (`-U1`,
/// `-M50`, …), so neither consumes the next element.
const SHORT_TAKES_VALUE: &[&str] = &["-G", "-I", "-O", "-S", "-l"];

/// Whether `parse_options()` would resolve `name` against the range-diff option table.
///
/// `name` has already had any `=<value>` split off. A short option carries its value
/// attached, so only its first letter is looked up.
fn is_known_option(name: &str) -> bool {
    match name.strip_prefix("--") {
        // The caller has already run the name through [`super::canonical_long`], so
        // an abbreviation arrives spelled out; the lookup still goes through the
        // resolver rather than a second list, to keep one table as the only
        // statement of what the command accepts.
        Some(body) => matches!(
            super::resolve_long(LONG_OPTS, body),
            super::Resolved::One(..)
        ),
        // `-` alone never reaches here; the parse loop treats it as an operand.
        None => name
            .as_bytes()
            .get(1)
            .is_some_and(|c| KNOWN_SHORT.contains(c)),
    }
}

/// git's unknown-option convention: the complaint, then the usage block, exit 129.
///
/// `arg` is the whole argument as typed. A long option is quoted in full, `=<value>`
/// and all (`--bogus=1` reports ``unknown option `bogus=1'``); a short one is reported
/// by its letter alone, since the rest of the argument is that option's value.
fn unknown_option(arg: &str) -> ExitCode {
    match arg.strip_prefix("--") {
        Some(rest) => eprintln!("error: unknown option `{rest}'"),
        None => eprintln!("error: unknown switch `{}'", &arg[1..2]),
    }
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// How the abbreviated commit id in every pair header is computed.
enum Abbrev {
    /// No `--abbrev`/`--no-abbrev` was given: use gitoxide's `core.abbrev`
    /// default, which is `find_unique_abbrev()` with `DEFAULT_ABBREV` (7).
    Default,
    /// `find_unique_abbrev()` with this minimum hex length. `Len(40)` is the
    /// full id (`--no-abbrev` / `--abbrev=0`), since a 40-hex prefix is always
    /// unambiguous.
    Len(usize),
}

/// Parsed command line.
struct Opts {
    creation_factor: i64,
    left_only: bool,
    right_only: bool,
    /// The notes trees upstream's inner `git log` would render, built by
    /// `--notes[=<ref>]` / `--no-notes` exactly as `notes_callback()` builds
    /// `struct display_notes_opt`. Range-diff renders notes by default, because
    /// its `git log` uses a built-in pretty format and so takes
    /// `cmd_log_init_finish()`'s default-notes branch.
    notes: super::notes::DisplayOpt,
    /// `-s` / `--no-patch`: emit only the pair headers, no diff-of-diffs body,
    /// exactly as `DIFF_FORMAT_NO_OUTPUT` suppresses the inner patch.
    no_patch: bool,
    /// `--max-memory=<size>`: the cost matrix's byte budget, checked in
    /// [`get_correspondences`] (range-diff.c:335-344). The default is the 4 GiB
    /// `RANGE_DIFF_MAX_MEMORY_DEFAULT` the `-h` text spells "default 4G".
    max_memory: u64,
    /// The xdiff algorithm of the *outer* diff-of-diffs: `--diff-algorithm=`,
    /// `--minimal`, `--patience`, `--histogram` (diff.c:3825-3838, where each
    /// spelling clears the previous one, so the last flag wins). It does not
    /// reach [`diffsize`], whose `xpparam_t` is zeroed (range-diff.c:307).
    algorithm: Algorithm,
    /// `XDF_INDENT_HEURISTIC`, on by default and cleared by
    /// `--no-indent-heuristic` (diff.c:6214-6216).
    indent_heuristic: bool,
    /// `diffopt.context`: `-U<n>` / `--unified=<n>` (diff.c:5945-5960),
    /// three lines by default.
    context: u32,
    /// `output_indicators[NEW/OLD/CONTEXT]` (diff.c:5143-5145), the three
    /// markers `--output-indicator-*` rewrites. A `0` byte is the empty value
    /// `diff_opt_char()` stores, which `emit_line_0()` writes as nothing at all
    /// (`if (first) fputc(first, file)`, diff.c:786-787).
    indicators: [u8; 3],
    /// `--output=<file>`: the sink `diffopt.file` names, opened while
    /// parse-options runs (diff.c:5821-5835) so the pair headers
    /// `output_pair_header()` writes with `fwrite(…, diffopt->file)`
    /// (range-diff.c:467) land there too.
    output: Option<std::fs::File>,
    /// Abbreviation length for the ids printed in every pair header, driven by
    /// `--abbrev` / `--no-abbrev` / `--abbrev=<n>`.
    abbrev: Abbrev,
    /// The first option this port recognises as real but does not implement,
    /// held until the run is about to produce output. See the module docs.
    deferred: Option<String>,
    /// `diff_get_color_opt()`'s palette for the pair header, empty strings when
    /// `diffopt.use_color` is off — which is the default, since output is not a
    /// terminal. `--dual-color` and `--color=always` turn it on.
    colors: diff_color::DiffColors,
}

impl Opts {
    /// Record an unimplemented option. Upstream reports the *first* offending
    /// option, so a later one never overwrites an earlier one.
    fn defer(&mut self, reason: String) {
        if self.deferred.is_none() {
            self.deferred = Some(reason);
        }
    }
}

pub fn range_diff(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        creation_factor: CREATION_FACTOR_DEFAULT,
        left_only: false,
        right_only: false,
        // Left at `given == false` here: the inner `git log` only falls back to
        // the default notes tree when no `--notes`/`--no-notes` reached it, so
        // the fallback is applied after the whole command line has been read.
        notes: super::notes::DisplayOpt::default(),
        no_patch: false,
        max_memory: MAX_MEMORY_DEFAULT,
        algorithm: Algorithm::Myers,
        indent_heuristic: true,
        context: 3,
        indicators: [b'+', b'-', b' '],
        output: None,
        abbrev: Abbrev::Default,
        deferred: None,
        colors: diff_color::DiffColors::disabled(),
    };
    // `simple_color` is upstream's tri-state: -1 until `--dual-color` sets it to 0 or
    // `--no-dual-color` sets it to 1 (builtin/range-diff.c:49). Only the 0 case forces
    // color, and it does so after `diff_setup_done()` — which is why `--dual-color
    // --no-color` still comes out colored.
    let mut simple_color: i8 = -1;
    let mut color_when: Option<diff_color::ColorWhen> = None;
    // `args` excludes the `range-diff` verb: `dispatch::run` takes the
    // subcommand separately, so option parsing starts at index 0. Positionals
    // are collected in order into `pos`, and the `--` end-of-options marker is
    // *kept* in that list the way upstream's `PARSE_OPT_KEEP_DASHDASH` keeps it,
    // because the classifier below reads its position (see [`classify`]).
    let mut pos: Vec<String> = Vec::new();
    let mut after_dash_dash = false;

    // `diffopt.output_format` (see [`FMT_NAME`]) and `diffopt.flags.
    // dirstat_by_line`, accumulated by [`format_bits`] as each argument is read.
    let mut output_format: u32 = 0;
    let mut dirstat_by_line = false;
    // `diffopt.flags.quick`, which `--quiet` sets and `diff_setup_done()` turns
    // into `DIFF_FORMAT_NO_OUTPUT` once every argument has been parsed.
    let mut quick = false;
    // The accumulated `diffopt.pickaxe_opts` bits (see [`PICKAXE_ALL`]) and
    // `diffopt.flags.follow_renames`, the other two things `diff_setup_done()`
    // refuses before any revision is resolved.
    let mut pickaxe_mask: u32 = 0;
    let mut follow = false;
    // `--find-object` resolves its value against the repository while
    // parse-options runs (diff.c:5531), so discovery has to happen here rather
    // than after the loop; the handle is reused below.
    let mut repo: Option<gix::Repository> = None;

    // The first `--diff-merges` style the inner `git log` would reject, held
    // back until that log runs so the ordering matches upstream.
    let mut bad_diff_merges: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if after_dash_dash {
            pos.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            pos.push("--".to_string());
            after_dash_dash = true;
            i += 1;
            continue;
        }
        // A bare `-` is a revision-ish operand, not an option.
        if a.len() < 2 || !a.starts_with('-') {
            pos.push(a.to_string());
            i += 1;
            continue;
        }
        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1124) is a bare `strcmp` ahead of `parse_long_opt()`,
        // so the name neither abbreviates nor takes a value: matching it after
        // the `=` split below accepted `--help-all=x` as a help request where
        // stock git answers ``error: unknown option `help-all=x'``. This table
        // has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` is the `-h` block.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }

        // Respell a unique abbreviation as the name it resolves to, so `--creation-fac`
        // reaches the same arm as `--creation-factor` — including the arm that defers
        // an option this port has not implemented. Short options pass through untouched.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };

        // `--name=value` splits; a short option's value is always attached, so
        // an `=` inside one belongs to the value.
        let (name, inline) = match a.find('=') {
            Some(p) if a.starts_with("--") => (&a[..p], Some(&a[p + 1..])),
            _ => (a, None),
        };

        // The shared `add_diff_options()` value checks (`--submodule`,
        // `--word-diff`, `--unified`/`-U`, `--diff-algorithm`, the `--stat-*`
        // widths). They run inside `parse_options()`, so a bad value is reported
        // here — before any operand is classified or resolved — even though this
        // port defers the rendering those options ask for.
        let (shared_name, shared_value) = match a.strip_prefix("-U") {
            // `-U` carries its value attached; a bare `-U` is the same as a bare
            // `--unified` and passes no value to the callback.
            Some("") => ("unified", None),
            Some(v) => ("unified", Some(v)),
            None => {
                let bare = name.trim_start_matches('-');
                // Options whose value may also be the next argv element see it
                // either way. The ones that take an *optional* value
                // (`--submodule`, `--word-diff`) are absent from that list, so
                // they correctly never look past their own token.
                let value = match inline {
                    Some(v) => Some(v),
                    None if LONG_TAKES_VALUE.contains(&name) => {
                        args.get(i + 1).map(String::as_str)
                    }
                    None => None,
                };
                (bare, value)
            }
        };
        if let Err(msg) = crate::diffopt::check(shared_name, shared_value) {
            return Ok(option_error(&msg));
        }

        match name {
            "--left-only" => opts.left_only = true,
            "--no-left-only" => opts.left_only = false,
            "--right-only" => opts.right_only = true,
            "--no-right-only" => opts.right_only = false,
            // `OPT_BOOL(0, "no-dual-color", &simple_color, …)`: the spelled option sets
            // it, and the negation `parse_options()` derives clears it.
            "--no-dual-color" => simple_color = 1,
            "--dual-color" => simple_color = 0,
            "--no-color" => color_when = Some(diff_color::ColorWhen::Never),
            "--color" => {
                // A bare `--color` is `always`; a value is parsed as `git_config_colorbool`
                // reads it, and an unrecognised one is a usage error at 129.
                let when = match inline {
                    None => diff_color::ColorWhen::Always,
                    Some(v) => match diff_color::parse_color_when(v) {
                        Some(w) => w,
                        None => {
                            return Ok(option_error(&format!(
                                "option `color' expects \"always\", \"auto\", or \"never\""
                            )))
                        }
                    },
                };
                color_when = Some(when);
            }
            // Patch output is what this port emits; `-p`/`-u` ask for it.
            "-p" | "-u" | "--patch" => {}
            // `--notes` is an `OPT_PASSTHRU_ARGV` with `PARSE_OPT_OPTARG`
            // (builtin/range-diff.c:56-58): the spelling reaches the inner `git
            // log` verbatim, so these are `notes_callback()`'s three cases and
            // the value is an attached OPTARG — a separate argv element is never
            // consumed. A bare `--notes` re-enables the default tree (plus
            // `notes.displayRef`), `--notes=<ref>` adds that ref *instead* of
            // the default, so both together print both blocks, and `--no-notes`
            // forgets every ref asked for.
            "--no-notes" => {
                opts.notes.disable();
                opts.notes.given = true;
            }
            "--notes" => {
                match inline {
                    Some(v) => opts.notes.enable_ref(v),
                    None => opts.notes.enable_default(),
                }
                opts.notes.given = true;
            }
            // `--abbrev`/`--no-abbrev`/`--abbrev=<n>` rewrite the abbreviated id
            // printed in every pair header. Upstream's `parse_opt_abbrev_cb`:
            // a bare `--abbrev` is `DEFAULT_ABBREV` (7), `--no-abbrev` is 0 (the
            // full id), and `--abbrev=<n>` parses `<n>` as a C `int` — a value
            // that is not a whole number (empty, trailing junk, non-digits) is
            // the 129 `error:` reported at parse time.
            "--no-abbrev" => opts.abbrev = Abbrev::Len(40),
            "--abbrev" => {
                opts.abbrev = match inline {
                    None => Abbrev::Len(7),
                    Some(v) => match crate::abbrev::parse_opt_abbrev_value(v) {
                        Some(0) => Abbrev::Len(40),
                        Some(n) => Abbrev::Len(n.clamp(4, 40) as usize),
                        None => {
                            return Ok(option_error(
                                "option `abbrev' expects a numerical value",
                            ))
                        }
                    },
                };
            }
            // Forwarded to the inner `git log`, but touch only patch bytes this
            // port already discards: `--full-index` only lengthens the `index`
            // line (dropped with the rest of the diff header), `--binary` adds a
            // binary hunk to text files that have none and leaves the `Binary
            // files … differ` label untouched, and every `--diff-merges` variant
            // acts on merges that range-diff excludes (`--no-merges`). So they
            // are genuine no-ops here, not deferrals — accept them silently.
            //
            // `--binary` carries `PARSE_OPT_NONEG` (diff.c:6089), so `--no-binary` is
            // not a spelling parse-options resolves at all; it falls through to the
            // `unknown option` refusal below, exactly as stock rejects it.
            "--full-index" | "--binary" | "--no-diff-merges" | "--remerge-diff" => {}
            // The rest of the provable no-ops, each one a diff option
            // `add_diff_options()` wires to the *outer* diff-of-diffs
            // (builtin/range-diff.c:83) whose effect that diff cannot have:
            //
            // * `--textconv`/`--no-textconv`: `get_textconv()` asks
            //   `diff_filespec_load_driver()` for a driver, which returns at
            //   once because one is already set (diff.c:2312-2313) — the
            //   hardcoded `section_headers` (range-diff.c:486), whose
            //   `.textconv` is NULL, so `userdiff_get_textconv()` gives up
            //   (userdiff.c:551-552).
            // * `--src-prefix`/`--dst-prefix`/`--no-prefix`/`--default-prefix`:
            //   the prefixes only reach the `diff --git`, `---` and `+++` lines,
            //   which `suppress_diff_headers` drops (range-diff.c:523).
            // * `--line-prefix`: overwritten by the four-space
            //   `output_prefix_data` (range-diff.c:527-529) after the user's
            //   `diff_options` is memcpy'd in.
            // * `--exit-code`/`--no-exit-code`: `cmd_range_diff()` returns
            //   `show_range_diff()`'s value (builtin/range-diff.c:189-196) and
            //   never calls `diff_result_code()`, so the status stays 0.
            // * `--relative`, `--ignore-submodules`, `--submodule`,
            //   `--ita-*-in-index`, `--max-depth`: there is no tree walk, no
            //   submodule and no index here — the two filespecs are `is_stdin`
            //   buffers built by `get_filespec()` (range-diff.c:477-489).
            //
            // Verified byte-identical to the flagless run on a both-sides
            // non-empty range against git 2.55.0.
            "--textconv" | "--no-textconv" | "--no-prefix" | "--default-prefix"
            | "--exit-code" | "--no-exit-code" | "--relative" | "--no-relative"
            | "--ignore-submodules" | "--submodule" | "--ita-invisible-in-index"
            | "--ita-visible-in-index" => {}
            // The same, for the ones whose value can be a separate argv element
            // (`LONG_TAKES_VALUE`), which has to be consumed so it is not
            // classified as a revision.
            "--src-prefix" | "--dst-prefix" | "--line-prefix" | "--max-depth" => {
                if inline.is_none() {
                    i += 1;
                }
            }
            // `--quiet` is `flags.quick`, which `diff_setup_done()` turns into
            // `DIFF_FORMAT_NO_OUTPUT` plus `exit_with_status` (diff.c:5348-5352)
            // — and the status is never read here, so the page it leaves is the
            // one `-s` leaves. It is *not* the same option, though: it is applied
            // after every argument has been parsed, so it never joins the
            // `cannot be used together` fatal (diff.c:5259, earlier), and no
            // later format option can revive the output it suppressed — where a
            // later format option does exactly that to `-s` (see
            // [`clears_no_output`]). Kept apart from `no_patch` for that reason
            // and folded in below.
            "--quiet" => quick = true,
            "--no-quiet" => quick = false,
            // `--output=<file>` is opened by `xfopen()` while parse-options runs
            // (diff.c:5829-5830), so a path that cannot be created is fatal
            // before every other check, and a path that can is truncated even
            // when the run then fails. It also forces colour off unless
            // `--color=always` already turned it on (diff.c:5832-5833).
            "--output" => {
                let path = match required_value(args, &mut i, name, inline) {
                    Ok(v) => v,
                    Err(code) => return Ok(code),
                };
                match std::fs::File::create(&path) {
                    Ok(f) => opts.output = Some(f),
                    Err(e) => {
                        crate::git_fatal!(
                            "could not open '{path}' for writing: {}",
                            io_reason(&e)
                        );
                    }
                }
                if color_when != Some(diff_color::ColorWhen::Always) {
                    color_when = Some(diff_color::ColorWhen::Never);
                }
            }
            // `parse_max_memory()` (builtin/range-diff.c:20-33): a
            // `git_parse_unsigned` magnitude bounded by `SIZE_MAX`, whose single
            // failure message is reported at parse time. `--no-max-memory`
            // returns early *without* touching the value, so it does not restore
            // the default.
            "--no-max-memory" => {}
            "--max-memory" => {
                let value = match required_value(args, &mut i, name, inline) {
                    Ok(v) => v,
                    Err(code) => return Ok(code),
                };
                match git_parse_unsigned(&value, u64::MAX) {
                    Ok(n) => opts.max_memory = n,
                    Err(_) => {
                        return Ok(option_error(&format!("invalid max-memory value: {value}")))
                    }
                }
            }
            // `--follow` sets `flags.follow_renames`, and `diff_setup_done()`
            // then hands `diffopt.pathspec` to `diff_check_follow_pathspec()`
            // (diff.c:5364-5365). Range-diff never fills that pathspec in — a
            // trailing `-- <path>` goes to `log_arg` instead
            // (builtin/range-diff.c:128/148/179) — so `ps->nr` is always 0 and
            // the check always dies (diff.c:5223-5226).
            "--follow" => follow = true,
            "--no-follow" => follow = false,
            // The pickaxe options. Their `pickaxe_opts` bits are tracked for the
            // three fatals `diff_setup_done()` raises before any revision is
            // resolved (diff.c:5263-5273).
            //
            // `--pickaxe-all` and `--pickaxe-regex` are modifiers, not filters:
            // `diffcore_std()` only reaches `diffcore_pickaxe()` when a *kind*
            // bit is set (`options->pickaxe_opts & DIFF_PICKAXE_KINDS_MASK`,
            // diff.c:7517), and every option that sets one is deferred below —
            // so on their own they cannot change a byte, and are accepted.
            "--pickaxe-all" => pickaxe_mask |= PICKAXE_ALL,
            "--pickaxe-regex" => pickaxe_mask |= PICKAXE_REGEX,
            // `-S`, `-G` and `--find-object` do filter, and a filtered-out
            // filepair means no diff-of-diffs body at all, so they are deferred.
            // Both carry their value either attached (`-Sfoo`) or as the next
            // argv element (`-S foo`), and the bit is set for each spelling.
            _ if name.starts_with("-S") || name.starts_with("-G") => {
                pickaxe_mask |= match name.as_bytes()[1] {
                    b'S' => PICKAXE_KIND_S,
                    _ => PICKAXE_KIND_G,
                };
                opts.defer(unsupported_flag(a));
                if name.len() == 2 {
                    i += 1;
                }
            }
            // `diff_opt_find_object()` resolves its value before it sets the
            // bit, and an unresolvable one is the 129 `error:` it reports
            // instead (diff.c:5531-5537).
            "--find-object" => {
                let value = match required_value(args, &mut i, name, inline) {
                    Ok(v) => v,
                    Err(code) => return Ok(code),
                };
                // `diff_opt_find_object()`'s own `--find-object requires a git
                // repository` (diff.c:5529) is unreachable from here: `git.c`
                // marks `range-diff` `RUN_SETUP`, so a run outside a repository
                // has already died with `not a git repository` — which is what
                // this discovery failure reports too.
                if repo.is_none() {
                    repo = Some(gix::discover(".")?);
                }
                // `repo_get_oid()`, which decodes a full-length hex without
                // consulting the object database (see [`crate::objname`]) — so
                // `--find-object <absent-full-hex>` is a perfectly good filter
                // that simply matches nothing, and the run continues.
                let found =
                    objname::resolve(repo.as_ref().expect("discovered just above"), &value).is_some();
                if !found {
                    return Ok(option_error(&format!("unable to resolve '{value}'")));
                }
                pickaxe_mask |= PICKAXE_KIND_OBJFIND;
                opts.defer(unsupported_flag(a));
            }
            // The xdiff algorithm of the diff-of-diffs. `set_diff_algorithm()`
            // clears the previous choice before setting the new one
            // (diff.c:3833-3835), so the last spelling on the line wins, and
            // `--minimal`/`--patience`/`--histogram` are the same callback with
            // the option's own name as the value (diff.c:5689-5704).
            // `parse_algorithm_value()` is case-insensitive (diff.c:220-236) and
            // `crate::diffopt::check` has already rejected an unknown name.
            "--minimal" => opts.algorithm = Algorithm::MyersMinimal,
            "--patience" => opts.algorithm = Algorithm::Patience,
            "--histogram" => opts.algorithm = Algorithm::Histogram,
            "--diff-algorithm" => {
                let value = match required_value(args, &mut i, name, inline) {
                    Ok(v) => v,
                    Err(code) => return Ok(code),
                };
                opts.algorithm = match value.to_ascii_lowercase().as_str() {
                    "minimal" => Algorithm::MyersMinimal,
                    "patience" => Algorithm::Patience,
                    "histogram" => Algorithm::Histogram,
                    // `myers` and `default`, the only names left once
                    // `crate::diffopt::check` has run.
                    _ => Algorithm::Myers,
                };
            }
            // `XDF_INDENT_HEURISTIC` is an `OPT_BIT` (diff.c:6214), on by
            // default, so only `--no-indent-heuristic` changes anything.
            "--indent-heuristic" => opts.indent_heuristic = true,
            "--no-indent-heuristic" => opts.indent_heuristic = false,
            // `-U<n>` / `--unified[=<n>]`, the context size of the
            // diff-of-diffs. `diff_opt_unified()` only assigns when a value came
            // with the option (`if (arg)`, diff.c:5953), and the option is
            // `PARSE_OPT_OPTARG`, so a bare `-U` / `--unified` leaves the
            // default 3 alone and never eats the next argv element.
            // `crate::diffopt::check` has already refused a non-numeric or
            // negative value, so `shared_value` parses here.
            _ if name == "--unified" || name.starts_with("-U") => {
                if let Some(v) = shared_value {
                    opts.context = crate::diffopt::strtol_long(v).unwrap_or(3).max(0) as u32;
                }
            }
            // The three `--output-indicator-*` markers. `diff_opt_char()` stores
            // `arg[0]`, so an empty value stores NUL and the marker column
            // disappears; a value longer than one byte was already rejected by
            // `crate::diffopt::check`.
            "--output-indicator-new" | "--output-indicator-old"
            | "--output-indicator-context" => {
                let value = match required_value(args, &mut i, name, inline) {
                    Ok(v) => v,
                    Err(code) => return Ok(code),
                };
                let slot = match name {
                    "--output-indicator-new" => IND_NEW,
                    "--output-indicator-old" => IND_OLD,
                    _ => IND_CONTEXT,
                };
                opts.indicators[slot] = value.as_bytes().first().copied().unwrap_or(0);
            }
            // `--diff-merges` is an `OPT_PASSTHRU_ARGV`, so its value is not
            // checked here — it is handed to the inner `git log`, which dies on
            // a style it does not know. Record the first bad one and report it
            // where that log would have run (see below); reporting it now would
            // put it ahead of the range resolution git does first.
            "--diff-merges" => {
                let value = match inline {
                    Some(v) => Some(v.to_string()),
                    None => {
                        i += 1;
                        args.get(i).cloned()
                    }
                };
                if let Some(v) = value {
                    if !crate::diffopt::diff_merges_is_valid(&v) && bad_diff_merges.is_none() {
                        bad_diff_merges = Some(v);
                    }
                }
            }
            // The four mutually-exclusive `diff_setup_done()` output formats.
            // Each still changes the diff-of-diffs body this port cannot render,
            // so they stay deferred; but their bits are tracked here so the
            // `cannot be used together` fatal can fire before any revision is
            // resolved. `-s`/`--no-patch` assigns `NO_OUTPUT`, clearing the rest.
            "--name-only" | "--name-status" | "--check" => opts.defer(unsupported_flag(a)),
            // `-s`/`--no-patch` assigns `DIFF_FORMAT_NO_OUTPUT`, clearing the
            // other format bits, and suppresses the diff-of-diffs body entirely
            // — leaving the pair headers, which this port renders. So it is
            // implemented here, not deferred.
            "-s" | "--no-patch" => output_format = FMT_NO_OUTPUT,
            // `--ws-error-highlight=<kind>` only tints whitespace errors when
            // color is on. This port always emits with color off, so it is a
            // byte-for-byte no-op; accept it and consume a detached value so the
            // value is not mistaken for a revision.
            "--ws-error-highlight" => {
                if inline.is_none() {
                    i += 1;
                }
            }
            // Upstream parses `--inter-hunk-context` as `OPTION_UNSIGNED` at
            // parse time, before any revision is resolved, so a bad value is
            // reported here (exit 129) rather than deferred to output. A value
            // upstream accepts is recorded and deferred like any other diff
            // option this port does not render.
            "--inter-hunk-context" => {
                let arg = match inline {
                    Some(v) => v.to_string(),
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            None => {
                                return Ok(option_error(
                                    "option `inter-hunk-context' requires a value",
                                ))
                            }
                        }
                    }
                };
                // `else if (!*arg)` in the `OPTION_UNSIGNED` case: an empty
                // value has its own message, distinct from a malformed one.
                if arg.is_empty() {
                    return Ok(option_error(
                        "option `inter-hunk-context' expects a numerical value",
                    ));
                }
                // `interhunkcontext` has 4-byte precision, so the bound is
                // `UINTMAX_MAX >> (64 - 32)` = `u32::MAX`.
                match git_parse_unsigned(&arg, u32::MAX as u64) {
                    // Accepted by upstream but not rendered by this port.
                    Ok(_) => opts.defer(unsupported_flag(a)),
                    Err(MagnitudeError::Range) => {
                        return Ok(option_error(&format!(
                            "value {arg} for option `inter-hunk-context' not in range \
                             [0,4294967295]"
                        )))
                    }
                    Err(MagnitudeError::Invalid) => {
                        return Ok(option_error(
                            "option `inter-hunk-context' expects a non-negative integer \
                             value with an optional k/m/g suffix",
                        ))
                    }
                }
            }
            // `--no-<int-option>` takes `OPTION_INTEGER`'s unset branch, which
            // stores 0 — *not* the value the struct was initialised with. So
            // `--no-creation-factor` weights creation at zero and nothing ever
            // pairs, where the default 60 pairs a lightly edited commit.
            "--no-creation-factor" => opts.creation_factor = 0,
            "--creation-factor" => {
                let value = match inline {
                    Some(v) => v.to_string(),
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            None => {
                                return Ok(option_error(
                                    "option `creation-factor' requires a value",
                                ))
                            }
                        }
                    }
                };
                // `OPTION_INTEGER`'s three failures, all `error:` at 129 with no
                // usage block: an empty value, a malformed one, and one outside
                // the `int` the option writes into.
                if value.is_empty() {
                    return Ok(option_error("option `creation-factor' expects a numerical value"));
                }
                match git_parse_signed(&value, i32::MIN as i64, i32::MAX as i64) {
                    Ok(n) => opts.creation_factor = n,
                    Err(MagnitudeError::Range) => {
                        return Ok(option_error(&format!(
                            "value {value} for option `creation-factor' not in range \
                             [-2147483648,2147483647]"
                        )))
                    }
                    Err(MagnitudeError::Invalid) => {
                        return Ok(option_error(
                            "option `creation-factor' expects an integer value with an \
                             optional k/m/g suffix",
                        ))
                    }
                }
            }
            // parse_options_step() answers `-h` where it meets it, on stdout at
            // 129 — ahead of `unknown_option()`, whose `error:` line and stderr
            // belong to rejections.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" => return Ok(super::show_usage(USAGE)),
            // A name `parse_options()` has never heard of loses immediately — it never
            // reaches the revision arguments, so unlike an unimplemented option there is
            // nothing to defer it behind.
            _ if !is_known_option(name) => return Ok(unknown_option(a)),
            _ => {
                // `--summary` and the `--dirstat` family (short of
                // `--dirstat=lines`) are the two formats that provably write
                // nothing for range-diff's filepair — see [`format_writes`] — so
                // there is nothing to defer them behind. They are still recorded
                // below, because setting *any* format bit suppresses the
                // `DIFF_FORMAT_PATCH` fallback and so does change the page.
                let writes_nothing = matches!(
                    format_bits(name, inline),
                    Some((FMT_SUMMARY, _)) | Some((FMT_DIRSTAT, false))
                );
                if !writes_nothing {
                    opts.defer(unsupported_flag(a));
                }
                if inline.is_none()
                    && (LONG_TAKES_VALUE.contains(&name) || SHORT_TAKES_VALUE.contains(&name))
                {
                    i += 1;
                }
            }
        }
        // Accumulated here rather than in each arm because two dozen spellings
        // share it, most of them reaching the catch-all above. `-s`/`--no-patch`
        // is the one option that *assigns* `output_format`, so it is not listed
        // in [`format_bits`] and its arm has already run.
        if let Some((bits, by_line)) = format_bits(name, inline) {
            output_format |= bits;
            if bits & FMT_DIRSTAT != 0 {
                dirstat_by_line = by_line;
            }
            // Every format but the three `OPT_BIT_F` ones unsets `NO_OUTPUT`.
            if bits & (FMT_NAME | FMT_NAME_STATUS | FMT_CHECKDIFF) == 0 {
                output_format &= !FMT_NO_OUTPUT;
            }
        }
        i += 1;
    }

    // `cmd_log_init_finish()`: with no `--notes`/`--no-notes` of its own, a run
    // whose pretty format is a built-in one — and the inner `git log` uses
    // `--pretty=medium` — renders the default notes tree.
    if !opts.notes.given {
        opts.notes.enable_default();
    }

    // `diff_setup_done()` runs before any revision is resolved: two or more of
    // `--name-only`/`--name-status`/`--check`/`-s` is a fatal (128) here, ahead
    // of the argument-shape (129), `--left-only`/`--right-only` (255) and range
    // (128/255) checks below. Value errors (`--creation-factor`,
    // `--inter-hunk-context`) already returned 129 inside the loop, matching
    // upstream's parse-options-first ordering.
    if (output_format & FMT_EXCLUSIVE).count_ones() >= 2 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' \
             cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }
    // One of those four wipes out every other format (diff.c:5261-5262), which
    // is why `--check` prints no patch even next to an explicit `-p`.
    if output_format & FMT_EXCLUSIVE != 0 {
        output_format &= !FMT_CLEARED_BY_EXCLUSIVE;
    }
    // `flags.quick` becomes `DIFF_FORMAT_NO_OUTPUT` here (diff.c:5348-5352),
    // *after* the test above and after the whole argv has been read — so
    // `--quiet` neither joins that fatal nor loses to a format option that
    // follows it, which is the whole reason it is not folded into `-s`.
    if quick {
        output_format = FMT_NO_OUTPUT;
    }
    // `show_range_diff()`'s fallback (range-diff.c:551-552): a run that named no
    // format at all gets `DIFF_FORMAT_PATCH`. Any format bit at all — even one
    // that writes nothing, like `--summary` — suppresses it, which is why
    // `range-diff --summary` prints pair headers and no bodies.
    if output_format == 0 {
        output_format = FMT_PATCH;
    }
    // Everything below asks only whether the diff-of-diffs body is written.
    opts.no_patch = output_format & FMT_PATCH == 0;
    // The three pickaxe refusals, in `diff_setup_done()`'s own order
    // (diff.c:5263-5273), each a `HAS_MULTI_BITS` test on `pickaxe_opts`.
    for (mask, message) in [
        (
            PICKAXE_KINDS_MASK,
            "options '-G', '-S', and '--find-object' cannot be used together",
        ),
        (
            PICKAXE_G_REGEX_MASK,
            "options '-G' and '--pickaxe-regex' cannot be used together, \
             use '--pickaxe-regex' with '-S'",
        ),
        (
            PICKAXE_ALL_OBJFIND_MASK,
            "options '--pickaxe-all' and '--find-object' cannot be used together, \
             use '--pickaxe-all' with '-G' and '-S'",
        ),
    ] {
        if (pickaxe_mask & mask).count_ones() >= 2 {
            eprintln!("fatal: {message}");
            return Ok(ExitCode::from(128));
        }
    }
    // `--follow` last (diff.c:5364-5365), and unconditionally: range-diff routes
    // every `-- <path>` to `log_arg`, so `diffopt.pathspec` is always empty and
    // `diff_check_follow_pathspec()` always takes its `ps->nr != 1` die.
    if follow {
        eprintln!("fatal: --follow requires exactly one pathspec");
        return Ok(ExitCode::from(128));
    }

    let repo = match repo {
        Some(r) => r,
        None => gix::discover(".")?,
    };

    // builtin/range-diff.c:89 — "force color when --dual-color was used", applied after
    // `diff_setup_done()` so it overrides `--no-color` and the `color.diff`/`color.ui`
    // config alike. Without it the palette is the ordinary `git_diff_ui_config()` one,
    // which resolves to off whenever stdout is not a terminal.
    let want_color = simple_color == 0 || diff_color::resolve_color(&repo, color_when);
    opts.colors = diff_color::DiffColors::resolve(&repo, want_color);
    // The pair headers are colored below, but the diff-of-diffs body carries the
    // dual-color markup (`contextDimmed`/`oldBold`/… under
    // `o->flags.dual_color_diffed_diffs`) that this port does not render. Refuse a run
    // that would print one rather than emit a plainly-colored approximation of it.
    if want_color && !opts.no_patch {
        opts.defer("colored diff-of-diffs body is not ported".to_string());
    }

    // Upstream's order: the argument shape is checked first (a bad shape is 129
    // even when `--left-only --right-only` were also given), and the two-range
    // form resolves each operand through `is_range_diff_range()`, which exits
    // 128 the moment `setup_revisions()` meets a token it cannot resolve.
    let dash_dash = pos.iter().position(|s| s.as_str() == "--");
    let Classified {
        range1,
        range2,
        extra,
    } = match classify(&repo, &pos, dash_dash) {
        Ok(c) => c,
        Err(code) => return Ok(code),
    };

    if opts.left_only && opts.right_only {
        // Upstream's `error()`, whose -1 return becomes git's exit status 255.
        eprintln!("error: options '--left-only' and '--right-only' cannot be used together");
        return Ok(ExitCode::from(255));
    }

    // Upstream resolves each range by running `git log` over it, oldest range
    // first; a range naming an unknown revision is fatal before any patch is
    // read, and `git log`'s -1 return becomes exit status 255.
    let mut ends1 = match endpoints(&repo, &range1) {
        Ok(e) => walkable(&repo, e),
        Err(_) => return Ok(could_not_parse_log(&repo, &range1)),
    };
    // The inner `git log` takes the range first and the forwarded `log_arg`
    // after it (`range-diff.c` `read_patches()`), so a range it cannot resolve
    // is reported ahead of a `--diff-merges` style it cannot parse — but a
    // resolvable range1 lets the style error out, before range2 is ever tried.
    if let Some(v) = &bad_diff_merges {
        eprintln!("fatal: invalid value for '--diff-merges': '{v}'");
        return Ok(log_parse_failed(&range1));
    }
    // Everything from the form's consumed count onward — the `--` included — is
    // what upstream hands that same `git log` after its range
    // (`strvec_pushv(&log_arg, argv + …)`, builtin/range-diff.c:128/148/179,
    // spliced in at range-diff.c:71-73). It is resolved here, between the two
    // ranges, because range1's log is the one that meets it first: an operand it
    // rejects is reported against range1, and range2's log never runs.
    let operands = match extra_operands(&repo, &range1, &extra) {
        Ok(e) => e,
        Err(code) => return Ok(code),
    };
    let mut ends2 = match endpoints(&repo, &range2) {
        Ok(e) => walkable(&repo, e),
        Err(_) => return Ok(could_not_parse_log(&repo, &range2)),
    };
    // range2's log is a *second* process handed the same `log_arg` list, so it
    // resolves every one of those operands over again. That is observable: an
    // operand that is a full-length hex and also a ref name earns
    // `get_oid_basic()`'s `warning: refname … is ambiguous.` once per log, and
    // stock 2.55.0 prints it twice for `range-diff <r1> <r2> <40-hex>`. Resolving
    // once and reusing the ids under-warns by exactly one. The result is
    // discarded — it can only equal the first pass's — for the same reason
    // `notes1` and `notes2` are loaded separately below: what is being reproduced
    // is the second log's side effects, not a second answer.
    if let Err(code) = extra_operands(&repo, &range2, &extra) {
        return Ok(code);
    }
    let extra = operands;

    // The one `log_arg` list is appended to *both* logs, so a revision operand
    // widens each walk alike.
    for (tips, hidden) in [&mut ends1, &mut ends2] {
        tips.extend(extra.tips.iter().copied());
        hidden.extend(extra.hidden.iter().copied());
    }

    // A pathspec limits both which commits appear and which file sections each
    // rendered patch carries.
    let matcher = build_matcher(&repo, &extra.pathspec)?;


    let mailmap = repo.open_mailmap();
    // `--notes[=<ref>]`/`--no-notes` are passed straight to the `git log`
    // upstream runs, so the display refs are the ones that log would have used.
    // Loaded once per range because upstream runs one log per range: a ref that
    // does not resolve warns once for each of them.
    let notes1 = super::notes::load_display(&repo, &opts.notes)?;
    let mut a = read_patches(&repo, ends1, &mailmap, matcher.as_ref(), &opts.abbrev, &notes1)?;
    let notes2 = super::notes::load_display(&repo, &opts.notes)?;
    let mut b = read_patches(&repo, ends2, &mailmap, matcher.as_ref(), &opts.abbrev, &notes2)?;

    find_exact_matches(&mut a, &mut b);
    if let Err(msg) = get_correspondences(&mut a, &mut b, opts.creation_factor, opts.max_memory) {
        crate::git_fatal!("{msg}");
    }

    // A deferred (unimplemented) diff option configures the *outer* diff, the
    // diff-of-diffs, and nothing else: `add_diff_options()` binds the whole diff
    // table to `diffopt` (builtin/range-diff.c:83) and only `--notes`,
    // `--diff-merges` and `--remerge-diff` are `OPT_PASSTHRU_ARGV` into the
    // inner `git log`. That outer diff is reached from exactly one place,
    // `patch_diff()`, which `output()` calls only for a *matched* pair
    // (range-diff.c:567-573). So when no pair matched, not one byte of the page
    // can depend on the option, and it is emitted exactly as upstream emits it.
    //
    // For a pair that did match, the question is whether `diff_flush()` writes
    // anything the option could show through. Two ways it does not:
    //
    // * `output_format` asks for nothing this port leaves unrendered — the
    //   `DIFF_FORMAT_PATCH` default, or `DIFF_FORMAT_NO_OUTPUT` from `-s` /
    //   `--quiet`. See [`format_writes`] for the formats that write and the
    //   three that provably never do.
    // * The two patch texts are byte-identical — the `=` pair
    //   `strcmp(a_util->patch, b_util->patch)` reports (range-diff.c:429).
    //   `patch_diff()` still runs, but the filepair it queues has nothing to
    //   report, so the patch itself is empty and so is the stat.
    //
    // Otherwise stop rather than print a diff-of-diffs that ignored the option.
    let observable = b.iter().any(|p| {
        if p.matching < 0 {
            return false;
        }
        let diff_is_empty = a[p.matching as usize].text == p.text;
        // The patch body is this port's own output, so it shows a deferred
        // option only when there is a body to show it in.
        (output_format & FMT_PATCH != 0 && !diff_is_empty)
            || format_writes(output_format, dirstat_by_line, diff_is_empty)
    });
    if let Some(reason) = &opts.deferred {
        if observable {
            crate::git_fatal!("{reason}");
        }
    }

    let mut rendered: Vec<u8> = Vec::new();
    output(&mut rendered, &mut a, &b, &opts)?;

    // Everything upstream writes — the pair headers included — goes to
    // `diffopt.file`, which `--output=<file>` has replaced.
    match opts.output.take() {
        Some(mut file) => {
            file.write_all(&rendered)?;
            file.flush()?;
        }
        None => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            out.write_all(&rendered)?;
            out.flush()?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `RANGE_DIFF_CREATION_FACTOR_DEFAULT`'s sibling
/// `CREATION_FACTOR_FOR_THE_SAME_SERIES` (`builtin/log.c`), the factor
/// `format-patch` passes when `--creation-factor` was not given: a rerolled
/// series is expected to be the *same* series, so creations are made expensive
/// enough that almost any pairing is preferred.
pub(super) const CREATION_FACTOR_FOR_THE_SAME_SERIES: i64 = 999;

/// Port of `show_range_diff()` (`range-diff.c`) as `format-patch` calls it:
/// render the diff of the two commit ranges into `out`, with the four-space
/// indent and dual-colour settings `log-tree.c`'s `show_diff_of_diff()` asks for
/// (colour is always off here, which makes dual and simple colouring identical).
///
/// `notes` mirrors `get_notes_args()`: `format-patch` forwards `--no-notes`
/// unless the series itself is rendering notes.
///
/// Both ranges must already be known to resolve; an endpoint that does not is
/// reported the way the inner `git log` reports it and returns the exit code
/// upstream leaves behind.
pub(super) fn show_range_diff(
    repo: &gix::Repository,
    range1: &str,
    range2: &str,
    creation_factor: i64,
    notes_on: bool,
    out: &mut Vec<u8>,
) -> Result<std::result::Result<(), ExitCode>> {
    // `get_notes_args()`: `format-patch` forwards a bare `--notes` (the default
    // tree) or `--no-notes`, never an explicit ref.
    let mut notes_opt = super::notes::DisplayOpt::default();
    if notes_on {
        notes_opt.enable_default();
    }
    notes_opt.given = true;
    let opts = Opts {
        creation_factor,
        left_only: false,
        right_only: false,
        notes: notes_opt,
        no_patch: false,
        // `show_range_diff()` is reached from `log-tree.c`, which leaves
        // `range_diff_opts` at its defaults.
        max_memory: MAX_MEMORY_DEFAULT,
        algorithm: Algorithm::Myers,
        indent_heuristic: true,
        context: 3,
        indicators: [b'+', b'-', b' '],
        output: None,
        abbrev: Abbrev::Default,
        deferred: None,
        // `format-patch --range-diff` embeds the range-diff in a patch, which is never
        // colored.
        colors: diff_color::DiffColors::disabled(),
    };
    let ends1 = match endpoints(repo, range1) {
        Ok(e) => walkable(repo, e),
        Err(_) => return Ok(Err(could_not_parse_log(repo, range1))),
    };
    let ends2 = match endpoints(repo, range2) {
        Ok(e) => walkable(repo, e),
        Err(_) => return Ok(Err(could_not_parse_log(repo, range2))),
    };

    let mailmap = repo.open_mailmap();
    let notes = super::notes::load_display(repo, &opts.notes)?;
    let mut a = read_patches(repo, ends1, &mailmap, None, &opts.abbrev, &notes)?;
    let mut b = read_patches(repo, ends2, &mailmap, None, &opts.abbrev, &notes)?;

    find_exact_matches(&mut a, &mut b);
    if let Err(msg) = get_correspondences(&mut a, &mut b, opts.creation_factor, opts.max_memory) {
        crate::git_fatal!("{msg}");
    }
    output(out, &mut a, &b, &opts)?;
    Ok(Ok(()))
}

/// `prepare_revision_walk()`'s `handle_commit()` pass over a range's pending
/// objects: tags are peeled and trees and blobs are dropped without a word,
/// because the inner `git log` has neither `tree_objects` nor `blob_objects` set.
///
/// It is deliberately not folded into [`endpoints`]: `is_range_diff_range()`
/// counts `revs.pending.nr` *before* this runs, so a `<tree>..<tip>` operand is
/// still a range with one positive and one negative object even though the walk
/// will go on to ignore the tree.
fn walkable(repo: &gix::Repository, ends: (Vec<ObjectId>, Vec<ObjectId>)) -> (Vec<ObjectId>, Vec<ObjectId>) {
    let keep = |ids: Vec<ObjectId>| -> Vec<ObjectId> {
        ids.into_iter().filter_map(|id| crate::objname::walk_pending(repo, id)).collect()
    };
    (keep(ends.0), keep(ends.1))
}

/// Upstream's `usage_msg_opt()`: the reason, a blank line, the synopsis, 129.
fn usage_error(reason: &str) -> ExitCode {
    eprintln!("fatal: {reason}\n");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// What `git log <range>` prints when an endpoint names nothing, followed by
/// `builtin/range-diff.c`'s own `error()`. `git log`'s failure is upstream's
/// exit status 255.
///
/// The message is `log`'s own, so it comes from the shared
/// [`super::log::bad_revision_message_in`] rather than being spelled out here:
/// an endpoint that is a full-length hex the repository does not have resolves
/// (see [`crate::objname`]) and reaches `dotdot_missing()`, which names the
/// whole range as `Invalid revision range <a>..<b>` instead of giving the
/// `ambiguous argument` advice.
fn could_not_parse_log(repo: &gix::Repository, range: &str) -> ExitCode {
    eprint!("{}", super::log::bad_revision_message_in(repo, range));
    log_parse_failed(range)
}

/// `builtin/range-diff.c`'s own `error()` for a failed inner log, on its own:
/// the caller has already printed whatever that log died with. Upstream's -1
/// return is exit status 255.
fn log_parse_failed(range: &str) -> ExitCode {
    eprintln!("error: could not parse log for '{range}'");
    ExitCode::from(255)
}

/// A parse-options value error: `error: <reason>` on stderr, exit 129, and —
/// unlike [`usage_error`] — no synopsis, because parse-options reports these
/// value failures with a bare `error()` and no `usage_with_options()` call.
fn option_error(reason: &str) -> ExitCode {
    eprintln!("error: {reason}");
    ExitCode::from(129)
}

/// parse-options' required-value fetch for a long option: the attached
/// `=<value>` if there is one, else the next argv element (consumed), else the
/// 129 ``error: option `<name>' requires a value``.
///
/// `name` arrives spelled with its leading `--`, which the message drops.
fn required_value(
    args: &[String],
    i: &mut usize,
    name: &str,
    inline: Option<&str>,
) -> std::result::Result<String, ExitCode> {
    if let Some(v) = inline {
        return Ok(v.to_string());
    }
    *i += 1;
    match args.get(*i) {
        Some(v) => Ok(v.clone()),
        None => Err(option_error(&format!(
            "option `{}' requires a value",
            name.trim_start_matches('-')
        ))),
    }
}

/// The bare `strerror` text of an I/O failure: Rust appends ` (os error <n>)` to
/// the system message, which git's `%s` of `strerror(errno)` never prints.
fn io_reason(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// The errno `git_parse_unsigned()` sets, which parse-options turns into two
/// different messages: `EINVAL` (malformed) and `ERANGE` (out of bounds).
enum MagnitudeError {
    /// `EINVAL`: not a non-negative integer with an optional k/m/g suffix.
    Invalid,
    /// `ERANGE`: parsed, but overflowed `uintmax_t` or exceeded the bound.
    Range,
}

/// Port of `get_unit_factor()` (`parse.c`): the k/m/g suffix multiplier, `1` for
/// no suffix, `None` for anything else. `strcasecmp` compares the whole
/// remainder, so only an exact `k`/`m`/`g` (any case) is a unit.
fn get_unit_factor(end: &[u8]) -> Option<u64> {
    if end.is_empty() {
        Some(1)
    } else if end.eq_ignore_ascii_case(b"k") {
        Some(1024)
    } else if end.eq_ignore_ascii_case(b"m") {
        Some(1024 * 1024)
    } else if end.eq_ignore_ascii_case(b"g") {
        Some(1024 * 1024 * 1024)
    } else {
        None
    }
}

/// Port of `git_parse_unsigned()` (`parse.c`) with `OPTION_UNSIGNED`'s bound
/// applied: `value` is a non-negative integer with an optional k/m/g suffix,
/// capped at `max`. The C reads `strtoumax(value, &end, 0)` — base auto-detect,
/// so `0x…` is hex and a leading `0` is octal — after rejecting any string
/// containing `-` (which `strtoumax` would otherwise accept), then multiplies by
/// the unit factor and range-checks. `errno` maps to [`MagnitudeError`].
fn git_parse_unsigned(value: &str, max: u64) -> Result<u64, MagnitudeError> {
    let bytes = value.as_bytes();
    // `if (strchr(value, '-'))` — a minus sign anywhere is rejected up front.
    if bytes.contains(&b'-') {
        return Err(MagnitudeError::Invalid);
    }

    // `strtoumax(value, &end, 0)`: skip leading isspace, an optional `+`, then
    // pick the base from a `0x`/`0` prefix.
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'+' {
        i += 1;
    }
    let (base, digits_start): (u64, usize) = if i < bytes.len() && bytes[i] == b'0' {
        if bytes.get(i + 1).is_some_and(|&b| b == b'x' || b == b'X')
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
        {
            (16, i + 2)
        } else {
            // A leading `0` is octal; the `0` itself is a valid octal digit, so
            // a bare `0` parses as zero.
            (8, i)
        }
    } else {
        (10, i)
    };

    let mut end = digits_start;
    let mut val: u128 = 0;
    let mut overflow = false;
    while end < bytes.len() {
        let digit = match bytes[end] {
            b'0'..=b'9' => u64::from(bytes[end] - b'0'),
            b'a'..=b'f' => u64::from(bytes[end] - b'a') + 10,
            b'A'..=b'F' => u64::from(bytes[end] - b'A') + 10,
            _ => break,
        };
        if digit >= base {
            break;
        }
        val = val * u128::from(base) + u128::from(digit);
        if val > u128::from(u64::MAX) {
            overflow = true;
        }
        end += 1;
    }
    // `if (end == value)` — no digits at all is malformed.
    if end == digits_start {
        return Err(MagnitudeError::Invalid);
    }
    // `strtoumax` sets `ERANGE` when the value overflows `uintmax_t`.
    if overflow {
        return Err(MagnitudeError::Range);
    }

    let factor = get_unit_factor(&bytes[end..]).ok_or(MagnitudeError::Invalid)?;
    // `unsigned_mult_overflows(factor, val) || factor * val > max`.
    match (val as u64).checked_mul(factor) {
        Some(product) if product <= max => Ok(product),
        _ => Err(MagnitudeError::Range),
    }
}

/// Port of `git_parse_signed()` (`parse.c`) as `OPTION_INTEGER` reaches it: a
/// signed integer with an optional k/m/g suffix, range-checked against
/// `[min, max]`.
///
/// The C is `strtoimax(value, &end, 0)` — leading whitespace skipped, an
/// optional sign, then base 16 for a `0x` prefix, base 8 for a leading `0` and
/// base 10 otherwise — followed by `parse_unit_factor(end, &factor)` on whatever
/// is left, so a trailing space is malformed while a leading one is not, and
/// `0x10` is 16 while `09` is not a number at all. An overflow of `intmax_t`, an
/// overflowing multiply by the unit factor, and a product outside the option's
/// own bounds are all `ERANGE`, which parse-options reports as the same
/// `value <v> for option … not in range [<min>,<max>]`.
///
/// Unlike [`git_parse_unsigned`] a `-` is not rejected up front: it is the sign
/// `strtoimax` accepts, so `--creation-factor=-1k` is -1024.
fn git_parse_signed(value: &str, min: i64, max: i64) -> Result<i64, MagnitudeError> {
    let bytes = value.as_bytes();

    // `strtoimax(value, &end, 0)`: skip leading isspace, take an optional sign,
    // then pick the base from a `0x`/`0` prefix.
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let (base, digits_start): (i128, usize) = if bytes.get(i) == Some(&b'0') {
        if bytes.get(i + 1).is_some_and(|&b| b == b'x' || b == b'X')
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
        {
            (16, i + 2)
        } else {
            // A leading `0` is octal; the `0` itself is a valid octal digit, so
            // a bare `0` parses as zero.
            (8, i)
        }
    } else {
        (10, i)
    };

    let mut end = digits_start;
    // Accumulated as a magnitude so that `i64::MIN` is representable, and in
    // `i128` so the `ERANGE` test is a comparison rather than a wrapping add.
    let mut magnitude: i128 = 0;
    let mut overflow = false;
    while end < bytes.len() {
        let digit = i128::from(match bytes[end] {
            b'0'..=b'9' => bytes[end] - b'0',
            b'a'..=b'f' => bytes[end] - b'a' + 10,
            b'A'..=b'F' => bytes[end] - b'A' + 10,
            _ => break,
        });
        if digit >= base {
            break;
        }
        magnitude = magnitude * base + digit;
        if magnitude > i128::from(i64::MAX) + 1 {
            overflow = true;
            magnitude = i128::from(i64::MAX) + 1;
        }
        end += 1;
    }
    // `if (end == value)` — no digits at all is malformed.
    if end == digits_start {
        return Err(MagnitudeError::Invalid);
    }
    // `strtoimax` sets `ERANGE` outside `[INTMAX_MIN, INTMAX_MAX]`.
    let signed = if negative { -magnitude } else { magnitude };
    if overflow || signed > i128::from(i64::MAX) || signed < i128::from(i64::MIN) {
        return Err(MagnitudeError::Range);
    }

    let factor = i128::from(get_unit_factor(&bytes[end..]).ok_or(MagnitudeError::Invalid)?);
    let product = signed * factor;
    if product < i128::from(min) || product > i128::from(max) {
        return Err(MagnitudeError::Range);
    }
    Ok(product as i64)
}


/// `find_unique_abbrev()`: the abbreviated id printed in a pair header. With no
/// `--abbrev`/`--no-abbrev`, gitoxide's `shorten()` applies `core.abbrev` (the 7
/// default). Otherwise probe from the requested minimum length upward for the
/// shortest hex prefix that resolves unambiguously to this commit — the value
/// git's `find_unique_abbrev(oid, len)` returns — falling back to the full id.
fn abbrev_id(repo: &gix::Repository, id: ObjectId, abbrev: &Abbrev) -> Result<String> {
    match abbrev {
        Abbrev::Default => Ok(id.attach(repo).shorten()?.to_string()),
        Abbrev::Len(min) => {
            let hex = id.to_hex().to_string();
            let min = (*min).clamp(4, hex.len());
            for len in min..hex.len() {
                if let Ok(found) = repo.rev_parse_single(&hex[..len]) {
                    if found.detach() == id {
                        return Ok(hex[..len].to_string());
                    }
                }
            }
            Ok(hex)
        }
    }
}

// ---------------------------------------------------------------------------
// Argument dispatch (builtin/range-diff.c)
// ---------------------------------------------------------------------------

/// A resolved argument shape: the two ranges to compare and the trailing
/// operands (from the form's consumed count on, including any `--`) that
/// upstream forwards to the inner `git log`.
struct Classified {
    range1: String,
    range2: String,
    extra: Vec<String>,
}

/// The three answers `is_range_diff_range()` can give, distinguishing "resolves
/// but is not a range" from "does not resolve at all", because upstream turns
/// the latter into a fatal `bad revision` (exit 128) rather than a fall-through.
enum RangeKind {
    /// Both a positive and a negative endpoint: a range.
    Range,
    /// Resolves, but not to a range (a plain committish such as `main`).
    NotRange,
    /// `setup_revisions()` could not resolve a token — upstream dies here.
    Bad,
}

/// Upstream's argument classification (`cmd_range_diff`), transcribed with its
/// exact precedence: three committishes, then two commit ranges, then one
/// symmetric range, then the `need two commit ranges` usage error. `Err(code)`
/// carries an already-reported exit status — a usage error (129) or, when a
/// two-range operand fails to resolve, `is_range_diff_range()`'s fatal 128.
///
/// `dash_dash` is the index of the first `--` in `pos` (or `None`). When it is
/// present it *forces* one of the three forms by position exactly as upstream
/// does, validating the operands and reporting the matching message.
fn classify(
    repo: &gix::Repository,
    pos: &[String],
    dash_dash: Option<usize>,
) -> Result<Classified, ExitCode> {
    let argc = pos.len();

    // Three committishes: `<base> <old-tip> <new-tip>`.
    if dash_dash == Some(3)
        || (dash_dash.is_none()
            && argc > 2
            && committish(repo, &pos[0])
            && committish(repo, &pos[1])
            && committish(repo, &pos[2]))
    {
        if dash_dash.is_some() {
            for token in &pos[..3] {
                if !committish(repo, token) {
                    return Err(usage_error(&format!("not a revision: '{token}'")));
                }
            }
        }
        let offset = dash_dash.unwrap_or(3);
        return Ok(Classified {
            range1: format!("{}..{}", pos[0], pos[1]),
            range2: format!("{}..{}", pos[0], pos[2]),
            extra: pos[offset..].to_vec(),
        });
    }

    // Two commit ranges. Auto-detection resolves each operand up front; a token
    // `setup_revisions()` cannot parse is fatal (`bad revision`, 128) rather
    // than a fall-through, and the second operand is only consulted when the
    // first is a range (upstream's `&&` short-circuit).
    let two_ranges = if dash_dash == Some(2) {
        true
    } else if dash_dash.is_none() && argc > 1 {
        match is_range_diff_range(repo, &pos[0]) {
            RangeKind::Bad => return Err(bad_revision(repo, &pos[0])),
            RangeKind::NotRange => false,
            RangeKind::Range => match is_range_diff_range(repo, &pos[1]) {
                RangeKind::Bad => return Err(bad_revision(repo, &pos[1])),
                RangeKind::NotRange => false,
                RangeKind::Range => true,
            },
        }
    } else {
        false
    };
    if two_ranges {
        if dash_dash.is_some() {
            for token in &pos[..2] {
                match is_range_diff_range(repo, token) {
                    RangeKind::Bad => return Err(bad_revision(repo, token)),
                    RangeKind::NotRange => {
                        return Err(usage_error(&format!("not a commit range: '{token}'")))
                    }
                    RangeKind::Range => {}
                }
            }
        }
        let offset = dash_dash.unwrap_or(2);
        return Ok(Classified {
            range1: pos[0].clone(),
            range2: pos[1].clone(),
            extra: pos[offset..].to_vec(),
        });
    }

    // One symmetric range: `<old-tip>...<new-tip>`, either side defaulting to
    // `HEAD`. Upstream detects this with a raw `strstr(argv[0], "...")`, so the
    // endpoints are validated later by the range resolution, not here.
    if dash_dash == Some(1) || (dash_dash.is_none() && argc > 0 && pos[0].contains("...")) {
        if dash_dash.is_some() && !pos[0].contains("...") {
            return Err(usage_error(&format!("not a symmetric range: '{}'", pos[0])));
        }
        let spec = &pos[0];
        let dots = spec.find("...").expect("symmetric form has ...");
        let a = if dots == 0 { "HEAD" } else { &spec[..dots] };
        let b = if spec.len() > dots + 3 {
            &spec[dots + 3..]
        } else {
            "HEAD"
        };
        let offset = dash_dash.unwrap_or(1);
        return Ok(Classified {
            range1: format!("{b}..{a}"),
            range2: format!("{a}..{b}"),
            extra: pos[offset..].to_vec(),
        });
    }

    Err(usage_error("need two commit ranges"))
}

/// `repo_get_oid_committish()`: does `spec` name an object at all?
///
/// Never fatal — a miss is reported as `false`, matching upstream's use of the
/// return value as a mere predicate in the three-committish test.
///
/// The name is resolved, never peeled. `repo_get_oid_committish()` is
/// `repo_get_oid_with_context(…, GET_OID_COMMITTISH, …)`, and that flag steers
/// how a *short* or `@{…}` name is disambiguated; it does not reject a name that
/// resolves to a tree or a blob. So stock 2.55.0 reads `range-diff <tree> <a>
/// <b>` as the three-committish form and renders it, while peeling here refused
/// the same command line as `need two commit ranges`. Resolution — and the
/// `get_oid_basic()` ambiguity warning that comes with it — is
/// [`crate::objname::resolve`]'s, shared with every other command that takes an
/// object name from argv.
///
/// The operand reaches that call **as typed**, and that is the one place this
/// differs from every *revision* operand in the file. The `^` exclusion mark
/// belongs to `revision.c`:
///
/// ```c
/// if (*arg == '^') {
///         local_flags = UNINTERESTING | BOTTOM;
///         arg++;
/// }
/// ```
///
/// `cmd_range_diff()` calls `repo_get_oid_committish()` directly, so no such
/// strip happens and `object-name.c` sees the leading `^`. Nothing there has a
/// form for it: `get_oid_basic()` measures one character too many for its
/// full-hex branch — taking the `warning: refname … is ambiguous.` with it —
/// `peel_onion()` and the `~<n>`/`^<n>` suffix rule both look at the *tail*, and
/// `repo_dwim_ref()` cannot match because `check_refname_format()` bans `^` in a
/// ref name. The answer is a silent `false`, which is why stock says nothing for
/// `range-diff ^<40-hex> <a> <b>` while handing the same operand to `git log` —
/// where the mark *is* stripped first — warns once.
fn committish(repo: &gix::Repository, spec: &str) -> bool {
    if spec.starts_with('^') {
        return false;
    }
    crate::objname::resolve(repo, spec).is_some()
}

/// `is_range_diff_range()`: run `spec` through the same resolution `git log`
/// uses and classify the result. An unresolvable token is [`RangeKind::Bad`],
/// which the caller turns into upstream's fatal `bad revision` (exit 128); a
/// resolved spec is a [`RangeKind::Range`] when it carries both a positive tip
/// and a hidden negative endpoint, and [`RangeKind::NotRange`] otherwise. This
/// recognises every spelling gitoxide's rev-parse does, so `<rev>^!` and
/// `<rev>^@` are handled alongside `<a>..<b>` and `<a>...<b>`.
///
/// ```c
/// if (setup_revisions(3, argv, &revs, NULL) == 1) {
///         for (i = 0; i < revs.pending.nr; i++)
///                 if (revs.pending.objects[i].item->flags & UNINTERESTING)
///                         negative++;
///                 else
///                         positive++;
/// }
/// …
/// return negative > 0 && positive > 0;
/// ```
///
/// The count walks the *pending list*, but `UNINTERESTING` is a bit on the
/// `struct object` each entry points at — not on the entry. So when both
/// endpoints name the same object the two entries are the same object, and
/// `handle_dotdot_1()`'s
///
/// ```c
/// a_obj->flags |= a_flags;
/// b_obj->flags |= flags;
/// ```
///
/// leaves that one object `UNINTERESTING` (`b_obj->flags |= 0` cannot clear what
/// the line above set). `<x>..<x>` is therefore `negative == 2, positive == 0` —
/// **not** a range — and stock answers `fatal: need two commit ranges` for
/// `range-diff HEAD..HEAD <a>..<b>`. The symmetric spelling reaches the same
/// place by a different route: `repo_get_merge_bases(a, a)` is `a` itself, and
/// `add_pending_commit_list(revs, exclude, flags_exclude)` marks it before
/// `a_flags`/`flags` are applied.
///
/// Modelled here by treating an id that appears on both sides as the single
/// object it is: the tips that are also hidden count as negative.
fn is_range_diff_range(repo: &gix::Repository, spec: &str) -> RangeKind {
    match endpoints(repo, spec) {
        Ok((tips, hidden)) => {
            let positive = tips.iter().filter(|id| !hidden.contains(id)).count();
            let negative = tips.len() + hidden.len() - positive;
            if positive > 0 && negative > 0 {
                RangeKind::Range
            } else {
                RangeKind::NotRange
            }
        }
        Err(_) => RangeKind::Bad,
    }
}

/// Upstream's fatal, exit 128, for a token `is_range_diff_range()` could not
/// turn into a range.
///
/// `is_range_diff_range()` hands `setup_revisions()` an argument vector that
/// ends in a literal `--` (range-diff.c), and `setup_revisions()` scans the
/// whole vector for one before it resolves anything — so `seen_dashdash` is
/// already set when a token fails, and the ending is
/// `die(_("bad revision '%s'"), arg)` rather than `verify_filename()`'s
/// `ambiguous argument` advice.
///
/// The exception is a token `handle_revision_arg()` dies *inside* — a range
/// whose endpoints resolve but whose objects are not in the database
/// (`dotdot_missing()`), or a single name `get_reference()` reports as
/// `bad object`. Both are reached because `get_oid_basic()` decodes a
/// full-length hex without consulting the object database, and both are asked of
/// [`super::log::early_revision_fatal`], which is where that pair of endings
/// lives for every command with a filename fallback. `cant_be_filename` is true
/// here: `is_range_diff_range()` hands `setup_revisions()` a `--` of its own, so
/// the operand can never be re-read as a path.
fn bad_revision(repo: &gix::Repository, spec: &str) -> ExitCode {
    if let Some(fatal) = super::log::early_revision_fatal(repo, spec, true) {
        eprint!("{fatal}");
        return ExitCode::from(128);
    }
    eprintln!("fatal: bad revision '{spec}'");
    ExitCode::from(128)
}

/// What the operands trailing the chosen argument form contribute to the walk.
///
/// They are not range-diff's own: upstream collects them into `log_arg` and hands
/// them to the inner `git log` of *each* range, so they read as ordinary `git log`
/// operands — more revisions, or a pathspec.
struct ExtraOperands {
    /// Positive endpoints, added to the tips of both ranges.
    tips: Vec<ObjectId>,
    /// Negative endpoints, added to the hidden set of both ranges.
    hidden: Vec<ObjectId>,
    /// The pathspec limiting both ranges.
    pathspec: Vec<String>,
}

/// Resolve the trailing operands the way `setup_revisions()` resolves them, and
/// report a rejection the way the inner `git log` for `range` would: the message
/// that log dies with, then `range-diff`'s own `could not parse log`, at 255.
///
/// The three outcomes, in `setup_revisions()`'s own order:
///
/// * `--` ends the revisions; everything after it is a pathspec, and none of it
///   is checked against the worktree.
/// * A token that resolves is another revision — but `verify_non_filename()`
///   refuses it when a file by that name also exists, because git will not guess
///   which was meant.
/// * The first token that does not resolve turns itself and every token after it
///   into the pathspec, provided each of them can be a path
///   (`for (j = i; j < argc; j++) verify_filename(…)`). A token starting with `^`
///   is exempt: it was explicitly negative, so it has no path reading to fall
///   back on and is a fatal `bad revision` instead.
fn extra_operands(
    repo: &gix::Repository,
    range: &str,
    extra: &[String],
) -> std::result::Result<ExtraOperands, ExitCode> {
    let mut out = ExtraOperands {
        tips: Vec::new(),
        hidden: Vec::new(),
        pathspec: Vec::new(),
    };
    for (n, arg) in extra.iter().enumerate() {
        if arg == "--" {
            out.pathspec = extra[n + 1..].to_vec();
            return Ok(out);
        }
        match endpoints(repo, arg) {
            Ok((tips, hidden)) => {
                if let Some(msg) = crate::setup::verify_non_filename(repo, arg) {
                    eprintln!("fatal: {msg}");
                    return Err(log_parse_failed(range));
                }
                out.tips.extend(tips);
                out.hidden.extend(hidden);
            }
            Err(_) => {
                // `handle_revision_arg()` has two endings that die *inside* it,
                // so the token never reaches either fallback below:
                // `dotdot_missing()` and `get_reference()`'s `bad object`.
                // `cant_be_filename` is false — these operands trail the ranges on
                // the inner `git log`, with no `--` in front of them — so a name
                // that is also a working-tree path is reported as "both revision
                // and filename" instead. Asked once and kept: the helper resolves
                // the name to answer, and git resolves an operand once.
                if let Some(message) = super::log::early_revision_fatal(repo, arg, false) {
                    eprint!("{message}");
                    return Err(log_parse_failed(range));
                }
                if arg.starts_with('^') {
                    eprintln!("fatal: bad revision '{arg}'");
                    return Err(log_parse_failed(range));
                }
                for (j, rest) in extra[n..].iter().enumerate() {
                    if let Some(msg) = crate::setup::verify_filename(rest, j == 0) {
                        eprintln!("fatal: {msg}");
                        return Err(log_parse_failed(range));
                    }
                }
                out.pathspec = extra[n..].to_vec();
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// The pathspec limiter, shared with every other verb.
type PathMatcher = super::log::PathspecMatcher;

/// Build the pathspec limiter, or `None` when there is nothing to limit — an
/// empty spec, or no spec at all, means every commit and every file section.
fn build_matcher(repo: &gix::Repository, pathspec: &[String]) -> Result<Option<PathMatcher>> {
    if pathspec.is_empty() || pathspec.iter().any(String::is_empty) {
        return Ok(None);
    }
    Ok(Some(PathMatcher::new(repo, pathspec)?))
}

/// Split a range into the tips it includes and the commits it hides.
///
/// `<a>..<b>` hides `a` and includes `b`; `<a>...<b>` includes both and hides
/// their merge bases, matching how `git log` resolves the same spelling. Any
/// other spelling gitoxide's rev-parse understands — `<rev>^!` (the commit with
/// its parents hidden), `<rev>^@` (only the parents), a bare committish, or
/// `^<rev>` — is mapped to the same tip/hidden split so the classifier can see
/// it as a range and the walk can traverse it. An unresolvable spec is an
/// error, which upstream reports as a `git log` failure.
fn endpoints(repo: &gix::Repository, spec: &str) -> Result<(Vec<ObjectId>, Vec<ObjectId>)> {
    // The two `..` spellings are `handle_dotdot_1()`'s, so ask
    // [`crate::objname`] rather than re-deriving them. What comes back is the
    // *pending* object of each endpoint, of whatever type — only the symmetric
    // form type-checks — because `is_range_diff_range()` counts
    // `revs.pending.nr` before `prepare_revision_walk()` has dropped anything,
    // and [`walkable`] is what applies the drop for the callers that walk.
    // `handle_dotdot_1()` resolves both endpoints through
    // `get_oid_with_context()` before it looks either of them up, so that pair of
    // calls is where an endpoint that is a full-length hex *and* a ref name earns
    // its `warning: refname … is ambiguous.`. [`crate::objname::dotdot`] itself is
    // quiet: it is a classifier, asked again by every caller that then wants to
    // diagnose what it classified.
    crate::objname::warn_dotdot_endpoints(repo, spec);
    match crate::objname::dotdot(repo, spec) {
        crate::objname::Dotdot::Ok { a, b } => {
            let symmetric = crate::objname::split_range(spec)
                .expect("a resolved range has a separator")
                .symmetric;
            if !symmetric {
                return Ok((vec![b], vec![a]));
            }
            let bases: Vec<ObjectId> =
                repo.merge_bases_many(a, &[b])?.into_iter().map(|id| id.detach()).collect();
            return Ok((vec![a, b], bases));
        }
        // `dotdot_missing()`. Deliberately *not* rendered here: every one of this
        // function's four callers discards the `Err` and re-reports through
        // `could_not_parse_log()` → [`super::log::bad_revision_message_in`],
        // which is the only path that also emits the `error: object … is a …`
        // notes `lookup_commit_reference()` prints ahead of the fatal. A second
        // copy of the wording built here would be a string nothing prints and no
        // test can pin — and the copy that used to stand in this spot proved the
        // point by drifting: it passed `symmetric: false` unconditionally, so it
        // called an `<a>...<b>` an `Invalid revision range` where git says
        // `Invalid symmetric difference expression`, for years, invisibly. What
        // the `Err` carries is only what a `Result` must carry.
        crate::objname::Dotdot::Missing { .. } => {
            return Err(anyhow!("{spec}: endpoints resolve but their objects are missing"));
        }
        // `handle_dotdot()` returned non-zero, so the token is not a range at
        // all and the spellings below get their turn — exactly as
        // `handle_revision_arg_1()` falls through.
        crate::objname::Dotdot::NotARange => {}
    }

    // No literal `..`/`...`: defer to rev-parse, which recognises `^!`, `^@`,
    // `^<rev>` and plain committishes. The parents of a `^!`/`^@` spec are read
    // straight off the named commit.
    use gix::revision::plumbing::Spec;
    let parents_of = |id: ObjectId| -> Result<Vec<ObjectId>> {
        let commit = repo.find_object(id)?.try_into_commit()?;
        Ok(commit.parent_ids().map(|p| p.detach()).collect())
    };
    // The single `get_oid_basic()` call this operand reaches, which is the other
    // place the ambiguity warning is due. It is asked of
    // [`crate::objname`] rather than of rev-parse, because rev-parse resolves
    // through the object database and so never sees the full-hex branch that
    // warns; the helper peels the `~<n>`/`^<n>`/`^{…}`/`:<path>` suffixes the way
    // `get_oid_1()` does before deciding.
    // The mark comes off first: `handle_revision_arg_1()` advances past a leading
    // `^` before `get_oid_with_context()` sees the name, so `get_oid_basic()`
    // measures `<40-hex>` rather than `^<40-hex>` and takes its full-hex branch.
    // `repo_get_oid()` performs no such strip, which is why
    // [`crate::objname::ambiguity_base`] does not either.
    crate::objname::warn_ambiguous_refname(repo, crate::objname::uninteresting_mark(spec).0);
    // …and the *resolution* that branch performs, which `rev_parse` does not
    // have: a name of exactly `hexsz` hex digits **is** the object id, decoded
    // without the object database or the ref store being consulted. gitoxide
    // looks the id up and, when the repository does not have it, falls back to a
    // ref of the same name — so in a repository holding `refs/heads/<40-hex>`
    // this walked that ref's history where git reports `bad object`. The mark is
    // split off first because `handle_revision_arg_1()` advances past it before
    // resolving; a `^@`/`^!`/`^-<n>` spelling is longer than `hexsz` and so falls
    // through to `rev_parse` untouched, which is where `add_parents_only()`'s
    // forms are handled.
    let (bare, negative) = crate::objname::uninteresting_mark(spec);
    if let Some(id) = crate::objname::full_hex(repo, bare) {
        // `get_reference()`: `parse_object()` or `die("bad object %s", name)`.
        // The caller renders that fatal through
        // [`super::log::early_revision_fatal`], so only the failure matters here.
        repo.find_object(id).map_err(|_| anyhow!("bad object {bare}"))?;
        return Ok(if negative { (vec![], vec![id]) } else { (vec![id], vec![]) });
    }
    let parsed = repo.rev_parse(spec).map_err(|e| anyhow!("{spec}: {e}"))?;
    match parsed.detach() {
        Spec::Include(id) => Ok((vec![id], vec![])),
        Spec::Exclude(id) => Ok((vec![], vec![id])),
        Spec::Range { from, to } => Ok((vec![to], vec![from])),
        Spec::Merge { theirs, ours } => {
            let bases: Vec<ObjectId> = repo
                .merge_bases_many(theirs, &[ours])?
                .into_iter()
                .map(|id| id.detach())
                .collect();
            Ok((vec![theirs, ours], bases))
        }
        Spec::ExcludeParents(id) => Ok((vec![id], parents_of(id)?)),
        Spec::IncludeOnlyParents(id) => Ok((parents_of(id)?, vec![])),
    }
}

// ---------------------------------------------------------------------------
// read_patches()
// ---------------------------------------------------------------------------

/// Render every non-merge commit of a range into its canonical patch text.
///
/// The range is taken already split into its endpoints, because upstream
/// resolves both ranges up front and reports an unresolvable one as a `git log`
/// failure rather than as a patch-rendering failure.
fn read_patches(
    repo: &gix::Repository,
    (tips, hidden): (Vec<ObjectId>, Vec<ObjectId>),
    mailmap: &gix::mailmap::Snapshot,
    matcher: Option<&PathMatcher>,
    abbrev: &Abbrev,
    notes: &[super::notes::Tree],
) -> Result<Vec<Patch>> {
    let ids = ordered_commits(repo, tips, hidden)?;
    let mut out = Vec::with_capacity(ids.len());
    // With a pathspec, a commit that touches no matching path is dropped
    // entirely (`git log -- <path>` never lists it), so the position numbers
    // upstream prints — `util->i` — count only surviving commits. Assign the
    // index as patches are kept, not from the pre-filter walk position.
    let mut index = 0usize;
    for id in ids {
        if let Some(patch) = build_patch(repo, id, index, mailmap, matcher, abbrev, notes)? {
            out.push(patch);
            index += 1;
        }
    }
    Ok(out)
}

/// `--no-merges --reverse --date-order`: the commits of the range, oldest first,
/// merges dropped.
///
/// `--date-order` is topological order with a newest-commit-date-first
/// tie-break; this is Kahn's algorithm over the in-range child counts, which is
/// what `sort_in_topological_order()` runs.
fn ordered_commits(
    repo: &gix::Repository,
    tips: Vec<ObjectId>,
    hidden: Vec<ObjectId>,
) -> Result<Vec<ObjectId>> {
    // `sort_in_topological_order()` is handed `revs->commits`, which
    // `prepare_revision_walk()` has already sorted newest-commit-date-first, and
    // the list index is what breaks a date tie in the ready set below. So the
    // traversal has to be git's commit-date order, not gitoxide's default
    // breadth-first — otherwise two commits sharing a second come out in graph
    // order instead.
    let mut walk = repo.rev_walk(tips).sorting(gix::revision::walk::Sorting::ByCommitTime(
        gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
    ));
    if !hidden.is_empty() {
        walk = walk.with_hidden(hidden);
    }

    // git's rule that `UNINTERESTING` propagates to every ancestor of a negative
    // endpoint and beats a positive mention of the same commit — which is what makes
    // `<a>..<b> <c>` with `<c>` an ancestor of `<a>` add nothing — is enforced by the
    // traversal itself: `gix-traverse`'s hidden frontier paints those commits and
    // `Simple` drops them, tips included. Nothing is subtracted here.

    // The membership of the range, with parents and commit times.
    let mut order: Vec<ObjectId> = Vec::new();
    let mut parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut times: HashMap<ObjectId, i64> = HashMap::new();
    for info in walk.all()? {
        let id = info?.id;
        let commit = repo.find_object(id)?.try_into_commit()?;
        times.insert(id, commit.time()?.seconds);
        parents.insert(id, commit.parent_ids().map(|p| p.detach()).collect());
        order.push(id);
    }

    // Child counts restricted to the range; upstream's `indegree` is 1-based.
    let mut indegree: HashMap<ObjectId, usize> = order.iter().map(|id| (*id, 1usize)).collect();
    for ps in parents.values() {
        for p in ps {
            if let Some(d) = indegree.get_mut(p) {
                *d += 1;
            }
        }
    }
    let seq: HashMap<ObjectId, usize> = order.iter().enumerate().map(|(n, id)| (*id, n)).collect();

    // Ready set: no children left inside the range. Newest commit date wins,
    // ties fall back to the (deterministic) traversal position.
    let mut ready: BinaryHeap<(i64, std::cmp::Reverse<usize>, ObjectId)> = order
        .iter()
        .filter(|id| indegree[*id] == 1)
        .map(|id| (times[id], std::cmp::Reverse(seq[id]), *id))
        .collect();

    let mut newest_first: Vec<ObjectId> = Vec::with_capacity(order.len());
    while let Some((_, _, id)) = ready.pop() {
        newest_first.push(id);
        for p in parents.get(&id).into_iter().flatten() {
            if let Some(d) = indegree.get_mut(p) {
                *d -= 1;
                if *d == 1 {
                    ready.push((times[p], std::cmp::Reverse(seq[p]), *p));
                }
            }
        }
    }

    newest_first.reverse();
    Ok(newest_first
        .into_iter()
        .filter(|id| parents[id].len() < 2)
        .collect())
}

/// Build the canonical patch text of one commit, or `None` when a pathspec is
/// in force and the commit touches no matching path — the case `git log -- …`
/// omits from the range entirely.
#[allow(clippy::too_many_arguments)]
fn build_patch(
    repo: &gix::Repository,
    id: ObjectId,
    index: usize,
    mailmap: &gix::mailmap::Snapshot,
    matcher: Option<&PathMatcher>,
    abbrev: &Abbrev,
    notes: &[super::notes::Tree],
) -> Result<Option<Patch>> {
    let commit = repo.find_object(id)?.try_into_commit()?;

    // ` ## Metadata ##` — only the `Author:` line of `--pretty=medium` survives
    // upstream's header filter; `Date:` and `commit` are dropped.
    let mut text: Vec<u8> = Vec::new();
    let sig = commit.author()?;
    let raw_name: &[u8] = sig.name.as_ref();
    let raw_email: &[u8] = sig.email.as_ref();
    let resolved = mailmap.try_resolve(sig);
    let (name, email): (&[u8], &[u8]) = match &resolved {
        Some(s) => (s.name.as_ref(), s.email.as_ref()),
        None => (raw_name, raw_email),
    };
    text.extend_from_slice(b" ## Metadata ##\nAuthor: ");
    text.extend_from_slice(name);
    text.extend_from_slice(b" <");
    text.extend_from_slice(email);
    text.extend_from_slice(b">\n\n ## Commit message ##\n");

    let raw = commit.message_raw()?;
    for line in message_lines(raw) {
        // `pp_remainder()` writes a 4-space indent which `read_patches()` keeps,
        // then right-trims — so a blank message line collapses to nothing.
        if !line.is_empty() {
            text.extend_from_slice(b"    ");
            text.extend_from_slice(&line);
        }
        text.push(b'\n');
    }

    // The notes blocks — upstream generates each patch with `git log`'s notes
    // on, so a note becomes part of the compared text. `read_patches()` rewrites
    // the header line of each block (range-diff.c:181-186): any in-header line
    // that starts with `Notes` and ends with `:` becomes `\n\n ## <that line
    // without its colon> ##\n`, which is what turns `Notes (alt):` into
    // ` ## Notes (alt) ##`. Every other in-header line is kept only if it
    // carries `git log`'s four-space indent, right-trimmed (range-diff.c:187-192).
    if !notes.is_empty() {
        let note = super::notes::format_display(repo, notes, id, false)?;
        for line in note.split(|&b| b == b'\n') {
            if line.starts_with(b"Notes") && line.last() == Some(&b':') {
                text.extend_from_slice(b"\n\n ## ");
                text.extend_from_slice(&line[..line.len() - 1]);
                text.extend_from_slice(b" ##\n");
            } else if line.starts_with(b"    ") {
                text.extend_from_slice(trim_end_ws(line));
                text.push(b'\n');
            }
        }
    }

    // One ` ## <path> ##` section per changed file, in path order — the order
    // `diff_tree()` walks both trees in.
    let new_tree = commit.tree()?;
    let old_tree = match commit.parent_ids().next() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    let mut changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    changes.sort_by(|x, y| change_path(x).cmp(change_path(y)));

    // A pathspec keeps only the sections it matches, and a commit left with no
    // section is not part of the limited history at all.
    if let Some(matcher) = matcher {
        changes.retain(|c| matcher.matches(change_path(c)));
        if changes.is_empty() {
            return Ok(None);
        }
    }

    reject_renames(repo, old_tree.as_ref(), &new_tree, &changes, id)?;

    let mut diff_offset = 0usize;
    let mut diffsize = 0i64;
    for change in &changes {
        text.push(b'\n');
        if diff_offset == 0 {
            diff_offset = text.len();
        }
        emit_section(repo, &mut text, change, &mut diffsize)?;
    }

    Ok(Some(Patch {
        index,
        abbrev: abbrev_id(repo, id, abbrev)?,
        subject: subject_of(raw),
        text,
        diff_offset,
        diffsize,
        matching: -1,
        shown: false,
    }))
}

/// `diff.renames` is on for `git log`, so a detected rename changes both the
/// section header and the diff body. Find that case with gitoxide's tracker at
/// git's default 50% threshold and refuse, rather than silently emitting the
/// delete-plus-add rendering that rename detection would have replaced.
fn reject_renames(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: &gix::Tree<'_>,
    changes: &[ChangeDetached],
    id: ObjectId,
) -> Result<()> {
    let has_add = changes
        .iter()
        .any(|c| matches!(c, ChangeDetached::Addition { .. }));
    let has_del = changes
        .iter()
        .any(|c| matches!(c, ChangeDetached::Deletion { .. }));
    if !(has_add && has_del) {
        return Ok(());
    }
    let tracked = repo.diff_tree_to_tree(
        old_tree,
        Some(new_tree),
        gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default())),
    )?;
    if tracked
        .iter()
        .any(|c| matches!(c, ChangeDetached::Rewrite { .. }))
    {
        bail!("commit {id} contains a rename; git's diffcore-rename scoring is not ported");
    }
    Ok(())
}

/// Emit one ` ## <path> ##` section plus its rewritten hunks, tallying the
/// `diffsize` upstream accumulates one line at a time.
fn emit_section(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    change: &ChangeDetached,
    diffsize: &mut i64,
) -> Result<()> {
    let mut body: Vec<u8> = Vec::new();

    out.extend_from_slice(b" ## ");
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            out.extend_from_slice(path);
            out.extend_from_slice(b" (new)");
            let content = content_of(repo, *id, entry_mode.is_commit())?;
            emit_hunks(&mut body, path, &[], &content, true, false)?;
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            out.extend_from_slice(path);
            out.extend_from_slice(b" (deleted)");
            let content = content_of(repo, *id, entry_mode.is_commit())?;
            emit_hunks(&mut body, path, &content, &[], false, true)?;
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let path: &[u8] = location;
            out.extend_from_slice(path);
            let old_mode = previous_entry_mode.value();
            let new_mode = entry_mode.value();
            if old_mode != new_mode {
                out.extend_from_slice(
                    format!(" (mode change {old_mode:06o} => {new_mode:06o})").as_bytes(),
                );
            }
            // A pure mode change (identical content) has no hunks, like git.
            if previous_id != id {
                let old = content_of(repo, *previous_id, previous_entry_mode.is_commit())?;
                let new = content_of(repo, *id, entry_mode.is_commit())?;
                emit_hunks(&mut body, path, &old, &new, false, false)?;
            }
        }
        // Never produced: rewrite tracking is off, and `reject_renames()` has
        // already refused the commits where git would have found a rename.
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    }
    out.extend_from_slice(b" ##\n");

    *diffsize += 1 + body.iter().filter(|&&b| b == b'\n').count() as i64;
    out.extend_from_slice(&body);
    Ok(())
}

/// Render the hunks of one file with each header reduced to
/// `@@ <path>: <function>` (or a bare `@@` when there is no function context),
/// and each body line re-signed the way `read_patches()` re-signs the
/// `--output-indicator-*` markers it asked `git log` for.
///
/// `old_missing`/`new_missing` say which side is `/dev/null`; they matter only
/// for the `Binary files ... differ` labels.
fn emit_hunks(
    out: &mut Vec<u8>,
    path: &[u8],
    old: &[u8],
    new: &[u8],
    old_missing: bool,
    new_missing: bool,
) -> Result<()> {
    if is_binary(old) || is_binary(new) {
        let label = |missing: bool| {
            if missing {
                "/dev/null".to_string()
            } else {
                quote_c_style(path)
            }
        };
        out.extend_from_slice(
            format!(
                " Binary files {} and {} differ\n",
                label(old_missing),
                label(new_missing)
            )
            .as_bytes(),
        );
        return Ok(());
    }

    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let before: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();
    let writer = InnerHunks {
        out,
        before,
        path: path.to_vec(),
    };
    UnifiedDiff::new(&diff, &input, writer, ContextSize::symmetrical(3)).consume()?;
    Ok(())
}

/// Writes the inner (per-commit) hunks in the canonical patch shape.
struct InnerHunks<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving the hunk header's function context.
    before: Vec<&'a [u8]>,
    path: Vec<u8>,
}

impl InnerHunks<'_> {
    /// git's `def_ff()`, the default hunk-header function finder used when no
    /// `diff` attribute selects a userdiff driver: the nearest line above the
    /// hunk whose first byte is a letter, `_` or `$`, capped at 80 bytes and
    /// then right-trimmed.
    fn func(&self, hunk_start_0based: i64) -> Option<Vec<u8>> {
        let mut idx = hunk_start_0based - 1;
        while idx >= 0 {
            let line = self.before[idx as usize];
            match line.first() {
                Some(&first) if first.is_ascii_alphabetic() || first == b'_' || first == b'$' => {
                    let mut n = line.len().min(FUNC_BUF_SIZE);
                    while n > 0 && line[n - 1].is_ascii_whitespace() {
                        n -= 1;
                    }
                    return (n > 0).then(|| line[..n].to_vec());
                }
                _ => idx -= 1,
            }
        }
        None
    }
}

impl ConsumeHunk for InnerHunks<'_> {
    type Out = ();

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        // Upstream keeps only what follows the closing `@@` of the git hunk
        // header, prefixed with the file name — never the line numbers.
        self.out.extend_from_slice(b"@@");
        if let Some(func) = self.func(header.before_hunk_start as i64 - 1) {
            self.out.push(b' ');
            self.out.extend_from_slice(&self.path);
            self.out.extend_from_slice(b": ");
            self.out.extend_from_slice(&func);
        }
        self.out.push(b'\n');

        for &(kind, content) in lines {
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.out
                .extend_from_slice(content.strip_suffix(b"\n").unwrap_or(content));
            self.out.push(b'\n');
            if !content.ends_with(b"\n") {
                // git emits the missing newline itself, then the marker line,
                // which `read_patches()` sees as ordinary content.
                self.out
                    .extend_from_slice(b" \\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) {}
}

/// The bytes to diff: a blob from the object database, or a submodule rendered
/// the way `--submodule=short` renders it.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// git's `buffer_is_binary()`: a NUL byte within the first 8000 bytes.
fn is_binary(content: &[u8]) -> bool {
    content.iter().take(FIRST_FEW_BYTES).any(|&b| b == 0)
}

/// `quote_c_style()` under git's default `core.quotePath=true`.
fn quote_c_style(path: &[u8]) -> String {
    let needs = path
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(path).into_owned();
    }
    let mut s = String::from("\"");
    for &b in path {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            0x07 => s.push_str("\\a"),
            0x08 => s.push_str("\\b"),
            0x0c => s.push_str("\\f"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x0b => s.push_str("\\v"),
            _ if !(0x20..0x7f).contains(&b) => s.push_str(&format!("\\{b:03o}")),
            _ => s.push(b as char),
        }
    }
    s.push('"');
    s
}

fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

// ---------------------------------------------------------------------------
// Commit-message plumbing (pretty.c)
// ---------------------------------------------------------------------------

/// The message lines `pp_remainder()` prints at indent 4, each already
/// right-trimmed by `is_blank_line()`, with leading blank lines skipped by
/// `skip_blank_lines()` and trailing ones removed by the final `strbuf_rtrim()`.
fn message_lines(msg: &BStr) -> Vec<Vec<u8>> {
    let bytes: &[u8] = msg;
    let mut lines: Vec<Vec<u8>> = bytes
        .split(|&b| b == b'\n')
        .map(|l| trim_end_ws(l).to_vec())
        .collect();
    // Splitting a newline-terminated message yields a trailing empty element.
    if bytes.last() == Some(&b'\n') {
        lines.pop();
    }
    let first_content = lines
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(lines.len());
    lines.drain(..first_content);
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// `pp_commit_easy(CMIT_FMT_ONELINE, ...)`: `format_subject()` with a single
/// space separator, i.e. the first paragraph folded onto one line.
fn subject_of(msg: &BStr) -> Vec<u8> {
    let mut title: Vec<u8> = Vec::new();
    for line in message_lines(msg) {
        if line.is_empty() {
            break;
        }
        if !title.is_empty() {
            title.push(b' ');
        }
        title.extend_from_slice(&line);
    }
    title
}

/// Strip trailing whitespace of git's `isspace` set.
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

// ---------------------------------------------------------------------------
// find_exact_matches() / get_correspondences() / linear-assignment.c
// ---------------------------------------------------------------------------

/// Pair off byte-identical diffs. Upstream's hashmap chains are LIFO, so when
/// the left range holds duplicates the highest index is matched first.
fn find_exact_matches(a: &mut [Patch], b: &mut [Patch]) {
    let mut map: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (i, p) in a.iter().enumerate() {
        map.entry(p.diff()).or_default().push(i);
    }
    // Collected first so the shared borrow of `a` ends before it is mutated.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (j, p) in b.iter().enumerate() {
        if let Some(i) = map.get_mut(p.diff()).and_then(Vec::pop) {
            pairs.push((i, j));
        }
    }
    drop(map);
    for (i, j) in pairs {
        a[i].matching = j as i64;
        b[j].matching = i as i64;
    }
}

/// Upstream's `diffsize()`: hunk count plus line count of the diff-of-diffs at
/// three context lines, with plain xdiff settings — note that `xpparam_t pp` is
/// zeroed there, so unlike every other diff in git the indent heuristic is off.
fn diffsize(a: &[u8], b: &[u8]) -> i64 {
    let input = InternedInput::new(a, b);
    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);
    let counter = LineCounter { count: 0 };
    UnifiedDiff::new(&diff, &input, counter, ContextSize::symmetrical(3))
        .consume()
        .unwrap_or(COST_MAX)
}

/// Counts one per hunk header plus one per emitted line.
struct LineCounter {
    count: i64,
}

impl ConsumeHunk for LineCounter {
    type Out = i64;

    fn consume_hunk(
        &mut self,
        _header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.count += 1 + lines.len() as i64;
        Ok(())
    }

    fn finish(self) -> i64 {
        self.count
    }
}

/// `a_util->diffsize * creation_factor / 100` (range-diff.c:391, 401), the price
/// of leaving a patch unmatched.
///
/// Upstream's `diffsize`, `creation_factor` and the whole `cost` matrix are
/// `int`, so the multiply is a 32-bit one and a large `--creation-factor` wraps
/// — `--creation-factor=2147483647` against a diffsize of 9 does not make
/// creation prohibitively expensive, it makes it *cheap*, and every patch is
/// reported as a deletion plus a creation. This port carries the costs as `i64`
/// (the matrix is a `Vec`, so there is no overflow to fear elsewhere), which is
/// why the truncation has to be reapplied here to keep the pairing identical.
/// `--creation-factor` is range-checked to `int` at parse time, and `diffsize`
/// is capped at `COST_MAX`, so only the product can leave the type.
fn creation_cost(diffsize: i64, creation_factor: i64) -> i64 {
    i64::from((diffsize as i32).wrapping_mul(creation_factor as i32) / 100)
}

/// Build and solve the cost matrix, recording the resulting correspondences.
///
/// `Err` is upstream's `die()` when the matrix would outgrow `max_memory`
/// (range-diff.c:335-344) — raised *before* any pairing is computed, so it
/// precedes the output even when one of the two ranges is empty.
fn get_correspondences(
    a: &mut [Patch],
    b: &mut [Patch],
    creation_factor: i64,
    max_memory: u64,
) -> std::result::Result<(), String> {
    let n = a.len() + b.len();
    // `st_mult(sizeof(int), st_mult(n, n))`, compared with `>=` so a budget
    // equal to the requirement is already too small.
    let cost_bytes = COST_ELEMENT_SIZE * (n as u64) * (n as u64);
    if cost_bytes >= max_memory {
        return Err(format!(
            "range-diff: unable to compute the range-diff, since it exceeds the \
             maximum memory for the cost matrix: {} ({cost_bytes} bytes) needed, \
             limited to {} ({max_memory} bytes)",
            humanise(cost_bytes),
            humanise(max_memory)
        ));
    }
    if n == 0 {
        return Ok(());
    }
    let mut cost = vec![0i64; n * n];

    for i in 0..a.len() {
        for j in 0..b.len() {
            cost[i + n * j] = if a[i].matching == j as i64 {
                0
            } else if a[i].matching < 0 && b[j].matching < 0 {
                diffsize(a[i].diff(), b[j].diff())
            } else {
                COST_MAX
            };
        }
        let c = if a[i].matching < 0 {
            creation_cost(a[i].diffsize, creation_factor)
        } else {
            COST_MAX
        };
        for j in b.len()..n {
            cost[i + n * j] = c;
        }
    }

    for j in 0..b.len() {
        let c = if b[j].matching < 0 {
            creation_cost(b[j].diffsize, creation_factor)
        } else {
            COST_MAX
        };
        for i in a.len()..n {
            cost[i + n * j] = c;
        }
    }

    for i in a.len()..n {
        for j in b.len()..n {
            cost[i + n * j] = 0;
        }
    }

    let mut a2b = vec![-1i64; n];
    let mut b2a = vec![-1i64; n];
    compute_assignment(n, n, &cost, &mut a2b, &mut b2a);

    for i in 0..a.len() {
        let j = a2b[i];
        if j >= 0 && (j as usize) < b.len() {
            a[i].matching = j;
            b[j as usize].matching = i as i64;
        }
    }
    Ok(())
}

/// `strbuf_humanise_bytes()` (`strbuf.c`), the `%s` of the `--max-memory` fatal:
/// git's truncating fraction arithmetic and its `>` (not `>=`) unit boundaries,
/// so `1048576` renders as `1024.00 KiB` and `1` as `1 byte`.
//
// NOTE: `index_pack.rs` carries a byte-identical private copy for its
// `--max-input-size` fatal. Hoisting one of them into a shared module is the
// right cleanup, but that is an edit outside this file.
fn humanise(bytes: u64) -> String {
    if bytes > 1 << 30 {
        format!(
            "{}.{:02} GiB",
            bytes >> 30,
            (bytes & ((1 << 30) - 1)) / 10_737_419
        )
    } else if bytes > 1 << 20 {
        let x = bytes + 5243; // git's rounding nudge
        format!("{}.{:02} MiB", x >> 20, ((x & ((1 << 20) - 1)) * 100) >> 20)
    } else if bytes > 1 << 10 {
        let x = bytes + 5;
        format!("{}.{:02} KiB", x >> 10, ((x & ((1 << 10) - 1)) * 100) >> 10)
    } else if bytes == 1 {
        "1 byte".to_string()
    } else {
        format!("{bytes} bytes")
    }
}

/// A port of `linear-assignment.c` — Jonker & Volgenant's shortest augmenting
/// path algorithm for the dense linear assignment problem.
///
/// `cost[column + column_count * row]` is the cost of assigning `column` to
/// `row`. `column2row` and `row2column` receive the assignment, `-1` where a
/// node stays unassigned. The control flow (including the two-phase augmenting
/// row reduction that re-queues in place, and the `goto update` that leaves `j`
/// holding the column the preceding scan left behind) is transcribed as-is.
fn compute_assignment(
    column_count: usize,
    row_count: usize,
    cost: &[i64],
    column2row: &mut [i64],
    row2column: &mut [i64],
) {
    let at = |column: usize, row: usize| cost[column + column_count * row];

    if column_count < 2 {
        column2row[..column_count].fill(0);
        row2column[..row_count].fill(0);
        return;
    }

    column2row[..column_count].fill(-1);
    row2column[..row_count].fill(-1);
    let mut v = vec![0i64; column_count];

    // Column reduction.
    for j in (0..column_count).rev() {
        let mut i1 = 0usize;
        for i in 1..row_count {
            if at(j, i1) > at(j, i) {
                i1 = i;
            }
        }
        v[j] = at(j, i1);
        if row2column[i1] == -1 {
            row2column[i1] = j as i64;
            column2row[j] = i1 as i64;
        } else {
            if row2column[i1] >= 0 {
                row2column[i1] = -2 - row2column[i1];
            }
            column2row[j] = -1;
        }
    }

    // Reduction transfer. `free_row` doubles as the work queue below, exactly as
    // upstream reuses the one allocation.
    let mut free_row = vec![0usize; row_count];
    let mut free_count = 0usize;
    // `i` is stored into free_row and used to mutate row2column[i]; not a plain slice read.
    #[allow(clippy::needless_range_loop)]
    for i in 0..row_count {
        let j1 = row2column[i];
        if j1 == -1 {
            free_row[free_count] = i;
            free_count += 1;
        } else if j1 < -1 {
            row2column[i] = -2 - j1;
        } else {
            let j1 = j1 as usize;
            // C's `!j1`: column 1 when j1 is 0, column 0 otherwise.
            let other = usize::from(j1 == 0);
            let mut min = at(other, i) - v[other];
            // `j` is passed to at(j, i) and compared to j1; not a plain index.
            #[allow(clippy::needless_range_loop)]
            for j in 1..column_count {
                if j != j1 && min > at(j, i) - v[j] {
                    min = at(j, i) - v[j];
                }
            }
            v[j1] -= min;
        }
    }

    let expected_free = row_count.saturating_sub(column_count);
    if free_count == expected_free {
        return;
    }

    // Augmenting row reduction, two phases.
    for _phase in 0..2 {
        let mut k = 0usize;
        let saved_free_count = free_count;
        free_count = 0;
        while k < saved_free_count {
            let i = free_row[k];
            k += 1;

            let mut j1 = 0usize;
            let mut u1 = at(j1, i) - v[j1];
            let mut j2: i64 = -1;
            let mut u2 = i64::MAX;
            // `j` is passed to at(j, i) and stored into j1/j2; not a plain index.
            #[allow(clippy::needless_range_loop)]
            for j in 1..column_count {
                let c = at(j, i) - v[j];
                if u2 > c {
                    if u1 < c {
                        u2 = c;
                        j2 = j as i64;
                    } else {
                        u2 = u1;
                        u1 = c;
                        j2 = j1 as i64;
                        j1 = j;
                    }
                }
            }
            if j2 < 0 {
                j2 = j1 as i64;
                u2 = u1;
            }

            let mut i0 = column2row[j1];
            if u1 < u2 {
                v[j1] -= u2 - u1;
            } else if i0 >= 0 {
                j1 = j2 as usize;
                i0 = column2row[j1];
            }

            if i0 >= 0 {
                if u1 < u2 {
                    k -= 1;
                    free_row[k] = i0 as usize;
                } else {
                    free_row[free_count] = i0 as usize;
                    free_count += 1;
                }
            }
            row2column[i] = j1 as i64;
            column2row[j1] = i as i64;
        }
    }

    // Augmentation.
    let saved_free_count = free_count;
    let mut d = vec![0i64; column_count];
    let mut pred = vec![0usize; column_count];
    let mut col: Vec<usize> = vec![0; column_count];
    for &i1 in &free_row[..saved_free_count] {
        let mut low = 0usize;
        let mut up = 0usize;
        let mut last;
        let mut min;
        let mut j: i64 = -1;

        for jj in 0..column_count {
            d[jj] = at(jj, i1) - v[jj];
            pred[jj] = i1;
            col[jj] = jj;
        }

        // `do { ... } while (low == up)` with two `goto update` exits.
        loop {
            last = low;
            min = d[col[up]];
            up += 1;
            // `up` is deliberately advanced inside the scan; the range is snapshotted
            // once, mirroring the C `for (k = up; ...)` bookkeeping.
            #[allow(clippy::mut_range_bound)]
            for k in up..column_count {
                j = col[k] as i64;
                let c = d[j as usize];
                if c <= min {
                    if c < min {
                        up = low;
                        min = c;
                    }
                    col[k] = col[up];
                    col[up] = j as usize;
                    up += 1;
                }
            }
            // Upstream jumps to `update` here without touching `j`, so the
            // augmenting path starts from whatever column the scan above left.
            if (low..up).any(|k| column2row[col[k]] == -1) {
                break;
            }

            // Scan a row: `do { ... } while (low != up)`.
            let mut jumped = false;
            loop {
                let j1 = col[low];
                low += 1;
                let i = column2row[j1] as usize;
                let u1 = at(j1, i) - v[j1] - min;
                // `up` is deliberately advanced inside the scan; the range is snapshotted
                // once, mirroring the C `for (k = up; ...)` bookkeeping.
                #[allow(clippy::mut_range_bound)]
                for k in up..column_count {
                    j = col[k] as i64;
                    let c = at(j as usize, i) - v[j as usize] - u1;
                    if c < d[j as usize] {
                        d[j as usize] = c;
                        pred[j as usize] = i;
                        if c == min {
                            if column2row[j as usize] == -1 {
                                jumped = true;
                                break;
                            }
                            col[k] = col[up];
                            col[up] = j as usize;
                            up += 1;
                        }
                    }
                }
                if jumped || low == up {
                    break;
                }
            }
            if jumped || low != up {
                break;
            }
        }

        // Updating of the column pieces.
        for &j1 in &col[..last] {
            v[j1] += d[j1] - min;
        }

        // Augmentation. Upstream `BUG()`s on a negative `j`; there is nothing
        // sensible to do here either, so leave the assignment untouched.
        if j < 0 {
            continue;
        }
        loop {
            let i = pred[j as usize];
            column2row[j as usize] = i as i64;
            std::mem::swap(&mut j, &mut row2column[i]);
            if i1 == i {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// output()
// ---------------------------------------------------------------------------

/// Walk both ranges in the order of the right-hand side, placing each left-hand
/// commit that has no counterpart once all of its predecessors have been shown.
fn output(out: &mut Vec<u8>, a: &mut [Patch], b: &[Patch], opts: &Opts) -> Result<()> {
    let patch_no_width = decimal_width(1 + a.len().max(b.len()) as u64);
    let mut dashes: Option<String> = None;
    let mut i = 0usize;
    let mut j = 0usize;

    while i < a.len() || j < b.len() {
        // Skip all the already-shown commits from the LHS.
        while i < a.len() && a[i].shown {
            i += 1;
        }

        // Show an unmatched LHS commit whose predecessors were shown.
        if i < a.len() && a[i].matching < 0 {
            if !opts.right_only {
                pair_header(out, patch_no_width, &mut dashes, Some(&a[i]), None, &opts.colors)?;
            }
            i += 1;
            continue;
        }

        // Show unmatched RHS commits.
        while j < b.len() && b[j].matching < 0 {
            if !opts.left_only {
                pair_header(out, patch_no_width, &mut dashes, None, Some(&b[j]), &opts.colors)?;
            }
            j += 1;
        }

        // Show a matching LHS/RHS pair. `-s`/`--no-patch` keeps the header but
        // drops the diff-of-diffs body (`DIFF_FORMAT_NO_OUTPUT`).
        if j < b.len() {
            let ai = b[j].matching as usize;
            pair_header(out, patch_no_width, &mut dashes, Some(&a[ai]), Some(&b[j]), &opts.colors)?;
            if !opts.no_patch {
                patch_diff(out, &a[ai].text, &b[j].text, opts)?;
            }
            a[ai].shown = true;
            j += 1;
        }
    }
    Ok(())
}

/// `output_pair_header()` (range-diff.c:399): the two index/abbreviation columns, the
/// status character and the one-line subject.
///
/// The whole line is wrapped in one color — red for a dropped commit, green for a new
/// one, commit-yellow for a matched pair. A `!` pair is the exception: it opens in red
/// (the left side), and resets to re-open in yellow, green and yellow again so the
/// status character, the right-hand column and the subject each carry their own. With
/// color off every one of those strings is empty and the line reduces to plain text.
fn pair_header(
    out: &mut Vec<u8>,
    width: usize,
    dashes: &mut Option<String>,
    a: Option<&Patch>,
    b: Option<&Patch>,
    colors: &diff_color::DiffColors,
) -> Result<()> {
    let anchor = a.or(b).expect("at least one side is present");
    if dashes.is_none() {
        *dashes = Some("-".repeat(anchor.abbrev.len()));
    }
    let dashes: &str = dashes.as_deref().expect("set just above");

    let status = match (a, b) {
        (Some(_), None) => b'<',
        (None, Some(_)) => b'>',
        (Some(x), Some(y)) if x.text != y.text => b'!',
        _ => b'=',
    };
    let reset = colors.reset();
    let color_old = colors.get(diff_color::DiffSlot::Old);
    let color_new = colors.get(diff_color::DiffSlot::New);
    let color = match status {
        b'<' => color_old,
        b'>' => color_new,
        _ => colors.get(diff_color::DiffSlot::Commit),
    };
    let split = status == b'!';

    let mut line: Vec<u8> = Vec::new();
    line.extend_from_slice(if split { color_old } else { color }.as_bytes());
    match a {
        Some(p) => line.extend_from_slice(
            format!("{:>width$}:  {} ", p.index + 1, p.abbrev, width = width).as_bytes(),
        ),
        None => {
            line.extend_from_slice(format!("{:>width$}:  {dashes} ", "-", width = width).as_bytes())
        }
    }
    if split {
        line.extend_from_slice(reset.as_bytes());
        line.extend_from_slice(color.as_bytes());
    }
    line.push(status);
    if split {
        line.extend_from_slice(reset.as_bytes());
        line.extend_from_slice(color_new.as_bytes());
    }
    match b {
        Some(p) => line.extend_from_slice(
            format!(" {:>width$}:  {}", p.index + 1, p.abbrev, width = width).as_bytes(),
        ),
        None => {
            line.extend_from_slice(format!(" {:>width$}:  {dashes}", "-", width = width).as_bytes())
        }
    }
    if split {
        line.extend_from_slice(reset.as_bytes());
        line.extend_from_slice(color.as_bytes());
    }
    line.push(b' ');
    line.extend_from_slice(&anchor.subject);
    line.extend_from_slice(reset.as_bytes());
    line.push(b'\n');
    out.extend_from_slice(&line);
    Ok(())
}

/// `decimal_width()` from pager.c.
fn decimal_width(mut number: u64) -> usize {
    let mut width = 1;
    while number >= 10 {
        number /= 10;
        width += 1;
    }
    width
}

/// The diff-of-diffs: four-space indented, no file headers, and a hunk header
/// of `@@` plus the section name the `section_headers` driver finds.
///
/// Unlike [`diffsize`], this is the diff the user's `diff_options` configure —
/// `--diff-algorithm`, `--no-indent-heuristic`, `-U<n>` and the three
/// `--output-indicator-*` markers all land here and nowhere else.
fn patch_diff(out: &mut Vec<u8>, a: &[u8], b: &[u8], opts: &Opts) -> Result<()> {
    let input = InternedInput::new(a, b);
    let diff = match opts.indent_heuristic {
        true => diff_with_slider_heuristics(opts.algorithm, &input),
        false => {
            let mut d = Diff::compute(opts.algorithm, &input);
            d.postprocess_no_heuristic(&input);
            d
        }
    };
    let before: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();

    let writer = OuterHunks {
        out,
        before,
        indicators: opts.indicators,
        func_line: Vec::new(),
        funclineprev: -1,
    };
    UnifiedDiff::new(
        &diff,
        &input,
        writer,
        ContextSize::symmetrical(opts.context),
    )
    .consume()?;
    Ok(())
}

/// Writes the outer hunks, carrying `func_line` and `funclineprev` across hunks
/// the way `xdl_emit_diff()` does.
struct OuterHunks<'a> {
    out: &'a mut Vec<u8>,
    before: Vec<&'a [u8]>,
    /// `o->output_indicators`, indexed by [`IND_NEW`] / [`IND_OLD`] /
    /// [`IND_CONTEXT`].
    indicators: [u8; 3],
    /// Deliberately *not* reset per hunk: `get_func_line()` only overwrites its
    /// buffer on a match, so a hunk with no match repeats the previous name.
    func_line: Vec<u8>,
    /// The `s1 - 1` of the previous hunk, the exclusive limit of the search.
    funclineprev: i64,
}

impl ConsumeHunk for OuterHunks<'_> {
    type Out = ();

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let s1 = header.before_hunk_start as i64 - 1;
        if let Some(f) = get_func_line(&self.before, s1 - 1, self.funclineprev) {
            self.func_line = f;
        }
        self.funclineprev = s1 - 1;

        self.out.extend_from_slice(INDENT);
        self.out.extend_from_slice(b"@@");
        if !self.func_line.is_empty() {
            self.out.push(b' ');
            self.out.extend_from_slice(&self.func_line);
        }
        self.out.push(b'\n');

        // `emit_line_0()` writes the prefix, the sign, then the record verbatim
        // — the patch text always ends its lines, so nothing is appended. A NUL
        // sign (the empty `--output-indicator-*` value) writes no column at all:
        // `if (first) fputc(first, file)` (diff.c:786-787).
        for &(kind, content) in lines {
            self.out.extend_from_slice(INDENT);
            let sign = self.indicators[match kind {
                DiffLineKind::Context => IND_CONTEXT,
                DiffLineKind::Add => IND_NEW,
                DiffLineKind::Remove => IND_OLD,
            }];
            if sign != 0 {
                self.out.push(sign);
            }
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
            }
        }
        Ok(())
    }

    fn finish(self) {}
}

/// `get_func_line()`: scan `records` from `start` towards `limit` (exclusive)
/// for the first line the section-header driver matches.
fn get_func_line(records: &[&[u8]], start: i64, limit: i64) -> Option<Vec<u8>> {
    let step: i64 = if start > limit { -1 } else { 1 };
    let mut l = start;
    while l != limit && 0 <= l && (l as usize) < records.len() {
        if let Some(f) = section_name(records[l as usize]) {
            return Some(f);
        }
        l += step;
    }
    None
}

/// Upstream's `section_headers` userdiff driver run through `ff_regexp()`: try
/// `^ ## (.*) ##$` then `^.?@@ (.*)$` against the record with its line
/// terminator excluded, take capture group 1, cap it at 80 bytes, then trim
/// trailing whitespace.
fn section_name(record: &[u8]) -> Option<Vec<u8>> {
    let mut len = record.len();
    if len > 0 && record[len - 1] == b'\n' {
        if len > 1 && record[len - 2] == b'\r' {
            len -= 2;
        } else {
            len -= 1;
        }
    }
    let line = &record[..len];

    let group = match_section(line).or_else(|| match_hunk(line))?;
    let mut n = group.len().min(FUNC_BUF_SIZE);
    while n > 0 && group[n - 1].is_ascii_whitespace() {
        n -= 1;
    }
    Some(group[..n].to_vec())
}

/// `^ ## (.*) ##$`. `.*` is greedy and `$` anchors, so the group runs from just
/// after the opening ` ## ` to just before the final ` ##`.
fn match_section(line: &[u8]) -> Option<&[u8]> {
    (line.len() >= 7 && line.starts_with(b" ## ") && line.ends_with(b" ##"))
        .then(|| &line[4..line.len() - 3])
}

/// `^.?@@ (.*)$`. The optional leading character is greedy, so a one-character
/// diff marker is consumed in preference to matching `@@ ` at offset zero.
fn match_hunk(line: &[u8]) -> Option<&[u8]> {
    if line.len() >= 4 && line[1..].starts_with(b"@@ ") {
        return Some(&line[4..]);
    }
    if line.starts_with(b"@@ ") {
        return Some(&line[3..]);
    }
    None
}
