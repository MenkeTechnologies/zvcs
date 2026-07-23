//! `git p4` — import from and submit to Perforce depots. **The Perforce half is
//! not ported**; only the parts of the command that never touch a p4 server are
//! reproduced here.
//!
//! Stock `git-p4` is a Python script (`git-p4.py`) whose entire working half
//! drives the external `p4` client: it shells out to `p4 changes`, `p4 describe`,
//! `p4 print`, `p4 sync`, `p4 submit`, `p4 login -s` and friends, and parses the
//! marshalled Python dictionaries `p4 -G` writes back. There is no Perforce
//! client, no Perforce wire protocol and no depot reader anywhere in the vendored
//! gitoxide crates under `src/ported` — and none could be inferred, because the
//! depot lives on a remote server this process cannot reach. `sync`, `clone`,
//! `rebase`, `submit`, `commit` and the import half of `unshelve` are therefore
//! rejected outright rather than approximated: their whole observable result is
//! the objects and refs they write from depot content, which is exactly the
//! post-command state a differential harness inspects.
//!
//! One subcommand — `branches` — never touches p4 at all. It is a pure git
//! query (plus one ref-writing step), and it is ported in full.
//!
//! ### Covered (byte-identical stdout/stderr and exit code against git 2.55.0)
//!
//! * `main()`'s dispatch: no arguments at all prints [`print_usage`] on stdout
//!   and exits 2; an unrecognized subcommand prints `unknown command <name>`,
//!   a blank line, then the same usage block, and exits 2. Both banners echo
//!   the script's `sys.argv[0]`, which git sets to `$GIT_EXEC_PATH/git-p4` —
//!   see [`prog_path`] for how that is recovered.
//! * The `optparse` option scan for `sync`, `rebase`, `clone`, `branches` and
//!   `unshelve`: exact and unique-prefix long-option matching, `--opt=value`
//!   and `--opt value`, bundled and attached short options (including the `-/`
//!   option `sync`/`clone` declare), `--` as a terminator, and the four error
//!   shapes `no such option: %s`, `ambiguous option: %s (%s?)`,
//!   `%s option requires 1 argument` and `%s option does not take a value`.
//! * `-h`/`--help` for those five subcommands: the `IndentedHelpFormatter`
//!   layout is recomputed here rather than hardcoded, so the help column and
//!   the help-text wrapping track `$COLUMNS` exactly as optparse does. The
//!   block is printed *twice* on stdout — optparse's `-h` handler prints it and
//!   raises `SystemExit`, which `main()`'s bare `except:` catches, prints the
//!   help again, and re-raises. Exit 0. On an option error the block goes to
//!   stdout once and `Usage: …` plus the error line go to stderr; exit 2.
//! * `main()`'s `needsGit` repository location: `--git-dir`/`$GIT_DIR` when
//!   given (falling back to `<dir>/.git`, else `fatal: cannot locate git
//!   repository at <dir>` on stderr, exit 1), otherwise `./.git` and then
//!   upward discovery.
//! * `P4Branches.run()` in full: `originP4BranchesExist()`,
//!   `createOrUpdateBranchesFromOrigin()` including its ref updates and its two
//!   `print` diagnostics, the `refs/remotes/` walk in refname order with the
//!   `p4/` filter and the `p4/HEAD` exclusion, `extractLogMessageFromGitCommit`,
//!   `extractSettingsGitLog`'s `[git-p4: …]` parser, and the
//!   `<branch> <= <depot-paths> (<change>)` line.
//! * `P4Unshelve.run()`'s pre-flight: a changelist count other than one prints
//!   the help block on stdout and exits 2; an `--origin` that names no existing
//!   ref prints `origin branch <name> does not exist` on stderr and exits 1.
//!
//! ### Not covered
//!
//! * `submit` and `commit` — `P4Submit.__init__` calls `p4_has_move_command()`,
//!   which spawns `p4` before any argument is looked at, so stock git dies with
//!   a Python `FileNotFoundError` traceback naming interpreter paths and script
//!   line numbers. Neither the traceback nor the p4 probe is reproducible;
//!   both spellings bail.
//! * The import/export bodies of `sync`, `clone`, `rebase` and `unshelve`.
//! * `-v`/`--verbose`, which makes the script trace every subprocess it spawns
//!   (`Reading pipe: git cat-file commit …`) to stderr and turns `die()` into a
//!   raised exception. Those lines describe `git` invocations this module does
//!   not make, so the flag bails rather than emitting a plausible-looking trace.
//! * The discovery-failure message quotes git's own `rev-parse` stderr; only
//!   its ordinary `not a git repository` form is reproduced, not variants such
//!   as `detected dubious ownership`.
//! * A `p4/` branch whose tip carries no `[git-p4: …]` metadata makes stock git
//!   raise `KeyError` and print a traceback; this bails instead.
//! * Help-text wrapping uses whitespace breaks only. Stock optparse hands the
//!   text to `textwrap`, which would additionally break on hyphens — no help
//!   string in git-p4 contains one, so the two agree today.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// One `optparse.make_option` declaration.
///
/// `metavar` doubles as the "takes a value" flag: optparse derives the metavar
/// from `dest.upper()` when none is given, and only value-taking options ever
/// display one.
struct OptDef {
    shorts: &'static [&'static str],
    longs: &'static [&'static str],
    metavar: Option<&'static str>,
    help: Option<&'static str>,
}

