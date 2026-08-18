//! `git tag -s` / `-u` / `tag.gpgSign` / `tag.forceSignAnnotated`, and the
//! `git tag -v` half of `builtin/tag.c` — the whole signing surface, which was
//! refused outright before (`zvcs: tag: signed tags (-s) are not supported`).
//!
//! Every expectation below was measured against stock git 2.55.0 first and is
//! quoted in the test that pins it. The three that would be most damaging to get
//! wrong, and which the refusal was hiding:
//!
//! * **A tag's signature is body text, not a header.** `do_sign()` ends with a bare
//!   `strbuf_addbuf(buffer, &sig)` onto the buffer the message already terminated
//!   (builtin/tag.c:191), so the armor follows the message with *no* separating
//!   newline. gix's `Tag::pgp_signature` field writes one, which would be a
//!   different object id and a payload stock git cannot verify — so the object is
//!   assembled by hand. Signing the same commit with the same fixed signature bytes
//!   produces the byte-identical object, and the same oid, under both binaries.
//! * **A signing failure must write nothing at all.** Success is
//!   `[GNUPG:] SIG_CREATED ` at the start of a line, never the exit status
//!   (gpg-interface.c:1035-1042); a gpg that exits 0 and signs nothing must leave no
//!   ref, no tag object, and exit 128. This codebase has already shipped the
//!   opposite shape once, for commits.
//! * **`tag.forceSignAnnotated` has two counter-intuitive halves**, both stock's:
//!   it signs the tag `-m` alone implied, pointedly leaves the one `-a` asked for
//!   unsigned, and overrides an explicit `--no-sign` — because it is applied after
//!   the command line has already been folded in (builtin/tag.c:684).
//!
//! The openpgp cases drive a `/bin/sh` stand-in through `gpg.program` rather than a
//! real gpg, so they run headless with no keyring and no agent. The two that need
//! real crypto probe for `ssh-keygen -Y` and skip loudly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repository plus the isolated environment every run needs. `home` keeps the
/// developer's `~/.gitconfig` out — this machine's sets `core.commentChar`, which
/// changes message cleanup.
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("zvcs-tagsign-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo, home };
        f.ok(&["init", "-q", "-b", "main"]);
        f.ok(&["config", "user.name", "C O Mitter"]);
        f.ok(&["config", "user.email", "committer@example.com"]);
        std::fs::write(f.repo.join("a.txt"), b"one\n").unwrap();
        f.ok(&["add", "a.txt"]);
        f.ok(&["commit", "-q", "-m", "one"]);
        f
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

    fn config(&self, key: &str, value: &str) {
        self.ok(&["config", key, value]);
    }

    fn code(&self, args: &[&str]) -> i32 {
        self.run(args).status.code().unwrap_or(-1)
    }

    fn stderr(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.run(args).stderr).into_owned()
    }

    /// The raw tag object bytes, which is where signature placement is visible.
    fn tag_object(&self, name: &str) -> Vec<u8> {
        self.run(&["cat-file", "tag", name]).stdout
    }

    fn tag_exists(&self, name: &str) -> bool {
        self.run(&["rev-parse", "-q", "--verify", &format!("refs/tags/{name}")])
            .status
            .success()
    }

    /// Whether the tag object carries a signature block at all.
    fn is_signed(&self, name: &str) -> bool {
        let raw = self.tag_object(name);
        raw.windows(30)
            .any(|w| w == b"-----BEGIN PGP SIGNATURE-----\n" || w == b"-----BEGIN SSH SIGNATURE-----\n")
    }

    /// Install an executable `/bin/sh` script and return its path, for `gpg.program`.
    fn program(&self, name: &str, body: &str) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        make_executable(&path);
        path.to_str().unwrap().to_owned()
    }

    /// A repository configured with a stand-in gpg that always succeeds.
    fn with_working_gpg(&self) {
        self.config("gpg.program", &self.program("gpg-ok", GPG_OK));
        self.config("user.signingKey", "DEADBEEF");
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

fn is_unix() -> bool {
    cfg!(unix)
}

/// A stand-in gpg that behaves: drains the payload, prints `SIG_CREATED` on fd 2
/// and a *fixed* armored block on stdout. Fixed so the resulting tag object is
/// byte-comparable across runs and across binaries.
const GPG_OK: &str = r#"cat > /dev/null
echo "[GNUPG:] SIG_CREATED D 1 8 00 0 0" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nZmFrZQ==\n-----END PGP SIGNATURE-----\n'
"#;

/// Exit 0, an armored block on stdout, and **no** `SIG_CREATED`. gpg reaches this
/// shape on real error paths; reading only the exit status accepts it.
const GPG_NO_STATUS: &str = r#"cat > /dev/null
echo "gpg: some chatter" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nZmFrZQ==\n-----END PGP SIGNATURE-----\n'
"#;

/// Whether `ssh-keygen -Y sign` exists here (openssh 8.2p1+).
fn ssh_signing_available() -> bool {
    let Ok(out) = Command::new("ssh-keygen").arg("-Y").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stderr);
    text.contains("sign") || text.contains("Usage")
}

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
// where the signature goes
// ---------------------------------------------------------------------------

