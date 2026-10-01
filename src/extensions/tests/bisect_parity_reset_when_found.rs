//! `git bisect start|run --reset-when-found[=<where>]`, new in git 2.56.0, and the
//! two other behaviour changes 2.56 made to builtin/bisect.c alongside it.
//!
//! * The mode (`original` when bare, or `found`) is written to
//!   `$GIT_DIR/BISECT_RESET_WHEN_FOUND`. Whichever subcommand then ends on the
//!   first bad commit, `cmd_bisect()` reads it back (builtin/bisect.c:1646-1654)
//!   and runs `checkout --quiet` to the start head or to `refs/bisect/<bad>`, then
//!   cleans the session away. An unknown recorded mode fails the command after the
//!   culprit has been reported.
//! * `--reset-when-found` is refused for a `--no-checkout` session — named on
//!   `start`, recorded as `BISECT_HEAD` for `run`, or implied by a bare repository.
//! * `get_terms(…, 1)` now refuses a `BISECT_TERMS` that runs out before its second
//!   line with `no terms defined` (exit 1, or 255 through a marking word).
//! * `bisect replay` cleans the session state instead of `bisect_reset(NULL)`, so it
//!   no longer checks the old start head out before replaying.
//!
//! Every expectation was measured from stock git 2.56.0 on this fixture; identity
//! and dates are pinned, so the commit ids are literals.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const C3: &str = "400da57470559d2bafcca35fa6439eb6fd556107";
const C4: &str = "9d94480d6f1cc7baa165bbc258c47364340d939c";
const C6: &str = "3388de8e72dcb28750387019bddf8501f78a164a";

