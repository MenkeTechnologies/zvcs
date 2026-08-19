//! zvcs — the git-shadowing superset engine, as a library.
//!
//! The `git` binary (`src/main.rs`) is a thin entry point over [`run`]. Exposing
//! the engine as a library lets integration tests drive the coordination layer
//! (e.g. [`lock::RepoLock`] against a live `zdaemon`) directly.

pub mod abbrev;
pub mod advice;
pub mod alias;
pub mod autocorrect;
pub mod autostart;
pub mod config;
pub mod crawler;
pub mod date;
pub mod db;
pub mod diffopt;
pub mod dispatch;
pub mod external;
pub mod fatal;
pub mod gitsig;
pub mod hooks;
pub mod index_commit;
pub mod jobpool;
pub mod jobrun;
pub mod lock;
pub mod merge_apply;
pub mod merge_guard;
pub mod merge_ws;
pub mod objname;
pub mod optint;
pub mod pager;
pub mod parseopt;
pub mod pathspec;
pub mod porcelain;
pub mod precompose;
pub mod progress;
pub mod quote;
pub mod rcache;
pub mod refname;
pub mod refsort;
pub mod revfilter;
pub mod sequencer;
pub mod setup;
pub mod shallow_serve;
pub mod showdate;
pub mod sigpipe;
pub mod superset;
pub mod threads;
pub mod transport_err;
pub mod trace2;
pub mod unicode_width;
pub mod utf8;
pub mod worktree;

use std::process::ExitCode;

/// Parse `argv`, dispatch the subcommand, and return the process exit code.
/// Errors are reported terse on stderr as `zvcs: <command>: <reason>`.
///
/// Wraps [`run_command`] in Trace2's session bracket, so the `start` record is
/// written before any argument is looked at and the `exit`/`atexit` pair covers
/// every path out — including the ones that return early. Both calls are inert
/// unless a Trace2 event target is configured.
pub fn run() -> ExitCode {
    trace2::start(&std::env::args().collect::<Vec<_>>());
    let code = run_command();
    trace2::exit(exit_status(code));
    code
}

/// The numeric status an [`ExitCode`] will hand the shell.
///
/// `ExitCode` exposes no accessor on stable Rust — only `From<u8>` and equality
/// — so the value is recovered by finding the one byte that constructs an equal
/// code. There are 256 candidates and the scan runs once per process, at exit.
fn exit_status(code: ExitCode) -> i32 {
    (0u8..=255).find(|&n| code == ExitCode::from(n)).map_or(1, i32::from)
}

/// `handle_options()`'s `-C <path>` branch (git.c), shared with the copy that
/// runs inside `run_argv`'s alias loop ([`alias::resolve`]).
///
/// Two details of the C are easy to lose. The chdir is guarded by
/// `if ((*argv)[1][0])`, so an *empty* path is a deliberate no-op — `git -C ""`
/// succeeds and stays put. And the failure is `die_errno("cannot change to
/// '%s'", …)`, so the message carries the bare `strerror` text and the caller
/// exits 128, not 1.
///
/// Returns the `die_errno` message (without git's `fatal: ` prefix) on failure.
pub fn chdir_global(dir: &str) -> Result<(), String> {
    if dir.is_empty() {
        return Ok(());
    }
    std::env::set_current_dir(dir)
        .map_err(|e| format!("cannot change to '{dir}': {}", errno_text(&e)))
}

/// `usage(git_usage_string)` after one of `handle_options`' "no value given"
/// complaints: the reason on stderr, then git's top-level usage block, exit 129.
///
/// git prints the reason with a plain `fprintf(stderr, …)` — no `fatal: ` and no
/// `error: ` prefix — and `usage()` follows it with `usage: <git_usage_string>`.
fn usage_missing_value(reason: &str) -> ExitCode {
    eprintln!("{reason}");
    eprintln!("usage: {}", porcelain::help::GIT_USAGE_STRING);
    ExitCode::from(129)
}

/// One `git_config_push_split_parameter()` pair — a command-line configuration
/// override from `-c <name>[=<value>]` or `--config-env <name>=<envvar>`.
///
/// `value` is `None` for the bool-only `-c <name>` form, which git records with
/// no `=` in `GIT_CONFIG_PARAMETERS` and reads back as true.
pub struct ConfigOverride {
    key: String,
    value: Option<String>,
}

/// What one pass of [`handle_options`] left behind.
pub enum Handled {
    /// The number of leading tokens consumed; the command begins after them.
    Consumed(usize),
    /// The pass finished the process' work — a query global that printed and
    /// exits, or a failure it already reported. Leave with this code.
    Exit(ExitCode),
}

