//! `git switch` — move `HEAD` to a branch, backed by the vendored gitoxide
//! ref store and worktree-state checkout.
//!
//! Supported invocations, reproduced from git 2.55.0 rather than inferred:
//!   * `git switch <branch>`                    → attach `HEAD` to an existing
//!                                                local branch and update the
//!                                                worktree/index to its tip.
//!   * `git switch <remote-only-name>`          → DWIM (`--guess`, the default):
//!                                                when exactly one remote has the
//!                                                branch, create a local tracking
//!                                                branch from it and switch.
//!   * `git switch -c|--create <new> [<start>]` → create `refs/heads/<new>` at
//!                                                `<start>` (default `HEAD`),
//!                                                attach `HEAD`, set up tracking
//!                                                when the start-point warrants.
//!   * `git switch -C|--force-create <n> [<s>]` → create-or-reset `<n>` at `<s>`.
//!   * `git switch -d|--detach [<commit>]`      → detach `HEAD` at a commit
//!                                                (default `HEAD`).
//!   * `git switch --orphan <new>`              → unborn branch, cleared worktree.
//!   * `git switch -|@{-N}`                      → the previous-branch shorthand,
//!                                                resolved from the `HEAD` reflog.
//!   * `-t`/`--track[=(direct|inherit)]` / `--no-track`, `--guess`/`--no-guess`,
//!     `-f`/`--force`/`--discard-changes`, `-q`/`--quiet`.
//!
//! Stream and exit-code conventions: the informational messages (`Switched to
//! branch '<b>'`, `Switched to a new branch '<b>'`, `Switched to and reset branch
//! '<b>'`, `Reset branch '<b>'`, `Already on '<b>'`, `HEAD is now at …`,
//! `Previous HEAD position was …`) all go to **stderr**. The tracking notice
//! (`branch '<b>' set up to track '<u>'.`) goes to **stdout**. Failures print
//! `fatal: <reason>` on stderr and exit 128; option-parsing failures print
//! `error: <reason>` (with the usage block for unknown options) and exit 129.
//!
//! Deferred (honest terse bail): `-m`/`--merge` (a real 3-way worktree merge that
//! carries local modifications onto the new branch — needs the unpack-trees
//! merge machinery gitoxide does not expose as a worktree operation) and
//! `--conflict` (only meaningful with `--merge`).
//!
//! Known divergences from git that are *not* fixable from this file:
//!   * No `.git/logs/HEAD` reflog line is written for the symbolic `HEAD` move —
//!     see [`attach_head`].
//!   * A switch that must rewrite tracked files requires a clean worktree unless
//!     `--force` is given; git instead carries non-conflicting local
//!     modifications across (reporting them as `<status>\t<path>` on stdout).
//!     Refusing the dirty case is narrower than git but never silently loses
//!     work; `--force` provides the discard-and-switch escape hatch.
//!   * The "you are leaving N commit behind" orphaned-commit warning printed when
//!     abandoning a detached HEAD with unreachable commits is not reproduced
//!     (consistent with `checkout.rs`).

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// Stock git's `git switch` usage block, byte-for-byte (git 2.55.0). Printed to
/// stdout on `-h`, and to stderr after the `error:` line on an unknown option.
const USAGE: &str = "\
usage: git switch [<options>] [<branch>]

    -c, --[no-]create <branch>
                          create and switch to a new branch
    -C, --[no-]force-create <branch>
                          create/reset and switch to a branch
    --[no-]guess          second guess 'git switch <no-such-branch>'
    --[no-]discard-changes
                          throw away local modifications
    -q, --[no-]quiet      suppress progress reporting
    --[no-]recurse-submodules[=<checkout>]
                          control recursive updating of submodules
    --[no-]progress       force progress reporting
    -m, --[no-]merge      perform a 3-way merge with the new branch
    --[no-]conflict <style>
                          conflict style (merge, diff3, or zdiff3)
    -d, --[no-]detach     detach HEAD at named commit
    -t, --[no-]track[=(direct|inherit)]
                          set branch tracking configuration
    -f, --[no-]force      force checkout (throw away local modifications)
    --[no-]orphan <new-branch>
                          new unborn branch
    --[no-]overwrite-ignore
                          update ignored files (default)
    --[no-]ignore-other-worktrees
                          do not check if another worktree is using this branch
