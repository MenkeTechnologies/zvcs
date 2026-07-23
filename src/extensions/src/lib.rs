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
pub mod dispatch;
pub mod external;
pub mod hooks;
pub mod index_commit;
pub mod jobpool;
pub mod jobrun;
pub mod lock;
pub mod merge_apply;
pub mod pager;
pub mod porcelain;
pub mod revfilter;
pub mod superset;
pub mod worktree;

use std::process::ExitCode;

/// Parse `argv`, dispatch the subcommand, and return the process exit code.
/// Errors are reported terse on stderr as `zvcs: <command>: <reason>`.
pub fn run() -> ExitCode {
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
    // (`-p`/`--paginate`) or off (`-P`/`--no-pager`). Unrecognized globals (`-c`,
    // `--git-dir`, …) are left in place and surface as an error rather than being
    // silently mishandled.
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
                let Some(dir) = raw.get(idx + 1) else { break };
                if std::env::set_current_dir(dir).is_err() {
                    eprintln!("zvcs: -C: cannot chdir to {dir}");
                    return ExitCode::FAILURE;
                }
                idx += 2;
            }
            "-c" => {
                let Some(pair) = raw.get(idx + 1) else { break };
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
                let key = match raw[idx].as_str() {
                    "--git-dir" => "GIT_DIR",
                    "--work-tree" => "GIT_WORK_TREE",
                    _ => "GIT_NAMESPACE",
                };
                let Some(val) = raw.get(idx + 1) else { break };
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
    // "not yet ported" instead of printing the version.
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
        alias::Outcome::Shell(code) => return code,
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
            if let Some(code) = external::try_dashed(&sub, &rest) {
                // The external existed and either was exec'd (never returns) or
                // failed to exec (returns a failure code). `None` falls through.
                return code;
            }
        }
        match autocorrect::correct(&sub) {
            autocorrect::Correction::None => return ExitCode::FAILURE,
            autocorrect::Correction::Use(corrected) => {
                match alias::resolve(&corrected, &rest, &mut pager_forced) {
                    alias::Outcome::Shell(code) => return code,
                    alias::Outcome::Fatal(msg) => {
                        eprintln!("zvcs: {msg}");
                        return ExitCode::FAILURE;
                    }
                    alias::Outcome::Command(head, args) => (head, args),
                }
            }
        }
    };

    // Install the pager (over stdout, and stderr when it is a tty) before the
    // command runs, so its output — and any error below — flows through it. Torn
    // down after the command and after error reporting, so the error lands in the
    // pager and control returns to the shell only once the user quits it.
    pager::maybe_setup(&sub, pager_forced);
    let code = match dispatch::run(&sub, &rest) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("zvcs: {sub}: {e:#}");
            ExitCode::FAILURE
        }
    };
    pager::finish();
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
