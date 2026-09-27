//! The `--prune` / `--mirror` deletions in a dry run and in a failed atomic push.
//!
//! `match_push_refs()` gives each advertised ref that `--prune`/`--mirror` finds
//! no local source for a `(delete)` peer (remote.c:1633-1653), so the deletion
//! is in `remote_refs` like any other update before `send_pack()` looks at it.
//! A dry run then marks it `REF_STATUS_OK` with the rest (send-pack.c:670-674)
//! and the report lists `- [deleted]`; a refused ref under `--atomic` makes
//! `reject_atomic_push()` (transport-helper.c:1670-1690) fail every pending ref
//! — the deletions included, printed with their `(delete)` peer
//! (transport.c:799-803). zvcs computed the deletions only on the path that
//! wrote commands, so a dry run and an atomic failure dropped them from the
//! report.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// `r.git` holds `keep`, `main`, `nf`, `old` and tag `t1` at the first
    /// commit. Locally `old` and `t1` are gone, `main` is one commit ahead and
    /// `nf` was rewritten (a non-fast-forward).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-deletion-reports-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        for b in ["old", "keep", "nf"] {
            f.run(&["branch", b]);
        }
        f.run(&["tag", "t1"]);
        f.run(&["remote", "add", "r", "../r.git"]);
        f.run(&["push", "-q", "r", "main", "old", "keep", "nf", "t1"]);
        f.run(&["branch", "-q", "-D", "old"]);
        f.run(&["tag", "-d", "t1"]);
        std::fs::write(f.work.join("a"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
        f.run(&["checkout", "-q", "--orphan", "tmp"]);
        f.run(&["commit", "-q", "-m", "orphan"]);
        f.run(&["branch", "-f", "nf", "tmp"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["branch", "-q", "-D", "tmp"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn short(&self, rev: &str) -> String {
        self.run(&["rev-parse", "--short", rev]).0.trim().to_string()
    }

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../r.git", "for-each-ref", "--format=%(refname)"]).0
    }
}

const UNTOUCHED: &str = "refs/heads/keep\nrefs/heads/main\nrefs/heads/nf\nrefs/heads/old\nrefs/tags/t1\n";
const NON_FF_HINT: &str = "hint: Updates were rejected because a pushed branch tip is behind its remote\n\
     hint: counterpart. If you want to integrate the remote changes, use 'git pull'\n\
     hint: before pushing again.\n\
     hint: See the 'Note about fast-forwards' in 'git push --help' for details.\n";

#[test]
fn a_dry_run_reports_the_prune_and_mirror_deletions() {
    let f = Fixture::new("dry");
    let (base, main, nf) = (f.short("keep"), f.short("main"), f.short("nf"));

    let (out, err, code) = f.run(&["push", "--dry-run", "--prune", "r", "refs/heads/*:refs/heads/*"]);
    assert_eq!(
        (out.as_str(), err, code),
        (
            "",
            format!(
                "To ../r.git\n   \
                 {base}..{main}  main -> main\n \
                 - [deleted]         old\n \
                 ! [rejected]        nf -> nf (non-fast-forward)\n\
                 error: failed to push some refs to '../r.git'\n{NON_FF_HINT}"
            ),
            1
        )
    );

    let (out, err, code) = f.run(&["push", "--dry-run", "--mirror", "r"]);
    assert_eq!(
        (out.as_str(), err, code),
        (
            "",
            format!(
                "To ../r.git\n   \
                 {base}..{main}  main -> main\n \
                 + {base}...{nf} nf -> nf (forced update)\n \
                 - [deleted]         old\n \
                 - [deleted]         t1\n \
                 * [new reference]   r/keep -> r/keep\n \
                 * [new reference]   r/main -> r/main\n \
                 * [new reference]   r/nf -> r/nf\n \
                 * [new reference]   r/old -> r/old\n"
            ),
            0
        )
    );
    assert_eq!(f.remote_refs(), UNTOUCHED);
}

#[test]
fn an_atomic_failure_fails_the_deletions_too() {
    let f = Fixture::new("atomic");
    let (out, err, code) =
        f.run(&["push", "--porcelain", "--atomic", "--prune", "r", "refs/heads/*:refs/heads/*"]);
    assert_eq!(
        (out.as_str(), err, code),
        (
            "To ../r.git\n\
             =\trefs/heads/keep:refs/heads/keep\t[up to date]\n\
             !\trefs/heads/main:refs/heads/main\t[rejected] (atomic push failed)\n\
             !\trefs/heads/nf:refs/heads/nf\t[rejected] (non-fast-forward)\n\
             !\t(delete):refs/heads/old\t[rejected] (atomic push failed)\n\
             Done\n",
            format!(
                "error: atomic push failed for ref refs/heads/nf. status: 2\n\
                 error: failed to push some refs to '../r.git'\n{NON_FF_HINT}"
            ),
            1
        )
    );

    // `send-pack` prints the same verdicts through its own status block.
    let (out, err, code) = f.run(&["send-pack", "--atomic", "../r.git", "main", "nf", ":keep"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "error: atomic push failed for ref refs/heads/nf. status: 2\n\
             To ../r.git\n \
             ! [rejected]        (delete) -> keep (atomic push failed)\n \
             ! [rejected]        main -> main (atomic push failed)\n \
             ! [rejected]        nf -> nf (non-fast-forward)\n",
            1
        )
    );
    assert_eq!(f.remote_refs(), UNTOUCHED);
}
