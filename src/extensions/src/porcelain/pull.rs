//! `git pull` — fetch the configured (or named) remote, then integrate the
//! fetched upstream into the current branch.
//!
//! `pull` is `fetch` followed by an integration step. The fetch is delegated to
//! the ported [`fetch`](super::fetch), so its option surface is available to
//! `pull` verbatim: `--all`, `-f`/`--force`, `-t`/`--tags`, `-p`/`--prune`,
//! `--depth`/`--deepen`/`--unshallow`, `--shallow-since`/`--shallow-exclude`,
//! `-q`/`-v`, `--progress`/`--no-progress`, `--dry-run`, `-a`/`--append`,
//! `--show-forced-updates`/`--no-show-forced-updates`,
//! `-k`/`--keep`, `--recurse-submodules`, `-j`/`--jobs`, `--upload-pack`,
//! `-o`/`--server-option`, `--refmap`, `-4`/`--ipv4`, `-6`/`--ipv6`,
//! `--negotiation-restrict`/`--negotiation-tip`/`--negotiation-include`, and the
//! `From …` per-ref summary git prints to stderr.
//!
//! The integration step is the ported [`merge`](super::merge) by default, or the
//! ported [`rebase`](super::rebase) when a rebase is requested. The rebase is
//! selected the way git's `config_get_rebase()` selects it: a CLI
//! `--rebase[=<value>]`/`-r`/`--no-rebase` wins, else `branch.<name>.rebase`,
//! else `pull.rebase`; `<value>` is `true`/`false`/`merges`/`interactive`
//! (`preserve` is refused as git refuses it). A rebase forwards its compatible
//! knobs to `rebase` — `-s`/`--strategy`, `-X`/`--strategy-option`, `--signoff`,
//! `--autostash` (and `pull.autoStash`/`rebase.autoStash`), `--rebase-merges` (from
//! `--rebase=merges`), `--stat`/`--no-stat`/`-n` — and rebases the current branch
//! onto the fetched upstream. A rebase whose upstream is already a descendant of
//! `HEAD` would replay nothing, so — as git's `can_ff`/`ran_ff` do — the merge runs
//! with a forced `--ff-only` instead and the rebase never starts.
//!
//! Supported invocation forms:
//!   * `git pull`                  — use the current branch's configured upstream.
//!   * `git pull <remote>`         — fetch `<remote>`, merge the configured upstream branch.
//!   * `git pull <remote> <branch>`— fetch `<remote>`, merge `refs/remotes/<remote>/<branch>`.
//!
//! Fast-forward policy (merge path only): `pull.ff` (`true`/`false`/`only`) is
//! the default, overriding `merge.ff` for `pull` as git's `config_get_ff()`
//! does; a CLI `--ff`/`--no-ff`/`--ff-only` overrides both, and when neither is
//! set the decision falls through to `merge.ff` inside [`merge`](super::merge).
//!
//! What is refused rather than faked, because the underlying substrate is
//! absent: the merge-only integration options the merge port does not implement
//! (`-s`/`-X`/`--squash`/`--commit`/`--no-commit`/`--edit`/`--cleanup`/`--log`/
//! `--signoff`/`--allow-unrelated-histories` on the *merge* path — `-s`/`-X`/
//! `--signoff` are honored on the *rebase* path), `--rebase=interactive`
//! (interactive todo editing needs a TTY editor loop), `--autostash` over a
//! dirty tree on the merge path (needs a 3-way stash apply the stash port lacks),
//! `--set-upstream`, and
//! `--gpg-sign`/`-S`/`--verify-signatures` (GPG is not vendored).
//!
//! The diffstat selectors `-n`, `--stat`/`--no-stat`, `--summary`/`--no-summary`
//! and `--compact-summary`/`--no-compact-summary` are git's `OPT_PASSTHRU`
//! `opt_diffstat` — a single slot, so the last one wins, and the flag git
//! reconstructed from it is handed verbatim to the integration step
//! (`run_merge()`/`run_rebase()` both push `opt_diffstat`). `git rebase` has no
//! `--summary` or `--compact-summary`, so those combined with `--rebase` fail in
//! the rebase step exactly as they do for git.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::remote::Direction;