/// Faithful port of git.c's `handle_options()`, the loop that consumes the
/// git-global options ahead of the subcommand.
///
/// git runs this same function twice: once over the command line in `cmd_main`,
/// and again over every alias expansion inside `run_argv`'s loop — which is why
/// it lives here rather than being written once per caller. The alias caller is
/// the one that passes a non-null `envchanged` in the C, and it is set by every
/// option that reaches a child process through the environment; `handle_alias`
/// then refuses the alias outright ([`alias::resolve`]).
///
/// Four details of the C shape the control flow:
///
/// * The loop stops at the first token that does not start with `-`, and also at
///   `-h`/`--help`/`-v`/`--version`, which `cmd_main` rewrites into the `help`
///   and `version` verbs afterwards. Anything else beginning with `-` is either
///   a known global or `unknown option: <x>` + usage, exit 129 — there is no
///   "leave it for the dispatcher" path. `--super-prefix` was one of the known
///   globals until 2.45 removed it, so on 2.55 it is an unknown option like any
///   other.
/// * `-c` is *pushed*, not parsed: git validates a command-line override only
///   when the configuration is first read, which is why `git -c foo --version`
///   succeeds. See [`validate_config_overrides`].
/// * `--exec-path` is a `skip_prefix` test, so it also matches spellings that
///   merely *begin* with it; only `=` after the prefix means "set". The bare
///   option, `--html-path`, `--man-path` and `--info-path` print one directory
///   and exit 0, ignoring everything after them.
/// * The loop can consume the entire command line. `--shallow-file` takes the
///   next token unconditionally (`git --shallow-file log` uses `log` as the
///   file name), and `--bare` needs no argument at all, so reaching the end
///   with no verb left is a normal outcome — `cmd_main` prints the usage block
///   and exits 1 for it.
///
/// One branch of the C is deliberately absent: `--list-cmds=<what>`, which git
/// answers from its `commands[]` table plus `command-list.txt` categories. This
/// port's verb set is not git's, so the answer could not match either way; the
/// option currently falls through to `unknown option`.
pub fn handle_options(
    argv: &[String],
    pager_forced: &mut Option<bool>,
    overrides: &mut Vec<ConfigOverride>,
    envchanged: &mut bool,
) -> Handled {
    let mut idx = 0;
    while idx < argv.len() {
        let cmd = argv[idx].as_str();
        if !cmd.starts_with('-') {
            break;
        }
        // "For legacy reasons, the version and help commands can be written
        // with -- prepended to make them look like flags."
        if matches!(cmd, "--help" | "-h" | "--version" | "-v") {
            break;
        }

        match cmd {
            // `skip_prefix(cmd, "--exec-path", &cmd)` is a *prefix* test, and the
            // only thing looked at afterwards is whether the remainder starts with
            // `=`. So `--exec-path=<dir>` sets the directory, and every other
            // spelling that merely begins with `--exec-path` — the bare option and
            // `--exec-pathZZZ` alike — prints the directory and exits 0 with the
            // remainder ignored. It is the first branch of the chain, so no later
            // option can be shadowed by the prefix.
            s if s.starts_with("--exec-path") => {
                match s["--exec-path".len()..].strip_prefix('=') {
                    Some(dir) => {
                        std::env::set_var("GIT_EXEC_PATH", dir);
                        idx += 1;
                    }
                    None => {
                        println!("{}", exec_path());
                        return Handled::Exit(ExitCode::SUCCESS);
                    }
                }
            }
            // The three `print_system_path()` queries beside it in
            // `handle_options`: each prints one directory of the installation and
            // exits, ignoring whatever follows. git's prefix is its build's; this
            // port's is `$ZVCS_HOME`, which is where its man pages, HTML pages and
            // info tree are installed — the same paths `git help -m/-w/-i` resolve
            // against, so a scripted `git --html-path` and `git help -w` agree.
            //
            // `--html-path` is the one that can name a directory outside that
            // prefix: `git help -w <cmd>` opens the git installation's own HTML
            // manual where the host has one, so that is the directory reported,
            // and the generated set only when it does not.
            "--html-path" => {
                println!("{}", superset::htmldoc::reported_dir().display());
                return Handled::Exit(ExitCode::SUCCESS);
            }
            "--man-path" => {
                println!("{}", superset::manpage::man_dir().display());
                return Handled::Exit(ExitCode::SUCCESS);
            }
            "--info-path" => {
                println!("{}", porcelain::help::info_dir().display());
                return Handled::Exit(ExitCode::SUCCESS);
            }
            "-p" | "--paginate" => {
                *pager_forced = Some(true);
                idx += 1;
            }
            "-P" | "--no-pager" => {
                *pager_forced = Some(false);
                *envchanged = true;
                idx += 1;
            }
            "--no-replace-objects" => {
                std::env::set_var("GIT_NO_REPLACE_OBJECTS", "1");
                *envchanged = true;
                idx += 1;
            }
            // `--git-dir`/`--work-tree`/`--namespace`/`--attr-source` set the
            // well-known environment variables, in both the `--flag <val>` and
            // `--flag=<val>` forms. Each is its own arm in the C with its own
            // "no value given" wording, which is what the table reproduces.
            "--git-dir" | "--work-tree" | "--namespace" | "--attr-source" => {
                let (key, missing) = match cmd {
                    "--git-dir" => ("GIT_DIR", "no directory given for '--git-dir' option"),
                    "--work-tree" => ("GIT_WORK_TREE", "no directory given for '--work-tree' option"),
                    "--namespace" => ("GIT_NAMESPACE", "no namespace given for --namespace"),
                    _ => ("GIT_ATTR_SOURCE", "no attribute source given for --attr-source"),
                };
                let Some(val) = argv.get(idx + 1) else {
                    return Handled::Exit(usage_missing_value(missing));
                };
                std::env::set_var(key, val);
                *envchanged = true;
                idx += 2;
            }
            s if s.starts_with("--git-dir=") => {
                std::env::set_var("GIT_DIR", &s["--git-dir=".len()..]);
                *envchanged = true;
                idx += 1;
            }
            s if s.starts_with("--work-tree=") => {
                std::env::set_var("GIT_WORK_TREE", &s["--work-tree=".len()..]);
                *envchanged = true;
                idx += 1;
            }
            s if s.starts_with("--namespace=") => {
                std::env::set_var("GIT_NAMESPACE", &s["--namespace=".len()..]);
                *envchanged = true;
                idx += 1;
            }
            s if s.starts_with("--attr-source=") => {
                std::env::set_var("GIT_ATTR_SOURCE", &s["--attr-source=".len()..]);
                *envchanged = true;
                idx += 1;
            }
            // `--bare` names the *current directory* as the repository, with
            // `setenv(…, 0)` so an inherited `$GIT_DIR` still wins, and turns off
            // the implicit work tree. git dies from `xgetcwd()` when the cwd
            // cannot be read, which is `die_errno` and so 128.
            "--bare" => {
                let cwd = match std::env::current_dir() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("fatal: unable to get current working directory: {}", errno_text(&e));
                        return Handled::Exit(ExitCode::from(fatal::EXIT_FATAL));
                    }
                };
                if std::env::var_os("GIT_DIR").is_none() {
                    std::env::set_var("GIT_DIR", cwd);
                }
                std::env::set_var("GIT_IMPLICIT_WORK_TREE", "0");
                *envchanged = true;
                idx += 1;
            }
            "-c" => {
                let Some(text) = argv.get(idx + 1) else {
                    return Handled::Exit(usage_missing_value("-c expects a configuration string"));
                };
                // `git_config_push_parameter()`: split at the *first* `=`, so a
                // value keeps every later one; a missing `=` is the bool-only form.
                let (key, value) = match text.split_once('=') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (text.clone(), None),
                };
                push_config_override(overrides, ConfigOverride { key, value });
                idx += 2;
            }
            // `--config-env <name>=<envvar>` reads the value out of the
            // environment instead of the command line. Unlike `-c` its three
            // failures are `die()` at push time, so they are reported here and
            // exit 128 — and the *key* is still left for the deferred check.
            "--config-env" => {
                let Some(spec) = argv.get(idx + 1) else {
                    return Handled::Exit(usage_missing_value("no config key given for --config-env"));
                };
                if let Some(code) = push_config_env(overrides, &spec) {
                    return Handled::Exit(code);
                }
                idx += 2;
            }
            s if s.starts_with("--config-env=") => {
                if let Some(code) = push_config_env(overrides, &s["--config-env=".len()..]) {
                    return Handled::Exit(code);
                }
                idx += 1;
            }
            "--literal-pathspecs" => {
                std::env::set_var("GIT_LITERAL_PATHSPECS", "1");
                *envchanged = true;
                idx += 1;
            }
            "--no-literal-pathspecs" => {
                std::env::set_var("GIT_LITERAL_PATHSPECS", "0");
                *envchanged = true;
                idx += 1;
            }
            "--glob-pathspecs" => {
                std::env::set_var("GIT_GLOB_PATHSPECS", "1");
                *envchanged = true;
                idx += 1;
            }
            "--noglob-pathspecs" => {
                std::env::set_var("GIT_NOGLOB_PATHSPECS", "1");
                *envchanged = true;
                idx += 1;
            }
            "--icase-pathspecs" => {
                std::env::set_var("GIT_ICASE_PATHSPECS", "1");
                *envchanged = true;
                idx += 1;
            }
            "--no-optional-locks" => {
                std::env::set_var("GIT_OPTIONAL_LOCKS", "0");
                *envchanged = true;
                idx += 1;
            }
            "--no-lazy-fetch" => {
                std::env::set_var("GIT_NO_LAZY_FETCH", "1");
                *envchanged = true;
                idx += 1;
            }
            "--no-advice" => {
                std::env::set_var("GIT_ADVICE", "0");
                *envchanged = true;
                idx += 1;
            }
            // `--shallow-file <path>` overrides the repository's shallow list for
            // this process only — git sets it on `the_repository` rather than in
            // the environment, so there is nothing to hand a child. The argument
            // is consumed the way the C consumes it so the verb after it is still
            // found; the override itself is not honored yet.
            //
            // The C takes the next token without checking that there is one:
            // `git --shallow-file` alone reads past the end of `argv` and stock
            // 2.55.0 dies of the signal (observed exit 139). The bounds test here
            // is the one deliberate departure — a NULL dereference is not a
            // behaviour to reproduce — and leaves the command line empty, which
            // `cmd_main` answers with the usage block and exit 1.
            "--shallow-file" => {
                *envchanged = true;
                idx += if idx + 1 < argv.len() { 2 } else { 1 };
            }
            // `-C <path>` chdirs, guarded by `if ((*argv)[1][0])` — an empty path
            // is a deliberate no-op that succeeds and stays put, and does *not*
            // count as an environment change for an alias. The failure is
            // `die_errno`, so it carries the bare `strerror` text and exits 128.
            "-C" => {
                let Some(dir) = argv.get(idx + 1) else {
                    return Handled::Exit(usage_missing_value("no directory given for '-C' option"));
                };
                if !dir.is_empty() {
                    if let Err(msg) = chdir_global(dir) {
                        eprintln!("fatal: {msg}");
                        return Handled::Exit(ExitCode::from(fatal::EXIT_FATAL));
                    }
                    *envchanged = true;
                }
                idx += 2;
            }
            _ => {
                eprintln!("unknown option: {cmd}");
                eprintln!("usage: {}", porcelain::help::GIT_USAGE_STRING);
                return Handled::Exit(ExitCode::from(129));
            }
        }
    }
    Handled::Consumed(idx)
}

