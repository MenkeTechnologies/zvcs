//! `git merge` against stock git: the `Conflicts:` hint written into `MERGE_MSG` is
//! commented with `comment_line_str`, which is `core.commentString` and `core.commentChar`
//! read together, last one wins — not `core.commentChar` alone.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn the_conflict_hint_uses_the_configured_comment_string() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("merge-comment-string", stock);
    let settings: &[&[&str]] = &[
        &["-c", "core.commentString=;"],
        &["-c", "core.commentChar=%"],
        &["-c", "core.commentChar=%", "-c", "core.commentString=all"],
        &["-c", "core.commentString=all", "-c", "core.commentChar=%"],
        &[],
    ];
    for cfg in settings {
        for mode in [&["merge", "side"][..], &["merge", "--squash", "side"]] {
            for side in [&s, &z] {
                side.git(&["merge", "--abort"]);
                side.git(&["reset", "-q", "--hard", "main"]);
                let _ = std::fs::remove_file(side.repo().join(".git/SQUASH_MSG"));
                let _ = std::fs::remove_file(side.repo().join(".git/MERGE_MSG"));
            }
            let args: Vec<&str> = cfg.iter().chain(mode.iter()).copied().collect();
            assert_eq!(z.git(&args), s.git(&args), "{args:?}");
            for file in [".git/MERGE_MSG", ".git/SQUASH_MSG"] {
                assert_eq!(z.read(file), s.read(file), "{file} after {args:?}");
            }
        }
    }
}
