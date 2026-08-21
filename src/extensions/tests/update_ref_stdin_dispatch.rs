//! `git update-ref --stdin` diagnoses a line the way git's dispatch loop does.
//!
//! ```c
//! while (!strbuf_getwholeline(&input, stdin, line_termination)) {
//!         const struct parse_cmd *cmd = NULL;
//!
//!         if (*input.buf == line_termination)
//!                 die("empty command in input");
//!         else if (isspace(*input.buf))
//!                 die("whitespace before command: %s", input.buf);
//!
//!         for (i = 0; i < ARRAY_SIZE(command); i++) {
//!                 const char *prefix = command[i].prefix;
//!                 char c;
//!
//!                 if (!starts_with(input.buf, prefix))
//!                         continue;
//!                 c = command[i].args ? ' ' : line_termination;
//!                 if (input.buf[strlen(prefix)] != c)
//!                         continue;
//!
//!                 cmd = &command[i];
//!                 break;
//!         }
//!         if (!cmd)
//!                 die("unknown command: %s", input.buf);
//! ```
//!
//! (`builtin/update-ref.c:712-740`.) Four things follow, and the port had all
//! four wrong:
//!
//! * `strbuf_getwholeline()` **keeps the terminator**, so the `%s` of every one
//!   of those diagnostics ends in the input's own newline and `die()` then adds
//!   its own. The stderr of an unknown command ends `\n\n`, not `\n`.
//! * An empty line is `empty command in input`, not a line to skip.
//! * A line starting with whitespace has its own message.
//! * The byte after the prefix must be exactly `' '` (for a command with
//!   arguments) or the terminator (for one without). That is what makes
//!   `commit foo`, a bare `option`, and a final line with no terminator at all
//!   *unknown commands* rather than mis-argued ones.
//!
//! Under `-z` the terminator is the NUL, so `%s` stops before it and only
//! `die()`'s newline shows — which is why the `-z` cases below expect one
//! newline where the line-mode ones expect two.
//!
//! Every expectation was captured from stock git 2.55.0, byte-for-byte.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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
        let root = std::env::temp_dir().join(format!("zvcs-urstdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work };
        fx.ok(&["init", "-q", "-b", "main", "."]);
        fx.ok(&["commit", "-q", "--allow-empty", "-m", "one"]);
        fx
    }

    fn feed(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(BIN)
            .args(["-c", "user.email=t@e.co", "-c", "user.name=t"])
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run binary");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.feed(args, b"");
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
        out
    }

    fn head(&self) -> String {
        String::from_utf8_lossy(&self.ok(&["rev-parse", "HEAD"]).stdout).trim_end().to_string()
    }

    fn has_ref(&self, name: &str) -> bool {
        self.feed(&["rev-parse", "--verify", "--quiet", name], b"").status.success()
    }
}

/// stderr as raw bytes: the whole point of these cases is the trailing newline.
fn err(out: &Output) -> Vec<u8> {
    out.stderr.clone()
}

#[test]
fn an_unknown_command_echoes_the_whole_line_including_its_newline() {
    let fx = Fixture::new("unknown");

    for line in [
        // Not a prefix of anything.
        "bogus line\n",
        // A real prefix with the wrong byte after it: `update` takes arguments,
        // so `updatex` is not `update`.
        "updatex a b c\n",
        // A no-argument command handed one.
        "commit foo\n",
        // A command *with* arguments handed none: `option` needs the space.
        "option\n",
    ] {
        let out = fx.feed(&["update-ref", "--stdin"], line.as_bytes());
        assert_eq!(out.status.code(), Some(128), "{line:?} exit: {out:?}");
        assert_eq!(
            err(&out),
            format!("fatal: unknown command: {line}\n").into_bytes(),
            "{line:?} stderr"
        );
    }
}

