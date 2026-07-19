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

use std::time::Duration;

/// Resolved `[zvcs]` settings for a repository.
pub struct ZvcsConfig {
    /// Reconcile every clean repo (top-level + submodules) to origin/main on `interval`.
    pub autoreconcile: bool,
    /// Forward-only submodule gitlink bumps on `interval`.
    pub autobump: bool,
    /// Delay between autonomous passes.
    pub interval: Duration,
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
        Self {
            autoreconcile: snap.boolean("zvcs.autoreconcile").unwrap_or(false),
            autobump: snap.boolean("zvcs.autobump").unwrap_or(false),
            interval: Duration::from_secs(interval),
        }
    }

    /// Whether any autonomous behavior is enabled (i.e. a daemon is worth running).
    pub fn any_autonomous(&self) -> bool {
        self.autoreconcile || self.autobump
    }
}
