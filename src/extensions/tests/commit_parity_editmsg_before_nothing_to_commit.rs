//! `git commit` against stock git: `prepare_to_commit()` runs `pre-commit` (builtin/commit.c:780),
//! writes the message buffer to `COMMIT_EDITMSG` (:931), checks the committer (:941), adds the
//! editor's status block and `--trailer` (:1025, :1069), and only then refuses an uncommittable
//! index (:1081) — before `prepare-commit-msg` (:1116) and `commit-msg` (:1133). So a refused
//! `commit -m x` leaves `x` in `COMMIT_EDITMSG` and has run `pre-commit` alone, and the `git commit`
//! child of a rebase stopping on an empty pick leaves the picked message there. A tree that cannot
//! be built is reported quietly by `prepare_index()` and loudly at :1111, past all of that.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn both(label: &str, setup: impl Fn(&Side)) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    setup(&s);
    setup(&z);
    Some((s, z))
}

/// Every commit hook appends its name and arguments to `.git/hook.log`.
fn logging_hooks(side: &Side) {
    for hook in ["pre-commit", "prepare-commit-msg", "commit-msg"] {
        side.write(
            &format!(".git/hooks/{hook}"),
            &format!("#!/bin/sh\necho {hook} \"$@\" >> .git/hook.log\n"),
        );
        let path = side.repo().join(".git/hooks").join(hook);
        std::process::Command::new("chmod").arg("+x").arg(path).status().unwrap();
    }
}

/// Run `args` on both sides from a fresh `COMMIT_EDITMSG` and hook log, then compare the
/// streams, the message file and the hooks that ran.
fn check(s: &Side, z: &Side, args: &[&str]) {
    for side in [s, z] {
        side.write(".git/COMMIT_EDITMSG", "old\n");
        let _ = std::fs::remove_file(side.repo().join(".git/hook.log"));
    }
    assert_eq!(z.git(args), s.git(args), "{args:?}");
    assert_eq!(z.read(".git/COMMIT_EDITMSG"), s.read(".git/COMMIT_EDITMSG"), "COMMIT_EDITMSG after {args:?}");
    assert_eq!(z.read(".git/hook.log"), s.read(".git/hook.log"), "hooks run by {args:?}");
}

#[test]
fn a_refused_commit_writes_the_message_and_runs_only_pre_commit() {
    let Some((s, z)) = both("commit-editmsg-refused", |side| {
        logging_hooks(side);
        side.write("msg", "from file\n\n\n");
        side.write("tmpl", "template text\n");
        side.write(".git/info/exclude", "msg\ntmpl\n");
    }) else {
        return;
    };
    for args in [
        &["commit", "-m", "x"][..],
        &["commit", "-n", "-m", "x"],
        &["commit", "-F", "msg"],
        &["commit", "-F", "no-such-file"],
        &["commit", "-e", "-m", "x"],
        &["commit", "-e", "--no-status", "-m", "x"],
        &["commit"],
        &["commit", "-t", "tmpl"],
        &["commit", "-t", "tmpl", "-m", "y"],
        &["commit", "-v", "-m", "x"],
        &["commit", "-v", "-e", "-m", "x"],
        &["commit", "-s", "-m", "x"],
        &["commit", "-m", "x", "--trailer", "Foo: bar"],
        &["commit", "--cleanup=verbatim", "-m", "  x  "],
        &["commit", "-e", "--cleanup=scissors", "-m", "x"],
        &["commit", "-C", "HEAD"],
        &["commit", "--fixup", "HEAD"],
        &["commit", "--squash", "HEAD", "-m", "extra"],
        &["commit", "--dry-run", "-m", "x"],
        &["commit", "--allow-empty", "-m", "x"],
    ] {
        check(&s, &z, args);
    }
}

#[test]
fn merge_and_squash_messages_reach_commit_editmsg_before_the_refusal() {
    let Some((s, z)) = both("commit-editmsg-merge-msg", logging_hooks) else {
        return;
    };
    for (file, body) in [("MERGE_MSG", "Merge msg\n\n# Conflicts:\n"), ("SQUASH_MSG", "Squash msg\n")] {
        for side in [&s, &z] {
            side.write(&format!(".git/{file}"), body);
        }
        check(&s, &z, &["commit", "--no-edit"]);
        check(&s, &z, &["commit", "-e"]);
        for side in [&s, &z] {
            std::fs::remove_file(side.repo().join(".git").join(file)).unwrap();
        }
    }
}

#[test]
fn a_tree_that_cannot_be_built_is_reported_after_the_message_is_written() {
    let Some((s, z)) = both("commit-editmsg-missing-blob", |side| {
        logging_hooks(side);
        side.git(&["update-index", "--add", "--cacheinfo", "100644,1111111111111111111111111111111111111111,zz"]);
    }) else {
        return;
    };
    for args in [
        &["commit", "-m", "x"][..],
        &["commit", "-F", "no-such-file"],
        &["commit", "-e", "-m", "x"],
        &["commit", "-m", "x", "--trailer", "K: v"],
        &["commit", "--dry-run", "-m", "x"],
    ] {
        check(&s, &z, args);
    }
}

#[test]
fn a_rebase_stopping_on_an_empty_pick_leaves_its_message() {
    let Some((s, z)) = both("commit-editmsg-rebase-empty", |side| {
        side.git(&["checkout", "-q", "-b", "dup", "main"]);
        side.write("c", "c\n");
        side.git(&["add", "c"]);
        side.git_env(&[("GIT_COMMITTER_DATE", "1700000100 +0000")], &["commit", "-q", "-m", "c dup"]);
        side.git(&["checkout", "-q", "main"]);
        side.write("c", "c\n");
        side.git(&["add", "c"]);
        side.git(&["commit", "-q", "-m", "c1"]);
        side.write("d", "d\n");
        side.git(&["add", "d"]);
        side.git(&["commit", "-q", "-m", "d1"]);
        side.write(".git/COMMIT_EDITMSG", "old\n");
    }) else {
        return;
    };
    let args = ["rebase", "--reapply-cherry-picks", "--empty=stop", "dup"];
    assert_eq!(z.git(&args), s.git(&args));
    assert_eq!(z.read(".git/COMMIT_EDITMSG"), s.read(".git/COMMIT_EDITMSG"));
    for probe in [&["status", "--porcelain"][..], &["for-each-ref"], &["reflog", "list"]] {
        assert_eq!(z.git(probe), s.git(probe), "{probe:?}");
    }
}

#[test]
fn an_only_commit_takes_its_editor_status_block_from_the_false_index() {
    let Some((s, z)) = both("commit-editmsg-only-status", |side| {
        logging_hooks(side);
        side.write("b", "b\n");
        side.git(&["add", "b"]);
        side.write("u", "u\n");
    }) else {
        return;
    };
    for args in [
        &["commit", "-e", "-m", "x", "--", "a"][..],
        &["commit", "-o", "-e", "-m", "x", "a"],
        &["commit", "-e", "-uno", "-m", "x", "--", "a"],
        &["commit", "-v", "-e", "-m", "x", "--", "a"],
        &["commit", "-i", "-e", "-m", "x", "--", "a"],
    ] {
        check(&s, &z, args);
    }
}
