use anyhow::{anyhow, bail, Result};
use std::process::ExitCode;

use gix::hash::ObjectId;

use super::push_proto::{self, Request};

/// `git push [<remote>] [<refspec>...]` — upload commits and update remote refs.
///
/// The object upload is a faithful port of git's `send-pack.c` (see
/// [`super::push_proto`]), bridged onto gitoxide's HTTP transport. This function
/// is the porcelain around it: it resolves the remote and the `<refspec>`s into
/// concrete ref updates, runs the push, and prints git's `To <url>` status block.
///
/// Supported forms:
///   * `git push` — push the current branch to a same-named remote branch
///   * `git push <remote>` / `git push <remote> <refspec>...`
///   * refspecs `src`, `src:dst`, `+src:dst` (force), `:dst` (delete)
///   * `-f` / `--force`
pub fn push(args: &[String]) -> Result<ExitCode> {
    let mut force = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut end_of_options = false;
    for a in args {
        if end_of_options || !a.starts_with('-') {
            positionals.push(a);
            continue;
        }
        match a.as_str() {
            "--" => end_of_options = true,
            "-f" | "--force" => force = true,
            // Progress/verbosity flags don't change what is pushed.
            "-q" | "--quiet" | "-v" | "--verbose" | "--progress" | "--no-progress" => {}
            other => bail!("unsupported option {other:?}"),
        }
    }

    let repo = gix::discover(".")?;

    let remote_name: String = match positionals.first() {
        Some(r) => (*r).to_string(),
        None => default_push_remote(&repo),
    };
    // git accepts a configured remote name or a bare URL as the destination.
    let remote = match repo.find_remote(remote_name.as_str()) {
        Ok(r) => r,
        Err(_) => repo.remote_at(remote_name.as_str())?,
    };

    // Resolve the refspecs into concrete updates. With none given, git pushes the
    // current branch; a detached HEAD or unborn branch has nothing to push.
    let specs: Vec<&str> = positionals.get(1..).unwrap_or(&[]).to_vec();
    let requests = if specs.is_empty() {
        vec![current_branch_request(&repo, force)?]
    } else {
        specs
            .iter()
            .map(|s| parse_refspec(&repo, s, force))
            .collect::<Result<Vec<_>>>()?
    };

    let outcome = push_proto::send_pack(&repo, &remote, &requests)?;

    // Print git's status block on stderr and derive the exit code.
    report(&outcome)
}

/// Turn one `<refspec>` into a ref update. Handles a leading `+` (force),
/// `src:dst`, bare `src` (same name on both ends), and `:dst` (delete).
fn parse_refspec(repo: &gix::Repository, spec: &str, force: bool) -> Result<Request> {
    let (spec, force) = match spec.strip_prefix('+') {
        Some(rest) => (rest, true),
        None => (spec, force),
    };
    let (src, dst) = match spec.split_once(':') {
        Some((s, d)) => (s, d),
        None => (spec, spec),
    };

    let null = ObjectId::null(repo.object_hash());
    let new = if src.is_empty() {
        // `:dst` deletes the remote ref.
        null
    } else {
        repo.rev_parse_single(src)
            .map_err(|_| anyhow!("src refspec {src} does not match any"))?
            .detach()
    };
    let dst = if dst.is_empty() { src } else { dst };
    Ok(Request {
        name: full_ref_name(dst),
        new,
        force,
    })
}

/// The update for a bare `git push`: the current branch to a same-named remote
/// branch. Rejects a detached HEAD and an unborn branch exactly as git does.
fn current_branch_request(repo: &gix::Repository, force: bool) -> Result<Request> {
    let head = repo.head()?;
    let branch = head
        .referent_name()
        .ok_or_else(|| anyhow!("You are not currently on a branch."))?
        .shorten()
        .to_string();
    let new = repo
        .head_id()
        .map_err(|_| anyhow!("src refspec {branch} does not match any"))?
        .detach();
    Ok(Request {
        name: format!("refs/heads/{branch}"),
        new,
        force,
    })
}

/// Expand a short ref name to its full form. A name that already starts with
/// `refs/` is kept; anything else is treated as a branch, as git's default push
/// refspec (`refs/heads/*`) does for an unqualified destination.
fn full_ref_name(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    }
}

/// Print the `To <url>` status block (git prints it on stderr) and return the
/// exit code: failure if the unpack failed or any ref was rejected.
fn report(outcome: &push_proto::Outcome) -> Result<ExitCode> {
    let mut any_failed = outcome.unpack.is_err();

    if let Err(reason) = &outcome.unpack {
        eprintln!("error: unpack failed: {reason}");
    }

    // "Everything up-to-date" when nothing was actually updated and nothing failed.
    let did_update = outcome
        .statuses
        .iter()
        .any(|s| !s.up_to_date && s.result.is_ok());
    if !did_update && !any_failed && outcome.statuses.iter().all(|s| s.result.is_ok()) {
        eprintln!("Everything up-to-date");
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("To {}", outcome.url);
    for s in &outcome.statuses {
        let short = |oid: &ObjectId| oid.to_hex_with_len(7).to_string();
        let src_dst = format!("{} -> {}", short_ref(&s.name), short_ref(&s.name));
        match &s.result {
            Ok(()) if s.up_to_date => eprintln!(" = [up to date]      {src_dst}"),
            Ok(()) if s.old.is_null() => eprintln!(" * [new branch]      {src_dst}"),
            Ok(()) if s.new.is_null() => {
                eprintln!(" - [deleted]         {}", short_ref(&s.name));
            }
            Ok(()) => {
                let sep = if s.forced { "..." } else { ".." };
                let flag = if s.forced { "+" } else { " " };
                eprintln!("{flag}  {}{sep}{}  {src_dst}", short(&s.old), short(&s.new));
            }
            Err(reason) => {
                any_failed = true;
                eprintln!(" ! [rejected]        {src_dst} ({reason})");
            }
        }
    }

    if any_failed {
        eprintln!("error: failed to push some refs to '{}'", outcome.url);
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// Shorten a full ref name for display (`refs/heads/main` → `main`), matching how
/// git names the pushed ref in its status block.
fn short_ref(name: &str) -> &str {
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/tags/"))
        .unwrap_or(name)
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
