//! An abbreviation two objects share, written where a revision is read: `get_short_oid()`'s
//! `error()` and candidate block (object-name.c:507-543) under the disambiguator each
//! `get_oid_1()` frame carries.
//!
//! Below a `~<n>`/`^<n>` the frame asks for a committish and has lost `GET_OID_QUIETLY`
//! (object-name.c:832-833, :866), so `die_verify_filename()`'s
//! `maybe_die_on_misspelt_object_name()` pass prints the block a second time
//! (object-name.c:1880-1889); `^{tree}` and the `<rev>` of `<rev>:<path>` ask for a treeish
//! (object-name.c:952-956, :1835-1840). `handle_dotdot_1()` resolves both ends of a range as
//! committishes and stops at the first that fails (revision.c:2074-2084); `log` and
//! `whatchanged` set `REVARG_COMMITTISH` for the name itself, `rev-list`, `show` and `diff` do
//! not (builtin/log.c:546, :844). `reset` asks for a committish, or a treeish with paths
//! (builtin/reset.c:278-279), and `checkout`'s `setup_branch_path()` resolves the operand again
//! as a committish (builtin/checkout.c:804-806).
//!
//! Stock git builds every fixture and is the oracle (`support/stock_git.rs`); the tests skip
//! when it is missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-objname-ambiguous-revs-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn cmd(bin: &str, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        // Outside the work tree, so nothing a binary keeps under `$HOME` shows in `status`.
        .env("HOME", dir.parent().unwrap_or(dir))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    c
}

