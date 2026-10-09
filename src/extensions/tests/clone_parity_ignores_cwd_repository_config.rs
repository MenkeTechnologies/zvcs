//! `clone` has no setup flag in git.c, so the repository the working directory sits in is never
//! opened: its `repo_config()` read sees system, global and command-line values only. A bad value
//! in that repository's own `.git/config` therefore changes nothing about `clone` - the usage
//! error still wins - while the same bad value on the command line (`-c`) still kills it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

fn append_config(side: &twin_repo::Side, text: &str) {
    let mut config = String::from_utf8(side.read(".git/config").unwrap()).unwrap();
    config.push_str(text);
    side.write(".git/config", &config);
}

#[test]
fn a_bad_value_in_the_working_directorys_repository_does_not_reach_clone() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("clone-cwd-config", stock);
    for side in [&s, &z] {
        append_config(side, "[core]\n\tautocrlf = \" \"\n[branch]\n\tautoSetupMerge = =\n");
    }
    for args in [
        &["clone", "./.git", "--bogus-option", "dest"][..],
        &["clone", "-4", "--mirror", "--quiet"],
        &["clone"],
    ] {
        let want = s.git(args);
        assert_eq!(want.code, 129, "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
}

#[test]
fn a_bad_value_on_the_command_line_still_reaches_clone() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("clone-cmdline-config", stock);
    let args = ["-c", "core.autocrlf= ", "clone", "./.git", "--bogus-option", "dest"];
    let want = s.git(&args);
    assert_eq!(want.code, 128, "{want:?}");
    assert_eq!(z.git(&args), want);
}
