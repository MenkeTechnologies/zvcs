//! Which notes ref `git notes` opens, and what happens when a manual merge is
//! already staged.
//!
//! Three behaviors are pinned:
//!
//! * `GIT_NOTES_REF=` is a *set* notes ref whose name happens to be empty.
//!   `notes.c:1007` reads it through `xstrdup_or_null(getenv(...))`, which is
//!   NULL only when the variable is unset, so the `core.notesref` lookup at
//!   `notes.c:1009` is skipped and every subcommand then refuses the empty name.
//!
//! * That refusal — `init_notes_check()`, `builtin/notes.c:418-435` — is not
//!   limited to the writing subcommands. `list` (`:457`) and `show` (`:785`)
//!   open the tree with `flags == 0` and check `t->ref`, which is the same name.
//!
//! * A second `git notes merge` while `$GIT_DIR/NOTES_MERGE_WORKTREE` still
//!   holds conflicts dies rather than restarting the merge
//!   (`notes-merge.c:276-310`, reached from `notes-merge.c:406`), and
//!   `advice.resolveConflict` picks between the long and short wordings.
//!
//! The `--commit`/`--abort` verbosity lines (`notes-merge.c:703-705`, `:765-766`)
//! ride along, since git counts verbosity from `NOTES_MERGE_VERBOSITY_DEFAULT`
//! (2) and so prints them at one `-v`.
//!
//! Every case is also run against the system `git` and compared on stdout,
//! stderr and exit code.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_git");

static SEQ: AtomicU64 = AtomicU64::new(0);

fn env_cmd(bin: &str, repo: &Path, home: &Path) -> Command {
    let mut c = Command::new(bin);
    c.current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@e")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000");
    c
}

/// Fixture setup with system `git`; never the behavior under test.
fn git(repo: &Path, home: &Path, args: &[&str]) {
    let ok = env_cmd("git", repo, home).args(args).status().unwrap().success();
    assert!(ok, "git {args:?} failed");
}

/// A one-commit repo with conflicting notes on `refs/notes/commits` and
/// `refs/notes/other`, plus `core.notesRef` pointed somewhere else entirely so
/// the environment-versus-config precedence is observable.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("zvcs-notesref-{tag}-{}-{uniq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    git(&repo, &home, &["notes", "--ref=commits", "add", "-m", "AAA", "HEAD"]);
    git(&repo, &home, &["notes", "--ref=other", "add", "-m", "BBB", "HEAD"]);
    (repo, home)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Run a whole scripted sequence of `<bin> …` invocations in one fresh fixture,
