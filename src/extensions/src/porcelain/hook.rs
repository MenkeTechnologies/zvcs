//! `git hook` — list and run the hooks configured for a hook event.
//!
//! This command needs no object-database substrate: it is a config reader plus a
//! process spawner, so it ports onto gitoxide cleanly. Hooks come from two
//! places, and git runs them in this order: every `hook.<friendly-name>.command`
//! whose `hook.<friendly-name>.event` names the queried event (in config parse
//! order), then the traditional `<hooks-dir>/<event>` script last.
//!
//! Covered, byte-identically with stock git:
//!   * `git hook list [--allow-unknown-hook-name] [-z] [--show-scope] <event>` —
//!     including the `disabled` / `event-disabled` markers, the literal
//!     `hook from hookdir` line, the `warning: no hooks found ...` + exit 1 path,
//!     and the `--show-scope` scope names (`system`/`global`/`local`/
//!     `worktree`/`command`).
//!   * `git hook run [--allow-unknown-hook-name] [--ignore-missing]
//!     [--to-stdin=<path>] [-j|--jobs <n>] <event> [-- <hook-args>]` — serial
//!     execution, git's `prepare_shell_cmd` argv construction, hook stdout
//!     redirected to stderr, and the bitwise-OR of every hook's exit status as
//!     the command's own exit code.
//!   * Config semantics: last-`command`-wins, `event` accumulation with an empty
//!     value resetting the list, a repeated event moving the hook to the end of
//!     the order, `hook.<name>.enabled`, `hook.<event>.enabled` (which does not
//!     suppress the hookdir hook), `core.hooksPath`, and the
//!     `advice.ignoredHook` hint for a non-executable hookdir script.
//!   * The known-hook-event check, the friendly-name/event collision fatals, and
//!     the usage/exit-129 paths for a missing or unknown subcommand.
//!
//! Not covered — rejected with a precise message rather than diverging silently:
//! parallel execution (`-j`/`--jobs`/`hook.jobs`/`hook.<event>.jobs` greater
//! than one when it would actually engage), because matching git's
//! `run_processes_parallel` output de-interleaving is not reproduced here.
//!
//! One known divergence: the `advice.ignoredHook` hint prints the hook path
//! relative to the current directory when it lies below it, and absolute
//! otherwise; git derives the same string from its own `git_path()` bookkeeping,
//! so the two can differ when `git hook` is run from a subdirectory.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use gix::bstr::{BString, ByteSlice};

/// Hook events git recognises, i.e. the ones accepted without
/// `--allow-unknown-hook-name`. Mirrors githooks(5).
const KNOWN_EVENTS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

/// Events git always runs serially because their hooks share mutable state, so
/// `-j`/`hook.jobs` never applies to them.
const ALWAYS_SERIAL: &[&str] = &[
    "applypatch-msg",
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-checkout",
    "push-to-checkout",
];

const USAGE_RUN: &str = "usage: git hook run [--allow-unknown-hook-name] [--ignore-missing] [--to-stdin=<path>] [(-j|--jobs) <n>]\n       <hook-name> [-- <hook-args>]\n";
const USAGE_LIST_ALT: &str =
    "   or: git hook list [--allow-unknown-hook-name] [-z] [--show-scope] <hook-name>\n";
const OPTS_RUN: &str = "\n    --[no-]allow-unknown-hook-name\n                          allow running a hook with a non-native hook name\n    --[no-]ignore-missing silently ignore missing requested <hook-name>\n    --[no-]to-stdin <path>\n                          file to read into hooks' stdin\n    -j, --[no-]jobs <n>   run up to <n> hooks simultaneously (-1 for CPU count)\n\n";
const USAGE_LIST: &str =
    "usage: git hook list [--allow-unknown-hook-name] [-z] [--show-scope] <hook-name>\n";
const OPTS_LIST: &str = "\n    -z                    use NUL as line terminator\n    --[no-]show-scope     show the config scope that defined each hook\n    --[no-]allow-unknown-hook-name\n                          allow running a hook with a non-native hook name\n\n";

/// `git hook` — dispatch to the `run` or `list` subcommand.
///
/// A missing or unrecognised subcommand prints git's combined usage on stderr
/// and exits 129, exactly as parse-options does.
pub fn hook(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand name being present at index 0 so both calling
    // conventions of the dispatcher behave the same.
    let args = match args.first() {
        Some(a) if a == "hook" => &args[1..],
        _ => args,
    };

    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("list") => list(&args[1..]),
        None => {
            eprint!("error: need a subcommand\n{USAGE_RUN}{USAGE_LIST_ALT}\n");
            Ok(ExitCode::from(129))
        }
        Some(other) => {
            eprint!("error: unknown subcommand: `{other}'\n{USAGE_RUN}{USAGE_LIST_ALT}\n");
            Ok(ExitCode::from(129))
        }
    }
}

