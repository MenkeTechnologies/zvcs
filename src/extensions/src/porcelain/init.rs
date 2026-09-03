use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use crate::lock::RepoLock;

/// `git init` — create an empty repository (worktree or `--bare`).
///
/// Ported onto gitoxide's `gix::init` / `gix::init_bare`, which lay down the
/// same on-disk layout git does (`.git/{HEAD,config,objects,refs,hooks,info}`)
/// with an unborn `HEAD` pointing at the initial branch. The initial branch is
/// resolved with git's exact precedence: `-b`/`--initial-branch` on the command
/// line, else the `init.defaultBranch` config value, else the compiled-in
/// default `master`. (gix's own fallback is `main`; this port overrides it to
/// git's `master` so the no-config case matches stock git byte-for-byte.)
/// Output mirrors stock git:
/// ```text
///   * fresh repo:    `Initialized empty Git repository in <gitdir>/`
///   * existing repo: `Reinitialized existing Git repository in <gitdir>/`
///   * with `--shared`: the word `shared ` is inserted before `Git repository`.
/// ```
///
/// Supported invocation forms:
/// ```text
///   * `git init [<directory>]`
///   * `git init --bare [<directory>]`                    (also into a non-empty dir)
///   * `git init -b <name>` / `--initial-branch=<name>`   (sets `HEAD` symref)
///   * `git init -q` / `--quiet`                          (suppresses the line)
///   * `git init --template=<dir>` / `--template <dir>`   (seed from a template)
///     with git's `copy_templates()` precedence: `--template` > the
///     `GIT_TEMPLATE_DIR` env var > the `init.templateDir` config (a pathname,
///     so a leading `~` expands) > gix's built-in default template.
///   * `git init --separate-git-dir=<gitdir>`             (real git dir elsewhere + `.git` link file)
///   * `git init --shared[=<permissions>]`                (group/world/octal sharing)
///   * `git init --object-format=<hash>` / `--object-format <hash>`
///                                                        (both `sha1` and `sha256`)
///     with git's precedence: the option > the `GIT_DEFAULT_HASH` env var >
///     the `init.defaultObjectFormat` config > the compiled-in `sha1`. `sha256`
///     writes the same `extensions.objectformat` + `core.repositoryformatversion
///     = 1` pair stock writes; the config level is resolved *before* the
///     repository is laid down, since the hash it names decides what is written.
///   * `git init --ref-format=<format>` / `--ref-format <format>`
///                                                        (`files` accepted; `reftable` rejected — see deviations)
///     with git's precedence: the option > the `GIT_DEFAULT_REF_FORMAT` env var >
///     the `init.defaultRefFormat` config > the compiled-in `files`.
///   * `init.defaultSubmodulePathConfig=true` seeds
///     `extensions.submodulePathConfig=true` (and the `core.repositoryformatversion=1`
///     bump it requires) into the new repository, exactly like stock git.
///   * `--no-bare` / `--no-quiet` / `--no-template` / `--no-separate-git-dir` /
///     `--no-initial-branch` / `--no-object-format` / `--no-ref-format`
///                                                        (git's auto-generated negations; reset to default, last-wins)
///   * `--` to terminate option parsing
/// ```
///
/// The environment `cmd_init_db()` reads for itself is honored too: `GIT_DIR`
/// names the directory to create (and `guess_repository_type()` then decides
/// whether it is a bare layout), `GIT_WORK_TREE` without it — or with `--bare` —
/// is the refusal git catches early, and `GIT_OBJECT_DIRECTORY` moves the object
/// store out of the git dir entirely, with no `<git-dir>/objects` written
/// alongside it.
///
/// A `<directory>` that does not exist is created, leading directories and all
/// (git `chdir()`s into it and falls back to
/// `safe_create_leading_directories_const()` + `mkdir()`), for bare and
/// non-bare alike.
///
/// Refusals follow `cmd_init_db`'s order, which mixes two exit codes and two
/// different usage renderings — callers branch on both, so neither the order
/// nor the rendering is interchangeable:
/// ```text
///   * an unknown option              parse_options -> usage_with_options():
///                                    `error: unknown option `x'`, the reflowed
///                                    usage, the option list. Exit 129.
///   * `--separate-git-dir` + `--bare`  die() -> exit 128.
///   * a second `<directory>`         usage(init_db_usage[0]): the usage string
///                                    alone, unreflowed, no option list. Exit 129.
///   * an unknown `--object-format` / `--ref-format`   die() -> exit 128.
///   * an initial branch name `check_refname_format()` rejects
///                                    die() -> exit 128, after the repository
///                                    skeleton exists but before `HEAD` is written.
/// ```
///
/// Ported from git's `builtin/init-db.c` + `setup.c` (`create_default_files`,
/// `copy_templates_1`, `separate_git_dir`) and `path.c`
/// (`calc_shared_perm`/`adjust_shared_perm`). The `--shared` permission math,
/// the `core.sharedrepository`/`receive.denyNonFastforwards` config values, the
/// template merge semantics, and the `gitdir:` link file all match stock git.
///
/// Reinitialization is `init_db()`'s second half, not a no-op that prints a
/// line: the templates are re-copied (never overwriting what is already there),
/// `core.bare` is rewritten when the flags disagree with the file, `--shared`
/// records its config and re-widens the permissions, `--separate-git-dir` moves
/// the existing git dir and leaves a `gitdir:` link behind, `--object-format`
/// disagreeing with the repository's own hash is fatal, and `--initial-branch`
/// is warned about and ignored. Which git dir is reinitialized follows git's own
/// resolution — `GIT_DIR`, else `.git`, with a `gitdir:` link file followed — so
/// `git init` inside a linked worktree reinitializes that worktree's git dir and
/// `git init` standing inside a `.git` directory creates a repository under it
/// rather than reinitializing the one it is standing in.
///
/// # Deviations (surfaced honestly, never faked)
/// ```text
///   * `--ref-format=reftable` is rejected with an honest "not supported" error
///     (not "silently accepted", and never faked into a mismatched-format repo):
///     there is no vendored reftable backend. `--ref-format=files` is a no-op
///     that matches the repository gix already writes, both object formats are
///     laid down for real, and an otherwise unrecognized value reproduces git's
///     exact error text (`unknown hash algorithm '<v>'` / `unknown ref storage
///     format '<v>'`).
/// ```

/// `cmd_init_db()`'s `struct option init_db_options[]` (builtin/init-db.c), in
/// table order, as [`super::resolve_long`] reads it. `git init-db` is the same
/// builtin ([`super::init_db`] forwards here), so it is the same table.
///
/// `--shared` is an `OPT_CALLBACK_F(... PARSE_OPT_OPTARG | PARSE_OPT_NONEG)`, so
/// `--no-shared` is not a spelling parse-options resolves.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "template",                    neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "bare",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "shared",                      neg: false, arg: super::Arg::Optional },
    super::LongOpt { name: "quiet",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "separate-git-dir",            neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "initial-branch",              neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "object-format",               neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "ref-format",                  neg: true,  arg: super::Arg::Required },
];
/// `git init`'s usage block, byte-for-byte from stock git 2.55.0. `-h` prints it on
/// stdout; a usage error prints the complaint and then this on stderr. Both exit 129.
const USAGE: &str = "usage: git init [-q | --quiet] [--bare] [--template=<template-directory>]\n                [--separate-git-dir <git-dir>] [--object-format=<format>]\n                [--ref-format=<format>]\n                [-b <branch-name> | --initial-branch=<branch-name>]\n                [--shared[=<permissions>]] [<directory>]\n\n    --[no-]template <template-directory>\n                          directory from which templates will be used\n    --[no-]bare           create a bare repository\n    --shared[=<permissions>]\n                          specify that the git repository is to be shared amongst several users\n    -q, --[no-]quiet      be quiet\n    --[no-]separate-git-dir <gitdir>\n                          separate git dir from working tree\n    -b, --[no-]initial-branch <name>\n                          override the name of the initial branch\n    --[no-]object-format <hash>\n                          specify the hash algorithm to use\n    --[no-]ref-format <format>\n                          specify the reference format to use\n\n";

