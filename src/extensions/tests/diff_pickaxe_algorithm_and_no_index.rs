//! Five gaps in the `diff` family, each pinned against stock git 2.55.0.
//!
//! 1. **One ambiguity warning per operand.** `get_oid_basic()` (`object-name.c`)
//!    warns when a 40-hex operand is also a refname, and it warns *once* because
//!    `repo_get_oid()` reaches it once per operand. `cmd_diff()` then sorts the
//!    pending objects into trees and blobs by reading `entry->item` — the object
//!    `setup_revisions()` already attached (builtin/diff.c:572-604) — and never
//!    re-resolves the name. Resolving it a second time made every one of these
//!    operands warn twice.
//!
//! 2. **`--no-index` takes the algorithm options.** `diff_no_index()` builds its
//!    option table with `add_diff_options(no_index_options, &revs->diffopt)`
//!    (diff-no-index.c:372) — the whole `diff_opts` table — so `--diff-algorithm`
//!    in both spellings, `--minimal`, `--patience` and `--histogram` all reach it.
//!    Its default is git's zero-valued `diff_algorithm` (diff.c:78), i.e. Myers.
//!
//! 3. **`-S`/`-G` filter `git diff`.** `diffcore_pickaxe()` runs inside
//!    `diffcore_std()` for every diff verb. Both value spellings are taken —
//!    `OPT_PICKAXE_S`/`OPT_PICKAXE_G` (diff.c:6270-6275) are `OPT_CALLBACK_F`
//!    without `PARSE_OPT_OPTARG`, so a bare `-S` consumes the next argv entry —
//!    and an empty pattern is the callback's own `error()` at 129.
//!
//! 4. **The separated `--diff-algorithm <value>`** is the same declaration as the
//!    glued one, so `diff-tree` must consume its value rather than leave it to be
//!    read as a revision.
//!
//! 5. **`cmd_diff()`'s implicit `--no-index`** (builtin/diff.c:466-476): exactly
//!    two operands after the leading options, at least one of them naming
//!    somewhere outside the worktree, and git switches to the no-index comparison
//!    — which is why `git diff .. --` prints the no-index usage block at 129.
//!
//! The algorithm cases use inputs that are *proven* to separate the algorithms
//! under stock: each `ALGO_FIXTURE` entry records, alongside its two files, the
//! partition stock 2.55.0 produces over `{myers, minimal, patience, histogram}`.
//! An input on which all four agree would pass no matter which algorithm ran, so
//! [`algorithm_fixtures_discriminate`] asserts the partition itself first.
//!
//! Expectations are stock git 2.55.0's, captured with the parity harness's
//! environment (fixed identity and date, no global or system config, `LC_ALL=C`,
//! `TZ=UTC`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `object_name_msg` (`object-name.c`), which follows the `warning:` line unless
/// `advice.objectNameWarning` is off. Present twice in the "before" output and
/// once in stock's, so counting the `warning:` lines is what the tests assert.
const AMBIGUOUS_WARNING_PREFIX: &str = "warning: refname '";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-diff-pickaxe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.ok(&["init", "-q", "-b", "main", "."]);
        f
    }

    /// Three commits over two files, shaped so the pickaxe cases have all four
    /// answers available: `two.txt` duplicates a line (an occurrence *count*
    /// change, which `-S` sees), `one.txt`'s third commit only reorders its lines
    /// (a change `-G` sees and `-S` does not), and `three.txt` is an addition.
    fn with_pickaxe_history(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.write("one.txt", "alpha\nbeta\n");
        f.write("two.txt", "keep\n");
        f.ok(&["add", "."]);
        f.commit("c1");
        f.write("one.txt", "alpha\nbeta\ngamma\n");
        f.write("two.txt", "keep\nkeep\n");
        f.write("three.txt", "brand new\n");
        f.ok(&["add", "."]);
        f.commit("c2");
        f.write("one.txt", "gamma\nalpha\nbeta\n");
        f.ok(&["add", "."]);
        f.commit("c3");
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.repo.join(name), body).unwrap();
    }

    fn commit(&self, msg: &str) {
        self.ok(&["commit", "-q", "-m", msg]);
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.repo, args)
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "@1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "@1112911993 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn ok(&self, args: &[&str]) {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "setup `git {args:?}` failed: {out}{err}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}` exited {code}: {err}");
        out
    }
}

