//! Interop guarantee: repositories written by zvcs must be valid to **stock**
//! git and vice versa. Verifies round-trip read, `git fsck --full` on a
//! zvcs-written repo, and that a zvcs submodule pointer bump is exactly what
//! stock git records.
//!
//! The "reference" git must be a real stock git, not zvcs. We detect it by
//! probing `<git> zdaemon status`: stock git rejects the unknown subcommand
//! (non-zero), zvcs accepts it. If no stock git is found, the test skips.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Locate a stock git distinct from zvcs, or `None` to skip.
///
/// Probe with `zjobs` (a zvcs-only verb): stock git rejects it (non-zero) and,
/// unlike `zdaemon`, git's autocorrect won't map it to a real blocking command
/// (`zdaemon` → `daemon`, which hangs). zvcs runs it (zero).
fn stock_git() -> Option<String> {
    for cand in ["/usr/bin/git", "git"] {
        match Command::new(cand).args(["zjobs"]).output() {
            Ok(out) if !out.status.success() => return Some(cand.to_string()),
            _ => continue,
        }
    }
    None
}

/// Run a command with args verbatim, with a deterministic identity via env vars
/// (honored by both stock git and gix). We avoid git's global `-c` because zvcs
/// porcelain doesn't parse it.
fn run(bin: &str, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"))
}

fn ok(bin: &str, dir: &Path, args: &[&str]) -> String {
    let out = run(bin, dir, args);
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-interop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

#[test]
fn zvcs_writes_are_valid_to_stock_git_and_back() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found (only zvcs on PATH) — skipping interop test");
        return;
    };

    let repo = tmp("rw");
    // Stock git creates a commit; zvcs must read the same HEAD.
    ok(&git, &repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), b"a\n").unwrap();
    ok(&git, &repo, &["add", "a.txt"]);
    ok(&git, &repo, &["commit", "-q", "-m", "stock c0"]);
    let stock_head = ok(&git, &repo, &["rev-parse", "HEAD"]);
    let zvcs_head = ok(BIN, &repo, &["rev-parse", "HEAD"]);
    assert_eq!(stock_head, zvcs_head, "zvcs must read stock git's commit");

    // zvcs writes a commit; stock git must read it and fsck must pass.
    std::fs::write(repo.join("b.txt"), b"b\n").unwrap();
    ok(BIN, &repo, &["add", "b.txt"]);
    ok(BIN, &repo, &["commit", "-q", "-m", "zvcs c1"]);
    let after = ok(&git, &repo, &["log", "--oneline"]);
    assert!(after.contains("zvcs c1"), "stock git must read zvcs's commit:\n{after}");
    assert_eq!(
        ok(&git, &repo, &["cat-file", "-p", "HEAD:b.txt"]),
        "b\n",
        "stock git must read the blob zvcs wrote"
    );
    let fsck = run(&git, &repo, &["fsck", "--full"]);
    assert!(
        fsck.status.success(),
        "git fsck must pass on a zvcs-written repo:\n{}",
        String::from_utf8_lossy(&fsck.stderr)
    );

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn zvcs_zbump_pointer_matches_stock_git() {
    let Some(git) = stock_git() else {
        eprintln!("no stock git found — skipping interop test");
        return;
    };

    let root = tmp("sub");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    ok(&git, &sub, &["init", "-q", "-b", "main"]);
    ok(&git, &sub, &["commit", "--allow-empty", "-q", "-m", "s0"]);

    let sup = root.join("super");
    std::fs::create_dir_all(&sup).unwrap();
    ok(&git, &sup, &["init", "-q", "-b", "main"]);
    ok(&git, &sup, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", sub.to_str().unwrap(), "sub"]);
    ok(&git, &sup, &["commit", "-q", "-m", "add sub"]);

    // Advance the submodule, then zvcs bumps the pointer.
    ok(&git, &sup.join("sub"), &["commit", "--allow-empty", "-q", "-m", "s1"]);
    let sub_head = ok(&git, &sup.join("sub"), &["rev-parse", "HEAD"]);
    ok(BIN, &sup, &["zbump"]);

    // Stock git must record exactly the submodule's HEAD as the gitlink.
    let ls = ok(&git, &sup, &["ls-tree", "HEAD", "sub"]);
    let gitlink = ls.split_whitespace().nth(2).unwrap_or("");
    assert_eq!(gitlink, sub_head.trim(), "stock git gitlink must equal the bumped submodule HEAD");

    let fsck = run(&git, &sup, &["fsck", "--full"]);
    assert!(fsck.status.success(), "fsck must pass after a zvcs bump");
    // Pointer is committed → stock git sees a clean tree.
    assert!(
        ok(&git, &sup, &["status", "--porcelain"]).trim().is_empty(),
        "stock git must see a clean tree after the committed bump"
    );

    let _ = std::fs::remove_dir_all(&root);
}
