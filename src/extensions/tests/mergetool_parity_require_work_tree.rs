//! `git mergetool` sources `git-sh-setup` and calls `require_work_tree`, which asks a fresh
//! `git rev-parse --is-inside-work-tree` and dies with `fatal: $0 cannot be used without a
//! working tree.` (exit 1) unless it says `true` — so a cwd outside the work tree
//! (`--work-tree=src` from the root) and a bare repository are refused alike, ahead of the
//! "No files need merging" answer. Compared against stock git; only the exec-path prefix of
//! `$0` differs by installation and is normalised away.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    // `fatal: <exec-path>/git-mergetool cannot ...` -> `fatal: git-mergetool cannot ...`
    let stderr = match text(&out.stderr).split_once("/git-mergetool") {
        Some((_, rest)) => format!("fatal: git-mergetool{rest}"),
        None => text(&out.stderr),
    };
    (text(&out.stdout), stderr, out.status.code())
}

fn fixture(stock: &str, root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    run(stock, root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("src/lib.rs"), "l\n").unwrap();
    run(stock, root, &["add", "."]);
    run(stock, root, &["commit", "-qm", "one"]);
}

#[test]
fn a_cwd_outside_the_work_tree_is_refused() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-mergetool-rwt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    fixture(stock, &base);

    for args in [
        &["--work-tree=src", "mergetool", "src"][..],
        &["--work-tree=src", "mergetool"],
        &["--work-tree=src", "--no-pager", "mergetool", "--", "lib.rs"],
    ] {
        let want = run(stock, &base, args);
        assert_eq!(want.2, Some(1), "{args:?}: {want:?}");
        assert_eq!(want.1, "fatal: git-mergetool cannot be used without a working tree.\n");
        assert_eq!(run(BIN, &base, args), want, "{args:?}");
    }
    // Inside the work tree the same command is the ordinary no-op.
    let args = ["mergetool"];
    let want = run(stock, &base.join("src"), &args);
    assert_eq!(want.0, "No files need merging\n");
    assert_eq!(run(BIN, &base.join("src"), &args), want);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_bare_repository_is_refused() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-mergetool-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    fixture(stock, &base);
    let bare = base.join("bare.git");
    run(stock, &base, &["clone", "-q", "--bare", ".", bare.to_str().unwrap()]);

    let want = run(stock, &bare, &["mergetool"]);
    assert_eq!(want.2, Some(1), "{want:?}");
    assert_eq!(run(BIN, &bare, &["mergetool"]), want);
    let _ = std::fs::remove_dir_all(&base);
}
