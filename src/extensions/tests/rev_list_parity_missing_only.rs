//! `git rev-list --missing-only`, new in git 2.56.
//!
//! builtin/rev-list.c (2.56.0): the pre-`setup_revisions()` scan takes the
//! option (:778-779) and dies unless that same scan saw `--missing=print` or
//! `--missing=print-info` (:782-783); `--count` and `--disk-usage` are refused
//! (:943-946); `show_commit()` and `show_object()` return before any output
//! (:263-266, :406-407); and `print_missing_object()` drops the `?` prefix
//! unless `-z` is in effect (:170-176). Every expectation below was measured
//! against stock git 2.56.0 on this exact fixture.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The blob `d/b c` holds, whose loose object the fixture deletes.
const GONE: &str = "16ac006812e54296af3122d43a85c4f5754f7018";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `one` adds `a` and `d/b c`; `two` rewrites `a`. Then `d/b c`'s blob goes.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rev-list-missing-only-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("d")).unwrap();
        let f = Fixture { root };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        std::fs::write(f.root.join("d/b c"), "b c\n").unwrap();
        f.ok(&["add", "."]);
        f.ok(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.root.join("a"), "a2\n").unwrap();
        f.ok(&["commit", "-q", "-a", "-m", "two"]);
        std::fs::remove_file(f.root.join(".git/objects").join(&GONE[..2]).join(&GONE[2..])).unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (Vec<u8>, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }

    fn ok(&self, args: &[&str]) {
        let (_, err, code) = self.run(args);
        assert_eq!(code, 0, "git {args:?}: {err}");
    }

    fn expect(&self, args: &[&str], stdout: &[u8], stderr: &str, code: i32) {
        let (out, err, rc) = self.run(args);
        assert_eq!(
            (String::from_utf8_lossy(&out).into_owned(), err.as_str(), rc),
            (String::from_utf8_lossy(stdout).into_owned(), stderr, code),
            "git {args:?}"
        );
    }
}

#[test]
fn only_the_missing_objects_are_listed_without_the_question_mark() {
    let f = Fixture::new("listing");
    // The control: without the option the whole walk is listed, `?` and all.
    let (all, _, rc) = f.run(&["rev-list", "--objects", "--missing=print", "HEAD"]);
    assert_eq!(rc, 0);
    assert!(String::from_utf8_lossy(&all).ends_with(&format!("?{GONE}\n")));
    assert_eq!(String::from_utf8_lossy(&all).lines().count(), 8);

    f.expect(&["rev-list", "--objects", "--missing=print", "--missing-only", "HEAD"], format!("{GONE}\n").as_bytes(), "", 0);
    f.expect(
        &["rev-list", "--objects", "--missing=print-info", "--missing-only", "HEAD"],
        format!("{GONE} path=\"d/b c\" type=blob\n").as_bytes(),
        "",
        0,
    );
    // `--quiet` changes nothing: the missing listing is not part of the walk output.
    f.expect(&["rev-list", "--objects", "--missing=print", "--missing-only", "--quiet", "HEAD"], format!("{GONE}\n").as_bytes(), "", 0);
}

#[test]
fn nul_termination_keeps_its_missing_field() {
    let f = Fixture::new("nul");
    f.expect(
        &["rev-list", "--objects", "--missing=print", "--missing-only", "-z", "HEAD"],
        format!("{GONE}\0missing=yes\0").as_bytes(),
        "",
        0,
    );
    f.expect(
        &["rev-list", "--objects", "--missing=print-info", "--missing-only", "-z", "HEAD"],
        format!("{GONE}\0missing=yes\0path=d/b c\0type=blob\0").as_bytes(),
        "",
        0,
    );
}

#[test]
fn commit_walk_and_boundary_print_nothing() {
    let f = Fixture::new("commits");
    f.expect(&["rev-list", "--missing=print", "--missing-only", "HEAD"], b"", "", 0);
    f.expect(&["rev-list", "--objects", "--missing=print", "--missing-only", "--boundary", "HEAD~1..HEAD"], b"", "", 0);
    // `show_edge()` is not one of the two callbacks the option gates.
    f.expect(
        &["rev-list", "--objects", "--missing=print", "--missing-only", "--objects-edge", "HEAD~1..HEAD"],
        b"-390c4bf8ccd435f0ad0e17f3932b8063c482ae00\n",
        "",
        0,
    );
    // The pre-scan reads every argument, `--`-separated paths included.
    f.expect(&["rev-list", "--objects", "--missing=print", "HEAD", "--", "--missing-only"], b"", "", 0);
}

#[test]
fn omitted_objects_still_listed() {
    let f = Fixture::new("omitted");
    f.expect(
        &["rev-list", "--objects", "--missing=print", "--missing-only", "--filter=blob:none", "--filter-print-omitted", "HEAD"],
        format!("~c1827f07e114c20547dc6a7296588870a4b5b62c\n~78981922613b2afb6025042ff6bd878ac1994e85\n~{GONE}\n").as_bytes(),
        "",
        0,
    );
}

#[test]
fn refusals() {
    let f = Fixture::new("refusals");
    let needs = "fatal: --missing-only requires --missing=print or --missing=print-info\n";
    for args in [
        &["rev-list", "--missing-only", "HEAD"][..],
        &["rev-list", "--missing-only", "--missing=allow-any", "HEAD"][..],
        // The last *recognised* action decides; `error` is one.
        &["rev-list", "--missing=print", "--missing=error", "--missing-only", "HEAD"][..],
        &["rev-list", "--missing-only", "--exclude-promisor-objects", "HEAD"][..],
        // Ahead of `setup_revisions()`, so ahead of the bad revision.
        &["rev-list", "--missing-only", "zzz"][..],
    ] {
        f.expect(args, b"", needs, 128);
    }
    // An action the parser does not know leaves `print` standing.
    f.expect(&["rev-list", "--missing=print", "--missing=bogus", "--missing-only", "HEAD"], b"", "", 0);
    f.expect(
        &["rev-list", "--objects", "--missing=print", "--missing-only", "--count", "HEAD"],
        b"",
        "fatal: options '--missing-only' and '--count' cannot be used together\n",
        128,
    );
    f.expect(
        &["rev-list", "--objects", "--missing=print", "--missing-only", "--disk-usage", "HEAD"],
        b"",
        "fatal: options '--missing-only' and '--disk-usage' cannot be used together\n",
        128,
    );
    f.expect(
        &["rev-list", "--missing=print", "--missing-only", "--count", "--disk-usage", "HEAD"],
        b"",
        "fatal: options '--missing-only' and '--count' cannot be used together\n",
        128,
    );
}
