//! `git --work-tree=<missing> apply` from inside `.git`.
//!
//! `apply` is `RUN_SETUP_GENTLY`; when the named work tree cannot be entered git simply stays
//! where it is and takes the patch's paths as given, so a patch that cannot be opened is
//! `error: can't open patch` (128), `--stat` still reports, and a patch that wants a file
//! fails on that file. zvcs canonicalised the work tree to chdir into it and died with the
//! raw `No such file or directory (os error 2)` at 1.


#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let root = dir.to_string_lossy();
    let scrub = |b: &[u8]| String::from_utf8_lossy(b).replace(root.as_ref(), "<ROOT>");
    (scrub(&out.stdout), scrub(&out.stderr), out.status.code())
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-applywt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("a"), "a\n").unwrap();
    std::fs::write(
        dir.join("p.patch"),
        "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n",
    )
    .unwrap();
    run(stock, &dir, &[], &["init", "-q", "-b", "main"]);
    run(stock, &dir, &[], &["add", "a"]);
    assert_eq!(run(stock, &dir, &[], &["commit", "-qm", "one"]).2, Some(0));
    dir
}

#[test]
fn an_unenterable_work_tree_leaves_the_process_where_it_stands() {
    let Some(stock) = stock_git() else { return };
    let patch = "../p.patch";
    let cases: Vec<Vec<&str>> = vec![
        vec!["--work-tree=src", "apply", "does-not-exist", "quilt/series"],
        vec!["--work-tree=src", "apply", patch],
        vec!["--work-tree=src", "apply", "--check", patch],
        vec!["--work-tree=src", "apply", "--stat", patch],
        vec!["--work-tree=src", "apply", "--numstat", "--summary", patch],
        vec!["--work-tree=src", "apply", "--cached", patch],
        vec!["--work-tree=src", "apply", "--index", patch],
        vec!["--work-tree=src", "apply", "-"],
        vec!["--glob-pathspecs", "--work-tree=src", "apply", "-z", "--allow-empty", "does-not-exist"],
    ];
    let envs: [&[(&str, &str)]; 1] = [&[]];
    for env in envs {
        for args in &cases {
            let mut seen = Vec::new();
            for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
                let dir = fixture(who, stock);
                let git_dir = dir.join(".git");
                let nosuch = dir.join(".git/no-such-objects").to_string_lossy().into_owned();
                let env: Vec<(&str, &str)> =
                    env.iter().map(|(k, _)| (*k, nosuch.as_str())).collect();
                seen.push(run(bin, &git_dir, &env, args));
                let _ = std::fs::remove_dir_all(&dir);
            }
            assert_eq!(seen[1], seen[0], "env {env:?} args {args:?}");
        }
    }
}
