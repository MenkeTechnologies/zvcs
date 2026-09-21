//! `git replay` accepts `handle_revision_arg_1()`'s parent marks.
//!
//! `cmd_replay()` hands its leftover arguments to `setup_revisions()`, so
//! `<rev>^!`, `<rev>^@` and `<rev>^-<n>` reach the block at revision.c:2178-2207
//! like anywhere else. Two things there are observable and were measured
//! against stock git 2.55.0:
//!
//! * `<rev>^!` queues the parents with `flags ^ (UNINTERESTING | BOTTOM)` and
//!   then replays `<rev>` alone. Without the block the operand keeps its mark,
//!   nothing excludes the parents, and `git replay --advance main topic^!`
//!   replays the whole branch instead of its tip.
//! * The operand is recorded under `arg_`, the *untouched* argument
//!   (`add_rev_cmdline(revs, object, arg_, …)`, revision.c:2234), while
//!   `add_parents_only()` records the parents under the trimmed name. So
//!   `get_ref_information()` can dwim the parent entries to a reference but not
//!   the operand, and `git replay --onto <base> topic^!` updates no reference at
//!   all — where plain `git replay --onto <base> topic` updates `topic`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(format!("{name}.t")), format!("{name}\n")).unwrap();
    git(repo, &["add", &format!("{name}.t")]);
    git(repo, &["commit", "-q", "-m", name]);
}

/// `main` is A-B; `topic` adds C then D on top of B.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-replay-marks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    commit(&repo, "A");
    commit(&repo, "B");
    git(&repo, &["switch", "-q", "-c", "topic"]);
    commit(&repo, "C");
    commit(&repo, "D");
    git(&repo, &["switch", "-q", "main"]);
    commit(&repo, "E");
    repo
}

/// The subjects a replayed tip carries, newest first.
fn subjects(repo: &Path, oid: &str) -> Vec<String> {
    git(repo, &["log", "--format=%s", oid])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn caret_bang_replays_only_the_named_commit() {
    let repo = fixture("bang");
    let out = run(&repo, &["replay", "--ref-action=print", "--advance", "main", "topic^!"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let line = stdout.lines().next().unwrap_or_default().to_owned();
    assert!(
        line.starts_with("update refs/heads/main "),
        "--advance should have reported one update to main, got:\n{stdout}"
    );
    let new_tip = line.split(' ').nth(2).unwrap();
    assert_eq!(
        subjects(&repo, new_tip),
        vec!["D", "E", "B", "A"],
        "only D belongs on top of main; C means the parents were not excluded"
    );
}

#[test]
fn caret_bang_names_no_reference_to_update() {
    let repo = fixture("onto");

    // The control: the same operand without the mark does update `topic`.
    let plain = run(&repo, &["replay", "--ref-action=print", "--onto", "main", "topic"]);
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));
    assert!(
        String::from_utf8_lossy(&plain.stdout).starts_with("update refs/heads/topic "),
        "the control case did not update topic, so the assertion below proves nothing:\n{}",
        String::from_utf8_lossy(&plain.stdout)
    );

    let marked = run(&repo, &["replay", "--ref-action=print", "--onto", "main", "topic^!"]);
    assert!(marked.status.success(), "{}", String::from_utf8_lossy(&marked.stderr));
    assert_eq!(
        String::from_utf8_lossy(&marked.stdout),
        "",
        "the operand of a `^!` is recorded under its untrimmed name, which dwims to no reference"
    );

    // Nothing was updated because nothing dwimmed — not because the operand was
    // dropped. Naming a destination explicitly proves the same range really was
    // replayed.
    let with_ref = run(
        &repo,
        &[
            "replay",
            "--ref-action=print",
            "--onto",
            "main",
            "--ref=refs/heads/landing",
            "topic^!",
        ],
    );
    let stdout = String::from_utf8_lossy(&with_ref.stdout);
    assert!(
        with_ref.status.success(),
        "{}",
        String::from_utf8_lossy(&with_ref.stderr)
    );
    let tip = stdout
        .lines()
        .next()
        .filter(|l| l.starts_with("update refs/heads/landing "))
        .and_then(|l| l.split(' ').nth(2))
        .unwrap_or_else(|| panic!("--ref reported no update for the same range:\n{stdout}"))
        .to_owned();
    assert_eq!(
        subjects(&repo, &tip),
        vec!["D", "E", "B", "A"],
        "the `^!` range itself must still replay D alone"
    );
}

#[test]
fn caret_at_replays_the_parents_and_not_the_commit() {
    let repo = fixture("at");
    // `topic^@` is every parent of D, i.e. C alone, replayed onto main.
    let out = run(&repo, &["replay", "--ref-action=print", "--advance", "main", "topic^@"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let new_tip = stdout
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(2))
        .unwrap_or_default()
        .to_owned();
    assert_eq!(
        subjects(&repo, &new_tip),
        vec!["C", "E", "B", "A"],
        "`^@` replays everything the parents reach, and never the operand itself"
    );
}

#[test]
fn a_bad_parent_number_is_a_bad_revision() {
    let repo = fixture("badn");
    // `strtol_i()` rejects `0`, so `add_parents_only()` is never reached and the
    // operand is never resolved (revision.c:2197-2201).
    let out = run(&repo, &["replay", "--ref-action=print", "--advance", "main", "topic^-0"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "bad revision exits 128: {stderr}");
    assert!(
        stderr.contains("ambiguous argument 'topic^-0'"),
        "expected the bad-revision report, got:\n{stderr}"
    );
}

#[test]
fn a_full_hex_with_no_object_dies_as_bad_object() {
    let repo = fixture("badobj");
    // `get_reference()` inside `add_parents_only()`'s peeling loop dies naming
    // the object, not the operand.
    let missing = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let out = run(
        &repo,
        &["replay", "--ref-action=print", "--advance", "main", &format!("{missing}^!")],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "{stderr}");
    assert_eq!(
        stderr.trim(),
        format!("fatal: bad object {missing}"),
        "expected git's bad-object report"
    );
}
