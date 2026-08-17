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
//! ### What this module does
//!
//! `cmd_whatchanged` is `cmd_log` with two settings changed — the raw format becomes
//! the default when no other diff format is asked for, and `always_show_header` stays
//! off — so once the deprecation gate is cleared the walk and the rendering are handed
//! to [`super::log::whatchanged`], which runs `log`'s renderer under that flavour. What
//! stays here is everything that happens *before* the gate:
//!
//! * The whole `setup_revisions` classification above, with its exit-128/129 paths.
//! * `git_log_config`'s validation of `log.date`, which is fatal ahead of the
//!   deprecation notice and of any argument-parse error.
//! * The 702-byte deprecation notice (stderr, empty stdout, exit 128) when
//!   `--i-still-use-this` is absent — the whole behaviour of the stock command on
//!   modern git, and the path most callers hit.
//! * Dropping `--i-still-use-this` itself, which `cmd_log_init`'s `parse_options` pass
//!   removes before `setup_revisions` ever sees the array.
//!
//! ### Limitations
//!
//! * The option tables below are the ones that were verified against stock git. An
//!   option git recognises but that is absent from them is reported as
//!   `unrecognized argument`, which is wrong for that option; nothing silently passes.
//! * Whatever `git log` does not implement is not implemented here either, and is
//!   reported by that module.

use anyhow::Result;
use std::process::ExitCode;

use gix::bstr::ByteSlice;

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

/// A message git writes before exiting non-zero. `text` is complete, already
/// newline-terminated, and includes its own `fatal: ` / `error: ` prefix.
///
/// Almost always stderr; `-h` is the exception, since parse-options treats a
/// help request as an answer rather than a complaint and writes it to stdout.
struct Fatal {
    text: String,
    code: u8,
    /// `-h` only: `usage_with_options_internal(…, USAGE_TO_STDOUT)`.
    to_stdout: bool,
}

impl Fatal {
    /// A `die()` whose text a shared helper has already rendered in full —
    /// prefix, newline and any preceding `error:` line included. Exit 128, as
    /// `die()` always is.
    fn raw(text: String) -> Self {
        Fatal {
            text,
            code: 128,
            to_stdout: false,
        }
    }

    /// `die()`: the `fatal: ` prefix and exit 128.
    fn die(msg: impl Into<String>) -> Self {
        Fatal {
            text: format!("fatal: {}\n", msg.into()),
            code: 128,
            to_stdout: false,
        }
    }

    /// A `parse-options` complaint: the `error: ` prefix and exit 129.
    fn usage(msg: impl Into<String>) -> Self {
        Fatal {
            text: format!("error: {}\n", msg.into()),
            code: 129,
            to_stdout: false,
        }
    }

    /// parse-options answering `-h`: the usage block on stdout, exit 129.
    fn help(usage: &str) -> Self {
        Fatal {
            text: usage.to_string(),
            code: 129,
            to_stdout: true,
        }
    }

