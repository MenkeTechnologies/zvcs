//! `git push` with no `<remote>` resolves the default remote in git's order:
//! `branch.<name>.pushRemote` > `remote.pushDefault` > `branch.<name>.remote` >
//! `origin`. Regression guard for the remote being hardcoded to `origin`.
//!
//! Each candidate remote points at a distinct local bare repo, and the resolved
//! remote is read from `push --dry-run --porcelain`'s `To <url>` line — network-free
//! and independent of whether the push itself would succeed. (Earlier this asserted
//! against a pre-flight "cannot upload to <remote>" error from a send-pack-less
//! build; zvcs now has send-pack, so a bare push would actually hit the network.)

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary")
}

fn git(cwd: &Path, home: &Path, args: &[&str]) {
    let out = run(cwd, home, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pushcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    // Three distinct local bare remotes, so the resolved remote is identifiable from
    // the URL the push targets.
    for r in ["origin", "backup", "other"] {
        let bare = root.join(format!("{r}.git"));
    // `-b main` explicitly: a runner has no `init.defaultBranch`, so the bare
    // repo would init to `master` and every later `main` reference — the
    // clone's branch, a checkout, a refspec — would miss.
        git(&root, &home, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    }

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@e.x"]);
    git(&repo, &home, &["config", "user.name", "t"]);
    // `push.default=current`, so a bare push has a refspec whatever the remote turns out to
    // be: under the default `simple`, stock git refuses a branch with no upstream
    // (`The current branch main has no upstream branch.`, builtin/push.c:212-220) and there
    // is no `To <url>` line to read the resolved remote from. The resolution order this
    // test is about is the same either way.
    git(&repo, &home, &["config", "push.default", "current"]);
    std::fs::write(repo.join("f"), "x\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "c0"]);
    for r in ["origin", "backup", "other"] {
        let bare = root.join(format!("{r}.git"));
        git(&repo, &home, &["remote", "add", r, bare.to_str().unwrap()]);
    }
    (repo, home)
}

/// The remote a bare `push` resolves to, read from `--dry-run --porcelain`'s
/// `To <url>` line — each remote's URL basename (minus `.git`) is its name.
fn resolved_remote(repo: &Path, home: &Path) -> String {
    let out = run(repo, home, &["push", "--dry-run", "--porcelain"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let to = text.lines().find(|l| l.starts_with("To ")).unwrap_or_else(|| {
        panic!("no `To <url>` line in push output:\n{text}");
    });
    // "To /path/to/<name>.git"
    to.rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".git"))
        .unwrap_or("")
        .to_string()
}

#[test]
fn push_default_remote_resolution_order() {
    let (repo, home) = fixture("order");

    // No config → origin.
    assert_eq!(resolved_remote(&repo, &home), "origin");

    // remote.pushDefault takes over.
    git(&repo, &home, &["config", "remote.pushDefault", "backup"]);
    assert_eq!(resolved_remote(&repo, &home), "backup");

    // branch.<name>.pushRemote overrides remote.pushDefault.
    git(&repo, &home, &["config", "branch.main.pushRemote", "other"]);
    assert_eq!(resolved_remote(&repo, &home), "other");

    // With neither pushRemote nor pushDefault, fall back to branch.<name>.remote.
    git(&repo, &home, &["config", "--unset", "branch.main.pushRemote"]);
    git(&repo, &home, &["config", "--unset", "remote.pushDefault"]);
    git(&repo, &home, &["config", "branch.main.remote", "backup"]);
    assert_eq!(resolved_remote(&repo, &home), "backup");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
