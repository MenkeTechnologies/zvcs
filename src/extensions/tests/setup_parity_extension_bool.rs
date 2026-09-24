//! `extensions.worktreeConfig` / `extensions.preciousObjects` with a value that
//! is not a boolean.
//!
//! `handle_extension_v0()` reads both with `git_config_bool()` (setup.c:622,
//! 630) while `read_repository_format()` walks `.git/config`, and that function
//! `die()`s on an unparsable value itself — so every verb that runs setup exits
//! 128 with `bad boolean config value '<v>' for 'extensions.<key>'` and nothing
//! else, not the `error:` + `bad config line` pair a table-checked extension
//! gets. Before this, zvcs ignored the value and ran the command.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.
#![cfg(unix)]

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-setup-ext-bool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "init"]);
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn append_config(&self, text: &str) {
        let path = self.work.join(".git/config");
        let mut config = std::fs::read_to_string(&path).unwrap();
        config.push_str(text);
        std::fs::write(path, config).unwrap();
    }
}

/// The die precedes the builtin's own usage error and a plain `status`.
#[test]
fn unparsable_worktree_config_dies_before_any_verb_runs() {
    let f = Fixture::new("wtc");
    f.append_config("[extensions]\n\tworktreeConfig = input\n");
    let fatal = "fatal: bad boolean config value 'input' for 'extensions.worktreeconfig'\n";
    for args in [&["merge-recursive-ours", "main"][..], &["status"], &["log", "--oneline"]] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("", fatal, 128), "{args:?}");
    }
}

/// `preciousObjects` is read the same way; integers are booleans to git, so
/// `0x10` is accepted and the command runs.
#[test]
fn precious_objects_is_checked_with_the_full_boolean_grammar() {
    let f = Fixture::new("precious");
    f.append_config("[extensions]\n\tpreciousObjects = maybe\n");
    let (out, err, code) = f.run(&["rev-parse", "HEAD"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: bad boolean config value 'maybe' for 'extensions.preciousobjects'\n", 128)
    );

    let f = Fixture::new("precious-int");
    f.append_config("[extensions]\n\tpreciousObjects = 0x10\n");
    let (out, err, code) = f.run(&["rev-parse", "--is-inside-work-tree"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("true\n", "", 0));
}