/// The signature is appended to the tag **body**, with no separating newline.
///
/// ```c
/// if (compat_sig.len)  add_header_signature(buffer, &compat_sig, compat);
/// strbuf_addbuf(buffer, &sig);
/// ```
/// (builtin/tag.c:188-191). The message has already been terminated by
/// `strbuf_stripspace`, so the armor starts on the very next byte.
///
/// Stock git 2.55.0, measured on this exact fixture shape with this exact
/// stand-in signature: the object ends `\nsigned msg\n-----BEGIN PGP SIGNATURE-----`.
/// gix's `Tag::pgp_signature` writes `\n\n-----BEGIN` instead — one byte that
/// changes the object id and breaks verification everywhere.
#[test]
fn signature_follows_the_message_with_no_blank_line() {
    if !is_unix() {
        eprintln!("SKIP signature_follows_the_message_with_no_blank_line: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("place");
    f.with_working_gpg();
    assert_eq!(f.code(&["tag", "-s", "-m", "signed msg", "sgn"]), 0);

    let raw = f.tag_object("sgn");
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.contains("\nsigned msg\n-----BEGIN PGP SIGNATURE-----\n"),
        "signature must abut the message; got:\n{text}"
    );
    assert!(
        !text.contains("\nsigned msg\n\n-----BEGIN"),
        "a blank line crept in between message and signature:\n{text}"
    );
    // And the payload git would recover is everything before the armor, which must
    // still be a well-formed tag ending in exactly one newline.
    let split = text.find("-----BEGIN PGP SIGNATURE-----").unwrap();
    let payload = &text[..split];
    assert!(payload.ends_with("signed msg\n"), "payload was {payload:?}");
    assert!(payload.starts_with("object "), "payload was {payload:?}");
}

/// The whole object, byte for byte, against what stock wrote for the same input.
///
/// Stock git 2.55.0 with this stand-in gpg produced exactly these bytes; the tag
/// object id was identical between the two binaries. Asserting the full text
/// catches a stray header, a reordered field, or a lost `tagger` line that the
/// placement test above would let through.
#[test]
fn signed_tag_object_matches_stock_byte_for_byte() {
    if !is_unix() {
        eprintln!("SKIP signed_tag_object_matches_stock_byte_for_byte: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("bytes");
    f.with_working_gpg();
    f.ok(&["tag", "-s", "-m", "signed msg", "sgn"]);

    let commit = f.ok(&["rev-parse", "HEAD"]).trim().to_owned();
    let expected = format!(
        "object {commit}\n\
         type commit\n\
         tag sgn\n\
         tagger C O Mitter <committer@example.com> 1112911993 -0700\n\
         \n\
         signed msg\n\
         -----BEGIN PGP SIGNATURE-----\n\
         \n\
         ZmFrZQ==\n\
         -----END PGP SIGNATURE-----\n"
    );
    assert_eq!(String::from_utf8_lossy(&f.tag_object("sgn")), expected);
}

// ---------------------------------------------------------------------------
// a signing failure writes nothing
// ---------------------------------------------------------------------------

/// A gpg that exits 0 without `SIG_CREATED` is a failure, and the tag must not
/// exist afterwards.
///
/// Stock git 2.55.0, measured with a real gpg and an unusable key:
///
/// ```text
/// error: gpg failed to sign the data:
/// …gpg's own status stream…
/// error: unable to sign the tag
/// The tag message has been left in .git/TAG_EDITMSG
/// ```
/// exit 128, and `refs/tags/<name>` absent.
#[test]
fn gpg_exit_zero_without_sig_created_writes_no_tag() {
    if !is_unix() {
        eprintln!("SKIP gpg_exit_zero_without_sig_created_writes_no_tag: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("nosig");
    f.config("gpg.program", &f.program("gpg-quiet", GPG_NO_STATUS));
    f.config("user.signingKey", "DEADBEEF");

    let out = f.run(&["tag", "-s", "-m", "m", "boom"]);
    assert_eq!(out.status.code(), Some(128));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error: gpg failed to sign the data:"), "{err}");
    assert!(err.contains("error: unable to sign the tag"), "{err}");
    assert!(
        err.contains("The tag message has been left in .git/TAG_EDITMSG"),
        "{err}"
    );
    assert!(!f.tag_exists("boom"), "a tag was written despite the failure");
}

/// The ref is not the only thing that must stay absent: no tag object may be
/// written either, signed or unsigned. An unsigned object left in the odb under a
/// `-s` that failed is a tag that later claims to be signed and is not.
#[test]
fn failed_signing_leaves_no_tag_object_behind() {
    if !is_unix() {
        eprintln!("SKIP failed_signing_leaves_no_tag_object_behind: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("noobj");
    f.config("gpg.program", &f.program("gpg-quiet", GPG_NO_STATUS));
    f.config("user.signingKey", "DEADBEEF");
    assert_eq!(f.code(&["tag", "-s", "-m", "m", "boom"]), 128);

    // `cat-file --batch-all-objects` over the odb: no object of type tag at all.
    let listed = f.ok(&["cat-file", "--batch-all-objects", "--batch-check=%(objecttype)"]);
    assert!(
        !listed.lines().any(|l| l.trim() == "tag"),
        "a tag object survived a failed signing:\n{listed}"
    );
}

// ---------------------------------------------------------------------------
// tag.gpgSign / tag.forceSignAnnotated precedence
// ---------------------------------------------------------------------------

/// `tag.gpgSign` signs, and implies an annotated tag object; `--no-sign` beats it.
///
/// ```c
/// if (opt.sign == -1)  opt.sign = cmdmode ? 0 : config_sign_tag > 0;
/// ```
/// (builtin/tag.c:574). `opt.sign` starts at -1, so a written 0 from `--no-sign` is
/// what stops the config — which is why the flag cannot be modelled as a plain
/// `bool` defaulting to false.
#[test]
fn tag_gpgsign_signs_and_no_sign_overrides_it() {
    if !is_unix() {
        eprintln!("SKIP tag_gpgsign_signs_and_no_sign_overrides_it: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("cfgsign");
    f.with_working_gpg();
    f.config("tag.gpgSign", "true");

    f.ok(&["tag", "-m", "m", "on"]);
    assert!(f.is_signed("on"), "tag.gpgSign did not sign");

    f.ok(&["tag", "--no-sign", "-m", "m", "off"]);
    assert!(!f.is_signed("off"), "--no-sign did not beat tag.gpgSign");

    // `cmdmode ? 0 : …` — listing in a repository that sets it still just lists.
    assert_eq!(f.code(&["tag", "-l"]), 0);
}

/// `tag.forceSignAnnotated` signs the tag `-m` implied and leaves the one `-a`
/// asked for unsigned.
///
/// ```c
/// if (create_tag_object) {
///         if (force_sign_annotate && !annotate)  opt.sign = 1;
/// ```
/// (builtin/tag.c:683-684) — `annotate` there is the bare `-a` flag, not
/// `create_tag_object`. Stock git 2.55.0, measured: `-m m` signs, `-a -m m` does not.
#[test]
fn force_sign_annotated_skips_an_explicit_dash_a() {
    if !is_unix() {
        eprintln!("SKIP force_sign_annotated_skips_an_explicit_dash_a: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("forcesign");
    f.with_working_gpg();
    f.config("tag.forceSignAnnotated", "true");

    f.ok(&["tag", "-m", "m", "implied"]);
    assert!(f.is_signed("implied"), "-m alone should have been signed");

    f.ok(&["tag", "-a", "-m", "m", "explicit"]);
    assert!(
        !f.is_signed("explicit"),
        "-a with tag.forceSignAnnotated must stay unsigned"
    );

    // And with nothing to annotate it stays a lightweight ref: `create_tag_object`
    // is false, so the whole block is skipped.
    f.ok(&["tag", "light"]);
    assert_eq!(f.ok(&["cat-file", "-t", "light"]).trim(), "commit");
}

/// `tag.forceSignAnnotated` overrides an explicit `--no-sign`, because it is
/// applied after the command line has been folded in (builtin/tag.c:684).
///
/// Stock git 2.55.0, measured: `git -c tag.forceSignAnnotated=true tag --no-sign
/// -m m t` produces a **signed** tag. Counter-intuitive, and stock's.
#[test]
fn force_sign_annotated_beats_no_sign() {
    if !is_unix() {
        eprintln!("SKIP force_sign_annotated_beats_no_sign: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("forceover");
    f.with_working_gpg();
    f.config("tag.forceSignAnnotated", "true");
    f.ok(&["tag", "--no-sign", "-m", "m", "t"]);
    assert!(
        f.is_signed("t"),
        "tag.forceSignAnnotated must override --no-sign"
    );
}

/// `-u <key>` turns signing on unconditionally, so it wins over `--no-sign` from
/// either side; `-s` and `--no-sign` are an ordinary last-one-wins `OPT_BOOL`.
///
/// ```c
/// if (keyid) { opt.sign = 1; set_signing_key(keyid); }
/// ```
/// (builtin/tag.c:577-580) runs *after* parsing, which is what makes `-u` order
/// -independent while `-s` is not. Stock git 2.55.0, measured, all four cases.
#[test]
fn dash_u_beats_no_sign_but_dash_s_does_not() {
    if !is_unix() {
        eprintln!("SKIP dash_u_beats_no_sign_but_dash_s_does_not: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("keyid");
    f.with_working_gpg();

    f.ok(&["tag", "-u", "KEYA", "--no-sign", "-m", "m", "ua"]);
    assert!(f.is_signed("ua"), "-u then --no-sign must still sign");
    f.ok(&["tag", "--no-sign", "-u", "KEYA", "-m", "m", "ub"]);
    assert!(f.is_signed("ub"), "--no-sign then -u must still sign");

    f.ok(&["tag", "-s", "--no-sign", "-m", "m", "sa"]);
    assert!(!f.is_signed("sa"), "-s then --no-sign must not sign");
    f.ok(&["tag", "--no-sign", "-s", "-m", "m", "sb"]);
    assert!(f.is_signed("sb"), "--no-sign then -s must sign");

    // `--no-local-user` sets keyid back to NULL, so nothing forces signing on.
    f.ok(&["tag", "--no-local-user", "-m", "m", "nu"]);
    assert!(!f.is_signed("nu"), "--no-local-user must not sign");
}

/// `-s` implies a tag object even against `--no-annotate`, and the short cluster
/// spellings reach the same place as the separated ones.
///
/// `create_tag_object = (opt.sign || annotate || msg.given || …)` (builtin/tag.c:581)
/// — `opt.sign` is the first term, so annotation is never what carries it.
#[test]
fn sign_implies_a_tag_object_and_clusters_parse() {
    if !is_unix() {
        eprintln!("SKIP sign_implies_a_tag_object_and_clusters_parse: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("imply");
    f.with_working_gpg();

    f.ok(&["tag", "-s", "--no-annotate", "-m", "m", "na"]);
    assert!(f.is_signed("na"), "-s must survive --no-annotate");

    // `-sm <msg>` and `-u<key>` stuck to the flag, both of which used to be
    // `unsupported option`.
    f.ok(&["tag", "-sm", "clustered", "cl"]);
    assert!(f.is_signed("cl"), "-sm did not sign");
    f.ok(&["tag", "-uKEYB", "-m", "m", "inline"]);
    assert!(f.is_signed("inline"), "-u<key> did not sign");
    f.ok(&["tag", "--local-user=KEYB", "-m", "m", "eq"]);
    assert!(f.is_signed("eq"), "--local-user=<key> did not sign");
}

// ---------------------------------------------------------------------------
// mode conflicts and only_in_list
// ---------------------------------------------------------------------------

/// `(create_tag_object || force) && cmdmode` is a usage error (builtin/tag.c:584).
///
/// Stock git 2.55.0, measured: `-s -l`, `-a`, `-f`, `-e -l`, `-s -d` and `-s -v`
/// all exit 129 with the usage block. `-a` and `-f` alone matter most — with no
/// tagname argv is empty, so `cmdmode` becomes `'l'` and a creation flag is now in
/// list mode. This port used to *list* for both, silently discarding the flag.
///
/// `--create-reflog` is deliberately **not** in this list: it is absent from
/// `create_tag_object` (builtin/tag.c:581, which is `opt.sign || annotate ||
/// msg.given || msgfile || edit_flag || trailer_args.nr` and nothing else), so
/// `git tag --create-reflog -l` is an ordinary listing at exit 0. Measured against
/// stock rather than assumed — the first draft of this test asserted 129 and was
/// wrong.
#[test]
fn creation_flags_in_list_mode_are_a_usage_error() {
    let f = Fixture::new("modes");
    for args in [
        &["tag", "-s", "-l"][..],
        &["tag", "-a"][..],
        &["tag", "-f"][..],
        &["tag", "-e", "-l"][..],
        &["tag", "-s", "-d", "nosuch"][..],
        &["tag", "-s", "-v", "nosuch"][..],
    ] {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(129), "for {args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.starts_with("usage: git tag [-a | -s | -u <key-id>]"),
            "for {args:?} got:\n{err}"
        );
    }
    // The boundary of `create_tag_object`: these are listing options or neither,
    // so they are not a usage error however they are combined with `-l`.
    for args in [
        &["tag", "--create-reflog", "-l"][..],
        &["tag", "--create-reflog"][..],
        &["tag", "-i", "-l"][..],
        &["tag", "--omit-empty", "-l"][..],
    ] {
        assert_eq!(f.code(args), 0, "for {args:?}");
    }
}

/// Two different `OPT_CMDMODE`s cannot be combined, and the message names the one
/// just seen first (parse-options.c:417-420). Exit 129, and *no* usage block —
/// unlike an unknown option.
///
/// Stock git 2.55.0, measured: `-d -l` → `error: options '-l' and '-d' cannot be
/// used together`; `-l -d` → the same two names in the other order. Repeating the
/// same mode (`-l -l`) is accepted.
#[test]
fn conflicting_cmdmodes_are_refused_in_the_order_typed() {
    let f = Fixture::new("cmdmode");
    for (args, expected) in [
        (&["tag", "-d", "-l"][..], "error: options '-l' and '-d' cannot be used together\n"),
        (&["tag", "-l", "-d"][..], "error: options '-d' and '-l' cannot be used together\n"),
        (&["tag", "-dl"][..], "error: options '-l' and '-d' cannot be used together\n"),
        (
            &["tag", "--list", "--delete"][..],
            "error: options '--delete' and '--list' cannot be used together\n",
        ),
    ] {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(129), "for {args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), expected, "for {args:?}");
    }
    // The same mode twice is not a conflict.
    assert_eq!(f.code(&["tag", "-l", "-l"]), 0);
}

/// A listing-only option outside list mode is a `die()` naming it, in git's own
/// fixed order rather than the order typed (builtin/tag.c:610-623).
///
/// Stock git 2.55.0, measured: exit 128, `fatal: the '-n' option is only allowed in
/// list mode`. This port used to honor `-n` and *delete the tag anyway*.
#[test]
fn listing_only_options_outside_list_mode_die() {
    let f = Fixture::new("onlylist");
    f.ok(&["tag", "-m", "m", "AA"]);
    for (args, name) in [
        (&["tag", "-d", "-n1", "AA"][..], "-n"),
        (&["tag", "-v", "-n1", "AA"][..], "-n"),
        (&["tag", "-d", "--contains", "HEAD", "AA"][..], "--contains"),
        (&["tag", "-v", "--merged", "HEAD", "AA"][..], "--merged"),
        (&["tag", "-d", "--points-at", "HEAD", "AA"][..], "--points-at"),
    ] {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(128), "for {args:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: the '{name}' option is only allowed in list mode\n"),
            "for {args:?}"
        );
    }
    // and the tag survived every one of them
    assert!(f.tag_exists("AA"));
}

/// A value-taking option with nothing after it is `parse_options()`' own error, and
/// the wording differs between the short and long spellings.
///
/// ```c
/// return error(_("%s requires a value"), optnamearg(opt, NULL, flags));
/// ```
/// (parse-options.c:126-127). Stock git 2.55.0, measured: ``error: switch `u'
/// requires a value`` for `-u`, ``error: option `local-user' requires a value`` for
/// `--local-user`, exit 129 both, with no usage block. This port used to answer
/// ``zvcs: tag: option `local-user' requires a value`` at exit 1 for every one.
#[test]
fn missing_option_values_use_parse_options_wording() {
    let f = Fixture::new("noval");
    for (args, expected) in [
        (&["tag", "-u"][..], "error: switch `u' requires a value\n"),
        (&["tag", "-m"][..], "error: switch `m' requires a value\n"),
        (&["tag", "-F"][..], "error: switch `F' requires a value\n"),
        (&["tag", "--local-user"][..], "error: option `local-user' requires a value\n"),
        (&["tag", "--message"][..], "error: option `message' requires a value\n"),
        (&["tag", "--sort"][..], "error: option `sort' requires a value\n"),
    ] {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(129), "for {args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), expected, "for {args:?}");
    }
}

// ---------------------------------------------------------------------------
// git tag -v
// ---------------------------------------------------------------------------

/// `git tag -v` prints the tag payload on **stdout** and the checker's report on
/// stderr; bare `git verify-tag` prints no payload.
///
/// `verify_tag()` passes `GPG_VERIFY_VERBOSE` (builtin/tag.c:147) where
/// `cmd_verify_tag()` passes nothing unless `-v` is given — the one place the two
/// commands genuinely differ. Stock git 2.55.0, measured.
#[test]
fn tag_dash_v_prints_the_payload_and_verify_tag_does_not() {
    if !is_unix() {
        eprintln!("SKIP tag_dash_v_prints_the_payload_and_verify_tag_does_not: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("vshape");
    f.with_working_gpg();
    f.ok(&["tag", "-s", "-m", "good msg", "good"]);

    // The stand-in gpg cannot verify, so only the payload half is asserted here;
    // the end-to-end verdict is the ssh test below.
    let tv = f.run(&["tag", "-v", "good"]);
    let payload = String::from_utf8_lossy(&tv.stdout).into_owned();
    assert!(payload.starts_with("object "), "tag -v stdout:\n{payload}");
    assert!(payload.contains("\ntag good\n"), "tag -v stdout:\n{payload}");
    assert!(payload.ends_with("good msg\n"), "tag -v stdout:\n{payload}");
    assert!(
        !payload.contains("BEGIN PGP SIGNATURE"),
        "the signature is not part of the payload:\n{payload}"
    );

    let vt = f.run(&["verify-tag", "good"]);
    assert!(
        vt.stdout.is_empty(),
        "verify-tag must print no payload without -v: {:?}",
        String::from_utf8_lossy(&vt.stdout)
    );
    // `-v` on verify-tag opts back in.
    let vtv = f.run(&["verify-tag", "-v", "good"]);
    assert_eq!(String::from_utf8_lossy(&vtv.stdout), payload);
}

/// `git tag -v` resolves `refs/tags/<name>` and nothing else, while `git verify-tag`
/// goes through `repo_get_oid()`.
///
/// `for_each_tag_name()` builds `refs/tags/%s` and calls `refs_read_ref()`
/// (builtin/tag.c:93-100), so an object id or `HEAD` is "not found" for `tag -v`
/// even when it names a perfectly good signed tag — which `verify-tag` accepts.
/// Stock git 2.55.0, measured, both halves.
#[test]
fn tag_dash_v_takes_only_tag_names() {
    if !is_unix() {
        eprintln!("SKIP tag_dash_v_takes_only_tag_names: needs a POSIX shell");
        return;
    }
    let f = Fixture::new("vnames");
    f.with_working_gpg();
    f.ok(&["tag", "-s", "-m", "m", "good"]);
    let oid = f.ok(&["rev-parse", "refs/tags/good"]).trim().to_owned();

    let out = f.run(&["tag", "-v", &oid]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("error: tag '{oid}' not found.\n")
    );

    let head = f.run(&["tag", "-v", "HEAD"]);
    assert_eq!(head.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&head.stderr),
        "error: tag 'HEAD' not found.\n"
    );

    // verify-tag takes the same id happily (it fails only on the stand-in gpg's
    // inability to verify, not on the name).
    let vt = f.run(&["verify-tag", &oid]);
    assert!(
        !String::from_utf8_lossy(&vt.stderr).contains("not found"),
        "verify-tag rejected an object id: {:?}",
        String::from_utf8_lossy(&vt.stderr)
    );
}

/// An unsigned or lightweight tag fails verification with git's exact wording, and
/// `-v` still prints the payload of the unsigned one first.
///
/// `run_gpg_verify()` writes the whole buffer under `GPG_VERIFY_VERBOSE` *before*
/// `return error("no signature found")` (tag.c:28-32); a non-tag object never gets
/// that far. Stock git 2.55.0, measured: exit 1 for both.
#[test]
fn unsigned_and_lightweight_tags_fail_verification() {
    let f = Fixture::new("vunsigned");
    f.ok(&["tag", "-m", "plain", "plain"]);
    f.ok(&["tag", "light"]);

    let out = f.run(&["tag", "-v", "plain"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: no signature found\n");
    let payload = String::from_utf8_lossy(&out.stdout);
    assert!(payload.ends_with("plain\n"), "payload:\n{payload}");

    let lw = f.run(&["tag", "-v", "light"]);
    assert_eq!(lw.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&lw.stderr),
        "error: light: cannot verify a non-tag object of type commit.\n"
    );
    assert!(lw.stdout.is_empty());

    // A name with no ref at all, and the exit code after a mix of good and bad.
    let missing = f.run(&["tag", "-v", "nosuch"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&missing.stderr),
        "error: tag 'nosuch' not found.\n"
    );
}

// ---------------------------------------------------------------------------
// end-to-end: real crypto, both directions
// ---------------------------------------------------------------------------

/// Sign a tag with this binary and verify it with the same `ssh-keygen -Y verify`
/// stock would hand it to — proving the payload split is stock-compatible without
/// needing stock git installed.
///
/// The payload handed to the verifier is everything before the armor, which is only
/// correct if the signature was appended with no separating newline. A stray byte
/// here fails the signature check rather than merely changing the object id, which
/// is what makes this the real regression test for placement.
#[test]
fn ssh_signed_tag_verifies_end_to_end() {
    if !is_unix() || !ssh_signing_available() {
        eprintln!("SKIP ssh_signed_tag_verifies_end_to_end: ssh-keygen -Y sign unavailable");
        return;
    }
    let f = Fixture::new("sshe2e");
    let Some(key) = keygen(&f.root) else {
        eprintln!("SKIP ssh_signed_tag_verifies_end_to_end: ssh-keygen could not make a key");
        return;
    };
    let pubkey = std::fs::read_to_string(f.root.join("id_ed25519.pub")).unwrap();
    let allowed = f.root.join("allowed_signers");
    std::fs::write(&allowed, format!("tester {pubkey}")).unwrap();

    f.config("gpg.format", "ssh");
    f.config("user.signingKey", key.to_str().unwrap());
    f.config("gpg.ssh.allowedSignersFile", allowed.to_str().unwrap());

    assert_eq!(f.code(&["tag", "-s", "-m", "ssh msg", "sshtag"]), 0);
    assert!(f.is_signed("sshtag"), "no SSH SIGNATURE block was written");

    // This binary's own verdict.
    let out = f.run(&["tag", "-v", "sshtag"]);
    assert_eq!(out.status.code(), Some(0), "verify failed:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Good \"git\" signature"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And `ssh-keygen -Y verify` directly, over the split this port computed: the
    // payload on stdin, the armor in a file, exactly as gpg-interface.c does it.
    let raw = f.tag_object("sshtag");
    let marker = b"-----BEGIN SSH SIGNATURE-----";
    let split = raw
        .windows(marker.len())
        .position(|w| w == marker)
        .expect("no signature block");
    let sig_path = f.root.join("tag.sig");
    std::fs::write(&sig_path, &raw[split..]).unwrap();
    let payload_path = f.root.join("tag.payload");
    std::fs::write(&payload_path, &raw[..split]).unwrap();

    let verified = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "ssh-keygen -Y verify -f {} -I tester -n git -s {} < {}",
            allowed.display(),
            sig_path.display(),
            payload_path.display()
        ))
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "ssh-keygen rejected the payload/signature split:\n{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

/// `gpg.format = ssh` with no key at all dies inside `get_default_ssh_signing_key()`
/// and nothing is written.
///
/// That `die()` names both settings the user could have used and ends the command on
/// the spot, so — unlike a backend `error()` — no `unable to sign the tag` follows
/// it. Stock git 2.55.0, measured: exit 128, one line.
#[test]
fn ssh_without_a_key_dies_before_writing_anything() {
    let f = Fixture::new("sshnokey");
    f.config("gpg.format", "ssh");
    let out = f.run(&["tag", "-s", "-m", "m", "t"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: either user.signingkey or gpg.ssh.defaultKeyCommand needs to be configured\n"
    );
    assert!(!f.tag_exists("t"));
}
