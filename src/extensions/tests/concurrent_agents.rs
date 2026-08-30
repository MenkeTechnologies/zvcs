//! The two pieces of shared state that many agents touch at the same time: the
//! per-repo lease (`zclaim`) and the append-only event feed.
//!
//! Both are sequentially covered elsewhere — `claim.rs` claims in one session
//! and is refused in another, `event_feed.rs` reads a feed one writer produced.
//! Neither exercises the case this tool is built for: sixteen agents acting at
//! once. A lease granted twice is worse than no lease, because the second holder
//! believes it is alone; a feed that drops writes is an audit trail with holes.
//!
//! Both hold today, and these cases keep it that way. They are written to fail
//! loudly rather than flakily: the assertions are exact counts, and the fixtures
//! are per-test.
//!
//! What this file proves is the observable contract — one winner, no lost
//! events — not which mechanism produced it. `claim()` checks the holder and
//! then inserts, and in practice the check catches every contender here: making
//! the insert an `INSERT OR REPLACE`, which would hand a held lease to a second
//! agent in the window between check and insert, does not fail these cases. The
//! primary key that closes that window is pinned where it is deterministic, in
//! `db::snapshot_atomic_tests::a_second_claim_on_one_repo_cannot_be_inserted`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const AGENTS: usize = 12;

fn run(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn ok(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = run(home, dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One repo per name, each with a commit, all indexed.
fn fixture(tag: &str, repos: &[String]) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-agents-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    for name in repos {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        ok(&home, &r, &["init", "-q", "-b", "main"]);
        ok(&home, &r, &["config", "user.email", "t@example"]);
        ok(&home, &r, &["config", "user.name", "T"]);
        std::fs::write(r.join("f.txt"), b"v\n").unwrap();
        ok(&home, &r, &["add", "f.txt"]);
        ok(&home, &r, &["commit", "-q", "-m", "c0"]);
    }
    ok(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    (root, home)
}

/// Start one command per agent and collect their exit codes, so the race is
/// between real processes rather than threads sharing one address space.
fn race(home: &Path, dirs: &[PathBuf], args_for: impl Fn(usize) -> Vec<String>) -> Vec<Output> {
    let kids: Vec<Child> = (0..dirs.len())
        .map(|i| {
            Command::new(BIN)
                .args(args_for(i))
                .current_dir(&dirs[i])
                .env("HOME", home)
                .env("ZVCS_HOME", home)
                .env("ZVCS_SESSION", format!("agent{i}"))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn agent")
        })
        .collect();
    kids.into_iter().map(|k| k.wait_with_output().expect("agent exited")).collect()
}

#[test]
fn exactly_one_agent_wins_a_contested_claim() {
    let (root, home) = fixture("claim", &["shared".to_string()]);
    let repo = root.join("shared");

    // Every agent claims the same repository at the same moment.
    let dirs = vec![repo.clone(); AGENTS];
    let outs = race(&home, &dirs, |_| vec!["zclaim".into()]);

    let winners = outs.iter().filter(|o| o.status.success()).count();
    assert_eq!(winners, 1, "a lease was granted {winners} times — every holder believes it is alone");

    // The ledger agrees with the exit codes: one holder, and the refusals name
    // it rather than failing anonymously.
    let who = ok(&home, &root, &["zwho"]);
    assert_eq!(who.lines().filter(|l| !l.trim().is_empty()).count(), 1, "zwho disagrees with the winner count:\n{who}");
    let holder = who.split_whitespace().next().unwrap_or_default().to_string();
    for o in outs.iter().filter(|o| !o.status.success()) {
        let msg = String::from_utf8_lossy(&o.stderr);
        assert!(msg.contains(&holder), "a refusal did not name the holder ({holder}):\n{msg}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn claims_on_different_repositories_do_not_contend() {
    // The lease is per repository. If it serialised on anything wider, agents
    // working different repos would refuse each other — the failure that would
    // make the whole scheme useless at the scale it is for.
    let names: Vec<String> = (0..AGENTS).map(|i| format!("r{i}")).collect();
    let (root, home) = fixture("spread", &names);
    let dirs: Vec<PathBuf> = names.iter().map(|n| root.join(n)).collect();

    let outs = race(&home, &dirs, |_| vec!["zclaim".into()]);
    let winners = outs.iter().filter(|o| o.status.success()).count();
    assert_eq!(winners, AGENTS, "claims on distinct repositories refused each other");

    let who = ok(&home, &root, &["zwho"]);
    assert_eq!(
        who.lines().filter(|l| !l.trim().is_empty()).count(),
        AGENTS,
        "zwho lost a lease:\n{who}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_event_feed_keeps_every_concurrent_write() {
    // `git add` records a `stage` event. Twelve agents staging in twelve
    // repositories at once must produce twelve more events, not "some": the feed
    // is what `zevents`, `zsince` and `zaudit` answer from.
    let names: Vec<String> = (0..AGENTS).map(|i| format!("r{i}")).collect();
    let (root, home) = fixture("feed", &names);
    let dirs: Vec<PathBuf> = names.iter().map(|n| root.join(n)).collect();

    let before = ok(&home, &root, &["zevents", "--no-follow", "--json", "-n", "500"])
        .lines()
        .filter(|l| l.contains("\"kind\":\"stage\""))
        .count();

    for (i, d) in dirs.iter().enumerate() {
        std::fs::write(d.join("f.txt"), format!("change{i}\n")).unwrap();
    }
    let outs = race(&home, &dirs, |_| vec!["add".into(), "f.txt".into()]);
    for (i, o) in outs.iter().enumerate() {
        assert!(o.status.success(), "agent {i} failed to stage: {}", String::from_utf8_lossy(&o.stderr));
    }

    let after = ok(&home, &root, &["zevents", "--no-follow", "--json", "-n", "500"]);
    let staged = after.lines().filter(|l| l.contains("\"kind\":\"stage\"")).count();
    assert_eq!(
        staged,
        before + AGENTS,
        "the feed lost a concurrent write ({} new, expected {AGENTS})",
        staged - before
    );

    // And each repository is represented, so the losses are not hidden by one
    // repo writing twice.
    for name in &names {
        assert!(after.contains(&format!("/{name}\"")), "no event from {name}:\n{after}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
