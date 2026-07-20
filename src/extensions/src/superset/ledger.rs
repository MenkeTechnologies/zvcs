//! Ledger read/control verbs: `zrepos`, `zreindex`, `zjobs`, `zjob`.
//!
//! Read verbs open the SQLite ledger **read-only** (WAL), so they work whether
//! or not the daemon is running and never contend with the daemon's writes.
//! `zreindex` performs the crawl inline (writing the repo index).

use anyhow::Result;
use std::process::ExitCode;

/// `git zrepos` — list every repository in the index.
pub fn zrepos(_args: &[String]) -> Result<ExitCode> {
    let conn = match crate::db::open_ro() {
        Ok(c) => c,
        Err(_) => {
            println!("no repo index yet (run `git zreindex`)");
            return Ok(ExitCode::SUCCESS);
        }
    };
    let repos = crate::db::list_repos(&conn)?;
    if repos.is_empty() {
        println!("no repos indexed (run `git zreindex`)");
        return Ok(ExitCode::SUCCESS);
    }
    for r in &repos {
        match &r.workdir {
            Some(wd) => println!("{}", wd),
            None => println!("{}", r.git_dir),
        }
    }
    println!("{} repo(s)", repos.len());
    Ok(ExitCode::SUCCESS)
}

/// `git zreindex [<path>...]` — (re)crawl for git repos and refresh the index.
/// With no argument, crawls the configured roots (`[zvcs] crawlroots`, else
/// `$HOME`); with paths, crawls exactly those.
pub fn zreindex(args: &[String]) -> Result<ExitCode> {
    let roots: Vec<std::path::PathBuf> = if args.is_empty() {
        crate::crawler::configured_roots()
    } else {
        args.iter().map(std::path::PathBuf::from).collect()
    };
    let n = crate::crawler::crawl_into_db(&roots)?;
    println!("indexed {n} repo(s)");
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

/// `git zjob <id>` — show one job in full (state, exit, output, resulting sha).
pub fn zjob(args: &[String]) -> Result<ExitCode> {
    let id: i64 = match args.first().and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => anyhow::bail!("usage: git zjob <id>"),
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
