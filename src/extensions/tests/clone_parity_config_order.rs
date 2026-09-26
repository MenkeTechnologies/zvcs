//! Where `clone -c <key>=<value>` puts its pairs in the new `.git/config`.
//!
//! `write_config()` replays `option_config` straight after `init_db()`
//! (builtin/clone.c:1189, :1241), through `git_config_set_multivar_gently()`,
//! which canonicalizes each key with `git_config_parse_key()` — section and
//! variable name lowercased, subsection kept. Everything the clone sets later
//! (`remote.<name>.url`, its `fetch` refspec, `branch.<name>.*`) is a
//! `git_config_set()` that appends to the end of an existing section of the same
//! name. So the `-c` sections sit between `[core]` and the remote, and a `-c`
//! key in `[remote "origin"]` or `[branch "main"]` precedes the keys the clone
//! adds there. zvcs appended every pair at the very end of the file, in the case
//! it was typed.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clone-config-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        f.run(&src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("a"), "a\n").unwrap();
        f.run(&src, &["add", "a"]);
        f.run(&src, &["commit", "-q", "-m", "a"]);
        f
    }

    /// Clone `src` to `dst` with `opts` and return the new config file, with the
    /// fixture root abbreviated to `$R` and the `[core]` block (whose platform
    /// keys vary) cut down to its header.
    fn config(&self, dst: &str, config: &str, opts: &[&str]) -> String {
        let mut args = vec!["clone", "-q"];
        args.extend_from_slice(opts);
        args.extend_from_slice(&["src", dst]);
        assert_eq!(self.run(&self.root, &args), (String::new(), String::new(), 0), "{opts:?}");
        let text = std::fs::read_to_string(self.root.join(dst).join(config)).unwrap();
        let mut out = String::new();
        let mut in_core = false;
        for line in text.lines() {
            if line.starts_with('[') {
                in_core = line == "[core]";
            } else if in_core {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out.replace(self.root.to_str().unwrap(), "$R")
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
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

#[test]
fn pairs_precede_what_the_clone_writes_afterwards() {
    let f = Fixture::new("order");
    assert_eq!(
        f.config(
            "d",
            ".git/config",
            &["-c", "foo.bar=1", "-c", "remote.origin.x=y", "-c", "branch.main.q=1", "-c", "foo.bar=2"],
        ),
        "[core]\n\
         [foo]\n\tbar = 1\n\tbar = 2\n\
         [remote \"origin\"]\n\tx = y\n\turl = $R/src\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
         [branch \"main\"]\n\tq = 1\n\tremote = origin\n\tmerge = refs/heads/main\n"
    );
}

#[test]
fn section_and_name_are_lowercased_but_not_the_subsection() {
    let f = Fixture::new("case");
    assert_eq!(
        f.config("d", ".git/config", &["-c", "Foo.BAR=1", "-c", "remote.Origin.X=1", "-c", "remote.origin.XyZ=2"]),
        "[core]\n\
         [foo]\n\tbar = 1\n\
         [remote \"Origin\"]\n\tx = 1\n\
         [remote \"origin\"]\n\txyz = 2\n\turl = $R/src\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
         [branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n"
    );
}

#[test]
fn a_bare_clone_orders_the_same_way() {
    let f = Fixture::new("bare");
    assert_eq!(
        f.config("d.git", "config", &["--bare", "-c", "remote.origin.x=y", "-c", "a.b=c"]),
        "[core]\n\
         [remote \"origin\"]\n\tx = y\n\turl = $R/src\n\
         [a]\n\tb = c\n"
    );
}
