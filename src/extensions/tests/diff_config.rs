//! `git diff` honors its config-provided defaults — `diff.context`,
//! `diff.noPrefix`, `diff.srcPrefix`/`diff.dstPrefix`, `diff.algorithm` — with the
//! CLI flags still overriding. Regression guard for these being hardcoded
//! (context=3, `a/`/`b/`, myers).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with a 9-line file committed, then one middle line changed in the
/// worktree — enough to show context and prefixes.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-diffcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("f"), "l1\nl2\nl3\nl4\nCHANGED\nl6\nl7\nl8\nl9\n").unwrap();
    (repo, home)
}

fn diff(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["diff"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--", "f"]);
    Command::new(BIN)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}


#[test]
fn diff_context_default_and_override() {
    let (repo, home) = fixture("context");
    git(&repo, &["config", "diff.context", "1"]);
    let ctx1 = out(&diff(&repo, &home, &[]));
    // With 1 context line, l3 and l7 are outside the window.
    assert!(ctx1.contains("\n l4\n"), "l4 in context:\n{ctx1}");
    assert!(!ctx1.contains("\n l3\n"), "l3 must be outside 1-line context:\n{ctx1}");

    // -U3 overrides the config back to 3 context lines, so l3 reappears.
    let ctx3 = out(&diff(&repo, &home, &["-U3"]));
    assert!(ctx3.contains("\n l3\n"), "-U3 must override diff.context:\n{ctx3}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn diff_no_prefix_config() {
    let (repo, home) = fixture("noprefix");
    git(&repo, &["config", "diff.noPrefix", "true"]);
    let d = out(&diff(&repo, &home, &[]));
    assert!(d.contains("\n--- f\n"), "no a/ prefix:\n{d}");
    assert!(d.contains("\n+++ f\n"), "no b/ prefix:\n{d}");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn diff_algorithm_default_matches_flag_and_is_overridden() {
    let (repo, home) = fixture("algo");
    // Each valid `diff.algorithm` default must render exactly like the matching
    // `--diff-algorithm=` flag: the config routes through the same selection.
    for algo in ["myers", "minimal", "histogram", "default"] {
        git(&repo, &["config", "diff.algorithm", algo]);
        let cfg = out(&diff(&repo, &home, &[]));
        let flag_name = if algo == "default" { "myers" } else { algo };
        let flag = out(&diff(&repo, &home, &[&format!("--diff-algorithm={flag_name}")]));
        assert_eq!(cfg, flag, "diff.algorithm={algo} must equal --diff-algorithm={flag_name}");
        assert!(cfg.contains("@@ "), "diff.algorithm={algo} must still emit a patch:\n{cfg}");
    }

    // A `--diff-algorithm=` flag overrides the config default (flag wins).
    git(&repo, &["config", "diff.algorithm", "histogram"]);
    let overridden = out(&diff(&repo, &home, &["--diff-algorithm=myers"]));
    git(&repo, &["config", "--unset", "diff.algorithm"]);
    let plain_myers = out(&diff(&repo, &home, &[]));
    assert_eq!(
        overridden, plain_myers,
        "--diff-algorithm=myers must override diff.algorithm=histogram"
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn diff_algorithm_invalid_value_errors() {
    let (repo, home) = fixture("algobad");
    git(&repo, &["config", "diff.algorithm", "bogus"]);
    let o = diff(&repo, &home, &[]);
    assert!(!o.status.success(), "invalid diff.algorithm must fail");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("diff algorithm \"bogus\" is not available"),
        "stderr: {err}"
    );
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn diff_algorithm_patience_bails_but_flag_overrides() {
    let (repo, home) = fixture("algopat");
    git(&repo, &["config", "diff.algorithm", "patience"]);

    // `patience` is git-valid but has no imara-diff equivalent, so an unqualified
    // run bails rather than faking myers/histogram output. This also proves the
    // config is actually consumed by the algorithm selection.
    let o = diff(&repo, &home, &[]);
    assert!(!o.status.success(), "patience default must bail");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("diff algorithm \"patience\" is not available"),
        "stderr: {err}"
    );

    // An overriding `--diff-algorithm=` flag is honored, matching git's precedence
    // (git renders patience; a flag makes both agree).
    let ok = diff(&repo, &home, &["--diff-algorithm=histogram"]);
    assert!(ok.status.success(), "flag must override patience config");
    assert!(out(&ok).contains("@@ "), "override must emit a patch");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn diff_custom_src_dst_prefix_config() {
    let (repo, home) = fixture("prefixes");
    git(&repo, &["config", "diff.srcPrefix", "OLD/"]);
    git(&repo, &["config", "diff.dstPrefix", "NEW/"]);
    let d = out(&diff(&repo, &home, &[]));
    assert!(d.contains("\n--- OLD/f\n"), "custom src prefix:\n{d}");
    assert!(d.contains("\n+++ NEW/f\n"), "custom dst prefix:\n{d}");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
