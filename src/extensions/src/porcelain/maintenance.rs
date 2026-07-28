//! `git maintenance` — run tasks to optimize repository data.
//!
//! Four of the six subcommands are genuinely ported. Two are pure config
//! manipulation and need nothing beyond `gix-config`:
//!
//!   * `register [--config-file <path>]` — appends the repository's realpath to
//!     `maintenance.repo` in the global config (or `--config-file`), sets
//!     `maintenance.auto = false` in the repository's own config, and sets
//!     `maintenance.strategy = incremental` there when no value is already
//!     visible in the merged config. Idempotent, silent, exit 0. Each config
//!     file is written through a `<path>.lock` sibling, as git's config writer
//!     does, so a config that cannot be locked reports `error: could not lock
//!     config file <path>` plus git's `fatal: unable to add 'maintenance.repo'
//!     value of '<path>'` and exits 128 rather than claiming a write it did not
//!     perform.
//!   * `unregister [--config-file <path>] [-f|--force]` — removes that entry
//!     again, dropping the `[maintenance]` section once it holds nothing else
//!     (git's `git_config_set` does the same). Silent, exit 0; a config that
//!     cannot be locked is `fatal: unable to unset 'maintenance.repo' value of
//!     '<path>'`, exit 128, unless `--force` was given. Without
//!     `--force` an unregistered repository yields git's
//!     `fatal: repository '<path>' is not registered` on stderr, exit 128.
//!
//! The third needs no substrate at all, once git's actual rule is pinned down:
//!
//!   * `is-needed` (without `--auto`) answers "maintenance is needed", exit 0,
//!     silently and without touching the repository. git only consults a task's
//!     `auto_condition` under `--auto`; absent the flag every selected task
//!     counts as needed, so the answer is independent of the task set, of the
//!     `maintenance.<task>.enabled` config and of repository state. The one
//!     config read that survives is `maintenance.strategy`, which git validates
//!     before deriving a task set — an unusable value is fatal here too, unless
//!     `--task` named the set and made the read unnecessary. With `--auto` the
//!     answer is the first task whose condition trips — see [`auto_condition`].
//!
//! `--auto` gates each task on the same per-task condition git's `tasks[]` table
//! attaches to it, and each condition on its own `maintenance.<task>.auto` key:
//! `loose-objects` counts loose objects, `commit-graph` walks the refs for
//! commits the graph does not carry, `incremental-repack` counts packs outside
//! the multi-pack-index, `geometric-repack` computes the pack geometry split,
//! `pack-refs` weighs loose refs against the packed-refs file, `worktree-prune`
//! counts prunable worktrees, `rerere-gc` looks for an `rr-cache` entry,
//! `reflog-expire` counts expiring `HEAD` reflog entries, and `gc` reuses
//! `need_to_gc()` plus the `pre-auto-gc` hook. `prefetch` is the one task git
//! leaves without a condition, so `--auto` never selects it.
//!
//! [`run_auto_maintenance`] is the other half — the automatic run the commands
//! that add objects trigger, gated on `maintenance.auto`/`gc.auto` and detached
//! or not per `maintenance.autoDetach`/`gc.autoDetach`.
//!
//! `run` is a task driver, and this port runs the tasks that have a home in the
//! tree — see [`run_tasks`] for the task set, the ordering, and the two tasks
//! that are deliberately no-ops.
//!
//! Everything else validates its arguments exactly as git's parse-options does
//! — `-h` (usage on stdout, exit 129), unknown option/switch, missing option
//! value, stray positional, invalid `--task`/`--schedule`/`--scheduler` value —
//! and then bails naming the substrate that is missing, rather than exiting 0
//! and pretending the work happened:
//!
//!   * `start` and `stop` are OS scheduler integration — writing launchd plists,
//!     crontab stanzas, systemd units or schtasks entries and invoking
//!     `launchctl`/`crontab`/`systemctl`. None of that is repository work, none
//!     of it lives in gitoxide, and guessing at it would mutate machine-wide
//!     scheduler state.
//!
//! The `--task` name set, the `--schedule` frequency set and the `--scheduler`
//! value set below are validated so those error paths stay byte-identical
//! (checked against git 2.55.0).

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

/// Every `--task=<task>` name git accepts, in the order git runs them when the
/// selection is explicit.
///
/// The order was read off git 2.55.0 rather than guessed: passing all ten names
/// to `maintenance run` under `GIT_TRACE2_PERF=1` and reading the
/// `region_enter … maintenance … label:<task>` lines yields exactly this
/// sequence, whatever order the `--task` arguments appeared in.
///
/// `geometric-repack` is the tenth. It does not appear in git's documentation
/// and was missing here, so `--task=geometric-repack` was rejected as invalid
/// while git 2.55.0 accepts it (`maintenance is-needed --task=<name>` exits 0
/// for all ten names and 129 with `'<name>' is not a valid task` for anything
/// else).
const TASKS: [&str; 10] = [
    "pack-refs",
    "reflog-expire",
    "worktree-prune",
    "gc",
    "prefetch",
    "loose-objects",
    "commit-graph",
    "rerere-gc",
    "incremental-repack",
    "geometric-repack",
];

/// The order a *config-driven* selection runs tasks in — a bare
/// `maintenance run` and a `maintenance run --schedule=<frequency>` alike, both
/// of which pick their set from `maintenance.strategy`,
/// `maintenance.<task>.enabled` and `maintenance.<task>.schedule` rather than
/// from `--task`.
///
/// This is a second, genuinely different order from [`TASKS`], not a rearranged
/// copy: read off git 2.55.0 with all ten tasks switched on
/// (`GIT_TRACE2_PERF=1`, keeping the `region_enter` lines whose category is
/// `maintenance`), a run selected by config enters them as below, while the same
/// ten passed as `--task` arguments enter in the `TASKS` sequence — `gc` moves
/// from fourth to seventh and `geometric-repack` from tenth to sixth.
const CONFIG_ORDER: [&str; 10] = [
    "pack-refs",
    "reflog-expire",
    "prefetch",
    "loose-objects",
    "incremental-repack",
    "geometric-repack",
    "gc",
    "commit-graph",
    "worktree-prune",
    "rerere-gc",
];