fn setup(stock: &str, dir: &Path, args: &[&str]) -> String {
    let out = cmd(stock, dir, args).output().unwrap();
    assert!(out.status.success(), "setup {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(bin, dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run `args` through both binaries in `repo` and demand the same exit code, stdout and stderr.
fn same(stock: &str, repo: &Path, args: &[&str]) -> (i32, String, String) {
    let theirs = run(stock, repo, args);
    let ours = run(ZVCS, repo, args);
    assert_eq!(ours, theirs, "{args:?}");
    theirs
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Run `args` in a fresh copy of `repo` per binary, and demand the same output and the same
/// refs, index and worktree afterwards.
fn same_mutating(stock: &str, root: &Path, repo: &Path, args: &[&str]) {
    let outcome = |bin: &str, label: &str| {
        let copy = root.join(label);
        let _ = std::fs::remove_dir_all(&copy);
        copy_dir(repo, &copy);
        let result = run(bin, &copy, args);
        let refs = setup(stock, &copy, &["for-each-ref", "refs/heads"]);
        let status = setup(stock, &copy, &["status", "--porcelain"]);
        let head = run(stock, &copy, &["rev-parse", "HEAD"]).1;
        (result, refs, status, head)
    };
    assert_eq!(outcome(ZVCS, "ours"), outcome(stock, "theirs"), "{args:?}");
}

/// A repository crowded enough that several four-hex prefixes are shared, with each shared
/// prefix mapped to the sorted kinds that share it — at most one prefix per combination.
fn crowded(stock: &str, root: &Path) -> (PathBuf, BTreeMap<Vec<String>, String>) {
    setup(stock, root, &["init", "-q", "-b", "main", "crowded"]);
    let repo = root.join("crowded");
    let mut stream = String::new();
    for i in 1..=400 {
        let data = format!("blob {i}\n");
        stream.push_str(&format!("blob\ndata {}\n{data}\n", data.len()));
    }
    for i in 1..=300 {
        let msg = format!("m{i}\n");
        let file = format!("c{i}\n");
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter C O Mitter <committer@example.com> 1112911993 -0700\ndata {}\n{msg}M 100644 inline f\ndata {}\n{file}\n",
            msg.len(),
            file.len()
        ));
    }
    for i in 1..=150 {
        let msg = format!("msg {i}\n");
        stream.push_str(&format!(
            "tag t{i}\nfrom refs/heads/main\ntagger T <t@example.com> 1112911993 -0700\ndata {}\n{msg}\n",
            msg.len()
        ));
    }
    let mut child = cmd(stock, &repo, &["fast-import", "--quiet"]).stdin(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    setup(stock, &repo, &["reset", "-q", "--hard", "main"]);
    let listing = setup(stock, &repo, &["cat-file", "--batch-all-objects", "--batch-check=%(objectname) %(objecttype)"]);
    let mut by_prefix: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in listing.lines() {
        let (id, kind) = line.split_once(' ').unwrap();
        by_prefix.entry(id[..4].to_owned()).or_default().push(kind.to_owned());
    }
    let mut by_kinds = BTreeMap::new();
    for (prefix, mut kinds) in by_prefix.into_iter().filter(|(_, kinds)| kinds.len() > 1) {
        kinds.sort();
        by_kinds.entry(kinds).or_insert(prefix);
    }
    (repo, by_kinds)
}

#[test]
fn walk_operands_report_the_ambiguity_once_per_resolution() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("walk");
    let (repo, prefixes) = crowded(stock, &root);
    assert!(prefixes.keys().any(|kinds| kinds.iter().all(|k| k == "blob")), "{prefixes:?}");
    assert!(prefixes.keys().any(|kinds| kinds.contains(&"commit".to_owned())), "{prefixes:?}");
    let mut doubled = false;
    for p in prefixes.values() {
        let tilde = format!("{p}~1");
        let caret = format!("{p}^");
        let left = format!("{p}..main");
        let right = format!("main..{p}");
        let symmetric = format!("{p}...main");
        let excluded = format!("^{p}");
        let peel = format!("{p}^{{tree}}");
        let path = format!("{p}:f");
        for args in [
            &["log", "--oneline", &tilde][..],
            &["log", "--oneline", &caret],
            &["log", "--oneline", &left],
            &["log", "--oneline", &right],
            &["log", "--oneline", &symmetric],
            &["log", "--oneline", &excluded, "main"],
            &["log", "--oneline", &peel],
            &["log", "--oneline", &path],
            &["log", "--oneline", &tilde, "--"],
            &["log", "--oneline", "--end-of-options", &tilde],
            &["whatchanged", "--i-still-use-this", "--oneline", p],
            &["whatchanged", "--i-still-use-this", "--oneline", &tilde],
            &["rev-list", p],
            &["rev-list", "--objects", p],
            &["rev-list", &tilde],
            &["rev-list", &left],
            &["show", "--oneline", &tilde],
            &["show", &path],
            &["diff", &tilde],
            &["diff", p, "main"],
            &["diff", &left],
            &["rev-parse", &tilde],
            &["rev-parse", &left],
            &["rev-parse", &path],
            &["cat-file", "-p", &path],
        ] {
            let (_, _, err) = same(stock, &repo, args);
            doubled |= err.matches(&format!("error: short object ID {p} is ambiguous")).count() == 2;
        }
        for setting in ["commit", "tree", "blob"] {
            let config = format!("core.disambiguate={setting}");
            same(stock, &repo, &["-c", &config, "log", "--oneline", &tilde]);
            same(stock, &repo, &["-c", &config, "rev-list", &left]);
            same(stock, &repo, &["-c", &config, "show", &path]);
            same(stock, &repo, &["-c", &config, "diff", &peel, "main"]);
        }
    }
    assert!(doubled, "no operand printed the block twice: {prefixes:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ref_updating_operands_resolve_as_their_callers_ask() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = scratch("refs");
    let (repo, prefixes) = crowded(stock, &root);
    for p in prefixes.values() {
        let tilde = format!("{p}~1");
        let symmetric = format!("{p}...main");
        for args in [
            &["reset", p][..],
            &["reset", &tilde],
            &["reset", p, "--", "f"],
            &["checkout", p],
            &["checkout", &symmetric],
            &["branch", "new", p],
            &["branch", "new", &symmetric],
        ] {
            same_mutating(stock, &root, &repo, args);
        }
        same_mutating(stock, &root, &repo, &["-c", "core.disambiguate=tree", "checkout", p]);
    }
    let _ = std::fs::remove_dir_all(&root);
}
