//! What `git pack-objects` makes of stdin, and what `--thin` does to that.
//!
//! `cmd_pack_objects()` dispatches on one flag:
//!
//! ```c
//! } else if (!use_internal_rev_list) {
//!         read_object_list_from_stdin();
//! } else {
//!         [...] get_object_list(&revs, &rp);
//! }
//! ```
//!
//! (builtin/pack-objects.c:5388-5404.) With the internal rev list off, stdin is
//! a bare list of object ids that are packed as they stand; with it on, stdin is
//! a list of *revision arguments* fed to the walk (:4834-4858). `--revs` is only
//! one of the options that turns it on: `--all`, `--reflog`,
//! `--indexed-objects`, `--unpacked`, the two unreachable options, the promisor
//! options and — the one this file is mostly about — `--thin` all do
//! (:5233-5277).
//!
//! `--thin` setting it at :5233 is what makes
//! `git rev-list --objects --all | git pack-objects --thin --stdout` fail in
//! stock git: the `<oid> <path>` lines `rev-list --objects` prints are revision
//! arguments there, not object ids, and the trailing path makes each one a bad
//! revision. It is also what makes `--thin` incompatible with `--stdin-packs`
//! (:5312) and `--cruft` (:5316), both of which refuse the internal rev list.
//!
//! Every expectation below was measured against git 2.55.0.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(dir: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x");
    c
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = cmd(dir).args(args).output().expect("run binary");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

