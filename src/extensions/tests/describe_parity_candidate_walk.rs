//! `git describe`'s candidate walk: where it stops, what it counts, and who it
//! hands `--contains` to.
//!
//! `describe_commit()` (builtin/describe.c:365-536) walks a date-ordered priority
//! queue, collects at most `max_candidates` names, and hands the best one to
//! `finish_depth_computation()`. Four separate rules decide when it stops and what
//! the printed `-<n>-` is, and each is a place the port had drifted:
//!
//!   * `if (match_cnt == max_candidates || match_cnt == hashmap_get_size(&names))`
//!     (`:418-422`) sits at the *top* of the loop body — so the commit reported as
//!     "gave up search at" is the first one popped after the table filled, named or
//!     not, and a walk that has already found every known name stops instead of
//!     walking (and counting) the rest of the history.
//!   * `names` holds every ref the `--match`/`--exclude` filters admit, lightweight
//!     tags included: "we still remember lightweight ones, only to give hints in an
//!     error message" (`:219-222`). Its size is therefore *not* the number of names
//!     a default, annotated-only describe will turn into candidates.
//!   * `if (annotated_cnt && lazy_queue_empty(&queue))` (`:446`) arms the early stop
//!     on *annotated* candidates only, so under `--tags` a lightweight tag does not
//!     end the search.
//!   * `lazy_queue_put(&queue, gave_up_on)` (`:500`) is `prio_queue_replace`, which
//!     re-queues the commit under its own date and a fresh insertion counter
//!     (prio-queue.c:105-115) — so among commits sharing a timestamp it comes out
//!     last, not first.
//!
//! And `--contains` is not a walk at all: `cmd_describe()` builds a name-rev
//! argument vector and calls `cmd_name_rev()` with it (`:703-752`).
//!
//! Every expectation below was read off a differential run against stock git 2.55.0
//! in a byte-identical fixture. The tests shell out to nothing but the binary under
//! test, so they are as portable as it is.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The one timestamp every fixture commit shares unless [`Fixture::ticking`] is on.
const BASE_DATE: &str = "2005-04-07T15:16:17+0000";

