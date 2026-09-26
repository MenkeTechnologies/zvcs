//! `git stash list` is `git log -g`, so it takes the whole `log` vocabulary.
//!
//! `list_stash()` (builtin/stash.c) returns 0 when `refs/stash` does not exist
//! and otherwise runs
//!
//! ```c
//! strvec_pushl(&cp.args, "log", "--format=%gd: %gs", "-g",
//!              "--first-parent", NULL);
//! strvec_pushv(&cp.args, argv);
//! strvec_pushl(&cp.args, ref_stash, "--", NULL);
//! ```
//!
//! so every user format placeholder, `%C` colour, `--date` mode and diff option
//! behaves exactly as under `git log -g`. zvcs fed the arguments to a separate
//! reflog renderer that refused the `%+` magic, `%C(...)` under `--color=always`,
//! and the `human`, `relative` and `format:` date modes as "not ported".
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

//! walked HEAD limited to it.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// `file` committed as `one`, then a change to it stashed.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-stash-list-as-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "x\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("file"), "y\n").unwrap();
        f.run(&["stash", "-q"]);
        f
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
            .env("GIT_PAGER", "cat")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "{args:?}");
        out
    }

    /// `WIP on main: <short> one`, what `git stash` recorded.
    fn subject(&self) -> String {
        let short = self.stdout(&["rev-parse", "--short", "main"]);
        format!("WIP on main: {} one", short.trim_end())
    }
}

#[test]
fn user_format_magic_and_colour_render_as_under_log() {
    let f = Fixture::new("format");
    assert_eq!(
        f.stdout(&["stash", "list", "--format=%gd%+gs"]),
        format!("stash@{{0}}\n{}\n", f.subject())
    );
    assert_eq!(
        f.stdout(&["stash", "list", "--format=%C(red)%gd%C(reset)", "--color=always"]),
        "\x1b[31mstash@{0}\x1b[m\n"
    );
}

#[test]
fn every_date_mode_reaches_the_selector() {
    let f = Fixture::new("date");
    assert_eq!(
        f.stdout(&["stash", "list", "--date=format:%Y"]),
        format!("stash@{{2023}}: {}\n", f.subject())
    );
    assert_eq!(
        f.stdout(&["stash", "list", "--date=human"]),
        format!("stash@{{Nov 14 2023}}: {}\n", f.subject())
    );
}
