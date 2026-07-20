//! `git maintenance` — run tasks to optimize repository data.
//!
//! Three of the six subcommands are genuinely ported. Two are pure config
//! manipulation and need nothing beyond `gix-config`:
//!
//!   * `register [--config-file <path>]` — appends the repository's realpath to
//!     `maintenance.repo` in the global config (or `--config-file`), sets
//!     `maintenance.auto = false` in the repository's own config, and sets
//!     `maintenance.strategy = incremental` there when no value is already
//!     visible in the merged config. Idempotent, silent, exit 0.
//!   * `unregister [--config-file <path>] [-f|--force]` — removes that entry
//!     again, dropping the `[maintenance]` section once it holds nothing else
//!     (git's `git_config_set` does the same). Silent, exit 0; without
//!     `--force` an unregistered repository yields git's
//!     `fatal: repository '<path>' is not registered` on stderr, exit 128.
//!
//! The third needs no substrate at all, once git's actual rule is pinned down:
//!
//!   * `is-needed` (without `--auto`) answers "maintenance is needed", exit 0,
//!     silently and without touching the repository. git only consults a task's
//!     `auto_condition` under `--auto`; absent the flag every selected task
//!     counts as needed, so the answer is independent of the task set, of the
//!     `maintenance.<task>.enabled` config and of repository state. `--auto`
//!     itself is not ported — see `is_needed_sub`.
//!
//! Everything else validates its arguments exactly as git's parse-options does
//! — `-h` (usage on stdout, exit 129), unknown option/switch, missing option
//! value, stray positional, invalid `--task`/`--schedule`/`--scheduler` value —
//! and then bails naming the substrate that is missing, rather than exiting 0
//! and pretending the work happened:
//!
//!   * `run` needs the maintenance tasks themselves. `gc`, `loose-objects` and
//!     `incremental-repack` all need a pack writer that can delta-compress;
//!     `gix-pack`'s only mode is documented as "Copy base objects and deltas
//!     from packs, while non-packed objects will be treated as base objects
//!     (i.e. without trying to delta compress them)"
//!     (`gix-pack/src/data/output/entry/iter_from_counts.rs`). `commit-graph`
//!     needs a commit-graph writer; `gix-commitgraph` ships `file`, `init`,
//!     `access` and `verify` only, and is read-only. `reflog-expire` needs
//!     reflog rewriting, which `gix-ref` does not offer (it only ever appends as
//!     a side effect of a ref transaction). `prefetch` needs a fetch that
//!     rewrites refspecs into `refs/prefetch/`. `rerere-gc` and `worktree-prune`
//!     have no counterpart in the vendored crates at all. Reporting success here
//!     would be indistinguishable from stock git, which also prints nothing and
//!     exits 0 — the worst outcome for a differential harness that compares
//!     post-command repository state.
//!   * `is-needed --auto` needs git's per-task condition heuristics, which rest
//!     on a loose-object estimator that samples the `objects/17/` fanout
//!     directory and scales by 256, plus multi-pack-index state. That sampling
//!     is observable rather than incidental, and the answer is carried entirely
//!     by the exit code, so a wrong guess would be silent.
//!   * `start` and `stop` are OS scheduler integration — writing launchd plists,
//!     crontab stanzas, systemd units or schtasks entries and invoking
//!     `launchctl`/`crontab`/`systemctl`. None of that is repository work, none
//!     of it lives in gitoxide, and guessing at it would mutate machine-wide
//!     scheduler state.
//!
//! The `--task` name set, the `--schedule` frequency set and the `--scheduler`
//! value set below are validated so those error paths stay byte-identical even
//! though the tasks never run (checked against git 2.55.0).

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};

/// git's top-level usage block, byte-for-byte (49 bytes, git 2.55.0).
const TOP_USAGE: &str = "usage: git maintenance <subcommand> [<options>]\n\
                     \n";

/// `git maintenance run -h` (434 bytes).
const RUN_USAGE: &str = "usage: git maintenance run [--auto] [--[no-]quiet] [--task=<task>] [--schedule]\n\
                     \n\
                     \x20   --[no-]auto           run tasks based on the state of the repository\n\
                     \x20   --[no-]detach         perform maintenance in the background\n\
                     \x20   --[no-]schedule <frequency>\n\
                     \x20                         run tasks based on frequency\n\
                     \x20   --[no-]quiet          do not report progress or other information over stderr\n\
                     \x20   --task <task>         run a specific task\n\
                     \n";

