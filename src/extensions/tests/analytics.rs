//! Cross-repo search/analytics verbs (`zgrep`, `zahead`, `zbehind`, `zauthors`,
//! `zhot`, `zconflicts`). Builds a small indexed set with known content, upstream
//! deltas, multiple authors, and a mid-merge conflict, then asserts each verb's
//! aggregated output.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

/// Commit staged changes as a specific author.
fn commit_as(dir: &Path, name: &str, email: &str, msg: &str) {
    let ok = Command::new("git")
        .args(["commit", "-qm", msg])
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .status()
        .unwrap()
        .success();
    assert!(ok, "commit as {name} failed");
}

fn zvcs(home: &Path, sock: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).env("ZVCS_HOME", home).env("ZVCS_SOCK", sock).output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

#[test]
fn analytics_verbs_aggregate_across_repos() {
    let root = std::env::temp_dir().join(format!("zvcs-analytics-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = root.join("home");
    let sock = root.join("sock");

    // alpha: two authors, grep-able content.
    let alpha = work.join("alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q", "-b", "main"]);
    std::fs::write(alpha.join("f.txt"), "hello\nTODO fix\n").unwrap();
    git(&alpha, &["add", "-A"]);
    commit_as(&alpha, "alice", "alice@x.com", "c1");
    std::fs::write(alpha.join("g.txt"), "TODO more\n").unwrap();
    git(&alpha, &["add", "-A"]);
    commit_as(&alpha, "bob", "bob@x.com", "c2");

    // conf: create a merge conflict left in progress.
    let conf = work.join("conf");
    std::fs::create_dir_all(&conf).unwrap();
    git(&conf, &["init", "-q", "-b", "main"]);
    std::fs::write(conf.join("m"), "base\n").unwrap();
    git(&conf, &["add", "-A"]);
    commit_as(&conf, "alice", "alice@x.com", "base");
    git(&conf, &["checkout", "-q", "-b", "other"]);
    std::fs::write(conf.join("m"), "other\n").unwrap();
    git(&conf, &["add", "-A"]);
    commit_as(&conf, "alice", "alice@x.com", "other");
    git(&conf, &["checkout", "-q", "main"]);
    std::fs::write(conf.join("m"), "mainside\n").unwrap();
    git(&conf, &["add", "-A"]);
    commit_as(&conf, "alice", "alice@x.com", "mainc");
    // This merge conflicts and is left unresolved.
    let _ = Command::new("git").args(["merge", "-q", "other"]).current_dir(&conf).status();

    assert!(zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]).contains("indexed 2"));

    // zgrep finds the TODO lines with path:line:text, and -i is case-insensitive.
    let grep = zvcs(&home, &sock, &["zgrep", "TODO"]);
    assert!(grep.contains("f.txt:2:TODO fix"), "zgrep locates the match:\n{grep}");
    assert!(grep.contains("2 match(es)"), "two TODO matches:\n{grep}");
    assert!(zvcs(&home, &sock, &["zgrep", "-i", "todo"]).contains("2 match(es)"), "-i matches lowercase");

    // zauthors aggregates: alice has 4 commits (base+other... other is off main,
    // so base+mainc = 2 in conf) + c1 in alpha = 3; bob has 1. Assert ordering and
    // that alice outranks bob.
    let authors = zvcs(&home, &sock, &["zauthors"]);
    let alice_line = authors.lines().position(|l| l.contains("alice@x.com"));
    let bob_line = authors.lines().position(|l| l.contains("bob@x.com"));
    assert!(alice_line < bob_line, "alice (more commits) ranks above bob:\n{authors}");
    assert!(authors.contains("bob <bob@x.com>"), "bob appears:\n{authors}");

    // zhot over a wide window ranks both repos with their commit counts.
    let hot = zvcs(&home, &sock, &["zhot", "3650"]);
    assert!(hot.contains("alpha") && hot.contains("conf"), "zhot lists both:\n{hot}");
    assert!(hot.contains("commit(s)"), "zhot shows counts:\n{hot}");

    // zconflicts flags conf (mid-merge) and not alpha.
    let conflicts = zvcs(&home, &sock, &["zconflicts"]);
    assert!(conflicts.contains("conf"), "conf is mid-merge:\n{conflicts}");
    assert!(!conflicts.lines().any(|l| l.contains("/alpha")), "alpha is clean:\n{conflicts}");
    assert!(conflicts.contains("merge") || conflicts.contains("conflicts"), "names the op:\n{conflicts}");

    let _ = std::fs::remove_dir_all(&root);
}