/// One `hook.<name>.event = <event>` occurrence that survived config parsing.
///
/// The vector of these is the run order: a later occurrence of the same
/// (name, event) pair replaces the earlier one, moving the hook to the end.
struct Registration {
    name: String,
    event: String,
    /// `--show-scope` label of the config file the winning occurrence came from.
    scope: &'static str,
}

/// The non-`event` settings of one `hook.<friendly-name>` section, merged across
/// every config file with last-value-wins.
#[derive(Default)]
struct HookCfg {
    command: Option<BString>,
    enabled: Option<bool>,
    parallel: Option<bool>,
    /// Whether any `event` key at all was seen, which is what makes a
    /// friendly-name colliding with a known event name a fatal error.
    has_event_key: bool,
}

/// Everything `run`/`list` need after the config has been parsed and validated.
struct Config {
    regs: Vec<Registration>,
    hooks: BTreeMap<String, HookCfg>,
}

/// `git hook list <event>` — print the hooks that would fire for `<event>`.
///
/// Exits 1 with a warning when the resulting list is empty; note that hooks
/// present but disabled still count as found and are printed with their marker.
fn list(args: &[String]) -> Result<ExitCode> {
    let mut nul = false;
    let mut show_scope = false;
    let mut allow_unknown = false;
    let mut event: Option<String> = None;

    for a in args {
        match a.as_str() {
            "-z" => nul = true,
            "--show-scope" => show_scope = true,
            "--no-show-scope" => show_scope = false,
            "--allow-unknown-hook-name" => allow_unknown = true,
            "--no-allow-unknown-hook-name" => allow_unknown = false,
            "--end-of-options" => {}
            s if s.starts_with('-') && s.len() > 1 => {
                eprint!(
                    "error: unknown option `{}'\n{USAGE_LIST}{OPTS_LIST}",
                    s.trim_start_matches('-')
                );
                return Ok(ExitCode::from(129));
            }
            s => {
                if event.is_some() {
                    bail!("unexpected extra argument {s:?}");
                }
                event = Some(s.to_string());
            }
        }
    }

    let Some(event) = event else {
        eprint!("fatal: you must specify a hook event name to list\n\n{USAGE_LIST}{OPTS_LIST}");
        return Ok(ExitCode::from(129));
    };
    if let Some(code) = reject_unknown_event(&event, allow_unknown) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    let cfg = match parse_config(&repo)? {
        Ok(cfg) => cfg,
        Err(code) => return Ok(code),
    };
    let event_disabled = event_disabled(&cfg, &event);

    let term = if nul { '\0' } else { '\n' };
    let mut out = String::new();
    let mut found = false;

    for reg in cfg.regs.iter().filter(|r| r.event == event) {
        found = true;
        if show_scope {
            out.push_str(reg.scope);
            out.push('\t');
        }
        if event_disabled {
            out.push_str("event-disabled\t");
        } else if cfg.hooks.get(&reg.name).and_then(|h| h.enabled) == Some(false) {
            out.push_str("disabled\t");
        }
        out.push_str(&reg.name);
        out.push(term);
    }

    if hookdir_hook(&repo, &event)?.is_some() {
        found = true;
        out.push_str("hook from hookdir");
        out.push(term);
    }

    if !found {
        eprintln!("warning: no hooks found for event '{event}'");
        return Ok(ExitCode::from(1));
    }
    print!("{out}");
    std::io::stdout().flush()?;
    Ok(ExitCode::SUCCESS)
}

