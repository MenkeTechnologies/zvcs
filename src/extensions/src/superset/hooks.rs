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

use std::path::Path;
use std::process::Command;

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
