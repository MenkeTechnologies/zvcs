//! `--check` against stock git under the options that change a patch but not the
//! check: `builtin_checkdiff()` runs xdiff with its own zeroed parameters,
//!
//! ```c
//! memset(&xpp, 0, sizeof(xpp));
//! memset(&xecfg, 0, sizeof(xecfg));
//! xecfg.ctxlen = 1; /* at least one context line */
//! xpp.flags = 0;
//! ```
//! (diff.c:4300-4303), so `-w`, `-b`, `--ignore-space-at-eol`, `-I`, the diff
//! algorithm and the indent heuristic never hide an added line from it. zvcs
//! walked the hunks the command had built with those options, and so lost every
//! whitespace error on a line the option folded away; `diff-files` refused `-I`
//! with `--check` outright.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn check_ignores_the_patch_options() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-check-xpp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = Repo(root);
    let work = repo.0.join("w");
    std::fs::create_dir_all(&work).unwrap();
    let git = |args: &[&str]| {
        let o = run(stock, &work, &repo.0, args);
        assert_eq!(o.2, Some(0), "{args:?}: {}", o.1);
    };
    git(&["-c", "init.defaultBranch=main", "init", "-q"]);
    std::fs::write(work.join("w.c"), "int main() {\n\treturn 0;\n}\nfoo bar\nbaz  qux\n\nend\n").unwrap();
    std::fs::write(work.join("t.txt"), "a\nb\nc\n").unwrap();
    git(&["add", "."]);
    git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    // Whitespace-only rewrites, a CR at the end of a line, a TODO line `-I` drops,
    // and an incomplete last line.
    std::fs::write(work.join("w.c"), "int main()  {\n        return 0;   \n}\nfoo  bar\nbazqux\n\n\n\nend \n").unwrap();
    std::fs::write(work.join("t.txt"), "a\r\nb\nc\nTODO trailing \nlast ").unwrap();

    for opt in ["-w", "-b", "--ignore-space-at-eol", "-ITODO", "--histogram"] {
        for args in [
            &["diff", opt, "--check"][..],
            &["diff-files", opt, "--check"],
            &["diff-index", opt, "--check", "HEAD"],
        ] {
            let want = run(stock, &work, &repo.0, args);
            assert_eq!(run(BIN, &work, &repo.0, args), want, "{args:?}");
        }
    }
    let args = ["diff", "-w", "--check", "--stat"];
    let want = run(stock, &work, &repo.0, &args);
    assert_eq!(run(BIN, &work, &repo.0, &args), want, "{args:?}");
}
