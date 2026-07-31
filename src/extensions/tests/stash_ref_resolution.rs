//! Which `<stash>` each subcommand accepts, and what it says when it will not.
//!
//! `get_stash_info()` takes either a `refs/stash` reflog entry (`stash@{n}`, or
//! a bare `n`) or any stash-like commit — two parents at least. `apply`, `show`
//! and `branch` are happy with either; `drop` and `pop` rewrite the reflog and
//! so require an entry of it (`assert_stash_ref()`).
//!
//! Also covered: `stash list` is `log -g --first-parent`, and the
//! `--first-parent` is what gives its diff options anything to describe — every
//! stash entry is a merge commit, which a plain reflog walk skips.
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
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two stash entries: `first` touches `a.txt`, `second` touches `b.txt`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stashref-r-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("a.txt"), "a\n").unwrap();
        std::fs::write(f.work.join("b.txt"), "b\n").unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        std::fs::write(f.work.join("a.txt"), "a\nfirst\n").unwrap();
        f.git(&["stash", "push", "-q", "-m", "first"]);
        std::fs::write(f.work.join("b.txt"), "b\nsecond\n").unwrap();
        f.git(&["stash", "push", "-q", "-m", "second"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn stash_oid(&self) -> String {
        self.run(&["rev-parse", "refs/stash"]).1.trim().to_string()
    }
}

/// `apply` takes any stash-like commit, named directly.
#[test]
fn apply_accepts_a_stash_like_commit() {
    let f = Fixture::new("apply-commitish");
    let oid = f.stash_oid();

    let (code, out, err) = f.run(&["stash", "apply", "-q", &oid]);
    assert_eq!(code, 0, "apply by object id failed: {out}{err}");
    assert_eq!(std::fs::read_to_string(f.work.join("b.txt")).unwrap(), "b\nsecond\n");
    // Applying does not consume the entry.
    assert_eq!(f.run(&["stash", "list"]).1.lines().count(), 2);
}

/// `pop` and `drop` rewrite the reflog, so they refuse a bare commit.
#[test]
fn pop_and_drop_require_a_stash_reference() {
    let f = Fixture::new("assert-ref");
    let oid = f.stash_oid();

    for sub in ["pop", "drop"] {
        let (code, out, err) = f.run(&["stash", sub, &oid]);
        assert_eq!(code, 1, "`stash {sub} <oid>` should fail: {out}{err}");
        assert_eq!(err, format!("error: '{oid}' is not a stash reference\n"), "stderr: {err}");
    }
    assert_eq!(f.run(&["stash", "list"]).1.lines().count(), 2, "nothing may be dropped");
}

/// An entry past the end of the log is `rev-parse`'s message, exit 128 — not a
/// generic "not a valid reference".
#[test]
fn an_out_of_range_entry_reports_the_log_length() {
    let f = Fixture::new("out-of-range");
    for sub in ["apply", "drop", "show"] {
        let (code, out, err) = f.run(&["stash", sub, "stash@{9}"]);
        assert_eq!(code, 128, "`stash {sub} stash@{{9}}`: {out}{err}");
        assert_eq!(err, "fatal: log for 'stash' only has 2 entries\n", "stderr: {err}");
    }
}

/// A commit that is not stash-shaped, and a name that resolves to nothing.
#[test]
fn a_non_stash_commit_and_an_unknown_name_are_reported_apart() {
    let f = Fixture::new("not-stash");
    let head = f.run(&["rev-parse", "HEAD"]).1.trim().to_string();

    let (code, _, err) = f.run(&["stash", "apply", &head]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, format!("fatal: '{head}' is not a stash-like commit\n"), "stderr: {err}");

    let (code, _, err) = f.run(&["stash", "apply", "nosuchthing"]);
    assert_eq!(code, 1, "stderr: {err}");
    assert_eq!(err, "error: nosuchthing is not a valid reference\n", "stderr: {err}");
}

/// `stash list` renders each entry's own diff for the formats the reflog port
/// supports — which only works because the walk is `--first-parent`.
#[test]
fn list_describes_each_entry_with_diff_options() {
    let f = Fixture::new("list-diff");

    let (code, out, err) = f.run(&["stash", "list", "--name-only"]);
    assert_eq!(code, 0, "stash list --name-only failed: {out}{err}");
    assert_eq!(
        out,
        "stash@{0}: On main: second\n\nb.txt\nstash@{1}: On main: first\n\na.txt\n",
        "each entry should name the file it stashed"
    );

    let (code, out, err) = f.run(&["stash", "list", "--numstat"]);
    assert_eq!(code, 0, "stash list --numstat failed: {out}{err}");
    assert!(out.contains("1\t0\tb.txt") && out.contains("1\t0\ta.txt"), "stdout: {out}");
}

/// Every message about a stash names the revision *as spelled*, because
/// `get_stash_info()` keeps `parse_stash_revision()`'s expansion in
/// `info->revision` and prints that. `drop` reports the entry it dropped under
/// the same spelling, and `-q` silences the line entirely.
#[test]
fn messages_name_the_revision_as_spelled() {
    let f = Fixture::new("spelling");
    let (code, out, err) = f.run(&["stash", "drop", "stash@{0}"]);
    assert_eq!(code, 0, "drop failed: {out}{err}");
    assert!(
        out.starts_with("Dropped stash@{0} ("),
        "the spelling must survive into the report: {out}"
    );

    // A bare number expands to the full ref, and that expansion is what shows.
    let (code, out, err) = f.run(&["stash", "drop", "0"]);
    assert_eq!(code, 0, "drop failed: {out}{err}");
    assert!(out.starts_with("Dropped refs/stash@{0} ("), "stdout: {out}");
}

/// `is_stash_ref` is decided by `repo_dwim_ref()` on everything before the first
/// `@`, so a suffix does not make a stash reference stop being one — and
/// `reflog_delete()` still reads the `@{<n>}` out of it, ignoring what follows
/// the closing brace.
#[test]
fn a_suffixed_stash_reference_still_names_its_entry() {
    let f = Fixture::new("suffixed");
    let newest = f.run(&["rev-parse", "stash@{0}"]).1.trim().to_string();

    let (code, out, err) = f.run(&["stash", "drop", "stash@{0}~0"]);
    assert_eq!(code, 0, "`drop stash@{{0}}~0` should have worked: {out}{err}");
    assert_eq!(out, format!("Dropped stash@{{0}}~0 ({newest})\n"), "stdout: {out}");
    assert_eq!(f.run(&["stash", "list"]).1.lines().count(), 1, "entry 0 must be gone");
}

/// `refs/stash` with no `@{…}` passes `assert_stash_ref()` — it dwims to the
/// stash ref — and then fails inside `reflog_delete()`, which needs a reflog
/// *spec*. Both of its error lines are git's.
#[test]
fn a_bare_stash_ref_is_not_a_reflog_spec() {
    let f = Fixture::new("bare-ref");
    let (code, out, err) = f.run(&["stash", "drop", "refs/stash"]);
    assert_eq!(code, 1, "`drop refs/stash` should have failed: {out}{err}");
    assert_eq!(
        err,
        "error: not a reflog: refs/stash\nerror: refs/stash: Could not drop stash entry\n",
        "stderr: {err}"
    );
    assert_eq!(f.run(&["stash", "list"]).1.lines().count(), 2, "nothing may be dropped");
}

/// The out-of-range message names the ref the way the revision spells it, since
/// it comes from `get_oid_basic()` parsing that very string.
#[test]
fn the_log_length_message_uses_the_spelled_ref() {
    let f = Fixture::new("log-len-spelling");
    let (code, _, err) = f.run(&["stash", "pop", "refs/stash@{9}"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, "fatal: log for 'refs/stash' only has 2 entries\n", "stderr: {err}");
}

/// `get_stash_info()` refuses a second revision by listing every one it was
/// given, quoted, on stderr and unprefixed.
#[test]
fn a_second_revision_is_refused_with_the_list() {
    let f = Fixture::new("too-many");
    for sub in ["apply", "show", "drop"] {
        let (code, out, err) = f.run(&["stash", sub, "stash@{0}", "stash@{1}"]);
        assert_eq!(code, 1, "`stash {sub}` with two revisions: {out}{err}");
        assert_eq!(
            err,
            "Too many revisions specified: 'stash@{0}' 'stash@{1}'\n",
            "stderr: {err}"
        );
    }
}
