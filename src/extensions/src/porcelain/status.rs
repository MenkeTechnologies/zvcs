use anyhow::Result;
use std::collections::BTreeMap;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;

use super::color::{Slot, StatusColors};
use super::diffcore_rename;
use super::{Arg, LongOpt};

/// `cmd_status()`'s `struct option builtin_status_options[]`
/// (builtin/commit.c:1568-1599), in table order, as [`super::resolve_long`]
/// reads it. `-M`/`--find-renames` is `PARSE_OPT_OPTARG | PARSE_OPT_NONEG`, and
/// `--no-renames` is an `OPT_BOOL` whose name already carries the `no-`, so
/// `--renames` is that same entry unset rather than a slot of its own.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "verbose", neg: true, arg: Arg::None },
    LongOpt { name: "short", neg: true, arg: Arg::None },
    LongOpt { name: "branch", neg: true, arg: Arg::None },
    LongOpt { name: "show-stash", neg: true, arg: Arg::None },
    LongOpt { name: "ahead-behind", neg: true, arg: Arg::None },
    LongOpt { name: "porcelain", neg: true, arg: Arg::Optional },
    LongOpt { name: "long", neg: true, arg: Arg::None },
    LongOpt { name: "null", neg: true, arg: Arg::None },
    LongOpt { name: "untracked-files", neg: true, arg: Arg::Optional },
    LongOpt { name: "ignored", neg: true, arg: Arg::Optional },
    LongOpt { name: "ignore-submodules", neg: true, arg: Arg::Optional },
    LongOpt { name: "column", neg: true, arg: Arg::Optional },
    LongOpt { name: "no-renames", neg: true, arg: Arg::None },
    LongOpt { name: "find-renames", neg: false, arg: Arg::Optional },
];

/// The exact usage block stock `git status` prints on a usage error (exit 129).
const USAGE: &str = "usage: git status [<options>] [--] [<pathspec>...]

    -v, --[no-]verbose    be verbose
    -s, --[no-]short      show status concisely
    -b, --[no-]branch     show branch information
    --[no-]show-stash     show stash information
    --[no-]ahead-behind   compute full ahead/behind values
    --[no-]porcelain[=<version>]
                          machine-readable output
    --[no-]long           show status in long format (default)
    -z, --[no-]null       terminate entries with NUL
    -u, --[no-]untracked-files[=<mode>]
                          show untracked files, optional modes: all, normal, no. (Default: all)
    --[no-]ignored[=<mode>]
                          show ignored files, optional modes: traditional, matching, no. (Default: traditional)
    --[no-]ignore-submodules[=<when>]
                          ignore changes to submodules, optional when: all, dirty, untracked. (Default: all)
    --[no-]column[=<style>]
                          list untracked files in columns
    --no-renames          do not detect renames
    --renames             opposite of --no-renames
    -M, --find-renames[=<n>]
                          detect renames, optionally set similarity index

";

/// How untracked files are reported, mirroring git's `--untracked-files` modes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Untracked {
    /// `-uno` — no directory walk at all.
    No,
    /// `-unormal` (git's default) — collapse wholly-untracked directories.
    Normal,
    /// `-uall` — list every untracked file individually.
    All,
}

/// `git status` — working-tree status vs the index and `HEAD`.
///
/// Backed entirely by gitoxide's `Repository::status()` platform, which fans a
/// tree↔index diff (the staged changes) and an index↔worktree diff (the
/// unstaged changes plus the directory walk for untracked and ignored files)
/// into a single iterator. From those items we reconstruct git's own output.
///
/// Supported invocations (output byte-for-byte matches stock `git status`):
/// ```text
///   * `git status`                      — default long format.
///   * `git status -s|--short`           — short format. It deliberately disagrees
///                                          with `--porcelain` on submodules:
///                                          `short_submodule_status()`
///                                          (wt-status.c:449) runs only for
///                                          `STATUS_FORMAT_SHORT`, so a gitlink whose
///                                          recorded commit is unchanged prints `m`
///                                          (modified content) or `?` (untracked
///                                          content) here and `M` there.
///   * `git status --porcelain[=v1]`     — porcelain v1.
///   * `git status -b|--branch`          — the `## <branch>...<upstream> [ahead N, behind M]`
///                                          short-format header.
///   * `git status -u<mode>`             — all three `--untracked-files` modes.
///   * `git status --ignored[=<mode>]`   — the `!!` / `Ignored files:` listing.
///   * `git status --no-renames | --renames | -M | --find-renames[=<n>]`.
///   * `git status --show-stash` — the trailing stash-count line (long) / the
///     `# stash <n>` header (porcelain v2), driven by `status.showStash`.
///   * `git status --[no-]ahead-behind` — FULL counts vs. git's QUICK (`[different]`
///     / `+? -?` / "refer to different commits") mode, driven by `status.aheadBehind`.
///   * `git status --ignore-submodules[=<when>]` — `all` hides every submodule
///     change (staged gitlink bumps included), while `dirty` / `untracked` /
///     `none` tune which index↔worktree submodule differences surface via gix's
///     submodule-status ignore level; an invalid `<when>` is fatal (exit 128).
///   * `git status --no-short | --no-long | --no-porcelain` — reset to the long
///     format and pin it against `status.short`.
///   * `git status --column[=<opts>] | --no-column` — lay the long-format
///     untracked and ignored file listings out in columns through the same engine
///     `git column` uses (padding 1); honors `column.ui`/`column.status` and
///     resolves `auto` against the terminal.
///   * `status.displayCommentPrefix` — prefixes every long-format line with the
///     comment string. `core.commentString` and `core.commentChar` are one variable
///     (environment.c:435), so the last of the two that was set wins and `auto`
///     resolves to `#`; the value is kept whole, so `//` prefixes with `//`. The
///     trailing summary / stash lines stay unprefixed, matching git.
///   * `git status -v|--verbose` (`OPT__VERBOSE_MORE`, so `-vv`/`-v -v` count up,
///     `--no-verbose` resets) — `wt_status_print_verbose()`. One appends the
///     staged patch to the long format; two also label it `Changes to be
///     committed:` with `c/`…`i/` prefixes (only when something is committable —
///     otherwise git leaves the configured prefixes alone), then a 50-dash rule,
///     `Changes not staged for commit:` and the index↔worktree patch with
///     `i/`…`w/` prefixes. The section labels and rule go through
///     `status_printf` and so pick up `status.displayCommentPrefix`; the patch
///     bodies do not. Like git, the verbose patches ignore the command line's
///     pathspec, and the short / porcelain formats ignore `-v` entirely. The
///     patches come from this binary's own `git diff`, so they are byte-identical
///     to it (see [`verbose_patch`]).
///   * a `HEAD` that does not name a commit. Nothing on the staged path requires
///     one: `run_diff_index` peels `s->reference` to a *tree*
///     (`repo_parse_tree_indirect`, diff-lib.c:555), so a `HEAD` detached onto a
///     tree reports normally, one on a blob is `error: bad tree object HEAD`
///     (diff-lib.c:557 + `exit(128)` at :647-648) and one naming an object the
///     odb lacks is `fatal: bad object HEAD` (revision.c:368). Both refusals
///     happen during collection, so stdout stays empty in every format. (`git
///     commit` is stricter and refuses all three — see [`super::commit`].)
///   * a detached `HEAD` named after `HEAD`'s reflog rather than after its
///     object — `HEAD detached at refs/heads/<b>` / `at <tag>` / `from <oid>`,
///     and `Not currently on any branch.` when no switch was ever logged (see
///     [`detached_from`]).
///   * unmerged (conflicted) paths, in both long and short form.
///   * `git status [--] <pathspec>...` — limits the report to matching paths
///     (the gix status iterator is given the patterns), across every format.
///   * `git status -z|--null` — NUL-terminated, unquoted entries. Per git's
///     `finalize_deferred_config` it forces a machine format (an unset/`--no-…`
///     format becomes porcelain v1, `--long` is rejected, an explicit short /
///     porcelain / v2 keeps its format) and turns off the deferred `status.*`
///     config inheritance. Output is uncolored (git only colors `-z` under a
///     forced color, which is not a real workflow).
/// ```
///
/// Intent-to-add entries (`git add -N`) render as git does: a new file in the
/// worktree column (` A`), absent from HEAD and index in porcelain v2.
pub fn status(args: &[String]) -> Result<ExitCode> {
    status_with(args, Reference::Status)
}

/// `prepare_to_commit()`'s commented status block in `COMMIT_EDITMSG`
/// (builtin/commit.c:1025) — the same engine as a report on stdout, pointed at a
/// string and with the four settings the editor buffer forces.
///
/// git reaches it through `run_status(s->fp, index_file, prefix, 1, s)` after
/// having already set, on the very same `wt_status`:
///
///   * `s->display_comment_prefix = 1` (builtin/commit.c:917) — the block *is*
///     comments, whatever `status.displayCommentPrefix` says. The comment string
///     is the caller's because an `auto` comment char is chosen against the
///     message body (`adjust_comment_line_char()`, builtin/commit.c:935), which
///     this module cannot see;
///   * `s->hints = 0` (:923) — "most hints are counter-productive when the commit
///     has already started", so no `(use "git …")` direction survives;
///   * `s->use_color = GIT_COLOR_NEVER` (:1024) — a commit message is not a
///     terminal;
///   * `nowarn = 1` (:1025) — the trailing `no changes added to commit` /
///     `nothing to commit …` summary is dropped (wt-status.c:1977-1978). `No
///     changes` under `--amend` is *not*, because git tests `s->amend` first.
///
/// `status_format` is `STATUS_FORMAT_NONE` for the whole of `cmd_commit`
/// (builtin/commit.c:1810, "Ignore status.short"), which is what `--long` pins
/// here.
pub(crate) fn commit_template_block(
    reference: Reference,
    untracked: Option<&str>,
    comment: &str,
) -> Result<String> {
    let mut args = vec!["--long".to_string()];
    // `handle_untracked_files_arg()` ran before `prepare_to_commit()`, so the
    // block honors `-u<mode>` exactly as the report on stdout does.
    if let Some(u) = untracked {
        args.push(format!("--untracked-files={u}"));
    }
    let mut body = String::new();
    status_report(&args, reference, Some(Template { comment, out: &mut body }))?;
    Ok(body)
}

/// [`commit_template_block`]'s destination and the comment string it commits to.
struct Template<'a> {
    /// `comment_line_str` as `prepare_to_commit()` settled it.
    comment: &'a str,
    /// `s->fp`, which for the editor block is `COMMIT_EDITMSG` rather than stdout.
    out: &'a mut String,
}

/// What the staged half of the report is measured against: git's `s->reference`,
/// with `s->amend` riding along because the two are only ever set together
/// (builtin/commit.c:571-574).
///
/// `git status` leaves both alone and compares the index against `HEAD`. `git
/// commit --amend` points the engine at `HEAD^1` instead, because the commit it
/// is about to write replaces `HEAD` rather than following it — so "what would
/// this commit record" is the difference from `HEAD`'s *parent*. Six things in
/// the long format change with it, all of them because git branched on
/// `s->reference` or `s->amend` rather than on `HEAD`:
///
///   * the staged section is the index against `HEAD^1` (wt-status.c:673);
///   * `s->is_initial` becomes "`HEAD^1` does not resolve", which is true when
///     `HEAD` is a root commit, so amending one prints the initial-commit block;
///   * the unstage hint names the reference:
///     `git restore --source=HEAD^1 --staged <file>...` (wt-status.c:208-211);
///   * an uncommittable report ends in `No changes` instead of one of the
///     `nothing to commit` wordings (wt-status.c:1974-1976);
///   * the "use `git commit --amend`" advice on a rebase banner is suppressed —
///     the user is already doing that (wt-status.c:1542);
///   * the staged submodule summary is `git submodule summary … HEAD^` rather
///     than `… HEAD` (wt-status.c:1046).
/// The third variant is not about the reference at all but travels with it:
/// `cmd_commit()` sets `s->commit_template = 1` for every report it produces
/// (builtin/commit.c:1809), which is what turns the unborn-repository notice from
/// "No commits yet" into "Initial commit" (wt-status.c:1929-1934). Since only
/// `git commit` ever asks for a non-default reference, one enum carries both.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Reference {
    /// `git status`: `s->reference = "HEAD"`, `s->amend = 0`, no commit template.
    Status,
    /// `git commit`: the same reference, but reported as a commit would.
    Commit,
    /// `git commit --amend`: `s->reference = "HEAD^1"`, `s->amend = 1`.
    AmendParent,
}

impl Reference {
    /// git's `s->amend`.
    fn amend(self) -> bool {
        self == Reference::AmendParent
    }

    /// git's `s->commit_template`.
    fn commit_template(self) -> bool {
        self != Reference::Status
    }

    /// The revision `s->reference` names — the hint that prints it, and the
    /// `committable` test that diffs the index against it, both need it.
    pub(crate) fn spec(self) -> &'static str {
        match self {
            Reference::Status | Reference::Commit => "HEAD",
            Reference::AmendParent => "HEAD^1",
        }
    }
}

/// `git status`, and the same engine pointed somewhere else — see [`Reference`].
///
/// git reaches this second form through `run_status()` (builtin/commit.c:563),
/// which `git commit` calls both for `--dry-run` and for the report that stands
/// in for a refusal; the port's callers in [`super::commit`] mirror those two.
pub(crate) fn status_with(args: &[String], reference: Reference) -> Result<ExitCode> {
    status_report(args, reference, None)
}

