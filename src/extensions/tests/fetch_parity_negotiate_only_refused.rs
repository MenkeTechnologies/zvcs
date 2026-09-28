//! `fetch --negotiate-only` against a server that cannot negotiate that way.
//!
//! `fetch_refs_via_pack()` (transport.c:487-495) answers a protocol v0/v1
//! server with `warning(_("--negotiate-only requires protocol v2"))` and
//! `ret = -1`, then closes the connection at `cleanup:` without the flush
//! `disconnect_git()` would send (transport.c:530-535): `upload-pack` reads
//! EOF where it expected wants and says `the remote end hung up
//! unexpectedly`. `cmd_fetch()` returns the -1, so the exit status is 255
//! and nothing is printed on stdout. zvcs reported `The server does not
//! support the 'wait-for-done' capability` as a `zvcs:` error at exit 1.
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
    /// `up` has `a-b` on `main`; `work` clones it and copies `origin/main` to
    /// branch `copy`; then `up` rewinds `main` to `a` and commits `c` on it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fetch-negotiate-only-v0-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root, work };
        let up = f.root.join("up");
        f.run_in(&f.root, &["init", "-q", "-b", "main", "up"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "b"]);
        f.run_in(&f.root, &["clone", "-q", "up", "work"]);
        f.run(&["branch", "copy", "origin/main"]);
        f.run_in(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        f.run_in(&up, &["commit", "-q", "--allow-empty", "-m", "c"]);
        f
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }


}

#[test]
fn a_v0_server_is_refused_with_a_warning_and_255() {
    let f = Fixture::new("v0");
    for version in ["0", "1"] {
        let (out, err, code) = f.run(&[
            "-c",
            &format!("protocol.version={version}"),
            "fetch",
            "--negotiate-only",
            "--negotiation-tip=main",
        ]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            (
                "",
                "warning: --negotiate-only requires protocol v2\n\
                 fatal: the remote end hung up unexpectedly\n",
                255
            )
        );
    }
}