/// `strerror(errno)` without the ` (os error <n>)` tail Rust appends, so a
/// `die_errno` message reads exactly as git's does.
fn errno_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error ").next().unwrap_or(&text).to_string()
}

/// Port of config.c's `git_config_push_env()`: `--config-env <name>=<envvar>`
/// takes the value from the environment, splitting at the **last** `=` so the
/// key may contain one.
///
/// All three failures are `die()`, so they are reported here and exit 128. The
/// key itself is not checked — git validates it when the configuration is read,
/// which [`validate_config_overrides`] does.
fn push_config_env(overrides: &mut Vec<ConfigOverride>, spec: &str) -> Option<ExitCode> {
    let Some(split) = spec.rfind('=') else {
        eprintln!("fatal: invalid config format: {spec}");
        return Some(ExitCode::from(fatal::EXIT_FATAL));
    };
    let (key, env_name) = (&spec[..split], &spec[split + 1..]);
    if env_name.is_empty() {
        eprintln!("fatal: missing environment variable name for configuration '{key}'");
        return Some(ExitCode::from(fatal::EXIT_FATAL));
    }
    let Some(value) = std::env::var_os(env_name) else {
        eprintln!("fatal: missing environment variable '{env_name}' for configuration '{key}'");
        return Some(ExitCode::from(fatal::EXIT_FATAL));
    };
    push_config_override(
        overrides,
        ConfigOverride {
            key: key.to_string(),
            value: Some(value.to_string_lossy().into_owned()),
        },
    );
    None
}

/// git's deferred validation of the command-line configuration, run at the point
/// the configuration would first be read: `error: <reason>` for the first key
/// that fails, then `fatal: unable to parse command-line config`, exit 128.
///
/// git has no such point — `git_config_from_parameters()` is called by whatever
/// reads config first, so a command that never reads any (`git -c foo version`,
/// `git -c foo --exec-path`) never notices a bad key. This port has no single
/// place where config is read either, so the check runs once, here, after the
/// `version`/`help` rewrite that `cmd_main` does; `version` and a bare `help`
/// are the two verbs it exempts, both confirmed silent under stock 2.55.0.
pub(crate) fn validate_config_overrides(
    overrides: &[ConfigOverride],
    sub: &str,
    rest: &[String],
) -> Option<ExitCode> {
    if sub == "version" || (sub == "help" && rest.is_empty()) {
        return None;
    }
    report_bad_config_overrides(overrides)
}

