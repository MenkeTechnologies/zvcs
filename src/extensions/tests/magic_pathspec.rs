//! Magic pathspecs (`:(exclude)`/`:!`, `:(glob)`, `:(icase)`, `:(literal)`,
//! `:(top)`) must limit `log`, `diff` and `shortlog` the way they limit stock git.
//!
//! Regression: all three ran hand-rolled matchers that understood only plain
//! paths. `log` and `shortlog` refused a magic spec outright
//! (`magic pathspecs are not ported`) and `diff` refused magic *and* wildcards
//! (`magic/glob pathspecs are not supported`), so a command that works everywhere
//! else died mid-pipeline. All three now go through `repo.pathspec()`, the engine
//! `add`, `grep`, `rm` and `ls-files` already share.
//!
//! Two things only the real engine gets right, both asserted below: an exclusion
//! *subtracts* from the set rather than matching on its own, and a spec is
//! relative to the current directory, not to the repository root — the old
//! matchers silently reported nothing for `log -- gen` run from `src/`.
//!
//! Every expectation here was taken from stock git 2.55.0 on this fixture.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary")
}

/// stdout lines of a command that must succeed.
fn out(cwd: &Path, home: &Path, args: &[&str]) -> Vec<String> {
    let o = run(cwd, home, args);
    assert!(o.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).lines().map(str::to_owned).collect()
}

/// A repository with one commit per path, so a pathspec selects a known subset:
/// c1 `src/lib.rs`, c2 `src/gen/table.rs`, c3 `web/guide.md`, c4 `README`.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-magicspec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("src/gen")).unwrap();
    std::fs::create_dir_all(repo.join("web")).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    for (path, msg) in [
        ("src/lib.rs", "c1"),
        ("src/gen/table.rs", "c2"),
        ("web/guide.md", "c3"),
        ("README", "c4"),
    ] {
        std::fs::write(repo.join(path), format!("{msg}\n")).unwrap();
        run(&repo, &home, &["add", "-A"]);
        run(&repo, &home, &["commit", "-q", "-m", msg]);
    }
    assert_eq!(out(&repo, &home, &["log", "--format=%s"]).len(), 4, "fixture: 4 commits");
    (repo, home)
}

#[test]
fn log_honors_every_magic_pathspec_form() {
    let (repo, home) = fixture("log");
    let subjects = |spec: &[&str]| -> Vec<String> {
        let mut args = vec!["log", "--format=%s", "--"];
        args.extend_from_slice(spec);
        out(&repo, &home, &args)
    };

    // An exclusion selects everything it does not name — `web` drops only c3.
    assert_eq!(subjects(&[":!web"]), ["c4", "c2", "c1"]);
    assert_eq!(subjects(&[":(exclude)web"]), ["c4", "c2", "c1"], "long form is the same spec");
    // `:(glob)` gives `**` its recursive meaning.
    assert_eq!(subjects(&[":(glob)src/**"]), ["c2", "c1"]);
    // `:(icase)` folds case; `SRC` would otherwise name nothing.
    assert_eq!(subjects(&[":(icase)SRC"]), ["c2", "c1"]);
    // `:(literal)` turns the wildcards off, so this is an exact path.
    assert_eq!(subjects(&[":(literal)src/lib.rs"]), ["c1"]);
    assert_eq!(subjects(&[":(top)web"]), ["c3"]);
    // A plain wildcard pathspec still works (`*` spans `/`, git's `wildmatch(…, 0)`).
    assert_eq!(subjects(&["*.rs"]), ["c2", "c1"]);

    // The set is the unit: the exclusion carves out of the positive spec, which no
    // per-spec "does any of them match" test can express.
    assert_eq!(subjects(&["src", ":!src/gen"]), ["c1"]);
}

#[test]
fn diff_and_shortlog_honor_magic_pathspecs() {
    let (repo, home) = fixture("diffshort");

    // `diff` used to refuse wildcards too, not just magic.
    assert_eq!(
        out(&repo, &home, &["diff", "--name-only", "HEAD~3", "HEAD", "--", ":!web"]),
        ["README", "src/gen/table.rs"]
    );
    assert_eq!(
        out(&repo, &home, &["diff", "--name-only", "HEAD~3", "HEAD", "--", "*.rs"]),
        ["src/gen/table.rs"]
    );
    assert_eq!(
        out(&repo, &home, &["diff", "--name-only", "HEAD~3", "HEAD", "--", ":(top)web"]),
        ["web/guide.md"]
    );

    // shortlog counts the commits a pathspec-limited log would show.
    let count = |spec: &str| -> String {
        out(&repo, &home, &["shortlog", "-s", "HEAD", "--", spec])
            .first()
            .map(|l| l.split_whitespace().next().unwrap_or("").to_owned())
            .unwrap_or_default()
    };
    assert_eq!(count(":!web"), "3");
    assert_eq!(count(":(glob)src/**"), "2");
    assert_eq!(count(":(literal)src/lib.rs"), "1");
    assert_eq!(count("*.md"), "1");
}

