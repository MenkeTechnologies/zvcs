//! `--recurse-submodules` moves each submodule an unpack touches through
//! `submodule_move_head()` (submodule.c:2126-2240): `check_submodule_move_head()`
//! dry-runs the move from the merge functions (unpack-trees.c:315-333), and
//! `checkout_entry()` / `unlink_entry()` run it for real (entry.c:517-535, :594-607)
//! as `read-tree --recurse-submodules --super-prefix=<path>/ -u (--reset|-m)` plus
//! `update-ref HEAD --no-deref <new>` inside the submodule.
//!
//! Observable consequences checked here against stock git: `reset --hard` resets a
//! dirty submodule whose gitlink did not move (nested ones included) and detaches its
//! `HEAD` with an empty reflog message; a refused dry run reports the child's
//! super-prefixed path and the porcelain `Cannot update submodule:` block; a checkout
//! discards a submodule change the move does not conflict with; a submodule the target
//! drops is emptied and loses `core.worktree`, and one it adds is populated with its
//! nested submodule connected; `reset --merge` dies before moving `HEAD`.
//!
//! `reset --hard` runs with `-q`: its `Updating files` meter is a delayed progress
//! display, so whether it appears depends on how long the submodule moves take.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "3")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_1", "always")
        .env("GIT_CONFIG_KEY_2", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_2", "main")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn ok(bin: &str, dir: &Path, args: &[&str]) {
    let (_, err, code) = run(bin, dir, args);
    assert_eq!(code, 0, "{args:?} in {}: {err}", dir.display());
}

/// `subsrc` with nested submodule `nested`, and `super` whose `c0` has no
/// submodule, `c1` records `sub` at `s1b` and `c2` (checked out, on `main`) at `s2`.
/// `s1b` -> `s2` changes `f` and adds `g`; `keep` is the same in both.
fn base(stock: &str) -> &'static Path {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("zvcs-submodule-move-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let nested = root.join("nested");
        let subsrc = root.join("subsrc");
        let sup = root.join("super");
        ok(stock, &root, &["init", "-q", "nested"]);
        std::fs::write(nested.join("n"), "n1\n").unwrap();
        ok(stock, &nested, &["add", "n"]);
        ok(stock, &nested, &["commit", "-q", "-m", "n1"]);
        ok(stock, &root, &["init", "-q", "subsrc"]);
        std::fs::write(subsrc.join("f"), "s1\n").unwrap();
        std::fs::write(subsrc.join("keep"), "k\n").unwrap();
        ok(stock, &subsrc, &["add", "f", "keep"]);
        ok(stock, &subsrc, &["commit", "-q", "-m", "s1"]);
        ok(stock, &subsrc, &["submodule", "add", "-q", "../nested", "nested"]);
        ok(stock, &subsrc, &["commit", "-q", "-m", "s1b"]);
        std::fs::write(subsrc.join("f"), "s2\n").unwrap();
        std::fs::write(subsrc.join("g"), "g\n").unwrap();
        ok(stock, &subsrc, &["add", "f", "g"]);
        ok(stock, &subsrc, &["commit", "-q", "-m", "s2"]);
        ok(stock, &root, &["init", "-q", "super"]);
        std::fs::write(sup.join("t"), "top\n").unwrap();
        ok(stock, &sup, &["add", "t"]);
        ok(stock, &sup, &["commit", "-q", "-m", "c0"]);
        ok(stock, &sup, &["tag", "c0"]);
        ok(stock, &sup, &["submodule", "add", "-q", "../subsrc", "sub"]);
        ok(stock, &sup.join("sub"), &["checkout", "-q", "HEAD~1"]);
        ok(stock, &sup.join("sub"), &["submodule", "update", "-q", "--init"]);
        ok(stock, &sup, &["add", "sub"]);
        ok(stock, &sup, &["commit", "-q", "-m", "c1"]);
        ok(stock, &sup, &["tag", "c1"]);
        ok(stock, &sup.join("sub"), &["checkout", "-q", "main"]);
        ok(stock, &sup.join("sub"), &["submodule", "update", "-q"]);
        ok(stock, &sup, &["add", "sub"]);
        ok(stock, &sup, &["commit", "-q", "-m", "c2"]);
        root
    })
}