/// `init_db_usage[0]` verbatim from git 2.55.0's `builtin/init-db.c`, with the
/// `"usage: "` prefix `usage_builtin()` prepends (`usage.c`).
///
/// This is NOT [`USAGE`]. git has two ways out of `builtin/init-db.c`:
/// `parse_options` failures go through `usage_with_options()`, which *reflows*
/// the usage string (continuation lines re-indented under `usage: git init `)
/// and appends the option list — that is [`USAGE`], what `-h` and an unknown
/// option print. Too many positional arguments instead calls plain
/// `usage(init_db_usage[0])`, which prints the C string literal as written: the
/// continuation lines keep the 9 spaces they have in the source, and no option
/// list follows. Both exit 129, but the bytes differ, so the two paths need two
/// constants.
const PLAIN_USAGE: &str = "usage: git init [-q | --quiet] [--bare] [--template=<template-directory>]\n         [--separate-git-dir <git-dir>] [--object-format=<format>]\n         [--ref-format=<format>]\n         [-b <branch-name> | --initial-branch=<branch-name>]\n         [--shared[=<permissions>]] [<directory>]\n";

/// `die()`: `fatal: <msg>` and exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// A `parse-options` complaint followed by the usage block, exit 129.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// git's bare `usage(init_db_usage[0])`: the unreflowed usage string alone on
/// stderr, exit 129. Reached only by `0 < argc` after `parse_options` — i.e.
/// more than one `<directory>` operand.
fn usage_only() -> ExitCode {
    eprint!("{PLAIN_USAGE}");
    ExitCode::from(129)
}