/// [`status_with`] and [`commit_template_block`] in one body, because git runs
/// them from one `wt_status`: `template` is `Some` only for the editor block.
fn status_report(
    args: &[String],
    reference: Reference,
    template: Option<Template<'_>>,
) -> Result<ExitCode> {
    let mut short = false;
    let mut porcelain_v2 = false;
    // `--porcelain` selects the short *machine* format, which git never colors;
    // `-s`/`--short` is the colored short display. Both set `short`, so this tracks
    // which one, last-format-flag winning.
    let mut porcelain = false;
    let mut branch_header = false;
    // Whether the command line pinned the output format / branch header. When it
    // did not, `status.short` / `status.branch` supply the default after the repo
    // is opened (git resolves these in `wt_status_collect`/`git_status_config`).
    let mut format_explicit = false;
    let mut branch_explicit = false;
    // `--untracked-files` and `--ignored` are git OPT_STRING options: the raw
    // argument is *stored* during parsing (last occurrence wins; the `--no-`
    // form resets it to unspecified) and validated exactly once *after* the whole
    // command line is parsed. So an intermediate invalid value that a later flag
    // overrides must never error. `None` means unspecified — for untracked that
    // lets `status.showUntrackedFiles` win, for ignored it means "do not show".
    let mut untracked_arg: Option<String> = None;
    let mut ignored_arg: Option<String> = None;
    // `--ignore-submodules[=<when>]` is git's OPTION_STRING with a `PARSE_OPT_OPTARG`
    // default of "all": the raw value is stored during parsing (last wins; `--no-`
    // resets to unspecified) and validated once by `handle_ignore_submodules_arg`
    // *after* the command line is parsed. `None` leaves each submodule's own
    // configured ignore level in force (gix's `AsConfigured` default).
    let mut ignore_submodules_arg: Option<String> = None;
    // `None` keeps git's configured default (`status.renames`/`diff.renames`),
    // i.e. `s->detect_rename == -1`; `Some` is what a command-line flag pinned.
    let mut renames: Option<RenameOpts> = None;
    // `git status [--] <pathspec>...` limits the report to matching paths.
    let mut pathspecs: Vec<BString> = Vec::new();
    let mut operands_only = false;
    // `--show-stash` / `--no-show-stash` (`OPT_BOOL`): `None` defers to
    // `status.showStash`. Only the long and porcelain-v2 formats render it.
    let mut show_stash: Option<bool> = None;
    // `--ahead-behind` / `--no-ahead-behind` (`OPT_BOOL` over git's tri-state
    // `ahead_behind_flags`): `Some(true)` = `AHEAD_BEHIND_FULL`, `Some(false)` =
    // `AHEAD_BEHIND_QUICK`, `None` = unspecified (resolved from `status.aheadBehind`
    // for the human formats, else FULL).
    let mut ahead_behind: Option<bool> = None;
    // `-z` / `--null` (`OPT_BOOL`): NUL-terminate entries and emit paths raw. It
    // also forces a machine format and disables the deferred `status.*` config
    // inheritance (git's `finalize_deferred_config` / `use_deferred_config`);
    // resolved after the loop once the whole command line is known.
    let mut null_term = false;
    // Tracks whether the last format flag was specifically `--long` (git's
    // `STATUS_FORMAT_LONG`). Only that combination is fatal with `-z`; a
    // `--no-…`-reset (`STATUS_FORMAT_NONE`) instead becomes porcelain v1.
    let mut long_format = false;
    // Column layout state for the long-format untracked/ignored listings, seeded
    // from `column.ui` / `column.status` before the command line is parsed so a
    // `--column` flag overrides the config (git's `git_status_config` runs during
    // config, `parseopt_column_callback` after).
    // `-v`/`--verbose` is git's `OPT__VERBOSE_MORE`, a count-up: one appends the
    // staged patch to the long format, two or more also label it and append the
    // unstaged patch. `--no-verbose` resets the count to zero. The short and
    // porcelain formats ignore it entirely.
    let mut verbose: u32 = 0;
    let mut colopts: u32 = super::column::DISABLED;
    if let Err(msg) = super::column::config_colopts(&mut colopts, "status") {
        eprint!("{msg}");
        return Ok(ExitCode::from(128));
    }

    for a in args {
        let s = a.as_str();
        if operands_only {
            pathspecs.push(s.into());
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`. This table has no `PARSE_OPT_HIDDEN` entry, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if s == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }
        let resolved = match super::canonical_long(s, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(s, &first, &second, USAGE))
            }
        };
        let s = resolved.as_ref();
        match s {
            // `s->status_format` is one variable, so the last format option on
            // the command line is the format — which means each of these has to
            // clear porcelain-v2 as well, or `--porcelain=v2 --short` would keep
            // rendering v2.
            "-s" | "--short" => {
                short = true;
                porcelain = false;
                porcelain_v2 = false;
                format_explicit = true;
                long_format = false;
            }
            "--porcelain" | "--porcelain=v1" | "--porcelain=1" => {
                short = true;
                porcelain = true;
                porcelain_v2 = false;
                format_explicit = true;
                long_format = false;
            }
            "--long" => {
                short = false;
                porcelain = false;
                porcelain_v2 = false;
                format_explicit = true;
                long_format = true;
            }
            // git's `--short`/`--long` are `OPT_SET_INT` and `--porcelain` an
            // `OPT_CALLBACK`; every `--no-` form resets the format to
            // `STATUS_FORMAT_NONE`, which renders long and — crucially — pins the
            // format so `status.short` config can no longer promote it to short.
            "--no-short" | "--no-long" | "--no-porcelain" => {
                short = false;
                porcelain = false;
                porcelain_v2 = false;
                format_explicit = true;
                long_format = false;
            }
            "--porcelain=v2" | "--porcelain=2" => {
                porcelain_v2 = true;
                format_explicit = true;
                long_format = false;
            }
            "-z" | "--null" => null_term = true,
            "--no-null" => null_term = false,
            "-b" | "--branch" => {
                branch_header = true;
                branch_explicit = true;
            }
            "--no-branch" => {
                branch_header = false;
                branch_explicit = true;
            }
            "--verbose" => verbose += 1,
            "--no-verbose" => verbose = 0,
            "--show-stash" => show_stash = Some(true),
            "--no-show-stash" => show_stash = Some(false),
            // `--ahead-behind` selects FULL counts, `--no-ahead-behind` the QUICK
            // (eq/neq) mode; either flag wins over `status.aheadBehind`.
            "--ahead-behind" => ahead_behind = Some(true),
            "--no-ahead-behind" => ahead_behind = Some(false),
            // Bare forms take git's default optarg ("all" / "traditional"); the
            // `--no-` forms reset to unspecified. Attached values (`--...=<v>`,
            // `-u<v>`) are captured raw below and validated after the loop.
            "--untracked-files" => untracked_arg = Some("all".to_string()),
            "--no-untracked-files" => untracked_arg = None,
            "--ignored" => ignored_arg = Some("traditional".to_string()),
            "--no-ignored" => ignored_arg = None,
            // Bare `--ignore-submodules` takes git's "all" default optarg; the
            // `--no-` form resets to unspecified. An attached `=<when>` is captured
            // raw below and validated after the loop.
            "--ignore-submodules" => ignore_submodules_arg = Some("all".to_string()),
            "--no-ignore-submodules" => ignore_submodules_arg = None,
            // Everything after `--` is a pathspec; the pathspec arm below rejects
            // any that follow, and a trailing `--` on its own is a no-op.
            "--" => operands_only = true,
            "--no-renames" => renames = Some(RenameOpts::disabled()),
            "--renames" | "-M" | "--find-renames" => {
                renames = Some(RenameOpts::renames());
            }
            // `--column[=<opts>]` / `--no-column`: lay the long-format untracked and
            // ignored file listings out in columns (git's `OPT_COLUMN`).
            "--column" => {
                if let Err(m) = super::column::parseopt_column(&mut colopts, None, false) {
                    eprintln!("error: {m}");
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
            "--no-column" => {
                let _ = super::column::parseopt_column(&mut colopts, None, true);
            }
            _ if s.starts_with("--column=") => {
                if let Err(m) =
                    super::column::parseopt_column(&mut colopts, Some(&s["--column=".len()..]), false)
                {
                    eprintln!("error: {m}");
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
            // git validates the `--porcelain=<version>` value as it parses, dying
            // immediately (exit 128) on anything but v1/v2 — a later valid
            // `--porcelain=v1` does not rescue an earlier bad version.
            _ if s.starts_with("--porcelain=") => {
                let version = &s["--porcelain=".len()..];
                eprintln!("fatal: unsupported porcelain version '{version}'");
                return Ok(ExitCode::from(128));
            }
            _ if s.starts_with("--untracked-files=") => {
                untracked_arg = Some(s["--untracked-files=".len()..].to_string());
            }
            _ if s.starts_with("--ignored=") => {
                ignored_arg = Some(s["--ignored=".len()..].to_string());
            }
            _ if s.starts_with("--ignore-submodules=") => {
                ignore_submodules_arg = Some(s["--ignore-submodules=".len()..].to_string());
            }
            _ if s.starts_with("--find-renames=") || s.starts_with("-M") => {
                let raw = s
                    .strip_prefix("--find-renames=")
                    .unwrap_or_else(|| s.trim_start_matches("-M"));
                match parse_similarity(raw) {
                    Some(opts) => renames = Some(opts),
                    None => {
                        eprintln!("error: unknown option `{}'", s.trim_start_matches('-'));
                        eprint!("{USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if s.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &s[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // A cluster of short flags, e.g. `-sb`. `-u` and `-M` swallow the
            // remainder of the argument as their optional value, as git does.
            _ if s.starts_with('-') && s.len() > 1 => {
                let mut chars = s[1..].chars();
                while let Some(c) = chars.next() {
                    let rest = chars.as_str();
                    match c {
                        's' => {
                            short = true;
                            porcelain = false;
                            format_explicit = true;
                            long_format = false;
                        }
                        'b' => {
                            branch_header = true;
                            branch_explicit = true;
                        }
                        'v' => verbose += 1,
                        'z' => null_term = true,
                        // parse_options_step() tests `internal_help` inside the
                        // short-option loop, so `-h` answers wherever it appears
                        // in a cluster — and on stdout, with no `error:` line,
                        // because asking for help is not a rejection.
                        'h' => return Ok(super::show_usage(USAGE)),
                        'u' => {
                            // A bare `-u` (no attached value) is git's `all` default;
                            // an attached value is captured raw and validated after
                            // the loop, exactly as `--untracked-files=`.
                            untracked_arg = Some(if rest.is_empty() {
                                "all".to_string()
                            } else {
                                rest.to_string()
                            });
                            break;
                        }
                        'M' => {
                            match parse_similarity(rest) {
                                Some(opts) => renames = Some(opts),
                                None => {
                                    eprintln!("error: unknown option `{}'", &s[1..]);
                                    eprint!("{USAGE}");
                                    return Ok(ExitCode::from(129));
                                }
                            }
                            break;
                        }
                        other => {
                            eprintln!("error: unknown switch `{other}'");
                            eprint!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
            }
            // A non-flag token is a pathspec: `git status <path>...`.
            _ => pathspecs.push(s.into()),
        }
    }

    // Validate the deferred OPT_STRING modes now that the whole command line is
    // parsed, in git's own order: `--untracked-files` first, then `--ignored`.
    // Only the final stored value is checked; a bad value dies with exit 128.
    let untracked_flag: Option<Untracked> = match &untracked_arg {
        Some(v) => match parse_untracked_mode(v) {
            Some(m) => Some(m),
            None => {
                eprintln!("fatal: Invalid untracked files mode '{v}'");
                return Ok(ExitCode::from(128));
            }
        },
        None => None,
    };
    // `wt_status`'s `show_ignored_mode`. `traditional` lists the ignored *files* (and
    // with `-uall` git clears `DIR_SHOW_OTHER_DIRECTORIES`, so it descends into an
    // ignored directory to name them), while `matching` reports whatever the ignore
    // pattern matched — the directory itself when a directory pattern matched it.
    let ignored_matching = matches!(ignored_arg.as_deref(), Some("matching"));
    let show_ignored = match &ignored_arg {
        // git accepts exactly these three ignored modes (no boolean coercion);
        // `no` is valid but suppresses the listing, anything else is fatal.
        Some(v) => match v.as_str() {
            "traditional" | "matching" => true,
            "no" => false,
            _ => {
                eprintln!("fatal: Invalid ignored mode '{v}'");
                return Ok(ExitCode::from(128));
            }
        },
        None => false,
    };
    // git validates `--ignore-submodules` last (in `wt_status_collect` via
    // `handle_ignore_submodules_arg`, after untracked and ignored). Only the final
    // stored value is checked; a bad value dies with exit 128. `None` leaves gix on
    // its `AsConfigured` default (each submodule's own configured ignore level).
    let ignore_submodules: Option<gix::submodule::config::Ignore> = match &ignore_submodules_arg {
        Some(v) => match v.as_str() {
            "all" => Some(gix::submodule::config::Ignore::All),
            "dirty" => Some(gix::submodule::config::Ignore::Dirty),
            "untracked" => Some(gix::submodule::config::Ignore::Untracked),
            "none" => Some(gix::submodule::config::Ignore::None),
            _ => {
                eprintln!("fatal: bad --ignore-submodules argument: {v}");
                return Ok(ExitCode::from(128));
            }
        },
        None => None,
    };
    // `-z` finalize (git's `finalize_deferred_config`): NUL output forces a
    // machine format. `--long` is fatal, an unset/`--no-…`-reset format renders
    // as porcelain v1, and any explicit short/porcelain/v2 keeps its format;
    // pinning the format here also stops `status.short` from promoting the
    // display below (the branch / ahead-behind config guards test `null_term`).
    if null_term {
        if long_format {
            eprintln!("fatal: options '--long' and '-z' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        if !short && !porcelain_v2 {
            short = true;
            porcelain = true;
        }
        format_explicit = true;
    }

    // `--ignore-submodules=all` also hides *staged* gitlink changes: git sets
    // `diffopt.ignore_submodules` for the tree↔index diff, not only the worktree
    // pass. The gix platform's `index_worktree_submodules` covers only the latter,
    // so the tree↔index collection filters commit-mode entries itself for `all`.
    let ignore_all = matches!(
        ignore_submodules,
        Some(gix::submodule::config::Ignore::All)
    );

    // Resolve `auto` against the terminal (git's `finalize_colopts(&s.colopts, -1)`).
    // Columns only affect the long-format untracked/ignored listings; a piped
    // stdout leaves them off, so the default one-per-line output is unchanged.
    super::column::finalize(&mut colopts);

    let repo = crate::setup::discover()?;
    // git.c runs `status` with `RUN_SETUP | NEED_WORK_TREE`, so a setup that found no work tree —
    // a bare repository, or a cwd inside the git directory — dies here rather than in the walk.
    if repo.workdir().is_none() {
        return Err(crate::fatal::need_work_tree());
    }

    // `status.displayCommentPrefix` (git's `git_status_config`): when true the
    // long human format prefixes every line with the comment string. Resolved to
    // the actual comment string here so the borrow of the snapshot ends before the
    // long renderer runs; `None` leaves the format uncommented (git's default).
    let mut comment_prefix: Option<String> = None;

    // `status.relativePaths`, git's `s->relative_paths` (default 1), resolved
    // alongside the other `git_status_config` keys below.
    let relative_paths: bool;

    // `status.submoduleSummary`, git's `s->submodule_summary`.
    let submodule_summary_limit: Option<i64>;

    // With no format/branch flag on the command line, `status.short` selects the
    // colored short display and `status.branch` adds the `## <branch>` header.
    // A flag (including `--long` / `--no-branch`) always wins over the config.
    {
        let snap = repo.config_snapshot();
        if !format_explicit && snap.boolean("status.short") == Some(true) {
            short = true;
            porcelain = false;
        }
        // `-z` disables git's `use_deferred_config`, so `status.branch` (like
        // `status.short` above, already pinned via `format_explicit`) no longer
        // promotes the branch header.
        // ```c
        // if (use_deferred_config && s->status_format != STATUS_FORMAT_PORCELAIN &&
        //     s->status_format != STATUS_FORMAT_PORCELAIN_V2)
        //         s->show_branch = status_deferred_config.show_branch;
        // ```
        //
        // (builtin/commit.c:1176-1179.) The machine formats never inherit
        // `status.branch` — only `-b`/`--branch` puts a header on them — which is
        // what keeps `git -c status.branch=true status --porcelain` parseable by
        // everything that has ever consumed it.
        if !branch_explicit && !null_term && !porcelain && !porcelain_v2
            && snap.boolean("status.branch") == Some(true)
        {
            branch_header = true;
        }
        // `status.renames` supplies the rename-detection default when the command
        // line carries no `--renames` / `--no-renames` / `-M`. git reads this key
        // in `status_config` — *before* it parses the command line — and dies on a
        // non-boolean value, so an invalid value is fatal even when a flag would
        // otherwise override it; only the resolved value is what a flag supersedes.
        let configured_detect = match configured_renames(&snap) {
            Ok(setting) => setting,
            Err(bad) => {
                eprintln!("fatal: bad boolean config value '{bad}' for 'status.renames'");
                return Ok(ExitCode::from(128));
            }
        };
        if renames.is_none() {
            // `s->detect_rename` is still `-1`: `diff_setup()` fills it from
            // `diff.renames`, which is why `-c diff.renames=copies` reaches
            // `status` at all.
            let detect = configured_detect.unwrap_or_else(|| configured_diff_renames(&snap));
            renames = Some(RenameOpts {
                detect,
                ..RenameOpts::disabled()
            });
        }
        // The limit is not a flag on `status`, so config alone decides it — and it
        // applies whichever way detection was turned on.
        if let Some(opts) = renames.as_mut() {
            opts.limit = configured_rename_limit(&snap);
        }
        // `status.showStash` is git's default for `--show-stash`; a command-line
        // flag (`Some`) always wins.
        if show_stash.is_none() {
            show_stash = Some(snap.boolean("status.showStash") == Some(true));
        }
        // `finalize_deferred_config`: only the human formats (long / short display)
        // inherit `status.aheadBehind`; the porcelain machine formats keep FULL for
        // backwards compatibility, and an explicit flag always wins.
        if ahead_behind.is_none() && !porcelain && !porcelain_v2 && !null_term {
            if let Some(v) = snap.boolean("status.aheadBehind") {
                ahead_behind = Some(v);
            }
        }
        // `status.displayCommentPrefix` only affects the long human format (git
        // routes every long-format line through `status_printf`, which prepends the
        // comment string; the short and porcelain renderers never do). Resolve the
        // comment string now so `render_long` needs no snapshot borrow.
        if let Some(t) = &template {
            // `s->display_comment_prefix = 1` (builtin/commit.c:917) with the
            // comment string `prepare_to_commit()` settled on — the key is not
            // even consulted.
            comment_prefix = Some(t.comment.to_string());
        } else if snap.boolean("status.displayCommentPrefix") == Some(true) {
            // `core.commentChar` and `core.commentString` are one variable in
            // `git_default_core_config()` (environment.c:435-456), so the *last* one set
            // across both spellings wins and `auto` resolves to `#` right there —
            // `adjust_comment_line_char()` only ever revises it from
            // `builtin/commit.c`'s `prepare_to_commit()`, never for `status`. The rule
            // lives in [`super::interpret_trailers::comment_string`], which `commit.rs`
            // and `rebase_todo.rs` already read it from.
            let bytes = super::interpret_trailers::comment_string(snap.plumbing());
            comment_prefix = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        // `status.relativePaths` (git's `git_status_config`), default on: it is
        // what makes `cmd_status`' closing `if (s.relative_paths) s.prefix =
        // prefix;` hand the cwd prefix to every `quote_path` call.
        relative_paths = snap.boolean("status.relativePaths") != Some(false);
        // `status.submoduleSummary` is `git_config_bool_or_int`: a plain integer
        // is the `--summary-limit`, while a boolean `true` becomes git's `-1`
        // sentinel (no limit). git skips the whole section under
        // `--ignore-submodules=all`.
        submodule_summary_limit = match snap.integer("status.submoduleSummary") {
            Some(0) => None,
            Some(n) => Some(n),
            None if snap.boolean("status.submoduleSummary") == Some(true) => Some(-1),
            None => None,
        }
        .filter(|_| !matches!(ignore_submodules, Some(gix::submodule::config::Ignore::All)));
    }

    // `rev.diffopt.detect_rename` for both halves of the report, settled: the
    // command line, else `status.renames`, else `diff.renames`.
    let renames = renames.unwrap_or_else(RenameOpts::renames);

    // git's `s->prefix`. Two renderers drop it regardless of the config:
    // `wt_porcelain_print` resets `relative_paths`/`prefix` before printing (v1
    // is a stable machine format), and every `-z` path bypasses `quote_path`
    // entirely and writes the raw index path.
    let path_prefix: Option<BString> = if relative_paths && !porcelain && !null_term {
        display_prefix(&repo)?
    } else {
        None
    };
    let path_prefix: Option<&[u8]> = path_prefix.as_ref().map(|p| p.as_slice());

    // Resolve the deferred booleans: absent `--show-stash`/config means off;
    // `quick` is git's `AHEAD_BEHIND_QUICK` (only `--no-ahead-behind` /
    // `status.aheadBehind=false` selects it, everything else is FULL).
    let show_stash = show_stash.unwrap_or(false);
    let quick = ahead_behind == Some(false);
    // `wt_status_prepare`: `s->hints = advice_enabled(ADVICE_STATUS_HINTS)`.
    // Every parenthesized "(use …)" direction in the long format hangs off this
    // one flag, and the trailing summary switches to its short wording when it is
    // off. The short/porcelain formats carry no hints at all. The editor block
    // pins it to zero (builtin/commit.c:923).
    let hints = template.is_none() && crate::advice::Advice::StatusHints.enabled_in(&repo);

    // Resolve the head into an owned description so the borrow ends before we
    // re-open references for the tracking computation.
    let head = repo.head()?;
    // `s->branch` is always read off `HEAD`, whatever `s->reference` is: the
    // "On branch …" line names where the commit will land, not what it is measured
    // against.
    let head_unborn = head.is_unborn();
    let head_state = if head_unborn {
        HeadState::Unborn(referent_short(head.referent_name(), "main"))
    } else if head.is_detached() {
        // `wt_status_get_state(..., s->branch && !strcmp(s->branch, "HEAD"))`
        // (wt-status.c:883): the reflog lookup runs only for a detached `HEAD`.
        match detached_from(&repo) {
            Some((from, at)) => HeadState::Detached { from: Some(from), at },
            None => HeadState::Detached { from: None, at: false },
        }
    } else {
        HeadState::Branch(referent_short(head.referent_name(), "HEAD"))
    };
    drop(head);

    // `s->is_initial = repo_get_oid(s->reference, &oid) ? 1 : 0` (builtin/commit.c:580):
    // whether the *reference* resolves, which for `--amend` is `HEAD^1` and so is
    // false as soon as `HEAD` is a root commit. `reference_tree` is the tree side
    // the staged section is diffed against — git's `opt.def`, which falls back to
    // the empty tree when the reference is unresolvable (wt-status.c:673).
    let reference_tree: Option<ObjectId> = match reference {
        Reference::Status | Reference::Commit => match reference_tree_oid(&repo, "HEAD")? {
            ReferenceTree::Resolved(tree) => tree,
            ReferenceTree::BadObject => {
                eprintln!("fatal: bad object {}", reference.spec());
                return Ok(ExitCode::from(128));
            }
            ReferenceTree::BadTree => {
                eprintln!("error: bad tree object {}", reference.spec());
                return Ok(ExitCode::from(128));
            }
        },
        Reference::AmendParent => {
            // `HEAD^1` only resolves when `HEAD` is a commit, so neither failure
            // mode above can arise: an unresolvable `HEAD^1` is git's
            // `s->is_initial`, and a resolvable one is a commit with a tree.
            match repo.rev_parse_single("HEAD^1").ok() {
                Some(id) => Some(repo.find_commit(id.detach())?.tree_id()?.detach()),
                None => None,
            }
        }
    };
    let unborn = reference_tree.is_none();

    // `MERGE_HEAD` is what makes git treat the run as "from merge": it both
    // enables the in-progress banner and suppresses the unstage hint.
    let merging = repo.git_dir().join("MERGE_HEAD").exists();

    // `wt_status_get_state()`: everything else the long format announces as being in
    // progress — an `am` session, a rebase, a cherry-pick, a revert, a bisect.
    let progress = ProgressState::detect(&repo, merging);

    // `wt_status_get_state()`'s sparse-checkout share (wt-status.c:1795): off unless
    // `core.sparseCheckout` is set and the index has entries, `None` for a sparse
    // index (which has no per-file view to count), otherwise the percentage of index
    // entries that are actually in the worktree, in git's integer arithmetic.
    let sparse_checkout = sparse_checkout_state(&repo);

    // Resolved unconditionally: git validates the config key while reading the
    // config, before `handle_untracked_files_arg()` can override it.
    let configured = match configured_untracked(&repo) {
        Ok(mode) => mode,
        Err(code) => return Ok(code),
    };
    let untracked = untracked_flag.unwrap_or(configured);

    // `setup_standard_excludes()` runs before the walk and dies on an unusable
    // `core.excludesFile`, whether or not the report would have listed anything
    // ignored — see [`crate::config::excludes_file_fatal`].
    if let Some(msg) = crate::config::excludes_file_fatal(&repo) {
        eprintln!("fatal: {msg}");
        return Ok(ExitCode::from(128));
    }

    // ```c
    // if (s.show_ignored_mode == SHOW_MATCHING_IGNORED &&
    //     s.show_untracked_files == SHOW_NO_UNTRACKED_FILES)
    //         die(_("Unsupported combination of ignored and untracked-files arguments"));
    // ```
    //
    // (builtin/commit.c:1527-1529.) `--ignored=matching` reports whatever the
    // ignore patterns matched, which is a *superset* of the untracked walk it
    // would have to run; asking for it with the walk turned off has no answer.
    // The test is on the resolved untracked mode, so `status.showUntrackedFiles=no`
    // reaches it just as `-uno` does.
    if ignored_matching && untracked == Untracked::No {
        eprintln!("fatal: Unsupported combination of ignored and untracked-files arguments");
        return Ok(ExitCode::from(128));
    }

    // The porcelain-v2 machine format is a separate renderer with its own,
    // richer per-path fields (HEAD/index/worktree modes + oids); it shares none
    // of the v1/long collection below, so the two cannot regress each other.
    if porcelain_v2 {
        return porcelain_v2_output(
            &repo,
            reference_tree.unwrap_or_else(|| repo.object_hash().empty_tree()),
            untracked,
            show_ignored,
            ignored_matching,
            renames,
            branch_header,
            &pathspecs,
            show_stash,
            quick,
            ignore_submodules,
            ignore_all,
            null_term,
            path_prefix,
        );
    }

    // Collect the four change classes from the unified status iterator.
    let mut staged: Vec<(StageKind, BString, Option<BString>)> = Vec::new();
    let mut unstaged: Vec<(WorkKind, BString, Option<BString>, SubmoduleState)> = Vec::new();
    // The two `diff_filepair` queues `diffcore_rename()` runs over, in the order
    // the iterator produced them — one per half of the report, as git has one per
    // `run_diff_index()` / `run_diff_files()` call.
    let mut staged_pairs: Vec<(RenameSide, RenameSide)> = Vec::new();
    let mut work_pairs: Vec<(RenameSide, RenameSide)> = Vec::new();
    let hash = repo.object_hash();
    let mut unmerged: Vec<(u8, BString)> = Vec::new();
    let mut untracked_paths: Vec<BString> = Vec::new();
    let mut ignored_paths: Vec<BString> = Vec::new();

    let mut platform = repo
        .status(gix::progress::Discard)?
        .index_worktree_options_mut(preload_index_threads(&repo))
        .untracked_files(match untracked {
            Untracked::No => gix::status::UntrackedFiles::None,
            Untracked::Normal => gix::status::UntrackedFiles::Collapsed,
            Untracked::All => gix::status::UntrackedFiles::Files,
        });
    // `opt.def = s->is_initial ? empty_tree : s->reference` (wt-status.c:673): the
    // staged section is the index against whatever the reference resolved to.
    platform =
        platform.head_tree(reference_tree.unwrap_or_else(|| repo.object_hash().empty_tree()));
    if show_ignored {
        // git lists ignored entries at the same granularity as untracked ones.
        // `--ignored=matching` reports whatever the ignore pattern matched, so it never
        // collapses; the traditional mode follows the untracked granularity, and under
        // `-uall` it additionally descends into an ignored directory to name the files
        // in it.
        let mode = if untracked == Untracked::All || ignored_matching {
            gix::dir::walk::EmissionMode::Matching
        } else {
            gix::dir::walk::EmissionMode::CollapseDirectory
        };
        let descend = untracked == Untracked::All && !ignored_matching;
        platform = platform.dirwalk_options(|opts| {
            opts.emit_ignored(Some(mode)).recurse_ignored_directories(descend)
        });
    }
    // Rename detection is git's own `diffcore_rename()` pass, run over the
    // collected pairs below — gix's tracker scores similarity differently and has
    // no worktree-side equivalent at all, so it stays off on both halves.
    platform = platform.tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled);
    // `--ignore-submodules=<when>` fixes the index↔worktree submodule check at the
    // requested ignore level (git's `handle_ignore_submodules_arg`); absent the
    // flag, gix keeps each submodule's own configured level.
    if let Some(ignore) = ignore_submodules {
        platform = platform.index_worktree_submodules(gix::status::Submodule::Given {
            ignore,
            check_dirty: true,
        });
    }

    // `wt_status_collect_untracked` brackets the worktree walk with `getnanotime()`
    // and keeps the elapsed time in `s->untracked_in_ms` — but only when
    // `advice.statusUoption` is on, since that hint is its sole consumer. gix fuses
    // the untracked walk into the same index↔worktree pass as the modification
    // checks, so this bracket covers both rather than the walk alone.
    let untracked_t0 = (untracked != Untracked::No
        && crate::advice::Advice::StatusUoption.enabled_in(&repo))
    .then(std::time::Instant::now);

    let patterns: Vec<BString> = pathspecs.to_vec();
    for item in platform.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                // `--ignore-submodules=all` suppresses staged gitlink (submodule)
                // changes too; skip any tree↔index change on a commit-mode entry.
                if ignore_all {
                    let mode = match &change {
                        ChangeRef::Addition { entry_mode, .. }
                        | ChangeRef::Deletion { entry_mode, .. }
                        | ChangeRef::Modification { entry_mode, .. }
                        | ChangeRef::Rewrite { entry_mode, .. } => *entry_mode,
                    };
                    if type_class(mode) == 2 {
                        continue;
                    }
                }
                match change {
                    ChangeRef::Addition {
                        location,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let path = location.into_owned();
                        staged_pairs.push((
                            RenameSide::absent(path.clone(), hash),
                            RenameSide {
                                path: path.clone(),
                                mode: entry_mode.bits(),
                                id: id.into_owned(),
                                id_valid: true,
                            },
                        ));
                        staged.push((StageKind::New, path, None));
                    }
                    ChangeRef::Deletion {
                        location,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let path = location.into_owned();
                        staged_pairs.push((
                            RenameSide {
                                path: path.clone(),
                                mode: entry_mode.bits(),
                                id: id.into_owned(),
                                id_valid: true,
                            },
                            RenameSide::absent(path.clone(), hash),
                        ));
                        staged.push((StageKind::Deleted, path, None));
                    }
                    ChangeRef::Modification {
                        location,
                        previous_entry_mode,
                        previous_id,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let kind = if type_class(previous_entry_mode) != type_class(entry_mode) {
                            StageKind::TypeChange
                        } else {
                            StageKind::Modified
                        };
                        let path = location.into_owned();
                        staged_pairs.push((
                            RenameSide {
                                path: path.clone(),
                                mode: previous_entry_mode.bits(),
                                id: previous_id.into_owned(),
                                id_valid: true,
                            },
                            RenameSide {
                                path: path.clone(),
                                mode: entry_mode.bits(),
                                id: id.into_owned(),
                                id_valid: true,
                            },
                        ));
                        staged.push((kind, path, None));
                    }
                    // Rename tracking is off in the platform, so gix never emits
                    // this: the pairing is `diffcore_rename()`'s below.
                    ChangeRef::Rewrite { location, .. } => {
                        staged.push((StageKind::Modified, location.into_owned(), None));
                    }
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::{Change, Conflict, EntryStatus};
                match iw {
                    Item::Modification {
                        rela_path,
                        status,
                        entry,
                        ..
                    } => match status {
                        // gitoxide already folds the up-to-three conflict stages
                        // of one path into a single summary, which maps 1:1 onto
                        // git's stagemask.
                        EntryStatus::Conflict { summary, .. } => {
                            let mask = match summary {
                                Conflict::BothDeleted => 1,
                                Conflict::AddedByUs => 2,
                                Conflict::DeletedByThem => 3,
                                Conflict::AddedByThem => 4,
                                Conflict::DeletedByUs => 5,
                                Conflict::BothAdded => 6,
                                Conflict::BothModified => 7,
                            };
                            unmerged.push((mask, rela_path));
                        }
                        // `git add -N` records a placeholder so the path can be
                        // diffed, but nothing is staged: git lists it under
                        // "Changes not staged for commit" as a new file, ` A`.
                        //
                        // `rev.diffopt.ita_invisible_in_index = 1` (wt-status.c:665)
                        // is what makes `diff-files` queue it as an *addition*, and
                        // so the only worktree-side rename destination there is.
                        EntryStatus::IntentToAdd => {
                            work_pairs.push((
                                RenameSide::absent(rela_path.clone(), hash),
                                RenameSide {
                                    path: rela_path.clone(),
                                    mode: worktree_mode(&repo, gix::bstr::BStr::new(&rela_path)),
                                    id: ObjectId::null(hash),
                                    id_valid: false,
                                },
                            ));
                            unstaged.push((
                                WorkKind::Added,
                                rela_path,
                                None,
                                SubmoduleState::default(),
                            ))
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(change) => {
                            let short_format = short && !porcelain;
                            let (kind, sub) = match change {
                                // gix calls every non-submodule entry whose path is a
                                // directory removed; `check_removed()` (diff-lib.c:58)
                                // only agrees when that directory is not a repository.
                                // When it is one the pair becomes `100644` → `160000`,
                                // so the worktree side is a gitlink and
                                // `wt_status_collect_changed_cb()` (wt-status.c:484)
                                // fills in the submodule fields — with `two->oid` still
                                // null, `new_submodule_commits` is always set.
                                Change::Removed => match removed_gitlink(&repo, gix::bstr::BStr::new(&rela_path))
                                {
                                    Some(()) => (
                                        WorkKind::TypeChange,
                                        SubmoduleState {
                                            new_commits: true,
                                            ..SubmoduleState::default()
                                        },
                                    ),
                                    None => (WorkKind::Deleted, SubmoduleState::default()),
                                },
                                Change::Type { .. } => {
                                    (WorkKind::TypeChange, SubmoduleState::default())
                                }
                                Change::Modification { .. } => {
                                    (WorkKind::Modified, SubmoduleState::default())
                                }
                                Change::SubmoduleModification(sm) => {
                                    (WorkKind::Modified, SubmoduleState::from_gix(&sm))
                                }
                            };
                            let kind = if short_format {
                                short_submodule_kind(kind, sub)
                            } else {
                                kind
                            };
                            // The worktree half of the pair: `diff-files` leaves it
                            // unhashed (`oid_valid == 0`), so a similarity check
                            // reads the file itself. A deletion has no worktree side
                            // at all, which is what makes it a rename *source*.
                            let wt_mode = match kind {
                                WorkKind::Deleted => 0,
                                _ => worktree_mode(&repo, gix::bstr::BStr::new(&rela_path)),
                            };
                            work_pairs.push((
                                RenameSide {
                                    path: rela_path.clone(),
                                    mode: entry.mode.bits(),
                                    id: entry.id,
                                    id_valid: true,
                                },
                                RenameSide {
                                    path: rela_path.clone(),
                                    mode: wt_mode,
                                    id: ObjectId::null(hash),
                                    id_valid: false,
                                },
                            ));
                            unstaged.push((kind, rela_path, None, sub));
                        }
                    },
                    Item::DirectoryContents { entry, .. } => match entry.status {
                        gix::dir::entry::Status::Untracked => {
                            untracked_paths.push(walk_path(&entry));
                        }
                        gix::dir::entry::Status::Ignored(_) => {
                            ignored_paths.push(walk_path(&entry));
                        }
                        _ => {}
                    },
                    // Rename tracking is disabled for the index↔worktree pass in the
                    // default status platform, so this never fires; ignore defensively.
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }

    // `diffcore_std()`'s rename pass, run once per half of the report exactly as
    // `run_diff_index()` and `run_diff_files()` each run it. A rename replaces the
    // destination's own classification and consumes the source's deletion record
    // (diffcore-rename.c:1614: a deletion with `rename_used` set never reaches the
    // output queue); a copy source that was a *modification* keeps its record.
    for rw in detect_rewrites(&repo, &staged_pairs, renames) {
        let kind = if rw.kind == b'C' {
            StageKind::Copied
        } else {
            StageKind::Renamed
        };
        staged.retain(|(k, p, _)| !(matches!(k, StageKind::Deleted) && *p == rw.src.path));
        for e in staged.iter_mut() {
            if e.1 == rw.dst.path {
                e.0 = kind;
                e.2 = Some(rw.src.path.clone());
            }
        }
    }
    for rw in detect_rewrites(&repo, &work_pairs, renames) {
        let kind = if rw.kind == b'C' {
            WorkKind::Copied
        } else {
            WorkKind::Renamed
        };
        unstaged.retain(|(k, p, _, _)| !(matches!(k, WorkKind::Deleted) && *p == rw.src.path));
        for e in unstaged.iter_mut() {
            if e.1 == rw.dst.path {
                e.0 = kind;
                e.2 = Some(rw.src.path.clone());
            }
        }
    }

    let untracked_slow = uf_was_slow(untracked_t0);

    // `wt_status_collect_untracked()` (wt-status.c:834, :840) filters both dirwalk
    // lists through `index_name_is_other()`.
    {
        let index = repo.index_or_empty()?;
        untracked_paths.retain(|p| index_name_is_other(&index, gix::bstr::BStr::new(p)));
        ignored_paths.retain(|p| index_name_is_other(&index, gix::bstr::BStr::new(p)));
    }

    // git orders each section (and each short-format block) by path.
    staged.sort_by(|a, b| a.1.cmp(&b.1));
    unstaged.sort_by(|a, b| a.1.cmp(&b.1));
    unmerged.sort_by(|a, b| a.1.cmp(&b.1));
    untracked_paths.sort();
    ignored_paths.sort();

    // `wt_longstatus_print_tracking` times `format_tracking_info`, whose cost is
    // the ahead/behind revision walk — [`tracking_info`] here.
    //
    // The two formats measure different things: `wt_shortstatus_print_tracking`
    // asks `stat_tracking_info()` about the upstream alone, while the long
    // format runs `format_tracking_info()`, which walks every ref
    // `status.compareBranches` names. Only the format about to be printed is
    // computed, so neither pays for the other's revision walks.
    let ab_t0 = std::time::Instant::now();
    let tracking = if unborn || !short {
        None
    } else {
        tracking_info(&repo)?
    };
    let comparisons = if unborn || short {
        Vec::new()
    } else {
        tracking_comparisons(&repo)?
    };
    let ab_elapsed_ms = ab_t0.elapsed().as_millis() as u64;

    // git colors the human formats (long and short display) when `color.status`
    // (or `color.ui`) is on and stdout is a terminal; the porcelain machine format
    // is never colored.
    // The editor block joins them: `s->use_color = GIT_COLOR_NEVER` around the
    // `run_status` that renders it (builtin/commit.c:1023-1026).
    let colors = super::color::StatusColors::resolve(&repo, porcelain || template.is_some());

    if short {
        if null_term {
            // `-z`: NUL-terminated, unquoted, uncolored — raw bytes straight to
            // stdout so binary paths survive (a String would be lossy).
            let mut out: Vec<u8> = Vec::new();
            if branch_header {
                short_branch_header_z(&mut out, &head_state, tracking.as_ref(), quick);
            }
            render_short_z(
                &mut out,
                &staged,
                &unstaged,
                &unmerged,
                &untracked_paths,
                &ignored_paths,
            );
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out);
        } else {
            let mut out = String::new();
            if branch_header {
                out.push_str(&short_branch_header(&head_state, tracking.as_ref(), quick, &colors));
            }
            out.push_str(&render_short(
                staged,
                unstaged,
                unmerged,
                &untracked_paths,
                &ignored_paths,
                &colors,
                path_prefix,
            ));
            print!("{out}");
        }
    } else {
        // `--show-stash` appends a stash-count summary after the trailer; the count
        // is the number of `refs/stash` reflog entries (git's `count_stash_entries`).
        let stash_count = if show_stash { count_stash_entries(&repo) } else { 0 };
        // `format_tracking_info(branch, &sb, s->ahead_behind_flags,
        // !s->commit_template)` (wt-status.c:1231-1232): the editor's status
        // block is the one caller that suppresses the divergence hint.
        let mut tracking_block = tracking_lines(&comparisons, quick, hints, template.is_none());
        // The ahead/behind warning is appended to the same strbuf
        // `format_tracking_info` filled, so it only shows when that produced
        // something, and only for the full counts `--no-ahead-behind` skips.
        if !tracking_block.is_empty()
            && !quick
            && ab_elapsed_ms > AB_DELAY_WARNING_IN_MS
            && crate::advice::Advice::StatusAheadBehindWarning.enabled_in(&repo)
        {
            tracking_block.push_str(&format!(
                "\nIt took {:.2} seconds to compute the branch ahead/behind values.\n\
                 You can use '--no-ahead-behind' to avoid this.\n",
                ab_elapsed_ms as f64 / 1000.0
            ));
        }
        let nowarn = template.is_some();
        let sink = if nowarn { LongSink::Retain } else { LongSink::Stdout };
        // Rendered first and printed second: [`render_long`] hands stdout the
        // part above the `submodule summary` fork itself, so it must not be
        // called from inside a `print!` that already holds the lock.
        let body = render_long(
                &head_state,
                &tracking_block,
                untracked_slow,
                hints,
                unborn,
                &progress,
                &rebase_information(&progress, &repo, repo.git_dir(), hints, &|text: &str| {
                    text.split_inclusive('\n')
                        .map(|line| match line.strip_suffix('\n') {
                            Some(body) => format!("{}\n", colors.paint(Slot::Header, body)),
                            None => colors.paint(Slot::Header, line),
                        })
                        .collect()
                }),
                repo.git_dir(),
                sparse_checkout,
                untracked,
                show_ignored,
                &staged,
                &unstaged,
                &unmerged,
                &untracked_paths,
                &ignored_paths,
                show_stash,
                stash_count,
                &colors,
                comment_prefix.as_deref(),
                colopts,
                verbose,
                repo.workdir(),
                path_prefix,
                submodule_summary_limit,
                reference,
                // `nowarn`: `run_status(s->fp, index_file, prefix, 1, s)`
                // (builtin/commit.c:1025) for the editor block, `0` for a report
                // (builtin/commit.c:1085, and `cmd_status` leaves it unset).
                nowarn,
                sink,
            );
        match template {
            Some(t) => *t.out = body,
            None => print!("{body}"),
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// `wt_status_get_detached_from()` (wt-status.c:1709-1743): what the long format
/// names a detached `HEAD` after, and whether it is still sitting where the
/// switch put it.
///
/// git does not name the object `HEAD` currently holds. It reads `HEAD`'s reflog
/// backwards for the most recent `checkout: moving from <x> to <y>` entry
/// (`grab_1st_switch`, wt-status.c:1680-1707) and reports `<y>` — so checking out
/// a tag says the tag, and checking out a branch says that branch. Only the
/// object id that switch landed on is compared against, which is what tells
/// `HEAD detached at ` from `HEAD detached from `: the former means `HEAD` still
/// holds it, the latter that commits have been made since.
///
/// `None` is git's NULL `state->detached_from`, which happens when the reflog has
/// no switch entry at all (a hand-written `HEAD`, or a reflog that was pruned).
/// The long format then says `Not currently on any branch.` (wt-status.c:1914-1917).
pub(super) fn detached_from(repo: &gix::Repository) -> Option<(String, bool)> {
    let head_ref = repo.find_reference("HEAD").ok()?;
    let mut platform = head_ref.log_iter();
    // `refs_for_each_reflog_ent_reverse` returning <= 0 leaves `detached_from`
    // NULL, which covers both "no reflog" and "no entry matched".
    let entries = platform.rev().ok()??;
    let mut switched: Option<(String, ObjectId)> = None;
    for entry in entries {
        let Ok(line) = entry else { break };
        let Some(rest) = line.message.strip_prefix(b"checkout: moving from ".as_slice()) else {
            continue;
        };
        // `strstr(message, " to ")` — the *first* occurrence, so a branch whose
        // name contains " to " is split exactly where git splits it.
        let Some(at) = rest.windows(4).position(|w| w == b" to ") else {
            continue;
        };
        let target = &rest[at + 4..];
        // `strchrnul(target, '\n')`: the entry's message is one line here, but
        // git still stops at the first newline.
        let target = match target.iter().position(|&b| b == b'\n') {
            Some(n) => &target[..n],
            None => target,
        };
        switched = Some((
            String::from_utf8_lossy(target).into_owned(),
            line.new_oid,
        ));
        break;
    }
    let (target, noid) = switched?;
    let abbrev = |id: ObjectId| -> String {
        repo.find_object(id)
            .ok()
            .map(|obj| obj.id().shorten_or_id().to_string())
            .unwrap_or_else(|| id.to_string())
    };
    // "HEAD is relative. Resolve it to the right reflog entry." (wt-status.c:1701-1705)
    let target = match target == "HEAD" {
        true => abbrev(noid),
        false => target,
    };
    // `repo_dwim_ref(..., 1) == 1`: an unambiguous match, whose object is the one
    // the switch landed on — directly, or after peeling a tag to its commit.
    let matches = super::rev_parse::dwim_ref_matches(repo, &target);
    let resolved = match matches.as_slice() {
        [only] => repo
            .try_find_reference(only.as_str())
            .ok()
            .flatten()
            .and_then(|mut r| r.peel_to_id().ok().map(|id| (only.clone(), id.detach()))),
        _ => None,
    };
    let name = match resolved {
        Some((full, id)) if id == noid => {
            // `skip_prefix(from, "refs/tags/")` else `skip_prefix(from,
            // "refs/remotes/")` — and nothing else, which is why a branch prints
            // as the full `refs/heads/<name>`.
            match full.strip_prefix("refs/tags/") {
                Some(tail) => tail.to_string(),
                None => full.strip_prefix("refs/remotes/").unwrap_or(&full).to_string(),
            }
        }
        _ => abbrev(noid),
    };
    // `state->detached_at = !repo_get_oid(r, "HEAD", &oid) && oideq(&oid, &state->detached_oid)`.
    let at = repo.head_id().map(|id| id.detach() == noid).unwrap_or(false);
    Some((name, at))
}

/// What `s->reference` resolved to for the staged half of the report — the tree
/// `run_diff_index` will diff the index against, or one of the two ways git
/// refuses to produce one.
///
/// Nothing on this path requires a commit. `wt_status_collect_changes_index`
/// hands `opt.def = s->reference` to `setup_revisions`, which turns the name into
/// an object with `get_reference` (revision.c:353-369), and `run_diff_index` then
/// peels *that* object with `repo_parse_tree_indirect` (diff-lib.c:555) — a
/// commit yields its tree, a tag is followed, and a tree is already one. So a
/// `HEAD` detached onto a tree is a perfectly ordinary status report.
enum ReferenceTree {
    /// The tree to diff against, or `None` for git's `s->is_initial` — the
    /// reference does not resolve at all, and the staged half is measured against
    /// the empty tree (wt-status.c:673).
    Resolved(Option<ObjectId>),
    /// The reference named an object the odb does not have: `die("bad object %s")`
    /// (revision.c:368), exit 128.
    BadObject,
    /// The object exists but does not peel to a tree — a blob:
    /// `error("bad tree object %s")` (diff-lib.c:557) and then `exit(128)`
    /// (diff-lib.c:647-648).
    BadTree,
}

/// Resolve `s->reference` the way `run_diff_index` does — see [`ReferenceTree`].
fn reference_tree_oid(repo: &gix::Repository, spec: &str) -> Result<ReferenceTree> {
    // `repo_get_oid(s->reference, &oid)` (builtin/commit.c:1639) only turns the
    // name into an oid; it does not read the object, so an unborn `HEAD` is the
    // only thing that makes it fail here.
    let Ok(id) = repo.rev_parse_single(spec) else {
        return Ok(ReferenceTree::Resolved(None));
    };
    let Some(object) = repo.try_find_object(id.detach())? else {
        return Ok(ReferenceTree::BadObject);
    };
    Ok(match object.peel_to_kind(gix::object::Kind::Tree) {
        Ok(tree) => ReferenceTree::Resolved(Some(tree.id)),
        Err(_) => ReferenceTree::BadTree,
    })
}

/// Resolve `status.renames`, git's `git_config_rename`: an explicit `copies` /
/// `copy` (case-insensitive) enables copy detection, any other value is a
/// boolean — truthy means rename detection, falsy disables it — and a valueless
/// key (`[status]\n\trenames`) is git's NULL value, i.e. plain rename detection.
///
/// `Ok(None)` is the key being unset, which leaves `s->detect_rename` at `-1` so
/// that `diff_setup()`'s `diff_detect_rename_default` — i.e. `diff.renames` — has
/// the last word. `Err(value)` is a non-boolean value, which git reports as a
/// fatal config error (exit 128).
fn configured_renames(
    snap: &gix::config::Snapshot,
) -> std::result::Result<Option<u8>, String> {
    use gix::bstr::ByteSlice;
    let Some(value) = snap.string("status.renames") else {
        // No string value: either the key is absent, or it is present but
        // valueless — gitoxide reports the latter as boolean `true`, which git's
        // NULL-value branch treats as plain rename detection.
        return Ok(match snap.boolean("status.renames") {
            Some(true) => Some(diffcore_rename::DETECT_RENAME),
            _ => None,
        });
    };
    let text = value.to_str_lossy();
    if text.eq_ignore_ascii_case("copies") || text.eq_ignore_ascii_case("copy") {
        return Ok(Some(diffcore_rename::DETECT_COPY));
    }
    // git_config_rename falls through to git_config_bool, which is exactly the
    // `git_parse_maybe_bool` we already port for `--untracked-files`.
    match parse_maybe_bool(&text) {
        Some(true) => Ok(Some(diffcore_rename::DETECT_RENAME)),
        Some(false) => Ok(Some(0)),
        None => Err(text.into_owned()),
    }
}

/// `diff.renames`, git's `diff_detect_rename_default` (diff.c:398): the fallback
/// `diff_setup()` puts in `rev.diffopt.detect_rename` when `status.renames` left
/// `s->detect_rename` at `-1`. Absent, it is `DIFF_DETECT_RENAME` — rename
/// detection is on by default in every porcelain.
///
/// `git_config_rename()` dies on a value outside git's boolean grammar, and it
/// does so while reading the config, before the command line is parsed; the
/// shared reader in [`diffcore_rename::config_rename`] carries that exit.
fn configured_diff_renames(snap: &gix::config::Snapshot) -> u8 {
    match snap.string("diff.renames") {
        Some(value) => diffcore_rename::config_rename(Some(value.as_ref())),
        // A valueless `[diff]\n\trenames` is git's NULL value: plain detection.
        None => match snap.boolean("diff.renames") {
            Some(true) => diffcore_rename::DETECT_RENAME,
            Some(false) => 0,
            None => diffcore_rename::DETECT_RENAME,
        },
    }
}

/// `status.renameLimit`, else `diff.renameLimit`, else git's
/// `diff_rename_limit_default` of 1000 — the ceiling
/// `too_many_rename_candidates()` (diffcore-rename.c:1237) enforces on the
/// inexact matrix.
fn configured_rename_limit(snap: &gix::config::Snapshot) -> i64 {
    snap.integer("status.renameLimit")
        .or_else(|| snap.integer("diff.renameLimit"))
        .unwrap_or(diffcore_rename::DEFAULT_RENAME_LIMIT)
}

/// Resolve `status.showUntrackedFiles`, which stands in for an absent
/// `--untracked-files` flag. Anything unrecognised falls back to git's default.
fn configured_untracked(repo: &gix::Repository) -> Result<Untracked, ExitCode> {
    Ok(show_untracked_files_config(repo)?.unwrap_or(Untracked::Normal))
}

/// `git_status_config`'s `status.showUntrackedFiles` arm (builtin/commit.c:1509-1517):
/// the key resolved through the same [`parse_untracked_mode`] the command line
/// uses, so the boolean spellings it accepts (`false` → `no`, `true`/`2` →
/// `normal`) are accepted here too.
///
/// An unparseable value is not ignored. git's arm ends in
/// `return error(_("Invalid untracked files mode '%s'"), v)`, which fails the
/// config callback, and `git_config()` then dies naming where the value came from
/// (config.c:2555-2558). Both `git status` and `git commit` read the key through
/// `status_init_config()` *before* they parse their command line, so the death
/// happens even under a `-u<mode>` that would have overridden the value, and
/// before either command has printed anything.
fn show_untracked_files_config(repo: &gix::Repository) -> Result<Option<Untracked>, ExitCode> {
    let snapshot = repo.config_snapshot();
    let Some(value) = snapshot.string("status.showUntrackedFiles") else {
        return Ok(None);
    };
    let value = value.to_string();
    if let Some(mode) = parse_untracked_mode(&value) {
        return Ok(Some(mode));
    }
    // git reports the key lowercased, because that is the form the config
    // machinery canonicalized it to before the callback saw it.
    const KEY: &str = "status.showuntrackedfiles";
    eprintln!("error: Invalid untracked files mode '{value}'");
    let origin = match untracked_config_metadata(&snapshot) {
        Some(meta) => match meta.source {
            gix::config::Source::Cli | gix::config::Source::Env => {
                format!("unable to parse '{KEY}' from command-line config")
            }
            _ => match &meta.path {
                // gix records no per-value line number, so the `at line <n>` tail
                // git appends for a file-sourced value is omitted — the same
                // limitation this crate's other config-fatal paths carry (see
                // [`super::color::invalid_color_fatal`]).
                Some(path) => {
                    let shown = path.display().to_string();
                    let shown = shown.strip_prefix("./").unwrap_or(&shown).to_string();
                    format!("bad config variable '{KEY}' in file '{shown}'")
                }
                None => format!("bad config variable '{KEY}'"),
            },
        },
        None => format!("bad config variable '{KEY}'"),
    };
    eprintln!("fatal: {origin}");
    Err(ExitCode::from(128))
}

/// Where the *last* assignment of `status.showUntrackedFiles` came from — the one
/// whose value the snapshot returns, and so the one git would name.
fn untracked_config_metadata(
    snapshot: &gix::config::Snapshot<'_>,
) -> Option<gix::config::file::Metadata> {
    let mut found = None;
    for section in snapshot.plumbing().sections() {
        if !section.header().name().eq_ignore_ascii_case(b"status") {
            continue;
        }
        if section
            .value_names()
            .any(|v| v.eq_ignore_ascii_case("showUntrackedFiles"))
        {
            found = Some(section.meta().clone());
        }
    }
    found
}

/// [`show_untracked_files_config`] for its side effect alone: `git commit` reads
/// the same key through `status_init_config(&s, git_commit_config)`
/// (builtin/commit.c:1808), so a bad value kills it too — including the
/// `-m <msg>` path, which never renders a report.
pub(crate) fn validate_show_untracked_files(repo: &gix::Repository) -> Option<ExitCode> {
    show_untracked_files_config(repo).err()
}

/// Resolve a `--untracked-files=<mode>` / `-u<mode>` value the way git does.
/// The three named modes match verbatim; any other value is run through git's
/// `git_parse_maybe_bool`, where a truthy value means `normal` and a falsy value
/// means `no`. `None` is git's "Invalid untracked files mode" (fatal, exit 128).
fn parse_untracked_mode(value: &str) -> Option<Untracked> {
    match value {
        "no" => Some(Untracked::No),
        "normal" => Some(Untracked::Normal),
        "all" => Some(Untracked::All),
        _ => match parse_maybe_bool(value) {
            Some(true) => Some(Untracked::Normal),
            Some(false) => Some(Untracked::No),
            None => None,
        },
    }
}

/// Port of git's `git_parse_maybe_bool`: recognise the textual booleans, then
/// fall back to an integer parse where any non-zero value is `true`. `None` is
/// git's parse failure.
fn parse_maybe_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "" | "false" | "no" | "off" => return Some(false),
        "true" | "yes" | "on" => return Some(true),
        _ => {}
    }
    parse_git_int(value).map(|n| n != 0)
}

/// Port of git's `git_parse_int` (`git_parse_signed` with an `INT_MAX` ceiling):
/// C `strtoimax(value, &end, 0)` — base auto-detected from the `0x`/`0` prefix —
/// followed by `get_unit_factor` (an optional single `k`/`m`/`g` suffix, 1024-
/// based) and the range check. `None` is git's EINVAL / ERANGE.
fn parse_git_int(value: &str) -> Option<i64> {
    let b = value.as_bytes();
    let mut i = 0;
    // strtoimax skips leading C whitespace.
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let mut negative = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        negative = b[i] == b'-';
        i += 1;
    }
    // base-0 prefix detection: `0x`/`0X` (with a hex digit) is hex, a lone
    // leading `0` is octal, everything else decimal.
    let base: u64 = if i < b.len() && b[i] == b'0' {
        if i + 2 < b.len()
            && (b[i + 1] == b'x' || b[i + 1] == b'X')
            && (b[i + 2] as char).is_ascii_hexdigit()
        {
            i += 2;
            16
        } else {
            8 // the leading `0` is itself the first octal digit
        }
    } else {
        10
    };
    let digits_start = i;
    let mut val: i64 = 0;
    while i < b.len() {
        let digit = match b[i] {
            b'0'..=b'9' => (b[i] - b'0') as u64,
            b'a'..=b'f' => (b[i] - b'a' + 10) as u64,
            b'A'..=b'F' => (b[i] - b'A' + 10) as u64,
            _ => break,
        };
        if digit >= base {
            break;
        }
        // Overflow here is git's ERANGE from strtoimax.
        val = val.checked_mul(base as i64)?.checked_add(digit as i64)?;
        i += 1;
    }
    if i == digits_start {
        return None; // no digits converted -> EINVAL
    }
    if negative {
        val = -val;
    }
    // get_unit_factor: the remainder must be exactly empty or one of k/m/g.
    let factor: i64 = match value[i..].to_ascii_lowercase().as_str() {
        "" => 1,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        _ => return None, // EINVAL
    };
    // git_parse_int caps at INT_MAX before applying the factor.
    const MAX: i64 = i32::MAX as i64;
    if (val < 0 && -MAX / factor > val) || (val > 0 && MAX / factor < val) {
        return None; // ERANGE
    }
    Some(val * factor)
}

/// Parse the `<n>` of `-M<n>` / `--find-renames=<n>` through git's own
/// `parse_rename_score()` (diff.c:5679), the same reader `git diff -M<n>` uses:
/// `50`, `50%` and `.5` all mean half, and a trailing remainder the parser could
/// not consume is what makes git reject the option.
fn parse_similarity(raw: &str) -> Option<RenameOpts> {
    let (score, rest) = diffcore_rename::parse_rename_score(raw);
    if !rest.is_empty() {
        return None;
    }
    Some(RenameOpts {
        score,
        ..RenameOpts::renames()
    })
}

/// The repo-relative path a dirwalk entry should be displayed as: git suffixes a
/// `/` on directories (and nested repositories) it reports as a single entry.
fn walk_path(entry: &gix::dir::Entry) -> BString {
    let mut path = entry.rela_path.clone();
    if matches!(
        entry.disk_kind,
        Some(gix::dir::entry::Kind::Directory) | Some(gix::dir::entry::Kind::Repository)
    ) {
        path.push(b'/');
    }
    path
}

enum HeadState {
    Branch(String),
    /// `s->branch` is `"HEAD"`. The payload is
    /// `state->detached_from` / `state->detached_at` as
    /// [`detached_from`] worked them out; `None` is git's NULL, which prints
    /// `Not currently on any branch.`
    Detached { from: Option<String>, at: bool },
    Unborn(String),
}

/// Upstream relationship of the current branch, as git's `stat_tracking_info`
/// computes it.
struct Tracking {
    upstream: String,
    /// The configured upstream ref no longer exists.
    gone: bool,
    ahead: usize,
    behind: usize,
}

#[derive(Clone, Copy)]
enum StageKind {
    New,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
}

#[derive(Clone, Copy)]
enum WorkKind {
    Modified,
    Deleted,
    TypeChange,
    /// `git add -N`: the index holds a placeholder, so the path is a NEW FILE
    /// that is not staged. git reports it in the worktree column (` A`), never
    /// as a staged addition.
    Added,
    /// `diffcore_rename()` paired a worktree deletion with an intent-to-add
    /// destination: the rename that has not been staged, ` R old -> new`.
    Renamed,
    /// The same pairing, with the source used more than once.
    Copied,
    /// `short_submodule_status()`'s `m` (wt-status.c:453): a submodule whose recorded
    /// commit is unchanged but whose worktree has modified tracked content. Reachable
    /// only from `--short`, never from `--porcelain` or the long format.
    SubmoduleDirty,
    /// `short_submodule_status()`'s `?` (wt-status.c:455): the same, for untracked
    /// content only.
    SubmoduleUntracked,
}

/// The two `wt_status_change_data` fields a *gitlink* worktree change carries
/// (wt-status.c:484-489), which the three formats read differently:
///
/// * `--short` collapses them into one letter with `short_submodule_status()`;
/// * the long format appends them as ` (new commits, modified content, untracked
///   content)` (wt-status.c:399-409) and turns on the extra dirty-submodule hint
///   (wt-status.c:262);
/// * `--porcelain` and `--porcelain=v2` ignore them entirely, which is why
///   `git status --short` and `git status --porcelain` disagree on the very same
///   worktree.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct SubmoduleState {
    /// `d->new_submodule_commits`: `!oideq(&p->one->oid, &p->two->oid)` — the
    /// submodule has a different commit checked out than the index records.
    new_commits: bool,
    /// `DIRTY_SUBMODULE_MODIFIED`: `is_submodule_modified()` (submodule.c) saw a
    /// non-`?` line in the submodule's own status.
    modified: bool,
    /// `DIRTY_SUBMODULE_UNTRACKED`: it saw a `?` line.
    untracked: bool,
}

impl SubmoduleState {
    /// `d->dirty_submodule` as a whole, which is what the dirty-submodule hint and
    /// `wt_status_check_worktree_changes()` (wt-status.c:968) test.
    fn dirty(self) -> bool {
        self.modified || self.untracked
    }

    /// `is_submodule_modified()` (submodule.c:1880) classifying the lines of the
    /// submodule's own `git status --porcelain=2`:
    ///
    /// ```c
    /// if (buf.buf[0] == '?')                       /* regular untracked files */
    ///         dirty_submodule |= DIRTY_SUBMODULE_UNTRACKED;
    /// if (buf.buf[0] == 'u' || buf.buf[0] == '1' || buf.buf[0] == '2') {
    ///         if (buf.buf[5] == 'S' && buf.buf[8] == 'U')   /* nested untracked file */
    ///                 dirty_submodule |= DIRTY_SUBMODULE_UNTRACKED;
    ///         if (buf.buf[0] == 'u' || buf.buf[0] == '2' ||
    ///             memcmp(buf.buf + 5, "S..U", 4))          /* other change */
    ///                 dirty_submodule |= DIRTY_SUBMODULE_MODIFIED;
    /// }
    /// ```
    ///
    /// The middle clause is what makes this recursive, and it is not a corner case: a
    /// submodule whose *own* submodule holds untracked files is `S..U` at the inner
    /// level, and that `U` propagates outward without making the outer one "modified".
    /// A `1` line is therefore the only kind that can contribute `UNTRACKED` alone, and
    /// only when its whole `<sub>` column is exactly `S..U`.
    ///
    /// gix hands back the same changes as items rather than as formatted lines, so the
    /// `<sub>` column is re-derived by recursing into the nested
    /// [`gix::submodule::Status`]. Entries `status --porcelain=2` would not print —
    /// ignored paths, and the stat-only refreshes gix reports as `NeedsUpdate` — carry
    /// no line and therefore no bit.
    fn from_gix(sm: &gix::submodule::Status) -> SubmoduleState {
        use gix::status::index_worktree::Item as IwItem;
        use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
        use gix::status::Item;

        let mut state = SubmoduleState {
            new_commits: sm.checked_out_head_id != sm.index_id,
            ..SubmoduleState::default()
        };
        for item in sm.changes.iter().flatten() {
            match item {
                // The dirwalk's `?` lines. An ignored path is only listed under
                // `--ignored`, which this status never passes on.
                Item::IndexWorktree(IwItem::DirectoryContents { entry, .. }) => {
                    if entry.status == gix::dir::entry::Status::Untracked {
                        state.untracked = true;
                    }
                }
                Item::IndexWorktree(IwItem::Modification { status, .. }) => match status {
                    EntryStatus::Change(Change::SubmoduleModification(inner)) => {
                        let inner = SubmoduleState::from_gix(inner);
                        state.untracked |= inner.untracked;
                        // `memcmp(buf.buf + 5, "S..U", 4)`: anything but a bare
                        // nested-untracked marker is an "other change".
                        state.modified |= inner.new_commits || inner.modified || !inner.untracked;
                    }
                    EntryStatus::NeedsUpdate(_) => {}
                    _ => state.modified = true,
                },
                _ => state.modified = true,
            }
        }
        state
    }
}

/// `short_submodule_status()` (wt-status.c:449), applied by
/// `wt_status_collect_changed_cb()` (wt-status.c:488) *only* when the format is
/// `STATUS_FORMAT_SHORT` — so `git status --short` prints `m`/`?` for a submodule
/// whose recorded commit is unchanged while `git status --porcelain` prints `M`.
fn short_submodule_kind(kind: WorkKind, sub: SubmoduleState) -> WorkKind {
    if sub.new_commits {
        WorkKind::Modified
    } else if sub.modified {
        WorkKind::SubmoduleDirty
    } else if sub.untracked {
        WorkKind::SubmoduleUntracked
    } else {
        kind
    }
}

/// Shorten a `HEAD` referent name (`refs/heads/main` → `main`), or fall back.
fn referent_short(name: Option<&gix::refs::FullNameRef>, fallback: &str) -> String {
    use gix::bstr::ByteSlice;
    name.map(|n| n.shorten().to_str_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_owned())
}

/// Map an index-entry mode to a coarse type class, ignoring the executable bit
/// (git treats a permission-only change as `modified`, not `typechange`).
/// 0 = regular blob, 1 = symlink, 2 = gitlink/commit, 3 = tree.
/// One path's porcelain-v2 record, merged across the tree↔index (staged) and
/// index↔worktree (unstaged) passes. Modes are the git octal values.
struct V2Rec {
    x: u8,
    y: u8,
    m_h: u32,
    m_i: u32,
    m_w: u32,
    h_h: gix::hash::ObjectId,
    h_i: gix::hash::ObjectId,
    /// Whether a tree↔index change set the HEAD/index fields (else fill from index).
    staged: bool,
    /// `git add -N`: the index entry is a placeholder, so git reports mode
    /// `000000` and a null oid for BOTH the HEAD and index columns rather than
    /// the placeholder's own values.
    ita: bool,
    /// `d->new_submodule_commits` and `d->dirty_submodule`, which fill the `<sub>`
    /// column (`wt_porcelain_v2_submodule_state()`, wt-status.c:2308).
    sub: SubmoduleState,
    /// `(R|C, similarity, source-path)` for a rename/copy — renders a `2` line.
    rename: Option<(u8, u32, BString)>,
}

/// `d->mode_worktree`: `check_removed()` (diff-lib.c:42) followed by
/// `ce_mode_from_stat()` (read-cache.h:8) for one path.
///
/// A vanished path is `0`, which is what `run_diff_files()`'s unmerged branch writes
/// (`wt_mode = 0`, diff-lib.c:169) and what `git status --porcelain=v2` prints as the
/// `<mW>` column of a `u` line for a conflict whose file was deleted from the worktree.
/// A directory is `S_IFGITLINK` when it is a repository (`create_ce_mode()`,
/// object.h:140) and `0` when it is not, since `check_removed()` then calls the entry
/// removed.
fn worktree_mode(repo: &gix::Repository, path: &gix::bstr::BStr) -> u32 {
    let Some(wd) = repo.workdir() else {
        return 0;
    };
    let full = wd.join(gix::path::from_bstr(path));
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.file_type().is_symlink() => 0o120000,
        Ok(m) if m.file_type().is_dir() => {
            match super::worktree_filespec::removed_became_gitlink(wd, path) {
                Some(_) => 0o160000,
                None => 0,
            }
        }
        Ok(_m) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if _m.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                }
            }
            #[cfg(not(unix))]
            {
                0o100644
            }
        }
        Err(_) => 0,
    }
}

/// `check_removed()`'s directory arm (diff-lib.c:58) for a path gix reported as
/// `Change::Removed`: `Some(())` when the vanished entry's name was taken by a
/// checked-out repository, which git reports as a type change to `160000` rather than a
/// deletion, `None` when the removal stands.
fn removed_gitlink(repo: &gix::Repository, rela_path: &gix::bstr::BStr) -> Option<()> {
    let wd = repo.workdir()?;
    super::worktree_filespec::removed_became_gitlink(wd, rela_path).map(|_| ())
}

/// `wt_porcelain_v2_submodule_state()` (wt-status.c:2308): the `<sub>` column.
///
/// ```c
/// if (S_ISGITLINK(d->mode_head) || S_ISGITLINK(d->mode_index) ||
///     S_ISGITLINK(d->mode_worktree)) {
///         sub[0] = 'S';
///         sub[1] = d->new_submodule_commits ? 'C' : '.';
///         sub[2] = (d->dirty_submodule & DIRTY_SUBMODULE_MODIFIED) ? 'M' : '.';
///         sub[3] = (d->dirty_submodule & DIRTY_SUBMODULE_UNTRACKED) ? 'U' : '.';
/// } else { "N..." }
/// ```
///
/// `modes` are the mode columns of the record being printed. For a `1`/`2` line those
/// are git's own three; for a `u` line git tests the same three accumulated fields
/// while the line itself prints the stage modes, so the stage modes plus the worktree
/// mode stand in — measured equal against git 2.55.0 for an `AA` gitlink conflict both
/// with the submodule checked out (`u AA S... 000000 160000 160000 160000 …`) and with
/// its directory removed (`… 160000 160000 000000 …`, still `S...`).
fn v2_submodule_token(modes: &[u32], sub: SubmoduleState) -> String {
    if !modes.iter().any(|m| *m == 0o160000) {
        return "N...".to_owned();
    }
    let flag = |on: bool, c: char| if on { c } else { '.' };
    format!(
        "S{}{}{}",
        flag(sub.new_commits, 'C'),
        flag(sub.modified, 'M'),
        flag(sub.untracked, 'U'),
    )
}

/// `index_name_is_other()` (read-cache.c:3442): whether a dirwalk entry is really
/// untracked, i.e. the index does not mention its name at any stage.
///
/// The trailing `/` a directory entry carries is stripped first, which is the whole
/// point: with `f` a tracked blob and a directory now standing at `f`, the dirwalk
/// offers `f/`, and git drops it because the index holds `f`. `git status` then prints
/// ` D f` and nothing else, while `-uall` lists the files inside under their own names.
/// `wt_status_collect_untracked()` (wt-status.c:834, :840) applies it to the untracked
/// and the ignored list alike.
fn index_name_is_other(index: &gix::index::State, name: &gix::bstr::BStr) -> bool {
    let stem = name.strip_suffix(b"/").map_or(name, gix::bstr::ByteSlice::as_bstr);
    // `entry_range()` spans every stage of one path, so it answers the C's two tests at
    // once: the exact stage-0 hit and the unmerged entry at the insertion point.
    index.entry_range(stem).is_none()
}

/// `git status --porcelain=v2` — the stable machine format (git-status(1),
/// "Porcelain Format Version 2"). Ordinary changes render as
/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`, renames/copies as `2 …`,
/// unmerged paths as `u …`, and untracked / ignored as `? <path>` / `! <path>`;
/// with `--branch` the `# branch.*` header precedes them. A separate renderer
/// from v1/long — it shares no collection, so neither can regress the other.
#[allow(clippy::too_many_arguments)]
fn porcelain_v2_output(
    repo: &gix::Repository,
    // git's `opt.def` for the staged half, already resolved by the caller the way
    // `run_diff_index` resolves it: the reference's tree, or the empty tree when
    // the reference is unborn (`s->is_initial`).
    head_tree: ObjectId,
    untracked: Untracked,
    show_ignored: bool,
    ignored_matching: bool,
    renames: RenameOpts,
    branch_header: bool,
    pathspecs: &[BString],
    show_stash: bool,
    quick: bool,
    ignore_submodules: Option<gix::submodule::config::Ignore>,
    ignore_all: bool,
    null_term: bool,
    prefix: Option<&[u8]>,
) -> Result<ExitCode> {
    use gix::bstr::ByteSlice;
    use std::collections::BTreeMap;

    let zero = gix::hash::ObjectId::null(gix::hash::Kind::Sha1);
    let mut out = String::new();

    // ---------------------------------------------------------------- header
    // With `-z` the header is emitted as NUL-terminated raw bytes further down
    // (git's `use_deferred_config` is off and every terminator becomes NUL), so
    // the LF/`String` header below is built only for the non-`-z` formats.
    if branch_header && !null_term {
        match repo.head_id() {
            Ok(id) => out.push_str(&format!("# branch.oid {}\n", id.detach())),
            Err(_) => out.push_str("# branch.oid (initial)\n"),
        }
        let head = repo.head()?;
        let head_name = if head.is_detached() {
            "(detached)".to_string()
        } else {
            head.referent_name()
                .map(|n| n.shorten().to_str_lossy().into_owned())
                .unwrap_or_else(|| "(detached)".to_string())
        };
        drop(head);
        out.push_str(&format!("# branch.head {head_name}\n"));
        if let Some(t) = tracking_info(repo)? {
            out.push_str(&format!("# branch.upstream {}\n", t.upstream));
            if !t.gone {
                // FULL prints the exact counts (`+0 -0` when identical); QUICK knows
                // only whether the branches diverged, so a divergence is `+? -?`.
                if quick && (t.ahead > 0 || t.behind > 0) {
                    out.push_str("# branch.ab +? -?\n");
                } else {
                    out.push_str(&format!("# branch.ab +{} -{}\n", t.ahead, t.behind));
                }
            }
        }
    }

    // `# stash <n>` follows the branch header (independent of `--branch`), before
    // the change entries; git omits it when there are no stash entries. (`-z`
    // renders it as NUL-terminated bytes in the null-termination branch below.)
    if show_stash && !null_term {
        let n = count_stash_entries(repo);
        if n > 0 {
            out.push_str(&format!("# stash {n}\n"));
        }
    }

    // --------------------------------------------------------------- collect
    let mut recs: BTreeMap<BString, V2Rec> = BTreeMap::new();
    let mut unmerged: Vec<(u8, BString)> = Vec::new();
    let mut untracked_paths: Vec<BString> = Vec::new();
    let mut ignored_paths: Vec<BString> = Vec::new();

    let new_rec = || V2Rec {
        x: b'.',
        y: b'.',
        m_h: 0,
        m_i: 0,
        m_w: 0,
        h_h: zero,
        h_i: zero,
        staged: false,
        ita: false,
        rename: None,
        sub: SubmoduleState::default(),
    };

    // The two `diff_filepair` queues `diffcore_rename()` runs over — one per half
    // of the report, as git has one per `run_diff_index()` / `run_diff_files()`.
    let mut staged_pairs: Vec<(RenameSide, RenameSide)> = Vec::new();
    let mut work_pairs: Vec<(RenameSide, RenameSide)> = Vec::new();
    let hash = repo.object_hash();

    let mut platform = repo
        .status(gix::progress::Discard)?
        .index_worktree_options_mut(preload_index_threads(repo))
        .head_tree(head_tree)
        .untracked_files(match untracked {
            Untracked::No => gix::status::UntrackedFiles::None,
            Untracked::Normal => gix::status::UntrackedFiles::Collapsed,
            Untracked::All => gix::status::UntrackedFiles::Files,
        });
    if show_ignored {
        // `--ignored=matching` reports whatever the ignore pattern matched, so it never
        // collapses; the traditional mode follows the untracked granularity, and under
        // `-uall` it additionally descends into an ignored directory to name the files
        // in it.
        let mode = if untracked == Untracked::All || ignored_matching {
            gix::dir::walk::EmissionMode::Matching
        } else {
            gix::dir::walk::EmissionMode::CollapseDirectory
        };
        let descend = untracked == Untracked::All && !ignored_matching;
        platform = platform.dirwalk_options(|opts| {
            opts.emit_ignored(Some(mode)).recurse_ignored_directories(descend)
        });
    }
    // Rename detection runs as git's own `diffcore_rename()` pass below, over
    // both halves of the report; gix's tracker stays off.
    platform = platform.tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled);
    if let Some(ignore) = ignore_submodules {
        platform = platform.index_worktree_submodules(gix::status::Submodule::Given {
            ignore,
            check_dirty: true,
        });
    }

    let patterns: Vec<BString> = pathspecs.to_vec();
    for item in platform.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                // `--ignore-submodules=all` hides staged gitlink changes too.
                if ignore_all {
                    let mode = match &change {
                        ChangeRef::Addition { entry_mode, .. }
                        | ChangeRef::Deletion { entry_mode, .. }
                        | ChangeRef::Modification { entry_mode, .. }
                        | ChangeRef::Rewrite { entry_mode, .. } => *entry_mode,
                    };
                    if type_class(mode) == 2 {
                        continue;
                    }
                }
                match change {
                    ChangeRef::Addition {
                        location,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let path = location.into_owned();
                        let id = id.into_owned();
                        staged_pairs.push((
                            RenameSide::absent(path.clone(), hash),
                            RenameSide {
                                path: path.clone(),
                                mode: entry_mode.bits(),
                                id,
                                id_valid: true,
                            },
                        ));
                        let r = recs.entry(path).or_insert_with(new_rec);
                        r.x = b'A';
                        r.m_i = entry_mode.bits();
                        r.h_i = id;
                        r.staged = true;
                    }
                    ChangeRef::Deletion {
                        location,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let path = location.into_owned();
                        let id = id.into_owned();
                        staged_pairs.push((
                            RenameSide {
                                path: path.clone(),
                                mode: entry_mode.bits(),
                                id,
                                id_valid: true,
                            },
                            RenameSide::absent(path.clone(), hash),
                        ));
                        let r = recs.entry(path).or_insert_with(new_rec);
                        r.x = b'D';
                        r.m_h = entry_mode.bits();
                        r.h_h = id;
                        r.staged = true;
                    }
                    ChangeRef::Modification {
                        location,
                        previous_entry_mode,
                        previous_id,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let path = location.into_owned();
                        let previous_id = previous_id.into_owned();
                        let id = id.into_owned();
                        staged_pairs.push((
                            RenameSide {
                                path: path.clone(),
                                mode: previous_entry_mode.bits(),
                                id: previous_id,
                                id_valid: true,
                            },
                            RenameSide {
                                path: path.clone(),
                                mode: entry_mode.bits(),
                                id,
                                id_valid: true,
                            },
                        ));
                        let r = recs.entry(path).or_insert_with(new_rec);
                        r.x = if type_class(previous_entry_mode) != type_class(entry_mode) {
                            b'T'
                        } else {
                            b'M'
                        };
                        r.m_h = previous_entry_mode.bits();
                        r.h_h = previous_id;
                        r.m_i = entry_mode.bits();
                        r.h_i = id;
                        r.staged = true;
                    }
                    // Rename tracking is off in the platform, so gix never emits
                    // this: the pairing is `diffcore_rename()`'s below.
                    ChangeRef::Rewrite {
                        location,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let r = recs.entry(location.into_owned()).or_insert_with(new_rec);
                        r.x = b'M';
                        r.m_i = entry_mode.bits();
                        r.h_i = id.into_owned();
                        r.staged = true;
                    }
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::{Change, Conflict, EntryStatus};
                match iw {
                    Item::Modification {
                        rela_path,
                        status,
                        entry,
                        ..
                    } => match status {
                        EntryStatus::Conflict { summary, .. } => {
                            let mask = match summary {
                                Conflict::BothDeleted => 1,
                                Conflict::AddedByUs => 2,
                                Conflict::DeletedByThem => 3,
                                Conflict::AddedByThem => 4,
                                Conflict::DeletedByUs => 5,
                                Conflict::BothAdded => 6,
                                Conflict::BothModified => 7,
                            };
                            unmerged.push((mask, rela_path));
                        }
                        // `rev.diffopt.ita_invisible_in_index = 1` (wt-status.c:665):
                        // `diff-files` queues an intent-to-add entry as an addition,
                        // which makes it the only worktree-side rename destination.
                        EntryStatus::IntentToAdd => {
                            work_pairs.push((
                                RenameSide::absent(rela_path.clone(), hash),
                                RenameSide {
                                    path: rela_path.clone(),
                                    mode: worktree_mode(repo, gix::bstr::BStr::new(&rela_path)),
                                    id: ObjectId::null(hash),
                                    id_valid: false,
                                },
                            ));
                            let r = recs.entry(rela_path).or_insert_with(new_rec);
                            r.y = b'A';
                            r.ita = true;
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(change) => {
                            // gix reports every non-submodule entry whose path is a
                            // directory as removed; `check_removed()` (diff-lib.c:58)
                            // only agrees when that directory is not a repository, and
                            // otherwise `ce_mode_from_stat()` makes the pair a type
                            // change to `160000`.
                            let (y, sub) = match change {
                                Change::Removed => {
                                    match removed_gitlink(repo, gix::bstr::BStr::new(&rela_path)) {
                                        // `wt_status_collect_changed_cb()`
                                        // (wt-status.c:486) computes
                                        // `new_submodule_commits` from the pair's two
                                        // ids, and the worktree side of a `diff-files`
                                        // gitlink is null — so it is always set here.
                                        Some(()) => (
                                            b'T',
                                            SubmoduleState {
                                                new_commits: true,
                                                ..SubmoduleState::default()
                                            },
                                        ),
                                        None => (b'D', SubmoduleState::default()),
                                    }
                                }
                                Change::Type { .. } => (b'T', SubmoduleState::default()),
                                Change::Modification { .. } => {
                                    (b'M', SubmoduleState::default())
                                }
                                Change::SubmoduleModification(sm) => {
                                    (b'M', SubmoduleState::from_gix(&sm))
                                }
                            };
                            // The worktree side of the pair is unhashed, as
                            // `diff-files` leaves it; a deletion has no worktree
                            // side at all, which is what makes it a rename source.
                            let wt_mode = if y == b'D' {
                                0
                            } else {
                                worktree_mode(repo, gix::bstr::BStr::new(&rela_path))
                            };
                            work_pairs.push((
                                RenameSide {
                                    path: rela_path.clone(),
                                    mode: entry.mode.bits(),
                                    id: entry.id,
                                    id_valid: true,
                                },
                                RenameSide {
                                    path: rela_path.clone(),
                                    mode: wt_mode,
                                    id: ObjectId::null(hash),
                                    id_valid: false,
                                },
                            ));
                            let r = recs.entry(rela_path).or_insert_with(new_rec);
                            r.y = y;
                            r.sub = sub;
                        }
                    },
                    Item::DirectoryContents { entry, .. } => match entry.status {
                        gix::dir::entry::Status::Untracked => untracked_paths.push(walk_path(&entry)),
                        gix::dir::entry::Status::Ignored(_) => ignored_paths.push(walk_path(&entry)),
                        _ => {}
                    },
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }

    // ------------------------------------------------------ diffcore_rename()
    // `wt_status_collect_updated_cb()`'s `DIFF_STATUS_RENAMED` arm
    // (wt-status.c:559): the destination record keeps the rename source's path,
    // mode and id in its HEAD columns, plus `d->rename_score`. The source's own
    // deletion record is consumed by the rename and never printed.
    for rw in detect_rewrites(repo, &staged_pairs, renames) {
        match recs.get_mut(&rw.src.path) {
            Some(r) if r.x == b'D' && r.y == b'.' => {
                recs.remove(&rw.src.path);
            }
            Some(r) if r.x == b'D' => {
                r.x = b'.';
                r.m_h = 0;
                r.h_h = zero;
                r.staged = false;
            }
            _ => {}
        }
        if let Some(r) = recs.get_mut(&rw.dst.path) {
            r.x = rw.kind;
            r.m_h = rw.src.mode;
            r.h_h = rw.src.id;
            r.rename = Some((rw.kind, rw.score, rw.src.path.clone()));
        }
    }
    // `wt_status_collect_changed_cb()`'s `DIFF_STATUS_RENAMED` arm
    // (wt-status.c:520): the *index* columns come from the rename source, and the
    // HEAD columns then follow them through `wt_porcelain_v2_fix_up_changed()`
    // because nothing is staged for this path.
    for rw in detect_rewrites(repo, &work_pairs, renames) {
        if recs.get(&rw.src.path).is_some_and(|r| r.y == b'D' && r.x == b'.') {
            recs.remove(&rw.src.path);
        }
        if let Some(r) = recs.get_mut(&rw.dst.path) {
            r.y = rw.kind;
            r.rename = Some((rw.kind, rw.score, rw.src.path.clone()));
            if !r.staged {
                r.m_i = rw.src.mode;
                r.h_i = rw.src.id;
                r.m_h = r.m_i;
                r.h_h = r.h_i;
                r.staged = true;
                r.ita = false;
            }
        }
    }

    // --------------------------------------- fill from index & worktree stat
    let index = repo.index_or_empty()?;
    // `wt_status_collect_untracked()` (wt-status.c:834, :840): a dirwalk entry whose
    // name the index already holds is not untracked content.
    untracked_paths.retain(|p| index_name_is_other(&index, gix::bstr::BStr::new(p)));
    ignored_paths.retain(|p| index_name_is_other(&index, gix::bstr::BStr::new(p)));
    for (path, r) in recs.iter_mut() {
        if !r.staged && !r.ita {
            // No staged change: HEAD == index for this path, so pull both from
            // the stage-0 index entry. An intent-to-add placeholder is skipped:
            // git reports it as absent from HEAD and index alike.
            if let Ok(idx) = index.entry_index_by_path(path.as_bstr()) {
                let e = &index.entries()[idx];
                r.m_i = e.mode.bits();
                r.h_i = e.id;
            }
            r.m_h = r.m_i;
            r.h_h = r.h_i;
        }
        r.m_w = match r.y {
            b'D' => 0,
            b'.' => r.m_i, // worktree matches the index
            _ => worktree_mode(repo, path.as_bstr()),
        };
    }

    // ------------------------------------------------- -z (null-terminated)
    // A separate byte renderer: every terminator is NUL, the rename separator
    // is NUL (with the current path first), and paths are emitted raw — never
    // C-quoted — so binary paths survive (a `String` would be lossy). This keeps
    // the LF/`String` renderer below byte-for-byte unchanged for the common case.
    if null_term {
        let mut b: Vec<u8> = Vec::new();

        // Header — same fields as the LF form (git's `wt_porcelain_v2_print_tracking`
        // with `eol = '\0'`), NUL-terminated and uncolored.
        if branch_header {
            match repo.head_id() {
                Ok(id) => b.extend_from_slice(format!("# branch.oid {}", id.detach()).as_bytes()),
                Err(_) => b.extend_from_slice(b"# branch.oid (initial)"),
            }
            b.push(0);
            let head = repo.head()?;
            let head_name = if head.is_detached() {
                "(detached)".to_string()
            } else {
                head.referent_name()
                    .map(|n| n.shorten().to_str_lossy().into_owned())
                    .unwrap_or_else(|| "(detached)".to_string())
            };
            drop(head);
            b.extend_from_slice(format!("# branch.head {head_name}").as_bytes());
            b.push(0);
            if let Some(t) = tracking_info(repo)? {
                b.extend_from_slice(format!("# branch.upstream {}", t.upstream).as_bytes());
                b.push(0);
                if !t.gone {
                    if quick && (t.ahead > 0 || t.behind > 0) {
                        b.extend_from_slice(b"# branch.ab +? -?");
                    } else {
                        b.extend_from_slice(
                            format!("# branch.ab +{} -{}", t.ahead, t.behind).as_bytes(),
                        );
                    }
                    b.push(0);
                }
            }
        }
        if show_stash {
            let n = count_stash_entries(repo);
            if n > 0 {
                b.extend_from_slice(format!("# stash {n}").as_bytes());
                b.push(0);
            }
        }

        // 1/2/u entry lines, together and sorted by path.
        let mut lines: Vec<(BString, Vec<u8>)> = Vec::new();
        for (path, r) in &recs {
            let xy = format!("{}{}", r.x as char, r.y as char);
            let sub = v2_submodule_token(&[r.m_h, r.m_i, r.m_w], r.sub);
            let mut line: Vec<u8> = Vec::new();
            if let Some((kind, score, ref orig)) = r.rename {
                line.extend_from_slice(
                    format!(
                        "2 {xy} {sub} {:06o} {:06o} {:06o} {} {} {}{} ",
                        r.m_h, r.m_i, r.m_w, r.h_h, r.h_i, kind as char, score,
                    )
                    .as_bytes(),
                );
                line.extend_from_slice(path);
                line.push(0);
                line.extend_from_slice(orig);
            } else {
                line.extend_from_slice(
                    format!(
                        "1 {xy} {sub} {:06o} {:06o} {:06o} {} {} ",
                        r.m_h, r.m_i, r.m_w, r.h_h, r.h_i,
                    )
                    .as_bytes(),
                );
                line.extend_from_slice(path);
            }
            lines.push((path.clone(), line));
        }
        for (mask, path) in &unmerged {
            let xy = match mask {
                1 => "DD",
                2 => "AU",
                3 => "UD",
                4 => "UA",
                5 => "DU",
                6 => "AA",
                _ => "UU",
            };
            let mut sm = [0u32; 3];
            let mut sh = [zero; 3];
            for e in index.entries() {
                if e.path(&index) == path.as_bstr() {
                    match e.stage_raw() {
                        1 => {
                            sm[0] = e.mode.bits();
                            sh[0] = e.id;
                        }
                        2 => {
                            sm[1] = e.mode.bits();
                            sh[1] = e.id;
                        }
                        3 => {
                            sm[2] = e.mode.bits();
                            sh[2] = e.id;
                        }
                        _ => {}
                    }
                }
            }
            let m_w = worktree_mode(repo, path.as_bstr());
            // An unmerged path has no `wt_status_change_data` here, so the stage modes
            // stand in for git's accumulated `mode_head`/`mode_index`, and it never
            // carries dirty-submodule bits. See [`v2_submodule_token`].
            let sub = v2_submodule_token(&[sm[0], sm[1], sm[2], m_w], SubmoduleState::default());
            let mut line: Vec<u8> = Vec::new();
            line.extend_from_slice(
                format!(
                    "u {xy} {sub} {:06o} {:06o} {:06o} {:06o} {} {} {} ",
                    sm[0], sm[1], sm[2], m_w, sh[0], sh[1], sh[2],
                )
                .as_bytes(),
            );
            line.extend_from_slice(path);
            lines.push((path.clone(), line));
        }
        lines.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, line) in lines {
            b.extend_from_slice(&line);
            b.push(0);
        }

        untracked_paths.sort();
        ignored_paths.sort();
        for p in &untracked_paths {
            b.extend_from_slice(b"? ");
            b.extend_from_slice(p);
            b.push(0);
        }
        for p in &ignored_paths {
            b.extend_from_slice(b"! ");
            b.extend_from_slice(p);
            b.push(0);
        }

        use std::io::Write;
        let _ = std::io::stdout().write_all(&b);
        return Ok(ExitCode::SUCCESS);
    }

    // ------------------------------------------------------------- render
    // git emits 1/2/u lines together, sorted by path, then '?' then '!'.
    let mut lines: Vec<(BString, String)> = Vec::new();
    for (path, r) in &recs {
        let xy = format!("{}{}", r.x as char, r.y as char);
            let sub = v2_submodule_token(&[r.m_h, r.m_i, r.m_w], r.sub);
        let line = if let Some((kind, score, ref orig)) = r.rename {
            format!(
                "2 {xy} {sub} {:06o} {:06o} {:06o} {} {} {}{} {}\t{}",
                r.m_h,
                r.m_i,
                r.m_w,
                r.h_h,
                r.h_i,
                kind as char,
                score,
                quote_path(path, prefix),
                quote_path(orig, prefix),
            )
        } else {
            format!(
                "1 {xy} {sub} {:06o} {:06o} {:06o} {} {} {}",
                r.m_h,
                r.m_i,
                r.m_w,
                r.h_h,
                r.h_i,
                quote_path(path, prefix),
            )
        };
        lines.push((path.clone(), line));
    }
    for (mask, path) in &unmerged {
        let xy = match mask {
            1 => "DD",
            2 => "AU",
            3 => "UD",
            4 => "UA",
            5 => "DU",
            6 => "AA",
            _ => "UU",
        };
        // Per-stage (1=base, 2=ours, 3=theirs) modes and oids from the index.
        let mut sm = [0u32; 3];
        let mut sh = [zero; 3];
        for e in index.entries() {
            if e.path(&index) == path.as_bstr() {
                match e.stage_raw() {
                    1 => {
                        sm[0] = e.mode.bits();
                        sh[0] = e.id;
                    }
                    2 => {
                        sm[1] = e.mode.bits();
                        sh[1] = e.id;
                    }
                    3 => {
                        sm[2] = e.mode.bits();
                        sh[2] = e.id;
                    }
                    _ => {}
                }
            }
        }
        let m_w = worktree_mode(repo, path.as_bstr());
        // An unmerged path has no `wt_status_change_data` here, so the stage modes stand
        // in for git's accumulated `mode_head`/`mode_index`, and it never carries
        // dirty-submodule bits. See [`v2_submodule_token`].
        let sub = v2_submodule_token(&[sm[0], sm[1], sm[2], m_w], SubmoduleState::default());
        let line = format!(
            "u {xy} {sub} {:06o} {:06o} {:06o} {:06o} {} {} {} {}",
            sm[0],
            sm[1],
            sm[2],
            m_w,
            sh[0],
            sh[1],
            sh[2],
            quote_path(path, prefix),
        );
        lines.push((path.clone(), line));
    }
    lines.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, line) in lines {
        out.push_str(&line);
        out.push('\n');
    }

    untracked_paths.sort();
    ignored_paths.sort();
    for p in &untracked_paths {
        out.push_str(&format!("? {}\n", quote_path(p, prefix)));
    }
    for p in &ignored_paths {
        out.push_str(&format!("! {}\n", quote_path(p, prefix)));
    }

    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

fn type_class(mode: gix::index::entry::Mode) -> u8 {
    match mode.to_tree_entry_mode() {
        Some(m) if m.is_link() => 1,
        Some(m) if m.is_commit() => 2,
        Some(m) if m.is_tree() => 3,
        _ => 0,
    }
}

/// git's `prefix` — the path from the work-tree root down to the current
/// directory, with a trailing `/`. `None` at the top level (git passes a NULL
/// prefix there, which `relative_path` short-circuits on).
fn display_prefix(repo: &gix::Repository) -> Result<Option<BString>> {
    Ok(match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let mut b = gix::path::into_bstr(p).into_owned();
            b.push(b'/');
            Some(b)
        }
        _ => None,
    })
}

