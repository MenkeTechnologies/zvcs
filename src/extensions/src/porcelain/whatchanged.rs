//! `git whatchanged` — commit history with the raw diff each commit introduces.
//!
//! Stock git documents the command as exactly `git log --raw --no-merges`, and as of
//! git 2.47 it is deprecated: it refuses to run at all unless `--i-still-use-this` is
//! passed. Both halves are ported here.
//!
//! ### Argument parsing is the dominant behaviour
//!
//! `cmd_whatchanged` runs `cmd_log_init` (i.e. `setup_revisions`) *before* the
//! deprecation check, so on modern git almost every invocation ends in one of a small
//! set of exit-128 paths, and *which* one depends entirely on how `setup_revisions`
//! classified the arguments. That classification is reproduced here, left to right,
//! because it decides the output long before any history is walked:
//!
//! * An argument after `--`, or after the first argument that failed to resolve as a
//!   revision, is a pathspec.
//! * An argument starting with `-` is looked up in git's revision/diff option tables.
//!   Recognised options that take a value accept it attached (`--grep=x`, `-S x` is
//!   also accepted as the next argv element). Unrecognised ones are *remembered*, not
//!   fatal yet — `cmd_log_init` reports the first one only after `setup_revisions`
//!   returns, so an earlier bad revision wins.
//! * `^<rev>` that does not resolve is `fatal: bad revision '<rev>'`; it never falls
//!   back to being a pathspec.
//! * Any other argument is tried as a revision. On failure git runs `verify_filename`
//!   over that argument and every argument after it: an option-looking one is
//!   `option '%s' must come before non-option arguments`, one that looks like a
//!   pathspec (leading `:`, or an unescaped `*`, `?` or `[`) or names an existing path
//!   is accepted, the first remaining one is `ambiguous argument '%s': unknown revision
//!   or path not in the working tree.`, and a later one is `%s: no such path in the
//!   working tree.`. All of these are exit 128.
//!
//! Two passes, not one. `cmd_log_init` first runs `parse_options` over
//! `builtin_log_options` and *removes* what it matches (`--decorate`, `--source`,
//! `--use-mailmap`, `-q`, and `--i-still-use-this` itself), so those never reach
//! `setup_revisions`: `-n --no-decorate 5` reads `5` as the count, and
//! `-S --i-still-use-this` leaves `-S` without a value.
//!
//! After the loop the mutually exclusive combinations are rejected in this exact
//! order, each verified by running the pair against stock git:
//!
//! 1. `--combined-all-paths` without `-c`/`--cc` or a later merge-diff selector
//! 2. `--name-only` / `--name-status` / `--check` / `-s` used together
//! 3. `-G` / `-S` / `--find-object` used together, then `-G` with `--pickaxe-regex`
//! 4. `--follow` without exactly one non-exclude pathspec, then its pathspec magic
//! 5. `--walk-reflogs` with a history-limiting option, then with `--reverse`
//! 6. `--parents` (or `--graph`/`--simplify-merges`, which imply it) with `--children`
//! 7. `--graph` with `--show-linear-break`, `--reverse`, or `--no-walk`
//! 8. the first unrecognised option
//! 9. the deprecation notice
//!
//! `--cherry-mark`/`--cherry-pick` and `--walk-reflogs` over an excluded tip are the
//! two git raises mid-loop instead, ahead of all of the above. Every message and exit
//! code was reproduced against stock git 2.55.0 rather than inferred; the
//! `error:`-prefixed ones that come from `parse-options` rather than `die()` exit 129,
//! the rest exit 128.
//!
//! ### Covered (byte-identical stdout/stderr and exit code against stock git 2.55.0)
//!
//! * Every exit-128/129 argument-parsing path listed above.
//! * No `--i-still-use-this`: the 702-byte deprecation notice on stderr, empty stdout,
//!   exit 128. This is the whole behaviour of the stock command on modern git, so it
//!   is the path most callers actually hit.
//! * `--i-still-use-this`: the `medium` commit header (`commit`/`Author:`/`Date:` plus
//!   the four-space-indented message) followed by a blank line and the recursive
//!   `--raw` change list, newest commit first by commit date, commits separated by a
//!   blank line.
//! * Merge commits are skipped entirely (`--no-merges`), and — unlike `git log --raw`,
//!   which sets `always_show_header` — a commit whose diff is empty prints nothing and
//!   does not consume a `--max-count` slot. Both match `cmd_whatchanged`.
//! * A root commit is diffed against the empty tree, so it lists its whole tree as
//!   additions.
//! * `-n <n>` / `-n<n>` / `-<n>` / `--max-count[=]<n>`, `--no-renames`, `--raw` and
//!   `--no-merges` (both already implied by `whatchanged`), and a single `<rev>`
//!   (default `HEAD`).
//! * Object ids are abbreviated the way git's `diff_aligned_abbrev` does: `core.abbrev`
//!   when set, otherwise an auto width floored at 7, extended per id until unambiguous;
//!   an absent side renders as that many `0`s.
//! * **`log.*` display config, read by [`read_log_config`] the way git's `git_log_config`
//!   does, with the command line overriding — matching `git log`'s own handling.**
//!   * `log.abbrevCommit` (default false) abbreviates the `commit <id>` header to the same
//!     `find_unique_abbrev` width the raw diff uses; `--abbrev-commit` / `--no-abbrev-commit`
//!     override it.
//!   * `log.showRoot` (default true) shows the root commit's empty-tree diff. Set false it
//!     is suppressed, so — being TREESAME-empty — the root commit drops out of the output
//!     entirely and consumes no `--max-count` slot; `--root` forces it back on (git has no
//!     `--no-root`).
//!   * `log.date` is validated at read time exactly like git: an unknown value is
//!     `fatal: unknown date format <v>` (exit 128), raised ahead of the deprecation gate and
//!     of any `--date` override. `default` is a no-op; every other valid mode selects a date
//!     format `whatchanged` does not render, so it is deferred to the unported bail below
//!     (identical treatment to a command-line `--date`).
//! * **Path-limited traversal.** Everything after `--` — and any pre-`--` argument that
//!   resolved as a filename rather than a revision — is a pathspec. A commit is shown
//!   iff its `--raw` change list, filtered down to the paths the pathspec matches, is
//!   non-empty; the shown lines are the surviving ones. Matching is delegated to gix's
//!   `Pathspec`, git's own algorithm: literal paths and directory prefixes, shell globs
//!   (`*.rs`), and `:(glob)` / `:!exclude` / exclude-only magic. Slot accounting matches
//!   git — a path-filtered-empty commit consumes no `--max-count`.
//! * **Ref-set selectors.** `--all`, `--tags`, `--branches` and `--remotes` replace the
//!   default `HEAD` tip with the union of the matching commit refs (any explicit `<rev>`
//!   is unioned in too), so history from every selected ref is walked. An empty selection
//!   (`--tags` in a tagless repo) walks nothing and exits 0, as git does.
//! * **`--grep` commit-message filter** for the literal-pattern case. git greps the
//!   message with POSIX *basic* regex by default; a pattern that is a pure literal under
//!   the active flavour (`-F` forces literal, `-E`/`-P` widen the metacharacter set) is
//!   matched with an exact substring test — byte-identical to git's result. Multiple
//!   `--grep` OR (`--all-match` ANDs), `--invert-grep` negates, and `-i` folds ASCII case.
//!   A pattern carrying regex metacharacters is deferred (see below) rather than matched
//!   with the wrong flavour.
//! * **Unported options are applied lazily, matching git's exit code on empty output.**
//!   git only applies display/filter options to the commits it actually shows, so an
//!   invocation whose filters leave nothing to show exits 0 with empty output whatever
//!   those options are. This module mirrors that: a recognised-but-unported option no
//!   longer bails up front — it bails only when a commit survives every filter and would
//!   be rendered. `whatchanged --i-still-use-this --unified=1 --grep=X` over a repo where
//!   no message matches `X` now exits 0 (empty), exactly like git, instead of erroring.
//! * **Malformed option values are rejected at parse time, matching git's exit code.**
//!   An invalid `--pretty=`/`--format=` value is `fatal: invalid --pretty format` (128);
//!   `--min-parents=`/`--max-parents=` reject a non-integer (128); `--unified=`,
//!   `--stat-width=`/`--stat-count=`/`--stat-name-width=`, `--color=`, and `--word-diff=`
//!   reject a bad value as a `parse-options` `error:` (129). The value itself is still
//!   unimplemented, but a bad one now exits exactly as git does instead of reaching the
//!   generic recognised-but-unported path.
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * **Every other recognised option.** They are recognised for the purpose of argument
//!   classification — that is what git does, and it is what decides the exit-128 output
//!   above — but under `--i-still-use-this` a recognised option this module does not
//!   implement bails *when a commit would be shown* rather than being ignored (see the
//!   lazy-application note above; when the filters empty the walk, they exit 0 like git).
//!   `-p`/`--patch`, `--stat` and friends, a *valid* `--pretty`/`--format`, `--graph`,
//!   date/author filters, a non-literal `--grep`, `-M`/`-C`, and `--decorate` all land here.
//! * **Tip-set-broadening selectors this module does not resolve** — `--reflog`,
//!   `--walk-reflogs`/`-g`, `--stdin`, `--bisect`, `--not`, and patterned ref globs
//!   (`--glob=`, `--exclude=`, `--branches=`/`--tags=`/`--remotes=` with a value) — bail
//!   *before* the walk, since ignoring one could make real history look empty.
//! * **`:(attr:…)` attribute pathspecs** (which need the worktree attribute stack) and
//!   **multiple or non-single revisions** (`a..b`, `^a`, `a...b`) bail under
//!   `--i-still-use-this`.
//! * **Rename detection.** git's `diff.renames` defaults to on, so a commit that both
//!   adds and deletes files gets `R<score>` lines in a queue order produced by
//!   `diffcore_rename`. The vendored `gix-diff` rewrite tracker computes similarity
//!   from a line-based blob diff (`rewrites/tracker.rs`, `(old_len - removed_bytes) /
//!   max(old_len, new_len)`) rather than git's 64-byte spanhash `src_copied`, does not
//!   expose an integer score on `ChangeRef::Rewrite` at all, and emits changes in
//!   tree-walk order rather than git's rename-queue order. Neither the `R<score>`
//!   digits nor the line ordering could be reproduced, so when rename detection is
//!   active *and* a commit's diff contains both an addition and a deletion, this bails
//!   instead of printing plausible-looking wrong lines. `--no-renames` (or
//!   `diff.renames=false`) makes every commit reproducible.
//! * The option tables below are the ones that were verified against stock git. An
//!   option git recognises but that is absent from them is reported as
//!   `unrecognized argument`, which is wrong for that option; nothing silently passes.
//! * The auto abbreviation width is derived from gix's *packed* object count; git also
//!   estimates loose objects, so the two can differ by a hex digit in a repository with
//!   many loose objects and no pack.
//! * `i18n.commitEncoding` / the commit `encoding` header is not applied; the message
//!   bytes are passed through as stored.

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::EntryMode;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// Stock git's deprecation notice, byte-for-byte (702 bytes). Written to stderr, with
/// nothing on stdout, when `--i-still-use-this` is absent; exit code 128.
const DEPRECATION: &str = concat!(
    "'git whatchanged' is nominated for removal.\n",
    "\n",
    "hint: You can replace 'git whatchanged <opts>' with:\n",
    "hint:\tgit log <opts> --raw --no-merges\n",
    "hint: Or make an alias:\n",
    "hint:\tgit config set --global alias.whatchanged 'log --raw --no-merges'\n",
    "\n",
    "If you still use this command, here's what you can do:\n",
    "\n",
    "- read https://git-scm.com/docs/BreakingChanges.html\n",
    "- check if anyone has discussed this on the mailing\n",
    "  list and if they came up with something that can\n",
    "  help you: https://lore.kernel.org/git/?q=git%20whatchanged\n",
    "- send an email to <git@vger.kernel.org> to let us\n",
    "  know that you still use this command and were unable\n",
    "  to determine a suitable replacement\n",
    "\n",
    "fatal: refusing to run without --i-still-use-this\n",
);

