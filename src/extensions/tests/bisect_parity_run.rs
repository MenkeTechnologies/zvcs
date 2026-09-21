//! `git bisect run`'s driving loop, pinned against stock git 2.55.0.
//!
//! Three things in `bisect_run()` (builtin/bisect.c:1236-1333) are easy to get
//! subtly wrong and invisible unless they are asserted directly:
//!
//!   * `verify_good()`'s answer is rejected by `rc < 0 || 128 <= rc`, so a
//!     command that dies of a signal on the known-good revision ends the run —
//!     it is not compared against the first run's 126/127 and then treated as a
//!     verdict.
//!   * a failing step is reported as `'git bisect <term>' exited with error code
//!     <negative>`: `res` is the `bisect_error` enum, whose members are negative,
//!     while the process exits with its negation.
//!   * git writes its progress with `printf()` and its refusals with `error()`,
//!     so a caller that captures both streams into one file sees the stdout half
//!     held in the stdio buffer until `exit()` — the `[<oid>] <subject>` line
//!     `verify_good()` prints on its way back lands *after* the `error:` line
//!     that follows it in program order.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn base(dir: &Path, home: &Path, args: &[&str], date: Option<&str>) -> Command {
    let stamp = date.unwrap_or("1700000000 +0000").to_string();
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp);
    cmd
}

fn run_at(dir: &Path, home: &Path, args: &[&str], date: Option<&str>) -> Output {
    base(dir, home, args, date).output().expect("run binary")
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    run_at(dir, home, args, None)
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// Run with both streams pointed at one file, the way a shell's `2>&1 >f` does,
/// and hand back what landed there in order.
fn run_merged(dir: &Path, home: &Path, args: &[&str], sink: &Path) -> (Option<i32>, String) {
    let file = std::fs::File::create(sink).unwrap();
    let err = file.try_clone().unwrap();
    let status = base(dir, home, args, None)
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err))
        .status()
        .expect("run binary");
    (status.code(), std::fs::read_to_string(sink).unwrap())
}

fn commit(repo: &Path, home: &Path, i: usize) {
    std::fs::write(repo.join(format!("f{i}")), "x\n").unwrap();
    let date = format!("{} +0000", 1_700_000_000 + i * 60);
    git(repo, home, &["add", "-A"]);
    let msg = format!("c{i}");
    let o = run_at(repo, home, &["commit", "-q", "-m", &msg], Some(&date));
    assert!(o.status.success(), "commit {msg}: {}", String::from_utf8_lossy(&o.stderr));
    git(repo, home, &["tag", &msg]);
}

fn fixture(tag: &str, n: usize) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bpr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    for i in 1..=n {
        commit(&repo, &home, i);
    }
    (root, repo, home)
}

