//! `log --graph` around the lines `show_log()` prints itself, compared byte for
//! byte with stock git.
//!
//! The `commit` line, each line of the `--show-signature` report and `log size
//! <n>` are each followed by `graph_show_oneline()` (log-tree.c:576, :834, :903),
//! whatever comes next; only `msgbuf` goes through `graph_show_strbuf()`, whose
//! `strchr()` walk draws no row in front of a line that begins with a NUL
//! (graph.c:1948-1973). A record converted into UTF-16BE or UTF-32BE begins
//! with one, and zvcs applied that rule to the head lines too. `missing_newline`
//! and the remainder rows read `msgbuf` alone (log-tree.c:909-915,
//! graph.c:1989-2010), and a terminator format closes the record before the diff
//! (log-tree.c:916-920), so a commit under a collapse prints its padding row
//! ahead of the terminator. `reference` gets the signature report like every
//! other format (log-tree.c:851-854).
//!
//! The signed commit carries an SSH signature: with no
//! `gpg.ssh.allowedSignersFile` the check fails with no output, which is the
//! `No signature` report, and no `ssh-keygen` is run.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn command(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0100")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0100")
        .env("GIT_EDITOR", "true")
        .env("TZ", "UTC");
    cmd
}

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = command(bin, dir, home, args).output().unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

/// Built by stock git: a root commit, an SSH-signed commit on `main`, a side
/// branch off the root, and the merge of the two.
fn fixture(stock: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-log-graph-show-log-lines-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let run = |args: &[&str]| -> Vec<u8> {
        let mut full = vec!["-c", "maintenance.auto=false"];
        full.extend_from_slice(args);
        let (out, err, rc) = git(stock, &root, &root, &full);
        assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
        out
    };
    run(&["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("f"), "a\n").unwrap();
    run(&["add", "f"]);
    run(&["commit", "-q", "-m", "base"]);
    let rev = |name: &str| String::from_utf8(run(&["rev-parse", name])).unwrap().trim().to_string();
    let signed = format!(
        "tree {}\nparent {}\nauthor A U Thor <author@example.com> 1700000000 +0100\n\
         committer C O Mitter <committer@example.com> 1700000000 +0100\n\
         gpgsig -----BEGIN SSH SIGNATURE-----\n U1NIU0lH\n -----END SSH SIGNATURE-----\n\n\
         signed subject\n\nsigned body\n",
        rev("HEAD^{tree}"),
        rev("HEAD"),
    );
    let mut hash = command(stock, &root, &root, &["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hash.stdin.take().unwrap().write_all(signed.as_bytes()).unwrap();
    let out = hash.wait_with_output().unwrap();
    assert!(out.status.success());
    let signed = String::from_utf8(out.stdout).unwrap();
    run(&["update-ref", "refs/heads/main", signed.trim()]);
    run(&["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(root.join("s"), "s\n").unwrap();
    run(&["add", "s"]);
    run(&["commit", "-q", "-m", "side"]);
    run(&["checkout", "-q", "main"]);
    run(&["merge", "-q", "--no-edit", "side"]);
    Fixture(root)
}

#[test]
fn graph_rows_after_show_log_lines_match_stock() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(stock);
    let mut cases: Vec<Vec<&str>> = vec![
        vec!["log", "--graph", "--encoding=UTF-16BE", "--format=%s", "--log-size"],
        vec!["log", "--graph", "--encoding=UTF-32BE", "--oneline", "--log-size", "--show-signature"],
        vec!["log", "--graph", "--encoding=UTF-16LE", "--pretty=oneline", "--log-size", "-p"],
        vec!["log", "--graph", "--encoding=UTF-32BE", "--pretty=format:%B", "--log-size", "--show-signature"],
        vec!["log", "--graph", "--encoding=UTF-16BE", "-z", "--log-size", "--format=%h%n%b"],
        vec!["log", "--graph", "--encoding=UTF-16BE", "-m", "-p", "--log-size", "--oneline"],
        vec!["log", "--graph", "--format=%h%n%b", "--stat"],
        vec!["log", "--graph", "--encoding=ISO-8859-1", "--format=%h%n%b", "-p"],
        vec!["log", "--graph", "--pretty=reference", "--show-signature", "--log-size"],
        vec!["log", "--graph", "--pretty=fuller", "--show-signature", "--log-size", "--stat"],
    ];
    // macOS's libiconv writes a bare `UTF-16`/`UTF-32` big-endian behind its mark.
    if cfg!(target_os = "macos") {
        cases.push(vec!["log", "--graph", "--encoding=UTF-16", "--format=tformat:%an %s", "--log-size"]);
        cases.push(vec!["log", "--graph", "--encoding=UTF-32", "--oneline", "--log-size", "--show-signature"]);
    }
    let mut diverged = Vec::new();
    for args in &cases {
        let want = git(stock, &fx.0, &fx.0, args);
        let got = git(BIN, &fx.0, &fx.0, args);
        if want != got {
            diverged.push(format!(
                "{args:?}\n  stock rc={} out={:?} err={:?}\n  zvcs  rc={} out={:?} err={:?}",
                want.2,
                String::from_utf8_lossy(&want.0),
                String::from_utf8_lossy(&want.1),
                got.2,
                String::from_utf8_lossy(&got.0),
                String::from_utf8_lossy(&got.1),
            ));
        }
    }
    assert!(diverged.is_empty(), "{}", diverged.join("\n"));
}
