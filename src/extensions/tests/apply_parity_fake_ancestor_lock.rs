//! `--build-fake-ancestor=<file>` takes `<file>.lock` with `LOCK_DIE_ON_ERROR`.
//!
//! `build_fake_ancestor()` ends with `hold_lock_file_for_update(&lock, state->fake_ancestor,
//! LOCK_DIE_ON_ERROR)` (apply.c), so a lock it cannot create is a `fatal:` at 128 naming the
//! absolute `<file>.lock` and the errno — a missing directory is `No such file or directory`, an
//! existing lock gets the one-line hint. zvcs let the index writer fail on its own, and the
//! dispatcher read that failure as lock contention: it printed `index is locked by another
//! writer — queueing`, queued the command as a job and exited 0.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

const CREATE: &[u8] = b"diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..3b18e51\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello world\n";

fn git(bin: &str, dir: &Path, stdin: &[u8], args: &[&str]) -> (String, String, i32) {
    let root = dir.ancestors().find(|p| p.ends_with("work")).unwrap().parent().unwrap();
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(root.to_str().unwrap(), "<root>");
    (clean(&out.stdout), clean(&out.stderr), out.status.code().expect("no signal"))
}

fn repo(label: &str, bin: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-apply-fakelock-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(bin, &work, b"", &["init", "-q", "-b", "main", "."]);
    work
}

/// Run `apply --build-fake-ancestor=<target>` from `subdir` of both worlds, after `prep` ran there.
fn same(label: &str, subdir: &str, target: &str, prep: impl Fn(&Path)) -> (String, String, i32) {
    let stock = stock_git::stock_git().expect("checked by caller");
    let s = repo(&format!("{label}-stock"), stock);
    let z = repo(&format!("{label}-zvcs"), ZVCS);
    prep(&s);
    prep(&z);
    let arg = format!("--build-fake-ancestor={target}");
    let want = git(stock, &s.join(subdir), CREATE, &["apply", &arg]);
    let got = git(ZVCS, &z.join(subdir), CREATE, &["apply", &arg]);
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "apply {arg} from {subdir:?}: left is zvcs, right is stock");
    want
}

#[test]
fn a_missing_directory_is_fatal_not_a_queued_job() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let want = same("nodir", ".git/refs/heads", ".git/no-such-dir/fake", |_| {});
    assert_eq!(want.2, 128);
    assert!(want.1.contains("fake.lock': No such file or directory"), "{want:?}");
}

#[test]
fn an_existing_lock_is_fatal_with_the_stale_lock_hint() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let want = same("held", ".", "fake", |work| std::fs::write(work.join("fake.lock"), "").unwrap());
    assert_eq!(want.2, 128);
    assert!(want.1.contains("File exists."), "{want:?}");
}

#[test]
fn a_free_path_writes_the_index_and_leaves_no_lock() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let want = same("free", ".", "fake", |_| {});
    assert_eq!(want.2, 0, "{want:?}");
}
