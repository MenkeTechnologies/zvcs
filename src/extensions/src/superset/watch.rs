//! Reactive, file-watcher-driven autonomy — the daemon's autobump / attach /
//! reconcile passes, triggered by **local** ref changes and never by a timer or
//! a remote poll.
//!
//! A `git commit`/`git pull` inside a submodule rewrites that submodule's
//! `logs/HEAD` and `refs/*`; `notify` fires; the daemon reacts. It never contacts
//! a remote — reconcile here is the fetch-free ([`reconcile_repo_local`]) fast
//! forward to whatever `origin/main` a prior local pull already fetched.
//!
//! What each reaction does, coalesced over a debounce window:
//!   * **attach** every submodule off any detached HEAD ([`ensure_attached`]) —
//!     ends the stash/attach/pop dance;
//!   * **autobump** (if `[zvcs] autobump`) — forward-only local pointer bumps
//!     committed into the parent, clearing the `(new commits)` markers;
//!   * **reconcile-local** (if `[zvcs] autoreconcile`) — fetch-free ff of each
//!     clean submodule to its already-present `origin/main`.

use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;

use crate::config::ZvcsConfig;

/// Spawn the watch loop iff the discovered repo has `[zvcs]` autonomy enabled.
/// Runs entirely on a background thread; a no-op otherwise.
pub fn spawn_if_configured() {
    let Ok(repo) = gix::discover(".") else {
        return;
    };
    let cfg = ZvcsConfig::load(&repo);
    if !cfg.any_autonomous() {
        return;
    }
    thread::spawn(move || run(cfg));
}

fn run(cfg: ZvcsConfig) {
    // 1. Converge on start: attach every submodule off detached HEAD, then a
    //    first autobump/reconcile pass so current state is consistent before any
    //    event arrives (a detached-but-current submodule generates no events).
    react(&cfg);

    // 2. Watch each submodule's ref/reflog trees for local changes.
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
    for dir in watch_dirs() {
        if watcher.watch(&dir, RecursiveMode::Recursive).is_ok() {
            watched += 1;
        }
    }
    println!("[zvcs watch] watching {watched} ref tree(s), debounce={:?}", cfg.interval);

    // 3. Debounced event loop: block for an event, drain the burst until a quiet
    //    gap of `interval`, then act once (coalescing).
    let debounce = cfg.interval;
    loop {
        if rx.recv().is_err() {
            return; // watcher dropped
        }
        loop {
            match rx.recv_timeout(debounce) {
                Ok(_) => continue,               // more events in the burst
                Err(RecvTimeoutError::Timeout) => break, // quiet -> act
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        react(&cfg);
    }
}

/// One coalesced reaction: attach, then (config-gated) reconcile-local and
/// autobump. Re-opens the repo so it always sees current state.
fn react(cfg: &ZvcsConfig) {
    let Ok(repo) = gix::discover(".") else {
        return;
    };

    // Attach every submodule (+ top) off any detached HEAD. Local, no-clobber.
    attach_all(&repo);

    // Fetch-free ff of each clean submodule to its already-present origin/main.
    if cfg.autoreconcile {
        if let Ok(Some(subs)) = repo.submodules() {
            for sm in subs {
                if let Ok(Some(sub)) = sm.open() {
                    if let Err(e) = crate::superset::reconcile_repo_local(&sub) {
                        let path = sm.path().map(|p| p.to_string()).unwrap_or_default();
                        println!("[zvcs reconcile] {path}: error: {e:#}");
                    }
                }
            }
        }
    }

    // Forward-only local pointer bumps, committed (clears the markers).
    if cfg.autobump {
        if let Err(e) = crate::superset::zbump(&[]) {
            println!("[zvcs autobump] error: {e:#}");
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

/// Ref/reflog directories to watch: each **submodule's** `refs/` and `logs/`.
///
/// The top repo is deliberately not watched: autobump commits into it, and
/// watching it would re-trigger the reaction on the daemon's own writes.
/// Reactions are keyed off submodule HEAD moves (a bot committing/pulling).
fn watch_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(repo) = gix::discover(".") else {
        return out;
    };
    if let Ok(Some(subs)) = repo.submodules() {
        for sm in subs {
            if let Ok(Some(sub)) = sm.open() {
                let gd = sub.git_dir();
                for sub_dir in ["refs", "logs"] {
                    let p = gd.join(sub_dir);
                    if p.exists() {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}