/// concatenating each step's exit code, stdout and stderr into one transcript.
/// Conflict-staging scenarios need the steps to share a repository, so a
/// per-command helper would not express them.
fn transcript(bin: &str, tag: &str, steps: &[(&[&str], &[(&str, &str)])]) -> String {
    let (repo, home) = fixture(tag);
    let mut log = String::new();
    for (args, envs) in steps {
        let mut cmd = env_cmd(bin, &repo, &home);
        for (k, v) in *envs {
            cmd.env(k, v);
        }
        let out = cmd.args(*args).stdin(std::process::Stdio::null()).output().unwrap();
        log.push_str(&format!(
            "$ {}\nrc={}\n{}{}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            stdout(&out),
            stderr(&out)
        ));
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    log
}

/// The same transcript from both binaries, asserted equal, and returned.
fn both(tag: &str, steps: &[(&[&str], &[(&str, &str)])]) -> String {
    let z = transcript(BIN, tag, steps);
    let g = transcript("git", &format!("{tag}-g"), steps);
    assert_eq!(z, g, "{tag}: transcript differs from system git");
    z
}

#[test]
fn empty_git_notes_ref_is_set_and_refused_everywhere() {
    // `core.notesRef` names a perfectly usable ref, so anything that falls
    // through to it will succeed — which is exactly the bug this pins. With
    // `GIT_NOTES_REF=` every subcommand must instead refuse the empty name,
    // including the two read-only ones.
    let empty: &[(&str, &str)] = &[("GIT_NOTES_REF", "")];
    let log = both(
        "emptyenv",
        &[
            (&["config", "core.notesRef", "refs/notes/commits"], &[]),
            (&["notes", "get-ref"], empty),
            (&["notes", "list"], empty),
            (&["notes", "show"], empty),
            (&["notes", "add", "-m", "x"], empty),
            (&["notes", "append", "-m", "x"], empty),
            (&["notes", "copy", "HEAD", "HEAD"], empty),
            (&["notes", "remove"], empty),
            (&["notes", "prune"], empty),
            (&["notes", "merge", "other"], empty),
            // Unset again: the config ref is back in play and the note is there.
            (&["notes", "list"], &[]),
        ],
    );
    // `get-ref` prints the empty name and succeeds; everything else dies 128.
    assert!(log.contains("$ notes get-ref\nrc=0\n\n"), "get-ref prints an empty ref:\n{log}");
    for sub in ["list", "show", "add", "append", "copy", "remove", "prune", "merge"] {
        assert!(
            log.contains(&format!("fatal: refusing to {sub} notes in  (outside of refs/notes/)")),
            "`{sub}` must refuse the empty notes ref:\n{log}"
        );
    }
    assert_eq!(log.matches("rc=128").count(), 8, "eight refusals, one per subcommand:\n{log}");
    // And with the variable unset the config ref really does work, so the
    // refusals above are about the environment, not a broken fixture.
    assert!(
        log.trim_end().ends_with(" f6ff480b95036f304c76fb00fc918ce8204cb626"),
        "unset GIT_NOTES_REF falls back to core.notesRef:\n{log}"
    );
}

#[test]
fn read_only_subcommands_refuse_a_ref_outside_refs_notes() {
    // `--ref` runs through `expand_notes_ref()`, which cannot produce a name
    // outside `refs/notes/`; `core.notesRef` is taken verbatim and can. `list`
    // and `show` must refuse it just like the writing subcommands do.
    let log = both(
        "outside",
        &[
            (&["config", "core.notesRef", "refs/heads/main"], &[]),
            (&["notes", "list"], &[]),
            (&["notes", "show"], &[]),
            (&["notes", "get-ref"], &[]),
        ],
    );
    assert!(
        log.contains("fatal: refusing to list notes in refs/heads/main (outside of refs/notes/)"),
        "list must refuse:\n{log}"
    );
    assert!(
        log.contains("fatal: refusing to show notes in refs/heads/main (outside of refs/notes/)"),
        "show must refuse:\n{log}"
    );
    // `get-ref` never opens a tree, so it still just prints the name.
    assert!(log.contains("$ notes get-ref\nrc=0\nrefs/heads/main\n"), "get-ref prints:\n{log}");
}

#[test]
fn a_staged_merge_blocks_a_second_merge() {
    // The first merge conflicts and leaves NOTES_MERGE_WORKTREE populated; the
    // second must refuse instead of redoing the merge. With
    // `advice.resolveConflict=false` the same refusal loses its second
    // paragraph.
    let log = both(
        "inprogress",
        &[
            (&["notes", "merge", "other"], &[]),
            (&["notes", "merge", "other"], &[]),
            (&["-c", "advice.resolveConflict=false", "notes", "merge", "other"], &[]),
            (&["notes", "merge", "--abort"], &[]),
            // Once aborted, a merge may start again.
            (&["notes", "merge", "other"], &[]),
        ],
    );
    assert_eq!(
        log.matches("fatal: You have not concluded your previous notes merge").count(),
        1,
        "exactly one long-form refusal (the advice-enabled second merge):\n{log}"
    );
    assert!(
        log.contains(
            "Please, use 'git notes merge --commit' or 'git notes merge --abort' to \
             commit/abort the previous merge before you start a new notes merge."
        ),
        "the advice paragraph:\n{log}"
    );
    assert_eq!(
        log.matches("fatal: You have not concluded your notes merge (").count(),
        1,
        "and exactly one short-form refusal (advice.resolveConflict=false):\n{log}"
    );
    assert_eq!(log.matches("rc=128").count(), 2, "both refusals are fatal:\n{log}");
    // The first and the post-abort merge both conflicted (exit 1), which is
    // what makes the two refusals in between meaningful.
    assert_eq!(log.matches("rc=1\n").count(), 2, "two real conflicting merges:\n{log}");
}

#[test]
fn merge_commit_and_abort_are_verbose_at_one_dash_v() {
    // git's notes-merge verbosity is offset by NOTES_MERGE_VERBOSITY_DEFAULT
    // (2), so `notes-merge.c`'s `>= 3` guards fire at a single `-v` and its
    // `>= 4` guards at `-vv`.
    let log = both(
        "verbose",
        &[
            (&["notes", "merge", "other"], &[]),
            (&["notes", "merge", "--commit", "-v"], &[]),
            // The worktree directory itself survives `--commit` (git keeps it
            // because it may be the user's cwd), so this `--abort` announces a
            // removal it finds nothing to do and still succeeds.
            (&["notes", "merge", "--abort", "-v"], &[]),
        ],
    );
    assert!(
        log.contains("Committing notes in notes merge worktree at .git/NOTES_MERGE_WORKTREE\n"),
        "`--commit -v` announces the worktree:\n{log}"
    );
    assert_eq!(
        log.matches("Removing notes merge worktree at .git/NOTES_MERGE_WORKTREE/*\n").count(),
        2,
        "once from the successful --commit, once from the empty --abort:\n{log}"
    );
    // Both of those lines are gated: without `-v` neither appears at all.
    let quiet = both(
        "verbose-off",
        &[
            (&["notes", "merge", "other"], &[]),
            (&["notes", "merge", "--commit"], &[]),
        ],
    );
    assert!(
        !quiet.contains("Committing notes in notes merge worktree")
            && !quiet.contains("Removing notes merge worktree"),
        "the default verbosity prints neither line:\n{quiet}"
    );
}
