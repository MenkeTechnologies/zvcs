//! `index-pack --keep` over a `.keep` that is already there.
//!
//! `write_special_file()` opens the file through
//! `safe_create_file_with_leading_directories()`, which is
//! `open(path, O_RDWR|O_CREAT|O_EXCL, 0600)` (path.c:912-924). On `EEXIST` it
//! writes nothing and leaves `*report` alone (builtin/index-pack.c:1572-1587),
//! so `final()` prints its initial `report = "pack"` (:1613, :1652) and the
//! first message survives. zvcs rewrote the `.keep` with the new message and
//! printed `keep\t<hash>` whenever `--keep` was given.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    src: PathBuf,
    dst: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src` holds two commits packed into one pack; `dst` is empty.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-index-pack-keep-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let f = Fixture { root, src, dst };
        run(&f.src, &f.root, &["init", "-q", "."], None);
        run(&f.dst, &f.root, &["init", "-q", "."], None);
        for i in 1..=2 {
            std::fs::write(f.src.join("f"), format!("{i}\n")).unwrap();
            run(&f.src, &f.root, &["add", "f"], None);
            run(&f.src, &f.root, &["commit", "-q", "-m", &format!("c{i}")], None);
        }
        f
    }

    fn pack(&self) -> Vec<u8> {
        run(&self.src, &self.root, &["pack-objects", "--all", "--stdout"], Some(b"")).0
    }

    fn pack_dir(&self) -> PathBuf {
        self.dst.join(".git/objects/pack")
    }

    fn keep_file(&self, ext: &str) -> PathBuf {
        let dir = self.pack_dir();
        std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == ext))
            .unwrap_or_else(|| panic!("no .{ext} in {}", dir.display()))
    }
}

fn run(dir: &Path, home: &Path, args: &[&str], stdin: Option<&[u8]>) -> (Vec<u8>, String, i32) {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
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
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(data) = stdin {
        child.stdin.take().unwrap().write_all(data).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    (
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn stdout(v: Vec<u8>) -> String {
    String::from_utf8(v).unwrap()
}

#[test]
fn stdin_keep_over_an_existing_keep_reports_pack_and_keeps_the_first_message() {
    let f = Fixture::new("stdin");
    let pack = f.pack();

    let (out, err, code) = run(&f.dst, &f.root, &["index-pack", "--stdin", "--keep=first"], Some(&pack));
    let out = stdout(out);
    assert_eq!((err.as_str(), code), ("", 0));
    let hash = out.strip_prefix("keep\t").expect("first run creates the .keep").trim_end().to_string();

    let (out, err, code) = run(&f.dst, &f.root, &["index-pack", "--stdin", "--keep=second"], Some(&pack));
    assert_eq!((stdout(out), err.as_str(), code), (format!("pack\t{hash}\n"), "", 0));
    assert_eq!(std::fs::read_to_string(f.keep_file("keep")).unwrap(), "first\n");
}

#[test]
fn stdin_promisor_over_an_existing_promisor_keeps_the_first_message() {
    let f = Fixture::new("promisor");
    let pack = f.pack();
    for msg in ["one", "two"] {
        let (_, err, code) = run(
            &f.dst,
            &f.root,
            &["index-pack", "--stdin", &format!("--promisor={msg}")],
            Some(&pack),
        );
        assert_eq!((err.as_str(), code), ("", 0), "{msg}");
    }
    assert_eq!(std::fs::read_to_string(f.keep_file("promisor")).unwrap(), "one\n");
}

#[test]
fn named_pack_keep_leaves_an_existing_keep_alone() {
    let f = Fixture::new("named");
    let named = f.root.join("n.pack");
    std::fs::write(&named, f.pack()).unwrap();
    let path = named.to_str().unwrap();

    let (first, err, code) = run(&f.dst, &f.root, &["index-pack", "--keep=a", path], None);
    assert_eq!((err.as_str(), code), ("", 0));
    let (second, err, code) = run(&f.dst, &f.root, &["index-pack", "--keep=b", path], None);
    assert_eq!((err.as_str(), code), ("", 0));
    assert_eq!(second, first);
    assert_eq!(std::fs::read_to_string(f.root.join("n.keep")).unwrap(), "a\n");
}