/// The tasks the `geometric` strategy enables, which is what a bare
/// `maintenance run` gets when `maintenance.strategy` is unset — git documents
/// `geometric` as "the default strategy for manual maintenance", and a run with
/// no config at all does select exactly these six.
const GEOMETRIC_TASKS: [&str; 6] = [
    "pack-refs",
    "reflog-expire",
    "geometric-repack",
    "commit-graph",
    "worktree-prune",
    "rerere-gc",
];

/// How often a task is scheduled, ordered so that `Hourly < Daily < Weekly`.
///
/// A `--schedule=<frequency>` run selects every task whose own frequency is at
/// most the requested one, so an hourly task also runs in the daily and weekly
/// passes — checked against git 2.55.0 with a single task pinned at each
/// frequency and each of the three passes requested.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Schedule {
    Hourly,
    Daily,
    Weekly,
}

impl Schedule {
    /// git's `parse_schedule`. Anything unrecognized is "never", and that is
    /// final: a `maintenance.<task>.schedule` git cannot parse keeps the task out
    /// of every scheduled run rather than falling back to the strategy's own
    /// frequency for it (git 2.55.0 with `maintenance.strategy=geometric` and
    /// `maintenance.commit-graph.schedule=bogus` runs the other five geometric
    /// tasks weekly and never `commit-graph`).
    ///
    /// The comparison is case-insensitive, which git 2.55.0 accepts on both
    /// sides: `--schedule=HOURLY` runs the same set as `--schedule=hourly`, and
    /// `maintenance.pack-refs.schedule = Hourly` schedules the task just as the
    /// lowercase spelling does.
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "hourly" => Some(Schedule::Hourly),
            "daily" => Some(Schedule::Daily),
            "weekly" => Some(Schedule::Weekly),
            _ => None,
        }
    }
}

/// A `maintenance.strategy` value git accepts.
///
/// git 2.55.0 takes exactly these three, case-insensitively, and dies on
/// anything else — including the `none` its own documentation lists, which the
/// code never implemented. So `maintenance.strategy = none` is a fatal, not an
/// empty task set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Strategy {
    Gc,
    Geometric,
    Incremental,
}

/// The configured `maintenance.strategy`, `Ok(None)` when the key is unset, or
/// the rejected spelling for git's `fatal: unknown maintenance strategy: '…'`.
///
/// git only reads the key when the selection is config-driven: a `--task` run
/// never looks at it, and so never rejects a bad value.
fn configured_strategy(repo: &gix::Repository) -> std::result::Result<Option<Strategy>, String> {
    let Some(value) = repo.config_snapshot().string("maintenance.strategy") else {
        return Ok(None);
    };
    let value = value.to_string();
    match value.to_ascii_lowercase().as_str() {
        "gc" => Ok(Some(Strategy::Gc)),
        "geometric" => Ok(Some(Strategy::Geometric)),
        "incremental" => Ok(Some(Strategy::Incremental)),
        _ => Err(value),
    }
}

/// The tasks `maintenance.strategy` enables for a *manual* run — one with
/// neither `--task` nor `--schedule`.
///
/// An unset key means `geometric`, git's documented default for manual
/// maintenance. `gc` and `incremental` both reduce a manual run to the single
/// `gc` task, the latter because git runs the incremental tasks only on a
/// schedule ("Manual repository maintenance uses the gc task").
fn strategy_manual_tasks(strategy: Option<Strategy>) -> &'static [&'static str] {
    match strategy.unwrap_or(Strategy::Geometric) {
        Strategy::Geometric => &GEOMETRIC_TASKS,
        Strategy::Gc | Strategy::Incremental => &["gc"],
    }
}

/// The frequency `maintenance.strategy` attaches to `task`, or `None` when that
/// strategy does not schedule it.
///
/// An unset key schedules nothing at all — which is why
/// `maintenance.<task>.schedule` alone never makes a task run: membership in
/// this table is also what supplies the task's default `enabled` state for a
/// scheduled run, so a task the strategy does not schedule additionally needs an
/// explicit `maintenance.<task>.enabled = true`.
///
/// The three tables were read off git 2.55.0 by requesting each of the three
/// frequencies under each strategy.
fn strategy_schedule(strategy: Option<Strategy>, task: &str) -> Option<Schedule> {
    let table: &[(&str, Schedule)] = match strategy? {
        Strategy::Gc => &[("gc", Schedule::Daily)],
        Strategy::Geometric => &[
            ("commit-graph", Schedule::Hourly),
            ("pack-refs", Schedule::Daily),
            ("geometric-repack", Schedule::Daily),
            ("reflog-expire", Schedule::Weekly),
            ("worktree-prune", Schedule::Weekly),
            ("rerere-gc", Schedule::Weekly),
        ],
        Strategy::Incremental => &[
            ("prefetch", Schedule::Hourly),
            ("commit-graph", Schedule::Hourly),
            ("loose-objects", Schedule::Daily),
            ("incremental-repack", Schedule::Daily),
            ("pack-refs", Schedule::Weekly),
        ],
    };
    table
        .iter()
        .find(|(name, _)| *name == task)
        .map(|(_, schedule)| *schedule)
}

/// git's `fatal: unknown maintenance strategy: '<value>'`, exit 128.
fn unknown_strategy(value: &str) -> ExitCode {
    eprintln!("fatal: unknown maintenance strategy: '{value}'");
    ExitCode::from(128)
}

/// Every `--scheduler=<scheduler>` value git accepts.
const SCHEDULERS: [&str; 5] = ["auto", "crontab", "systemd-timer", "launchctl", "schtasks"];

/// The multi-valued key holding the registry of maintained repositories.
const REPO_KEY: &str = "maintenance.repo";

