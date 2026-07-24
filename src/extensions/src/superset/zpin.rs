//! `git zpin [<path>...|list]` / `git zunpin [<path>...|--all]` — freeze repos
//! from daemon autonomy.
//!
//! A pinned repo is refused by the daemon's autobump and reconcile: its pointer
//! will not move on its own until unpinned. Selective, per-repo — the rest of the
//! tree stays autonomous. The flag lives in `repos.pinned`, so it survives daemon
//! restarts and is visible to every session.
//!
//!   git zpin                  pin the repo at the cwd
//!   git zpin <path>...        pin those repos
//!   git zpin list             list every pinned repo
//!   git zunpin [<path>...]    unpin (cwd if no path)
//!   git zunpin --all          unpin everything

use anyhow::Result;
use std::path::PathBuf;
use std::process::ExitCode;

/// Canonical git_dir + workdir for the repo discovered at `path`.
fn discover(path: &str) -> Result<(PathBuf, Option<PathBuf>)> {
    let repo = gix::discover(path)?;
    let gd = repo.git_dir();
    let gd = gd.canonicalize().unwrap_or_else(|_| gd.to_path_buf());
    let wd = repo.workdir().map(|w| w.canonicalize().unwrap_or_else(|_| w.to_path_buf()));
    Ok((gd, wd))
}

pub fn zpin(args: &[String]) -> Result<ExitCode> {
    let conn = crate::db::open_rw()?;
    if args.first().map(|s| s.as_str()) == Some("list") {
        let pins = crate::db::list_pins(&conn)?;
        if pins.is_empty() {
            println!("no pinned repos");
        }
        for (gd, wd) in pins {
            println!("{}", wd.unwrap_or(gd));
        }
        return Ok(ExitCode::SUCCESS);
    }
    set_all(&conn, args, true)
}

pub fn zunpin(args: &[String]) -> Result<ExitCode> {
    let conn = crate::db::open_rw()?;
    if args.iter().any(|a| a == "--all") {
        for (gd, _) in crate::db::list_pins(&conn)? {
            let _ = crate::db::set_pin(&conn, std::path::Path::new(&gd), false);
        }
        println!("unpinned all");
        return Ok(ExitCode::SUCCESS);
    }
    set_all(&conn, args, false)
}

fn set_all(conn: &rusqlite::Connection, args: &[String], pin: bool) -> Result<ExitCode> {
    let verb = if pin { "zpin" } else { "zunpin" };
    let targets: Vec<String> = if args.is_empty() { vec![".".into()] } else { args.to_vec() };
    for t in &targets {
        let Ok((gd, wd)) = discover(t) else {
            eprintln!("zvcs: {verb}: {t}: not a git repository");
            continue;
        };
        // Ensure the repo is indexed so the flag has a row to land on.
        let _ = crate::db::upsert_repo(conn, &gd, wd.as_deref());
        crate::db::set_pin(conn, &gd, pin)?;
        let shown = wd.unwrap_or(gd);
        println!("{} {}", if pin { "pinned" } else { "unpinned" }, shown.display());
    }
    Ok(ExitCode::SUCCESS)
}
