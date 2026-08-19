//! `git last-modified` orders its commit queue by the commit-graph's generation number.
//!
//! `compare_commits_by_gen_then_commit_date()` (commit.c:909) sorts by
//! `commit_graph_generation()` first and treats the commit date only as a tie-break. With
//! no graph every generation is `GENERATION_NUMBER_INFINITY` and the date decides; with a
//! graph carrying `GDA2` the number is the *corrected commit date*
//! (`fill_commit_graph_info()`, commit-graph.c:902-915) — max of the commit's own date and
//! one past its parents' — so a commit whose ancestor carries a far-future date sorts
//! ahead of a sibling with a newer raw date.
//!
//! The fixture below is built so the two rules disagree: `C`'s parent `B` is dated in
//! 2033, which lifts `C`'s corrected date above `D`'s despite `D` being the newer commit
//! by its own timestamp. Emission order is therefore `d, c, b, base` without a graph and
//! `c, b, d, base` with one — both measured from stock git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn run(&self, args: &[&str], date: Option<&str>) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.home.join("gitsystem"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null());
        if let Some(date) = date {
            cmd.env("GIT_AUTHOR_DATE", date).env("GIT_COMMITTER_DATE", date);
        }
        cmd.output().expect("run binary")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args, None);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Commit everything staged with a fixed author/committer timestamp, so the graph's
    /// corrected dates — and therefore the queue order — are reproducible.
    fn commit_at(&self, epoch: &str, message: &str) {
        let out = self.run(&["commit", "-q", "-m", message], Some(&format!("{epoch} +0000")));
        assert!(
            out.status.success(),
            "commit {message} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn add(&self, name: &str) {
        std::fs::write(self.repo.join(name), format!("{name}\n")).unwrap();
        self.ok(&["add", name]);
    }
}

/// ```text
///       M (merge)
///      / \
///     C   D          C: 2001-09-09, D: 2001-09-09 (100s later)
///     |   |
///     B   |          B: 2033-05-18 — far in the future
///     \  /
///      A             A: 2001-09-09
/// ```
fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-lm-graph-order-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let fx = Fixture { repo, home };
    fx.ok(&["init", "-q", "-b", "main", "."]);
    fx.ok(&["config", "user.email", "t@e.co"]);
    fx.ok(&["config", "user.name", "t"]);

    fx.add("base.txt");
    fx.commit_at("1000000000", "A");
    let a = fx.ok(&["rev-parse", "HEAD"]).trim().to_string();

    fx.add("b.txt");
    fx.commit_at("2000000000", "B");
    fx.add("c.txt");
    fx.commit_at("1000000100", "C");

    fx.ok(&["checkout", "-q", "-b", "sided", &a]);
    fx.add("d.txt");
    fx.commit_at("1000000200", "D");

    fx.ok(&["checkout", "-q", "main"]);
    let out = fx.run(
        &["merge", "--no-ff", "-m", "M", "sided"],
        Some("1000000300 +0000"),
    );
    assert!(
        out.status.success(),
        "merge failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    fx
}

/// The path column of `git last-modified`, which is `<oid>\t<path>` per line.
fn paths(output: &str) -> Vec<&str> {
    output
        .lines()
        .map(|line| line.split_once('\t').expect("oid TAB path").1)
        .collect()
}

fn has_commit_graph(repo: &Path) -> bool {
    repo.join(".git/objects/info/commit-graph").exists()
}

#[test]
fn the_queue_falls_back_to_commit_date_without_a_graph() {
    let fx = fixture("nograph");
    assert!(!has_commit_graph(&fx.repo), "no graph should exist yet");
    assert_eq!(paths(&fx.ok(&["last-modified"])), ["d.txt", "c.txt", "b.txt", "base.txt"]);
}

#[test]
fn a_commit_graph_reorders_the_queue_by_corrected_commit_date() {
    let fx = fixture("graph");
    let before = fx.ok(&["last-modified"]);
    fx.ok(&["commit-graph", "write", "--reachable"]);
    assert!(has_commit_graph(&fx.repo), "commit-graph write produced no file");

    let after = fx.ok(&["last-modified"]);
    assert_eq!(paths(&after), ["c.txt", "b.txt", "d.txt", "base.txt"]);
    assert_ne!(
        paths(&before),
        paths(&after),
        "the fixture must actually discriminate the two orderings"
    );
    // Only the order may move: the same paths resolve to the same commits either way.
    let mut sorted_before: Vec<&str> = before.lines().collect();
    let mut sorted_after: Vec<&str> = after.lines().collect();
    sorted_before.sort_unstable();
    sorted_after.sort_unstable();
    assert_eq!(sorted_before, sorted_after);
}
