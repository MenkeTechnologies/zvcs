//! `git blame --ignore-rev` moves the lines of a purely-reformatting commit back onto
//! the commit that wrote them, by matching each line against the parent's lines with
//! git's byte-pair fingerprint. The markers `blame.markIgnoredLines` and
//! `blame.markUnblamableLines` report which lines were moved and which could not be.
//!
//! Also covers `git blame`'s two "no such path" diagnostics, which differ in quoting
//! and in which revision they name.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity env vars git honors above `user.name`/`user.email`; a CI runner that
/// exports them would otherwise author every commit here.
const IDENTITY_ENV: [&str; 4] =
    ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"];

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    for var in IDENTITY_ENV {
        cmd.env_remove(var);
    }
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run `git [<global>...] blame <args>...` and return stdout. `global` holds the
/// `-c key=value` pairs, which have to precede the subcommand.
fn blame_with(dir: &Path, home: &Path, global: &[&str], args: &[&str]) -> String {
    let mut full: Vec<&str> = global.to_vec();
    full.push("blame");
    full.extend_from_slice(args);
    let out = run(dir, home, &full);
    assert!(
        out.status.success(),
        "{full:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn blame(dir: &Path, home: &Path, args: &[&str]) -> String {
    blame_with(dir, home, &[], args)
}

/// The object-name column of every blame line, i.e. everything before the first space.
fn object_names(output: &str) -> Vec<String> {
    output
        .lines()
        .map(|l| l.split(' ').next().unwrap_or_default().to_string())
        .collect()
}

fn rev_parse(dir: &Path, home: &Path, rev: &str) -> String {
    let out = run(dir, home, &["rev-parse", rev]);
    assert!(out.status.success(), "rev-parse {rev} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository whose third commit only re-indents the file: `c1` writes the body,
/// `c2` appends a function, `c3` converts every four-space indent to a tab.
fn fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "a@e.co"]);
    git(&repo, &home, &["config", "user.name", "author"]);

    let base = "fn alpha() {\n    one();\n    two();\n}\n";
    std::fs::write(repo.join("code.txt"), base).unwrap();
    git(&repo, &home, &["add", "code.txt"]);
    git(&repo, &home, &["commit", "-qm", "c1"]);

    std::fs::write(repo.join("code.txt"), format!("{base}fn beta() {{\n    three();\n}}\n")).unwrap();
    git(&repo, &home, &["add", "code.txt"]);
    git(&repo, &home, &["commit", "-qm", "c2"]);

    let reindented = std::fs::read_to_string(repo.join("code.txt"))
        .unwrap()
        .replace("    ", "\t");
    std::fs::write(repo.join("code.txt"), reindented).unwrap();
    git(&repo, &home, &["add", "code.txt"]);
    git(&repo, &home, &["commit", "-qm", "reindent"]);

    (repo, home)
}

#[test]
fn ignore_rev_moves_reformatted_lines_back_to_their_author() {
    let (repo, home) = fixture("blame-ignore-rev");
    let c1 = rev_parse(&repo, &home, "HEAD~2");
    let c2 = rev_parse(&repo, &home, "HEAD~1");
    let reindent = rev_parse(&repo, &home, "HEAD");

    // `--root` keeps the root commit out of the boundary treatment, so every object
    // name in the output is a plain full hash.
    // Without the option, the re-indented lines belong to the reindent commit.
    let plain_names = object_names(&blame(&repo, &home, &["-l", "--root", "code.txt"]));
    assert_eq!(plain_names[1], reindent, "line 2 before ignoring");
    assert_eq!(plain_names[2], reindent, "line 3 before ignoring");
    assert_eq!(plain_names[5], reindent, "line 6 before ignoring");

    // With it, each line goes back to the commit that wrote its content: the `alpha`
    // body to c1 and the `beta` body to c2. Getting this right requires the fuzzy
    // matcher to map lines across the reindent, not merely to skip the commit.
    let ignored = blame(&repo, &home, &["-l", "--root", "--ignore-rev", &reindent, "code.txt"]);
    let names = object_names(&ignored);
    assert_eq!(names[1], c1, "`one();` belongs to c1");
    assert_eq!(names[2], c1, "`two();` belongs to c1");
    assert_eq!(names[5], c2, "`three();` belongs to c2");
    assert!(
        !ignored.contains(&reindent),
        "no line may still be attributed to the ignored commit:\n{ignored}"
    );
}

