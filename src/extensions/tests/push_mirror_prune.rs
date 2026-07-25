//! `git push --mirror` / `--prune` — the deletion half of push, plus the pattern
//! refspecs that make pruning meaningful.
//!
//! The rules being pinned, each measured against stock git before being written
//! down:
//!
//!   * `--mirror` pushes every ref under `refs/` (branches, tags, and the
//!     remote-tracking refs too) and deletes remote refs this repository no
//!     longer has.
//!   * `--prune` deletes only inside the namespaces the push's refspecs COVER.
//!     `git push --prune origin main` therefore prunes nothing — the refspec
//!     covers one ref. Getting this wrong deletes remote branches stock git
//!     keeps, so the negative case is the important one here.
//!   * A pattern refspec (`refs/heads/*:refs/heads/*`) expands to one update per
//!     matching local ref.
//!
//! Hermetic: the bare remote, the work repo, every commit and every push run
//! through the binary under test over its own local transport, with a PATH shim
//! pointing `git-receive-pack` at that same binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn shim(root: &Path) -> PathBuf {
    let dir = root.join("shim");
    std::fs::create_dir_all(&dir).expect("mkdir shim");
    for name in ["git", "git-upload-pack", "git-receive-pack"] {
        let link = dir.join(name);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(BIN, &link).expect("symlink shim");
    }
    dir
}

fn run(dir: &Path, shim_dir: &Path, args: &[&str]) -> Output {
    let path = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap_or_default());
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .output()
        .expect("run zvcs git")
}

fn ok(dir: &Path, shim_dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, shim_dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

/// The remote's branch names, sorted.
fn remote_branches(work: &Path, shim_dir: &Path) -> Vec<String> {
    let out = ok(work, shim_dir, &["ls-remote", "../remote.git"]);
    let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter_map(|r| r.strip_prefix("refs/heads/"))
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}

/// A bare remote plus a work repo with `main`, `topic` and `keepme` already
/// pushed, then `topic` deleted locally. Returns `(shim, work)`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-mirror-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir root");
    let shim_dir = shim(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    ok(&root, &shim_dir, &["init", "-q", "--bare", "remote.git"]);
    ok(&work, &shim_dir, &["init", "-q", "-b", "main"]);
    ok(&work, &shim_dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    ok(&work, &shim_dir, &["branch", "topic"]);
    ok(&work, &shim_dir, &["branch", "keepme"]);
    ok(&work, &shim_dir, &["remote", "add", "origin", "../remote.git"]);
    ok(&work, &shim_dir, &["push", "-q", "origin", "main", "topic", "keepme"]);
    ok(&work, &shim_dir, &["branch", "-D", "topic"]);
    (shim_dir, work)
}

#[test]
fn prune_with_an_explicit_refspec_deletes_nothing() {
    let (shim_dir, work) = fixture("explicit");

    let out = ok(&work, &shim_dir, &["push", "--prune", "origin", "main"]);

    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("[deleted]"),
        "one refspec covers one ref, so nothing is in prune scope: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(remote_branches(&work, &shim_dir), vec!["keepme", "main", "topic"]);
}

#[test]
fn prune_with_a_pattern_refspec_deletes_the_stale_branch() {
    let (shim_dir, work) = fixture("pattern");

    let out = ok(&work, &shim_dir, &["push", "--prune", "origin", "refs/heads/*:refs/heads/*"]);

    assert!(
        String::from_utf8_lossy(&out.stderr).contains(" - [deleted]"),
        "the pattern covers refs/heads/, so the branch with no local counterpart goes: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        remote_branches(&work, &shim_dir),
        vec!["keepme", "main"],
        "only the deleted branch is pruned"
    );
}

#[test]
fn mirror_pushes_every_ref_and_deletes_what_is_gone() {
    let (shim_dir, work) = fixture("mirror");
    ok(&work, &shim_dir, &["tag", "-a", "v1", "-m", "annotated"]);

    let out = ok(&work, &shim_dir, &["push", "--mirror", "origin"]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(text.contains(" - [deleted]"), "the stale branch is deleted: {text}");
    assert_eq!(remote_branches(&work, &shim_dir), vec!["keepme", "main"]);

    // The annotated tag travelled as its TAG object, so the remote can peel it.
    let ls = ok(&work, &shim_dir, &["ls-remote", "../remote.git"]);
    let listing = String::from_utf8_lossy(&ls.stdout);
    assert!(listing.contains("refs/tags/v1"), "the tag is on the remote: {listing}");
    assert!(
        listing.contains("refs/tags/v1^{}"),
        "a peeled line means an annotated tag object, not a lightweight ref: {listing}"
    );
}

#[test]
fn up_to_date_refs_are_listed_only_under_verbose() {
    let (shim_dir, work) = fixture("verbose");

    let quiet = ok(&work, &shim_dir, &["push", "origin", "main"]);
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("[up to date]"),
        "git's default block shows only what moved: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );

    let loud = ok(&work, &shim_dir, &["push", "-v", "origin", "main"]);
    assert!(
        String::from_utf8_lossy(&loud.stderr).contains("[up to date]"),
        "-v lists the unchanged ref: {}",
        String::from_utf8_lossy(&loud.stderr)
    );
}

#[test]
fn push_options_are_refused_when_the_server_lacks_the_capability() {
    let (shim_dir, work) = fixture("pushopt");

    let out = run(&work, &shim_dir, &["push", "-o", "ci.skip", "origin", "main"]);

    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not support push options"),
        "a dropped option would silently change what the server was told: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
