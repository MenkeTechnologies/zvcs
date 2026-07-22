//! Client-side hook execution — the veto/notify hooks git runs around commits,
//! merges, pushes and rebases (`pre-commit`, `commit-msg`, `pre-push`,
//! `pre-rebase`, `pre-merge-commit`, `post-commit`, `post-merge`, …).
//!
//! A hook is the executable file `<hooks-dir>/<event>` (`core.hooksPath` or
//! `<git-dir>/hooks`). It runs in the worktree with `GIT_DIR` set, its stdout
//! pointed at stderr (as git does), and — for hooks that receive one — a payload
//! on stdin. A non-zero exit aborts the operation that invoked it.

use anyhow::Result;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Locate `<hooks-dir>/<event>`, returning it only when it exists and is
/// executable. A present-but-non-executable file draws git's `advice.ignoredHook`
/// hint, matching `builtin/hook` behavior.
fn find(repo: &gix::Repository, event: &str) -> Result<Option<PathBuf>> {
    let dir = match repo.config_snapshot().trusted_path("core.hooksPath")? {
        Some(p) => p.to_path_buf(),
        None => repo.common_dir().join("hooks"),
    };
    let path = dir.join(event);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(None);
    };
    if meta.is_dir() {
        return Ok(None);
    }
    if meta.permissions().mode() & 0o111 != 0 {
        return Ok(Some(path));
    }
    if repo.config_snapshot().boolean("advice.ignoredHook") != Some(false) {
        eprintln!(
            "hint: The '{}' hook was ignored because it's not set as executable.",
            path.display()
        );
    }
    Ok(None)
}

/// Run the client-side hook `event` if present, feeding `args` and (optionally)
/// `stdin`. Returns `Ok(true)` to proceed — no hook installed, or the hook exited
/// 0 — and `Ok(false)` when the hook exited non-zero, which the caller treats as
/// a veto and aborts the operation, exactly as git does.
pub fn run(
    repo: &gix::Repository,
    event: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<bool> {
    let Some(path) = find(repo, event)? else {
        return Ok(true);
    };

    // Hooks run in the worktree (or the git dir for a bare repo) with GIT_DIR set,
    // and git points their stdout at stderr so hook chatter never pollutes the
    // command's own stdout.
    let workdir = repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_owned();
    let mut cmd = Command::new(&path);
    cmd.args(args)
        .current_dir(&workdir)
        .env("GIT_DIR", repo.git_dir())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    let mut child = cmd.spawn()?;
    if let Some(data) = stdin {
        if let Some(mut sink) = child.stdin.take() {
            sink.write_all(data)?;
        }
    }
    Ok(child.wait()?.success())
}
