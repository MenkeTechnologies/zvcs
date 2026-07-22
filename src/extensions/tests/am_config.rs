//! `git am` reads `am.threeway` and `am.messageId` in `git_am_config` before
//! option parsing, so they seed the default for `--3way`/`--message-id` and the
//! command line overrides them. Both values are written by `am_setup` into the
//! `.git/rebase-apply/threeway` and `.git/rebase-apply/messageid` state files,
//! which are left behind whenever the run stops — here on an empty patch
//! (`Patch is empty.`, exit 128) — and are therefore directly observable.
//!
//! These tests pin zvcs to stock git byte-for-byte (stdout + stderr + exit code
//! + the two state files + whether a session directory survives) across the
//! config default, a CLI override of that default, a CLI-only value, and a
//! malformed boolean (git's exact `git_config_bool` fatal at config-read time,
//! before any session is created).
//!
//! `am.keepcr` is deliberately *not* mapped: it only tunes `mailsplit`'s CR
//! handling, which this port does not implement, so it writes no state file and
//! must not perturb anything. The final test locks that in — setting it changes
//! nothing relative to stock git.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with a single commit and a patch mailbox whose message carries a
/// subject but no diff. Both git and zvcs treat such a message as an empty
/// patch and stop, leaving `.git/rebase-apply` populated for inspection. The
/// mailbox is produced by stock `git format-patch` so it is byte-identical to
/// what a real `git am` consumes.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-amcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f.txt"), "line1\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);

    // An empty commit format-patches to a message with a subject and no diff.
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "empty subject line"]);
    let mbox = Command::new("git")
        .args(["format-patch", "-1", "--stdout"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(mbox.status.success(), "format-patch failed");
    std::fs::write(repo.join("patch.mbox"), &mbox.stdout).unwrap();
    // Drop the empty commit so HEAD is the pristine base again.
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    (repo, home)
}

struct AmRun {
    out: Output,
    threeway: Option<Vec<u8>>,
    messageid: Option<Vec<u8>>,
    session: bool,
}

/// Run `<bin> am [extra] patch.mbox` under a byte-identical isolated
/// environment, capturing the process output plus the two state files and
/// whether a session directory was left behind. Any prior session is cleared
/// first so git and zvcs each start from nothing, and cleared afterward so the
/// two runs do not collide over `.git/rebase-apply`.
fn run_am(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> AmRun {
    let ra = repo.join(".git/rebase-apply");
    let _ = std::fs::remove_dir_all(&ra);
    let mut args = vec!["am"];
    args.extend_from_slice(extra);
    args.push("patch.mbox");
    let out = Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let threeway = std::fs::read(ra.join("threeway")).ok();
    let messageid = std::fs::read(ra.join("messageid")).ok();
    let session = ra.is_dir();
    let _ = std::fs::remove_dir_all(&ra);
    AmRun { out, threeway, messageid, session }
}

/// Assert zvcs and stock git agree on exit code, stdout, stderr, the two state
/// files this port drives, and whether a session survives.
fn assert_match(repo: &Path, home: &Path, extra: &[&str], what: &str) {
    let z = run_am(BIN, repo, home, extra);
    let g = run_am("git", repo, home, extra);
    assert_eq!(z.out.status.code(), g.out.status.code(), "{what}: exit code");
    assert_eq!(
        String::from_utf8_lossy(&z.out.stdout),
        String::from_utf8_lossy(&g.out.stdout),
        "{what}: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.out.stderr),
        String::from_utf8_lossy(&g.out.stderr),
        "{what}: stderr"
    );
    assert_eq!(z.threeway, g.threeway, "{what}: threeway state file");
    assert_eq!(z.messageid, g.messageid, "{what}: messageid state file");
    assert_eq!(z.session, g.session, "{what}: session directory survives");
}

#[test]
fn am_threeway_config_sets_default() {
    let (repo, home) = fixture("threeway-cfg");
    // Mixed case exercises git's case-insensitive variable lookup.
    git(&repo, &["config", "am.threeWay", "true"]);
    assert_match(&repo, &home, &[], "am.threeWay=true");
    // The default now flows into the state file as `t`.
    let z = run_am(BIN, &repo, &home, &[]);
    assert_eq!(z.threeway.as_deref(), Some(&b"t\n"[..]), "threeway defaulted on");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn am_threeway_cli_overrides_config() {
    let (repo, home) = fixture("threeway-override");
    git(&repo, &["config", "am.threeWay", "true"]);
    // `--no-3way` wins over the config default in both implementations.
    assert_match(&repo, &home, &["--no-3way"], "am.threeWay=true + --no-3way");
    let z = run_am(BIN, &repo, &home, &["--no-3way"]);
    assert_eq!(z.threeway.as_deref(), Some(&b"f\n"[..]), "CLI forced threeway off");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn am_threeway_cli_only_with_no_config() {
    let (repo, home) = fixture("threeway-cli");
    // No config at all; `--3way` alone still sets the state file to `t`.
    assert_match(&repo, &home, &["--3way"], "--3way, no config");
    let z = run_am(BIN, &repo, &home, &["--3way"]);
    assert_eq!(z.threeway.as_deref(), Some(&b"t\n"[..]), "CLI set threeway on");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn am_messageid_config_sets_default() {
    let (repo, home) = fixture("messageid-cfg");
    git(&repo, &["config", "am.messageId", "true"]);
    assert_match(&repo, &home, &[], "am.messageId=true");
    let z = run_am(BIN, &repo, &home, &[]);
    assert_eq!(z.messageid.as_deref(), Some(&b"t\n"[..]), "messageid defaulted on");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn am_messageid_cli_overrides_config() {
    let (repo, home) = fixture("messageid-override");
    git(&repo, &["config", "am.messageId", "true"]);
    assert_match(&repo, &home, &["--no-message-id"], "am.messageId=true + --no-message-id");
    let z = run_am(BIN, &repo, &home, &["--no-message-id"]);
    assert_eq!(z.messageid.as_deref(), Some(&b"f\n"[..]), "CLI forced messageid off");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn am_threeway_invalid_config_is_fatal() {
    let (repo, home) = fixture("threeway-bad");
    git(&repo, &["config", "am.threeWay", "notabool"]);
    // git dies at config-read time with the normalized (lowercased) key, before
    // any session directory exists; zvcs matches byte-for-byte.
    assert_match(&repo, &home, &[], "am.threeWay=notabool");
    let z = run_am(BIN, &repo, &home, &[]);
    assert_eq!(z.out.status.code(), Some(128), "config-read fatal exit");
    assert_eq!(
        String::from_utf8_lossy(&z.out.stderr),
        "fatal: bad boolean config value 'notabool' for 'am.threeway'\n",
        "config-read fatal message"
    );
    assert!(!z.session, "no session created on config error");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn am_messageid_invalid_config_is_fatal() {
    let (repo, home) = fixture("messageid-bad");
    git(&repo, &["config", "am.messageId", "maybe"]);
    assert_match(&repo, &home, &[], "am.messageId=maybe");
    let z = run_am(BIN, &repo, &home, &[]);
    assert_eq!(
        String::from_utf8_lossy(&z.out.stderr),
        "fatal: bad boolean config value 'maybe' for 'am.messageid'\n",
        "config-read fatal message"
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn am_keepcr_config_is_not_honored() {
    let (repo, home) = fixture("keepcr");
    // `am.keepcr` only governs mailsplit CR handling, which this port does not
    // implement: it writes no state file and must leave everything identical to
    // stock git, whose `threeway`/`messageid` stay at their `f` defaults.
    git(&repo, &["config", "am.keepcr", "true"]);
    assert_match(&repo, &home, &[], "am.keepcr=true");
    let z = run_am(BIN, &repo, &home, &[]);
    assert_eq!(z.threeway.as_deref(), Some(&b"f\n"[..]), "keepcr left threeway default");
    assert_eq!(z.messageid.as_deref(), Some(&b"f\n"[..]), "keepcr left messageid default");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
