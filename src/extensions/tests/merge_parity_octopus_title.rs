//! The generated subject of a merge with more than one head, and the
//! `--ff-only` refusal that precedes it.
//!
//! `fmt_merge_msg_title()` (fmt-merge-msg.c:452-503) does not say `branches`
//! for everything it is handed. It groups twice:
//!
//! * by *source*, keyed on the text after ` of ` in each `merge_name()` line
//!   (fmt-merge-msg.c:158-166) — a ref resolves to ` of .` while a raw commit
//!   gets no ` of ` at all (builtin/merge.c:566-580, :634-635), so every ref
//!   shares one group and each raw commit is a group of its own, printed
//!   verbatim and separated by `; `;
//! * by *category* within a source — branches, then remote-tracking branches,
//!   then tags, then commits, each run through `print_joined()`
//!   (fmt-merge-msg.c:209-224) and the runs joined by `, `.
//!
//! `if (fast_forward == FF_ONLY) die_ff_impossible()` (builtin/merge.c:1756)
//! sits below the fork between one head and several, so it governs an octopus
//! too — which can never be a fast-forward, three histories needing a commit to
//! join them.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, stdout, stderr and
//! exit status compared separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const FF_HINT: &str = "hint: Diverging branches can't be fast-forwarded, you need to either:\n\
     hint:\n\
     hint: \tgit merge --no-ff\n\
     hint:\n\
     hint: or:\n\
     hint:\n\
     hint: \tgit rebase\n\
     hint:\n\
     hint: Disable this message with \"git config set advice.diverging false\"\n\
     fatal: Not possible to fast-forward, aborting.\n";

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
    /// `main` at the root commit `c0`, with three side branches `br-a`, `br-b`,
    /// `br-c` each adding one file and each also carrying a tag `tag-<n>`, plus
    /// a remote-tracking ref `origin/rt` at `br-a`'s tip.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-octo-title-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "1\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "commit 0"]);
        f.git(&["tag", "c0"]);
        for n in ["a", "b", "c"] {
            f.git(&["checkout", "-q", "-b", &format!("br-{n}"), "c0"]);
            std::fs::write(f.work.join(n), format!("{n}\n")).unwrap();
            f.git(&["add", n]);
            f.git(&["commit", "-q", "-m", &format!("commit {n}")]);
            f.git(&["tag", &format!("tag-{n}")]);
        }
        f.git(&["checkout", "-q", "main"]);
        let a = f.oid("br-a");
        f.git(&["update-ref", "refs/remotes/origin/rt", &a]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn oid(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }

    /// Octopus the given operands onto a fresh `c0` and answer the recorded
    /// subject.
    fn subject_of_merge(&self, operands: &[&str]) -> String {
        self.git(&["reset", "-q", "--hard", "c0"]);
        let mut args = vec!["merge"];
        args.extend_from_slice(operands);
        let (_, err, code) = self.run(&args);
        assert_eq!(code, 0, "`git merge {operands:?}` failed: {err}");
        self.stdout(&["log", "-1", "--pretty=%s"]).trim_end().to_string()
    }
}

/// The category buckets: all tags plural, a branch and a tag side by side, and
/// three of a kind joined `a, b and c`.
#[test]
fn octopus_subject_names_each_head_by_the_category_its_ref_resolved_into() {
    let f = Fixture::new("cat");
    assert_eq!(f.subject_of_merge(&["br-a", "br-b"]), "Merge branches 'br-a' and 'br-b'");
    assert_eq!(f.subject_of_merge(&["tag-a", "tag-b"]), "Merge tags 'tag-a' and 'tag-b'");
    assert_eq!(
        f.subject_of_merge(&["tag-a", "tag-b", "tag-c"]),
        "Merge tags 'tag-a', 'tag-b' and 'tag-c'"
    );
    // Category runs are joined with `, ` in branch, remote-tracking, tag order
    // — not the order the operands were typed in.
    assert_eq!(f.subject_of_merge(&["br-a", "tag-b"]), "Merge branch 'br-a', tag 'tag-b'");
    assert_eq!(
        f.subject_of_merge(&["tag-b", "origin/rt"]),
        "Merge remote-tracking branch 'origin/rt', tag 'tag-b'"
    );
}

/// A raw commit has no ` of .` on its `merge_name()` line, so it is its own
/// source: `; ` separates it from the `.` group, and it is printed as the whole
/// `commit '<oid>'` line rather than folded into a `commits` run.
#[test]
fn a_raw_commit_operand_becomes_its_own_source_separated_by_a_semicolon() {
    let f = Fixture::new("src");
    let c = f.oid("br-c");
    assert_eq!(
        f.subject_of_merge(&["br-a", &c, "br-b"]),
        format!("Merge branches 'br-a' and 'br-b'; commit '{c}'")
    );

    let a = f.oid("br-a");
    let b = f.oid("br-b");
    assert_eq!(
        f.subject_of_merge(&[&a, &b]),
        format!("Merge commit '{a}'; commit '{b}'")
    );
}

/// `dest_suppressed()` governs the octopus title as much as the two-head one:
/// `main` is on the built-in suppression list, `topic` is not.
#[test]
fn the_into_branch_tail_follows_merge_suppress_dest() {
    let f = Fixture::new("into");
    assert_eq!(f.subject_of_merge(&["tag-a", "tag-b"]), "Merge tags 'tag-a' and 'tag-b'");
    f.git(&["checkout", "-q", "-b", "topic", "c0"]);
    f.git(&["tag", "-f", "c0", "c0"]);
    assert_eq!(
        f.subject_of_merge(&["tag-a", "tag-b"]),
        "Merge tags 'tag-a' and 'tag-b' into topic"
    );
}

/// `die_ff_impossible()` governs both head counts. Nothing is written: no merge
/// commit, no `MERGE_HEAD`, `HEAD` unmoved.
#[test]
fn ff_only_refuses_an_octopus_from_the_flag_and_from_merge_ff() {
    let f = Fixture::new("ffonly");
    f.git(&["reset", "-q", "--hard", "c0"]);
    let before = f.oid("HEAD");

    let (out, err, code) = f.run(&["merge", "--ff-only", "br-a", "br-b"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", FF_HINT, 128));

    let (out, err, code) = f.run(&["-c", "merge.ff=only", "merge", "br-a", "br-b"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", FF_HINT, 128));

    assert_eq!(f.oid("HEAD"), before);
    let (_, _, code) = f.run(&["rev-parse", "--verify", "-q", "MERGE_HEAD"]);
    assert_eq!(code, 1);
    assert_eq!(f.stdout(&["status", "--porcelain"]), "");
    // `refs_update_ref("updating ORIG_HEAD", …)` (builtin/merge.c:1636) is
    // above the refusal, so it still records the pre-merge tip.
    assert_eq!(f.oid("ORIG_HEAD"), before);
}
