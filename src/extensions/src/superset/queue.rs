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
        "env": carried_env(),
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
        "env": carried_env(),
    });
    submit_or_run(&spec)
}

/// `git zsubmit [--] <command> [args...]` — ship an arbitrary command to the
/// daemon's worker pool as an async job. Prints the job id; track it with `git
/// zjobs` / `git zjob <id>` and cancel it with `git zjob stop <id>`, like any
/// job. The command runs in the current directory (a repo is not required) with
/// no shell (submit `sh -c "..."` for pipes/redirects). Falls back to running
/// inline when no daemon is up.
pub fn zsubmit(args: &[String]) -> Result<ExitCode> {
    let argv: Vec<String> = match args.split_first() {
        Some((first, rest)) if first == "--" => rest.to_vec(),
        _ => args.to_vec(),
    };
    if argv.is_empty() {
        anyhow::bail!("usage: git zsubmit [--] <command> [args...]");
    }
    submit_or_run(&exec_spec(argv))
}

/// Build an `exec` job spec for `argv` (argv[0] is the program). Attributes the
/// job to the cwd repo when there is one; carries the session + environment.
/// Shared by `zsubmit` and the lock-contention auto-queue.
fn exec_spec(argv: Vec<String>) -> Value {
    // Runs anywhere: the workdir is the current directory. If that happens to be a
    // repo, attribute the job to it; otherwise it is a repo-less job.
    let workdir = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let git_dir = gix::discover(".").ok().map(|r| {
        r.git_dir()
            .canonicalize()
            .unwrap_or_else(|_| r.git_dir().to_path_buf())
            .to_string_lossy()
            .into_owned()
    });
    // A readable ledger label: `exec: <command>`, char-safely trimmed.
    let cmd = argv.join(" ");
    let label = if cmd.chars().count() > 52 {
        let head: String = cmd.chars().take(52).collect();
        format!("exec: {head}…")
    } else {
        format!("exec: {cmd}")
    };
    let mut spec = json!({
        "kind": "exec",
        "label": label,
        "workdir": workdir,
        "argv": argv,
        "session": session(),
        "env": carried_env(),
    });
    if let Some(gd) = git_dir {
        spec["git_dir"] = json!(gd);
    }
    spec
}

/// Queue a git command that could not take its repo lock (contention). The job
/// re-runs the command through this binary with `ZVCS_QUEUED=1` set, so when the
/// daemon's jobpool runs it, it BLOCKS on the repo's fair lane rather than
/// re-queueing (loop guard). Prints the job number; runs inline if no daemon.
pub fn queue_verb(sub: &str, args: &[String]) -> Result<ExitCode> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("cannot resolve zvcs binary: {e}"))?;
    let mut argv = vec![exe.to_string_lossy().into_owned(), sub.to_string()];
    argv.extend(args.iter().cloned());
    let mut spec = exec_spec(argv);
    spec["env"]["ZVCS_QUEUED"] = json!("1");
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

/// The submitting session key (for attribution/notify). Delegates to the shared
/// [`crate::session_key`] so the empty-`ZVCS_SESSION` fallback is applied here too.
fn session() -> String {
    crate::session_key()
}

/// Identity/config env vars carried into the async job spec so the commit is
/// attributed to the SUBMITTER, not the daemon's inherited environment. Without
/// this, every agent's async `zcommit` takes the identity of whoever first
/// autostarted the daemon.
const CARRY_ENV: &[&str] = &[
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
];

/// Snapshot the carried identity env of the submitting process as a JSON object.
fn carried_env() -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    for k in CARRY_ENV {
        if let Ok(v) = std::env::var(k) {
            m.insert((*k).to_string(), serde_json::Value::String(v));
        }
    }
    m
}

/// A push pre-flight verdict.
enum Verdict {
    /// Fast-forwardable (or new branch / up to date) — push may proceed.
    Ok,
    /// Non-fast-forward — refuse before enqueue, with a reason.
    Refuse(String),
    /// Could not determine (offline, no remote, detached HEAD) — try the next
    /// source or, ultimately, let the push itself decide.
    Unknown,
}

/// Push pre-flight: prefer a live `ls-refs` against the remote (the accurate,
/// current tip); fall back to the network-free remote-tracking comparison when
/// the remote can't be reached. Refuse a non-fast-forward before enqueue.
fn preflight_push() -> Result<()> {
    let repo = gix::discover(".")?;
    let verdict = match lsrefs_verdict(&repo) {
        Verdict::Unknown => local_verdict(&repo),
        v => v,
    };
    match verdict {
        Verdict::Refuse(msg) => Err(anyhow!(msg)),
        Verdict::Ok | Verdict::Unknown => Ok(()),
    }
}