pub fn init(args: &[String]) -> Result<ExitCode> {
    let mut bare = false;
    let mut quiet = false;
    let mut initial_branch: Option<String> = None;
    // Every non-option operand, in order. git's `parse_options` collects these
    // by compacting `argv` and reports how many survived; the count is only
    // judged *after* parsing, so a second directory is not an error until the
    // whole command line has been read (`git init a b --frobnicate` reports the
    // unknown option, not the extra operand).
    let mut positionals: Vec<String> = Vec::new();
    let mut template: Option<String> = None;
    let mut separate_git_dir: Option<String> = None;
    // The requested `--object-format`/`--ref-format` values (validated after the
    // parse loop, exactly like git validates `object_format` after parse-options).
    let mut object_format: Option<String> = None;
    let mut ref_format: Option<String> = None;
    // The `git_config_perm` value: `None` = `--shared` not given; `Some(0)` =
    // umask/false (no sharing); `Some(0o660)` = group; `Some(0o664)` = everybody;
    // `Some(neg)` = an explicit `0xxx` file mode (stored negated, as git does).
    let mut shared: Option<i32> = None;
    let mut positional_only = false;

    let mut i = 0;
    while i < args.len() {
        // `i` steps past this argument up front so that it is already
        // `parse_opt_ctx_t`'s "next unread argument"; `take_value` — the shared
        // port of `get_arg()` — then advances it over a value, and the
        // missing-value refusal is parse-options' rather than each arm's own.
        let arg = &args[i];
        i += 1;
        if positional_only || !arg.starts_with('-') || arg == "-" {
            positionals.push(arg.clone());
            continue;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, which is why the test sits before the abbreviation
        // resolution below rather than in `LONG_OPTS`. This table has no
        // `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block `-h`
        // prints.
        if arg == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // Respell a unique abbreviation as the name it resolves to, so `--init-b`
        // reaches the same arm as `--initial-branch`.
        let canonical;
        let arg = match super::canonical_long(arg, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(arg, &first, &second, USAGE))
            }
        };
        match arg {
            "--" => positional_only = true,
            "--bare" => bare = true,
            // git's parse-options auto-generates a `--no-` form for every OPT_BOOL
            // and OPT_STRING; the negation resets the option to its default (false
            // for booleans, unset for strings), and parsing is last-wins. `--shared`
            // uses a custom callback with no negation, so `--no-shared` is NOT
            // accepted (git reports it as an unknown option) and is left unhandled.
            "--no-bare" => bare = false,
            // `parse-options` answers `-h` before anything else, on stdout.
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            "-b" | "--initial-branch" => {
                initial_branch = Some(super::take_value(args, &mut i, arg)?.to_string())
            }
            "--no-initial-branch" => initial_branch = None,
            "--template" => template = Some(super::take_value(args, &mut i, arg)?.to_string()),
            "--no-template" => template = None,
            "--separate-git-dir" => {
                separate_git_dir = Some(super::take_value(args, &mut i, arg)?.to_string())
            }
            "--no-separate-git-dir" => separate_git_dir = None,
            // `--object-format <hash>` / `--ref-format <format>` take a required
            // value (space or `=`), validated after the loop. Their `--no-` forms
            // unset the request, matching git's OPT_STRING negation.
            "--object-format" => {
                object_format = Some(super::take_value(args, &mut i, arg)?.to_string())
            }
            "--no-object-format" => object_format = None,
            "--ref-format" => ref_format = Some(super::take_value(args, &mut i, arg)?.to_string()),
            "--no-ref-format" => ref_format = None,
            // `--shared` takes an OPTIONAL argument, attached with `=` only (git's
            // PARSE_OPT_OPTARG). `--shared group` treats `group` as a positional.
            "--shared" => shared = Some(0o660),
            _ if arg.starts_with("--initial-branch=") => {
                initial_branch = Some(arg["--initial-branch=".len()..].to_string());
            }
            _ if arg.starts_with("--template=") => {
                template = Some(arg["--template=".len()..].to_string());
            }
            _ if arg.starts_with("--separate-git-dir=") => {
                separate_git_dir = Some(arg["--separate-git-dir=".len()..].to_string());
            }
            _ if arg.starts_with("--shared=") => {
                shared = Some(parse_shared_value(&arg["--shared=".len()..])?);
            }
            _ if arg.starts_with("--object-format=") => {
                object_format = Some(arg["--object-format=".len()..].to_string());
            }
            _ if arg.starts_with("--ref-format=") => {
                ref_format = Some(arg["--ref-format=".len()..].to_string());
            }
            // A long name no entry claims is `PARSE_OPT_UNKNOWN`: the `error:`
            // line and the block, both on stderr, exit 129.
            _ if arg.starts_with("--") => return Ok(super::unknown_option(arg, USAGE)),
            // Every remaining `-<chars>` token, walked the way
            // `parse_options_step()` walks a short cluster
            // (parse-options.c:1061-1107). `init`'s three short options are `-q`,
            // `-b` and the implicit `-h`, and `-b` takes its value from the rest
            // of the cluster or the next argv element — so `git init -qb main`
            // is `-q -b main` and `git init -qb` is ``switch `b' requires a
            // value``. Reporting the whole token as one long option is what made
            // `git init -a` say ``unknown option `a'`` where stock says
            // ``unknown switch `a'``.
            _ => {
                for (off, c) in arg.char_indices().skip(1) {
                    match c {
                        'q' => quiet = true,
                        'b' => {
                            let rest = &arg[off + c.len_utf8()..];
                            initial_branch = Some(match rest.is_empty() {
                                true => super::take_value(args, &mut i, "-b")?.to_string(),
                                false => rest.to_string(),
                            });
                            break;
                        }
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => {
                            return Ok(super::unknown_option(&format!("-{}", &arg[off..]), USAGE))
                        }
                    }
                }
            }
        }
    }

    // Post-parse checks, in git's order — `cmd_init_db` decides these one after
    // another and each `die()`s/`usage()`s on the spot, so a command line that
    // trips several is judged by the FIRST one git reaches, not by ours:
    //   1. `--separate-git-dir` with `--bare`   → die, exit 128
    //   2. more than one `<directory>` operand  → usage(), exit 129
    //   3. an unknown `--object-format`         → die, exit 128
    //   4. an unknown `--ref-format`            → die, exit 128
    // Verified against stock 2.55.0: `git init --object-format=bogus a b` prints
    // the usage block and exits 129 (the operand count is judged first), while
    // `git init --separate-git-dir=x --bare a b` dies with the option-conflict
    // message and exits 128 (that check precedes the operand count).

    // git refuses to combine these (builtin/init-db.c: "cannot be used together").
    if separate_git_dir.is_some() && bare {
        crate::git_fatal!(
            "options '--separate-git-dir' and '--bare' cannot be used together"
        );
    }

    // `} else if (0 < argc) { usage(init_db_usage[0]); }` — anything past the
    // single optional `<directory>` is a usage error, not a `die()`.
    if positionals.len() > 1 {
        return Ok(usage_only());
    }
    let directory = positionals.into_iter().next();

    // `real_pathdup(real_git_dir, 1)` / `real_pathdup(template_dir, 1)`
    // (`builtin/init-db.c`, immediately after `parse_options`): a *relative*
    // `--separate-git-dir` / `--template` is resolved against the directory git
    // was invoked from — before the `<directory>` operand moves it — and a path
    // whose leading components do not exist is fatal on the spot. An absolute
    // value is left untouched, exactly as `is_absolute_path()` decides.
    let separate_git_dir = match separate_git_dir {
        Some(p) if !Path::new(&p).is_absolute() => Some(real_pathdup(&p)?),
        other => other,
    };
    let template = match template {
        Some(p) if !p.is_empty() && !Path::new(&p).is_absolute() => Some(real_pathdup(&p)?),
        other => other,
    };

    let target = PathBuf::from(directory.as_deref().unwrap_or("."));

    // git `chdir()`s into the `<directory>` operand and, when that fails, creates
    // it — leading directories via `safe_create_leading_directories_const()` then
    // the leaf via `mkdir()` — before retrying (`builtin/init-db.c`). So
    // `git init nested/dir` works with no `nested/` present. This port never
    // chdirs (it passes the path down instead), so the creation has to be done
    // here: `gix::init` happens to create leading directories itself, but
    // `gix::init_bare` does not, which is why `git init --bare nested/dir` failed
    // where the non-bare form succeeded.
    //
    // It runs here, *ahead* of the format validation below, because that is
    // git's order: `cmd_init_db` enters (and creates) the operand directory and
    // only then looks at `--object-format` / `--ref-format`. So
    // `git init --object-format=bogus sub` leaves `sub/` behind on its way out.
    if let Some(dir) = directory.as_deref() {
        if !target.is_dir() {
            if let Err(e) = std::fs::create_dir_all(&target) {
                // git's `die_errno(_("cannot mkdir %s"), argv[0])`, whose reason
                // is `strerror(errno)` with none of Rust's ` (os error N)` tail.
                return Ok(fatal(&format!(
                    "cannot mkdir {dir}: {}",
                    crate::external::strerror(&e)
                )));
            }
        }
    }

    // The directory git stands in once the operand has been entered. Every
    // relative path read out of the environment below resolves against it,
    // which is what `chdir(argv[0])` buys stock git.
    let cwd = match std::fs::canonicalize(&target) {
        Ok(p) => p,
        Err(_) => std::env::current_dir()?.join(&target),
    };

    // Resolve the repository formats with git's precedence (`builtin/init-db.c`
    // + `setup.c`): the command-line option wins, then the `GIT_DEFAULT_HASH` /
    // `GIT_DEFAULT_REF_FORMAT` environment variable, then the
    // `init.defaultObjectFormat` / `init.defaultRefFormat` config, then the
    // compiled-in `sha1` / `files`. The first two levels are fatal on an
    // unrecognized value; the config level only warns and falls back to the
    // compiled-in default (verified against stock git 2.55.0: `GIT_DEFAULT_REF_FORMAT=bogus
    // git init` dies with `fatal: unknown ref storage format 'bogus'`, while
    // `init.defaultObjectFormat=bogus` prints `warning: unknown hash algorithm
    // 'bogus'` and still creates a sha1 repository).
    //
    // The command-line level is judged here, where `cmd_init_db` judges it; the
    // environment level is `init_db`'s `validate_hash_algorithm()` /
    // `validate_ref_storage_format()`, which run later — after the git directory
    // has been resolved — and are therefore checked further down.
    if let Some(fmt) = object_format.as_deref() {
        if check_object_format(fmt)? == FormatCheck::Unrecognized {
            return Ok(fatal(&format!("unknown hash algorithm '{fmt}'")));
        }
    }
    if let Some(fmt) = ref_format.as_deref() {
        if check_ref_format(fmt)? == FormatCheck::Unrecognized {
            return Ok(fatal(&format!("unknown ref storage format '{fmt}'")));
        }
    }

    // umask/false/0 leaves the repository unshared, exactly like `--shared` never
    // being passed (git's init_shared_repository == 0 is falsy).
    let shared = shared.filter(|&s| s != 0);

    // `--bare` makes the directory git is standing in the git directory itself:
    // `setenv(GIT_DIR_ENVIRONMENT, cwd, argc > 0)`. The overwrite flag is
    // `argc > 0`, so an inherited `GIT_DIR` survives `git init --bare` with no
    // operand and loses to the operand directory when one is given.
    let mut git_dir_env = std::env::var("GIT_DIR").ok();
    if bare && (directory.is_some() || git_dir_env.is_none()) {
        git_dir_env = Some(cwd.to_string_lossy().into_owned());
    }

    // "GIT_WORK_TREE makes sense only in conjunction with GIT_DIR without
    // --bare.  Catch the error early." — `cmd_init_db`'s own comment, and its
    // own message.
    let work_tree_env = std::env::var("GIT_WORK_TREE").ok();
    if (git_dir_env.is_none() || bare) && work_tree_env.is_some() {
        crate::git_fatal!(
            "GIT_WORK_TREE (or --work-tree=<directory>) not allowed without \
             specifying GIT_DIR (or --git-dir=<directory>)"
        );
    }

    // `if (!git_dir) git_dir = DEFAULT_GIT_DIR_ENVIRONMENT;`
    let git_dir_spec = git_dir_env.unwrap_or_else(|| ".git".to_string());
    // `guess_repository_type()`: with no `--bare`, a git dir that is not `.`,
    // not the cwd, not `.git` and not `*/.git` is taken to be a bare repository
    // ("Otherwise it is often bare. At this point we are just guessing.").
    let bare_layout = bare || guess_repository_type(&git_dir_spec, &cwd);

    // `init_db`'s `original_git_dir`: `real_pathdup(git_dir, 1)`, the git
    // directory named on its own terms, before any `gitdir:` link is followed.
    let git_dir_arg = PathBuf::from(&git_dir_spec);
    let original_git_dir = real_path(if git_dir_arg.is_absolute() {
        git_dir_arg
    } else {
        cwd.join(git_dir_arg)
    })?;

    // `separate_git_dir()` (`setup.c`): when the named git dir already exists,
    // move it to the requested location and leave a `gitdir:` link file behind.
    // On a fresh init there is nothing to move, and the relocation happens after
    // the skeleton has been laid down (below), which is where this port already
    // did it.
    let migrated = separate_git_dir.is_some() && std::fs::symlink_metadata(&original_git_dir).is_ok();
    let mut git_dir: PathBuf = match separate_git_dir.as_deref() {
        Some(real) if migrated => {
            let real_abs = PathBuf::from(real);
            migrate_git_dir(&real_abs, &original_git_dir)?;
            real_abs
        }
        Some(_) => original_git_dir.clone(),
        // `repo_set_gitdir()` reads a `gitdir:` link file and takes its target as
        // the git directory, which is how `git init` inside a linked worktree
        // reinitializes `<common>/worktrees/<name>` rather than the link file.
        None => read_gitfile(&original_git_dir).unwrap_or_else(|| original_git_dir.clone()),
    };

    // `is_reinit()` (`setup.c`) is exactly this: a readable (or symlinked)
    // `HEAD` inside the git directory, nothing else.
    let reinit = std::fs::symlink_metadata(git_dir.join("HEAD")).is_ok();

    // `validate_hash_algorithm()` (`setup.c`), in its order: the command-line
    // hash may not disagree with the hash an existing repository already uses,
    // and only then is `GIT_DEFAULT_HASH` looked at.
    if reinit {
        if let Some(fmt) = object_format.as_deref() {
            if fmt != repository_object_format(&git_dir) {
                crate::git_fatal!("attempt to reinitialize repository with different hash");
            }
        }
    }
    let env_object_format = std::env::var("GIT_DEFAULT_HASH").ok();
    if let Some(fmt) = env_object_format.as_deref() {
        if check_object_format(fmt)? == FormatCheck::Unrecognized {
            return Ok(fatal(&format!("unknown hash algorithm '{fmt}'")));
        }
    }
    let env_ref_format = std::env::var("GIT_DEFAULT_REF_FORMAT").ok();
    if let Some(fmt) = env_ref_format.as_deref() {
        if check_ref_format(fmt)? == FormatCheck::Unrecognized {
            return Ok(fatal(&format!("unknown ref storage format '{fmt}'")));
        }
    }
    let object_format = object_format.or(env_object_format);
    let ref_format = ref_format.or(env_ref_format);

    // Create the repository. gix lays down the full template + config and returns
    // an opened handle with an unborn HEAD on the default branch. gix refuses
    // `--bare` into a non-empty directory where stock git permits it, so fall
    // back to a scratch-dir build in that one case.
    //
    // The object format decides what is laid down, so the config level of its
    // precedence chain has to be resolved *before* the repository exists rather
    // than out of the finished repository's snapshot below. That is also where
    // git reads it: `cmd_init_db()` calls `git_config_get_string()` for
    // `init.defaultobjectformat` with no repository open, so the value can only
    // come from the global/system files — [`crate::config::global_config`] is
    // that same pair. An unrecognized value selects nothing here and is warned
    // about below, which is git's fall back to the compiled-in default.
    let configured_object_format = match object_format {
        Some(_) => None,
        None => crate::config::global_config()
            .string("init.defaultObjectFormat")
            .map(|v| v.to_string())
            .filter(|v| matches!(check_object_format(v), Ok(FormatCheck::Implemented))),
    };
    let create_opts = gix::create::Options {
        object_hash: object_hash_of(
            object_format
                .as_deref()
                .or(configured_object_format.as_deref()),
        ),
        ..Default::default()
    };
    // Where the skeleton goes is decided by the git directory, not by the
    // operand: gix's bare kind lays it down *in* the directory it is given, its
    // worktree kind lays it down in that directory's `.git`. A `bare_layout` git
    // dir is therefore built directly, and a `<worktree>/.git` one is built from
    // its parent.
    let repo = if reinit {
        None
    } else if bare_layout {
        Some(
            match gix::ThreadSafeRepository::init(&git_dir, gix::create::Kind::Bare, create_opts) {
                Ok(r) => r.to_thread_local(),
                Err(gix::init::Error::Init(gix::create::Error::DirectoryNotEmpty { .. })) => {
                    init_bare_into_nonempty(&git_dir, create_opts)?
                }
                Err(e) => return Err(anyhow::anyhow!("{e}")),
            },
        )
    } else {
        let worktree = git_dir.parent().unwrap_or(&cwd).to_path_buf();
        Some(
            gix::ThreadSafeRepository::init(
                &worktree,
                gix::create::Kind::WithWorktree,
                create_opts,
            )
            .map(|r| r.to_thread_local())
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        )
    };

    if let Some(repo) = repo.as_ref() {
        // The config level of the format precedence chain: consulted only when
        // neither the option nor the environment variable named a format. An
        // unrecognized value is a warning and falls back to the compiled-in default,
        // matching stock git; a *recognized but unimplemented* value (`sha256` /
        // `reftable`) is rejected with the same honest error the option produces,
        // rather than silently laying down a repository in the other format.
        if object_format.is_none() {
            if let Some(fmt) = config_string(repo, "init.defaultObjectFormat") {
                if check_object_format(&fmt)? == FormatCheck::Unrecognized {
                    eprintln!("warning: unknown hash algorithm '{fmt}'");
                }
            }
        }
        if ref_format.is_none() {
            if let Some(fmt) = config_string(repo, "init.defaultRefFormat") {
                if check_ref_format(&fmt)? == FormatCheck::Unrecognized {
                    eprintln!("warning: unknown ref storage format '{fmt}'");
                }
            }
        }

        // Resolve the initial branch name, matching git's precedence exactly:
        //   1. `-b <name>` / `--initial-branch=<name>` on the command line, else
        //   2. the `init.defaultBranch` config value (any scope), else
        //   3. the compiled-in default `master`.
        // gix::init already points the unborn HEAD at `init.defaultBranch` (or its
        // own `main` fallback when that is unset), so recomputing here is what lets
        // the no-config case land on git's `master` rather than gix's `main`. When
        // the name gix already chose matches, the HEAD repoint below is a no-op.
        let branch_name = match initial_branch.clone() {
            Some(name) => name,
            None => repo
                .config_snapshot()
                .string("init.defaultBranch")
                .map(|v| v.to_string())
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "master".to_string()),
        };

        // Repoint the unborn HEAD symref to the resolved branch. This is a ref
        // mutation, so serialize it through the repo coordinator like every other
        // write command. gix writes no reflog for a symbolic update to an unborn
        // branch, matching stock git init (which creates no `logs/HEAD`). For a
        // separate git dir this happens before the git dir is moved, so the lock and
        // ref edit still target the freshly-created `<target>/.git`.
        //
        // git checks the composed ref here, not at parse time:
        // `create_reference_database()` (`setup.c`) builds `refs/heads/<name>` and
        // `die(_("invalid initial branch name: '%s'"), initial_branch)` when
        // `check_refname_format(ref, 0)` rejects it — so the diagnostic is git's
        // `die()` (exit 128), and it lands only after the repository skeleton
        // already exists, exactly like here.
        let src_git_dir = repo.git_dir().to_path_buf();
        let branch: FullName = match format!("refs/heads/{branch_name}").try_into() {
            Ok(b) => b,
            Err(_) => {
                // git dies *before* `HEAD` is written: `create_default_files()` lays
                // down `config`, `description`, `hooks/`, `info/` and the refs
                // directories, and only then does `create_reference_database()`
                // validate the name and, if it passes, symref `HEAD` at it. gix's
                // `init`/`init_bare` bundle the skeleton and `HEAD` into one call, so
                // the file exists here by the time the name can be checked; removing
                // it leaves the same on-disk wreckage stock git leaves behind (which
                // the parity probe compares, since a bare init drops these files in
                // the caller's directory for it to see).
                // Same reasoning for the object store: stock git creates
                // `objects/{info,pack}` only after the reference database is set up,
                // so a failed init leaves none. `remove_dir` refuses a non-empty
                // directory, so a pre-existing object store is never touched.
                let _ = std::fs::remove_file(src_git_dir.join("HEAD"));
                for dir in ["objects/pack", "objects/info", "objects"] {
                    let _ = std::fs::remove_dir(src_git_dir.join(dir));
                }
                return Ok(fatal(&format!(
                    "invalid initial branch name: '{branch_name}'"
                )));
            }
        };
        {
            let _lock = RepoLock::acquire(&src_git_dir);
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: "init: set initial branch".into(),
                    },
                    expected: PreviousValue::Any,
                    new: Target::Symbolic(branch),
                },
                name: "HEAD"
                    .try_into()
                    .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
                deref: false,
            })
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        // `--separate-git-dir` on a *fresh* init: relocate the git dir just built
        // to the requested path and drop a `gitdir: <abs>` link file in its
        // place, exactly like git's `separate_git_dir()` (`setup.c`). The message
        // then names the real git dir.
        if let Some(real) = separate_git_dir.as_deref() {
            git_dir = relocate_git_dir(&src_git_dir, &target, real)?;
        }
    } else if let Some(name) = initial_branch.as_deref() {
        // `if (reinit && initial_branch) warning(_("re-init: ignored --initial-branch=%s"))`.
        eprintln!("warning: re-init: ignored --initial-branch={name}");
    }

    // Resolve the template directory with git's `copy_templates()` precedence
    // (`builtin/init-db.c`): the `--template` command-line value wins, else the
    // `GIT_TEMPLATE_DIR` environment variable, else the `init.templateDir`
    // config (read as a pathname, so a leading `~` expands, matching git's
    // `git_config_get_pathname("init.templatedir")`), else gix's already
    // laid-down built-in default. An explicit (even empty) `--template` or a set
    // `GIT_TEMPLATE_DIR` short-circuits before the config is consulted, so the
    // config is a DEFAULT the flag/env override — never the other way around.
    let template = template
        .or_else(|| std::env::var("GIT_TEMPLATE_DIR").ok())
        .or_else(|| configured_template_dir(repo.as_ref(), &git_dir));

    // Seed the git dir from the resolved template. On a fresh init this replaces
    // gix's built-in default template so ONLY the requested template's files
    // remain (matching git, which uses the given template dir instead of the
    // default, not in addition to it); on a reinit git only fills in what is
    // missing, so nothing already there is disturbed. Structural files stay in
    // place either way.
    if let Some(tpl) = template.as_deref().filter(|t| !t.is_empty()) {
        if reinit {
            copy_templates(tpl, &git_dir)?;
        } else {
            apply_template(tpl, &git_dir)?;
        }
    }

    // `init.defaultSubmodulePathConfig=true` asks every new repository to opt into
    // the submodule-path extension, which git records as
    // `extensions.submodulePathConfig=true` plus the `core.repositoryformatversion=1`
    // bump every extension requires.
    if let Some(repo) = repo.as_ref() {
        if config_bool(repo, "init.defaultSubmodulePathConfig") == Some(true) {
            enable_submodule_path_config(&git_dir)?;
        }
    }

    // `create_default_files()` rewrites `core.bare` (and, when the repository has
    // a work tree, seeds `core.logallrefupdates`) on every run, reinit included —
    // which is only observable when the flags disagree with what is on disk, as
    // `git init --bare` inside an existing non-bare git dir does. gix has already
    // written both for a fresh repository, so this only has work to do on reinit,
    // and it writes nothing whose value is already correct.
    let has_work_tree = !bare_layout || work_tree_env.is_some();
    if reinit {
        set_config_value(&git_dir, "core", "bare", if has_work_tree { "false" } else { "true" })?;
        if has_work_tree {
            set_config_default(&git_dir, "core", "logallrefupdates", "true")?;
        }
    } else if bare_layout && work_tree_env.is_some() {
        // `is_bare_repository()` is `is_bare_repository_cfg && !get_git_work_tree()`,
        // so a guessed-bare git dir with `GIT_WORK_TREE` set is *not* bare: git
        // writes `core.bare = false` and points `core.worktree` back at the tree.
        let work_tree = work_tree_env.as_deref().unwrap_or(".");
        let work_tree_abs = real_path(cwd.join(work_tree))?;
        set_config_value(&git_dir, "core", "bare", "false")?;
        set_config_default(&git_dir, "core", "logallrefupdates", "true")?;
        set_config_value(
            &git_dir,
            "core",
            "worktree",
            &work_tree_abs.to_string_lossy(),
        )?;
    }

    // `create_object_directory()`: `GIT_OBJECT_DIRECTORY` moves the object store
    // out of the git dir entirely, and git creates only the store the variable
    // names — never `<git-dir>/objects` as well.
    create_object_directory(&git_dir, &cwd)?;

    // `--shared[=...]`: record the sharing config and widen permissions across the
    // whole git dir, porting git's `create_default_files` config write and
    // `adjust_shared_perm` (which git applies per-file during creation; a single
    // recursive walk here produces the identical on-disk result). Absent the
    // flag, `get_shared_repository()` still answers with whatever
    // `core.sharedRepository` the repository's own config carries, which is what
    // decides the wording of the message below.
    let shared = shared.or_else(|| configured_shared(&git_dir));
    if let Some(shared) = shared {
        write_shared_config(&git_dir, shared)?;
        #[cfg(unix)]
        adjust_shared_perm_recursive(&git_dir, shared)?;
    }

    if !quiet {
        let shared_word = if shared.is_some() { "shared " } else { "" };
        let verb = if reinit {
            "Reinitialized existing"
        } else {
            "Initialized empty"
        };
        println!(
            "{verb} {shared_word}Git repository in {}",
            display_git_dir(&git_dir)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The outcome of checking a requested object/ref storage format name against
/// the formats git knows and this port implements.
#[derive(PartialEq, Eq)]
enum FormatCheck {
    /// A name git recognizes and this port lays down (`sha1` / `files`).
    Implemented,
    /// A name git does not recognize at all. The caller decides whether that is
    /// fatal (command line / environment) or a warning (config).
    Unrecognized,
}

/// git's `hash_algo_by_name` recognizes exactly `sha1` and `sha256`, and this
/// port lays down both. `sha1` is the compiled-in default, so it is a no-op that
/// matches stock git byte-for-byte; `sha256` routes [`object_hash_of`]'s
/// `gix_hash::Kind::Sha256` into `gix::create::Options`, which writes
/// `extensions.objectformat = sha256` together with the
/// `core.repositoryformatversion = 1` bump every extension requires
/// (`src/ported/gix/src/create.rs:287-299`).
fn check_object_format(fmt: &str) -> Result<FormatCheck> {
    match fmt {
        "sha1" | "sha256" => Ok(FormatCheck::Implemented),
        _ => Ok(FormatCheck::Unrecognized),
    }
}

/// The `gix_hash::Kind` a resolved `--object-format` name selects, as
/// `gix::create::Options::object_hash` wants it: `None` is git's legacy sha1
/// repository, which carries no `extensions.objectformat` key at all, and
/// `Some(Sha256)` is the one that does.
fn object_hash_of(fmt: Option<&str>) -> Option<gix::hash::Kind> {
    match fmt {
        Some("sha256") => Some(gix::hash::Kind::Sha256),
        _ => None,
    }
}

/// git's ref storage formats are exactly `files` and `reftable`. `files` is the
/// backend gix writes, so it is a no-op match; `reftable` has no vendored
/// backend and is rejected honestly.
fn check_ref_format(fmt: &str) -> Result<FormatCheck> {
    match fmt {
        "files" => Ok(FormatCheck::Implemented),
        "reftable" => anyhow::bail!(
            "the reftable ref storage format is not supported: no vendored \
             reftable backend"
        ),
        _ => Ok(FormatCheck::Unrecognized),
    }
}

/// Read a non-empty string config value from `repo`'s resolved configuration
/// (any scope), the way git's `git_config_get_string` sees it.
fn config_string(repo: &gix::Repository, key: &str) -> Option<String> {
    repo.config_snapshot()
        .string(key)
        .map(|v| v.to_string())
        .filter(|v| !v.trim().is_empty())
}

/// Read a boolean config value from `repo`'s resolved configuration (any scope).
fn config_bool(repo: &gix::Repository, key: &str) -> Option<bool> {
    repo.config_snapshot().boolean(key)
}

/// Record the submodule-path extension in `git_dir`'s config the way stock git
/// does: `extensions.submodulePathConfig=true` together with the
/// `core.repositoryformatversion=1` bump that any `extensions.*` entry requires.
/// Shared with `git clone`, which honors `init.defaultSubmodulePathConfig` too.
pub(super) fn enable_submodule_path_config(git_dir: &Path) -> Result<()> {
    let path = git_dir.join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    file.set_raw_value_by("extensions", None, "submodulePathConfig", "true")?;
    file.set_raw_value_by("core", None, "repositoryformatversion", "1")?;
    std::fs::write(&path, file.to_bstring())?;
    Ok(())
}

/// Build a bare repository inside a non-empty `target`. gix hard-refuses this
/// (`create::into` checks emptiness unconditionally for bare), while stock git
/// permits it. Lay the layout down in an empty scratch subdirectory, then move
/// each entry up into `target`, yielding the same on-disk result git produces.
fn init_bare_into_nonempty(
    target: &Path,
    create_opts: gix::create::Options,
) -> Result<gix::Repository> {
    std::fs::create_dir_all(target)?;
    let scratch = target.join(format!(".git-init-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    gix::ThreadSafeRepository::init(&scratch, gix::create::Kind::Bare, create_opts)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    for entry in std::fs::read_dir(&scratch)? {
        let entry = entry?;
        std::fs::rename(entry.path(), target.join(entry.file_name()))?;
    }
    std::fs::remove_dir(&scratch)?;
    // `gix::open` would discover the *worktree* repository a `.git` directory beside
    // the new layout still names; the bare repository just laid down is `target` itself.
    gix::open_opts(
        target,
        gix::open::Options::isolated().open_path_as_is(true),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Move the freshly-created git dir (`src`, i.e. `<target>/.git`) to the
/// requested `real` location and write a `gitdir: <abs>` link file at
/// `<target>/.git`. Returns the absolute real git dir. Ports `separate_git_dir()`
/// from git's `setup.c` for the fresh-init case. Shared with `git clone`, whose
/// `--separate-git-dir` lands on the same on-disk result.
pub(super) fn relocate_git_dir(src: &Path, target: &Path, real: &str) -> Result<PathBuf> {
    let real_pb = {
        let p = PathBuf::from(real);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()?.join(p)
        }
    };
    if let Some(parent) = real_pb.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    // Resolve symlinks in the (now-existing) parent, keeping the leaf, so the link
    // file records the same absolute path git would (git real-paths the git dir).
    let real_abs = match (real_pb.parent(), real_pb.file_name()) {
        (Some(par), Some(leaf)) => std::fs::canonicalize(par)
            .map(|c| c.join(leaf))
            .unwrap_or_else(|_| real_pb.clone()),
        _ => real_pb.clone(),
    };

    std::fs::rename(src, &real_abs).map_err(|e| {
        anyhow::anyhow!(
            "unable to move {} to {}: {e}",
            src.display(),
            real_abs.display()
        )
    })?;
    std::fs::write(
        target.join(".git"),
        format!("gitdir: {}\n", real_abs.display()),
    )?;
    Ok(real_abs)
}

/// Seed `git_dir` from the template at `template`. Ports git's `copy_templates`
/// and `copy_templates_1`: entries whose name starts with `.` are skipped,
/// directories are merged, existing files are left untouched, symlinks are
/// recreated, and regular files are copied preserving their source mode.
///
/// gix already populated its built-in default template (sample hooks,
/// `info/exclude`, `description`) inside `git_dir`; git, given `--template`, uses
/// that dir *instead of* the default. So the default-template artifacts are
/// stripped first, letting the requested template fully define the
/// template-provided files while structural files (`HEAD`, `config`, `objects/`,
/// `refs/`) remain. Shared with `git clone --template=<dir>`, which git routes
/// through the very same `copy_templates()` call during its own init step.
pub(super) fn apply_template(template: &str, git_dir: &Path) -> Result<()> {
    // The strip happens before the template dir is even opened, because stock
    // git has no default template to fall back on: `copy_templates()` warns and
    // returns, leaving a repository with no `description`, no `info/exclude` and
    // no `hooks/` at all. Stripping only on success would leave gix's default in
    // place and answer `--template=no-such-dir` with a payload stock never wrote.
    strip_default_template(git_dir)?;
    copy_templates(template, git_dir)
}

/// The half of [`apply_template`] git's `copy_templates()` actually is: seed
/// `git_dir` from `template` without touching what is already there. This is the
/// whole of what a reinitialization does — the repository's existing hooks,
/// `description` and `info/exclude` stay exactly as they are, and only paths the
/// template names and the repository lacks are filled in.
pub(super) fn copy_templates(template: &str, git_dir: &Path) -> Result<()> {
    // `if (!template_dir[0]) goto free_return;` — an explicitly empty
    // `--template=` names no template and is not a warning.
    if template.is_empty() {
        return Ok(());
    }
    let src = {
        let p = PathBuf::from(template);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()?.join(p)
        }
    };
    // git warns and skips when the template dir cannot be opened.
    if std::fs::read_dir(&src).is_err() {
        eprintln!("warning: templates not found in {template}");
        return Ok(());
    }
    copy_template_dir(&src, git_dir)?;
    Ok(())
}

/// Remove gix's built-in default-template files so a `--template` dir can fully
/// replace them. Only the template-provided paths are touched
/// (`description`, `info/exclude` + the now-empty `info/`, and everything under
/// `hooks/` + the now-empty `hooks/`); structural files are left in place. Empty
/// directories are removed only when they end up empty, so a template that omits
/// them leaves them absent, matching git.
fn strip_default_template(git_dir: &Path) -> Result<()> {
    let _ = std::fs::remove_file(git_dir.join("description"));
    let _ = std::fs::remove_file(git_dir.join("info").join("exclude"));
    let _ = std::fs::remove_dir(git_dir.join("info"));

    let hooks = git_dir.join("hooks");
    if let Ok(entries) = std::fs::read_dir(&hooks) {
        for entry in entries {
            let _ = std::fs::remove_file(entry?.path());
        }
    }
    let _ = std::fs::remove_dir(&hooks);
    Ok(())
}

/// Recursively copy `src` into `dst` with git's `copy_templates_1` semantics.
fn copy_template_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let s = entry.path();
        let d = dst.join(&name);
        let meta = std::fs::symlink_metadata(&s)?;
        let ft = meta.file_type();
        if ft.is_dir() {
            copy_template_dir(&s, &d)?;
        } else if d.exists() {
            // git's copy_templates_1 never overwrites an existing file.
            continue;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&s)?, &d)?;
            #[cfg(not(unix))]
            {
                std::fs::copy(&s, &d)?;
            }
        } else if ft.is_file() {
            std::fs::copy(&s, &d)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &d,
                    std::fs::Permissions::from_mode(meta.permissions().mode()),
                )?;
            }
        }
    }
    Ok(())
}

