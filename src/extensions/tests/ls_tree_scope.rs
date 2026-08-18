//! `git ls-tree` cwd-relative scoping parity: run from a subdirectory the
//! listing is limited to that directory's subtree and paths print relative to
//! it, while `--full-name` widens the display to root-relative paths and
//! `--full-tree` widens the scope back to the whole tree. Each case is asserted
//! byte-for-byte against stock git on an identical repository (object ids match
//! because it is the same repo).
//!
//! The oracle is resolved EXPLICITLY, never through `PATH`. This test previously
//! spawned `Command::new("git")`, and on the machine this is developed on `PATH`
//! finds `~/.zvcs/bin/git` — zvcs itself. Every assertion here was therefore
//! comparing the build under test against an older *released zvcs*, and would
//! have agreed just as happily on a shared bug. A version string cannot tell the
//! two apart either, because zvcs answers `git version 2.55.0` deliberately, so
//! each candidate is probed with a superset verb the way
//! `src/parity/src/stock.rs` does: zvcs serves `zverbs`, stock does not.
//!
//! When no stock git is installed the test skips rather than fails, so a headless
//! CI box does not turn a missing oracle into a red build.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn stdout_of(bin: &str, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(bin).args(args).current_dir(cwd).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether `bin` is zvcs wearing git's name. zvcs serves the superset verb
/// `zverbs` itself; a stock git looks for a `git-zverbs` on `PATH` and fails.
/// The probe runs with an emptied `PATH` so an installed `git-zverbs` shim
/// cannot make stock answer it.
fn is_zvcs(bin: &str) -> bool {
    Command::new(bin)
        .arg("zverbs")
        .env("PATH", "")
        .current_dir(std::env::temp_dir())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The stock git to compare against, or `None` when this machine has none.
///
/// `ZVCS_STOCK_GIT` wins if it names something that is not zvcs; otherwise the
/// usual install locations are probed and the newest non-zvcs candidate is used,
/// mirroring `src/parity/src/stock.rs`. Picking the *newest* matters: this port
/// targets 2.55.0, and `/usr/bin/git` is an older Apple build on macOS.
fn stock_git() -> Option<String> {
    fn version(bin: &str) -> Option<(u32, u32, u32)> {
        let out = Command::new(bin).arg("--version").output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let rest = s.split("git version ").nth(1)?;
        let mut it = rest.split_whitespace().next()?.split('.');
        Some((
            it.next()?.parse().ok()?,
            it.next().unwrap_or("0").parse().unwrap_or(0),
            it.next().unwrap_or("0").parse().unwrap_or(0),
        ))
    }

    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        if Path::new(&p).exists() && !is_zvcs(&p) {
            return Some(p);
        }
        return None;
    }
    ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"]
        .into_iter()
        .filter(|p| Path::new(p).exists() && !is_zvcs(p))
        .filter_map(|p| version(p).map(|v| (v, p.to_owned())))
        .max()
        .map(|(_, p)| p)
}

/// Assert the zvcs binary's `ls-tree` stdout matches stock git's, verbatim.
fn assert_parity(stock: &str, cwd: &Path, args: &[&str]) {
    let want = stdout_of(stock, cwd, args);
    let got = stdout_of(BIN, cwd, args);
    assert_eq!(
        got, want,
        "ls-tree {args:?} in {cwd:?}\n--- stock ({stock}) ---\n{want}\n--- zvcs ---\n{got}"
    );
}

#[test]
fn ls_tree_cwd_scope_and_full_variants() {
    let Some(stock) = stock_git() else {
        eprintln!("no stock git found to compare against; skipping");
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-lstree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("dir/sub")).unwrap();
    std::fs::create_dir_all(repo.join("d2/d1")).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "a@b.c"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("root.txt"), b"x").unwrap();
    std::fs::write(repo.join("dir/a.txt"), b"yy").unwrap();
    std::fs::write(repo.join("dir/b.txt"), b"zzz").unwrap();
    std::fs::write(repo.join("dir/sub/deep.txt"), b"q").unwrap();
    std::fs::write(repo.join("d2/d1/f.txt"), b"w").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"]);

    let dir = repo.join("dir");
    let sub = repo.join("dir/sub");

    // From a subdirectory: default is scoped to that directory, paths relative.
    assert_parity(&stock, &dir, &["ls-tree", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "-r", "HEAD"]);
    // `-t` surfaces the descended directory itself, rendered as "./".
    assert_parity(&stock, &dir, &["ls-tree", "-t", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "-d", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "-l", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "--name-only", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "--format=%(path)", "HEAD"]);

    // --full-name: same scope, root-relative display.
    assert_parity(&stock, &dir, &["ls-tree", "--full-name", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "-r", "--full-name", "HEAD"]);

    // --full-tree: whole tree from the root, root-relative display.
    assert_parity(&stock, &dir, &["ls-tree", "--full-tree", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "-r", "--full-tree", "HEAD"]);
    assert_parity(&stock, &dir, &["ls-tree", "--full-tree", "HEAD", "dir/a.txt"]);

    // Operands are taken relative to the current directory (prefix-prepended).
    assert_parity(&stock, &dir, &["ls-tree", "HEAD", "a.txt"]);
    assert_parity(&stock, &dir, &["ls-tree", "--full-name", "HEAD", "a.txt"]);
    assert_parity(&stock, &dir, &["ls-tree", "-r", "HEAD", "sub"]);

    // Deeper subdirectory: ancestor directories render with "../" under -t.
    assert_parity(&stock, &sub, &["ls-tree", "HEAD"]);
    assert_parity(&stock, &sub, &["ls-tree", "-t", "HEAD", "deep.txt"]);

    // From the root: a trailing-slash operand lists directory contents.
    assert_parity(&stock, &repo, &["ls-tree", "HEAD"]);
    assert_parity(&stock, &repo, &["ls-tree", "HEAD", "dir/"]);
    assert_parity(&stock, &repo, &["ls-tree", "HEAD", "dir"]);
    assert_parity(&stock, &repo, &["ls-tree", "-d", "-r", "HEAD"]);

    let _ = std::fs::remove_dir_all(&root);
}
