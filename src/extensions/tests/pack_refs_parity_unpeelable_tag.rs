//! `git pack-refs --all` packs a tag it cannot peel, without a peeled line.
//!
//! `ref_transaction_update()` peels an annotated tag with
//! `peel_object(…, PEEL_OBJECT_VERIFY_TAGGED_OBJECT_TYPE)` and records the result
//! only when that succeeds (refs.c:1442-1446). A tag whose target is missing is
//! `PEEL_INVALID` (object.c:230-245): the ref is written to `packed-refs` with no
//! `^` line, and the command succeeds.
//!
//! zvcs's packed transaction treated any missing object met while peeling as
//! fatal — `The packed transaction could not be prepared … peeling` — and packed
//! nothing at all.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pack-refs-unpeelable-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("t"), "a\n").unwrap();
        f.git(&["add", "t"]);
        f.git(&["commit", "-q", "-m", "a"]);
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

    fn git(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out.trim_end().to_string()
    }

    /// Write a tag object pointing at `target` (declared `kind`) and point
    /// `refs/tags/<name>` at it; returns the tag's id.
    fn raw_tag(&self, name: &str, target: &str, kind: &str) -> String {
        let body = format!(
            "object {target}\ntype {kind}\ntag {name}\ntagger T <t@example.com> 1700000000 +0000\n\nmsg\n"
        );
        let path = self.root.join(format!("{name}.tag"));
        std::fs::write(&path, body).unwrap();
        let id = self.git(&["hash-object", "-t", "tag", "-w", "--literally", path.to_str().unwrap()]);
        self.git(&["update-ref", &format!("refs/tags/{name}"), &id]);
        id
    }
}

#[test]
fn a_tag_whose_target_is_missing_is_packed_without_a_peeled_line() {
    let f = Fixture::new("missing");
    let head = f.git(&["rev-parse", "HEAD"]);
    let dangling = f.raw_tag("dangling", "1111111111111111111111111111111111111111", "commit");
    // A tag of that tag: the chain breaks one level further down.
    let nested = f.raw_tag("nested", &dangling, "tag");
    f.git(&["tag", "-a", "-m", "ok", "good"]);
    let good = f.git(&["rev-parse", "refs/tags/good"]);

    let (out, err, code) = f.run(&["pack-refs", "--all"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(
        std::fs::read_to_string(f.work.join(".git/packed-refs")).unwrap(),
        format!(
            "# pack-refs with: peeled fully-peeled sorted \n\
             {head} refs/heads/main\n\
             {dangling} refs/tags/dangling\n\
             {good} refs/tags/good\n\
             ^{head}\n\
             {nested} refs/tags/nested\n"
        )
    );
    // Every loose ref went into the file.
    assert!(!f.work.join(".git/refs/tags/dangling").exists());
    assert!(!f.work.join(".git/refs/tags/nested").exists());
    assert_eq!(f.git(&["rev-parse", "refs/tags/dangling"]), dangling);
}
