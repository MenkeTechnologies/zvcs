//! Shallow clone and fetch against a `file://` remote served by this binary's
//! `upload-pack` — the deepen half of the protocol end to end, with both sides
//! being this implementation.
//!
//! Every expectation here is stock git 2.50.1's, taken from the same commands run
//! against `git-upload-pack`: which commits land, what `.git/shallow` holds after
//! each round, and which tags come along.
//!
//! Unix-only (uses a symlink so the transport spawns this binary as
//! `git-upload-pack`); skipped elsewhere.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, bindir: &Path, args: &[&str]) -> Output {
    let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap_or_default());
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("PATH", path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary")
}

fn stdout(cwd: &Path, home: &Path, bindir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(cwd, home, bindir, args).stdout).trim().to_string()
}

/// The commits `.git/shallow` names, sorted, so a boundary can be compared
/// without depending on the file's write order.
fn shallow_of(repo: &Path) -> Vec<String> {
    let path = repo.join(".git").join("shallow");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = text.lines().map(str::to_owned).filter(|l| !l.is_empty()).collect();
    ids.sort();
    ids
}

/// A six-commit line with an annotated tag three commits back, which is far
/// enough that a shallow window can be drawn on either side of it.
fn fixture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let home = root.join("home");
    let bindir = root.join("bin");
    let src = root.join("src");
    for d in [&home, &bindir, &src] {
        std::fs::create_dir_all(d).unwrap();
    }
    for name in ["git", "git-upload-pack", "git-receive-pack"] {
        std::os::unix::fs::symlink(BIN, bindir.join(name)).unwrap();
    }
    run(&src, &home, &bindir, &["init", "-q", "-b", "main"]);
    run(&src, &home, &bindir, &["config", "user.email", "t@e.co"]);
    run(&src, &home, &bindir, &["config", "user.name", "t"]);
    for i in 0..6 {
        std::fs::write(src.join("f"), format!("c{i}\n")).unwrap();
        run(&src, &home, &bindir, &["add", "f"]);
        run(&src, &home, &bindir, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    run(&src, &home, &bindir, &["tag", "-a", "v3", "-m", "three", "HEAD~3"]);
    (home, bindir, src)
}

/// `clone --depth <n>` must stop exactly `n` commits back and record that commit
/// as the boundary — `get_shallow_commits()` counts the wants as depth 1, so a
/// depth equal to the whole history still writes a `.git/shallow` naming the
/// root, while a depth past it writes none at all.
#[test]
fn depth_clone_stops_at_the_boundary_the_server_names() {
    let root = std::env::temp_dir().join(format!("zvcs-shallow-depth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, bindir, src) = fixture(&root);
    let url = format!("file://{}", src.display());

    for (depth, commits, boundary_from_tip) in [(1u32, 1usize, 0usize), (2, 2, 1), (6, 6, 5)] {
        let dst = root.join(format!("d{depth}"));
        let out = run(
            &root,
            &home,
            &bindir,
            &["clone", "-q", "--depth", &depth.to_string(), &url, dst.to_str().unwrap()],
        );
        assert!(out.status.success(), "depth {depth}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            stdout(&dst, &home, &bindir, &["rev-list", "--count", "HEAD"]),
            commits.to_string(),
            "depth {depth} should carry {commits} commits"
        );
        let expected =
            stdout(&src, &home, &bindir, &["rev-parse", &format!("HEAD~{boundary_from_tip}")]);
        assert_eq!(
            shallow_of(&dst),
            vec![expected],
            "depth {depth} should record exactly the commit its window ends at"
        );
    }

    // A depth larger than the history has no boundary to report, so no shallow
    // file is written and the clone is an ordinary complete one.
    let dst = root.join("d99");
    let out = run(&root, &home, &bindir, &["clone", "-q", "--depth", "99", &url, dst.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&dst, &home, &bindir, &["rev-list", "--count", "HEAD"]), "6");
    assert!(shallow_of(&dst).is_empty(), "a depth past the root is not a shallow clone");

    let _ = std::fs::remove_dir_all(&root);
}

/// Deepening an existing shallow clone: the server answers with the commit that
/// becomes the new boundary and with `unshallow` for the old one, and the pack
/// carries only what lies between them. `--unshallow` removes the boundary
/// altogether.
#[test]
fn deepening_moves_the_boundary_and_unshallow_removes_it() {
    let root = std::env::temp_dir().join(format!("zvcs-shallow-deepen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, bindir, src) = fixture(&root);
    let url = format!("file://{}", src.display());

    let dst = root.join("dst");
    let out = run(&root, &home, &bindir, &["clone", "-q", "--depth", "2", &url, dst.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(shallow_of(&dst), vec![stdout(&src, &home, &bindir, &["rev-parse", "HEAD~1"])]);

    // An absolute deepen: the window is measured from the tip, so `--depth 4`
    // leaves the boundary three commits back regardless of where it was.
    let out = run(&dst, &home, &bindir, &["fetch", "-q", "--depth", "4", "origin"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&dst, &home, &bindir, &["rev-list", "--count", "HEAD"]), "4");
    assert_eq!(shallow_of(&dst), vec![stdout(&src, &home, &bindir, &["rev-parse", "HEAD~3"])]);

    // A relative deepen: two more commits behind the *current* boundary.
    let out = run(&dst, &home, &bindir, &["fetch", "-q", "--deepen", "2", "origin"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&dst, &home, &bindir, &["rev-list", "--count", "HEAD"]), "6");

    let out = run(&dst, &home, &bindir, &["fetch", "-q", "--unshallow", "origin"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&dst, &home, &bindir, &["rev-list", "--count", "HEAD"]), "6");
    assert!(shallow_of(&dst).is_empty(), "--unshallow must leave no boundary behind");

    let _ = std::fs::remove_dir_all(&root);
}

/// `--depth` implies `--single-branch` (git-clone(1)), and a single-branch clone
/// asks for its tags with `include-tag` rather than wanting them outright. The
/// difference is invisible in a full clone and decisive in a shallow one: an
/// explicit `want` for a tag outside the window makes the server open a second
/// boundary at it, so the clone ends up with two `.git/shallow` entries and a tag
/// stock git does not fetch.
#[test]
fn depth_implies_single_branch_and_tags_ride_the_pack() {
    let root = std::env::temp_dir().join(format!("zvcs-shallow-tags-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, bindir, src) = fixture(&root);
    let url = format!("file://{}", src.display());

    // v3 sits four commits back, so a depth-2 window excludes it.
    let outside = root.join("outside");
    let out =
        run(&root, &home, &bindir, &["clone", "-q", "--depth", "2", &url, outside.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout(&outside, &home, &bindir, &["config", "--get", "remote.origin.fetch"]),
        "+refs/heads/main:refs/remotes/origin/main",
        "--depth must imply --single-branch"
    );
    assert_eq!(shallow_of(&outside).len(), 1, "one window, one boundary");
    assert_eq!(stdout(&outside, &home, &bindir, &["tag"]), "", "a tag outside the window is not fetched");

    // Deep enough to reach it, and the tag arrives with the pack.
    let inside = root.join("inside");
    let out =
        run(&root, &home, &bindir, &["clone", "-q", "--depth", "5", &url, inside.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(shallow_of(&inside).len(), 1);
    assert_eq!(stdout(&inside, &home, &bindir, &["tag"]), "v3", "a tag inside the window comes along");

    // `--no-single-branch` opts back out, and then every branch is tracked.
    let wide = root.join("wide");
    let out = run(
        &root,
        &home,
        &bindir,
        &["clone", "-q", "--depth", "2", "--no-single-branch", &url, wide.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout(&wide, &home, &bindir, &["config", "--get", "remote.origin.fetch"]),
        "+refs/heads/*:refs/remotes/origin/*"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `is_git_directory()` requires a `refs` directory, so a bare clone whose refs
/// all went into `packed-refs` still has to have one — without it stock git
/// refuses to open the repository this clone just made.
#[test]
fn a_bare_clone_has_the_ref_directories_git_creates() {
    let root = std::env::temp_dir().join(format!("zvcs-bareclone-refs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, bindir, src) = fixture(&root);

    let dst = root.join("bare.git");
    let out =
        run(&root, &home, &bindir, &["clone", "-q", "--bare", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for sub in ["refs/heads", "refs/tags"] {
        assert!(dst.join(sub).is_dir(), "a bare clone must have {sub}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
