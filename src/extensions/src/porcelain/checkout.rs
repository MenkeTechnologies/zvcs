//! `git checkout` — the legacy combined verb: switch branches (or detach at a
//! commit) *and* restore paths, backed by the vendored gitoxide crates so tools
//! on PATH see the same `.git`, index, and worktree.
//!
//! Supported invocations (the common forms):
//! ```text
//!   * `git checkout <branch>`                 → switch to an existing local branch
//!   * `git checkout <commit-ish>`             → detach `HEAD` at a commit
//!   * `git checkout -d|--detach <rev>`        → detach even when `<rev>` is a branch
//!   * `git checkout -b <name> [<start>]`      → create branch at `<start>` (default HEAD), switch
//!   * `git checkout -B <name> [<start>]`      → create-or-reset branch at `<start>`, switch
//!   * `git checkout [<tree-ish>] -- <path>…`  → restore paths (index+worktree from `<tree-ish>`; worktree-only from index when no tree-ish)
//!   * `git checkout <path>…`                  → restore paths from the index (bare pathspec form)
//!   * `git checkout <remote-only-name>`         → DWIM (`--guess`, the default):
//!                                                create-and-track a local branch
//!                                                when exactly one remote has it.
//!   * `git checkout -f|--force [<commit-ish>]`  → git's `discard_changes`: the
//!                                                worktree and index are reset to
//!                                                the target tree through
//!                                                `reset_tree()`, throwing away
//!                                                modified, staged, conflicted and
//!                                                deleted tracked files instead of
//!                                                refusing or carrying them
//!   * `git checkout [-f]`                       → no ref moves; only the worktree
//!                                                reconciliation runs
//!   * `git checkout HEAD` / `git checkout @`    → likewise a no-op for every ref:
//!                                                there is no `refs/heads/HEAD` to
//!                                                switch to, so this is NOT a detach
//!   * `git checkout --no-overlay <tree-ish> -- <path>…` → also delete paths that
//!                                                match the pathspec but are absent
//!                                                from `<tree-ish>` (overlay mode,
//!                                                the default, never removes).
//!   * `git checkout --pathspec-from-file <file>` (`--pathspec-file-nul`) → read
//!                                                pathspecs from `<file>` (or stdin
//!                                                for `-`) instead of the argv.
//!   * `-q`/`--quiet` suppress the transition messages
//! ```
//!
//! Every transition message (`Switched to …`, `Already on …`, `Reset branch …`,
//! `HEAD is now at …`, `Previous HEAD position was …`, `Updated N path(s) …`,
//! and the `advice.detachedHead` block) goes to **stderr**, as in stock git —
//! `git checkout` writes nothing to stdout on success.
//!
//! Deviations (honest, conservative — never corrupting):
//! ```text
//!   * An *unforced* branch/commit switch that changes the working tree requires
//!     a clean tracked worktree. Stock git also permits a switch when the dirty
//!     files do not collide with the diff between trees; that non-conflicting case
//!     is refused here (message names it) rather than risking an incorrect merge.
//!     `-f`/`--force` is not affected — it discards the changes outright, as git
//!     does.
//!   * An unforced switch does not print git's `show_local_changes()` name-status
//!     listing except on the no-ref path (`git checkout`, `git checkout HEAD`),
//!     where it is the only output there is.
//!     Switches whose target tree equals the current tree (e.g. `-b` at HEAD, or
//!     two branches on the same commit) carry local changes and are never
//!     refused. Untracked files are ignored for the clean check, matching git.
//!   * Pathspecs match literal files and directory prefixes (and `.`); general
//!     glob magic is left to the shell.
//!   * `--ours`/`--theirs` write a conflicted path's stage-2/stage-3 blob into
//!     the worktree (index left conflicted), `-t`/`--track` create-and-track,
//!     `--orphan` starts an unborn branch — all matching stock git.
//!   * `-m`/`--merge` is accepted: with a clean worktree it is byte-identical to
//!     a plain switch, and the dirty case is governed by the same conservative
//!     clean-check as every other switch here.
//!   * `-p`/`--patch` runs the interactive hunk selector ([`super::add_patch`]),
//!     restoring the picked hunks into the index and the worktree.
//!   * `-U`/`--unified <n>`, `--inter-hunk-context <n>` and `--[no-]auto-advance`
//!     configure that hunk selector and nothing else, but are still observable
//!     without `--patch`: their values go through parse-options' `OPT_INTEGER`
//!     validation and `cmd_checkout()` then refuses any non-default one with
//!     `fatal: '--unified' cannot be negative` / `fatal: the option '<x>'
//!     requires '--patch'`, right after the parse and before any ref or pathspec
//!     is resolved. Shared with `git reset` — see [`super::reset::PatchDiffOpts`].
//!   * `--conflict <style>` is validated and implies `-m`; the style only affects
//!     the deferred dirty-merge rendering, so on the clean-switch path honored
//!     here it is a no-op (the 3-way carry is refused by the same clean-check as
//!     every other switch).
//! ```

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

