//! `git diff --no-index` against stock git: `--relative[=<p>]` / `--no-relative` are in the
//! option table and do nothing (there is no tree to take a prefix from), and an option the
//! no-index table lacks (`--cached`) is a usage error at 129 wherever it stands among them.
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
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

fn tree(root: &Path) {
    for (dir, body) in [("d1", "a\n"), ("d2", "b\n")] {
        std::fs::create_dir_all(root.join(dir).join("sub")).unwrap();
        std::fs::write(root.join(dir).join("sub/f"), body).unwrap();
    }
    std::fs::write(root.join("d1/g"), "x\n").unwrap();
}

#[test]
fn relative_is_accepted_and_ignored() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-ni-relative-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    tree(&base);
    for args in [
        &["diff", "--no-index", "--relative", "d1", "d2"][..],
        &["diff", "--no-index", "--relative=sub", "d1", "d2"],
        &["diff", "--no-index", "--no-relative", "--numstat", "d1", "d2"],
        &["diff", "--no-index", "--numstat", "--relative", "--cached", "d1", "d2"],
        &["diff", "--no-index", "--src-prefix=X/", "-Sfn", "--relative", "--cached", "d1", "d2"],
    ] {
        let want = run(stock, &base, args);
        assert_eq!(run(BIN, &base, args), want, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
