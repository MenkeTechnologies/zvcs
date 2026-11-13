//! `git grep` against stock git on one fixture: the matching and naming rules a
//! differential sweep found apart from stock 2.56.
//!
//! * Each pattern is its own `regcomp()`/PCRE2 program and is asked in turn
//!   (grep.c:492-573, :1151-1204): the earliest hit wins and a tie goes to the
//!   longer one, and `-w` retries each pattern past a failed word on its own
//!   (grep.c:946-1008) — so `-w -e foo -e foobar` finds `foobar`, an empty
//!   pattern never matches under `-w`, and `-o -e foo -e foobar` prints `foobar`.
//! * `regexec()` is leftmost-longest: `-o -E 'fooba|foobar'` prints `foobar`.
//! * The `regcomp()` verdict is the C library's own (with `REG_ENHANCED` for a
//!   basic expression on macOS, compat/regcomp_enhanced.c:11-12), a pattern
//!   `is_fixed()` never reaches it (grep.c:510-556), and its origin is `-e option`
//!   for `-E` too; an ERE `)` that closes no group is a literal.
//! * `-P`: `\<`/`\>` are the literal `<`/`>` and `\Q…\E` quotes.
//! * A tree object's files carry no path, so no `.gitattributes` applies
//!   (builtin/grep.c:845-846); a blob named `<rev>:<path>` is printed relative to
//!   the current directory (builtin/grep.c:328-358).
//! * Under `-z` the separator NUL is never coloured (grep.c:894-900).
//! * `-f` drops a CR before each LF (strbuf.c:737-741).
//!
//! Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    c.args(args);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PAGER", "cat")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("spawn git")
}

/// `repo` with one commit of a few text files, a `-diff` file, and a pattern
/// file written with CRLF line ends.
fn fixture(stock: &str, root: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let g = |dir: &Path, args: &[&str]| {
        let mut full = vec!["-c", "maintenance.auto=false"];
        full.extend_from_slice(args);
        let out = run(stock, dir, &home, &full);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    g(&root, &["init", "-q", "-b", "main", "repo"]);
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("doc")).unwrap();
    std::fs::write(repo.join("doc/readme.txt"), "foo\nfoobar\nbar foo baz\nx.y a)\n<foo> end\n").unwrap();
    std::fs::write(repo.join("a.c"), "int foo;\nint bar;\n").unwrap();
    std::fs::write(repo.join("nodiff.txt"), "foo in nodiff\n").unwrap();
    std::fs::write(repo.join(".gitattributes"), "nodiff.txt -diff\n").unwrap();
    g(&repo, &["add", "."]);
    g(&repo, &["commit", "-q", "-m", "one"]);
    std::fs::write(root.join("pats"), "foo\r\nbaz\r\n").unwrap();
    repo
}

#[test]
fn grep_matching_and_naming_match_stock() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-grep-sweep-matching-{}", std::process::id()));
    let repo = fixture(stock, &root);
    let home = root.join("home");
    let pats = root.join("pats");
    let pats = pats.to_str().unwrap();
    // `-P` needs a stock git built with PCRE2. The probe pattern must not be
    // `is_fixed()`: a fixed one goes to compile_fixed_regexp() before the PCRE
    // branch is reached (grep.c:521-556), so it succeeds in any build.
    let pcre = run(stock, &repo, &home, &["grep", "-q", "-P", "f.o"]).status.success();

    let mut cases: Vec<(&str, Vec<&str>)> = vec![
        (".", vec!["grep", "-w", "-e", "foo", "-e", "foobar"]),
        (".", vec!["grep", "-o", "-e", "foo", "-e", "foobar"]),
        (".", vec!["grep", "--color=always", "-e", "o", "-e", "foo", "--", "doc"]),
        (".", vec!["grep", "-w", "-e", "x*"]),
        (".", vec!["grep", "-o", "-E", "-e", "fooba|foobar"]),
        (".", vec!["grep", "-w", "-E", "-e", "fooba|foobar"]),
        (".", vec!["grep", "-E", "-e", "a)"]),
        (".", vec!["grep", "-e", "[[:foo:]]"]),
        (".", vec!["grep", "-E", "-e", "*foo"]),
        (".", vec!["grep", "-E", "-e", "(?<"]),
        (".", vec!["grep", "-n", "foo", "HEAD^{tree}", "--", "nodiff.txt"]),
        (".", vec!["grep", "-n", "foo", "HEAD", "--", "nodiff.txt"]),
        ("doc", vec!["grep", "-n", "int", "HEAD:../a.c"]),
        (".", vec!["grep", "--color=always", "-z", "-n", "--column", "foo", "--", "doc"]),
        (".", vec!["grep", "--color=always", "-z", "-c", "foo"]),
        (".", vec!["grep", "-c", "-f", pats]),
    ];
    if pcre {
        // An empty pattern is fixed, so a PCRE2 build compiles it with PCRE2 and
        // one without hands it to regcomp(), which macOS refuses (grep.c:521-554).
        cases.push((".", vec!["grep", "-w", "-e", "", "-e", "foo"]));
        cases.push((".", vec!["grep", "-P", "-e", r"\<foo\>"]));
        cases.push((".", vec!["grep", "-P", "-o", "-e", r"\Qx.y\E"]));
    }

    let mut diffs = Vec::new();
    for (dir, args) in &cases {
        let cwd = repo.join(dir);
        let want = run(stock, &cwd, &home, args);
        let got = run(BIN, &cwd, &home, args);
        if (&want.stdout, &want.stderr, want.status.code()) != (&got.stdout, &got.stderr, got.status.code()) {
            diffs.push(format!(
                "{dir}: {args:?}\n  stock rc={:?} out={:?} err={:?}\n  zvcs  rc={:?} out={:?} err={:?}",
                want.status.code(),
                String::from_utf8_lossy(&want.stdout),
                String::from_utf8_lossy(&want.stderr),
                got.status.code(),
                String::from_utf8_lossy(&got.stdout),
                String::from_utf8_lossy(&got.stderr),
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
