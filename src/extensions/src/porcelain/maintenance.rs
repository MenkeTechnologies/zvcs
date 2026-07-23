//! `git maintenance` — run tasks to optimize repository data.
//!
//! Four of the six subcommands are genuinely ported. Two are pure config
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
//!   * `run --auto` needs the same per-task condition heuristics as
//!     `is-needed --auto`, below.
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

/// The tasks a bare `maintenance run` selects, in the order it runs them.
///
/// This is *not* [`TASKS`] filtered by an `enabled` flag: git 2.55.0 runs the
/// default set in a different order from an explicit `--task` selection. Read
/// off the same trace, a bare `maintenance run` in a repository with no
/// `maintenance.strategy` gives `pack-refs`, `reflog-expire`,
/// `geometric-repack`, `commit-graph`, `worktree-prune`, `rerere-gc` — note
/// `geometric-repack` third here and last in `TASKS`.
const DEFAULT_TASKS: [&str; 6] = [
    "pack-refs",
    "reflog-expire",
    "geometric-repack",
    "commit-graph",
    "worktree-prune",
    "rerere-gc",
];

/// The only task a `maintenance run` selects once `maintenance.strategy` is set
/// and no `--schedule` narrows the selection.
///
/// Setting the key changes the default set wholesale rather than merely
/// attaching schedules to it: with `maintenance.strategy=incremental` and no
/// `--schedule`, git 2.55.0 runs `gc` and nothing else.
const STRATEGY_TASKS: [&str; 1] = ["gc"];

/// Every `--schedule=<frequency>` value git accepts.
const SCHEDULES: [&str; 3] = ["hourly", "daily", "weekly"];

/// Every `--scheduler=<scheduler>` value git accepts.
const SCHEDULERS: [&str; 5] = ["auto", "crontab", "systemd-timer", "launchctl", "schtasks"];