/// The `S_IFMT` mask git uses to tell a *type* change (`T`) from a plain modification
/// (`M`); `100644` and `100755` share a type, `120000` and `160000` do not.
const IFMT: u16 = 0o170000;

/// git's `MINIMUM_ABBREV`, the floor `core.abbrev` is clamped to.
const MINIMUM_ABBREV: usize = 4;

/// git's `FALLBACK_DEFAULT_ABBREV`, the floor of the auto-computed width.
const FALLBACK_ABBREV: usize = 7;

/// A message git writes to stderr before exiting non-zero. `text` is complete, already
/// newline-terminated, and includes its own `fatal: ` / `error: ` prefix.
struct Fatal {
    text: String,
    code: u8,
}

impl Fatal {
    /// `die()`: the `fatal: ` prefix and exit 128.
    fn die(msg: impl Into<String>) -> Self {
        Fatal {
            text: format!("fatal: {}\n", msg.into()),
            code: 128,
        }
    }

    /// A `parse-options` complaint: the `error: ` prefix and exit 129.
    fn usage(msg: impl Into<String>) -> Self {
        Fatal {
            text: format!("error: {}\n", msg.into()),
            code: 129,
        }
    }
}

/// Options that take a value, either attached (`--grep=x`) or as the next argv element
/// (`--grep x`), together with git's message when the value is missing.
///
/// `-n` is the odd one out: it is spelled as a `parse-options` `error:` but still exits
/// 128, because `cmd_log_init` turns it into a `die()`.
const VALUE_OPTS: &[&str] = &[
    "--max-count",
    "--skip",
    "--since",
    "--after",
    "--until",
    "--before",
    "--author",
    "--committer",
    "--grep",
    "--exclude",
    "--date",
    "--encoding",
    "--diff-merges",
    "--output",
];

/// Value-taking options whose missing-value complaint comes from `parse-options`
/// (`error: switch \`X' requires a value`, exit 129) rather than from `die()`.
const VALUE_SWITCHES: &[char] = &['S', 'G', 'l', 'O'];

/// Recognised options that carry no value. Verified against stock git 2.55.0: each of
/// these reaches the deprecation notice rather than `unrecognized argument`.
const FLAG_OPTS: &[&str] = &[
    "--raw",
    "-p",
    "-u",
    "-s",
    "-t",
    "--patch",
    "--no-patch",
    "--patch-with-stat",
    "--patch-with-raw",
    "--stat",
    "--shortstat",
    "--numstat",
    "--dirstat",
    "--cumulative",
    "--compact-summary",
    "--summary",
    "--name-only",
    "--name-status",
    "--abbrev",
    "--no-abbrev",
    "--oneline",
    "--pretty",
    "--relative-date",
    "--date-order",
    "--author-date-order",
    "--topo-order",
    "--reverse",
    "--merges",
    "--no-merges",
    "--no-min-parents",
    "--no-max-parents",
    "--first-parent",
    // NB: `--abbrev-commit`, `--no-abbrev-commit` and `--root` are recognised but handled
    // explicitly in `consume_option` (they drive `log.abbrevCommit`/`log.showRoot`), so
    // they are intentionally absent from this generic recognised-but-unported list.
    "--all",
    "--branches",
    "--tags",
    "--remotes",
    "--reflog",
    "--stdin",
    "--bisect",
    "--no-walk",
    "--do-walk",
    "--all-match",
    "--invert-grep",
    "-i",
    "-E",
    "--pickaxe-all",
    "--pickaxe-regex",
    "--graph",
    "--no-graph",
    "--decorate",
    "--no-decorate",
    "--parents",
    "--children",
    "--boundary",
    "--left-right",
    "--left-only",
    "--right-only",
    "--cherry",
    "--cherry-mark",
    "--cherry-pick",
    "--source",
    "--full-history",
    "--simplify-merges",
    "--sparse",
    "--dense",
    "--ancestry-path",
    "--remove-empty",
    "--full-diff",
    "--follow",
    "--no-follow",
    "--find-renames",
    "--find-copies",
    "--find-copies-harder",
    "--no-renames",
    "--break-rewrites",
    "--function-context",
    "--ignore-all-space",
    "--ignore-space-change",
    "--ignore-space-at-eol",
    "--ignore-blank-lines",
    "--ignore-cr-at-eol",
    "--no-prefix",
    "--default-prefix",
    "--relative",
    "--no-relative",
    "--binary",
    "--full-index",
    "--irreversible-delete",
    "--histogram",
    "--patience",
    "--minimal",
    "--check",
    "--exit-code",
    "-R",
    "--textconv",
    "--no-textconv",
    "--ext-diff",
    "--no-ext-diff",
    "--expand-tabs",
    "--no-expand-tabs",
    "--notes",
    "--no-notes",
    "--show-signature",
    "--no-show-signature",
    "--walk-reflogs",
    "-g",
    "-c",
    "--cc",
    "-m",
    "--no-diff-merges",
    "--combined-all-paths",
    "--use-mailmap",
    "--no-use-mailmap",
    "--mailmap",
    "--log-size",
    "--show-linear-break",
    "--no-color",
    "--color",
    "-q",
    "--quiet",
    "--i-still-use-this",
];

/// Recognised `--name=value` families. The value is always attached for these; the
/// separable ones live in [`VALUE_OPTS`].
const PREFIX_OPTS: &[&str] = &[
    "--pretty=",
    "--format=",
    "--abbrev=",
    "--unified=",
    "--inter-hunk-context=",
    "--color=",
    "--decorate=",
    "--min-parents=",
    "--max-parents=",
    "--dirstat=",
    "--stat=",
    "--stat-width=",
    "--stat-name-width=",
    "--stat-count=",
    "--word-diff=",
    "--word-diff-regex=",
    "--src-prefix=",
    "--dst-prefix=",
    "--line-prefix=",
    "--notes=",
    "--ignore-submodules=",
    "--submodule=",
    "--anchored=",
    "--relative=",
    "--find-object=",
    "--show-linear-break=",
];

/// Recognised single-letter options whose value may be attached (`-M50`, `-U3`) and
/// which are also valid bare.
const OPTIONAL_ARG_SWITCHES: &[char] = &['M', 'C', 'B', 'U'];

/// Option state the post-loop checks read.
#[derive(Default)]
struct OptState {
    /// `-c`, `--cc`, or a combining `--diff-merges=` value: satisfies
    /// `--combined-all-paths` from anywhere on the command line.
    combine_merges: bool,
    /// Any other merge-diff selector seen *after* `--combined-all-paths`, which stock
    /// git also accepts as satisfying it.
    merge_diff_after_combined: bool,
    combined_all_paths: bool,
    follow: bool,
    reverse: bool,
    walk_reflogs: bool,
    /// Whether a history-limiting option was seen, which `--walk-reflogs` rejects.
    limiting: bool,
    parents: bool,
    children: bool,
    graph: bool,
    simplify_merges: bool,
    no_walk: bool,
    show_linear_break: bool,
    cherry_mark: bool,
    cherry_pick: bool,
    pickaxe_g: bool,
    pickaxe_s: bool,
    find_object: bool,
    pickaxe_regex: bool,
    /// The four mutually exclusive bits of git's `output_format`.
    out_bits: u8,
}

