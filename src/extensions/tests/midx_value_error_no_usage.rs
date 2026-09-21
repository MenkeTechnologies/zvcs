//! `git multi-pack-index`: which parse failures print the usage block and which
//! print the `error:` line alone.
//!
//! parse-options draws the line, not the command:
//!
//! ```c
//! switch (parse_options_step(&ctx, options, usagestr)) {
//! case PARSE_OPT_HELP:
//! case PARSE_OPT_ERROR:
//!         exit(129);
//! [...]
//! case PARSE_OPT_UNKNOWN:
//!         if (ctx.argv[0][1] == '-') {
//!                 error(_("unknown option `%s'"), ctx.argv[0] + 2);
//!         } else if (isascii(*ctx.opt)) {
//!                 error(_("unknown switch `%c'"), *ctx.opt);
//!         } [...]
//!         usage_with_options(usagestr, options);
//! }
//! ```
//!
//! (parse-options.c:1198-1224.) A *value* failure — `opterror()`'s
//! `option `<name>' requires a value`, and the `OPT_MAGNITUDE` value
//! diagnostics — is `PARSE_OPT_ERROR` and exits straight away, so no usage
//! follows it. Only an unrecognised option reaches `usage_with_options()`, along
//! with `need a subcommand` (:1208-1211) and the validations a command runs
//! through `usage_with_options()` itself.
//!
//! All of it measured against git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-midxerr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    let out = run(&dir, &["init", "-q", "-b", "main"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    dir
}

/// `--batch-size`'s `OPT_MAGNITUDE` callback and the missing-value path both
/// stop at `PARSE_OPT_ERROR`, so the `error:` line is the whole of stderr.
#[test]
fn a_bad_batch_size_prints_no_usage_block() {
    let dir = fixture("batch");
    const MAGNITUDE: &str = "error: option `batch-size' expects a non-negative integer value \
                             with an optional k/m/g suffix\n";
    for value in ["abc", "-1", "1x"] {
        let out = run(&dir, &["multi-pack-index", "repack", &format!("--batch-size={value}")]);
        assert_eq!(out.status.code(), Some(129), "--batch-size={value}");
        assert_eq!(stderr_of(&out), MAGNITUDE, "--batch-size={value}");
    }
    let empty = run(&dir, &["multi-pack-index", "repack", "--batch-size="]);
    assert_eq!(stderr_of(&empty), "error: option `batch-size' expects a numerical value\n");

    let missing = run(&dir, &["multi-pack-index", "repack", "--batch-size"]);
    assert_eq!(missing.status.code(), Some(129));
    assert_eq!(stderr_of(&missing), "error: option `batch-size' requires a value\n");
    std::fs::remove_dir_all(&dir).ok();
}

/// The same rule for every `OPT_STRING` the command has, at the top level and in
/// each subcommand — the usage block a missing value used to drag in differed
/// per subcommand, so each one is checked.
#[test]
fn a_missing_option_value_prints_no_usage_block() {
    let dir = fixture("value");
    for (args, name) in [
        (vec!["multi-pack-index", "--object-dir"], "object-dir"),
        (vec!["multi-pack-index", "write", "--object-dir"], "object-dir"),
        (vec!["multi-pack-index", "verify", "--object-dir"], "object-dir"),
        (vec!["multi-pack-index", "expire", "--object-dir"], "object-dir"),
        (vec!["multi-pack-index", "repack", "--object-dir"], "object-dir"),
        (vec!["multi-pack-index", "write", "--preferred-pack"], "preferred-pack"),
        (vec!["multi-pack-index", "write", "--refs-snapshot"], "refs-snapshot"),
        (vec!["multi-pack-index", "compact", "--base"], "base"),
    ] {
        let out = run(&dir, &args);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert_eq!(stderr_of(&out), format!("error: option `{name}' requires a value\n"), "{args:?}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// The other half of the rule, so a fix for the above cannot quietly strip the
/// usage block from the failures that are supposed to carry one.
#[test]
fn unknown_options_and_subcommand_errors_still_carry_the_usage_block() {
    let dir = fixture("usage");
    for args in [
        vec!["multi-pack-index", "repack", "--bogus"],
        vec!["multi-pack-index", "repack", "-x"],
        vec!["multi-pack-index", "--bogus"],
        vec!["multi-pack-index", "nosuch"],
        vec!["multi-pack-index"],
        vec!["multi-pack-index", "write", "--no-write-chain-file"],
    ] {
        let out = run(&dir, &args);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert!(
            stderr_of(&out).contains("usage: git multi-pack-index"),
            "{args:?} lost its usage block: {}",
            stderr_of(&out)
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}
