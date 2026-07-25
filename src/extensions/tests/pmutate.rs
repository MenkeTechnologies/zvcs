//! Parallel mutation verbs (`zgc`, `zcheckout`, `ztagall`, `zcommitall`,
//! `zclean`) and coordination verbs (`zqueue`, `zbarrier`, `zwait`). Builds a
//! two-repo index with known state and asserts each verb's effect and its
//! skip/guard behavior. Network verbs (`zfetch`/`zpushall`) are exercised
//! manually, not here, since they need a real remote transport.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new(BIN)
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

fn zvcs(home: &Path, sock: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN).args(args).env("ZVCS_HOME", home).env("ZVCS_SOCK", sock).output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.success())
}

#[test]
fn parallel_mutations_and_coordination() {
    let root = std::env::temp_dir().join(format!("zvcs-pmutate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = root.join("home");
    let sock = root.join("sock");

    // alpha has a `feature` branch and will be made dirty; beta has neither.
    let alpha = work.join("alpha");
    let beta = work.join("beta");
    for r in [&alpha, &beta] {
        std::fs::create_dir_all(r).unwrap();
        git(r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("f"), "hi\n").unwrap();
        git(r, &["add", "-A"]);
        git(r, &["commit", "-qm", "c1"]);
    }
    git(&alpha, &["branch", "feature"]);

    let idx = zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]);
    assert!(idx.0.contains("indexed 2"), "reindex: {}", idx.0);

    // zgc runs everywhere.
    let (gc, _) = zvcs(&home, &sock, &["zgc"]);
    assert!(gc.contains("2 ok, 0 failed"), "zgc: {gc}");

    // zcheckout: alpha has `feature`, beta doesn't → 1 ok, 1 skipped.
    let (co, _) = zvcs(&home, &sock, &["zcheckout", "feature"]);
    assert!(co.contains("1 ok") && co.contains("1 skipped"), "zcheckout: {co}");
    assert_eq!(head_branch(&alpha), "feature", "alpha moved to feature");
    assert_eq!(head_branch(&beta), "main", "beta stayed on main");

    // ztagall tags both.
    let (tag, _) = zvcs(&home, &sock, &["ztagall", "v9"]);
    assert!(tag.contains("2 ok, 0 failed"), "ztagall: {tag}");
    assert!(has_tag(&alpha, "v9") && has_tag(&beta, "v9"), "both tagged v9");

    // zcommitall: dirty alpha commits, clean beta is skipped.
    std::fs::write(alpha.join("f"), "hi\nchange\n").unwrap();
    let (ca, _) = zvcs(&home, &sock, &["zcommitall", "-m", "bulk"]);
    assert!(ca.contains("1 ok") && ca.contains("1 skipped"), "zcommitall: {ca}");

    // zclean without -f is refused; with -f it removes a non-ignored untracked file.
    std::fs::write(alpha.join("junk.txt"), "x\n").unwrap();
    let (refused, ok) = zvcs(&home, &sock, &["zclean"]);
    assert!(!ok && refused.contains("pass -f"), "zclean must require -f: {refused}");
    assert!(alpha.join("junk.txt").exists(), "junk left intact without -f");
    let (_cleaned, _) = zvcs(&home, &sock, &["zclean", "-f"]);
    assert!(!alpha.join("junk.txt").exists(), "zclean -f removes untracked");

    // Coordination: with no daemon there are no jobs, so all are immediate.
    assert!(zvcs(&home, &sock, &["zqueue"]).0.contains("queue empty"), "zqueue empty");
    assert!(zvcs(&home, &sock, &["zbarrier"]).0.contains("idle"), "zbarrier idle");
    let (wait, wok) = zvcs(&home, &sock, &["zwait", alpha.to_str().unwrap()]);
    assert!(wok && wait.contains("idle"), "zwait idle: {wait}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zreset_hard_and_zabort_cleanup() {
    let root = std::env::temp_dir().join(format!("zvcs-reset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = root.join("home");
    let sock = root.join("sock");

    // alpha: committed, then a dirty (staged) change zreset --hard will discard.
    let alpha = work.join("alpha");
    let beta = work.join("beta");
    for r in [&alpha, &beta] {
        std::fs::create_dir_all(r).unwrap();
        git(r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("f"), "hi\n").unwrap();
        git(r, &["add", "-A"]);
        git(r, &["commit", "-qm", "c1"]);
    }
    std::fs::write(alpha.join("f"), "hi\nstaged\n").unwrap();
    git(&alpha, &["add", "-A"]);

    // conf: a conflicting merge left in progress for zabort to clean up.
    let conf = work.join("conf");
    std::fs::create_dir_all(&conf).unwrap();
    git(&conf, &["init", "-q", "-b", "main"]);
    std::fs::write(conf.join("m"), "base\n").unwrap();
    git(&conf, &["add", "-A"]);
    git(&conf, &["commit", "-qm", "base"]);
    git(&conf, &["checkout", "-q", "-b", "other"]);
    std::fs::write(conf.join("m"), "other\n").unwrap();
    git(&conf, &["add", "-A"]);
    git(&conf, &["commit", "-qm", "other"]);
    git(&conf, &["checkout", "-q", "main"]);
    std::fs::write(conf.join("m"), "mainside\n").unwrap();
    git(&conf, &["add", "-A"]);
    git(&conf, &["commit", "-qm", "mainc"]);
    let _ = Command::new(BIN).args(["merge", "-q", "other"]).current_dir(&conf).status();
    assert!(conf.join(".git/MERGE_HEAD").exists(), "conf should be mid-merge");

    assert!(zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]).0.contains("indexed 3"));

    // zreset --hard scoped to alpha discards its staged change; conf is untouched.
    let (reset, rok) = zvcs(&home, &sock, &["zreset", "--repo", "alpha", "--hard"]);
    assert!(rok && reset.contains("1 ok"), "zreset ran on alpha: {reset}");
    assert!(status_clean(&alpha), "zreset --hard left alpha clean");

    // zabort aborts conf's merge (1 ok) and skips the two repos not mid-op.
    let (abort, aok) = zvcs(&home, &sock, &["zabort"]);
    assert!(aok, "zabort succeeds: {abort}");
    assert!(abort.contains("1 ok") && abort.contains("2 skipped"), "one aborted, two skipped: {abort}");
    assert!(!conf.join(".git/MERGE_HEAD").exists(), "zabort cleared conf's merge state");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zattach_reattaches_detached_head() {
    let root = std::env::temp_dir().join(format!("zvcs-attach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = root.join("home");
    let sock = root.join("sock");

    // A repo on main, committed, then deliberately detached at that commit.
    let repo = work.join("det");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "hi\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "c1"]);
    git(&repo, &["checkout", "-q", "--detach", "HEAD"]);
    assert_eq!(head_branch(&repo), "HEAD", "repo should be detached before zattach");

    assert!(zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]).0.contains("indexed 1"));
    let (out, ok) = zvcs(&home, &sock, &["zattach"]);
    assert!(ok, "zattach succeeds: {out}");
    assert!(out.contains("attached to main"), "zattach reports the attach: {out}");
    assert_eq!(head_branch(&repo), "main", "repo is re-attached to main after zattach");

    let _ = std::fs::remove_dir_all(&root);
}

fn status_clean(repo: &Path) -> bool {
    let out = Command::new(BIN).args(["status", "--porcelain"]).current_dir(repo).output().unwrap();
    out.stdout.is_empty()
}

fn head_branch(repo: &Path) -> String {
    let out = Command::new(BIN).args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(repo).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn has_tag(repo: &Path, tag: &str) -> bool {
    let out = Command::new(BIN).args(["tag", "--list", tag]).current_dir(repo).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim() == tag
}
