//! Started inside `.git`, discovery settles on the git directory itself
//! (`setup_bare_git_dir()`): git does not `chdir()` to the work tree, so a relative repository
//! operand such as `.` or `./` names the git directory and `enter_repo()` accepts it as is.
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

fn from_dir(t: &Twins, subdir: &str, args: &[&str]) -> config_twins::Outcome {
    let home = t.root.join("home");
    let stock = config_twins::run(t.stock_bin, &t.root.join("stock").join(subdir), &home, args);
    let zvcs = config_twins::run(config_twins::BIN, &t.root.join("zvcs").join(subdir), &home, args);
    assert_eq!(stock, zvcs, "`git {args:?}` in {subdir:?}: stock (left) vs zvcs (right)");
    stock
}

#[test]
fn a_dot_operand_inside_the_git_directory_names_that_directory() {
    let Some(t) = Twins::new("ls-remote-in-gitdir") else { return };
    for args in [
        &["ls-remote", "."][..],
        &["ls-remote", "./"][..],
        &["ls-remote", "-b", "--", "./", "HEAD"][..],
        &["ls-remote", "--heads", "."][..],
    ] {
        let stock = from_dir(&t, ".git", args);
        assert_eq!(stock.code, 0, "{args:?}: {stock:?}");
    }
    let listed = from_dir(&t, ".git", &["ls-remote", "."]);
    assert!(listed.stdout.contains("refs/heads/main"), "{listed:?}");
}

#[test]
fn a_parent_operand_from_below_the_git_directory_keeps_working() {
    let Some(t) = Twins::new("ls-remote-below-gitdir") else { return };
    let stock = from_dir(&t, ".git/refs", &["ls-remote", "../"]);
    assert_eq!(stock.code, 0, "{stock:?}");
}

#[test]
fn the_work_tree_root_still_resolves_dot_through_its_dot_git() {
    let Some(t) = Twins::new("ls-remote-at-root") else { return };
    let stock = from_dir(&t, "", &["ls-remote", "."]);
    assert_eq!(stock.code, 0, "{stock:?}");
    let stock = from_dir(&t, "", &["ls-remote", "./.git"]);
    assert_eq!(stock.code, 0, "{stock:?}");
}
