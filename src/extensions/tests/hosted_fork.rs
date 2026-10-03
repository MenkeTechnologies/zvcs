//! A host that forks: the `git` builtin dispatched in a `fork()` child of a
//! multithreaded host, as zshrs-native does for every pipeline element but the
//! last.
//!
//! In-process, that child crashed on macOS the first time the https transport
//! reached Security.framework — `objc[PID]: +[NSNumber initialize] may have
//! been in progress in another thread when fork() was called ... Crashing
//! instead.` — and a crashed writer left its `index.lock` behind. These tests
//! pin the two halves of the fix without a network: the forked copy serves the
//! verb from an exec'd `git` and reports that child's status faithfully, and an
//! aborting zvcs removes the lock files it holds.
//!
//! Each test forks the test binary itself, which libtest has already made
//! multithreaded — the same shape as the host.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-hosted-fork-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("zvcs-stub");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Fork, run `child` in the copy and `_exit` with its answer; return the raw
/// wait status.
fn in_fork(child: impl FnOnce() -> i32) -> libc::c_int {
    // Safety: the child only runs `child` and then `_exit`s, never returning
    // into the test harness.
    match unsafe { libc::fork() } {
        -1 => panic!("fork: {}", std::io::Error::last_os_error()),
        0 => {
            let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(child)).unwrap_or(101);
            unsafe { libc::_exit(code) }
        }
        pid => {
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            status
        }
    }
}

fn exit_code(status: libc::c_int) -> i32 {
    assert!(libc::WIFEXITED(status), "child did not exit normally: {status:#x}");
    libc::WEXITSTATUS(status)
}

#[test]
fn the_host_itself_is_not_a_forked_copy_and_its_child_is() {
    assert!(!zvcs::hosted::forked_from_host());
    let status = in_fork(|| i32::from(zvcs::hosted::forked_from_host()));
    assert_eq!(exit_code(status), 1, "the load-time pid must survive into the fork child unchanged");
}

#[test]
fn a_forked_copy_serves_the_verb_from_an_exec_d_git() {
    let dir = scratch("delegate");
    let marker = dir.join("argv");
    let stub = script(&dir, &format!("printf '%s\\n' \"$@\" > '{}'\nexit 7", marker.display()));
    let status = in_fork(|| {
        std::env::set_var("ZVCS_GIT_EXE", &stub);
        zvcs::run_argv(&["git".into(), "push".into(), "-q".into(), "origin".into(), "main".into()])
    });
    assert_eq!(exit_code(status), 7, "the exec'd git's status is the builtin's status");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "push\n-q\norigin\nmain\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_killed_delegate_is_reported_as_128_plus_the_signal_never_as_success() {
    let dir = scratch("killed");
    let stub = script(&dir, "kill -ABRT $$");
    let status = in_fork(|| {
        std::env::set_var("ZVCS_GIT_EXE", &stub);
        zvcs::run_argv(&["git".into(), "pull".into()])
    });
    assert_eq!(exit_code(status), 128 + libc::SIGABRT);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_abort_removes_the_index_lock_it_holds() {
    let dir = scratch("abort");
    let index = dir.join("index");
    let lock = dir.join("index.lock");
    let status = in_fork(|| {
        zvcs::remove_lock_files_on_abort();
        let held = gix::lock::File::acquire_to_update_resource(
            &index,
            gix::lock::acquire::Fail::Immediately,
            None,
        )
        .expect("lock taken");
        assert!(lock.exists());
        std::mem::forget(held);
        std::process::abort()
    });
    assert!(libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGABRT, "status {status:#x}");
    assert!(!lock.exists(), "an aborted writer left {} behind", lock.display());
    let _ = std::fs::remove_dir_all(&dir);
}
