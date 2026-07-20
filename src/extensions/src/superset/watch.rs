//! Reactive, file-watcher-driven autonomy **and** hooks — triggered by local ref
//! changes, never by a timer or a remote poll.
//!
//! A `git commit`/`git pull` rewrites a repo's `logs/HEAD` and `refs/*`;
//! `notify` fires; the daemon reacts. Two kinds of reaction, both coalesced over
//! a debounce window:
//!
//!   * **autonomy** (working tree) — attach detached submodules, fetch-free
//!     reconcile, and forward-only autobump, when `[zvcs]` autonomy is enabled;
//!   * **hooks** (any indexed repo) — when `[zvcs] hook` is set, every repo in
//!     the ledger is watched and its per-repo hook runs on ref-change. Because
//!     all repos are indexed in the db, this is a filesystem-driven hook system
//!     with nothing installed in any `.git/hooks`.
//!
//! Reconcile here is the fetch-free [`reconcile_repo_local`] fast-forward to
//! whatever `origin/main` a prior local pull already fetched; the daemon never
//! contacts a remote.

use notify::{Event, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;

use crate::config::ZvcsConfig;

/// A watched repository: its git directory (watched) and working directory
/// (passed to hooks).
struct Target {
    git_dir: PathBuf,
    workdir: PathBuf,
}

/// Cap on watched repos, so a whole-device index can't exhaust inotify watches.
const MAX_WATCHED: usize = 1024;

/// Spawn the watch loop iff `[zvcs]` autonomy or a hook is configured.
pub fn spawn_if_configured() {
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    let cfg = ZvcsConfig::load(&repo);
    if !cfg.should_watch() {
        return;
    }
    thread::spawn(move || run(cfg));
}

fn run(cfg: ZvcsConfig) {
    // Converge autonomy on start (attach detached submodules, first bump pass).
    if cfg.any_autonomous() {
        react(&cfg);
    }

    let targets = build_targets(&cfg);

    // Populate status for every watched repo on start (instant `zstatus --all`).
    if cfg.autostatus {
        if let Ok(conn) = crate::db::open_rw() {
            for t in &targets {
                crate::superset::status::record(&conn, &t.git_dir, &t.workdir);
            }
        }
    }

    let (tx, rx) = mpsc::channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            println!("[zvcs watch] cannot create watcher: {e}");
            return;
        }
    };

    let mut watched = 0usize;
    for t in &targets {
        for sub in ["refs", "logs"] {
            let p = t.git_dir.join(sub);
            if p.exists() && watcher.watch(&p, RecursiveMode::Recursive).is_ok() {
                watched += 1;
            }
        }
    }
    println!(
        "[zvcs watch] watching {} repo(s) ({watched} ref tree(s)), hooks={}, debounce={:?}",
        targets.len(),
        cfg.hook.is_some(),
        cfg.interval
    );

    let debounce = cfg.interval;
    loop {
        let first = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => return,
        };
        let mut affected: HashSet<PathBuf> = HashSet::new();
        collect(&first, &targets, &mut affected);
        loop {
            match rx.recv_timeout(debounce) {
                Ok(ev) => collect(&ev, &targets, &mut affected),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        // Autonomy (working tree), then per-repo status + hooks for repos changed.
        if cfg.any_autonomous() {
            react(&cfg);
        }
        if cfg.autostatus {
            if let Ok(conn) = crate::db::open_rw() {
                for t in &targets {
                    if affected.contains(&t.git_dir) {
                        crate::superset::status::record(&conn, &t.git_dir, &t.workdir);
                    }
                }
            }
        }
        if cfg.hooks_enabled() {
            for t in &targets {
                // Skip the daemon's own bookkeeping (autobump/attach/reconcile) —
                // fire only on user/agent ref changes. (`zhook test` still fires
                // manually.) hooks::run reads each repo's own hook (no-op if none).
                if affected.contains(&t.git_dir)
                    && !crate::superset::oplog::head_authored_by_zvcs(&t.git_dir)
                {
                    crate::superset::hooks::run(&t.git_dir, &t.workdir);
                }
            }
        }
    }
}

