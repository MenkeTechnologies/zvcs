//! `cherry-pick`/`revert` with revision *ranges* rather than named commits.
//!
//! `builtin/revert.c` feeds the whole operand list to `setup_revisions()` and
//! lets the revision walker decide what is replayed, so `<a>..<b>`, `<a>...<b>`,
//! `^<commit>` and any mix of those with plain commits all work — and
//! `prepare_revs()` flips `revs->reverse` for a *pick* that walks, so a picked
//! range is replayed oldest first while a reverted one is backed out newest
//! first. These tests pin that contract: which commits are selected, in which
//! order, and what a selection naming nothing at all does.
//!
//! The fixture is built with the binary under test so the tests need nothing
//! installed, and every case is additionally diffed against a stock git when the
//! machine has one — the differential is what catches an ordering rule that is
//! self-consistent but not git's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A STOCK git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// Resolved EXPLICITLY rather than through `PATH`: on a machine where zvcs
/// shadows git — the machine this is developed on — a `PATH` lookup silently
/// makes the oracle the thing under test.
///
/// The *newest* installed git wins, the policy `src/parity/src/stock.rs` uses:
/// a machine usually has an OS-vendored git beside a current one, the port
/// tracks the current one, and the two disagree about real behaviour. They do so
/// here in particular — 2.50.1 and 2.55.0 interleave two unrelated branches in a
/// walked selection differently.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .filter(|p| Path::new(p).exists())
        .filter_map(|p| Some((version_of(p)?, p.to_owned())))
        .max()
        .map(|(_, p)| p)
}

