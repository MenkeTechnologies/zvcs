//! `git send-pack` parity: the compare-and-swap options, the refspec-matching
//! refusal, and the capability list the client puts on the wire.
//!
//! Every case here was measured against stock git 2.55.0 driving a real
//! `git-receive-pack` over pipes, and pins a behaviour the port did not have:
//!
//!   * `--force-with-lease=<ref>:<expect>` was parsed and then dropped on the
//!     floor — the expected value never reached the wire layer, so a lease the
//!     advertisement contradicts reported `(non-fast-forward)` instead of
//!     `(stale info)` and a lease that held did not force
//!     (`apply_push_cas()`, send-pack.c:313-314; `apply_cas()`, remote.c:2818-2852).
//!   * `<expect>` had to resolve locally, so the one value a lease is usually
//!     spelled with — the remote's tip, which a stale checkout has never seen —
//!     was rejected outright (`repo_get_oid()` in `parse_push_cas_option()`,
//!     remote.c:2656).
//!   * a source refspec that matched nothing stopped at the first one and left
//!     with the dispatcher's status instead of reporting each and returning the
//!     -1 `run_builtin()` masks to 255 (remote.c:1179, send-pack.c:309-312).
//!   * the `quiet` capability was never requested, `object-format` was written
//!     before `atomic`/`push-options` rather than after, and `session-id` was
//!     never sent at all (send-pack.c:617-634).
//!
//! The wire cases read the client's first pkt-line back out of a
//! `--receive-pack` wrapper that tees its stdin, which is the only way to see
//! the capability list without a network.
//!
//! Unix-only: the transport is pointed at the binary under test with symlinks,
//! and the tee needs an executable shell script. No sockets, no network, and
//! nothing that needs gpg.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A work tree with two commits on `main`, and a bare remote.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    bindir: PathBuf,
    src: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.home.join("gitsystem"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .stdin(std::process::Stdio::null());
        cmd.output().expect("run binary")
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn remote_str(&self) -> String {
        self.remote.to_str().unwrap().to_owned()
    }

    fn remote_main(&self) -> String {
        self.ok(&self.remote, &["rev-parse", "refs/heads/main"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "zvcs-send-pack-lease-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let fx = Fixture {
        home: root.join("home"),
        bindir: root.join("bin"),
        src: root.join("src"),
        remote: root.join("remote.git"),
        root,
    };
    for dir in [&fx.home, &fx.bindir, &fx.src] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // The transport looks the service up on `PATH`; serve it with the binary
    // under test so the whole exchange stays inside this process tree.
    for name in ["git", "git-receive-pack", "git-upload-pack"] {
        std::os::unix::fs::symlink(BIN, fx.bindir.join(name)).unwrap();
    }

    fx.ok(&fx.src, &["init", "-q", "--bare", "-b", "main", fx.remote.to_str().unwrap()]);
    fx.ok(&fx.src, &["init", "-q", "-b", "main", "."]);
    fx.ok(&fx.src, &["config", "user.email", "t@e.co"]);
    fx.ok(&fx.src, &["config", "user.name", "t"]);
    std::fs::write(fx.src.join("a.txt"), "one\n").unwrap();
    fx.ok(&fx.src, &["add", "a.txt"]);
    fx.ok(&fx.src, &["commit", "-q", "-m", "one"]);
    std::fs::write(fx.src.join("a.txt"), "two\n").unwrap();
    fx.ok(&fx.src, &["commit", "-q", "-a", "-m", "two"]);
    fx
}

/// Publish `main`, then rewind the local branch one commit, so the remote is
/// strictly ahead: any further push of `main` is a non-fast-forward, and the
/// remote's tip is a value a lease can be spelled with.
fn published_then_rewound(fx: &Fixture) -> String {
    fx.ok(&fx.src, &["send-pack", &fx.remote_str(), "main"]);
    let published = fx.remote_main();
    fx.ok(&fx.src, &["reset", "-q", "--hard", "HEAD~1"]);
    published
}

/// An executable `--receive-pack` that copies the client's byte stream into
/// `capture` before handing it to the real service.
fn teeing_receive_pack(fx: &Fixture, capture: &Path) -> String {
    let script = fx.root.join("tee-rp.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ntee '{}' | '{BIN}' receive-pack \"$@\"\n",
            capture.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_str().unwrap().to_owned()
}

/// A canned `--receive-pack` that advertises an empty repository with exactly
/// `caps`, answers `unpack ok`, and then copies everything the client sent into
/// `capture`.
///
/// Needed for capabilities this repository's own `receive-pack` does not
/// advertise — `session-id` among them — where a real server could never make
/// the client offer them back. Safe because the request is tiny: the client's
/// command list and pack together are a few hundred bytes, far inside a pipe
/// buffer, so writing the response before draining the request cannot deadlock.
fn canned_receive_pack(fx: &Fixture, tag: &str, caps: &str, capture: &Path) -> String {
    let script = fx.root.join(format!("canned-rp-{tag}.sh"));
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
zero=0000000000000000000000000000000000000000
head="$zero capabilities^{{}}"
caps='{caps}'
len=$(( ${{#head}} + 1 + ${{#caps}} + 1 + 4 ))
printf '%04x%s' "$len" "$head"
printf '\000%s\n' "$caps"
printf '0000'
printf '000eunpack ok\n0000'
cat > '{}'
"#,
            capture.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_str().unwrap().to_owned()
}

/// A `--receive-pack` that advertises an empty repository over `report-status`,
/// swallows whatever the client sends, and answers `unpack ok` with no verdict
/// for any ref.
///
/// That is the state git calls `REF_STATUS_EXPECTING_REPORT`: the command went
/// out and the report ended before answering it. A real `receive-pack` gets
/// there by dying part-way through (two refspecs writing one destination), which
/// needs a server-side refusal this port does not have yet — the canned one
/// reproduces the client-side situation exactly and depends on nothing.
fn silent_report_receive_pack(fx: &Fixture) -> String {
    let script = fx.root.join("silent-rp.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
zero=0000000000000000000000000000000000000000
head="$zero capabilities^{}"
caps='report-status object-format=sha1'
len=$(( ${#head} + 1 + ${#caps} + 1 + 4 ))
printf '%04x%s' "$len" "$head"
printf '\000%s\n' "$caps"
printf '0000'
printf '000eunpack ok\n0000'
cat > /dev/null
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_str().unwrap().to_owned()
}

/// The capability string of the client's first command pkt-line: everything
/// after the NUL that `send_pack()` glues `cap_buf` on with
/// (`"%s %s %s%c%s"`, send-pack.c:700-703).
fn capabilities(capture: &Path) -> String {
    let bytes = std::fs::read(capture).expect("the wrapper captured nothing");
    let mut off = 0usize;
    while off + 4 <= bytes.len() {
        let header = std::str::from_utf8(&bytes[off..off + 4]).expect("pkt-line header");
        let len = usize::from_str_radix(header, 16).expect("pkt-line length");
        if len < 4 || off + len > bytes.len() {
            break;
        }
        let line = &bytes[off + 4..off + len];
        if let Some(nul) = line.iter().position(|b| *b == 0) {
            return String::from_utf8_lossy(&line[nul + 1..]).trim().to_owned();
        }
        off += len;
    }
    panic!("no command pkt-line with a capability list in the captured stream");
}

#[test]
fn a_lease_the_advertisement_contradicts_is_stale_info() {
    let fx = fixture("stale");
    let published = published_then_rewound(&fx);
    // Lease the value the remote had *before* it moved: HEAD is now the parent
    // of what the remote carries, so `oideq(&ref->old_oid, &ref->old_oid_expect)`
    // fails and `set_ref_status_for_push()` takes the `REF_STATUS_REJECT_STALE`
    // arm before the ladder (remote.c:1698-1711).
    let out = fx.run(
        &fx.src,
        &[
            "send-pack",
            &fx.remote_str(),
            "--force-with-lease=refs/heads/main:HEAD",
            "main",
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("! [rejected]") && err.contains("(stale info)"),
        "a contradicted lease must report (stale info), not the plain ladder; stderr was: {err}"
    );
    assert_eq!(out.status.code(), Some(1), "stderr was: {err}");
    assert_eq!(fx.remote_main(), published, "a stale lease must not move the ref");
}

#[test]
fn a_lease_that_holds_forces_past_a_non_fast_forward() {
    let fx = fixture("holds");
    let published = published_then_rewound(&fx);
    let local = fx.ok(&fx.src, &["rev-parse", "HEAD"]);
    assert_ne!(local, published, "the fixture must be behind the remote");

    // The lease names exactly what the remote advertises, so it holds and
    // *becomes* the force (`force_ref_update = 1`, remote.c:1698-1710) — the
    // rewind lands without `--force` anywhere on the command line.
    let spec = format!("--force-with-lease=refs/heads/main:{published}");
    let out = fx.run(&fx.src, &["send-pack", &fx.remote_str(), &spec, "main"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the lease held but the push failed: {err}");
    assert!(
        err.contains("(forced update)"),
        "a lease that holds is a forced update; stderr was: {err}"
    );
    assert_eq!(fx.remote_main(), local, "the forced update never landed: {err}");
}

#[test]
fn a_full_length_hex_expect_needs_no_local_object() {
    let fx = fixture("ghost");
    let published = published_then_rewound(&fx);
    // 40 hex digits naming nothing in this repository. `get_oid_basic()` takes
    // the full-length case through `get_oid_hex()` and never looks the object
    // up, which is what lets a lease name a tip this checkout has not fetched.
    let out = fx.run(
        &fx.src,
        &[
            "send-pack",
            &fx.remote_str(),
            "--force-with-lease=refs/heads/main:0123456789012345678901234567890123456789",
            "main",
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("cannot parse expected object name"),
        "a full-length hex must be accepted as written; stderr was: {err}"
    );
    assert_ne!(out.status.code(), Some(129), "that is a parse refusal: {err}");
    // It is still a lease, and one nothing can satisfy.
    assert!(
        err.contains("(stale info)"),
        "an unmatchable lease is stale info; stderr was: {err}"
    );
    assert_eq!(fx.remote_main(), published, "a stale lease must not move the ref");

    // One digit short is git's parse error, and that refusal must survive.
    let out = fx.run(
        &fx.src,
        &[
            "send-pack",
            &fx.remote_str(),
            "--force-with-lease=refs/heads/main:012345678901234567890123456789012345678",
            "main",
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(129), "stderr was: {err}");
    assert!(
        err.contains(
            "error: cannot parse expected object name '012345678901234567890123456789012345678'"
        ),
        "stderr was: {err}"
    );
}

#[test]
fn a_lease_on_another_ref_leaves_this_one_alone() {
    let fx = fixture("otherref");
    let published = published_then_rewound(&fx);
    // `refname_match()` fails for this entry, so `apply_cas()` returns without
    // setting `expect_old_sha1` and the ordinary ladder decides — the rewind is
    // a plain non-fast-forward, not a lease outcome.
    let out = fx.run(
        &fx.src,
        &[
            "send-pack",
            &fx.remote_str(),
            "--force-with-lease=refs/heads/unrelated:HEAD",
            "main",
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("(non-fast-forward)") && !err.contains("(stale info)"),
        "a lease that covers no pushed ref must not change the refusal; stderr was: {err}"
    );
    assert_eq!(fx.remote_main(), published);
}

#[test]
fn every_unmatched_src_refspec_is_reported_and_the_push_is_abandoned() {
    let fx = fixture("nomatch");
    let out = fx.run(
        &fx.src,
        &["send-pack", &fx.remote_str(), "nope1", "main", "nope2"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error: src refspec nope1 does not match any"),
        "stderr was: {err}"
    );
    assert!(
        err.contains("error: src refspec nope2 does not match any"),
        "both unmatched sources are reported, not just the first; stderr was: {err}"
    );
    // `cmd_send_pack` returns -1 and `run_builtin()` masks it to 255.
    assert_eq!(out.status.code(), Some(255), "stderr was: {err}");
    // The good refspec in the middle travels no further than the bad ones.
    let out = fx.run(&fx.remote, &["rev-parse", "--verify", "refs/heads/main"]);
    assert!(
        !out.status.success(),
        "the push must be abandoned whole, but refs/heads/main landed"
    );
}

#[test]
fn the_quiet_capability_tracks_whether_progress_is_on() {
    let fx = fixture("quietcap");
    let capture = fx.root.join("cap-quiet.bin");
    let rp = teeing_receive_pack(&fx, &capture);

    // stderr is a pipe here, so `progress` is off and the sender asks the
    // receiving end not to run a meter either (send-pack.c:623-624).
    fx.ok(
        &fx.src,
        &["send-pack", &format!("--receive-pack={rp}"), &fx.remote_str(), "main"],
    );
    let caps = capabilities(&capture);
    assert!(
        caps.split(' ').any(|c| c == "quiet"),
        "quiet must be requested when progress is off; capabilities were: {caps}"
    );

    // `--progress` forces the local meter on, and then git does not ask.
    let fx = fixture("quietcap-progress");
    let capture = fx.root.join("cap-progress.bin");
    let rp = teeing_receive_pack(&fx, &capture);
    fx.ok(
        &fx.src,
        &[
            "send-pack",
            &format!("--receive-pack={rp}"),
            "--progress",
            &fx.remote_str(),
            "main",
        ],
    );
    let caps = capabilities(&capture);
    assert!(
        !caps.split(' ').any(|c| c == "quiet"),
        "quiet must not be requested under --progress; capabilities were: {caps}"
    );
}

#[test]
fn object_format_is_written_after_atomic() {
    let fx = fixture("caporder");
    let capture = fx.root.join("cap-atomic.bin");
    let rp = teeing_receive_pack(&fx, &capture);
    fx.ok(
        &fx.src,
        &[
            "send-pack",
            &format!("--receive-pack={rp}"),
            "--atomic",
            &fx.remote_str(),
            "main",
        ],
    );
    let caps = capabilities(&capture);
    let atomic = caps
        .split(' ')
        .position(|c| c == "atomic")
        .unwrap_or_else(|| panic!("--atomic did not reach the wire; capabilities were: {caps}"));
    let object_format = caps
        .split(' ')
        .position(|c| c.starts_with("object-format="))
        .unwrap_or_else(|| panic!("no object-format; capabilities were: {caps}"));
    assert!(
        atomic < object_format,
        "cap_buf is built atomic-then-object-format (send-pack.c:625-630); capabilities were: {caps}"
    );
}

#[test]
fn the_session_id_capability_follows_transfer_advertisesid() {
    // The server has to advertise `session-id` for the client to offer one back
    // (`if (!server_supports("session-id")) advertise_sid = 0;`), so the peer
    // here is the canned one: this repository's own `receive-pack` never
    // advertises it, which would make the negative case prove nothing.
    let fx = fixture("sid");
    let capture = fx.root.join("cap-nosid.bin");
    let rp = canned_receive_pack(
        &fx,
        "nosid",
        "report-status session-id object-format=sha1",
        &capture,
    );
    // Off by default: nothing names this process on the wire, however willing
    // the other end is.
    fx.run(
        &fx.src,
        &["send-pack", &format!("--receive-pack={rp}"), &fx.remote_str(), "main"],
    );
    let caps = capabilities(&capture);
    assert!(
        !caps.split(' ').any(|c| c.starts_with("session-id=")),
        "session-id must stay off until asked for; capabilities were: {caps}"
    );

    // With the config on, the sender names itself (send-pack.c:562, :579-580,
    // :633-634).
    let fx = fixture("sid-on");
    let capture = fx.root.join("cap-sid.bin");
    let rp = canned_receive_pack(
        &fx,
        "sid",
        "report-status session-id object-format=sha1",
        &capture,
    );
    fx.ok(&fx.src, &["config", "transfer.advertiseSID", "true"]);
    fx.run(
        &fx.src,
        &["send-pack", &format!("--receive-pack={rp}"), &fx.remote_str(), "main"],
    );
    let caps = capabilities(&capture);
    let sid = caps
        .split(' ')
        .find(|c| c.starts_with("session-id="))
        .unwrap_or_else(|| panic!("no session-id under transfer.advertiseSID; caps: {caps}"));
    assert!(
        sid.len() > "session-id=".len(),
        "session-id must carry this process's trace2 SID; capabilities were: {caps}"
    );
}

#[test]
fn an_atomic_push_a_server_cannot_do_reports_the_hang_up_too() {
    let fx = fixture("atomicrefuse");
    fx.ok(&fx.remote, &["config", "receive.advertiseAtomic", "false"]);
    let out = fx.run(
        &fx.src,
        &["send-pack", &fx.remote_str(), "--atomic", "main"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // `die()` inside `send_pack()` leaves the connection half-open, so the peer
    // reads EOF where the command list should have been — the same two-`fatal:`
    // shape the push-options and --signed refusals produce.
    assert!(
        err.contains("fatal: the receiving end does not support --atomic push"),
        "stderr was: {err}"
    );
    assert!(
        err.contains("fatal: the remote end hung up unexpectedly"),
        "the torn-down connection must be reported as well; stderr was: {err}"
    );
    assert_eq!(out.status.code(), Some(128), "stderr was: {err}");
}

#[test]
fn a_command_the_report_never_answers_is_a_remote_failure() {
    let fx = fixture("noreport");
    let rp = silent_report_receive_pack(&fx);
    let out = fx.run(
        &fx.src,
        &["send-pack", &format!("--receive-pack={rp}"), &fx.remote_str(), "main"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // `case REF_STATUS_EXPECTING_REPORT` has its own summary and its own message
    // (transport.c:793-798) — reporting it as `[remote rejected]` claimed the
    // server had passed judgement when the report simply ran out.
    assert!(
        err.contains("! [remote failure]"),
        "an unanswered command is [remote failure], not [rejected]; stderr was: {err}"
    );
    assert!(
        err.contains("(remote failed to report status)"),
        "stderr was: {err}"
    );
    assert_eq!(out.status.code(), Some(1), "stderr was: {err}");
}

#[test]
fn helper_status_spells_an_unanswered_command_expecting_report() {
    let fx = fixture("noreport-helper");
    let rp = silent_report_receive_pack(&fx);
    let out = fx.run(
        &fx.src,
        &[
            "send-pack",
            "--helper-status",
            &format!("--receive-pack={rp}"),
            &fx.remote_str(),
            "main",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `res = "error"; msg = "expecting report";` (builtin/send-pack.c:89-92).
    assert_eq!(
        stdout.trim(),
        "error refs/heads/main expecting report",
        "stderr was: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1));
}
