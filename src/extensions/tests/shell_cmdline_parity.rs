//! `git shell`'s command-line splitter and the diagnostics it feeds.
//!
//! `git-shell` is what stands between an SSH key and a repository, so the exact
//! text it refuses with is the whole of its interface: an administrator reads it
//! out of `auth.log`, and a client library matches on it. Two of those strings
//! come out of `alias.c: split_cmdline()`, which has three failure codes and a
//! `split_cmdline_strerror()` table to render them (alias.c:"cmdline ends with
//! \\", "unclosed quote", "too many arguments"), and one comes out of
//! `shell.c: run_shell()`'s invalid-name branch, which quotes `argv[0]` rather
//! than the line it came from (shell.c:133).
//!
//! Both are driven without a network, a socket or a repository: `-c` mode is one
//! process, and interactive mode is a pipe.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Home {
    root: PathBuf,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Home {
    /// A HOME with a readable, searchable `git-shell-commands`, which is what
    /// `shell.c: cmd_main()` requires before it will run the prompt loop.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-gitshell-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("git-shell-commands")).unwrap();
        Home { root }
    }

    /// Run `git shell` with `args`, feeding `stdin`. Returns `(code, stderr)`;
    /// every diagnostic `git shell` writes goes to stderr.
    fn run(&self, args: &[&str], stdin: &[u8]) -> (i32, String) {
        let mut child = Command::new(BIN)
            .arg("shell")
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().expect("git shell exited rather than being signalled"),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// A trailing backslash is `SPLIT_CMDLINE_BAD_ENDING`, not a word to exec.
///
/// alias.c's splitter consumes the byte after a `\` and returns
/// `-SPLIT_CMDLINE_BAD_ENDING` when there is none; `shell.c: cmd_main()` renders
/// that through `split_cmdline_strerror()` and dies. Stock git 2.55.0:
///
/// ```text
/// $ git shell -c 'foo\'; echo $?
/// fatal: invalid command format 'foo\': cmdline ends with \
/// 128
/// ```
///
/// A splitter that instead dropped the dangling `\` would try to exec
/// `git-shell-commands/foo` and report `unrecognized command 'foo\'` — a
/// different string, and one that has already attempted an exec the request was
/// never entitled to.
#[test]
fn trailing_backslash_is_a_parse_failure_not_an_exec() {
    let home = Home::new("badending");
    let (code, err) = home.run(&["-c", "foo\\"], b"");
    assert_eq!(
        err, "fatal: invalid command format 'foo\\': cmdline ends with \\\n",
        "trailing backslash diagnostic"
    );
    assert_eq!(code, 128, "die() exits 128");
}

/// The quote error keeps its own text, so the two codes stay distinguishable.
#[test]
fn unclosed_quote_keeps_its_own_diagnostic() {
    let home = Home::new("unclosed");
    let (code, err) = home.run(&["-c", "foo 'bar"], b"");
    assert_eq!(
        err, "fatal: invalid command format 'foo 'bar': unclosed quote\n",
        "unclosed quote diagnostic"
    );
    assert_eq!(code, 128, "die() exits 128");
}

/// A `\` inside single quotes is literal, so it is not a bad ending.
///
/// alias.c only treats `\` as an escape when `quoted != '\''`; inside `'…'` the
/// byte is data and the word ends at the closing quote. This is the line between
/// the two cases above and the normal path, so it is asserted directly: the
/// command is unrecognised (nothing is installed under `git-shell-commands`),
/// **not** malformed.
#[test]
fn backslash_inside_single_quotes_is_literal() {
    let home = Home::new("litbs");
    let (code, err) = home.run(&["-c", "'foo\\'"], b"");
    assert_eq!(
        err, "fatal: unrecognized command ''foo\\''\n",
        "a quoted backslash is a word, not a parse error"
    );
    assert_eq!(code, 128, "die() exits 128");
}

/// Interactive mode quotes `argv[0]`, not the whole line, for an invalid name.
///
/// shell.c:133 is `fprintf(stderr, "invalid command format '%s'\n", prog)` where
/// `prog = argv[0]`, while the `split_cmdline` failure five lines above quotes
/// `rawargs`. Stock git 2.55.0, fed `foo/bar baz`, `qux.x` and `unk`:
///
/// ```text
/// git> invalid command format 'foo/bar'
/// git> invalid command format 'qux.x'
/// git> unrecognized command 'unk'
/// git>
/// ```
///
/// A port that quoted the raw line would print `invalid command format 'foo/bar
/// baz'`, which reads as though the arguments were part of the problem.
#[test]
fn interactive_invalid_name_quotes_argv0_only() {
    let home = Home::new("interactive");
    let (code, err) = home.run(&[], b"foo/bar baz\nqux.x\nunk\n");
    assert_eq!(
        err,
        "git> invalid command format 'foo/bar'\n\
         git> invalid command format 'qux.x'\n\
         git> unrecognized command 'unk'\n\
         git> \n",
        "prompt/diagnostic stream"
    );
    assert_eq!(code, 0, "EOF ends the loop successfully");
}

/// The interactive splitter failure still quotes the whole line.
///
/// The counterpart to the test above: shell.c:117-119 renders `rawargs`, so the
/// two branches must not be collapsed onto one spelling.
#[test]
fn interactive_split_failure_quotes_the_whole_line() {
    let home = Home::new("interactive-split");
    let (code, err) = home.run(&[], b"foo bar\\\n");
    assert_eq!(
        err,
        "git> invalid command format 'foo bar\\': cmdline ends with \\\n\
         git> \n",
        "prompt/diagnostic stream"
    );
    assert_eq!(code, 0, "a bad line does not end the loop");
}
