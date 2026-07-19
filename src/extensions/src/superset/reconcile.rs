//! Whole-tree reconcile: keep every CLEAN repo — the top-level repo AND each
//! submodule — fast-forwarded to its tracked mainline (`origin/main`, else
//! `origin/master`). Drives the autonomous daemon pass and generalizes `zsync`
//! from submodules-only to the entire working tree.
//!
//! Each repo is reconciled independently by [`super::reconcile_repo`], which is
//! fast-forward only and skips a dirty worktree — so a bot's in-flight work is
//! never regressed or clobbered. A single repo failing does not stop the rest.

use super::reconcile_repo;

/// Reconcile the top-level repo and all initialized submodules.
///
/// Returns one `(label, status)` per repo — `"."` for the top-level, the
/// submodule path otherwise. Never errors: per-repo failures are captured as a
/// status string so the caller (CLI or daemon) sees the whole picture.
pub fn reconcile_tree(top: &gix::Repository) -> Vec<(String, String)> {
    let mut out = Vec::new();

    match reconcile_repo(top) {
        Ok(status) => out.push((".".to_string(), status)),
        Err(e) => out.push((".".to_string(), format!("error: {e:#}"))),
    }

    if let Ok(Some(submodules)) = top.submodules() {
        for sm in submodules {
            let label = sm
                .path()
                .map(|p| p.to_string())
                .unwrap_or_else(|_| "<submodule>".to_string());
            match sm.open() {
                Ok(Some(sub_repo)) => {
                    let status = match reconcile_repo(&sub_repo) {
                        Ok(s) => s,
                        Err(e) => format!("error: {e:#}"),
                    };
                    out.push((label, status));
                }
                Ok(None) => out.push((label, "not initialized, skipped".to_string())),
                Err(e) => out.push((label, format!("open error: {e:#}"))),
            }
        }
    }

    out
}
