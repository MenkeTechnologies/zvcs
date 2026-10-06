//! When and how `git clone` judges its `--depth` value.
//!
//! `--depth` is an `OPT_STRING`: `cmd_clone()` checks the operand count first
//! (129), then the source (`repository '%s' does not exist`), and only then
//! `atoi(option_depth) < 1` — `depth %s is not a positive number`
//! (builtin/clone.c:1017-1066). `atoi()` reads ` 2` and `+2` as 2 and `3x` as 3;
//! the transport then takes the value through `strtol(value, &end, 0)` and dies
//! `transport: invalid depth option` on trailing junk, after the `Cloning into`
//! banner and the local-clone warning (transport.c:261-268). zvcs judged the value
//! while parsing options with a strict decimal parse, so a bad depth beat every
//! one of those and ` 2`/`+2` were refused. Expectations measured from stock git
//! 2.56.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CEILING_DIRECTORIES", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@x")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run the binary under test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn the_depth_value_is_judged_after_the_operands_and_by_atoi() {
    let root = std::env::temp_dir().join(format!("zvcs-clone-depth-value-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("up")).unwrap();
    git(&root.join("up"), &["init", "-q", "-b", "main"]);
    git(&root.join("up"), &["commit", "-q", "--allow-empty", "-m", "a"]);
    let url = format!("file://{}/up", root.canonicalize().unwrap().display());

    let (_, err, code) = git(&root, &["clone", "--depth=0", "a", "b", "c"]);
    assert!(err.starts_with("fatal: Too many arguments.\n"), "{err}");
    assert_eq!(code, 129);
    assert_eq!(
        git(&root, &["clone", "--depth=0", "nosuch", "x"]),
        (String::new(), "fatal: repository 'nosuch' does not exist\n".into(), 128)
    );
    assert_eq!(
        git(&root, &["clone", "-q", "--depth=0", &url, "z"]),
        (String::new(), "fatal: depth 0 is not a positive number\n".into(), 128)
    );
    assert_eq!(
        git(&root, &["clone", "--depth=3x", &url, "j"]),
        (String::new(), "Cloning into 'j'...\nfatal: transport: invalid depth option '3x'\n".into(), 128)
    );
    assert!(!root.join("j").exists());
    for (value, dir) in [(" 2", "s"), ("+2", "p")] {
        let (_, err, code) = git(&root, &["clone", "-q", &format!("--depth={value}"), &url, dir]);
        assert_eq!((err.as_str(), code), ("", 0), "--depth={value:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
