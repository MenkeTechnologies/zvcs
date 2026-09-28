//! `git commit-tree -S` signs the commit it writes.
//!
//! `sign_commit` is an `OPTION_STRING` with `PARSE_OPT_OPTARG` and a `""`
//! default (builtin/commit-tree.c:115-124): `-S`/`--gpg-sign` sign with
//! `get_signing_key()`'s key (`user.signingKey`, else the committer ident),
//! `-S<key>`/`--gpg-sign=<key>` name one, `--no-gpg-sign` turns it off.
//! `commit_tree_extended()` runs the UTF-8 check first and signs the checked
//! buffer (commit.c:1618-1628), and `add_header_signature()` splices the
//! signature in as a `gpgsig` header (commit.c:1160-1190). A signing failure
//! is `error: gpg failed to sign the data:` and exit 1. zvcs refused `-S`.
//!
//! The fake `gpg.program` prints its arguments and a `cksum` of the payload it
//! was handed, so the payload itself is compared too.
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

const TREE: &str = "3be22be77da4887e869c981806d8452f034dd014";

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-commit-tree-sign-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        let gpg = f.root.join("gpg.sh");
        std::fs::write(
            &gpg,
            "#!/bin/sh\nsum=$(cksum | cut -d\" \" -f1)\n\
             echo \"[GNUPG:] SIG_CREATED D 1 8 00 1112911993 ABC\" >&2\n\
             printf -- \"-----BEGIN PGP SIGNATURE-----\\n\\n%s %s\\n-----END PGP SIGNATURE-----\\n\" \"$sum\" \"$*\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gpg, std::fs::Permissions::from_mode(0o755)).unwrap();
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-q", "-m", "init"]);
        f.ok(&["config", "gpg.program", gpg.to_str().unwrap()]);
        f
    }

    fn run(&self, args: &[&str]) -> (Vec<u8>, String, i32) {
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
            .output()
            .unwrap();
        (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        String::from_utf8(out).unwrap()
    }

    /// `cat-file commit` of what `commit-tree <args>` wrote.
    fn written(&self, args: &[&str]) -> Vec<u8> {
        let id = self.ok(args);
        self.run(&["cat-file", "commit", id.trim()]).0
    }
}

fn signed(parent: bool, sum: &str, key: &str, msg: &str) -> String {
    format!(
        "tree {TREE}\n{}author A U Thor <a@e.x> 1112911993 -0700\n\
         committer C O Mitter <c@e.x> 1112911993 -0700\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n \n {sum} --status-fd=2 -bsau {key}\n \
         -----END PGP SIGNATURE-----\n\n{msg}\n",
        if parent { "parent 28b87c48b7d0c681bf67477382616ee184161643\n" } else { "" }
    )
}

#[test]
fn s_signs_with_the_committer_ident_by_default() {
    let f = Fixture::new("default");
    let got = f.written(&["commit-tree", "-S", "-p", "HEAD", TREE, "-m", "x"]);
    assert_eq!(String::from_utf8(got).unwrap(), signed(true, "3002135681", "C O Mitter <c@e.x>", "x"));
}

#[test]
fn a_named_key_beats_user_signing_key_and_no_gpg_sign_turns_it_off() {
    let f = Fixture::new("key");
    let got = f.written(&["-c", "user.signingKey=K1", "commit-tree", "--gpg-sign=K2", TREE, "-m", "y"]);
    assert_eq!(String::from_utf8(got).unwrap(), signed(false, "3791506528", "K2", "y"));
    let plain = f.written(&["commit-tree", "-SK2", "--no-gpg-sign", TREE, "-m", "y"]);
    assert!(!plain.windows(6).any(|w| w == b"gpgsig"));
}

#[test]
fn the_signed_payload_is_the_utf8_checked_buffer() {
    let f = Fixture::new("utf8");
    std::fs::write(f.root.join("m"), b"caf\xe9\n").unwrap();
    let (id, err, code) = f.run(&["commit-tree", "-S", TREE, "-F", f.root.join("m").to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(err.starts_with("Warning: commit message did not conform to UTF-8.\n"), "{err}");
    let got = f.run(&["cat-file", "commit", String::from_utf8(id).unwrap().trim()]).0;
    assert_eq!(got, signed(false, "1750357142", "C O Mitter <c@e.x>", "caf\u{e9}").into_bytes());
}

#[test]
fn a_failing_signer_exits_1_without_a_commit() {
    let f = Fixture::new("fail");
    let (out, err, code) = f.run(&["-c", "gpg.program=false", "commit-tree", "-S", TREE, "-m", "z"]);
    assert_eq!(
        (out.as_slice(), err.as_str(), code),
        (&b""[..], "error: gpg failed to sign the data:\n(no gpg output)\n", 1)
    );
}
