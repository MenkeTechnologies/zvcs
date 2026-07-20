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
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * **Every other recognised option.** They are recognised for the purpose of argument
//!   classification — that is what git does, and it is what decides the exit-128 output
//!   above — but under `--i-still-use-this` a recognised option this module does not
//!   implement bails rather than being ignored. `-p`/`--patch`, `--stat` and friends,
//!   `--pretty`/`--format`, `--graph`, date/author/grep filters, `-M`/`-C`, and
//!   `--decorate` all land here.
//! * **Pathspec filtering** and **multiple or non-single revisions** (`a..b`, `^a`,
//!   `a...b`) bail under `--i-still-use-this`.
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
    "--abbrev-commit",
    "--no-abbrev-commit",
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
    "--root",
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

/// The result of reproducing `setup_revisions` over the argument list.
#[derive(Default)]
struct Parsed {
    no_renames: bool,
    max_count: Option<usize>,
    revs: Vec<String>,
    pathspecs: Vec<String>,
    /// The first recognised option this module does not implement, if any.
    unimplemented: Option<String>,
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

    let mut parsed = match parse_args(&repo, &args, phase1.quiet) {
        Ok(p) => p,
        Err(f) => {
            eprint!("{}", f.text);
            return Ok(ExitCode::from(f.code));
        }
    };
    if parsed.unimplemented.is_none() {
        parsed.unimplemented = phase1.unimplemented;
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
fn parse_args(repo: &gix::Repository, args: &[String], quiet: bool) -> Result<Parsed, Fatal> {
    let mut p = Parsed::default();
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
    if PREFIX_OPTS.iter().any(|pre| a.starts_with(pre)) {
        note_recognized(a, p);
        return Ok(1);
    }
    for opt in VALUE_OPTS {
        let attached = format!("{opt}=");
        if let Some(v) = a.strip_prefix(attached.as_str()) {
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

/// Walk and render, once `--i-still-use-this` has cleared the deprecation gate.
fn run(repo: &gix::Repository, parsed: Parsed) -> Result<ExitCode> {
    if let Some(flag) = &parsed.unimplemented {
        bail!(
            "{flag} is recognised but not ported (ported: --i-still-use-this, --raw, \
             --no-merges, --no-renames, -n/--max-count/-nN/-N)"
        );
    }
    if !parsed.pathspecs.is_empty() {
        bail!("pathspec filtering is not ported");
    }
    if parsed.revs.len() > 1 {
        bail!("multiple revisions are not ported");
    }

    // Resolve the starting tip. A bare `HEAD` may be unborn, which git reports as a
    // fatal error rather than as empty output.
    let tip = match parsed.revs.first() {
        // `parse_args` already resolved this spec, so the error arm is unreachable; it
        // is spelled out rather than `?`-ed to keep the gix error out of `anyhow`.
        Some(spec) => match repo.rev_parse(spec.as_str()) {
            Ok(rev) => match rev.single() {
                Some(id) => id.detach(),
                None => bail!("revision ranges are not ported"),
            },
            Err(e) => bail!("{spec}: {e}"),
        },
        None => match repo.head()?.try_peel_to_id()? {
            Some(id) => id.detach(),
            None => bail!("your current branch does not have any commits yet"),
        },
    };

    let renames = !parsed.no_renames && renames_enabled(repo);
    let abbrev = base_abbrev(repo)?;

    // Newest-first by commit date, the default `git log` ordering.
    let walk = repo
        .rev_walk([tip])
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

        let new_tree = commit.tree_id()?.detach();
        let old_tree = match parents.first() {
            Some(p) => Some(repo.find_object(*p)?.peel_to_tree()?.id),
            None => None, // root commit: diff against the empty tree
        };

        let mut changes: Vec<Change> = Vec::new();
        walk_trees(repo, old_tree, Some(new_tree), BStr::new(""), &mut changes)?;

        // `cmd_whatchanged` leaves `always_show_header` off, so a commit that produced
        // no diff prints nothing at all and git restores the `--max-count` it spent.
        if changes.is_empty() {
            continue;
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
        render_commit(repo, &commit, &changes, abbrev, &mut out)?;
        shown += 1;
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
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
    abbrev: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    let author = commit.author()?;
    let time = author.time()?;

    out.extend_from_slice(format!("commit {}\n", commit.id()).as_bytes());
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
/// `Www Mmm <sp-padded-day> HH:MM:SS YYYY +ZZZZ`, in the commit's own offset. Done by
/// hand because gix's exported `DEFAULT` format uses an unpadded day (`%-d`) where git
/// space-pads it (`%e`), and the crate exposes no custom format string.
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
        "{} {} {:>2} {:02}:{:02}:{:02} {} {}{:02}{:02}",
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
