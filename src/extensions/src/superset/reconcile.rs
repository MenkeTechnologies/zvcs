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
/// The whole working tree as repositories: `top` first, then every initialized
/// submodule at any depth, parents before their children.
///
/// Tree-wide verbs kept hand-rolling this walk, and the hand-rolled ones
/// disagreed: `zsnapshot`, `zstash`, `zup` and `zworktree` recursed while
/// `zrewind` and the daemon's autonomy pass stopped at the first level, so a
/// submodule that itself has submodules was invisible to half the tree-wide
/// features. Recursion is the behaviour the docs describe ("every nested
/// submodule"), and this is the one place that implements it.
pub fn tree_repos(top: &gix::Repository) -> Vec<gix::Repository> {
    let mut out = vec![top.clone()];
    push_submodules(top, &mut out);
    out
}

fn push_submodules(repo: &gix::Repository, out: &mut Vec<gix::Repository>) {
    let Ok(Some(subs)) = repo.submodules() else { return };
    for sm in subs {
        // An uninitialized submodule has no repository to act on; callers that
        // report on those read the submodule list themselves.
        if let Ok(Some(sub)) = sm.open() {
            out.push(sub.clone());
            push_submodules(&sub, out);
        }
    }
}

/// Returns one `(label, status)` per repo — `"."` for the top-level, the
/// submodule path otherwise. Never errors: per-repo failures are captured as a
/// status string so the caller (CLI or daemon) sees the whole picture.
pub fn reconcile_tree(top: &gix::Repository) -> Vec<(String, String)> {
    let mut out = Vec::new();

    match reconcile_repo(top) {
        Ok(status) => out.push((".".to_string(), status)),
        Err(e) => out.push((".".to_string(), format!("error: {e:#}"))),
    }

    // Every submodule at any depth: a submodule that has submodules of its own
    // is the normal shape here, and stopping at the first level left them out of
    // a walk documented as covering the entire working tree.
    for sub in tree_repos(top).into_iter().skip(1) {
        let label = sub
            .workdir()
            .map(|w| w.display().to_string())
            .unwrap_or_else(|| "<submodule>".to_string());
        let status = match reconcile_repo(&sub) {
            Ok(s) => s,
            Err(e) => format!("error: {e:#}"),
        };
        out.push((label, status));
    }

    out
}
