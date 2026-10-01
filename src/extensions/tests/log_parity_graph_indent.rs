//! `git log --graph` indents visual roots (git 2.56).
//!
//! A commit none of whose parents the graph draws — a root, or one whose parents
//! were filtered — used to sit in the first column straight above whatever
//! unrelated commit the walk printed next, reading as its child. 2.56 indents
//! such a "visual root" (graph.c:930-981, 1350-1366): one lane further for each
//! visual root in a row, wrapping after four, the first of a run left in place
//! when another follows it, a ` \` row (`graph_output_pre_root_line()`,
//! graph.c:1738-1760) joining it to the column that led to it, and no indent at
//! all for the last commit or for a non-first parent a merge already reaches.
//! `--[no-]graph-indent` and `log.graphIndent` (read when `--graph` is parsed,
//! graph.c:420-443) turn it on and off; either option without `--graph` dies
//! (revision.c:3241-3242).
//!
//! The history is a set of unrelated empty-tree branches built by the binary
//! under test; every expectation was measured from stock git 2.56.0 on the same
//! history. Trailing spaces git pads rows with are written as `·`.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    tick: std::cell::Cell<u64>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Branches `a` (a1, a2), `b` (b1), `c` (c1, c2), `d`, `e`, `f` (one commit
    /// each), `p1`, `p2`, `p3`, and `octo`, an octopus merge of the last three.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-log-graph-indent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let f = Fixture { root, work, tick: std::cell::Cell::new(0) };
        f.ok(&["init", "-q", "-b", "main", "."]);
        for (branch, commits) in [
            ("a", &["a1", "a2"][..]),
            ("b", &["b1"]),
            ("c", &["c1", "c2"]),
            ("d", &["d1"]),
            ("e", &["e1"]),
            ("f", &["f1"]),
            ("p1", &["p1"]),
            ("p2", &["p2"]),
            ("p3", &["p3"]),
        ] {
            f.ok(&["checkout", "-q", "--orphan", branch]);
            for msg in commits {
                f.advance();
                f.ok(&["commit", "-q", "--allow-empty", "-m", msg]);
            }
        }
        let tree = f.ok(&["write-tree"]);
        f.advance();
        let octo =
            f.ok(&["commit-tree", tree.trim(), "-p", "p1", "-p", "p2", "-p", "p3", "-m", "octo"]);
        f.ok(&["update-ref", "refs/heads/octo", octo.trim()]);
        f
    }

    /// Move the clock a minute on, so each commit sorts after the one before.
    fn advance(&self) {
        self.tick.set(self.tick.get() + 1);
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let date = format!("{} +0000", 1_700_000_000 + self.tick.get() * 60);
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
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

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "git {args:?}: {err}");
        out
    }
}

fn rows(lines: &[&str]) -> String {
    let mut s = lines.join("\n").replace('·', " ");
    s.push('\n');
    s
}

#[test]
fn a_root_above_an_unrelated_commit_is_indented() {
    let f = Fixture::new("root");
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s", "a", "b"]),
        rows(&["  * b1", "* a2", "* a1"])
    );
    // The last commit is never indented: nothing follows it to be confused with.
    assert_eq!(f.ok(&["log", "--graph", "--format=%s", "-1", "b"]), rows(&["* b1"]));
    // `--max-count` makes the last commit it lets through the last one.
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s", "--max-count=2", "d", "e", "f"]),
        rows(&["  * f1", "* e1"])
    );
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s", "--left-right", "a...c"]),
        rows(&["> c2", " \\·", "  > c1", "< a2", "< a1"])
    );
}

#[test]
fn a_root_reached_through_a_column_gets_a_joining_row() {
    let f = Fixture::new("preroot");
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s", "a", "c"]),
        rows(&["* c2", " \\·", "  * c1", "* a2", "* a1"])
    );
    // The indentation carries through the commit's message lines.
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s%nbody", "a", "c"]),
        rows(&["* c2", "| body", " \\·", "  * c1", "    body", "* a2", "| body", "* a1", "  body"])
    );
    // `%<|(N)` counts the indentation as graph width (log-tree.c:801, 888).
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%<|(12)%s|", "a", "c"]),
        rows(&["* c2        |", " \\·", "  * c1      |", "* a2        |", "* a1        |"])
    );
    assert_eq!(
        f.ok(&["rev-list", "--graph", "--format=%s", "a", "c"]),
        rows(&[
            "* commit d789b5fe5c9ddd72ed4e5078b54e539dcc447c8f",
            "| c2",
            " \\·",
            "  * commit 416a80da9a42a37567cb6460fe31e7f8fc5392b0",
            "    c1",
            "* commit a9cfd8e4f37a6072387d1318ac862ae002538258",
            "| a2",
            "* commit 9938693b6de360ed07d715e4856ee68bd86525b3",
            "  a1",
        ])
    );
}