pub fn checkout(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // --- Argument classification -------------------------------------------
    // `new_branch` is Some((name, reset_if_exists)) for -b / -B.
    let mut new_branch: Option<(String, bool)> = None;
    let mut detach = false;
    let mut quiet = false;
    // `-f`/`--force` → git's `opts->discard_changes`.
    let mut force = false;
    let mut track = false;
    let mut orphan: Option<String> = None;
    // Which conflict stage `--ours`/`--theirs` writes out (2 = ours, 3 = theirs);
    // the last of the two flags wins, exactly like git's `opts.writeout_stage`.
    let mut writeout_stage: Option<u8> = None;
    let mut pre: Vec<&str> = Vec::new(); // positionals before `--`
    let mut post: Vec<&str> = Vec::new(); // pathspecs after `--`
    let mut has_dashdash = false;
    // `None` = fall back to `checkout.guess` (default on); `Some(b)` = `--[no-]guess`.
    let mut guess_flag: Option<bool> = None;
    // Overlay (default) never removes paths; `--no-overlay` deletes paths that
    // match the pathspec but are absent from the source tree.
    let mut overlay = true;
    let mut pathspec_from_file: Option<String> = None;
    let mut pathspec_file_nul = false;
    // `--recurse-submodules[=<pathspec>]` / `--no-recurse-submodules`. `None` =
    // fall back to `submodule.recurse` config; `Some(b)` = explicit flag. After a
    // switch, each initialized submodule's worktree is moved to the gitlink the
    // superproject now records (git's `submodule_move_head` via the worktree updater).
    let mut recurse_submodules: Option<bool> = None;
    // `-U`/`--unified`, `--inter-hunk-context`, `--[no-]auto-advance`: the
    // interactive-hunk-selector options, shared verbatim with `git reset` and
    // refused after the loop exactly as git refuses them without `--patch`.
    let mut patch_opts = super::reset::PatchDiffOpts::default();
    // `-p`/`--patch`: hand the paths to the interactive hunk selector instead of
    // restoring them wholesale.
    let mut patch_mode = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // A `-U`/`--unified`/`--inter-hunk-context` value still owed is taken
        // verbatim even past `--`, the way parse-options takes it; outside that,
        // these options are only recognised before `--`.
        if patch_opts.awaiting_value() || !has_dashdash {
            match patch_opts.take_arg(a) {
                Err(code) => return Ok(code),
                Ok(true) => {
                    i += 1;
                    continue;
                }
                Ok(false) => {}
            }
        }
        if has_dashdash {
            post.push(a);
            i += 1;
            continue;
        }
        // Long options that take a value, in `--opt=value` or `--opt value` form.
        // `--conflict` implies `-m`; the style only affects the deferred dirty
        // merge rendering, so here it is validated and otherwise ignored.
        if a == "--conflict" || a.starts_with("--conflict=") {
            let val = match a.strip_prefix("--conflict=") {
                Some(v) => v.to_string(),
                None => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("option `conflict' requires a value"))?;
                    i += 1;
                    v.clone()
                }
            };
            if !matches!(val.as_str(), "merge" | "diff3" | "zdiff3") {
                eprintln!("error: unknown style '{val}' given for '--conflict'");
                return Ok(ExitCode::from(129));
            }
            i += 1;
            continue;
        }
        // `--track=(direct|inherit)`: the optional-value form of `-t`/`--track`
        // (`git`'s `parse_opt_tracking_mode`). `direct` is the default explicit
        // tracking already implemented here; `inherit` needs upstream-inheritance
        // substrate that is not vendored, so it errors honestly rather than
        // silently behaving like `direct`. An unknown value is git's 129.
        if let Some(val) = a.strip_prefix("--track=") {
            match val {
                "direct" => track = true,
                "inherit" => bail!(
                    "--track=inherit is not supported (upstream-inheritance tracking not implemented)"
                ),
                _ => {
                    eprintln!("error: option `--track' expects \"direct\" or \"inherit\"");
                    return Ok(ExitCode::from(129));
                }
            }
            i += 1;
            continue;
        }
        if a == "--pathspec-from-file" || a.starts_with("--pathspec-from-file=") {
            let val = match a.strip_prefix("--pathspec-from-file=") {
                Some(v) => v.to_string(),
                None => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("option `pathspec-from-file' requires a value"))?;
                    i += 1;
                    v.clone()
                }
            };
            pathspec_from_file = Some(val);
            i += 1;
            continue;
        }
        match a {
            "--" => has_dashdash = true,
            "-b" | "-B" => {
                let name = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("option '{a}' requires a value"))?;
                new_branch = Some((name.clone(), a == "-B"));
                i += 1;
            }
            "--orphan" => {
                let Some(name) = args.get(i + 1) else {
                    // git: `error: option `orphan' requires a value`, exit 129.
                    eprintln!("error: option `orphan' requires a value");
                    return Ok(ExitCode::from(129));
                };
                orphan = Some(name.clone());
                i += 1;
            }
            // `-d` is git's short form of `--detach` (OPT_BOOL('d', "detach")).
            "-d" | "--detach" => detach = true,
            "--no-detach" => detach = false,
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            // git's `-f` sets `opts->discard_changes`, which routes the switch
            // through `reset_tree()` instead of the 2-way merge: local changes
            // are thrown away rather than carried or refused.
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            "-t" | "--track" => track = true,
            "--no-track" => {} // accepted; auto-tracking is off unless -t is given
            "--ours" | "-2" => writeout_stage = Some(2),
            "--theirs" | "-3" => writeout_stage = Some(3),
            // `-m` only changes behavior when local changes must be carried across
            // the switch; with a clean worktree it is byte-identical to a plain
            // checkout, so accept it and let the shared clean-check govern the
            // dirty case exactly as every other switch here does.
            "-m" | "--merge" => {}
            // Negation of -m: turns the 3-way carry off, which is already the only
            // behavior on the clean-switch path honored here, so it is a no-op.
            "--no-merge" => {}
            // `--no-conflict` clears the conflict style (git sets it to NULL); the
            // style is only consulted on the deferred dirty-merge path that is
            // refused here, so clearing it changes nothing.
            "--no-conflict" => {}
            "-l" => {} // create the branch reflog — always on here (RefLog::AndReference)
            "--guess" => guess_flag = Some(true),
            "--no-guess" => guess_flag = Some(false),
            "--overlay" => overlay = true,
            "--no-overlay" => overlay = false,
            "--pathspec-file-nul" => pathspec_file_nul = true,
            // Accepted no-ops: progress is discarded, ignored-file overwrite is the
            // default, and the other-worktree / skip-worktree checks git guards
            // against are not enforced here, so toggling them changes nothing.
            "--progress" | "--no-progress" => {}
            "--overwrite-ignore" | "--no-overwrite-ignore" => {}
            "--ignore-other-worktrees" | "--no-ignore-other-worktrees" => {}
            "--ignore-skip-worktree-bits" | "--no-ignore-skip-worktree-bits" => {}
            "-p" | "--patch" => patch_mode = true,
            "--no-patch" => patch_mode = false,
            "--recurse-submodules" => recurse_submodules = Some(true),
            "--no-recurse-submodules" => recurse_submodules = Some(false),
            // `--recurse-submodules=<pathspec>` limits which submodules move; this
            // port recurses into all active ones rather than honoring the pathspec.
            _ if a.starts_with("--recurse-submodules=") => recurse_submodules = Some(true),
            _ if a.starts_with('-') && a.len() > 1 => bail!("unsupported flag {a:?}"),
            _ => pre.push(a),
        }
        i += 1;
    }

    if let Err(code) = patch_opts.finish() {
        return Ok(code);
    }
    // git collects the hunk-selector options into `add_p_opt` and refuses them
    // right after parse-options, before any ref or pathspec is resolved — so a
    // `-U 3` alongside an unknown branch reports the option, not the branch
    // (verified against git 2.55.0).
    if let Some(code) = patch_opts.require_patch(patch_mode) {
        return Ok(code);
    }

    // `-p`: `git checkout -p [<tree-ish>] [--] [<pathspec>...]` selects hunks to
    // restore into BOTH the index and the worktree (git's `ADD_P_CHECKOUT`). The
    // exact patch mode depends on the source: the index when no tree-ish is
    // given, `HEAD` verbatim, and any other tree-ish resolved to its hex oid —
    // `checkout_paths()` does the same substitution because `diff-index` cannot
    // take an `<a>...<b>` range.
    if patch_mode {
        if pathspec_from_file.is_some() {
            eprintln!("fatal: options '--pathspec-from-file' and '--patch' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        // Without `--`, a leading positional is the tree-ish only when it
        // resolves as a revision; otherwise every positional is a pathspec.
        let (rev, specs): (Option<&str>, &[&str]) = if has_dashdash {
            match pre.len() {
                0 => (None, post.as_slice()),
                1 => (Some(pre[0]), post.as_slice()),
                _ => bail!("only one <tree-ish> may precede `--`"),
            }
        } else if !pre.is_empty() && repo.rev_parse_single(pre[0]).is_ok() {
            (Some(pre[0]), &pre[1..])
        } else {
            (None, pre.as_slice())
        };
        let revision = match rev {
            None | Some("HEAD") => rev.map(str::to_string),
            Some(r) => Some(repo.rev_parse_single(r)?.detach().to_string()),
        };
        let specs: Vec<String> = specs.iter().map(|s| s.to_string()).collect();
        return super::add_patch::run(
            &repo,
            super::add_patch::Mode::Checkout,
            revision.as_deref(),
            patch_opts.to_interactive(false),
            &specs,
        );
    }

    // Resolve submodule recursion: explicit flag wins, else `submodule.recurse`.
    let recurse_submodules = recurse_submodules
        .unwrap_or_else(|| repo.config_snapshot().boolean("submodule.recurse") == Some(true));

    // --- Dispatch -----------------------------------------------------------
    // `--pathspec-from-file`: pathspecs come from the file (or stdin for `-`),
    // never the command line. A single positional may still precede them as the
    // `<tree-ish>` source; anything else is git's incompatibility error.
    if let Some(file) = pathspec_from_file {
        if has_dashdash || !post.is_empty() {
            bail!("--pathspec-from-file is incompatible with pathspec arguments");
        }
        if new_branch.is_some() || orphan.is_some() || writeout_stage.is_some() {
            bail!("--pathspec-from-file cannot be combined with branch creation or --ours/--theirs");
        }
        let specs = read_pathspec_file(&file, pathspec_file_nul)?;
        let refs: Vec<&str> = specs.iter().map(String::as_str).collect();
        return match pre.len() {
            0 => restore_from_index(&repo, &refs, false, quiet),
            1 => restore_from_tree(&repo, pre[0], &refs, overlay, quiet),
            _ => bail!("only one <tree-ish> may precede pathspecs"),
        };
    }

    // `--orphan <name> [<start>]`: start an unborn branch off `<start>`'s tree.
    if let Some(name) = orphan {
        let start = pre.first().copied().unwrap_or("HEAD");
        return orphan_checkout(&repo, &name, start, quiet);
    }

    // `--ours`/`--theirs <path>…`: write one conflict side into the worktree.
    if let Some(stage) = writeout_stage {
        let paths = if has_dashdash { &post } else { &pre };
        if paths.is_empty() {
            eprintln!("fatal: '--ours/--theirs' needs the paths to check out");
            return Ok(ExitCode::from(128));
        }
        return restore_conflict_stage(&repo, paths, stage, !has_dashdash, quiet);
    }

    if let Some((name, reset)) = new_branch {
        if has_dashdash || !post.is_empty() {
            bail!("cannot combine branch creation (-b/-B) with path restore");
        }
        if pre.len() > 1 {
            bail!("too many start-points given for branch creation");
        }
        let start = pre.first().copied().unwrap_or("HEAD");
        // On an UNBORN HEAD (a fresh `git init`, or a clone of an empty repo)
        // there is no commit for the default start-point to resolve to, and git
        // still succeeds: `-b`/`-B` with no explicit start just re-points the
        // unborn HEAD at the new name. Resolving "HEAD" here instead would fail
        // with a rev-spec parse error and make `checkout -B main` unusable in an
        // empty repository.
        if pre.is_empty() && repo.head()?.is_unborn() {
            let full: gix::refs::FullName = format!("refs/heads/{name}").try_into()?;
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("checkout: moving to {name}").into(),
                    },
                    expected: PreviousValue::Any,
                    new: Target::Symbolic(full),
                },
                name: "HEAD".try_into()?,
                deref: false,
            })?;
            if !quiet {
                eprintln!("Switched to a new branch '{name}'");
            }
            return Ok(ExitCode::SUCCESS);
        }
        return create_and_switch(&repo, &name, reset, start, quiet, track);
    }

    // `-t <remote>/<branch>` with no `-b`: DWIM the local branch name from the
    // remote-tracking start-point, then create-and-track.
    if track {
        if pre.len() != 1 {
            eprintln!("fatal: missing branch name; try -b");
            return Ok(ExitCode::from(128));
        }
        match resolve_tracking(&repo, pre[0])? {
            Some(info) => {
                let Some(name) = info.dwim_name.clone() else {
                    // A local-branch start-point can't DWIM a new name.
                    eprintln!("fatal: missing branch name; try -b");
                    return Ok(ExitCode::from(128));
                };
                return create_and_switch(&repo, &name, false, pre[0], quiet, true);
            }
            None => {
                eprintln!("fatal: missing branch name; try -b");
                return Ok(ExitCode::from(128));
            }
        }
    }

    if has_dashdash {
        if post.is_empty() {
            bail!("you must specify path(s) to restore");
        }
        return match pre.len() {
            0 => restore_from_index(&repo, &post, false, quiet),
            1 => restore_from_tree(&repo, pre[0], &post, overlay, quiet),
            _ => bail!("only one <tree-ish> may precede `--`"),
        };
    }

    // No `--`, no -b/-B.
    if pre.is_empty() {
        // `git checkout --detach` with no revision detaches at the CURRENT HEAD:
        // git resolves the missing argument to HEAD rather than erroring
        // (builtin/checkout.c, `opts->force_detach && !argc`). The worktree is
        // already at that commit, so this is a ref-only move.
        if detach {
            let head = repo
                .head_id()
                .map_err(|_| anyhow::anyhow!("you are on a branch yet to be born"))?
                .detach();
            let commit = head.attach(&repo).object()?.peel_to_commit()?;
            let code = detached_checkout(&repo, "HEAD", commit, quiet, true, force)?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        // `git checkout` with neither a ref nor a pathspec is not an error:
        // `switch_branches()` names the missing branch "HEAD", takes the current
        // commit, and `update_refs_for_switch()` then does nothing to any ref.
        // Only the worktree reconciliation happens.
        let code = checkout_head_in_place(&repo, quiet, force)?;
        maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
        return Ok(code);
    }

    // Single positional: prefer ref interpretation (branch → switch; else rev →
    // detach); fall back to a bare path restore from the index.
    if pre.len() == 1 {
        let spec = pre[0];
        // `parse_branchname_arg()` resolves through `get_oid_mb()`, so
        // `get_oid_basic()`'s `core.warnAmbiguousRefs` warning is emitted here,
        // ahead of anything the checkout itself prints. `--quiet` does not
        // suppress it — stock warns under `git checkout -q` too.
        super::rev_parse::warn_ambiguous_refname(&repo, spec, false);
        // A revspec like `HEAD~3` is not a valid ref *name* (`~` is rejected by
        // ref validation), so treat a lookup error as "not a branch" and let the
        // `rev_parse_single` path below resolve and detach-checkout it.
        let is_branch = repo
            .try_find_reference(format!("refs/heads/{spec}").as_str())
            .ok()
            .flatten()
            .is_some();
        if is_branch && !detach {
            let code = switch_to_branch(&repo, spec, quiet, force)?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        // `git checkout HEAD` (and its `@` spelling) is not a detach: with no
        // `refs/heads/HEAD` to resolve, `new_branch_info->path` stays NULL while
        // its name is "HEAD", and `update_refs_for_switch()`'s first arm leaves
        // every ref alone. Only the worktree reconciliation runs.
        if !detach && matches!(spec, "HEAD" | "@") && !is_branch {
            let code = checkout_head_in_place(&repo, quiet, force)?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        if let Ok(id) = repo.rev_parse_single(spec) {
            let commit = id.object()?.peel_to_commit()?;
            let code = detached_checkout(&repo, spec, commit, quiet, detach, force)?;
            maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
            return Ok(code);
        }
        // DWIM (`--guess`, default on via `checkout.guess`): a bare name that is
        // not a local ref and does not resolve as a rev, but names a branch on
        // exactly one remote, becomes `-b <name> --track <remote>/<name>` — git's
        // `dwim_new_local_branch` path in `builtin/checkout.c`.
        if !detach {
            let guess = guess_flag.unwrap_or_else(|| {
                repo.config_snapshot().boolean("checkout.guess") != Some(false)
            });
            if guess {
                match unique_remote_branch(&repo, spec)? {
                    Dwim::One(remote_short) => {
                        let code =
                            create_and_switch(&repo, spec, false, &remote_short, quiet, true)?;
                        maybe_recurse_submodules(&repo, recurse_submodules, quiet)?;
                        return Ok(code);
                    }
                    Dwim::Many { count } => {
                        crate::advice::ambiguous_remote_branch_name(&repo, "checkout");
                        eprintln!(
                            "fatal: '{spec}' matched multiple ({count}) remote tracking branches"
                        );
                        return Ok(ExitCode::from(128));
                    }
                    Dwim::None => {}
                }
            }
        }
        // Not a ref/rev — treat as a path restore from the index (bare form).
        return restore_from_index(&repo, &pre, true, quiet);
    }

    // Multiple positionals, no `--`: if the first resolves to a tree-ish it is the
    // source and the rest are paths; otherwise all are paths from the index.
    if repo.rev_parse_single(pre[0]).is_ok() {
        return restore_from_tree(&repo, pre[0], &pre[1..], overlay, quiet);
    }
    restore_from_index(&repo, &pre, true, quiet)
}

/// After a switch that changed `HEAD`, move each active, initialized submodule's
/// worktree to the commit the superproject now records for it — git's
/// `--recurse-submodules` worktree updater (`submodule_move_head` in submodule.c).
///
/// Implemented by re-executing this binary's own `checkout <gitlink>` inside each
/// submodule, so the move stays faithful (dirty-worktree refusal, nested
/// recursion) without duplicating checkout logic. Uninitialized submodules (no
/// repo of their own) are skipped, exactly like git.
pub(super) fn maybe_recurse_submodules(
    repo: &gix::Repository,
    recurse: bool,
    quiet: bool,
) -> Result<()> {
    if !recurse {
        return Ok(());
    }
    // Re-open the repository so the index reflects the checkout that just ran: the
    // `repo` handed in was opened before the worktree/index update and its cached
    // index snapshot still records the OLD submodule gitlinks, which would make
    // `index_id()` match `head_id()` and skip every move.
    let repo = gix::open(repo.git_dir())?;
    let Some(subs) = repo.submodules()? else {
        return Ok(());
    };
    let Some(workdir) = repo.workdir() else {
        return Ok(());
    };
    let exe = std::env::current_exe()?;
    for sm in subs {
        if !sm.is_active().unwrap_or(false) {
            continue;
        }
        // The gitlink the just-checked-out superproject index records for this path
        // (both `index_id` and gix's `head_id` are the *superproject's* view — they
        // match after a checkout, so they can't tell us whether the submodule needs
        // moving; the submodule's actual worktree HEAD is what to compare against).
        let Ok(Some(target)) = sm.index_id() else {
            continue;
        };
        // Only recurse into an initialized submodule (one with its own repo).
        let Some(sub_repo) = sm.open().ok().flatten() else {
            continue;
        };
        // The submodule's ACTUAL checked-out commit. Already at the recorded gitlink
        // → nothing to move.
        let actual = sub_repo.head_id().ok().map(|id| id.detach());
        if actual == Some(target) {
            continue;
        }
        let Ok(rel) = sm.path() else {
            continue;
        };
        let sub_path = workdir.join(gix::path::from_bstr(rel.as_bstr()));
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("-C")
            .arg(&sub_path)
            .arg("checkout")
            .arg("--recurse-submodules");
        if quiet {
            cmd.arg("-q");
        }
        cmd.arg(target.to_string());
        let _ = cmd.status();
    }
    Ok(())
}

/// git's "nothing to do" ref case: `new_branch_info->name` is "HEAD" and there is
/// no branch path to move to, so `update_refs_for_switch()` touches no ref and
/// prints nothing. Reached by `git checkout` with no arguments and by
/// `git checkout HEAD`.
///
/// Only `merge_working_tree()` has an effect: forced, it resets the worktree and
/// index to `HEAD`'s tree; unforced, the 2-way merge against an identical tree is
/// a no-op and all that remains is the local-changes listing.
fn checkout_head_in_place(repo: &gix::Repository, quiet: bool, force: bool) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    if force {
        let tree = repo
            .head_tree_id()
            .map_err(|_| anyhow!("You are on a branch yet to be born"))?
            .detach();
        reset_worktree_to_tree(repo, tree)?;
    } else {
        show_local_changes(quiet)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Switch `HEAD` to an existing local branch `spec`, updating the worktree when
/// the target tree differs from the current one.
///
/// `force` is git's `opts->discard_changes`: it replaces the clean-worktree
/// requirement with `reset_tree()`, which is also why an already-current branch
/// still does work — `merge_working_tree()` runs before
/// `update_refs_for_switch()` decides there is no ref to move.
/// Shared with `rebase <upstream> <branch>`, which checks the branch out before
/// replaying onto it — quietly, since git's rebase prints no switch message.
pub(crate) fn switch_to_branch(
    repo: &gix::Repository,
    spec: &str,
    quiet: bool,
    force: bool,
) -> Result<ExitCode> {
    // Already on it → the branch `HEAD` points at does not change, but git still
    // goes through `refs_update_symref("HEAD", ...)`, so the move is reflogged
    // ("checkout: moving from main to main") before "Already on 'x'" is printed.
    if let Some(cur) = repo.head_name()? {
        if cur.shorten() == spec {
            let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
            let head_id = repo.head_id().ok().map(|id| id.detach());
            if force {
                let tree = repo.head_tree_id_or_empty()?.detach();
                reset_worktree_to_tree(repo, tree)?;
            }
            let branch_full: FullName = format!("refs/heads/{spec}")
                .try_into()
                .map_err(|e| anyhow!("invalid branch name '{spec}': {e}"))?;
            set_head_symbolic(
                repo,
                branch_full,
                &format!("checkout: moving from {spec} to {spec}"),
                head_id,
                head_id,
            )?;
            if !quiet {
                eprintln!("Already on '{spec}'");
            }
            return Ok(ExitCode::SUCCESS);
        }
    }

    let commit = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(&head);
    let cur_tree = repo.head_tree_id_or_empty()?.detach();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if force {
        reset_worktree_to_tree(repo, target_tree)?;
    } else if target_tree != cur_tree {
        if let Some(code) = ensure_clean(repo, cur_tree, target_tree)? {
            return Ok(code);
        }
        update_worktree_to_tree(repo, target_tree)?;
    }

    let branch_full: FullName = format!("refs/heads/{spec}")
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{spec}': {e}"))?;
    set_head_symbolic(
        repo,
        branch_full,
        &format!("checkout: moving from {old_label} to {spec}"),
        old_id,
        Some(commit.id),
    )?;

    if !quiet {
        // git only reports the abandoned detached position when it actually
        // moves (checkout.c: `!old->path && old->commit != new->commit`).
        if old_detached {
            if let Some(id) = old_id.filter(|id| *id != commit.id) {
                let (abbrev, summary) = describe(repo, id)?;
                eprintln!("Previous HEAD position was {abbrev} {summary}");
            }
        }
        eprintln!("Switched to branch '{spec}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// Detach `HEAD` at `commit`, updating the worktree when the target tree differs.
/// `force_detach` is true for an explicit `--detach`, which suppresses the
/// `advice.detachedHead` block just as git's `opts->force_detach` does.
fn detached_checkout(
    repo: &gix::Repository,
    spec: &str,
    commit: gix::Commit<'_>,
    quiet: bool,
    force_detach: bool,
    force: bool,
) -> Result<ExitCode> {
    let target_id = commit.id;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(&head);
    let cur_tree = repo.head_tree_id_or_empty()?.detach();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if force {
        reset_worktree_to_tree(repo, target_tree)?;
    } else if target_tree != cur_tree {
        if let Some(code) = ensure_clean(repo, cur_tree, target_tree)? {
            return Ok(code);
        }
        update_worktree_to_tree(repo, target_tree)?;
    }

    set_head_detached(
        repo,
        target_id,
        &format!("checkout: moving from {old_label} to {spec}"),
        old_id,
    )?;

    if !quiet {
        if old_detached {
            if let (Some(old), true) = (old_id, old_id != Some(target_id)) {
                let (abbrev, summary) = describe(repo, old)?;
                eprintln!("Previous HEAD position was {abbrev} {summary}");
            }
        } else if !force_detach
            && repo.config_snapshot().boolean("advice.detachedHead") != Some(false)
        {
            // Leaving an attached HEAD without an explicit --detach: git warns.
            print_detached_head_advice(spec);
        }
        let (abbrev, summary) = describe(repo, target_id)?;
        eprintln!("HEAD is now at {abbrev} {summary}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The `advice.detachedHead` block git prints when a bare `git checkout <commit>`
/// moves off a branch, verbatim (git 2.55.0, `builtin/checkout.c`).
fn print_detached_head_advice(spec: &str) {
    eprintln!("Note: switching to '{spec}'.\n");
    eprintln!(
        "You are in 'detached HEAD' state. You can look around, make experimental\n\
         changes and commit them, and you can discard any commits you make in this\n\
         state without impacting any branches by switching back to a branch.\n\
         \n\
         If you want to create a new branch to retain commits you create, you may\n\
         do so (now or later) by using -c with the switch command. Example:\n\
         \n\
         \x20 git switch -c <new-branch-name>\n\
         \n\
         Or undo this operation with:\n\
         \n\
         \x20 git switch -\n\
         \n\
         Turn off this advice by setting config variable advice.detachedHead to false\n"
    );
}

/// Create (`-b`) or create-or-reset (`-B`) `refs/heads/<name>` at `start`, then
/// switch `HEAD` to it, updating the worktree when the tree changes.
fn create_and_switch(
    repo: &gix::Repository,
    name: &str,
    reset: bool,
    start: &str,
    quiet: bool,
    track: bool,
) -> Result<ExitCode> {
    let full = format!("refs/heads/{name}");
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        bail!("'{name}' is not a valid branch name");
    }

    // `-t`: resolve the upstream before any mutation, so a bad start-point fails
    // exactly like git — branch untouched, HEAD unmoved.
    let track_info = if track {
        match resolve_tracking(repo, start)? {
            Some(info) => Some(info),
            None => {
                eprintln!(
                    "fatal: cannot set up tracking information; starting point '{start}' is not a branch"
                );
                return Ok(ExitCode::from(128));
            }
        }
    } else {
        None
    };

    let commit = repo.rev_parse_single(start)?.object()?.peel_to_commit()?;
    let start_id = commit.id;
    let target_tree = commit.tree_id()?.detach();

    let head = repo.head()?;
    let old_detached = head.is_detached();
    let old_id = head.id().map(|i| i.detach());
    let old_label = head_label(&head);
    // Whether HEAD is already attached to the branch we're (re)creating.
    let already_on = head
        .referent_name()
        .map(|n| n.shorten() == name)
        .unwrap_or(false);
    let cur_tree = repo.head_tree_id_or_empty()?.detach();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let existed = repo.try_find_reference(full.as_str())?.is_some();
    if existed && !reset {
        bail!("a branch named '{name}' already exists");
    }

    if target_tree != cur_tree {
        if let Some(code) = ensure_clean(repo, cur_tree, target_tree)? {
            return Ok(code);
        }
        update_worktree_to_tree(repo, target_tree)?;
    }

    let branch_full: FullName = full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{name}': {e}"))?;
    // Create fresh, or force-move an existing branch for -B.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("branch: Created from {start}").into(),
            },
            expected: if existed {
                PreviousValue::Any
            } else {
                PreviousValue::MustNotExist
            },
            new: Target::Object(start_id),
        },
        name: branch_full.clone(),
        deref: false,
    })?;
    set_head_symbolic(
        repo,
        branch_full,
        &format!("checkout: moving from {old_label} to {name}"),
        old_id,
        Some(start_id),
    )?;

    // `-t`: persist branch.<name>.remote / .merge (lock already held above; the
    // per-thread RepoLock is reentrant, so config.rs-style locking isn't needed).
    if let Some(info) = &track_info {
        write_tracking_config(repo, name, info)?;
    }

    if !quiet {
        // Reset-in-place (-B on the current branch) prints only "Reset branch".
        if existed && already_on {
            eprintln!("Reset branch '{name}'");
        } else {
            if old_detached {
                if let Some(id) = old_id.filter(|id| *id != start_id) {
                    let (abbrev, summary) = describe(repo, id)?;
                    eprintln!("Previous HEAD position was {abbrev} {summary}");
                }
            }
            if existed {
                eprintln!("Switched to and reset branch '{name}'");
            } else {
                eprintln!("Switched to a new branch '{name}'");
            }
        }
        // git prints the tracking confirmation to stdout, after the stderr
        // transition line, and only when not quiet.
        if let Some(info) = &track_info {
            println!("branch '{name}' set up to track '{}'.", info.display);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git checkout --orphan <name> [<start>]`: point `HEAD` at an unborn branch
/// `<name>` whose worktree/index come from `<start>`'s tree. The ref is not
/// created (git materializes it only at the first commit) and no reflog entry is
/// written, matching stock git.
fn orphan_checkout(
    repo: &gix::Repository,
    name: &str,
    start: &str,
    quiet: bool,
) -> Result<ExitCode> {
    // git resolves the start-point before anything else: a bad one aborts here.
    let commit = match repo
        .rev_parse_single(start)
        .ok()
        .and_then(|id| id.object().ok())
        .and_then(|o| o.peel_to_commit().ok())
    {
        Some(c) => c,
        None => {
            eprintln!(
                "fatal: '{start}' is not a commit and a branch '{name}' cannot be created from it"
            );
            return Ok(ExitCode::from(128));
        }
    };

    let full = format!("refs/heads/{name}");
    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        eprintln!("fatal: '{name}' is not a valid branch name");
        crate::advice::Advice::RefSyntax.advise_in(repo, "See 'git help check-ref-format'");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() {
        eprintln!("fatal: a branch named '{name}' already exists");
        return Ok(ExitCode::from(128));
    }

    let target_tree = commit.tree_id()?.detach();
    let cur_tree = repo.head_tree_id_or_empty()?.detach();
    if target_tree != cur_tree {
        if let Some(code) = ensure_clean(repo, cur_tree, target_tree)? {
            return Ok(code);
        }
        update_worktree_to_tree(repo, target_tree)?;
    }

    // Write HEAD as a plain symref to the (not-yet-existing) branch. No ref is
    // created and no reflog line is appended — git's exact orphan behavior.
    let head_path = repo.git_dir().join("HEAD");
    std::fs::write(&head_path, format!("ref: {full}\n"))?;

    if !quiet {
        eprintln!("Switched to a new branch '{name}'");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git checkout --ours|--theirs <path>…`: write one side of a conflict into the
/// worktree. `stage` is 2 (ours) or 3 (theirs). A non-conflicted path falls back
/// to its stage-0 blob; a conflicted path missing the requested side errors with
/// git's `path 'X' does not have our/their version` and exits 1. The index is
/// left untouched (a conflicted path stays conflicted).
fn restore_conflict_stage(
    repo: &gix::Repository,
    paths: &[&str],
    stage: u8,
    bare: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let index = repo.open_index()?;
    let matched = match_paths(&index, paths)?;
    let mset: HashSet<BString> = matched.iter().cloned().collect();

    // Which stages each matched path carries (index 0..=3).
    let mut have: HashMap<BString, [bool; 4]> = HashMap::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            let p = e.path_in(backing).to_owned();
            if mset.contains(&p) {
                let s = (e.stage_raw() as usize).min(3);
                have.entry(p).or_insert([false; 4])[s] = true;
            }
        }
    }

    let side = if stage == 2 { "our" } else { "their" };
    let mut keep: HashMap<BString, u32> = HashMap::new();
    let mut had_error = false;
    for p in &matched {
        let flags = have.get(p).copied().unwrap_or([false; 4]);
        let chosen = if flags[0] {
            Some(0u32) // not conflicted → the single indexed blob
        } else if flags[stage as usize] {
            Some(stage as u32)
        } else {
            None
        };
        match chosen {
            Some(st) => {
                keep.insert(p.clone(), st);
            }
            None => {
                let pb: &[u8] = p.as_ref();
                eprintln!(
                    "error: path '{}' does not have {side} version",
                    String::from_utf8_lossy(pb)
                );
                had_error = true;
            }
        }
    }

    if !keep.is_empty() {
        // Build a stage-0 view holding exactly the chosen entries and check it out;
        // the real index is never rewritten, so conflicts survive.
        let mut subset = repo.open_index()?;
        subset.remove_entries(|_, path, e| match keep.get(&path.to_owned()) {
            Some(&st) => e.stage_raw() != st,
            None => true,
        });
        for e in subset.entries_mut() {
            e.flags.remove(Flags::STAGE_MASK);
        }
        let should_interrupt = AtomicBool::new(false);
        checkout_subset(repo, &mut subset, &should_interrupt)?;
    }

    if had_error {
        return Ok(ExitCode::from(1));
    }
    if bare && !quiet {
        let n = keep.len();
        eprintln!(
            "Updated {n} path{} from the index",
            if n == 1 { "" } else { "s" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Upstream a `-t`/`--track` start-point resolves to.
struct TrackInfo {
    /// `branch.<name>.remote`: `"."` for a local start-point, else the remote name.
    remote: String,
    /// `branch.<name>.merge`, always `refs/heads/<branch>`.
    merge: String,
    /// Upstream short name shown in the "set up to track" line.
    display: String,
    /// For `-t` without `-b`: the local branch name DWIM'd from the start-point
    /// (`Some` only for a remote-tracking start; a local one can't DWIM a name).
    dwim_name: Option<String>,
}

/// Classify a `-t` start-point as a trackable branch. Returns `None` when it is
/// neither a local branch nor a remote-tracking branch of a configured remote —
/// the caller turns that into git's "is not a branch" / "missing branch name".
fn resolve_tracking(repo: &gix::Repository, start: &str) -> Result<Option<TrackInfo>> {
    if repo
        .try_find_reference(format!("refs/heads/{start}").as_str())?
        .is_some()
    {
        return Ok(Some(TrackInfo {
            remote: ".".into(),
            merge: format!("refs/heads/{start}"),
            display: start.into(),
            dwim_name: None,
        }));
    }
    if repo
        .try_find_reference(format!("refs/remotes/{start}").as_str())?
        .is_some()
    {
        // Remote names carry no '/', so the first component is the remote.
        if let Some((remote, rest)) = start.split_once('/') {
            if !rest.is_empty()
                && repo
                    .remote_names()
                    .iter()
                    .any(|n| n.to_str_lossy() == remote)
            {
                return Ok(Some(TrackInfo {
                    remote: remote.into(),
                    merge: format!("refs/heads/{rest}"),
                    display: start.into(),
                    dwim_name: Some(rest.into()),
                }));
            }
        }
    }
    Ok(None)
}

/// Persist `branch.<name>.remote` / `branch.<name>.merge` into the repo-local
/// config. The caller already holds the reentrant `RepoLock`.
fn write_tracking_config(repo: &gix::Repository, name: &str, info: &TrackInfo) -> Result<()> {
    let path = repo.common_dir().join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)?;
    file.set_raw_value_by("branch", Some(gix::bstr::BStr::new(name)), "remote", info.remote.as_str())?;
    file.set_raw_value_by("branch", Some(gix::bstr::BStr::new(name)), "merge", info.merge.as_str())?;
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// --- DWIM (`--guess`) ------------------------------------------------------

/// Result of resolving a bare `<name>` against the remote-tracking namespace.
enum Dwim {
    /// Exactly one remote has the branch; its short name (`<remote>/<name>`).
    One(String),
    /// More than one remote has it — ambiguous (unless `checkout.defaultRemote`).
    Many { count: usize },
    /// No remote has it.
    None,
}

/// Find the remote-tracking branch a bare `<name>` should DWIM to: `refs/remotes/
/// <remote>/<name>` across every configured remote. `checkout.defaultRemote`
/// disambiguates a multi-remote match. Mirrors `switch`'s identical resolver.
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

// --- `--pathspec-from-file` ------------------------------------------------

/// Read pathspecs from `file` (or stdin for `-`). With `nul`, entries are split
/// on NUL and taken verbatim; otherwise on newlines (trailing `\r` stripped,
/// blank lines skipped) and a C-quoted line (`"…"`) is unquoted — matching git's
/// `parse_pathspec_from_file` / `strbuf_getline` + `unquote_c_style`.
fn read_pathspec_file(file: &str, nul: bool) -> Result<Vec<String>> {
    let raw = if file == "-" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        buf
    } else {
        std::fs::read(file)?
    };

    if nul {
        return Ok(raw
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect());
    }

    let mut out = Vec::new();
    for line in raw.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let spec = if line.first() == Some(&b'"') {
            unquote_c_style(line)?
        } else {
            String::from_utf8_lossy(line).into_owned()
        };
        out.push(spec);
    }
    Ok(out)
}

/// Port of git's `unquote_c_style` (quote.c) for a double-quoted pathspec line:
/// `\a \b \f \n \r \t \v \\ \"` and up to three octal digits `\NNN`.
fn unquote_c_style(quoted: &[u8]) -> Result<String> {
    let mut out: Vec<u8> = Vec::with_capacity(quoted.len());
    let mut it = quoted[1..].iter().copied().peekable();
    while let Some(c) = it.next() {
        match c {
            b'"' => return Ok(String::from_utf8_lossy(&out).into_owned()),
            b'\\' => {
                let Some(e) = it.next() else {
                    bail!("unterminated quoted pathspec");
                };
                match e {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(0x0b),
                    b'\\' | b'"' => out.push(e),
                    b'0'..=b'7' => {
                        let mut val = (e - b'0') as u32;
                        for _ in 0..2 {
                            match it.peek() {
                                Some(&d @ b'0'..=b'7') => {
                                    val = val * 8 + (d - b'0') as u32;
                                    it.next();
                                }
                                _ => break,
                            }
                        }
                        out.push(val as u8);
                    }
                    _ => bail!("invalid escape in quoted pathspec"),
                }
            }
            _ => out.push(c),
        }
    }
    bail!("missing closing quote in pathspec")
}

/// Restore `paths` in the worktree from the current index (index left unchanged;
/// only stat info is refreshed). `bare` is true for the no-`--` pathspec form,
/// which prints git's "Updated N path(s) from the index" confirmation.
fn restore_from_index(
    repo: &gix::Repository,
    paths: &[&str],
    bare: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut index = repo.open_index()?;
    let matched = match_paths(&index, paths)?;

    let mut subset = repo.open_index()?;
    keep_only(&mut subset, &matched);
    let should_interrupt = AtomicBool::new(false);
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Refresh stat info in the real index for the restored paths so a later
    // status stays cheap; content ids are unchanged.
    let fresh = stats_by_path(&subset);
    for path in &matched {
        if let Ok(idx) = index.entry_index_by_path(BStr::new(path)) {
            if let Some((id, mode, stat)) = fresh.get(path) {
                let e = &mut index.entries_mut()[idx];
                e.id = *id;
                e.mode = *mode;
                e.stat = *stat;
            }
        }
    }
    index.remove_tree();
    index.write(Default::default())?;

    if bare && !quiet {
        let n = matched.len();
        eprintln!("Updated {n} path{} from the index", if n == 1 { "" } else { "s" });
    }
    Ok(ExitCode::SUCCESS)
}

/// Restore `paths` from `tree_ish` into both the index and the worktree
/// (matching stock `git checkout <tree-ish> -- <path>`). In overlay mode (the
/// default) paths absent from `tree_ish` are left untouched; with `overlay ==
/// false` a pathspec-matched path that exists in the current index but not in
/// `tree_ish` is deleted from both the worktree and the index, so the result
/// matches `tree_ish` exactly (git's `--no-overlay`).
fn restore_from_tree(
    repo: &gix::Repository,
    tree_ish: &str,
    paths: &[&str],
    overlay: bool,
    _quiet: bool,
) -> Result<ExitCode> {
    let tree_id = repo.rev_parse_single(tree_ish)?.object()?.peel_to_tree()?.id;

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let src = repo.index_from_tree(&tree_id)?;

    // Paths to write from the tree, and (no-overlay only) paths to delete.
    let (matched, to_remove) = if overlay {
        (match_paths(&src, paths)?, Vec::new())
    } else {
        let (tree_matched, tree_hit) = matches_in(&src, paths);
        let cur = repo.open_index()?;
        let (idx_matched, idx_hit) = matches_in(&cur, paths);
        // A pathspec must match in the tree or the index; else git's "did not match".
        if let Some(si) = (0..paths.len()).find(|&si| !tree_hit[si] && !idx_hit[si]) {
            bail!(
                "pathspec '{}' did not match any file(s) known to git",
                paths[si]
            );
        }
        let tset: HashSet<BString> = tree_matched.iter().cloned().collect();
        let rm: Vec<BString> = idx_matched.into_iter().filter(|p| !tset.contains(p)).collect();
        (tree_matched, rm)
    };

    let mut subset = repo.index_from_tree(&tree_id)?;
    keep_only(&mut subset, &matched);
    let should_interrupt = AtomicBool::new(false);
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Fold the tree's blobs (with fresh checkout stats) into the real index.
    let fresh = stats_by_path(&subset);
    let mut index = repo.open_index()?;
    let mut pushed = false;
    for path in &matched {
        let Some((id, mode, stat)) = fresh.get(path) else {
            continue;
        };
        match index.entry_index_by_path(BStr::new(path)) {
            Ok(idx) => {
                let e = &mut index.entries_mut()[idx];
                e.id = *id;
                e.mode = *mode;
                e.stat = *stat;
            }
            Err(_) => {
                index.dangerously_push_entry(
                    *stat,
                    *id,
                    gix::index::entry::Flags::empty(),
                    *mode,
                    BStr::new(path),
                );
                pushed = true;
            }
        }
    }

    // No-overlay: delete pathspec-matched paths that the tree does not carry.
    if !to_remove.is_empty() {
        for path in &to_remove {
            if let Some(full) = repo.workdir_path(BStr::new(path)) {
                let _ = std::fs::remove_file(full);
            }
        }
        let rmset: HashSet<BString> = to_remove.into_iter().collect();
        index.remove_entries(|_, path, _| rmset.contains(&path.to_owned()));
    }

    if pushed {
        index.sort_entries();
    }
    index.remove_tree();
    index.write(Default::default())?;

    Ok(ExitCode::SUCCESS)
}

// --- Worktree / ref helpers ------------------------------------------------

/// `unpack_trees()` with `twoway_merge` over `HEAD -> <target>`: refuse the
/// switch only for the paths it would actually write over.
///
/// git does not ask whether the worktree is clean — it asks, per path the two
/// trees disagree on, whether the file there still matches the index
/// (`verify_uptodate()`) or, for a path being added, whether something untracked
/// is in the way (`verify_absent()`). Everything else is carried across
/// untouched, which is why `git checkout <branch>` works with unrelated local
/// edits in the tree and reports them as `M <path>` afterwards.
///
/// Returns the exit code when the switch is refused; the paths and the advice
/// are printed by [`crate::merge_guard::Clobber::report`].
fn ensure_clean(repo: &gix::Repository, cur_tree: ObjectId, target_tree: ObjectId) -> Result<Option<ExitCode>> {
    let index = repo.index_or_load_from_head_or_empty()?;
    let clobber = crate::merge_guard::verify_two_way(repo, cur_tree, target_tree, &index)?;
    if clobber.is_empty() {
        return Ok(None);
    }
    clobber.report("checkout");
    Ok(Some(ExitCode::from(1)))
}

/// Move a clean worktree and its index from the current state to `new_tree`,
/// writing only the files that changed (added/modified checked out, removed
/// deleted). Mirrors the file-level reconciliation used by `zsync`.
fn update_worktree_to_tree(repo: &gix::Repository, new_tree: ObjectId) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    // Current tracked state (worktree == this when clean), with real stats.
    // `_or_empty` because the first checkout of a repository has neither: a
    // freshly `init`ed repo that has only fetched objects has no index file and
    // an unborn `HEAD`, and the plain `index_or_load_from_head` peels that
    // unborn `HEAD` and fails. git checks out into exactly that state — it is
    // how `git init && git fetch <url> <sha> && git checkout <sha>` works, the
    // sequence tree-sitter grammar fetchers use.
    let old = repo.index_or_load_from_head_or_empty()?.into_owned();
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // Full target index (the whole new tree) — what is finally written.
    let mut new_index = repo.index_from_tree(&new_tree)?;

    // Just the changed subset (added, or content/mode differs) — what is written
    // to disk.
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Delete files present in the old tree but not the new one.
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

    // Fresh stats for changed entries; reuse previous stats for unchanged ones.
    let subset_stats = stats_by_path(&subset);
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((_, _, stat)) = subset_stats.get(&path) {
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

/// `git checkout -f`'s worktree update: git's `reset_tree()`, i.e. a one-tree
/// `unpack_trees` run with `reset = UNPACK_RESET_OVERWRITE_UNTRACKED` and
/// `update = 1`, driven by `oneway_merge`.
///
/// The difference from [`update_worktree_to_tree`] is the whole point of `-f`:
/// every safety check `verify_uptodate()` performs is skipped (`o->reset` returns
/// early from it), so a modified, staged, conflicted or *deleted* worktree file is
/// rewritten from the tree instead of blocking the switch. `oneway_merge` decides
/// per entry: an entry the tree leaves alone is still written when
/// `lstat()` fails or `ie_match_stat()` reports a change, and an entry the tree
/// changes — including a conflicted one, which `same()` never matches — is always
/// written and lands at stage 0.
fn reset_worktree_to_tree(repo: &gix::Repository, new_tree: ObjectId) -> Result<()> {
    let should_interrupt = AtomicBool::new(false);

    let old = repo.index_or_load_from_head_or_empty()?.into_owned();
    let stat_ctx = super::read_tree::StatCtx::new(repo, &old)?;
    // Stage-0 entries only: a conflicted path has no stage-0 entry, so it never
    // matches `same(old, a)` and always ends up rewritten, which is what
    // `oneway_merge` does with a `CE_CONFLICTED` entry.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat, super::read_tree::Probe)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries().iter().filter(|e| e.stage_raw() == 0) {
            let path = e.path_in(backing);
            old_map.insert(
                path.to_owned(),
                (e.id, e.mode, e.stat, stat_ctx.probe(repo, e, path)),
            );
        }
    }

    let mut new_index = repo.index_from_tree(&new_tree)?;

    // What actually gets written: everything the tree changes, plus every
    // carried-forward entry whose worktree file is missing or stat-dirty.
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _, probe)) => {
            *oid == entry.id && *mode == entry.mode && *probe == super::read_tree::Probe::Uptodate
        }
        None => false,
    });
    checkout_subset(repo, &mut subset, &should_interrupt)?;

    // Paths the tree drops: `deleted_entry()` removes them from the worktree too
    // (`verify_uptodate` is a no-op under `--reset`).
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

    // "Take the stat information from stage0, take the data from stage1": an entry
    // that was not rewritten keeps the stat cache it already had.
    let subset_stats = stats_by_path(&subset);
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some((_, _, stat)) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat, _)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }
    new_index.remove_tree();
    new_index.write(Default::default())?;
    // `remove_branch_state()`: a forced switch abandons any in-progress merge,
    // cherry-pick or revert, exactly as git's `switch_branches` does after the
    // worktree is reconciled.
    remove_branch_state(repo);
    Ok(())
}

