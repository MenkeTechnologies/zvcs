//! `diff.mnemonicPrefix` — the per-comparison `a/`/`b/` replacements.
//!
//! With the key on, `diff_setup()` leaves `options->a_prefix`/`b_prefix` NULL and
//! whichever comparison the command runs names them (diff.c:5149-5153 plus the
//! `diff_set_mnemonic_prefix()` calls in diff-lib.c and diff-no-index.c), so the
//! header says *what* is being compared:
//!
//! | comparison                         | prefixes |
//! |------------------------------------|----------|
//! | `git diff` (index vs worktree)     | `i/` `w/` |
//! | `git diff --cached` (commit vs index) | `c/` `i/` |
//! | `git diff <commit>` (commit vs worktree) | `c/` `w/` |
//! | `git diff <commit> <commit>` (tree vs tree) | `a/` `b/` |
//! | `git diff --no-index`              | `1/` `2/` |
//!
//! Every expectation below is bytes read back from stock git 2.55.0 on this
//! fixture. The three overrides — `--src-prefix`/`--dst-prefix` per side,
//! `--no-prefix`/`diff.noPrefix` for both, `--default-prefix` — all beat the
//! mnemonic fill because they *assign* the slot, and `diff.srcPrefix`/
//! `diff.dstPrefix` are silently ignored while the key is on, because
//! `diff_set_default_prefix()` is the function that would have read them and
//! `diff_setup()` skips it. The plumbing verbs read `git_diff_basic_config()` and
//! so never see the key at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const DATE: &str = "1136214245 +0000";

fn cmd(dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.com")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.com")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE);
    c
}

