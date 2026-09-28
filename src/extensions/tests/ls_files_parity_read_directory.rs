//! `ls-files -o` is `fill_directory()` under the flags its options set.
//!
//! `-i` is `DIR_SHOW_IGNORED`, `--directory` is `DIR_SHOW_OTHER_DIRECTORIES` and
//! `--no-empty-directory` is `DIR_HIDE_EMPTY_DIRECTORIES` (builtin/ls-files.c:
//! 618-631); `show_other_files()` prints `dir->entries` as the walk left them
//! (builtin/ls-files.c:166-177). What lands there is `treat_directory()`'s call
//! (dir.c:1966-2221):
//!
//! * an untracked directory holding only ignored paths is `path_excluded` once
//!   `read_directory_recursive()` has added everything below it — with
//!   `DIR_SHOW_IGNORED` those go to `entries` and are never popped, so `-oi
//!   --directory` lists `uj/`, `uj/a.o`, `uj/k/` and `uj/k/b.o`;
//! * an untracked directory with an ignored file among untracked ones is
//!   `path_untracked`, and under `DIR_SHOW_IGNORED` only its ignored files show;
//! * without `DIR_SHOW_IGNORED` or `DIR_HIDE_EMPTY_DIRECTORIES` an untracked
//!   directory is `path_untracked` without ever being opened, so an unreadable
//!   one draws no warning;
//! * `ls-files` prunes the index to the pathspecs' common directory before the
//!   walk (builtin/ls-files.c:744-750), so under `DIR_COLLECT_KILLED_ONLY` (`-k`)
//!   `tr/` has nothing in the index and is not entered at all (dir.c:2468-2472).
//!
//! zvcs listed a flat gix walk through post-filters: `-oi --directory` named
//! only the top-level ignored file, `--directory` warned about directories stock
//! never opens, and `-k -- tr/sub/zz/` tried to open the missing directory.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = set_mode(&self.work.join("lock"), 0o755);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

impl Fixture {
    /// `.gitignore` ignores `*.o` and `ign/`. Tracked: `t`, `tr/sub/b`.
    /// Untracked: `u/` (an untracked file among ignored ones), `uj/` and `ul/`
    /// (ignored files only), `ud/` (an ignored file beside a nested repository),
    /// `ign/` (an ignored directory), `lock/` (unreadable).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ls-files-rd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.write(".gitignore", "*.o\nign/\n");
        f.write("t", "t\n");
        f.write("tr/sub/b", "t\n");
        f.run(&["add", ".gitignore", "t", "tr"]);
        f.run(&["commit", "-q", "-m", "i"]);
        for path in [
            "top.o", "ign/a", "ign/sub/b", "u/x.o", "u/deep/y.o", "u/deep/f", "uj/a.o", "uj/k/b.o",
            "ul/f.o", "ud/a.o", "lock/in/f",
        ] {
            f.write(path, "");
        }
        f.run(&["init", "-q", "ud/nest"]);
        set_mode(&f.work.join("lock"), 0o000).unwrap();
        f
    }

    fn write(&self, rela: &str, content: &str) {
        let path = self.work.join(rela);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Whether `chmod 000` keeps this process out (it does not for root).
    fn lock_is_unreadable(&self) -> bool {
        std::fs::read_dir(self.work.join("lock")).is_err()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const LOCK_WARNING: &str = "warning: could not open directory 'lock/': Permission denied\n";

#[test]
fn ignored_directory_listing_names_what_the_walk_added_below_it() {
    let f = Fixture::new("oi");
    let warning = if f.lock_is_unreadable() { LOCK_WARNING } else { "" };
    let (out, err, code) = f.run(&["ls-files", "-o", "-i", "--directory", "--exclude-standard"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "ign/\ntop.o\nu/deep/y.o\nu/x.o\nud/a.o\nuj/\nuj/a.o\nuj/k/\nuj/k/b.o\nul/\nul/f.o\n",
            warning,
            0
        )
    );

    let (out, err, code) = f.run(&[
        "ls-files",
        "-o",
        "-i",
        "--directory",
        "--no-empty-directory",
        "--exclude-standard",
    ]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("ign/\ntop.o\nuj/\nul/\n", warning, 0)
    );

    // `*.o` is only a leading match for `uj/`, so the walk recurses instead of
    // listing the directory (dir.c:2074-2081); the ignored directory skips the
    // pathspec test altogether (dir.c:1997).
    let (out, err, code) = f.run(&["ls-files", "-o", "-i", "--directory", "--exclude-standard", "--", "*.o"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "ign/\ntop.o\nu/deep/y.o\nu/x.o\nud/a.o\nuj/a.o\nuj/k/b.o\nul/f.o\n",
            warning,
            0
        )
    );
}

#[test]
fn other_directories_are_listed_without_being_opened() {
    let f = Fixture::new("dir");
    let (out, err, code) = f.run(&["ls-files", "-o", "--directory", "--exclude-standard"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("lock/\nu/\nud/\nuj/\nul/\n", "", 0)
    );

    // Hiding empty directories means looking inside; `uj/` and `ul/` hold
    // nothing but ignored files, and `ud/` counts for its nested repository.
    let warning = if f.lock_is_unreadable() { LOCK_WARNING } else { "" };
    let (out, err, code) = f.run(&[
        "ls-files",
        "-o",
        "--directory",
        "--no-empty-directory",
        "--exclude-standard",
    ]);
    let want_out = if warning.is_empty() { "lock/\nu/\nud/\n" } else { "u/\nud/\n" };
    assert_eq!((out.as_str(), err.as_str(), code), (want_out, warning, 0));
}

#[test]
fn killed_listing_walks_the_pruned_index() {
    let f = Fixture::new("killed");
    let (out, err, code) = f.run(&["ls-files", "-k", "--", "tr/sub/zz/"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));

    let (out, err, code) = f.run(&["ls-files", "-o", "--", "tr/sub/zz/"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "warning: could not open directory 'tr/sub/zz/': No such file or directory\n", 0)
    );
}
