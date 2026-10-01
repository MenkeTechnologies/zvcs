//! `git rebase --skip` over a fixup/squash that conflicted, as git 2.56 does it.
//!
//! `update_squash_messages()` runs before the merge (sequencer.c:2429-2434), so
//! the conflicting member is already in `current-fixups` and `message-squash`
//! when the rebase stops, and `error_failed_squash()` stops through
//! `error_with_patch(…, to_amend = 1)` (sequencer.c:3911-3926): the `amend`
//! marker is written and the `You can amend the commit now` text replaces
//! `Could not apply`. `--skip` reaches `commit_staged_changes()`
//! (builtin/rebase.c:1385-1397), which drops the skipped member from the chain
//! and, when it ended the chain, re-commits the melded commit with its message
//! cleaned up — opening the editor when the chain holds a `squash` or (2.56,
//! sequencer.c:5490-5491) a `fixup -c` (sequencer.c:5430-5530).
//!
//! zvcs used to leave the commented melded message in the final commit, never
//! opened the editor, and — with no `amend` marker — had `--continue` of a
//! failed squash make a new commit instead of amending.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `base`, then `wants-fixup` creating `wants-fixup.t`, then three commits
    /// rewriting it to `1`, `2` and `3` — t3418's `with-conflicting-fixup`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rebase-skip-fixup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("r")).unwrap();
        let f = Fixture { root: std::fs::canonicalize(&root).unwrap() };
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "commit.status", "false"]);
        f.commit("base", "base", "base");
        for (subject, content, tag) in [
            ("wants-fixup", "wants-fixup", "wants-fixup"),
            ("fixup 1", "1", "wants-fixup-1"),
            ("fixup 2", "2", "wants-fixup-2"),
            ("fixup 3", "3", "wants-fixup-3"),
        ] {
            f.commit("wants-fixup.t", content, subject);
            f.git(&["tag", tag]);
        }
        // The editor records each message it is shown, and appends a line so
        // that an edit is visible in the commit.
        std::fs::write(
            f.root.join("editor"),
            format!("#!/bin/sh\ncp \"$1\" '{}/shown'\necho edited >>\"$1\"\n", f.root.display()),
        )
        .unwrap();
        make_executable(&f.root.join("editor"));
        f
    }

    fn repo(&self) -> PathBuf {
        self.root.join("r")
    }

    fn commit(&self, file: &str, content: &str, subject: &str) {
        std::fs::write(self.repo().join(file), format!("{content}\n")).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-m", subject]);
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(self.repo())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", self.root.join("editor"))
            .env("GIT_SEQUENCE_EDITOR", self.root.join("seq-editor"))
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn id(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev]).trim().to_string()
    }

    /// Start `rebase -i HEAD~4` on this exact sheet; every case stops on a conflict.
    fn start(&self, sheet: &[(&str, &str)]) -> Output {
        let mut todo = String::new();
        for (cmd, rev) in sheet {
            todo.push_str(&format!("{cmd} {}\n", self.id(rev)));
        }
        std::fs::write(self.root.join("todo"), todo).unwrap();
        std::fs::write(
            self.root.join("seq-editor"),
            format!("#!/bin/sh\ncp '{}/todo' \"$1\"\n", self.root.display()),
        )
        .unwrap();
        make_executable(&self.root.join("seq-editor"));
        let out = self.run(&["rebase", "-i", "HEAD~4"]);
        assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
        out
    }

    /// `rebase --skip`, returning its exit code and the message the editor was
    /// shown, if it ran.
    fn skip(&self) -> (Option<i32>, Option<String>) {
        let _ = std::fs::remove_file(self.root.join("shown"));
        let code = self.run(&["rebase", "--skip"]).status.code();
        (code, std::fs::read_to_string(self.root.join("shown")).ok())
    }

    fn state(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.repo().join(".git/rebase-merge").join(name)).ok()
    }

    fn head_message(&self) -> String {
        self.git(&["log", "-1", "--format=%B"])
    }
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_failed_squash_stops_to_amend_with_the_member_already_counted() {
    let f = Fixture::new("stop");
    let out = f.start(&[
        ("pick", "wants-fixup"),
        ("fixup", "wants-fixup-1"),
        ("squash", "wants-fixup-3"),
        ("fixup", "wants-fixup-2"),
    ]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.ends_with(
            "You can amend the commit now, with\n\n  git commit --amend \n\n\
             Once you are satisfied with your changes, run\n\n  git rebase --continue\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("Could not apply"), "{stderr}");
    assert_eq!(f.state("amend"), Some(format!("{}\n", f.id("HEAD"))));
    assert_eq!(
        f.state("current-fixups"),
        Some(format!("fixup {}\nsquash {}", f.id("wants-fixup-1"), f.id("wants-fixup-3")))
    );
    let combined = "# This is a combination of 3 commits.\n\
                    # This is the 1st commit message:\n\nwants-fixup\n\n\
                    # The commit message #2 will be skipped:\n\n# fixup 1\n\n\
                    # This is the commit message #3:\n\nfixup 3\n";
    assert_eq!(f.state("message-squash").as_deref(), Some(combined));
    assert_eq!(f.state("message").as_deref(), Some(combined));
    assert_eq!(
        std::fs::read_to_string(f.repo().join(".git/MERGE_MSG")).unwrap(),
        combined
    );
    assert_eq!(f.state("message-fixup"), None);

    // `--continue` amends the melded commit rather than adding one on top.
    std::fs::write(f.repo().join("wants-fixup.t"), "3\n").unwrap();
    f.git(&["add", "wants-fixup.t"]);
    let out = f.run(&["rebase", "--continue"]);
    assert_eq!(out.status.code(), Some(1), "the trailing fixup conflicts again");
    assert_eq!(f.git(&["log", "--format=%s", "-2"]), "wants-fixup\nbase\n");
}

#[test]
fn skipping_conflicting_squashes_rebuilds_the_chain_and_edits_once_at_the_end() {
    let f = Fixture::new("squash");
    f.start(&[
        ("pick", "wants-fixup"),
        ("squash", "wants-fixup-1"),
        ("squash", "wants-fixup"),
        ("squash", "wants-fixup-2"),
        ("squash", "wants-fixup"),
        ("squash", "wants-fixup-3"),
        ("squash", "wants-fixup"),
    ]);

    // Not the final squash: no editor, and the commit carries the chain so far.
    assert_eq!(f.skip(), (Some(1), None));
    assert_eq!(
        f.head_message(),
        "# This is a combination of 3 commits.\n# This is the 1st commit message:\n\n\
         wants-fixup\n\n# This is the commit message #2:\n\nfixup 1\n\n\
         # This is the commit message #3:\n\nfixup 2\n\n"
    );
    assert_eq!(f.skip(), (Some(1), None));
    let four = "# This is a combination of 4 commits.\n# This is the 1st commit message:\n\n\
                wants-fixup\n\n# This is the commit message #2:\n\nfixup 1\n\n\
                # This is the commit message #3:\n\nfixup 2\n\n\
                # This is the commit message #4:\n\nfixup 3\n";
    assert_eq!(f.head_message(), format!("{four}\n"));

    // The final squash is skipped: the chain holds a squash, so the editor
    // opens on the melded message and the commit is cleaned up.
    assert_eq!(f.skip(), (Some(0), Some(four.to_string())));
    assert_eq!(
        f.head_message(),
        "wants-fixup\n\nfixup 1\n\nfixup 2\n\nfixup 3\nedited\n\n"
    );
    assert!(!f.repo().join(".git/rebase-merge").exists());
}

#[test]
fn skipping_the_final_fixup_cleans_up_without_an_editor() {
    let f = Fixture::new("fixup");
    f.start(&[
        ("pick", "wants-fixup"),
        ("fixup", "wants-fixup-1"),
        ("fixup", "wants-fixup-3"),
    ]);
    assert_eq!(f.skip(), (Some(0), None));
    assert_eq!(f.head_message(), "wants-fixup\n\n");
}

#[test]
fn skipping_after_a_fixup_c_still_opens_the_editor() {
    let f = Fixture::new("fixup-c");
    f.start(&[
        ("pick", "wants-fixup"),
        ("fixup -c", "wants-fixup-1"),
        ("fixup", "wants-fixup"),
    ]);
    assert_eq!(
        f.state("current-fixups"),
        Some(format!("fixup -c {}\nfixup {}", f.id("wants-fixup-1"), f.id("wants-fixup")))
    );
    assert_eq!(
        f.skip(),
        (
            Some(0),
            Some(
                "# This is a combination of 2 commits.\n# The 1st commit message will be skipped:\n\n\
                 # wants-fixup\n\n# This is the commit message #2:\n\nfixup 1\n"
                    .to_string()
            )
        )
    );
    assert_eq!(f.head_message(), "fixup 1\nedited\n\n");
}
