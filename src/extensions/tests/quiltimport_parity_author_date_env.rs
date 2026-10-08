//! `git quiltimport` against stock git: the author date of an imported patch.
//!
//! git-quiltimport.sh exports `GIT_AUTHOR_DATE` unconditionally from the mailinfo `Date:`
//! header. A patch without that header exports an empty value, which means "now" and
//! shadows a `GIT_AUTHOR_DATE` the caller had in the environment; a patch with the header
//! uses it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const PINNED: &str = "1700000000 +0000";

fn run(bin: &str, dir: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", PINNED)
        .env("GIT_COMMITTER_DATE", PINNED)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository whose `quilt/` holds one patch, with `date_header` as its `Date:` line.
fn fixture(root: &Path, date_header: &str) {
    std::fs::create_dir_all(root.join("quilt")).unwrap();
    run(BIN, root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "hi\n").unwrap();
    run(BIN, root, &["add", "f"]);
    run(BIN, root, &["commit", "-qm", "init"]);
    let patch = format!(
        "From: P <p@x>\n{date_header}Subject: [PATCH] add g\n\n---\n g | 1 +\n 1 file changed, 1 insertion(+)\n\n\
         diff --git a/g b/g\nnew file mode 100644\nindex 0000000..587be6b\n--- /dev/null\n+++ b/g\n@@ -0,0 +1 @@\n+x\n"
    );
    std::fs::write(root.join("quilt/p1.patch"), patch).unwrap();
    std::fs::write(root.join("quilt/series"), "p1.patch\n").unwrap();
}

fn author_line(bin: &str, root: &Path) -> String {
    run(bin, root, &["quiltimport", "--patches", "quilt"]);
    run(bin, root, &["cat-file", "-p", "HEAD"])
        .lines()
        .find(|l| l.starts_with("author "))
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn patch_date_header_is_the_author_date() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-quilt-{}-{}", line!(), std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("s"), base.join("z"));
    let header = "Date: Wed, 15 Nov 2023 22:13:20 +0000\n";
    fixture(&s, header);
    fixture(&z, header);
    let want = author_line(stock, &s);
    assert!(want.contains("1700086400 +0000"), "{want}");
    assert_eq!(author_line(BIN, &z), want);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn patch_without_date_header_ignores_environment_author_date() {
    let Some(stock) = stock_git::stock_git() else { return };
    let base = std::env::temp_dir().join(format!("zvcs-quilt-{}-{}", line!(), std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (s, z) = (base.join("s"), base.join("z"));
    fixture(&s, "");
    fixture(&z, "");
    let stock_author = author_line(stock, &s);
    let zvcs_author = author_line(BIN, &z);
    assert!(!stock_author.contains("1700000000"), "stock: {stock_author}");
    assert!(!zvcs_author.contains("1700000000"), "zvcs: {zvcs_author}");
    let _ = std::fs::remove_dir_all(&base);
}
