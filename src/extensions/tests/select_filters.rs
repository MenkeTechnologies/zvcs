//! The shared selector's filters must actually narrow the set — and the
//! status/claim filters depend on the db paths matching between where status and
//! claims are *written* and where the selector *reads* them (a canonicalization
//! mismatch would silently return nothing).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

fn zvcs(home: &Path, sess: Option<&str>, cwd: &Path, args: &[&str]) -> String {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).env("ZVCS_HOME", home);
    if let Some(s) = sess {
        c.env("ZVCS_SESSION", s);
    }
    String::from_utf8_lossy(&c.output().unwrap().stdout).into_owned()
}

#[test]
fn selector_dirty_and_claimed_filters() {
    let root = std::env::temp_dir().join(format!("zvcs-sel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("f.txt"), b"1\n").unwrap();
        git(&r, &["add", "f.txt"]);
        git(&r, &["commit", "-q", "-m", "c0"]);
    }
    zvcs(&home, None, &root, &["zreindex", root.to_str().unwrap()]);

    // Make alpha dirty (tracked change) and record status for both.
    std::fs::write(root.join("alpha/f.txt"), b"2\n").unwrap();
    zvcs(&home, None, &root.join("alpha"), &["zstatus"]);
    zvcs(&home, None, &root.join("beta"), &["zstatus"]);

    // --dirty → only alpha.
    let dirty = zvcs(&home, None, &root, &["zforeach", "--dirty", "--", "git", "rev-parse", "HEAD"]);
    assert!(dirty.contains("alpha"), "--dirty must include alpha (path-join / dirty filter broken):\n{dirty}");
    assert!(!dirty.contains("beta"), "--dirty must exclude clean beta:\n{dirty}");

    // Claim beta as session s1.
    zvcs(&home, Some("s1"), &root.join("beta"), &["zclaim"]);

    // --claimed → only beta; --session s1 → only beta; --session other → none.
    let claimed = zvcs(&home, None, &root, &["zforeach", "--claimed", "--", "git", "rev-parse", "HEAD"]);
    assert!(claimed.contains("beta") && !claimed.contains("alpha"), "--claimed must be only beta:\n{claimed}");
    let by_sess = zvcs(&home, None, &root, &["zforeach", "--session", "s1", "--", "git", "rev-parse", "HEAD"]);
    assert!(by_sess.contains("beta") && !by_sess.contains("alpha"), "--session s1 must be only beta:\n{by_sess}");
    let other = zvcs(&home, None, &root, &["zforeach", "--session", "nobody", "--", "git", "rev-parse", "HEAD"]);
    assert!(!other.contains("alpha") && !other.contains("beta"), "--session nobody must match nothing:\n{other}");

    let _ = std::fs::remove_dir_all(&root);
}
