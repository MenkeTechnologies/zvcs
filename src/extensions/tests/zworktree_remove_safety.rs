//! `git zworktree remove` deletes directories it reads out of a *file inside the
//! tree it is deleting*, so the pointer's word is not enough.
//!
//! `provision()` writes a round trip: the worktree's `.git` says
//! `gitdir: <G>/worktrees/<name>`, and `<G>/worktrees/<name>/gitdir` says
//! `<wt>/.git`. `remove` may `remove_dir_all` a path outside the worktree **only**
//! when that round trip closes and the directory looks like the metadata
//! `provision` wrote (right shape, a real directory, a `commondir` beside it).
//! Everything else is refused by name on stderr and left on disk.
//!
//! This is a safety property, not a parity one — `zworktree` is a zvcs-only verb,
//! so there is no stock git to differ from. Each case below plants one hostile or
//! broken pointer, and asserts three things: the outside directory survives, the
//! refusal names the reason, and the command does not report success.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the binary under test with an isolated environment and identity, so no
/// ambient config or home directory can reach the run.
fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// Same, asserting success — used only to build fixtures.
fn ok(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(cwd, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The pieces every case needs: an isolated root, a one-commit repository, a
/// directory outside the worktree that must survive, and a provisioned worktree.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    outsider: PathBuf,
    worktree: PathBuf,
    name: String,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-zwtsafe-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();

        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        ok(&repo, &home, &["init", "-q", "-b", "main", "."]);
        std::fs::write(repo.join("a.txt"), b"a\n").unwrap();
        ok(&repo, &home, &["add", "a.txt"]);
        ok(&repo, &home, &["commit", "-q", "-m", "c0"]);

        // The directory every case is trying to trick `remove` into deleting.
        let outsider = root.join("outsider");
        std::fs::create_dir_all(&outsider).unwrap();
        std::fs::write(outsider.join("keep.txt"), b"precious\n").unwrap();

        let name = tag.to_owned();
        ok(&repo, &home, &["zworktree", "add", &name]);
        let worktree = home.join("worktrees").join(&name);
        assert!(worktree.is_dir(), "fixture worktree was not provisioned");

        Fixture { root, home, repo, outsider, worktree, name }
    }

    /// Overwrite the worktree's `.git` pointer with `body`.
    fn plant(&self, body: &str) {
        std::fs::write(self.worktree.join(".git"), body).unwrap();
    }

    fn remove(&self) -> Output {
        run(&self.repo, &self.home, &["zworktree", "remove", &self.name])
    }

    /// The invariant every hostile case shares: the outside directory is still
    /// there, `remove` did not claim success, and stderr says why.
    fn assert_refused(&self, out: &Output, expect: &str) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            self.outsider.join("keep.txt").is_file(),
            "`zworktree remove` deleted a directory outside the worktree\nstderr:\n{stderr}"
        );
        assert!(
            !out.status.success(),
            "refusing to delete must not exit 0\nstderr:\n{stderr}"
        );
        assert!(
            stderr.contains(expect),
            "refusal did not name the reason (wanted {expect:?}):\n{stderr}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The plain attack: an absolute path with no relation to this repository. The
/// pre-fix code ran `remove_dir_all` on exactly this string.
#[test]
fn an_absolute_gitdir_outside_the_repository_is_refused() {
    let f = Fixture::new("outside");
    f.plant(&format!("gitdir: {}\n", f.outsider.display()));
    let out = f.remove();
    f.assert_refused(&out, "is not a `worktrees/outside` metadata directory");
}

/// A `.git` file that is not a pointer at all. Silently ignoring it hid the fact
/// that the metadata was never pruned.
#[test]
fn a_malformed_git_file_is_refused() {
    let f = Fixture::new("malformed");
    f.plant("this is not a pointer\n");
    let out = f.remove();
    f.assert_refused(&out, "is not a `gitdir:` pointer");
}

/// Two `gitdir:` lines is not git's format either — the second could not be seen
/// by a `trim().strip_prefix()` read of the whole body.
#[test]
fn a_multi_line_git_file_is_refused() {
    let f = Fixture::new("multiline");
    f.plant(&format!("gitdir: {}\ngitdir: {}\n", f.outsider.display(), f.outsider.display()));
    let out = f.remove();
    f.assert_refused(&out, "is not a single `gitdir:` line");
}

/// `provision` always writes an absolute path, so a relative one that climbs out
/// of the worktree cannot be metadata this command wrote.
#[test]
fn a_relative_gitdir_that_escapes_the_worktree_is_refused() {
    let f = Fixture::new("relative");
    f.plant("gitdir: ../../../outsider\n");
    let out = f.remove();
    f.assert_refused(&out, "relative gitdir `../../../outsider` that leaves the worktree");
}

/// A relative pointer that stays *inside* the tree is the nested-clone case, and
/// is not an attack: the tree removal takes it, so there is nothing to refuse and
/// nothing to complain about.
#[test]
fn a_relative_gitdir_inside_the_worktree_is_not_refused() {
    let f = Fixture::new("nested");
    let nested = f.worktree.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join(".git"), "gitdir: ../.git/modules/nested\n").unwrap();

    let out = f.remove();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a nested in-tree pointer must not make `remove` fail:\n{stderr}"
    );
    assert!(stderr.is_empty(), "nothing should be reported for an in-tree pointer:\n{stderr}");
    assert!(!f.worktree.exists(), "the worktree tree should still be gone");
}

