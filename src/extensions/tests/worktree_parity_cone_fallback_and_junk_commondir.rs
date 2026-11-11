//! Two `worktree` paths that read a file zvcs took on trust.
//!
//! * `worktree add` in a cone-mode sparse checkout: the `reset --hard` child loads
//!   the copied `info/sparse-checkout` through `add_pattern_to_hashsets()`
//!   (dir.c:798-936), which warns about a pattern cone mode cannot read and then
//!   matches the file as the non-cone list it is; a `/<dir>/` whose parents the file
//!   never names leaves their files out. The load happens once in
//!   `populate_from_existing_patterns()` (unpack-trees.c:1832-1840) and once more
//!   for the attributes of the first file checked out (attr.c:821,
//!   dir.c:1557-1574), so the warnings come twice when anything is written. zvcs
//!   read every such file as a cone of its positive lines, silently.
//! * A linked worktree whose `commondir` names somewhere else: its ref store takes
//!   shared refs from that directory (setup.c:312-350), so `list` shows its branch
//!   at the null id, a `commondir` with a missing inner component dies in
//!   `strbuf_add_real_path()`, and `validate_worktree()` (worktree.c:362-431)
//!   refuses `move` and `remove` with `error code 7`. zvcs listed the main
//!   repository's branch and moved or removed the worktree.
//!
//! Each case runs stock git and zvcs on twin fixtures and compares the output
//! with the fixture's own path replaced, and the resulting worktree state.

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

/// `top`, `a/x`, `a/b/y` and `c/z` committed in `<root>/r`.
fn base_repo(stock: &str, root: &Path) {
    let r = root.join("r");
    std::fs::create_dir_all(r.join("a/b")).unwrap();
    std::fs::create_dir_all(r.join("c")).unwrap();
    for (path, body) in [
        ("top", "1\n"),
        ("a/x", "2\n"),
        ("a/b/y", "3\n"),
        ("c/z", "4\n"),
    ] {
        std::fs::write(r.join(path), body).unwrap();
    }
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["add", "."],
        &["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"],
    ] {
        assert_eq!(run(stock, &r, args).2, 0, "{args:?}");
    }
}

/// Date the checked-out `paths` in the past before the `check` commands run. Whether
/// stock's `status` hashes a file, and so loads the sparse patterns for its
/// attributes and warns, otherwise depends on whether it was written in the same
/// second as the index (read-cache.c:355-366 is_racy_stat()); a past mtime makes
/// every entry stat-dirty on both sides, so the content check always runs.
fn backdate(root: &Path, paths: &[&str]) {
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    for path in paths {
        let path = root.join(path);
        if path.is_file() {
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(past).unwrap();
        }
    }
}

