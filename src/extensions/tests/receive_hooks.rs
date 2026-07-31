//! The receiving half of a push beyond "the ref moved": the hooks
//! `receive-pack` runs, the push options and push-certificate nonce it
//! negotiates, the `proc-receive` protocol `receive.procReceiveRefs` diverts a
//! command into, and the all-or-nothing guarantee `--atomic` makes.
//!
//! Every expectation here was taken from stock git 2.55.0 running the same
//! hooks against the same repository.
//!
//! The assertions read *server-side* evidence — the refs the bare repository
//! ends up with, and files the hooks wrote — rather than the pusher's terminal.
//! That keeps them about `receive-pack` and independent of how much of the
//! sideband and of `report-status-v2` the client end happens to render.
//!
//! Only the hooks that run *before* the report can be observed here. The push
//! is driven by this binary's own client, whose transport kills
//! `git-receive-pack` the moment the report has been read
//! (`gix-transport/src/client/blocking_io/file.rs:228`), so `post-receive` and
//! `post-update` never finish. Their behaviour is covered by differential runs
//! against stock git's client instead.
//!
//! Unix-only (symlinks + executable hook scripts); skipped elsewhere.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One fully wired scratch area: a work tree with two commits and a bare remote,
/// both served by the binary under test.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    bindir: PathBuf,
    work: PathBuf,
    bare: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-recvhook-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let f = Fixture {
            home: root.join("home"),
            bindir: root.join("bin"),
            work: root.join("work"),
            bare: root.join("remote.git"),
            root,
        };
        for d in [&f.home, &f.bindir, &f.work] {
            std::fs::create_dir_all(d).unwrap();
        }
        // The transport spawns `git-receive-pack`; point it at the binary under
        // test so our code, not stock git, serves the push.
        for name in ["git", "git-receive-pack", "git-upload-pack"] {
            std::os::unix::fs::symlink(BIN, f.bindir.join(name)).unwrap();
        }

        f.run_in(&f.work, &["init", "-q", "--bare", "-b", "main", f.bare.to_str().unwrap()]);
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("f"), "one\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "c0"]);
        std::fs::write(f.work.join("f"), "two\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "c1"]);
        f.git(&["remote", "add", "origin", f.bare.to_str().unwrap()]);
        f
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("PATH", path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run binary")
    }

    fn git(&self, args: &[&str]) -> Output {
        self.run_in(&self.work.clone(), args)
    }

    /// Set a configuration variable on the *receiving* repository, from inside
    /// it — the operand-based spellings are not what is under test here.
    fn remote_config(&self, key: &str, value: &str) {
        let out = self.run_in(&self.bare.clone(), &["config", key, value]);
        assert!(out.status.success(), "config {key} failed");
    }

    /// Install an executable hook on the receiving repository.
    fn hook(&self, name: &str, body: &str) {
        let path = self.bare.join("hooks").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// What a hook wrote to `<git-dir>/<name>`, or an empty string.
    fn hook_log(&self, name: &str) -> String {
        std::fs::read_to_string(self.bare.join(name)).unwrap_or_default()
    }

    /// Every ref the receiving repository ends up with, as `<oid> <name>` lines.
    fn remote_refs(&self) -> String {
        let out = self.run_in(&self.bare.clone(), &["show-ref"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Whether the receiving repository has `name` at all.
    fn has_ref(&self, name: &str) -> bool {
        self.remote_refs().lines().any(|l| l.ends_with(name))
    }
}

/// One pkt-line: a four-digit hex length that counts its own header.
fn pkt(payload: &str) -> String {
    format!("{:04x}{payload}", payload.len() + 4)
}

/// The `proc-receive` hook's half of the version handshake: `version=1` and a
/// flush, with no capabilities asked for.
fn version_reply() -> String {
    format!("{}0000", pkt("version=1\n"))
}

/// A `proc-receive` hook that answers from a canned script.
///
/// It replies to the handshake, then *drains its stdin to end of file* before
/// sending the report. That is what a real hook does, and it is what makes the
/// exchange deterministic: the server writes the command list and closes the
/// pipe, so a hook that exited early would have the server's write fail with
/// `EPIPE` instead — a race, not a test.
fn proc_receive_hook(version: &str, report: &str) -> String {
    format!(
        "#!/bin/sh\n\
         printf '%s' '{version}'\n\
         cat > /dev/null\n\
         printf '%s' '{report}'\n"
    )
}

/// A hook that records the `<old> <new> <ref>` lines it was fed and the push
/// options it inherited, so the test can read them back off disk.
fn recording_hook(log: &str) -> String {
    format!(
        "#!/bin/sh\n\
         while read old new ref; do echo \"ref $ref\" >> \"$GIT_DIR/{log}\"; done\n\
         echo \"count ${{GIT_PUSH_OPTION_COUNT-absent}}\" >> \"$GIT_DIR/{log}\"\n\
         echo \"opt0 ${{GIT_PUSH_OPTION_0-absent}}\" >> \"$GIT_DIR/{log}\"\n\
         exit 0\n"
    )
}

/// `receive.advertisePushOptions` makes `receive-pack` read the option pkt-lines
/// off the wire — they arrive *before* the pack, so a server that skips them
/// desynchronises the stream — and hand them to the hooks as
/// `GIT_PUSH_OPTION_COUNT` / `GIT_PUSH_OPTION_<n>`.
#[test]
fn push_options_reach_the_pre_receive_hook() {
    let f = Fixture::new("opts");
    f.remote_config("receive.advertisePushOptions", "true");
    f.hook("pre-receive", &recording_hook("pre.log"));

    let out = f.git(&["push", "-o", "alpha", "-o", "beta", "origin", "main"]);
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = f.hook_log("pre.log");
    assert!(log.contains("ref refs/heads/main"), "hook was not fed the command: {log}");
    assert!(log.contains("count 2"), "hook did not see two push options: {log}");
    assert!(log.contains("opt0 alpha"), "hook did not see the first option: {log}");
    // The pack must still have been read correctly after the options.
    assert!(f.has_ref("refs/heads/main"), "ref did not land: {}", f.remote_refs());
}

/// Without the capability advertised the client sends no options, and git still
/// exports the count — as `0`, not as an absent variable: `execute_commands()`
/// always hands `run_receive_hook()` a real (if empty) option list.
#[test]
fn push_option_count_is_zero_not_absent_when_none_were_negotiated() {
    let f = Fixture::new("noopts");
    f.hook("pre-receive", &recording_hook("pre.log"));

    let out = f.git(&["push", "origin", "main"]);
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = f.hook_log("pre.log");
    assert!(log.contains("count 0"), "expected count 0, got: {log}");
}

/// A `pre-receive` hook that exits non-zero vetoes the whole push, and no ref
/// moves.
#[test]
fn a_declining_pre_receive_hook_stops_every_ref() {
    let f = Fixture::new("decline");
    f.hook("pre-receive", "#!/bin/sh\nexit 1\n");

    let out = f.git(&["push", "origin", "main:refs/heads/a", "main:refs/heads/b"]);
    assert!(!out.status.success(), "declined push must fail");
    assert!(!f.has_ref("refs/heads/a"), "ref a moved: {}", f.remote_refs());
    assert!(!f.has_ref("refs/heads/b"), "ref b moved: {}", f.remote_refs());
}

/// `receive.certNonceSeed` turns the server into a signed-push server: the
/// advertisement gains `push-cert=<stamp>-<hmac>`, the HMAC being the
/// repository's hash width in hex. Without the seed the capability is absent.
#[test]
fn cert_nonce_seed_adds_the_push_cert_capability_to_the_advertisement() {
    let f = Fixture::new("nonce");
    let bare = f.bare.to_str().unwrap().to_string();

    let without = f.git(&["receive-pack", "--advertise-refs", &bare]);
    let without = String::from_utf8_lossy(&without.stdout).into_owned();
    assert!(
        !without.contains("push-cert="),
        "unseeded server must not offer push-cert: {without}"
    );

    f.remote_config("receive.certNonceSeed", "s3cr3t");
    let with = f.git(&["receive-pack", "--advertise-refs", &bare]);
    let with = String::from_utf8_lossy(&with.stdout).into_owned();
    let nonce = with
        .split("push-cert=")
        .nth(1)
        .and_then(|rest| rest.split([' ', '\n', '\0']).next())
        .unwrap_or_default()
        .to_string();
    let (stamp, mac) = nonce
        .split_once('-')
        .unwrap_or_else(|| panic!("nonce is not <stamp>-<hmac>: {nonce:?} in {with}"));
    assert!(
        !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_digit()),
        "nonce stamp is not a timestamp: {nonce:?}"
    );
    assert_eq!(mac.len(), 40, "sha1 repository, so the hmac is 40 hex digits: {nonce:?}");
    assert!(
        mac.chars().all(|c| c.is_ascii_hexdigit()),
        "nonce hmac is not hex: {nonce:?}"
    );

    // Two advertisements a second apart must differ: the stamp is part of the
    // signed material, so a replayed certificate cannot pass twice.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let again = f.git(&["receive-pack", "--advertise-refs", &bare]);
    let again = String::from_utf8_lossy(&again.stdout).into_owned();
    assert!(
        !again.contains(&nonce),
        "the nonce must be re-minted per session: {again}"
    );
}

/// A command whose ref matches `receive.procReceiveRefs` is handed to the
/// `proc-receive` hook and never reaches the ref store.
///
/// What the hook's `option refname` answer does to the `report-status-v2` reply
/// and to `post-receive` is deliberately *not* asserted here: this binary's own
/// push client tears the connection down as soon as it has the report, so the
/// hooks that run after it never get to finish. That half is covered by the
/// differential runs against stock git's client.
#[test]
fn proc_receive_owns_matching_refs() {
    let f = Fixture::new("procrecv");
    f.remote_config("receive.procReceiveRefs", "refs/for");

    let reply = format!(
        "{}{}0000",
        pkt("ok refs/for/main\n"),
        pkt("option refname refs/pull/7/head\n"),
    );
    f.hook("proc-receive", &proc_receive_hook(&version_reply(), &reply));

    let out = f.git(&["push", "origin", "main:refs/for/main"]);
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !f.has_ref("refs/for/main"),
        "proc-receive refs must never reach the ref store: {}",
        f.remote_refs()
    );
}

/// `option fall-through` hands the command back: `receive-pack` executes it
/// itself, so the ref really is created under the pushed name.
#[test]
fn proc_receive_fall_through_returns_the_command_to_receive_pack() {
    let f = Fixture::new("procft");
    f.remote_config("receive.procReceiveRefs", "refs/for");
    let reply = format!(
        "{}{}0000",
        pkt("ok refs/for/main\n"),
        pkt("option fall-through\n"),
    );
    f.hook("proc-receive", &proc_receive_hook(&version_reply(), &reply));

    let out = f.git(&["push", "origin", "main:refs/for/main"]);
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.has_ref("refs/for/main"),
        "a fall-through command must be executed normally: {}",
        f.remote_refs()
    );
}

/// A `proc-receive` hook that answers `ng` fails just that ref.
#[test]
fn proc_receive_ng_rejects_the_ref() {
    let f = Fixture::new("procng");
    f.remote_config("receive.procReceiveRefs", "refs/for");
    let reply = format!("{}0000", pkt("ng refs/for/main not allowed here\n"));
    f.hook("proc-receive", &proc_receive_hook(&version_reply(), &reply));

    let out = f.git(&["push", "origin", "main:refs/for/main"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "declined push must fail: {err}");
    assert!(
        err.contains("not allowed here"),
        "the hook's reason must reach the pusher: {err}"
    );
    assert!(!f.has_ref("refs/for/main"), "rejected ref was written");
}

/// A `receive.procReceiveRefs` prefix only matches at a `/` boundary, so a
/// sibling ref with the same textual prefix goes through the ref store as usual.
///
/// The sibling has to be two levels deep: `update()` refuses anything under
/// `refs/` that `check_refname_format()` rejects without `ALLOW_ONELEVEL`, so a
/// bare `refs/formal` is a `funny refname` for git too and would prove nothing
/// about prefix matching.
#[test]
fn proc_receive_prefix_matches_only_at_a_slash_boundary() {
    let f = Fixture::new("procbound");
    f.remote_config("receive.procReceiveRefs", "refs/for");
    // Any invocation of the hook is a failure, so make it one.
    f.hook("proc-receive", "#!/bin/sh\nexit 1\n");

    let out = f.git(&["push", "origin", "main:refs/format/main"]);
    assert!(
        out.status.success(),
        "refs/format/ is not under refs/for/: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(f.has_ref("refs/format/main"), "ref did not land: {}", f.remote_refs());
}

/// `--atomic` is all or nothing: an `update` hook that vetoes the second ref
/// must leave the first one unwritten too. The same push without `--atomic`
/// lands the allowed ref, which is what makes this a test of atomicity rather
/// than of the hook.
#[test]
fn an_atomic_push_writes_nothing_when_one_ref_is_declined() {
    let f = Fixture::new("atomic");
    f.hook("update", "#!/bin/sh\ncase \"$1\" in */veto) exit 1;; esac\nexit 0\n");

    let out = f.git(&["push", "--atomic", "origin", "main:refs/heads/ok", "main:refs/heads/veto"]);
    assert!(!out.status.success(), "declined atomic push must fail");
    assert!(
        !f.has_ref("refs/heads/ok"),
        "an atomic push wrote a ref anyway: {}",
        f.remote_refs()
    );
    assert!(!f.has_ref("refs/heads/veto"), "vetoed ref was written");

    let out = f.git(&["push", "origin", "main:refs/heads/ok", "main:refs/heads/veto"]);
    assert!(!out.status.success(), "the vetoed ref still fails the push");
    assert!(
        f.has_ref("refs/heads/ok"),
        "a non-atomic push must still land the allowed ref: {}",
        f.remote_refs()
    );
    assert!(!f.has_ref("refs/heads/veto"), "vetoed ref was written");
}
