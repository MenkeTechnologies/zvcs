//! `.gitattributes` `filter` drivers: what git says when one fails, and what it
//! stages when it does.
//!
//! `apply_filter()` (convert.c:996-1022) answers 0 for every way a driver can fail to
//! deliver content — no `clean`/`smudge` command at all, a long-running process that
//! does not advertise the capability, a process that answered `error`/`abort`, or a
//! single-file program that exited non-zero. Its callers then split on one thing:
//! `if (!ret && ca.drv && ca.drv->required)` dies naming the path and the driver
//! (convert.c:1441, :1517-1518), and without `required` the *unfiltered* content is
//! what the rest of the conversion sees, because git left its `dst` untouched.
//!
//! Getting either half wrong is not a cosmetic difference: a non-required driver that
//! fails must not fail the command, and a required one must not silently stage the raw
//! file. Every expectation here is stock git 2.55.0's output for the same repository.
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
        let root = std::env::temp_dir().join(format!("zvcs-convdrv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
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

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn staged(&self) -> String {
        self.run(&["ls-files", "--stage"]).1
    }
}

/// The blob id stock git 2.55.0 writes for `hello\n`, i.e. the file unfiltered.
const HELLO_BLOB: &str = "ce013625030ba8dba906f756967f9e9ca394464a";

/// The two lines `apply_single_file_filter()` leaves on stderr when the program exits
/// non-zero: one from the async half naming the status (convert.c:694) and one from the
/// parent half (convert.c:735). Both name the command as configured.
const FAILED_FALSE: &str = "error: external filter 'false' failed 1\nerror: external filter 'false' failed\n";

/// A `clean` driver that fails, without `required`: git reports it and stages the file
/// as it stands, because `apply_filter()` answering 0 leaves `convert_to_git()`'s `dst`
/// untouched (convert.c:1440-1447).
///
/// `update-index` is the verb under test because it converts each path exactly once, so
/// the report can be compared in full rather than searched.
#[test]
fn failing_clean_driver_reports_both_of_gits_error_lines_and_stages_the_original() {
    let f = Fixture::new("cleanfail");
    f.write("f.txt", b"hello\n");
    f.write(".gitattributes", b"f.txt filter=x\n");
    f.git(&["config", "filter.x.clean", "false"]);

    let (code, _out, err) = f.run(&["update-index", "--add", "f.txt"]);
    assert_eq!(code, 0, "a driver without `required` cannot fail the command; stderr: {err}");
    assert_eq!(err, FAILED_FALSE, "git names the command and the exit status");
    assert_eq!(f.staged(), format!("100644 {HELLO_BLOB} 0\tf.txt\n"));
}

/// A `required` driver that has no `clean` command at all. `apply_filter()` returns 0
/// from its last line (convert.c:1021) exactly as it does for a driver that failed, and
/// `convert_to_git()` cannot tell the two apart — so this dies with the same message.
///
/// Configuring only `filter.x.required` is what creates the driver: `read_convert_config()`
/// makes an entry for any `filter.<name>.<key>` it meets (convert.c:1036-1046).
#[test]
fn required_driver_with_no_clean_command_dies_naming_path_and_driver() {
    let f = Fixture::new("reqnocmd");
    f.write("f.txt", b"hello\n");
    f.write(".gitattributes", b"f.txt filter=x\n");
    f.git(&["config", "filter.x.required", "true"]);

    let (code, _out, err) = f.run(&["add", "f.txt"]);
    assert_eq!(code, 128, "git `die()`s; stderr: {err}");
    assert_eq!(err, "fatal: f.txt: clean filter 'x' failed\n");
    assert_eq!(f.staged(), "", "the die happens before anything is indexed");
}

/// A `required` driver whose `clean` program fails: the same `die()`, after the two
/// `error()` lines the program's failure produced.
#[test]
fn required_clean_driver_failure_dies_after_reporting_the_program() {
    let f = Fixture::new("reqfail");
    f.write("f.txt", b"hello\n");
    f.write(".gitattributes", b"f.txt filter=x\n");
    f.git(&["config", "filter.x.clean", "false"]);
    f.git(&["config", "filter.x.required", "true"]);

    let (code, _out, err) = f.run(&["add", "f.txt"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, format!("{FAILED_FALSE}fatal: f.txt: clean filter 'x' failed\n"));
    assert_eq!(f.staged(), "");
}

/// The same rule on the way out. `convert_to_working_tree_internal()` dies for a failed
/// `required` smudge driver (convert.c:1517-1518) — note that git quotes the driver name
/// in the `clean` message and leaves it bare here — and `die()` is exit 128, not this
/// command's own per-entry `error:` and exit 1.
#[test]
fn required_smudge_driver_failure_is_fatal_in_checkout_index() {
    let f = Fixture::new("smudgefail");
    f.write("f.txt", b"hello\n");
    f.write(".gitattributes", b"f.txt filter=x\n");
    f.git(&["config", "filter.x.smudge", "false"]);
    f.git(&["update-index", "--add", "f.txt"]);
    f.git(&["config", "filter.x.required", "true"]);
    std::fs::remove_file(f.work.join("f.txt")).unwrap();

    let (code, _out, err) = f.run(&["checkout-index", "-f", "-a"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, format!("{FAILED_FALSE}fatal: f.txt: smudge filter x failed\n"));
    assert!(
        !f.work.join("f.txt").exists(),
        "git writes nothing when the smudge filter it required failed"
    );
}

/// `checkout-index` converts *to* git in exactly one place: the content compare that a
/// racy index entry forces, which is `ce_compare_data()` -> `index_fd(..., flags = 0)`
/// (read-cache.c:210-221). `get_conv_flags(0)` answers a plain 0 (object-file.c:33-41),
/// without `global_conv_flags_eol`, so the `core.safecrlf` round-trip check does not run
/// and git says nothing about the line endings of a file it is only reading.
///
/// The racy path is the one that matters here, so the entry is made racy the way git's
/// own tests do: write the file within the same second as the index.
#[test]
fn checkout_index_never_runs_the_safecrlf_round_trip_check() {
    let f = Fixture::new("safecrlf");
    f.write(".gitattributes", b"* text=auto\n");
    f.write("f.txt", b"l1\nl2\n");
    f.git(&["add", "-A", "."]);
    f.git(&["config", "core.autocrlf", "true"]);
    // `core.safecrlf` defaults to `warn`, which is what makes a stray check audible.

    let (code, _out, err) = f.run(&["checkout-index", "-f", "-a"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(err, "", "git checkout-index warns about nothing here");
}