/// Parse a `--shared=<value>` argument into git's `git_config_perm` result:
/// `umask`/`false` → 0; `group`/`true` → `0o660`; `all`/`world`/`everybody` →
/// `0o664`; a `0`/`1`/`2` compatibility number → 0/`0o660`/`0o664`; any other
/// octal `0xxx` file mode → `-(mode & 0o666)` (stored negated). An octal mode
/// that would deny the owner read+write is rejected, exactly like git.
fn parse_shared_value(value: &str) -> Result<i32> {
    match value {
        "umask" => return Ok(0),
        "group" => return Ok(0o660),
        "all" | "world" | "everybody" => return Ok(0o664),
        _ => {}
    }
    match parse_octal_full(value) {
        Some(0) => Ok(0),
        Some(1) => Ok(0o660),
        Some(2) => Ok(0o664),
        Some(mode) => {
            if (mode & 0o600) != 0o600 {
                crate::git_fatal!(
                    "problem with core.sharedRepository filemode value (0{mode:03o}).\n\
                     The owner of files must always have read and write permissions."
                );
            }
            Ok(-(mode & 0o666))
        }
        // Not an octal number: fall back to boolean, like git_config_bool —
        // which is `git_parse_maybe_bool` plus a `die()` on anything it cannot
        // read, not a silent "false". The variable name in the message is
        // literally `arg`, because `git_config_perm("arg", arg)` is how
        // `cmd_init_db`'s `--shared` callback spells it.
        None => match parse_maybe_bool(value) {
            Some(true) => Ok(0o660),
            Some(false) => Ok(0),
            None => crate::git_fatal!("bad boolean config value '{value}' for 'arg'"),
        },
    }
}

