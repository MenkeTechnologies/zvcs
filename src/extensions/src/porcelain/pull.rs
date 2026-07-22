//! `git pull` — fetch the configured (or named) remote, then integrate the
//! fetched upstream into the current branch.
//!
//! `pull` is `fetch` followed by a merge. The fetch is a blocking network
//! operation (like `zsync`); the integration step is delegated to the
//! already-ported [`merge`](super::merge), so a fast-forward
//! (`Updating <old>..<new>` / `Fast-forward`), a `--no-ff` merge commit, a
//! diverged three-way merge, and `Already up to date.` are all byte-for-byte
//! identical to `git merge`'s.
//!
//! Supported invocation forms:
//!   * `git pull`                  — use the current branch's configured upstream.
//!   * `git pull <remote>`         — fetch `<remote>`, merge the configured upstream branch.
//!   * `git pull <remote> <branch>`— fetch `<remote>`, merge `refs/remotes/<remote>/<branch>`.
//!
//! Fast-forward policy: `pull.ff` (`true`/`false`/`only`) is the default,
//! overriding `merge.ff` for `pull` as git's `config_get_ff()` does; a CLI
//! `--ff`/`--no-ff`/`--ff-only` overrides both, and when neither is set the
//! decision falls through to `merge.ff` inside [`merge`](super::merge). A
//! `--no-ff` requesting a merge commit over a fast-forwardable history, and a
//! `pull.ff=only`/`--ff-only` refusal of a diverged upstream, are handled by
//! `merge` unchanged.
//!
//! A history rewrite (`--rebase`, `pull.rebase`) is refused with a precise
//! message rather than faked — `pull` does not run a post-fetch rebase. The
//! fetch progress summary git prints to stderr (`From …`, per-ref update lines)
//! is not reproduced; refs, index and worktree are fully correct.

use anyhow::{bail, Context, Result};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::remote::Direction;

pub fn pull(args: &[String]) -> Result<ExitCode> {
    // Split flags from positionals. The fast-forward flags are captured to be
    // forwarded verbatim to `merge` (which implements ff/no-ff/ff-only); a
    // post-fetch rebase is refused rather than faked.
    let mut positionals: Vec<&str> = Vec::new();
    // The ff flag a CLI option selects, forwarded to `merge` to override both
    // pull.ff and merge.ff. `None` until an --ff/--no-ff/--ff-only is seen.
    let mut ff_cli: Option<&'static str> = None;
    for arg in args {
        let a = arg.as_str();
        if let Some(flag) = a.strip_prefix("--") {
            let key = flag.split('=').next().unwrap_or(flag);
            match key {
                "ff" => ff_cli = Some("--ff"),
                "ff-only" => ff_cli = Some("--ff-only"),
                "no-ff" => ff_cli = Some("--no-ff"),
                "no-rebase" | "quiet" | "verbose" => {}
                "rebase" => bail!("--rebase is not supported (fast-forward only)"),
                other => bail!("unsupported flag --{other}"),
            }
        } else if a.starts_with('-') && a != "-" {
            match a {
                "-q" | "-v" => {}
                "-r" => bail!("--rebase is not supported (fast-forward only)"),
                other => bail!("unsupported flag {other}"),
            }
        } else {
            positionals.push(a);
        }
    }

    let repo = gix::discover(".")?;
    let head_name = repo.head_name()?;

    // Resolve the fast-forward policy git's `config_get_ff()` computes for pull:
    // a CLI flag wins; else pull.ff (which overrides merge.ff) is forwarded to
    // `merge`; else nothing is forwarded and `merge` reads merge.ff itself. The
    // pull.ff value grammar mirrors merge.ff's parse in `merge()`.
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

    // Resolve which remote to fetch and which remote-tracking ref to merge.
    let (remote_name, target_ref) = if positionals.len() >= 2 {
        // Explicit `<remote> <branch>`: after a default-refspec fetch the branch
        // lands at refs/remotes/<remote>/<branch>.
        let remote = positionals[0].to_string();
        let target = format!("refs/remotes/{}/{}", remote, positionals[1]);
        (remote, target)
    } else {
        // No explicit branch: derive everything from the current branch's
        // upstream configuration (branch.<name>.remote / .merge).
        let head = head_name.as_ref().ok_or_else(|| {
            anyhow::anyhow!("You are not currently on a branch. Please specify which branch to pull.")
        })?;

        let remote = match positionals.first() {
            Some(r) => r.to_string(),
            None => match repo.branch_remote_name(head.shorten(), Direction::Fetch) {
                Some(name) => name.as_bstr().to_string(),
                None => bail!("There is no tracking information for the current branch."),
            },
        };

        let target = match repo.branch_remote_tracking_ref_name(head.as_ref(), Direction::Fetch) {
            Some(Ok(name)) => name.as_bstr().to_string(),
            Some(Err(err)) => return Err(err.into()),
            None => bail!("There is no tracking information for the current branch."),
        };
        (remote, target)
    };

    // Phase 1: fetch. Wrap the ref-mutating fetch in the repo lock, then release
    // it before delegating to `merge` (which re-acquires it) to avoid nesting a
    // second acquisition inside the first — that would deadlock a live daemon.
    {
        let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let should_interrupt = AtomicBool::new(false);
        let remote = repo
            .find_remote(remote_name.as_str())
            .with_context(|| format!("'{remote_name}' does not appear to be a configured remote"))?;
        remote
            .connect(Direction::Fetch)?
            .prepare_fetch(gix::progress::Discard, gix::remote::ref_map::Options::default())?
            .receive(gix::progress::Discard, &should_interrupt)?;
    }

    // The upstream ref must now exist locally; if the fetch produced no such
    // tracking ref the requested branch does not exist on the remote.
    if repo.try_find_reference(target_ref.as_str())?.is_none() {
        bail!("couldn't find remote ref {target_ref}");
    }

    // Phase 2: integrate. Delegate the fast-forward, --no-ff/diverged merge,
    // dirty check, worktree/index update and git-identical stdout to the ported
    // `merge`, forwarding the resolved ff policy ahead of the ref to merge.
    let mut merge_args: Vec<String> = Vec::with_capacity(2);
    if let Some(f) = ff_flag {
        merge_args.push(f.to_string());
    }
    merge_args.push(target_ref);
    super::merge(&merge_args)
}
