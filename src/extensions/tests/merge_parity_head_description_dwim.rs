//! `git merge` against stock git: the default merge message names each head the way
//! `merge_name()` does — by the full name `dwim_ref()` lands on with its category prefix cut
//! off, not by the spelling the command line used. `@{-1}` is the branch it names,
//! `heads/other` and `refs/heads/other` are `other`, and `tags/t1` is the tag `t1`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn heads_are_described_by_the_name_dwim_lands_on() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("merge-head-description", stock);
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "-b", "other", "main~2"]);
        side.write("b", "b\n");
        side.git(&["add", "b"]);
        side.git(&["commit", "-q", "-m", "other"]);
        side.git(&["tag", "t1"]);
        side.git(&["checkout", "-q", "-b", "third", "main~2"]);
        side.write("c", "c\n");
        side.git(&["add", "c"]);
        side.git(&["commit", "-q", "-m", "third"]);
        side.git(&["checkout", "-q", "other"]);
        side.git(&["checkout", "-q", "main"]);
    }
    let message = |side: &twin_repo::Side| side.git(&["log", "-1", "--format=%B"]).stdout;
    for specs in [
        &["@{-1}"][..],
        &["heads/other"],
        &["refs/heads/other"],
        &["other"],
        &["tags/t1"],
        &["t1"],
        &["other", "@{-1}"],
        &["other", "third"],
        &["third", "t1", "refs/heads/other"],
    ] {
        for side in [&s, &z] {
            side.git(&["merge", "--abort"]);
            side.git(&["reset", "-q", "--hard", "main"]);
            side.git(&["checkout", "-q", "other"]);
            side.git(&["checkout", "-q", "main"]);
        }
        let mut args = vec!["merge", "--no-ff", "--no-edit"];
        args.extend_from_slice(specs);
        assert_eq!(z.git(&args).code, s.git(&args).code, "{args:?}");
        assert_eq!(message(&z), message(&s), "{args:?}");
    }
}