fn warning_lines(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.starts_with(AMBIGUOUS_WARNING_PREFIX)).count()
}

// ---------------------------------------------------------------------------
// 1. one ambiguity warning per operand
// ---------------------------------------------------------------------------

/// Every `git diff` shape that carries a 40-hex operand which is *also* a
/// refname, against a repository that holds `refs/heads/<40-hex-of-HEAD>`.
///
/// Stock warns once for each time `get_oid_basic()` is reached: once for a single
/// operand, and once per *endpoint* for a range — `handle_dotdot_1()` resolves
/// both before either is looked up. `HEAD` is not 40 hex and contributes nothing,
/// so every case here is exactly one warning.
///
/// `diff-index` and `diff-tree` are included as controls: they were already
/// right, and a fix that reached into the shared gate rather than into `cmd_diff`'s
/// second resolution would have silenced them too.
#[test]
fn a_forty_hex_operand_warns_once_per_resolution() {
    let f = Fixture::new("ambig-once");
    f.write("a.txt", "a\n");
    f.ok(&["add", "a.txt"]);
    f.commit("c1");
    f.write("b.txt", "b\n");
    f.ok(&["add", "b.txt"]);
    f.commit("c2");
    f.write("c.txt", "cc\ndd\n");
    f.ok(&["add", "c.txt"]);
    f.commit("c3");
    let hex = f.stdout(&["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(hex.len(), 40, "fixture needs a 40-hex id");
    f.ok(&["update-ref", &format!("refs/heads/{hex}"), "HEAD~1"]);
    // An unstaged edit, so the worktree shapes have work to do.
    f.write("c.txt", "cc\ndd\nee\n");

    let range = format!("{hex}..HEAD");
    let symmetric = format!("{hex}...HEAD");
    let peeled = format!("{hex}^{{tree}}");
    for args in [
        vec!["diff", hex.as_str()],
        vec!["diff", hex.as_str(), "--"],
        vec!["diff", hex.as_str(), "HEAD"],
        vec!["diff", hex.as_str(), "--", "c.txt"],
        vec!["diff", "--cached", hex.as_str()],
        vec!["diff", range.as_str()],
        vec!["diff", symmetric.as_str()],
        vec!["diff", peeled.as_str()],
        vec!["diff", "--stat", hex.as_str()],
        vec!["diff", "--name-only", hex.as_str()],
        // Controls: the two plumbing verbs resolve their operand once and always did.
        vec!["diff-index", hex.as_str()],
        vec!["diff-tree", hex.as_str(), "HEAD"],
    ] {
        let (_, err, _) = f.run(&args);
        assert_eq!(warning_lines(&err), 1, "expected exactly one warning for {args:?}:\n{err}");
    }
}

/// `core.warnAmbiguousRefs=false` is the fourth gate in `get_oid_basic()`'s
/// condition, so it must silence the warning outright — a fix that merely
/// de-duplicated the message would leave one behind here.
#[test]
fn the_ambiguity_warning_still_answers_to_its_config_gate() {
    let f = Fixture::new("ambig-config");
    f.write("a.txt", "a\n");
    f.ok(&["add", "a.txt"]);
    f.commit("c1");
    f.write("b.txt", "b\n");
    f.ok(&["add", "b.txt"]);
    f.commit("c2");
    let hex = f.stdout(&["rev-parse", "HEAD"]).trim().to_owned();
    f.ok(&["update-ref", &format!("refs/heads/{hex}"), "HEAD~1"]);

    let (_, err, _) = f.run(&["-c", "core.warnAmbiguousRefs=false", "diff", &hex]);
    assert_eq!(warning_lines(&err), 0, "config gate ignored:\n{err}");
    let (_, err, _) = f.run(&["diff", &hex]);
    assert_eq!(warning_lines(&err), 1, "warning lost with the config unset:\n{err}");
}

// ---------------------------------------------------------------------------
// 2 & 4. algorithm selection
// ---------------------------------------------------------------------------

/// Inputs on which stock git 2.55.0 does **not** produce one patch for all four
/// algorithms, with the partition it does produce recorded alongside.
///
/// Each entry is `(name, a, b, partition)`, where `a` and `b` are spelled one
/// character per line and `partition` groups the four algorithm names by the
/// output they produce, each group sorted and the groups themselves sorted. Found
/// by exhaustive search over a three-letter alphabet; between them the five
/// entries separate all six unordered pairs.
const ALGO_FIXTURES: &[(&str, &str, &str, &[&[&str]])] = &[
    ("myers-vs-minimal", "caaaaabc", "bbbb", &[&["histogram", "minimal"], &["myers", "patience"]]),
    ("vs-patience", "baacbab", "bbabc", &[&["histogram", "patience"], &["minimal", "myers"]]),
    (
        "vs-histogram",
        "cbaaccbccccb",
        "abcccccabaaababbaa",
        &[&["histogram"], &["minimal", "myers", "patience"]],
    ),
    (
        "three-way-a",
        "ccccbca",
        "bacbbcccccb",
        &[&["histogram"], &["minimal", "myers"], &["patience"]],
    ),
    (
        "three-way-b",
        "bcbbabbcbccccbbcbb",
        "cbbcbabbbbb",
        &[&["histogram"], &["minimal", "myers"], &["patience"]],
    ),
];

const ALGORITHMS: [&str; 4] = ["histogram", "minimal", "myers", "patience"];

fn spread(s: &str) -> String {
    s.chars().map(|c| format!("{c}\n")).collect()
}

/// Group the algorithm names by the patch they produce, in the same normal form
/// [`ALGO_FIXTURES`] records: groups sorted internally and against each other.
fn partition(mut patch_of: impl FnMut(&str) -> String) -> Vec<Vec<String>> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for algo in ALGORITHMS {
        let patch = patch_of(algo);
        match groups.iter_mut().find(|(p, _)| *p == patch) {
            Some((_, names)) => names.push(algo.to_owned()),
            None => groups.push((patch, vec![algo.to_owned()])),
        }
    }
    let mut out: Vec<Vec<String>> = groups
        .into_iter()
        .map(|(_, mut names)| {
            names.sort();
            names
        })
        .collect();
    out.sort();
    out
}

/// The `index <old>..<new>` line carries blob ids, which say nothing about which
/// algorithm ran; dropping it keeps the comparison to the hunks.
fn hunks_only(patch: &str) -> String {
    patch.lines().filter(|l| !l.starts_with("index ")).map(|l| format!("{l}\n")).collect()
}

/// The premise every algorithm assertion below rests on: these inputs really do
/// tell the four algorithms apart, and in the exact grouping recorded.
///
/// Without this an "all four agree" fixture would let a port that ignores the
/// option entirely pass every case that follows.
#[test]
fn algorithm_fixtures_discriminate() {
    let f = Fixture::new("algo-discriminates");
    for (name, a, b, expected) in ALGO_FIXTURES {
        f.write("a", &spread(a));
        f.write("b", &spread(b));
        let got = partition(|algo| {
            let (out, err, code) = f.run(&[
                "diff",
                "--no-index",
                "-U3",
                &format!("--diff-algorithm={algo}"),
                "a",
                "b",
            ]);
            assert_eq!(code, 1, "fixture {name}/{algo} produced no difference: {err}");
            hunks_only(&out)
        });
        assert_eq!(got, *expected, "fixture {name} does not partition as recorded");
    }
}

/// Every spelling of the algorithm options, on `--no-index` — which had refused
/// all of them — checked against the *specific* algorithm's output rather than
/// merely against "some patch".
#[test]
fn no_index_honours_every_algorithm_spelling() {
    let f = Fixture::new("no-index-algos");
    for (name, a, b, _) in ALGO_FIXTURES {
        f.write("a", &spread(a));
        f.write("b", &spread(b));
        let reference = |algo: &str| -> String {
            let (out, _, code) =
                f.run(&["diff", "--no-index", "-U3", &format!("--diff-algorithm={algo}"), "a", "b"]);
            assert_eq!(code, 1, "{name}/{algo}");
            hunks_only(&out)
        };
        let cases: [(&[&str], &str); 9] = [
            (&["--diff-algorithm", "myers"], "myers"),
            (&["--diff-algorithm", "minimal"], "minimal"),
            (&["--diff-algorithm", "patience"], "patience"),
            (&["--diff-algorithm", "histogram"], "histogram"),
            (&["--minimal"], "minimal"),
            (&["--patience"], "patience"),
            (&["--histogram"], "histogram"),
            // `parse_algorithm_value()` uses `strcasecmp`, and `default` is its
            // spelling for Myers.
            (&["--diff-algorithm=HISTOGRAM"], "histogram"),
            (&["--diff-algorithm=default"], "myers"),
        ];
        for (flags, want) in cases {
            let mut args = vec!["diff", "--no-index", "-U3"];
            args.extend_from_slice(flags);
            args.extend_from_slice(&["a", "b"]);
            let (out, err, code) = f.run(&args);
            assert_eq!(code, 1, "{name} {flags:?}: {err}");
            assert_eq!(err, "", "{name} {flags:?}");
            assert_eq!(hunks_only(&out), reference(want), "{name} {flags:?} is not {want}");
        }
    }
}

/// `--no-index`'s own default is Myers: `static long diff_algorithm;` (diff.c:78)
/// is zero, and `diff_setup()` ORs that into `options->xdl_opts`.
///
/// This is asserted on the three fixtures where Myers and histogram *differ*, so
/// a port that had hardcoded histogram cannot pass it.
#[test]
fn no_index_defaults_to_myers() {
    let f = Fixture::new("no-index-default");
    for (name, a, b, _) in ALGO_FIXTURES {
        f.write("a", &spread(a));
        f.write("b", &spread(b));
        let with = |algo: &str| -> String {
            hunks_only(&f.run(&["diff", "--no-index", "-U3", &format!("--diff-algorithm={algo}"), "a", "b"]).0)
        };
        let (myers, histogram) = (with("myers"), with("histogram"));
        assert_ne!(myers, histogram, "fixture {name} cannot tell the default apart");
        let bare = hunks_only(&f.run(&["diff", "--no-index", "-U3", "a", "b"]).0);
        assert_eq!(bare, myers, "fixture {name}: the default is not myers");
    }
}

/// `git_diff_ui_config()` reads `diff.algorithm` before `cmd_diff()` dispatches,
/// so the key reaches a `--no-index` comparison run from inside a repository —
/// and a flag still beats it, because `diff_opt_diff_algorithm()` writes
/// `options->xdl_opts` after `diff_setup()` copied the configured default in.
#[test]
fn no_index_reads_diff_algorithm_from_config() {
    let f = Fixture::new("no-index-config");
    let (name, a, b, _) = ALGO_FIXTURES[2];
    f.write("a", &spread(a));
    f.write("b", &spread(b));
    let with = |algo: &str| -> String {
        hunks_only(&f.run(&["diff", "--no-index", "-U3", &format!("--diff-algorithm={algo}"), "a", "b"]).0)
    };
    let (myers, histogram) = (with("myers"), with("histogram"));
    assert_ne!(myers, histogram, "fixture {name} cannot tell the config apart");

    f.ok(&["config", "diff.algorithm", "histogram"]);
    assert_eq!(
        hunks_only(&f.run(&["diff", "--no-index", "-U3", "a", "b"]).0),
        histogram,
        "diff.algorithm did not reach --no-index"
    );
    assert_eq!(
        hunks_only(&f.run(&["diff", "--no-index", "-U3", "--diff-algorithm=myers", "a", "b"]).0),
        myers,
        "the flag did not beat diff.algorithm"
    );
}

/// `diff_opt_diff_algorithm()`'s `error()` and parse-options' missing-value
/// message, both of which `--no-index` had answered with its own bail.
#[test]
fn no_index_rejects_a_bad_algorithm_value_the_way_git_does() {
    let f = Fixture::new("no-index-bad-algo");
    f.write("a", "x\n");
    f.write("b", "y\n");

    for args in [
        vec!["diff", "--no-index", "--diff-algorithm=bogus", "a", "b"],
        vec!["diff", "--no-index", "--diff-algorithm", "bogus", "a", "b"],
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!(
            err,
            "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"\n",
            "{args:?}"
        );
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 129, "{args:?}");
    }

    let (_, err, code) = f.run(&["diff", "--no-index", "a", "b", "--diff-algorithm"]);
    assert_eq!(err, "error: option `diff-algorithm' requires a value\n");
    assert_eq!(code, 129);
}

