//! `git history` — EXPERIMENTAL history rewriting (`fixup`, `reword`, `split`).
//!
//! What this module implements, byte-identically with stock git 2.55.0: the
//! whole command-line surface. That is `-h` for the command and for each
//! subcommand (usage text on stdout, exit 129), the missing/unknown subcommand
//! diagnostics, per-subcommand option parsing (`--update-refs`, `-n`/
//! `--dry-run`/`--no-dry-run`, `--reedit-message`/`--no-reedit-message`,
//! `--empty`, `--` plus trailing pathspecs for `split`), the option-value
//! validation messages, repository discovery, the single-revision check, commit
//! lookup, and `fixup`'s bare-repository rejection — each with git's exact
//! wording, stream, and exit status (129 for usage errors, 128 for `fatal:`,
//! 255 for `die()` after setup).
//!
//! What this module does NOT implement: the rewrite itself. All three
//! subcommands stop with a terse error once their arguments have been validated.
//! The missing substrate is named per subcommand below; none of it exists in
//! the vendored gitoxide:
//!
//! * A commit-replay engine. Every subcommand rewrites the target commit and
//!   then replays each descendant onto it, re-resolving trees through a
//!   three-way merge and re-dating committers. `gix-merge` can merge two trees
//!   against a base, but there is no replay driver, no empty-commit detection
//!   (`--empty=drop|keep|abort`), and no merge-in-history rejection.
//! * The ref-update fan-out. git collects the local branches descending from
//!   the target in a `strmap` and emits/updates them in that hash table's
//!   iteration order, which is neither sorted nor insertion-ordered
//!   (observed `zzz`, `main`, `aaa`, `mmm` for branches created `main`, `zzz`,
//!   `aaa`, `mmm`). `--dry-run` prints those `update <ref> <new> <old>` lines
//!   to stdout, so multi-branch dry-run output cannot be reproduced without
//!   porting git's `hashmap.c` bucket layout and growth policy.
//! * The commit-message editor template. `reword` (and `fixup
//!   --reedit-message`) writes `.git/COMMIT_EDITMSG` with the old message plus
//!   a commented `Changes to be committed:` block under an auto-selected
//!   comment character, launches the editor, then applies git's default
//!   message cleanup. None of the template generation or cleanup exists here.
//! * `split`'s interactive hunk selection, which is `git add -p`'s
//!   `add--interactive` patch loop (hunk splitting, `y/n/q/a/d/p/?` prompts)
//!   plus two editor invocations for the resulting messages.
//!
//! Emitting an approximation of any of the above would produce commits and ref
//! states that differ from git's, so each path errors instead.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

/// The three-line synopsis git prints for the command as a whole.
const USAGE: &str = "\
usage: git history fixup <commit> [--dry-run] [--update-refs=(branches|head)] [--reedit-message] [--empty=(drop|keep|abort)]
   or: git history reword <commit> [--dry-run] [--update-refs=(branches|head)]
   or: git history split <commit> [--dry-run] [--update-refs=(branches|head)] [--] [<pathspec>...]
";

/// `fixup`'s own `-h` text: synopsis, blank line, option list.
const USAGE_FIXUP: &str = "\
usage: git history fixup <commit> [--dry-run] [--update-refs=(branches|head)] [--reedit-message] [--empty=(drop|keep|abort)]

    --update-refs (branches|head)
                          control which refs should be updated
    -n, --[no-]dry-run    perform a dry-run without updating any refs
    --[no-]reedit-message open an editor to modify the commit message
    --empty (drop|keep|abort)
                          how to handle commits that become empty
";

/// `reword`'s own `-h` text.
const USAGE_REWORD: &str = "\
usage: git history reword <commit> [--dry-run] [--update-refs=(branches|head)]

    --update-refs (branches|head)
                          control which refs should be updated
    -n, --[no-]dry-run    perform a dry-run without updating any refs
";

/// `split`'s own `-h` text. Note the deliberately different `--update-refs`
/// description; git's own option table words it differently here.
const USAGE_SPLIT: &str = "\
usage: git history split <commit> [--dry-run] [--update-refs=(branches|head)]

    --update-refs (branches|head)
                          control ref update behavior
    -n, --[no-]dry-run    perform a dry-run without updating any refs
";