/// Two commits touching `f`, then a worktree edit to `f` and a *staged* new file
/// `s` — so each of the four comparisons has something of its own to show:
/// index-vs-worktree sees `f`, commit-vs-index sees `s`, commit-vs-worktree sees
/// both, and commit-vs-commit sees `f`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-mnemonic-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    let run = |args: &[&str]| {
        let o = cmd(&repo, &home, args).output().unwrap();
        assert!(
            o.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "alice@example.com"]);
    run(&["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "one\n").unwrap();
    run(&["add", "f"]);
    run(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("f"), "one\ntwo\n").unwrap();
    run(&["add", "f"]);
    run(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c1"]);
    // Unstaged edit + a staged addition.
    std::fs::write(repo.join("f"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join("s"), "staged\n").unwrap();
    run(&["add", "s"]);
    (repo, home)
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    cmd(repo, home, args).output().unwrap()
}

/// Every `diff --git`, `---` and `+++` line of the output, which is where a
/// prefix is visible. `/dev/null` stays as-is: it never takes a prefix.
fn prefix_lines(o: &Output) -> Vec<String> {
    assert!(
        o.status.success() || o.status.code() == Some(1),
        "diff failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| l.starts_with("diff --git ") || l.starts_with("--- ") || l.starts_with("+++ "))
        .map(str::to_owned)
        .collect()
}

fn cleanup(repo: &Path) {
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git -c diff.mnemonicPrefix=true <args>`.
fn mnemonic(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut a = vec!["-c", "diff.mnemonicPrefix=true"];
    a.extend_from_slice(args);
    run(repo, home, &a)
}

#[test]
fn each_comparison_names_its_own_two_ends() {
    let (repo, home) = fixture("kinds");

    // `git diff` is `run_diff_files()`: the index against the worktree.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--", "f"])),
        ["diff --git i/f w/f", "--- i/f", "+++ w/f"]
    );

    // `git diff --cached` is `run_diff_index()` with `cached`: HEAD against the
    // index. `s` is an addition, so its pre-image is `/dev/null`.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--cached", "--", "s"])),
        ["diff --git c/s i/s", "--- /dev/null", "+++ i/s"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--staged", "--", "s"])),
        ["diff --git c/s i/s", "--- /dev/null", "+++ i/s"],
        "--staged is the same comparison as --cached"
    );

    // `git diff <commit>` is `run_diff_index()` without `cached`: HEAD against the
    // worktree.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "HEAD", "--", "f"])),
        ["diff --git c/f w/f", "--- c/f", "+++ w/f"]
    );

    // Two trees never reach a `diff_set_mnemonic_prefix()` call, so
    // `builtin_diff()`'s own `a/`/`b/` stands — for the two-dot, the range and the
    // three-dot spellings alike.
    for spec in [
        vec!["diff", "HEAD~1", "HEAD"],
        vec!["diff", "HEAD~1..HEAD"],
        vec!["diff", "HEAD~1...HEAD"],
    ] {
        assert_eq!(
            prefix_lines(&mnemonic(&repo, &home, &spec)),
            ["diff --git a/f b/f", "--- a/f", "+++ b/f"],
            "{spec:?} compares two trees"
        );
    }
    cleanup(&repo);
}

#[test]
fn no_index_uses_the_numbered_prefixes() {
    let (repo, home) = fixture("noindex");
    // `builtin_diff_no_index()` names the two operands `1/` and `2/`
    // (diff-no-index.c:425); the two sides may have different names.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--no-index", "f", "s"])),
        ["diff --git 1/f 2/s", "--- 1/f", "+++ 2/s"]
    );
    // Without the key it is the ordinary `a/`/`b/` pair.
    assert_eq!(
        prefix_lines(&run(&repo, &home, &["diff", "--no-index", "f", "s"])),
        ["diff --git a/f b/s", "--- a/f", "+++ b/s"]
    );
    cleanup(&repo);
}

#[test]
fn reverse_exchanges_whichever_prefixes_were_chosen() {
    let (repo, home) = fixture("reverse");
    // `builtin_diff()` swaps the two prefixes under `-R` (diff.c:3839-3845), so
    // the exchange is of the mnemonic pair rather than of a fixed `a/`/`b/`.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "-R", "--", "f"])),
        ["diff --git w/f i/f", "--- w/f", "+++ i/f"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "-R", "HEAD", "--", "f"])),
        ["diff --git w/f c/f", "--- w/f", "+++ c/f"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--cached", "-R", "--", "s"])),
        ["diff --git i/s c/s", "--- i/s", "+++ /dev/null"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--no-index", "-R", "f", "s"])),
        ["diff --git 2/s 1/f", "--- 2/s", "+++ 1/f"]
    );
    cleanup(&repo);
}

#[test]
fn an_explicit_prefix_flag_claims_only_its_own_side() {
    let (repo, home) = fixture("flags");
    // `--src-prefix`/`--dst-prefix` write one slot each, and
    // `diff_set_mnemonic_prefix()` only fills a slot still unset — so the other
    // side keeps its mnemonic letter.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--src-prefix=SRC/", "--", "f"])),
        ["diff --git SRC/f w/f", "--- SRC/f", "+++ w/f"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--dst-prefix=DST/", "--", "f"])),
        ["diff --git i/f DST/f", "--- i/f", "+++ DST/f"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(
            &repo,
            &home,
            &["diff", "--cached", "--src-prefix=SRC/", "--", "s"]
        )),
        ["diff --git SRC/s i/s", "--- /dev/null", "+++ i/s"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(
            &repo,
            &home,
            &["diff", "--no-index", "--src-prefix=S/", "f", "s"]
        )),
        ["diff --git S/f 2/s", "--- S/f", "+++ 2/s"]
    );
    cleanup(&repo);
}

#[test]
fn no_prefix_and_default_prefix_beat_the_mnemonic_fill() {
    let (repo, home) = fixture("override");
    // `diff_set_noprefix()` assigns two empty strings, which the mnemonic fill
    // then cannot reach.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--no-prefix", "--", "f"])),
        ["diff --git f f", "--- f", "+++ f"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(
            &repo,
            &home,
            &["diff", "--no-index", "--no-prefix", "f", "s"]
        )),
        ["diff --git f s", "--- f", "+++ s"]
    );
    // `diff.noPrefix` is the same assignment made from config, and it is tested
    // *before* the key (diff.c:5149-5151), so it wins there too.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["-c", "diff.noprefix=true", "diff", "--", "f"])),
        ["diff --git f f", "--- f", "+++ f"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(
            &repo,
            &home,
            &["-c", "diff.noprefix=true", "diff", "--cached", "--", "s"]
        )),
        ["diff --git s s", "--- /dev/null", "+++ s"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(
            &repo,
            &home,
            &["-c", "diff.noprefix=true", "diff", "--no-index", "f", "s"]
        )),
        ["diff --git f s", "--- f", "+++ s"]
    );
    // `--default-prefix` frees the configured prefixes before installing `a/`/`b/`
    // (diff.c:5792-5794), so it ignores both the key and `diff.srcPrefix`.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["diff", "--default-prefix", "--", "f"])),
        ["diff --git a/f b/f", "--- a/f", "+++ b/f"]
    );
    assert_eq!(
        prefix_lines(&run(
            &repo,
            &home,
            &["-c", "diff.srcprefix=S/", "diff", "--default-prefix", "--", "f"]
        )),
        ["diff --git a/f b/f", "--- a/f", "+++ b/f"]
    );
    cleanup(&repo);
}

#[test]
fn configured_prefixes_are_ignored_while_the_key_is_on() {
    let (repo, home) = fixture("cfgprefix");
    // `diff_set_default_prefix()` is the only reader of `diff.srcPrefix` /
    // `diff.dstPrefix`, and `diff_setup()` does not call it when the key is set —
    // so the configured strings simply never appear.
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["-c", "diff.srcprefix=S/", "diff", "--", "f"])),
        ["diff --git i/f w/f", "--- i/f", "+++ w/f"]
    );
    assert_eq!(
        prefix_lines(&mnemonic(&repo, &home, &["-c", "diff.dstprefix=D/", "diff", "--", "f"])),
        ["diff --git i/f w/f", "--- i/f", "+++ w/f"]
    );
    // With the key off they are honoured, in a tracked diff and a no-index one
    // alike — the latter reads them through the same `git_diff_ui_config()` pass.
    assert_eq!(
        prefix_lines(&run(
            &repo,
            &home,
            &["-c", "diff.srcprefix=S/", "-c", "diff.dstprefix=D/", "diff", "--", "f"]
        )),
        ["diff --git S/f D/f", "--- S/f", "+++ D/f"]
    );
    assert_eq!(
        prefix_lines(&run(
            &repo,
            &home,
            &["-c", "diff.srcprefix=S/", "diff", "--no-index", "f", "s"]
        )),
        ["diff --git S/f b/s", "--- S/f", "+++ b/s"]
    );
    cleanup(&repo);
}

#[test]
fn the_plumbing_verbs_and_the_history_commands_do_not_see_the_key() {
    let (repo, home) = fixture("scope");
    // `diff.mnemonicprefix` is read by `git_diff_ui_config()` (diff.c:406-409), so
    // the three plumbing verbs — which run `git_diff_basic_config()` — ignore it.
    for args in [
        vec!["diff-files", "-p", "--", "f"],
        vec!["diff-index", "-p", "HEAD", "--", "f"],
        vec!["diff-tree", "-p", "HEAD~1", "HEAD", "--", "f"],
    ] {
        assert_eq!(
            prefix_lines(&mnemonic(&repo, &home, &args)),
            ["diff --git a/f b/f", "--- a/f", "+++ b/f"],
            "{args:?} is plumbing"
        );
    }
    // `log -p`, `show` and `format-patch` are porcelain and do read the key, but
    // every diff they render is tree-against-tree, which has no mnemonic call.
    for args in [
        vec!["log", "-p", "-1", "--", "f"],
        vec!["show", "--", "f"],
        vec!["format-patch", "-1", "--stdout", "--", "f"],
    ] {
        assert_eq!(
            prefix_lines(&mnemonic(&repo, &home, &args)),
            ["diff --git a/f b/f", "--- a/f", "+++ b/f"],
            "{args:?} compares two trees"
        );
    }
    cleanup(&repo);
}

#[test]
fn status_verbose_inherits_the_prefixes_of_the_diff_it_runs() {
    let (repo, home) = fixture("status");
    // `wt_status_print_verbose()` only overrides the prefixes on the branch that
    // also prints a section label, so a plain `-v` leaves its staged patch on the
    // configured defaults — which is where `diff.mnemonicPrefix` reaches it, with
    // exactly the `c/`/`i/` pair `git diff --cached` would have used.
    let v = prefix_lines(&mnemonic(&repo, &home, &["status", "-v"]));
    assert!(
        v.contains(&"diff --git c/s i/s".to_owned()),
        "status -v staged patch: {v:?}"
    );
    // `-v -v` adds the unstaged patch, which is the index against the worktree.
    let vv = prefix_lines(&mnemonic(&repo, &home, &["status", "-v", "-v"]));
    assert!(
        vv.contains(&"diff --git c/s i/s".to_owned())
            && vv.contains(&"diff --git i/f w/f".to_owned()),
        "status -v -v patches: {vv:?}"
    );
    cleanup(&repo);
}
