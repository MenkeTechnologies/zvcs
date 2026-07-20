//! Shared repo selector over the indexed set — the "which repos" half of the
//! machine-wide verbs (`zforeach`, and reusable by others).
//!
//! Filters compose (all must match) and are fast db queries, not walks:
//!   * default        — every indexed repo
//!   * `--repo <sub>` — workdir path contains `<sub>`
//!   * `--dirty`      — dirty in the status cache
//!   * `--ahead` / `--behind` — sync state in the status cache
//!   * `--claimed`    — has an active claim
//!   * `--session <s>`— claimed by session `<s>`

use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Default)]
pub struct Selector {
    pub repo: Option<String>,
    pub dirty: bool,
    pub ahead: bool,
    pub behind: bool,
    pub claimed: bool,
    pub session: Option<String>,
}

impl Selector {
    /// Parse leading selector flags; return the selector and the remaining args
    /// (everything not consumed as a selector, in order).
    pub fn parse(args: &[String]) -> (Selector, Vec<String>) {
        let mut sel = Selector::default();
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--repo" => {
                    i += 1;
                    sel.repo = args.get(i).cloned();
                }
                "--dirty" => sel.dirty = true,
                "--ahead" => sel.ahead = true,
                "--behind" => sel.behind = true,
                "--claimed" => sel.claimed = true,
                "--session" => {
                    i += 1;
                    sel.session = args.get(i).cloned();
                    sel.claimed = true;
                }
                _ => rest.push(args[i].clone()),
            }
            i += 1;
        }
        (sel, rest)
    }

    /// Resolve the selection to `(git_dir, workdir)` pairs from the ledger.
    pub fn select(&self) -> Result<Vec<(PathBuf, PathBuf)>> {
        let conn = match crate::db::open_ro() {
            Ok(c) => c,
            Err(_) => return Ok(Vec::new()), // no ledger → nothing indexed
        };

        // Status filter set (by workdir path), if any status filter is active.
        let status_set: Option<HashSet<String>> = if self.dirty || self.ahead || self.behind {
            let mut set = HashSet::new();
            for s in crate::db::list_status(&conn)? {
                let hit = (self.dirty && s.dirty)
                    || (self.ahead && s.sync == "ahead")
                    || (self.behind && s.sync == "behind");
                if hit {
                    set.insert(s.path);
                }
            }
            Some(set)
        } else {
            None
        };

        // Claim filter set (by workdir path), if any claim filter is active.
        let claim_set: Option<HashSet<String>> = if self.claimed || self.session.is_some() {
            let mut set = HashSet::new();
            for (path, session, _ts) in crate::db::list_claims(&conn)? {
                if self.session.as_ref().map(|s| *s == session).unwrap_or(true) {
                    set.insert(path);
                }
            }
            Some(set)
        } else {
            None
        };

        let mut out = Vec::new();
        for r in crate::db::list_repos(&conn)? {
            let workdir = r.workdir.clone().unwrap_or_else(|| r.git_dir.clone());
            if let Some(sub) = &self.repo {
                if !workdir.contains(sub.as_str()) {
                    continue;
                }
            }
            if let Some(set) = &status_set {
                if !set.contains(&workdir) {
                    continue;
                }
            }
            if let Some(set) = &claim_set {
                if !set.contains(&workdir) {
                    continue;
                }
            }
            out.push((PathBuf::from(r.git_dir), PathBuf::from(workdir)));
        }
        Ok(out)
    }
}
