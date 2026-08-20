//! `push.negotiate` and `push.useBitmaps`, the two keys `send_pack()` reads for
//! itself (`send-pack.c:549-560`).
//!
//! Neither acceleration exists here — there are no negotiation rounds before a
//! push and `gix-pack` has no bitmap reader — so what is portable is *when* and
//! *how* git refuses a value it cannot read, and that is what these assert.
//!
//! The "when" is the interesting half. Both reads sit inside `send_pack()`,
//! after the `if (!remote_refs)` early return and therefore after the transport
//! is already open, so git 2.55.0:
//!
//! * reports them for an already-up-to-date push and for `--dry-run`, and
//! * does *not* report them when the remote could not be reached at all.
//!
//! A read hoisted to option-parsing time would pass a naive "does it die" test
//! and fail both of those, which is why they are here.
//!
//! git prints a second line for the local transport — `fatal: the remote end
//! hung up unexpectedly`, from the `receive-pack` child noticing its parent has
//! gone — that this port's in-process receive path has no counterpart for. The
//! assertions are on git's own first line and on the exit code.
//!
//! Literals captured from git 2.55.0 (`/opt/homebrew/bin/git`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn ok(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(cwd, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// A one-commit repository with a local bare `origin`, plus an isolated `HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pushneg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let bare = root.join("origin.git");
    ok(&root, &home, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    ok(&repo, &home, &["remote", "add", "origin", bare.to_str().unwrap()]);
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

const KEYS: &[(&str, &str)] = &[
    ("push.negotiate", "push.negotiate"),
    ("push.useBitmaps", "push.usebitmaps"),
];

#[test]
fn a_bad_boolean_is_fatal_when_the_push_reaches_send_pack() {
    // git 2.55.0:
    //     $ git -c push.negotiate=bogus push origin HEAD:refs/heads/main
    //     fatal: bad boolean config value 'bogus' for 'push.negotiate'
    //     …
    //     (exit 128)
    for (key, lowered) in KEYS {
        let (repo, home) = fixture(&format!("bad-{}", lowered.replace('.', "-")));
        let out = run(
            &repo,
            &home,
            &["-c", &format!("{key}=bogus"), "push", "origin", "HEAD:refs/heads/main"],
        );
        assert_eq!(code(&out), 128, "{key}=bogus must die");
        assert_eq!(
            stderr(&out).lines().next().unwrap_or_default(),
            format!("fatal: bad boolean config value 'bogus' for '{lowered}'"),
            "{key}=bogus must be reported with git's wording"
        );
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}

#[test]
fn a_bad_boolean_is_still_fatal_for_a_dry_run_and_for_an_up_to_date_push() {
    // Both cases still enter `send_pack()`, so both still report — this is the
    // half that pins the read to the transport rather than to option parsing.
    let (repo, home) = fixture("dryrun");
    ok(&repo, &home, &["push", "-q", "origin", "HEAD:refs/heads/main"]);

    let up_to_date = run(
        &repo,
        &home,
        &["-c", "push.negotiate=bogus", "push", "origin", "HEAD:refs/heads/main"],
    );
    assert_eq!(code(&up_to_date), 128, "an up-to-date push still reaches send-pack");
    assert!(
        stderr(&up_to_date).starts_with("fatal: bad boolean config value 'bogus' for 'push.negotiate'"),
        "got: {}",
        stderr(&up_to_date)
    );

    let dry = run(
        &repo,
        &home,
        &["-c", "push.useBitmaps=bogus", "push", "--dry-run", "origin", "HEAD:refs/heads/main"],
    );
    assert_eq!(code(&dry), 128, "--dry-run still reaches send-pack");
    assert!(
        stderr(&dry).starts_with("fatal: bad boolean config value 'bogus' for 'push.usebitmaps'"),
        "got: {}",
        stderr(&dry)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn an_unreachable_remote_reports_the_transport_not_the_config() {
    // git never gets to `send_pack()` when the connection fails, so the config is
    // never read and the transport's own diagnostic is what the user sees. A read
    // done earlier would shadow it.
    let (repo, home) = fixture("noremote");
    let out = run(&repo, &home, &["-c", "push.negotiate=bogus", "push", "nosuchremote"]);
    assert!(
        !stderr(&out).contains("bad boolean config value"),
        "the config must not be reached: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("does not appear to be a git repository"),
        "the transport reports instead: {}",
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn readable_values_leave_the_push_working() {
    // The other half of the claim: every value git can read must be accepted, and
    // the push must still land. `push.negotiate=true` asks for negotiation rounds
    // this port does not perform; git's documented fallback for `false` — "rely
    // solely on the server's ref advertisement" — is what happens either way, so
    // the ref still moves.
    for (i, (key, value)) in [
        ("push.negotiate", "true"),
        ("push.negotiate", "false"),
        ("push.useBitmaps", "true"),
        ("push.useBitmaps", "false"),
        ("push.useBitmaps", "0"),
        ("push.negotiate", "on"),
    ]
    .iter()
    .enumerate()
    {
        let (repo, home) = fixture(&format!("good-{i}"));
        let out = run(
            &repo,
            &home,
            &["-c", &format!("{key}={value}"), "push", "-q", "origin", "HEAD:refs/heads/main"],
        );
        assert!(out.status.success(), "{key}={value} must be accepted: {}", stderr(&out));

        let landed = ok(&repo, &home, &["ls-remote", "origin", "refs/heads/main"]);
        let expected = ok(&repo, &home, &["rev-parse", "HEAD"]);
        let tip = String::from_utf8_lossy(&expected.stdout).trim().to_string();
        assert!(
            String::from_utf8_lossy(&landed.stdout).starts_with(&tip),
            "{key}={value}: the push must still have landed; remote has {:?}, local tip is {tip}",
            String::from_utf8_lossy(&landed.stdout)
        );
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}
