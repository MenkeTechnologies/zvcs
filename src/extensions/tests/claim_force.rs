//! `zunclaim --force` clears a lease held by ANY session — the escape hatch for a
//! dead agent's claim (which otherwise no other session can release).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

fn zvcs(home: &Path, sess: &str, cwd: &Path, args: &[&str]) -> bool {
    Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).env("ZVCS_SESSION", sess).status().unwrap().success()
}

#[test]
fn force_unclaim_clears_another_sessions_lease() {
    let root = std::env::temp_dir().join(format!("zvcs-claimforce-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c0"]);

    // agentA claims it (then "dies" — never unclaims).
    assert!(zvcs(&home, "agentA", &repo, &["zclaim"]), "agentA claim");

    // agentB cannot claim (held) nor release it normally.
    assert!(!zvcs(&home, "agentB", &repo, &["zclaim"]), "agentB must NOT be able to claim agentA's repo");
    assert!(!zvcs(&home, "agentB", &repo, &["zunclaim"]), "plain zunclaim must not release another session's lease");

    // --force clears it, and now agentB can claim.
    assert!(zvcs(&home, "agentB", &repo, &["zunclaim", "--force"]), "zunclaim --force must clear the dead lease");
    assert!(zvcs(&home, "agentB", &repo, &["zclaim"]), "repo must be claimable after force-release");

    let _ = std::fs::remove_dir_all(&root);
}
