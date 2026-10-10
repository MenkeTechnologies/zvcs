//! A malformed remote setting dies in `branch_get()` / `for_each_remote()`.
//!
//! Stock git reads the whole remote configuration the first time a command asks about a
//! branch's remote (`read_config()`, remote.c), so `remote.origin.prune=none` is fatal for the
//! DWIM lookup of `checkout <name>`, for the tracking setup of a branch creation and for the
//! `report_tracking()` that closes a switch. zvcs never looked, so each of them ran to the
//! end. Expectations measured from stock git 2.56.0.

use std::path::PathBuf;
use std::process::Command;

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-checkout-bad-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["branch", "side"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
    }
}

const NUL_NEEDS_FILE: &str = "fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'\n";
const BAD: &str = "fatal: bad boolean config value 'none' for 'remote.origin.prune'\n";

fn with_bad_remote(args: &[&str]) -> Vec<String> {
    let mut v = vec!["-c".to_string(), "remote.origin.prune=none".to_string()];
    v.extend(args.iter().map(|s| s.to_string()));
    v
}

fn run_bad(f: &Fixture, args: &[&str]) -> (String, i32) {
    let owned = with_bad_remote(args);
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    f.run(&refs)
}

#[test]
fn dwim_lookup_of_an_unknown_name_dies_on_the_remote_config() {
    let f = Fixture::new("dwim");
    assert_eq!(run_bad(&f, &["checkout", "stray"]), (BAD.to_string(), 128));
    assert_eq!(run_bad(&f, &["checkout", "--pathspec-from-file=x", "stray"]), (BAD.to_string(), 128));
    assert_eq!(
        run_bad(&f, &["checkout", "--no-guess", "stray"]),
        ("error: pathspec 'stray' did not match any file(s) known to git\n".to_string(), 1)
    );
    // A name that is only a path asks the DWIM too; `--no-guess` is what skips it.
    assert_eq!(run_bad(&f, &["checkout", "a"]), (BAD.to_string(), 128));
}

#[test]
fn branch_creation_dies_before_the_branch_is_reported() {
    for (i, args) in [
        &["checkout", "-b", "nb"][..],
        &["switch", "-c", "nb"][..],
        &["branch", "nb"][..],
    ]
    .into_iter()
    .enumerate()
    {
        let f = Fixture::new(&format!("create{i}"));
        assert_eq!(run_bad(&f, args), (BAD.to_string(), 128), "{args:?}");
    }
}

#[test]
fn switching_reports_the_move_then_dies_in_report_tracking() {
    let f = Fixture::new("switch");
    assert_eq!(run_bad(&f, &["checkout", "side"]), (format!("Switched to branch 'side'\n{BAD}"), 128));
    // The move happened before the die.
    assert_eq!(f.run(&["symbolic-ref", "--short", "HEAD"]).1, 0);
    let f = Fixture::new("switch-same");
    assert_eq!(run_bad(&f, &["switch", "main"]), (format!("Already on 'main'\n{BAD}"), 128));
    // `-q` skips `report_tracking()` altogether.
    assert_eq!(run_bad(&f, &["checkout", "-q", "side"]), (String::new(), 0));
}
