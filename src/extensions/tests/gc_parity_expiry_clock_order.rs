//! `repo_config_get_expiry()` (config.c:2486-2490) reads the clock before the value:
//!
//! ```c
//! timestamp_t now = approxidate("now");
//! if (approxidate(*output) >= now)
//!         git_die_config(r, key, _("Invalid %s: '%s'"), key, *output);
//! ```
//!
//! `approxidate("bogus")` resolves to the clock at its own call, which comes second, so it is
//! never earlier than `now` and `gc_config()` (builtin/gc.c:176-189) always refuses it. Read the
//! other way round, a second boundary falling between the two reads let `bogus` through and
//! `gc --auto` exit 0 in silence.
//!
//! The boundary is made certain here rather than waited for: a preloaded shim moves the wall
//! clock one second forward on every read, so each process sees time advance between any two
//! reads. Under it stock git still refuses, and so must zvcs. The shim is built with the host C
//! compiler; without one, or where the loader ignores it, the test has nothing to measure and
//! returns.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/stock_git.rs"]
mod stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[cfg(target_os = "macos")]
const PRELOAD: &str = "DYLD_INSERT_LIBRARIES";
#[cfg(not(target_os = "macos"))]
const PRELOAD: &str = "LD_PRELOAD";

/// Every wall-clock read returns one second later than the previous one.
const SHIM: &str = r#"
#define _GNU_SOURCE
#include <time.h>
#include <sys/time.h>
static long ticks;
static long tick(void) { return __atomic_fetch_add(&ticks, 1, __ATOMIC_SEQ_CST); }
#ifdef __APPLE__
static int shim_clock_gettime(clockid_t id, struct timespec *ts) {
    int r = clock_gettime(id, ts);
    if (!r && id == CLOCK_REALTIME) ts->tv_sec += tick();
    return r;
}
static int shim_gettimeofday(struct timeval *tv, void *tz) {
    int r = gettimeofday(tv, tz);
    if (!r && tv) tv->tv_sec += tick();
    return r;
}
static time_t shim_time(time_t *t) {
    time_t now = time(NULL) + tick();
    if (t) *t = now;
    return now;
}
__attribute__((used, section("__DATA,__interpose")))
static struct { const void *replacement, *original; } interposers[] = {
    { (const void *)shim_clock_gettime, (const void *)clock_gettime },
    { (const void *)shim_gettimeofday, (const void *)gettimeofday },
    { (const void *)shim_time, (const void *)time },
};
#else
#include <dlfcn.h>
int clock_gettime(clockid_t id, struct timespec *ts) {
    static int (*real)(clockid_t, struct timespec *);
    if (!real) real = (int (*)(clockid_t, struct timespec *))dlsym(RTLD_NEXT, "clock_gettime");
    int r = real(id, ts);
    if (!r && id == CLOCK_REALTIME) ts->tv_sec += tick();
    return r;
}
int gettimeofday(struct timeval *tv, void *tz) {
    static int (*real)(struct timeval *, void *);
    if (!real) real = (int (*)(struct timeval *, void *))dlsym(RTLD_NEXT, "gettimeofday");
    int r = real(tv, tz);
    if (!r && tv) tv->tv_sec += tick();
    return r;
}
time_t time(time_t *t) {
    struct timeval tv;
    gettimeofday(&tv, 0);
    if (t) *t = tv.tv_sec;
    return tv.tv_sec;
}
#endif
"#;

/// Two `CLOCK_REALTIME` reads in one process, printed as seconds.
const PROBE: &str = r#"
#include <stdio.h>
#include <time.h>
int main(void) {
    struct timespec a, b;
    clock_gettime(CLOCK_REALTIME, &a);
    clock_gettime(CLOCK_REALTIME, &b);
    printf("%ld\n", (long)(b.tv_sec - a.tv_sec));
    return 0;
}
"#;

fn scratch() -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-gc-expiry-clock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn cc(args: &[&str]) -> bool {
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    Command::new(compiler).args(args).output().is_ok_and(|o| o.status.success())
}

/// The shim's path, once a probe run under it has seen the clock jump.
fn shim(root: &Path) -> Option<PathBuf> {
    let (src, lib) = (root.join("tick.c"), root.join("tick.so"));
    let (probe_src, probe) = (root.join("probe.c"), root.join("probe"));
    std::fs::write(&src, SHIM).unwrap();
    std::fs::write(&probe_src, PROBE).unwrap();
    let mut shared = vec!["-shared", "-fPIC", "-o", lib.to_str()?, src.to_str()?];
    if cfg!(target_os = "linux") {
        shared.push("-ldl");
    }
    if !cc(&shared) || !cc(&["-o", probe.to_str()?, probe_src.to_str()?]) {
        return None;
    }
    let out = Command::new(&probe).env(PRELOAD, &lib).output().ok()?;
    let jump: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    (jump >= 1).then_some(lib)
}

fn git(bin: &str, repo: &Path, home: &Path, preload: Option<&Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("GIT_TEST_DATE_NOW");
    if let Some(lib) = preload {
        cmd.env(PRELOAD, lib);
    }
    cmd.output().unwrap()
}

fn repo(root: &Path, name: &str, home: &Path) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("f"), "one\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "f"],
        &["-c", "user.name=A", "-c", "user.email=a@example.com", "commit", "-qm", "c0"],
    ] {
        let out = git(BIN, &repo, home, None, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    repo
}

#[test]
fn a_bogus_gc_expiry_is_refused_when_the_clock_moves_between_reads() {
    let root = scratch();
    let Some(lib) = shim(&root) else {
        return;
    };
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let zvcs_repo = repo(&root, "zvcs", &home);
    let stock = stock_git::stock_git_at_least((2, 56, 0)).map(|bin| (bin, repo(&root, "stock", &home)));

    for key in ["gc.pruneexpire", "gc.worktreepruneexpire", "gc.logexpiry"] {
        let assignment = format!("{key}=bogus");
        let args = ["-c", assignment.as_str(), "gc", "--auto"];
        let want = format!(
            "error: Invalid {key}: 'bogus'\nfatal: unable to parse '{key}' from command-line config\n"
        );

        let got = git(BIN, &zvcs_repo, &home, Some(&lib), &args);
        assert_eq!(
            (String::from_utf8_lossy(&got.stderr).into_owned(), got.status.code()),
            (want.clone(), Some(128)),
            "zvcs {args:?}"
        );
        if let Some((bin, stock_repo)) = &stock {
            let out = git(bin, stock_repo, &home, Some(&lib), &args);
            assert_eq!(
                (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code()),
                (want, Some(128)),
                "stock {args:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