/// Port of `relative_path()` (path.c): re-express the repository-root-relative
/// `input` relative to `prefix` (itself root-relative, with a trailing `/`).
///
/// Both arguments are relative here, so git's `have_same_root` is always true
/// and the DOS-drive skip is a no-op — the scan therefore starts at index 0 of
/// each. The loop walks the common byte prefix, remembering the last directory
/// boundary it crossed (`prefix_off` / `input_off`); what is left of `prefix`
/// past that boundary becomes one `../` per component, and what is left of
/// `input` is appended verbatim. A path equal to the prefix renders as `./`.
fn relative_path(input: &[u8], prefix: &[u8]) -> Vec<u8> {
    let (in_len, prefix_len) = (input.len(), prefix.len());
    if in_len == 0 {
        return b"./".to_vec();
    } else if prefix_len == 0 {
        return input.to_vec();
    }

    let (mut in_off, mut prefix_off) = (0usize, 0usize);
    let (mut i, mut j) = (0usize, 0usize);
    while i < prefix_len && j < in_len && prefix[i] == input[j] {
        if prefix[i] == b'/' {
            while i < prefix_len && prefix[i] == b'/' {
                i += 1;
            }
            while j < in_len && input[j] == b'/' {
                j += 1;
            }
            prefix_off = i;
            in_off = j;
        } else {
            i += 1;
            j += 1;
        }
    }

    if i >= prefix_len && prefix_off < prefix_len {
        // `prefix` looks like a prefix of `input`, and does not end in `/`.
        if j >= in_len {
            // input="a/b", prefix="a/b"
            in_off = in_len;
        } else if input[j] == b'/' {
            // input="a/b/c", prefix="a/b"
            while j < in_len && input[j] == b'/' {
                j += 1;
            }
            in_off = j;
        } else {
            // input="a/bbb/c", prefix="a/b" — not a component prefix after all.
            i = prefix_off;
        }
    } else if j >= in_len && in_off < in_len {
        // `input` is shorter than `prefix` and does not end in `/`.
        if i < prefix_len && prefix[i] == b'/' {
            // input="a/b", prefix="a/b/c/"
            while i < prefix_len && prefix[i] == b'/' {
                i += 1;
            }
            in_off = in_len;
        }
    }
    let rest = &input[in_off..];

    if i >= prefix_len {
        return if rest.is_empty() { b"./".to_vec() } else { rest.to_vec() };
    }

    let mut out: Vec<u8> = Vec::with_capacity(rest.len());
    while i < prefix_len {
        if prefix[i] == b'/' {
            out.extend_from_slice(b"../");
            while i < prefix_len && prefix[i] == b'/' {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    if prefix[prefix_len - 1] != b'/' {
        out.extend_from_slice(b"../");
    }
    out.extend_from_slice(rest);
    out
}

/// Port of `quote_path()` (quote.c): make `path` relative to `prefix` (git's
/// `s->prefix`, which is `NULL` whenever `status.relativePaths` is off or the
/// format never re-bases paths), then hand it to `quote_c_style()`. The table and
/// the `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>, prefix: Option<&[u8]>) -> String {
    let rebased;
    let bytes = match prefix {
        Some(p) => {
            rebased = relative_path(path.as_ref(), p);
            rebased.as_slice()
        }
        None => path.as_ref(),
    };
    crate::quote::quoted_name_string(bytes)
}

/// `core.preloadIndex` (`read-cache.c`'s `preload_index()`): git fans the index
/// refresh's `lstat()` pass out over threads unless the key is false, in which
/// case `preload_index()` returns immediately and the refresh runs on one
/// thread. Default is on.
///
/// The knob picks a code path, never a different answer — `git -c
/// core.preloadIndex=false status` prints exactly what the threaded run prints,
/// which is why the parity corpus sees no difference either.
fn preload_index_threads(
    repo: &gix::Repository,
) -> impl FnOnce(&mut gix::status::index_worktree::Options) {
    let preload = repo
        .config_snapshot()
        .boolean("core.preloadIndex")
        .unwrap_or(true);
    // `Repository::status()` reads `status.showUntrackedFiles` itself
    // (gix/src/status/mod.rs:119-129), and `Platform::untracked_files(None)`
    // *takes* the dirwalk options out of the platform
    // (gix/src/status/platform.rs:35) — after which the walk cannot be turned
    // back on, because every later setter mutates options that are no longer
    // there. git resolves the key and the flag together with the flag winning
    // (`handle_untracked_files_arg()`, builtin/commit.c:1215), so
    // `-c status.showUntrackedFiles=no status -uall` must still walk. Putting
    // the defaults back here restores that: the caller's own
    // `untracked_files(...)` runs after this and has the final say.
    let dirwalk = repo.dirwalk_options().ok();
    move |opts| {
        if !preload {
            opts.thread_limit = Some(1);
        }
        if opts.dirwalk_options.is_none() {
            opts.dirwalk_options = dirwalk;
        }
    }
}

/// Resolve the upstream of the current branch and how far it has diverged.
/// Returns `None` when no upstream is configured, matching git's "no tracking
/// information at all" case.
/// `AB_DELAY_WARNING_IN_MS` / `UF_DELAY_WARNING_IN_MS` (wt-status.c): both are two
/// seconds, the point past which git decides the wait was worth a word.
const AB_DELAY_WARNING_IN_MS: u64 = 2 * 1000;
const UF_DELAY_WARNING_IN_MS: u64 = 2 * 1000;

/// Port of `uf_was_slow()` (wt-status.c). `t0` is `Some` only when
/// `advice.statusUoption` allowed the walk to be timed at all; the result is the
/// elapsed seconds to name in the hint, or `None` to stay quiet.
///
/// `GIT_TEST_UF_DELAY_WARNING` pins the elapsed time to git's own 3250 ms so the
/// hint can be exercised without a repository big enough to be genuinely slow.
fn uf_was_slow(t0: Option<std::time::Instant>) -> Option<f64> {
    let t0 = t0?;
    let ms = if std::env::var_os("GIT_TEST_UF_DELAY_WARNING").is_some() {
        3250
    } else {
        t0.elapsed().as_millis() as u64
    };
    (ms > UF_DELAY_WARNING_IN_MS).then(|| ms as f64 / 1000.0)
}

fn tracking_info(repo: &gix::Repository) -> Result<Option<Tracking>> {
    use gix::bstr::ByteSlice;

    let Some(branch_ref) = repo.head_ref()? else {
        return Ok(None);
    };
    let Some(Ok(upstream_name)) = branch_ref.remote_tracking_ref_name(gix::remote::Direction::Fetch)
    else {
        return Ok(None);
    };
    let upstream = upstream_name.shorten().to_str_lossy().into_owned();
    let upstream_full = upstream_name.as_bstr().to_str_lossy().into_owned();

    let upstream_ref = match repo.try_find_reference(upstream_full.as_str())? {
        Some(r) => r,
        None => {
            return Ok(Some(Tracking {
                upstream,
                gone: true,
                ahead: 0,
                behind: 0,
            }));
        }
    };

    let upstream_id = upstream_ref.into_fully_peeled_id()?.detach();
    let local_id = repo.head_id()?.detach();

    Ok(Some(Tracking {
        upstream,
        gone: false,
        ahead: count_commits(repo, local_id, upstream_id)?,
        behind: count_commits(repo, upstream_id, local_id)?,
    }))
}

/// One entry of `format_tracking_info()`'s loop (`remote.c:2400-2459`) — the
/// current branch measured against one of the refs `status.compareBranches`
/// names.
struct Comparison {
    /// `short_ref` — the base ref as the message names it.
    name: String,
    /// `cmp < 0`: `stat_branch_pair()` (`remote.c:2190-2211`) could not read one
    /// of the two refs, which for the upstream entry is the "upstream is gone"
    /// report and for any other entry is silence.
    gone: bool,
    ahead: usize,
    behind: usize,
    /// `is_upstream` (`remote.c:2420`) — this ref *is* `branch_get_upstream()`'s
    /// answer. Gates `ENABLE_ADVICE_PULL` and the divergence hint.
    is_upstream: bool,
    /// `is_push` (`remote.c:2421-2424`) — this ref is `branch_get_push()`'s
    /// answer, or it is the upstream and the branch has no separate push
    /// destination. Gates `ENABLE_ADVICE_PUSH`.
    is_push: bool,
}

/// The value of `status.compareBranches`, split as `format_tracking_info()`
/// splits it (`remote.c:2387-2395`):
///
/// ```c
/// repo_config_get_string(the_repository, "status.comparebranches",
///                        &compare_branches);
///
/// if (compare_branches) {
///         string_list_split(&branches, compare_branches, " ", -1);
///         string_list_remove_empty_items(&branches, 0);
/// } else {
///         string_list_append(&branches, "@{upstream}");
/// }
/// ```
///
/// The delimiter is one literal space, so a tab is *part of* an entry rather
/// than a separator — `status.compareBranches="@{upstream}\t@{push}"` is one
/// unrecognised name, and git warns about it whole. An empty value splits into a
/// single empty item which `string_list_remove_empty_items` drops, leaving no
/// comparisons at all and therefore no tracking block.
///
/// A key set with no value at all is the one shape of this variable not
/// reproduced: `repo_config_get_string()` reports `missing value for
/// 'status.comparebranches'` and dies *after* `On branch <name>` has already
/// reached stdout, because git resolves the value inside the printer. This port
/// builds the whole block before the header is rendered, so it cannot place the
/// diagnostic where git places it; a valueless key is treated as unset, exactly
/// as it was before this key was read at all.
fn compare_branch_names(repo: &gix::Repository) -> Vec<String> {
    let Some(raw) = repo.config_snapshot().string("status.compareBranches") else {
        return vec!["@{upstream}".to_string()];
    };
    raw.to_string()
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `format_tracking_info()`'s loop (`remote.c:2375-2459`) up to the point it
/// starts writing sentences: which refs are compared, in which order, and with
/// which of the three advice flags.
///
/// Resolution is `resolve_compare_branch()` (`remote.c:2291-2312`) — a
/// case-insensitive `@{upstream}` or `@{push}` and nothing else:
///
/// ```c
/// if (!strcasecmp(name, "@{upstream}")) {
///         resolved = branch_get_upstream(branch, NULL);
/// } else if (!strcasecmp(name, "@{push}")) {
///         resolved = branch_get_push(branch, NULL);
/// } else {
///         warning(_("ignoring value '%s' for status.compareBranches, "
///                   "only @{upstream} and @{push} are supported"),
///                 name);
///         return NULL;
/// }
/// ```
///
/// The warning fires per occurrence, before any ref is resolved, so it is
/// printed even for a branch that tracks nothing. A name that resolves to no ref
/// is skipped silently.
///
/// `strset_add(&processed_refs, full_ref)` (`remote.c:2412`) suppresses a repeat
/// of a ref already compared, which is why `@{push} @{upstream} @{push}` shows
/// at most two comparisons and why one shows when the two resolve alike.
fn tracking_comparisons(repo: &gix::Repository) -> Result<Vec<Comparison>> {
    use gix::bstr::ByteSlice;

    let Some(branch_ref) = repo.head_ref()? else {
        return Ok(Vec::new());
    };
    let full = branch_ref.name().as_bstr().to_owned();
    // `upstream_ref` / `push_ref` are this crate's `branch_get_upstream()` and
    // `branch_get_push()`; git reads both once, outside the loop
    // (`remote.c:2397-2398`), because the loop compares against them.
    let upstream = super::branch::upstream_ref(repo, full.as_bstr());
    let push = super::branch::push_ref(repo, full.as_bstr());
    let local = repo.head_id().ok();

    let mut seen: std::collections::HashSet<gix::refs::FullName> =
        std::collections::HashSet::new();
    let mut out = Vec::new();
    for name in compare_branch_names(repo) {
        let resolved = if name.eq_ignore_ascii_case("@{upstream}") {
            upstream.clone()
        } else if name.eq_ignore_ascii_case("@{push}") {
            push.clone()
        } else {
            eprintln!(
                "warning: ignoring value '{name}' for status.compareBranches, \
                 only @{{upstream}} and @{{push}} are supported"
            );
            None
        };
        let Some(full_ref) = resolved else { continue };
        if !seen.insert(full_ref.clone()) {
            continue;
        }

        let is_upstream = upstream.as_ref() == Some(&full_ref);
        // `if (is_upstream && (!push_ref || !strcmp(upstream_ref, push_ref)))
        //          is_push = 1;` — a branch with no distinct push destination
        // still gets the "git push" hint on its upstream comparison. When the
        // two *do* differ, the upstream comparison loses that hint, which is the
        // whole visible effect of `remote.pushDefault` on a default `git status`.
        let is_push = push.as_ref() == Some(&full_ref)
            || (is_upstream && (push.is_none() || push == upstream));
        let counts = super::branch::stat_tracking_info(repo, local, &full_ref);
        out.push(Comparison {
            name: full_ref.shorten().to_str_lossy().into_owned(),
            gone: counts.is_none(),
            ahead: counts.map_or(0, |c| c.0),
            behind: counts.map_or(0, |c| c.1),
            is_upstream,
            is_push,
        });
    }
    Ok(out)
}

/// Build the tracking header line(s) for the long format, matching git's
/// `format_tracking_info` output including advice hints. Empty when there is no
/// upstream configured.
///
/// `hints` is git's `!(s->hints)` argument to `format_tracking_info`
/// (`show_divergence_advice`): with `advice.statusHints=false` the state line
/// stays but the "(use …)" line under it is dropped.
/// `wt_status_print_tracking()`: the `Your branch is …` lines that follow the
/// `On branch <name>` header, plus the blank line git emits under them — and
/// nothing at all when the branch has no upstream, which is why the state block
/// sits flush against the header there.
///
/// Shared with the commands that stop mid-sequence and reprint the header
/// themselves (`cherry-pick`, `revert`); they were rendering the branch line
/// alone, which drops the upstream relation from output git shows. Both reach
/// `format_tracking_info` through `wt_status_print`, whose `s->commit_template`
/// is unset there, so the divergence hint is enabled — as it is for
/// `builtin/checkout.c:941`, which passes the flag literally.
pub(crate) fn tracking_block(repo: &gix::Repository) -> String {
    let quick = repo.config_snapshot().boolean("status.aheadBehind") == Some(false);
    let hints = crate::advice::Advice::StatusHints.enabled_in(repo);
    let comparisons = tracking_comparisons(repo).unwrap_or_default();
    let block = tracking_lines(&comparisons, quick, hints, true);
    if block.is_empty() {
        block
    } else {
        format!("{block}\n")
    }
}

/// The sentences `format_tracking_info()` writes for each surviving comparison,
/// separated by the blank line `remote.c:2444-2445` inserts before every entry
/// after the first one that reported something.
///
/// `divergence` is `show_divergence_advice`, git's fourth argument: `status`
/// passes `!s->commit_template`, `checkout` passes 1.
fn tracking_lines(
    comparisons: &[Comparison],
    quick: bool,
    hints: bool,
    divergence: bool,
) -> String {
    let mut sb = String::new();
    let mut reported = false;
    for c in comparisons {
        // `cmp < 0` (`remote.c:2429-2442`): only the upstream entry has anything
        // to say about a base ref that cannot be read, and it says it without a
        // preceding blank line — the separator below is reached only by entries
        // that got as far as `format_branch_comparison()`.
        if c.gone {
            if c.is_upstream {
                sb.push_str(&format!(
                    "Your branch is based on '{}', but the upstream is gone.\n",
                    c.name
                ));
                if hints {
                    sb.push_str("  (use \"git branch --unset-upstream\" to fixup)\n");
                }
                reported = true;
            }
            continue;
        }
        if reported {
            sb.push('\n');
        }
        reported = true;

        // `format_branch_comparison()` (`remote.c:2314-2370`). Every hint is
        // gated on `advice_enabled(ADVICE_STATUS_HINTS)` *and* on the flag its
        // own branch reads, so a comparison against a ref that is neither the
        // upstream nor the push destination prints its state line bare.
        let advice = |on: bool, line: &str| {
            if on && hints {
                format!("  ({line})\n")
            } else {
                String::new()
            }
        };
        let name = &c.name;
        let (ahead, behind) = (c.ahead, c.behind);
        if ahead == 0 && behind == 0 {
            sb.push_str(&format!("Your branch is up to date with '{name}'.\n"));
        } else if quick {
            // AHEAD_BEHIND_QUICK: git knows the branches differ but not by how much.
            sb.push_str(&format!(
                "Your branch and '{name}' refer to different commits.\n{}",
                advice(c.is_push, "use \"git status --ahead-behind\" for details")
            ));
        } else if behind == 0 {
            let noun = if ahead == 1 { "commit" } else { "commits" };
            sb.push_str(&format!(
                "Your branch is ahead of '{name}' by {ahead} {noun}.\n{}",
                advice(c.is_push, "use \"git push\" to publish your local commits")
            ));
        } else if ahead == 0 {
            let noun = if behind == 1 { "commit" } else { "commits" };
            sb.push_str(&format!(
                "Your branch is behind '{name}' by {behind} {noun}, and can be fast-forwarded.\n{}",
                advice(c.is_upstream, "use \"git pull\" to update your local branch")
            ));
        } else {
            sb.push_str(&format!(
                "Your branch and '{name}' have diverged,\nand have {ahead} and {behind} different commits each, respectively.\n{}",
                advice(
                    divergence && c.is_upstream,
                    "use \"git pull\" if you want to integrate the remote branch with yours"
                )
            ));
        }
    }
    sb
}

/// The `## …` line of `git status -sb`, per git's `wt_shortstatus_print_tracking`.
fn short_branch_header(
    head_state: &HeadState,
    tracking: Option<&Tracking>,
    quick: bool,
    colors: &StatusColors,
) -> String {
    // git wraps the fixed scaffolding (`## `, `...`, the `[ahead …]` labels) in the
    // header slot, the current branch/ahead count in the local-branch slot, and the
    // upstream/behind count in the remote-branch slot.
    let h = |s: &str| colors.paint(Slot::Header, s);
    let mut out = h("## ");
    match head_state {
        HeadState::Detached { .. } => {
            out.push_str(&colors.paint(Slot::Nobranch, "HEAD (no branch)"));
            out.push('\n');
            return out;
        }
        HeadState::Unborn(name) => {
            // An unborn branch has no commits to compare, so git stops at the name.
            out.push_str(&h("No commits yet on "));
            out.push_str(&colors.paint(Slot::LocalBranch, name));
            out.push('\n');
            return out;
        }
        HeadState::Branch(name) => out.push_str(&colors.paint(Slot::LocalBranch, name)),
    }

    let Some(t) = tracking else {
        out.push('\n');
        return out;
    };
    out.push_str(&h("..."));
    out.push_str(&colors.paint(Slot::RemoteBranch, &t.upstream));
    if t.gone {
        out.push_str(&h(" [gone]"));
    } else if quick {
        // AHEAD_BEHIND_QUICK collapses any divergence to `[different]`; an
        // up-to-date branch still prints no bracket at all.
        if t.ahead > 0 || t.behind > 0 {
            out.push_str(&h(" [different]"));
        }
    } else if t.ahead > 0 && t.behind > 0 {
        out.push_str(&h(" [ahead "));
        out.push_str(&colors.paint(Slot::LocalBranch, &t.ahead.to_string()));
        out.push_str(&h(", behind "));
        out.push_str(&colors.paint(Slot::RemoteBranch, &t.behind.to_string()));
        out.push_str(&h("]"));
    } else if t.ahead > 0 {
        out.push_str(&h(" [ahead "));
        out.push_str(&colors.paint(Slot::LocalBranch, &t.ahead.to_string()));
        out.push_str(&h("]"));
    } else if t.behind > 0 {
        out.push_str(&h(" [behind "));
        out.push_str(&colors.paint(Slot::RemoteBranch, &t.behind.to_string()));
        out.push_str(&h("]"));
    }
    out.push('\n');
    out
}

/// Count commits reachable from `tip` but not from `hidden` — i.e. the ahead/
/// behind count, exactly as git derives it from the merge base.
fn count_commits(repo: &gix::Repository, tip: ObjectId, hidden: ObjectId) -> Result<usize> {
    let walk = repo
        .rev_walk(Some(tip))
        .with_hidden(Some(hidden))
        .all()?;
    Ok(walk.take_while(Result::is_ok).count())
}

/// Render one of the two patches `git status -v` appends, by re-executing this
/// binary's own `git diff` with the flags git gives its verbose `rev_info`.
///
/// Going through the real `diff` implementation rather than a second renderer is
/// what keeps the appended patch byte-identical to `git diff` — index-line
/// abbreviation, rename detection, `diff.*` config and the hunk formatting all
/// come from one place. The child runs at the top of the working tree because
/// git's verbose diff is repo-wide and root-relative regardless of where `status`
/// was invoked.
///
/// A child that cannot be spawned (or a bare repository, which has no worktree to
/// diff) contributes no patch, exactly as git's empty diff would.
fn verbose_patch(workdir: Option<&std::path::Path>, args: &[&str]) -> String {
    let (Some(dir), Ok(exe)) = (workdir, crate::hosted::git_exe()) else {
        return String::new();
    };
    let out = std::process::Command::new(exe)
        .current_dir(dir)
        .arg("diff")
        .args(args)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// `wt_longstatus_print_submodule_summary`: run `git submodule summary
/// --cached|--files --for-status --summary-limit <n>` (plus `HEAD` for the
/// staged side) and, when it produced anything, prefix the section header and a
/// blank line. git shells out for this too, so re-executing this binary's own
/// `submodule summary` keeps the body byte-identical rather than forking a
/// second renderer.
fn submodule_summary(
    workdir: Option<&std::path::Path>,
    uncommitted: bool,
    limit: i64,
    reference: Reference,
) -> String {
    let (Some(dir), Ok(exe)) = (workdir, crate::hosted::git_exe()) else {
        return String::new();
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.current_dir(dir)
        .args(["submodule", "summary"])
        .arg(if uncommitted { "--files" } else { "--cached" })
        .arg("--for-status")
        .arg("--summary-limit")
        .arg(limit.to_string());
    if !uncommitted {
        // `s->amend ? "HEAD^" : "HEAD"` (wt-status.c:1046): only `git commit
        // --amend` sets `s->amend`, and it measures the staged side against the
        // commit it is replacing rather than that commit itself.
        cmd.arg(if reference.amend() { "HEAD^" } else { "HEAD" });
    }
    // `capture_command(&sm_summary, &cmd_stdout, 1024)` (wt-status.c:1051) takes
    // only stdout; the child keeps this process's stderr, so a diagnostic it
    // prints — a submodule replaced by a file makes `rev-parse` fail its
    // `chdir` — reaches the user rather than being swallowed here.
    cmd.stderr(std::process::Stdio::inherit());
    let body = match cmd.output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => return String::new(),
    };
    if body.is_empty() {
        return String::new();
    }
    let header = if uncommitted {
        "Submodules changed but not updated:"
    } else {
        "Submodule changes to be committed:"
    };
    format!("{header}\n\n{body}")
}

/// `wt_status_state` (wt-status.h): the operations the long format announces, and
/// the few strings their banners need.
///
/// Detection is `wt_status_get_state()` and its `wt_status_check_rebase()` /
/// `wt_status_check_bisect()` helpers: purely files under `$GIT_DIR`, in the same
/// order, so an `am` that stopped and a rebase that stopped are told apart by
/// `rebase-apply/applying` exactly as git tells them apart.
#[derive(Default)]
struct ProgressState {
    merge: bool,
    am: bool,
    /// `rebase-apply/patch` exists and is empty.
    am_empty_patch: bool,
    rebase: bool,
    rebase_interactive: bool,
    /// `rebase-*/head-name`, as `get_branch()` shortens it.
    branch: Option<String>,
    /// `rebase-*/onto`, likewise.
    onto: Option<String>,
    /// `Some(None)` when the sequencer says "pick" without a `CHERRY_PICK_HEAD`.
    cherry_pick: Option<Option<String>>,
    /// Whether the `CHERRY_PICK_HEAD` ref itself is there, which is the only thing
    /// `sequencer_determine_whence()` looks at.
    cherry_pick_head: bool,
    revert: Option<Option<String>>,
    bisect: bool,
    /// `BISECT_START`, the branch the bisect started from.
    bisecting_from: Option<String>,
}

/// What `die_if_some_operation_in_progress()` (builtin/checkout.c:1602) found: the message
/// `git switch` dies with, or the bisect it only warns about.
pub(super) enum SwitchBlocker {
    Die(String),
    WarnBisecting,
}

/// `die_if_some_operation_in_progress()`, in git's order. `git switch` refuses to move `HEAD`
/// while an operation is unfinished — `git checkout` does not, which is the whole of
/// `opts->can_switch_when_in_progress` (builtin/checkout.c:2116, :2166).
pub(super) fn switch_blocked_by_operation(repo: &gix::Repository) -> Option<SwitchBlocker> {
    let merging = repo.git_dir().join("MERGE_HEAD").exists();
    let state = ProgressState::detect(repo, merging);
    let die = |what: &str, quit: &str| {
        Some(SwitchBlocker::Die(format!(
            "cannot switch branch {what}\nConsider \"git {quit} --quit\" or \"git worktree add\"."
        )))
    };
    if state.merge {
        return die("while merging", "merge");
    }
    if state.am {
        return Some(SwitchBlocker::Die(
            "cannot switch branch in the middle of an am session\n\
             Consider \"git am --quit\" or \"git worktree add\"."
                .to_string(),
        ));
    }
    if state.rebase_interactive || state.rebase {
        return die("while rebasing", "rebase");
    }
    if state.cherry_pick.is_some() {
        return die("while cherry-picking", "cherry-pick");
    }
    if state.revert.is_some() {
        return die("while reverting", "revert");
    }
    state.bisect.then_some(SwitchBlocker::WarnBisecting)
}

impl ProgressState {
    fn detect(repo: &gix::Repository, merging: bool) -> Self {
        let git_dir = repo.git_dir();
        let mut state = ProgressState { merge: merging, ..Default::default() };

        // `wt_status_check_rebase()`: `rebase-apply` is either an `am` session or a
        // patch-based rebase, `rebase-merge` is the sequencer-driven one.
        if git_dir.join("rebase-apply").is_dir() {
            if git_dir.join("rebase-apply/applying").exists() {
                state.am = true;
                state.am_empty_patch = std::fs::metadata(git_dir.join("rebase-apply/patch"))
                    .is_ok_and(|md| md.len() == 0);
            } else {
                state.rebase = true;
                state.branch = read_state_branch(repo, "rebase-apply/head-name");
                state.onto = read_state_branch(repo, "rebase-apply/onto");
            }
        } else if git_dir.join("rebase-merge").is_dir() {
            if git_dir.join("rebase-merge/interactive").exists() {
                state.rebase_interactive = true;
            } else {
                state.rebase = true;
            }
            state.branch = read_state_branch(repo, "rebase-merge/head-name");
            state.onto = read_state_branch(repo, "rebase-merge/onto");
        }

        state.cherry_pick = sequencer_head(repo, "CHERRY_PICK_HEAD");
        state.cherry_pick_head = git_dir.join("CHERRY_PICK_HEAD").exists();
        state.revert = sequencer_head(repo, "REVERT_HEAD");

        // `wt_status_check_bisect()`.
        if git_dir.join("BISECT_LOG").exists() {
            state.bisect = true;
            state.bisecting_from = read_state_branch(repo, "BISECT_START");
        }
        state
    }

    /// `determine_whence()` (builtin/commit.c:198): `s->whence != FROM_COMMIT` leaves
    /// the "use `git restore --staged` to unstage" hint out of the staged and unmerged
    /// headers. Only two things move it off `FROM_COMMIT` — `MERGE_HEAD`, and the
    /// `CHERRY_PICK_HEAD` that `sequencer_determine_whence()` tests. A revert or a
    /// stopped rebase keeps the hint.
    fn suppresses_unstage_hint(&self) -> bool {
        self.merge || self.cherry_pick_head
    }
}

/// `get_branch()` (wt-status.c:1645): the contents of a state file as a name —
/// `refs/heads/x` becomes `x`, another `refs/` name stays whole, a raw object id is
/// abbreviated, and the rebase placeholder `detached HEAD` means "no name".
fn read_state_branch(repo: &gix::Repository, rela: &str) -> Option<String> {
    let text = std::fs::read_to_string(repo.git_dir().join(rela)).ok()?;
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        return None;
    }
    if let Some(name) = text.strip_prefix("refs/heads/") {
        return Some(name.to_string());
    }
    if text.starts_with("refs/") {
        return Some(text.to_string());
    }
    if let Ok(id) = gix::ObjectId::from_hex(text.as_bytes()) {
        return Some(
            repo.find_object(id)
                .ok()
                .map(|obj| obj.id().shorten_or_id().to_string())
                .unwrap_or_else(|| id.to_hex_with_len(7).to_string()),
        );
    }
    if text == "detached HEAD" {
        return None;
    }
    Some(text.to_string())
}

/// `CHERRY_PICK_HEAD`/`REVERT_HEAD` as an abbreviated id, falling back to
/// `sequencer_get_last_command()`: a stopped sequencer whose head ref is already gone
/// still reports the operation, then with no commit to name.
fn sequencer_head(repo: &gix::Repository, name: &str) -> Option<Option<String>> {
    if let Ok(text) = std::fs::read_to_string(repo.git_dir().join(name)) {
        if let Ok(id) = gix::ObjectId::from_hex(text.trim().as_bytes()) {
            return Some(Some(
                repo.find_object(id)
                    .ok()
                    .map(|obj| obj.id().shorten_or_id().to_string())
                    .unwrap_or_else(|| id.to_hex_with_len(7).to_string()),
            ));
        }
    }
    let todo = repo.git_dir().join("sequencer/todo");
    let text = std::fs::read_to_string(todo).ok()?;
    let verb = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .and_then(|line| line.split_whitespace().next())?;
    let is_revert = matches!(verb, "revert");
    let is_pick = matches!(verb, "pick" | "p");
    match name {
        "CHERRY_PICK_HEAD" if is_pick => Some(None),
        "REVERT_HEAD" if is_revert => Some(None),
        _ => None,
    }
}

/// `show_rebase_information()` (wt-status.c:1430): what an interactive rebase has
/// done and has left to do, two lines of each.
///
/// A non-interactive rebase keeps no todo list and contributes nothing here.
fn rebase_information(
    progress: &ProgressState,
    repo: &gix::Repository,
    git_dir: &std::path::Path,
    hints: bool,
    h: &dyn Fn(&str) -> String,
) -> String {
    if !progress.rebase_interactive {
        return String::new();
    }
    const SHOWN: usize = 2;
    let mut out = String::new();
    let done = read_rebase_todolist(repo, git_dir, "rebase-merge/done");
    let todo = read_rebase_todolist(repo, git_dir, "rebase-merge/git-rebase-todo");
    if todo.is_none() {
        out.push_str(&h("git-rebase-todo is missing.\n"));
    }
    let done = done.unwrap_or_default();
    if done.is_empty() {
        out.push_str(&h("No commands done.\n"));
    } else {
        out.push_str(&h(&format!(
            "Last command{} done ({} command{} done):\n",
            if done.len() == 1 { "" } else { "s" },
            done.len(),
            if done.len() == 1 { "" } else { "s" }
        )));
        for line in done.iter().skip(done.len().saturating_sub(SHOWN)) {
            out.push_str(&h(&format!("   {line}\n")));
        }
        if done.len() > SHOWN && hints {
            out.push_str(&h(&format!(
                "  (see more in file {})\n",
                git_dir.join("rebase-merge/done").display()
            )));
        }
    }
    let todo = todo.unwrap_or_default();
    if todo.is_empty() {
        out.push_str(&h("No commands remaining.\n"));
    } else {
        out.push_str(&h(&format!(
            "Next command{} to do ({} remaining command{}):\n",
            if todo.len() == 1 { "" } else { "s" },
            todo.len(),
            if todo.len() == 1 { "" } else { "s" }
        )));
        for line in todo.iter().take(SHOWN) {
            out.push_str(&h(&format!("   {line}\n")));
        }
        if hints {
            out.push_str(&h("  (use \"git rebase --edit-todo\" to view and edit)\n"));
        }
    }
    out
}

/// `read_rebase_todolist()` (wt-status.c:1399): the todo file without its comments
/// and blank lines, each line run through `abbrev_oid_in_line()`. `None` when the
/// file is missing.
fn read_rebase_todolist(
    repo: &gix::Repository,
    git_dir: &std::path::Path,
    rela: &str,
) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(git_dir.join(rela)).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| abbrev_oid_in_line(repo, line))
            .collect(),
    )
}

