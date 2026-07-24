//! Reactive, file-watcher-driven autonomy **and** hooks — triggered by local
//! filesystem changes, never by a timer or a remote poll.
//!
//! `notify` fires on changes under each watched path; the daemon reacts. Three
//! kinds of reaction, fired the instant an event arrives (no debounce):
//!
//!   * **directory triggers** (`git ztrigger <DIR> <cmd>`) — the general case:
//!     watch ANY directory recursively (repo or not) and run its command on any
//!     file change. Stored in the `triggers` table, keyed by path.
//!   * **repo hooks** — a repo with a local `zvcs.hook` is watched whole-dir and
//!     runs its own hook on any change; a *global* `zvcs.hook` watches every
//!     indexed repo on the lighter ref-change model.
//!   * **autonomy** (working tree) — attach detached submodules, fetch-free
//!     reconcile, forward-only autobump, when `[zvcs]` autonomy is enabled. Keyed
//!     off ref moves, so those repos watch only the `refs/`+`logs/` trees.
//!
//! Reconcile here is the fetch-free [`reconcile_repo_local`] fast-forward to
//! whatever `origin/main` a prior local pull already fetched; the daemon never
//! contacts a remote.

use notify::{Event, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ZvcsConfig;

/// A watched repository: its git directory and working directory (passed to
/// hooks).
struct Target {
    git_dir: PathBuf,
    workdir: PathBuf,
    /// Watched over its ENTIRE directory (worktree — and `.git` for a repo) so any
    /// file create/modify/delete fires, not just ref moves. Unarmed repos are
    /// watched only over `refs/`+`logs/` (autonomy/status).
    armed: bool,
    /// A directory trigger's command (`git ztrigger <DIR> <cmd>`), run on any
    /// change under the directory. `None` for a git repo hook (whose command is
    /// read from the repo's own `zvcs.hook`). Present ⇒ always fires, independent
    /// of the git-hook config.
    command: Option<String>,
    /// Leading-edge throttle (ms) for a directory trigger: after firing, suppress
    /// further fires for this long, coalescing the burst of events a single file
    /// action emits into one fire. 0 = fire on every event.
    throttle_ms: u64,
}

impl Target {
    /// The path registered with the OS watcher. Armed repos watch the whole
    /// working directory (recursively covering `.git`); unarmed repos watch the
    /// git dir, under which only the `refs`/`logs` subtrees are registered.
    fn watch_root(&self) -> &PathBuf {
        if self.armed {
            &self.workdir
        } else {
            &self.git_dir
        }
    }
}

/// Cap on watched repos, so a whole-device index can't exhaust the kernel's
/// watch budget. macOS FSEvents is effectively unbounded here; the real ceiling
/// is Linux inotify, where each repo costs ~10 watch descriptors (the subdirs
/// under `refs/`+`logs/`) against the per-UID `fs.inotify.max_user_watches`.
/// At 5120 repos that is ~50k descriptors, so a Linux host must have
/// `max_user_watches` tuned above the old 8192 default (modern systemd distros
/// already default well above it). Over the cap, later repos are simply not
/// watched — no crash, `build_targets` logs the cap and stops.
const MAX_WATCHED: usize = 5120;

/// Spawn the watch loop iff `[zvcs]` autonomy/hook/status is configured OR any
/// directory trigger exists. Directory triggers don't require the daemon to be
/// inside a repository.
pub fn spawn_if_configured() {
    let cfg = match gix::discover(".") {
        Ok(repo) => ZvcsConfig::load(&repo),
        Err(_) => ZvcsConfig::default(),
    };
    if !cfg.should_watch() && !crate::db::has_triggers() {
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
    let mut armed_n = 0usize;
    for t in &targets {
        if t.armed {
            // Whole-directory recursive watch: worktree AND `.git`, so creating
            // or editing ANY file in the repo fires the trigger — not only ref
            // moves. This is the directory a `git ztrigger` armed.
            if watcher.watch(&t.workdir, RecursiveMode::Recursive).is_ok() {
                watched += 1;
                armed_n += 1;
            }
        } else {
            for sub in ["refs", "logs"] {
                let p = t.git_dir.join(sub);
                if p.exists() && watcher.watch(&p, RecursiveMode::Recursive).is_ok() {
                    watched += 1;
                }
            }
        }
    }
    println!(
        "[zvcs watch] watching {} path(s) ({watched} tree(s), {armed_n} whole-dir), hooks={}",
        targets.len(),
        cfg.hooks_enabled(),
    );

    // Leading-edge throttle state per directory trigger: (last fire, events
    // coalesced since). No global debounce — we fire on the FIRST event of a
    // burst immediately, then suppress for `throttle_ms` so one file action (which
    // emits several fs events) fires once, not N times.
    let mut last_fired: HashMap<PathBuf, (Option<Instant>, u32)> = HashMap::new();
    loop {
        let ev = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => return,
        };
        let mut affected: HashSet<PathBuf> = HashSet::new();
        collect(&ev, &targets, &mut affected);
        if affected.is_empty() {
            continue;
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
        for t in &targets {
            if !affected.contains(&t.git_dir) {
                continue;
            }
            if let Some(cmd) = &t.command {
                // Directory trigger: fire the command, subject to a leading-edge
                // throttle. The first event fires immediately; events arriving
                // within `throttle_ms` of that fire are coalesced (counted, not
                // run), so a single file action fires once. Each real fire is
                // recorded for `git ztrigger tail`/`top`.
                let entry = last_fired.entry(t.git_dir.clone()).or_insert((None, 0));
                let now = Instant::now();
                let suppressed = t.throttle_ms > 0
                    && entry.0.is_some_and(|last| now.duration_since(last) < Duration::from_millis(t.throttle_ms));
                if suppressed {
                    entry.1 += 1;
                } else {
                    let coalesced = entry.1;
                    let ok = crate::superset::hooks::run_command(&t.workdir, cmd);
                    crate::superset::trigger::record_fire(&t.workdir, ok, coalesced);
                    *entry = (Some(now), 0);
                }
            } else if cfg.hooks_enabled() {
                if t.armed {
                    // Repo hook, whole-dir: fire on ANY change. No reflog-author
                    // gate — that distinguishes user vs daemon *ref moves*, but a
                    // plain file event leaves the reflog untouched, so gating on it
                    // would wrongly suppress real file-change fires.
                    crate::superset::hooks::run(&t.git_dir, &t.workdir);
                } else if !crate::superset::oplog::head_authored_by_zvcs(&t.git_dir) {
                    // Unarmed repo reached only via a global `zvcs.hook`: ref-change
                    // model (refs/logs watch), skipping the daemon's own bookkeeping.
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
        // Attribute each event to the SINGLE deepest matching repo, keyed on each
        // target's watch root (whole workdir for armed repos, git dir otherwise).
        // A submodule's tree lives under its parent's, so a plain prefix match
        // would also mark the parent — firing the parent's hook on a child-only
        // change. Longest matching watch root wins.
        if let Some(t) = targets
            .iter()
            .filter(|t| path.starts_with(t.watch_root()))
            .max_by_key(|t| t.watch_root().as_os_str().len())
        {
            affected.insert(t.git_dir.clone());
        }
    }
}

/// The repos to watch, in priority order so startup stays cheap on a big index:
///   1. **armed** repos (carry a hook) — read straight from the index's `hook`
///      column, so the trigger set is found in O(armed), NOT by opening every one
///      of thousands of indexed repos. Watched whole-dir (any file change fires).
///   2. working-repo **submodules** — only when autonomy is on (keyed off their
///      HEAD moves).
///   3. **all** indexed repos — only when `autostatus` needs per-repo status.
/// Deduped by git dir (armed wins) and capped at [`MAX_WATCHED`].
fn build_targets(cfg: &ZvcsConfig) -> Vec<Target> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut targets: Vec<Target> = Vec::new();

    // 0. Directory triggers (`git ztrigger <DIR> <cmd>`) — watch ANY directory
    //    whole-dir and run its command on change. Independent of git; always
    //    active when triggers exist. Read straight from the index.
    if let Ok(conn) = crate::db::open_ro() {
        if let Ok(triggers) = crate::db::list_triggers(&conn) {
            for t in triggers {
                let p = PathBuf::from(t.path);
                add_target(&mut seen, &mut targets, p.clone(), p, true, Some(t.command), t.throttle_ms.max(0) as u64);
                if targets.len() >= MAX_WATCHED {
                    break;
                }
            }
        }
    }

    // 1. Hook repos. Two shapes:
    //    * a GLOBAL hook (`cfg.hook`, e.g. in ~/.gitconfig) fires in EVERY indexed
    //      repo, so it must watch them all (ref-change model). Inherently O(all) —
    //      a global hook opts into machine-wide watching.
    //    * otherwise (`autohook` only) each repo runs its OWN local hook; watch
    //      just the ARMED set (repos with a hook), read from the index's `hook`
    //      column and watched whole-dir. This is what keeps startup O(armed) — a
    //      watch per armed repo, not per indexed repo — so the daemon reaches the
    //      watching state fast even on a whole-device index.
    if cfg.hooks_enabled() {
        if let Ok(conn) = crate::db::open_ro() {
            if cfg.hook.is_some() {
                if let Ok(repos) = crate::db::list_repos(&conn) {
                    for r in repos {
                        let wd = r.workdir.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&r.git_dir));
                        add_target(&mut seen, &mut targets, PathBuf::from(r.git_dir), wd, false, None, 0);
                        if targets.len() >= MAX_WATCHED {
                            break;
                        }
                    }
                }
            } else if let Ok(armed) = crate::db::list_armed(&conn) {
                for (git_dir, workdir) in armed {
                    let wd = workdir.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&git_dir));
                    add_target(&mut seen, &mut targets, PathBuf::from(git_dir), wd, true, None, 0);
                    if targets.len() >= MAX_WATCHED {
                        break;
                    }
                }
            }
        }
    }

    // 2. Working-repo submodules — autonomy is keyed off their HEAD moves.
    if cfg.any_autonomous() {
        if let Ok(repo) = gix::discover(".") {
            if let Ok(Some(subs)) = repo.submodules() {
                for sm in subs {
                    if let Ok(Some(sub)) = sm.open() {
                        if let Some(wd) = sub.workdir() {
                            add_target(&mut seen, &mut targets, sub.git_dir().to_path_buf(), wd.to_path_buf(), false, None, 0);
                        }
                    }
                }
            }
        }
    }

    // 3. Every indexed repo — only when autostatus maintains per-repo status.
    if cfg.autostatus {
        if let Ok(conn) = crate::db::open_ro() {
            if let Ok(repos) = crate::db::list_repos(&conn) {
                for r in repos {
                    let wd = r
                        .workdir
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(&r.git_dir));
                    add_target(&mut seen, &mut targets, PathBuf::from(r.git_dir), wd, false, None, 0);
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

/// Add a repo to the watch set, canonicalizing and deduping by git dir (an armed
/// repo added first wins over a later status-only pass). Both paths are
/// canonicalized so the stored `workdir` matches what other verbs key on.
fn add_target(
    seen: &mut HashSet<PathBuf>,
    targets: &mut Vec<Target>,
    git_dir: PathBuf,
    workdir: PathBuf,
    armed: bool,
    command: Option<String>,
    throttle_ms: u64,
) {
    let git_dir = git_dir.canonicalize().unwrap_or(git_dir);
    let workdir = workdir.canonicalize().unwrap_or(workdir);
    if seen.insert(git_dir.clone()) {
        targets.push(Target { git_dir, workdir, armed, command, throttle_ms });
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
        // The top-level repo too — `autoreconcile` is documented as "this one +
        // submodules". reconcile_repo_local is ff-only and skips a dirty worktree,
        // so this is safe (and usually a no-op while bots leave gitlinks dirty).
        if let Err(e) = crate::superset::reconcile_repo_local(&repo) {
            println!("[zvcs reconcile] (top): error: {e:#}");
            let _ = crate::db::record_failure(repo.git_dir(), "reconcile", &format!("{e:#}"));
        }
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

#[cfg(test)]
mod tests {
    use super::{collect, Target};
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[test]
    fn collect_attributes_event_to_deepest_repo_only() {
        // A submodule's git dir lives under its parent's; a plain prefix match would
        // mark BOTH. collect must attribute each event to the single deepest repo.
        let parent = PathBuf::from("/x/.git");
        let sub = PathBuf::from("/x/.git/modules/foo");
        let targets = vec![
            Target { git_dir: parent.clone(), workdir: PathBuf::from("/x"), armed: false, command: None, throttle_ms: 0 },
            Target { git_dir: sub.clone(), workdir: PathBuf::from("/x/foo"), armed: false, command: None, throttle_ms: 0 },
        ];

        // Event under the submodule → only the submodule is marked.
        let ev = notify::Event::new(notify::EventKind::Any).add_path(sub.join("refs/heads/main"));
        let mut affected = HashSet::new();
        collect(&Ok(ev), &targets, &mut affected);
        assert!(affected.contains(&sub), "submodule must be marked");
        assert!(!affected.contains(&parent), "parent must NOT be marked for a submodule-only event");

        // Event directly under the parent (not the submodule) → only the parent.
        let ev2 = notify::Event::new(notify::EventKind::Any).add_path(parent.join("refs/heads/main"));
        let mut a2 = HashSet::new();
        collect(&Ok(ev2), &targets, &mut a2);
        assert!(a2.contains(&parent), "parent must be marked for a parent event");
        assert!(!a2.contains(&sub), "submodule must NOT be marked for a parent event");
    }

    #[test]
    fn armed_repo_matches_plain_worktree_file_events() {
        // An armed repo watches its whole workdir, so a plain worktree file event
        // (NOT under refs/logs, and not even under .git) must be attributed to it —
        // this is the "fire on any file change" behavior an unarmed (git-dir-only)
        // target would miss entirely.
        let git_dir = PathBuf::from("/repo/.git");
        let workdir = PathBuf::from("/repo");
        let targets = vec![Target { git_dir: git_dir.clone(), workdir, armed: true, command: None, throttle_ms: 0 }];

        for rel in ["newfile.txt", ".git/index", "src/main.rs"] {
            let ev = notify::Event::new(notify::EventKind::Any).add_path(PathBuf::from("/repo").join(rel));
            let mut affected = HashSet::new();
            collect(&Ok(ev), &targets, &mut affected);
            assert!(affected.contains(&git_dir), "armed repo must be marked for worktree event {rel}");
        }
    }
}