/// A subcommand: its `usage`/`description` strings and its `needsGit` flag.
struct CommandDef {
    name: &'static str,
    /// Everything after `%prog <cmd> ` in `self.usage`.
    usage_tail: &'static str,
    description: &'static str,
    needs_git: bool,
}

/// `commands` in declaration order — the order `printUsage` joins them in.
const COMMANDS: &[&str] = &[
    "submit", "commit", "sync", "rebase", "clone", "branches", "unshelve",
];

/// `P4Sync.description`. The `\n` after the first sentence is an escape inside
/// the triple-quoted literal, so it stacks with the source newline.
const SYNC_DESCRIPTION: &str = "Imports from Perforce into a git repository.\n
    example:
    //depot/my/project/ -- to import the current head
    //depot/my/project/@all -- to import everything
    //depot/my/project/@1,6 -- to import only from revision 1 to 6

    (a ... is not needed in the path p4 specification, it's added implicitly)";

/// `P4Sync.options`, which `P4Clone` inherits and extends.
const SYNC_OPTIONS: &[OptDef] = &[
    OptDef { shorts: &[], longs: &["--branch"], metavar: Some("BRANCH"), help: None },
    OptDef { shorts: &[], longs: &["--detect-branches"], metavar: None, help: None },
    OptDef { shorts: &[], longs: &["--changesfile"], metavar: Some("CHANGESFILE"), help: None },
    OptDef { shorts: &[], longs: &["--silent"], metavar: None, help: None },
    OptDef { shorts: &[], longs: &["--detect-labels"], metavar: None, help: None },
    OptDef { shorts: &[], longs: &["--import-labels"], metavar: None, help: None },
    OptDef {
        shorts: &[],
        longs: &["--import-local"],
        metavar: None,
        help: Some("Import into refs/heads/ , not refs/remotes"),
    },
    OptDef {
        shorts: &[],
        longs: &["--max-changes"],
        metavar: Some("MAXCHANGES"),
        help: Some("Maximum number of changes to import"),
    },
    OptDef {
        shorts: &[],
        longs: &["--changes-block-size"],
        metavar: Some("CHANGES_BLOCK_SIZE"),
        help: Some("Internal block size to use when iteratively calling p4 changes"),
    },
    OptDef {
        shorts: &[],
        longs: &["--keep-path"],
        metavar: None,
        help: Some("Keep entire BRANCH/DIR/SUBDIR prefix during import"),
    },
    OptDef {
        shorts: &[],
        longs: &["--use-client-spec"],
        metavar: None,
        help: Some("Only sync files that are included in the Perforce Client Spec"),
    },
    OptDef {
        shorts: &["-/"],
        longs: &[],
        metavar: Some("CLONEEXCLUDE"),
        help: Some("exclude depot path"),
    },
];

/// `P4Clone.options += [...]`.
const CLONE_EXTRA_OPTIONS: &[OptDef] = &[
    OptDef {
        shorts: &[],
        longs: &["--destination"],
        metavar: Some("CLONEDESTINATION"),
        help: Some("where to leave result of the clone"),
    },
    OptDef { shorts: &[], longs: &["--bare"], metavar: None, help: None },
];

/// `P4Rebase.options`.
const REBASE_OPTIONS: &[OptDef] =
    &[OptDef { shorts: &[], longs: &["--import-labels"], metavar: None, help: None }];

/// `P4Unshelve.options`. The default in the help text is `self.origin`, which
/// the constructor has already set to `HEAD`.
const UNSHELVE_OPTIONS: &[OptDef] = &[OptDef {
    shorts: &[],
    longs: &["--origin"],
    metavar: Some("ORIGIN"),
    help: Some("Use this base revision instead of the default (HEAD)"),
}];

/// `--verbose`, appended to every command's option list by `main()`.
static VERBOSE_OPTION: OptDef =
    OptDef { shorts: &["-v"], longs: &["--verbose"], metavar: None, help: None };

/// `--git-dir`, appended by `main()` only when the command sets `needsGit`.
static GIT_DIR_OPTION: OptDef =
    OptDef { shorts: &[], longs: &["--git-dir"], metavar: Some("GITDIR"), help: None };

/// optparse's own `-h`, added last by `OptionParser._populate_option_list`.
static HELP_OPTION: OptDef = OptDef {
    shorts: &["-h"],
    longs: &["--help"],
    metavar: None,
    help: Some("show this help message and exit"),
};

/// `git p4` — dispatch a Perforce subcommand.
///
/// Only `branches` runs to completion; every other subcommand reaches the point
/// where stock git would invoke the `p4` client and bails there. See the module
/// documentation for the exact division.
pub fn p4(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand name at index 0 so both calling conventions work.
    let args = match args.first() {
        Some(a) if a == "p4" => &args[1..],
        _ => args,
    };

    // `if len(sys.argv[1:]) == 0: printUsage(...); sys.exit(2)`.
    let Some(cmd_name) = args.first() else {
        print!("{}", usage_block()?);
        return Ok(ExitCode::from(2));
    };

    let Some(cmd) = command_def(cmd_name) else {
        // `except KeyError:` — the message, a blank line, then the usage block.
        println!("unknown command {cmd_name}");
        println!();
        print!("{}", usage_block()?);
        return Ok(ExitCode::from(2));
    };

    if matches!(cmd.name, "submit" | "commit") {
        bail!(
            "unsupported subcommand {cmd_name:?}: P4Submit's constructor probes the external `p4` \
             client for its `move` command before parsing any argument, and no Perforce client \
             exists in the vendored gitoxide crates (ported: branches, and the option/help \
             handling of sync, rebase, clone, unshelve)"
        );
    }

    let opts = option_list(&cmd);
    let parsed = match parse_options(&opts, &args[1..]) {
        Ok(p) => p,
        Err(err) => {
            // optparse's `error()` writes to stderr and exits 2; `main()`'s bare
            // `except:` first prints the help block to stdout.
            print!("{}", format_help(&cmd, &opts)?);
            eprintln!("{}", usage_line(&cmd));
            eprintln!("git-p4: error: {err}");
            return Ok(ExitCode::from(2));
        }
    };

    if parsed.help {
        // Printed once by optparse's handler, once by the `except:` in `main()`.
        let help = format_help(&cmd, &opts)?;
        print!("{help}{help}");
        return Ok(ExitCode::SUCCESS);
    }

    if parsed.verbose {
        bail!(
            "unsupported flag \"--verbose\": it traces every `git`/`p4` subprocess git-p4 spawns \
             to stderr, and this port issues no subprocesses to trace"
        );
    }

    // `main()`'s needsGit block. `clone` clears the flag, so it never runs.
    let repo = if cmd.needs_git {
        let explicit = parsed
            .git_dir
            .clone()
            .or_else(|| std::env::var("GIT_DIR").ok());
        match locate_repo(explicit.as_deref()) {
            Ok(repo) => Some(repo),
            Err(code) => return Ok(code),
        }
    } else {
        None
    };

    match cmd.name {
        "branches" => {
            let repo = repo.expect("branches sets needsGit");
            run_branches(&repo)
        }
        "unshelve" => {
            let repo = repo.expect("unshelve sets needsGit");
            run_unshelve_preflight(&repo, &cmd, &opts, &parsed)
        }
        _ => bail!(
            "unsupported subcommand {cmd_name:?}: it reads depot content through the external `p4` \
             client, for which there is no substrate in the vendored gitoxide crates \
             (ported: branches, and the option/help handling of sync, rebase, clone, unshelve)"
        ),
    }
}

/// The `commands` table lookup, plus each class's `usage`/`description`/
/// `needsGit`. `submit` and `commit` name the same class.
fn command_def(name: &str) -> Option<CommandDef> {
    let (usage_tail, description, needs_git) = match name {
        "submit" | "commit" => ("[options]", "", true),
        "sync" => ("[options] //depot/path[@revRange]", SYNC_DESCRIPTION, true),
        "rebase" => (
            "[options]",
            "Fetches the latest revision from perforce and rebases the current work (branch) \
             against it",
            true,
        ),
        "clone" => (
            "[options] //depot/path[@revRange]",
            "Creates a new git repository and imports from Perforce into it",
            false,
        ),
        "branches" => (
            "[options]",
            "Shows the git branches that hold imports and their corresponding perforce depot paths",
            true,
        ),
        "unshelve" => (
            "[options] changelist",
            "Unshelve a P4 changelist into a git commit",
            true,
        ),
        _ => return None,
    };
    Some(CommandDef {
        // Borrow the caller's spelling only for known names, so the static
        // lifetime holds.
        name: COMMANDS.iter().copied().find(|c| *c == name)?,
        usage_tail,
        description,
        needs_git,
    })
}

/// The full option list optparse sees: the command's own options, then
/// `--verbose`, then `--git-dir` when `needsGit`, then `-h`.
fn option_list(cmd: &CommandDef) -> Vec<&'static OptDef> {
    let mut opts: Vec<&'static OptDef> = match cmd.name {
        "sync" => SYNC_OPTIONS.iter().collect(),
        "clone" => SYNC_OPTIONS.iter().chain(CLONE_EXTRA_OPTIONS).collect(),
        "rebase" => REBASE_OPTIONS.iter().collect(),
        "unshelve" => UNSHELVE_OPTIONS.iter().collect(),
        _ => Vec::new(),
    };
    opts.push(&VERBOSE_OPTION);
    if cmd.needs_git {
        opts.push(&GIT_DIR_OPTION);
    }
    opts.push(&HELP_OPTION);
    opts
}

