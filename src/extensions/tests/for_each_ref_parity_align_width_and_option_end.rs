//! Three ref-filter corners, each driven through stock git and zvcs on one
//! fixture that stock git built.
//!
//! * `%(align)`: `end_align_handler()` hands the block's output to
//!   `strbuf_utf8_align()` as a C string (ref-filter.c:1187), which measures it
//!   with `utf8_strnwidth()` (utf8.c:820-840). The block therefore stops at its
//!   first NUL — `%(raw)` of a blob holding one — and a wide character takes two
//!   columns, a combining one none. zvcs padded the whole buffer by its char
//!   count.
//! * `--end-of-options` ends `for-each-ref`'s and `tag`'s options as `--` does,
//!   by an exact `strcmp()` (parse-options.c:1113-1124); zvcs called it an
//!   unknown option.
//! * `%(is-base:<x>)` peels `<x>` with the non-quiet
//!   `lookup_commit_reference_by_name()` (ref-filter.c:913-926), so a blob says
//!   `error: object … is a blob, not a commit` before `failed to find`, as
//!   `%(ahead-behind:<x>)` already did.
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
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

impl Fixture {
    /// `main` with two commits, a blob holding a NUL tagged `nul`, the tree
    /// tagged `tree`, and annotated `v1.0`, `v1.1` and `v2.0`.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fer-align-eoo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        let f = Fixture { root, repo };
        f.setup(stock, &["init", "-q", "-b", "main"]);
        std::fs::write(f.repo.join("bin"), b"bin\0ary\n").unwrap();
        f.setup(stock, &["add", "bin"]);
        f.setup(stock, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
        f.setup(stock, &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "two"]);
        f.setup(stock, &["tag", "nul", "HEAD:bin"]);
        f.setup(stock, &["tag", "tree", "HEAD^{tree}"]);
        for v in ["v1.0", "v1.1", "v2.0"] {
            f.setup(stock, &["tag", "-a", "-m", v, v]);
        }
        f
    }

    fn setup(&self, stock: &str, args: &[&str]) {
        let out = run(stock, &self.root, &self.repo, args);
        assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Run `args` under both binaries and require identical stdout, stderr and
    /// exit status.
    fn same(&self, stock: &str, args: &[&str]) {
        let want = run(stock, &self.root, &self.repo, args);
        let got = run(BIN, &self.root, &self.repo, args);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), String::from_utf8_lossy(&got.stderr), got.status.code()),
            (String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&want.stderr), want.status.code()),
            "{args:?}"
        );
        assert_eq!(got.stdout, want.stdout, "{args:?} stdout bytes");
    }
}

#[test]
fn align_stops_at_a_nul_and_counts_display_columns() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "align");
    for format in [
        "%(align:5)%(raw)%(end)|",
        "%(align:2)%(raw)%(end)|",
        "%(align:9,right)%(raw)%(end)|",
        "%(align:6)\u{65e5}\u{672c}%(end)|",
        "%(align:6,middle)\u{65e5}\u{672c}%(end)|",
        "%(align:7,right)\u{65e5}\u{672c}%(end)|",
        "%(align:3)\u{65e5}\u{672c}%(end)|",
        "%(align:4)e%cc%81x%(end)|",
        "%(align:4)e%xcc%x81x%(end)|",
        "%(align:4)%xff%xfe%(end)|",
    ] {
        f.same(stock, &["for-each-ref", &format!("--format={format}"), "refs/tags/nul"]);
    }
    assert_eq!(
        run(BIN, &f.root, &f.repo, &["for-each-ref", "--format=%(align:5)%(raw)%(end)|", "refs/tags/nul"]).stdout,
        b"bin  |\n"
    );
}

#[test]
fn end_of_options_ends_the_options() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "eoo");
    for args in [
        &["for-each-ref", "--format=%(refname)", "--end-of-options", "refs/tags/v1*"][..],
        &["for-each-ref", "--end-of-options", "--format=%(refname)"],
        &["for-each-ref", "--end-of", "refs/tags"],
        &["tag", "-l", "--end-of-options", "v1*"],
        &["tag", "-l", "--end-of-options", "-n"],
        &["tag", "--end-of-options", "-l"],
        &["tag", "-n", "--end-of-options", "v2*", "v1.1"],
    ] {
        f.same(stock, args);
    }
}

#[test]
fn is_base_reports_a_non_commit_before_dying() {
    let Some(stock) = stock_git::stock_git() else { return };
    let f = Fixture::new(stock, "isbase");
    for format in ["%(is-base:nul)", "%(is-base:tree)", "%(is-base:v1.0)", "%(is-base:nope)", "%(ahead-behind:nul)"] {
        f.same(stock, &["for-each-ref", &format!("--format={format}")]);
    }
}
