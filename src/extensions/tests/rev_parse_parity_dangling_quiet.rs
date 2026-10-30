//! `expand_ref()` (refs.c:808-834) resolves every `ref_rev_parse_rules`
//! spelling with `RESOLVE_REF_READING` and speaks for the rules that fail:
//! `ignoring dangling symref <rule>` for a symref whose chain ends nowhere,
//! `ignoring broken ref <rule>` for a loose file that does not parse. Neither
//! line tests `GET_OID_QUIETLY`; `rev-parse --quiet` silences only the
//! ambiguity warning (object-name.c:752) and the reflog reach messages. zvcs
//! dropped the dwim, and with it the dangling line, under `--quiet`, printed no
//! broken-ref line at all, missed the second pass's line for `<rev>:<path>` and
//! `<rev>^{<type>}`, and let a dangling `refs/tags/x` hide a real
//! `refs/heads/x` that the C loop goes on to find.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// Two commits on `main`, a `side` branch, and the refs the C loop skips:
/// `refs/heads/dang` (dangling), `refs/heads/chain` → `dang2` → nothing,
/// `refs/heads/broken` (garbage), `refs/remotes/o/HEAD` (dangling),
/// `refs/tags/dup` + `refs/heads/dup` (ambiguous) and a dangling
/// `refs/tags/tdang` shadowing a real `refs/heads/tdang`. `main`'s upstream is
/// the dangling `dang` of the local remote.
fn fixture(bin: &str) -> PathBuf {
    let side = if bin == BIN { "zvcs" } else { "stock" };
    let dir = std::env::temp_dir().join(format!("zvcs-rpdangq-{}-{side}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| assert_eq!(run(bin, &dir, args).2, 0, "{args:?}");
    git(&["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("f"), "a\n").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-q", "-m", "a"]);
    std::fs::write(dir.join("f"), "b\n").unwrap();
    git(&["commit", "-q", "-am", "b"]);
    git(&["checkout", "-q", "-b", "side"]);
    git(&["checkout", "-q", "main"]);
    git(&["symbolic-ref", "refs/heads/dang", "refs/heads/nowhere"]);
    git(&["symbolic-ref", "refs/heads/chain", "refs/heads/dang2"]);
    git(&["symbolic-ref", "refs/heads/dang2", "refs/heads/gone"]);
    git(&["symbolic-ref", "refs/remotes/o/HEAD", "refs/remotes/o/nope"]);
    git(&["symbolic-ref", "refs/tags/tdang", "refs/tags/none"]);
    git(&["branch", "tdang"]);
    git(&["tag", "dup"]);
    git(&["branch", "dup"]);
    git(&["config", "branch.main.remote", "."]);
    git(&["config", "branch.main.merge", "refs/heads/dang"]);
    std::fs::write(dir.join(".git/refs/heads/broken"), "garbage\n").unwrap();
    dir
}

#[test]
fn quiet_keeps_expand_refs_warnings_and_the_loop_skips_unreadable_rules() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let operands = [
        "dang",
        "refs/heads/dang",
        "dang~1",
        "dang^{commit}",
        "dang:f",
        "dang..main",
        "^dang",
        "chain",
        "o",
        "broken",
        "tdang",
        "tdang~1",
        "dup",
        "dup~1",
        "@{u}",
        "@{-1}",
        "dang@{u}",
    ];
    for modes in [&[][..], &["-q"], &["--verify"], &["--verify", "-q"]] {
        for operand in operands {
            let mut args = vec!["rev-parse"];
            args.extend_from_slice(modes);
            args.push(operand);
            assert_eq!(run(BIN, &z, &args), run(stock, &s, &args), "{args:?}");
        }
        for extra in [&["--symbolic-full-name", "tdang"][..], &["--abbrev-ref", "tdang"], &["dang", "dang"]] {
            let mut args = vec!["rev-parse"];
            args.extend_from_slice(modes);
            args.extend_from_slice(extra);
            assert_eq!(run(BIN, &z, &args), run(stock, &s, &args), "{args:?}");
        }
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
