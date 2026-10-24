//! `OPT_FILENAME`'s value step, `get_value()` (parse-options.c:209-235): one
//! leading `:(optional)` is stripped, the rest goes through `fix_filename()`, and
//! a name `is_missing_file()` reports absent leaves the option as it was — an
//! earlier occurrence, or the command's config. An empty name stores NULL (and
//! behind `:(optional)` dies `could not stat (null)`), a `stat()` failure other
//! than ENOENT dies at parse time, and the `--no-` form changes nothing.
//!
//! Every `OPT_FILENAME` this port parses is driven through both binaries on
//! identical fixtures: `--pathspec-from-file` (add, rm, reset, restore, checkout,
//! stash push, commit), `commit -F`/`-t`, `tag -F`, `-O` (diff, log, show,
//! format-patch), `format-patch --signature-file`/`--description-file`,
//! `check-mailmap --mailmap-file`, `archive -o`, `fmt-merge-msg -F` and
//! `sparse-checkout check-rules --rules-file`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");
#[path = "support/stock_git.rs"]
mod stock_git;

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, root: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", root)
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

impl Fixture {
    /// The same repository for either binary, built with stock git so the two
    /// start from identical bytes.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-optfilename-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        let f = Fixture { root, repo };
        f.setup(stock, &["init", "-q", "-b", "main"]);
        for (path, body) in [("f", "x\n"), ("g", "y\n"), ("sub/s", "s\n")] {
            std::fs::write(f.repo.join(path), body).unwrap();
        }
        f.setup(stock, &["add", "."]);
        f.setup(stock, &["commit", "-q", "-m", "init"]);
        for (path, body) in [
            ("f", "x\nz\n"),
            ("g", "y\nz\n"),
            ("sub/s", "s\nz\n"),
            ("m", "msg\n"),
            ("order", "g\nf\n"),
            ("list", "f\n"),
            ("sub/list", "s\n"),
            ("tmpl", "tmpl\n"),
            ("mm", "A <a@b> B <b@c>\n"),
            ("sig", "sig\n"),
            // A file named `-` at the top: `prefix_filename_except_for_dash()`
            // keeps `:(optional)-` unprefixed, so even from `sub/` this is the
            // file `is_missing_file()` stats.
            ("-", "f\n"),
        ] {
            std::fs::write(f.repo.join(path), body).unwrap();
        }
        f.setup(stock, &["config", "diff.orderFile", "order"]);
        f
    }

    fn setup(&self, stock: &str, args: &[&str]) {
        let o = run(stock, &self.root, &self.repo, args);
        assert!(o.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn run(&self, bin: &str, sub: &str, args: &[&str]) -> String {
        let o = run(bin, &self.root, &self.repo.join(sub), args);
        format!(
            "status={:?}\nstdout:\n{}\nstderr:\n{}",
            o.status.code(),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    }
}

/// One line per case: `(subdirectory, argv)`. Each case runs in a fresh pair of
/// fixtures so a command that writes (commit, tag, stash, reset) cannot leak
/// into the next.
const CASES: &[(&str, &[&str])] = &[
    ("", &["add", "-n", "--pathspec-from-file=:(optional)list"]),
    ("", &["add", "-n", "--pathspec-from-file=:(optional)nope"]),
    ("", &["add", "-n", "--pathspec-from-file=list", "--pathspec-from-file=:(optional)nope"]),
    ("", &["add", "-n", "--pathspec-from-file=list", "--no-pathspec-from-file"]),
    ("", &["add", "-n", "--pathspec-from-file=:(optional)nope", "-p"]),
    ("", &["add", "-n", "--pathspec-from-file=:(optional)"]),
    ("", &["add", "-n", "--pathspec-from-file=:(optional)f/x", "--bogus"]),
    ("", &["add", "-n", "--pathspec-from-file=:(optional)-"]),
    ("sub", &["add", "-n", "--pathspec-from-file=:(optional)list"]),
    ("sub", &["add", "-n", "--pathspec-from-file=:(optional)-"]),
    ("sub", &["add", "-n", "--pathspec-from-file=:(optional)f/x"]),
    ("sub", &["add", "-n", "--pathspec-from-file=nope"]),
    ("", &["rm", "-n", "--pathspec-from-file=:(optional)list"]),
    ("", &["rm", "-n", "--pathspec-from-file=:(optional)nope"]),
    ("", &["reset", "-q", "--pathspec-from-file=:(optional)nope"]),
    ("", &["restore", "--pathspec-from-file=:(optional)nope"]),
    ("", &["checkout", "--pathspec-from-file=:(optional)nope", "--", "f"]),
    ("", &["stash", "push", "--pathspec-from-file=:(optional)nope"]),
    ("", &["commit", "-F", ":(optional)nope", "-m", "viaM"]),
    ("", &["commit", "--allow-empty", "-F", "m", "-F", ":(optional)nope"]),
    ("", &["commit", "--allow-empty", "-F", "m", "--no-file"]),
    ("", &["commit", "--allow-empty", "-F", "m", "-F", ""]),
    ("", &["commit", "-F", ":(optional)f/x", "-m", "x"]),
    ("", &["-c", "commit.template=tmpl", "commit", "--allow-empty", "-t", ""]),
    ("", &["-c", "commit.template=tmpl", "commit", "--allow-empty", "--no-template"]),
    ("", &["-c", "commit.template=tmpl", "commit", "--allow-empty", "-t", ":(optional)nope"]),
    ("", &["tag", "-F", ":(optional)nope", "-m", "tagm", "t1"]),
    ("", &["tag", "-F", "m", "--no-file", "t2"]),
    ("", &["diff", "--name-only", "-O", ":(optional)nope"]),
    ("", &["diff", "--name-only", "-O", "/dev/null", "-O", ":(optional)nope"]),
    ("", &["diff", "--name-only", "-O", ""]),
    ("", &["log", "-1", "-O", ":(optional)nope", "--name-only", "--format=%s", "-p"]),
    ("", &["show", "-O", ":(optional)nope", "--name-only", "--format=%s"]),
    ("", &["format-patch", "--stdout", "-1", "-O", ""]),
    ("", &["format-patch", "--stdout", "-1", "--signature-file=sig", "--signature-file=:(optional)nope"]),
    ("", &["check-mailmap", "--mailmap-file=mm", "--mailmap-file=:(optional)nope", "A <a@b>"]),
    ("", &["check-mailmap", "--mailmap-file=mm", "--no-mailmap-file", "A <a@b>"]),
    ("", &["archive", "-o", "out.tar", "-o", ":(optional)nope", "--format=tar", "HEAD", "f"]),
    ("", &["fmt-merge-msg", "-F", ":(optional)nope"]),
    ("", &["sparse-checkout", "check-rules", "--rules-file=:(optional)nope"]),
    ("", &["sparse-checkout", "check-rules", "--rules-file=list", "--no-rules-file"]),
];

#[test]
fn opt_filename_values_match_stock() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else {
        eprintln!("skipping: no stock git >= 2.56.0 installed");
        return;
    };
    for (i, (sub, args)) in CASES.iter().enumerate() {
        let want = Fixture::new(stock, &format!("s{i}"));
        let got = Fixture::new(stock, &format!("z{i}"));
        let expected = want.run(stock, sub, args);
        let actual = got.run(BIN, sub, args);
        assert_eq!(actual, expected, "case {i}: `git {args:?}` in `{sub}`");
        // What the command left behind, seen by stock: the index, the refs and
        // the stash, so a write that differs fails even when the output agrees.
        for probe in [
            &["status", "--porcelain"][..],
            &["for-each-ref", "--format=%(refname) %(objectname)"][..],
            &["log", "-1", "--format=%B", "--all"][..],
        ] {
            assert_eq!(
                got.run(stock, "", probe),
                want.run(stock, "", probe),
                "case {i}: post-state `{probe:?}` after `git {args:?}`"
            );
        }
    }
}