/// The separated `--diff-algorithm <value>` on `diff-tree`, which had left the
/// value to be read as a revision. Asserted against the glued spelling, which was
/// already right, so the two forms cannot drift apart.
#[test]
fn diff_tree_takes_the_separated_diff_algorithm() {
    let f = Fixture::new("diff-tree-algo");
    // A body long enough for the algorithms to have something to disagree about,
    // though the assertion here is form-vs-form rather than algorithm-vs-algorithm.
    f.write("f.txt", &spread("cbaaccbccccb"));
    f.ok(&["add", "f.txt"]);
    f.commit("c1");
    f.write("f.txt", &spread("abcccccabaaababbaa"));
    f.ok(&["add", "f.txt"]);
    f.commit("c2");

    for algo in ALGORITHMS {
        let glued = f.run(&["diff-tree", "-p", &format!("--diff-algorithm={algo}"), "HEAD~1", "HEAD"]);
        let separated = f.run(&["diff-tree", "-p", "--diff-algorithm", algo, "HEAD~1", "HEAD"]);
        assert_eq!(glued.2, 0, "glued {algo}: {}", glued.1);
        assert_eq!(separated.2, 0, "separated {algo}: {}", separated.1);
        assert_eq!(separated.1, "", "separated {algo}");
        assert_eq!(separated.0, glued.0, "the two spellings of {algo} disagree");
        assert!(!separated.0.is_empty(), "separated {algo} produced nothing");
    }

    // And it really is the algorithm being selected, not merely a consumed value:
    // this fixture separates histogram from myers under stock.
    let hist = f.run(&["diff-tree", "-p", "--diff-algorithm", "histogram", "HEAD~1", "HEAD"]).0;
    let myers = f.run(&["diff-tree", "-p", "--diff-algorithm", "myers", "HEAD~1", "HEAD"]).0;
    assert_ne!(hunks_only(&hist), hunks_only(&myers), "the option had no effect");
}