/// Whole-string octal parse mirroring C's `strtol(value, &endptr, 8)` with
/// `*endptr == 0`: an empty string is 0 (as `strtol` reports), a fully-octal
/// string is its value, anything else is `None` (falls through to boolean).
fn parse_octal_full(s: &str) -> Option<i32> {
    if s.is_empty() {
        return Some(0);
    }
    if s.bytes().all(|b| b.is_ascii_digit() && b <= b'7') {
        i32::from_str_radix(s, 8).ok()
    } else {
        None
    }
}

/// Port of `git_parse_maybe_bool()` (`config.c`): the three truthy and three
/// falsy spellings, the empty string as false, and — failing those — a plain
/// integer read as C's `!!value`. `None` is git's `-1`, the answer that makes
/// `git_config_bool()` die.
fn parse_maybe_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "" => Some(false),
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        _ => s.parse::<i64>().ok().map(|v| v != 0),
    }
}

/// Write `core.sharedrepository` and `receive.denyNonFastforwards` into the git
/// dir's config, porting the config write in git's `create_default_files`.
/// The stored value uses git's compatibility encoding: `1` for group, `2` for
/// everybody, `0xxx` for an explicit file mode.
fn write_shared_config(git_dir: &Path, shared: i32) -> Result<()> {
    let value = if shared < 0 {
        format!("0{:o}", -shared)
    } else if shared == 0o660 {
        "1".to_string()
    } else if shared == 0o664 {
        "2".to_string()
    } else {
        crate::git_fatal!("invalid value for shared repository");
    };

    let path = config_path(git_dir);
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    file.set_raw_value_by("core", None, "sharedrepository", value.as_str())?;
    file.set_raw_value_by("receive", None, "denyNonFastforwards", "true")?;
    std::fs::write(&path, file.to_bstring())?;
    Ok(())
}

