//! `remote.<name>.tagOpt` against stock git: only `--tags` and `--no-tags` mean
//! anything. `handle_config()` (remote.c) compares the value to those two and
//! ignores everything else, so `tagOpt = yes` neither fails `fetch` nor stops a
//! partial clone's lazy blob fetch during `checkout`. zvcs rejected the remote
//! as soon as it was built, which made the lazy fetch fail and checkout die with
//! "object for checkout ... could not be found".
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// `peer.git` (filter-capable) holding `main` and `side`, plus a blob-less clone
/// `cl` of it made by stock git.
fn fixture(stock: &str, root: &Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    run(stock, &src, &["init", "-q", "-b", "main"]);
    for n in 0..4 {
        std::fs::write(src.join("hist.txt"), format!("hist v{n}\n")).unwrap();
        run(stock, &src, &["add", "hist.txt"]);
        run(stock, &src, &["commit", "-qm", &format!("v{n}")]);
    }
    run(stock, &src, &["branch", "side", "main~2"]);
    run(stock, root, &["init", "-q", "--bare", "-b", "main", "peer.git"]);
    run(stock, &root.join("peer.git"), &["config", "uploadpack.allowFilter", "true"]);
    run(stock, &src, &["remote", "add", "origin", "../peer.git"]);
    run(stock, &src, &["push", "-q", "origin", "main", "side"]);
    let url = format!("file://{}", root.join("peer.git").display());
    let (_, err, code) = run(
        stock,
        root,
        &["clone", "-q", "--no-single-branch", "--filter=blob:none", &url, "cl"],
    );
    assert_eq!(code, Some(0), "{err}");
}

#[test]
fn unrecognised_tagopt_is_ignored_by_fetch_and_lazy_checkout() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-tagopt-ignored-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let (s, z) = (base.join("stock"), base.join("zvcs"));
    for root in [&s, &z] {
        std::fs::create_dir_all(root).unwrap();
        fixture(stock, root);
    }
    for value in ["yes", "--TAGS", ""] {
        let key = format!("remote.origin.tagOpt={value}");
        let steps: &[&[&str]] = &[
            &["-c", &key, "fetch"],
            &["-c", &key, "checkout", "side"],
            &["cat-file", "-p", "side:hist.txt"],
            &["-c", &key, "checkout", "main"],
        ];
        for args in steps {
            let want = run(stock, &s.join("cl"), args);
            let got = run(BIN, &z.join("cl"), args);
            assert_eq!(got, want, "tagOpt={value:?} {args:?}");
        }
        // reset so the next value starts blob-less on `side` again
        for (bin, root) in [(stock, &s), (BIN, &z)] {
            run(bin, &root.join("cl"), &["checkout", "-q", "main"]);
            run(bin, &root.join("cl"), &["branch", "-D", "side"]);
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
