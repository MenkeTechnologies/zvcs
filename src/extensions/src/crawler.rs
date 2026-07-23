//! Repo crawler: discover git repositories under a set of roots and record them
//! in the `repos` index (`db`). This backs "index all git repos on the storage
//! device": crawl roots default to `$HOME` (override with `[zvcs] crawlroots`),
//! and `git zreindex [path]` forces a rescan.
//!
//! The walk uses the `ignore` crate for speed and never descends into a `.git`
//! directory (its object/ref trees are large and internal); repositories are
//! detected by a `.git` child on each visited directory, so both normal repos
//! (`.git` dir) and submodule/worktree checkouts (`.git` file) are found.
//! Permission-denied paths are skipped.

use anyhow::Result;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Discover repos under `roots`. Returns `(git_dir, workdir)` pairs.
pub fn crawl(roots: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let mut found = Vec::new();
    for root in roots {
        let mut wb = WalkBuilder::new(root);
        // Traverse everything (hidden dirs included) but respect no ignore files —
        // we want every repo on disk, not just non-ignored ones.
        wb.standard_filters(false).follow_links(false);
        // Never descend into (or yield) `.git`; repos are detected via the parent.
        wb.filter_entry(|e| e.file_name() != ".git");

        for entry in wb.build().flatten() {
            let is_dir = entry.file_type().map_or(false, |t| t.is_dir());
            if !is_dir {
                continue;
            }
            let dir = entry.path();
            let dotgit = dir.join(".git");
            if !dotgit.exists() {
                continue;
            }
            if let Some(git_dir) = resolve_git_dir(dir, &dotgit) {
                found.push((git_dir, dir.to_path_buf()));
            }
        }
    }
    found
}

/// Resolve the actual git directory for a worktree `dir` whose `.git` is at
/// `dotgit`: the directory itself for a normal repo, or the `gitdir:` target for
/// a submodule/worktree `.git` file.
fn resolve_git_dir(dir: &Path, dotgit: &Path) -> Option<PathBuf> {
    if dotgit.is_dir() {
        return dotgit.canonicalize().ok().or_else(|| Some(dotgit.to_path_buf()));
    }
    // `.git` file: `gitdir: <path>` (path may be relative to the worktree).
    let content = std::fs::read_to_string(dotgit).ok()?;
    let rest = content.trim().strip_prefix("gitdir:")?.trim();
    let p = Path::new(rest);
    let abs = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
    abs.canonicalize().ok().or(Some(abs))
}

/// Crawl `roots` and upsert every discovered repo into the index.
/// Returns the number of repos recorded.
pub fn crawl_into_db(roots: &[PathBuf]) -> Result<usize> {
    let repos = crawl(roots);
    let conn = crate::db::open_rw()?;
    for (git_dir, workdir) in &repos {
        crate::db::upsert_repo(&conn, git_dir, Some(workdir))?;
    }
    Ok(repos.len())
}

/// Spawn a one-shot background crawl of the configured roots on daemon start,
/// iff `[zvcs] autocrawl` is enabled. A no-op otherwise (a whole-device scan is
/// opt-in; `git zreindex` triggers it on demand regardless).
pub fn spawn_if_configured() {
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    if !crate::config::ZvcsConfig::load(&repo).autocrawl {
        return;
    }
    std::thread::spawn(|| {
        let roots = configured_roots();
        match crawl_into_db(&roots) {
            Ok(n) => log_line(&format!("[zvcs crawl] indexed {n} repo(s)")),
            Err(e) => log_line(&format!("[zvcs crawl] error: {e:#}")),
        }
    });
}

/// Append one line to the singleton daemon log (`$ZVCS_HOME/zvcs.log`). The crawl
/// runs on a detached thread, so its result must never touch the terminal — a
/// foreground daemon still inherits stdout. Writing the log directly (rather than
/// `println!`) keeps this chatter log-only no matter how the daemon was launched.
fn log_line(msg: &str) {
    use std::io::Write;
    let path = crate::superset::zdaemon::zvcs_home().join("zvcs.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

/// The configured crawl roots: `[zvcs] crawlroots` (whitespace/comma separated),
/// else `$HOME`, else the current directory.
pub fn configured_roots() -> Vec<PathBuf> {
    if let Ok(repo) = gix::discover(".") {
        let roots = crate::config::ZvcsConfig::load(&repo).crawlroots;
        if !roots.is_empty() {
            return roots;
        }
    }
    match std::env::var_os("HOME") {
        Some(h) => vec![PathBuf::from(h)],
        None => vec![PathBuf::from(".")],
    }
}