/// The `output_format` bits git's `--name-only`/`--name-status`/`--check`/`-s` check
/// looks at. More than one set at the end of parsing is fatal.
const OUT_NAME: u8 = 1;
const OUT_NAME_STATUS: u8 = 2;
const OUT_CHECK: u8 = 4;
const OUT_NO_OUTPUT: u8 = 8;

impl OptState {
    /// Record everything the post-loop checks read about `a`.
    ///
    /// `--cherry-mark`/`--cherry-pick` is the one conflict git raises while parsing
    /// rather than afterwards — it fires before a later bad revision does — and it
    /// names whichever of the pair is being parsed first.
    fn track(&mut self, a: &str) -> Result<(), Fatal> {
        match a {
            "--cherry-pick" if self.cherry_mark => {
                return Err(Fatal::die(
                    "options '--cherry-pick' and '--cherry-mark' cannot be used together",
                ));
            }
            "--cherry-mark" if self.cherry_pick => {
                return Err(Fatal::die(
                    "options '--cherry-mark' and '--cherry-pick' cannot be used together",
                ));
            }
            _ => {}
        }
        match a {
            "--cherry-mark" => self.cherry_mark = true,
            "--cherry-pick" => self.cherry_pick = true,
            "--graph" => self.graph = true,
            "--simplify-merges" => self.simplify_merges = true,
            "--no-walk" => self.no_walk = true,
            "--do-walk" => self.no_walk = false,
            "--show-linear-break" => self.show_linear_break = true,
            _ => {}
        }
        if a.starts_with("--no-walk=") {
            self.no_walk = true;
        }
        if a.starts_with("--show-linear-break=") {
            self.show_linear_break = true;
        }
        if a.starts_with("--find-object=") {
            self.find_object = true;
        }
        if !a.starts_with("--") {
            if a.starts_with("-S") {
                self.pickaxe_s = true;
            }
            if a.starts_with("-G") {
                self.pickaxe_g = true;
            }
        }
        match a {
            // `-c`/`--cc` satisfy `--combined-all-paths` wherever they appear; every
            // other merge-diff selector only satisfies it when it comes after.
            "-c" | "--cc" => self.combine_merges = true,
            "-m" | "--no-diff-merges" => self.note_merge_diff(),
            "--combined-all-paths" => self.combined_all_paths = true,
            "--follow" => self.follow = true,
            "--reverse" => self.reverse = true,
            "-g" | "--walk-reflogs" => self.walk_reflogs = true,
            "--parents" => self.parents = true,
            "--children" => self.children = true,
            "--pickaxe-regex" => self.pickaxe_regex = true,
            // `-s` (`--no-patch`) *assigns* `NO_OUTPUT`, clearing the other three,
            // which is why `--name-only -s` is fine but `-s --name-only` is not.
            "-s" | "--no-patch" => self.out_bits = OUT_NO_OUTPUT,
            "--name-only" => self.out_bits |= OUT_NAME,
            "--name-status" => self.out_bits |= OUT_NAME_STATUS,
            "--check" => self.out_bits |= OUT_CHECK,
            // Anything that turns real output back on clears `NO_OUTPUT`.
            "-p" | "-u" | "--patch" | "--raw" | "--patch-with-stat" | "--patch-with-raw" => {
                self.out_bits &= !OUT_NO_OUTPUT;
            }
            _ => {}
        }
        if REFLOG_LIMITING.contains(&a) {
            self.limiting = true;
        }
        Ok(())
    }

    /// `--children` conflicts with anything that turns parent rewriting on, which
    /// `--graph` and `--simplify-merges` both do implicitly.
    fn parents_effective(&self) -> bool {
        self.parents || self.graph || self.simplify_merges
    }

    /// Record a non-combining merge-diff selector (`-m`, `--no-diff-merges`,
    /// `--diff-merges=on` and friends). It turns combined output back off, but stock
    /// git still lets it satisfy an earlier `--combined-all-paths`.
    fn note_merge_diff(&mut self) {
        self.combine_merges = false;
        if self.combined_all_paths {
            self.merge_diff_after_combined = true;
        }
    }
}

/// One side of a change: absent (`None`) means the path was added or deleted.
#[derive(Clone, Copy)]
struct Side {
    mode: EntryMode,
    id: ObjectId,
}

/// A single blob-level change, in the shape the `--raw` line needs.
struct Change {
    old: Option<Side>,
    new: Option<Side>,
    path: BString,
}

/// A tree entry, materialised so the borrow on the tree's buffer ends before we recurse.
struct Entry {
    mode: EntryMode,
    name: BString,
    id: ObjectId,
}

/// Options that make `--walk-reflogs` fatal, because they force git to build a limited
/// (topologically ordered) revision list. Determined by running each grammar option
/// alongside `-g` against stock git; `--reverse` has its own message and is not here.
const REFLOG_LIMITING: &[&str] = &[
    "--topo-order",
    "--date-order",
    "--author-date-order",
    "--graph",
    "--children",
    "--cherry-mark",
    "--cherry-pick",
    "--simplify-merges",
    "--ancestry-path",
];

/// Pathspec magic keywords that `--follow` tolerates. `exclude` is tolerated but the
/// pathspec it marks does not count towards the one `--follow` requires.
const FOLLOW_OK_MAGIC: &[&str] = &["top", "exclude"];

/// A `--grep` commit-message filter, reproduced for the literal-pattern case.
///
/// git greps the commit message with `regcomp`, defaulting to POSIX *basic* regular
/// expressions (`-E` selects extended, `-F` fixed strings, `-P` PCRE). Only patterns
/// that are pure literals under the active flavour are honoured here — for those, git's
/// regex match degenerates to a substring test, which is reproduced exactly. A pattern
/// carrying any regex metacharacter is left to the deferred-unimplemented path instead of
/// being matched with the wrong flavour. Multiple `--grep` are OR-ed (`--all-match`
/// AND-s them), `--invert-grep` negates the verdict, and `-i` folds ASCII case.
#[derive(Default)]
struct GrepFilter {
    patterns: Vec<String>,
    ignore_case: bool,
    all_match: bool,
    invert: bool,
    fixed: bool,
    extended: bool,
}

impl GrepFilter {
    fn active(&self) -> bool {
        !self.patterns.is_empty()
    }

    /// Whether every collected pattern is a pure literal under the active flavour, so a
    /// substring test reproduces git's match. `-F` makes any pattern literal; otherwise a
    /// regex metacharacter (basic, or the wider extended set) disqualifies it.
    fn is_faithful(&self) -> bool {
        if self.fixed {
            return true;
        }
        let specials: &[u8] = if self.extended {
            b".*[]^$\\+?(){}|"
        } else {
            b".*[]^$\\"
        };
        self.patterns
            .iter()
            .all(|p| !p.bytes().any(|b| specials.contains(&b)))
    }

    /// Whether a commit with this message is kept. Only called once [`is_faithful`] has
    /// confirmed the substring test is exact.
    fn keeps(&self, message: &[u8]) -> bool {
        let hay: Vec<u8> = if self.ignore_case {
            message.to_ascii_lowercase()
        } else {
            message.to_vec()
        };
        let test = |pat: &str| {
            let needle = if self.ignore_case {
                pat.to_ascii_lowercase()
            } else {
                pat.to_string()
            };
            contains_subslice(&hay, needle.as_bytes())
        };
        let matched = if self.all_match {
            self.patterns.iter().all(|p| test(p))
        } else {
            self.patterns.iter().any(|p| test(p))
        };
        matched != self.invert
    }
}

/// Whether `haystack` contains `needle` as a contiguous run (`needle` empty ⇒ true).
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The result of reproducing `setup_revisions` over the argument list.
#[derive(Default)]
struct Parsed {
    no_renames: bool,
    /// `log.abbrevCommit` (default) as overridden by `--abbrev-commit`/`--no-abbrev-commit`:
    /// abbreviate the `commit <id>` header line to git's `find_unique_abbrev` width.
    abbrev_commit: bool,
    /// `log.showRoot` (default true) as overridden by `--root`: when false the root commit's
    /// empty-tree diff is suppressed, so — like any TREESAME commit — it is dropped entirely.
    show_root: bool,
    max_count: Option<usize>,
    revs: Vec<String>,
    pathspecs: Vec<String>,
    /// The first recognised option this module does not implement, if any. Consulted
    /// only when a commit actually survives filtering and is about to be rendered — git
    /// applies these options to *shown* commits, so an invocation whose filters leave
    /// nothing to show produces empty output and exit 0 no matter what they are.
    unimplemented: Option<String>,
    /// A collected `--grep` filter. Applied when every pattern is literal-faithful;
    /// otherwise it feeds `unimplemented` so a shown commit still bails.
    grep: GrepFilter,
    /// Ref-set selectors that replace the default `HEAD` tip with a union of refs.
    select_all: bool,
    select_tags: bool,
    select_branches: bool,
    select_remotes: bool,
    /// A tip-set-broadening selector this module does not implement (`--reflog`,
    /// `--walk-reflogs`, `--stdin`, `--bisect`, a patterned `--glob=`/`--exclude=`/
    /// `--branches=`/… selector). Bailed *before* the walk, because ignoring it could let
    /// this module report exit-0-empty while git still has history to show.
    set_broadening: Option<String>,
}

/// The `log.*` display config `whatchanged` shares with `git log`, read once up front by
/// [`read_log_config`] the way git's `git_log_config` does.
struct LogConfig {
    /// `log.abbrevCommit` (default false): abbreviate the `commit <id>` header.
    abbrev_commit: bool,
    /// `log.showRoot` (default true): show the root commit's empty-tree diff.
    show_root: bool,
    /// `log.date` is set to a valid mode other than `default`. `whatchanged` only renders
    /// git's `DATE_NORMAL`, so any other mode is deferred to the unported bail (like a
    /// command-line `--date`). An *invalid* `log.date` is fatal at read time instead.
    date_unsupported: bool,
}

