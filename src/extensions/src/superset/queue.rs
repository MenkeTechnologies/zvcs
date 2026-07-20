//! Async write-verbs: `zcommit` and `zpush`.
//!
//! Each builds a job spec and submits it to the daemon over the socket, which
//! runs it off this process's critical path and records it in the ledger; the
//! client returns immediately with the job number (stderr). If no daemon is
//! reachable the job runs **synchronously** in this process instead (and is
//! still recorded), so the verb always works.
//!
//! `zpush` additionally does a network-free pre-flight: if the local
//! remote-tracking ref shows the remote ahead of / diverged from HEAD, the push
//! is refused before enqueue (`pull first`) rather than failing async later.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

/// `git zcommit [<paths>...] -m <msg> [--push]` — atomic staged-commit job.
pub fn zcommit(args: &[String]) -> Result<ExitCode> {
    let mut paths: Vec<String> = Vec::new();
    let mut message: Option<String> = None;
    let mut push = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-m" | "--message" => {
                i += 1;
                message = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow!("`-m` requires a message"))?
                        .clone(),
                );
            }
            "--push" => push = true,
            s if s.starts_with("-m") && s.len() > 2 => message = Some(s[2..].to_string()),
            s if s.starts_with('-') => anyhow::bail!("unsupported option `{s}`"),
            s => paths.push(s.to_string()),
        }
        i += 1;
    }

    let message = message.ok_or_else(|| anyhow!("zcommit needs -m <message>"))?;
    let (git_dir, workdir) = here()?;
    let spec = json!({
        "kind": "commit",
        "git_dir": git_dir,
        "workdir": workdir,
        "paths": paths,
        "message": message,
        "push": push,
        "session": session(),
    });
    submit_or_run(&spec)
}

/// `git zpush [<refspec>]` — async push job with a network-free ff pre-flight.
pub fn zpush(args: &[String]) -> Result<ExitCode> {
    let refspec: Option<String> = {
        let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
        if positional.is_empty() {
            None
        } else {
            Some(positional.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "))
        }
    };

    preflight_push()?;

    let (git_dir, workdir) = here()?;
    let spec = json!({
        "kind": "push",
        "git_dir": git_dir,
        "workdir": workdir,
        "refspec": refspec,
        "session": session(),
    });
    submit_or_run(&spec)
}

/// Canonical `(git_dir, workdir)` of the repo at cwd.
fn here() -> Result<(String, String)> {
    let repo = gix::discover(".")?;
    let git_dir = repo
        .git_dir()
        .canonicalize()
        .unwrap_or_else(|_| repo.git_dir().to_path_buf());
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("a working tree is required"))?
        .canonicalize()
        .unwrap_or_else(|_| repo.workdir().unwrap().to_path_buf());
    Ok((git_dir.to_string_lossy().into_owned(), workdir.to_string_lossy().into_owned()))
}

/// The submitting session key (for attribution/notify): `ZVCS_SESSION` env, else
/// the parent process id.
fn session() -> String {
    std::env::var("ZVCS_SESSION").unwrap_or_else(|_| format!("pid-{}", std::os::unix::process::parent_id()))
}

/// Network-free push pre-flight: refuse if the remote-tracking ref is ahead of
/// or diverged from HEAD (a prior fetch already told us so). Skipped when there
/// is no remote-tracking ref to compare against.
fn preflight_push() -> Result<()> {
    let repo = gix::discover(".")?;
    let Some(branch) = repo.head_name()? else {
        return Ok(()); // detached HEAD: let the push itself decide
    };
    let short = branch.shorten().to_string();
    let local = match repo.head()?.try_peel_to_id()? {
        Some(id) => id.detach(),
        None => return Ok(()),
    };
    let tracking = format!("refs/remotes/origin/{short}");
    let remote = match repo.try_find_reference(&tracking)? {
        Some(r) => r.into_fully_peeled_id()?.detach(),
        None => return Ok(()), // never fetched: nothing to compare
    };
    if local == remote {
        return Ok(()); // up to date; push is a no-op / safe
    }
    // ff-able iff the remote tip is an ancestor of local (merge-base == remote).
    let ff = matches!(repo.merge_base(local, remote), Ok(base) if base.detach() == remote);
    if ff {
        Ok(())
    } else {
        Err(anyhow!(
            "origin/{short} has commits you don't have; pull first"
        ))
    }
}

/// Submit `spec` to the daemon; on success print the job number to stderr.
/// If no daemon is reachable, run the job synchronously in-process (still
/// recorded in the ledger) and return its real exit status.
fn submit_or_run(spec: &Value) -> Result<ExitCode> {
    if let Some(id) = try_submit(spec) {
        eprintln!("zvcs: queued job #{id}");
        return Ok(ExitCode::SUCCESS);
    }
    // No daemon: synchronous fallback.
    run_inline(spec)
}

/// Try to hand the job to the daemon. Returns the job id on success.
fn try_submit(spec: &Value) -> Option<i64> {
    let sock = crate::superset::zdaemon::socket_path();
    let mut stream = UnixStream::connect(&sock).ok()?;
    let line = format!("SUBMIT {}\n", serde_json::to_string(spec).ok()?);
    stream.write_all(line.as_bytes()).ok()?;
    stream.flush().ok()?;
    let mut reader = BufReader::new(&stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).ok()?;
    resp.trim().strip_prefix("JOB ").and_then(|s| s.parse().ok())
}

/// Execute the job in this process and record it in the ledger.
fn run_inline(spec: &Value) -> Result<ExitCode> {
    let (id, _) = record_queued(spec);
    let result = crate::jobrun::execute(spec);
    finalize(id, &result, spec);

    print!("{}", result.output);
    if result.ok {
        if let Some(id) = id {
            eprintln!("zvcs: job #{id} done");
        }
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Insert a queued ledger row for an inline run; `None` if the ledger is
/// unavailable (the job still runs, just unrecorded).
fn record_queued(spec: &Value) -> (Option<i64>, ()) {
    let (Some(gd), Some(kind)) = (
        spec.get("git_dir").and_then(Value::as_str),
        spec.get("kind").and_then(Value::as_str),
    ) else {
        return (None, ());
    };
    let wd = spec.get("workdir").and_then(Value::as_str);
    let session = spec.get("session").and_then(Value::as_str);
    let id = (|| {
        let conn = crate::db::open_rw().ok()?;
        let repo_id = crate::db::upsert_repo(&conn, std::path::Path::new(gd), wd.map(std::path::Path::new)).ok()?;
        crate::db::insert_job(&conn, repo_id, kind, &spec.to_string(), session).ok()
    })();
    (id, ())
}

fn finalize(id: Option<i64>, result: &crate::jobrun::JobResult, _spec: &Value) {
    let Some(id) = id else { return };
    if let Ok(conn) = crate::db::open_rw() {
        let _ = crate::db::job_running(&conn, id);
        let state = if result.ok { "done" } else { "failed" };
        let exit = if result.ok { 0 } else { 1 };
        let _ = crate::db::job_finished(&conn, id, state, exit, &result.output, result.sha_after.as_deref());
    }
}
