//! Two config keys `--pretty=raw` suppresses, and a valueless
//! `log.excludeDecoration`.
//!
//! ```c
//! if (rev->pretty_given && rev->commit_format == CMIT_FMT_RAW) {
//!         /*
//!          * "log --pretty=raw" is special; ignore UI oriented
//!          * configuration variables such as decoration.
//!          */
//!         if (!cfg->decoration_given)
//!                 cfg->decoration_style = 0;
//!         if (!rev->abbrev_commit_given)
//!                 rev->abbrev_commit = 0;
//! }
//! ```
//! (`cmd_log_init_finish()`, builtin/log.c:348-357, v2.55.0)
//!
//! `--pretty=raw` reproduces the object as it is stored, so `log.decorate` and
//! `log.abbrevCommit` are dropped unless the command line asked for them itself.
//! `decoration_given` is set by every spelling of the option, `--no-decorate`
//! included (`decorate_callback()`, builtin/log.c:181); `abbrev_commit_given` is
//! set only by `--abbrev-commit` (revision.c:2649-2653).
//!
//! The other key is read with the multi-value reader:
//!
//! ```c
//! if (!repo_config_get_string_multi(the_repository, "log.excludeDecoration",
//!                                  &config_exclude)) {
//!         struct string_list_item *item;
//!         for_each_string_list_item(item, config_exclude)
//!                 string_list_append(decoration_filter->exclude_ref_config_pattern,
//!                                    item->string);
//! }
//! ```
//! (`set_default_decoration_filter()`, builtin/log.c:229-235)
//!
//! A valueless occurrence makes that reader write `error: missing value for
//! '<key>'` (config.c:3554) and return non-zero, so the `if` skips the whole list
//! and the command still exits 0. Reading it as an empty pattern instead — which
//! this port did — excluded *every* ref, so a stray `[log] excludeDecoration`
//! silently stripped the decorations it was meant to keep.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-rawcfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "a\n").unwrap();
        f.git(&["add", "file"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["tag", "v1"]);
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

    fn append_config(&self, text: &str) {
        let path = self.work.join(".git/config");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(text);
        std::fs::write(&path, body).unwrap();
    }

    fn first_line(&self, args: &[&str]) -> String {
        self.stdout(args).lines().next().unwrap_or_default().to_string()
    }
}

/// With `--pretty=raw` on the command line the two UI keys are ignored: the
/// commit header carries the full object name and no decoration.
#[test]
fn pretty_raw_ignores_log_decorate_and_log_abbrev_commit() {
    let f = Fixture::new("raw");
    let full = f.stdout(&["rev-parse", "HEAD"]).trim().to_string();

    let line = f.first_line(&[
        "-c",
        "log.abbrevCommit=true",
        "-c",
        "log.decorate=full",
        "log",
        "--pretty=raw",
    ]);
    assert_eq!(line, format!("commit {full}"));

    // Each key on its own, so neither half of the rule can be passing by accident.
    assert_eq!(
        f.first_line(&["-c", "log.abbrevCommit=true", "log", "--pretty=raw"]),
        format!("commit {full}")
    );
    assert_eq!(
        f.first_line(&["-c", "log.decorate=short", "log", "--pretty=raw"]),
        format!("commit {full}")
    );
}

/// The command line still wins: an explicit `--decorate` or `--abbrev-commit`
/// sets the `given` flag the rule is guarded by.
#[test]
fn an_explicit_flag_survives_pretty_raw() {
    let f = Fixture::new("rawflag");
    let short = f.stdout(&["rev-parse", "--short", "HEAD"]).trim().to_string();

    let line = f.first_line(&["log", "--pretty=raw", "--abbrev-commit", "--decorate"]);
    assert_eq!(line, format!("commit {short} (HEAD -> main, tag: v1)"));

    // `log.abbrevCommit` alone plus an explicit `--decorate`: only the decoration
    // is kept, because only its `given` flag was set.
    let full = f.stdout(&["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(
        f.first_line(&["-c", "log.abbrevCommit=true", "log", "--pretty=raw", "--decorate"]),
        format!("commit {full} (HEAD -> main, tag: v1)")
    );
}

/// A raw format that came from `format.pretty` is not `pretty_given`, so the two
/// keys still apply.
#[test]
fn a_raw_format_from_config_is_not_pretty_given() {
    let f = Fixture::new("rawcfg");
    let short = f.stdout(&["rev-parse", "--short", "HEAD"]).trim().to_string();
    assert_eq!(
        f.first_line(&["-c", "format.pretty=raw", "-c", "log.abbrevCommit=true", "log"]),
        format!("commit {short}")
    );
}

/// A valueless `log.excludeDecoration` reports itself on stderr and then excludes
/// nothing, at exit 0.
#[test]
fn a_valueless_exclude_decoration_excludes_nothing() {
    let f = Fixture::new("noval");
    let decorated = f.stdout(&["log", "--decorate=short", "--format=%H%d"]);
    assert!(decorated.contains("(HEAD -> main, tag: v1)"), "{decorated}");

    f.append_config("[log]\n\texcludeDecoration\n");
    let (out, err, code) = f.run(&["log", "--decorate=short", "--format=%H%d"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "error: missing value for 'log.excludeDecoration'\n");
    assert_eq!(out, decorated);
}

/// A key that does have values still excludes what it names, and a valueless
/// occurrence beside them discards the whole list rather than part of it.
#[test]
fn values_are_still_honoured_and_one_valueless_entry_discards_them_all() {
    let f = Fixture::new("someval");

    f.append_config("[log]\n\texcludeDecoration = refs/tags/*\n");
    let out = f.stdout(&["log", "--decorate=short", "--format=%H%d"]);
    assert!(out.contains("(HEAD -> main)"), "{out}");
    assert!(!out.contains("tag: v1"), "{out}");

    f.append_config("[log]\n\texcludeDecoration\n");
    let (out, err, code) = f.run(&["log", "--decorate=short", "--format=%H%d"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "error: missing value for 'log.excludeDecoration'\n");
    assert!(out.contains("tag: v1"), "{out}");
}