/// git's `remove_branch_state()` (branch.c): the merge/sequencer state files a
/// switch invalidates, plus the `CHERRY_PICK_HEAD` / `REVERT_HEAD` that
/// `sequencer_post_commit_cleanup()` drops on its way through.
///
/// `AUTO_MERGE` is on the list because `git merge` writes it for the conflicted
/// tree and nothing else removes it: a forced checkout out of a conflicted state
/// that leaves it behind makes the next `git diff` compare against a stale tree.
/// `MERGE_AUTOSTASH` is saved rather than deleted by git and is left alone here.
fn remove_branch_state(repo: &gix::Repository) {
    for name in [
        "MERGE_HEAD",
        "MERGE_RR",
        "MERGE_MSG",
        "MERGE_MODE",
        "AUTO_MERGE",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        let _ = std::fs::remove_file(repo.git_dir().join(name));
    }
}

/// git's `show_local_changes()`: the `diff-index --name-status` listing a
/// non-forced checkout prints to **stdout** when it does not discard changes.
fn show_local_changes(quiet: bool) -> Result<()> {
    if quiet {
        return Ok(());
    }
    let args = ["--name-status".to_string(), "HEAD".to_string()];
    super::diff_index::diff_index(&args)?;
    Ok(())
}

/// Check out the entries currently held in `index` into the worktree, overwriting
/// existing files (filters, mode and symlink handling applied by gitoxide).
fn checkout_subset(
    repo: &gix::Repository,
    index: &mut gix::index::File,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    crate::worktree::checkout_subset(
        index,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;
    Ok(())
}

/// Set `HEAD` to point symbolically at `branch` (attached), logging the move.
///
/// `from`/`to` are the object ids `HEAD` resolved to before and after; they exist
/// only for [`record_head_move`], which repairs the `.git/logs/HEAD` line the
/// vendored `gix-ref` cannot write correctly for a symbolic-target update.
fn set_head_symbolic(
    repo: &gix::Repository,
    branch: FullName,
    message: &str,
    from: Option<ObjectId>,
    to: Option<ObjectId>,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow!("invalid ref name HEAD: {e}"))?,
        deref: false,
    })?;
    record_head_move(repo, from, to, message);
    Ok(())
}