#[test]
fn mark_ignored_lines_flags_exactly_the_re_attributed_lines() {
    let (repo, home) = fixture("blame-mark-ignored");
    let reindent = rev_parse(&repo, &home, "HEAD");

    let marked = blame_with(
        &repo,
        &home,
        &["-c", "blame.markIgnoredLines=true"],
        &["--ignore-rev", &reindent, "code.txt"],
    );
    let marked: Vec<&str> = marked.lines().collect();
    // Lines 1 and 4 (`fn alpha() {` and `}`) never changed, so they are not ignored;
    // lines 2, 3 and 6 were re-indented and therefore re-attributed.
    for (index, expect_marker) in [(0, false), (1, true), (2, true), (3, false), (5, true)] {
        let name = marked[index].split(' ').next().unwrap();
        assert_eq!(
            name.contains('?'),
            expect_marker,
            "line {} marker in {name:?}",
            index + 1
        );
    }
    // The marker takes a column from the object name rather than widening it.
    let widths: Vec<usize> = marked
        .iter()
        .map(|l| l.split(' ').next().unwrap().len())
        .collect();
    assert!(
        widths.windows(2).all(|w| w[0] == w[1]),
        "object-name column must stay one width: {widths:?}"
    );
}

#[test]
fn ignore_revs_file_matches_ignore_rev_and_honors_comments() {
    let (repo, home) = fixture("blame-ignore-file");
    let reindent = rev_parse(&repo, &home, "HEAD");

    std::fs::write(
        repo.join("skip.txt"),
        format!("# a comment\n\n   {reindent}   # trailing comment\n"),
    )
    .unwrap();

    let via_file = blame(&repo, &home, &["-l", "--ignore-revs-file", "skip.txt", "code.txt"]);
    let via_flag = blame(&repo, &home, &["-l", "--ignore-rev", &reindent, "code.txt"]);
    assert_eq!(via_file, via_flag);

    // `blame.ignoreRevsFile` feeds the same list, and `--no-ignore-revs-file` clears
    // it — config-supplied entries included.
    let via_config = blame_with(
        &repo,
        &home,
        &["-c", "blame.ignoreRevsFile=skip.txt"],
        &["-l", "code.txt"],
    );
    assert_eq!(via_config, via_flag);

    let cleared = blame_with(
        &repo,
        &home,
        &["-c", "blame.ignoreRevsFile=skip.txt"],
        &["-l", "--no-ignore-revs-file", "code.txt"],
    );
    let plain = blame(&repo, &home, &["-l", "code.txt"]);
    assert_eq!(cleared, plain);
}

#[test]
fn a_bad_ignore_list_is_fatal_with_gits_wording() {
    let (repo, home) = fixture("blame-ignore-bad");

    let out = run(&repo, &home, &["blame", "--ignore-revs-file", "missing.txt", "code.txt"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: could not open object name list: missing.txt\n"
    );

    std::fs::write(repo.join("bad.txt"), "not-a-hash\n").unwrap();
    let out = run(&repo, &home, &["blame", "--ignore-revs-file", "bad.txt", "code.txt"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: invalid object name: not-a-hash\n"
    );

    let out = run(&repo, &home, &["blame", "--ignore-rev", "no-such-rev", "code.txt"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: cannot find revision no-such-rev to ignore\n"
    );
}

#[test]
fn a_missing_path_uses_gits_two_different_diagnostics() {
    let (repo, home) = fixture("blame-missing-path");

    // With a final image overlaid on the commit (no revision given), git quotes the
    // path and always names HEAD.
    let out = run(&repo, &home, &["blame", "nosuchfile"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: no such path 'nosuchfile' in HEAD\n"
    );

    // With an explicit revision and no overlay, it does not quote and names the
    // revision exactly as the user typed it.
    let out = run(&repo, &home, &["blame", "HEAD", "--", "nosuchfile"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: no such path nosuchfile in HEAD\n"
    );
    let out = run(&repo, &home, &["blame", "main", "--", "nosuchfile"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: no such path nosuchfile in main\n"
    );

    // A path that is only in the index has no history to blame, but is not an error:
    // every line belongs to the synthetic not-yet-committed commit.
    std::fs::write(repo.join("staged.txt"), "fresh\n").unwrap();
    git(&repo, &home, &["add", "staged.txt"]);
    let out = blame(&repo, &home, &["staged.txt"]);
    assert!(out.contains("Not Committed Yet"), "{out}");
    assert!(out.trim_end().ends_with("fresh"), "{out}");
}
