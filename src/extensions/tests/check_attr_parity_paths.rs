//! `git check-attr` parity for the path it is handed, and for the file name it
//! quotes back when an attribute file is malformed.
//!
//! `check_attr()` looks paths up through `prefix_path()`
//! (builtin/check-attr.c:69-70), whose absolute arm is
//! `abspath_part_inside_repo()` (setup.c:50-106): it dereferences symlinks that
//! lie outside the work tree, so a path that reaches the work tree through one
//! is inside it, and it resolves each level with `strbuf_realpath(…, 1)`, which
//! dies `Invalid path '<dir>': …` for a leading directory that is not there
//! (abspath.c:128-137) instead of reporting "outside repository".
//!
//! The name in `report_invalid_attr()` (attr.c:218-226) is `pl->src`, the
//! attribute file's path relative to the work tree root, which is where git
//! stands while it reads them.
//!
//! Every case here is byte-compared against stock git 2.55.0 by hand.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-ckattr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("s1/s2")).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitattributes"), "*.txt text\n").unwrap();
    std::fs::write(repo.join("s1/s2/.gitattributes"), "*.d bad@name\n").unwrap();
    std::fs::write(repo.join("f.txt"), "").unwrap();
    std::fs::write(repo.join("s1/s2/x.d"), "").unwrap();
    (repo, home)
}

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_ATTR_SOURCE")
        .output()
        .unwrap()
}

fn out_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `report_invalid_attr()` names the attribute file the way `prep_attr_stack()`
/// built it: relative to the work tree root, with no `./` or `../` lead-in, and
/// the same from anywhere inside the repository.
#[test]
fn a_malformed_attribute_file_is_named_relative_to_the_work_tree() {
    let (repo, home) = fixture("src");
    const EXPECTED: &str = "bad@name is not a valid attribute name: s1/s2/.gitattributes:1\n";

    let out = run(&home, &repo, &["check-attr", "-a", "s1/s2/x.d"]);
    assert_eq!(err_of(&out), EXPECTED);

    let out = run(&home, &repo.join("s1"), &["check-attr", "-a", "s2/x.d"]);
    assert_eq!(err_of(&out), EXPECTED);
}

/// The absolute arm of `prefix_path()`: a work tree reached through a symlink
/// resolves, and a leading directory that does not exist dies naming *that*
/// directory rather than the argument.
#[test]
#[cfg(unix)]
fn absolute_paths_resolve_through_symlinks_and_die_on_missing_directories() {
    let (repo, home) = fixture("abspath");
    let link = repo.parent().unwrap().join("repolink");
    std::os::unix::fs::symlink(&repo, &link).unwrap();

    let through_link = link.join("f.txt");
    let out = run(&home, &repo, &["check-attr", "text", through_link.to_str().unwrap()]);
    assert_eq!(out_of(&out), format!("{}: text: set\n", through_link.display()));
    assert!(out.status.success(), "stderr: {}", err_of(&out));

    let out = run(&home, &repo, &["check-attr", "text", "/zvcs-no-such-root/deeper/x"]);
    assert_eq!(
        err_of(&out),
        "fatal: Invalid path '/zvcs-no-such-root': No such file or directory\n"
    );
    assert_eq!(out.status.code().unwrap(), 128);

    // `strbuf_realpath()` tolerates a missing *last* component, so this one
    // reaches the ordinary refusal instead.
    let missing = repo.parent().unwrap().join("nope");
    let out = run(&home, &repo, &["check-attr", "text", missing.to_str().unwrap()]);
    assert_eq!(
        err_of(&out),
        format!(
            "fatal: '{}' is outside repository at '{}'\n",
            missing.display(),
            repo.display()
        )
    );
    assert_eq!(out.status.code().unwrap(), 128);

    // A die mid-stream keeps the records written for the paths before it.
    let out = run(
        &home,
        &repo,
        &["check-attr", "text", "f.txt", missing.to_str().unwrap()],
    );
    assert_eq!(out_of(&out), "f.txt: text: set\n");
    assert_eq!(out.status.code().unwrap(), 128);
}