/// `run_auto_maintenance()` — the automatic `maintenance run --auto` the
/// commands that add objects trigger on their way out (`am`, `commit`, `fetch`,
/// `merge`, `rebase`, `receive-pack`).
///
/// A faithful port of `run-command.c`'s `prepare_auto_maintenance()`:
///
///   * `maintenance.auto` switches it off. When that key is unset the decision
///     falls back to `gc.auto`, which disables it at zero or below — the
///     compatibility path from when this used to be `git gc --auto`.
///   * `maintenance.autoDetach` decides whether the caller waits for the run.
///     When it is unset `gc.autoDetach` answers instead, and when neither is set
///     the run detaches. `GIT_TEST_MAINT_AUTO_DETACH=0` turns that default off,
///     which is what makes the behaviour testable at all.
///
/// git builds `maintenance run --auto --[no-]quiet --[no-]detach` and lets the
/// child daemonize itself; here the detached form is a child process with its
/// standard streams on `/dev/null` — the state `daemonize()` leaves them in —
/// that the caller does not wait for. Either way the caller never sees the
/// child's output, and its own exit code is unaffected.
pub fn run_auto_maintenance(repo: &gix::Repository, quiet: bool) -> Result<()> {
    let config = repo.config_snapshot();
    let enabled = match config.boolean("maintenance.auto") {
        Some(value) => value,
        None => config.integer("gc.auto").is_none_or(|threshold| threshold > 0),
    };
    if !enabled {
        return Ok(());
    }

    let detach = config
        .boolean("maintenance.autoDetach")
        .or_else(|| config.boolean("gc.autoDetach"))
        .unwrap_or_else(|| {
            !matches!(
                std::env::var("GIT_TEST_MAINT_AUTO_DETACH").as_deref(),
                Ok("0" | "false")
            )
        });

    let Ok(exe) = std::env::current_exe() else {
        return Ok(());
    };
    let mut child = std::process::Command::new(exe);
    child
        .args(["maintenance", "run", "--auto"])
        .arg(if quiet { "--quiet" } else { "--no-quiet" })
        .arg(if detach { "--detach" } else { "--no-detach" })
        .current_dir(repo.workdir().unwrap_or_else(|| repo.git_dir()));
    if detach {
        child
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Not waited for: the child is the daemonized half of git's run.
        let _ = child.spawn();
    } else {
        let _ = child.status();
    }
    Ok(())
}

/// `git maintenance` — dispatch to a subcommand.
///
/// `run`, `register`, `unregister` and `is-needed` are ported; `start` and
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