/// Read the `log.*` display config, reproducing `git_log_config`'s validation of
/// `log.date`: an unknown value is `fatal: unknown date format <v>` (exit 128), raised
/// before the deprecation gate and before argument parsing, matching stock git 2.55.0.
fn read_log_config(repo: &gix::Repository) -> Result<LogConfig, Fatal> {
    let snap = repo.config_snapshot();
    let abbrev_commit = snap.boolean("log.abbrevCommit").unwrap_or(false);
    // git's `log.showRoot` defaults to true; only an explicit false suppresses the root.
    let show_root = snap.boolean("log.showRoot").unwrap_or(true);
    let date_unsupported = match snap.string("log.date") {
        None => false,
        Some(v) => {
            let v = v.to_str_lossy();
            // Same validation git applies via `parse_date_format`; invalid ⇒ fatal 128.
            validate_date_format(&v)?;
            // `default` renders exactly `DATE_NORMAL`, which is all `whatchanged` produces,
            // so it is a no-op; every other (valid) mode changes the `Date:` line.
            v.as_ref() != "default"
        }
    };
    Ok(LogConfig {
        abbrev_commit,
        show_root,
        date_unsupported,
    })
}

/// `git whatchanged` — see the module documentation for the covered surface.
pub fn whatchanged(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("whatchanged") => &args[1..],
        _ => args,
    };

    // git runs the deprecation check inside `cmd_whatchanged`, i.e. after repository
    // setup, so a missing repository is still reported first.
    let repo = gix::discover(".")?;

    // git reads `log.*` display config in `git_log_config`, before `setup_revisions` and
    // before the deprecation gate. An invalid `log.date` is fatal here — ahead of the
    // deprecation notice and of any argument-parse error — even when a valid `--date` on
    // the command line would otherwise take over. Verified against stock git 2.55.0.
    let cfg = match read_log_config(&repo) {
        Ok(c) => c,
        Err(f) => {
            eprint!("{}", f.text);
            return Ok(ExitCode::from(f.code));
        }
    };

    // `cmd_log_init` runs its own `parse_options` pass over `builtin_log_options` and
    // *removes* what it matches before `setup_revisions` ever sees the array. Those
    // options are therefore invisible below: `-n --no-decorate 5` reads 5, not
    // `--no-decorate`, and `-S --i-still-use-this` is left without a value.
    let phase1 = match extract_log_options(args) {
        Ok(p) => p,
        Err(f) => {
            eprint!("{}", f.text);
            return Ok(ExitCode::from(f.code));
        }
    };
    let (opted_in, args) = (phase1.opted_in, phase1.rest);

    let mut parsed = match parse_args(&repo, &args, phase1.quiet, &cfg) {
        Ok(p) => p,
        Err(f) => {
            eprint!("{}", f.text);
            return Ok(ExitCode::from(f.code));
        }
    };
    if parsed.unimplemented.is_none() {
        parsed.unimplemented = phase1.unimplemented;
    }
    // A valid but non-default `log.date` selects a date format `whatchanged` does not
    // render (it only produces `DATE_NORMAL`), exactly like a command-line `--date`: defer
    // it to the render-time unported bail rather than silently emitting a wrong `Date:`
    // line. A filter that empties the walk still exits 0, matching git.
    if parsed.unimplemented.is_none() && cfg.date_unsupported {
        parsed.unimplemented = Some("log.date".to_string());
    }

    if !opted_in {
        eprint!("{DEPRECATION}");
        return Ok(ExitCode::from(128));
    }

    run(&repo, parsed)
}

/// What `cmd_log_init`'s `parse_options` pass takes out of the argument list.
struct Phase1 {
    opted_in: bool,
    /// `-q`/`--quiet` is extracted here but still sets `NO_OUTPUT`, so it participates
    /// in the `--name-only`/`--name-status`/`--check`/`-s` conflict below.
    quiet: bool,
    /// The first extracted option this module does not implement.
    unimplemented: Option<String>,
    /// The arguments `setup_revisions` actually sees.
    rest: Vec<String>,
}

/// Options `cmd_log_init` consumes and removes, verified one at a time against stock
/// git with `git whatchanged -n <option> 5`: an option that leaves `5` as the count was
/// removed before `setup_revisions` ran. The pass stops at `--`.
const LOG_OPTS: &[&str] = &[
    "--decorate",
    "--no-decorate",
    "--clear-decorations",
    "--source",
    "--no-source",
    "--use-mailmap",
    "--no-use-mailmap",
    "--mailmap",
    "--no-mailmap",
    "-q",
    "--quiet",
    "--no-quiet",
];

/// `--name=value` forms `cmd_log_init` consumes and removes.
const LOG_PREFIX_OPTS: &[&str] = &["--decorate-refs=", "--decorate-refs-exclude="];

/// git's `--decorate=` values.
const DECORATE_VALUES: &[&str] = &["short", "full", "auto", "no"];

fn extract_log_options(args: &[String]) -> Result<Phase1, Fatal> {
    let mut out = Phase1 {
        opted_in: false,
        quiet: false,
        unimplemented: None,
        rest: Vec::with_capacity(args.len()),
    };
    let mut seen_dashdash = false;
    for a in args {
        if seen_dashdash {
            out.rest.push(a.clone());
            continue;
        }
        if a == "--" {
            seen_dashdash = true;
            out.rest.push(a.clone());
            continue;
        }
        if a == "--i-still-use-this" {
            out.opted_in = true;
            continue;
        }
        let s = a.as_str();
        let extracted = if let Some(v) = s.strip_prefix("--decorate=") {
            if !DECORATE_VALUES.contains(&v) {
                return Err(Fatal::die(format!("invalid --decorate option: {v}")));
            }
            true
        } else {
            LOG_OPTS.contains(&s) || LOG_PREFIX_OPTS.iter().any(|p| s.starts_with(p))
        };
        if extracted {
            match s {
                "-q" | "--quiet" => out.quiet = true,
                "--no-quiet" => out.quiet = false,
                _ => {}
            }
            if out.unimplemented.is_none() {
                out.unimplemented = Some(a.clone());
            }
            continue;
        }
        out.rest.push(a.clone());
    }
    Ok(out)
}

/// Reproduce `setup_revisions` and the checks that follow it, in git's order.
/// Everything that can end the command with a message lives here.
fn parse_args(
    repo: &gix::Repository,
    args: &[String],
    quiet: bool,
    cfg: &LogConfig,
) -> Result<Parsed, Fatal> {
    let mut p = Parsed::default();
    // Config supplies the defaults; the flags parsed below override them.
    p.abbrev_commit = cfg.abbrev_commit;
    p.show_root = cfg.show_root;
    let mut st = OptState::default();
    if quiet {
        st.out_bits = OUT_NO_OUTPUT;
    }
    let mut unrecognized: Option<String> = None;
    let mut seen_dashdash = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        if seen_dashdash {
            p.pathspecs.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            seen_dashdash = true;
            i += 1;
            continue;
        }

        if a.starts_with('-') && a.len() > 1 {
            i += consume_option(args, i, &mut p, &mut st, &mut unrecognized)?;
            continue;
        }

        // `^<rev>` never degrades into a pathspec.
        if let Some(rest) = a.strip_prefix('^') {
            if rest.is_empty() || repo.rev_parse(rest).is_err() {
                return Err(Fatal::die(format!("bad revision '{a}'")));
            }
            accept_rev(repo, &st, a, &mut p)?;
            i += 1;
            continue;
        }

        if !a.is_empty() && repo.rev_parse(a).is_ok() {
            accept_rev(repo, &st, a, &mut p)?;
            i += 1;
            continue;
        }

        // Not a revision: this argument and every one after it must be a filename.
        for (n, arg) in args[i..].iter().enumerate() {
            verify_filename(arg, n == 0)?;
        }
        p.pathspecs.extend(args[i..].iter().cloned());
        break;
    }

    // The post-loop checks, in the order stock git applies them.
    if st.combined_all_paths && !st.combine_merges && !st.merge_diff_after_combined {
        return Err(Fatal::die(
            "--combined-all-paths makes no sense without -c or --cc",
        ));
    }
    if st.out_bits.count_ones() > 1 {
        return Err(Fatal::die(
            "options '--name-only', '--name-status', '--check', and '-s' cannot be used together",
        ));
    }
    if [st.pickaxe_g, st.pickaxe_s, st.find_object]
        .iter()
        .filter(|b| **b)
        .count()
        > 1
    {
        return Err(Fatal::die(
            "options '-G', '-S', and '--find-object' cannot be used together",
        ));
    }
    if st.pickaxe_g && st.pickaxe_regex {
        return Err(Fatal::die(
            "options '-G' and '--pickaxe-regex' cannot be used together, \
             use '--pickaxe-regex' with '-S'",
        ));
    }
    if st.follow {
        // An exclude pathspec does not count towards the one `--follow` demands.
        let counted = p
            .pathspecs
            .iter()
            .filter(|s| !pathspec_is_exclude(s.as_str()))
            .count();
        if counted != 1 {
            return Err(Fatal::die("--follow requires exactly one pathspec"));
        }
        for s in &p.pathspecs {
            if let Some(magic) = unsupported_follow_magic(s) {
                return Err(Fatal::die(format!(
                    "pathspec magic not supported by --follow: '{magic}'"
                )));
            }
        }
    }
    if st.walk_reflogs {
        if st.limiting {
            return Err(Fatal::die(
                "cannot combine --walk-reflogs with history-limiting options",
            ));
        }
        if st.reverse {
            return Err(Fatal::die(
                "options '--reverse' and '--walk-reflogs' cannot be used together",
            ));
        }
    }
    if st.parents_effective() && st.children {
        return Err(Fatal::die(
            "options '--parents' and '--children' cannot be used together",
        ));
    }
    if st.graph && st.show_linear_break {
        return Err(Fatal::die(
            "options '--show-linear-break' and '--graph' cannot be used together",
        ));
    }
    if st.graph && st.reverse {
        return Err(Fatal::die(
            "options '--graph' and '--reverse' cannot be used together",
        ));
    }
    if st.graph && st.no_walk {
        return Err(Fatal::die(
            "options '--no-walk' and '--graph' cannot be used together",
        ));
    }
    if let Some(u) = unrecognized {
        return Err(Fatal::die(format!("unrecognized argument: {u}")));
    }
    Ok(p)
}

