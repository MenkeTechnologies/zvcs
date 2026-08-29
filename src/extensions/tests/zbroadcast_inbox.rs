//! `git zbroadcast` / `git zhandoff` — inter-agent messaging over the shared db.
//!
//! The inbox is a *pull*: nothing is delivered on the hot path, and reading is
//! what marks a message read. That design has one property everything else
//! depends on, and it is invisible from a single agent's point of view —
//! **each message is delivered once per agent, not once in total**. A read
//! marker stored on the message rather than on the (message, session) pair
//! behaves identically for whoever reads first and silently swallows the
//! message for everybody else, which in a fleet of agents is the failure that
//! matters.
//!
//! So these cases run several sessions against one database, by varying
//! `ZVCS_SESSION` — the value `session_key()` reads — and assert what each one
//! sees. Everything is hermetic: `ZVCS_HOME` is the fixture's, so the database
//! is its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run as a named agent.
fn as_session(dir: &Path, home: &Path, session: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("ZVCS_SOCK", home.join("sock"))
        .env("ZVCS_SESSION", session)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zbcast-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&as_session(&repo, &home, "setup", &["init", "-q", "-b", "main", "."]), "init");
    std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
    ok(&as_session(&repo, &home, "setup", &["add", "a.txt"]), "add");
    ok(&as_session(&repo, &home, "setup", &["commit", "-q", "-m", "first"]), "commit");
    (repo, home)
}

#[test]
fn a_broadcast_reaches_every_other_agent_once_each() {
    // The property a per-message read marker would break: two agents each read
    // the same broadcast, and neither steals it from the other.
    let (repo, home) = fixture("fanout");
    ok(&as_session(&repo, &home, "alice", &["zbroadcast", "deploy", "starting"]), "post");

    let bob = ok(&as_session(&repo, &home, "bob", &["zbroadcast"]), "bob reads");
    assert!(bob.contains("deploy starting"), "bob did not receive the broadcast:\n{bob}");

    let carol = ok(&as_session(&repo, &home, "carol", &["zbroadcast"]), "carol reads");
    assert!(
        carol.contains("deploy starting"),
        "carol lost the message because bob read it first — the read marker is not per-agent:\n{carol}"
    );

    // And each of them has now read it: a second look is empty.
    let bob_again = ok(&as_session(&repo, &home, "bob", &["zbroadcast"]), "bob re-reads");
    assert!(bob_again.contains("no unread"), "the message was delivered twice to bob:\n{bob_again}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_sender_does_not_receive_their_own_broadcast() {
    let (repo, home) = fixture("self");
    ok(&as_session(&repo, &home, "alice", &["zbroadcast", "note", "to", "the", "fleet"]), "post");
    let mine = ok(&as_session(&repo, &home, "alice", &["zbroadcast"]), "alice reads");
    assert!(mine.contains("no unread"), "the sender received their own message:\n{mine}");

    // …while somebody else does.
    let bob = ok(&as_session(&repo, &home, "bob", &["zbroadcast"]), "bob reads");
    assert!(bob.contains("note to the fleet"), "{bob}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_directed_message_reaches_only_its_addressee() {
    let (repo, home) = fixture("directed");
    let sent = ok(
        &as_session(&repo, &home, "alice", &["zbroadcast", "--to", "bob", "just", "for", "you"]),
        "post directed",
    );
    assert!(sent.contains("sent to bob"), "{sent}");

    let carol = ok(&as_session(&repo, &home, "carol", &["zbroadcast"]), "carol reads");
    assert!(carol.contains("no unread"), "a directed message leaked to another agent:\n{carol}");

    let bob = ok(&as_session(&repo, &home, "bob", &["zbroadcast"]), "bob reads");
    assert!(bob.contains("just for you"), "the addressee did not receive it:\n{bob}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn zhandoff_moves_the_claim_and_tells_the_receiver() {
    let (repo, home) = fixture("handoff");
    // Nothing to hand off until somebody holds the repo.
    let unclaimed = as_session(&repo, &home, "alice", &["zhandoff", ".", "bob"]);
    assert!(!unclaimed.status.success(), "handing off an unclaimed repo succeeded");

    ok(&as_session(&repo, &home, "alice", &["zclaim"]), "alice claims");
    let who = ok(&as_session(&repo, &home, "alice", &["zwho"]), "zwho");
    assert!(who.contains("alice"), "the claim is not recorded:\n{who}");

    ok(&as_session(&repo, &home, "alice", &["zhandoff", ".", "bob"]), "handoff");
    let who = ok(&as_session(&repo, &home, "alice", &["zwho"]), "zwho after handoff");
    assert!(who.contains("bob"), "the claim did not move:\n{who}");
    assert!(!who.contains("alice"), "the previous holder still holds it:\n{who}");

    // The receiver is told, through the same inbox.
    let bob = ok(&as_session(&repo, &home, "bob", &["zbroadcast"]), "bob reads");
    assert!(bob.contains("handed to you"), "the receiver was not notified:\n{bob}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