/// Make the last `.git/logs/HEAD` entry the one `refs_update_ref`/
/// `refs_update_symref` would have written for this move.
///
/// Two things the vendored `gix-ref` gets wrong for a `HEAD` update made with
/// `deref: false`, both visible in `git reflog`:
///
///  * a **symbolic** new target drops the reflog entry entirely
///    (`gix-ref/src/store/file/transaction/commit.rs`), so no line is written;
///  * `PreviousValue::Any` leaves the *old* field as the null id even when `HEAD`
///    resolved to a commit, because the previous value is a symref it does not
///    peel.
///
/// Rewriting the trailing line — or appending it when none was written — is
/// confined to the entry this command just made: the line is only replaced when
/// its message is the one passed in.
pub(super) fn record_head_move(
    repo: &gix::Repository,
    from: Option<ObjectId>,
    to: Option<ObjectId>,
    message: &str,
) {
    // `log_all_ref_updates` is on by default for a repository with a worktree,
    // and git creates `logs/HEAD` on the first update there.
    let path = repo.git_dir().join("logs").join("HEAD");
    if !path.exists()
        && (repo.workdir().is_none()
            || repo.config_snapshot().boolean("core.logAllRefUpdates") == Some(false))
    {
        return;
    }
    let null = ObjectId::null(repo.object_hash());
    let now = gix::date::Time::now_local_or_utc().format_or_unix(gix::date::time::Format::Raw);
    let sig = match repo.committer() {
        Some(Ok(sig)) => sig,
        _ => gix::actor::SignatureRef {
            name: b"zvcs".as_bstr(),
            email: b"zvcs@localhost".as_bstr(),
            time: &now,
        },
    };
    let line = format!(
        "{} {} {} <{}> {}\t{}\n",
        from.unwrap_or(null),
        to.unwrap_or(null),
        sig.name,
        sig.email,
        sig.time,
        message
    );

    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    let tail_is_ours = body
        .lines()
        .next_back()
        .is_some_and(|last| last.ends_with(&format!("\t{message}")));
    if tail_is_ours {
        let cut = body.trim_end_matches('\n').rfind('\n').map_or(0, |i| i + 1);
        body.truncate(cut);
    }
    body.push_str(&line);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, body);
}

