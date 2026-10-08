//! `git stage` against stock git: an entry in git's command table pointing at `cmd_add()`
//! (`git-stage(1)`: "This is a synonym for git-add(1)"), so it answers whatever `git add` does —
//! the usage block, a `-N` run over tracked paths (which leaves their cache-tree nodes invalid),
//! `--refresh`, `--dry-run` and the pathspec errors.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

fn tree_extension(side: &twin_repo::Side) -> Option<Vec<u8>> {
    twin_repo::index_extension(&side.read(".git/index")?, b"TREE")
}

#[test]
fn stage_answers_as_add_does() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("stage-is-add", stock);
    for side in [&s, &z] {
        side.write("docs/guide.md", "guide\n");
        side.write("docs/keep.txt", "keep\n");
        side.git(&["add", "."]);
        side.git(&["commit", "-q", "-m", "docs"]);
        side.git(&["write-tree"]);
        side.write("docs/guide.md", "guide, edited\n");
        side.write("new.txt", "new\n");
    }
    for args in [
        &["stage", "-h"][..],
        &["stage", "-N", "docs/guide.md"],
        &["stage", "--dry-run", "."],
        &["stage", "--refresh"],
        &["stage", "no-such-path"],
        &["stage", "--bogus"],
        &["stage", "-v", "new.txt"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        assert_eq!(tree_extension(&z), tree_extension(&s), "cache tree after {args:?}");
        for side in [&s, &z] {
            side.git(&["reset", "-q"]);
            side.git(&["write-tree"]);
        }
    }
}
