//! `<GIT_DIR>/info/grafts` reaches every consumer of a parsed parent list.
//!
//! `parse_commit_buffer()` replaces a grafted commit's `parent` lines with the
//! graft's (commit.c:554-590), and `prepare_commit_graft()` fills the table from
//! `info/grafts` (or `$GIT_GRAFT_FILE`) and `shallow` (commit.c:316-330). zvcs
//! honoured that table in `log` and `rev-list`, but four consumers read
//! `shallow` alone:
//!
//! * `fsck` followed a grafted commit's true parents, so `--root` missed a
//!   commit grafted to no parents and `--no-reflogs` called the wrong commits
//!   dangling;
//! * `log --decorate` gave only shallow commits the `grafted` decoration that
//!   `for_each_commit_graft()` hands every entry (log-tree.c:211-247);
//! * `name-rev` named commits through their true parents;
//! * `prune`'s `mark_reachable_objects()` kept what only a true parent reached.
//!
//! And `commit-graph write` skipped on a non-empty graft file without the read
//! `commit_graph_compatible()` makes (commit-graph.c:223-242), so it neither
//! printed the deprecation advice nor wrote a graph when the file held only
//! comments.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// One repository per binary, built with identical pinned dates so the two
/// share every object id.
struct Pair {
    root: PathBuf,
    bins: [&'static str; 2],
}

impl Drop for Pair {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

/// `c1 - c2 - c3 - c4 - c5` on `main`, and `side` forked from `c2`.
const HISTORY: [&str; 5] = ["c1", "c2", "c3", "c4", "c5"];

impl Pair {
    fn new(label: &str) -> Option<Pair> {
        let stock = stock_git::stock_git()?;
        let root = std::env::temp_dir().join(format!("zvcs-fsck-info-grafts-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let pair = Pair { root, bins: [stock, ZVCS] };
        for (i, bin) in pair.bins.iter().enumerate() {
            let dir = pair.dir(i);
            run(bin, &pair.root, &["init", "-q", "-b", "main", dir.to_str().unwrap()], &[]);
            for (n, name) in HISTORY.iter().enumerate() {
                std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
                run(bin, &dir, &["add", name], &[]);
                let date = format!("{} +0000", 1_700_000_001 + n as u64);
                run(bin, &dir, &["commit", "-q", "-m", name], &[("GIT_COMMITTER_DATE", &date)]);
            }
            run(bin, &dir, &["checkout", "-q", "-b", "side", "main~3"], &[]);
            std::fs::write(dir.join("s"), "s\n").unwrap();
            run(bin, &dir, &["add", "s"], &[]);
            run(bin, &dir, &["commit", "-q", "-m", "side"], &[("GIT_COMMITTER_DATE", "1700000010 +0000")]);
            run(bin, &dir, &["checkout", "-q", "main"], &[]);
        }
        Some(pair)
    }

    fn dir(&self, i: usize) -> PathBuf {
        self.root.join(["stock", "zvcs"][i])
    }

    fn rev(&self, spec: &str) -> String {
        run(self.bins[0], &self.dir(0), &["rev-parse", spec], &[]).stdout.trim_end().to_owned()
    }

    /// Write `body` to `rel` inside both repositories.
    fn write(&self, rel: &str, body: &str) {
        for i in 0..2 {
            std::fs::write(self.dir(i).join(rel), body).unwrap();
        }
    }

    /// `c5` grafted onto `c3` and `side` (a replaced and an added parent), `c3`
    /// grafted to none (a removed one), `side` onto `c1` and `c2`.
    fn graft_all(&self) {
        let [c1, c2, c3, c5, side] = ["main~4", "main~3", "main~2", "main", "side"].map(|r| self.rev(r));
        self.write(".git/info/grafts", &format!("{c5} {c3} {side}\n{c3}\n{side} {c1} {c2}\n"));
    }

    #[track_caller]
    fn same(&self, args: &[&str]) -> Outcome {
        self.same_env(args, &[])
    }

    #[track_caller]
    fn same_env(&self, args: &[&str], env: &[(&str, &str)]) -> Outcome {
        let [stock, zvcs] = [0, 1].map(|i| {
            let mut out = run(self.bins[i], &self.dir(i), args, env);
            let dir = self.dir(i).to_string_lossy().into_owned();
            out.stdout = out.stdout.replace(&dir, "<repo>");
            out.stderr = out.stderr.replace(&dir, "<repo>");
            out
        });
        assert_eq!(zvcs, stock, "git {args:?}: left is zvcs, right is stock");
        stock
    }
}

fn run(bin: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Outcome {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
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
        .env("TZ", "UTC");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    Outcome {
        code: out.status.code().expect("no signal"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn fsck_walks_the_graft_parents() {
    let Some(p) = Pair::new("fsck") else { return };
    let c4 = p.rev("main~1");
    p.graft_all();
    for args in [
        &["fsck"][..],
        &["fsck", "--root"],
        &["fsck", "--no-reflogs"],
        &["fsck", "--no-reflogs", "--unreachable"],
        &["fsck", "--no-reflogs", "--connectivity-only"],
        &["fsck", "--no-reflogs", "main"],
        &["fsck", "--no-reflogs", "--root", "--name-objects"],
    ] {
        p.same(args);
    }
    // `c4` is reachable only through `c5`'s true parent, which the graft drops.
    let out = p.same(&["fsck", "--no-reflogs", "main"]);
    assert!(out.stdout.contains(&format!("dangling commit {c4}")), "{out:?}");
}

#[test]
fn a_graft_parent_that_is_missing_is_a_broken_link() {
    let Some(p) = Pair::new("fsck-missing") else { return };
    let c5 = p.rev("main");
    p.write(".git/info/grafts", &format!("{c5} {}\n", "1".repeat(40)));
    p.same(&["fsck", "--no-reflogs"]);
    p.same(&["fsck", "--no-reflogs", "--connectivity-only"]);
}

#[test]
fn decorate_marks_every_graft() {
    let Some(p) = Pair::new("decorate") else { return };
    p.graft_all();
    for args in [
        &["log", "--oneline", "--decorate", "--all"][..],
        &["log", "--format=%h%d", "--all"],
        &["log", "--oneline", "--decorate=full", "--graph", "main"],
        &["log", "--oneline", "--no-walk", "--decorate", "main", "side"],
    ] {
        p.same(args);
    }
}

#[test]
fn name_rev_follows_the_graft_parents() {
    let Some(p) = Pair::new("name-rev") else { return };
    p.graft_all();
    p.same(&["name-rev", "--all"]);
    p.same(&["name-rev", "main~2", "side"]);
}

#[test]
fn commit_graph_write_reads_the_table() {
    let Some(p) = Pair::new("commit-graph") else { return };
    p.graft_all();
    p.same(&["commit-graph", "write", "--reachable"]);
    for i in 0..2 {
        assert!(!p.dir(i).join(".git/objects/info/commit-graph").exists());
    }

    // A graft file of comments and blank lines leaves `grafts_nr` at 0, so the
    // graph is written — after the advice its read printed.
    p.write(".git/info/grafts", "# nothing\n\n");
    p.same(&["commit-graph", "write", "--reachable"]);
    for i in 0..2 {
        assert!(p.dir(i).join(".git/objects/info/commit-graph").exists());
    }
    p.same(&["commit-graph", "verify"]);
}

#[test]
fn commit_graph_write_reports_bad_lines_and_honours_the_advice_switch() {
    let Some(p) = Pair::new("commit-graph-bad") else { return };
    let [c3, c5] = ["main~2", "main"].map(|r| p.rev(r));
    p.write(".git/info/grafts", &format!("{c5}  {c3}\nbogus\n{c5}\n{c5} {c3}\n"));
    let out = p.same(&["commit-graph", "write", "--reachable"]);
    assert!(out.stderr.contains("error: bad graft data: bogus\n"), "{out:?}");
    p.same(&["-c", "advice.graftFileDeprecated=false", "commit-graph", "write", "--reachable"]);
}

#[test]
fn the_graft_file_variable_moves_the_table() {
    let Some(p) = Pair::new("graft-file") else { return };
    let c3 = p.rev("main~2");
    let file = p.root.join("grafts");
    std::fs::write(&file, format!("{c3}\n")).unwrap();
    let env = [("GIT_GRAFT_FILE", file.to_str().unwrap())];
    for args in [
        &["fsck", "--root"][..],
        &["log", "--oneline", "--decorate", "main"],
        &["name-rev", "main~2"],
        &["commit-graph", "write", "--reachable"],
    ] {
        p.same_env(args, &env);
    }
}

#[test]
fn prune_drops_what_only_a_true_parent_reached() {
    let Some(p) = Pair::new("prune") else { return };
    let c4 = p.rev("main~1");
    p.graft_all();
    // Without its reflog entries `c4` is reachable only through `c5`'s true
    // parent; `mark_reachable_objects()` follows the graft and so prunes it.
    p.same(&["reflog", "expire", "--expire=all", "--all"]);
    p.same(&["prune", "-n", "-v", "--expire=now"]);
    p.same(&["prune", "--expire=now"]);
    p.same(&["cat-file", "-t", &c4]);
}