/// `git hook run <event> [-- <args>]` — execute the hooks for `<event>`.
///
/// Every hook's stdout is redirected to stderr (as git does), so this command
/// writes nothing to its own stdout. The exit code is the bitwise OR of the
/// individual hook exit codes, matching git's accumulation.
fn run(args: &[String]) -> Result<ExitCode> {
    let mut allow_unknown = false;
    let mut ignore_missing = false;
    let mut to_stdin: Option<String> = None;
    let mut jobs_flag: Option<i64> = None;
    let mut event: Option<String> = None;
    let mut hook_args: Vec<String> = Vec::new();

    let mut i = 0;
    let mut end_of_options = false;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_options {
            hook_args.push(a.to_string());
            i += 1;
            continue;
        }
        // Pull `--opt=<v>`, or consume the following argument for `--opt <v>`.
        let next = |i: &mut usize, inline: Option<&str>, name: &str| -> Result<String> {
            match inline {
                Some(v) => Ok(v.to_string()),
                None => {
                    *i += 1;
                    match args.get(*i) {
                        Some(v) => Ok(v.clone()),
                        None => bail!("option `{name}` requires a value"),
                    }
                }
            }
        };
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (a, None),
        };

        match name {
            "--" => end_of_options = true,
            "--end-of-options" => end_of_options = true,
            "--allow-unknown-hook-name" => allow_unknown = true,
            "--no-allow-unknown-hook-name" => allow_unknown = false,
            "--ignore-missing" => ignore_missing = true,
            "--no-ignore-missing" => ignore_missing = false,
            "--to-stdin" => to_stdin = Some(next(&mut i, inline, "--to-stdin")?),
            "--no-to-stdin" => to_stdin = None,
            "--jobs" => jobs_flag = Some(parse_jobs(&next(&mut i, inline, "--jobs")?)?),
            "-j" => jobs_flag = Some(parse_jobs(&next(&mut i, None, "-j")?)?),
            s if s.starts_with("-j") && s.len() > 2 => jobs_flag = Some(parse_jobs(&s[2..])?),
            s if s.starts_with('-') && s.len() > 1 => {
                eprint!(
                    "error: unknown option `{}'\n{USAGE_RUN}{OPTS_RUN}",
                    s.trim_start_matches('-')
                );
                return Ok(ExitCode::from(129));
            }
            s => {
                if event.is_some() {
                    bail!("unexpected extra argument {s:?} (hook arguments go after `--`)");
                }
                event = Some(s.to_string());
            }
        }
        i += 1;
    }

    let Some(event) = event else {
        eprint!("{USAGE_RUN}{OPTS_RUN}");
        return Ok(ExitCode::from(129));
    };
    if let Some(code) = reject_unknown_event(&event, allow_unknown) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    let cfg = match parse_config(&repo)? {
        Ok(cfg) => cfg,
        Err(code) => return Ok(code),
    };
    let event_disabled = event_disabled(&cfg, &event);
    let hookdir = hookdir_hook(&repo, &event)?;

    // "Found" ignores the enabled flags: git only reports a missing hook when
    // nothing is configured for the event and no hookdir script exists.
    let registered: Vec<&Registration> = cfg.regs.iter().filter(|r| r.event == event).collect();
    if registered.is_empty() && hookdir.is_none() {
        if ignore_missing {
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!("error: cannot find a hook named {event}");
        return Ok(ExitCode::from(1));
    }

    // The command lines to execute, in order, hookdir script last.
    let mut commands: Vec<BString> = Vec::new();
    if !event_disabled {
        for reg in &registered {
            let Some(h) = cfg.hooks.get(&reg.name) else {
                continue;
            };
            if h.enabled == Some(false) {
                continue;
            }
            if let Some(cmd) = &h.command {
                commands.push(cmd.clone());
            }
        }
    }
    let configured_count = commands.len();
    if let Some(path) = &hookdir {
        commands.push(gix::path::into_bstr(path.as_path()).into_owned());
    }

    // Parallelism is only rejected when it would actually engage, so a repo that
    // merely sets `hook.jobs` still runs its serial hooks normally.
    let jobs = effective_jobs(&repo, &event, jobs_flag)?;
    let all_parallel = registered
        .iter()
        .filter_map(|r| cfg.hooks.get(&r.name))
        .all(|h| h.parallel == Some(true));
    let engages = commands.len() > 1
        && jobs > 1
        && !ALWAYS_SERIAL.contains(&event.as_str())
        && (jobs_flag.is_some() || configured_count == 0 || all_parallel);
    if engages {
        bail!(
            "unsupported flag \"--jobs\" (parallel hook execution is not ported; \
             ported: serial execution with -j1)"
        );
    }

    // Hooks inherit stderr, and their stdout is pointed at it too.
    let stderr = stderr_dup()?;
    let mut rc: i32 = 0;
    for cmd in &commands {
        let stdin = match &to_stdin {
            Some(path) => match std::fs::File::open(path) {
                Ok(f) => Stdio::from(f),
                Err(e) => {
                    eprintln!(
                        "fatal: could not open '{path}' for reading: {}",
                        errno_str(&e)
                    );
                    return Ok(ExitCode::from(128));
                }
            },
            None => Stdio::null(),
        };
        let mut child = shell_command(cmd, &hook_args)?;
        child
            .stdin(stdin)
            .stdout(Stdio::from(stderr.try_clone()?))
            .stderr(Stdio::inherit());
        let status = child.status()?;
        // A signal-terminated hook has no exit code; git surfaces its failure as
        // a non-zero status, so saturate rather than treat it as success.
        rc |= status.code().unwrap_or(255);
    }

    Ok(ExitCode::from((rc & 0xff) as u8))
}