/// `printUsage(commands.keys())`, rendered into a string.
fn usage_block() -> Result<String> {
    let prog = prog_path()?;
    Ok(format!(
        "usage: {prog} <command> [options]\n\nvalid commands: {}\n\nTry {prog} <command> --help for \
         command specific help.\n\n",
        COMMANDS.join(", ")
    ))
}

/// `sys.argv[0]` as git sets it when it execs the script: the absolute path
/// `<exec-path>/git-p4`.
///
/// git exports `GIT_EXEC_PATH` to every subcommand it runs, so that is where the
/// value comes from. It is not derivable from gitoxide — the exec path is baked
/// into the git binary — so an unset variable is an error rather than a guess,
/// which would put a wrong path in the banner.
fn prog_path() -> Result<String> {
    match std::env::var("GIT_EXEC_PATH") {
        Ok(dir) if !dir.is_empty() => Ok(format!("{dir}/git-p4")),
        _ => bail!(
            "cannot render the git-p4 usage banner: it echoes the script's own path \
             ($GIT_EXEC_PATH/git-p4) and GIT_EXEC_PATH is unset"
        ),
    }
}

// ---------------------------------------------------------------------------
// optparse replica
// ---------------------------------------------------------------------------

/// What the option scan extracted. Values other than these are consumed and
/// discarded: every code path that would read them bails.
#[derive(Default)]
struct Parsed {
    help: bool,
    verbose: bool,
    git_dir: Option<String>,
    origin: Option<String>,
    positional: Vec<String>,
}

