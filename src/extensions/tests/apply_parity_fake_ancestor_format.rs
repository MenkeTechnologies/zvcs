//! The file `apply --build-fake-ancestor` writes is an ordinary index write.
//!
//! `build_fake_ancestor()` (apply.c:4243) starts from `INDEX_STATE_INIT`, whose
//! version is zero, and hands it to `write_locked_index()`. So `do_write_index()`
//! chooses the version (`get_index_format_default()`, read-cache.c:2865) and
//! `record_eoie()` decides the `EOIE` extension (read-cache.c:2957) exactly as for
//! `.git/index`: no `EOIE` unless `index.threads` or
//! `index.recordEndOfIndexEntries` asks for one.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A one-line append to `a` (blob `7898192` holds `a\n`).
const PATCH: &str = "\
diff --git a/a b/a
index 7898192..422c2b7 100644
--- a/a
+++ b/a
@@ -1 +1,2 @@
 a
+b
";

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
        let root = std::env::temp_dir().join(format!("zvcs-fakeanc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.apply(&["init", "-q", "-b", "main", "."], &[]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.apply(&["add", "a"], &[]);
        f
    }

    /// `git <args>` with `env`, [`PATCH`] on stdin; returns the fake ancestor's bytes
    /// when the run wrote one.
    fn apply(&self, args: &[&str], env: &[(&str, &str)]) -> Option<Vec<u8>> {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(PATCH.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        let fake = self.work.join("fake");
        let bytes = std::fs::read(&fake).ok();
        let _ = std::fs::remove_file(fake);
        bytes
    }
}

fn has_eoie(index: &[u8]) -> bool {
    index.windows(4).any(|w| w == b"EOIE")
}

#[test]
fn unconfigured_write_is_version_2_without_eoie() {
    let f = Fixture::new("plain");
    let idx = f.apply(&["apply", "--build-fake-ancestor=fake", "--check"], &[]).unwrap();
    // 12-byte header, one 62-byte entry padded to 64, 20-byte checksum.
    assert_eq!(idx.len(), 96);
    assert_eq!(&idx[..8], b"DIRC\0\0\0\x02");
    assert!(!has_eoie(&idx));
}

#[test]
fn version_and_eoie_follow_the_index_settings() {
    let f = Fixture::new("cfg");
    let v4 = f
        .apply(&["apply", "--build-fake-ancestor=fake"], &[("GIT_INDEX_VERSION", "4")])
        .unwrap();
    assert_eq!(&v4[..8], b"DIRC\0\0\0\x04");
    assert_eq!(v4.len(), 97);

    let threaded = f
        .apply(&["-c", "index.threads=true", "apply", "--build-fake-ancestor=fake", "--check"], &[])
        .unwrap();
    assert_eq!(threaded.len(), 128);
    assert!(has_eoie(&threaded));
}
