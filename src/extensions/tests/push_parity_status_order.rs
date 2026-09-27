//! The order a push reports its refs in.
//!
//! Everything after the match walks `remote_refs`: the advertisement in the
//! order the remote sent it, with the refs `match_push_refs()` had to create
//! (`make_linked_ref()`) appended in the order they were matched.
//! `transport_print_push_status()` walks it three times — up to date (under
//! `-v`/`--porcelain`), then moved, then everything else (transport.c:850-899)
//! — and `transport_update_tracking_ref()` once (transport.c:1552-1557). zvcs
//! reported in request order, so a rejected ref named first was listed first,
//! and new refs sat wherever the command line put them.
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
    /// `r.git` holds `keep`, `main` and `nf` at the first commit. Locally `main`
    /// is one commit ahead, `nf` was rewritten (a non-fast-forward), and `zz`
    /// and `aa` are new.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-status-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "--bare", "-b", "main", "../r.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["branch", "keep"]);
        f.run(&["branch", "nf"]);
        f.run(&["remote", "add", "r", "../r.git"]);
        f.run(&["push", "-q", "r", "main", "keep", "nf"]);
        std::fs::write(f.work.join("a"), "b\n").unwrap();
        f.run(&["commit", "-q", "-am", "b"]);
        f.run(&["checkout", "-q", "--orphan", "tmp"]);
        f.run(&["commit", "-q", "-m", "orphan"]);
        f.run(&["branch", "-f", "nf", "tmp"]);
        f.run(&["checkout", "-q", "main"]);
        f.run(&["branch", "-q", "-D", "tmp"]);
        f.run(&["branch", "zz"]);
        f.run(&["branch", "aa"]);
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
}

#[test]
fn porcelain_lists_up_to_date_then_moved_then_rejected_in_remote_order() {
    let f = Fixture::new("porcelain");
    let main = f.run(&["rev-parse", "--short", "main"]).0;
    let base = f.run(&["rev-parse", "--short", "keep"]).0;
    let (out, _, code) = f.run(&["push", "--porcelain", "r", "nf", "zz", "main", "keep", "aa"]);
    assert_eq!(
        (out, code),
        (
            format!(
                "To ../r.git\n\
                 =\trefs/heads/keep:refs/heads/keep\t[up to date]\n \
                 \trefs/heads/main:refs/heads/main\t{}..{}\n\
                 *\trefs/heads/zz:refs/heads/zz\t[new branch]\n\
                 *\trefs/heads/aa:refs/heads/aa\t[new branch]\n\
                 !\trefs/heads/nf:refs/heads/nf\t[rejected] (non-fast-forward)\n\
                 Done\n",
                base.trim(),
                main.trim()
            ),
            1
        )
    );
}

#[test]
fn the_human_block_and_the_tracking_refs_follow_the_same_order() {
    let f = Fixture::new("human");
    let main = f.run(&["rev-parse", "--short", "main"]).0;
    let base = f.run(&["rev-parse", "--short", "keep"]).0;
    let (out, err, code) = f.run(&["push", "-v", "r", "zz", "main", "keep", "aa"]);
    assert_eq!(
        (out.as_str(), err, code),
        (
            "",
            format!(
                "Pushing to ../r.git\n\
                 To ../r.git\n \
                 = [up to date]      keep -> keep\n   \
                 {}..{}  main -> main\n \
                 * [new branch]      zz -> zz\n \
                 * [new branch]      aa -> aa\n\
                 updating local tracking ref 'refs/remotes/r/keep'\n\
                 updating local tracking ref 'refs/remotes/r/main'\n\
                 updating local tracking ref 'refs/remotes/r/zz'\n\
                 updating local tracking ref 'refs/remotes/r/aa'\n",
                base.trim(),
                main.trim()
            ),
            0
        )
    );
}
