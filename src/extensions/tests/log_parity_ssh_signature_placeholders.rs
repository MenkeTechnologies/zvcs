//! The `%G…` placeholders (pretty.c:1659-1710) on ssh-signed commits, read
//! through the shared verifier. `parse_ssh_output()` (gpg-interface.c:398-459)
//! sets `sigc->fingerprint` and `sigc->key` to everything after `key ` on
//! ssh-keygen's first line, `signer` to the principal and `trust_level` to
//! `TRUST_FULLY` for a known principal or `TRUST_UNDEFINED` for a valid key
//! no allowed-signers entry names; `primary_key_fingerprint` stays unset, so
//! `%GP` is empty. An unsigned commit prints every field's initial value.
//!
//! Every expectation is stock git's output for the same command on the same
//! fixture. The keys are generated here with ssh-keygen; no user key is read.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin")
        .env("HOME", root)
        .env("ZVCS_HOME", root.join(".zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

/// Whether this machine's ssh-keygen has the `-Y` signature subcommands.
fn ssh_signing_available() -> bool {
    match Command::new("ssh-keygen").args(["-Y", "check-novalidate", "-h"]).output() {
        Ok(o) => !String::from_utf8_lossy(&o.stderr).contains("unknown option"),
        Err(_) => false,
    }
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// An unsigned commit, one signed by a key `gpg.ssh.allowedSignersFile`
    /// names, and one signed by a key it does not.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git()?;
        if !ssh_signing_available() {
            return None;
        }
        let root = std::env::temp_dir().join(format!("zvcs-ssh-sig-pretty-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let known = root.join("known");
        let other = root.join("other");
        for key in [&known, &other] {
            let out = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "test", "-f"])
                .arg(key)
                .output()
                .unwrap();
            assert!(out.status.success(), "ssh-keygen: {}", String::from_utf8_lossy(&out.stderr));
        }
        let public = std::fs::read_to_string(known.with_extension("pub")).unwrap();
        let allowed = root.join("allowed");
        std::fs::write(&allowed, format!("author@example.com {public}")).unwrap();

        let fx = Fixture { root, work, stock };
        fx.stock(&["init", "-q", "-b", "main", "."]);
        fx.stock(&["config", "gpg.format", "ssh"]);
        fx.stock(&["config", "gpg.ssh.allowedSignersFile", allowed.to_str().unwrap()]);
        fx.stock(&["commit", "-q", "--allow-empty", "-m", "unsigned"]);
        let known_cfg = format!("user.signingkey={}", known.display());
        fx.stock(&["-c", &known_cfg, "commit", "-q", "--allow-empty", "-S", "-m", "known"]);
        let other_cfg = format!("user.signingkey={}", other.display());
        fx.stock(&["-c", &other_cfg, "commit", "-q", "--allow-empty", "-S", "-m", "unknown"]);
        Some(fx)
    }

    fn stock(&self, args: &[&str]) {
        let out = run(self.stock, &self.work, &self.root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn same(&self, args: &[&str]) {
        let want = run(self.stock, &self.work, &self.root, args);
        let got = run(BIN, &self.work, &self.root, args);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
            (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
            "{args:?}"
        );
    }
}

/// `%GF` and `%GK` carry the `SHA256:` fingerprint ssh-keygen printed, `%GS` the
/// principal, `%GT` `fully`/`undefined`, and `%GP` nothing, for each commit.
#[test]
fn ssh_fingerprint_trust_and_signer_fields() {
    let Some(fx) = Fixture::new("fields") else { return };
    fx.same(&["log", "--format=[%GF][%GP][%GT][%GS][%GK][%G?]"]);
    for one in ["%GF", "%GP", "%GT", "%GS", "%GK", "%G?", "%GG"] {
        fx.same(&["log", &format!("--format={one}|")]);
    }
    for rev in ["HEAD", "HEAD~1", "HEAD~2"] {
        fx.same(&["show", "-s", "--format=%GF %GT %G?", rev]);
    }
}