/// `OptionParser.parse_args`, restricted to the option shapes git-p4 declares
/// (every option is `store`/`store_true`/`store_false` with `nargs == 1`).
///
/// Arguments are processed left to right and the first error aborts, so a `-h`
/// earlier in the line wins over a later bad option and vice versa — matching
/// optparse, which acts on each option as it is consumed.
fn parse_options(opts: &[&OptDef], args: &[String]) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;

        if arg == "--" {
            out.positional.extend(args[i..].iter().cloned());
            break;
        }

        if let Some(body) = arg.strip_prefix("--") {
            let (spelled, attached) = match body.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (format!("--{body}"), None),
            };
            let (def, canonical) = match_long(opts, &spelled)?;
            let value = if def.metavar.is_some() {
                match attached.or_else(|| {
                    let v = args.get(i).cloned();
                    if v.is_some() {
                        i += 1;
                    }
                    v
                }) {
                    Some(v) => Some(v),
                    None => return Err(format!("{canonical} option requires 1 argument")),
                }
            } else {
                if attached.is_some() {
                    return Err(format!("{canonical} option does not take a value"));
                }
                None
            };
            if store(&mut out, canonical, value) {
                return Ok(out);
            }
            continue;
        }

        // A lone `-` and anything not starting with `-` are positionals.
        let Some(bundle) = arg.strip_prefix('-').filter(|b| !b.is_empty()) else {
            out.positional.push(arg.to_string());
            continue;
        };

        let mut rest = bundle;
        while let Some(c) = rest.chars().next() {
            rest = &rest[c.len_utf8()..];
            let spelled = format!("-{c}");
            let Some(def) = opts
                .iter()
                .find(|o| o.shorts.contains(&spelled.as_str()))
            else {
                return Err(format!("no such option: {spelled}"));
            };
            if def.metavar.is_none() {
                if store(&mut out, &spelled, None) {
                    return Ok(out);
                }
                continue;
            }
            // The remainder of the bundle is the value; an empty remainder
            // consumes the next argument.
            let value = if rest.is_empty() {
                let v = args.get(i).cloned();
                if v.is_some() {
                    i += 1;
                }
                v
            } else {
                Some(std::mem::take(&mut rest).to_string())
            };
            match value {
                Some(v) => {
                    if store(&mut out, &spelled, Some(v)) {
                        return Ok(out);
                    }
                }
                None => return Err(format!("{spelled} option requires 1 argument")),
            }
            break;
        }
    }

    Ok(out)
}