/// Which integration step `pull` runs after the fetch, mirroring git's
/// `enum rebase_type` as selected by `config_get_rebase()`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RebaseMode {
    Disabled,
    Plain,
    Merges,
    Interactive,
}

/// Parse a `--rebase=<value>` / `pull.rebase` / `branch.<name>.rebase` value the
/// way git's `rebase_parse_value()` does.
fn parse_rebase_value(v: &str) -> Result<RebaseMode> {
    match v.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" | "1" => Ok(RebaseMode::Plain),
        "false" | "no" | "off" | "0" => Ok(RebaseMode::Disabled),
        "merges" => Ok(RebaseMode::Merges),
        "interactive" => Ok(RebaseMode::Interactive),
        "preserve" => bail!(
            "preserve is no longer supported (--rebase=preserve / pull.rebase=preserve); use 'merges' instead"
        ),
        other => bail!("Invalid value for rebase: '{other}'"),
    }
}

/// Resolve the configured rebase policy: `branch.<name>.rebase` overrides
/// `pull.rebase`, and an unset key means a merge.
fn config_rebase(repo: &gix::Repository, branch: Option<&str>) -> Result<RebaseMode> {
    let snap = repo.config_snapshot();
    let raw = branch
        .and_then(|b| snap.string(&format!("branch.{b}.rebase")))
        .or_else(|| snap.string("pull.rebase"));
    match raw.map(|v| v.to_string()) {
        None => Ok(RebaseMode::Disabled),
        Some(v) => parse_rebase_value(&v),
    }
}

