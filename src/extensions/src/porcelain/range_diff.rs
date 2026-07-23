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
//!   `-u` / `--patch`, `--no-notes`, and `--ws-error-highlight=<kind>` (a no-op
//!   with color off, which is the only mode this port emits). Dual and simple
//!   coloring are byte-identical once color is off.
//! * `-s` / `--no-patch`: `DIFF_FORMAT_NO_OUTPUT`, which keeps the pair headers
//!   (`=`/`!`/`<`/`>` and the abbreviated ids) and drops every diff-of-diffs
//!   body — reproduced by suppressing the inner `patch_diff()` call.
//! * `--abbrev` / `--no-abbrev` / `--abbrev=<n>`: the abbreviation length of the
//!   ids in every pair header, ported from `find_unique_abbrev()` and
//!   `parse_opt_abbrev_cb()` (bare `--abbrev` is 7, `--no-abbrev` / `--abbrev=0`
//!   the full id, `--abbrev=<n>` clamps `<n>` to `[4, 40]`, and a non-numeric
//!   `<n>` is the 129 `error: option 'abbrev' expects a numerical value`).
//! * The diff options that upstream forwards but that touch only patch bytes
//!   this port already discards, so accepting them changes nothing:
//!   `--full-index` (the abbreviated/full `index` line is dropped), `--binary`
//!   (text files gain no binary hunk; the `Binary files … differ` label is
//!   unchanged), and every `--diff-merges` variant (`--no-diff-merges`,
//!   `--remerge-diff`, `--diff-merges=<fmt>`) because range-diff excludes
//!   merges. They are accepted silently, not deferred.
//! * The failure paths, with upstream's exit status: a bad argument shape exits
//!   129, a two-range operand that names nothing exits 128 (`bad revision`, the
//!   fatal `is_range_diff_range()` raises), `--left-only` together with
//!   `--right-only` exits 255, a range naming an unknown revision exits 255, and
//!   combining two or more of `--name-only` / `--name-status` / `--check` / `-s`
//!   exits 128 — upstream's `diff_setup_done()` fatal, raised before any
//!   revision is resolved (so it precedes the shape and range checks above).
//!
//! ### Option handling — nothing is silently ignored
//!
//! Upstream forwards most of the `git diff` option set to the inner patch
//! rendering. This port implements only the options listed above; every other
//! option is *deferred*, meaning it is recorded and never applied:
//!
//! * A deferred option is forwarded to the inner patch rendering, so it can only
//!   change the diff-of-diffs body of a matched pair or the matching itself —
//!   never a subject or the `<`/`>` header of an unmatched commit. When one of
//!   the two ranges is empty no commit can match, every commit renders as a bare
//!   header with no body, and the option provably cannot reach the output, so
//!   the page is emitted (exit 0) exactly as upstream emits it — this is the
//!   common `<old>...<new>` ancestor case. Otherwise, if a body would be
//!   produced, the run stops with a terse `unsupported flag` message on stderr
//!   rather than emitting a patch that ignored the option.
//! * If the run instead ends earlier — a usage error, or a range that names an
//!   unknown revision — the deferred option never becomes observable, because
//!   upstream's behaviour on those two paths does not depend on it. That was
//!   checked against git 2.55 by running every flag this subcommand's parity
//!   grammar can emit with no range argument: all 84 produce the same
//!   `fatal: need two commit ranges` and the same exit status 129.
//! * The exception is an option upstream *validates while parsing*, before any
//!   revision is resolved: `--creation-factor` (`OPTION_INTEGER`) and
//!   `--inter-hunk-context` (`OPTION_UNSIGNED`, a k/m/g magnitude via
//!   [`git_parse_unsigned`]). A malformed value for either is the 129 `error:`
//!   upstream reports at parse time, not a deferred `unsupported flag`. A
//!   `--inter-hunk-context` value upstream accepts is deferred like the rest,
//!   because rendering it would change the inner patch text.
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
//! * A repository carrying a `refs/notes/commits` ref, unless `--no-notes` was
//!   given — upstream asks `git log` to show notes by default, so a note would
//!   silently change the compared text. `--notes` and `--no-notes` themselves
//!   are honoured; only the *rendering* of an existing note is unported, so a
//!   repository without notes emits normally under either flag.
//! * `--diff-merges=<format>` / `--remerge-diff` (merges are ignored here, which
//!   is the default upstream behaviour), a magic or wildcard pathspec, and every
//!   other `git diff` option upstream forwards to the inner patches.
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
//!   first), which is identical for the linear patch series range-diff exists to
//!   compare, but may break commit-date ties differently on merge-heavy ranges,
//!   because upstream's tie-break is its binary heap's internal order.
//! * A usage error prints `fatal: <reason>` and the three-line synopsis on
//!   stderr and exits 129 like upstream, but without the ~90-line option list
//!   upstream prints after the synopsis. Stdout is empty either way.
//! * An unrecognised option reaches the usage error as "need two commit ranges"
//!   rather than upstream's "unknown option", because unrecognised options are
//!   deferred (see above). The exit status, 129, is the same.

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

