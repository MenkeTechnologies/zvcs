//! `git submodule update` with an `update = !<command>` strategy or run from a
//! subdirectory, and `git submodule add` with `--reference`/`--dissociate` or onto
//! a `modules/<name>` an earlier add left behind.
//!
//! * SM_UPDATE_COMMAND runs `<command> <oid>` through the shell in the submodule
//!   and prints `Submodule path '<path>': '<command> <oid>'`, or dies `Execution
//!   of '<command> <oid>' failed in submodule path '<path>'`
//!   (builtin/submodule--helper.c:2553-2643). zvcs refused the strategy.
//! * clone_submodule()'s path is repo_get_work_tree() + "/" + path
//!   (submodule--helper.c:1918-1920), so from `super/nest` the "Cloning into" line
//!   names `/.../super/suba`, where zvcs printed `/.../super/nest/../suba`.
//! * `add --reference <repo>` and `--dissociate` reach the clone
//!   (submodule--helper.c:1939-1951); zvcs refused both options.
//! * A second `add` of a removed submodule lists the remotes of the git directory
//!   it found (append_fetch_remotes(), submodule--helper.c:3383-3407), and with
//!   `--force` reuses that directory instead of cloning into it
//!   (submodule--helper.c:1984-1992), which zvcs failed with "already exists".
//! * update's is_tip_reachable() is `rev-list -n 1 <oid> --not --all`, whose
//!   limit_list() keeps popping uninteresting commits for SLOP rounds
//!   (revision.c:1304-1327): with a commit dated after its child, zvcs stopped the
//!   hidden walk early, took the recorded commit for unreachable and fetched it
//!   by id.
//!
//! Each case builds the fixture with stock git at the same path for both binaries.
//! Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str], date: &str) -> Output {
    let mut c = Command::new(bin);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_0", "always")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("LC_ALL", "C")
        .output()
        .expect("spawn git")
}

const DATE: &str = "1700000000 +0000";

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str], date: &str) {
    let out = run(bin, dir, home, args, date);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn commit(stock: &str, dir: &Path, home: &Path, content: &str, date: &str) {
    std::fs::write(dir.join("f"), content).unwrap();
    ok(stock, dir, home, &["add", "f"], date);
    ok(stock, dir, home, &["commit", "-q", "-m", content], date);
}

/// `up_a` (main and topic, tag v1), `up_c`, and `super` cloned from `up_super`
/// with `suba` and `nest/b` checked out. `up_a`'s second commit is dated after
/// the one on top of it, as a skewed clock leaves history.
fn fixture(stock: &str, root: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let a = root.join("up_a");
    ok(stock, &root, &home, &["init", "-q", "up_a"], DATE);
    commit(stock, &a, &home, "a1", "1700000060 +0000");
    ok(stock, &a, &home, &["tag", "v1"], DATE);
    ok(stock, &a, &home, &["branch", "topic"], DATE);
    commit(stock, &a, &home, "a2", "1700000120 +0000");
    ok(stock, &a, &home, &["checkout", "-q", "topic"], DATE);
    commit(stock, &a, &home, "t1", "1700000180 +0000");
    ok(stock, &a, &home, &["checkout", "-q", "main"], DATE);
    for up in ["up_b", "up_c"] {
        ok(stock, &root, &home, &["init", "-q", up], DATE);
        commit(stock, &root.join(up), &home, up, DATE);
    }
    let up = root.join("up_super");
    ok(stock, &root, &home, &["init", "-q", "up_super"], DATE);
    let url = |n: &str| format!("file://{}", root.join(n).display());
    ok(stock, &up, &home, &["submodule", "add", "-q", &url("up_a"), "suba"], DATE);
    ok(stock, &up, &home, &["submodule", "add", "-q", &url("up_b"), "nest/b"], DATE);
    ok(stock, &up, &home, &["commit", "-q", "-m", "subs"], DATE);
    // A commit on up_a's main dated before the recorded one it sits on, and before
    // the tag, so the hidden side pops the tag first.
    commit(stock, &a, &home, "a3", "1700000050 +0000");
    ok(stock, &root, &home, &["clone", "-q", up.to_str().unwrap(), "super"], DATE);
    let sup = root.join("super");
    ok(stock, &sup, &home, &["submodule", "update", "-q", "--init"], DATE);
    sup
}

/// Run `script` through `sh -c` in `super/<sub>` with `PATH` leading to each
/// binary in turn, each on its own fresh fixture at the same path.
fn twin(stock: &str, tag: &str, sub: &str, script: &str) -> (Output, Output) {
    let root = std::env::temp_dir().join(format!("zvcs-sm-upd-{tag}-{}", std::process::id()));
    let bins = root.with_extension("bin");
    let mut outs = Vec::new();
    for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
        let sup = fixture(stock, &root);
        let home = sup.parent().unwrap().join("home");
        let dir = bins.join(side);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(bin, dir.join("git")).unwrap();
        let path = format!("{}:/usr/bin:/bin", dir.display());
        let mut c = Command::new("/bin/sh");
        for (k, _) in std::env::vars() {
            if k.starts_with("GIT_") {
                c.env_remove(k);
            }
        }
        let out = c
            .args(["-c", script])
            .current_dir(sup.join(sub))
            .env("PATH", path)
            .env("HOME", &home)
            .env_remove("XDG_CONFIG_HOME")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "protocol.file.allow")
            .env("GIT_CONFIG_VALUE_0", "always")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_DATE", DATE)
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        outs.push(out);
    }
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&bins);
    let z = outs.pop().unwrap();
    (outs.pop().unwrap(), z)
}