/// The diagnostic half of [`validate_config_overrides`], for the places that
/// read configuration unconditionally rather than only when the command does.
///
/// [`alias::resolve`] is the caller: `alias_lookup()` reads the configuration
/// on every turn of `run_argv`'s loop whatever the command turns out to be, so
/// there is no verb to exempt there.
pub(crate) fn report_bad_config_overrides(overrides: &[ConfigOverride]) -> Option<ExitCode> {
    let reason = overrides
        .iter()
        .find_map(|o| config::check_config_key(&o.key).err())?;
    eprintln!("error: {reason}");
    eprintln!("fatal: unable to parse command-line config");
    Some(ExitCode::from(fatal::EXIT_FATAL))
}

/// Parse `argv`, dispatch the subcommand, and return the process exit code.
/// Errors are reported terse on stderr as `zvcs: <command>: <reason>`.
fn run_command() -> ExitCode {
    // Dashed invocation: run as `git-<verb>` (a symlink in `~/.zvcs/bin`, or any
    // `git-*` on PATH) and git dispatches `<verb>` — git.c strips the `git-` prefix
    // from argv[0]. We fold it in by prepending the verb to the argument list;
    // `from_dashed` then suppresses external re-dispatch of that verb (it would
    // re-exec this same binary and loop). No git-global option layer applies to a
    // dashed form — `git-add -C x` is git-add's own `-C`, not the wrapper's.
    let from_dashed = dashed_subcommand(&std::env::args().next().unwrap_or_default());
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(verb) = &from_dashed {
        raw.insert(0, verb.clone());
    }
    let from_dashed = from_dashed.is_some();

    // git.c's `handle_options()` — the git-global option layer that runs before
    // any subcommand is dispatched. `-C <dir>` chdirs here (before autostart /
    // failure-surfacing, which key off the cwd), the pager flags force paging on
    // or off, and the environment-carried globals are exported for the verb and
    // for any child it runs. Anything else that begins with `-`, other than the
    // `-h`/`--help`/`-v`/`--version` legacy spellings the loop stops at, is
    // `unknown option: <x>` + usage, exit 129.
    let mut pager_forced: Option<bool> = None;
    let mut config_overrides: Vec<ConfigOverride> = Vec::new();
    // Only the alias caller looks at this; on the command line an option that
    // changes the environment is exactly what the user asked for.
    let mut envchanged = false;
    let idx = match handle_options(&raw, &mut pager_forced, &mut config_overrides, &mut envchanged) {
        Handled::Consumed(n) => n,
        Handled::Exit(code) => return code,
    };
    let args = &raw[idx..];

    // `cmd_main`'s `if (!argc)` — nothing left after the option layer, so the
    // user gets the same three `printf`s `git help` ends with (the synopsis, the
    // common command groups, the trailer) on **stdout**, and the process leaves
    // with 1. `handle_options` can consume the whole command line without ever
    // reaching a verb — `git --bare`, `git -c x.y=1`, `git --shallow-file log`
    // (the shallow path swallows the token after it) — so this is not only the
    // bare `git` case.
    let Some(sub) = args.first() else {
        if let Err(e) = porcelain::help(&[]) {
            eprintln!("zvcs: help: {e:#}");
        }
        return ExitCode::FAILURE;
    };

    // Faithful port of `cmd_main()` in git.c: `handle_options()` breaks out early
    // on `-v`/`--version`/`-h`/`--help`, then `cmd_main` rewrites the command token
    // (`argv[0] = "version"` / `argv[0] = "help"`) before dispatch. Without this,
    // `git --version` reaches the dispatch table as an unknown verb and errors
    // "is not a git command" instead of printing the version.
    let sub = match sub.as_str() {
        "--version" | "-v" => "version",
        "--help" | "-h" => "help",
        other => other,
    };
    let rest = &args[1..];

    // The verb is known, so the deferred check on any `-c`/`--config-env` key can
    // run — this is git's `git_config_from_parameters()` reporting a malformed
    // command-line override the first time configuration is read.
    if let Some(code) = validate_config_overrides(&config_overrides, sub, rest) {
        return code;
    }

    // Surface any headless autonomous-op failures recorded since last time, on
    // this next `git` invocation. Async/daemon failures carry no exit code back,
    // so this at-least-once notification is their only channel. stderr only, so
    // `$(git …)` capture stays clean. Skipped for `zdaemon` to avoid self-noise.
    if sub != "zdaemon" {
        surface_pending_failures();
    }

    // Bring up the singleton coordinator when `[zvcs]` autonomy is configured, so
    // the user never starts it by hand. Skipped for `zdaemon` (it would self-race).
    if sub != "zdaemon" {
        autostart::ensure_if_configured();
    }

    // Resolve gitconfig `alias.<cmd>` before paging and dispatch, mirroring git's
    // run_argv: a real verb wins over a same-named alias, otherwise the alias is
    // expanded (recursively) and a `!shell` alias is run directly. Done before
    // the pager so paging keys off the resolved command, not the alias name.
    //
    // `typed` is `cmd_main`'s `cmd`, the verb as it stood before any expansion.
    // It is the name the alias-failure diagnostic below names, whatever depth the
    // chain reached.
    let typed = sub.to_string();
    let mut was_alias = false;
    let (sub, rest): (String, Vec<String>) =
        match alias::resolve(sub, rest, &mut pager_forced, &mut was_alias, &mut config_overrides) {
            alias::Outcome::Shell(code) | alias::Outcome::Exit(code) => return code,
            alias::Outcome::Fatal(msg) => {
                eprintln!("fatal: {msg}");
                return ExitCode::from(fatal::EXIT_FATAL);
            }
            alias::Outcome::Command(head, args) => (head, args),
        };

    // The chain is resolved, so any `-c` an expansion pushed is now checked
    // against the command that will actually read it — git's re-exec of the
    // expanded command inherits `GIT_CONFIG_PARAMETERS` and reports the bad key
    // from inside it, so `alias.x = "-c foo status"` complains and
    // `alias.x = "-c foo version"` does not.
    if let Some(code) = validate_config_overrides(&config_overrides, &sub, &rest) {
        return code;
    }

    // An unknown verb (not a builtin, not an alias) follows git's exact
    // precedence: `execv_dashed_external` first — exec `git-<verb>` from PATH so
    // third-party subcommands (`git fuzzy`, `git lfs`, `git flow`, …) work when
    // zvcs shadows `git` — and only if none is found does it fall to git's
    // `help_unknown_cmd`: `help.autocorrect` may auto-run the nearest command,
    // otherwise git's "not a git command" message + suggestions is printed. A
    // correction may itself be an alias, so it is re-resolved before dispatch.
    let (sub, rest): (String, Vec<String>) = if dispatch::is_verb(&sub) {
        (sub, rest)
    } else {
        // Not a builtin. Try an external `git-<verb>` from PATH first (git's
        // precedence: builtin → external → help_unknown_cmd). Skip it when we were
        // ourselves invoked AS `git-<verb>` — the matching external is this very
        // binary, so re-execing it would loop.
        if !from_dashed {
            // Trace2's `exec` record has to be written *before* the exec: a
            // successful one never returns, so this is the session's last word.
            // Only a failed exec comes back to report an `exec_result`.
            let exec_id = trace2::exec(&format!("git-{sub}"), &rest);
            // The external existed and either was exec'd (never returns) or
            // failed to exec (returns a failure code). `None` falls through.
            let outcome = external::try_dashed(&sub, &rest);
            // Reaching here at all means the exec failed — a missing external
            // and an unrunnable one alike — which is `execvp`'s -1 return.
            trace2::exec_result(exec_id, -1);
            if let Some(code) = outcome {
                return code;
            }
        }
        // `cmd_main`'s `if (was_alias)`: a verb that came out of an alias and
        // still resolves to nothing is reported against the name the user typed,
        // and `help_unknown_cmd` — suggestions and `help.autocorrect` alike — is
        // never reached for it. Exit 1.
        if was_alias {
            eprintln!("expansion of alias '{typed}' failed; '{sub}' is not a git command");
            return ExitCode::FAILURE;
        }
        match autocorrect::correct(&sub) {
            autocorrect::Correction::None => return ExitCode::FAILURE,
            autocorrect::Correction::Use(corrected) => {
                let mut corrected_alias = false;
                match alias::resolve(
                    &corrected,
                    &rest,
                    &mut pager_forced,
                    &mut corrected_alias,
                    &mut config_overrides,
                ) {
                    alias::Outcome::Shell(code) | alias::Outcome::Exit(code) => return code,
                    alias::Outcome::Fatal(msg) => {
                        eprintln!("fatal: {msg}");
                        return ExitCode::from(fatal::EXIT_FATAL);
                    }
                    alias::Outcome::Command(head, args) => (head, args),
                }
            }
        }
    };

    // The verb is final here — aliases expanded, autocorrection applied — which
    // is where git calls `trace2_cmd_name`, and where the `def_param` records for
    // `trace2.configParams` / `trace2.envVars` follow it.
    trace2::cmd_name(&sub);

    // `precompose_argv_prefix(argc, argv, NULL)` (git.c:488), which `run_builtin()`
    // runs over the dispatched command's arguments — after repository setup, so
    // `core.precomposeunicode` is readable, and after alias expansion, so an alias
    // is still looked up under the name that was typed. macOS only; see the module.
    let mut rest = rest;
    precompose::argv(&mut rest);

    // git's repository setup runs next, and its one refusal that is pure policy —
    // `safe.bareRepository` — has to happen before the verb touches the repository.
    if let Some(code) = disallowed_bare_repository(&sub) {
        return code;
    }

    // Install the pager (over stdout, and stderr when it is a tty) before the
    // command runs, so its output — and any error below — flows through it. Torn
    // down after the command and after error reporting, so the error lands in the
    // pager and control returns to the shell only once the user quits it.
    pager::maybe_setup(&sub, pager_forced);
    let code = match dispatch::run(&sub, &rest) {
        Ok(code) => code,
        // A closed stdout is not a command failure. git dies from SIGPIPE here
        // and prints nothing; reporting it as an error meant `zvcs <cmd> | head`
        // printed a spurious diagnostic and exited 1.
        Err(e) if sigpipe::is_broken_pipe(e.as_ref()) => sigpipe::exit_broken_pipe(),
        // A message git itself would `die()` with is rendered the way git renders
        // it — `fatal: <message>`, exit 128 — and without the anyhow context
        // chain, which git has no equivalent of. Anything else is this port
        // speaking for itself and keeps the `zvcs: <verb>:` prefix that says so.
        // The diagnostic is already on stderr; only the exit code is left.
        Err(e) if e.downcast_ref::<fatal::Silent>().is_some() => {
            ExitCode::from(e.downcast_ref::<fatal::Silent>().expect("checked").0)
        }
        Err(e) if e.downcast_ref::<fatal::Fatal>().is_some() => {
            let msg = e.downcast_ref::<fatal::Fatal>().expect("checked").0.clone();
            trace2::error(&msg);
            eprintln!("fatal: {msg}");
            ExitCode::from(fatal::EXIT_FATAL)
        }
        // Repository setup is git's, not this port's. git runs it in the
        // dispatcher (`run_builtin()`, for every `RUN_SETUP` entry of the
        // `commands[]` table) and dies there; the port lets each command find
        // its own repository, so the failure arrives here instead — but it is
        // the same failure and says the same thing, `fatal: …` at 128. Reading
        // it off the error rather than checking up front is what keeps the
        // commands that legitimately run without one working: `grep --no-index`,
        // `archive --remote`, and `rev-parse --parseopt` never ask.
        Err(e) if fatal::discovery_fatal(&e).is_some() => {
            let msg = fatal::discovery_fatal(&e).expect("checked");
            trace2::error(&msg);
            eprintln!("fatal: {msg}");
            ExitCode::from(fatal::EXIT_FATAL)
        }
        Err(e) => {
            let msg = format!("{sub}: {e:#}");
            trace2::error(&msg);
            eprintln!("zvcs: {msg}");
            ExitCode::FAILURE
        }
    };
    pager::finish();
    // Cache rows queued during the command are written by a background thread
    // (see `rcache::cache_write`); this is the one place that waits for it, after
    // the output is out and the pager has been torn down. A detached thread would
    // not outlive the process, so the wait has to happen — but by now the writer
    // has had the whole command, and usually the whole pager session, to get ahead.
    rcache::cache_flush();
    code
}

