//! `git last-modified` with the rest of `setup_revisions()`' revision syntax: `A...B`
//! pends the merge bases excluded ahead of A and B (revision.c:2100-2127), `X^@`, `X^!` and
//! `X^-<n>` pend X's parents through `add_parents_only()` (revision.c:1895-1934,
//! 2190-2219), `--not` flips the flags every later operand is pended under, `--all` pends
//! every ref, `HEAD` and the other worktrees' `HEAD`s, and `-n` turns each commit popped
//! past the limit into a `^`-prefixed boundary (builtin/last-modified.c:394-407). Each is
//! checked with and without a commit-graph, since the merge-base walk and both queues read
//! generation numbers when there is one.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// Extend the shared history with a directory, a two-commit topic merged back, an annotated
/// tag of a commit, a tag of a tree, and a criss-cross pair whose merge bases are two
/// commits, each commit at its own pinned date.
fn grow(side: &twin_repo::Side) {
    let at = |t: &'static str| [("GIT_AUTHOR_DATE", t), ("GIT_COMMITTER_DATE", t)];
    let ok = |out: twin_repo::Out| assert_eq!(out.code, 0, "{out:?}");
    side.write("d/x.rs", "x\n");
    side.write("b", "b\n");
    ok(side.git(&["add", "d/x.rs", "b"]));
    ok(side.git_env(&at("1700000100 +0000"), &["commit", "-q", "-m", "four"]));
    ok(side.git(&["checkout", "-q", "-b", "topic", "main~1"]));
    side.write("d/y.rs", "y\n");
    side.write("c", "c\n");
    ok(side.git(&["add", "d/y.rs", "c"]));
    ok(side.git_env(&at("1700000200 +0000"), &["commit", "-q", "-m", "topic1"]));
    side.write("d/y.rs", "y2\n");
    ok(side.git(&["add", "d/y.rs"]));
    ok(side.git_env(&at("1700000250 +0000"), &["commit", "-q", "-m", "topic2"]));
    ok(side.git(&["checkout", "-q", "main"]));
    side.write("a", "a3\n");
    ok(side.git(&["add", "a"]));
    ok(side.git_env(&at("1700000260 +0000"), &["commit", "-q", "-m", "five"]));
    ok(side.git_env(&at("1700000300 +0000"), &["merge", "-q", "--no-edit", "topic"]));
    ok(side.git_env(&at("1700000400 +0000"), &["tag", "-a", "-m", "t", "t", "main~1"]));
    ok(side.git(&["tag", "tt", "main^{tree}"]));
    // Criss-cross: cx and cy each merge the other's first commit.
    ok(side.git(&["checkout", "-q", "-b", "cx", "main"]));
    side.write("x", "x\n");
    ok(side.git(&["add", "x"]));
    ok(side.git_env(&at("1700000500 +0000"), &["commit", "-q", "-m", "cx1"]));
    ok(side.git(&["checkout", "-q", "-b", "cy", "main"]));
    side.write("y", "y\n");
    ok(side.git(&["add", "y"]));
    ok(side.git_env(&at("1700000510 +0000"), &["commit", "-q", "-m", "cy1"]));
    ok(side.git_env(&at("1700000520 +0000"), &["merge", "-q", "--no-edit", "cx~0"]));
    ok(side.git(&["checkout", "-q", "cx"]));
    ok(side.git_env(&at("1700000530 +0000"), &["merge", "-q", "--no-edit", "cy~1"]));
    ok(side.git(&["checkout", "-q", "main"]));
}

fn compare(cases: &[&[&str]], label: &str, extra: impl Fn(&twin_repo::Side)) {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        grow(side);
        extra(side);
    }
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

#[test]
fn symmetric_ranges_exclude_the_merge_bases_as_stock_does() {
    compare(
        &[
            &["last-modified", "-r", "main...topic"],
            &["last-modified", "-r", "topic...main"],
            &["last-modified", "-r", "main~1...main"],
            &["last-modified", "-r", "-t", "main...main~2", "d"],
            &["last-modified", "main...main"],
            &["last-modified", "..."],
            &["last-modified", "-r", "...main~1"],
            &["last-modified", "-r", "main~1..."],
            &["last-modified", "-r", "t...main"],
            &["last-modified", "-r", "side...main"],
            &["last-modified", "-r", "cx...cy"],
            &["last-modified", "-r", "--not", "cx...cy"],
            &["last-modified", "-r", "cy...cy~1"],
            &["last-modified", "-r", "--not", "main...topic"],
            &["last-modified", "tt...main"],
            &["last-modified", "main...tt"],
            &["last-modified", "nosuch...main"],
            &["last-modified", "-r", "main...topic", "--", "d"],
        ],
        "last-modified-symmetric",
        |_| {},
    );
}