/// Run with `input` on stdin and hand back the whole outcome.
fn piped(dir: &Path, args: &[&str], input: &str) -> Output {
    let mut child = cmd(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn binary");
    child.stdin.take().expect("stdin").write_all(input.as_bytes()).expect("write stdin");
    child.wait_with_output().expect("wait")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn rev(dir: &Path, spec: &str) -> String {
    String::from_utf8_lossy(&ok(dir, &["rev-parse", spec]).stdout).trim().to_string()
}

/// Three commits, so a walk from the tip has somewhere to go.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-postdin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    ok(&dir, &["init", "-q", "-b", "main"]);
    for name in ["a", "b", "c"] {
        std::fs::write(dir.join(format!("{name}.txt")), format!("{name}\n")).expect("write");
        ok(&dir, &["add", &format!("{name}.txt")]);
        ok(&dir, &["commit", "-qm", name]);
    }
    dir
}

/// The property that defines `--thin`'s stdin: it is the internal rev list, so a
/// tip on stdin is *walked*. With no exclusion in play that is the same pack
/// `--revs` produces from the same input, byte for byte — the two flags reach
/// `get_object_list()` through the same door.
///
/// Without the fix `--thin` took the object-list door instead and packed the one
/// named commit, which is the pack the third assertion pins as *different*.
#[test]
fn thin_reads_stdin_as_revisions_like_revs_does() {
    let dir = fixture("thin");
    let head = rev(&dir, "HEAD");
    let input = format!("{head}\n");

    let thin = piped(&dir, &["pack-objects", "--stdout", "--thin"], &input);
    let revs = piped(&dir, &["pack-objects", "--stdout", "--revs"], &input);
    let list = piped(&dir, &["pack-objects", "--stdout"], &input);
    assert!(thin.status.success(), "{}", stderr_of(&thin));
    assert!(revs.status.success(), "{}", stderr_of(&revs));
    assert!(list.status.success(), "{}", stderr_of(&list));

    assert_eq!(thin.stdout, revs.stdout, "--thin walks the tip exactly as --revs does");
    assert_ne!(
        thin.stdout, list.stdout,
        "and is not the one-entry pack a bare object list produces"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `rev-list --objects` prints `<oid> <path>`, and as a *revision argument* that
/// whole line — trailing space and all — is what fails to resolve. The message
/// quotes the line unstripped, which is only true because `get_object_list()`
/// removes the newline and nothing else.
#[test]
fn thin_rejects_a_rev_list_objects_feed() {
    let dir = fixture("feed");
    let objects = String::from_utf8_lossy(&ok(&dir, &["rev-list", "--objects", "--all"]).stdout)
        .into_owned();

    let out = piped(&dir, &["pack-objects", "--stdout", "--thin"], &objects);
    assert!(!out.status.success(), "stock dies here");
    assert!(
        stderr_of(&out).starts_with("fatal: bad revision '"),
        "{}",
        stderr_of(&out)
    );

    // The same feed is fine when stdin really is an object list.
    let list = piped(&dir, &["pack-objects", "--stdout"], &objects);
    assert!(list.status.success(), "{}", stderr_of(&list));
    std::fs::remove_dir_all(&dir).ok();
}

/// `--thin` turns the internal rev list on before both refusals, so it trips
/// them even though neither names it.
#[test]
fn thin_conflicts_with_stdin_packs_and_cruft() {
    let dir = fixture("conflict");
    for (args, message) in [
        (
            ["pack-objects", "--stdout", "--cruft", "--thin"],
            "fatal: cannot use internal rev list with --cruft\n",
        ),
        (
            ["pack-objects", "--stdout", "--stdin-packs", "--thin"],
            "fatal: cannot use internal rev list with --stdin-packs\n",
        ),
    ] {
        let out = piped(&dir, &args, "");
        assert!(!out.status.success(), "{args:?} should die");
        assert_eq!(stderr_of(&out), message, "{args:?}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// `--all` turns the internal rev list on by itself, so stdin is revisions there
/// too — an unreachable commit named on stdin joins the pack. Pinned as the
/// `--revs` pack rather than a byte count so no zlib detail is asserted.
#[test]
fn all_reads_stdin_as_revisions_without_revs() {
    let dir = fixture("all");
    let tree = rev(&dir, "HEAD^{tree}");
    let loose =
        String::from_utf8_lossy(&ok(&dir, &["commit-tree", "-m", "orphan", &tree]).stdout)
            .trim()
            .to_string();
    let input = format!("{loose}\n");

    let plain = piped(&dir, &["pack-objects", "--stdout", "--all"], &input);
    let with_revs = piped(&dir, &["pack-objects", "--stdout", "--all", "--revs"], &input);
    let empty = piped(&dir, &["pack-objects", "--stdout", "--all"], "");
    assert!(plain.status.success(), "{}", stderr_of(&plain));

    assert_eq!(plain.stdout, with_revs.stdout, "--revs adds nothing --all had not already");
    assert_ne!(plain.stdout, empty.stdout, "the orphan commit on stdin reached the pack");
    std::fs::remove_dir_all(&dir).ok();
}

/// The `-` branch of the loop, and the `!len` break in front of it.
#[test]
fn the_stdin_loop_reproduces_gits_line_grammar() {
    let dir = fixture("grammar");
    let head = rev(&dir, "HEAD");

    let bad = piped(&dir, &["pack-objects", "--stdout", "--revs"], "-x\n");
    assert_eq!(stderr_of(&bad), "fatal: not a rev '-x'\n");

    let shallow = piped(&dir, &["pack-objects", "--stdout", "--revs"], "--shallow zzz\n");
    assert_eq!(stderr_of(&shallow), "fatal: not an object name 'zzz'\n");

    let unknown = piped(&dir, &["pack-objects", "--stdout", "--revs"], "nosuchref\n");
    assert_eq!(stderr_of(&unknown), "fatal: bad revision 'nosuchref'\n");

    // An exclusion is validated even though the walk cannot honour it yet.
    let excluded = piped(&dir, &["pack-objects", "--stdout", "--revs"], "^nosuchref\n");
    assert_eq!(stderr_of(&excluded), "fatal: bad revision '^nosuchref'\n");

    // `if (!len) break;`: nothing past a blank line is read, so the bad
    // revision behind it is never seen.
    let stopped =
        piped(&dir, &["pack-objects", "--stdout", "--revs"], &format!("{head}\n\nnosuchref\n"));
    assert!(stopped.status.success(), "{}", stderr_of(&stopped));

    // `--not` is a line the loop understands rather than a bad rev.
    let not = piped(&dir, &["pack-objects", "--stdout", "--revs"], "--not\n");
    assert!(not.status.success(), "{}", stderr_of(&not));
    std::fs::remove_dir_all(&dir).ok();
}
