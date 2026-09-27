//! The `error:` line `verify-pack` / `index-pack --verify` print ahead of
//! `Cannot open existing pack idx file` for a malformed `.idx`.
//!
//! `open_pack_index()` runs `check_packed_git_idx()` and `load_idx()`
//! (packfile.c:160-262), each of which reports why it refused the file before
//! returning: `index file <p> is too small`, `index file <p> is version <n> and
//! is not supported by this binary (try upgrading GIT to a newer version)`,
//! `non-monotonic index <p>`, `wrong index v1 file size in <p>` and `wrong index
//! v2 file size in <p>`. zvcs reproduced only the last.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

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
    /// A repository with two commits and `good.pack`/`good.idx` beside it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-verify-pack-idx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        for i in 1..=2 {
            std::fs::write(f.work.join("f"), format!("{i}\n")).unwrap();
            f.run(&["add", "f"]);
            f.run(&["commit", "-q", "-m", &format!("c{i}")]);
        }
        let out = Command::new(BIN)
            .args(["pack-objects", "--all", "../good"])
            .current_dir(&f.work)
            .env("HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success());
        let hash = String::from_utf8(out.stdout).unwrap();
        let hash = hash.trim_end();
        for ext in ["pack", "idx"] {
            std::fs::rename(
                f.root.join(format!("good-{hash}.{ext}")),
                f.root.join(format!("good.{ext}")),
            )
            .unwrap();
        }
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// `../<name>.pack` copied from the good pack, `../<name>.idx` from `idx`.
    fn pair(&self, name: &str, idx: &[u8]) {
        std::fs::copy(self.root.join("good.pack"), self.root.join(format!("{name}.pack"))).unwrap();
        std::fs::write(self.root.join(format!("{name}.idx")), idx).unwrap();
    }

    fn good_idx(&self) -> Vec<u8> {
        std::fs::read(self.root.join("good.idx")).unwrap()
    }
}

fn expect(f: &Fixture, name: &str, error: &str) {
    let idx = format!("../{name}.idx");
    let want = format!("error: {error}\nfatal: Cannot open existing pack idx file for '{idx}'\n");
    let (out, err, code) = f.run(&["verify-pack", &idx]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 1), "verify-pack {name}");
    let (out, err, code) = f.run(&["index-pack", "--verify", &format!("../{name}.pack")]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "index-pack {name}");
}

#[test]
fn a_short_index_is_too_small() {
    let f = Fixture::new("small");
    f.pair("d", &f.good_idx()[..500]);
    expect(&f, "d", "index file ../d.idx is too small");
}

#[test]
fn an_unknown_version_is_named() {
    let f = Fixture::new("version");
    let mut idx = f.good_idx();
    idx[4..8].copy_from_slice(&3u32.to_be_bytes());
    f.pair("e", &idx);
    expect(
        &f,
        "e",
        "index file ../e.idx is version 3 and is not supported by this binary \
         (try upgrading GIT to a newer version)",
    );
}

#[test]
fn a_decreasing_fanout_is_non_monotonic() {
    let f = Fixture::new("fanout");
    let mut idx = f.good_idx();
    idx[8 + 4 * 10..8 + 4 * 11].copy_from_slice(&0xff00_0000u32.to_be_bytes());
    f.pair("f", &idx);
    expect(&f, "f", "non-monotonic index ../f.idx");
}

#[test]
fn a_file_without_the_v2_signature_is_measured_as_v1() {
    let f = Fixture::new("v1");
    f.pair("h", &[0u8; 1100]);
    expect(&f, "h", "wrong index v1 file size in ../h.idx");
}
