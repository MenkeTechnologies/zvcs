//! `add` and `mv` in a sparse checkout whose `info/sparse-checkout` cone mode cannot read.
//!
//! `path_in_sparse_checkout()` loads the file into the index through
//! `init_sparse_checkout_patterns()` (dir.c:1557-1622), and `add_pattern_to_hashsets()`
//! (dir.c:798-936) gives up on a pattern that is not cone-shaped: it warns, once per
//! `index_state`, and from there the list is matched as the non-cone list it is
//! (`pl->use_cone_patterns = 0`). `/a/*` then takes in nothing at the top level, so `top`
//! is outside the definition, where zvcs read the file as a cone that includes every
//! top-level file and added it.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// One commit over `top`, `a/x` and `c/z`, all three then modified in the worktree, in a
/// cone-mode sparse checkout whose pattern file holds only `/a/*`. `attrs` puts an untracked
/// `.gitattributes` in the worktree's root and `a/`, so no attribute lookup reads the index.
fn prepare(stock: &str, r: &Path, attrs: bool) {
    std::fs::create_dir_all(r.join("a")).unwrap();
    std::fs::create_dir_all(r.join("c")).unwrap();
    for path in ["top", "a/x", "c/z"] {
        std::fs::write(r.join(path), "1\n").unwrap();
    }
    let ok = |args: &[&str]| assert_eq!(run(stock, r, args).2, 0, "{args:?}");
    ok(&["init", "-q", "-b", "main", "."]);
    ok(&["add", "."]);
    ok(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    for path in ["top", "a/x", "c/z"] {
        std::fs::write(r.join(path), "22\n").unwrap();
    }
    if attrs {
        std::fs::write(r.join(".gitattributes"), "").unwrap();
        std::fs::write(r.join("a/.gitattributes"), "").unwrap();
    }
    ok(&["config", "core.sparseCheckout", "true"]);
    ok(&["config", "core.sparseCheckoutCone", "true"]);
    std::fs::write(r.join(".git/info/sparse-checkout"), "/a/*\n").unwrap();
}

fn compare(tag: &str, attrs: bool, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!("zvcs-add-cone-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let r = base.join(side);
        std::fs::create_dir_all(&r).unwrap();
        prepare(stock, &r, attrs);
        let out = run(bin, &r, args);
        let staged = run(stock, &r, &["diff", "--cached", "--name-status"]).0;
        outputs.push((out, staged));
    }
    assert_eq!(outputs[1], outputs[0], "{tag}: {args:?}");
}

#[test]
fn a_cone_file_cone_mode_cannot_read_is_matched_as_a_non_cone_list() {
    compare("top", false, &["add", "top"]);
    compare("both", false, &["add", "c/z", "top"]);
    compare("inside", false, &["add", "a/x"]);
    compare("dry-run", false, &["add", "-n", "top"]);
    compare("mv", false, &["mv", "top", "a/"]);
}

#[test]
fn the_definition_warns_once_without_an_attribute_read_from_the_index() {
    // Every lookup finds a `.gitattributes` on disk, so the warnings are the path checks'.
    compare("attrs-top", true, &["add", "top"]);
    compare("attrs-inside", true, &["add", "a/x"]);
    compare("attrs-all", true, &["add", "-A"]);
}