/// The shape is right but the directory is a symlink. Following it would delete
/// whatever it points at.
#[cfg(unix)]
#[test]
fn a_symlinked_metadata_directory_is_refused() {
    let f = Fixture::new("symmeta");
    let fake = f.root.join("fake").join("worktrees");
    std::fs::create_dir_all(&fake).unwrap();
    std::os::unix::fs::symlink(&f.outsider, fake.join("symmeta")).unwrap();

    f.plant(&format!("gitdir: {}\n", fake.join("symmeta").display()));
    let out = f.remove();
    f.assert_refused(&out, "is not a directory");
}

/// A `.git` *symlink* is never collected as a pointer by the walk, so without an
/// explicit check the run would say nothing at all while leaving the real
/// metadata and the `zwt/<name>` branch behind.
#[cfg(unix)]
#[test]
fn a_symlinked_git_file_is_refused() {
    let f = Fixture::new("symgit");
    let target = f.outsider.join("keep.txt");
    std::fs::remove_file(f.worktree.join(".git")).unwrap();
    std::os::unix::fs::symlink(&target, f.worktree.join(".git")).unwrap();

    let out = f.remove();
    f.assert_refused(&out, "is a symlink, not a `gitdir:` pointer");
}

/// No pointer at all: there is nothing left that identifies which metadata
/// belonged to this worktree, so none is deleted and the run says so.
#[test]
fn a_missing_git_file_is_refused() {
    let f = Fixture::new("nogit");
    std::fs::remove_file(f.worktree.join(".git")).unwrap();

    let out = f.remove();
    f.assert_refused(&out, "so its metadata cannot be identified");
    assert!(
        f.repo.join(".git/worktrees/nogit").is_dir(),
        "unidentifiable metadata must be left in place, not guessed at"
    );
}

/// A well-formed pointer at a directory that simply is not there. The old code's
/// `let _ =` swallowed the `ENOENT` and reported success.
#[test]
fn a_gitdir_naming_a_missing_directory_is_refused() {
    let f = Fixture::new("gone");
    f.plant(&format!("gitdir: {}/nowhere/worktrees/gone\n", f.root.display()));
    let out = f.remove();
    f.assert_refused(&out, "cannot be read");
}

/// The round trip is the check that actually confines the deletion: right shape,
/// real directory, real `commondir`, but its `gitdir` names someone else's `.git`.
/// Nothing about the path alone distinguishes this from the legitimate case.
#[test]
fn metadata_that_points_back_elsewhere_is_refused() {
    let f = Fixture::new("roundtrip");
    let meta = f.root.join("fake").join("worktrees").join("roundtrip");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join("commondir"), "../..\n").unwrap();
    std::fs::write(meta.join("gitdir"), "/somewhere/else/.git\n").unwrap();

    f.plant(&format!("gitdir: {}\n", meta.display()));
    let out = f.remove();
    f.assert_refused(&out, "points back at /somewhere/else/.git rather than at itself");
    assert!(meta.is_dir(), "a directory that failed the round trip must be left alone");
}

/// The shape check on its own: a directory that *is* real linked-worktree
/// metadata, but for a different worktree name.
#[test]
fn metadata_for_a_different_worktree_name_is_refused() {
    let f = Fixture::new("othername");
    ok(&f.repo, &f.home, &["zworktree", "add", "sibling"]);
    let sibling_meta = f.repo.join(".git/worktrees/sibling");
    assert!(sibling_meta.is_dir(), "sibling fixture missing");

    f.plant(&format!("gitdir: {}\n", sibling_meta.display()));
    let out = f.remove();
    f.assert_refused(&out, "is not a `worktrees/othername` metadata directory");
    assert!(sibling_meta.is_dir(), "another worktree's metadata must survive");
}

/// The whole point of the gate is that the legitimate path still works: a
/// worktree `add` provisioned is torn down completely — tree, metadata, and the
/// `zwt/<name>` branch — and reports success.
#[test]
fn a_legitimate_remove_still_tears_everything_down() {
    let f = Fixture::new("legit");
    let meta = f.repo.join(".git/worktrees/legit");
    assert!(meta.is_dir(), "fixture metadata missing");

    let out = f.remove();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "legitimate remove failed:\n{stderr}");
    assert!(stderr.is_empty(), "legitimate remove should be quiet:\n{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "removed worktree 'legit'\n");

    assert!(!f.worktree.exists(), "worktree tree survived");
    assert!(!meta.exists(), "linked-worktree metadata survived");

    let refs = ok(&f.repo, &f.home, &["for-each-ref", "--format=%(refname)", "refs/heads/zwt/"]);
    assert!(
        String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "the zwt/legit branch survived: {}",
        String::from_utf8_lossy(&refs.stdout)
    );

    let listing = ok(&f.repo, &f.home, &["zworktree", "list"]);
    assert!(
        !String::from_utf8_lossy(&listing.stdout).contains("legit"),
        "the removed worktree is still listed"
    );
}

/// A worktree root that is itself a symlink is not one this command provisioned,
/// and `remove_dir_all` through it would work on the link's target.
#[cfg(unix)]
#[test]
fn a_symlinked_worktree_root_is_refused() {
    let f = Fixture::new("symroot");
    // Replace the recorded worktree path with a symlink to the outsider.
    std::fs::remove_dir_all(&f.worktree).unwrap();
    std::os::unix::fs::symlink(&f.outsider, &f.worktree).unwrap();

    let out = f.remove();
    f.assert_refused(&out, "is not a directory");
}
