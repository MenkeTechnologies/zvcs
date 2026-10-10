//! `commit-graph --object-dir` is read from the work tree top.
//!
//! The option is a plain `OPT_STRING` (builtin/commit-graph.c:55), never run through
//! `prefix_filename()`, and `commit-graph` is `RUN_SETUP`, so `setup_git_directory()` has already
//! moved git to the top of the work tree. `odb_find_source_or_die()` then resolves the string with
//! `real_pathdup(path, 1)`: a relative value names a path under the top, and a missing
//! intermediate component is `fatal: Invalid path '<resolved so far>'`. zvcs resolved the value
//! against the directory the user typed it in.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn a_relative_object_dir_names_a_path_under_the_top_from_a_subdirectory() {
    let Some(t) = Twin::new("cg-objdir-top") else { return };
    t.mkdir("work/sub");
    for args in [
        &["commit-graph", "--object-dir=.git/objects", "write", "--reachable"][..],
        &["commit-graph", "--object-dir=.git/objects", "verify"],
        &["commit-graph", "--object-dir=objects", "verify"],
        &["commit-graph", "--object-dir=../.git/objects", "verify"],
    ] {
        let (stock, zvcs) = t.run_in("work/sub", args);
        assert_eq!(zvcs, stock, "{args:?}");
    }
    let (stock, _) = t.run_in("work/sub", &["commit-graph", "--object-dir=.git/objects", "verify"]);
    assert_eq!(stock.code, 0, "{stock:?}");
    let (stock, _) = t.run_in("work/sub", &["commit-graph", "--object-dir=../.git/objects", "verify"]);
    assert_eq!(stock.code, 128, "{stock:?}");
}
