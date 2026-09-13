//! A cached blame must not cross into a repository with a different ancestry.
//!
//! `blame` memoises a whole-file attribution machine-wide in
//! `~/.zvcs/cache/blame`, keyed by the suspect commit, the path and the diff
//! options. The commit id pins the objects but not the history git walks from
//! it: `parse_commit_buffer()` (commit.c:554-581) takes a commit's parents from
//! the graft table when it has an entry, and a shallow clone registers every
//! boundary there (`register_shallow()`, shallow.c:34). So a full clone and a
//! `--depth 1` clone of the same history blame the same `(commit, path)` to
//! different commits:
//!
//! ```text
//! $ git -C full blame -s f          $ git -C shallow blame -s f   # stock 2.55.0
//! ^6f3af4a 1) one                   ^d345795 1) one
//! d3457952 2) two                   ^d345795 2) two
//! ```
//!
//! Before the key carried the ancestry, the shallow clone read the full clone's
//! entry and died on the commit it names but does not hold:
//! `zvcs: blame: An object with id 6f3af4a… could not be found`.
//!
//! Every test runs the built binary against its own `ZVCS_HOME`, so the
//! developer's real cache is never read or written.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zvcs-blame-cache-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        Fixture { root, home }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("ZVCS_HOME", &self.home)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        (
            out.status.code().expect("exited via a signal"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.run(dir, args);
        assert_eq!(code, 0, "`git {args:?}` failed: {stderr}");
        stdout
    }

    /// Two commits: `one` in the first, `two` appended in the second.
    fn full_repo(&self) -> PathBuf {
        let full = self.root.join("full");
        std::fs::create_dir_all(&full).unwrap();
        self.ok(&full, &["init", "-q", "-b", "main", "."]);
        std::fs::write(full.join("f"), "one\n").unwrap();
        self.ok(&full, &["add", "f"]);
        self.ok(&full, &["commit", "-q", "-m", "c1"]);
        std::fs::write(full.join("f"), "one\ntwo\n").unwrap();
        self.ok(&full, &["commit", "-q", "-a", "-m", "c2"]);
        full
    }
}

/// The commit each `blame -s` line is attributed to, boundary caret included.
fn attributions(blame: &str) -> Vec<String> {
    blame.lines().map(|l| l.split_whitespace().next().unwrap_or_default().to_string()).collect()
}

/// The reproduction: blame in the full clone first, so the cache holds its
/// attribution, then the same `(commit, path)` in a shallow clone sharing the
/// cache home. The shallow clone must print its own boundary for both lines —
/// what stock git 2.55.0 prints — and not the full clone's history.
#[test]
fn a_full_clone_blame_is_not_served_in_a_shallow_clone() {
    let f = Fixture::new("shallow");
    let full = f.full_repo();
    let head = f.ok(&full, &["rev-parse", "HEAD"]).trim_end().to_string();
    let first = f.ok(&full, &["rev-parse", "HEAD~1"]).trim_end().to_string();

    let full_blame = attributions(&f.ok(&full, &["blame", "-s", "f"]));
    assert!(full_blame[0].starts_with(&format!("^{}", &first[..7])), "full: {full_blame:?}");
    assert!(head.starts_with(&full_blame[1]), "full: {full_blame:?}");

    let url = format!("file://{}", full.display());
    f.ok(&f.root, &["clone", "-q", "--depth", "1", &url, "shallow"]);
    let shallow = f.root.join("shallow");
    assert!(shallow.join(".git/shallow").is_file(), "the clone must be shallow");

    let (code, stdout, stderr) = f.run(&shallow, &["blame", "-s", "f"]);
    assert_eq!(code, 0, "shallow blame failed: {stderr}");
    let boundary = format!("^{}", &head[..7]);
    assert_eq!(
        attributions(&stdout),
        vec![boundary.clone(), boundary],
        "every line belongs to the shallow boundary"
    );

    // And the other direction: the shallow clone's entry must not answer the full
    // clone, whose history reaches past that boundary.
    assert_eq!(attributions(&f.ok(&full, &["blame", "-s", "f"])), full_blame);
}

/// A replace ref rewrites the history the same way a graft does
/// (`lookup_replace_object()`, odb.c:558). Replacing the second commit with a
/// parentless copy makes it a root, so its blame credits it with every line; a
/// cached attribution from before the replacement must not be served after it.
#[test]
fn a_replace_ref_does_not_read_the_unreplaced_blame() {
    let f = Fixture::new("replace");
    let full = f.full_repo();
    let head = f.ok(&full, &["rev-parse", "HEAD"]).trim_end().to_string();
    let before = attributions(&f.ok(&full, &["blame", "-s", "f"]));
    assert!(!before[0].contains(&head[..7]), "line one predates HEAD: {before:?}");

    // HEAD's commit object without its `parent` header.
    let raw = f.ok(&full, &["cat-file", "commit", "HEAD"]);
    let orphan: String = raw.lines().filter(|l| !l.starts_with("parent ")).map(|l| format!("{l}\n")).collect();
    let orphan_path = f.root.join("orphan");
    std::fs::write(&orphan_path, orphan).unwrap();
    let orphan_id = f
        .ok(&full, &["hash-object", "-t", "commit", "-w", orphan_path.to_str().unwrap()])
        .trim_end()
        .to_string();
    f.ok(&full, &["replace", &head, &orphan_id]);

    let after = attributions(&f.ok(&full, &["blame", "-s", "f"]));
    assert_eq!(after.len(), 2);
    for line in &after {
        assert!(
            !line.contains(&before[0].trim_start_matches('^')[..7]),
            "the replaced history has no first commit, got {after:?}"
        );
    }
}
