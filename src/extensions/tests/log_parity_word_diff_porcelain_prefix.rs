//! `git log -p` / `git show` with `--word-diff=porcelain --line-prefix`, and an
//! external driver's output under `--line-prefix`.
//!
//! The history verbs hand the same `diff_options` to `diff_flush()` that `git
//! diff` does (log-tree.c `log_tree_diff_flush()`), so the word diff's
//! `DIFF_SYMBOL_WORD_DIFF` records print verbatim (diff.c:1622-1623) with the
//! prefix only where `fn_out_diff_words_write_helper()` and `diff_words_show()`
//! write it (diff.c:2019-2020, 2127-2129, 2237-2275), and porcelain's `~` lines
//! follow bare (diff.c:1547-1552). An external driver writes to git's own
//! descriptor (`run_external_diff()`, diff.c), so `diff_line_prefix()` never
//! reaches its output either. zvcs prefixed every line of the record afterwards.
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
    /// `a` goes from "two" to "hello world\nsecond line\n" in the second commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-word-diff-prefix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a"), "hello world\nsecond line\n").unwrap();
        f.run(&["commit", "-q", "-am", "change"]);
        f
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
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
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_env(args, &[])
    }
}

const RECORD: &str = "Pchange\nP\nPdiff --git a/a b/a\nPindex f719efd..f0e9ea9 100644\nP--- a/a\nP+++ b/a\n\
                      P@@ -1 +1,2 @@\nP-two\n+hello world\n~\nP+second line\n~\n";

#[test]
fn log_and_show_leave_the_word_records_their_own_prefixes() {
    let f = Fixture::new("words");
    for args in [
        &["log", "-1", "-p", "--word-diff=porcelain", "--line-prefix=P", "--format=%s"][..],
        &["show", "--word-diff=porcelain", "--line-prefix=P", "--format=%s"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), (RECORD, "", 0), "{args:?}");
    }
}

#[test]
fn an_external_driver_writes_past_the_prefix() {
    let f = Fixture::new("ext");
    let script = f.root.join("ext.sh");
    std::fs::write(&script, "#!/bin/sh\necho \"ext $1\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = [("GIT_EXTERNAL_DIFF", script.to_str().unwrap())];
    for args in [
        &["log", "-1", "-p", "--ext-diff", "--line-prefix=P", "--format=%s"][..],
        &["show", "--ext-diff", "--line-prefix=P", "--format=%s"][..],
    ] {
        let (out, err, code) = f.run_env(args, &env);
        assert_eq!((out.as_str(), err.as_str(), code), ("Pchange\nP\next a\n", "", 0), "{args:?}");
    }
}