/// `git maintenance register -h` (135 bytes).
const REGISTER_USAGE: &str = "usage: git maintenance register [--config-file <path>]\n\
                     \n\
                     \x20   --[no-]config-file <file>\n\
                     \x20                         use given config file\n\
                     \n";

/// `git maintenance unregister -h` (226 bytes).
const UNREGISTER_USAGE: &str = "usage: git maintenance unregister [--config-file <path>] [--force]\n\
                     \n\
                     \x20   --[no-]config-file <file>\n\
                     \x20                         use given config file\n\
                     \x20   -f, --[no-]force      return success even if repository was not registered\n\
                     \n";

/// `git maintenance start -h` (152 bytes).
const START_USAGE: &str = "usage: git maintenance start [--scheduler=<scheduler>]\n\
                     \n\
                     \x20   --scheduler <scheduler>\n\
                     \x20                         scheduler to trigger git maintenance run\n\
                     \n";

/// `git maintenance stop -h` (29 bytes).
const STOP_USAGE: &str = "usage: git maintenance stop\n\
                     \n";

/// `git maintenance is-needed -h` (185 bytes).
const IS_NEEDED_USAGE: &str = "usage: git maintenance is-needed [--task=<task>] [--schedule]\n\
                     \n\
                     \x20   --[no-]auto           run tasks based on the state of the repository\n\
                     \x20   --task <task>         check a specific task\n\
                     \n";

/// Every `--task=<task>` name git accepts, in the order of its `tasks[]` table.
const TASKS: [&str; 9] = [
    "prefetch",
    "loose-objects",
    "incremental-repack",
    "gc",
    "commit-graph",
    "pack-refs",
    "reflog-expire",
    "rerere-gc",
    "worktree-prune",
];

/// Every `--schedule=<frequency>` value git accepts.
const SCHEDULES: [&str; 3] = ["hourly", "daily", "weekly"];

/// Every `--scheduler=<scheduler>` value git accepts.
const SCHEDULERS: [&str; 5] = ["auto", "crontab", "systemd-timer", "launchctl", "schtasks"];

/// The multi-valued key holding the registry of maintained repositories.
const REPO_KEY: &str = "maintenance.repo";

/// `git maintenance` — dispatch to a subcommand.
///
/// `register` and `unregister` are ported; `run`, `is-needed`, `start` and
/// `stop` validate their arguments and then bail, naming the missing substrate.
/// See the module documentation.
pub fn maintenance(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `maintenance` is never a valid
    // subcommand of itself, so dropping a leading copy is unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("maintenance") => &args[1..],
        _ => args,
    };

    let Some(first) = args.first().map(String::as_str) else {
        return Ok(usage_error(TOP_USAGE, Some("need a subcommand")));
    };
    let rest = &args[1..];

    match first {
        "-h" => {
            print!("{TOP_USAGE}");
            Ok(ExitCode::from(129))
        }
        // git consumes `--` as end-of-options and then finds no subcommand,
        // whatever follows it.
        "--" => Ok(usage_error(TOP_USAGE, Some("need a subcommand"))),
        "run" => run_sub(rest),
        "start" => start_sub(rest),
        "stop" => stop_sub(rest),
        "register" => register_sub(rest),
        "unregister" => unregister_sub(rest),
        "is-needed" => is_needed_sub(rest),
        _ => match option_name(first) {
            Some(msg) => Ok(usage_error(TOP_USAGE, Some(&msg))),
            None => Ok(usage_error(
                TOP_USAGE,
                Some(&format!("unknown subcommand: `{first}'")),
            )),
        },
    }
}

/// git's parse-options failure shape: an optional `error: <msg>` line followed
/// by the usage block, both on stderr, exit 129. A stray positional produces the
/// usage block alone.
fn usage_error(usage: &str, msg: Option<&str>) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{usage}"),
        None => eprint!("{usage}"),
    }
    ExitCode::from(129)
}