/// If this binary was invoked as `git-<verb>` (a dashed external form — a symlink
/// in `~/.zvcs/bin` or any `git-*` on PATH), return `<verb>`. Bare `git`, an empty
/// name, or a name lacking the `git-` prefix yields `None`. Mirrors git.c stripping
/// the `git-` prefix from argv[0] before dispatch.
fn dashed_subcommand(arg0: &str) -> Option<String> {
    let base = std::path::Path::new(arg0).file_name()?.to_str()?;
    let verb = base.strip_prefix("git-")?;
    (!verb.is_empty()).then(|| verb.to_string())
}

/// The verbs that keep working when repository setup comes up empty — git's
/// `RUN_SETUP_GENTLY` and no-setup entries in the `commands[]` table of `git.c`,
/// minus the handful (`grep`, `rev-parse`, `archive`) that call
/// `setup_git_directory()` themselves and therefore *do* die. Everything else
/// needs a repository, which is what makes `safe.bareRepository` refuse it.
const NO_SETUP_VERBS: &[&str] = &[
    "apply",
    "bugreport",
    "bundle",
    "check-ref-format",
    "clone",
    "column",
    "config",
    "credential",
    "credential-cache",
    "credential-cache--daemon",
    "credential-store",
    "diagnose",
    "diff",
    "difftool",
    "for-each-repo",
    "get-tar-commit-id",
    "hash-object",
    "help",
    "hook",
    "index-pack",
    "init",
    "init-db",
    "interpret-trailers",
    "ls-remote",
    "mailinfo",
    "mailsplit",
    "merge-file",
    "mergetool",
    "patch-id",
    "receive-pack",
    "remote-ext",
    "remote-fd",
    "shortlog",
    "show-index",
    "stripspace",
    "upload-archive",
    "upload-archive--writer",
    "upload-pack",
    "url-parse",
    "var",
    "verify-pack",
    "version",
];

