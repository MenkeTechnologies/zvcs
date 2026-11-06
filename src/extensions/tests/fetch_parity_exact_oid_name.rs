//! `git fetch <remote> <object id>`: the summary row and the FETCH_HEAD note quote
//! the id as it was typed.
//!
//! get_fetch_map() makes an exact-id refspec into `alloc_ref(name)` with `name`
//! the refspec source (remote.c:2141-2152), and store_updated_refs() quotes that
//! `rm->name` in `'<name>' of <url>` and in the `* branch <name> -> FETCH_HEAD`
//! row (builtin/fetch.c:1324-1365). zvcs abbreviated the id in the row and left
//! the note out, which `submodule update --depth 1` shows whenever it fetches the
//! recorded commit by id.
//!
//! Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

fn ok(bin: &str, dir: &Path, args: &[&str]) -> String {
    let out = run(bin, dir, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn an_exact_id_is_quoted_as_typed() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-fetch-exact-oid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    ok(stock, &root, &["init", "-q", "-b", "main", "up"]);
    let up = root.join("up");
    ok(stock, &up, &["commit", "-q", "--allow-empty", "-m", "one"]);
    ok(stock, &root, &["clone", "-q", "up", "base"]);
    ok(stock, &up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    let id = ok(stock, &up, &["rev-parse", "HEAD"]);
    let upper = id.to_uppercase();

    for (tag, extra, spec) in [
        ("lower", None, id.as_str()),
        ("upper", None, upper.as_str()),
        ("depth", Some("--depth=1"), id.as_str()),
    ] {
        let mut seen = Vec::new();
        for bin in [stock, BIN] {
            let dir = root.join(format!("{tag}-clone"));
            let _ = std::fs::remove_dir_all(&dir);
            ok(stock, &root, &["clone", "-q", "base", dir.to_str().unwrap()]);
            ok(stock, &dir, &["remote", "set-url", "origin", up.to_str().unwrap()]);
            let mut args = vec!["fetch"];
            args.extend(extra);
            args.extend(["origin", spec]);
            let out = run(bin, &dir, &args);
            let head = std::fs::read_to_string(dir.join(".git/FETCH_HEAD")).unwrap_or_default();
            seen.push(format!(
                "{}\n{}\n{:?}\n{head}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
                out.status.code()
            ));
        }
        assert_eq!(seen[1], seen[0], "{tag}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