/// git's `error: <msg>` line with no usage block after it, exit 129 — the shape
/// used for a missing option value and for a rejected `--scheduler` argument.
fn bare_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// The parse-options wording for an argument that looks like an option but is
/// not recognized: `unknown option \`<rest>'` for `--<rest>` (git quotes the
/// whole remainder, `--x=1` included) and `unknown switch \`<c>'` for the first
/// character of a short cluster. `None` when `arg` is a positional — a lone `-`
/// counts as a positional, as it does for git.
fn option_name(arg: &str) -> Option<String> {
    if let Some(long) = arg.strip_prefix("--") {
        return Some(format!("unknown option `{long}'"));
    }
    let short = arg.strip_prefix('-')?;
    let c = short.chars().next()?;
    Some(format!("unknown switch `{c}'"))
}

/// `git maintenance run` — validates arguments, then bails: no task can run.
fn run_sub(args: &[String]) -> Result<ExitCode> {
    let mut auto = false;
    let mut scheduled = false;
    let mut end_of_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            return Ok(usage_error(RUN_USAGE, None));
        }
        match a {
            "-h" => {
                print!("{RUN_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--" => end_of_opts = true,
            "--auto" => auto = true,
            "--no-auto" => auto = false,
            "--quiet" | "--no-quiet" | "--detach" | "--no-detach" => {}
            // git's `--schedule` callback rejects the negated form outright, at
            // the position it appears — before any later option is parsed.
            "--no-schedule" => {
                eprintln!("fatal: --no-schedule is not allowed");
                return Ok(ExitCode::from(128));
            }
            "--task" | "--schedule" => {
                let name = &a[2..];
                let Some(value) = args.get(i + 1) else {
                    return Ok(bare_error(&format!("option `{name}' requires a value")));
                };
                if let Some(code) = check_value(name, value, &mut scheduled)? {
                    return Ok(code);
                }
                i += 1;
            }
            _ if a.starts_with("--task=") => {
                if let Some(code) = check_value("task", &a["--task=".len()..], &mut scheduled)? {
                    return Ok(code);
                }
            }
            _ if a.starts_with("--schedule=") => {
                if let Some(code) =
                    check_value("schedule", &a["--schedule=".len()..], &mut scheduled)?
                {
                    return Ok(code);
                }
            }
            _ => match option_name(a) {
                Some(msg) => return Ok(usage_error(RUN_USAGE, Some(&msg))),
                None => return Ok(usage_error(RUN_USAGE, None)),
            },
        }
        i += 1;
    }

    // git rejects this combination after parsing, and dies rather than raising a
    // usage error.
    if auto && scheduled {
        eprintln!("fatal: options '--auto' and '--schedule=' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    bail!(
        "maintenance run is not ported: gc/loose-objects/incremental-repack need a delta-compressing \
         pack writer (gix-pack writes base objects only), commit-graph needs a commit-graph writer \
         (gix-commitgraph is read-only), reflog-expire needs reflog rewriting in gix-ref, prefetch \
         needs a refspec-rewriting fetch, and rerere-gc/worktree-prune have no counterpart in the \
         vendored crates (ported: register, unregister, and argument validation)"
    );
}

/// `git maintenance is-needed` — report whether maintenance would do work.
///
/// Exit 0 means "needed", exit 1 means "not needed"; nothing is ever printed and
/// nothing in the repository is touched. A repository is still required, but
/// only after parse-options has run: outside one git reports `fatal: not a git
/// repository ...` and exits 128, while `is-needed --task=bogus` outside a
/// repository reports the bad task name instead.
///
/// Without `--auto` the answer is unconditionally 0. git only consults a task's
/// `auto_condition` when `--auto` is given; with the flag absent every selected
/// task counts as needed, so the reply does not depend on the task set, on the
/// `maintenance.<task>.enabled` config, or on the state of the repository. That
/// was checked against git 2.55.0 in an empty repo, a bare repo, a freshly
/// `gc`-ed repo and a detached HEAD, for each of the nine task names and with
/// every task explicitly disabled — 0 in every case.
///
/// `--auto` is the part that is not ported. Its per-task conditions rest on
/// git's loose-object estimator, which counts the entries of the single
/// `objects/17/` fanout directory and multiplies by 256, then compares against
/// `gc.auto` scaled by the same factor. That sampling is observable: with 300
/// loose objects and `gc.auto=10` git answers "not needed" because the sampled
/// directory happens to be empty, while 900 loose objects and `gc.auto=1`
/// answers "needed". Reproducing the thresholds without git's source would be
/// guesswork, and since the answer is carried by the exit code alone a wrong
/// guess is silent.
///
/// Note that `--schedule` is *not* accepted here despite appearing in git's own
/// usage block — the option belongs to `run`, and `is-needed --schedule=daily`
/// reports ``unknown option `schedule=daily'``.
fn is_needed_sub(args: &[String]) -> Result<ExitCode> {
    let mut auto = false;
    let mut end_of_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            return Ok(usage_error(IS_NEEDED_USAGE, None));
        }
        match a {
            "-h" => {
                print!("{IS_NEEDED_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--" => end_of_opts = true,
            "--auto" => auto = true,
            "--no-auto" => auto = false,
            "--task" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(bare_error("option `task' requires a value"));
                };
                if let Some(code) = check_task(value) {
                    return Ok(code);
                }
                i += 1;
            }
            _ if a.starts_with("--task=") => {
                if let Some(code) = check_task(&a["--task=".len()..]) {
                    return Ok(code);
                }
            }
            _ => match option_name(a) {
                Some(msg) => return Ok(usage_error(IS_NEEDED_USAGE, Some(&msg))),
                None => return Ok(usage_error(IS_NEEDED_USAGE, None)),
            },
        }
        i += 1;
    }

    // git checks the repository only after parse-options has had its say, so
    // `is-needed --task=bogus` outside a repository still reports the bad task.
    if gix::discover(".").is_err() {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    }

    if auto {
        bail!(
            "maintenance is-needed --auto is not ported: the per-task conditions rest on git's \
             loose-object estimator, which samples the objects/17 fanout directory and scales by \
             256, and on multi-pack-index state; those thresholds have no counterpart in the \
             vendored crates and the answer is carried by the exit code alone, so guessing it \
             would be silently wrong (ported: is-needed without --auto, register, unregister, \
             and argument validation)"
        );
    }

    // No `--auto`: no condition is evaluated, so every selected task is needed.
    Ok(ExitCode::SUCCESS)
}

