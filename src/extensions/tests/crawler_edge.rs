//! The crawler must index both a normal repo (`.git` dir) and a submodule
//! checkout (`.git` *file* pointing into `.git/modules/...`), and a nested repo,
//! while ignoring non-repo dirs.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

#[test]
fn crawler_indexes_submodule_git_file_and_nested_repo() {
    let root = std::env::temp_dir().join(format!("zvcs-crawledge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // A submodule source.
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);

    // A parent repo with the submodule (parent/sub has a `.git` FILE).
    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["commit", "--allow-empty", "-q", "-m", "p0"]);
    git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);
    assert!(parent.join("sub/.git").is_file(), "submodule should have a .git file");

    // A standalone repo nested inside a plain subdir + a non-repo dir.
    let nested = root.join("plain/nested");
    std::fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("not_a_repo/deep")).unwrap();

    let out = Command::new(BIN).args(["zreindex", root.to_str().unwrap()]).current_dir(&root).env("ZVCS_HOME", &home).output().unwrap();
    assert!(out.status.success(), "zreindex failed");

    let repos = String::from_utf8(Command::new(BIN).args(["zrepos"]).current_dir(&root).env("ZVCS_HOME", &home).output().unwrap().stdout).unwrap();

    assert!(repos.lines().any(|l| l.ends_with("/parent")), "parent repo missing:\n{repos}");
    assert!(repos.lines().any(|l| l.ends_with("/parent/sub")), "submodule (.git file) missing:\n{repos}");
    assert!(repos.lines().any(|l| l.ends_with("/plain/nested")), "nested repo missing:\n{repos}");
    assert!(repos.lines().any(|l| l.ends_with("/sub_src")), "submodule source missing:\n{repos}");
    assert!(!repos.contains("not_a_repo"), "non-repo dir wrongly indexed:\n{repos}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn crawlroots_tilde_expands_to_home() {
    // `zvcs.crawlroots = ~/target` must expand to $HOME/target and crawl it. A
    // raw `~` would match nothing (silent no-op) — the bug this guards.
    let root = std::env::temp_dir().join(format!("zvcs-crawltilde-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let zvcs_home = root.join("zhome");

    // $HOME for the child: contains the target repo we want found via `~/target`.
    let fake_home = root.join("fakehome");
    let target = fake_home.join("target");
    std::fs::create_dir_all(&target).unwrap();
    git(&target, &["init", "-q", "-b", "main"]);
    git(&target, &["commit", "--allow-empty", "-q", "-m", "t0"]);

    // The cwd repo carries the config the crawler reads (discover(".")).
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    git(&cwd, &["init", "-q", "-b", "main"]);
    git(&cwd, &["config", "zvcs.crawlroots", "~/target"]);

    // No path arg → configured_roots() → expand_tilde against the child's HOME.
    let out = Command::new(BIN)
        .args(["zreindex"])
        .current_dir(&cwd)
        .env("HOME", &fake_home)
        .env("ZVCS_HOME", &zvcs_home)
        .output()
        .unwrap();
    assert!(out.status.success(), "zreindex failed: {}", String::from_utf8_lossy(&out.stderr));

    let repos = String::from_utf8(
        Command::new(BIN).args(["zrepos"]).current_dir(&cwd).env("HOME", &fake_home).env("ZVCS_HOME", &zvcs_home).output().unwrap().stdout,
    ).unwrap();
    assert!(repos.lines().any(|l| l.ends_with("/target")), "~/target did not expand/crawl:\n{repos}");

    let _ = std::fs::remove_dir_all(&root);
}
