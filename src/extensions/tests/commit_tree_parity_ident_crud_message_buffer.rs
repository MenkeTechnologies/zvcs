//! Three places where `git commit-tree` and zvcs wrote different commits or
//! died differently.
//!
//! * `fmt_ident()` copies the name and the email through
//!   `strbuf_addstr_without_crud()` (ident.c:229-266, :525-528): crud is trimmed
//!   from both ends and every `\n`, `<` and `>` left inside is dropped. zvcs kept
//!   the bytes, so `GIT_AUTHOR_NAME='  sp  '` wrote a different commit and a name
//!   holding `<` could not be written at all.
//! * The message is read from stdin whenever `-m` and `-F` left the buffer empty
//!   (`if (!buffer.len)`, builtin/commit-tree.c:142-145) — after `-m ''` or an
//!   empty `-F` file too, where zvcs read stdin only when neither was given.
//! * `-F <dir>` opens (open(2) accepts a directory) and then fails to read,
//!   which is the command's own `git commit-tree: failed to read '<f>'`
//!   (builtin/commit-tree.c:87-88); zvcs reported it as a failed open with
//!   Rust's `(os error 21)` suffix.
//!
//! Peeling `<commit>^{tree}` parses the commit (`peel_onion()`,
//! object-name.c:961), and the first parse reads `info/grafts` and prints its
//! deprecation advice (commit.c:287-300, :554); zvcs read the graft file only
//! when walking parents, so `commit-tree HEAD^{tree}` stayed silent.
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

/// `git <args>` in `side`'s repository with `env` and `input` on stdin, the
/// repository root replaced by `<root>`.
fn run(side: &twin_repo::Side, env: &[(&str, &str)], args: &[&str], input: &str) -> (i32, String, String) {
    let mut cmd = Command::new(&side.bin);
    cmd.args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let root = side.root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
}

#[test]
fn identities_lose_their_crud_and_an_empty_buffer_reads_stdin() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("commit-tree-crud", stock);
    s.write("empty", "");
    z.write("empty", "");
    let tree = s.git(&["rev-parse", "HEAD^{tree}"]).stdout.trim().to_owned();
    let cases: &[(&[(&str, &str)], &[&str])] = &[
        (&[("GIT_AUTHOR_NAME", "  sp  ")], &["-m", "x"]),
        (&[("GIT_AUTHOR_NAME", ".,;<>x")], &["-m", "x"]),
        (&[("GIT_AUTHOR_NAME", "a<b>c\nd")], &["-m", "x"]),
        (&[("GIT_AUTHOR_EMAIL", " <a@b>; ")], &["-m", "x"]),
        (&[("GIT_COMMITTER_NAME", "\"q\"")], &["-m", "x"]),
        (&[], &["-m", ""]),
        (&[], &["-m", "", "-m", ""]),
        (&[], &["-F", "empty"]),
        (&[], &["-F", ".git"]),
    ];
    for (env, extra) in cases {
        let mut args = vec!["commit-tree", tree.as_str()];
        args.extend_from_slice(extra);
        let want = run(&s, env, &args, "from stdin\n");
        assert_eq!(run(&z, env, &args, "from stdin\n"), want, "{env:?} {args:?}");
    }
}

#[test]
fn peeling_a_commit_reads_the_graft_file() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("commit-tree-graft", stock);
    let head = s.git(&["rev-parse", "HEAD"]).stdout;
    s.write(".git/info/grafts", &head);
    z.write(".git/info/grafts", &head);
    for args in [
        &["commit-tree", "HEAD^{tree}", "-m", "x"][..],
        &["rev-parse", "HEAD^{}", "HEAD^{object}", "HEAD:a"],
    ] {
        assert_eq!(run(&z, &[], args, ""), run(&s, &[], args, ""), "{args:?}");
    }
}
