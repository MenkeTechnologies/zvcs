//! What `merge_working_tree()` always prints, and what the two halves of
//! `cmd_checkout()` always refuse.
//!
//! `builtin/checkout.c` splits on one question — is `opts->pathspec.nr` zero? —
//! and the two answers reject *different* option combinations with *different*
//! wording. `checkout_branch()` refuses `--[no]-overlay`, `--ours/--theirs`,
//! `-f` with `-m` and `--detach` with `-b/-B/--orphan`; `checkout_paths()`
//! refuses `--track`, `-l`, and `--merge` with `--patch`. An option that lands
//! in neither list is not "harmless": it is accepted, ignored, and the worktree
//! and index still change at exit 0, which is the one failure a user cannot see.
//!
//! The listing is the other half. `merge_working_tree()` ends with
//!
//! ```c
//! if (!opts->discard_changes && !opts->quiet && new_branch_info->commit)
//!         show_local_changes(&new_branch_info->commit->object, &opts->diff_options);
//! ```
//!
//! outside the two-way merge, so it runs for every switch that names an operand
//! — `--orphan` included, and identical-tree switches included. An autostashed
//! `-m` prints a *second*, headed listing instead, after
//! `update_refs_for_switch()` has announced the switch.
//!
//! Every expectation below was measured against git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    /// `main`, `same` (an identical-tree branch) and `other` (which rewrites
    /// `f.txt`'s middle line). `g.txt` is identical everywhere.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-cogates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("f.txt"), "l1\nl2\nl3\n").unwrap();
        std::fs::write(f.work.join("g.txt"), "g\n").unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["branch", "same"]);
        f.git(&["checkout", "-q", "-b", "other"]);
        std::fs::write(f.work.join("f.txt"), "l1\nOTHER\nl3\n").unwrap();
        f.git(&["commit", "-q", "-am", "other"]);
        f.git(&["checkout", "-q", "main"]);
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
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("LC_ALL", "C");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.work.join(name)).unwrap()
    }

    /// Put an uncommitted edit into `f.txt` and return to a clean `main`.
    fn dirty(&self) {
        self.git(&["checkout", "-q", "-f", "main"]);
        std::fs::write(self.work.join("f.txt"), "l1\nLOCAL\nl3\n").unwrap();
    }

    fn head_symref(&self) -> String {
        self.run(&["symbolic-ref", "-q", "HEAD"]).1.trim().to_string()
    }

    fn ls_files(&self) -> String {
        self.run(&["ls-files", "-s"]).1
    }
}

