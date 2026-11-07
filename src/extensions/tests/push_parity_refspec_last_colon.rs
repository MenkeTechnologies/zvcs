//! Two client-side refspec questions `git push` answers before it connects:
//!
//! * `parse_refspec()` splits a refspec at its *last* colon (`rhs =
//!   strrchr(lhs, ':')`, refspec.c:37), so `main:f:refs/heads/blob` pushes the
//!   blob `main:f` to `refs/heads/blob`, where zvcs split at the first colon and
//!   sent `main` to `f:refs/heads/blob`;
//! * `ref_newer()` peels both sides with `deref_tag()` (commit-reach.c:786-808),
//!   so an annotated tag pushed over the branch tip it points at is a
//!   fast-forward, where zvcs rejected it as non-fast-forward.
//!
//! The pushing binary is swapped; the receiving end is stock `receive-pack` in
//! both runs, over identical fixtures at the same path. Stock git is the oracle
//! (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(bin: &str, cwd: &Path, root: &Path, args: &[&str]) {
    let out = git(bin, cwd, root, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `src` with `main` at one commit and annotated tag `lt` on it, and a bare
/// `dst` that already has `main`; `client` pushes `refspecs` into it.
fn push(stock: &str, root: &Path, client: &str, refspecs: &[&str]) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let src = root.join("src");
    ok(stock, root, root, &["init", "-q", "-b", "main", "src"]);
    std::fs::write(src.join("f"), "a\n").unwrap();
    ok(stock, &src, root, &["add", "f"]);
    ok(stock, &src, root, &["commit", "-q", "-m", "one"]);
    ok(stock, &src, root, &["tag", "-a", "-m", "t", "lt"]);
    ok(stock, root, root, &["clone", "-q", "--bare", "--no-local", "src", "dst"]);
    let receive_pack = format!("--receive-pack={stock} receive-pack");
    let mut args = vec!["push", receive_pack.as_str(), "../dst"];
    args.extend(refspecs);
    let out = git(client, &src, root, &args);
    let refs = git(stock, &root.join("dst"), root, &["for-each-ref"]);
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}--- refs\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&refs.stdout),
    )
}

fn compare(name: &str, refspecs: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-push-last-colon-{name}-{}", std::process::id()));
    let fx = Fixture { root: base };
    let want = push(stock, &fx.root, stock, refspecs);
    let got = push(stock, &fx.root, ZVCS, refspecs);
    assert_eq!(got, want, "{name}");
}

#[test]
fn the_last_colon_splits_a_refspec() {
    compare("blob", &["main:f:refs/heads/blob"]);
}

#[test]
fn a_tag_over_the_commit_it_names_is_a_fast_forward() {
    compare("tag-ff", &["lt:refs/heads/main"]);
}
