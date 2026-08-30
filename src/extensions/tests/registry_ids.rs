//! Ids handed out by the `$ZVCS_HOME` registries — `zguard`, `zintercept`,
//! `zsched` — must not be handed out twice.
//!
//! Each of these verbs is driven by id: `zguard rm <id>`, `zintercept remove
//! <id>`, `zsched rm <id>`. Numbering a new entry `max(existing) + 1` reuses an
//! id the moment the entry holding the highest one is removed. Measured before
//! the fix, in all three: with entries #1 and #2, removing #2 and adding another
//! produced a *new* #2. A script that recorded "rule #2 blocks force-pushes"
//! then removes, or trusts, something else entirely — and nothing in the output
//! says the meaning changed.
//!
//! The check is the same shape for each verb: note what #2 is, remove it, add a
//! different entry, and require the new one to be numbered past the id that was
//! retired.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

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

/// Every id the listing shows, in order — ANSI stripped, since `zsched` colours
/// its ids.
fn ids(listing: &str) -> Vec<u64> {
    let plain: String = {
        let mut out = String::with_capacity(listing.len());
        let mut chars = listing.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    };
    plain
        .split_whitespace()
        .filter_map(|t| t.trim_start_matches('#').parse::<u64>().ok())
        .collect()
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-regids-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&home, &repo, &["init", "-q", "-b", "main"]);
    ok(&home, &repo, &["config", "user.email", "t@example"]);
    ok(&home, &repo, &["config", "user.name", "T"]);
    ok(&home, &repo, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    (root, home)
}

#[test]
fn zguard_does_not_reuse_the_id_of_a_removed_rule() {
    let (root, home) = fixture("guard");
    let repo = root.join("repo");

    ok(&home, &repo, &["zguard", "deny", "aaa*"]);
    ok(&home, &repo, &["zguard", "deny", "bbb*"]);
    assert_eq!(ids(&ok(&home, &repo, &["zguard", "list"])), vec![1, 2]);

    // #2 is the highest, which is the case that used to recycle.
    ok(&home, &repo, &["zguard", "rm", "2"]);
    ok(&home, &repo, &["zguard", "deny", "ccc*"]);

    let listing = ok(&home, &repo, &["zguard", "list"]);
    assert!(listing.contains("ccc*"), "the new rule is missing:\n{listing}");
    assert!(
        !ids(&listing).contains(&2),
        "the id of the removed rule came back, so `zguard rm 2` now removes a different rule:\n{listing}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zintercept_does_not_reuse_the_id_of_a_removed_hook() {
    let (root, home) = fixture("intercept");
    let repo = root.join("repo");

    ok(&home, &repo, &["zintercept", "before", "aaa", "true"]);
    ok(&home, &repo, &["zintercept", "before", "bbb", "true"]);
    ok(&home, &repo, &["zintercept", "remove", "2"]);
    ok(&home, &repo, &["zintercept", "before", "ccc", "true"]);

    let listing = ok(&home, &repo, &["zintercept", "list"]);
    assert!(listing.contains("ccc"), "the new hook is missing:\n{listing}");
    assert!(
        !ids(&listing).contains(&2),
        "the id of the removed hook came back — `zintercept remove 2` now removes the wrong advice:\n{listing}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zsched_does_not_reuse_the_id_of_a_removed_schedule() {
    let (root, home) = fixture("sched");
    let repo = root.join("repo");

    ok(&home, &repo, &["zsched", "add", "60", "status"]);
    ok(&home, &repo, &["zsched", "add", "90", "fetch"]);
    ok(&home, &repo, &["zsched", "rm", "2"]);
    ok(&home, &repo, &["zsched", "add", "120", "gc"]);

    let listing = ok(&home, &repo, &["zsched", "list"]);
    assert!(listing.contains("gc"), "the new schedule is missing:\n{listing}");
    assert!(
        !ids(&listing).contains(&2),
        "the id of the removed schedule came back:\n{listing}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_emptied_registry_does_not_restart_its_numbering() {
    // The sharpest form: clear everything, then add. Starting again from #1
    // would make the ids of the old rules mean the new ones.
    let (root, home) = fixture("emptied");
    let repo = root.join("repo");

    ok(&home, &repo, &["zguard", "deny", "aaa*"]);
    ok(&home, &repo, &["zguard", "deny", "bbb*"]);
    ok(&home, &repo, &["zguard", "clear"]);
    ok(&home, &repo, &["zguard", "deny", "ccc*"]);

    let listing = ok(&home, &repo, &["zguard", "list"]);
    let ids = ids(&listing);
    assert_eq!(ids.len(), 1, "exactly one rule should remain:\n{listing}");
    assert!(ids[0] > 2, "numbering restarted after a clear (#{}):\n{listing}", ids[0]);

    let _ = std::fs::remove_dir_all(&root);
}
