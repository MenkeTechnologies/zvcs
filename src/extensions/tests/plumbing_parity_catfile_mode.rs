//! `cat-file`'s `%(objectmode)` atom, and the option chain that decides which
//! flags need a batch mode.
//!
//!   * `%(objectmode)` prints `%06o` of `data->mode`, and only when that is not
//!     `S_IFINVALID` (builtin/cat-file.c:360-363). `expand_data` starts at
//!     `S_IFINVALID` (`EXPAND_DATA_INIT`) and only `data->mode = ctx.mode`
//!     (builtin/cat-file.c:629) replaces it, from the `oc->mode` a path operand
//!     resolved through — the index entry's `ce_mode` for `:<path>`
//!     (object-name.c:1811) or the tree entry's mode for `<rev>:<path>`
//!     (object-name.c:1852). A plain object id, and every `--batch-all-objects`
//!     record, therefore expand it to the empty string rather than to `000000`.
//!   * `cmd_cat_file()` rejects `--follow-symlinks`, `--buffer`,
//!     `--batch-all-objects`, `-z` and `-Z` outside batch mode in exactly that
//!     order, and the chain runs *before* the `<object> required with '-e'`
//!     arity checks (builtin/cat-file.c:1185-1201 vs 1229-1249). `--buffer` was
//!     missing from the chain entirely, so `git cat-file --buffer` printed a
//!     bare usage with no `fatal:` line at all.
//!   * A gitlink resolves through `get_tree_entry()`, which never reads the
//!     named object, so the operand succeeds and the odb read is what fails.
//!     `batch_object_write()` then tells the two apart by the mode alone:
//!     `S_IFGITLINK` reports `<gitlink oid> submodule` — keyed by the oid,
//!     because `obj_name` is passed NULL — and everything else reports
//!     `<operand> missing` (builtin/cat-file.c:471,505-511).
//!   * `batch.buffer_output` is a tristate: `--no-buffer` counts as "given", and
//!     an unset one defaults to `batch.all_objects`, so `--batch-all-objects`
//!     buffers and a plain `--batch` does not (builtin/cat-file.c:1210-1211).
//!
//! Every expectation was read off stock git 2.55.0 in the same throwaway
//! repository, under the same pinned environment.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// `f` (regular), `d/g` (executable), `link` (symlink to `f`), all in one
    /// commit on `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cf-mode-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("d")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "hi\n").unwrap();
        std::fs::write(f.work.join("d/g"), "yo\n").unwrap();
        std::os::unix::fs::symlink("f", f.work.join("link")).unwrap();
        std::fs::set_permissions(
            f.work.join("d/g"),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
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

    /// Feed `stdin` to `git <args>` and return its stdout.
    fn batch(&self, args: &[&str], stdin: &str) -> String {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "`git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

/// The mode of the entry the operand was found at, for both path spellings, and
/// nothing at all for an operand with no path arm.
#[test]
fn cat_file_objectmode_renders_the_resolved_entry_mode() {
    let f = Fixture::new("atom");
    let fmt = "--batch-check=[%(objectmode)] %(objecttype)";

    assert_eq!(f.batch(&["cat-file", fmt], "HEAD:f\n"), "[100644] blob\n");
    assert_eq!(f.batch(&["cat-file", fmt], "HEAD:d/g\n"), "[100755] blob\n");
    assert_eq!(f.batch(&["cat-file", fmt], "HEAD:d\n"), "[040000] tree\n");
    assert_eq!(f.batch(&["cat-file", fmt], "HEAD:link\n"), "[120000] blob\n");
    // `:<path>` reads `ce_mode` off the index rather than a tree.
    assert_eq!(f.batch(&["cat-file", fmt], ":f\n"), "[100644] blob\n");

    // No path arm: `oc->mode` stays `S_IFINVALID` and the atom is empty — not
    // `000000`, which is what a naive `%06o` of a zeroed mode would print.
    assert_eq!(f.batch(&["cat-file", fmt], "HEAD\n"), "[] commit\n");
    assert_eq!(f.batch(&["cat-file", fmt], "HEAD^{tree}\n"), "[] tree\n");
    // `HEAD:` reaches the tree walk but never calls `find_tree_entry()`.
    assert_eq!(f.batch(&["cat-file", fmt], "HEAD:\n"), "[] tree\n");

    // `--batch-all-objects` drives `batch_object_cb()`, which sets only the oid.
    let all = f.batch(&["cat-file", "--batch-all-objects", "--batch-check=[%(objectmode)]"], "");
    assert!(
        all.lines().all(|l| l == "[]"),
        "every --batch-all-objects record must have an empty mode: {all:?}"
    );

    // `--follow-symlinks` resolves through `get_tree_entry_follow_symlinks()`,
    // which fills the same `oc->mode`: the *target's* mode, not the link's.
    assert_eq!(
        f.batch(&["cat-file", fmt, "--follow-symlinks"], "HEAD:link\n"),
        "[100644] blob\n"
    );
}

/// Each batch-mode-only flag names itself, `--buffer` included, and the chain
/// wins over the arity check that would otherwise blame the missing operand.
#[test]
fn cat_file_batch_only_flags_are_refused_in_chain_order() {
    let f = Fixture::new("chain");
    let want = |flag: &str| (String::new(), format!("'{flag}' requires a batch mode"), 129);

    for (args, flag) in [
        (vec!["cat-file", "--buffer"], "--buffer"),
        // `--no-buffer` still counts as given: git tests `buffer_output >= 0`.
        (vec!["cat-file", "--no-buffer"], "--buffer"),
        (vec!["cat-file", "--follow-symlinks"], "--follow-symlinks"),
        (vec!["cat-file", "--batch-all-objects"], "--batch-all-objects"),
        (vec!["cat-file", "-z"], "-z"),
        (vec!["cat-file", "-Z"], "-Z"),
        // The chain runs before `<object> required with '-e'`.
        (vec!["cat-file", "-e", "--buffer"], "--buffer"),
        (vec!["cat-file", "-p", "--buffer"], "--buffer"),
        (vec!["cat-file", "-t", "--follow-symlinks"], "--follow-symlinks"),
        // `--follow-symlinks` is first in the chain, so it wins over `--buffer`.
        (vec!["cat-file", "--buffer", "--follow-symlinks"], "--follow-symlinks"),
    ] {
        let (out, err, code) = f.run(&args);
        let (want_out, want_line, want_code) = want(flag);
        assert_eq!(out, want_out, "{args:?} wrote to stdout");
        assert_eq!(code, want_code, "{args:?} exit code");
        assert_eq!(
            err.lines().next().unwrap_or_default(),
            format!("fatal: {want_line}"),
            "{args:?} first stderr line"
        );
    }

    // In batch mode every one of them is accepted.
    assert_eq!(
        f.batch(&["cat-file", "--batch-check", "--buffer"], "HEAD:f\n")
            .split_whitespace()
            .nth(1),
        Some("blob")
    );
}

/// A gitlink entry whose commit lives in another repository: resolvable as a
/// tree entry, unreadable as an object, reported `submodule` and never
/// `missing`.
#[test]
fn cat_file_batch_reports_a_gitlink_as_a_submodule() {
    let f = Fixture::new("gitlink");
    let sub = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    std::fs::write(
        f.work.join("mktree-in"),
        format!("160000 commit {sub}\tsub\n"),
    )
    .unwrap();
    let mut child = f
        .cmd(&["mktree"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("160000 commit {sub}\tsub\n").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "mktree failed: {out:?}");
    let tree = String::from_utf8(out.stdout).unwrap().trim().to_string();

    let spec = format!("{tree}:sub\n");
    assert_eq!(
        f.batch(&["cat-file", "--batch-check"], &spec),
        format!("{sub} submodule\n")
    );
    // `--batch` takes the same early return: no contents follow the status line.
    assert_eq!(
        f.batch(&["cat-file", "--batch"], &spec),
        format!("{sub} submodule\n")
    );
    // The mode is what made the difference, and it is still renderable.
    assert_eq!(
        f.batch(&["cat-file", "--batch-check=[%(objectmode)]"], &spec),
        format!("{sub} submodule\n")
    );

    // A path the tree does not carry is still `missing`, keyed by the operand.
    let absent = format!("{tree}:nope\n");
    assert_eq!(
        f.batch(&["cat-file", "--batch-check"], &absent),
        format!("{tree}:nope missing\n")
    );
}
