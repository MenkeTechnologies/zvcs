//! `git request-pull` is a shell script whose first diff-aware child is `git show -s`, and
//! `cmd_show()` goes through `repo_init_revisions()`, which reads `grep.*` and dies on a value
//! `grep_config()` refuses. The script's `|| status=1` turns that into exit 1 with nothing on
//! stdout - after the revision checks and the remote probe have had their say.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_bad_grep_pattern_type_ends_request_pull_with_the_childs_fatal() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("request-pull-grep-config", stock);
    for args in [
        &["-c", "grep.patternType=auto", "request-pull", "main~1", ".", "main"][..],
        &["-c", "grep.patternType=auto", "request-pull", "-p", "main~1", ".", "main"],
        &["-c", "grep.extendedRegexp=maybe", "request-pull", "main~1", ".", "main"],
        // The revision checks come first.
        &["-c", "grep.patternType=auto", "request-pull", "nosuch", ".", "main"],
        &["-c", "grep.patternType=auto", "request-pull", "main~1"],
    ] {
        let want = s.git(args);
        assert_eq!(z.git(args), want, "{args:?}");
    }
    let want = s.git(&["-c", "grep.patternType=auto", "request-pull", "main~1", ".", "main"]);
    assert_eq!((want.code, want.stdout.as_str()), (1, ""), "{want:?}");
    assert!(want.stderr.contains("bad grep.patterntype argument: auto"), "{want:?}");
}
