//! Two things `checkout-index` leaves on disk.
//!
//! * `--temp` creates each file with libc's `mkstemp()` (`open_output_fd()`,
//!   entry.c:109-112), so it is mode `0600`. zvcs created it `0666 & ~umask`.
//! * The index `-u` writes is smudged where an entry is racily clean and
//!   `ce_modified_check_fs()` finds its content changed (read-cache.c:2574-2594).
//!   For a regular file that is `ce_compare_data()`, which hashes through
//!   `index_fd()` with the entry's path (read-cache.c:231-245), so the clean
//!   filter, `ident` and EOL normalisation run first and a file that differs from
//!   its blob only by what checkout put there is unchanged. zvcs hashed the raw
//!   bytes and recorded size 0 for every such file.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, home: &Path, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

impl Twin {
    /// Files under a clean filter, `ident` and `eol=crlf`, committed and then deleted
    /// from the work tree.
    fn new(tag: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-citemp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("stock");
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let (_, err, code) = git(stock, &root, &dir, args);
            assert_eq!(code, 0, "fixture `git {args:?}`: {err}");
        };
        run(&["init", "-q", "-b", "main", "."]);
        run(&["config", "filter.up.clean", "tr a-z A-Z"]);
        run(&["config", "filter.up.smudge", "tr A-Z a-z"]);
        std::fs::write(dir.join(".gitattributes"), "*.flt filter=up\n*.id ident\n*.crlf text eol=crlf\n").unwrap();
        std::fs::write(dir.join("w.flt"), "hello\n").unwrap();
        std::fs::write(dir.join("y.id"), "$Id$\n").unwrap();
        std::fs::write(dir.join("x.crlf"), "l1\nl2\n").unwrap();
        std::fs::write(dir.join("plain.txt"), "plain\n").unwrap();
        run(&["add", "-A"]);
        run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
        for name in ["w.flt", "y.id", "x.crlf", "plain.txt"] {
            std::fs::remove_file(dir.join(name)).unwrap();
        }
        let status = Command::new("cp").arg("-Rp").arg(&dir).arg(root.join("zvcs")).status().unwrap();
        assert!(status.success());
        Twin { root }
    }

    fn both(&self, stock: &str, args: &[&str]) -> [(String, String, i32); 2] {
        [
            git(stock, &self.root, &self.root.join("stock"), args),
            git(BIN, &self.root, &self.root.join("zvcs"), args),
        ]
    }

    /// `ls-files --debug` read by stock git, without the time and inode lines that
    /// differ between the two copies.
    fn debug(&self, stock: &str, which: &str) -> String {
        let (out, _, _) = git(stock, &self.root, &self.root.join(which), &["ls-files", "--debug"]);
        out.lines()
            .filter(|l| !l.trim_start().starts_with("ctime:") && !l.trim_start().starts_with("mtime:") && !l.trim_start().starts_with("dev:"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[test]
fn temporary_files_are_owner_only() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("temp", stock);
    let [s, z] = t.both(stock, &["checkout-index", "--temp", "plain.txt", "w.flt"]);
    assert_eq!(s.2, 0, "{s:?}");
    assert_eq!(z.2, 0, "{z:?}");
    for (which, out) in [("stock", &s.0), ("zvcs", &z.0)] {
        let names: Vec<&str> = out.lines().map(|l| l.split('\t').next().unwrap()).collect();
        assert_eq!(names.len(), 2, "{out}");
        for name in names {
            assert!(name.starts_with(".merge_file_"), "{name}");
            let mode = std::fs::metadata(t.root.join(which).join(name)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{which} {name}");
        }
    }
}

#[test]
fn a_racily_clean_filtered_file_is_compared_through_its_filters() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("racy", stock);
    let [s, z] = t.both(stock, &["checkout-index", "-a", "-u", "-f"]);
    assert_eq!(z, s);
    let stock_debug = t.debug(stock, "stock");
    assert!(!stock_debug.contains("size: 0"), "{stock_debug}");
    assert_eq!(t.debug(stock, "zvcs"), stock_debug);
}
