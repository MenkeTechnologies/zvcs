//! The file name receive-pack's config refusals print.
//!
//! `cmd_receive_pack()` calls `enter_repo()` before `repo_config(…,
//! receive_pack_config, …)` (builtin/receive-pack.c:2645-2652), and
//! `enter_repo()` chdirs into the git directory and calls `set_git_dir(".")`
//! (path.c:760-834). The repository's own config is therefore `./config`,
//! printed through `strbuf_cleanup_path()` as plain `config` — by
//! `die_bad_number()` (`… in file config: invalid unit`) and by
//! `git_die_config_linenr()` (`… in file 'config' at line N`) alike, however
//! the directory was spelled on the command line. zvcs named the file by its
//! absolute path.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `r.git` (bare), `nb` (non-bare) and `w`, a work tree with one commit and
    /// `r.git` as its `origin`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-receive-pack-config-path-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "--bare", "-b", "main", "r.git"]);
        f.run(&["init", "-q", "-b", "main", "nb"]);
        f.run(&["init", "-q", "-b", "main", "w"]);
        std::fs::write(f.root.join("w/a"), "a\n").unwrap();
        f.run(&["-C", "w", "add", "a"]);
        f.run(&["-C", "w", "commit", "-q", "-m", "a"]);
        f.run(&["-C", "w", "remote", "add", "origin", "../r.git"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
}

const BAD_NUMBER: &str =
    "fatal: bad numeric config value 'bogus' for 'receive.unpacklimit' in file config: invalid unit\n";

#[test]
fn a_bad_number_names_the_file_relative_to_the_git_dir() {
    let f = Fixture::new("number");
    f.run(&["-C", "r.git", "config", "receive.unpackLimit", "bogus"]);
    f.run(&["-C", "nb", "config", "receive.unpackLimit", "bogus"]);
    let absolute = f.root.join("r.git");
    for dir in ["r.git", absolute.to_str().unwrap(), "nb"] {
        assert_eq!(f.run(&["receive-pack", dir]), (String::new(), BAD_NUMBER.to_string(), 128), "{dir}");
    }
    // The pushing side relays the remote's line ahead of its own refusal.
    let (out, err, code) = f.run(&["-C", "w", "push", "-q", "origin", "main"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(err.starts_with(&format!("{BAD_NUMBER}fatal: Could not read from remote repository.\n")), "{err}");
}

#[test]
fn a_valueless_hide_refs_names_the_file_and_line_relative_to_the_git_dir() {
    let f = Fixture::new("nonbool");
    let config = f.root.join("r.git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[receive]\n\thideRefs\n");
    let line = text.lines().count();
    std::fs::write(&config, text).unwrap();
    assert_eq!(
        f.run(&["receive-pack", "r.git"]),
        (
            String::new(),
            format!(
                "error: missing value for 'receive.hiderefs'\n\
                 fatal: bad config variable 'receive.hiderefs' in file 'config' at line {line}\n"
            ),
            128
        )
    );
}
