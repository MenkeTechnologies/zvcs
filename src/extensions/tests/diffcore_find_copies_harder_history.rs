//! `--find-copies-harder` across the history commands, and the `--follow` pass that
//! is built on top of it.
//!
//! Plain `-C` can only pair a new file with a source the same commit also touched,
//! because those are the only entries the tree walk queued. `--find-copies-harder`
//! widens that: `tree-diff.c:517-532` skips emitting a path whose entry is identical
//! on both sides only `if (!opt->flags.find_copies_harder)`, so with the flag every
//! unchanged path is queued as an unmodified pair and `diffcore_rename()` can take it
//! as a copy source. `diff_setup_done()` (diff.c:5288) additionally turns copy
//! detection on for a lone `--find-copies-harder`.
//!
//! `--follow` runs exactly that pass for itself: `try_to_follow_renames()`
//! (tree-diff.c:631-640) builds its own `diff_options` with
//! `flags.find_copies_harder = 1`, keeps only the command's `rename_score` and
//! `break_opt`, and the `R`/`C` pair it finds replaces the queue outright
//! (`q->queue[0] = choice; q->nr = 1`) with `found_follow = 1` telling
//! `diffcore_std()` to leave the rename information alone. So a followed file that
//! was copied is reported as the copy it is — and the walk carries on into the
//! source's history.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PINNED_ENV: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "author"),
    ("GIT_AUTHOR_EMAIL", "a@e.co"),
    ("GIT_COMMITTER_NAME", "author"),
    ("GIT_COMMITTER_EMAIL", "a@e.co"),
    ("GIT_AUTHOR_DATE", "2005-04-07T22:13:13+0000"),
    ("GIT_COMMITTER_DATE", "2005-04-07T22:13:13+0000"),
];

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    for (key, value) in PINNED_ENV {
        cmd.env(key, value);
    }
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn git(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `a.txt` with twenty lines, then a commit that writes `b.txt` as a copy of it with
/// one line changed and leaves `a.txt` alone. The source is untouched, so only the
/// harder pass can find it; the one changed line keeps the similarity below 100%, so
/// the exact-rename pass cannot find it either.
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

    let body: String = (1..=20).map(|n| format!("line {n}\n")).collect();
    std::fs::write(repo.join("a.txt"), &body).unwrap();
    git(&repo, &home, &["add", "a.txt"]);
    git(&repo, &home, &["commit", "-qm", "c1"]);

    std::fs::write(repo.join("b.txt"), body.replace("line 10\n", "LINE TEN\n")).unwrap();
    git(&repo, &home, &["add", "b.txt"]);
    git(&repo, &home, &["commit", "-qm", "copy a->b"]);

    (repo, home)
}

/// The `--name-status` records of the output, ignoring the commit subjects and the
/// blank lines between them.
fn status_lines(out: &str) -> Vec<&str> {
    out.lines().filter(|l| l.contains('\t')).collect()
}

#[test]
fn show_and_log_find_a_copy_whose_source_the_commit_left_alone() {
    let (repo, home) = fixture("fch-show-log");

    // The similarity is fixed by the fixture: one line of twenty rewritten.
    const COPY: &str = "C094\ta.txt\tb.txt";
    const ADD: &str = "A\tb.txt";

    for args in [
        &["show", "-C", "-C", "--name-status", "--format=", "HEAD"][..],
        &["show", "--find-copies-harder", "--name-status", "--format=", "HEAD"][..],
        &["log", "-1", "-C", "-C", "--name-status", "--format=", "HEAD"][..],
        &["log", "-1", "--find-copies-harder", "--name-status", "--format=", "HEAD"][..],
    ] {
        let out = git(&repo, &home, args);
        assert_eq!(status_lines(&out), vec![COPY], "{args:?} must report the copy:\n{out}");
    }

    // Raw carries the same pairing with both object names and the mode columns.
    let raw = git(&repo, &home, &["show", "-C", "-C", "--raw", "--format=", "HEAD"]);
    let raw = status_lines(&raw);
    assert_eq!(raw.len(), 1, "one entry expected: {raw:?}");
    assert!(
        raw[0].ends_with("C094\ta.txt\tb.txt") && raw[0].starts_with(":100644 100644 "),
        "raw entry: {}",
        raw[0]
    );

    // A single `-C` must NOT find it: `a.txt` is not among the entries the tree walk
    // queued, so there is no source to pair with. This is the control that keeps the
    // harder pass from simply being "copy detection, always on".
    for args in [
        &["show", "-C", "--name-status", "--format=", "HEAD"][..],
        &["show", "--name-status", "--format=", "HEAD"][..],
        &["log", "-1", "-C", "--name-status", "--format=", "HEAD"][..],
    ] {
        let out = git(&repo, &home, args);
        assert_eq!(status_lines(&out), vec![ADD], "{args:?} must report an addition:\n{out}");
    }

    // git limits the *tree walk* to the pathspec, so a source the pathspec excludes is
    // never queued and cannot become a copy source however hard the pass looks.
    let limited = git(&repo, &home, &["show", "-C", "-C", "--name-status", "--format=", "HEAD", "--", "b.txt"]);
    assert_eq!(
        status_lines(&limited),
        vec![ADD],
        "a pathspec that excludes a.txt excludes it as a copy source too:\n{limited}"
    );
    let both = git(&repo, &home, &["show", "-C", "-C", "--name-status", "--format=", "HEAD", "--", "a.txt", "b.txt"]);
    assert_eq!(
        status_lines(&both),
        vec![COPY],
        "with a.txt in the pathspec the copy is found again:\n{both}"
    );
}

#[test]
fn follow_reports_the_copy_pair_and_keeps_walking_into_the_source() {
    let (repo, home) = fixture("fch-follow");

    // `--follow` finds the pair with its own harder pass, so the newest record is the
    // copy — not the addition the command's own `-M` would see — and the followed
    // path then becomes `a.txt`, which is why the root commit is reached at all.
    let names = git(&repo, &home, &["log", "--follow", "--name-status", "--format=%s", "b.txt"]);
    assert_eq!(
        status_lines(&names),
        vec!["C094\ta.txt\tb.txt", "A\ta.txt"],
        "--follow must report the copy and then continue into a.txt:\n{names}"
    );

    // The patch is rendered from that same pair, so it is a copy patch against
    // `a.txt` rather than a `new file mode` patch.
    let patch = git(&repo, &home, &["log", "--follow", "-p", "--format=%s", "b.txt"]);
    assert!(
        patch.contains("diff --git a/a.txt b/b.txt\nsimilarity index 94%\ncopy from a.txt\ncopy to b.txt\n"),
        "--follow -p must render the copy patch:\n{patch}"
    );
    assert!(
        !patch.contains("diff --git a/b.txt b/b.txt"),
        "b.txt must never be rendered as a one-sided creation:\n{patch}"
    );
    // The root commit really does create `a.txt`, which is what the walk reached by
    // following the copy backwards.
    assert!(
        patch.contains("diff --git a/a.txt b/a.txt\nnew file mode 100644\n"),
        "the walk must reach the commit that created a.txt:\n{patch}"
    );

    // `diff_opts.rename_score = opt->rename_score`: the follow pass runs at the
    // threshold the command line set, so a 94% copy is declined at `-C97%` and the
    // walk stops at the commit that created `b.txt`.
    let strict = git(&repo, &home, &["log", "--follow", "-C97%", "--name-status", "--format=%s", "b.txt"]);
    assert_eq!(
        status_lines(&strict),
        vec!["A\tb.txt"],
        "-C97% declines this copy, so the follow stops here:\n{strict}"
    );
}
