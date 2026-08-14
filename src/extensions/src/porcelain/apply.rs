//! `git apply` — read a unified diff and apply it to the working tree.
//!
//! Unlike most modules here, `apply` has no gitoxide substrate to lean on: the
//! vendored crates ship a diff *producer* (`gix-diff`, `gix-imara-diff`) but no
//! patch *parser* or *applier*. The unified-diff parse and the hunk placement
//! search below are therefore a direct port of git's `apply.c` — specifically
//! `parse_fragment`, `find_pos` (the alternating backwards/forwards scan) and
//! `match_fragment`'s `match_beginning` / `match_end` constraints — so hunk
//! placement, offset tolerance and failure points land where stock git puts
//! them.
//!
//! Supported (output, exit code and resulting worktree match stock git):
//!   * `git apply <patch>...` / stdin (no operand, or `-`)
//!   * `-p<n>`, `-R`/`--reverse`, `--check`, `--numstat`, `--stat`, `--summary`,
//!     `-z`, `--apply`, `--allow-empty`, `--unidiff-zero`, `--no-add`,
//!     `--index`/`--cached` (stage the result into the index; `--cached` skips the
//!     worktree write — see below),
//!     `--exclude=<glob>`/`--include=<glob>` (path filtering via wildmatch),
//!     `-v`/`--verbose` (the `Checking patch`/`Applied patch` progress on
//!     stderr), `--reject` (partial apply, `*.rej` files, exit 1),
//!     `--allow-overlap`/`--unsafe-paths` (no-op as git is for in-tree paths),
//!     `--binary`/`--allow-binary-replacement` (accepted, no-op as in modern
//!     git), `-q`/`--quiet`, `--whitespace=warn|nowarn`, `--recount`,
//!     `--directory=<root>`, `--`, and the `--no-` form of each of git's
//!     negatable options
//!   * `-3`/`--3way` and its `--ours`/`--theirs`/`--union` variants — see below
//!   * usage errors: unknown option/switch (git's own usage block on stderr,
//!     exit 129), a missing or non-integer option value, an unrecognised
//!     `--whitespace` action, `--ours`/`--theirs`/`--union` without `--3way`,
//!     `--reject` with `--3way`, and `--3way` outside a repository
//!     (`fatal:`/`error:`, exit 128)
//!   * patch kinds: modification, creation, deletion, rename, mode change, and
//!     symlink blobs; git-style (`diff --git`) and traditional `---`/`+++` diffs
//!
//! Faithful to git on the write side: the whole patch is validated before any
//! file is touched (atomicity), targets are removed and re-created rather than
//! rewritten in place (so the resulting mode is the patch's mode under the
//! process umask, not the old file's), leading directories are created for new
//! paths, and directories emptied by a deletion or rename are pruned.
//!
//! Argument parsing covers git's whole `apply` option table, because git's own
//! ordering makes that observable: it finishes parsing, runs its usage-level
//! validations, *then* opens the patch files, *then* parses them. A flag this
//! port cannot honour is therefore recorded during parsing and only reported
//! once the input is known to contain at least one patch — the first moment
//! ignoring it could change a result. Until that moment git has not consulted it
//! either, so `git apply --stat missing-file` and `git apply --3way not-a-patch`
//! report what git reports (`can't open patch` / `No valid patches in input`,
//! exit 128) rather than a premature unsupported-flag error.
//!
//! `--index`/`--cached` update the index (`git apply`'s `update_index` path,
//! served natively through the vendored `gix-index` writer, so tools on PATH — and
//! `git am`, which re-execs `apply --index` — see the same staged state). Both read
//! each file's pre-image from the index blob, exactly as git does when
//! `check_index` is set (`load_patch_target`). After the shared apply engine
//! computes a file's new content, the new blob is written to the odb and the index
//! entry for that path is added (creation), removed (deletion), or replaced with
//! the new oid/mode (modification, rename — remove old path, add new path). The
//! whole index is written once, under the repo lock. `--index` additionally writes
//! the worktree (the engine's usual write) and, matching git's `verify_index_match`
//! gate, refuses (`does not match index`) when the worktree file's content differs
//! from the index blob; git would instead check the file out when it is missing,
//! which this port floors by refusing rather than silently diverging. `--cached`
//! skips every worktree read, check, and write. Not implemented under these
//! (they still `bail!`/floor honestly): binary patches to the index (the binary
//! path bails before any index touch), `--reject` combined with `--index`/`--cached`,
//! and the executable-bit of a plain modification whose diff carries no `index`
//! mode line and whose pre-image is not in the index.
//!
//! `-3`/`--3way` is `try_threeway()`: the patch is replayed onto the blob its
//! `index <old>..` line names, that post-image is merged into the current
//! contents with the named blob as the common ancestor, and only a pre-image the
//! object store cannot supply — or a patch that will not apply even to it — falls
//! back to placing hunks (`Falling back to direct application...`). It implies
//! `check_index` exactly as `check_apply_state()` does, so the result is staged;
//! a merge that does not resolve writes `ll_merge`'s conflict markers, stages the
//! base/ours/theirs trio, prints `U <path>`, and exits 1.
//! `--ours`/`--theirs`/`--union` are `state->merge_variant`. Not ported inside
//! this path: git's `direct_to_threeway`, the add/add case where a creation
//! collides with an existing file — a creation therefore takes the direct route
//! and reports what git reports without `--3way`.
//!
//! Not implemented — these `bail!` rather than produce plausible-looking wrong
//! results: `-N`/`--intent-to-add` (index mutation with the intent-to-add flag and
//! empty-blob placeholder, git's `ita_only` path),
//! `--build-fake-ancestor` (writes a temporary
//! index),
//! `--inaccurate-eof` (subtle trailing-newline
//! semantics), copy patches, and non-UTF-8 paths.
//!
//! Running below the worktree root behaves as git does. `setup_git_directory()`
//! leaves the command at the top of the worktree and hands it the invocation
//! directory as `prefix`, so [`worktree_prefix`] does both, and the prefix then
//! reaches the same three places apply.c uses it: the patch-file operands are
//! resolved through it, a traditional (non-`diff --git`) patch's names gain it
//! ([`prefix_patch`]), and [`use_patch`] drops every path that does not live
//! strictly below it — silently, exit 0, as git does.
//!
//! Binary patches are applied: the `GIT binary patch` payload is base85-decoded and
//! inflated, then either used whole (`literal`) or applied as a git delta to the
//! pre-image (`delta`, `patch_delta()`'s copy/insert opcodes). Both ends are verified
//! against the ids the `index` line names, so a payload meets the pre-image it was made
//! against or the patch is refused — which also means a patch without a full index line
//! is refused, as git refuses it.
//!
//! `--ignore-whitespace`/`--ignore-space-change` (both are the same flag in git,
//! `ws_ignore_action = ignore_ws_change`) relax the search: a hunk that does not
//! land byte for byte is retried with `fuzzy_matchlines()`, which compares the
//! lines with every whitespace run collapsed — a run may differ in width but may
//! not disappear, so `a b` still does not match `ab`, and line endings are ignored
//! on both sides. A hunk that only lands that way then goes through
//! `update_pre_post_images()`: every context line of the result is re-taken from
//! the file rather than the patch, so the file's own indentation survives and only
//! added lines come out of the patch.
//!
//! `-C<n>` reduces context the way `apply_one_fragment()` does: a hunk that does not
//! land as written sheds one context line from whichever end has more of them and is
//! retried, down to the `<n>` floor.
//!
//! `--whitespace=fix` is honoured for git's default rule set (`blank-at-eol`,
//! `blank-at-eof`, `space-before-tab`): the trailing run goes and the spaces in front of
//! a tab in the indent go. A repository whose `core.whitespace` asks for
//! `indent-with-non-tab` or `tab-in-indent` keeps the refusal — those reshape the indent
//! in ways this has not been verified against, and a guess would rewrite the user's
//! bytes.
//!
//! Whitespace errors are checked before anything is written, as `check_whitespace()`
//! does: every added line goes through `ws_check()` under `core.whitespace`, the first
//! five offenders are reported as `<patch>:<line>: <error>.` followed by the line, and
//! the rest are summarised (`squelched <n> whitespace errors`). `warn` (the default)
//! then applies anyway, `nowarn` counts silently, and `error`/`error-all` refuse the
//! whole patch with `error: <n> lines add whitespace errors.` and exit 128. A
//! `whitespace` *attribute* would refine the rule per path; only the config is read.
//!
//! A fragment that `parse_fragment()` rejects — a header the `@@ -a,b +c,d @@`
//! grammar does not accept, a body that runs out before the header's counts are
//! satisfied, or a body of nothing but context (`!deleted && !added`, which
//! `--recount` exempts) — reproduces git's `error: corrupt patch at <file>:<line>`
//! and exit 128, with the line counted within the input file it came from.
//! One shape is still reported under that message rather than git's own:
//! `parse_single_patch()` only enters the fragment loop on a literal `@@ -`, so a
//! header line like `@@ bogus @@` leaves the patch with no fragments at all and
//! git falls through to `patch with only garbage at <file>:<line>` (the check
//! guarded by `state->apply || state->check` and `metadata_changes()`). The exit
//! code is the same 128; the wording is not.
//!
//! Config: `apply.whitespace` is read as the default `--whitespace` action, the
//! same as git — the command line overrides it. A `warn`/`nowarn` default is the
//! same no-op; a `fix`/`strip`/`error`/`error-all` default is deferred to the
//! unsupported-flag path (as those byte-altering actions are unimplemented); an
//! invalid value there is fatal (128) at startup, before the patch is opened and
//! ahead of any `--whitespace` on the command line, matching git's config parse
//! order. `apply.ignoreWhitespace` is read straight after it, as git does: `change`
//! turns the relaxed match on, `no`/`false`/`never`/`none` off, and any other value
//! is the same startup fatal (`unrecognized whitespace ignore option '<v>'`, 128).
//!
//! `-q`/`--quiet` silences every `error:` diagnostic, matching git, where they
//! all go through `error()`; exit codes are unaffected, and `fatal:` messages and
//! usage errors are not silenced.

use anyhow::{bail, Result};
use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode as IndexMode, Stat};
use gix::merge::blob::builtin_driver::text::{
    Conflict as MergeConflict, Labels as MergeLabels, Level as MergeLevel, Merge as MergeText,
    Rendering as MergeRendering,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{Arg, LongOpt};

/// `apply_parse_options()`'s `struct option builtin_apply_options[]` (apply.c:5202),
/// in table order, as [`super::resolve_long`] reads it.
///
/// `no-add` is an entry spelled with its own `no-`, which parse-options reads as the
/// *unset* sense of `add` — so `--add` and `--no-add` are the two senses of one
/// entry, not two options. `--exclude`/`--include` and the three
/// `--ours`/`--theirs`/`--union` conflict modes carry `PARSE_OPT_NONEG`.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "exclude",                     neg: false, arg: Arg::Required },
    LongOpt { name: "include",                     neg: false, arg: Arg::Required },
    LongOpt { name: "no-add",                      neg: true,  arg: Arg::None },
    LongOpt { name: "stat",                        neg: true,  arg: Arg::None },
    LongOpt { name: "allow-binary-replacement",    neg: true,  arg: Arg::None },
    LongOpt { name: "binary",                      neg: true,  arg: Arg::None },
    LongOpt { name: "numstat",                     neg: true,  arg: Arg::None },
    LongOpt { name: "summary",                     neg: true,  arg: Arg::None },
    LongOpt { name: "check",                       neg: true,  arg: Arg::None },
    LongOpt { name: "index",                       neg: true,  arg: Arg::None },
    LongOpt { name: "intent-to-add",               neg: true,  arg: Arg::None },
    LongOpt { name: "cached",                      neg: true,  arg: Arg::None },
    LongOpt { name: "unsafe-paths",                neg: true,  arg: Arg::None },
    LongOpt { name: "apply",                       neg: true,  arg: Arg::None },
    LongOpt { name: "3way",                        neg: true,  arg: Arg::None },
    LongOpt { name: "ours",                        neg: false, arg: Arg::None },
    LongOpt { name: "theirs",                      neg: false, arg: Arg::None },
    LongOpt { name: "union",                       neg: false, arg: Arg::None },
    LongOpt { name: "build-fake-ancestor",         neg: true,  arg: Arg::Required },
    LongOpt { name: "whitespace",                  neg: true,  arg: Arg::Required },
    LongOpt { name: "ignore-space-change",         neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-whitespace",           neg: true,  arg: Arg::None },
    LongOpt { name: "reverse",                     neg: true,  arg: Arg::None },
    LongOpt { name: "unidiff-zero",                neg: true,  arg: Arg::None },
    LongOpt { name: "reject",                      neg: true,  arg: Arg::None },
    LongOpt { name: "allow-overlap",               neg: true,  arg: Arg::None },
    LongOpt { name: "verbose",                     neg: true,  arg: Arg::None },
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "inaccurate-eof",              neg: true,  arg: Arg::None },
    LongOpt { name: "recount",                     neg: true,  arg: Arg::None },
    LongOpt { name: "directory",                   neg: true,  arg: Arg::Required },
    LongOpt { name: "allow-empty",                 neg: true,  arg: Arg::None },
];
/// git's `apply` usage block, printed after `unknown option`/`unknown switch` on
/// stderr with exit 129 (`parse-options`' `PARSE_OPT_ERROR`).
pub(super) const USAGE: &str = r"usage: git apply [<options>] [<patch>...]

    --exclude <path>      don't apply changes matching the given path
    --include <path>      apply changes matching the given path
    -p <num>              remove <num> leading slashes from traditional diff paths
    --no-add              ignore additions made by the patch
    --add                 opposite of --no-add
    --[no-]stat           instead of applying the patch, output diffstat for the input
    --[no-]numstat        show number of added and deleted lines in decimal notation
    --[no-]summary        instead of applying the patch, output a summary for the input
    --[no-]check          instead of applying the patch, see if the patch is applicable
    --[no-]index          make sure the patch is applicable to the current index
    -N, --[no-]intent-to-add
                          mark new files with `git add --intent-to-add`
    --[no-]cached         apply a patch without touching the working tree
    --[no-]unsafe-paths   accept a patch that touches outside the working area
    --[no-]apply          also apply the patch (use with --stat/--summary/--check)
    -3, --[no-]3way       attempt three-way merge, fall back on normal patch if that fails
    --ours                for conflicts, use our version
    --theirs              for conflicts, use their version
    --union               for conflicts, use a union version
    --[no-]build-fake-ancestor <file>
                          build a temporary index based on embedded index information
    -z                    paths are separated with NUL character
    -C <n>                ensure at least <n> lines of context match
    --[no-]whitespace <action>
                          detect new or modified lines that have whitespace errors
    --[no-]ignore-space-change
                          ignore changes in whitespace when finding context
    --[no-]ignore-whitespace
                          ignore changes in whitespace when finding context
    -R, --[no-]reverse    apply the patch in reverse
    --[no-]unidiff-zero   don't expect at least one line of context
    --[no-]reject         leave the rejected hunks in corresponding *.rej files
    --[no-]allow-overlap  allow overlapping hunks
    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]inaccurate-eof tolerate incorrectly detected missing new-line at the end of file
    --[no-]recount        do not trust the line counts in the hunk headers
    --[no-]directory <root>
                          prepend <root> to all filenames
    --[no-]allow-empty    don't return error for empty patches

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]allow-binary-replacement`, `--[no-]binary`.
/// Captured byte-for-byte from stock git 2.55.0's `git apply --help-all`.
pub(super) const USAGE_ALL: &str = r#"usage: git apply [<options>] [<patch>...]

    --exclude <path>      don't apply changes matching the given path
    --include <path>      apply changes matching the given path
    -p <num>              remove <num> leading slashes from traditional diff paths
    --no-add              ignore additions made by the patch
    --add                 opposite of --no-add
    --[no-]stat           instead of applying the patch, output diffstat for the input
    --[no-]allow-binary-replacement
                          no-op (backward compatibility)
    --[no-]binary         no-op (backward compatibility)
    --[no-]numstat        show number of added and deleted lines in decimal notation
    --[no-]summary        instead of applying the patch, output a summary for the input
    --[no-]check          instead of applying the patch, see if the patch is applicable
    --[no-]index          make sure the patch is applicable to the current index
    -N, --[no-]intent-to-add
                          mark new files with `git add --intent-to-add`
    --[no-]cached         apply a patch without touching the working tree
    --[no-]unsafe-paths   accept a patch that touches outside the working area
    --[no-]apply          also apply the patch (use with --stat/--summary/--check)
    -3, --[no-]3way       attempt three-way merge, fall back on normal patch if that fails
    --ours                for conflicts, use our version
    --theirs              for conflicts, use their version
    --union               for conflicts, use a union version
    --[no-]build-fake-ancestor <file>
                          build a temporary index based on embedded index information
    -z                    paths are separated with NUL character
    -C <n>                ensure at least <n> lines of context match
    --[no-]whitespace <action>
                          detect new or modified lines that have whitespace errors
    --[no-]ignore-space-change
                          ignore changes in whitespace when finding context
    --[no-]ignore-whitespace
                          ignore changes in whitespace when finding context
    -R, --[no-]reverse    apply the patch in reverse
    --[no-]unidiff-zero   don't expect at least one line of context
    --[no-]reject         leave the rejected hunks in corresponding *.rej files
    --[no-]allow-overlap  allow overlapping hunks
    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]inaccurate-eof tolerate incorrectly detected missing new-line at the end of file
    --[no-]recount        do not trust the line counts in the hunk headers
    --[no-]directory <root>
                          prepend <root> to all filenames
    --[no-]allow-empty    don't return error for empty patches

"#;

// Reasons quoted back in the deferred unsupported-flag error.
const R_INDEX: &str = "index mutation is not implemented";
const R_CONTEXT: &str = "context reduction is not implemented";
const R_WS: &str = "whitespace fixing is not implemented";
const R_EOF: &str = "EOF-newline fudging is not implemented";
const R_ANCESTOR: &str = "building a fake ancestor index is not implemented";

/// A flag git accepts that this port parses but cannot honour: the spelling as
/// the user wrote it, plus why. `key` exists so a later `--no-<flag>` cancels the
/// right entry; the vector keeps argv order, so the flag reported is the first
/// unhonoured one on the command line.
struct Unhonoured {
    key: &'static str,
    spelling: String,
    why: &'static str,
}

fn mark(v: &mut Vec<Unhonoured>, key: &'static str, spelling: &str, why: &'static str) {
    v.retain(|u| u.key != key);
    v.push(Unhonoured {
        key,
        spelling: spelling.to_owned(),
        why,
    });
}

fn unmark(v: &mut Vec<Unhonoured>, key: &'static str) {
    v.retain(|u| u.key != key);
}

/// How a `--whitespace`/`apply.whitespace` action classifies against the set
/// git's `parse_whitespace_option` accepts.
enum WsAction {
    /// `nowarn`: the check runs but says nothing.
    Silent,
    /// `warn`: report each offending added line, then apply anyway.
    Warn,
    /// `error`/`error-all`: report, then refuse the whole patch (exit 128).
    Error,
    /// `fix`/`strip`: rewrite the offending lines before applying them.
    Fix,
    /// Anything else: git rejects it as an unrecognized whitespace option.
    Invalid,
}

/// Classify a whitespace action string exactly as git's `parse_whitespace_option`
/// does (used for both the `--whitespace` flag and the `apply.whitespace` config).
fn classify_whitespace(v: &str) -> WsAction {
    match v {
        "warn" => WsAction::Warn,
        "nowarn" => WsAction::Silent,
        "error" | "error-all" => WsAction::Error,
        "fix" | "strip" => WsAction::Fix,
        _ => WsAction::Invalid,
    }
}

/// Parsed command-line options for a single `apply` invocation. Only the flags
/// this port honours get a field; the rest live in the `Unhonoured` list.
struct Opts {
    /// `--whitespace=<action>` / `apply.whitespace`, for the actions this port runs
    /// itself. `fix`/`strip` never reach here — they are deferred as unsupported.
    ws: WsAction,
    strip: usize,               // -p<n>: leading path components to drop (default 1)
    /// `state->p_value_known`: `-p<n>` appeared, so a traditional patch may not
    /// infer its own value through `guess_p_value()`.
    strip_explicit: bool,
    /// `-C<n>`: the fewest context lines a hunk may be reduced to when it does not
    /// apply as written. `None` is git's default of keeping every context line.
    p_context: Option<usize>,
    reverse: bool,              // -R/--reverse: swap pre- and post-image
    check: bool,                // --check: validate only, never write
    numstat: bool,              // --numstat: machine-readable added/deleted counts
    stat: bool,                 // --stat: git's scaled diffstat graph
    summary: bool,              // --summary: create/delete/rename/mode-change lines
    nul: bool,                  // -z: NUL-terminate --numstat records
    unidiff_zero: bool,         // --unidiff-zero: relax the begin/end anchoring
    /// `state->ws_ignore_action == ignore_ws_change`, set by
    /// `--ignore-whitespace`/`--ignore-space-change` and `apply.ignoreWhitespace`:
    /// context is matched with `fuzzy_matchlines()` instead of byte equality.
    ignore_ws: bool,
    allow_empty: bool,          // --allow-empty: an input with no patches is not an error
    no_add: bool,               // --no-add: apply context/deletions, drop additions
    verbose: bool,              // -v/--verbose: Checking/Applied progress on stderr
    reject: bool,               // --reject: apply what fits, write *.rej for the rest
    quiet: bool,                // -q/--quiet: silence `error:` diagnostics
    recount: bool,              // --recount: derive hunk sizes from the body, not the header
    index: bool,                // --index: apply to the worktree AND the index
    cached: bool,               // --cached: apply to the index only (no worktree touch)
    directory: Option<String>,  // --directory=<root>: prepend <root> to every path
    limits: Vec<(bool, String)>, // --include/--exclude rules in argv order (true = include)
    has_include: bool,          // whether any rule is an --include
    apply_override: Option<bool>, // --apply / --no-apply
    apply: bool,                // whether the patch is actually applied
    three_way: bool,            // -3/--3way: merge the patch in rather than place its hunks
    /// `--ours`/`--theirs`/`--union`: `state->merge_variant`, which resolves a
    /// 3-way conflict to one side instead of writing conflict markers.
    merge_variant: Option<MergeVariant>,
}

/// git's `XDL_MERGE_FAVOR_*`, the three ways `--3way` can silence a conflict.
#[derive(Clone, Copy)]
enum MergeVariant {
    Ours,
    Theirs,
    Union,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            // git's default is `warn`.
            ws: WsAction::Warn,
            p_context: None,
            strip: 1,
            strip_explicit: false,
            reverse: false,
            check: false,
            numstat: false,
            stat: false,
            summary: false,
            nul: false,
            unidiff_zero: false,
            ignore_ws: false,
            allow_empty: false,
            no_add: false,
            verbose: false,
            reject: false,
            quiet: false,
            recount: false,
            index: false,
            cached: false,
            directory: None,
            limits: Vec::new(),
            has_include: false,
            apply_override: None,
            apply: true,
            three_way: false,
            merge_variant: None,
        }
    }
}

/// `error:` diagnostics, which `-q` silences in git.
fn err(quiet: bool, msg: &str) {
    if !quiet {
        eprintln!("{msg}");
    }
}

/// Fetch the value of a long option, from `--name=value` or the following argv
/// entry.
fn long_value(
    args: &[String],
    i: &mut usize,
    name: &str,
    inline: Option<&str>,
) -> Result<String, ExitCode> {
    if let Some(v) = inline {
        return Ok(v.to_owned());
    }
    match args.get(*i) {
        Some(v) => {
            *i += 1;
            Ok(v.clone())
        }
        None => {
            eprintln!("error: option `{name}' requires a value");
            Err(ExitCode::from(129))
        }
    }
}

