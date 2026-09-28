//! An argument that is not UTF-8 panicked before any command ran.
//!
//! git's `main()` (common-main.c) hands `cmd_main()` the raw `argv` bytes, and
//! nothing between there and a builtin asks whether they are UTF-8: a ref name
//! holding `\x80` passes `check_refname_component()` (refs.c), which refuses only
//! control characters, DEL and a handful of ASCII specials. zvcs collected argv
//! with `std::env::args()`, which panics (exit 101) on the first non-UTF-8
//! element — so `git check-ref-format 'refs/heads/a\x80b'` crashed. argv is now
//! read with `args_os()` through `crate::rawarg`, and the bytes reach the verb,
//! its output and any child process (`git-<cmd>` on PATH, a `!` alias) intact.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn raw(b: &[u8]) -> OsString {
    OsString::from_vec(b.to_vec())
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-crf-non-utf8-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        Fixture { root }
    }

    fn run(&self, args: &[OsString]) -> (Vec<u8>, Vec<u8>, i32) {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (out.stdout, out.stderr, out.status.code().expect("no signal"))
    }
}

#[test]
fn check_ref_format_takes_the_bytes() {
    let f = Fixture::new("crf");
    let (out, err, code) = f.run(&["check-ref-format".into(), raw(b"refs/heads/a\x80b")]);
    assert_eq!((out.as_slice(), err.as_slice(), code), (&b""[..], &b""[..], 0));

    let (out, err, code) =
        f.run(&["check-ref-format".into(), "--normalize".into(), raw(b"refs//heads/a\x80b")]);
    assert_eq!((out.as_slice(), err.as_slice(), code), (&b"refs/heads/a\x80b\n"[..], &b""[..], 0));

    let (out, err, code) = f.run(&["check-ref-format".into(), "--branch".into(), raw(b"a\x80b")]);
    assert_eq!((out.as_slice(), err.as_slice(), code), (&b"a\x80b\n"[..], &b""[..], 0));

    let (out, err, code) = f.run(&["check-ref-format".into(), "--branch".into(), raw(b"-a\x80b")]);
    assert_eq!(
        (out.as_slice(), err.as_slice(), code),
        (&b""[..], &b"fatal: '-a\x80b' is not a valid branch name\n"[..], 128)
    );
}

#[test]
fn children_receive_the_bytes() {
    let f = Fixture::new("child");
    let script = f.root.join("bin/git-showargs");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s|' \"$@\"\n").unwrap();
    let mut perm = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&script, perm).unwrap();

    let (out, _, code) = f.run(&["showargs".into(), raw(b"a\xe9"), "b".into()]);
    assert_eq!((out.as_slice(), code), (&b"a\xe9|b|"[..], 0));

    let (out, _, code) =
        f.run(&["-c".into(), "alias.sa=!printf '%s|'".into(), "sa".into(), raw(b"q\xe9")]);
    assert_eq!((out.as_slice(), code), (&b"q\xe9|"[..], 0));
}
