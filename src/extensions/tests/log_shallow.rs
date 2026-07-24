//! `git log` must honour `.git/shallow`: a commit listed there is grafted to have
//! no parents, so the walk stops at it instead of reading its (out-of-clone) parent
//! — which a `--depth` clone leaves absent, previously erroring with "object … could
//! not be found". Self-contained: fabricate the shallow file rather than clone.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary");
    assert!(
        out.status.success() || !args.contains(&"log"),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn log_stops_at_the_shallow_boundary() {
    let root = std::env::temp_dir().join(format!("zvcs-logshallow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    for m in ["c0", "c1", "c2"] {
        std::fs::write(repo.join("f"), format!("{m}\n")).unwrap();
        run(&repo, &home, &["add", "f"]);
        run(&repo, &home, &["commit", "-q", "-m", m]);
    }

    // Full history is three commits.
    assert_eq!(run(&repo, &home, &["log", "--oneline"]).lines().count(), 3);

    // Graft HEAD as shallow: the walk must now show only HEAD (its parent is
    // "outside the clone"), not error and not descend.
    let head = run(&repo, &home, &["rev-parse", "HEAD"]).trim().to_string();
    std::fs::write(repo.join(".git/shallow"), format!("{head}\n")).unwrap();

    let out = run(&repo, &home, &["log", "--oneline"]);
    assert_eq!(out.lines().count(), 1, "shallow HEAD must stop the walk; got:\n{out}");
    assert!(out.contains("c2"), "the one line should be HEAD (c2); got:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}
