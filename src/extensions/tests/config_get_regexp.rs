//! `git config --get-regexp <name-regex>` — the key-pattern read.
//!
//! Output shape is git's: one `key value` line per matching entry, separated by
//! a **space** (not the `=` of `--list`), keys git-normalized (section and value
//! names lower-cased, subsection case preserved), file order preserved with
//! multivars repeated rather than collapsed. No match is exit 1 with no output;
//! an invalid ERE is `error: invalid key pattern: <pattern>` at exit 6.
//!
//! `gh` runs this on every push (`^remote\..*\.gh-resolved$` and
//! `^branch\.<name>\.(remote|merge|pushremote|gh-merge-base)$`), so the two
//! anchored-alternation cases below are the real-world regression guard.
//!
//! Every case reads with `--local` so the assertions depend only on the repo
//! config this test writes — never on the developer's global/system files, which
//! would otherwise leak into the merged snapshot and make expected output
//! machine-specific.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the zvcs binary in `dir` and return the raw output.
fn zvcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(dir).output().expect("run zvcs git")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo whose LOCAL config carries a known set of entries: a multivar
/// (`remote.origin.fetch` twice), a subsection key, mixed-case names to prove
/// normalization, and the `branch.main.*` keys gh probes.
///
/// Named per test and per pid so concurrent test binaries never share a repo.
fn repo(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-cfgre-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir fixture");
    assert!(zvcs(&p, &["init", "-q", "-b", "main"]).status.success(), "init failed");

    let config = p.join(".git").join("config");
    let mut text = std::fs::read_to_string(&config).unwrap_or_default();
    text.push_str(
        r#"
[remote "origin"]
	url = git@github.com:o/r.git
	fetch = +refs/heads/*:refs/remotes/origin/*
	fetch = +refs/tags/*:refs/tags/*
[branch "main"]
	remote = origin
	merge = refs/heads/main
[User]
	Name = Ada
"#,
    );
    std::fs::write(&config, text).expect("write config");
    p
}

#[test]
fn matches_keys_in_file_order_with_multivars_repeated() {
    let dir = repo("keys");
    let out = zvcs(&dir, &["config", "--local", "--get-regexp", r"^remote\."]);

    assert!(out.status.success(), "exit {:?}", out.status.code());
    assert_eq!(
        stdout_of(&out),
        "remote.origin.url git@github.com:o/r.git\n\
         remote.origin.fetch +refs/heads/*:refs/remotes/origin/*\n\
         remote.origin.fetch +refs/tags/*:refs/tags/*\n",
        "space-separated, file order, both multivar values"
    );
}

#[test]
fn key_names_are_normalized_to_lowercase() {
    let dir = repo("lowercase");
    let out = zvcs(&dir, &["config", "--local", "--get-regexp", r"^user\."]);

    // `[User] Name` is written mixed-case; git reports `user.name`.
    assert_eq!(stdout_of(&out), "user.name Ada\n");
}

#[test]
fn gh_push_probes_resolve() {
    let dir = repo("gh");

    // gh's remote-resolution probe: no such key here, so exit 1 and no output.
    let resolved =
        zvcs(&dir, &["config", "--local", "--get-regexp", r"^remote\..*\.gh-resolved$"]);
    assert_eq!(resolved.status.code(), Some(1), "unmatched pattern is exit 1");
    assert!(stdout_of(&resolved).is_empty());

    // gh's branch probe: an anchored alternation that must match two of four.
    let branch = zvcs(
        &dir,
        &[
            "config",
            "--local",
            "--get-regexp",
            r"^branch\.main\.(remote|merge|pushremote|gh-merge-base)$",
        ],
    );
    assert!(branch.status.success());
    assert_eq!(stdout_of(&branch), "branch.main.remote origin\nbranch.main.merge refs/heads/main\n");
}

#[test]
fn name_only_drops_the_value_half() {
    let dir = repo("nameonly");
    let out =
        zvcs(&dir, &["config", "--local", "--name-only", "--get-regexp", r"^branch\."]);

    assert!(out.status.success());
    assert_eq!(stdout_of(&out), "branch.main.remote\nbranch.main.merge\n");
}

#[test]
fn invalid_key_pattern_is_exit_6() {
    let dir = repo("badpat");
    let out = zvcs(&dir, &["config", "--local", "--get-regexp", "["]);

    assert_eq!(out.status.code(), Some(6), "git reports a bad key pattern as exit 6");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: invalid key pattern: [\n");
    assert!(stdout_of(&out).is_empty());
}
