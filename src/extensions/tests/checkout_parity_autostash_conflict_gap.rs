//! A `-m` switch whose autostash re-apply conflicted ran its warning straight
//! into the switch announcement.
//!
//! `switch_branches()` remembers what `apply_autostash_ref()` answered and, once
//! `orphaned_commit_warning()` has had its say, separates a conflicted re-apply
//! from everything `update_refs_for_switch()` prints:
//!
//! ```c
//! if (autostash_res == STASH_APPLY_CONFLICT && !opts->quiet)
//!         fputc('\n', stderr);
//! update_refs_for_switch(opts, &old_branch_info, new_branch_info);
//! ```
//! (git 2.56.0 builtin/checkout.c:1259-1261.) A clean re-apply (`Applied
//! autostash.`) and `-q` get no blank line; the detached-HEAD advice, which
//! `update_refs_for_switch()` prints, comes after it.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const CONFLICT_WARNING: &str = "Your local changes are stashed, however applying them\n\
resulted in conflicts.  You can either resolve the conflicts\n\
and then discard the stash with \"git stash drop\", or, if you\n\
do not want to resolve them now, run \"git reset --hard\" and\n\
apply the local changes later by running \"git stash pop\".\n";

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
    /// `f` reads `base`, then `main` on `main` and `side` on `side`; `main` is
    /// checked out and `f` carries the local edit `local`, which conflicts with
    /// `side`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-checkout-autostash-gap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("base\n");
        f.run(&["add", "f"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["branch", "side"]);
        f.write("main\n");
        f.run(&["commit", "-q", "-a", "-m", "main"]);
        f.run(&["checkout", "-q", "side"]);
        f.write("side\n");
        f.run(&["commit", "-q", "-a", "-m", "side"]);
        f.run(&["checkout", "-q", "main"]);
        f.write("local\n");
        f
    }

    fn write(&self, content: &str) {
        std::fs::write(self.work.join("f"), content).unwrap();
    }

    fn read(&self) -> String {
        std::fs::read_to_string(self.work.join("f")).unwrap()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const MARKERS: &str = "<<<<<<< side\nside\n=======\nlocal\n>>>>>>> local\n";
const LISTING: &str = "The following paths have local changes:\nM\tf\n";

#[test]
fn a_conflicted_branch_switch_is_set_apart_by_a_blank_line() {
    let f = Fixture::new("branch");
    let (out, err, code) = f.run(&["checkout", "-m", "side"]);
    assert_eq!(code, 0);
    assert_eq!(out, LISTING);
    assert_eq!(err, format!("{CONFLICT_WARNING}\nSwitched to branch 'side'\n"));
    assert_eq!(f.read(), MARKERS);
    assert_eq!(f.run(&["stash", "list"]).0, "stash@{0}: autostash while switching to 'side'\n");
}

#[test]
fn quiet_drops_the_blank_line_with_the_announcement() {
    let f = Fixture::new("quiet");
    let (out, err, code) = f.run(&["checkout", "-q", "-m", "side"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", CONFLICT_WARNING, 0));
    assert_eq!(f.read(), MARKERS);
}

#[test]
fn a_new_branch_and_a_detach_get_it_too() {
    let f = Fixture::new("new");
    let (out, err, code) = f.run(&["checkout", "-m", "-b", "nb", "side"]);
    assert_eq!(code, 0);
    assert_eq!(out, LISTING);
    assert_eq!(err, format!("{CONFLICT_WARNING}\nSwitched to a new branch 'nb'\n"));

    let f = Fixture::new("detach");
    let (out, err, code) = f.run(&["checkout", "-m", "--detach", "side"]);
    assert_eq!(code, 0);
    assert_eq!(out, LISTING);
    let id = f.run(&["rev-parse", "--short", "side"]).0;
    assert_eq!(err, format!("{CONFLICT_WARNING}\nHEAD is now at {} side\n", id.trim()));
}

#[test]
fn the_detached_head_advice_follows_the_blank_line() {
    let f = Fixture::new("advice");
    let (_, err, code) = f.run(&["checkout", "-m", "side~0"]);
    assert_eq!(code, 0);
    assert!(
        err.starts_with(&format!("{CONFLICT_WARNING}\nNote: switching to 'side~0'.\n\n")),
        "{err}"
    );
}

#[test]
fn switch_shares_it() {
    let f = Fixture::new("switch");
    let (out, err, code) = f.run(&["switch", "-m", "side"]);
    assert_eq!(code, 0);
    assert_eq!(out, LISTING);
    assert_eq!(err, format!("{CONFLICT_WARNING}\nSwitched to branch 'side'\n"));
}

#[test]
fn a_clean_reapply_has_no_blank_line() {
    let f = Fixture::new("clean");
    // `side` and `main` disagree on `g` only through the line the local edit
    // leaves alone, so the re-apply merges cleanly.
    std::fs::write(f.work.join("g"), "1\n2\n3\n4\n5\n").unwrap();
    f.run(&["checkout", "-q", "-f", "main"]);
    f.run(&["add", "g"]);
    f.run(&["commit", "-q", "-m", "g"]);
    f.run(&["checkout", "-q", "-b", "g2"]);
    std::fs::write(f.work.join("g"), "1\n2\n3\n4\nX\n").unwrap();
    f.run(&["commit", "-q", "-a", "-m", "g2"]);
    f.run(&["checkout", "-q", "main"]);
    std::fs::write(f.work.join("g"), "Y\n2\n3\n4\n5\n").unwrap();
    let (out, err, code) = f.run(&["checkout", "-m", "g2"]);
    assert_eq!(code, 0);
    assert_eq!(out, "The following paths have local changes:\nM\tg\n");
    assert_eq!(err, "Applied autostash.\nSwitched to branch 'g2'\n");
}
