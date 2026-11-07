//! A local source that is missing, is no repository, or is a plain file, as
//! `clone`, `fetch`, `ls-remote`, `remote show`, `push` and `submodule add` reach it.
//!
//! A path `get_repo_path()` does not claim, and every `file://` URL, goes to
//! `git_connect()`, which starts `upload-pack`/`receive-pack` on the path
//! `parse_connect_url()` takes out of the URL (connect.c:1067-1106) — `/srv/r` for
//! `file:///srv/r`. That child's `enter_repo()` (setup.c:1821-1906) settles on the
//! first of `<p>/.git`, `<p>`, `<p>.git/.git`, `<p>.git` that is a git directory or a
//! regular file and reads it with the dying `read_gitfile()`, so a plain file is
//! `invalid gitfile format: <candidate>`; finding none is upload-pack's
//! `'<p>' does not appear to be a git repository`. The parent then dies in
//! `die_initial_contact()`, and a clone's `remove_junk()` takes the new directory
//! down. zvcs's clone reported gitoxide's `Could not verify that "file://..." url is
//! a valid git directory` at exit 1, and the other commands quoted the whole URL.
//!
//! `get_repo_path_1()` reads a candidate that starts `gitdir: ` with the dying
//! `read_gitfile()` too (builtin/clone.c:113-127), before `Cloning into`; and
//! `transport_get()` reports a plain file that is not a bundle (transport.c:1258)
//! before `git_connect()` checks the transport is allowed, which is the order a
//! `submodule add` clone child, run with `GIT_PROTOCOL_FROM_USER=0`, prints them in.
//!
//! Each case runs stock git and zvcs on identical fixtures and compares stdout,
//! stderr, the exit status and what is left in the fixture's top directory.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
#[path = "support/stock_git.rs"]
mod stock_git;

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let noperm = self.root.join("noperm");
        if noperm.exists() {
            let _ = Command::new("chmod").arg("755").arg(&noperm).status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(format!("{}-home", self.root.display()));
    }
}

/// `HOME` and `ZVCS_HOME` sit beside the fixture, so zvcs's own state does not
/// show up among the entries [`top_level`] compares.
fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let home = PathBuf::from(format!("{}-home", dir.display()));
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PROTOCOL_FROM_USER")
        .env_remove("GIT_ALLOW_PROTOCOL")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// `up` (a repository with one commit), `w` (a work repository), `notrepo/` (an
/// empty directory), `noperm/` (a directory nobody may enter), `afile` (a plain
/// file) and `gf` (a gitfile naming nothing), all built with stock git.
fn fixture(stock: &str, tag: &str, case: usize) -> Fixture {
    let base = std::env::temp_dir().join(format!("zvcs-clone-missing-src-{}-{case}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let root = base.canonicalize().unwrap();
    for args in [
        &["init", "-q", "-b", "main", "up"][..],
        &["-C", "up", "commit", "-q", "--allow-empty", "-m", "a"],
        &["init", "-q", "-b", "main", "w"],
        &["-C", "w", "commit", "-q", "--allow-empty", "-m", "b"],
    ] {
        assert_eq!(run(stock, &root, args).2, 0, "{args:?}");
    }
    std::fs::create_dir(root.join("notrepo")).unwrap();
    std::fs::create_dir(root.join("noperm")).unwrap();
    assert!(Command::new("chmod").arg("000").arg(root.join("noperm")).status().unwrap().success());
    std::fs::write(root.join("afile"), "hi\n").unwrap();
    std::fs::write(root.join("gf"), "gitdir: nowhere\n").unwrap();
    Fixture { root }
}

fn top_level(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// One case: each `{D}` in an argument is the fixture's root; the output has the
/// root replaced by `<D>` so the two runs compare.
fn compare(case: usize, steps: &[&[&str]]) {
    let Some(stock) = stock_git::stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let mut results = Vec::new();
    for (tag, bin) in [("stock", stock), ("zvcs", ZVCS)] {
        let fx = fixture(stock, tag, case);
        let root = fx.root.display().to_string();
        let mut transcript = Vec::new();
        for step in steps {
            let args: Vec<String> = step.iter().map(|a| a.replace("{D}", &root)).collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let (out, err, code) = run(bin, &fx.root, &args);
            transcript.push((out.replace(&root, "<D>"), err.replace(&root, "<D>"), code));
        }
        results.push((transcript, top_level(&fx.root)));
    }
    assert_eq!(results[0], results[1], "case {case}: {steps:?}");
}

#[test]
fn clone_of_a_file_url_that_is_no_repository_dies_in_the_upload_pack_child() {
    compare(1, &[&["clone", "file:///nowhere/at/all", "x"]]);
    compare(2, &[&["clone", "file://{D}/notrepo", "x"]]);
    compare(3, &[&["clone", "file://{D}/notrepo/", "x"]]);
    compare(4, &[&["clone", "file://{D}/noperm", "x"]]);
    compare(5, &[&["clone", "file://{D}/afile", "x"]]);
    compare(6, &[&["clone", "--bare", "-q", "file://{D}/gf", "x"]]);
}

#[test]
fn clone_of_a_gitfile_that_names_no_repository_dies_before_the_banner() {
    compare(10, &[&["clone", "./gf", "x"]]);
    compare(11, &[&["clone", "./gf"]]);
    compare(12, &[&["clone", "./afile", "x"]]);
}

#[test]
fn the_child_names_the_path_not_the_url() {
    compare(20, &[&["-C", "w", "ls-remote", "file://{D}/notrepo"]]);
    compare(21, &[&["-C", "w", "remote", "add", "r", "file://{D}/notrepo"], &["-C", "w", "remote", "show", "r"]]);
    compare(22, &[&["-C", "w", "remote", "add", "r", "file:///nowhere"], &["-C", "w", "fetch", "r"]]);
    compare(23, &[&["-C", "w", "fetch", "file://{D}/afile"]]);
}

#[test]
fn a_plain_file_is_read_as_a_gitfile_by_the_child() {
    compare(30, &[&["-C", "w", "fetch", "../afile"]]);
    compare(31, &[&["-C", "w", "ls-remote", "../gf"]]);
    compare(32, &[&["-C", "w", "pull", "../afile"]]);
    compare(33, &[&["-C", "w", "remote", "add", "r", "../afile"], &["-C", "w", "remote", "show", "r"]]);
    compare(34, &[&["-C", "w", "push", "../afile", "HEAD:refs/heads/x"]]);
}

#[test]
fn submodule_add_of_a_plain_file_reports_the_bundle_check_first() {
    compare(40, &[&["-C", "w", "submodule", "add", "../afile", "s"]]);
    compare(41, &[&["-C", "w", "-c", "protocol.file.allow=always", "submodule", "add", "../afile", "s"]]);
    compare(42, &[&["-C", "w", "submodule", "add", "../gf", "s"]]);
}
