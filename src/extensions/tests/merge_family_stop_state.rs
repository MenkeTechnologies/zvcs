//! The repository state the merge family leaves when it stops, and the identity
//! its summaries name.
//!
//! These verbs mutate; the part a port gets wrong is not the wording but which
//! files are on disk afterwards, because the recovery commands read those files
//! rather than the message. `MERGE_HEAD` is what makes the next `git commit`
//! write a two-parent commit and what gives `git merge --abort` something to
//! abort; `MERGE_MSG` is the message that commit uses; `AUTO_MERGE` is the tree
//! `git diff AUTO_MERGE` compares against; `REBASE_HEAD` names the commit a
//! stopped rebase was applying. A stop that records the wrong set of them looks
//! right on stdout and behaves wrong on the next command.
//!
//! Every conflict case here asserts the conflict *first* — a fixture that
//! quietly stopped conflicting would otherwise let the state assertions pass
//! against a clean merge that never wrote anything.
//!
//! Expectations were measured against stock 2.55.0, and when the machine has a
//! stock git each case is additionally diffed against it, so an expectation that
//! is self-consistent but not git's fails here rather than shipping.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A stock git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// Resolved explicitly rather than through `PATH`: on a machine where zvcs
/// shadows `git` a `PATH` lookup silently makes the oracle the thing under test.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .filter(|p| Path::new(p).exists())
        .filter_map(|p| Some((version_of(p)?, p.to_owned())))
        // The state below includes files whose contents git rewords between
        // releases. An older git is a different oracle, not a worse one, so a
        // machine that only has one simply runs the pinned expectations.
        .filter(|(v, _)| *v >= (2, 55, 0))
        .max()
        .map(|(_, p)| p)
}

