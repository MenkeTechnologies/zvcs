//! `git fetch --refetch` hints the auto-maintenance child to consolidate the duplicate pack it
//! leaves behind (builtin/fetch.c:2868-2886): `gc.autoPackLimit=1` and
//! `maintenance.incremental-repack.auto=-1` are pushed into `GIT_CONFIG_PARAMETERS` unless the
//! repository sets the key to `0`, and a value that is not an integer dies first. Captured
//! from git 2.55.0; the child's view is read back through trace2 `def_param` records.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["-c", "maintenance.autoDetach=false"])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env_remove("GIT_CONFIG_PARAMETERS");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-refetch-hints-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    for args in [&["init", "-q", "-b", "main", "up"][..], &["-C", "up", "commit", "-q", "--allow-empty", "-m", "c0"], &["clone", "-q", "up", "dn"]] {
        let out = run(&root, &home, args, &[]);
        assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    (root, home)
}

/// The `(param, value)` pairs trace2 recorded for `gc.autopacklimit` and
/// `maintenance.incremental-repack.auto` in any process of the run, and how many
/// `maintenance` children were started.
fn traced(root: &Path, home: &Path, args: &[&str]) -> (Vec<String>, usize) {
    let trace = root.join(format!("trace-{}.json", args.len()));
    let _ = std::fs::remove_file(&trace);
    let mut full = vec!["-C", "dn"];
    full.extend_from_slice(args);
    let out = run(
        root,
        home,
        &full,
        &[
            ("GIT_TRACE2_EVENT", trace.as_path()),
            ("GIT_TRACE2_CONFIG_PARAMS", Path::new("gc.autopacklimit,maintenance.incremental-repack.auto")),
        ],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&trace).unwrap_or_default();
    let mut params: Vec<String> = text
        .lines()
        .filter(|l| l.contains("\"event\":\"def_param\""))
        .filter_map(|l| {
            let param = l.split("\"param\":\"").nth(1)?.split('"').next()?;
            let value = l.split("\"value\":\"").nth(1)?.split('"').next()?;
            Some(format!("{param}={value}"))
        })
        .collect();
    params.sort();
    params.dedup();
    let children = text.lines().filter(|l| l.contains("\"event\":\"start\"") && l.contains("\"maintenance\",\"run\"")).count();
    (params, children)
}

#[test]
fn the_maintenance_child_of_a_refetch_sees_both_hints() {
    let (root, home) = fixture("hints");
    let (params, children) = traced(&root, &home, &["fetch", "-q", "--refetch", "origin"]);
    assert_eq!(children, 1);
    assert_eq!(params, ["gc.autopacklimit=1", "maintenance.incremental-repack.auto=-1"]);

    // A `0` switches its own hint off and leaves the other.
    let (params, _) = traced(&root, &home, &["-c", "gc.autoPackLimit=0", "fetch", "-q", "--refetch", "origin"]);
    assert_eq!(params, ["gc.autopacklimit=0", "maintenance.incremental-repack.auto=-1"]);

    // No hint without `--refetch`, and `--dry-run` does not skip the child.
    let (params, _) = traced(&root, &home, &["fetch", "-q", "origin"]);
    assert!(params.is_empty(), "{params:?}");
    let (_, children) = traced(&root, &home, &["fetch", "-q", "--dry-run", "--refetch", "origin"]);
    assert_eq!(children, 1);
}

#[test]
fn a_non_integer_value_dies_before_maintenance_runs() {
    let (root, home) = fixture("die");
    let out = run(&root, &home, &["-C", "dn", "-c", "maintenance.incremental-repack.auto=bogus", "-c", "gc.autoPackLimit=bogus", "fetch", "--refetch", "origin"], &[]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: bad numeric config value 'bogus' for 'gc.autopacklimit': invalid unit\n"
    );
    assert_eq!(out.status.code(), Some(128));

    let out = run(&root, &home, &["-C", "dn", "-c", "maintenance.incremental-repack.auto=bogus", "fetch", "--refetch", "origin"], &[]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: bad numeric config value 'bogus' for 'maintenance.incremental-repack.auto': invalid unit\n"
    );
    assert_eq!(out.status.code(), Some(128));

    // Neither key is read without auto-maintenance.
    let out = run(&root, &home, &["-C", "dn", "-c", "gc.autoPackLimit=bogus", "fetch", "--refetch", "--no-auto-maintenance", "origin"], &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