";

/// git's fatal error convention: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git's option-parsing error convention: `error: <msg>` on stderr, exit 129,
/// with no usage block (matching parse-options' `optbug`/`requires a value`).
fn usage_error(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(129))
}

/// git's unknown-option convention: `error: <msg>` then the usage block on
/// stderr, exit 129.
fn unknown_option(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Parsed command line.
struct Parsed<'a> {
    create: Option<&'a str>,
    force_create: Option<&'a str>,
    orphan: Option<&'a str>,
    detach: bool,
    force: bool,
    quiet: bool,
    /// `None` default, `Some(true)` for `--track`, `Some(false)` for `--no-track`.
    track: Option<bool>,
    /// `None` = unset on the CLI (fall back to `checkout.guess`, default on);
    /// `Some(true)` for `--guess`, `Some(false)` for `--no-guess`.
    guess: Option<bool>,
    positionals: Vec<&'a str>,
}

/// Either a fully parsed command line, or the exit code of an option-parsing
/// failure that has already been reported on stderr.
enum Parse<'a> {
    Ok(Parsed<'a>),
    Failed(ExitCode),
}

/// Parse `switch`'s command line the way git's parse-options does.
fn parse<'a>(args: &'a [String]) -> Result<Parse<'a>> {
    let mut p = Parsed {
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        force: false,
        quiet: false,
        track: None,
        guess: None,
        positionals: Vec::new(),
    };
    let mut only_positional = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        if only_positional {
            p.positionals.push(a);
            i += 1;
            continue;
        }
        if a == "--" {
            only_positional = true;
            i += 1;
            continue;
        }
        // `-` is the previous-branch shorthand (git treats it as `@{-1}`); pass
        // it through as a positional to be resolved against the HEAD reflog.
        // `@{-N}` does not start with `-`, so it is already a positional.
        if a == "-" {
            p.positionals.push(a);
            i += 1;
            continue;
        }

        // Long options, with or without an attached `=value`.
        if let Some(long) = a.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            macro_rules! take_value {
                ($optname:literal) => {
                    match attached {
                        Some(v) => v,
                        None => {
                            i += 1;
                            match args.get(i) {
                                Some(v) => v.as_str(),
                                None => {
                                    return usage_error(concat!(
                                        "option `",
                                        $optname,
                                        "' requires a value"
                                    ))
                                    .map(Parse::Failed)
                                }
                            }
                        }
                    }
                };
            }
            match name {
                "create" => p.create = Some(take_value!("create")),
                "force-create" => p.force_create = Some(take_value!("force-create")),
                "orphan" => p.orphan = Some(take_value!("orphan")),
                "quiet" => p.quiet = true,
                "no-quiet" => p.quiet = false,
                "detach" => p.detach = true,
                "no-detach" => p.detach = false,
                "force" | "discard-changes" => p.force = true,
                "no-force" | "no-discard-changes" => p.force = false,
                "guess" => p.guess = Some(true),
                "no-guess" => p.guess = Some(false),
                "track" => {
                    if let Some(v) = attached {
                        if v != "direct" && v != "inherit" {
                            return usage_error(
                                "option `--track' expects \"direct\" or \"inherit\"",
                            )
                            .map(Parse::Failed);
                        }
                    }
                    p.track = Some(true);
                }
                "no-track" => p.track = Some(false),
                // A real 3-way worktree merge — not reproducible here.
                "merge" | "conflict" => {
                    bail!("three-way merge on switch (--{name}) is not supported")
                }
                // Silently-accepted no-ops that do not change deterministic output.
                "progress"
                | "no-progress"
                | "overwrite-ignore"
                | "no-overwrite-ignore"
                | "ignore-other-worktrees"
                | "no-ignore-other-worktrees"
                | "recurse-submodules"
                | "no-recurse-submodules" => {}
                _ => {
                    return Ok(Parse::Failed(unknown_option(format!(
                        "unknown option `{name}'"
                    ))))
                }
            }
            i += 1;
            continue;
        }

        // Short option cluster: booleans, plus `c`/`C` which take a value.
        if let Some(shorts) = a.strip_prefix('-') {
            let mut off = 0;
            while off < shorts.len() {
                let ch = shorts[off..].chars().next().expect("in-bounds");
                let next_off = off + ch.len_utf8();
                match ch {
                    'q' => {}
                    'd' => p.detach = true,
                    'f' => p.force = true,
                    't' => p.track = Some(true),
                    'c' | 'C' => {
                        let rest = &shorts[next_off..];
                        let value = if rest.is_empty() {
                            i += 1;
                            match args.get(i) {
                                Some(v) => v.as_str(),
                                None => {
                                    return usage_error(format!("switch `{ch}' requires a value"))
                                        .map(Parse::Failed)
                                }
                            }
                        } else {
                            rest
                        };
                        if ch == 'c' {
                            p.create = Some(value);
                        } else {
                            p.force_create = Some(value);
                        }
                        off = shorts.len();
                        continue;
                    }
                    'm' => bail!("three-way merge on switch (-m) is not supported"),
                    _ => {
                        return Ok(Parse::Failed(unknown_option(format!(
                            "unknown switch `{ch}'"
                        ))))
                    }
                }
                off = next_off;
            }
            i += 1;
            continue;
        }

        p.positionals.push(a);
        i += 1;
    }

    Ok(Parse::Ok(p))
}