#[test]
fn adjacent_roots_cascade() {
    let f = Fixture::new("cascade");
    // The first root of a run stays put; each later one moves a lane right.
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s", "a", "d", "e", "f"]),
        rows(&["* f1", "  * e1", "    * d1", "* a2", "* a1"])
    );
    // A run that ends the output leaves its last root unindented.
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s", "d", "e", "f"]),
        rows(&["* f1", "  * e1", "* d1"])
    );
}

#[test]
fn merge_parents_are_not_indented_but_the_first_parent_is() {
    let f = Fixture::new("octo");
    // `p2` and `p3` hang off the octopus by their own edges; `p1` inherited the
    // merge's column, so it is a visual root joined by a ` \` row.
    assert_eq!(
        f.ok(&["log", "--graph", "--format=%s", "octo", "b"]),
        rows(&["*-.   octo", "|\\ \\··", "| | * p3", "| * p2", " \\·", "  * p1", "* b1"])
    );
}

#[test]
fn indentation_switches() {
    let f = Fixture::new("switch");
    let flat = rows(&["* f1", "* e1", "* d1", "* a2", "* a1"]);
    let indented = rows(&["* f1", "  * e1", "    * d1", "* a2", "* a1"]);
    let log = |pre: &[&str], post: &[&str]| {
        let mut argv: Vec<&str> = pre.to_vec();
        argv.push("log");
        argv.extend_from_slice(post);
        argv.extend_from_slice(&["--format=%s", "a", "d", "e", "f"]);
        f.ok(&argv)
    };
    assert_eq!(log(&[], &["--graph", "--no-graph-indent"]), flat);
    assert_eq!(log(&["-c", "log.graphIndent=false"], &["--graph"]), flat);
    assert_eq!(log(&["-c", "log.graphIndent=false"], &["--graph", "--graph-indent"]), indented);
    // The config is read when `--graph` is parsed, so it overrides an earlier
    // `--no-graph-indent`.
    assert_eq!(log(&["-c", "log.graphIndent=true"], &["--no-graph-indent", "--graph"]), indented);
    assert_eq!(
        f.ok(&["rev-list", "--graph", "--no-graph-indent", "a", "d", "e", "f"]),
        rows(&[
            "* da74359d8e72517a5b8e4db9d8362d09e896c7d6",
            "* cd65370636a3b0407a1d4e4e6c0c3b5671f00d17",
            "* 6e8cf1cdcd5717cc56cef5fe4316be0f4a146ce7",
            "* a9cfd8e4f37a6072387d1318ac862ae002538258",
            "* 9938693b6de360ed07d715e4856ee68bd86525b3",
        ])
    );
}

#[test]
fn indentation_options_need_graph_and_a_boolean() {
    let f = Fixture::new("errors");
    let need = "fatal: the option '--[no-]graph-indent' requires '--graph'\n";
    for argv in [
        &["log", "--graph-indent", "a"][..],
        &["log", "--no-graph-indent", "a"],
        &["log", "--graph", "--graph-indent", "--no-graph", "a"],
        &["rev-list", "--graph-indent", "a"],
    ] {
        let (out, err, code) = f.run(argv);
        assert_eq!((out.as_str(), err.as_str(), code), ("", need, 128), "{argv:?}");
    }
    let (out, err, code) = f.run(&["-c", "log.graphIndent=nope", "log", "--graph", "a"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: bad boolean config value 'nope' for 'log.graphIndent'\n", 128)
    );
    // Without `--graph` the key is never read.
    let (_, err, code) = f.run(&["-c", "log.graphIndent=nope", "log", "--format=%s", "-1", "a"]);
    assert_eq!((err.as_str(), code), ("", 0));
}