// ---------------------------------------------------------------------------
// 3. -S / -G
// ---------------------------------------------------------------------------

/// `-S` counts occurrences and `-G` greps the changed lines — the distinction the
/// manual draws, and the one a port that implemented only one of them would blur.
///
/// `HEAD~1..HEAD` reorders `one.txt` without changing any line's count, so `-S`
/// must find nothing there and `-G` must find it.
#[test]
fn diff_s_and_g_filter_the_queue() {
    let f = Fixture::with_pickaxe_history("pickaxe-kinds");

    // A pure reordering: -G sees it, -S does not.
    assert_eq!(f.stdout(&["diff", "--name-only", "-S", "gamma", "HEAD~1", "HEAD"]), "");
    assert_eq!(f.stdout(&["diff", "--name-only", "-G", "gamma", "HEAD~1", "HEAD"]), "one.txt\n");
    // An occurrence-count change: both see it.
    assert_eq!(f.stdout(&["diff", "--name-only", "-S", "keep", "HEAD~2", "HEAD~1"]), "two.txt\n");
    assert_eq!(f.stdout(&["diff", "--name-only", "-G", "keep", "HEAD~2", "HEAD~1"]), "two.txt\n");
    // A pattern in nothing at all keeps nothing.
    assert_eq!(f.stdout(&["diff", "--name-only", "-S", "nosuchtext", "HEAD~2", "HEAD"]), "");
    assert_eq!(f.stdout(&["diff", "--name-only", "-G", "nosuchtext", "HEAD~2", "HEAD"]), "");
}