/// Reject a hook event git does not know, unless `--allow-unknown-hook-name`.
fn reject_unknown_event(event: &str, allow_unknown: bool) -> Option<ExitCode> {
    if allow_unknown || KNOWN_EVENTS.contains(&event) {
        return None;
    }
    eprintln!(
        "error: unknown hook event '{event}';\nuse --allow-unknown-hook-name to allow non-native hook names"
    );
    Some(ExitCode::from(1))
}

/// Walk the merged config in parse order and build the hook registry.
///
/// Returns `Err(code)` (never an `anyhow` error) for git's own fatal config
/// diagnostics, which print their message and exit 128.
fn parse_config(repo: &gix::Repository) -> Result<std::result::Result<Config, ExitCode>> {
    let snapshot = repo.config_snapshot();
    let file = snapshot.plumbing();

    let mut regs: Vec<Registration> = Vec::new();
    let mut hooks: BTreeMap<String, HookCfg> = BTreeMap::new();
    // Registration order is per-section; remember first sight of each name so
    // the diagnostics below are reported in config order, as git does.
    let mut seen: Vec<String> = Vec::new();

    for section in file.sections() {
        if !section.header().name().eq_ignore_ascii_case(b"hook") {
            continue;
        }
        let Some(sub) = section.header().subsection_name() else {
            continue;
        };
        let name = sub.to_str_lossy().into_owned();
        let scope = scope_name(section.meta().source);

        let entry = hooks.entry(name.clone()).or_default();
        if !seen.contains(&name) {
            seen.push(name.clone());
        }
        if let Some(v) = section.value("command") {
            entry.command = Some(v);
        }
        // A valueless key (`enabled` with no `= ...`) is git's implicit `true`.
        match section.value_implicit("enabled") {
            Some(Some(v)) => entry.enabled = Some(parse_bool(&v, &format!("hook.{name}.enabled"))?),
            Some(None) => entry.enabled = Some(true),
            None => {}
        }
        match section.value_implicit("parallel") {
            Some(Some(v)) => {
                entry.parallel = Some(parse_bool(&v, &format!("hook.{name}.parallel"))?)
            }
            Some(None) => entry.parallel = Some(true),
            None => {}
        }

        for value in section.values("event") {
            entry.has_event_key = true;
            if value.is_empty() {
                // An empty value clears every event previously set for this hook.
                regs.retain(|r| r.name != name);
                continue;
            }
            let event = value.to_str_lossy().into_owned();
            regs.retain(|r| !(r.name == name && r.event == event));
            regs.push(Registration {
                name: name.clone(),
                event,
                scope,
            });
        }
    }

    for name in &seen {
        let h = &hooks[name];
        if !h.has_event_key {
            continue;
        }
        if KNOWN_EVENTS.contains(&name.as_str()) {
            eprintln!(
                "fatal: hook friendly-name '{name}' collides with a known event name; please choose a different friendly-name"
            );
            return Ok(Err(ExitCode::from(128)));
        }
        if h.command.is_none() && regs.iter().any(|r| &r.name == name) {
            eprintln!(
                "fatal: 'hook.{name}.command' must be configured or 'hook.{name}.event' must be removed; aborting."
            );
            return Ok(Err(ExitCode::from(128)));
        }
        if regs.iter().any(|r| &r.name == name && &r.event == name) {
            eprintln!(
                "warning: hook friendly-name '{name}' is the same as its event; this may cause ambiguity with hook.{name}.enabled"
            );
        }
    }

    Ok(Ok(Config { regs, hooks }))
}

/// Whether `hook.<event>.enabled` turns the whole event off.
///
/// A friendly-name that shadows the event name claims the key for itself, so the
/// event-level switch is only consulted when no such hook is registered.
fn event_disabled(cfg: &Config, event: &str) -> bool {
    if cfg.regs.iter().any(|r| r.name == event) {
        return false;
    }
    cfg.hooks.get(event).and_then(|h| h.enabled) == Some(false)
}

