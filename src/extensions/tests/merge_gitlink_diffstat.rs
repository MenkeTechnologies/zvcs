//! A merge whose diffstat covers a submodule must not try to read the submodule's
//! commit out of the *parent's* object database.
//!
//! Regression: `merge`'s diffstat asked the tree diff for line counts, and for a
//! gitlink the resource cache has no blob to hand it, so the row fell through to
//! the binary branch — which called `find_object()` on the gitlink id. That commit
//! lives in the submodule, never in the parent, so a plain fast-forward that moved
//! a submodule pointer died with `An object with id <sha> could not be found`
//! *after* the worktree, index and ref had already been updated: the merge (and
//! every `pull` that delegates to it) reported failure on work it had completed.
//! git's `diff_populate_filespec()` substitutes the single line
//! `Subproject commit <oid>` for a gitlink instead of reading an object, which is
//! what makes a bumped submodule cost one insertion and one deletion.
//!
//! Expected output is stock git 2.55.0's, byte for byte.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// PATH with any zvcs shadow dir removed, so a nested `git` in setup resolves to
/// the real system git rather than recursing into the binary under test.
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .env("PATH", real_git_path())
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// A fresh temp root, removed if a previous run left one behind.
fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-gitlinkstat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// A submodule source with two distinct commits, returned oldest first.
fn submodule_source(root: &Path) -> (std::path::PathBuf, String, String) {
    let src = root.join("sub_src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["commit", "--allow-empty", "-q", "-m", "s0"]);
    let c0 = git(&src, &["rev-parse", "HEAD"]).trim().to_string();
    git(&src, &["commit", "--allow-empty", "-q", "-m", "s1"]);
    let c1 = git(&src, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(c0, c1);
    (src, c0, c1)
}

/// The diffstat block: everything the merge printed after its `Fast-forward` line.
fn stat_block(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (_, rest) = stdout
        .split_once("Fast-forward\n")
        .unwrap_or_else(|| panic!("no fast-forward in merge output: {stdout}"));
    rest.to_string()
}

fn assert_merge_ok(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("could not be found"),
        "merge looked for a submodule commit in the parent odb: {stderr}"
    );
    assert!(out.status.success(), "merge failed: {stderr}");
}

/// Moving a submodule pointer is one insertion and one deletion of the
/// `Subproject commit` line — the shape every meta-repo pointer bump takes.
#[test]
fn fast_forward_over_a_bumped_submodule_stats_as_one_line_changed() {
    let root = temp_root("bump");
    let (src, c0, c1) = submodule_source(&root);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["submodule", "add", "-q", src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);

    // The bump lands on a branch so `main` can fast-forward onto it.
    git(&parent, &["checkout", "-q", "-b", "topic"]);
    git(&parent.join("sub"), &["checkout", "-q", &c0]);
    git(&parent, &["add", "sub"]);
    git(&parent, &["commit", "-q", "-m", "bump sub"]);
    git(&parent, &["checkout", "-q", "main"]);
    // A superproject checkout leaves the submodule worktree where it was, so put it
    // back on `main`'s recorded commit — otherwise the merge refuses a dirty tree
    // before it ever reaches the diffstat under test.
    git(&parent.join("sub"), &["checkout", "-q", &c1]);

    let out = run(&parent, &["merge", "topic"]);
    assert_merge_ok(&out);
    assert_eq!(stat_block(&out), " sub | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n");

    // The pointer really moved; the stat is not describing a no-op.
    let staged = git(&parent, &["ls-files", "--stage", "--", "sub"]);
    assert_eq!(staged.split_whitespace().nth(1), Some(c0.as_str()), "gitlink after merge");
    assert_ne!(c0, c1);

    let _ = std::fs::remove_dir_all(&root);
}

/// A new submodule is one added line, and the `.gitmodules` it comes with sets the
/// name column's width — both rows are git's.
#[test]
fn fast_forward_that_adds_a_submodule_stats_as_one_insertion() {
    let root = temp_root("add");
    let (src, _c0, _c1) = submodule_source(&root);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    std::fs::write(parent.join("base"), b"base\n").unwrap();
    git(&parent, &["add", "base"]);
    git(&parent, &["commit", "-q", "-m", "base"]);

    git(&parent, &["checkout", "-q", "-b", "topic"]);
    git(&parent, &["submodule", "add", "-q", src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);
    git(&parent, &["checkout", "-q", "main"]);

    let out = run(&parent, &["merge", "topic"]);
    assert_merge_ok(&out);
    assert_eq!(
        stat_block(&out),
        " .gitmodules | 3 +++\n sub         | 1 +\n 2 files changed, 4 insertions(+)\n \
         create mode 100644 .gitmodules\n create mode 160000 sub\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Dropping a submodule is the mirror image: one deleted line, and the gitlink id
/// that is only readable as a `Subproject commit` line is the *old* side here.
#[test]
fn fast_forward_that_drops_a_submodule_stats_as_one_deletion() {
    let root = temp_root("drop");
    let (src, _c0, _c1) = submodule_source(&root);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    git(&parent, &["submodule", "add", "-q", src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);

    git(&parent, &["checkout", "-q", "-b", "topic"]);
    git(&parent, &["rm", "-q", "-r", "--cached", "sub"]);
    let _ = std::fs::remove_dir_all(parent.join("sub"));
    git(&parent, &["rm", "-q", "-f", ".gitmodules"]);
    git(&parent, &["commit", "-q", "-m", "drop sub"]);
    git(&parent, &["checkout", "-q", "main"]);

    let out = run(&parent, &["merge", "topic"]);
    assert_merge_ok(&out);
    assert_eq!(
        stat_block(&out),
        " .gitmodules | 3 ---\n sub         | 1 -\n 2 files changed, 4 deletions(-)\n \
         delete mode 100644 .gitmodules\n delete mode 160000 sub\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}
