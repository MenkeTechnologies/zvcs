//! `rm` in a sparse checkout whose `info/sparse-checkout` cone mode cannot read.
//!
//! `cmd_rm()` loads the index's sparse patterns at two points, and `add_pattern_to_hashsets()`
//! (dir.c:798-936) prints its `unrecognized pattern` / `disabling cone pattern matching` pair at
//! the first of them, once per `index_state` (dir.c:1557-1574): `refresh_index()`
//! (builtin/rm.c:307) when it hashes a matching entry its stat data cannot vouch for and
//! `convert_attrs()` reads `.gitattributes` from the index (attr.c:821), and, without
//! `--sparse`, the cache loop's `path_in_sparse_checkout()` (builtin/rm.c:316-319). zvcs matched
//! the paths as stock does but printed neither. The refresh also records the file's stat data in
//! every matching entry whose content is unchanged, and `write_locked_index()` (builtin/rm.c:442)
//! writes it with the removal; zvcs wrote the entries with the stat data it had read.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// How the tracked files stand when `rm` runs.
#[derive(Clone, Copy)]
enum Files {
    /// `top`, `a/x` and `c/z` rewritten with a longer content: the size tells the refresh they
    /// changed, so it hashes nothing.
    Grown,
    /// Every file's content kept and its mtime moved into the past: the refresh has to hash
    /// each matching entry to clear it.
    Backdated,
}

/// One commit over `top`, `a/x`, `c/z` and `c/d/w` in a cone-mode sparse checkout whose pattern
/// file holds only `/a/*`, read as the non-cone list that keeps `a/` alone.
fn prepare(stock: &str, r: &Path, files: Files) {
    let paths = ["top", "a/x", "c/z", "c/d/w"];
    std::fs::create_dir_all(r.join("a")).unwrap();
    std::fs::create_dir_all(r.join("c/d")).unwrap();
    for path in paths {
        std::fs::write(r.join(path), "1\n").unwrap();
    }
    let ok = |args: &[&str]| assert_eq!(run(stock, r, args).2, 0, "{args:?}");
    ok(&["init", "-q", "-b", "main", "."]);
    ok(&["add", "."]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    match files {
        Files::Grown => {
            for path in ["top", "a/x", "c/z"] {
                std::fs::write(r.join(path), "22\n").unwrap();
            }
        }
        Files::Backdated => {
            let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
            for path in paths {
                std::fs::File::options().write(true).open(r.join(path)).unwrap().set_modified(past).unwrap();
            }
        }
    }
    ok(&["config", "core.sparseCheckout", "true"]);
    ok(&["config", "core.sparseCheckoutCone", "true"]);
    std::fs::write(r.join(".git/info/sparse-checkout"), "/a/*\n").unwrap();
}

fn compare(tag: &str, files: Files, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-rm-cone-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r, files);
        let out = run(bin, &r, args);
        let mut staged = run(stock, &r, &["ls-files", "-s", "-t"]).0;
        // Whether each entry carries its file's backdated mtime: the stat data the refresh recorded
        // and `write_locked_index()` (builtin/rm.c:442) persisted.
        if let Files::Backdated = files {
            let debug = run(stock, &r, &["ls-files", "--debug"]).0;
            for line in debug.lines() {
                match line.strip_prefix("  mtime: ") {
                    Some(mtime) => staged.push_str(&format!("  backdated: {}\n", mtime == "1500000000:0")),
                    None if !line.starts_with(' ') => staged.push_str(&format!("{line}\n")),
                    None => {}
                }
            }
        }
        outputs.push((out, staged));
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn the_path_checks_load_the_definition_with_its_warnings() {
    compare("outside", Files::Grown, &["rm", "top"]);
    compare("inside-cached", Files::Grown, &["rm", "--cached", "a/x"]);
    compare("dir-dry-run", Files::Grown, &["rm", "-n", "-r", "c"]);
    compare("unmatched", Files::Grown, &["rm", "nomatch"]);
}

#[test]
fn under_sparse_only_a_refresh_that_hashes_loads_the_definition() {
    // The size changed, so nothing is hashed and nothing loads the patterns.
    compare("sparse-grown", Files::Grown, &["rm", "--sparse", "--cached", "top"]);
    // Clearing the stat-dirty entry hashes it, through the index's `.gitattributes` lookup.
    compare("sparse-backdated", Files::Backdated, &["rm", "--sparse", "top"]);
    compare("sparse-backdated-dir", Files::Backdated, &["rm", "--sparse", "-r", "--cached", "c"]);
}

#[test]
fn the_refreshed_stat_data_is_written_with_the_removal() {
    // `refresh_index()` (builtin/rm.c:307) gives `top`, `c/z` and `c/d/w` their backdated
    // mtime although only `a/x` is removed.
    compare("refresh-written", Files::Backdated, &["rm", "-r", "--cached", "."]);
}
