//! The v2 `bundle-uri` command omits a `bundle.*` entry with no value, and
//! warns about it, rather than sending a `key=` line the client cannot parse.
//!
//! `config_to_packet_line()` (bundle-uri.c:945-957, v2.56.0) writes
//! `key=value` only `if (value && *value)` and otherwise
//! `warning(_("config '%s' has no value"), key)`. 2.55 sent every entry; zvcs
//! sent the empty `key=` and silently skipped a valueless one.
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

#[test]
fn valueless_and_empty_bundle_entries_are_warned_about_not_sent() {
    let root = std::env::temp_dir().join(format!("zvcs-bundle-uri-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
    let env = |c: &mut Command| {
        c.current_dir(&f.root)
            .env_remove("GIT_DIR")
            .env("HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C");
    };
    let mut init = Command::new(BIN);
    env(&mut init);
    assert!(init.args(["init", "-q", "--bare", "r.git"]).status().unwrap().success());
    let mut cfg = std::fs::OpenOptions::new().append(true).open(f.root.join("r.git/config")).unwrap();
    cfg.write_all(
        b"[uploadpack]\n\tadvertiseBundleURIs = true\n[bundle]\n\tversion = 1\n\tmode\n\tempty =\n\
          [bundle \"x\"]\n\turi = file:///tmp/x\n",
    )
    .unwrap();

    let mut cmd = Command::new(BIN);
    env(&mut cmd);
    let mut child = cmd
        .args(["upload-pack", "--stateless-rpc", "r.git"])
        .env("GIT_PROTOCOL", "version=2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"0017command=bundle-uri\n0000").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code()
        ),
        (
            "0014bundle.version=1001ebundle.x.uri=file:///tmp/x0000".to_owned(),
            "warning: config 'bundle.mode' has no value\nwarning: config 'bundle.empty' has no value\n".to_owned(),
            Some(0)
        )
    );
}
