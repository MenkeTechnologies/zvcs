//! `format-patch` routes `To:`/`Cc:` headers into the recipient lists.
//!
//! `add_header()` (builtin/log.c:958-977), behind both `format.headers` and
//! `--add-header`, turns a value opening with `To: ` or `Cc: ` (any case) into
//! one more address for that list, so it is folded into the single `To:`/`Cc:`
//! header with the rest. `--no-to`/`--no-cc` are `OPT_STRING_LIST` negations
//! that empty the list, configured addresses included, and `--no-add-header`
//! (`header_callback()`, builtin/log.c:1649-1662) empties the headers and both
//! lists. zvcs printed such a header verbatim as an extra line and refused the
//! three negations.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-format-patch-header-lists-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        for n in ["1", "2"] {
            std::fs::write(f.work.join("a"), format!("{n}\n")).unwrap();
            f.ok(&["add", "a"]);
            f.ok(&["commit", "-q", "-m", n]);
        }
        f
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    /// The lines between `Subject:` and the blank line that ends the headers.
    fn headers(&self, args: &[&str]) -> String {
        let mut full = vec!["format-patch", "--stdout", "-1"];
        full.extend_from_slice(args);
        let out = self.ok(&full);
        let after = out.split_once("Subject: [PATCH] 2\n").expect("a subject").1;
        after.split_once("\n\n").map_or(after, |(h, _)| h).to_owned()
    }
}

#[test]
fn to_and_cc_headers_join_the_lists() {
    let f = Fixture::new("cli");
    assert_eq!(
        f.headers(&["--add-header=To: t@x", "--add-header=cc: c@x", "--to=u@x"]),
        "To: t@x,\n    u@x\nCc: c@x"
    );
    // `format.headers` goes through the same function, in configuration order.
    f.ok(&["config", "format.headers", "X-A: 1"]);
    f.ok(&["config", "--add", "format.to", "f@x"]);
    f.ok(&["config", "--add", "format.headers", "TO: g@x"]);
    assert_eq!(f.headers(&[]), "X-A: 1\nTo: f@x,\n    g@x");
}

#[test]
fn the_negations_empty_the_lists() {
    let f = Fixture::new("neg");
    f.ok(&["config", "format.to", "f@x"]);
    f.ok(&["config", "format.cc", "k@x"]);
    assert_eq!(f.headers(&["--no-to"]), "Cc: k@x");
    assert_eq!(f.headers(&["--no-cc", "--to=z@x"]), "To: f@x,\n    z@x");
    assert_eq!(
        f.headers(&["--add-header=A: b", "--no-add-header", "--add-header=C: d", "--to=q@q"]),
        "C: d\nTo: q@q"
    );
}
