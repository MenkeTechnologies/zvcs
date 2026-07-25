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
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Discover repos under `roots`. Returns `(git_dir, workdir)` pairs.
///
/// The walk is parallel and bounded: a whole-device scan (`git zreindex /`)
/// touches millions of inodes, and it must never wander into the mount points
/// that turn `/` into a hanging or bottomless walk (see [`is_skip_mount`]).
///
/// Progress streams to the singleton log as it goes — a start line, every repo
/// as it is found, and a final count — so `git zdaemon log -f` shows a live
/// crawl instead of a silent wait followed by one summary line at the end.
pub fn crawl(roots: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let found = Mutex::new(Vec::new());
    // Cap threads so a burst crawl can't saturate the box; `ignore` fans the walk
    // across them, cutting a whole-disk scan from minutes to seconds.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4);
    // One log handle for the whole walk, shared across the walk threads (opened
    // once, not per line, to keep syscalls off the hot path). `File` is unbuffered,
    // so each line is a single append that `tail -f`/`zdaemon log -f` sees at once.
    let log = Mutex::new(open_log());

    log_progress(&log, &format!("[zvcs crawl] scanning {} root(s)", roots.len()));
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
            let log = &log;
            Box::new(move |result| {
                let Ok(entry) = result else {
                    return WalkState::Continue; // permission-denied / transient: skip
                };
                if !entry.file_type().is_some_and(|t| t.is_dir()) {
                    return WalkState::Continue;
                }
                let dir = entry.path();
                let dotgit = dir.join(".git");
                if !dotgit.exists() {
                    return WalkState::Continue;
                }
                if let Some(git_dir) = resolve_git_dir(dir, &dotgit) {
                    log_progress(log, &format!("[zvcs crawl] + {}", dir.display()));
                    found.lock().unwrap().push((git_dir, dir.to_path_buf()));
                }
                WalkState::Continue
            })
        });
    }
    let out = found.into_inner().unwrap();
    log_progress(&log, &format!("[zvcs crawl] walk done: {} repo(s)", out.len()));
    out
}

/// Open the singleton daemon log (`$ZVCS_HOME/zvcs.log`) for append. `None` if it
/// cannot be opened (progress logging then silently no-ops — never fatal).
fn open_log() -> Option<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::superset::zdaemon::zvcs_home().join("zvcs.log"))
        .ok()
}

/// Append one progress line to the shared log handle, if the log opened. Holding
/// the mutex over the write keeps concurrent walk threads' lines from interleaving.
fn log_progress(log: &Mutex<Option<File>>, msg: &str) {
    if let Ok(mut guard) = log.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{msg}");
        }
    }
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

/// Spawn the daemon's autocrawl on start: an initial crawl of the configured roots
/// iff `[zvcs] autocrawl` is enabled right now, plus a watcher that re-crawls when
/// the resolved roots later change.
///
/// The watcher exists because a daemon can come up *mid-configuration*: the
/// coordinator autostarts on a `git` command, so a daemon can read the `[zvcs]`
/// section after `autocrawl` is set but before `crawlroots` is written (or before
/// either is), then hold the singleton — leaving a stale first daemon whose crawl
/// used the wrong (or empty → `$HOME`) roots and no second daemon able to start.
/// Reacting to the config change closes that race: whichever daemon is up re-runs
/// the crawl with the current config, so the configured roots always get indexed.
/// The re-read uses the absolute git dir captured here, never `discover(".")`, so
/// it is independent of the daemon's working directory.
pub fn spawn_if_configured() {
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    let git_dir = repo.git_dir();
    let git_dir = git_dir.canonicalize().unwrap_or_else(|_| git_dir.to_path_buf());

    // Initial crawl from the config as it stands at daemon start.
    let cfg = crate::config::ZvcsConfig::load(&repo);
    let initial = cfg.autocrawl.then(|| {
        let roots = roots_from_config(&cfg);
        spawn_crawl(roots.clone(), "indexed");
        roots
    });

    std::thread::spawn(move || watch_config_and_recrawl(git_dir, initial));
}

/// The crawl roots for a config: `[zvcs] crawlroots` if set, else `$HOME`, else `.`.
fn roots_from_config(cfg: &crate::config::ZvcsConfig) -> Vec<PathBuf> {
    if !cfg.crawlroots.is_empty() {
        return cfg.crawlroots.clone();
    }
    match std::env::var_os("HOME") {
        Some(h) => vec![PathBuf::from(h)],
        None => vec![PathBuf::from(".")],
    }
}

/// Crawl `roots` into the index on a detached thread, logging the outcome under
/// `verb` (`indexed`/`reindexed`).
fn spawn_crawl(roots: Vec<PathBuf>, verb: &'static str) {
    std::thread::spawn(move || match crawl_into_db(&roots) {
        Ok(n) => log_line(&format!("[zvcs crawl] {verb} {n} repo(s)")),
        Err(e) => log_line(&format!("[zvcs crawl] error: {e:#}")),
    });
}

/// Watch the repo's `config` for the daemon's life and re-crawl whenever the
/// resolved autocrawl roots change (autocrawl enabled, or `crawlroots` edited).
/// Config is written by temp-file + rename, so watch the git dir and filter for the
/// `config` entry rather than watching the file (which the rename would orphan).
fn watch_config_and_recrawl(git_dir: PathBuf, initial_roots: Option<Vec<PathBuf>>) {
    use notify::{RecursiveMode, Watcher};
    let (tx, rx) = std::sync::mpsc::channel();
    let Ok(mut watcher) = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) else {
        return;
    };
    if watcher.watch(&git_dir, RecursiveMode::NonRecursive).is_err() {
        return;
    }
    let mut last_roots = initial_roots;
    while let Ok(res) = rx.recv() {
        let Ok(event) = res else { continue };
        if !event
            .paths
            .iter()
            .any(|p| p.file_name() == Some(std::ffi::OsStr::new("config")))
        {
            continue;
        }
        // Re-open by the captured git dir (cwd-independent) and re-read config.
        let Ok(repo) = gix::open(&git_dir) else { continue };
        let cfg = crate::config::ZvcsConfig::load(&repo);
        if !cfg.autocrawl {
            last_roots = None;
            continue;
        }
        let roots = roots_from_config(&cfg);
        if last_roots.as_ref() == Some(&roots) {
            continue; // roots unchanged — an unrelated config edit
        }
        last_roots = Some(roots.clone());
        match crawl_into_db(&roots) {
            Ok(n) => log_line(&format!("[zvcs crawl] reindexed {n} repo(s) after config change")),
            Err(e) => log_line(&format!("[zvcs crawl] error: {e:#}")),
        }
    }
}

/// Append one line to the singleton daemon log (`$ZVCS_HOME/zvcs.log`). The crawl
/// runs on a detached thread, so its result must never touch the terminal — a
/// foreground daemon still inherits stdout. Writing the log directly (rather than
/// `println!`) keeps this chatter log-only no matter how the daemon was launched.
fn log_line(msg: &str) {
    let path = crate::superset::zdaemon::zvcs_home().join("zvcs.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

/// The configured crawl roots: `[zvcs] crawlroots` (whitespace/comma separated),
/// else `$HOME`, else the current directory.
pub fn configured_roots() -> Vec<PathBuf> {
    match gix::discover(".") {
        Ok(repo) => roots_from_config(&crate::config::ZvcsConfig::load(&repo)),
        // No repo → the `$HOME`/`.` fallback `roots_from_config` would apply anyway.
        Err(_) => roots_from_config(&crate::config::ZvcsConfig::default()),
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