/// Which subcommand is running; selects the option table and usage text.
#[derive(Clone, Copy, PartialEq)]
enum Sub {
    Fixup,
    Reword,
    Split,
}

impl Sub {
    /// The `-h` text for this subcommand, also printed after an unknown option.
    fn usage(self) -> &'static str {
        match self {
            Sub::Fixup => USAGE_FIXUP,
            Sub::Reword => USAGE_REWORD,
            Sub::Split => USAGE_SPLIT,
        }
    }

    /// The name as it appears in the missing-substrate error.
    fn name(self) -> &'static str {
        match self {
            Sub::Fixup => "fixup",
            Sub::Reword => "reword",
            Sub::Split => "split",
        }
    }
}

/// Options common to all three subcommands, plus the `fixup`-only ones.
struct Opts {
    dry_run: bool,
    /// `--update-refs=head` restricts updates to HEAD; `branches` is the default.
    head_only: bool,
    /// `--reedit-message` (fixup only).
    reedit_message: bool,
    /// `--empty=<action>` (fixup only), unparsed beyond validation.
    empty: &'static str,
    /// The single `<commit>` argument.
    rev: Option<String>,
    /// Trailing pathspecs (split only).
    pathspecs: Vec<String>,
}

/// git's usage-error exit status (`usage()`/`usage_with_options()`).
const EXIT_USAGE: u8 = 129;
/// git's `die()` exit status once the command is past setup.
const EXIT_DIE: u8 = 255;
/// git's `die()` exit status from setup / option-value parsing (`fatal:`).
const EXIT_FATAL: u8 = 128;

/// `git history` — rewrite history by modifying one commit and replaying its
/// descendants.
///
/// Argument handling matches stock git exactly, including which stream each
/// diagnostic goes to and the exit status. The rewrite itself is not ported:
/// once arguments validate, each subcommand fails with an error naming the
/// substrate it would need. See the module docs for the full list.
pub fn history(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand name at index 0 so both calling conventions work.
    let args = match args.first() {
        Some(a) if a == "history" => &args[1..],
        _ => args,
    };

    let Some(first) = args.first() else {
        eprint!("error: need a subcommand\n{USAGE}\n");
        return Ok(ExitCode::from(EXIT_USAGE));
    };

    // `-h` anywhere in the leading position prints to stdout and still exits 129.
    if first == "-h" || first == "--help" {
        print!("{USAGE}\n");
        std::io::stdout().flush()?;
        return Ok(ExitCode::from(EXIT_USAGE));
    }

    let sub = match first.as_str() {
        "fixup" => Sub::Fixup,
        "reword" => Sub::Reword,
        "split" => Sub::Split,
        other => {
            eprint!("error: unknown subcommand: `{other}'\n{USAGE}\n");
            return Ok(ExitCode::from(EXIT_USAGE));
        }
    };

    let opts = match parse(sub, &args[1..])? {
        Parsed::Opts(o) => o,
        Parsed::Exit(code) => return Ok(code),
    };

    // git runs its repository setup before touching the revision.
    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            return Ok(ExitCode::from(EXIT_FATAL));
        }
    };

    let Some(rev) = opts.rev.as_deref() else {
        eprintln!("error: command expects a single revision");
        return Ok(ExitCode::from(EXIT_DIE));
    };

    // `fixup` reads staged changes, so it needs an index and a worktree.
    if sub == Sub::Fixup && repo.worktree().is_none() {
        eprintln!("error: cannot run fixup in a bare repository");
        return Ok(ExitCode::from(EXIT_DIE));
    }

    // git resolves the revision and requires it to name a commit.
    let commit = repo
        .rev_parse_single(rev)
        .ok()
        .and_then(|id| id.object().ok())
        .and_then(|obj| obj.peel_to_commit().ok());
    if commit.is_none() {
        eprintln!("error: commit cannot be found: {rev}");
        return Ok(ExitCode::from(EXIT_DIE));
    }

    // Everything past this point is the rewrite, which is not ported. Report the
    // specific substrate that is missing rather than producing divergent state.
    let _ = (opts.dry_run, opts.head_only, opts.reedit_message, opts.empty, &opts.pathspecs);
    let missing = match sub {
        Sub::Fixup => {
            "a commit-replay engine over gix-merge (three-way apply of the staged tree, \
             --empty=drop/keep/abort handling) and git's strmap-ordered branch fan-out"
        }
        Sub::Reword => {
            "the COMMIT_EDITMSG template plus message cleanup, a commit-replay engine, \
             and git's strmap-ordered branch fan-out"
        }
        Sub::Split => {
            "git add--interactive's patch-mode hunk selection, the COMMIT_EDITMSG template, \
             a commit-replay engine, and git's strmap-ordered branch fan-out"
        }
    };
    bail!("`history {}` is not ported: requires {missing}", sub.name());
}