/// The `--show-scope` label git prints for a config source.
fn scope_name(source: gix::config::Source) -> &'static str {
    use gix::config::Source::*;
    match source {
        GitInstallation | System => "system",
        Git | User => "global",
        Local => "local",
        Worktree => "worktree",
        Env | Cli | Api | EnvOverride => "command",
    }
}

/// git's boolean config syntax for an explicit value: `true`/`yes`/`on`/`1` are
/// true, `false`/`no`/`off`/`0`/empty are false. A valueless key is git's
/// implicit `true` and is resolved by the caller.
fn parse_bool(value: &[u8], key: &str) -> Result<bool> {
    let v = value.to_str_lossy();
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" | "" => Ok(false),
        _ => bail!("bad boolean config value '{v}' for '{key}'"),
    }
}

/// `-j`/`--jobs` value: a positive count, or `-1` for the CPU count.
fn parse_jobs(value: &str) -> Result<i64> {
    let n: i64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid job count `{value}`"))?;
    if n == 0 || n < -1 {
        bail!("invalid job count `{value}`");
    }
    Ok(n)
}

/// The job count actually in effect: the flag, else `hook.<event>.jobs`, else
/// `hook.jobs`, else 1. `-1` resolves to the available parallelism.
fn effective_jobs(repo: &gix::Repository, event: &str, flag: Option<i64>) -> Result<i64> {
    let snapshot = repo.config_snapshot();
    let n = match flag {
        Some(n) => n,
        None => {
            let key = format!("hook.{event}.jobs");
            let raw = snapshot
                .string(key.as_str())
                .or_else(|| snapshot.string("hook.jobs"));
            match raw {
                Some(v) => parse_jobs(&v.to_str_lossy())?,
                None => 1,
            }
        }
    };
    if n == -1 {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get() as i64)
            .unwrap_or(1);
        return Ok(cpus);
    }
    Ok(n)
}

/// Locate the traditional `<hooks-dir>/<event>` script.
///
/// Returns the path only when it exists and is executable; a present but
/// non-executable file produces git's `advice.ignoredHook` hint on stderr.
fn hookdir_hook(repo: &gix::Repository, event: &str) -> Result<Option<PathBuf>> {
    let dir = match repo.config_snapshot().trusted_path("core.hooksPath")? {
        Some(p) => p,
        None => repo.common_dir().join("hooks"),
    };
    let path = dir.join(event);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(None);
    };
    if meta.is_dir() {
        return Ok(None);
    }
    if meta.permissions().mode() & 0o111 != 0 {
        return Ok(Some(path));
    }
    if repo.config_snapshot().boolean("advice.ignoredHook") != Some(false) {
        let shown = display_path(&path);
        eprintln!("hint: The '{shown}' hook was ignored because it's not set as executable.");
        eprintln!(
            "hint: You can disable this warning with `git config set advice.ignoredHook false`."
        );
    }
    Ok(None)
}

/// Render a path the way git tends to: relative to the current directory when it
/// lies below it, absolute otherwise.
fn display_path(path: &Path) -> String {
    let rel = std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf));
    rel.unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

/// Build the child process for one hook command, reproducing git's
/// `prepare_shell_cmd`.
///
/// A command containing any shell metacharacter runs under `/bin/sh -c`, with
/// ` "$@"` appended only when there are hook arguments to pass, and the command
/// itself repeated as `$0`. Anything else is executed directly.
fn shell_command(command: &[u8], args: &[String]) -> Result<Command> {
    const META: &[u8] = b"|&;<>()$`\\\"' \t\n*?[#~=%";

    let cmd = command.to_os_str()?;
    if !command.iter().any(|b| META.contains(b)) {
        let mut c = Command::new(cmd);
        c.args(args);
        return Ok(c);
    }

    let mut script = command.to_vec();
    if !args.is_empty() {
        script.extend_from_slice(b" \"$@\"");
    }
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(script.to_os_str()?).arg(cmd).args(args);
    Ok(c)
}

/// The bare strerror text of an I/O error, without Rust's ` (os error N)` tail,
/// so diagnostics read like git's.
fn errno_str(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Duplicate this process's stderr so it can be handed to a child as its stdout,
/// which is how git keeps hook output off its own stdout.
fn stderr_dup() -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd;
    Ok(std::io::stderr().as_fd().try_clone_to_owned()?)
}
