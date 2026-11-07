//! The ref-status block `push` and `send-pack` print: each `<old>..<new>` is
//! `strbuf_add_unique_abbrev(…, DEFAULT_ABBREV)` (transport.c:740-751), and every
//! summary is padded to `transport_summary_width()`, twice the widest such
//! abbreviation over `remote_refs` plus 3 (transport.c:871-892). So `core.abbrev`
//! widens the hashes and the column together, `[new branch]`, `[rejected]` and
//! `[up to date]` included, where both were once fixed at 7 and 17.
//!
//! Both clients push to a bare repository through stock `receive-pack`, so only
//! the client differs. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn src(&self) -> PathBuf {
        self.root.join("src")
    }
}

fn run(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> Output {
    Command::new(bin)
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

/// `src` holds three commits on `main`; `dst.git` has `main` at the first and
/// `old` at the third.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-pushwidth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let ok = |cwd: &Path, args: &[&str]| {
        let out = run(stock, cwd, &root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    ok(&root, &["init", "-q", "-b", "main", "src"]);
    let src = root.join("src");
    for i in 1..=3 {
        std::fs::write(src.join(format!("f{i}")), format!("{i}\n")).unwrap();
        ok(&src, &["add", "."]);
        ok(&src, &["commit", "-qm", &format!("c{i}")]);
    }
    ok(&root, &["init", "-q", "--bare", "-b", "main", "dst.git"]);
    ok(&src, &["push", "-q", "../dst.git", "HEAD~2:refs/heads/main", "HEAD:refs/heads/old"]);
    Fixture { root }
}

/// Run `args` in a fresh fixture with each binary and compare stdout, stderr and
/// the exit code. `--receive-pack` pins the far end to stock.
fn check(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let receive_pack = format!("--receive-pack={stock} receive-pack");
    let mut results = Vec::new();
    for bin in [stock, ZVCS] {
        let fx = fixture(stock, tag);
        let mut full: Vec<&str> = Vec::new();
        let (verb, rest) = args.split_at(args.iter().position(|a| *a == "push" || *a == "send-pack").unwrap() + 1);
        full.extend_from_slice(verb);
        full.push(&receive_pack);
        full.extend_from_slice(rest);
        let out = run(bin, &fx.src(), &fx.root, &full);
        let refs = run(stock, &fx.root.join("dst.git"), &fx.root, &["for-each-ref"]);
        results.push((
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).replace(fx.root.to_str().unwrap(), "ROOT"),
            out.status.code(),
            String::from_utf8_lossy(&refs.stdout).into_owned(),
        ));
    }
    assert_eq!(results[0], results[1], "{args:?}: stock (left) vs zvcs (right)");
}

#[test]
fn push_parity_summary_width_follows_core_abbrev() {
    check("abbrev10", &["-c", "core.abbrev=10", "push", "../dst.git", "main"]);
    check(
        "abbrev12",
        &["-c", "core.abbrev=12", "push", "../dst.git", "main", "main:refs/heads/new", "main:refs/tags/t"],
    );
}

#[test]
fn push_parity_summary_width_pads_every_label() {
    // A forced update, a deletion and a non-fast-forward refusal, all at one width.
    check(
        "labels",
        &["-c", "core.abbrev=9", "push", "-v", "../dst.git", "+main~1:refs/heads/main", ":refs/heads/new0", "main~2:refs/heads/old"],
    );
    check("uptodate", &["-c", "core.abbrev=11", "push", "-v", "../dst.git", "main~2:refs/heads/main"]);
}

#[test]
fn push_parity_porcelain_summary_uses_core_abbrev() {
    check("porcelain", &["-c", "core.abbrev=10", "push", "--porcelain", "../dst.git", "main", "main~1:refs/heads/x"]);
}

#[test]
fn send_pack_parity_summary_width_follows_core_abbrev() {
    check("sendpack", &["-c", "core.abbrev=11", "send-pack", "../dst.git", "main", "main:refs/heads/new"]);
    check("sendpackforce", &["-c", "core.abbrev=8", "send-pack", "--force", "../dst.git", "main~1:refs/heads/old"]);
}
