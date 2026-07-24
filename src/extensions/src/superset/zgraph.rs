//! `git zgraph` — fleet topology.
//!
//! The cross-repo relationship view git has no notion of: which local checkouts
//! are the SAME upstream repo (a dup group — same `origin` URL, N working trees),
//! which git no command can show because git only ever sees one repo. Reads every
//! indexed repo's origin; on-demand, so the per-repo config read is acceptable.

use anyhow::Result;
use std::collections::BTreeMap;
use std::process::ExitCode;

pub fn zgraph(_args: &[String]) -> Result<ExitCode> {
    let conn = crate::db::open_rw()?;
    let repos = crate::db::all_repos(&conn)?;

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut no_origin = 0usize;
    for (gd, wd) in &repos {
        let path = wd.clone().unwrap_or_else(|| gd.clone());
        match origin_url(gd) {
            Some(url) => groups.entry(url).or_default().push(path),
            None => no_origin += 1,
        }
    }

    let mut dups: Vec<(&String, &Vec<String>)> = groups.iter().filter(|(_, v)| v.len() > 1).collect();
    dups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));

    if dups.is_empty() {
        println!("no dup groups (every indexed repo has a distinct origin)");
    } else {
        println!("dup groups — same origin, multiple local checkouts:\n");
        for (url, members) in &dups {
            println!("  {url}  ({} checkouts)", members.len());
            for m in members.iter() {
                println!("    ├─ {m}");
            }
            println!();
        }
    }
    println!(
        "{} repos · {} distinct origins · {} dup group(s) · {} without an origin",
        repos.len(),
        groups.len(),
        dups.len(),
        no_origin
    );
    Ok(ExitCode::SUCCESS)
}

/// The `origin` fetch URL for the repo at `git_dir`, if it has one.
fn origin_url(git_dir: &str) -> Option<String> {
    let repo = gix::open(git_dir).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url(gix::remote::Direction::Fetch)?;
    Some(String::from_utf8_lossy(&url.to_bstring()).into_owned())
}