fn base(cmd: &mut Command, repo: &Path, home: &Path) {
    cmd.current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("LC_ALL", "C");
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    /// Seconds past [`BASE_DATE`] for the next commit, when this fixture wants a
    /// strictly increasing history. `None` flattens every commit onto one second,
    /// which puts them all in one priority-queue tie group where the order is
    /// decided by git's insertion counter (prio-queue.c:4-11) rather than by date.
    clock: Option<u32>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-descwalk-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let f = Fixture { root, repo, home, clock: None };
        f.ok(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn ticking(mut self) -> Self {
        self.clock = Some(0);
        self
    }

    /// Run at [`BASE_DATE`]. Everything that is not a commit uses this, so tag
    /// objects all carry one tagger date and `replace_name()`'s date tie-break
    /// (builtin/describe.c:125-126) never fires.
    fn git(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        base(&mut cmd, &self.repo, &self.home);
        cmd.args(args).env("GIT_AUTHOR_DATE", BASE_DATE).env("GIT_COMMITTER_DATE", BASE_DATE);
        cmd.output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.git(args);
        assert!(out.status.success(), "setup `git {args:?}` failed: {}", stderr(&out));
        out
    }

    /// Run at the fixture's current clock, then advance it.
    fn tick(&mut self, args: &[&str]) -> Output {
        let date = match self.clock {
            Some(s) => format!("2005-04-07T15:16:{:02}+0000", 17 + s),
            None => BASE_DATE.to_owned(),
        };
        let mut cmd = Command::new(BIN);
        base(&mut cmd, &self.repo, &self.home);
        cmd.args(args).env("GIT_AUTHOR_DATE", &date).env("GIT_COMMITTER_DATE", &date);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {}", stderr(&out));
        if let Some(s) = &mut self.clock {
            *s += 1;
        }
        out
    }

    /// A commit adding one new file named after itself.
    fn commit(&mut self, name: &str) -> String {
        std::fs::write(self.repo.join(name), format!("{name}\n")).unwrap();
        self.ok(&["add", name]);
        self.tick(&["commit", "-q", "-m", name]);
        self.rev("HEAD")
    }

    fn merge(&mut self, branch: &str) -> String {
        self.tick(&["merge", "-q", "--no-ff", "-m", "merge", branch]);
        self.rev("HEAD")
    }

    fn rev(&self, spec: &str) -> String {
        stdout(&self.ok(&["rev-parse", spec])).trim().to_owned()
    }

    fn annotated(&self, name: &str) {
        self.ok(&["tag", "-a", "-m", name, name]);
    }

    fn lightweight(&self, name: &str) {
        self.ok(&["tag", name]);
    }

    fn branch_at(&self, branch: &str, at: &str) {
        self.ok(&["checkout", "-q", "-b", branch, at]);
    }

    fn checkout(&self, branch: &str) {
        self.ok(&["checkout", "-q", branch]);
    }

    fn describe(&self, args: &[&str]) -> Output {
        let mut argv = vec!["describe"];
        argv.extend_from_slice(args);
        self.git(&argv)
    }

    fn name_rev(&self, args: &[&str]) -> Output {
        let mut argv = vec!["name-rev"];
        argv.extend_from_slice(args);
        self.git(&argv)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Assert `line` is `<expected>-g<abbrev>` for some abbreviation of `oid`.
///
/// The abbreviation length is sized from the repository's object count, so pinning
/// it would make these tests about `core.abbrev` instead of about the walk.
fn assert_described(line: &str, expected: &str, oid: &str) {
    let line = line.trim();
    let prefix = format!("{expected}-g");
    let abbrev = line
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("expected `{prefix}<abbrev>`, got {line:?}"));
    assert!(
        !abbrev.is_empty() && oid.starts_with(abbrev),
        "{abbrev:?} is not an abbreviation of {oid}"
    );
}

/// The t6120 shape, flattened onto one timestamp:
///
/// ```text
///   initial(A) -- third(b,e) -- fourth(A2,test1,test2) -- fifth(test-lightweight) -- merge(mergetag) -- seventh
///      |                                                                             /
///      +-- sixth(sidetag) ----------------------------------------------------------+
///      |
///      +-- second(c)          (never merged)
/// ```
///
/// Three annotated tags share `fourth`, which is the tie group `--contains` has to
/// resolve. `test-lightweight` is the lightweight candidate. The unmerged `second`
/// and the side branch keep the queue non-empty at the moment the candidate table
/// fills, which is what reaches the names-exhausted break.
fn flat_history(name: &str) -> Fixture {
    let mut f = Fixture::new(name);
    let initial = f.commit("f0");
    f.annotated("A");

    f.branch_at("other", &initial);
    f.commit("f2");
    f.annotated("c");

    f.checkout("main");
    f.commit("f3");
    f.annotated("b");
    f.lightweight("e");
    f.commit("f4");
    f.annotated("A2");
    f.annotated("test1");
    f.annotated("test2");
    f.commit("f5");
    f.lightweight("test-lightweight");

    f.branch_at("side", &initial);
    f.commit("f6");
    f.annotated("sidetag");

    f.checkout("main");
    f.merge("side");
    f.annotated("mergetag");
    f.tick(&["commit", "-q", "--allow-empty", "-m", "seventh"]);
    f
}

/// `--tags --match 'test*'` admits exactly two commits, `fourth` and `fifth`. Once
/// both are candidates there is nothing left to find, so git takes the second arm
/// of `builtin/describe.c:418-419` on the very next pop and stops — reporting that
/// commit, and not counting the rest of the history into the depth.
///
/// Walking on instead printed `test-lightweight-4-`, one more than stock, because
/// every further iteration bumped the best candidate's depth.
#[test]
fn the_walk_stops_once_every_known_name_is_a_candidate() {
    let f = flat_history("names-exhausted");
    let head = f.rev("HEAD");
    let initial = f.rev("A^{commit}");

    let out = f.describe(&["--tags", "--debug", "--match", "test*", "HEAD"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_described(&stdout(&out), "test-lightweight-3", &head);
    assert_eq!(
        stderr(&out),
        format!(
            "describe HEAD\n\
             No exact match on refs or tags, searching to describe\n\
             \x20lightweight        3 test-lightweight\n\
             \x20annotated          4 test1\n\
             traversed 7 commits\n\
             found 10 tags; gave up search at {initial}\n"
        )
    );
}

/// The message git prints for that stop, verbatim. 2.55.0 says it on one line and
/// names `max_candidates` — the *limit*, not the number of tags it listed
/// (builtin/describe.c:523-528) — which is why the default says "found 10 tags"
/// above a two-row table, and `--candidates=3` says "found 3".
#[test]
fn the_give_up_message_names_the_candidate_limit_on_one_line() {
    let f = flat_history("give-up-wording");
    let initial = f.rev("A^{commit}");

    let out = f.describe(&["--tags", "--debug", "--candidates=3", "--match", "test*", "HEAD"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.ends_with(&format!("found 3 tags; gave up search at {initial}\n")),
        "expected git 2.55.0's one-line wording, got:\n{err}"
    );
    assert!(
        !err.contains("most recent"),
        "\"more than N tags found; listed N most recent\" is the pre-2.55 wording:\n{err}"
    );
}

/// Without `--tags`, `test-lightweight` never becomes a candidate — but it is still
/// in git's `names`, so the names-exhausted arm needs *two* names before it can
/// fire, and the walk runs on to `third`, where the queue empties and the annotated
/// candidate already covers it.
///
/// Sizing that arm from the candidate map instead (one name, not two) stopped the
/// walk at the first annotated tag and printed a shorter depth.
#[test]
fn the_names_count_includes_the_lightweight_tags_no_candidate_will_ever_use() {
    let f = flat_history("names-width");
    let head = f.rev("HEAD");
    let third = f.rev("b^{commit}");

    let out = f.describe(&["--debug", "--match", "test*", "HEAD"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_described(&stdout(&out), "test1-5", &head);
    assert_eq!(
        stderr(&out),
        format!(
            "describe HEAD\n\
             No exact match on refs or tags, searching to describe\n\
             finished search at {third}\n\
             \x20annotated          5 test1\n\
             traversed 7 commits\n"
        )
    );
}

/// A linear history `f0 -- f1(ann) -- f2(lw) -- f3`, where `--tags` reaches the
/// lightweight tag with the queue already empty.
///
/// The early stop is guarded by `annotated_cnt`, not by "any candidate", so the
/// walk must carry on and pick up the annotated tag behind it: two rows in the
/// table and two more commits traversed. Guarding on the candidate count ended the
/// search at the lightweight tag with a one-row table and `traversed 2 commits`.
#[test]
fn a_lightweight_candidate_does_not_arm_the_early_stop() {
    let mut f = Fixture::new("lightweight-stop").ticking();
    f.commit("f0");
    f.commit("f1");
    f.annotated("ann");
    let annotated = f.rev("HEAD");
    f.commit("f2");
    f.lightweight("lw");
    let head = f.commit("f3");

    let out = f.describe(&["--tags", "--debug", "HEAD"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_described(&stdout(&out), "lw-1", &head);
    assert_eq!(
        stderr(&out),
        format!(
            "describe HEAD\n\
             No exact match on refs or tags, searching to describe\n\
             finished search at {annotated}\n\
             \x20lightweight        1 lw\n\
             \x20annotated          2 ann\n\
             traversed 3 commits\n"
        ),
        "the walk has to reach the annotated tag behind the lightweight one"
    );
}

/// ```text
///   f0 -- f1(v1) -- merge
///    \              /
///     g1 -- g2 ----+
/// ```
///
/// with `f1` newer than `g2`. Under `--candidates=1` the table fills at `f1` and
/// the *next* pop is `g2`, which carries no name at all — the check runs before the
/// popped commit is looked up in the name map (builtin/describe.c:418), so that is
/// the commit reported and the walk ends there.
///
/// Testing the limit only where a name was found skipped `g2`, `g1` and `f0`, ran
/// the queue dry, and reported no give-up commit at all.
#[test]
fn the_give_up_commit_can_be_a_commit_with_no_name() {
    let mut f = Fixture::new("give-up-unnamed").ticking();
    let root = f.commit("f0");

    f.branch_at("side", &root);
    f.commit("g1");
    let side_tip = f.commit("g2");

    f.checkout("main");
    f.commit("f1");
    f.annotated("v1");
    let head = f.merge("side");

    let out = f.describe(&["--debug", "--candidates=1", "HEAD"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_described(&stdout(&out), "v1-3", &head);
    assert_eq!(
        stderr(&out),
        format!(
            "describe HEAD\n\
             No exact match on refs or tags, searching to describe\n\
             \x20annotated          3 v1\n\
             traversed 5 commits\n\
             found 1 tags; gave up search at {side_tip}\n"
        )
    );
    assert!(
        stdout(&f.name_rev(&["--name-only", "--tags", &side_tip])).trim() != side_tip,
        "the give-up commit is meant to be one the tag walk reaches, not a tagged one"
    );
}

/// The commit the walk gave up on goes back on the queue under its own date
/// (`prio_queue_replace`, prio-queue.c:105-115), behind the commits already queued
/// at that date rather than in front of them.
///
/// In this fixture every commit shares one second, so re-queuing it at the head of
/// the queue changed what `finish_depth_computation()` visited and the printed
/// depth came out one too large.
#[test]
fn the_give_up_commit_rejoins_the_queue_in_date_order() {
    let f = flat_history("give-up-order");
    let merge = f.rev("mergetag^{commit}");

    // `HEAD~1` is the merge: the merge itself and `sixth` are in its history but
    // not in `fifth`'s, so the depth is 2.
    let out = f.describe(&["--tags", "--match", "test*", "HEAD~1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_described(&stdout(&out), "test-lightweight-2", &merge);
}

/// `--contains` is `cmd_name_rev()` with a fixed argument vector
/// (builtin/describe.c:710-748), so it has to answer exactly what that invocation
/// answers — including on a tie.
///
/// `fourth` carries `A2`, `test1` and `test2`, all created in one second: the tie
/// `cmp_by_tag_and_age()` cannot break, left to an unstable `QSORT`
/// (builtin/name-rev.c:460). Which of the three wins is the C library's business,
/// and name-rev reproduces it by calling that very `qsort`; a second, independent
/// copy of the walk inside `describe` cannot, and answered `A2^0` where the
/// delegation answers `test2^0`. So what is worth pinning is that both commands go
/// through the one implementation.
#[test]
fn contains_answers_exactly_what_the_name_rev_it_delegates_to_answers() {
    let f = flat_history("contains-tie");

    for tag in ["test1", "test2", "A2"] {
        let describe = f.describe(&["--contains", tag]);
        let name_rev = f.name_rev(&["--peel-tag", "--name-only", "--no-undefined", "--tags", tag]);
        assert!(describe.status.success(), "describe --contains {tag}: {}", stderr(&describe));
        assert_eq!(
            stdout(&describe),
            stdout(&name_rev),
            "describe --contains {tag} must be its name-rev delegation"
        );
        // The tie is real: all three tags sit on one commit, so this is the case an
        // independent walk gets wrong, not a single-candidate walkover.
        assert!(
            stdout(&describe).trim().ends_with("^0"),
            "expected a peeled tag name, got {:?}",
            stdout(&describe)
        );
    }
}

/// The same delegation under `--all`, which drops `--tags` and adds the
/// `refs/heads/` and `refs/remotes/` pattern prefixes (builtin/describe.c:715-732).
#[test]
fn contains_all_delegates_without_the_tags_restriction() {
    let f = flat_history("contains-all");

    let describe = f.describe(&["--contains", "--all", "test1"]);
    let name_rev = f.name_rev(&["--peel-tag", "--name-only", "--no-undefined", "test1"]);
    assert_eq!(stdout(&describe), stdout(&name_rev));
    assert!(
        stdout(&describe).starts_with("tags/"),
        "--all keeps git's `tags/` prefix, got {:?}",
        stdout(&describe)
    );

    // `--match side` reaches name-rev as `--refs=refs/heads/side` among others;
    // the merge is not in that branch's past, so `--no-undefined` is fatal.
    let out = f.describe(&["--contains", "--all", "--match", "side", "HEAD"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stderr(&out), format!("fatal: cannot describe '{}'\n", f.rev("HEAD")));
}

/// None of the stops above may disturb the case that never walks: a name sitting on
/// the commit itself still short-circuits at builtin/describe.c:376-386, before the
/// `--debug` narration starts.
#[test]
fn an_exact_match_still_answers_without_walking() {
    let f = flat_history("exact");

    let out = f.describe(&["--debug", "HEAD~1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "mergetag");
    assert_eq!(stderr(&out), "describe HEAD~1\n");
}