/// `_match_long_opt`: an exact hit, else a unique prefix. Ambiguous candidates
/// are reported sorted, as `_match_abbrev` sorts them before joining.
fn match_long<'a>(
    opts: &[&'a OptDef],
    spelled: &str,
) -> Result<(&'a OptDef, &'static str), String> {
    for def in opts {
        if let Some(name) = def.longs.iter().find(|l| **l == spelled) {
            return Ok((*def, *name));
        }
    }
    let mut hits: Vec<(&'a OptDef, &'static str)> = Vec::new();
    for def in opts {
        for name in def.longs {
            if name.starts_with(spelled) {
                hits.push((*def, *name));
            }
        }
    }
    match hits.len() {
        0 => Err(format!("no such option: {spelled}")),
        1 => Ok(hits[0]),
        _ => {
            let mut names: Vec<&str> = hits.iter().map(|(_, n)| *n).collect();
            names.sort_unstable();
            Err(format!(
                "ambiguous option: {spelled} ({}?)",
                names.join(", ")
            ))
        }
    }
}

/// Apply one parsed option. Returns `true` when `-h` was seen, which optparse
/// acts on immediately by printing help and exiting.
fn store(out: &mut Parsed, canonical: &str, value: Option<String>) -> bool {
    match canonical {
        "-h" | "--help" => {
            out.help = true;
            return true;
        }
        "-v" | "--verbose" => out.verbose = true,
        "--git-dir" => out.git_dir = value,
        "--origin" => out.origin = value,
        // Every other option feeds a code path that bails before reading it.
        _ => {}
    }
    false
}

// ---------------------------------------------------------------------------
// optparse.IndentedHelpFormatter replica
// ---------------------------------------------------------------------------

/// `HelpFormatter.__init__`'s width: `$COLUMNS` (default 80) less 2.
fn help_width_total() -> usize {
    let columns = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(80);
    columns.saturating_sub(2)
}

