//! `git credential-netrc` reads the script's `@ARGV`, which is everything after
//! the verb (contrib/credential/netrc/git-credential-netrc.perl:52-162). The port
//! skipped the first argument as though it were the verb, so a lone `-h` fell
//! through to the `Syntax:` death (255) instead of the help text (exit 0,
//! git-credential-netrc.perl:61-156), and a lone `get` did the same.
//!
//! Stock git 2.55.0's Homebrew build cannot serve as the oracle here: the script
//! `use`s `Git.pm`, which that install lacks, so every invocation dies in `BEGIN`
//! with status 2 before parsing argv. The expectations are the script's own.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("znetrc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn run(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(BIN)
        .arg("credential-netrc")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_lone_dash_h_prints_help_and_exits_zero() {
    let dir = fixture("h");
    let out = run(&dir, &["-h"], "");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[(-f <authfile>)...] [-g <program>] [-d] [-v] [-k] get"), "{stdout}");
    assert!(out.stderr.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_first_argument_is_the_mode() {
    // `$HOME/.netrc` is one of the default authfiles; `get` as the only argument
    // must be taken as the mode and answer from it.
    let dir = fixture("get");
    let netrc = dir.join(".netrc");
    std::fs::write(&netrc, "machine example.com login u password p\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&netrc, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let out = run(&dir, &["get"], "protocol=https\nhost=example.com\n\n");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "password=p\nusername=u\n");
    let _ = std::fs::remove_dir_all(&dir);
}
