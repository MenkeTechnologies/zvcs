//! `git rev-parse --exclude-hidden=<section>` calls `exclude_hidden_refs()`
//! (revision.c:1600-1616): the section must be `fetch`, `receive` or
//! `uploadpack`, a second use dies, and `parse_hide_refs_config()`
//! (refs.c:1688-1708) collects `transfer.hideRefs` and `<section>.hideRefs` in
//! configuration order, dropping trailing slashes. The next ref walk's
//! `show_reference()` skips what `ref_is_hidden()` hides — last matching
//! pattern wins, `!` un-hides (refs.c:1710-1740) — and `clear_ref_exclusions()`
//! then forgets it. `--branches`, `--tags` and `--remotes` refuse to follow it
//! with `return error(...)`, exit 255 (builtin/rev-parse.c:958-979). zvcs
//! refused the option as "not ported yet".
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
        let root = std::env::temp_dir().join(format!("zvcs-rev-parse-exclude-hidden-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write("a", "a\n");
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn write(&self, path: &str, body: &str) {
        let path = self.work.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args, None)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> (String, String, i32) {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or_default()).unwrap();
        drop(pipe);
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

impl Fixture {
    /// `main`, `hid`, tag `t1` and `refs/hidden/x`, with hide rules whose
    /// order matters: the `!refs/heads/hid` in `[transfer]` comes before the
    /// `[uploadpack]` rule that hides it again.
    fn hiding(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.run(&["branch", "hid"]);
        f.run(&["tag", "t1"]);
        f.run(&["update-ref", "refs/hidden/x", "HEAD"]);
        f.run(&["config", "transfer.hideRefs", "refs/hidden/"]);
        f.run(&["config", "uploadpack.hideRefs", "refs/heads/hid"]);
        f.run(&["config", "--add", "transfer.hideRefs", "!refs/heads/hid"]);
        f.run(&["config", "receive.hideRefs", "refs/tags"]);
        f
    }
}

#[test]
fn each_section_hides_its_own_refs_for_one_walk() {
    let f = Fixture::hiding("walk");
    let ok = |out: &str| (out.to_string(), String::new(), 0);
    assert_eq!(
        f.run(&["rev-parse", "--symbolic", "--exclude-hidden=fetch", "--all"]),
        ok("refs/heads/hid\nrefs/heads/main\nrefs/tags/t1\n")
    );
    assert_eq!(
        f.run(&["rev-parse", "--symbolic", "--exclude-hidden=uploadpack", "--all"]),
        ok("refs/heads/main\nrefs/tags/t1\n")
    );
    // Only the first walk after it is filtered.
    assert_eq!(
        f.run(&["rev-parse", "--symbolic", "--exclude-hidden=receive", "--all", "--all"]),
        ok("refs/heads/hid\nrefs/heads/main\nrefs/heads/hid\nrefs/heads/main\nrefs/hidden/x\nrefs/tags/t1\n")
    );
}

#[test]
fn refusals() {
    let f = Fixture::hiding("refuse");
    assert_eq!(
        f.run(&["rev-parse", "--exclude-hidden=bogus", "--all"]),
        (String::new(), "fatal: unsupported section for hidden refs: bogus\n".into(), 128)
    );
    assert_eq!(
        f.run(&["rev-parse", "--exclude-hidden=fetch", "--exclude-hidden=receive"]),
        (String::new(), "fatal: --exclude-hidden= passed more than once\n".into(), 128)
    );
    assert_eq!(
        f.run(&["rev-parse", "--exclude-hidden=fetch", "--tags"]),
        (String::new(), "error: options '--exclude-hidden' and '--tags' cannot be used together\n".into(), 255)
    );
    assert_eq!(
        f.run(&["-c", "transfer.hideRefs", "rev-parse", "--exclude-hidden=fetch", "--all"]),
        (
            String::new(),
            "error: missing value for 'transfer.hiderefs'\n\
             fatal: unable to parse 'transfer.hiderefs' from command-line config\n"
                .into(),
            128
        )
    );
}
