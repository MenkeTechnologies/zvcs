//! `core.lockfilePid` — the `~pid.lock` companion, from the config read that
//! turns it on to the diagnosis it buys when a lock cannot be taken.
//!
//! git 2.55.0 keeps this in one process global and two places that touch it
//! (`lockfile.c`):
//!
//! ```c
//! /* Global config variable, initialized from core.lockfilePid */
//! int lockfile_pid_enabled;                                  /* lockfile.c:90-91  */
//!
//! lk->tempfile = repo_create_tempfile_mode(r, lock_path.buf, mode);
//! if (lk->tempfile)
//!         lk->pid_tempfile = create_lock_pid_file(pid_path.buf, mode);  /* :182-184 */
//!
//! int commit_lock_file(struct lock_file *lk) {
//!         char *result_path = get_locked_file_path(lk);
//!         delete_tempfile(&lk->pid_tempfile);                /* :352-356 */
//! ```
//!
//! and `environment.c:532-535` is the only assignment of the global:
//!
//! ```c
//! if (!strcmp(var, "core.lockfilepid")) {
//!         lockfile_pid_enabled = git_config_bool(var, value);
//!         return 0;
//! }
//! ```
//!
//! Three properties follow, and each has a test below.
//!
//! 1. **Writing is gated on the key, and on the lock having been taken.** The
//!    companion is created only inside `if (lk->tempfile)`, so a contended lock
//!    never leaves one behind claiming a hold it does not have.
//! 2. **Reading is not gated on the key.** `unable_to_lock_message()` tries the
//!    companion path unconditionally — "it may exist if core.lockfilePid was
//!    enabled" (`lockfile.c:269-272`) for the process that took the lock, which is
//!    not the process now reporting the failure.
//! 3. **The companion never outlives its lock.** `commit_lock_file()` deletes it
//!    before persisting and `rollback_lock_file()` deletes it too
//!    (`lockfile.c:352-372`), so a successful command leaves nothing behind.
//!
//! Property 3 is also why there is no end-to-end assertion here that a *running*
//! `git` has a companion on disk: every verb that takes one of these locks either
//! commits or rolls back within the same process, and both paths remove it. Stock
//! git behaves identically — measured with git 2.55.0, `git -c
//! core.lockfilePid=true update-ref refs/heads/probe HEAD` leaves no `~pid.lock`
//! in `.git/refs/heads/`. The presence-while-held property is therefore asserted
//! against a lock this test itself holds open, which is the only moment it is
//! observable.
//!
//! Every expectation was measured against stock git 2.55.0; the diagnostic
//! sentences are quoted from `lockfile.c:280-292`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `lockfile_pid_enabled` is a process global in C and a process global here, so
/// two tests assigning it in the same test binary would race. Every test that
/// touches it holds this first.
fn serialized() -> MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A scratch directory of our own, since this crate carries no `tempfile`
/// dev-dependency.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-lockfile-pid-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// The PATH with this repository's shim removed, so a nested call cannot reach
/// the installed `zvcs` instead of the binary under test.
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Run the binary under test with every config source outside the repository
/// pinned away, so a developer's own `core.lockfilePid` cannot decide a result.
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .env("PATH", real_git_path())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

/// An initialised repository with one commit, so `HEAD` resolves.
fn repo_with_a_commit(name: &str) -> PathBuf {
    let dir = scratch(name);
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "one"]);
    dir
}

// ---------------------------------------------------------------------------
// The mechanism: lockfile.c:182-184 and lockfile.c:352-372
// ---------------------------------------------------------------------------

