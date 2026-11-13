//! `shortlog`'s `parse_uint()` (`builtin/shortlog.c:279-293`) reads each `-w`
//! field with `strtoul()`, so leading white space and a sign are accepted
//! (`-w+20`, `-w-0`) and a value past `INT_MAX` is the usage error; and
//! `parse_group_option()` (`builtin/shortlog.c:335-344`) compares `author` and
//! `committer` with `strcasecmp()` while `trailer:`/`format:` stay
//! case-sensitive. zvcs wrapped at `-w2147483648`, refused `-w+20`, and
//! rejected `--group=Author`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Author Person")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Committer Person")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-slwrap35-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| assert_eq!(run(stock, &dir, args).2, 0, "{args:?}");
    git(&["init", "-q", "-b", "main", "."]);
    git(&[
        "-c",
        "maintenance.auto=false",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "a subject long enough to be wrapped somewhere along the way",
    ]);
    dir
}

#[test]
fn wrap_fields_use_strtoul_and_group_names_ignore_case() {
    let Some(stock) = stock_git() else { return };
    let dir = fixture(stock);
    for opt in [
        "-w2147483648",
        "-w2147483647",
        "-w99999999999999999999",
        "-w+20",
        "-w-0",
        "-w20,+3",
        "-w20,-0,-0",
        "-w-1",
        "-w+",
        "-w 20",
        "--group=Author",
        "--group=COMMITTER",
        "--group=Trailer:signed-off-by",
        "--group=Format:%an",
    ] {
        let args = ["shortlog", opt, "HEAD"];
        assert_eq!(run(BIN, &dir, &args), run(stock, &dir, &args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