#[test]
fn a_final_line_without_a_terminator_is_an_unknown_command() {
    let fx = Fixture::new("unterminated");

    // `input.buf[strlen("start")]` is the string's NUL, and the terminator is
    // `'\n'`, so the table never matches. The `%s` then has no newline of its
    // own and only `die()`'s shows.
    let out = fx.feed(&["update-ref", "--stdin"], b"start");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(err(&out), b"fatal: unknown command: start\n".to_vec());

    // The commands ahead of it have already run, so their output stands.
    let out = fx.feed(&["update-ref", "--stdin"], b"start\ncommit");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "start: ok\n", "{out:?}");
    assert_eq!(err(&out), b"fatal: unknown command: commit\n".to_vec());
}

#[test]
fn an_empty_line_and_a_leading_space_have_their_own_messages() {
    let fx = Fixture::new("shape");

    let out = fx.feed(&["update-ref", "--stdin"], b"\n");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(err(&out), b"fatal: empty command in input\n".to_vec());

    // The empty line wins even when a valid command follows it — the check is
    // per line, in order.
    let out = fx.feed(&["update-ref", "--stdin"], b"\nstart\n");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(err(&out), b"fatal: empty command in input\n".to_vec());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{out:?}");

    // C's `isspace()`, and the line comes back verbatim — terminator included.
    for line in [" update x\n", "\tupdate x\n", "\t\n"] {
        let out = fx.feed(&["update-ref", "--stdin"], line.as_bytes());
        assert_eq!(out.status.code(), Some(128), "{line:?} exit: {out:?}");
        assert_eq!(
            err(&out),
            format!("fatal: whitespace before command: {line}\n").into_bytes(),
            "{line:?} stderr"
        );
    }
}

#[test]
fn a_carriage_return_belongs_to_the_line() {
    let fx = Fixture::new("crlf");

    // `strbuf_getwholeline()` splits on `'\n'` alone, so the `\r` of a CRLF line
    // is part of the command text — which makes `start\r` an unknown command and
    // puts the `\r` in the message. Splitting with a helper that strips it
    // silently accepted CRLF input git refuses.
    let out = fx.feed(&["update-ref", "--stdin"], b"start\r\n");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(err(&out), b"fatal: unknown command: start\r\n\n".to_vec());
}

#[test]
fn nul_terminated_input_reports_without_the_extra_newline() {
    let fx = Fixture::new("nul");

    // `%s` stops at the NUL that terminates the record, so exactly one newline —
    // `die()`'s — reaches stderr here.
    let out = fx.feed(&["update-ref", "-z", "--stdin"], b"bogus line\0");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(err(&out), b"fatal: unknown command: bogus line\n".to_vec());

    // And an empty record is the empty-command message, not an unknown command
    // whose name happens to be the empty string.
    let out = fx.feed(&["update-ref", "-z", "--stdin"], b"\0");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(err(&out), b"fatal: empty command in input\n".to_vec());
}

#[test]
fn well_formed_input_still_applies() {
    let fx = Fixture::new("apply");
    let head = fx.head();

    // The diagnostics moved; the transaction did not. A batch that git accepts
    // still commits, and the controls still print their `ok` lines.
    let input = format!("start\nupdate refs/heads/topic {head}\ncommit\n");
    let out = fx.feed(&["update-ref", "--stdin"], input.as_bytes());
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(err(&out), Vec::<u8>::new(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "start: ok\ncommit: ok\n", "{out:?}");
    assert!(fx.has_ref("refs/heads/topic"), "the batch did not land: {out:?}");

    // The implicit transaction, with no controls at all.
    let input = format!("create refs/heads/other {head}\n");
    let out = fx.feed(&["update-ref", "--stdin"], input.as_bytes());
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{out:?}");
    assert!(fx.has_ref("refs/heads/other"), "the implicit batch did not land: {out:?}");

    // And the `-z` form, whose records are NUL-separated value slots.
    let input = format!("create refs/heads/zed\0{head}\0");
    let out = fx.feed(&["update-ref", "-z", "--stdin"], input.as_bytes());
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(fx.has_ref("refs/heads/zed"), "the -z batch did not land: {out:?}");
}
