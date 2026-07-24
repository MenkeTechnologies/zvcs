//! `git push` source DWIM: a bare push source that names a **tag** must resolve to
//! `refs/tags/<name>` — git's `ref_rev_parse_rules` precedence puts tags before
//! heads — not `refs/heads/<name>`. Regression for the shim resolving `git push
//! origin v0.1.0` (a tag) to `refs/heads/v0.1.0`, which the remote rejected.
//!
//! Self-contained: the fixture is built with the shadow binary itself and the push
//! is a `--dry-run --porcelain` against a local bare remote, so no system git and no
//! network are needed (CI-safe).

use std::path::Path;
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

#[test]
fn bare_tag_source_resolves_to_refs_tags_not_heads() {
    let root = std::env::temp_dir().join(format!("zvcs-pushspec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    run(&repo, &home, &["init", "-q", "-b", "main", "."]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    run(&repo, &home, &["add", "f"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0"]);
    run(&repo, &home, &["remote", "add", "origin", bare.to_str().unwrap()]);
    run(&repo, &home, &["tag", "v0.1.0"]);

    // The reported bug: a bare tag source.
    let out = run(&repo, &home, &["push", "--dry-run", "--porcelain", "origin", "v0.1.0"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("refs/tags/v0.1.0:refs/tags/v0.1.0"),
        "bare tag source must resolve to refs/tags on both sides; got:\n{text}"
    );
    assert!(
        !text.contains("refs/heads/v0.1.0"),
        "a tag must never be pushed as a branch; got:\n{text}"
    );

    // Regression guard: a bare branch source still resolves to refs/heads.
    let outb = run(&repo, &home, &["push", "--dry-run", "--porcelain", "origin", "main"]);
    let tb = String::from_utf8_lossy(&outb.stdout);
    assert!(
        tb.contains("refs/heads/main:refs/heads/main"),
        "branch source must resolve to refs/heads; got:\n{tb}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