/// Port of setup.c's `GIT_DIR_DISALLOWED_BARE` refusal: with
/// `safe.bareRepository = explicit`, a repository that was *found by walking up
/// from the current directory* and turns out to be bare is rejected, so a bare
/// repository can only be used when it was named outright (`--git-dir`,
/// `GIT_DIR`). `all` — the default — accepts every one.
///
/// Three parts of git's behaviour are reproduced, because each is observable:
///
/// * The value is read from *protected* configuration only (`git_protected_config`
///   → system, `~/.gitconfig` and the command line), never from the repository's
///   own `config`, so a bare repository cannot whitelist itself.
/// * `is_implicit_bare_repo()` exempts the three paths that are bare only as an
///   implementation detail: a `.git` directory, and `$GIT_DIR` of a secondary
///   worktree or of a submodule.
/// * Only commands that need a repository die; the ones git runs with
///   `RUN_SETUP_GENTLY` (or no setup at all) carry on ([`NO_SETUP_VERBS`]).
///
/// Returns the exit code to leave with, or `None` to continue.
fn disallowed_bare_repository(sub: &str) -> Option<ExitCode> {
    if NO_SETUP_VERBS.contains(&sub) {
        return None;
    }
    // `get_allowed_bare_repo()` defaults to `all`, so the walk below is skipped
    // outright unless the user asked for `explicit`.
    let allowed = config::global_config()
        .string("safe.bareRepository")
        .map(|v| v.to_string());
    if allowed.as_deref() != Some("explicit") {
        return None;
    }
    // `setup_git_directory_gently_1` returns `GIT_DIR_EXPLICIT` before ever
    // reaching the bare check when `$GIT_DIR` names the repository.
    if std::env::var_os("GIT_DIR").is_some() {
        return None;
    }
    let repo = gix::discover(".").ok()?;
    if repo.workdir().is_some() {
        return None;
    }
    let git_dir = std::fs::canonicalize(repo.path()).unwrap_or_else(|_| repo.path().to_owned());
    if is_implicit_bare_repo(&git_dir) {
        return None;
    }
    eprintln!(
        "fatal: cannot use bare repository '{}' (safe.bareRepository is 'explicit')",
        git_dir.display()
    );
    Some(ExitCode::from(128))
}

/// Port of setup.c's `is_implicit_bare_repo()`: the gitdir paths that are bare
/// without the user having made a bare repository — a work tree's own `.git`
/// directory, and the `$GIT_DIR` of a secondary worktree or of a submodule of a
/// non-bare superproject.
fn is_implicit_bare_repo(path: &std::path::Path) -> bool {
    if path.file_name().is_some_and(|n| n == ".git") {
        return true;
    }
    let text = path.to_string_lossy();
    text.contains("/.git/worktrees/") || text.contains("/.git/modules/")
}