/// Record an accepted revision.
///
/// A reflog walk cannot start from an excluded tip, and git raises that the moment it
/// processes the revision — ahead of every post-loop check — but only when
/// `--walk-reflogs` already appeared earlier on the command line.
fn accept_rev(
    repo: &gix::Repository,
    st: &OptState,
    spec: &str,
    p: &mut Parsed,
) -> Result<(), Fatal> {
    if st.walk_reflogs {
        if let Some(bottom) = reflog_bottom(repo, spec) {
            return Err(Fatal::die(format!("cannot walk reflogs for {bottom}")));
        }
    }
    p.revs.push(spec.to_string());
    Ok(())
}

/// Whether a pathspec is an exclusion (`:!p`, `:^p`, or `:(exclude)p`).
fn pathspec_is_exclude(s: &str) -> bool {
    if s.starts_with(":!") || s.starts_with(":^") {
        return true;
    }
    magic_keywords(s).is_some_and(|kw| kw.iter().any(|k| k.as_str() == "exclude"))
}

/// The first magic keyword in `s` that `--follow` rejects, if any.
fn unsupported_follow_magic(s: &str) -> Option<String> {
    let kw = magic_keywords(s)?;
    kw.into_iter()
        .find(|k| !FOLLOW_OK_MAGIC.contains(&k.as_str()))
}

/// The keywords of a long-form `:(a,b)path` pathspec; `None` when there are none.
fn magic_keywords(s: &str) -> Option<Vec<String>> {
    let rest = s.strip_prefix(":(")?;
    let end = rest.find(')')?;
    Some(
        rest[..end]
            .split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect(),
    )
}

/// The revision `--walk-reflogs` refuses to walk, spelled the way git reports it.
///
/// git expands `a..b` into `b ^a` and `a...b` into `a b ^<merge-base>`, and a reflog
/// walk cannot start from an excluded tip; `None` means this spec is a plain revision.
fn reflog_bottom(repo: &gix::Repository, spec: &str) -> Option<String> {
    if let Some(rest) = spec.strip_prefix('^') {
        return Some(rest.to_string());
    }
    if let Some((lhs, rhs)) = spec.split_once("...") {
        let l = if lhs.is_empty() { "HEAD" } else { lhs };
        let r = if rhs.is_empty() { "HEAD" } else { rhs };
        let l = repo.rev_parse_single(l).ok()?;
        let r = repo.rev_parse_single(r).ok()?;
        return repo
            .merge_base(l, r)
            .ok()
            .map(|base| base.detach().to_string());
    }
    if let Some((lhs, _)) = spec.split_once("..") {
        return Some(if lhs.is_empty() {
            "HEAD".to_string()
        } else {
            lhs.to_string()
        });
    }
    None
}