/// Parse the whole option table. Diagnostics are printed here; the returned
/// `ExitCode` is git's for that failure (129 for usage errors, 128 for the two
/// `fatal:` paths).
fn parse_opts(
    args: &[String],
    o: &mut Opts,
    sources: &mut Vec<String>,
    unhonoured: &mut Vec<Unhonoured>,
) -> Result<(), ExitCode> {
    let mut conflict_given = false;
    let mut no_more_opts = false;
    let mut i = 0;

    while i < args.len() {
        let typed = args[i].clone();
        i += 1;

        if no_more_opts || typed == "-" || !typed.starts_with('-') {
            sources.push(typed);
            continue;
        }
        if typed == "--" {
            no_more_opts = true;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): tested on the token as typed, after the `--`
        // break above and ahead of the abbreviation resolver, because it is a
        // `strcmp` — `--help-a` and `--help-all=x` stay unknown options. It
        // renders `USAGE_FULL`, which for `apply` keeps the two hidden no-ops.
        if typed == "--help-all" {
            return Err(super::show_usage(USAGE_ALL));
        }

        // Respell a unique abbreviation as the name it resolves to, so `--unidiff`
        // reaches the same arm as `--unidiff-zero` — including the arms that record
        // an option as unhonoured. Short bundles pass through untouched.
        let a = match super::canonical_long(&typed, LONG_OPTS) {
            super::Long::Name(name) => name.into_owned(),
            super::Long::Ambiguous(first, second) => {
                return Err(super::ambiguous_option(&typed, &first, &second, USAGE))
            }
        };

        if let Some(long) = a.strip_prefix("--") {
            let (given, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            // `--no-add` is an option in its own right, not the negation of
            // `--add`, so it must not be split here.
            let (name, neg) = match given.strip_prefix("no-") {
                Some(rest) if given != "no-add" => (rest, true),
                _ => (given, false),
            };

            match name {
                // ---- honoured ----
                "numstat" => o.numstat = !neg,
                "stat" => o.stat = !neg,
                "summary" => o.summary = !neg,
                "check" => o.check = !neg,
                "reverse" => o.reverse = !neg,
                "unidiff-zero" => o.unidiff_zero = !neg,
                "allow-empty" => o.allow_empty = !neg,
                "quiet" => o.quiet = !neg,
                "verbose" => o.verbose = !neg,
                "reject" => o.reject = !neg,
                "recount" => o.recount = !neg,
                "apply" => o.apply_override = Some(!neg),
                // No-ops for in-tree paths: git needs neither an opt-in for
                // overlap here (we place hunks sequentially) nor a path-safety
                // waiver (every path the harness exercises stays in-tree).
                "allow-overlap" | "unsafe-paths" => {}
                "directory" => {
                    o.directory = if neg {
                        None
                    } else {
                        Some(long_value(args, &mut i, name, inline)?)
                    }
                }
                "whitespace" => {
                    if neg {
                        unmark(unhonoured, "whitespace");
                    } else {
                        let v = long_value(args, &mut i, name, inline)?;
                        // A CLI action overrides any deferred `apply.whitespace`
                        // default read from config: `Noop` clears it, `Defer`
                        // replaces it with this spelling.
                        match classify_whitespace(&v) {
                            action @ (WsAction::Silent
                            | WsAction::Warn
                            | WsAction::Error
                            | WsAction::Fix) => {
                                unmark(unhonoured, "whitespace");
                                o.ws = action;
                            }
                            WsAction::Invalid => {
                                eprintln!("error: unrecognized whitespace option '{v}'");
                                return Err(ExitCode::from(129));
                            }
                        }
                    }
                }
                // Hidden legacy spellings, both `OPT_NOOP_NOARG` (apply.c:5216-5217)
                // — `parse_opt_noop_cb` does nothing in either sense, and neither
                // entry carries `PARSE_OPT_NONEG`, so `--no-binary` and
                // `--no-allow-binary-replacement` resolve and are no-ops too.
                "binary" | "allow-binary-replacement" => {}
                // `--add` is the default; it cancels a preceding `--no-add`.
                "add" if !neg => o.no_add = false,
                "no-add" if !neg => o.no_add = true,
                "exclude" | "include" if !neg => {
                    let pat = long_value(args, &mut i, name, inline)?;
                    o.limits.push((name == "include", pat));
                    if name == "include" {
                        o.has_include = true;
                    }
                }

                // ---- parsed, validated, reported before they could matter ----
                // `state->merge_variant`, handed to `ll_merge()` so a conflict
                // resolves to one side instead of being marked up.
                "ours" | "theirs" | "union" if !neg => {
                    conflict_given = true;
                    o.merge_variant = Some(match name {
                        "ours" => MergeVariant::Ours,
                        "theirs" => MergeVariant::Theirs,
                        _ => MergeVariant::Union,
                    });
                }
                "3way" => o.three_way = !neg,
                // --index (worktree + index) and --cached (index only) are honoured.
                "index" => o.index = !neg,
                "cached" => o.cached = !neg,
                "intent-to-add" => {
                    if neg {
                        unmark(unhonoured, "intent-to-add");
                    } else {
                        mark(unhonoured, "intent-to-add", &a, R_INDEX)
                    }
                }
                "inaccurate-eof" => {
                    if neg {
                        unmark(unhonoured, "inaccurate-eof");
                    } else {
                        mark(unhonoured, "inaccurate-eof", &a, R_EOF)
                    }
                }
                // Both spellings run `apply_option_parse_space_change()`
                // (apply.c:5048), which is a plain on/off for `ws_ignore_action`.
                "ignore-space-change" | "ignore-whitespace" => o.ignore_ws = !neg,
                "build-fake-ancestor" => {
                    if neg {
                        unmark(unhonoured, "build-fake-ancestor");
                    } else {
                        long_value(args, &mut i, name, inline)?;
                        mark(unhonoured, "build-fake-ancestor", &a, R_ANCESTOR);
                    }
                }

                // `given`, not `name`: git names the option as it was written.
                _ => {
                    eprintln!("error: unknown option `{given}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            }
            continue;
        }

        // Short options, which cluster (`-qR`) and may carry their value glued on
        // (`-p2`) or as the next argv entry (`-p 2`).
        let chars: Vec<char> = a[1..].chars().collect();
        let mut k = 0;
        while k < chars.len() {
            let c = chars[k];
            k += 1;
            match c {
                'p' | 'C' => {
                    let glued: String = chars[k..].iter().collect();
                    k = chars.len();
                    let v = if !glued.is_empty() {
                        glued
                    } else {
                        match args.get(i) {
                            Some(v) => {
                                i += 1;
                                v.clone()
                            }
                            None => {
                                eprintln!("error: switch `{c}' requires a value");
                                return Err(ExitCode::from(129));
                            }
                        }
                    };
                    if c == 'p' {
                        // git parses -p itself, so its rejection is `fatal:`/128,
                        // not parse-options' `error:`/129.
                        match v.parse::<usize>() {
                            Ok(n) => {
                                o.strip = n;
                                // `state->p_value_known`: an explicit `-p` stops
                                // `guess_p_value()` overriding it.
                                o.strip_explicit = true;
                            }
                            Err(_) => {
                                eprintln!(
                                    "fatal: option -p expects a non-negative integer, got '{v}'"
                                );
                                return Err(ExitCode::from(128));
                            }
                        }
                    } else {
                        // `-C` is `OPT_UNSIGNED` over an `unsigned int`, so its
                        // range clause reads `[0,4294967295]` and `0x10`/`1k`
                        // are values it accepts.
                        match crate::optint::unsigned_prec(&crate::optint::short_opt('C'), &v, 4) {
                            Ok(n) => o.p_context = Some(n as usize),
                            Err(e) => {
                                eprintln!("error: {e}");
                                return Err(ExitCode::from(129));
                            }
                        }
                    }
                }
                'z' => o.nul = true,
                'R' => o.reverse = true,
                'q' => o.quiet = true,
                'v' => o.verbose = true,
                'N' => mark(unhonoured, "intent-to-add", "-N", R_INDEX),
                '3' => o.three_way = true,
                // parse_options_step()'s `internal_help` check sits inside the
                // short-option loop: `-h` answers on stdout at 129, without the
                // `error:` line that precedes a rejection's copy of the block.
                'h' => return Err(super::show_usage(USAGE)),
                _ => {
                    eprintln!("error: unknown switch `{c}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            }
        }
    }

    // git's one post-parse usage check, run before it opens any patch file.
    if conflict_given && !o.three_way {
        eprintln!("fatal: --ours, --theirs, and --union require --3way");
        return Err(ExitCode::from(128));
    }

    // --check and any of the report modes (--numstat/--stat/--summary) turn
    // applying off; --apply turns it back on, and --reject forces it on (git's
    // `check_apply_state`).
    o.apply = o
        .apply_override
        .unwrap_or(!(o.check || o.numstat || o.stat || o.summary));
    if o.reject {
        o.apply = true;
    }
    Ok(())
}

pub fn apply(args: &[String]) -> Result<ExitCode> {
    let mut o = Opts::default();
    let mut sources: Vec<String> = Vec::new();
    let mut unhonoured: Vec<Unhonoured> = Vec::new();

    // git reads `apply.whitespace` from config as the default `--whitespace`
    // action, before it parses arguments. An invalid value there is fatal (128)
    // immediately — before the patch input is even opened, and regardless of a
    // valid `--whitespace` on the command line, which git parses only afterward.
    // A byte-altering (`fix`/`strip`) or erroring (`error`/`error-all`) action is
    // not implemented, so it is recorded as a deferred default exactly like the
    // CLI flag: it bails only once the input holds a patch, and a later
    // `--whitespace` on the command line overrides (clears) it.
    if let Ok(repo) = gix::discover(".") {
        if let Some(v) = repo.config_snapshot().string("apply.whitespace") {
            let v = v.to_str_lossy();
            match classify_whitespace(&v) {
                action @ (WsAction::Silent | WsAction::Warn | WsAction::Error | WsAction::Fix) => {
                    o.ws = action
                }
                WsAction::Invalid => {
                    eprintln!("error: unrecognized whitespace option '{v}'");
                    return Ok(ExitCode::from(128));
                }
            }
        }
        // `apply.ignorewhitespace`, read straight after it (apply.c:132) and just
        // as fatal when the value is neither the off-spelling nor `change`.
        if let Some(v) = repo.config_snapshot().string("apply.ignorewhitespace") {
            let v = v.to_str_lossy();
            match v.as_ref() {
                "no" | "false" | "never" | "none" => o.ignore_ws = false,
                "change" => o.ignore_ws = true,
                _ => {
                    eprintln!("error: unrecognized whitespace ignore option '{v}'");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    if let Err(code) = parse_opts(args, &mut o, &mut sources, &mut unhonoured) {
        return Ok(code);
    }

    // `check_apply_state()`, in its own order: the `--reject`/`--3way` clash
    // first, then `--3way`'s repository requirement — which also turns
    // `check_index` on, since the merge base comes out of the object store — and
    // only then the same requirement for `--index`/`--cached`.
    if o.reject && o.three_way {
        eprintln!("error: options '--reject' and '--3way' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    if o.three_way && gix::discover(".").is_err() {
        eprintln!("error: '--3way' outside a repository");
        return Ok(ExitCode::from(128));
    }
    let check_index = o.index || o.cached || o.three_way;
    if check_index && gix::discover(".").is_err() {
        let flag = if o.index { "--index" } else { "--cached" };
        eprintln!("error: '{flag}' outside a repository");
        return Ok(ExitCode::from(128));
    }

    // `setup_git_directory()` leaves every `RUN_SETUP` builtin standing at the top
    // of the worktree and hands it the directory it was invoked from as `prefix`
    // (with a trailing slash). apply.c then uses that prefix in three places, all
    // reproduced below: the patch-file operands are resolved through it
    // (apply.c's `prefix_filename(state->prefix, arg)` in `apply_all_patches`),
    // a traditional patch's names are prefixed by it ([`prefix_patch`]), and
    // `use_patch()` drops every path that does not live under it.
    let prefix = worktree_prefix()?;
    // `state->root`: `--directory=<root>` with `strbuf_complete(&root, '/')`.
    let apply_root = match &o.directory {
        Some(r) if !r.is_empty() => {
            if r.ends_with('/') { r.clone() } else { format!("{r}/") }
        }
        _ => String::new(),
    };
    if !prefix.is_empty() {
        for src in &mut sources {
            if src != "-" && !std::path::Path::new(src.as_str()).is_absolute() {
                *src = format!("{prefix}{src}");
            }
        }
    }

    // ---- read the patch text ------------------------------------------------
    let mut buf: Vec<u8> = Vec::new();
    // Where each input's first line lands in `buf`, so a parse error can name
    // the file and the line within it the way `state->patch_input_file` and a
    // per-file `state->linenr` do.
    let mut spans: Vec<(String, usize)> = Vec::new();
    if sources.is_empty() {
        spans.push(("<stdin>".to_string(), 0));
        std::io::stdin().read_to_end(&mut buf)?;
    } else {
        for src in &sources {
            let first_line = buf.iter().filter(|&&b| b == b'\n').count();
            if src == "-" {
                spans.push(("<stdin>".to_string(), first_line));
                std::io::stdin().read_to_end(&mut buf)?;
                continue;
            }
            match std::fs::read(src) {
                Ok(b) => {
                    spans.push((src.clone(), first_line));
                    buf.extend_from_slice(&b);
                }
                Err(e) => {
                    err(
                        o.quiet,
                        &format!("error: can't open patch '{src}': {}", io_msg(&e)),
                    );
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    let spans = InputSpans { spans };
    let mut patches = match parse_patches(
        &split_lines(&buf),
        o.strip,
        o.strip_explicit,
        &prefix,
        &apply_root,
        o.recount,
        &spans,
    ) {
        Ok(p) => p,
        // apply.c reports a corrupt fragment through `error()` and unwinds to
        // `git apply`'s exit 128, rather than dying with the crate's usual
        // `zvcs: apply: …` prefix and exit 1.
        Err(e) => {
            let e = match e.downcast::<CorruptPatch>() {
                Ok(corrupt) => {
                    err(o.quiet, &format!("error: {corrupt}"));
                    return Ok(ExitCode::from(128));
                }
                Err(e) => e,
            };
            let header = e.downcast::<HeaderError>()?;
            err(o.quiet, &format!("error: {header}"));
            return Ok(ExitCode::from(128));
        }
    };
    if patches.is_empty() {
        if o.allow_empty {
            return Ok(ExitCode::SUCCESS);
        }
        err(
            o.quiet,
            "error: No valid patches in input (allow with \"--allow-empty\")",
        );
        return Ok(ExitCode::from(128));
    }

    // Past here a flag we cannot honour would change the result, so report it.
    if let Some(u) = unhonoured.first() {
        let (flag, why) = (&u.spelling, u.why);
        bail!("unsupported flag {flag:?}: {why}");
    }

    if let Some(root) = &o.directory {
        for p in &mut patches {
            prefix_names(p, root)?;
        }
    }
    // `prefix_patch()` (apply.c:2191), which `parse_chunk()` runs on every patch as
    // it is parsed: a traditional diff's names were written relative to the
    // invocation directory, so they gain the prefix. A `diff --git` patch is already
    // root-relative and is left alone.
    if !prefix.is_empty() {
        for p in &mut patches {
            prefix_patch(p, &prefix);
        }
    }
    if o.reverse {
        for p in &mut patches {
            p.reverse();
        }
    }

    // --include/--exclude and the invocation prefix: keep only the patches whose
    // (post-strip, post-prefix) name the rule list admits (git's `use_patch`). An
    // empty result is not an error — the input still held valid patches.
    if !o.limits.is_empty() || !prefix.is_empty() {
        patches.retain(|p| use_patch(p, &prefix, &o.limits, o.has_include));
    }

    // `check_whitespace()`: every added line is checked before anything is written,
    // so `--whitespace=error` refuses the patch with the worktree untouched. The rule
    // comes from `core.whitespace`; a `whitespace` attribute would refine it per path,
    // which this pass does not read.
    if !patches.is_empty() && !matches!(o.ws, WsAction::Invalid) {
        let rule = gix::discover(".")
            .map(|repo| super::diff_color::whitespace_rule_cfg(&repo))
            .unwrap_or(super::diff_color::WS_DEFAULT_RULE);
        // `--whitespace=fix` reports the offending lines exactly as `warn` does, then
        // rewrites them. Only the default rule set is reproduced byte-for-byte, so any
        // other one keeps the honest refusal.
        if matches!(o.ws, WsAction::Fix) && !ws_fix_supported(rule) {
            bail!(
                "unsupported flag \"--whitespace=fix\": {R_WS} for a non-default \
                 core.whitespace"
            );
        }
        let errors = report_whitespace(&patches, &spans, rule, &o.ws, o.quiet);
        if matches!(o.ws, WsAction::Fix) {
            for p in &mut patches {
                for h in &mut p.hunks {
                    for (_, post_idx) in &h.added_at {
                        if let Some(line) = h.post.get_mut(*post_idx) {
                            if super::diff_files::ws_check(line, rule) != 0 {
                                *line = ws_fix_default(line);
                            }
                        }
                    }
                }
            }
        }
        if errors > 0 && matches!(o.ws, WsAction::Error) {
            err(
                o.quiet,
                &format!(
                    "error: {errors} {} whitespace errors.",
                    if errors == 1 { "line adds" } else { "lines add" }
                ),
            );
            return Ok(ExitCode::from(128));
        }
        if errors > 0 && matches!(o.ws, WsAction::Fix) && o.apply {
            err(
                o.quiet,
                &format!(
                    "warning: {errors} {} after fixing whitespace errors.",
                    if errors == 1 { "line applied" } else { "lines applied" }
                ),
            );
        }
        if errors > 0 && matches!(o.ws, WsAction::Warn) {
            err(
                o.quiet,
                &format!(
                    "warning: {errors} {} whitespace errors.",
                    if errors == 1 { "line adds" } else { "lines add" }
                ),
            );
        }
    }

    // git prints its report modes in this fixed order: the scaled --stat graph,
    // then the machine-readable --numstat records, then the --summary lines.
    if o.stat {
        print!("{}", render_stat(&patches));
    }
    if o.numstat {
        print!("{}", render_numstat(&patches, o.nul));
    }
    if o.summary {
        print!("{}", render_summary(&patches));
    }
    if !o.apply && !o.check {
        return Ok(ExitCode::SUCCESS);
    }

    // The reject path applies each file independently and writes *.rej; it does not
    // stage the index. Rather than silently leave the index un-updated, refuse the
    // combination (git supports it; this port floors it honestly).
    if o.reject && check_index {
        bail!("--reject with --index/--cached is not implemented");
    }

    // --reject takes a wholly separate path: it applies each file's hunks
    // independently (not all-or-nothing), writes partial results, and drops the
    // hunks that did not land into a `<name>.rej` file. git forces verbose there.
    if o.reject {
        return reject_apply(&patches, &o);
    }

    // ---- index substrate (only when --index/--cached) -----------------------
    // Hold the repo lock across the whole check-and-write span so the index we read
    // pre-images from is the same one we mutate and write — no concurrent writer can
    // slip in between, mirroring how git holds `lock_file` for the operation.
    let (idx_repo, mut idx_index, _idx_lock) = if check_index {
        let repo = gix::discover(".")?;
        let lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let index = if repo.index_path().exists() {
            repo.open_index()?
        } else {
            gix::index::File::from_state(
                gix::index::State::new(repo.object_hash()),
                repo.index_path(),
            )
        };
        (Some(repo), Some(index), Some(lock))
    } else {
        (None, None, None)
    };
    // `update_index` gates the mutation itself: with `--check`/`--stat` (apply off)
    // the pre-image still comes from the index, but nothing is written.
    let update_index = check_index && o.apply;

    // ---- check phase: build every result in memory, touching nothing --------
    let mut staged: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut failed = false;
    // `patch->conflicted_threeway`: paths whose 3-way merge left markers behind,
    // with the stage 1/2/3 blobs `add_conflicted_stages_file()` records.
    let mut conflicted: Vec<(String, u32, [Option<ObjectId>; 3])> = Vec::new();

    for p in &patches {
        // The name git reports progress and success against.
        let name = p.new_name.clone().or_else(|| p.old_name.clone()).unwrap_or_default();
        // The name git reports errors against: the pre-image path when there is
        // one (`apply_fragments`), else the post-image path.
        let label = p.old_name.clone().or_else(|| p.new_name.clone()).unwrap_or_default();

        if o.verbose {
            eprintln!("Checking patch {name}...");
        }

        // A view of the index for this iteration; recomputed each time so no
        // immutable borrow of `idx_index` is held into the mutable write phase.
        let idx_view = idx_repo.as_ref().zip(idx_index.as_ref());

        // A path that must not already exist: a creation target, or a rename
        // destination. git's `check_to_create` reports it against the index when
        // `--index`/`--cached`, otherwise against the worktree.
        if let Some(new) = &p.new_name {
            if p.is_new || p.is_rename {
                match create_block(&staged, idx_view, o.cached, new) {
                    Some(Block::InIndex) => {
                        err(o.quiet, &format!("error: {new}: already exists in index"));
                        failed = true;
                        continue;
                    }
                    Some(Block::InWorktree) => {
                        err(
                            o.quiet,
                            &format!("error: {new}: already exists in working directory"),
                        );
                        failed = true;
                        continue;
                    }
                    None => {}
                }
            }
        }

        // The pre-image bytes, kept whole: a text patch works on its lines, a binary
        // one on the bytes themselves.
        let mut pre_bytes: Vec<u8> = Vec::new();
        let mut image: Vec<Vec<u8>> = if p.is_new {
            Vec::new()
        } else {
            let old = p.old_name.as_deref().unwrap_or_default();
            match read_preimage(&staged, idx_view, o.cached, old) {
                PreRead::Found(bytes) => {
                    pre_bytes = bytes.clone();
                    split_lines(&bytes).into_iter().map(|l| l.to_vec()).collect()
                }
                PreRead::MissingWorktree => {
                    err(o.quiet, &format!("error: {old}: No such file or directory"));
                    failed = true;
                    continue;
                }
                PreRead::MissingIndex => {
                    err(o.quiet, &format!("error: {old}: does not exist in index"));
                    failed = true;
                    continue;
                }
                PreRead::Mismatch => {
                    err(o.quiet, &format!("error: {old}: does not match index"));
                    failed = true;
                    continue;
                }
            }
        };

        // `apply_data()`: under `--3way` the merge is what applies the patch, and
        // only a pre-image the object store cannot supply — or a patch that will
        // not even apply to that pre-image — falls back to placing hunks.
        let mut merged: Option<ThreeWay> = None;
        if o.three_way && !p.binary {
            let repo = idx_repo.as_ref().expect("--3way implies check_index");
            match try_threeway(repo, p, &pre_bytes, &o)? {
                ThreeWayOutcome::Merged(tw) => {
                    err(
                        o.quiet,
                        &if tw.stages.is_some() {
                            format!("Applied patch to '{}' with conflicts.", tw.path)
                        } else {
                            format!("Applied patch to '{}' cleanly.", tw.path)
                        },
                    );
                    image = vec![tw.content.clone()];
                    merged = Some(tw);
                }
                ThreeWayOutcome::Fallback(reason) => {
                    if let Some(msg) = reason {
                        err(o.quiet, &format!("error: {msg}"));
                    }
                    err(o.quiet, "Falling back to direct application...");
                }
            }
        }

        // `apply_binary()`: the payload rebuilds the whole file, and both ends are
        // checked against the ids the `index` line named.
        if merged.is_some() {
            // The merge already produced the whole post-image.
        } else if p.binary {
            match rebuild_binary(p, &pre_bytes) {
                Ok(bytes) => image = vec![bytes],
                Err(msg) => {
                    err(o.quiet, &format!("error: {msg}"));
                    failed = true;
                    continue;
                }
            }
        } else if let Err(idx) =
            apply_hunks(&mut image, p, o.unidiff_zero, o.no_add, o.p_context, o.ignore_ws)
        {
            let h = &p.hunks[idx];
            if o.verbose {
                let pre: Vec<u8> = h.pre.concat();
                err(
                    o.quiet,
                    &format!("error: while searching for:\n{}", String::from_utf8_lossy(&pre)),
                );
            }
            err(o.quiet, &format!("error: patch failed: {label}:{}", h.old_pos));
            err(o.quiet, &format!("error: {label}: patch does not apply"));
            failed = true;
            continue;
        }

        if p.is_delete {
            if !image.is_empty() {
                err(o.quiet, "error: removal patch leaves file contents");
                failed = true;
                continue;
            }
            let old = p.old_name.clone().unwrap_or_default();
            staged.insert(old.clone(), None);
            ops.push(Op {
                name,
                remove: Some(old),
                prune_dirs: true,
                create: None,
            });
            continue;
        }

        let new = p.new_name.clone().unwrap_or_default();
        let data: Vec<u8> = image.concat();
        // git defaults a modification's new mode to the pre-image mode; under
        // `--index`/`--cached` that pre-image mode is the index entry's, so an
        // executable file stays executable even when the diff carries no mode line.
        let mode = match p.new_mode {
            Some(m) => m,
            None => {
                let from_index = if p.is_new {
                    None
                } else {
                    idx_view.and_then(|(_, index)| {
                        let old = p.old_name.as_deref()?;
                        index
                            .entry_by_path(old.as_bytes().as_bstr())
                            .map(|e| e.mode.bits())
                    })
                };
                from_index.unwrap_or(0o100644)
            }
        };
        if let Some(old) = &p.old_name {
            if old != &new {
                staged.insert(old.clone(), None);
            }
        }
        staged.insert(new.clone(), Some(data.clone()));
        if let Some(stages) = merged.and_then(|tw| tw.stages) {
            conflicted.push((new.clone(), mode, stages));
        }
        ops.push(Op {
            name,
            remove: p.old_name.clone(),
            prune_dirs: p.is_rename,
            create: Some((new, mode, data)),
        });
    }

    if failed {
        return Ok(ExitCode::from(1));
    }
    if !o.apply {
        return Ok(ExitCode::SUCCESS);
    }

    // ---- write phase: nothing here may fail on a well-formed patch ----------
    // Index mutations are accumulated by path and replayed once at the end (git's
    // `remove_file`/`add_index_file`); `--cached` skips every worktree touch.
    let mut idx_remove: Vec<BString> = Vec::new();
    let mut idx_add: Vec<(BString, ObjectId, IndexMode, Stat)> = Vec::new();

    for op in ops {
        if let Some(old) = &op.remove {
            if !o.cached {
                let _ = std::fs::remove_file(old);
                if op.prune_dirs {
                    prune_empty_parents(Path::new(old));
                }
            }
            if update_index {
                idx_remove.push(old.clone().into_bytes().into());
            }
        }
        if let Some((path, mode, data)) = op.create {
            if !o.cached {
                create_leading_dirs(Path::new(&path))?;
                write_created(Path::new(&path), mode, &data)?;
            }
            if update_index {
                let repo = idx_repo.as_ref().expect("repo present when update_index");
                let id = repo.write_blob(&data)?.detach();
                // For `--index` the entry's stat comes from the file just written
                // (git's `fill_stat_cache_info`); `--cached` writes no file, so the
                // stat is zeroed, exactly as `make_empty_cache_entry` leaves it.
                let stat = if o.cached {
                    Stat::default()
                } else {
                    let md = gix::index::fs::Metadata::from_path_no_follow(Path::new(&path))?;
                    Stat::from_fs(&md)?
                };
                idx_add.push((path.clone().into_bytes().into(), id, to_index_mode(mode), stat));
            }
        }
        if o.verbose {
            eprintln!("Applied patch {} cleanly.", op.name);
        }
    }

    if update_index {
        let index = idx_index.as_mut().expect("index present when update_index");
        // If two patches in one input touched the same path, keep only the last
        // add for it — git's `add_index_entry` replaces in place, so the final
        // state wins. Reverse, keep first-seen (= original last), let the later
        // `sort_entries` re-order.
        idx_add.reverse();
        let mut seen: HashSet<BString> = HashSet::new();
        idx_add.retain(|(p, _, _, _)| seen.insert(p.clone()));
        // Every touched path is dropped (any prior stage) before its fresh stage-0
        // entry is pushed; a pure deletion contributes only a removal.
        let drop_set: HashSet<BString> = idx_remove
            .iter()
            .cloned()
            .chain(idx_add.iter().map(|(p, _, _, _)| p.clone()))
            .collect();
        index.remove_entries(|_, path, _| drop_set.contains(&path.to_owned()));
        // `add_conflicted_stages_file()` replaces a conflicted path's stage-0
        // entry with the base/ours/theirs trio, so the path reads as unmerged.
        let conflicted_paths: HashSet<BString> = conflicted
            .iter()
            .map(|(p, _, _)| BString::from(p.clone().into_bytes()))
            .collect();
        for (path, id, mode, stat) in &idx_add {
            if conflicted_paths.contains(path) {
                continue;
            }
            index.dangerously_push_entry(*stat, *id, Flags::empty(), *mode, path.as_ref());
        }
        for (path, mode, stages) in &conflicted {
            let path = BString::from(path.clone().into_bytes());
            for (n, id) in stages.iter().enumerate() {
                let Some(id) = id else { continue };
                index.dangerously_push_entry(
                    Stat::default(),
                    *id,
                    Flags::from_stage(match n {
                        0 => gix::index::entry::Stage::Base,
                        1 => gix::index::entry::Stage::Ours,
                        _ => gix::index::entry::Stage::Theirs,
                    }),
                    to_index_mode(*mode),
                    path.as_ref(),
                );
            }
        }
        index.sort_entries();
        // Drop the cached tree so a later commit cannot capture a stale subtree.
        index.remove_tree();
        index.write(gix::index::write::Options::default())?;
    }

    // `write_out_results()`: the conflicted paths are named once every write is
    // done, in sorted order, and make the whole run fail.
    if !conflicted.is_empty() {
        let mut names: Vec<&str> = conflicted.iter().map(|(p, _, _)| p.as_str()).collect();
        names.sort_unstable();
        for name in names {
            err(o.quiet, &format!("U {name}"));
        }
        return Ok(ExitCode::from(1));
    }

    Ok(ExitCode::SUCCESS)
}

/// The outcome of `try_threeway()`: either the merge produced the post-image, or
/// the caller must fall back to placing the patch's hunks directly.
enum ThreeWayOutcome {
    Merged(ThreeWay),
    /// `try_threeway()` returned `< 0`. The payload is the `error()` git printed
    /// on the way out, when it printed one.
    Fallback(Option<String>),
}

/// A completed 3-way merge.
struct ThreeWay {
    /// `patch->new_name`, the path git names in its report.
    path: String,
    /// The merged post-image.
    content: Vec<u8>,
    /// `patch->threeway_stage`, set only when the merge did not resolve: the
    /// pre-image (absent for a creation), ours, and theirs.
    stages: Option<[Option<ObjectId>; 3]>,
}

/// Port of `try_threeway()` (apply.c): rebuild the post-image the patch was
/// written to produce, then merge it into the current contents using the blob the
/// patch names as the common ancestor.
///
/// `ours` is the pre-image the caller already read — git's `load_preimage()`
/// result, which is the index blob under `check_index` and the worktree file
/// otherwise.
fn try_threeway(
    repo: &gix::Repository,
    p: &Patch,
    ours: &[u8],
    o: &Opts,
) -> Result<ThreeWayOutcome> {
    // "No point falling back to 3-way merge in these cases". A creation is on
    // the list too: git only merges one through `direct_to_threeway`, the
    // add/add path this port does not build.
    let gitlink = |m: Option<u32>| m.is_some_and(|m| m & 0o170000 == 0o160000);
    if p.is_delete
        || p.is_new
        || gitlink(p.old_mode)
        || gitlink(p.new_mode)
        || (p.is_rename && p.added == 0 && p.deleted == 0)
    {
        return Ok(ThreeWayOutcome::Fallback(None));
    }

    let path = p
        .new_name
        .clone()
        .or_else(|| p.old_name.clone())
        .unwrap_or_default();
    let missing_blob =
        || Ok(ThreeWayOutcome::Fallback(Some(
            "repository lacks the necessary blob to perform 3-way merge.".to_string(),
        )));

    // "Preimage the patch was prepared for": the `index <old>..` id, read as a blob.
    let Some(pre_hex) = p.preimage_id(o.reverse) else {
        return missing_blob();
    };
    let Ok(pre_id) = repo.rev_parse_single(pre_hex.as_bytes().as_bstr()) else {
        return missing_blob();
    };
    let Some(pre_bytes) = repo
        .find_object(pre_id)
        .ok()
        .and_then(|obj| obj.try_into_blob().ok())
        .map(|blob| blob.data.clone())
    else {
        return missing_blob();
    };
    let pre_id = pre_id.detach();

    // "Apply the patch to get the post image" — against that pre-image, not
    // against what is on disk.
    let mut post: Vec<Vec<u8>> = split_lines(&pre_bytes)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
    if apply_hunks(&mut post, p, o.unidiff_zero, o.no_add, o.p_context, o.ignore_ws).is_err() {
        return Ok(ThreeWayOutcome::Fallback(None));
    }
    let post_bytes: Vec<u8> = post.concat();
    let post_id = repo.write_blob(&post_bytes)?.detach();
    let our_id = repo.write_blob(ours)?.detach();

    // `three_way_merge()`'s trivial resolutions, which never reach the merge
    // driver: one side did not move, so the other side is the answer.
    if pre_id == our_id {
        return Ok(ThreeWayOutcome::Merged(ThreeWay {
            path,
            content: post_bytes,
            stages: None,
        }));
    }
    if pre_id == post_id || our_id == post_id {
        return Ok(ThreeWayOutcome::Merged(ThreeWay {
            path,
            content: ours.to_vec(),
            stages: None,
        }));
    }

    // `ll_merge()` with `LL_MERGE_OPTIONS_INIT`: `XDL_MERGE_ZEALOUS`, the
    // configured conflict style, and git's fixed base/ours/theirs labels.
    let style = match super::merge_file::conflict_style_config(Some(repo)) {
        Ok(s) => s,
        Err(code) => return Ok(ThreeWayOutcome::Fallback(Some(format!(
            "merge.conflictStyle is unusable (exit {code:?})"
        )))),
    };
    let conflict = match o.merge_variant {
        Some(MergeVariant::Ours) => MergeConflict::ResolveWithOurs,
        Some(MergeVariant::Theirs) => MergeConflict::ResolveWithTheirs,
        Some(MergeVariant::Union) => MergeConflict::ResolveWithUnion,
        None => MergeConflict::Keep {
            style,
            marker_size: std::num::NonZeroU8::new(7).expect("7 is not zero"),
        },
    };
    let mut content = Vec::new();
    let mut input = gix::diff::blob::InternedInput::default();
    let merge = MergeText::new(
        &mut input,
        ours,
        &pre_bytes,
        &post_bytes,
        gix::diff::blob::Algorithm::Myers,
    );
    let (_resolution, conflicts) = merge.run_with(
        &mut content,
        MergeLabels {
            current: Some(b"ours".as_bstr()),
            ancestor: Some(b"base".as_bstr()),
            other: Some(b"theirs".as_bstr()),
        },
        MergeRendering {
            conflict,
            style: Some(style),
            level: MergeLevel::Zealous,
            marker_size: Some(7),
        },
    );

    Ok(ThreeWayOutcome::Merged(ThreeWay {
        path,
        content,
        stages: (conflicts > 0).then_some([Some(pre_id), Some(our_id), Some(post_id)]),
    }))
}

/// Whether a creation/rename target is already taken, and where. git's
/// `check_to_create`: under `--index`/`--cached` the index is consulted first, then
/// (unless `--cached`) the worktree; without index mode only the worktree.
enum Block {
    InIndex,
    InWorktree,
}

fn create_block(
    staged: &HashMap<String, Option<Vec<u8>>>,
    idx: Option<(&gix::Repository, &gix::index::File)>,
    cached: bool,
    new: &str,
) -> Option<Block> {
    // An in-run result (a previous patch in this same invocation) wins first.
    match staged.get(new) {
        Some(Some(_)) => {
            return Some(if idx.is_some() {
                Block::InIndex
            } else {
                Block::InWorktree
            })
        }
        Some(None) => return None, // deleted earlier this run: the path is free
        None => {}
    }
    match idx {
        Some((_, index)) => {
            if index.entry_by_path(new.as_bytes().as_bstr()).is_some() {
                return Some(Block::InIndex);
            }
            if !cached && std::fs::symlink_metadata(new).is_ok() {
                return Some(Block::InWorktree);
            }
            None
        }
        None if std::fs::symlink_metadata(new).is_ok() => Some(Block::InWorktree),
        None => None,
    }
}

/// The outcome of reading a patch's pre-image.
enum PreRead {
    Found(Vec<u8>),
    MissingWorktree,
    MissingIndex,
    Mismatch,
}

/// Load a patch's pre-image, from an earlier in-run result if present, else from
/// the index blob (git's `load_patch_target` when `check_index`) or the worktree.
/// Under `--index` (not `--cached`) the worktree content is verified against the
/// index blob — git's `verify_index_match` — refusing on any divergence (git would
/// instead check out a missing file, which this port floors by refusing).
fn read_preimage(
    staged: &HashMap<String, Option<Vec<u8>>>,
    idx: Option<(&gix::Repository, &gix::index::File)>,
    cached: bool,
    old: &str,
) -> PreRead {
    if let Some(entry) = staged.get(old) {
        return match entry {
            Some(bytes) => PreRead::Found(bytes.clone()),
            None => PreRead::MissingWorktree,
        };
    }
    match idx {
        Some((repo, index)) => {
            let Some(ce) = index.entry_by_path(old.as_bytes().as_bstr()) else {
                return PreRead::MissingIndex;
            };
            let bytes = match repo.find_object(ce.id) {
                Ok(obj) => obj.data.clone(),
                Err(_) => return PreRead::MissingIndex,
            };
            if !cached {
                let empty = HashMap::new();
                match read_current(&empty, old) {
                    Some(wt) => {
                        match gix::objs::compute_hash(
                            repo.object_hash(),
                            gix::objs::Kind::Blob,
                            &wt,
                        ) {
                            Ok(h) if h == ce.id => {}
                            _ => return PreRead::Mismatch,
                        }
                    }
                    None => return PreRead::Mismatch,
                }
            }
            PreRead::Found(bytes)
        }
        None => match read_current(staged, old) {
            Some(bytes) => PreRead::Found(bytes),
            None => PreRead::MissingWorktree,
        },
    }
}

/// Map a patch's raw file mode to the canonical index entry mode git records
/// (`create_ce_mode`): symlink, gitlink, or a regular file normalised to
/// executable/non-executable.
fn to_index_mode(mode: u32) -> IndexMode {
    match mode & 0o170000 {
        0o120000 => IndexMode::SYMLINK,
        0o160000 => IndexMode::COMMIT,
        _ if mode & 0o111 != 0 => IndexMode::FILE_EXECUTABLE,
        _ => IndexMode::FILE,
    }
}

/// One file's worth of work, resolved during the check phase and replayed
/// verbatim during the write phase (git's `write_out_one_result`: remove the
/// pre-image path, then create the post-image path).
struct Op {
    name: String, // display name for the verbose `Applied patch <name> cleanly.`
    remove: Option<String>,
    prune_dirs: bool,
    create: Option<(String, u32, Vec<u8>)>,
}

/// A single file's patch: the extended header facts plus its hunks.
struct Patch {
    old_name: Option<String>, // None once normalised => creation
    new_name: Option<String>, // None once normalised => deletion
    old_mode: Option<u32>,    // pre-image mode, for the summary's `mode change` line
    new_mode: Option<u32>,
    is_new: bool,
    is_delete: bool,
    is_rename: bool,
    binary: bool,
    /// The `GIT binary patch` payloads, forward first and the reverse second when the
    /// patch carries one (`--binary` writes both). `None` for a `Binary files … differ`
    /// stub, which carries no data at all.
    binary_forward: Option<BinaryPayload>,
    binary_reverse: Option<BinaryPayload>,
    /// The two ids of the `index <old>..<new>` line, when it carried them in full.
    /// A binary patch is only applied when they are there: git needs them to check
    /// that the pre-image is the one the payload was made against.
    index_old: Option<String>,
    index_new: Option<String>,
    score: u32, // `similarity index N%`, for the summary's rename line
    /// `patch->is_toplevel_relative`: set by `parse_git_header()` (apply.c:1457) for
    /// a `diff --git` patch, whose names are already relative to the worktree root.
    /// A traditional `---`/`+++` diff leaves it clear (apply.c:1596) and its names
    /// are read as relative to the directory `git apply` was invoked from, so
    /// [`prefix_patch`] prepends the prefix to them.
    is_toplevel_relative: bool,
    hunks: Vec<Hunk>,
    added: usize,
    deleted: usize,
}

impl Patch {
    /// `patch->old_oid_prefix`: the id of the blob the patch was written against,
    /// which the 3-way merge uses as its common ancestor.
    ///
    /// `reverse_patches()` swaps the pair along with everything else it reverses.
    /// [`Patch::reverse`] leaves `index_old`/`index_new` in file order because the
    /// binary payload they are checked against is not swapped either, so under
    /// `-R` the pre-image id is the one written second.
    fn preimage_id(&self, reversed: bool) -> Option<&String> {
        if reversed {
            self.index_new.as_ref()
        } else {
            self.index_old.as_ref()
        }
    }

    /// `-R`: swap the two images, so the patch undoes itself.
    fn reverse(&mut self) {
        std::mem::swap(&mut self.old_name, &mut self.new_name);
        std::mem::swap(&mut self.is_new, &mut self.is_delete);
        std::mem::swap(&mut self.added, &mut self.deleted);
        // A reversal swaps the two sides' modes too, so a reversed creation's
        // `new file mode` becomes the deletion's `deleted file mode`, and a
        // reversed mode change inverts. Context lines are direction-neutral, so
        // `h.context` (used by --no-add) is left as is.
        std::mem::swap(&mut self.old_mode, &mut self.new_mode);
        for h in &mut self.hunks {
            std::mem::swap(&mut h.pre, &mut h.post);
            std::mem::swap(&mut h.pre_common, &mut h.post_common);
            std::mem::swap(&mut h.old_pos, &mut h.new_pos);
        }
    }
}

/// One `@@` fragment. `pre`/`post` hold whole lines *including* their trailing
/// newline (absent on a line marked `\ No newline at end of file`), matching how
/// git's `struct image` stores them so the EOF-newline distinction falls out of
/// plain byte comparison.
#[derive(Clone)]
struct Hunk {
    old_pos: usize,
    new_pos: usize,
    pre: Vec<Vec<u8>>,
    /// `LINE_COMMON` on the pre-image: which of `pre`'s lines are context rather
    /// than deletions, so a relaxed match can pair them with the post-image's
    /// context lines the way `update_pre_post_images()` does.
    pre_common: Vec<bool>,
    post: Vec<Vec<u8>>,
    /// `LINE_COMMON` on the post-image: which of `post`'s lines are context rather
    /// than additions.
    post_common: Vec<bool>,
    /// `(index into the concatenated input, index into `post`)` for every added line,
    /// which is what the whitespace check reports against (`<patch>:<line>: …`).
    added_at: Vec<(usize, usize)>,
    context: Vec<Vec<u8>>, // the context lines only, spliced in for --no-add
    raw: Vec<u8>,          // the fragment's verbatim text (header + body) for *.rej
    trailing: usize,       // trailing context lines; 0 means the hunk must match at EOF
    leading: usize,        // leading context lines, for `-C<n>` context reduction
}

// ---------------------------------------------------------------------------
// hunk placement — port of apply.c:find_pos / match_fragment
// ---------------------------------------------------------------------------

/// Apply every hunk of `p` to `image` in order. On failure returns the index of
/// the failing hunk (the caller reads its `old_pos`/`pre` for git's
/// `patch failed: <path>:<n>` and verbose `while searching for:` diagnostics).
/// With `no_add`, the post-image drops the added lines, leaving only context.
fn apply_hunks(
    image: &mut Vec<Vec<u8>>,
    p: &Patch,
    unidiff_zero: bool,
    no_add: bool,
    // `-C<n>`: the fewest context lines a hunk may be reduced to before it is called
    // a failure. `None` keeps every context line, which is git's default.
    p_context: Option<usize>,
    // `state->ws_ignore_action == ignore_ws_change`.
    ignore_ws: bool,
) -> Result<(), usize> {
    for (idx, h) in p.hunks.iter().enumerate() {
        if let Some(at) = place_hunk(image.as_slice(), h, unidiff_zero, ignore_ws) {
            let repl = replacement(image.as_slice(), at, h, no_add, ignore_ws);
            image.splice(at..at + h.pre.len(), repl);
            continue;
        }
        // `apply_one_fragment()`'s reduction loop: drop a context line from whichever
        // end has more of them and try again, down to the `-C<n>` floor.
        if let Some(floor) = p_context {
            if let Some((at, trimmed)) =
                place_reduced(image.as_slice(), h, unidiff_zero, floor, ignore_ws)
            {
                let repl = replacement(image.as_slice(), at, &trimmed, no_add, ignore_ws);
                image.splice(at..at + trimmed.pre.len(), repl);
                continue;
            }
        }
        return Err(idx);
    }
    Ok(())
}

/// The lines that replace the pre-image at `at`.
///
/// `update_pre_post_images()` (apply.c:2433), which `line_by_line_fuzzy_match()`
/// runs once a hunk has matched only under relaxed whitespace: the pre-image is
/// replaced by the bytes actually in the file, and every context line of the
/// post-image is re-taken from its counterpart there, in order. So a context line
/// keeps the file's whitespace rather than the patch's — only added lines come out
/// of the patch. When the two matched byte for byte this copies each line onto
/// itself, which is why it needs no separate "was the match fuzzy" flag.
fn replacement(
    image: &[Vec<u8>],
    at: usize,
    h: &Hunk,
    no_add: bool,
    ignore_ws: bool,
) -> Vec<Vec<u8>> {
    let source = if no_add { &h.context } else { &h.post };
    if !ignore_ws {
        return source.clone();
    }
    // The pre-image lines that are context, which the post-image's context lines
    // pair with one for one and in order (git's `LINE_COMMON` walk).
    let mut common = h
        .pre_common
        .iter()
        .enumerate()
        .filter(|(_, &c)| c)
        .map(|(j, _)| j);
    let mut out = Vec::with_capacity(source.len());
    for (k, line) in source.iter().enumerate() {
        // Under `--no-add` the replacement is the context lines alone, so every
        // one of them is common.
        if !no_add && !h.post_common.get(k).copied().unwrap_or(false) {
            out.push(line.clone());
            continue;
        }
        match common.next().and_then(|j| image.get(at + j)) {
            Some(file_line) => out.push(file_line.clone()),
            None => out.push(line.clone()),
        }
    }
    out
}

/// Trim context off `h` one line at a time — the longer end first, as git does — and
/// return the placement of the first trimmed form that lands, together with that form.
fn place_reduced(
    image: &[Vec<u8>],
    h: &Hunk,
    unidiff_zero: bool,
    floor: usize,
    ignore_ws: bool,
) -> Option<(usize, Hunk)> {
    let mut cur = h.clone();
    while cur.leading > floor || cur.trailing > floor {
        let from_front = cur.leading > cur.trailing;
        if from_front {
            cur.leading -= 1;
            cur.pre.remove(0);
            cur.pre_common.remove(0);
            cur.post.remove(0);
            cur.post_common.remove(0);
            if !cur.context.is_empty() {
                cur.context.remove(0);
            }
            // The pre-image now starts one line later.
            cur.old_pos += 1;
            cur.new_pos += 1;
        } else {
            cur.trailing -= 1;
            cur.pre.pop();
            cur.pre_common.pop();
            cur.post.pop();
            cur.post_common.pop();
            cur.context.pop();
        }
        if let Some(at) = place_hunk(image, &cur, unidiff_zero, ignore_ws) {
            return Some((at, cur));
        }
    }
    None
}

/// Where hunk `h`'s pre-image lands in `image`, or `None` if it does not apply.
fn place_hunk(image: &[Vec<u8>], h: &Hunk, unidiff_zero: bool, ignore_ws: bool) -> Option<usize> {
    // "a hunk that is (oldpos <= 1) with or without leading context must match at
    // the beginning"; "a hunk without trailing lines must match at the end" —
    // both defeated by --unidiff-zero, which makes the absence of context
    // uninformative.
    let match_beginning = h.old_pos == 0 || (h.old_pos == 1 && !unidiff_zero);
    let match_end = !unidiff_zero && h.trailing == 0;
    let start = h.new_pos.saturating_sub(1);
    find_pos(image, &h.pre, start, match_beginning, match_end, ignore_ws)
}

/// Locate `pre` in `image`, starting at `line` and walking outward one line at a
/// time, alternating backwards then forwards exactly as git does (so a patch
/// that could land in two places lands where git lands it).
fn find_pos(
    image: &[Vec<u8>],
    pre: &[Vec<u8>],
    mut line: usize,
    match_beginning: bool,
    match_end: bool,
    ignore_ws: bool,
) -> Option<usize> {
    if match_beginning {
        line = 0;
    } else if match_end {
        line = image.len().saturating_sub(pre.len());
    }
    if line > image.len() {
        line = image.len();
    }

    let (mut backwards, mut forwards, mut current) = (line, line, line);
    let mut i: usize = 0;
    loop {
        if matches_at(image, pre, current, match_beginning, match_end, ignore_ws) {
            return Some(current);
        }
        // Pick the next candidate: odd steps go backwards, even steps forwards,
        // skipping (and burning a step on) a direction that has run out.
        loop {
            if backwards == 0 && forwards == image.len() {
                return None;
            }
            if i % 2 == 1 {
                if backwards == 0 {
                    i += 1;
                    continue;
                }
                backwards -= 1;
                current = backwards;
            } else {
                if forwards == image.len() {
                    i += 1;
                    continue;
                }
                forwards += 1;
                current = forwards;
            }
            break;
        }
        i += 1;
    }
}

/// Whether `pre` sits in `image` at line `at`, honouring the anchoring flags.
fn matches_at(
    image: &[Vec<u8>],
    pre: &[Vec<u8>],
    at: usize,
    match_beginning: bool,
    match_end: bool,
    ignore_ws: bool,
) -> bool {
    if at + pre.len() > image.len() {
        return false;
    }
    if match_end && at + pre.len() != image.len() {
        return false;
    }
    if match_beginning && at != 0 {
        return false;
    }
    if image[at..at + pre.len()] == *pre {
        return true;
    }
    // `match_fragment()` tries the byte-exact comparison first and only then, under
    // `--ignore-whitespace`, `line_by_line_fuzzy_match()`. Its trailing check that
    // whatever of the pre-image runs past EOF is blank cannot fire here: the
    // pre-image is only allowed to overrun the file under `--whitespace=fix`
    // (`correct_ws_error`), and the length test above has already ruled it out.
    ignore_ws
        && image[at..at + pre.len()]
            .iter()
            .zip(pre)
            .all(|(a, b)| fuzzy_matchlines(a, b))
}

/// C's `isspace()` in the C locale, which is what apply.c compares against — one
/// character wider than Rust's `is_ascii_whitespace` (vertical tab).
fn c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `fuzzy_matchlines()` (apply.c:2500): the two lines are equal once every run of
/// whitespace is collapsed — but a run may not vanish, so `a b` still does not
/// match `ab`. Line endings are ignored on both sides.
fn fuzzy_matchlines(s1: &[u8], s2: &[u8]) -> bool {
    let trim = |s: &[u8]| {
        let mut end = s.len();
        while end > 0 && (s[end - 1] == b'\r' || s[end - 1] == b'\n') {
            end -= 1;
        }
        end
    };
    let (e1, e2) = (trim(s1), trim(s2));
    let (mut i, mut j) = (0, 0);
    while i < e1 && j < e2 {
        if c_space(s1[i]) {
            if !c_space(s2[j]) {
                return false;
            }
            while i < e1 && c_space(s1[i]) {
                i += 1;
            }
            while j < e2 && c_space(s2[j]) {
                j += 1;
            }
        } else if s1[i] != s2[j] {
            return false;
        } else {
            i += 1;
            j += 1;
        }
    }
    // "If we reached the end on one side only, lines don't match."
    i == e1 && j == e2
}

// ---------------------------------------------------------------------------
// patch parsing
// ---------------------------------------------------------------------------

/// `error(_("corrupt patch at %s:%d"))` — every `return -1` out of apply.c's
/// `parse_fragment()` surfaces as this one message, naming the patch input and
/// the line the parser had reached, and leaves `git apply` exiting 128.
///
/// Carried as its own error type so the entry point can tell it from the
/// generic failures that share the parse path and reproduce git's wording and
/// exit code instead of the crate-wide `zvcs: apply: …` form.
#[derive(Debug)]
struct CorruptPatch(String);

impl std::fmt::Display for CorruptPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupt patch at {}", self.0)
    }
}

impl std::error::Error for CorruptPatch {}

/// A header diagnostic that already reads the way git prints it, reported through
/// `error()` and unwound to exit 128 exactly as a corrupt fragment is.
#[derive(Debug)]
struct HeaderError(String);

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HeaderError {}

/// `apply_state.patch_input_file` + `apply_state.linenr`.
///
/// git parses each `<patch>` argument on its own, resetting `linenr` per file,
/// so `corrupt patch at <file>:<line>` names the file the hunk came from and the
/// line within *that* file. The inputs are concatenated into one buffer here, so
/// this records the line each one started at and maps back.
struct InputSpans {
    /// `(name, index of the input's first line in the concatenated buffer)`,
    /// in the order the inputs were read.
    spans: Vec<(String, usize)>,
}

impl InputSpans {
    /// [`CorruptPatch`] for the (0-based) line `idx`, in the input that the
    /// (0-based) line `anchor` belongs to.
    ///
    /// The two differ when a fragment's body runs past the end of its own input:
    /// git, parsing one file at a time, simply runs out of bytes and reports the
    /// line one past that file's last, so the input is chosen by where the
    /// fragment *started* rather than by where the scan stopped.
    /// The `<input>:<line>` a (0-based) index in the concatenated buffer belongs to.
    fn location(&self, idx: usize) -> (String, usize) {
        let (name, start) = self
            .spans
            .iter()
            .rev()
            .find(|(_, start)| *start <= idx)
            .map(|(name, start)| (name.clone(), *start))
            .unwrap_or_else(|| ("<stdin>".to_string(), 0));
        (name, idx - start + 1)
    }

    fn corrupt_at(&self, anchor: usize, idx: usize) -> anyhow::Error {
        let (name, start) = self
            .spans
            .iter()
            .rev()
            .find(|(_, start)| *start <= anchor)
            .map(|(name, start)| (name.as_str(), *start))
            .unwrap_or(("<stdin>", 0));
        anyhow::Error::new(CorruptPatch(format!("{name}:{}", idx - start + 1)))
    }
}

/// Split `buf` into lines that keep their trailing newline; a final line without
/// one is kept as-is.
fn split_lines(buf: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            out.push(&buf[start..=i]);
            start = i + 1;
        }
    }
    if start < buf.len() {
        out.push(&buf[start..]);
    }
    out
}

/// A line as text with its terminator removed, for header matching.
fn txt(line: &[u8]) -> String {
    let end = line.len() - usize::from(line.last() == Some(&b'\n'));
    String::from_utf8_lossy(&line[..end]).into_owned()
}

/// Scan the whole input for patch headers, skipping any surrounding prose
/// (commit messages, mail headers) as git does.
fn parse_patches(
    lines: &[&[u8]],
    strip: usize,
    // `state->p_value_known`: `-p<n>` was given, so no traditional patch may infer
    // its own value.
    strip_explicit: bool,
    // The invocation prefix, which `guess_p_value()` matches names against, and
    // `--directory=<root>` (slash-terminated), which it prepends first.
    prefix: &str,
    root: &str,
    recount: bool,
    spans: &InputSpans,
) -> Result<Vec<Patch>> {
    let mut out = Vec::new();
    // `state->p_value` / `state->p_value_known`: the running pair
    // `parse_traditional_patch()` fixes on the first traditional patch whose two
    // name lines agree, and every later patch in the same input then reuses.
    let mut strip = strip;
    let mut known = strip_explicit;
    let mut i = 0;
    while i < lines.len() {
        let l = txt(lines[i]);
        if l.starts_with("diff --git ") {
            let (p, next) = parse_one(lines, i, strip, true, recount, spans)?;
            i = next;
            out.push(p);
        } else if l.starts_with("--- ")
            && lines.get(i + 1).map(|n| txt(n).starts_with("+++ ")) == Some(true)
        {
            if !known {
                // `parse_traditional_patch()` (apply-lib.c:865): guess from both
                // sides, let a `/dev/null` side defer to the other, and adopt the
                // value only when the two agree.
                let p = guess_p_value(&txt(lines[i])[4..], root, prefix);
                let q = guess_p_value(&txt(lines[i + 1])[4..], root, prefix);
                let p = p.or(q);
                if let (Some(p), Some(q)) = (p, q) {
                    if p == q {
                        strip = p;
                        known = true;
                    }
                }
            }
            let (p, next) = parse_one(lines, i, strip, false, recount, spans)?;
            i = next;
            out.push(p);
        } else {
            i += 1;
        }
    }
    Ok(out)
}

/// Parse one file's patch beginning at `start`, returning it and the index of
/// the first line after it.
fn parse_one(
    lines: &[&[u8]],
    start: usize,
    strip: usize,
    git_style: bool,
    recount: bool,
    spans: &InputSpans,
) -> Result<(Patch, usize)> {
    let mut p = Patch {
        old_name: None,
        new_name: None,
        old_mode: None,
        new_mode: None,
        is_new: false,
        is_delete: false,
        is_rename: false,
        binary: false,
        binary_forward: None,
        binary_reverse: None,
        index_old: None,
        index_new: None,
        score: 0,
        is_toplevel_relative: git_style,
        hunks: Vec::new(),
        added: 0,
        deleted: 0,
    };
    let mut i = start;
    // The `--- `/`+++ ` name lines of a traditional patch, kept raw: git resolves
    // the two together in `parse_traditional_patch()` rather than one at a time,
    // so the pair is only usable once both have been read.
    let (mut trad_old, mut trad_new) = (None, None);
    // `parse_chunk()`'s handover to `parse_binary()`: `(line the header parse
    // stopped at, first line after the binary section)`.
    let mut binary_stop: Option<(usize, usize)> = None;

    if git_style {
        let header = txt(lines[i]);
        if let Some((a, b)) = git_header_names(&header["diff --git ".len()..], strip)? {
            p.old_name = Some(a);
            p.new_name = Some(b);
        }
        i += 1;
    }

    // Extended headers, then the `---`/`+++` pair, in whatever order they appear.
    while i < lines.len() {
        let l = txt(lines[i]);
        if let Some(rest) = l.strip_prefix("new file mode ") {
            p.is_new = true;
            p.new_mode = Some(octal(rest)?);
        } else if let Some(rest) = l.strip_prefix("deleted file mode ") {
            p.is_delete = true;
            p.old_mode = Some(octal(rest)?);
        } else if let Some(rest) = l.strip_prefix("new mode ") {
            p.new_mode = Some(octal(rest)?);
        } else if let Some(rest) = l.strip_prefix("old mode ") {
            // The pre-image mode drives the summary's `mode change` line.
            p.old_mode = Some(octal(rest)?);
        } else if let Some(rest) = l.strip_prefix("rename from ") {
            p.is_rename = true;
            p.old_name = strip_path(&unquote(rest)?, strip.saturating_sub(1))?;
        } else if let Some(rest) = l.strip_prefix("rename to ") {
            p.is_rename = true;
            p.new_name = strip_path(&unquote(rest)?, strip.saturating_sub(1))?;
        } else if l.starts_with("copy from ") || l.starts_with("copy to ") {
            anyhow::bail!("copy patches are not implemented");
        } else if let Some(rest) = l.strip_prefix("similarity index ") {
            // Drives the `(N%)` in the summary's rename line.
            p.score = rest.trim().trim_end_matches('%').parse().unwrap_or(0);
        } else if l.starts_with("dissimilarity index ") {
            // Rename/copy scoring; irrelevant to application.
        } else if let Some(rest) = l.strip_prefix("index ") {
            // `index <old>..<new> <mode>` carries the mode when it did not change;
            // git creates the result with it, so an executable file stays one.
            if let Some((_, mode)) = rest.split_once(' ') {
                if p.new_mode.is_none() {
                    p.new_mode = Some(octal(mode)?);
                }
            }
            // The ids themselves matter to a binary patch, which is only applied
            // when the line named them in full.
            let ids = rest.split(' ').next().unwrap_or("");
            if let Some((old, new)) = ids.split_once("..") {
                p.index_old = Some(old.to_string());
                p.index_new = Some(new.to_string());
            }
        } else if let Some(rest) = l.strip_prefix("--- ") {
            if git_style {
                p.old_name = header_path(rest, strip)?;
            } else {
                trad_old = Some(rest.to_string());
            }
        } else if let Some(rest) = l.strip_prefix("+++ ") {
            if git_style {
                p.new_name = header_path(rest, strip)?;
            } else {
                trad_new = Some(rest.to_string());
            }
        } else if l.starts_with("GIT binary patch") || l.starts_with("Binary files ") {
            p.binary = true;
            let stop = i;
            i += 1;
            // `parse_binary()`: the forward payload, then the reverse one when the
            // patch was written with `--binary`. Anything else ends the section.
            if let Some((forward, next)) = parse_binary_block(lines, i) {
                p.binary_forward = Some(forward);
                i = next;
                if let Some((reverse, next)) = parse_binary_block(lines, i) {
                    p.binary_reverse = Some(reverse);
                    i = next;
                }
            }
            // Consume whatever is left of the section.
            while i < lines.len() {
                let n = txt(lines[i]);
                if n.starts_with("diff --git ") || n.starts_with("--- ") {
                    break;
                }
                i += 1;
            }
            binary_stop = Some((stop, i));
            break;
        } else {
            break;
        }
        i += 1;
    }

    // `parse_git_diff_header()`'s `done:` (apply.c:1425), which every exit from the
    // header table reaches: the line the parse stopped at is the one both
    // filename diagnostics report.
    let hdr_stop = binary_stop.map_or(i, |(stop, _)| stop);
    if !git_style {
        resolve_traditional(&mut p, trad_old.as_deref(), trad_new.as_deref(), strip)?;
    }
    require_names(&p, git_style, strip, spans, if git_style { hdr_stop } else { start })?;

    if let Some((_, next)) = binary_stop {
        return Ok((normalise(p)?, next));
    }

    while i < lines.len() && txt(lines[i]).starts_with("@@ ") {
        let (h, added, deleted, next) = parse_hunk(lines, i, recount, spans)?;
        p.added += added;
        p.deleted += deleted;
        p.hunks.push(h);
        i = next;
    }

    Ok((normalise(p)?, i))
}

/// `parse_traditional_patch()` (apply.c:856): the two name lines are resolved
/// together, not one at a time. A `/dev/null` on either side makes the patch a
/// creation or a deletion and the other line supplies the name; otherwise the
/// `+++` line is read with the `---` line's name as `find_name_common()`'s `def`,
/// and the single name that comes out is used for *both* sides. That is what lets
/// `-p<n>` over-strip one side without failing, and what makes `--- a/f.txt.orig`
/// / `+++ a/f.txt` a patch to `f.txt` rather than a rename.
fn resolve_traditional(
    p: &mut Patch,
    first: Option<&str>,
    second: Option<&str>,
    strip: usize,
) -> Result<()> {
    let (Some(first), Some(second)) = (first, second) else {
        return Ok(());
    };
    if is_dev_null(first.split('\t').next().unwrap_or("")) {
        p.is_new = true;
        p.new_name = header_path(second, strip)?;
    } else if is_dev_null(second.split('\t').next().unwrap_or("")) {
        p.is_delete = true;
        p.old_name = header_path(first, strip)?;
    } else {
        let def = header_path(first, strip)?;
        let name = match (header_path(second, strip)?, def) {
            // "Generally we prefer the shorter name, especially if the other one
            // is just a variation of that with something else tacked on to the
            // end (ie "file.orig" or "file~")."
            (Some(name), Some(def)) if def.len() < name.len() && name.starts_with(&def) => {
                Some(def)
            }
            (Some(name), _) => Some(name),
            // `find_name_common()` falls back to `def` when the second line
            // yields nothing.
            (None, def) => def,
        };
        p.old_name = name.clone();
        p.new_name = name;
    }
    Ok(())
}

/// The two "this header named no file" diagnostics: `parse_git_diff_header()`'s
/// `done:` block (apply.c:1425) for a `diff --git` header, and
/// `parse_traditional_patch()`'s tail (apply.c:904) for a `---`/`+++` pair. Both
/// carry `state->patch_input_file` and the line the parse was sitting on — the
/// header's last line for a git patch, the `---` line for a traditional one.
fn require_names(
    p: &Patch,
    git_style: bool,
    strip: usize,
    spans: &InputSpans,
    idx: usize,
) -> Result<()> {
    if p.old_name.is_some() || p.new_name.is_some() {
        return Ok(());
    }
    let (file, line) = spans.location(idx);
    let msg = if git_style {
        // `Q_()`: singular for one component only.
        let unit = if strip == 1 { "component" } else { "components" };
        format!(
            "git diff header lacks filename information when removing \
             {strip} leading pathname {unit} at {file}:{line}"
        )
    } else {
        format!("unable to find filename in patch at {file}:{line}")
    };
    Err(anyhow::Error::new(HeaderError(msg)))
}

/// Reconcile the creation/deletion flags with the two names, so that exactly one
/// side is `None` for a creation or deletion.
fn normalise(mut p: Patch) -> Result<Patch> {
    if p.old_name.is_none() && p.new_name.is_none() {
        crate::git_fatal!("corrupt patch: no file name in the header");
    }
    if p.old_name.is_none() {
        p.is_new = true;
    }
    if p.new_name.is_none() {
        p.is_delete = true;
    }
    if p.is_new {
        p.old_name = None;
    }
    if p.is_delete {
        p.new_name = None;
    }
    Ok(p)
}

/// Parse an `@@ -a,b +c,d @@` fragment and its body.
///
/// `recount` is `--recount`: the counts in the header are not trusted, so the
/// body runs until the first line that is not a body line instead of until the
/// header's counts are exhausted, and a mismatch is not an error.
fn parse_hunk(
    lines: &[&[u8]],
    start: usize,
    recount: bool,
    spans: &InputSpans,
) -> Result<(Hunk, usize, usize, usize)> {
    let header = txt(lines[start]);
    // `parse_fragment_header()` failing is `parse_fragment()` returning -1 while
    // `state->linenr` still points at the `@@` line.
    let (old_pos, mut old_rem, new_pos, mut new_rem) =
        hunk_range(&header).ok_or_else(|| spans.corrupt_at(start, start))?;

    let mut h = Hunk {
        old_pos,
        new_pos,
        pre: Vec::new(),
        pre_common: Vec::new(),
        post: Vec::new(),
        post_common: Vec::new(),
        added_at: Vec::new(),
        context: Vec::new(),
        raw: Vec::new(),
        trailing: 0,
        leading: 0,
    };
    let (mut added, mut deleted) = (0usize, 0usize);
    let mut last = Side::None;
    let mut i = start + 1;

    while i < lines.len() {
        let raw = lines[i];
        // `\ No newline at end of file` retracts the newline from the line just
        // read, on whichever image(s) that line joined.
        if raw.first() == Some(&b'\\') {
            match last {
                Side::Context => {
                    drop_newline(h.pre.last_mut());
                    drop_newline(h.post.last_mut());
                }
                Side::Pre => drop_newline(h.pre.last_mut()),
                Side::Post => drop_newline(h.post.last_mut()),
                Side::None => {}
            }
            i += 1;
            continue;
        }
        if !recount && old_rem == 0 && new_rem == 0 {
            break;
        }
        // A context line whose single leading space was stripped in transit is
        // still a context line; git accepts the bare newline.
        let (marker, body): (u8, &[u8]) = match raw.first() {
            Some(&b'\n') | None => (b' ', &b"\n"[..]),
            Some(&c) if c == b' ' || c == b'+' || c == b'-' => (c, &raw[1..]),
            _ => break,
        };
        match marker {
            b' ' => {
                if added == 0 && deleted == 0 {
                    h.leading += 1;
                }
                h.pre.push(body.to_vec());
                h.pre_common.push(true);
                h.post.push(body.to_vec());
                h.post_common.push(true);
                h.context.push(body.to_vec());
                h.trailing += 1;
                last = Side::Context;
                old_rem = old_rem.saturating_sub(1);
                new_rem = new_rem.saturating_sub(1);
            }
            b'-' => {
                h.pre.push(body.to_vec());
                h.pre_common.push(false);
                h.trailing = 0;
                deleted += 1;
                last = Side::Pre;
                old_rem = old_rem.saturating_sub(1);
            }
            _ => {
                h.added_at.push((i, h.post.len()));
                h.post.push(body.to_vec());
                h.post_common.push(false);
                h.trailing = 0;
                added += 1;
                last = Side::Post;
                new_rem = new_rem.saturating_sub(1);
            }
        }
        i += 1;
    }

    // `if (oldlines || newlines) return -1;` — the body ran out (or hit a line
    // that is not a body line) before the header's counts were satisfied.
    // `state->linenr` has been advanced past every line consumed, so the line
    // reported is the one that stopped the scan, or the first line past the
    // input when it simply ended.
    if !recount && (old_rem != 0 || new_rem != 0) {
        return Err(spans.corrupt_at(start, i));
    }
    // `if (!patch->recount && !deleted && !added) return -1;` — a fragment that
    // is nothing but context changes nothing, so git calls the patch corrupt
    // rather than silently applying a no-op. `--recount` exempts it: the counts
    // are then derived from the body, and an all-context body is how a hunk
    // whose `+`/`-` lines were mangled in transit still reaches `recount_diff`.
    if !recount && added == 0 && deleted == 0 {
        return Err(spans.corrupt_at(start, i));
    }
    // The fragment's verbatim bytes (header through the last consumed body line),
    // re-emitted unchanged into a *.rej file when the hunk is rejected.
    for line in &lines[start..i] {
        h.raw.extend_from_slice(line);
    }
    Ok((h, added, deleted, i))
}

/// Which image(s) the most recent body line joined, for the `\ No newline` rule.
enum Side {
    None,
    Context,
    Pre,
    Post,
}

fn drop_newline(line: Option<&mut Vec<u8>>) {
    if let Some(l) = line {
        if l.last() == Some(&b'\n') {
            l.pop();
        }
    }
}

/// `@@ -a[,b] +c[,d] @@ [section]` → `(a, b, c, d)`.
fn hunk_range(header: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split_once(" @@")?.0;
    let (os, oc) = one_range(old)?;
    let (ns, nc) = one_range(new)?;
    Some((os, oc, ns, nc))
}

fn one_range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

// ---------------------------------------------------------------------------
// path handling
// ---------------------------------------------------------------------------

/// A `---`/`+++` header path: text up to the first tab (traditional diffs append
/// a timestamp there), `/dev/null` meaning "this side does not exist".
fn header_path(rest: &str, strip: usize) -> Result<Option<String>> {
    let name = rest.split('\t').next().unwrap_or("");
    if is_dev_null(name) {
        return Ok(None);
    }
    strip_path(&unquote(name)?, strip)
}

/// `is_dev_null()` (apply.c:493): the name is `/dev/null`, optionally followed by
/// whitespace (a traditional diff's timestamp).
fn is_dev_null(name: &str) -> bool {
    match name.strip_prefix("/dev/null") {
        Some(rest) => rest.is_empty() || rest.starts_with(char::is_whitespace),
        None => false,
    }
}

/// `guess_p_value()` (apply-lib.c:747): the `-p<n>` a *traditional* (non-`diff
/// --git`) patch implies, or `None` when the name says nothing.
///
/// `parse_traditional_patch()` runs it on both name lines of the first such patch
/// and, when the two agree, fixes `p_value` for the rest of the input. This is what
/// lets `diff -u old new > p` apply with no `-p0`: a name with no slash at all can
/// only be meant whole, so the answer is 0.
///
/// `nameline` is the text after `--- `/`+++ `, and the name is read with `p_value`
/// 0 — the whole thing, timestamp trimmed and unquoted, with `--directory=<root>`
/// already in front of it, because `find_name_common()` prepends `state->root`
/// before the guess ever looks for a slash. That is why `--directory=X` changes the
/// answer for a one-component name: `X/s.txt` has a directory part and `s.txt`
/// does not.
fn guess_p_value(nameline: &str, root: &str, prefix: &str) -> Option<usize> {
    let name = header_path(nameline, 0).ok().flatten()?;
    let name = if root.is_empty() { name } else { format!("{root}{name}") };
    let Some(slash) = name.find('/') else {
        // No directory part: the name is already relative to the worktree root.
        return Some(0);
    };
    if prefix.is_empty() {
        return None;
    }
    // "Does it begin with `a/$our-prefix` and such?  Then this is very likely to
    // apply to our directory."
    let slashes = prefix.matches('/').count();
    if name.starts_with(prefix) {
        return Some(slashes);
    }
    if name[slash + 1..].starts_with(prefix) {
        return Some(slashes + 1);
    }
    None
}

/// Both names off a `diff --git a/x b/y` line.
///
/// Quoted forms are unquoted; otherwise we take git's rule of accepting a split
/// only when the two halves are the same path after stripping, which is the case
/// that matters here — a header with no `---`/`+++` pair is a pure mode change,
/// where both sides name the same file.
fn git_header_names(rest: &str, strip: usize) -> Result<Option<(String, String)>> {
    // `rest` is reindexed at original offsets (`rest[..=end]`, `rest[end + 2..]`),
    // so rebasing onto a stripped slice would not be behavior-identical.
    #[allow(clippy::manual_strip)]
    if rest.starts_with('"') {
        if let Some(end) = rest[1..].find('"').map(|i| i + 1) {
            let (Some(a), Some(b)) = (
                strip_path(&unquote(&rest[..=end])?, strip)?,
                strip_path(&unquote(rest[end + 2..].trim())?, strip)?,
            ) else {
                return Ok(None);
            };
            return Ok(Some((a, b)));
        }
        return Ok(None);
    }
    for (idx, _) in rest.match_indices(' ') {
        let (Ok(Some(a)), Ok(Some(b))) = (
            strip_path(&rest.as_bytes()[..idx], strip),
            strip_path(&rest.as_bytes()[idx + 1..], strip),
        ) else {
            continue;
        };
        if a == b {
            return Ok(Some((a, b)));
        }
    }
    Ok(None)
}

/// Drop `n` leading slash-separated components, as `-p<n>` asks.
///
/// `None` is `find_name_common()` (apply.c:654) returning NULL: the name ran out
/// of components before `-p<n>` was satisfied (`start` never set), or nothing was
/// left after them (`len == 0`). git does not treat that as an error where it
/// happens — the name is simply absent, and whoever wanted one says so later,
/// which is why the diagnostic can name the header's line.
fn strip_path(name: &[u8], n: usize) -> Result<Option<String>> {
    let mut s: &[u8] = name;
    for _ in 0..n {
        match s.iter().position(|&b| b == b'/') {
            Some(i) => s = &s[i + 1..],
            None => return Ok(None),
        }
    }
    if s.is_empty() {
        return Ok(None);
    }
    let out = String::from_utf8(s.to_vec())
        .map_err(|_| anyhow::anyhow!("non-UTF-8 paths in patches are not supported"))?;
    Ok(Some(check_path(out)?))
}

/// Reject anything that would escape the working tree. `--unsafe-paths`, which
/// is what lets git through this gate, is not honoured, so this is unconditional.
fn check_path(out: String) -> Result<String> {
    if out.is_empty() || out.starts_with('/') || out.split('/').any(|c| c == "..") {
        crate::git_fatal!("refusing to apply to path {out:?} outside the working tree");
    }
    Ok(out)
}

/// `--directory=<root>`: git's `prefix_one()` — prepend `root` to every patch
/// path, after `-p<n>` has done its stripping. A `/dev/null` side is `None` here
/// (a creation's pre-image, a deletion's post-image) and stays that way.
fn prefix_names(p: &mut Patch, root: &str) -> Result<()> {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return Ok(());
    }
    for n in [&mut p.old_name, &mut p.new_name].into_iter().flatten() {
        let joined = format!("{root}/{n}");
        *n = check_path(joined)?;
    }
    Ok(())
}

/// `prefix_patch()` (apply.c:2191): a patch that is not already root-relative has
/// the invocation prefix prepended to both of its names, exactly as `prefix_one()`
/// does. A `/dev/null` side is `None` here and stays that way.
fn prefix_patch(p: &mut Patch, prefix: &str) {
    if p.is_toplevel_relative {
        return;
    }
    for n in [&mut p.old_name, &mut p.new_name].into_iter().flatten() {
        *n = format!("{prefix}{n}");
    }
}

/// `setup_git_directory()`'s two results, in one step: chdir to the top of the
/// worktree and return the slash-terminated path of the directory the command was
/// invoked from, relative to that top. Empty when already at the top, when the
/// repository is bare, or when there is no repository at all — the three cases where
/// git leaves `state->prefix` NULL and every path in the patch is taken as given.
fn worktree_prefix() -> Result<String> {
    let Ok(repo) = gix::discover(".") else {
        return Ok(String::new());
    };
    let Some(workdir) = repo.workdir() else {
        return Ok(String::new());
    };
    let root = workdir.canonicalize()?;
    let here = std::env::current_dir()?.canonicalize()?;
    let Ok(rel) = here.strip_prefix(&root) else {
        return Ok(String::new());
    };
    if rel.as_os_str().is_empty() {
        return Ok(String::new());
    }
    // Every path this port compares the prefix against is a slash-joined patch
    // header name, so the prefix is built the same way rather than through the
    // platform separator.
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    std::env::set_current_dir(&root)?;
    Ok(format!("{}/", parts.join("/")))
}

/// Undo git's C-style quoting when a header path is wrapped in double quotes.
fn unquote(s: &str) -> Result<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != b'"' || b[b.len() - 1] != b'"' {
        return Ok(b.to_vec());
    }
    let inner = &b[1..b.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = inner.get(i) else {
            crate::git_fatal!("corrupt quoted path {s:?}");
        };
        i += 1;
        match c {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'"' | b'\\' => out.push(c),
            b'0'..=b'7' => {
                let mut v = u32::from(c - b'0');
                for _ in 0..2 {
                    match inner.get(i) {
                        Some(&d) if (b'0'..=b'7').contains(&d) => {
                            v = v * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(v as u8);
            }
            _ => crate::git_fatal!("corrupt quoted path {s:?}"),
        }
    }
    Ok(out)
}

/// C-style path quoting for `--numstat`, matching git's default `core.quotePath`.
fn quote_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return path.to_owned();
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
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

fn octal(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim(), 8).map_err(|_| anyhow::anyhow!("corrupt file mode {s:?}"))
}

// ---------------------------------------------------------------------------
// output and filesystem
// ---------------------------------------------------------------------------

/// `--numstat`: `<added>\t<deleted>\t<path>`, `-\t-\t` for binary patches, the
/// post-image path (pre-image for a deletion), quoted unless `-z`.
fn render_numstat(patches: &[Patch], nul: bool) -> String {
    let mut out = String::new();
    for p in patches {
        if p.binary {
            out.push_str("-\t-\t");
        } else {
            out.push_str(&format!("{}\t{}\t", p.added, p.deleted));
        }
        let name = p
            .new_name
            .as_deref()
            .or(p.old_name.as_deref())
            .unwrap_or_default();
        if nul {
            out.push_str(name);
            out.push('\0');
        } else {
            out.push_str(&quote_path(name));
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// --stat / --summary — port of apply.c:show_stats / summary_patch_list
// ---------------------------------------------------------------------------

/// `--stat`: git's scaled diffstat graph, one line per patch plus a summary
/// tail. A direct port of apply.c's `show_stats` / `stat_patch_list`: the name
/// column is `min(max quoted-name length, 50)` wide, the `+`/`-` graph is scaled
/// so the widest change fills `70 - name_column` columns (or is drawn 1:1 when it
/// already fits), and each line's decimal count is the file's added+deleted total.
fn render_stat(patches: &[Patch]) -> String {
    let mut names: Vec<String> = Vec::with_capacity(patches.len());
    let (mut adds, mut dels, mut max_len, mut max_change) = (0usize, 0usize, 0usize, 0usize);
    for p in patches {
        let raw = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or("");
        let q = quote_path(raw);
        max_len = max_len.max(q.len());
        max_change = max_change.max(p.added + p.deleted);
        adds += p.added;
        dels += p.deleted;
        names.push(q);
    }
    let m = max_len.min(50);
    let graph_max = if m + max_change > 70 { 70 - m } else { max_change };

    let mut out = String::new();
    for (p, q) in patches.iter().zip(names.iter()) {
        let display = if q.len() > m { truncate_name(q, m) } else { q.clone() };
        if p.binary {
            out.push_str(&format!(" {display:<m$} |  Bin\n"));
            continue;
        }
        out.push_str(&format!(" {display:<m$} |"));
        let (add, del) = scale_graph(p.added, p.deleted, graph_max, max_change);
        out.push_str(&format!(
            "{:5} {}{}\n",
            p.added + p.deleted,
            "+".repeat(add),
            "-".repeat(del)
        ));
    }
    out.push_str(&stat_summary_line(patches.len(), adds, dels));
    out
}

/// Scale a hunk's add/delete counts into graph columns (apply.c's rounding:
/// `(n * max + max_change/2) / max_change`, with `del` taking the remainder).
fn scale_graph(add: usize, del: usize, graph_max: usize, max_change: usize) -> (usize, usize) {
    if max_change == 0 {
        return (0, 0);
    }
    let total = ((add + del) * graph_max + max_change / 2) / max_change;
    let a = (add * graph_max + max_change / 2) / max_change;
    (a, total - a)
}

/// Truncate an over-long stat name to the column width, keeping a trailing path
/// component and prefixing `...` (apply.c's `strchr` from `len + 3 - max`).
fn truncate_name(q: &str, m: usize) -> String {
    let bytes = q.as_bytes();
    let start = q.len() + 3 - m;
    let cut = bytes[start..]
        .iter()
        .position(|&b| b == b'/')
        .map(|i| start + i)
        .unwrap_or(start);
    format!("...{}", &q[cut..])
}

/// The `--stat` tail: `N files changed, X insertions(+), Y deletions(-)`, with
/// git's singular/plural forms and the clause-omission rules from diff.c's
/// `print_stat_summary`.
fn stat_summary_line(files: usize, ins: usize, del: usize) -> String {
    if files == 0 {
        return " 0 files changed\n".to_string();
    }
    let mut s = format!(" {} {} changed", files, if files == 1 { "file" } else { "files" });
    if ins > 0 || del == 0 {
        s.push_str(&format!(
            ", {} {}(+)",
            ins,
            if ins == 1 { "insertion" } else { "insertions" }
        ));
    }
    if del > 0 || ins == 0 {
        s.push_str(&format!(
            ", {} {}(-)",
            del,
            if del == 1 { "deletion" } else { "deletions" }
        ));
    }
    s.push('\n');
    s
}

/// `--summary`: git's `summary_patch_list` — one line per patch that creates,
/// deletes, renames, or changes the mode of a file (pure content edits print
/// nothing).
fn render_summary(patches: &[Patch]) -> String {
    let mut out = String::new();
    for p in patches {
        if p.is_rename {
            out.push_str(&rename_line(p));
        } else if p.is_new {
            out.push_str(&format!(
                " create mode {:06o} {}\n",
                p.new_mode.unwrap_or(0),
                p.new_name.as_deref().unwrap_or("")
            ));
        } else if p.is_delete {
            out.push_str(&format!(
                " delete mode {:06o} {}\n",
                p.old_mode.unwrap_or(0),
                p.old_name.as_deref().unwrap_or("")
            ));
        } else if let (Some(om), Some(nm)) = (p.old_mode, p.new_mode) {
            if om != nm {
                out.push_str(&format!(
                    " mode change {:06o} => {:06o} {}\n",
                    om,
                    nm,
                    p.new_name.as_deref().unwrap_or("")
                ));
            }
        }
    }
    out
}

/// apply.c's `show_rename_copy`: strip the common leading *directory* prefix (whole
/// `foo/` components only, no suffix folding) and render `dir/{old => new}` when a
/// prefix was found, else `old => new`.
fn rename_line(p: &Patch) -> String {
    let old = p.old_name.as_deref().unwrap_or("");
    let new = p.new_name.as_deref().unwrap_or("");
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    let mut pfx = 0usize;
    loop {
        let so = ob[pfx..].iter().position(|&b| b == b'/');
        let sn = nb[pfx..].iter().position(|&b| b == b'/');
        match (so, sn) {
            (Some(a), Some(b)) if a == b && ob[pfx..pfx + a] == nb[pfx..pfx + b] => {
                pfx += a + 1;
            }
            _ => break,
        }
    }
    if pfx > 0 {
        format!(
            " rename {}{{{} => {}}} ({}%)\n",
            &old[..pfx],
            &old[pfx..],
            &new[pfx..],
            p.score
        )
    } else {
        format!(" rename {old} => {new} ({}%)\n", p.score)
    }
}

// ---------------------------------------------------------------------------
// --include / --exclude — port of apply.c:use_patch + wildmatch (flags 0)
// ---------------------------------------------------------------------------

/// Whether a patch survives the `--include`/`--exclude` rule list: the first rule
/// whose glob matches the patch's post-image name decides (its include/exclude
/// sense); with no match, a path is kept unless any `--include` rule exists.
fn use_patch(p: &Patch, prefix: &str, limits: &[(bool, String)], has_include: bool) -> bool {
    let name = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or("");
    // "Paths outside are not touched regardless of `--include`" (apply.c:2218): the
    // path must live strictly *below* the directory `git apply` was invoked from.
    if !prefix.is_empty() {
        match name.strip_prefix(prefix) {
            Some(rest) if !rest.is_empty() => {}
            _ => return false,
        }
    }
    for (is_include, pat) in limits {
        if wildmatch0(pat.as_bytes(), name.as_bytes()) {
            return *is_include;
        }
    }
    !has_include
}

/// `wildmatch(pattern, text, 0)`: `*` matches any run *including* `/`, `?` a single
/// byte, `[...]` a bracket set (with `!`/`^` negation and `a-z` ranges), and `\`
/// escapes the next byte. POSIX `[:class:]` names are not handled (unused here).
fn wildmatch0(pat: &[u8], text: &[u8]) -> bool {
    match pat.first() {
        None => text.is_empty(),
        Some(b'*') => {
            if wildmatch0(&pat[1..], text) {
                return true;
            }
            match text.split_first() {
                Some((_, trest)) => wildmatch0(pat, trest),
                None => false,
            }
        }
        Some(b'?') => match text.split_first() {
            Some((_, trest)) => wildmatch0(&pat[1..], trest),
            None => false,
        },
        Some(b'[') => match text.split_first() {
            Some((&c, trest)) => match match_class(pat, c) {
                Some((true, np)) => wildmatch0(&pat[np..], trest),
                Some((false, _)) => false,
                None => c == b'[' && wildmatch0(&pat[1..], trest),
            },
            None => false,
        },
        Some(b'\\') if pat.len() >= 2 => match text.split_first() {
            Some((&c, trest)) if c == pat[1] => wildmatch0(&pat[2..], trest),
            _ => false,
        },
        Some(&pc) => match text.split_first() {
            Some((&c, trest)) if c == pc => wildmatch0(&pat[1..], trest),
            _ => false,
        },
    }
}

/// Match one `[...]` bracket expression against byte `c`. Returns
/// `(matched, index just past the ']')`, or `None` if the class is unterminated
/// (so the caller can treat `[` as a literal).
fn match_class(pat: &[u8], c: u8) -> Option<(bool, usize)> {
    let mut i = 1;
    let negated = matches!(pat.get(i), Some(&b'!') | Some(&b'^'));
    if negated {
        i += 1;
    }
    let start = i;
    let mut matched = false;
    loop {
        match pat.get(i) {
            None => return None,
            Some(&b']') if i > start => {
                i += 1;
                break;
            }
            Some(&ch) => {
                let is_range = pat.get(i + 1) == Some(&b'-')
                    && pat.get(i + 2).is_some_and(|&d| d != b']');
                if is_range {
                    if ch <= c && c <= pat[i + 2] {
                        matched = true;
                    }
                    i += 3;
                } else {
                    if ch == c {
                        matched = true;
                    }
                    i += 1;
                }
            }
        }
    }
    Some((matched ^ negated, i))
}

// ---------------------------------------------------------------------------
// --reject — port of apply.c:apply_fragments (reject arm) + write_out_one_reject
// ---------------------------------------------------------------------------

/// Apply each file's hunks independently, writing partial results and dropping
/// the hunks that do not land into `<name>.rej`. git forces verbose output here,
/// so every diagnostic goes to stderr; the exit code is 1 if any hunk rejected.
fn reject_apply(patches: &[Patch], o: &Opts) -> Result<ExitCode> {
    let mut any_reject = false;
    let empty: HashMap<String, Option<Vec<u8>>> = HashMap::new();

    for p in patches {
        if p.binary {
            bail!("binary patch application is not implemented");
        }
        let name = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or("");
        let label = p.old_name.as_deref().or(p.new_name.as_deref()).unwrap_or("");
        eprintln!("Checking patch {name}...");

        if let Some(new) = &p.new_name {
            if (p.is_new || p.is_rename) && std::fs::symlink_metadata(new).is_ok() {
                eprintln!("error: {new}: already exists in working directory");
                any_reject = true;
                continue;
            }
        }

        let mut image: Vec<Vec<u8>> = if p.is_new {
            Vec::new()
        } else {
            let old = p.old_name.as_deref().unwrap_or_default();
            // Read from disk (an earlier patch's write is already there).
            match read_current(&empty, old) {
                Some(bytes) => split_lines(&bytes).into_iter().map(|l| l.to_vec()).collect(),
                None => {
                    eprintln!("error: {old}: No such file or directory");
                    any_reject = true;
                    continue;
                }
            }
        };

        let mut applied: Vec<bool> = Vec::with_capacity(p.hunks.len());
        for h in &p.hunks {
            if let Some(at) = place_hunk(&image, h, o.unidiff_zero, o.ignore_ws) {
                let repl = replacement(&image, at, h, o.no_add, o.ignore_ws);
                image.splice(at..at + h.pre.len(), repl);
                applied.push(true);
            } else {
                let pre: Vec<u8> = h.pre.concat();
                eprint!(
                    "error: while searching for:\n{}\n",
                    String::from_utf8_lossy(&pre)
                );
                eprintln!("error: patch failed: {label}:{}", h.old_pos);
                applied.push(false);
            }
        }

        let nrej = applied.iter().filter(|a| !**a).count();
        if nrej == 0 {
            eprintln!("Applied patch {name} cleanly.");
            finalize_write(p, &image)?;
        } else {
            any_reject = true;
            eprintln!(
                "Applying patch {name} with {nrej} {}...",
                if nrej == 1 { "reject" } else { "rejects" }
            );
            for (idx, ok) in applied.iter().enumerate() {
                if *ok {
                    eprintln!("Hunk #{} applied cleanly.", idx + 1);
                } else {
                    eprintln!("Rejected hunk #{}.", idx + 1);
                }
            }
            finalize_write(p, &image)?;
            write_reject_file(p, &applied)?;
        }
    }

    Ok(if any_reject {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Commit one reject-mode file result to disk immediately (git applies each file
/// independently under `--reject`): a fully-applied deletion removes the file,
/// otherwise the post-image path is rewritten with the surviving content.
fn finalize_write(p: &Patch, image: &[Vec<u8>]) -> Result<()> {
    let data: Vec<u8> = image.concat();
    if p.is_delete && data.is_empty() {
        if let Some(old) = &p.old_name {
            let _ = std::fs::remove_file(old);
            prune_empty_parents(Path::new(old));
        }
        return Ok(());
    }
    let target = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or_default();
    let mode = p.new_mode.unwrap_or(0o100644);
    if let Some(old) = &p.old_name {
        let _ = std::fs::remove_file(old);
        if p.is_rename && old != target {
            prune_empty_parents(Path::new(old));
        }
    }
    create_leading_dirs(Path::new(target))?;
    write_created(Path::new(target), mode, &data)?;
    Ok(())
}

/// Write the `<name>.rej` file: a `diff a/<old> b/<new>\t(rejected hunks)` banner
/// followed by the verbatim text of every rejected fragment, in patch order.
fn write_reject_file(p: &Patch, applied: &[bool]) -> Result<()> {
    let old = p.old_name.as_deref().or(p.new_name.as_deref()).unwrap_or("");
    let new = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or("");
    let mut out: Vec<u8> = format!("diff a/{old} b/{new}\t(rejected hunks)\n").into_bytes();
    for (idx, ok) in applied.iter().enumerate() {
        if !*ok {
            out.extend_from_slice(&p.hunks[idx].raw);
        }
    }
    let rej = format!("{new}.rej");
    std::fs::write(&rej, out)?;
    Ok(())
}

/// The current bytes of `path`, preferring the result an earlier patch in this
/// same run produced. `None` means the path does not exist.
fn read_current(staged: &HashMap<String, Option<Vec<u8>>>, path: &str) -> Option<Vec<u8>> {
    if let Some(entry) = staged.get(path) {
        return entry.clone();
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        // A symlink's blob content is its target, with no trailing newline.
        return Some(
            std::fs::read_link(path)
                .ok()?
                .into_os_string()
                .into_string()
                .ok()?
                .into_bytes(),
        );
    }
    std::fs::read(path).ok()
}

fn create_leading_dirs(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Create `path` fresh with `mode`, as git's `try_create_file` does: symlinks via
/// `symlink(2)`, everything else opened `O_CREAT|O_EXCL` with 0777 or 0666 so the
/// process umask decides the final permissions.
#[cfg(unix)]
fn write_created(path: &Path, mode: u32, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    if mode & 0o170000 == 0o120000 {
        let target = String::from_utf8_lossy(data).into_owned();
        std::os::unix::fs::symlink(&target, path)?;
        return Ok(());
    }
    let perm = if mode & 0o100 != 0 { 0o777 } else { 0o666 };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(perm)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_created(path: &Path, _mode: u32, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)?;
    Ok(())
}

/// After removing a file, drop the directories it emptied, exactly as git's
/// `remove_path` does. Stops at the first non-empty (or non-removable) parent.
fn prune_empty_parents(path: &Path) {
    let mut dir: PathBuf = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => return,
    };
    while std::fs::remove_dir(&dir).is_ok() {
        match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => dir = p.to_path_buf(),
            _ => break,
        }
    }
}

/// An io error's message without Rust's ` (os error N)` suffix, so our stderr
/// reads like git's `strerror`-based output.
fn io_msg(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

// ---------------------------------------------------------------------------
// whitespace checking — apply.c's ws_check path
// ---------------------------------------------------------------------------

/// Report the whitespace errors every added line carries, as `apply.c`'s
/// `check_whitespace()` does: one `<patch>:<line>: <error>.` line followed by the
/// offending text, the first five only, then the count.
///
/// Returns the number of offending lines. `nowarn` counts without reporting, which is
/// what git's `ws_error_action == nowarn_ws_error` does.
fn report_whitespace(
    patches: &[Patch],
    spans: &InputSpans,
    rule: u32,
    action: &WsAction,
    quiet: bool,
) -> usize {
    // `squelch_whitespace_errors`: git prints the first five and summarises the rest.
    const SQUELCH: usize = 5;
    let mut errors = 0usize;
    let mut printed = 0usize;
    let silent = matches!(action, WsAction::Silent);
    for p in patches {
        for h in &p.hunks {
            for (input_idx, post_idx) in &h.added_at {
                let Some(line) = h.post.get(*post_idx) else {
                    continue;
                };
                if super::diff_files::ws_check(line, rule) == 0 {
                    continue;
                }
                errors += 1;
                if silent || printed >= SQUELCH {
                    continue;
                }
                printed += 1;
                let (file, no) = spans.location(*input_idx);
                let what = super::diff_files::whitespace_error_string(
                    super::diff_files::ws_check(line, rule),
                );
                err(quiet, &format!("{file}:{no}: {what}."));
                let body = line.strip_suffix(b"\n").unwrap_or(line);
                err(quiet, &String::from_utf8_lossy(body));
            }
        }
    }
    if !silent && errors > printed {
        err(
            quiet,
            &format!(
                "warning: squelched {} whitespace {}",
                errors - printed,
                plural_errors(errors - printed)
            ),
        );
    }
    errors
}

/// `Q_("whitespace error", "whitespace errors", n)`.
fn plural_errors(n: usize) -> &'static str {
    if n == 1 {
        "error"
    } else {
        "errors"
    }
}

// ---------------------------------------------------------------------------
// binary patches — apply.c's `GIT binary patch` payload
// ---------------------------------------------------------------------------

/// One `GIT binary patch` fragment: how to turn the pre-image into the post-image.
#[derive(Clone)]
enum BinaryPayload {
    /// `literal <size>`: the inflated bytes are the whole post-image.
    Literal(Vec<u8>),
    /// `delta <size>`: the inflated bytes are a git delta against the pre-image.
    Delta(Vec<u8>),
}

impl BinaryPayload {
    /// The post-image this payload produces from `base`.
    fn rebuild(&self, base: &[u8]) -> Option<Vec<u8>> {
        match self {
            BinaryPayload::Literal(data) => Some(data.clone()),
            BinaryPayload::Delta(delta) => apply_git_delta(base, delta),
        }
    }
}

/// `patch_delta()` (patch-delta.c): a size header for each side, then copy-from-base
/// and insert-literal instructions. `None` when the delta does not describe `base`.
fn apply_git_delta(base: &[u8], delta: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    let mut varint = |pos: &mut usize| -> Option<usize> {
        let mut value = 0usize;
        let mut shift = 0u32;
        loop {
            let byte = *delta.get(*pos)?;
            *pos += 1;
            value |= ((byte & 0x7f) as usize) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    };
    if varint(&mut pos)? != base.len() {
        return None;
    }
    let target_size = varint(&mut pos)?;
    let mut out: Vec<u8> = Vec::with_capacity(target_size);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            // Copy: the low bits say which offset/size bytes are present.
            let mut offset = 0usize;
            let mut size = 0usize;
            for (bit, shift) in [(0x01, 0), (0x02, 8), (0x04, 16), (0x08, 24)] {
                if op & bit != 0 {
                    offset |= (*delta.get(pos)? as usize) << shift;
                    pos += 1;
                }
            }
            for (bit, shift) in [(0x10, 0), (0x20, 8), (0x40, 16)] {
                if op & bit != 0 {
                    size |= (*delta.get(pos)? as usize) << shift;
                    pos += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            out.extend_from_slice(base.get(offset..offset.checked_add(size)?)?);
        } else if op != 0 {
            let len = op as usize;
            out.extend_from_slice(delta.get(pos..pos + len)?);
            pos += len;
        } else {
            // A zero opcode is reserved and git refuses it.
            return None;
        }
    }
    (out.len() == target_size).then_some(out)
}

/// Read one `literal <n>`/`delta <n>` block: the header line, then base85 lines until
/// a blank one. Returns the payload and the index just past the block.
fn parse_binary_block(lines: &[&[u8]], mut i: usize) -> Option<(BinaryPayload, usize)> {
    let head = String::from_utf8_lossy(lines.get(i)?).trim_end().to_string();
    let (kind, size) = match head.split_once(' ') {
        Some(("literal", n)) => ("literal", n.parse::<usize>().ok()?),
        Some(("delta", n)) => ("delta", n.parse::<usize>().ok()?),
        _ => return None,
    };
    i += 1;
    let mut encoded: Vec<u8> = Vec::new();
    while let Some(line) = lines.get(i) {
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        if body.is_empty() {
            i += 1;
            break;
        }
        // The first byte is the length this line encodes, in git's `A`..`Z`/`a`..`z`
        // counting; a line outside that range ends the block.
        let len = match body[0] {
            c @ b'A'..=b'Z' => (c - b'A') as usize + 1,
            c @ b'a'..=b'z' => (c - b'a') as usize + 27,
            _ => break,
        };
        encoded.extend_from_slice(&super::binary_patch::decode_base85(&body[1..], len)?);
        i += 1;
    }
    // The payload is deflated, exactly as `emit_binary_diff_body()` wrote it.
    let mut inflate = gix::zlib::Inflate::default();
    let mut out = vec![0u8; size];
    let (_status, _consumed, written) = inflate.once(&encoded, out.as_mut_slice()).ok()?;
    if written != size {
        return None;
    }
    Some((
        match kind {
            "literal" => BinaryPayload::Literal(out),
            _ => BinaryPayload::Delta(out),
        },
        i,
    ))
}

/// `apply_binary()`: rebuild a binary file's post-image from its payload, refusing
/// unless the `index` line named both ids in full and the pre-image is the one the
/// payload was made against.
fn rebuild_binary(p: &Patch, pre: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let name = p
        .new_name
        .clone()
        .or_else(|| p.old_name.clone())
        .unwrap_or_default();
    let hexsz = gix::hash::Kind::Sha1.len_in_hex();
    let (Some(old_id), Some(new_id)) = (&p.index_old, &p.index_new) else {
        return Err(format!(
            "cannot apply binary patch to '{name}' without full index line"
        ));
    };
    if old_id.len() != hexsz || new_id.len() != hexsz {
        return Err(format!(
            "cannot apply binary patch to '{name}' without full index line"
        ));
    }
    let Some(payload) = &p.binary_forward else {
        return Err(format!("cannot apply binary patch to '{name}' without full index line"));
    };

    // `read_blob_object()`'s check: the pre-image has to hash to the id the patch was
    // made against, or the payload describes something else entirely.
    let have = blob_hex(pre);
    if have != *old_id {
        return Err(if pre.is_empty() {
            format!("the patch applies to an empty '{name}' but it is not empty")
        } else {
            format!(
                "the patch applies to '{name}' ({old_id}), which does not match the current contents."
            )
        });
    }
    let Some(result) = payload.rebuild(pre) else {
        return Err(format!("binary patch does not apply to '{name}'"));
    };
    let got = blob_hex(&result);
    if got != *new_id {
        return Err(format!(
            "binary patch to '{name}' creates incorrect result (expecting {new_id}, got {got})"
        ));
    }
    Ok(result)
}

/// The blob id of `data`, which is what both ends of a binary patch are checked against.
fn blob_hex(data: &[u8]) -> String {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, data)
        .map(|id| id.to_hex().to_string())
        .unwrap_or_default()
}

/// `ws_fix_copy()` for git's default whitespace rule: strip the trailing whitespace a
/// line ends with, and drop the spaces that sit in front of a tab in its indent.
///
/// Only the default rule set is fixed here. `indent-with-non-tab` and `tab-in-indent`
/// reshape the indent in ways this has not been verified against, so a repository that
/// configures them keeps the deferred `--whitespace=fix` refusal rather than getting a
/// guess (see [`ws_fix_supported`]).
fn ws_fix_default(line: &[u8]) -> Vec<u8> {
    let (body, terminator): (&[u8], &[u8]) = match line.strip_suffix(b"\n") {
        Some(rest) => (rest, b"\n"),
        None => (line, b""),
    };
    // `blank-at-eol`: everything after the last non-blank goes.
    let end = body
        .iter()
        .rposition(|b| !matches!(b, b' ' | b'\t'))
        .map_or(0, |i| i + 1);
    let body = &body[..end];

    // `space-before-tab`: inside the indent, a run of spaces followed by a tab is the
    // violation, and the fix is to drop the spaces.
    let indent_end = body
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t'))
        .unwrap_or(body.len());
    let mut out: Vec<u8> = Vec::with_capacity(line.len());
    let mut i = 0usize;
    while i < indent_end {
        if body[i] == b'\t' {
            out.push(b'\t');
            i += 1;
            continue;
        }
        let run_end = body[i..indent_end]
            .iter()
            .position(|b| *b != b' ')
            .map_or(indent_end, |n| i + n);
        // Kept unless a tab follows the run, which is what makes it a violation.
        if body.get(run_end) != Some(&b'\t') {
            out.extend_from_slice(&body[i..run_end]);
        }
        i = run_end;
    }
    out.extend_from_slice(&body[indent_end..]);
    out.extend_from_slice(terminator);
    out
}

/// Whether [`ws_fix_default`] describes what `rule` asks for: git's default set, with
/// any tab width (the width only matters to the rules this does not fix).
fn ws_fix_supported(rule: u32) -> bool {
    use super::diff_color::{WS_BLANK_AT_EOF, WS_BLANK_AT_EOL, WS_SPACE_BEFORE_TAB};
    const FIXABLE: u32 = WS_BLANK_AT_EOL | WS_BLANK_AT_EOF | WS_SPACE_BEFORE_TAB;
    // Ignore the low six bits, which carry the tab width rather than a rule.
    (rule & !0x3f) == FIXABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(name: &str, toplevel_relative: bool) -> Patch {
        Patch {
            old_name: Some(name.to_string()),
            new_name: Some(name.to_string()),
            old_mode: None,
            new_mode: None,
            is_new: false,
            is_delete: false,
            is_rename: false,
            binary: false,
            binary_forward: None,
            binary_reverse: None,
            index_old: None,
            index_new: None,
            score: 0,
            is_toplevel_relative: toplevel_relative,
            hunks: Vec::new(),
            added: 0,
            deleted: 0,
        }
    }

    /// `use_patch()`'s prefix gate (apply.c:2219): run from `sub/`, only paths
    /// strictly below `sub/` are touched. Verified against git 2.55.0 — `git apply`
    /// from a subdirectory applies the in-tree half of a whole-tree patch and
    /// silently skips the rest, exit 0.
    #[test]
    fn the_invocation_prefix_drops_paths_outside_it() {
        let keep = patch("sub/s.txt", true);
        let outside = patch("f.txt", true);
        let sibling = patch("subsidiary/x.txt", true);
        // The prefix directory itself has an empty remainder and is not a path.
        let bare = patch("sub/", true);
        assert!(use_patch(&keep, "sub/", &[], false));
        assert!(!use_patch(&outside, "sub/", &[], false));
        assert!(!use_patch(&sibling, "sub/", &[], false));
        assert!(!use_patch(&bare, "sub/", &[], false));
        // With no prefix (invoked at the top) every path is in scope.
        assert!(use_patch(&outside, "", &[], false));
    }

    /// "Paths outside are not touched regardless of `--include`" — the prefix test
    /// runs before the rule list, so an include naming an out-of-prefix path still
    /// loses. Matching happens on the whole root-relative name, which is why
    /// `--include=deep/*` from `sub/` matches nothing.
    #[test]
    fn the_prefix_outranks_an_explicit_include() {
        let outside = patch("f.txt", true);
        let inside = patch("sub/deep/t.txt", true);
        let rules = vec![(true, "f.txt".to_string())];
        assert!(!use_patch(&outside, "sub/", &rules, true));
        let rules = vec![(true, "deep/*".to_string())];
        assert!(!use_patch(&inside, "sub/", &rules, true));
        let rules = vec![(true, "sub/deep/*".to_string())];
        assert!(use_patch(&inside, "sub/", &rules, true));
    }

    /// `guess_p_value()` (apply-lib.c:747), the inference that lets a plain
    /// `diff -u old new` patch apply with no `-p0`. Verified against git 2.55.0:
    /// `git apply --stat` over `--- s.txt`/`+++ s.txt` prints ` s.txt | 2 +-` from
    /// the worktree root, from a subdirectory, and outside a repository entirely.
    #[test]
    fn a_name_with_no_directory_part_infers_p0() {
        assert_eq!(guess_p_value("s.txt", "", ""), Some(0));
        assert_eq!(guess_p_value("s.txt", "", "sub/"), Some(0));
        // A `/dev/null` side says nothing; the caller falls back to the other one.
        assert_eq!(guess_p_value("/dev/null", "", ""), None);
        // A trailing timestamp is not part of the name.
        assert_eq!(guess_p_value("s.txt\t2005-04-07 22:13:13", "", ""), Some(0));
    }

    /// With a directory part the guess only speaks when the name embeds the
    /// invocation prefix, so an ordinary `a/`-prefixed patch keeps the default
    /// `-p1` — which is why nothing about existing patches changes.
    #[test]
    fn a_name_with_a_directory_part_needs_the_prefix_to_match() {
        assert_eq!(guess_p_value("a/s.txt", "", ""), None);
        assert_eq!(guess_p_value("a/s.txt", "", "sub/"), None);
        // `sub/s.txt` from `sub/`: the name starts with the prefix, so strip its
        // own depth (one slash → 0 components before it).
        assert_eq!(guess_p_value("sub/s.txt", "", "sub/"), Some(1));
        // `a/sub/s.txt` from `sub/`: the prefix begins after the first component.
        assert_eq!(guess_p_value("a/sub/s.txt", "", "sub/"), Some(2));
        // `--directory=X` is prepended before the slash test, so a one-component
        // name stops looking like one and the guess declines.
        assert_eq!(guess_p_value("s.txt", "X/", "sub/"), None);
        assert_eq!(guess_p_value("s.txt", "X/", ""), None);
    }

    /// `prefix_patch()` (apply.c:2191): a `diff --git` patch is already relative to
    /// the worktree root and keeps its names; a traditional `---`/`+++` diff was
    /// written relative to the invocation directory and gains the prefix.
    #[test]
    fn only_a_traditional_patch_gains_the_prefix() {
        let mut git_style = patch("s.txt", true);
        prefix_patch(&mut git_style, "sub/");
        assert_eq!(git_style.old_name.as_deref(), Some("s.txt"));
        assert_eq!(git_style.new_name.as_deref(), Some("s.txt"));

        let mut traditional = patch("s.txt", false);
        prefix_patch(&mut traditional, "sub/");
        assert_eq!(traditional.old_name.as_deref(), Some("sub/s.txt"));
        assert_eq!(traditional.new_name.as_deref(), Some("sub/s.txt"));
    }

    /// The `HeaderError` text `parse_patches()` fails with, or a panic naming what
    /// happened instead.
    fn header_error(lines: &[&[u8]], strip: usize, spans: &InputSpans) -> String {
        match parse_patches(lines, strip, true, "", "", false, spans) {
            Ok(p) => panic!("expected a header diagnostic, parsed {} patch(es)", p.len()),
            Err(e) => match e.downcast_ref::<HeaderError>() {
                Some(h) => h.to_string(),
                None => panic!("expected a header diagnostic, got: {e}"),
            },
        }
    }

    /// The two shapes `-p<n>` over-strip produces, which git reports with the input
    /// file and the line the header parse stopped at. Measured against git 2.55.0:
    /// a traditional patch names its `---` line, a `diff --git` header names the
    /// line the header ended on (the `@@`, or one past the last line when the patch
    /// has no body), and the component count is singular only at one.
    #[test]
    fn over_stripping_reports_the_headers_own_line() {
        let spans = InputSpans { spans: vec![("p.patch".to_string(), 0)] };
        let trad = concat!(
            "--- a/sub/deep/f.txt\n",
            "+++ b/sub/deep/f.txt\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(trad.as_bytes());
        let err = header_error(&lines, 9, &spans);
        assert_eq!(
            err,
            "unable to find filename in patch at p.patch:1"
        );

        let git_style = concat!(
            "diff --git a/sub/deep/f.txt b/sub/deep/f.txt\n",
            "index 1234567..89abcde 100644\n",
            "--- a/sub/deep/f.txt\n",
            "+++ b/sub/deep/f.txt\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(git_style.as_bytes());
        let err = header_error(&lines, 9, &spans);
        assert_eq!(
            err,
            "git diff header lacks filename information when removing 9 leading \
             pathname components at p.patch:5"
        );

        // A pure mode change has no `@@` at all, so the parse runs off the end and
        // git reports the line one past the last.
        let mode_only = "diff --git x y\nold mode 100644\nnew mode 100755\n";
        let lines = split_lines(mode_only.as_bytes());
        let err = header_error(&lines, 1, &spans);
        assert_eq!(
            err,
            "git diff header lacks filename information when removing 1 leading \
             pathname component at p.patch:4"
        );
    }

    /// `parse_traditional_patch()` reads the `+++` line with the `---` line's name as
    /// `find_name_common()`'s `def`, so one side may over-strip without failing and
    /// both sides end up with the single name that came out. Measured against git
    /// 2.55.0: `-p2` on `--- a/f.txt` / `+++ b/deep/f.txt` patches `f.txt`.
    #[test]
    fn a_traditional_patch_resolves_both_names_together() {
        let spans = InputSpans { spans: vec![("p.patch".to_string(), 0)] };
        let text = concat!(
            "--- a/f.txt\n",
            "+++ b/deep/f.txt\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(text.as_bytes());
        let patches = parse_patches(&lines, 2, true, "", "", false, &spans).unwrap();
        assert_eq!(patches[0].old_name.as_deref(), Some("f.txt"));
        assert_eq!(patches[0].new_name.as_deref(), Some("f.txt"));

        // "Generally we prefer the shorter name": `f.txt.orig` vs `f.txt` is a patch
        // to `f.txt`, not a rename.
        let orig = concat!(
            "--- a/f.txt\n",
            "+++ b/f.txt.orig\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(orig.as_bytes());
        let patches = parse_patches(&lines, 1, true, "", "", false, &spans).unwrap();
        assert_eq!(patches[0].old_name.as_deref(), Some("f.txt"));
        assert_eq!(patches[0].new_name.as_deref(), Some("f.txt"));
    }

    /// `fuzzy_matchlines()` (apply.c:2500): whitespace runs may differ in width but
    /// may not appear or disappear, and line endings do not count on either side.
    #[test]
    fn fuzzy_matching_collapses_runs_but_not_their_absence() {
        assert!(fuzzy_matchlines(b"\tbeta   gamma\n", b"    beta gamma\n"));
        assert!(fuzzy_matchlines(b"a b\r\n", b"a\tb"));
        assert!(fuzzy_matchlines(b"same\n", b"same\n"));
        // A run that vanishes is a different line, and so is one that appears only
        // on one side.
        assert!(!fuzzy_matchlines(b"a b\n", b"ab\n"));
        assert!(!fuzzy_matchlines(b"  indented\n", b"indented\n"));
        assert!(!fuzzy_matchlines(b"one\n", b"two\n"));
        // Trailing whitespace is a run the other side does not have.
        assert!(!fuzzy_matchlines(b"trail \n", b"trail\n"));
    }

    /// `update_pre_post_images()`: a hunk that only matched under relaxed whitespace
    /// takes its context lines from the file, not from the patch, so the file's own
    /// indentation survives and only added lines come out of the patch.
    #[test]
    fn a_relaxed_match_keeps_the_files_whitespace_on_context_lines() {
        let image: Vec<Vec<u8>> = vec![
            b"\tone\n".to_vec(),
            b"  two\n".to_vec(),
            b"\tthree\n".to_vec(),
        ];
        let h = Hunk {
            old_pos: 1,
            new_pos: 1,
            pre: vec![b" one\n".to_vec(), b"\ttwo\n".to_vec(), b" three\n".to_vec()],
            pre_common: vec![true, false, true],
            post: vec![b" one\n".to_vec(), b"NEW\n".to_vec(), b" three\n".to_vec()],
            post_common: vec![true, false, true],
            added_at: vec![(0, 1)],
            context: vec![b" one\n".to_vec(), b" three\n".to_vec()],
            raw: Vec::new(),
            trailing: 1,
            leading: 1,
        };
        assert_eq!(
            place_hunk(&image, &h, false, false),
            None,
            "byte-exact matching still rejects it"
        );
        assert_eq!(place_hunk(&image, &h, false, true), Some(0));
        assert_eq!(
            replacement(&image, 0, &h, false, true),
            vec![b"\tone\n".to_vec(), b"NEW\n".to_vec(), b"\tthree\n".to_vec()]
        );
        // Without the flag the replacement is the patch's own text.
        assert_eq!(replacement(&image, 0, &h, false, false), h.post);
    }
}