/// Reject an unknown `--task` value the way git's callback does: a lone
/// `error:` line, exit 129. `None` when the name is one git knows.
fn check_task(value: &str) -> Option<ExitCode> {
    (!TASKS.contains(&value)).then(|| bare_error(&format!("'{value}' is not a valid task")))
}

/// Validate a `--task` or `--schedule` value the way git's option callbacks do.
/// Returns `Some(exit_code)` when the value is rejected, `None` when accepted.
fn check_value(name: &str, value: &str, scheduled: &mut bool) -> Result<Option<ExitCode>> {
    match name {
        "task" => {
            if let Some(code) = check_task(value) {
                return Ok(Some(code));
            }
        }
        "schedule" => {
            if !SCHEDULES.contains(&value) {
                // git's `parse_schedule` dies rather than raising a usage error.
                eprintln!("fatal: unrecognized --schedule argument '{value}'");
                return Ok(Some(ExitCode::from(128)));
            }
            *scheduled = true;
        }
        _ => bail!("internal: unexpected option name {name:?}"),
    }
    Ok(None)
}

/// `git maintenance start` — validates arguments, then bails: no scheduler.
fn start_sub(args: &[String]) -> Result<ExitCode> {
    let mut end_of_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            return Ok(usage_error(START_USAGE, None));
        }
        match a {
            "-h" => {
                print!("{START_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--" => end_of_opts = true,
            "--scheduler" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(bare_error("option `scheduler' requires a value"));
                };
                if let Some(code) = check_scheduler(value) {
                    return Ok(code);
                }
                i += 1;
            }
            _ if a.starts_with("--scheduler=") => {
                if let Some(code) = check_scheduler(&a["--scheduler=".len()..]) {
                    return Ok(code);
                }
            }
            _ => match option_name(a) {
                Some(msg) => return Ok(usage_error(START_USAGE, Some(&msg))),
                None => return Ok(usage_error(START_USAGE, None)),
            },
        }
        i += 1;
    }

    bail!(
        "maintenance start is not ported: it installs an OS scheduler entry (launchd plist, \
         crontab stanza, systemd timer or schtasks task) and invokes launchctl/crontab/systemctl — \
         machine-wide state with no counterpart in the vendored crates \
         (ported: register, unregister, and argument validation)"
    );
}

