//! `git switch` — move `HEAD` to a branch, backed by the vendored gitoxide
//! ref store and worktree-state checkout.
//!
//! Supported invocations, reproduced from git 2.55.0 rather than inferred:
//! ```text
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
//! ```
//!
//! Stream and exit-code conventions: the informational messages (`Switched to
//! branch '<b>'`, `Switched to a new branch '<b>'`, `Switched to and reset branch
//! '<b>'`, `Reset branch '<b>'`, `Already on '<b>'`, `HEAD is now at …`,
//! `Previous HEAD position was …`) all go to **stderr**. The tracking notice
//! (`branch '<b>' set up to track '<u>'.`) goes to **stdout**. Failures print
//! `fatal: <reason>` on stderr and exit 128; option-parsing failures print
//! `error: <reason>` (with the usage block for unknown options) and exit 129.
//!
//! The worktree move is `checkout`'s, because git's is: both commands run the
//! same two-way `unpack_trees()` from the tree `HEAD` holds to the target's, so
//! a local modification to a file the two branches agree on is carried across
//! (and listed as `<status>\t<path>` on stdout), one to a file they disagree on
//! refuses the switch in `checkout`'s wording, and `--force`/`--discard-changes`
//! resets instead. See [`move_worktree`].
//!
//! The listing runs for every switch that names an operand, whether or not the
//! two trees differ — `merge_working_tree()` is entered unconditionally and the
//! call sits at its end, outside the merge. A switch with *no* operand
//! (`git switch -c <new>`, a bare `git switch --detach`) sets git's
//! `do_merge = 0` and prints nothing.
//!
//! `-m`/`--merge` and the `--conflict=<style>` that implies it are `checkout`'s
//! too, through the same [`super::checkout::move_worktree`]: git 2.55 answers a
//! refused two-way merge by autostashing the local changes, switching clean, and
//! re-applying the stash with a three-way merge. `-f`/`--discard-changes`
//! collide with it — `'-f' cannot be used with '-m'` and `'--discard-changes'
//! cannot be used with '--merge'`, two flags with two messages.
//!
//! Known divergences from git that are *not* fixable from this file:
//! ```text
//!   * No `.git/logs/HEAD` reflog line is written for the symbolic `HEAD` move —
//!     see [`attach_head`].
//!   * The "you are leaving N commit behind" orphaned-commit warning printed when
//!     abandoning a detached HEAD with unreachable commits is not reproduced
//!     (consistent with `checkout.rs`).
//! ```

use anyhow::{anyhow, Result};
// Every `print!`/`println!` below goes through git's stdout buffer; see
// `crate::cstdio` and the `defer()` call in `switch()`.
use crate::cstdio::{print, println};
use std::io::Write as _;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use super::checkout::TreeIsh;
use super::{Arg, LongOpt};

