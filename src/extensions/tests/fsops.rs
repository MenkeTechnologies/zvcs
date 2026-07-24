//! Native filesystem verbs (`zmkdir`, `ztouch`, `zrm`, `zcp`, `zmv`, `zcat`,
//! `zln`) that make the `zrepl` console usable like a shell. These exercise the
//! create/copy/move/remove paths, including the recursive and guard cases, so a
//! regression in the on-disk operations is caught.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn ok(args: &[&str]) {
    let out = run(args);
    assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn filesystem_verbs_roundtrip() {
    let root = std::env::temp_dir().join(format!("zvcs-fsops-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let p = |rel: &str| root.join(rel).to_string_lossy().into_owned();

    // zmkdir -p makes the whole chain.
    ok(&["zmkdir", "-p", &p("a/b/c")]);
    assert!(Path::new(&p("a/b/c")).is_dir());

    // ztouch creates a file.
    ok(&["ztouch", &p("a/f.txt")]);
    assert!(Path::new(&p("a/f.txt")).is_file());

    // zcat prints contents.
    std::fs::write(p("a/f.txt"), "hello\n").unwrap();
    let out = run(&["zcat", &p("a/f.txt")]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");

    // zcp -r copies a tree; zcp into a directory keeps the basename.
    ok(&["zcp", "-r", &p("a"), &p("a2")]);
    assert!(Path::new(&p("a2/b/c")).is_dir());
    assert_eq!(std::fs::read_to_string(p("a2/f.txt")).unwrap(), "hello\n");

    // zmv renames.
    ok(&["zmv", &p("a/f.txt"), &p("a/renamed.txt")]);
    assert!(!Path::new(&p("a/f.txt")).exists());
    assert!(Path::new(&p("a/renamed.txt")).is_file());

    // zln -s makes a symlink pointing at the target.
    ok(&["zln", "-s", &p("a/renamed.txt"), &p("a/link")]);
    assert!(std::fs::symlink_metadata(p("a/link")).unwrap().file_type().is_symlink());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zrm_guards_and_force() {
    let root = std::env::temp_dir().join(format!("zvcs-fsrm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let p = |rel: &str| root.join(rel).to_string_lossy().into_owned();

    std::fs::create_dir(p("dir")).unwrap();
    std::fs::write(p("dir/inner.txt"), "x").unwrap();

    // Removing a directory without -r is refused.
    let out = run(&["zrm", &p("dir")]);
    assert!(!out.status.success(), "zrm of a dir without -r must fail");
    assert!(Path::new(&p("dir")).exists(), "the directory must be left intact");

    // -r removes it.
    ok(&["zrm", "-r", &p("dir")]);
    assert!(!Path::new(&p("dir")).exists());

    // -f ignores a missing path (success).
    ok(&["zrm", "-f", &p("does-not-exist")]);

    // Without -f, a missing path is an error.
    let out = run(&["zrm", &p("also-missing")]);
    assert!(!out.status.success(), "zrm of a missing path without -f must fail");

    let _ = std::fs::remove_dir_all(&root);
}