/// `create_lock_pid_file()` returns immediately unless the global is set
/// (`lockfile.c:118-119`), so the companion exists beside a held lock only with
/// the key on — and carries exactly the `pid %ju\n` line `lockfile.c:125` writes.
#[test]
fn the_companion_sits_beside_a_held_lock_only_when_the_key_is_on() {
    let _guard = serialized();
    let dir = scratch("held");
    let resource = dir.join("index");
    let lock = dir.join("index.lock");
    let companion = dir.join("index~pid.lock");

    gix::lock::pid::set_enabled(false);
    let off = gix::lock::File::acquire_to_update_resource(
        &resource,
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .expect("uncontended lock");
    assert!(lock.exists(), "the lock itself is always taken");
    assert!(!companion.exists(), "no companion with the key off");
    drop(off);

    gix::lock::pid::set_enabled(true);
    let on = gix::lock::File::acquire_to_update_resource(
        &resource,
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .expect("uncontended lock");
    assert!(companion.exists(), "the companion appears with the key on");
    assert_eq!(
        std::fs::read_to_string(&companion).unwrap(),
        format!("pid {}\n", std::process::id()),
        "`strbuf_addf(&content, \"pid %\" PRIuMAX \"\\n\", getpid())` — lockfile.c:125"
    );

    // `rollback_lock_file()` deletes the companion as well as the lock
    // (`lockfile.c:368-371`); dropping the handle is that rollback.
    drop(on);
    assert!(!companion.exists(), "rollback removes the companion");
    assert!(!lock.exists(), "rollback removes the lock");

    gix::lock::pid::set_enabled(false);
}

/// `commit_lock_file()` calls `delete_tempfile(&lk->pid_tempfile)` *before*
/// `commit_lock_file_to()` (`lockfile.c:352-358`), so a committed lock leaves the
/// resource in place and nothing else.
#[test]
fn committing_the_lock_leaves_no_companion_behind() {
    let _guard = serialized();
    let dir = scratch("commit");
    let resource = dir.join("index");
    let companion = dir.join("index~pid.lock");

    gix::lock::pid::set_enabled(true);
    let mut file = gix::lock::File::acquire_to_update_resource(
        &resource,
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .expect("uncontended lock");
    assert!(companion.exists(), "held, so the companion is there");

    std::io::Write::write_all(&mut file, b"contents\n").unwrap();
    let (written, _) = file.commit().expect("commit the lock");

    assert_eq!(written, resource);
    assert_eq!(std::fs::read_to_string(&resource).unwrap(), "contents\n");
    assert!(!companion.exists(), "commit removes the companion");
    assert!(!dir.join("index.lock").exists(), "commit consumes the lock");

    gix::lock::pid::set_enabled(false);
}

// ---------------------------------------------------------------------------
// The diagnosis: lockfile.c:254-300
// ---------------------------------------------------------------------------

/// The three sentences of `unable_to_lock_message()` (`lockfile.c:280-292`),
/// each reached by what the companion beside the contended lock says.
///
/// The key is left **off** throughout, because `unable_to_lock_message()` reads
/// the companion unconditionally — it belongs to whoever took the lock, not to
/// the process reporting the failure (`lockfile.c:269-272`).
#[test]
fn a_failed_acquisition_reports_what_the_companion_says() {
    let _guard = serialized();
    let dir = scratch("diagnose");
    let resource = dir.join("index");
    let companion = dir.join("index~pid.lock");
    gix::lock::pid::set_enabled(false);

    // Somebody else holds it.
    std::fs::write(dir.join("index.lock"), "").unwrap();

    let attempt = || -> String {
        gix::lock::File::acquire_to_update_resource(
            &resource,
            gix::lock::acquire::Fail::Immediately,
            None,
        )
        .expect_err("the lock is held")
        .to_string()
    };

    // No companion — `pid_status` stays 0 and git falls back.
    assert!(
        attempt().ends_with(
            "Another git process seems to be running in this repository, \
             or the lock file may be stale"
        ),
        "unqualified fallback, got: {}",
        attempt()
    );

    // A PID that cannot be running: `kill(pid, 0)` returns `ESRCH`, so
    // `pid_status == -1` and the lock is reported stale. 4194303 is one past
    // 4194304, the maximum `kernel.pid_max` on 64-bit Linux, and above macOS's
    // 99998 — no live process can carry it.
    std::fs::write(&companion, "pid 4194303\n").unwrap();
    assert!(
        attempt().ends_with(
            "Lock was held by process 4194303, which is no longer running; \
             the lock file appears to be stale"
        ),
        "stale-holder sentence, got: {}",
        attempt()
    );

    // A PID that certainly is running: our own. `kill` succeeds, `pid_status == 1`.
    let me = std::process::id();
    std::fs::write(&companion, format!("pid {me}\n")).unwrap();
    assert!(
        attempt().ends_with(&format!(
            "Lock may be held by process {me}; if no git process is running, \
             the lock file may be stale (PIDs can be reused)"
        )),
        "live-holder sentence, got: {}",
        attempt()
    );

    // A companion that exists but does not parse warns and yields no PID
    // (`lockfile.c:159-160`), which lands back on the fallback sentence.
    std::fs::write(&companion, "garbage\n").unwrap();
    assert!(
        attempt().ends_with(
            "Another git process seems to be running in this repository, \
             or the lock file may be stale"
        ),
        "malformed companion falls back, got: {}",
        attempt()
    );
}

/// `read_lock_pid()`'s grammar (`lockfile.c:141-165`), including the two inputs
/// that must stay silent and the ones that must warn.
///
/// `gix-lock` lives under `src/ported`, which the workspace excludes, so its own
/// `#[cfg(test)]` module never runs under `cargo test` here. These assertions are
/// the ones that do.
#[test]
fn read_lock_pid_accepts_only_gits_own_spelling() {
    let dir = scratch("readpid");
    let at = dir.join("p");

    // Absent and empty return without warning: `strbuf_read_file(..) <= 0` jumps
    // past the `malformed` warning to `out` (`lockfile.c:147-148`).
    assert_eq!(gix::lock::pid::read_lock_pid(&at), None);
    std::fs::write(&at, "").unwrap();
    assert_eq!(gix::lock::pid::read_lock_pid(&at), None);

    // The spelling `create_lock_pid_file()` writes, with `strbuf_rtrim()`
    // taking the trailing newline off.
    std::fs::write(&at, "pid 1234\n").unwrap();
    assert_eq!(gix::lock::pid::read_lock_pid(&at), Some(1234));
    std::fs::write(&at, "pid 1234").unwrap();
    assert_eq!(gix::lock::pid::read_lock_pid(&at), Some(1234), "the newline is optional");

    // `strtoumax` is permissive in ways `create_lock_pid_file()` never exercises,
    // and the port matches rather than tightens. Each of these was measured
    // against stock git 2.55.0 by planting the companion beside a held
    // `refs/heads/main.lock` and reading the sentence `update-ref` printed.
    for (content, expected) in [
        ("pid  12\n", 12),                        // leading space, skipped
        ("pid +12\n", 12),                        // leading sign
        ("pid -12\n", 18446744073709551604),      // negation wraps in unsigned
    ] {
        std::fs::write(&at, content).unwrap();
        assert_eq!(
            gix::lock::pid::read_lock_pid(&at),
            Some(expected),
            "git accepts {content:?} as {expected}"
        );
    }

    // `*pid_out > 0` rejects zero, `!*endptr` rejects a trailing tail, and the
    // prefix is matched by `skip_prefix(content, "pid ")` — case-sensitively,
    // with the space required. All five warn `malformed lock pid file` under
    // stock git and yield no PID.
    for malformed in ["pid 0\n", "pid 12x\n", "pid\n", "pid12\n", "1234\n", "PID 1234\n"] {
        std::fs::write(&at, malformed).unwrap();
        assert_eq!(
            gix::lock::pid::read_lock_pid(&at),
            None,
            "should reject {malformed:?}"
        );
    }
}

/// The non-`EEXIST` branch of `unable_to_lock_message()` is one line, names the
/// resource with `.lock` glued on by the format string, and carries no holder
/// paragraph (`lockfile.c:296-299`).
#[test]
fn a_non_contention_error_gets_gits_one_line_form() {
    let dir = scratch("oneline");
    let resource = dir.join("index");
    let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);

    let msg = gix::lock::pid::unable_to_lock_message(&resource, &err);
    assert!(
        msg.starts_with(&format!("Unable to create '{}.lock': ", resource.display())),
        "got: {msg}"
    );
    assert!(!msg.contains("\n\n"), "no holder paragraph in the one-line form: {msg}");

    // The `EEXIST` branch is the two-paragraph one, and its `strerror` renders
    // bare — `File exists`, not Rust's `File exists (os error 17)`.
    let held = std::io::Error::from_raw_os_error(
        std::fs::write(&resource, "x")
            .and_then(|()| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&resource)
            })
            .expect_err("the resource already exists")
            .raw_os_error()
            .expect("a real errno"),
    );
    let msg = gix::lock::pid::unable_to_lock_message(&resource, &held);
    assert!(
        msg.starts_with(&format!("Unable to create '{}.lock': File exists.\n\n", resource.display())),
        "bare strerror and the blank line, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// The config read: environment.c:532-535
// ---------------------------------------------------------------------------

/// `crate::default_config::validate` is what the dispatcher runs for a verb whose
/// callback is `git_default_config` — the `core.lockfilePid` arm inside it is the
/// port of `environment.c:532-535`, and this asserts it really assigns the global.
///
/// Reverting that arm to plain validation (which is what it used to be — the key
/// sat in the `bool_value(v, key)?;` group with the other booleans) fails this
/// test on the first assertion.
#[test]
fn validating_the_config_assigns_the_lockfile_pid_global() {
    let _guard = serialized();
    let dir = repo_with_a_commit("configread");
    let config = dir.join(".git/config");

    let with_key = |value: &str| {
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str(&format!("[core]\n\tlockfilePid = {value}\n"));
        std::fs::write(&config, text).unwrap();
    };
    let reread = || {
        let repo = gix::discover(&dir).expect("discover the repository");
        // `Rejection` carries git's own diagnostic rather than a `Debug` form,
        // so surface that text if a value this test believes is well-formed is
        // somehow refused.
        if let Err(refusal) = zvcs::default_config::validate(&repo) {
            panic!("a well-formed value was refused: {}", refusal.into_fatal());
        }
        gix::lock::pid::enabled()
    };

    // Absent: the global keeps whatever it had, exactly as a C global untouched
    // by a callback that never fired. Off is the value a fresh process starts on.
    gix::lock::pid::set_enabled(false);
    assert!(!reread(), "absent key leaves the global alone");

    // git's boolean grammar, in both directions and every spelling
    // `git_config_bool` accepts (config.c:1292-1298 → `git_parse_maybe_bool`).
    for on in ["true", "yes", "on", "1"] {
        std::fs::write(&config, "[core]\n\trepositoryformatversion = 0\n").unwrap();
        gix::lock::pid::set_enabled(false);
        with_key(on);
        assert!(reread(), "`{on}` turns the companion on");
    }
    for off in ["false", "no", "off", "0"] {
        std::fs::write(&config, "[core]\n\trepositoryformatversion = 0\n").unwrap();
        gix::lock::pid::set_enabled(true);
        with_key(off);
        assert!(!reread(), "`{off}` turns the companion off");
    }

    // The valueless spelling is `true`: `git_config_bool` maps a NULL value to 1
    // (config.c:1292-1298), which `bool_value` reproduces.
    std::fs::write(&config, "[core]\n\trepositoryformatversion = 0\n").unwrap();
    gix::lock::pid::set_enabled(false);
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[core]\n\tlockfilePid\n");
    std::fs::write(&config, text).unwrap();
    assert!(reread(), "a valueless key is true");

    // Last occurrence wins, because the walk is in parse order and every
    // occurrence assigns.
    std::fs::write(&config, "[core]\n\trepositoryformatversion = 0\n").unwrap();
    gix::lock::pid::set_enabled(false);
    with_key("true");
    with_key("false");
    assert!(!reread(), "the last assignment stands");

    gix::lock::pid::set_enabled(false);
}

/// The refusal, through the real binary. `bool_value` routes to
/// `git_config_bool` (config.c:1292-1298), whose message carries no origin
/// clause — so these bytes are identical whether the value arrived by `-c`, by
/// file, or by environment.
///
/// Measured from stock git 2.55.0:
///
/// ```text
/// $ git -c core.lockfilePid=bogus status --short
/// fatal: bad boolean config value 'bogus' for 'core.lockfilepid'
/// ```
#[test]
fn a_bad_value_dies_the_way_git_dies() {
    let dir = repo_with_a_commit("badvalue");

    let out = git(&dir, &["-c", "core.lockfilePid=bogus", "status", "--short"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: bad boolean config value 'bogus' for 'core.lockfilepid'\n"
    );
    assert_eq!(out.status.code(), Some(128));

    // A value git accepts must not disturb the verb at all, in either direction.
    for value in ["true", "false"] {
        let out = git(&dir, &["-c", &format!("core.lockfilePid={value}"), "status", "--short"]);
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "",
            "`{value}` is a clean run"
        );
        assert_eq!(out.status.code(), Some(0), "`{value}` exits 0");
    }
}
