//! `revert -n` / `cherry-pick -n` over an unmerged index.
//!
//! Under `--no-commit` `do_pick_commit` takes *ours* from
//! `write_index_as_tree()` (sequencer.c:2293). Over an unmerged index that
//! fails in `cache_tree_update()` → `verify_cache()` (cache-tree.c:165-186),
//! which prints one `<path>: unmerged (<oid>)` line per conflicted stage,
//! stopping after ten with a bare `...`. `do_pick_commit` then returns
//! `error(_("your index file is unmerged."))` and `run_sequencer` dies with
//! `_("%s failed")` (builtin/revert.c:276). zvcs's revert printed only the
//! `error:` line, and cherry-pick listed every stage without the cap.
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
    /// `main` and `side` both rewrite `f1`..`f<n>` from a common base, and
    /// `main` has merged `side`, leaving every file conflicted.
    fn conflicted(tag: &str, n: usize) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-revert-nc-unmerged-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write_all(n, "base\n");
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        f.write_all(n, "side\n");
        f.run(&["commit", "-q", "-am", "side"]);
        f.run(&["checkout", "-q", "main"]);
        f.write_all(n, "main\n");
        f.run(&["commit", "-q", "-am", "main"]);
        assert_eq!(f.run(&["merge", "side"]).2, 1);
        f
    }

    fn write_all(&self, n: usize, body: &str) {
        for i in 1..=n {
            std::fs::write(self.work.join(format!("f{i}")), body).unwrap();
        }
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
            .env("GIT_MERGE_AUTOEDIT", "no")
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

const BASE: &str = "df967b96a579e45a18b8251732d16804b2e56a55";
const MAIN: &str = "ba2906d0666cf726c7eaadd2cd3db615dedfdf3a";
const SIDE: &str = "2299c37978265a95cbe835a4b0f0bbf15aad5549";

#[test]
fn revert_no_commit_names_the_stages_and_dies() {
    let f = Fixture::conflicted("one", 1);
    let (out, err, code) = f.run(&["revert", "-n", "HEAD"]);
    let want = format!(
        "f1: unmerged ({BASE})\nf1: unmerged ({MAIN})\nf1: unmerged ({SIDE})\n\
         error: your index file is unmerged.\nfatal: revert failed\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
    // The conflict the merge left is untouched.
    assert_eq!(f.run(&["status", "--short"]).0, "UU f1\n");
}

#[test]
fn the_stage_list_stops_after_ten() {
    let f = Fixture::conflicted("many", 12);
    // Index order: f1, f10, f11, f12, f2, ... — three stages each.
    let mut want = String::new();
    for path in ["f1", "f10", "f11"] {
        for id in [BASE, MAIN, SIDE] {
            want.push_str(&format!("{path}: unmerged ({id})\n"));
        }
    }
    want.push_str(&format!("f12: unmerged ({BASE})\n...\nerror: your index file is unmerged.\n"));
    for (verb, target) in [("revert", "HEAD"), ("cherry-pick", "side")] {
        let (out, err, code) = f.run(&[verb, "-n", target]);
        let want = format!("{want}fatal: {verb} failed\n");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{verb}");
    }
}
