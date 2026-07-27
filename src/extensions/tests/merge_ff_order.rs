//! A fast-forward that cannot check out must leave the branch where it was.
//!
//! git's `checkout_fast_forward()` updates the working tree first and only then
//! moves the ref, so an aborted fast-forward leaves `refs/heads/<branch>` alone
//! (git 2.55.0 writes `ORIG_HEAD` either way). Moving the ref first strands the
//! branch ahead of its own checkout: the index still describes the old commit, so
//! every later `status` reports the whole difference as staged work, and nothing
//! says the checkout never happened.
//!
//! The regression this pins: a `pull` fast-forwarded a branch two commits ahead
//! of its worktree after the checkout failed to read the objects it needed.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new(BIN).args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev(dir: &Path, spec: &str) -> String {
    let out = Command::new(BIN)
        .args(["rev-parse", spec])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "rev-parse {spec} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// An upstream one commit ahead of its clone, where the new commit adds
/// `sub/new` — a path the test can make unwritable in the clone.
fn fixture() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-ffo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let upstream = root.join("up");
    std::fs::create_dir_all(&upstream).unwrap();
    git(&upstream, &["init", "-q", "-b", "main", "."]);
    git(&upstream, &["config", "user.email", "alice@example.com"]);
    git(&upstream, &["config", "user.name", "Alice"]);
    std::fs::write(upstream.join("f"), "v1\n").unwrap();
    git(&upstream, &["add", "f"]);
    git(&upstream, &["commit", "-q", "-m", "c1"]);

    let clone = root.join("clone");
    git(&root, &["clone", "-q", "up", "clone"]);

    std::fs::create_dir_all(upstream.join("sub")).unwrap();
    std::fs::write(upstream.join("sub/new"), "added\n").unwrap();
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "c2"]);
    (upstream, clone)
}

/// Make `dir` unwritable and report whether that actually blocks writes — it does
/// not for a privileged user, and CI that runs as root would otherwise see the
/// checkout succeed and the assertion below turn into a false failure.
fn seal(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::write(dir.join("probe"), "x").is_err()
}

fn unseal(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
}

#[test]
fn a_fast_forward_that_cannot_check_out_leaves_the_branch_alone() {
    let (_upstream, clone) = fixture();
    let before = rev(&clone, "refs/heads/main");

    let blocked = clone.join("sub");
    std::fs::create_dir_all(&blocked).unwrap();
    if !seal(&blocked) {
        unseal(&blocked);
        eprintln!("skipped: writes to a read-only directory succeed here");
        return;
    }

    let out = Command::new(BIN).arg("pull").current_dir(&clone).output().unwrap();
    let after = rev(&clone, "refs/heads/main");
    unseal(&blocked);

    assert!(
        !out.status.success(),
        "a fast-forward whose checkout cannot write must fail: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        before, after,
        "the branch moved even though its checkout never happened"
    );
    assert!(
        !clone.join("sub/new").exists(),
        "fixture is wrong: the file the checkout could not write exists"
    );
}
