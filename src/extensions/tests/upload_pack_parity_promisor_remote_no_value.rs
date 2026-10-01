//! A v2 request carrying a bare `promisor-remote` (no `=<names>`) is refused.
//!
//! `promisor_remote_receive()` (serve.c:46-53, v2.56.0) dies with
//! `promisor-remote capability requires an argument` when the value is NULL;
//! 2.55 handed the NULL to `mark_promisor_remotes_as_accepted()` and crashed.
//! zvcs ignored the capability and answered the command. The capability is
//! admitted whether or not it was advertised (`promisor_remote_advertise()`
//! returns 1 when asked with a NULL buffer), so a plain repository shows it.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// `tag` keeps the tests, which run in parallel, out of each other's directory.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-promisor-remote-no-value-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
        let status = f.command().args(["init", "-q", "--bare", "r.git"]).status().unwrap();
        assert!(status.success());
        f
    }

    fn command(&self) -> Command {
        let mut c = Command::new(BIN);
        c.current_dir(&self.root)
            .env_remove("GIT_DIR")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C");
        c
    }

    /// `command=ls-refs` with `capability` among its capability lines.
    fn ls_refs_with(&self, capability: &str) -> (String, String, Option<i32>) {
        let mut child = self
            .command()
            .args(["upload-pack", "--stateless-rpc", "r.git"])
            .env("GIT_PROTOCOL", "version=2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = format!(
            "0014command=ls-refs\n{:04x}{capability}\n00010000",
            capability.len() + 5
        );
        child.stdin.take().unwrap().write_all(request.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
        )
    }
}

#[test]
fn a_bare_promisor_remote_capability_is_fatal() {
    let f = Fixture::new("bare");
    assert_eq!(
        f.ls_refs_with("promisor-remote"),
        (
            String::new(),
            "fatal: promisor-remote capability requires an argument\n".to_owned(),
            Some(128)
        )
    );
}

#[test]
fn a_promisor_remote_with_names_is_still_accepted() {
    let f = Fixture::new("names");
    assert_eq!(
        f.ls_refs_with("promisor-remote=x"),
        (
            "0000".to_owned(),
            "warning: accepted promisor remote 'x' not found\n".to_owned(),
            Some(0)
        )
    );
}