/// Reject an unknown `--scheduler` value the way git's callback does: a lone
/// `error:` line, exit 129.
fn check_scheduler(value: &str) -> Option<ExitCode> {
    (!SCHEDULERS.contains(&value)).then(|| {
        bare_error(&format!("unrecognized --scheduler argument '{value}'"))
    })
}

/// `git maintenance stop` — takes no options at all; validates, then bails.
fn stop_sub(args: &[String]) -> Result<ExitCode> {
    let mut end_of_opts = false;
    for a in args {
        let a = a.as_str();
        if end_of_opts {
            return Ok(usage_error(STOP_USAGE, None));
        }
        match a {
            "-h" => {
                print!("{STOP_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--" => end_of_opts = true,
            _ => match option_name(a) {
                Some(msg) => return Ok(usage_error(STOP_USAGE, Some(&msg))),
                None => return Ok(usage_error(STOP_USAGE, None)),
            },
        }
    }

    bail!(
        "maintenance stop is not ported: it removes the OS scheduler entry installed by \
         `maintenance start` (launchd, crontab, systemd timer or schtasks) — machine-wide state \
         with no counterpart in the vendored crates \
         (ported: register, unregister, and argument validation)"
    );
}

/// `git maintenance register [--config-file <path>]`.
///
/// Writes `maintenance.auto = false` and, when no `maintenance.strategy` is
/// visible in the merged config, `maintenance.strategy = incremental` into the
/// repository's own config; then appends the repository's realpath to
/// `maintenance.repo` in the target config unless it is already listed. Prints
/// nothing and exits 0, as stock git does.
fn register_sub(args: &[String]) -> Result<ExitCode> {
    let config_file = match parse_config_file_opts(args, REGISTER_USAGE, false)? {
        Parsed::Error(code) => return Ok(code),
        Parsed::Ok { config_file, .. } => config_file,
    };

    let repo = gix::discover(".")?;
    let maintpath = maintpath(&repo)?;

    // Repository-local config first, matching git's ordering: `auto` is set
    // unconditionally, `strategy` only when nothing already provides a value.
    let local_path = repo.common_dir().join("config");
    let mut local = load_config(&local_path)?;
    local.set_raw_value("maintenance.auto", "false")?;
    if repo.config_snapshot().string("maintenance.strategy").is_none() {
        local.set_raw_value("maintenance.strategy", "incremental")?;
    }
    write_config(&local_path, &local)?;

    // Then the registry itself: the global config, or `--config-file`.
    let target = match config_file {
        Some(path) => path,
        None => global_config_path()?,
    };
    let mut file = load_config(&target)?;
    let already = file
        .raw_values(REPO_KEY)
        .unwrap_or_default()
        .iter()
        .any(|value| value == &maintpath);
    if !already {
        file.section_mut_or_create_new("maintenance", None::<&BStr>)?
            .push("repo", Some(maintpath.as_bstr()))?;
        write_config(&target, &file)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// `git maintenance unregister [--config-file <path>] [-f|--force]`.
///
/// Removes every `maintenance.repo` entry equal to the repository's realpath
/// from the target config, dropping the `[maintenance]` section once it holds
/// nothing else. Prints nothing and exits 0. Without `--force`, a repository
/// that is not listed produces git's `fatal:` line on stderr and exit 128.
fn unregister_sub(args: &[String]) -> Result<ExitCode> {
    let (config_file, force) = match parse_config_file_opts(args, UNREGISTER_USAGE, true)? {
        Parsed::Error(code) => return Ok(code),
        Parsed::Ok { config_file, force } => (config_file, force),
    };

    let repo = gix::discover(".")?;
    let maintpath = maintpath(&repo)?;

    let target = match config_file {
        Some(path) => path,
        None => global_config_path()?,
    };
    let mut file = load_config(&target)?;

    let matches: Vec<usize> = file
        .raw_values(REPO_KEY)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter_map(|(i, value)| (value == &maintpath).then_some(i))
        .collect();

    if matches.is_empty() {
        if force {
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!(
            "fatal: repository '{}' is not registered",
            maintpath.to_str_lossy()
        );
        return Ok(ExitCode::from(128));
    }

    {
        let mut values = file.raw_values_mut(REPO_KEY)?;
        // Descending, so no removal shifts an index that is still to be removed.
        for i in matches.into_iter().rev() {
            values.delete(i);
        }
    }

    // git's config writer drops a section that its last value just left empty.
    let emptied: Vec<_> = file
        .sections_and_ids()
        .filter(|(section, _)| {
            section.header().name().to_str_lossy() == "maintenance" && section.body().is_void()
        })
        .map(|(_, id)| id)
        .collect();
    for id in emptied {
        file.remove_section_by_id(id);
    }

    write_config(&target, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// Outcome of parsing the option set shared by `register` and `unregister`.
enum Parsed {
    Ok {
        config_file: Option<PathBuf>,
        force: bool,
    },
    Error(ExitCode),
}

/// Parse `--config-file <path>`/`--config-file=<path>`/`--no-config-file`, plus
/// `-f`/`--force`/`--no-force` when `with_force` is set, reporting usage errors
/// the way git's parse-options does.
fn parse_config_file_opts(args: &[String], usage: &str, with_force: bool) -> Result<Parsed> {
    let mut config_file: Option<PathBuf> = None;
    let mut force = false;
    let mut end_of_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            return Ok(Parsed::Error(usage_error(usage, None)));
        }
        match a {
            "-h" => {
                print!("{usage}");
                return Ok(Parsed::Error(ExitCode::from(129)));
            }
            "--" => end_of_opts = true,
            "--no-config-file" => config_file = None,
            "--config-file" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(Parsed::Error(bare_error(
                        "option `config-file' requires a value",
                    )));
                };
                config_file = Some(PathBuf::from(value));
                i += 1;
            }
            _ if a.starts_with("--config-file=") => {
                config_file = Some(PathBuf::from(&a["--config-file=".len()..]));
            }
            "-f" | "--force" if with_force => force = true,
            "--no-force" if with_force => force = false,
            _ => match option_name(a) {
                Some(msg) => return Ok(Parsed::Error(usage_error(usage, Some(&msg)))),
                None => return Ok(Parsed::Error(usage_error(usage, None))),
            },
        }
        i += 1;
    }
    Ok(Parsed::Ok { config_file, force })
}

/// The path git records in `maintenance.repo`: the worktree root if there is
/// one, else the git directory, with symlinks resolved (git's `strbuf_realpath`).
fn maintpath(repo: &gix::Repository) -> Result<BString> {
    let base = repo.workdir().unwrap_or_else(|| repo.path());
    let real = std::fs::canonicalize(base)?;
    let Some(text) = real.to_str() else {
        bail!("repository path is not valid UTF-8: {real:?}");
    };
    Ok(BString::from(text))
}

/// git's `git_global_config()`: `$GIT_CONFIG_GLOBAL` wins outright; otherwise
/// `~/.gitconfig`, except that the XDG file is preferred when it exists and
/// `~/.gitconfig` does not.
fn global_config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")))
        .map(|dir| dir.join("git").join("config"));

    match home {
        Some(home) => {
            let user = home.join(".gitconfig");
            if !user.exists() {
                if let Some(xdg) = xdg.filter(|path| path.exists()) {
                    return Ok(xdg);
                }
            }
            Ok(user)
        }
        None => xdg.ok_or_else(|| anyhow::anyhow!("$HOME is not set")),
    }
}

/// Parse `path` as a config file, or start from an empty one when it is absent
/// (git creates the file on first write). Includes are deliberately not
/// followed: entries must land in, and be removed from, this file alone.
fn load_config(path: &Path) -> Result<gix::config::File> {
    if path.exists() {
        Ok(gix::config::File::from_path_no_includes(
            path.to_owned(),
            gix::config::Source::User,
        )?)
    } else {
        Ok(gix::config::File::default())
    }
}

/// Serialize `file` back over `path`, creating parent directories as git does
/// for the XDG location. Everything untouched round-trips byte-for-byte.
fn write_config(path: &Path, file: &gix::config::File) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, file.to_bstring())?;
    Ok(())
}
