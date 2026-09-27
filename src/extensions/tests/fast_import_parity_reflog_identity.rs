//! `fast-import` in a repository with no configured identity.
//!
//! The refs are written at the end of the run with a reflog entry whose ident
//! comes from `git_committer_info(0)` — the non-strict identity, which with no
//! `user.*` and no `GIT_COMMITTER_*` is synthesized from the account and host
//! (ident.c) instead of refused. Stock therefore imports and logs
//! `<login>@<host>.(none)`-style entries; zvcs died
//! `fatal: The reflog could not be created or updated` with nothing updated.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.
//! The synthesized ident depends on the machine, so only its shape is checked.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
            .join(format!("zvcs-fast-import-reflog-ident-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."], "");
        f
    }

    /// No identity anywhere: the environment is cleared down to `PATH`.
    fn run(&self, args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn refs_are_written_with_a_synthesized_reflog_identity() {
    let f = Fixture::new("synth");
    let stream = "commit refs/heads/x\n\
                  committer A <a@x> 1700000000 +0000\n\
                  data 0\n\n\
                  tag t\nfrom refs/heads/x\n\
                  tagger A <a@x> 1700000000 +0000\n\
                  data 0\n\n";
    let (out, err, code) = f.run(&["fast-import", "--quiet"], stream);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(
        f.run(&["for-each-ref"], "").0,
        "70d1b2009325fda12bac727268447a7e31c98715 commit\trefs/heads/x\n\
         bb643123cf34d2d084e3d16591ff10ed33f53fe0 tag\trefs/tags/t\n"
    );
    let log = std::fs::read_to_string(f.work.join(".git/logs/refs/heads/x")).unwrap();
    let (head, message) = log.trim_end().split_once('\t').unwrap();
    assert_eq!(message, "fast-import");
    assert!(
        head.starts_with(
            "0000000000000000000000000000000000000000 70d1b2009325fda12bac727268447a7e31c98715 "
        ),
        "{log}"
    );
    let ident = &head[82..];
    let lt = ident.find(" <").expect(&log);
    assert!(ident[lt..].contains("@") && ident.contains("> "), "{log}");
}
