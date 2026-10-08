//! `git merge-ours` against stock git: `HEAD` is resolved through `repo_get_oid()`, whose ref
//! dwim asks `core.warnAmbiguousRefs` — read with `git_config_bool()`, so an unreadable value
//! ends the command at 128.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn merge_ours_dies_on_an_unreadable_warn_ambiguous_refs() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("merge-ours-warn", stock);
    for side in [&s, &z] {
        side.write("../bad.cfg", "[core]\n\twarnAmbiguousRefs = always\n");
    }
    let run = |side: &twin_repo::Side, args: &[&str]| {
        let cfg = side.root.join("bad.cfg");
        side.git_env(&[("GIT_CONFIG_GLOBAL", cfg.to_str().unwrap())], args)
    };
    for args in [&["merge-ours", "main", "--", "HEAD", "side"][..], &["merge-ours"]] {
        assert_eq!(run(&z, args), run(&s, args), "{args:?}");
    }
}
