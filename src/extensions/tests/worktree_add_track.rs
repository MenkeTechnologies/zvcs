//! `git worktree add --track` / `--no-track`.
//!
//! Both were refused outright, with a reason that could not apply to the negative
//! spelling: the refusal cited remote-tracking DWIM, and `--no-track` is the
//! option that turns that off. In git they are one `OPT_PASSTHRU`
//! (`builtin/worktree.c:819-821`) pushed onto the child `git branch`'s command
//! line verbatim (`:946-947`), so the mode's meaning belongs to `git branch` and
//! the only things `worktree add` decides are the two refusals around it.
//!
//! Three details here were measured rather than assumed, and each is the opposite
//! of the obvious guess:
//!
//! * The option carries `PARSE_OPT_NOARG` as well as `PARSE_OPT_OPTARG`, so
//!   `--track=direct` is a parse-options error, not a mode selector — `worktree
//!   add` cannot express `direct`/`inherit` at all, though `git branch` can.
//! * `--no-track` with no `-b` **succeeds**, because the DWIM branch named after
//!   the path counts as a new branch for the passthru's purposes.
//! * `--orphan --no-track` reports `options '--orphan' and '--track' cannot be
//!   used together` — naming `--track` even though `--no-track` was given.
//!
//! Every expectation below was measured against git 2.55.0 first. Fixtures are
//! built with the zvcs binary itself, so nothing needs a stock git on PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity and date vars git honors above config. CI exports some of these for
/// the whole job, which would change the commit these fixtures build.
const PINNED: [&str; 6] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
];

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    for v in PINNED {
        c.env_remove(v);
    }
    c.args(args)
        .current_dir(dir)
        // This machine's ~/.gitconfig sets core.commentChar; an unpinned run
        // measures a different config than the one under test.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn err(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn out_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A single-commit repo on `main`, plus a scratch root the worktrees go under.
fn repo(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-wtt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let r = root.join("r");
    std::fs::create_dir_all(&r).unwrap();
    ok(&r, &["init", "-q", "-b", "main", "."]);
    std::fs::write(r.join("f.txt"), "a\n").unwrap();
    ok(&r, &["add", "f.txt"]);
    ok(&r, &["commit", "-q", "-m", "base"]);
    (root, r)
}