/// git's `FORCE_DIR_SET_GID`: `git-compat-util.h` defaults it to `S_ISGID`
/// (`#ifndef FORCE_DIR_SET_GID #define FORCE_DIR_SET_GID S_ISGID`), so a shared
/// directory that grants any group access is forced set-gid. No config.mak.uname
/// entry for the platforms zvcs targets (Darwin, Linux) undefines it — verified
/// against stock git, which stamps `.git/` `2775` under `--shared=group`.
#[cfg(unix)]
const FORCE_DIR_SET_GID: bool = true;

/// Port of git's `calc_shared_perm` (`path.c`): widen `mode` according to the
/// stored shared value. Positive values OR in extra bits; a negative value forces
/// the low 9 bits to the requested file mode.
#[cfg(unix)]
fn calc_shared_perm(shared: i32, mode: u32) -> u32 {
    const S_IWUSR: u32 = 0o200;
    const S_IXUSR: u32 = 0o100;

    let mut tweak: i32 = if shared < 0 { -shared } else { shared };
    if mode & S_IWUSR == 0 {
        tweak &= !0o222;
    }
    if mode & S_IXUSR != 0 {
        // Copy read bits to execute bits.
        tweak |= (tweak & 0o444) >> 2;
    }
    let mode = mode as i32;
    let new = if shared < 0 {
        (mode & !0o777) | tweak
    } else {
        mode | tweak
    };
    new as u32
}

