//! The `(see more in file …)` line of `git status`'s interactive-rebase banner.
//!
//! `show_rebase_information()` prints at most two of the commands already done
//! and then, when there are more, `status_printf_ln(s, color, _("  (see more in
//! file %s)"), rebase_path_done())` (wt-status.c). That path comes from
//! `git_path()`, and `setup_git_directory()` has normalized `$GIT_DIR` long
//! before — so what git prints is `.git/rebase-merge/done`.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
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
        // A rebase left in progress holds nothing this cannot remove, but the
        // sequencer's state directory is removed with the rest of the tree.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Four commits on `main`, each rewriting `file`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-rbanner-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        for n in 1..=4 {
            std::fs::write(f.work.join("file"), format!("{n}\n")).unwrap();
            f.git(&["add", "file"]);
            f.git(&["commit", "-q", "-m", &format!("c{n}")]);
        }
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", ":")
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// Three commands done is one more than the banner lists, so the line naming
/// the file appears — spelled from the top of the work tree, with no `./`.
#[test]
fn the_done_file_is_named_the_way_git_path_spells_it() {
    let f = Fixture::new("seemore");
    // Stop the rebase on its last command so three commands are already done.
    let editor = f.root.join("sequence-editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nperl -i -pe 's/^pick/edit/ if $. == 3' \"$1\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = f
        .cmd(&["rebase", "-i", "HEAD~3"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rebase did not stop cleanly: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let status = f.stdout(&["status"]);
    assert!(
        status.contains("\n  (see more in file .git/rebase-merge/done)\n"),
        "banner was:\n{status}"
    );
    assert!(
        !status.contains("./.git/rebase-merge/done"),
        "the discovered git directory's ./ prefix reached the line:\n{status}"
    );
    // The two most recent commands are listed above it, newest last.
    assert!(status.contains("Last commands done (3 commands done):\n"), "{status}");
    f.git(&["rebase", "--abort"]);
}

/// With no more commands done than the banner lists, git names no file at all.
#[test]
fn a_short_done_list_names_no_file() {
    let f = Fixture::new("short");
    let editor = f.root.join("sequence-editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nperl -i -pe 's/^pick/edit/ if $. == 1' \"$1\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = f
        .cmd(&["rebase", "-i", "HEAD~3"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let status = f.stdout(&["status"]);
    assert!(status.contains("Last command done (1 command done):\n"), "{status}");
    assert!(!status.contains("see more in file"), "{status}");
    f.git(&["rebase", "--abort"]);
}
