//! Four `git submodule` behaviours that depend on state the port was not
//! looking at. Every expectation is the measured output of stock git 2.55.0.
//!
//! 1. **A conflicted gitlink is still a summary row.** Both diff commands
//!    special-case an unmerged index entry before they ever compare it —
//!    `run_diff_files()` at diff-lib.c:154 and `do_oneway_diff()` at
//!    diff-lib.c:467 — so `submodule summary`, `--cached` and `--files` each
//!    print a row with a different shape. zvcs walked stage-0 entries only and
//!    printed nothing in all three modes.
//!
//! 2. **`foreach --recursive` reports the failure at every level.** The
//!    recursion is a child `submodule--helper foreach` for git, and a failing
//!    child is `die(_("run_command returned non-zero status while recursing in
//!    the nested submodules of %s\n."))` (submodule--helper.c:422-425). zvcs
//!    recursed in-process and swallowed that line.
//!
//! 3. **`.gitmodules` is read from the index or `HEAD` when the work tree has no
//!    copy** (`config_from_gitmodules()`, submodule-config.c:795-806) — which is
//!    where `git submodule deinit` gets the url it prints for a submodule whose
//!    `.gitmodules` has already been deleted.
//!
//! 4. **`set-branch`/`set-url` create the work tree `.gitmodules`.** Neither
//!    consults `is_writing_gitmodules_ok()`; both go straight to
//!    `config_set_in_gitmodules_file_gently()` (submodule--helper.c:3313, 3245),
//!    which creates the file. zvcs failed to open it and exited 1.
//!
//! The relative-url anchor is checked here too: `resolve_relative_url()` reads
//! `xgetcwd()` (submodule--helper.c:66) *after* `setup_git_directory()` has
//! chdir'd to the top of the work tree, so `git submodule sync` run from a
//! subdirectory must resolve `../<repo>` against the superproject, not against
//! the subdirectory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
        .env("LC_ALL", "C");
    c
}

struct World {
    root: PathBuf,
    sup: PathBuf,
}