/// Port of git's `adjust_shared_perm` (`path.c`), applied recursively so the
/// whole git dir git built up file-by-file ends up with the same modes. For
/// directories, read bits are copied to execute bits and — where git does — the
/// set-gid bit is forced when any group access is granted. Symlinks are left
/// untouched (git init creates none, and chmod through them is undesirable).
#[cfg(unix)]
fn adjust_shared_perm_recursive(path: &Path, shared: i32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    const S_ISGID: u32 = 0o2000;

    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let old = meta.permissions().mode();
    let is_dir = meta.is_dir();

    let mut new = calc_shared_perm(shared, old);
    if is_dir {
        new |= (new & 0o444) >> 2;
        if FORCE_DIR_SET_GID && (new & 0o60) != 0 {
            new |= S_ISGID;
        }
    }

    if (old & 0o7777) != (new & 0o7777) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(new & 0o7777))?;
    }

    if is_dir {
        for entry in std::fs::read_dir(path)? {
            adjust_shared_perm_recursive(&entry?.path(), shared)?;
        }
    }
    Ok(())
}

/// Render a git-dir path the way stock git does in the init message: an absolute,
/// symlink-resolved path with a trailing slash. Falls back to the given path when
/// canonicalization is unavailable (should not happen for a just-created dir).
fn display_git_dir(git_dir: &Path) -> String {
    let abs = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    format!("{}/", abs.display())
}

/// Port of `real_pathdup(path, 1)` (`abspath.c`) as `cmd_init_db` uses it for
/// `--separate-git-dir` and `--template`: resolve `path` against the current
/// directory and die on a path git cannot resolve. The empty string is refused
/// outright, matching `strbuf_realpath`'s first test.
fn real_pathdup(path: &str) -> Result<String> {
    if path.is_empty() {
        crate::git_fatal!("The empty string is not a valid path");
    }
    let base = std::env::current_dir()?;
    let full = match Path::new(path).is_absolute() {
        true => PathBuf::from(path),
        false => base.join(path),
    };
    Ok(real_path(full)?.to_string_lossy().into_owned())
}

/// Port of `strbuf_realpath(&out, path, die_on_error=1)` (`abspath.c`): walk the
/// already-absolute `path` one component at a time, resolving symlinks as they
/// are met. A component that does not exist is tolerated only as the *last* one
/// ("error out unless this was the last component"); anything earlier is
/// `die_errno(_("Invalid path '%s'"), resolved)` with only the part resolved so
/// far named, which is the diagnostic `git init --separate-git-dir=sub/gd sub`
/// produces before `sub` exists.
fn real_path(path: PathBuf) -> Result<PathBuf> {
    let components: Vec<_> = path.components().collect();
    let last = components.len().saturating_sub(1);
    let mut resolved = PathBuf::new();
    for (i, component) in components.iter().enumerate() {
        match component {
            std::path::Component::Prefix(p) => {
                resolved.push(p.as_os_str());
                continue;
            }
            std::path::Component::RootDir => {
                resolved.push(std::path::MAIN_SEPARATOR_STR);
                continue;
            }
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => {
                resolved.pop();
                continue;
            }
            std::path::Component::Normal(name) => resolved.push(name),
        }
        match std::fs::symlink_metadata(&resolved) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if let Ok(target) = std::fs::canonicalize(&resolved) {
                    resolved = target;
                }
            }
            Ok(_) => {}
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound || i != last {
                    crate::git_fatal!(
                        "Invalid path '{}': {}",
                        resolved.display(),
                        crate::external::strerror(&e)
                    );
                }
            }
        }
    }
    Ok(resolved)
}