/// `abbrev_oid_in_line()` (wt-status.c:1376): turn
/// `pick d6a2f0303e897ec2… some message` into `pick d6a2f03 some message`, leaving
/// the commands that carry no object id alone.
fn abbrev_oid_in_line(repo: &gix::Repository, line: &str) -> String {
    if ["exec ", "x ", "label ", "l "]
        .iter()
        .any(|verb| line.starts_with(verb))
    {
        return line.to_string();
    }
    let mut parts = line.splitn(3, ' ');
    let (Some(verb), Some(oid)) = (parts.next(), parts.next()) else {
        return line.to_string();
    };
    let rest = parts.next();
    let Some(short) = gix::ObjectId::from_hex(oid.as_bytes())
        .ok()
        .and_then(|id| repo.find_object(id).ok())
        .map(|obj| obj.id().shorten_or_id().to_string())
    else {
        return line.to_string();
    };
    match rest {
        Some(rest) => format!("{verb} {short} {rest}"),
        None => format!("{verb} {short}"),
    }
}

/// `split_commit_in_progress()` (wt-status.c:1333): a rebase that stopped to edit a
/// commit and has already had part of it committed, which is what tells "splitting"
/// apart from "editing".
///
/// The check only runs on a dirty worktree (git's `s->workdir_dirty`, the same
/// "Changes not staged for commit" set) and a detached `HEAD`; a clean stop at an
/// `edit` is "editing", not "splitting".
fn split_commit_in_progress(
    git_dir: &std::path::Path,
    head_state: &HeadState,
    workdir_dirty: bool,
) -> bool {
    if !workdir_dirty || !matches!(head_state, HeadState::Detached { .. }) {
        return false;
    }
    let line = |rela: &str| -> Option<String> {
        std::fs::read_to_string(git_dir.join(rela))
            .ok()
            .map(|s| s.trim().to_string())
    };
    // Both refs have to resolve; git bails when either read fails.
    let (Some(head), Some(orig_head)) = (line("HEAD"), line("ORIG_HEAD")) else {
        return false;
    };
    let (Some(amend), Some(rebase_orig_head)) = (
        line("rebase-merge/amend"),
        line("rebase-merge/orig-head"),
    ) else {
        return false;
    };
    if amend == rebase_orig_head {
        // The rebase recorded the commit it stopped at; `HEAD` having moved past it
        // means part of that commit is already committed.
        head != amend
    } else {
        // Otherwise the split shows in `ORIG_HEAD` no longer being where the rebase
        // started.
        orig_head != rebase_orig_head
    }
}

