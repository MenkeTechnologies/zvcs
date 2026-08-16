//! The three `setup_revisions()` shapes `cherry-pick`/`revert` reach that are
//! neither a plain commit nor a `<a>..<b>` range:
//!
//! * the pseudo-revisions — `--not`, `--all`, `--branches`, `--tags`,
//!   `--remotes`, `--glob=<pattern>`, `--exclude=<pattern>` — which
//!   `handle_revision_pseudo_opt()` claims out of the operand list before any of
//!   it is treated as a revision;
//! * `<a>^-[<n>]`, which `handle_revision_arg_1()` rewrites into
//!   `<a>^<n>..<a>`;
//! * `verify_opt_compatible()`'s list, which decides which options a sequencer
//!   verb refuses — and, for `--strategy`, does not.
//!
//! Every case is diffed against a stock git when the machine has one; the
//! differential is what makes the expectations git's rather than this port's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A STOCK git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// Resolved EXPLICITLY rather than through `PATH`: on a machine where zvcs
/// shadows git a `PATH` lookup silently makes the oracle the thing under test.
/// The *newest* installed git wins, the policy `src/parity/src/stock.rs` uses.
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

struct Repo {
    bin: String,
    dir: PathBuf,
    home: PathBuf,
    /// Commit timestamps advance one day per commit, so the walk's commit-date
    /// order — and every object id — is the same in both fixtures.
    day: usize,
}

impl Repo {
    fn git(&self, args: &[&str]) -> Output {
        let date = date_of(self.day);
        run(&self.bin, &self.dir, &self.home, &date, args)
    }

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

    /// The instructions left in `.git/sequencer/todo`, subject only, or an empty
    /// list when no sequence is live.
    fn todo(&self) -> Vec<String> {
        let path = self.dir.join(".git/sequencer/todo");
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| line.splitn(3, ' ').nth(2).map(str::to_owned))
            .collect()
    }

    fn has(&self, path: &str) -> bool {
        self.dir.join(".git").join(path).exists()
    }
}

fn date_of(day: usize) -> String {
    format!("2023-01-{:02} 00:00:00 +0000", day + 1)
}

/// ```text
///        feat1 - feat2                    (feature)
///       /
/// base -- side1 ---------------\          (side, refs/remotes/origin/side)
///       \                       \
///        main1 - main2 --------- merge    (main, HEAD)
/// ```
///
/// `merge` has two parents, which is what `<a>^-<n>` needs a choice of; two
/// commits on the mainline give a walked selection something to order; `v1` tags
/// `main1` so `--tags` and `--all` select something no branch tip does, and
/// `refs/remotes/origin/side` gives `--remotes` a namespace of its own.
fn fixture(bin: &str, tag: &str) -> Repo {
    let root = std::env::temp_dir().join(format!("zvcs-cppseudo-{tag}-{}", std::process::id()));
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
    repo.checkout(&["main"]);
    repo.checkout(&["-b", "side"]);
    repo.commit("side1");
    repo.checkout(&["main"]);
    repo.commit("main1");
    repo.commit("main2");
    repo.day += 1;
    let out = repo.git(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    assert!(out.status.success(), "merge side: {}", String::from_utf8_lossy(&out.stderr));
    repo.git(&["tag", "v1", "main~2"]);
    let side = repo.git(&["rev-parse", "side"]);
    let side = String::from_utf8_lossy(&side.stdout).trim().to_owned();
    repo.git(&["update-ref", "refs/remotes/origin/side", &side]);
    repo
}

/// What a run is compared on: everything a caller could observe about the
/// selection, short of the merge output itself.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    status: Option<i32>,
    stderr: String,
    subjects: Vec<String>,
    todo: Vec<String>,
    sequencer: bool,
}

fn observe(repo: &Repo, out: &Output) -> Observed {
    Observed {
        status: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        subjects: repo.subjects(9),
        todo: repo.todo(),
        sequencer: repo.has("sequencer"),
    }
}

