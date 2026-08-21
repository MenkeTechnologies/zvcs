use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;

use super::filespec::{content_of, count_changed_lines_ws, is_binary};
use super::diff_color;
use super::diffstat::{self, StatWidths};
use super::line_log;
use super::pretty_pad::{FlushType, PadState, WrapState};

/// `usage_with_options()` over `builtin/log.c`'s `builtin_log_usage` and option
/// table. `git show` and `git whatchanged` are the same builtin and print it too.
pub(super) const USAGE: &str = r"usage: git log [<options>] [<revision-range>] [[--] <path>...]
   or: git show [<options>] <object>...

    -q, --[no-]quiet      suppress diff output
    --[no-]source         show source
    --[no-]use-mailmap    use mail map file
    --[no-]mailmap        alias of --use-mailmap
    --clear-decorations   clear all previously-defined decoration filters
    --[no-]decorate-refs <pattern>
                          only decorate refs that match <pattern>
    --[no-]decorate-refs-exclude <pattern>
                          do not decorate refs that match <pattern>
    --[no-]decorate[=...] decorate options
    -L <range:file>       trace the evolution of line range <start>,<end> or function :<funcname> in <file>

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. `git show` and `git whatchanged` are the same builtin and print it
/// too. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]i-still-use-this`.
/// Captured byte-for-byte from stock git 2.55.0's `git log --help-all`.
pub(super) const USAGE_ALL: &str = r#"usage: git log [<options>] [<revision-range>] [[--] <path>...]
   or: git show [<options>] <object>...

    -q, --[no-]quiet      suppress diff output
    --[no-]source         show source
    --[no-]use-mailmap    use mail map file
    --[no-]i-still-use-this
                          <use this deprecated command>
    --[no-]mailmap        alias of --use-mailmap
    --clear-decorations   clear all previously-defined decoration filters
    --[no-]decorate-refs <pattern>
                          only decorate refs that match <pattern>
    --[no-]decorate-refs-exclude <pattern>
                          do not decorate refs that match <pattern>
    --[no-]decorate[=...] decorate options
    -L <range:file>       trace the evolution of line range <start>,<end> or function :<funcname> in <file>

"#;

/// Every long option `git log` recognises, without its leading `--`, sorted so a
/// binary search resolves it.
///
/// git's `cmd_log_init_finish()` runs `parse_options()` over `builtin_log_options`
/// and then `setup_revisions()` over `revision.c`'s `handle_revision_opt()` and
/// `diff.c`'s `diff_opt_parse()`; whatever those three leave unconsumed is reported
/// by `builtin/log.c:320` as `unrecognized argument`. Membership here is therefore
/// what separates "git has no such option" from "git has it and this port does not
/// implement it" — the two must not answer with the same message, so this list
/// exists to keep the second population out of git's wording.
///
/// Derived from git 2.55.0 and verified against the stock binary: every option-shaped
/// token in the git tree (`--[a-z][a-z0-9-]*` literals plus the `"name"` field of every
/// `OPT_*`/`parse_long_opt` entry) was run through `git log <tok>` and `git log <tok>=x`,
/// and every token that did not answer `unrecognized argument` is listed here.
const GIT_LOG_LONG_OPTS: &[&str] = &[
    "abbrev",
    "abbrev-commit",
    "after",
    "all",
    "all-match",
    "alternate-refs",
    "always",
    "ancestry-path",
    "anchored",
    "author",
    "author-date-order",
    "basic-regexp",
    "before",
    "binary",
    "bisect",
    "boundary",
    "branches",
    "break-rewrites",
    "cc",
    "check",
    "cherry",
    "cherry-mark",
    "cherry-pick",
    "children",
    "clear-decorations",
    "color",
    "color-moved",
    "color-moved-ws",
    "color-words",
    "combined-all-paths",
    "committer",
    "compact-summary",
    "count",
    "cumulative",
    "date",
    "date-order",
    "dd",
    "decorate",
    "decorate-refs",
    "decorate-refs-exclude",
    "default",
    "default-prefix",
    "dense",
    "diff-algorithm",
    "diff-filter",
    "diff-merges",
    "dirstat",
    "dirstat-by-file",
    "do-walk",
    "dst-prefix",
    "encode-email-headers",
    "encoding",
    "end-of-options",
    "exclude",
    "exclude-first-parent-only",
    "exclude-hidden",
    "exit-code",
    "expand-tabs",
    "ext-diff",
    "extended-regexp",
    "filter",
    "find-copies",
    "find-copies-harder",
    "find-object",
    "find-renames",
    "first-parent",
    "fixed-strings",
    "follow",
    "format",
    "full-diff",
    "full-history",
    "full-index",
    "function-context",
    "git-completion-helper",
    "git-completion-helper-all",
    "glob",
    "graph",
    "graph-lane-limit",
    "grep",
    "grep-reflog",
    "help",
    "help-all",
    "histogram",
    "i-still-use-this",
    "ignore-all-space",
    "ignore-blank-lines",
    "ignore-cr-at-eol",
    "ignore-matching-lines",
    "ignore-missing",
    "ignore-space-at-eol",
    "ignore-space-change",
    "ignore-submodules",
    "in-commit-order",
    "indent-heuristic",
    "indexed-objects",
    "inter-hunk-context",
    "invert-grep",
    "irreversible-delete",
    "ita-invisible-in-index",
    "ita-visible-in-index",
    "left-only",
    "left-right",
    "line-prefix",
    "log-size",
    "mailmap",
    "max-age",
    "max-count",
    "max-count-oldest",
    "max-depth",
    "max-parents",
    "maximal-only",
    "merge",
    "merges",
    "min-age",
    "min-parents",
    "minimal",
    "name-only",
    "name-status",
    "no-abbrev",
    "no-abbrev-commit",
    "no-color",
    "no-color-moved",
    "no-color-moved-ws",
    "no-commit-id",
    "no-compact-summary",
    "no-decorate",
    "no-decorate-refs",
    "no-decorate-refs-exclude",
    "no-diff-merges",
    "no-encode-email-headers",
    "no-exit-code",
    "no-expand-tabs",
    "no-ext-diff",
    "no-filter",
    "no-find-copies-harder",
    "no-follow",
    "no-full-index",
    "no-function-context",
    "no-graph",
    "no-i-still-use-this",
    "no-ignore-matching-lines",
    "no-indent-heuristic",
    "no-kept-objects",
    "no-mailmap",
    "no-max-parents",
    "no-merges",
    "no-min-parents",
    "no-notes",
    "no-patch",
    "no-prefix",
    "no-quiet",
    "no-relative",
    "no-rename-empty",
    "no-renames",
    "no-show-signature",
    "no-source",
    "no-standard-notes",
    "no-text",
    "no-textconv",
    "no-use-mailmap",
    "no-walk",
    "not",
    "notes",
    "numstat",
    "objects",
    "objects-edge",
    "objects-edge-aggressive",
    "oneline",
    "output",
    "output-indicator-context",
    "output-indicator-new",
    "output-indicator-old",
    "parents",
    "patch",
    "patch-with-raw",
    "patch-with-stat",
    "patience",
    "perl-regexp",
    "pickaxe-all",
    "pickaxe-regex",
    "pretty",
    "quiet",
    "raw",
    "reflog",
    "regexp-ignore-case",
    "relative",
    "relative-date",
    "remerge-diff",
    "remotes",
    "remove-empty",
    "rename-empty",
    "reverse",
    "right-only",
    "root",
    "rotate-to",
    "shortstat",
    "show-linear-break",
    "show-notes",
    "show-notes-by-default",
    "show-pulls",
    "show-signature",
    "simplify-by-decoration",
    "simplify-merges",
    "since",
    "since-as-filter",
    "single-worktree",
    "skip",
    "skip-to",
    "source",
    "sparse",
    "src-prefix",
    "standard-notes",
    "stat",
    "stat-count",
    "stat-graph-width",
    "stat-name-width",
    "stat-width",
    "stdin",
    "submodule",
    "summary",
    "tags",
    "text",
    "textconv",
    "topo-order",
    "unified",
    "unpacked",
    "until",
    "use-mailmap",
    "verify-objects",
    "walk-reflogs",
    "word-diff",
    "word-diff-regex",
    "ws-error-highlight",
];

/// Every short option `git log` recognises, verified the same way (`-a` through `-Z`
/// against the stock binary). Digits are absent because `-<n>` is `--max-count=<n>`
/// and is consumed before the fallthrough that consults this.
const GIT_LOG_SHORT_OPTS: &str = "abcghilmnpqrstuvwzBCDEFGILMOPRSUWX";

/// Whether `git log`'s own parser would recognise `arg` — the test `builtin/log.c`
/// makes implicitly by having consumed it before its leftover check.
///
/// A long option is matched by name with any `=<value>` cut off, which is git's
/// granularity: `--pretty` and `--pretty=x` are the same table entry, while
/// `--no-pretty` is a distinct one that git does not have. A short option is matched
/// on its first letter only, because that is the one `parse_options` looks up before
/// it either consumes an attached value (`-U5`) or re-emits the rest of the cluster.
fn git_log_knows(arg: &str) -> bool {
    if let Some(rest) = arg.strip_prefix("--") {
        let name = rest.split('=').next().unwrap_or(rest);
        return GIT_LOG_LONG_OPTS.binary_search(&name).is_ok();
    }
    match arg.strip_prefix('-').and_then(|rest| rest.chars().next()) {
        Some(c) => GIT_LOG_SHORT_OPTS.contains(c),
        None => false,
    }
}

/// The terminal width git assumes for `--stat` when stdout is not a terminal.
/// git's `MINIMUM_ABBREV`: no `--abbrev` may cut an id shorter than this.
const MINIMUM_ABBREV: usize = 4;

/// git's `DEFAULT_ABBREV`, the length a valueless `--abbrev` selects.
const DEFAULT_ABBREV: usize = 7;

/// Parse an integer with git's lenient `strtoul`-ish behavior for a `--stat*=<n>`
/// value; a non-numeric value leaves the slot at its "unset" sentinel.
fn parse_stat_i64(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(-1)
}


/// git's ref-decoration style (`--decorate` / `log.decorate`): whether commit
/// decorations are shown and, when shown, with short (`main`) or full
/// (`refs/heads/main`) ref names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecorateStyle {
    /// No decorations on the built-in header/oneline formats.
    Off,
    /// `main`, `tag: v1`, `origin/main`.
    Short,
    /// `refs/heads/main`, `tag: refs/tags/v1`, `refs/remotes/origin/main`.
    Full,
}

/// git's `parse_decoration_style`: a maybe-bool (`true`/`false`/`yes`/`no`/
/// `on`/`off`/integer), or the words `short`/`full`/`auto`. `auto` resolves to
/// `Short` when stdout is a terminal and `Off` otherwise, matching git's
/// `auto_decoration_style`. Returns `None` for a value git rejects — config
/// treats that as `Off`, while `--decorate=<value>` makes it fatal.
pub(crate) fn parse_decoration_style(value: &str) -> Option<DecorateStyle> {
    let lower = value.to_ascii_lowercase();
    match lower.as_str() {
        "true" | "yes" | "on" | "short" => return Some(DecorateStyle::Short),
        "false" | "no" | "off" | "" => return Some(DecorateStyle::Off),
        "full" => return Some(DecorateStyle::Full),
        "auto" => {
            return Some(if std::io::stdout().is_terminal() {
                DecorateStyle::Short
            } else {
                DecorateStyle::Off
            })
        }
        _ => {}
    }
    // git falls back to integer parsing: a non-zero value is true (Short).
    if let Ok(n) = lower.parse::<i64>() {
        return Some(if n != 0 {
            DecorateStyle::Short
        } else {
            DecorateStyle::Off
        });
    }
    None
}

/// `git log` — commit history reachable from a starting revision (default `HEAD`).
///
/// Ported invocation forms:
///   * `git log [<rev>...]`                      → history from `HEAD`, a revision, or the
///     union of several revisions
///   * `-- <pathspec>...`                        → path-limited traversal: show only commits
///     that touched a matching pathspec, magic (`:(exclude)`, `:(glob)`, …) included
///   * `-n N` / `--max-count=N` / `-N` / `-nN`   → limit the number of commits shown
///   * `--skip=N`                                → drop the first N selected commits
///   * `--all`                                   → start from every ref plus `HEAD`
///   * `--reflog`                                → `add_reflogs_to_pending()`: the old
///     and the new id of every entry of every reflog become pending tips, so a commit
///     no ref points at any more is still walked. It reads no `--exclude` pattern and
///     clears none, unlike the ref selectors it stands beside
///   * `--max-age=<epoch>` / `--min-age=<epoch>`  → the same `revs->max_age`/`min_age`
///     as `--since`/`--until`, read as a raw epoch by `parse_age()` rather than by
///     `approxidate()`
///   * `--merges` / `--no-merges`                → keep only (or drop) multi-parent commits
///   * `--min-parents=N` / `--max-parents=N` and
///     their `--no-` forms                       → parent-count limiting
///   * `--first-parent`                          → follow only the first parent
///   * `--follow`                                 → track one path across renames
///   * `-m` / `-c` / `--cc` / `--diff-merges=<m>` → what a merge's diff shows
///   * `--reverse`                               → emit the selected commits oldest-first
///   * `--date-order` / `--topo-order`           → git's two topological sort orders
///   * `--oneline`, `--pretty=`/`--format=` with
///     `oneline`, `short`, `medium`, `full`, `fuller`, `raw`, `reference`, `email`,
///     `mboxrd`, and
///     `format:`/`tformat:` strings (last flag wins; an invalid value is rejected
///     exactly as git's `get_commit_format` does). The two mail formats are
///     `pretty.c`'s `CMIT_FMT_EMAIL`/`CMIT_FMT_MBOXRD`: the magic
///     `From <oid> Mon Sep 17 00:00:00 2001` line, RFC2047-encoded `From:`, an
///     RFC2822 `Date:`, `Subject: [<prefix>] …` from `format.subjectPrefix`, and the
///     `MIME-Version:`/`Content-Transfer-Encoding: 8bit` block a non-ASCII body
///     forces. `--encode-email-headers`/`--no-encode-email-headers`
///     (`revs->encode_email_headers`, seeded from `format.encodeEmailHeaders`) turn
///     the Q-encoding off; `rev-list` renders the same format from a zeroed
///     `pretty_print_context`, so there it has no `[<prefix>]` and no encoding.
///     User-format placeholders include
///     `%C`/`%C(...)` colors (with `%C(auto)`), `%d`/`%D` ref decorations, and
///     `%cr`/`%ar` relative dates, alongside the hash/tree/parent/author/committer/
///     subject/body set
///   * `--abbrev-commit` / `--no-abbrev-commit`, `--parents`
///   * `--date=<mode>`                           → `default`/`short`/`iso`/`iso-strict`/
///     `rfc`/`unix`/`raw`/`relative` (the remaining zone-dependent modes `human`/`local`
///     are surfaced terse)
///   * `--color[=<when>]` / `--no-color`         → enable/disable the `%C` and
///     `%C(auto)`-gated decoration colors (`always`/`never`/`auto`; auto colors when
///     stdout is a terminal or a pager is in use)
///   * `--name-only`, `--name-status`, `--raw`, `--stat`,
///     `--numstat`, `--shortstat`, `--summary`     → per-commit diff against the first parent.
///     `diff_setup_done()`'s precedence applies: `--name-only`/`--name-status` are mutually
///     exclusive and clear every other format, `--raw` clears nothing (so `--raw --stat -p`
///     prints all three, in git's order), and `--stat --shortstat` prints both summary
///     lines. `-s`/`--no-patch` clears them all. `--patch-with-stat` and
///     `--patch-with-raw` are the `OPT_BITOP` spellings that set their own bit
///     *and* the patch, i.e. exactly `-p --stat` and `-p --raw`
///   * `--dirstat[=<params>]`, `--dirstat-by-file[=<params>]`, `--cumulative`
///                                               → `show_dirstat()`, rendered by the
///     port `git diff` uses. It is the one format writer `diff_flush()` does not
///     count into `separator` (diff.c:7238), so `--dirstat -p` runs the patch
///     straight on where `--stat -p` inserts a blank line; only `--dirstat=lines`,
///     emitted from inside the count-format block, earns the separator
///   * `--compact-summary` / `--no-compact-summary` → `fill_print_name()`'s
///     ` (new|gone|mode ±x|mode ±l)` annotation on each stat row. The positive
///     spelling also turns `--stat` on; the negation clears only the annotation
///   * `--relative[=<path>]` / `--no-relative`   → two separate things:
///     `diff_queue()`'s prefix test narrows *every* format, while `strip_prefix()`
///     (diff.c:5009) shortens only the patch, raw, name and stat writers —
///     `diff_summary()` and `show_dirstat()` never call it, so both keep the
///     repository-root name. Seeded from `diff.relative`
///   * `--diff-filter=<letters>`                 → `diffcore_apply_filter()`, applied
///     after rename detection so a pair is judged by the status it finally carries.
///     It is a queue filter, so `cmd_log_init_finish()` (builtin/log.c:333) clears
///     `always_show_header` for it and `revision.c:3149` raises `revs->diff`: a
///     commit the filter empties prints nothing at all, even under `-s`
///   * `--output-indicator-{new,old,context}=<c>` → `o->output_indicators[]`,
///     substituted by `emit_line_ws_markup()` at emit time, so the `---`/`+++` file
///     headers keep their own characters and an empty value drops the sign entirely
///   * `--word-diff[=<mode>]`, `--word-diff-regex=<re>`, `--color-words[=<re>]`,
///     `--color-moved[=<mode>]`, `--color-moved-ws=<modes>`, `--ws-error-highlight=<kind>`
///                                               → the family that re-emits the
///     assembled patch rather than changing how it is generated, run through the
///     same `fn_out_consume()` chain `git diff` uses. The patch body is painted from
///     the run's own `o->use_color`, so `log -p --color=always` colours it exactly as
///     `git diff` does
///   * `--expand-tabs[=<n>]` / `--no-expand-tabs` → `revs->expand_tabs_in_log`, the
///     width a tab in the commit message is expanded to under the four-space
///     indent the header formats print it with. The default is git's
///     `expand_tabs_in_log_default` of 8 for `medium`/`full`/`fuller` and none for
///     `raw`; an explicit value reaches `raw` too, and `--no-expand-tabs` (or
///     `--expand-tabs=0`) leaves every tab as written
///   * `-z`                                      → `line_termination = 0`: the raw/name
///     records and the per-commit record use NUL separators and the paths go out
///     unquoted
///   * `-q`/`--quiet` / `--no-quiet`               → git's position-independent
///     NO_OUTPUT: with no diff requested it changes nothing (`git log` shows no diff
///     by default), and any explicit `-p`/`--stat` still wins, so its only visible
///     effect is the `--name-only`/`--name-status` + NO_OUTPUT conflict
///   * `--decorate[=short|full|auto|no]` / `--no-decorate` → ref decorations on the
///     built-in header/oneline formats, defaulting to `log.decorate` and then to
///     `auto`. `--decorate-refs=<pattern>` and `--decorate-refs-exclude=<pattern>`
///     (both repeatable, matched with git's `normalize_glob_ref` +
///     `match_ref_pattern` rules) narrow which refs may decorate, and
///     `--clear-decorations` empties both lists and drops the default
///     known-namespace restriction, exposing refs such as `refs/bisect/*`.
///     `log.excludeDecoration` and `log.initialDecorationSet=all` are honored
///   * `--use-mailmap`/`--mailmap` and their `--no-` forms → resolve the
///     `Author:`/`Commit:` identities of the built-in header formats through
///     `.mailmap`, defaulting to `log.mailmap` (true, as in git since 2.24).
///     Like git, this affects only `pp_user_info`'s formats: `oneline`, `raw` and
///     user formats print the identity as the commit recorded it
///   * `--source` / `--no-source`                  → annotate each commit with the
///     ref/argument it was first reached from (`\t<source>` after the hash), on the
///     built-in header formats (not the user or `reference` formats), with git's
///     parent-inheritance during the walk
///   * `-p`/`--patch`/`-u`                        → per-commit `diff --git` patch against the
///     first parent (the empty tree for a root commit), three lines of context; suppressed by
///     `--name-only`/`--name-status`, emitted after the count formats otherwise, and skipped
///     for merge commits (git shows no diff there without `-m`/`-c`/`--cc`). Rendered by the
///     same pipeline as `git diff`, so the two produce byte-identical patches. The root
///     commit's empty-tree diff obeys `log.showRoot` (default true); `--root` forces it on.
///   * `--graph`                                 → git's ASCII commit graph (see below)
///   * `-L<start>,<end>:<file>` and its
///     `<start>,+<n>` / `<start>,-<n>` / `/<regex>/` / `:<funcname>` / `^:<funcname>`
///     spellings, repeatable across files and across ranges of one file → git's
///     line-level traversal (see [`super::line_log`]): only the commits that changed
///     a tracked line are shown, each with a diff clipped to that line range. `-L`
///     implies `--topo-order` and, with no other diff format given, `-p`; it is
///     rejected against a pathspec and against the count formats exactly as git
///     rejects them.
///
/// ### The `--graph` commit graph
///
/// The column state machine is `graph.c`, function for function (see [`Graph`]).
/// Three of its rules are worth naming because they make the graph disagree with
/// the commit's own parent list, which reads like a bug and is not:
///
///   * A lane is drawn per *interesting* parent — `graph_is_interesting()`, which
///     asks `get_commit_action()`. A parent dropped by `--merges`, `--no-merges`,
///     a parent-count limit, a date limit, a `--grep`/`--author`/`--committer` or a
///     `^rev` exclusion gets no lane, so `log --graph --merges` draws a merge as a
///     plain `*` even though its `Merge:` header still names both parents.
///     `--boundary` is the exception: git marks every parent of a returned commit
///     CHILD_SHOWN, and that flag alone makes it interesting.
///   * A commit the walk reached but printed nothing for — a `-S`/`-G` miss, a
///     `whatchanged` record whose diff came out empty — still moves the columns on,
///     because `graph_update()` runs from `get_revision()` while the rows are drawn
///     from `log_tree_commit()`. The gap shows up as the `...` skip row.
///   * The graph's remaining rows (`|\`, `|/`) are drained after the commit
///     *message*, not after the whole record: the diff below them is written
///     through the diff prefix callback, which draws padding rows instead.
///
/// `--graph` forces `--topo-order` unless `--date-order` was asked for, and it is
/// refused against `--reverse` and `--no-walk`.
///
/// ### Rename detection in the per-commit diff
///
/// The per-commit diff runs `diffcore_rename` — the same port `git diff` uses, so the
/// pairing and the similarity indices agree — because `init_diff_ui_defaults()` turns
/// it on for every porcelain. A commit that renamed a file shows `R<score> <old>
/// <new>` in `--name-status`, `<old> => <new>` (compacted to `dir/{a => b}` where the
/// paths share a prefix) in `--stat`/`--numstat`, and a `similarity index` plus
/// `rename from`/`rename to` patch header. `diff.renames=false` turns it off.
///
/// `--follow`'s own rename search runs on the *raw* change list, before that pass, so
/// the commit list it produces is unaffected by it.
///
/// `--follow` itself is ported: `try_to_follow_renames()`'s rewrite of the
/// pathspec, one commit at a time along the first parent, so the log walks back
/// through every name the file has had. Its exact-rename pass is git's; the
/// inexact one uses the same `diffcore_count_changes()` estimator, which agrees on
/// the score but may pick a different winner when several deletions in one commit
/// score alike.
///
/// Output separation follows git's `format:` (separator) versus `tformat:`
/// (terminator) distinction, which is why `--format=%s` and `--pretty=format:%s`
/// lay out differently; `--oneline`/`--pretty=oneline` are terminator formats.
///
/// Deviations, surfaced rather than faked:
///   * `--graph` is a port of `graph.c` covering every parent count, octopus merges
///     included, minus `graph_needs_truncation()` — the lane cap only reachable
///     through `--graph-lane-limit=<n>`, which this port rejects as unsupported.
///   * `--abbrev[=<n>]`/`--no-abbrev` set the width of every abbreviated id, applied as a
///     `core.abbrev` override. `--no-abbrev` is git's zero: the raw columns and `%h` print
///     the whole id while the patch `index` line stays at the configured default.
///   * `--stat` is the shared [`super::diffstat`] port of `show_stats()`: the name
///     column is measured in display columns, the total width comes from
///     `term_columns()` (`$COLUMNS`, else 80 — there is no `TIOCGWINSZ` probe), and
///     the `--stat-width`/`--stat=<w>`, `--stat-name-width`, `--stat-graph-width`
///     and `--stat-count` flags and the `diff.statNameWidth`/`diff.statGraphWidth`
///     config are honored (flag over config over the terminal / uncapped default).
///     The graph is never colorized here, because this module's diff output never is.
///   * Pathspec limiting is git's default history simplification: a commit
///     TREESAME to any parent over the pathspec is simplified away and the
///     history behind that parent alone is followed, so a merge that took one
///     side's change drops out along with the side it did not take. The diff
///     formats (`-p`, `--stat`, `--name-*`) are limited to the same paths.
///     `--full-history` (`revs->simplify_history = 0`) follows every parent
///     instead, `--simplify-merges` rewrites each commit to its simplification
///     on top of that, and `--sparse`/`--dense` (`revs->dense`) decide whether a
///     TREESAME commit is *dropped* at all. `simplify_history` is also cleared
///     without being asked for, by `cmd_whatchanged()` and by every
///     `--diff-merges` value that routes through `set_separate()` (`-m`,
///     `separate`, `m`, `on`, `1`, `first-parent`, `remerge`).
///   * Revision ranges are supported: `A..B` (`^A B`), `A...B` (symmetric
///     difference, excluding the merge-base), and a leading `^A` exclusion.
///   * `-M`/`-C`/`-B` and their long spellings drive `diffcore_rename`'s rename, copy and
///     break-rewrite passes, and reach every format (`--raw`, `--name-status`, `--stat`,
///     `--summary`, the patch).
///   * `%aN`/`%aE`/`%cN`/`%cE` resolve through `.mailmap` whether or not `--use-mailmap`
///     is in effect, and `--author`/`--committer` grep the mailmapped headers while it is.
///   * `--check` is `DIFF_FORMAT_CHECKDIFF`: it clears every other output format
///     (`diff_setup_done()`) and reports through `diff_result_code()`'s `02` bit,
///     while `--exit-code`/`--no-exit-code` is the `01` bit and makes
///     `log_tree_diff()`'s `all_need_diff` true on its own. A merge under a
///     combined mode sets neither, because `diff_tree_combined()` never looks at
///     `DIFF_FORMAT_CHECKDIFF` and has no `has_changes` assignment.
///   * `--line-prefix=<s>` is `diff_line_prefix()`, written in front of every
///     emitted line including the header — refused beside `-z`, where git prefixes
///     each NUL-terminated *record* rather than each NUL.
///   * `--rename-empty` / `--no-rename-empty` is `o->flags.rename_empty`: with it
///     off, an empty file that moved reports as a deletion plus an addition
///     instead of an `R100`.
///   * `--remerge-diff` / `--diff-merges=remerge` parses and is refused where
///     `do_remerge_diff()` (log-tree.c:1029-1090) would run — a walk that reaches
///     no merge is rendered exactly as git renders it.
///   * `<rev>^@`, `<rev>^!` and `<rev>^-<n>` are decoded by
///     [`crate::objname::parents_only`] before the revision parser sees the
///     operand, because they are `handle_revision_arg_1()`'s own grammar
///     (revision.c:2178-2207) rather than the parser's.
///   * Every flag not listed above is rejected.
/// Which builtin is being run. `cmd_whatchanged()` is `cmd_log()` with two settings
/// changed: the raw format is the default when nothing else is asked for
/// (`if (!rev.diffopt.output_format)`, which `-s`/`--no-patch`/`-q` satisfy with
/// `DIFF_FORMAT_NO_OUTPUT` — so those leave it with no listing at all), and
/// `always_show_header` stays off, so a commit whose diff queue came out empty
/// prints nothing at all — and does not spend a `--max-count` slot. That queue is
/// still built under `NO_OUTPUT`, because it is what decides whether the commit is
/// shown; only the rendering is skipped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    Log,
    WhatChanged,
}

/// `git whatchanged`: `cmd_log()` under [`Flavor::WhatChanged`].
pub(crate) fn whatchanged(args: &[String]) -> Result<ExitCode> {
    log_flavored(args, Flavor::WhatChanged)
}

pub fn log(args: &[String]) -> Result<ExitCode> {
    log_flavored(args, Flavor::Log)
}

fn log_flavored(args: &[String], flavor: Flavor) -> Result<ExitCode> {
    // Repeated object reads (one per rendered commit) re-inflate from the pack
    // without a cache; gix ships one and simply does not enable it by default.
    // A few MB turns `log` on a deep history from thousands of decompressions
    // into a warm-cache walk.
    let mut repo = gix::discover(".")?;
    // Rendering re-reads every walked commit for its message; without gix's
    // object cache each read re-inflates from the pack. Enabling it is the
    // difference between one decompression per commit and one per cache miss.
    repo.object_cache_size_if_unset(8 * 1024 * 1024);
    // gix's DEFAULT pack cache is a 64-entry linked list; git ships a 96MB
    // delta-base cache (core.deltaBaseCacheLimit). On a deep history every
    // rendered commit re-resolves its delta chain against those 64 slots, which
    // is where `log` spent its time. Size it like git.
    repo.objects.set_pack_cache(|| {
        Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(96 * 1024 * 1024))
    });

    // Config supplies the defaults; the flags below override them. git reads
    // these in `git_log_config` before parsing args, and validates `log.date`
    // there — an invalid value is fatal even when `--date` later overrides it.
    let (cfg_abbrev_commit, cfg_date_mode, cfg_show_root, cfg_decorate, cfg_mailmap, cfg_follow) = {
        let snap = repo.config_snapshot();
        let abbrev = snap.boolean("log.abbrevCommit").unwrap_or(false);
        // `log.follow` (`default_follow` in builtin/log.c:588) is NOT `--follow`:
        // it sets `default_follow_renames`, which `log_setup_revisions_tweak()`
        // promotes to real following only when there is exactly one pathspec
        // (builtin/log.c:857-859). With none or several it is silently dropped —
        // where the explicit flag dies — and `cmd_whatchanged()` installs no tweak
        // at all, so the key does not reach it.
        let follow = snap.boolean("log.follow").unwrap_or(false) && flavor == Flavor::Log;
        // `log.mailmap` has defaulted to true since git 2.24, so the built-in
        // formats route identities through `.mailmap` unless `--no-use-mailmap`
        // or `log.mailmap=false` turns it off.
        let mailmap = snap.boolean("log.mailmap").unwrap_or(true);
        // `log.decorate` sets the default decoration style for the built-in
        // header/oneline formats. It reuses git's `parse_decoration_style`, so it
        // accepts a maybe-bool plus `short`/`full`/`auto`; an invalid value is
        // treated as `Off` (git's `decoration_style = 0`), never fatal. `None`
        // here means the key is absent, so the built-in default (`auto`) applies.
        let decorate: Option<DecorateStyle> = match snap.boolean("log.decorate") {
            Some(true) => Some(DecorateStyle::Short),
            Some(false) => Some(DecorateStyle::Off),
            None => snap.string("log.decorate").map(|v| {
                parse_decoration_style(&v.to_str_lossy()).unwrap_or(DecorateStyle::Off)
            }),
        };
        // `log.showRoot` defaults to true: the root commit is shown as a big
        // creation event (a diff against the empty tree). `--root` on the command
        // line forces it on but there is no `--no-root`, so config is the only way
        // to suppress the root diff.
        let show_root = snap.boolean("log.showRoot").unwrap_or(true);
        let date = match snap.string("log.date") {
            Some(v) => {
                let v = v.to_str_lossy();
                match parse_date_mode(&v) {
                    Some(m) => m,
                    None => {
                        eprintln!("fatal: unknown date format {v}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            None => DateMode::Default,
        };
        (abbrev, date, show_root, decorate, mailmap, follow)
    };
    // `git_log_config()` also reads the two keys behind `--pretty=email`'s
    // headers (builtin/log.c:560-561 and 566-569), so `git log --pretty=email`
    // honours both even though the options that set them belong to
    // `format-patch`:
    //
    // ```c
    // if (!strcmp(var, "format.subjectprefix"))
    //         return git_config_string(&fmt_patch_subject_prefix, var, value);
    // …
    // if (!strcmp(var, "format.encodeemailheaders")) {
    //         default_encode_email_headers = git_config_bool(var, value);
    //         return 0;
    // }
    // ```
    //
    // `fmt_patch_subject_prefix` starts at `"PATCH"`, which is the bracketed word
    // an unconfigured repository prints; an empty value drops the brackets
    // entirely, as `fmt_output_email_subject()`'s `*opt->subject_prefix` test does.
    let (cfg_subject_prefix, cfg_encode_email_headers) = email_config(&repo);
    let mut encode_email_headers = cfg_encode_email_headers;

    // `--stat` width geometry, seeded from `diff.statNameWidth`/`diff.statGraphWidth`
    // (`git_diff_ui_config()`); a later `--stat*` flag overrides the corresponding slot.
    // git loads config before parsing args, so the flag always wins.
    let mut stat_widths = StatWidths::default();
    {
        let snap = repo.config_snapshot();
        if let Some(n) = snap.integer("diff.statNameWidth") {
            if n > 0 {
                stat_widths.name_width = n;
            }
        }
        if let Some(n) = snap.integer("diff.statGraphWidth") {
            if n > 0 {
                stat_widths.graph_width = n;
            }
        }
    }

    let mut max_count: Option<usize> = None;
    let mut skip: usize = 0;
    let mut pretty = Pretty::Medium;
    let mut terminator = false;
    // `-z`: `line_termination = 0` — the raw/name records use NUL field and record
    // separators and stop C-quoting, and the per-commit record terminator/separator
    // becomes NUL too.
    let mut z = false;
    // `--abbrev[=<n>]`, applied as a `core.abbrev` override so every abbreviation
    // in the run — `%h`, oneline ids, diff index lines — reads the same length.
    let mut abbrev_len: Option<usize> = None;
    // `rev->pretty_given`: the built-in formats show notes only when the caller
    // did not pick a format, so the flag has to be tracked, not inferred from
    // `pretty` (which starts at the same `medium` a `--pretty=medium` selects).
    let mut pretty_given = false;
    let mut notes_opt = super::notes::DisplayOpt::default();
    let mut abbrev_commit = cfg_abbrev_commit;
    // `--show-signature` / `--no-show-signature` (`rev_info.show_signature`), which
    // `show_log()` consults at log-tree.c:851. Off unless asked for; `log.showSignature`
    // is not read here (see the module header).
    let mut show_signature = false;
    let mut name_only = false;
    let mut name_status = false;
    // `--raw`: the `:<old mode> <new mode> <old sha> <new sha> <status>\t<path>` listing.
    let mut raw = false;
    // `--check`: `DIFF_FORMAT_CHECKDIFF`, which `diff_setup_done()` lets clear every
    // other output format — so a commit under it prints its header and the
    // whitespace report and nothing else. Declared `PARSE_OPT_NONEG`, so there is
    // no `--no-check`.
    // `--remerge-diff` / `--diff-merges=remerge` (`revs->remerge_diff`): the mode
    // is tracked beside [`DiffMerges`] because this port refuses it only where a
    // merge would actually need the re-merge.
    let mut remerge = false;
    // `--line-prefix=<s>` (`diff_line_prefix()`): the string `emit_line_0()` writes
    // in front of every emitted line, the header `show_log()` wrote included.
    let mut line_prefix: Vec<u8> = Vec::new();
    let mut check = false;
    // `--exit-code` (`o->flags.exit_with_status`), an `OPT_BOOL` (diff.c:6256).
    // `log_tree_diff()`'s `all_need_diff` is `opt->diff || exit_with_status`, so it
    // builds the queue on its own even with no format asking for one.
    let mut exit_code = false;
    let mut stat = false;
    let mut numstat = false;
    let mut shortstat = false;
    // `--summary`: the creation/deletion/rename/mode-change lines.
    let mut summary = false;
    // `--relative[=<path>]` / `--no-relative` (`diff_opt_relative()`): the prefix
    // every reported name is narrowed to and then shortened by. Seeded from
    // `diff.relative` once the repository is open.
    let mut relative: Option<String> = None;
    /// Whether `--no-relative` was seen, which beats `diff.relative`.
    let mut no_relative_given = false;
    // `--compact-summary` (`diff_opt_compact_summary()`, diff.c): the stat rows gain
    // a ` (new|gone|mode +x|…)` annotation, and the flag also turns `--stat` on.
    // `--no-compact-summary` only clears the annotation; it never touches the format.
    let mut compact_summary = false;
    // `--dirstat[=<params>]` / `--dirstat-by-file[=<params>]` / `--cumulative`
    // (`diff_opt_dirstat()`, diff.c), and the `diff.dirstat` config behind them.
    let mut dirstat_on = false;
    let mut dirstat = super::diff_files::DirStat::default();
    let mut patch = false;
    // `-q`/`--quiet`: git pre-sets DIFF_FORMAT_NO_OUTPUT before the other diff-format
    // flags parse, so it is position-independent. On `git log` (which shows no diff by
    // default) its only observable effect is the name-only/name-status conflict below.
    let mut quiet = false;
    // `--source`: annotate each commit with the ref/argument it was first reached
    // from (`\t<source>` after the hash), for the built-in header formats.
    let mut source_mode = false;
    let mut graph = false;
    // git's built-in default is `auto` (short refs when interactive, none when
    // piped); `log.decorate` overrides it, and the `--decorate` flags override
    // that in turn.
    let builtin_decorate = if std::io::stdout().is_terminal() {
        DecorateStyle::Short
    } else {
        DecorateStyle::Off
    };
    let mut decorate = cfg_decorate.unwrap_or(builtin_decorate);
    // `--decorate-refs=<pattern>` / `--decorate-refs-exclude=<pattern>` (both
    // repeatable) and `--clear-decorations`, which empties them again and drops
    // git's default "known namespaces" include list.
    let mut decorate_refs: Vec<String> = Vec::new();
    let mut decorate_refs_exclude: Vec<String> = Vec::new();
    let mut default_decoration_filter = true;
    // `--use-mailmap`/`--mailmap`: route the author/committer identity of the
    // built-in header formats through `.mailmap`. Seeded from `log.mailmap`.
    let mut use_mailmap = cfg_mailmap;
    let mut all = false;
    // `--all`, `--branches`/`--tags`/`--remotes` (each optionally `=<glob>`) and
    // `--glob=<glob>`, kept in command-line order because that is the order
    // `handle_refs()` appends their tips in.
    let mut ref_selections: Vec<RefSelection> = Vec::new();
    // `--exclude=<glob>`, accumulated until the next ref-selecting option consumes
    // and clears it (`clear_ref_exclusions`, revision.c).
    let mut ref_excludes: Vec<String> = Vec::new();
    // `--reflog`: where each occurrence stood, and whether `--not` was in force
    // when it did. `add_reflogs_to_pending()` appends to the same pending list the
    // ref selectors above do, so its position among them is what orders the tips.
    // Parallel to [`ref_selections`] rather than part of it because the two share
    // no state at all: `--reflog` reads no glob and — unlike every selector beside
    // it — neither consumes nor clears the `--exclude` patterns (revision.c:2766).
    let mut reflog_selections: Vec<(usize, bool)> = Vec::new();
    // `--stdin`: read further revisions (then, after a bare `--`, pathspecs) from
    // standard input. It is how a caller asks about a set of commits too large or
    // too dynamic for a command line — the JetBrains client loads every commit's
    // details with `log --no-walk --stdin`, feeding the hashes it wants.
    let mut read_stdin = false;
    // `--not`: reverses the sense of every revision that follows, and toggles
    // again at the next `--not` (`handle_revision_pseudo_opt()`). It applies to
    // the arguments after it, so the flip is recorded per revision as it is read.
    let mut negate_revs = false;
    // `--no-walk[=(sorted|unsorted)]` / `--do-walk`: show the named commits
    // themselves and traverse no further. `sorted` (the default) orders them by
    // commit date, newest first; `unsorted` keeps the order they were named in.
    let mut no_walk: Option<NoWalk> = None;
    // `--expand-tabs[=<n>]` / `--no-expand-tabs`; `None` leaves each pretty format
    // on its own default.
    let mut expand_tabs: Option<usize> = None;
    // `-g`/`--walk-reflogs` (`revs->reflog_info`): walk each named ref's reflog,
    // newest entry first, instead of the history reachable from its tip.
    let mut walk_reflogs = false;
    // `revs->date_mode_explicit`: whether `--date=` was given on the command line,
    // which is what the `-g` selector consults (`log.date` alone does not).
    let mut date_explicit = false;
    let mut reverse = false;
    let mut only_merges = false;
    let mut no_merges = false;
    let mut first_parent = false;
    let mut show_parents = false;
    let mut show_children = false;
    let mut boundary = false;
    // `--simplify-by-decoration`, plus somewhere to keep the decoration map when
    // the format itself did not ask for one.
    let mut simplify_by_decoration = false;
    // `--full-history` (git's `revs->simplify_history = 0`): follow every parent
    // of a merge even when the merge is TREESAME to one of them, so a change that
    // arrived on a side branch keeps both the merge and that side in the history.
    //
    // The flag has three other sources, and each is a plain `= 0` that nothing
    // ever puts back:
    //
    //   * `cmd_whatchanged()`: `rev.simplify_history = 0;` (builtin/log.c:620),
    //     which is why `git whatchanged --parents -- <path>` keeps a merge that
    //     `git log --parents -- <path>` collapses;
    //   * `set_separate()` — so `-m`, `--diff-merges=separate|m|on|1|first-parent`
    //     and `--diff-merges=remerge` all carry it (diff-merges.c:38, 65). A later
    //     `--diff-merges=off` selects a different mode but runs only `suppress()`,
    //     which does not touch `simplify_history`, so the history stays unsimplified;
    //   * `--simplify-merges` (revision.c:2424), handled with the option below.
    let mut full_history = flavor == Flavor::WhatChanged;
    // `--simplify-merges`: build the `--full-history` graph, then replace each
    // commit with its simplification (see [`simplify_merges`]). revision.c:2424
    // sets, in order: simplify_merges, topo_order, rewrite_parents,
    // simplify_history = 0, limited.
    let mut simplify_merges_opt = false;
    // `revs->dense` (`--dense`/`--sparse`, revision.c:2462-2465), which
    // `repo_init_revisions()` starts at 1. It only matters where `revs->prune`
    // is on — a pathspec — and it turns off two things at once: the
    // `!revs->dense && !commit->parents->next` early return in
    // `try_to_simplify_commit()` (revision.c:996), which keeps every non-merge
    // out of TREESAME, and the `if (revs->prune && revs->dense)` gates in
    // `get_commit_action()` (revision.c:4221) and `simplify_commit()`
    // (revision.c:4318), which are what actually *drop* a TREESAME commit and
    // rewrite its ancestry. What survives `--sparse` is the parent pruning
    // `try_to_simplify_commit()` does in place, so a merge that is TREESAME to
    // one parent is still shown but the sides it did not take are not walked.
    let mut dense = true;
    let decorations_for_simplify: Option<Decorations>;
    let mut min_parents: Option<usize> = None;
    let mut max_parents: Option<usize> = None;
    let mut date_mode = cfg_date_mode;
    let mut show_root = cfg_show_root;
    let mut color = ColorWhen::Auto;
    let mut order = Order::Default;
    // `git_log_output_encoding` (environment.c:51), set by `--encoding=<enc>`.
    let mut log_encoding: Option<String> = None;
    // `diff_options.anchors` — the repeatable `--anchored=<text>` list.
    let mut anchors: Vec<String> = Vec::new();
    let mut revs: Vec<String> = Vec::new();
    // Parallel to `revs`: whether a `--not` was in force when it was read, which
    // reverses the sense the `^` prefix would otherwise give it.
    let mut rev_negated: Vec<bool> = Vec::new();
    let mut pathspecs: Vec<String> = Vec::new();
    // History filtering (`--grep`/`--author`/`--committer` + dialect flags),
    // matched through the shared `revfilter` so log and shortlog agree.
    let mut grep_pats: Vec<String> = Vec::new();
    let mut author_pats: Vec<String> = Vec::new();
    let mut committer_pats: Vec<String> = Vec::new();
    let mut grep_dialect = crate::revfilter::Dialect::Basic;
    let mut grep_ignore_case = false;
    let mut grep_all_match = false;
    let mut grep_invert = false;
    // `--since`/`--after` and `--until`/`--before` commit-date range (committer
    // time), parsed with git's approxidate.
    let mut since: Option<i64> = None;
    let mut until: Option<i64> = None;
    // Pickaxe: `-S<string>` (net occurrence count changed) / `-G<regex>` (a
    // changed line matches). Both diff each commit against its first parent.
    let mut pickaxe_s: Option<String> = None;
    let mut pickaxe_g: Option<String> = None;
    // `--pickaxe-regex` promotes `-S`'s literal to a regex whose *match count* is
    // compared; `--pickaxe-all` keeps the whole changeset when any pair matches;
    // `--find-object=<id>` selects pairs that touch a named object instead of
    // searching content. All three are read before the needle they modify is
    // finalised, because git accepts them on either side of `-S`.
    let mut pickaxe_regex = false;
    let mut pickaxe_all = false;
    let mut find_object: Vec<String> = Vec::new();
    // The diff options the per-commit patch is rendered with; `-U3` and no whitespace
    // folding until a flag says otherwise.
    let mut patch_opts = super::diff::PatchOpts::default();
    // `--color-moved*` / `--word-diff*` / `--color-words`, layered over
    // `diff.colorMoved` / `diff.colorMovedWS` / `diff.wordRegex` once the repository
    // is readable.
    let mut move_word = diff_color::MoveWordOpts::default();
    // The `options->use_color = GIT_COLOR_ALWAYS` the two color spellings of the
    // word-diff family set; folded into `color` as soon as it is seen.
    let mut move_word_color: Option<diff_color::ColorWhen> = None;
    // `--ws-error-highlight=<kind>`. `None` leaves `diff.wsErrorHighlight` (and then
    // git's `WSEH_NEW` default) in charge.
    let mut ws_error_highlight: Option<u32> = None;
    // `--diff-merges=<mode>`: what a *merge* commit's patch shows. git's default is
    // `off`, which is why `git log -p` prints no diff for a merge at all.
    let mut diff_merges = DiffMerges::Off;
    // `revs->explicit_diff_merges` (`diff_merges_parse_opts()` sets it after every
    // branch it took, diff-merges.c:149). It is what
    // `diff_merges_default_to_first_parent()` consults, so `--no-diff-merges
    // --first-parent` keeps a merge diffless while `--first-parent` alone does not.
    let mut explicit_diff_merges = false;
    // `revs->merges_need_diff` / `revs->merges_imply_patch`. `common_setup()`
    // raises `merges_need_diff` for every mode but `off`, and `-m` clears it again
    // (diff-merges.c:125-127) — which is why `git log -m` alone still prints no
    // diff while `--diff-merges=separate` prints one. `diff_merges_setup_revs()`
    // then promotes either flag to the patch format, but only when no other format
    // claimed it (diff-merges.c:186-191):
    //
    // ```c
    // if (revs->merges_imply_patch || revs->merges_need_diff) {
    //         if (!revs->diffopt.output_format)
    //                 revs->diffopt.output_format = DIFF_FORMAT_PATCH;
    // }
    // ```
    //
    // That `if` is also what `cmd_whatchanged()`'s raw default tests, so an
    // explicit `--diff-merges=<mode>` takes `whatchanged` off raw and onto patches.
    let mut merges_need_diff = false;
    // `revs->merges_imply_patch`, which only the short spellings raise
    // (diff-merges.c:130-140). It differs from `merges_need_diff` in one way that
    // shows: `diff_merges_setup_revs()` also does `revs->diff = 1` for it
    // (diff-merges.c:186-187), so `git log -c` diffs *every* commit while
    // `git log --diff-merges=combined` diffs only the merges. `-c`, `--cc`, `--dd`
    // and `--remerge-diff` all raise it; `-m` is the one that does not
    // (diff-merges.c:125-127).
    let mut merges_imply_patch = false;
    // `--follow`: keep following the one pathspec across renames.
    let mut follow = false;
    // `log.follow`'s separate flag, mirroring git's `default_follow_renames`:
    // `--no-follow` clears BOTH (diff.c:5192-5194), so an explicit negation beats
    // the config, while `--follow` leaves this one alone.
    let mut default_follow = cfg_follow;
    // `-L<range>:<file>`, repeatable: line-level traversal (see `line_log`).
    let mut line_ranges: Vec<String> = Vec::new();
    // `-s`/`--no-patch` resets the diff output format to git's NO_OUTPUT. That is a
    // non-empty format, so `-L` does not fall back to its `DIFF_FORMAT_PATCH`
    // default after one — even though every individual format flag is off again.
    let mut saw_no_patch = false;

    // `setup_revisions()`'s `seen_dashdash`, which it sets in a scan of its own
    // *before* it resolves anything — so it is in force for the arguments
    // standing in front of the separator too, as `REVARG_CANNOT_BE_FILENAME`.
    let mut seen_dashdash = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            // Everything after `--` is a pathspec, even tokens that look like
            // flags — git stops option parsing at the separator.
            seen_dashdash = true;
            pathspecs.extend(args[i + 1..].iter().cloned());
            break;
        }
        // parse_options_step()'s `internal_help`, which `cmd_log_init` runs
        // before `setup_revisions`: the block on stdout at 129, no `error:` line.
        if a == "-h" {
            return Ok(super::show_usage(USAGE));
        }
        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122), one exact `strcmp` after the `--` break above:
        // the same block with the hidden `--i-still-use-this` left in.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE_ALL));
        }
        // A short option that spends the *next* argv slot on its value, standing
        // last on the line: `get_arg()` (parse-options.c:59-60) — or, for `-n`,
        // `handle_revision_opt()`'s own `argc <= 1` check (revision.c) — refuses it
        // before the option's arm ever runs, so this has to come ahead of every
        // other decision about the argument.
        //
        // Each arm below reads its value as `args.get(i).unwrap_or_default()`,
        // which turns "you forgot the pattern" into "the pattern is the empty
        // string": `git log -S` searched for `""`, matched every commit and exited
        // 0 where git exits 129. The two tables that decide *which* refusal and
        // *which* status live in [`super::blame::trailing_option_missing_value`],
        // which is asked here rather than restated — `-S`, `-G`, `-I`, `-O` and
        // `-l` are parse-options' ``switch `<c>' requires a value`` at 129 while
        // `-n` is `revision.c`'s `error: -n requires an argument` at 128.
        //
        // Only short options are routed here. A long one is spelled `--name=<v>`
        // or takes the next slot too, and its arms are answered elsewhere; a
        // cluster (`-Sfoo`) already carries its value and the table declines it.
        //
        // A `--` in the value slot is not a value. `setup_revisions()` cuts the
        // option region at the separator before it parses a single option:
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
        // (`revision.c`.) So `git log -S --` is a missing value, not a search for
        // the string `--`. That cut is also exactly what separates these options
        // from `-L`: `-L` is `builtin_log_options`' own entry and is read in stage
        // 1, where the separator is still an ordinary argv slot — `git log -L --`
        // really does hand `--` to the range parser, and dies at 128 for a
        // malformed range rather than 129 for a missing value.
        let value_slot_empty = i + 1 == args.len() || args[i + 1] == "--";
        if value_slot_empty && a.starts_with('-') && !a.starts_with("--") {
            if let Some(code) = super::blame::trailing_option_missing_value(a)? {
                return Ok(code);
            }
        }
        // The value checks `diff_opt_parse`'s callbacks run as each option is seen.
        // `cmd_log` hands the whole argument list to `setup_revisions`, so a diff
        // option's value is validated here whether or not this command renders it.
        if let Some(line) = super::diff_optval::reject(a) {
            eprintln!("{line}");
            return Ok(ExitCode::from(129));
        }
        if a == "-n" || a == "--max-count" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
            match parse_max_count(v) {
                Ok(mc) => max_count = mc,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--max-count=") {
            match parse_max_count(v) {
                Ok(mc) => max_count = mc,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--decorate" {
            decorate = DecorateStyle::Short;
        } else if let Some(m) = a.strip_prefix("--decorate=") {
            match parse_decoration_style(m) {
                Some(s) => decorate = s,
                None => {
                    eprintln!("fatal: invalid --decorate option: {m}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--no-decorate" {
            decorate = DecorateStyle::Off;
        } else if a == "--decorate-refs" || a == "--decorate-refs-exclude" {
            // git's `OPT_STRING_LIST` also takes its value as the next argv token,
            // and its parse-options layer rejects a missing one with exit 129.
            i += 1;
            let Some(v) = args.get(i) else {
                eprintln!("error: option `{}' requires a value", &a[2..]);
                return Ok(ExitCode::from(129));
            };
            if a == "--decorate-refs" {
                decorate_refs.push(v.clone());
            } else {
                decorate_refs_exclude.push(v.clone());
            }
        } else if let Some(v) = a.strip_prefix("--decorate-refs=") {
            decorate_refs.push(v.to_string());
        } else if let Some(v) = a.strip_prefix("--decorate-refs-exclude=") {
            decorate_refs_exclude.push(v.to_string());
        } else if a == "--clear-decorations" {
            // git's `clear_decorations_callback`: forget every pattern given so
            // far and stop applying the default namespace filter, so refs outside
            // the known namespaces become decoratable.
            decorate_refs.clear();
            decorate_refs_exclude.clear();
            default_decoration_filter = false;
        } else if a == "--use-mailmap" || a == "--mailmap" {
            use_mailmap = true;
        } else if a == "--no-use-mailmap" || a == "--no-mailmap" {
            use_mailmap = false;
        } else if a == "--oneline" {
            pretty = Pretty::Oneline;
            terminator = true;
            abbrev_commit = true;
            pretty_given = true;
        // `--notes[=<ref>]` and its `--show-notes` spelling, plus `--no-notes`:
        // git`s `notes_callback`. A later flag overrides an earlier one, and an
        // explicit ref suppresses both the default tree and `notes.displayRef`.
        } else if a == "--notes" || a == "--show-notes" {
            notes_opt.enable_default();
            notes_opt.given = true;
        } else if let Some(v) = a
            .strip_prefix("--notes=")
            .or_else(|| a.strip_prefix("--show-notes="))
        {
            notes_opt.enable_ref(v);
            notes_opt.given = true;
        } else if a == "--no-notes" || a == "--no-show-notes" {
            notes_opt.disable();
            notes_opt.given = true;
        } else if let Some(v) = a.strip_prefix("--pretty=") {
            match get_commit_format(Some(&repo), v)? {
                Some((p, t)) => {
                    pretty = p;
                    terminator = t;
                    pretty_given = true;
                }
                None => {
                    eprintln!("fatal: invalid --pretty format: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--format=") {
            // `--format=<s>` is git`s alias for `--pretty=<s>` (same parser, not a
            // blind `tformat:` wrapper — `--format=abc` is rejected just like
            // `--pretty=abc`).
            match get_commit_format(Some(&repo), v)? {
                Some((p, t)) => {
                    pretty = p;
                    terminator = t;
                    pretty_given = true;
                }
                None => {
                    eprintln!("fatal: invalid --pretty format: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--pretty" {
            // Bare `--pretty` is git`s `--pretty=medium`.
            pretty = Pretty::Medium;
            terminator = false;
            pretty_given = true;
        } else if a == "--format" {
            // Bare `--format` (no `=value`) is a git usage error, exit 128.
            eprintln!("fatal: unrecognized argument: --format");
            return Ok(ExitCode::from(128));
        } else if a == "--skip" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
            match parse_skip(v) {
                Ok(n) => skip = n,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--skip=") {
            match parse_skip(v) {
                Ok(n) => skip = n,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--date=") {
            match parse_date_mode(v) {
                Some(m) => date_mode = m,
                None => {
                    eprintln!("fatal: unknown date format {v}");
                    return Ok(ExitCode::from(128));
                }
            }
            // `revs->date_mode_explicit` (revision.c): only the *option* sets it,
            // never `log.date`. It is what switches a `--walk-reflogs` selector
            // from `HEAD@{0}` to `HEAD@{<date>}`.
            date_explicit = true;
        } else if let Some(v) = a.strip_prefix("--min-parents=") {
            match parse_nonneg(v) {
                Some(n) => min_parents = Some(n),
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--max-parents=") {
            match parse_nonneg(v) {
                Some(n) => max_parents = Some(n),
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--no-min-parents" {
            min_parents = Some(0);
        } else if a == "--no-max-parents" {
            max_parents = None;
        } else if a == "--first-parent" {
            first_parent = true;
        } else if a == "--parents" {
            show_parents = true;
        } else if a == "--full-history" {
            full_history = true;
        } else if a == "--simplify-merges" {
            simplify_merges_opt = true;
            full_history = true;
            order = Order::Topo;
        } else if a == "--simplify-by-decoration" {
            simplify_by_decoration = true;
        } else if a == "--boundary" {
            boundary = true;
        } else if a == "--no-boundary" {
            boundary = false;
        } else if a == "--children" {
            show_children = true;
        } else if a == "--no-children" {
            show_children = false;
        } else if a == "--abbrev-commit" {
            abbrev_commit = true;
        } else if a == "--no-abbrev-commit" {
            abbrev_commit = false;
        // `--abbrev[=<n>]` / `--no-abbrev`: the length every abbreviated id in the
        // run is cut to. git clamps below `MINIMUM_ABBREV` (4) and at the hash
        // width, and `--no-abbrev` is the full width. It reaches `%h`, the
        // oneline id and the diff `index` lines — but not the `commit <id>`
        // header, which only `--abbrev-commit` shortens.
        } else if a == "--abbrev" {
            abbrev_len = Some(DEFAULT_ABBREV);
        } else if a == "--no-abbrev" {
            // `--no-abbrev` zeroes `revs->abbrev`, which every id-printing format
            // reads as "the whole thing" — except the `index` line, which falls back
            // to the configured default (pinned below, before `core.abbrev` moves).
            abbrev_len = Some(repo.object_hash().len_in_hex());
            patch_opts.index_abbrev =
                Some(crate::abbrev::configured_abbrev(&repo, repo.object_hash().len_in_hex()));
        } else if let Some(v) = a.strip_prefix("--abbrev=") {
            abbrev_len =
                Some(crate::abbrev::parse_abbrev_arg(v, repo.object_hash().len_in_hex()));
        } else if a == "-p" || a == "--patch" || a == "-u" {
            // `-u` is git's documented synonym for `-p`.
            patch = true;
        } else if a == "-q" || a == "--quiet" {
            // Position-independent NO_OUTPUT (git applies it before `setup_revisions`
            // parses `-p`/`--stat`), so a later or earlier format flag always wins.
            quiet = true;
        } else if a == "--no-quiet" {
            quiet = false;
        } else if a == "--source" {
            source_mode = true;
        } else if a == "--no-source" {
            source_mode = false;
        } else if a == "-L" {
            // git's `OPT_CALLBACK('L', ...)` takes its value as the next argv token
            // and its parse-options layer rejects a missing one with exit 129.
            i += 1;
            let Some(v) = args.get(i) else {
                eprintln!("error: switch `L' requires a value");
                return Ok(ExitCode::from(129));
            };
            line_ranges.push(v.clone());
        } else if let Some(v) = a.strip_prefix("-L") {
            line_ranges.push(v.to_string());
        } else if a == "-s" || a == "--no-patch" {
            // Suppress diff output — git treats `-s` as order-sensitive, so a
            // later `--stat`/`-p` re-enables whichever format follows it.
            saw_no_patch = true;
            check = false;
            stat = false;
            numstat = false;
            shortstat = false;
            summary = false;
            raw = false;
            name_only = false;
            name_status = false;
            patch = false;
        } else if let Some(v) = a.strip_prefix("--line-prefix=") {
            line_prefix = v.as_bytes().to_vec();
        } else if a == "--check" {
            check = true;
        } else if a == "--exit-code" {
            exit_code = true;
        } else if a == "--no-exit-code" {
            exit_code = false;
        } else if a == "--name-only" {
            name_only = true;
        } else if a == "--name-status" {
            name_status = true;
        } else if a == "--summary" {
            summary = true;
        } else if a == "--raw" {
            raw = true;
        } else if a == "--stat" {
            stat = true;
        // `diff_opt_parse`'s two combining spellings: each sets its own format bit
        // *and* `DIFF_FORMAT_PATCH`, so they are exactly `-p --raw` and `-p --stat`
        // (diff.c's `OPT_BITOP` entries for `patch-with-raw`/`patch-with-stat`).
        } else if a == "--patch-with-raw" {
            patch = true;
            raw = true;
        } else if a == "--patch-with-stat" {
            patch = true;
            stat = true;
        } else if let Some(v) = a.strip_prefix("--stat=") {
            // `--stat[=<width>[,<name-width>[,<count>]]]`: sets the total width (and
            // optionally the name column / line cap) and, like every `--stat*` flag,
            // requests the diffstat.
            stat = true;
            diffstat::parse_stat_geometry(&mut stat_widths, v);
        } else if let Some(v) = a.strip_prefix("--stat-width=") {
            stat = true;
            stat_widths.width = parse_stat_i64(v);
        } else if let Some(v) = a.strip_prefix("--stat-name-width=") {
            stat = true;
            stat_widths.name_width = parse_stat_i64(v);
        } else if let Some(v) = a.strip_prefix("--stat-graph-width=") {
            stat = true;
            stat_widths.graph_width = parse_stat_i64(v);
        } else if let Some(v) = a.strip_prefix("--stat-count=") {
            stat = true;
            stat_widths.count = parse_stat_i64(v);
        } else if a == "--numstat" {
            numstat = true;
        } else if a == "--shortstat" {
            shortstat = true;
        } else if a == "--root" {
            // Force the root commit's diff on (a diff against the empty tree),
            // overriding `log.showRoot=false`. git has no `--no-root`.
            show_root = true;
        } else if a == "--graph" {
            graph = true;
        // The ref-selecting pseudo-options. Each consumes and clears whatever
        // `--exclude` patterns had accumulated (`clear_ref_exclusions`), and each
        // takes the `UNINTERESTING` flag `--not` is holding, so `--not --all`
        // hides every ref instead of walking from it.
        } else if let Some((sel, pattern)) = ref_selector(a) {
            // `--glob` is a `parse_long_opt()` option, so its value may stand as
            // the next argv element; the fixed spellings never take one.
            let detached = if sel == RefSelector::Glob && pattern.is_none() {
                i += 1;
                match args.get(i) {
                    Some(v) => Some(v.clone()),
                    None => {
                        eprintln!("error: option 'glob' requires a value");
                        return Ok(ExitCode::from(128));
                    }
                }
            } else {
                None
            };
            if sel == RefSelector::All {
                all = true;
            }
            // Under `--not` these refs are pended UNINTERESTING, which clears
            // `revs->no_walk` exactly as a `^<rev>` does.
            if negate_revs {
                no_walk = None;
            }
            ref_selections.push(RefSelection::new(
                revs.len(),
                sel,
                pattern.or(detached.as_deref()),
                std::mem::take(&mut ref_excludes),
                negate_revs,
            ));
        // ```c
        // } else if (!strcmp(arg, "--reflog")) {
        //         add_reflogs_to_pending(revs, *flags);
        // ```
        //
        // (revision.c:2766-2767.) It is a pseudo-option like the ref selectors
        // above, and it lands in the same pending list at the same argv position —
        // but it reads no pattern, and the `--exclude` patterns standing beside it
        // are neither applied nor cleared. `*flags` is what `--not` is holding, so
        // `--not --reflog` pends every reflog id UNINTERESTING, and that (through
        // `add_pending_object_with_path()`) is what clears `revs->no_walk`.
        } else if a == "--reflog" {
            if negate_revs {
                no_walk = None;
            }
            reflog_selections.push((revs.len(), negate_revs));
        // `revs->encode_email_headers` (revision.c:2526-2529). Only the mail
        // pretty formats read it, and the last spelling on the line wins.
        } else if a == "--encode-email-headers" {
            encode_email_headers = true;
        } else if a == "--no-encode-email-headers" {
            encode_email_headers = false;
        // `--exclude=<glob>` only accumulates; the next ref-selecting option
        // applies and clears it, and anything else leaves it alone.
        } else if a == "--exclude" || a.starts_with("--exclude=") {
            let v = match a.strip_prefix("--exclude=") {
                Some(v) => v.to_string(),
                None => {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        None => {
                            eprintln!("error: option 'exclude' requires a value");
                            return Ok(ExitCode::from(128));
                        }
                    }
                }
            };
            ref_excludes.push(v);
        // Options `git log` does not have, and whose owners are elsewhere:
        // `--timestamp` belongs to `rev-list`, `--no-stat` to `merge`/`pull`. git's
        // `setup_revisions()` leaves them unconsumed and `cmd_log_walk()` dies on the
        // first one, so this is git's own refusal rather than an unported feature.
        } else if a == "--timestamp" || a == "--no-stat" {
            eprintln!("fatal: unrecognized argument: {a}");
            return Ok(ExitCode::from(128));
        } else if a == "--stdin" {
            read_stdin = true;
        } else if a == "--not" {
            negate_revs = !negate_revs;
        } else if a == "--no-walk" {
            no_walk = Some(NoWalk::Sorted);
        } else if let Some(v) = a.strip_prefix("--no-walk=") {
            match v {
                "sorted" => no_walk = Some(NoWalk::Sorted),
                "unsorted" => no_walk = Some(NoWalk::Unsorted),
                _ => {
                    eprintln!("fatal: invalid argument to --no-walk");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--do-walk" {
            no_walk = None;
        } else if a == "-g" || a == "--walk-reflogs" {
            // `init_reflog_walk(&revs->reflog_info)`: the walk stops being a
            // traversal of ancestry and becomes one of each named ref's reflog.
            walk_reflogs = true;
        } else if a == "--reverse" {
            // `revs->reverse ^= 1` (revision.c): a toggle, so an even number of
            // `--reverse`s leaves the walk in its original order.
            reverse = !reverse;
        } else if a == "--merges" {
            only_merges = true;
        } else if a == "--no-merges" {
            no_merges = true;
        } else if a == "--color" {
            // Bare `--color` is git's `--color=always`.
            color = ColorWhen::Always;
        } else if a == "--no-color" {
            color = ColorWhen::Never;
        } else if let Some(v) = a.strip_prefix("--color=") {
            match v {
                "always" => color = ColorWhen::Always,
                "never" => color = ColorWhen::Never,
                "auto" => color = ColorWhen::Auto,
                _ => {
                    eprintln!("fatal: invalid color value: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        // `revs->expand_tabs_in_log`: how wide a tab is when the message is
        // reprinted under a four-space indent, which shifts every tab stop the
        // author lined up against. A bare `--expand-tabs` is
        // `expand_tabs_in_log_default` (8) — the value the indenting formats
        // already use — `--no-expand-tabs` is zero, and an explicit value reaches
        // `raw` too, which indents without expanding by default.
        } else if a == "--expand-tabs" {
            expand_tabs = Some(8);
        } else if a == "--no-expand-tabs" {
            expand_tabs = Some(0);
        } else if let Some(v) = a.strip_prefix("--expand-tabs=") {
            // `git_parse_ulong` behind `OPT_INTEGER`, whose failure `cmd_log_init`
            // reports as a fatal rather than a usage error.
            match v.parse::<usize>() {
                Ok(n) => expand_tabs = Some(n),
                Err(_) => {
                    eprintln!("fatal: '{v}': not a non-negative integer");
                    return Ok(ExitCode::from(128));
                }
            }
        // `--color-moved[=<mode>]`, `--color-moved-ws=<modes>`, `--word-diff[=<mode>]`,
        // `--word-diff-regex=<re>` and `--color-words[=<re>]`. These do not change how
        // the patch is generated; they re-emit the assembled one, so they ride along on
        // [`super::diff::PatchOpts`] and are resolved once the repository is in hand.
        } else if let Some(res) = {
            // The two that are `OPT_STRING`/`OPT_CALLBACK` without `PARSE_OPT_OPTARG`
            // take the next argv entry when nothing is glued on.
            let glued = match diff_color::needs_separate_value(a) {
                true => {
                    i += 1;
                    match args.get(i) {
                        Some(v) => format!("{a}={v}"),
                        None => {
                            eprintln!("error: {}", diff_color::missing_value(a));
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
                false => a.clone(),
            };
            move_word.parse_flag(&glued, &mut move_word_color)
        } {
            if let Err(msg) = res {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            // `diff_opt_word_diff()` sets `options->use_color = GIT_COLOR_ALWAYS` for
            // the two color spellings, so a `--color-words` anywhere on the line turns
            // the whole of `git log`'s output — header included — colored.
            if move_word_color == Some(diff_color::ColorWhen::Always) {
                color = ColorWhen::Always;
                move_word_color = None;
            }
        // `--output-indicator-new`/`-old`/`-context=<char>` (`diff_opt_char()`,
        // diff.c:5593): one byte replaces the sign this side of a hunk line carries.
        } else if let Some(name) = indicator_name(a) {
            let val = match a.split_once('=') {
                Some((_, v)) => v.to_string(),
                None => {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        None => {
                            eprintln!("error: {}", diff_color::missing_value(name));
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
            };
            if val.len() > 1 {
                eprintln!("error: {} expects a character, got '{val}'", &name[2..]);
                return Ok(ExitCode::from(129));
            }
            let c = val.as_bytes().first().copied().unwrap_or(0);
            match name {
                "--output-indicator-new" => patch_opts.indicators.0 = c,
                "--output-indicator-old" => patch_opts.indicators.1 = c,
                _ => patch_opts.indicators.2 = c,
            }
        // `--ws-error-highlight=<kind>` (`diff_opt_ws_error_highlight()`), which
        // `emit_line_ws_markup()` reads when it decides whether a line's whitespace
        // errors are painted.
        } else if a == "--ws-error-highlight" || a.starts_with("--ws-error-highlight=") {
            let raw = match a.split_once('=') {
                Some((_, v)) => v.to_string(),
                None => {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        None => {
                            eprintln!("error: {}", diff_color::missing_value(a));
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
            };
            match diff_color::parse_ws_error_highlight(&raw) {
                Ok(v) => ws_error_highlight = Some(v),
                Err(accepted) => {
                    eprintln!("error: unknown value after ws-error-highlight={}", &raw[..accepted]);
                    return Ok(ExitCode::from(129));
                }
            }
        // `--dirstat[=<params>]` / `--dirstat-by-file[=<params>]` / `--cumulative`
        // (`diff_opt_dirstat()`, diff.c): each turns the format on, and the parameter
        // list is folded into the same `struct dirstat_opts` `git diff` fills.
        } else if let Some(v) = a.strip_prefix("--diff-filter=") {
            // `diff_opt_diff_filter()`: the letters accumulate across repeats, and
            // `diffcore_apply_filter()` drops every pair whose final status is not
            // selected.
            patch_opts.diff_filter.get_or_insert_with(Vec::new).extend_from_slice(v.as_bytes());
        } else if a == "--relative" {
            relative = Some(super::diff::cwd_prefix(&repo));
            no_relative_given = false;
        } else if a == "--no-relative" {
            relative = None;
            no_relative_given = true;
        } else if let Some(v) = a.strip_prefix("--relative=") {
            // git stores the prefix with a trailing slash so a plain prefix match
            // cannot cross a name boundary.
            let mut p = v.to_string();
            if !p.is_empty() && !p.ends_with('/') {
                p.push('/');
            }
            relative = Some(p);
            no_relative_given = false;
        } else if a == "--compact-summary" {
            compact_summary = true;
            stat = true;
        } else if a == "--no-compact-summary" {
            compact_summary = false;
        } else if a == "--dirstat" {
            dirstat_on = true;
        } else if a == "--dirstat-by-file" {
            dirstat_on = true;
            dirstat.by_file = true;
        } else if a == "--cumulative" {
            dirstat_on = true;
            dirstat.cumulative = true;
        } else if a.starts_with("--dirstat=") || a.starts_with("--dirstat-by-file=") {
            let by_file = a.starts_with("--dirstat-by-file=");
            let params = a.split_once('=').map(|(_, v)| v).unwrap_or_default();
            let errors = super::diff_files::parse_dirstat_params(params, &mut dirstat);
            if !errors.is_empty() {
                // `parse_dirstat_opt()`'s `die()`, carrying the accumulated text.
                eprint!("fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n");
                return Ok(ExitCode::from(128));
            }
            if by_file {
                dirstat.by_file = true;
            }
            dirstat_on = true;
        // `revision.c:2462-2465`. `--dense` restores the `repo_init_revisions()`
        // default, so it is only ever an undo of an earlier `--sparse`.
        } else if a == "--dense" {
            dense = true;
        } else if a == "--sparse" {
            dense = false;
        } else if a == "--date-order" {
            order = Order::Date;
        } else if a == "--topo-order" {
            order = Order::Topo;
        // ```c
        // } else if (!strcmp(arg, "--author-date-order")) {
        //         revs->sort_order = REV_SORT_BY_AUTHOR_DATE;
        //         revs->topo_order = 1;
        // ```
        // (`revision.c:2456-2458`.) Like `--date-order` it turns the topological
        // sort on; only the tie-break differs.
        } else if a == "--author-date-order" {
            order = Order::AuthorDate;
        } else if let Some(v) = a.strip_prefix("--grep=") {
            grep_pats.push(v.to_string());
        } else if a == "--grep" {
            i += 1;
            grep_pats.push(args.get(i).cloned().unwrap_or_default());
        } else if let Some(v) = a.strip_prefix("--author=") {
            author_pats.push(v.to_string());
        } else if a == "--author" {
            i += 1;
            author_pats.push(args.get(i).cloned().unwrap_or_default());
        } else if let Some(v) = a.strip_prefix("--committer=") {
            committer_pats.push(v.to_string());
        } else if a == "--committer" {
            i += 1;
            committer_pats.push(args.get(i).cloned().unwrap_or_default());
        } else if a == "-i" || a == "--regexp-ignore-case" {
            grep_ignore_case = true;
        } else if a == "-E" || a == "--extended-regexp" {
            grep_dialect = crate::revfilter::Dialect::Extended;
        } else if a == "-F" || a == "--fixed-strings" {
            grep_dialect = crate::revfilter::Dialect::Fixed;
        } else if a == "-P" || a == "--perl-regexp" {
            grep_dialect = crate::revfilter::Dialect::Perl;
        } else if a == "--basic-regexp" {
            grep_dialect = crate::revfilter::Dialect::Basic;
        } else if a == "--all-match" {
            grep_all_match = true;
        } else if a == "--invert-grep" {
            grep_invert = true;
        // `--max-age`/`--min-age` set the very same `revs->max_age`/`revs->min_age`
        // as `--since`/`--until` (revision.c:2379-2393); only the value parser
        // differs — [`parse_age`]'s raw epoch instead of `approxidate()`. Both
        // spellings take their value attached or as the next argv element, which is
        // `parse_long_opt()` (diff.c:5380-5399), and a missing one is its
        // `die("Option '--%s' requires a value")`.
        } else if a == "--max-age"
            || a == "--min-age"
            || a.starts_with("--max-age=")
            || a.starts_with("--min-age=")
        {
            let name = if a.starts_with("--max-age") { "max-age" } else { "min-age" };
            let value = match a.split_once('=') {
                Some((_, v)) => v.to_string(),
                None => {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        None => {
                            eprintln!("fatal: Option '--{name}' requires a value");
                            return Ok(ExitCode::from(128));
                        }
                    }
                }
            };
            let Ok(age) = parse_age(&value) else {
                eprintln!("fatal: '{value}': not a number of seconds since epoch");
                return Ok(ExitCode::from(128));
            };
            if name == "max-age" {
                since = age;
            } else {
                until = age;
            }
        } else if let Some(v) = a.strip_prefix("--since=").or_else(|| a.strip_prefix("--after=")) {
            since = Some(approxidate(v));
        } else if a == "--since" || a == "--after" {
            i += 1;
            since = Some(approxidate(&args.get(i).cloned().unwrap_or_default()));
        } else if let Some(v) = a
            .strip_prefix("--until=")
            .or_else(|| a.strip_prefix("--before="))
        {
            until = Some(approxidate(v));
        } else if a == "--until" || a == "--before" {
            i += 1;
            until = Some(approxidate(&args.get(i).cloned().unwrap_or_default()));
        } else if a == "-S" {
            i += 1;
            pickaxe_s = Some(args.get(i).cloned().unwrap_or_default());
        } else if let Some(v) = a.strip_prefix("-S") {
            pickaxe_s = Some(v.to_string());
        } else if a == "-G" {
            i += 1;
            pickaxe_g = Some(args.get(i).cloned().unwrap_or_default());
        } else if a == "--pickaxe-all" {
            pickaxe_all = true;
        } else if a == "--pickaxe-regex" {
            pickaxe_regex = true;
        } else if a == "--find-object" {
            i += 1;
            find_object.push(args.get(i).cloned().unwrap_or_default());
        } else if let Some(v) = a.strip_prefix("--find-object=") {
            find_object.push(v.to_string());
        } else if a == "--follow" {
            follow = true;
        } else if a == "--no-follow" {
            follow = false;
            default_follow = false;
        } else if a == "-m" {
            // `diff_merges_parse_opts()` (diff-merges.c:119-151): each spelling
            // selects a mode, and the last one wins. `-m` is the odd one out — it
            // runs `set_to_default()` and then clears `merges_need_diff` again, so
            // it selects the mode without asking for a patch format.
            diff_merges = DiffMerges::Separate;
            merges_need_diff = false;
            remerge = false;
            explicit_diff_merges = true;
            // `set_separate()`'s second statement, which is not about the diff at
            // all: `revs->simplify_history = 0` (diff-merges.c:38). It is why
            // `git log -m -- <path>` shows a merge that `git log -- <path>`
            // collapses onto the side it took.
            full_history = true;
        } else if a == "-c" {
            diff_merges = DiffMerges::Combined;
            merges_need_diff = true;
            merges_imply_patch = true;
            remerge = false;
            explicit_diff_merges = true;
        } else if a == "--cc" {
            diff_merges = DiffMerges::DenseCombined;
            merges_need_diff = true;
            merges_imply_patch = true;
            remerge = false;
            explicit_diff_merges = true;
        } else if a == "--dd" {
            // `set_first_parent()` (diff-merges.c:43-47) is `set_separate()` plus
            // `first_parent_merges`, so it clears `simplify_history` the way
            // `--diff-merges=separate` does — and, unlike `--first-parent`, it says
            // nothing about which parents the *walk* follows.
            diff_merges = DiffMerges::FirstParent;
            merges_need_diff = true;
            merges_imply_patch = true;
            remerge = false;
            explicit_diff_merges = true;
            full_history = true;
        } else if a == "--remerge-diff" {
            // `set_remerge_diff()` plus `merges_imply_patch = 1`
            // (diff-merges.c:137-139); refused where the remerge would run.
            diff_merges = DiffMerges::Separate;
            remerge = true;
            merges_need_diff = true;
            merges_imply_patch = true;
            explicit_diff_merges = true;
            full_history = true;
        } else if a == "--no-diff-merges" {
            diff_merges = DiffMerges::Off;
            merges_need_diff = false;
            remerge = false;
            explicit_diff_merges = true;
        } else if let Some(v) = a.strip_prefix("--diff-merges=") {
            match DiffMerges::parse(v) {
                Some(m) => {
                    diff_merges = m;
                    remerge = false;
                    // `set_none()` is the only `func_by_opt` arm that does not run
                    // `common_setup()`, so it is the only one that leaves
                    // `merges_need_diff` at zero.
                    merges_need_diff = m != DiffMerges::Off;
                    explicit_diff_merges = true;
                    // `set_separate()` and `set_first_parent()` (which calls it)
                    // also clear `revs->simplify_history`; `set_combined()`,
                    // `set_dense_combined()` and `set_none()` do not.
                    if matches!(m, DiffMerges::Separate | DiffMerges::FirstParent) {
                        full_history = true;
                    }
                }
                // `func_by_opt()` (diff-merges.c:82-83) maps `r`/`remerge` onto
                // `set_remerge_diff()`, which is `common_setup()` plus
                // `remerge_diff = 1` and `simplify_history = 0`. This port has no
                // merge engine to re-run, so the request is refused where
                // `do_remerge_diff()` would run (log-tree.c:1134-1142) rather than
                // at parse time — a walk that reaches no merge behaves exactly as
                // git's does, which is what the parse-time refusal got wrong.
                None if matches!(v, "r" | "remerge") => {
                    diff_merges = DiffMerges::Separate;
                    remerge = true;
                    merges_need_diff = true;
                    explicit_diff_merges = true;
                    full_history = true;
                }
                None => {
                    eprintln!("fatal: invalid value for '--diff-merges': '{v}'");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("-G") {
            pickaxe_g = Some(v.to_string());
        // The patch-shaping options `setup_revisions()` takes from `diff_opt_parse()`.
        } else if a == "--no-renames" {
            patch_opts.renames = Some(0);
        } else if a == "--renames" || a == "-M" || a == "--find-renames" {
            patch_opts.renames = Some(super::diffcore_rename::DETECT_RENAME);
        } else if let Some(v) = a
            .strip_prefix("-M")
            .filter(|v| !v.is_empty())
            .or_else(|| a.strip_prefix("--find-renames="))
        {
            let (score, rest) = super::diffcore_rename::parse_rename_score(v);
            if !rest.is_empty() {
                eprintln!("fatal: invalid argument to -M: {v}");
                return Ok(ExitCode::from(128));
            }
            patch_opts.renames = Some(super::diffcore_rename::DETECT_RENAME);
            patch_opts.rename_score = score;
        // `diff_opt_find_copies()`: a second `-C` is `--find-copies-harder`.
        } else if a == "-C" || a == "--find-copies" {
            patch_opts.rename_score = 0;
            if patch_opts.renames == Some(super::diffcore_rename::DETECT_COPY) {
                patch_opts.find_copies_harder = true;
            } else {
                patch_opts.renames = Some(super::diffcore_rename::DETECT_COPY);
            }
        } else if let Some(v) = a
            .strip_prefix("-C")
            .filter(|v| !v.is_empty())
            .or_else(|| a.strip_prefix("--find-copies="))
        {
            let (score, rest) = super::diffcore_rename::parse_rename_score(v);
            if !rest.is_empty() {
                eprintln!("fatal: invalid argument to -C: {v}");
                return Ok(ExitCode::from(128));
            }
            patch_opts.rename_score = score;
            if patch_opts.renames == Some(super::diffcore_rename::DETECT_COPY) {
                patch_opts.find_copies_harder = true;
            } else {
                patch_opts.renames = Some(super::diffcore_rename::DETECT_COPY);
            }
        // `--rename-empty` / `--no-rename-empty` (`o->flags.rename_empty`,
        // `diff_setup()`'s default 1): whether `record_if_better()` may pair an
        // empty blob, i.e. whether an empty file that moved reports as `R100` or as
        // a deletion plus an addition.
        } else if a == "--rename-empty" {
            patch_opts.rename_empty = true;
        } else if a == "--no-rename-empty" {
            patch_opts.rename_empty = false;
        } else if a == "--find-copies-harder" {
            patch_opts.find_copies_harder = true;
        } else if a == "--no-find-copies-harder" {
            patch_opts.find_copies_harder = false;
        // `diff_opt_break_rewrites()`: `-B[<n>][/<m>]`, packed as `n | (m << 16)`.
        } else if a == "-B" || a == "--break-rewrites" {
            patch_opts.break_opt = 0;
        } else if let Some(v) = a
            .strip_prefix("-B")
            .filter(|v| !v.is_empty())
            .or_else(|| a.strip_prefix("--break-rewrites="))
        {
            match super::diffcore_rename::parse_break_opt(v) {
                Ok(n) => patch_opts.break_opt = n,
                Err(()) => {
                    eprintln!("fatal: invalid argument to -B: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        // ```c
        // } else if ((argcount = parse_long_opt("encoding", argv, &optarg))) {
        //         free(git_log_output_encoding);
        //         if (strcmp(optarg, "none"))
        //                 git_log_output_encoding = xstrdup(optarg);
        //         else
        //                 git_log_output_encoding = xstrdup("");
        //         return argcount;
        // ```
        //
        // (`revision.c:2701-2707`.) `none` is stored as the *empty* string, which
        // `repo_logmsg_reencode()` reads as "hand the message back untouched" — it is
        // not the same as asking for UTF-8, which drops the `encoding` header.
        // `parse_long_opt()` takes both the attached and the separated spelling.
        } else if a == "--encoding" {
            i += 1;
            let v = args.get(i).cloned().unwrap_or_default();
            log_encoding = Some(if v == "none" { String::new() } else { v });
        } else if let Some(v) = a.strip_prefix("--encoding=") {
            log_encoding = Some(if v == "none" { String::new() } else { v.to_string() });
        } else if a == "-z" {
            z = true;
        } else if a == "-w" || a == "--ignore-all-space" {
            patch_opts.ws = super::diff::Whitespace::IgnoreAll;
        } else if a == "-b" || a == "--ignore-space-change" {
            patch_opts.ws = super::diff::Whitespace::IgnoreChange;
        } else if a == "--ignore-space-at-eol" {
            patch_opts.ws = super::diff::Whitespace::IgnoreAtEol;
        } else if a == "--ignore-cr-at-eol" {
            patch_opts.ws = super::diff::Whitespace::IgnoreCrAtEol;
        } else if a == "--full-index" {
            patch_opts.full_index = true;
        } else if a == "-a" || a == "--text" {
            patch_opts.text = true;
        // Diff-algorithm selection. `setup_revisions()` hands every unrecognised
        // token to `diff_opt_parse()` (revision.c:2721), so `log`/`show` take the
        // same four spellings `git diff` does; the last one on the line wins.
        } else if a == "--minimal" {
            patch_opts.algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal);
        } else if a == "--patience" {
            patch_opts.algorithm = Some(gix::diff::blob::Algorithm::Patience);
            // `diff_opt_patience()` frees every anchor named before it (`diff.c:5845-5853`).
            anchors.clear();
        // `--anchored=<text>` (`diff_opt_anchored()`, diff.c:5544-5556): repeatable, and
        // each occurrence re-pins the algorithm to patience. `setup_revisions()` hands
        // every unrecognised token to `diff_opt_parse()`, so `git log -p` takes it too.
        } else if let Some(v) = a.strip_prefix("--anchored=") {
            patch_opts.algorithm = Some(gix::diff::blob::Algorithm::Patience);
            anchors.push(v.to_string());
        } else if a == "--anchored" {
            i += 1;
            let Some(v) = args.get(i).cloned() else {
                eprintln!("error: option `anchored' requires a value");
                return Ok(ExitCode::from(129));
            };
            patch_opts.algorithm = Some(gix::diff::blob::Algorithm::Patience);
            anchors.push(v);
        } else if a == "--histogram" {
            patch_opts.algorithm = Some(gix::diff::blob::Algorithm::Histogram);
        } else if let Some(v) = a.strip_prefix("--diff-algorithm=") {
            // `diff_opt_diff_algorithm()`: an unknown name is a usage error (129).
            match super::diff_optval::parse_algorithm_value(v) {
                Some(alg) => patch_opts.algorithm = Some(alg),
                None => {
                    eprintln!("fatal: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"");
                    return Ok(ExitCode::from(129));
                }
            }
        } else if a == "--indent-heuristic" {
            patch_opts.indent_heuristic = true;
        } else if a == "--no-indent-heuristic" {
            patch_opts.indent_heuristic = false;
        } else if a == "--ignore-blank-lines" {
            patch_opts.blank_lines = true;
        // `-I<re>` / `--ignore-matching-lines=<re>` (`diff_opt_ignore_regex()`,
        // diff.c:5859): each pattern is `regcomp`ed with `REG_EXTENDED | REG_NEWLINE`
        // and appended to `options->ignore_regex`, so repeats accumulate. Its value is
        // required, which is why a separated `-I` eats the next argv slot even when that
        // slot looks like a revision.
        } else if a == "-I" || a == "--ignore-matching-lines" {
            i += 1;
            let pat = args.get(i).cloned().unwrap_or_default();
            match super::diff_pickaxe::compile_regex(pat.as_bytes()) {
                Ok(re) => patch_opts.ignore_lines.push(super::diff_pickaxe::Needle::Regex(re)),
                Err(_) => {
                    eprintln!("error: invalid regex given to -I: '{pat}'");
                    return Ok(ExitCode::from(129));
                }
            }
        } else if let Some(v) = a
            .strip_prefix("--ignore-matching-lines=")
            .or_else(|| if a.len() > 2 { a.strip_prefix("-I") } else { None })
        {
            match super::diff_pickaxe::compile_regex(v.as_bytes()) {
                Ok(re) => patch_opts.ignore_lines.push(super::diff_pickaxe::Needle::Regex(re)),
                Err(_) => {
                    eprintln!("error: invalid regex given to -I: '{v}'");
                    return Ok(ExitCode::from(129));
                }
            }
        } else if let Some(v) = a.strip_prefix("--inter-hunk-context=") {
            match v.parse::<usize>() {
                Ok(n) => patch_opts.inter_hunk_ctx = n,
                Err(_) => {
                    eprintln!("fatal: invalid argument to --inter-hunk-context: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--binary" {
            patch_opts.binary = true;
            // `diff_opt_binary()` (diff.c:5564) ends in `enable_patch_output()`, which
            // sets `DIFF_FORMAT_PATCH`. That is invisible to `log`, but decides
            // `whatchanged`: its raw listing is only the fallback for
            // `!rev.diffopt.output_format` (builtin/log.c:559-560), so `--binary`
            // replaces the raw records with a patch rather than adding to them.
            patch = true;
        // `--submodule[=<format>]`. A bare `--submodule` is `DIFF_SUBMODULE_LOG`
        // (diff.c:6269, whose `PARSE_OPT_OPTARG` default is "log"); an unknown value
        // is `parse_submodule_params()`'s usage error (129).
        } else if a == "--submodule" {
            patch_opts.submodule_format = super::diff::SubmoduleFormat::Log;
        } else if let Some(v) = a.strip_prefix("--submodule=") {
            match super::diff::parse_submodule_params(v) {
                Some(f) => patch_opts.submodule_format = f,
                None => {
                    eprintln!("fatal: bad --submodule argument: {v}");
                    return Ok(ExitCode::from(129));
                }
            }
        } else if a == "-D" || a == "--irreversible-delete" {
            patch_opts.irreversible_delete = true;
        } else if a == "-W" || a == "--function-context" {
            patch_opts.func_context = true;
        } else if a == "--no-function-context" {
            patch_opts.func_context = false;
        } else if a == "--no-prefix" {
            patch_opts.src_prefix.clear();
            patch_opts.dst_prefix.clear();
        } else if a == "--default-prefix" {
            patch_opts.src_prefix = b"a/".to_vec();
            patch_opts.dst_prefix = b"b/".to_vec();
        } else if let Some(v) = a.strip_prefix("--src-prefix=") {
            patch_opts.src_prefix = v.as_bytes().to_vec();
        } else if let Some(v) = a.strip_prefix("--dst-prefix=") {
            patch_opts.dst_prefix = v.as_bytes().to_vec();
        } else if let Some(v) = a.strip_prefix("-U").filter(|v| !v.is_empty()) {
            match v.parse::<u32>() {
                Ok(n) => patch_opts.ctx = n,
                Err(_) => {
                    eprintln!("fatal: invalid argument to -U: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
            // `diff_opt_unified()` (diff.c:5961) ends in `enable_patch_output()`,
            // same as `--binary` above.
            patch = true;
        } else if let Some(v) = a.strip_prefix("--unified=") {
            match v.parse::<u32>() {
                Ok(n) => patch_opts.ctx = n,
                Err(_) => {
                    eprintln!("fatal: invalid argument to --unified: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
            // `diff_opt_unified()` (diff.c:5961) ends in `enable_patch_output()`.
            patch = true;
        } else if let Some(body) = a.strip_prefix('-') {
            if let Some(num) = body.strip_prefix('n') {
                // `-nN` shorthand (e.g. `-n5`).
                match parse_max_count(num) {
                    Ok(mc) => max_count = mc,
                    Err(()) => {
                        eprintln!("fatal: '{num}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                }
            } else if !body.is_empty() && body.bytes().all(|c| c.is_ascii_digit()) {
                // `-N` shorthand (e.g. `-5`): show N commits, so N is positive.
                match parse_max_count(body) {
                    Ok(mc) => max_count = mc,
                    Err(()) => {
                        eprintln!("fatal: '{body}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                }
            } else if a == "--show-signature" {
                show_signature = true;
            } else if a == "--no-show-signature" {
                show_signature = false;
            } else if super::diff::history_noop_diff_option(a) {
                // Accepted and inert: each of these sets a `diff_options` field to the
                // value this port already runs at, so there is nothing to plumb and
                // nothing that could come out wrong. See the list's own documentation.
            } else if !git_log_knows(a) {
                // git has no such option, so this is git's own refusal rather than
                // an unported feature: `parse_options()` and `setup_revisions()`
                // both leave the token behind and `cmd_log_init_finish()` reports
                // the first survivor (`builtin/log.c:320`). Options git *does* have
                // fall through to the gap message below, which is the truthful
                // answer for them — borrowing git's wording there would claim git
                // rejects an option it accepts.
                eprintln!("fatal: unrecognized argument: {a}");
                return Ok(ExitCode::from(128));
            } else {
                bail!("unsupported flag {a:?}");
            }
        } else {
            // A non-flag token before `--` is a revision; git accepts several and
            // walks the union of their histories.
            if argument_excludes(a, negate_revs) {
                no_walk = None;
            }
            revs.push(a.clone());
            rev_negated.push(negate_revs);
        }
        i += 1;
    }

    // `setup_revisions()` runs `opt->tweak(revs)` (revision.c:3121-3122) once the
    // whole command line has been read, and `cmd_log` is the only caller here that
    // installs one:
    //
    // ```c
    // static void log_setup_revisions_tweak(struct rev_info *rev)
    // {
    //         ...
    //         if (rev->first_parent_only)
    //                 diff_merges_default_to_first_parent(rev);
    // }
    // ```
    //
    // (builtin/log.c:815-823, installed at builtin/log.c:846.) `cmd_whatchanged`
    // reaches `cmd_log_init` with a zeroed `setup_revision_opt` (builtin/log.c:545)
    // and therefore has no tweak at all — which is the whole reason
    // `git whatchanged --first-parent` prints no record for a merge while
    // `git log --raw --first-parent` prints one.
    //
    // ```c
    // void diff_merges_default_to_first_parent(struct rev_info *revs)
    // {
    //         if (!revs->explicit_diff_merges)
    //                 revs->separate_merges = 1;
    //         if (revs->separate_merges)
    //                 revs->first_parent_merges = 1;
    // }
    // ```
    //
    // It never touches `merges_need_diff`, so `--first-parent` alone still asks for
    // no output format: it decides only what a merge's diff *is*, once something
    // else has asked for one.
    if flavor == Flavor::Log && first_parent {
        if !explicit_diff_merges {
            diff_merges = DiffMerges::Separate;
        }
        if diff_merges == DiffMerges::Separate {
            diff_merges = DiffMerges::FirstParent;
        }
    }

    // `-L` (`rev->line_level_traverse`). git rejects the combinations it cannot
    // render in `setup_revisions`, before the pathspec check in `cmd_log_init_finish`.
    let line_level = !line_ranges.is_empty();
    if line_level {
        // git's allowed set is PATCH / NO_OUTPUT / RAW / NAME / NAME_STATUS /
        // SUMMARY; the count formats are not in it.
        if stat || numstat || shortstat {
            eprintln!("fatal: -L does not yet support the requested diff format");
            return Ok(ExitCode::from(128));
        }
        if !pathspecs.is_empty() {
            eprintln!("fatal: -L<range>:<file> cannot be used with pathspec");
            return Ok(ExitCode::from(128));
        }
        // `if (!revs->diffopt.output_format) output_format = DIFF_FORMAT_PATCH;`
        if !patch && !name_only && !name_status && !saw_no_patch && !quiet {
            patch = true;
        }
    }

    // git's `diff_setup_done` rejects using more than one of `--name-only`,
    // `--name-status`, `--check`, and `-s` (NO_OUTPUT) together. `--quiet` pre-sets
    // NO_OUTPUT, but the stat/patch output formats clear it again, so `--quiet`
    // counts toward this conflict only when none of them are present (matching
    // `git log --name-only --stat --quiet`, which git accepts).
    // `-s`/`--no-patch` and `-q`/`--quiet` both *assign* `DIFF_FORMAT_NO_OUTPUT`,
    // and only the `OPT_BITOP` formats clear it again — `-p`, `--numstat`,
    // `--shortstat`, `--summary`, `--patch-with-*` and `--stat`. `--raw`,
    // `--name-only`, `--name-status` and `--check` are plain `OPT_BIT`s, so they
    // leave the bit standing and land in this count beside it. That is why
    // `-s --check` and `-s --name-only` are the fatal while `--check -s` and
    // `--name-only --stat --quiet` are fine.
    let no_output_bit =
        (quiet || saw_no_patch) && !patch && !stat && !numstat && !shortstat && !summary;
    // The `-z` formats are the one shape the whole-record `--line-prefix` pass
    // cannot reproduce: git prefixes each NUL-terminated *record*, not each NUL, so
    // a `--numstat -z` rename (`<counts>\0<from>\0<to>\0`) carries one prefix where
    // splitting on NUL would write three. Refused rather than approximated.
    if !line_prefix.is_empty() && z {
        bail!("unsupported option --line-prefix with -z");
    }
    if name_only as u8 + name_status as u8 + no_output_bit as u8 + check as u8 > 1 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }

    // Collect the starting tips in git's order: the named revision (or HEAD),
    // then every ref sorted by full name, then HEAD again for `--all`.
    let mut tips: Vec<ObjectId> = Vec::new();
    // Parallel to `tips` and populated only under `--source`: the name each tip was
    // reached from (a rev argument, a full refname for `--all`, or `HEAD`). A commit
    // inherits the source of the tip that first reaches it during the walk.
    let mut tip_sources: Vec<String> = Vec::new();
    // Parallel to `tips`: the argument or refname each was named by, which is what
    // `check_single_commit`'s "More than one commit to dig from" reports under `-L`.
    let mut tip_names: Vec<String> = Vec::new();
    // Split each revision arg into positive tips and negative (excluded) tips to
    // support git's range forms: `A..B` (= `^A B`), `A...B` (symmetric difference —
    // exclude the merge-base), and a leading `^A`. An empty endpoint means `HEAD`
    // (`A..`, `..B`). Anything without `..`/`^` is a single positive tip, as before.
    // Each positive spec carries the index of the revision argument it came from, so
    // the ref-naming pseudo-options can be slotted back in at the position they were
    // written at: `setup_revisions()` appends to one `pending` list as it reads the
    // command line, and a tie in commit date is broken by that order.
    // `read_revisions_from_stdin()`: every line is another revision argument, until
    // a bare `--` turns the rest into pathspecs. They are appended after the ones
    // the command line named, which is where git puts them.
    // `read_revisions_from_stdin()` brackets its loop with
    // `cfg->warn_on_object_refname_ambiguity = 0`, so a name that arrives on stdin
    // never gets the ambiguity warning the same name on argv gets. The lines are
    // appended to `revs` and resolved further down rather than here, so the
    // boundary is remembered instead of the switch being held.
    let argv_revs = revs.len();
    if read_stdin {
        use std::io::Read as _;
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        let mut in_paths = false;
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            if in_paths {
                pathspecs.push(line.to_string());
            } else if line == "--" {
                in_paths = true;
            } else {
                // The same `handle_revision_arg()` reads these, so an exclusion
                // arriving on stdin cancels `--no-walk` like one on the command line.
                if argument_excludes(line, negate_revs) {
                    no_walk = None;
                }
                revs.push(line.to_string());
                rev_negated.push(negate_revs);
            }
        }
    }
    let mut pos_specs: Vec<(usize, String, String)> = Vec::new();
    let mut neg_ids: Vec<ObjectId> = Vec::new();
    // Commits whose parent list the command line has already caused to be read.
    // Only `--no-walk` cares (see [`no_walk_uninteresting`]); it is collected
    // here because that is where the revision arguments are navigated.
    let mut parsed: HashSet<ObjectId> = HashSet::new();
    // Resolve one endpoint onto the excluded side, reporting git's fatal if it is
    // not a revision. Ranges and `^`-prefixed arguments both land here. `token` is
    // the argument as written, which is what `setup_revisions()` names in the
    // message even when only one endpoint of a range failed.
    // git's `revs->rev_input_given`, for the arguments that resolve and then
    // leave nothing behind: a pending tree or blob is dropped by
    // `handle_commit()` but the flag was already set, so `revs->def` stays out.
    let mut rev_input_given = false;
    let mut resolve_neg = |spec: &str, token: &str, neg_ids: &mut Vec<ObjectId>| -> Option<ExitCode> {
        match resolve_rev(&repo, crate::objname::canonical_spec(&repo, spec).as_ref()) {
            Ok(id) => {
                // `handle_commit()` drops an UNINTERESTING tree or blob rather
                // than excluding anything by it, so `<tree>..HEAD` walks the
                // whole history instead of failing.
                neg_ids.extend(crate::objname::walk_pending(&repo, id));
                None
            }
            Err(_) => {
                // `setup_revisions()`'s `if (seen_dashdash || *arg == '^') die(_("bad
                // revision '%s'"), arg);` (revision.c:3035-3036): once a `--` has
                // been seen anywhere on the line the operand can no longer be a
                // pathspec, so the three-line `ambiguous argument` advice is not
                // printed.
                eprint!("{}", bad_revision_message_in_gated(&repo, token, seen_dashdash));
                Some(ExitCode::from(128))
            }
        }
    };
    for (at, (spec, negated)) in revs.iter().zip(rev_negated.iter().copied()).enumerate() {
        // The lines `--stdin` supplied are exempt from the ambiguity half, which
        // is what `read_revisions_from_stdin()` clears the switch for.
        warn_operand(&repo, spec, at < argv_revs);
        // `handle_revision_arg_1()`'s guard ahead of `handle_dotdot()`: a bare
        // `..` is the pathspec for the parent directory, not `HEAD..HEAD`. It
        // falls through to the plain branch, fails to resolve, and is taken as a
        // path — which the pathspec layer then rejects for leaving the
        // repository. See [`crate::objname::is_parent_directory_pathspec`].
        if crate::objname::is_parent_directory_pathspec(spec, seen_dashdash) {
            pos_specs.push((at, spec.to_string(), spec.to_string()));
        } else if let Some((a, b)) = spec.split_once("...") {
            let a = if a.is_empty() { "HEAD" } else { a };
            let b = if b.is_empty() { "HEAD" } else { b };
            parsed.extend(navigation_path(&repo, a));
            parsed.extend(navigation_path(&repo, b));
            pos_specs.push((at, a.to_string(), a.to_string()));
            pos_specs.push((at, b.to_string(), b.to_string()));
            // `A...B` hides what both endpoints can reach: their merge-base.
            if let (Ok(ia), Ok(ib)) = (resolve_rev(&repo, a), resolve_rev(&repo, b)) {
                if let Ok(base) = repo.merge_base(ia, ib) {
                    let base = base.detach();
                    neg_ids.push(base);
                    // `paint_down_to_common()` parses its way from both endpoints
                    // down past the bases, so a merge base's whole ancestry is
                    // loaded by the time `mark_parents_uninteresting()` runs.
                    parsed.extend(ancestor_closure(&repo, &[base])?);
                }
            }
        } else if let Some((a, b)) = spec.split_once("..") {
            let a = if a.is_empty() { "HEAD" } else { a };
            let b = if b.is_empty() { "HEAD" } else { b };
            parsed.extend(navigation_path(&repo, a));
            parsed.extend(navigation_path(&repo, b));
            // `A..B` is `^A B`; under `--not` each endpoint takes the other side.
            let (kept, excluded) = if negated { (a, b) } else { (b, a) };
            if let Some(code) = resolve_neg(excluded, spec, &mut neg_ids) {
                return Ok(code);
            }
            pos_specs.push((at, kept.to_string(), kept.to_string()));
        } else {
            // `handle_revision_arg_1()`'s parent-mark block (revision.c:2178-2207),
            // decoded once for every verb by [`crate::objname::parents_only`]:
            // `<rev>^@` pends the parents alone and claims the operand, while
            // `<rev>^!` and `<rev>^-<n>` pend the selected parents with
            // `flags ^ (UNINTERESTING | BOTTOM)` and then put the truncated name
            // back in `arg`, so the commit itself is pended after them.
            //
            // The mark is found with `strstr`'s first-match rule rather than by
            // stripping a suffix, which is why `main^!^!` carries no mark at all
            // and fails as an ordinary revision.
            let mut spec: &str = spec.as_str();
            match crate::objname::parents_only(spec) {
                crate::objname::ParentsOnly::Absent => {}
                // `strtol_i()` refused the `<n>`, so `add_parents_only()` is never
                // reached: `ret = -1` and the operand is diagnosed as written.
                crate::objname::ParentsOnly::BadParent => {
                    eprint!("{}", bad_revision_message_in_gated(&repo, spec, seen_dashdash));
                    return Ok(ExitCode::from(128));
                }
                crate::objname::ParentsOnly::Mark { base, nth, replaces } => {
                    // `^@` keeps `flags`; `^!` and `^-<n>` pass
                    // `flags ^ (UNINTERESTING | BOTTOM)`.
                    let sense = if replaces { negated } else { !negated };
                    let mut queued: Vec<(String, ObjectId, bool)> = Vec::new();
                    let mut queue = |name: &str, id: ObjectId, not: bool| {
                        queued.push((name.to_string(), id, not));
                    };
                    let answer =
                        crate::objname::add_parents_only(&repo, base, sense, nth, &mut queue);
                    match answer {
                        // `get_reference()`'s `die(_("bad object %s"), name)`,
                        // naming the base with its leading `^` already stripped.
                        crate::objname::Parents::BadObject => {
                            let name = crate::objname::uninteresting_mark(base).0;
                            eprintln!("fatal: bad object {name}");
                            return Ok(ExitCode::from(128));
                        }
                        // `return 0` leaves `arg` alone, and an operand that still
                        // carries a mark cannot resolve — `get_oid_1()` has no case
                        // for `^@`, `^!` or `^-<n>` — so this is the bad-revision
                        // fatal rather than a fall-through.
                        crate::objname::Parents::None => {
                            eprint!(
                                "{}",
                                bad_revision_message_in_gated(&repo, spec, seen_dashdash)
                            );
                            return Ok(ExitCode::from(128));
                        }
                        crate::objname::Parents::Queued => {
                            parsed.extend(navigation_path(&repo, base));
                            for (name, id, not) in queued {
                                if not {
                                    neg_ids.push(id);
                                } else {
                                    pos_specs.push((at, id.to_string(), name));
                                }
                            }
                            // `if (add_parents_only(…)) { ret = 0; goto out; }` —
                            // `^@` claimed the operand and never pends the commit.
                            if replaces {
                                continue;
                            }
                            // `arg = arg_minus_excl;` / `arg = arg_minus_dash;`.
                            // The base may still carry its own leading `^`, which
                            // the exclusion step below strips a *second* time.
                            spec = base;
                        }
                    }
                }
            }
            // A plain revision is a tip and `^rev` excludes one; `--not` reverses
            // both readings, which is all `handle_revision_arg()` does with its
            // `UNINTERESTING` flip.
            let bare = spec.strip_prefix('^').unwrap_or(spec);
            parsed.extend(navigation_path(&repo, bare));
            if spec.starts_with('^') != negated {
                if let Some(code) = resolve_neg(bare, spec, &mut neg_ids) {
                    return Ok(code);
                }
                // `revs->rev_input_given` is set by `handle_revision_arg()` as
                // soon as the operand resolves, whichever side it lands on. An
                // excluded *tree* leaves nothing behind — `handle_commit()` drops
                // it — but the flag stands, so `git log ^main^{tree}` walks
                // nothing rather than falling back to `revs->def`.
                rev_input_given = true;
            } else {
                pos_specs.push((at, bare.to_string(), bare.to_string()));
            }
        }
    }
    // `add_reflog_for_walk()`: `if (commit->object.flags & UNINTERESTING) die("cannot
    // walk reflogs for %s", name)`. A reflog walk starts from a ref's log, and an
    // excluded tip has none to start from — git raises it the moment the argument is
    // pended, so it beats every post-loop conflict check below.
    if walk_reflogs {
        if let Some(name) = reflog_excluded_tip(&repo, &revs, &rev_negated) {
            eprintln!("fatal: cannot walk reflogs for {name}");
            return Ok(ExitCode::from(128));
        }
    }
    // git resolves each positional token as a revision; the first that is *not* a
    // revision but names an existing path switches to pathspec mode — that token and
    // every one after it become pathspecs, exactly as if a `--` had preceded them
    // (so `git log .` == `git log -- .`). A token that is neither a revision nor a
    // path is the "ambiguous argument" fatal.
    // Every ref, sorted by full name: git walks `refs/` in that order, which decides
    // the tie-break between tips that share a commit date. Materialised once, and only
    // when a ref-naming option asked for it — the iterator holds the packed-refs
    // buffer, which would block the per-ref object lookups.
    let ref_names: Vec<String> = if !ref_selections.is_empty() {
        let mut names: Vec<Vec<u8>> = Vec::new();
        for r in repo.references()?.all()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            names.push(r.name().as_bstr().to_vec());
        }
        names.sort();
        names.iter().filter_map(|n| n.to_str().ok().map(str::to_owned)).collect()
    } else {
        Vec::new()
    };
    // `add_reflogs_to_pending()`'s ids, read once and replayed at each `--reflog`.
    // git re-walks `$GIT_DIR/logs` per occurrence, which only shows in the
    // pruned-commit warning a second `--reflog` would repeat; the ids themselves
    // are the same list either way.
    let reflog_tips: Vec<ObjectId> = if reflog_selections.is_empty() {
        Vec::new()
    } else {
        super::shortlog::reflog_pending(&repo)?
    };
    // Append the tips of the ref-selecting pseudo-options that stood at argument
    // index `at`. Each yields its refs in refname order — the ref iterator's own
    // order, which is what breaks a commit-date tie between two of them.
    let mut push_ref_tips = |at: usize,
                             tips: &mut Vec<ObjectId>,
                             tip_names: &mut Vec<String>,
                             tip_sources: &mut Vec<String>,
                             neg_ids: &mut Vec<ObjectId>| {
        for sel in ref_selections.iter().filter(|s| s.at == at) {
            // `handle_one_ref()` names each pending object by the name the
            // iterator handed it: trimmed for `--branches`/`--tags`/`--remotes`,
            // the full refname for `--all`/`--glob`. That is what `--source`
            // prints.
            let mut pend = |oid: ObjectId,
                            name: &str,
                            tips: &mut Vec<ObjectId>,
                            tip_names: &mut Vec<String>,
                            tip_sources: &mut Vec<String>,
                            neg_ids: &mut Vec<ObjectId>| {
                if sel.negated {
                    neg_ids.push(oid);
                    return;
                }
                tips.push(oid);
                tip_names.push(name.to_string());
                if source_mode {
                    tip_sources.push(name.to_string());
                }
            };
            for full in &ref_names {
                let Some(name) = sel.selects(full) else {
                    continue;
                };
                let Ok(reference) = repo.find_reference(full.as_str()) else {
                    continue;
                };
                let Ok(id) = reference.into_fully_peeled_id() else {
                    continue;
                };
                let oid = id.detach();
                // A tag pointing at a tree or blob is not a history tip.
                if !repo.find_object(oid).is_ok_and(|o| o.kind == gix::objs::Kind::Commit) {
                    continue;
                }
                pend(oid, name, tips, tip_names, tip_sources, neg_ids);
            }
            // `handle_refs(refs, revs, flags, refs_head_ref)`: `--all` pends
            // `HEAD` too, after the ref list and under that literal name — which
            // is why a `refs/…` exclusion pattern never removes it.
            if sel.head && !sel.excluded("HEAD") {
                if let Some(id) = repo.head().ok().and_then(|mut h| h.try_peel_to_id().ok().flatten())
                {
                    pend(id.detach(), "HEAD", tips, tip_names, tip_sources, neg_ids);
                }
            }
        }
        // `handle_one_reflog_commit()` pends each id under the empty name
        // (`add_pending_object(cb->all_revs, o, "")`), so `--source` reports
        // nothing for a commit reached this way — unlike `--all`, which names the
        // ref.
        for negated in reflog_selections.iter().filter(|(i, _)| *i == at).map(|(_, n)| *n) {
            for oid in &reflog_tips {
                if negated {
                    neg_ids.push(*oid);
                    continue;
                }
                tips.push(*oid);
                tip_names.push(String::new());
                if source_mode {
                    tip_sources.push(String::new());
                }
            }
        }
    };

    // git resolves each positional token as a revision; the first that is *not* a
    // revision but names an existing path switches to pathspec mode — that token and
    // every one after it become pathspecs, exactly as if a `--` had preceded them
    // (so `git log .` == `git log -- .`). A token that is neither a revision nor a
    // path is the "ambiguous argument" fatal.
    let mut in_paths = false;
    let mut specs = pos_specs.iter().peekable();
    for at in 0..=revs.len() {
        push_ref_tips(at, &mut tips, &mut tip_names, &mut tip_sources, &mut neg_ids);
        // `handle_dotdot()` is the first thing `handle_revision_arg_1()` tries, so a
        // range whose endpoints resolve as names but not as usable objects dies here
        // — before this token is read as a tip and long before the walk finds out.
        // Checked per argument, in argument order, which is the order git dies in.
        if !in_paths {
            if let Some(msg) = revs.get(at).and_then(|t| crate::objname::dotdot_fatal(&repo, t)) {
                eprint!("{msg}");
                return Ok(ExitCode::from(128));
            }
        }
        // `append_prune_data(&prune_data, argv + i)` prunes with the *arguments*,
        // not with the endpoints a range split produced — so a token that failed
        // as a revision becomes the pathspec it was written as. `../..` splits
        // into `HEAD` and `/..`, and it is `../..` that git names when the
        // pathspec layer rejects it.
        let mut prune = |pathspecs: &mut Vec<String>, fallback: &String| {
            let token = revs.get(at).unwrap_or(fallback);
            if pathspecs.last() != Some(token) {
                pathspecs.push(token.clone());
            }
        };
        while let Some((_, spec, name)) = specs.next_if(|(i, _, _)| *i == at) {
            if in_paths {
                prune(&mut pathspecs, spec);
                continue;
            }
            // `get_oid_with_context()` rewrites a `./`/`../` path arm against the
            // prefix and folds the `@{u}` family before anything is looked up;
            // gitoxide's parser does neither, so `git log main:./f` and
            // `git log main@{u}` under `branch.<n>.remote = .` were refused here.
            // `get_oid_basic()` resolves `<ref>@{<n>}` / `<ref>@{<date>}` itself —
            // `repo_dwim_log()` and then `read_ref_at()` (`object-name.c:742-789`)
            // — and gitoxide's revspec grammar does not agree with it. gitoxide
            // hands back the selected entry's raw *new* id, where `read_ref_at()`
            // answers with the ref's current value whenever the entry one newer is
            // a creation; after a `git branch -m` round trip that raw id is the
            // null id, so `git log HEAD@{1}` walked nothing at all.
            // The test is on the *reduced* name: `HEAD@{<n>}~1` reaches
            // `get_oid_basic()` as `HEAD@{<n>}` and walks from what came back.
            // See [`crate::objname::reflog_spec_oid`].
            let resolved = if crate::objname::resolves_through_reflog(spec) {
                crate::objname::reflog_spec_oid(&repo, spec).ok_or(())
            } else {
                repo.rev_parse_single(crate::objname::canonical_spec(&repo, spec).as_ref())
                    .map(|id| id.detach())
                    .map_err(|_| ())
            };
            match resolved {
                Ok(id) => {
                    // `prepare_revision_walk()`'s `handle_commit()` peels a tag
                    // and drops a tree or a blob without a word — the pending
                    // entry disappears, name and all, and the command still
                    // exits 0. `git log <tree>`, `git log <blob>` and
                    // `git log HEAD..<tag-of-a-tree>` all print nothing in stock
                    // 2.55.0 rather than failing.
                    //
                    // The argument still counts as revision input: git sets
                    // `revs->rev_input_given` in `handle_revision_arg()` as soon
                    // as `handle_revision_arg_1()` returns 0, which is long
                    // before the walk drops the entry. So `git log <tree>` walks
                    // nothing at all rather than falling back to `revs->def`.
                    rev_input_given = true;
                    let Some(commit) = crate::objname::walk_pending(&repo, id) else {
                        continue;
                    };
                    tips.push(commit);
                    tip_names.push(name.clone());
                    if source_mode {
                        tip_sources.push(name.clone());
                    }
                }
                Err(_) if spec_is_path(&repo, spec) => {
                    in_paths = true;
                    prune(&mut pathspecs, spec);
                }
                Err(_) => {
                    // `setup_revisions()` names the argument as written, so a range
                    // whose endpoint failed is reported whole.
                    let token = revs.get(at).map(String::as_str).unwrap_or(spec.as_str());
                    eprint!("{}", bad_revision_message_in_gated(&repo, token, seen_dashdash));
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }
    // git's `rev_input_given`: `setup_revisions()` falls back to `HEAD` only when the
    // command line named no revision at all. An argument that named one and excluded
    // it (`^rev`, `--not rev`) still counts, so `git log ^feature` walks nothing
    // rather than quietly walking `HEAD`. So does a namespace option that selected
    // nothing: `--remotes` in a repository with no remotes is still an input.
    // A token that turned out to be a pathspec is not — `git log .` walks `HEAD`.
    let positive_from_args = rev_input_given
        || !tips.is_empty()
        || !neg_ids.is_empty()
        || !ref_selections.is_empty()
        || !reflog_selections.is_empty();
    if !positive_from_args {
        let head = repo.head()?;
        if head.is_unborn() && !all {
            let branch = head
                .referent_name()
                .map(|n| n.shorten().to_str_lossy().into_owned())
                .unwrap_or_else(|| "master".to_owned());
            eprintln!("fatal: your current branch '{branch}' does not have any commits yet");
            return Ok(ExitCode::from(128));
        }
        if let Some(id) = repo.head()?.try_peel_to_id()? {
            tips.push(id.detach());
            tip_names.push("HEAD".to_string());
            if source_mode {
                tip_sources.push("HEAD".to_string());
            }
        }
    }

    // The option combinations `setup_revisions()` refuses once it has finished
    // reading the command line, in its order. They come after the revisions are
    // resolved because git resolves them first, in the argument loop: an
    // unreadable revision and an unborn `HEAD` both report themselves before any
    // of these, and `--graph --reverse <bogus-rev>` names the revision.
    //
    // `revision.c` guards the ancestry decorations with
    // `revs->rewrite_parents && revs->children.name` rather than with `--parents`
    // itself: `--simplify-merges` and `--simplify-by-decoration` set
    // `rewrite_parents` where they are parsed and `revision_opts_finish()` sets it
    // for `--graph`, so each of those conflicts with `--children` exactly as
    // `--parents` does. The two decorations share one slot in the header and git
    // refuses to print both rather than pick an order.
    // `if (revs->reflog_info && revs->limited) die(...)` — a reflog walk hands its
    // entries out in reflog order, so anything that makes git build a *limited*
    // (topologically ordered) revision list first has nothing to hand it. The
    // limiting options this module models are the three sort orders, `--graph`,
    // `--children` and `--simplify-merges`; `--reverse` has its own message and
    // comes next. Both are refused ahead of the `--parents`/`--children` check
    // below, which is where `setup_revisions()` puts them.
    if walk_reflogs {
        if order != Order::Default || graph || show_children || simplify_merges_opt {
            eprintln!("fatal: cannot combine --walk-reflogs with history-limiting options");
            return Ok(ExitCode::from(128));
        }
        if reverse {
            eprintln!("fatal: options '--reverse' and '--walk-reflogs' cannot be used together");
            return Ok(ExitCode::from(128));
        }
    }
    let rewrite_parents = show_parents || graph || simplify_merges_opt || simplify_by_decoration;
    if rewrite_parents && show_children {
        eprintln!("fatal: options '--parents' and '--children' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    // "Limitations on the graph functionality", which `setup_revisions()` reaches
    // only after the check above.
    if graph && reverse {
        eprintln!("fatal: options '--graph' and '--reverse' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    // `revision.c:3197`: the graph lays its columns out by following each commit's
    // parents into the walk, and `--no-walk` yields the named commits alone — so
    // there is no history for it to draw.
    if graph && no_walk.is_some() {
        eprintln!("fatal: options '--no-walk' and '--graph' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // Walk in git's default commit-date order, then re-sort if a topological
    // order was asked for. `--graph` implies `--topo-order` unless `--date-order`
    // was given explicitly.
    // Commits reachable from the negative tips are hidden from the walk (the `..`
    // range exclusion). Empty when no `A..B`/`^A` was given.
    let hidden = if neg_ids.is_empty() {
        HashSet::new()
    } else if no_walk.is_some() {
        // Nothing paints UNINTERESTING under `--no-walk`; only what
        // `mark_parents_uninteresting()` already reached is excluded.
        no_walk_uninteresting(&repo, &neg_ids, &parsed)
    } else {
        ancestor_closure(&repo, &neg_ids)?
    };
    // The walk may stop early only when every commit it yields is guaranteed to
    // be shown: no pathspec, parent-count, date, grep or pickaxe filter can drop
    // one, no topological re-sort needs the whole set, and `--reverse` does not
    // need the tail. Anything else walks the full history as before.
    let unfiltered = pathspecs.is_empty()
        && !line_level
        && !only_merges
        && !no_merges
        && min_parents.is_none()
        && max_parents.is_none()
        && since.is_none()
        && until.is_none()
        && author_pats.is_empty()
        && committer_pats.is_empty()
        && grep_pats.is_empty()
        && pickaxe_s.is_none()
        && pickaxe_g.is_none()
        && !reverse
        && !graph
        && order == Order::Default;
    // A suppressed `whatchanged` record does not consume its `--max-count`, so the
    // walk cannot stop at `skip + max_count` commits there.
    let budget = (unfiltered && max_count.is_some() && flavor == Flavor::Log)
        .then(|| skip.saturating_add(max_count.unwrap_or(0)));
    // `-g`: `get_revision_1()` calls `next_reflog_entry()` in place of popping the
    // frontier, so the list is the reflog entries themselves rather than anything
    // reachable from a tip. Each entry keeps the commit's *real* parents, which is
    // what the pruning and the diffs below then use.
    let mut nodes = if walk_reflogs {
        reflog_walk(&repo, &tip_names)?
    } else {
        walk(&repo, &tips, &tip_sources, first_parent, &hidden, budget, no_walk)?
    };
    // `-L` sets `revs->topo_order = 1` without touching `sort_order`, so it walks
    // topologically unless `--date-order` asked for the date-ordered variant.
    let effective_order = match (order, graph || line_level) {
        (Order::Default, true) => Order::Topo,
        (o, _) => o,
    };
    // `prepare_revision_walk()` returns as soon as `revs->no_walk` survived
    // (revision.c:4009), which is *before* both `sort_in_topological_order()` and
    // `init_topo_walk()` — so `--topo-order`/`--date-order` are silently inert
    // there, and the pending order (or its date sort) stands.
    if effective_order != Order::Default && no_walk.is_none() {
        nodes = topo_sort_ordered(&repo, nodes, effective_order);
    }

    // `-L`: carry the tracked ranges backward through the history, keeping only the
    // commits that took blame for one. The file pairs a kept commit is responsible
    // for are held for the output pass below.
    let mut line_log_pairs: HashMap<ObjectId, Vec<(line_log::Pair, Vec<line_log::Range>)>> =
        HashMap::new();
    if line_level {
        // A positional token that turned out to name a path only becomes a pathspec
        // during the loop above, which is why this repeats the earlier check.
        if !pathspecs.is_empty() {
            eprintln!("fatal: -L<range>:<file> cannot be used with pathspec");
            return Ok(ExitCode::from(128));
        }
        // `check_single_commit`: the ranges are resolved against exactly one commit,
        // so several positive tips leave the starting blob undefined.
        if tips.len() > 1 {
            eprintln!(
                "fatal: More than one commit to dig from: {} and {}?",
                tip_names.get(1).map(String::as_str).unwrap_or_default(),
                tip_names.first().map(String::as_str).unwrap_or_default()
            );
            return Ok(ExitCode::from(128));
        }
        let Some(start) = tips.first().copied() else {
            eprintln!("fatal: No commit specified?");
            return Ok(ExitCode::from(128));
        };
        let tracked = match line_log::parse_lines(&repo, start, &line_ranges) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("fatal: {}", e.0);
                return Ok(ExitCode::from(128));
            }
        };
        let mut tracker = line_log::Tracker::new(&repo, start, tracked, first_parent);
        let mut kept = Vec::with_capacity(nodes.len());
        // Every walked commit's post-line-log parent list plus whether it survived,
        // which is what `line_log_rewrite_one` reads (a dropped commit is git's
        // TREESAME).
        let mut seen: HashMap<ObjectId, (Vec<ObjectId>, bool)> = HashMap::new();
        for mut node in nodes.into_iter() {
            let (range, parents) = tracker.process(node.id, &node.parents)?;
            node.parents = parents;
            seen.insert(node.id, (node.parents.clone(), range.is_some()));
            if let Some(range) = range {
                line_log_pairs.insert(node.id, line_log::queue_pairs(&range));
                kept.push(node);
            }
        }
        // `line_log_filter` finishes with `rewrite_parents()`, which git runs only
        // when the caller wants ancestry — `--graph` and `--parents` are what set
        // `rewrite_parents` here. Every other format never prints a parent.
        if graph || show_parents {
            for node in &mut kept {
                let mut rewritten: Vec<ObjectId> = Vec::with_capacity(node.parents.len());
                for p in &node.parents {
                    if let Some(id) = line_log_rewrite_one(*p, &seen, &hidden) {
                        if !rewritten.contains(&id) {
                            rewritten.push(id);
                        }
                    }
                }
                node.parents = rewritten;
            }
        }
        nodes = kept;
    }

    // Path-limited traversal, a port of `try_to_simplify_commit()` followed by
    // `rewrite_parents()` (revision.c).
    //
    // The test is TREESAME *per parent*, not against the first one: a commit that
    // matches any parent over the pathspec is "simplified away" — it is not
    // shown, and the history it stands for is the one behind that parent alone.
    // For a merge that is what removes both the merge itself and the entire side
    // whose changes it did not take; comparing only against the first parent
    // leaves the merge in the log and lists the other side's commits as well.
    // `log_setup_revisions_tweak()`: the config default becomes real following
    // only with exactly one pathspec. It deliberately cannot reach the die below —
    // that belongs to the explicit flag.
    if default_follow && pathspecs.len() == 1 {
        follow = true;
    }
    if follow {
        // `cmd_log_init_finish()`: `--follow` rewrites the pathspec as the walk
        // goes back, so it can only track one path.
        if pathspecs.len() != 1 {
            eprintln!("fatal: --follow requires exactly one pathspec");
            return Ok(ExitCode::from(128));
        }
        // `try_to_follow_renames()` (tree-diff.c): walk newest first along the
        // first parent, and when the followed path turns out to have arrived by a
        // rename, switch to the name it came from. A commit is shown exactly when
        // the followed path changed in it.
        let mut current: gix::bstr::BString = pathspecs[0].clone().into();
        // The followed path is the whole pathspec set here, so the matcher is
        // rebuilt only when a rename moves it — not once per commit.
        let mut matcher = PathspecMatcher::new(&repo, std::slice::from_ref(&pathspecs[0]))?;
        let mut shown: Vec<Node> = Vec::new();
        for node in std::mem::take(&mut nodes) {
            let commit = repo.find_object(node.id)?.try_into_commit()?;
            let parent = node.parents.first().copied();
            // `--follow` turns pruning off entirely — "Can't prune commits with
            // rename following: the paths change" (revision.c) — and sets
            // `revs->diff`, so what a commit is judged by is whether it *renders a
            // diff*. A merge renders none by default, which is why `--follow` drops
            // a merge that the same pathspec keeps without it. `--first-parent` (or
            // an explicit `-m`/`-c`/`--cc`) gives the merge a diff, and then it is
            // shown like any other commit.
            let merge_without_diff = node.parents.len() > 1 && diff_merges == DiffMerges::Off;
            let changed =
                !merge_without_diff && changes_match(&repo, &commit, parent, &mut matcher)?;
            if changed {
                let mut node = node;
                node.follow_path = Some(current.clone());
                shown.push(node);
            }
            // The switch happens after the commit is judged: the rename *is* the
            // change that makes the commit interesting.
            if let Some(parent) = parent {
                if let Some(src) = follow_source(&repo, &commit, parent, &current)? {
                    current = src;
                    matcher = PathspecMatcher::new(&repo, &[current.to_string()])?;
                }
            }
        }
        nodes = shown;
    } else if !pathspecs.is_empty() {
        let mut matcher = PathspecMatcher::new(&repo, &pathspecs)?;
        // id → (parents the simplified history follows, whether it is shown).
        let mut simplified: HashMap<ObjectId, (Vec<ObjectId>, bool)> =
            HashMap::with_capacity(nodes.len());
        // Only `--simplify-merges` needs the per-parent detail.
        let mut merge_simp: HashMap<ObjectId, super::simplify::Classified> = HashMap::new();
        // `commit->parents` as `try_to_simplify_commit()` rewrote it in place
        // (revision.c:1050-1054): the single parent a commit turned out to be
        // TREESAME to, with every other parent freed. That mutation is part of the
        // walk, not of the display, so `--parents`, `--graph` and `%p`/`%P` all see
        // it whatever `revs->dense` is — only `rewrite_parents()` further down is
        // gated on dense. Absent means the commit kept the parents it was walked
        // with.
        let mut pruned: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        for node in &nodes {
            let commit = repo.find_object(node.id)?.try_into_commit()?;
            // `--first-parent` limits the comparison the same way it limits the
            // walk: git never looks at the parents it is not following.
            let parents: &[ObjectId] = if first_parent {
                &node.parents[..node.parents.len().min(1)]
            } else {
                &node.parents
            };
            if parents.is_empty() {
                // A root commit is compared against the empty tree, so it shows
                // exactly when it introduced a matching path. That comparison runs
                // ahead of the `--sparse` early return (revision.c:979-997), so a
                // root is still marked TREESAME under `--sparse`; what `--sparse`
                // removes is the `revs->prune && revs->dense` gate that would have
                // dropped it (revision.c:4221).
                let changed = changes_match(&repo, &commit, None, &mut matcher)?;
                if simplify_merges_opt {
                    merge_simp.insert(
                        node.id,
                        super::simplify::Classified {
                            parents: Vec::new(),
                            treesame_with: Vec::new(),
                            treesame: !changed,
                        },
                    );
                }
                simplified.insert(node.id, (Vec::new(), changed || !dense));
                continue;
            }
            // `if (!revs->dense && !commit->parents->next) return;`
            // (revision.c:996): under `--sparse` a non-merge is never compared at
            // all, so it is never TREESAME, is always shown, and keeps its parent.
            if !dense && parents.len() == 1 {
                if simplify_merges_opt {
                    merge_simp.insert(
                        node.id,
                        super::simplify::Classified {
                            parents: node.parents.clone(),
                            treesame_with: vec![false],
                            treesame: false,
                        },
                    );
                }
                simplified.insert(node.id, (parents.to_vec(), true));
                continue;
            }
            if full_history {
                // `--full-history` clears `revs->simplify_history`, and the
                // `REV_TREE_SAME` arm then records the parent and *continues*
                // instead of pruning to it and returning. Every parent is walked,
                // and the verdict is the one at the end of the loop:
                //     if (relevant_parents ? !relevant_change : !irrelevant_change)
                //             commit->object.flags |= TREESAME;
                // With no uninteresting commits in play every parent is relevant,
                // so a commit is shown exactly when some parent differs over the
                // pathspec. That is what keeps a merge whose side branch carried
                // the change, and the side itself, in the history.
                let mut any_change = false;
                let mut treesame_with: Vec<bool> = Vec::with_capacity(parents.len());
                for p in parents {
                    let changed = changes_match(&repo, &commit, Some(*p), &mut matcher)?;
                    treesame_with.push(!changed);
                    any_change |= changed;
                }
                if simplify_merges_opt {
                    // The parent list `simplify_one()` rewrites is the *whole*
                    // one: `--first-parent` stops the comparison at parent 1 but
                    // leaves the later parents in place, and `%p`/`--parents`
                    // still print them.
                    merge_simp.insert(
                        node.id,
                        super::simplify::Classified {
                            parents: node.parents.clone(),
                            treesame_with,
                            treesame: !any_change,
                        },
                    );
                }
                simplified.insert(node.id, (parents.to_vec(), any_change || !dense));
                continue;
            }
            let mut treesame: Option<ObjectId> = None;
            for p in parents {
                if !changes_match(&repo, &commit, Some(*p), &mut matcher)? {
                    treesame = Some(*p);
                    break;
                }
            }
            match treesame {
                // The parent pruning `try_to_simplify_commit()` performs in place
                // (revision.c:1050-1054) happens whatever `revs->dense` is; only
                // the `revs->prune && revs->dense` display gate below it does not,
                // so under `--sparse` the merge itself is still printed while the
                // side it did not take stays unwalked.
                Some(p) => {
                    pruned.insert(node.id, vec![p]);
                    simplified.insert(node.id, (vec![p], !dense))
                }
                None => simplified.insert(node.id, (parents.to_vec(), true)),
            };
        }
        // Whatever the simplified parent lists no longer reach was never walked
        // by git in the first place, so it cannot appear in the output.
        let mut reachable: HashSet<ObjectId> = HashSet::with_capacity(nodes.len());
        let mut stack: Vec<ObjectId> = tips.clone();
        while let Some(id) = stack.pop() {
            if !reachable.insert(id) {
                continue;
            }
            if let Some((parents, _)) = simplified.get(&id) {
                stack.extend(parents.iter().copied());
            }
        }
        if simplify_merges_opt {
            // `simplify_merges()` prunes `revs->commits` to the commits that
            // simplify to themselves; `get_commit_action()` then applies its own
            // TREESAME filter to what is left. Both apply.
            let order: Vec<ObjectId> =
                nodes.iter().map(|n| n.id).filter(|id| reachable.contains(id)).collect();
            let walked: HashSet<ObjectId> = order.iter().copied().collect();
            let sm = super::simplify::merge_simplify(&repo, &order, &merge_simp, first_parent)?;
            // `--simplify-merges` sets `revs->rewrite_parents`, so `want_ancestry`
            // holds and a TREESAME merge between two relevant commits stays in the
            // output to tie the topology together.
            nodes.retain(|n| {
                sm.kept(&n.id)
                    && super::simplify::shows(
                        sm.treesame.get(&n.id).copied().unwrap_or(false),
                        sm.parents.get(&n.id).map(Vec::as_slice).unwrap_or(&[]),
                        &walked,
                        true,
                        true,
                        true,
                    )
            });
            // The rewritten ancestry is what `%p`/`%P` report as well, not only
            // `--parents`/`--graph`: git rewrites `commit->parents` in place, so
            // every consumer sees the simplified list. `simplify_commit()` then
            // runs the ordinary `rewrite_parents()` on top of it, which is what
            // drops a TREESAME root parent from the list.
            let ancestry = super::simplify::Ancestry {
                walked: &walked,
                treesame: &sm.treesame,
                parents: &sm.parents,
                first_parent,
            };
            for node in &mut nodes {
                if let Some(parents) = sm.parents.get(&node.id) {
                    node.parents = ancestry.rewrite(parents);
                }
            }
        } else {
            nodes.retain(|n| {
                reachable.contains(&n.id) && simplified.get(&n.id).is_some_and(|(_, shown)| *shown)
            });
        }

        // The in-place prune belongs to the walk, so it lands on every commit that
        // survived it — `--sparse --parents` shows `385d3ba c6e564c`, not the two
        // parents the merge object records. Under `--dense` no *shown* commit can
        // carry one (a TREESAME commit is dropped by `get_commit_action()` unless
        // `--simplify-merges` handled it separately, and both cases are covered
        // above), so it is applied only where it can still matter.
        if !dense && !simplify_merges_opt {
            for node in &mut nodes {
                if let Some(parents) = pruned.get(&node.id) {
                    node.parents = parents.clone();
                }
            }
        }

        // `rewrite_parents()`: the ancestry the output shows is the simplified
        // one, and a parent reachable from another parent drops out of it. Only
        // the ancestry-printing formats take it: the per-commit diff stays
        // against the real first parent, which is what `log --name-status --
        // <path>` reports. `--simplify-merges` has already written its own.
        // `simplify_commit()` reaches `rewrite_parents()` only under
        // `revs->prune && revs->dense && want_ancestry(revs)` (revision.c:4317-4318),
        // so `--sparse --parents` prints the ancestry the commits really have.
        if (graph || show_parents) && !simplify_merges_opt && dense {
            for node in &mut nodes {
                let mut rewritten: Vec<ObjectId> = Vec::with_capacity(node.parents.len());
                for p in &node.parents {
                    if let Some(id) = simplify_rewrite_one(*p, &simplified) {
                        if !rewritten.contains(&id) {
                            rewritten.push(id);
                        }
                    }
                }
                // Dropping a parent that is reachable from another is
                // `mark_redundant_parents()`, which belongs to
                // `--simplify-merges`. Plain `rewrite_parents()` keeps both, so
                // `--full-history --parents` shows the merge with the ancestry it
                // actually had.
                if !full_history {
                    prune_redundant_parents(&repo, &mut rewritten);
                }
                node.parents = rewritten;
            }
        }
    }

    // `--merges`/`--no-merges` are git's aliases for `--min-parents=2` /
    // `--max-parents=1`; parent-count limiting happens before commit limiting.
    if only_merges {
        nodes.retain(|n| n.parents.len() >= 2);
    }
    if no_merges {
        nodes.retain(|n| n.parents.len() < 2);
    }
    if let Some(min) = min_parents {
        nodes.retain(|n| n.parents.len() >= min);
    }
    if let Some(max) = max_parents {
        nodes.retain(|n| n.parents.len() <= max);
    }

    // `--use-mailmap` / `log.mailmap`: loaded once (worktree `.mailmap`, then
    // `mailmap.blob`, then `mailmap.file`) and shared by every rendered record.
    // `%aN`/`%aE`/`%cN`/`%cE` resolve through the mailmap whether or not the header
    // formats do, so a format that names one loads it even under `--no-use-mailmap`.
    let format_maps_identities = match &pretty {
        Pretty::User(f) => format_names_mapped_identity(f),
        _ => false,
    };
    let mailmap = (use_mailmap || format_maps_identities)
        .then(|| std::sync::Arc::new(Mailmap::load(&repo)));

    // `--grep`/`--author`/`--committer` header/message filtering, applied during
    // selection — before `--skip`/`--max-count`, exactly as git does.
    let commit_filter = crate::revfilter::CommitFilter {
        // `commit_match()` greps the mailmapped headers when a mailmap is in effect,
        // which is what makes `--author=<canonical>` find an aliased commit. The
        // format-only load above does not enable it: git ties this to `revs->mailmap`.
        ident_map: use_mailmap.then(|| mailmap.clone()).flatten().map(|m| {
            let m = m.clone();
            std::sync::Arc::new(move |name: &[u8], email: &[u8]| m.mapped(name, email))
                as crate::revfilter::IdentMapper
        }),
        author_res: crate::revfilter::compile_patterns(&author_pats, grep_dialect, grep_ignore_case)?,
        committer_res: crate::revfilter::compile_patterns(
            &committer_pats,
            grep_dialect,
            grep_ignore_case,
        )?,
        grep_res: crate::revfilter::compile_patterns(&grep_pats, grep_dialect, grep_ignore_case)?,
        all_match: grep_all_match,
        invert_grep: grep_invert,
    };
    // Pickaxe `-G<regex>` compiles once, in the same dialect as --grep.
    let pickaxe_g_re = match &pickaxe_g {
        Some(p) => Some(crate::revfilter::build_regex(p, grep_dialect, grep_ignore_case)?),
        None => None,
    };
    // `diff_setup_done()` (diff.c:5262-5273) rejects two pickaxe combinations outright,
    // both `die()`s and so both exit 128. They are checked here, after the revisions have
    // been resolved, because `setup_revisions()` reaches `diff_setup_done()` only once it
    // has finished walking argv — a bad revision is reported first.
    if usize::from(pickaxe_s.is_some())
        + usize::from(pickaxe_g.is_some())
        + usize::from(!find_object.is_empty())
        > 1
    {
        eprintln!("fatal: options '-G', '-S', and '--find-object' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    // `diff_setup_done()` (diff.c): `DIFF_PICKAXE_REGEX` is `-S`'s modifier, and git
    // names the pairing explicitly rather than ignoring it.
    if pickaxe_regex && pickaxe_g.is_some() {
        eprintln!(
            "fatal: options '-G' and '--pickaxe-regex' cannot be used together, \
             use '--pickaxe-regex' with '-S'"
        );
        return Ok(ExitCode::from(128));
    }
    if pickaxe_all && !find_object.is_empty() {
        eprintln!(
            "fatal: options '--pickaxe-all' and '--find-object' cannot be used together, \
             use '--pickaxe-all' with '-G' and '-S'"
        );
        return Ok(ExitCode::from(128));
    }
    // `diffcore_pickaxe()`'s needle, for the two kinds that are decided from the blobs
    // alone: `-S` (`has_changes`, a literal unless `--pickaxe-regex` promotes it) and
    // `--find-object` (`o->objfind`, a plain id-set test). `-G` needs the patch text and
    // keeps its own path below.
    let pickaxe = match (&find_object, &pickaxe_s) {
        (ids, _) if !ids.is_empty() => {
            let mut oids = Vec::with_capacity(ids.len());
            for spec in ids {
                // `--find-object` resolves through the usual revision machinery, so an
                // abbreviated id or any other object-ish spelling works.
                match repo.rev_parse_single(spec.as_bytes()) {
                    Ok(id) => oids.push(id.detach()),
                    // `diff_opt_find_object()` (diff.c:5532) returns `error()` from the
                    // option callback, which parse-options turns into 129 rather than the
                    // 128 a `die()` would give.
                    Err(_) => {
                        eprintln!("error: unable to resolve '{spec}'");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            Some(super::diff_pairs::Pickaxe {
                kind: super::diff_pairs::PickaxeKind::ObjFind(oids),
                all: pickaxe_all,
            })
        }
        (_, Some(needle)) => {
            let kind = match pickaxe_regex {
                true => match super::diff_pairs::compile_regex(needle.as_bytes()) {
                    Ok(re) => super::diff_pairs::PickaxeKind::Occurrences(
                        super::diff_pairs::Needle::Regex(re),
                    ),
                    Err(msg) => {
                        eprintln!("fatal: invalid regex: {msg}");
                        return Ok(ExitCode::from(128));
                    }
                },
                false => super::diff_pairs::PickaxeKind::Occurrences(
                    super::diff_pairs::Needle::Literal(needle.as_bytes().to_vec()),
                ),
            };
            Some(super::diff_pairs::Pickaxe { kind, all: pickaxe_all })
        }
        _ => None,
    };
    let has_pickaxe = pickaxe.is_some() || pickaxe_g_re.is_some();

    // `--graph`: the commits `get_commit_action()` would show, which is what
    // `graph_is_interesting()` asks about each parent. Taken before `-S`/`-G`,
    // which git applies to the *diff* of a commit it has already walked and
    // graphed, and before `--skip`/`--max-count`, which stop the walk rather than
    // judge a commit.
    let mut interesting: HashSet<ObjectId> = HashSet::new();
    // `--graph` with `-S`/`-G`: the commits the pickaxe kept. `None` when no
    // pickaxe ran, which means every commit prints.
    let mut pickaxe_shown: Option<HashSet<ObjectId>> = None;
    if !commit_filter.is_empty() || since.is_some() || until.is_some() || has_pickaxe {
        let mut kept = Vec::with_capacity(nodes.len());
        for node in nodes.into_iter() {
            let commit = repo.find_commit(node.id)?;
            // `--since`/`--until` gate on committer time (git's default), then
            // the header/message predicates. `comparison_date()` (revision.c):
            // under `--walk-reflogs` the clock the range is measured against is
            // the *reflog entry's*, not the commit's — the same commit can sit
            // under entries from either side of the cutoff.
            let seconds = match &node.reflog {
                Some(rl) => rl.time.seconds,
                None => commit.time()?.seconds,
            };
            if since.is_some_and(|s| seconds < s) || until.is_some_and(|u| seconds > u) {
                continue;
            }
            if !commit_filter.matches(&commit)? {
                continue;
            }
            kept.push(node);
        }
        // Pickaxe: test each surviving commit's changes against `-S`/`-G`. Both
        // scans run across the thread pool — the commits are independent, and git
        // walks the same candidates one at a time on one core.
        //
        // Under `--graph` the ones it drops stay in the list: git walks and graphs
        // them, then prints nothing, so their columns still move (see
        // [`render_graph`]). `pickaxe_shown` records which ones printed.
        if has_pickaxe {
            // A merge produces no diff without `-m`/`-c`/`--cc`, and the pickaxe
            // tests a diff — so git never reports a merge for `-S`/`-G` no matter
            // what its parents contain. Dropping them here also keeps the scan
            // from reading blobs for the largest commits in the history.
            let candidates: Vec<Node> = match graph {
                true => kept.iter().filter(|n| n.parents.len() < 2).cloned().collect(),
                false => {
                    kept.retain(|n| n.parents.len() < 2);
                    std::mem::take(&mut kept)
                }
            };
            let hits = match (&pickaxe, &pickaxe_g_re) {
                // `-S` and `--find-object` never need patch text. git's `has_changes`
                // counts the needle in each side's whole blob and keeps the file when
                // the two counts differ, and `objfind` only compares ids, so the scan
                // reads blobs (or nothing at all) and never diffs them.
                (Some(px), None) => pickaxe_by_count(&repo, candidates, &px.kind)?,
                _ => {
                    let jobs: Vec<(ObjectId, Option<ObjectId>)> =
                        candidates.iter().map(|n| (n.id, n.parents.first().copied())).collect();
                    let patches = super::diff::commit_patches(&repo, &jobs, &super::diff::PatchOpts { ctx: 0, ..patch_opts.clone() }, &pathspecs, false)?;
                    candidates
                        .into_iter()
                        .zip(patches)
                        .filter(|(_, patch)| {
                            pickaxe_hit(patch, pickaxe_s.as_deref(), pickaxe_g_re.as_ref())
                        })
                        .map(|(node, _)| node)
                        .collect()
                }
            };
            match graph {
                true => pickaxe_shown = Some(hits.iter().map(|n| n.id).collect()),
                false => kept = hits,
            }
        }
        if graph {
            interesting = kept.iter().map(|n| n.id).collect();
        }
        nodes = kept;
    } else if graph {
        interesting = nodes.iter().map(|n| n.id).collect();
    }

    // `--skip` drops the first N of the selected commits, then `--max-count` caps
    // what remains — git's order in `get_revision`.
    if skip > 0 {
        let drop = skip.min(nodes.len());
        nodes.drain(0..drop);
    }
    // `cmd_log_walk()` restores a `--max-count` slot spent on a commit that printed
    // nothing (`if (!log_tree_commit(...)) rev->max_count++`), which only `whatchanged`
    // can hit — `git log` always shows the header. The cap therefore moves to the
    // render loop there, counting records actually printed.
    let print_limit = match flavor {
        Flavor::WhatChanged => max_count,
        Flavor::Log => {
            if let Some(limit) = max_count {
                nodes.truncate(limit);
            }
            None
        }
    };
    if reverse {
        nodes.reverse();
    }

    // `revs->diffopt.output_format` as the command line itself left it — every bit
    // but the `DIFF_FORMAT_PATCH` that `diff_merges_setup_revs()` may add below, and
    // every bit but `whatchanged`'s raw fallback, which is later still.
    // `DIFF_FORMAT_NO_OUTPUT` (`-s`, `-q`) counts: it is a format, and it is what
    // makes `git log --diff-merges=separate -s` print two headerless merge records
    // instead of two patches.
    let rendering_format = patch
        || check
        || stat
        || numstat
        || shortstat
        || summary
        || name_only
        || name_status
        || raw
        // `DIFF_FORMAT_DIRSTAT` is an output-format bit like any other, so it
        // satisfies `cmd_whatchanged()`'s `if (!rev.diffopt.output_format)` raw
        // fallback (builtin/log.c): measured against stock 2.55.0,
        // `git whatchanged --i-still-use-this --dirstat` prints the dirstat block
        // alone, with no `:100644 …` records in front of it.
        || dirstat_on;
    let asked_format = rendering_format || saw_no_patch || quiet;
    // `revs->diff` — "Did the user ask for any diff output?" (revision.c:3145-3146),
    // answered *before* `diff_merges_setup_revs()` runs, so the patch format that
    // call installs never reaches it. It is `log_tree_diff()`'s `all_need_diff`
    // (log-tree.c:1103), the flag that decides whether a *non-merge* is diffed at
    // all — which is why `git log --diff-merges=separate` prints a diff for a merge
    // and nothing but the header for every other commit. `cmd_whatchanged` sets it
    // unconditionally (`rev.diff = 1`, builtin/log.c:543), and `merges_imply_patch`
    // sets it too (diff-merges.c:186-187).
    //
    // `--diff-filter` sets it too: revision.c:3149-3152 raises `revs->diff` for the
    // pickaxe, `--diff-filter` and `--follow` alike, under the comment "Pickaxe,
    // diff-filter and rename following need diffs" — which is why `git log -s
    // --diff-filter=M` still builds the queue, and still prints the header of every
    // commit the filter left something in.
    let all_need_diff = flavor == Flavor::WhatChanged
        || merges_imply_patch
        || rendering_format
        // `int all_need_diff = opt->diff || opt->diffopt.flags.exit_with_status;`
        // (log-tree.c:1103): `--exit-code` builds the queue by itself, which is why
        // `git log --exit-code -s` still reports 1.
        || exit_code
        || patch_opts.diff_filter.is_some();
    // `--name-only`/`--name-status` are git's reported format; they suppress both
    // the count formats and the `-p` patch. The patch is emitted after the count
    // formats otherwise.
    // `diff_merges_setup_revs()` (diff-merges.c:186-191) promotes either merge flag
    // to the patch format, but only when nothing else claimed one — so `-c --stat`
    // stays a stat and `--diff-merges=separate -s` stays silent. `-m` is the one
    // spelling that raises neither flag, which is why `git log -m` on its own still
    // prints no diff.
    let patch = patch || ((merges_imply_patch || merges_need_diff) && !asked_format);
    // `diff_setup_done()`: `DIFF_FORMAT_CHECKDIFF` clears every other format bit, so
    // `--check --stat -p` prints only the whitespace report.
    let emit_patch = patch && !name_only && !name_status && !check;
    // `diff_setup_done()`: `--name-only`/`--name-status` clear every other output
    // format, `--raw` among them; `--raw` itself clears nothing, so it stacks with
    // the count formats and the patch.
    // `cmd_whatchanged()`: `if (!rev.diffopt.output_format) output_format = DIFF_FORMAT_RAW`.
    // `-s`/`--no-patch` and `-q`/`--quiet` *are* an output format
    // (`DIFF_FORMAT_NO_OUTPUT`), so they satisfy that `if` and leave
    // `whatchanged` with no raw listing at all.
    let raw = raw
        || (flavor == Flavor::WhatChanged
            && !(patch
                || stat
                || numstat
                || shortstat
                || summary
                || name_only
                || name_status
                || saw_no_patch
                || quiet
                // `DIFF_FORMAT_DIRSTAT` counts as an output format too, so
                // `whatchanged --dirstat` prints the dirstat block alone (measured
                // against stock 2.55.0) rather than raw records plus a dirstat.
                || dirstat_on));
    let raw = raw && !name_only && !name_status && !check;
    let summary = summary && !name_only && !name_status && !check;
    let (name_only, name_status) = (name_only && !check, name_status && !check);
    let (stat, numstat, shortstat) = (stat && !check, numstat && !check, shortstat && !check);
    let dirstat_on = dirstat_on && !check;
    let want_names =
        name_only || name_status || raw || summary || stat || numstat || shortstat || dirstat_on;
    // `whatchanged` under `DIFF_FORMAT_NO_OUTPUT`: nothing is rendered, but the
    // pair queue still decides whether the commit is shown at all, so it is built
    // and thrown away.
    // `--diff-filter` clears `always_show_header` the same way (builtin/log.c:333),
    // so the queue decides whether the commit prints even under `-s`, where nothing
    // would otherwise build it.
    let probe_queue =
        (flavor == Flavor::WhatChanged || patch_opts.diff_filter.is_some()) && !want_names && !patch;
    // Whether `%C`/`%d` emit ANSI: git's auto rule is "stdout is a terminal, or we
    // are paging to one" — `pager::maybe_setup` records the latter via the env flag.
    let want_color = match color {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        // git routes `git log`'s coloring through the diff machinery, so the
        // config switch is `color.diff` falling back to `color.ui`; `auto` then
        // asks whether stdout is a terminal or a `color.pager` pager.
        ColorWhen::Auto => super::color::want_color_stdout(&repo, "diff"),
    };
    // `--color-moved` / `--word-diff` layered over their config defaults, and the
    // palette the re-emit pass paints with. `log_tree_commit()` hands the diff
    // machinery the same `o->use_color` the header was written under, so a patch body
    // is colored exactly when the header is.
    patch_opts.extra = match move_word.resolve(&repo) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(128));
        }
    };
    patch_opts.colors = diff_color::DiffColors::resolve(&repo, want_color);
    // `diff.relative` seeds the very flag `--relative` sets
    // (`options->flags.relative_name = diff_relative`, diff.c:4639), so the config
    // alone both narrows and shortens; `--no-relative` clears it again, which is why
    // an explicit flag always wins.
    patch_opts.relative = match (&relative, repo.config_snapshot().boolean("diff.relative")) {
        (Some(p), _) => Some(p.clone()),
        (None, Some(true)) if !no_relative_given => Some(super::diff::cwd_prefix(&repo)),
        _ => None,
    };
    // `diff.wsErrorHighlight`, which `--ws-error-highlight` overrides.
    patch_opts.ws_error_highlight = match ws_error_highlight {
        Some(v) => v,
        None => diff_color::ws_error_highlight_default(&repo).unwrap_or(diff_color::WSEH_NEW),
    };
    // `%d`/`%D` need a commit→refs map; build it only when the format asks for one
    // so plain formats pay nothing for the ref scan.
    let decorations = if pretty_uses_decoration(&pretty) || decorate != DecorateStyle::Off {
        let filter = DecorationFilter::build(
            &repo,
            &decorate_refs,
            &decorate_refs_exclude,
            default_decoration_filter,
        );
        Some(build_decorations(&repo, &filter)?)
    } else {
        None
    };
    // git's `color.decorate.<slot>` table, plus the `color.diff.commit` color it
    // paints the decoration punctuation and the commit object name with. Resolved
    // once; the disabled table when this run is not coloring at all.
    let deco_colors = if want_color {
        super::color::DecorateColors::resolve(&repo)
    } else {
        super::color::DecorateColors::disabled()
    };
    // `--simplify-by-decoration`: the same simplification the pathspec path runs,
    // with a different question asked of each commit. `simplify_commit()` keeps a
    // commit that carries a decoration, and — since simplification may not drop
    // the shape of the history — a root or a merge; everything else is walked
    // past. The parent lists are rewritten so `--graph`/`--parents` draw the
    // simplified history rather than the real one.
    if simplify_by_decoration {
        let decos = match &decorations {
            Some(d) => d,
            None => {
                let filter = DecorationFilter::build(
                    &repo,
                    &decorate_refs,
                    &decorate_refs_exclude,
                    default_decoration_filter,
                );
                decorations_for_simplify = Some(build_decorations(&repo, &filter)?);
                decorations_for_simplify.as_ref().expect("just built")
            }
        };
        let mut simplified: HashMap<ObjectId, (Vec<ObjectId>, bool)> =
            HashMap::with_capacity(nodes.len());
        for node in &nodes {
            let shown =
                decos.decorates(&node.id) || node.parents.is_empty() || node.parents.len() > 1;
            let parents = if shown {
                node.parents.clone()
            } else {
                node.parents[..node.parents.len().min(1)].to_vec()
            };
            simplified.insert(node.id, (parents, shown));
        }
        let mut reachable: HashSet<ObjectId> = HashSet::with_capacity(nodes.len());
        let mut stack: Vec<ObjectId> = tips.clone();
        while let Some(id) = stack.pop() {
            if !reachable.insert(id) {
                continue;
            }
            if let Some((parents, _)) = simplified.get(&id) {
                stack.extend(parents.iter().copied());
            }
        }
        nodes.retain(|n| {
            reachable.contains(&n.id) && simplified.get(&n.id).is_some_and(|(_, shown)| *shown)
        });
        // `rewrite_parents()` runs whenever a simplification did: the ancestry the
        // output shows — the `Merge:` header, `--parents`, the graph — is the
        // simplified one, not the commit's real parent list.
        for node in &mut nodes {
            let mut rewritten: Vec<ObjectId> = Vec::with_capacity(node.parents.len());
            for p in &node.parents {
                if let Some(id) = simplify_rewrite_one(*p, &simplified) {
                    if !rewritten.contains(&id) {
                        rewritten.push(id);
                    }
                }
            }
            prune_redundant_parents(&repo, &mut rewritten);
            node.parents = rewritten;
        }
    }

    // `--boundary`: the excluded commits the shown history hangs off — every
    // parent that the exclusion hid — appended after the walk with a `-` mark.
    // git emits them from `revs->boundary_commits` once the main walk is done, so
    // they come last regardless of their dates and skip the filters above.
    if boundary && !hidden.is_empty() {
        let shown: HashSet<ObjectId> = nodes.iter().map(|n| n.id).collect();
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut edge: Vec<Node> = Vec::new();
        let reader = NodeReader::new(&repo);
        for node in &nodes {
            for parent in &node.parents {
                if shown.contains(parent) || !hidden.contains(parent) || !seen.insert(*parent) {
                    continue;
                }
                let mut n = reader.read(&repo, *parent)?;
                n.boundary = true;
                edge.push(n);
            }
        }
        edge.sort_by_key(|n| std::cmp::Reverse(n.time));
        nodes.extend(edge);
    }

    // `--boundary`: every parent of a commit the walk *returned* carries
    // CHILD_SHOWN, which `graph_is_interesting()` accepts on its own. The boundary
    // commits appended above never marked theirs — git hands them out from
    // `create_boundary_commit_list()`, below the loop that does the marking — so
    // they are the ones left out here.
    let interest = GraphInterest {
        child_shown: match graph && boundary {
            true => nodes.iter().filter(|n| !n.boundary).flat_map(|n| n.parents.iter().copied()).collect(),
            false => HashSet::new(),
        },
        shown: interesting,
    };

    // `show_log()` reads `graph_width(opt->graph)` for each commit right after
    // `graph_update()` has laid its row out, and a `%<|(<N>)` column target in the
    // format has to leave room for that prefix. The records are rendered before
    // the graph is drawn here, so the widths are measured up front — the same
    // state machine, run for its column bookkeeping alone.
    if graph {
        measure_graph_widths(&mut nodes, first_parent, &interest);
    }

    // `--children`: git records a child on every parent as it walks, so the list
    // names only commits this run reached.
    let children: Option<HashMap<ObjectId, Vec<ObjectId>>> = show_children.then(|| {
        let mut map: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        for node in &nodes {
            for parent in &node.parents {
                // `push_children()` splices each child onto the *front* of the
                // list, so the ids come out in reverse walk order.
                map.entry(*parent).or_default().insert(0, node.id);
            }
        }
        map
    });

    // `--abbrev=<n>` is `revs->abbrev`, which every abbreviation in the run reads.
    // Pushing it into the repository's config as `core.abbrev` puts it in front of
    // the same lookup gitoxide already makes, so `%h`, the oneline id and the diff
    // `index` lines all shorten together rather than each growing a knob.
    if let Some(n) = abbrev_len {
        let mut config = repo.config_snapshot_mut();
        config.append_config(Some(format!("core.abbrev={n}")), gix::config::Source::Cli)?;
        config.commit()?;
    }

    // Relative dates (`%cr`/`%ar`, `--date=relative`) are measured against now.
    let now = now_secs();

    // `cmd_log_init_finish()`: with no `--notes`/`--no-notes` of its own, a run
    // shows notes when the caller picked no format at all — or picked a user
    // format, where they surface only through `%N`. `--pretty=oneline` and the
    // other built-ins therefore stay silent unless asked.
    if !notes_opt.given && (!pretty_given || matches!(pretty, Pretty::User(_))) {
        notes_opt.enable_default();
    }
    let notes_trees = super::notes::load_display(&repo, &notes_opt)?;

    // git emits one terminated record per commit for any non-empty format, even
    // when a given commit expands to nothing (e.g. `%d` on an undecorated commit).
    // Only the genuinely empty user format (`--pretty=`, `tformat:`) emits nothing.
    let empty_user_format = matches!(&pretty, Pretty::User(f) if f.is_empty());

    // `--graph` needs every commit's block up front to lay out the columns, so it
    // buffers; every other format streams commit-by-commit (see the write below).
    let abbrev_cache = std::cell::RefCell::new(AbbrevCache::new(&repo));
    // `opt->diffopt.needed_rename_limit` / `degraded_cc_to_c`: `cmd_log_walk()`
    // reports them once, through `diff_result_code()`, when the whole walk is done
    // (builtin/log.c:443, diff.c:7546-7548). Each commit's rename pass overwrites
    // them, so what is reported is the last diffed commit's.
    let mut rename_warn = super::diffcore_rename::Warnings::default();
    let mut blocks: Vec<Option<GraphBlock>> = Vec::new();
    // BLOCK-buffered, not line-buffered: Rust's stdout is a LineWriter, so writing
    // one terminated record per commit meant one write(2) per commit — 6375
    // syscalls for a full `log` on a deep history, which showed up as ~400ms of
    // system time against git's 8ms. git buffers and so does this now; the tail
    // is flushed below, and a closed pipe still surfaces as BrokenPipe.
    let mut stdout = std::io::BufWriter::with_capacity(64 * 1024, std::io::stdout().lock());
    let mut first = true;
    // Formats that need only what the walk produced skip the object read
    // entirely — the dominant cost of `--pretty=format:%H` on a deep history.
    let walk_only = match &pretty {
        Pretty::User(f) => !want_names && !emit_patch && format_is_walk_only(f),
        _ => false,
    };
    // `-p` renders each commit's patch from an immutable tree pair, so the patch
    // for a commit ten rows down the output does not depend on anything the rows
    // above it do. The window computes a batch of them across the thread pool
    // while the loop below stays a plain in-order stream — git computes them one
    // at a time on one core.
    let mut patches =
        PatchWindow::new(emit_patch, show_root, diff_merges, all_need_diff, patch_opts.clone());
    // Each record's text comes out of its own commit object, and reading 6000 of
    // them is the whole cost of a format like `--oneline` or `%s`. The window
    // renders a batch of records at a time across the thread pool; the loop below
    // still writes them one after another, in walk order.
    // ```c
    // const char *get_log_output_encoding(void)
    // {
    //         return git_log_output_encoding ? git_log_output_encoding
    //                 : get_commit_output_encoding();
    // }
    // const char *get_commit_output_encoding(void)
    // {
    //         return git_commit_encoding ? git_commit_encoding : "UTF-8";
    // }
    // ```
    //
    // (`environment.c:189-198`.) `--encoding=` wins, then `i18n.logOutputEncoding`,
    // then `i18n.commitEncoding`, and UTF-8 when none of them is set. The two config
    // keys write the *same* two slots the option does, so `--encoding=none` (the
    // empty string) still beats a configured `i18n.logOutputEncoding`.
    let output_encoding = match log_encoding {
        Some(v) => v,
        None => {
            let cfg = repo.config_snapshot();
            cfg.string("i18n.logOutputEncoding")
                .or_else(|| cfg.string("i18n.commitEncoding"))
                .map(|v| v.to_string())
                .unwrap_or_else(|| "UTF-8".to_string())
        }
    };
    // `reencode_string_len()` delegates to `iconv(3)`; this port's stand-in is
    // `encoding_rs`, which has no UTF-16/UTF-32 *encoder* — see
    // `crate::porcelain::mailinfo::encode_to`. git's own UTF-16 output is
    // platform-dependent there too (`ICONV_OMITS_BOM` decides whether it writes the
    // byte-order mark itself), so rather than emit bytes that are neither, the
    // request is refused outright.
    if super::mailinfo::is_utf16_or_32_name(&output_encoding) {
        bail!(
            "unsupported flag \"--encoding={output_encoding}\" (the UTF-16 and UTF-32 \
             families are not ported; every other charset is)"
        );
    }

    // The anchor list is final once the option scan is; it reaches the blob differ
    // through the process-wide slot. See [`super::diff_pairs::set_anchor_texts`].
    super::diff_pairs::set_anchor_texts(anchors);

    let mut entries = EntryWindow::new(EntryParams {
        abbrev_commit,
        show_signature,
        show_parents,
        graph,
        children: children.as_ref(),
        date_mode,
        want_color,
        colors: &deco_colors,
        now,
        decorations: decorations.as_ref(),
        decorate,
        source_mode,
        mailmap: use_mailmap.then(|| mailmap.as_deref()).flatten(),
        identity_mailmap: mailmap.as_deref(),
        terminator,
        rec_term: if z { 0u8 } else { b'\n' },
        empty_user_format,
        pretty: &pretty,
        notes: &notes_trees,
        expand_tabs,
        date_explicit,
        // `show_log()`'s `ctx.rev = opt; ctx.print_email_subject = 1;`
        // (log-tree.c:700-701), which is what puts `[<prefix>] ` on the
        // `Subject:` line and turns the RFC2047 encoding on.
        email: EmailStyle {
            subject_prefix: &cfg_subject_prefix,
            encode_headers: encode_email_headers,
        },
        output_encoding: &output_encoding,
    });
    // `do_remerge_diff()` (log-tree.c:1029-1090) re-runs the merge into a temporary
    // object directory and diffs its tree against the recorded one. This port has
    // no merge engine to re-run, so `--remerge-diff` is refused exactly when the
    // walk carries a merge that would reach it — a walk without one is rendered
    // normally, because `set_remerge_diff()` changes nothing else that this command
    // reads. Checked before any record is written, since the records stream.
    if remerge && nodes.iter().any(|n| n.parents.len() > 1) {
        eprintln!("fatal: --diff-merges=remerge is not supported by this build");
        return Ok(ExitCode::from(128));
    }
    // The pathspec set the name/stat formats are limited to, parsed once rather
    // than per commit. `--follow` replaces it per commit (see below).
    let mut path_limit = if pathspecs.is_empty() {
        None
    } else {
        Some(PathspecMatcher::new(&repo, &pathspecs)?)
    };
    // `-z` replaces the record terminator (and the separator between records) with NUL,
    // which is what `line_termination` feeds in git.
    let rec_term = if z { 0u8 } else { b'\n' };

    // `whatchanged` counts what it actually printed against `--max-count`.
    let mut printed = 0usize;
    // `o->flags.has_changes` / `o->flags.check_failed`, the two bits
    // `diff_result_code()` (diff.c) turns into the exit status: `01` and `02`.
    let mut has_changes = false;
    let mut check_failed = false;

    // `log_tree_commit()` under `-m`: a merge is rendered once per parent, each record
    // carrying its own ` (from <oid>)` header insert and diffing against that parent.
    // Every other commit is one record against its first parent.
    // The repetition is the `for (;;)` loop in `log_tree_diff()`, so it happens
    // whenever that function gets past its early returns — `--diff-merges=separate -s`
    // repeats the header twice with no diff under either copy.
    let separate_merges =
        diff_merges == DiffMerges::Separate && (all_need_diff || merges_need_diff);
    if separate_merges && graph && nodes.iter().any(|n| n.parents.len() > 1) {
        bail!("`-m` with `--graph` is not ported: git lays out one graph row per
               per-parent record");
    }
    let records: Vec<(usize, Option<ObjectId>, Option<ObjectId>)> = nodes
        .iter()
        .enumerate()
        .flat_map(|(ni, n)| {
            if separate_merges && n.parents.len() > 1 {
                n.parents.iter().map(|p| (ni, Some(*p), Some(*p))).collect::<Vec<_>>()
            } else {
                vec![(ni, n.parents.first().copied(), None)]
            }
        })
        .collect();

    // `log_tree_commit()`'s `shown`, carried across the per-parent records of one
    // merge: `log_tree_diff()` returns whatever the `for (;;)` loop's flushes
    // reported, and only when *none* of them showed anything does the
    // `always_show_header` fallback print a single parentless header.
    let mut merge_shown_any = false;
    for (ri, (ni, diff_parent, from)) in records.iter().copied().enumerate() {
        let node = &nodes[ni];
        if print_limit.is_some_and(|n| printed >= n) {
            break;
        }
        // `--graph` with `-S`/`-G`: git walked this commit and ran `graph_update()`
        // on it, then `log_tree_commit()` found nothing to print. The row is
        // dropped but the columns still move — the gap is what the `...` skip row
        // in [`render_graph`] marks. Only `--graph` gets here: every other path
        // dropped the commit from the list outright.
        if pickaxe_shown.as_ref().is_some_and(|hits| !hits.contains(&node.id)) {
            blocks.push(None);
            continue;
        }
        // Whether this record produced a diff queue; `whatchanged` prints nothing for
        // a commit that did not.
        let mut record_has_diff = false;
        // `log_tree_diff_flush()` reached its `diff_queue_is_empty()` test and it
        // came back true, which is its `return 0` — no header, no diff. Distinct
        // from `!record_has_diff`, which is also what an `-s` record looks like:
        // there the queue was never asked about, so the flush still ran.
        let mut record_queue_empty = false;
        if walk_only {
            let Pretty::User(fmt) = &pretty else { unreachable!() };
            let mut block: Vec<u8> = Vec::new();
            expand_walk_only(&mut block, fmt, node, abbrev_commit, &abbrev_cache, &repo);
            if terminator && !empty_user_format {
                block.push(rec_term);
            }
            if graph {
                blocks.push(Some(GraphBlock::message_only(block)));
                continue;
            }
            let mut piece: Vec<u8> = Vec::new();
            if !terminator && !first {
                piece.push(rec_term);
            }
            piece.extend_from_slice(&block);
            first = false;
            let piece = super::diff::apply_line_prefix(piece, &line_prefix);
            if let Err(e) = stdout.write_all(&piece) {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    crate::sigpipe::exit_broken_pipe();
                }
                return Err(e.into());
            }
            continue;
        }
        let mut block = match from {
            None => entries.get(&repo, &nodes, ni, &abbrev_cache)?,
            // A per-parent `-m` record is rendered on its own: the batched window has
            // one slot per commit and no room for the ` (from <oid>)` insert.
            Some(parent) => {
                entry_block_from(&repo, node, &entries.params, &abbrev_cache, Some(parent))?
            }
        };
        // Where `show_log()` stops and `log_tree_diff_flush()` takes over. Under
        // `--graph` that hand-over is where the graph's remaining rows are drained
        // (`graph_show_commit_msg`), so the split has to survive into
        // [`render_graph`]; everything appended below is diff output.
        let mut msg_len = block.len();

        // `log_tree_diff` short-circuits under `-L`: it flushes the pairs
        // `line_log_queue_pairs()` produced and returns, so neither the merge rule
        // nor `log.showRoot` applies — a surviving merge simply has no pairs, and a
        // root commit's creation pair is shown like any other.
        if line_level {
            let pairs = line_log_pairs.get(&node.id).map(Vec::as_slice).unwrap_or(&[]);
            let mut diff: Vec<u8> = Vec::new();
            if name_status {
                for (pair, _) in pairs {
                    let (status, path) = line_log_name_status(pair);
                    diff.push(status);
                    diff.push(b'\t');
                    diff.extend_from_slice(path);
                    diff.push(b'\n');
                }
            } else if name_only {
                for (pair, _) in pairs {
                    diff.extend_from_slice(&pair.path);
                    diff.push(b'\n');
                }
            } else if emit_patch && !pairs.is_empty() {
                diff = super::diff::line_range_patch(&repo, pairs, 3)?;
            }
            if !diff.is_empty() {
                // A merge's combined diff is separated from the header even under
                // `oneline`, which is the one format that otherwise runs the patch
                // straight on: `show_combined_diff()` writes the blank line itself.
                let combined_here = node.parents.len() > 1
                    && matches!(diff_merges, DiffMerges::Combined | DiffMerges::DenseCombined);
                // ```c
                // if ((opt->diffopt.output_format & ~DIFF_FORMAT_NO_OUTPUT) &&
                //     opt->verbose_header &&
                //     opt->commit_format != CMIT_FMT_ONELINE &&
                //     !commit_format_is_empty(opt->commit_format)) {
                // ```
                //
                // (log-tree.c:941-944.) `commit_format_is_empty()` tests the format
                // *string* — `CMIT_FMT_USERFORMAT` with nothing in it — not the
                // record it produced. A format whose expansion happened to come out
                // empty (`%N` on a commit with no note, `%d` on an undecorated one)
                // is still separated from its diff.
                if (!matches!(pretty, Pretty::Oneline) || combined_here) && !empty_user_format {
                    block.push(b'\n');
                }
                block.extend_from_slice(&diff);
            }
        }
        // `log_tree_diff()`'s early returns, in order (log-tree.c:1103-1152):
        //
        // ```c
        // int all_need_diff = opt->diff || opt->diffopt.flags.exit_with_status;
        // if (!all_need_diff && !opt->merges_need_diff)
        //         return 0;
        // ...
        // is_merge = parents && parents->next;
        // if (!is_merge && !all_need_diff)
        //         return 0;
        // if (!parents) { if (opt->show_root_diff) { … } return !opt->loginfo; }
        // if (is_merge) { … if (opt->separate_merges) { … } else return 0; }
        // ```
        //
        // So a merge needs a `--diff-merges` mode (`separate_merges` or
        // `combine_merges`) *and* one of the two diff flags; every other commit,
        // root included, needs only `all_need_diff`, and a root additionally obeys
        // `log.showRoot`. That is why `git whatchanged --first-parent` emits no
        // record at all for a merge: `cmd_whatchanged` installs no tweak, so the
        // mode stays `off`.
        // `DIFF_FORMAT_NO_OUTPUT` renders nothing, but `log_tree_diff_flush()` still
        // builds and tests the pair queue — and its answer is what decides whether
        // `whatchanged` prints the commit at all. So the queue is still walked
        // under `-s`/`-q`; only the rendering is skipped.
        else if (want_names || emit_patch || probe_queue || check || exit_code)
            && if node.parents.len() > 1 {
                (all_need_diff || merges_need_diff) && diff_merges != DiffMerges::Off
            } else {
                all_need_diff && (show_root || !node.parents.is_empty())
            }
        {
            let mut diff: Vec<u8> = Vec::new();
            // `diff_flush()`'s `separator` counter: raised by the raw/name loop, by
            // the count-format block and by a non-empty `--summary`, and read once by
            // the patch format to decide whether a blank line precedes it. It is not
            // "the buffer is non-empty" — `show_dirstat()` writes without raising it
            // (diff.c:7238 sits outside the block that does), so `--dirstat -p` runs
            // the patch straight on.
            let mut separator = false;
            // `log_tree_diff_flush()` separates the message from the diff whenever the
            // pair queue is non-empty, even for a format that has nothing to say about
            // those pairs (`--summary` over a plain content change).
            let mut queue_nonempty = false;
            // The paths `diffcore_pickaxe()` left in the queue, which the patch is
            // rendered from. Empty and unused when no pickaxe ran.
            let mut pickaxe_paths: Vec<String> = Vec::new();
            if want_names || probe_queue || check || exit_code || has_pickaxe {
                // `--name-only`/`--name-status` are the reported format when
                // present; git suppresses the count formats in that case, so the
                // blob reads they need are skipped too.
                let count_formats = (stat || numstat || shortstat) && !name_only && !name_status;
                // The record was rendered by a worker, which kept nothing; the
                // count formats need the commit itself for its tree.
                let commit = repo.find_object(node.id)?.try_into_commit()?;
                // `-- <pathspec>` limits what the name/stat formats report, not just
                // which commits reach them, and it limits the *tree diff* — so it
                // goes in ahead of rename detection, where git puts it.
                // `--follow` limits each commit by the name the file had there.
                let mut followed = match &node.follow_path {
                    Some(path) => Some(PathspecMatcher::new(&repo, &[path.to_string()])?),
                    None => None,
                };
                // `--follow` is the exception: its record is the rename *pair*
                // (`R100 a.txt renamed.txt`), so both sides have to survive into
                // rename detection and the limit is applied to the result instead.
                let pre = if followed.is_some() { None } else { path_limit.as_mut() };
                let mut files = collect_changes(
                    &repo,
                    &commit,
                    diff_parent,
                    count_formats || patch_opts.ws != super::diff::Whitespace::Keep,
                    patch_opts.ws,
                    Some(&patch_opts),
                    pre,
                    Some(&mut rename_warn),
                )?;
                if let Some(m) = followed.as_mut() {
                    files.retain(|f| m.matches(&f.path));
                }
                // `--relative[=<path>]`'s *narrowing* half (`diff_queue()`'s prefix
                // test, diff.c:7630), which every format sees. The *shortening* half
                // is `strip_prefix()` (diff.c:5009) and is applied per format below,
                // because `diff_summary()` and `show_dirstat()` do not call it.
                if let Some(prefix) = &patch_opts.relative {
                    files.retain(|f| f.path.starts_with(prefix.as_bytes()));
                }
                // `diffcore_apply_filter()`: the name and stat formats report the
                // same filtered queue the patch renders.
                if let Some(filter) = &patch_opts.diff_filter {
                    files.retain(|f| super::diff::diff_filter_selected(filter, f.status));
                }
                // `diffcore_pickaxe()` munges the queue itself, so every format below
                // sees only the pairs that matched — `git log -Sfoo --raw` names the
                // file that changed its occurrence count, not the whole commit. This
                // runs inside `diffcore_std()`, i.e. before the whitespace re-render
                // and before the queue is tested for emptiness.
                if let Some(px) = &pickaxe {
                    pickaxe_filter_files(&repo, px, &mut files)?;
                }
                // `-G` is `diff_grep()`, which tests each pair's *own* change text
                // rather than its blobs — but it sits in the same `diffcore_std()`
                // slot, so the queue it leaves behind is the one every format below
                // renders.
                if let Some(re) = &pickaxe_g_re {
                    grep_filter_files(&repo, re, pickaxe_all, &mut files)?;
                }
                if has_pickaxe {
                    // Both sides of a rename, since limiting the tree diff to the
                    // destination alone would hide the deletion the pair needs.
                    pickaxe_paths = files
                        .iter()
                        .flat_map(|f| {
                            std::iter::once(f.path.clone()).chain(f.source.iter().cloned())
                        })
                        .map(|p| String::from_utf8_lossy(&p).into_owned())
                        .collect();
                }
                // `diff_flush()` (diff.c:7210): under a whitespace rule the queue is
                // re-rendered quietly first and every pair whose patch came out empty
                // is dropped, so the raw, name and stat formats never mention a file
                // whose only change was whitespace — nor does the separator appear.
                // The queue is tested for emptiness *before* the quiet flush, so a
                // commit whose only change is whitespace still separates its message
                // from the (empty) diff.
                queue_nonempty = !files.is_empty();
                if patch_opts.ws != super::diff::Whitespace::Keep {
                    files.retain(reports_change);
                }
                // `o->flags.has_changes`, the `01` bit of `diff_result_code()`.
                // `diff_tree_combined()` never sets it, so `--exit-code` on a merge
                // under `-c`/`--cc` reports 0; `--check` reports through
                // `check_failed` instead, which is why `--check --exit-code` is 2
                // rather than 3.
                if !check
                    && !(node.parents.len() > 1
                        && matches!(
                            diff_merges,
                            DiffMerges::Combined | DiffMerges::DenseCombined
                        ))
                {
                    let changed = match patch_opts.ws != super::diff::Whitespace::Keep {
                        true => !files.is_empty(),
                        false => queue_nonempty,
                    };
                    has_changes |= changed;
                }
                // A merge under a combined mode runs `diff_tree_combined()`
                // (combine-diff.c:1600-1610) instead of `diff_flush()`, and the two
                // do not agree on block order or on what earns the separator:
                //
                // ```c
                // if (opt->output_format & (DIFF_FORMAT_RAW | DIFF_FORMAT_NAME |
                //                           DIFF_FORMAT_NAME_STATUS)) {
                //         for (p = paths; p; p = p->next)
                //                 show_raw_diff(p, num_parent, rev);
                //         needsep = 1;
                // }
                // else if (opt->output_format & STAT_FORMAT_MASK)
                //         needsep = 1;
                // ```
                //
                // The `STAT_FORMAT_MASK` formats (numstat, diffstat, shortstat,
                // dirstat, summary — combine-diff.c:1371-1375) were already written
                // by `find_paths_generic()`'s `i == 0` pass, against the *first
                // parent*, so they precede the raw block rather than follow it. And
                // `needsep` answers to the format bits alone, which is why
                // `--summary -p` on a merge with an empty summary still separates.
                let combined_merge = node.parents.len() > 1
                    && matches!(diff_merges, DiffMerges::Combined | DiffMerges::DenseCombined);
                if check {
                    // `diff_flush_checkdiff()` in place of every other format.
                    // `diff_tree_combined()` never looks at `DIFF_FORMAT_CHECKDIFF`
                    // (combine-diff.c:1600-1610), so a merge under `-c`/`--cc`
                    // reports nothing at all — only the separator its own header
                    // block already wrote.
                    if !combined_merge {
                        check_failed |= super::diff::commit_check(
                            &repo,
                            &mut diff,
                            node.id,
                            diff_parent,
                            &patch_opts,
                            &pathspecs,
                        )?;
                    }
                } else if combined_merge {
                    let rel = patch_opts.relative.as_deref().unwrap_or("");
                    let (fsep, fend) = if z { (0u8, 0u8) } else { (b'\t', b'\n') };
                    if !name_only && !name_status {
                        if numstat {
                            emit_numstat(&mut diff, &files, z, rel);
                        }
                        if stat {
                            emit_stat(&mut diff, &files, &stat_widths, compact_summary, rel, &patch_opts.colors)?;
                        }
                        if shortstat {
                            emit_shortstat(&mut diff, &files)?;
                        }
                        if dirstat_on {
                            super::diff::commit_dirstat(
                                &repo,
                                node.id,
                                diff_parent,
                                &patch_opts,
                                path_limit.as_mut(),
                                &dirstat,
                                &mut diff,
                            )?;
                        }
                        if summary {
                            emit_summary(&mut diff, &files);
                        }
                        if numstat || stat || shortstat || dirstat_on || summary {
                            separator = true;
                        }
                    }
                    if raw {
                        diff.extend_from_slice(&super::diff::merge_combined_raw(
                            &repo,
                            node.id,
                            &node.parents,
                            &pathspecs,
                            crate::abbrev::configured_abbrev(&repo, repo.object_hash().len_in_hex())
                                .max(MINIMUM_ABBREV),
                            z,
                            true,
                        )?);
                        separator = true;
                    } else if name_only || name_status {
                        for (path, letters) in super::diff::merge_combined_names(
                            &repo,
                            node.id,
                            &node.parents,
                            &pathspecs,
                        )? {
                            if name_status {
                                diff.extend_from_slice(letters.as_bytes());
                                diff.push(fsep);
                            }
                            diff.extend_from_slice(&name_field(&path, z));
                            diff.push(fend);
                        }
                        separator = true;
                    }
                } else {
                    // `diff_flush()`'s fixed order: the raw/name loop first, then the
                    // count formats. `--raw` does not displace them, so `--raw --stat`
                    // prints both.
                    // `strip_prefix()`'s reach (diff.c:5009): the raw, name and stat
                    // writers, and the patch. Not `--summary`, not `--dirstat` —
                    // neither calls it, so both keep the repository-root name.
                    let rel = patch_opts.relative.as_deref().unwrap_or("");
                    if raw {
                        emit_raw(&repo, &mut diff, &files, z, rel)?;
                        separator = true;
                    }
                    let (fsep, fend) = if z { (0u8, 0u8) } else { (b'\t', b'\n') };
                    if name_status {
                        for f in &files {
                            diff.push(f.status);
                            // `diff_flush_name_status()`: a rename carries its similarity
                            // index and names both paths. Every name goes out through
                            // `write_name_quoted()`.
                            if let Some(source) = &f.source {
                                diff.extend_from_slice(format!("{:03}", f.score).as_bytes());
                                diff.push(fsep);
                                diff.extend_from_slice(&name_field(shorten_path(source, rel), z));
                            }
                            diff.push(fsep);
                            diff.extend_from_slice(&name_field(shorten_path(&f.path, rel), z));
                            diff.push(fend);
                        }
                        separator = true;
                    } else if name_only {
                        for f in &files {
                            diff.extend_from_slice(&name_field(shorten_path(&f.path, rel), z));
                            diff.push(fend);
                        }
                        separator = true;
                    } else {
                        // git stacks the count formats in a fixed order: numstat, then
                        // the full stat block, then a bare shortstat summary if stat did
                        // not already print one.
                        if numstat || stat || shortstat {
                            separator = true;
                        }
                        if numstat {
                            emit_numstat(&mut diff, &files, z, rel);
                        }
                        // `diff_flush()` tests the two bits separately, so
                        // `--stat --shortstat` prints the stat block and then a
                        // second summary line.
                        if stat {
                            emit_stat(&mut diff, &files, &stat_widths, compact_summary, rel, &patch_opts.colors)?;
                        }
                        if shortstat {
                            emit_shortstat(&mut diff, &files)?;
                        }
                        // `diff_flush()`: dirstat sits between the stat formats
                        // and the summary. Note that `show_dirstat()` is the one
                        // format writer that does *not* `separator++` (diff.c:7238 is
                        // outside the block that does), so a bare `--dirstat -p` runs
                        // the patch straight on with no blank line between them —
                        // only `--dirstat=lines`, which is emitted from inside the
                        // diffstat block at diff.c:7233, gets the separator.
                        if dirstat_on {
                            if dirstat.by_line {
                                separator = true;
                            }
                            super::diff::commit_dirstat(
                                &repo,
                                node.id,
                                diff_parent,
                                &patch_opts,
                                path_limit.as_mut(),
                                &dirstat,
                                &mut diff,
                            )?;
                        }
                        if summary {
                            let before = diff.len();
                            emit_summary(&mut diff, &files);
                            // `!is_summary_empty(q)` guards the `separator++`, so a
                            // summary with nothing to say does not earn a blank line.
                            separator |= diff.len() != before;
                        }
                    }
                }
            }
            if emit_patch {
                // The full patch, rendered by the same pipeline as `git diff` so
                // the two agree byte-for-byte. git separates a preceding count
                // format from the patch with a blank line.
                // Under `--follow` the limit is the name the file had *at this
                // commit*, not the one on the command line — and it differs from
                // commit to commit, so the batching window (one pathspec per fill)
                // cannot serve it.
                let follow_patch: Vec<u8> = match &node.follow_path {
                    Some(path) => super::diff::commit_patches(
                        &repo,
                        &[(node.id, node.parents.first().copied())],
                        &patch_opts,
                        &[path.to_string()],
                        true,
                    )?
                    .pop()
                    .unwrap_or_default(),
                    None => Vec::new(),
                };
                // A per-parent `-m` record diffs against *that* parent, which the
                // batched window (one first-parent patch per commit) cannot serve.
                let separate_patch: Vec<u8> = match from {
                    Some(parent) => super::diff::commit_patches(
                        &repo,
                        &[(node.id, Some(parent))],
                        &patch_opts,
                        &pathspecs,
                        false,
                    )?
                    .pop()
                    .unwrap_or_default(),
                    None => Vec::new(),
                };
                // `diffcore_pickaxe()` filters the queue, and `diff_flush()` renders
                // the *filtered* queue — the patch included. The batched window has
                // one pathspec set for the whole span and cannot serve a limit that
                // differs per commit, so a pickaxe run renders this commit on its
                // own, against the paths that survived (both sides of a rename, since
                // limiting to the destination alone would hide the deletion).
                let pickaxe_patch: Vec<u8> = match has_pickaxe && from.is_none() {
                    false => Vec::new(),
                    true if pickaxe_paths.is_empty() => Vec::new(),
                    true => super::diff::commit_patches(
                        &repo,
                        &[(node.id, diff_parent)],
                        &patch_opts,
                        &pickaxe_paths,
                        false,
                    )?
                    .pop()
                    .unwrap_or_default(),
                };
                let p: &[u8] = match (&node.follow_path, from) {
                    (Some(_), _) => &follow_patch,
                    (None, Some(_)) => &separate_patch,
                    (None, None) if has_pickaxe => &pickaxe_patch,
                    (None, None) => patches.get(&repo, &nodes, ni, 3, &pathspecs)?,
                };
                if !p.is_empty() {
                    // `if (separator) emit_diff_symbol(DIFF_SYMBOL_SEPARATOR)`
                    // (diff.c): the blank line is owed by the *format writers* that
                    // raised `separator`, not by whatever happens to be in the
                    // buffer — which is why a non-`lines` `--dirstat` block does not
                    // earn one.
                    if separator {
                        // `DIFF_SYMBOL_SEPARATOR` writes `o->line_termination`
                        // (diff.c:1436-1440), so under `-z` the blank line between an
                        // earlier block and the patch is a NUL instead.
                        diff.push(rec_term);
                    }
                    diff.extend_from_slice(p);
                }
            }
            // A merge's combined diff is separated from the header even under
            // `oneline`, which is the one format that otherwise runs the patch
            // straight on — and it is separated even when the combined diff came
            // out *empty*, because that separator is not `log_tree_diff_flush()`'s
            // at all. `diff_tree_combined()` prints the header and the blank line
            // itself, before it has scanned a single path:
            //
            // ```c
            // show_log_first = !!rev->loginfo && !rev->no_commit_id;
            // needsep = 0;
            // if (show_log_first) {
            //         show_log(rev);
            //
            //         if (rev->verbose_header && opt->output_format &&
            //             opt->output_format != DIFF_FORMAT_NO_OUTPUT &&
            //             !commit_format_is_empty(rev->commit_format))
            //                 printf("%s%c", diff_line_prefix(opt),
            //                        opt->line_termination);
            // }
            // ```
            //
            // (combine-diff.c:1512-1522.) So `git log -c` on a merge that is
            // TREESAME to every parent still prints its header followed by a blank
            // line, and `git whatchanged -c` — whose `always_show_header` is off —
            // prints that merge too, even though `do_diff_combined()` reports it as
            // not shown.
            let combined_here = node.parents.len() > 1
                && matches!(diff_merges, DiffMerges::Combined | DiffMerges::DenseCombined);
            // `if (opt->diffopt.output_format & ~DIFF_FORMAT_NO_OUTPUT)`
            // (log-tree.c): with nothing but `NO_OUTPUT` set there is no diff to
            // separate the message from, so `-s` prints no blank line either —
            // and combine-diff's own separator carries the same
            // `output_format != DIFF_FORMAT_NO_OUTPUT` test, which `probe_queue`
            // is exactly the state of.
            // ```c
            // if ((opt->diffopt.output_format & ~DIFF_FORMAT_NO_OUTPUT) && …)
            // ```
            //
            // (log-tree.c:941.) With no output format at all — `--exit-code` on its
            // own builds the queue without asking for one — there is nothing to
            // separate the message from, so no blank line is written either.
            if (!diff.is_empty() || queue_nonempty || combined_here)
                && !probe_queue
                && (want_names || emit_patch || check)
            {
                // git puts a separator between the log message and the diff for
                // every format but `oneline` — and only when the message block
                // rendered something to separate from. A `--stat` block shown
                // together with `-p` is fenced off with a `---` line; every other
                // diff format uses a plain blank line.
                // `commit_format_is_empty()` again (log-tree.c:944): the test is on
                // the format string, so a `%N` that expanded to nothing still gets
                // its separator.
                if (!matches!(pretty, Pretty::Oneline) || combined_here) && !empty_user_format {
                    if combined_here {
                        // `diff_tree_combined()` writes this one itself, as
                        // `printf("%s%c", diff_line_prefix(opt), opt->line_termination)`
                        // (combine-diff.c:1514-1515) — so it is a NUL under `-z`, and
                        // it is never the `---` fence, which lives in
                        // `log_tree_diff_flush()` and is not on this path at all.
                        block.push(rec_term);
                    // A mail format that already fenced its notes block with `---`
                    // raised `opt->shown_dashes`, which suppresses this second one.
                    } else if stat
                        && emit_patch
                        && !mail_notes_shown_dashes(&repo, &notes_trees, &pretty, node.id)?
                    {
                        block.extend_from_slice(b"---\n");
                    } else {
                        block.push(b'\n');
                    }
                }
                block.extend_from_slice(&diff);
            }
            record_has_diff = !diff.is_empty() || queue_nonempty || combined_here;
            record_queue_empty = !record_has_diff;
        }
        // ```c
        // if (diff_queue_is_empty(&opt->diffopt)) {
        //         …
        //         return 0;
        // }
        //
        // if (opt->loginfo && !opt->no_commit_id) {
        //         show_log(opt);
        // ```
        //
        // (log-tree.c:864-873.) The ` (from <oid>)` header belongs to the flush, so
        // a per-parent record whose queue came out empty prints nothing at all —
        // which is why `git log --diff-merges=separate -- <path>` shows only the
        // parents that path really differs against. If every parent came out empty,
        // `log_tree_diff()` reported `shown == 0` and `log_tree_commit()`'s
        // `always_show_header` prints one header with `log.parent = NULL`, i.e.
        // without the insert.
        if from.is_some() {
            let last_of_merge = records.get(ri + 1).is_none_or(|next| next.0 != ni);
            let shown_before = merge_shown_any;
            // The flag belongs to one merge, so it is cleared the moment that
            // merge's last per-parent record is reached.
            merge_shown_any = !last_of_merge && (shown_before || !record_queue_empty);
            if record_queue_empty {
                if !last_of_merge || shown_before {
                    continue;
                }
                block = entry_block_from(&repo, node, &entries.params, &abbrev_cache, None)?;
                msg_len = block.len();
            }
        }
        // `cmd_whatchanged()` leaves `always_show_header` off, so `log_tree_commit()`
        // prints nothing at all for a commit whose diff queue came out empty — and
        // `cmd_log_walk()` hands the `--max-count` slot back.
        //
        // `cmd_log_init_finish()` (builtin/log.c:333) clears the same flag for
        // `git log` itself the moment a pickaxe, `--diff-filter` or `--follow` is in
        // play, because each of those *is* a queue filter: a commit that survives the
        // walk but loses every pair must print nothing rather than a bare header.
        if flavor == Flavor::WhatChanged || patch_opts.diff_filter.is_some() {
            if !record_has_diff {
                // Under `--graph` the commit was still walked and graphed, so it
                // keeps its slot and prints no row (see [`render_graph`]).
                if graph {
                    blocks.push(None);
                }
                continue;
            }
            printed += 1;
        }
        if graph {
            // Buffer for the column layout, which spans all commits at once.
            blocks.push(Some(GraphBlock { text: block, msg_len }));
            continue;
        }

        // Stream this commit's block immediately, so `git log -p | head` stops
        // after a commit or two instead of computing every patch first. A
        // `format:`/built-in (separator) format precedes every record but the
        // first with a blank line; a `tformat:` record was already terminated
        // above, so no separator is inserted.
        let mut piece: Vec<u8> = Vec::new();
        if !terminator && !first {
            piece.push(rec_term);
        }
        piece.extend_from_slice(&block);
        first = false;
        // `--line-prefix`: `emit_line_0()` writes `diff_line_prefix(o)` in front of
        // every emitted line, and for a history verb that includes the header
        // `show_log()` wrote. Applied per record because the records stream; a
        // record ends in its terminator, so the next record's leading prefix lands
        // exactly where an interior newline would have put it.
        let piece = super::diff::apply_line_prefix(piece, &line_prefix);
        // Each block ends in a newline, so the line-buffered stdout flushes it here;
        // a closed downstream pipe (`| head`) surfaces as a BrokenPipe on this write,
        // which is a normal stop rather than an error. No per-commit flush is needed.
        if let Err(e) = stdout.write_all(&piece) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                crate::sigpipe::exit_broken_pipe();
            }
            return Err(e.into());
        }
    }

    // Persist whatever abbreviations this run had to compute, off the critical
    // path — the next `log` in any clone holding these objects reads them back.
    abbrev_cache.into_inner().flush();

    // `diff_result_code()` (diff.c): `01` when `--exit-code` saw changes, `02` when
    // `--check` found a whitespace error.
    let result_code = u8::from(exit_code && has_changes) | (u8::from(check_failed) << 1);

    if graph {
        // `format:` separates records with a newline; `tformat:` already
        // terminated each block above. The separator is not simply appended to the
        // previous block: `show_log()` prints it once the *next* commit has been
        // through `graph_update()`, so [`render_graph`] emits it there.
        let separator = (!terminator).then_some(rec_term);
        // A terminator format's byte is already at the end of each block; the graph
        // path re-emits it after the commit's remaining rows, where git puts it.
        let record_terminator = terminator.then_some(rec_term);
        // A `whatchanged --max-count` run stops mid-list: `get_revision()` returns
        // nothing once the cap is spent, so the commits past it were never walked
        // and never reached `graph_update()`.
        nodes.truncate(blocks.len());
        let out = render_graph(
            &nodes,
            &blocks,
            graph_colors(&repo),
            want_color,
            separator,
            record_terminator,
            first_parent,
            &interest,
        )?;
        let out = super::diff::apply_line_prefix(out, &line_prefix);
        let rc = match stdout.write_all(&out) {
            Ok(()) => Ok(ExitCode::from(result_code)),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                crate::sigpipe::exit_broken_pipe()
            }
            Err(e) => Err(e.into()),
        };
        rename_warn.emit("diff.renameLimit");
        rc
    } else {
        // Flush the tail: a block that did not end in a newline (an empty user
        // format) may still be buffered.
        let rc = match stdout.flush() {
            Ok(()) => Ok(ExitCode::from(result_code)),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                crate::sigpipe::exit_broken_pipe()
            }
            Err(e) => Err(e.into()),
        };
        // `cmd_log_walk()` closes with `diff_result_code()`, whose
        // `diff_warn_rename_limit()` flushes stdout before it warns — so this lands
        // after the last record (builtin/log.c:443, diff.c:7038-7040).
        rename_warn.emit("diff.renameLimit");
        rc
    }
}

/// `line_log_rewrite_one`: replace a parent that `-L` dropped by the first commit
/// below it that is worth drawing an edge to. The walk stops at a merge, at an
/// excluded (`^rev`) commit, and at a commit `-L` kept; running out of parents
/// removes the edge entirely (git's `rewrite_one_noparents`).
fn line_log_rewrite_one(
    parent: ObjectId,
    seen: &HashMap<ObjectId, (Vec<ObjectId>, bool)>,
    hidden: &HashSet<ObjectId>,
) -> Option<ObjectId> {
    let mut p = parent;
    loop {
        let Some((parents, kept)) = seen.get(&p) else {
            return Some(p);
        };
        if parents.len() > 1 || hidden.contains(&p) || *kept {
            return Some(p);
        }
        p = *parents.first()?;
    }
}

/// `remove_duplicate_parents()`: after rewriting, a parent reachable from another
/// parent adds nothing to the simplified ancestry, and git drops it — which is
/// what turns a merge whose two sides collapse onto one line back into an
/// ordinary commit (no `Merge:` header, no fork in the graph).
fn prune_redundant_parents(repo: &gix::Repository, parents: &mut Vec<ObjectId>) {
    if parents.len() < 2 {
        return;
    }
    let original = parents.clone();
    parents.retain(|p| {
        !original.iter().any(|other| {
            other != p
                && repo
                    .merge_base(*p, *other)
                    .map(|base| base.detach() == *p)
                    .unwrap_or(false)
        })
    });
}

/// `rewrite_one()` for pathspec simplification: walk past every simplified-away
/// ancestor until a shown commit (or one the walk never reached) is found, so
/// `--graph`/`--parents` draw the simplified history rather than the real one.
fn simplify_rewrite_one(
    parent: ObjectId,
    simplified: &HashMap<ObjectId, (Vec<ObjectId>, bool)>,
) -> Option<ObjectId> {
    let mut p = parent;
    loop {
        let Some((parents, shown)) = simplified.get(&p) else {
            return Some(p);
        };
        if *shown {
            return Some(p);
        }
        p = *parents.first()?;
    }
}

/// The `--name-status` letter and path of a `-L` file pair.
///
/// `diff_resolve_rename_copy()` re-derives the letter from the two filespecs of the
/// `diff_filepair_dup()` the `-L` queue holds, and that copy carries no rename flag —
/// so even a pair whose sides name different files reports a plain `M`.
/// `diff_flush_raw()` then prints the pre-image path for anything but `R`/`C`.
fn line_log_name_status(pair: &line_log::Pair) -> (u8, &gix::bstr::BString) {
    match (pair.old, pair.new) {
        (None, _) => (b'A', &pair.path),
        (_, None) => (b'D', &pair.old_path),
        _ => (b'M', &pair.old_path),
    }
}

/// Parse a `-n`/`--max-count` value the way git does: a base-10 signed integer
/// with no trailing garbage. A negative value means "unlimited" (git's `-1`
/// sentinel), reported as `Ok(None)`; a non-negative value caps the walk.
/// `Err(())` marks a value git rejects with `fatal: '<value>': not an integer`.
/// The `--output-indicator-*` option a token names, whether it is glued to its
/// value or not. `OPT_CALLBACK_F(..., diff_opt_char)` declares all three with a
/// required argument (diff.c:6146-6160), so the bare spelling takes the next entry.
pub(crate) fn indicator_name(a: &str) -> Option<&'static str> {
    match a.split_once('=').map_or(a, |(n, _)| n) {
        "--output-indicator-new" => Some("--output-indicator-new"),
        "--output-indicator-old" => Some("--output-indicator-old"),
        "--output-indicator-context" => Some("--output-indicator-context"),
        _ => None,
    }
}

pub(crate) fn parse_max_count(value: &str) -> Result<Option<usize>, ()> {
    match parse_int(value) {
        Some(n) if n < 0 => Ok(None),
        Some(n) => Ok(Some(n as usize)),
        None => Err(()),
    }
}

/// git's `parse_age()` (revision.c:2286-2296), the value parser behind
/// `--max-age=`/`--min-age=`:
///
/// ```c
/// static timestamp_t parse_age(const char *arg)
/// {
///         timestamp_t num;
///         char *p;
///
///         errno = 0;
///         num = parse_timestamp(arg, &p, 10);
///         if (errno || *p || p == arg)
///                 die("'%s': not a number of seconds since epoch", arg);
///         return num;
/// }
/// ```
///
/// `parse_timestamp` is `strtoumax`, so the token is read **unsigned**: leading
/// whitespace is skipped, an optional sign is accepted, and `-` negates by
/// wrapping. Anything left over, an empty digit run, or an overflow (`ERANGE`) is
/// the fatal.
///
/// `Ok(None)` is the one value that parses and still does nothing:
/// `repo_init_revisions()` leaves `revs->max_age`/`revs->min_age` at `-1` and
/// every reader tests against that sentinel, so `--max-age=-1` — which wraps to
/// exactly `UINTMAX_MAX` — is indistinguishable from the option never having been
/// given.
pub(super) fn parse_age(arg: &str) -> Result<Option<i64>, ()> {
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = match b.get(i) {
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
    let digits_at = i;
    let mut num: u64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        // `strtoumax` sets `ERANGE` rather than wrapping, and `parse_age` dies on it.
        num = num
            .checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(b[i] - b'0')))
            .ok_or(())?;
        i += 1;
    }
    // `p == arg` (nothing converted) and `*p` (trailing garbage), in that order.
    if i == digits_at || i != b.len() {
        return Err(());
    }
    let num = if negative { num.wrapping_neg() } else { num };
    if num == u64::MAX {
        return Ok(None);
    }
    // A bound past `i64::MAX` can only exclude every commit there is, which is what
    // the saturating conversion leaves it doing.
    Ok(Some(i64::try_from(num).unwrap_or(i64::MAX)))
}

/// A non-negative base-10 integer (`--min-parents`, `--max-parents`).
/// `None` for anything git would reject with `fatal: '<value>': not an integer`.
fn parse_nonneg(value: &str) -> Option<usize> {
    match parse_int(value) {
        Some(n) if n >= 0 => Some(n as usize),
        _ => None,
    }
}

/// Parse a `--skip=<n>` value the way `revision.c` does: `strtol_i` into a signed
/// `skip_count`, which the walk then only ever tests with `> 0`. So a negative
/// value is accepted and skips nothing (verified against git 2.55.0:
/// `git log --skip=-1` lists the whole history), while a non-numeric value is
/// `fatal: '<value>': not an integer`.
pub(crate) fn parse_skip(value: &str) -> Result<usize, ()> {
    match parse_int(value) {
        Some(n) if n < 0 => Ok(0),
        Some(n) => Ok(n as usize),
        None => Err(()),
    }
}

/// A base-10 signed integer git would accept: optional `+`/`-`, then digits only,
/// no trailing characters, no overflow. Returns `None` for anything else.
fn parse_int(value: &str) -> Option<i64> {
    let (neg, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// Whether `spec` names a path, so git treats an unresolvable revision token as a
/// pathspec instead of erroring (`git checkout`-style disambiguation, git's
/// `verify_filename`). True when the path is present in the working tree, or is
/// tracked in the index — the latter covers `git log <file>` for a path that was
/// deleted from the worktree but still has history.
fn spec_is_path(repo: &gix::Repository, spec: &str) -> bool {
    if std::path::Path::new(spec).exists() {
        return true;
    }
    let needle = spec.strip_suffix('/').unwrap_or(spec);
    if needle.is_empty() {
        return false;
    }
    let Ok(index) = repo.open_index() else {
        return false;
    };
    let n = needle.as_bytes();
    index.entries().iter().any(|e| {
        let p: &[u8] = e.path(&index).as_ref();
        // Exact file, or a directory prefix (`p` lies under `needle/`).
        p == n || (p.len() > n.len() && p.starts_with(n) && p[n.len()] == b'/')
    })
}

/// The fatal `setup_revisions()` raises for a revision argument it could not
/// resolve. `spec` is the token as written, `^` and range separator included.
///
/// Three shapes, in the order git reaches them:
///
/// * A well-formed object name that is not in the database dies inside `get_oid()`
///   itself — `fatal: bad object <hex>` — before `handle_revision_arg()` ever
///   returns a failure, so the leading `^` has already been stripped by then.
/// * A token starting with `^` (or any token after a `--`) is
///   `die(_("bad revision '%s'"), arg)`, naming the whole token.
/// * Anything else falls through to `verify_filename()`, whose
///   `diagnose_misspelt_rev` text is the three-line "ambiguous argument" — and it
///   too names the whole token, so `nosuch..main` is reported as written rather
///   than as the endpoint that failed.
pub(super) fn bad_revision_message(spec: &str, hex_len: usize) -> String {
    bad_revision_message_gated(spec, hex_len, false)
}

/// [`bad_revision_message`] with the *other* half of its own condition supplied.
///
/// git gates the second shape above on two things, not one
/// (`setup_revisions()`, `revision.c`):
///
/// ```c
/// if (handle_revision_arg(arg, revs, flags, revarg_opt)) {
///         int j;
///         if (seen_dashdash || *arg == '^')
///                 die(_("bad revision '%s'"), arg);
///         for (j = i; j < argc; j++)
///                 verify_filename(revs->prefix, argv[j], j == i);
///         …
/// }
/// ```
///
/// `seen_dashdash` is decided by a scan of the whole argument vector before any
/// operand is resolved, so it is not "after a `--`" in argv order — a separator
/// anywhere makes *every* operand revision-only, including the ones written in
/// front of it. `git diff nosuch..HEAD --` is therefore `bad revision`, while the
/// same operand with no separator is still a pathspec candidate and gets the
/// three-line `ambiguous argument` text.
///
/// A caller that has not scanned for a separator passes `false` and gets exactly
/// the behaviour [`bad_revision_message`] always had.
pub(super) fn bad_revision_message_gated(
    spec: &str,
    hex_len: usize,
    seen_dashdash: bool,
) -> String {
    let bare = spec.strip_prefix('^').unwrap_or(spec);
    if bare.len() == hex_len && bare.bytes().all(|b| b.is_ascii_hexdigit()) {
        return format!("fatal: bad object {bare}\n");
    }
    if seen_dashdash || spec.starts_with('^') {
        return format!("fatal: bad revision '{spec}'\n");
    }
    format!(
        "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    )
}

/// [`bad_revision_message`] with the repository in hand, which is the only way to
/// tell a range apart from a name that merely looks like one.
///
/// `setup_revisions()` reaches `handle_dotdot()` *before* the single-name path, so
/// a token holding `..` gets the range diagnosis when it earns one and falls back
/// to the plain message otherwise. Every caller that has a repository should use
/// this; [`bad_revision_message`] stays as the shape-only half.
pub(super) fn bad_revision_message_in(repo: &gix::Repository, spec: &str) -> String {
    bad_revision_message_in_gated(repo, spec, false)
}

/// [`bad_revision_message_in`] with `setup_revisions()`'s `seen_dashdash` in hand,
/// for a command that scanned its argument vector for the separator — see
/// [`bad_revision_message_gated`] for the C and for why the flag is not positional.
///
/// The two checks in front of the shape test are unaffected by it: a range whose
/// endpoints resolved but whose objects are missing dies in `handle_dotdot_1()`,
/// and a reflog selector past the end of the log dies inside `get_oid()` — both
/// *before* `setup_revisions()` gets its failure back and consults the gate.
pub(super) fn bad_revision_message_in_gated(
    repo: &gix::Repository,
    spec: &str,
    seen_dashdash: bool,
) -> String {
    // `interpret_branch_mark()` dies while `handle_dotdot_1()` is still resolving
    // its first endpoint, so an `@{u}` that names no upstream is reported *before*
    // the range diagnostic — and, since the two `repo_get_oid_with_context()` calls
    // are joined by `||`, only up to the endpoint that failed. What the range
    // resolution then hands back is `-1`, and `handle_revision_arg_1()` re-reads
    // the whole token as one name: `nosuchrev..lonely@{u}` is stock's
    // `fatal: no such branch: 'nosuchrev..lonely'`.
    for endpoint in revision_endpoints(spec) {
        if let Some(message) = crate::objname::upstream_mark_fatal(repo, endpoint) {
            return format!("fatal: {message}\n");
        }
        // Diagnosing a token that has already been resolved and warned about, so
        // this resolution says nothing at all.
        if crate::objname::resolve_quiet(repo, endpoint).is_none() {
            break;
        }
    }
    if let Some(message) = crate::objname::upstream_mark_fatal(repo, spec) {
        return format!("fatal: {message}\n");
    }
    // `prefix_path()` dies inside `get_oid_with_context_1()`, so a `../` path arm
    // that climbs out of the work tree never reaches `die_verify_filename()` and
    // never gets its magic-pathspec guard.
    if let Some(message) = crate::objpath::relative_path_fatal(repo, spec) {
        return format!("fatal: {message}\n");
    }
    if let Some(message) = crate::objname::dotdot_fatal(repo, spec) {
        return message;
    }
    // `read_ref_at()` dies inside `get_oid()` for a selector past the end of the
    // log, so this never becomes the "ambiguous argument" fallback.
    if let Some(crate::objname::ReflogReach::Fatal(message)) =
        crate::objname::reflog_reach(repo, spec.strip_prefix('^').unwrap_or(spec))
    {
        return format!("fatal: {message}\n");
    }
    // `die_verify_filename()` resolves the operand a *second* time before it dies
    // (`maybe_die_on_misspelt_object_name()` → `get_oid_with_context_1()`), so a
    // reflog operand that reached back past its log and then failed for some other
    // reason — `HEAD@{<old date>}^` on a root commit — carries the warning twice:
    // once from the resolution that failed, once from the diagnosis. Verified
    // against stock 2.55.0, which prints two for every `~`/`^<n>` suffix and one
    // for `^{…}`, `:<path>` and the bare name.
    let mut message = String::new();
    if let Some(warning) =
        crate::objname::reflog_reach_warning(repo, spec.strip_prefix('^').unwrap_or(spec))
    {
        message.push_str(&warning);
    }
    // `die_verify_filename()` gives the operand one more pass, with
    // `GET_OID_ONLY_TO_DIE`, and `<rev>:<path>` / `:<n>:<path>` have their own
    // messages there — far more specific than the fallback below. The two shapes
    // that die inside `handle_revision_arg()` never reach it, which is exactly
    // the pair `bad_revision_message_gated` answers with `bad revision`/`bad
    // object`, so the diagnosis is asked for only when the fallback would be the
    // three-line `ambiguous argument` text.
    // `peel_onion()` reports an unreachable `^{<type>}` through `error()` while
    // the resolution is still running, so the line precedes whatever the caller
    // then dies with. `handle_revision_arg_1()` strips the exclusion mark before
    // resolving, so `^main^{blob}` is measured as `main^{blob}`.
    if let Some(peel) =
        crate::objname::peel_type_error(repo, spec.strip_prefix('^').unwrap_or(spec))
    {
        message.push_str(&format!("error: {peel}\n"));
    }
    let generic =
        bad_revision_message_gated(spec, repo.object_hash().len_in_hex(), seen_dashdash);
    if generic.starts_with("fatal: ambiguous argument") {
        // `die_verify_filename()` resolves the operand once more, so its
        // `error()` comes out a second time — but only on this branch: the
        // `bad revision`/`bad object` shapes die inside `handle_revision_arg()`
        // and never reach `verify_filename()`.
        if let Some(peel) = crate::objname::peel_type_error(repo, spec) {
            message.push_str(&format!("error: {peel}\n"));
        }
        if let Some(diagnosis) = crate::objpath::verify_filename_diagnosis(repo, spec) {
            message.push_str(&format!("fatal: {diagnosis}\n"));
            return message;
        }
    }
    message.push_str(&generic);
    message
}

/// The fatal `handle_revision_arg()` raises *itself*, or `None` when the token is
/// still free to become a pathspec.
///
/// [`bad_revision_message_in`] answers "what does git say once this argument has
/// failed"; this answers the question a caller has to ask first, because
/// `setup_revisions()` has two quite different endings:
///
/// ```c
/// if (handle_revision_arg(arg, revs, flags, revarg_opt)) {
///         if (seen_dashdash || *arg == '^')
///                 die(_("bad revision '%s'"), arg);
///         for (j = i; j < argc; j++)
///                 verify_filename(revs->prefix, argv[j], j == i);
///         …
/// }
/// ```
///
/// — a *returned* failure may still end up a pathspec, while the two shapes here
/// die inside `handle_revision_arg()` and never reach that block at all:
///
/// * `dotdot_missing()`, for a range whose endpoints resolved but whose objects
///   are not in the database (see [`crate::objname::dotdot_fatal`]);
/// * `get_reference()`'s `die("bad object %s", name)`, for a name that
///   [`crate::objname::full_hex`] decoded and `parse_object()` could not find —
///   with the leading `^` already stripped, since `handle_revision_arg_1()`
///   advances past it before resolving.
///
/// So a caller with a filename fallback must consult this before taking it: `git
/// log <absent-full-hex>` is `fatal: bad object`, not a pathspec, even when a file
/// of that name is sitting in the working tree.
///
/// One thing does come between the name resolving and `get_reference()` dying,
/// and it is the reason `cant_be_filename` is a parameter:
///
/// ```c
/// if (get_oid_with_context(revs->repo, arg, get_sha1_flags, &oid, &oc))
///         return revs->ignore_missing ? 0 : -1;
/// if (!cant_be_filename)
///         verify_non_filename(revs->prefix, arg);
/// object = get_reference(revs, arg, &oid, flags ^ local_flags);
/// ```
///
/// A name that resolved *and* names an existing file is "both revision and
/// filename" whether or not the object is there, so that message wins over
/// `bad object`. `cant_be_filename` is `REVARG_CANNOT_BE_FILENAME`, which
/// `setup_revisions()` sets only for arguments in front of a `--` it found itself.
///
/// `handle_dotdot_1()` runs the same check on `full_name`, between resolving the
/// two endpoints and `parse_object()`ing them, so a range that is *also* a
/// working-tree path is "both revision and filename" ahead of
/// `dotdot_missing()`. This port takes the range branch first and so misses that
/// ordering; the shape needs a file literally named `<rev>..<rev>`, and no case
/// in the differential or the parity corpus reaches it.
pub(super) fn early_revision_fatal(
    repo: &gix::Repository,
    spec: &str,
    cant_be_filename: bool,
) -> Option<String> {
    if let Some(message) = crate::objname::dotdot_fatal(repo, spec) {
        return Some(message);
    }
    let bare = spec.strip_prefix('^').unwrap_or(spec);
    // `read_ref_at()` dies inside `get_oid()`, so an out-of-range reflog selector
    // never reaches the pathspec fallback the way an unresolvable name does.
    if let Some(crate::objname::ReflogReach::Fatal(message)) =
        crate::objname::reflog_reach(repo, bare)
    {
        return Some(format!("fatal: {message}\n"));
    }
    if !crate::objname::resolves_but_absent(repo, bare) {
        return None;
    }
    if !cant_be_filename {
        if let Some(message) = crate::setup::verify_non_filename(repo, bare) {
            return Some(format!("fatal: {message}\n"));
        }
    }
    Some(bad_revision_message(bare, repo.object_hash().len_in_hex()))
}

// ---------------------------------------------------------------------------
// Revision walk
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Order {
    /// git's default: pure commit-date order.
    Default,
    /// `--date-order`: topological, breaking ties by commit date.
    Date,
    /// `--topo-order`: topological, following the graph rather than the clock.
    Topo,
    /// `--author-date-order`: topological, breaking ties by *author* date.
    ///
    /// `REV_SORT_BY_AUTHOR_DATE` (revision.c:2456-2458). The commit date a walk
    /// normally orders by is the one a rebase or an amend rewrites; the author date
    /// survives both, so this is the order that keeps a rewritten history in the
    /// sequence its patches were written in.
    AuthorDate,
}

/// `--no-walk[=(sorted|unsorted)]` (`revision.c`'s `no_walk` field).
///
/// The named commits are shown and nothing is traversed, which is how a caller
/// asks for "just these objects" — `git log --no-walk <a> <b>` is two records,
/// not two histories.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NoWalk {
    /// The default: the named commits in commit-date order, newest first.
    Sorted,
    /// `--no-walk=unsorted`: the order they were named in.
    Unsorted,
}

/// What the walk needs to know about a commit, read once up front.
/// Everything a commit's record needs beyond the commit itself: the flags and
/// lookup tables that are fixed for the whole command.
struct EntryParams<'a> {
    abbrev_commit: bool,
    /// `--show-signature` / `--no-show-signature`.
    show_signature: bool,
    show_parents: bool,
    /// `--graph`: suppresses the `--boundary` mark, which the `o` node draws instead
    /// (`show_log` skips `put_revision_mark` whenever a graph is active).
    graph: bool,
    /// `--children`: each commit`s children among the walked set, or `None` when
    /// the flag is off.
    children: Option<&'a HashMap<ObjectId, Vec<ObjectId>>>,
    date_mode: DateMode,
    want_color: bool,
    /// The resolved `color.decorate.*` / `color.diff.commit` slots.
    colors: &'a super::color::DecorateColors,
    now: i64,
    decorations: Option<&'a Decorations>,
    decorate: DecorateStyle,
    source_mode: bool,
    /// `--use-mailmap` / `log.mailmap`: the loaded mailmap, or `None` when the
    /// identities are shown as recorded.
    mailmap: Option<&'a Mailmap>,
    /// The mailmap `%aN`/`%aE`/`%cN`/`%cE` read, which is loaded even when the
    /// header formats are not routed through one.
    identity_mailmap: Option<&'a Mailmap>,
    terminator: bool,
    /// `-z`: the byte a `tformat:` record is terminated with.
    rec_term: u8,
    empty_user_format: bool,
    pretty: &'a Pretty,
    /// The notes trees to render after the message; empty when notes are off.
    notes: &'a [super::notes::Tree],
    /// `--expand-tabs[=<n>]` / `--no-expand-tabs`; see [`RenderCtx::expand_tabs`].
    expand_tabs: Option<usize>,
    /// `revs->date_mode_explicit`: see [`RenderCtx::date_explicit`].
    date_explicit: bool,
    /// See [`RenderCtx::email`].
    email: EmailStyle<'a>,
    /// `get_log_output_encoding()` (`environment.c:189-193`): the charset commit
    /// messages are re-coded into before they are rendered. Empty means
    /// `--encoding=none` — hand the stored bytes back untouched.
    output_encoding: &'a str,
}

/// A look-ahead buffer of rendered commit records.
///
/// Reading a commit object and expanding its format is per-commit work with no
/// shared state, and on a deep history it is the entire cost of `--oneline`,
/// `%s` or the default format — the walk itself is already cheap. The window
/// renders `SPAN` records at a time across the thread pool and hands them out in
/// order, so the caller stays a simple in-order loop and memory is bounded by
/// the span, not the history.
struct EntryWindow<'a> {
    params: EntryParams<'a>,
    /// Index of `slots[0]` within the caller's node list.
    start: usize,
    slots: Vec<Vec<u8>>,
}

impl<'a> EntryWindow<'a> {
    /// Records rendered per refill. Records are small (a line for `--oneline`,
    /// a paragraph for the default format), so the span can be wide.
    const SPAN: usize = 256;
    /// Records per worker. An object read plus a format expansion is small work,
    /// so a batch must be sizeable before threads repay their setup.
    const PER_WORKER: usize = 32;

    fn new(params: EntryParams<'a>) -> Self {
        EntryWindow { params, start: 0, slots: Vec::new() }
    }

    /// The rendered record for `nodes[i]`, refilling the window when `i` runs
    /// past it. The record is moved out: the caller appends its diff to it.
    fn get(
        &mut self,
        repo: &gix::Repository,
        nodes: &[Node],
        i: usize,
        abbrev: &std::cell::RefCell<AbbrevCache>,
    ) -> Result<Vec<u8>> {
        if i < self.start || i >= self.start + self.slots.len() {
            let end = (i + Self::SPAN).min(nodes.len());
            self.slots = self.render_span(repo, &nodes[i..end], abbrev)?;
            self.start = i;
        }
        Ok(std::mem::take(&mut self.slots[i - self.start]))
    }

    fn render_span(
        &self,
        repo: &gix::Repository,
        span: &[Node],
        abbrev: &std::cell::RefCell<AbbrevCache>,
    ) -> Result<Vec<Vec<u8>>> {
        let workers = crate::threads::count(span.len(), Self::PER_WORKER);
        if workers <= 1 {
            return span.iter().map(|n| entry_block(repo, n, &self.params, abbrev)).collect();
        }

        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let mut done: Vec<(usize, Vec<u8>)> = Vec::with_capacity(span.len());
        let mut caches: Vec<AbbrevCache> = Vec::with_capacity(workers);
        let mut failure: Option<anyhow::Error> = None;
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for _ in 0..workers {
                let proto = repo.clone();
                // A worker abbreviates ids of its own, so it takes a fork of the
                // cache — the ledger's half is shared, the new half is private
                // until it is merged back below.
                let mine_abbrev = std::cell::RefCell::new(abbrev.borrow().fork());
                let cursor = &cursor;
                let params = &self.params;
                #[allow(clippy::type_complexity)] // per-worker (rows, abbrev-cache) result
                handles.push(scope.spawn(move || -> Result<(Vec<(usize, Vec<u8>)>, AbbrevCache)> {
                    let repo = proto;
                    let mut mine = Vec::new();
                    loop {
                        let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(node) = span.get(i) else { break };
                        mine.push((i, entry_block(&repo, node, params, &mine_abbrev)?));
                    }
                    Ok((mine, mine_abbrev.into_inner()))
                }));
            }
            for h in handles {
                match h.join() {
                    Ok(Ok((mine, cache))) => {
                        done.extend(mine);
                        caches.push(cache);
                    }
                    Ok(Err(e)) => {
                        failure.get_or_insert(e);
                    }
                    Err(_) => {
                        failure.get_or_insert_with(|| anyhow::anyhow!("log worker panicked"));
                    }
                }
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        for cache in caches {
            abbrev.borrow_mut().absorb(cache);
        }

        done.sort_by_key(|(i, _)| *i);
        Ok(done.into_iter().map(|(_, block)| block).collect())
    }
}

/// One commit's rendered record: its header/message block plus the record
/// terminator, with no diff attached.
fn entry_block(
    repo: &gix::Repository,
    node: &Node,
    p: &EntryParams<'_>,
    abbrev: &std::cell::RefCell<AbbrevCache>,
) -> Result<Vec<u8>> {
    entry_block_from(repo, node, p, abbrev, None)
}

/// [`entry_block`] with `show_log()`'s ` (from <oid>)` insert, which the per-parent
/// records `-m` prints carry after the commit id and before any decorations.
fn entry_block_from(
    repo: &gix::Repository,
    node: &Node,
    p: &EntryParams<'_>,
    abbrev: &std::cell::RefCell<AbbrevCache>,
    from: Option<ObjectId>,
) -> Result<Vec<u8>> {
    let mut commit = repo.find_object(node.id)?.try_into_commit()?;
    // `show_log()` renders from `repo_logmsg_reencode(commit, NULL, encoding)`
    // (`pretty.c:2315-2316`) rather than from the stored buffer, so the whole
    // object — headers included — is re-coded before anything reads it.
    //
    // A *user* format takes the other road. `pretty_print_commit()` hands
    // `CMIT_FMT_USERFORMAT` straight to `repo_format_commit_message()`
    // (`pretty.c:2318-2322`), which re-codes the commit to **UTF-8** whatever
    // `--encoding` said (`pretty.c:1734`), expands the format against those bytes,
    // and only then converts the finished record out of UTF-8 into the requested
    // encoding (`pretty.c:2026-2046`). The observable difference is
    // `--encoding=none`: the built-in formats print the stored bytes untouched,
    // while a user format still prints UTF-8.
    // `reference` is not a format of its own in git: `get_commit_format()` sets
    // `CMIT_FMT_USERFORMAT` and saves `%C(auto)%h (%s, %ad)` as the user format
    // (pretty.c`s `setup_commit_format`), so it takes the user-format road too.
    let user_format = matches!(p.pretty, Pretty::User(_) | Pretty::Reference);
    logmsg_reencode(&mut commit.data, if user_format { "UTF-8" } else { p.output_encoding });
    // `--parents` then `--children` decorate the header with ids, in that order
    // (`show_log` prints `print_parents` before `children`). A child list is what
    // the walk saw, so it names only commits this run reached.
    let mut extra = Vec::new();
    let push_ids = |ids: &[ObjectId], out: &mut Vec<u8>| {
        for id in ids {
            out.push(b' ');
            let attached = id.attach(repo);
            if p.abbrev_commit {
                out.extend_from_slice(abbrev.borrow_mut().get(attached).as_bytes());
            } else {
                out.extend_from_slice(attached.to_string().as_bytes());
            }
        }
    };
    if p.show_parents {
        push_ids(&node.parents, &mut extra);
    }
    if let Some(children) = p.children {
        push_ids(children.get(&node.id).map_or(&[][..], Vec::as_slice), &mut extra);
    }
    if let Some(parent) = from {
        extra.extend_from_slice(b" (from ");
        let attached = parent.attach(repo);
        if p.abbrev_commit {
            extra.extend_from_slice(abbrev.borrow_mut().get(attached).as_bytes());
        } else {
            extra.extend_from_slice(attached.to_string().as_bytes());
        }
        extra.push(b')');
    }
    let ctx = RenderCtx {
        abbrev_commit: p.abbrev_commit,
        abbrev,
        date_mode: p.date_mode,
        extra,
        want_color: p.want_color,
        colors: p.colors,
        now: p.now,
        decorations: p.decorations,
        decorate: p.decorate,
        // A `-g` record has no source to print: git never pends the commit itself
        // under `--walk-reflogs`, so `revs->sources` records no name for it and
        // `--source` adds nothing — not even the separating tab.
        source: if p.source_mode && node.reflog.is_none() {
            Some(node.source.as_bytes())
        } else {
            None
        },
        show_signature: p.show_signature,
        mailmap: p.mailmap,
        identity_mailmap: p.identity_mailmap,
        notes: p.notes,
        repo,
        mark: if node.boundary && !p.graph { "- " } else { "" },
        parents: &node.parents,
        graph_width: node.graph_width,
        expand_tabs: p.expand_tabs,
        reflog: node.reflog.as_ref(),
        date_explicit: p.date_explicit,
        email: p.email,
    };
    let mut block: Vec<u8> = Vec::new();
    render_entry(&mut block, &commit, p.pretty, &ctx)?;
    // ```c
    // if (output_enc) {
    //         if (same_encoding(utf8, output_enc))
    //                 output_enc = NULL;
    // } …
    // if (output_enc) {
    //         char *out = reencode_string_len(sb->buf, sb->len, output_enc, utf8, &outsz);
    //         if (out)
    //                 strbuf_attach(sb, out, outsz, outsz + 1);
    // }
    // ```
    //
    // (`pretty.c:2026-2046`.) The conversion is of the *rendered record*, and a
    // conversion that cannot be done leaves the UTF-8 bytes in place. An empty
    // output encoding — `--encoding=none` — reaches `iconv_open("", "UTF-8")`,
    // which is the locale's own charset and changes nothing here.
    if user_format && !p.output_encoding.is_empty() {
        if !super::mailinfo::same_encoding("UTF-8", p.output_encoding) {
            if let Some(out) =
                super::mailinfo::reencode(&block, "UTF-8", p.output_encoding)
            {
                block = out;
            }
        }
    }
    // A `tformat:` record is terminated by a newline. git still terminates a
    // record whose expansion happened to be empty (so `%d` prints one line per
    // commit); only the genuinely empty user format emits no terminator.
    if p.terminator && !p.empty_user_format {
        block.push(p.rec_term);
    }
    Ok(block)
}

/// A look-ahead buffer of rendered `-p` patch bodies.
///
/// The output is a stream, but the work behind it is not sequential: a commit's
/// patch is a pure function of its tree and its first parent's, both immutable.
/// So instead of computing one patch, printing it, and leaving the rest of the
/// machine idle — which is all git can do — the window computes the next
/// `SPAN` commits' patches at once across the thread pool and hands them out in
/// order. Memory stays bounded by the span rather than by the length of the
/// history, so `log -p` over ten thousand commits still streams.
///
/// Commits the caller will not show a diff for (merges, and root commits under
/// `log.showRoot=false`) get an empty slot rather than a wasted diff.
struct PatchWindow {
    active: bool,
    show_root: bool,
    /// `--diff-merges=<mode>`: what a merge commit's patch shows.
    merges: DiffMerges,
    /// `revs->diff`, `log_tree_diff()`'s `all_need_diff` (log-tree.c:1103): whether
    /// the command line asked for diff output at all. A *non*-merge is diffed only
    /// when it is set, which is what leaves `git log --diff-merges=separate` diffing
    /// merges and nothing else.
    all_need_diff: bool,
    /// The diff options the patch bodies are rendered with (`-U<n>`, `-w`, …).
    patch_opts: super::diff::PatchOpts,
    /// Index of `slots[0]` within the caller's node list.
    start: usize,
    slots: Vec<Vec<u8>>,
}

impl PatchWindow {
    /// Commits computed per refill. Large enough to keep every core busy on a
    /// wide box, small enough that the buffered patches stay a few megabytes.
    const SPAN: usize = 64;


    fn new(
        active: bool,
        show_root: bool,
        merges: DiffMerges,
        all_need_diff: bool,
        patch_opts: super::diff::PatchOpts,
    ) -> Self {
        PatchWindow { active, show_root, merges, all_need_diff, patch_opts, start: 0, slots: Vec::new() }
    }

    /// `log_tree_diff()`'s three early returns (log-tree.c:1119-1152): a merge is
    /// diffed only under a `--diff-merges` mode other than `off` — which is what
    /// `-m`/`-c`/`--cc` and `cmd_log`'s `--first-parent` tweak select — every other
    /// commit only when the command line asked for diff output, and a root commit's
    /// empty-tree diff additionally obeys `log.showRoot`.
    fn diffable(&self, node: &Node) -> bool {
        if node.parents.len() > 1 {
            return self.merges != DiffMerges::Off;
        }
        self.all_need_diff && (self.show_root || !node.parents.is_empty())
    }

    /// The patch body for `nodes[i]`, refilling the window when `i` runs past it.
    ///
    /// A merge is rendered here rather than through the batch: its shape depends on
    /// the `--diff-merges` mode, and the combined form needs every parent's tree at
    /// once.
    fn get<'a>(
        &'a mut self,
        repo: &gix::Repository,
        nodes: &[Node],
        i: usize,
        ctx: u32,
        paths: &[String],
    ) -> Result<&'a [u8]> {
        if !self.active {
            return Ok(&[]);
        }
        if i < self.start || i >= self.start + self.slots.len() {
            let end = (i + Self::SPAN).min(nodes.len());
            let span = &nodes[i..end];
            // Only diffable commits become jobs; `at[k]` is the slot that job
            // `k`'s result belongs in, so the batch carries no wasted diffs.
            let mut jobs: Vec<(ObjectId, Option<ObjectId>)> = Vec::with_capacity(span.len());
            let mut at: Vec<usize> = Vec::with_capacity(span.len());
            // A merge under `combined`/`dense-combined` needs every parent tree at
            // once, so it is rendered on its own rather than as a two-way job.
            let mut merged: Vec<(usize, Vec<u8>)> = Vec::new();
            for (k, n) in span.iter().enumerate() {
                if !self.diffable(n) {
                    continue;
                }
                if n.parents.len() > 1 {
                    match self.merges {
                        DiffMerges::Combined | DiffMerges::DenseCombined => {
                            merged.push((
                                k,
                                super::diff::merge_combined_patch_painted(
                                    repo,
                                    n.id,
                                    &n.parents,
                                    paths,
                                    ctx,
                                    self.merges == DiffMerges::DenseCombined,
                                    &self.patch_opts.colors,
                                )?,
                            ));
                            continue;
                        }
                        // `separate` repeats the *record* once per parent, so the
                        // render loop asks for each of those patches itself and this
                        // window has nothing to contribute.
                        DiffMerges::Separate => continue,
                        // `first-parent` is an ordinary two-way job.
                        DiffMerges::FirstParent | DiffMerges::Off => {}
                    }
                }
                jobs.push((n.id, n.parents.first().copied()));
                at.push(k);
            }
            let computed = super::diff::commit_patches(repo, &jobs, &self.patch_opts, paths, false)?;
            self.slots = vec![Vec::new(); span.len()];
            for (slot, patch) in at.into_iter().zip(computed) {
                self.slots[slot] = patch;
            }
            for (slot, patch) in merged {
                self.slots[slot] = patch;
            }
            self.start = i;
        }
        Ok(&self.slots[i - self.start])
    }
}

/// `--diff-merges=<mode>` (diff-merges.c): what a merge commit's patch shows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DiffMerges {
    /// The default: a merge gets no patch.
    Off,
    /// `-m` / `--diff-merges=separate`: one ordinary patch per parent, each
    /// preceded by its own copy of the commit header.
    Separate,
    /// `-c` / `--diff-merges=combined`: one `diff --combined` section per path
    /// that differs from every parent.
    Combined,
    /// `--cc` / `--diff-merges=dense-combined`: the same, headed `diff --cc`.
    DenseCombined,
    /// `--diff-merges=first-parent`: an ordinary patch against the first parent.
    FirstParent,
}

impl DiffMerges {
    pub(crate) fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "off" | "none" => DiffMerges::Off,
            "m" | "separate" => DiffMerges::Separate,
            "c" | "combined" => DiffMerges::Combined,
            "cc" | "dense-combined" => DiffMerges::DenseCombined,
            "1" | "first-parent" => DiffMerges::FirstParent,
            // `func_by_opt()`'s `"m"`/`"on"` arm returns `set_to_default`, whose
            // initial value is `set_separate` (diff-merges.c:10). Only the
            // `diff.mergesDefault`/`log.diffMerges` config can move it, and that is
            // not read here, so `on` is `separate`.
            "on" => DiffMerges::Separate,
            _ => return None,
        })
    }
}

#[derive(Clone)]
pub(crate) struct Node {
    pub(crate) id: ObjectId,
    pub(crate) parents: Vec<ObjectId>,
    pub(crate) time: i64,
    /// `--source`: the ref/argument this commit was first reached from. Empty when
    /// `--source` is off (the field is never rendered in that case).
    pub(crate) source: String,
    /// Order this node entered the frontier, which is what breaks a date tie.
    /// Set by the walk at push time; never rendered.
    pub(crate) seq: u64,
    /// `--boundary`: an excluded commit that a shown commit descends from, which
    /// git prints with a `-` mark after the rest of the walk.
    pub(crate) boundary: bool,
    /// `--follow`: the name the tracked file had *at this commit*, which is what
    /// its diff and name formats are limited to. `None` when not following.
    pub(crate) follow_path: Option<gix::bstr::BString>,
    /// `graph_width(opt->graph)` at the moment `show_log()` renders this commit —
    /// the width of the `--graph` prefix its first row carries, which a `%<|(<N>)`
    /// column target has to leave room for. Zero unless `--graph` is on; filled in
    /// by [`measure_graph_widths`] once the walk's node list is final.
    pub(crate) graph_width: i32,
    /// `-g`/`--walk-reflogs`: the reflog entry this record stands for. `None` for
    /// an ordinary ancestry walk, which is what leaves every `%g…` placeholder and
    /// the `Reflog:` header lines empty.
    pub(crate) reflog: Option<ReflogEntry>,
}

/// One walked reflog entry: git's `struct reflog_info` together with the
/// `<ref>@{…}` selector [`ReflogEntry::selector`] prints for it.
///
/// A reflog walk yields one of these per entry, newest first, and the same commit
/// may appear under several of them — a reflog records where a ref *was*, so a
/// reset and the commit it went back to are two entries naming one object.
#[derive(Clone)]
pub(crate) struct ReflogEntry {
    /// `commit_reflog->reflogs->ref`: the ref as `-g` was asked for it — `HEAD`,
    /// `main`, or the full name `--all` supplies. The `Reflog:` line and `%gD`
    /// print it as-is; `%gd` shortens it.
    refname: String,
    /// `reflogs->nr - 2 - recno` once the entry has been consumed: its `@{<n>}`.
    index: usize,
    /// The entry's own clock — what `@{<date>}` prints under an explicit `--date=`,
    /// and what orders a walk spanning several reflogs.
    time: gix::date::Time,
    /// `info->email`, which git prints whole inside the `Reflog:` parentheses.
    who_name: Vec<u8>,
    who_email: Vec<u8>,
    /// The entry's message, without its trailing newline (`%gs`).
    message: Vec<u8>,
    /// `info->noid`: what the ref pointed at *after* this update, which is the
    /// commit the record shows.
    new_oid: ObjectId,
    /// `commit_reflog->selector`: how the walk was asked for, which decides
    /// whether the selector prints an index or a date.
    kind: ReflogSelector,
}

/// git's `enum selector_type`: how the `@{…}` on a `-g` argument was spelled.
///
/// It outranks `--date=`: `HEAD@{1}` keeps index selectors under `--date=short`,
/// and `HEAD@{yesterday}` prints dates without one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReflogSelector {
    /// No `@{…}`: index form unless `--date=` was given (git's `force_date`).
    None,
    /// `@{<n>}`: always the index form.
    Index,
    /// `@{<date>}`: always the date form.
    Date,
}

/// Where in a ref's log a `-g <name>` argument starts, per `add_reflog_for_walk()`.
enum ReflogStart {
    /// No `@{…}` suffix: the newest entry.
    Newest,
    /// `@{<n>}`: `n` entries back from the newest.
    Index(usize),
    /// `@{<date>}`: the newest entry that was already current then.
    Date(i64),
}

/// Split a `-g` argument into the ref to read and the entry to start at.
///
/// `add_reflog_for_walk()` looks at the *first* `@`, requires a `{` after it, and
/// reads the rest with `strtoul`: digits followed by `}` is an index, anything
/// else goes to `approxidate()`. A name without `@{` starts at the newest entry.
fn split_reflog_name(name: &str) -> (&str, ReflogStart) {
    let Some(at) = name.find('@') else {
        return (name, ReflogStart::Newest);
    };
    let Some(inner) = name[at + 1..].strip_prefix('{') else {
        return (name, ReflogStart::Newest);
    };
    let base = &name[..at];
    let digits = inner.len() - inner.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if inner[digits..].starts_with('}') {
        // `strtoul` answers 0 for an empty run, which is what `@{}` gets.
        (base, ReflogStart::Index(inner[..digits].parse().unwrap_or(0)))
    } else {
        // git hands `approxidate` the tail as-is, closing brace included; it
        // tokenizes on non-alphanumerics and ignores what it cannot read.
        (base, ReflogStart::Date(crate::date::approxidate(inner)))
    }
}

impl ReflogEntry {
    /// `get_reflog_selector()`: `<ref>@{<n>}`, or `<ref>@{<date>}` when `--date=`
    /// was given on the command line (git's `force_date`, i.e.
    /// `revs->date_mode_explicit`). `shorten` is `%gd`'s short ref; the `Reflog:`
    /// line and `%gD` pass `false`.
    fn selector(
        &self,
        repo: &gix::Repository,
        shorten: bool,
        date_mode: DateMode,
        force_date: bool,
        now: i64,
    ) -> String {
        // `refs_shorten_unambiguous_ref(store, ref, 0)` (reflog-walk.c:252) — the
        // *non-strict* form, which walks `ref_rev_parse_rules` and keeps the
        // shortest suffix that resolves back to this ref and no other. A plain
        // category strip is not the same function: with both `refs/heads/dup` and
        // `refs/tags/dup` present, stock shortens `refs/heads/dup` to `heads/dup`
        // (measured), because `dup` would be ambiguous.
        let name = if shorten {
            super::reflog::shorten_ref_unambiguous(repo, &self.refname)
        } else {
            self.refname.clone()
        };
        // `selector == SELECTOR_DATE || (selector == SELECTOR_NONE && force_date)`.
        let force_date = match self.kind {
            ReflogSelector::Date => true,
            ReflogSelector::Index => false,
            ReflogSelector::None => force_date,
        };
        if force_date {
            let when = fmt_time(self.time.seconds, self.time.offset, date_mode, now);
            format!("{name}@{{{when}}}")
        } else {
            format!("{name}@{{{}}}", self.index)
        }
    }
}

/// The name `commit_reflog->reflogs->ref` ends up holding, which is what the
/// `Reflog:` header and `%gD` print (and what `%gd` shortens).
///
/// `read_complete_reflog()` looks for the log file under four spellings of the
/// argument — as typed, under the reference it resolves to, `refs/<name>` and
/// `refs/heads/<name>` — and records the name **as typed** whenever one of them
/// answers. Only when all four come up empty does `add_reflog_for_walk()` fall
/// back to `repo_dwim_log()` and *replace* the name with the full ref it found.
/// That is why `-g main` keeps `main@{0}` while `-g origin/main` prints
/// `refs/remotes/origin/main@{0}`: no `refs/origin/main` or
/// `refs/heads/origin/main` exists, so only the `refs/remotes/` rule reaches it.
fn reflog_display_name(repo: &gix::Repository, name: &str) -> String {
    if reflog_candidates(repo, name)
        .iter()
        .any(|cand| super::reflog::log_file(repo, cand).is_file())
    {
        return name.to_string();
    }
    super::reflog::dwim_log(repo, name).unwrap_or_else(|| name.to_string())
}

/// `read_complete_reflog()`'s four spellings of the argument (reflog-walk.c:68-103),
/// in the order it tries them: as typed, then the reference it resolves to under
/// `RESOLVE_REF_READING`, then `refs/<name>`, then `refs/heads/<name>`.
///
/// The last two are what make `git log -g <name>` work for a name whose *resolution*
/// lands somewhere without a log. With both a branch and a tag called `dup`,
/// `refs_resolve_refdup()` answers `refs/tags/dup` (tags precede heads in
/// `ref_rev_parse_rules`) and that ref has no reflog, so only the `refs/heads/`
/// spelling reaches the branch's log — measured against stock 2.55.0, which prints
/// `dup@{0}` there while resolution alone finds nothing.
fn reflog_candidates(repo: &gix::Repository, name: &str) -> Vec<String> {
    [
        Some(name.to_string()),
        super::reflog::resolve_ref_reading(repo, name),
        Some(format!("refs/{name}")),
        Some(format!("refs/heads/{name}")),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// `read_complete_reflog()`: a ref's whole reflog in git's stored order (oldest
/// entry first), each entry carrying the `@{<n>}` index its selector prints.
///
/// `None` means the ref has no log. That is not an error: `add_reflog_for_walk()`
/// answers -1 and `add_pending_object_with_path()` returns without pending the
/// commit either way, so `git log -g <tag>` prints nothing and exits 0.
fn read_reflog(
    repo: &gix::Repository,
    name: &str,
    kind: ReflogSelector,
) -> Result<Option<Vec<ReflogEntry>>> {
    // `reflogs->ref` is the argument *as typed* (reflog-walk.c:71), whichever
    // spelling ends up supplying the entries.
    let refname = reflog_display_name(repo, name);
    let mut items: Vec<ReflogEntry> = Vec::new();
    for cand in reflog_candidates(repo, name) {
        let Some(reference) = repo.try_find_reference(cand.as_str()).ok().flatten() else {
            continue;
        };
        let mut platform = reference.log_iter();
        let Some(iter) = platform.all()? else {
            continue;
        };
        read_reflog_entries(iter, &refname, kind, &mut items);
        // `if (reflogs->nr == 0)` guards each fallback, so the first spelling with
        // any entry at all is the one that answers.
        if !items.is_empty() {
            break;
        }
    }
    if items.is_empty() {
        return Ok(None);
    }
    let nr = items.len();
    for (k, item) in items.iter_mut().enumerate() {
        item.index = nr - 1 - k;
    }
    Ok(Some(items))
}

/// `read_one_reflog()`: one stored line per entry, in the order the file holds them
/// (oldest first). The `@{<n>}` index is filled in by the caller, since it counts
/// down from the newest and is only known once the whole log has been read.
fn read_reflog_entries<'a>(
    iter: impl Iterator<Item = std::result::Result<gix::refs::file::log::LineRef<'a>, gix::refs::file::log::iter::decode::Error>>,
    refname: &str,
    kind: ReflogSelector,
    items: &mut Vec<ReflogEntry>,
) {
    for line in iter {
        let Ok(line) = line else { break };
        items.push(ReflogEntry {
            refname: refname.to_string(),
            // Filled in below: the index counts down from the newest entry, so it
            // is only known once the whole log has been read.
            index: 0,
            time: line.signature.time().ok().unwrap_or_default(),
            who_name: line.signature.name.to_vec(),
            who_email: line.signature.email.to_vec(),
            message: line.message.to_vec(),
            new_oid: line.new_oid(),
            kind,
        });
    }
}

/// One `struct commit_reflog`: a ref's complete log plus `recno`, the cursor that
/// walks it from the newest entry down.
struct ReflogCursor {
    items: Vec<ReflogEntry>,
    recno: i64,
}

impl ReflogCursor {
    /// `next_reflog_commit()`: step `recno` past any entry whose new object is not
    /// a commit — a reflog records ref updates, and a ref may have pointed at a
    /// tag, a tree, or an object this repository no longer holds.
    fn advance(&mut self, repo: &gix::Repository) {
        while self.recno >= 0 {
            let id = self.items[self.recno as usize].new_oid;
            if matches!(repo.find_header(id).map(|h| h.kind()), Ok(gix::objs::Kind::Commit)) {
                return;
            }
            self.recno -= 1;
        }
    }

    /// `log_timestamp()`: the clock of the entry the cursor is sitting on.
    fn time(&self) -> i64 {
        self.items[self.recno as usize].time.seconds
    }
}

/// `-g`/`--walk-reflogs`: the node list `next_reflog_entry()` produces.
///
/// Every named ref's log is read whole, then drained newest-entry-first across all
/// of them at once — git picks the log whose current entry is newest, and compares
/// with `>` so a tie leaves the earlier-named log ahead. Each node keeps the
/// commit's **real** parents: git saves them before the walk and `log_tree_diff()`
/// reads them back, so both the diff a record shows and the pathspec pruning that
/// decides whether it is shown at all are the ordinary ones. Only the *selection*
/// of commits comes from the reflog, which is why one commit can appear several
/// times over.
fn reflog_walk(repo: &gix::Repository, names: &[String]) -> Result<Vec<Node>> {
    let reader = NodeReader::new(repo);
    let mut logs: Vec<ReflogCursor> = Vec::new();
    for name in names {
        let (base, start) = split_reflog_name(name);
        let kind = match start {
            ReflogStart::Newest => ReflogSelector::None,
            ReflogStart::Index(_) => ReflogSelector::Index,
            ReflogStart::Date(_) => ReflogSelector::Date,
        };
        // `if (*branch == '\0') branch = resolve_refdup("HEAD")`: a bare `@{…}`
        // names the branch `HEAD` points at, so the selector shows that ref.
        let resolved;
        let base = if base.is_empty() {
            resolved = repo
                .head()
                .ok()
                .and_then(|h| h.referent_name().map(|n| n.as_bstr().to_str_lossy().into_owned()))
                .unwrap_or_else(|| "HEAD".to_string());
            resolved.as_str()
        } else {
            base
        };
        let Some(items) = read_reflog(repo, base, kind)? else { continue };
        let nr = items.len() as i64;
        let recno = match start {
            ReflogStart::Newest => nr - 1,
            // `commit_reflog->recno = reflogs->nr - recno - 1`, unchecked: a count
            // past the end simply leaves the cursor below zero and walks nothing.
            ReflogStart::Index(n) => nr - n as i64 - 1,
            // `get_reflog_recno_by_time()`: the newest entry already current then,
            // or nothing at all when the log does not go back that far.
            ReflogStart::Date(when) => {
                match (0..items.len()).rev().find(|&i| when >= items[i].time.seconds) {
                    Some(i) => i as i64,
                    None => continue,
                }
            }
        };
        let mut cursor = ReflogCursor { items, recno };
        cursor.advance(repo);
        logs.push(cursor);
    }

    let mut nodes: Vec<Node> = Vec::new();
    loop {
        let mut best: Option<usize> = None;
        for (li, log) in logs.iter().enumerate() {
            if log.recno < 0 {
                continue;
            }
            if best.is_none_or(|b| log.time() > logs[b].time()) {
                best = Some(li);
            }
        }
        let Some(bi) = best else { break };
        let entry = logs[bi].items[logs[bi].recno as usize].clone();
        logs[bi].recno -= 1;
        logs[bi].advance(repo);
        let mut node = reader.read(repo, entry.new_oid)?;
        node.seq = nodes.len() as u64;
        node.reflog = Some(entry);
        nodes.push(node);
    }
    Ok(nodes)
}

/// The excluded revision a `--walk-reflogs` run would have to start from, spelled
/// the way `add_reflog_for_walk()` names it.
///
/// git expands `a..b` into `b ^a` and `a...b` into `a b ^<merge-base>`, so either
/// range pends an UNINTERESTING tip; `--not` swaps which side that is. `None` when
/// every argument is a plain positive revision.
fn reflog_excluded_tip(
    repo: &gix::Repository,
    revs: &[String],
    negated: &[bool],
) -> Option<String> {
    for (spec, flip) in revs.iter().zip(negated.iter().copied()) {
        if let Some(rest) = spec.strip_prefix('^') {
            if !flip {
                return Some(rest.to_string());
            }
            continue;
        }
        if let Some((lhs, rhs)) = spec.split_once("...") {
            let l = if lhs.is_empty() { "HEAD" } else { lhs };
            let r = if rhs.is_empty() { "HEAD" } else { rhs };
            let (l, r) = (repo.rev_parse_single(l).ok()?, repo.rev_parse_single(r).ok()?);
            return repo.merge_base(l, r).ok().map(|b| b.detach().to_string());
        }
        if let Some((lhs, rhs)) = spec.split_once("..") {
            let excluded = if flip { rhs } else { lhs };
            return Some(if excluded.is_empty() { "HEAD".to_string() } else { excluded.to_string() });
        }
        if flip {
            return Some(spec.clone());
        }
    }
    None
}

/// Heap order for the walk's frontier: newest commit-date first, ties broken by
/// insertion order — the commit that entered the frontier first pops first.
///
/// git's frontier is a list kept sorted by `commit_list_insert_by_date()`, which
/// walks past every entry whose date is *not older* than the new one before
/// splicing it in. Equal dates therefore come out first-in-first-out, and equal
/// dates are the norm rather than the exception: an import, a scripted series,
/// or any two commits inside the same second all tie. Breaking those ties by
/// object id instead reorders `git log` against git — and against this port's
/// own `rev-list`, which goes through gitoxide's date-ordered walk.
impl Ord for Node {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time.cmp(&other.time).then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.seq == other.seq
    }
}
impl Eq for Node {}

/// Abbreviated object ids, memoised in the zero-copy cache.
///
/// `gix`'s `shorten_or_id()` disambiguates a prefix against the whole object
/// database on EVERY call, which measures ~60-90us per id here regardless of
/// repository size — on a 6375-commit `log --oneline` that alone is ~480ms of
/// the 530ms runtime (`%H` renders in 50ms, `%h` in 534ms; the delta is nothing
/// but abbreviation). git pays a couple of binary searches for the same answer.
///
/// The answer is a pure function of an immutable object id and the repository's
/// current hex length, so it is cached machine-wide, keyed by both. Object ids
/// are content addresses, so an entry is valid for every clone that holds the
/// object; the hex length is part of the key because the correct abbreviation
/// grows as a repository does, and a stale, now-ambiguous prefix must never be
/// served.
///
/// Nothing is loaded up front. The store is an mmap'd image searched per id
/// (`crate::rcache`), so a `log -5` touches a handful of pages instead of
/// decoding every abbreviation the machine has ever computed — which is what
/// reading them out of the SQLite ledger cost, hex-parsing each id on the way in.
struct AbbrevCache {
    /// The width `gix::Id::shorten_or_id()` will start disambiguating from for
    /// this repository, which is the half of the key that is not the object id.
    ///
    /// It has to be the *resolved* width and not the raw `core.abbrev` text,
    /// because `core.abbrev` is three different things (`git_default_core_config()`,
    /// environment.c):
    ///
    /// ```c
    /// if (!strcmp(var, "core.abbrev")) {
    ///         if (!value)
    ///                 return config_error_nonbool(var);
    ///         if (!strcasecmp(value, "auto"))
    ///                 default_abbrev = -1;
    ///         else if (!git_parse_maybe_bool_text(value))
    ///                 default_abbrev = GIT_MAX_HEXSZ;
    ///         else {
    ///                 int abbrev = git_config_int(var, value, ctx->kvi);
    ///                 if (abbrev < minimum_abbrev)
    ///                         return error(_("abbrev length out of range: %d"), abbrev);
    ///                 default_abbrev = abbrev;
    ///         }
    ///         return 0;
    /// }
    /// ```
    ///
    /// `auto` is a *derived* width and a false-y word (`no`, `off`, `false`, the
    /// empty value) is the **whole** hash — two answers that are never the same
    /// and must never share a key. Reading the key with `.integer("core.abbrev")`
    /// did share it: no integer parses out of `no`, so it and `auto` both landed
    /// on `0`. One `git -c core.abbrev=no log --oneline` then wrote every id it
    /// touched into `~/.zvcs/cache/abbrev` at 40 characters under the key `auto`
    /// reads, and every later `git log --oneline` in any clone on that machine
    /// served the full hash back — a persistent, machine-wide corruption from a
    /// single invocation, not per-process state.
    ///
    /// Resolving `auto` to its concrete width fixes a second, quieter version of
    /// the same hole: at `0` every `auto` repository shared one key, so an
    /// abbreviation computed while a repository was small stayed cached after it
    /// grew enough to need a longer prefix. [`crate::abbrev::configured_abbrev`]
    /// is the port of the C above and is shared with every other verb that has to
    /// agree on this width.
    hex_len: usize,
    /// Abbreviations computed by THIS cache. Anything else is one lookup away in
    /// the shared image, so only what the image lacks is held here.
    local: std::collections::HashMap<ObjectId, String>,
    /// New rows for the cache, keyed by the id's raw bytes as the image keys them.
    fresh: Vec<(Vec<u8>, String)>,
}

impl AbbrevCache {
    fn new(repo: &gix::Repository) -> Self {
        // See [`AbbrevCache::hex_len`]: the resolved width, never the raw config
        // text, because `no`/`off`/`false` and `auto` are different answers.
        let hex_len = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
        AbbrevCache { hex_len, local: Default::default(), fresh: Vec::new() }
    }

    /// A cache for a worker thread: the shared image needs no handing over, and
    /// anything the worker computes stays private until
    /// [`absorb`](Self::absorb) takes it.
    fn fork(&self) -> Self {
        AbbrevCache { hex_len: self.hex_len, local: Default::default(), fresh: Vec::new() }
    }

    /// Take what a forked cache computed. Two workers may have shortened the same
    /// id, which is harmless — the cache write is keyed by id, so a duplicate row
    /// overwrites itself with the same value.
    fn absorb(&mut self, other: Self) {
        self.local.extend(other.local);
        self.fresh.extend(other.fresh);
    }

    fn get(&mut self, id: gix::Id<'_>) -> String {
        let oid = id.detach();
        if let Some(short) = self.local.get(&oid) {
            return short.clone();
        }
        if let Some(short) = crate::rcache::abbrev_load(oid.as_slice(), self.hex_len) {
            return short.to_string();
        }
        let short = id.shorten_or_id().to_string();
        self.fresh.push((oid.as_slice().to_vec(), short.clone()));
        self.local.insert(oid, short.clone());
        short
    }

    /// Hand what this run computed to the cache's writer thread.
    ///
    /// The rows are queued rather than written here: the command has its
    /// abbreviations already, and `run()` waits for the queue once, after the
    /// output is on its way. Losing a batch would only cost a recomputation, so
    /// nothing here reports an error.
    fn flush(self) {
        if self.fresh.is_empty() {
            return;
        }
        crate::rcache::cache_write(crate::rcache::CacheWrite::Abbrev {
            hex_len: self.hex_len,
            rows: self.fresh,
        });
    }
}

/// The byte two hex digits name, or `None` if either is not a hex digit. Upper
/// and lower case both count, as in git.
fn hex_byte(hi: Option<char>, lo: Option<char>) -> Option<u8> {
    let nibble = |c: Option<char>| c?.to_digit(16).map(|v| v as u8);
    Some((nibble(hi)? << 4) | nibble(lo)?)
}

/// Whether a user format can be rendered from the WALK alone — the ids and
/// parents already in hand — without reading each commit object.
///
/// git's `%H`/`%h`/`%P`/`%p` need nothing the walk did not already produce, and
/// on a deep history the object read is the whole cost: rendering `%H` for 6375
/// commits spent ~40ms opening objects for data it never used, which is why
/// zvcs's `%H` was slower than its own `--oneline`.
///
/// Anything else — a date, an author, a message, a decoration, a colour — still
/// takes the object, so the check is a deliberate whitelist: an unknown
/// placeholder answers `false` and keeps the faithful path.
fn format_is_walk_only(fmt: &str) -> bool {
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            i += 1;
            continue;
        }
        let Some(&p) = chars.get(i + 1) else { return false };
        match p {
            'H' | 'h' | 'P' | 'p' | 'n' | '%' => i += 2,
            _ => return false,
        }
    }
    true
}

/// Expand a walk-only format for one node. Mirrors the placeholder handling in
/// [`expand_format`] for exactly the subset [`format_is_walk_only`] admits.
fn expand_walk_only(
    out: &mut Vec<u8>,
    fmt: &str,
    node: &Node,
    abbrev_commit: bool,
    cache: &std::cell::RefCell<AbbrevCache>,
    repo: &gix::Repository,
) {
    let short = |id: ObjectId| -> String {
        if abbrev_commit || true {
            cache.borrow_mut().get(id.attach(repo))
        } else {
            id.to_string()
        }
    };
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(chars[i].encode_utf8(&mut buf).as_bytes());
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('H') => out.extend_from_slice(node.id.to_string().as_bytes()),
            Some('h') => out.extend_from_slice(short(node.id).as_bytes()),
            Some('P') => {
                let text: Vec<String> = node.parents.iter().map(ToString::to_string).collect();
                out.extend_from_slice(text.join(" ").as_bytes());
            }
            Some('p') => {
                let text: Vec<String> = node.parents.iter().map(|p| short(*p)).collect();
                out.extend_from_slice(text.join(" ").as_bytes());
            }
            Some('n') => out.push(b'\n'),
            Some('%') => out.push(b'%'),
            _ => {}
        }
        i += 2;
    }
}

/// Resolve a single revision to its object id (a range endpoint), `Err(())` if it
/// doesn't name anything — the caller turns that into git's bad-revision error.
/// The names `handle_revision_arg_1()` hands to `get_oid_with_context()` for one
/// revision argument: both endpoints of a range, the base of a `^@`/`^!` mark,
/// or the argument itself with any leading `^` stripped.
///
/// Only used to decide what to warn about — the resolutions themselves are spread
/// across the branches below, and several of them ask twice.
/// Everything `get_oid_basic()` writes to stderr for one revision operand, in
/// git's order: the `refname … is ambiguous.` block (object-name.c:902-912 and
/// 964-967) first, then the reflog reach warning (object-name.c:1006-1011).
///
/// `handle_revision_arg_1()` puts every endpoint of the token through
/// `get_oid_with_context()`, so both fire once per endpoint — twice for a range.
/// A range is not two independent operands, though: `handle_dotdot_1()` joins its
/// two resolutions with `||`
///
/// ```c
/// if (repo_get_oid_with_context(r, a, oc_flags, &a_oid, &a_oc) ||
///     repo_get_oid_with_context(r, b, oc_flags, &b_oid, &b_oc))
///         return -1;
/// ```
///
/// so a left endpoint that does not resolve means the right one is never looked
/// at and warns about nothing — `nosuch..<40-hex-ref>` is silent in stock 2.55.0
/// while `<40-hex-ref>..nosuch` warns once.
/// [`crate::objname::warn_dotdot_endpoints`] owns that rule for the ambiguity
/// half; the reflog half stops at the same place.
///
/// `ambiguity` is `warn_on_object_refname_ambiguity`, which
/// `read_revisions_from_stdin()` clears around a `--stdin` read. The reflog
/// warning carries no such gate, so it fires for a stdin line too.
pub(super) fn warn_operand(repo: &gix::Repository, spec: &str, ambiguity: bool) {
    let endpoints = revision_endpoints(spec);
    let range = crate::objname::split_range(spec).is_some();
    {
        // `ambiguity` is git's own switch, so hold it rather than skip the block:
        // it gates `get_oid_basic()`'s full-hex branch only, and the plain-name
        // warning underneath it has no such gate. Stock
        // `printf dup | git rev-list --stdin` prints
        // `warning: refname 'dup' is ambiguous.` while the same command is silent
        // for a 40-hex ref name.
        let _switch = (!ambiguity).then(crate::objname::AmbiguityWarnings::off);
        if range {
            crate::objname::warn_dotdot_endpoints(repo, spec);
        } else {
            for endpoint in &endpoints {
                crate::objname::warn_ambiguous_refname(repo, endpoint);
            }
        }
    }
    for endpoint in &endpoints {
        // `handle_dotdot()` runs before `handle_revision_arg_1()` strips the
        // exclusion mark, so an endpoint still carrying it fails the pair.
        if range && endpoint.starts_with('^') {
            break;
        }
        // `read_ref_at()`'s own `warning()` (`refs.c:1135`, `refs.c:1141`) comes
        // out of the same `get_oid_basic()` call as the one below it, just from
        // one frame deeper, so both belong on this one pass over the endpoints.
        if let Some(message) = crate::objname::read_ref_at_warning(repo, endpoint) {
            eprintln!("warning: {message}");
        }
        if let Some(warning) = crate::objname::reflog_reach_warning(repo, endpoint) {
            eprint!("{warning}");
        }
        if range {
            // Already warned about above, so this is the same resolution rather
            // than a second operand — `resolve_quiet` and not
            // [`crate::objname::AmbiguityWarnings`], whose switch git only reads
            // in `get_oid_basic()`'s full-hex branch and so cannot suppress the
            // plain-name warning.
            if crate::objname::resolve_quiet(repo, endpoint).is_none() {
                break;
            }
        }
    }
}

pub(super) fn revision_endpoints(spec: &str) -> Vec<&str> {
    if let Some(range) = crate::objname::split_range(spec) {
        return vec![range.a, range.b];
    }
    let base = spec
        .strip_suffix("^@")
        .or_else(|| spec.strip_suffix("^!"))
        .filter(|b| !b.is_empty())
        .unwrap_or(spec);
    vec![base.strip_prefix('^').filter(|rest| !rest.is_empty()).unwrap_or(base)]
}

fn resolve_rev(repo: &gix::Repository, spec: &str) -> Result<ObjectId, ()> {
    repo.rev_parse_single(spec).map(|id| peel_to_commit(repo, id.detach())).map_err(|_| ())
}

/// The commit a revision names, following annotated tags.
///
/// `git log v1.0` walks from the COMMIT the tag points at; the tag object itself
/// is not a walkable node. Without this, every release tag — the most natural
/// thing to `git log` — failed with "was supposed to be of kind commit, but was
/// kind tag". A spec that names something with no commit behind it (a tree, a
/// blob) is left as-is so the walk reports it the way git does.
fn peel_to_commit(repo: &gix::Repository, id: ObjectId) -> ObjectId {
    repo.find_object(id)
        .ok()
        .and_then(|obj| obj.peel_tags_to_end().ok())
        .filter(|obj| obj.kind == gix::object::Kind::Commit)
        .map_or(id, |obj| obj.id)
}

/// Whether a revision argument puts anything on the UNINTERESTING side, judged from
/// the token alone.
///
/// `handle_revision_arg()` answers this from the objects it resolved, but a command
/// still scanning its arguments needs the same answer before it resolves anything:
/// `add_pending_object_with_path()` clears `revs->no_walk` the moment an
/// UNINTERESTING object is pended, which is `git-rev-list(1)`'s "`--no-walk` … has
/// no effect if a range is specified". Both spellings are positional, so the test
/// has to run per token rather than over the finished set.
///
/// Five forms exclude: `^<rev>`, the left side of `<a>..<b>`, the merge bases of
/// `<a>...<b>`, the parents `<rev>^!` adds, and — only under `--not`, which flips
/// them — the parents `<rev>^@` adds. `--not` flips the sense of the first and
/// leaves the ranges and `^!` alone, because each of those excludes one side of
/// itself whichever way round it is read.
pub(super) fn argument_excludes(spec: &str, negated: bool) -> bool {
    if spec.contains("..") {
        return true;
    }
    match crate::objname::parents_only(spec) {
        // `^!` and `^-<n>` pend the selected parents with
        // `flags ^ (UNINTERESTING | BOTTOM)` and the commit itself with `flags`
        // (revision.c:2186-2207), so whichever way `--not` leaves them one of the
        // two sides is always UNINTERESTING.
        crate::objname::ParentsOnly::Mark { replaces: false, .. } => true,
        // `^@` pends only the parents, and with `flags` unchanged — so they are
        // UNINTERESTING exactly when `--not` or a leading `^` on the base says so.
        crate::objname::ParentsOnly::Mark { base, replaces: true, .. } => {
            negated ^ base.starts_with('^')
        }
        _ => spec.starts_with('^') != negated,
    }
}

/// The commits a revision argument's `~<n>` / `^<n>` chain reads the parents of.
///
/// `get_oid_1()` follows such a chain one commit at a time, parsing each commit
/// it steps *off*. Which commits are parsed matters under `--no-walk`, where
/// `mark_parents_uninteresting()` is the only thing that spreads UNINTERESTING
/// and stops as soon as it meets a commit whose parents are not loaded yet
/// (revision.c:262-269, "normally we haven't parsed the parent yet"). So
/// `git rev-list --no-walk ^main main~2` drops `main~2` — walking to it parsed
/// the commit in between — while `^main side` keeps `side`, which names the same
/// generation through a ref and parses nothing on the way.
///
/// Only the `~<n>` and `^<n>` suffixes are read here. Any other spelling
/// (`^{commit}`, `@{…}`, `:/text`) ends the scan and contributes nothing, which
/// is the safe direction: fewer commits known to be parsed means less of the
/// history is marked, never more.
pub(super) fn navigation_path(repo: &gix::Repository, spec: &str) -> Vec<ObjectId> {
    // Peel the trailing chain off the base name, right to left.
    let mut ops: Vec<(u8, usize)> = Vec::new();
    let mut end = spec.len();
    while let Some(pos) = spec[..end].rfind(['~', '^']) {
        let tail = &spec[pos + 1..end];
        if !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        ops.push((spec.as_bytes()[pos], if tail.is_empty() { 1 } else { tail.parse().unwrap_or(1) }));
        end = pos;
    }
    if ops.is_empty() || end == 0 {
        return Vec::new();
    }
    ops.reverse();
    let Ok(base) = repo.rev_parse_single(&spec[..end]) else {
        return Vec::new();
    };
    let Ok(commit) = repo.find_object(base.detach()).and_then(|o| o.peel_tags_to_end()) else {
        return Vec::new();
    };
    let mut cur = commit.id;
    let mut path = Vec::new();
    // One step reads `cur`'s parent list, so `cur` — not the commit stepped to —
    // is what ends up parsed.
    let mut step = |cur: &mut ObjectId, nth: usize, path: &mut Vec<ObjectId>| -> bool {
        let parents: Vec<ObjectId> =
            match repo.find_object(*cur).ok().and_then(|o| o.try_into_commit().ok()) {
                Some(c) => c.parent_ids().map(|p| p.detach()).collect(),
                None => return false,
            };
        path.push(*cur);
        match parents.get(nth.saturating_sub(1)) {
            Some(p) => {
                *cur = *p;
                true
            }
            None => false,
        }
    };
    for (op, n) in ops {
        if op == b'~' {
            for _ in 0..n {
                if !step(&mut cur, 1, &mut path) {
                    return path;
                }
            }
        } else if n > 0 && !step(&mut cur, n, &mut path) {
            // `<rev>^0` peels without reading a parent, so it parses nothing.
            return path;
        }
    }
    path
}

/// git's UNINTERESTING set as it stands under `--no-walk`.
///
/// `prepare_revision_walk()` returns before `limit_list()` when `revs->no_walk`
/// survived, so nothing paints the flag over the history: the only commits
/// carrying it are the negative endpoints themselves, their direct parents, and
/// whatever `mark_parents_uninteresting()` could reach onward through commits
/// the command line had already parsed (see [`navigation_path`]). A caller that
/// hands the full [`ancestor_closure`] to a `--no-walk` list drops commits git
/// still prints — `git log --no-walk main..side` keeps `side` even when `side`
/// is an ancestor of `main`.
pub(super) fn no_walk_uninteresting(
    repo: &gix::Repository,
    negatives: &[ObjectId],
    parsed: &HashSet<ObjectId>,
) -> HashSet<ObjectId> {
    let mut set: HashSet<ObjectId> = negatives.iter().copied().collect();
    // The negatives were parsed by `handle_commit()` itself, so their own parent
    // lists are always read; everything below that needs `parsed`.
    let mut stack: Vec<ObjectId> = negatives.to_vec();
    while let Some(id) = stack.pop() {
        let Some(commit) = repo.find_object(id).ok().and_then(|o| o.try_into_commit().ok()) else {
            continue;
        };
        for p in commit.parent_ids() {
            let pid = p.detach();
            if set.insert(pid) && parsed.contains(&pid) {
                stack.push(pid);
            }
        }
    }
    set
}

/// Everything a set of negative endpoints covers — the roots and every ancestor —
/// gathered by a plain ancestor DFS.
///
/// This is git's `UNINTERESTING` after `mark_parents_uninteresting()` has run over
/// the whole set, and it is what a caller needs when the flag has to be read off an
/// *object* rather than followed during a walk: `log` pre-seeds its `seen` set with
/// it, and `bundle` decides which pending refs still get written from it. A caller
/// that only walks does not need this — `gix-traverse` paints the same commits itself.
pub(super) fn ancestor_closure(repo: &gix::Repository, roots: &[ObjectId]) -> Result<HashSet<ObjectId>> {
    let mut set: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    for &r in roots {
        if set.insert(r) {
            stack.push(r);
        }
    }
    while let Some(id) = stack.pop() {
        let Ok(obj) = repo.find_object(id) else { continue };
        let Ok(commit) = obj.try_into_commit() else { continue };
        for p in commit.parent_ids() {
            let pid = p.detach();
            if set.insert(pid) {
                stack.push(pid);
            }
        }
    }
    Ok(set)
}

fn read_node(repo: &gix::Repository, id: ObjectId) -> Result<Node> {
    let commit = repo.find_object(id)?.try_into_commit()?;
    Ok(Node {
        id,
        parents: commit.parent_ids().map(|p| p.detach()).collect(),
        time: commit.time()?.seconds,
        seq: 0,
        boundary: false,
        follow_path: None,
        source: String::new(),
        graph_width: 0,
        reflog: None,
    })
}

/// The walk needs exactly three things per commit — id, parents, commit time —
/// and all three live in the **commit-graph** when the repository has one, which
/// is why git can walk a 6000-commit history without touching the object
/// database. `read_node` decodes a full commit object (zlib inflate, header
/// parse) for the same three fields.
///
/// This reader prefers the graph and falls back to the object for any commit the
/// graph does not carry (a graph written before the newest commits, or none at
/// all), so the walk is always correct and merely faster when the graph is
/// current.
struct NodeReader {
    graph: Option<gix::commitgraph::Graph>,
}

impl NodeReader {
    fn new(repo: &gix::Repository) -> Self {
        NodeReader { graph: repo.commit_graph().ok() }
    }

    fn read(&self, repo: &gix::Repository, id: ObjectId) -> Result<Node> {
        if let Some(graph) = &self.graph {
            if let Some(commit) = graph.commit_by_id(id) {
                let parents: Vec<ObjectId> = commit
                    .iter_parents()
                    .filter_map(|p| p.ok())
                    .map(|pos| graph.commit_at(pos).id().to_owned())
                    .collect();
                return Ok(Node {
                    id,
                    parents,
                    time: commit.committer_timestamp() as i64,
                    source: String::new(),
                    seq: 0,
                    boundary: false,
                    follow_path: None,
                    graph_width: 0,
                    reflog: None,
                });
            }
        }
        read_node(repo, id)
    }
}

/// git's `commit_list_insert_by_date`: keep the list newest-first, and place a
/// commit *after* every commit with the same date so equal timestamps come out
/// in insertion order — the tie-break git's priority queue also uses.
#[allow(dead_code)] // faithful port of git's commit_list_insert_by_date; kept for the walk.
fn insert_by_date(list: &mut Vec<Node>, node: Node) {
    let pos = list
        .iter()
        .position(|e| e.time < node.time)
        .unwrap_or(list.len());
    list.insert(pos, node);
}

/// Breadth-first walk over the reachable history, newest commit first. With
/// `first_parent`, only the first parent of each commit is followed — git's
/// `--first-parent`.
pub(super) fn walk(
    repo: &gix::Repository,
    tips: &[ObjectId],
    tip_sources: &[String],
    first_parent: bool,
    hidden: &HashSet<ObjectId>,
    budget: Option<usize>,
    no_walk: Option<NoWalk>,
) -> Result<Vec<Node>> {
    // Shallow commits (from `.git/shallow`, as a `--depth` clone leaves) are grafted
    // to have no parents: the walk must stop at them, not try to read their absent
    // parent objects (which is git's `is_repository_shallow` / grafting behaviour).
    let shallow: HashSet<ObjectId> = repo
        .shallow_commits()
        .ok()
        .flatten()
        .map(|c| c.iter().copied().collect())
        .unwrap_or_default();

    // Pre-seeding `seen` with the hidden (uninteresting) commits means any tip or
    // parent reachable from a negative range endpoint is never emitted or traversed
    // — git's `..` exclusion, implemented as a boundary the walk cannot cross.
    let reader = NodeReader::new(repo);
    let mut seen: HashSet<ObjectId> = hidden.clone();
    // A binary heap, not a date-sorted Vec: the frontier is popped newest-first,
    // and both the push and the pop are logarithmic. The previous sorted-insert
    // plus `remove(0)` made a full walk quadratic in the number of commits.
    let mut pending: std::collections::BinaryHeap<Node> = std::collections::BinaryHeap::new();
    // Insertion order, which is what decides a date tie — see `Node`'s `Ord`.
    // Tips enter in argument order and parents in parent order, exactly as
    // `add_parents_to_list()` feeds git's frontier.
    let mut seq: u64 = 0;
    for (idx, tip) in tips.iter().enumerate() {
        if seen.insert(*tip) {
            let mut node = reader.read(repo, *tip)?;
            // `--source` names each tip; without it `tip_sources` is empty and the
            // source stays blank (never rendered). Parents inherit below.
            if let Some(src) = tip_sources.get(idx) {
                node.source = src.clone();
            }
            node.seq = seq;
            seq += 1;
            pending.push(node);
        }
    }

    // `--no-walk`: git sets `revs->no_walk` and `get_revision()` returns the pending
    // objects themselves without ever calling `add_parents_to_list()`. `sorted` (the
    // default) hands them back newest-first, which is the heap's own order;
    // `unsorted` keeps the order they were named in.
    if let Some(mode) = no_walk {
        let mut out: Vec<Node> = std::iter::from_fn(|| pending.pop()).collect();
        if mode == NoWalk::Unsorted {
            out.sort_by_key(|n| n.seq);
        }
        return Ok(out);
    }

    let mut out: Vec<Node> = Vec::new();
    while let Some(node) = pending.pop() {
        // `budget` is `skip + max-count` when the caller has established that
        // nothing downstream can drop a commit (no pathspec, no parent/date/grep
        // filter, default order). Stopping there turns `log -n 100` on a
        // 6000-commit history from a full-history read into 100 object reads.
        if budget.is_some_and(|b| out.len() >= b) {
            break;
        }
        let parents: &[ObjectId] = if shallow.contains(&node.id) {
            &[] // grafted: a shallow commit's parents are outside the clone
        } else if first_parent {
            &node.parents[..node.parents.len().min(1)]
        } else {
            &node.parents
        };
        for parent in parents {
            if seen.insert(*parent) {
                let mut pnode = reader.read(repo, *parent)?;
                // git's `add_parents_to_list`: a parent inherits the source of the
                // commit that first reaches it (an empty-string clone when off).
                pnode.source = node.source.clone();
                pnode.seq = seq;
                seq += 1;
                pending.push(pnode);
            }
        }
        out.push(node);
    }
    Ok(out)
}

/// git's `sort_in_topological_order`: an indegree count over the already-walked
/// set, drained through a queue that is date-ordered for `--date-order` and a
/// LIFO stack for `--topo-order`.
pub(crate) fn topo_sort(nodes: Vec<Node>, by_date: bool) -> Vec<Node> {
    let keys = by_date.then(|| nodes.iter().map(|n| (n.id, n.time)).collect());
    topo_sort_keyed(nodes, keys.as_ref())
}

/// [`topo_sort`] driven by an [`Order`] rather than a bool, which is what
/// `sort_in_topological_order(&revs->commits, revs->sort_order)` takes.
///
/// ```c
/// switch (sort_order) {
/// default: /* REV_SORT_IN_GRAPH_ORDER */ queue.compare = NULL; break;
/// case REV_SORT_BY_COMMIT_DATE: queue.compare = compare_commits_by_commit_date; break;
/// case REV_SORT_BY_AUTHOR_DATE:
///         init_author_date_slab(&author_date);
///         queue.compare = compare_commits_by_author_date;
///         queue.cb_data = &author_date;
///         break;
/// }
/// …
/// for (next = orig; next; next = next->next) {
///         …
///         if (sort_order == REV_SORT_BY_AUTHOR_DATE)
///                 record_author_date(&author_date, commit);
/// }
/// ```
///
/// (`commit.c:961-982`.) `record_author_date()` reads the `author` header of each
/// listed commit up front and stores its timestamp in a slab; a commit with no
/// author line, or one whose date does not parse, keeps the slab's zero — so it
/// sorts as the epoch rather than being dropped.
pub(crate) fn topo_sort_ordered(
    repo: &gix::Repository,
    nodes: Vec<Node>,
    order: Order,
) -> Vec<Node> {
    let keys: Option<std::collections::HashMap<ObjectId, i64>> = match order {
        Order::Default | Order::Topo => None,
        Order::Date => Some(nodes.iter().map(|n| (n.id, n.time)).collect()),
        Order::AuthorDate => Some(
            nodes
                .iter()
                .map(|n| (n.id, author_date_of(repo, n.id).unwrap_or(0)))
                .collect(),
        ),
    };
    topo_sort_keyed(nodes, keys.as_ref())
}

/// `record_author_date()` (`commit.c:866-891`): the `author` header's timestamp, or
/// `None` when the commit has no author line or its date is malformed.
fn author_date_of(repo: &gix::Repository, id: ObjectId) -> Option<i64> {
    let commit = repo.find_object(id).ok()?.try_into_commit().ok()?;
    Some(commit.author().ok()?.time().ok()?.seconds)
}

/// The shared drain: `keys` is the priority the frontier is ordered by (highest
/// first, earliest-queued breaking ties), or `None` for the LIFO stack that keeps
/// `--topo-order`'s graph order.
fn topo_sort_keyed(
    nodes: Vec<Node>,
    keys: Option<&std::collections::HashMap<ObjectId, i64>>,
) -> Vec<Node> {
    let by_date = keys.is_some();
    let key_of = |id: &ObjectId| keys.and_then(|k| k.get(id)).copied().unwrap_or(0);
    let mut indegree: std::collections::HashMap<ObjectId, usize> =
        nodes.iter().map(|n| (n.id, 1usize)).collect();
    for node in &nodes {
        for parent in &node.parents {
            if let Some(d) = indegree.get_mut(parent) {
                *d += 1;
            }
        }
    }

    let index: std::collections::HashMap<ObjectId, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();

    // Tips are queued in list order. A LIFO stack is reversed first so that
    // popping still yields them in that order, exactly as git does.
    let mut queue: Vec<usize> = (0..nodes.len())
        .filter(|&i| indegree.get(&nodes[i].id) == Some(&1))
        .collect();
    if !by_date {
        queue.reverse();
    }

    let mut out: Vec<usize> = Vec::with_capacity(nodes.len());
    while !queue.is_empty() {
        let at = if by_date {
            // Highest key wins; the earliest-queued entry breaks ties.
            let mut best = 0usize;
            for (k, &i) in queue.iter().enumerate() {
                if key_of(&nodes[i].id) > key_of(&nodes[queue[best]].id) {
                    best = k;
                }
            }
            best
        } else {
            queue.len() - 1
        };
        let i = queue.remove(at);

        for parent in &nodes[i].parents {
            if let Some(d) = indegree.get_mut(parent) {
                if *d == 0 {
                    continue;
                }
                *d -= 1;
                if *d == 1 {
                    if let Some(&pi) = index.get(parent) {
                        queue.push(pi);
                    }
                }
            }
        }
        out.push(i);
    }

    // Anything the drain could not reach keeps its original relative position.
    let mut placed: Vec<bool> = vec![false; nodes.len()];
    for &i in &out {
        placed[i] = true;
    }
    for (i, &is_placed) in placed.iter().enumerate() {
        if !is_placed {
            out.push(i);
        }
    }

    let mut slots: Vec<Option<Node>> = nodes.into_iter().map(Some).collect();
    out.into_iter()
        .filter_map(|i| slots[i].take())
        .collect()
}

// ---------------------------------------------------------------------------
// Pretty formats
// ---------------------------------------------------------------------------

pub(crate) enum Pretty {
    /// git's default: `commit`/`Merge`/`Author`/`Date` and an indented message.
    Medium,
    /// `medium` without the `Date` line, and only the subject.
    Short,
    /// `commit`/`Merge`/`Author`/`Commit` and the full indented message.
    Full,
    /// `full` plus `AuthorDate`/`CommitDate` lines.
    Fuller,
    /// The raw object header: `tree`/`parent`/`author`/`committer`.
    Raw,
    /// `<abbrev> (<subject>, <short-date>)` on one line.
    Reference,
    /// `<hash> <subject>` on one line.
    Oneline,
    /// `CMIT_FMT_EMAIL`: the mail message `format-patch` sends, without any of
    /// the options that decorate it.
    Email,
    /// `CMIT_FMT_MBOXRD`: [`Pretty::Email`] whose body escapes `/^>*From /` with
    /// one more `>`, so a reader splitting an mbox on `From ` cannot mistake a
    /// body line for a message separator. `pretty.c` models it as its own format
    /// because the escaping lives in `pp_remainder()`.
    MboxRd,
    /// A `--format=`/`format:` string with `%` placeholders.
    User(String),
}

/// The two halves of `pp_title_line()` that differ between the commands sharing
/// [`Pretty::Email`], both of which come from `pretty_print_context` fields that
/// `log-tree.c`'s `show_log()` fills in and `builtin/rev-list.c`'s zeroed context
/// leaves at their `0` defaults.
///
/// ```c
/// if (pp->print_email_subject) {
///         if (pp->rev)
///                 fmt_output_email_subject(sb, pp->rev);
///         if (pp->encode_email_headers &&
///             needs_rfc2047_encoding(title.buf, title.len))
///                 add_rfc2047(sb, title.buf, title.len, encoding, RFC2047_SUBJECT);
///         else
///                 strbuf_add_wrapped_bytes(sb, title.buf, title.len,
///                                  -last_line_length(sb), 1, max_length);
/// }
/// ```
///
/// (pretty.c:1968-1977.) So `git log --pretty=email` gets `pp->rev`, whose
/// `subject_prefix` `repo_init_revisions()` seeded with `PATCH`, and gets the
/// RFC2047 encoding; `git rev-list --pretty=email` gets neither, which is why it
/// prints a bare `Subject:` and a raw UTF-8 `From:`.
#[derive(Clone, Copy)]
pub(crate) struct EmailStyle<'a> {
    /// `opt->subject_prefix` through `fmt_output_email_subject()`. Empty is
    /// `rev-list`, which never reaches that function at all — the two spell the
    /// same `Subject: ` and are kept as one case for that reason.
    ///
    /// `format.subjectPrefix` is read by `git_log_config()` itself
    /// (builtin/log.c:560-561) into the `fmt_patch_subject_prefix` that
    /// `init_log_defaults()` copies into `rev->subject_prefix`, so `git log
    /// --pretty=email` honours it; `--subject-prefix` is `format-patch`'s alone.
    pub(crate) subject_prefix: &'a str,
    /// `pp->encode_email_headers`. `init_log_defaults()` seeds it from
    /// `default_encode_email_headers`, which starts at 1 and which
    /// `git_log_config()` moves for `format.encodeEmailHeaders` (builtin/log.c:
    /// 50, 172, 566-569); `--[no-]encode-email-headers` is `setup_revisions()`'s
    /// own option (revision.c:2526-2529), so the last spelling wins.
    pub(crate) encode_headers: bool,
}

impl EmailStyle<'_> {
    /// `builtin/rev-list.c`'s `struct pretty_print_context ctx = {0}`: no `rev`,
    /// so `fmt_output_email_subject()` is never reached and neither the config
    /// nor the command-line switch behind these two fields is visible to it.
    pub(crate) const REV_LIST: EmailStyle<'static> =
        EmailStyle { subject_prefix: "", encode_headers: false };
}

/// The two keys `git_log_config()` reads for [`EmailStyle`], as
/// `(format.subjectPrefix, format.encodeEmailHeaders)`:
///
/// ```c
/// if (!strcmp(var, "format.subjectprefix"))
///         return git_config_string(&fmt_patch_subject_prefix, var, value);
/// …
/// if (!strcmp(var, "format.encodeemailheaders")) {
///         default_encode_email_headers = git_config_bool(var, value);
///         return 0;
/// }
/// ```
///
/// (builtin/log.c:560-561 and 566-569.) `git_log_config()` is `cmd_log`'s *and*
/// `cmd_show`'s, so both honour these even though the options that set them
/// belong to `format-patch`. `fmt_patch_subject_prefix` starts at `"PATCH"`,
/// which is the bracketed word an unconfigured repository prints; an empty value
/// drops the brackets entirely, as `fmt_output_email_subject()`'s
/// `*opt->subject_prefix` test does.
pub(super) fn email_config(repo: &gix::Repository) -> (String, bool) {
    let snap = repo.config_snapshot();
    (
        snap.string("format.subjectPrefix")
            .map_or_else(|| "PATCH".to_string(), |v| v.to_str_lossy().into_owned()),
        snap.boolean("format.encodeEmailHeaders").unwrap_or(true),
    )
}

/// `pretty_print_commit()` for `CMIT_FMT_EMAIL`/`CMIT_FMT_MBOXRD`, minus the
/// magic `From <oid> Mon Sep 17 00:00:00 2001` line — that one is
/// `log_write_email_headers()`'s (log-tree.c:440) and therefore belongs to the
/// caller that has a `struct rev_info`.
///
/// The trailing shape is `pretty_print_commit()`'s last four statements
/// (pretty.c:2206-2221):
///
/// ```c
/// beginning_of_body = sb->len;
/// if (pp->fmt != CMIT_FMT_ONELINE)
///         pp_remainder(pp, &msg, sb, indent);
/// strbuf_rtrim(sb);
/// if (pp->fmt != CMIT_FMT_ONELINE)
///         strbuf_addch(sb, '\n');
/// if (cmit_fmt_is_mail(pp->fmt) && sb->len <= beginning_of_body)
///         strbuf_addch(sb, '\n');
/// ```
///
/// The `strbuf_rtrim()` reaches back into the headers, so a commit with no body
/// loses the blank line the header block ended with and the last `if` puts one
/// back — which is why an empty-bodied record ends in exactly two newlines and a
/// full one in a single newline.
/// `pp_user_info()`'s mail branch for one identity (pretty.c:516-595): the
/// `From:` line, RFC2047-encoded when `encode` is on and the name needs it, and
/// the RFC2822 `Date:` line under it — a date `--date=` never reaches, because
/// the switch hard-codes `DATE_MODE(RFC2822)` for the mail formats.
///
/// Shared by `--pretty=email`'s author block and `git show`'s annotated-tag
/// header, which `show_tag_object()` puts through the same `pp_user_info()`.
pub(super) fn write_identity_headers_for(
    sb: &mut String,
    who: &gix::actor::SignatureRef<'_>,
    encode: bool,
) -> Result<()> {
    let date = who.time()?.format(gix::date::time::format::GIT_RFC2822)?;
    let name = who.name.to_str().map_err(|_| {
        anyhow!("identity name is not valid UTF-8; RFC2047 encoding needs a known charset")
    })?;
    let mail = who.email.to_str().map_err(|_| {
        anyhow!("identity email is not valid UTF-8; RFC2047 encoding needs a known charset")
    })?;
    super::format_patch::write_identity_headers(sb, name, mail, &date, encode);
    Ok(())
}

pub(super) fn email_body(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
    style: EmailStyle,
) -> Result<()> {
    let raw = commit.message_raw()?;
    let author = commit.author()?;

    let mut sb = String::new();
    // `pp_header()` → `pp_user_info(pp, "Author", …)`, whose mail branch writes
    // `From:` and then the RFC2822 `Date:` (pretty.c:516-595). `add_merge_info()`
    // returns early for a mail format, so a merge has no `Merge:` line here.
    write_identity_headers_for(&mut sb, &author, style.encode_headers)?;

    let msg = super::format_patch::skip_blank_lines(raw);
    let (title, rest) = super::format_patch::format_subject(msg);
    let title = title
        .to_str()
        .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
        .to_owned();
    if style.subject_prefix.is_empty() {
        sb.push_str("Subject: ");
    } else {
        sb.push_str(&format!("Subject: [{}] ", style.subject_prefix));
    }
    if style.encode_headers && super::format_patch::needs_rfc2047_encoding(&title) {
        super::format_patch::add_rfc2047(&mut sb, &title, false);
    } else {
        let consumed = -super::format_patch::last_line_length(&sb);
        super::format_patch::wrap_text(
            &mut sb,
            &title,
            consumed,
            1,
            super::format_patch::HEADER_MAX_LENGTH,
        );
    }
    sb.push('\n');

    // `pretty_print_commit()`'s `need_8bit_cte` scan (pretty.c:2175-2192) looks
    // only at the *body*: the author line may be non-ASCII while the log is not.
    // It runs on the reencoded message, which — with no `encoding` header and
    // `i18n.commitEncoding` unset — is the raw one.
    let body_is_8bit = {
        let after_headers = raw;
        after_headers.iter().any(|&b| b >= 0x80)
    };
    if body_is_8bit {
        sb.push_str("MIME-Version: 1.0\n");
        sb.push_str("Content-Type: text/plain; charset=UTF-8\n");
        sb.push_str("Content-Transfer-Encoding: 8bit\n");
    }
    // `if (cmit_fmt_is_mail(pp->fmt)) strbuf_addch(sb, '\n');` (pretty.c:2003-2005).
    sb.push('\n');

    out.extend_from_slice(sb.as_bytes());
    let beginning_of_body = out.len();
    let mut body: Vec<u8> = Vec::new();
    super::format_patch::pp_remainder(rest, &mut body);
    if matches!(pretty, Pretty::MboxRd) {
        let mut escaped = Vec::with_capacity(body.len());
        for line in body.split_inclusive(|&b| b == b'\n') {
            if super::format_patch::is_mboxrd_from(line) {
                escaped.push(b'>');
            }
            escaped.extend_from_slice(line);
        }
        body = escaped;
    }
    out.extend_from_slice(&body);
    while out.last().is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
        out.pop();
    }
    out.push(b'\n');
    if out.len() <= beginning_of_body {
        out.push(b'\n');
    }
    Ok(())
}

/// git's `get_commit_format` (pretty.c:190-222), the shared parser behind
/// `--pretty=` and `--format=`. Returns the format and whether it terminates
/// (rather than separates) records:
///   * `Ok(Some(..))` — a valid, supported format.
///   * `Ok(None)`     — a value git itself rejects (`fatal: invalid --pretty
///     format: <arg>`, exit 128): non-empty, no `%`, not a `format:`/`tformat:`
///     prefix, and naming no entry in the format table.
///   * `Err(..)`      — either a value git accepts but this port does not yet
///     render (an unsupported `%` placeholder), surfaced terse rather than faked,
///     or the [`Fatal`](crate::fatal::Fatal) git dies with when a `pretty.<name>`
///     alias chain loops back on itself.
///
/// ```c
/// rev->use_terminator = 0;
/// if (!arg) { rev->commit_format = CMIT_FMT_DEFAULT; return; }
/// if (skip_prefix(arg, "format:", &arg)) { save_user_format(rev, arg, 0); return; }
/// if (!*arg || skip_prefix(arg, "tformat:", &arg) || strchr(arg, '%')) {
///         save_user_format(rev, arg, 1);
///         return;
/// }
/// commit_format = find_commit_format(arg);
/// if (!commit_format)
///         die("invalid --pretty format: %s", arg);
/// ```
///
/// The three shortcuts are tried in that order and never consult config; only a
/// value that survives them reaches [`super::pretty_formats::resolve`], which is
/// where the built-in table and the `pretty.<name>` keys both live. `repo` is
/// git's `the_repository`, whose config the table is built from; `None` lets the
/// resolver discover it, for the option parsers that run before their command has
/// opened one.
///
/// An empty value is git's empty user format: it renders nothing per commit and,
/// as a terminator format, drops even the trailing newline.
pub(crate) fn get_commit_format(
    repo: Option<&gix::Repository>,
    spec: &str,
) -> Result<Option<(Pretty, bool)>> {
    // `skip_prefix(arg, "format:", &arg)` comes first, so a `pretty.<name>` can
    // never shadow the `format:` shortcut.
    if let Some(fmt) = spec.strip_prefix("format:") {
        check_format(fmt)?;
        return Ok(Some((Pretty::User(fmt.to_string()), false)));
    }
    if spec.is_empty() {
        return Ok(Some((Pretty::User(String::new()), true)));
    }
    if let Some(fmt) = spec.strip_prefix("tformat:") {
        check_format(fmt)?;
        return Ok(Some((Pretty::User(fmt.to_string()), true)));
    }
    if spec.contains('%') {
        check_format(spec)?;
        return Ok(Some((Pretty::User(spec.to_string()), true)));
    }
    match super::pretty_formats::resolve(repo, spec)? {
        None => Ok(None),
        Some(super::pretty_formats::Resolved::Builtin(b)) => Ok(Some(builtin_pretty(b))),
        // `if (commit_format->format == CMIT_FMT_USERFORMAT) save_user_format(…)`
        // (pretty.c:218-221): a `pretty.<name>` entry renders as its stored format
        // string, with the entry's own terminator/separator answer.
        Some(super::pretty_formats::Resolved::User { format, is_tformat }) => {
            check_format(&format)?;
            Ok(Some((Pretty::User(format), is_tformat)))
        }
    }
}

/// `rev->commit_format = commit_format->format; rev->use_terminator =
/// commit_format->is_tformat` (pretty.c:213-214) for the nine `builtin_formats[]`
/// entries, in this port's own `Pretty` spelling.
pub(crate) fn builtin_pretty(b: super::pretty_formats::Builtin) -> (Pretty, bool) {
    use super::pretty_formats::Builtin;
    match b {
        Builtin::Oneline => (Pretty::Oneline, true),
        Builtin::Medium => (Pretty::Medium, false),
        Builtin::Short => (Pretty::Short, false),
        Builtin::Full => (Pretty::Full, false),
        Builtin::Fuller => (Pretty::Fuller, false),
        Builtin::Raw => (Pretty::Raw, false),
        Builtin::Reference => (Pretty::Reference, true),
        // `cmit_fmt_is_mail()`: the two mail formats terminate rather than
        // separate their records, since `pp_title_line()` already ended the
        // header block with the blank line a reader splits on.
        Builtin::Email => (Pretty::Email, false),
        Builtin::MboxRd => (Pretty::MboxRd, false),
    }
}

/// Reject any placeholder [`expand_format`] does not implement, so an unsupported
/// format fails loudly instead of expanding to something plausible but wrong.
///
/// `%C` is always accepted: like git, an unrecognized color word after it renders
/// literally rather than erroring, and its `(...)` argument is ordinary text the
/// outer scan skips. `%d`/`%D` are the ref decorations.
fn check_format(fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        match it.next() {
            Some(
                'H' | 'h' | 'T' | 't' | 'P' | 'p' | 's' | 'b' | 'B' | 'f' | 'n' | '%' | 'C' | 'd'
                | 'D' | 'N',
            ) => {}
            Some('a') => match it.next() {
                // `%aN`/`%aE` are the mailmap-resolved name and address, which
                // `format_person_part()` maps whether or not `--use-mailmap` is on.
                Some('n' | 'e' | 'N' | 'E' | 'd' | 'i' | 'I' | 't' | 'r') => {}
                Some(x) => anyhow::bail!("unsupported format placeholder %a{x}"),
                None => anyhow::bail!("unsupported trailing % in format"),
            },
            Some('c') => match it.next() {
                Some('n' | 'e' | 'N' | 'E' | 'd' | 'i' | 'I' | 't' | 'r') => {}
                Some(x) => anyhow::bail!("unsupported format placeholder %c{x}"),
                None => anyhow::bail!("unsupported trailing % in format"),
            },
            // Reflog placeholders, all empty outside a `--walk-reflogs` walk:
            // `%gd`/`%gD` are the short and full selector, `%gn`/`%gN`/`%ge`/`%gE`
            // the entry's identity, and `%gs` its message.
            Some('g') => match it.next() {
                Some('d' | 'D' | 'n' | 'N' | 'e' | 'E' | 's') => {}
                Some(x) => anyhow::bail!("unsupported format placeholder %g{x}"),
                None => anyhow::bail!("unsupported trailing % in format"),
            },
            // The `%G…` family (pretty.c:1659-1710), all seven of which share one
            // `check_commit_signature()` call: `%GG` the checker's own report, `%G?`
            // the status character, `%GS` the signer, `%GK` the key, `%GF` the
            // fingerprint, `%GP` the primary key's fingerprint and `%GT` the trust
            // level's name.
            //
            // `%GF` is deliberately absent: `sigc->fingerprint` is what it prints, and
            // the shared verifier leaves that field empty for an ssh signature (where
            // git sets it to the key, gpg-interface.c:445-447). Accepting it would
            // print an empty line where stock prints a fingerprint, so it stays
            // refused until the verifier fills the field.
            Some('G') => match it.next() {
                Some('G' | '?' | 'S' | 'K' | 'P' | 'T') => {}
                Some(x) => anyhow::bail!("unsupported format placeholder %G{x}"),
                None => anyhow::bail!("unsupported trailing % in format"),
            },
            // `%xNN` is always accepted: two hex digits emit that byte, and
            // anything else prints literally rather than failing, so there is
            // nothing here to reject.
            Some('x') => {}
            // `%(trailers[:<options>])`, whose option list is validated when it is
            // expanded — an unknown option prints literally there rather than
            // failing here, exactly as git does.
            Some('(') => {}
            // The column-control atoms — `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)`
            // and their `|`, `trunc`, `ltrunc` and `mtrunc` forms — and the `%w(…)`
            // wrap atom are validated where they are expanded: a malformed one is
            // not a placeholder at all, and git prints it literally rather than
            // failing (see [`pretty_pad`]).
            Some('<' | '>' | 'w') => {}
            Some(x) => anyhow::bail!("unsupported format placeholder %{x}"),
            None => anyhow::bail!("unsupported trailing % in format"),
        }
    }
    Ok(())
}

/// Render one commit through a bare user format string, uncolored, undecorated
/// and with the default date mode — git's `pretty_print_commit()` over a
/// `pretty_print_context` that carries nothing but the format.
///
/// This is the entry point `git rebase -i` needs: `sequencer_make_script()`
/// prints each instruction's oneline through `rebase.instructionFormat`, which
/// is an ordinary `--pretty=format:` string.
pub(crate) fn format_commit(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    fmt: &str,
) -> Result<Vec<u8>> {
    let abbrev = std::cell::RefCell::new(AbbrevCache::new(repo));
    let colors = super::color::DecorateColors::disabled();
    let ctx = RenderCtx {
        abbrev_commit: false,
        abbrev: &abbrev,
        // Neither caller is `git log`: `--show-signature` is a `rev_info` field and
        // these two render through `pretty_print_commit()` with a bare context.
        show_signature: false,
        date_mode: DateMode::Default,
        extra: Vec::new(),
        want_color: false,
        colors: &colors,
        now: now_secs(),
        decorations: None,
        decorate: DecorateStyle::Off,
        source: None,
        mailmap: None,
        identity_mailmap: None,
        // `rebase -i` renders its instruction lines with no notes; `%N` in an
        // instruction format expands to nothing, as it does under git.
        notes: &[],
        repo,
        mark: "",
        parents: &[],
        // No `--graph` behind either of these callers.
        graph_width: 0,
        expand_tabs: None,
        // Neither caller is a reflog walk, so every `%g…` expands to nothing.
        reflog: None,
        date_explicit: false,
        // Only a user format reaches this caller, and no `%` placeholder reads it.
        email: EmailStyle::REV_LIST,
    };
    let mut out = Vec::new();
    expand_format(&mut out, commit, &unabbreviated(fmt), &ctx)?;
    Ok(out)
}

/// The abbreviating placeholders rewritten to their full-length twins.
///
/// `pretty_print_context pp = {0}` leaves `pp.abbrev` at 0, and
/// `repo_find_unique_abbrev_r()` answers a request for length 0 with the full
/// hash — so `%h`, `%p` and `%t` render exactly what `%H`, `%P` and `%T` do
/// under a zeroed context. `git rebase -i` is one such caller, which is why a
/// `rebase.instructionFormat` of `%h %s` puts a *full* object id in the sheet.
///
/// `%%h` is an escaped percent followed by a literal `h` and is left alone.
fn unabbreviated(fmt: &str) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::with_capacity(fmt.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' || i + 1 >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        out.push('%');
        out.push(match chars[i + 1] {
            'h' => 'H',
            'p' => 'P',
            't' => 'T',
            other => other,
        });
        i += 2;
    }
    out
}

/// Expand the placeholders accepted by [`check_format`] for `commit`, using the
/// render knobs in `ctx` (`--date=`, color enablement, decorations, and the clock
/// for relative dates).
/// A `%(trailers...)` placeholder starting at `chars[i] == '('`: its option text
/// and the index just past the closing paren. `None` for anything else that opens
/// with `%(`, which is `%C(...)`'s territory or a malformed placeholder.
fn trailers_placeholder(chars: &[char], i: usize) -> Option<(String, usize)> {
    let close = chars[i..].iter().position(|&c| c == ')')? + i;
    let inner: String = chars[i + 1..close].iter().collect();
    let spec = inner
        .strip_prefix("trailers:")
        .or_else(|| (inner == "trailers").then_some(""))?;
    Some((spec.to_string(), close + 1))
}

/// git's `repo_format_commit_message()` driver loop: literal text is copied,
/// `%%` is expanded here (which is why it never consumes a pending padding
/// request), and every other `%` placeholder goes through
/// [`expand_one`] — directly, or through the padding machinery when a `%<`/`%>`
/// atom left a field pending.
fn expand_format(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    fmt: &str,
    ctx: &RenderCtx<'_>,
) -> Result<()> {
    // `%C(auto)` latches auto-coloring on for the placeholders that follow it —
    // notably `%d`/`%D`, which stay uncolored until it appears (matching git).
    let mut auto = false;
    // Signature evaluation (gpg/ssh) is lazy and computed at most once per commit,
    // shared between %G? and %GK.
    let mut gsig: Option<crate::gitsig::SigCheck> = None;
    let chars: Vec<char> = fmt.chars().collect();
    // The deferred state `struct format_commit_context` carries: a column field a
    // `%<`/`%>` atom is holding open, and the `%w()` wrap parameters.
    let mut pad = PadState::default();
    let mut wrap = WrapState::default();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        let Some(&p) = chars.get(i) else {
            // A `%` with nothing after it: `format_commit_one()` switches on the
            // NUL and returns 0, so the driver prints the `%`. [`check_format`]
            // rejects a format that simply ends in one, so the only way here is a
            // `%C…` chain having realigned onto the second `%` of a trailing `%%`
            // — which has already laid out the field it was holding open.
            out.push(b'%');
            break;
        };
        // `strbuf_expand_step()` handles `%%` before `format_commit_item()` is
        // reached, so it is neither padded nor does it spend a pending field.
        if p == '%' {
            out.push(b'%');
            i += 1;
            continue;
        }
        i += 1;
        if pad.flush == FlushType::None {
            if !expand_one(out, commit, &chars, &mut i, p, ctx, &mut auto, &mut gsig, &mut pad, &mut wrap)? {
                // `format_commit_item()` answered 0: git prints the `%` and
                // rescans from the placeholder character itself.
                out.push(b'%');
                i -= 1;
            }
            continue;
        }
        // `format_and_pad_commit()`: the placeholder renders into a buffer of its
        // own so its *display* width can be measured, and a `%C…` color keeps
        // pulling the following placeholder into the same field — the escape adds
        // bytes but no columns, so the field measures the text.
        let padding = pad.padding;
        let mut local: Vec<u8> = Vec::new();
        let mut p = p;
        // Whether the chain has already swallowed a `%`. `format_and_pad_commit()`
        // counts it in `total_consumed`, so a *later* placeholder that expands to
        // nothing still leaves the driver with a non-zero count — and the driver
        // only prints a bare `%` when the count is zero.
        let mut chained = false;
        let consumed = loop {
            let modifier = p == 'C';
            let consumed =
                expand_one(&mut local, commit, &chars, &mut i, p, ctx, &mut auto, &mut gsig, &mut pad, &mut wrap)?;
            if !modifier || !consumed {
                break consumed;
            }
            if chars.get(i) != Some(&'%') {
                break consumed;
            }
            i += 1;
            match chars.get(i) {
                Some(&next) => {
                    chained = true;
                    p = next;
                    i += 1;
                }
                None => break consumed,
            }
        };
        pad.apply(out, local, padding, ctx.graph_width);
        if !consumed {
            i -= 1;
            if !chained {
                out.push(b'%');
            }
        }
    }
    // `repo_format_commit_message()` closes with a rewrap to width 0, which wraps
    // whatever a trailing `%w()` was still governing.
    wrap.rewrap_message_tail(out, 0, 0, 0);
    Ok(())
}

/// `format_commit_one()`: expand the single placeholder `p`, whose following
/// character is at `chars[*i]`, advancing `*i` past whatever it consumes.
///
/// `false` is git's "consumed 0 bytes" — the placeholder is not one git knows how
/// to expand, nothing was written, and the caller prints the `%` literally.
#[allow(clippy::too_many_arguments)]
fn expand_one(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    chars: &[char],
    i: &mut usize,
    p: char,
    ctx: &RenderCtx<'_>,
    auto: &mut bool,
    gsig: &mut Option<crate::gitsig::SigCheck>,
    pad: &mut PadState,
    wrap: &mut WrapState,
) -> Result<bool> {
    let date_mode = ctx.date_mode;
    // `%(trailers[:<options>])` — the one parenthesised placeholder that is not
    // a colour request. `format_trailers_from_commit()` renders the message's
    // trailer block; an unparsable option list makes git print the placeholder
    // literally rather than fail.
    if p == '(' {
        let Some((spec, next)) = trailers_placeholder(chars, *i - 1) else {
            return Ok(false);
        };
        let Some(opts) = super::interpret_trailers::PrettyOpts::parse(spec.as_bytes()) else {
            return Ok(false);
        };
        out.extend_from_slice(&super::interpret_trailers::format_pretty(
            commit.message_raw()?,
            &opts,
        ));
        *i = next;
        return Ok(true);
    }
    // The column atoms `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)` and their
    // `|`/`trunc`/`ltrunc`/`mtrunc` forms: they expand to nothing and leave the
    // field pending for the next placeholder.
    if p == '<' || p == '>' {
        return Ok(match pad.parse(chars, *i - 1) {
            Some(consumed) => {
                *i = *i - 1 + consumed;
                true
            }
            None => false,
        });
    }
    // `%w(<width>,<indent1>,<indent2>)`: everything emitted after it is
    // re-wrapped to that width when the parameters next change.
    if p == 'w' {
        return Ok(match wrap.parse_and_apply(out, chars, *i - 1) {
            Some(consumed) => {
                *i = *i - 1 + consumed;
                true
            }
            None => false,
        });
    }
    match p {
        // Under `%C(auto)`, git paints the commit hash with `color.diff.commit`.
        'H' => push_maybe_auto(out, &commit.id().to_string(), auto_commit_color(ctx, *auto)),
        'h' => push_maybe_auto(
            out,
            &ctx.abbrev.borrow_mut().get(commit.id()),
            auto_commit_color(ctx, *auto),
        ),
        'T' => out.extend_from_slice(commit.tree_id()?.to_string().as_bytes()),
't' => {
            out.extend_from_slice(ctx.abbrev.borrow_mut().get(commit.tree_id()?).as_bytes());
        }
        'P' => write_parents(out, false, ctx.abbrev, ctx.parents, ctx.repo),
        'p' => write_parents(out, true, ctx.abbrev, ctx.parents, ctx.repo),
        's' => out.extend_from_slice(&subject(commit.message_raw()?)),
        'b' => out.extend_from_slice(&body(commit.message_raw()?)),
        'B' => out.extend_from_slice(commit.message_raw()?),
        // `%N`: the raw note text — no header, no indent — which is the only
        // way a user format shows notes at all.
        //
        // ```c
        // case 'N':
        //         if (c->pretty_ctx->notes_message) {
        //                 strbuf_addstr(sb, c->pretty_ctx->notes_message);
        //                 return 1;
        //         }
        //         return 0;
        // ```
        //
        // (pretty.c:1650-1655.) `show_log()` fills `notes_message` only under
        // `opt->show_notes`, so with notes off the placeholder consumes nothing
        // and `%N` prints literally — it is not the same as a commit that simply
        // has no note, which yields an empty (but present) message.
        'N' => {
            if ctx.notes.is_empty() {
                return Ok(false);
            }
            out.extend_from_slice(&super::notes::format_display(
                ctx.repo,
                ctx.notes,
                commit.id().detach(),
                true,
            )?);
        }
        'f' => out.extend_from_slice(&sanitized_subject(&subject(commit.message_raw()?))),
        'n' => out.push(b'\n'),
        // `%xNN`: the byte with that hex code, which is how a format asks for
        // a literal tab, NUL or any byte the shell would eat. Two hex digits
        // are required; `strbuf_expand_literal()` answers 0 for anything else,
        // and git then prints the text as typed.
        'x' => match hex_byte(chars.get(*i).copied(), chars.get(*i + 1).copied()) {
            Some(byte) => {
                out.push(byte);
                *i += 2;
            }
            None => return Ok(false),
        },
        'C' => {
            if !expand_color(out, chars, i, ctx.want_color, auto) {
                return Ok(false);
            }
        }
        // `%d`/`%D` are always shown (short by default); `log.decorate=full`
        // / `--decorate=full` switches them to full ref names.
        'd' => expand_decoration(out, commit, ctx, *auto, true, ctx.decorate == DecorateStyle::Full),
        'D' => expand_decoration(out, commit, ctx, *auto, false, ctx.decorate == DecorateStyle::Full),
        'a' => {
            let author = commit.author()?;
            match chars.get(*i).copied() {
                Some('n') => out.extend_from_slice(author.name),
                Some('e') => out.extend_from_slice(author.email),
                Some('N') => out.extend_from_slice(mapped_name(&author, ctx.identity_mailmap)),
                Some('E') => out.extend_from_slice(mapped_email(&author, ctx.identity_mailmap)),
                Some('d') => expand_date(out, &author, date_mode, ctx.now)?,
                Some('i') => expand_date(out, &author, DateMode::Iso, ctx.now)?,
                Some('I') => expand_date(out, &author, DateMode::IsoStrict, ctx.now)?,
                Some('r') => expand_date(out, &author, DateMode::Relative, ctx.now)?,
                Some('t') => write!(out, "{}", author.time()?.seconds)?,
                _ => unreachable!("check_format rejected this already"),
            }
            *i += 1;
        }
        'c' => {
            let committer = commit.committer()?;
            match chars.get(*i).copied() {
                Some('n') => out.extend_from_slice(committer.name),
                Some('e') => out.extend_from_slice(committer.email),
                Some('N') => out.extend_from_slice(mapped_name(&committer, ctx.identity_mailmap)),
                Some('E') => out.extend_from_slice(mapped_email(&committer, ctx.identity_mailmap)),
                Some('d') => expand_date(out, &committer, date_mode, ctx.now)?,
                Some('i') => expand_date(out, &committer, DateMode::Iso, ctx.now)?,
                Some('I') => expand_date(out, &committer, DateMode::IsoStrict, ctx.now)?,
                Some('r') => expand_date(out, &committer, DateMode::Relative, ctx.now)?,
                Some('t') => write!(out, "{}", committer.time()?.seconds)?,
                _ => unreachable!("check_format rejected this already"),
            }
            *i += 1;
        }
        // The reflog placeholders. `format_reflog_person()` and the selector both
        // read `pretty_ctx->reflog_info`, which is only set under `--walk-reflogs`;
        // without one every `%g…` expands to nothing at all.
        'g' => {
            let which = chars.get(*i).copied();
            if let Some(rl) = ctx.reflog {
                match which {
                    Some('d') => write_reflog_selector(out, rl, ctx, true),
                    Some('D') => write_reflog_selector(out, rl, ctx, false),
                    Some('n') => out.extend_from_slice(&rl.who_name),
                    Some('e') => out.extend_from_slice(&rl.who_email),
                    Some('N') => out.extend_from_slice(
                        ctx.identity_mailmap
                            .and_then(|m| m.lookup(&rl.who_name, &rl.who_email))
                            .and_then(|info| info.name.as_deref())
                            .unwrap_or(&rl.who_name),
                    ),
                    Some('E') => out.extend_from_slice(
                        ctx.identity_mailmap
                            .and_then(|m| m.lookup(&rl.who_name, &rl.who_email))
                            .and_then(|info| info.email.as_deref())
                            .unwrap_or(&rl.who_email),
                    ),
                    Some('s') => out.extend_from_slice(&rl.message),
                    _ => unreachable!("check_format rejected this already"),
                }
            }
            *i += 1;
        }
        'G' => {
            // `if (!c->signature_check.result) check_commit_signature(...)` — one
            // verification per commit, shared by all seven placeholders.
            let sig = match gsig {
                Some(sig) => sig,
                None => gsig.insert(
                    checked_signature(ctx.repo, &commit.data)?
                        .map(|(check, _)| check)
                        .unwrap_or_default(),
                ),
            };
            match chars.get(*i).copied() {
                // `sigc->output`, the checker's own human-readable report.
                Some('G') => out.extend_from_slice(&sig.output),
                // `sigc->result` with the `TRUST_UNDEFINED`/`TRUST_NEVER` fold to `U`,
                // which is what `pretty_status()` carries. A result the switch does not
                // name adds nothing at all — `NoSignature` is `'N'`, which it does name.
                Some('?') => out.push(sig.pretty_status().code() as u8),
                Some('S') => out.extend_from_slice(sig.signer.as_bytes()),
                Some('K') => out.extend_from_slice(sig.key.as_bytes()),
                Some('P') => out.extend_from_slice(sig.primary_key_fingerprint.as_bytes()),
                // `gpg_trust_level_to_str()` (gpg-interface.c:963) over
                // `sigcheck_gpg_trust_level[]`'s `display_key` column — printed even
                // for an unsigned commit, whose level is the `TRUST_UNDEFINED` that
                // `check_signature()` starts from.
                Some('T') => out.extend_from_slice(trust_display(sig.trust).as_bytes()),
                _ => unreachable!("check_format rejected this already"),
            }
            *i += 1;
        }
        // A `%` reaches `format_commit_one()` when `format_and_pad_commit()`'s
        // `%C…` chain swallows the `%` of a `%%` (pretty.c:1828-1831) and hands
        // it the second one. git falls off the end of the switch and returns 0
        // (pretty.c:1799), which breaks the chain without printing a bare `%`,
        // and the driver rescans from this `%`.
        '%' => return Ok(false),
        // That rescan realigns the format by one byte, so the placeholders after
        // it are not the ones [`check_format`] validated — `%<(20)%Cred%%|` walks
        // in here with `%|`, which the pair reading made literal text. Report it
        // the way `check_format` would rather than treat an unvalidated character
        // as impossible.
        _ => anyhow::bail!("unsupported format placeholder %{p}"),
    }
    Ok(true)
}

/// `gpg_trust_level_to_str()` (gpg-interface.c:963): the `display_key` column of
/// `sigcheck_gpg_trust_level[]` (gpg-interface.c:204-210), which is what `%GT`
/// prints. Lower-case, unlike the `TRUST_*` status-line keys the same table parses.
fn trust_display(level: crate::gitsig::Trust) -> &'static str {
    use crate::gitsig::Trust;
    match level {
        Trust::Undefined => "undefined",
        Trust::Never => "never",
        Trust::Marginal => "marginal",
        Trust::Fully => "fully",
        Trust::Ultimate => "ultimate",
    }
}

/// Expand a `%C…` color placeholder starting just past the `C` (index `i` points
/// at the first following char). Advances `i` over whatever the placeholder
/// consumes. Recognizes git's `%Cred`/`%Cgreen`/`%Cblue`/`%Creset` shortcuts and
/// the general `%C(<spec>)` form; anything else is `parse_color()` answering 0,
/// reported as `false` so the caller renders the `%C` literally.
fn expand_color(
    out: &mut Vec<u8>,
    chars: &[char],
    i: &mut usize,
    want_color: bool,
    auto: &mut bool,
) -> bool {
    // git suppresses the `%C(auto)` reset when nothing has been emitted yet for
    // this commit's format, so record that before appending anything.
    let out_empty = out.is_empty();
    let rest: String = chars[*i..].iter().collect();
    // `%C(<spec>)`
    if rest.starts_with('(') {
        if let Some(close) = rest.find(')') {
            let spec = &rest[1..close];
            out.extend_from_slice(parse_color_spec(spec, want_color, auto, out_empty).as_bytes());
            // Consume through the `)`. `find` answered in bytes and `i` indexes
            // characters, so a non-ASCII byte in the spec would otherwise push the
            // cursor past it — `%C(café)%s` swallowed the `%` of the `%s`.
            *i += rest[..=close].chars().count();
            return true;
        }
        // No closing paren: git prints the rest verbatim. Fall through to literal.
    }
    // Shortcuts.
    for (name, ansi) in [
        ("red", "\x1b[31m"),
        ("green", "\x1b[32m"),
        ("blue", "\x1b[34m"),
        ("reset", "\x1b[m"),
    ] {
        if rest.starts_with(name) {
            if want_color {
                out.extend_from_slice(ansi.as_bytes());
            }
            *i += name.len();
            return true;
        }
    }
    // Unrecognized: git renders the `%C` literally and continues.
    false
}

/// Parse a `%C(<spec>)` color specification into an ANSI escape (empty when color
/// is disabled). Handles `reset`, `auto`/`auto,<colors>` (which also latches the
/// auto-color flag on), attribute words (`bold`, `dim`, `ul`, …), and up to two
/// color names (foreground then background).
fn parse_color_spec(spec: &str, want_color: bool, auto: &mut bool, out_empty: bool) -> String {
    let spec = spec.trim();
    let colors = if let Some(rest) = spec.strip_prefix("auto") {
        // `%C(auto)` alone enables auto-coloring and emits a reset — but git omits
        // that reset at the very start of a commit's output. `%C(auto,<colors>)`
        // additionally applies those colors.
        *auto = true;
        let rest = rest.strip_prefix(',').unwrap_or(rest).trim();
        if rest.is_empty() {
            return if want_color && !out_empty {
                "\x1b[m".to_string()
            } else {
                String::new()
            };
        }
        rest
    } else {
        spec
    };
    if !want_color {
        return String::new();
    }
    if colors == "reset" {
        return "\x1b[m".to_string();
    }
    let mut codes: Vec<String> = Vec::new();
    let mut foreground = true;
    for tok in colors.split_whitespace() {
        let attr = match tok {
            "bold" => Some("1"),
            "dim" => Some("2"),
            "italic" => Some("3"),
            "ul" | "underline" => Some("4"),
            "blink" => Some("5"),
            "reverse" => Some("7"),
            "strike" => Some("9"),
            "nobold" | "no-bold" => Some("22"),
            _ => None,
        };
        if let Some(a) = attr {
            codes.push(a.to_string());
        } else if let Some(base) = color_base(tok) {
            codes.push((if foreground { base } else { base + 10 }).to_string());
            foreground = false;
        }
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Write a built-in format's commit object name — `<hash>` for `oneline`, the
/// `commit <hash>` header otherwise — in `color.diff.commit`. git's span covers
/// exactly the prefix and the hash: `--parents`, `--source` and the decorations
/// that follow it are all outside, each opening their own color.
fn write_commit_name(out: &mut Vec<u8>, prefix: &[u8], id: &str, ctx: &RenderCtx<'_>) {
    let color = &ctx.colors.commit;
    if !color.is_empty() {
        out.extend_from_slice(color.as_bytes());
    }
    out.extend_from_slice(prefix);
    // `get_revision_mark()`: `--boundary` puts a `-` in front of the object name,
    // after the `commit ` the header formats print.
    out.extend_from_slice(ctx.mark.as_bytes());
    out.extend_from_slice(id.as_bytes());
    if !color.is_empty() {
        out.extend_from_slice(b"\x1b[m");
    }
}

/// The `color.diff.commit` sequence for a `%C(auto)`-gated placeholder: the
/// configured color when this run colors and a `%C(auto)` has been seen, else
/// the empty string (which paints nothing).
fn auto_commit_color<'a>(ctx: &'a RenderCtx<'_>, auto: bool) -> &'a str {
    if auto && ctx.want_color {
        &ctx.colors.commit
    } else {
        ""
    }
}

/// Emit `text` in `commit` — git's `color.diff.commit`, which is the color
/// `%C(auto)` gives the commit hash `%h`/`%H`. An empty `commit` (coloring off, or
/// a spec that selects nothing) emits the text bare, with no reset.
fn push_maybe_auto(out: &mut Vec<u8>, text: &str, commit: &str) {
    if commit.is_empty() {
        out.extend_from_slice(text.as_bytes());
    } else {
        out.extend_from_slice(commit.as_bytes());
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[m");
    }
}

/// Map a color name to its SGR foreground base code (background is `+10`).
fn color_base(name: &str) -> Option<u8> {
    Some(match name {
        "black" => 30,
        "red" => 31,
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" => 35,
        "cyan" => 36,
        "white" => 37,
        "default" | "normal" => 39,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Decorations (%d / %D)
// ---------------------------------------------------------------------------

/// The kinds of ref a commit can be decorated with, in git's color scheme —
/// `log-tree.c`'s `decoration_colors[]` indexed by `enum decoration_type`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DecoKind {
    /// `HEAD` itself (bold cyan), the entry the `HEAD -> <branch>` fold hangs off.
    Head,
    Tag,
    LocalBranch,
    RemoteBranch,
    /// The single `refs/stash` ref (bold magenta).
    Stash,
    /// `DECORATION_GRAFTED` — not a ref at all but the pseudo-decoration
    /// `add_graft_decoration()` (log-tree.c:211-219) hangs on every commit the
    /// graft table names, rendered as the literal word `grafted`. A `--depth`
    /// clone registers its boundary commits as grafts, which is the form this
    /// port reaches; `refs/replace/*`'s `replaced` entry shares the slot.
    Grafted,
    /// Any other ref, reachable only once the default namespace filter is
    /// relaxed by `--clear-decorations` / `log.initialDecorationSet=all`.
    /// git's `DECORATION_NONE`, whose color slot is a bare reset.
    Other,
}

/// One ref pointing at a commit, stored under its full name
/// (`refs/remotes/origin/main`); `--decorate=short` prettifies it at render time.
struct Deco {
    kind: DecoKind,
    full: String,
}

/// The ref→commit map plus HEAD state needed to render `%d`/`%D`.
pub(crate) struct Decorations {
    /// Commit oid → the refs pointing at it (annotated tags peeled through to
    /// their commit), including `HEAD` itself when it survived the filter.
    map: HashMap<ObjectId, Vec<Deco>>,
    /// The full refname HEAD symbolically points at (`refs/heads/main`), for the
    /// `HEAD -> <branch>` fold. `None` when HEAD is detached or unborn.
    head_branch: Option<String>,
}

impl Decorations {
    /// `get_name_decoration()`: whether any ref points at this commit, which is
    /// the whole of `--simplify-by-decoration`s interest in them.
    pub(crate) fn decorates(&self, id: &ObjectId) -> bool {
        self.map.contains_key(id)
    }
}

/// git's `prettify_refname`: strip the three namespaces whose short form is
/// unambiguous. Everything else (`refs/stash`, `refs/custom/thing`) is shown in
/// full even under `--decorate=short`.
fn prettify_refname(full: &str) -> &str {
    full.strip_prefix("refs/heads/")
        .or_else(|| full.strip_prefix("refs/tags/"))
        .or_else(|| full.strip_prefix("refs/remotes/"))
        .unwrap_or(full)
}

/// One normalized decoration-filter pattern — the product of git's
/// `refs.c:normalize_glob_ref`.
struct RefPattern {
    /// The pattern with `refs/` prepended unless it already started with `refs/`
    /// or was the literal `HEAD`, and any trailing `/` stripped.
    text: String,
    /// git's `item->util`: set when the *original* pattern held no glob
    /// metacharacter (`has_glob_specials` = `strpbrk(pattern, "?*[")`), which
    /// turns matching into a `/`-bounded prefix test instead of a wildmatch.
    literal: bool,
}

impl RefPattern {
    /// git's `refs.c:normalize_glob_ref` with a `NULL` prefix.
    fn new(pattern: &str) -> RefPattern {
        let mut text = String::new();
        if !pattern.starts_with("refs/") && pattern != "HEAD" {
            text.push_str("refs/");
        }
        text.push_str(pattern);
        if text.ends_with('/') {
            text.pop();
        }
        RefPattern {
            text,
            literal: !pattern.contains(['?', '*', '[']),
        }
    }

    /// git's `log-tree.c:match_ref_pattern`: a literal pattern matches a whole
    /// path prefix (`refs/heads` matches `refs/heads/main` but not
    /// `refs/headsfoo`), a glob pattern goes through `wildmatch(…, 0)`.
    fn matches(&self, refname: &str) -> bool {
        if self.literal {
            match refname.strip_prefix(&self.text) {
                Some(rest) => rest.is_empty() || rest.starts_with('/'),
                None => false,
            }
        } else {
            wildmatch(self.text.as_bytes(), refname.as_bytes())
        }
    }
}

/// git's `struct decoration_filter`: which refs are allowed to decorate a commit.
///
/// The three lists are consulted in git's order (`log-tree.c:ref_filter_match`):
/// a `--decorate-refs-exclude` hit rejects outright; otherwise, when
/// `--decorate-refs` was given at all, only a hit there is kept; otherwise a
/// `log.excludeDecoration` hit rejects. Anything else is decorated.
pub(crate) struct DecorationFilter {
    include: Vec<RefPattern>,
    exclude: Vec<RefPattern>,
    exclude_config: Vec<RefPattern>,
}

/// The refs git decorates by default — `refs.c:ref_namespace[]` filtered to the
/// entries that carry a `decoration` type, in declaration order. Used verbatim
/// as the default `include` list, which is why an unknown namespace such as
/// `refs/bisect/` is invisible until `--clear-decorations` drops this list.
const DEFAULT_DECORATION_NAMESPACES: [&str; 6] = [
    "HEAD",
    "refs/heads/",
    "refs/tags/",
    "refs/remotes/",
    "refs/stash",
    "refs/replace/",
];

impl DecorationFilter {
    /// git's `builtin/log.c:set_default_decoration_filter` followed by the
    /// normalization `load_ref_decorations` performs on all three lists.
    ///
    /// `use_default` is git's `use_default_decoration_filter`, which starts set
    /// and is cleared by `--clear-decorations`. `log.excludeDecoration` is read
    /// unconditionally — and because a non-empty list of any kind suppresses the
    /// namespace defaults, configuring it alone also exposes refs outside the
    /// known namespaces.
    pub(crate) fn build(
        repo: &gix::Repository,
        include_cli: &[String],
        exclude_cli: &[String],
        mut use_default: bool,
    ) -> DecorationFilter {
        let snap = repo.config_snapshot();
        let mut include: Vec<RefPattern> = include_cli.iter().map(|p| RefPattern::new(p)).collect();
        let exclude: Vec<RefPattern> = exclude_cli.iter().map(|p| RefPattern::new(p)).collect();
        // `log.excludeDecoration` is multi-valued: git appends every occurrence
        // across the whole config hierarchy rather than letting the last win.
        let exclude_config: Vec<RefPattern> = snap
            .plumbing()
            .strings("log.excludeDecoration")
            .into_iter()
            .flatten()
            .map(|v| RefPattern::new(&v.to_str_lossy()))
            .collect();

        // `log.initialDecorationSet=all` relaxes the filter exactly as
        // `--clear-decorations` does.
        if use_default
            && snap
                .string("log.initialDecorationSet")
                .is_some_and(|v| v.to_str_lossy() == "all")
        {
            use_default = false;
        }
        if use_default
            && include.is_empty()
            && exclude.is_empty()
            && exclude_config.is_empty()
        {
            include.extend(DEFAULT_DECORATION_NAMESPACES.iter().map(|n| RefPattern::new(n)));
        }

        DecorationFilter {
            include,
            exclude,
            exclude_config,
        }
    }

    /// Port of `log-tree.c:ref_filter_match`.
    fn matches(&self, refname: &str) -> bool {
        if self.exclude.iter().any(|p| p.matches(refname)) {
            return false;
        }
        if !self.include.is_empty() {
            return self.include.iter().any(|p| p.matches(refname));
        }
        if self.exclude_config.iter().any(|p| p.matches(refname)) {
            return false;
        }
        true
    }
}

/// Does this format use a decoration placeholder, so the ref map is worth
/// building? `%%d` (an escaped percent then a literal `d`) does not count.
pub(crate) fn pretty_uses_decoration(pretty: &Pretty) -> bool {
    let Pretty::User(fmt) = pretty else {
        return false;
    };
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c == '%' && matches!(it.next(), Some('d' | 'D')) {
            return true;
        }
    }
    false
}

/// Build the commit→refs decoration map — git's `load_ref_decorations`: every
/// ref that survives `filter` (peeled through annotated tags to its commit),
/// then `HEAD`, which git adds last and therefore renders first.
///
/// `refs/replace/*` is skipped: git turns those into a `replaced` decoration on
/// the object being *replaced*, which is a mechanism this port does not model,
/// so the ref decorating its own target would be plainly wrong.
pub(crate) fn build_decorations(repo: &gix::Repository, filter: &DecorationFilter) -> Result<Decorations> {
    let mut map: HashMap<ObjectId, Vec<Deco>> = HashMap::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow!("{e}"))?;
        let Ok(full) = r.name().as_bstr().to_str().map(str::to_owned) else {
            continue;
        };
        if !filter.matches(&full) || full.starts_with("refs/replace/") {
            continue;
        }
        // git's `add_ref_decoration` classifies by the first `ref_namespace[]`
        // entry the refname matches; anything unclaimed is `DECORATION_NONE`.
        let kind = if full.starts_with("refs/heads/") {
            DecoKind::LocalBranch
        } else if full.starts_with("refs/tags/") {
            DecoKind::Tag
        } else if full.starts_with("refs/remotes/") {
            DecoKind::RemoteBranch
        } else if full == "refs/stash" {
            DecoKind::Stash
        } else {
            DecoKind::Other
        };
        // Peel through annotated tags so a tag ref decorates its target commit.
        let Ok(id) = r.into_fully_peeled_id() else {
            continue;
        };
        map.entry(id.detach()).or_default().push(Deco { kind, full });
    }

    let mut head_branch = None;
    if filter.matches("HEAD") {
        if let Ok(head) = repo.head() {
            if let Some(name) = head.referent_name() {
                if let Ok(full) = name.as_bstr().to_str() {
                    if full.starts_with("refs/") {
                        head_branch = Some(full.to_string());
                    }
                }
            }
            if let Some(id) = head.id() {
                map.entry(id.detach()).or_default().push(Deco {
                    kind: DecoKind::Head,
                    full: "HEAD".to_string(),
                });
            }
        }
    }

    // `for_each_commit_graft(add_graft_decoration, NULL)` (log-tree.c:242): every
    // commit the graft table names gets the literal `grafted` decoration, and it
    // is added last so it renders first. The table is `.git/shallow` plus
    // `.git/info/grafts`; this port carries the shallow half, which is the one a
    // `clone --depth`/`fetch --depth` leaves behind. No `ref_filter_match()` runs
    // over these — they are not refs, so `--decorate-refs` cannot exclude them.
    for id in repo.shallow_commits().ok().flatten().iter().flat_map(|c| c.iter()) {
        map.entry(*id).or_default().push(Deco {
            kind: DecoKind::Grafted,
            full: "grafted".to_string(),
        });
    }

    Ok(Decorations { map, head_branch })
}

/// Expand `%d` (`wrap` true: ` (…)`) or `%D` (`wrap` false: bare) for `commit`.
/// Colored only when `auto` (set by a preceding `%C(auto)`) and color is enabled,
/// matching git, whose decorations stay plain until `%C(auto)` appears.
fn expand_decoration(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    ctx: &RenderCtx<'_>,
    auto: bool,
    wrap: bool,
    full_refs: bool,
) {
    let Some(decos) = ctx.decorations else {
        return;
    };
    // git's decorations stay plain until a `%C(auto)` turns coloring on for the
    // rest of the format, so an un-auto'd run gets the disabled table.
    let disabled;
    let colors = if auto && ctx.want_color {
        ctx.colors
    } else {
        disabled = super::color::DecorateColors::disabled();
        &disabled
    };
    format_decorations(out, decos, &commit.id().detach(), full_refs, colors, wrap);
}

/// Port of `log-tree.c:format_decorations`: the ` (HEAD -> main, tag: v1)` list
/// for one commit. `wrap` picks `%d`'s parenthesised form over `%D`'s bare one,
/// `full_refs` picks `--decorate=full` over `short`, and `colors` supplies git's
/// `decoration_colors[]` as configured by `color.decorate.<slot>` (the disabled
/// table renders the list uncolored). Emits nothing when the commit carries no
/// surviving decoration.
pub(crate) fn format_decorations(
    out: &mut Vec<u8>,
    decos: &Decorations,
    id: &ObjectId,
    full_refs: bool,
    colors: &super::color::DecorateColors,
    wrap: bool,
) {
    let Some(refs) = decos.map.get(id) else {
        return;
    };
    if refs.is_empty() {
        return;
    }

    // `format_decorations()` (log-tree.c:399-401, :387-391) writes the slot color,
    // the text, then `color_reset` — **unconditionally**, never conditioned on the
    // slot having produced a sequence. `color_reset` is
    // `decorate_get_color(use_color, DECORATION_NONE)`, i.e. `\e[m` while coloring
    // is on and `""` while it is off, which is exactly `colors.none`. Skipping the
    // reset for an empty slot dropped a byte stock emits: `color.decorate.branch =
    // normal` parses to an empty sequence but still closes with `\e[m`.
    let paint = |text: &str, code: &str| -> String { format!("{code}{text}{}", colors.none) };
    // git's slot defaults: HEAD bold cyan, local branch bold green, remote bold
    // red, tag bold yellow, stash bold magenta, anything else a bare reset. The
    // punctuation between and around the entries takes `color.diff.commit`, the
    // same color the commit object name it follows is painted with.
    let punct = |text: &str| paint(text, &colors.commit);
    let color_of = |kind: DecoKind| match kind {
        DecoKind::Head => colors.head.as_str(),
        DecoKind::LocalBranch => colors.branch.as_str(),
        DecoKind::RemoteBranch => colors.remote_branch.as_str(),
        DecoKind::Tag => colors.tag.as_str(),
        DecoKind::Stash => colors.stash.as_str(),
        DecoKind::Grafted => colors.grafted.as_str(),
        DecoKind::Other => colors.none.as_str(),
    };
    let show = |d: &Deco| -> String {
        // `--decorate=full` / `log.decorate=full` renders the full ref name
        // (`refs/heads/main`) in place of the prettified one (`main`).
        if full_refs {
            d.full.clone()
        } else {
            prettify_refname(&d.full).to_string()
        }
    };

    // git's `current_pointed_by_HEAD`: the `HEAD -> <branch>` fold happens only
    // when BOTH the `HEAD` decoration and the local branch it resolves to are on
    // this commit and survived the filter. The branch is then not listed twice.
    let head_here = refs.iter().any(|d| d.kind == DecoKind::Head);
    let folded: Option<&Deco> = head_here.then(|| decos.head_branch.as_deref()).flatten().and_then(
        |branch| {
            refs.iter()
                .find(|d| d.kind == DecoKind::LocalBranch && d.full == branch)
        },
    );

    // git prepends each decoration as it iterates refs in ascending full-refname
    // order and adds `HEAD` last, so the display order is `HEAD` first and then
    // DESCENDING full refname: refs/heads/dev, refs/heads/feature, refs/tags/v1
    // -> (tag: v1, feature, dev).
    let mut ordered: Vec<&Deco> = refs
        .iter()
        .filter(|d| folded.is_none_or(|f| !std::ptr::eq(*d, f)))
        .collect();
    // `for_each_commit_graft(add_graft_decoration)` runs *after* the ref walk and
    // after `HEAD` (log-tree.c:221-243), and `add_name_decoration` prepends, so a
    // graft decoration renders ahead of even `HEAD`.
    ordered.sort_by_key(|d| {
        (d.kind != DecoKind::Grafted, d.kind != DecoKind::Head, std::cmp::Reverse(d.full.clone()))
    });

    let mut entries: Vec<String> = Vec::new();
    for d in ordered {
        let mut entry = String::new();
        // git colors the `tag: ` prefix and the tag name as two separate
        // bold-yellow spans.
        if d.kind == DecoKind::Tag {
            entry.push_str(&paint("tag: ", color_of(d.kind)));
        }
        entry.push_str(&paint(&show(d), color_of(d.kind)));
        if d.kind == DecoKind::Head {
            if let Some(f) = folded {
                entry.push_str(&punct(" -> "));
                entry.push_str(&paint(&show(f), color_of(f.kind)));
            }
        }
        entries.push(entry);
    }

    // `%d` wraps in ` (…)`; `%D` emits the bare, comma-separated list.
    if wrap {
        out.extend_from_slice(punct(" (").as_bytes());
    }
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(punct(", ").as_bytes());
        }
        out.extend_from_slice(e.as_bytes());
    }
    if wrap {
        out.extend_from_slice(punct(")").as_bytes());
    }
}

/// Current time in epoch seconds, for relative dates. Delegates to the shared
/// resolver so `%cr`/`%ar`/`--date=relative` honor `GIT_TEST_DATE_NOW` like git.
pub(crate) fn now_secs() -> i64 {
    crate::date::now_seconds()
}

/// `-S<string>` / `--find-object=<id>` over a set of commits, keeping those whose
/// first-parent diff contains a pair `diffcore_pickaxe()` would have kept.
///
/// This is git's `has_changes` (diffcore-pickaxe.c): for each changed path, count the
/// needle in the whole old blob and the whole new blob, and keep the commit as soon as
/// one pair's counts differ. No patch is built and no line diff is run — the needle's
/// position is irrelevant, only how many times it appears, and a blob whose id is
/// unchanged cannot change its own count. `--find-object` is cheaper still: it compares
/// the recorded ids and never reads a blob at all.
///
/// The commits are independent, so the scan runs across the thread pool. Each
/// worker owns a repository handle, which is not `Sync`.
fn pickaxe_by_count(
    repo: &gix::Repository,
    nodes: Vec<Node>,
    kind: &super::diff_pairs::PickaxeKind,
) -> Result<Vec<Node>> {
    let empty_needle = match kind {
        super::diff_pairs::PickaxeKind::Occurrences(super::diff_pairs::Needle::Literal(n)) => {
            n.is_empty()
        }
        _ => false,
    };
    if empty_needle || nodes.is_empty() {
        return Ok(nodes);
    }
    // Two commits per worker: a single commit's scan can read many blobs, so
    // there is real work in each unit.
    let workers = crate::threads::count(nodes.len(), 2);
    if workers <= 1 {
        let mut kept = Vec::new();
        for node in nodes {
            if commit_changes_count(repo, &node, kind)? {
                kept.push(node);
            }
        }
        return Ok(kept);
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut hits: Vec<usize> = Vec::new();
    let mut failure: Option<anyhow::Error> = None;
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        let nodes = &nodes;
        for _ in 0..workers {
            let proto = repo.clone();
            let cursor = &cursor;
            handles.push(scope.spawn(move || -> Result<Vec<usize>> {
                let repo = proto;
                let mut mine = Vec::new();
                loop {
                    let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(node) = nodes.get(i) else { break };
                    if commit_changes_count(&repo, node, kind)? {
                        mine.push(i);
                    }
                }
                Ok(mine)
            }));
        }
        for h in handles {
            match h.join() {
                Ok(Ok(mine)) => hits.extend(mine),
                Ok(Err(e)) => {
                    failure.get_or_insert(e);
                }
                Err(_) => {
                    failure.get_or_insert_with(|| anyhow::anyhow!("pickaxe worker panicked"));
                }
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }

    hits.sort_unstable();
    let mut keep = vec![false; nodes.len()];
    for i in hits {
        keep[i] = true;
    }
    Ok(nodes.into_iter().zip(keep).filter(|(_, k)| *k).map(|(n, _)| n).collect())
}

/// `true` when this commit's first-parent diff holds a pair `diffcore_pickaxe()` keeps.
fn commit_changes_count(
    repo: &gix::Repository,
    node: &Node,
    kind: &super::diff_pairs::PickaxeKind,
) -> Result<bool> {
    let new_tree = repo.find_object(node.id)?.try_into_commit()?.tree()?;
    let old_tree = match node.parents.first() {
        Some(pid) => Some(repo.find_object(*pid)?.try_into_commit()?.tree()?),
        None => None,
    };
    // Counting a blob means reading it, so the count is memoized per blob id
    // within the commit: a file that appears on both sides of several changes
    // (or a tree that reuses a blob) is read once. `--find-object` reads nothing.
    let mut counted: std::collections::HashMap<ObjectId, i64> = std::collections::HashMap::new();
    let mut count_of = |repo: &gix::Repository, id: Option<ObjectId>| -> Result<i64> {
        let Some(id) = id else { return Ok(0) };
        if let Some(n) = counted.get(&id) {
            return Ok(*n);
        }
        // A gitlink or a missing object counts as absent, exactly as git's
        // pickaxe treats a side it cannot read as an empty buffer.
        let n = match repo.find_object(id) {
            Ok(obj) if obj.kind == gix::object::Kind::Blob => match kind {
                super::diff_pairs::PickaxeKind::Occurrences(needle) => {
                    needle.count(&obj.data) as i64
                }
                // Neither reads content; `objfind` short-circuits before this runs and
                // `-G` never reaches this scan.
                _ => 0,
            },
            _ => 0,
        };
        counted.insert(id, n);
        Ok(n)
    };

    // Two passes, because rename detection is expensive and almost never
    // changes the answer.
    //
    // git runs diffcore's rename pass BEFORE the pickaxe, so content moved from
    // one path to another arrives as a single pair whose two sides hold the
    // needle the same number of times — no change, no match. Pairing can only
    // ever CANCEL a difference, never create one: an unpaired deletion and
    // addition compare against nothing, and joining them can only bring the two
    // counts closer. So a first pass with no rename tracking is a strict
    // over-approximation, and only a commit it flags needs the second, exact
    // pass. Most commits are not flagged, and the history's renames are paid for
    // only where they might matter.
    if !any_count_changed(repo, old_tree.as_ref(), &new_tree, kind, &mut count_of, false)? {
        return Ok(false);
    }
    any_count_changed(repo, old_tree.as_ref(), &new_tree, kind, &mut count_of, true)
}

/// Whether any changed pair between the two trees holds the needle a different
/// number of times, with git's rename tracking (50% similarity, no copies)
/// either on or off.
fn any_count_changed(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: &gix::Tree<'_>,
    kind: &super::diff_pairs::PickaxeKind,
    count_of: &mut impl FnMut(&gix::Repository, Option<ObjectId>) -> Result<i64>,
    rename_tracking: bool,
) -> Result<bool> {
    let mut options = gix::diff::Options::default();
    if rename_tracking {
        options.track_rewrites(Some(Default::default()));
    }
    let changes = repo.diff_tree_to_tree(old_tree, Some(new_tree), Some(options))?;
    for change in changes {
        let (old_id, new_id) = change_blob_ids(&change);
        // `o->objfind`: a pair is kept when either side *is* one of the named objects,
        // which is decided before the unmodified-pair short circuit below.
        if let super::diff_pairs::PickaxeKind::ObjFind(ids) = kind {
            if old_id.is_some_and(|i| ids.contains(&i)) || new_id.is_some_and(|i| ids.contains(&i))
            {
                return Ok(true);
            }
            continue;
        }
        // An unchanged blob id on both sides cannot change its own count.
        if old_id == new_id {
            continue;
        }
        if count_of(repo, old_id)? != count_of(repo, new_id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `pickaxe()` (diffcore-pickaxe.c) over one commit's change list.
///
/// Without `--pickaxe-all` only the matching pairs survive. With it the whole list
/// survives as soon as one pair matches ("do not munge the queue") and is cleared
/// outright when none does.
///
/// `-G` is absent because it is decided from patch text rather than the blobs, and
/// reaches the commit through [`pickaxe_hit`] instead.
fn pickaxe_filter_files(
    repo: &gix::Repository,
    px: &super::diff_pairs::Pickaxe,
    files: &mut Vec<FileChange>,
) -> Result<()> {
    let mut hit = Vec::with_capacity(files.len());
    for f in files.iter() {
        hit.push(pickaxe_file_hit(repo, &px.kind, f)?);
    }
    if px.all {
        if !hit.iter().any(|h| *h) {
            files.clear();
        }
        return Ok(());
    }
    let mut it = hit.into_iter();
    files.retain(|_| it.next().unwrap_or(false));
    Ok(())
}

/// `diff_grep()` (diffcore-pickaxe.c) over one commit's change list: a pair is kept
/// when any line its own diff adds or removes matches the regex. `--pickaxe-all`
/// widens that to the whole list once anything matched, exactly as it does for `-S`.
fn grep_filter_files(
    repo: &gix::Repository,
    re: &regex::bytes::Regex,
    all: bool,
    files: &mut Vec<FileChange>,
) -> Result<()> {
    let blob = |side: Option<(u32, ObjectId)>| -> Vec<u8> {
        let Some((_, id)) = side else { return Vec::new() };
        repo.find_object(id).map(|o| o.data.clone()).unwrap_or_default()
    };
    let mut hit = Vec::with_capacity(files.len());
    for f in files.iter() {
        // `diff_grep()` runs its own zero-context diff over the two blobs rather
        // than reusing whatever patch the command is about to print, so `-U<n>`,
        // the whitespace flags and `--diff-algorithm` do not reach it.
        let (old, new) = (blob(f.old_side), blob(f.new_side));
        let before = super::diff_pickaxe::split_lines(&old);
        let after = super::diff_pickaxe::split_lines(&new);
        let mut found = false;
        super::diff_pickaxe::for_each_changed_line(&before, &after, |line| {
            found |= re.is_match(line.strip_suffix(b"\n").unwrap_or(line));
        });
        hit.push(found);
    }
    if all {
        if !hit.iter().any(|h| *h) {
            files.clear();
        }
        return Ok(());
    }
    let mut it = hit.into_iter();
    files.retain(|_| it.next().unwrap_or(false));
    Ok(())
}

/// `pickaxe_match()` for one change: `objfind` compares the recorded ids, `-S` compares
/// the needle's occurrence count in each side's whole blob.
fn pickaxe_file_hit(
    repo: &gix::Repository,
    kind: &super::diff_pairs::PickaxeKind,
    f: &FileChange,
) -> Result<bool> {
    let old = f.old_side.map(|(_, id)| id);
    let new = f.new_side.map(|(_, id)| id);
    match kind {
        super::diff_pairs::PickaxeKind::ObjFind(ids) => {
            Ok(old.is_some_and(|i| ids.contains(&i)) || new.is_some_and(|i| ids.contains(&i)))
        }
        super::diff_pairs::PickaxeKind::Occurrences(needle) => {
            // `diff_unmodified_pair()`: identical ids hold identical content.
            if old == new {
                return Ok(false);
            }
            let count = |id: Option<ObjectId>| -> Result<usize> {
                let Some(id) = id else { return Ok(0) };
                Ok(match repo.find_object(id) {
                    Ok(obj) if obj.kind == gix::object::Kind::Blob => needle.count(&obj.data),
                    // A gitlink or an unreadable object is an empty buffer to the pickaxe.
                    _ => 0,
                })
            };
            Ok(count(old)? != count(new)?)
        }
        // Reached only if a `-G` needle were ever routed here; it is not.
        super::diff_pairs::PickaxeKind::Grep(_) => Ok(true),
    }
}

/// The old and new blob ids of a tree change, or `None` for a side that does not
/// exist (an addition has no old side, a deletion no new one).
fn change_blob_ids(change: &gix::object::tree::diff::ChangeDetached) -> (Option<ObjectId>, Option<ObjectId>) {
    use gix::object::tree::diff::ChangeDetached as C;
    match change {
        C::Addition { id, .. } => (None, Some(*id)),
        C::Deletion { id, .. } => (Some(*id), None),
        C::Modification { previous_id, id, .. } => (Some(*previous_id), Some(*id)),
        C::Rewrite { source_id, id, .. } => (Some(*source_id), Some(*id)),
    }
}

/// Whether a commit's patch satisfies the pickaxe filter, scanning only the
/// added/removed content lines (git's `-S`/`-G` operate on the change text).
///
/// * `-S<string>`: the net occurrence count changed — occurrences on `+` lines
///   minus occurrences on `-` lines is non-zero. This equals git's
///   count-after − count-before, because only changed lines move the total.
/// * `-G<regex>`: some added or removed line matches the regex.
pub(crate) fn pickaxe_hit(
    patch: &[u8],
    needle: Option<&str>,
    re: Option<&regex::bytes::Regex>,
) -> bool {
    let literal = needle.map(|n| super::diff_pickaxe::Needle::Literal(n.as_bytes().to_vec()));
    pickaxe_hit_needle(patch, literal.as_ref(), re)
}

/// [`pickaxe_hit`] with `-S`'s needle already compiled, so `--pickaxe-regex` can
/// hand it the regular-expression form `pickaxe_match()` counts under
/// `DIFF_PICKAXE_REGEX`.
pub(crate) fn pickaxe_hit_needle(
    patch: &[u8],
    needle: Option<&super::diff_pickaxe::Needle>,
    re: Option<&regex::bytes::Regex>,
) -> bool {
    let mut net: i64 = 0;
    for line in patch.split(|&b| b == b'\n') {
        // Only real content changes; skip the `+++`/`---` file headers.
        let (sign, content) = match line.first() {
            Some(b'+') if !line.starts_with(b"+++") => (1i64, &line[1..]),
            Some(b'-') if !line.starts_with(b"---") => (-1i64, &line[1..]),
            _ => continue,
        };
        if let Some(re) = re {
            if re.is_match(content) {
                return true;
            }
        }
        if let Some(needle) = needle {
            net += sign * needle.count(content) as i64;
        }
    }
    // `-G` reached here without matching (or was absent); `-S` hits on net != 0.
    needle.is_some() && net != 0
}


/// git's `approxidate()` for `--since`/`--until`, shared with every other verb that takes a date
/// argument. Re-exported so `rev-list`/`whatchanged`/`show` can keep importing it from here.
pub(crate) use crate::date::approxidate;

/// git's `show_date_relative`, via the shared port (exact thresholds + the
/// `(diff*24+365)/730` years/months rounding).
fn format_relative(then: i64, now: i64) -> String {
    crate::date::show_date_relative(then, now)
}

/// Write a signature's timestamp in `mode`, the shared body of `%ad`/`%cd` and
/// their fixed-format `%ai`/`%aI` cousins.
fn expand_date(
    out: &mut Vec<u8>,
    sig: &gix::actor::SignatureRef<'_>,
    mode: DateMode,
    now: i64,
) -> Result<()> {
    let t = sig.time()?;
    out.extend_from_slice(fmt_time(t.seconds, t.offset, mode, now).as_bytes());
    Ok(())
}

/// Format a timestamp, routing the clock-relative `relative` mode (which needs
/// `now`) to [`format_relative`] and everything else to [`format_date`].
pub(crate) fn fmt_time(seconds: i64, offset: i32, mode: DateMode, now: i64) -> String {
    match mode {
        DateMode::Relative => format_relative(seconds, now),
        other => format_date(seconds, offset, other),
    }
}

/// git's `%b`: the message body — everything after the blank line that ends the
/// subject paragraph. An empty string when the message is a subject only.
fn body(msg: &[u8]) -> Vec<u8> {
    // Skip leading blank lines, then the subject paragraph, then the single blank
    // line separating it from the body.
    let mut rest = msg;
    while let Some(stripped) = rest.strip_prefix(b"\n") {
        rest = stripped;
    }
    match rest.windows(2).position(|w| w == b"\n\n") {
        Some(pos) => rest[pos + 2..].to_vec(),
        None => Vec::new(),
    }
}

/// git's `%f`: the subject sanitised into a filename — `istitlechar` bytes
/// (alphanumeric, `.`, `_`) kept, every other run folded to a single `-`, runs of
/// `.` collapsed, and trailing `.` trimmed.
fn sanitized_subject(subj: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    // 2 = at start, 1 = a separator run is pending, 0 = mid-word.
    let mut space: u8 = 2;
    let mut i = 0;
    while i < subj.len() {
        let c = subj[i];
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' {
            if space == 1 {
                out.push(b'-');
            }
            space = 0;
            out.push(c);
            if c == b'.' {
                while i + 1 < subj.len() && subj[i + 1] == b'.' {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    while out.last() == Some(&b'.') {
        out.pop();
    }
    out
}

/// Space-separated parent ids, abbreviated for `%p` and full for `%P`.
/// `%p`/`%P`: the commit's *effective* parents.
///
/// git rewrites `commit->parents` in place under history simplification, so these
/// placeholders print the simplified ancestry — a merge that `--simplify-merges`
/// replaced is named by what it became, not by itself. Reading the commit object
/// here instead would print the real ancestry and disagree with `--parents`.
fn write_parents(
    out: &mut Vec<u8>,
    abbrev: bool,
    cache: &std::cell::RefCell<AbbrevCache>,
    parents: &[ObjectId],
    repo: &gix::Repository,
) {
    for (i, p) in parents.iter().copied().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        let text = if abbrev {
            cache.borrow_mut().get(p.attach(repo))
        } else {
            p.to_string()
        };
        out.extend_from_slice(text.as_bytes());
    }
}

/// git's subject: the first paragraph of the message, folded onto one line.
fn subject(msg: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for line in msg.split(|&b| b == b'\n') {
        let line = trim_end_ws(line);
        if line.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}

/// The `pretty_print_commit()` body alone, without the `commit <oid>` line.
///
/// `git log` prints that line from `show_log()` (log-tree.c) and the body from
/// `pretty_print_commit()` (pretty.c); [`render_entry`] fuses the two because
/// every `log` caller wants both. `git rev-list`'s `show_commit()` prints the
/// object name itself — with its own `"commit "` prefix, revision mark and
/// `--parents`/`--children` ids in front — and then calls `pretty_print_commit()`
/// for the rest, so it needs the halves separated the way upstream has them.
///
/// The render knobs are the ones `rev-list` leaves at their defaults: no
/// decoration, no color, no `--date=`, and `revs->abbrev` at `DEFAULT_ABBREV` so
/// `%h` shortens while the object name stays full length.
pub(crate) fn rev_list_pretty_body(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
) -> Result<Vec<u8>> {
    let abbrev = std::cell::RefCell::new(AbbrevCache::new(repo));
    let colors = super::color::DecorateColors::disabled();
    let ctx = RenderCtx {
        abbrev_commit: false,
        abbrev: &abbrev,
        // Neither caller is `git log`: `--show-signature` is a `rev_info` field and
        // these two render through `pretty_print_commit()` with a bare context.
        show_signature: false,
        date_mode: DateMode::Default,
        extra: Vec::new(),
        want_color: false,
        colors: &colors,
        now: now_secs(),
        decorations: None,
        decorate: DecorateStyle::Off,
        source: None,
        mailmap: None,
        identity_mailmap: None,
        // `cmd_rev_list` never calls `init_display_notes`, so `rev-list --pretty`
        // prints no notes even where `log` would.
        notes: &[],
        repo,
        mark: "",
        parents: &[],
        // No `--graph` behind either of these callers.
        graph_width: 0,
        // `rev-list --pretty` has no `--expand-tabs` of its own.
        expand_tabs: None,
        // `rev-list` has no reflog walk, so every `%g…` expands to nothing.
        reflog: None,
        date_explicit: false,
        email: EmailStyle::REV_LIST,
    };
    let mut out = Vec::new();
    match pretty {
        // `pp_title_line()` only: `pretty_print_commit()` skips `pp_remainder()`
        // for oneline, so the body is the subject with no trailing newline.
        Pretty::Oneline => out.extend_from_slice(&subject(commit.message_raw()?)),
        // `builtin/rev-list.c` builds its `pretty_print_context` from scratch and
        // leaves `rev`/`print_email_subject`/`encode_email_headers` at zero, so
        // the mail formats come out with a bare `Subject:` and unencoded headers.
        // The magic `From <oid> …` line is `log_write_email_headers()`'s, which
        // only `show_log()` calls — `rev-list` prints its own `commit <oid>`
        // header instead, above this body.
        Pretty::Email | Pretty::MboxRd => {
            email_body(&mut out, commit, pretty, EmailStyle::REV_LIST)?;
        }
        Pretty::User(fmt) => expand_format(&mut out, commit, fmt, &ctx)?,
        Pretty::Reference => {
            let author = commit.author()?;
            let t = author.time()?;
            out.extend_from_slice(abbrev.borrow_mut().get(commit.id()).as_bytes());
            out.extend_from_slice(b" (");
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(b", ");
            out.extend_from_slice(
                fmt_time(t.seconds, t.offset, DateMode::Short, ctx.now).as_bytes(),
            );
            out.push(b')');
        }
        Pretty::Raw => {
            // `pp_header()` copies every header line of the object through
            // unchanged under `CMIT_FMT_RAW` — including `gpgsig`, `encoding` and
            // `mergetag`, which a reconstruction from the parsed fields would
            // drop — and stops at the blank line without emitting it.
            let data = commit.data.as_slice();
            let header_len = data
                .windows(2)
                .position(|w| w == b"\n\n")
                .map_or(data.len(), |at| at + 1);
            out.extend_from_slice(&data[..header_len]);
            // `pretty_print_commit()` adds the blank line, then `pp_remainder()`
            // indents the message four spaces with no tab expansion for `raw`.
            out.push(b'\n');
            indent_message(&mut out, commit.message_raw()?, ctx.expand_tabs.unwrap_or(0));
        }
        Pretty::Medium | Pretty::Short | Pretty::Full | Pretty::Fuller => {
            let author = commit.author()?;
            // `pp_header()` folds the `parent` lines of a merge into one `Merge:`
            // line of abbreviated ids.
            let parents: Vec<_> = commit.parent_ids().collect();
            if parents.len() > 1 {
                out.extend_from_slice(b"Merge:");
                for pid in &parents {
                    out.push(b' ');
                    out.extend_from_slice(abbrev.borrow_mut().get(*pid).as_bytes());
                }
                out.push(b'\n');
            }
            match pretty {
                Pretty::Fuller => {
                    let committer = commit.committer()?;
                    let at = author.time()?;
                    let ct = committer.time()?;
                    write_person(&mut out, b"Author:     ", &author, None);
                    writeln!(
                        out,
                        "AuthorDate: {}",
                        fmt_time(at.seconds, at.offset, ctx.date_mode, ctx.now)
                    )?;
                    write_person(&mut out, b"Commit:     ", &committer, None);
                    writeln!(
                        out,
                        "CommitDate: {}",
                        fmt_time(ct.seconds, ct.offset, ctx.date_mode, ctx.now)
                    )?;
                }
                Pretty::Full => {
                    let committer = commit.committer()?;
                    write_person(&mut out, b"Author: ", &author, None);
                    write_person(&mut out, b"Commit: ", &committer, None);
                }
                _ => {
                    write_person(&mut out, b"Author: ", &author, None);
                    if matches!(pretty, Pretty::Medium) {
                        let t = author.time()?;
                        writeln!(
                            out,
                            "Date:   {}",
                            fmt_time(t.seconds, t.offset, ctx.date_mode, ctx.now)
                        )?;
                    }
                }
            }
            out.push(b'\n');
            if matches!(pretty, Pretty::Short) {
                out.extend_from_slice(b"    ");
                out.extend_from_slice(&subject(commit.message_raw()?));
                out.push(b'\n');
            } else {
                indent_message(&mut out, commit.message_raw()?, ctx.expand_tabs.unwrap_or(8));
            }
        }
    }
    abbrev.into_inner().flush();
    Ok(out)
}

/// The knobs `git show` fills its [`RenderCtx`] from.
///
/// `cmd_show` runs the same `cmd_log_init` as `cmd_log` and prints each record
/// through the same `show_log()`/`pretty_print_commit()` pair, so every pretty
/// format `git log` renders is a format `git show` renders identically. The
/// fields left out here are the ones `cmd_show` never sets: it has no `--graph`,
/// no `--parents`/`--children`, no `--boundary` mark, and no reflog walk, and it
/// never colors its output.
pub(crate) struct ShowEntry<'a> {
    /// `--abbrev-commit` / `log.abbrevCommit`.
    pub(crate) abbrev_commit: bool,
    /// `--date=` / `log.date`.
    pub(crate) date_mode: DateMode,
    /// `--decorate` / `log.decorate`.
    pub(crate) decorate: DecorateStyle,
    /// The commit→refs map behind `decorate` and `%d`/`%D`.
    pub(crate) decorations: Option<&'a Decorations>,
    /// `--use-mailmap` / `log.mailmap`, for the `Author:`/`Commit:` lines.
    pub(crate) mailmap: Option<&'a Mailmap>,
    /// The mailmap `%aN`/`%aE`/`%cN`/`%cE` resolve through regardless of the flag.
    pub(crate) identity_mailmap: Option<&'a Mailmap>,
    /// The notes trees whose `Notes[ (<ref>)]:` blocks follow the message.
    pub(crate) notes: &'a [super::notes::Tree],
    /// `--expand-tabs[=<n>]` / `--no-expand-tabs`.
    pub(crate) expand_tabs: Option<usize>,
    /// The two `pretty_print_context` fields the mail formats read.
    pub(crate) email: EmailStyle<'a>,
    /// `--source`: the argument this commit was reached from.
    pub(crate) source: Option<&'a [u8]>,
    /// `--show-signature`: print the signature checker's report above the header.
    pub(crate) show_signature: bool,
    /// `log->parent` (log-tree.c:1149): the parent this record's diff was taken
    /// against, which `show_log()` prints as ` (from <oid>)` after the commit id
    /// and before the decorations (log-tree.c:824-826). Only the per-parent
    /// records of `--diff-merges=separate`/`-m` carry one.
    pub(crate) from: Option<ObjectId>,
}

/// A reusable [`render_entry`] driver for the commands that render one record at
/// a time rather than through [`log`]'s windowed walk.
///
/// It owns the two things a `RenderCtx` borrows and a single call cannot: the
/// memoised abbreviation cache (so a multi-commit `git show A..B` shortens each
/// id once) and the disabled color table.
pub(crate) struct EntryRenderer<'r> {
    repo: &'r gix::Repository,
    abbrev: std::cell::RefCell<AbbrevCache>,
    colors: super::color::DecorateColors,
    /// `o->use_color`: whether `%C…` and the header's own coloring emit ANSI.
    want_color: bool,
    now: i64,
}

impl<'r> EntryRenderer<'r> {
    pub(crate) fn new(repo: &'r gix::Repository) -> Self {
        Self::with_color(repo, false)
    }

    /// The same renderer with `o->use_color` set, which is what
    /// `diff_opt_word_diff()`'s `GIT_COLOR_ALWAYS` turns on for the whole record:
    /// `log_tree_commit()` hands the header the run's own color setting, so the
    /// `commit <id>` line and the decorations are painted exactly when the patch is.
    pub(crate) fn with_color(repo: &'r gix::Repository, want_color: bool) -> Self {
        EntryRenderer {
            repo,
            abbrev: std::cell::RefCell::new(AbbrevCache::new(repo)),
            colors: match want_color {
                true => super::color::DecorateColors::resolve(repo),
                false => super::color::DecorateColors::disabled(),
            },
            want_color,
            now: now_secs(),
        }
    }

    /// Render one commit's header in `pretty`, exactly as `git log` would.
    pub(crate) fn render(
        &self,
        out: &mut Vec<u8>,
        commit: &gix::Commit<'_>,
        pretty: &Pretty,
        opts: &ShowEntry<'_>,
    ) -> Result<()> {
        // `cmd_show` never simplifies history, so the effective parent list the
        // `Merge:` line prints is the commit's own.
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        let ctx = RenderCtx {
            abbrev_commit: opts.abbrev_commit,
            abbrev: &self.abbrev,
            show_signature: opts.show_signature,
            date_mode: opts.date_mode,
            // No `--parents`/`--children` in `cmd_show`; the one thing that can
            // stand in this slot is `show_log()`'s ` (from <oid>)` insert, printed
            // at the abbreviation width the header itself uses (log-tree.c:824-826).
            extra: match opts.from {
                None => Vec::new(),
                Some(parent) => {
                    let attached = parent.attach(self.repo);
                    let id = if opts.abbrev_commit {
                        self.abbrev.borrow_mut().get(attached)
                    } else {
                        attached.to_string()
                    };
                    format!(" (from {id})").into_bytes()
                }
            },
            want_color: self.want_color,
            colors: &self.colors,
            now: self.now,
            decorations: opts.decorations,
            decorate: opts.decorate,
            source: opts.source,
            mailmap: opts.mailmap,
            identity_mailmap: opts.identity_mailmap,
            notes: opts.notes,
            repo: self.repo,
            // No `--boundary` mark and no `--graph` columns.
            mark: "",
            parents: &parents,
            graph_width: 0,
            expand_tabs: opts.expand_tabs,
            // No `-g` walk, so every `%g…` expands to nothing.
            reflog: None,
            date_explicit: false,
            email: opts.email,
        };
        // `pretty_print_commit()` fills a `struct strbuf msgbuf` of its own, which
        // `show_log()` then writes out. Rendering into a fresh buffer here is that
        // separation: `%w(…)`'s `rewrap_message_tail()` re-wraps everything from
        // `wrap_start`, and with `wrap_start` at 0 of a shared output buffer it
        // would re-wrap — and swallow the record separator of — whatever this
        // command already printed.
        let mut rec = Vec::new();
        render_entry(&mut rec, commit, pretty, &ctx)?;
        out.extend_from_slice(&rec);
        Ok(())
    }

    /// Persist whatever abbreviations this renderer computed, as [`log`] does at
    /// the end of its walk.
    pub(crate) fn finish(self) {
        self.abbrev.into_inner().flush();
    }
}

/// The per-commit rendering knobs threaded down from [`log`].
struct RenderCtx<'a> {
    /// `--abbrev-commit`: shorten the commit id on the header/oneline.
    abbrev_commit: bool,
    /// Memoised abbreviations (see [`AbbrevCache`]); shared, so a `&RenderCtx`
    /// can still record what it computed.
    abbrev: &'a std::cell::RefCell<AbbrevCache>,
    /// `--date=`: the format `%ad`/`%cd` and the `Date`/`*Date` lines follow.
    date_mode: DateMode,
    /// `--parents`: the commit's own parent ids, decorating the header/oneline.
    /// Empty when the flag is off. Full-length ids unless `abbrev_commit`.
    extra: Vec<u8>,
    /// Whether `%C`/`%C(...)` color placeholders and `%C(auto)`-gated decoration
    /// emit ANSI escapes (git's `want_color`).
    want_color: bool,
    /// The `color.decorate.*` slots and `color.diff.commit`, resolved from config;
    /// the disabled table when coloring is off.
    colors: &'a super::color::DecorateColors,
    /// Current time in epoch seconds, for relative dates (`%cr`/`%ar`).
    now: i64,
    /// Commit→refs map plus HEAD info for `%d`/`%D`; `None` when the format has no
    /// decoration placeholder.
    decorations: Option<&'a Decorations>,
    /// `--decorate` / `log.decorate`: the decoration style for the oneline/header
    /// formats. `Off` appends nothing; `Short`/`Full` append ` (refs)` with short
    /// or full ref names. Also selects short-vs-full for the `%d`/`%D`
    /// placeholders (which are shown regardless of `Off`, in short form).
    decorate: DecorateStyle,
    /// `--source`: the ref/argument this commit was reached from, rendered as
    /// `\t<source>` after the hash on the built-in header formats. `None` when
    /// `--source` is off (and for user/`reference` formats, which git leaves bare).
    source: Option<&'a [u8]>,
    /// `--show-signature`: `show_signature()` (log-tree.c:580, called at :851) prints
    /// the signature checker's own report between the commit-name line and the
    /// pretty-printed header — for every format, `oneline` and the user formats
    /// included, because the call sits outside the format switch.
    show_signature: bool,
    /// `--use-mailmap` / `log.mailmap`: rewrites the `Author:`/`Commit:` lines of
    /// the built-in header formats through `.mailmap`. `None` leaves the
    /// identities as the commit recorded them. git applies it in `pp_user_info`
    /// only, so `oneline`, `raw` and user formats are unaffected — `%aN`/`%aE`
    /// consult the mailmap on their own, independent of this flag.
    mailmap: Option<&'a Mailmap>,
    /// The mailmap `%aN`/`%aE`/`%cN`/`%cE` resolve through. Loaded whenever a format
    /// asks for them, even under `--no-use-mailmap`, which is what
    /// `format_person_part()` does.
    identity_mailmap: Option<&'a Mailmap>,
    /// The notes trees whose `Notes[ (<ref>)]:` blocks follow the message. Empty
    /// when notes are off; a user format reaches them only through `%N`.
    notes: &'a [super::notes::Tree],
    /// Rendering a note means reading its blob.
    repo: &'a gix::Repository,
    /// `get_revision_mark()`: `- ` for a `--boundary` commit, empty otherwise.
    mark: &'static str,
    /// The commit's effective parents — its own, or the rewritten list a history
    /// simplification left behind. What `Merge:` and `--parents` print.
    parents: &'a [ObjectId],
    /// `pretty_ctx->graph_width`: the columns `--graph` prefixes this commit's row
    /// with, which count against a `%<|(<N>)` column target. Zero without `--graph`.
    graph_width: i32,
    /// `revs->expand_tabs_in_log`, when `--expand-tabs[=<n>]`/`--no-expand-tabs`
    /// set one. `None` leaves each format on its own default —
    /// `expand_tabs_in_log_default` (8) for the indented headers, and none for
    /// `raw`, which prints the message unindented by git's own reckoning.
    expand_tabs: Option<usize>,
    /// `-g`: the reflog entry this record stands for. `Some` puts the `Reflog:` /
    /// `Reflog message:` pair in the built-in headers, replaces `oneline`'s subject
    /// with `<selector>: <message>`, and fills the `%g…` placeholders.
    reflog: Option<&'a ReflogEntry>,
    /// `revs->date_mode_explicit` — set only by a `--date=` on the command line,
    /// never by `log.date`. It is `get_reflog_selector()`'s `force_date`: the
    /// selector prints `HEAD@{<date>}` instead of `HEAD@{<n>}`.
    date_explicit: bool,
    /// The two `pretty_print_context` fields [`Pretty::Email`] reads.
    email: EmailStyle<'a>,
}

/// The `Notes[ (<ref>)]:` blocks for `commit`, or empty.
///
/// git appends these to the message buffer, so the leading newline
/// `format_display_notes()` emits lands differently per format: after a
/// `medium` message (which already ends in a newline) it renders as the blank
/// line above the block, and after a `oneline` subject it just ends that line.
fn notes_block(commit: &gix::Commit<'_>, ctx: &RenderCtx<'_>) -> Result<Vec<u8>> {
    if ctx.notes.is_empty() {
        return Ok(Vec::new());
    }
    super::notes::format_display(ctx.repo, ctx.notes, commit.id().detach(), false)
}

/// `show_signature()` (log-tree.c:580) plus `show_sig_lines()` (log-tree.c:564).
///
/// ```c
/// if (parse_signed_commit(commit, &payload, &signature, the_hash_algo) <= 0)
///         goto out;
/// status = check_signature(&sigc, payload.buf, payload.len, signature.buf, signature.len);
/// if (status && !sigc.output)
///         show_sig_lines(opt, status, "No signature\n");
/// else
///         show_sig_lines(opt, status, sigc.output);
/// ```
///
/// An unsigned commit prints nothing at all — `parse_signed_commit()` answers 0 and
/// the function returns before the checker runs. `show_sig_lines()` copies the report
/// verbatim, adding a newline only where one was already there, so a report without a
/// trailing newline runs into the line that follows; that is reproduced rather than
/// tidied.
///
/// The two colours it would wrap each line in (`DIFF_FRAGINFO` when the check passed,
/// `DIFF_WHITESPACE` when it did not) are empty unless colour is on, which this
/// renderer does not paint — the same standing gap as the patch body.
fn write_signature_block(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    ctx: &RenderCtx<'_>,
) -> Result<()> {
    if !ctx.show_signature {
        return Ok(());
    }
    let Some((check, min_trust)) = checked_signature(ctx.repo, &commit.data)? else {
        return Ok(());
    };
    // `status` is `check_signature()`'s return value: 0 only for a signature that both
    // verified and cleared `gpg.minTrustLevel`.
    if !check.verified(min_trust) && check.output.is_empty() {
        out.extend_from_slice(b"No signature\n");
    } else {
        out.extend_from_slice(&check.output);
    }
    Ok(())
}

/// Check a raw commit object's signature the way `check_commit_signature()` does,
/// with `gpg_interface_lazy_init()`'s config validation in front of it.
///
/// `Ok(None)` is an unsigned commit: `parse_signed_commit()` answers 0 and neither
/// the checker nor the config check is reached, which is why `git log` in a
/// repository with an invalid `gpg.format` prints `N` for `%G?` and says nothing —
/// right up until it meets a signed commit, where the same command dies. The
/// validation itself is [`super::verify_commit::min_trust_level`], so the wording and
/// the `error:`-then-fatal shape are the ones the verify verbs already emit.
fn checked_signature(
    repo: &gix::Repository,
    raw: &[u8],
) -> Result<Option<(crate::gitsig::SigCheck, crate::gitsig::Trust)>> {
    let Some((sig, payload)) = crate::gitsig::split_signed(raw) else {
        return Ok(None);
    };
    let min_trust = super::verify_commit::min_trust_level(repo)?;
    Ok(Some((crate::gitsig::verify_full(&sig, &payload), min_trust)))
}

/// Render one commit's header in the selected format. Built-in formats end with
/// a newline; user formats, `oneline`, and `reference` do not, because their
/// record ending is supplied by the separator/terminator rule in [`log`].
fn render_entry(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
    ctx: &RenderCtx<'_>,
) -> Result<()> {
    let id = if ctx.abbrev_commit {
        ctx.abbrev.borrow_mut().get(commit.id())
    } else {
        commit.id().to_string()
    };

    match pretty {
        Pretty::Oneline => {
            write_commit_name(out, b"", &id, ctx);
            out.extend_from_slice(&ctx.extra);
            write_source(out, ctx);
            // `--decorate`: ` (HEAD -> main, tag: v1)` between the hash and subject.
            if ctx.decorate != DecorateStyle::Off {
                expand_decoration(
                    out,
                    commit,
                    ctx,
                    ctx.want_color,
                    true,
                    ctx.decorate == DecorateStyle::Full,
                );
            }
            out.push(b' ');
            write_signature_block(out, commit, ctx)?;
            // `show_log()`: under `-g` the oneline record is the reflog selector and
            // the entry's own message, and it `return`s there — the commit's subject
            // and its notes are never reached.
            if let Some(rl) = ctx.reflog {
                write_reflog_selector(out, rl, ctx, false);
                out.extend_from_slice(b": ");
                out.extend_from_slice(&rl.message);
                return Ok(());
            }
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(&notes_block(commit, ctx)?);
        }
        Pretty::Reference => {
            // `%h (%s, %ad)` with `--date=short` unless `--date=` overrode it.
            let date_mode = match ctx.date_mode {
                DateMode::Default => DateMode::Short,
                other => other,
            };
            let author = commit.author()?;
            let t = author.time()?;
            out.extend_from_slice(ctx.abbrev.borrow_mut().get(commit.id()).as_bytes());
            out.extend_from_slice(b" (");
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(b", ");
            out.extend_from_slice(fmt_time(t.seconds, t.offset, date_mode, ctx.now).as_bytes());
            out.push(b')');
        }
        // ```c
        // if (cmit_fmt_is_mail(opt->commit_format)) {
        //         log_write_email_headers(opt, commit, &extra_headers,
        //                                 &ctx.need_8bit_cte, 1);
        //         ctx.rev = opt;
        //         ctx.print_email_subject = 1;
        // } else if (opt->commit_format != CMIT_FMT_USERFORMAT) {
        //         … fputs("commit ", …) …
        // ```
        //
        // (log-tree.c:697-705.) The mail formats take the `From <oid> Mon Sep 17
        // 00:00:00 2001` line *instead of* the `commit <oid>` line, so none of
        // what decorates that one — `--abbrev-commit`, `--decorate`, `--source`,
        // `--parents`, the `Reflog:` header — is reached. The name is
        // `oid_to_hex()`, never the abbreviation.
        Pretty::Email | Pretty::MboxRd => {
            writeln!(out, "From {} Mon Sep 17 00:00:00 2001", commit.id())?;
            write_signature_block(out, commit, ctx)?;
            email_body(out, commit, pretty, ctx.email)?;
            // ```c
            // if ((ctx.fmt != CMIT_FMT_USERFORMAT) &&
            //     ctx.notes_message && *ctx.notes_message) {
            //         if (cmit_fmt_is_mail(ctx.fmt))
            //                 next_commentary_block(opt, &msgbuf);
            //         strbuf_addstr(&msgbuf, ctx.notes_message);
            // }
            // ```
            //
            // (log-tree.c:893-898.) The mail formats fence the notes off from the
            // commit message with the `---` line `next_commentary_block()` writes,
            // since everything past it is commentary a patch applier drops.
            let notes = notes_block(commit, ctx)?;
            if !notes.is_empty() {
                out.extend_from_slice(b"---\n");
                out.extend_from_slice(&notes);
            }
        }
        Pretty::User(fmt) => {
            write_signature_block(out, commit, ctx)?;
            expand_format(out, commit, fmt, ctx)?;
        }
        Pretty::Raw => {
            let author = commit.author()?;
            let committer = commit.committer()?;
            // `show_log()` prints one `commit <name>` line for every non-mail,
            // non-user format (log-tree.c:810-834), so `raw` gets exactly what
            // `medium` gets: the `--abbrev-commit` name, `--parents`/`--children`
            // ids, `--source`, and the `--decorate` suffix.
            write_commit_name(out, b"commit ", &id, ctx);
            out.extend_from_slice(&ctx.extra);
            write_source(out, ctx);
            if ctx.decorate != DecorateStyle::Off {
                expand_decoration(
                    out,
                    commit,
                    ctx,
                    ctx.want_color,
                    true,
                    ctx.decorate == DecorateStyle::Full,
                );
            }
            out.push(b'\n');
            write_reflog_header(out, ctx);
            write_signature_block(out, commit, ctx)?;
            writeln!(out, "tree {}", commit.tree_id()?)?;
            for pid in commit.parent_ids() {
                writeln!(out, "parent {pid}")?;
            }
            write_raw_ident(out, b"author", &author)?;
            write_raw_ident(out, b"committer", &committer)?;
            // ```c
            // if (pp->fmt == CMIT_FMT_RAW) {
            //         strbuf_add(sb, line, linelen);
            //         continue;
            // }
            // ```
            //
            // (`pp_header()`, pretty.c.) `raw` copies **every** header line of the
            // commit through, so the ones this port does not reconstruct by name —
            // `encoding`, `gpgsig`, `mergetag` — still have to be written. They come
            // out in the order they are stored, which is behind the four above.
            for line in extra_headers(commit.data.as_slice()) {
                out.extend_from_slice(line);
                out.push(b'\n');
            }
            out.push(b'\n');
            // `raw` prints the message as stored: its table entry has no tab width.
            indent_message(out, commit.message_raw()?, ctx.expand_tabs.unwrap_or(0));
            out.extend_from_slice(&notes_block(commit, ctx)?);
        }
        Pretty::Medium | Pretty::Short | Pretty::Full | Pretty::Fuller => {
            let author = commit.author()?;
            write_commit_name(out, b"commit ", &id, ctx);
            out.extend_from_slice(&ctx.extra);
            write_source(out, ctx);
            // `--decorate`: ` (HEAD -> main, tag: v1)` after the commit id.
            if ctx.decorate != DecorateStyle::Off {
                expand_decoration(
                    out,
                    commit,
                    ctx,
                    ctx.want_color,
                    true,
                    ctx.decorate == DecorateStyle::Full,
                );
            }
            out.push(b'\n');
            write_reflog_header(out, ctx);
            write_signature_block(out, commit, ctx)?;

            // A merge commit lists its abbreviated parents right after `commit`.
            // The list is the *effective* one: history simplification rewrites
            // parents before anything is printed, so a merge whose sides
            // collapsed onto one line is no longer shown as a merge.
            if ctx.parents.len() > 1 {
                out.extend_from_slice(b"Merge:");
                for pid in ctx.parents {
                    out.push(b' ');
                    out.extend_from_slice(ctx.abbrev.borrow_mut().get(pid.attach(ctx.repo)).as_bytes());
                }
                out.push(b'\n');
            }

            match pretty {
                Pretty::Fuller => {
                    let committer = commit.committer()?;
                    let at = author.time()?;
                    let ct = committer.time()?;
                    write_person(out, b"Author:     ", &author, ctx.mailmap);
                    writeln!(
                        out,
                        "AuthorDate: {}",
                        fmt_time(at.seconds, at.offset, ctx.date_mode, ctx.now)
                    )?;
                    write_person(out, b"Commit:     ", &committer, ctx.mailmap);
                    writeln!(
                        out,
                        "CommitDate: {}",
                        fmt_time(ct.seconds, ct.offset, ctx.date_mode, ctx.now)
                    )?;
                }
                Pretty::Full => {
                    let committer = commit.committer()?;
                    write_person(out, b"Author: ", &author, ctx.mailmap);
                    write_person(out, b"Commit: ", &committer, ctx.mailmap);
                }
                _ => {
                    // medium / short
                    write_person(out, b"Author: ", &author, ctx.mailmap);
                    if matches!(pretty, Pretty::Medium) {
                        let time = author.time()?;
                        writeln!(
                            out,
                            "Date:   {}",
                            fmt_time(time.seconds, time.offset, ctx.date_mode, ctx.now)
                        )?;
                    }
                }
            }
            out.push(b'\n');

            if matches!(pretty, Pretty::Short) {
                // `short` shows only the subject, indented four spaces.
                out.extend_from_slice(b"    ");
                out.extend_from_slice(&subject(commit.message_raw()?));
                out.push(b'\n');
            } else {
                indent_message(out, commit.message_raw()?, ctx.expand_tabs.unwrap_or(8));
            }
            out.extend_from_slice(&notes_block(commit, ctx)?);
        }
    }
    Ok(())
}

/// `next_commentary_block()`'s side effect, `opt->shown_dashes`: the mail formats
/// print a `---` line above their notes block, and having shown it there is what
/// suppresses the second one a `--stat`-plus-`-p` pair would otherwise put between
/// the message and the diff.
///
/// ```c
/// if (!opt->shown_dashes &&
///     (pch & opt->diffopt.output_format) == pch)
///         fprintf(opt->diffopt.file, "---");
/// putc('\n', opt->diffopt.file);
/// ```
///
/// (log-tree.c:965-968.)
pub(crate) fn mail_notes_shown_dashes(
    repo: &gix::Repository,
    notes: &[super::notes::Tree],
    pretty: &Pretty,
    id: ObjectId,
) -> Result<bool> {
    if notes.is_empty() || !matches!(pretty, Pretty::Email | Pretty::MboxRd) {
        return Ok(false);
    }
    Ok(!super::notes::format_display(repo, notes, id, false)?.is_empty())
}

/// `--source`: git's `show_log` prints `\t<source>` right after the commit hash
/// (and any `--parents` ids) on the built-in header formats. A no-op when `--source`
/// is off. User and `reference` formats never call this, matching git.
fn write_source(out: &mut Vec<u8>, ctx: &RenderCtx<'_>) {
    if let Some(src) = ctx.source {
        out.push(b'\t');
        out.extend_from_slice(src);
    }
}

/// `get_reflog_selector()`: `<ref>@{<n>}` — or `<ref>@{<date>}` when `--date=` was
/// given explicitly. `shorten` picks `%gd`'s canonical short ref; the `Reflog:`
/// header line and `%gD` print the ref as the walk was asked for it.
fn write_reflog_selector(
    out: &mut Vec<u8>,
    entry: &ReflogEntry,
    ctx: &RenderCtx<'_>,
    shorten: bool,
) {
    let sel = entry.selector(ctx.repo, shorten, ctx.date_mode, ctx.date_explicit, ctx.now);
    out.extend_from_slice(sel.as_bytes());
}

/// `show_reflog_message()` for a header format: the `Reflog:` / `Reflog message:`
/// pair `show_log()` prints between the `commit <id>` line and whatever
/// `pretty_print_commit()` writes next (`Merge:` for the built-in headers, `tree`
/// for `raw`). A no-op outside a `-g` walk.
fn write_reflog_header(out: &mut Vec<u8>, ctx: &RenderCtx<'_>) {
    let Some(rl) = ctx.reflog else { return };
    out.extend_from_slice(b"Reflog: ");
    write_reflog_selector(out, rl, ctx, false);
    out.extend_from_slice(b" (");
    out.extend_from_slice(&rl.who_name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(&rl.who_email);
    out.extend_from_slice(b">)\nReflog message: ");
    out.extend_from_slice(&rl.message);
    out.push(b'\n');
}

/// Write git's `<label> <name> <<email>>` header line, mapped through the
/// mailmap when `--use-mailmap` / `log.mailmap` supplied one — git's
/// `pp_user_info`, which is the single place the built-in formats resolve an
/// identity.
/// Whether a user format names `%aN`, `%aE`, `%cN` or `%cE` — the placeholders that
/// resolve through `.mailmap` on their own, so their presence is what decides
/// whether one has to be loaded.
pub(crate) fn format_names_mapped_identity(fmt: &str) -> bool {
    let bytes = fmt.as_bytes();
    bytes.windows(3).any(|w| {
        w[0] == b'%' && matches!(w[1], b'a' | b'c') && matches!(w[2], b'N' | b'E')
    })
}

/// `format_person_part()`'s `N`: the mailmap's name for an identity, or the one the
/// commit recorded when nothing maps it.
fn mapped_name<'a>(sig: &'a gix::actor::SignatureRef<'a>, mailmap: Option<&'a Mailmap>) -> &'a [u8] {
    mailmap
        .and_then(|m| m.lookup(sig.name, sig.email))
        .and_then(|info| info.name.as_deref())
        .unwrap_or(sig.name)
}

/// `format_person_part()`'s `E`: the same for the address.
fn mapped_email<'a>(
    sig: &'a gix::actor::SignatureRef<'a>,
    mailmap: Option<&'a Mailmap>,
) -> &'a [u8] {
    mailmap
        .and_then(|m| m.lookup(sig.name, sig.email))
        .and_then(|info| info.email.as_deref())
        .unwrap_or(sig.email)
}

pub(crate) fn write_person(
    out: &mut Vec<u8>,
    label: &[u8],
    sig: &gix::actor::SignatureRef<'_>,
    mailmap: Option<&Mailmap>,
) {
    let (mut name, mut email): (&[u8], &[u8]) = (sig.name, sig.email);
    if let Some(info) = mailmap.and_then(|m| m.lookup(name, email)) {
        if let Some(e) = &info.email {
            email = e;
        }
        if let Some(n) = &info.name {
            name = n;
        }
    }
    out.extend_from_slice(label);
    out.extend_from_slice(name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(email);
    out.extend_from_slice(b">\n");
}

/// git's mailmap lookup structure (`mailmap.c`), built from the entries
/// gitoxide parsed out of the repository's mailmap sources.
///
/// `gix_mailmap::Snapshot::resolve` cannot be used directly: it also normalizes
/// the *case* of the address to the mailmap's spelling, even for an entry that
/// only renames the author. git leaves the address exactly as the commit
/// recorded it there, so `Renamed Nick <NICK@X.com>` keeps its capitals. Only
/// the lookup is reimplemented here; finding, reading and parsing the mailmap
/// files is still gitoxide's (`Repository::open_mailmap`).
#[derive(Default)]
pub(crate) struct Mailmap {
    /// Keyed by the ASCII-lowercased old email, which is how git's `strcasecmp`
    /// comparison behaves.
    by_email: HashMap<Vec<u8>, MailmapEmail>,
}

/// All entries sharing one commit email — git's `struct mailmap_entry`.
#[derive(Default)]
struct MailmapEmail {
    /// The mapping used when no `<old-name>` qualifier matched.
    simple: MailmapInfo,
    /// Name-qualified mappings, keyed by the ASCII-lowercased old name.
    by_name: HashMap<Vec<u8>, MailmapInfo>,
}

/// The replacement name and/or email a matched entry supplies — git's
/// `struct mailmap_info`. An entry with neither is "no match".
#[derive(Default)]
pub(crate) struct MailmapInfo {
    name: Option<Vec<u8>>,
    email: Option<Vec<u8>>,
}

impl Mailmap {
    /// Load every mailmap source gitoxide knows about (worktree `.mailmap`, then
    /// `mailmap.blob`, then `mailmap.file`) and index it git's way.
    pub(crate) fn load(repo: &gix::Repository) -> Mailmap {
        let snapshot = repo.open_mailmap();
        let mut map = Mailmap::default();
        // git's `add_mapping`: a name-qualified line owns its own sub-entry, an
        // unqualified line overrides only the fields it carries.
        for entry in snapshot.entries() {
            let slot = map.by_email.entry(lower_ascii(entry.old_email())).or_default();
            match entry.old_name() {
                None => {
                    if let Some(n) = entry.new_name() {
                        slot.simple.name = Some(n.to_vec());
                    }
                    if let Some(e) = entry.new_email() {
                        slot.simple.email = Some(e.to_vec());
                    }
                }
                Some(old_name) => {
                    slot.by_name.insert(
                        lower_ascii(old_name),
                        MailmapInfo {
                            name: entry.new_name().map(|n| n.to_vec()),
                            email: entry.new_email().map(|e| e.to_vec()),
                        },
                    );
                }
            }
        }
        map
    }

    /// git's `map_user`: find the email, then prefer a name-qualified sub-entry
    /// when one matches, else fall back to the unqualified mapping.
    /// The identity as the mailmap reports it: each half replaced where a mapping
    /// covers it, kept as recorded otherwise.
    pub(crate) fn mapped(&self, name: &[u8], email: &[u8]) -> (Vec<u8>, Vec<u8>) {
        match self.lookup(name, email) {
            None => (name.to_vec(), email.to_vec()),
            Some(info) => (
                info.name.clone().unwrap_or_else(|| name.to_vec()),
                info.email.clone().unwrap_or_else(|| email.to_vec()),
            ),
        }
    }

    fn lookup(&self, name: &[u8], email: &[u8]) -> Option<&MailmapInfo> {
        let slot = self.by_email.get(&lower_ascii(email))?;
        let info = if slot.by_name.is_empty() {
            &slot.simple
        } else {
            slot.by_name.get(&lower_ascii(name)).unwrap_or(&slot.simple)
        };
        (info.name.is_some() || info.email.is_some()).then_some(info)
    }
}

/// The ASCII-lowercased lookup key for a mailmap email or name, matching the
/// `strcasecmp` git compares them with.
fn lower_ascii(s: &[u8]) -> Vec<u8> {
    s.iter().map(u8::to_ascii_lowercase).collect()
}

/// Write a raw-format identity line: `<role> <name> <<email>> <seconds> +ZZZZ`.
fn write_raw_ident(out: &mut Vec<u8>, role: &[u8], sig: &gix::actor::SignatureRef<'_>) -> Result<()> {
    let t = sig.time()?;
    let (sign, off) = if t.offset < 0 { ('-', -t.offset) } else { ('+', t.offset) };
    out.extend_from_slice(role);
    out.push(b' ');
    out.extend_from_slice(sig.name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(sig.email);
    out.push(b'>');
    writeln!(
        out,
        " {} {sign}{:02}{:02}",
        t.seconds,
        off / 3600,
        (off % 3600) / 60
    )?;
    Ok(())
}

/// Indent a commit message four spaces per line, exactly as git's `pp_remainder`:
/// every line — blank ones included — is prefixed, and trailing blank lines are
/// dropped.
///
/// `tab_width` is git's `expand_tabs_in_log`, which its format table sets to 8
/// for the formats that indent (`medium`, `full`, `fuller`) and 0 for `raw`. A
/// tab inside a commit message was written against the message's own left edge,
/// so a four-space indent would shift every tab stop and misalign whatever the
/// author lined up; git expands the tabs instead, and the columns survive.
pub(super) fn indent_message(out: &mut Vec<u8>, msg: &[u8], tab_width: usize) {
    let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').collect();
    while lines.last() == Some(&&b""[..]) {
        lines.pop();
    }
    for line in lines {
        out.extend_from_slice(b"    ");
        if tab_width == 0 {
            out.extend_from_slice(line);
        } else {
            expand_tabs(out, line, tab_width);
        }
        out.push(b'\n');
    }
}

/// git's `strbuf_add_tabexpand`: replace each tab with spaces up to the next tab
/// stop, measuring columns from the START OF THE LINE — the indent the caller
/// already wrote does not count, which is what keeps a message's internal
/// alignment intact.
///
/// Width is display width, so a wide character occupies two columns. A segment
/// that is not valid UTF-8 cannot be measured, and git stops expanding that line
/// and copies the rest verbatim rather than guessing.
fn expand_tabs(out: &mut Vec<u8>, line: &[u8], tab_width: usize) {
    let mut rest = line;
    let mut column = 0usize;
    while let Some(at) = memchr::memchr(b'\t', rest) {
        let Ok(text) = std::str::from_utf8(&rest[..at]) else {
            break;
        };
        column += unicode_width::UnicodeWidthStr::width(text);
        out.extend_from_slice(&rest[..at]);
        out.extend(std::iter::repeat_n(b' ', tab_width - (column % tab_width)));
        column += tab_width - (column % tab_width);
        rest = &rest[at + 1..];
    }
    out.extend_from_slice(rest);
}

// ---------------------------------------------------------------------------
// Per-commit diff
// ---------------------------------------------------------------------------

/// One changed path, with the line counts `--stat` needs.
struct FileChange {
    path: Vec<u8>,
    status: u8,
    added: usize,
    deleted: usize,
    is_binary: bool,
    old_size: usize,
    new_size: usize,
    /// The path the content came from, for a rename; `None` for everything else.
    source: Option<Vec<u8>>,
    /// `similarity_index()` in percent, meaningless without `source`.
    score: u32,
    /// `(mode, object id)` of each side, kept for the rename pass; `None` where the
    /// path does not exist. Not part of the cached record — by the time a change list
    /// is cached the renames are already resolved.
    old_side: Option<(u32, ObjectId)>,
    new_side: Option<(u32, ObjectId)>,
}

/// Diff `commit`'s tree against `parent`'s (or the empty tree for a root commit),
/// dropping the directory entries gix reports alongside the files it recurses into.
/// Blob contents are only read when `with_counts` is set, which is the only case
/// that needs them.
/// Fill the ledger's log caches for the newest `limit` commits reachable from
/// `HEAD`, and report how many commits were covered.
///
/// This is what the daemon calls after a watched repo's refs move. Everything it
/// computes is a pure function of immutable objects — an abbreviation is fixed
/// once the object exists, and a tree pair's change list and line tallies never
/// expire — so the work is valid forever and can be done before anyone asks for
/// it. That is the part git has no way to do: it has no process alive between
/// commands, so the first `log --stat` after a pull always pays full price.
///
/// Bounded by `limit` because only the recent end of a history is ever read
/// interactively, and each pass is a fresh walk from the new tip. Failures are
/// silent by design: a warmed cache is an optimization, and the verb that missed
/// it simply computes the value itself.
pub fn warm_caches(repo: &gix::Repository, limit: usize) -> usize {
    let Ok(head) = repo.head_commit() else { return 0 };
    let mut abbrev = AbbrevCache::new(repo);
    let mut warmed = 0usize;
    let Ok(walk) = repo.rev_walk([head.id]).all() else { return 0 };
    for info in walk.take(limit).flatten() {
        let Ok(commit) = repo.find_commit(info.id) else { continue };
        abbrev.get(commit.id());
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        for parent in &parents {
            abbrev.get(parent.attach(repo));
        }
        // git shows no diff for a merge, so nothing would ever read its tallies.
        if parents.len() < 2 {
            // Stores into the tree-diff cache as a side effect (see
            // `collect_changes`), with the counts every `--stat`-style format
            // needs — the expensive half, one blob read per changed file.
            let _ = collect_changes(repo, &commit, parents.first().copied(), true, super::diff::Whitespace::Keep, None, None, None);
        }
        warmed += 1;
    }
    abbrev.flush();
    warmed
}

/// Record what a commit's rename pass reported into the command-wide slot
/// `diff_result_code()` will read.
///
/// `cmd_log_walk_no_free()` does not report what the *last* commit's rename pass
/// left in `rev->diffopt`; it accumulates across the walk into two locals and
/// writes them back once the walk is over (builtin/log.c:402-403, 435-441):
///
/// ```c
/// int saved_nrl = 0, saved_dcctc = 0;
/// ...
///     if (saved_nrl < rev->diffopt.needed_rename_limit)
///             saved_nrl = rev->diffopt.needed_rename_limit;
///     if (rev->diffopt.degraded_cc_to_c)
///             saved_dcctc = 1;
/// }
/// rev->diffopt.degraded_cc_to_c = saved_dcctc;
/// rev->diffopt.needed_rename_limit = saved_nrl;
/// ```
///
/// So the reported limit is the **maximum** any commit in the walk needed, and
/// `degraded_cc_to_c` is a sticky OR. That aggregation is also what makes the
/// per-commit bookkeeping unobservable: `too_many_rename_candidates()` zeroes
/// `needed_rename_limit` on entry (diffcore-rename.c:1092) and is only reached when
/// the pass still has both sources and destinations, so a commit that skipped the
/// check leaves the previous commit's value in the field — but that value is
/// already in `saved_nrl`, so re-reading it cannot raise the maximum. A commit that
/// reached the check and came in under the limit contributes 0, which cannot raise
/// it either. Both cases are therefore indistinguishable *and* correct, which is
/// why no "the check ran" flag on `Warnings` is needed to reproduce git here.
fn record_rename_warnings(
    slot: &mut super::diffcore_rename::Warnings,
    reported: super::diffcore_rename::Warnings,
) {
    slot.needed_rename_limit = slot.needed_rename_limit.max(reported.needed_rename_limit);
    slot.degraded_cc_to_c |= reported.degraded_cc_to_c;
}

fn collect_changes(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    with_counts: bool,
    // `-w`/`-b`/`--ignore-space-at-eol`: the tallies are computed the way the patch
    // would compute them, and the cache is bypassed so a whitespace-insensitive run
    // never stores its counts under the plain key.
    ws: super::diff::Whitespace,
    // `diffcore_rename()`: `None` for `--follow`, which runs its own rename search
    // over the raw change list; otherwise the settings the command was given.
    detect: Option<&super::diff::PatchOpts>,
    // `-- <pathspec>`: git limits the *tree diff* to it, so `diffcore_rename()` only
    // ever sees the entries that matched. Filtering afterwards instead lets a rename
    // pair a matched deletion with an unmatched addition and then drop the pair for
    // its destination, which is how `log --name-status -- a.txt` lost the `D a.txt`
    // of the commit that renamed the file away.
    limit: Option<&mut PathspecMatcher>,
    // `opt->diffopt.needed_rename_limit` / `degraded_cc_to_c`: fields of the one
    // `diff_options` the command owns, so each commit's rename pass overwrites the
    // last and `diff_result_code()` reports what the final one left behind.
    warn: Option<&mut super::diffcore_rename::Warnings>,
) -> Result<Vec<FileChange>> {
    let mut warn = warn;
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };

    // A tree-to-tree diff is a pure function of two immutable trees, so the file
    // list — and the per-file line tallies, which cost a blob read each — are
    // memoised exactly as blame is. `--stat` over a range re-diffs the same
    // parent/child pairs on every invocation; git does too, but this sidesteps
    // the work instead of racing it.
    let old_key = old_tree.as_ref().map(|t| t.id.to_string()).unwrap_or_default();
    let new_key = new_tree.id.to_string();
    // The cached list is the raw one the tree walk produced; rename detection runs on
    // the way out, so `--follow` (which does its own rename search on the raw list) and
    // the reporting formats can share one cache entry.
    let cacheable = ws == super::diff::Whitespace::Keep;
    if let Some(text) = cacheable
        .then(|| crate::rcache::treediff_load(&old_key, &new_key, with_counts))
        .flatten()
    {
        if let Some(mut files) = decode_changes(text) {
            if let Some(m) = limit {
                files.retain(|f| m.matches(&f.path));
            }
            if let Some(opts) = detect {
                let w = detect_renames(repo, &mut files, with_counts, opts)?;
                if let Some(slot) = warn.as_deref_mut() {
                    record_rename_warnings(slot, w);
                }
            }
            return Ok(files);
        }
    }

    let mut changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    let mut out = Vec::with_capacity(changes.len());
    for change in &changes {
        if let Some(f) = prepare_change(repo, change, with_counts, ws)? {
            out.push(f);
        }
    }
    // Off-thread: the answer is already in `out`, so the row is bookkeeping and
    // the caller must not wait for a transaction to reach the disk.
    if cacheable {
        crate::rcache::cache_write(crate::rcache::CacheWrite::TreeDiff {
            old_tree: old_key,
            new_tree: new_key,
            counts: with_counts,
            files: encode_changes(&out),
        });
    }
    if let Some(m) = limit {
        out.retain(|f| m.matches(&f.path));
    }
    if let Some(opts) = detect {
        let w = detect_renames(repo, &mut out, with_counts, opts)?;
        if let Some(slot) = warn.as_deref_mut() {
            record_rename_warnings(slot, w);
        }
    }
    Ok(out)
}

/// `diffcore_rename()`: pair each deletion with an addition carrying the same (or
/// similar enough) content, so a moved file is one `R` entry instead of a `D` and an
/// `A`. `git log` is a porcelain, so detection is on unless `diff.renames` says
/// otherwise, at git's default 50% similarity.
///
/// This is the same port `git diff` runs, so the pairing and the similarity indices
/// agree across the commands.
fn detect_renames(
    repo: &gix::Repository,
    files: &mut Vec<FileChange>,
    with_counts: bool,
    opts: &super::diff::PatchOpts,
) -> Result<super::diffcore_rename::Warnings> {
    let cfg = repo.config_snapshot();
    let detect = opts.renames.unwrap_or_else(|| super::diffcore_rename::config_rename(
        cfg.string("diff.renames").as_deref().map(|v| v.as_bstr()),
    ));
    // `-B` runs on its own: `diffcore_std()` breaks rewrites whether or not a rename
    // pass follows, so the early exit only applies when neither is asked for.
    let wants_break = opts.break_opt != -1;
    if (detect == 0 && !wants_break)
        || (!wants_break && !files.iter().any(|f| f.status == b'A' || f.status == b'D'))
    {
        return Ok(super::diffcore_rename::Warnings::default());
    }
    let ws = opts.ws;
    let opts = super::diffcore_rename::Options {
        detect_rename: detect,
        rename_score: opts.rename_score,
        find_copies_harder: opts.find_copies_harder,
        break_opt: opts.break_opt,
        rename_empty: opts.rename_empty,
        rename_limit: cfg
            .integer("diff.renameLimit")
            .unwrap_or(super::diffcore_rename::DEFAULT_RENAME_LIMIT),
        hash_kind: repo.object_hash(),
        ..Default::default()
    };

    // The queue needs object ids, which the change list does not keep; they are read
    // back from the two trees by path, which is what the ids were taken from.
    let mut q = super::diffcore_rename::Queue::default();
    for f in files.iter() {
        let (old_mode, old_id) = f.old_side.unwrap_or((0, ObjectId::null(repo.object_hash())));
        let (new_mode, new_id) = f.new_side.unwrap_or((0, ObjectId::null(repo.object_hash())));
        let one = q.add_spec(super::diffcore_rename::FileSpec::new(
            f.path.clone().into(),
            old_mode,
            old_id,
            old_mode != 0,
        ));
        let two = q.add_spec(super::diffcore_rename::FileSpec::new(
            f.path.clone().into(),
            new_mode,
            new_id,
            new_mode != 0,
        ));
        let idx = q.add_pair(one, two);
        q.pairs[idx].status = f.status;
    }

    let mut content = super::diffcore_rename::OdbContent { repo };
    // `too_many_rename_candidates()` records the limit this pass would have needed
    // in `opt->diffopt`; `diff_result_code()` prints it once when the walk ends.
    let warnings = super::diffcore_rename::run(&mut q, &opts, &mut content);
    super::diffcore_rename::resolve_rename_copy(&mut q);

    let mut rebuilt: Vec<FileChange> = Vec::with_capacity(q.pairs.len());
    for pair in &q.pairs {
        let source = &q.specs[pair.one];
        let dest = &q.specs[pair.two];
        let status = if pair.status == 0 { b'M' } else { pair.status };
        if !matches!(status, b'R' | b'C') {
            // Not a rename: the entry this pair was built from already has its
            // contents and counts, and both of its sides carry that one path.
            // A `-B` rewrite that stayed a modification carries a score, which
            // `--summary` prints as its ` rewrite ... (n%)` line.
            if let Some(at) = files.iter().position(|f| f.path == dest.path.as_slice()) {
                let mut kept = files.swap_remove(at);
                kept.score = super::diffcore_rename::similarity_index(pair.score);
                rebuilt.push(kept);
            }
            continue;
        }
        let mut f = FileChange {
            path: dest.path.to_vec(),
            status,
            added: 0,
            deleted: 0,
            is_binary: false,
            old_size: 0,
            new_size: 0,
            source: Some(source.path.to_vec()),
            score: super::diffcore_rename::similarity_index(pair.score),
            old_side: Some((source.mode, source.oid)),
            new_side: Some((dest.mode, dest.oid)),
        };
        if with_counts {
            let old_is_sub = source.mode & 0o170000 == 0o160000;
            let new_is_sub = dest.mode & 0o170000 == 0o160000;
            let old_content = content_of(repo, source.oid, old_is_sub)?;
            let new_content = content_of(repo, dest.oid, new_is_sub)?;
            f.old_size = old_content.len();
            f.new_size = new_content.len();
            f.is_binary = is_binary(&old_content) || is_binary(&new_content);
            if !f.is_binary && source.oid != dest.oid {
                let (added, deleted) =
                    count_changed_lines_ws(&old_content, &new_content, ws)?;
                f.added = added;
                f.deleted = deleted;
            }
        }
        rebuilt.push(f);
    }
    rebuilt.sort_by(|a, b| a.path.cmp(&b.path));
    *files = rebuilt;
    Ok(warnings)
}

/// Encode a change list for the ledger: one record per file,
/// `status,added,deleted,binary,old_size,new_size,path`, NUL-separated so a path
/// containing any printable byte survives the round trip.
fn encode_changes(files: &[FileChange]) -> String {
    files
        .iter()
        .map(|f| {
            format!(
                "{},{},{},{},{},{},{},{},{},{},{}",
                f.status as char,
                f.added,
                f.deleted,
                u8::from(f.is_binary),
                f.old_size,
                f.new_size,
                f.old_side.map(|(mode, _)| mode).unwrap_or(0),
                f.old_side.map(|(_, id)| id.to_hex().to_string()).unwrap_or_default(),
                f.new_side.map(|(mode, _)| mode).unwrap_or(0),
                f.new_side.map(|(_, id)| id.to_hex().to_string()).unwrap_or_default(),
                String::from_utf8_lossy(&f.path)
            )
        })
        .collect::<Vec<_>>()
        .join("\0")
}

/// Decode what [`encode_changes`] wrote. `None` for a malformed record, so a
/// damaged row falls back to a real diff rather than a wrong answer.
fn decode_changes(text: &str) -> Option<Vec<FileChange>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for rec in text.split('\0') {
        let mut f = rec.splitn(11, ',');
        let status = f.next()?.bytes().next()?;
        let added: usize = f.next()?.parse().ok()?;
        let deleted: usize = f.next()?.parse().ok()?;
        let is_binary = f.next()? == "1";
        let old_size: usize = f.next()?.parse().ok()?;
        let new_size: usize = f.next()?.parse().ok()?;
        // The two sides feed the rename pass, which runs after the cache is consulted.
        // A record written before they were stored has too few fields and is rejected
        // here, which sends the caller back to a real diff.
        let side = |mode: &str, id: &str| -> Option<Option<(u32, ObjectId)>> {
            let mode: u32 = mode.parse().ok()?;
            if mode == 0 {
                return Some(None);
            }
            Some(Some((mode, ObjectId::from_hex(id.as_bytes()).ok()?)))
        };
        let old_side = side(f.next()?, f.next()?)?;
        let new_side = side(f.next()?, f.next()?)?;
        let path = f.next()?.as_bytes().to_vec();
        out.push(FileChange {
            path,
            status,
            added,
            deleted,
            is_binary,
            old_size,
            new_size,
            source: None,
            score: 0,
            old_side,
            new_side,
        });
    }
    Some(out)
}

/// Whether the diff between `commit` and `parent` (the empty tree when `None`)
/// touches any of the pathspecs — git's TREESAME test, negated.
/// The name the followed path arrived from, when this commit renamed it —
/// `try_to_follow_renames()` reduced to what `--follow` needs: the path must be new
/// in this commit, and the source is the deletion whose content is most similar.
///
/// git runs its full `diffcore_rename` here (exact matches first, then the 50%
/// similarity pass). The exact pass is reproduced faithfully; the inexact one uses
/// the same `diffcore_count_changes()` estimator, so it agrees on the score but
/// picks its own winner when several deletions score alike.
fn follow_source(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: ObjectId,
    path: &gix::bstr::BString,
) -> Result<Option<gix::bstr::BString>> {
    let files = collect_changes(repo, commit, Some(parent), false, super::diff::Whitespace::Keep, None, None, None)?;
    // The followed path has to be an addition here; anything else is not a rename.
    if !files.iter().any(|f| f.path.as_slice() == path.as_slice() && f.status == b'A') {
        return Ok(None);
    }
    let new_tree = commit.tree()?;
    let old_tree = repo.find_commit(parent)?.tree()?;
    let blob = |tree: &gix::Tree<'_>, p: &gix::bstr::BString| -> Result<Option<(ObjectId, Vec<u8>)>> {
        let Some(entry) = tree.lookup_entry_by_path(gix::path::from_bstr(p.as_bstr()))? else {
            return Ok(None);
        };
        let id = entry.object_id();
        Ok(Some((id, repo.find_object(id)?.detach().data)))
    };
    let Some((new_id, new_bytes)) = blob(&new_tree, path)? else {
        return Ok(None);
    };

    let mut best: Option<(f64, gix::bstr::BString)> = None;
    for f in &files {
        if f.status != b'D' {
            continue;
        }
        let old_name = gix::bstr::BString::from(f.path.clone());
        let Some((old_id, old_bytes)) = blob(&old_tree, &old_name)? else {
            continue;
        };
        // `find_exact_renames()`: an identical blob is a rename outright.
        if old_id == new_id {
            return Ok(Some(old_name));
        }
        let score = similarity_score(&old_bytes, &new_bytes);
        // `DEFAULT_RENAME_SCORE`: half the content has to survive.
        if score >= super::diffcore_rename::MAX_SCORE / 2.0
            && best.as_ref().is_none_or(|(b, _)| score > *b)
        {
            best = Some((score, old_name));
        }
    }
    Ok(best.map(|(_, p)| p))
}

/// `estimate_similarity()` (diffcore-rename.c): how much of `old` survives in
/// `new`, in `MAX_SCORE` units, off the same chunk-hash counter rename detection
/// uses everywhere else.
fn similarity_score(old: &[u8], new: &[u8]) -> f64 {
    if old.is_empty() && new.is_empty() {
        return super::diffcore_rename::MAX_SCORE;
    }
    let max = old.len().max(new.len()) as f64;
    if max == 0.0 {
        return 0.0;
    }
    let (copied, _added) = super::diff_files::count_changes_sides(old, true, new, true);
    (copied as f64 * super::diffcore_rename::MAX_SCORE) / max
}

fn changes_match(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    specs: &mut PathspecMatcher,
) -> Result<bool> {
    let files = collect_changes(repo, commit, parent, false, super::diff::Whitespace::Keep, None, None, None)?;
    Ok(files.iter().any(|f| specs.matches(&f.path)))
}

/// A parsed `-- <pathspec>...` set, matched by git's real pathspec engine.
///
/// Built once per spec list and then asked about one path at a time. A set is not
/// the same thing as a list of independent patterns, which is why this is a type
/// and not a `matches(spec, path)` function: `:(exclude)`/`:!` *subtracts* from
/// what the positive specs select, and a set of nothing but exclusions selects
/// everything they do not name — neither is expressible as "does any one spec
/// match". The rest of the magic grammar (`:(glob)`, `:(icase)`, `:(literal)`,
/// `:(top)`, `:(attr:…)`) comes along with it.
///
/// This is `repo.pathspec()`, the same engine `add`, `grep`, `rm`, `ls-files`,
/// `stage` and `status` match with, so a pathspec means one thing across the whole
/// binary. Specs are resolved against the repository prefix, so they are relative
/// to the current directory exactly as git's are.
///
/// Matching takes `&self`: gix wants `&mut` for the attribute stack a `:(attr:…)`
/// spec consults, but a pathspec set is logically a predicate, and half the callers
/// ask from inside a `retain`/`remove_entries` closure that cannot hold a mutable
/// borrow. The cell keeps that detail here instead of spreading it over every one
/// of them.
pub(crate) struct PathspecMatcher {
    inner: std::cell::RefCell<gix::PathspecDetached>,
}

impl PathspecMatcher {
    /// Parse `specs` for `repo`. Callers skip matching altogether for an empty
    /// list — git treats "no pathspec" as "no limiting", not as a set that matches
    /// nothing — so this is only ever handed a non-empty one.
    pub(crate) fn new<S: AsRef<[u8]>>(repo: &gix::Repository, specs: &[S]) -> Result<Self> {
        // git's `parse_pathspec()` runs over the whole list before the command does
        // anything, and every way it can fail is a `die()`. gitoxide raises the same
        // failures from inside the constructor below, where `?` would render them in
        // this port's voice at exit 1 — so the list is parsed here first, and the
        // first bad element reported as git reports it. Same `Defaults` as the real
        // matcher (`inherit_ignore_case: false` below), so acceptance cannot diverge.
        let defaults = repo.pathspec_defaults_inherit_ignore_case(false)?;
        if let Some(msg) = crate::pathspec::first_magic_fatal(specs, defaults) {
            return Err(crate::fatal::die(msg));
        }
        // `init_pathspec_item()`'s second `die()`, once the magic is off and the
        // path itself is normalized against the prefix. gitoxide raises it from
        // inside the constructor below, where `?` would render it in this port's
        // voice at exit 1 — `git log -- ..` is `fatal: ..: '..' is outside
        // repository at '<worktree>'` and exit 128.
        if let Some(msg) = crate::pathspec::first_outside_repository_fatal(repo, specs, defaults) {
            return Err(crate::fatal::die(msg));
        }
        // `IdMapping` reads `.gitattributes` from the index, and is only consulted
        // at all when a spec carries `:(attr:…)`.
        let index = repo.index_or_empty()?;
        let inner = repo
            .pathspec(
                true,
                specs.iter().map(|s| gix::bstr::BStr::new(s.as_ref())),
                false,
                &index,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )?
            .detach()?;
        Ok(Self { inner: std::cell::RefCell::new(inner) })
    }

    /// Is this repo-relative file path in the set?
    pub(crate) fn matches(&self, path: &[u8]) -> bool {
        self.inner.borrow_mut().is_included(path.as_bstr(), Some(false))
    }

    /// Is this repo-relative *directory* itself selected, or could something under
    /// it be? This is git's `tree_entry_interesting()` for a tree entry: a walk has
    /// to descend into `src` to reach `src/gen/table.rs`, and `ls-tree` without
    /// `-r` reports the tree itself for a spec that lives below it. A wildcard has
    /// no literal prefix to test, so `can_match_relative_path` answers on the
    /// shortest shared prefix and errs towards descending.
    pub(crate) fn may_contain_match(&self, dir: &[u8]) -> bool {
        let mut inner = self.inner.borrow_mut();
        inner.is_included(dir.as_bstr(), Some(true))
            || inner.search.can_match_relative_path(dir.as_bstr(), Some(true))
    }

    /// Should this directory be *reported* as an entry in its own right?
    ///
    /// Narrower than [`Self::may_contain_match`], which answers "is it worth
    /// descending". git reports a tree for a spec that names it, and for one that
    /// points at a literal path inside it — `diff-tree -- d1/sub` without `-r`
    /// still lists `d1`. A wildcard names no such path, so `-- '*.rs'` lists no
    /// trees at all even though the walk has to descend through them.
    pub(crate) fn selects_dir(&self, dir: &[u8]) -> bool {
        if self.inner.borrow_mut().is_included(dir.as_bstr(), Some(true)) {
            return true;
        }
        let inner = self.inner.borrow();
        let named_below = inner.search.patterns().any(|p| {
            let path = p.path();
            !p.is_excluded()
                && path.len() > dir.len()
                && path.starts_with(dir)
                && path[dir.len()] == b'/'
        });
        named_below
    }
}

/// Glob match for a plain (non-magic) pathspec, delegating to the faithful
/// `wildmatch.c:dowild` port below. Only git's `WM_MATCH` counts as a match, so a
/// malformed pattern (`WM_ABORT_ALL`) is reported as no-match, exactly as git's
/// pathspec callers treat `wildmatch(...) != 0`.
pub(crate) fn wildmatch(pat: &[u8], text: &[u8]) -> bool {
    matches!(dowild(pat, text), Wm::Match)
}

/// Return states of git's `wildmatch.c:dowild`, specialised to the `flags == 0`
/// case a non-magic ("plain") git pathspec uses: `dir.c:git_fnmatch` calls
/// `wildmatch(pattern, string, 0)` for a pathspec without `:(glob)`/`:(icase)`
/// magic (dir.c: "wildmatch has not learned no FNM_PATHNAME mode yet"). With
/// `flags == 0` there is no `WM_PATHNAME` (so `*`/`?`/`[…]` all span `/`) and no
/// `WM_CASEFOLD`; that also means the `WM_ABORT_TO_STARSTAR` state cannot arise
/// (it needs `match_slash == 0`, but here `*` behaves as `**`), so only these
/// three outcomes remain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wm {
    /// `WM_MATCH`.
    Match,
    /// `WM_NOMATCH`.
    NoMatch,
    /// `WM_ABORT_ALL`: text ended with the pattern still expecting a literal, or a
    /// malformed bracket expression. A no-match at the top level.
    AbortAll,
}

/// The ref-selecting pseudo-option `a` spells, together with its `=<glob>`.
///
/// `--glob` is the one spelling whose value may also stand as the next argv
/// element (`parse_long_opt()`), which the caller resolves; the rest take their
/// pattern attached or not at all.
pub(crate) fn ref_selector(a: &str) -> Option<(RefSelector, Option<&str>)> {
    let (name, pattern) = match a.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (a, None),
    };
    let selector = match name {
        "--all" if pattern.is_none() => RefSelector::All,
        "--branches" => RefSelector::Branches,
        "--tags" => RefSelector::Tags,
        "--remotes" => RefSelector::Remotes,
        "--glob" => RefSelector::Glob,
        _ => return None,
    };
    Some((selector, pattern))
}

/// Which ref-selecting pseudo-option a [`RefSelection`] came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefSelector {
    /// `--all`: every ref under `refs/`, then `HEAD`.
    All,
    /// `--branches[=<glob>]`.
    Branches,
    /// `--tags[=<glob>]`.
    Tags,
    /// `--remotes[=<glob>]`.
    Remotes,
    /// `--glob=<glob>`: no namespace of its own, the pattern carries it.
    Glob,
}

/// One ref-selecting pseudo-option, resolved to the shape
/// `refs_for_each_ref_ext()` iterates with.
///
/// `handle_revision_pseudo_opt()` (revision.c) turns each of `--all`,
/// `--branches`, `--tags`, `--remotes` and `--glob` into one such iteration and
/// pends every ref it yields. The command-line position is part of the record
/// because `setup_revisions()` appends to a single `pending` list as it reads:
/// that order is what `--no-walk=unsorted` prints and what breaks a commit-date
/// tie between two tips.
pub(crate) struct RefSelection {
    /// How many revision arguments preceded it — its slot in the pending list.
    pub(crate) at: usize,
    /// `refs_for_each_ref_options.prefix`: the namespace iterated. Empty for
    /// `--all` and `--glob`, which see every ref.
    prefix: &'static str,
    /// `refs_for_each_ref_options.trim_prefix`: how much of the front of a
    /// refname `handle_one_ref()` never sees. The trimmed name is what
    /// `--source` prints and what an `--exclude` pattern is matched against.
    trim: usize,
    /// `real_pattern` exactly as `refs_for_each_ref_ext()` builds it
    /// (`refs.c:1903-1930`). `None` selects the whole namespace.
    pattern: Option<String>,
    /// `--all` alone also pends `HEAD`, after the ref list (`refs_head_ref`).
    pub(crate) head: bool,
    /// The `--exclude` patterns in force when this selection consumed them.
    /// `handle_one_ref()` drops a ref whose (trimmed) name any of them matches.
    excludes: Vec<String>,
    /// `--not` in force, which hands `handle_refs()` the `UNINTERESTING` flag.
    pub(crate) negated: bool,
}

impl RefSelection {
    /// Build the selection `handle_revision_pseudo_opt()` would install.
    ///
    /// The pattern construction is `refs.c:1903-1930`: the namespace prefix (or
    /// a bare `refs/` for a `--glob` pattern that lacks one) is prepended, and a
    /// pattern holding none of `?`, `*` or `[` gains a trailing `/` (if it has
    /// none) plus `*` — which is why `--branches=topic` selects the branches
    /// *below* `topic/` rather than `topic` itself.
    pub(crate) fn new(
        at: usize,
        selector: RefSelector,
        pattern: Option<&str>,
        excludes: Vec<String>,
        negated: bool,
    ) -> Self {
        let prefix = match selector {
            RefSelector::All | RefSelector::Glob => "",
            RefSelector::Branches => "refs/heads/",
            RefSelector::Tags => "refs/tags/",
            RefSelector::Remotes => "refs/remotes/",
        };
        let pattern = pattern.map(|p| {
            let mut real = String::new();
            if prefix.is_empty() {
                if !p.starts_with("refs/") {
                    real.push_str("refs/");
                }
            } else {
                real.push_str(prefix);
            }
            real.push_str(p);
            // `if (!has_glob_specials(opts->pattern))` — tested against the
            // pattern as written, not against the prefixed form.
            if !p.contains(['?', '*', '[']) {
                if !real.ends_with('/') {
                    real.push('/');
                }
                real.push('*');
            }
            real
        });
        RefSelection {
            at,
            prefix,
            trim: prefix.len(),
            pattern,
            head: selector == RefSelector::All,
            excludes,
            negated,
        }
    }

    /// The name `handle_one_ref()` would be handed for `full`, or `None` when
    /// this selection does not yield that ref.
    ///
    /// The pattern is matched against the *whole* refname with `wildmatch(…, 0)`
    /// (`refs.c:475-490`) — no `WM_PATHNAME`, so a `*` crosses `/` and
    /// `--remotes=origin*` really does reach `origin/main`. Trimming happens
    /// after the match, and `ref_excluded()` (revision.c:1551-1566) then tests
    /// the trimmed name, full-string, with no implicit `/*` of its own.
    pub(crate) fn selects<'a>(&self, full: &'a str) -> Option<&'a str> {
        if !full.starts_with(self.prefix) {
            return None;
        }
        if let Some(pattern) = &self.pattern {
            if !wildmatch(pattern.as_bytes(), full.as_bytes()) {
                return None;
            }
        }
        let name = &full[self.trim..];
        if self.excluded(name) {
            return None;
        }
        Some(name)
    }

    /// `ref_excluded()` for a name already trimmed by [`Self::selects`]. `--all`
    /// pends `HEAD` through the same test, under that literal name.
    pub(crate) fn excluded(&self, name: &str) -> bool {
        self.excludes
            .iter()
            .any(|p| wildmatch(p.as_bytes(), name.as_bytes()))
    }
}

/// git's `is_glob_special`: the bytes `wildmatch` treats as metacharacters.
fn is_glob_special(c: u8) -> bool {
    matches!(c, b'*' | b'?' | b'[' | b'\\')
}

/// `pat.get(i)` as a byte, using `0` (git's NUL terminator) past the end.
fn at(pat: &[u8], i: usize) -> u8 {
    pat.get(i).copied().unwrap_or(0)
}

/// Faithful port of `wildmatch.c:dowild` for `flags == 0` (see [`Wm`]). Matches
/// pattern `pat` against `text`.
fn dowild(pat: &[u8], text: &[u8]) -> Wm {
    let mut p = 0usize;
    let mut t = 0usize;
    while p < pat.len() {
        let mut p_ch = pat[p];
        let t_ch = if t < text.len() { text[t] } else { 0 };
        // `if ((t_ch = *text) == '\0' && p_ch != '*') return WM_ABORT_ALL;`
        if t_ch == 0 && p_ch != b'*' {
            return Wm::AbortAll;
        }
        match p_ch {
            // `case '?'`: flags=0 matches any char, `/` included.
            b'?' => {}
            b'*' => {
                // Collapse a run of `*`; with flags=0, `*` behaves as `**`
                // (`match_slash` is always true).
                p += 1;
                while p < pat.len() && pat[p] == b'*' {
                    p += 1;
                }
                // Trailing `*`/`**` matches the remaining text unconditionally.
                if p >= pat.len() {
                    return Wm::Match;
                }
                loop {
                    if t >= text.len() {
                        break;
                    }
                    // When the char after `*` is a literal, fast-forward the text to
                    // it: everything skipped must belong to the `*`.
                    if !is_glob_special(pat[p]) {
                        let lit = pat[p];
                        while t < text.len() && text[t] != lit {
                            t += 1;
                        }
                        if t >= text.len() {
                            return Wm::NoMatch;
                        }
                    }
                    match dowild(&pat[p..], &text[t..]) {
                        Wm::NoMatch => {}
                        other => return other,
                    }
                    t += 1;
                }
                return Wm::AbortAll;
            }
            b'[' => match bracket(pat, &mut p, t_ch) {
                // On a match `p` is left on the `]`; the advance below steps past it.
                Wm::Match => {}
                nonmatch => return nonmatch,
            },
            b'\\' => {
                // Literal match with the following char. `p[1] == '\0'` falls out as
                // `p_ch == 0`, which the `t_ch != p_ch` test rejects (t_ch != 0
                // here), exactly as git's `default` arm handles it.
                p += 1;
                p_ch = at(pat, p);
                if t_ch != p_ch {
                    return Wm::NoMatch;
                }
            }
            _ => {
                if t_ch != p_ch {
                    return Wm::NoMatch;
                }
            }
        }
        p += 1;
        t += 1;
    }
    if t < text.len() {
        Wm::NoMatch
    } else {
        Wm::Match
    }
}

/// Port of the `case '['` block of `wildmatch.c:dowild` (flags=0). `*p` enters on
/// the `[` and, on a match/no-match decision, is left on the closing `]` so the
/// caller's single advance steps past it. Returns [`Wm::AbortAll`] for a malformed
/// class (missing `]`), matching git.
fn bracket(pat: &[u8], p: &mut usize, t_ch: u8) -> Wm {
    // `p_ch = *++p`
    *p += 1;
    let mut p_ch = at(pat, *p);
    // NEGATE_CLASS2 `^` is normalised to NEGATE_CLASS `!`.
    if p_ch == b'^' {
        p_ch = b'!';
    }
    let negated = p_ch == b'!';
    if negated {
        *p += 1;
        p_ch = at(pat, *p);
    }
    let mut prev_ch: u8 = 0;
    let mut matched = false;
    loop {
        if p_ch == 0 {
            return Wm::AbortAll;
        }
        if p_ch == b'\\' {
            *p += 1;
            p_ch = at(pat, *p);
            if p_ch == 0 {
                return Wm::AbortAll;
            }
            if t_ch == p_ch {
                matched = true;
            }
        } else if p_ch == b'-' && prev_ch != 0 && at(pat, *p + 1) != 0 && at(pat, *p + 1) != b']' {
            // `prev_ch`..`p_ch` inclusive range.
            *p += 1;
            p_ch = at(pat, *p);
            if p_ch == b'\\' {
                *p += 1;
                p_ch = at(pat, *p);
                if p_ch == 0 {
                    return Wm::AbortAll;
                }
            }
            if t_ch <= p_ch && t_ch >= prev_ch {
                matched = true;
            }
            p_ch = 0; // makes prev_ch get set to 0 next iteration
        } else if p_ch == b'[' && at(pat, *p + 1) == b':' {
            // POSIX `[:class:]`.
            *p += 2;
            let s = *p;
            while at(pat, *p) != 0 && at(pat, *p) != b']' {
                *p += 1;
            }
            if at(pat, *p) == 0 {
                return Wm::AbortAll;
            }
            // `*p` is now on `]`; the class name is `pat[s..*p-1]` and `pat[*p-1]`
            // must be `:`. `i < 0` in git corresponds to `*p <= s` here.
            if *p <= s || pat[*p - 1] != b':' {
                // Not a real `[:class:]`: treat the inner `[` as a literal member.
                *p = s - 2;
                p_ch = b'[';
                if t_ch == p_ch {
                    matched = true;
                }
            } else {
                match class_matches(&pat[s..*p - 1], t_ch) {
                    Some(true) => matched = true,
                    Some(false) => {}
                    // Malformed `[:class:]` string.
                    None => return Wm::AbortAll,
                }
                p_ch = 0;
            }
        } else if t_ch == p_ch {
            matched = true;
        }
        // git's do-while tail: `prev_ch = p_ch, (p_ch = *++p) != ']'`.
        prev_ch = p_ch;
        *p += 1;
        p_ch = at(pat, *p);
        if p_ch == b']' {
            break;
        }
    }
    // `if (matched == negated) return WM_NOMATCH;` (the `WM_PATHNAME`/`'/'` guard
    // is inert at flags=0).
    if matched == negated {
        Wm::NoMatch
    } else {
        Wm::Match
    }
}

/// git's `wildmatch.c` POSIX character classes (`[:alpha:]`, `[:digit:]`, …),
/// evaluated for ASCII byte `c`. `None` marks a class name git rejects as a
/// malformed `[:class:]` string.
fn class_matches(name: &[u8], c: u8) -> Option<bool> {
    let m = match name {
        b"alnum" => c.is_ascii_alphanumeric(),
        b"alpha" => c.is_ascii_alphabetic(),
        b"blank" => c == b' ' || c == b'\t',
        b"cntrl" => c.is_ascii_control(),
        b"digit" => c.is_ascii_digit(),
        b"graph" => c.is_ascii_graphic(),
        b"lower" => c.is_ascii_lowercase(),
        // `isprint`: printable, space included.
        b"print" => (0x20..=0x7e).contains(&c),
        b"punct" => c.is_ascii_punctuation(),
        // C's `isspace`: space, `\t`, `\n`, `\v`, `\f`, `\r`.
        b"space" => matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'),
        b"upper" => c.is_ascii_uppercase(),
        b"xdigit" => c.is_ascii_hexdigit(),
        _ => return None,
    };
    Some(m)
}

/// Turn one gix change into a [`FileChange`], or `None` for the directory entries
/// git does not report (gix emits those *and* recurses into them).
fn prepare_change(
    repo: &gix::Repository,
    change: &ChangeDetached,
    with_counts: bool,
    ws: super::diff::Whitespace,
) -> Result<Option<FileChange>> {
    let (path, status, old, new) = match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            (
                location.to_vec(),
                b'A',
                None,
                Some((*id, u32::from(entry_mode.value()))),
            )
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            (
                location.to_vec(),
                b'D',
                Some((*id, u32::from(entry_mode.value()))),
                None,
            )
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            // A directory whose contents changed; the changed files themselves are
            // reported separately by the recursive walk.
            if entry_mode.is_tree() && previous_entry_mode.is_tree() {
                return Ok(None);
            }
            let status = if type_class(previous_entry_mode.kind()) == type_class(entry_mode.kind()) {
                b'M'
            } else {
                b'T'
            };
            (
                location.to_vec(),
                status,
                Some((*previous_id, u32::from(previous_entry_mode.value()))),
                Some((*id, u32::from(entry_mode.value()))),
            )
        }
        // Never produced: rewrite tracking is disabled via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    };

    let mut f = FileChange {
        path,
        status,
        added: 0,
        deleted: 0,
        is_binary: false,
        old_size: 0,
        new_size: 0,
        source: None,
        score: 0,
        old_side: old.map(|(id, mode)| (mode, id)),
        new_side: new.map(|(id, mode)| (mode, id)),
    };

    if with_counts {
        let is_sub = |mode: u32| mode & 0o170000 == 0o160000;
        let old_content = match old {
            Some((id, mode)) => content_of(repo, id, is_sub(mode))?,
            None => Vec::new(),
        };
        let new_content = match new {
            Some((id, mode)) => content_of(repo, id, is_sub(mode))?,
            None => Vec::new(),
        };
        f.old_size = old_content.len();
        f.new_size = new_content.len();
        f.is_binary = is_binary(&old_content) || is_binary(&new_content);
        let mode_only = matches!((old, new), (Some((a, _)), Some((b, _))) if a == b);
        if !f.is_binary && !mode_only {
            let (added, deleted) = count_changed_lines_ws(&old_content, &new_content, ws)?;
            f.added = added;
            f.deleted = deleted;
        }
    }
    Ok(Some(f))
}

/// git's status letters distinguish a change of file *type* (`T`) from a change of
/// contents or permissions (`M`); regular and executable files are the same type.
fn type_class(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Tree => 0,
        EntryKind::Blob | EntryKind::BlobExecutable => 1,
        EntryKind::Link => 2,
        EntryKind::Commit => 3,
    }
}

/// The path of a change, for stable diff ordering.
/// Is this change a *directory* entry rather than a file?
///
/// A recursive tree diff reports each containing directory alongside the files
/// inside it. Every pathspec test in git is file-granular, so a directory must
/// never be offered to one: under `:(exclude)src/gen` the parent `src` is not
/// excluded, and a commit whose only real change is `src/gen/table.rs` would be
/// kept on the strength of the `src` entry alone. [`prepare_change`] drops these
/// for the same reason.
pub(crate) fn change_is_tree(change: &ChangeDetached) -> bool {
    match change {
        ChangeDetached::Addition { entry_mode, .. }
        | ChangeDetached::Deletion { entry_mode, .. }
        | ChangeDetached::Modification { entry_mode, .. }
        | ChangeDetached::Rewrite { entry_mode, .. } => entry_mode.is_tree(),
    }
}

pub(crate) fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

// ---------------------------------------------------------------------------
// --stat
// ---------------------------------------------------------------------------

/// The rows [`super::diffstat::show_stats`] renders. A binary file's "counts" are
/// the two byte sizes, which is what `builtin_diffstat()` puts in `added`/`deleted`
/// for one.
fn stat_rows(files: &[FileChange], compact: bool, rel: &str) -> Vec<diffstat::StatFile> {
    files
        .iter()
        .map(|f| diffstat::StatFile {
            print_name: stat_name(f, compact, rel),
            added: if f.is_binary { f.new_size as u64 } else { f.added as u64 },
            deleted: if f.is_binary { f.old_size as u64 } else { f.deleted as u64 },
            binary: f.is_binary,
            // `log` walks committed trees, so no pair is ever unmerged.
            is_unmerged: false,
        })
        .collect()
}

/// git's `--stat` (`show_stats()`), rendered by the shared port.
fn emit_stat(
    out: &mut Vec<u8>,
    files: &[FileChange],
    sw: &StatWidths,
    compact: bool,
    rel: &str,
    colors: &diff_color::DiffColors,
) -> Result<()> {
    diffstat::show_stats(out, &stat_rows(files, compact, rel), sw, colors);
    Ok(())
}

/// `diff_flush_raw()`: `:<old mode> <new mode> <old sha> <new sha> <status>\t<path>`,
/// with a rename's similarity index after the status letter and its source path first.
/// An absent side is mode `000000` and an all-zero id padded to the same width.
fn emit_raw(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    files: &[FileChange],
    z: bool,
    rel: &str,
) -> Result<()> {
    // `-z`: the field and record separators are NUL and paths go out unquoted.
    let sep = if z { 0u8 } else { b'\t' };
    let end = if z { 0u8 } else { b'\n' };
    // `diff_aligned_abbrev()` abbreviates to `revs->abbrev`, which `--abbrev[=<n>]`
    // and `--no-abbrev` already wrote into this repository's `core.abbrev`.
    let abbrev =
        crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex()).max(MINIMUM_ABBREV);
    for f in files {
        let side = |s: Option<(u32, ObjectId)>| -> Result<(u32, String)> {
            let Some((mode, id)) = s else {
                return Ok((0, "0".repeat(abbrev)));
            };
            // A gitlink names a commit this odb does not have, so it cannot be
            // disambiguated against — git truncates it as-is.
            if mode & 0o170000 == 0o160000 {
                return Ok((mode, id.to_hex_with_len(abbrev).to_string()));
            }
            Ok((mode, id.attach(repo).shorten()?.to_string()))
        };
        let (old_mode, old_hex) = side(f.old_side)?;
        let (new_mode, new_hex) = side(f.new_side)?;
        write!(out, ":{old_mode:06o} {new_mode:06o} {old_hex} {new_hex} ")?;
        out.push(f.status);
        if let Some(source) = &f.source {
            write!(out, "{:03}", f.score)?;
            out.push(sep);
            out.extend_from_slice(&name_field(shorten_path(source, rel), z));
        }
        out.push(sep);
        out.extend_from_slice(&name_field(shorten_path(&f.path, rel), z));
        out.push(end);
    }
    Ok(())
}

/// Whether a pair still has something to report once a whitespace rule has been
/// applied — `diff_flush_patch_quietly()`'s test, stated over the change list: a
/// creation, deletion, mode change, rename or binary difference always prints a
/// header, and everything else survives only if lines actually differ.
fn reports_change(f: &FileChange) -> bool {
    let old_mode = f.old_side.map(|(m, _)| m);
    let new_mode = f.new_side.map(|(m, _)| m);
    old_mode.is_none()
        || new_mode.is_none()
        || old_mode != new_mode
        || f.source.is_some()
        || (f.is_binary && f.old_side.map(|(_, id)| id) != f.new_side.map(|(_, id)| id))
        || f.added != 0
        || f.deleted != 0
}

/// `diff_summary()`: one line per created, deleted, renamed or mode-changed file.
fn emit_summary(out: &mut Vec<u8>, files: &[FileChange]) {
    for f in files {
        let old_mode = f.old_side.map(|(m, _)| m);
        let new_mode = f.new_side.map(|(m, _)| m);
        match (old_mode, new_mode, &f.source) {
            // `show_rename_copy()`: the paired name, then a mode-change line whose
            // own name is suppressed because the line above already carried one.
            (_, _, Some(source)) => {
                out.extend_from_slice(b" ");
                out.extend_from_slice(if f.status == b'C' { b"copy " } else { b"rename " });
                out.extend_from_slice(&super::diff_pairs::pprint_rename(source, &f.path));
                out.extend_from_slice(format!(" ({}%)\n", f.score).as_bytes());
                summary_mode_change(out, old_mode, new_mode, None);
            }
            (None, Some(mode), None) => summary_mode_name(out, "create", mode, &f.path),
            (Some(mode), None, None) => summary_mode_name(out, "delete", mode, &f.path),
            // `diff_summary()`'s default arm: a `-B` rewrite that stayed a
            // modification announces itself and suppresses the mode-change name.
            _ if f.score != 0 => {
                out.extend_from_slice(b" rewrite ");
                out.extend_from_slice(&super::diff_files::quoted_name_bytes(&f.path));
                out.extend_from_slice(format!(" ({}%)\n", f.score).as_bytes());
                summary_mode_change(out, old_mode, new_mode, None);
            }
            _ => summary_mode_change(out, old_mode, new_mode, Some(&f.path)),
        }
    }
}

/// `show_file_mode_name()`: ` create mode <mode> <path>` / ` delete mode …`.
fn summary_mode_name(out: &mut Vec<u8>, verb: &str, mode: u32, path: &[u8]) {
    out.extend_from_slice(format!(" {verb} mode {mode:06o} ").as_bytes());
    out.extend_from_slice(&super::diff_files::quoted_name_bytes(path));
    out.push(b'\n');
}

/// `show_mode_change()`: the ` mode change <old> => <new>` line, named only when no
/// other summary line for the same pair printed the path.
fn summary_mode_change(out: &mut Vec<u8>, old: Option<u32>, new: Option<u32>, name: Option<&[u8]>) {
    let (Some(old), Some(new)) = (old, new) else {
        return;
    };
    if old == new {
        return;
    }
    out.extend_from_slice(format!(" mode change {old:06o} => {new:06o}").as_bytes());
    if let Some(path) = name {
        out.push(b' ');
        out.extend_from_slice(&super::diff_files::quoted_name_bytes(path));
    }
    out.push(b'\n');
}

/// A path as a raw/name record field: the bytes themselves under `-z`, C-quoted
/// otherwise — `write_name_quoted()`.
fn name_field(path: &[u8], z: bool) -> Vec<u8> {
    if z {
        path.to_vec()
    } else {
        super::diff_files::quoted_name_bytes(path)
    }
}

/// git's `--numstat`: `<added>\t<deleted>\t<path>` per file, with `-\t-` for a
/// binary file whose line counts are undefined. Under `-z` the record is
/// `<added>\t<deleted>\t\0<path>\0`, with a rename naming both sides.
fn emit_numstat(out: &mut Vec<u8>, files: &[FileChange], z: bool, rel: &str) {
    for f in files {
        if f.is_binary {
            out.extend_from_slice(b"-\t-\t");
        } else {
            out.extend_from_slice(format!("{}\t{}\t", f.added, f.deleted).as_bytes());
        }
        if z {
            // `show_numstat()`'s `-z` arm: the raw path, NUL-terminated. A rename
            // prefixes an empty field and names its source first.
            if let Some(source) = &f.source {
                out.push(0);
                out.extend_from_slice(shorten_path(source, rel));
                out.push(0);
            }
            out.extend_from_slice(shorten_path(&f.path, rel));
            out.push(0);
            continue;
        }
        out.extend_from_slice(&stat_name(f, false, rel));
        out.push(b'\n');
    }
}

/// The name the stat formats print for a file: a rename goes through
/// `pprint_rename()`, which factors out a shared prefix and suffix
/// (`pkg/{a.txt => b.txt}`) and otherwise prints `old => new`.
/// `strip_prefix()` (diff.c:5009): advance a reported name past `--relative`'s
/// prefix. The queue was already narrowed to it, so a name that does not start with
/// it can only be a `--summary`/`--dirstat` caller passing an empty prefix.
fn shorten_path<'a>(path: &'a [u8], rel: &str) -> &'a [u8] {
    match path.starts_with(rel.as_bytes()) {
        true => &path[rel.len()..],
        false => path,
    }
}

fn stat_name(f: &FileChange, compact: bool, rel: &str) -> Vec<u8> {
    let mut name = match &f.source {
        // `pprint_rename()` quotes the pair itself; a plain name goes through
        // `quote_c_style()` in `fill_print_name()`.
        Some(source) => {
            super::diff_pairs::pprint_rename(shorten_path(source, rel), shorten_path(&f.path, rel))
        }
        None => super::diff_files::quoted_name_bytes(shorten_path(&f.path, rel)),
    };
    // `--compact-summary`'s ` (<comment>)` suffix (`fill_print_name()`), derived
    // from the two sides' mode words exactly as `git diff` derives it.
    if compact {
        if let Some(c) = super::diff::compact_comment_for_modes(
            f.old_side.map(|(m, _)| m),
            f.new_side.map(|(m, _)| m),
        ) {
            name.push(b' ');
            name.push(b'(');
            name.extend_from_slice(c.as_bytes());
            name.push(b')');
        }
    }
    name
}

/// git's `--shortstat` (`show_shortstats()`): the summary line only.
fn emit_shortstat(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    diffstat::show_shortstats(out, &stat_rows(files, false, ""));
    Ok(())
}

// ---------------------------------------------------------------------------
// --graph
// ---------------------------------------------------------------------------

/// The palette `--graph` paints its branch lines with, in git's `column_colors`
/// layout: the drawing colors followed by the reset that terminates each of them,
/// so the last entry is both "the reset" and the sentinel index meaning "uncolored".
///
/// `git help config` calls the knob `log.graphColors`; git's `parse_graph_colors_config`
/// splits it on commas, keeps the specs it can parse, and warns about the rest.
fn graph_colors(repo: &gix::Repository) -> Vec<String> {
    const RESET: &str = "\x1b[m";
    let Some(spec) = repo.config_snapshot().string("log.graphColors") else {
        // git's `column_colors_ansi`.
        return [
            "\x1b[31m", "\x1b[32m", "\x1b[33m", "\x1b[34m", "\x1b[35m", "\x1b[36m", "\x1b[1;31m",
            "\x1b[1;32m", "\x1b[1;33m", "\x1b[1;34m", "\x1b[1;35m", "\x1b[1;36m", RESET,
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    };
    let spec = spec.to_string();
    let mut colors: Vec<String> = Vec::new();
    // `parse_graph_colors_config()` walks `start` to the end of the string rather
    // than splitting it: an empty value has nothing between `start` and `end`, so
    // the loop never runs, and a trailing comma leaves `start == end` and yields
    // no final chunk. Rust's `split(',')` disagrees with both — it hands back one
    // empty word for `""` and a trailing empty word for `"red,"`, each of which
    // parses as a valid (empty) color and so adds a column color git does not have.
    let bytes = spec.as_bytes();
    let mut start = 0usize;
    while start < bytes.len() {
        let comma = bytes[start..].iter().position(|&b| b == b',').map_or(bytes.len(), |i| start + i);
        let word = &spec[start..comma];
        match super::color::parse_color_spec(word) {
            Some(code) => colors.push(code),
            None => {
                // `color_parse_mem()` reports the value itself before
                // `parse_graph_colors_config()` reports what it did about it.
                eprintln!("error: invalid color value: {word}");
                eprintln!("warning: ignored invalid color '{word}' in log.graphColors");
            }
        }
        start = comma + 1;
    }
    colors.push(RESET.to_string());
    colors
}

/// Record each commit's `graph_width(opt->graph)` — the width of the `--graph`
/// prefix its commit row will carry — in `Node::graph_width`.
///
/// `graph->width` is set entirely by `graph_update()`/`graph_update_columns()`
/// (graph.c:651-718); drawing the rows never changes it. Running the state
/// machine over the node list therefore yields the same widths `show_log()` reads
/// one commit at a time, and costs a pass over the columns rather than a second
/// render. The colors are irrelevant to the width, so the cheapest palette is used.
fn measure_graph_widths(nodes: &mut [Node], first_parent: bool, interest: &GraphInterest) {
    let mut graph = Graph::new(vec![String::new()], false);
    for node in nodes.iter_mut() {
        let drawn_parents = interest.drawn_parents(node, first_parent);
        graph.update(node.id, &drawn_parents, node.boundary);
        node.graph_width = graph.width as i32;
    }
}

/// git's `graph_is_interesting()` (graph.c:457): which commits the graph draws a
/// lane for.
///
/// A parent that the run will never print is not one of them — `--merges` and the
/// other parent-count limits, `--since`/`--until`, `--grep`/`--author`/`--committer`
/// and a `^rev` exclusion all drop commits through `get_commit_action()`, and
/// `graph_is_interesting()` asks that same function. The merge naming such a
/// parent therefore draws no edge towards it and is rendered as an ordinary
/// commit, even though its `Merge:` header and `%P` still list the parent.
///
/// The two halves of the function are held as two sets.
struct GraphInterest {
    /// `revs->boundary` together with the CHILD_SHOWN flag: before returning a
    /// commit git marks each of its parents CHILD_SHOWN (revision.c:4583-4590), and
    /// under `--boundary` `graph_is_interesting()` accepts that flag on its own —
    /// which is how an excluded parent still gets the lane its `o` row sits in.
    /// Empty when `--boundary` is off.
    ///
    /// Only the commits git *returned* fill it, so one dropped by a filter
    /// contributes nothing — and neither does a boundary commit, which
    /// `create_boundary_commit_list()` (revision.c:4470) hands out from below the
    /// marking loop. That is what closes the column under the last `o` of a branch.
    child_shown: HashSet<ObjectId>,
    /// The commits that survived the filters `get_commit_action()` applies. git
    /// decides one commit at a time as the walk reaches it, and the walk that feeds
    /// `--graph` is never cut short (`--no-walk` is refused, and the early-exit
    /// budget is not taken), so running the filters over the whole walk gives the
    /// same answer. `--skip`/`--max-count` are deliberately not among them — they
    /// stop the walk rather than judge a commit, which is why `log --graph -1` still
    /// draws a merge's `|\` with neither parent below it.
    shown: HashSet<ObjectId>,
}

impl GraphInterest {
    fn is_interesting(&self, id: &ObjectId) -> bool {
        self.child_shown.contains(id) || self.shown.contains(id)
    }

    /// The parents `graph_update()` counts — `first_interesting_parent()` followed
    /// by `next_interesting_parent()` (graph.c:476-519).
    ///
    /// `--first-parent` makes `next_interesting_parent()` return nothing, which
    /// leaves the first parent alone — and nothing at all when that parent is
    /// itself uninteresting.
    fn drawn_parents(&self, node: &Node, first_parent: bool) -> Vec<ObjectId> {
        let parents: &[ObjectId] =
            if first_parent { &node.parents[..node.parents.len().min(1)] } else { &node.parents };
        parents.iter().copied().filter(|p| self.is_interesting(p)).collect()
    }
}

/// Prefix every line of every commit's block with git's ASCII graph, flushing the
/// merge and collapse rows that fall between commits.
///
/// `separator` is the byte a separator format (`format:` and the built-in pretties)
/// puts between records, and `None` for a terminator format whose blocks already
/// carry it. `first_parent` mirrors `revs->first_parent_only`, which makes
/// `next_interesting_parent()` return nothing so only the first parent is drawn.
///
/// A `None` block is a commit the walk reached but printed nothing for — a
/// `-S`/`-G` miss, or a `whatchanged` record whose diff came out empty. git calls
/// `graph_update()` from `get_revision()` and draws the rows from
/// `log_tree_commit()`, so such a commit still moves the columns on while
/// emitting no row at all; the next commit that does print then opens with the
/// `...` skip row, since the graph never reached padding.
fn render_graph(
    nodes: &[Node],
    blocks: &[Option<GraphBlock>],
    colors: Vec<String>,
    want_color: bool,
    separator: Option<u8>,
    terminator: Option<u8>,
    first_parent: bool,
    interest: &GraphInterest,
) -> Result<Vec<u8>> {
    let mut graph = Graph::new(colors, want_color);
    let mut out: Vec<u8> = Vec::new();
    // `opt->shown_one` together with `opt->missing_newline`: whether a record has
    // printed yet, and whether the last one's *message* ended in a newline
    // (log-tree.c:906-912 reads `msgbuf`, so a diff below it does not count). A
    // commit that printed nothing leaves both as the record before it left them.
    let mut prev_message: Option<&[u8]> = None;

    for (i, node) in nodes.iter().enumerate() {
        graph.update(node.id, &interest.drawn_parents(node, first_parent), node.boundary);

        let Some(record) = blocks[i].as_ref() else {
            continue;
        };
        let current: &[u8] = &record.text;

        // `show_log()` prints the record separator *after* the next commit has been
        // through `graph_update()`, and puts that commit's padding row in front of
        // it whenever the previous record ended in a newline — otherwise the gap
        // would read as a hole in the graph. The row is git's `graph_padding_line()`,
        // which also marks the state as padded so the commit row below it does not
        // mistake the collapse it interrupted for its own.
        if let (Some(prev), Some(sep)) = (prev_message, separator) {
            if sep == b'\n' && prev.ends_with(b"\n") {
                out.extend_from_slice(&graph.padding_line_before_record());
            }
            out.push(sep);
        }
        prev_message = Some(&record.text[..record.msg_len.min(record.text.len())]);

        // `graph_show_commit()` drains every row that comes *before* the commit
        // row onto lines of its own — the `...` skip row, and the expansion rows
        // an octopus merge needs — so that the commit's text lands on the row
        // carrying its `*`.
        while matches!(graph.state, GraphState::Skip | GraphState::PreCommit) {
            out.extend_from_slice(&graph.next_line());
            out.push(b'\n');
        }

        // A terminator format's byte is carried at the end of the block, but git
        // prints it *after* the commit's remaining graph rows (log-tree.c:915-919),
        // so it is taken off here and put back below.
        let block: &[u8] = match terminator {
            Some(t) if current.last() == Some(&t) => &current[..current.len() - 1],
            _ => current,
        };
        // `show_log()` hands only the commit *message* to `graph_show_commit_msg()`;
        // everything after it — the blank line fencing the diff off, and the diff
        // itself — is written by `log_tree_diff_flush()` through the diff prefix
        // callback, which is `graph_padding_line()` (graph.c:329-342). The graph's
        // remaining rows are therefore drained *between* the two, not after the
        // whole record: a commit under a collapse prints `|/` on a row of its own
        // and its diff on the padded rows below it.
        let split = record.msg_len.min(block.len());
        let (msg, diff) = block.split_at(split);

        let msg_nl = msg.ends_with(b"\n");
        graph_write_lines(&mut out, &mut graph, msg);

        // Rows the commit's message did not consume: the `|\` of a merge and the
        // `|/` of a collapse both appear on lines of their own. `graph_show_commit_msg()`
        // opens a line for them when the message did not end in one, then
        // `graph_show_remainder()` puts a newline *between* the rows only — the
        // trailing one comes back at the end, and only for a message that was newline
        // terminated. A collapse needs at most one row per column, so the bound below
        // can only trip on a bug here — failing beats hanging the caller.
        if graph.state != GraphState::Padding {
            if !msg_nl {
                out.push(b'\n');
            }
            let mut guard = graph.columns.len() + graph.new_columns.len() + graph.num_parents + 8;
            loop {
                out.extend_from_slice(&graph.next_line());
                if graph.state == GraphState::Padding {
                    break;
                }
                out.push(b'\n');
                guard -= 1;
                if guard == 0 {
                    crate::git_fatal!("--graph failed to settle the commit graph");
                }
            }
            if msg_nl {
                out.push(b'\n');
            }
        }

        if !diff.is_empty() {
            graph_write_lines(&mut out, &mut graph, diff);
        }

        // `show_log()` closes a terminator format's record last of all, and puts a
        // padding row in front of the terminator when the record's own text ended
        // in a newline (log-tree.c:915-919) — the same reason the separator carries
        // one: an empty line would read as a hole in the graph.
        if let Some(term) = terminator {
            if current.last() == Some(&term) {
                let tail_nl = if diff.is_empty() { msg_nl } else { diff.ends_with(b"\n") };
                if tail_nl {
                    out.extend_from_slice(&graph.next_line());
                }
                out.push(term);
            }
        }
    }
    Ok(out)
}

/// One record's text, split where `show_log()` hands over to `log_tree_diff_flush()`.
struct GraphBlock {
    /// Everything the record prints, message and diff together.
    text: Vec<u8>,
    /// How much of `text` is the commit message — what `graph_show_commit_msg()`
    /// receives, and after which the graph's remaining rows are drained. The rest
    /// is diff output, which git prefixes with `graph_padding_line()` instead.
    msg_len: usize,
}

impl GraphBlock {
    /// A record with no diff, so nothing follows the message.
    fn message_only(text: Vec<u8>) -> Self {
        GraphBlock { msg_len: text.len(), text }
    }
}

/// Write `buf` line by line, each line behind the graph row that belongs in front
/// of it — `graph_show_strbuf()` (graph.c:1622) for a message, the diff prefix
/// callback for everything after it. Both draw one row per line and neither adds a
/// newline the buffer did not already carry.
fn graph_write_lines(out: &mut Vec<u8>, graph: &mut Graph, buf: &[u8]) {
    let ends_nl = buf.ends_with(b"\n");
    let mut lines: Vec<&[u8]> = buf.split(|&b| b == b'\n').collect();
    if ends_nl {
        lines.pop();
    }
    for (j, line) in lines.iter().enumerate() {
        out.extend_from_slice(&graph.next_line());
        out.extend_from_slice(line);
        if ends_nl || j + 1 < lines.len() {
            out.push(b'\n');
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphState {
    Padding,
    /// The previous commit's rows were cut short, so git marks the gap with `...`.
    Skip,
    /// An expansion row that opens space around an octopus merge.
    PreCommit,
    Commit,
    PostMerge,
    Collapsing,
}

/// A branch line and the palette index it is drawn in — git's `struct column`.
#[derive(Clone, Copy)]
struct GraphColumn {
    id: ObjectId,
    color: usize,
}

/// A row under construction. The visible width is tracked separately from the
/// buffer because the color escapes occupy bytes but no columns, and every row of
/// a commit is padded to the same *visible* width so the text to its right aligns.
struct GraphLine {
    buf: Vec<u8>,
    width: usize,
}

impl GraphLine {
    fn new() -> Self {
        GraphLine { buf: Vec::new(), width: 0 }
    }

    fn addch(&mut self, c: u8) {
        self.buf.push(c);
        self.width += 1;
    }

    /// Every graph row for one commit is the same width, so text to its right lines up.
    fn pad_to(&mut self, width: usize) {
        while self.width < width {
            self.addch(b' ');
        }
    }
}

/// git's `graph.c` column state machine, ported for any number of parents —
/// two-way merges, octopus merges, and the left/right-skewed layouts alike.
///
/// The port follows `graph.c` function for function: [`Graph::update`] is
/// `graph_update()`, [`Graph::update_columns`] is `graph_update_columns()`, and
/// each `*_line` method is the matching `graph_output_*_line()`. The one piece
/// left out is `graph_needs_truncation()`, which only fires under
/// `--graph-lane-limit=<n>`; this port's `log` rejects that option outright, so
/// `revs->graph_max_lanes` is always 0 and every truncation branch is dead.
struct Graph {
    /// Columns as of the previous commit.
    columns: Vec<GraphColumn>,
    /// Columns as of the current commit.
    new_columns: Vec<GraphColumn>,
    /// Screen-slot to new-column index, `-1` for an empty slot. Sized at
    /// `2 * column_capacity` like git's array, with `mapping_size` the live
    /// prefix — the slots past it keep their stale values, which the commit row
    /// reads back out of `old_mapping`.
    mapping: Vec<i32>,
    old_mapping: Vec<i32>,
    mapping_size: usize,
    column_capacity: usize,
    commit: ObjectId,
    /// `--boundary`: the current commit is an excluded ancestor, drawn `o`.
    boundary: bool,
    /// The current commit's parents, in order — the post-merge row draws one edge
    /// per parent and takes each edge's color from that parent's column.
    parents: Vec<ObjectId>,
    num_parents: usize,
    width: usize,
    /// The next expansion row to print while in [`GraphState::PreCommit`].
    expansion_row: usize,
    state: GraphState,
    prev_state: GraphState,
    /// The column this commit sits in, or `columns.len()` when no column was
    /// following it yet.
    commit_index: usize,
    prev_commit_index: usize,
    /// Which merge layout to draw: 0 when the first parent is already in a column
    /// left of the merge (the left-skewed `|/| | |` form), 1 otherwise. `-1` while
    /// the layout for the current commit has not been chosen yet.
    merge_layout: i32,
    /// Columns this commit added, which is what decides whether the edges right of
    /// a merge are drawn `\` or `|`.
    edges_added: i32,
    prev_edges_added: i32,
    /// `log.graphColors`, with the reset appended; the last index means "uncolored".
    colors: Vec<String>,
    /// The color the next column to be opened is assigned, cycling through `colors`.
    default_column_color: usize,
    want_color: bool,
}

/// `graph_init()`'s starting column capacity; grown by doubling.
const GRAPH_COLUMN_CAPACITY: usize = 30;

impl Graph {
    fn new(colors: Vec<String>, want_color: bool) -> Self {
        // git starts one short of the wrap point, because the first column opened
        // always increments first — which lands the first branch line on index 0.
        let default_column_color = colors.len().saturating_sub(2);
        Graph {
            boundary: false,
            columns: Vec::new(),
            new_columns: Vec::new(),
            mapping: vec![-1; 2 * GRAPH_COLUMN_CAPACITY],
            old_mapping: vec![-1; 2 * GRAPH_COLUMN_CAPACITY],
            mapping_size: 0,
            column_capacity: GRAPH_COLUMN_CAPACITY,
            commit: ObjectId::null(gix::hash::Kind::Sha1),
            parents: Vec::new(),
            num_parents: 0,
            width: 0,
            expansion_row: 0,
            state: GraphState::Padding,
            prev_state: GraphState::Padding,
            commit_index: 0,
            prev_commit_index: 0,
            merge_layout: 0,
            edges_added: 0,
            prev_edges_added: 0,
            colors,
            default_column_color,
            want_color,
        }
    }

    /// The index that means "emit no escapes" — git's `column_colors_max`, which is
    /// also where the reset lives.
    fn uncolored(&self) -> usize {
        self.colors.len() - 1
    }

    /// git's `graph_get_current_column_color`: the color a newly opened column takes,
    /// or the uncolored sentinel when this run is not coloring at all.
    fn current_column_color(&self) -> usize {
        if self.want_color {
            self.default_column_color
        } else {
            self.uncolored()
        }
    }

    /// git's `graph_increment_column_color`: `(default + 1) % column_colors_max`.
    ///
    /// A `log.graphColors` whose every entry was rejected (or that is empty)
    /// leaves the parse holding only the reset, so `column_colors_max` is 0 and
    /// git's modulo is a division by zero — C's undefined behaviour, which the
    /// counter's value never escapes anyway: `graph_line_write_column` paints a
    /// column only when `color < column_colors_max`, and nothing is below zero.
    /// Leave the counter alone in that case rather than divide; the graph comes
    /// out uncolored either way, which is what git prints.
    fn increment_column_color(&mut self) {
        let max = self.uncolored();
        if max > 0 {
            self.default_column_color = (self.default_column_color + 1) % max;
        }
    }

    /// git's `graph_find_commit_color`: a commit that already owns a column keeps its
    /// color across the row, so a branch line does not change color as it descends.
    fn commit_color(&self, id: ObjectId) -> usize {
        self.columns
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| self.current_column_color(), |c| c.color)
    }

    /// Draw one branch-line character in its column's color — git's
    /// `graph_line_write_column`.
    fn write_column(&self, line: &mut GraphLine, col: &GraphColumn, ch: u8) {
        let uncolored = self.uncolored();
        if col.color < uncolored {
            line.buf.extend_from_slice(self.colors[col.color].as_bytes());
        }
        line.addch(ch);
        if col.color < uncolored {
            line.buf.extend_from_slice(self.colors[uncolored].as_bytes());
        }
    }

    /// `graph_update_state()`: remember the row that was just drawn, since the next
    /// row's shape depends on it.
    fn update_state(&mut self, s: GraphState) {
        self.prev_state = self.state;
        self.state = s;
    }

    /// `graph_ensure_capacity()`: the mapping arrays are twice the column capacity,
    /// which doubles until it covers the columns the next commit can need.
    fn ensure_capacity(&mut self, num_columns: usize) {
        if self.column_capacity >= num_columns {
            return;
        }
        while self.column_capacity < num_columns {
            self.column_capacity *= 2;
        }
        self.mapping.resize(2 * self.column_capacity, -1);
        self.old_mapping.resize(2 * self.column_capacity, -1);
    }

    fn find_new_column_by_commit(&self, id: ObjectId) -> Option<usize> {
        self.new_columns.iter().position(|c| c.id == id)
    }

    /// `graph_num_dashed_parents()`: the parents an octopus merge reaches with a
    /// horizontal `-` run, which is one less when the merge skews left.
    fn num_dashed_parents(&self) -> i32 {
        self.num_parents as i32 + self.merge_layout - 3
    }

    /// `graph_num_expansion_rows()`: two rows per dashed parent, to open the space
    /// the octopus merge's edges need.
    fn num_expansion_rows(&self) -> i32 {
        self.num_dashed_parents() * 2
    }

    /// `graph_needs_pre_commit_line()`: an octopus merge with a branch line to its
    /// right needs its expansion rows drawn before the commit row.
    fn needs_pre_commit_line(&self) -> bool {
        self.num_parents >= 3
            && (self.commit_index as isize) < self.columns.len() as isize - 1
            && (self.expansion_row as i32) < self.num_expansion_rows()
    }

    /// `graph_is_mapping_correct()`: every branch line is at its target column, so
    /// nothing is left to collapse.
    fn mapping_correct(&self) -> bool {
        self.mapping[..self.mapping_size]
            .iter()
            .enumerate()
            .all(|(i, &t)| t < 0 || t == (i as i32) / 2)
    }

    fn update(&mut self, id: ObjectId, parents: &[ObjectId], boundary: bool) {
        self.commit = id;
        self.boundary = boundary;
        self.parents = parents.to_vec();
        self.num_parents = parents.len();
        self.prev_commit_index = self.commit_index;
        self.update_columns();
        self.expansion_row = 0;
        // `graph_update()` assigns the state directly rather than through
        // `graph_update_state()`: no line was drawn for the state being left, so
        // `prev_state` must keep describing the last row actually printed.
        self.state = if self.state != GraphState::Padding {
            // The previous commit never reached padding, so part of the graph is
            // missing and git marks the gap.
            GraphState::Skip
        } else if self.needs_pre_commit_line() {
            GraphState::PreCommit
        } else {
            GraphState::Commit
        };
    }

    /// `graph_insert_into_new_columns()`: record `id` in the new column list
    /// (reusing its column when it is already there) and point a screen slot at it.
    /// `idx` is the column the current commit occupies when `id` is one of its
    /// parents, and `-1` for a column merely passing through.
    fn insert_into_new_columns(&mut self, id: ObjectId, idx: i32) {
        let i = match self.find_new_column_by_commit(id) {
            Some(i) => i,
            None => {
                let color = self.commit_color(id);
                self.new_columns.push(GraphColumn { id, color });
                self.new_columns.len() - 1
            }
        };

        let mapping_idx: isize;
        if self.num_parents > 1 && idx > -1 && self.merge_layout == -1 {
            // The first parent of a merge picks the layout: 0 when that parent
            // already sits in a column left of the merge (the edges fuse and one
            // less column is added), 1 when it does not.
            let dist = idx - i as i32;
            let shift = if dist > 1 { 2 * dist - 3 } else { 1 };
            self.merge_layout = i32::from(dist <= 0);
            self.edges_added = self.num_parents as i32 + self.merge_layout - 2;
            mapping_idx = self.width as isize + (self.merge_layout as isize - 1) * shift as isize;
            self.width += 2 * self.merge_layout as usize;
        } else if self.edges_added > 0
            && self.width >= 2
            && i as i32 == self.mapping[self.width - 2]
        {
            // Columns were added by a merge but this one was found in the last
            // existing column, so the two edges join immediately.
            mapping_idx = self.width as isize - 2;
            self.edges_added = -1;
        } else {
            mapping_idx = self.width as isize;
            self.width += 2;
        }

        if let Some(slot) = usize::try_from(mapping_idx).ok().and_then(|k| self.mapping.get_mut(k)) {
            *slot = i as i32;
        }
    }

    /// `graph_update_columns()`: roll the column state forward one commit, deciding
    /// which lanes the next row carries and where each of them is heading.
    fn update_columns(&mut self) {
        std::mem::swap(&mut self.columns, &mut self.new_columns);
        self.new_columns.clear();

        let num_columns = self.columns.len();
        let max_new_columns = num_columns + self.num_parents;
        self.ensure_capacity(max_new_columns);

        self.mapping_size = 2 * max_new_columns;
        for slot in self.mapping.iter_mut().take(self.mapping_size) {
            *slot = -1;
        }

        self.width = 0;
        self.prev_edges_added = self.edges_added;
        self.edges_added = 0;

        let mut seen_this = false;
        let mut is_commit_in_columns = true;
        for i in 0..=num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                is_commit_in_columns = false;
                self.commit
            } else {
                self.columns[i].id
            };

            if col_commit == self.commit {
                seen_this = true;
                self.commit_index = i;
                self.merge_layout = -1;
                for parent in self.parents.clone() {
                    // A merge fans out, and a commit no column was following starts a
                    // fresh line: both open a lane that gets the next color in the cycle.
                    if self.num_parents > 1 || !is_commit_in_columns {
                        self.increment_column_color();
                    }
                    self.insert_into_new_columns(parent, i as i32);
                }
                // A commit occupies at least two screen slots even with no parents.
                if self.num_parents == 0 {
                    self.width += 2;
                }
            } else {
                self.insert_into_new_columns(col_commit, -1);
            }
        }

        while self.mapping_size > 1 && self.mapping[self.mapping_size - 1] < 0 {
            self.mapping_size -= 1;
        }
    }

    fn next_line(&mut self) -> Vec<u8> {
        let mut line = GraphLine::new();
        match self.state {
            GraphState::Padding => self.padding_line(&mut line),
            GraphState::Skip => self.skip_line(&mut line),
            GraphState::PreCommit => self.pre_commit_line(&mut line),
            GraphState::Commit => self.commit_line(&mut line),
            GraphState::PostMerge => self.post_merge_line(&mut line),
            GraphState::Collapsing => self.collapsing_line(&mut line),
        }
        line.pad_to(self.width);
        line.buf
    }

    /// `graph_padding_line()` (graph.c:1480): the row printed in front of the
    /// newline separating two records. It runs after the next commit's
    /// `graph_update()`, so while that commit still waits in
    /// [`GraphState::Commit`] the row draws the columns as they stood *before* it,
    /// widening the octopus merge's own column to cover the dashes its commit row
    /// is about to carry. In any other state git falls through to that state's
    /// ordinary row, which advances the state machine as usual.
    fn padding_line_before_record(&mut self) -> Vec<u8> {
        if self.state != GraphState::Commit {
            return self.next_line();
        }
        let mut line = GraphLine::new();
        for col in &self.columns {
            self.write_column(&mut line, col, b'|');
            if col.id == self.commit && self.num_parents > 2 {
                for _ in 0..(self.num_parents - 2) * 2 {
                    line.addch(b' ');
                }
            } else {
                line.addch(b' ');
            }
        }
        line.pad_to(self.width);
        // git records the padded row so the commit row below it is not read as the
        // continuation of a collapse.
        self.prev_state = GraphState::Padding;
        line.buf
    }

    /// `graph_output_padding_line()`: every branch line carries straight down.
    fn padding_line(&mut self, line: &mut GraphLine) {
        for col in &self.new_columns {
            self.write_column(line, col, b'|');
            line.addch(b' ');
        }
    }

    /// `graph_output_skip_line()`: the previous commit never finished its rows, so
    /// git marks the gap and picks up where the new commit needs to start.
    fn skip_line(&mut self, line: &mut GraphLine) {
        for ch in b"..." {
            line.addch(*ch);
        }
        if self.needs_pre_commit_line() {
            self.update_state(GraphState::PreCommit);
        } else {
            self.update_state(GraphState::Commit);
        }
    }

    /// `graph_output_pre_commit_line()`: widen the space around an octopus merge,
    /// one row at a time, so its dashed edges have room. Only reached with three or
    /// more parents.
    fn pre_commit_line(&mut self, line: &mut GraphLine) {
        let mut seen_this = false;
        for i in 0..self.columns.len() {
            let col = self.columns[i];
            if col.id == self.commit {
                seen_this = true;
                self.write_column(line, &col, b'|');
                for _ in 0..self.expansion_row {
                    line.addch(b' ');
                }
            } else if seen_this && self.expansion_row == 0 {
                // First expansion row: a branch line that the previous commit's
                // post-merge row drew as `\` keeps going as `\` here.
                if self.prev_state == GraphState::PostMerge && self.prev_commit_index < i {
                    self.write_column(line, &col, b'\\');
                } else {
                    self.write_column(line, &col, b'|');
                }
            } else if seen_this {
                self.write_column(line, &col, b'\\');
            } else {
                self.write_column(line, &col, b'|');
            }
            line.addch(b' ');
        }

        self.expansion_row += 1;
        if !self.needs_pre_commit_line() {
            self.update_state(GraphState::Commit);
        }
    }

    /// `graph_draw_octopus_merge()`: the horizontal `-`…`.` run that reaches the
    /// parents beyond the first two. Each dash takes the color of the lane the edge
    /// under it will collapse to, which the mapping — not `new_columns` order —
    /// knows.
    fn draw_octopus_merge(&self, line: &mut GraphLine) {
        let dashed_parents = self.num_dashed_parents();
        for i in 0..dashed_parents {
            let slot = (self.commit_index + i as usize + 2) * 2;
            let Some(col) = self
                .mapping
                .get(slot)
                .and_then(|&j| usize::try_from(j).ok())
                .and_then(|j| self.new_columns.get(j))
                .copied()
            else {
                continue;
            };
            self.write_column(line, &col, b'-');
            self.write_column(line, &col, if i == dashed_parents - 1 { b'.' } else { b'-' });
        }
    }

    /// `graph_output_commit_line()`: the row carrying the commit's mark.
    fn commit_line(&mut self, line: &mut GraphLine) {
        let mut seen_this = false;
        let num_columns = self.columns.len();
        for i in 0..=num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit
            } else {
                self.columns[i].id
            };

            if col_commit == self.commit {
                seen_this = true;
                // `graph_output_commit_char()`: a boundary commit is drawn as a
                // hollow `o` rather than the usual `*`.
                line.addch(if self.boundary { b'o' } else { b'*' });
                if self.num_parents > 2 {
                    self.draw_octopus_merge(line);
                }
            } else if seen_this && self.edges_added > 1 {
                self.write_column(line, &self.columns[i], b'\\');
            } else if seen_this && self.edges_added == 1 {
                // A right-skewed two-way merge or a left-skewed three-way one:
                // there is no expansion row, so this is the commit's first row and
                // a `\` coming out of the previous post-merge row keeps its shape.
                if self.prev_state == GraphState::PostMerge
                    && self.prev_edges_added > 0
                    && self.prev_commit_index < i
                {
                    self.write_column(line, &self.columns[i], b'\\');
                } else {
                    self.write_column(line, &self.columns[i], b'|');
                }
            } else if self.prev_state == GraphState::Collapsing
                && self.old_mapping.get(2 * i + 1).copied().unwrap_or(-1) == i as i32
                && self.mapping.get(2 * i).copied().unwrap_or(-1) < i as i32
            {
                self.write_column(line, &self.columns[i], b'/');
            } else {
                self.write_column(line, &self.columns[i], b'|');
            }
            line.addch(b' ');
        }

        if self.num_parents > 1 {
            self.update_state(GraphState::PostMerge);
        } else if self.mapping_correct() {
            self.update_state(GraphState::Padding);
        } else {
            self.update_state(GraphState::Collapsing);
        }
    }

    /// `graph_output_post_merge_line()`: the `|\`, `|\ \`, `/|\` … row under a merge,
    /// one character per parent in the color of the lane that parent took.
    fn post_merge_line(&mut self, line: &mut GraphLine) {
        /// git's `merge_chars`, indexed by how far along the merge fan the edge is.
        const MERGE_CHARS: [u8; 3] = [b'/', b'|', b'\\'];

        let first_parent = self.parents.first().copied();
        let mut parent_col: Option<GraphColumn> = None;
        let mut seen_this = false;
        let num_columns = self.columns.len();

        for i in 0..=num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit
            } else {
                self.columns[i].id
            };

            if col_commit == self.commit {
                seen_this = true;
                // The merge's own edges: one per parent, drawn from the column each
                // parent just took. `merge_layout` picks where in `merge_chars` the
                // run starts, so a left-skewed merge opens with `/`.
                let mut idx = self.merge_layout.clamp(0, 2) as usize;
                for (j, parent) in self.parents.clone().into_iter().enumerate() {
                    let ch = MERGE_CHARS[idx];
                    match self.find_new_column_by_commit(parent) {
                        Some(p) => {
                            let col = self.new_columns[p];
                            self.write_column(line, &col, ch);
                        }
                        None => line.addch(ch),
                    }
                    if idx == 2 {
                        if self.edges_added > 0 || j < self.num_parents - 1 {
                            line.addch(b' ');
                        }
                    } else {
                        idx += 1;
                    }
                }
                if self.edges_added == 0 {
                    line.addch(b' ');
                }
            } else if seen_this {
                if self.edges_added > 0 {
                    self.write_column(line, &self.columns[i], b'\\');
                } else {
                    self.write_column(line, &self.columns[i], b'|');
                }
                line.addch(b' ');
            } else {
                self.write_column(line, &self.columns[i], b'|');
                // The gap left of a left-skewed merge is filled with the first
                // parent's `_` run once that parent's column has been passed.
                if self.merge_layout != 0 || i as isize != self.commit_index as isize - 1 {
                    match parent_col {
                        Some(col) => self.write_column(line, &col, b'_'),
                        None => line.addch(b' '),
                    }
                }
            }

            if Some(col_commit) == first_parent && i < num_columns {
                parent_col = Some(self.columns[i]);
            }
        }

        if self.mapping_correct() {
            self.update_state(GraphState::Padding);
        } else {
            self.update_state(GraphState::Collapsing);
        }
    }

    /// `graph_output_collapsing_line()`: move every branch line one step towards its
    /// target column, drawing `/` for a diagonal step and `_` for a horizontal run.
    fn collapsing_line(&mut self, line: &mut GraphLine) {
        std::mem::swap(&mut self.mapping, &mut self.old_mapping);
        for slot in self.mapping.iter_mut().take(self.mapping_size) {
            *slot = -1;
        }

        let mut horizontal_edge: i32 = -1;
        let mut horizontal_edge_target: i32 = -1;

        for i in 0..self.mapping_size {
            let target = self.old_mapping[i];
            if target < 0 {
                continue;
            }
            // `update_columns()` always inserts the leftmost column first, so a
            // branch's target is never to the right of where it is now.
            if (target as usize) * 2 == i {
                self.mapping[i] = target;
            } else if i >= 1 && self.mapping[i - 1] < 0 {
                // Nothing to the left: step one slot over.
                self.mapping[i - 1] = target;
                if horizontal_edge == -1 {
                    horizontal_edge = i as i32;
                    horizontal_edge_target = target;
                    // `target * 2 + 3` is the screen column the horizontal run starts at.
                    let mut j = (target as usize) * 2 + 3;
                    while (j as isize) < i as isize - 2 {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            } else if i >= 1 && self.mapping[i - 1] == target {
                // Shares a parent with the line to its left; already drawn.
            } else if i >= 2 {
                // Cross over the unrelated line to the left, and claim the
                // horizontal edge so no other line moves sideways this row.
                self.mapping[i - 2] = target;
                if horizontal_edge == -1 {
                    horizontal_edge_target = target;
                    horizontal_edge = i as i32 - 1;
                    let mut j = (target as usize) * 2 + 3;
                    while (j as isize) < i as isize - 2 {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            }
        }

        // The commit row of the *next* commit reads this row's mapping back out of
        // `old_mapping`, so it is copied before the drawing loop clears the spans
        // that must not continue.
        self.old_mapping[..self.mapping_size].copy_from_slice(&self.mapping[..self.mapping_size]);

        if self.mapping_size > 0 && self.mapping[self.mapping_size - 1] < 0 {
            self.mapping_size -= 1;
        }

        let mut used_horizontal = false;
        for i in 0..self.mapping_size {
            let target = self.mapping[i];
            // A collapsing edge is drawn in the color of the lane it is heading for,
            // which is the new column the mapping points at.
            let col = usize::try_from(target).ok().and_then(|t| self.new_columns.get(t)).copied();
            let Some(col) = col else {
                line.addch(b' ');
                continue;
            };
            if (target as usize) * 2 == i {
                self.write_column(line, &col, b'|');
            } else if target == horizontal_edge_target && i as i32 != horizontal_edge - 1 {
                // Only the first segment of a horizontal run continues onto the
                // next row.
                if i != (target as usize) * 2 + 3 {
                    self.mapping[i] = -1;
                }
                used_horizontal = true;
                self.write_column(line, &col, b'_');
            } else {
                if used_horizontal && (i as i32) < horizontal_edge {
                    self.mapping[i] = -1;
                }
                self.write_column(line, &col, b'/');
            }
        }

        // Only the row that finishes the collapse advances `prev_state`: git leaves
        // both fields alone while more collapsing rows are still to come.
        if self.mapping_correct() {
            self.update_state(GraphState::Padding);
        }
    }
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// The `--date=` output modes this port renders byte-for-byte, plus `relative`,
/// which is measured against the current wall clock. The remaining process-time /
/// zone-dependent modes (`human`, `local`) are still rejected rather than faked.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DateMode {
    /// git's `DATE_NORMAL`: `Www Mmm D HH:MM:SS YYYY +ZZZZ`.
    Default,
    /// `short`: `YYYY-MM-DD`.
    Short,
    /// `iso`/`iso8601`: `YYYY-MM-DD HH:MM:SS +ZZZZ`.
    Iso,
    /// `iso-strict`/`iso8601-strict`: `YYYY-MM-DDTHH:MM:SS+ZZ:ZZ`.
    IsoStrict,
    /// `rfc`/`rfc2822`: `Www, D Mmm YYYY HH:MM:SS +ZZZZ`.
    Rfc,
    /// `unix`: the raw epoch seconds, no timezone.
    Unix,
    /// `raw`: `<seconds> +ZZZZ`.
    Raw,
    /// `relative`: `N <unit> ago`, measured against the current time.
    Relative,
}

/// `--color=<when>` (and `--color`/`--no-color`): whether `%C`/`%d` emit ANSI.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorWhen {
    Always,
    Never,
    /// Color when stdout is a terminal (or we are paging to one).
    Auto,
}

/// Map a `--date=` value to a [`DateMode`]. `None` for a value git accepts but
/// this port renders time/zone-dependently (surfaced terse) or does not know.
pub(crate) fn parse_date_mode(spec: &str) -> Option<DateMode> {
    Some(match spec {
        "default" | "normal" => DateMode::Default,
        "short" => DateMode::Short,
        "iso" | "iso8601" => DateMode::Iso,
        "iso-strict" | "iso8601-strict" => DateMode::IsoStrict,
        "rfc" | "rfc2822" => DateMode::Rfc,
        "unix" => DateMode::Unix,
        "raw" => DateMode::Raw,
        "relative" => DateMode::Relative,
        _ => return None,
    })
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `show_date(…, DATE_MODE(RFC2822))`, which is the one date mode git hardcodes
/// outside the `--date=` machinery: `refs.c`'s
/// `warning(_("log for '%.*s' only goes back to %s"), …)` renders its timestamp
/// that way regardless of the caller's date settings.
///
/// Exposed so [`crate::objname::reflog_reach`] can build that warning without
/// reaching for [`DateMode`] itself.
pub(crate) fn show_date_rfc2822(seconds: i64, offset: i32) -> String {
    format_date(seconds, offset, DateMode::Rfc)
}

/// Format a timestamp in the requested [`DateMode`], matching git byte-for-byte.
fn format_date(seconds: i64, offset: i32, mode: DateMode) -> String {
    match mode {
        DateMode::Default => format_git_date(seconds, offset),
        // Relative dates need the current time; callers route them through
        // `fmt_time`, but keep this arm self-contained rather than unreachable.
        DateMode::Relative => format_relative(seconds, now_secs()),
        DateMode::Unix => format!("{seconds}"),
        DateMode::Raw => {
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            format!("{seconds} {sign}{:02}{:02}", off / 3600, (off % 3600) / 60)
        }
        DateMode::Short | DateMode::Iso | DateMode::IsoStrict | DateMode::Rfc => {
            let local = seconds + offset as i64;
            let days = local.div_euclid(86_400);
            let secs = local.rem_euclid(86_400);
            let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
            let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
            let (year, month, day) = civil_from_days(days);
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            let (oh, om) = (off / 3600, (off % 3600) / 60);
            match mode {
                DateMode::Short => format!("{year}-{month:02}-{day:02}"),
                DateMode::Iso => format!(
                    "{year}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}"
                ),
                // git renders a zero UTC offset as `Z` in iso-strict (RFC 3339),
                // not `+00:00` (verified against git 2.55).
                DateMode::IsoStrict => {
                    let tz = if offset == 0 {
                        "Z".to_string()
                    } else {
                        format!("{sign}{oh:02}:{om:02}")
                    };
                    format!("{year}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}{tz}")
                }
                DateMode::Rfc => format!(
                    "{}, {day} {} {year} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}",
                    WEEKDAYS[weekday],
                    MONTHS[(month - 1) as usize],
                ),
                _ => unreachable!(),
            }
        }
    }
}

/// Format a commit time exactly like stock `git log`'s default (`DATE_NORMAL`)
/// mode: `Www Mmm <day> HH:MM:SS YYYY +ZZZZ`, in the commit's own timezone
/// offset. The day is **unpadded** — git's `show_date` builds this with a bare
/// `%d` (printf integer), so a single-digit day gets one space, not two
/// (verified against git 2.55: `Mon Jan 2 ...`, not `Mon Jan  2 ...`).
fn format_git_date(seconds: i64, offset: i32) -> String {
    // Shift into the commit's local wall-clock time, then split into whole days
    // (since the Unix epoch) and the seconds within the day. `div_euclid` /
    // `rem_euclid` keep the split correct for pre-1970 (negative) timestamps.
    let local = seconds + offset as i64;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // 1970-01-01 (day 0) was a Thursday, index 4 with Sunday = 0.
    let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);

    let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    let (off_h, off_m) = (off / 3600, (off % 3600) / 60);

    format!(
        "{} {} {} {:02}:{:02}:{:02} {} {}{:02}{:02}",
        WEEKDAYS[weekday],
        MONTHS[(month - 1) as usize],
        day,
        hour,
        min,
        sec,
        year,
        sign,
        off_h,
        off_m,
    )
}

/// Convert a day count since the Unix epoch into a civil `(year, month, day)`,
/// month and day 1-based. Howard Hinnant's `civil_from_days` algorithm, which is
/// exact for the whole representable range and needs no calendar tables.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month as u32, day)
}

/// Strip trailing whitespace (git trims a subject line this way).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b'\n' || last == b'\r' || last == b' ' || last == b'\t' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

#[cfg(test)]
mod option_surface_tests {
    use super::{git_log_knows, GIT_LOG_LONG_OPTS};

    /// The table has to stay sorted, because [`git_log_knows`] binary-searches it.
    #[test]
    fn the_long_option_table_is_sorted_and_bare() {
        let mut sorted = GIT_LOG_LONG_OPTS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted.as_slice(), GIT_LOG_LONG_OPTS);
        assert!(GIT_LOG_LONG_OPTS.iter().all(|n| !n.starts_with("--")));
    }

    /// The whole point of the table: a name git has keeps this port's own
    /// "unsupported flag" gap message, while a name git does not have gets git's
    /// `unrecognized argument`. Answering both the same way means either lying about
    /// git rejecting an option it accepts, or hiding an unported one behind git's
    /// wording. Each expectation below was run against stock git 2.55.0.
    #[test]
    fn a_real_option_is_known_and_an_invented_one_is_not() {
        // Recognised by `setup_revisions()`/`diff_opt_parse()` but not implemented
        // here, so the honest gap message is the right answer.
        for real in ["--cherry-pick", "--dirstat", "--color-words", "--ita-visible-in-index"] {
            assert!(git_log_knows(real), "{real} is a git log option");
        }
        for invented in ["--zzbogus", "--zzbogus=x", "--cherry-picks", "--no-zzbogus"] {
            assert!(!git_log_knows(invented), "{invented} is not a git log option");
        }
    }

    /// The name is matched with any `=<value>` cut off — git's own granularity, since
    /// `parse_long_opt()` looks the name up before it splits the value — but `--no-`
    /// is part of the name, so a negation git does not have stays unknown.
    #[test]
    fn a_value_and_a_negation_are_two_different_questions() {
        assert!(git_log_knows("--pretty"));
        assert!(git_log_knows("--pretty=oneline"));
        // `--no-pretty` is absent from git's table; stock reports it unrecognized.
        assert!(!git_log_knows("--no-pretty"));
        // `--no-merges` is a table entry in its own right.
        assert!(git_log_knows("--no-merges"));
    }

    /// A short option is looked up on its first letter, because that is what
    /// `parse_options` resolves before it takes an attached value or re-emits the
    /// rest of the cluster. Stock: `git log -Zp` reports `-Zp`, `git log -o5`
    /// reports `-o5`, and `git log -U5` is a context width.
    #[test]
    fn a_short_option_is_judged_by_its_first_letter() {
        assert!(git_log_knows("-U5"));
        assert!(git_log_knows("-p"));
        assert!(!git_log_knows("-Zp"));
        assert!(!git_log_knows("-o5"));
        // Degenerate tokens git also reports as unrecognized arguments.
        assert!(!git_log_knows("-"));
        assert!(!git_log_knows("---"));
        assert!(!git_log_knows("--=x"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every expectation below was verified against stock `git log -- <spec>` on a
    // real repository with git 2.55.0: a bracket pathspec is a wildcard pathspec,
    // so it never gets the literal leading-directory shortcut, and its `[…]`
    // expression follows git's `wildmatch.c:dowild` (flags=0) rules.

    /// A throwaway repository for the matcher to take its defaults from. Nothing
    /// here reads the worktree or the index — the specs are matched against the
    /// paths given, not against anything on disk — but a pathspec is always parsed
    /// in the context of a repository, so there has to be one.
    fn scratch_repo() -> gix::Repository {
        static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        let root = ROOT.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("zvcs-pathspec-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            gix::init(&dir).expect("init scratch repo");
            dir
        });
        gix::open(root).expect("open scratch repo")
    }

    fn m(spec: &str, path: &[u8]) -> bool {
        let repo = scratch_repo();
        PathspecMatcher::new(&repo, &[spec]).expect("parse pathspec").matches(path)
    }

    #[test]
    fn bracket_set_matches_a_listed_char() {
        // `git log -- 'READM[Ee]'` shows the README commit; the set picks `E`.
        assert!(m("READM[Ee]", b"README"));
        assert!(m("f[oi]le", b"file"));
        assert!(m("f[oi]le", b"fole"));
        // `x` is not in the set, so no match.
        assert!(!m("f[oi]le", b"fxle"));
    }

    #[test]
    fn bracket_range() {
        // `READM[A-Z]` matches (E in A-Z); `READM[a-d]` does not (E not in a-d).
        assert!(m("READM[A-Z]", b"README"));
        assert!(!m("READM[a-d]", b"README"));
    }

    #[test]
    fn bracket_negation_both_forms() {
        // `[!x]`/`[^x]` match `E` (not `x`); `[!E]` rejects `E`.
        assert!(m("READM[!x]", b"README"));
        assert!(m("READM[^x]", b"README"));
        assert!(!m("READM[!E]", b"README"));
    }

    #[test]
    fn posix_character_class() {
        // `[[:upper:]]` matches `E`; `[[:digit:]]` does not.
        assert!(m("READM[[:upper:]]", b"README"));
        assert!(!m("READM[[:digit:]]", b"README"));
    }

    #[test]
    fn malformed_bracket_is_no_match() {
        // git prints nothing for an unterminated class (WM_ABORT_ALL → no-match).
        assert!(!m("READM[Ee", b"README"));
    }

    #[test]
    fn star_spans_slashes_and_bracket_dir_needs_full_match() {
        // flags=0: `*` spans `/`, so `builtin*log.c` matches `builtin/log.c`.
        assert!(m("builtin*log.c", b"builtin/log.c"));
        // A wildcard pathspec that names a directory gets no leading-dir shortcut,
        // and wildmatch leaves the trailing `/log.c` unmatched — git shows nothing.
        assert!(!m("buil[dt]in", b"builtin/log.c"));
    }

    #[test]
    fn magic_pathspecs_are_matched_not_refused() {
        // These used to `bail!("magic pathspecs are not ported")`, which took the
        // whole command down. Each one is a real match now.
        assert!(m(":(glob)foo", b"foo"));
        assert!(m(":(literal)f[oi]le", b"f[oi]le"), ":(literal) turns off the wildcards");
        assert!(!m(":(literal)f[oi]le", b"file"));
        assert!(m(":(icase)README", b"readme"));
        assert!(m(":(top)src", b"src/lib.rs"));
        // `:(glob)` gives `*` pathname semantics, so it stops at a `/` where a
        // plain wildcard pathspec would run straight through one.
        assert!(!m(":(glob)builtin*log.c", b"builtin/log.c"));
        assert!(m(":(glob)builtin/**/log.c", b"builtin/sub/log.c"));
    }

    /// An exclusion subtracts from the set rather than matching on its own, so it
    /// cannot be modelled by asking each spec in turn — the whole set answers.
    #[test]
    fn exclusions_subtract_from_the_set() {
        let repo = scratch_repo();
        let mut set = |specs: &[&str], path: &[u8]| {
            PathspecMatcher::new(&repo, specs).expect("parse pathspecs").matches(path)
        };
        // A lone exclusion selects everything it does not name.
        assert!(set(&[":!docs"], b"src/lib.rs"));
        assert!(!set(&[":!docs"], b"docs/guide.md"));
        assert!(!set(&[":(exclude)docs"], b"docs/guide.md"));
        // With a positive spec present, the exclusion carves out of it.
        assert!(set(&["src", ":!src/gen"], b"src/lib.rs"));
        assert!(!set(&["src", ":!src/gen"], b"src/gen/table.rs"));
        // A path outside every positive spec is still not selected.
        assert!(!set(&["src", ":!src/gen"], b"docs/guide.md"));
    }
}

// ---------------------------------------------------------------------------
// `repo_logmsg_reencode()` (pretty.c:708-775)
// ---------------------------------------------------------------------------

/// ```c
/// const char *repo_logmsg_reencode(struct repository *r, const struct commit *commit,
///                                  char **commit_encoding, const char *output_encoding)
/// {
///         static const char *utf8 = "UTF-8";
///         const char *msg = repo_get_commit_buffer(r, commit, NULL);
///
///         if (!output_encoding || !*output_encoding) {
///                 if (commit_encoding)
///                         *commit_encoding = get_header(msg, "encoding");
///                 return msg;
///         }
///         encoding = get_header(msg, "encoding");
///         use_encoding = encoding ? encoding : utf8;
///         if (same_encoding(use_encoding, output_encoding)) {
///                 if (!encoding)
///                         return msg;
///                 out = …msg…;
///         } else {
///                 out = reencode_string(msg, output_encoding, use_encoding);
///         }
///         if (out)
///                 out = replace_encoding_header(out, output_encoding);
///         return out ? out : msg;
/// }
/// ```
///
/// (`pretty.c:708-775`.) Four behaviours fall out of that shape and each one is
/// observable:
///
/// * `--encoding=none` stores the *empty* string, which is the early return: the
///   commit is rendered exactly as it is stored, `encoding` header and all.
/// * a commit with no `encoding` header is assumed to be UTF-8, so
///   `--encoding=ISO-8859-1` re-codes it even though nothing says it is UTF-8.
/// * the header is rewritten to name the encoding the message is now in, and
///   *dropped* when that is UTF-8 — so `git log --encoding=UTF-8 --pretty=raw` shows
///   no `encoding` line even for a commit that carries one.
/// * a conversion `iconv(3)` cannot do — an unknown charset name, or a character
///   the target cannot represent — is not an error: `reencode_string()` returns
///   NULL and the stored bytes are printed unchanged.
///
/// The buffer is rewritten in place, which is where `show_log()` reads it from.
pub(crate) fn logmsg_reencode(data: &mut Vec<u8>, output_encoding: &str) {
    if output_encoding.is_empty() {
        return;
    }
    let header = commit_header(data, b"encoding");
    let use_encoding = header.clone().unwrap_or_else(|| "UTF-8".to_string());
    if super::mailinfo::same_encoding(&use_encoding, output_encoding) {
        // Nothing to convert; only the header still has to be brought in line, and
        // a commit that has none needs even that.
        if header.is_none() {
            return;
        }
    } else {
        let Some(out) = super::mailinfo::reencode(data, &use_encoding, output_encoding) else {
            return;
        };
        *data = out;
    }
    replace_encoding_header(data, output_encoding);
}

/// `get_header(msg, key)` → `find_commit_header()`: the value of a header line in
/// the commit's header block, which ends at the first empty line.
fn commit_header(data: &[u8], key: &[u8]) -> Option<String> {
    for line in data.split(|b| *b == b'\n') {
        if line.is_empty() {
            return None;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(value) = rest.strip_prefix(b" ") {
                return Some(String::from_utf8_lossy(value).into_owned());
            }
        }
    }
    None
}

/// ```c
/// static char *replace_encoding_header(char *buf, const char *encoding)
/// {
///         /* guess if there is an encoding header before a \n\n */
///         while (!starts_with(cp, "encoding ")) {
///                 cp = strchr(cp, '\n');
///                 if (!cp || *++cp == '\n')
///                         return buf;
///         }
///         start = cp - buf;
///         cp = strchr(cp, '\n');
///         if (!cp)
///                 return buf; /* should not happen but be defensive */
///         len = cp + 1 - (buf + start);
///
///         if (is_encoding_utf8(encoding)) {
///                 /* we have re-coded to UTF-8; drop the header */
///                 strbuf_remove(&tmp, start, len);
///         } else {
///                 /* just replaces XXXX in 'encoding XXXX\n' */
///                 strbuf_splice(&tmp, start + strlen("encoding "),
///                               len - strlen("encoding \n"), encoding, strlen(encoding));
///         }
/// }
/// ```
///
/// (`pretty.c:677-707`.) The scan stops at the blank line that ends the header
/// block, so a body line beginning `encoding ` is never mistaken for the header.
fn replace_encoding_header(data: &mut Vec<u8>, encoding: &str) {
    let mut at = 0usize;
    let start = loop {
        if data[at..].starts_with(b"encoding ") {
            break at;
        }
        let Some(nl) = data[at..].iter().position(|b| *b == b'\n') else { return };
        at += nl + 1;
        // `*++cp == '\n'`: the blank line that ends the header block.
        if data.get(at) == Some(&b'\n') || at >= data.len() {
            return;
        }
    };
    let Some(nl) = data[start..].iter().position(|b| *b == b'\n') else { return };
    let end = start + nl + 1;
    if super::mailinfo::is_utf8_name(encoding) {
        data.drain(start..end);
    } else {
        let value_at = start + b"encoding ".len();
        data.splice(value_at..end - 1, encoding.bytes());
    }
}

/// The header lines of a commit that [`Pretty::Raw`] does not reconstruct from the
/// parsed object: everything that is not `tree`, `parent`, `author` or `committer`,
/// continuation lines (a leading space, as `gpgsig` uses) included.
///
/// The scan stops at the blank line that ends the header block, so a body line
/// starting with one of those words is never mistaken for a header.
fn extra_headers(data: &[u8]) -> Vec<&[u8]> {
    const RECONSTRUCTED: [&[u8]; 4] = [b"tree ", b"parent ", b"author ", b"committer "];
    let mut out = Vec::new();
    let mut in_reconstructed = false;
    for line in data.split(|b| *b == b'\n') {
        if line.is_empty() {
            break;
        }
        // A continuation line belongs to whatever header opened it.
        if line.starts_with(b" ") {
            if !in_reconstructed {
                out.push(line);
            }
            continue;
        }
        in_reconstructed = RECONSTRUCTED.iter().any(|k| line.starts_with(k));
        if !in_reconstructed {
            out.push(line);
        }
    }
    out
}
