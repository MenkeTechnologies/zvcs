//! `log.diffMerges` on `log`, `show`, `whatchanged` and `range-diff`.
//!
//! `git_log_config()` hands the value to `diff_merges_config()`
//! (builtin/log.c:498-502, diff-merges.c:103-112), which moves `set_to_default`
//! — the setter `-m` and `--diff-merges=on|m` run (diff-merges.c:84-85, :125-127).
//! zvcs did not read the key, so `-m` was always `separate`, and a value
//! `func_by_opt()` refuses never ended the command with
//! `git_die_config_linenr()`'s text.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// The twin history plus a merge of `side` (which also adds `b`) into `main`.
fn merged(label: &str, stock: &str) -> (twin_repo::Side, twin_repo::Side) {
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        side.git(&["checkout", "-q", "side"]);
        side.write("b", "b\n");
        side.git(&["add", "b"]);
        side.git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "b"]);
        side.git(&["checkout", "-q", "main"]);
        let merge = side.git(&["-c", "maintenance.auto=false", "merge", "-q", "-X", "ours", "--no-edit", "side"]);
        assert_eq!(merge.code, 0, "{merge:?}");
    }
    (s, z)
}

#[test]
fn configured_default_is_what_dash_m_and_on_select() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = merged("diff-merges-config", stock);
    for value in ["first-parent", "combined", "dense-combined", "remerge", "off", "on"] {
        let key = format!("log.diffMerges={value}");
        for args in [
            &["log", "-1", "-m", "-p"][..],
            &["log", "-1", "--diff-merges=on"],
            &["log", "-1", "--diff-merges=m", "--stat"],
            &["log", "-1", "--cc"],
            &["show", "-m"],
            &["show", "--diff-merges=on", "--first-parent"],
        ] {
            let args: Vec<&str> = ["-c", key.as_str()].iter().chain(args).copied().collect();
            assert_eq!(z.git(&args), s.git(&args), "{args:?}");
        }
    }
    let args = ["-c", "log.diffMerges=first-parent", "range-diff", "--diff-merges=on", "main~1...side"];
    assert_eq!(z.git(&args), s.git(&args), "{args:?}");
}

#[test]
fn a_refused_value_ends_the_command() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("diff-merges-config-bad", stock);
    for args in [
        &["-c", "log.diffMerges=bogus", "log", "-1"][..],
        &["-c", "log.diffMerges", "show"],
        &["-c", "log.diffMerges=bogus", "-c", "log.diffMerges=cc", "show", "-h"],
        &["-c", "log.diffMerges=bogus", "log", "-h"],
        &["-c", "log.diffMerges=bogus", "whatchanged", "-1"],
        &["-c", "log.diffMerges=bogus", "reflog", "show"],
        &["-c", "log.diffMerges=bogus", "rev-list", "-1", "HEAD"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
    // A file value is named by its line, and an earlier bad line is not saved by
    // a later good one.
    for side in [&s, &z] {
        side.git(&["config", "log.diffMerges", "bogus"]);
        side.git(&["config", "--add", "log.diffMerges", "separate"]);
    }
    assert_eq!(z.git(&["log", "-1", "-m"]), s.git(&["log", "-1", "-m"]));
}