#[test]
fn parent_shorthands_pend_the_parents_as_stock_does() {
    compare(
        &[
            &["last-modified", "-r", "main^@"],
            &["last-modified", "-r", "main~1^@"],
            &["last-modified", "-r", "t^@"],
            &["last-modified", "-r", "^main^@"],
            &["last-modified", "-r", "main~3^@"],
            &["last-modified", "tt^@"],
            &["last-modified", "nosuch^@"],
            &["last-modified", "-r", "main^!"],
            &["last-modified", "-r", "main~1^!"],
            &["last-modified", "-r", "t^!"],
            &["last-modified", "-r", "topic^!"],
            &["last-modified", "-r", "^main^!"],
            &["last-modified", "-r", "main~3^!"],
            &["last-modified", "-r", "main^-"],
            &["last-modified", "-r", "main^-1"],
            &["last-modified", "-r", "main^-2"],
            &["last-modified", "main^-3"],
            &["last-modified", "main^-0"],
            &["last-modified", "main^-x"],
            &["last-modified", "-r", "cx^-2", "--", "x", "y"],
            &["last-modified", "-r", "--not", "main^-", "--not", "main"],
        ],
        "last-modified-parents",
        |_| {},
    );
}

#[test]
fn not_all_and_max_count_reach_the_walk_as_stock_does() {
    compare(
        &[
            &["last-modified", "--not", "main"],
            &["last-modified", "-r", "main", "--not", "topic"],
            &["last-modified", "-r", "main..topic", "--not"],
            &["last-modified", "-r", "--not", "main~2", "--not", "main"],
            &["last-modified", "--all"],
            &["last-modified", "-r", "main", "--not", "--all"],
            &["last-modified", "-r", "--not", "--all", "--not", "cx"],
            &["last-modified", "-n", "2"],
            &["last-modified", "-r", "-n2"],
            &["last-modified", "-r", "-rn1"],
            &["last-modified", "-r", "-2"],
            &["last-modified", "--max-count=0"],
            &["last-modified", "-r", "--max-count", "1", "topic"],
            &["last-modified", "-r", "-n", "-1"],
            &["last-modified", "-r", "-n", "1", "--", "a"],
            &["last-modified", "-r", "-n", "3", "main~1..main"],
            &["last-modified", "-n"],
            &["last-modified", "main", "-n"],
            &["last-modified", "-n", "--", "a"],
            &["last-modified", "-n", "x"],
            &["last-modified", "--max-count=x"],
            &["last-modified", "--foo", "-n", "1"],
            &["last-modified", "a", "--not"],
            &["last-modified", "a", "--foo"],
            &["last-modified", "-"],
            &["last-modified", "nosuch", "--", "a"],
        ],
        "last-modified-not-all-count",
        |_| {},
    );
}

#[test]
fn all_pends_the_other_worktrees_heads_as_stock_does() {
    compare(
        &[
            &["last-modified", "--all"],
            &["last-modified", "-r", "main", "--not", "--all"],
            &["last-modified", "-r", "--not", "--all", "--not", "HEAD"],
        ],
        "last-modified-all-worktrees",
        |side| {
            let wt = side.root.join("wt");
            let out = side.git(&["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), "topic~1"]);
            assert_eq!(out.code, 0, "{out:?}");
            // Every ref but the main worktree's HEAD and the other worktree's: `--all` then
            // pends two different commits that `--not` can exclude.
            for name in [
                "refs/heads/topic",
                "refs/heads/side",
                "refs/heads/cx",
                "refs/heads/cy",
                "refs/tags/t",
                "refs/tags/tt",
            ] {
                let out = side.git(&["update-ref", "-d", name]);
                assert_eq!(out.code, 0, "{out:?}");
            }
        },
    );
}