impl World {
    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        cmd(dir, &self.root, args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed ({:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A superproject `sup` holding submodule `s1`, cloned from `src1` beside it.
/// `src1` itself holds `nested` (from `src2`) when `nested` is set, which is
/// what makes a two-level `foreach --recursive` possible.
fn world(tag: &str, nested: bool) -> World {
    let root = std::env::temp_dir().join(format!("zvcs-smconf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(
        root.join("gitconfig"),
        "[protocol \"file\"]\n\tallow = always\n",
    )
    .unwrap();

    let w = World {
        sup: root.join("sup"),
        root,
    };
    for name in ["src1", "src2"] {
        let dir = w.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        w.ok(&dir, &["init", "-q", "-b", "main", "."]);
        std::fs::write(dir.join("a.txt"), format!("{name}\n")).unwrap();
        w.ok(&dir, &["add", "a.txt"]);
        w.ok(&dir, &["commit", "-qm", "c0"]);
    }
    if nested {
        let src1 = w.root.join("src1");
        w.ok(&src1, &["submodule", "add", "../src2", "nested"]);
        w.ok(&src1, &["commit", "-qm", "nest"]);
    }

    std::fs::create_dir_all(&w.sup).unwrap();
    w.ok(&w.sup, &["init", "-q", "-b", "main", "."]);
    std::fs::write(w.sup.join("f.txt"), "f\n").unwrap();
    w.ok(&w.sup, &["add", "f.txt"]);
    w.ok(&w.sup, &["commit", "-qm", "base"]);
    w.ok(&w.sup, &["submodule", "add", "../src1", "s1"]);
    w.ok(&w.sup, &["commit", "-qm", "add s1"]);
    w
}

/// Leave `s1` conflicted: two superproject branches move it to two different
/// commits, and the merge of the second into the first cannot resolve a gitlink.
/// Returns the abbreviated oid of the commit `s1` is checked out at and the
/// first-parent commit count `rev-list` reports for it, which is what the
/// summary header's `(<n>)` is.
fn conflict_the_submodule(w: &World) -> (String, String) {
    let s1 = w.sup.join("s1");
    w.ok(&w.sup, &["checkout", "-q", "-b", "other"]);
    std::fs::write(s1.join("b.txt"), "other\n").unwrap();
    w.ok(&s1, &["add", "b.txt"]);
    w.ok(&s1, &["commit", "-qm", "other side"]);
    w.ok(&w.sup, &["commit", "-qam", "other s1"]);

    w.ok(&w.sup, &["checkout", "-q", "main"]);
    w.ok(&s1, &["checkout", "-q", "main"]);
    std::fs::write(s1.join("c.txt"), "mine\n").unwrap();
    w.ok(&s1, &["add", "c.txt"]);
    w.ok(&s1, &["commit", "-qm", "my side"]);
    w.ok(&w.sup, &["commit", "-qam", "main s1"]);

    // The merge is expected to fail; only its effect on the index matters.
    let _ = w.run(&w.sup, &["merge", "other"]);
    let unmerged = w.ok(&w.sup, &["ls-files", "-u", "--", "s1"]);
    assert_eq!(unmerged.lines().count(), 3, "want three stages: {unmerged}");

    (
        w.ok(&s1, &["rev-parse", "--short", "HEAD"]).trim().to_string(),
        w.ok(&s1, &["rev-list", "--first-parent", "--count", "HEAD"])
            .trim()
            .to_string(),
    )
}

/// Stock git 2.55.0 on a conflicted `s1` whose checkout is at `<head>`:
///
/// ```text
/// $ git submodule summary
/// * s1 <head>...<head> (0):
///
/// $ git submodule summary --cached
/// * s1 <head>...0000000 (<n>):
///   < my side
///
/// $ git submodule summary --files
/// * s1 0000000...<head> (<n>):
///   > my side
/// ```
#[test]
fn summary_renders_a_conflicted_gitlink_in_all_three_modes() {
    let w = world("summary", false);
    let (head, n) = conflict_the_submodule(&w);

    let plain = w.ok(&w.sup, &["submodule", "summary"]);
    assert_eq!(plain, format!("* s1 {head}...{head} (0):\n\n"));

    // `--cached` compares HEAD against the index, where the unmerged entry has
    // no stage 0 at all, so the destination side is the null oid and the log
    // walks backwards from the source.
    let cached = w.ok(&w.sup, &["submodule", "summary", "--cached"]);
    assert_eq!(cached, format!("* s1 {head}...0000000 ({n}):\n  < my side\n\n"));

    // `--files` compares the index against the work tree, so the *source* is the
    // empty side and the log walks forwards to the checkout.
    let files = w.ok(&w.sup, &["submodule", "summary", "--files"]);
    assert_eq!(files, format!("* s1 0000000...{head} ({n}):\n  > my side\n\n"));

    // A merged superproject prints nothing at all for an unchanged submodule —
    // the rows above are the conflict, not a permanent row.
    w.ok(&w.sup, &["merge", "--abort"]);
    assert_eq!(w.ok(&w.sup, &["submodule", "summary"]), "");
}

/// A failure two levels down produces one `for <path>` line and one `while
/// recursing in the nested submodules of <path>` line per level it passes
/// through, and the whole walk exits 128.
#[test]
fn foreach_recursive_reports_the_failure_at_every_level() {
    let w = world("foreach", true);
    w.ok(&w.sup, &["submodule", "update", "--init", "--recursive"]);

    let out = w.run(
        &w.sup,
        &["submodule", "foreach", "--recursive", "test $name != nested"],
    );
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stdout_of(&out), "Entering 's1'\nEntering 's1/nested'\n");
    assert_eq!(
        stderr_of(&out),
        "fatal: run_command returned non-zero status for s1/nested\n.\n\
         fatal: run_command returned non-zero status while recursing in the nested submodules of s1\n.\n"
    );

    // Failing at the top level stops before any recursion, so only the `for`
    // line is printed.
    let out = w.run(
        &w.sup,
        &["submodule", "foreach", "--recursive", "test $name != s1"],
    );
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stdout_of(&out), "Entering 's1'\n");
    assert_eq!(
        stderr_of(&out),
        "fatal: run_command returned non-zero status for s1\n.\n"
    );

    // And a command that succeeds everywhere still visits both levels.
    let out = w.run(&w.sup, &["submodule", "foreach", "--recursive", "true"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "Entering 's1'\nEntering 's1/nested'\n");
}

/// With `.gitmodules` gone from the work tree the mappings still come from the
/// index (and then from `HEAD`), so `deinit` names the url it always named.
#[test]
fn gitmodules_falls_back_to_the_index_and_head_copies() {
    let w = world("fallback", false);
    std::fs::remove_file(w.sup.join(".gitmodules")).unwrap();

    let out = w.run(&w.sup, &["submodule", "deinit", "-f", "s1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "Cleared directory 's1'\n\
         Submodule 's1' (../src1) unregistered for path 's1'\n"
    );

    // Gone from the index too: `HEAD:.gitmodules` is the last source, and it
    // still carries the same url.
    let w = world("fallback2", false);
    w.ok(&w.sup, &["rm", "-q", "--cached", ".gitmodules"]);
    std::fs::remove_file(w.sup.join(".gitmodules")).unwrap();
    let out = w.run(&w.sup, &["submodule", "deinit", "-f", "s1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("Submodule 's1' (../src1) unregistered for path 's1'"),
        "{}",
        stdout_of(&out)
    );
}

/// `config_set_in_gitmodules_file_gently()` creates the file. The name still
/// comes from the index copy, so `set-branch` succeeds and leaves a
/// `.gitmodules` holding nothing but the key it wrote.
#[test]
fn set_branch_and_set_url_create_a_missing_gitmodules() {
    let w = world("setbranch", false);
    std::fs::remove_file(w.sup.join(".gitmodules")).unwrap();

    w.ok(&w.sup, &["submodule", "set-branch", "--branch", "x", "s1"]);
    assert_eq!(
        std::fs::read_to_string(w.sup.join(".gitmodules")).unwrap(),
        "[submodule \"s1\"]\n\tbranch = x\n"
    );

    // The file now exists and is the only source, so it no longer maps `s1` to a
    // path — and `set-url`, which resolves by path, says so.
    let out = w.run(&w.sup, &["submodule", "set-url", "s1", "../src2"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr_of(&out),
        "fatal: no submodule mapping found in .gitmodules for path 's1'\n"
    );

    // Reaching `set-url` first instead writes the url into a file it creates.
    let w = world("seturl", false);
    std::fs::remove_file(w.sup.join(".gitmodules")).unwrap();
    w.ok(&w.sup, &["submodule", "set-url", "s1", "../src2"]);
    assert_eq!(
        std::fs::read_to_string(w.sup.join(".gitmodules")).unwrap(),
        "[submodule \"s1\"]\n\turl = ../src2\n"
    );
}

/// `resolve_relative_url()`'s anchor is the top of the work tree. Run from
/// `sup/d/e`, `git submodule sync` must still turn `../src1` into
/// `<root>/src1` — anchoring on the process directory would give
/// `<root>/sup/d/src1`.
#[test]
fn sync_from_a_subdirectory_anchors_the_relative_url_on_the_superproject() {
    let w = world("syncsub", false);
    let deep = w.sup.join("d/e");
    std::fs::create_dir_all(&deep).unwrap();
    let want = w.root.join("src1");

    // Scrub the url the initial `submodule add` recorded, so only `sync` can
    // put it back.
    w.ok(&w.sup, &["config", "submodule.s1.url", "bogus://nowhere"]);
    let out = w.run(&deep, &["submodule", "sync"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "Synchronizing submodule url for '../../s1'\n");

    assert_eq!(
        w.ok(&w.sup, &["config", "--get", "submodule.s1.url"]).trim(),
        want.to_str().unwrap()
    );
    assert_eq!(
        w.ok(&w.sup.join("s1"), &["config", "--get", "remote.origin.url"]).trim(),
        want.to_str().unwrap()
    );
}
