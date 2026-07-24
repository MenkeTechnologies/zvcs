//! User hooks: run a configured command when a watched repo's refs change.
//!
//! Because every indexed repo is in the ledger, the daemon can watch them all
//! (via `notify`) and fire a per-repo hook on change — a filesystem-driven hook
//! system that needs no `.git/hooks` files installed in any repo.
//!
//! The hook is `[zvcs] hook` in the repo's **merged** config, so a single
//! `~/.gitconfig` `zvcs.hook` applies to every watched repo, and any repo may
//! override it in its own `.git/config`. The command runs via `sh -c` with the
//! repo as cwd and a **typed** event context in the environment — enough to write
//! cross-repo reactive rules ("on `commit` in this repo, do X in repo Y"):
//!   * `ZVCS_REPO`    — the repo working directory
//!   * `ZVCS_GIT_DIR` — the repo git directory
//!   * `ZVCS_EVENT`   — the operation, typed from the reflog: `commit`,
//!     `checkout`, `merge`, `pull`, `rebase`, `reset`, `clone`, … (`ref-change` if
//!     it can't be classified)
//!   * `ZVCS_OLD_SHA` / `ZVCS_NEW_SHA` — HEAD before/after the change
//!   * `ZVCS_REF`     — the current branch (or `HEAD` if detached)
//!
//! Hook output goes to the daemon log; a failing hook is recorded in the ledger
//! so it surfaces via notify-on-next-command.

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The zvcs `git` binary, so config writes go through its own porcelain.
fn exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| anyhow!("cannot resolve exe: {e}"))
}

/// Resolve a directory argument (default `.`) to the `(git_dir, workdir)` of the
/// repo containing it. A working tree is required — hooks run inside it.
pub fn resolve(dir: &str) -> Result<(PathBuf, PathBuf)> {
    let repo = gix::discover(dir).map_err(|e| anyhow!("{dir}: not a git repository: {e}"))?;
    let git_dir = repo.git_dir().to_path_buf();
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("{dir}: a working tree is required (bare repo)"))?
        .to_path_buf();
    Ok((git_dir, workdir))
}

/// Write `zvcs.hook = cmd` into the *local* config of the repo at `workdir`.
/// `git config` runs with `cwd = workdir` so it always targets that repo's own
/// `.git/config`, never the caller's cwd.
pub fn set_hook(workdir: &Path, cmd: &str) -> Result<()> {
    let ok = Command::new(exe()?)
        .current_dir(workdir)
        .args(["config", "zvcs.hook", cmd])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        bail!("failed to set zvcs.hook in {}", workdir.display());
    }
    // Mirror the hook into the index so the daemon can build its watch set from
    // the armed repos directly, without opening every indexed repo's config.
    if let Ok((git_dir, wd)) = resolve(&workdir.to_string_lossy()) {
        if let Ok(conn) = crate::db::open_rw() {
            let _ = crate::db::set_repo_hook(&conn, &git_dir, Some(&wd), Some(cmd));
        }
    }
    Ok(())
}

/// Remove the local `zvcs.hook` from the repo at `workdir` (idempotent — a
/// missing key is not an error here).
pub fn unset_hook(workdir: &Path) -> Result<()> {
    let _ = Command::new(exe()?)
        .current_dir(workdir)
        .args(["config", "--unset", "zvcs.hook"])
        .status();
    // Clear the mirrored hook in the index so the daemon stops watching it.
    if let Ok((git_dir, wd)) = resolve(&workdir.to_string_lossy()) {
        if let Ok(conn) = crate::db::open_rw() {
            let _ = crate::db::set_repo_hook(&conn, &git_dir, Some(&wd), None);
        }
    }
    Ok(())
}

/// Flip the global `zvcs.autohook` master switch on, so every indexed repo with
/// a local hook fires. Idempotent; safe to call on every `ztrigger`.
pub fn enable_autohook() -> Result<()> {
    let _ = Command::new(exe()?)
        .args(["config", "--global", "zvcs.autohook", "true"])
        .status();
    Ok(())
}

/// Flip the global `zvcs.autostatus` switch on, so the daemon watches every
/// indexed repo and maintains its cached status. Idempotent.
pub fn enable_autostatus() -> Result<()> {
    let _ = Command::new(exe()?)
        .args(["config", "--global", "zvcs.autostatus", "true"])
        .status();
    Ok(())
}