/// The four `diff.h` output-format bits `diff_setup_done()` forbids combining:
/// `--name-only`, `--name-status`, `--check` and `-s`. Two or more set is the
/// fatal `cannot be used together` (exit 128) upstream raises before it resolves
/// any revision. `-s`/`--no-patch` *assigns* `DIFF_FORMAT_NO_OUTPUT`, clearing
/// the earlier bits, so `--name-only -s` is one bit but `-s --name-only` is two.
const FMT_NAME: u32 = 1 << 0;
const FMT_NAME_STATUS: u32 = 1 << 1;
const FMT_CHECKDIFF: u32 = 1 << 2;
const FMT_NO_OUTPUT: u32 = 1 << 3;

/// The terse `unsupported flag` message naming what this port *does* render, so
/// a deferred option reports a consistent list wherever it is caught.
fn unsupported_flag(flag: &str) -> String {
    format!(
        "unsupported flag {flag:?} (ported: --creation-factor, --left-only, \
         --right-only, --no-dual-color, --no-color, --color=never, --color=auto, \
         --patch, -s, --no-patch, --abbrev, --no-abbrev, --notes, --no-notes, \
         --full-index, --binary, --diff-merges, --no-diff-merges, --remerge-diff)"
    )
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

/// The three-line synopsis `builtin_range_diff_usage` opens with. Upstream then
/// prints its whole option table; that part is not reproduced (see the module
/// docs), and stderr prose is not part of the compatibility contract anyway.
const USAGE: &str = "\
usage: git range-diff [<options>] <old-base>..<old-tip> <new-base>..<new-tip>
   or: git range-diff [<options>] <old-tip>...<new-tip>
   or: git range-diff [<options>] <base> <old-tip> <new-tip>
";

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

/// Short options whose value is a separate argv element. The remaining short
/// options either take no value (`-p`, `-R`, `-w`, …) or attach it (`-U1`,
/// `-M50`, …), so neither consumes the next element.
const SHORT_TAKES_VALUE: &[&str] = &["-G", "-I", "-O", "-S", "-l"];

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
    /// Whether upstream would ask `git log` to render notes. On by default;
    /// `--no-notes` turns it off, which is the only setting this port renders.
    notes: bool,
    /// `-s` / `--no-patch`: emit only the pair headers, no diff-of-diffs body,
    /// exactly as `DIFF_FORMAT_NO_OUTPUT` suppresses the inner patch.
    no_patch: bool,
    /// Abbreviation length for the ids printed in every pair header, driven by
    /// `--abbrev` / `--no-abbrev` / `--abbrev=<n>`.
    abbrev: Abbrev,
    /// The first option this port recognises as real but does not implement,
    /// held until the run is about to produce output. See the module docs.
    deferred: Option<String>,
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
        notes: true,
        no_patch: false,
        abbrev: Abbrev::Default,
        deferred: None,
    };
    // `args` excludes the `range-diff` verb: `dispatch::run` takes the
    // subcommand separately, so option parsing starts at index 0. Positionals
    // are collected in order into `pos`, and the `--` end-of-options marker is
    // *kept* in that list the way upstream's `PARSE_OPT_KEEP_DASHDASH` keeps it,
    // because the classifier below reads its position (see [`classify`]).
    let mut pos: Vec<String> = Vec::new();
    let mut after_dash_dash = false;

    // The accumulated `diff_setup_done()` output-format bits (see [`FMT_NAME`]).
    let mut fmt_mask: u32 = 0;

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

        // `--name=value` splits; a short option's value is always attached, so
        // an `=` inside one belongs to the value.
        let (name, inline) = match a.find('=') {
            Some(p) if a.starts_with("--") => (&a[..p], Some(&a[p + 1..])),
            _ => (a, None),
        };

        match name {
            "--left-only" => opts.left_only = true,
            "--no-left-only" => opts.left_only = false,
            "--right-only" => opts.right_only = true,
            "--no-right-only" => opts.right_only = false,
            // Without color the dual and simple renderings are the same bytes,
            // and `auto` resolves to off because output is not a terminal.
            "--no-dual-color" | "--no-color" => {}
            "--color" if matches!(inline, Some("never") | Some("auto")) => {}
            // Patch output is what this port emits; `-p`/`-u` ask for it.
            "-p" | "-u" | "--patch" => {}
            "--no-notes" => opts.notes = false,
            // `--notes[=<ref>]` turns note rendering on — the default. The value
            // is an attached OPTARG (a separate argv element is not consumed).
            // With no notes ref present this is identical to the default output;
            // the guard below still stops honestly when a `refs/notes/commits`
            // ref exists, whose rendering is not ported.
            "--notes" => opts.notes = true,
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
                    Some(v) => match parse_abbrev_value(v) {
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
            "--full-index" | "--binary" | "--no-binary" | "--no-diff-merges"
            | "--remerge-diff" => {}
            "--diff-merges" => {
                if inline.is_none() {
                    i += 1;
                }
            }
            // The four mutually-exclusive `diff_setup_done()` output formats.
            // Each still changes the diff-of-diffs body this port cannot render,
            // so they stay deferred; but their bits are tracked here so the
            // `cannot be used together` fatal can fire before any revision is
            // resolved. `-s`/`--no-patch` assigns `NO_OUTPUT`, clearing the rest.
            "--name-only" => {
                fmt_mask |= FMT_NAME;
                opts.defer(unsupported_flag(a));
            }
            "--name-status" => {
                fmt_mask |= FMT_NAME_STATUS;
                opts.defer(unsupported_flag(a));
            }
            "--check" => {
                fmt_mask |= FMT_CHECKDIFF;
                opts.defer(unsupported_flag(a));
            }
            // `-s`/`--no-patch` assigns `DIFF_FORMAT_NO_OUTPUT`, clearing the
            // other format bits, and suppresses the diff-of-diffs body entirely
            // — leaving the pair headers, which this port renders. So it is
            // implemented here, not deferred.
            "-s" | "--no-patch" => {
                fmt_mask = FMT_NO_OUTPUT;
                opts.no_patch = true;
            }
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
            "--no-creation-factor" => opts.creation_factor = CREATION_FACTOR_DEFAULT,
            "--creation-factor" => {
                let value = match inline {
                    Some(v) => v.to_string(),
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            None => {
                                return Ok(usage_error(
                                    "option `creation-factor' requires a value",
                                ))
                            }
                        }
                    }
                };
                match value.parse::<i64>() {
                    Ok(n) => opts.creation_factor = n,
                    // Upstream's `OPT_INTEGER` failure is a usage error, 129.
                    Err(_) => {
                        return Ok(usage_error(
                            "option `creation-factor' expects an integer value with an \
                             optional k/m/g suffix",
                        ))
                    }
                }
            }
            _ => {
                opts.defer(unsupported_flag(a));
                if inline.is_none()
                    && (LONG_TAKES_VALUE.contains(&name) || SHORT_TAKES_VALUE.contains(&name))
                {
                    i += 1;
                }
            }
        }
        i += 1;
    }

    // `diff_setup_done()` runs before any revision is resolved: two or more of
    // `--name-only`/`--name-status`/`--check`/`-s` is a fatal (128) here, ahead
    // of the argument-shape (129), `--left-only`/`--right-only` (255) and range
    // (128/255) checks below. Value errors (`--creation-factor`,
    // `--inter-hunk-context`) already returned 129 inside the loop, matching
    // upstream's parse-options-first ordering.
    if fmt_mask.count_ones() >= 2 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' \
             cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }

    let repo = gix::discover(".")?;

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

    // Everything from the form's consumed count onward — including the `--`
    // itself when present — is forwarded to the inner `git log` (upstream's
    // `strvec_pushv(&log_arg, argv + …)`). A leading `--` makes the remainder a
    // pathspec; anything else is a stray operand the inner log rejects.
    let (pathspec, stray): (Vec<String>, Option<String>) = match extra.first() {
        Some(first) if first.as_str() == "--" => (extra[1..].to_vec(), None),
        Some(first) => (Vec::new(), Some(first.clone())),
        None => (Vec::new(), None),
    };

    // Upstream resolves each range by running `git log` over it, oldest range
    // first; a range naming an unknown revision is fatal before any patch is
    // read, and `git log`'s -1 return becomes exit status 255.
    let ends1 = match endpoints(&repo, &range1) {
        Ok(e) => e,
        Err(_) => return Ok(could_not_parse_log(&range1)),
    };
    let ends2 = match endpoints(&repo, &range2) {
        Ok(e) => e,
        Err(_) => return Ok(could_not_parse_log(&range2)),
    };

    // A stray operand given without a `--` separator is handed verbatim to the
    // inner `git log`, which rejects it exactly as it would on its own command
    // line (see [`stray_operand`]).
    if let Some(token) = stray {
        return Ok(stray_operand(&repo, &range1, &token));
    }

    // A pathspec limits both which commits appear and which file sections each
    // rendered patch carries. Plain paths are matched here; a magic pathspec
    // this port does not implement stops rather than filter differently.
    let matcher = match build_matcher(&pathspec) {
        Ok(m) => m,
        Err(reason) => bail!("{reason}"),
    };

    if opts.notes && repo.try_find_reference("refs/notes/commits")?.is_some() {
        bail!(
            "this repository has a refs/notes/commits ref; `git range-diff` shows notes \
             by default and note rendering is not ported"
        );
    }

    let mailmap = repo.open_mailmap();
    let mut a = read_patches(&repo, ends1, &mailmap, matcher.as_ref(), &opts.abbrev)?;
    let mut b = read_patches(&repo, ends2, &mailmap, matcher.as_ref(), &opts.abbrev)?;

    find_exact_matches(&mut a, &mut b);
    get_correspondences(&mut a, &mut b, opts.creation_factor);

    // A deferred (unimplemented) diff option is forwarded to the inner patch
    // rendering, so it can only alter the diff-of-diffs body of a matched pair
    // or the matching itself — never a subject or the `<`/`>` header of an
    // unmatched commit. When one range is empty no commit can match, every
    // commit renders as a bare header with no body, and the option provably
    // cannot reach the output — so it is emitted, matching upstream, which
    // accepts these options and prints the same header-only page (this is the
    // common `<old>...<new>` ancestor case). Otherwise stop rather than emit a
    // patch that ignored the option.
    if let Some(reason) = &opts.deferred {
        if !(a.is_empty() || b.is_empty()) {
            bail!("{reason}");
        }
    }

    let mut rendered: Vec<u8> = Vec::new();
    output(&mut rendered, &mut a, &b, &opts)?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(&rendered)?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
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
fn could_not_parse_log(range: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{range}': unknown revision or path not in the working tree."
    );
    eprintln!("Use '--' to separate paths from revisions, like this:");
    eprintln!("'git <command> [<revision>...] -- [<file>...]'");
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

