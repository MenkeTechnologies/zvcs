//! `git rev-parse --short <tree-ish>:<submodule>` must abbreviate a gitlink to the
//! requested width, the same as stock git.
//!
//! A gitlink names a commit that lives in the submodule's object database, not the
//! parent's, so the parent has nothing to disambiguate the prefix against.
//! `find_unique_abbrev_r()` (object-name.c:900-916) returns early with the caller's
//! `len` when the lookup misses — git neither fails nor widens back to full hex.
//!
//! Regression: `render_id` propagated gix's `shorten()` error for bare `--short`
//! (exit 1, "Id could not be shortened"), and for `--short=n` fell through to the
//! full 40-char hash while still exiting 0. The second was the worse half — a
//! silent wrong-width answer that a caller slicing a fixed column cannot detect.
//! Both broke `rev-parse --short HEAD:<submodule>`, the ordinary way to read a
//! recorded submodule pointer in the meta-repo bump workflow.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// An object id of valid shape that is deliberately absent from the repository —
/// exactly the state a real gitlink is in from the parent's point of view.
const ABSENT_OID: &str = "0123456789abcdef0123456789abcdef01234567";

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

/// Run the built binary, asserting exit 0 — which is itself half the regression:
/// bare `--short` used to exit 1 here.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
        .args(args)
        .env("PATH", real_git_path())
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_string()
}

/// A repository whose HEAD tree carries a gitlink at `sub` pointing at
/// [`ABSENT_OID`]. `update-index --cacheinfo` does not validate mode-160000
/// entries against the odb, which is what lets the absent-object state exist.
///
/// `slug` keeps concurrent test threads in separate trees.
fn repo_with_gitlink(slug: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir()
        .join(format!("zvcs-revparse-gitlink-{slug}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    git(&root, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("file.txt"), b"seed\n").expect("write seed");
    git(&root, &["add", "file.txt"]);
    git(&root, &["update-index", "--add", "--cacheinfo", &format!("160000,{ABSENT_OID},sub")]);
    git(&root, &["commit", "-qm", "seed with gitlink"]);
    root
}

#[test]
fn short_abbreviates_gitlink_instead_of_failing() {
    let p = &repo_with_gitlink("short");

    // Pinned rather than left on `auto`, whose width scales with object count.
    let got = git(p, &["-c", "core.abbrev=10", "rev-parse", "--short", "HEAD:sub"]);
    assert_eq!(got, &ABSENT_OID[..10], "bare --short must cut at core.abbrev");
}

#[test]
fn short_with_length_honours_the_requested_width() {
    let p = &repo_with_gitlink("width");

    // The silent half of the regression: these used to return all 40 chars, rc=0.
    for n in [4usize, 8, 12, 20] {
        let got = git(p, &["rev-parse", &format!("--short={n}"), "HEAD:sub"]);
        assert_eq!(got.len(), n, "--short={n} returned {} chars: {got}", got.len());
        assert_eq!(got, &ABSENT_OID[..n], "--short={n} must be the id's own prefix");
    }
}

#[test]
fn full_and_max_width_still_print_the_whole_id() {
    let p = &repo_with_gitlink("full");

    assert_eq!(git(p, &["rev-parse", "HEAD:sub"]), ABSENT_OID);
    assert_eq!(git(p, &["rev-parse", "--short=40", "HEAD:sub"]), ABSENT_OID);
}

#[test]
fn present_objects_keep_disambiguating_against_the_odb() {
    let p = &repo_with_gitlink("present");

    // The fallback must not swallow the normal path: HEAD is in this odb, so the
    // width still comes from disambiguation rather than a blind cut.
    let full = git(p, &["rev-parse", "HEAD"]);
    assert_eq!(full.len(), 40);
    for n in [7usize, 12] {
        let got = git(p, &["rev-parse", &format!("--short={n}"), "HEAD"]);
        assert_eq!(got.len(), n, "--short={n} on a present object");
        assert_eq!(got, &full[..n]);
    }
    let auto = git(p, &["-c", "core.abbrev=10", "rev-parse", "--short", "HEAD"]);
    assert_eq!(auto, &full[..10]);
}
