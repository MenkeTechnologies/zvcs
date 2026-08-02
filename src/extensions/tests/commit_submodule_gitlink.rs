//! `git commit -- <submodule>`, `-i <submodule>` and `-a` must record a moved
//! submodule's checked-out HEAD as the parent gitlink (mode 160000), exactly as
//! stock git does. Regression: commit's pathspec staging walked the worktree for
//! `File`/`Symlink` entries only, so a submodule worktree (`Kind::Repository`) was
//! dropped before it could reach the commit's tree — a path-restricted commit of
//! bumped gitlinks died with `nothing to commit (no changes staged)` even with the
//! gitlinks correctly staged, and `commit -a` skipped them outright. That is the
//! meta-repo's core submodule-bump workflow.
//!
//! A gitlink also has no blob to diff, so the summary's line counts must add git's
//! one-line `Subproject commit <oid>` rendering by hand: a pointer move is one
//! insertion plus one deletion.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// PATH with any zvcs shadow dir removed, so `git` in setup resolves to the real
/// system git (the shadow's own binary is exercised via `BIN` by absolute path).
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .env("PATH", real_git_path())
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn out_str(o: std::process::Output) -> String {
    String::from_utf8(o.stdout).unwrap()
}

/// The gitlink SHA the commit-ish `rev` records for `path`.
fn gitlink_at(parent: &Path, rev: &str, path: &str) -> String {
    out_str(git(parent, &["rev-parse", &format!("{rev}:{path}")])).trim().to_string()
}

/// The staged gitlink SHA for `path` (empty if not staged).
fn staged_gitlink(parent: &Path, path: &str) -> String {
    out_str(git(parent, &["ls-files", "--stage", "--", path]))
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string()
}

/// Two commits in a throwaway source repo, so a pointer can actually move.
struct SubSource {
    path: std::path::PathBuf,
    c0: String,
    c1: String,
}

fn sub_source(root: &Path) -> SubSource {
    let path = root.join("sub_src");
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    git(&path, &["commit", "--allow-empty", "-q", "-m", "s0"]);
    let c0 = out_str(git(&path, &["rev-parse", "HEAD"])).trim().to_string();
    git(&path, &["commit", "--allow-empty", "-q", "-m", "s1"]);
    let c1 = out_str(git(&path, &["rev-parse", "HEAD"])).trim().to_string();
    assert_ne!(c0, c1);
    SubSource { path, c0, c1 }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-commitgitlink-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// A parent repo with `vendor/a` and `vendor/b` embedded at the source's HEAD.
fn parent_with_two_submodules(root: &Path, src: &SubSource) -> std::path::PathBuf {
    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    for name in ["vendor/a", "vendor/b"] {
        git(&parent, &["submodule", "add", "-q", src.path.to_str().unwrap(), name]);
    }
    git(&parent, &["commit", "-q", "-m", "add subs"]);
    for name in ["vendor/a", "vendor/b"] {
        assert_eq!(gitlink_at(&parent, "HEAD", name), src.c1, "precondition: {name} at c1");
    }
    parent
}

#[test]
fn path_restricted_commit_records_moved_gitlink() {
    let root = scratch("only");
    let src = sub_source(&root);
    let parent = parent_with_two_submodules(&root, &src);

    // Move BOTH submodules, then commit only one of them by path.
    for name in ["vendor/a", "vendor/b"] {
        git(&parent.join(name), &["checkout", "-q", &src.c0]);
    }
    git(&parent, &["add", "vendor/a", "vendor/b"]);

    // A staged change to an unrelated path must be disregarded by `--only` mode.
    std::fs::write(parent.join("bystander.txt"), "staged but not committed\n").unwrap();
    git(&parent, &["add", "bystander.txt"]);

    let out = git(&parent, &["commit", "-m", "bump a", "--", "vendor/a"]);
    let text = out_str(out);

    assert_eq!(
        gitlink_at(&parent, "HEAD", "vendor/a"),
        src.c0,
        "the pathspec-limited commit must record vendor/a's checked-out HEAD"
    );
    assert_eq!(
        gitlink_at(&parent, "HEAD", "vendor/b"),
        src.c1,
        "vendor/b was not named by the pathspec and must keep its HEAD value"
    );
    assert_eq!(
        staged_gitlink(&parent, "vendor/a"),
        src.c0,
        "the same path must be left staged in the real index"
    );
    assert!(
        !run(&parent, &["rev-parse", "HEAD:bystander.txt"]).status.success(),
        "--only mode must not commit a staged path the pathspec did not name"
    );
    // A gitlink carries no blob lines; git still scores the pointer move as a
    // one-line `Subproject commit` replacement.
    assert!(
        text.contains("1 file changed, 1 insertion(+), 1 deletion(-)"),
        "gitlink move must be counted as one insertion and one deletion, got: {text}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn include_and_all_modes_record_moved_gitlinks() {
    let root = scratch("incall");
    let src = sub_source(&root);
    let parent = parent_with_two_submodules(&root, &src);

    // `-i <path>` adds the named path to the index, then commits the whole index.
    git(&parent.join("vendor/a"), &["checkout", "-q", &src.c0]);
    git(&parent, &["commit", "-m", "include a", "-i", "vendor/a"]);
    assert_eq!(gitlink_at(&parent, "HEAD", "vendor/a"), src.c0, "-i must stage the gitlink");

    // `-a` auto-stages every tracked change, submodule pointers included.
    git(&parent.join("vendor/b"), &["checkout", "-q", &src.c0]);
    git(&parent, &["commit", "-am", "all b"]);
    assert_eq!(gitlink_at(&parent, "HEAD", "vendor/b"), src.c0, "-a must stage the gitlink");

    // An unmoved submodule is not restaged: a second `-a` has nothing to do and
    // fails the way git's empty commit does, leaving HEAD alone.
    let head_before = out_str(git(&parent, &["rev-parse", "HEAD"]));
    let again = run(&parent, &["commit", "-am", "noop"]);
    assert!(!again.status.success(), "an unchanged gitlink must not produce a commit");
    assert_eq!(out_str(git(&parent, &["rev-parse", "HEAD"])), head_before, "HEAD must not move");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn commit_stages_removed_and_unresolvable_submodule_dirs() {
    let root = scratch("gone");
    let src = sub_source(&root);
    let parent = parent_with_two_submodules(&root, &src);

    // A submodule worktree that is gone entirely is a deletion, as for any file.
    std::fs::remove_dir_all(parent.join("vendor/a")).unwrap();
    git(&parent, &["commit", "-am", "drop a"]);
    assert!(
        !run(&parent, &["rev-parse", "HEAD:vendor/a"]).status.success(),
        "a removed submodule worktree must delete the gitlink"
    );

    // A directory git cannot resolve a HEAD for reads as UNCHANGED
    // (`ce_compare_gitlink`), so the entry is neither restaged nor deleted.
    std::fs::remove_dir_all(parent.join("vendor/b")).unwrap();
    std::fs::create_dir_all(parent.join("vendor/b")).unwrap();
    let noop = run(&parent, &["commit", "-am", "empty b"]);
    assert!(!noop.status.success(), "an unresolvable submodule dir must look unchanged");
    assert_eq!(
        gitlink_at(&parent, "HEAD", "vendor/b"),
        src.c1,
        "an uninitialized submodule must keep its recorded gitlink"
    );

    let _ = std::fs::remove_dir_all(&root);
}