/// Both spellings of the value reach the same callback: `OPT_PICKAXE_S` has no
/// `PARSE_OPT_OPTARG`, so a bare `-S` takes the next argv entry. Leaving it behind
/// made the pattern be read as a revision.
#[test]
fn the_pickaxe_value_may_be_glued_or_separated() {
    let f = Fixture::with_pickaxe_history("pickaxe-spelling");

    for (glued, separated) in [
        (vec!["-Sgamma"], vec!["-S", "gamma"]),
        (vec!["-Ggamma"], vec!["-G", "gamma"]),
        (vec!["-Skeep"], vec!["-S", "keep"]),
        (vec!["-Gkeep"], vec!["-G", "keep"]),
    ] {
        for verb in [
            vec!["diff", "--name-only"],
            vec!["diff-index", "--name-only"],
            vec!["diff-tree", "-r", "--name-only"],
        ] {
            let tail: &[&str] = if verb[0] == "diff-tree" {
                &["HEAD~2", "HEAD"]
            } else {
                &["HEAD~2"]
            };
            let build = |flags: &[&str]| -> Vec<String> {
                verb.iter()
                    .chain(flags.iter())
                    .chain(tail.iter())
                    .map(|s| (*s).to_owned())
                    .collect()
            };
            let (a, b) = (build(&glued), build(&separated));
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            let b: Vec<&str> = b.iter().map(String::as_str).collect();
            let (ao, ae, ac) = f.run(&a);
            let (bo, be, bc) = f.run(&b);
            assert_eq!(ac, 0, "{a:?}: {ae}");
            assert_eq!(bc, 0, "{b:?}: {be}");
            assert_eq!(bo, ao, "{b:?} disagrees with {a:?}");
        }
    }
}