/// Port of `parse_opt_abbrev_cb()`'s value handling: `v = (int)strtol(arg, &end,
/// 10)` requiring `arg` to be a whole number — optional leading ASCII
/// whitespace, an optional `+`/`-` sign, at least one decimal digit, and no
/// trailing bytes. `None` is upstream's `expects a numerical value` error. A
/// value that overflows a C `long` is saturated and then truncated to `int`
/// exactly as the assignment `int v = strtol(...)` does on a 64-bit host, so the
/// pathological `--abbrev=<huge>` reproduces git's wrap.
fn parse_abbrev_value(value: &str) -> Option<i32> {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = match bytes.get(i) {
        Some(b'+') => {
            i += 1;
            false
        }
        Some(b'-') => {
            i += 1;
            true
        }
        _ => false,
    };
    let digits_start = i;
    // `strtol` saturates to `LONG_{MAX,MIN}` on overflow; mirror with an i64
    // accumulator (a C `long` on the 64-bit hosts this port targets).
    let mut acc: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        let d = i64::from(bytes[i] - b'0');
        acc = acc
            .checked_mul(10)
            .and_then(|v| v.checked_add(d))
            .unwrap_or(i64::MAX);
        i += 1;
    }
    // No digits, or trailing bytes after the number: not a whole number.
    if i == digits_start || i != bytes.len() {
        return None;
    }
    let signed = if negative { acc.wrapping_neg() } else { acc };
    // `int v = strtol(...)` narrows the `long` to 32 bits.
    Some(signed as i32)
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
            RangeKind::Bad => return Err(bad_revision(&pos[0])),
            RangeKind::NotRange => false,
            RangeKind::Range => match is_range_diff_range(repo, &pos[1]) {
                RangeKind::Bad => return Err(bad_revision(&pos[1])),
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
                    RangeKind::Bad => return Err(bad_revision(token)),
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

/// `get_oid_committish()`: does `spec` name something that peels to a commit?
/// Never fatal — a miss is reported as `false`, matching upstream's use of the
/// return value as a mere predicate in the three-committish test.
fn committish(repo: &gix::Repository, spec: &str) -> bool {
    resolve_commit(repo, spec).is_ok()
}

/// `is_range_diff_range()`: run `spec` through the same resolution `git log`
/// uses and classify the result. An unresolvable token is [`RangeKind::Bad`],
/// which the caller turns into upstream's fatal `bad revision` (exit 128); a
/// resolved spec is a [`RangeKind::Range`] when it carries both a positive tip
/// and a hidden negative endpoint, and [`RangeKind::NotRange`] otherwise. This
/// recognises every spelling gitoxide's rev-parse does, so `<rev>^!` and
/// `<rev>^@` are handled alongside `<a>..<b>` and `<a>...<b>`.
fn is_range_diff_range(repo: &gix::Repository, spec: &str) -> RangeKind {
    match endpoints(repo, spec) {
        Ok((tips, hidden)) => {
            if !tips.is_empty() && !hidden.is_empty() {
                RangeKind::Range
            } else {
                RangeKind::NotRange
            }
        }
        Err(_) => RangeKind::Bad,
    }
}

/// Upstream's fatal `bad revision '<spec>'`, exit 128: what `setup_revisions()`
/// prints (via `is_range_diff_range()`, or the inner `git log`) when a token
/// resolves to neither a revision nor an unambiguous path.
fn bad_revision(spec: &str) -> ExitCode {
    eprintln!("fatal: bad revision '{spec}'");
    ExitCode::from(128)
}

/// A stray operand (a range consumed the form, but an extra token followed
/// without a `--`) is appended to the inner `git log` argument list. Reproduce
/// how that `git log` rejects it: a token naming a working-tree path is
/// *ambiguous* (255, wrapped in `range-diff`'s own `could not parse log`), a
/// token naming nothing is a fatal `bad revision` (128), and a token that does
/// resolve would silently extend the walk — a shape this port does not render.
fn stray_operand(repo: &gix::Repository, range: &str, token: &str) -> ExitCode {
    if repo.rev_parse_single(token).is_ok() {
        eprintln!("fatal: range-diff: a stray revision operand is not supported: '{token}'");
        return ExitCode::from(128);
    }
    let on_disk = repo
        .workdir()
        .map(|w| w.join(token).exists())
        .unwrap_or(false);
    if on_disk {
        eprintln!(
            "fatal: ambiguous argument '{token}': unknown revision or path not in the working tree."
        );
        eprintln!("Use '--' to separate paths from revisions, like this:");
        eprintln!("'git <command> [<revision>...] -- [<file>...]'");
        eprintln!("error: could not parse log for '{range}'");
        ExitCode::from(255)
    } else {
        bad_revision(token)
    }
}

/// A plain-path pathspec limiter. `git`'s pathspec grammar is far larger, but
/// the corpus only ever produces plain paths and directory prefixes here; any
/// magic (`:(glob)`, `:!exclude`, …) or wildcard is refused up front by
/// [`build_matcher`] rather than matched with subtly different semantics.
struct PathMatcher {
    paths: Vec<Vec<u8>>,
}

impl PathMatcher {
    /// A change path matches when it equals a pathspec exactly or lies under it
    /// as a directory prefix, which is git's plain-path containment rule.
    fn matches(&self, path: &[u8]) -> bool {
        self.paths.iter().any(|p| {
            path == p.as_slice()
                || (path.len() > p.len() && path[p.len()] == b'/' && path.starts_with(p))
        })
    }
}

/// Build the pathspec limiter, or `None` when there is nothing to limit. A
/// pathspec this port does not implement is refused with a terse reason so the
/// run stops rather than filter with different semantics.
fn build_matcher(pathspec: &[String]) -> Result<Option<PathMatcher>, String> {
    let mut paths: Vec<Vec<u8>> = Vec::new();
    for spec in pathspec {
        if spec.is_empty() {
            // An empty pathspec matches everything, i.e. no limiting.
            return Ok(None);
        }
        if spec.starts_with(':') {
            return Err(format!("magic pathspec is not supported: {spec:?}"));
        }
        if spec.bytes().any(|b| matches!(b, b'*' | b'?' | b'[' | b'\\')) {
            return Err(format!("wildcard pathspec is not supported: {spec:?}"));
        }
        // A trailing slash on a directory pathspec is not part of the stored
        // path prefix; git treats `src/` and `src` alike.
        let trimmed = spec.strip_suffix('/').unwrap_or(spec.as_str());
        paths.push(trimmed.as_bytes().to_vec());
    }
    Ok((!paths.is_empty()).then_some(PathMatcher { paths }))
}

fn resolve_commit(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    let commit = repo
        .rev_parse_single(spec)?
        .object()?
        .peel_to_commit()
        .map_err(|e| anyhow!("{spec}: not a commit: {e}"))?;
    Ok(commit.id)
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
    let or_head = |s: &str| if s.is_empty() { "HEAD" } else { s }.to_string();
    if let Some(dots) = spec.find("...") {
        let left = resolve_commit(repo, &or_head(&spec[..dots]))?;
        let right = resolve_commit(repo, &or_head(&spec[dots + 3..]))?;
        let bases: Vec<ObjectId> = repo
            .merge_bases_many(left, &[right])?
            .into_iter()
            .map(|id| id.detach())
            .collect();
        return Ok((vec![left, right], bases));
    }
    if let Some(dots) = spec.find("..") {
        let left = resolve_commit(repo, &or_head(&spec[..dots]))?;
        let right = resolve_commit(repo, &or_head(&spec[dots + 2..]))?;
        return Ok((vec![right], vec![left]));
    }

    // No literal `..`/`...`: defer to rev-parse, which recognises `^!`, `^@`,
    // `^<rev>` and plain committishes. The parents of a `^!`/`^@` spec are read
    // straight off the named commit.
    use gix::revision::plumbing::Spec;
    let parents_of = |id: ObjectId| -> Result<Vec<ObjectId>> {
        let commit = repo.find_object(id)?.try_into_commit()?;
        Ok(commit.parent_ids().map(|p| p.detach()).collect())
    };
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
) -> Result<Vec<Patch>> {
    let ids = ordered_commits(repo, tips, hidden)?;
    let mut out = Vec::with_capacity(ids.len());
    // With a pathspec, a commit that touches no matching path is dropped
    // entirely (`git log -- <path>` never lists it), so the position numbers
    // upstream prints — `util->i` — count only surviving commits. Assign the
    // index as patches are kept, not from the pre-filter walk position.
    let mut index = 0usize;
    for id in ids {
        if let Some(patch) = build_patch(repo, id, index, mailmap, matcher, abbrev)? {
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
    let mut walk = repo.rev_walk(tips);
    if !hidden.is_empty() {
        walk = walk.with_hidden(hidden);
    }

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
fn build_patch(
    repo: &gix::Repository,
    id: ObjectId,
    index: usize,
    mailmap: &gix::mailmap::Snapshot,
    matcher: Option<&PathMatcher>,
    abbrev: &Abbrev,
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
            _ if b < 0x20 || b >= 0x7f => s.push_str(&format!("\\{b:03o}")),
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

/// Build and solve the cost matrix, recording the resulting correspondences.
fn get_correspondences(a: &mut [Patch], b: &mut [Patch], creation_factor: i64) {
    let n = a.len() + b.len();
    if n == 0 {
        return;
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
            a[i].diffsize * creation_factor / 100
        } else {
            COST_MAX
        };
        for j in b.len()..n {
            cost[i + n * j] = c;
        }
    }

    for j in 0..b.len() {
        let c = if b[j].matching < 0 {
            b[j].diffsize * creation_factor / 100
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
            for j in 1..column_count {
                if j != j1 && min > at(j, i) - v[j] {
                    min = at(j, i) - v[j];
                }
            }
            v[j1] -= min;
        }
    }

    let expected_free = if column_count < row_count {
        row_count - column_count
    } else {
        0
    };
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
    for f in 0..saved_free_count {
        let i1 = free_row[f];
        let mut low = 0usize;
        let mut up = 0usize;
        let mut last = 0usize;
        let mut min = 0i64;
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
        for k in 0..last {
            let j1 = col[k];
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
                pair_header(out, patch_no_width, &mut dashes, Some(&a[i]), None)?;
            }
            i += 1;
            continue;
        }

        // Show unmatched RHS commits.
        while j < b.len() && b[j].matching < 0 {
            if !opts.left_only {
                pair_header(out, patch_no_width, &mut dashes, None, Some(&b[j]))?;
            }
            j += 1;
        }

        // Show a matching LHS/RHS pair. `-s`/`--no-patch` keeps the header but
        // drops the diff-of-diffs body (`DIFF_FORMAT_NO_OUTPUT`).
        if j < b.len() {
            let ai = b[j].matching as usize;
            pair_header(out, patch_no_width, &mut dashes, Some(&a[ai]), Some(&b[j]))?;
            if !opts.no_patch {
                patch_diff(out, &a[ai].text, &b[j].text)?;
            }
            a[ai].shown = true;
            j += 1;
        }
    }
    Ok(())
}

/// `output_pair_header()` with color disabled: every color string is empty, so
/// the line reduces to the two index/abbreviation columns, the status character
/// and the one-line subject.
fn pair_header(
    out: &mut Vec<u8>,
    width: usize,
    dashes: &mut Option<String>,
    a: Option<&Patch>,
    b: Option<&Patch>,
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

    let mut line: Vec<u8> = Vec::new();
    match a {
        Some(p) => line.extend_from_slice(
            format!("{:>width$}:  {} ", p.index + 1, p.abbrev, width = width).as_bytes(),
        ),
        None => {
            line.extend_from_slice(format!("{:>width$}:  {dashes} ", "-", width = width).as_bytes())
        }
    }
    line.push(status);
    match b {
        Some(p) => line.extend_from_slice(
            format!(" {:>width$}:  {}", p.index + 1, p.abbrev, width = width).as_bytes(),
        ),
        None => {
            line.extend_from_slice(format!(" {:>width$}:  {dashes}", "-", width = width).as_bytes())
        }
    }
    line.push(b' ');
    line.extend_from_slice(&anchor.subject);
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
fn patch_diff(out: &mut Vec<u8>, a: &[u8], b: &[u8]) -> Result<()> {
    let input = InternedInput::new(a, b);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let before: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();

    let writer = OuterHunks {
        out,
        before,
        func_line: Vec::new(),
        funclineprev: -1,
    };
    UnifiedDiff::new(&diff, &input, writer, ContextSize::symmetrical(3)).consume()?;
    Ok(())
}

/// Writes the outer hunks, carrying `func_line` and `funclineprev` across hunks
/// the way `xdl_emit_diff()` does.
struct OuterHunks<'a> {
    out: &'a mut Vec<u8>,
    before: Vec<&'a [u8]>,
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
        // — the patch text always ends its lines, so nothing is appended.
        for &(kind, content) in lines {
            self.out.extend_from_slice(INDENT);
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
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