pub fn switch(args: &[String]) -> Result<ExitCode> {
    // `-h` as any argument prints usage on stdout and exits 129.
    if args.iter().any(|a| a == "-h") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let p = match parse(args)? {
        Parse::Ok(p) => p,
        Parse::Failed(code) => return Ok(code),
    };

    // git's mutual-exclusion checks, in its order.
    let create_modes =
        p.create.is_some() as u8 + p.force_create.is_some() as u8 + p.orphan.is_some() as u8;
    if create_modes > 1 {
        return fatal("options '-c', '-C', and '--orphan' cannot be used together");
    }
    if p.detach && create_modes >= 1 {
        return fatal("'--detach' cannot be used with '-b/-B/--orphan'");
    }

    let repo = gix::discover(".")?;

    // DWIM default: `--[no-]guess` on the CLI wins, else `checkout.guess`
    // (git's default is on).
    let guess = p
        .guess
        .unwrap_or_else(|| repo.config_snapshot().boolean("checkout.guess") != Some(false));

    if let Some(name) = p.orphan {
        return switch_orphan(&repo, name, &p.positionals, p.force, p.quiet);
    }
    if p.detach {
        return switch_detach(&repo, &p.positionals, p.force, p.quiet);
    }
    if let Some(name) = p.force_create {
        return switch_create(&repo, name, true, &p.positionals, p.quiet, p.force, p.track);
    }
    if let Some(name) = p.create {
        return switch_create(
            &repo,
            name,
            false,
            &p.positionals,
            p.quiet,
            p.force,
            p.track,
        );
    }
    if p.positionals.is_empty() {
        return fatal("missing branch or commit argument");
    }
    switch_existing(&repo, &p.positionals, p.quiet, guess, p.force)
}