/// Record which watched repos an event touched (its paths live under a repo's
/// git dir).
fn collect(ev: &notify::Result<Event>, targets: &[Target], affected: &mut HashSet<PathBuf>) {
    let Ok(ev) = ev else { return };
    for path in &ev.paths {
        for t in targets {
            if path.starts_with(&t.git_dir) {
                affected.insert(t.git_dir.clone());
            }
        }
    }
}

/// The repos to watch: the working repo's submodules (for autonomy) plus, when a
/// hook is configured, every indexed repo (for hooks). Deduped by git dir and
/// capped at [`MAX_WATCHED`].
fn build_targets(cfg: &ZvcsConfig) -> Vec<Target> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut targets: Vec<Target> = Vec::new();

    // Working repo submodules (autonomy is keyed off their HEAD moves).
    if let Ok(repo) = gix::discover(".") {
        if let Ok(Some(subs)) = repo.submodules() {
            for sm in subs {
                if let Ok(Some(sub)) = sm.open() {
                    if let Some(wd) = sub.workdir() {
                        add_target(&mut seen, &mut targets, sub.git_dir().to_path_buf(), wd.to_path_buf());
                    }
                }
            }
        }
    }

    // Every indexed repo, for hooks and/or status maintenance.
    if cfg.watch_all_repos() {
        if let Ok(conn) = crate::db::open_ro() {
            if let Ok(repos) = crate::db::list_repos(&conn) {
                for r in repos {
                    let wd = r
                        .workdir
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(&r.git_dir));
                    add_target(&mut seen, &mut targets, PathBuf::from(r.git_dir), wd);
                    if targets.len() >= MAX_WATCHED {
                        println!(
                            "[zvcs watch] capped at {MAX_WATCHED} watched repos; \
                             narrow `zvcs.crawlroots` to watch fewer"
                        );
                        break;
                    }
                }
            }
        }
    }

    targets
}

/// Add a repo to the watch set, canonicalizing and deduping by git dir.
/// Both paths are canonicalized so the daemon's `status::record`/`upsert_repo`
/// stores the same canonical `workdir` other verbs (claims, selector) key on.
fn add_target(seen: &mut HashSet<PathBuf>, targets: &mut Vec<Target>, git_dir: PathBuf, workdir: PathBuf) {
    let git_dir = git_dir.canonicalize().unwrap_or(git_dir);
    let workdir = workdir.canonicalize().unwrap_or(workdir);
    if seen.insert(git_dir.clone()) {
        targets.push(Target { git_dir, workdir });
    }
}

/// One coalesced autonomy reaction: attach, then (config-gated) reconcile-local
/// and autobump. Re-opens the repo so it always sees current state.
fn react(cfg: &ZvcsConfig) {
    let Ok(repo) = gix::discover(".") else {
        return;
    };

    attach_all(&repo);

    if cfg.autoreconcile {
        if let Ok(Some(subs)) = repo.submodules() {
            for sm in subs {
                if let Ok(Some(sub)) = sm.open() {
                    if let Err(e) = crate::superset::reconcile_repo_local(&sub) {
                        let path = sm.path().map(|p| p.to_string()).unwrap_or_default();
                        println!("[zvcs reconcile] {path}: error: {e:#}");
                        let _ = crate::db::record_failure(
                            sub.git_dir(),
                            "reconcile",
                            &format!("{path}: {e:#}"),
                        );
                    }
                }
            }
        }
    }

    if cfg.autobump {
        match crate::superset::zbump_run(&[]) {
            Ok(outcome) => {
                for (path, reason) in &outcome.refusals {
                    let _ = crate::db::record_failure(
                        repo.git_dir(),
                        "autobump",
                        &format!("{path}: {reason}"),
                    );
                }
            }
            Err(e) => {
                println!("[zvcs autobump] error: {e:#}");
                let _ = crate::db::record_failure(repo.git_dir(), "autobump", &format!("{e:#}"));
            }
        }
    }
}

/// Attach the top repo and every initialized submodule off any detached HEAD.
fn attach_all(repo: &gix::Repository) {
    let _ = crate::superset::ensure_attached(repo);
    if let Ok(Some(subs)) = repo.submodules() {
        for sm in subs {
            if let Ok(Some(sub)) = sm.open() {
                let _ = crate::superset::ensure_attached(&sub);
            }
        }
    }
}
