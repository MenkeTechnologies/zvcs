//! How `git bisect replay` sizes up its log before opening it.
//!
//! `bisect_replay()` (builtin/bisect.c:1198-1212) asks
//! `is_empty_or_missing_file()` (wrapper.c:815-826) first: a `stat()` that
//! fails with `ENOENT`, or a zero `st_size`, is `error: cannot read file '%s'
//! for replaying` at exit 1, and any other `stat()` failure is
//! `die_errno(_("could not stat %s"))` — the name unquoted — at exit 128.
//! zvcs quoted the name. `/dev/stdin` is a log like any other: what decides it
//! is the `st_size` of whatever descriptor 0 is, so a regular file redirected
//! in replays and an empty one is refused. Every case runs both binaries on
//! identical fixtures and compares stdout, stderr, exit code and the
//! `refs/bisect/*` left behind.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, args: &[&str], stdin: Option<&Path>) -> (String, String, i32) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(match stdin {
            Some(p) => Stdio::from(File::open(p).expect("open stdin file")),
            None => Stdio::null(),
        });
    let out = cmd.output().expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A five-commit line with `side` at the second commit, built by stock git so
/// both binaries start from the same objects.
fn fixture(root: &Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    run(STOCK, &dir, &["init", "-q", "-b", "main"], None);
    for n in 1..=5 {
        std::fs::write(dir.join(format!("f{n}")), format!("{n}\n")).unwrap();
        run(STOCK, &dir, &["add", "."], None);
        run(STOCK, &dir, &["commit", "-q", "-m", &format!("c{n}")], None);
        if n == 2 {
            run(STOCK, &dir, &["branch", "side"], None);
        }
    }
    dir
}

/// Run `args` through both binaries on fresh copies of the fixture and return
/// each side's answer plus its `refs/bisect/*` listing.
fn both(root: &Path, tag: &str, args: &[&str], stdin: Option<&Path>) -> [(String, String, i32, String); 2] {
    [STOCK, ZVCS].map(|bin| {
        let side = if bin == STOCK { "stock" } else { "zvcs" };
        let dir = fixture(root, &format!("{tag}-{side}"));
        let got = run(bin, &dir, args, stdin);
        let refs = run(STOCK, &dir, &["for-each-ref", "refs/bisect"], None).0;
        (got.0, got.1, got.2, refs)
    })
}

#[test]
fn replay_log_that_cannot_be_stat_ed_dies_with_the_name_unquoted() {
    if !stock_available() {
        return;
    }
    let root = std::env::temp_dir().join(format!("zvcs-bisect-replay-stat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // ENOTDIR: a path component is a regular file.
    let [stock, zvcs] = both(&root, "notdir", &["bisect", "replay", "f1/log"], None);
    assert_eq!(stock.1, "fatal: could not stat f1/log: Not a directory\n");
    assert_eq!(stock.2, 128);
    assert_eq!(zvcs, stock);

    // ENAMETOOLONG: one component past NAME_MAX.
    let long = "l".repeat(300);
    let [stock, zvcs] = both(&root, "toolong", &["bisect", "replay", &long], None);
    assert_eq!(stock.2, 128);
    assert_eq!(zvcs, stock);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn replay_dev_stdin_is_sized_by_the_descriptor_behind_it() {
    if !stock_available() {
        return;
    }
    let root = std::env::temp_dir().join(format!("zvcs-bisect-replay-stdin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("replay.log");
    std::fs::write(&log, "git bisect start\ngit bisect bad main\ngit bisect good side\n").unwrap();
    let empty = root.join("empty.log");
    std::fs::write(&empty, "").unwrap();

    let [stock, zvcs] = both(&root, "full", &["bisect", "replay", "/dev/stdin"], Some(&log));
    assert_eq!(stock.2, 0);
    assert!(stock.0.starts_with("status: waiting for both 'good' and 'bad' commits\n"), "{stock:?}");
    assert!(stock.3.contains("refs/bisect/bad"), "{stock:?}");
    assert_eq!(zvcs, stock);

    // A zero `st_size` is refused before `bisect_clean_state()` runs, exactly
    // like a missing file.
    let [stock, zvcs] = both(&root, "empty", &["bisect", "replay", "/dev/stdin"], Some(&empty));
    assert_eq!(stock.1, "error: cannot read file '/dev/stdin' for replaying\n");
    assert_eq!(stock.2, 1);
    assert_eq!(zvcs, stock);

    let _ = std::fs::remove_dir_all(&root);
}
