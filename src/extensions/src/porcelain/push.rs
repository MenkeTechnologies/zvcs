use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::remote::Direction;

/// `git push [<remote>] [<refspec>...]` — upload commits and update a remote ref.
///
/// gitoxide (the vendored `gix`) implements the fetch / `git-upload-pack` half of
/// the smart transfer protocol but ships **no** `git-receive-pack` (send-pack)
/// driver: there is no API to encode the ref-update command list, generate and
/// stream the pack, or parse the server's `report-status`. `Service::ReceivePack`
/// exists in the transport layer as a bare enum variant with nothing behind it.
///
/// So this command does the faithful local pre-flight that stock `git push`
/// performs before it ever contacts the remote — resolve the named (or default)
/// remote, and the branch / commit that would be pushed, rejecting the same
/// invalid invocations git rejects (unknown remote, detached HEAD, unborn
/// branch) — and then reports precisely that the object upload itself is
/// unsupported. It never fakes success or prints a fabricated status line.
pub fn push(args: &[String]) -> Result<ExitCode> {
    // Positional args are `[<remote>] [<refspec>...]`; anything starting with '-'
    // is a flag. Every push variant (force, delete, tags, atomic, …) still needs
    // send-pack, so flags don't change the outcome here and are not parsed.
    let positionals: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();
    let refspecs: &[&str] = positionals.get(1..).unwrap_or(&[]);

    let repo = gix::discover(".")?;

    // The default remote when none is given follows git's push order:
    // branch.<name>.pushRemote, remote.pushDefault, branch.<name>.remote, origin.
    let remote_name: String = match positionals.first() {
        Some(r) => (*r).to_string(),
        None => default_push_remote(&repo),
    };

    // Resolve the remote as `git push <remote>` does — a bad name is a pre-flight
    // error, before any network access.
    let remote = repo.find_remote(remote_name.as_str())?;

    // Determine what would be pushed. With an explicit refspec we echo it; with
    // none, git pushes the current branch, so resolve HEAD the way git does and
    // reject detached HEAD / unborn branch identically.
    let target = match refspecs.first() {
        Some(spec) => (*spec).to_string(),
        None => {
            let head = repo.head()?;
            let branch = head
                .referent_name()
                .ok_or_else(|| anyhow::anyhow!("You are not currently on a branch."))?
                .shorten()
                .to_string();
            // An unborn branch points at no commit — nothing to push.
            if repo.head_id().is_err() {
                bail!("src refspec {branch} does not match any");
            }
            branch
        }
    };

    // Name the destination for the error, preferring the push URL.
    let dest = match remote
        .url(Direction::Push)
        .or_else(|| remote.url(Direction::Fetch))
    {
        Some(url) => format!("{remote_name} ({url})"),
        None => remote_name.to_string(),
    };

    bail!(
        "gitoxide has no git-receive-pack (send-pack) implementation; \
         cannot upload {target} to {dest}"
    )
}

/// The remote `git push` targets with no `<remote>` argument, in git's order:
/// the current branch's `pushRemote`, then `remote.pushDefault`, then the
/// branch's `remote`, then `origin`.
fn default_push_remote(repo: &gix::Repository) -> String {
    let snap = repo.config_snapshot();
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.referent_name().map(|n| n.shorten().to_string()));

    if let Some(b) = &branch {
        if let Some(r) = snap.string(&format!("branch.{b}.pushRemote")) {
            return r.to_string();
        }
    }
    if let Some(r) = snap.string("remote.pushDefault") {
        return r.to_string();
    }
    if let Some(b) = &branch {
        if let Some(r) = snap.string(&format!("branch.{b}.remote")) {
            return r.to_string();
        }
    }
    "origin".to_string()
}
