//! `[zvcs]` gitconfig — the switches that make the coordination layer autonomous.
//!
//! Configure once in `.gitconfig` (or `.git/config`) and the daemon does the
//! work on a timer; nothing is run by hand:
//!
//! ```gitconfig
//! [zvcs]
//!     autoreconcile = true   ; keep every CLEAN repo (this one + submodules) at origin/main
//!     autobump      = true   ; forward-only submodule gitlink bumps
//!     interval      = 30     ; seconds between autonomous passes (default 30)
//! ```

use std::path::PathBuf;
use std::time::Duration;

/// Resolved `[zvcs]` settings for a repository.
pub struct ZvcsConfig {
    /// Reconcile every clean repo (top-level + submodules) to origin/main on `interval`.
    pub autoreconcile: bool,
    /// Forward-only submodule gitlink bumps on `interval`.
    pub autobump: bool,
    /// Debounce window for coalescing watch-driven reaction bursts.
    pub interval: Duration,
    /// Roots for the repo crawler (`zvcs.crawlroots`, whitespace/comma separated).
    /// Empty means "use `$HOME`".
    pub crawlroots: Vec<PathBuf>,
    /// Crawl the configured roots for git repos in the background on daemon start
    /// (`zvcs.autocrawl`). Off by default — a whole-device scan is opt-in.
    pub autocrawl: bool,
    /// A `zvcs.hook` command to run on ref-change in any watched repo. When set,
    /// the daemon watches every indexed repo (not just the working submodules)
    /// and fires the hook per repo. `None` means no hooks.
    pub hook: Option<String>,
    /// Maintain each watched repo's cached status in the db on ref-change
    /// (`zvcs.autostatus`), so `git zstatus --all` is instant. Off by default.
    pub autostatus: bool,
    /// Watch every indexed repo and fire each repo's *own* `zvcs.hook` on
    /// ref-change (`zvcs.autohook`). This is the master switch that makes
    /// **per-repo (local) hooks** work without also setting a hook on the
    /// daemon's repo. Off by default.
    pub autohook: bool,
}

impl ZvcsConfig {
    /// Read `[zvcs]` from the repository's merged config. Absent keys default to
    /// off; `interval` defaults to 30s and ignores non-positive values.
    pub fn load(repo: &gix::Repository) -> Self {
        let snap = repo.config_snapshot();
        let interval = snap
            .integer("zvcs.interval")
            .filter(|s| *s > 0)
            .unwrap_or(30) as u64;
        let crawlroots = snap
            .string("zvcs.crawlroots")
            .map(|s| {
                s.to_string()
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|t| !t.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            autoreconcile: snap.boolean("zvcs.autoreconcile").unwrap_or(false),
            autobump: snap.boolean("zvcs.autobump").unwrap_or(false),
            interval: Duration::from_secs(interval),
            crawlroots,
            autocrawl: snap.boolean("zvcs.autocrawl").unwrap_or(false),
            hook: snap
                .string("zvcs.hook")
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty()),
            autostatus: snap.boolean("zvcs.autostatus").unwrap_or(false),
            autohook: snap.boolean("zvcs.autohook").unwrap_or(false),
        }
    }

    /// Whether any autonomous (working-tree) behavior is enabled.
    pub fn any_autonomous(&self) -> bool {
        self.autoreconcile || self.autobump
    }

    /// Whether the daemon should run the watch loop at all — autonomy, hooks, or
    /// status maintenance.
    pub fn should_watch(&self) -> bool {
        self.any_autonomous() || self.hooks_enabled() || self.autostatus
    }

    /// Whether the watcher should cover every indexed repo (not just working
    /// submodules): needed for machine-wide hooks or status.
    pub fn watch_all_repos(&self) -> bool {
        self.hooks_enabled() || self.autostatus
    }

    /// Whether hooks should fire: a hook set here, or the `autohook` master
    /// switch (which fires each repo's own local hook).
    pub fn hooks_enabled(&self) -> bool {
        self.hook.is_some() || self.autohook
    }
}