pub fn pull(args: &[String]) -> Result<ExitCode> {
    // ---- parse -----------------------------------------------------------
    let mut positionals: Vec<&str> = Vec::new();

    // Fast-forward flag a CLI option selects (merge path); forwarded to `merge`
    // to override both pull.ff and merge.ff. `None` until seen.
    let mut ff_cli: Option<&'static str> = None;

    // Integration selection (CLI). `None` falls through to config.
    let mut rebase_cli: Option<RebaseMode> = None;

    // Integration knobs forwarded to `merge`/`rebase`.
    // git's `opt_diffstat` is a single `OPT_PASSTHRU` slot shared by `-n`,
    // `--stat`/`--no-stat`, `--summary`/`--no-summary` and
    // `--compact-summary`/`--no-compact-summary`: the last occurrence wins and
    // `recreate_opt()` re-renders it, so the literal below is what reaches the
    // integration step.
    let mut diffstat: Option<&'static str> = None;
    let mut strategy: Option<String> = None; // -s / --strategy
    let mut strategy_opts: Vec<String> = Vec::new(); // -X / --strategy-option
    let mut signoff = false;
    let mut autostash: Option<bool> = None;
    // `--verify` (git's default) / `--no-verify`: forwarded to `merge`, which
    // runs the `pre-merge-commit` and `commit-msg` hooks. git's pull passes it
    // only to the merge, never to the rebase.
    let mut no_verify = false;

    // Knobs forwarded to `fetch`.
    let mut f_all = false;
    let mut f_force = false;
    let mut f_tags = false;
    let mut f_prune = false;
    let mut f_unshallow = false;
    let mut f_update_shallow = false;
    let mut f_depth: Option<String> = None;
    let mut f_deepen: Option<String> = None;
    let mut f_shallow_since: Option<String> = None;
    let mut f_shallow_exclude: Vec<String> = Vec::new();
    let mut f_quiet = false;
    let mut f_verbose = false;
    // Fetch knobs with no merge/rebase meaning, forwarded verbatim.
    let mut f_progress: Option<bool> = None;
    let mut f_dry_run = false;
    let mut f_append = false;
    let mut f_keep = false;
    let mut f_show_forced: Option<bool> = None;
    let mut f_recurse: Option<String> = None;
    let mut f_jobs: Option<String> = None;
    let mut f_upload_pack: Option<String> = None;
    let mut f_server_options: Vec<String> = Vec::new();
    let mut f_refmap: Vec<String> = Vec::new();
    // `--negotiation-restrict`/`--negotiation-tip`/`--negotiation-include`, forwarded verbatim so the
    // fetch resolves them against the same repository.
    let mut f_negotiation_restrict: Vec<String> = Vec::new();
    let mut f_negotiation_include: Vec<String> = Vec::new();
    // `-4`/`--ipv4` and `-6`/`--ipv6`, forwarded to the fetch as git's pull does.
    let mut f_address_family: Option<&'static str> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        // Split `--opt=value` for the value-taking long options.
        let (key, inline) = match (a.starts_with("--"), a.split_once('=')) {
            (true, Some((k, v))) => (k, Some(v.to_string())),
            _ => (a, None),
        };

        // Value for a value-taking option: inline `=v` or the next argv entry.
        macro_rules! take_value {
            ($name:literal) => {
                match inline.clone() {
                    Some(v) => v,
                    None => {
                        let v = args.get(i).cloned().ok_or_else(|| {
                            anyhow::anyhow!(concat!("option `", $name, "' requires a value"))
                        })?;
                        i += 1;
                        v
                    }
                }
            };
        }

        match key {
            // Fast-forward policy (merge path).
            "--ff" => ff_cli = Some("--ff"),
            "--ff-only" => ff_cli = Some("--ff-only"),
            "--no-ff" => ff_cli = Some("--no-ff"),

            // Rebase selection.
            "--rebase" => {
                rebase_cli = Some(match inline.as_deref() {
                    None => RebaseMode::Plain,
                    Some(v) => parse_rebase_value(v)?,
                });
            }
            "--no-rebase" => rebase_cli = Some(RebaseMode::Disabled),
            "-r" => rebase_cli = Some(RebaseMode::Plain),

            // Integration knobs forwarded to merge/rebase.
            "--stat" => diffstat = Some("--stat"),
            "--no-stat" => diffstat = Some("--no-stat"),
            "--summary" => diffstat = Some("--summary"),
            "--no-summary" => diffstat = Some("--no-summary"),
            "-n" => diffstat = Some("-n"),
            "--compact-summary" => diffstat = Some("--compact-summary"),
            "--no-compact-summary" => diffstat = Some("--no-compact-summary"),
            "-s" | "--strategy" => strategy = Some(take_value!("strategy")),
            "-X" | "--strategy-option" => strategy_opts.push(take_value!("strategy-option")),
            "--signoff" => signoff = true,
            "--no-signoff" => signoff = false,
            "--autostash" => autostash = Some(true),
            "--no-autostash" => autostash = Some(false),

            // Fetch knobs forwarded to super::fetch.
            "--all" => f_all = true,
            "-f" | "--force" => f_force = true,
            "-t" | "--tags" => f_tags = true,
            "-p" | "--prune" => f_prune = true,
            "--unshallow" => f_unshallow = true,
            "--update-shallow" => f_update_shallow = true,
            "--no-update-shallow" => f_update_shallow = false,
            "--depth" => f_depth = Some(take_value!("depth")),
            "--deepen" => f_deepen = Some(take_value!("deepen")),
            "--shallow-since" => f_shallow_since = Some(take_value!("shallow-since")),
            "--shallow-exclude" => f_shallow_exclude.push(take_value!("shallow-exclude")),
            "-q" | "--quiet" => f_quiet = true,
            "-v" | "--verbose" => f_verbose = true,
            "--progress" => f_progress = Some(true),
            "--no-progress" => f_progress = Some(false),
            // git's pull runs the fetch with `--dry-run` and then returns
            // without integrating anything (`builtin/pull.c` returns right after
            // `run_fetch`), so nothing is merged or rebased.
            "--dry-run" => f_dry_run = true,
            "--no-dry-run" => f_dry_run = false,
            "-a" | "--append" => f_append = true,
            "--no-append" => f_append = false,
            "-k" | "--keep" => f_keep = true,
            "--show-forced-updates" => f_show_forced = Some(true),
            "--no-show-forced-updates" => f_show_forced = Some(false),
            "--recurse-submodules" => {
                f_recurse = Some(inline.clone().unwrap_or_else(|| "yes".into()))
            }
            "--no-recurse-submodules" => f_recurse = Some("no".into()),
            // git's pull declares `-j`/`--jobs` with an *optional* value, so a
            // detached `5` in `git pull -j 5` is a positional, not the count.
            // Only an attached value sets it.
            "-j" | "--jobs" => f_jobs = inline.clone(),
            "--no-jobs" => f_jobs = None,
            "--upload-pack" => f_upload_pack = Some(take_value!("upload-pack")),
            "-o" | "--server-option" => f_server_options.push(take_value!("server-option")),
            "--refmap" => f_refmap.push(take_value!("refmap")),
            "--negotiation-restrict" | "--negotiation-tip" => {
                f_negotiation_restrict.push(take_value!("negotiation-restrict"));
            }
            "--negotiation-include" => f_negotiation_include.push(take_value!("negotiation-include")),
            "-4" | "--ipv4" => f_address_family = Some("--ipv4"),
            "-6" | "--ipv6" => f_address_family = Some("--ipv6"),

            // `--verify` (default) / `--no-verify` reach the merge, which runs
            // the `pre-merge-commit` and `commit-msg` hooks.
            "--verify" => no_verify = false,
            "--no-verify" => no_verify = true,

            // Merge-only integration options the merge port does not implement,
            // with no rebase equivalent: refused rather than faked.
            "--squash" | "--no-squash" => {
                bail!("--squash is not supported (the merge port has no squash-merge path)")
            }
            "--commit" | "--no-commit" => {
                bail!("--commit/--no-commit is not supported (the merge port always commits)")
            }
            "--edit" | "-e" | "--no-edit" => {
                bail!("--edit is not supported (editing the merge message needs a TTY editor loop)")
            }
            "--cleanup" => {
                // `i` bump inside take_value! is dead here because we bail immediately;
                // the value is still consumed so a missing one errors identically to git.
                #[allow(unused_assignments)]
                let _ = take_value!("cleanup");
                bail!("--cleanup is not supported (the merge port does not run message cleanup)")
            }
            "--log" | "--no-log" => {
                bail!("--log is not supported (the merge port does not append a shortlog)")
            }
            "--allow-unrelated-histories" => bail!(
                "--allow-unrelated-histories is not supported (the merge port requires a common ancestor)"
            ),

            // Absent substrate.
            "--set-upstream" => {
                bail!("--set-upstream is not supported (not exposed by the high-level fetch)")
            }
            "-S" | "--gpg-sign" => bail!("--gpg-sign is not supported (GPG is not vendored)"),
            "--verify-signatures" | "--no-verify-signatures" => {
                bail!("--verify-signatures is not supported (GPG is not vendored)")
            }

            "--" => {
                positionals.extend(args[i..].iter().map(String::as_str));
                break;
            }
            // Attached short-option values git's parse-options accepts, e.g.
            // `-Xtheirs` / `-sort`.
            other if other.starts_with("-X") && other.len() > 2 => {
                strategy_opts.push(other[2..].to_string())
            }
            other if other.starts_with("-s") && other.len() > 2 => {
                strategy = Some(other[2..].to_string())
            }
            other if other.starts_with('-') && other != "-" => bail!("unsupported flag {other}"),
            other => positionals.push(other),
        }
    }

    let repo = gix::discover(".")?;
    let head_name = repo.head_name()?;
    let branch_short = head_name.as_ref().map(|h| h.shorten().to_string());

    // Resolve the integration policy git's `config_get_rebase()` computes: a CLI
    // flag wins, else branch.<name>.rebase / pull.rebase.
    let rebase_mode = match rebase_cli {
        Some(m) => m,
        None => config_rebase(&repo, branch_short.as_deref())?,
    };
    let rebasing = rebase_mode != RebaseMode::Disabled;

    // Resolve which remote-tracking ref the fetched upstream lands at.
    // `<repository>` may just as well be a URL, and then there is no `remote.<name>` section and
    // nothing under `refs/remotes/` for the fetch to have updated. git never depends on one:
    // `cmd_pull()` collects its merge heads from `FETCH_HEAD`, which the fetch has just written for
    // exactly the refs that were asked for. Only a configured remote gets the tracking-ref
    // treatment, because that is the one whose name a tracking ref can be built from.
    let named_remote = positionals
        .first()
        .is_some_and(|name| repo.remote_names().iter().any(|known| known == name));
    let target_ref = if positionals.len() >= 2 && named_remote {
        // Explicit `<remote> <branch>`: after a default-refspec fetch the branch
        // lands at refs/remotes/<remote>/<branch>.
        format!("refs/remotes/{}/{}", positionals[0], positionals[1])
    } else if positionals.len() >= 2 {
        "FETCH_HEAD".to_string()
    } else {
        // No explicit branch: derive the tracking ref from the current branch's
        // upstream configuration (branch.<name>.remote / .merge).
        let head = head_name.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "You are not currently on a branch. Please specify which branch to pull."
            )
        })?;
        match repo.branch_remote_tracking_ref_name(head.as_ref(), Direction::Fetch) {
            Some(Ok(name)) => name.as_bstr().to_string(),
            Some(Err(err)) => return Err(err.into()),
            None => bail!("There is no tracking information for the current branch."),
        }
    };

    // ---- phase 1: fetch --------------------------------------------------
    // Delegate to the ported fetch, which acquires the repo lock itself, prints
    // the git-style `From …` per-ref summary, and honors the forwarded knobs.
    let mut fetch_args: Vec<String> = Vec::new();
    if f_all {
        fetch_args.push("--all".into());
    }
    if f_force {
        fetch_args.push("--force".into());
    }
    if f_tags {
        fetch_args.push("--tags".into());
    }
    if f_prune {
        fetch_args.push("--prune".into());
    }
    if f_update_shallow {
        fetch_args.push("--update-shallow".into());
    }
    if f_unshallow {
        fetch_args.push("--unshallow".into());
    }
    if let Some(d) = &f_depth {
        fetch_args.push("--depth".into());
        fetch_args.push(d.clone());
    }
    if let Some(d) = &f_deepen {
        fetch_args.push("--deepen".into());
        fetch_args.push(d.clone());
    }
    if let Some(t) = &f_shallow_since {
        fetch_args.push("--shallow-since".into());
        fetch_args.push(t.clone());
    }
    for r in &f_shallow_exclude {
        fetch_args.push("--shallow-exclude".into());
        fetch_args.push(r.clone());
    }
    if f_quiet {
        fetch_args.push("--quiet".into());
    }
    if f_verbose {
        fetch_args.push("--verbose".into());
    }
    match f_progress {
        Some(true) => fetch_args.push("--progress".into()),
        Some(false) => fetch_args.push("--no-progress".into()),
        None => {}
    }
    if f_dry_run {
        fetch_args.push("--dry-run".into());
    }
    if f_append {
        fetch_args.push("--append".into());
    }
    if f_keep {
        fetch_args.push("--keep".into());
    }
    match f_show_forced {
        Some(true) => fetch_args.push("--show-forced-updates".into()),
        Some(false) => fetch_args.push("--no-show-forced-updates".into()),
        None => {}
    }
    if let Some(r) = &f_recurse {
        fetch_args.push(format!("--recurse-submodules={r}"));
    }
    if let Some(j) = &f_jobs {
        fetch_args.push(format!("--jobs={j}"));
    }
    if let Some(p) = &f_upload_pack {
        fetch_args.push(format!("--upload-pack={p}"));
    }
    for o in &f_server_options {
        fetch_args.push(format!("--server-option={o}"));
    }
    for r in &f_refmap {
        fetch_args.push(format!("--refmap={r}"));
    }
    for t in &f_negotiation_restrict {
        fetch_args.push(format!("--negotiation-restrict={t}"));
    }
    for t in &f_negotiation_include {
        fetch_args.push(format!("--negotiation-include={t}"));
    }
    if let Some(f) = f_address_family {
        fetch_args.push(f.into());
    }
    // `--all` fans out over every configured remote and takes no repository
    // argument; otherwise git hands the whole `<remote> [<refspec>…]` tail to the
    // fetch (`run_fetch()` in `builtin/pull.c`). The refspecs select what is
    // downloaded, and the remote's configured refspecs still update the tracking
    // ref that the merge or rebase below reads, via the opportunistic second stage.
    if !f_all {
        fetch_args.extend(positionals.iter().map(|p| (*p).to_string()));
    }
    // Network / bad-remote failures surface as `Err`; a ref-rejection returns a
    // non-success code with the summary already printed. The tracking-ref check
    // below then reports the missing upstream, as git's pull does.
    let fetch_code = super::fetch(&fetch_args)?;

    // `--dry-run` stops here: git's pull returns right after `run_fetch` without
    // touching the worktree, so no merge or rebase is attempted and the tracking
    // refs the integration step would need were never written.
    if f_dry_run {
        return Ok(fetch_code);
    }

    // `cmd_pull()` is `if (run_fetch(...)) return 1;` - a fetch that failed ends the pull right
    // there, with 1 whatever the fetch itself exited with, and no integration step is attempted.
    if fetch_code != ExitCode::SUCCESS {
        return Ok(ExitCode::FAILURE);
    }

    // The upstream ref must now exist locally; if the fetch produced no such
    // tracking ref the requested branch does not exist on the remote.
    // `FETCH_HEAD` is a file of candidate lines rather than a reference, so it is looked up the way
    // `parse_fetch()` reaches it - by resolving the name.
    if repo.try_find_reference(target_ref.as_str())?.is_none()
        && repo.rev_parse_single(target_ref.as_str()).is_err()
    {
        bail!("couldn't find remote ref {target_ref}");
    }

    // ---- phase 2: integrate ----------------------------------------------

    // Nothing was fetched that we do not already have: git reports this as the
    // PULL being up to date and never starts the integration step, so the line
    // is `Already up to date.` — not the rebase's own `Current branch <b> is up
    // to date.`, which git prints only when the branch has commits the upstream
    // lacks (the rebase runs and finds nothing to replay). Both cases are
    // exercised against stock git in tests/pull_up_to_date.rs.
    if let (Ok(head), Ok(upstream)) = (
        repo.head_id().map(|id| id.detach()),
        repo.rev_parse_single(target_ref.as_str()).map(|id| id.detach()),
    ) {
        if head == upstream {
            println!("Already up to date.");
            return Ok(ExitCode::SUCCESS);
        }
    }

    // `builtin/pull.c`'s `can_ff`: `get_can_ff()` asks whether the fetched head is a
    // descendant of `HEAD`. When it is, a rebase would replay nothing, so git forces
    // `opt_ff = "--ff-only"` and runs the *merge* (`ran_ff`), never starting the rebase.
    let can_ff = match (
        repo.head_id().map(|id| id.detach()),
        repo.rev_parse_single(target_ref.as_str()).map(|id| id.detach()),
    ) {
        (Ok(head), Ok(upstream)) => repo
            .merge_base(upstream, head)
            .map(|base| base.detach() == head)
            .unwrap_or(false),
        _ => false,
    };

    if rebasing && !can_ff {
        if rebase_mode == RebaseMode::Interactive {
            bail!(
                "--rebase=interactive is not supported (interactive todo editing needs a TTY editor loop)"
            );
        }

        // Rebase the current branch onto the fetched upstream, forwarding the
        // knobs the ported rebase accepts.
        let mut rebase_args: Vec<String> = Vec::new();
        if rebase_mode == RebaseMode::Merges {
            rebase_args.push("--rebase-merges".into());
        }
        if let Some(s) = &strategy {
            rebase_args.push("--strategy".into());
            rebase_args.push(s.clone());
        }
        for x in &strategy_opts {
            rebase_args.push("--strategy-option".into());
            rebase_args.push(x.clone());
        }
        if signoff {
            rebase_args.push("--signoff".into());
        }
        // Autostash: CLI flag wins, else pull.autoStash — which overrides
        // rebase.autoStash for pull the way git's config_autostash resolution
        // does — else rebase.autoStash, else off. A clean tree makes it a no-op;
        // a dirty tree is handled by the rebase port's own policy.
        let want_autostash = match autostash {
            Some(v) => v,
            None => {
                let snap = repo.config_snapshot();
                snap.boolean("pull.autoStash")
                    .or_else(|| snap.boolean("rebase.autoStash"))
                    == Some(true)
            }
        };
        if want_autostash {
            rebase_args.push("--autostash".into());
        }
        // `run_rebase()` pushes `opt_diffstat` verbatim. `git rebase` knows only
        // `-n`/`--stat`/`--no-stat`, so `--summary`/`--compact-summary` reach it
        // as unknown options and fail there, as they do for git.
        if let Some(d) = diffstat {
            rebase_args.push(d.to_string());
        }
        rebase_args.push(target_ref);
        return super::rebase(&rebase_args);
    }

    // Merge path. Integration knobs the merge port does not implement cannot be
    // forwarded; refuse rather than silently drop them.
    if strategy.is_some() || !strategy_opts.is_empty() || signoff {
        bail!(
            "-s/--strategy, -X/--strategy-option and --signoff are not supported on the merge path \
             (the merge port implements only the 'ort' strategy with no strategy options or sign-off)"
        );
    }
    // `--autostash` over a dirty tree needs a 3-way stash apply the stash port
    // cannot do; a clean tree makes it a no-op, so only the dirty case refuses.
    if autostash == Some(true) && repo.is_dirty()? {
        bail!(
            "--autostash over a dirty tree is not supported on the merge path \
             (re-applying the stash over the merged worktree needs a 3-way stash apply)"
        );
    }

    // Resolve the fast-forward policy git's `config_get_ff()` computes for pull:
    // a CLI flag wins; else pull.ff (which overrides merge.ff) is forwarded to
    // `merge`; else nothing is forwarded and `merge` reads merge.ff itself.
    let ff_flag: Option<&str> = match ff_cli {
        // The `can_ff` short-circuit above overrides everything, as git's assignment to
        // `opt_ff` right before `run_merge()` does.
        _ if rebasing => Some("--ff-only"),
        Some(f) => Some(f),
        None => match repo
            .config_snapshot()
            .string("pull.ff")
            .map(|v| v.to_string().to_ascii_lowercase())
            .as_deref()
        {
            Some("only") => Some("--ff-only"),
            Some("false" | "no" | "off" | "0") => Some("--no-ff"),
            Some(_) => Some("--ff"), // true/yes/on/1/valueless → allow
            None => None,
        },
    };

    // Delegate the fast-forward, --no-ff/diverged merge, dirty check,
    // worktree/index update and git-identical stdout to the ported `merge`,
    // forwarding the resolved ff policy and any diffstat preference.
    let mut merge_args: Vec<String> = Vec::new();
    if let Some(f) = ff_flag {
        merge_args.push(f.to_string());
    }
    // git's pull hands `--no-verify` to the merge only; the rebase path never
    // sees it (`builtin/pull.c` pushes it in `run_merge`).
    if no_verify {
        merge_args.push("--no-verify".into());
    }
    // `run_merge()` pushes `opt_diffstat` verbatim.
    if let Some(d) = diffstat {
        merge_args.push(d.to_string());
    }
    merge_args.push(target_ref);
    super::merge(&merge_args)
}
