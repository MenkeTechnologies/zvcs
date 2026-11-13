//! `git mktag` reads its `fsck.*` configuration through `git_fsck_config()`
//! (fsck.c:1440-1469), one variable at a time in config order:
//!
//! * `fsck.skiplist` is opened by `oidset_parse_file()`, which dies
//!   `could not open object name list: <path>` (fsck.c:1446-1458);
//! * every other `fsck.<id>` is `fsck_set_msg_type()` (fsck.c:154-181), which dies
//!   `Unhandled message id: <id>` for an id fsck does not know — of *any* object
//!   type, not only the tag's — and `Unknown fsck message type: '<v>'` for a value
//!   other than `error`, `warn` and `ignore` (`info` included);
//! * a valueless `fsck.<id>` is `config_error_nonbool()`.
//!
//! zvcs consulted only the rows a tag can report, so a misspelt id or a missing
//! skip list wrote the tag, and it accepted `info`.
//!
//! A `type` line that names no type is `FSCK_MSG_BAD_TYPE`, and only a report that
//! answers 1 stops the parse (fsck.c:1061-1065). With `fsck.badType=ignore` the tag
//! goes on to `verify_object_in_tag()`, which dies naming the tagged type as
//! `type_name(OBJ_BAD)` — NULL, printed `(null)` (builtin/mktag.c:62-65). zvcs
//! died with a sentence of its own.
//!
//! `--end-of-options` ends the options as `--` does (parse-options.c:1119-1124).
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

/// `git <args>` in `side`'s repository with `input` on stdin, the repository
/// root replaced by `<root>`.
fn run(side: &twin_repo::Side, args: &[&str], input: &str) -> (i32, String, String) {
    let mut child = Command::new(&side.bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let root = side.root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
}

fn tag(object: &str, kind: &str) -> String {
    format!("object {object}\ntype {kind}\ntag v9\ntagger T <t@example.com> 1700000000 +0000\n\nmsg\n")
}

#[test]
fn fsck_config_is_read_as_git_fsck_config_reads_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mktag-fsck-config", stock);
    let head = s.git(&["rev-parse", "HEAD"]).stdout.trim().to_owned();
    let good = tag(&head, "commit");
    let cases: &[&[&str]] = &[
        &["-c", "fsck.bogusThing=ignore", "mktag"],
        &["-c", "fsck.skipList=/nonexistent/skip", "mktag"],
        &["-c", "fsck.badTagName=info", "mktag"],
        &["-c", "fsck.badParentSha1=nope", "mktag"],
        &["-c", "fsck.badParentSha1=ignore", "mktag"],
        &["-c", "fsck.badTagName=bogus", "-c", "fsck.zzz=ignore", "mktag"],
        &["-c", "fsck.zzz=ignore", "-c", "fsck.badTagName=bogus", "mktag"],
        &["-c", "fsck.largePathname=warn:100", "mktag"],
        &["-c", "fsck.badTagName", "mktag"],
        &["mktag", "--end-of-options"],
    ];
    for args in cases {
        assert_eq!(run(&z, args, &good), run(&s, args, &good), "{args:?}");
    }
    // Neither side may have written a tag for the refused runs; the accepted
    // ones wrote the same object.
    let count = |side: &twin_repo::Side| side.git(&["count-objects", "-v"]).stdout;
    assert_eq!(count(&z), count(&s));
}

#[test]
fn a_demoted_bad_type_reaches_the_tagged_object_check() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("mktag-bad-type", stock);
    let head = s.git(&["rev-parse", "HEAD"]).stdout.trim().to_owned();
    for (args, input) in [
        (&["-c", "fsck.badType=ignore", "mktag"][..], tag(&head, "blub")),
        (&["-c", "fsck.badType=warn", "mktag", "--no-strict"], tag(&head, "blub")),
        (&["-c", "fsck.badType=ignore", "mktag"], tag(&"d".repeat(40), "blub")),
    ] {
        assert_eq!(run(&z, args, &input), run(&s, args, &input), "{args:?}");
    }
}