/// Add the repo at `(git_dir, workdir)` to the index so the watch loop's
/// `build_targets` covers it.
pub fn index(git_dir: &Path, workdir: &Path) -> Result<()> {
    let conn = crate::db::open_rw()?;
    crate::db::upsert_repo(&conn, git_dir, Some(workdir))?;
    Ok(())
}

/// Restart the daemon (start-if-down, reload-if-up) so it rebuilds its watch set
/// and immediately covers a just-registered repo. Best-effort — output goes to
/// the daemon's own reload path. Suppressed when `ZVCS_NO_DAEMON` is set, so
/// scripted bulk `ztrigger` runs (and tests) don't reload once per call.
pub fn reload_daemon() {
    if std::env::var_os("ZVCS_NO_DAEMON").is_some() {
        return;
    }
    if let Ok(e) = exe() {
        let _ = Command::new(e).args(["zdaemon", "reload"]).status();
    }
}

/// Every indexed repo that carries a local `zvcs.hook`, as `(path, command)`
/// pairs — the shared source for `zhook list` and `ztrigger list`.
pub fn list() -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let Ok(conn) = crate::db::open_ro() else {
        return Ok(out);
    };
    for r in crate::db::list_repos(&conn)? {
        if let Ok(repo) = gix::open(&r.git_dir) {
            if let Some(hook) = repo.config_snapshot().string("zvcs.hook") {
                let path = r.workdir.unwrap_or(r.git_dir);
                out.push((path, hook.to_string()));
            }
        }
    }
    Ok(out)
}

/// Run a directory trigger's command (`git ztrigger`): `sh -c <cmd>` with the
/// watched directory as cwd and `ZVCS_DIR` in the environment. Best-effort;
/// output goes to the daemon log. Independent of git — the directory need not be
/// a repository.
pub fn run_command(dir: &Path, cmd: &str) -> bool {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .env("ZVCS_DIR", dir)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            if !s.trim().is_empty() {
                println!("[zvcs trigger] {}: {}", dir.display(), s.trim());
            }
            true
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            println!("[zvcs trigger] {}: FAILED: {}", dir.display(), err.trim());
            false
        }
        Err(e) => {
            println!("[zvcs trigger] {}: could not run: {e}", dir.display());
            false
        }
    }
}

/// Read the `zvcs.hook` command for the repo at `workdir`, if configured.
pub fn hook_for(workdir: &Path) -> Option<String> {
    let repo = gix::discover(workdir).ok()?;
    let snap = repo.config_snapshot();
    let cmd = snap.string("zvcs.hook")?;
    let cmd = cmd.to_string();
    (!cmd.trim().is_empty()).then_some(cmd)
}

/// Run the repo's hook (if any) for a ref-change event. Best-effort, never panics.
pub fn run(git_dir: &Path, workdir: &Path) {
    let Some(cmd) = hook_for(workdir) else {
        return;
    };
    // Typed event context from the reflog.
    let (old, new, kind) = crate::superset::oplog::latest_head_event(git_dir)
        .unwrap_or_else(|| (String::new(), String::new(), "ref-change".to_string()));
    let refname = gix::open(git_dir)
        .ok()
        .and_then(|r| r.head_name().ok().flatten())
        .map(|n| n.shorten().to_string())
        .unwrap_or_else(|| "HEAD".to_string());

    let out = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(workdir)
        .env("ZVCS_REPO", workdir)
        .env("ZVCS_GIT_DIR", git_dir)
        .env("ZVCS_EVENT", &kind)
        .env("ZVCS_OLD_SHA", &old)
        .env("ZVCS_NEW_SHA", &new)
        .env("ZVCS_REF", &refname)
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            if !s.trim().is_empty() {
                println!("[zvcs hook] {}: {}", workdir.display(), s.trim());
            }
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            println!("[zvcs hook] {}: FAILED: {}", workdir.display(), err.trim());
            let _ = crate::db::record_failure(git_dir, "hook", &format!("{cmd}: {}", err.trim()));
        }
        Err(e) => {
            println!("[zvcs hook] {}: could not run: {e}", workdir.display());
            let _ = crate::db::record_failure(git_dir, "hook", &format!("{cmd}: {e}"));
        }
    }
}
