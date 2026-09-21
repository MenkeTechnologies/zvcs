//! `git bisect visualize|view`, pinned against stock git 2.55.0.
//!
//! `bisect_visualize()` (builtin/bisect.c:1148-1183) is one `run_command()` over
//! the bisection range. Without arguments and without a windowing environment it
//! runs `git log --bisect --`; with arguments the first one decides whether the
//! child is a git subcommand or a program looked up on `$PATH`, and a leading
//! dash means `log` with those options.
//!
//! The `BISECT_NAMES` tail is the part most likely to be "fixed" by a port:
//! `strbuf_read_file()` reads the file as it stands and `sq_dequote_to_strvec()`
//! is handed the leading space `sq_quote_argv()` wrote, so it fails at the first
//! character and pushes nothing. `read_bisect_paths()` trims each line before
//! dequoting and therefore does see the pathspec; this caller does not. The
//! visible consequence is that `git bisect visualize` shows the *unfiltered*
//! range even for a session started with a pathspec.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run_at(dir: &Path, home: &Path, args: &[&str], date: Option<&str>) -> Output {
    let stamp = date.unwrap_or("1700000000 +0000");
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
        .env("GIT_AUTHOR_DATE", stamp)
        .env("GIT_COMMITTER_DATE", stamp);
    // The gitk branch is taken only when one of these says there is a display to
    // put a window on; clear them so the test sees the `git log` branch wherever
    // it runs.
    for key in ["DISPLAY", "SESSIONNAME", "MSYSTEM", "SECURITYSESSIONID"] {
        cmd.env_remove(key);
    }
    cmd.output().expect("run binary")
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    run_at(dir, home, args, None)
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn fixture(tag: &str, n: usize) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bpv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    for i in 1..=n {
        std::fs::write(repo.join(format!("f{i}")), "x\n").unwrap();
        let date = format!("{} +0000", 1_700_000_000 + i * 60);
        git(&repo, &home, &["add", "-A"]);
        let msg = format!("c{i}");
        let o = run_at(&repo, &home, &["commit", "-q", "-m", &msg], Some(&date));
        assert!(o.status.success(), "commit {msg}: {}", String::from_utf8_lossy(&o.stderr));
        git(&repo, &home, &["tag", &msg]);
    }
    (root, repo, home)
}

/// With no session there is nothing to show: `bisect_next_check(terms, NULL)`
/// answers without a message and the command fails.
#[test]
fn visualize_without_a_session_is_a_silent_failure() {
    let (root, repo, home) = fixture("unstarted", 3);
    let o = run(&repo, &home, &["bisect", "visualize"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let _ = std::fs::remove_dir_all(&root);
}

/// `visualize` and its alias `view` run `git log --bisect --`, so options land on
/// `log` and the output is the whole bisection range, newest first.
#[test]
fn visualize_runs_git_log_over_the_bisect_range() {
    let (root, repo, home) = fixture("range", 6);
    git(&repo, &home, &["bisect", "start", "c6", "c1"]);

    let expected = "c6\nc5\nc4\nc3\nc2\n";
    for sub in ["visualize", "view"] {
        let o = run(&repo, &home, &["bisect", sub, "--format=%s"]);
        assert!(o.status.success(), "{sub}: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout), expected, "{sub}");
    }

    // A first argument that is not an option and does not name git or tig is
    // taken as a git subcommand, so this is the same walk spelled out.
    let o = run(&repo, &home, &["bisect", "visualize", "log", "--format=%s"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), expected);

    // …and one that is not a subcommand at all fails the way the git wrapper
    // does, with `run_command()`'s status negated into the exit code.
    let o = run(&repo, &home, &["bisect", "visualize", "definitely-not-a-git-command"]);
    assert_eq!(o.status.code(), Some(255), "{:?}", o.status);

    let _ = std::fs::remove_dir_all(&root);
}

/// The pathspec a session was started with never reaches the visualizer, because
/// the file it is read from begins with the space `sq_quote_argv()` wrote.
#[test]
fn the_recorded_pathspec_is_not_passed_to_the_visualizer() {
    let (root, repo, home) = fixture("paths", 6);
    git(&repo, &home, &["bisect", "start", "c6", "c1", "--", "f3"]);

    // The pathspec did reach the *bisection* — `read_bisect_paths()` trims — so
    // the file is there and holds the quoted argument.
    let names = std::fs::read_to_string(repo.join(".git/BISECT_NAMES")).unwrap();
    assert_eq!(names, " '--' 'f3'\n");

    // The visualizer still sees the whole range: had the tail been appended, the
    // walk would have been limited to the single commit that touched `f3`.
    let o = run(&repo, &home, &["bisect", "visualize", "--format=%s"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "c6\nc5\nc4\nc3\nc2\n");

    let _ = std::fs::remove_dir_all(&root);
}