/// Port of `guess_repository_type()` (`builtin/init-db.c`): with no `--bare` on
/// the command line, the git directory's own spelling decides whether the
/// repository is bare. `.`, the current directory itself, `.git` and `*/.git`
/// are worktree repositories; everything else "is often bare … at this point we
/// are just guessing".
fn guess_repository_type(git_dir: &str, cwd: &Path) -> bool {
    if git_dir == "." {
        return true;
    }
    if Path::new(git_dir) == cwd {
        return true;
    }
    if git_dir == ".git" {
        return false;
    }
    !git_dir.ends_with("/.git")
}

/// Port of `read_gitfile()` (`setup.c`) as `repo_set_gitdir()` calls it: a
/// regular file whose only line is `gitdir: <path>` names the real git
/// directory, relative paths being taken from the link file's own directory.
/// Anything else — a directory, a missing path, a file with another shape — is
/// not a gitfile.
fn read_gitfile(path: &Path) -> Option<PathBuf> {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let target = text.trim_end_matches(['\n', '\r']).strip_prefix("gitdir: ")?;
    let target = PathBuf::from(target);
    match target.is_absolute() {
        true => Some(target),
        false => Some(path.parent()?.join(target)),
    }
}

/// Port of `separate_git_dir()` (`setup.c`) for the reinitialization case: an
/// existing git directory named by `link` (either the directory itself or a
/// `gitdir:` file pointing at it) is moved to `real`, and a fresh `gitdir:` link
/// file is written in its place. A `link` that does not exist yet leaves nothing
/// to move — the caller lays the repository down at `real` and this only writes
/// the link.
fn migrate_git_dir(real: &Path, link: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(link) {
        let src = if meta.is_file() {
            match read_gitfile(link) {
                Some(p) => p,
                None => crate::git_fatal!("unable to handle file type {}", link.display()),
            }
        } else if meta.is_dir() {
            link.to_path_buf()
        } else {
            crate::git_fatal!("unable to handle file type {}", link.display());
        };
        if let Some(parent) = real.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = std::fs::rename(&src, real) {
            crate::git_fatal!(
                "unable to move {} to {}: {}",
                src.display(),
                real.display(),
                crate::external::strerror(&e)
            );
        }
    }
    std::fs::write(link, format!("gitdir: {}\n", real.display()))?;
    Ok(())
}

/// The object format an existing repository records, as
/// `check_repository_format()` reads it: `extensions.objectFormat` when the
/// config carries one, else git's compiled-in `sha1`.
fn repository_object_format(git_dir: &Path) -> String {
    local_config(git_dir)
        .and_then(|f| {
            f.string_by("extensions", None, "objectFormat")
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| "sha1".to_string())
}

/// `core.sharedRepository` as `git_default_core_config` parses it into
/// `get_shared_repository()`. `0`/`umask` reads as "not shared", which is the
/// same falsy answer as an absent key, so it is reported as `None`.
fn configured_shared(git_dir: &Path) -> Option<i32> {
    let value = local_config(git_dir)?
        .string_by("core", None, "sharedRepository")
        .map(|v| v.to_string())?;
    parse_shared_value(&value).ok().filter(|&s| s != 0)
}

/// The repository's own `config` file, parsed without following `include`s —
/// the same file every write below edits in place.
fn local_config(git_dir: &Path) -> Option<gix::config::File> {
    gix::config::File::from_path_no_includes(config_path(git_dir), gix::config::Source::Local).ok()
}

/// Port of `get_common_dir()` (`setup.c`): a linked worktree's git directory
/// carries a `commondir` file naming the repository every worktree shares, and
/// the configuration, the object store and the ref database all live there
/// rather than in the per-worktree directory. Without that file the git
/// directory is its own common directory.
fn common_dir(git_dir: &Path) -> PathBuf {
    let Ok(text) = std::fs::read_to_string(git_dir.join("commondir")) else {
        return git_dir.to_path_buf();
    };
    let target = PathBuf::from(text.trim_end_matches(['\n', '\r']));
    match target.is_absolute() {
        true => target,
        false => git_dir.join(target),
    }
}

/// The config file `git_config_set()` writes: the shared one when `git_dir` is a
/// linked worktree, its own otherwise.
fn config_path(git_dir: &Path) -> PathBuf {
    common_dir(git_dir).join("config")
}

/// `git_config_set(<section>.<key>, <value>)` against the repository's own
/// config, skipped when the file already spells that value — so a reinit that
/// changes nothing rewrites nothing.
fn set_config_value(git_dir: &Path, section: &str, key: &str, value: &str) -> Result<()> {
    let path = config_path(git_dir);
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    if file
        .string_by(section, None, key)
        .is_some_and(|v| v.to_string() == value)
    {
        return Ok(());
    }
    file.set_raw_value_by(section, None, key, value)?;
    std::fs::write(&path, file.to_bstring())?;
    Ok(())
}

/// `if (log_all_ref_updates == LOG_REFS_UNSET) git_config_set(...)`: seed a key
/// only when the configuration does not define it at all, leaving an explicit
/// value — including an explicit `false` — alone.
fn set_config_default(git_dir: &Path, section: &str, key: &str, value: &str) -> Result<()> {
    let path = config_path(git_dir);
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    if file.string_by(section, None, key).is_some() {
        return Ok(());
    }
    file.set_raw_value_by(section, None, key, value)?;
    std::fs::write(&path, file.to_bstring())?;
    Ok(())
}

/// Port of `create_object_directory()` (`setup.c`): the object store is
/// `<git-dir>/objects` unless `GIT_OBJECT_DIRECTORY` names another, in which
/// case git creates *only* that one — `info/` and `pack/` inside it and no
/// `<git-dir>/objects` at all. gix always lays the default store down, so the
/// unwanted one is removed here (only when empty, so an existing store is never
/// destroyed).
fn create_object_directory(git_dir: &Path, cwd: &Path) -> Result<()> {
    let default = common_dir(git_dir).join("objects");
    let objects = match std::env::var_os("GIT_OBJECT_DIRECTORY") {
        Some(v) if !v.is_empty() => {
            let p = PathBuf::from(v);
            match p.is_absolute() {
                true => p,
                false => cwd.join(p),
            }
        }
        _ => default.clone(),
    };
    if objects != default {
        let _ = std::fs::remove_dir(default.join("info"));
        let _ = std::fs::remove_dir(default.join("pack"));
        let _ = std::fs::remove_dir(&default);
    }
    std::fs::create_dir_all(objects.join("info"))?;
    std::fs::create_dir_all(objects.join("pack"))?;
    Ok(())
}

/// `init.templateDir` read as a pathname out of whichever configuration is at
/// hand: the repository just created, or — on a reinitialization, where there is
/// no freshly-opened handle — the existing repository on disk.
fn configured_template_dir(repo: Option<&gix::Repository>, git_dir: &Path) -> Option<String> {
    fn read(repo: &gix::Repository) -> Option<String> {
        repo.config_snapshot()
            .trusted_path("init.templateDir")
            .ok()
            .flatten()
            .map(|p| p.to_string_lossy().into_owned())
    }
    match repo {
        Some(repo) => read(repo),
        None => {
            let repo = gix::open_opts(
                git_dir,
                gix::open::Options::isolated().open_path_as_is(true),
            )
            .ok()?;
            read(&repo)
        }
    }
}
