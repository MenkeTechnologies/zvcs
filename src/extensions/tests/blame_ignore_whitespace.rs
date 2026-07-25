//! `git blame -w` ignores whitespace when diffing revisions: a line whose only change
//! between two commits is whitespace is attributed to the EARLIER commit, not the
//! whitespace-only change. A real content change is still attributed to its author.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let ok = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

/// The author on blame line 1 (`^abbrev (author date ... 1) text`).
fn author_of_line1(dir: &Path, home: &Path, extra: &[&str]) -> String {
    let mut args = vec!["blame"];
    args.extend_from_slice(extra);
    args.push("f");
    let out = Command::new(BIN)
        .args(&args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "blame {extra:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.lines().next().unwrap_or_default();
    // `^b957… (alice 2026-… 1) text` → the token after the `(`.
    line.split('(').nth(1).and_then(|s| s.split_whitespace().next()).unwrap_or("").to_string()
}

#[test]
fn blame_w_attributes_whitespace_only_change_to_earlier_commit() {
    let root = std::env::temp_dir().join(format!("zvcs-blamew-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "a@e.co"]);

    // c1 (alice): original line 1.
    git(&repo, &home, &["config", "user.name", "alice"]);
    std::fs::write(repo.join("f"), "hello world\nsecond line\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "c1"]);

    // c2 (bob): whitespace-ONLY change to line 1 (indent + inner + trailing spaces/tab).
    git(&repo, &home, &["config", "user.name", "bob"]);
    std::fs::write(repo.join("f"), "\thello    world   \nsecond line\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "c2ws"]);

    // Plain blame: line 1 is bob's (he last touched those bytes).
    assert_eq!(author_of_line1(&repo, &home, &[]), "bob", "plain blame should credit the whitespace change");
    // `-w`: whitespace ignored → line 1 goes back to alice (c1).
    assert_eq!(
        author_of_line1(&repo, &home, &["-w"]),
        "alice",
        "-w should ignore the whitespace-only change and credit the earlier commit"
    );

    // c3 (carol): a REAL content change to line 1.
    git(&repo, &home, &["config", "user.name", "carol"]);
    std::fs::write(repo.join("f"), "\thello    WORLD   \nsecond line\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "c3real"]);

    // `-w` still credits a genuine content change to its author.
    assert_eq!(
        author_of_line1(&repo, &home, &["-w"]),
        "carol",
        "-w must still attribute a real content change to its author"
    );

    let _ = std::fs::remove_dir_all(&root);
}