/// Run `steps` (a subdirectory and the arguments for the binary under test) on twin
/// fixtures `prepare` builds with stock git, then `check` (stock-git commands that
/// report the resulting state), and compare everything, along with which of the
/// `paths` exist afterwards.
fn compare(
    tag: &str,
    prepare: &dyn Fn(&str, &Path),
    steps: &[(&str, &[&str])],
    check: &[(&str, &[&str])],
    paths: &[&str],
) {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let base = std::env::temp_dir().join(format!(
        "zvcs-worktree-cone-commondir-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let _twin = Twin { root: base.clone() };
    let mut outputs = Vec::new();
    for (side, bin) in [("s", stock), ("z", BIN)] {
        let root = base.join(side);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        prepare(stock, &root);
        let cut = |s: String| s.replace(root.to_str().unwrap(), "ROOT");
        let mut out = Vec::new();
        for (dir, args) in steps {
            let (o, e, c) = run(bin, &root.join(dir), args);
            out.push((cut(o), cut(e), c));
        }
        backdate(&root, paths);
        for (dir, args) in check {
            let (o, e, c) = run(stock, &root.join(dir), args);
            out.push((cut(o), cut(e), c));
        }
        for path in paths {
            out.push((
                path.to_string(),
                String::new(),
                i32::from(root.join(path).exists()),
            ));
        }
        outputs.push(out);
    }
    assert_eq!(outputs[1], outputs[0]);
}

fn sparse_add(tag: &str, cone: &str, file: &'static str) {
    let cone = cone.to_owned();
    let prepare = move |stock: &str, root: &Path| {
        base_repo(stock, root);
        let r = root.join("r");
        assert_eq!(
            run(stock, &r, &["config", "core.sparseCheckout", "true"]).2,
            0
        );
        assert_eq!(
            run(stock, &r, &["config", "core.sparseCheckoutCone", &cone]).2,
            0
        );
        std::fs::create_dir_all(r.join(".git/info")).unwrap();
        std::fs::write(r.join(".git/info/sparse-checkout"), file).unwrap();
    };
    compare(
        tag,
        &prepare,
        &[("r", &["worktree", "add", "-q", "../wt"])],
        &[
            ("wt", &["ls-files", "-t"]),
            ("wt", &["status", "--porcelain"]),
        ],
        &["wt/top", "wt/a/x", "wt/a/b/y", "wt/c/z"],
    );
}

#[test]
fn worktree_add_reads_a_file_cone_mode_cannot_as_patterns_with_the_warnings() {
    sparse_add("plain-dir", "true", "/*\n!/*/\na/\n");
    sparse_add("glob", "true", "/a/*\n");
    sparse_add("excludes-all", "true", "junk\n");
    sparse_add("negative", "true", "!/a\n");
    sparse_add("repeated", "true", "/*\n!/*/\n/a/\n!/a/*/\n/a/\n");
}

#[test]
fn worktree_add_takes_a_cones_parents_from_the_file() {
    sparse_add("no-parent-pair", "true", "/*\n!/*/\n/a/b/\n");
    sparse_add("escaped", "true", "/*\n!/*/\n/a/\\*/\n");
    sparse_add("well-formed", "true", "/*\n!/*/\n/a/\n!/a/*/\n/a/b/\n");
    sparse_add("non-cone", "false", "/*\n!/*/\na/\n");
}

fn junk_commondir(tag: &str, commondir: &'static str) {
    let prepare = move |stock: &str, root: &Path| {
        base_repo(stock, root);
        let r = root.join("r");
        for args in [
            &["worktree", "add", "-q", "../wt1"][..],
            &["worktree", "add", "-q", "../wt2"],
        ] {
            assert_eq!(run(stock, &r, args).2, 0, "{args:?}");
        }
        std::fs::write(r.join(".git/worktrees/wt1/commondir"), commondir).unwrap();
    };
    compare(
        tag,
        &prepare,
        &[
            ("r", &["worktree", "list"]),
            ("r", &["worktree", "list", "--porcelain"]),
            ("wt2", &["worktree", "list", "-v"]),
            ("r", &["worktree", "repair"]),
            ("r", &["worktree", "prune", "-n", "-v"]),
            ("r", &["worktree", "lock", "../wt1"]),
            ("r", &["worktree", "unlock", "../wt1"]),
            ("r", &["worktree", "move", "../wt1", "../wt1m"]),
            ("r", &["worktree", "remove", "../wt1"]),
            ("r", &["worktree", "remove", "-f", "../wt1"]),
            ("r", &["worktree", "prune", "-v"]),
        ],
        &[("r", &["for-each-ref", "--format=%(refname) %(objectname)"])],
        &[
            "r/.git/worktrees/wt1",
            "r/.git/worktrees/wt2",
            "wt1",
            "wt1m",
            "wt2",
        ],
    );
}

#[test]
fn a_junk_commondir_resolves_nothing_and_fails_validation() {
    junk_commondir("junk", "junk\n");
    junk_commondir("absent", "/nonexistent\n");
    junk_commondir("newline", "\n");
    junk_commondir("wrong-git-dir", "../../.git\n");
}

#[test]
fn a_commondir_with_a_missing_inner_component_dies() {
    junk_commondir("inner", "x/y/z\n");
}

#[test]
fn the_real_commondir_changes_nothing() {
    junk_commondir("real", "../..\n");
    junk_commondir("real-crlf", "../..\r\n");
}
