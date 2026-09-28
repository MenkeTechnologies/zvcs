//! `git checkout` on a branch yet to be born.
//!
//! * With no operand `switch_branches()` names the branch "HEAD" at the current
//!   commit and dies when there is none: `fatal: You are on a branch yet to be
//!   born` (builtin/checkout.c:1195-1199), for `--detach`, `-f` and the bare form
//!   alike, and for `switch --detach`. zvcs said it in lower case as `zvcs:
//!   checkout: …` at 1, or ran `git checkout` into an `ambiguous argument 'HEAD'`
//!   and exited 0.
//! * `HEAD` and `@` are revisions only while `HEAD` resolves;
//!   `parse_branchname_arg()` (builtin/checkout.c:1476-1518) otherwise takes them
//!   as pathspecs — `error: pathspec 'HEAD' did not match …` at 1, or under
//!   `--detach` the `does not take a path argument` refusal — and with `--` after
//!   one dies `invalid reference: HEAD`. That last refusal holds for any operand
//!   that does not resolve and has no remote to DWIM from: zvcs restored
//!   `nosuch --` as a path.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-checkout-unborn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
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

fn err(msg: &str, code: i32) -> (String, String, i32) {
    (String::new(), format!("{msg}\n"), code)
}

#[test]
fn no_operand_has_no_commit_to_take() {
    let f = Fixture::new("none");
    let unborn = err("fatal: You are on a branch yet to be born", 128);
    for args in [
        &["checkout"][..],
        &["checkout", "--detach"][..],
        &["checkout", "-q", "--detach"][..],
        &["checkout", "-f"][..],
        &["switch", "--detach"][..],
        &["switch", "-f", "--detach"][..],
    ] {
        assert_eq!(f.run(args), unborn, "{args:?}");
    }
}

#[test]
fn head_is_a_pathspec_until_it_resolves() {
    let f = Fixture::new("head");
    for staged in [false, true] {
        if staged {
            std::fs::write(f.work.join("f"), "x\n").unwrap();
            f.run(&["add", "f"]);
        }
        assert_eq!(f.run(&["checkout", "HEAD"]), err("error: pathspec 'HEAD' did not match any file(s) known to git", 1));
        assert_eq!(f.run(&["checkout", "@"]), err("error: pathspec '@' did not match any file(s) known to git", 1));
        assert_eq!(
            f.run(&["checkout", "--detach", "HEAD"]),
            err("fatal: git checkout: --detach does not take a path argument 'HEAD'", 128)
        );
    }
    assert_eq!(f.run(&["checkout", "-f", "HEAD"]), err("error: pathspec 'HEAD' did not match any file(s) known to git", 1));
    assert_eq!(f.run(&["checkout", "HEAD", "--"]), err("fatal: invalid reference: HEAD", 128));
    assert_eq!(f.run(&["checkout", "--detach", "HEAD", "--"]), err("fatal: invalid reference: HEAD", 128));
}

#[test]
fn an_unresolved_operand_before_dashdash_is_an_invalid_reference() {
    let f = Fixture::new("dashdash");
    f.run(&["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "c1"]);
    assert_eq!(f.run(&["checkout", "nosuch", "--"]), err("fatal: invalid reference: nosuch", 128));
}