/// `--pickaxe-regex` promotes `-S` from a literal search to a `regcomp`, and it
/// counts wherever it appears on the line — `diffcore_pickaxe()` reads
/// `o->pickaxe_opts` once, after the whole scan.
#[test]
fn pickaxe_regex_promotes_s_and_may_follow_it() {
    let f = Fixture::with_pickaxe_history("pickaxe-regex");

    // `ke+p` matches `keep` as a regex and nothing as a literal.
    assert_eq!(f.stdout(&["diff", "--name-only", "-Ske+p", "HEAD~2", "HEAD~1"]), "");
    assert_eq!(
        f.stdout(&["diff", "--name-only", "--pickaxe-regex", "-Ske+p", "HEAD~2", "HEAD~1"]),
        "two.txt\n"
    );
    assert_eq!(
        f.stdout(&["diff", "--name-only", "-Ske+p", "--pickaxe-regex", "HEAD~2", "HEAD~1"]),
        "two.txt\n",
        "--pickaxe-regex after the -S it promotes"
    );
}

/// `--pickaxe-all` keeps the whole queue once one pair matched, which is the only
/// thing it does — with no pickaxe kind set it changes nothing.
#[test]
fn pickaxe_all_keeps_the_whole_queue() {
    let f = Fixture::with_pickaxe_history("pickaxe-all");

    let filtered = f.stdout(&["diff", "--name-only", "-Skeep", "HEAD~2", "HEAD"]);
    let all = f.stdout(&["diff", "--name-only", "-Skeep", "--pickaxe-all", "HEAD~2", "HEAD"]);
    assert_eq!(filtered, "two.txt\n");
    assert_eq!(all, "one.txt\nthree.txt\ntwo.txt\n");
    // The unfiltered queue, for the "keeps *the whole* queue" half of the claim.
    assert_eq!(f.stdout(&["diff", "--name-only", "HEAD~2", "HEAD"]), all);
}