/// Run `args` under zvcs, and under stock git in an identical fixture when one
/// exists, asserting the two agree on everything [`Observed`] records.
fn both(tag: &str, args: &[&str]) -> Observed {
    let zvcs = fixture(BIN, &format!("{tag}-zvcs"));
    let zout = zvcs.git(args);
    let observed = observe(&zvcs, &zout);

    if let Some(bin) = stock_git() {
        let stock = fixture(&bin, &format!("{tag}-stock"));
        let sout = stock.git(args);
        let expected = observe(&stock, &sout);
        assert_eq!(expected.status, observed.status, "exit status must match stock for {args:?}");
        assert_eq!(expected.stderr, observed.stderr, "stderr must match stock for {args:?}");
        assert_eq!(expected.subjects, observed.subjects, "history must match stock for {args:?}");
        assert_eq!(expected.todo, observed.todo, "todo list must match stock for {args:?}");
        assert_eq!(
            expected.sequencer, observed.sequencer,
            "sequencer directory must match stock for {args:?}"
        );
        let _ = std::fs::remove_dir_all(stock.dir.parent().unwrap());
    }

    let _ = std::fs::remove_dir_all(zvcs.dir.parent().unwrap());
    observed
}

// --- verify_opt_compatible ------------------------------------------------

/// `--strategy` beside a sequencer verb is **accepted**, and that is not an
/// oversight to be tidied up: `run_sequencer()` parses `--strategy` into a local
/// pointer and only copies it into `opts->strategy` *after*
/// `verify_opt_compatible()` has read that field, so the list's
/// `"--strategy", opts->strategy ? 1 : 0` entry can never fire from a command
/// line. `--quit` with no sequence live is therefore a silent success.
#[test]
fn strategy_is_accepted_beside_every_sequencer_verb() {
    let quit = both("strategy-quit", &["cherry-pick", "--strategy=ort", "--quit"]);
    assert_eq!(quit.status, Some(0), "--strategy=ort --quit is a silent success");
    assert_eq!(quit.stderr, "", "and prints nothing");

    // The other three still reach their own refusal rather than the option
    // check, which is what proves `--strategy` was not what stopped them.
    for verb in ["--continue", "--abort"] {
        let out = both(&format!("strategy{verb}"), &["cherry-pick", "--strategy=ort", verb]);
        assert_eq!(out.status, Some(128), "{verb} with nothing in progress is a 128");
        assert_eq!(
            out.stderr,
            "error: no cherry-pick or revert in progress\nfatal: cherry-pick failed\n"
        );
    }
    let skip = both("strategy-skip", &["cherry-pick", "--strategy=ort", "--skip"]);
    assert_eq!(skip.stderr, "error: no cherry-pick in progress\nfatal: cherry-pick failed\n");

    // `revert` shares the option table and reads the same list.
    let revert = both("strategy-revert-quit", &["revert", "--strategy=ort", "--quit"]);
    assert_eq!(revert.status, Some(0), "revert --strategy=ort --quit is a silent success too");
}

/// The entries around `--strategy` in that same list do fire, so the fix above
/// is `--strategy` alone and not the whole check going quiet. `-X` is the
/// neighbouring entry; `--empty=keep` is reported under the
/// `--keep-redundant-commits` name because `run_sequencer()` folds the two
/// before the check runs.
#[test]
fn the_rest_of_verify_opt_compatible_still_refuses() {
    for (arg, named) in [
        ("-Xtheirs", "--strategy-option"),
        ("-n", "--no-commit"),
        ("-x", "-x"),
        ("--empty=keep", "--keep-redundant-commits"),
        ("--empty=drop", "--empty"),
        ("--rerere-autoupdate", "--rerere-autoupdate"),
    ] {
        let out = both(&format!("incompat{named}"), &["cherry-pick", arg, "--quit"]);
        assert_eq!(out.status, Some(128), "{arg} --quit must be fatal");
        assert_eq!(
            out.stderr,
            format!("fatal: cherry-pick: {named} cannot be used with --quit\n"),
            "{arg} must be reported under the name git's list gives it"
        );
    }
}

// --- <a>^-[<n>] -----------------------------------------------------------

/// `<a>^-` is `<a>^1..<a>` — the commit and everything under it that its first
/// parent does not reach. On a merge that is the whole side branch, and because
/// the exclusion turns the walk on it is replayed oldest first.
#[test]
fn caret_dash_excludes_the_first_parent() {
    let out = both("caret-dash", &["cherry-pick", "-n", "feature^-"]);
    assert_eq!(out.status, Some(0), "feature^- picks the tip alone: {}", out.stderr);
}

