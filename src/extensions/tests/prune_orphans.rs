//! Regression for the prune/rowid-reuse bug: pruning a deleted repo must also
//! drop its `claims`/`repo_status` rows, or a newly-indexed repo that reuses the
//! freed rowid would inherit the dead repo's lease/status.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(["-c", "user.email=t@e.x", "-c", "user.name=t"]).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn zvcs(home: &Path, sess: Option<&str>, cwd: &Path, args: &[&str]) -> (String, bool) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).env("ZVCS_HOME", home);
    if let Some(s) = sess {
        c.env("ZVCS_SESSION", s);
    }
    let o = c.output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(), o.status.success())
}

fn mkrepo(root: &Path, name: &str) -> std::path::PathBuf {
    let r = root.join(name);
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    git(&r, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    r
}

#[test]
fn prune_drops_claim_so_reused_rowid_starts_fresh() {
    let root = std::env::temp_dir().join(format!("zvcs-prune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let rs = root.to_str().unwrap();

    // A: index (id 1), claim as agentA, record status.
    let a = mkrepo(&root, "a");
    zvcs(&home, None, &root, &["zreindex", rs]);
    assert!(zvcs(&home, Some("agentA"), &a, &["zclaim"]).1, "claim A");
    zvcs(&home, None, &a, &["zstatus"]);

    // Delete A, reindex → prune removes A's repos row AND its claim/status; table
    // is now empty so the next inserted rowid (id 1) is reused.
    std::fs::remove_dir_all(&a).unwrap();
    zvcs(&home, None, &root, &["zreindex", rs]);

    // B: index (reuses rowid 1). It must be unclaimed — a fresh agent can claim it.
    let b = mkrepo(&root, "b");
    zvcs(&home, None, &root, &["zreindex", rs]);
    let (out, ok) = zvcs(&home, Some("agentB"), &b, &["zclaim"]);
    assert!(ok && out.contains("claimed"), "B must be unclaimed (no orphan lease inherited); got: {out}");

    // zwho must attribute B to agentB, never the dead agentA.
    let (who, _) = zvcs(&home, None, &b, &["zwho"]);
    assert!(who.contains("agentB"), "zwho should show agentB:\n{who}");
    assert!(!who.contains("agentA"), "dead agentA lease must not resurface:\n{who}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn prune_detaches_jobs_so_reused_rowid_gets_no_stale_failure() {
    // A pruned repo's failed job must not surface on a NEW repo that reuses its
    // rowid (notify-on-next-command joins jobs->repos on repo_id).
    let root = std::env::temp_dir().join(format!("zvcs-prunejob-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let rs = root.to_str().unwrap();

    // A: index (id 1), then record a FAILED job for it (a foreach that fails in A).
    let a = mkrepo(&root, "a");
    zvcs(&home, None, &root, &["zreindex", rs]);
    zvcs(&home, None, &root, &["zforeach", "--repo", "a", "--", "git", "rev-parse", "--verify", "--quiet", "no-such-ref"]);

    // Delete A and reindex → prune removes A's repos row and DETACHES its job
    // (repo_id → NULL). The table is empty so the next rowid (1) is reused.
    std::fs::remove_dir_all(&a).unwrap();
    zvcs(&home, None, &root, &["zreindex", rs]);

    // B reuses rowid 1. A plain command in B must NOT surface A's foreach failure.
    let b = mkrepo(&root, "b");
    zvcs(&home, None, &root, &["zreindex", rs]);
    let out = Command::new(BIN).args(["zstatus"]).current_dir(&b).env("ZVCS_HOME", &home).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("foreach failed"), "pruned repo's failure resurfaced on rowid-reused repo:\n{stderr}");

    let _ = std::fs::remove_dir_all(&root);
}