/// Ensure a committer identity exists for reflog writes on paths that update refs
/// without making a commit — `fetch`/`pull` (remote-tracking reflogs) and
/// `receive-pack` (the pushed ref reflogs). For a *commit* git errors when
/// `user.name`/`user.email` are unset ("Please tell me who you are"), but for a
/// *reflog* it synthesizes a default from the system user and hostname and proceeds;
/// gix errors in both cases (its personas are cached at open, so an env override set
/// this late is never seen). Fill the reflog gap by injecting a synthesized
/// `user.name`/`user.email` into the repo's *in-memory* config — which `committer()`
/// falls back to — when nothing is configured. Nothing is written to disk, and a real
/// identity is left untouched, so `commit`'s own "who are you" behaviour is unchanged.
pub fn ensure_reflog_identity(repo: &mut gix::Repository) {
    // A configured identity (env or `user.*`, valid or not) is left as-is.
    if repo.committer().is_some() {
        return;
    }
    let name = auto_name();
    // `git_default_email()`: the `EMAIL` environment variable outranks the
    // address built from the account and host, and is what most machines that
    // have no `user.email` actually go by.
    let email = std::env::var("EMAIL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| auto_email(&name.login).0);
    let mut cfg = repo.config_snapshot_mut();
    let _ = cfg.set_value(&gix::config::tree::User::NAME, name.display.as_str());
    let _ = cfg.set_value(&gix::config::tree::User::EMAIL, email.as_str());
    // Rebuilds the cached personas so the injected identity takes effect.
    let _ = cfg.commit();
}

/// The account's login and display names, as `ident.c` reads them: the passwd
/// gecos field up to its first comma is the name git shows, and the login name
/// is what the auto address is built from.
struct AutoName {
    login: String,
    display: String,
}

fn auto_name() -> AutoName {
    let (login, gecos) = passwd_self();
    // `copy_gecos()` (ident.c:60-78) copies the field up to its first comma,
    // expanding `&` to the login name with its first letter capitalized, and
    // `ident_default_name()` trims the result.
    let display = copy_gecos(&gecos, &login);
    // `fmt_ident()` (ident.c:509-518): a gecos field that yields nothing falls
    // back to the login name itself.
    let display = if display.is_empty() { login.clone() } else { display };
    AutoName { login, display }
}

/// `copy_gecos()`: the gecos field up to its first comma, with each `&` replaced
/// by the login name capitalized, trimmed as `ident_default_name()` trims it.
fn copy_gecos(gecos: &str, login: &str) -> String {
    let mut name = String::new();
    for ch in gecos.chars().take_while(|&c| c != ',') {
        if ch == '&' {
            let mut chars = login.chars();
            if let Some(first) = chars.next() {
                name.extend(first.to_uppercase());
                name.push_str(chars.as_str());
            }
        } else {
            name.push(ch);
        }
    }
    name.trim().to_string()
}

/// `xgetpwuid_self()` (ident.c:41-58): this uid's `(pw_name, pw_gecos)`, or the
/// `("unknown", "Unknown")` stand-in git substitutes when there is no passwd
/// entry. git reads the account from the passwd database only — `USER` and
/// `LOGNAME` never take part — so an environment without them still yields the
/// same address stock git builds.
fn passwd_self() -> (String, String) {
    // SAFETY: `getpwuid` returns a pointer into a static buffer owned by libc;
    // both fields are copied out before anything else can overwrite it.
    let pw = unsafe { libc::getpwuid(libc::getuid()) };
    if pw.is_null() {
        return ("unknown".to_string(), "Unknown".to_string());
    }
    let field = |raw: *const libc::c_char| -> String {
        if raw.is_null() {
            String::new()
        } else {
            // SAFETY: a non-null passwd field is a NUL-terminated C string.
            unsafe { std::ffi::CStr::from_ptr(raw) }.to_string_lossy().into_owned()
        }
    };
    (field(unsafe { (*pw).pw_name }), field(unsafe { (*pw).pw_gecos }))
}

/// `<login>@<domain>` the way `add_domainname()` builds it, and whether git
/// considers the result bogus.
///
/// A hostname that already carries a domain is used as-is; otherwise the
/// canonical name from the resolver is tried; when neither yields a domain git
/// appends `.(none)` and marks the address as *not* auto-detected — which is
/// what makes a `commit` on a domain-less machine refuse while its reflogs are
/// written with that same address.
fn auto_email(login: &str) -> (String, bool) {
    let host = hostname().unwrap_or_else(|| "(none)".to_string());
    if host.contains('.') {
        return (format!("{login}@{host}"), false);
    }
    match canonical_hostname(&host) {
        Some(fqdn) if fqdn.contains('.') => (format!("{login}@{fqdn}"), false),
        _ => (format!("{login}@{host}.(none)"), true),
    }
}

/// The resolver's canonical name for `host` (`getaddrinfo` with `AI_CANONNAME`),
/// which is how git finds a domain for a short hostname.
fn canonical_hostname(host: &str) -> Option<String> {
    let c_host = std::ffi::CString::new(host).ok()?;
    let hints = libc::addrinfo {
        ai_flags: libc::AI_CANONNAME,
        ai_family: libc::AF_UNSPEC,
        ai_socktype: 0,
        ai_protocol: 0,
        ai_addrlen: 0,
        ai_canonname: std::ptr::null_mut(),
        ai_addr: std::ptr::null_mut(),
        ai_next: std::ptr::null_mut(),
    };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: `c_host` outlives the call, `hints` is fully initialized, and the
    // list `res` points at is freed before returning.
    let rc = unsafe { libc::getaddrinfo(c_host.as_ptr(), std::ptr::null(), &hints, &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    let canon = unsafe { (*res).ai_canonname };
    let out = if canon.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(canon) }.to_str().ok().map(str::to_string)
    };
    unsafe { libc::freeaddrinfo(res) };
    out
}

/// git's identity rules for a command that writes a **commit or tag object**.
///
/// `fmt_ident()` fills each half the user did not give — the name from the
/// system account, the email from `<user>@<host>` — and only refuses when
/// `user.useConfigOnly` turns that auto-detection off. The refusal is a block of
/// advice and `fatal: no <field> was given and auto-detection is disabled`,
/// exit 128, with the missing *email* reported ahead of a missing name.
///
/// `role` is the word git heads the block with: `Author` for the commit-shaped
/// commands (`commit`, `notes`, `commit-tree`) and `Committer` for `tag`.
/// Commands that write an object *without* the strict check — `git stash` makes
/// a commit under `user.useConfigOnly` just fine — want
/// [`ensure_reflog_identity`] instead.
///
/// Returns the exit code to hand back when git would refuse; `None` means the
/// identity is settled and the caller proceeds.
pub fn ensure_object_identity(repo: &mut gix::Repository, role: &str) -> Option<std::process::ExitCode> {
    let env_set = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
    let cfg = repo.config_snapshot();
    let cfg_set = |key: &str| cfg.string(key).is_some_and(|v| !v.is_empty());
    let upper = role.to_uppercase();
    // git reads `GIT_<ROLE>_NAME`/`_EMAIL` ahead of the config, and `EMAIL` as a
    // last resort for the address alone.
    let name = env_set(&format!("GIT_{upper}_NAME")) || cfg_set("user.name");
    // `EMAIL` is a fallback, not a setting: `user.useConfigOnly` refuses over a
    // missing `user.email` even on a machine that exports one.
    let email_configured = env_set(&format!("GIT_{upper}_EMAIL")) || cfg_set("user.email");
    let email = email_configured || env_set("EMAIL");
    let use_config_only = cfg.boolean("user.useConfigOnly").unwrap_or(false);
    drop(cfg);

    if name && email_configured {
        return None;
    }
    if use_config_only {
        let missing = if !email_configured { "email" } else { "name" };
        identity_advice(role);
        eprintln!("fatal: no {missing} was given and auto-detection is disabled");
        return Some(std::process::ExitCode::from(128));
    }
    if name && email {
        return None;
    }
    // The address git would auto-detect, and whether it counts as detected at
    // all: on a machine whose hostname carries no domain it does not, and that
    // is what a strict command refuses over.
    let auto = auto_name();
    let (auto_addr, bogus) = auto_email(&auto.login);
    if !email && bogus {
        identity_advice(role);
        eprintln!("fatal: unable to auto-detect email address (got '{auto_addr}')");
        return Some(std::process::ExitCode::from(128));
    }
    ensure_reflog_identity(repo);
    None
}

/// The block `fmt_ident()` prints before it gives up, verbatim.
fn identity_advice(role: &str) {
    eprintln!(
        "{role} identity unknown\n\n\
         *** Please tell me who you are.\n\n\
         Run\n\n\
         \x20 git config --global user.email \"you@example.com\"\n\
         \x20 git config --global user.name \"Your Name\"\n\n\
         to set your account's default identity.\n\
         Omit --global to set the identity only in this repository.\n"
    );
}

/// The machine's hostname (`gethostname`), for the synthesized reflog identity.
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).ok().filter(|s| !s.is_empty()).map(str::to_string)
}