/// Write an executable `sh` script into the worktree. It stays untracked, so it
/// survives every checkout the bisection performs.
fn script(repo: &Path, name: &str, body: &str) {
    let path = repo.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Whether this platform delivers a signalled death through the wrapper
/// `do_bisect_run()` uses — `sh -c <command> <command>`, which is
/// `prepare_shell_cmd()`'s shape (bisect.rs, run-command.c).
///
/// A `/bin/sh` that exec's the script leaves the script's own process as the
/// direct child, so killing it is `WIFSIGNALED` for the caller. A `/bin/sh` that
/// forks and waits instead reports the job itself — printing `Terminated` — and
/// then exits 143 normally, so the signal never reaches zvcs and the
/// `died of signal` line cannot be produced by any implementation. Observed on
/// the GitHub ubuntu and macos runners; not reproducible on macOS bash 3.2,
/// where the probe below answers true.
///
/// Probing is the honest way to tell those apart: the rest of the case still
/// runs everywhere, and the one assertion that depends on the platform being
/// able to produce the condition is skipped visibly rather than silently.
#[cfg(unix)]
fn shell_forwards_a_signalled_death(dir: &Path) -> bool {
    use std::os::unix::process::ExitStatusExt;
    let probe = dir.join("probe-signal.sh");
    std::fs::write(&probe, "#!/bin/sh\nkill -TERM $$\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("./probe-signal.sh")
        .arg("./probe-signal.sh")
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn the signal probe");
    let _ = std::fs::remove_file(&probe);
    status.signal().is_some()
}

/// A command that exits 127 on the commit under test and then dies of a signal on
/// the known-good revision: the second answer is not a verdict, so the run ends.
#[test]
#[cfg(unix)]
fn a_verification_that_dies_of_a_signal_ends_the_run() {
    let (root, repo, home) = fixture("verify", 15);
    // The good end is c1, which is the only commit without `f2`.
    script(&repo, "s.sh", "if test -f f2; then exit 127; fi\nkill -TERM $$");

    git(&repo, &home, &["bisect", "start", "c15", "c1"]);
    let o = run(&repo, &home, &["bisect", "run", "./s.sh"]);
    assert_eq!(o.status.code(), Some(1), "{:?}", o.status);
    let err = String::from_utf8_lossy(&o.stderr);
    if shell_forwards_a_signalled_death(&repo) {
        assert!(err.contains("error: './s.sh' died of signal 15\n"), "{err}");
    } else {
        // The platform reported the job itself and exited normally, so no
        // implementation can see WIFSIGNALED here. Everything below still holds:
        // a non-verdict answer ends the run without recording anything.
        eprintln!(
            "note: /bin/sh does not forward a signalled death through `sh -c`; \
             skipping only the `died of signal` line"
        );
    }
    assert!(
        err.contains("error: unable to verify './s.sh' on 'good' revision\n"),
        "{err}"
    );
    // No verdict was recorded: the log still holds only what `start` wrote.
    let log = std::fs::read_to_string(repo.join(".git/BISECT_LOG")).unwrap();
    assert_eq!(log.lines().filter(|l| l.starts_with("git bisect ")).count(), 1, "{log}");

    let _ = std::fs::remove_dir_all(&root);
}

/// A step that fails inside `bisect_state()` is reported with the term that was
/// being applied and the negative `bisect_error`, while the process exits with
/// its magnitude.
#[test]
fn a_failing_step_names_the_term_and_the_negative_error_code() {
    let (root, repo, home) = fixture("mergebase", 3);
    // A side branch off c1, so `a` and `c3` have a merge base neither side has
    // marked and the first step has to test it.
    git(&repo, &home, &["checkout", "-q", "-b", "side", "c1"]);
    commit(&repo, &home, 90);
    git(&repo, &home, &["checkout", "-q", "main"]);
    script(&repo, "bad.sh", "exit 1");

    // The merge base c1 is checked out for testing, which is an early success.
    let start = run(&repo, &home, &["bisect", "start", "c3", "c90"]);
    assert!(start.status.success(), "{}", String::from_utf8_lossy(&start.stderr));
    assert!(
        String::from_utf8_lossy(&start.stdout).contains("Bisecting: a merge base must be tested"),
        "{}",
        String::from_utf8_lossy(&start.stdout)
    );

    // Calling that merge base bad is `BISECT_MERGE_BASE_CHECK` (-3).
    let o = run(&repo, &home, &["bisect", "run", "./bad.sh"]);
    assert_eq!(o.status.code(), Some(3), "{:?}", o.status);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("error: bisect run failed: 'git bisect bad' exited with error code -3\n"),
        "{err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// With both streams captured together, the stdout half comes out in stdio's
/// order, not in the order the lines were produced.
#[test]
#[cfg(unix)]
fn a_captured_run_orders_its_streams_the_way_stdio_does() {
    let (root, repo, home) = fixture("order", 15);
    script(&repo, "s.sh", "exit 127");

    git(&repo, &home, &["bisect", "start", "c15", "c1"]);
    let sink = root.join("out");
    let (code, text) = run_merged(&repo, &home, &["bisect", "run", "./s.sh"], &sink);
    assert_eq!(code, Some(1), "{text}");

    // `verify_good()` checks out the good end, re-runs the command there, gets 127
    // again and moves back — so the last `[<oid>] <subject>` line is printed
    // before the `error:` line. stdio holds it until exit, which puts it after.
    let bogus = text
        .lines()
        .position(|l| l == "error: bogus exit code 127 for 'good' revision")
        .unwrap_or_else(|| panic!("no refusal in:\n{text}"));
    let moved_back = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with('['))
        .map(|(i, _)| i)
        .last()
        .unwrap_or_else(|| panic!("no checkout line in:\n{text}"));
    assert!(bogus < moved_back, "refusal should precede the buffered line:\n{text}");

    let _ = std::fs::remove_dir_all(&root);
}