/// Detach `HEAD` at object `id`, logging the move.
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
    record_head_move(repo, from, Some(id), message);
    Ok(())
}

/// Human label for the current `HEAD` used in reflog "moving from …" messages:
/// the short branch name, else the abbreviated detached hash, else "(unborn)".
fn head_label(head: &gix::Head<'_>) -> String {
    if let Some(name) = head.referent_name() {
        name.shorten().to_string()
    } else if let Some(id) = head.id() {
        id.shorten_or_id().to_string()
    } else {
        "(unborn)".to_string()
    }
}

/// Abbreviated hash + commit summary for `HEAD is now at …` / `Previous HEAD …`.
fn describe(repo: &gix::Repository, id: ObjectId) -> Result<(String, String)> {
    let abbrev = id.attach(repo).shorten_or_id().to_string();
    let commit = repo.find_object(id)?.peel_to_commit()?;
    let summary = commit.message()?.summary().into_owned().to_string();
    Ok((abbrev, summary))
}

// --- Pathspec / index helpers ----------------------------------------------

/// Collect the index entries (by path) matching every pathspec in `specs`.
/// Each spec must match at least one entry, else git's "did not match" error.
fn match_paths(index: &gix::index::File, specs: &[&str]) -> Result<Vec<BString>> {
    let (matched, hit) = matches_in(index, specs);
    if let Some(si) = hit.iter().position(|h| !h) {
        bail!(
            "pathspec '{}' did not match any file(s) known to git",
            specs[si]
        );
    }
    Ok(matched)
}