/// `wt_status_get_state()`'s sparse-checkout share (wt-status.c:1795).
///
/// `None` when `core.sparseCheckout` is off or the index is empty — git skips the
/// computation rather than divide by zero. `Some(None)` for a sparse index, which
/// git reports without a number. Otherwise `Some(Some(percent))` with git's
/// `100 - (100 * skipped) / total` in integer arithmetic.
fn sparse_checkout_state(repo: &gix::Repository) -> Option<Option<u32>> {
    if !repo
        .config_snapshot()
        .boolean("core.sparseCheckout")
        .unwrap_or(false)
    {
        return None;
    }
    let index = repo.index_or_empty().ok()?;
    let total = index.entries().len();
    if total == 0 {
        return None;
    }
    if index.is_sparse() {
        return Some(None);
    }
    let skipped = index
        .entries()
        .iter()
        .filter(|e| e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE))
        .count();
    Some(Some((100 - (100 * skipped) / total) as u32))
}

#[allow(clippy::too_many_arguments)]
fn render_long(
    head_state: &HeadState,
    tracking: &str,
    // `uf_was_slow`'s verdict: the elapsed seconds when the untracked-file
    // enumeration crossed [`UF_DELAY_WARNING_IN_MS`] with `advice.statusUoption`
    // on, else `None`.
    untracked_slow: Option<f64>,
    hints: bool,
    unborn: bool,
    // `s->state`: which operation is in progress, and what its banner needs.
    progress: &ProgressState,
    // `show_rebase_information()`'s block, rendered by the caller because it reads the
    // todo files and abbreviates their object ids.
    rebase_info: &str,
    // `$GIT_DIR`, for the rebase state files the banners read.
    git_dir: &std::path::Path,
    // `s->state.sparse_checkout_percentage`, as [`sparse_checkout_state`] computed it.
    sparse_checkout: Option<Option<u32>>,
    untracked_mode: Untracked,
    show_ignored: bool,
    staged: &[(StageKind, BString, Option<BString>)],
    unstaged: &[(WorkKind, BString, Option<BString>, SubmoduleState)],
    unmerged: &[(u8, BString)],
    untracked: &[BString],
    ignored: &[BString],
    show_stash: bool,
    stash_count: usize,
    colors: &StatusColors,
    comment_prefix: Option<&str>,
    colopts: u32,
    verbose: u32,
    workdir: Option<&std::path::Path>,
    prefix: Option<&[u8]>,
    // git's `s->submodule_summary` once the `--ignore-submodules=all` gate has
    // been applied: `Some(<summary-limit>)` (`-1` = unlimited) or `None` for off.
    submodule_summary_limit: Option<i64>,
    // `s->reference` / `s->amend`, which change four of the lines below.
    reference: Reference,
    // `s->nowarn`: drop the trailing `no changes added to commit` / `nothing to
    // commit …` summary (wt-status.c:1977-1978). `No changes` under `--amend`
    // survives it, because git tests `s->amend` one branch earlier.
    nowarn: bool,
    // Where the body is going, which decides whether the mid-body flush before a
    // `submodule summary` fork is real — see [`LongSink`].
    sink: LongSink,
) -> String {
    let mut out = String::new();
    // When columns are active, the untracked/ignored path lists are replaced by a
    // sentinel line, laid out through the shared engine, and spliced back in after
    // the comment-prefix pass (git bakes the `#` and color into the column indent,
    // which must not be re-prefixed by `comment_prefix_body`).
    let column_on = super::column::active(colopts);
    let mut blocks: Vec<String> = Vec::new();

    // Everything git writes through `status_printf`/`status_printf_ln` with
    // `color(WT_STATUS_HEADER, s)` — every section title, every `(use "git …")`
    // hint, the leading `\t` of every change line, the in-progress-operation
    // blocks, and the blank line that closes a section — is painted with
    // `color.status.header`. `status_vprintf` colors one line at a time, so a
    // multi-line hint gets one SGR/reset pair per line.
    let h = |text: &str| -> String {
        text.split_inclusive('\n')
            .map(|line| match line.strip_suffix('\n') {
                Some(body) => format!("{}\n", colors.paint(Slot::Header, body)),
                None => colors.paint(Slot::Header, line),
            })
            .collect()
    };
    // git's `wt_longstatus_print_trailer`: an *empty* header-colored line, which
    // still emits the SGR and its reset around nothing.
    let trailer = || format!("{}\n", colors.paint(Slot::Header, ""));
    // The `\t` that `wt_longstatus_print_change_data` writes before a change
    // line is a header write of its own, closed before the per-file slot opens.
    let tab = || colors.paint(Slot::Header, "\t");

    // git's long-format branch header (wt_longstatus_print): a leading empty
    // `header`-slot write, then the prefix — `header` for a real branch, `nobranch`
    // for detached HEAD — and finally the branch name / detached object name in the
    // `branch` slot (`WT_STATUS_ONBRANCH`, config `color.status.branch`).
    match head_state {
        HeadState::Branch(name) | HeadState::Unborn(name) => {
            out.push_str(&colors.paint(Slot::Header, ""));
            out.push_str(&colors.paint(Slot::Header, "On branch "));
            out.push_str(&colors.paint(Slot::Branch, name));
            out.push('\n');
        }
        HeadState::Detached { from, at } => {
            out.push_str(&colors.paint(Slot::Header, ""));
            // A rebase names what it is rebasing onto instead of the detached id:
            // `wt_longstatus_print` prefers `state.onto` whenever a rebase is in
            // progress (wt-status.c:1902).
            let (prefix, name) = if progress.rebase || progress.rebase_interactive {
                let prefix = if progress.rebase_interactive {
                    "interactive rebase in progress; onto "
                } else {
                    "rebase in progress; onto "
                };
                (prefix, progress.onto.as_deref().unwrap_or(""))
            } else {
                match from.as_deref() {
                    // `HEAD detached at ` while `HEAD` still holds what the switch
                    // put there, `HEAD detached from ` once it has moved on
                    // (wt-status.c:1908-1913).
                    Some(name) if *at => ("HEAD detached at ", name),
                    Some(name) => ("HEAD detached from ", name),
                    // git prints the whole sentence as the prefix and an empty
                    // branch name after it (wt-status.c:1914-1917).
                    None => ("Not currently on any branch.", ""),
                }
            };
            out.push_str(&colors.paint(Slot::Nobranch, prefix));
            out.push_str(&colors.paint(Slot::Branch, name));
            out.push('\n');
        }
    }

    // git prints a blank line after the tracking block and after each
    // in-progress-operation block; a plain branch/detached header runs straight
    // into the first section.
    out.push_str(tracking);
    if !tracking.is_empty() {
        out.push('\n');
    }

    // `wt_longstatus_print_state()` (wt-status.c:1863): one banner from the
    // merge/am/rebase/cherry-pick/revert chain, then the bisect one if a bisect is
    // also running, then the sparse-checkout line.
    if progress.merge {
        if progress.rebase_interactive {
            // A conflicted `rebase -i` shows what the todo list is doing before the
            // merge banner, with a plain newline between them.
            out.push_str(rebase_info);
            out.push('\n');
        }
        if unmerged.is_empty() {
            out.push_str(&h("All conflicts fixed but you are still merging.\n"));
            if hints {
                out.push_str(&h("  (use \"git commit\" to conclude merge)\n"));
            }
        } else {
            out.push_str(&h("You have unmerged paths.\n"));
            if hints {
                out.push_str(&h("  (fix conflicts and run \"git commit\")\n"));
                out.push_str(&h("  (use \"git merge --abort\" to abort the merge)\n"));
            }
        }
        out.push_str(&trailer());
    } else if progress.am {
        out.push_str(&h("You are in the middle of an am session.\n"));
        if progress.am_empty_patch {
            out.push_str(&h("The current patch is empty.\n"));
        }
        if hints {
            if !progress.am_empty_patch {
                out.push_str(&h("  (fix conflicts and then run \"git am --continue\")\n"));
            }
            out.push_str(&h("  (use \"git am --skip\" to skip this patch)\n"));
            if progress.am_empty_patch {
                out.push_str(&h(
                    "  (use \"git am --allow-empty\" to record this patch as an empty commit)\n",
                ));
            }
            out.push_str(&h("  (use \"git am --abort\" to restore the original branch)\n"));
        }
        out.push_str(&trailer());
    } else if progress.rebase || progress.rebase_interactive {
        out.push_str(rebase_info);
        // `print_rebase_state()`: named branch when the rebase recorded one.
        let state_line = || match &progress.branch {
            Some(branch) => format!(
                "You are currently rebasing branch '{branch}' on '{}'.\n",
                progress.onto.as_deref().unwrap_or_default()
            ),
            None => "You are currently rebasing.\n".to_string(),
        };
        if !unmerged.is_empty() {
            out.push_str(&h(&state_line()));
            if hints {
                out.push_str(&h("  (fix conflicts and then run \"git rebase --continue\")\n"));
                out.push_str(&h("  (use \"git rebase --skip\" to skip this patch)\n"));
                out.push_str(&h(
                    "  (use \"git rebase --abort\" to check out the original branch)\n",
                ));
            }
        } else if progress.rebase || git_dir.join("MERGE_MSG").exists() {
            out.push_str(&h(&state_line()));
            if hints {
                out.push_str(&h("  (all conflicts fixed: run \"git rebase --continue\")\n"));
            }
        } else if split_commit_in_progress(git_dir, head_state, !unstaged.is_empty()) {
            out.push_str(&h(&match &progress.branch {
                Some(branch) => format!(
                    "You are currently splitting a commit while rebasing branch '{branch}' on '{}'.\n",
                    progress.onto.as_deref().unwrap_or_default()
                ),
                None => "You are currently splitting a commit during a rebase.\n".to_string(),
            }));
            if hints {
                out.push_str(&h(
                    "  (Once your working directory is clean, run \"git rebase --continue\")\n",
                ));
            }
        } else {
            out.push_str(&h(&match &progress.branch {
                Some(branch) => format!(
                    "You are currently editing a commit while rebasing branch '{branch}' on '{}'.\n",
                    progress.onto.as_deref().unwrap_or_default()
                ),
                None => "You are currently editing a commit during a rebase.\n".to_string(),
            }));
            // `if (s->hints && !s->amend)` (wt-status.c:1542): telling someone who
            // is *running* `git commit --amend` to run `git commit --amend` is the
            // one hint git withholds.
            if hints && !reference.amend() {
                out.push_str(&h("  (use \"git commit --amend\" to amend the current commit)\n"));
                out.push_str(&h(
                    "  (use \"git rebase --continue\" once you are satisfied with your changes)\n",
                ));
            }
        }
        out.push_str(&trailer());
    } else if let Some(commit) = &progress.cherry_pick {
        out.push_str(&h(&match commit {
            Some(id) => format!("You are currently cherry-picking commit {id}.\n"),
            None => "Cherry-pick currently in progress.\n".to_string(),
        }));
        if hints {
            if !unmerged.is_empty() {
                out.push_str(&h("  (fix conflicts and run \"git cherry-pick --continue\")\n"));
            } else if commit.is_none() {
                out.push_str(&h("  (run \"git cherry-pick --continue\" to continue)\n"));
            } else {
                out.push_str(&h(
                    "  (all conflicts fixed: run \"git cherry-pick --continue\")\n",
                ));
            }
            out.push_str(&h("  (use \"git cherry-pick --skip\" to skip this patch)\n"));
            out.push_str(&h(
                "  (use \"git cherry-pick --abort\" to cancel the cherry-pick operation)\n",
            ));
        }
        out.push_str(&trailer());
    } else if let Some(commit) = &progress.revert {
        out.push_str(&h(&match commit {
            Some(id) => format!("You are currently reverting commit {id}.\n"),
            None => "Revert currently in progress.\n".to_string(),
        }));
        if hints {
            if !unmerged.is_empty() {
                out.push_str(&h("  (fix conflicts and run \"git revert --continue\")\n"));
            } else if commit.is_none() {
                out.push_str(&h("  (run \"git revert --continue\" to continue)\n"));
            } else {
                out.push_str(&h("  (all conflicts fixed: run \"git revert --continue\")\n"));
            }
            out.push_str(&h("  (use \"git revert --skip\" to skip this patch)\n"));
            out.push_str(&h(
                "  (use \"git revert --abort\" to cancel the revert operation)\n",
            ));
        }
        out.push_str(&trailer());
    }

    // A bisect can run alongside any of the above, so it is its own `if`.
    if progress.bisect {
        out.push_str(&h(&match &progress.bisecting_from {
            Some(branch) => format!("You are currently bisecting, started from branch '{branch}'.\n"),
            None => "You are currently bisecting.\n".to_string(),
        }));
        if hints {
            out.push_str(&h(
                "  (use \"git bisect reset\" to get back to the original branch)\n",
            ));
        }
        out.push_str(&trailer());
    }

    // `show_sparse_checkout_in_use()` (wt-status.c:1626): the last of the state
    // blocks `wt_longstatus_print_state()` writes, so it lands after any in-progress
    // banner and before the initial-commit notice.
    if let Some(percentage) = sparse_checkout {
        match percentage {
            Some(percentage) => out.push_str(&h(&format!(
                "You are in a sparse checkout with {percentage}% of tracked files present.\n"
            ))),
            None => out.push_str(&h("You are in a sparse checkout.\n")),
        }
        out.push_str(&trailer());
    }

    if unborn {
        // `wt_longstatus_print`'s initial-commit block: a header-colored blank
        // line, the notice, then another blank line — all three header writes.
        // `s->commit_template` picks the wording (wt-status.c:1929-1934): `git
        // commit` sets it (builtin/commit.c:1809) and gets "Initial commit", while
        // `git status` leaves it clear and gets "No commits yet".
        out.push_str(&trailer());
        out.push_str(&h(match reference.commit_template() {
            true => "Initial commit\n",
            false => "No commits yet\n",
        }));
        out.push_str(&trailer());
    }

    // `wt_longstatus_print_cached_header()` (wt-status.c:227): the unstage hint
    // names `s->reference` whenever it is not plain `HEAD`, so an `--amend` report
    // says `--source=HEAD^1` — restoring from `HEAD` would put back the very
    // content the amend is replacing.
    let unstage_hint = |out: &mut String| {
        if unborn {
            out.push_str(&h("  (use \"git rm --cached <file>...\" to unstage)\n"));
        } else if reference.spec() == "HEAD" {
            out.push_str(&h("  (use \"git restore --staged <file>...\" to unstage)\n"));
        } else {
            out.push_str(&h(&format!(
                "  (use \"git restore --source={} --staged <file>...\" to unstage)\n",
                reference.spec()
            )));
        }
    };

    if !staged.is_empty() {
        out.push_str(&h("Changes to be committed:\n"));
        // Mid-merge git offers no unstage hint, as `git restore --staged` is not
        // the right advice while `MERGE_HEAD` is around.
        if hints && !progress.suppresses_unstage_hint() {
            unstage_hint(&mut out);
        }
        for (kind, path, orig) in staged {
            let label = stage_label(*kind);
            let body = match orig {
                Some(o) => format!("{label:<12}{} -> {}", quote_path(o, prefix), quote_path(path, prefix)),
                None => format!("{label:<12}{}", quote_path(path, prefix)),
            };
            out.push_str(&format!("{}{}\n", tab(), colors.paint(Slot::Added, &body)));
        }
        out.push_str(&trailer());
    }

    if !unmerged.is_empty() {
        out.push_str(&h("Unmerged paths:\n"));
        if hints {
            // `wt_longstatus_print_unmerged_header()` leads with the same unstage
            // hint the staged section carries, under the same conditions — so a
            // conflict left by something other than a merge in progress (a
            // `stash apply`, say) says how to unstage it.
            if !progress.suppresses_unstage_hint() {
                unstage_hint(&mut out);
            }
            out.push_str(&h(unmerged_hint(unmerged)));
        }
        for (mask, path) in unmerged {
            let label = unmerged_label(*mask);
            let body = format!("{label:<17}{}", quote_path(path, prefix));
            out.push_str(&format!("{}{}\n", tab(), colors.paint(Slot::Unmerged, &body)));
        }
        out.push_str(&trailer());
    }

    if !unstaged.is_empty() {
        let any_deleted = unstaged.iter().any(|(k, ..)| matches!(k, WorkKind::Deleted));
        let add_hint = if any_deleted { "git add/rm" } else { "git add" };
        out.push_str(&h("Changes not staged for commit:\n"));
        if hints {
            out.push_str(&h(&format!(
                "  (use \"{add_hint} <file>...\" to update what will be committed)\n"
            )));
            out.push_str(&h(
                "  (use \"git restore <file>...\" to discard changes in working directory)\n",
            ));
            // `wt_longstatus_print_dirty_header()` (wt-status.c:262), keyed on
            // `d->dirty_submodule` alone — a submodule that merely moved to a new
            // commit does not raise it.
            if unstaged.iter().any(|(_, _, _, sub)| sub.dirty()) {
                out.push_str(&h(
                    "  (commit or discard the untracked or modified content in submodules)\n",
                ));
            }
        }
        for (kind, path, orig, sub) in unstaged {
            let label = work_label(*kind);
            // A worktree rename renders like the staged one:
            // `renamed:    <source> -> <destination>`.
            let body = match orig {
                Some(o) => format!(
                    "{label:<12}{} -> {}",
                    quote_path(o, prefix),
                    quote_path(path, prefix)
                ),
                None => format!("{label:<12}{}", quote_path(path, prefix)),
            };
            // `wt_longstatus_print_change_data()` (wt-status.c:440) writes the
            // parenthesised submodule note in the *header* color, not the change color.
            let extra = submodule_extra(*sub);
            out.push_str(&format!("{}{}", tab(), colors.paint(Slot::Changed, &body)));
            if !extra.is_empty() {
                out.push_str(&h(&extra));
            }
            out.push('\n');
        }
        out.push_str(&trailer());
    }

    // `status.submoduleSummary`, between `wt_longstatus_print_changed` and the
    // untracked listing: the staged side first, then the unstaged one.
    //
    // Each of the two shells out, and `start_command()` runs `fflush(NULL)`
    // before it forks (run-command.c:743) — so everything git has written is on
    // the descriptor before the child can write a word, and a diagnostic the
    // child prints lands *between* the sections rather than ahead of the whole
    // report. This renderer accumulates, so the flush has to be explicit; see
    // [`flush_rendered`].
    if let Some(limit) = submodule_summary_limit {
        flush_rendered(&mut out, comment_prefix, sink);
        out.push_str(&submodule_summary(workdir, false, limit, reference));
        flush_rendered(&mut out, comment_prefix, sink);
        out.push_str(&submodule_summary(workdir, true, limit, reference));
    }

    let committable = !staged.is_empty();

    if untracked_mode == Untracked::No {
        // git only mentions the suppressed listing when the run is committable —
        // otherwise the trailing summary already carries the `-u` hint.
        if committable {
            if hints {
                out.push_str(
                    "Untracked files not listed (use -u option to show untracked files)\n",
                );
            } else {
                out.push_str("Untracked files not listed\n");
            }
        }
    } else {
        if !untracked.is_empty() {
            out.push_str(&h("Untracked files:\n"));
            if hints {
                out.push_str(&h(
                    "  (use \"git add <file>...\" to include in what will be committed)\n",
                ));
            }
            if column_on {
                out.push_str(&format!("\u{1}{}\u{1}\n", blocks.len()));
                blocks.push(status_column_block(
                    colopts,
                    colors,
                    comment_prefix.is_some(),
                    untracked,
                    prefix,
                ));
            } else {
                for path in untracked {
                    out.push_str(&format!(
                        "{}{}\n",
                        tab(),
                        colors.paint(Slot::Untracked, &quote_path(path, prefix))
                    ));
                }
            }
            // `wt_longstatus_print_other` closes with `GIT_COLOR_NORMAL`, not the
            // header slot — this blank line stays unpainted even when
            // `color.status.header` is set.
            out.push('\n');
        }
        if show_ignored && !ignored.is_empty() {
            out.push_str(&h("Ignored files:\n"));
            if hints {
                out.push_str(&h(
                    "  (use \"git add -f <file>...\" to include in what will be committed)\n",
                ));
            }
            // git colors ignored paths with the untracked slot — there is no
            // separate `color.status.ignored`.
            if column_on {
                out.push_str(&format!("\u{1}{}\u{1}\n", blocks.len()));
                blocks.push(status_column_block(
                    colopts,
                    colors,
                    comment_prefix.is_some(),
                    ignored,
                    prefix,
                ));
            } else {
                for path in ignored {
                    out.push_str(&format!(
                        "{}{}\n",
                        tab(),
                        colors.paint(Slot::Untracked, &quote_path(path, prefix))
                    ));
                }
            }
            out.push('\n');
        }
        // `wt_longstatus_print_other`'s epilogue: when enumerating the untracked
        // files was slow enough to notice, say so. Printed with `GIT_COLOR_NORMAL`
        // — no `color.status.header` — but still inside the comment-prefix pass.
        if let Some(seconds) = untracked_slow {
            out.push('\n');
            out.push_str(&format!("It took {seconds:.2} seconds to enumerate untracked files.\n"));
            out.push_str("See 'git help status' for information on how to improve this.\n");
            out.push('\n');
        }
    }

    // `wt_status_print_verbose()`: `-v` appends the staged patch, `-vv` labels it
    // and appends the unstaged one too. It runs *before* the trailing summary and
    // ignores the command line's pathspec entirely — git builds a fresh `rev_info`
    // for it, so `git status -v -- b.txt` still shows a staged `a.txt` (verified
    // against git 2.55.0). Section labels and the rule go through `status_printf`
    // and so pick up `status.displayCommentPrefix`; the patch bodies bypass it,
    // which is what the sentinel/`blocks` splice below reproduces.
    if verbose > 0 {
        // git only overrides the prefixes on the branch that also prints the
        // header, so a `-v` (or a `-vv` with nothing committable) leaves the diff
        // on its configured defaults — `diff.noprefix` / `diff.mnemonicprefix`
        // then apply to it, exactly as they do to `git diff --cached`.
        let labelled = verbose > 1 && committable;
        if labelled {
            out.push_str(&h("Changes to be committed:\n"));
        }
        let mut staged_args: Vec<&str> = vec!["--cached"];
        if labelled {
            staged_args.extend_from_slice(&["--src-prefix=c/", "--dst-prefix=i/"]);
        }
        // `opt.def = s->reference` again (wt-status.c:1173): the verbose patch is
        // measured against the same commit the staged section was, so an `--amend`
        // report diffs the index against `HEAD^1` rather than `HEAD`.
        if !unborn && reference.spec() != "HEAD" {
            staged_args.push(reference.spec());
        }
        out.push_str(&format!("\u{1}{}\u{1}\n", blocks.len()));
        blocks.push(verbose_patch(workdir, &staged_args));

        // git gates the second patch on `wt_status_check_worktree_changes()`,
        // which skips every entry whose worktree status is `UNMERGED` — so a
        // conflicted-but-otherwise-clean tree gets no `Changes not staged`
        // section here, unlike the trailing summary's `workdir_dirty`.
        if verbose > 1 && !unstaged.is_empty() {
            out.push_str(&h(&format!("{}\n", "-".repeat(50))));
            out.push_str(&h("Changes not staged for commit:\n"));
            out.push_str(&format!("\u{1}{}\u{1}\n", blocks.len()));
            blocks.push(verbose_patch(
                workdir,
                &["--src-prefix=i/", "--dst-prefix=w/"],
            ));
        }
    }

    // Trailing summary + stash line — git emits both with plain `fprintf`, never
    // through `status_printf`, so they are NOT comment-prefixed even under
    // `status.displayCommentPrefix`. They are collected into `trailer` and appended
    // raw after the (optionally prefixed) body below.
    let mut trailer = String::new();

    // Trailing summary — omitted entirely when there is anything staged
    // (git's "committable" state), matching stock output.
    // `if (s->amend) status_printf_ln(s, GIT_COLOR_NORMAL, _("No changes"))`
    // (wt-status.c:1974-1976) takes the whole `!committable` branch, so none of the
    // `nothing to commit` wordings below can be reached under `--amend`. Note which
    // writer it uses: `status_printf_ln` goes through `status_vprintf` and so picks
    // up `status.displayCommentPrefix`, while every other summary is a bare
    // `fprintf` that never does — which is why this one line joins the body rather
    // than the raw trailer.
    if !committable && reference.amend() {
        // `GIT_COLOR_NORMAL` is the empty string, so the line carries no SGR pair
        // even when `color.status.header` is set — only the comment prefix.
        out.push_str("No changes\n");
    } else if !committable && !nowarn {
        let workdir_dirty = !unstaged.is_empty() || !unmerged.is_empty();
        // Each summary has a hints-on and a hints-off wording in
        // `wt_longstatus_print`; only the clean-tree line is the same either way.
        let summary = if workdir_dirty {
            if hints {
                "no changes added to commit (use \"git add\" and/or \"git commit -a\")"
            } else {
                "no changes added to commit"
            }
        } else if !untracked.is_empty() {
            if hints {
                "nothing added to commit but untracked files present (use \"git add\" to track)"
            } else {
                "nothing added to commit but untracked files present"
            }
        } else if unborn {
            if hints {
                "nothing to commit (create/copy files and use \"git add\" to track)"
            } else {
                "nothing to commit"
            }
        } else if untracked_mode == Untracked::No {
            if hints {
                "nothing to commit (use -u to show untracked files)"
            } else {
                "nothing to commit"
            }
        } else {
            "nothing to commit, working tree clean"
        };
        trailer.push_str(summary);
        trailer.push('\n');
    }

    // `wt_longstatus_print_stash_summary`: an unconditional trailing line after
    // the summary, emitted only when there is at least one stash entry.
    if show_stash && stash_count > 0 {
        let noun = if stash_count == 1 { "entry" } else { "entries" };
        trailer.push_str(&format!("Your stash currently has {stash_count} {noun}\n"));
    }

    // `status.displayCommentPrefix`: prefix each body line with the comment string
    // (git's `status_vprintf`). The trailer keeps its raw, unprefixed form.
    let mut body = match comment_prefix {
        Some(cs) => comment_prefix_body(&out, cs),
        None => out,
    };
    // Splice the pre-laid-out column blocks over their sentinels. A sentinel line
    // does not start with a tab, so `comment_prefix_body` renders it as `<cs> <s>`;
    // the block itself already carries the correct (possibly `#`/colored) indent.
    for (idx, block) in blocks.iter().enumerate() {
        let key = match comment_prefix {
            Some(cs) => format!("{cs} \u{1}{idx}\u{1}\n"),
            None => format!("\u{1}{idx}\u{1}\n"),
        };
        body = body.replace(&key, block);
    }
    body.push_str(&trailer);
    body
}