/// The current branch short name and HEAD commit, or `None` if detached/unborn.
fn head_branch(repo: &gix::Repository) -> Option<(String, gix::hash::ObjectId)> {
    let branch = repo.head_name().ok().flatten()?;
    let short = branch.shorten().to_string();
    let local = repo.head().ok()?.try_peel_to_id().ok()??.detach();
    Some((short, local))
}

/// Given the remote's tip for our branch, decide fast-forwardability locally.
fn verdict_against_remote(
    repo: &gix::Repository,
    short: &str,
    local: gix::hash::ObjectId,
    remote: gix::hash::ObjectId,
) -> Verdict {
    if local == remote {
        return Verdict::Ok; // up to date
    }
    // If we don't even have the remote's commit, the remote is strictly ahead of
    // our knowledge — a non-fast-forward.
    if repo.find_object(remote).is_err() {
        return Verdict::Refuse(format!("origin/{short} has commits you don't have; pull first"));
    }
    // Otherwise ff-able iff the remote tip is an ancestor of local.
    match repo.merge_base(local, remote) {
        Ok(base) if base.detach() == remote => Verdict::Ok,
        _ => Verdict::Refuse(format!("origin/{short} has diverged from your branch; pull first")),
    }
}

/// Live `ls-refs` verdict: one ref advertisement (no packfile) to read the
/// remote's current tip for our branch. `Unknown` on any connection error.
fn lsrefs_verdict(repo: &gix::Repository) -> Verdict {
    let Some((short, local)) = head_branch(repo) else {
        return Verdict::Unknown;
    };
    let Ok(remote) = repo.find_remote("origin") else {
        return Verdict::Unknown;
    };
    let conn = match remote.connect(gix::remote::Direction::Fetch) {
        Ok(c) => c,
        Err(_) => return Verdict::Unknown,
    };
    let refmap = match conn.ref_map(gix::progress::Discard, gix::remote::ref_map::Options::default())
    {
        Ok((rm, _handshake)) => rm,
        Err(_) => return Verdict::Unknown,
    };

    let target = format!("refs/heads/{short}");
    let remote_oid = refmap.remote_refs.iter().find_map(|r| {
        use gix::protocol::handshake::Ref::*;
        match r {
            Direct { full_ref_name, object }
            | Peeled { full_ref_name, object, .. }
            | Symbolic { full_ref_name, object, .. }
                if full_ref_name.to_string() == target =>
            {
                Some(*object)
            }
            _ => None,
        }
    });

    match remote_oid {
        None => Verdict::Ok, // remote lacks the branch → push creates it
        Some(remote) => verdict_against_remote(repo, &short, local, remote),
    }
}

/// Network-free verdict: compare HEAD to the already-fetched `origin/<branch>`
/// remote-tracking ref. `Unknown` when there's nothing to compare against.
fn local_verdict(repo: &gix::Repository) -> Verdict {
    let Some((short, local)) = head_branch(repo) else {
        return Verdict::Unknown;
    };
    let tracking = format!("refs/remotes/origin/{short}");
    let remote = match repo.try_find_reference(&tracking) {
        Ok(Some(r)) => match r.into_fully_peeled_id() {
            Ok(id) => id.detach(),
            Err(_) => return Verdict::Unknown,
        },
        _ => return Verdict::Unknown, // never fetched
    };
    verdict_against_remote(repo, &short, local, remote)
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
    let result = crate::jobrun::execute(spec, &crate::jobrun::Cancel::none());
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
    // The ledger's displayed kind prefers `label` (a human summary, e.g. the
    // exec command) over the dispatch `kind`; commit/push set no label, so they
    // still show "commit"/"push".
    let Some(kind) = spec
        .get("label")
        .and_then(Value::as_str)
        .or_else(|| spec.get("kind").and_then(Value::as_str))
    else {
        return (None, ());
    };
    // `git_dir` is optional: a `zsubmit` job run outside any repo has none, so it
    // records with a NULL repo_id (the ledger and `zjobs` LEFT-JOIN handle it).
    let git_dir = spec.get("git_dir").and_then(Value::as_str);
    let wd = spec.get("workdir").and_then(Value::as_str);
    let session = spec.get("session").and_then(Value::as_str);
    let id = (|| {
        let conn = crate::db::open_rw().ok()?;
        let repo_id = match git_dir {
            Some(gd) => Some(crate::db::upsert_repo(&conn, std::path::Path::new(gd), wd.map(std::path::Path::new)).ok()?),
            None => None,
        };
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