/// `<a>^-<n>` picks *which* parent to exclude, so on a merge `^-1` keeps the
/// side branch and `^-2` keeps the mainline. The selection is read off the todo
/// list the stopped sequence leaves behind: replaying `main`'s own commits onto
/// `main` produces an empty result and stops on the very first instruction, so
/// the file still holds the whole list.
#[test]
fn caret_dash_n_chooses_which_parent_is_excluded() {
    let first = both("caret-dash-1", &["cherry-pick", "main^-1"]);
    assert_eq!(
        first.todo,
        ["side1", "merge"],
        "^-1 excludes the mainline parent, leaving the side branch and the merge"
    );

    let second = both("caret-dash-2", &["cherry-pick", "main^-2"]);
    assert_eq!(
        second.todo,
        ["main1", "main2", "merge"],
        "^-2 excludes the side parent, leaving the mainline commits, oldest first"
    );
}

/// The mark is found anywhere in the operand — `strstr(arg, "^-")` — and
/// everything before it is a rev-spec in its own right, so the *include* side is
/// that whole prefix and not just the ref it started from.
#[test]
fn caret_dash_applies_to_the_navigated_commit_not_the_bare_ref() {
    let out = both("caret-dash-nav", &["cherry-pick", "-n", "main^^-"]);
    assert_eq!(
        out.status,
        Some(0),
        "main^^- is main^^^..main^, which is `main1` alone and applies cleanly: {}",
        out.stderr
    );
    assert_eq!(out.sequencer, false, "a finished sequence leaves no directory behind");
}

/// A leading `^` inverts the whole operand a second time: `add_parents_only()`
/// strips it and flips back, so the *parent* stays interesting while `<a>` does
/// not. git's own `rev-parse` refuses this spelling; `setup_revisions()` accepts
/// it, and the selection it produces is empty.
#[test]
fn caret_dash_under_a_leading_caret_selects_nothing() {
    let out = both("caret-dash-excluded", &["cherry-pick", "^main^-1"]);
    assert_eq!(out.status, Some(128));
    assert_eq!(out.stderr, "error: empty commit set passed\nfatal: cherry-pick failed\n");
}

/// The spellings `add_parents_only()` refuses: a zero or absent parent number, a
/// number past the parent count, and a non-numeric suffix. Each leaves `arg`
/// unrewritten, so the whole operand is looked up as a revision and fails.
#[test]
fn malformed_caret_dash_is_a_bad_revision() {
    for spec in ["main^-0", "main^-3", "main^-x", "main^-1x"] {
        let out = both(&format!("caret-dash-bad-{spec}"), &["cherry-pick", spec]);
        assert_eq!(out.status, Some(128), "{spec} must be refused");
        assert_eq!(out.stderr, format!("fatal: bad revision '{spec}'\n"));
        assert!(!out.sequencer, "{spec} must not leave sequencer state behind");
    }
}

// --- pseudo-revisions -----------------------------------------------------

/// `--not` flips the UNINTERESTING sense for every operand *after* it and for
/// nothing before it, so the same two names either side of it select opposite
/// things: `<a> --not <b>` is `<a> ^<b>`, while `--not <a> <b>` marks both
/// uninteresting and selects nothing at all.
#[test]
fn not_flips_the_sense_of_later_operands_only() {
    let after = both("not-after", &["cherry-pick", "feature", "--not", "main"]);
    assert_eq!(
        after.subjects[..3],
        ["feat2", "feat1", "merge"],
        "feature ^main is the two feature commits, replayed oldest first onto the merge"
    );

    let before = both("not-before", &["cherry-pick", "--not", "feature", "main"]);
    assert_eq!(before.status, Some(128));
    assert_eq!(before.stderr, "error: empty commit set passed\nfatal: cherry-pick failed\n");
}

/// `--all` is every ref plus `HEAD`, queued in refname order with no walk — so
/// the tag contributes `main1` even though no branch tip names it, and `HEAD`
/// deduplicates against the branch it points at.
///
/// The selection is `refs/heads/feature`, `refs/heads/main`, `refs/heads/side`,
/// `refs/remotes/origin/side` (a duplicate of `side`), `refs/tags/v1` and then
/// `HEAD` (a duplicate of `main`) — four distinct commits. `feat2` is replayed
/// before the merge stops the sequence for want of `-m`, so what lands and what
/// is left on the todo list together spell the whole list out.
#[test]
fn all_selects_every_ref_and_head() {
    let out = both("all", &["cherry-pick", "--all"]);
    assert_eq!(out.subjects[0], "feat2", "the feature tip is replayed first");
    assert_eq!(
        out.todo,
        ["merge", "side1", "main1"],
        "then the merge, the side branch and the tagged commit — refname order, no walk"
    );
}