/// `format_option_strings`: value-taking options display their metavar, short
/// options are listed before long ones (`short_first` defaults to 1).
fn option_strings(def: &OptDef) -> String {
    let mut parts: Vec<String> = Vec::new();
    match def.metavar {
        Some(mv) => {
            parts.extend(def.shorts.iter().map(|s| format!("{s} {mv}")));
            parts.extend(def.longs.iter().map(|l| format!("{l}={mv}")));
        }
        None => {
            parts.extend(def.shorts.iter().map(|s| (*s).to_string()));
            parts.extend(def.longs.iter().map(|l| (*l).to_string()));
        }
    }
    parts.join(", ")
}

/// `OptionParser.format_help()` under git-p4's `HelpFormatter`, whose
/// `format_description` override returns the description verbatim (no wrapping),
/// which is why `sync`'s multi-line description keeps its own indentation.
fn format_help(cmd: &CommandDef, opts: &[&OptDef]) -> Result<String> {
    let width = help_width_total();
    // `store_option_strings`: the widest entry, measured at indent level 1.
    let rendered: Vec<String> = opts.iter().map(|o| option_strings(o)).collect();
    let max_len = rendered.iter().map(|s| s.chars().count() + 2).max().unwrap_or(0);
    let max_help_position = 24usize.min(width.saturating_sub(20).max(4));
    let help_position = (max_len + 2).min(max_help_position);
    let help_width = width.saturating_sub(help_position).max(11);
    let opt_width = help_position.saturating_sub(4);

    let mut out = String::new();
    // `usage_line` already ends in a newline; `format_help` adds the blank line.
    out.push_str(&usage_line(cmd));
    out.push('\n');
    if !cmd.description.is_empty() {
        out.push_str(cmd.description);
        out.push_str("\n\n");
    }
    out.push_str("Options:\n");

    for (def, strings) in opts.iter().zip(&rendered) {
        let len = strings.chars().count();
        let (head, first_indent) = if len > opt_width {
            (format!("  {strings}\n"), help_position)
        } else {
            let pad = opt_width - len;
            (
                format!("  {strings}{}  ", " ".repeat(pad)),
                0,
            )
        };
        out.push_str(&head);
        match def.help {
            None => {
                // No help text: optparse emits the option column alone, keeping
                // its trailing padding.
                if first_indent == 0 {
                    out.push('\n');
                }
            }
            Some(text) => {
                let lines = wrap(text, help_width);
                for (n, line) in lines.iter().enumerate() {
                    if n == 0 {
                        out.push_str(&" ".repeat(first_indent));
                    } else {
                        out.push_str(&" ".repeat(help_position));
                    }
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }

    Ok(out)
}

/// `formatter.format_usage()`: optparse strips the leading `usage: ` from the
/// string the caller supplied and expands `%prog` to the *basename* of
/// `sys.argv[0]`, which is why this line is installation-independent while
/// [`usage_block`] is not.
fn usage_line(cmd: &CommandDef) -> String {
    format!("Usage: git-p4 {} {}\n", cmd.name, cmd.usage_tail)
}

/// Greedy whitespace wrapping, matching `textwrap.wrap` for the (hyphen-free,
/// single-spaced) help strings git-p4 declares.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

// ---------------------------------------------------------------------------
// repository location
// ---------------------------------------------------------------------------

/// `main()`'s `needsGit` block.
///
/// An explicit directory (from `--git-dir` or `$GIT_DIR`) is tried as given and
/// then with `/.git` appended; failing both is `die()`, exit 1. Without one,
/// `./.git` is tried and then upward discovery, whose failure quotes git's own
/// `rev-parse` diagnostic.
///
/// The `Err` arm carries the exit code and has already written its message.
fn locate_repo(explicit: Option<&str>) -> std::result::Result<gix::Repository, ExitCode> {
    if let Some(dir) = explicit {
        if let Ok(repo) = gix::open(dir) {
            return Ok(repo);
        }
        if let Ok(repo) = gix::open(format!("{dir}/.git")) {
            return Ok(repo);
        }
        eprintln!("fatal: cannot locate git repository at {dir}");
        return Err(ExitCode::from(1));
    }

    if let Ok(repo) = gix::open(".git") {
        return Ok(repo);
    }
    match gix::discover(".") {
        Ok(repo) => Ok(repo),
        Err(_) => {
            // `read_pipe`'s die: the failed command, then git's stderr verbatim
            // (which carries its own newline), then die's own newline.
            eprint!(
                "Command failed: git rev-parse --git-dir\nError: fatal: not a git repository (or \
                 any of the parent directories): .git\n\n"
            );
            Err(ExitCode::from(1))
        }
    }
}

// ---------------------------------------------------------------------------
// P4Branches
// ---------------------------------------------------------------------------

/// `P4Branches.run()` — update the local `p4/` remotes from `origin/p4/`, then
/// list every `p4/` remote with the depot paths and change number recorded in
/// its tip commit's `[git-p4: …]` trailer.
fn run_branches(repo: &gix::Repository) -> Result<ExitCode> {
    if origin_p4_branches_exist(repo) {
        create_or_update_branches_from_origin(repo)?;
    }

    for line in symbolic_remotes(repo)? {
        if !line.starts_with("p4/") || line == "p4/HEAD" {
            continue;
        }
        let full = format!("refs/remotes/{line}");
        let settings = extract_settings(&log_message(repo, &full)?);
        let (Some(paths), Some(change)) = (settings.depot_paths(), settings.get("change")) else {
            bail!(
                "branch {line:?} has no complete [git-p4: ...] trailer on its tip commit; stock \
                 git raises KeyError and prints a traceback here"
            );
        };
        println!("{line} <= {} ({change})", paths.join(","));
    }

    Ok(ExitCode::SUCCESS)
}

/// `originP4BranchesExist()` — whether `git rev-parse` resolves any of the three
/// origin names the import layout uses.
fn origin_p4_branches_exist(repo: &gix::Repository) -> bool {
    ["origin", "origin/p4", "origin/p4/master"]
        .iter()
        .any(|name| repo.rev_parse_single(*name).is_ok())
}

/// `createOrUpdateBranchesFromOrigin()` — mirror `origin/p4/<name>` into
/// `refs/remotes/p4/<name>` when the local side is absent or records an older
/// change for the same depot paths.
fn create_or_update_branches_from_origin(repo: &gix::Repository) -> Result<()> {
    const ORIGIN_PREFIX: &str = "origin/p4/";

    for line in symbolic_remotes(repo)? {
        if !line.starts_with(ORIGIN_PREFIX) || line.ends_with("HEAD") {
            continue;
        }
        let head_name = &line[ORIGIN_PREFIX.len()..];
        let remote_head = format!("refs/remotes/p4/{head_name}");
        let origin_head = format!("refs/remotes/{line}");

        let original = extract_settings(&log_message(repo, &origin_head)?);
        let (Some(orig_paths), Some(orig_change)) =
            (original.depot_paths(), original.get("change"))
        else {
            continue;
        };

        let mut update = false;
        match repo.try_find_reference(remote_head.as_str())? {
            None => update = true,
            Some(_) => {
                let settings = extract_settings(&log_message(repo, &remote_head)?);
                if let Some(change) = settings.get("change") {
                    let Some(paths) = settings.depot_paths() else {
                        bail!(
                            "{remote_head} records a change but no depot paths; stock git raises \
                             KeyError and prints a traceback here"
                        );
                    };
                    if paths == orig_paths {
                        // Non-numeric values make stock git raise ValueError;
                        // treat them as not-newer rather than fake a traceback.
                        let a = orig_change.parse::<i64>().ok();
                        let b = change.parse::<i64>().ok();
                        if let (Some(a), Some(b)) = (a, b) {
                            if a > b {
                                println!(
                                    "{line} ({a}) is newer than {remote_head} ({b}). Updating p4 \
                                     branch from origin."
                                );
                                update = true;
                            }
                        }
                    } else {
                        println!(
                            "Ignoring: {line} was imported from {} while {remote_head} was \
                             imported from {}",
                            orig_paths.join(","),
                            paths.join(",")
                        );
                    }
                }
            }
        }

        if update {
            let target = repo.rev_parse_single(origin_head.as_str())?.detach();
            let name: FullName = remote_head
                .as_str()
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name {remote_head:?}: {e}"))?;
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: Default::default(),
                    },
                    expected: PreviousValue::Any,
                    new: Target::Object(target),
                },
                name,
                deref: false,
            })?;
        }
    }
    Ok(())
}

