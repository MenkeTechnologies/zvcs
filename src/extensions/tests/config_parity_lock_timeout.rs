//! The config writers take `<file>.lock` before they read the file, and git 2.56
//! retries a lock another process holds for `core.configLockTimeout` milliseconds
//! (config.c:2966-2982, default 1000) through `lock_file_timeout()`'s backoff
//! (lockfile.c:206-253).
//!
//! * `repo_config_set_multivar_in_file_gently()` reports a lock it could not take
//!   with `error_errno()` — `could not lock config file <f>: <strerror>` — and
//!   `repo_config_copy_or_rename_section_in_file()` with plain `error()`, no errno
//!   (config.c:3067-3071, 3413-3418). Both exit 255 from `git config`.
//! * The timeout is read on every write, before the lock is attempted, so a value
//!   that is not an `int` dies even when nothing holds the lock.
//! * A rename against a file that does not exist still commits the lock, leaving
//!   an empty file behind (config.c:3421-3427).
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-config-lock-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    assert!(run(&repo, &["init", "-q"]).status.success());
    repo
}

#[test]
fn a_held_lock_is_refused_with_the_writers_own_wording() {
    let repo = fixture("held");
    let lock = repo.join(".git/config.lock");
    std::fs::write(&lock, "").unwrap();
    let before = std::fs::read(repo.join(".git/config")).unwrap();

    for args in [&["x.y", "z"][..], &["--unset", "x.y"], &["set", "x.y", "z"]] {
        let mut argv = vec!["-c", "core.configLockTimeout=0", "config"];
        argv.extend_from_slice(args);
        let out = run(&repo, &argv);
        assert_eq!(out.status.code(), Some(255), "{args:?}");
        assert_eq!(stderr(&out), "error: could not lock config file .git/config: File exists\n", "{args:?}");
    }
    for args in [&["--rename-section", "x", "y"][..], &["--remove-section", "x"], &["rename-section", "x", "y"]] {
        let mut argv = vec!["-c", "core.configLockTimeout=0", "config"];
        argv.extend_from_slice(args);
        let out = run(&repo, &argv);
        assert_eq!(out.status.code(), Some(255), "{args:?}");
        assert_eq!(stderr(&out), "error: could not lock config file .git/config\n", "{args:?}");
    }
    assert_eq!(std::fs::read(repo.join(".git/config")).unwrap(), before, "the config is untouched");
    assert!(lock.exists(), "a lock this process did not take is never removed");

    // The default timeout retries for a second before giving up.
    let start = Instant::now();
    let out = run(&repo, &["config", "x.y", "z"]);
    assert_eq!(out.status.code(), Some(255));
    assert!(start.elapsed() >= Duration::from_millis(1000), "{:?}", start.elapsed());

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_lock_released_within_the_timeout_is_taken() {
    let repo = fixture("released");
    let lock = repo.join(".git/config.lock");
    std::fs::write(&lock, "").unwrap();
    let releaser = {
        let lock = lock.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            std::fs::remove_file(lock).unwrap();
        })
    };
    // `-1` retries for as long as it takes.
    let out = run(&repo, &["-c", "core.configLockTimeout=-1", "config", "x.y", "q"]);
    releaser.join().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&run(&repo, &["config", "x.y"]).stdout), "q\n");
    assert!(!lock.exists(), "the committed lock is renamed over the config");

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn an_unparsable_timeout_dies_on_every_write() {
    let repo = fixture("bad");
    let out = run(&repo, &["-c", "core.configLockTimeout=abc", "config", "x.y", "z"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'abc' for 'core.configlocktimeout': invalid unit\n"
    );

    // Setting it is a write that reads the old value first, which is still unset.
    assert!(run(&repo, &["config", "core.configLockTimeout", "abc"]).status.success());
    let out = run(&repo, &["config", "x.y", "w"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr(&out),
        "fatal: bad numeric config value 'abc' for 'core.configlocktimeout' in file .git/config: invalid unit\n"
    );
    // Reads never consult it.
    assert_eq!(run(&repo, &["config", "core.configLockTimeout"]).status.code(), Some(0));

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn renaming_in_a_missing_file_leaves_an_empty_one() {
    let repo = fixture("rename-missing");
    let file = repo.join("nf");
    let out = run(&repo, &["config", "--file", "nf", "--rename-section", "a", "b"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stderr(&out), "fatal: no such section: a\n");
    assert_eq!(std::fs::read(&file).unwrap(), b"", "the committed lock is the new file");

    let _ = std::fs::remove_dir_all(&repo);
}