fn version_of(bin: &str) -> Option<(u32, u32, u32)> {
    let out = Command::new(bin).arg("--version").env_clear().output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.trim().strip_prefix("git version ")?;
    let mut parts = rest.split(['.', ' ', '-']).filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// A fixture repository plus the binary that built it.
struct Repo {
    bin: String,
    dir: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn git(&self, args: &[&str]) -> Output {
        self.git_env(&[], args)
    }

    /// [`Repo::git`] with extra environment, for the cases that need an author
    /// identity different from the committer's.
    fn git_env(&self, extra: &[(&str, &str)], args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(&self.dir)
            .env_remove("GIT_CHERRY_PICK_HELP")
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("GIT_AUTHOR_NAME", "C O Mitter")
            .env("GIT_AUTHOR_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn rev(&self, spec: &str) -> String {
        String::from_utf8_lossy(&self.git(&["rev-parse", spec]).stdout).trim().to_owned()
    }

    fn write(&self, file: &str, body: &str) {
        std::fs::write(self.dir.join(file), format!("{body}\n")).unwrap();
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        self.write(file, body);
        assert!(self.git(&["add", file]).status.success(), "add {file}");
        let out = self.git(&["commit", "-q", "-m", msg]);
        assert!(out.status.success(), "commit {msg}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// The contents of `.git/<name>`, or `None` when it does not exist.
    fn state(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(".git").join(name)).ok()
    }

    fn worktree(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.join(file)).unwrap()
    }
}

fn fixture(bin: &str, tag: &str) -> Repo {
    let root =
        std::env::temp_dir().join(format!("zvcs-mfss-{tag}-{}-{:p}", std::process::id(), &tag));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let dir = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let repo = Repo { bin: bin.to_owned(), dir, home };
    assert!(repo.git(&["init", "-q", "-b", "main", "."]).status.success(), "init");
    repo
}

/// What one case observes: the run's own output plus every state file that
/// distinguishes one kind of stop from another.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    state: Vec<(&'static str, Option<String>)>,
}

/// The state files a stopped merge, rebase, cherry-pick or revert can leave.
/// Collected as a whole rather than one assertion per file so a case that starts
/// writing an *extra* one fails too.
const STATE_FILES: &[&str] = &[
    "MERGE_HEAD",
    "MERGE_MSG",
    "MERGE_MODE",
    "AUTO_MERGE",
    "SQUASH_MSG",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "rebase-merge/message",
    "rebase-merge/author-script",
    "rebase-merge/stopped-sha",
];

fn observe(repo: &Repo, out: Output) -> Observed {
    Observed {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
        state: STATE_FILES.iter().map(|f| (*f, repo.state(f))).collect(),
    }
}

/// Run `case` against zvcs and — when the machine has one — against stock git,
/// asserting the two observe the same thing.
///
/// The stock side is named in the assertion because the direction matters when
/// reading a failure: `expected` is stock, `actual` is zvcs.
fn both<F>(tag: &str, case: F) -> (Repo, Observed)
where
    F: Fn(&Repo) -> Output,
{
    let zvcs = fixture(BIN, &format!("{tag}-zvcs"));
    let zobs = observe(&zvcs, case(&zvcs));

    if let Some(bin) = stock_git() {
        let stock = fixture(&bin, &format!("{tag}-stock"));
        let sobs = observe(&stock, case(&stock));
        assert_eq!(
            sobs, zobs,
            "{tag}: zvcs must observe what stock does (left = stock 2.55.0, right = zvcs)"
        );
    }
    (zvcs, zobs)
}

/// `main` and `side` rewrite the same line of `f.txt`, so merging or replaying
/// one onto the other always conflicts.
fn diverged(repo: &Repo) {
    repo.commit("f.txt", "base", "base");
    assert!(repo.git(&["checkout", "-q", "-b", "side"]).status.success(), "branch side");
    repo.commit("f.txt", "side", "sidechange");
    assert!(repo.git(&["checkout", "-q", "main"]).status.success(), "back to main");
    repo.commit("f.txt", "mainline", "mainchange");
}

// --------------------------------------------------------- merge --squash ---

/// `cmd_merge()` sends a conflicted `--squash` through `finish()` with a NULL new
/// head rather than `write_merge_state()` (builtin/merge.c:1770-1775): it
/// announces the squash, writes `SQUASH_MSG`, and records *no* `MERGE_HEAD` and
/// no `MERGE_MODE`. `suggest_conflicts()` then appends the conflict hint to a
/// `MERGE_MSG` nothing has written, so the hint stands alone in it.
///
/// The distinction is a state one, not a wording one: a `MERGE_HEAD` here would
/// make the following `git commit` write a merge commit, which is exactly what
/// `--squash` exists not to do.
#[test]
fn squash_conflict_records_squash_msg_and_no_merge_head() {
    let (repo, obs) = both("squash-conflict", |r| {
        diverged(r);
        r.git(&["merge", "--squash", "side"])
    });

    assert!(
        repo.worktree("f.txt").contains("<<<<<<< HEAD"),
        "fixture must conflict, or the state below is a clean merge's:\n{}",
        repo.worktree("f.txt")
    );
    assert_eq!(obs.code, Some(1), "a conflicted merge exits 1:\n{}", obs.stdout);
    assert!(
        obs.stdout.contains("Squash commit -- not updating HEAD\n"),
        "squash_message() announces itself before it writes:\n{}",
        obs.stdout
    );

    let state = |name: &str| obs.state.iter().find(|(f, _)| *f == name).unwrap().1.clone();
    assert_eq!(state("MERGE_HEAD"), None, "a squash records no merge to conclude");
    assert_eq!(state("MERGE_MODE"), None, "and no merge mode");
    assert_eq!(
        state("MERGE_MSG").as_deref(),
        Some("\n# Conflicts:\n#\tf.txt\n"),
        "the appended hint is the whole file"
    );
    assert!(
        state("SQUASH_MSG")
            .is_some_and(|s| s.starts_with("Squashed commit of the following:\n")),
        "SQUASH_MSG carries the squashed log: {:?}",
        state("SQUASH_MSG")
    );
}

// --------------------------------------------------------------- rebase -----

/// A conflicted pick leaves everything `git commit` and `git rebase --continue`
/// need to conclude it: `MERGE_MSG` from `do_pick_commit()`
/// (sequencer.c:2309-2310), `AUTO_MERGE` from `merge_switch_to_result()`
/// (merge-ort.c:4702-4707), and `REBASE_HEAD` plus the author script from
/// `make_patch()`/`write_author_script()` (sequencer.c:3450-3453, 2298).
///
/// The author script is the one with a silent failure mode: without it the
/// commit the user makes at the stop is authored by whoever is resolving the
/// conflict rather than by whoever wrote the commit being replayed.
#[test]
fn rebase_conflict_records_the_state_its_recovery_paths_read() {
    let (repo, obs) = both("rebase-conflict", |r| {
        diverged(r);
        assert!(r.git(&["checkout", "-q", "side"]).status.success(), "onto side");
        r.git(&["rebase", "main"])
    });

    assert!(
        repo.worktree("f.txt").contains("<<<<<<< HEAD"),
        "fixture must conflict:\n{}",
        repo.worktree("f.txt")
    );
    assert!(
        obs.stderr.contains("error: could not apply "),
        "and must stop on that conflict:\n{}",
        obs.stderr
    );

    let state = |name: &str| obs.state.iter().find(|(f, _)| *f == name).unwrap().1.clone();
    assert_eq!(
        state("MERGE_MSG").as_deref(),
        Some("sidechange\n\n# Conflicts:\n#\tf.txt\n"),
        "the replayed message plus the conflict hint"
    );
    assert!(state("AUTO_MERGE").is_some(), "the merged tree stays available as AUTO_MERGE");
    // The rebase is stopped, so it has not moved `side` yet — the branch still
    // names the commit whose pick conflicted.
    assert_eq!(
        state("REBASE_HEAD").as_deref().map(str::trim),
        Some(repo.rev("side").as_str()),
        "REBASE_HEAD names the commit being applied"
    );
    assert_eq!(
        state("rebase-merge/author-script").as_deref(),
        Some(
            "GIT_AUTHOR_NAME='C O Mitter'\n\
             GIT_AUTHOR_EMAIL='committer@example.com'\n\
             GIT_AUTHOR_DATE='@1672531200 +0000'\n"
        ),
        "the replayed author survives the stop"
    );
    assert_eq!(
        state("rebase-merge/message").as_deref(),
        state("MERGE_MSG").as_deref(),
        "--continue commits the same message a bare `git commit` would"
    );
}

/// The same stop, concluded: `--continue` must produce the commit the pick was
/// going to make, which is only possible if the message and author it left
/// behind were the right ones.
#[test]
fn rebase_continue_uses_the_message_and_author_the_stop_recorded() {
    let (_repo, obs) = both("rebase-continue", |r| {
        diverged(r);
        assert!(r.git(&["checkout", "-q", "side"]).status.success(), "onto side");
        let stop = r.git(&["rebase", "main"]);
        assert!(
            String::from_utf8_lossy(&stop.stderr).contains("error: could not apply "),
            "fixture must stop on a conflict: {}",
            String::from_utf8_lossy(&stop.stderr)
        );
        r.write("f.txt", "resolved");
        assert!(r.git(&["add", "f.txt"]).status.success(), "stage the resolution");
        // A different identity resolves the conflict than wrote the commit, so a
        // lost author script shows up as a changed author on the result.
        r.git_env(
            &[("GIT_AUTHOR_NAME", "R E Solver"), ("GIT_AUTHOR_EMAIL", "solver@example.com")],
            &["rebase", "--continue"],
        );
        r.git(&["log", "-1", "--format=%an <%ae>|%cn <%ce>|%B"])
    });

    assert_eq!(
        obs.stdout, "C O Mitter <committer@example.com>|C O Mitter <committer@example.com>|sidechange\n\n",
        "the replayed author and message survive the stop"
    );
}

// --------------------------------------------------------------- revert -----

/// `print_commit_summary()` prints ` Author:` when the *new* commit's author and
/// committer identities differ (sequencer.c:1339-1344). A revert never reuses the
/// reverted commit's author, so the line is about the identity doing the
/// reverting — which is why a fixture with a single identity cannot see it.
#[test]
fn revert_summary_names_a_divergent_author() {
    let author = [("GIT_AUTHOR_NAME", "A U Thor"), ("GIT_AUTHOR_EMAIL", "author@example.com")];

    let (_repo, split) = both("revert-split-ident", |r| {
        r.commit("f.txt", "base", "base");
        for n in 1..=3 {
            r.write("f.txt", &format!("line{n}"));
            r.git(&["commit", "-q", "-a", "-m", &format!("c{n}")]);
        }
        r.git_env(&author, &["revert", "--no-edit", "HEAD~2..HEAD"])
    });

    assert_eq!(
        split.stdout.matches(" Author: A U Thor <author@example.com>\n").count(),
        2,
        "each reverted commit's summary names the divergent author:\n{}",
        split.stdout
    );

    let (_repo, same) = both("revert-same-ident", |r| {
        r.commit("f.txt", "base", "base");
        for n in 1..=3 {
            r.write("f.txt", &format!("line{n}"));
            r.git(&["commit", "-q", "-a", "-m", &format!("c{n}")]);
        }
        r.git(&["revert", "--no-edit", "HEAD~2..HEAD"])
    });

    assert!(
        !same.stdout.contains(" Author: "),
        "an identity that matches the committer's earns no line:\n{}",
        same.stdout
    );
}

// -------------------------------------------------- merge reduce_parents ----

/// `collect_parents()` inserts `HEAD` into the head list and then reduces the
/// whole set to its independent members (builtin/merge.c:1214-1215, 1102-1131),
/// so an operand `HEAD` already reaches is gone before a strategy is picked.
/// `git merge <side> <ancestor>` is therefore the two-head `git merge <side>` —
/// ort, one `MERGE_HEAD`, an `AUTO_MERGE` — and not an octopus.
#[test]
fn merge_drops_an_ancestor_operand_before_choosing_a_strategy() {
    let (repo, obs) = both("merge-reduce", |r| {
        r.commit("f.txt", "base", "base");
        assert!(r.git(&["checkout", "-q", "-b", "side"]).status.success(), "branch side");
        r.commit("g.txt", "side", "s1");
        assert!(r.git(&["checkout", "-q", "main"]).status.success(), "back to main");
        r.commit("h.txt", "m1", "m1");
        assert!(r.git(&["tag", "anc"]).status.success(), "tag the ancestor");
        r.commit("h.txt", "m2", "m2");
        r.git(&["merge", "--no-commit", "side", "anc"])
    });

    assert!(
        !obs.stdout.contains("Trying simple merge with"),
        "the octopus strategy never runs once the ancestor is dropped:\n{}",
        obs.stdout
    );

    let state = |name: &str| obs.state.iter().find(|(f, _)| *f == name).unwrap().1.clone();
    assert_eq!(
        state("MERGE_MSG").as_deref(),
        Some("Merge branch 'side'\n"),
        "the dropped operand is not named in the generated message"
    );
    assert_eq!(
        state("MERGE_HEAD").as_deref().map(str::trim),
        Some(repo.rev("side").as_str()),
        "only the surviving head is recorded"
    );
    assert!(state("AUTO_MERGE").is_some(), "ort ran, so its result is recorded");
}

/// The degenerate end of the same rule: when every operand is reachable from
/// `HEAD` the reduced list is empty, and `cmd_merge()` reports `Already up to
/// date.` without running any strategy (builtin/merge.c:1550-1558).
#[test]
fn merge_of_only_ancestors_is_already_up_to_date() {
    let (_repo, obs) = both("merge-reduce-empty", |r| {
        r.commit("f.txt", "base", "base");
        assert!(r.git(&["tag", "anc0"]).status.success(), "tag anc0");
        r.commit("f.txt", "two", "m1");
        assert!(r.git(&["tag", "anc1"]).status.success(), "tag anc1");
        r.commit("f.txt", "three", "m2");
        r.git(&["merge", "--no-commit", "anc0", "anc1"])
    });

    assert_eq!(obs.stdout, "Already up to date.\n", "no strategy runs at all");
    assert_eq!(obs.code, Some(0));
    for (name, contents) in &obs.state {
        assert_eq!(*contents, None, "{name} must not be written by an up-to-date merge");
    }
}