/// Where [`render_long`]'s body ends up, which is what decides whether its
/// mid-body flush does anything.
///
/// git writes the long format to `s->fp` a call at a time, so by the time
/// `wt_longstatus_print_submodule_summary` forks, everything above it is already
/// in that stream — `start_command()`'s `fflush(NULL)` (run-command.c:743) makes
/// sure of it even when the stream is a pipe. This renderer builds the body in
/// one string instead, which would put a child's `fatal:` ahead of the entire
/// report rather than in the middle of it.
#[derive(Clone, Copy, PartialEq)]
enum LongSink {
    /// `git status` / `git commit`'s report, which git writes to stdout: hand the
    /// rendered prefix over before forking so the child's stderr interleaves
    /// where git's does.
    Stdout,
    /// The `COMMIT_EDITMSG` block, which git writes to that file rather than
    /// stdout (builtin/commit.c:911): the caller wants the whole body back as a
    /// string, and nothing it holds belongs on stdout at any point.
    Retain,
}

/// git's `fflush(NULL)` before the fork, for a [`LongSink::Stdout`] body: write
/// what is rendered so far and empty the buffer.
///
/// `comment_prefix_body` is line-wise and every flush point sits on a line
/// boundary, so prefixing the halves separately is the same as prefixing the
/// whole — which is also how git does it, one `status_printf` at a time.
fn flush_rendered(out: &mut String, comment_prefix: Option<&str>, sink: LongSink) {
    if sink == LongSink::Retain || out.is_empty() {
        return;
    }
    let text = std::mem::take(out);
    let text = match comment_prefix {
        Some(cs) => comment_prefix_body(&text, cs),
        None => text,
    };
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// The leading SGR sequence git's `color()` would emit for `slot`, recovered from
/// [`StatusColors::paint`] (which wraps text as `<sgr>text<reset>`, or leaves it
/// unchanged for an uncolored slot).
fn slot_sgr(colors: &StatusColors, slot: Slot) -> String {
    match colors.paint(slot, "\u{1}").split_once('\u{1}') {
        Some((sgr, _)) => sgr.to_string(),
        None => String::new(),
    }
}

/// Build the column-laid-out block for one untracked/ignored listing, byte-for-byte
/// as git's `wt_status_print_other` does: the paths are C-quoted cells laid out
/// with padding 1 through the shared engine, the indent is
/// `<header-sgr>[#]\t<untracked-sgr>` (git bakes the comment `#` and colors into the
/// indent), and the row terminator carries git's `GIT_COLOR_RESET` when colored.
fn status_column_block(
    colopts: u32,
    colors: &StatusColors,
    comment: bool,
    paths: &[BString],
    prefix: Option<&[u8]>,
) -> String {
    let header_sgr = slot_sgr(colors, Slot::Header);
    let untracked_sgr = slot_sgr(colors, Slot::Untracked);
    // git gates the reset-terminated newline on `want_color`; the untracked slot is
    // the block's color, so a non-empty slot SGR is the reliable "colored" signal.
    let colored = !untracked_sgr.is_empty();
    let mut indent = header_sgr;
    if comment {
        indent.push('#');
    }
    indent.push('\t');
    indent.push_str(&untracked_sgr);
    let nl = if colored { "\x1b[m\n" } else { "\n" };
    let items: Vec<Vec<u8>> = paths.iter().map(|p| quote_path(p, prefix).into_bytes()).collect();
    let opts = super::column::ColumnOptions {
        width: 0,
        padding: 1,
        indent: Some(indent),
        nl: Some(nl.to_string()),
    };
    String::from_utf8_lossy(&super::column::layout(&items, colopts, &opts)).into_owned()
}

/// Apply git's `status_vprintf` comment-prefix rule to every line of the long-
/// format body: each line is prefixed with the comment string `cs`, then a single
/// space *unless* the line's first byte is a tab (git suppresses the space so the
/// `\t`-indented change entries stay aligned). An empty line becomes the comment
/// string alone (no trailing space). Only the human long format is prefixed — git
/// routes it through `status_printf`, while the trailing summary uses raw
/// `fprintf` and is therefore excluded by the caller.
fn comment_prefix_body(body: &str, cs: &str) -> String {
    let mut out = String::with_capacity(body.len() + body.len() / 8 + cs.len());
    for line in body.split_inclusive('\n') {
        let (content, nl) = match line.strip_suffix('\n') {
            Some(c) => (c, "\n"),
            None => (line, ""),
        };
        out.push_str(cs);
        if !content.is_empty() {
            if !content.starts_with('\t') {
                out.push(' ');
            }
            out.push_str(content);
        }
        out.push_str(nl);
    }
    out
}

/// Count `refs/stash` reflog entries — git's `count_stash_entries`, which drives
/// the `--show-stash` line. Zero when the stash ref (and thus its reflog) is absent.
fn count_stash_entries(repo: &gix::Repository) -> usize {
    let reference = match repo.try_find_reference("refs/stash") {
        Ok(Some(r)) => r,
        _ => return 0,
    };
    let mut platform = reference.log_iter();
    let mut n = 0;
    if let Ok(Some(iter)) = platform.all() {
        for line in iter {
            if line.is_ok() {
                n += 1;
            } else {
                break;
            }
        }
    }
    n
}

fn render_short(
    staged: Vec<(StageKind, BString, Option<BString>)>,
    unstaged: Vec<(WorkKind, BString, Option<BString>, SubmoduleState)>,
    unmerged: Vec<(u8, BString)>,
    untracked: &[BString],
    ignored: &[BString],
    colors: &StatusColors,
    prefix: Option<&[u8]>,
) -> String {
    struct Short {
        x: u8,
        y: u8,
        orig: Option<BString>,
        /// A conflicted path: git colors both status columns together with the
        /// unmerged slot, rather than the index/worktree slots separately.
        unmerged: bool,
    }

    // Merge the change streams per path: X is the staged (index) column, Y the
    // worktree column; a file can carry both (e.g. "MM"). Untracked and ignored
    // entries are *not* merged in — git prints them as separate trailing blocks
    // rather than interleaving them by path.
    let mut map: BTreeMap<BString, Short> = BTreeMap::new();
    for (kind, path, orig) in staged {
        let e = map.entry(path).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
            unmerged: false,
        });
        e.x = stage_char(kind);
        if orig.is_some() {
            e.orig = orig;
        }
    }
    for (kind, path, orig, _) in unstaged {
        let e = map.entry(path).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
            unmerged: false,
        });
        e.y = work_char(kind);
        if orig.is_some() {
            e.orig = orig;
        }
    }
    for (mask, path) in unmerged {
        let (x, y) = unmerged_chars(mask);
        map.insert(
            path,
            Short {
                x,
                y,
                orig: None,
                unmerged: true,
            },
        );
    }

    let mut out = String::new();
    for (path, e) in &map {
        // git colors a conflicted path's two columns together with the unmerged
        // slot; otherwise the index column takes the added slot and the worktree
        // column the changed slot, and a blank column stays an uncolored space.
        let cols = if e.unmerged {
            colors.paint(Slot::Unmerged, &format!("{}{}", e.x as char, e.y as char))
        } else {
            let x = short_col(colors, Slot::Added, e.x);
            let y = short_col(colors, Slot::Changed, e.y);
            format!("{x}{y}")
        };
        match &e.orig {
            Some(o) => {
                out.push_str(&format!("{cols} {} -> {}\n", quote_path(o, prefix), quote_path(path, prefix)))
            }
            None => out.push_str(&format!("{cols} {}\n", quote_path(path, prefix))),
        }
    }
    for path in untracked {
        out.push_str(&format!("{} {}\n", colors.paint(Slot::Untracked, "??"), quote_path(path, prefix)));
    }
    for path in ignored {
        out.push_str(&format!("{} {}\n", colors.paint(Slot::Untracked, "!!"), quote_path(path, prefix)));
    }
    out
}

