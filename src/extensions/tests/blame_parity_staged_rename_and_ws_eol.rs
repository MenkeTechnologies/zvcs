//! Two blame gaps of the working-tree overlay and of `-w`.
//!
//! * A path renamed in the index (`git mv old new`, uncommitted) is blamed through
//!   the rename: the fake working-tree commit's `pass_blame()` asks `find_rename()`
//!   (blame.c:1422-1456), whose `do_diff_cache()` of `HEAD` against the index with
//!   `single_follow = path` pairs the new path with the old one, so the lines go to
//!   the commits that wrote them under the old name, shown in the filename column.
//!   zvcs gave every line to "Not Committed Yet".
//! * Under `-w` the `\n` ending a record is whitespace to `xdl_recmatch()`
//!   (`XDL_ISSPACE()`), so a last line that gains a newline is unchanged. zvcs
//!   blamed it on the commit that added the newline.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// `a.txt` is written, edited, and renamed to `b.txt` with two lines appended;
/// `nonl.txt` gains its missing final newline and a second line.
const FIXTURE: &str = r#"
set -e
t=1700000000
c() { t=$((t+3600)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" commit -q -m "$1"; }
"$GIT" init -q -b main .
printf 'a\nb\nc\nd\ne\nf\n' > a.txt
printf 'no newline' > nonl.txt
"$GIT" add .; c one
printf 'a\nB\nc\nd\nE\nf\n' > a.txt
printf 'no newline\nmore\n' > nonl.txt
"$GIT" add .; c two
"$GIT" mv a.txt b.txt; printf 'x\ny\n' >> b.txt; "$GIT" add b.txt; c three
printf 'p\n' > other.txt; "$GIT" add other.txt; c four
"#;

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Twin {
    fn new(stock: &str, label: &str) -> Twin {
        let root = std::env::temp_dir().join(format!("zvcs-blame-staged-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for side in ["stock", "zvcs"] {
            let dir = root.join(side);
            std::fs::create_dir_all(&dir).unwrap();
            let out = env(&mut Command::new("sh"), &root)
                .args(["-c", FIXTURE])
                .env("GIT", stock)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
        }
        Twin { root }
    }

    fn sh(&self, bin: &str, side: &str, script: &str) -> (String, String, i32) {
        let out = env(&mut Command::new("sh"), &self.root)
            .args(["-c", script])
            .env("GIT", bin)
            .env("STOCK", stock_git().unwrap())
            .current_dir(self.root.join(side))
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn same(&self, stock: &str, script: &str) {
        assert_eq!(self.sh(ZVCS, "zvcs", script), self.sh(stock, "stock", script), "{script}");
    }
}

fn env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
}

/// The working-tree lines the rename did not touch belong to `HEAD`'s history.
#[test]
fn a_rename_staged_in_the_index_is_followed() {
    let Some(stock) = stock_git() else { return };
    let mv = r#""$STOCK" mv b.txt c.txt; "#;
    let no_times = r#" | grep -v '^\(author\|committer\)-time'"#;
    for (i, script) in [
        format!(r#"{mv}"$GIT" blame -s c.txt"#),
        format!(r#"{mv}"$GIT" blame -f -n c.txt"#),
        format!(r#"{mv}echo z >> c.txt; "$GIT" blame -s -L 6, c.txt"#),
        format!(r#"{mv}"$GIT" blame --line-porcelain c.txt{no_times}"#),
        format!(r#"{mv}echo z >> c.txt; "$GIT" blame --porcelain c.txt{no_times}"#),
        format!(r#"{mv}"$GIT" blame --incremental c.txt{no_times}"#),
        // A copy is no rename: the old path is still in the index.
        r#"cp b.txt d.txt; "$STOCK" add d.txt; "$GIT" blame -s d.txt"#.to_owned(),
    ]
    .iter()
    .enumerate()
    {
        Twin::new(stock, &format!("mv{i}")).same(stock, script);
    }
}

#[test]
fn a_final_newline_is_whitespace_under_w() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "ws");
    t.same(stock, r#""$GIT" blame -w -s nonl.txt"#);
    t.same(stock, r#""$GIT" blame -w --porcelain nonl.txt"#);
    t.same(stock, r#""$GIT" blame -s nonl.txt"#);
    t.same(stock, r#"printf 'no newline\nmore' > nonl.txt; "$GIT" blame -w -s nonl.txt"#);
}
