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
use ignore::{WalkBuilder, WalkState};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Discover repos under `roots`. Returns `(git_dir, workdir)` pairs.
///
/// The walk is parallel and bounded: a whole-device scan (`git zreindex /`)
/// touches millions of inodes, and it must never wander into the mount points
/// that turn `/` into a hanging or bottomless walk (see [`is_skip_mount`]).
pub fn crawl(roots: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let found = Mutex::new(Vec::new());
    // Cap threads so a burst crawl can't saturate the box; `ignore` fans the walk
    // across them, cutting a whole-disk scan from minutes to seconds.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4);

    for root in roots {
        let mut wb = WalkBuilder::new(root);
        // Traverse everything (hidden dirs included) but respect no ignore files —
        // we want every repo on disk, not just non-ignored ones.
        wb.standard_filters(false).follow_links(false).threads(threads);
        // Prune whole subtrees we must never descend into: `.git` (repos are
        // detected via the parent) and the pseudo/auto/network mounts that hang or
        // loop a root scan (`is_skip_mount`). `filter_entry` returning false skips
        // the directory AND everything under it.
        wb.filter_entry(|e| e.file_name() != ".git" && !is_skip_mount(e.path()));

        wb.build_parallel().run(|| {
            let found = &found;
            Box::new(move |result| {
                let Ok(entry) = result else {
                    return WalkState::Continue; // permission-denied / transient: skip
                };
                if !entry.file_type().map_or(false, |t| t.is_dir()) {
                    return WalkState::Continue;
                }
                let dir = entry.path();
                let dotgit = dir.join(".git");
                if !dotgit.exists() {
                    return WalkState::Continue;
                }
                if let Some(git_dir) = resolve_git_dir(dir, &dotgit) {
                    found.lock().unwrap().push((git_dir, dir.to_path_buf()));
                }
                WalkState::Continue
            })
        });
    }
    found.into_inner().unwrap()
}

/// Directories that turn a whole-device crawl (`git zreindex /`) into a hang or an
/// unbounded walk, and never hold a real repo: kernel pseudo-filesystems,
/// automounted/network mount roots (a dead NFS/SMB mount blocks in the kernel on
/// `readdir`), and the macOS firmlink reflection of the data volume (which would
/// otherwise be walked a second time). Matched only at their canonical absolute
/// paths, so a crawl rooted under `$HOME` never trips on them.
fn is_skip_mount(path: &Path) -> bool {
    // `/home` is autofs on macOS (skip) but the real user tree on Linux (keep),
    // so the list is per-OS rather than shared.
    #[cfg(target_os = "macos")]
    const SKIP: &[&str] = &[
        "/dev",
        "/Volumes",
        "/System/Volumes",
        "/net",
        "/home",
        "/.vol",
        "/private/var/vm",
    ];
    #[cfg(target_os = "linux")]
    const SKIP: &[&str] = &["/proc", "/sys", "/dev", "/run"];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    const SKIP: &[&str] = &["/proc", "/sys", "/dev"];

    SKIP.iter().any(|s| path == Path::new(s))
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

#[cfg(test)]
mod tests {
    use super::is_skip_mount;
    use std::path::Path;

    #[test]
    fn skips_hanging_mounts_but_keeps_repo_trees() {
        // Pseudo/device filesystems are pruned on every platform.
        assert!(is_skip_mount(Path::new("/dev")));
        // Real repo locations must never be pruned, or a `/` scan finds nothing.
        assert!(!is_skip_mount(Path::new("/Users")));
        assert!(!is_skip_mount(Path::new("/Users/someone/project")));
        // The prune is anchored to the canonical absolute path, so a same-named
        // directory nested under a crawl root is still walked.
        assert!(!is_skip_mount(Path::new("/Users/someone/dev")));

        // `/home` is the crux of the per-OS split: autofs on macOS (prune, or the
        // scan hangs), the real user tree on Linux (keep, or every repo is missed).
        #[cfg(target_os = "macos")]
        {
            assert!(is_skip_mount(Path::new("/Volumes")));
            assert!(is_skip_mount(Path::new("/System/Volumes")));
            assert!(is_skip_mount(Path::new("/home")));
        }
        #[cfg(target_os = "linux")]
        {
            assert!(is_skip_mount(Path::new("/proc")));
            assert!(!is_skip_mount(Path::new("/home")));
        }
    }
}
