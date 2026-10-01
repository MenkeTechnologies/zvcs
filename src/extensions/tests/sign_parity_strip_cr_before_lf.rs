//! A signature the signer returns with CRLF line endings is stored with LF
//! endings, and a CR anywhere else is kept.
//!
//! `strip_cr_before_lf()` (gpg-interface.c:993-1006, v2.56.0) runs over the
//! output of both `sign_buffer_gpg()` (:1049-1050) and `sign_buffer_ssh()`
//! (:1136-1137). 2.55's `remove_cr_after()` dropped every CR; zvcs dropped none,
//! so a CRLF-emitting signer left CRs inside the `gpgsig` header and the tag
//! body.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Prints a status line gpg would, then an armored "signature" with CRLF line
/// endings and one lone CR.
const FAKE_GPG: &str = "#!/bin/sh\ncat >/dev/null\n\
    echo '[GNUPG:] SIG_CREATED D 1 8 00 1700000000 ABC' >&2\n\
    printf -- '-----BEGIN PGP SIGNATURE-----\\r\\n\\r\\nab\\rcd\\r\\n-----END PGP SIGNATURE-----\\r\\n'\n";

/// Writes `<buffer>.sig` the way `ssh-keygen -Y sign` does, CRLF endings and a
/// lone CR.
const FAKE_SSH: &str = "#!/bin/sh\nfor last; do :; done\n\
    printf -- '-----BEGIN SSH SIGNATURE-----\\r\\nab\\rcd\\r\\n-----END SSH SIGNATURE-----\\r\\n' > \"$last.sig\"\n";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("zvcs-sign-strip-cr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
        for (name, body) in [("fakegpg", FAKE_GPG), ("fakessh", FAKE_SSH)] {
            let path = f.root.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let r = f.root.clone();
        f.git(&r, &["init", "-q", "-b", "main", "r"]);
        f
    }

    fn repo(&self) -> PathBuf {
        self.root.join("r")
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn gpg_config(&self) -> [String; 4] {
        [
            "-c".into(),
            format!("gpg.program={}", self.root.join("fakegpg").display()),
            "-c".into(),
            "user.signingkey=K".into(),
        ]
    }
}

#[test]
fn openpgp_commit_signature_keeps_only_lone_crs() {
    let f = Fixture::new("gpg-commit");
    let cfg = f.gpg_config();
    let mut args: Vec<&str> = cfg.iter().map(String::as_str).collect();
    args.extend(["commit", "-q", "--allow-empty", "-S", "-m", "m"]);
    f.git(&f.repo(), &args);
    assert_eq!(
        f.git(&f.repo(), &["cat-file", "commit", "HEAD"]),
        "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
         author A <a@x> 1700000000 +0000\n\
         committer A <a@x> 1700000000 +0000\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n \n ab\rcd\n -----END PGP SIGNATURE-----\n\nm\n"
    );
    assert_eq!(f.git(&f.repo(), &["rev-parse", "HEAD"]), "11b9e5a6946e2def8840681e531fb958e56c1097\n");
}

#[test]
fn openpgp_tag_signature_keeps_only_lone_crs() {
    let f = Fixture::new("gpg-tag");
    f.git(&f.repo(), &["commit", "-q", "--allow-empty", "-m", "m"]);
    let cfg = f.gpg_config();
    let mut args: Vec<&str> = cfg.iter().map(String::as_str).collect();
    args.extend(["tag", "-s", "-m", "t", "t1"]);
    f.git(&f.repo(), &args);
    let body = f.git(&f.repo(), &["cat-file", "tag", "t1"]);
    assert!(
        body.ends_with("tagger A <a@x> 1700000000 +0000\n\nt\n-----BEGIN PGP SIGNATURE-----\n\nab\rcd\n-----END PGP SIGNATURE-----\n"),
        "{body:?}"
    );
}

#[test]
fn ssh_commit_signature_keeps_only_lone_crs() {
    let f = Fixture::new("ssh-commit");
    let program = format!("gpg.ssh.program={}", f.root.join("fakessh").display());
    f.git(
        &f.repo(),
        &[
            "-c", "gpg.format=ssh", "-c", &program, "-c", "user.signingkey=/nonexist/key",
            "commit", "-q", "--allow-empty", "-S", "-m", "s",
        ],
    );
    assert_eq!(
        f.git(&f.repo(), &["cat-file", "commit", "HEAD"]),
        "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
         author A <a@x> 1700000000 +0000\n\
         committer A <a@x> 1700000000 +0000\n\
         gpgsig -----BEGIN SSH SIGNATURE-----\n ab\rcd\n -----END SSH SIGNATURE-----\n\ns\n"
    );
}