/// Outcome of option parsing: either the options, or an exit status to return
/// after git's diagnostic has already been written.
enum Parsed {
    Opts(Box<Opts>),
    Exit(ExitCode),
}

/// Parse one subcommand's arguments with git's option table for that subcommand.
///
/// The first non-option argument is `<commit>`. For `split`, any further
/// arguments (with or without a preceding `--`) are pathspecs; for `fixup` and
/// `reword` a second positional is a usage error, reported by the caller via
/// the single-revision check.
fn parse(sub: Sub, args: &[String]) -> Result<Parsed> {
    let mut opts = Opts {
        dry_run: false,
        head_only: false,
        reedit_message: false,
        empty: "drop",
        rev: None,
        pathspecs: Vec::new(),
    };
    let mut positionals: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    // Report an unknown option exactly as git's parse-options does: the offending
    // name without its leading dashes, then the subcommand's full usage.
    let unknown = |name: &str| -> Parsed {
        eprint!("error: unknown option `{name}'\n{}\n", sub.usage());
        Parsed::Exit(ExitCode::from(EXIT_USAGE))
    };

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            positionals.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }

        let (name, value) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (a, None),
        };

        // Pull `--opt <v>` when `--opt=<v>` was not used.
        let mut take = |i: &mut usize| -> Option<String> {
            match value {
                Some(v) => Some(v.to_string()),
                None => {
                    *i += 1;
                    args.get(*i).cloned()
                }
            }
        };

        match name {
            "-h" | "--help" => {
                print!("{}\n", sub.usage());
                std::io::stdout().flush()?;
                return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
            }
            "-n" | "--dry-run" => opts.dry_run = true,
            "--no-dry-run" => opts.dry_run = false,
            "--update-refs" => {
                let Some(v) = take(&mut i) else {
                    eprint!("error: option `update-refs' requires a value\n{}\n", sub.usage());
                    return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
                };
                match v.as_str() {
                    "branches" => opts.head_only = false,
                    "head" => opts.head_only = true,
                    _ => {
                        eprintln!("error: update-refs expects one of 'branches' or 'head'");
                        return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
                    }
                }
            }
            "--reedit-message" if sub == Sub::Fixup => opts.reedit_message = true,
            "--no-reedit-message" if sub == Sub::Fixup => opts.reedit_message = false,
            "--empty" if sub == Sub::Fixup => {
                let Some(v) = take(&mut i) else {
                    eprint!("error: option `empty' requires a value\n{}\n", sub.usage());
                    return Ok(Parsed::Exit(ExitCode::from(EXIT_USAGE)));
                };
                opts.empty = match v.as_str() {
                    "drop" => "drop",
                    "keep" => "keep",
                    "abort" => "abort",
                    other => {
                        eprintln!(
                            "fatal: unrecognized '--empty=' action '{other}'; \
                             valid values are \"drop\", \"keep\", and \"abort\"."
                        );
                        return Ok(Parsed::Exit(ExitCode::from(EXIT_FATAL)));
                    }
                };
            }
            _ => {
                // git strips the leading dashes but keeps any `=<value>` suffix,
                // so report the whole argument, not the split-off name.
                return Ok(unknown(a.trim_start_matches('-')));
            }
        }
        i += 1;
    }

    let mut positionals = positionals.into_iter();
    opts.rev = positionals.next();
    let rest: Vec<String> = positionals.collect();
    match sub {
        // `split` takes trailing pathspecs; the other two take nothing further,
        // and a second positional trips the single-revision check.
        Sub::Split => opts.pathspecs = rest,
        _ if !rest.is_empty() => opts.rev = None,
        _ => {}
    }

    Ok(Parsed::Opts(Box::new(opts)))
}
