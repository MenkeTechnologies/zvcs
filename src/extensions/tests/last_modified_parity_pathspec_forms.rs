//! `git last-modified` against stock git: pathspec forms.
//!
//! * `.` and `./x`/`x/../y` are normalised by `prefix_path()` before matching, so `.`
//!   lists everything rather than nothing.
//! * Wildcard items go through `tree_entry_interesting()`'s `do_match()`: `*` crosses
//!   `/` (no `:(glob)`), a directory no item rules out is descended into.
//! * `diff_setup_done()` refuses a depth limit together with a wildcard pathspec before
//!   `last-modified` reports an unknown argument; without `-r` the depth is 0.
//! * `:^`/`:!`/`:(exclude)` items are a second pass over what the positive items accepted, and an
//!   all-exclude pathspec gains an implicit "match everything" item.
//! * A path outside the repository dies with git's pathspec wording.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str], date: Option<&str>) -> Outcome {
    let mut cmd = Command::new(bin);
    cmd.args(args)
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
        .env("GIT_COMMITTER_EMAIL", "c@x");
    if let Some(date) = date {
        cmd.env("GIT_AUTHOR_DATE", date).env("GIT_COMMITTER_DATE", date);
    }
    let out = cmd.output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// Four commits with fixed dates, each touching a different subset of the tree.
fn fixture(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let git = |args: &[&str], at: u64| run(BIN, root, args, Some(&format!("{at} +0000")));
    git(&["init", "-q", "-b", "main"], 1_700_000_000);
    let write = |name: &str, body: &str| {
        let p = root.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    for (n, files) in [
        vec!["a.rs", "b.txt", "src/c.rs", "src/d.md", "src/deep/e.rs", "docs/f.md"],
        vec!["b.txt", "src/d.md"],
        vec!["src/deep/e.rs", "src/deep/x/y.rs"],
        vec!["sp ace.rs"],
    ]
    .into_iter()
    .enumerate()
    {
        for f in files {
            write(f, &format!("{f} {n}\n"));
        }
        git(&["add", "-A"], 1_700_000_000 + n as u64 * 100);
        git(&["commit", "-q", "-m", &format!("c{n}")], 1_700_000_000 + n as u64 * 100);
    }
}

const CASES: &[(&str, &[&str])] = &[
    ("", &["-r", "--", "."]),
    ("", &["-r", "-z", "--", "./src/../docs"]),
    ("", &["-r", "--", "*.rs"]),
    ("", &["-r", "--", "src/*.rs"]),
    ("", &["-r", "--", "*/y.rs"]),
    ("", &["-r", "--", "?rc"]),
    ("", &["-r", "--", "[ab].*"]),
    ("", &["-r", "--", "src/deep/*"]),
    ("", &["-r", "--", "d*/"]),
    ("", &["-r", "--", "*.rs", "*.md"]),
    ("", &["-r", "-t", "--", "s*"]),
    ("", &["-r", "--no-show-trees", "--", ".", "--max-depth=0", "*.rs", "-z"]),
    ("src", &["-r", "--", "*.rs"]),
    ("src", &["-r", "--", "."]),
    ("src/deep", &["-r", "--", "../*.md"]),
    ("", &["--", "*.rs"]),
    ("", &["--max-depth=1", "--", "src/*"]),
    ("", &["--max-depth=0", "--bogus-option", "--", "*.md"]),
    ("", &["-r", "--", ":^src"]),
    ("", &["-r", "--", "src", ":!src/deep"]),
    ("", &["-r", "--", ":(exclude)*.md"]),
    ("", &["-r", "--", "*.rs", ":^src/deep/x"]),
    ("", &["--max-depth=1", "--", "src", ":^src/deep"]),
    ("", &["--", ":^src"]),
    ("src", &["-r", "--", ":^deep"]),
    ("", &["-r", "--", "/"]),
    ("", &["-r", "--", "../x"]),
];

#[test]
fn pathspec_forms_match_stock() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-lm-pathspec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("s"), base.join("z"));
    fixture(&s);
    fixture(&z);
    let (s, z) = (s.canonicalize().unwrap(), z.canonicalize().unwrap());
    for (sub, args) in CASES {
        let mut full = vec!["last-modified"];
        full.extend_from_slice(args);
        let want = run(stock, &s.join(sub), &full, None);
        let got = run(BIN, &z.join(sub), &full, None);
        // The outside-repository message names the work tree, which differs per side.
        let norm = |o: Outcome| {
            (
                o.0,
                o.1.replace(s.to_str().unwrap(), "<root>").replace(z.to_str().unwrap(), "<root>"),
                o.2,
            )
        };
        assert_eq!(norm(got), norm(want), "last-modified {args:?} from {sub:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn dot_lists_the_whole_tree() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-lm-dot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let s = base.join("s");
    fixture(&s);
    let all = run(stock, &s, &["last-modified", "-r"], None).0;
    let dot = run(BIN, &s, &["last-modified", "-r", "--", "."], None).0;
    assert!(!all.is_empty());
    assert_eq!(dot, all);
    let _ = std::fs::remove_dir_all(&base);
}
