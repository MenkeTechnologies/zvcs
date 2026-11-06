//! `%(HEAD)` compares each refname with `resolve_refdup("HEAD", ...)`
//! (`head_atom_parser()` / `populate_value()`, ref-filter.c), which for a
//! detached `HEAD` is the name `HEAD` itself, so `--include-root-refs` marks the
//! `HEAD` row with `*`. zvcs only ever compared against a branch name.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
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

#[test]
fn detached_head_marks_the_head_root_ref() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    for format in ["files", "reftable"] {
        let dir = std::env::temp_dir().join(format!("zvcs-fer-detached-head-{format}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main", "--ref-format", format, "."][..],
            &["commit", "-q", "--allow-empty", "-m", "one"],
            &["branch", "side"],
            &["checkout", "-q", "--detach"],
        ] {
            assert_eq!(run(stock, &dir, args).2, 0, "{args:?}");
        }
        for args in [
            &["for-each-ref", "--include-root-refs", "--format=%(refname) %(HEAD)"][..],
            &["for-each-ref", "--include-root-refs", "--format=%(if)%(HEAD)%(then)%(refname)%(end)"],
            &["for-each-ref", "--format=%(refname)%(HEAD)", "refs/heads"],
        ] {
            assert_eq!(run(BIN, &dir, args), run(stock, &dir, args), "{format} {args:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
