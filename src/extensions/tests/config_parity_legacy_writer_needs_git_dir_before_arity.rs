//! The legacy writers (`--add`, `--replace-all`, `--unset`, `--unset-all`,
//! `--remove-section`) call `check_write()` ahead of `check_argc()`, so outside a repository
//! `fatal: not in a git directory` (128) wins over the arity error (129). The `set`/`unset`
//! subcommands count their operands first. zvcs checked arity first for the legacy forms.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

#[test]
fn a_legacy_writer_outside_a_repository_dies_before_counting_operands() {
    let Some((s, z)) = world("config-writer-nongit") else { return };
    // A directory with no repository above it, honouring the same ceiling for both sides.
    let bare = |side: &Side| {
        let dir = side.root.join("plain");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let run = |side: &Side, args: &[&str]| {
        side.run_in(&bare(side), &[("GIT_CEILING_DIRECTORIES", side.root.to_str().unwrap())], args)
    };
    for args in [
        &["config", "--unset", "a", "b", "c"][..],
        &["config", "--unset-all", "a", "b", "c"],
        &["config", "--add", "a"],
        &["config", "--replace-all", "a"],
        &["config", "--remove-section", "a", "b"],
        &["config", "--unset"],
        &["config", "unset", "a", "b"],
        &["config", "--get-all"],
        &["config", "--list", "x"],
    ] {
        assert_eq!(run(&z, args), run(&s, args), "{args:?}");
    }
    let want = run(&s, &["config", "--unset", "a", "b", "c"]);
    assert_eq!((want.code, want.stderr.as_str()), (128, "fatal: not in a git directory\n"));
    let sub = run(&s, &["config", "unset", "a", "b"]);
    assert_eq!(sub.code, 129, "{sub:?}");
}

#[test]
fn inside_a_repository_the_arity_error_stands() {
    let Some((s, z)) = world("config-writer-repo") else { return };
    for args in [&["config", "--unset", "a", "b", "c"][..], &["config", "--add", "a"]] {
        let want = s.git(args);
        assert_eq!(want.code, 129, "{want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
    }
}