/// `-z` short / porcelain-v1 body (git's `wt_shortstatus_status` /
/// `wt_shortstatus_other` in null-termination mode): each entry is
/// `XY <path>\0`, a rename is `XY <path>\0<source>\0` with the *current* path
/// first, and untracked / ignored are `?? <path>\0` / `!! <path>\0`. Paths are
/// emitted raw — never C-quoted — and the output is uncolored.
fn render_short_z(
    out: &mut Vec<u8>,
    staged: &[(StageKind, BString, Option<BString>)],
    unstaged: &[(WorkKind, BString, Option<BString>, SubmoduleState)],
    unmerged: &[(u8, BString)],
    untracked: &[BString],
    ignored: &[BString],
) {
    struct Short {
        x: u8,
        y: u8,
        orig: Option<BString>,
    }

    // Merge the change streams per path exactly as `render_short` does: X is the
    // staged (index) column, Y the worktree column, and a conflicted path sets
    // both columns from its stagemask.
    let mut map: BTreeMap<BString, Short> = BTreeMap::new();
    for (kind, path, orig) in staged {
        let e = map.entry(path.clone()).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
        });
        e.x = stage_char(*kind);
        if orig.is_some() {
            e.orig = orig.clone();
        }
    }
    for (kind, path, orig, _) in unstaged {
        let e = map.entry(path.clone()).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
        });
        e.y = work_char(*kind);
        if orig.is_some() {
            e.orig = orig.clone();
        }
    }
    for (mask, path) in unmerged {
        let (x, y) = unmerged_chars(*mask);
        map.insert(path.clone(), Short { x, y, orig: None });
    }

    for (path, e) in &map {
        out.push(e.x);
        out.push(e.y);
        out.push(b' ');
        out.extend_from_slice(path);
        out.push(0);
        if let Some(o) = &e.orig {
            out.extend_from_slice(o);
            out.push(0);
        }
    }
    for path in untracked {
        out.extend_from_slice(b"?? ");
        out.extend_from_slice(path);
        out.push(0);
    }
    for path in ignored {
        out.extend_from_slice(b"!! ");
        out.extend_from_slice(path);
        out.push(0);
    }
}

