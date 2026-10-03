//! `git show --color[=<when>]`.
//!
//! `--color` is `OPT_COLOR_FLAG` on the diff table `cmd_show()` parses through
//! `setup_revisions()`; it writes `diffopt.use_color`, the slot `--color-words`
//! and `--word-diff=color` also write, so the last spelling wins. With it on,
//! `log_tree_commit()` paints the header and patch, and `cmd_show()` paints its
//! own `tag`/`tree` lines in the `commit` color:
//!
//! ```c
//! printf("%stag %s%s\n",
//!        diff_get_color_opt(&rev.diffopt, DIFF_COMMIT),
//!        t->tag,
//!        diff_get_color_opt(&rev.diffopt, DIFF_RESET));
//! ```
//! (`builtin/log.c:717-719`, v2.56.0)
//!
//! The port refused `--color` and `--color=always` as unsupported options.
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
    /// Two commits of `f` (`a`, then `b`) and an annotated `v1` on the second.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-show-color-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "two"]);
        f.git(&["tag", "-a", "-m", "msg", "v1"]);
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
            .env("GIT_PAGER", "cat")
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

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }
}

const TAG_SHOWN: &str = "\x1b[33mtag v1\x1b[m
Tagger: C <c@e>
Date:   Tue Nov 14 22:13:20 2023 +0000

msg

\x1b[33mcommit 9708fe07346c7e717ce0c8d7e8de3ab62d307a0a\x1b[m
Author: A <a@e>
Date:   Tue Nov 14 22:13:20 2023 +0000

    two

\x1b[1mdiff --git a/f b/f\x1b[m
\x1b[1mindex 7898192..6178079 100644\x1b[m
\x1b[1m--- a/f\x1b[m
\x1b[1m+++ b/f\x1b[m
\x1b[36m@@ -1 +1 @@\x1b[m
\x1b[31m-a\x1b[m
\x1b[32m+\x1b[m\x1b[32mb\x1b[m
";

#[test]
fn color_flag_paints_tag_header_and_patch() {
    let f = Fixture::new("paint");
    for flag in ["--color", "--color=always", "--color=ALWAYS"] {
        assert_eq!(f.ok(&["show", flag, "v1"]), TAG_SHOWN, "{flag}");
    }
    // `color.ui=always` reaches the same slot when no flag is given.
    assert_eq!(f.ok(&["-c", "color.ui=always", "show", "v1"]), TAG_SHOWN);
    let plain = f.ok(&["show", "v1"]);
    assert!(!plain.contains('\x1b'));
    for flag in ["--no-color", "--color=never", "--color=auto"] {
        assert_eq!(f.ok(&["show", flag, "v1"]), plain, "{flag}");
    }
    // One slot: a later `--no-color` undoes `--color-words`.
    assert_eq!(f.ok(&["show", "--color-words", "--no-color", "-s", "--format=%s"]), "two\n");
    let tree = f.ok(&["show", "--color", "HEAD^{tree}"]);
    assert_eq!(tree, "\x1b[33mtree HEAD^{tree}\x1b[m\n\nf\n");
}

#[test]
fn bad_color_value_is_a_usage_error() {
    let f = Fixture::new("bad");
    assert_eq!(
        f.run(&["show", "--color=bogus"]),
        (String::new(), "error: option `color' expects \"always\", \"auto\", or \"never\"\n".into(), 129)
    );
}
