//! `fast-export --fake-missing-tagger` on a tag object with no `tagger` line.
//!
//! `handle_tag()` substitutes
//! `tagger Unspecified Tagger <unspecified-tagger> 0 +0000`
//! (builtin/fast-export.c:920-923); without the option the stanza simply has
//! no tagger line. zvcs invented `tagger <unknown> <unknown> 0 +0000`.
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
    /// One commit and `refs/tags/nt`, a tag object without a tagger.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fast-export-fake-tagger-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        let head = f.run(&["rev-parse", "HEAD"]).0;
        let body = format!("object {}\ntype commit\ntag nt\n\nmsg\n", head.trim_end());
        let path = f.root.join("tag-object");
        std::fs::write(&path, body).unwrap();
        let id = f.run(&["hash-object", "-t", "tag", "-w", "--literally", path.to_str().unwrap()]).0;
        f.run(&["update-ref", "refs/tags/nt", id.trim_end()]);
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
}

const COMMIT: &str = "blob\nmark :1\ndata 2\na\n\n\
reset refs/tags/nt\n\
commit refs/tags/nt\nmark :2\n\
author A U Thor <author@example.com> 1700000000 +0000\n\
committer C O Mitter <committer@example.com> 1700000000 +0000\n\
data 4\none\nM 100644 :1 a\n\n";

#[test]
fn the_invented_tagger_is_unspecified_tagger() {
    let f = Fixture::new("fake");
    let (out, err, code) = f.run(&["fast-export", "--fake-missing-tagger", "nt"]);
    let want = format!(
        "{COMMIT}tag nt\nfrom :2\n\
         tagger Unspecified Tagger <unspecified-tagger> 0 +0000\n\
         data 4\nmsg\n\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0));
}

#[test]
fn without_the_option_the_tagger_line_is_absent() {
    let f = Fixture::new("plain");
    let (out, err, code) = f.run(&["fast-export", "nt"]);
    let want = format!("{COMMIT}tag nt\nfrom :2\ndata 4\nmsg\n\n");
    assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0));
}
