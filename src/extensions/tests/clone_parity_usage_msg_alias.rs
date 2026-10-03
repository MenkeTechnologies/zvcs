//! The block `git clone`'s argument-count refusals print renders `--recursive`
//! as declared.
//!
//! `usage_msg_opt()` after `parse_options()` renders `builtin_clone_options`
//! itself, whose `OPT_ALIAS(0, "recursive", "recurse-submodules")` carries no
//! argument help: `preprocess_options()` (parse-options.c:899-960) only fills that
//! in on the copy `parse_options()` works on. So `-h` and an unknown option show
//! `--[no-]recursive[=<pathspec>]` on a line of its own, while `Too many
//! arguments.` and `You must specify a repository to clone.` show
//! `--[no-]recursive ...  alias of --recurse-submodules` (`usage_argh()`'s
//! `...`, parse-options.c:1296). Expectations captured from stock git 2.56.0.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn stderr(args: &[&str]) -> (String, i32) {
    let dir = std::env::temp_dir();
    let out = Command::new(BIN)
        .args(args)
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
}

#[test]
fn argument_count_refusals_render_the_alias_unprocessed() {
    let alias = "\n    --[no-]recursive ...  alias of --recurse-submodules\n    -j, ";
    for (args, first) in [
        (&["clone", "a", "b", "c"][..], "fatal: Too many arguments.\n\n"),
        (&["clone"][..], "fatal: You must specify a repository to clone.\n\n"),
    ] {
        let (err, code) = stderr(args);
        assert_eq!(code, 129);
        assert!(err.starts_with(first), "{err}");
        assert!(err.contains(alias), "{err}");
    }
    let (err, code) = stderr(&["clone", "--bogus"]);
    assert_eq!(code, 129);
    assert!(err.contains("\n    --[no-]recursive[=<pathspec>]\n                          alias of --recurse-submodules\n"));
}
