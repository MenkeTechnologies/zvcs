//! `show-ref -d` peels the way `reference_get_peeled_oid()` does.
//!
//! `show_one()` prints the `^{}` line whenever `reference_get_peeled_oid()`
//! succeeds (builtin/show-ref.c:56-59). That takes the `^` line packed-refs
//! recorded for the ref as given, and otherwise calls `peel_object()`
//! (refs.c:2486-2496), which parses each tag in the chain but only *looks up*
//! the tag's target under the type its `type` header names (object.c:211-250).
//! So a tag whose target commit is missing still peels — to that commit's id —
//! and a packed ref's recorded peel is printed without reading any object.
//!
//! zvcs peeled with a full object walk: the tag with a missing target printed
//! no `^{}` line at all.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const GONE: &str = "1111111111111111111111111111111111111111";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `main` at one commit, annotated `ann` on it, and `refs/tags/dangling`: a
    /// real tag object whose target commit does not exist.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-show-ref-peel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.run(&["tag", "-a", "-m", "ann", "ann"]);
        let body = format!(
            "object {GONE}\ntype commit\ntag dangling\ntagger C <c@example.com> 1700000000 +0000\n\ndangling\n"
        );
        let tag_path = f.root.join("tag-body");
        std::fs::write(&tag_path, body).unwrap();
        let (oid, _, code) = f.run(&["hash-object", "-t", "tag", "-w", tag_path.to_str().unwrap()]);
        assert_eq!(code, 0);
        f.run(&["update-ref", "refs/tags/dangling", oid.trim()]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, rev: &str) -> String {
        self.run(&["rev-parse", rev]).0.trim().to_string()
    }
}

#[test]
fn a_tag_whose_target_is_missing_peels_to_the_target_id() {
    let f = Fixture::new("dangling");
    let tag = f.oid("refs/tags/dangling");
    let want = format!("{tag} refs/tags/dangling\n{GONE} refs/tags/dangling^{{}}\n");
    assert_eq!(f.run(&["show-ref", "-d", "dangling"]), (want.clone(), String::new(), 0));
    assert_eq!(
        f.run(&["show-ref", "--verify", "-d", "refs/tags/dangling"]),
        (want, String::new(), 0)
    );
    // The listing keeps going past it, and the healthy tag peels as before.
    let want = format!(
        "{main} refs/heads/main\n{ann} refs/tags/ann\n{main} refs/tags/ann^{{}}\n\
         {tag} refs/tags/dangling\n{GONE} refs/tags/dangling^{{}}\n",
        main = f.oid("main"),
        ann = f.oid("refs/tags/ann"),
    );
    assert_eq!(f.run(&["show-ref", "-d"]), (want, String::new(), 0));
}

#[test]
fn a_packed_peel_line_is_printed_as_recorded() {
    let f = Fixture::new("packed");
    f.run(&["update-ref", "-d", "refs/tags/dangling"]);
    assert_eq!(f.run(&["pack-refs", "--all"]).2, 0);
    let packed = f.work.join(".git/packed-refs");
    let text = std::fs::read_to_string(&packed).unwrap();
    let main = f.oid("main");
    let bogus = "2222222222222222222222222222222222222222";
    let rewritten = text.replace(&format!("^{main}\n"), &format!("^{bogus}\n"));
    assert_ne!(text, rewritten, "pack-refs recorded ann's peel");
    std::fs::write(&packed, rewritten).unwrap();
    let want = format!(
        "{} refs/tags/ann\n{bogus} refs/tags/ann^{{}}\n",
        f.oid("refs/tags/ann")
    );
    assert_eq!(f.run(&["show-ref", "-d", "ann"]), (want, String::new(), 0));
}