/// `git switch <branch>` — attach `HEAD` to an existing local branch, with DWIM
/// (`--guess`) fallback to a remote-tracking branch.
fn switch_existing(
    repo: &gix::Repository,
    positionals: &[&str],
    quiet: bool,
    guess: bool,
    force: bool,
) -> Result<ExitCode> {
    if positionals.len() > 1 {
        return fatal("only one reference expected");
    }

    // Resolve the `-`/`@{-N}` previous-branch shorthand to a concrete name.
    let raw = positionals[0];
    let resolved;
    let branch: &str = if raw == "-" || raw.starts_with("@{-") {
        let expanded = if raw == "-" { "@{-1}" } else { raw };
        match resolve_prev_branch(repo, expanded) {
            Some(name) => {
                resolved = name;
                &resolved
            }
            None => return fatal(format!("invalid reference: {expanded}")),
        }
    } else {
        raw
    };

    let full = format!("refs/heads/{branch}");

    // Already on the requested branch → git reports it and exits 0 untouched.
    if repo.head_name()?.as_ref().map(|n| n.as_bstr())
        == FullName::try_from(full.as_str())
            .ok()
            .as_ref()
            .map(|n| n.as_bstr())
    {
        if !quiet {
            eprintln!("Already on '{branch}'");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let full_name = match FullName::try_from(full.as_str()) {
        Ok(n) => n,
        Err(_) => return fatal(format!("invalid reference: {branch}")),
    };

    // Not a local branch: DWIM to a remote-tracking branch, else classify.
    if repo.try_find_reference(full.as_str())?.is_none() {
        if guess {
            match unique_remote_branch(repo, branch)? {
                Dwim::One(remote_short) => {
                    let sp = [remote_short.as_str()];
                    return switch_create(repo, branch, false, &sp, quiet, force, None);
                }
                Dwim::Many { count, hint_remote } => {
                    if crate::advice::enabled("checkoutAmbiguousRemoteBranchName") {
                        print_ambiguous_remote_hint(&hint_remote);
                    }
                    return fatal(format!(
                        "'{branch}' matched multiple ({count}) remote tracking branches"
                    ));
                }
                Dwim::None => {}
            }
        }
        return branch_expected(repo, branch);
    }

    let target = repo
        .try_find_reference(full.as_str())?
        .expect("just checked present")
        .into_fully_peeled_id()?
        .detach();

    let mut head = repo.head()?;
    let current_commit = head.try_peel_to_id()?.map(|id| id.detach());
    let from_desc = describe_head(repo)?;

    let needs_worktree = current_commit != Some(target);

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if needs_worktree {
        let old = prepare_worktree_change(repo, force)?;
        update_worktree(repo, &old, target, force)?;
    }

    attach_head(repo, &full_name, &from_desc, branch)?;
    if !quiet {
        eprintln!("Switched to branch '{branch}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git switch -c|-C <new> [<start>]` — create (or, for `-C`, create-or-reset) a
/// local branch, attach `HEAD`, and optionally set up upstream tracking.
fn switch_create(
    repo: &gix::Repository,
    branch: &str,
    reset: bool,
    positionals: &[&str],
    quiet: bool,
    force: bool,
    track: Option<bool>,
) -> Result<ExitCode> {
    if positionals.len() > 1 {
        return fatal("only one reference expected");
    }
    let start = positionals.first().copied();
    let full = format!("refs/heads/{branch}");

    if let Some(code) = reject_invalid_branch_name(repo, branch, &full) {
        return Ok(code);
    }
    let full_name: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{branch}': {e}"))?;

    let mut head = repo.head()?;
    let old_detached = head.is_detached();
    let current_commit = head.try_peel_to_id()?.map(|id| id.detach());
    let already_on = head
        .referent_name()
        .map(|n| n.shorten().to_string() == branch)
        .unwrap_or(false);

    let start_commit: Option<ObjectId> = match start {
        Some(s) => match repo.rev_parse_single(BStr::new(s)) {
            Ok(id) => Some(id.detach()),
            Err(_) => return fatal(format!("invalid reference: {s}")),
        },
        None => current_commit,
    };
    let from_desc = describe_head(repo)?;

    // Determine the upstream ref this branch should track, if any.
    let upstream = tracking_upstream(repo, start, track);

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let existed = repo.try_find_reference(full.as_str())?.is_some();
    if existed && !reset {
        return fatal(format!("a branch named '{branch}' already exists"));
    }

    // Unborn HEAD with no start-point: point HEAD at the new branch; the ref
    // materialises with the first commit.
    let Some(start_commit) = start_commit else {
        attach_head(repo, &full_name, &from_desc, branch)?;
        if let Some(up) = &upstream {
            install_tracking(repo, branch, up)?;
        }
        if !quiet {
            eprintln!("Switched to a new branch '{branch}'");
        }
        return Ok(ExitCode::SUCCESS);
    };

    let needs_worktree = current_commit != Some(start_commit);
    let old = if needs_worktree {
        Some(prepare_worktree_change(repo, force)?)
    } else {
        None
    };

    // Create fresh, or force-move an existing branch for -C.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("branch: Created from {from_desc}").into(),
            },
            expected: if existed {
                PreviousValue::Any
            } else {
                PreviousValue::MustNotExist
            },
            new: Target::Object(start_commit),
        },
        name: full_name.clone(),
        deref: false,
    })?;

    if let Some(up) = &upstream {
        install_tracking(repo, branch, up)?;
    }

    if let Some(old) = old {
        update_worktree(repo, &old, start_commit, force)?;
    }

    attach_head(repo, &full_name, &from_desc, branch)?;

    if !quiet {
        if existed && already_on {
            eprintln!("Reset branch '{branch}'");
        } else {
            if old_detached {
                if let Some(id) = current_commit.filter(|id| *id != start_commit) {
                    let (abbrev, summary) = describe(repo, id)?;
                    eprintln!("Previous HEAD position was {abbrev} {summary}");
                }
            }
            if existed {
                eprintln!("Switched to and reset branch '{branch}'");
            } else {
                eprintln!("Switched to a new branch '{branch}'");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git switch -d|--detach [<commit>]` — detach `HEAD` at a commit.
fn switch_detach(
    repo: &gix::Repository,
    positionals: &[&str],
    force: bool,
    quiet: bool,
) -> Result<ExitCode> {
    if positionals.len() > 1 {
        return fatal("only one reference expected");
    }

    let target_id: ObjectId = match positionals.first().copied() {
        Some(s) => match repo.rev_parse_single(BStr::new(s)) {
            Ok(id) => match repo.find_object(id.detach())?.peel_to_commit() {
                Ok(c) => c.id,
                Err(_) => return fatal(format!("invalid reference: {s}")),
            },
            Err(_) => return fatal(format!("invalid reference: {s}")),
        },
        None => {
            let mut head = repo.head()?;
            match head.try_peel_to_id()? {
                Some(id) => id.detach(),
                None => return fatal("you are on a branch yet to be born"),
            }
        }
    };
    let target_tree = repo
        .find_object(target_id)?
        .peel_to_commit()?
        .tree_id()?
        .detach();

    let mut head = repo.head()?;
    let old_detached = head.is_detached();
    let old_id = head.try_peel_to_id()?.map(|id| id.detach());
    let from_desc = describe_head(repo)?;
    let cur_tree = head_tree(repo)?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if cur_tree != Some(target_tree) {
        let old = prepare_worktree_change(repo, force)?;
        update_worktree(repo, &old, target_id, force)?;
    }

    let to_desc = target_id.attach(repo).shorten_or_id().to_string();
    set_head_detached(
        repo,
        target_id,
        &format!("checkout: moving from {from_desc} to {to_desc}"),
    )?;

    if !quiet {
        // git reports the abandoned detached position only when moving from one
        // detached commit to a different one.
        if old_detached {
            if let Some(id) = old_id.filter(|id| *id != target_id) {
                let (abbrev, summary) = describe(repo, id)?;
                eprintln!("Previous HEAD position was {abbrev} {summary}");
            }
        }
        let (abbrev, summary) = describe(repo, target_id)?;
        eprintln!("HEAD is now at {abbrev} {summary}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git switch --orphan <new>` — point `HEAD` at an unborn branch and clear the
/// tracked worktree and index.
fn switch_orphan(
    repo: &gix::Repository,
    branch: &str,
    positionals: &[&str],
    force: bool,
    quiet: bool,
) -> Result<ExitCode> {
    // `--orphan` takes no start-point; a resolvable extra arg is a start-point
    // error, an unresolvable one is a bad reference (git's evaluation order).
    if let Some(p) = positionals.first().copied() {
        return if repo.rev_parse_single(BStr::new(p)).is_ok() {
            fatal("'--orphan' cannot take <start-point>")
        } else {
            fatal(format!("invalid reference: {p}"))
        };
    }

    let full = format!("refs/heads/{branch}");
    if let Some(code) = reject_invalid_branch_name(repo, branch, &full) {
        return Ok(code);
    }
    let full_name: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{branch}': {e}"))?;

    let from_desc = describe_head(repo)?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() {
        return fatal(format!("a branch named '{branch}' already exists"));
    }

    let old = repo.index_or_load_from_head()?.into_owned();
    if !old.entries().is_empty() {
        if !force && repo.is_dirty()? {
            bail!("your local changes would be overwritten; commit or stash them, or use --force (dirty-worktree orphan is unsupported)");
        }
        clear_tracked_worktree(repo, &old)?;
    }

    attach_head(repo, &full_name, &from_desc, branch)?;
    if !quiet {
        eprintln!("Switched to a new branch '{branch}'");
    }
    Ok(ExitCode::SUCCESS)
}

// --- DWIM / tracking -------------------------------------------------------

/// Result of resolving a bare name against the remote-tracking namespace.
enum Dwim {
    /// Exactly one remote has the branch; its short name (`<remote>/<branch>`).
    One(String),
    /// More than one remote has it — ambiguous.
    Many { count: usize, hint_remote: String },
    /// No remote has it.
    None,
}

/// Find the remote-tracking branch a bare `<name>` should DWIM to: `refs/remotes/
/// <remote>/<name>` across every configured remote.
fn unique_remote_branch(repo: &gix::Repository, name: &str) -> Result<Dwim> {
    let mut matches: Vec<String> = Vec::new();
    for remote in repo.remote_names() {
        let remote = remote.to_str_lossy();
        let full = format!("refs/remotes/{remote}/{name}");
        if repo.try_find_reference(full.as_str())?.is_some() {
            matches.push(remote.into_owned());
        }
    }
    matches.sort();
    match matches.len() {
        0 => Ok(Dwim::None),
        1 => Ok(Dwim::One(format!("{}/{name}", matches[0]))),
        n => Ok(Dwim::Many {
            count: n,
            hint_remote: matches[0].clone(),
        }),
    }
}

/// The `advise()` block git prints for an ambiguous DWIM name, verbatim.
fn print_ambiguous_remote_hint(remote: &str) {
    eprintln!("hint: If you meant to check out a remote tracking branch on, e.g. '{remote}',");
    eprintln!("hint: you can do so by fully qualifying the name with the --track option:");
    eprintln!("hint:");
    eprintln!("hint:     git switch --track {remote}/<name>");
    eprintln!("hint:");
    eprintln!("hint: If you'd like to always have checkouts of an ambiguous <name> prefer");
    eprintln!("hint: one remote, e.g. the '{remote}' remote, consider setting");
    eprintln!("hint: checkout.defaultRemote={remote} in your config.");
    eprintln!(
        "hint: Disable this message with \"git config set advice.checkoutAmbiguousRemoteBranchName false\""
    );
}

/// The upstream a newly created branch should track: `(remote, merge_ref,
/// short)`. Auto-set when the start-point is a remote-tracking branch (git's
/// default `branch.autoSetupMerge=true`); a local branch is tracked only with an
/// explicit `--track`. `--no-track` disables it entirely.
fn tracking_upstream(
    repo: &gix::Repository,
    start: Option<&str>,
    track: Option<bool>,
) -> Option<(String, String, String)> {
    if track == Some(false) {
        return None;
    }
    // Resolve the start-point (or current HEAD branch) to a full ref name.
    let full: BString = match start {
        Some(s) => repo.find_reference(s).ok()?.name().as_bstr().to_owned(),
        None => repo.head_name().ok()??.as_bstr().to_owned(),
    };
    let s = full.to_str_lossy();

    if let Some(rest) = s.strip_prefix("refs/remotes/") {
        // Remote-tracking: auto-track by default and with explicit --track.
        let (remote, branch) = rest.split_once('/')?;
        return Some((
            remote.to_string(),
            format!("refs/heads/{branch}"),
            format!("{remote}/{branch}"),
        ));
    }
    if let Some(branch) = s.strip_prefix("refs/heads/") {
        // Local branch: track only when explicitly requested.
        if track == Some(true) {
            return Some((
                ".".to_string(),
                format!("refs/heads/{branch}"),
                branch.to_string(),
            ));
        }
    }
    None
}

/// Write `branch.<name>.remote` / `branch.<name>.merge` into the repository-local
/// config and print git's `set up to track` notice on stdout. Called while the
/// repo lock is already held.
fn install_tracking(
    repo: &gix::Repository,
    branch: &str,
    upstream: &(String, String, String),
) -> Result<()> {
    let (remote, merge_ref, short) = upstream;
    let path = repo.common_dir().join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)?;
    let sub = BStr::new(branch.as_bytes());
    file.set_raw_value_by("branch", Some(sub), "remote", remote.as_str())?;
    file.set_raw_value_by("branch", Some(sub), "merge", merge_ref.as_str())?;

    // branch.autoSetupRebase also records `branch.<name>.rebase=true`, gated on
    // whether the upstream is local (`remote == "."`) or remote-tracking. git's
    // default is "never".
    let is_local = remote == ".";
    let want_rebase = match repo
        .config_snapshot()
        .string("branch.autoSetupRebase")
        .map(|v| v.to_str_lossy().into_owned())
        .as_deref()
    {
        Some("always") => true,
        Some("local") => is_local,
        Some("remote") => !is_local,
        _ => false, // "never" (default) or unset
    };
    if want_rebase {
        file.set_raw_value_by("branch", Some(sub), "rebase", "true")?;
    }

    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;

    println!("branch '{branch}' set up to track '{short}'.");
    Ok(())
}

// --- Ref / worktree helpers ------------------------------------------------

/// git's rejection for a non-branch target: `fatal: a branch is expected, got
/// <kind> '<x>'` plus the detach hint, exit 128.
fn branch_expected(repo: &gix::Repository, branch: &str) -> Result<ExitCode> {
    let hint =
        "hint: If you want to detach HEAD at the commit, try again with the --detach option.";
    if repo
        .try_find_reference(format!("refs/tags/{branch}").as_str())?
        .is_some()
    {
        eprintln!("fatal: a branch is expected, got tag '{branch}'");
        eprintln!("{hint}");
        return Ok(ExitCode::from(128));
    }
    match repo.rev_parse_single(BStr::new(branch)) {
        Ok(id) => {
            let full = id.detach().to_string();
            eprintln!("fatal: a branch is expected, got commit '{full}'");
            eprintln!("{hint}");
            Ok(ExitCode::from(128))
        }
        Err(_) => fatal(format!("invalid reference: {branch}")),
    }
}

/// Validate a name as a local branch, reporting git's fatal + advice on failure.
/// Returns `Some(exit)` when rejected, `None` when the name is valid.
fn reject_invalid_branch_name(
    repo: &gix::Repository,
    branch: &str,
    full: &str,
) -> Option<ExitCode> {
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_ok() {
        return None;
    }
    eprintln!("fatal: '{branch}' is not a valid branch name");
    if repo
        .config_snapshot()
        .boolean("advice.refSyntax")
        .unwrap_or(true)
    {
        eprintln!("hint: See 'git help check-ref-format'");
        eprintln!("hint: Disable this message with \"git config set advice.refSyntax false\"");
    }
    Some(ExitCode::from(128))
}

/// Resolve `@{-N}` to the branch that was left N checkouts ago, from the HEAD
/// reflog (mirrors `refs.c::interpret_nth_prior_checkout`).
fn resolve_prev_branch(repo: &gix::Repository, expanded: &str) -> Option<String> {
    let bytes = expanded.as_bytes();
    if !bytes.starts_with(b"@{-") {
        return None;
    }
    let brace = bytes.iter().position(|&c| c == b'}')?;
    let nth: i64 = std::str::from_utf8(&bytes[3..brace]).ok()?.parse().ok()?;
    if nth <= 0 {
        return None;
    }
    let mut nth = nth as usize;

    let head = repo.head().ok()?;
    let mut platform = head.log_iter();
    let log = platform.rev().ok()??;
    for line in log.filter_map(Result::ok) {
        let Some(from_to) = line.message.strip_prefix(b"checkout: moving from ") else {
            continue;
        };
        let Some(pos) = from_to.find(" to ") else {
            continue;
        };
        nth -= 1;
        if nth == 0 {
            return Some(from_to[..pos].to_str_lossy().into_owned());
        }
    }
    None
}

/// The tree of the current `HEAD` commit, or `None` on an unborn HEAD.
fn head_tree(repo: &gix::Repository) -> Result<Option<ObjectId>> {
    let mut head = repo.head()?;
    match head.try_peel_to_id()? {
        Some(id) => Ok(Some(
            repo.find_object(id.detach())?
                .peel_to_commit()?
                .tree_id()?
                .detach(),
        )),
        None => Ok(None),
    }
}

/// Ensure the tracked worktree may be rewritten and capture its current index.
/// Refuses a dirty worktree unless `force` is set (git's discard path).
fn prepare_worktree_change(repo: &gix::Repository, force: bool) -> Result<gix::index::File> {
    if !force && repo.is_dirty()? {
        bail!("your local changes would be overwritten; commit or stash them, or use --force (dirty-worktree switch is unsupported)");
    }
    Ok(repo.index_or_load_from_head()?.into_owned())
}

/// Point `HEAD` symbolically at `branch_ref`, requesting a `checkout: moving
/// from <from> to <to>` reflog entry.
///
/// Known gap: the vendored `gix-ref` drops the reflog entry for a symbolic-target
/// update (see `gix-ref/src/store/file/transaction/commit.rs`), so no
/// `.git/logs/HEAD` line is written here even though git writes one.
fn attach_head(
    repo: &gix::Repository,
    branch_ref: &FullName,
    from_desc: &str,
    to_short: &str,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("checkout: moving from {from_desc} to {to_short}").into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_ref.clone()),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

/// Point `HEAD` directly at object `id` (detached).
fn set_head_detached(repo: &gix::Repository, id: ObjectId, message: &str) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    Ok(())
}

/// Human description of where `HEAD` currently is, for the reflog `from` field.
fn describe_head(repo: &gix::Repository) -> Result<String> {
    if let Some(name) = repo.head_name()? {
        return Ok(name.shorten().to_string());
    }
    let mut head = repo.head()?;
    match head.try_peel_to_id()? {
        Some(id) => Ok(id.shorten_or_id().to_string()),
        None => Ok("HEAD".to_string()),
    }
}

/// Abbreviated hash + commit summary for `HEAD is now at …` / `Previous HEAD …`.
fn describe(repo: &gix::Repository, id: ObjectId) -> Result<(String, String)> {
    let abbrev = id.attach(repo).shorten_or_id().to_string();
    let commit = repo.find_object(id)?.peel_to_commit()?;
    let summary = commit.message()?.summary().into_owned().to_string();
    Ok((abbrev, summary))
}

/// Remove every tracked file from the worktree (pruning emptied parent dirs) and
/// write an empty index — the state a `--orphan` switch leaves behind.
fn clear_tracked_worktree(repo: &gix::Repository, old: &gix::index::File) -> Result<()> {
    let workdir = repo.workdir().map(|p| p.to_owned());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if let Some(full) = repo.workdir_path(path) {
                let _ = std::fs::remove_file(&full);
                if let Some(wd) = &workdir {
                    let mut dir = full.parent().map(|p| p.to_owned());
                    while let Some(d) = dir {
                        if d.as_path() == wd.as_path() || std::fs::remove_dir(&d).is_err() {
                            break;
                        }
                        dir = d.parent().map(|p| p.to_owned());
                    }
                }
            }
        }
    }
    let mut idx = repo.index_or_load_from_head()?.into_owned();
    idx.remove_entries(|_, _, _| true);
    idx.remove_tree();
    idx.write(Default::default())?;
    Ok(())
}

/// Move the worktree and index from `old` to the tree of commit `new_commit`.
///
/// Clean mode (`force == false`) writes only the files that differ from `old`.
/// Force mode (`force == true`) checks out the entire target tree, overwriting
/// any local modifications — git's `--force`/`--discard-changes` behavior.
fn update_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
    force: bool,
) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    let mut new_index = repo.index_from_tree(&new_tree_id)?;

    // The subset written to disk: in force mode the whole tree (discarding local
    // edits), otherwise only entries that differ from `old`.
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    if !force {
        subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
            Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
            None => false,
        });
    }

    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        &should_interrupt,
        opts,
    )?;

    // Remove files present in the old tree but not the new one.
    let new_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if !new_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
    }

    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}
