//! `git pull` over a diverged branch with no integration strategy chosen.
//!
//! `show_advice_pull_non_ff()` (builtin/pull.c:840-854) is a single `advise()`
//! call, so its lines go through `vadvise()` (advice.c:106-118), which frames
//! each one as `hint: ` and wraps the whole line — prefix included — in
//! `color.advice.hint` whenever `want_color_stderr(advice_use_color)` allows
//! it (advice.c:41-47). A blank line in the body becomes a bare `hint:` with no
//! trailing space.
//!
//! The port wrote the lines out itself, so the block was correct in plain text
//! and silently uncolored under `color.advice=always` — the case
//! `t7601-merge-pull-config.sh`'s `pull.rebase not set (not-fast-forward)`
//! decodes.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, stdout, stderr and
//! exit status compared separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[m";

const BODY: [&str; 12] = [
    "You have divergent branches and need to specify how to reconcile them.",
    "You can do so by running one of the following commands sometime before",
    "your next pull:",
    "",
    "  git config pull.rebase false  # merge",
    "  git config pull.rebase true   # rebase",
    "  git config pull.ff only       # fast-forward only",
    "",
    "You can replace \"git config\" with \"git config --global\" to set a default",
    "preference for all repositories. You can also pass --rebase, --no-rebase,",
    "or --ff-only on the command line to override the configured default per",
    "invocation.",
];

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `main` on `c2`, with `c1` a sibling commit off the shared root `c0` — so
    /// pulling `c1` can neither fast-forward nor be already up to date.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-pull-diverge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "1\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "c0"]);
        f.git(&["tag", "c0"]);
        std::fs::write(f.work.join("f"), "2\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "c1"]);
        f.git(&["tag", "c1"]);
        f.git(&["reset", "-q", "--hard", "c0"]);
        std::fs::write(f.work.join("f"), "3\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "c2"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
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
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// `color.advice=always`: every line of the block is painted, the blank ones
/// as a bare `hint:` with the span still closed around it.
#[test]
fn the_diverging_pull_advice_is_colored_when_color_advice_says_so() {
    let f = Fixture::new("color");
    let (_, err, code) = f.run(&["-c", "color.advice=always", "pull", ".", "c1"]);
    assert_eq!(code, 128);

    let want: String = BODY
        .iter()
        .map(|line| match line.is_empty() {
            true => format!("{YELLOW}hint:{RESET}\n"),
            false => format!("{YELLOW}hint: {line}{RESET}\n"),
        })
        .collect();
    let hints: String =
        err.lines().filter(|l| l.contains("hint:")).map(|l| format!("{l}\n")).collect();
    assert_eq!(hints, want);
    assert!(err.ends_with("fatal: Need to specify how to reconcile divergent branches.\n"), "{err:?}");
}

/// `color.advice.hint` picks the sequence, as it does for every other hint.
#[test]
fn the_hint_slot_is_configurable() {
    let f = Fixture::new("slot");
    let (_, err, _) =
        f.run(&["-c", "color.advice=always", "-c", "color.advice.hint=blue", "pull", ".", "c1"]);
    assert!(err.contains("\x1b[34mhint: You have divergent branches"), "{err:?}");
}

/// Without the knob nothing is painted — stderr is not a terminal here, which
/// is what `want_color_stderr()` asks — and the plain block is unchanged.
#[test]
fn the_diverging_pull_advice_is_plain_by_default() {
    let f = Fixture::new("plain");
    let (_, err, code) = f.run(&["pull", ".", "c1"]);
    assert_eq!(code, 128);
    assert!(!err.contains('\x1b'), "{err:?}");
    for line in BODY {
        let want = match line.is_empty() {
            true => "\nhint:\n".to_string(),
            false => format!("\nhint: {line}\n"),
        };
        assert!(err.contains(&want), "missing {want:?} in {err:?}");
    }
}