/// A fresh copy of the base fixture, its indexes refreshed after the copy changed
/// every inode, with `setup` applied by stock git.
fn fixture(stock: &str, name: &str, setup: &[&[&str]]) -> PathBuf {
    let base = base(stock);
    let dir = base.with_file_name(format!("{}-{name}", base.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&dir);
    let status = Command::new("cp").arg("-R").arg(base).arg(&dir).status().unwrap();
    assert!(status.success());
    let sup = dir.join("super");
    for repo in [sup.clone(), sup.join("sub"), sup.join("sub/nested")] {
        run(stock, &repo, &["update-index", "-q", "--refresh"]);
    }
    for step in setup {
        match step {
            ["write", path, content] => std::fs::write(sup.join(path), content).unwrap(),
            args => ok(stock, &sup, args),
        }
    }
    dir
}

/// Everything a submodule move leaves behind, read with stock git.
fn state(stock: &str, sup: &Path) -> String {
    let mut out = String::new();
    let probe = |out: &mut String, dir: &Path, args: &[&str]| {
        let (o, e, code) = run(stock, dir, args);
        out.push_str(&format!("$ {args:?}\n{o}{e}[{code}]\n"));
    };
    probe(&mut out, sup, &["rev-parse", "HEAD"]);
    probe(&mut out, sup, &["status", "--porcelain", "--ignore-submodules=none"]);
    for path in ["sub", "sub/nested"] {
        let dir = sup.join(path);
        if !dir.exists() {
            out.push_str(&format!("{path}: absent\n"));
            continue;
        }
        let mut names: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        out.push_str(&format!("{path}: {names:?} {:?}\n", std::fs::read_to_string(dir.join(".git")).ok()));
        // `branch.oid` and `branch.head` are the submodule `HEAD` and whether it is detached.
        probe(&mut out, &dir, &["status", "--porcelain=v2", "--branch"]);
        probe(&mut out, &dir, &["reflog", "-n1", "--format=%H %gs", "HEAD"]);
    }
    for gitdir in [".git/modules/sub", ".git/modules/sub/modules/nested"] {
        if sup.join(gitdir).exists() {
            probe(&mut out, sup, &["--git-dir", gitdir, "config", "core.worktree"]);
        }
    }
    out
}

/// Run `args` in `super` of identical fixtures through both binaries and compare
/// stdout, stderr, exit code and the state left behind.
fn compare(name: &str, setup: &[&[&str]], args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (label, bin) in [("stock", stock), ("zvcs", ZVCS)] {
        let dir = fixture(stock, &format!("{name}-{label}"), setup);
        let sup = dir.join("super");
        let result = run(bin, &sup, args);
        seen.push((result, state(stock, &sup)));
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(seen[1].0, seen[0].0, "{args:?}: zvcs (left) vs stock (right) output");
    assert_eq!(seen[1].1, seen[0].1, "{args:?}: zvcs (left) vs stock (right) state");
}

#[test]
fn reset_hard_resets_dirty_submodules_whose_gitlink_did_not_move() {
    compare(
        "hard-dirty",
        &[&["write", "sub/f", "x\n"], &["write", "sub/nested/n", "y\n"]],
        &["reset", "-q", "--hard", "--recurse-submodules", "HEAD"],
    );
}

#[test]
fn checkout_refuses_a_submodule_move_that_loses_changes() {
    compare("co-refused", &[&["write", "sub/f", "x\n"]], &["checkout", "--recurse-submodules", "c1"]);
}

#[test]
fn switch_moves_submodule_and_discards_unconflicting_changes() {
    compare(
        "switch-keep",
        &[&["write", "sub/keep", "x\n"]],
        &["-c", "submodule.recurse=true", "switch", "-d", "c1"],
    );
}

#[test]
fn read_tree_child_messages_carry_the_super_prefix() {
    compare(
        "read-tree",
        &[&["write", "sub/f", "x\n"]],
        &["read-tree", "-u", "-m", "--recurse-submodules", "HEAD", "c1"],
    );
}

#[test]
fn checkout_drops_and_adds_submodules() {
    compare("co-drop", &[], &["checkout", "--recurse-submodules", "c0"]);
    compare(
        "reset-add",
        &[&["reset", "-q", "--hard", "--recurse-submodules", "c0"]],
        &["reset", "-q", "--hard", "--recurse-submodules", "main"],
    );
}

#[test]
fn reset_merge_dies_before_moving_head() {
    compare("merge-refused", &[&["write", "sub/f", "x\n"]], &["reset", "--merge", "--recurse-submodules", "c1"]);
}
