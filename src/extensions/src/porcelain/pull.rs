//! `git pull` — fetch the configured (or named) remote, then integrate the
//! fetched upstream into the current branch.
//!
//! `pull` is `fetch` followed by an integration step. The fetch is delegated to
//! the ported [`fetch`](super::fetch), so its option surface is available to
//! `pull` verbatim: `--all`, `-f`/`--force`, `-t`/`--tags`, `-p`/`--prune`,
//! `--depth`/`--deepen`/`--unshallow`, `--shallow-since`/`--shallow-exclude`,
//! `-q`/`-v`, and the `From …` per-ref summary git prints to stderr.
//!
//! The integration step is the ported [`merge`](super::merge) by default, or the
//! ported [`rebase`](super::rebase) when a rebase is requested. The rebase is
//! selected the way git's `config_get_rebase()` selects it: a CLI
//! `--rebase[=<value>]`/`-r`/`--no-rebase` wins, else `branch.<name>.rebase`,
//! else `pull.rebase`; `<value>` is `true`/`false`/`merges`/`interactive`
//! (`preserve` is refused as git refuses it). A rebase forwards its compatible
//! knobs to `rebase` — `-s`/`--strategy`, `-X`/`--strategy-option`, `--signoff`,
//! `--autostash` (and `rebase.autoStash`), `--rebase-merges` (from
//! `--rebase=merges`), `--stat`/`--no-stat`/`-n` — and rebases the current branch
//! onto the fetched upstream.
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
//! `--set-upstream`/`-a`/`--append` (not exposed by the high-level fetch), and
//! `--gpg-sign`/`-S`/`--verify-signatures` (GPG is not vendored).

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
    let mut stat: Option<bool> = None; // --stat / --no-stat / -n
    let mut strategy: Option<String> = None; // -s / --strategy
    let mut strategy_opts: Vec<String> = Vec::new(); // -X / --strategy-option
    let mut signoff = false;
    let mut autostash: Option<bool> = None;

    // Knobs forwarded to `fetch`.
    let mut f_all = false;
    let mut f_force = false;
    let mut f_tags = false;
    let mut f_prune = false;
    let mut f_unshallow = false;
    let mut f_depth: Option<String> = None;
    let mut f_deepen: Option<String> = None;
    let mut f_shallow_since: Option<String> = None;
    let mut f_shallow_exclude: Vec<String> = Vec::new();
    let mut f_quiet = false;
    let mut f_verbose = false;

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
            "--stat" | "--summary" => stat = Some(true),
            "--no-stat" | "--no-summary" | "-n" => stat = Some(false),
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
            "--depth" => f_depth = Some(take_value!("depth")),
            "--deepen" => f_deepen = Some(take_value!("deepen")),
            "--shallow-since" => f_shallow_since = Some(take_value!("shallow-since")),
            "--shallow-exclude" => f_shallow_exclude.push(take_value!("shallow-exclude")),
            "-q" | "--quiet" => f_quiet = true,
            "-v" | "--verbose" => f_verbose = true,

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
            "-a" | "--append" => {
                bail!("--append is not supported (FETCH_HEAD is not written)")
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
    let target_ref = if positionals.len() >= 2 {
        // Explicit `<remote> <branch>`: after a default-refspec fetch the branch
        // lands at refs/remotes/<remote>/<branch>.
        format!("refs/remotes/{}/{}", positionals[0], positionals[1])
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
    // `--all` fans out over every configured remote and takes no repository
    // argument; otherwise the first positional names the remote to fetch (the
    // configured default refspec updates the tracking ref merged/rebased below).
    if !f_all {
        if let Some(r) = positionals.first() {
            fetch_args.push((*r).to_string());
        }
    }
    // Network / bad-remote failures surface as `Err`; a ref-rejection returns a
    // non-success code with the summary already printed. The tracking-ref check
    // below then reports the missing upstream, as git's pull does.
    let _ = super::fetch(&fetch_args)?;

    // The upstream ref must now exist locally; if the fetch produced no such
    // tracking ref the requested branch does not exist on the remote.
    if repo.try_find_reference(target_ref.as_str())?.is_none() {
        bail!("couldn't find remote ref {target_ref}");
    }

    // ---- phase 2: integrate ----------------------------------------------
    if rebasing {
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
        // Autostash: CLI flag wins, else rebase.autoStash. A clean tree makes it
        // a no-op; a dirty tree is handled by the rebase port's own policy.
        let want_autostash = match autostash {
            Some(v) => v,
            None => repo.config_snapshot().boolean("rebase.autoStash") == Some(true),
        };
        if want_autostash {
            rebase_args.push("--autostash".into());
        }
        match stat {
            Some(true) => rebase_args.push("--stat".into()),
            Some(false) => rebase_args.push("--no-stat".into()),
            None => {}
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
    match stat {
        Some(true) => merge_args.push("--stat".into()),
        Some(false) => merge_args.push("--no-stat".into()),
        None => {}
    }
    merge_args.push(target_ref);
    super::merge(&merge_args)
}