/// The `## …` line of `git status -sbz` — git's `wt_shortstatus_print_tracking`
/// in null-termination mode: identical text to the non-`-z` header but
/// NUL-terminated and uncolored.
fn short_branch_header_z(
    out: &mut Vec<u8>,
    head_state: &HeadState,
    tracking: Option<&Tracking>,
    quick: bool,
) {
    out.extend_from_slice(b"## ");
    match head_state {
        HeadState::Detached { .. } => {
            out.extend_from_slice(b"HEAD (no branch)");
            out.push(0);
            return;
        }
        HeadState::Unborn(name) => {
            out.extend_from_slice(b"No commits yet on ");
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            return;
        }
        HeadState::Branch(name) => out.extend_from_slice(name.as_bytes()),
    }

    let Some(t) = tracking else {
        out.push(0);
        return;
    };
    out.extend_from_slice(b"...");
    out.extend_from_slice(t.upstream.as_bytes());
    if t.gone {
        out.extend_from_slice(b" [gone]");
    } else if quick {
        if t.ahead > 0 || t.behind > 0 {
            out.extend_from_slice(b" [different]");
        }
    } else if t.ahead > 0 && t.behind > 0 {
        out.extend_from_slice(format!(" [ahead {}, behind {}]", t.ahead, t.behind).as_bytes());
    } else if t.ahead > 0 {
        out.extend_from_slice(format!(" [ahead {}]", t.ahead).as_bytes());
    } else if t.behind > 0 {
        out.extend_from_slice(format!(" [behind {}]", t.behind).as_bytes());
    }
    out.push(0);
}

/// One short-format status column: a blank column is an uncolored space; a set
/// column is the letter painted in `slot`.
fn short_col(colors: &StatusColors, slot: Slot, ch: u8) -> String {
    if ch == b' ' {
        " ".to_string()
    } else {
        colors.paint(slot, &(ch as char).to_string())
    }
}

/// git picks the resolution hint from which conflict flavours are present:
/// pure both-deleted conflicts want `git rm`, mixed delete/modify ones want
/// either, and everything else wants `git add`.
fn unmerged_hint(unmerged: &[(u8, BString)]) -> &'static str {
    let mut both_deleted = false;
    let mut del_mod_conflict = false;
    let mut not_deleted = false;
    for (mask, _) in unmerged {
        match mask {
            1 => both_deleted = true,
            3 | 5 => del_mod_conflict = true,
            _ => not_deleted = true,
        }
    }
    if !both_deleted {
        if del_mod_conflict {
            "  (use \"git add/rm <file>...\" as appropriate to mark resolution)\n"
        } else {
            "  (use \"git add <file>...\" to mark resolution)\n"
        }
    } else if !del_mod_conflict && !not_deleted {
        "  (use \"git rm <file>...\" to mark resolution)\n"
    } else {
        "  (use \"git add/rm <file>...\" as appropriate to mark resolution)\n"
    }
}

/// Long-format label for a conflict stagemask (bit 0 = base, 1 = ours, 2 = theirs).
fn unmerged_label(mask: u8) -> &'static str {
    match mask {
        1 => "both deleted:",
        2 => "added by us:",
        3 => "deleted by them:",
        4 => "added by them:",
        5 => "deleted by us:",
        6 => "both added:",
        _ => "both modified:",
    }
}

/// Short-format two-letter code for a conflict stagemask.
fn unmerged_chars(mask: u8) -> (u8, u8) {
    match mask {
        1 => (b'D', b'D'),
        2 => (b'A', b'U'),
        3 => (b'U', b'D'),
        4 => (b'U', b'A'),
        5 => (b'D', b'U'),
        6 => (b'A', b'A'),
        _ => (b'U', b'U'),
    }
}

fn stage_label(kind: StageKind) -> &'static str {
    match kind {
        StageKind::New => "new file:",
        StageKind::Modified => "modified:",
        StageKind::Deleted => "deleted:",
        StageKind::Renamed => "renamed:",
        StageKind::Copied => "copied:",
        StageKind::TypeChange => "typechange:",
    }
}

fn work_label(kind: WorkKind) -> &'static str {
    match kind {
        // `short_submodule_status()` only ever runs for `STATUS_FORMAT_SHORT`, so the
        // two submodule letters cannot reach the long format; they render as the
        // modification they are if they ever did.
        WorkKind::Modified | WorkKind::SubmoduleDirty | WorkKind::SubmoduleUntracked => "modified:",
        WorkKind::Deleted => "deleted:",
        WorkKind::TypeChange => "typechange:",
        WorkKind::Added => "new file:",
        WorkKind::Renamed => "renamed:",
        WorkKind::Copied => "copied:",
    }
}

/// `wt_longstatus_print_change_data()`'s `extra` for a worktree change
/// (wt-status.c:399-409): the parenthesised, comma-separated list of what the
/// submodule side of the pair carries, in git's own fixed order. Empty for anything
/// that is not a submodule change.
fn submodule_extra(sub: SubmoduleState) -> String {
    let parts = [
        (sub.new_commits, "new commits"),
        (sub.modified, "modified content"),
        (sub.untracked, "untracked content"),
    ];
    let listed: Vec<&str> = parts.iter().filter(|(on, _)| *on).map(|(_, s)| *s).collect();
    if listed.is_empty() {
        return String::new();
    }
    format!(" ({})", listed.join(", "))
}

fn stage_char(kind: StageKind) -> u8 {
    match kind {
        StageKind::New => b'A',
        StageKind::Modified => b'M',
        StageKind::Deleted => b'D',
        StageKind::Renamed => b'R',
        StageKind::Copied => b'C',
        StageKind::TypeChange => b'T',
    }
}

fn work_char(kind: WorkKind) -> u8 {
    match kind {
        WorkKind::Modified => b'M',
        WorkKind::Deleted => b'D',
        WorkKind::TypeChange => b'T',
        WorkKind::Added => b'A',
        WorkKind::Renamed => b'R',
        WorkKind::Copied => b'C',
        WorkKind::SubmoduleDirty => b'm',
        WorkKind::SubmoduleUntracked => b'?',
    }
}

// ---------------------------------------------------------------------------
// rename detection — the `diffcore_rename()` pass wt-status runs twice
// ---------------------------------------------------------------------------

/// The three `diff_options` fields `wt_status_collect_changes_index()` and
/// `wt_status_collect_changes_worktree()` (wt-status.c:596, :660) set before they
/// call `run_diff_index()` / `run_diff_files()`, which is where git's rename
/// detection for `status` lives:
///
/// ```c
/// if (s->detect_rename >= 0) rev.diffopt.detect_rename = s->detect_rename;
/// if (s->rename_limit >= 0)  rev.diffopt.rename_limit  = s->rename_limit;
/// if (s->rename_score >= 0)  rev.diffopt.rename_score  = s->rename_score;
/// ```
///
/// So both halves of the report — staged *and* unstaged — get the same detection,
/// which is why `git status` can print ` R old -> new` for a rename that was never
/// staged (its destination being an intent-to-add entry, the only worktree addition
/// `diff-files` can see).
#[derive(Clone, Copy)]
struct RenameOpts {
    /// `0`, [`diffcore_rename::DETECT_RENAME`] or [`diffcore_rename::DETECT_COPY`].
    detect: u8,
    /// `-M<n>` in `MAX_SCORE` units; `0` means git's 50% default.
    score: u32,
    /// `status.renameLimit` / `diff.renameLimit`, git's `rename_limit`.
    limit: i64,
}

impl RenameOpts {
    /// `--no-renames`, and the resolved value of a falsy `status.renames`.
    fn disabled() -> Self {
        RenameOpts {
            detect: 0,
            score: 0,
            limit: diffcore_rename::DEFAULT_RENAME_LIMIT,
        }
    }

    /// Plain rename detection at git's default similarity — `diff_setup()`'s state
    /// once `diff.renames` has had its say, which is what an unconfigured `status`
    /// runs with.
    fn renames() -> Self {
        RenameOpts {
            detect: diffcore_rename::DETECT_RENAME,
            ..RenameOpts::disabled()
        }
    }

    fn enabled(self) -> bool {
        self.detect != 0
    }
}

/// One side of a `struct diff_filepair`, in the shape [`detect_rewrites`] needs.
#[derive(Clone)]
struct RenameSide {
    path: BString,
    /// The git mode; `0` is `!DIFF_FILE_VALID`, i.e. this side does not exist.
    mode: u32,
    id: ObjectId,
    /// git's `oid_valid`. False for a worktree side, which `diff-files` leaves
    /// unhashed and `diff_populate_filespec()` answers by reading the file.
    id_valid: bool,
}

impl RenameSide {
    /// The absent half of an addition or a deletion: git gives it the *other*
    /// side's path and a zero mode.
    fn absent(path: BString, hash: gix::hash::Kind) -> Self {
        RenameSide {
            path,
            mode: 0,
            id: ObjectId::null(hash),
            id_valid: false,
        }
    }
}

/// An `R`/`C` pair `diffcore_rename()` produced, with the score already in the
/// percentage units `d->rename_score` records (`p->score * 100 / MAX_SCORE`).
struct Rewrite {
    kind: u8,
    score: u32,
    src: RenameSide,
    dst: RenameSide,
}

/// `diff_populate_filespec()` for a status pair: an id-carrying side is an odb
/// lookup, a worktree side is read off disk (a symlink yields its target, which is
/// what git hashes for a `120000` entry).
struct StatusContent<'a> {
    repo: &'a gix::Repository,
    workdir: Option<std::path::PathBuf>,
}

impl StatusContent<'_> {
    fn read_worktree(&self, path: &BString) -> Option<Vec<u8>> {
        let full = self
            .workdir
            .as_ref()?
            .join(gix::path::from_bstr(gix::bstr::BStr::new(path)));
        let md = std::fs::symlink_metadata(&full).ok()?;
        if md.is_symlink() {
            let target = std::fs::read_link(&full).ok()?;
            Some(gix::path::into_bstr(target).into_owned().into())
        } else {
            std::fs::read(&full).ok()
        }
    }
}

impl diffcore_rename::Content for StatusContent<'_> {
    fn size(&mut self, spec: &diffcore_rename::FileSpec) -> Option<u64> {
        if spec.oid_valid {
            // `check_size_only = 1`: the odb header answers without inflating.
            let header = self.repo.find_header(spec.oid).ok()?;
            return (header.kind() == gix::object::Kind::Blob).then(|| header.size());
        }
        self.read_worktree(&spec.path).map(|d| d.len() as u64)
    }

    fn data(&mut self, spec: &diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        if spec.oid_valid {
            if let Ok(obj) = self.repo.find_object(spec.oid) {
                return Some(obj.detach().data);
            }
        }
        self.read_worktree(&spec.path)
    }
}

/// Run `diffcore_rename()` over one of `wt_status`' two queues and return only the
/// rename/copy outcomes — everything else keeps the classification its caller
/// already gave it, exactly as git's `wt_status_collect_*_cb()` reads
/// `p->status` pair by pair.
///
/// The queue must hold *every* pair of that half of the report, not just the
/// additions and deletions: `diffcore_rename()` registers a modified pair as a
/// rename *source* under `-C` (diffcore-rename.c:1478), and the destination limit
/// `too_many_rename_candidates()` enforces counts them all.
fn detect_rewrites(
    repo: &gix::Repository,
    pairs: &[(RenameSide, RenameSide)],
    opts: RenameOpts,
) -> Vec<Rewrite> {
    if !opts.enabled() || pairs.is_empty() {
        return Vec::new();
    }
    let mut q = diffcore_rename::Queue::default();
    for (one, two) in pairs {
        let a = q.add_spec(diffcore_rename::FileSpec::new(
            one.path.clone(),
            one.mode,
            one.id,
            one.id_valid,
        ));
        let b = q.add_spec(diffcore_rename::FileSpec::new(
            two.path.clone(),
            two.mode,
            two.id,
            two.id_valid,
        ));
        q.add_pair(a, b);
    }

    let ropts = diffcore_rename::Options {
        detect_rename: opts.detect,
        rename_score: opts.score,
        rename_limit: opts.limit,
        hash_kind: repo.object_hash(),
        ..diffcore_rename::Options::default()
    };
    let mut content = StatusContent {
        repo,
        workdir: repo.workdir().map(std::path::Path::to_path_buf),
    };
    // `wt_status` never reaches `diff_warn_rename_limit()`: it flushes through
    // `DIFF_FORMAT_CALLBACK`, and the warning is `diff_flush()`'s (diff.c:6875),
    // printed only for the patch/stat formats. A `diff.renameLimit` too small to
    // finish the matrix therefore just yields fewer renames, silently.
    let _ = diffcore_rename::run(&mut q, &ropts, &mut content);
    diffcore_rename::resolve_rename_copy(&mut q);

    let mut out = Vec::new();
    for p in &q.pairs {
        if !matches!(p.status, b'R' | b'C') {
            continue;
        }
        let one = &q.specs[p.one];
        let two = &q.specs[p.two];
        out.push(Rewrite {
            kind: p.status,
            score: diffcore_rename::similarity_index(p.score),
            src: RenameSide {
                path: one.path.clone(),
                mode: one.mode,
                id: one.oid,
                id_valid: one.oid_valid,
            },
            dst: RenameSide {
                path: two.path.clone(),
                mode: two.mode,
                id: two.oid,
                id_valid: two.oid_valid,
            },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::relative_path;

    fn rel(input: &str, prefix: &str) -> String {
        String::from_utf8(relative_path(input.as_bytes(), prefix.as_bytes())).unwrap()
    }

    /// The four shapes `relative_path()` (path.c) distinguishes, spelled with the
    /// repo-relative paths `git status` actually feeds it: a path below the
    /// prefix keeps only its tail, a sibling directory walks up one level per
    /// prefix component, a path at the repository root walks up all of them, and
    /// a prefix that is a *byte* prefix but not a *component* prefix must not be
    /// mistaken for a parent.
    #[test]
    fn relative_path_rebases_against_the_cwd_prefix() {
        assert_eq!(rel("sub/b.txt", "sub/"), "b.txt");
        assert_eq!(rel("sub/deep/c.txt", "sub/"), "deep/c.txt");
        assert_eq!(rel("a.txt", "sub/"), "../a.txt");
        assert_eq!(rel("a.txt", "sub/deep/"), "../../a.txt");
        assert_eq!(rel("sub/b.txt", "sub/deep/"), "../b.txt");
        // "subdir/" is not below "sub/" — the shared `sub` is not a component.
        assert_eq!(rel("subdir/x", "sub/"), "../subdir/x");
        // An empty prefix is git's NULL: the path is returned untouched.
        assert_eq!(rel("sub/b.txt", ""), "sub/b.txt");
        // in == prefix-without-slash is git's `in="/a/b", prefix="/a/b"` arm.
        assert_eq!(rel("sub", "sub/"), "./");
    }
}
