//! `git last-modified` against stock git: the order things happen in.
//!
//! * Pathspec items keep their command-line order (`PATHSPEC_KEEP_ORDER`): `check_recursion_depth()`
//!   walks them last to first, so `src/lib.rs src` and `src src/lib.rs` list different things.
//! * `git_default_config` is read after `parse_options()` (a bad `--max-depth` is a 129 usage
//!   error, not the config fatal), and the settings block waits for the first revision to
//!   resolve, so `core.packedGitLimit` loses to an unresolvable argument while
//!   `core.ignorecase` does not.
//! * `check_filename()` strips `:^` / `:!` before looking at the disk, so an exclude that names
//!   an existing path is a pathspec without `--`.
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

/// Two commits with fixed dates: `src/d/f` and `src/z` land in the second.
fn fixture(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str], at: u64| {
        let date = format!("{at} +0000");
        let st = Command::new(BIN)
            .args(args)
            .current_dir(root)
            .env("HOME", root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"], 1_700_000_000);
    for (n, files) in [vec!["README.md", "src/lib.rs"], vec!["src/d/f", "src/z"]].into_iter().enumerate() {
        for f in files {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, format!("{f} {n}\n")).unwrap();
        }
        let at = 1_700_000_000 + n as u64 * 100;
        git(&["add", "-A"], at);
        git(&["commit", "-q", "-m", &format!("c{n}")], at);
    }
}

const CASES: &[&[&str]] = &[
    &["src/lib.rs", "src"],
    &["src", "src/lib.rs"],
    &["src/d/f", "src"],
    &["src/d/f", "src/d"],
    &["src/d", "src/d/f"],
    &["src/d/f", "src/z"],
    &["-r", "src/d/f", "src"],
    &[":^src"],
    &[":!src", "README.md"],
    &["-r", ":^src/d"],
    &["--max-depth=true"],
    &["--max-depth=true", "nosuch"],
    &["-c", "core.ignorecase=bogus", "last-modified", "--max-depth=true"],
    &["-c", "core.ignorecase=bogus", "last-modified", "nosuch"],
    &["-c", "core.ignorecase=bogus", "last-modified", "-h"],
    &["-c", "core.packedGitLimit=bogus", "last-modified", "--max-depth=true"],
    &["-c", "core.packedGitLimit=bogus", "last-modified", "nosuch"],
    &["-c", "core.packedGitLimit=bogus", "last-modified", "HEAD", "nosuch"],
    &["-c", "core.packedGitLimit=bogus", "last-modified"],
    &["-c", "core.packedGitLimit=bogus", "last-modified", "--bogus"],
    &["-c", "core.packedGitLimit=bogus", "last-modified", "-h"],
];

#[test]
fn argument_and_config_order_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-lm-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("s"), base.join("z"));
    fixture(&s);
    fixture(&z);
    let (s, z) = (s.canonicalize().unwrap(), z.canonicalize().unwrap());
    for args in CASES {
        // A case that already carries `-c <k=v> last-modified` is spelled out; the rest get the verb.
        let mut full: Vec<&str> = Vec::new();
        if args.first() != Some(&"-c") {
            full.push("last-modified");
        }
        full.extend_from_slice(args);
        let want = run(stock, &s, &full);
        let got = run(BIN, &z, &full);
        assert_eq!(got, want, "git {full:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
