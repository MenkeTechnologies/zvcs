//! `create_branch()` dies with the ref transaction's error text (branch.c:665-673),
//! so a branch whose name conflicts with an existing one is refused in git's
//! words, whether the conflicting branch is loose (`lock_raw_ref()`,
//! refs/files-backend.c:795-829, :903-934) or only in `packed-refs` (:3064-3082),
//! and nothing is written. `update-ref` and `branch -m` go through the same check.
//!
//! Each case builds identical fixtures with stock git and runs the command with
//! stock and with zvcs, on the files and the reftable backend.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::Path;
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Run `script` under `sh` in a fresh `root`, with `$S` the stock git that builds the
/// fixture and `$G` the git under test, and report its output with `root` spelled `<D>`.
fn run(stock: &str, git: &str, root: &Path, format: &str, script: &str) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let out = Command::new("sh")
        .args(["-c", script])
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("COLUMNS", "80")
        .env("LC_ALL", "C")
        .env("S", stock)
        .env("G", git)
        .env("F", format)
        .output()
        .unwrap();
    let text = format!(
        "{}--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.replace(&root.display().to_string(), "<D>")
}

fn compare(name: &str, script: &str) {
    let Some(stock) = stock_git() else { return };
    for format in ["files", "reftable"] {
        let root = std::env::temp_dir().join(format!("zvcs-branch-name-conflict-{name}-{format}-{}", std::process::id()));
        let want = run(stock, stock, &root, format, script);
        let got = run(stock, ZVCS, &root, format, script);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(got, want, "{name} ({format})");
    }
}

/// `r` holds `main`, `packed` and `packed2` in `packed-refs`, `loose` and
/// `deep/x` loose.
const FIXTURE: &str = r#"
$S init -q -b main --ref-format=$F r; cd r
$S commit -q --allow-empty -m one
$S branch packed; $S branch packed2; $S pack-refs --all
$S branch loose; $S branch deep/x
show() { echo "exit $?"; $S for-each-ref; $S reflog list; ls .git/logs/refs/heads 2>/dev/null; }
"#;

#[test]
fn a_branch_over_another_one_is_refused_in_gits_words() {
    for name in ["packed/y", "loose/y", "deep", "main/a/b"] {
        compare(&format!("branch-{}", name.replace('/', "-")), &format!("{FIXTURE}$G branch {name}; show"));
    }
    compare("branch-force", &format!("{FIXTURE}$G branch -f packed/y; show"));
    compare("branch-move", &format!("{FIXTURE}$G branch -m packed2 packed/z; show"));
}

#[test]
fn update_ref_takes_the_same_check() {
    for name in ["packed/y", "loose/y", "deep"] {
        compare(
            &format!("update-ref-{}", name.replace('/', "-")),
            &format!("{FIXTURE}$G update-ref refs/heads/{name} HEAD; show"),
        );
    }
}