/// A refusal is only a refusal if nothing moved. Every case here is one stock
/// rejects at 128 before touching anything, and each was measured to leave the
/// dirty `f.txt`, the index and `HEAD` exactly as they were.
#[test]
fn refused_option_combinations_change_nothing() {
    let f = Fixture::new("refuse");

    // (argv, exact stderr) — git 2.55.0, verbatim.
    let cases: &[(&[&str], &str)] = &[
        // `checkout_branch()` (builtin/checkout.c:1667-1699).
        (
            &["checkout", "--overlay", "other"],
            "fatal: '--[no]-overlay' cannot be used with switching branches\n",
        ),
        (
            &["checkout", "--no-overlay", "other"],
            "fatal: '--[no]-overlay' cannot be used with switching branches\n",
        ),
        (
            &["checkout", "--ours", "other"],
            "fatal: '--ours/--theirs' cannot be used with switching branches\n",
        ),
        (&["checkout", "-f", "-m", "other"], "fatal: '-f' cannot be used with '-m'\n"),
        (
            &["checkout", "--detach", "-b", "nb"],
            "fatal: '--detach' cannot be used with '-b/-B/--orphan'\n",
        ),
        (
            &["checkout", "--orphan", "o", "-t"],
            "fatal: '--orphan' cannot be used with '-t'\n",
        ),
        // `--no-track` is `BRANCH_TRACK_NEVER`, not `BRANCH_TRACK_UNSPECIFIED`,
        // so it collides with `--orphan` exactly as `-t` does.
        (
            &["checkout", "--orphan", "o", "--no-track"],
            "fatal: '--orphan' cannot be used with '-t'\n",
        ),
        // `cmd_checkout()` itself (builtin/checkout.c:1926-1931).
        (
            &["checkout", "-b", "x", "-B", "y"],
            "fatal: options '-b', '-B', and '--orphan' cannot be used together\n",
        ),
        (
            &["checkout", "-p", "--overlay", "--", "f.txt"],
            "fatal: options '-p' and '--overlay' cannot be used together\n",
        ),
        // `checkout_paths()` (builtin/checkout.c:530-551) and :2031.
        (
            &["checkout", "-l", "--", "f.txt"],
            "fatal: '-l' cannot be used with updating paths\n",
        ),
        (
            &["checkout", "-m", "-p", "--", "f.txt"],
            "fatal: options '--merge' and '--patch' cannot be used together\n",
        ),
        (
            &["checkout", "--detach", "--", "f.txt"],
            "fatal: git checkout: --detach does not take a path argument 'f.txt'\n",
        ),
        // `--track` with nothing to name a branch after (:1964-1975).
        (&["checkout", "--track", "--", "f.txt"], "fatal: --track needs a branch name\n"),
        (&["checkout", "--no-track"], "fatal: --track needs a branch name\n"),
        // `restore`'s tri-state targets (:1933-1943 then :554): naming *either*
        // flag in *either* sense turns the other off, so `--no-worktree` alone
        // leaves both off.
        (
            &["restore", "--no-worktree", "f.txt"],
            "fatal: neither '--staged' or '--worktree' is specified\n",
        ),
        (
            &["restore", "--no-worktree", "--no-staged", "f.txt"],
            "fatal: neither '--staged' or '--worktree' is specified\n",
        ),
        (
            &["restore", "--ignore-unmerged", "-m", "f.txt"],
            "fatal: options '--ignore-unmerged' and '-m' cannot be used together\n",
        ),
        // A boolean-only optional value (submodule.c's parser).
        (
            &["checkout", "--recurse-submodules=bogus", "other"],
            "fatal: bad recurse-submodules argument: bogus\n",
        ),
    ];

    for (argv, want_err) in cases {
        f.dirty();
        let index_before = f.ls_files();
        let (code, out, err) = f.run(argv);
        assert_eq!(code, 128, "`git {argv:?}` should exit 128: {out:?} {err:?}");
        assert_eq!(&err, want_err, "`git {argv:?}` stderr");
        assert_eq!(out, "", "`git {argv:?}` should print nothing on stdout");
        // Nothing moved: worktree, index and HEAD are untouched.
        assert_eq!(f.read("f.txt"), "l1\nLOCAL\nl3\n", "`git {argv:?}` touched the worktree");
        assert_eq!(f.ls_files(), index_before, "`git {argv:?}` touched the index");
        assert_eq!(f.head_symref(), "refs/heads/main", "`git {argv:?}` moved HEAD");
    }
}

/// The closing `show_local_changes()` is outside the two-way merge, so it runs
/// even where the two trees are identical and nothing was checked out — and for
/// `--orphan`, whose start-point commit is its own `new_branch_info->commit`.
///
/// The gate is `!opts->discard_changes && !opts->quiet && new_branch_info->commit`,
/// which is why `-f` and `-q` print nothing, and why a switch that names no
/// operand at all (`do_merge = 0`) prints nothing either.
#[test]
fn the_local_changes_listing_survives_an_identical_tree() {
    let f = Fixture::new("listing");

    // (argv, expected stdout).
    let cases: &[(&[&str], &str)] = &[
        // Identical tree, so the two-way merge has nothing to do — and still lists.
        (&["switch", "same"], "M\tf.txt\n"),
        (&["switch", "--detach", "same"], "M\tf.txt\n"),
        (&["switch", "-c", "n1", "same"], "M\tf.txt\n"),
        (&["switch", "-C", "n2", "main"], "M\tf.txt\n"),
        (&["checkout", "same"], "M\tf.txt\n"),
        (&["checkout", "--detach", "same"], "M\tf.txt\n"),
        (&["checkout", "-b", "n3", "main"], "M\tf.txt\n"),
        // `git checkout --orphan` keeps its start-point commit, so it lists too.
        (&["checkout", "--orphan", "o1"], "M\tf.txt\n"),
        // `opts->discard_changes` and `opts->quiet` each suppress it.
        (&["checkout", "-f", "--orphan", "o2"], ""),
        (&["checkout", "-q", "--orphan", "o3"], ""),
        (&["checkout", "-f", "same"], ""),
        (&["switch", "-q", "same"], ""),
        // No operand → `only_merge_on_switching_branches` sets `do_merge = 0`,
        // so `merge_working_tree()` never runs and nothing is listed.
        (&["switch", "-c", "n4"], ""),
        (&["switch", "--detach"], ""),
    ];

    for (argv, want_out) in cases {
        f.dirty();
        let (code, out, err) = f.run(argv);
        assert_eq!(code, 0, "`git {argv:?}`: {out:?} {err:?}");
        assert_eq!(&out, want_out, "`git {argv:?}` stdout");
    }
}

