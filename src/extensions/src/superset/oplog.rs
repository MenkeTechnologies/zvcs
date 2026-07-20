//! Cross-repo op ledger + rewind: `zlog` and `zundo`.
//!
//! `zlog` merges the `HEAD` reflogs of every indexed repo into one machine-wide,
//! time-ordered timeline — "what moved, where, when" across the whole tree, which
//! git's per-repo reflog can't show. `zundo` rewinds a repo one step: it reads the
//! previous `HEAD` from the reflog and `reset --hard`s to it (reusing the faithful
//! porcelain reset), refusing on a dirty worktree so no work is clobbered.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// One reflog entry.
struct Entry {
    time: i64,
    old: String,
    new: String,
    msg: String,
}

/// Parse a `.git/logs/HEAD` line: `OLD NEW IDENT... UNIXTIME TZ\tMESSAGE`.
fn parse_line(line: &str) -> Option<Entry> {
    let (header, msg) = line.split_once('\t')?;
    let toks: Vec<&str> = header.split_whitespace().collect();
    if toks.len() < 4 {
        return None;
    }
    Some(Entry {
        old: toks[0].to_string(),
        new: toks[1].to_string(),
        time: toks[toks.len() - 2].parse().ok()?,
        msg: msg.to_string(),
    })
}

/// Read a repo's HEAD reflog (oldest→newest), empty if none.
fn read_head_reflog(git_dir: &Path) -> Vec<Entry> {
    match std::fs::read_to_string(git_dir.join("logs/HEAD")) {
        Ok(c) => c.lines().filter_map(parse_line).collect(),
        Err(_) => Vec::new(),
    }
}

/// The most recent HEAD event as `(old_sha, new_sha, kind)`, where `kind` is the
/// operation typed from the reflog message (`commit`, `checkout`, `merge`,
/// `pull`, `rebase`, `reset`, `clone`, …). Used to give hooks a typed event.
pub(crate) fn latest_head_event(git_dir: &Path) -> Option<(String, String, String)> {
    let entries = read_head_reflog(git_dir);
    let e = entries.last()?;
    let kind = e
        .msg
        .split([':', ' ', '('])
        .next()
        .unwrap_or("")
        .to_lowercase();
    let kind = if kind.is_empty() { "ref-change".to_string() } else { kind };
    Some((e.old.clone(), e.new.clone(), kind))
}

/// True if the most recent HEAD change was authored by zvcs itself (autobump,
/// attach, or zsync reconcile), so hooks don't fire on the daemon's own
/// bookkeeping commits/ref-updates.
pub(crate) fn head_authored_by_zvcs(git_dir: &Path) -> bool {
    match read_head_reflog(git_dir).last() {
        Some(e) => e.msg.contains("zvcs") || e.msg.contains("zsync"),
        None => false,
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// `git zlog [-n N]` — machine-wide reflog timeline across all indexed repos
/// (newest first). Pipe-clean: `<unixtime>\t<repo>\t<old>..<new>\t<message>`.
pub fn zlog(args: &[String]) -> Result<ExitCode> {
    let mut n: usize = 30;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-n" {
            i += 1;
            n = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(n);
        }
        i += 1;
    }

    // Scope: every indexed repo; fall back to the cwd repo if none indexed.
    let repos = indexed_repos().unwrap_or_default();
    let repos = if repos.is_empty() {
        match gix::discover(".") {
            Ok(r) => vec![(
                r.git_dir().to_path_buf(),
                r.workdir().unwrap_or_else(|| r.git_dir()).to_path_buf(),
            )],
            Err(_) => return Ok(ExitCode::SUCCESS),
        }
    } else {
        repos
    };

    let mut all: Vec<(String, Entry)> = Vec::new();
    for (git_dir, workdir) in &repos {
        let label = workdir.to_string_lossy().into_owned();
        for e in read_head_reflog(git_dir) {
            all.push((label.clone(), e));
        }
    }
    all.sort_by(|a, b| b.1.time.cmp(&a.1.time));
    for (repo, e) in all.iter().take(n) {
        println!("{}\t{}\t{}..{}\t{}", e.time, repo, short(&e.old), short(&e.new), e.msg);
    }
    Ok(ExitCode::SUCCESS)
}

/// Indexed repos as `(git_dir, workdir)`, or `None` if the ledger is unavailable.
fn indexed_repos() -> Option<Vec<(PathBuf, PathBuf)>> {
    let conn = crate::db::open_ro().ok()?;
    let repos = crate::db::list_repos(&conn).ok()?;
    Some(
        repos
            .into_iter()
            .map(|r| {
                let wd = r.workdir.clone().unwrap_or_else(|| r.git_dir.clone());
                (PathBuf::from(r.git_dir), PathBuf::from(wd))
            })
            .collect(),
    )
}

/// `git zundo [<path>]` — rewind a repo one reflog step (reset --hard to the
/// previous HEAD). Refuses on a dirty worktree.
pub fn zundo(args: &[String]) -> Result<ExitCode> {
    let at = args.iter().find(|a| !a.starts_with('-')).map(PathBuf::from);
    let repo = match at {
        Some(p) => gix::discover(p)?,
        None => gix::discover(".")?,
    };
    if repo.is_dirty()? {
        anyhow::bail!("worktree is dirty; commit or stash before undo");
    }
    let entries = read_head_reflog(repo.git_dir());
    let last = entries.last().ok_or_else(|| anyhow!("nothing to undo (no reflog)"))?;
    if last.old.chars().all(|c| c == '0') {
        anyhow::bail!("nothing to undo (already at the initial commit)");
    }
    let prev = last.old.clone();
    let msg = last.msg.clone();
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("a working tree is required"))?
        .to_path_buf();

    // Reuse the faithful porcelain reset (moves ref + index + worktree, reflogged
    // so the undo is itself undoable).
    let exe = std::env::current_exe().map_err(|e| anyhow!("cannot resolve exe: {e}"))?;
    let status = Command::new(exe)
        .args(["reset", "--hard", &prev])
        .current_dir(&workdir)
        .status()
        .map_err(|e| anyhow!("reset failed to run: {e}"))?;
    if !status.success() {
        anyhow::bail!("reset --hard {} failed", short(&prev));
    }
    println!("undid \"{}\" — now at {}", msg, short(&prev));
    Ok(ExitCode::SUCCESS)
}
