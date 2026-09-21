//! `git difftool` hands the tool the *live work-tree file* for a side whose
//! content the index says is already on disk, instead of inflating a second temp
//! copy. That is `reuse_worktree_file()` (diff.c:4388-4467), reached from
//! `prepare_temp_file()` (diff.c:4716), and it is what makes an edit made inside
//! the diff tool land in the work tree rather than in a throwaway file.
//!
//! The rule has two halves and both are pinned here:
//!
//! ```text
//!   * the index must be read at all — `if (!istate->cache) return 0`
//!     (diff.c:4410). `cmd_diff()` (builtin/diff.c:611-640) reads it only on the
//!     `builtin_diff_files()` (`ent.nr == 0`) and `builtin_diff_index()`
//!     (`ent.nr == 1`) arms, so `git difftool <a> <b>` and `git difftool <a>..<b>`
//!     reach `builtin_diff_tree()` and reuse nothing.
//!   * the entry must match — same object id, regular file, no `CE_VALID`
//!     ("assume unchanged") or skip-worktree bit, and stat-clean
//!     (diff.c:4440-4465).
//! ```
//!
//! Every expectation below is stock git 2.55.0's measured behaviour, expressed as
//! an absolute assertion rather than a live comparison against whatever `git` a CI
//! image happens to ship. The tool is a shell script that prints its argv and
//! exits, so no GUI, no editor, and no network is involved.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        // Keep every read and write inside the fixture: a failed `init` must not
        // let discovery walk up into the real repository.
        .env("GIT_CEILING_DIRECTORIES", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .status()
        .unwrap();
    assert!(ok.success(), "git {args:?} failed");
}

/// A two-commit repository whose work tree is clean at `HEAD`:
///
/// ```text
///   HEAD~1: a.txt, b.txt
///   HEAD:   a.txt and b.txt modified, c.txt added
/// ```
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-dt-reuse-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join("b.txt"), "x\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "first"]);
    std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(repo.join("b.txt"), "y\n").unwrap();
    std::fs::write(repo.join("c.txt"), "new\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "second"]);

    // The `--extcmd` stands in for the diff tool: it records the two paths it was
    // handed, one line per launch, and nothing else.
    let tool = root.join("recordargv");
    std::fs::write(&tool, "#!/bin/sh\nprintf '%s|%s\\n' \"$1\" \"$2\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    repo
}

/// Run `difftool -x <recordargv> -y <args…>` and return one `left|right` line per
/// launch, with every path that lies under a temp directory folded to `TMP` so
/// only the *reuse decision* is compared, not the temp file's name.
fn launches(repo: &Path, args: &[&str]) -> Vec<String> {
    let tool = repo.parent().unwrap().join("recordargv");
    let out = Command::new(BIN)
        .arg("difftool")
        .args(["-x", tool.to_str().unwrap(), "-y"])
        .args(args)
        .current_dir(repo)
        .env("GIT_CEILING_DIRECTORIES", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "difftool {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            line.split('|')
                .map(|p| {
                    // A reused work-tree file is named by its work-tree-relative
                    // path; anything absolute is a staged temp.
                    if p == "/dev/null" || !p.starts_with('/') {
                        p.to_owned()
                    } else {
                        "TMP".to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

/// `git difftool <commit>` is `builtin_diff_index()`: the index is read, so every
/// clean path's right-hand side is the work-tree file itself, named relatively.
#[test]
fn diff_index_reuses_the_clean_work_tree_file() {
    let repo = fixture("index");
    assert_eq!(
        launches(&repo, &["HEAD~1"]),
        vec!["TMP|a.txt", "TMP|b.txt", "/dev/null|c.txt"],
    );
}

/// `--cached` is still `builtin_diff_index()` (builtin/diff.c:633), so the reuse
/// decision is unchanged: the index entry and the work-tree file agree.
#[test]
fn diff_index_cached_reuses_the_clean_work_tree_file() {
    let repo = fixture("cached");
    assert_eq!(
        launches(&repo, &["--cached", "HEAD~1"]),
        vec!["TMP|a.txt", "TMP|b.txt", "/dev/null|c.txt"],
    );
}

/// Two tree-ish operands reach `builtin_diff_tree()` (builtin/diff.c:638), which
/// never calls `repo_read_index()`. `reuse_worktree_file()`'s `if (!istate->cache)`
/// therefore declines every path even though the work tree holds exactly the bytes
/// `HEAD` records — both sides are staged temps.
#[test]
fn diff_tree_never_reuses_even_with_a_clean_work_tree() {
    let repo = fixture("tree");
    assert_eq!(
        launches(&repo, &["HEAD~1", "HEAD"]),
        vec!["TMP|TMP", "TMP|TMP", "/dev/null|TMP"],
    );
}

/// A `<a>..<b>` range is two pending objects, so it is the same
/// `builtin_diff_tree()` arm as two separate operands.
#[test]
fn a_range_is_a_tree_diff_and_reuses_nothing() {
    let repo = fixture("range");
    assert_eq!(
        launches(&repo, &["HEAD~1..HEAD"]),
        vec!["TMP|TMP", "TMP|TMP", "/dev/null|TMP"],
    );
}

/// `CE_VALID` means "the work tree is not guaranteed to match this entry"
/// (diff.c:4453-4458), so the marked path alone falls back to a staged temp while
/// its neighbours still reuse.
#[test]
fn assume_unchanged_declines_reuse_for_that_path_only() {
    let repo = fixture("assume");
    git(&repo, &["update-index", "--assume-unchanged", "a.txt"]);
    assert_eq!(
        launches(&repo, &["HEAD~1"]),
        vec!["TMP|TMP", "TMP|b.txt", "/dev/null|c.txt"],
    );
}

/// A path modified in the work tree has no valid id on the unstaged side at all
/// (`!one->oid_valid`, diff.c:4715) and is borrowed whole. It is named by
/// `one->path` — the work-tree-relative path, with no `./` in front of it.
#[test]
fn a_dirty_path_is_borrowed_and_named_relatively() {
    let repo = fixture("dirty");
    std::fs::write(repo.join("b.txt"), "y\ndirty\n").unwrap();
    assert_eq!(launches(&repo, &["HEAD"]), vec!["TMP|b.txt"]);
}
