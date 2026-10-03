//! `git format-patch -R`.
//!
//! `-R` is `OPT_BOOL('R', NULL, &options->flags.reverse_diff, ...)` on the diff
//! table (`diff.c:6233`), which `format-patch` parses through `setup_revisions()`.
//! Every pair is swapped as it is queued (`diff_change()`/`diff_addremove()`), so
//! a patch runs new-to-old — an addition becomes a deletion — and
//! `builtin_diff()` names the sides with the prefixes swapped:
//!
//! ```c
//! if (o->flags.reverse_diff) {
//!         a_prefix = o->b_prefix;
//!         b_prefix = o->a_prefix;
//! ```
//! (`diff.c:3841-3843`, v2.56.0)
//!
//! The port rejected it with `unrecognized argument: -R`.
//! Expectations measured from stock git 2.56.0 under the same environment.
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
    /// `one` adds `f` (`a`); `two` changes it to `b` and adds `n`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fp-reverse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        std::fs::write(f.work.join("n"), "n\n").unwrap();
        f.git(&["add", "f", "n"]);
        f.git(&["commit", "-q", "-m", "two"]);
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
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@e")
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

    fn git(&self, args: &[&str]) {
        let (_, err, code) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}`: {err}");
    }

    /// The patch, from the `---` that opens the diffstat on.
    fn body(&self, args: &[&str]) -> String {
        let all = [&["format-patch", "--stdout", "--no-signature"][..], args].concat();
        let (out, err, code) = self.run(&all);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        let at = out.find("\n---\n").expect("no diffstat separator");
        out[at + 1..].to_string()
    }
}

#[test]
fn reverse_swaps_pairs_and_prefixes() {
    let f = Fixture::new("swap");
    assert_eq!(
        f.body(&["-R", "-1"]),
        "---
 f | 2 +-
 n | 1 -
 2 files changed, 1 insertion(+), 2 deletions(-)
 delete mode 100644 n

diff --git b/f a/f
index 6178079..7898192 100644
--- b/f
+++ a/f
@@ -1 +1 @@
-b
+a
diff --git b/n a/n
deleted file mode 100644
index 8ba3a16..0000000
--- b/n
+++ /dev/null
@@ -1 +0,0 @@
-n
"
    );
    // The root commit's creation becomes a deletion, summary line included.
    assert_eq!(
        f.body(&["-R", "--root", "-1", "HEAD~1"]),
        "---
 f | 1 -
 1 file changed, 1 deletion(-)
 delete mode 100644 f

diff --git b/f a/f
deleted file mode 100644
index 7898192..0000000
--- b/f
+++ /dev/null
@@ -1 +0,0 @@
-a
"
    );
    // Explicit prefixes swap with it.
    let named = f.body(&["-R", "--src-prefix=S/", "--dst-prefix=D/", "-1"]);
    assert!(named.contains("diff --git D/f S/f\n") && named.contains("--- D/n\n+++ /dev/null\n"), "{named}");
}
