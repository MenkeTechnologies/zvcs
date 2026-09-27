//! `git rev-list --simplify-by-decoration` is `--simplify-merges` asking a
//! different question.
//!
//! The option sets `simplify_merges`, `topo_order`, `rewrite_parents`,
//! `limited` and `prune` and clears `simplify_history` (revision.c:2445-2452);
//! `rev_compare_tree()` then calls a decorated commit different from every
//! parent and, with no pathspec, an undecorated one the same
//! (revision.c:789-805). So `--parents` names the nearest *kept* ancestor
//! (`rewrite_parents()`), `--children` is the `--parents`/`--children` conflict
//! (`rewrite_parents` is set), and `--sparse` shows every commit. zvcs pruned
//! with a private walk and printed each kept commit's original parent.
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
            .join(format!("zvcs-rev-list-simplify-deco-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Fixture { root, work }
    }

    fn run_at(&self, date: i64, args: &[&str]) -> (String, String, i32) {
        let stamp = format!("{date} +0000");
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
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

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_at(1_700_000_000, args)
    }

    /// `c1`..`c6` on `main` (tags `v1` on `c2`, `v2` on `c4`), a `side` branch
    /// off `c3` with one commit, merged back with `--no-ff`.
    fn linear_with_side(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.run(&["init", "-q", "-b", "main", "."]);
        for i in 1..=6 {
            std::fs::write(f.work.join("f"), format!("{i}\n")).unwrap();
            f.run_at(1_700_000_000 + i * 10, &["add", "f"]);
            f.run_at(1_700_000_000 + i * 10, &["commit", "-q", "-m", &format!("c{i}")]);
        }
        f.run(&["tag", "v1", "HEAD~4"]);
        f.run(&["tag", "v2", "HEAD~2"]);
        f.run(&["checkout", "-q", "-b", "side", "HEAD~3"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"]);
        f.run_at(1_700_000_100, &["commit", "-q", "-m", "s1"]);
        f.run(&["checkout", "-q", "main"]);
        f.run_at(1_700_000_200, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);
        f
    }
}

const MERGE: &str = "4e423ac2f6a6d1befb05a9938ad91990d90d0cfc";
const C6: &str = "fb74a52c5d7932848907765a6c25ffd20d7261a6";
const C5: &str = "5f699ad5bd2c66cea4af75d627cb899bdf3b361f";
const V2: &str = "cef529d492e27c74f02d77ff01ddc8364e2221fb";
const C3: &str = "f8caf9cdb02c961e3eeb3150bf5a342715b60ca8";
const V1: &str = "e7b8d568a9b2c3a3c547ab75cc9af12ee559aa44";
const C1: &str = "89af97685fbb9904866975cdb9f7568dd7b40628";
const S1: &str = "11db4ec5a0d7167ddb5327ec44ea04494949b858";

#[test]
fn parents_name_the_nearest_kept_ancestor() {
    let f = Fixture::linear_with_side("parents");
    assert_eq!(f.run(&["rev-parse", "HEAD"]).0.trim(), MERGE);
    let want = format!("{MERGE} {V2} {S1}\n{S1} {V1}\n{V2} {V1}\n{V1} {C1}\n{C1}\n");
    for extra in [&[][..], &["--topo-order"], &["--full-history"]] {
        let mut args = vec!["rev-list", "--simplify-by-decoration", "--parents"];
        args.extend_from_slice(extra);
        args.push("HEAD");
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), err.as_str(), code), (want.as_str(), "", 0), "{extra:?}");
    }
    // Without `--parents` the kept set was already right.
    let (out, _, code) = f.run(&["rev-list", "--simplify-by-decoration", "HEAD"]);
    assert_eq!((out, code), (format!("{MERGE}\n{S1}\n{V2}\n{V1}\n{C1}\n"), 0));
}

#[test]
fn sparse_shows_every_commit_with_its_own_parents() {
    let f = Fixture::linear_with_side("sparse");
    let (out, _, code) = f.run(&["rev-list", "--simplify-by-decoration", "--parents", "--sparse", "HEAD"]);
    let want = format!(
        "{MERGE} {C6} {S1}\n{S1} {C3}\n{C6} {C5}\n{C5} {V2}\n{V2} {C3}\n{C3} {V1}\n{V1} {C1}\n{C1}\n"
    );
    assert_eq!((out, code), (want, 0));
}

#[test]
fn children_conflicts_with_the_parent_rewrite() {
    let f = Fixture::linear_with_side("children");
    for opt in ["--simplify-by-decoration", "--simplify-merges"] {
        let (out, err, code) = f.run(&["rev-list", opt, "--children", "HEAD"]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: options '--parents' and '--children' cannot be used together\n", 128),
            "{opt}"
        );
    }
}
