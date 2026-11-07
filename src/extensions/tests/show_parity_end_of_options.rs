//! `--end-of-options` ends option parsing for every command that hands its argv
//! to `setup_revisions()`: past it, a word that starts with `-` is a revision
//! (revision.c:3077, :3100-3103), so a branch called `-x` can be named. The
//! commands reach that point differently — parse_options() keeps the word under
//! PARSE_OPT_KEEP_UNKNOWN_OPT (show, whatchanged, format-patch, cherry-pick) or
//! consumes it (shortlog, blame, grep, cherry) — and each spelling is compared
//! with stock git here.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// Both worlds with a branch `-x` at `main~1` (`two`).
fn pair(label: &str, stock: &str) -> (twin_repo::Side, twin_repo::Side) {
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        let made = side.git(&["update-ref", "refs/heads/-x", "main~1"]);
        assert_eq!(made.code, 0, "{made:?}");
    }
    (s, z)
}

#[test]
fn a_dash_revision_after_end_of_options_reads_as_a_revision() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = pair("eoo-read", stock);
    for args in [
        &["show", "--end-of-options", "-x"][..],
        &["show", "-s", "--end-of-options", "-x", "--", "a"],
        &["show", "--end-of-options", "-x^!"],
        &["show", "--end-of-options", "-sp"],
        &["show", "--end-of-options", "-h"],
        &["show", "--end-of-options", "--stat"],
        &["whatchanged", "--i-still-use-this", "--end-of-options", "-x"],
        &["whatchanged", "--end-of-options", "--i-still-use-this"],
        &["log", "--oneline", "--end-of-options", "-x"],
        &["diff", "--end-of-options", "-x"],
        &["diff", "--stat", "--end-of-options", "-x", "main", "--", "a"],
        &["diff", "--end-of-options", "--stat"],
        &["diff-tree", "-r", "--end-of-options", "-x", "main"],
        &["diff-tree", "-p", "--end-of-options", "-x"],
        &["diff-tree", "--end-of-options", "--stat"],
        &["format-patch", "--stdout", "--end-of-options", "-x"],
        &["format-patch", "--stdout", "--end-of-options", "--stat"],
        &["shortlog", "--end-of-options", "-x"],
        &["shortlog", "--end-of-options", "--numbered", "main"],
        &["shortlog", "--end-of-options", "-n", "1", "main"],
        &["shortlog", "-s", "--end-of-options", "--all"],
        &["blame", "--end-of-options", "-x", "a"],
        &["blame", "--end-of-options", "main~2", "a"],
        &["blame", "--end-of-options", "a"],
        &["blame", "--end-of-options", "a", "main~1"],
        &["blame", "--end-of-options", "nosuch", "a"],
        &["grep", "--end-of-options", "-x", "main"],
        &["grep", "-n", "--end-of-options", "2", "main", "--", "a"],
        &["cherry", "--end-of-options", "-x"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}

#[test]
fn cherry_pick_takes_a_dash_revision_after_end_of_options() {
    let Some(stock) = stock_git::stock_git() else { return };
    for args in [
        &["cherry-pick", "-n", "--end-of-options", "-x"][..],
        &["cherry-pick", "--end-of-options", "--", "side"],
        &["revert", "--end-of-options", "-x"],
    ] {
        let (s, z) = pair("eoo-pick", stock);
        assert_eq!(z.git(args), s.git(args), "{args:?}");
        // The state each side was left in, read by stock git in both.
        for probe in [&["status", "--porcelain"][..], &["log", "--format=%s", "-3"]] {
            let read = |side: &twin_repo::Side| {
                let out = std::process::Command::new(stock)
                    .args(probe)
                    .current_dir(side.repo())
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&out.stdout).into_owned()
            };
            assert_eq!(read(&z), read(&s), "{args:?} then {probe:?}");
        }
    }
}
