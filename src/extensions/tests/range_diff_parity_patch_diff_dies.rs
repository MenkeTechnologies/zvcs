//! `range-diff` refused `-O<orderfile>` and an uncompilable `-G` /
//! `--pickaxe-regex -S` needle as unported instead of dying where git dies.
//!
//! Both are `die()`s from `diffcore_std()` inside `patch_diff()`
//! (range-diff.c:491-500, diff.c:7517-7520): `diffcore_pickaxe()`'s
//! `regcomp_or_die()` (diffcore-pickaxe.c:219-228) first, then
//! `diffcore_order()`'s `prepare_order()`, which is skipped when the pickaxe
//! emptied the queue (diffcore-order.c:118-119). `output()` calls
//! `patch_diff()` right after a matched pair's header and only without
//! `-s` (range-diff.c:567-573), so the header is on stdout, a run with `-s` or
//! with no matched pair exits 0, and a readable order file — which cannot
//! reorder a one-pair queue — changes nothing.
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
    /// `old` and `new` each carry a two-commit series off `base` that differ
    /// in one word, so `--creation-factor=300` pairs them as `!` pairs.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-patch-diff-dies-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.commit("a\nb\nc\n", "base");
        f.run(&["branch", "base"]);
        f.run(&["checkout", "-q", "-b", "old"]);
        f.commit("a\nB x\nc\n", "one");
        f.commit("a\nB x\nc\nd\n", "two");
        f.run(&["checkout", "-q", "-b", "new", "base"]);
        f.commit("a\nB y\nc\n", "one");
        f.commit("a\nB y\nc\nd e\n", "two");
        f
    }

    fn commit(&self, content: &str, subject: &str) {
        std::fs::write(self.work.join("f"), content).unwrap();
        self.run(&["add", "f"]);
        self.run(&["commit", "-q", "-m", subject]);
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

    fn range_diff(&self, extra: &[&str]) -> (String, String, i32) {
        let mut args = vec!["range-diff", "--creation-factor=300", "old...new"];
        args.extend_from_slice(extra);
        self.run(&args)
    }
}

#[test]
fn the_first_matched_body_dies_after_its_header() {
    let f = Fixture::new("dies");
    let (plain, _, _) = f.range_diff(&[]);
    let header = plain.lines().next().unwrap().to_string() + "\n";
    assert!(header.ends_with(" one\n") && header.contains(" ! "), "{header:?}");
    let missing = f.work.join("missing-order");
    let missing = missing.to_str().unwrap();
    let no_order = format!("fatal: failed to read orderfile '{missing}': No such file or directory\n");
    assert_eq!(f.range_diff(&[&format!("-O{missing}")]), (header.clone(), no_order, 128));
    // The pickaxe runs first, so its die wins over the order file's.
    for args in [
        vec!["-G(".to_string(), format!("-O{missing}")],
        vec![format!("-O{missing}"), "-G(".to_string()],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_eq!(
            f.range_diff(&args),
            (header.clone(), "fatal: invalid regex: parentheses not balanced\n".into(), 128),
            "{args:?}"
        );
    }
    assert_eq!(
        f.range_diff(&["--stat", "--pickaxe-regex", "-S["]),
        (header, "fatal: invalid regex: brackets ([ ]) not balanced\n".into(), 128)
    );
}

#[test]
fn no_body_means_no_die() {
    let f = Fixture::new("quiet");
    let (headers, _, _) = f.range_diff(&["-s"]);
    let missing = f.work.join("missing-order");
    let o = format!("-O{}", missing.to_str().unwrap());
    // `-s` never calls `patch_diff()`.
    assert_eq!(f.range_diff(&["-s", &o, "-G("]), (headers.clone(), String::new(), 0));
    // A pickaxe that drops the pair leaves no queue for `diffcore_order()`.
    assert_eq!(f.range_diff(&[&o, "-Szzz"]), (headers, String::new(), 0));
    // A readable order file changes nothing, attached or separate.
    let (plain, _, _) = f.range_diff(&[]);
    std::fs::write(f.work.join("order"), "f\n*\n").unwrap();
    assert_eq!(f.range_diff(&["-Oorder"]), (plain.clone(), String::new(), 0));
    assert_eq!(f.range_diff(&["-O", "order"]), (plain, String::new(), 0));
}
