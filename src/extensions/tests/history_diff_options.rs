//! `log` and `show` take the patch-shaping diff options `git diff` takes.
//!
//! `setup_revisions()` hands every `diff_opt_parse()` flag to the same diff machinery,
//! so `-w`, `-U<n>`, `--full-index`, the prefixes, `-W` and the rename knobs shape a
//! commit's patch exactly as they shape `git diff`'s. Both commands render through the
//! one pipeline here, which is what keeps them byte-identical.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

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
    /// One commit that re-indents a line (whitespace only) and rewrites another.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-histopts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        let body: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        f.write("f.txt", body.as_bytes());
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);
        let edited = body
            .replace("line 6\n", "line 6   \n")
            .replace("line 12\n", "line twelve\n");
        f.write("f.txt", edited.as_bytes());
        f.git(&["commit", "-q", "-am", "edit"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `-w` drops the whitespace-only hunk, leaving only the real edit.
#[test]
fn whitespace_options_shape_the_commit_patch() {
    let f = Fixture::new("ws");
    let plain = f.stdout(&["log", "-p", "-1", "--format="]);
    assert!(plain.contains("-line 6\n"), "{plain}");

    for args in [
        ["log", "-p", "-1", "--format=", "-w"],
        ["log", "-p", "-1", "--format=", "-b"],
    ] {
        let out = f.stdout(&args);
        assert!(!out.contains("-line 6\n"), "{args:?}: {out}");
        assert!(out.contains("-line 12\n+line twelve\n"), "{args:?}: {out}");
    }
    // `show` renders through the same pipeline, so it agrees.
    assert_eq!(
        f.stdout(&["show", "-w", "--format=", "HEAD"]),
        f.stdout(&["log", "-p", "-1", "-w", "--format="])
    );
}

/// `-U<n>` sets the context, `-U0` leaves only changed lines.
#[test]
fn the_context_size_is_configurable() {
    let f = Fixture::new("ctx");
    let three = f.stdout(&["log", "-p", "-1", "--format="]);
    assert!(three.contains("@@ -3,10 +3,10 @@"), "{three}");

    let zero = f.stdout(&["log", "-p", "-1", "--format=", "-U0"]);
    assert!(zero.contains("@@ -6 +6 @@"), "{zero}");
    // No context lines survive; only `@@`, `-` and `+` bodies (the `@@` line still
    // carries the enclosing-line hint, which is not a context line).
    assert!(
        !zero.lines().any(|l| l.starts_with(' ')),
        "{zero}"
    );

    assert_eq!(
        f.stdout(&["show", "--unified=1", "--format=", "HEAD"]),
        f.stdout(&["log", "-p", "-1", "-U1", "--format="])
    );
}

/// `--full-index` and the prefix flags reach the header lines.
#[test]
fn the_header_options_reach_the_patch() {
    let f = Fixture::new("header");
    let full = f.stdout(&["log", "-p", "-1", "--format=", "--full-index"]);
    let index = full
        .lines()
        .find(|l| l.starts_with("index "))
        .expect("index line");
    // Two full 40-hex names plus the mode.
    assert_eq!(index.len(), "index ".len() + 40 + 2 + 40 + 1 + 6, "{index}");

    let noprefix = f.stdout(&["show", "--no-prefix", "--format=", "HEAD"]);
    assert!(noprefix.contains("diff --git f.txt f.txt\n"), "{noprefix}");
    assert!(noprefix.contains("--- f.txt\n"), "{noprefix}");

    let custom = f.stdout(&["show", "--src-prefix=x/", "--dst-prefix=y/", "--format=", "HEAD"]);
    assert!(custom.contains("diff --git x/f.txt y/f.txt\n"), "{custom}");
}
