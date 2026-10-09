//! The `RUN_SETUP` gate walks up from the working directory like setup does, so a
//! `GIT_CEILING_DIRECTORIES` naming the repository hides it: the command dies at 128 with
//! "not a git repository" before it parses a single option, instead of reaching its own usage
//! error or running.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_ceiling_naming_the_repository_hides_it_from_the_setup_gate() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("ceiling-setup-gate", stock);
    for side in [&s, &z] {
        std::fs::create_dir_all(side.repo().join("sub")).unwrap();
    }
    for args in [&["replay"][..], &["replay", "--bogus"], &["fast-export"], &["log"], &["repack", "--bogus"]] {
        let run = |side: &twin_repo::Side| {
            let ceiling = side.repo().to_string_lossy().into_owned();
            side.run_in(&side.repo().join("sub"), &[("GIT_CEILING_DIRECTORIES", &ceiling)], args)
        };
        let want = run(&s);
        assert_eq!(want.code, 128, "{args:?}: {want:?}");
        assert!(want.stderr.starts_with("fatal: not a git repository"), "{args:?}: {want:?}");
        assert_eq!(run(&z), want, "{args:?}");
    }
}