/// The entries of `index` matching any pathspec, plus a per-spec "did it match
/// anything" flag. Unlike [`match_paths`] this never fails, so callers that must
/// consider several indexes (e.g. no-overlay's tree ∪ index) can decide the
/// "did not match" error against their own union.
fn matches_in(index: &gix::index::File, specs: &[&str]) -> (Vec<BString>, Vec<bool>) {
    let mut matched: Vec<BString> = Vec::new();
    let mut seen: HashSet<BString> = HashSet::new();
    let mut hit = vec![false; specs.len()];

    let backing = index.path_backing();
    for e in index.entries() {
        let path = e.path_in(backing);
        let bytes: &[u8] = path.as_ref();
        for (si, spec) in specs.iter().enumerate() {
            if spec_matches(bytes, spec) {
                hit[si] = true;
                let owned = path.to_owned();
                if seen.insert(owned.clone()) {
                    matched.push(owned);
                }
            }
        }
    }
    (matched, hit)
}

/// A pathspec matches a file path when it is `.`/empty (all), equals the path,
/// or is a directory prefix of it.
fn spec_matches(path: &[u8], spec: &str) -> bool {
    let s = spec.as_bytes();
    if s.is_empty() || spec == "." {
        return true;
    }
    if path == s {
        return true;
    }
    path.len() > s.len() && path.starts_with(s) && path[s.len()] == b'/'
}

/// Reduce `index` to only the entries whose path is in `keep`.
fn keep_only(index: &mut gix::index::File, keep: &[BString]) {
    index.remove_entries(|_, path, _| !keep.iter().any(|k| BStr::new(k) == path));
}

/// Map path → (id, mode, stat) for every entry of `index` (post-checkout stats).
fn stats_by_path(index: &gix::index::File) -> HashMap<BString, (ObjectId, Mode, Stat)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode, e.stat)))
        .collect()
}
