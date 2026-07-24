//! Parallel query verbs over the indexed repo set (`zheads`, `zdirty`,
//! `zbranches`, `ztags`, `zsize`). These build a small ledger of two repos with
//! known state and assert each verb's aggregated output, including that the
//! `zforeach` selectors narrow the set.

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

fn zvcs(home: &Path, sock: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .env("ZVCS_HOME", home)
        .env("ZVCS_SOCK", sock)
        .output()
        .unwrap();
    // Merge stdout+stderr: some verbs print their summary to stderr.
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

#[test]
fn parallel_query_verbs_report_repo_state() {
    let root = std::env::temp_dir().join(format!("zvcs-query-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = root.join("home");
    let sock = root.join("sock");

    for name in ["alpha", "beta"] {
        let repo = work.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f.txt"), "hi\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        git(&repo, &["tag", "v1"]);
    }
    // alpha gets a tracked change → dirty; beta stays clean.
    std::fs::write(work.join("alpha/f.txt"), "hi\nmore\n").unwrap();

    // Index the two repos.
    let idx = zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]);
    assert!(idx.contains("indexed 2"), "reindex should find both repos: {idx}");

    // zheads names both repos, each on `main`.
    let heads = zvcs(&home, &sock, &["zheads"]);
    assert!(heads.contains("alpha") && heads.contains("beta"), "zheads lists both:\n{heads}");
    assert_eq!(heads.matches("main").count(), 2, "both are on main:\n{heads}");

    // zdirty lists exactly alpha.
    let dirty = zvcs(&home, &sock, &["zdirty"]);
    assert!(dirty.contains("alpha"), "alpha is dirty:\n{dirty}");
    assert!(!dirty.lines().any(|l| l.ends_with("beta")), "beta is clean:\n{dirty}");
    assert!(dirty.contains("1 dirty of 2"), "summary counts one dirty:\n{dirty}");

    // ztags reports one tag each.
    let tags = zvcs(&home, &sock, &["ztags"]);
    assert_eq!(tags.matches("1 tag(s)").count(), 2, "both have one tag:\n{tags}");

    // zbranches shows the main branch.
    let branches = zvcs(&home, &sock, &["zbranches"]);
    assert!(branches.contains("main"), "branches include main:\n{branches}");

    // A selector narrows the set: only beta.
    let beta_only = zvcs(&home, &sock, &["zheads", "--repo", "beta"]);
    assert!(beta_only.contains("beta") && !beta_only.contains("alpha"), "selector limits to beta:\n{beta_only}");

    // zsize prints a total line.
    let size = zvcs(&home, &sock, &["zsize"]);
    assert!(size.contains("total across 2 repos"), "zsize totals both:\n{size}");

    let _ = std::fs::remove_dir_all(&root);
}