/// `diff_opt_pickaxe_string()`'s one refusal (diff.c:5901): an empty pattern is
/// `error()` from the callback, so exit 129 at the flag's own argv position — and
/// the kind bit is set *before* it, which is why `-S '' -Gx` reports the empty
/// argument rather than `diff_setup_done()`'s two-kind conflict.
#[test]
fn an_empty_pickaxe_pattern_is_refused() {
    let f = Fixture::with_pickaxe_history("pickaxe-empty");

    for (args, want) in [
        (vec!["diff", "-S", "", "HEAD"], "-S"),
        (vec!["diff", "-G", "", "HEAD"], "-G"),
        (vec!["diff", "-S", "", "-Gx", "HEAD"], "-S"),
        (vec!["diff-index", "-S", "", "HEAD"], "-S"),
        (vec!["diff-files", "-S", ""], "-S"),
        (vec!["diff-tree", "-S", "", "HEAD~1", "HEAD"], "-S"),
        (vec!["diff-tree", "-G", "", "HEAD~1", "HEAD"], "-G"),
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!(err, format!("error: {want} requires a non-empty argument\n"), "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 129, "{args:?}");
    }
}

/// `diff_setup_done()`'s two `HAS_MULTI_BITS` `die()`s still fire once `-S`/`-G`
/// are rendered rather than refused — they are `die()`s, exit 128, and they beat
/// an unknown option while losing to a bad positional.
#[test]
fn the_pickaxe_conflicts_survive() {
    let f = Fixture::with_pickaxe_history("pickaxe-conflict");

    let (_, err, code) = f.run(&["diff", "-Sa", "-Gb", "HEAD"]);
    assert_eq!(err, "fatal: options '-G', '-S', and '--find-object' cannot be used together\n");
    assert_eq!(code, 128);

    let head = f.stdout(&["rev-parse", "HEAD"]).trim().to_owned();
    let (_, err, code) = f.run(&["diff", "--pickaxe-all", &format!("--find-object={head}"), "HEAD"]);
    assert_eq!(
        err,
        "fatal: options '--pickaxe-all' and '--find-object' cannot be used together, \
         use '--pickaxe-all' with '-G' and '-S'\n"
    );
    assert_eq!(code, 128);

    // The same on `diff-tree`, which raised only the first of the two.
    let (_, err, code) =
        f.run(&["diff-tree", "--pickaxe-all", &format!("--find-object={head}"), "HEAD~1", "HEAD"]);
    assert_eq!(
        err,
        "fatal: options '--pickaxe-all' and '--find-object' cannot be used together, \
         use '--pickaxe-all' with '-G' and '-S'\n"
    );
    assert_eq!(code, 128);
}

/// `--find-object` is `DIFF_PICKAXE_KIND_OBJFIND`, which `pickaxe_match()` tests
/// before it looks at a needle at all — so it must keep working now that a needle
/// can also be present.
#[test]
fn find_object_still_outranks_a_needle() {
    let f = Fixture::with_pickaxe_history("pickaxe-objfind");
    let blob = f.stdout(&["rev-parse", "HEAD~1:two.txt"]).trim().to_owned();

    assert_eq!(
        f.stdout(&["diff", "--name-only", &format!("--find-object={blob}"), "HEAD~2", "HEAD"]),
        "two.txt\n"
    );
}

// ---------------------------------------------------------------------------
// 5. implicit --no-index
// ---------------------------------------------------------------------------

/// `cmd_diff()`'s implicit `--no-index` arm, which fires *inside* a repository:
/// exactly two argv entries after the leading options, at least one of them
/// naming somewhere outside the worktree.
///
/// `git diff .. --` is the shape that made this visible: `..` escapes the
/// worktree and two entries follow the options, so git hands the pair to
/// `diff_no_index()` — which then finds only one operand left and prints the
/// no-index usage block behind its `warning: Not a git repository` line.
///
/// The three negative cases pin the rule's edges rather than the outcome alone: a
/// third entry takes the count out of range, a single one likewise, and `...` is
/// an ordinary (if unlikely) directory name that stays inside the worktree.
#[test]
fn two_operands_with_one_outside_the_worktree_go_no_index() {
    let f = Fixture::new("implicit-no-index");
    f.write("a.txt", "a\n");
    f.ok(&["add", "a.txt"]);
    f.commit("c1");
    f.write("a.txt", "a\nb\n");

    let (out, err, code) = f.run(&["diff", "..", "--"]);
    assert_eq!(code, 129, "stderr:\n{err}");
    assert_eq!(out, "");
    assert!(
        err.starts_with(
            "warning: Not a git repository. Use --no-index to compare two paths outside a working tree\n"
        ),
        "{err}"
    );
    assert!(
        err.contains("usage: git diff --no-index [<options>] <path> <path> [<pathspec>...]"),
        "{err}"
    );

    // Not two entries: stays an ordinary diff, where `handle_revision_arg_1()`
    // refuses a bare `..` ahead of `handle_dotdot()` and the pathspec layer then
    // rejects it for leaving the repository.
    let (_, err, code) = f.run(&["diff", ".."]);
    assert_eq!(code, 128, "{err}");
    assert!(err.starts_with("fatal: ..: '..' is outside repository at '"), "{err}");

    // Three entries: also an ordinary diff, and this one succeeds.
    let (_, err, code) = f.run(&["diff", "..", "--", "a.txt"]);
    assert_eq!(code, 0, "{err}");

    // Two entries, both inside: `...` is a directory name, not an escape.
    let (_, err, code) = f.run(&["diff", "...", "--"]);
    assert_eq!(code, 0, "{err}");

    // Two revisions are two entries and both resolve inside; the rule must not
    // steal an ordinary revision pair.
    let (_, err, code) = f.run(&["diff", "HEAD", "HEAD"]);
    assert_eq!(code, 0, "{err}");
}

/// The same rule from a subdirectory, where the prefix is what decides whether a
/// relative operand escapes: `prefix_path_gently()` resolves against it before
/// normalizing, so `..` from `sub/` lands on the worktree root and stays inside.
#[test]
fn the_prefix_decides_whether_a_relative_operand_escapes() {
    let f = Fixture::new("implicit-prefix");
    std::fs::create_dir_all(f.repo.join("sub")).unwrap();
    f.write("a.txt", "a\n");
    f.write("sub/s.txt", "s\n");
    f.ok(&["add", "."]);
    f.commit("c1");
    let sub = f.repo.join("sub");

    // From `sub/`, `..` is the worktree root: inside, so this stays an ordinary
    // diff and `..` is refused as a revision by the pathspec layer.
    let (_, err_sub, code_sub) = f.run_in(&sub, &["diff", "..", "--"]);
    // From the root, `..` is above it: outside, so the no-index usage block.
    let (_, err_root, code_root) = f.run(&["diff", "..", "--"]);
    assert_ne!(
        (code_sub, err_sub.is_empty()),
        (code_root, err_root.is_empty()),
        "the prefix made no difference: sub={code_sub}/{err_sub} root={code_root}/{err_root}"
    );
    assert_eq!(code_root, 129, "{err_root}");
    assert!(err_root.contains("usage: git diff --no-index"), "{err_root}");
    assert_eq!(code_sub, 0, "from sub/, `..` is inside the worktree: {err_sub}");

    // Two levels up from `sub/` escapes again.
    let (_, err, code) = f.run_in(&sub, &["diff", "../..", "--"]);
    assert_eq!(code, 129, "{err}");
    assert!(err.contains("usage: git diff --no-index"), "{err}");
}

/// A 100%-similar *binary* rename in `--no-index` carries no body:
/// `builtin_diff()`'s binary arm stops at the header when the two sides hold the
/// same object (`if (oideq(&one->oid, &two->oid))`). The text side has always been
/// right because its body is simply empty; the binary side was printing
/// `Binary files … differ` for content that is identical.
#[test]
fn an_unchanged_binary_rename_has_no_body() {
    let f = Fixture::new("no-index-binary-rename");
    let (one, two) = (f.root.join("d1"), f.root.join("d2"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(one.join("x"), b"bin\0data\n").unwrap();
    std::fs::write(two.join("y"), b"bin\0data\n").unwrap();
    std::fs::write(one.join("t"), "text\n").unwrap();
    std::fs::write(two.join("u"), "text\n").unwrap();

    let (out, err, code) = f.run(&[
        "diff",
        "--no-index",
        one.to_str().unwrap(),
        two.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{err}");
    assert!(out.contains("similarity index 100%"), "expected two renames:\n{out}");
    assert!(!out.contains("Binary files"), "identical binary rename got a body:\n{out}");
    // Two renames and nothing else: four header lines each.
    assert_eq!(out.matches("similarity index 100%").count(), 2, "{out}");
}