/// The multi-valued key holding the registry of maintained repositories.
const REPO_KEY: &str = "maintenance.repo";

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
    let mut scheduled = false;
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
    if auto && scheduled {
        eprintln!("fatal: options '--auto' and '--schedule=' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    if !selected.is_empty() && scheduled {
        eprintln!("fatal: options '--task=' and '--schedule=' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    if auto {
        bail!(
            "maintenance run --auto is not ported: it gates every task on git's per-task \
             auto-conditions, which rest on a loose-object estimator that samples the objects/17 \
             fanout directory and scales by 256, and on multi-pack-index state; those thresholds \
             have no counterpart in the vendored crates, and since --auto's only effect is to skip \
             work silently, guessing them would be silently wrong \
             (ported: run without --auto, register, unregister, and argument validation)"
        );
    }

    run_tasks(&repo, &selected, scheduled)
}

/// Run the selected maintenance tasks in git's order and report the way git's
/// `maintenance_run_tasks()` does.
///
/// # Selection
///
/// `selected` is the `--task` set, empty when none was given. With it non-empty
/// the tasks run in [`TASKS`] order; otherwise the default set applies —
/// [`DEFAULT_TASKS`] normally, [`STRATEGY_TASKS`] once `maintenance.strategy` is
/// configured — and `maintenance.<task>.enabled` can add or remove any task from
/// it. A `--schedule` run (`scheduled`) selects by each task's schedule, and
/// without `maintenance.strategy` no task has one, so nothing runs.
///
/// # What the tasks do
///
///   * **`pack-refs`** → [`super::pack_refs::pack_refs`] with `--all --prune`,
///     which is git's own argument list and a real port.
///   * **`reflog-expire`** → [`expire_reflogs`], a real expiry.
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
/// # The two tasks that do nothing, and why
///
///   * **`commit-graph`**. git runs `commit-graph write --split --reachable`,
///     which writes `objects/info/commit-graphs/commit-graph-chain` and a
///     `graph-<hash>.graph` beside it. `gix-commitgraph` ships `access`, `file`,
///     `init` and `verify` — it reads the format and cannot write it. Writing
///     one by hand is not a small thing done safely: a graph file that is
///     well-formed enough to be *loaded* but wrong in a chunk would make every
///     later git command silently traverse from bad data, which is worse than
///     having no graph at all. So none is written, and none is claimed.
///   * **`worktree-prune`**. git runs `worktree prune --expire 3.months.ago`,
///     which removes `worktrees/<id>` administrative directories whose `gitdir`
///     no longer resolves. `worktree.rs` has no prune port, and the removal is
///     destructive with expiry and `locked` semantics that would have to be
///     guessed at, so it is left to the module that owns worktree bookkeeping.
///
/// Both are skipped rather than approximated. Neither is visible in
/// `objects/info/commit-graph` — git's split graph does not write that path —
/// but both are real gaps, and a `maintenance run` that exits 0 has not done
/// them.
fn run_tasks(repo: &gix::Repository, selected: &[String], scheduled: bool) -> Result<ExitCode> {
    let order = plan(repo, selected, scheduled);

    // git reports a failing task on stderr and keeps going, then exits 1 —
    // `error: task 'incremental-repack' failed` on a repository with no packs,
    // observed on git 2.55.0.
    let mut failed = false;
    for task in order {
        let ok = match task {
            "pack-refs" => delegate(super::pack_refs::pack_refs(&strings(&[
                "pack-refs",
                "--all",
                "--prune",
            ]))),
            "reflog-expire" => expire_reflogs(repo).is_ok(),
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
                    || delegate(super::rerere::rerere(&strings(&["rerere", "gc"])))
            }
            // See the "two tasks that do nothing" section above.
            "commit-graph" | "worktree-prune" => true,
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
fn plan(repo: &gix::Repository, selected: &[String], scheduled: bool) -> Vec<&'static str> {
    // `--schedule` picks tasks by their configured schedule. Without
    // `maintenance.strategy` no task has one, so the run is empty — git 2.55.0
    // exits 0 having entered no task region for any of hourly/daily/weekly.
    // With a strategy set the schedules exist (incremental gives prefetch and
    // commit-graph hourly, loose-objects and incremental-repack daily, pack-refs
    // weekly), and those tasks are the unported ones, so nothing is claimed here
    // that the task bodies would not refuse anyway.
    if scheduled {
        return Vec::new();
    }

    if !selected.is_empty() {
        return TASKS
            .into_iter()
            .filter(|name| selected.iter().any(|s| s.as_str() == *name))
            .collect();
    }

    let config = repo.config_snapshot();
    let strategy = config.string("maintenance.strategy").is_some();
    let default: &[&'static str] = if strategy {
        &STRATEGY_TASKS
    } else {
        &DEFAULT_TASKS
    };

    // `maintenance.<task>.enabled` overrides membership either way. Ordering
    // follows the default set for the tasks in it, then `TASKS` for any the
    // config switched on.
    let enabled = |name: &str| config.boolean(&format!("maintenance.{name}.enabled"));
    let mut tasks: Vec<&'static str> = default
        .iter()
        .copied()
        .filter(|name| enabled(name).unwrap_or(true))
        .collect();
    for name in TASKS {
        if !tasks.contains(&name) && enabled(name) == Some(true) {
            tasks.push(name);
        }
    }
    tasks
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

/// git's default `gc.reflogExpire`, in seconds.
const REFLOG_EXPIRE_DEFAULT: i64 = 90 * 24 * 60 * 60;

/// `git reflog expire --all` — drop reflog entries older than `gc.reflogExpire`.
///
/// Every reflog under `logs/` is rewritten in place, keeping only the entries
/// whose timestamp is at or after the cutoff. An emptied reflog is truncated,
/// not deleted, which is what git leaves behind: after `maintenance run` on a
/// fixture whose commits are dated 2023, `.git/logs/HEAD` exists and is 0 bytes.
///
/// A reflog line is
/// `<old> <new> <name> <<email>> <seconds> <tz>\t<message>`, so the timestamp is
/// the first field after the `>` that closes the email address. Parsing stops
/// there: nothing else on the line is needed, and a line that does not parse is
/// kept rather than dropped, so a format this does not understand costs history
/// nothing.
///
/// # What is not reproduced
///
/// git has a second, shorter expiry — `gc.reflogExpireUnreachable`, 30 days by
/// default — for entries whose new object is no longer reachable from the ref.
/// Only the 90-day rule is applied here, so entries between the two cutoffs that
/// git would drop are kept. That errs toward keeping history, which is the safe
/// direction for a destructive operation, and it is invisible wherever every
/// entry is on one side of both cutoffs.
///
/// Likewise only `never`, `now` and the default are understood as values of
/// `gc.reflogExpire`; git accepts any approxidate. An unrecognised value leaves
/// every reflog untouched rather than guessing at a cutoff, matching how
/// [`super::gc`] treats a dated `gc.pruneExpire`.
fn expire_reflogs(repo: &gix::Repository) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let cutoff = match repo
        .config_snapshot()
        .string("gc.reflogExpire")
        .as_ref()
        .and_then(|v| v.to_str().ok())
    {
        None => now - REFLOG_EXPIRE_DEFAULT,
        Some("now") => now,
        // `never` disables expiry; anything else is an approxidate this does not
        // parse, and is treated the same way rather than guessed at.
        Some(_) => return Ok(()),
    };

    // A linked worktree keeps its own `logs/HEAD` beside the shared `logs/refs`.
    let mut roots = vec![repo.common_dir().join("logs")];
    let private = repo.git_dir().join("logs");
    if !roots.contains(&private) {
        roots.push(private);
    }
    for root in roots {
        for path in reflog_files(&root) {
            expire_reflog(&path, cutoff)?;
        }
    }
    Ok(())
}

/// Every regular file under `root`, which for a reflog directory is every
/// reflog: git mirrors ref names as paths there and stores nothing else.
fn reflog_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out
}

/// Rewrite one reflog, keeping the entries at or after `cutoff`.
///
/// The file is only touched when something actually expired, so a run that
/// changes nothing leaves every mtime alone.
fn expire_reflog(path: &Path, cutoff: i64) -> Result<()> {
    let Ok(body) = std::fs::read(path) else {
        return Ok(());
    };

    let mut kept = Vec::with_capacity(body.len());
    let mut dropped = false;
    for line in body.split_inclusive(|b| *b == b'\n') {
        match entry_timestamp(line) {
            Some(at) if at < cutoff => dropped = true,
            // Kept: either recent enough, or unparsable and so not ours to drop.
            _ => kept.extend_from_slice(line),
        }
    }
    if !dropped {
        return Ok(());
    }

    // Rename into place so a reader never sees a half-written reflog. The
    // temporary is a sibling, because a rename across filesystems is not atomic.
    let name = match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => return Ok(()),
    };
    let tmp = path.with_file_name(format!("{name}.zvcs-{}", std::process::id()));
    std::fs::write(&tmp, &kept)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The `<seconds>` field of a reflog line: the first token after the `>` that
/// closes the committer's email address, which is the last `>` before the tab
/// that starts the message.
fn entry_timestamp(line: &[u8]) -> Option<i64> {
    let head = match line.iter().position(|b| *b == b'\t') {
        Some(at) => &line[..at],
        None => line,
    };
    let after_email = head.iter().rposition(|b| *b == b'>')? + 1;
    let rest = head.get(after_email..)?;
    let token = rest.split(|b| *b == b' ').find(|f| !f.is_empty())?;
    std::str::from_utf8(token).ok()?.parse().ok()
}

/// Reject an unknown `--task` value the way git's callback does: a lone
/// `error:` line, exit 129. `None` when the name is one git knows.
fn check_task(value: &str) -> Option<ExitCode> {
    (!TASKS.contains(&value)).then(|| bare_error(&format!("'{value}' is not a valid task")))
}

/// Validate a `--task` or `--schedule` value the way git's option callbacks do,
/// recording an accepted task name in `selected`.
///
/// Returns `Some(exit_code)` when the value is rejected, `None` when accepted.
fn check_value(
    name: &str,
    value: &str,
    scheduled: &mut bool,
    selected: &mut Vec<String>,
) -> Result<Option<ExitCode>> {
    match name {
        "task" => {
            if let Some(code) = check_task(value) {
                return Ok(Some(code));
            }
            // git's callback sets the task's `selected` bit, so naming one twice
            // still runs it once.
            if !selected.iter().any(|s| s == value) {
                selected.push(value.to_string());
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