    /// Write the message to the stream it belongs on.
    fn emit(&self) {
        if self.to_stdout {
            print!("{}", self.text);
        } else {
            eprint!("{}", self.text);
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
    "-z",
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
    "--expand-tabs=",
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
}

/// Read the `log.*` display config, reproducing `git_log_config`'s validation of
/// `log.date`: an unknown value is `fatal: unknown date format <v>` (exit 128), raised
/// before the deprecation gate and before argument parsing, matching stock git 2.55.0.
fn read_log_config(repo: &gix::Repository) -> Result<LogConfig, Fatal> {
    let snap = repo.config_snapshot();
    let abbrev_commit = snap.boolean("log.abbrevCommit").unwrap_or(false);
    // git's `log.showRoot` defaults to true; only an explicit false suppresses the root.
    let show_root = snap.boolean("log.showRoot").unwrap_or(true);
    // `git_log_config` validates `log.date` here, before the deprecation gate and before
    // argument parsing; the rendering itself is log's, which reads the mode again.
    if let Some(v) = snap.string("log.date") {
        validate_date_format(&v.to_str_lossy())?;
    }
    Ok(LogConfig {
        abbrev_commit,
        show_root,
    })
}

/// `git whatchanged` — see the module documentation for the covered surface.
pub fn whatchanged(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("whatchanged") => &args[1..],
        _ => args,
    };

    let original_args = args;

    // git runs the deprecation check inside `cmd_whatchanged`, i.e. after repository
    // setup, so a missing repository is still reported first.
    let repo = gix::discover(".")?;
    super::diff_files::init_quote_path(&repo);

    // git reads `log.*` display config in `git_log_config`, before `setup_revisions` and
    // before the deprecation gate. An invalid `log.date` is fatal here — ahead of the
    // deprecation notice and of any argument-parse error — even when a valid `--date` on
    // the command line would otherwise take over. Verified against stock git 2.55.0.
    let cfg = match read_log_config(&repo) {
        Ok(c) => c,
        Err(f) => {
            f.emit();
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
            f.emit();
            return Ok(ExitCode::from(f.code));
        }
    };
    let (opted_in, args) = (phase1.opted_in, phase1.rest);

    // `setup_revisions` classifies the whole array before the deprecation check, so
    // its exit-128/129 paths come first and are reproduced here.
    if let Err(f) = parse_args(&repo, &args, phase1.quiet, &cfg) {
        f.emit();
        return Ok(ExitCode::from(f.code));
    }

    if !opted_in {
        eprint!("{DEPRECATION}");
        return Ok(ExitCode::from(128));
    }

    // `cmd_whatchanged` *is* `cmd_log` with the raw format as the default and
    // `always_show_header` left off, so the walk and the rendering are log's. Only
    // `--i-still-use-this` is dropped, exactly as `cmd_log_init`'s `parse_options`
    // pass removes it before `setup_revisions` sees the array.
    let forwarded: Vec<String> = args_without_opt_in(original_args);
    super::log::whatchanged(&forwarded)
}

/// The argument array `cmd_log_init` leaves for `setup_revisions`, minus the opt-in
/// flag it consumed. Anything after `--` is a pathspec and is left alone.
fn args_without_opt_in(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut after_dashdash = false;
    for a in args {
        if after_dashdash || a != "--i-still-use-this" {
            out.push(a.clone());
        }
        if a == "--" {
            after_dashdash = true;
        }
    }
    out
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
    // Config supplies the defaults; the flags parsed below override them.
    let mut p = Parsed {
        abbrev_commit: cfg.abbrev_commit,
        show_root: cfg.show_root,
        ..Parsed::default()
    };
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
        // parse_options_step()'s `internal_help`. `git whatchanged` is
        // `builtin/log.c`, so the block is `git log`'s — stdout, 129.
        if a == "-h" {
            return Err(Fatal::help(super::log::USAGE));
        }
        // `--help-all` renders `USAGE_FULL`: the same block with the hidden
        // `--i-still-use-this` left in.
        if a == "--help-all" {
            return Err(Fatal::help(super::log::USAGE_ALL));
        }

        if a.starts_with('-') && a.len() > 1 {
            i += consume_option(args, i, &mut p, &mut st, &mut unrecognized)?;
            continue;
        }

        // `handle_revision_arg()` dies on its own for a range whose endpoints are
        // absent objects, and for a full-length hex name `get_oid()` decoded but
        // `parse_object()` could not find. Both happen before the `^` branch and
        // the pathspec fallback below exist as far as git is concerned, so they
        // are asked about first — a token git dies on never reaches either.
        // A `--` earlier on the line was consumed at the top of this loop, so an
        // argument reaching here is always one `setup_revisions()` would still let
        // be a filename.
        if let Some(message) = super::log::early_revision_fatal(repo, a, false) {
            return Err(Fatal::raw(message));
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

        // `handle_revision_arg_1()` refuses a bare `..` ahead of
        // `handle_dotdot()`, so gitoxide reading it as `HEAD..HEAD` below must
        // not get the chance. See
        // [`crate::objname::is_parent_directory_pathspec`].
        let parent_dir = crate::objname::is_parent_directory_pathspec(a, args.iter().any(|x| x == "--"));
        if !parent_dir && !a.is_empty() && repo.rev_parse(a).is_ok() {
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
    // `parse_pathspec()` runs inside `setup_revisions()`, and `cmd_whatchanged`
    // reaches that through `cmd_log_init()` *before* its own refusal:
    //
    // ```c
    // cmd_log_init(argc, argv, prefix, &rev, &opt, &cfg);
    //
    // if (!cfg.i_still_use_this)
    //         you_still_use_that("git whatchanged", …);
    // ```
    //
    // So a rejected pathspec element is fatal ahead of the deprecation notice —
    // `git whatchanged -- ..` is the outside-repository fatal, not the refusal.
    if let Some(msg) = crate::pathspec::parse_pathspec_fatal(repo, &p.pathspecs) {
        return Err(Fatal::die(msg));
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
                to_stdout: false,
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

/// [`crate::setup::verify_filename`], raised as this module's fatal.
///
/// `first` is git's `diagnose_misspelt_rev`: it marks the argument that failed
/// revision resolution, which gets the `ambiguous argument` wording.
fn verify_filename(arg: &str, first: bool) -> Result<(), Fatal> {
    match crate::setup::verify_filename(arg, first) {
        Some(msg) => Err(Fatal::die(msg)),
        None => Ok(()),
    }
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

