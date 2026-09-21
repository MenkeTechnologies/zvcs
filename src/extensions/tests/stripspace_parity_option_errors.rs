//! `git stripspace` rejects three different kinds of bad command line, and only
//! one of them prints the usage block.
//!
//! `parse_options()` switches on what `parse_options_step()` returned:
//!
//! ```c
//! case PARSE_OPT_HELP:
//! case PARSE_OPT_ERROR:
//!         exit(129);
//! …
//! case PARSE_OPT_UNKNOWN:
//!         if (ctx.argv[0][1] == '-') {
//!                 error(_("unknown option `%s'"), ctx.argv[0] + 2);
//!         } else if (isascii(*ctx.opt)) {
//!                 error(_("unknown switch `%c'"), *ctx.opt);
//!         }
//!         …
//!         usage_with_options(usagestr, options);
//! }
//! ```
//!
//! (parse-options.c:1198-1224.) The usage block hangs off `PARSE_OPT_UNKNOWN`
//! alone. `PARSE_OPT_ERROR` — which is what `error()` returns from
//! `do_get_value()` when a `PARSE_OPT_NOARG` option is handed an `=<value>`
//! (parse-options.c:142-143) — exits on the message by itself. The `OPT_CMDMODE`
//! conflict between `-s` and `-c` is a third shape again: its message names each
//! option with the spelling the command line used, and it too prints no block.
//!
//! Because the same three shapes carry across every parse-options command, a
//! port that reaches for one "usage error" helper gets two of the three wrong,
//! which is the regression this pins.
//!
//! Every expectation below was measured from stock git 2.55.0.
#![cfg(unix)]

use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The first two lines of git's `stripspace` usage block; enough to tell
/// "printed the block" from "did not" without pinning the whole option table.
const USAGE_HEAD: &str = "usage: git stripspace [-s | --strip-comments]\n\
                          \x20  or: git stripspace [-c | --comment-lines]\n";

fn stripspace(args: &[&str]) -> Output {
    let mut full = vec!["stripspace"];
    full.extend_from_slice(args);
    Command::new(BIN)
        .args(&full)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_noarg_option_given_a_value_prints_the_error_alone() {
    for (args, name) in [
        (["--strip-comments=1"], "strip-comments"),
        (["--comment-lines=x"], "comment-lines"),
        // An unambiguous abbreviation resolves first, so the message names the
        // option in full even though the command line spelled it short.
        (["--strip=1"], "strip-comments"),
    ] {
        let out = stripspace(&args);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} wrote stdout: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("error: option `{name}' takes no value\n"),
            "PARSE_OPT_ERROR exits on the message; no usage block follows"
        );
    }
}

#[test]
fn an_unknown_option_is_the_one_shape_that_prints_the_usage_block() {
    let out = stripspace(&["--bogus"]);
    assert_eq!(out.status.code(), Some(129));
    let err = String::from_utf8_lossy(&out.stderr);
    // `ctx.argv[0] + 2`: the leading dashes are not part of the name.
    assert!(
        err.starts_with("error: unknown option `bogus'\n"),
        "unexpected stderr: {err}"
    );
    assert!(err.contains(USAGE_HEAD), "usage block missing from: {err}");

    // The short-option arm of the same case reports the character, not the word.
    let out = stripspace(&["-z"]);
    assert_eq!(out.status.code(), Some(129));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("error: unknown switch `z'\n"),
        "unexpected stderr: {err}"
    );
    assert!(err.contains(USAGE_HEAD), "usage block missing from: {err}");
}

#[test]
fn the_cmdmode_conflict_names_the_spellings_it_was_given() {
    // `OPT_CMDMODE` reports the option being set first and the one already set
    // second, each as the command line wrote it.
    let out = stripspace(&["-s", "-c"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: options '-c' and '-s' cannot be used together\n"
    );

    let out = stripspace(&["--comment-lines", "--strip-comments"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: options '--strip-comments' and '--comment-lines' cannot be used together\n"
    );

    // Repeating one mode is not a conflict — it selects the same value again.
    let out = stripspace(&["-s", "-s"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
}
