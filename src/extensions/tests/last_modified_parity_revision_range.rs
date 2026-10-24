//! `git last-modified <revision-range>`: the excluded side of `A..B` / `^X` seeds the
//! `--not` queue (builtin/last-modified.c:364-386), its ancestry is marked PARENT2 as the
//! walk goes (builtin/last-modified.c:416-430), and paths that reach it are emitted against
//! the `^`-prefixed boundary commit in tree-diff order. With a commit-graph the `--not`
//! drain stops by generation, and the queue reads an unparsed parent as
//! `GENERATION_NUMBER_INFINITY`, so both are checked with and without one.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// Extend the shared history with a second directory, a topic branch and a merge, each
/// commit at its own pinned date.
fn grow(side: &twin_repo::Side) {
    let at = |t: &'static str| [("GIT_AUTHOR_DATE", t), ("GIT_COMMITTER_DATE", t)];
    side.write("d/x.rs", "x\n");
    side.write("b", "b\n");
    side.git(&["add", "d/x.rs", "b"]);
    side.git_env(&at("1700000100 +0000"), &["commit", "-q", "-m", "four"]);
    side.git(&["checkout", "-q", "-b", "topic", "main~1"]);
    side.write("d/y.rs", "y\n");
    side.git(&["add", "d/y.rs"]);
    side.git_env(&at("1700000200 +0000"), &["commit", "-q", "-m", "topic"]);
    side.git(&["checkout", "-q", "main"]);
    side.git_env(&at("1700000300 +0000"), &["merge", "-q", "--no-edit", "topic"]);
    side.git_env(&at("1700000400 +0000"), &["tag", "-a", "-m", "t", "t", "main"]);
}

#[test]
fn ranges_walk_to_the_boundary_as_stock_does() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("last-modified-range", stock);
    grow(&s);
    grow(&z);
    let cases: &[&[&str]] = &[
        &["last-modified", "main..HEAD"],
        &["last-modified", "-r", "-t", "main..HEAD", "*.rs"],
        &["last-modified", "-r", "main~1..main"],
        &["last-modified", "-r", "-t", "main~2..main"],
        &["last-modified", "-r", "^main~3", "main"],
        &["last-modified", "-r", "main", "^topic", "--", "d"],
        &["last-modified", "-r", "topic..main", "*.rs"],
        &["last-modified", "-r", "main..topic"],
        &["last-modified", "-r", "-z", "side..main"],
        &["last-modified", "-r", "main~1.."],
        &["last-modified", "^main"],
        &["last-modified", "^nosuch"],
        // The tag is excluded while the commit it names is the tip: that commit never
        // enters the walk queue and stock dies in BUG().
        &["last-modified", "t..main"],
    ];
    for graph in [false, true] {
        if graph {
            for side in [&s, &z] {
                let out = side.run_in(&side.repo(), &[], &["commit-graph", "write", "--reachable"]);
                assert_eq!(out.code, 0, "{out:?}");
            }
        }
        for args in cases {
            assert_eq!(z.git(args), s.git(args), "{args:?} graph={graph}");
        }
    }
}