/// The exec-path `git --exec-path` reports: `$GIT_EXEC_PATH` when set, else the zvcs
/// bin directory where the shadow's `git-*` helper symlinks live (`$HOME/.zvcs/bin`).
/// Unlike stock git's `libexec/git-core`, the shadow serves every `git-*` helper from
/// one binary, so that install dir is the honest answer.
pub(crate) fn exec_path() -> String {
    if let Ok(p) = std::env::var("GIT_EXEC_PATH") {
        if !p.is_empty() {
            return p;
        }
    }
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => format!("{h}/.zvcs/bin"),
        _ => ".zvcs/bin".to_string(),
    }
}

/// Record one command-line configuration override and publish it, the way
/// `git_config_push_split_parameter()` appends to `GIT_CONFIG_PARAMETERS`.
///
/// The transport differs — this port uses git's other command-line-config
/// channel, the `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_N` / `GIT_CONFIG_VALUE_N`
/// sequence, because that is the one `gix-config` reads — but the timing is the
/// C's: each override is visible to every child process the moment it is parsed,
/// and it appends to any count a parent process already set. A bare `-c <name>`
/// (no `=`) is git's boolean-true form, encoded as an empty value, which gix
/// reads as true for boolean keys.
///
/// The `overrides` list is kept for [`validate_config_overrides`], which is
/// where git's own diagnostics for a malformed key are produced.
fn push_config_override(overrides: &mut Vec<ConfigOverride>, over: ConfigOverride) {
    let count: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    std::env::set_var(format!("GIT_CONFIG_KEY_{count}"), &over.key);
    std::env::set_var(format!("GIT_CONFIG_VALUE_{count}"), over.value.as_deref().unwrap_or(""));
    std::env::set_var("GIT_CONFIG_COUNT", (count + 1).to_string());
    overrides.push(over);
}

/// The current session key for attributing operations to an agent: `ZVCS_SESSION`
/// if set (export `ZVCS_SESSION=$$` per shell), else the parent process id. Used
/// by claims, job submission, and the op ledger.
pub fn session_key() -> String {
    // Treat a set-but-EMPTY `ZVCS_SESSION` as unset. `env::var` returns `Ok("")`
    // for it, which would otherwise collapse every such shell to the one session
    // key `""` — cross-session claim release / false "already mine".
    std::env::var("ZVCS_SESSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("pid-{}", std::os::unix::process::parent_id()))
}

/// Print (once) any unnotified failed autonomous jobs for the current repo, then
/// mark them notified. Cheap no-op when there is no ledger or no failures; never
/// creates the ledger (only reads/updates one the daemon already made).
fn surface_pending_failures() {
    if !db::db_path().exists() {
        return;
    }
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    let git_dir = match repo.git_dir().canonicalize() {
        Ok(p) => p,
        Err(_) => return,
    };
    // Read with the cheap RO handle: this runs on EVERY git invocation across all
    // concurrent instances, and the common case is zero pending failures. Opening
    // RW here would replay the whole schema DDL and take a write lock every time,
    // purely to run a SELECT. Only take the RW handle when there is something to
    // clear.
    let Ok(conn) = db::open_ro() else {
        return;
    };
    let Ok(pending) = db::pending_failures(&conn, &git_dir) else {
        return;
    };
    if pending.is_empty() {
        return;
    }
    let ids: Vec<i64> = pending.iter().map(|(id, _, _)| *id).collect();
    for (_, kind, reason) in &pending {
        if reason.is_empty() {
            eprintln!("zvcs: {kind} failed");
        } else {
            eprintln!("zvcs: {kind} failed: {reason}");
        }
    }
    if let Ok(wconn) = db::open_rw() {
        let _ = db::mark_notified(&wconn, &ids);
    }
}
