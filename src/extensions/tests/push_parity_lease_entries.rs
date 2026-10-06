//! `--force-with-lease` is a list, not a single setting.
//!
//! `parse_push_cas_option()` (remote.c:2723-2751) appends one entry per
//! `--force-with-lease=<ref>[:<expect>]` and lets the bare form set
//! `use_tracking_for_rest` beside them; `apply_cas()` (remote.c:2911-2945) gives
//! each ref the first entry naming it and only falls back to the bare form for
//! the rest. `<ref>:` with nothing after the colon expects the ref to be absent,
//! and an `<expect>` `repo_get_oid()` cannot resolve is an `error()` from the
//! option callback, so `parse_options()` exits 129. zvcs kept only the last
//! occurrence (a bare one discarded every explicit entry), read `<ref>:` as a
//! lease against the tracking ref, and died at exit 1 with a `zvcs:` prefix.
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// `main` pushed to `origin` = `../r.git`, then amended so the next push is
    /// not a fast-forward.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-push-lease-entries-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        for args in [
            &["init", "-q", "--bare", "../r.git"][..],
            &["init", "-q", "-b", "main", "."],
            &["commit", "-q", "--allow-empty", "-m", "a"],
            &["remote", "add", "origin", "../r.git"],
            &["push", "-q", "origin", "main"],
            &["commit", "-q", "--amend", "--allow-empty", "-m", "b"],
        ] {
            assert_eq!(f.run(args).2, 0, "{args:?}");
        }
        f
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

const STALE: &str = "To ../r.git\n ! [rejected]        main -> main (stale info)\nerror: failed to push some refs to '../r.git'\n";

#[test]
fn an_empty_expect_means_the_ref_must_not_exist() {
    let f = Fixture::new("empty");
    assert_eq!(f.run(&["push", "--force-with-lease=main:", "origin", "main"]), (String::new(), STALE.into(), 1));
}

#[test]
fn an_unresolvable_expect_is_an_option_error() {
    let f = Fixture::new("unresolvable");
    assert_eq!(
        f.run(&["push", "--force-with-lease=main:nosuch", "origin", "main"]),
        (String::new(), "error: cannot parse expected object name 'nosuch'\n".into(), 129)
    );
}

#[test]
fn an_explicit_entry_outranks_the_bare_form_and_survives_later_entries() {
    let f = Fixture::new("entries");
    // `main:HEAD` names the amended commit, which the remote does not have.
    assert_eq!(
        f.run(&["push", "--force-with-lease=main:HEAD", "--force-with-lease", "origin", "main"]),
        (String::new(), STALE.into(), 1)
    );
    // The `main` entry still leases `main` after an entry for another ref.
    let (out, err, code) =
        f.run(&["push", "--force-with-lease=main:origin/main", "--force-with-lease=other", "origin", "main"]);
    assert_eq!((out.as_str(), code), ("", 0), "{err}");
    assert!(err.ends_with(" main -> main (forced update)\n"), "{err}");
}
