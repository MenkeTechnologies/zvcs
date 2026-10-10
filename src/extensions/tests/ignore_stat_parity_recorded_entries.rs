//! `core.ignoreStat` marks every index entry whose stat `fill_stat_cache_info()` records.
//!
//! `read-cache.c` sets `CE_VALID` on the entry (`ls-files -v` shows the tag in lower case) for
//! each file `add`, a path checkout, `apply --index`/`am` and the worktree update of a rebase
//! store. zvcs already did it for branch switches and `checkout-index`; these paths recorded the
//! stat and left the tag upper case. Expectations measured from stock git 2.56.0.

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

impl Fixture {
    /// `main` has `README.md` changed and `new` added on top of `base`; `HEAD` is on `base`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ignorestat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("README.md"), "r\n").unwrap();
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["branch", "base"]);
        std::fs::write(f.root.join("README.md"), "r\nb\n").unwrap();
        std::fs::write(f.root.join("new"), "n\n").unwrap();
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "b"]);
        f.run(&["config", "core.ignoreStat", "true"]);
        f
    }

    fn run(&self, args: &[&str]) -> Vec<u8> {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    }

    fn tags(&self) -> String {
        String::from_utf8(self.run(&["ls-files", "-v"])).unwrap()
    }
}

#[test]
fn add_marks_what_it_stages() {
    let f = Fixture::new("add");
    std::fs::write(f.root.join("z"), "z\n").unwrap();
    f.run(&["add", "z"]);
    assert_eq!(f.tags(), "H README.md\nH a\nH new\nh z\n");
}

#[test]
fn a_path_checkout_marks_what_it_writes() {
    let f = Fixture::new("checkout");
    f.run(&["checkout", "-q", "base"]);
    f.run(&["checkout", "main", "--", "README.md"]);
    assert_eq!(f.tags(), "h README.md\nH a\n");
}

#[test]
fn am_marks_what_it_applies() {
    let f = Fixture::new("am");
    let patch = f.run(&["format-patch", "-1", "--stdout"]);
    f.run(&["checkout", "-q", "base"]);
    let mut am = Command::new(BIN)
        .args(["am"])
        .current_dir(&f.root)
        .env("HOME", &f.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::io::Write::write_all(am.stdin.as_mut().unwrap(), &patch).unwrap();
    assert!(am.wait().unwrap().success());
    assert_eq!(f.tags(), "h README.md\nH a\nh new\n");
}

#[test]
fn a_rebase_fast_forward_marks_what_it_rewrites() {
    let f = Fixture::new("rebase");
    f.run(&["checkout", "-q", "-b", "behind", "base"]);
    f.run(&["rebase", "main"]);
    assert_eq!(f.tags(), "h README.md\nH a\nh new\n");
}
