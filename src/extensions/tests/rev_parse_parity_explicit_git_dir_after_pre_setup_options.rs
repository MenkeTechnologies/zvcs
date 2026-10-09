//! `cmd_rev_parse()` sets up lazily: `--local-env-vars` and `--resolve-git-dir` are answered
//! before any repository is looked for, and `--parseopt` / `--sq-quote` as the first argument
//! likewise, so an explicit `$GIT_DIR` that names no repository is refused only at the first
//! argument past them.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn leading_pre_setup_options_print_before_the_missing_git_dir_is_refused() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("rev-parse-pre-setup", stock);
    for args in [
        &["--git-dir=no-such", "rev-parse", "--local-env-vars"][..],
        &["--git-dir=no-such", "rev-parse", "--local-env-vars", "--is-shallow-repository"],
        &["--git-dir=no-such", "rev-parse", "--local-env-vars", "HEAD"],
        &["--git-dir=no-such", "rev-parse", "--local-env-vars", "--local-env-vars", "--disambiguate=ab"],
        &["--git-dir=no-such", "rev-parse", "--resolve-git-dir", ".git", "--git-dir"],
        &["--git-dir=no-such", "rev-parse", "--sq-quote", "a", "b", "--is-shallow-repository"],
        &["--git-dir=no-such", "rev-parse", "--is-shallow-repository", "--local-env-vars"],
        &["--git-dir=no-such", "rev-parse", "--git-dir"],
    ] {
        assert_eq!(z.git(args), s.git(args), "{args:?}");
    }
}
