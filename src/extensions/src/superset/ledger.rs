//! Ledger read/control verbs: `zrepos`, `zreindex`, `zjobs`, `zjob`.
//!
//! Read verbs open the SQLite ledger **read-only** (WAL), so they work whether
//! or not the daemon is running and never contend with the daemon's writes.
//! `zreindex` performs the crawl inline (writing the repo index).

use anyhow::Result;
use std::io::IsTerminal;
use std::process::ExitCode;

/// `git zrepos` — list every repository in the index.
pub fn zrepos(_args: &[String]) -> Result<ExitCode> {
    // One clean path per line on stdout — safe to pipe into fzf/xargs. The count
    // and any hints go to stderr, and only when interactive, so scripts see just
    // the list.
    let interactive = std::io::stdout().is_terminal();
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => {
            if interactive {
                eprintln!("zvcs: no repo index yet (run `git zreindex`)");
            }
            return Ok(ExitCode::SUCCESS);
        }
    };
    let repos = crate::db::list_repos(&conn)?;
    for r in &repos {
        println!("{}", r.workdir.as_deref().unwrap_or(&r.git_dir));
    }
    if interactive {
        eprintln!("{} repo(s)", repos.len());
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zreindex [<path>...]` — (re)crawl for git repos and refresh the index,
/// pruning repos that have since been deleted. With no argument, crawls the
/// configured roots (`[zvcs] crawlroots`, else `$HOME`); with paths, crawls
/// exactly those.
pub fn zreindex(args: &[String]) -> Result<ExitCode> {
    let roots: Vec<std::path::PathBuf> = if args.is_empty() {
        crate::crawler::configured_roots()
    } else {
        args.iter().map(std::path::PathBuf::from).collect()
    };
    let n = crate::crawler::crawl_into_db(&roots)?;
    let pruned = {
        let conn = crate::db::open_rw()?;
        crate::db::prune_missing(&conn)?
    };
    println!("indexed {n} repo(s), pruned {pruned}");
    Ok(ExitCode::SUCCESS)
}

/// `git zjobs [-n <count>]` — list recent ledger jobs (newest first).
pub fn zjobs(args: &[String]) -> Result<ExitCode> {
    let mut limit: i64 = 20;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-n" {
            i += 1;
            limit = args
                .get(i)
                .and_then(|s| s.parse().ok())
                .unwrap_or(limit);
        }
        i += 1;
    }

    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => {
            println!("no jobs yet");
            return Ok(ExitCode::SUCCESS);
        }
    };
    let jobs = crate::db::list_jobs(&conn, limit)?;
    if jobs.is_empty() {
        println!("no jobs");
        return Ok(ExitCode::SUCCESS);
    }
    for j in &jobs {
        println!("#{:<5} {:<8} {}", j.id, j.state, j.kind);
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zjob <id>` — show one job; `git zjob stop|restart <id>` — control it.
pub fn zjob(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        Some("stop") => return zjob_control("JOBSTOP", args.get(1)),
        Some("restart") => return zjob_control("JOBRESTART", args.get(1)),
        _ => {}
    }

    let id: i64 = match args.first().and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => anyhow::bail!("usage: git zjob <id> | git zjob stop|restart <id>"),
    };
    let conn = crate::db::open_ro()?;
    let job = match crate::db::get_job(&conn, id)? {
        Some(j) => j,
        None => anyhow::bail!("no job #{id}"),
    };
    println!("job:    #{}", job.id);
    println!("kind:   {}", job.kind);
    println!("state:  {}", job.state);
    if let Some(gd) = &job.git_dir {
        println!("repo:   {gd}");
    }
    if let Some(code) = job.exit_code {
        println!("exit:   {code}");
    }
    if let Some(sha) = &job.sha_after {
        println!("sha:    {sha}");
    }
    if let Some(out) = &job.output {
        if !out.is_empty() {
            println!("output:\n{out}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Send a one-shot job control request (`JOBSTOP`/`JOBRESTART <id>`) to the
/// daemon and print its reply. Requires a running daemon.
fn zjob_control(verb: &str, id_arg: Option<&String>) -> Result<ExitCode> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let id: i64 = match id_arg.and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => anyhow::bail!("usage: git zjob {} <id>", verb.trim_start_matches("JOB").to_lowercase()),
    };
    let sock = crate::superset::zdaemon::socket_path();
    let mut stream = UnixStream::connect(&sock)
        .map_err(|_| anyhow::anyhow!("daemon not running (job control needs the daemon)"))?;
    writeln!(stream, "{verb} {id}")?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let reply = reply.trim();
    if let Some(new_id) = reply.strip_prefix("JOB ") {
        println!("restarted as job #{new_id}");
        Ok(ExitCode::SUCCESS)
    } else if reply == "OK" {
        println!("stopped job #{id}");
        Ok(ExitCode::SUCCESS)
    } else {
        anyhow::bail!("{}", reply.strip_prefix("ERR ").unwrap_or(reply));
    }
}
