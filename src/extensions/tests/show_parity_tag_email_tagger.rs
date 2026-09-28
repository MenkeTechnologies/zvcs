//! `git show --pretty=email <tag>` never Q-encodes the tagger.
//!
//! `show_tagger()` (builtin/log.c:570-577) puts the tagger line through
//! `pp_user_info()` with a zeroed `pretty_print_context` of its own, so
//! `encode_email_headers` is 0 whatever `format.encodeEmailHeaders` or
//! `--encode-email-headers` say, and a non-ASCII name goes out raw. zvcs used
//! the log's setting and printed `=?UTF-8?q?…?=`.
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
    /// One commit and an annotated tag `T1` by `Zoë Q`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-show-tag-email-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], &[]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"], &[]);
        f.run(&["commit", "-q", "-m", "one"], &[]);
        f.run(&["tag", "-a", "-m", "tag body", "T1"], &[("GIT_COMMITTER_NAME", "Zo\u{eb} Q")]);
        f
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
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
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn the_tagger_goes_out_raw() {
    let f = Fixture::new("raw");
    let want = "tag T1\nFrom: Zo\u{eb} Q <c@e.x>\nDate: Thu, 7 Apr 2005 15:13:13 -0700\n\ntag body\n";
    for extra in [&[][..], &["--encode-email-headers"][..]] {
        let mut args = vec!["show", "--pretty=email", "-s"];
        args.extend_from_slice(extra);
        args.push("T1");
        let (out, err, code) = f.run(&args, &[]);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        assert!(out.starts_with(want), "{args:?}: {out}");
    }
}
