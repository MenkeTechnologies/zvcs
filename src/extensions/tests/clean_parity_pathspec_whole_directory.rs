//! `git clean` with a pathspec takes an untracked directory whole when the
//! pathspec matches the directory itself.
//!
//! `cmd_clean()` walks with `DIR_SHOW_OTHER_DIRECTORIES` (builtin/clean.c:964).
//! `treat_directory()` asks the pathspec about the directory name only, with its
//! trailing slash and `DO_MATCH_LEADING_PATHSPEC` (dir.c:1998-2005): no match is
//! `path_none`, a merely leading one recurses, and anything else makes the
//! directory `path_untracked` — also when nothing below it matched
//! (dir.c:2212-2213). Files are tested against the pathspec before their ignore
//! status (dir.c:2501-2509), so a file the pathspec does not reach is neither a
//! candidate nor an ignored path. `correct_untracked_entries()`
//! (builtin/clean.c:887-917) then keeps a directory unless it holds an ignored
//! path, and `remove_dirs()` removes it without consulting the pathspec again.
//!
//! zvcs required every entry under the directory to match: `*/` found nothing,
//! an exclusion naming a file inside (`:!ud/f`) or a directory inside
//! (`:!ud/sub`) listed the survivors one by one, and `*` with `:!*y` dropped
//! `ux/` altogether.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Tracked `t` and `td/t`; untracked `td/u`, `ud/{f,g,sub/h}`, `ux/y`, `uf`,
    /// `xy`, `ui/a`. With `ignore`, `*.o` is ignored and `ui/b.o` exists.
    fn new(tag: &str, ignore: bool) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clean-whole-dir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        if ignore {
            f.write(".gitignore", "*.o\n");
        }
        f.write("t", "a\n");
        f.write("td/t", "a\n");
        f.run(&["add", "."]);
        f.run(&["commit", "-q", "-m", "a"]);
        for path in ["td/u", "ud/f", "ud/g", "ud/sub/h", "ux/y", "uf", "xy", "ui/a"] {
            f.write(path, "x\n");
        }
        if ignore {
            f.write("ui/b.o", "x\n");
        }
        f
    }

    fn write(&self, path: &str, body: &str) {
        let p = self.work.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
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

    fn dry_run(&self, pathspecs: &[&str]) -> String {
        let mut args = vec!["clean", "-nd"];
        args.extend_from_slice(pathspecs);
        let (out, err, code) = self.run(&args);
        assert_eq!((err.as_str(), code), ("", 0), "{pathspecs:?}");
        out
    }
}

fn would(paths: &[&str]) -> String {
    paths.iter().map(|p| format!("Would remove {p}\n")).collect()
}

#[test]
fn a_trailing_slash_pathspec_takes_directories_whose_files_it_cannot_match() {
    let f = Fixture::new("slash", false);
    assert_eq!(f.dry_run(&["*/"]), would(&["ud/", "ui/", "ux/"]));
    assert_eq!(f.dry_run(&["u*/"]), would(&["ud/", "ui/", "ux/"]));
}

#[test]
fn an_exclusion_inside_a_matched_directory_does_not_split_it() {
    let f = Fixture::new("inside", false);
    assert_eq!(f.dry_run(&["ud", ":!ud/f"]), would(&["ud/"]));
    assert_eq!(f.dry_run(&["*", ":!*y"]), would(&["td/u", "ud/", "uf", "ui/", "ux/"]));
    // An excluded directory is `path_none`: it keeps nothing of its own and does
    // not stop its parent from being taken whole.
    assert_eq!(
        f.dry_run(&[":!ud/sub"]),
        would(&["td/u", "ud/", "uf", "ui/", "ux/", "xy"])
    );

    // `remove_dirs()` does not look at the pathspec: the excluded file goes too.
    let (out, err, code) = f.run(&["clean", "-fd", "ud", ":!ud/f"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("Removing ud/\n", "", 0));
    assert!(!f.work.join("ud").exists());
}

#[test]
fn an_ignored_file_outside_the_pathspec_does_not_keep_its_directory() {
    let f = Fixture::new("ignored", true);
    // `ui/b.o` is ignored, but `:!*.o` makes it `path_none` before that is asked.
    assert_eq!(
        f.dry_run(&["*", ":!*.o"]),
        would(&["td/u", "ud/", "uf", "ui/", "ux/", "xy"])
    );
    // Reached by the pathspec, the ignored file keeps `ui/` from being removed whole.
    assert_eq!(f.dry_run(&["ui"]), would(&["ui/a"]));
    // A file `*/` cannot match is `path_none` whether ignored or not.
    assert_eq!(f.dry_run(&["*/"]), would(&["ud/", "ui/", "ux/"]));
}