/// `-f` on the `--orphan` path is `opts->discard_changes`, which routes
/// `merge_working_tree()` through `reset_tree()` — the local edit is thrown
/// away, not carried. Accepting the flag and carrying the edit anyway leaves the
/// worktree in a state the user asked git to destroy, at exit 0, silently.
#[test]
fn force_on_orphan_discards_the_local_edit() {
    let f = Fixture::new("orphanforce");

    f.dirty();
    let (code, _, err) = f.run(&["checkout", "--orphan", "o1"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(f.read("f.txt"), "l1\nLOCAL\nl3\n", "a plain --orphan carries the edit");

    f.dirty();
    let (code, out, err) = f.run(&["checkout", "-f", "--orphan", "o2"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "", "-f suppresses the listing");
    assert_eq!(f.read("f.txt"), "l1\nl2\nl3\n", "-f must reset the worktree");
    // git 2.55.0 reports `A.` here, not `AM`: the edit is gone from disk too.
    assert_eq!(
        f.run(&["status", "--porcelain"]).1,
        "A  f.txt\nA  g.txt\n",
        "-f left the worktree dirty against the new index"
    );
}

/// `checkout: moving from <what> to <branch>`. `old_branch_info.name` is taken
/// only from a symbolic `HEAD` under `refs/heads/`; anything else that still
/// peels to a commit records that **commit's** full hex, and a `HEAD` that peels
/// to nothing records `(invalid)`.
///
/// The last case is the one that matters: `lookup_commit_reference_gently()` is
/// why stock can switch *away* from a `HEAD` holding a blob's id. Peeling
/// strictly instead turns a recoverable repository into one the user is stuck
/// in, because the command that gets them out is the one that fails.
#[test]
fn the_reflog_names_the_old_head_the_way_git_does() {
    let f = Fixture::new("label");
    let main_oid = f.run(&["rev-parse", "main"]).1.trim().to_owned();
    f.git(&["tag", "-a", "-m", "annot", "at1", "main"]);
    let tag_oid = f.run(&["rev-parse", "at1"]).1.trim().to_owned();
    assert_ne!(tag_oid, main_oid, "the annotated tag must be its own object");
    let head_path = f.work.join(".git/HEAD");

    let last_msg = |f: &Fixture| -> String {
        let log = std::fs::read_to_string(f.work.join(".git/logs/HEAD")).unwrap();
        let line = log.trim_end().rsplit('\n').next().unwrap().to_owned();
        line.split_once('\t').unwrap().1.to_owned()
    };

    // Detached at the *tag* object: the message names the commit it peels to.
    std::fs::write(&head_path, format!("{tag_oid}\n")).unwrap();
    assert_eq!(f.run(&["checkout", "other"]).0, 0);
    assert_eq!(last_msg(&f), format!("checkout: moving from {main_oid} to other"));

    // Symbolic, but not under `refs/heads/`: still the commit's hex, not `at1`.
    std::fs::write(&head_path, "ref: refs/tags/at1\n").unwrap();
    assert_eq!(f.run(&["checkout", "main"]).0, 0);
    assert_eq!(last_msg(&f), format!("checkout: moving from {main_oid} to main"));

    // Peels to no commit at all → `(invalid)`, and the switch still succeeds.
    std::fs::write(f.work.join("blob.tmp"), "not a commit\n").unwrap();
    let blob = f.run(&["hash-object", "-w", "blob.tmp"]).1.trim().to_owned();
    assert_eq!(blob.len(), 40, "hash-object should have written the blob");
    std::fs::write(&head_path, format!("{blob}\n")).unwrap();
    let (code, _, err) = f.run(&["checkout", "other"]);
    assert_eq!(code, 0, "a broken HEAD must not trap the user: {err}");
    assert_eq!(last_msg(&f), "checkout: moving from (invalid) to other");
    assert_eq!(f.head_symref(), "refs/heads/other");
}

/// `git switch -m` is `git checkout -m`: git has one `merge_working_tree()` and
/// both commands call it. The autostash carries the local edit onto the target
/// branch, the conflicted result stays in the index at stages 1/2/3, and the
/// snapshot is left in `refs/stash`.
///
/// The listing an autostashed switch prints is a *second*, headed one emitted
/// after `update_refs_for_switch()` — not the one at the tail of
/// `merge_working_tree()`, which the retry ran with `merge = false`.
#[test]
fn switch_dash_m_carries_a_conflicting_edit_like_checkout_dash_m() {
    for verb in ["checkout", "switch"] {
        let f = Fixture::new(&format!("m{verb}"));
        f.dirty();

        let (code, out, err) = f.run(&[verb, "-m", "other"]);
        assert_eq!(code, 0, "`git {verb} -m other`: {out:?} {err:?}");

        // The headed listing, on stdout, after the switch was announced.
        assert_eq!(out, "The following paths have local changes:\nM\tf.txt\n");
        assert!(
            err.starts_with("Your local changes are stashed, however applying them\n"),
            "`git {verb} -m` stderr: {err:?}"
        );
        assert!(err.contains("Switched to branch 'other'\n"), "{err:?}");

        // Conflict markers, in the default `merge` style, labelled with the
        // branch switched to and `local`.
        assert_eq!(
            f.read("f.txt"),
            "l1\n<<<<<<< other\nOTHER\n=======\nLOCAL\n>>>>>>> local\nl3\n"
        );

        // Three stages for the conflicted path, one entry for the untouched one.
        let staged: Vec<String> = f
            .ls_files()
            .lines()
            .filter(|l| l.ends_with("f.txt"))
            .map(|l| l.split_whitespace().nth(2).unwrap().to_owned())
            .collect();
        assert_eq!(staged, ["1", "2", "3"], "`git {verb} -m` index stages");

        assert_eq!(f.head_symref(), "refs/heads/other");
        assert_eq!(
            f.run(&["stash", "list"]).1,
            "stash@{0}: autostash while switching to 'other'\n"
        );
    }
}

/// `restore`'s targets are not `checkout`'s. `opts->checkout_index` starts at
/// `-1` ("default off") for `restore` and `-2` ("default on") for `checkout`,
/// and `opts->overlay_mode` is `0` for `restore` against `-1` for `checkout`.
/// A shared implementation that adopted one verb's defaults for both passes
/// every single-verb test and still restores the wrong things.
#[test]
fn restore_and_checkout_do_not_share_defaults() {
    let f = Fixture::new("defaults");

    // A path the source tree does not carry, staged so both verbs see it.
    let setup = |f: &Fixture| {
        f.git(&["checkout", "-q", "-f", "main"]);
        f.git(&["clean", "-qfdx"]);
        std::fs::write(f.work.join("h.txt"), "extra\n").unwrap();
        f.git(&["add", "h.txt"]);
    };

    // `restore` is no-overlay by default: `h.txt` is absent from `main`, so the
    // restore removes it from the worktree.
    setup(&f);
    let (code, _, err) = f.run(&["restore", "--source=main", "."]);
    assert_eq!(code, 0, "{err}");
    assert!(!f.work.join("h.txt").exists(), "restore defaults to --no-overlay");

    // `checkout <tree> -- <paths>` is overlay by default: `h.txt` survives.
    setup(&f);
    let (code, _, err) = f.run(&["checkout", "main", "--", "."]);
    assert_eq!(code, 0, "{err}");
    assert!(f.work.join("h.txt").exists(), "checkout defaults to overlay");

    // `restore --overlay` opts back in.
    setup(&f);
    let (code, _, err) = f.run(&["restore", "--overlay", "--source=main", "."]);
    assert_eq!(code, 0, "{err}");
    assert!(f.work.join("h.txt").exists(), "restore --overlay keeps it");

    // The index target is off by default for `restore`: a plain restore leaves
    // the staged `h.txt` in the index even as it clears the worktree.
    setup(&f);
    let (code, _, err) = f.run(&["restore", "--source=main", "."]);
    assert_eq!(code, 0, "{err}");
    assert!(
        f.ls_files().contains("h.txt"),
        "restore must not touch the index without --staged"
    );
}