/// `cmd_switch()`'s option table (builtin/checkout.c:2148), in the order
/// `parse_options_concat()` builds it: `switch_options[]`, then
/// `add_common_options()`, then `add_common_switch_branch_options()`.
///
/// `add_checkout_path_options()` is *not* concatenated here, which is why
/// `--patch`, `--ours`, `--unified` and `--pathspec-from-file` are unknown to
/// `git switch` even though `git checkout` takes all four.
const LONG_OPTS: &[LongOpt] = &[
    // switch_options[] (builtin/checkout.c:2148)
    LongOpt { name: "create",                      neg: true,  arg: Arg::Required },
    LongOpt { name: "force-create",                neg: true,  arg: Arg::Required },
    LongOpt { name: "guess",                       neg: true,  arg: Arg::None },
    LongOpt { name: "discard-changes",             neg: true,  arg: Arg::None },
    // add_common_options() (builtin/checkout.c:1767)
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "recurse-submodules",          neg: true,  arg: Arg::Optional },
    LongOpt { name: "progress",                    neg: true,  arg: Arg::None },
    LongOpt { name: "merge",                       neg: true,  arg: Arg::None },
    LongOpt { name: "conflict",                    neg: true,  arg: Arg::Required },
    // add_common_switch_branch_options() (builtin/checkout.c:1787)
    LongOpt { name: "detach",                      neg: true,  arg: Arg::None },
    LongOpt { name: "track",                       neg: true,  arg: Arg::Optional },
    LongOpt { name: "force",                       neg: true,  arg: Arg::None },
    LongOpt { name: "orphan",                      neg: true,  arg: Arg::Required },
    LongOpt { name: "overwrite-ignore",            neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-other-worktrees",      neg: true,  arg: Arg::None },
];
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
    /// `-f`/`--force` → `opts->force`. Kept apart from `--discard-changes`
    /// because git refuses each against `--merge` with its own wording
    /// (builtin/checkout.c:1685-1689) before folding `force` into
    /// `discard_changes` (checkout.c:1921).
    force: bool,
    discard_changes: bool,
    /// `-m`/`--merge` → `opts->merge`, and the `--conflict=<style>` that implies it.
    merge: bool,
    conflict_style: Option<String>,
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
        discard_changes: false,
        merge: false,
        conflict_style: None,
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

        // Respell a unique abbreviation as the name it resolves to, so `--disc`
        // dispatches where `--discard-changes` dispatches. Short options and names
        // no entry claims come back untouched, so the refusals below still quote
        // what was typed.
        let canonical;
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(Parse::Failed(super::ambiguous_option(
                    a, &first, &second, USAGE,
                )))
            }
        };

        // Long options, with or without an attached `=value`.
        if let Some(long) = a.strip_prefix("--") {
            // The *value* is taken from the token as typed. The resolver copies it
            // through verbatim, so the two spell the same bytes — but only this one
            // borrows from `args`, which is what the parsed options hold on to.
            let attached = long.split_once('=').map(|(_, v)| v);
            // The *name* is the resolved spelling, and is only ever matched and
            // formatted, never stored, so it may borrow from `canonical`.
            let name = resolved.strip_prefix("--").unwrap_or(resolved);
            let name = name.split_once('=').map_or(name, |(n, _)| n);
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
                // The unset half of the three `OPT_STRING`s: parse-options writes
                // NULL over the slot (parse-options.c:200-202), so a later
                // `--no-create` discards an earlier `--create=<branch>`.
                "no-create" => p.create = None,
                "no-force-create" => p.force_create = None,
                "no-orphan" => p.orphan = None,
                "quiet" => p.quiet = true,
                "no-quiet" => p.quiet = false,
                "detach" => p.detach = true,
                "no-detach" => p.detach = false,
                "force" => p.force = true,
                "no-force" => p.force = false,
                "discard-changes" => p.discard_changes = true,
                "no-discard-changes" => p.discard_changes = false,
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
                "merge" => p.merge = true,
                // `--no-merge` is the `OPT_BOOL` unset (0). `--no-conflict` is
                // `parse_opt_conflict()`'s unset arm (builtin/checkout.c:1750,
                // `conflict_style = -1`), which NULLs the style without clearing
                // the `merge` an earlier `--conflict` set.
                "no-merge" => p.merge = false,
                "no-conflict" => p.conflict_style = None,
                "conflict" => {
                    let v = take_value!("conflict");
                    if !matches!(v, "merge" | "diff3" | "zdiff3") {
                        return Ok(Parse::Failed(unknown_option(format!(
                            "unknown style '{v}' given for '--conflict'"
                        ))));
                    }
                    p.conflict_style = Some(v.to_string());
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
                // The message names the argument as typed, `=<value>` and all
                // (parse-options.c:1215-1216), so it quotes `long` whole.
                _ => {
                    return Ok(Parse::Failed(unknown_option(format!(
                        "unknown option `{long}'"
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
                    'q' => p.quiet = true,
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
                    'm' => p.merge = true,
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

        // A non-option argument is handed back unchanged by the resolver, so the
        // argv slice itself is pushed and the operand keeps `args`' lifetime.
        p.positionals.push(args[i].as_str());
        i += 1;
    }

    Ok(Parse::Ok(p))
}

pub fn switch(args: &[String]) -> Result<ExitCode> {
    // Same split as `checkout`: the worktree-change listing is stdout, the
    // `Switched to …` line is stderr, and stdio's buffering of the first is what
    // orders them for a caller capturing both. See `crate::cstdio`.
    crate::cstdio::defer();
    // `-h` as any argument prints usage on stdout and exits 129.
    //
    // `--help-all` prints the same block. parse_options_step() tests it with a
    // `strcmp()` of its own, ahead of parse_long_opt(), so the name never
    // abbreviates and never takes an `=<value>` — it is deliberately not a
    // [`LONG_OPTS`] entry. That test sits inside the argv loop *after* the `--`
    // and `--end-of-options` breaks, so unlike `-h` it is not seen past either
    // terminator, which is what the `take_while` reproduces. switch's option
    // table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` is this block.
    let help_all = args
        .iter()
        .take_while(|a| a.as_str() != "--" && a.as_str() != "--end-of-options")
        .any(|a| a == "--help-all");
    if help_all || args.iter().any(|a| a == "-h") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let p = match parse(args)? {
        Parse::Ok(p) => p,
        Parse::Failed(code) => return Ok(code),
    };

    // `if (opts->merge == -1) opts->merge = opts->conflict_style >= 0;`
    // (builtin/checkout.c:1918-1919): `--conflict=<style>` implies `--merge`,
    // and a later `--no-conflict` takes the implication with it because it
    // NULLs the style before this runs.
    let merge = p.merge || p.conflict_style.is_some();
    // Absent the option the style is `merge.conflictStyle`'s, which implies
    // nothing on its own.
    let conflict_style_cli = p.conflict_style.clone();

    // git's mutual-exclusion checks, in its order.
    let create_modes =
        p.create.is_some() as u8 + p.force_create.is_some() as u8 + p.orphan.is_some() as u8;
    if create_modes > 1 {
        return fatal("options '-c', '-C', and '--orphan' cannot be used together");
    }
    // `if (opts->force && opts->merge) die(_("'%s' cannot be used with '%s'"), "-f", "-m");`
    // and the `--discard-changes`/`--merge` pairing right after it
    // (builtin/checkout.c:1684-1689) — two separate flags with two separate
    // messages, so `switch` cannot fold them into one.
    if p.force && merge {
        return fatal("'-f' cannot be used with '-m'");
    }
    if p.discard_changes && merge {
        return fatal("'--discard-changes' cannot be used with '--merge'");
    }
    if p.detach && create_modes >= 1 {
        return fatal("'--detach' cannot be used with '-b/-B/--orphan'");
    }

    // Every ref this moves carries a reflog line, and git writes those with an
    // identity it synthesizes from the OS when `user.*` is unset — only a
    // `commit` with nothing determinable is refused. Without this a bare runner,
    // a container or a `sudo` shell cannot switch branches at all, and a
    // recursive submodule walk aborts on the first one it reaches.
    let mut repo = gix::discover(".")?;
    crate::ensure_reflog_identity(&mut repo);

    // DWIM default: `--[no-]guess` on the CLI wins, else `checkout.guess`
    // (git's default is on).
    let guess = p
        .guess
        .unwrap_or_else(|| repo.config_snapshot().boolean("checkout.guess") != Some(false));

    // `if (opts->force) { opts->discard_changes = 1; … }` (builtin/checkout.c:1921)
    // — `merge_working_tree()` reads only `discard_changes`, so from here on the
    // two flags are one.
    let discard = p.force || p.discard_changes;

    // `--conflict=<style>`'s effective value: the flag, else `merge.conflictStyle`,
    // else git's built-in `merge`.
    let conflict_style = conflict_style_cli
        .or_else(|| {
            repo.config_snapshot()
                .string("merge.conflictStyle")
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| "merge".to_string());

    // `opts->merge` for the call sites below: `Some(<style>)` when `-m` is in
    // effect, `None` otherwise. Only the style travels — the other half of
    // [`super::checkout::MergeOpt`], the switch target as the user spelled it, is
    // what each call site knows and this one does not.
    let merge_style: Option<&str> = merge.then_some(conflict_style.as_str());

    // The `--track` DWIM (builtin/checkout.c:1964-1975), shared verbatim with
    // `checkout` except for the branch-creating option it names: `cb_option` is
    // `'c'` for `switch`, so the hint reads `try -c`. It runs before either
    // half's own gates, and `--orphan` suppresses it because `opts->new_branch`
    // has already absorbed the orphan name.
    let create_from_track = if p.track.is_some() && create_modes == 0 {
        let Some(argv0) = p.positionals.first().copied() else {
            return fatal("--track needs a branch name");
        };
        let stem = argv0.strip_prefix("refs/").unwrap_or(argv0);
        let stem = stem.strip_prefix("remotes/").unwrap_or(stem);
        match stem.split_once('/') {
            Some((_, rest)) if !rest.is_empty() => Some(rest.to_string()),
            _ => return fatal("missing branch name; try -c"),
        }
    } else {
        None
    };

    if let Some(name) = p.orphan {
        if p.track.is_some() {
            return fatal("'--orphan' cannot be used with '-t'");
        }
        return switch_orphan(&repo, name, &p.positionals, discard, p.quiet);
    }
    if p.detach {
        if create_from_track.is_some() {
            return fatal("'--detach' cannot be used with '-b/-B/--orphan'");
        }
        return switch_detach(&repo, &p.positionals, discard, p.quiet, merge_style);
    }
    if let Some(name) = p.force_create {
        return switch_create(&repo, name, true, &p.positionals, p.quiet, discard, p.track,
            merge_style);
    }
    if let Some(name) = p.create.map(str::to_string).or(create_from_track) {
        return switch_create(
            &repo,
            &name,
            false,
            &p.positionals,
            p.quiet,
            discard,
            p.track,
            merge_style,
        );
    }
    if p.positionals.is_empty() {
        return fatal("missing branch or commit argument");
    }
    switch_existing(&repo, &p.positionals, p.quiet, guess, discard,
        merge_style)
}


/// `git switch <branch>` — attach `HEAD` to an existing local branch, with DWIM
/// (`--guess`) fallback to a remote-tracking branch.
fn switch_existing(
    repo: &gix::Repository,
    positionals: &[&str],
    quiet: bool,
    guess: bool,
    force: bool,
    merge_style: Option<&str>,
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

    // `parse_branchname_arg()` resolves the name through `get_oid_mb()`, so the
    // `core.warnAmbiguousRefs` warning `get_oid_basic()` emits lands here, before
    // anything else switch prints. `--quiet` does not suppress it: git's
    // `GET_OID_QUIETLY` is a rev-parse concept, and stock still warns under
    // `git switch -q`.
    super::rev_parse::warn_ambiguous_refname(repo, branch, false);

    let full = format!("refs/heads/{branch}");

    // Already on the requested branch → git reports it and exits 0 untouched.
    // `merge_working_tree()` still runs (the branch *is* named, so `do_merge`
    // stays set), and its listing of local changes comes before the message.
    //
    // `update_refs_for_switch()` still runs too: its `refs_update_symref("HEAD",
    // new_branch_info->path, msg.buf)` is unconditional for a named branch and
    // only *then* does it choose between `Reset branch` / `Already on` / the two
    // `Switched to` wordings (builtin/checkout.c:1017-1032). So a no-op
    // `git switch <current>` appends `checkout: moving from <b> to <b>` to
    // `logs/HEAD` every time, exactly as `git checkout <current>` does.
    if repo.head_name()?.as_ref().map(|n| n.as_bstr())
        == FullName::try_from(full.as_str())
            .ok()
            .as_ref()
            .map(|n| n.as_bstr())
    {
        if !force {
            super::checkout::show_local_changes(branch, quiet)?;
        }
        if let Ok(name) = FullName::try_from(full.as_str()) {
            let from_desc = describe_head(repo)?;
            attach_head(repo, &name, &from_desc, branch)?;
        }
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
                    return switch_create(
                        repo, branch, false, &sp, quiet, force, None, merge_style,
                    );
                }
                Dwim::Many { count } => {
                    crate::advice::ambiguous_remote_branch_name(repo, "switch");
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

    let from_desc = describe_head(repo)?;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Entered unconditionally: `switch <branch>` always names an operand, so
    // git's `do_merge` stays 1 and `merge_working_tree()` — listing and all —
    // runs even when the two branches point at the same tree.
    let target_tree = repo.find_object(target)?.peel_to_commit()?.tree_id()?.detach();
    let cur_tree = head_tree(repo)?.unwrap_or_else(|| repo.empty_tree().id().detach());
    let merge = merge_style.map(|style| super::checkout::MergeOpt { style, name: branch });
    let autostashed = match move_worktree(
        repo,
        cur_tree,
        target_tree,
        force,
        quiet,
        &target.to_string(),
        merge,
    )? {
        Err(code) => return Ok(code),
        Ok(stashed) => stashed,
    };

    attach_head(repo, &full_name, &from_desc, branch)?;
    if !quiet {
        eprintln!("Switched to branch '{branch}'");
        // `report_tracking()`, which `cmd_switch` reaches through the same
        // `update_refs_for_switch()` `checkout` does.
        super::checkout::print_tracking_status(repo);
    }
    if autostashed {
        show_autostash_listing(&target.to_string(), quiet)?;
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
    merge_style: Option<&str>,
) -> Result<ExitCode> {
    if positionals.len() > 1 {
        return fatal("only one reference expected");
    }
    let start = positionals.first().copied();
    let full = format!("refs/heads/{branch}");

    if let Some(code) = reject_invalid_branch_name(repo, branch) {
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
        .map(|n| n.shorten() == branch)
        .unwrap_or(false);

    // `switch` shares `checkout`'s `parse_branchname_arg()`, so the start-point is
    // resolved by `get_oid_mb()` — where a full-length hex name is the id itself,
    // odb untouched — and only then looked up. An absent id therefore reaches
    // `unable to read tree`, not `invalid reference`, and a tag is peeled to the
    // commit the branch is actually created at.
    let start_commit: Option<ObjectId> = match start {
        Some(s) => {
            let Some(id) = crate::objname::resolve(repo, s) else {
                return fatal(format!("invalid reference: {s}"));
            };
            let commit = match super::checkout::classify_tree_ish(repo, id)? {
                TreeIsh::Commit(commit) => Some(commit.id),
                TreeIsh::Tree(_) => {
                    return fatal(format!("Cannot switch branch to a non-commit '{s}'"))
                }
            };
            // `create_branch()` hands the start-point to `dwim_branch_start()`
            // (branch.c:539-594), which resolves it a *second* time and then
            // DWIMs it — so the name warns twice and more than one matching ref
            // is fatal before the branch exists:
            //
            // ```c
            // if (repo_get_oid_mb(r, start_name, &oid)) { … }
            // switch (repo_dwim_ref(r, start_name, strlen(start_name), &oid, &real_ref, 0)) {
            // …
            // default:
            //         die(_("ambiguous object name: '%s'"), start_name);
            // }
            // ```
            crate::objname::warn_ambiguous_refname(repo, s);
            if super::rev_parse::dwim_ref_matches(repo, s).len() > 1 {
                return fatal(format!("ambiguous object name: '{s}'"));
            }
            commit
        }
        None => current_commit,
    };
    let from_desc = describe_head(repo)?;

    // Determine the upstream ref this branch should track, if any.
    let upstream = tracking_upstream(repo, start, track, branch);

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

    // ```c
    // if (!new_branch_info->name) {
    //         new_branch_info->name = xstrdup("HEAD");
    //         new_branch_info->commit = old_branch_info.commit;
    //         …
    //         if (opts->only_merge_on_switching_branches)
    //                 do_merge = 0;
    // }
    // ```
    // (builtin/checkout.c:1195-1203.) `only_merge_on_switching_branches` is set
    // for `switch`, so `merge_working_tree()` is skipped only when **no operand
    // was given at all** — not when the operand happens to resolve to the commit
    // `HEAD` already holds. `switch -c <new> <start>` therefore still runs the
    // merge, and still prints its closing listing, even where `<start>` is the
    // current commit.
    let needs_worktree = start.is_some();
    let mut autostashed = false;
    let trees = if needs_worktree {
        let target_tree = repo.find_object(start_commit)?.peel_to_commit()?.tree_id()?.detach();
        let cur_tree = head_tree(repo)?.unwrap_or_else(|| repo.empty_tree().id().detach());
        // Refuse before the branch is created, so a blocked switch leaves no ref
        // behind — git's `merge_working_tree()` runs before `update_refs_for_switch()`.
        // `-m` is exempt: it has an answer for the paths the two-way merge
        // rejects, so the refusal it would raise here is not the final one.
        if let Some(code) = if force || merge_style.is_some() {
            None
        } else {
            super::checkout::ensure_clean(repo, cur_tree, target_tree)?
        } {
            return Ok(code);
        }
        Some((cur_tree, target_tree))
    } else {
        None
    };

    // Create fresh, or force-move an existing branch for -C.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                // `create_branch()` logs the start-point as the caller spelled it, and
                // `HEAD` when none was given — not the branch `HEAD` happens to be on.
                message: format!("branch: Created from {}", start.unwrap_or("HEAD")).into(),
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

    if let Some((cur_tree, target_tree)) = trees {
        let merge = merge_style.map(|style| super::checkout::MergeOpt { style, name: branch });
        match move_worktree(
            repo,
            cur_tree,
            target_tree,
            force,
            quiet,
            &start_commit.to_string(),
            merge,
        )? {
            Err(code) => return Ok(code),
            Ok(stashed) => autostashed = stashed,
        }
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
    if autostashed {
        show_autostash_listing(&start_commit.to_string(), quiet)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `git switch -d|--detach [<commit>]` — detach `HEAD` at a commit.
fn switch_detach(
    repo: &gix::Repository,
    positionals: &[&str],
    force: bool,
    quiet: bool,
    merge_style: Option<&str>,
) -> Result<ExitCode> {
    if positionals.len() > 1 {
        return fatal("only one reference expected");
    }

    let target_id: ObjectId = match positionals.first().copied() {
        // Same `get_oid_mb()` ordering as the `-c` path above: a full-length hex
        // name resolves without the odb, so `git switch --detach <absent-sha>` is
        // git's `unable to read tree`, not `invalid reference`.
        Some(s) => {
            let Some(id) = crate::objname::resolve(repo, s) else {
                return fatal(format!("invalid reference: {s}"));
            };
            // `setup_new_branch_info_and_source_tree()` (`builtin/checkout.c:1311`)
            // resolves the operand a second time through `setup_branch_path()`:
            //
            // ```c
            // if (!repo_dwim_ref(the_repository, branch->name, strlen(branch->name),
            //                    &branch->oid, &branch->refname, 0))
            //         repo_get_oid_committish(the_repository, branch->name, &branch->oid);
            // ```
            //
            // (`builtin/checkout.c:804-806`.) `<ref>@{<n>}` is never a ref name, so
            // the fallback always fires for it and stock 2.55.0 prints
            // `warning: log for 'HEAD' only goes back to …` twice for
            // `git switch --detach 'HEAD@{<old date>}'`, where a plain branch name
            // stops at `repo_dwim_ref()` and warns once.
            if super::rev_parse::dwim_ref_matches(repo, s).is_empty() {
                crate::objname::resolve(repo, s);
            }
            match super::checkout::classify_tree_ish(repo, id)? {
                TreeIsh::Commit(commit) => commit.id,
                TreeIsh::Tree(_) => {
                    return fatal(format!("Cannot switch branch to a non-commit '{s}'"))
                }
            }
        }
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

    // Same `do_merge` rule as [`switch_create`]: a bare `switch --detach` names no
    // operand, so `merge_working_tree()` never runs; with one it always does,
    // whatever the trees turn out to be.
    let mut autostashed = false;
    if !positionals.is_empty() {
        let from_tree = cur_tree.unwrap_or_else(|| repo.empty_tree().id().detach());
        // The `ours` label and the autostash message name the target as typed —
        // `git switch -m --detach HEAD~1` writes `HEAD~1`, not the id.
        let name = positionals.first().copied().unwrap_or("HEAD");
        let merge = merge_style.map(|style| super::checkout::MergeOpt { style, name });
        match move_worktree(
            repo,
            from_tree,
            target_tree,
            force,
            quiet,
            &target_id.to_string(),
            merge,
        )? {
            Err(code) => return Ok(code),
            Ok(stashed) => autostashed = stashed,
        }
    }

    // git logs the name the caller typed, not the id it resolved to — `switch
    // --detach HEAD~1` records `to HEAD~1`, and a bare `--detach` (which detaches
    // where `HEAD` already is) records `to HEAD`.
    let to_desc = positionals.first().copied().unwrap_or("HEAD").to_string();
    set_head_detached(
        repo,
        target_id,
        &format!("checkout: moving from {from_desc} to {to_desc}"),
        old_id,
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
    if autostashed {
        show_autostash_listing(&target_id.to_string(), quiet)?;
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
        let Some(id) = crate::objname::resolve(repo, p) else {
            return fatal(format!("invalid reference: {p}"));
        };
        // `parse_branchname_arg()` still has to read the object it resolved, and
        // fails first when it cannot: `--orphan <absent-full-hex>` is git's
        // `unable to read tree`, while `--orphan <tree>` reaches the refusal below.
        super::checkout::classify_tree_ish(repo, id)?;
        return fatal("'--orphan' cannot take <start-point>");
    }

    let full = format!("refs/heads/{branch}");
    if let Some(code) = reject_invalid_branch_name(repo, branch) {
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
        // `orphan_from_empty_tree`: the target tree is the empty one, so the same
        // two-way pass every switch runs decides this — and refuses in the same
        // words. No listing follows: `new_branch_info->commit` is NULL, which is
        // what `show_local_changes()` is guarded on.
        if !force {
            let cur_tree = head_tree(repo)?.unwrap_or_else(|| repo.empty_tree().id().detach());
            let empty = repo.empty_tree().id().detach();
            if let Some(code) = super::checkout::ensure_clean(repo, cur_tree, empty)? {
                return Ok(code);
            }
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
    Many { count: usize },
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
        n => {
            // checkout.defaultRemote disambiguates: if it names one of the
            // matching remotes, DWIM to that one instead of erroring.
            if let Some(def) = repo.config_snapshot().string("checkout.defaultRemote") {
                let def = def.to_str_lossy().into_owned();
                if matches.contains(&def) {
                    return Ok(Dwim::One(format!("{def}/{name}")));
                }
            }
            Ok(Dwim::Many { count: n })
        }
    }
}

/// The upstream a newly created branch should track: `(remote, merge_ref,
/// short)`. Auto-set when the start-point is a remote-tracking branch (git's
/// default `branch.autoSetupMerge=true`); a local branch is tracked only with an
/// explicit `--track`. `--no-track` disables it entirely.
fn tracking_upstream(
    repo: &gix::Repository,
    start: Option<&str>,
    track: Option<bool>,
    new_branch: &str,
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
    let explicit = track == Some(true); // `--track` given

    // branch.autoSetupMerge decides when tracking is auto-installed (no --track).
    // git's default is "true". `--track` overrides it and always tracks.
    let snap = repo.config_snapshot();
    let mode = snap
        .string("branch.autoSetupMerge")
        .map(|v| v.to_str_lossy().to_ascii_lowercase());
    let mode = mode.as_deref();
    let off = matches!(mode, Some("false" | "no" | "off" | "0"));

    if let Some(rest) = s.strip_prefix("refs/remotes/") {
        // Remote-tracking start-point.
        let (remote, branch) = rest.split_once('/')?;
        let auto = if off {
            false
        } else if mode == Some("simple") {
            // "simple": only when the local and remote branch names match.
            branch == new_branch
        } else {
            true // "true"/"always"/"inherit"/unset auto-track a remote start
        };
        if explicit || auto {
            return Some((
                remote.to_string(),
                format!("refs/heads/{branch}"),
                format!("{remote}/{branch}"),
            ));
        }
        return None;
    }

    if let Some(branch) = s.strip_prefix("refs/heads/") {
        // Local branch start-point.
        if explicit || mode == Some("always") {
            // Track the local branch itself (the "." remote).
            return Some((
                ".".to_string(),
                format!("refs/heads/{branch}"),
                branch.to_string(),
            ));
        }
        if mode == Some("inherit") {
            // Copy the start branch's own upstream, if it has one.
            let remote = snap.string(&format!("branch.{branch}.remote"))?.to_str_lossy().into_owned();
            let merge = snap.string(&format!("branch.{branch}.merge"))?.to_str_lossy().into_owned();
            let short = match merge.strip_prefix("refs/heads/") {
                Some(b) if remote == "." => b.to_string(),
                Some(b) => format!("{remote}/{b}"),
                None => merge.clone(),
            };
            return Some((remote, merge, short));
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
    // `die_expecting_a_branch()` first asks `repo_dwim_ref()` whether the name
    // resolves to exactly one ref, and names *that ref* — short of its
    // `refs/tags/` or `refs/remotes/` prefix — rather than an object id. Only a
    // name that is no ref at all (a raw commit id, `HEAD^`, …) falls through to
    // the `got commit '<name>'` wording, and that one echoes the *argument*.
    let code = match dwim_ref(repo, branch) {
        Some(full) => {
            if let Some(tag) = full.strip_prefix("refs/tags/") {
                eprintln!("fatal: a branch is expected, got tag '{tag}'");
            } else if let Some(rb) = full.strip_prefix("refs/remotes/") {
                eprintln!("fatal: a branch is expected, got remote branch '{rb}'");
            } else {
                eprintln!("fatal: a branch is expected, got '{full}'");
            }
            ExitCode::from(128)
        }
        None => {
            let Some(id) = crate::objname::resolve(repo, branch) else {
                return fatal(format!("invalid reference: {branch}"));
            };
            // `parse_branchname_arg()` reads the object before `switch` gets to
            // complain that the name is not a branch, so an id this repository does
            // not have is `unable to read tree` and a tree is the non-commit
            // refusal. Neither carries the detach hint below.
            if let TreeIsh::Tree(_) = super::checkout::classify_tree_ish(repo, id)? {
                return fatal(format!("Cannot switch branch to a non-commit '{branch}'"));
            }
            eprintln!("fatal: a branch is expected, got commit '{branch}'");
            ExitCode::from(128)
        }
    };
    // git checks `advice_enabled()` itself and then calls plain `advise()`, so
    // there is no `Disable this message with …` trailer on this one.
    crate::advice::Advice::SuggestDetachingHead.advise_plain_in(
        repo,
        "If you want to detach HEAD at the commit, try again with the --detach option.",
    );
    Ok(code)
}

/// `repo_dwim_ref()` for the `die_expecting_a_branch` case: the ref git treats as
/// *the* match for `name`, or `None` when it names no ref. `HEAD` is in the rules
/// and is resolved through, so `git switch HEAD` reports the branch HEAD points
/// at rather than an object id.
fn dwim_ref(repo: &gix::Repository, name: &str) -> Option<String> {
    super::rev_parse::dwim_ref_matches(repo, name).into_iter().next()
}

/// Validate a name as a local branch, reporting git's fatal + advice on failure.
/// Returns `Some(exit)` when rejected, `None` when the name is valid.
fn reject_invalid_branch_name(repo: &gix::Repository, branch: &str) -> Option<ExitCode> {
    if super::branch::valid_branch_name(branch) {
        return None;
    }
    eprintln!("fatal: '{branch}' is not a valid branch name");
    crate::advice::Advice::RefSyntax.advise_in(repo, "See 'git help check-ref-format'");
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

/// `merge_working_tree()` for a switch: move the worktree and index from the
/// tree `HEAD` holds now onto `target_tree`, then list what was carried across.
///
/// This is `checkout`'s path verbatim, because git's is: both commands run the
/// same two-way `unpack_trees()`, so a local change to a file the two branches
/// agree on comes along, one to a file they disagree on refuses the switch (in
/// `checkout`'s wording — `git switch` does not have its own), and `--force` /
/// `--discard-changes` resets instead.
///
/// Returns the exit code of a refusal, or `None` when the worktree moved.
///
/// `cur_tree`/`target_tree` may be equal: `merge_working_tree()` is entered for
/// every switch, and its closing `show_local_changes()` runs whether or not the
/// two-way merge had anything to do. Moving that listing inside a
/// "the trees differ" guard silently drops it for the common case of switching
/// between two branches that point at the same tree.
///
/// The gate, the write-out and the `-m` autostash/re-apply are
/// [`super::checkout::move_worktree`]'s, not a second copy: git has one
/// `merge_working_tree()` and both commands call it, so `git switch -m` carries
/// local changes across exactly as `git checkout -m` does, `--conflict=<style>`
/// included.
fn move_worktree(
    repo: &gix::Repository,
    cur_tree: ObjectId,
    target_tree: ObjectId,
    discard: bool,
    quiet: bool,
    listing_rev: &str,
    merge: Option<super::checkout::MergeOpt<'_>>,
) -> Result<Result<bool, ExitCode>> {
    if discard {
        // `opts->discard_changes` → `reset_tree()`, and the listing is skipped:
        // `if (!opts->discard_changes && !opts->quiet && …)` (checkout.c:930).
        super::checkout::reset_worktree_to_tree(repo, target_tree)?;
        return Ok(Ok(false));
    }
    let mut autostashed = false;
    if cur_tree != target_tree {
        match super::checkout::move_worktree(repo, cur_tree, target_tree, merge)? {
            super::checkout::Moved::Refused(code) => return Ok(Err(code)),
            super::checkout::Moved::Autostashed => autostashed = true,
            super::checkout::Moved::Clean => {}
        }
    }
    if !autostashed {
        super::checkout::show_local_changes(listing_rev, quiet)?;
    }
    Ok(Ok(autostashed))
}

/// The second, headed listing an autostashed switch prints once `HEAD` has moved
/// (builtin/checkout.c:1265-1271). Kept next to [`move_worktree`] so the two
/// halves of git's one `created_autostash` branch stay together.
fn show_autostash_listing(listing_rev: &str, quiet: bool) -> Result<()> {
    if quiet {
        return Ok(());
    }
    println!("The following paths have local changes:");
    super::checkout::show_local_changes(listing_rev, quiet)
}

/// Point `HEAD` symbolically at `branch_ref`, logging the `checkout: moving from
/// <from> to <to>` entry git writes.
///
/// The vendored `gix-ref` drops the reflog entry for a symbolic-target update
/// (see `gix-ref/src/store/file/transaction/commit.rs`), so the line is written
/// afterwards by [`super::checkout::record_head_move`] — the same repair
/// `git checkout` makes.
fn attach_head(
    repo: &gix::Repository,
    branch_ref: &FullName,
    from_desc: &str,
    to_short: &str,
) -> Result<()> {
    let from = repo.head().ok().and_then(|mut h| {
        h.try_peel_to_id()
            .ok()
            .flatten()
            .map(|id| id.detach())
    });
    let message = format!("checkout: moving from {from_desc} to {to_short}");
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.clone().into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_ref.clone()),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    let to = repo
        .find_reference(branch_ref.as_ref())
        .ok()
        .and_then(|mut r| r.peel_to_id_in_place().ok().map(|id| id.detach()));
    super::checkout::append_head_log(repo, from, to, &message);
    Ok(())
}

/// Point `HEAD` directly at object `id` (detached).
/// Detach `HEAD` at `id`, logging the move.
///
/// `from` is what `HEAD` resolved to beforehand: the vendored `gix-ref` leaves the
/// old field of a `deref: false` update null when the previous value was a symref,
/// so the line is repaired afterwards by [`super::checkout::record_head_move`].
fn set_head_detached(
    repo: &gix::Repository,
    id: ObjectId,
    message: &str,
    from: Option<ObjectId>,
) -> Result<()> {
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
    super::checkout::record_head_move(repo, from, Some(id), message);
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
    // `unpack_trees()` ends with `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`
    // (unpack-trees.c:2088-2092), so the index git leaves here carries a cache-tree.
    super::write_tree::rebuild_cache_tree(repo, &mut idx);
    idx.write(crate::config::index_write_options(repo))?;
    Ok(())
}

