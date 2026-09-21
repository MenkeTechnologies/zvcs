//! Three places `rev-list` reads state the walk already holds and the port was
//! dropping on the floor.
//!
//! * `%P` / `%p` under `rev-list --pretty`: `format_commit_one()`'s `'P'` arm
//!   walks `commit->parents`, so the parent list belongs in the
//!   `pretty_print_context` `cmd_rev_list()` builds, not only in `log`'s.
//! * `--ancestry-path=<commit>`: `get_reference(revs, optarg, &oid, ANCESTRY_PATH)`
//!   (revision.c:2422) flags the named commit and `process_parents()` passes the
//!   flag down (revision.c:1179), so `limit_to_ancestry()` exempts the bottom's
//!   own ancestry (revision.c:1391) — which the argument-less spelling, setting
//!   no flag, does not.
//! * `refs/replace/<oid>`: `add_ref_decoration()` (log-tree.c:162-175) reads the
//!   replaced object's name out of the refname and decorates *that* object with
//!   the word `replaced`, at that point in the ref walk.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(repo)
        .env("HOME", repo.join(".isolated-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com");
    c
}

fn run(repo: &Path, args: &[&str]) -> Output {
    cmd(repo, args).output().unwrap()
}

fn ok(repo: &Path, args: &[&str]) -> String {
    let out = run(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn oid(repo: &Path, rev: &str) -> String {
    ok(repo, &["rev-parse", rev]).trim().to_string()
}

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn commit(repo: &Path, name: &str, n: i64) {
    std::fs::write(repo.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
    let date = format!("{} +0000", 1_600_000_000 + n * 60);
    assert!(cmd(repo, &["add", "-A"]).output().unwrap().status.success());
    let out = cmd(repo, &["commit", "-q", "-m", name])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// ```text
/// a --- b --- c        (main)
///        \
///         d --- e      (topic)
/// ```
///
/// `b` is on the path *into* `d` without descending from it, which is the only
/// thing `--ancestry-path=d` keeps that `limit_to_ancestry()` alone would drop.
fn fixture(tag: &str) -> Fixture {
    let dir = Fixture(
        std::env::temp_dir().join(format!("zvcs-revlist-ancestry-{tag}-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(dir.path());
    std::fs::create_dir_all(dir.path().join(".isolated-home")).unwrap();
    let repo = dir.path();
    ok(repo, &["init", "-q", "-b", "main"]);
    commit(repo, "a", 1);
    commit(repo, "b", 2);
    commit(repo, "c", 3);
    ok(repo, &["checkout", "-q", "-b", "topic", "main~1"]);
    commit(repo, "d", 4);
    commit(repo, "e", 5);
    ok(repo, &["checkout", "-q", "main"]);
    dir
}

/// `%P` is the full parent list and `%p` the abbreviated one, for a merge as
/// well as for a single-parent commit. `git log` already rendered both; the
/// context `cmd_rev_list()` builds is a different one.
#[test]
fn rev_list_pretty_expands_the_parent_placeholders() {
    let dir = fixture("parents");
    let repo = dir.path();
    ok(repo, &["merge", "-q", "--no-ff", "-m", "m", "topic"]);

    let out = ok(repo, &["rev-list", "--no-walk", "--pretty=format:%P", "main", "main~1"]);
    assert_eq!(
        out,
        format!(
            "commit {m}\n{c} {e}\ncommit {c}\n{b}\n",
            m = oid(repo, "main"),
            c = oid(repo, "main^1"),
            e = oid(repo, "main^2"),
            b = oid(repo, "main^1~1"),
        )
    );

    // `%p` abbreviates each entry the same way `%h` abbreviates the object name.
    let out = ok(repo, &["rev-list", "--no-walk", "--pretty=format:%p", "main"]);
    let (_, parents) = out.split_once('\n').unwrap();
    let expected: Vec<String> = ["main^1", "main^2"]
        .iter()
        .map(|rev| ok(repo, &["rev-parse", "--short", &oid(repo, rev)]).trim().to_string())
        .collect();
    assert_eq!(parents, format!("{}\n", expected.join(" ")));
}

/// `--ancestry-path=d` keeps `e` (descends from `d`), `d` itself, and `b`
/// (flagged `ANCESTRY_PATH` as `d`'s ancestor) while dropping `c`, which is
/// neither. The argument-less spelling takes its bottoms from the range and
/// flags nothing, so it keeps `c` and drops nothing above `a`.
#[test]
fn an_explicit_ancestry_path_bottom_keeps_its_own_ancestry() {
    let dir = fixture("ancestry");
    let repo = dir.path();

    let out = ok(repo, &["rev-list", "--ancestry-path=topic~1", "^main~2", "--all"]);
    assert_eq!(
        out,
        format!(
            "{e}\n{d}\n{b}\n",
            e = oid(repo, "topic"),
            d = oid(repo, "topic~1"),
            b = oid(repo, "main~1"),
        )
    );

    let out = ok(repo, &["rev-list", "--ancestry-path", "^main~2", "--all"]);
    assert_eq!(
        out,
        format!(
            "{e}\n{d}\n{c}\n{b}\n",
            e = oid(repo, "topic"),
            d = oid(repo, "topic~1"),
            c = oid(repo, "main"),
            b = oid(repo, "main~1"),
        )
    );
}

/// The `replaced` decoration lands on the object the ref replaces, takes its
/// place among the refs by the `refs/replace/<oid>` name it was added under —
/// after the `HEAD -> main` fold, which is added last and renders first — and
/// disappears when the replacement map is switched off.
#[test]
fn a_replace_ref_decorates_the_object_it_replaces() {
    let dir = fixture("replace");
    let repo = dir.path();
    let replaced = oid(repo, "main");
    ok(repo, &["replace", "--graft", &replaced, &oid(repo, "main~2")]);

    assert_eq!(
        ok(repo, &["log", "-1", "--pretty=%D", &replaced]),
        "HEAD -> main, replaced\n"
    );
    // The replacement commit the ref points at is not decorated: only the
    // object being replaced is.
    let replacement = ok(repo, &["rev-parse", &format!("refs/replace/{replaced}")])
        .trim()
        .to_string();
    assert_ne!(replacement, replaced);
    assert_eq!(ok(repo, &["log", "-1", "--pretty=%D", &replacement]), "\n");

    for off in [
        vec!["--no-replace-objects", "log"],
        vec!["-c", "core.useReplaceRefs=false", "log"],
    ] {
        let mut args = off;
        args.extend(["-1", "--pretty=%D", &replaced]);
        assert_eq!(ok(repo, &args), "HEAD -> main\n", "{args:?}");
    }

    // `--bisect-all` renders the same decoration list ahead of its `dist=`
    // entry, which `best_bisection_sorted()` appends last.
    let out = ok(repo, &["rev-list", "--bisect-all", &replaced]);
    assert!(
        out.lines().any(|l| l.contains(&replaced) && l.contains("replaced, dist=")),
        "no replaced decoration in:\n{out}"
    );
}
