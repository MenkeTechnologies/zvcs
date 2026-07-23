//! Multi-agent claim/lease: one session claims a repo; another is refused;
//! `zwho` lists the holder; release frees it.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// Run zvcs as session `sess`; return (stdout, stderr, success).
fn zvcs(home: &Path, cwd: &Path, sess: &str, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .env("ZVCS_SESSION", sess)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn claim_is_exclusive_across_sessions() {
    let root = std::env::temp_dir().join(format!("zvcs-claim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);

    // Agent A claims → acquired.
    let (out, _e, ok) = zvcs(&home, &repo, "agentA", &["zclaim"]);
    assert!(ok && out.contains("claimed"), "A should acquire: {out}");

    // Agent B is refused.
    let (_o, err, ok_b) = zvcs(&home, &repo, "agentB", &["zclaim"]);
    assert!(!ok_b && err.contains("claimed by agentA"), "B must be refused: {err}");

    // A re-claiming is idempotent.
    let (out2, _e, ok2) = zvcs(&home, &repo, "agentA", &["zclaim"]);
    assert!(ok2 && out2.contains("already yours"), "A re-claim: {out2}");

    // zwho shows agentA holding the repo.
    let (who, _e, _ok) = zvcs(&home, &repo, "agentA", &["zwho"]);
    assert!(who.contains("agentA") && who.contains("repo"), "zwho: {who}");

    // B cannot release A's claim; A can, then B can claim.
    let (_o, _e, b_rel) = zvcs(&home, &repo, "agentB", &["zunclaim"]);
    assert!(!b_rel, "B must not release A's claim");
    let (_o, _e, a_rel) = zvcs(&home, &repo, "agentA", &["zunclaim"]);
    assert!(a_rel, "A releases its own claim");
    let (out3, _e, ok3) = zvcs(&home, &repo, "agentB", &["zclaim"]);
    assert!(ok3 && out3.contains("claimed"), "B claims after release: {out3}");

    let _ = std::fs::remove_dir_all(&root);
}