/// `git rev-parse --symbolic --remotes`: every ref under `refs/remotes/`, name
/// shortened by that prefix, in refname order.
fn symbolic_remotes(repo: &gix::Repository) -> Result<Vec<String>> {
    const PREFIX: &[u8] = b"refs/remotes/";
    let mut names = Vec::new();
    for reference in repo.references()?.all()? {
        let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(rest) = reference.name().as_bstr().strip_prefix(PREFIX) {
            names.push(rest.to_str_lossy().into_owned());
        }
    }
    Ok(names)
}

/// `extractLogMessageFromGitCommit()` — everything after the header block of the
/// commit `rev` names, exactly as `git cat-file commit` would expose it.
///
/// The script scans for the first line of length one (the bare `\n` separating
/// headers from message); a header continuation always starts with a space, so
/// that is the first `\n\n`.
fn log_message(repo: &gix::Repository, rev: &str) -> Result<String> {
    let object = repo.rev_parse_single(rev)?.object()?;
    if object.kind != gix::objs::Kind::Commit {
        bail!("{rev} does not name a commit");
    }
    let data = &object.data;
    let body = match data.windows(2).position(|w| w == b"\n\n") {
        Some(i) => &data[i + 2..],
        None => &data[..0],
    };
    Ok(String::from_utf8_lossy(body).into_owned())
}

