//! `transfer.hideRefs` / `<section>.hideRefs` as `parse_hide_refs_config()`
//! (refs.c:1688-1708) collects them: one list in configuration order, each
//! value with its trailing slashes stripped.
//!
//! ```c
//! ref = (char *)strvec_push(hide_refs, value);
//! len = strlen(ref);
//! while (len && ref[len - 1] == '/')
//!         ref[--len] = '\0';
//! ```
//!
//! `ref_is_hidden()` (refs.c:1710-1740) lets the last matching pattern win, so
//! the order decides a `!`-negation that overlaps a pattern from the other key.
//! zvcs read `transfer.hideRefs` before `<section>.hideRefs` regardless of where
//! they sat in the file, and kept the trailing slash, so `refs/tags/` (which
//! can then only match a ref named `refs/tags//…`) hid nothing — in
//! receive-pack's advertisement (builtin/receive-pack.c:149) and in
//! upload-pack's v0 advertisement and v2 `ls-refs` (upload-pack.c:1378,
//! ls-refs.c:158).
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit, branches `main`, `x`, `y` and tag `t`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-hide-refs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["branch", "x"]);
        f.run(&["branch", "y"]);
        f.run(&["tag", "t"]);
        f
    }

    fn run(&self, args: &[&str]) -> (Vec<u8>, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The ref names of a v0 `--advertise-refs` pkt-line stream.
    fn advertised(&self, service: &str) -> Vec<String> {
        let (out, err, code) = self.run(&[service, "--advertise-refs", "."]);
        assert_eq!((err.as_str(), code), ("", 0), "{service}");
        let mut names = Vec::new();
        let mut rest = out.as_slice();
        while rest.len() >= 4 {
            let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
            if len == 0 {
                rest = &rest[4..];
                continue;
            }
            let payload = &rest[4..len];
            let line = payload.split(|b| *b == 0).next().unwrap();
            let line = String::from_utf8_lossy(line).trim_end().to_string();
            names.push(line.split_once(' ').unwrap().1.to_string());
            rest = &rest[len..];
        }
        names
    }

    fn ls_remote(&self, version: &str) -> Vec<String> {
        let (out, err, code) =
            self.run(&["-c", &format!("protocol.version={version}"), "ls-remote", "."]);
        assert_eq!((err.as_str(), code), ("", 0));
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| l.split_once('\t').unwrap().1.to_string())
            .collect()
    }
}

#[test]
fn a_trailing_slash_is_stripped_from_each_pattern() {
    let f = Fixture::new("slash");
    f.run(&["config", "receive.hideRefs", "refs/tags/"]);
    f.run(&["config", "uploadpack.hideRefs", "refs/tags//"]);
    assert_eq!(
        f.advertised("receive-pack"),
        ["refs/heads/main", "refs/heads/x", "refs/heads/y"]
    );
    assert_eq!(
        f.advertised("upload-pack"),
        ["HEAD", "refs/heads/main", "refs/heads/x", "refs/heads/y"]
    );
    for version in ["0", "2"] {
        assert_eq!(
            f.ls_remote(version),
            ["HEAD", "refs/heads/main", "refs/heads/x", "refs/heads/y"],
            "protocol.version={version}"
        );
    }
}

#[test]
fn a_later_transfer_negation_beats_an_earlier_section_pattern() {
    let f = Fixture::new("order");
    // `receive.hideRefs` first in the file, the `transfer.hideRefs` negation
    // after it: the negation is last, so `x` is shown.
    f.run(&["config", "receive.hideRefs", "refs/heads"]);
    f.run(&["config", "uploadpack.hideRefs", "refs/heads/"]);
    f.run(&["config", "transfer.hideRefs", "!refs/heads/x"]);
    assert_eq!(f.advertised("receive-pack"), ["refs/heads/x", "refs/tags/t"]);
    assert_eq!(f.advertised("upload-pack"), ["HEAD", "refs/heads/x", "refs/tags/t"]);
    for version in ["0", "2"] {
        assert_eq!(
            f.ls_remote(version),
            ["HEAD", "refs/heads/x", "refs/tags/t"],
            "protocol.version={version}"
        );
    }
    // `--exclude-hidden` shares the same collection.
    let (out, _, code) = f.run(&["rev-parse", "--symbolic", "--exclude-hidden=receive", "--all"]);
    assert_eq!(
        (String::from_utf8(out).unwrap().as_str(), code),
        ("refs/heads/x\nrefs/tags/t\n", 0)
    );
}
