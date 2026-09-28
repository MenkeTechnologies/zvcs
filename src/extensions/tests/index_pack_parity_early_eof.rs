//! `index-pack` over a pack that stops partway through an entry.
//!
//! Whether the pack is named or read from `--stdin`, `fill()` runs dry and
//! dies `early EOF` (builtin/index-pack.c:325-331); `bundle unbundle` reports
//! that child's death with `error: index-pack died` and exit 1. zvcs printed
//! gix's `A pack entry could not be extracted: pack is incomplete: …`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
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
    /// `../p.pack` of three commits growing `f`, and `../b.bundle` of them.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-index-pack-early-eof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let f = Fixture { root, src, dst };
        f.run_in(&f.src, &["init", "-q", "-b", "main", "."], None);
        f.run_in(&f.dst, &["init", "-q", "-b", "main", "."], None);
        for i in 1..=3 {
            let body: String = (1..=i * 200).map(|n| format!("{n}\n")).collect();
            std::fs::write(f.src.join("f"), body).unwrap();
            f.run_in(&f.src, &["add", "f"], None);
            f.run_in(&f.src, &["commit", "-q", "-m", &format!("c{i}")], None);
        }
        f.run_in(&f.src, &["bundle", "create", "-q", "../b.bundle", "main"], None);
        let pack = Command::new(BIN)
            .args(["pack-objects", "--all", "--stdout"])
            .current_dir(&f.src)
            .env("HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .stdout;
        std::fs::write(f.root.join("p.pack"), pack).unwrap();
        f
    }

    fn run_in(&self, dir: &PathBuf, args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
        let input = match stdin {
            Some(name) => Stdio::from(std::fs::File::open(self.root.join(name)).unwrap()),
            None => Stdio::null(),
        };
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .stdin(input)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// `../<name>` holding the first `len` bytes of `../<from>`.
    fn truncate(&self, from: &str, name: &str, len: usize) {
        let bytes = std::fs::read(self.root.join(from)).unwrap();
        std::fs::write(self.root.join(name), &bytes[..len]).unwrap();
    }
}

#[test]
fn a_truncated_pack_is_early_eof_named_or_on_stdin() {
    let f = Fixture::new("pack");
    let size = std::fs::metadata(f.root.join("p.pack")).unwrap().len() as usize;
    for len in [size - 100, 700, 200] {
        f.truncate("p.pack", "t.pack", len);
        let named = f.run_in(&f.dst, &["index-pack", "-o", "../t.idx", "../t.pack"], None);
        assert_eq!(named, (String::new(), "fatal: early EOF\n".to_string(), 128), "named {len}");
        let stdin = f.run_in(&f.dst, &["index-pack", "--stdin"], Some("t.pack"));
        assert_eq!(stdin, (String::new(), "fatal: early EOF\n".to_string(), 128), "stdin {len}");
    }
}

#[test]
fn a_truncated_bundle_is_early_eof_then_index_pack_died() {
    let f = Fixture::new("bundle");
    let size = std::fs::metadata(f.root.join("b.bundle")).unwrap().len() as usize;
    f.truncate("b.bundle", "t.bundle", size / 2);
    assert_eq!(
        f.run_in(&f.dst, &["bundle", "unbundle", "../t.bundle"], None),
        (String::new(), "fatal: early EOF\nerror: index-pack died\n".to_string(), 1)
    );
}