/// `git version X.Y.Z` as a comparable tuple, or `None` when it will not answer.
fn version_of(bin: &str) -> Option<(u32, u32, u32)> {
    let out = Command::new(bin).arg("--version").env_clear().output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.trim().strip_prefix("git version ")?;
    let mut parts = rest.split(['.', ' ', '-']).filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

fn run(bin: &str, repo: &Path, home: &Path, date: &str, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap()
}

/// A fixture repository plus the binary that built it.
struct Repo {
    bin: String,
    dir: PathBuf,
    home: PathBuf,
    /// Commit timestamps advance one day per commit, which is what decides how
    /// two commits with no ancestry between them interleave in the walk.
    day: usize,
}

impl Repo {
    fn git(&self, args: &[&str]) -> Output {
        let date = date_of(self.day);
        run(&self.bin, &self.dir, &self.home, &date, args)
    }

    /// One commit adding `<name>.txt`, on the current branch.
    fn commit(&mut self, name: &str) {
        self.day += 1;
        std::fs::write(self.dir.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
        let file = format!("{name}.txt");
        assert!(self.git(&["add", &file]).status.success(), "add {name}");
        let out = self.git(&["commit", "-q", "-m", name]);
        assert!(out.status.success(), "commit {name}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn checkout(&self, args: &[&str]) {
        let mut argv = vec!["checkout", "-q"];
        argv.extend_from_slice(args);
        let out = self.git(&argv);
        assert!(out.status.success(), "checkout {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Subjects of the first `n` commits of `HEAD`, newest first.
    fn subjects(&self, n: usize) -> Vec<String> {
        let out = self.git(&["log", "--format=%s", &format!("-{n}")]);
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }

    fn has(&self, path: &str) -> bool {
        self.dir.join(".git").join(path).exists()
    }
}

fn date_of(day: usize) -> String {
    format!("2023-01-{:02} 00:00:00 +0000", day + 1)
}

/// ```text
///        feat1 - feat2 - feat3   (feature)
///       /
/// base -- side1                  (side)
///       \
///        main1 - main2           (main, HEAD)
/// ```
///
/// Three shapes in one history: a range with several commits, a second branch
/// that no range excludes (so a mixed operand list has something to interleave),
/// and commits on `main` itself for `^` exclusions and reverts.
fn fixture(bin: &str, tag: &str) -> Repo {
    let root = std::env::temp_dir().join(format!("zvcs-cpranges-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let dir = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let mut repo = Repo { bin: bin.to_owned(), dir, home, day: 0 };
    assert!(repo.git(&["init", "-q", "-b", "main", "."]).status.success(), "init");
    repo.git(&["config", "user.name", "A U Thor"]);
    repo.git(&["config", "user.email", "author@example.com"]);
    repo.commit("base");
    repo.checkout(&["-b", "feature"]);
    repo.commit("feat1");
    repo.commit("feat2");
    repo.commit("feat3");
    repo.checkout(&["main"]);
    repo.checkout(&["-b", "side"]);
    repo.commit("side1");
    repo.checkout(&["main"]);
    repo.commit("main1");
    repo.commit("main2");
    repo
}

/// Run `args` under zvcs, and under stock git in an identical fixture when one
/// exists, asserting the two agree. Returns zvcs's output and the history it
/// left behind.
fn both(tag: &str, args: &[&str]) -> (Output, Vec<String>) {
    let zvcs = fixture(BIN, &format!("{tag}-zvcs"));
    let zout = zvcs.git(args);
    let zsubjects = zvcs.subjects(9);
    let zstate = (zvcs.has("sequencer"), zvcs.has("CHERRY_PICK_HEAD"), zvcs.has("REVERT_HEAD"));

    let stock = stock_git().map(|bin| {
        let stock = fixture(&bin, &format!("{tag}-stock"));
        let out = stock.git(args);
        let subjects = stock.subjects(9);
        let state = (stock.has("sequencer"), stock.has("CHERRY_PICK_HEAD"), stock.has("REVERT_HEAD"));
        (out, subjects, state, stock)
    });

    if let Some((sout, ssubjects, sstate, srepo)) = stock {
        assert_eq!(
            sout.status.code(),
            zout.status.code(),
            "exit status must match stock for {args:?}\nzvcs stderr: {}\nstock stderr: {}",
            String::from_utf8_lossy(&zout.stderr),
            String::from_utf8_lossy(&sout.stderr),
        );
        assert_eq!(
            String::from_utf8_lossy(&sout.stderr),
            String::from_utf8_lossy(&zout.stderr),
            "stderr must match stock for {args:?}"
        );
        assert_eq!(ssubjects, zsubjects, "replayed history must match stock for {args:?}");
        assert_eq!(
            sstate, zstate,
            "sequencer state must match stock for {args:?} (sequencer, CHERRY_PICK_HEAD, REVERT_HEAD)"
        );
        let _ = std::fs::remove_dir_all(srepo.dir.parent().unwrap());
    }

    let _ = std::fs::remove_dir_all(zvcs.dir.parent().unwrap());
    (zout, zsubjects)
}

/// `<a>..<b>` replays every commit `<b>` reaches that `<a>` does not, **oldest
/// first** — `prepare_revs()`'s `revs->reverse ^= 1` for a pick. The reverse is
/// not cosmetic: replayed newest-first, `feat2` would be applied before the
/// `feat1` it builds on.
#[test]
fn picked_range_replays_oldest_first() {
    let (out, subjects) = both("range", &["cherry-pick", "main..feature"]);
    assert!(out.status.success(), "cherry-pick main..feature: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        subjects,
        ["feat3", "feat2", "feat1", "main2", "main1", "base"],
        "the range's three commits must land on main in fixture order"
    );
}

/// A range beside a plain commit is one walk over the union, not two operations:
/// here `HEAD~1` is `main1`, which the range's `^main` already excludes, so the
/// extra operand contributes nothing. This is the shape the parity harness
/// reported as unsupported.
#[test]
fn range_plus_excluded_commit_picks_only_the_range() {
    let (out, subjects) = both("range-plus-hidden", &["cherry-pick", "main..feature", "HEAD~1"]);
    assert!(out.status.success(), "cherry-pick main..feature HEAD~1: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        subjects,
        ["feat3", "feat2", "feat1", "main2", "main1", "base"],
        "an operand the range already excludes must not be replayed again"
    );
}

/// A range beside a commit the range does *not* exclude replays both: one walk
/// over the union, not one operation per operand.
///
/// Only the *set* and the range's internal order are pinned. Where `side1` — a
/// commit with no ancestry relation to any of `feat1..feat3` — lands among them
/// is not stable across git versions (2.50.1 replays it third, 2.55.0 last), so
/// asserting it would pin an accident rather than the contract. The differential
/// in [`both`] compares against the newest installed git, which is the version
/// the port tracks.
#[test]
fn range_plus_extra_commit_replays_both() {
    let repo = fixture(BIN, "range-plus-side");
    let out = repo.git(&["cherry-pick", "main..feature", "side"]);
    assert!(out.status.success(), "cherry-pick main..feature side: {}", String::from_utf8_lossy(&out.stderr));

    let subjects = repo.subjects(7);
    let replayed: Vec<&String> = subjects.iter().take(4).collect();
    let mut sorted: Vec<&str> = replayed.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    assert_eq!(sorted, ["feat1", "feat2", "feat3", "side1"], "both operands must be replayed, once each");
    assert_eq!(&subjects[4..], ["main2", "main1", "base"], "nothing below the replayed commits may move");

    // Newest first in the log is oldest first in the replay, so the range's own
    // commits must appear in descending order.
    let position = |name: &str| replayed.iter().position(|s| s.as_str() == name).expect(name);
    assert!(
        position("feat3") < position("feat2") && position("feat2") < position("feat1"),
        "the range must still be replayed oldest first: {subjects:?}"
    );

    let _ = std::fs::remove_dir_all(repo.dir.parent().unwrap());
}

/// `^<commit>` excludes without naming a range, and turns the walk on by itself:
/// `feature ^feature~2` is the last two commits of `feature`, oldest first.
#[test]
fn caret_exclusion_selects_the_tail_of_a_branch() {
    let (out, subjects) = both("exclusion", &["cherry-pick", "feature", "^feature~2"]);
    assert!(out.status.success(), "cherry-pick feature ^feature~2: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        subjects,
        ["feat3", "feat2", "main2", "main1", "base"],
        "only the two commits past feature~2 may be replayed"
    );
}

/// A walked selection that names no commit is `walk_revs_populate_todo()`'s
/// `empty commit set passed`, and it is refused *before* any sequencer state is
/// written — an empty range must not leave a `.git/sequencer` behind.
#[test]
fn empty_range_is_refused_before_any_state_is_written() {
    let repo = fixture(BIN, "empty-range");
    let out = repo.git(&["cherry-pick", "main..main"]);

    assert_eq!(out.status.code(), Some(128), "an empty commit set is a 128, not a usage error");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: empty commit set passed\nfatal: cherry-pick failed\n"
    );
    assert_eq!(repo.subjects(3), ["main2", "main1", "base"], "nothing may be committed");
    assert!(!repo.has("sequencer"), "no sequencer directory may survive an empty selection");
    assert!(!repo.has("CHERRY_PICK_HEAD"), "no CHERRY_PICK_HEAD may survive an empty selection");

    let _ = std::fs::remove_dir_all(repo.dir.parent().unwrap());
}

/// A *reverted* range is the same walk without the reversal, so it is backed out
/// newest first — `main2` before `main1`, the only order in which the two
/// reverts apply. Same operand grammar, opposite order: the flag that separates
/// them is `opts->action == REPLAY_PICK`.
#[test]
fn reverted_range_backs_out_newest_first() {
    let (out, subjects) = both("revert-range", &["revert", "--no-edit", "main~2..main"]);
    assert!(out.status.success(), "revert main~2..main: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        subjects,
        ["Revert \"main1\"", "Revert \"main2\"", "main2", "main1", "base"],
        "a reverted range must undo the newest commit first"
    );
}

/// The `<a>...<b>` symmetric difference reaches the sequencer too: its merge
/// bases are excluded, which is what turns the walk on, and both tips are
/// included. `main...side` is `main1`, `main2` and `side1` — all but `base`.
#[test]
fn symmetric_difference_excludes_the_merge_base() {
    let repo = fixture(BIN, "symmetric");
    // Picked onto `main`, `main1`/`main2` are already applied and the pick stops
    // on the first of them; what this pins is the *selection*, read from the todo
    // list the stopped sequence leaves behind.
    let out = repo.git(&["cherry-pick", "main...side"]);
    assert_eq!(out.status.code(), Some(1), "re-picking an applied commit stops the sequence");

    let todo = std::fs::read_to_string(repo.dir.join(".git/sequencer/todo")).expect("todo list");
    let picked: Vec<&str> = todo
        .lines()
        .filter_map(|l| l.rsplit_once(' ').map(|(_, subject)| subject))
        .collect();
    assert_eq!(
        picked,
        ["main1", "main2"],
        "the merge base is excluded and `side1`, already replayed, is off the list"
    );
    assert_eq!(repo.subjects(4), ["side1", "main2", "main1", "base"], "side1 lands before the stop");

    let _ = std::fs::remove_dir_all(repo.dir.parent().unwrap());
}