fn assert_same(what: &str, s: &Output, z: &Output) {
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        String::from_utf8_lossy(&s.stderr),
        "{what}: stderr"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stdout),
        String::from_utf8_lossy(&s.stdout),
        "{what}: stdout"
    );
    assert_eq!(z.status.code(), s.status.code(), "{what}: exit code");
}

fn check(tag: &str, sub: &str, script: &str) {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin(stock, tag, sub, script);
    assert_same(script, &s, &z);
}

#[test]
fn an_update_command_strategy_runs_through_the_shell() {
    let rewind = "git -C suba checkout -q HEAD~1 && ";
    check("cmd-ok", "", &format!("git config submodule.suba.update '!echo ran' && {rewind}git submodule update"));
    check("cmd-meta", "", &format!("git config submodule.suba.update '!echo a; echo b' && {rewind}git submodule update"));
    check("cmd-quiet", "", &format!("git config submodule.suba.update '!echo q' && {rewind}git submodule --quiet update"));
    check("cmd-fail", "", &format!("git config submodule.suba.update '!false' && {rewind}git submodule update"));
}

#[test]
fn a_clone_from_a_subdirectory_names_the_resolved_path() {
    check("subdir", "nest", "git submodule deinit -q -f --all && git submodule update --init 2>&1 | grep Cloning");
}

#[test]
fn add_takes_a_reference_and_reuses_a_left_behind_git_directory() {
    check("ref", "", "git submodule add --reference ../up_c ../up_c refd && cat .git/modules/refd/objects/info/alternates");
    check("dissociate", "", "git submodule add --dissociate --reference ../up_c ../up_c refd; ls .git/modules/refd/objects/info");
    let readd = "git submodule add -q ../up_c newc && git rm -q -f newc && git submodule add";
    check("found", "", &format!("{readd} ../up_c newc"));
    check("force", "", &format!("{readd} --force ../up_c newc && git submodule status newc"));
}

#[test]
fn update_finds_a_recorded_commit_dated_after_its_child_without_fetching_it() {
    check("skew", "", "git -C suba checkout -q main && git -C suba commit -q --allow-empty -m local && git submodule update && ls .git/modules/suba");
}
