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
pub mod objname;
pub mod optint;
pub mod pager;
pub mod parseopt;
pub mod pathspec;
pub mod porcelain;
pub mod precompose;
pub mod progress;
pub mod rcache;
pub mod revfilter;
pub mod sequencer;
pub mod setup;
pub mod shallow_serve;
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
    std::env::set_current_dir(dir).map_err(|e| {
        // `strerror(errno)` has no Rust ` (os error <n>)` tail.
        let text = e.to_string();
        let text = text.split(" (os error ").next().unwrap_or(&text);
        format!("cannot change to '{dir}': {text}")
    })
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

    // Consume the leading git-global options we support, so `git -C <dir> <verb>`
    // (extremely common in scripts and tooling) reaches the verb instead of
    // treating `-C` as the subcommand. `-C <dir>` chdirs (before autostart /
    // failure-surfacing, which key off the cwd); the pager flags force paging on
    // (`-p`/`--paginate`) or off (`-P`/`--no-pager`). A global given without its
    // value ends the same way `handle_options()` does: the complaint, git's usage
    // block, exit 129. A global this loop has never heard of is left in place and
    // surfaces as an unknown verb rather than being silently mishandled — which is
    // a divergence from git's `unknown option: --<x>` + usage (129).
    let mut idx = 0;
    let mut pager_forced: Option<bool> = None;
    // `-c <name>=<value>` overrides, collected and injected into gix's config
    // resolution via git's `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_N`/…_VALUE_N env
    // mechanism (which gix-config reads), so `git -c foo.bar=x <verb>` behaves
    // exactly as git does — tooling and the submodule re-exec path rely on it.
    let mut config_overrides: Vec<String> = Vec::new();
    while idx < raw.len() {
        match raw[idx].as_str() {
            "-C" => {
                let Some(dir) = raw.get(idx + 1) else {
                    return usage_missing_value("no directory given for '-C' option");
                };
                if let Err(msg) = chdir_global(dir) {
                    eprintln!("fatal: {msg}");
                    return ExitCode::from(fatal::EXIT_FATAL);
                }
                idx += 2;
            }
            "-c" => {
                let Some(pair) = raw.get(idx + 1) else {
                    return usage_missing_value("-c expects a configuration string");
                };
                config_overrides.push(pair.clone());
                idx += 2;
            }
            "-p" | "--paginate" => {
                pager_forced = Some(true);
                idx += 1;
            }
            "-P" | "--no-pager" => {
                pager_forced = Some(false);
                idx += 1;
            }
            // `--git-dir`/`--work-tree`/`--namespace` set the well-known env vars
            // gix honors, in both the `--flag <val>` and `--flag=<val>` forms.
            "--git-dir" | "--work-tree" | "--namespace" => {
                let (key, missing) = match raw[idx].as_str() {
                    "--git-dir" => ("GIT_DIR", "no directory given for '--git-dir' option"),
                    "--work-tree" => ("GIT_WORK_TREE", "no directory given for '--work-tree' option"),
                    _ => ("GIT_NAMESPACE", "no namespace given for --namespace"),
                };
                let Some(val) = raw.get(idx + 1) else {
                    return usage_missing_value(missing);
                };
                std::env::set_var(key, val);
                idx += 2;
            }
            s if s.starts_with("--git-dir=") => {
                std::env::set_var("GIT_DIR", &s["--git-dir=".len()..]);
                idx += 1;
            }
            s if s.starts_with("--work-tree=") => {
                std::env::set_var("GIT_WORK_TREE", &s["--work-tree=".len()..]);
                idx += 1;
            }
            s if s.starts_with("--namespace=") => {
                std::env::set_var("GIT_NAMESPACE", &s["--namespace=".len()..]);
                idx += 1;
            }
            // `git --exec-path` (no value) prints the core-programs directory and
            // exits, ignoring any following command — git's `handle_options`. For the
            // shadow that is where its `git-*` helpers live (`~/.zvcs/bin`), or
            // `$GIT_EXEC_PATH` when set. The `=<path>` form sets the prefix instead.
            "--exec-path" => {
                println!("{}", exec_path());
                return ExitCode::SUCCESS;
            }
            s if s.starts_with("--exec-path=") => {
                std::env::set_var("GIT_EXEC_PATH", &s["--exec-path=".len()..]);
                idx += 1;
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
                return ExitCode::SUCCESS;
            }
            "--man-path" => {
                println!("{}", superset::manpage::man_dir().display());
                return ExitCode::SUCCESS;
            }
            "--info-path" => {
                println!("{}", porcelain::help::info_dir().display());
                return ExitCode::SUCCESS;
            }
            _ => break,
        }
    }
    if !config_overrides.is_empty() {
        apply_config_overrides(&config_overrides);
    }
    let args = &raw[idx..];

    let Some(sub) = args.first() else {
        eprintln!("zvcs: no subcommand given");
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
    let (sub, rest): (String, Vec<String>) = match alias::resolve(sub, rest, &mut pager_forced) {
        alias::Outcome::Shell(code) | alias::Outcome::Exit(code) => return code,
        alias::Outcome::Fatal(msg) => {
            eprintln!("zvcs: {msg}");
            return ExitCode::FAILURE;
        }
        alias::Outcome::Command(head, args) => (head, args),
    };

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
        match autocorrect::correct(&sub) {
            autocorrect::Correction::None => return ExitCode::FAILURE,
            autocorrect::Correction::Use(corrected) => {
                match alias::resolve(&corrected, &rest, &mut pager_forced) {
                    alias::Outcome::Shell(code) | alias::Outcome::Exit(code) => return code,
                    alias::Outcome::Fatal(msg) => {
                        eprintln!("zvcs: {msg}");
                        return ExitCode::FAILURE;
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

/// Translate `git -c <name>=<value>` overrides into the `GIT_CONFIG_COUNT` /
/// `GIT_CONFIG_KEY_N` / `GIT_CONFIG_VALUE_N` environment sequence that
/// `gix-config` reads, appending to any count a parent process already set. A
/// bare `-c <name>` (no `=`) is git's boolean-true form, encoded as an empty
/// value (which gix reads as true for boolean keys), matching git.
fn apply_config_overrides(overrides: &[String]) {
    let mut count: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    for pair in overrides {
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair.as_str(), ""),
        };
        std::env::set_var(format!("GIT_CONFIG_KEY_{count}"), key);
        std::env::set_var(format!("GIT_CONFIG_VALUE_{count}"), value);
        count += 1;
    }
    std::env::set_var("GIT_CONFIG_COUNT", count.to_string());
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