fn config(dir: &Path, key: &str) -> Option<String> {
    let out = git(dir, &["config", "--get", key]);
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `--track` reaches `git branch`'s tracking setup: config is written and the
/// child's line lands on stdout, between `Preparing worktree` (stderr) and the
/// checkout's own line.
#[test]
fn track_sets_up_the_upstream_and_says_so_on_stdout() {
    let (root, r) = repo("track");
    let wt = root.join("w");
    let out = git(&r, &["worktree", "add", "-b", "nb", "--track", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        out_str(&out).contains("branch 'nb' set up to track 'main'."),
        "the child's tracking line belongs on stdout, got stdout {:?} stderr {:?}",
        out_str(&out),
        err(&out)
    );
    assert_eq!(config(&r, "branch.nb.remote").as_deref(), Some("."));
    assert_eq!(config(&r, "branch.nb.merge").as_deref(), Some("refs/heads/main"));
}

/// `--no-track` is the whole point of the passthru being two-valued: it must
/// suppress the tracking that `branch.autoSetupMerge` would otherwise set up, and
/// say nothing while doing it.
#[test]
fn no_track_suppresses_the_upstream_silently() {
    let (root, r) = repo("notrack");
    let wt = root.join("w");
    // `branch.autoSetupMerge=always` makes the default track even a local branch,
    // so without --no-track this fixture *would* write config. That is what makes
    // this test discriminating rather than vacuous.
    ok(&r, &["config", "branch.autoSetupMerge", "always"]);
    let out = git(&r, &["worktree", "add", "-b", "nb", "--no-track", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        !out_str(&out).contains("set up to track"),
        "--no-track must not announce tracking, got {:?}",
        out_str(&out)
    );
    assert_eq!(config(&r, "branch.nb.remote"), None);
    assert_eq!(config(&r, "branch.nb.merge"), None);
}

/// The same fixture without `--no-track` does write the config. This is the
/// control for the test above; if it ever stops tracking, that test stops proving
/// anything.
#[test]
fn the_same_add_without_no_track_does_set_up_tracking() {
    let (root, r) = repo("notrack-control");
    let wt = root.join("w");
    ok(&r, &["config", "branch.autoSetupMerge", "always"]);
    let out = git(&r, &["worktree", "add", "-b", "nb", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(config(&r, "branch.nb.merge").as_deref(), Some("refs/heads/main"));
}

/// No `-b`, no `<commit-ish>`: the branch named after the path is a new branch, so
/// the passthru has a child to reach and the add succeeds. Measured on stock,
/// which exits 0 here — the obvious guess is that this is the "no new branch"
/// case, and it is not.
#[test]
fn no_track_is_accepted_for_the_path_dwim_branch() {
    let (root, r) = repo("dwim");
    let wt = root.join("wdwim");
    let out = git(&r, &["worktree", "add", "--no-track", wt.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(err(&out).contains("Preparing worktree (new branch 'wdwim')"), "{}", err(&out));
    assert_eq!(config(&r, "branch.wdwim.merge"), None);
}

/// A `<commit-ish>` naming an existing branch creates nothing, so there is no
/// child to hand the passthru to. The fatal comes *after* the `Preparing worktree`
/// line, which is where git prints it.
#[test]
fn track_without_a_new_branch_is_refused_after_the_preparing_line() {
    for flag in ["--track", "--no-track"] {
        let (root, r) = repo(&format!("nonew{}", flag.trim_start_matches('-')));
        let wt = root.join("w");
        let out = git(&r, &["worktree", "add", flag, wt.to_str().unwrap(), "main"]);

        assert_eq!(code(&out), 128, "{flag}: {}", err(&out));
        assert_eq!(
            err(&out),
            "Preparing worktree (checking out 'main')\n\
             fatal: --[no-]track can only be used if a new branch is created\n",
            "{flag}"
        );
        assert!(!wt.exists(), "{flag}: nothing should have been created");
    }
}

/// `PARSE_OPT_NOARG`: an `=value` is rejected by parse-options itself, with the
/// bare error line and no usage block. `git branch` accepts `--track=direct`;
/// `worktree add` cannot, and that asymmetry is git's, not an oversight here.
#[test]
fn an_explicit_track_mode_is_a_parse_options_error() {
    for spec in ["--track=direct", "--track=inherit", "--track=bogus"] {
        let (root, r) = repo(&format!("mode{}", spec.len()));
        let wt = root.join("w");
        let out = git(&r, &["worktree", "add", spec, "-b", "nb", wt.to_str().unwrap()]);

        assert_eq!(code(&out), 129, "{spec}: {}", err(&out));
        assert_eq!(err(&out), "error: option `track' takes no value\n", "{spec}");
    }
}

/// `--orphan` is checked against the passthru before anything else, so the
/// combination reports itself rather than `--orphan`'s own unported floor — and it
/// names `--track` whichever spelling was given. Both argv orders, since the check
/// is on the parsed flags rather than on position.
#[test]
fn orphan_with_either_track_spelling_reports_the_combination() {
    for args in [
        vec!["--orphan", "--no-track"],
        vec!["--no-track", "--orphan"],
        vec!["--orphan", "--track"],
    ] {
        let (root, r) = repo(&format!("orphan{}", args.join("")));
        let wt = root.join("w");
        let mut argv = vec!["worktree", "add"];
        argv.extend(args.iter().copied());
        argv.push(wt.to_str().unwrap());
        let out = git(&r, &argv);

        assert_eq!(code(&out), 128, "{args:?}: {}", err(&out));
        assert_eq!(
            err(&out),
            "fatal: options '--orphan' and '--track' cannot be used together\n",
            "{args:?}"
        );
    }
}