/// The key/value pairs recovered from a commit message's `[git-p4: …]` lines.
#[derive(Default)]
struct Settings(Vec<(String, String)>);

impl Settings {
    /// Last write wins, as assignment into a Python dict does.
    fn set(&mut self, key: String, value: String) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key, value)),
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `depot-paths`, falling back to the singular `depot-path`, split on `,`.
    fn depot_paths(&self) -> Option<Vec<String>> {
        let raw = self.get("depot-paths").or_else(|| self.get("depot-path"))?;
        if raw.is_empty() {
            return None;
        }
        Some(raw.split(',').map(str::to_string).collect())
    }
}

/// `extractSettingsGitLog()` — parse every `[git-p4: <assignments>]` line.
///
/// Each line is stripped, matched against `^ *\[git-p4: (.*)\]$`, split on `:`
/// into assignments, and each assignment split on the first `=`. A value
/// wrapped in double quotes has them removed.
fn extract_settings(log: &str) -> Settings {
    const OPEN: &str = "[git-p4: ";
    let mut settings = Settings::default();

    for line in log.split('\n') {
        let line = line.trim();
        let Some(inner) = line
            .strip_prefix(OPEN)
            .and_then(|rest| rest.strip_suffix(']'))
        else {
            continue;
        };
        for assignment in inner.split(':') {
            let (key, value) = match assignment.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (assignment.trim(), ""),
            };
            let value = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                // `val[1:-1]` on a lone `"` yields the empty string.
                Some(v) => v,
                None if value == "\"" => "",
                None => value,
            };
            settings.set(key.to_string(), value.to_string());
        }
    }
    settings
}

// ---------------------------------------------------------------------------
// P4Unshelve
// ---------------------------------------------------------------------------

/// `P4Unshelve.run()` up to the first p4 access.
///
/// A changelist count other than one makes `run` return false, which `main()`
/// answers with the help block and exit 2. A missing `--origin` ref is
/// `sys.exit("origin branch … does not exist")`, i.e. stderr and exit 1.
fn run_unshelve_preflight(
    repo: &gix::Repository,
    cmd: &CommandDef,
    opts: &[&OptDef],
    parsed: &Parsed,
) -> Result<ExitCode> {
    if parsed.positional.len() != 1 {
        print!("{}", format_help(cmd, opts)?);
        return Ok(ExitCode::from(2));
    }

    let origin = parsed.origin.as_deref().unwrap_or("HEAD");
    if repo.rev_parse_single(origin).is_err() {
        eprintln!("origin branch {origin} does not exist");
        return Ok(ExitCode::from(1));
    }

    bail!(
        "unsupported: unshelving changelist {:?} reads the shelved files through the external `p4` \
         client, for which there is no substrate in the vendored gitoxide crates (ported: the \
         changelist-count and --origin pre-flight)",
        parsed.positional[0]
    );
}
