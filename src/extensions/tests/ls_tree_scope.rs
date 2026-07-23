//! `git ls-tree` cwd-relative scoping parity: run from a subdirectory the
//! listing is limited to that directory's subtree and paths print relative to
//! it, while `--full-name` widens the display to root-relative paths and
//! `--full-tree` widens the scope back to the whole tree. Each case is asserted
//! byte-for-byte against the system `git` on an identical repository (object ids
//! match because it is the same repo), so the test is self-validating parity.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn stdout_of(bin: &str, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(bin).args(args).current_dir(cwd).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Assert the zvcs binary's `ls-tree` stdout matches the real `git`'s, verbatim.
fn assert_parity(cwd: &Path, args: &[&str]) {
    let want = stdout_of("git", cwd, args);
    let got = stdout_of(BIN, cwd, args);
    assert_eq!(got, want, "ls-tree {args:?} in {cwd:?}\n--- git ---\n{want}\n--- zvcs ---\n{got}");
}

#[test]
fn ls_tree_cwd_scope_and_full_variants() {
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
    assert_parity(&dir, &["ls-tree", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "-r", "HEAD"]);
    // `-t` surfaces the descended directory itself, rendered as "./".
    assert_parity(&dir, &["ls-tree", "-t", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "-d", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "-l", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "--name-only", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "--format=%(path)", "HEAD"]);

    // --full-name: same scope, root-relative display.
    assert_parity(&dir, &["ls-tree", "--full-name", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "-r", "--full-name", "HEAD"]);

    // --full-tree: whole tree from the root, root-relative display.
    assert_parity(&dir, &["ls-tree", "--full-tree", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "-r", "--full-tree", "HEAD"]);
    assert_parity(&dir, &["ls-tree", "--full-tree", "HEAD", "dir/a.txt"]);

    // Operands are taken relative to the current directory (prefix-prepended).
    assert_parity(&dir, &["ls-tree", "HEAD", "a.txt"]);
    assert_parity(&dir, &["ls-tree", "--full-name", "HEAD", "a.txt"]);
    assert_parity(&dir, &["ls-tree", "-r", "HEAD", "sub"]);

    // Deeper subdirectory: ancestor directories render with "../" under -t.
    assert_parity(&sub, &["ls-tree", "HEAD"]);
    assert_parity(&sub, &["ls-tree", "-t", "HEAD", "deep.txt"]);

    // From the root: a trailing-slash operand lists directory contents.
    assert_parity(&repo, &["ls-tree", "HEAD"]);
    assert_parity(&repo, &["ls-tree", "HEAD", "dir/"]);
    assert_parity(&repo, &["ls-tree", "HEAD", "dir"]);
    assert_parity(&repo, &["ls-tree", "-d", "-r", "HEAD"]);

    let _ = std::fs::remove_dir_all(&root);
}
