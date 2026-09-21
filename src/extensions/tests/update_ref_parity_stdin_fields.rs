//! `git update-ref --stdin` parses each command's arguments off the command's
//! own line, one at a time, and every diagnostic it raises interpolates what is
//! left of that line.
//!
//! `parse_arg()`, `parse_refname()`, `parse_next_refname()`, `parse_next_arg()`
//! and `parse_next_oid()` (builtin/update-ref.c:43-244, v2.55.0) walk a single
//! `next` pointer, and the `parse_cmd_*` functions that drive them name the
//! command and the reference in almost every failure:
//!
//! ```c
//! die("%s %s: invalid <old-oid>: %s", command, refname, arg.buf);
//! die("update %s: missing <new-oid>", refname);
//! die("update %s: extra input: %s", refname, next);
//! die("%s %s: expected SP but got: %s", command, refname, *next);
//! ```
//!
//! The port used to split the whole line into fields up front and then report
//! generic failures — `zzz: not a valid old SHA1`, `update: wrong number of
//! arguments`, `unterminated quoted string` — which name neither the command nor
//! the reference, and which in one case (`symref-create <ref> <target> extra`)
//! did not fail at all: the trailing junk was dropped and the symref written.
//!
//! Every expectation here was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-update-ref-fields-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "subject"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn oid(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        assert!(out.status.success(), "rev-parse {spec}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `git update-ref --stdin` fed exactly `input`; returns stdout, stderr, code.
    fn stdin(&self, input: &str) -> (String, String, i32) {
        let mut child = self
            .cmd(&["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The one refusal `input` produces, asserting nothing reached stdout.
    fn refusal(&self, input: &str) -> String {
        let (out, err, code) = self.stdin(input);
        assert_eq!((out.as_str(), code), ("", 128), "input {input:?}");
        err
    }

    fn exists(&self, refname: &str) -> bool {
        self.cmd(&["rev-parse", "--verify", "-q", refname])
            .output()
            .unwrap()
            .status
            .success()
    }
}

/// A value slot that names nothing is reported by slot, command and reference —
/// `parse_next_oid()`'s `invalid` label (builtin/update-ref.c:233-237).
///
/// Which slot it is depends on how many the command has already read, which is
/// exactly what a field-at-a-time parse knows and a pre-split one does not:
/// `update <ref> <oid> zzz` has read `<new-oid>` already, so `zzz` is the
/// `<old-oid>`, while `update <ref> zzz` is still on `<new-oid>`.
#[test]
fn an_unresolvable_value_names_its_slot_command_and_ref() {
    let f = Fixture::new("slot");
    let head = f.oid("HEAD");

    assert_eq!(
        f.refusal(&format!("update refs/heads/t {head} zzz\n")),
        "fatal: update refs/heads/t: invalid <old-oid>: zzz\n"
    );
    assert_eq!(
        f.refusal("update refs/heads/t zzz\n"),
        "fatal: update refs/heads/t: invalid <new-oid>: zzz\n"
    );
    assert_eq!(
        f.refusal("create refs/heads/t zzz\n"),
        "fatal: create refs/heads/t: invalid <new-oid>: zzz\n"
    );
    assert_eq!(
        f.refusal("delete refs/heads/t zzz\n"),
        "fatal: delete refs/heads/t: invalid <old-oid>: zzz\n"
    );
    assert_eq!(
        f.refusal("verify refs/heads/t zzz\n"),
        "fatal: verify refs/heads/t: invalid <old-oid>: zzz\n"
    );
    assert!(!f.exists("refs/heads/t"), "nothing may be written");
}

/// A slot that is absent rather than unparseable, for the two commands that
/// require one (builtin/update-ref.c:320, :416).
#[test]
fn a_missing_new_oid_names_the_reference() {
    let f = Fixture::new("missing");
    assert_eq!(
        f.refusal("update refs/heads/t\n"),
        "fatal: update refs/heads/t: missing <new-oid>\n"
    );
    assert_eq!(
        f.refusal("create refs/heads/t\n"),
        "fatal: create refs/heads/t: missing <new-oid>\n"
    );
}

/// `parse_arg()` ends an unquoted argument at any `isspace()`, but only `' '` is
/// an accepted separator (builtin/update-ref.c:52-55, :187-189). A tab after the
/// reference therefore ends a *well-formed* name and then fails as a separator,
/// rather than making one tab-bearing name that no format check would accept.
#[test]
fn a_tab_separator_is_a_separator_error_not_a_bad_ref_name() {
    let f = Fixture::new("tab");
    let head = f.oid("HEAD");
    assert_eq!(
        f.refusal(&format!("update refs/heads/t\t{head}\n")),
        format!("fatal: update refs/heads/t: expected SP but got: \t{head}\n\n")
    );
    // The same byte between a `symref-*` command's two names is reported by
    // `parse_next_refname()`, which names neither command nor reference
    // (builtin/update-ref.c:101-102).
    assert_eq!(
        f.refusal("option no-deref\nsymref-verify refs/heads/main\tx\n"),
        "fatal: expected SP but got: \tx\n\n"
    );
}

/// Every `parse_cmd_*` that takes arguments ends with `if (*next !=
/// line_termination) die("<cmd> %s: extra input: %s", refname, next)`. The
/// remainder it interpolates starts at the separator, so the reported text opens
/// with a space.
///
/// `symref-create` is the case that was not merely worded differently: the port
/// dropped the trailing junk and wrote the symref, exit 0.
#[test]
fn trailing_input_is_refused_and_quotes_the_remainder() {
    let f = Fixture::new("extra");
    let head = f.oid("HEAD");

    assert_eq!(
        f.refusal(&format!("update refs/heads/t {head} {head} extra\n")),
        "fatal: update refs/heads/t: extra input:  extra\n\n"
    );
    assert_eq!(
        f.refusal("symref-create refs/heads/s refs/heads/main extra\n"),
        "fatal: symref-create refs/heads/s: extra input:  extra\n\n"
    );
    assert!(!f.exists("refs/heads/s"), "the symref must not be written");
}

/// `parse_arg()`'s two quoting failures name the argument from its opening quote
/// to the end of the line (builtin/update-ref.c:45-51), terminator included, so
/// `die()`'s own newline makes the stderr end in a blank line.
#[test]
fn a_malformed_quoted_argument_quotes_it_from_the_opening_quote() {
    let f = Fixture::new("quote");
    assert_eq!(
        f.refusal("update refs/heads/t \"unterminated\n"),
        "fatal: badly quoted argument: \"unterminated\n\n"
    );
    assert_eq!(
        f.refusal("update refs/heads/t \"bad\\qescape\"\n"),
        "fatal: badly quoted argument: \"bad\\qescape\"\n\n"
    );
}

/// `parse_cmd_option()` accepts `no-deref` only when the terminator follows it
/// immediately, and otherwise reports the raw remainder of the line
/// (builtin/update-ref.c:601-610) — not a re-rendered option name.
#[test]
fn an_unusable_option_line_is_quoted_verbatim() {
    let f = Fixture::new("option");
    assert_eq!(f.refusal("option bogus\n"), "fatal: option unknown: bogus\n\n");
    assert_eq!(
        f.refusal("option no-deref extra\n"),
        "fatal: option unknown: no-deref extra\n\n"
    );
    // A second space is part of the remainder, so `no-deref` no longer starts it.
    assert_eq!(
        f.refusal("option  no-deref\n"),
        "fatal: option unknown:  no-deref\n\n"
    );
    // Unterminated: `*rest` is the buffer's NUL rather than the newline.
    assert_eq!(
        f.refusal("option no-deref"),
        "fatal: option unknown: no-deref\n"
    );
}

/// `parse_cmd_symref_update()` reads its optional old value as two plain
/// arguments and has its own wording for each way they can be wrong
/// (builtin/update-ref.c:357-378) — none of it routed through
/// `parse_next_oid()`, so none of it carries that function's messages.
#[test]
fn symref_update_words_each_half_of_its_old_value() {
    let f = Fixture::new("symref");
    assert_eq!(
        f.refusal("symref-update refs/heads/s\n"),
        "fatal: symref-update refs/heads/s: missing <new-target>\n"
    );
    assert_eq!(
        f.refusal("symref-update refs/heads/s refs/heads/main ref\n"),
        "fatal: symref-update refs/heads/s: expected old value\n"
    );
    assert_eq!(
        f.refusal("symref-update refs/heads/s refs/heads/main bogus x\n"),
        "fatal: symref-update refs/heads/s: invalid arg 'bogus' for old value\n"
    );
    assert_eq!(
        f.refusal("symref-update refs/heads/s refs/heads/main oid zzz\n"),
        "fatal: symref-update refs/heads/s: invalid oid: zzz\n"
    );
    // `symref-create` names the reference in the same failure.
    assert_eq!(
        f.refusal("symref-create refs/heads/s\n"),
        "fatal: symref-create refs/heads/s: missing <new-target>\n"
    );
    assert!(!f.exists("refs/heads/s"));
}

/// Under `-z` the value slots are separate NUL-terminated records appended to
/// the same buffer (builtin/update-ref.c:747-749), and an empty one means
/// "unspecified" — except for `update`'s `<new-oid>`, which is zero and says so
/// (:215-219).
#[test]
fn nul_mode_reads_each_slot_as_its_own_record() {
    let f = Fixture::new("nul");
    let head = f.oid("HEAD");

    let run = |input: &[u8]| {
        let mut child = f
            .cmd(&["update-ref", "-z", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    };

    let mut create = Vec::new();
    create.extend_from_slice(b"update refs/heads/z\0");
    create.extend_from_slice(head.as_bytes());
    create.extend_from_slice(b"\0\0");
    assert_eq!(run(&create), (String::new(), 0));
    assert_eq!(f.oid("refs/heads/z"), head);

    // An empty `<new-oid>` is the zero value, which deletes — with a warning that
    // names the command and the reference.
    let mut zero = Vec::new();
    zero.extend_from_slice(b"update refs/heads/z\0\0\0");
    assert_eq!(
        run(&zero),
        (
            "warning: update refs/heads/z: missing <new-oid>, treating as zero\n".to_string(),
            0
        )
    );
    assert!(!f.exists("refs/heads/z"));

    // A value slot that names nothing is reported with the same `invalid` label
    // the line form uses.
    let mut bad = Vec::new();
    bad.extend_from_slice(b"update refs/heads/z\0zzz\0\0");
    assert_eq!(
        run(&bad),
        ("fatal: update refs/heads/z: invalid <new-oid>: zzz\n".to_string(), 128)
    );
}