/// The rest of the pathspec-taking family, which ran its own matchers: `rev-list`
/// and `fast-export` refused magic outright, `show` and `reflog` silently dropped
/// their output, `diff-tree` refused magic *and* wildcards, and `restore` reported
/// `did not match any file(s) known to git`.
#[test]
fn the_rest_of_the_family_honors_magic_pathspecs() {
    let (repo, home) = fixture("family");
    let one = |args: &[&str]| -> Vec<String> { out(&repo, &home, args) };

    // rev-list counts commits, so a wrong pathspec shows up as a wrong number.
    assert_eq!(one(&["rev-list", "--count", "HEAD", "--", ":!web"]), ["3"]);
    assert_eq!(one(&["rev-list", "--count", "HEAD", "--", "*.rs"]), ["2"]);
    assert_eq!(one(&["rev-list", "--count", "HEAD", "--", ":(glob)src/**"]), ["2"]);

    // show prints the commit and then its file list; the list used to vanish.
    assert_eq!(
        one(&["show", "--name-only", "--format=%s", "HEAD~2", "--", ":!web"]),
        ["c2", "", "src/gen/table.rs"]
    );
    assert_eq!(
        one(&["show", "--name-only", "--format=%s", "HEAD~2", "--", "*.rs"]),
        ["c2", "", "src/gen/table.rs"]
    );

    assert_eq!(one(&["reflog", "show", "--format=%s", "HEAD", "--", ":!web"]), ["c4", "c2", "c1"]);
    assert_eq!(one(&["reflog", "show", "--format=%s", "HEAD", "--", "*.rs"]), ["c2", "c1"]);

    assert_eq!(
        one(&["diff-tree", "-r", "--name-only", "HEAD~3", "HEAD", "--", ":!web"]),
        ["README", "src/gen/table.rs"]
    );
    assert_eq!(
        one(&["diff-tree", "-r", "--name-only", "HEAD~3", "HEAD", "--", "*.rs"]),
        ["src/gen/table.rs"]
    );

    // These two only have to stop refusing; both print nothing for this history.
    assert!(one(&["restore", "--source=HEAD", "--staged", "--", ":!web"]).is_empty());
    assert!(!one(&["fast-export", "--no-data", "HEAD", "--", ":!web"]).is_empty());
}

/// A recursive tree diff reports the containing directories alongside the files
/// in them, and a directory is not what a pathspec names. Handing `src` to
/// `:(exclude)src/gen` says "not excluded", which kept every commit whose only
/// real change lived under `src/gen` — a nested exclusion that quietly did
/// nothing while a top-level one worked.
#[test]
fn a_nested_exclusion_is_decided_on_files_not_directories() {
    let (repo, home) = fixture("nested");
    let count = |spec: &str| -> String {
        out(&repo, &home, &["rev-list", "--count", "HEAD", "--", spec]).remove(0)
    };
    // c2 is the only commit under src/gen, so excluding it must drop exactly one.
    assert_eq!(count(":!src/gen"), "3");
    assert_eq!(count(":(exclude)src/gen"), "3");
    // The top-level case that used to work by accident still does.
    assert_eq!(count(":!web"), "3");
    // shortlog decides the same way and must agree.
    let tally = out(&repo, &home, &["shortlog", "-s", "HEAD", "--", ":!src/gen"]);
    assert_eq!(tally.first().map(|l| l.split_whitespace().next().unwrap_or("")), Some("3"));
}

#[test]
fn pathspecs_are_relative_to_the_current_directory() {
    let (repo, home) = fixture("prefix");
    let src = repo.join("src");

    // From `src/`, `gen` means `src/gen` — the old matcher read every spec as
    // repo-root-relative and silently reported nothing at all here.
    assert_eq!(out(&src, &home, &["log", "--format=%s", "--", "gen"]), ["c2"]);
    assert_eq!(out(&src, &home, &["log", "--format=%s", "--", "lib.rs"]), ["c1"]);
    assert_eq!(out(&src, &home, &["shortlog", "-s", "HEAD", "--", "gen"]).len(), 1);
    assert_eq!(
        out(&src, &home, &["diff", "--name-only", "HEAD~3", "HEAD", "--", "gen"]),
        ["src/gen/table.rs"],
        "diff reports repo-relative paths for a cwd-relative spec"
    );

    // `:(top)` opts back out to the repository root from anywhere.
    assert_eq!(out(&src, &home, &["log", "--format=%s", "--", ":(top)web"]), ["c3"]);
    // An exclusion is resolved against the same prefix.
    assert_eq!(out(&src, &home, &["log", "--format=%s", "--", ":!gen"]), ["c4", "c3", "c1"]);
    assert_eq!(out(&src, &home, &["rev-list", "--count", "HEAD", "--", ":!gen"]), ["3"]);

    // `restore` resolves its specs to repo-root-relative paths before matching, so
    // the engine must not apply the prefix a second time — that matched nothing at
    // all and reported every spec as unknown.
    let r = run(&src, &home, &["restore", "--source=HEAD", "--staged", "--", "gen"]);
    assert!(
        r.status.success(),
        "restore from a subdirectory must accept a relative spec: {}",
        String::from_utf8_lossy(&r.stderr)
    );
}
