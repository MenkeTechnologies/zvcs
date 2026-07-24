//! `git zcontend` — live agent-vs-agent contention across the tree.
//!
//! Three reads that together answer "who is stepping on whom": the
//! session-attributed claims (advisory leases), the per-repo job backlog (the
//! daemon FIFO's queued/running depth), and the intersection — repos that are
//! both claimed and have jobs stacked behind them, i.e. actively contested. All
//! from the shared db; read-only.

use anyhow::Result;
use std::collections::HashMap;
use std::process::ExitCode;

pub fn zcontend(_args: &[String]) -> Result<ExitCode> {
    let conn = crate::db::open_rw()?;
    let claims = crate::db::list_claims(&conn)?;
    let backlog = crate::db::contention(&conn)?;

    println!("claims ({}):", claims.len());
    if claims.is_empty() {
        println!("  (none)");
    }
    for (path, session, _) in &claims {
        println!("  {session}  ⇒  {path}");
    }

    println!("\nbacklog ({} repo(s) with queued/running jobs):", backlog.len());
    if backlog.is_empty() {
        println!("  (idle)");
    }
    for (path, q, r) in &backlog {
        println!("  {q:>3} queued  {r:>2} running   {path}");
    }

    // Contested = claimed AND has a backlog: another agent's work is waiting on a
    // repo someone holds. Both queries key paths on COALESCE(workdir, git_dir), so
    // they compare directly.
    let held: HashMap<&str, &str> = claims.iter().map(|(p, s, _)| (p.as_str(), s.as_str())).collect();
    let contested: Vec<(&String, &&str, i64)> = backlog
        .iter()
        .filter_map(|(p, q, r)| held.get(p.as_str()).map(|s| (p, s, q + r)))
        .collect();
    if !contested.is_empty() {
        println!("\ncontested (claimed + queued):");
        for (path, holder, n) in contested {
            println!("  {path}  held by {holder}, {n} job(s) waiting/running");
        }
    }
    Ok(ExitCode::SUCCESS)
}
