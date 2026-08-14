use anyhow::Result;
use std::collections::BTreeMap;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;

use super::color::{Slot, StatusColors};
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
///   * `git status -s|--short`           — short format.
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
///     comment string (`core.commentString`/`core.commentChar`, default `#`); the
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
    // `None` keeps git's configured default (`status.renames`/`diff.renames`).
    let mut renames: Option<Option<gix::diff::Rewrites>> = None;
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
            "-s" | "--short" => {
                short = true;
                porcelain = false;
                format_explicit = true;
                long_format = false;
            }
            "--porcelain" | "--porcelain=v1" | "--porcelain=1" => {
                short = true;
                porcelain = true;
                format_explicit = true;
                long_format = false;
            }
            "--long" => {
                short = false;
                porcelain = false;
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
            "--no-renames" => renames = Some(None),
            "--renames" | "-M" | "--find-renames" => {
                renames = Some(Some(gix::diff::Rewrites::default()));
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
                    Some(rewrites) => renames = Some(Some(rewrites)),
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
                                Some(rewrites) => renames = Some(Some(rewrites)),
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

    let repo = gix::discover(".")?;
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
        if !branch_explicit && !null_term && snap.boolean("status.branch") == Some(true) {
            branch_header = true;
        }
        // `status.renames` supplies the rename-detection default when the command
        // line carries no `--renames` / `--no-renames` / `-M`. git reads this key
        // in `status_config` — *before* it parses the command line — and dies on a
        // non-boolean value, so an invalid value is fatal even when a flag would
        // otherwise override it; only the resolved value is what a flag supersedes.
        match configured_renames(&snap) {
            Ok(setting) => {
                if renames.is_none() {
                    if let Some(cfg) = setting {
                        renames = Some(cfg);
                    }
                }
            }
            Err(bad) => {
                eprintln!("fatal: bad boolean config value '{bad}' for 'status.renames'");
                return Ok(ExitCode::from(128));
            }
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
        if snap.boolean("status.displayCommentPrefix") == Some(true) {
            comment_prefix = Some(resolve_comment_string(&snap));
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
    // off. The short/porcelain formats carry no hints at all.
    let hints = crate::advice::Advice::StatusHints.enabled_in(&repo);

    // Resolve the head into an owned description so the borrow ends before we
    // re-open references for the tracking computation.
    let head = repo.head()?;
    let unborn = head.is_unborn();
    let head_state = if unborn {
        HeadState::Unborn(referent_short(head.referent_name(), "main"))
    } else if head.is_detached() {
        let short_id = head
            .id()
            .map(|id| id.shorten_or_id().to_string())
            .unwrap_or_default();
        HeadState::Detached(short_id)
    } else {
        HeadState::Branch(referent_short(head.referent_name(), "HEAD"))
    };
    drop(head);

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

    let untracked = untracked_flag.unwrap_or_else(|| configured_untracked(&repo));

    // The porcelain-v2 machine format is a separate renderer with its own,
    // richer per-path fields (HEAD/index/worktree modes + oids); it shares none
    // of the v1/long collection below, so the two cannot regress each other.
    if porcelain_v2 {
        return porcelain_v2_output(
            &repo,
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
    let mut unstaged: Vec<(WorkKind, BString)> = Vec::new();
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
    if let Some(rewrites) = renames {
        platform = platform.tree_index_track_renames(match rewrites {
            Some(r) => gix::status::tree_index::TrackRenames::Given(r),
            None => gix::status::tree_index::TrackRenames::Disabled,
        });
    }
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
                    ChangeRef::Addition { location, .. } => {
                        staged.push((StageKind::New, location.into_owned(), None));
                    }
                    ChangeRef::Deletion { location, .. } => {
                        staged.push((StageKind::Deleted, location.into_owned(), None));
                    }
                    ChangeRef::Modification {
                        location,
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => {
                        let kind = if type_class(previous_entry_mode) != type_class(entry_mode) {
                            StageKind::TypeChange
                        } else {
                            StageKind::Modified
                        };
                        staged.push((kind, location.into_owned(), None));
                    }
                    ChangeRef::Rewrite {
                        source_location,
                        location,
                        copy,
                        ..
                    } => {
                        let kind = if copy {
                            StageKind::Copied
                        } else {
                            StageKind::Renamed
                        };
                        staged.push((kind, location.into_owned(), Some(source_location.into_owned())));
                    }
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::{Change, Conflict, EntryStatus};
                match iw {
                    Item::Modification { rela_path, status, .. } => match status {
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
                        EntryStatus::IntentToAdd => unstaged.push((WorkKind::Added, rela_path)),
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(change) => match change {
                            Change::Removed => unstaged.push((WorkKind::Deleted, rela_path)),
                            Change::Type { .. } => unstaged.push((WorkKind::TypeChange, rela_path)),
                            Change::Modification { .. } | Change::SubmoduleModification(_) => {
                                unstaged.push((WorkKind::Modified, rela_path))
                            }
                        },
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

    let untracked_slow = uf_was_slow(untracked_t0);

    // git orders each section (and each short-format block) by path.
    staged.sort_by(|a, b| a.1.cmp(&b.1));
    unstaged.sort_by(|a, b| a.1.cmp(&b.1));
    unmerged.sort_by(|a, b| a.1.cmp(&b.1));
    untracked_paths.sort();
    ignored_paths.sort();

    // `wt_longstatus_print_tracking` times `format_tracking_info`, whose cost is
    // the ahead/behind revision walk — [`tracking_info`] here.
    let ab_t0 = std::time::Instant::now();
    let tracking = if unborn {
        None
    } else {
        tracking_info(&repo)?
    };
    let ab_elapsed_ms = ab_t0.elapsed().as_millis() as u64;

    // git colors the human formats (long and short display) when `color.status`
    // (or `color.ui`) is on and stdout is a terminal; the porcelain machine format
    // is never colored.
    let colors = super::color::StatusColors::resolve(&repo, porcelain);

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
        let mut tracking_block = tracking_lines(tracking.as_ref(), quick, hints);
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
        print!(
            "{}",
            render_long(
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
            )
        );
    }

    Ok(ExitCode::SUCCESS)
}

/// Resolve `status.renames`, git's `git_config_rename`: an explicit `copies` /
/// `copy` (case-insensitive) enables copy detection, any other value is a
/// boolean — truthy means rename detection, falsy disables it — and a valueless
/// key (`[status]\n\trenames`) is git's NULL value, i.e. plain rename detection.
///
/// The three layers of the return value mirror the caller's `renames` field:
/// `Ok(None)` — the key is unset, leave gitoxide's own default (which, like
/// git's `diff.renames` default, detects renames); `Ok(Some(None))` — disabled;
/// `Ok(Some(Some(rewrites)))` — enabled with those rewrite options. `Err(value)`
/// is a non-boolean value, which git reports as a fatal config error (exit 128).
fn configured_renames(
    snap: &gix::config::Snapshot,
) -> std::result::Result<Option<Option<gix::diff::Rewrites>>, String> {
    use gix::bstr::ByteSlice;
    let Some(value) = snap.string("status.renames") else {
        // No string value: either the key is absent, or it is present but
        // valueless — gitoxide reports the latter as boolean `true`, which git's
        // NULL-value branch treats as plain rename detection.
        return Ok(match snap.boolean("status.renames") {
            Some(true) => Some(Some(gix::diff::Rewrites::default())),
            _ => None,
        });
    };
    let text = value.to_str_lossy();
    if text.eq_ignore_ascii_case("copies") || text.eq_ignore_ascii_case("copy") {
        return Ok(Some(Some(gix::diff::Rewrites {
            copies: Some(gix::diff::rewrites::Copies::default()),
            ..Default::default()
        })));
    }
    // git_config_rename falls through to git_config_bool, which is exactly the
    // `git_parse_maybe_bool` we already port for `--untracked-files`.
    match parse_maybe_bool(&text) {
        Some(true) => Ok(Some(Some(gix::diff::Rewrites::default()))),
        Some(false) => Ok(Some(None)),
        None => Err(text.into_owned()),
    }
}

/// Resolve `status.showUntrackedFiles`, which stands in for an absent
/// `--untracked-files` flag. Anything unrecognised falls back to git's default.
fn configured_untracked(repo: &gix::Repository) -> Untracked {
    let Some(value) = repo.config_snapshot().string("status.showUntrackedFiles") else {
        return Untracked::Normal;
    };
    match value.as_slice() {
        b"no" => Untracked::No,
        b"all" => Untracked::All,
        _ => Untracked::Normal,
    }
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

/// Parse the `<n>` of `-M<n>` / `--find-renames=<n>` into a similarity fraction.
/// git accepts a bare percentage (`-M50`) or a fraction (`-M0.5`).
fn parse_similarity(raw: &str) -> Option<gix::diff::Rewrites> {
    let (body, had_percent) = match raw.strip_suffix('%') {
        Some(body) => (body, true),
        None => (raw, false),
    };
    if body.is_empty() {
        return Some(gix::diff::Rewrites::default());
    }
    let value: f32 = body.parse().ok()?;
    // git reads a bare integer as a percentage (`-M50`) and a decimal as a
    // fraction (`-M0.5`); an explicit `%` always means a percentage.
    let percentage = if had_percent || !body.contains('.') {
        value / 100.0
    } else {
        value
    };
    if !(0.0..=1.0).contains(&percentage) {
        return None;
    }
    Some(gix::diff::Rewrites {
        percentage: Some(percentage),
        ..Default::default()
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
    Detached(String),
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
    /// `(R|C, similarity, source-path)` for a rename/copy — renders a `2` line.
    rename: Option<(u8, u32, BString)>,
}

/// The worktree file's git mode: symlink, executable blob, or plain blob. A
/// missing file yields the plain-blob default (the caller uses 0 for deletions).
fn worktree_mode(repo: &gix::Repository, path: &gix::bstr::BStr) -> u32 {
    let Some(wd) = repo.workdir() else {
        return 0o100644;
    };
    let full = wd.join(gix::path::from_bstr(path));
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.file_type().is_symlink() => 0o120000,
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
        Err(_) => 0o100644,
    }
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
    untracked: Untracked,
    show_ignored: bool,
    ignored_matching: bool,
    renames: Option<Option<gix::diff::Rewrites>>,
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
    };

    let mut platform = repo
        .status(gix::progress::Discard)?
        .index_worktree_options_mut(preload_index_threads(&repo))
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
    if let Some(rewrites) = renames {
        platform = platform.tree_index_track_renames(match rewrites {
            Some(r) => gix::status::tree_index::TrackRenames::Given(r),
            None => gix::status::tree_index::TrackRenames::Disabled,
        });
    }
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
                        let r = recs.entry(location.into_owned()).or_insert_with(new_rec);
                        r.x = b'A';
                        r.m_i = entry_mode.bits();
                        r.h_i = id.into_owned();
                        r.staged = true;
                    }
                    ChangeRef::Deletion {
                        location,
                        entry_mode,
                        id,
                        ..
                    } => {
                        let r = recs.entry(location.into_owned()).or_insert_with(new_rec);
                        r.x = b'D';
                        r.m_h = entry_mode.bits();
                        r.h_h = id.into_owned();
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
                        let r = recs.entry(location.into_owned()).or_insert_with(new_rec);
                        r.x = if type_class(previous_entry_mode) != type_class(entry_mode) {
                            b'T'
                        } else {
                            b'M'
                        };
                        r.m_h = previous_entry_mode.bits();
                        r.h_h = previous_id.into_owned();
                        r.m_i = entry_mode.bits();
                        r.h_i = id.into_owned();
                        r.staged = true;
                    }
                    ChangeRef::Rewrite {
                        source_location,
                        source_entry_mode,
                        source_id,
                        location,
                        entry_mode,
                        id,
                        copy,
                        ..
                    } => {
                        let kind = if copy { b'C' } else { b'R' };
                        let orig = source_location.into_owned();
                        let r = recs.entry(location.into_owned()).or_insert_with(new_rec);
                        r.x = kind;
                        r.m_h = source_entry_mode.bits();
                        r.h_h = source_id.into_owned();
                        r.m_i = entry_mode.bits();
                        r.h_i = id.into_owned();
                        r.staged = true;
                        // Rename detection here is exact-match (100% similarity).
                        r.rename = Some((kind, 100, orig));
                    }
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::{Change, Conflict, EntryStatus};
                match iw {
                    Item::Modification {
                        rela_path, status, ..
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
                        EntryStatus::IntentToAdd => {
                            let r = recs.entry(rela_path).or_insert_with(new_rec);
                            r.y = b'A';
                            r.ita = true;
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(change) => {
                            let r = recs.entry(rela_path).or_insert_with(new_rec);
                            match change {
                                Change::Removed => r.y = b'D',
                                Change::Type { .. } => r.y = b'T',
                                Change::Modification { .. }
                                | Change::SubmoduleModification(_) => r.y = b'M',
                            }
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

    // --------------------------------------- fill from index & worktree stat
    let index = repo.index_or_empty()?;
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
            let mut line: Vec<u8> = Vec::new();
            if let Some((kind, score, ref orig)) = r.rename {
                line.extend_from_slice(
                    format!(
                        "2 {xy} N... {:06o} {:06o} {:06o} {} {} {}{} ",
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
                        "1 {xy} N... {:06o} {:06o} {:06o} {} {} ",
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
            let mut line: Vec<u8> = Vec::new();
            line.extend_from_slice(
                format!(
                    "u {xy} N... {:06o} {:06o} {:06o} {:06o} {} {} {} ",
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
        let line = if let Some((kind, score, ref orig)) = r.rename {
            format!(
                "2 {xy} N... {:06o} {:06o} {:06o} {} {} {}{} {}\t{}",
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
                "1 {xy} N... {:06o} {:06o} {:06o} {} {} {}",
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
        let line = format!(
            "u {xy} N... {:06o} {:06o} {:06o} {:06o} {} {} {} {}",
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
/// format never re-bases paths), then apply the C-style quoting of
/// `core.quotePath=true` — the path is wrapped in double quotes and escaped when
/// it contains control bytes, a quote, a backslash, or any byte >= 0x80;
/// otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>, prefix: Option<&[u8]>) -> String {
    let rebased;
    let bytes = match prefix {
        Some(p) => {
            rebased = relative_path(path.as_ref(), p);
            rebased.as_slice()
        }
        None => path.as_ref(),
    };
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
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
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
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
    move |opts| {
        if !preload {
            opts.thread_limit = Some(1);
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
/// alone, which drops the upstream relation from output git shows.
pub(crate) fn tracking_block(repo: &gix::Repository) -> String {
    let quick = repo.config_snapshot().boolean("status.aheadBehind") == Some(false);
    let hints = crate::advice::Advice::StatusHints.enabled_in(repo);
    let tracking = tracking_info(repo).ok().flatten();
    let block = tracking_lines(tracking.as_ref(), quick, hints);
    if block.is_empty() {
        block
    } else {
        format!("{block}\n")
    }
}

fn tracking_lines(tracking: Option<&Tracking>, quick: bool, hints: bool) -> String {
    let Some(t) = tracking else {
        return String::new();
    };
    // Each state's advice line, appended only when hints are on.
    let advice = |line: &str| if hints { format!("  ({line})\n") } else { String::new() };
    let upstream = &t.upstream;
    if t.gone {
        return format!(
            "Your branch is based on '{upstream}', but the upstream is gone.\n{}",
            advice("use \"git branch --unset-upstream\" to fixup")
        );
    }
    let (ahead, behind) = (t.ahead, t.behind);
    if ahead == 0 && behind == 0 {
        format!("Your branch is up to date with '{upstream}'.\n")
    } else if quick {
        // AHEAD_BEHIND_QUICK: git knows the branches differ but not by how much.
        format!(
            "Your branch and '{upstream}' refer to different commits.\n{}",
            advice("use \"git status --ahead-behind\" for details")
        )
    } else if behind == 0 {
        let noun = if ahead == 1 { "commit" } else { "commits" };
        format!(
            "Your branch is ahead of '{upstream}' by {ahead} {noun}.\n{}",
            advice("use \"git push\" to publish your local commits")
        )
    } else if ahead == 0 {
        let noun = if behind == 1 { "commit" } else { "commits" };
        format!(
            "Your branch is behind '{upstream}' by {behind} {noun}, and can be fast-forwarded.\n{}",
            advice("use \"git pull\" to update your local branch")
        )
    } else {
        format!(
            "Your branch and '{upstream}' have diverged,\nand have {ahead} and {behind} different commits each, respectively.\n{}",
            advice("use \"git pull\" if you want to integrate the remote branch with yours")
        )
    }
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
        HeadState::Detached(_) => {
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
    let (Some(dir), Ok(exe)) = (workdir, std::env::current_exe()) else {
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
fn submodule_summary(workdir: Option<&std::path::Path>, uncommitted: bool, limit: i64) -> String {
    let (Some(dir), Ok(exe)) = (workdir, std::env::current_exe()) else {
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
        // `s->amend ? "HEAD^" : "HEAD"`; `git status` never amends.
        cmd.arg("HEAD");
    }
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

    /// Whether any of these has a banner to print.
    fn any(&self) -> bool {
        self.merge
            || self.am
            || self.rebase
            || self.rebase_interactive
            || self.cherry_pick.is_some()
            || self.revert.is_some()
            || self.bisect
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
    if !workdir_dirty || !matches!(head_state, HeadState::Detached(_)) {
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
    unstaged: &[(WorkKind, BString)],
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
        HeadState::Detached(short) => {
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
                (prefix, progress.onto.as_deref().unwrap_or(short.as_str()))
            } else {
                ("HEAD detached at ", short.as_str())
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
            if hints {
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
        out.push_str(&trailer());
        out.push_str(&h("No commits yet\n"));
        out.push_str(&trailer());
    }

    if !staged.is_empty() {
        out.push_str(&h("Changes to be committed:\n"));
        // Mid-merge git offers no unstage hint, as `git restore --staged` is not
        // the right advice while `MERGE_HEAD` is around.
        if hints && !progress.suppresses_unstage_hint() {
            if unborn {
                out.push_str(&h("  (use \"git rm --cached <file>...\" to unstage)\n"));
            } else {
                out.push_str(&h("  (use \"git restore --staged <file>...\" to unstage)\n"));
            }
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
                if unborn {
                    out.push_str(&h("  (use \"git rm --cached <file>...\" to unstage)\n"));
                } else {
                    out.push_str(&h("  (use \"git restore --staged <file>...\" to unstage)\n"));
                }
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
        let any_deleted = unstaged.iter().any(|(k, _)| matches!(k, WorkKind::Deleted));
        let add_hint = if any_deleted { "git add/rm" } else { "git add" };
        out.push_str(&h("Changes not staged for commit:\n"));
        if hints {
            out.push_str(&h(&format!(
                "  (use \"{add_hint} <file>...\" to update what will be committed)\n"
            )));
            out.push_str(&h(
                "  (use \"git restore <file>...\" to discard changes in working directory)\n",
            ));
        }
        for (kind, path) in unstaged {
            let label = work_label(*kind);
            let body = format!("{label:<12}{}", quote_path(path, prefix));
            out.push_str(&format!("{}{}\n", tab(), colors.paint(Slot::Changed, &body)));
        }
        out.push_str(&trailer());
    }

    // `status.submoduleSummary`, between `wt_longstatus_print_changed` and the
    // untracked listing: the staged side first, then the unstaged one.
    if let Some(limit) = submodule_summary_limit {
        out.push_str(&submodule_summary(workdir, false, limit));
        out.push_str(&submodule_summary(workdir, true, limit));
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
    if !committable {
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

/// Resolve git's comment string for `status.displayCommentPrefix` output, matching
/// git's `comment_line_str` precedence: `core.commentString` wins if set, else
/// `core.commentChar` (a literal `auto` resolves to `#` for status display, as git
/// does), else the built-in default `#`.
fn resolve_comment_string(snap: &gix::config::Snapshot) -> String {
    use gix::bstr::ByteSlice;
    if let Some(v) = snap.string("core.commentString") {
        let s = v.to_str_lossy();
        if !s.is_empty() {
            return s.into_owned();
        }
    }
    if let Some(v) = snap.string("core.commentChar") {
        let s = v.to_str_lossy();
        if !s.is_empty() && s != "auto" {
            return s.into_owned();
        }
    }
    "#".to_string()
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
    unstaged: Vec<(WorkKind, BString)>,
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
    for (kind, path) in unstaged {
        let e = map.entry(path).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
            unmerged: false,
        });
        e.y = work_char(kind);
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
    unstaged: &[(WorkKind, BString)],
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
    for (kind, path) in unstaged {
        let e = map.entry(path.clone()).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
        });
        e.y = work_char(*kind);
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
        HeadState::Detached(_) => {
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
        WorkKind::Modified => "modified:",
        WorkKind::Deleted => "deleted:",
        WorkKind::TypeChange => "typechange:",
        WorkKind::Added => "new file:",
    }
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
    }
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
