//! `git hook` against stock git: only `hook run` reads the configuration.
//!
//! `cmd_hook()` itself is a bare `parse_options()` over subcommands; `cmd_hook_run()` is
//! the one that calls `repo_config()` (builtin/hook.c). So a value `git_default_config()`
//! refuses ends `hook run` at 128 but leaves the parse-options usage errors of a missing,
//! unknown or dashed subcommand at 129, and `hook list` is not gated by it either way.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn only_run_is_refused_by_a_bad_default_config_value() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("hook-config", stock);
    for side in [&s, &z] {
        side.write("../bad.cfg", "[core]\n\tfileMode = always\n");
    }
    let cases: &[&[&str]] = &[
        &["hook"],
        &["hook", "parity-unknown-hook"],
        &["hook", "--to-stdin=README.md"],
        &["hook", "--no-allow-unknown-hook-name"],
        &["hook", "--", "run", "pre-push"],
        &["hook", "run", "pre-push"],
        &["hook", "run", "--no-allow-unknown-hook-name"],
        &["hook", "list", "pre-push"],
    ];
    for args in cases {
        let run = |side: &twin_repo::Side| {
            let cfg = side.root.join("bad.cfg");
            side.git_env(&[("GIT_CONFIG_GLOBAL", cfg.to_str().unwrap())], args)
        };
        assert_eq!(run(&z), run(&s), "{args:?}");
    }
}
