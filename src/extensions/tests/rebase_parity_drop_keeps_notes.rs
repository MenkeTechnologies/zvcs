//! A pick that rebase drops because its changes are already upstream is not
//! recorded as rewritten, so `notes.rewriteRef` does not copy its notes.
//!
//! `do_pick_commit()` reports `PICK_RESULT_DROPPED` (sequencer.c:2591-2592,
//! v2.56.0) and `do_pick_commit()`'s caller records only `PICK_RESULT_OK`
//! (:5089-5096). 2.55 recorded the dropped commit too, mapping it onto the
//! `HEAD` it left behind, so `copy_notes_for_rewrite()` copied its note to the
//! upstream tip. zvcs did the same.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rebase-drop-notes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r")).unwrap();
        Fixture { root: std::fs::canonicalize(&root).unwrap() }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("r")
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.repo().join(name), body).unwrap();
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn a_dropped_pick_keeps_its_note_to_itself() {
    let f = Fixture::new("upstream");
    let r = f.repo();
    f.git(&r, &["init", "-q", "-b", "main"]);
    f.write("a", "a\n");
    f.write("b", "b\n");
    f.git(&r, &["add", "."]);
    f.git(&r, &["commit", "-q", "-m", "base"]);
    f.git(&r, &["checkout", "-q", "-b", "topic"]);
    f.write("a", "x\n");
    f.git(&r, &["commit", "-q", "-am", "A"]);
    f.git(&r, &["notes", "add", "-m", "note A", "HEAD"]);
    f.write("b", "y\n");
    f.git(&r, &["commit", "-q", "-am", "B"]);
    f.git(&r, &["notes", "add", "-m", "note B", "HEAD"]);
    // Upstream makes A's change along with another, so the patch ids differ and
    // A is picked, merges to an unchanged tree, and is dropped.
    f.git(&r, &["checkout", "-q", "main"]);
    f.write("a", "x\n");
    f.write("c", "z\n");
    f.git(&r, &["add", "."]);
    f.git(&r, &["commit", "-q", "-m", "X+Z"]);
    f.git(&r, &["checkout", "-q", "topic"]);

    f.git(&r, &["-c", "notes.rewriteRef=refs/notes/commits", "rebase", "main"]);

    assert_eq!(f.git(&r, &["log", "--format=%s %N", "main..HEAD"]), "B note B\n\n");
    // A's note stays on A alone; B's is copied to the rewritten B. Nothing lands
    // on `main`'s tip.
    assert_eq!(
        f.git(&r, &["notes", "list"]),
        "8aa87e5d9ebcbf85b726718970432330cbd2e1df a01eb4fb9dadcf91474607fe95b62d14b6faf4f7\n\
         27116141d49e089f2da1a1a102329116377349a2 e709a93786bb797823a25b80309bf1896ee9c168\n\
         27116141d49e089f2da1a1a102329116377349a2 fbe81bab212065c53430df0676b0e9a746e2b97a\n"
    );
}