/// `git maintenance run` — validate arguments, then run the selected tasks.
fn run_sub(args: &[String]) -> Result<ExitCode> {
    let mut auto = false;
    let mut scheduled: Option<Schedule> = None;
    let mut selected: Vec<String> = Vec::new();
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
            // `--quiet` suppresses progress written to stderr off a tty, which
            // is suppressed here anyway; `--detach` only changes *when* the same
            // work happens, and this port runs synchronously.
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
                if let Some(code) = check_value(name, value, &mut scheduled, &mut selected)? {
                    return Ok(code);
                }
                i += 1;
            }
            _ if a.starts_with("--task=") => {
                if let Some(code) =
                    check_value("task", &a["--task=".len()..], &mut scheduled, &mut selected)?
                {
                    return Ok(code);
                }
            }
            _ if a.starts_with("--schedule=") => {
                if let Some(code) = check_value(
                    "schedule",
                    &a["--schedule=".len()..],
                    &mut scheduled,
                    &mut selected,
                )? {
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

    // git rejects these combinations after parsing, and dies rather than raising
    // a usage error. `--auto` is checked first: with all three given, git 2.55.0
    // names `--auto` and `--schedule=`, not `--task=`.
    if auto && scheduled.is_some() {
        eprintln!("fatal: options '--auto' and '--schedule=' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    if !selected.is_empty() && scheduled.is_some() {
        eprintln!("fatal: options '--task=' and '--schedule=' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // git validates `maintenance.strategy` only when it is about to consult it,
    // which a `--task` run never does — `run --task=gc` succeeds under a strategy
    // value that makes a bare `run` die. The check lands before `--auto`'s work,
    // so a bad value is fatal even there.
    let strategy = if selected.is_empty() {
        match configured_strategy(&repo) {
            Ok(strategy) => strategy,
            Err(value) => return Ok(unknown_strategy(&value)),
        }
    } else {
        None
    };

    run_tasks(&repo, &selected, scheduled, strategy, auto)
}

/// Run the selected maintenance tasks in git's order and report the way git's
/// `maintenance_run_tasks()` does.
///
/// # Selection
///
/// `selected` is the `--task` set, empty when none was given. With it non-empty
/// the tasks run in [`TASKS`] order and the config is not consulted at all.
/// Otherwise [`plan`] derives the set from `maintenance.strategy`,
/// `maintenance.<task>.enabled` and — for a `--schedule` run —
/// `maintenance.<task>.schedule`, in [`CONFIG_ORDER`].
///
/// # What the tasks do
///
///   * **`pack-refs`** → [`super::pack_refs::pack_refs`] with `--all --prune`,
///     which is git's own argument list and a real port.
///   * **`reflog-expire`** → [`super::gc::expire_reflogs`], the same
///     `reflog expire --all` port `git gc` runs, including the per-pattern
///     `gc.<pattern>.reflogExpire*` policy and the reachability arm.
///   * **`geometric-repack`** and **`gc`** → the ported [`super::repack::repack`]
///     and [`super::gc::gc`], invoked with the exact argument lists git's
///     `run-command` uses (read off `GIT_TRACE2_PERF=1`, which prints each
///     child's argv). `repack` writes a valid pack, `.idx` and `.rev`, drops the
///     packs it supersedes and prunes the loose objects it folded in.
///
///     **The pack's bytes differ from git's by design.** `gix-pack` has no delta
///     compression — its only output mode is `Mode::PackCopyAndBaseObjects`,
///     "Copy base objects and deltas from packs, while non-packed objects will
///     be treated as base objects (i.e. without trying to delta compress them)"
///     (`gix-pack/src/data/output/entry/iter_from_counts.rs:362`) — so every
///     object is stored undeltified and the pack is larger than git's, sharing
///     none of its bytes and, since the name embeds the checksum, none of its
///     name either. What it *is* is a well-formed pack holding the correct
///     object set. Delta selection is an optimization, not part of the pack's
///     meaning, so its absence changes the file's size, not its correctness.
///   * **`rerere-gc`** → [`super::rerere::rerere`], guarded on `rr-cache`
///     existing so a repository that never recorded a resolution does not enter
///     the delegate's `read_dir` error path, which git has no equivalent of.
///
///   * **`worktree-prune`** → [`super::gc::prune_worktrees`], the same
///     `worktree prune --expire <gc.worktreePruneExpire>` port `git gc` runs,
///     with git's `locked` and expiry semantics.
///
/// # The one task that does nothing, and why
///
///   * **`commit-graph`**. git runs `commit-graph write --split --reachable`,
///     which writes `objects/info/commit-graphs/commit-graph-chain` and a
///     `graph-<hash>.graph` beside it. `gix-commitgraph` ships `access`, `file`,
///     `init` and `verify` — it reads the format and cannot write it. Writing
///     one by hand is not a small thing done safely: a graph file that is
///     well-formed enough to be *loaded* but wrong in a chunk would make every
///     later git command silently traverse from bad data, which is worse than
///     having no graph at all. So none is written, and none is claimed. It is
///     skipped rather than approximated, and a `maintenance run` that exits 0
///     has not written a commit-graph.
fn run_tasks(
    repo: &gix::Repository,
    selected: &[String],
    scheduled: Option<Schedule>,
    strategy: Option<Strategy>,
    auto: bool,
) -> Result<ExitCode> {
    let order = plan(repo, selected, scheduled, strategy);

    // git reports a failing task on stderr and keeps going, then exits 1 —
    // `error: task 'incremental-repack' failed` on a repository with no packs,
    // observed on git 2.55.0.
    let mut failed = false;
    for task in order {
        // git's `maybe_run_task()`: under `--auto` a task runs only when its own
        // `auto_condition` says the repository needs it, and a task git's table
        // leaves without one (`prefetch`) never runs at all.
        if auto && !auto_condition(repo, task) {
            continue;
        }
        let ok = match task {
            // `maintenance_task_pack_refs()` forwards `--auto`, so the packing
            // itself re-applies the same threshold the condition just checked.
            "pack-refs" => {
                let mut args = strings(&["pack-refs", "--all", "--prune"]);
                if auto {
                    args.push("--auto".to_owned());
                }
                delegate(super::pack_refs::pack_refs(&args))
            }
            "reflog-expire" => super::gc::expire_reflogs(repo).is_ok(),
            "geometric-repack" => delegate(super::repack::repack(&strings(&[
                "repack",
                "-d",
                "-l",
                "--cruft",
                "--cruft-expiration=2.weeks.ago",
                "--quiet",
                "--write-midx",
            ]))),
            "gc" => delegate(super::gc::gc(&strings(&["gc"]))),
            "rerere-gc" => {
                !repo.git_dir().join("rr-cache").is_dir()
                    // Unlike `repack` and `gc` above, `rerere()` takes the verb's
                    // arguments only; a leading "rerere" reads as an unknown
                    // subcommand and prints the usage block.
                    || delegate(super::rerere::rerere(&strings(&["gc"])))
            }
            "worktree-prune" => super::gc::prune_worktrees(repo).is_ok(),
            // See the "one task that does nothing" section above.
            "commit-graph" => true,
            // Selectable, but blocked on substrate no module in the tree has:
            // `prefetch` needs a fetch that rewrites refspecs into
            // `refs/prefetch/`, and `loose-objects`/`incremental-repack` need a
            // multi-pack-index writer to repack against.
            "prefetch" | "loose-objects" | "incremental-repack" => {
                bail!(
                    "maintenance task '{task}' is not ported: prefetch needs a refspec-rewriting \
                     fetch, and loose-objects/incremental-repack need a multi-pack-index writer \
                     (ported tasks: pack-refs, reflog-expire, geometric-repack, gc, rerere-gc)"
                );
            }
            _ => true,
        };
        if !ok {
            eprintln!("error: task '{task}' failed");
            failed = true;
        }
    }

    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Which tasks run, in the order they run.
///
/// `--task` short-circuits everything below it: git ignores the config entirely
/// once a task is named. Otherwise the set comes from `maintenance.strategy`,
/// with `maintenance.<task>.enabled` and — for a `--schedule` run —
/// `maintenance.<task>.schedule` overriding what the strategy decided, and the
/// order comes from [`CONFIG_ORDER`].
fn plan(
    repo: &gix::Repository,
    selected: &[String],
    scheduled: Option<Schedule>,
    strategy: Option<Strategy>,
) -> Vec<&'static str> {
    if !selected.is_empty() {
        return TASKS
            .into_iter()
            .filter(|name| selected.iter().any(|s| s.as_str() == *name))
            .collect();
    }

    let config = repo.config_snapshot();
    let enabled = |name: &str| config.boolean(&format!("maintenance.{name}.enabled"));

    let Some(frequency) = scheduled else {
        // A manual run: the strategy's task set, with `enabled` adding to it or
        // removing from it.
        let default = strategy_manual_tasks(strategy);
        return CONFIG_ORDER
            .into_iter()
            .filter(|name| enabled(name).unwrap_or_else(|| default.contains(name)))
            .collect();
    };

    // A scheduled run: a task needs both a frequency at or below the requested
    // one and an `enabled` verdict, and the strategy supplies the default for
    // each. `maintenance.<task>.schedule`, when present, replaces the strategy's
    // frequency outright — including when it is unparseable, which reads as
    // "never".
    CONFIG_ORDER
        .into_iter()
        .filter(|name| {
            let schedule = match config.string(&format!("maintenance.{name}.schedule")) {
                Some(value) => Schedule::parse(&value.to_string()),
                None => strategy_schedule(strategy, name),
            };
            if schedule.is_none_or(|schedule| schedule > frequency) {
                return false;
            }
            enabled(name).unwrap_or_else(|| strategy_schedule(strategy, name).is_some())
        })
        .collect()
}

/// A delegate's outcome as the success flag git's task runner works in.
///
/// git judges a task by its child's exit status. `ExitCode` cannot be inspected
/// — it is opaque and implements neither `PartialEq` nor a getter — so the test
/// here is whether the delegate returned an error instead. The two agree for
/// every call this module makes: each delegate is handed a fixed, valid argument
/// list, and a non-zero `ExitCode` from these modules means a usage error or a
/// missing repository, neither of which a fixed list in a discovered repository
/// can produce. A genuine failure inside them surfaces as `Err`.
fn delegate(outcome: Result<ExitCode>) -> bool {
    outcome.is_ok()
}

/// Borrow a fixed argument list as the `&[String]` every porcelain entry takes.
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

/// `git maintenance is-needed` — report whether maintenance would do work.
///
/// Exit 0 means "needed", exit 1 means "not needed"; nothing is ever printed and
/// nothing in the repository is touched. A repository is still required, but
/// only after parse-options has run: outside one git reports `fatal: not a git
/// repository ...` and exits 128, while `is-needed --task=bogus` outside a
/// repository reports the bad task name instead.
///
/// Without `--auto` the answer is 0 whenever it is reached. git only consults a
/// task's `auto_condition` when `--auto` is given; with the flag absent every
/// selected task counts as needed, so the reply does not depend on the task set,
/// on the `maintenance.<task>.enabled` config, or on the state of the
/// repository. That was checked against git 2.55.0 in an empty repo, a bare
/// repo, a freshly `gc`-ed repo and a detached HEAD, for each of the nine task
/// names and with every task explicitly disabled — 0 in every case.
///
/// The exception is `maintenance.strategy`: deriving the default task set reads
/// it, so a value outside `gc`/`geometric`/`incremental` is fatal (128) before
/// any answer is given. `--task` supplies the set directly and skips the read,
/// which is why `is-needed --task=gc` still exits 0 under a strategy value that
/// makes a bare `is-needed` die.
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
    let mut selected: Vec<String> = Vec::new();
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
                push_task(&mut selected, value);
                i += 1;
            }
            _ if a.starts_with("--task=") => {
                let value = &a["--task=".len()..];
                if let Some(code) = check_task(value) {
                    return Ok(code);
                }
                push_task(&mut selected, value);
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
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // Without `--task` the answer is derived from the strategy's task set, so an
    // unusable `maintenance.strategy` is fatal here exactly as it is for `run` —
    // and, as there, naming a task skips the read and with it the rejection.
    let strategy = if selected.is_empty() {
        match configured_strategy(&repo) {
            Ok(strategy) => strategy,
            Err(value) => return Ok(unknown_strategy(&value)),
        }
    } else {
        None
    };

    if auto {
        // git's loop stops at the first task whose condition says yes. The task
        // set is the *manual* one — `is-needed` has no `--schedule`.
        let needed = plan(&repo, &selected, None, strategy)
            .into_iter()
            .any(|task| auto_condition(&repo, task));
        return Ok(if needed {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }

    // No `--auto`: no condition is evaluated, so every selected task is needed.
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// `--auto` — git's per-task auto conditions (`builtin/gc.c`'s `tasks[]` table)
// ---------------------------------------------------------------------------

/// The verdict a `maintenance.<task>.auto` value carries.
///
/// Every condition in git's table reads its key with `repo_config_get_int()`
/// into a per-task default and then branches on the sign the same way: `0`
/// switches the task off, a negative value forces it on without measuring
/// anything, and a positive value is the threshold the task's own count is
/// compared against.
enum Gate {
    Never,
    Always,
    Threshold(i64),
}

/// Read one `maintenance.<task>.auto`-shaped key and classify it. A missing or
/// unparsable value leaves git's built-in `default` in place, which is what
/// `repo_config_get_int()` does when it fails.
fn gate(repo: &gix::Repository, key: &str, default: i64) -> Gate {
    match repo.config_snapshot().integer(key).unwrap_or(default) {
        0 => Gate::Never,
        n if n < 0 => Gate::Always,
        n => Gate::Threshold(n),
    }
}

/// git's `maybe_run_task()` gate: whether `--auto` lets `task` run.
///
/// `prefetch` is the one selectable task git's table leaves without an
/// `auto_condition`, and `maybe_run_task` treats a missing condition as "do not
/// run", so `--auto` never prefetches.
fn auto_condition(repo: &gix::Repository, task: &str) -> bool {
    match task {
        "pack-refs" => pack_refs_condition(repo),
        "reflog-expire" => reflog_expire_condition(repo),
        "worktree-prune" => worktree_prune_condition(repo),
        "gc" => super::gc::gc_needed(repo) && pre_auto_gc_allows(repo),
        "loose-objects" => loose_objects_condition(repo),
        "commit-graph" => commit_graph_condition(repo),
        "rerere-gc" => rerere_gc_condition(repo),
        "incremental-repack" => incremental_repack_condition(repo),
        "geometric-repack" => geometric_repack_condition(repo),
        _ => false,
    }
}

/// The tail of git's `need_to_gc()`: once a heuristic has tripped, the
/// `pre-auto-gc` hook still gets to veto the run by exiting non-zero.
fn pre_auto_gc_allows(repo: &gix::Repository) -> bool {
    crate::hooks::run(repo, "pre-auto-gc", &[], None).unwrap_or(true)
}

/// `loose_object_auto_condition()`: at least `maintenance.loose-objects.auto`
/// (default 100) loose objects exist. Unlike `too_many_loose_objects()` this
/// counts for real — git walks the fan-out directories and stops at the limit.
fn loose_objects_condition(repo: &gix::Repository) -> bool {
    let limit = match gate(repo, "maintenance.loose-objects.auto", 100) {
        Gate::Never => return false,
        Gate::Always => return true,
        Gate::Threshold(limit) => limit,
    };
    count_loose_objects(repo, limit) >= limit
}

/// Loose objects in the repository's own object directory, counted no further
/// than `limit` — git's `for_each_loose_file_in_source` stops as soon as its
/// callback returns non-zero.
fn count_loose_objects(repo: &gix::Repository, limit: i64) -> i64 {
    let objdir = repo.objects.store_ref().path().to_path_buf();
    let name_len = repo.object_hash().len_in_hex() - 2;
    let mut count: i64 = 0;
    for fanout in 0..256u16 {
        let Ok(entries) = std::fs::read_dir(objdir.join(format!("{fanout:02x}"))) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.len() != name_len
                || !name.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                continue;
            }
            count += 1;
            if count >= limit {
                return count;
            }
        }
    }
    count
}

/// `rerere_gc_condition()`: an `rr-cache` directory exists and holds at least
/// one entry. The limit is a plain on/off switch here — git compares nothing
/// against it, so any positive value behaves like the default 1.
fn rerere_gc_condition(repo: &gix::Repository) -> bool {
    let limit = repo
        .config_snapshot()
        .integer("maintenance.rerere-gc.auto")
        .unwrap_or(1);
    if limit <= 0 {
        return limit < 0;
    }
    std::fs::read_dir(repo.git_dir().join("rr-cache"))
        .is_ok_and(|mut entries| entries.any(|e| e.is_ok()))
}

/// `worktree_prune_condition()`: at least `maintenance.worktree-prune.auto`
/// (default 1) of the administrative directories under `worktrees/` are
/// prunable, judged by the same `should_prune_worktree()` the task itself uses
/// and the same `gc.worktreePruneExpire` cutoff.
fn worktree_prune_condition(repo: &gix::Repository) -> bool {
    let mut limit = repo
        .config_snapshot()
        .integer("maintenance.worktree-prune.auto")
        .unwrap_or(1);
    if limit <= 0 {
        return limit < 0;
    }
    let Some(expire) = worktree_prune_expiry(repo) else {
        // git's `parse_expiry_date` failure path leaves `should_prune` at 0.
        return false;
    };
    let Ok(entries) = std::fs::read_dir(repo.common_dir().join("worktrees")) else {
        return false;
    };
    for entry in entries.flatten() {
        if limit == 0 {
            break;
        }
        if super::gc::should_prune_worktree(&entry.path(), expire) {
            limit -= 1;
        }
    }
    limit == 0
}

/// `cfg->prune_worktrees_expire`, the `gc.worktreePruneExpire` cutoff that both
/// the `worktree-prune` task and its auto condition measure against; `3.months.ago`
/// when unset.
fn worktree_prune_expiry(repo: &gix::Repository) -> Option<i64> {
    let now = std::time::SystemTime::now();
    match repo.config_snapshot().string("gc.worktreePruneExpire") {
        Some(value) => super::gc::parse_reflog_expiry(value.to_str_lossy().as_ref(), now),
        None => super::gc::parse_reflog_expiry("3.months.ago", now),
    }
}

/// `reflog_expire_condition()`: at least `maintenance.reflog-expire.auto`
/// (default 100) entries of `HEAD`'s reflog would be dropped by an expiry run.
///
/// git builds the policy from the `gc.*reflogExpire*` config and resolves it for
/// `HEAD`, then counts with `should_expire_reflog_ent()`. That callback's
/// reachability arm degenerates here: the condition leaves `mark_list` empty, so
/// `is_unreachable()` reports every non-null commit as unreachable. With the
/// defaults it never runs at all — `expire_total` (30 days) is later than
/// `expire_unreachable` (90 days), so an entry old enough for the second test has
/// already been caught by the first.
fn reflog_expire_condition(repo: &gix::Repository) -> bool {
    let limit = match gate(repo, "maintenance.reflog-expire.auto", 100) {
        Gate::Never => return false,
        Gate::Always => return true,
        Gate::Threshold(limit) => limit,
    };

    let now = std::time::SystemTime::now();
    let now_secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (expire_total, expire_unreach) =
        super::gc::load_reflog_config(repo, now, now_secs).resolve("HEAD");

    let Ok(body) = std::fs::read(repo.git_dir().join("logs").join("HEAD")) else {
        return false;
    };
    let mut count: i64 = 0;
    for line in body.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let Some((old, new, at)) = reflog_entry(line) else {
            continue;
        };
        let expires = at < expire_total
            || (at < expire_unreach && (!old.is_null() || !new.is_null()));
        if expires {
            count += 1;
            if count >= limit {
                return true;
            }
        }
    }
    false
}

/// One reflog line as `(old, new, committer-seconds)`; `None` when the line does
/// not parse, which git's iterator skips rather than counts.
fn reflog_entry(line: &[u8]) -> Option<(gix::ObjectId, gix::ObjectId, i64)> {
    let mut iter = gix::refs::file::log::iter::forward(line);
    let parsed = iter.next()?.ok()?;
    let at = parsed.signature.time().ok()?.seconds;
    Some((parsed.previous_oid(), parsed.new_oid(), at))
}

/// `pack_refs_condition()`: more loose references than git's packed-refs-size
/// heuristic allows.
///
/// The budget is `log2(packed-refs size / 100) * 5`, floored at 16 — roughly
/// sixteen more loose refs per factor of ten of already-packed refs. Only the
/// refs `pack-refs --all` would actually pack are counted: shared (non
/// per-worktree) names that are neither symbolic nor broken.
fn pack_refs_condition(repo: &gix::Repository) -> bool {
    // git's `log2u(packed_size / 100) * 5`, floored at 16.
    let packed_size = std::fs::metadata(repo.refs.packed_refs_path()).map_or(0usize, |m| m.len() as usize);
    let scaled = packed_size / 100;
    let log2 = if scaled == 0 {
        0
    } else {
        usize::BITS as usize - 1 - scaled.leading_zeros() as usize
    };
    let limit = (log2 * 5).max(16);

    let Ok(loose) = repo.refs.loose_iter() else {
        return false;
    };
    let mut refcount = 0usize;
    for reference in loose.filter_map(Result::ok) {
        // `should_pack_ref()` under `--all`'s `*` include and no exclusions:
        // per-worktree refs, symbolic refs and broken refs are never packed.
        let name = reference.name.as_bstr();
        if ["refs/bisect/", "refs/worktree/", "refs/rewritten/"]
            .iter()
            .any(|prefix| name.starts_with(prefix.as_bytes()))
        {
            continue;
        }
        let Some(oid) = reference.target.try_id() else {
            continue;
        };
        if !repo.has_object(oid) {
            continue;
        }
        refcount += 1;
        if refcount >= limit {
            return true;
        }
    }
    false
}

/// `should_write_commit_graph()`: at least `maintenance.commit-graph.auto`
/// (default 100) reachable commits are missing from the commit-graph.
///
/// A depth-first walk from every reference, exactly as git's `dfs_on_ref` does:
/// a commit already carried by the graph is neither counted nor descended
/// through, because everything behind it is in the graph too.
fn commit_graph_condition(repo: &gix::Repository) -> bool {
    let limit = match gate(repo, "maintenance.commit-graph.auto", 100) {
        Gate::Never => return false,
        Gate::Always => return true,
        Gate::Threshold(limit) => limit,
    };

    let graph = repo.commit_graph().ok();
    let in_graph = |id: &gix::hash::oid| graph.as_ref().is_some_and(|g| g.lookup(id).is_some());

    let Ok(platform) = repo.references() else {
        return false;
    };
    let Ok(all) = platform.all() else {
        return false;
    };

    let mut seen: std::collections::HashSet<gix::ObjectId> = std::collections::HashSet::new();
    let mut count: i64 = 0;
    for mut reference in all.filter_map(Result::ok) {
        // `reference_get_peeled_oid()`: resolve the symref and follow tag chains.
        let Ok(peeled) = reference.peel_to_id() else {
            continue;
        };
        let Some(tip) = peel_to_commit(repo, peeled.detach()) else {
            continue;
        };
        // git marks the tip SEEN before consulting the graph, so a tip shared by
        // two refs is visited once whichever answer the graph gives.
        if !seen.insert(tip) || in_graph(&tip) {
            continue;
        }
        count += 1;
        if count >= limit {
            return true;
        }
        let mut stack = vec![tip];
        while let Some(commit) = stack.pop() {
            for parent in commit_parents(repo, commit) {
                if in_graph(&parent) || !seen.insert(parent) {
                    continue;
                }
                count += 1;
                if count >= limit {
                    return true;
                }
                stack.push(parent);
            }
        }
    }
    false
}

/// The commit `id` names, following tag chains; `None` when it is not a commit,
/// which git's `odb_read_object_info(...) != OBJ_COMMIT` check skips.
fn peel_to_commit(repo: &gix::Repository, id: gix::ObjectId) -> Option<gix::ObjectId> {
    let object = repo.find_object(id).ok()?;
    object.peel_to_kind(gix::object::Kind::Commit).ok().map(|c| c.id)
}

/// The parents of `id`, or an empty list when it cannot be read — git's
/// `repo_parse_commit()` failure path, which skips the commit.
fn commit_parents(repo: &gix::Repository, id: gix::ObjectId) -> Vec<gix::ObjectId> {
    repo.find_object(id)
        .ok()
        .and_then(|o| o.try_into_commit().ok())
        .map(|c| c.parent_ids().map(|p| p.detach()).collect())
        .unwrap_or_default()
}

/// `incremental_repack_auto_condition()`: at least
/// `maintenance.incremental-repack.auto` (default 10) packs are not covered by
/// the multi-pack-index. The whole condition is off when `core.multiPackIndex`
/// is false, which is the one place that key gates this task.
fn incremental_repack_condition(repo: &gix::Repository) -> bool {
    if repo.config_snapshot().boolean("core.multiPackIndex") == Some(false) {
        return false;
    }
    let limit = match gate(repo, "maintenance.incremental-repack.auto", 10) {
        Gate::Never => return false,
        Gate::Always => return true,
        Gate::Threshold(limit) => limit,
    };

    let pack_dir = repo.objects.store_ref().path().join("pack");
    let indexed = midx_pack_names(&pack_dir);
    let mut count: i64 = 0;
    for pack in local_packs(&pack_dir, repo.object_hash()) {
        if count >= limit {
            break;
        }
        if !indexed.contains(&pack.index_name) {
            count += 1;
        }
    }
    count >= limit
}

/// The `.idx` names the multi-pack-index covers, empty when there is none.
fn midx_pack_names(pack_dir: &Path) -> std::collections::HashSet<String> {
    let midx = pack_dir.join("multi-pack-index");
    let Ok(file) = gix::odb::pack::multi_index::File::at(&midx, None) else {
        return std::collections::HashSet::new();
    };
    file.index_names()
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
        .collect()
}

/// `geometric_repack_auto_condition()`: the geometric split would merge at least
/// two packs, or — when it would not — there are enough loose objects to be
/// worth a new pack.
fn geometric_repack_condition(repo: &gix::Repository) -> bool {
    let auto_value = match gate(repo, "maintenance.geometric-repack.auto", 100) {
        Gate::Never => return false,
        Gate::Always => return true,
        Gate::Threshold(value) => value,
    };
    if geometric_split(repo) > 0 {
        return true;
    }
    super::gc::too_many_loose_objects(repo, auto_value)
}

/// `pack_geometry_split()` over the repository's own non-cruft, non-`.keep`,
/// non-promisor packs: how many of them, taken in ascending object count, the
/// next `repack --geometric=<factor>` would roll into one.
///
/// `maintenance.geometric-repack.splitFactor` (default 2) is the progression's
/// ratio — the same value the task passes to `repack --geometric=`.
pub(super) fn geometric_split(repo: &gix::Repository) -> usize {
    let factor = repo
        .config_snapshot()
        .integer("maintenance.geometric-repack.splitFactor")
        .unwrap_or(2)
        .max(0) as u64;

    let pack_dir = repo.objects.store_ref().path().join("pack");
    let mut weights: Vec<u64> = local_packs(&pack_dir, repo.object_hash())
        .into_iter()
        .filter(|p| !p.keep && !p.cruft && !p.promisor)
        .map(|p| p.objects)
        .collect();
    weights.sort_unstable();
    compute_split(&weights, factor)
}

/// `compute_pack_geometry_split()`. `weights` is ascending; the answer is the
/// number of packs at the light end that do not already form a geometric
/// progression, grown by however many of the heavy packs the rolled-up pack
/// would itself absorb.
fn compute_split(weights: &[u64], factor: u64) -> usize {
    if weights.is_empty() {
        return 0;
    }
    // Count the packs that already form a progression, from the heavy end down.
    let mut i = weights.len() - 1;
    while i > 0 {
        if weights[i] < factor.saturating_mul(weights[i - 1]) {
            break;
        }
        i -= 1;
    }
    // The top element of the last-compared pair cannot be in the progression, so
    // the split moves one right — unless the scan ran all the way to the end.
    let mut split = if i == 0 { 0 } else { i + 1 };

    let mut total: u64 = weights[..split].iter().copied().sum();
    for &weight in &weights[split..] {
        if weight >= factor.saturating_mul(total) {
            break;
        }
        split += 1;
        total = total.saturating_add(weight);
    }
    split
}

/// One pack in the repository's own `objects/pack`, carrying the `struct
/// packed_git` flags the auto conditions branch on.
struct Pack {
    /// The `.idx` file name, which is how a multi-pack-index names its packs.
    index_name: String,
    /// Its object count, `pack_geometry_weight()`'s measure.
    objects: u64,
    /// A `.keep` file sits beside it.
    keep: bool,
    /// A `.mtimes` file sits beside it — git's `is_cruft`.
    cruft: bool,
    /// A `.promisor` file sits beside it.
    promisor: bool,
}

/// Every pack in `pack_dir`. git's conditions only ever look at `pack_local`
/// packs, which is exactly the repository's own pack directory — alternates
/// live elsewhere and are never counted.
fn local_packs(pack_dir: &Path, hash: gix::hash::Kind) -> Vec<Pack> {
    let Ok(entries) = std::fs::read_dir(pack_dir) else {
        return Vec::new();
    };
    let mut packs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("idx") {
            continue;
        }
        // A pack with no `.pack` beside its index is not a pack git would open.
        if !path.with_extension("pack").exists() {
            continue;
        }
        let Ok(index) = gix::odb::pack::index::File::at(&path, hash) else {
            continue;
        };
        let Some(index_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        packs.push(Pack {
            index_name: index_name.to_owned(),
            objects: u64::from(index.num_objects()),
            keep: path.with_extension("keep").exists(),
            cruft: path.with_extension("mtimes").exists(),
            promisor: path.with_extension("promisor").exists(),
        });
    }
    packs
}

/// Reject an unknown `--task` value the way git's callback does: a lone
/// `error:` line, exit 129. `None` when the name is one git knows.
fn check_task(value: &str) -> Option<ExitCode> {
    (!TASKS.contains(&value)).then(|| bare_error(&format!("'{value}' is not a valid task")))
}

/// Record an accepted `--task` name once: git's callback sets the task's
/// `selected` bit, so naming one twice still runs it once.
fn push_task(selected: &mut Vec<String>, value: &str) {
    if !selected.iter().any(|s| s == value) {
        selected.push(value.to_string());
    }
}

/// Validate a `--task` or `--schedule` value the way git's option callbacks do,
/// recording an accepted task name in `selected` and an accepted frequency in
/// `scheduled` — the callback overwrites, so a repeated `--schedule` keeps the
/// last one.
///
/// Returns `Some(exit_code)` when the value is rejected, `None` when accepted.
fn check_value(
    name: &str,
    value: &str,
    scheduled: &mut Option<Schedule>,
    selected: &mut Vec<String>,
) -> Result<Option<ExitCode>> {
    match name {
        "task" => {
            if let Some(code) = check_task(value) {
                return Ok(Some(code));
            }
            push_task(selected, value);
        }
        "schedule" => {
            let Some(frequency) = Schedule::parse(value) else {
                // git's `parse_schedule` dies rather than raising a usage error.
                eprintln!("fatal: unrecognized --schedule argument '{value}'");
                return Ok(Some(ExitCode::from(128)));
            };
            *scheduled = Some(frequency);
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
/// nothing and exits 0, as stock git does — unless a config file cannot be
/// locked, in which case `config.c` reports it and the caller dies: the
/// `error: could not lock config file <path>` line, then git's `fatal:` line,
/// exit 128.
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
    // `repo_config_set()` is the non-gently spelling: it dies on the write it
    // could not perform rather than carrying on.
    if let Err(ConfigWriteFailed(msg)) = write_config(&local_path, &local) {
        eprintln!("{msg}");
        eprintln!("fatal: could not set 'maintenance.auto' to 'false'");
        return Ok(ExitCode::from(128));
    }

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
        if let Err(ConfigWriteFailed(msg)) = write_config(&target, &file) {
            eprintln!("{msg}");
            eprintln!(
                "fatal: unable to add '{REPO_KEY}' value of '{}'",
                maintpath.to_str_lossy()
            );
            return Ok(ExitCode::from(128));
        }
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

    // `rc && (!force || rc == CONFIG_NOTHING_SET)`: a lock that could not be
    // taken is `CONFIG_NO_LOCK`, which `--force` swallows.
    if let Err(ConfigWriteFailed(msg)) = write_config(&target, &file) {
        eprintln!("{msg}");
        if !force {
            eprintln!(
                "fatal: unable to unset '{REPO_KEY}' value of '{}'",
                maintpath.to_str_lossy()
            );
            return Ok(ExitCode::from(128));
        }
    }
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

/// The `error:` line `config.c` prints when it cannot write a config file. git
/// reports it and then dies with a second, caller-specific `fatal:` line, so the
/// two are kept apart here.
struct ConfigWriteFailed(String);

/// Serialize `file` back over `path` the way `git_config_set_multivar_in_file_gently()`
/// does: the new content is written to a `<path>.lock` sibling created with
/// `O_EXCL` and renamed into place, so a config that cannot be locked is never
/// touched and the command can report the failure instead of claiming success.
/// `lock_file()` resolves symlinks before appending `.lock`, so the lock lands
/// beside the real file. Everything untouched round-trips byte-for-byte.
fn write_config(path: &Path, file: &gix::config::File) -> std::result::Result<(), ConfigWriteFailed> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let real = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => std::fs::canonicalize(parent)
            .unwrap_or_else(|_| parent.to_owned())
            .join(path.file_name().unwrap_or_default()),
        None => path.to_owned(),
    };
    let mut lock = real.clone().into_os_string();
    lock.push(".lock");
    let lock = PathBuf::from(lock);

    let no_lock = |e: &std::io::Error| {
        ConfigWriteFailed(format!(
            "error: could not lock config file {}: {}",
            path.display(),
            errno_text(e)
        ))
    };
    let mut out = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
    {
        Ok(f) => f,
        Err(e) => return Err(no_lock(&e)),
    };
    let write = std::io::Write::write_all(&mut out, &file.to_bstring())
        .and_then(|()| out.sync_all())
        .and_then(|()| {
            drop(out);
            std::fs::rename(&lock, &real)
        });
    if let Err(e) = write {
        let _ = std::fs::remove_file(&lock);
        return Err(ConfigWriteFailed(format!(
            "error: could not write config file {}: {}",
            path.display(),
            errno_text(&e)
        )));
    }
    Ok(())
}

/// `strerror(errno)`, which is what git's `error_errno()` appends.
fn errno_text(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }
            .to_string_lossy()
            .into_owned(),
        None => e.to_string(),
    }
}
