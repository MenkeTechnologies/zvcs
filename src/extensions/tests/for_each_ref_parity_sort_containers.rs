//! Two places where `for-each-ref`'s option handling is looser than the atom
//! table suggests, both measured against stock git 2.55.0.
//!
//!   * A `--sort` key goes through `parse_sorting_atom()`
//!     (ref-filter.c:3673-3687), which runs `parse_ref_filter_atom()` and
//!     nothing else. The `reject_atom()` pass that refuses `%(rest)` belongs to
//!     `verify_ref_format()` (ref-filter.c:1401-1402), so it never sees a sort
//!     key; and the container atoms are ordinary table entries here rather than
//!     stack directives, so `%(align)`, `%(end)`, `%(if)`, `%(then)` and
//!     `%(else)` all parse. `populate_value()` answers each of them with a fixed
//!     string (ref-filter.c:2546-2575), so every ref compares equal and the
//!     iteration order survives — while each atom's own parser still runs, which
//!     is why `--sort=align` dies for want of a width.
//!
//!   * `--points-at` is `OPT_CALLBACK(…, parse_opt_object_name)`
//!     (builtin/for-each-ref.c:42-44), and that callback *appends* to an
//!     `oid_array` (parse-options-cb.c:126-140). Repeating the option widens the
//!     filter — `match_points_at()` (ref-filter.c:2840-2866) accepts a ref whose
//!     own id or any object along its tag chain is in the array — and
//!     `--no-points-at` clears it.
#![cfg(unix)]

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
    /// Two commits on `main`, with branch `old` and an annotated tag `v1` left
    /// behind on the first — so a ref pointing at one commit is distinguishable
    /// from a ref pointing at the other, and `v1` reaches its commit only by
    /// peeling.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fer-sortc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["branch", "old"]);
        f.git(&["tag", "-a", "-m", "msg", "v1", "old"]);
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.git(&["add", "b"]);
        f.git(&["commit", "-q", "-m", "two"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
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
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    /// The refnames one `for-each-ref` invocation lists, in the order given.
    fn names(&self, extra: &[&str]) -> String {
        let mut args = vec!["for-each-ref", "--format=%(refname)"];
        args.extend_from_slice(extra);
        self.stdout(&args)
    }

    fn dies(&self, args: &[&str], fatal: &str) {
        let (out, err, code) = self.run(args);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", format!("fatal: {fatal}\n").as_str(), 128),
            "`git {args:?}`"
        );
    }
}

/// `%(align)`, `%(end)`, `%(if)`, `%(then)`, `%(else)` and `%(rest)` are all
/// legal sort keys, and each compares every ref equal — so the key contributes
/// nothing and the keys given *before* it decide the order (`--sort` keys apply
/// last-given-first). The deref `*` and an ignored argument change none of that.
#[test]
fn container_atoms_sort_every_ref_equal() {
    let f = Fixture::new("equal");
    let unsorted = f.names(&[]);
    assert_eq!(unsorted, "refs/heads/main\nrefs/heads/old\nrefs/tags/v1\n");
    let reversed = "refs/tags/v1\nrefs/heads/old\nrefs/heads/main\n";

    for key in [
        "align:3,left",
        "end",
        "if",
        "if:equals=x",
        "then",
        "else",
        "rest",
        // `then` and `else` carry no parser, so a trailing argument is ignored
        // rather than refused.
        "then:x",
        "end:x",
        // `parse_ref_filter_atom()` only strips the deref (ref-filter.c:1042).
        "*if",
        "*rest",
        "*align:5",
        // The `version:` prefix is stripped before the atom is parsed, and
        // `versioncmp("", "")` is 0 just the same.
        "version:if",
    ] {
        assert_eq!(f.names(&[&format!("--sort={key}")]), unsorted, "--sort={key}");
        assert_eq!(
            f.names(&["--sort=-refname", &format!("--sort={key}")]),
            reversed,
            "--sort=-refname --sort={key}"
        );
    }
}

