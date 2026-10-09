//! Two inputs `fetch` takes verbatim.
//!
//! * `--stdin` refspecs are read with `strbuf_getline_lf()`: a line ends at LF alone, so the CR
//!   of a CRLF line stays in the refspec, which is then `invalid refspec 'README.md?'` (the
//!   control character is shown as `?` by `vreportf()`), before any remote is looked up.
//! * `--shallow-exclude=<value>` is a string list handed to the server as `deepen-not <value>`.
//!   Whether it is a ref is the server's verdict, so an empty or ill-formed name is
//!   `git upload-pack: deepen-not is not a ref` from a reachable remote, and no complaint at all
//!   from an unreachable one.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::io::Write;
use std::process::{Command, Stdio};

fn run_with_stdin(side: &twin_repo::Side, args: &[&str], input: &[u8]) -> twin_repo::Out {
    let mut child = Command::new(&side.bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    let root = side.root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    twin_repo::Out { code: out.status.code().unwrap_or(-1), stdout: text(&out.stdout), stderr: text(&out.stderr) }
}

#[test]
fn a_cr_ending_a_stdin_refspec_line_stays_in_the_refspec() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("fetch-stdin-crlf", stock);
    for input in [&b"README.md\r\nsrc/lib.rs\r\n"[..], b"main\r\n", b"refs/heads/main\nREADME.md\r\n"] {
        let args = ["fetch", "--stdin", "origin"];
        let want = run_with_stdin(&s, &args, input);
        assert_eq!(want.code, 128, "{input:?}: {want:?}");
        assert!(want.stderr.starts_with("fatal: invalid refspec '"), "{input:?}: {want:?}");
        assert!(want.stderr.contains("?'"), "{input:?}: {want:?}");
        assert_eq!(run_with_stdin(&z, &args, input), want, "{input:?}");
    }
    // The CR-free spelling gets as far as the missing remote.
    let want = run_with_stdin(&s, &["fetch", "--stdin", "origin"], b"main\n");
    assert!(want.stderr.contains("'origin' does not appear to be a git repository"), "{want:?}");
    assert_eq!(run_with_stdin(&z, &["fetch", "--stdin", "origin"], b"main\n"), want);
}

#[test]
fn shallow_exclude_values_are_the_servers_to_judge() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("fetch-shallow-exclude-values", stock);
    for side in [&s, &z] {
        let cloned = side.run_in(&side.root, &[], &["clone", "-q", "--bare", "repo", "remote.git"]);
        assert_eq!(cloned.code, 0, "{cloned:?}");
        let added = side.git(&["remote", "add", "origin", "../remote.git"]);
        assert_eq!(added.code, 0, "{added:?}");
    }
    for value in ["", " 1", "a..b", "nosuch", "bad~name", "@{", "main"] {
        let flag = format!("--shallow-exclude={value}");
        let args = ["fetch", flag.as_str(), "origin"];
        let want = s.git(&args);
        assert_eq!(z.git(&args), want, "{value:?}");
    }
    // An unreachable remote is never asked.
    for side in [&s, &z] {
        side.git(&["remote", "add", "alien", "../nowhere.git"]);
    }
    for value in ["", " 1", "a..b"] {
        let flag = format!("--shallow-exclude={value}");
        let args = ["fetch", flag.as_str(), "alien"];
        let want = s.git(&args);
        assert!(want.stderr.contains("does not appear to be a git repository"), "{value:?}: {want:?}");
        assert_eq!(z.git(&args), want, "{value:?}");
    }
}