/// The namespace selectors are the same machinery with a prefix, and the prefix
/// is *trimmed* off the name before the exclusions are matched — which is why
/// `--exclude=side` skips a branch under `--branches` while `--all` would need
/// the full `refs/heads/side`.
#[test]
fn branches_tags_and_remotes_select_their_own_namespace() {
    let branches = both("branches", &["cherry-pick", "--branches"]);
    assert_eq!(branches.subjects[0], "feat2", "refs/heads/feature comes first");
    assert_eq!(branches.todo, ["merge", "side1"], "refs/heads/* only — no tag, no remote");

    let tags = both("tags", &["cherry-pick", "--tags"]);
    assert_eq!(tags.todo, ["main1"], "refs/tags/v1 alone");
    assert!(
        tags.sequencer,
        "a ref carries REV_CMD_REF, not REV_CMD_REV, so single_pick() is not taken even at length 1"
    );

    let remotes = both("remotes", &["cherry-pick", "-n", "--remotes"]);
    assert_eq!(remotes.status, Some(0), "refs/remotes/origin/side alone: {}", remotes.stderr);

    let excluded = both("branches-excluded", &["cherry-pick", "--exclude=side", "--branches"]);
    assert_eq!(excluded.subjects[0], "feat2");
    assert_eq!(
        excluded.todo,
        ["merge"],
        "--exclude matches the *trimmed* name the branch iterator hands out, so `side` is enough"
    );
}

/// `--glob=<pattern>` matches full refnames, and a pattern with no glob
/// character selects everything *below* it rather than the ref itself — the
/// implied `/*` in `refs_for_each_ref_ext()`. So the exact-looking
/// `--glob=refs/heads/side` matches nothing while `refs/heads/sid*` matches the
/// branch.
#[test]
fn glob_patterns_get_an_implied_trailing_slash_star() {
    let exact = both("glob-exact", &["cherry-pick", "--glob=refs/heads/side"]);
    assert_eq!(exact.status, Some(128));
    assert_eq!(exact.stderr, "error: empty commit set passed\nfatal: cherry-pick failed\n");

    let wild = both("glob-wild", &["cherry-pick", "-n", "--glob=refs/heads/sid*"]);
    assert_eq!(wild.status, Some(0), "refs/heads/side alone: {}", wild.stderr);

    // `parse_long_opt()` takes the detached spelling too.
    let detached = both("glob-detached", &["cherry-pick", "-n", "--glob", "refs/heads/sid*"]);
    assert_eq!(detached.status, Some(0), "--glob <pattern>: {}", detached.stderr);
}

/// A pseudo-revision is claimed by `setup_revisions()` and never counts towards
/// `run_sequencer()`'s `argc > 1` usage check — but a dash argument it does *not*
/// claim still does, and a bad revision anywhere outranks both.
#[test]
fn unclaimed_dash_arguments_are_still_a_usage_error() {
    let unknown = both("unknown-opt", &["cherry-pick", "--no-such-option", "main~1"]);
    assert_eq!(unknown.status, Some(129), "an option nothing claims is the usage block");

    let bad = both("unknown-and-bad", &["cherry-pick", "--no-such-option", "does-not-exist"]);
    assert_eq!(bad.status, Some(128), "a bad revision is diagnosed first");
    assert_eq!(bad.stderr, "fatal: bad revision 'does-not-exist'\n");

    // With a verb, `setup_revisions()` never runs, so even a pseudo-revision is
    // left over and trips the usage check.
    let with_verb = both("all-and-verb", &["cherry-pick", "--all", "--quit"]);
    assert_eq!(with_verb.status, Some(129));
}

/// `revert` shares `setup_revisions()` with `cherry-pick` and differs only in
/// the replay order, so the pseudo-revisions and `^-` reach it identically.
#[test]
fn revert_reaches_the_same_operand_grammar() {
    let not = both("revert-not", &["revert", "--no-edit", "main~1", "--not", "main~3"]);
    assert_eq!(
        not.subjects[..2],
        ["Revert \"main1\"", "Revert \"main2\""],
        "a reverted walk backs out newest first, so `main2` is undone before `main1`"
    );

    let caret = both("revert-caret-dash", &["revert", "--no-edit", "-n", "main^^-"]);
    assert_eq!(caret.status, Some(0), "revert main^^-: {}", caret.stderr);
}
