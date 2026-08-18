//! The signing backend: `sign_buffer()`'s argument vector, its success test, the
//! per-format program table, `gpg.ssh.defaultKeyCommand`, and the sequencer verbs
//! that were refusing to sign at all.
//!
//! Every expectation below was measured against stock git 2.55.0 rather than
//! derived from the C, and each test names the divergence it pins:
//!
//! * **`--status-fd=2` was decorative.** `sign_buffer_gpg()` (gpg-interface.c:1010)
//!   asks gpg for its status stream *because success is `[GNUPG:] SIG_CREATED ` at
//!   the start of a line, not a zero exit* — `ret |= !cp`. Reading only the exit
//!   status let a gpg that exits 0 without signing put whatever it left on stdout
//!   into the `gpgsig` header: a commit that reports as signed, at exit 0, with no
//!   diagnostic. That is the one failure a signing command must not have, which is
//!   why three tests here drive fake `gpg` programs that exit 0 and sign nothing.
//! * **`--batch --no-tty` were ours, not git's.** Both suppress the pinentry prompt
//!   a passphrase-protected key needs, so they turn a signable key into a signing
//!   failure. git passes exactly `<program> --status-fd=2 -bsau <key>`.
//! * **the program table is per format.** `gpg.program` names the *openpgp* entry
//!   only (gpg-interface.c:788-807); an `x509` signer with just `gpg.program` set
//!   still runs `gpgsm`. `gpg.openpgp.program` and `gpg.x509.program` were not read
//!   at all.
//! * **`gpg.ssh.defaultKeyCommand` was unimplemented**, so an ssh signer with no
//!   `user.signingKey` reported `user.signingKey needs to be set for ssh signing`
//!   — which git prints only *after* a key command has run and produced nothing.
//!   With no command configured git dies with a different sentence naming both
//!   settings.
//! * **`revert -S` / `cherry-pick -S` were refused**, and `commit.gpgSign` was
//!   ignored by both — the second being the worse half, since it wrote unsigned
//!   commits at exit 0 in a repository that had asked for signed ones.
//!
//! No test needs a real gpg, gpgsm or ssh-agent: the openpgp cases drive a small
//! `/bin/sh` stand-in whose argv is recorded, which is what makes them runnable in
//! a headless Linux CI. The two that genuinely need `ssh-keygen -Y sign` probe for
//! it and skip loudly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repository plus the isolated environment every run needs. `home` keeps the
/// developer's `~/.gitconfig` out (this machine's sets `core.commentChar`, which
/// changes message cleanup), and `argv_log` is where the stand-in gpg records the
/// argument vector it was handed.
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    argv_log: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("zvcs-gitsig-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { argv_log: root.join("argv"), root, repo, home };
        f.ok(&["init", "-q", "-b", "main"]);
        f.ok(&["config", "user.name", "C O Mitter"]);
        f.ok(&["config", "user.email", "committer@example.com"]);
        f.write("a.txt", b"one\n");
        f.ok(&["add", "a.txt"]);
        f.ok(&["commit", "-q", "-m", "one"]);
        f.write("b.txt", b"two\n");
        f.ok(&["add", "b.txt"]);
        f.ok(&["commit", "-q", "-m", "two"]);
        f
    }

    fn write(&self, name: &str, body: &[u8]) {
        std::fs::write(self.repo.join(name), body).unwrap();
    }

    fn config(&self, key: &str, value: &str) {
        self.ok(&["config", key, value]);
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
            .env("ARGV_LOG", &self.argv_log)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "git {args:?} failed ({:?}):\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The raw HEAD commit object, which is where a `gpgsig` header is visible.
    fn head_object(&self) -> String {
        self.ok(&["cat-file", "commit", "HEAD"])
    }

    fn head_id(&self) -> String {
        self.ok(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Install an executable `/bin/sh` script and return its path, for use as a
    /// `gpg.*.program`.
    fn program(&self, name: &str, body: &str) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        make_executable(&path);
        path.to_str().unwrap().to_owned()
    }

    /// What the stand-in gpg was invoked with, one argument per line.
    fn recorded_argv(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// A stand-in gpg that behaves: records argv, drains the payload, prints the
/// `SIG_CREATED` status line on fd 2 and an armored block on stdout.
const GPG_OK: &str = r#"printf '%s\n' "$@" >> "$ARGV_LOG"
cat > /dev/null
echo "[GNUPG:] SIG_CREATED D 1 8 00 0 0" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nfake\n-----END PGP SIGNATURE-----\n'
"#;

/// `-----BEGIN PGP SIGNATURE-----` on stdout, exit 0, and *no* `SIG_CREATED`.
/// gpg reaches this shape on real error paths; git rejects it, and reading only
/// the exit status accepts it.
const GPG_NO_STATUS: &str = r#"cat > /dev/null
echo "gpg: some chatter" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nfake\n-----END PGP SIGNATURE-----\n'
"#;

/// `SIG_CREATED` present but not at the start of a line, which git's
/// `cp == gpg_status.buf || cp[-1] == '\n'` walk rejects. A substring search
/// accepts it — and a signature line quoted back inside gpg's own error text is
/// exactly how that would happen in the wild.
const GPG_MIDLINE_STATUS: &str = r#"cat > /dev/null
printf 'junk [GNUPG:] SIG_CREATED D 1 8 00 0 0\n' >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nfake\n-----END PGP SIGNATURE-----\n'
"#;

/// A stand-in signer that records its argv and fails, for the cases that only need
/// to see *how* the backend was invoked. `ssh-keygen -Y sign` leaves its output in
/// `<buffer>.sig` rather than on stdout, so failing here is enough.
const RECORD_AND_FAIL: &str = r#"printf '%s\n' "$@" >> "$ARGV_LOG"
while [ $# -gt 0 ]; do
  if [ "$1" = "-f" ]; then cp "$2" "$ARGV_LOG.key"; fi
  shift
done
exit 9
"#;

fn is_unix() -> bool {
    cfg!(unix)
}

/// Whether `ssh-keygen -Y sign` exists here (openssh 8.2p1+). Without it the two
/// ssh end-to-end tests cannot run at all, and skipping is the honest outcome.
fn ssh_signing_available() -> bool {
    let Ok(out) = Command::new("ssh-keygen").arg("-Y").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stderr);
    text.contains("sign") || text.contains("Usage")
}

/// Generate a throwaway ed25519 key pair in `dir`, returning the private key path.
fn keygen(dir: &Path) -> Option<PathBuf> {
    let key = dir.join("id_ed25519");
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "tester", "-f"])
        .arg(&key)
        .status()
        .ok()?;
    status.success().then_some(key)
}

// ---------------------------------------------------------------------------
// sign_buffer_gpg: the argument vector and the SIG_CREATED success test
// ---------------------------------------------------------------------------

/// `strvec_pushl(&gpg.args, use_format->program, "--status-fd=2", "-bsau",
/// signing_key, NULL)` — four words, in that order, and nothing else.
///
/// Stock git 2.55.0, measured: `--status-fd=2|-bsau|KEYID`.
///
/// `--batch` and `--no-tty` are the two that mattered: gpg reads them as "never
/// ask", so a key whose passphrase is not already cached stops signing instead of
/// prompting. Nothing in the output says why.
#[test]
fn gpg_argv_is_exactly_status_fd_and_bsau() {
    if !is_unix() {
        eprintln!("SKIP gpg_argv_is_exactly_status_fd_and_bsau: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("argv");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "KEYID");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);
    f.ok(&["commit", "-S", "-m", "signed"]);

    assert_eq!(
        f.recorded_argv(),
        vec!["--status-fd=2".to_string(), "-bsau".to_string(), "KEYID".to_string()],
        "gpg argument vector"
    );
}

/// `-S<keyid>` overrides `user.signingKey` and reaches gpg as the `-bsau` operand.
#[test]
fn dash_s_keyid_overrides_user_signingkey() {
    if !is_unix() {
        eprintln!("SKIP dash_s_keyid_overrides_user_signingkey: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("keyid");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "CFGKEY");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);
    f.ok(&["commit", "-SFLAGKEY", "-m", "signed"]);

    assert_eq!(f.recorded_argv().last().map(String::as_str), Some("FLAGKEY"));
}

/// A gpg that exits 0 and emits no `SIG_CREATED` has not signed anything.
///
/// Stock git 2.55.0, measured:
///
/// ```text
/// error: gpg failed to sign the data:
/// gpg: some chatter
///
/// fatal: failed to write commit object
/// ```
///
/// exit 128, and HEAD does not move. Accepting the exit status instead wrote a
/// commit at exit 0 whose `gpgsig` header held the four lines the fake printed —
/// a commit that `git log --show-signature` calls signed and no verifier accepts.
#[test]
fn gpg_without_sig_created_is_a_failure_not_a_signature() {
    if !is_unix() {
        eprintln!("SKIP gpg_without_sig_created_is_a_failure_not_a_signature: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("nostatus");
    f.config("gpg.program", &f.program("gpg-nostatus", GPG_NO_STATUS));
    f.config("user.signingKey", "KEYID");
    let before = f.head_id();
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(out.status.code(), Some(128), "exit code");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: gpg failed to sign the data:\ngpg: some chatter\n\nfatal: failed to write commit object\n",
    );
    assert_eq!(f.head_id(), before, "HEAD must not move when signing failed");
}

/// The `SIG_CREATED` search is anchored to a line start, so a status token quoted
/// inside other text does not count as a signature.
#[test]
fn sig_created_must_start_a_line() {
    if !is_unix() {
        eprintln!("SKIP sig_created_must_start_a_line: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("midline");
    f.config("gpg.program", &f.program("gpg-midline", GPG_MIDLINE_STATUS));
    f.config("user.signingKey", "KEYID");
    let before = f.head_id();
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(out.status.code(), Some(128), "exit code");
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("error: gpg failed to sign the data:\n"),
        "stderr was:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.head_id(), before, "HEAD must not move");
}

/// `gpg_status.len ? gpg_status.buf : "(no gpg output)"` — a gpg that says nothing
/// at all still produces a readable diagnostic rather than a blank line.
///
/// Stock git 2.55.0, measured, for a `gpg` that reads its input and exits 0:
///
/// ```text
/// error: gpg failed to sign the data:
/// (no gpg output)
/// fatal: failed to write commit object
/// ```
#[test]
fn silent_gpg_reports_no_gpg_output() {
    if !is_unix() {
        eprintln!("SKIP silent_gpg_reports_no_gpg_output: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("silent");
    f.config("gpg.program", &f.program("gpg-silent", "cat > /dev/null\n"));
    f.config("user.signingKey", "KEYID");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: gpg failed to sign the data:\n(no gpg output)\nfatal: failed to write commit object\n",
    );
}

// ---------------------------------------------------------------------------
// the gpg_format[] program table
// ---------------------------------------------------------------------------

/// `gpg.openpgp.program` names the openpgp binary just as `gpg.program` does
/// (gpg-interface.c:788). It was not read at all, so setting only it left the
/// signer running whatever `gpg` the PATH offered.
#[test]
fn gpg_openpgp_program_selects_the_openpgp_binary() {
    if !is_unix() {
        eprintln!("SKIP gpg_openpgp_program_selects_the_openpgp_binary: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("openpgpprog");
    f.config("gpg.openpgp.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "KEYID");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);
    f.ok(&["commit", "-S", "-m", "signed"]);

    assert_eq!(f.recorded_argv().len(), 3, "the stand-in gpg must have run");
    assert!(f.head_object().contains("gpgsig -----BEGIN PGP SIGNATURE-----"));
}

/// `gpg.program` and `gpg.openpgp.program` aim at the same table slot, so the one
/// the config reader reaches *last* wins — a positional rule, not a specificity
/// one. Here `gpg.program` is written second and must beat the `openpgp` key that
/// a naive "more specific first" lookup would prefer.
#[test]
fn later_program_key_wins_over_earlier_one() {
    if !is_unix() {
        eprintln!("SKIP later_program_key_wins_over_earlier_one: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("progorder");
    f.config("gpg.openpgp.program", "/nonexistent/never-runs");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "KEYID");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);
    f.ok(&["commit", "-S", "-m", "signed"]);

    assert_eq!(f.recorded_argv().len(), 3, "the last-written program must be the one run");
}

/// `gpg.program` does **not** reach the x509 entry: only `gpg.x509.program` does
/// (gpg-interface.c:791). Applying `gpg.program` to every format silently swapped
/// the x509 signer for the openpgp one, which produces a PGP signature where a
/// CMS one was asked for.
#[test]
fn gpg_program_does_not_reach_the_x509_format() {
    if !is_unix() {
        eprintln!("SKIP gpg_program_does_not_reach_the_x509_format: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("x509prog");
    f.config("gpg.format", "x509");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "KEYID");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert!(
        f.recorded_argv().is_empty(),
        "gpg.program must not be used for gpg.format=x509; it ran with {:?}",
        f.recorded_argv()
    );
    // Whatever `gpgsm` does here (absent, or present with no key) the commit must
    // not carry a PGP signature produced by the openpgp program.
    if out.status.success() {
        assert!(!f.head_object().contains("BEGIN PGP SIGNATURE"));
    }
}

/// `gpg.x509.program` is the key that *does* reach the x509 entry.
#[test]
fn gpg_x509_program_selects_the_x509_binary() {
    if !is_unix() {
        eprintln!("SKIP gpg_x509_program_selects_the_x509_binary: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("x509prog2");
    f.config("gpg.format", "x509");
    f.config("gpg.x509.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "KEYID");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);
    f.ok(&["commit", "-S", "-m", "signed"]);

    assert_eq!(f.recorded_argv(), vec!["--status-fd=2", "-bsau", "KEYID"]);
}

// ---------------------------------------------------------------------------
// gpg.ssh.defaultKeyCommand
// ---------------------------------------------------------------------------

/// With `gpg.format = ssh` and neither `user.signingKey` nor
/// `gpg.ssh.defaultKeyCommand`, `get_default_ssh_signing_key()` dies naming both
/// settings (gpg-interface.c:884-885).
///
/// Stock git 2.55.0, measured: exit 128 and exactly
/// `fatal: either user.signingkey or gpg.ssh.defaultKeyCommand needs to be configured`
/// — one line, with no `failed to write commit object` after it, because this is a
/// `die()` rather than the `error()` the commit machinery wraps.
///
/// The message previously printed here was `user.signingKey needs to be set for
/// ssh signing`, which git reserves for the case *after* a key command has run and
/// come back empty. It also arrived as an `error()` with a second `fatal:` line,
/// so both the sentence and the shape were wrong.
#[test]
fn ssh_without_key_or_key_command_names_both_settings() {
    let f = Fixture::new("sshnokey");
    f.config("gpg.format", "ssh");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(out.status.code(), Some(128), "exit code");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: either user.signingkey or gpg.ssh.defaultKeyCommand needs to be configured\n",
    );
}

/// A key command that succeeds but prints something that is not a key warns with
/// both of its streams and then falls through to the empty-key error.
///
/// Stock git 2.55.0, measured, for a command printing `not-a-key` on stdout:
///
/// ```text
/// warning: gpg.ssh.defaultKeyCommand succeeded but returned no keys:  not-a-key
///
/// error: user.signingKey needs to be set for ssh signing
/// fatal: failed to write commit object
/// ```
///
/// The doubled space is git's `"%s %s"` of an empty stderr and the stdout that
/// still carries its newline; the blank line after it is that newline meeting the
/// one `warning()` appends.
#[test]
fn key_command_returning_no_key_warns_then_reports_the_empty_key() {
    if !is_unix() {
        eprintln!("SKIP key_command_returning_no_key_warns_then_reports_the_empty_key: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("dkcjunk");
    f.config("gpg.format", "ssh");
    f.config("gpg.ssh.defaultKeyCommand", &f.program("dkc-junk", "echo not-a-key\n"));
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: gpg.ssh.defaultKeyCommand succeeded but returned no keys:  not-a-key\n\n\
         error: user.signingKey needs to be set for ssh signing\n\
         fatal: failed to write commit object\n",
    );
}

/// A key command that fails warns with `failed:` and both streams — stderr first,
/// then stdout.
///
/// Stock git 2.55.0, measured, for a command printing `stdout-noise` /
/// `stderr-noise` and exiting 3:
///
/// ```text
/// warning: gpg.ssh.defaultKeyCommand failed: stderr-noise
///  stdout-noise
///
/// error: user.signingKey needs to be set for ssh signing
/// fatal: failed to write commit object
/// ```
#[test]
fn failing_key_command_warns_with_both_streams() {
    if !is_unix() {
        eprintln!("SKIP failing_key_command_warns_with_both_streams: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("dkcfail");
    f.config("gpg.format", "ssh");
    f.config(
        "gpg.ssh.defaultKeyCommand",
        &f.program("dkc-fail", "echo stdout-noise\necho stderr-noise >&2\nexit 3\n"),
    );
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: gpg.ssh.defaultKeyCommand failed: stderr-noise\n stdout-noise\n\n\
         error: user.signingKey needs to be set for ssh signing\n\
         fatal: failed to write commit object\n",
    );
}

/// A key command whose first line *is* a key is used, and only its first line is:
/// `strchr(begin, '\n')` truncates there (gpg-interface.c:900-905).
///
/// The key it prints is a *literal* one, which `sign_buffer_ssh` writes to a temp
/// file and passes with `-U` — the flag that tells `ssh-keygen` the file holds a
/// public key rather than a private one. Asserting on that argument vector is what
/// makes this test discriminating: a port that ignored the key command reaches the
/// empty-key error and never runs the signer at all.
///
/// Stock git 2.55.0, measured, with `gpg.ssh.program` recording its argv:
/// `-Y sign -n git -f <tmpkey> -U <tmpbuffer>`.
#[test]
fn key_command_first_line_is_the_key() {
    if !is_unix() {
        eprintln!("SKIP key_command_first_line_is_the_key: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("dkcgood");
    f.config("gpg.format", "ssh");
    f.config("gpg.ssh.program", &f.program("ssh-rec", RECORD_AND_FAIL));
    f.config(
        "gpg.ssh.defaultKeyCommand",
        &f.program(
            "dkc-good",
            "echo 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexampleexampleexampleexample tester'\n\
             echo 'trailing garbage that must be ignored'\n",
        ),
    );
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("gpg.ssh.defaultKeyCommand"),
        "a first line that is a key must not warn; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("user.signingKey needs to be set"),
        "the key command supplied a key; got:\n{stderr}"
    );

    let argv = f.recorded_argv();
    assert_eq!(argv.len(), 8, "signer argv was {argv:?}");
    assert_eq!(&argv[..5], ["-Y", "sign", "-n", "git", "-f"], "signer argv was {argv:?}");
    assert_eq!(argv[6], "-U", "a literal key must be passed with -U: {argv:?}");
    // The key file the signer was handed — captured by the stand-in, since
    // `sign_buffer_ssh` deletes it on the way out — holds exactly the first line
    // of the command's output, with the trailing garbage dropped.
    let mut key_copy = f.argv_log.clone().into_os_string();
    key_copy.push(".key");
    assert_eq!(
        std::fs::read_to_string(PathBuf::from(key_copy)).unwrap_or_default(),
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexampleexampleexampleexample tester",
        "only the first line of the key command's output is the key"
    );
}

// ---------------------------------------------------------------------------
// revert / cherry-pick
// ---------------------------------------------------------------------------

/// `git revert -S` signs. It used to `bail!("GPG signing is not supported")` at
/// exit 1 with a sentence git never prints.
#[test]
fn revert_dash_s_signs() {
    if !is_unix() {
        eprintln!("SKIP revert_dash_s_signs: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("revertsign");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "CFGKEY");

    let out = f.run(&["revert", "--no-edit", "-SPICKKEY", "HEAD"]);
    assert!(
        out.status.success(),
        "revert -S failed ({:?}):\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.head_object().contains("gpgsig -----BEGIN PGP SIGNATURE-----"),
        "revert commit carries no gpgsig header:\n{}",
        f.head_object()
    );
    assert_eq!(
        f.recorded_argv().last().map(String::as_str),
        Some("PICKKEY"),
        "-S<key> must reach gpg, not just enable signing"
    );
}

/// `git cherry-pick -S` signs, and `-S<key>` attached inside a short cluster
/// still supplies the key.
#[test]
fn cherry_pick_dash_s_signs_with_the_attached_key() {
    if !is_unix() {
        eprintln!("SKIP cherry_pick_dash_s_signs_with_the_attached_key: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("cpsign");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "CFGKEY");
    f.ok(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    f.write("z.txt", b"zed\n");
    f.ok(&["add", "z.txt"]);
    f.ok(&["commit", "-q", "-m", "zed"]);
    let pick = f.head_id();
    f.ok(&["checkout", "-q", "main"]);

    let out = f.run(&["cherry-pick", "-SPICKKEY", &pick]);
    assert!(
        out.status.success(),
        "cherry-pick -S failed ({:?}):\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(f.head_object().contains("gpgsig -----BEGIN PGP SIGNATURE-----"));
    assert_eq!(f.recorded_argv().last().map(String::as_str), Some("PICKKEY"));
}

/// `commit.gpgSign` is a *default* for the sequencer's `opts->gpg_sign`
/// (sequencer.c:302-306), so a plain `git revert` in a repository that asks for
/// signed commits produces one.
///
/// This is the half of the gap that had no diagnostic at all: the refusal above at
/// least said something, while this wrote an unsigned commit and exited 0.
#[test]
fn commit_gpgsign_config_signs_revert_and_cherry_pick() {
    if !is_unix() {
        eprintln!("SKIP commit_gpgsign_config_signs_revert_and_cherry_pick: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("cfgsign");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "CFGKEY");
    f.config("commit.gpgSign", "true");

    f.ok(&["revert", "--no-edit", "HEAD"]);
    assert!(
        f.head_object().contains("gpgsig -----BEGIN PGP SIGNATURE-----"),
        "commit.gpgSign must sign a plain revert:\n{}",
        f.head_object()
    );

    f.ok(&["checkout", "-q", "-b", "side", "HEAD~2"]);
    f.write("z.txt", b"zed\n");
    f.ok(&["add", "z.txt"]);
    f.ok(&["commit", "-q", "-m", "zed"]);
    let pick = f.head_id();
    f.ok(&["checkout", "-q", "main"]);
    f.ok(&["cherry-pick", &pick]);
    assert!(
        f.head_object().contains("gpgsig -----BEGIN PGP SIGNATURE-----"),
        "commit.gpgSign must sign a plain cherry-pick:\n{}",
        f.head_object()
    );
}

/// `--no-gpg-sign` beats `commit.gpgSign`, because the config is read before
/// `parse_args()` runs.
#[test]
fn no_gpg_sign_overrides_the_config_default() {
    if !is_unix() {
        eprintln!("SKIP no_gpg_sign_overrides_the_config_default: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("nosign");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "CFGKEY");
    f.config("commit.gpgSign", "true");

    f.ok(&["revert", "--no-edit", "--no-gpg-sign", "HEAD"]);
    assert!(
        !f.head_object().contains("gpgsig"),
        "--no-gpg-sign must leave the revert unsigned:\n{}",
        f.head_object()
    );
    assert!(f.recorded_argv().is_empty(), "no signing program should have run");
}

// ---------------------------------------------------------------------------
// gpg.minTrustLevel is read lazily
// ---------------------------------------------------------------------------

/// `gpg_interface_lazy_init()` runs on the first `check_signature()`, not while
/// config is loaded, so a value nothing ever reads is a value nothing ever
/// complains about.
///
/// Stock git 2.55.0, measured with `gpg.minTrustLevel = bogus`:
///
/// | command                                  | exit | stderr                        |
/// |------------------------------------------|------|-------------------------------|
/// | `verify-commit <unsigned>`               | 1    | (empty)                       |
/// | `verify-tag <unsigned annotated tag>`    | 1    | `error: no signature found`   |
/// | `verify-tag <absent>`                    | 1    | `error: tag 'nope' not found.`|
///
/// Reading the key up front turned all three into `error: invalid value for
/// 'gpg.mintrustlevel': 'bogus'` at 128 — a config error reported for a command
/// that, in git, never touches the setting.
#[test]
fn bad_min_trust_level_is_not_reported_until_a_signature_is_checked() {
    let f = Fixture::new("mintrust");
    f.ok(&["tag", "-a", "-m", "msg", "vplain"]);
    f.config("gpg.minTrustLevel", "bogus");

    let commit = f.run(&["verify-commit", "HEAD"]);
    assert_eq!(commit.status.code(), Some(1), "unsigned commit exit code");
    assert_eq!(String::from_utf8_lossy(&commit.stderr), "", "unsigned commit stderr");

    let tag = f.run(&["verify-tag", "vplain"]);
    assert_eq!(tag.status.code(), Some(1), "unsigned tag exit code");
    assert_eq!(String::from_utf8_lossy(&tag.stderr), "error: no signature found\n");

    let absent = f.run(&["verify-tag", "nope"]);
    assert_eq!(absent.status.code(), Some(1), "absent tag exit code");
    assert_eq!(String::from_utf8_lossy(&absent.stderr), "error: tag 'nope' not found.\n");
}

/// A `gpg.format` naming no entry in `gpg_format[]` stops the first sign or
/// verify, and nothing else.
///
/// Stock git 2.55.0, measured with `gpg.format = bogus`:
///
/// | command                      | exit | first stderr line                            |
/// |------------------------------|------|----------------------------------------------|
/// | `status`                     | 0    | (none)                                       |
/// | `commit -S`                  | 128  | `error: invalid value for 'gpg.format': 'bogus'` |
/// | `verify-commit <signed>`     | 128  | same                                         |
/// | `verify-commit <unsigned>`   | 1    | (none)                                       |
///
/// Falling back to `openpgp` instead — as this used to — signed with a backend
/// the user never asked for and exited 0. Stock's second line, `fatal: bad config
/// variable 'gpg.format' in file '<path>' at line <n>`, is a known gap: gix's
/// config parser carries no per-entry line numbers, so only the first line and the
/// exit code are asserted here.
#[test]
fn invalid_gpg_format_stops_signing_and_leaves_everything_else_alone() {
    if !is_unix() {
        eprintln!("SKIP invalid_gpg_format_stops_signing_and_leaves_everything_else_alone: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("badformat");
    f.config("gpg.program", &f.program("gpg-ok", GPG_OK));
    f.config("user.signingKey", "KEYID");
    f.config("gpg.format", "bogus");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let signed = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(signed.status.code(), Some(128), "commit -S exit code");
    assert!(
        String::from_utf8_lossy(&signed.stderr)
            .starts_with("error: invalid value for 'gpg.format': 'bogus'\n"),
        "stderr was:\n{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    assert!(
        f.recorded_argv().is_empty(),
        "no backend may run for a format that names no entry: {:?}",
        f.recorded_argv()
    );

    // `gpg_interface_lazy_init()` never runs for a command that signs nothing.
    let status = f.run(&["status"]);
    assert_eq!(status.status.code(), Some(0), "status exit code");
    assert_eq!(String::from_utf8_lossy(&status.stderr), "", "status stderr");

    // Nor for a verification that finds no signature to check.
    let unsigned = f.run(&["verify-commit", "HEAD"]);
    assert_eq!(unsigned.status.code(), Some(1), "unsigned verify exit code");
    assert_eq!(String::from_utf8_lossy(&unsigned.stderr), "", "unsigned verify stderr");
}

// ---------------------------------------------------------------------------
// ssh, end to end
// ---------------------------------------------------------------------------

/// A real ssh signature that a real verifier accepts, through the whole chain:
/// `commit -S` writes it, `verify-commit` checks it against
/// `gpg.ssh.allowedSignersFile`, and `%G?` grades it `G`.
///
/// Checking only that a signature *block* appeared would pass on a signature over
/// the wrong payload, which is the mistake that matters here: git signs the commit
/// object serialized *without* the `gpgsig` header and inserts it afterwards.
#[test]
fn ssh_signature_verifies_end_to_end() {
    if !is_unix() || !ssh_signing_available() {
        eprintln!("SKIP ssh_signature_verifies_end_to_end: ssh-keygen -Y sign unavailable");
        return;
    }
    let f = Fixture::new("sshe2e");
    let Some(key) = keygen(&f.root) else {
        eprintln!("SKIP ssh_signature_verifies_end_to_end: ssh-keygen could not make a key");
        return;
    };
    let pubkey = std::fs::read_to_string(f.root.join("id_ed25519.pub")).unwrap();
    let allowed = f.root.join("allowed_signers");
    std::fs::write(&allowed, format!("committer@example.com {}", pubkey.trim())).unwrap();

    f.config("gpg.format", "ssh");
    f.config("user.signingKey", key.to_str().unwrap());
    f.config("gpg.ssh.allowedSignersFile", allowed.to_str().unwrap());
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);
    f.ok(&["commit", "-S", "-m", "signed"]);

    let raw = f.head_object();
    assert!(
        raw.contains("gpgsig -----BEGIN SSH SIGNATURE-----"),
        "expected an SSH SIGNATURE header:\n{raw}"
    );
    let verify = f.run(&["verify-commit", "HEAD"]);
    assert!(
        verify.status.success(),
        "verify-commit rejected our own signature:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        String::from_utf8_lossy(&verify.stderr)
            .starts_with("Good \"git\" signature for committer@example.com"),
        "verify-commit said:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert_eq!(f.ok(&["log", "-1", "--format=%G?"]).trim(), "G");
}

/// `git revert -S` under `gpg.format = ssh` produces a signature that verifies —
/// the sequencer path and the ssh backend together, since the sequencer's commit
/// is written in process rather than through `git commit`.
#[test]
fn revert_dash_s_ssh_signature_verifies() {
    if !is_unix() || !ssh_signing_available() {
        eprintln!("SKIP revert_dash_s_ssh_signature_verifies: ssh-keygen -Y sign unavailable");
        return;
    }
    let f = Fixture::new("sshrevert");
    let Some(key) = keygen(&f.root) else {
        eprintln!("SKIP revert_dash_s_ssh_signature_verifies: ssh-keygen could not make a key");
        return;
    };
    let pubkey = std::fs::read_to_string(f.root.join("id_ed25519.pub")).unwrap();
    let allowed = f.root.join("allowed_signers");
    std::fs::write(&allowed, format!("committer@example.com {}", pubkey.trim())).unwrap();

    f.config("gpg.format", "ssh");
    f.config("user.signingKey", key.to_str().unwrap());
    f.config("gpg.ssh.allowedSignersFile", allowed.to_str().unwrap());

    let out = f.run(&["revert", "--no-edit", "-S", "HEAD"]);
    assert!(
        out.status.success(),
        "revert -S failed ({:?}):\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.ok(&["log", "-1", "--format=%G?"]).trim(), "G");
    let verify = f.run(&["verify-commit", "HEAD"]);
    assert!(
        verify.status.success(),
        "verify-commit rejected the revert's own signature:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        String::from_utf8_lossy(&verify.stderr)
            .starts_with("Good \"git\" signature for committer@example.com"),
        "verify-commit said:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

/// `vfreportf()` (usage.c:30-33) turns every control character except tab and
/// newline into `?` on its way to stderr. `ssh-keygen` ends its lines with CRLF,
/// so git's rendering of a signing failure carries a literal `?` before the
/// newline; printing the child's bytes through drops it.
///
/// Stock git 2.55.0, measured, for `user.signingKey` pointing at a missing file:
/// `error: Couldn't load public key <path>: No such file or directory?` followed by
/// a blank line.
#[test]
fn ssh_stderr_control_characters_are_rendered_as_question_marks() {
    if !is_unix() {
        eprintln!("SKIP ssh_stderr_control_characters_are_rendered_as_question_marks: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("crlf");
    f.config("gpg.format", "ssh");
    // A stand-in signer that fails the way ssh-keygen does: a CRLF-terminated
    // complaint on stderr and no `.sig` file left behind.
    f.config(
        "gpg.ssh.program",
        &f.program("ssh-crlf", "printf 'Couldn'\\''t load public key\\r\\n' >&2\nexit 255\n"),
    );
    f.config("user.signingKey", "/nonexistent/key");
    f.write("a.txt", b"one\ntwo\n");
    f.ok(&["add", "a.txt"]);

    let out = f.run(&["commit", "-S", "-m", "signed"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: Couldn't load public key?\n\nfatal: failed to write commit object\n",
    );
}
