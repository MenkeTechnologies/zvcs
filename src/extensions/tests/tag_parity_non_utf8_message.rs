//! A `tag -m` message that was not UTF-8 was stored as escape code points.
//!
//! `-m` is an `OPT_CALLBACK_F` whose `parse_msg_arg()` (builtin/tag.c:439)
//! `strbuf_addstr()`s the argv string, and `create_tag()` writes the buffer
//! as-is — no `ensure_utf8()` for tags. The argument reaches zvcs through
//! `crate::rawarg`, so the verb has to hand the object the bytes it stands for;
//! it wrote the escape code points' UTF-8 (`F4 8F BF A9` for `\xe9`) instead.
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
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-tag-non-utf8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init".into(), "-q".into()]);
        f.run(&["commit".into(), "-q".into(), "--allow-empty".into(), "-m".into(), "x".into()]);
        f
    }

    fn run(&self, args: &[OsString]) -> (Vec<u8>, Vec<u8>, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "A U Thor")
            .env("GIT_COMMITTER_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (out.stdout, out.stderr, out.status.code().expect("no signal"))
    }

    fn body(&self, tag: &str) -> Vec<u8> {
        let obj = self.run(&["cat-file".into(), "tag".into(), tag.into()]).0;
        let p = obj.windows(2).position(|w| w == b"\n\n").unwrap();
        obj[p + 2..].to_vec()
    }
}

#[test]
fn every_message_spelling_keeps_the_bytes() {
    let f = Fixture::new();
    for (i, args) in [
        vec![raw(b"-m"), raw(b"t\xe9")],
        vec![raw(b"-mt\xe9")],
        vec![raw(b"--message=t\xe9")],
        vec![raw(b"-am"), raw(b"t\xe9")],
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("v{i}");
        let mut argv = vec![raw(b"tag")];
        argv.extend(args);
        argv.push(name.clone().into());
        let (out, err, code) = f.run(&argv);
        assert_eq!((out.as_slice(), err.as_slice(), code), (&b""[..], &b""[..], 0), "{name}");
        assert_eq!(f.body(&name), b"t\xe9\n", "{name}");
    }
    let (out, _, code) = f.run(&["tag".into(), "-n1".into(), "-l".into(), "v0".into()]);
    assert_eq!((out.as_slice(), code), (&b"v0              t\xe9\n"[..], 0));
}
