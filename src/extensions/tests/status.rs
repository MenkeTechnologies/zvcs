//! `git zstatus` computes a repo's live status and caches it; `git zstatus --all`
//! reads the cache across all indexed repos (pipe-clean).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn zstatus_live_and_all() {
    let root = std::env::temp_dir().join(format!("zvcs-status-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), b"1\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "root"]);

    // Clean.
    let clean = zvcs(&home, &repo, &["zstatus"]);
    assert!(clean.contains("clean"), "expected clean: {clean}");

    // Dirty after modifying a *tracked* file (gix is_dirty is tracked-change based,
    // like `git diff`; untracked-only files are not counted).
    std::fs::write(repo.join("a.txt"), b"2\n").unwrap();
    let dirty = zvcs(&home, &repo, &["zstatus"]);
    assert!(dirty.contains("dirty"), "expected dirty: {dirty}");

    // --all reads the cache (last zstatus recorded it): pipe-clean, lists the repo.
    let all = zvcs(&home, &repo, &["zstatus", "--all"]);
    assert!(all.contains("repo"), "zstatus --all should list the repo:\n{all}");
    assert!(all.contains("dirty"), "cached status should be dirty:\n{all}");
    assert!(!all.contains("repo(s)"), "--all stdout must be pipe-clean:\n{all}");

    let _ = std::fs::remove_dir_all(&root);
}