fn run(dir: &Path, args: &[&str]) -> Output {
    let home = std::env::temp_dir().join(format!("zvcs-brwf-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `(exit, stdout, stderr)`.
fn res(out: &Output) -> (i32, String, String) {
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `c1`..`c8` on `main`, each rewriting `f` to its number and tagged with its name.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-brwf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.join("r");
    ok(&root, &["init", "-q", "-b", "main", "r"]);
    for i in 1..=8 {
        std::fs::write(repo.join("f"), format!("{i}\n")).unwrap();
        ok(&repo, &["add", "f"]);
        ok(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
        ok(&repo, &["tag", &format!("c{i}")]);
    }
    (root, repo)
}

/// The `BISECT_*` files left in `$GIT_DIR`, sorted.
fn state(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo.join(".git"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("BISECT_"))
        .collect();
    names.sort();
    names
}

fn show(id: &str, n: u32) -> String {
    format!(
        "{id} is the first 'bad' commit\n\
         commit {id}\n\
         Author: A <a@x>\n\
         Date:   Sat Jan 1 00:00:00 2000 +0000\n\
         \n\
         \x20   c{n}\n\
         \n\
         \x20f | 2 +-\n\
         \x201 file changed, 1 insertion(+), 1 deletion(-)\n"
    )
}

const RUN_TO_C6: &str = "running 'sh' '-c' 'test $(cat f) -lt 6'\n\
    Bisecting: 1 revision left to test after this (roughly 1 step)\n\
    [3388de8e72dcb28750387019bddf8501f78a164a] c6\n\
    running 'sh' '-c' 'test $(cat f) -lt 6'\n\
    Bisecting: 0 revisions left to test after this (roughly 0 steps)\n\
    [5eb2805e3919f30855a0b2dbeba952396326071d] c5\n\
    running 'sh' '-c' 'test $(cat f) -lt 6'\n";

/// A bare `--reset-when-found` is `original`: the marking that finds the culprit
/// reports it, then puts `main` back — quietly, so the report is all there is — and
/// leaves no session behind.
#[test]
fn marking_the_culprit_returns_to_the_original_branch() {
    let (_root, repo) = fixture("orig");
    ok(&repo, &["bisect", "start", "--reset-when-found", "c8", "c1"]);
    assert_eq!(std::fs::read_to_string(repo.join(".git/BISECT_RESET_WHEN_FOUND")).unwrap(), "original\n");
    ok(&repo, &["bisect", "bad"]);
    ok(&repo, &["bisect", "bad"]);
    let out = run(&repo, &["bisect", "good"]);
    assert_eq!(res(&out), (0, show(C3, 3), String::new()));
    assert_eq!(ok(&repo, &["symbolic-ref", "HEAD"]), "refs/heads/main\n");
    assert!(state(&repo).is_empty(), "{:?}", state(&repo));
}

/// `=found` on `run` leaves the culprit checked out, detached; `bisect found first`
/// is still printed by the run before the session is wound up.
#[test]
fn run_found_stays_on_the_culprit() {
    let (_root, repo) = fixture("found");
    ok(&repo, &["bisect", "start", "c8", "c1"]);
    let out = run(&repo, &["bisect", "run", "--reset-when-found=found", "sh", "-c", "test $(cat f) -lt 6"]);
    assert_eq!(
        res(&out),
        (0, format!("{RUN_TO_C6}{}bisect found first 'bad' commit\n", show(C6, 6)), String::new())
    );
    assert_eq!(ok(&repo, &["rev-parse", "HEAD"]), format!("{C6}\n"));
    assert!(run(&repo, &["symbolic-ref", "-q", "HEAD"]).status.code() != Some(0));
    assert!(state(&repo).is_empty(), "{:?}", state(&repo));
}

/// The `start` that already ends on the culprit (`c3` bad, its parent good) winds
/// itself up too.
#[test]
fn a_start_that_finds_the_culprit_is_wound_up_at_once() {
    let (_root, repo) = fixture("start");
    let out = run(&repo, &["bisect", "start", "--reset-when-found=found", "c3", "c2"]);
    assert_eq!(res(&out), (0, show(C3, 3), String::new()));
    assert_eq!(ok(&repo, &["rev-parse", "HEAD"]), format!("{C3}\n"));
    assert!(state(&repo).is_empty(), "{:?}", state(&repo));
}

/// Refusals ahead of any state: an unknown `<where>`, and `--no-checkout` in either
/// order.
#[test]
fn start_refuses_an_unknown_mode_and_no_checkout() {
    let (_root, repo) = fixture("refuse");
    let out = run(&repo, &["bisect", "start", "--reset-when-found=bogus", "c8", "c1"]);
    assert_eq!(
        res(&out),
        (1, String::new(), "error: invalid value for '--reset-when-found': 'bogus'\n".to_owned())
    );
    for argv in [
        &["bisect", "start", "--reset-when-found", "--no-checkout", "c8", "c1"][..],
        &["bisect", "start", "--no-checkout", "--reset-when-found=found", "c8", "c1"][..],
    ] {
        let out = run(&repo, argv);
        assert_eq!(
            res(&out),
            (
                1,
                String::new(),
                "error: options '--reset-when-found' and '--no-checkout' cannot be used together\n".to_owned()
            ),
            "{argv:?}"
        );
    }
    assert!(state(&repo).is_empty(), "{:?}", state(&repo));
    assert_eq!(ok(&repo, &["symbolic-ref", "HEAD"]), "refs/heads/main\n");
}

/// `run` takes the option only as its first operand. It refuses a `--no-checkout`
/// session, and records the mode before it checks that a command follows.
#[test]
fn run_refusals_and_the_mode_written_before_the_command_check() {
    let (_root, repo) = fixture("runref");
    ok(&repo, &["bisect", "start", "c8", "c1"]);
    let out = run(&repo, &["bisect", "run", "--reset-when-found=x", "true"]);
    assert_eq!(res(&out), (1, String::new(), "error: invalid value for '--reset-when-found': 'x'\n".to_owned()));
    assert!(!repo.join(".git/BISECT_RESET_WHEN_FOUND").exists());

    let out = run(&repo, &["bisect", "run", "--reset-when-found"]);
    assert_eq!(res(&out), (1, String::new(), "error: bisect run failed: no command provided.\n".to_owned()));
    assert_eq!(std::fs::read_to_string(repo.join(".git/BISECT_RESET_WHEN_FOUND")).unwrap(), "original\n");
    ok(&repo, &["bisect", "reset"]);
    assert!(state(&repo).is_empty(), "reset cleans the mode away too: {:?}", state(&repo));

    ok(&repo, &["bisect", "start", "--no-checkout", "c8", "c1"]);
    let out = run(&repo, &["bisect", "run", "--reset-when-found", "true"]);
    assert_eq!(
        res(&out),
        (
            1,
            String::new(),
            "error: options '--reset-when-found' and '--no-checkout' cannot be used together\n".to_owned()
        )
    );
}

/// A mode `cmd_bisect()` does not know fails the step that found the culprit — after
/// the report — and leaves the session as it was.
#[test]
fn an_unknown_recorded_mode_fails_after_the_report() {
    let (_root, repo) = fixture("weird");
    ok(&repo, &["bisect", "start", "--reset-when-found", "c8", "c1"]);
    std::fs::write(repo.join(".git/BISECT_RESET_WHEN_FOUND"), "weird\n").unwrap();
    let out = run(&repo, &["bisect", "run", "sh", "-c", "test $(cat f) -lt 6"]);
    assert_eq!(
        res(&out),
        (
            1,
            format!("{RUN_TO_C6}{}bisect found first 'bad' commit\n", show(C6, 6)),
            "error: invalid value for '--reset-when-found': 'weird'\n".to_owned()
        )
    );
    assert!(state(&repo).contains(&"BISECT_START".to_owned()));
}

/// A bare repository is a `--no-checkout` session by construction.
#[test]
fn a_bare_repository_refuses_the_option() {
    let (root, repo) = fixture("bare");
    ok(&root, &["clone", "-q", "--bare", repo.to_str().unwrap(), "b.git"]);
    let out = run(&root.join("b.git"), &["bisect", "start", "--reset-when-found", "c8", "c1"]);
    assert_eq!(
        res(&out),
        (
            1,
            String::new(),
            "error: options '--reset-when-found' and '--no-checkout' cannot be used together\n".to_owned()
        )
    );
}

/// A `BISECT_TERMS` with only one line: every entry point that reads the terms with
/// a missing file allowed now says so. A marking word answers through a bare
/// `return error()`, hence 255.
#[test]
fn a_truncated_terms_file_is_no_terms_defined() {
    let (_root, repo) = fixture("terms");
    ok(&repo, &["bisect", "start", "c8", "c1"]);
    std::fs::write(repo.join(".git/BISECT_TERMS"), "bad").unwrap();
    for (argv, code) in [
        (&["bisect", "next"][..], 1),
        (&["bisect", "skip"][..], 1),
        (&["bisect", "visualize"][..], 1),
        (&["bisect", "run", "true"][..], 1),
        (&["bisect", "terms"][..], 1),
        (&["bisect", "good"][..], 255),
    ] {
        let out = run(&repo, argv);
        assert_eq!(res(&out), (code, String::new(), "error: no terms defined\n".to_owned()), "{argv:?}");
    }
    // A second line without its newline is still a second line.
    std::fs::write(repo.join(".git/BISECT_TERMS"), "bad\ngood").unwrap();
    let out = run(&repo, &["bisect", "next"]);
    assert_eq!(
        res(&out),
        (0, format!("Bisecting: 3 revisions left to test after this (roughly 2 steps)\n[{C4}] c4\n"), String::new())
    );
}

/// Replaying inside a running session cleans it and replays from where the worktree
/// is — the session's current commit — instead of first checking `main` back out,
/// so the replayed `start` records that commit as the head to return to.
#[test]
fn replay_no_longer_checks_the_start_head_out_first() {
    let (root, repo) = fixture("replay");
    ok(&repo, &["bisect", "start", "c8", "c1"]);
    let log = root.join("log");
    std::fs::write(&log, ok(&repo, &["bisect", "log"])).unwrap();
    let out = run(&repo, &["bisect", "replay", log.to_str().unwrap()]);
    let step = format!("Bisecting: 3 revisions left to test after this (roughly 2 steps)\n[{C4}] c4\n");
    assert_eq!(res(&out), (0, format!("{step}{step}"), String::new()));
    assert_eq!(std::fs::read_to_string(repo.join(".git/BISECT_START")).unwrap(), format!("{C4}\n"));
    let out = run(&repo, &["bisect", "reset"]);
    assert_eq!(res(&out), (0, String::new(), "HEAD is now at 9d94480 c4\n".to_owned()));
}

/// An empty `BISECT_START` is `strbuf_read_file()` answering 0, which `bisect_reset()`
/// reports on stdout without checking anything out.
#[test]
fn reset_with_an_empty_start_file_is_not_bisecting() {
    let (_root, repo) = fixture("empty");
    ok(&repo, &["bisect", "start"]);
    std::fs::write(repo.join(".git/BISECT_START"), "").unwrap();
    let out = run(&repo, &["bisect", "reset"]);
    assert_eq!(res(&out), (0, "We are not bisecting.\n".to_owned(), String::new()));
    assert!(state(&repo).is_empty(), "{:?}", state(&repo));
}
