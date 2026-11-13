//! `peel_onion()`'s `error()` and the `get_oid_basic()` fallback behind it, through
//! the walk commands and `rev-parse`.
//!
//! `get_oid_1()` falls back to `get_oid_basic()` on a frame's whole name once
//! `peel_onion()` has failed (object-name.c:1127-1133). For a reflog operand that
//! fallback answers: `HEAD@{1}^{blob}` reads `1}^{blob` as a date on HEAD's log.
//! So the operand resolves, after `repo_peel_to_type()` printed
//! `error: HEAD@{1}^{blob}: expected blob type, but the object dereferences to
//! tree type`. zvcs walked the right commits but printed nothing from `log`,
//! `log -g`, `show`, `rev-list` and `reflog show`. It also took the fallback only
//! for the whole operand, so `HEAD@{1}^{blob}^{tree}` (the inner frame falls back,
//! and the outer one peels that commit) came out as a commit instead of a tree,
//! and `^HEAD@{1}^{blob}` and `HEAD@{1}^{blob}..HEAD` were refused by `log` and
//! `show`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_failed_peel_reports_and_falls_back_per_frame() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("peel-frame-fallback", stock);
    for args in [
        &["log", "--oneline", "HEAD@{1}^{blob}"][..],
        &["log", "-g", "--oneline", "HEAD@{1}^{tag}"],
        &["show", "-s", "--oneline", "main@{1}^{blob}"],
        &["show", "-s", "HEAD@{1}^{blob}^{tree}"],
        &["rev-list", "HEAD@{1}^{tree}^{commit}"],
        &["reflog", "show", "HEAD@{1}^{blob}"],
        &["rev-parse", "HEAD@{1}^{blob}^{blob}", "HEAD@{1}^{blob}^{tree}"],
        &["rev-parse", "HEAD@{1}^{blob}..HEAD"],
        &["log", "--oneline", "^HEAD@{1}^{blob}", "main"],
        &["show", "-s", "--oneline", "HEAD@{1}^{blob}..HEAD"],
        &["log", "-g", "--oneline", "HEAD@{1}^{blob}...HEAD"],
        &["rev-list", "HEAD^{blob}..HEAD"],
        // A reflog read back past its start warns again from each fallback,
        // and the failing operand's second resolution (`GET_OID_QUIETLY`) does
        // so only below the `~1` that drops the flag.
        &["log", "--oneline", "HEAD@{2000-01-01}^{blob}"],
        &["log", "HEAD@{2000-01-01}^{blob}~1"],
        &["rev-parse", "HEAD@{2000-01-01}^{blob}~1"],
        &["log", "HEAD@{2000-01-01}^", "--"],
        &["rev-list", "HEAD@{2000-01-01}:nosuch"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
