//! With NO daemon reachable, concurrent zvcs writers must not lose each other's
//! index writes — and must never exit 0 on a write that did not land.
//!
//! The failure this pins down is a read-modify-write race, not a write race.
//! Stock git holds `.git/index.lock` from BEFORE it reads the index
//! (`repo_hold_locked_index(repo, &lock_file, LOCK_DIE_ON_ERROR)` precedes
//! `repo_read_index_preload()` in `builtin/add.c`), so a second writer cannot
//! even read a copy it will later write back. The port's only lock is taken by
//! `gix_index::File::write()` at WRITE time, so two writers happily read the same
//! base index, and the second one's write erases the first one's entry. Both
//! exit 0. Nothing is queued, nothing is reported, the entry is simply gone.
//!
//! Shape: N children, each `git add` on its OWN path, released together from a
//! stdin barrier so their read-modify-write windows overlap. Then the invariant:
//! **the number of children that exited 0 equals the number of paths in the
//! index**. A writer may legitimately fail (and say so); it may not succeed
//! silently and lose the write.
//!
//! `ZVCS_SOCK` points at a path that cannot exist, so the no-daemon fallback is
//! exercised deterministically rather than depending on whether a coordinator
//! happens to be running. Everything else is the port's own binary — no stock
//! git, no network, no daemon — so this runs headless in CI.

use std::io::Write;
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// How many writers race per round.
const WRITERS: usize = 8;
/// How many rounds. One round already loses a write nearly every time; several
/// make a pass mean something on a machine that happened to schedule one round
/// serially.
const ROUNDS: usize = 3;

/// The port's own binary, wired so nothing in the developer's environment (a
/// live coordinator, `~/.gitconfig`, the shared ledger) can reach the test.
fn wired(program: &str, repo: &std::path::Path, sock: &std::path::Path, home: &std::path::Path) -> Command {
    let mut c = Command::new(program);
    c.current_dir(repo)
        .env("ZVCS_SOCK", sock)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C");
    c
}

#[test]
fn concurrent_adds_without_a_daemon_never_report_a_write_they_dropped() {
    let root = std::env::temp_dir().join(format!("zvcs-lane-nodaemon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir root");
    let root = root.canonicalize().expect("canonicalize root");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    // A socket path that cannot be created, let alone connected to: every child
    // takes the no-daemon fallback, on every platform.
    let sock = root.join("no-such-dir").join("no.sock");
    let home = root.join("home");

    assert!(
        wired(BIN, &repo, &sock, &home).args(["init", "-q", "-b", "main", "."]).status().expect("run init").success(),
        "git init failed"
    );

    let mut failures = Vec::new();
    for round in 0..ROUNDS {
        for i in 0..WRITERS {
            std::fs::write(repo.join(format!("r{round}_f{i}.txt")), format!("round {round} writer {i}\n"))
                .expect("write payload");
        }

        // Each child blocks in `read` until we feed its stdin, so all N are
        // already loaded and parsed when the race starts. A plain spawn loop
        // would let process startup serialize them and hide the defect.
        let mut kids: Vec<Child> = (0..WRITERS)
            .map(|i| {
                wired("sh", &repo, &sock, &home)
                    .args(["-c", r#"read gate; exec "$0" add "$1""#, BIN, &format!("r{round}_f{i}.txt")])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("spawn writer")
            })
            .collect();

        for k in &mut kids {
            let _ = k.stdin.take().expect("child stdin").write_all(b"go\n");
        }

        let mut exit0 = 0usize;
        let mut said = Vec::new();
        for k in kids {
            let out = k.wait_with_output().expect("wait writer");
            if out.status.success() {
                exit0 += 1;
            }
            said.push(format!(
                "  rc={:?} out={:?} err={:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }

        let listed = wired(BIN, &repo, &sock, &home).args(["ls-files"]).output().expect("run ls-files");
        assert!(listed.status.success(), "ls-files failed: {}", String::from_utf8_lossy(&listed.stderr));
        let index = String::from_utf8_lossy(&listed.stdout).into_owned();
        let landed = index.lines().filter(|l| l.starts_with(&format!("r{round}_f"))).count();

        if exit0 != landed || landed != WRITERS {
            failures.push(format!(
                "round {round}: {exit0} writers exited 0, {landed}/{WRITERS} landed in the index\n{}",
                said.join("\n")
            ));
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "concurrent `git add` with no daemon dropped writes it had reported as successful:\n{}",
        failures.join("\n")
    );
}
