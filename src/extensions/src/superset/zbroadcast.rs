//! `git zbroadcast [--to <session>] [<msg>...]` and `git zhandoff <repo> <session>`
//! — inter-agent messaging over the shared db.
//!
//! With a message, zbroadcast posts it to every session (or one, with `--to`);
//! with no args it prints this session's unread messages and marks them read — a
//! pull inbox, so there is no per-command cost on the hot path. zhandoff reassigns
//! a repo's claim to another session and drops it a note.

use anyhow::{anyhow, Result};
use std::process::ExitCode;

pub fn zbroadcast(args: &[String]) -> Result<ExitCode> {
    let conn = crate::db::open_rw()?;
    let me = crate::session_key();

    // No args → read the inbox.
    if args.is_empty() {
        let unread = crate::db::unread_messages(&conn, &me)?;
        if unread.is_empty() {
            println!("no unread messages");
            return Ok(ExitCode::SUCCESS);
        }
        let ids: Vec<i64> = unread.iter().map(|(id, ..)| *id).collect();
        for (_, from, body, _) in &unread {
            println!("{}: {}", from.as_deref().unwrap_or("?"), body);
        }
        crate::db::mark_read(&conn, &ids, &me)?;
        return Ok(ExitCode::SUCCESS);
    }

    // `--to <session> <msg...>` targets one session; otherwise broadcast.
    let (to, rest): (Option<String>, &[String]) = if args[0] == "--to" {
        if args.len() < 3 {
            return Err(anyhow!("usage: git zbroadcast --to <session> <msg>"));
        }
        (Some(args[1].clone()), &args[2..])
    } else {
        (None, args)
    };
    let body = rest.join(" ");
    let id = crate::db::post_message(&conn, &me, to.as_deref(), &body)?;
    match to {
        Some(t) => println!("message #{id} sent to {t}"),
        None => println!("message #{id} broadcast"),
    }
    Ok(ExitCode::SUCCESS)
}

pub fn zhandoff(args: &[String]) -> Result<ExitCode> {
    if args.len() != 2 {
        return Err(anyhow!("usage: git zhandoff <repo-path> <session>"));
    }
    let (path, session) = (&args[0], &args[1]);
    let conn = crate::db::open_rw()?;
    let repo = gix::discover(path.as_str())?;
    let gd = repo.git_dir();
    let gd = gd.canonicalize().unwrap_or_else(|_| gd.to_path_buf());
    let wd = repo.workdir().map(|w| w.canonicalize().unwrap_or_else(|_| w.to_path_buf()));
    let repo_id = crate::db::upsert_repo(&conn, &gd, wd.as_deref())?;
    if !crate::db::transfer_claim(&conn, repo_id, session)? {
        return Err(anyhow!("{path}: not currently claimed — nothing to hand off"));
    }
    let me = crate::session_key();
    let shown = wd.clone().unwrap_or_else(|| gd.clone());
    let _ = crate::db::post_message(
        &conn,
        &me,
        Some(session),
        &format!("claim on {} handed to you by {me}", shown.display()),
    );
    println!("handed off {} to {session}", shown.display());
    Ok(ExitCode::SUCCESS)
}
