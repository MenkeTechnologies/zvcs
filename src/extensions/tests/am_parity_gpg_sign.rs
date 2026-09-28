//! `git am -S` signs every commit it writes.
//!
//! `state->sign_commit` starts from `commit.gpgSign` (`""` when true;
//! builtin/am.c:176-177), `-S[<key-id>]`/`--gpg-sign[=<key-id>]`/`--no-gpg-sign`
//! overwrite it (builtin/am.c:2429-2438), and `do_commit()` hands it to
//! `commit_tree_extended()`, dying with `failed to write commit object` when
//! signing fails (builtin/am.c:1704-1707). It is not saved in the state
//! directory, so `--continue -S` signs by its own command line. zvcs refused `-S`.
//!
//! The fake `gpg.program` prints its arguments and a `cksum` of the payload.
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
    /// `main` holds `one`; `side` adds `change two` and `change three`, which are
    /// written to `../mbox`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-am-gpg-sign-{tag}-{}", std::process::id()));
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
        let file = |s: &str| std::fs::write(f.work.join("f"), s).unwrap();
        f.ok(&["init", "-q", "-b", "main", "."]);
        file("1\n2\n3\n");
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-q", "-m", "one"]);
        f.ok(&["checkout", "-q", "-b", "side"]);
        file("1\ntwo\n3\n");
        f.ok(&["commit", "-q", "-am", "change two"]);
        file("1\ntwo\nthree\n");
        f.ok(&["commit", "-q", "-am", "change three"]);
        let mbox = f.ok(&["format-patch", "-2", "--stdout"]);
        std::fs::write(f.root.join("mbox"), mbox).unwrap();
        f.ok(&["checkout", "-q", "main"]);
        f.ok(&["config", "gpg.program", gpg.to_str().unwrap()]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    fn mbox(&self) -> String {
        self.root.join("mbox").to_str().unwrap().to_owned()
    }

    /// The signature line of `rev`'s `gpgsig` header, or `None` for an unsigned
    /// commit.
    fn sig_line(&self, rev: &str) -> Option<String> {
        let raw = self.ok(&["cat-file", "commit", rev]);
        raw.lines().find(|l| l.contains("-bsau")).map(|l| l.trim().to_owned())
    }
}

#[test]
fn s_signs_every_applied_commit() {
    let f = Fixture::new("s");
    let mbox = f.mbox();
    assert_eq!(f.ok(&["am", "-S", &mbox]), "Applying: change two\nApplying: change three\n");
    assert_eq!(
        f.ok(&["cat-file", "commit", "HEAD"]),
        "tree 070b0158e8d1a8e2d124c949610b2746f55a9243\n\
         parent 644c10511cce104f43475f3cade0020060576156\n\
         author A U Thor <a@e.x> 1112911993 -0700\n\
         committer C O Mitter <c@e.x> 1112911993 -0700\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n \n \
         4105927157 --status-fd=2 -bsau C O Mitter <c@e.x>\n \
         -----END PGP SIGNATURE-----\n\nchange three\n"
    );
    assert_eq!(
        f.sig_line("HEAD^").as_deref(),
        Some("3005611055 --status-fd=2 -bsau C O Mitter <c@e.x>")
    );
}

#[test]
fn commit_gpgsign_is_the_default_and_no_gpg_sign_overrides_it() {
    let f = Fixture::new("config");
    let mbox = f.mbox();
    f.ok(&["config", "commit.gpgSign", "true"]);
    f.ok(&["am", "--no-gpg-sign", &mbox]);
    assert_eq!(f.sig_line("HEAD"), None);
    f.ok(&["reset", "-q", "--hard", "HEAD~2"]);
    f.ok(&["am", &mbox]);
    assert!(f.sig_line("HEAD").unwrap().ends_with("-bsau C O Mitter <c@e.x>"));
    f.ok(&["reset", "-q", "--hard", "HEAD~2"]);
    f.ok(&["am", "-SKX", &mbox]);
    assert!(f.sig_line("HEAD").unwrap().ends_with("-bsau KX"));
}

#[test]
fn a_signing_failure_stops_with_the_state_directory_kept() {
    let f = Fixture::new("fail");
    let mbox = f.mbox();
    f.ok(&["config", "gpg.program", "false"]);
    let (out, err, code) = f.run(&["am", "-S", &mbox]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "Applying: change two\n",
            "error: gpg failed to sign the data:\n(no gpg output)\nfatal: failed to write commit object\n",
            128
        )
    );
    assert!(f.work.join(".git/rebase-apply/next").exists());
    assert_eq!(f.ok(&["log", "--format=%s"]), "one\n");
}

#[test]
fn continue_signs_by_its_own_command_line() {
    let f = Fixture::new("continue");
    let mbox = f.mbox();
    std::fs::write(f.work.join("f"), "1\n2\nX\n").unwrap();
    f.ok(&["commit", "-q", "-am", "conflict"]);
    let (_, _, code) = f.run(&["am", "-3", &mbox]);
    assert_eq!(code, 128);
    std::fs::write(f.work.join("f"), "1\ntwo\n3\n").unwrap();
    f.ok(&["add", "f"]);
    assert_eq!(f.ok(&["am", "-S", "--continue"]), "Applying: change two\nApplying: change three\n");
    assert_eq!(
        f.sig_line("HEAD").as_deref(),
        Some("3310630201 --status-fd=2 -bsau C O Mitter <c@e.x>")
    );
}
