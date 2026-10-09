//! A bad `--attr-source` dies on the first attribute lookup (`compute_default_attr_source()`,
//! attr.c:1201-1228). `diff-files` with a raw, name-only, `--quiet` or `--summary` output never
//! looks one up, so stock lists the changed paths and exits normally; a patch, stat or check
//! output loads the content and dies. zvcs resolved the diff drivers for every format and died
//! for all of them.

use std::path::Path;
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn fixture(stock: &str, side: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-difffiles-attrsrc-{side}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["add", "f"], &["commit", "-q", "-m", "one"]] {
        if args[0] == "add" {
            std::fs::write(root.join("f"), "a\n").unwrap();
        }
        assert_eq!(run(stock, &root, args).0, 0, "{args:?}");
    }
    std::fs::write(root.join("f"), "a\nb\n").unwrap();
    root
}

#[test]
fn only_the_formats_that_read_content_die() {
    let Some(stock) = stock_git() else { return };
    let stock_dir = fixture(stock, "stock");
    let zvcs_dir = fixture(stock, "zvcs");
    for format in [
        &[][..],
        &["--raw"],
        &["--name-only"],
        &["--name-status"],
        &["--quiet"],
        &["--summary"],
        &["--raw", "--summary"],
        &["-z"],
        &["-p"],
        &["--stat"],
        &["--numstat"],
        &["--shortstat"],
        &["--check"],
        &["--summary", "--stat"],
    ] {
        let mut args = vec!["--attr-source=does-not-exist", "diff-files"];
        args.extend_from_slice(format);
        let want = run(stock, &stock_dir, &args);
        let got = run(ZVCS, &zvcs_dir, &args);
        assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
    }
    let _ = std::fs::remove_dir_all(&stock_dir);
    let _ = std::fs::remove_dir_all(&zvcs_dir);
}
