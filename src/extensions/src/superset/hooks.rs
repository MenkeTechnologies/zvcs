//! User hooks: run a configured command when a watched repo's refs change.
//!
//! Because every indexed repo is in the ledger, the daemon can watch them all
//! (via `notify`) and fire a per-repo hook on change — a filesystem-driven hook
//! system that needs no `.git/hooks` files installed in any repo.
//!
//! The hook is `[zvcs] hook` in the repo's **merged** config, so a single
//! `~/.gitconfig` `zvcs.hook` applies to every watched repo, and any repo may
//! override it in its own `.git/config`. The command runs via `sh -c` with the
//! repo as cwd and these environment variables set:
//!   * `ZVCS_REPO`    — the repo working directory
//!   * `ZVCS_GIT_DIR` — the repo git directory
//!   * `ZVCS_EVENT`   — the event kind (currently `ref-change`)
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
    let out = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(workdir)
        .env("ZVCS_REPO", workdir)
        .env("ZVCS_GIT_DIR", git_dir)
        .env("ZVCS_EVENT", "ref-change")
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
