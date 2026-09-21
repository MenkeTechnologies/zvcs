//! `git http-fetch` — the option scanner's cursor.
//!
//! `http-fetch.c`'s `main` walks `argv` from index 1, index 0 being the command
//! name. The dispatcher here hands over the arguments that *follow* the verb, so
//! the scan must start at index 0; starting at 1 silently drops whichever option
//! the user typed first, which is every invocation with exactly one option.
//!
//! Both tests stop inside the option loop, before a URL is ever opened: nothing
//! here touches the network. The expectations were measured from stock git
//! 2.55.0 in an empty repository.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// An empty repository to run in — `http-fetch` dies with `not a git repository`
/// outside one, which would mask the option-scanning difference.
struct Repo(PathBuf);

impl Repo {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("zvcs-httpfetch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        let out = Command::new(BIN)
            .args(["init", "-q"])
            .current_dir(&p)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "init failed: {out:?}");
        Repo(p)
    }

    fn http_fetch(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("http-fetch")
            .args(args)
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `--packfile=` is validated inside the scan loop, with
/// `die(_("argument to --packfile must be a valid hash (got '%s')"), p)`
/// (http-fetch.c). Stock git 2.55.0:
///
/// ```text
/// $ git http-fetch --packfile=zzz http://example.invalid/
/// fatal: argument to --packfile must be a valid hash (got 'zzz')   [rc 128]
/// ```
///
/// The same invocation with a throwaway option in front (`-v --packfile=zzz`)
/// produces exactly the same thing, which is what makes this a cursor test: a
/// scanner that skips the first argument answers the usage line here and the
/// `fatal:` there.
#[test]
fn the_first_option_after_the_verb_is_scanned() {
    let repo = Repo::new("firstopt");

    let first = repo.http_fetch(&["--packfile=zzz", "http://example.invalid/"]);
    let second = repo.http_fetch(&["-v", "--packfile=zzz", "http://example.invalid/"]);

    assert_eq!(
        String::from_utf8_lossy(&first.stderr),
        "fatal: argument to --packfile must be a valid hash (got 'zzz')\n"
    );
    assert_eq!(first.status.code(), Some(128));
    assert!(first.stdout.is_empty());

    assert_eq!(
        String::from_utf8_lossy(&second.stderr),
        String::from_utf8_lossy(&first.stderr),
        "an option in first position must be scanned like one in second position"
    );
    assert_eq!(second.status.code(), first.status.code());
}

/// The argument-count check is `argc != arg + 2 - (commits_on_stdin || packfile)`
/// (http-fetch.c), so `--stdin` in first position leaves exactly one positional
/// — the URL. A scanner that skipped it would want two and answer the usage line.
///
/// Stock git 2.55.0 on an empty stdin walks nothing and exits 0 without opening
/// the URL; this test asserts only that the invocation is *not* rejected as a
/// usage error, so it stays independent of what the walk would then do.
#[test]
fn stdin_in_first_position_satisfies_the_argument_count() {
    let repo = Repo::new("stdinfirst");
    let out = repo.http_fetch(&["--stdin", "http://example.invalid/"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.starts_with("usage: git http-fetch"),
        "`--stdin <url>` was rejected as a usage error: {stderr:?}"
    );
    assert_ne!(out.status.code(), Some(129), "stderr: {stderr:?}");
}