/// Classify one option-looking argument, returning how many argv elements it consumed.
///
/// An unrecognised option is only *remembered*: git reports it after `setup_revisions`
/// returns, so a bad revision later on the command line wins over it.
fn consume_option(
    args: &[String],
    i: usize,
    p: &mut Parsed,
    st: &mut OptState,
    unrecognized: &mut Option<String>,
) -> Result<usize, Fatal> {
    let a = args[i].as_str();
    // `--` ends option parsing, so it is never taken as an option's value.
    let next = args
        .get(i + 1)
        .map(String::as_str)
        .filter(|v| *v != "--");

    // Bookkeeping the post-loop checks need, done before anything returns early.
    st.track(a)?;

    // --- the options this module actually implements -------------------------------
    match a {
        // Both are exactly what `whatchanged` already does, so accepting them is a
        // no-op rather than an approximation.
        "--raw" | "--no-merges" => return Ok(1),
        "--no-renames" => {
            p.no_renames = true;
            return Ok(1);
        }
        // `log.abbrevCommit` command-line overrides. git has both spellings; there is no
        // value form here (that is `--abbrev=<n>`, which controls the width, not this flag).
        "--abbrev-commit" => {
            p.abbrev_commit = true;
            return Ok(1);
        }
        "--no-abbrev-commit" => {
            p.abbrev_commit = false;
            return Ok(1);
        }
        // `--root` forces the root commit's diff on, overriding `log.showRoot=false`. git
        // has no `--no-root`, so config is the only way to turn it off.
        "--root" => {
            p.show_root = true;
            return Ok(1);
        }
        "-n" => {
            let v = next.ok_or_else(|| Fatal {
                text: "error: -n requires an argument\n".into(),
                code: 128,
            })?;
            p.max_count = parse_count(v)?;
            return Ok(2);
        }
        "--max-count" => {
            let v = next.ok_or_else(|| {
                Fatal::die("Option '--max-count' requires a value")
            })?;
            p.max_count = parse_count(v)?;
            return Ok(2);
        }
        _ => {}
    }
    if let Some(v) = a.strip_prefix("--max-count=") {
        p.max_count = parse_count(v)?;
        return Ok(1);
    }
    // The `-nN` and `-N` shorthands. Guarded on a single leading dash so that long
    // options beginning with `n` (`--numstat`, `--name-only`) are not misread.
    if !a.starts_with("--") {
        if let Some(v) = a.strip_prefix("-n") {
            if !v.is_empty() {
                p.max_count = parse_count(v)?;
                return Ok(1);
            }
        }
        let digits = &a[1..];
        if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
            p.max_count = parse_count(digits)?;
            return Ok(1);
        }
    }

    // --- options git recognises but this module does not implement -------------------
    // They still have to be classified exactly, because that is what decides which
    // exit-128 message the command ends with.
    fn note_recognized(flag: &str, p: &mut Parsed) {
        if p.unimplemented.is_none() {
            p.unimplemented = Some(flag.to_string());
        }
    }

    if let Some(v) = a.strip_prefix("--diff-merges=") {
        validate_diff_merges(v)?;
        if matches!(v, "c" | "cc" | "combined" | "dense-combined") {
            st.combine_merges = true;
        } else {
            st.note_merge_diff();
        }
        note_recognized(a, p);
        return Ok(1);
    }

    // `--grep` companion flags. On their own (no `--grep`) they are inert in git — verified
    // against stock git 2.55.0 — so, unlike the rest of FLAG_OPTS, they must not mark the
    // command unimplemented; they only tune how a collected `--grep` pattern is matched.
    match a {
        "-i" | "--regexp-ignore-case" => {
            p.grep.ignore_case = true;
            return Ok(1);
        }
        "--all-match" => {
            p.grep.all_match = true;
            return Ok(1);
        }
        "--invert-grep" => {
            p.grep.invert = true;
            return Ok(1);
        }
        "-F" | "--fixed-strings" => {
            p.grep.fixed = true;
            return Ok(1);
        }
        // `-E`/`-P` widen the metacharacter set; treated the same for the literal check
        // (only a pure-literal pattern is honoured under either).
        "-E" | "--extended-regexp" | "-P" | "--perl-regexp" => {
            p.grep.extended = true;
            return Ok(1);
        }
        _ => {}
    }

    // Ref-set selectors resolved into walk tips (a union of the matching commit refs).
    match a {
        "--all" => {
            p.select_all = true;
            return Ok(1);
        }
        "--tags" => {
            p.select_tags = true;
            return Ok(1);
        }
        "--branches" => {
            p.select_branches = true;
            return Ok(1);
        }
        "--remotes" => {
            p.select_remotes = true;
            return Ok(1);
        }
        _ => {}
    }

    // Tip-set-broadening selectors that are recognised but not resolved here. They inject
    // history from outside the default `HEAD` tip (reflogs, stdin, bisect, patterned ref
    // globs), so ignoring one could make a non-empty history look empty. Remembered and
    // bailed *before* the walk rather than deferred, to never falsely report exit-0-empty.
    const SET_BROADENING: &[&str] = &[
        "--reflog",
        "--stdin",
        "--bisect",
        "--walk-reflogs",
        "-g",
        "--not",
        "--alternate-refs",
    ];
    const SET_BROADENING_PREFIX: &[&str] = &[
        "--glob=",
        "--exclude=",
        "--exclude-hidden=",
        "--branches=",
        "--tags=",
        "--remotes=",
    ];
    if SET_BROADENING.contains(&a) || SET_BROADENING_PREFIX.iter().any(|pre| a.starts_with(pre)) {
        if p.set_broadening.is_none() {
            p.set_broadening = Some(a.to_string());
        }
        return Ok(1);
    }

    if FLAG_OPTS.contains(&a) {
        note_recognized(a, p);
        return Ok(1);
    }

    // `--name=value` families, plus the validation git performs on the value.
    if let Some(v) = a.strip_prefix("--date=") {
        validate_date_format(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--diff-filter=") {
        validate_diff_filter(v, a)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--skip=") {
        let _ = parse_count(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    // `--no-walk` takes only these two values; any other spelling is not an option git
    // knows, so it joins the deferred unrecognised-argument path rather than dying here.
    if let Some(v) = a.strip_prefix("--no-walk=") {
        if v == "sorted" || v == "unsorted" {
            note_recognized(a, p);
            return Ok(1);
        }
        if unrecognized.is_none() {
            *unrecognized = Some(a.to_string());
        }
        return Ok(1);
    }
    // `--pretty=`/`--format=` both funnel to git's `get_commit_format`, which validates
    // the format string the moment it is parsed (a `die()`, exit 128) — ahead of the
    // deprecation notice and of any deferred unrecognised option.
    if let Some(v) = a.strip_prefix("--pretty=") {
        validate_pretty_format(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--format=") {
        validate_pretty_format(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    // Numeric and enum options git rejects at parse time. The value is not implemented
    // here regardless, but git's *exit code* on a malformed value (128 for the
    // `die()`-backed integer parses, 129 for the `parse-options` ones) is reproduced so
    // a fuzzed bad value matches rather than reaching the generic recognised path.
    if let Some(v) = a.strip_prefix("--min-parents=") {
        let _ = parse_count(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--max-parents=") {
        let _ = parse_count(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--unified=") {
        validate_unified(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--stat-width=") {
        validate_stat_num(v, "stat-width")?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--stat-count=") {
        validate_stat_num(v, "stat-count")?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--stat-name-width=") {
        validate_stat_num(v, "stat-name-width")?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--color=") {
        validate_color(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if let Some(v) = a.strip_prefix("--word-diff=") {
        validate_word_diff(v)?;
        note_recognized(a, p);
        return Ok(1);
    }
    if PREFIX_OPTS.iter().any(|pre| a.starts_with(pre)) {
        note_recognized(a, p);
        return Ok(1);
    }
    for opt in VALUE_OPTS {
        let attached = format!("{opt}=");
        if let Some(v) = a.strip_prefix(attached.as_str()) {
            // `--grep` is honoured (for literal patterns) rather than deferred, so it does
            // not mark the command unimplemented on its own.
            if *opt == "--grep" {
                p.grep.patterns.push(v.to_string());
                return Ok(1);
            }
            match *opt {
                "--date" => validate_date_format(v)?,
                "--skip" => {
                    let _ = parse_count(v)?;
                }
                "--diff-merges" => validate_diff_merges(v)?,
                _ => {}
            }
            note_recognized(a, p);
            return Ok(1);
        }
        if a == *opt {
            let v = next.ok_or_else(|| Fatal::die(format!("Option '{opt}' requires a value")))?;
            if *opt == "--grep" {
                p.grep.patterns.push(v.to_string());
                return Ok(2);
            }
            match *opt {
                "--date" => validate_date_format(v)?,
                "--skip" => {
                    let _ = parse_count(v)?;
                }
                "--diff-merges" => {
                    validate_diff_merges(v)?;
                    if matches!(v, "c" | "cc" | "combined" | "dense-combined") {
                        st.combine_merges = true;
                    } else {
                        st.note_merge_diff();
                    }
                }
                _ => {}
            }
            note_recognized(a, p);
            return Ok(2);
        }
    }
    if a == "--diff-filter" {
        let v = next.ok_or_else(|| Fatal::usage("option `diff-filter' requires a value"))?;
        validate_diff_filter(v, &format!("--diff-filter={v}"))?;
        note_recognized(a, p);
        return Ok(2);
    }

    // Single-letter switches. `-S`/`-G`/`-l`/`-O` require a value, attached or next;
    // `-M`/`-C`/`-B`/`-U` take an optional attached one.
    let bytes = a.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'-' && bytes[1] != b'-' {
        let c = bytes[1] as char;
        if VALUE_SWITCHES.contains(&c) {
            if a.len() > 2 {
                note_recognized(a, p);
                return Ok(1);
            }
            let v = next.ok_or_else(|| Fatal::usage(format!("switch `{c}' requires a value")))?;
            if c == 'l' {
                let _ = parse_count(v).map_err(|_| {
                    Fatal::usage(format!(
                        "switch `{c}' expects an integer value with an optional k/m/g suffix"
                    ))
                })?;
            }
            note_recognized(a, p);
            return Ok(2);
        }
        if OPTIONAL_ARG_SWITCHES.contains(&c) {
            note_recognized(a, p);
            return Ok(1);
        }
    }

    if unrecognized.is_none() {
        *unrecognized = Some(a.to_string());
    }
    Ok(1)
}

/// git's `verify_filename`: decide whether a non-revision argument is an acceptable
/// pathspec, and produce the exact complaint when it is not. `first` marks the argument
/// that failed revision resolution, which gets the `ambiguous argument` wording.
fn verify_filename(arg: &str, first: bool) -> Result<(), Fatal> {
    if arg.starts_with('-') {
        return Err(Fatal::die(format!(
            "option '{arg}' must come before non-option arguments"
        )));
    }
    if looks_like_pathspec(arg) || std::fs::symlink_metadata(arg).is_ok() {
        return Ok(());
    }
    if first {
        Err(Fatal::die(format!(
            "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'"
        )))
    } else {
        Err(Fatal::die(format!(
            "{arg}: no such path in the working tree.\n\
             Use 'git <command> -- <path>...' to specify paths that do not exist locally."
        )))
    }
}

/// git's `looks_like_pathspec`: a leading `:` is pathspec magic, and so is any
/// unescaped glob special (`*`, `?`, `[`).
fn looks_like_pathspec(arg: &str) -> bool {
    if arg.starts_with(':') {
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

/// git's `strtol_i` for commit counts. A negative count means "no limit", as in git.
fn parse_count(value: &str) -> Result<Option<usize>, Fatal> {
    let n: i64 = value
        .parse()
        .map_err(|_| Fatal::die(format!("'{value}': not an integer")))?;
    Ok(usize::try_from(n).ok())
}

/// git's `parse_date_format`: the accepted names, the `-local` suffix, and the three
/// value-carrying prefixes.
fn validate_date_format(v: &str) -> Result<(), Fatal> {
    const NAMES: &[&str] = &[
        "relative",
        "human",
        "iso8601",
        "iso",
        "iso8601-strict",
        "iso-strict",
        "rfc2822",
        "rfc",
        "short",
        "default",
        "raw",
        "unix",
        "local",
    ];
    if v.starts_with("format:") || v.starts_with("format-local:") || v.starts_with("auto:") {
        return Ok(());
    }
    let base = v.strip_suffix("-local").unwrap_or(v);
    if NAMES.contains(&base) {
        return Ok(());
    }
    Err(Fatal::die(format!("unknown date format {v}")))
}

/// git's `parse_diff_filter_opt`. `whole` is the spelling used in the message, which
/// repeats the option as written.
fn validate_diff_filter(v: &str, whole: &str) -> Result<(), Fatal> {
    for c in v.chars() {
        if c == '*' || "acdmrtuxb".contains(c.to_ascii_lowercase()) {
            continue;
        }
        return Err(Fatal::usage(format!(
            "unknown change class '{c}' in {whole}"
        )));
    }
    Ok(())
}

/// git's `diff_merges_parse_option` value set.
fn validate_diff_merges(v: &str) -> Result<(), Fatal> {
    const VALUES: &[&str] = &[
        "off",
        "none",
        "on",
        "first-parent",
        "1",
        "separate",
        "m",
        "combined",
        "c",
        "dense-combined",
        "cc",
        "remerge",
        "r",
    ];
    if VALUES.contains(&v) {
        Ok(())
    } else {
        Err(Fatal::die(format!(
            "invalid value for '--diff-merges': '{v}'"
        )))
    }
}

/// git's `get_commit_format`: a `--pretty`/`--format` value is valid when it is empty,
/// carries a `format:`/`tformat:` prefix, contains a `%` placeholder, or is a
/// case-insensitive prefix of one of the built-in format names. Anything else is
/// `fatal: invalid --pretty format: <v>` (exit 128), reported the instant the option is
/// parsed. The set of names is git's `builtin_formats` table in `pretty.c`.
fn validate_pretty_format(v: &str) -> Result<(), Fatal> {
    const NAMES: &[&str] = &[
        "raw",
        "medium",
        "short",
        "email",
        "mboxrd",
        "fuller",
        "full",
        "oneline",
        "reference",
    ];
    if v.is_empty()
        || v.starts_with("format:")
        || v.starts_with("tformat:")
        || v.contains('%')
    {
        return Ok(());
    }
    let sought = v.to_ascii_lowercase();
    if NAMES.iter().any(|n| n.starts_with(sought.as_str())) {
        return Ok(());
    }
    Err(Fatal::die(format!("invalid --pretty format: {v}")))
}

/// git's `--unified`/`-U` value parse: an empty value keeps the default; otherwise the
/// value must be a non-negative decimal integer (git tolerates overflow but rejects a
/// sign or any trailing non-digit). A bad value is a `parse-options` `error:`, exit 129.
fn validate_unified(v: &str) -> Result<(), Fatal> {
    if v.is_empty() || v.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(());
    }
    Err(Fatal::usage("--unified expects a numerical value"))
}

/// git's `--stat-width`/`--stat-count`/`--stat-name-width` parse via `parse_stat_value`:
/// an empty value keeps the default, a leading `-` is tolerated, and the rest must be
/// digits. A non-numeric value is a `parse-options` `error:`, exit 129.
fn validate_stat_num(v: &str, name: &str) -> Result<(), Fatal> {
    let digits = v.strip_prefix('-').unwrap_or(v);
    if v.is_empty() || (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())) {
        return Ok(());
    }
    Err(Fatal::usage(format!("{name} expects a numerical value")))
}

/// git's `--color` value parse: `always`, `auto`, or `never` (case-insensitive). Bare
/// `--color` is handled as a flag; only the `--color=<v>` form reaches here. A bad value
/// is a `parse-options` `error:`, exit 129.
fn validate_color(v: &str) -> Result<(), Fatal> {
    if matches!(v.to_ascii_lowercase().as_str(), "always" | "auto" | "never") {
        return Ok(());
    }
    Err(Fatal::usage(
        "option `color' expects \"always\", \"auto\", or \"never\"",
    ))
}

/// git's `--word-diff` value parse: `plain`, `porcelain`, `color`, or `none`. A bad
/// value is a `parse-options` `error:`, exit 129.
fn validate_word_diff(v: &str) -> Result<(), Fatal> {
    if matches!(v, "plain" | "porcelain" | "color" | "none") {
        return Ok(());
    }
    Err(Fatal::usage(format!("bad --word-diff argument: {v}")))
}

/// Walk and render, once `--i-still-use-this` has cleared the deprecation gate.
fn run(repo: &gix::Repository, mut parsed: Parsed) -> Result<ExitCode> {
    // A tip-set-broadening selector this module does not resolve: bail up front, because
    // silently ignoring it could make an invocation with real history look empty.
    if let Some(flag) = &parsed.set_broadening {
        bail!("{flag} selects history that is not ported");
    }

    // A `--grep` whose patterns are not all literal under the active flavour cannot be
    // matched faithfully (git defaults to POSIX basic regex); treat it like any other
    // unported option — deferred to render time — rather than matching with wrong rules.
    if parsed.grep.active() && !parsed.grep.is_faithful() && parsed.unimplemented.is_none() {
        parsed.unimplemented = Some("--grep".to_string());
    }
    let apply_grep = parsed.grep.active() && parsed.grep.is_faithful();

    // The unported-option bail is deferred: git applies these options only to commits it
    // actually shows, so an invocation whose filters leave nothing to show exits 0 with no
    // output regardless of them. The bail therefore fires per-commit, below, the moment a
    // commit survives filtering and would be rendered — not here.
    let has_selector =
        parsed.select_all || parsed.select_tags || parsed.select_branches || parsed.select_remotes;
    if !has_selector && parsed.revs.len() > 1 {
        bail!("multiple revisions are not ported");
    }

    // Path-limited traversal: everything after `--` (and any pre-`--` argument that
    // resolved as a filename) is a pathspec. A commit is shown iff, after filtering its
    // raw change list down to the paths matching the pathspec, at least one change
    // remains — exactly git's default history simplification over this `--no-merges`
    // walk. gix's `Pathspec` reproduces git's matcher (literal prefixes, shell globs,
    // `:(glob)`/`:!exclude` magic and exclude-only sets); attribute pathspecs
    // (`:(attr:…)`) are the one form it would need the worktree for, and those bail.
    let mut matcher = if parsed.pathspecs.is_empty() {
        None
    } else {
        Some(gix::Pathspec::new(
            repo,
            false,
            parsed.pathspecs.iter().map(String::as_str),
            false,
            || {
                Err::<gix::worktree::Stack, Box<dyn std::error::Error + Send + Sync>>(
                    "attribute pathspecs are not ported".into(),
                )
            },
        )?)
    };

    // Resolve the walk tips. Ref-set selectors (`--all`/`--tags`/`--branches`/`--remotes`)
    // replace the default `HEAD` with the union of the matching commit refs; an empty
    // selector set (e.g. `--tags` in a tagless repo) is not an error — git walks nothing
    // and exits 0. Otherwise a single `<rev>` is used, defaulting to `HEAD`, which may be
    // unborn (git reports that as a fatal error rather than as empty output).
    let tips: Vec<ObjectId> = if has_selector {
        selector_tips(repo, &parsed)?
    } else {
        match parsed.revs.first() {
            // `parse_args` already resolved this spec, so the error arm is unreachable; it
            // is spelled out rather than `?`-ed to keep the gix error out of `anyhow`.
            Some(spec) => match repo.rev_parse(spec.as_str()) {
                Ok(rev) => match rev.single() {
                    Some(id) => vec![id.detach()],
                    None => bail!("revision ranges are not ported"),
                },
                Err(e) => bail!("{spec}: {e}"),
            },
            None => match repo.head()?.try_peel_to_id()? {
                Some(id) => vec![id.detach()],
                None => bail!("your current branch does not have any commits yet"),
            },
        }
    };

    let renames = !parsed.no_renames && renames_enabled(repo);
    let abbrev = base_abbrev(repo)?;

    // Newest-first by commit date, the default `git log` ordering.
    let walk = repo
        .rev_walk(tips.iter().copied())
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()?;

    let limit = parsed.max_count.unwrap_or(usize::MAX);
    let mut out: Vec<u8> = Vec::new();
    let mut shown = 0usize;

    for info in walk {
        if shown >= limit {
            break;
        }
        let commit = info?.object()?;

        // `--no-merges`: a merge is dropped from the output, but the walk still
        // traverses through it, and it never consumes a `--max-count` slot.
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        if parents.len() > 1 {
            continue;
        }

        // `log.showRoot=false` suppresses the root commit's empty-tree diff. With nothing
        // to diff it is TREESAME-empty, so — like the empty-diff case below — the commit is
        // dropped entirely and consumes no `--max-count` slot. `--root` turns it back on.
        if !parsed.show_root && parents.is_empty() {
            continue;
        }

        // `--grep`: git filters on the commit message during traversal, before the diff,
        // and an excluded commit is dropped whether or not it changed anything.
        if apply_grep {
            let message = commit.message_raw()?;
            let bytes: &[u8] = &message[..];
            if !parsed.grep.keeps(bytes) {
                continue;
            }
        }

        let new_tree = commit.tree_id()?.detach();
        let old_tree = match parents.first() {
            Some(p) => Some(repo.find_object(*p)?.peel_to_tree()?.id),
            None => None, // root commit: diff against the empty tree
        };

        let mut changes: Vec<Change> = Vec::new();
        walk_trees(repo, old_tree, Some(new_tree), BStr::new(""), &mut changes)?;

        // Path limiting: keep only the changes whose path matches the pathspec. A
        // commit with no surviving change is `TREESAME` and is dropped without
        // consuming a `--max-count` slot, just like an empty diff below.
        if let Some(m) = matcher.as_mut() {
            changes.retain(|c| m.is_included(c.path.as_bstr(), Some(false)));
        }

        // `cmd_whatchanged` leaves `always_show_header` off, so a commit that produced
        // no diff prints nothing at all and git restores the `--max-count` it spent.
        if changes.is_empty() {
            continue;
        }

        // Deferred unported-option bail: this commit survives every filter and would be
        // rendered, but an option this module cannot honour (a patch/stat display mode, a
        // non-literal `--grep`, …) is active, so its raw rendering would not match git.
        // Reaching here means git *does* have output, so an honest bail is the right
        // answer; an invocation whose filters emptied the walk never gets here and exits 0.
        if let Some(flag) = &parsed.unimplemented {
            bail!(
                "{flag} is recognised but not ported (ported: --i-still-use-this, --raw, \
                 --no-merges, --no-renames, --grep, --all/--branches/--tags/--remotes, \
                 -n/--max-count/-nN/-N, --abbrev-commit/--no-abbrev-commit, --root, \
                 and log.abbrevCommit/log.showRoot/log.date config)"
            );
        }

        // git would run `diffcore_rename` over this pair set; see the module docs for
        // why that cannot be reproduced byte-identically from the vendored crates.
        if renames
            && changes.iter().any(|c| c.old.is_none())
            && changes.iter().any(|c| c.new.is_none())
        {
            bail!(
                "commit {} both adds and deletes paths, so git's rename detection would \
                 emit R<score> lines; the vendored gix-diff exposes no diffcore-rename \
                 score or queue order (re-run with --no-renames, or set diff.renames=false)",
                commit.id()
            );
        }

        if shown > 0 {
            out.push(b'\n');
        }
        render_commit(repo, &commit, &changes, parsed.abbrev_commit, abbrev, &mut out)?;
        shown += 1;
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Collect the walk tips for the ref-set selectors (`--all`/`--tags`/`--branches`/
/// `--remotes`), matching git's rev-list: each selected ref is peeled to its object and
/// only commit tips are kept (a tag pointing at a tree/blob contributes no history). Any
/// explicit `<rev>` on the command line is unioned in, as git does. `--all` is the union
/// of local branches, tags and remote-tracking branches; exotic ref namespaces
/// (`refs/stash`, notes, …) are not included, matching the common `for-each-ref` set.
fn selector_tips(repo: &gix::Repository, parsed: &Parsed) -> Result<Vec<ObjectId>> {
    let mut ids: Vec<ObjectId> = Vec::new();
    let refs = repo.references()?;
    if parsed.select_branches || parsed.select_all {
        add_ref_tips(repo, refs.local_branches()?, &mut ids)?;
    }
    if parsed.select_tags || parsed.select_all {
        add_ref_tips(repo, refs.tags()?, &mut ids)?;
    }
    if parsed.select_remotes || parsed.select_all {
        add_ref_tips(repo, refs.remote_branches()?, &mut ids)?;
    }
    // git's `--all` is "all refs in refs/, along with HEAD" — so a detached HEAD (or any
    // commit reachable only from HEAD) is included even though no ref names it.
    if parsed.select_all {
        if let Ok(mut head) = repo.head() {
            if let Ok(Some(id)) = head.try_peel_to_id() {
                ids.push(id.detach());
            }
        }
    }
    for spec in &parsed.revs {
        if let Ok(rev) = repo.rev_parse(spec.as_str()) {
            if let Some(id) = rev.single() {
                ids.push(id.detach());
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Peel each reference in `iter` to its target object and push the commit ids onto `ids`.
fn add_ref_tips<'a>(
    repo: &gix::Repository,
    iter: impl Iterator<
        Item = std::result::Result<gix::Reference<'a>, Box<dyn std::error::Error + Send + Sync + 'static>>,
    >,
    ids: &mut Vec<ObjectId>,
) -> Result<()> {
    for r in iter {
        let mut r = r.map_err(|e| anyhow!("reference iteration failed: {e}"))?;
        let id = r
            .peel_to_id()
            .map_err(|e| anyhow!("cannot resolve reference: {e}"))?
            .detach();
        if matches!(repo.find_object(id).map(|o| o.kind), Ok(gix::objs::Kind::Commit)) {
            ids.push(id);
        }
    }
    Ok(())
}

/// Whether git would run rename detection: `diff.renames` defaults to on, and only an
/// explicit false value turns it off (`copies`/`copy` turn it *up*, not off).
fn renames_enabled(repo: &gix::Repository) -> bool {
    match repo.config_snapshot().string("diff.renames") {
        None => true,
        Some(v) => !matches!(
            v.to_str_lossy().to_ascii_lowercase().as_str(),
            "false" | "no" | "off" | "0"
        ),
    }
}

/// The base abbreviation width for `--raw` object ids.
///
/// `core.abbrev` when set (clamped to `MINIMUM_ABBREV..=hexsz`, with `no`/`off`/`false`
/// meaning the full id), otherwise git's auto width: half the bit-length of the object
/// count, rounded up, floored at `FALLBACK_ABBREV`. Individual ids may be rendered
/// longer than this when they need it to stay unambiguous; the all-zero id of an absent
/// side is always rendered at exactly this width, as `diff_aligned_abbrev` does.
fn base_abbrev(repo: &gix::Repository) -> Result<usize> {
    let hexsz = repo.object_hash().len_in_hex();
    if let Some(v) = repo.config_snapshot().string("core.abbrev") {
        match v.to_str_lossy().as_ref() {
            "auto" => {}
            "no" | "off" | "false" => return Ok(hexsz),
            other => {
                let n: usize = other
                    .parse()
                    .map_err(|_| anyhow!("Invalid value for 'core.abbrev' = '{other}'"))?;
                return Ok(n.clamp(MINIMUM_ABBREV, hexsz));
            }
        }
    }
    let count = repo.objects.packed_object_count()?;
    let bits = u64::BITS - count.leading_zeros();
    Ok((bits.div_ceil(2) as usize).max(FALLBACK_ABBREV))
}

/// Render one commit: the `medium` header, its message, a blank line, then the raw
/// change lines. No `Merge:` line is ever needed because merges are filtered out.
fn render_commit(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    changes: &[Change],
    abbrev_commit: bool,
    abbrev: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    let author = commit.author()?;
    let time = author.time()?;

    // `log.abbrevCommit`/`--abbrev-commit`: git shortens the header id with
    // `find_unique_abbrev` at the same default width the raw diff uses.
    if abbrev_commit {
        out.extend_from_slice(
            format!("commit {}\n", short(repo, commit.id().detach(), abbrev)?).as_bytes(),
        );
    } else {
        out.extend_from_slice(format!("commit {}\n", commit.id()).as_bytes());
    }
    out.extend_from_slice(b"Author: ");
    out.extend_from_slice(&author.name[..]);
    out.extend_from_slice(b" <");
    out.extend_from_slice(&author.email[..]);
    out.extend_from_slice(b">\n");
    out.extend_from_slice(
        format!("Date:   {}\n\n", format_git_date(time.seconds, time.offset)).as_bytes(),
    );

    // git's `pp_remainder`: leading blank lines are dropped, every remaining line gets a
    // four-space indent, and an all-whitespace line keeps only that indent.
    let raw = commit.message_raw()?;
    let bytes: &[u8] = &raw[..];
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop(); // the newline terminating the last line, not an extra blank one
    }
    let mut started = false;
    for line in lines {
        let blank = line.iter().all(u8::is_ascii_whitespace);
        if blank && !started {
            continue;
        }
        started = true;
        out.extend_from_slice(b"    ");
        if !blank {
            out.extend_from_slice(line);
        }
        out.push(b'\n');
    }
    out.push(b'\n');

    for c in changes {
        render_raw(repo, c, abbrev, out)?;
    }
    Ok(())
}

/// `:<omode> <nmode> <ooid> <noid> <status>\t<path>` — git's raw diff line.
fn render_raw(repo: &gix::Repository, c: &Change, abbrev: usize, out: &mut Vec<u8>) -> Result<()> {
    let zeros = "0".repeat(abbrev);
    let (omode, ooid) = match c.old {
        Some(s) => (s.mode.value(), short(repo, s.id, abbrev)?),
        None => (0, zeros.clone()),
    };
    let (nmode, noid) = match c.new {
        Some(s) => (s.mode.value(), short(repo, s.id, abbrev)?),
        None => (0, zeros),
    };
    out.extend_from_slice(format!(":{omode:06o} {nmode:06o} {ooid} {noid} ").as_bytes());
    out.push(status(c));
    out.push(b'\t');
    out.extend_from_slice(&c.path);
    out.push(b'\n');
    Ok(())
}

/// git's `diff_abbrev_oid`: the id shortened to the configured width, extended until it
/// is unambiguous. `gix::Id::shorten` derives the same width from `core.abbrev` (or the
/// same auto formula) and performs the same disambiguation.
fn short(repo: &gix::Repository, id: ObjectId, abbrev: usize) -> Result<String> {
    if abbrev >= repo.object_hash().len_in_hex() {
        return Ok(id.to_hex().to_string());
    }
    Ok(id.attach(repo).shorten()?.to_string())
}

/// The status letter git prints for a change.
fn status(c: &Change) -> u8 {
    match (c.old, c.new) {
        (None, _) => b'A',
        (_, None) => b'D',
        (Some(o), Some(n)) => {
            if o.mode.value() & IFMT != n.mode.value() & IFMT {
                b'T'
            } else {
                b'M'
            }
        }
    }
}

/// Read the entries of `id` in stored (git-sorted) order; `None` is the empty tree.
fn read_entries(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Entry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let tree = repo.find_tree(id)?;
    Ok(tree
        .decode()?
        .entries
        .iter()
        .map(|e| Entry {
            mode: e.mode,
            name: BString::from(e.filename.to_vec()),
            id: e.oid.to_owned(),
        })
        .collect())
}

/// git's `tree-entry-comparison`: names compare byte-wise with an implicit `/` appended
/// to tree entries, so a blob and a tree of the same name never compare `Equal`.
fn entry_cmp(a: &Entry, b: &Entry) -> Ordering {
    let common = a.name.len().min(b.name.len());
    match a.name[..common].cmp(&b.name[..common]) {
        Ordering::Equal => {
            let ac = a
                .name
                .get(common)
                .copied()
                .or(a.mode.is_tree().then_some(b'/'));
            let bc = b
                .name
                .get(common)
                .copied()
                .or(b.mode.is_tree().then_some(b'/'));
            ac.cmp(&bc)
        }
        other => other,
    }
}

/// Depth-first merge-walk of two trees rooted at `prefix`, collecting blob-level
/// changes. `--raw` in the log family is always recursive and never reports the tree
/// entries themselves, so this always descends and only ever pushes non-tree entries.
fn walk_trees(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &BStr,
    out: &mut Vec<Change>,
) -> Result<()> {
    let lhs = read_entries(repo, old)?;
    let rhs = read_entries(repo, new)?;
    let (mut i, mut j) = (0usize, 0usize);

    while i < lhs.len() || j < rhs.len() {
        let order = match (lhs.get(i), rhs.get(j)) {
            (Some(a), Some(b)) => entry_cmp(a, b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => unreachable!("loop condition guarantees one side has an entry"),
        };
        match order {
            Ordering::Equal => {
                let (a, b) = (&lhs[i], &rhs[j]);
                i += 1;
                j += 1;
                if a.mode == b.mode && a.id == b.id {
                    continue;
                }
                let path = join(prefix, a.name.as_bstr());
                // `Equal` implies both sides are trees or neither is.
                if a.mode.is_tree() {
                    walk_trees(repo, Some(a.id), Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        old: Some(side(a)),
                        new: Some(side(b)),
                        path,
                    });
                }
            }
            Ordering::Less => {
                let a = &lhs[i];
                i += 1;
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    walk_trees(repo, Some(a.id), None, path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        old: Some(side(a)),
                        new: None,
                        path,
                    });
                }
            }
            Ordering::Greater => {
                let b = &rhs[j];
                j += 1;
                let path = join(prefix, b.name.as_bstr());
                if b.mode.is_tree() {
                    walk_trees(repo, None, Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        old: None,
                        new: Some(side(b)),
                        path,
                    });
                }
            }
        }
    }
    Ok(())
}

fn side(e: &Entry) -> Side {
    Side {
        mode: e.mode,
        id: e.id,
    }
}

fn join(prefix: &BStr, name: &BStr) -> BString {
    let mut p = BString::from(prefix.to_vec());
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// Format a commit time exactly like git's default `DATE_NORMAL`:
/// `Www Mmm <day> HH:MM:SS YYYY +ZZZZ`, in the commit's own offset. Done by hand because
/// gix exposes no custom format string. git's `date.c` renders the day with `%d`, which
/// is *unpadded* — a single-digit day is one character (`Jan 2`), never space-padded.
fn format_git_date(seconds: i64, offset: i32) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // Shift into the commit's local wall-clock time, then split into whole days since
    // the Unix epoch and seconds within the day. `div_euclid`/`rem_euclid` keep the
    // split correct for pre-1970 (negative) timestamps.
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

/// Convert a day count since the Unix epoch into a civil `(year, month, day)`, month and
/// day 1-based. Howard Hinnant's `civil_from_days`, exact over the representable range.
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