/// Each container atom's own parser still runs for a sort key, so the failures
/// are the parser's, not "unknown field name" — and `%(rest)`'s refusal, which
/// lives in `verify_ref_format()` rather than in its parser, is absent here.
#[test]
fn container_sort_keys_report_their_parsers_failures() {
    let f = Fixture::new("parser");
    // `align_atom_parser()` (ref-filter.c:830-831) demands an argument, and an
    // empty one is nulled before it (ref-filter.c:1095-1101).
    for key in ["align", "align:", "-align", "*align"] {
        f.dies(
            &["for-each-ref", &format!("--sort={key}")],
            "expected format: %(align:<width>,<position>)",
        );
    }
    f.dies(
        &["for-each-ref", "--sort=align:wide"],
        "unrecognized %(align) argument: wide",
    );
    f.dies(
        &["for-each-ref", "--sort=align:left"],
        "positive width expected with the %(align) atom",
    );
    // `if_atom_parser()` (ref-filter.c:874-888) takes `equals=` / `notequals=`.
    f.dies(
        &["for-each-ref", "--sort=if:bogus"],
        "unrecognized %(if) argument: bogus",
    );
    // `rest_atom_parser()` (ref-filter.c:890-897) refuses an argument — that is
    // all it does, so this is the one `%(rest)` failure a sort key can reach.
    f.dies(
        &["for-each-ref", "--sort=rest:x"],
        "%(rest) does not take arguments",
    );
}

/// `reject_atom()` is still enforced where it belongs: in a `--format`. It runs
/// after the atom's parser, so `%(rest:x)` reports its argument instead, and it
/// quotes the atom exactly as written, deref `*` included.
#[test]
fn rest_is_still_rejected_in_a_format() {
    let f = Fixture::new("reject");
    f.dies(
        &["for-each-ref", "--format=%(rest)"],
        "this command reject atom %(rest)",
    );
    f.dies(
        &["for-each-ref", "--format=%(*rest)"],
        "this command reject atom %(*rest)",
    );
    f.dies(
        &["for-each-ref", "--format=x%(align:5)%(rest)%(end)"],
        "this command reject atom %(rest)",
    );
    f.dies(
        &["for-each-ref", "--format=%(rest:x)"],
        "%(rest) does not take arguments",
    );
}

/// A repeated `--points-at` is a union, not a replacement, and `--no-points-at`
/// empties the array. `v1` is an annotated tag, so it answers to the commit it
/// peels to rather than to its own id.
#[test]
fn points_at_accumulates_and_no_points_at_clears() {
    let f = Fixture::new("points");
    let main = "refs/heads/main\n";
    let old = "refs/heads/old\nrefs/tags/v1\n";
    let all = "refs/heads/main\nrefs/heads/old\nrefs/tags/v1\n";

    assert_eq!(f.names(&["--points-at=main"]), main);
    assert_eq!(f.names(&["--points-at=old"]), old);
    assert_eq!(f.names(&["--points-at=old", "--points-at=main"]), all);
    assert_eq!(f.names(&["--points-at=main", "--points-at=old"]), all);
    // The separate-argument spelling reaches the same callback.
    assert_eq!(f.names(&["--points-at", "main", "--points-at", "old"]), all);
    // `--no-points-at` is the callback's `unset` branch: `oid_array_clear()`.
    assert_eq!(f.names(&["--points-at=old", "--no-points-at"]), all);
    assert_eq!(
        f.names(&["--points-at=old", "--no-points-at", "--points-at=main"]),
        main
    );
    // A tag's own id is matched directly, without peeling.
    let v1 = f.stdout(&["rev-parse", "v1"]);
    assert_eq!(f.names(&[&format!("--points-at={}", v1.trim())]), "refs/tags/v1\n");
    // An id nothing points at filters everything out, and is not an error even
    // though no such object exists.
    assert_eq!(f.names(&["--points-at=0000000000000000000000000000000000000000"]), "");
}

/// The callback reports a name it cannot resolve through `error()` and returns
/// -1, which parse-options turns into exit 129 — and it does so as the option is
/// read, so a later `--points-at` never runs.
#[test]
fn points_at_rejects_a_malformed_name_before_the_later_ones() {
    let f = Fixture::new("badname");
    let (out, err, code) = f.run(&["for-each-ref", "--points-at=nope", "--points-at=main"]);
    assert_eq!(out, "");
    assert_eq!(err, "error: malformed object name 'nope'\n");
    assert_eq!(code, 129);
}
